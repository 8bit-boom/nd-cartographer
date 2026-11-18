//! Hardware detection for picking a model. The parsers are pure (tested on captured sample
//! outputs); the runners call fixed programs with fixed arguments, never a shell, and give up
//! after a short timeout. Nothing here needs a GPU or a runtime to compile or test.

use super::catalog::{tier_budget, Entry};
use serde::Serialize;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const MIB: u64 = 1024 * 1024;

#[derive(Serialize, Clone, Debug, PartialEq, Default)]
pub struct Gpu {
    pub name: String,
    pub vram_bytes: u64,
    /// `cuda`, `rocm` or `metal` (how it was found; the runtime's own device list is the authority).
    pub backend: String,
}

#[derive(Serialize, Clone, Debug, PartialEq, Default)]
pub struct Hw {
    pub os: String,
    pub arch: String,
    pub ram_bytes: u64,
    pub gpus: Vec<Gpu>,
    pub apple_silicon: bool,
    /// What `llama-server --list-devices` printed (one line per device); empty when there is
    /// no runtime or it lists none.
    pub runtime_devices: Vec<String>,
}

// ---- parsers -------------------------------------------------------------------------------

/// `nvidia-smi --query-gpu=name,memory.total --format=csv,noheader,nounits` (MiB).
pub fn parse_nvidia_smi(out: &str) -> Vec<Gpu> {
    out.lines()
        .filter_map(|l| {
            let (name, mem) = l.rsplit_once(',')?;
            let mib: u64 = mem.trim().parse().ok()?;
            let name = name.trim();
            (!name.is_empty()).then(|| Gpu {
                name: name.to_string(),
                vram_bytes: mib * MIB,
                backend: "cuda".into(),
            })
        })
        .collect()
}

/// `rocm-smi --showmeminfo vram --csv`: a header with a "Total Memory" column (bytes), then
/// one row per card. Best effort: a different layout gives no GPUs.
pub fn parse_rocm_smi(out: &str) -> Vec<Gpu> {
    let mut lines = out.lines().map(str::trim).filter(|l| !l.is_empty());
    let Some(header) = lines.find(|l| l.to_ascii_lowercase().contains("total memory")) else {
        return Vec::new();
    };
    let col = header.split(',').position(|c| {
        c.to_ascii_lowercase().contains("total memory") && !c.to_ascii_lowercase().contains("used")
    });
    let Some(col) = col else { return Vec::new() };
    lines
        .filter_map(|l| {
            let cells: Vec<&str> = l.split(',').map(str::trim).collect();
            let bytes: u64 = cells.get(col)?.parse().ok()?;
            let name = cells.first().copied().unwrap_or("gpu");
            (bytes > 0).then(|| Gpu {
                name: format!("AMD {name}"),
                vram_bytes: bytes,
                backend: "rocm".into(),
            })
        })
        .collect()
}

