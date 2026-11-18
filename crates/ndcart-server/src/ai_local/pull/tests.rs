//! The download, against an in-process stand-in for the Hub (no network, no real model).

use super::*;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, Response as HttpResponse};
use axum::Router;
use std::sync::{Arc, Mutex};

const REPO: &str = "unsloth/Test-GGUF";

#[derive(Clone, Debug)]
struct Seen {
    path: String,
    range: Option<String>,
    auth: Option<String>,
    host: Option<String>,
}

struct Hub {
    name: String,
    bytes: Vec<u8>,
    /// What the tree API claims the checksum is (differs from the bytes to test mismatches).
    claim_sha: String,
    redirect_to: Mutex<Option<String>>,
    ignore_range: bool,
    seen: Mutex<Vec<Seen>>,
}

fn sha_hex(b: &[u8]) -> String {
    hex(&Sha256::digest(b))
}

fn content(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i * 7 % 251) as u8).collect()
}

async fn handle(State(h): State<Arc<Hub>>, req: Request) -> HttpResponse<Body> {
    let hdr = |m: &HeaderMap, k: &str| m.get(k).and_then(|v| v.to_str().ok()).map(str::to_string);
    let path = req.uri().path().to_string();
    h.seen.lock().unwrap().push(Seen {
        path: path.clone(),
        range: hdr(req.headers(), "range"),
        auth: hdr(req.headers(), "authorization"),
        host: hdr(req.headers(), "host"),
    });
    let ok = |b: Body| HttpResponse::builder().status(200).body(b).unwrap();
    if path == format!("/api/models/{REPO}/tree/main") {
        let v = serde_json::json!([
            {"type":"file","path":"README.md","size":10},
            {"type":"file","path":h.name,"size":h.bytes.len(),
             "lfs":{"oid":h.claim_sha,"size":h.bytes.len(),"pointerSize":134}},
            {"type":"file","path":"mmproj-F16.gguf","size":5,"lfs":{"oid":sha_hex(b"x"),"size":5}}
        ]);
        return ok(Body::from(v.to_string()));
    }
    let is_resolve = path == format!("/{REPO}/resolve/main/{}", h.name);
    if is_resolve {
        if let Some(to) = h.redirect_to.lock().unwrap().clone() {
            return HttpResponse::builder()
                .status(302)
                .header(header::LOCATION, format!("{to}/cdn/{}", h.name))
                .body(Body::empty())
                .unwrap();
        }
    }
    if is_resolve || path == format!("/cdn/{}", h.name) {
        let start = hdr(req.headers(), "range")
            .and_then(|r| {
                r.strip_prefix("bytes=")
                    .and_then(|r| r.strip_suffix('-').map(str::to_string))
            })
            .and_then(|n| n.parse::<usize>().ok());
        return match start {
            Some(n) if !h.ignore_range && n < h.bytes.len() => HttpResponse::builder()
                .status(206)
                .header(
                    "content-range",
                    format!("bytes {n}-{}/{}", h.bytes.len() - 1, h.bytes.len()),
                )
                .body(Body::from(h.bytes[n..].to_vec()))
                .unwrap(),
            _ => ok(Body::from(h.bytes.clone())),
        };
    }
    HttpResponse::builder()
        .status(404)
        .body(Body::empty())
        .unwrap()
}

async fn hub(bytes: Vec<u8>, claim: Option<String>, ignore_range: bool) -> (Arc<Hub>, String) {
    let claim_sha = claim.unwrap_or_else(|| sha_hex(&bytes));
    let h = Arc::new(Hub {
        name: "Test-Q4.gguf".into(),
        bytes,
        claim_sha,
        redirect_to: Mutex::new(None),
        ignore_range,
        seen: Mutex::new(Vec::new()),
    });
    let app = Router::new().fallback(handle).with_state(h.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = axum::serve(l, app).await;
    });
    (h, format!("http://127.0.0.1:{port}"))
}

fn pull_for(base: &str, token: Option<&str>, hosts: &[&str]) -> Pull {
    Pull {
        client: client(),
        policy: Policy {
            base: base.to_string(),
            hosts: hosts.iter().map(|s| s.to_string()).collect(),
            allow_http: true,
        },
        token: token.map(str::to_string),
        free_bytes: Box::new(|_| Some(u64::MAX / 2)),
    }
}

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ndcart-pull-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn target() -> Target {
    Target {
        repo: REPO.into(),
        file: Some("Test-Q4.gguf".into()),
        quant: Some("Q4".into()),
        license: "mit".into(),
        catalog_id: None,
        pinned_sha256: None,
    }
}

