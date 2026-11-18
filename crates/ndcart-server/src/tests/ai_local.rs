//! The local-AI self-test and status report, against the stand-in model server.

use super::*;
use crate::ai_local::{selftest, status};
use crate::ai_standin::{self, Behaviour, Options};
use serde_json::{json, Value};

fn cfg(base: &str, key: Option<&str>) -> draft::Config {
    draft::Config {
        server: None,
        world: None,
        email: None,
        model: None,
        direct: Some((base.to_string(), "ndcart-local".into())),
        key: key.map(str::to_string),
        thinking: false,
        cache: Default::default(),
        stepwise: false,
    }
}

async fn verdict_for(behaviour: Behaviour) -> selftest::Report {
    let h = ai_standin::spawn(Options {
        behaviour,
        ..Options::default()
    })
    .await;
    selftest::run(&cfg(&h.base, None), &selftest::Opts { tools: false })
        .await
        .unwrap()
}

#[tokio::test]
async fn a_good_model_is_good_a_model_that_needs_repairs_is_usable_garbage_is_too_weak() {
    let r = verdict_for(Behaviour::Good).await;
    assert_eq!(r.verdict, "good", "{}", r.render());
    assert_eq!((r.valid, r.first_try, r.total), (3, 3, 3));
    assert_eq!(r.exit_code(), 0);
    let names: Vec<&str> = r.steps.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "health",
            "model list",
            "ping",
            "JSON schema",
            "golden 1",
            "golden 2",
            "golden 3"
        ]
    );
    assert!(r.steps.iter().all(|s| s.status == "pass"), "{}", r.render());
    // The speed comes from llama.cpp's timings when the server sends them.
    assert_eq!(r.tokens_per_second, Some(42.0));
    assert!(r.render().contains("Verdict: GOOD"));

    let r = verdict_for(Behaviour::FixOnce).await;
    assert_eq!(r.verdict, "usable", "{}", r.render());
    assert_eq!((r.valid, r.first_try), (3, 0));
    assert_eq!(r.exit_code(), 0);
    assert!(r
        .steps
        .iter()
        .filter(|s| s.name.starts_with("golden"))
        .all(|s| s.attempts == Some(2)));

    let r = verdict_for(Behaviour::Garbage).await;
    assert_eq!(r.verdict, "too weak", "{}", r.render());
    assert_eq!(r.valid, 0);
    assert_eq!(r.exit_code(), 3);
    assert!(r.render().contains("TOO WEAK"));
    // The probes still say why.
    assert_eq!(
        r.steps
            .iter()
            .find(|s| s.name == "JSON schema")
            .unwrap()
            .status,
        "fail"
    );
}

#[tokio::test]
async fn a_server_without_schema_support_is_still_testable() {
    let r = verdict_for(Behaviour::NoSchema).await;
    assert_eq!(r.verdict, "good", "{}", r.render());
}

#[tokio::test]
async fn an_unreachable_or_locked_or_wrong_endpoint_is_an_error_not_a_verdict() {
    // Nothing listens there.
    let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = dead.local_addr().unwrap().port();
    drop(dead);
    let r = selftest::run(
        &cfg(&format!("http://127.0.0.1:{port}/v1"), None),
        &selftest::Opts { tools: false },
    )
    .await
    .unwrap();
    assert_eq!((r.verdict, r.exit_code()), ("error", 1));
    assert_eq!(r.steps[0].status, "fail");
    assert!(r.steps.iter().skip(2).all(|s| s.status == "skip"));

    // A key is required and missing, then right.
    let h = ai_standin::spawn(Options {
        api_key: Some("secret".into()),
        ..Options::default()
    })
    .await;
    let r = selftest::run(&cfg(&h.base, None), &selftest::Opts { tools: false })
        .await
        .unwrap();
    assert_eq!(r.verdict, "error");
    assert!(
        r.steps
            .iter()
            .any(|s| s.detail.contains("refused the API key")),
        "{}",
        r.render()
    );
    let r = selftest::run(
        &cfg(&h.base, Some("secret")),
        &selftest::Opts { tools: false },
    )
    .await
    .unwrap();
    assert_eq!(r.verdict, "good", "{}", r.render());

    // The model id is not what the server serves.
    let h = ai_standin::spawn(Options {
        models: vec!["other".into()],
        ..Options::default()
    })
    .await;
    let r = selftest::run(&cfg(&h.base, None), &selftest::Opts { tools: false })
        .await
        .unwrap();
    assert_eq!(r.verdict, "error");
    assert!(r.steps[1].detail.contains("not served"));
    // nd-world is not a direct endpoint.
    let mut c = cfg(&h.base, None);
    c.direct = None;
    assert!(selftest::run(&c, &selftest::Opts { tools: false })
        .await
        .is_err());
}

