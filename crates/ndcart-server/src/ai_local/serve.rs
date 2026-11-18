//! `ndcart ai serve | stop | key`: run upstream llama.cpp's `llama-server` for the studio,
//! bound to loopback, with its API key in a 0600 file passed by path (never on the command
//! line), under the fixed alias `ndcart-local` so the settings survive a model swap.
//!
//! The child is started with a fixed argument list and no shell. Its pid and port are written
//! to `<data>/ai-runtime.json`; `stop` checks that the pid still belongs to the server before
//! signalling it. With `autostart`, `ndcart serve` supervises it ([`Supervisor`]).

use super::hw::run_capture;
use super::{catalog, iso_date, now_secs};
use crate::ai_settings::{Managed, StoredAiSettings, MANAGED_ALIAS};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const KEY_FILE: &str = "ai-runtime.key";
pub const REC_FILE: &str = "ai-runtime.json";
pub const LOG_FILE: &str = "ai-runtime.log";

/// A random 32-byte key as hex, in a 0600 file (created once, then kept).
pub fn ensure_key(data: &Path) -> Result<PathBuf, String> {
    let path = data.join(KEY_FILE);
    if std::fs::read_to_string(&path).is_ok_and(|t| !t.trim().is_empty()) {
        return Ok(path);
    }
    std::fs::create_dir_all(data).map_err(|e| format!("cannot create {}: {e}", data.display()))?;
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    crate::store::write_atomic_private(&path, hex.as_bytes())
        .map_err(|e| format!("cannot write the server key: {e}"))?;
    Ok(path)
}

/// Everything needed to start one server.
#[derive(Clone, Debug)]
pub struct Launch {
    pub runtime: PathBuf,
    pub model: PathBuf,
    pub model_file: String,
    pub port: u16,
    pub ctx: u32,
    pub gpu_layers: Option<String>,
    pub threads: Option<u32>,
    pub key_file: PathBuf,
    pub log: PathBuf,
}

/// Where a `model_file` setting points: relative paths live under `<data>/models`.
pub fn model_path(data: &Path, model_file: &str) -> Result<PathBuf, String> {
    let p = Path::new(model_file);
    if p.is_absolute() {
        return Ok(p.to_path_buf());
    }
    if model_file.is_empty()
        || p.components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(format!("unsafe model path '{model_file}'"));
    }
    Ok(super::pull::models_dir(data).join(p))
}

impl Launch {
    pub fn new(
        data: &Path,
        m: &Managed,
        runtime: PathBuf,
        key_file: PathBuf,
    ) -> Result<Launch, String> {
        let model = model_path(data, &m.model_file)?;
        if !model.is_file() {
            return Err(format!(
                "the model file {} is not there. Fetch it with: ndcart ai pull <id> (see ndcart ai models)",
                model.display()
            ));
        }
        if let Some(g) = &m.gpu_layers {
            if !(g == "auto"
                || g == "all"
                || (!g.is_empty() && g.bytes().all(|b| b.is_ascii_digit())))
            {
                return Err(format!(
                    "gpu_layers must be auto, all or a number (got '{g}')"
                ));
            }
        }
        Ok(Launch {
            runtime,
            model,
            model_file: m.model_file.clone(),
            port: m.port(),
            ctx: m.ctx(),
            gpu_layers: m.gpu_layers.clone(),
            threads: m.threads,
            key_file,
            log: data.join(LOG_FILE),
        })
    }
}

/// The fixed argument list. Loopback only, always an explicit port (llama-server's own default
/// changed between versions), the key by file path.
pub fn argv(l: &Launch) -> Vec<String> {
    let mut v: Vec<String> = vec![
        "-m".into(),
        l.model.to_string_lossy().into_owned(),
        "--host".into(),
        "127.0.0.1".into(),
        "--port".into(),
        l.port.to_string(),
        "--alias".into(),
        MANAGED_ALIAS.into(),
        "--api-key-file".into(),
        l.key_file.to_string_lossy().into_owned(),
        "-c".into(),
        l.ctx.to_string(),
        "-np".into(),
        "1".into(),
        "--no-webui".into(),
    ];
    if let Some(g) = &l.gpu_layers {
        v.extend(["-ngl".into(), g.clone()]);
    }
    if let Some(t) = l.threads {
        v.extend(["-t".into(), t.to_string()]);
    }
    v
}

