//! `ndcart doctor` — what a friend runs when something does not work. Each
//! check says ok / warn / FAIL in one line, with what to do about it. Nothing
//! here changes anything, and no password or key is ever printed.
//!
//!   ndcart doctor [--data-dir DIR] [--listen ADDR]

use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Ok,
    Warn,
    Fail,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct Check {
    pub level: Level,
    pub what: &'static str,
    pub detail: String,
}

fn ok(what: &'static str, detail: impl Into<String>) -> Check {
    Check {
        level: Level::Ok,
        what,
        detail: detail.into(),
    }
}
fn warn(what: &'static str, detail: impl Into<String>) -> Check {
    Check {
        level: Level::Warn,
        what,
        detail: detail.into(),
    }
}
fn fail(what: &'static str, detail: impl Into<String>) -> Check {
    Check {
        level: Level::Fail,
        what,
        detail: detail.into(),
    }
}

/// Can we create and remove a file in `dir` (making it if needed)?
fn data_dir(dir: &Path) -> Check {
    if let Err(e) = std::fs::create_dir_all(dir) {
        return fail(
            "data folder",
            format!(
                "cannot create {}: {e}. Pick a folder you own with --data-dir.",
                dir.display()
            ),
        );
    }
    let probe = dir.join(".ndcart-doctor");
    match std::fs::write(&probe, b"ok").and_then(|_| std::fs::remove_file(&probe)) {
        Ok(()) => {
            let docs = std::fs::read_dir(dir)
                .map(|rd| {
                    rd.flatten()
                        .filter(|e| e.path().join("manifest.json").is_file())
                        .count()
                })
                .unwrap_or(0);
            ok(
                "data folder",
                format!("{} is writable ({docs} project(s))", dir.display()),
            )
        }
        Err(e) => fail(
            "data folder",
            format!("{} is not writable: {e}", dir.display()),
        ),
    }
}

/// Every picture format and the bundled font work.
fn codecs_and_fonts() -> Vec<Check> {
    let mut out = Vec::new();
    let mut pm = tiny_skia::Pixmap::new(16, 16).expect("pixmap");
    pm.fill(tiny_skia::Color::from_rgba8(60, 90, 160, 255));
    for f in ["png", "webp", "avif"] {
        let format = crate::tiles::ImageFormat::parse(f).expect("known format");
        out.push(match crate::tiles::encode_for_doctor(&pm, format) {
            Ok(n) => ok("picture format", format!("{f} encodes ({n} bytes)")),
            Err(e) => fail("picture format", format!("{f} failed: {e}")),
        });
    }
    let lay = crate::text::layout("Neon & Dragons", false, 24.0, 0.0);
    out.push(if lay.width > 20.0 && !lay.glyphs.is_empty() {
        ok(
            "fonts",
            format!("the bundled font lays out text ({:.0} px wide)", lay.width),
        )
    } else {
        fail(
            "fonts",
            "the bundled font produced no text; the build may be damaged",
        )
    });
    out
}

/// Is the address free to listen on (or already ours)?
fn port(listen: &str) -> Check {
    match std::net::TcpListener::bind(listen) {
        Ok(_) => ok("address", format!("{listen} is free")),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            warn("address", format!("{listen} is already in use (a studio may already be running there; otherwise pick another with --listen)"))
        }
        Err(e) => fail("address", format!("cannot listen on {listen}: {e}")),
    }
}

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

/// The offline checks.
pub fn local_checks(data: Option<&Path>, listen: &str) -> Vec<Check> {
    local_checks_with(data, Some(listen))
}

/// The offline checks; `listen: None` skips the address check (the studio is already listening).
pub fn local_checks_with(data: Option<&Path>, listen: Option<&str>) -> Vec<Check> {
    let mut v = vec![ok(
        "build",
        format!(
            "ndcart {} ({} {})",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH
        ),
    )];
    v.push(match data {
        Some(d) => data_dir(d),
        None => warn("data folder", "none set: projects live in memory and vanish when the studio stops. Use --data-dir or ND_CARTO_DATA_DIR."),
    });
    v.extend(codecs_and_fonts());
    if let Some(l) = listen {
        v.push(port(l));
    }
    v
}

/// The AI and nd-world configuration, as much as can be known without a password. Reads the
/// saved AI settings from `data` (environment variables still win per field).
#[cfg(feature = "bridge")]
pub async fn ai_checks(data: Option<&Path>) -> Vec<Check> {
    ai_checks_with(&crate::ai_settings::load(data)).await
}