async fn run(p: &Pull, data: &Path) -> Result<String, String> {
    let plan = p.plan(&target()).await?;
    p.download(&plan, data, &mut |_| {}, None).await
}

#[tokio::test]
async fn the_happy_path_verifies_the_checksum_renames_and_records_it() {
    let bytes = content(300_000);
    let (hub, base) = hub(bytes.clone(), None, false).await;
    let p = pull_for(&base, None, &["127.0.0.1"]);
    let plan = p.plan(&target()).await.unwrap();
    assert_eq!(plan.total_bytes, 300_000);
    assert_eq!(
        plan.files[0].sha256.as_deref(),
        Some(sha_hex(&bytes).as_str())
    );
    let data = tmp("ok");
    let mut last = None;
    let rel = p
        .download(&plan, &data, &mut |pr| last = Some(pr.clone()), None)
        .await
        .unwrap();
    assert_eq!(rel, format!("{REPO}/Test-Q4.gguf"));
    let dest = models_dir(&data).join(&rel);
    assert_eq!(std::fs::read(&dest).unwrap(), bytes);
    assert!(!dest.with_extension("gguf.part").exists());
    let sc: Sidecar =
        serde_json::from_str(&std::fs::read_to_string(sidecar_path(&dest)).unwrap()).unwrap();
    assert_eq!(
        (sc.sha256, sc.size, sc.sha256_source.as_str()),
        (sha_hex(&bytes), 300_000, "hub")
    );
    assert_eq!(sc.license, "mit");
    let last = last.unwrap();
    assert_eq!((last.done, last.total), (300_000, 300_000));
    // Run again: the file is already here with its record, so nothing is fetched.
    let before = hub.seen.lock().unwrap().len();
    run(&p, &data).await.unwrap();
    let new: Vec<_> = hub.seen.lock().unwrap()[before..]
        .iter()
        .map(|s| s.path.clone())
        .collect();
    assert!(new.iter().all(|p| p.contains("/tree/")), "{new:?}");
    let _ = std::fs::remove_dir_all(&data);
}

#[tokio::test]
async fn a_wrong_checksum_deletes_the_part_file_and_errors() {
    let (_h, base) = hub(content(50_000), Some(sha_hex(b"something else")), false).await;
    let p = pull_for(&base, None, &["127.0.0.1"]);
    let data = tmp("bad");
    let err = run(&p, &data).await.unwrap_err();
    assert!(err.contains("checksum mismatch"), "{err}");
    let dir = models_dir(&data).join(REPO);
    assert!(!dir.join("Test-Q4.gguf").exists());
    assert!(!dir.join("Test-Q4.gguf.part").exists());
    let _ = std::fs::remove_dir_all(&data);
}

#[tokio::test]
async fn a_part_file_is_resumed_with_a_range_request() {
    let bytes = content(200_000);
    let (hub, base) = hub(bytes.clone(), None, false).await;
    let p = pull_for(&base, None, &["127.0.0.1"]);
    let data = tmp("resume");
    let dir = models_dir(&data).join(REPO);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Test-Q4.gguf.part"), &bytes[..65_536]).unwrap();
    let mut saw_resume = false;
    let plan = p.plan(&target()).await.unwrap();
    p.download(&plan, &data, &mut |pr| saw_resume |= pr.resumed, None)
        .await
        .unwrap();
    assert!(saw_resume);
    assert_eq!(std::fs::read(dir.join("Test-Q4.gguf")).unwrap(), bytes);
    let ranges: Vec<_> = hub
        .seen
        .lock()
        .unwrap()
        .iter()
        .filter_map(|s| s.range.clone())
        .collect();
    assert_eq!(ranges, vec!["bytes=65536-".to_string()]);
    let _ = std::fs::remove_dir_all(&data);
}

#[tokio::test]
async fn a_server_that_ignores_range_restarts_cleanly_and_a_corrupt_part_fails_the_hash() {
    let bytes = content(120_000);
    let (_h, base) = hub(bytes.clone(), None, true).await;
    let p = pull_for(&base, None, &["127.0.0.1"]);
    let data = tmp("norange");
    let dir = models_dir(&data).join(REPO);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Test-Q4.gguf.part"), &bytes[..4096]).unwrap();
    run(&p, &data).await.unwrap();
    assert_eq!(std::fs::read(dir.join("Test-Q4.gguf")).unwrap(), bytes);
    let _ = std::fs::remove_dir_all(&data);

    // A part file with wrong bytes resumes into a mismatch and is thrown away.
    let (_h, base) = hub(bytes.clone(), None, false).await;
    let p = pull_for(&base, None, &["127.0.0.1"]);
    let data = tmp("corruptpart");
    let dir = models_dir(&data).join(REPO);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Test-Q4.gguf.part"), vec![9u8; 4096]).unwrap();
    assert!(run(&p, &data)
        .await
        .unwrap_err()
        .contains("checksum mismatch"));
    assert!(!dir.join("Test-Q4.gguf.part").exists());
    let _ = std::fs::remove_dir_all(&data);
}

