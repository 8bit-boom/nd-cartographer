//! Project backups: a `.ndmapz` zip holding one project's own files (manifest,
//! snapshot, journal, structure, checkpoints), a `PACKS.txt` with the licence
//! of every pack the map's props come from, and, when asked, the pictures it
//! uses (`assets/<sha256>`) with the library entries that name them
//! (`custom.json`). Served by `GET /api/docs/{id}/bundle`, read back by
//! `POST /api/import/bundle` and written by `ndcart bundle`.
//!
//! Importing never trusts the archive: only the entry names below are read
//! (no `..`, no absolute paths, no extras), sizes are capped, every file is
//! parsed through the same code that loads a project from disk, every picture
//! is checked against its name and decoded, and the project gets a new id.

use crate::assets::{Assets, Custom};
use crate::store::{DocFiles, StoredDoc};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;

/// The files of a bundle before they are zipped.
pub struct BundleParts {
    pub files: Vec<(String, Vec<u8>)>,
    pub embedded: usize,
    pub packs_txt: String,
    /// Things the person making the bundle should know (imported art inside).
    pub warnings: Vec<String>,
}

/// Total uncompressed bytes an import will read, and the most entries.
const MAX_UNCOMPRESSED: u64 = 1 << 30;
const MAX_ENTRIES: usize = 20_000;

/// Gather a project's files from memory (so a bundle made while the server
/// runs is consistent with what the studio shows, whether or not a data
/// directory exists). With `embed_assets`, the pictures its custom props,
/// materials and underlays use come along.
pub fn bundle_parts(doc: &StoredDoc, embed_assets: bool) -> Result<BundleParts, String> {
    let id = &doc.manifest.id;
    let ser = |e: serde_json::Error| format!("cannot serialize the project: {e}");
    let mut files: Vec<(String, Vec<u8>)> = vec![
        (
            format!("{id}/manifest.json"),
            serde_json::to_string_pretty(&doc.manifest)
                .map_err(ser)?
                .into_bytes(),
        ),
        (
            format!("{id}/snapshot.json"),
            serde_json::to_string(
                &serde_json::json!({ "journal_len": doc.journal.len(), "floors": &doc.floors }),
            )
            .map_err(ser)?
            .into_bytes(),
        ),
        (
            format!("{id}/journal.ndjson"),
            doc.journal.to_ndjson().into_bytes(),
        ),
        (
            format!("{id}/structure.ndjson"),
            doc.structure.to_ndjson().into_bytes(),
        ),
        (
            format!("{id}/checkpoints.json"),
            serde_json::to_string(&doc.checkpoints)
                .map_err(ser)?
                .into_bytes(),
        ),
    ];

    let assets = crate::assets::global();
    let mut packs: BTreeMap<String, String> = BTreeMap::new();
    packs.insert(
        "nd-cartographer starter pack".into(),
        "MIT (built in)".into(),
    );
    let mut warnings = Vec::new();
    let mut embedded = 0usize;

    // Every floor, including the ones inside checkpoints, names the pictures it needs.
    let all_floors = doc
        .floors
        .iter()
        .chain(doc.checkpoints.iter().flat_map(|c| c.floors.iter()));
    let mut customs: BTreeMap<String, Custom> = BTreeMap::new();
    let mut raw_assets: BTreeSet<String> = BTreeSet::new();
    for floor in all_floors {
        let mut want = |id: &str, kind: &str| {
            if let Some(c) = assets.find_custom(id, kind) {
                customs.entry(c.id.clone()).or_insert(c);
            }
        };
        for s in &floor.stamps {
            if ndcart_paint::find_stamp(&s.asset).is_none() {
                want(&s.asset, "stamp");
            }
        }
        for sp in floor.grid.spaces.values() {
            if let Some(m) = &sp.material {
                want(m, "material");
            }
        }
        for st in &floor.strokes {
            if let Some(m) = &st.material {
                want(m, "material");
            }
        }
        if let Some(u) = &floor.underlay {
            raw_assets.insert(u.asset.clone());
        }
    }
    // Licence audit: the packs the custom props came from.
    for c in customs.values().filter(|c| c.kind == "stamp") {
        let pack = c.meta.pack.clone().unwrap_or_else(|| "imported art".into());
        let lic = c
            .meta
            .license
            .as_ref()
            .map(|l| {
                format!(
                    "{} (map-commercial: {:?}, redistribution: {:?}, attribution: {})",
                    if l.identifier.is_empty() {
                        "unstated"
                    } else {
                        &l.identifier
                    },
                    l.commercial_map,
                    l.redistribution,
                    l.attribution_required
                )
            })
            .unwrap_or_else(|| "no licence stated".into());
        packs.entry(pack).or_insert(lic);
    }
    if embed_assets {
        for c in customs.values() {
            raw_assets.insert(c.asset.clone());
            if c.origin.source == "imported" {
                warnings.push(format!(
                    "embedding imported art '{}' from '{}'; check its licence",
                    c.name,
                    c.meta.pack.clone().unwrap_or_else(|| "imported art".into())
                ));
            }
        }
        for a in &raw_assets {
            if let Some(bytes) = assets.get_bytes(a) {
                files.push((format!("{id}/assets/{a}"), bytes.to_vec()));
                embedded += 1;
            }
        }
        if !customs.is_empty() {
            let list: Vec<&Custom> = customs.values().collect();
            files.push((
                format!("{id}/custom.json"),
                serde_json::to_string(&list).map_err(ser)?.into_bytes(),
            ));
        }
    }
    let packs_txt: String = packs
        .iter()
        .map(|(k, v)| format!("{k}: {v}"))
        .collect::<Vec<_>>()
        .join("\n");
    files.push((format!("{id}/PACKS.txt"), packs_txt.clone().into_bytes()));
    files.sort();
    Ok(BundleParts {
        files,
        embedded,
        packs_txt,
        warnings,
    })
}