/// `/proc/meminfo`: `MemTotal:   16318740 kB`.
pub fn parse_meminfo(text: &str) -> Option<u64> {
    let line = text.lines().find(|l| l.starts_with("MemTotal:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb * 1024)
}

/// A bare byte count (`sysctl -n hw.memsize`, PowerShell's TotalPhysicalMemory).
pub fn parse_bytes(text: &str) -> Option<u64> {
    text.trim().parse().ok().filter(|b| *b > 0)
}

/// `llama-server --list-devices`: lines such as `  CUDA0: NVIDIA GeForce RTX 4090 (24564 MiB, 24000 MiB free)`
/// under an `Available devices:` heading. Best effort across versions: any indented
/// `Name: description` line counts.
pub fn parse_list_devices(out: &str) -> Vec<String> {
    out.lines()
        .filter(|l| l.starts_with(char::is_whitespace) && l.contains(':'))
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// The backends a device list names (`CUDA0:` gives `cuda`).
pub fn device_backends(devices: &[String]) -> Vec<String> {
    let mut v: Vec<String> = devices
        .iter()
        .filter_map(|d| d.split(':').next())
        .map(|n| {
            n.trim_end_matches(|c: char| c.is_ascii_digit())
                .to_ascii_lowercase()
        })
        .filter(|n| !n.is_empty() && n != "cpu")
        .collect();
    v.sort();
    v.dedup();
    v
}

// ---- runners -------------------------------------------------------------------------------

/// Run a fixed program with fixed arguments (no shell) and return its stdout when it exits
/// successfully within `timeout`.
pub fn run_capture(program: &Path, args: &[&str], timeout: Duration) -> Option<String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let start = Instant::now();
    // Read both pipes on threads so a chatty child cannot block on a full pipe.
    let mut out = child.stdout.take()?;
    let mut err = child.stderr.take()?;
    let t_out = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut out, &mut s);
        s
    });
    let t_err = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut err, &mut s);
        s
    });
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if start.elapsed() < timeout => std::thread::sleep(Duration::from_millis(15)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let stdout = t_out.join().unwrap_or_default();
    let stderr = t_err.join().unwrap_or_default();
    let status = status?;
    // Some builds print the device list on stderr.
    status.success().then(|| {
        if stdout.trim().is_empty() {
            stderr
        } else {
            stdout
        }
    })
}

const T: Duration = Duration::from_secs(4);

fn ram_bytes() -> u64 {
    if cfg!(target_os = "linux") {
        if let Some(b) = std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|t| parse_meminfo(&t))
        {
            return b;
        }
    }
    if cfg!(target_os = "macos") {
        if let Some(b) =
            run_capture(Path::new("sysctl"), &["-n", "hw.memsize"], T).and_then(|t| parse_bytes(&t))
        {
            return b;
        }
    }
    if cfg!(windows) {
        if let Some(b) = run_capture(
            Path::new("powershell"),
            &[
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "(Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory",
            ],
            Duration::from_secs(10),
        )
        .and_then(|t| parse_bytes(&t))
        {
            return b;
        }
    }
    0
}

/// Look at this machine. `runtime` is the `llama-server` path when one was found (its
/// `--list-devices` is the authority on what llama.cpp can actually use).
pub fn detect(runtime: Option<&Path>) -> Hw {
    let os = std::env::consts::OS.to_string();
    let arch = std::env::consts::ARCH.to_string();
    let apple_silicon = os == "macos" && arch == "aarch64";
    let mut gpus = run_capture(
        Path::new("nvidia-smi"),
        &[
            "--query-gpu=name,memory.total",
            "--format=csv,noheader,nounits",
        ],
        T,
    )
    .map(|t| parse_nvidia_smi(&t))
    .unwrap_or_default();
    if gpus.is_empty() {
        gpus = run_capture(
            Path::new("rocm-smi"),
            &["--showmeminfo", "vram", "--csv"],
            T,
        )
        .map(|t| parse_rocm_smi(&t))
        .unwrap_or_default();
    }
    let runtime_devices = runtime
        .and_then(|p| run_capture(p, &["--list-devices"], Duration::from_secs(8)))
        .map(|t| parse_list_devices(&t))
        .unwrap_or_default();
    Hw {
        os,
        arch,
        ram_bytes: ram_bytes(),
        gpus,
        apple_silicon,
        runtime_devices,
    }
}

// ---- recommendation ------------------------------------------------------------------------

/// How much of the machine a model may use. Apple Silicon shares unified memory with the OS,
/// so about 70% is usable (a heuristic, not a measurement).
pub const APPLE_USABLE: f64 = 0.70;
/// Without a GPU, leave room for the OS and the studio.
pub const CPU_USABLE: f64 = 0.60;

impl Hw {
    pub fn total_vram(&self) -> u64 {
        self.gpus.iter().map(|g| g.vram_bytes).sum()
    }