pub fn port_free(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// Start the child (stdout and stderr into the log file).
pub fn spawn_child(l: &Launch, detached: bool) -> Result<Child, String> {
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&l.log)
        .map_err(|e| format!("cannot open the log {}: {e}", l.log.display()))?;
    let log2 = log.try_clone().map_err(|e| e.to_string())?;
    let mut cmd = Command::new(&l.runtime);
    cmd.args(argv(l))
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log2))
        // The server needs neither of these.
        .env_remove("HF_TOKEN")
        .env_remove("HF_TOKEN_FILE");
    if detached {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP
            cmd.creation_flags(0x0000_0008 | 0x0000_0200);
        }
    }
    cmd.spawn()
        .map_err(|e| format!("cannot start {}: {e}", l.runtime.display()))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Health {
    Ready,
    Loading,
    Down,
}

/// `GET /health` on the loopback port: 200 ready, 503 still loading the model.
pub async fn health(port: u16) -> Health {
    let c = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .no_proxy()
        .build()
        .expect("client");
    match c
        .get(format!("http://127.0.0.1:{port}/health"))
        .send()
        .await
    {
        Ok(r) if r.status().as_u16() == 200 => Health::Ready,
        Ok(r) if r.status().as_u16() == 503 => Health::Loading,
        _ => Health::Down,
    }
}

fn log_tail(path: &Path) -> String {
    let t = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = t.lines().rev().take(6).collect();
    lines.into_iter().rev().collect::<Vec<_>>().join("\n")
}

/// Wait for `/health` to say ready, failing at once if the child exits.
pub async fn wait_ready(
    port: u16,
    timeout: Duration,
    mut child: Option<&mut Child>,
    log: &Path,
) -> Result<(), String> {
    let start = Instant::now();
    loop {
        if let Some(c) = child.as_deref_mut() {
            if let Ok(Some(st)) = c.try_wait() {
                return Err(format!(
                    "llama-server exited ({st}) before it was ready. The end of its log:\n{}",
                    log_tail(log)
                ));
            }
        }
        if health(port).await == Health::Ready {
            return Ok(());
        }
        if start.elapsed() >= timeout {
            return Err(format!(
                "llama-server was not ready after {} s (a large model can take longer to load; check `ndcart ai status`). Log: {}",
                timeout.as_secs(),
                log.display()
            ));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

// ---- the pid file --------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RuntimeRec {
    pub pid: u32,
    pub port: u16,
    pub model_file: String,
    pub started_at: String,
    pub runtime: String,
}

pub fn read_rec(data: &Path) -> Option<RuntimeRec> {
    serde_json::from_str(&std::fs::read_to_string(data.join(REC_FILE)).ok()?).ok()
}

pub fn write_rec(data: &Path, rec: &RuntimeRec) -> Result<(), String> {
    let body = serde_json::to_string_pretty(rec).map_err(|e| e.to_string())?;
    crate::store::write_atomic_private(&data.join(REC_FILE), body.as_bytes())
        .map_err(|e| format!("cannot write the server record: {e}"))
}

pub fn remove_rec(data: &Path) {
    let _ = std::fs::remove_file(data.join(REC_FILE));
}

fn rec_for(l: &Launch, pid: u32) -> RuntimeRec {
    let s = now_secs();
    RuntimeRec {
        pid,
        port: l.port,
        model_file: l.model_file.clone(),
        started_at: format!(
            "{}T{:02}:{:02}:{:02}Z",
            iso_date(s),
            (s % 86_400) / 3600,
            (s % 3600) / 60,
            s % 60
        ),
        runtime: l.runtime.to_string_lossy().into_owned(),
    }
}

// ---- processes -----------------------------------------------------------------------------

/// The process's state letter on Linux (`Z` is a zombie that only needs reaping).
#[cfg(target_os = "linux")]
fn proc_state(pid: u32) -> Option<char> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit_once(')')?.1.trim().chars().next()
}

pub fn is_alive(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        proc_state(pid).is_some_and(|s| s != 'Z' && s != 'X')
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        run_capture(
            Path::new("ps"),
            &["-o", "stat=", "-p", &pid.to_string()],
            Duration::from_secs(3),
        )
        .is_some_and(|s| !s.trim().is_empty() && !s.trim().starts_with('Z'))
    }
    #[cfg(windows)]
    {
        run_capture(
            Path::new("tasklist"),
            &["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"],
            Duration::from_secs(5),
        )
        .is_some_and(|s| s.contains(&format!("\"{pid}\"")))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        false
    }
}

