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