    /// Memory the model's weights and context may fill.
    pub fn usable_bytes(&self) -> u64 {
        if self.apple_silicon {
            (self.ram_bytes as f64 * APPLE_USABLE) as u64
        } else if !self.gpus.is_empty() {
            self.total_vram()
        } else {
            (self.ram_bytes as f64 * CPU_USABLE) as u64
        }
    }

    /// The catalog tier this machine belongs to (the biggest class it reaches).
    pub fn tier(&self) -> &'static str {
        const GB: u64 = 1_000_000_000;
        if self.apple_silicon {
            return if self.ram_bytes >= 30 * GB {
                "apple32"
            } else if self.ram_bytes >= 15 * GB {
                "apple16"
            } else {
                "cpu8"
            };
        }
        // 24 GB cards report about 24.5 GB, 12 GB cards about 12.9 GB.
        match self.gpus.iter().map(|g| g.vram_bytes).max().unwrap_or(0) {
            v if v >= 22 * GB => "vram24",
            v if v >= 11 * GB => "vram12",
            _ => "cpu8",
        }
    }

    /// One line for people.
    pub fn summary(&self) -> String {
        let gb = |b: u64| format!("{:.0} GB", b as f64 / 1e9);
        let gpu = if self.gpus.is_empty() {
            if self.apple_silicon {
                "Apple Silicon (unified memory)".to_string()
            } else {
                "no GPU found".to_string()
            }
        } else {
            self.gpus
                .iter()
                .map(|g| format!("{} {}", g.name, gb(g.vram_bytes)))
                .collect::<Vec<_>>()
                .join(", ")
        };
        format!(
            "{} {}, {} RAM, {}",
            self.os,
            self.arch,
            gb(self.ram_bytes),
            gpu
        )
    }
}

fn tier_rank(t: &str) -> u8 {
    match t {
        "vram12" | "apple16" => 1,
        "vram24" | "apple32" => 2,
        _ => 0,
    }
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Fit {
    /// The weights and the context fit in the machine's fast memory.
    Full,
    /// It fits only with part of it running on the CPU (slower).
    Partial,
    No,
}

#[derive(Serialize, Clone, Debug)]
pub struct Rec<'a> {
    pub entry: &'a Entry,
    pub fit: Fit,
    pub recommended: bool,
    /// What the rule estimated the model needs.
    pub need_bytes: u64,
}

/// Memory a model needs: file x 1.2 plus the context allowance (both heuristics).
pub fn need_bytes(e: &Entry) -> u64 {
    (e.approx_bytes as f64 * 1.2) as u64 + e.kv_kib_per_token * 1024 * e.ctx_default as u64
}

/// Rank the catalog for this machine. The first entries are the recommended ones (up to
/// three, from the best tier the machine reaches that has a fitting model); then other
/// full fits, partial fits, and what does not fit.
pub fn recommend<'a>(hw: &Hw, cat: &'a [Entry]) -> Vec<Rec<'a>> {
    let usable = hw.usable_bytes();
    let spill = if hw.gpus.is_empty() || hw.apple_silicon {
        0
    } else {
        (hw.ram_bytes as f64 * CPU_USABLE * 0.5) as u64
    };
    let cap = tier_rank(hw.tier());
    let mut recs: Vec<Rec> = cat
        .iter()
        .map(|e| {
            let need = need_bytes(e);
            let fit = if need <= usable {
                Fit::Full
            } else if need <= usable + spill {
                Fit::Partial
            } else {
                Fit::No
            };
            Rec {
                entry: e,
                fit,
                recommended: false,
                need_bytes: need,
            }
        })
        .collect();
    // Stable order: full fits first by tier (best first), then partial, then the rest.
    let key = |r: &Rec| {
        let f = match r.fit {
            Fit::Full => 0,
            Fit::Partial => 1,
            Fit::No => 2,
        };
        (f, std::cmp::Reverse(tier_rank(&r.entry.tier)))
    };
    recs.sort_by_key(key);
    let best = recs
        .iter()
        .filter(|r| r.fit == Fit::Full && tier_rank(&r.entry.tier) <= cap)
        .map(|r| tier_rank(&r.entry.tier))
        .max();
    if let Some(best) = best {
        let mut n = 0;
        for r in recs.iter_mut() {
            if n < 3 && r.fit == Fit::Full && tier_rank(&r.entry.tier) == best {
                r.recommended = true;
                n += 1;
            }
        }
        // Recommended first, even when a bigger full fit would sort above it on a CPU box.
        recs.sort_by_key(|r| (!r.recommended, key(r)));
    }
    recs
}