/// Does `pid` still look like the llama-server we started? Some(false): it is something else
/// now (a reused pid); None: cannot tell.
pub fn belongs(rec: &RuntimeRec) -> Option<bool> {
    #[cfg(unix)]
    {
        let cmdline = std::fs::read(format!("/proc/{}/cmdline", rec.pid))
            .ok()
            .map(|b| String::from_utf8_lossy(&b).replace('\0', " "))
            .or_else(|| {
                run_capture(
                    Path::new("ps"),
                    &["-p", &rec.pid.to_string(), "-o", "command="],
                    Duration::from_secs(3),
                )
            })?;
        let port = format!("--port {}", rec.port);
        Some(cmdline.contains(&format!("--alias {MANAGED_ALIAS}")) && cmdline.contains(&port))
    }
    #[cfg(windows)]
    {
        let out = run_capture(
            Path::new("tasklist"),
            &["/FI", &format!("PID eq {}", rec.pid), "/FO", "CSV", "/NH"],
            Duration::from_secs(5),
        )?;
        let want = Path::new(&rec.runtime)
            .file_name()?
            .to_string_lossy()
            .to_ascii_lowercase();
        Some(out.to_ascii_lowercase().contains(&want))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = rec;
        None
    }
}

fn signal(pid: u32, force: bool) {
    #[cfg(unix)]
    {
        let _ = Command::new("kill")
            .args([if force { "-KILL" } else { "-TERM" }, &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(windows)]
    {
        let mut a = vec!["/PID".to_string(), pid.to_string(), "/T".to_string()];
        if force {
            a.push("/F".into());
        }
        let _ = Command::new("taskkill")
            .args(a)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (pid, force);
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Stopped {
    NotRunning,
    StaleRecordRemoved,
    Ended(u32),
}

/// Stop the server named by the pid file. The record goes first, so a supervisor sees a
/// deliberate stop and does not restart it.
pub fn stop(data: &Path, patience: Duration) -> Result<Stopped, String> {
    let Some(rec) = read_rec(data) else {
        return Ok(Stopped::NotRunning);
    };
    if !is_alive(rec.pid) {
        remove_rec(data);
        return Ok(Stopped::StaleRecordRemoved);
    }
    match belongs(&rec) {
        Some(true) => {}
        Some(false) => {
            remove_rec(data);
            return Ok(Stopped::StaleRecordRemoved);
        }
        None => {
            return Err(format!(
                "cannot tell whether process {} is still llama-server, so it was not stopped",
                rec.pid
            ))
        }
    }
    remove_rec(data);
    signal(rec.pid, false);
    let start = Instant::now();
    while is_alive(rec.pid) {
        if start.elapsed() > patience {
            signal(rec.pid, true);
            std::thread::sleep(Duration::from_millis(300));
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(Stopped::Ended(rec.pid))
}

/// Start a detached server and wait for it. Refuses when one is already running or the port
/// is taken.
pub async fn start(data: &Path, l: &Launch, wait: Duration) -> Result<RuntimeRec, String> {
    if let Some(rec) = read_rec(data) {
        if is_alive(rec.pid) && belongs(&rec) != Some(false) {
            return Err(format!(
                "a model server is already running (pid {}, port {}); stop it first with: ndcart ai stop",
                rec.pid, rec.port
            ));
        }
        remove_rec(data);
    }
    if !port_free(l.port) {
        return Err(format!(
            "port {} is already in use. Pick another with --port N (or stop what is using it).",
            l.port
        ));
    }
    let mut child = spawn_child(l, true)?;
    let rec = rec_for(l, child.id());
    write_rec(data, &rec)?;
    if let Err(e) = wait_ready(l.port, wait, Some(&mut child), &l.log).await {
        let _ = child.kill();
        let _ = child.wait();
        remove_rec(data);
        return Err(e);
    }
    Ok(rec)
}

// ---- supervision ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Status {
    /// `off`, `starting`, `running`, `restarting`, `stopped` (on purpose), `gave_up` or `error`.
    pub state: &'static str,
    pub pid: Option<u32>,
    pub port: Option<u16>,
    pub restarts: u32,
    pub detail: String,
}

static STATUS: Mutex<Status> = Mutex::new(Status {
    state: "off",
    pid: None,
    port: None,
    restarts: 0,
    detail: String::new(),
});

pub fn status() -> Status {
    STATUS.lock().expect("status").clone()
}

fn set_status(
    state: &'static str,
    pid: Option<u32>,
    port: Option<u16>,
    restarts: u32,
    detail: impl Into<String>,
) {
    *STATUS.lock().expect("status") = Status {
        state,
        pid,
        port,
        restarts,
        detail: detail.into(),
    };
}

/// Restart delays after a crash; when they are used up the supervisor gives up.
#[derive(Clone, Debug)]
pub struct Backoff {
    pub delays: Vec<Duration>,
    /// A run this long counts as healthy and resets the count.
    pub healthy_after: Duration,
    pub ready_timeout: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Backoff {
            delays: vec![
                Duration::from_secs(1),
                Duration::from_secs(5),
                Duration::from_secs(30),
            ],
            healthy_after: Duration::from_secs(60),
            ready_timeout: Duration::from_secs(180),
        }
    }
}

/// Keeps one server running for as long as `ndcart serve` runs.
pub struct Supervisor {
    cancel: Arc<AtomicBool>,
    handle: tokio::task::JoinHandle<()>,
}

impl Supervisor {
    pub fn spawn(data: PathBuf, launch: Launch, backoff: Backoff) -> Supervisor {
        let cancel = Arc::new(AtomicBool::new(false));
        let handle = tokio::spawn(supervise(data, launch, backoff, cancel.clone()));
        Supervisor { cancel, handle }
    }

    /// Stop supervising and stop the server it started.
    pub async fn shutdown(self) {
        self.cancel.store(true, Ordering::SeqCst);
        let _ = self.handle.await;
    }
}

async fn supervise(data: PathBuf, l: Launch, b: Backoff, cancel: Arc<AtomicBool>) {
    let mut failures = 0usize;
    let mut restarts = 0u32;
    let pause = |ms: u64| tokio::time::sleep(Duration::from_millis(ms));
    loop {
        if cancel.load(Ordering::SeqCst) {
            break;
        }
        // A server someone else started (`ndcart ai serve`) is adopted, not duplicated.
        if let Some(rec) = read_rec(&data) {
            if rec.port == l.port && is_alive(rec.pid) && belongs(&rec) == Some(true) {
                set_status(
                    "running",
                    Some(rec.pid),
                    Some(l.port),
                    restarts,
                    "started by `ndcart ai serve`",
                );
                while !cancel.load(Ordering::SeqCst) && is_alive(rec.pid) {
                    pause(1000).await;
                }
                if cancel.load(Ordering::SeqCst) {
                    break;
                }
                if read_rec(&data).is_none() {
                    set_status(
                        "stopped",
                        None,
                        Some(l.port),
                        restarts,
                        "stopped on purpose",
                    );
                    return;
                }
                continue;
            }
        }
        set_status("starting", None, Some(l.port), restarts, "");
        let mut child = match spawn_child(&l, false) {
            Ok(c) => c,
            Err(e) => {
                set_status("error", None, Some(l.port), restarts, e);
                return;
            }
        };
        let pid = child.id();
        let _ = write_rec(&data, &rec_for(&l, pid));
        let began = Instant::now();
        let (exited, deliberate) =
            match wait_ready(l.port, b.ready_timeout, Some(&mut child), &l.log).await {
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    remove_rec(&data);
                    set_status("restarting", None, Some(l.port), restarts, e.clone());
                    (e, false)
                }
                Ok(()) => {
                    set_status("running", Some(pid), Some(l.port), restarts, "");
                    // Watch it.
                    let exited = loop {
                        if cancel.load(Ordering::SeqCst) {
                            let _ = child.kill();
                            let _ = child.wait();
                            remove_rec(&data);
                            set_status("off", None, Some(l.port), restarts, "");
                            return;
                        }
                        match child.try_wait() {
                            Ok(Some(st)) => break format!("exited ({st})"),
                            Ok(None) => {}
                            Err(e) => break format!("lost track of it: {e}"),
                        }
                        pause(500).await;
                    };
                    // `ndcart ai stop` removes the record first: that is a deliberate stop.
                    (exited, read_rec(&data).is_none())
                }
            };
        if deliberate {
            set_status(
                "stopped",
                None,
                Some(l.port),
                restarts,
                "stopped on purpose",
            );
            return;
        }
        if began.elapsed() >= b.healthy_after {
            failures = 0;
        }
        match b.delays.get(failures) {
            Some(d) => {
                failures += 1;
                restarts += 1;
                set_status(
                    "restarting",
                    None,
                    Some(l.port),
                    restarts,
                    format!("the model server {exited}; restarting"),
                );
                tokio::time::sleep(*d).await;
            }
            None => {
                remove_rec(&data);
                set_status(
                    "gave_up",
                    None,
                    Some(l.port),
                    restarts,
                    format!(
                        "the model server kept stopping ({exited}). See {} and restart the studio.",
                        l.log.display()
                    ),
                );
                return;
            }
        }
    }
    set_status("off", None, None, restarts, "");
}

/// At `ndcart serve` start: when the saved settings ask for autostart, make the key, find the
/// runtime and return a supervisor. Problems are reported in the status, never fatal.
pub fn autostart(data: Option<&Path>, stored: &mut StoredAiSettings) -> Option<Supervisor> {
    let data = data?;
    let m = stored.managed.clone().filter(|m| m.autostart)?;
    let fail = |msg: String| {
        eprintln!("ndcart: local AI not started: {msg}");
        set_status("error", None, Some(m.port()), 0, msg);
        None
    };
    let key = match ensure_key(data) {
        Ok(k) => k,
        Err(e) => return fail(e),
    };
    let Some(rt) = super::runtime::find(m.runtime_path.as_deref()) else {
        return fail(format!(
            "llama-server was not found. {}",
            super::runtime::install_hint_here()
        ));
    };
    let launch = match Launch::new(data, &m, rt.path, key.clone()) {
        Ok(l) => l,
        Err(e) => return fail(e),
    };
    // The studio reads the key from this file.
    let key_s = key.to_string_lossy().into_owned();
    if m.key_file.as_deref() != Some(key_s.as_str()) {
        if let Some(mm) = stored.managed.as_mut() {
            mm.key_file = Some(key_s);
        }
        let _ = crate::ai_settings::save(Some(data), stored);
    }
    println!("ndcart: starting the local model ({})", m.model_file);
    Some(Supervisor::spawn(
        data.to_path_buf(),
        launch,
        Backoff::default(),
    ))
}

// ---- the command line ----------------------------------------------------------------------

/// A catalog id or a model path, as the `model_file` setting.
pub fn resolve_model_arg(data: &Path, arg: &str, cat: &[catalog::Entry]) -> Result<String, String> {
    if let Some(e) = catalog::find(cat, arg) {
        let rel = format!("{}/{}", e.hf_repo, e.file);
        if !model_path(data, &rel)?.is_file() {
            return Err(format!(
                "{} is not downloaded yet: run ndcart ai pull {}",
                e.id, e.id
            ));
        }
        return Ok(rel);
    }
    let p = model_path(data, arg)?;
    if p.is_file() {
        Ok(arg.to_string())
    } else {
        Err(format!(
            "no such model: '{arg}' (a catalog id, a path under the models folder, or a full path)"
        ))
    }
}

/// The managed settings for `serve`: the saved block, with flag overrides.
fn managed_from_flags(
    data: &Path,
    stored: &StoredAiSettings,
    flags: &Flags,
) -> Result<Managed, String> {
    let mut m = match (&flags.model, stored.managed.clone()) {
        (Some(arg), saved) => {
            let mut base = saved.unwrap_or_default();
            base.model_file = resolve_model_arg(data, arg, &catalog::bundled())?;
            base.model_id = catalog::find(&catalog::bundled(), arg).map(|e| e.id.clone());
            base
        }
        (None, Some(saved)) => saved,
        (None, None) => {
            return Err("no model chosen. Run `ndcart ai setup`, or `ndcart ai pull <id>` and then `ndcart ai serve --model <id>`.".into())
        }
    };
    if let Some(p) = flags.port {
        m.port = Some(p);
    }
    if let Some(c) = flags.ctx {
        m.ctx = Some(c);
    }
    if let Some(g) = &flags.gpu_layers {
        m.gpu_layers = Some(g.clone());
    }
    if let Some(t) = flags.threads {
        m.threads = Some(t);
    }
    Ok(m)
}

#[derive(Default)]
struct Flags {
    model: Option<String>,
    port: Option<u16>,
    ctx: Option<u32>,
    gpu_layers: Option<String>,
    threads: Option<u32>,
    wait_secs: Option<u64>,
    foreground: bool,
}

fn parse_flags(rest: Vec<String>) -> Result<Flags, String> {
    let mut f = Flags::default();
    let mut it = rest.into_iter();
    fn num<T: std::str::FromStr>(v: Option<String>, name: &str) -> Result<T, String> {
        v.and_then(|v| v.parse().ok())
            .ok_or(format!("{name} needs a number"))
    }
    while let Some(a) = it.next() {
        match a.as_str() {
            "--foreground" => f.foreground = true,
            "--model" => f.model = Some(it.next().ok_or("--model needs a value")?),
            "--port" => f.port = Some(num(it.next(), "--port")?),
            "--ctx" => f.ctx = Some(num(it.next(), "--ctx")?),
            "--threads" => f.threads = Some(num(it.next(), "--threads")?),
            "--gpu-layers" => f.gpu_layers = Some(it.next().ok_or("--gpu-layers needs a value")?),
            "--wait-secs" => f.wait_secs = Some(num(it.next(), "--wait-secs")?),
            o => return Err(format!("unknown argument: {o}")),
        }
    }
    Ok(f)
}

/// `ndcart ai serve [--foreground] [--model ID|PATH] [--port N] [--ctx N] [--gpu-layers V] [--threads N] [--wait-secs N]`
pub async fn run_serve(args: Vec<String>) -> Result<(), String> {
    let c = super::common(args)?;
    let flags = parse_flags(c.rest)?;
    let stored = crate::ai_settings::load(Some(&c.data));
    let m = managed_from_flags(&c.data, &stored, &flags)?;
    let rt = super::runtime::find(m.runtime_path.as_deref()).ok_or_else(|| {
        format!(
            "llama-server was not found (ND_CARTO_LLAMA_SERVER, PATH and the usual places were searched).\n{}",
            super::runtime::install_hint_here()
        )
    })?;
    let key = ensure_key(&c.data)?;
    let launch = Launch::new(&c.data, &m, rt.path.clone(), key.clone())?;
    // The studio reads the key from this file; remember where it is.
    if stored.managed.is_some() {
        let mut st = stored.clone();
        if let Some(mm) = st.managed.as_mut() {
            let k = key.to_string_lossy().into_owned();
            if mm.key_file.as_deref() != Some(k.as_str()) {
                mm.key_file = Some(k);
                crate::ai_settings::save(Some(&c.data), &st)?;
            }
        }
    }
    let wait = Duration::from_secs(flags.wait_secs.unwrap_or(180));
    let url = format!("http://127.0.0.1:{}/v1", launch.port);
    let say = |rec: &RuntimeRec| {
        if c.json {
            println!(
                "{}",
                serde_json::json!({"url": url, "model": MANAGED_ALIAS, "pid": rec.pid, "port": rec.port, "log": launch.log})
            );
        } else {
            println!("{url}");
            eprintln!(
                "llama-server is ready (pid {}). Model id: {MANAGED_ALIAS}. The key is in {} and the log in {}.\nStop it with: ndcart ai stop",
                rec.pid,
                key.display(),
                launch.log.display()
            );
        }
    };
    if !flags.foreground {
        let rec = start(&c.data, &launch, wait).await?;
        say(&rec);
        return Ok(());
    }
    if !port_free(launch.port) {
        return Err(format!("port {} is already in use", launch.port));
    }
    let mut child = spawn_child(&launch, false)?;
    let rec = rec_for(&launch, child.id());
    write_rec(&c.data, &rec)?;
    if let Err(e) = wait_ready(launch.port, wait, Some(&mut child), &launch.log).await {
        let _ = child.kill();
        let _ = child.wait();
        remove_rec(&c.data);
        return Err(e);
    }
    say(&rec);
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = tokio::time::sleep(Duration::from_millis(500)) => {
                if let Ok(Some(st)) = child.try_wait() {
                    remove_rec(&c.data);
                    return Err(format!("llama-server exited ({st})"));
                }
            }
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    remove_rec(&c.data);
    Ok(())
}

/// `ndcart ai stop`
pub fn run_stop(args: Vec<String>) -> Result<(), String> {
    let c = super::common(args)?;
    if let Some(o) = c.rest.first() {
        return Err(format!("unknown argument: {o}"));
    }
    match stop(&c.data, Duration::from_secs(10))? {
        Stopped::NotRunning => println!("no model server is running"),
        Stopped::StaleRecordRemoved => {
            println!("the model server was not running (stale record removed)")
        }
        Stopped::Ended(pid) => println!("stopped the model server (pid {pid})"),
    }
    Ok(())
}

/// `ndcart ai key`: make the server key file if missing and print where it is (never its value).
pub fn run_key(args: Vec<String>) -> Result<(), String> {
    let c = super::common(args)?;
    let p = ensure_key(&c.data)?;
    println!("{}", p.display());
    Ok(())
}

#[cfg(test)]
mod tests;