/// Zip the files (the slow part: call it off the store lock).
pub fn zip_parts(files: &[(String, Vec<u8>)]) -> Result<Vec<u8>, String> {
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, bytes) in files {
        w.start_file(name, opts).map_err(|e| format!("zip: {e}"))?;
        std::io::Write::write_all(&mut w, bytes).map_err(|e| format!("zip: {e}"))?;
    }
    let cursor = w.finish().map_err(|e| format!("zip: {e}"))?;
    Ok(cursor.into_inner())
}

/// What an import found, before anything was stored.
pub struct Imported {
    /// The project, under a new id.
    pub doc: StoredDoc,
    /// Pictures to store (checked: name = SHA-256 of the bytes, decodes as an image).
    pub assets: Vec<(String, Vec<u8>)>,
    pub custom: Vec<Custom>,
}

/// Read a bundle in memory and validate all of it. Nothing is stored here.
pub fn read_bundle(bytes: &[u8]) -> Result<Imported, String> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| format!("not a bundle (zip): {e}"))?;
    if zip.len() > MAX_ENTRIES {
        return Err(format!("too many files in the bundle (over {MAX_ENTRIES})"));
    }
    let mut top: Option<String> = None;
    let mut named: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut assets: Vec<(String, Vec<u8>)> = Vec::new();
    let mut total = 0u64;
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).map_err(|e| format!("zip: {e}"))?;
        if f.is_dir() {
            continue;
        }
        let name = f.name().to_string();
        let (dir, rest) = split_entry(&name)?;
        match &top {
            None => top = Some(dir.to_string()),
            Some(t) if t != dir => {
                return Err("a bundle holds one project in one folder".to_string())
            }
            _ => {}
        }
        let asset_name = rest.strip_prefix("assets/");
        let allowed = matches!(
            rest,
            "manifest.json"
                | "snapshot.json"
                | "journal.ndjson"
                | "structure.ndjson"
                | "checkpoints.json"
                | "PACKS.txt"
                | "custom.json"
        );
        // Pictures are named by their hash (older bundles added `.bin`).
        let asset_id = asset_name
            .map(|a| a.strip_suffix(".bin").unwrap_or(a))
            .filter(|a| ndcart_core::hash::is_asset_id(a));
        if !allowed && asset_id.is_none() {
            return Err(format!("unexpected file in the bundle: {name}"));
        }
        // Declared sizes can lie: cap what is actually read.
        let room = MAX_UNCOMPRESSED - total;
        let mut data = Vec::new();
        f.by_ref()
            .take(room + 1)
            .read_to_end(&mut data)
            .map_err(|e| format!("zip: {e}"))?;
        total += data.len() as u64;
        if total > MAX_UNCOMPRESSED {
            return Err("the bundle is too large when unpacked".to_string());
        }
        if let Some(aid) = asset_id {
            assets.push((aid.to_string(), data));
        } else if named.insert(rest.to_string(), data).is_some() {
            return Err(format!("{rest} appears twice in the bundle"));
        }
    }
    let text = |name: &str| -> Result<Option<String>, String> {
        named
            .get(name)
            .map(|b| String::from_utf8(b.clone()).map_err(|_| format!("{name} is not UTF-8")))
            .transpose()
    };
    let files = DocFiles {
        manifest: text("manifest.json")?.ok_or("the bundle has no manifest.json")?,
        snapshot: Some(text("snapshot.json")?.ok_or("the bundle has no snapshot.json")?),
        legacy_floors: None,
        journal: text("journal.ndjson")?.unwrap_or_default(),
        structure: text("structure.ndjson")?.unwrap_or_default(),
        checkpoints: text("checkpoints.json")?,
    };
    let mut doc = StoredDoc::from_files(files, "bundle").map_err(|e| e.to_string())?;
    // A new identity: never trust the id the archive carries (it could collide
    // with, and overwrite, another project's folder).
    doc.manifest.id = ndcart_core::ids::new_id("doc");

    // Pictures: the name must be the hash of the content, and it must decode.
    let mut seen = BTreeSet::new();
    assets.retain(|(id, _)| seen.insert(id.clone()));
    for (id, data) in &assets {
        if ndcart_core::hash::sha256_hex(data) != *id {
            return Err(format!("picture {id} does not match its name"));
        }
        crate::convert::decode_rgba(data).map_err(|e| format!("picture {id}: {e}"))?;
    }
    let custom: Vec<Custom> = match text("custom.json")? {
        Some(t) => serde_json::from_str(&t).map_err(|e| format!("custom.json: {e}"))?,
        None => Vec::new(),
    };
    Ok(Imported {
        doc,
        assets,
        custom,
    })
}