#[tokio::test]
async fn a_redirect_to_an_unlisted_host_is_refused() {
    let (hub, base) = hub(content(1000), None, false).await;
    *hub.redirect_to.lock().unwrap() = Some("http://evil.example".into());
    let p = pull_for(&base, None, &["127.0.0.1"]);
    let data = tmp("evil");
    let err = run(&p, &data).await.unwrap_err();
    assert!(
        err.contains("refusing") && err.contains("evil.example"),
        "{err}"
    );
    assert!(!models_dir(&data).join(REPO).join("Test-Q4.gguf").exists());
    let _ = std::fs::remove_dir_all(&data);
}

#[tokio::test]
async fn the_token_goes_to_the_hub_but_never_to_the_host_it_redirects_to() {
    let bytes = content(5000);
    let (hub, base) = hub(bytes.clone(), None, false).await;
    let port = base.rsplit(':').next().unwrap().to_string();
    *hub.redirect_to.lock().unwrap() = Some(format!("http://localhost:{port}"));
    // `localhost` is a listed CDN-like host here, distinct from the base host 127.0.0.1.
    let p = pull_for(&base, Some("hf_secret_token"), &["127.0.0.1", "localhost"]);
    let data = tmp("token");
    run(&p, &data).await.unwrap();
    let seen = hub.seen.lock().unwrap().clone();
    let hub_hits: Vec<_> = seen
        .iter()
        .filter(|s| !s.path.starts_with("/cdn/"))
        .collect();
    let cdn_hits: Vec<_> = seen
        .iter()
        .filter(|s| s.path.starts_with("/cdn/"))
        .collect();
    assert!(!cdn_hits.is_empty());
    assert!(hub_hits
        .iter()
        .all(|s| s.auth.as_deref() == Some("Bearer hf_secret_token")));
    assert!(cdn_hits.iter().all(|s| s.auth.is_none()), "{cdn_hits:?}");
    assert!(cdn_hits[0]
        .host
        .as_deref()
        .unwrap()
        .starts_with("localhost"));
    let _ = std::fs::remove_dir_all(&data);
}

#[test]
fn low_disk_and_the_size_cap_refuse_before_any_download() {
    let plan = Plan {
        repo: REPO.into(),
        files: vec![FileSpec {
            path: "a.gguf".into(),
            size: 1000,
            sha256: None,
            pinned: false,
        }],
        total_bytes: 1000,
        license: String::new(),
        catalog_id: None,
    };
    assert!(check_budget(&plan, None, Some(1099))
        .unwrap_err()
        .contains("not enough free disk"));
    assert!(check_budget(&plan, None, Some(1100)).unwrap().is_none());
    assert!(check_budget(&plan, Some(999), Some(1_000_000))
        .unwrap_err()
        .contains("--max-gb"));
    // Unknown free space is a note, not a refusal.
    assert!(check_budget(&plan, None, None).unwrap().is_some());
    assert_eq!(parse_df("Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/sda1 1000 200 800 20% /\n"), Some(800 * 1024));
    assert_eq!(parse_df(""), None);
}

#[test]
fn files_are_selected_by_name_then_quant_and_split_sets_stay_together() {
    let item = |p: &str| TreeItem {
        kind: "file".into(),
        path: p.into(),
        size: Some(10),
        lfs: Some(Lfs {
            oid: "a".repeat(64),
            size: Some(10),
        }),
    };
    let tree = vec![
        item("README.md"),
        item("M-Q4_K_M.gguf"),
        item("M-UD-Q4_K_XL.gguf"),
        item("mmproj-F16.gguf"),
        item("Big-Q8_0-00001-of-00002.gguf"),
        item("Big-Q8_0-00002-of-00002.gguf"),
    ];
    assert_eq!(
        select(&tree, Some("M-Q4_K_M.gguf"), None).unwrap()[0].path,
        "M-Q4_K_M.gguf"
    );
    // The named file is missing: the quant picks the one that contains it.
    assert_eq!(
        select(&tree, Some("gone.gguf"), Some("UD-Q4_K_XL")).unwrap()[0].path,
        "M-UD-Q4_K_XL.gguf"
    );
    // Ambiguous quants list the candidates instead of guessing.
    let e = select(&tree, None, Some("Q4")).unwrap_err();
    assert!(
        e.contains("M-Q4_K_M.gguf") && e.contains("M-UD-Q4_K_XL.gguf"),
        "{e}"
    );
    // A missing file lists what is there; a vision projector is never picked by quant.
    assert!(select(&tree, Some("gone.gguf"), None)
        .unwrap_err()
        .contains("Files there"));
    assert!(select(&tree, None, Some("F16")).is_err());
    let set = select(&tree, None, Some("Q8_0")).unwrap();
    assert_eq!(set.len(), 2);
    assert_eq!(set[1].path, "Big-Q8_0-00002-of-00002.gguf");
    assert!(select(&tree[..5], None, Some("Q8_0"))
        .unwrap_err()
        .contains("missing part 2"));
    assert!(select(&[item("README.md")], None, None).is_err());
}

