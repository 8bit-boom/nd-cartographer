//! Local AI automation: `ndcart ai models | pull | import | serve | stop | setup | status |
//! test | key`. The runtime is upstream llama.cpp's `llama-server`, which the studio finds but
//! never downloads or installs; the studio downloads only model files (GGUF), after the person
//! confirms the size (docs/LOCAL_AI.md). Everything here is optional: the studio runs fully
//! without it.
//!
//! Layout: `catalog` (the model list), then the pieces each command uses.

// Pieces land one command at a time; the last step removes this allowance.
#![allow(dead_code)]

pub mod catalog;
pub mod hw;
pub mod pull;
pub mod runtime;
pub mod serve;

use std::path::PathBuf;

/// The data folder for the `ai` commands: `--data-dir`, else `ND_CARTO_DATA_DIR`, else `./data`
/// (the same folder `ndcart serve --data-dir` would use).
pub fn data_dir_from(flag: Option<String>) -> PathBuf {
    flag.or_else(|| {
        std::env::var("ND_CARTO_DATA_DIR")
            .ok()
            .filter(|v| !v.is_empty())
    })
    .map(PathBuf::from)
    .unwrap_or_else(|| PathBuf::from("data"))
}

/// Seconds since the epoch.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `YYYY-MM-DD` (UTC) for a Unix time.
pub fn iso_date(secs: u64) -> String {
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{:02}-{:02}", if m <= 2 { y + 1 } else { y }, m, d)
}

/// `1.5 GB` style sizes (decimal, as disks and the Hub count them).
pub fn human_bytes(b: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = b as f64;
    let mut u = 0;
    while v >= 1000.0 && u + 1 < UNITS.len() {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 {
        format!("{b} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

/// Split `--data-dir D` and `--json` out of a command's arguments.
pub struct Common {
    pub data: PathBuf,
    pub json: bool,
    pub rest: Vec<String>,
}

pub fn common(args: Vec<String>) -> Result<Common, String> {
    let (mut data, mut json, mut rest) = (None, false, Vec::new());
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--data-dir" => data = Some(it.next().ok_or("--data-dir needs a folder")?),
            "--json" => json = true,
            _ => rest.push(a),
        }
    }
    Ok(Common {
        data: data_dir_from(data),
        json,
        rest,
    })
}

/// `ndcart ai models [--json]`: the catalog, with what this machine has tested.
pub fn models(args: Vec<String>) -> Result<(), String> {
    let c = common(args)?;
    if let Some(o) = c.rest.first() {
        return Err(format!("unknown argument: {o}"));
    }
    let cat = catalog::bundled();
    let records = catalog::load_records(&c.data);
    let rt = runtime::find(None);
    let machine = hw::detect(rt.as_ref().map(|r| r.path.as_path()));
    let recs = hw::recommend(&machine, &cat);
    let mark = |id: &str| {
        recs.iter()
            .find(|r| r.entry.id == id)
            .map(|r| match (r.recommended, r.fit) {
                (true, _) => "recommended for this computer",
                (_, hw::Fit::Full) => "fits",
                (_, hw::Fit::Partial) => "fits with part on the CPU (slower)",
                _ => "too big for this computer",
            })
            .unwrap_or("")
    };
    if c.json {
        let rows: Vec<_> = cat
            .iter()
            .map(|m| {
                serde_json::json!({
                    "id": m.id, "label": m.label, "tier": m.tier, "hf_repo": m.hf_repo,
                    "file": m.file, "quant": m.quant, "approx_bytes": m.approx_bytes,
                    "ctx_default": m.ctx_default, "license": m.license,
                    "fit": mark(&m.id),
                    "verified": records.contains_key(&m.id),
                    "tested": records.get(&m.id),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&rows).map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    println!("This computer: {}\n", machine.summary());
    for m in &cat {
        println!(
            "{:<18} {:<8} ~{:<8} {}\n{:<18} {}/{}  [{}] [{}]",
            m.id,
            m.tier,
            human_bytes(m.approx_bytes),
            m.label,
            "",
            m.hf_repo,
            m.file,
            mark(&m.id),
            catalog::status_label(records.get(&m.id)),
        );
    }
    println!(
        "\nSizes are approximate; `ndcart ai pull ID` reads the real size and checksum from the \
         Hub and asks before downloading. A model counts as tested only after `ndcart ai test`."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_and_sizes_read_well() {
        assert_eq!(iso_date(0), "1970-01-01");
        assert_eq!(iso_date(1_795_000_000), "2026-11-18");
        assert_eq!(human_bytes(999), "999 B");
        assert_eq!(human_bytes(2_800_000_000), "2.8 GB");
        assert_eq!(human_bytes(1_500_000), "1.5 MB");
    }

    #[test]
    fn common_flags_are_split_out() {
        let c = common(vec![
            "--json".into(),
            "x".into(),
            "--data-dir".into(),
            "/d".into(),
        ])
        .unwrap();
        assert!(c.json);
        assert_eq!(c.data, PathBuf::from("/d"));
        assert_eq!(c.rest, vec!["x".to_string()]);
        assert!(common(vec!["--data-dir".into()]).is_err());
    }
}
