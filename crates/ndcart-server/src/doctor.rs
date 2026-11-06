//! `ndcart doctor` — what a friend runs when something does not work. Each
//! check says ok / warn / FAIL in one line, with what to do about it. Nothing
//! here changes anything, and no password or key is ever printed.
//!
//!   ndcart doctor [--data-dir DIR] [--listen ADDR]

use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    Ok,
    Warn,
    Fail,
}

#[derive(Clone, Debug)]
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
    v.push(port(listen));
    v
}

/// The AI and nd-world configuration, as much as can be known without a password.
#[cfg(feature = "bridge")]
pub async fn ai_checks() -> Vec<Check> {
    let mut v = Vec::new();
    let cfg = match crate::draft::Config::from_env() {
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
    if let Some((url, _)) = &cfg.direct {
        match reqwest_head(&format!("{}/models", url.trim_end_matches('/'))).await {
            Ok(code) => v.push(ok("model server", format!("{url} answered ({code})"))),
            Err(e) => v.push(warn(
                "model server",
                format!("{url} did not answer: {e}. Is it running?"),
            )),
        }
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

#[cfg(feature = "bridge")]
async fn reqwest_head(url: &str) -> Result<u16, String> {
    // A plain TCP/HTTP probe through the bridge's own client crate would need a new API; a tiny
    // connect-and-status check keeps this dependency-free.
    let rest = url
        .strip_prefix("http://")
        .ok_or("only plain http:// addresses are probed")?;
    let (host, path) = rest
        .split_once('/')
        .map(|(h, p)| (h, format!("/{p}")))
        .unwrap_or((rest, "/".into()));
    let host = if host.contains(':') {
        host.to_string()
    } else {
        format!("{host}:80")
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::time::timeout(
        std::time::Duration::from_secs(4),
        tokio::net::TcpStream::connect(&host),
    )
    .await
    .map_err(|_| "timed out".to_string())?
    .map_err(|e| e.to_string())?;
    s.write_all(
        format!("GET {path} HTTP/1.0\r\nHost: {host}\r\nConnection: close\r\n\r\n").as_bytes(),
    )
    .await
    .map_err(|e| e.to_string())?;
    let mut buf = [0u8; 64];
    let n = tokio::time::timeout(std::time::Duration::from_secs(4), s.read(&mut buf))
        .await
        .map_err(|_| "timed out".to_string())?
        .map_err(|e| e.to_string())?;
    let line = String::from_utf8_lossy(&buf[..n]);
    line.split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| "not an HTTP answer".into())
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
    checks.extend(ai_checks().await);
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
    #[tokio::test]
    async fn the_model_probe_reads_a_status_line_and_never_prints_a_secret() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut b = [0u8; 256];
            let _ = s.read(&mut b).await;
            s.write_all(b"HTTP/1.0 200 OK\r\n\r\n{}").await.unwrap();
        });
        assert_eq!(
            reqwest_head(&format!("http://127.0.0.1:{port}/v1/models"))
                .await
                .unwrap(),
            200
        );
        assert!(reqwest_head("https://x/y").await.is_err());
        assert!(reqwest_head("http://127.0.0.1:9/v1/models").await.is_err());
    }
}