/// [`recommend`], taking out models whose test here said "too weak" (a tested failure is
/// never recommended).
pub fn recommend_tested<'a>(
    hw: &Hw,
    cat: &'a [Entry],
    tested: &std::collections::BTreeMap<String, super::catalog::Record>,
) -> Vec<Rec<'a>> {
    let mut recs = recommend(hw, cat);
    for r in recs.iter_mut() {
        if tested
            .get(&r.entry.id)
            .is_some_and(|t| t.verdict == "too weak")
        {
            r.recommended = false;
        }
    }
    recs
}

/// The memory class a tier name stands for, for messages ("fits a 12 GB card").
pub fn tier_gb(tier: &str) -> u64 {
    tier_budget(tier) / 1_000_000_000
}

#[cfg(test)]
mod tests {
    use super::super::catalog;
    use super::*;

    #[test]
    fn nvidia_smi_one_two_none_and_garbage() {
        let one = "NVIDIA GeForce RTX 4090, 24564\n";
        assert_eq!(
            parse_nvidia_smi(one),
            vec![Gpu {
                name: "NVIDIA GeForce RTX 4090".into(),
                vram_bytes: 24564 * MIB,
                backend: "cuda".into()
            }]
        );
        let two = "NVIDIA A100-SXM4-40GB, 40960\nTesla T4, 15360\n";
        let g = parse_nvidia_smi(two);
        assert_eq!(g.len(), 2);
        assert_eq!(g[1].vram_bytes, 15360 * MIB);
        assert!(parse_nvidia_smi("").is_empty());
        assert!(
            parse_nvidia_smi("NVIDIA-SMI has failed because it couldn't communicate").is_empty()
        );
        assert!(parse_nvidia_smi("garbage, lots, of, commas").is_empty());
        // A name that itself contains a comma keeps it.
        assert_eq!(parse_nvidia_smi("Foo, Bar, 8192")[0].name, "Foo, Bar");
    }

