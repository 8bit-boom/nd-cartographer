use super::*;

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ndcart-serve-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn launch(dir: &Path) -> Launch {
    let model = dir.join("models").join("m.gguf");
    std::fs::create_dir_all(model.parent().unwrap()).unwrap();
    std::fs::write(&model, b"GGUF").unwrap();
    let m = Managed {
        model_file: "m.gguf".into(),
        port: Some(9123),
        ..Default::default()
    };
    Launch::new(
        dir,
        &m,
        PathBuf::from("/x/llama-server"),
        dir.join(KEY_FILE),
    )
    .unwrap()
}

#[test]
fn the_command_line_is_fixed_loopback_and_never_carries_the_key() {
    let d = tmp("argv");
    let mut l = launch(&d);
    let a = argv(&l);
    let at = |f: &str| a.iter().position(|x| x == f).map(|i| a[i + 1].clone());
    assert_eq!(at("--host").as_deref(), Some("127.0.0.1"));
    assert_eq!(at("--port").as_deref(), Some("9123"));
    assert_eq!(at("--alias").as_deref(), Some("ndcart-local"));
    assert_eq!(
        at("--api-key-file"),
        Some(d.join(KEY_FILE).to_string_lossy().into_owned())
    );
    assert_eq!(at("-c").as_deref(), Some("16384"));
    assert_eq!(at("-np").as_deref(), Some("1"));
    assert!(a.contains(&"--no-webui".to_string()));
    assert!(
        !a.contains(&"--api-key".to_string()),
        "the key must not be an argument"
    );
    assert!(!a.iter().any(|x| x == "-ngl" || x == "-t"));
    l.gpu_layers = Some("0".into());
    l.threads = Some(4);
    let a = argv(&l);
    assert!(a.windows(2).any(|w| w == ["-ngl", "0"]) && a.windows(2).any(|w| w == ["-t", "4"]));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn bad_settings_are_refused_before_anything_runs() {
    let d = tmp("bad");
    let key = d.join(KEY_FILE);
    let m = |f: &str| Managed {
        model_file: f.into(),
        ..Default::default()
    };
    // A missing model says how to get it.
    let e = Launch::new(&d, &m("nope.gguf"), "x".into(), key.clone()).unwrap_err();
    assert!(e.contains("ndcart ai pull"), "{e}");
    assert!(Launch::new(&d, &m("../etc/passwd"), "x".into(), key.clone()).is_err());
    std::fs::create_dir_all(d.join("models")).unwrap();
    std::fs::write(d.join("models/m.gguf"), b"GGUF").unwrap();
    // gpu_layers cannot smuggle another flag in.
    let mut bad = m("m.gguf");
    bad.gpu_layers = Some("--api-key".into());
    assert!(Launch::new(&d, &bad, "x".into(), key.clone())
        .unwrap_err()
        .contains("gpu_layers"));
    for ok in ["auto", "all", "0", "99"] {
        let mut g = m("m.gguf");
        g.gpu_layers = Some(ok.into());
        assert!(Launch::new(&d, &g, "x".into(), key.clone()).is_ok(), "{ok}");
    }
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_key_is_random_private_and_kept() {
    let d = tmp("key");
    let p = ensure_key(&d).unwrap();
    let k1 = std::fs::read_to_string(&p).unwrap();
    assert_eq!(k1.len(), 64);
    assert!(k1.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(ensure_key(&d).unwrap(), p);
    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        k1,
        "an existing key is kept"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let d2 = tmp("key2");
    assert_ne!(
        std::fs::read_to_string(ensure_key(&d2).unwrap()).unwrap(),
        k1
    );
    let _ = std::fs::remove_dir_all(&d);
    let _ = std::fs::remove_dir_all(&d2);
}

#[test]
fn the_record_round_trips_and_a_dead_pid_is_a_stale_record() {
    let d = tmp("rec");
    assert_eq!(
        stop(&d, Duration::from_secs(1)).unwrap(),
        Stopped::NotRunning
    );
    let l = launch(&d);
    // A pid far above any real one is not alive.
    let mut rec = rec_for(&l, 4_000_000);
    write_rec(&d, &rec).unwrap();
    assert_eq!(read_rec(&d).unwrap(), rec);
    assert_eq!(
        stop(&d, Duration::from_secs(1)).unwrap(),
        Stopped::StaleRecordRemoved
    );
    assert!(read_rec(&d).is_none());
    #[cfg(target_os = "linux")]
    {
        // Our own pid is alive but is not llama-server: never signalled, record dropped.
        rec.pid = std::process::id();
        assert!(is_alive(rec.pid));
        assert_eq!(belongs(&rec), Some(false));
        write_rec(&d, &rec).unwrap();
        assert_eq!(
            stop(&d, Duration::from_secs(1)).unwrap(),
            Stopped::StaleRecordRemoved
        );
    }
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn model_arguments_resolve_to_catalog_files_or_paths() {
    let d = tmp("arg");
    let cat = catalog::bundled();
    let e = &cat[0];
    assert!(resolve_model_arg(&d, &e.id, &cat)
        .unwrap_err()
        .contains("ndcart ai pull"));
    let f = d.join("models").join(&e.hf_repo).join(&e.file);
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    std::fs::write(&f, b"GGUF").unwrap();
    assert_eq!(
        resolve_model_arg(&d, &e.id, &cat).unwrap(),
        format!("{}/{}", e.hf_repo, e.file)
    );
    assert!(resolve_model_arg(&d, "nothing.gguf", &cat).is_err());
    assert!(resolve_model_arg(&d, "../x", &cat).is_err());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn flags_override_saved_settings_and_unknown_flags_are_errors() {
    let d = tmp("flags");
    std::fs::create_dir_all(d.join("models")).unwrap();
    std::fs::write(d.join("models/m.gguf"), b"GGUF").unwrap();
    let stored = StoredAiSettings {
        managed: Some(Managed {
            model_file: "m.gguf".into(),
            port: Some(1111),
            ..Default::default()
        }),
        ..Default::default()
    };
    let f = parse_flags(vec![
        "--port".into(),
        "2222".into(),
        "--ctx".into(),
        "8192".into(),
        "--gpu-layers".into(),
        "0".into(),
    ])
    .unwrap();
    let m = managed_from_flags(&d, &stored, &f).unwrap();
    assert_eq!(
        (m.port(), m.ctx(), m.gpu_layers.as_deref()),
        (2222, 8192, Some("0"))
    );
    assert!(parse_flags(vec!["--wat".into()]).is_err());
    assert!(parse_flags(vec!["--port".into(), "x".into()]).is_err());
    let none = managed_from_flags(&d, &StoredAiSettings::default(), &Flags::default()).unwrap_err();
    assert!(none.contains("ndcart ai setup"));
    let _ = std::fs::remove_dir_all(&d);
}

#[cfg(unix)]
mod supervised {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn script(dir: &Path, body: &str) -> PathBuf {
        let p = dir.join("llama-server");
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    fn fast() -> Backoff {
        Backoff {
            delays: vec![Duration::from_millis(30); 3],
            healthy_after: Duration::from_secs(60),
            ready_timeout: Duration::from_secs(3),
        }
    }

    #[tokio::test]
    async fn a_server_that_keeps_dying_is_given_up_on_after_the_backoff_and_reported() {
        let d = tmp("giveup");
        let mut l = launch(&d);
        l.runtime = script(&d, "exit 3");
        l.port = free_port();
        let sup = Supervisor::spawn(d.clone(), l, fast());
        let start = Instant::now();
        loop {
            let s = status();
            if s.state == "gave_up" {
                assert_eq!(s.restarts, 3);
                assert!(s.detail.contains("kept stopping"), "{}", s.detail);
                break;
            }
            assert!(start.elapsed() < Duration::from_secs(20), "{s:?}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        sup.shutdown().await;
        assert!(read_rec(&d).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn start_reports_an_early_exit_with_the_log_and_a_busy_port() {
        let d = tmp("early");
        let mut l = launch(&d);
        l.runtime = script(&d, "echo 'error: model is corrupt' >&2; exit 1");
        l.port = free_port();
        let e = start(&d, &l, Duration::from_secs(5)).await.unwrap_err();
        assert!(
            e.contains("exited") && e.contains("model is corrupt"),
            "{e}"
        );
        assert!(read_rec(&d).is_none());
        let busy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.port = busy.local_addr().unwrap().port();
        assert!(start(&d, &l, Duration::from_secs(1))
            .await
            .unwrap_err()
            .contains("already in use"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