/// `<folder>/<file>`: one folder, no tricks. Returns (folder, file).
fn split_entry(name: &str) -> Result<(&str, &str), String> {
    let bad = || format!("unexpected file in the bundle: {name}");
    if name.is_empty()
        || name.starts_with('/')
        || name.contains('\\')
        || name.contains(':')
        || name.contains('\0')
        || name
            .split('/')
            .any(|p| p == ".." || p == "." || p.is_empty())
    {
        return Err(bad());
    }
    name.split_once('/').ok_or_else(bad)
}

/// Store an import's pictures and library entries. The pictures were
/// validated in [`read_bundle`]; a library entry whose id the store already
/// has is left as it is.
pub fn store_assets(imp: &Imported, assets: &Assets) -> Result<(), String> {
    for (id, data) in &imp.assets {
        let put = assets.put(data)?;
        debug_assert_eq!(&put, id);
    }
    for c in &imp.custom {
        if assets.find_custom(&c.id, &c.kind).is_some() {
            continue;
        }
        // An entry whose picture is not here (a bundle made without assets) cannot be used.
        if let Err(e) = assets.register(c.clone()) {
            eprintln!("ndcart: bundle library entry {} skipped: {e}", c.id);
        }
    }
    Ok(())
}

/// `ndcart bundle DOC --out FILE.ndmapz [--embed-assets] --data-dir DIR`
/// (the same bundle `GET /api/docs/{id}/bundle` serves; imported art is NOT
/// embedded unless asked, since redistribution is the pack's licence call).
pub async fn run_bundle(args: Vec<String>) -> Result<(), String> {
    let mut doc = None;
    let mut data_dir = None;
    let mut out = None;
    let mut embed = false;
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        let mut val = |n: &str| it.next().ok_or(format!("{n} needs a value"));
        match a.as_str() {
            "--data-dir" => data_dir = Some(std::path::PathBuf::from(val("--data-dir")?)),
            "--out" => out = Some(val("--out")?),
            "--embed-assets" => embed = true,
            o if !o.starts_with("--") && doc.is_none() => doc = Some(o.to_string()),
            o => return Err(format!("unknown argument: {o}")),
        }
    }
    let doc = doc.ok_or("name the document (id or exact name)")?;
    let out = out.ok_or("--out FILE.ndmapz is required")?;
    let data_dir = data_dir.ok_or("--data-dir is required")?;
    crate::assets::init(Some(data_dir.join("assets")));
    let mut st = crate::store::Store::default();
    st.load_dir(&data_dir).map_err(|e| e.to_string())?;
    let d = st
        .docs
        .values()
        .find(|d| d.manifest.id == doc || d.manifest.name == doc)
        .ok_or(format!("no document {doc:?} in {}", data_dir.display()))?;
    let parts = bundle_parts(d, embed)?;
    for w in &parts.warnings {
        println!("warning: {w}");
    }
    let bytes = zip_parts(&parts.files)?;
    std::fs::write(&out, &bytes).map_err(|e| format!("cannot create {out}: {e}"))?;
    println!(
        "bundle written: {out} ({} file(s){})",
        parts.files.len(),
        if embed {
            format!(", {} asset(s) embedded", parts.embedded)
        } else {
            String::new()
        }
    );
    println!("packs referenced:\n{}", parts.packs_txt);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::Provenance;
    use ndcart_core::DocKind;

    #[test]
    fn a_bundle_carries_the_pictures_and_library_entries_a_fresh_server_needs() {
        let a = crate::assets::global();
        let mut pm = tiny_skia::Pixmap::new(4, 4).unwrap();
        pm.fill(tiny_skia::Color::from_rgba8(10, 200, 30, 255));
        let picture = a.put(&pm.encode_png().unwrap()).unwrap();
        a.register(Custom {
            meta: Default::default(),
            id: "stp_bundleunit1".into(),
            name: "Green".into(),
            kind: "stamp".into(),
            asset: picture.clone(),
            size: 2.0,
            origin: Provenance {
                source: "imported".into(),
                prompt: String::new(),
                seed: 0,
                date: "2026-11-18".into(),
            },
            tags: Vec::new(),
        })
        .unwrap();
        let mut doc = StoredDoc::new("Unit", DocKind::Battle);
        let fid = doc.floors[0].id.clone();
        let stamp = ndcart_core::Stamp {
            id: "s1".into(),
            asset: "stp_bundleunit1".into(),
            x: 1.0,
            y: 1.0,
            scale: 1.0,
            rotation: 0.0,
            flip: false,
            tint: None,
            shadow: true,
            gen: None,
        };
        doc.apply_ops(&fid, &[ndcart_core::Op::AddStamp { stamp, index: None }])
            .unwrap();
        let parts = bundle_parts(&doc, true).unwrap();
        assert_eq!(parts.embedded, 1);
        assert!(parts.warnings.iter().any(|w| w.contains("imported art")));
        let zip = zip_parts(&parts.files).unwrap();
        let imp = read_bundle(&zip).unwrap();
        assert_ne!(imp.doc.manifest.id, doc.manifest.id);
        assert_eq!(imp.doc.floors, doc.floors);
        assert_eq!(imp.assets.len(), 1);
        assert_eq!(imp.assets[0].0, picture);
        assert_eq!(imp.custom.len(), 1);
        assert_eq!(imp.custom[0].id, "stp_bundleunit1");
        // Into a store that has neither: the picture and entry arrive.
        let fresh = crate::assets::new_for_tests();
        store_assets(&imp, &fresh).unwrap();
        assert!(fresh.find_custom("stp_bundleunit1", "stamp").is_some());
        assert!(fresh.get_bytes(&picture).is_some());
        // A bundle made without assets has none, and still imports.
        let bare =
            read_bundle(&zip_parts(&bundle_parts(&doc, false).unwrap().files).unwrap()).unwrap();
        assert!(bare.assets.is_empty() && bare.custom.is_empty());
    }
}