/// The same checks for settings already in memory (the running studio's live copy).
#[cfg(feature = "bridge")]
pub async fn ai_checks_with(stored: &crate::ai_settings::StoredAiSettings) -> Vec<Check> {
    let mut v = Vec::new();
    let cfg = match crate::draft::Config::from_env_and(stored) {
        Ok(c) => c,
        Err(m) => {
            v.push(warn(
                "AI",
                format!("not set up: {m}. The studio works fully without it."),
            ));
            return v;
        }
    };
    v.push(ok("AI", cfg.describe()));
    if let Some((url, model)) = &cfg.direct {
        v.extend(probe_model_server(url, model, cfg.key.as_deref()).await);
    } else if let Some(server) = &cfg.server {
        match ndcart_bridge::Bridge::new(server, cfg.world.as_deref()) {
            Ok(b) => match b.probe().await {
                Ok(p) => v.push(ok(
                    "nd-world",
                    format!("{server} is up (build {}, {} mode)", p.build, p.mode),
                )),
                Err(e) => v.push(fail("nd-world", format!("{server}: {e}"))),
            },
            Err(e) => v.push(fail("nd-world", e.to_string())),
        }
        let has_pw = env("ND_CARTO_PASSWORD").is_some() || env("ND_CARTO_PASSWORD_FILE").is_some();
        v.push(if has_pw {
            ok("password", "found in the environment (not shown)")
        } else {
            warn("password", "ND_CARTO_PASSWORD (or ND_CARTO_PASSWORD_FILE) is not set, so publishing and AI will ask for it and fail")
        });
    }
    v
}

/// `scheme://host[:port]` of an endpoint address (what `/health` hangs off).
#[cfg(feature = "bridge")]
fn origin_of(url: &str) -> &str {
    let after = url.find("://").map(|i| i + 3).unwrap_or(0);
    match url[after..].find('/') {
        Some(i) => &url[..after + i],
        None => url,
    }
}