    #[test]
    fn rocm_smi_csv_is_read_best_effort() {
        let out =
            "device,VRAM Total Memory (B),VRAM Total Used Memory (B)\ncard0,17163091968,10526720\n";
        let g = parse_rocm_smi(out);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].vram_bytes, 17_163_091_968);
        assert_eq!(g[0].backend, "rocm");
        assert!(parse_rocm_smi("no such thing").is_empty());
    }

    #[test]
    fn ram_from_meminfo_sysctl_and_powershell() {
        let m = "MemTotal:       16318740 kB\nMemFree:  100 kB\n";
        assert_eq!(parse_meminfo(m), Some(16318740 * 1024));
        assert_eq!(parse_meminfo("nothing"), None);
        assert_eq!(parse_bytes("17179869184\n"), Some(17_179_869_184));
        assert_eq!(parse_bytes("Get-CimInstance : denied"), None);
        assert_eq!(parse_bytes("0"), None);
    }

    #[test]
    fn list_devices_sample_is_parsed() {
        // The README format: a heading, then indented `Name: description (memory)` lines.
        let out = "Available devices:\n  CUDA0: NVIDIA GeForce RTX 4090 (24564 MiB, 24000 MiB free)\n  Vulkan0: AMD Radeon (8192 MiB, 8000 MiB free)\n";
        let d = parse_list_devices(out);
        assert_eq!(d.len(), 2);
        assert!(d[0].starts_with("CUDA0:"));
        assert_eq!(
            device_backends(&d),
            vec!["cuda".to_string(), "vulkan".to_string()]
        );
        assert!(parse_list_devices("").is_empty());
        assert!(parse_list_devices("error: unknown argument").is_empty());
    }

    fn hw(ram_gb: u64, vram_gb: &[u64], apple: bool) -> Hw {
        Hw {
            os: if apple { "macos" } else { "linux" }.into(),
            arch: if apple { "aarch64" } else { "x86_64" }.into(),
            ram_bytes: ram_gb * 1_000_000_000,
            gpus: vram_gb
                .iter()
                .map(|v| Gpu {
                    name: "gpu".into(),
                    vram_bytes: v * 1_000_000_000,
                    backend: "cuda".into(),
                })
                .collect(),
            apple_silicon: apple,
            runtime_devices: vec![],
        }
    }

    fn first_ids(h: &Hw) -> Vec<String> {
        let cat = catalog::bundled();
        recommend(h, &cat)
            .iter()
            .filter(|r| r.recommended)
            .map(|r| r.entry.id.clone())
            .collect()
    }

    #[test]
    fn eight_gb_without_a_gpu_gets_the_cpu_tier() {
        let h = hw(8, &[], false);
        assert_eq!(h.tier(), "cpu8");
        let cat = catalog::bundled();
        let r = recommend(&h, &cat);
        assert_eq!(r[0].entry.id, "qwen3.5-4b");
        assert!(r[0].recommended);
        assert!(r
            .iter()
            .filter(|x| x.recommended)
            .all(|x| x.entry.tier == "cpu8"));
        let big = r.iter().find(|x| x.entry.id == "qwen3.8-27b").unwrap();
        assert_eq!(big.fit, Fit::No);
    }

    #[test]
    fn a_24_gb_card_gets_the_vram24_tier_first() {
        let h = hw(32, &[24], false);
        assert_eq!(h.tier(), "vram24");
        let ids = first_ids(&h);
        assert!(!ids.is_empty());
        let cat = catalog::bundled();
        for id in &ids {
            assert_eq!(catalog::find(&cat, id).unwrap().tier, "vram24");
        }
        // The mixture-of-experts file does not fit 24 GB by the rule, but runs with spill.
        let r = recommend(&h, &cat);
        let moe = r.iter().find(|x| x.entry.id == "qwen3.6-35b-a3b").unwrap();
        assert_eq!(moe.fit, Fit::Partial);
        assert!(!moe.recommended);
    }

    #[test]
    fn mid_machines_land_in_the_middle() {
        assert_eq!(first_ids(&hw(32, &[12], false))[0], "qwen3.5-9b");
        // A 16 GB Mac: unified memory, about 70% usable.
        let mac = hw(16, &[], true);
        assert_eq!(mac.tier(), "apple16");
        assert_eq!(first_ids(&mac)[0], "qwen3.5-9b");
        // A 64 GB CPU-only box can hold big models but is not told to run them.
        let big_cpu = hw(64, &[], false);
        assert!(first_ids(&big_cpu)
            .iter()
            .all(|i| i.starts_with("qwen3.5-") || i.starts_with("gemma-4-e")));
        // No information at all: nothing is recommended, nothing panics.
        assert!(recommend(&Hw::default(), &catalog::bundled())
            .iter()
            .all(|r| !r.recommended));
    }

    #[test]
    fn summary_is_readable() {
        assert!(hw(8, &[], false).summary().contains("no GPU found"));
        assert!(hw(16, &[], true).summary().contains("Apple Silicon"));
        assert!(hw(32, &[24], false).summary().contains("24 GB"));
    }
}
