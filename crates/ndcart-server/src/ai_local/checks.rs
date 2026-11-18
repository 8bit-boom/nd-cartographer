//! `ndcart doctor` checks for a local model the studio manages (shown only when one is set
//! up). [`evaluate`] is pure: it judges facts passed in, so tests need no GPU, runtime or
//! model; [`gather`] collects the facts from this computer.

use super::hw::Hw;
use super::pull::{models_dir, sha256_file, sidecar_path, Sidecar};
use super::runtime::{self, Runtime};
use super::serve::{self, Health};
use super::{human_bytes, hw};
use crate::ai_settings::{Managed, StoredAiSettings};
use crate::doctor::{Check, Level};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

fn check(level: Level, what: &'static str, detail: impl Into<String>) -> Check {
    Check {
        level,
        what,
        detail: detail.into(),
    }
}

/// What is known about the checksum of the model file.
#[derive(Clone, Debug, PartialEq)]
pub enum Checksum {
    /// Hashed now, or remembered from a hash of the same size and modification time.
    Match {
        cached: bool,
    },
    Mismatch {
        expected: String,
        got: String,
    },
    /// No `<file>.json` record (an import by hand, or a file copied in).
    NoRecord,
    /// Not hashed in this run (the web panel never hashes gigabytes in a request).
    NotChecked,
}

#[derive(Clone, Debug)]
pub struct Facts {
    pub runtime: Option<Runtime>,
    pub hw: Hw,
    /// The model file's size, None when it is not there.
    pub model_size: Option<u64>,
    pub checksum: Checksum,
    pub health: Health,
    pub free_bytes: Option<u64>,
}

/// Judge the facts. Each problem appears once, with what to do about it.
pub fn evaluate(m: &Managed, f: &Facts) -> Vec<Check> {
    let mut v = Vec::new();

    // The runtime.
    v.push(match &f.runtime {
        Some(r) => check(
            Level::Ok,
            "local runtime",
            format!(
                "llama-server {} ({})",
                r.version.as_deref().unwrap_or("version unknown"),
                r.path.display()
            ),
        ),
        None => check(
            Level::Warn,
            "local runtime",
            format!(
                "llama-server was not found, so the local model cannot start. {}",
                runtime::install_hint_here().replace('\n', " ")
            ),
        ),
    });

    // The model file and its checksum.
    v.push(match (f.model_size, &f.checksum) {
        (None, _) => check(
            Level::Warn,
            "local model",
            format!(
                "the model file {} is missing. Download it again with `ndcart ai pull {}`, or pick another in Settings.",
                m.model_file,
                m.model_id.as_deref().unwrap_or("<id>")
            ),
        ),
        (Some(s), Checksum::Match { cached }) => check(
            Level::Ok,
            "local model",
            format!("{} ({}), checksum verified{}", m.model_file, human_bytes(s), if *cached { " earlier" } else { "" }),
        ),
        (Some(_), Checksum::Mismatch { expected, got }) => check(
            Level::Fail,
            "local model",
            format!(
                "{} does not match its recorded checksum (expected {}..., got {}...). The file may be damaged: delete it and download it again.",
                m.model_file,
                &expected[..expected.len().min(12)],
                &got[..got.len().min(12)]
            ),
        ),
        (Some(s), Checksum::NoRecord) => check(
            Level::Warn,
            "local model",
            format!("{} ({}) has no checksum record, so it cannot be verified (a file copied in by hand).", m.model_file, human_bytes(s)),
        ),
        (Some(s), Checksum::NotChecked) => check(
            Level::Ok,
            "local model",
            format!("{} ({}) is there; run `ndcart doctor` to verify its checksum.", m.model_file, human_bytes(s)),
        ),
    });

    // The server.
    v.push(match f.health {
        Health::Ready => check(Level::Ok, "local server", format!("ready on port {}", m.port())),
        Health::Loading => check(
            Level::Warn,
            "local server",
            format!("port {} is still loading the model; wait a moment and check again", m.port()),
        ),
        Health::Down => check(
            Level::Warn,
            "local server",
            format!(
                "not running on port {}. Start it with `ndcart ai serve` or the Settings panel{}.",
                m.port(),
                if m.autostart { " (it is set to start with the studio: check its log, ai-runtime.log in the data folder)" } else { "" }
            ),
        ),
    });

    // A GPU the machine has that llama.cpp cannot use.
    let expects_gpu = !f.hw.gpus.is_empty() || f.hw.apple_silicon;
    if expects_gpu && f.runtime.is_some() {
        let backends = hw::device_backends(&f.hw.runtime_devices);
        v.push(if backends.is_empty() {
            check(
                Level::Warn,
                "local GPU",
                "this computer has a GPU, but llama.cpp lists no GPU device: the model will run on the CPU, slowly. Install the CUDA, Vulkan or Metal build of llama.cpp.",
            )
        } else {
            check(Level::Ok, "local GPU", format!("llama.cpp can use: {}", backends.join(", ")))
        });
    }

    // Disk.
    if let Some(free) = f.free_bytes {
        let size = f.model_size.unwrap_or(0);
        v.push(if free < 2_000_000_000 || free < size / 10 {
            check(
                Level::Warn,
                "disk space",
                format!("only {} free in the data folder; the server log and caches need room. Free some space.", human_bytes(free)),
            )
        } else {
            check(Level::Ok, "disk space", format!("{} free", human_bytes(free)))
        });
    }

    // The model against the memory it must fit in.
    if let Some(size) = f.model_size {
        let memory = f.hw.total_vram() + f.hw.ram_bytes;
        if memory > 0 {
            v.push(if size as f64 > 0.9 * memory as f64 {
                check(
                    Level::Warn,
                    "model size",
                    format!(
                        "the model file ({}) is larger than 90% of this computer's memory ({}). It will swap or fail to load: choose a smaller model.",
                        human_bytes(size),
                        human_bytes(memory)
                    ),
                )
            } else {
                check(
                    Level::Ok,
                    "model size",
                    format!("{} against {} of memory", human_bytes(size), human_bytes(memory)),
                )
            });
        }
    }
    v
}

