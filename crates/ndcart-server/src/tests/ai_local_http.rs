//! `/api/ai/local/*`: the panel's routes, end to end with a stand-in for the Hub and a fake
//! `llama-server`. The fake is this very test binary: a shell script runs it with only
//! `fake_llama_entry` selected, which serves the stand-in model on the port it was given.

#![cfg(unix)]

use super::*;
use crate::ai_local::{catalog, http as local, pull::Policy, serve};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Not a test when run normally. As the fake llama-server it serves until its owner is gone.
#[test]
fn fake_llama_entry() {
    let Ok(args) = std::env::var("NDCART_FAKE_LLAMA") else {
        return;
    };
    let owner = std::env::var("NDCART_FAKE_OWNER").unwrap_or_default();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(500));
        if !owner.is_empty() && !Path::new(&format!("/proc/{owner}")).exists() {
            std::process::exit(0);
        }
    });
    let mut a = vec!["--llama-compat".to_string()];
    a.extend(args.split_whitespace().map(str::to_string));
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(crate::ai_standin::run(a))
        .unwrap();
}

fn fake_runtime(dir: &Path) -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let p = dir.join("llama-server");
    std::fs::write(
        &p,
        format!(
            "#!/bin/sh\n\
             case \"$*\" in *--version*) echo 'version: 9 (fake)' >&2; exit 0;; esac\n\
             NDCART_FAKE_OWNER=$PPID NDCART_FAKE_LLAMA=\"$*\" \"{}\" --exact tests::ai_local_http::fake_llama_entry --nocapture >/dev/null 2>&1 &\n\
             C=$!\n\
             trap 'kill $C 2>/dev/null' TERM INT\n\
             wait $C\n",
            exe.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

mod hub {
    use axum::body::Body;
    use axum::extract::Request;
    use axum::http::Response;
    use sha2::{Digest, Sha256};
    use std::sync::Arc;

    pub async fn spawn(repo: &'static str, file: &'static str, bytes: Vec<u8>) -> String {
        let sha: String = Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let bytes = Arc::new(bytes);
        let app = axum::Router::new().fallback(move |req: Request| {
            let (bytes, sha) = (bytes.clone(), sha.clone());
            async move {
                let p = req.uri().path().to_string();
                if p == format!("/api/models/{repo}/tree/main") {
                    let v = serde_json::json!([{"type":"file","path":file,"size":bytes.len(),
                        "lfs":{"oid":sha,"size":bytes.len()}}]);
                    Response::new(Body::from(v.to_string()))
                } else if p == format!("/{repo}/resolve/main/{file}") {
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

fn app_in(dir: &Path, stored: ai_settings::StoredAiSettings) -> App {
    let app = test_app();
    // `test_app` has no data folder; local models need one.
    let inner = Arc::try_unwrap(app).ok().expect("fresh app");
    Arc::new(AppState {
        data_dir: Some(dir.to_path_buf()),
        ai: Mutex::new(stored),
        ..inner
    })
}

async fn get_json(r: &Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let (st, b) = call(r, "GET", uri, vec![]).await;
    (st, jv(&b))
}

async fn post_json(
    r: &Router,
    uri: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let (st, b) = call(r, "POST", uri, body.to_string().into_bytes()).await;
    (st, jv(&b))
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
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

struct StopOnDrop(PathBuf);
impl Drop for StopOnDrop {
    fn drop(&mut self) {
        let _ = serve::stop(&self.0, Duration::from_secs(5));
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_panel_downloads_uses_starts_tests_and_removes_a_model() {
    let dir = std::env::temp_dir().join(format!("ndcart-http-local-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = StopOnDrop(dir.clone());
    let cat = catalog::bundled();
    let entry = catalog::find(&cat, "qwen3.5-4b").unwrap().clone();
    let (repo, file): (&'static str, &'static str) = (
        Box::leak(entry.hf_repo.clone().into_boxed_str()),
        Box::leak(entry.file.clone().into_boxed_str()),
    );
    let mut bytes = b"GGUF".to_vec();
    bytes.extend((0..50_000u32).map(|i| (i % 249) as u8));
    let base = hub::spawn(repo, file, bytes.clone()).await;
    local::set_policy_for_test(Some(Policy {
        base,
        hosts: vec!["127.0.0.1".into()],
        allow_http: true,
    }));

    // Before anything: the runtime is the fake one (named in the settings), nothing installed.
    let port = free_port();
    let rt = fake_runtime(&dir);
    let stored = ai_settings::StoredAiSettings {
        managed: Some(ai_settings::Managed {
            model_file: "placeholder.gguf".into(),
            port: Some(port),
            runtime_path: Some(rt.to_string_lossy().into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let app = app_in(&dir, stored);
    let router = build_router(app.clone());
    let (st, v) = get_json(&router, "/api/ai/local").await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["runtime"]["found"], true);
    assert_eq!(v["runtime"]["version"], "9 (fake)");
    assert!(v["catalog"].as_array().unwrap().len() >= 6);
    assert!(v["catalog"]
        .as_array()
        .unwrap()
        .iter()
        .all(|m| m["installed"] == false && m["verified"] == false));
    assert_eq!(v["installed"], serde_json::json!([]));
    assert_eq!(v["pull"]["state"], "idle");
    assert!(v["hw"]["os"].is_string());

    // Unknown ids, and a download needs the size confirmed first.
    assert_eq!(
        post_json(
            &router,
            "/api/ai/local/pull",
            serde_json::json!({"id": "nope"})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let (st, v) = post_json(
        &router,
        "/api/ai/local/pull",
        serde_json::json!({"id": "qwen3.5-4b"}),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert_eq!(v["size_bytes"], bytes.len());
    let (st, _) = post_json(
        &router,
        "/api/ai/local/pull",
        serde_json::json!({"id": "qwen3.5-4b", "confirm_bytes": 5}),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "a wrong size is refused too");
    assert!(!dir.join("models").join(repo).join(file).exists());
    let (st, _) = post_json(
        &router,
        "/api/ai/local/use",
        serde_json::json!({"id": "qwen3.5-4b"}),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "not downloaded yet");

    // Confirmed: a background job with progress.
    let (st, v) = post_json(
        &router,
        "/api/ai/local/pull",
        serde_json::json!({"id": "qwen3.5-4b", "confirm_bytes": bytes.len()}),
    )
    .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{v}");
    until("the download", 20, || async {
        let (_, v) = get_json(&router, "/api/ai/local").await;
        match v["pull"]["state"].as_str() {
            Some("done") => Some(()),
            Some("error") => panic!("{v}"),
            _ => None,
        }
    })
    .await;
    assert_eq!(
        std::fs::read(dir.join("models").join(repo).join(file)).unwrap(),
        bytes
    );
    let (_, v) = get_json(&router, "/api/ai/local").await;
    assert_eq!(v["installed"][0]["catalog_id"], "qwen3.5-4b");
    assert_eq!(v["pull"]["done"], bytes.len());

    // Use it: the settings are written and live at once.
    let (st, v) = post_json(
        &router,
        "/api/ai/local/use",
        serde_json::json!({"id": "qwen3.5-4b"}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let saved = ai_settings::load(Some(&dir));
    let m = saved.managed.clone().unwrap();
    assert_eq!(m.model_id.as_deref(), Some("qwen3.5-4b"));
    assert_eq!(m.port, Some(port), "earlier tuning is kept");
    assert!(m.runtime_path.is_some());
    assert_eq!(
        app.ai.lock().unwrap().managed.as_ref().unwrap().model_file,
        m.model_file
    );

    // Start (it answers at once; the model "loads"), wait for ready, then the self-test.
    let (st, v) = post_json(&router, "/api/ai/local/start", serde_json::json!({})).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{v}");
    let (st, _) = post_json(&router, "/api/ai/local/start", serde_json::json!({})).await;
    assert_eq!(st, StatusCode::CONFLICT, "already running");
    until("the server to be ready", 30, || async {
        let (_, v) = get_json(&router, "/api/ai/local").await;
        (v["server"]["health"] == "ready").then_some(())
    })
    .await;
    let (_, v) = get_json(&router, "/api/ai/status").await;
    assert_eq!(v["mode"], "standalone");
    let (st, v) = post_json(&router, "/api/ai/local/test", serde_json::json!({})).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["verdict"], "good", "{v}");
    assert_eq!(v["recorded"], true);
    let (_, v) = get_json(&router, "/api/ai/local").await;
    let row = v["catalog"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "qwen3.5-4b")
        .unwrap()
        .clone();
    assert_eq!(row["tested"]["verdict"], "good");
    assert_eq!(row["installed"], true);

    // A running model cannot be deleted; stopped, it can.
    let (st, _) = call(&router, "DELETE", "/api/ai/local/models/qwen3.5-4b", vec![]).await;
    assert_eq!(st, StatusCode::CONFLICT);
    let (st, v) = post_json(&router, "/api/ai/local/stop", serde_json::json!({})).await;
    assert_eq!(
        (st, v["stopped"].clone()),
        (StatusCode::OK, serde_json::json!(true))
    );
    let (_, v) = get_json(&router, "/api/ai/local").await;
    assert_eq!(v["server"]["running"], false);
    let (st, v) = call(&router, "DELETE", "/api/ai/local/models/qwen3.5-4b", vec![]).await;
    assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&v));
    assert!(!dir.join("models").join(repo).join(file).exists());
    assert!(catalog::load_records(&dir).is_empty());
    let (st, _) = call(&router, "DELETE", "/api/ai/local/models/qwen3.5-4b", vec![]).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    // Stopping again is harmless.
    let (st, v) = post_json(&router, "/api/ai/local/stop", serde_json::json!({})).await;
    assert_eq!(
        (st, v["stopped"].clone()),
        (StatusCode::OK, serde_json::json!(false))
    );
    local::set_policy_for_test(None);
}

#[tokio::test]
async fn without_a_data_folder_or_a_runtime_the_routes_say_what_is_missing() {
    // No data folder: downloads and starts are refused with the reason.
    let router = build_router(test_app());
    let (st, v) = post_json(&router, "/api/ai/local/start", serde_json::json!({})).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert!(v["error"].as_str().unwrap().contains("data folder"), "{v}");
    let (st, v) = get_json(&router, "/api/ai/local").await;
    assert_eq!(
        (st, v["data_dir_ok"].clone()),
        (StatusCode::OK, serde_json::json!(false))
    );
    let (st, v) = post_json(&router, "/api/ai/local/test", serde_json::json!({})).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "{v}");

    // A data folder but nothing chosen, then a choice with no runtime anywhere.
    let dir = std::env::temp_dir().join(format!("ndcart-http-local2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("models")).unwrap();
    std::fs::write(dir.join("models/m.gguf"), b"GGUF").unwrap();
    let r = build_router(app_in(&dir, Default::default()));
    let (st, v) = post_json(&r, "/api/ai/local/start", serde_json::json!({})).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert!(v["error"].as_str().unwrap().contains("Use this model"));
    let stored = ai_settings::StoredAiSettings {
        managed: Some(ai_settings::Managed {
            model_file: "m.gguf".into(),
            runtime_path: Some(dir.join("nope").to_string_lossy().into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let r = build_router(app_in(&dir, stored));
    let (st, v) = post_json(&r, "/api/ai/local/start", serde_json::json!({})).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert!(
        v["error"]
            .as_str()
            .unwrap()
            .contains("llama-server was not found"),
        "{v}"
    );
    let (_, v) = get_json(&r, "/api/ai/local").await;
    assert_eq!(v["runtime"]["found"], false);
    assert!(v["runtime"]["hint"].as_str().unwrap().contains("llama.cpp"));
    let _ = std::fs::remove_dir_all(&dir);
}
