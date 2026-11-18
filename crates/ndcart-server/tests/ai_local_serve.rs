#![cfg(all(unix, feature = "bridge"))]
//! The managed local server, end to end with no model and no llama-server: a script named
//! `llama-server` execs `ndcart ai stand-in --llama-compat`, which honours `--port` and
//! `--api-key-file` and answers 401 without the key. `ND_CARTO_LLAMA_SERVER` points at it.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_ndcart");

struct Env {
    data: PathBuf,
    script: PathBuf,
    port: u16,
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn env(tag: &str, loading: u32, autostart: bool) -> Env {
    let data = std::env::temp_dir().join(format!("ndcart-aiserve-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data);
    std::fs::create_dir_all(data.join("models")).unwrap();
    std::fs::write(data.join("models/m.gguf"), b"GGUF-not-a-model").unwrap();
    let script = data.join("llama-server");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nexec \"{BIN}\" ai stand-in --llama-compat --loading {loading} \"$@\"\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let port = free_port();
    std::fs::write(
        data.join("ai-settings.json"),
        format!(r#"{{"managed":{{"model_file":"m.gguf","port":{port},"autostart":{autostart}}}}}"#),
    )
    .unwrap();
    Env { data, script, port }
}

fn ndcart(e: &Env, args: &[&str]) -> Command {
    let mut c = Command::new(BIN);
    c.args(args)
        .env("ND_CARTO_LLAMA_SERVER", &e.script)
        .env_remove("ND_CARTO_LLM_URL")
        .env_remove("ND_CARTO_LLM_MODEL")
        .env_remove("ND_CARTO_LLM_KEY")
        .env_remove("ND_CARTO_LLM_KEY_FILE")
        .env_remove("ND_CARTO_SERVER")
        .stdin(Stdio::null());
    c
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
}

fn pid_of(data: &Path) -> Option<u32> {
    let rec: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(data.join("ai-runtime.json")).ok()?).ok()?;
    rec["pid"].as_u64().map(|p| p as u32)
}

fn alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|s| {
            s.rsplit_once(')')
                .and_then(|(_, r)| r.trim().chars().next())
        })
        .is_some_and(|c| c != 'Z')
}

fn kill(sig: &str, pid: u32) {
    let _ = Command::new("kill").args([sig, &pid.to_string()]).status();
}

struct Guard(Vec<Child>, PathBuf);
impl Drop for Guard {
    fn drop(&mut self) {
        for c in &mut self.0 {
            let _ = c.kill();
            let _ = c.wait();
        }
        if let Some(p) = pid_of(&self.1) {
            kill("-KILL", p);
        }
        let _ = std::fs::remove_dir_all(&self.1);
    }
}

async fn until<T, F: FnMut() -> Fut, Fut: std::future::Future<Output = Option<T>>>(
    what: &str,
    secs: u64,
    mut f: F,
) -> T {
    let t = Instant::now();
    loop {
        if let Some(v) = f().await {
            return v;
        }
        assert!(
            t.elapsed() < Duration::from_secs(secs),
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn serve_waits_for_health_guards_the_key_drafts_through_the_studio_and_stop_ends_it() {
    let e = env("lifecycle", 3, false);
    let _g = Guard(vec![], e.data.clone());
    let d = e.data.to_str().unwrap();

    // `ai serve` returns once /health stops saying "loading" (the stand-in says 503 three times).
    let out = ndcart(&e, &["ai", "serve", "--data-dir", d, "--wait-secs", "30"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let url = String::from_utf8(out.stdout).unwrap().trim().to_string();
    assert_eq!(url, format!("http://127.0.0.1:{}/v1", e.port));
    let pid = pid_of(&e.data).expect("pid file");
    assert!(alive(pid));

    // The key is a private file; without it the server answers 401, with it 200.
    let key_file = e.data.join("ai-runtime.key");
    assert_eq!(
        std::fs::metadata(&key_file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let key = std::fs::read_to_string(&key_file)
        .unwrap()
        .trim()
        .to_string();
    assert_eq!(
        http()
            .get(format!("{url}/models"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        http()
            .get(format!("http://127.0.0.1:{}/health", e.port))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let models: serde_json::Value = http()
        .get(format!("{url}/models"))
        .bearer_auth(&key)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(models["data"][0]["id"], "ndcart-local");
    // The key never appears on the command line of the process.
    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap();
    assert!(!String::from_utf8_lossy(&cmdline).contains(&key));

    // A second serve is refused while one runs.
    let again = ndcart(&e, &["ai", "serve", "--data-dir", d])
        .output()
        .unwrap();
    assert!(!again.status.success());
    assert!(String::from_utf8_lossy(&again.stderr).contains("already running"));

    // The studio, started with the saved settings, uses the managed model and its key file.
    let studio_port = free_port();
    let studio = ndcart(
        &e,
        &[
            "serve",
            "--listen",
            &format!("127.0.0.1:{studio_port}"),
            "--data-dir",
            d,
        ],
    )
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()
    .unwrap();
    let mut g2 = Guard(vec![studio], PathBuf::from("/nonexistent"));
    let base = format!("http://127.0.0.1:{studio_port}");
    until("the studio", 20, || async {
        http()
            .get(format!("{base}/health"))
            .send()
            .await
            .ok()
            .map(|_| ())
    })
    .await;
    let st: serde_json::Value = http()
        .get(format!("{base}/api/ai/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(st["mode"], "standalone", "{st}");
    let r: serde_json::Value = http()
        .post(format!("{base}/api/draft"))
        .json(&serde_json::json!({"prompt": "a crypt with an ossuary", "strict_ai": true}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["source"], "ai", "{r}");
    assert!(r["spec"]["floors"].is_array());
    g2.0.iter_mut().for_each(|c| {
        let _ = c.kill();
        let _ = c.wait();
    });

    let out = ndcart(&e, &["ai", "stop", "--data-dir", d])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    until("the server to end", 15, || async {
        (!alive(pid)).then_some(())
    })
    .await;
    assert!(pid_of(&e.data).is_none());
    // Stopping again is harmless.
    assert!(ndcart(&e, &["ai", "stop", "--data-dir", d])
        .output()
        .unwrap()
        .status
        .success());
}

#[tokio::test]
async fn a_supervised_server_comes_back_after_a_kill_and_goes_with_the_studio() {
    let e = env("supervise", 0, true);
    let d = e.data.to_str().unwrap().to_string();
    let studio_port = free_port();
    let studio = ndcart(
        &e,
        &[
            "serve",
            "--listen",
            &format!("127.0.0.1:{studio_port}"),
            "--data-dir",
            &d,
        ],
    )
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()
    .unwrap();
    let studio_pid = studio.id();
    let _g = Guard(vec![studio], e.data.clone());
    let base = format!("http://127.0.0.1:{studio_port}");
    let status = || async {
        http()
            .get(format!("{base}/api/ai/status"))
            .send()
            .await
            .ok()?
            .json::<serde_json::Value>()
            .await
            .ok()
    };
    let first = until("the model server to run", 30, || async {
        match status().await {
            Some(v) if v["managed"]["state"] == "running" => {
                Some(v["managed"]["pid"].as_u64().unwrap() as u32)
            }
            _ => None,
        }
    })
    .await;
    assert!(alive(first));
    let key = std::fs::read_to_string(e.data.join("ai-runtime.key")).unwrap();
    let r: serde_json::Value = http()
        .get(format!("http://127.0.0.1:{}/v1/models", e.port))
        .bearer_auth(key.trim())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["data"][0]["id"], "ndcart-local");

    // Kill it: the supervisor starts another.
    kill("-KILL", first);
    let second = until("a restart", 30, || async {
        match status().await {
            Some(v)
                if v["managed"]["state"] == "running"
                    && v["managed"]["restarts"].as_u64() >= Some(1) =>
            {
                let p = v["managed"]["pid"].as_u64().unwrap() as u32;
                (p != first).then_some(p)
            }
            _ => None,
        }
    })
    .await;
    assert!(alive(second));

    // Interrupting the studio takes the model server with it.
    kill("-INT", studio_pid);
    until("the model server to go", 20, || async {
        (!alive(second)).then_some(())
    })
    .await;
}

// ---- `ai test`, `ai status` and `ai setup` as real processes --------------------------------

fn stand_in(behaviour: &str) -> (Child, String) {
    use std::io::{BufRead, BufReader};
    let mut child = Command::new(BIN)
        .args(["ai", "stand-in", "--port", "0", "--behaviour", behaviour])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    (child, line.trim().to_string())
}

#[tokio::test]
async fn ai_test_exits_0_for_good_and_usable_and_3_for_too_weak_and_prints_json() {
    let data = std::env::temp_dir().join(format!("ndcart-aitest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data);
    let e = Env {
        data: data.clone(),
        script: PathBuf::new(),
        port: 0,
    };
    for (behaviour, code, verdict) in [
        ("good", 0, "good"),
        ("fix-once", 0, "usable"),
        ("garbage", 3, "too weak"),
    ] {
        let (child, url) = stand_in(behaviour);
        let _g = Guard(vec![child], PathBuf::from("/nonexistent"));
        let out = ndcart(
            &e,
            &[
                "ai",
                "test",
                "--url",
                &url,
                "--model",
                "ndcart-local",
                "--json",
                "--data-dir",
                data.to_str().unwrap(),
            ],
        )
        .output()
        .unwrap();
        assert_eq!(
            out.status.code(),
            Some(code),
            "{behaviour}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(v["verdict"], verdict);
        assert_eq!(v["total"], 3);
    }
    // The human form says it too.
    let (child, url) = stand_in("good");
    let _g = Guard(vec![child], PathBuf::from("/nonexistent"));
    let out = ndcart(
        &e,
        &[
            "ai",
            "test",
            "--url",
            &url,
            "--model",
            "ndcart-local",
            "--data-dir",
            data.to_str().unwrap(),
        ],
    )
    .output()
    .unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("Verdict: GOOD"));
    let _ = std::fs::remove_dir_all(&data);
}

mod hub {
    use axum::body::Body;
    use axum::extract::Request;
    use axum::http::Response;
    use sha2::{Digest, Sha256};
    use std::sync::Arc;

    pub const REPO: &str = "unsloth/Qwen3.5-4B-GGUF";
    pub const FILE: &str = "Qwen3.5-4B-UD-Q4_K_XL.gguf";

    pub async fn spawn(bytes: Vec<u8>) -> String {
        let sha: String = Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let bytes = Arc::new(bytes);
        let app = axum::Router::new().fallback(move |req: Request| {
            let (bytes, sha) = (bytes.clone(), sha.clone());
            async move {
                let p = req.uri().path().to_string();
                if p == format!("/api/models/{REPO}/tree/main") {
                    let v = serde_json::json!([{"type":"file","path":FILE,"size":bytes.len(),
                        "lfs":{"oid":sha,"size":bytes.len()}}]);
                    Response::new(Body::from(v.to_string()))
                } else if p == format!("/{REPO}/resolve/main/{FILE}") {
                    Response::new(Body::from(bytes.to_vec()))
                } else {
                    Response::builder().status(404).body(Body::empty()).unwrap()
                }
            }
        });
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            let _ = axum::serve(l, app).await;
        });
        format!("http://127.0.0.1:{port}")
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn setup_downloads_serves_tests_and_remembers_it_then_status_and_stop_agree() {
    let e = env("setup", 1, false);
    // Start from nothing: no settings, no model.
    std::fs::remove_file(e.data.join("ai-settings.json")).unwrap();
    std::fs::remove_file(e.data.join("models/m.gguf")).unwrap();
    let mut model = b"GGUF".to_vec();
    model.extend((0..20_000u32).map(|i| (i % 253) as u8));
    let hub_base = hub::spawn(model.clone()).await;
    let d = e.data.to_str().unwrap();
    let port = e.port.to_string();
    let _g = Guard(vec![], e.data.clone());

    let out = ndcart(
        &e,
        &[
            "ai",
            "setup",
            "--model",
            "qwen3.5-4b",
            "--yes",
            "--port",
            &port,
            "--data-dir",
            d,
        ],
    )
    .env("ND_CARTO_HF_BASE", &hub_base)
    .output()
    .unwrap();
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "{err}");
    assert!(err.contains("Verdict: GOOD"), "{err}");
    // The file is in place with its checksum record, the settings point at it, the result is remembered.
    let file = e.data.join("models").join(hub::REPO).join(hub::FILE);
    assert_eq!(std::fs::read(&file).unwrap(), model);
    assert!(file.with_extension("gguf.json").exists());
    let st: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(e.data.join("ai-settings.json")).unwrap())
            .unwrap();
    assert_eq!(st["managed"]["model_id"], "qwen3.5-4b");
    assert_eq!(st["managed"]["port"].as_u64(), Some(e.port as u64));
    assert!(st["managed"]["key_file"]
        .as_str()
        .unwrap()
        .ends_with("ai-runtime.key"));
    assert!(st.get("key").is_none() && st.get("direct_url").is_none());
    let rec: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(e.data.join("ai-verified.json")).unwrap())
            .unwrap();
    assert_eq!(rec["qwen3.5-4b"]["verdict"], "good");
    let pid = pid_of(&e.data).expect("the server is running");

    // `ai models` now says it was tested here; `ai status --json` sees the running server.
    let out = ndcart(&e, &["ai", "models", "--json", "--data-dir", d])
        .output()
        .unwrap();
    let models: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let row = models
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "qwen3.5-4b")
        .unwrap();
    assert_eq!(row["verified"], true);
    assert_eq!(row["tested"]["verdict"], "good");
    let out = ndcart(&e, &["ai", "status", "--json", "--data-dir", d])
        .output()
        .unwrap();
    let s: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(s["runtime"]["found"], true);
    assert_eq!(s["managed"]["running"], true);
    assert_eq!(s["managed"]["pid"].as_u64(), Some(pid as u64));
    assert_eq!(s["managed"]["health"], "ready");
    assert_eq!(s["endpoint"]["source"], "managed");
    assert_eq!(s["models"][0]["catalog_id"], "qwen3.5-4b");

    assert!(ndcart(&e, &["ai", "stop", "--data-dir", d])
        .output()
        .unwrap()
        .status
        .success());
    until("the server to end", 15, || async {
        (!alive(pid)).then_some(())
    })
    .await;
}

#[tokio::test]
async fn setup_without_a_runtime_prints_how_to_get_one_and_stops() {
    let e = env("noruntime", 0, false);
    let out = Command::new(BIN)
        .args([
            "ai",
            "setup",
            "--model",
            "qwen3.5-4b",
            "--yes",
            "--data-dir",
            e.data.to_str().unwrap(),
        ])
        .env("ND_CARTO_LLAMA_SERVER", e.data.join("missing"))
        .env("PATH", "/nonexistent")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("llama-server") && err.contains("install"),
        "{err}"
    );
    assert!(
        !e.data.join("models/unsloth").exists(),
        "nothing is downloaded without a runtime"
    );
    let _ = std::fs::remove_dir_all(&e.data);
}