/// Probe a model server the way a client would: `/health` (llama.cpp: 503 while the model
/// loads, 200 when ready), then `{base}/models` with the key, checking the configured id is
/// served. The key is sent but never printed.
#[cfg(feature = "bridge")]
async fn probe_model_server(url: &str, model: &str, key: Option<&str>) -> Vec<Check> {
    let mut v = Vec::new();
    let client = ndcart_bridge::plain_client();
    let base = url.trim_end_matches('/');
    let get = |u: String, with_key: bool| {
        let mut r = client.get(u);
        if with_key {
            if let Some(k) = key {
                r = r.bearer_auth(k);
            }
        }
        r
    };
    match get(format!("{}/health", origin_of(base)), false)
        .send()
        .await
    {
        Ok(r) => match r.status().as_u16() {
            200 => v.push(ok("model server", format!("{base} is ready"))),
            503 => {
                v.push(warn(
                    "model server",
                    format!("{base} is still loading the model; wait a moment and try again"),
                ));
                return v;
            }
            // Not every server has /health (Ollama does not): the model list decides.
            _ => {}
        },
        Err(e) => {
            v.push(warn(
                "model server",
                format!("{base} did not answer: {e}. Is it running?"),
            ));
            return v;
        }
    }
    let models_url = if base.ends_with("/v1") {
        format!("{base}/models")
    } else {
        format!("{base}/v1/models")
    };
    match get(models_url, true).send().await {
        Ok(r) if r.status().as_u16() == 401 || r.status().as_u16() == 403 => v.push(warn(
            "model list",
            "the server refused the API key (or wants one); set it in Settings or ND_CARTO_LLM_KEY",
        )),
        Ok(r) if r.status().is_success() => {
            let body: serde_json::Value = r.json().await.unwrap_or_default();
            let ids: Vec<String> = body
                .get("data")
                .and_then(|d| d.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|m| m.get("id").and_then(|i| i.as_str()).map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            if ids.is_empty() || ids.iter().any(|i| i == model) {
                v.push(ok("model", format!("'{model}' is served")));
            } else {
                v.push(warn(
                    "model",
                    format!("model '{model}' is not served; served: {}", ids.join(", ")),
                ));
            }
        }
        Ok(r) => v.push(warn(
            "model list",
            format!("{base}/models answered {}", r.status()),
        )),
        Err(e) => v.push(warn(
            "model list",
            format!("cannot read the model list: {e}"),
        )),
    }
    v
}

pub fn render(checks: &[Check]) -> String {
    let mut out = String::new();
    for c in checks {
        let tag = match c.level {
            Level::Ok => "ok  ",
            Level::Warn => "warn",
            Level::Fail => "FAIL",
        };
        out.push_str(&format!("[{tag}] {}: {}\n", c.what, c.detail));
    }
    let (w, f) = (
        checks.iter().filter(|c| c.level == Level::Warn).count(),
        checks.iter().filter(|c| c.level == Level::Fail).count(),
    );
    out.push_str(&if f > 0 {
        format!("\n{f} problem(s) to fix, {w} warning(s).\n")
    } else if w > 0 {
        format!("\nNothing is broken; {w} thing(s) worth a look.\n")
    } else {
        "\nAll good.\n".into()
    });
    out
}

pub async fn run(args: Vec<String>) -> Result<(), String> {
    let (mut data, mut listen): (Option<PathBuf>, Option<String>) = (None, None);
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--data-dir" => data = it.next().map(PathBuf::from),
            "--listen" => listen = it.next(),
            o => return Err(format!("unknown argument: {o}")),
        }
    }
    let data = data.or_else(|| env("ND_CARTO_DATA_DIR").map(PathBuf::from));
    let listen = listen
        .or_else(|| env("ND_CARTO_LISTEN"))
        .unwrap_or_else(|| "127.0.0.1:8300".into());
    #[allow(unused_mut)]
    let mut checks = local_checks(data.as_deref(), &listen);
    #[cfg(feature = "bridge")]
    checks.extend(ai_checks(data.as_deref()).await);
    print!("{}", render(&checks));
    if checks.iter().any(|c| c.level == Level::Fail) {
        Err("doctor found problems".into())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ndcart-doctor-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn a_good_setup_passes_and_says_so() {
        let d = tmp("good");
        let checks = local_checks(Some(&d), "127.0.0.1:0");
        assert!(
            checks.iter().all(|c| c.level != Level::Fail),
            "{}",
            render(&checks)
        );
        let text = render(&checks);
        assert!(
            text.contains("[ok  ] data folder")
                && text.contains("avif encodes")
                && text.contains("bundled font"),
            "{text}"
        );
        assert!(text.ends_with("All good.\n"), "{text}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_missing_data_folder_warns_and_an_unwritable_one_fails_with_advice() {
        let c = local_checks(None, "127.0.0.1:0");
        assert!(c
            .iter()
            .any(|c| c.level == Level::Warn && c.detail.contains("vanish")));
        // A path under a regular file can never be made.
        let file = tmp("file");
        std::fs::write(&file, b"x").unwrap();
        let bad = file.join("inside");
        let c = local_checks(Some(&bad), "127.0.0.1:0");
        let f = c.iter().find(|c| c.what == "data folder").unwrap();
        assert_eq!(f.level, Level::Fail);
        assert!(f.detail.contains("--data-dir"), "{}", f.detail);
        assert!(render(&c).contains("problem(s) to fix"));
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn a_taken_address_is_a_warning_not_a_failure() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let c = port(&addr);
        assert_eq!(c.level, Level::Warn, "{}", c.detail);
        assert_eq!(port("not-an-address").level, Level::Fail);
    }

    #[cfg(feature = "bridge")]
    mod ai {
        use super::*;
        const KEY: &str = "sk-secret-do-not-print";

        /// A stand-in model server: /health answers 503 for the first `loading` calls, and
        /// /v1/models serves `ids`.
        async fn stand_in(loading: usize, ids: &'static [&'static str]) -> String {
            crate::ai_standin::spawn(crate::ai_standin::Options {
                loading,
                models: ids.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            })
            .await
            .base
        }

        fn settings(url: &str, model: &str) -> crate::ai_settings::StoredAiSettings {
            crate::ai_settings::StoredAiSettings {
                direct_url: Some(url.into()),
                direct_model: Some(model.into()),
                key: Some(KEY.into()),
                ..Default::default()
            }
        }

        #[tokio::test]
        async fn a_loading_server_warns_then_is_ok() {
            let url = stand_in(1, &["ndcart-local"]).await;
            let first = ai_checks_with(&settings(&url, "ndcart-local")).await;
            assert!(
                first
                    .iter()
                    .any(|c| c.level == Level::Warn && c.detail.contains("loading")),
                "{}",
                render(&first)
            );
            let second = ai_checks_with(&settings(&url, "ndcart-local")).await;
            assert!(
                second.iter().all(|c| c.level == Level::Ok),
                "{}",
                render(&second)
            );
        }

        #[tokio::test]
        async fn an_unserved_model_id_names_what_is_served() {
            let url = stand_in(0, &["alpha", "beta"]).await;
            let c = ai_checks_with(&settings(&url, "ndcart-local")).await;
            let w = c
                .iter()
                .find(|c| c.level == Level::Warn)
                .expect("a warning");
            assert!(
                w.detail.contains("'ndcart-local' is not served")
                    && w.detail.contains("alpha, beta"),
                "{}",
                w.detail
            );
        }

        #[tokio::test]
        async fn saved_settings_alone_make_the_ai_check_ok_and_the_key_is_never_printed() {
            let url = stand_in(0, &["ndcart-local"]).await;
            let d = tmp("saved-ai");
            crate::ai_settings::save(Some(&d), &settings(&url, "ndcart-local")).unwrap();
            let c = ai_checks(Some(&d)).await;
            let text = render(&c);
            assert!(c.iter().all(|c| c.level == Level::Ok), "{text}");
            assert!(text.contains("standalone"), "{text}");
            assert!(!text.contains(KEY), "{text}");
            // A dead endpoint is a warning that still hides the key.
            let dead = ai_checks_with(&settings("http://127.0.0.1:9/v1", "m")).await;
            assert!(dead.iter().any(|c| c.level == Level::Warn));
            assert!(!render(&dead).contains(KEY));
            let _ = std::fs::remove_dir_all(&d);
        }

        #[test]
        fn the_origin_drops_the_path() {
            assert_eq!(origin_of("http://h:1/v1"), "http://h:1");
            assert_eq!(origin_of("https://h"), "https://h");
        }
    }
}