#[test]
fn targets_are_catalog_ids_or_unsloth_repos_unless_allowed() {
    let cat = catalog::bundled();
    let t = parse_target("qwen3.5-4b", &cat, false).unwrap();
    assert_eq!(t.repo, "unsloth/Qwen3.5-4B-GGUF");
    assert_eq!(t.catalog_id.as_deref(), Some("qwen3.5-4b"));
    let t = parse_target("unsloth/X-GGUF:X-Q4.gguf", &cat, false).unwrap();
    assert_eq!((t.file.as_deref(), t.quant), (Some("X-Q4.gguf"), None));
    let t = parse_target("unsloth/X-GGUF:UD-Q4_K_XL", &cat, false).unwrap();
    assert_eq!((t.file, t.quant.as_deref()), (None, Some("UD-Q4_K_XL")));
    assert!(parse_target("someone/Else-GGUF", &cat, false)
        .unwrap_err()
        .contains("--allow-non-unsloth"));
    assert!(parse_target("someone/Else-GGUF", &cat, true).is_ok());
    assert!(parse_target("nonsense", &cat, false).is_err());
    assert!(parse_target("unsloth/../x-GGUF", &cat, false).is_err());
    assert!(parse_target("a/b/c-GGUF", &cat, true).is_err());
}

#[test]
fn hosts_and_schemes_are_policed() {
    let p = Policy::hub();
    let u = |s: &str| Url::parse(s).unwrap();
    assert!(p.check(&u("https://huggingface.co/a")).is_ok());
    assert!(p.check(&u("https://cdn-lfs.huggingface.co/a")).is_ok());
    assert!(p.check(&u("https://cas-bridge.xethub.hf.co/a")).is_ok());
    assert!(p.check(&u("http://huggingface.co/a")).is_err());
    assert!(p.check(&u("https://evilhuggingface.co/a")).is_err());
    assert!(p
        .check(&u("https://huggingface.co.evil.example/a"))
        .is_err());
    assert!(p.token_ok(&u("https://huggingface.co/a")));
    assert!(!p.token_ok(&u("https://cdn-lfs.huggingface.co/a")));
    assert_eq!(url_path("a b/c+d.gguf"), "a%20b/c%2Bd.gguf");
    assert!(
        safe_rel("../x.gguf").is_err()
            && safe_rel("a//b").is_err()
            && safe_rel("sub/ok.gguf").is_ok()
    );
}

#[test]
fn import_copies_a_gguf_and_records_its_checksum_and_refuses_other_files() {
    let d = tmp("import");
    std::fs::create_dir_all(&d).unwrap();
    let src = d.join("mine.gguf");
    let mut body = b"GGUF".to_vec();
    body.extend(content(4096));
    std::fs::write(&src, &body).unwrap();
    let rel = import(&d.join("data"), &src, &|_| Some(u64::MAX / 2)).unwrap();
    assert_eq!(rel, "local/mine.gguf");
    let dest = models_dir(&d.join("data")).join(&rel);
    assert_eq!(std::fs::read(&dest).unwrap(), body);
    let sc: Sidecar =
        serde_json::from_str(&std::fs::read_to_string(sidecar_path(&dest)).unwrap()).unwrap();
    assert_eq!(sc.sha256, sha_hex(&body));
    let fake = d.join("fake.gguf");
    std::fs::write(&fake, b"not a model").unwrap();
    assert!(import(&d.join("data"), &fake, &|_| None)
        .unwrap_err()
        .contains("not a GGUF"));
    assert!(import(&d.join("data"), &d.join("x.bin"), &|_| None).is_err());
    assert!(import(&d.join("data"), &src, &|_| Some(10))
        .unwrap_err()
        .contains("free disk"));
    let _ = std::fs::remove_dir_all(&d);
}