// ---- the checksum cache --------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
struct Seen {
    size: u64,
    mtime: u64,
    sha256: String,
}

const CACHE: &str = ".verified.json";

fn mtime(p: &Path) -> Option<u64> {
    std::fs::metadata(p)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

/// Hash only when the size or modification time changed since the last good check.
pub fn verify_file(data: &Path, rel: &str, verify: bool) -> Checksum {
    let path = models_dir(data).join(rel);
    let Some(rec) = std::fs::read_to_string(sidecar_path(&path))
        .ok()
        .and_then(|t| serde_json::from_str::<Sidecar>(&t).ok())
    else {
        return Checksum::NoRecord;
    };
    let (Ok(meta), Some(mt)) = (std::fs::metadata(&path), mtime(&path)) else {
        return Checksum::NotChecked;
    };
    let cache_path = models_dir(data).join(CACHE);
    let mut cache: BTreeMap<String, Seen> = std::fs::read_to_string(&cache_path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    if let Some(s) = cache.get(rel) {
        if s.size == meta.len() && s.mtime == mt && s.sha256 == rec.sha256 {
            return Checksum::Match { cached: true };
        }
    }
    if !verify {
        return Checksum::NotChecked;
    }
    match sha256_file(&path) {
        Ok(got) if got == rec.sha256 => {
            cache.insert(
                rel.to_string(),
                Seen {
                    size: meta.len(),
                    mtime: mt,
                    sha256: got,
                },
            );
            if let Ok(body) = serde_json::to_string(&cache) {
                let _ = std::fs::write(&cache_path, body);
            }
            Checksum::Match { cached: false }
        }
        Ok(got) => Checksum::Mismatch {
            expected: rec.sha256,
            got,
        },
        Err(_) => Checksum::NotChecked,
    }
}

// ---- gathering the facts -------------------------------------------------------------------

/// The checks for the managed model in `stored` (none when no local model is set up).
/// `verify` lets the checksum be computed (the command line); the web panel passes false.
pub async fn local_checks(
    data: Option<&Path>,
    stored: &StoredAiSettings,
    verify: bool,
) -> Vec<Check> {
    let Some(m) = stored.managed.clone() else {
        return Vec::new();
    };
    let data = data.map(Path::to_path_buf);
    let m2 = m.clone();
    let d2 = data.clone();
    let (rt, hw, size, checksum, free) = tokio::task::spawn_blocking(move || {
        let rt = runtime::find(m2.runtime_path.as_deref());
        let hw = super::http::hardware(rt.as_ref());
        let model = d2
            .as_deref()
            .and_then(|d| serve::model_path(d, &m2.model_file).ok());
        let size = model
            .as_ref()
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|m| m.len());
        let checksum = match (&d2, size) {
            (Some(d), Some(_)) if !Path::new(&m2.model_file).is_absolute() => {
                verify_file(d, &m2.model_file, verify)
            }
            (_, Some(_)) => Checksum::NoRecord,
            _ => Checksum::NotChecked,
        };
        let free = d2.as_deref().and_then(super::pull::free_bytes);
        (rt, hw, size, checksum, free)
    })
    .await
    .expect("local checks");
    let health = serve::health(m.port()).await;
    evaluate(
        &m,
        &Facts {
            runtime: rt,
            hw,
            model_size: size,
            checksum,
            health,
            free_bytes: free,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_local::hw::Gpu;
    use std::path::PathBuf;

    fn managed() -> Managed {
        Managed {
            model_file: "unsloth/X-GGUF/x.gguf".into(),
            model_id: Some("qwen3.5-4b".into()),
            port: Some(8391),
            ..Default::default()
        }
    }

    fn good() -> Facts {
        Facts {
            runtime: Some(Runtime {
                path: PathBuf::from("/usr/bin/llama-server"),
                version: Some("6789 (abc)".into()),
                source: "path",
            }),
            hw: Hw {
                os: "linux".into(),
                arch: "x86_64".into(),
                ram_bytes: 16_000_000_000,
                gpus: vec![Gpu {
                    name: "RTX".into(),
                    vram_bytes: 12_000_000_000,
                    backend: "cuda".into(),
                }],
                apple_silicon: false,
                runtime_devices: vec!["CUDA0: RTX (12000 MiB)".into()],
            },
            model_size: Some(3_000_000_000),
            checksum: Checksum::Match { cached: false },
            health: Health::Ready,
            free_bytes: Some(200_000_000_000),
        }
    }

    #[test]
    fn a_good_setup_is_all_ok() {
        let c = evaluate(&managed(), &good());
        assert!(
            c.iter().all(|c| c.level == Level::Ok),
            "{}",
            crate::doctor::render(&c)
        );
        assert_eq!(
            c.iter().map(|c| c.what).collect::<Vec<_>>(),
            [
                "local runtime",
                "local model",
                "local server",
                "local GPU",
                "disk space",
                "model size"
            ]
        );
        assert!(c[0].detail.contains("6789"));
    }

    #[test]
    fn each_problem_is_one_warning_with_advice() {
        let m = managed();
        let one = |f: Facts, what: &'static str, level: Level, needle: &str| {
            let c = evaluate(&m, &f);
            let bad: Vec<_> = c.iter().filter(|c| c.level != Level::Ok).collect();
            assert_eq!(bad.len(), 1, "{}", crate::doctor::render(&c));
            assert_eq!((bad[0].what, bad[0].level), (what, level));
            assert!(bad[0].detail.contains(needle), "{}", bad[0].detail);
        };
        let mut f = good();
        f.runtime = None;
        f.hw.runtime_devices.clear();
        // No runtime: one warning (the GPU line is left out, since nothing could be listed).
        one(f, "local runtime", Level::Warn, "brew install llama.cpp");
        let mut f = good();
        f.model_size = None;
        f.checksum = Checksum::NotChecked;
        f.hw.ram_bytes = 16_000_000_000;
        let c = evaluate(&m, &f);
        assert!(c.iter().any(|c| c.what == "local model"
            && c.level == Level::Warn
            && c.detail.contains("ndcart ai pull qwen3.5-4b")));
        let mut f = good();
        f.checksum = Checksum::Mismatch {
            expected: "a".repeat(64),
            got: "b".repeat(64),
        };
        one(f, "local model", Level::Fail, "download it again");
        let mut f = good();
        f.checksum = Checksum::NoRecord;
        one(f, "local model", Level::Warn, "no checksum record");
        let mut f = good();
        f.health = Health::Down;
        one(f, "local server", Level::Warn, "ndcart ai serve");
        let mut f = good();
        f.health = Health::Loading;
        one(f, "local server", Level::Warn, "still loading");
        let mut f = good();
        f.hw.runtime_devices.clear();
        one(f, "local GPU", Level::Warn, "CUDA, Vulkan or Metal build");
        let mut f = good();
        f.free_bytes = Some(500_000_000);
        one(f, "disk space", Level::Warn, "Free some space");
        let mut f = good();
        f.model_size = Some(30_000_000_000);
        one(f, "model size", Level::Warn, "smaller model");
    }

    #[test]
    fn a_computer_without_a_gpu_gets_no_gpu_line_and_an_unchecked_checksum_is_fine_in_the_web_panel(
    ) {
        let mut f = good();
        f.hw.gpus.clear();
        f.hw.runtime_devices.clear();
        f.checksum = Checksum::NotChecked;
        let c = evaluate(&managed(), &f);
        assert!(
            c.iter()
                .all(|c| c.level == Level::Ok && c.what != "local GPU"),
            "{}",
            crate::doctor::render(&c)
        );
        // Apple Silicon expects Metal.
        let mut f = good();
        f.hw.gpus.clear();
        f.hw.apple_silicon = true;
        f.hw.runtime_devices = vec!["Metal: Apple M2".into()];
        assert!(evaluate(&managed(), &f)
            .iter()
            .all(|c| c.level == Level::Ok));
        f.hw.runtime_devices.clear();
        assert!(evaluate(&managed(), &f)
            .iter()
            .any(|c| c.what == "local GPU" && c.level == Level::Warn));
    }

    #[test]
    fn the_checksum_is_hashed_once_then_remembered_until_the_file_changes() {
        let d = std::env::temp_dir().join(format!("ndcart-vf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let dir = d.join("models/unsloth/X-GGUF");
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("x.gguf");
        std::fs::write(&f, b"GGUF-content").unwrap();
        let rel = "unsloth/X-GGUF/x.gguf";
        assert_eq!(verify_file(&d, rel, true), Checksum::NoRecord);
        let sha = sha256_file(&f).unwrap();
        let rec = Sidecar {
            repo: "unsloth/X-GGUF".into(),
            file: "x.gguf".into(),
            sha256: sha,
            size: 12,
            fetched_at: "x".into(),
            license: String::new(),
            sha256_source: "hub".into(),
        };
        std::fs::write(sidecar_path(&f), serde_json::to_string(&rec).unwrap()).unwrap();
        // Not asked to hash and nothing remembered: not checked.
        assert_eq!(verify_file(&d, rel, false), Checksum::NotChecked);
        assert_eq!(
            verify_file(&d, rel, true),
            Checksum::Match { cached: false }
        );
        assert_eq!(
            verify_file(&d, rel, false),
            Checksum::Match { cached: true }
        );
        assert_eq!(verify_file(&d, rel, true), Checksum::Match { cached: true });
        // Damage with the same size: the modification time (or a forced re-hash) catches it.
        std::fs::write(&f, b"GGUF-contenX").unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&f)
            .unwrap()
            .set_modified(later)
            .unwrap();
        assert!(matches!(
            verify_file(&d, rel, true),
            Checksum::Mismatch { .. }
        ));
        assert_eq!(verify_file(&d, rel, false), Checksum::NotChecked);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn no_local_model_means_no_local_checks() {
        assert!(local_checks(None, &StoredAiSettings::default(), true)
            .await
            .is_empty());
    }
}