#[tokio::test]
async fn the_tool_call_probe_is_optional_and_never_changes_the_verdict() {
    let h = ai_standin::spawn(Options::default()).await;
    let r = selftest::run(&cfg(&h.base, None), &selftest::Opts { tools: true })
        .await
        .unwrap();
    let t = r.steps.iter().find(|s| s.name == "tool call").unwrap();
    assert_eq!(t.status, "pass", "{}", t.detail);
    assert_eq!(r.verdict, "good");
    let h = ai_standin::spawn(Options {
        behaviour: Behaviour::Garbage,
        ..Options::default()
    })
    .await;
    let r = selftest::run(&cfg(&h.base, None), &selftest::Opts { tools: true })
        .await
        .unwrap();
    assert_eq!(
        r.steps
            .iter()
            .find(|s| s.name == "tool call")
            .unwrap()
            .status,
        "fail"
    );
    assert_eq!(r.verdict, "too weak");
}

#[tokio::test]
async fn results_are_recorded_only_for_catalog_models_and_shown_as_tested_here() {
    let dir = std::env::temp_dir().join(format!("ndcart-selftest-rec-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let r = verdict_for(Behaviour::FixOnce).await;
    assert!(selftest::record(&dir, "qwen3.5-4b", &r).unwrap());
    assert!(!selftest::record(&dir, "not-a-model", &r).unwrap());
    let rec = crate::ai_local::catalog::load_records(&dir);
    assert_eq!(rec.len(), 1);
    assert_eq!(rec["qwen3.5-4b"].verdict, "usable");
    // An error is never recorded as a verdict.
    let mut e = r.clone();
    e.verdict = "error";
    assert!(!selftest::record(&dir, "qwen3.5-2b", &e).unwrap());
    // A model that tested too weak is not recommended any more.
    let mut weak = r.clone();
    weak.verdict = "too weak";
    selftest::record(&dir, "qwen3.5-4b", &weak).unwrap();
    let cat = crate::ai_local::catalog::bundled();
    let hw = crate::ai_local::hw::Hw {
        os: "linux".into(),
        arch: "x86_64".into(),
        ram_bytes: 8_000_000_000,
        ..Default::default()
    };
    let rec = crate::ai_local::catalog::load_records(&dir);
    let offered = crate::ai_local::setup::offer(&hw, &cat, &rec);
    assert!(offered.iter().all(|m| m.id != "qwen3.5-4b"));
    assert!(!offered.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// The JSON's shape: every value replaced by its type, arrays by their first element.
fn shape(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(m.iter().map(|(k, v)| (k.clone(), shape(v))).collect()),
        Value::Array(a) => json!([a.first().map(shape).unwrap_or(json!("empty"))]),
        Value::String(_) => json!("string"),
        Value::Number(_) => json!("number"),
        Value::Bool(_) => json!("bool"),
        Value::Null => json!("null"),
    }
}

#[tokio::test]
async fn status_json_has_the_documented_shape() {
    let dir = std::env::temp_dir().join(format!("ndcart-status-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let cat = crate::ai_local::catalog::bundled();
    let e = &cat[0];
    let f = dir.join("models").join(&e.hf_repo).join(&e.file);
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    std::fs::write(&f, b"GGUF").unwrap();
    std::fs::write(
        crate::ai_local::pull::sidecar_path(&f),
        json!({"repo": e.hf_repo, "file": e.file, "sha256": "ab", "size": 4, "fetched_at": "x", "license": "", "sha256_source": "hub"}).to_string(),
    )
    .unwrap();
    crate::ai_local::catalog::record(
        &dir,
        &e.id,
        crate::ai_local::catalog::Record {
            verdict: "good".into(),
            at: "2026-11-18".into(),
            tokens_per_second: Some(30.0),
        },
    )
    .unwrap();
    let h = ai_standin::spawn(Options {
        api_key: Some("k".into()),
        ..Options::default()
    })
    .await;
    let port: u16 = h.origin.rsplit(':').next().unwrap().parse().unwrap();
    let key_file = dir.join("ai-runtime.key");
    std::fs::write(&key_file, "k").unwrap();
    let stored = ai_settings::StoredAiSettings {
        managed: Some(ai_settings::Managed {
            model_file: format!("{}/{}", e.hf_repo, e.file),
            model_id: Some(e.id.clone()),
            port: Some(port),
            key_file: Some(key_file.to_string_lossy().into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let rt = Some(crate::ai_local::runtime::Runtime {
        path: "/usr/bin/llama-server".into(),
        version: Some("1 (abc)".into()),
        source: "path",
    });
    let hw = crate::ai_local::hw::Hw {
        os: "linux".into(),
        arch: "x86_64".into(),
        ram_bytes: 8_000_000_000,
        gpus: vec![crate::ai_local::hw::Gpu {
            name: "GPU".into(),
            vram_bytes: 1,
            backend: "cuda".into(),
        }],
        apple_silicon: false,
        runtime_devices: vec!["CUDA0: GPU".into()],
    };
    let report = status::collect(&dir, &stored, rt, hw).await;
    let v = serde_json::to_value(&report).unwrap();
    let fixture = json!({
        "data_dir": "string",
        "hardware": {"os": "string", "arch": "string", "ram_bytes": "number",
            "gpus": [{"name": "string", "vram_bytes": "number", "backend": "string"}],
            "apple_silicon": "bool", "runtime_devices": ["string"]},
        "runtime": {"found": "bool", "path": "string", "version": "string", "source": "string", "hint": "null"},
        "models": [{"file": "string", "size_bytes": "number", "sha256": "string", "catalog_id": "string",
            "tested": {"verdict": "string", "at": "string", "tokens_per_second": "number"}}],
        "managed": {"model_file": "string", "model_id": "string", "port": "number", "ctx": "number",
            "autostart": "bool", "running": "bool", "pid": "null", "health": "string"},
        "endpoint": {"configured": "bool", "url": "string", "model": "string", "source": "string", "key_set": "bool"},
        "caps": {"kind": "string", "json_schema": "bool", "ctx": "number", "models": ["string"]},
    });
    // llama.cpp is known to enforce schemas (`json_schema` is null for a server not known yet).
    assert_eq!(shape(&v), fixture, "{v:#}");
    assert_eq!(v["endpoint"]["source"], "managed");
    assert_eq!(v["endpoint"]["key_set"], true);
    assert_eq!(v["managed"]["health"], "ready");
    assert_eq!(v["caps"]["kind"], "llamacpp");
    assert_eq!(v["models"][0]["tested"]["verdict"], "good");
    let text = report.render();
    assert!(
        text.contains("Runtime:") && text.contains("tested here: good"),
        "{text}"
    );
    // No setup at all: a report that says so, and a hint to install.
    let empty = status::collect(
        &dir.join("nowhere"),
        &Default::default(),
        None,
        Default::default(),
    )
    .await;
    assert!(!empty.runtime.found && empty.runtime.hint.is_some());
    assert!(empty.managed.is_none() && empty.caps.is_none() && !empty.endpoint.configured);
    assert!(empty.render().contains("llama-server not found"));
    let _ = std::fs::remove_dir_all(&dir);
}
