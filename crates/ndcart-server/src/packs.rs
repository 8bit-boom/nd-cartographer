//! Art-pack import (plan §7.4): bring a friend's Dungeondraft pack or a plain
//! folder of pictures into the palette. A `.dungeondraft_pack` is a Godot 3
//! `.pck` archive, read here with every length and offset checked; a folder is
//! scanned the same way. Pictures under `objects`/`symbols` become props,
//! pictures under `terrain`/`textures`/`tiles` become materials. Nothing is
//! executed and nothing leaves the asset store; each item keeps where it came
//! from ("from pack X by Y") so the license trail stays attached.

use crate::assets::{Assets, Custom, Provenance};
use std::path::Path;

/// Pixels per grid square that Dungeondraft assumes when sizing art.
const PX_PER_SQUARE: f32 = 256.0;
pub const MAX_ITEMS: usize = 400;
pub const MAX_PACK_BYTES: usize = 200 * 1024 * 1024;

/// A file inside a pack: its path and bytes.
#[derive(Debug)]
pub struct Entry {
    pub path: String,
    pub bytes: Vec<u8>,
}

fn u32le(b: &[u8], at: usize) -> Result<u32, String> {
    b.get(at..at + 4)
        .map(|s| u32::from_le_bytes(s.try_into().expect("4")))
        .ok_or_else(|| "the pack is cut short".to_string())
}
fn u64le(b: &[u8], at: usize) -> Result<u64, String> {
    b.get(at..at + 8)
        .map(|s| u64::from_le_bytes(s.try_into().expect("8")))
        .ok_or_else(|| "the pack is cut short".to_string())
}

/// Read a Godot 3 (version 1) `.pck` archive.
pub fn read_pck(b: &[u8]) -> Result<Vec<Entry>, String> {
    if b.len() < 4 || &b[..4] != b"GDPC" {
        return Err("that is not a Dungeondraft pack (no GDPC header)".into());
    }
    let version = u32le(b, 4)?;
    if version != 1 {
        return Err(format!("this pack uses archive format {version}, which is not supported yet; unpack it to a folder and import the folder"));
    }
    // magic, version, 3 engine version numbers, 16 reserved words, then the file count.
    let mut at = 4 + 4 + 12 + 64;
    let count = u32le(b, at)? as usize;
    at += 4;
    if count > 100_000 {
        return Err("the pack lists an impossible number of files".into());
    }
    let mut out = Vec::new();
    for _ in 0..count {
        let plen = u32le(b, at)? as usize;
        at += 4;
        if plen > 4096 {
            return Err("a file name in the pack is impossibly long".into());
        }
        let raw = b.get(at..at + plen).ok_or("the pack is cut short")?;
        at += plen;
        let path = String::from_utf8_lossy(raw)
            .trim_end_matches('\0')
            .trim_start_matches("res://")
            .to_string();
        let (off, size) = (u64le(b, at)? as usize, u64le(b, at + 8)? as usize);
        at += 8 + 8 + 16; // offset, size, md5
        let end = off
            .checked_add(size)
            .filter(|e| *e <= b.len())
            .ok_or_else(|| format!("{path}: its data lies outside the pack"))?;
        out.push(Entry {
            path,
            bytes: b[off..end].to_vec(),
        });
    }
    Ok(out)
}

/// Read every file under a folder (not following links), with paths relative to it.
pub fn read_dir(dir: &Path) -> Result<Vec<Entry>, String> {
    fn walk(
        base: &Path,
        dir: &Path,
        out: &mut Vec<Entry>,
        total: &mut usize,
    ) -> Result<(), String> {
        for e in std::fs::read_dir(dir)
            .map_err(|e| format!("cannot read {}: {e}", dir.display()))?
            .flatten()
        {
            let ft = e.file_type().map_err(|e| e.to_string())?;
            let path = e.path();
            if ft.is_dir() {
                walk(base, &path, out, total)?;
            } else if ft.is_file() {
                let rel = path
                    .strip_prefix(base)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                let ext = path
                    .extension()
                    .and_then(|x| x.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                // Pictures become assets; json is pack metadata. The v2
                // manifest (ndpack.toml) and licence files ride too - import()
                // decides what each one is; a stray notes.txt stays out.
                let base = path
                    .file_name()
                    .and_then(|x| x.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                let meta_file = ext == "toml" && base == "ndpack.toml"
                    || (ext == "txt"
                        && (base == "license.txt"
                            || base == "licence.txt"
                            || base == "license"
                            || base == "licence"));
                if !["png", "webp", "jpg", "jpeg", "json"].contains(&ext.as_str()) && !meta_file {
                    continue;
                }
                let bytes = std::fs::read(&path).map_err(|e| format!("cannot read {rel}: {e}"))?;
                *total += bytes.len();
                if *total > MAX_PACK_BYTES {
                    return Err("that folder is over 200 MB of pictures".into());
                }
                out.push(Entry { path: rel, bytes });
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    let mut total = 0;
    walk(dir, dir, &mut out, &mut total)?;
    Ok(out)
}

/// Write a Godot 3 (version 1) `.pck` archive holding `files`, the layout
/// Dungeondraft reads: a header, an index (path, offset, size, md5), the data.
/// The md5 field is left zero; readers that do not verify it (this one) accept it.
pub fn write_pck(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut head = Vec::new();
    head.extend_from_slice(b"GDPC");
    head.extend_from_slice(&1u32.to_le_bytes());
    for v in [3u32, 5, 0] {
        head.extend_from_slice(&v.to_le_bytes());
    }
    head.extend_from_slice(&[0u8; 64]);
    head.extend_from_slice(&(files.len() as u32).to_le_bytes());
    let paths: Vec<Vec<u8>> = files
        .iter()
        .map(|(p, _)| {
            let mut b = format!("res://{p}").into_bytes();
            b.push(0);
            while b.len() % 4 != 0 {
                b.push(0);
            }
            b
        })
        .collect();
    let index_len: usize = paths.iter().map(|p| 4 + p.len() + 8 + 8 + 16).sum();
    let mut off = head.len() + index_len;
    let mut index = Vec::new();
    for (i, (_, data)) in files.iter().enumerate() {
        index.extend_from_slice(&(paths[i].len() as u32).to_le_bytes());
        index.extend_from_slice(&paths[i]);
        index.extend_from_slice(&(off as u64).to_le_bytes());
        index.extend_from_slice(&(data.len() as u64).to_le_bytes());
        index.extend_from_slice(&[0u8; 16]);
        off += data.len();
    }
    let mut out = head;
    out.extend(index);
    for (_, d) in files {
        out.extend_from_slice(d);
    }
    out
}

/// A pack of the GM's own art: custom props and materials, as a Dungeondraft-style
/// archive. Items that came from someone else's pack are left out unless
/// `include_imported`, so exporting never redistributes art by accident.
pub fn export_own(
    assets: &Assets,
    name: &str,
    include_imported: bool,
) -> Result<(Vec<u8>, usize, usize), String> {
    let mine = |c: &Custom| include_imported || !c.origin.prompt.starts_with("from pack ");
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let (mut stamps, mut mats) = (0, 0);
    let safe = |n: &str| -> String {
        let t: String = n
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect();
        t.trim_matches('_').chars().take(40).collect()
    };
    let mut used = std::collections::BTreeSet::new();
    let mut manifest: NdPackOut = NdPackOut::default();
    for (kind, dir) in [("stamp", "objects"), ("material", "terrain")] {
        for c in assets.customs(kind).into_iter().filter(|c| mine(c)) {
            let Some(bytes) = assets.get_bytes(&c.asset) else {
                continue;
            };
            // The file's own format is kept; only PNG and WebP packs are written.
            let png = bytes.starts_with(&[0x89, b'P', b'N', b'G']);
            let ext = if png { "png" } else { "webp" };
            let mut base = safe(&c.name);
            if base.is_empty() {
                base = c.id.clone();
            }
            let mut path = format!("textures/{dir}/{base}.{ext}");
            let mut n = 2;
            while !used.insert(path.clone()) {
                path = format!("textures/{dir}/{base}_{n}.{ext}");
                n += 1;
            }
            if !c.meta.is_default() {
                manifest.assets.insert(
                    path.clone(),
                    PackEntryOut {
                        footprint: c.meta.footprint.as_ref().map(|f| f.0.clone()),
                        clearance: (c.meta.clearance != 0.0).then_some(c.meta.clearance),
                        placement: (c.meta.placement
                            != ndcart_core::assetmeta::Placement::default())
                        .then_some(c.meta.placement.clone()),
                        style_tags: (!c.meta.style_tags.is_empty())
                            .then_some(c.meta.style_tags.clone()),
                        license: c.meta.license.clone(),
                    },
                );
            }
            files.push((path, bytes.to_vec()));
            if kind == "stamp" {
                stamps += 1;
            } else {
                mats += 1;
            }
        }
    }
    if files.is_empty() {
        return Err(
            "there is nothing of your own to export yet (make a prop or a material first)".into(),
        );
    }
    let pack_name: String = name.trim().chars().take(60).collect();
    let pack_name = if pack_name.is_empty() {
        "ndcart pack".to_string()
    } else {
        pack_name
    };
    let info = serde_json::json!({
        "name": pack_name, "id": safe(&pack_name), "version": "1", "author": "",
        "custom_color_overrides": {"enabled": false},
    });
    files.insert(0, ("pack.json".into(), info.to_string().into_bytes()));
    // The v2 manifest: footprints, clearance, placement, licences - what
    // another nd-cartographer reads back automatically (plan 22.1).
    if !manifest.assets.is_empty() || manifest.license.is_some() {
        match toml::to_string_pretty(&manifest) {
            Ok(body) => files.insert(1, ("ndpack.toml".into(), body.into_bytes())),
            Err(e) => eprintln!("ndpack.toml could not be written (skipped): {e}"),
        }
    }
    let refs: Vec<(&str, Vec<u8>)> = files.iter().map(|(p, b)| (p.as_str(), b.clone())).collect();
    Ok((write_pck(&refs), stamps, mats))
}

#[derive(Debug, Default, PartialEq)]
pub struct Report {
    pub pack: String,
    pub materials: usize,
    pub stamps: usize,
    /// Pictures that were not a prop or a texture (walls, paths, previews…) or would not decode.
    pub skipped: usize,
    pub notes: Vec<String>,
}

#[derive(PartialEq, Clone, Copy)]
enum Kind {
    Material,
    Stamp,
}

/// The v2 manifest as written into an exported pack (`ndpack.toml`).
#[derive(Default, serde::Serialize)]
struct NdPackOut {
    license: Option<ndcart_core::assetmeta::License>,
    assets: std::collections::BTreeMap<String, PackEntryOut>,
}

#[derive(serde::Serialize)]
struct PackEntryOut {
    #[serde(skip_serializing_if = "Option::is_none")]
    footprint: Option<Vec<[f32; 2]>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    clearance: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    placement: Option<ndcart_core::assetmeta::Placement>,
    #[serde(skip_serializing_if = "Option::is_none")]
    style_tags: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    license: Option<ndcart_core::assetmeta::License>,
}

/// A licence file a pack author shipped (read as terms text on import).
fn is_license_file(path: &str) -> bool {
    let base = path.rsplit('/').next().unwrap_or(path).to_lowercase();
    base == "license.txt" || base == "licence.txt" || base == "license" || base == "licence"
}

/// Draft style tags from the pack's own name: its words minus filler, so an
/// imported pack is searchable by look later. The GM confirms or edits them.
fn draft_style_tags(pack: &str) -> Vec<String> {
    const FILLER: [&str; 8] = ["pack", "the", "and", "of", "set", "assets", "art", "volume"];
    pack.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| w.len() >= 3 && !FILLER.contains(&w.to_lowercase().as_str()))
        .map(|w| w.to_lowercase())
        .take(ndcart_core::assetmeta::MAX_STYLE_TAGS)
        .collect()
}

/// One asset's overrides from `ndpack.toml` (the v2 authoring path).
#[derive(Default, serde::Deserialize)]
struct PackEntryOverride {
    footprint: Option<Vec<[f32; 2]>>,
    clearance: Option<f32>,
    placement: Option<ndcart_core::assetmeta::Placement>,
    style_tags: Option<Vec<String>>,
    license: Option<ndcart_core::assetmeta::License>,
}

#[derive(Default, serde::Deserialize)]
struct NdPackManifest {
    license: Option<ndcart_core::assetmeta::License>,
    #[serde(default)]
    assets: std::collections::BTreeMap<String, PackEntryOverride>,
}

/// Parse an optional `ndpack.toml` at the pack root. Malformed manifests are
/// reported, never fatal: the pictures still import.
fn ndpack_overrides(entries: &[Entry]) -> NdPackManifest {
    let Some(e) = entries
        .iter()
        .find(|e| e.path.to_lowercase() == "ndpack.toml")
    else {
        return NdPackManifest::default();
    };
    match toml::from_str::<NdPackManifest>(&String::from_utf8_lossy(&e.bytes)) {
        Ok(m) => m,
        Err(err) => {
            eprintln!("ndpack.toml could not be read (ignored): {err}");
            NdPackManifest::default()
        }
    }
}

fn classify(path: &str) -> Option<Kind> {
    let p = path.to_lowercase();
    let img = ["png", "webp", "jpg", "jpeg"]
        .iter()
        .any(|e| p.ends_with(&format!(".{e}")));
    if !img || p.contains("thumbnail") || p.contains("preview") || p.ends_with("pack.png") {
        return None;
    }
    let has = |k: &[&str]| k.iter().any(|w| p.split('/').any(|seg| seg == *w));
    if has(&["objects", "symbols"]) {
        Some(Kind::Stamp)
    } else if has(&["terrain", "tiles"]) || {
        // Wonderdraft keeps ground textures directly under textures/; Dungeondraft's
        // walls, paths, portals and lights live in their own sub-folders and are left out.
        let segs: Vec<&str> = p.split('/').collect();
        segs.len() == 2 && segs[0] == "textures"
    } {
        Some(Kind::Material)
    } else {
        None
    }
}

fn pretty(path: &str) -> String {
    let stem = path.rsplit('/').next().unwrap_or(path);
    let stem = stem.rsplit_once('.').map(|(s, _)| s).unwrap_or(stem);
    let words: String = stem.replace(['_', '-'], " ");
    let mut c = words.trim().chars();
    let name: String = c
        .next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default();
    name.chars().take(60).collect()
}

/// The pack's own name and author from `pack.json`, if present.
fn pack_info(entries: &[Entry], fallback: &str) -> (String, String) {
    let j = entries
        .iter()
        .find(|e| e.path.ends_with("pack.json"))
        .and_then(|e| serde_json::from_slice::<serde_json::Value>(&e.bytes).ok());
    let get = |k: &str| {
        j.as_ref()
            .and_then(|v| v.get(k))
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };
    (
        get("name").unwrap_or_else(|| fallback.to_string()),
        get("author").unwrap_or_default(),
    )
}

/// Put a pack's props and materials into the palette.
pub fn import(entries: &[Entry], fallback_name: &str, assets: &Assets) -> Result<Report, String> {
    let (pack, author) = pack_info(entries, fallback_name);
    let mut rep = Report {
        pack: pack.clone(),
        ..Default::default()
    };
    let origin = Provenance {
        source: "imported".into(),
        prompt: format!(
            "from pack {pack}{}",
            if author.is_empty() {
                String::new()
            } else {
                format!(" by {author}")
            }
        ),
        seed: -1,
        date: ndcart_core::doc::now_iso(),
    };
    // Pack-asset metadata v2 (plan 22.1): the pack's name rides every item,
    // a licence file becomes the licence block's terms, and an optional
    // `ndpack.toml` at the pack root overrides per-asset (authoring path).
    let mut pack_meta = ndcart_core::assetmeta::AssetMeta {
        pack: Some(pack.clone()),
        ..Default::default()
    };
    pack_meta.style_tags = draft_style_tags(&pack);
    if let Some(lic) = entries.iter().find(|e| is_license_file(&e.path)) {
        let terms = String::from_utf8_lossy(&lic.bytes[..lic.bytes.len().min(4_000)]).into_owned();
        pack_meta.license = Some(ndcart_core::assetmeta::License {
            identifier: String::new(),
            commercial_map: ndcart_core::assetmeta::Rights::Unknown,
            redistribution: ndcart_core::assetmeta::Rights::Unknown,
            attribution_required: false,
            terms,
        });
    }
    let overrides = ndpack_overrides(entries);
    if let Some(manifest_licence) = overrides.license {
        // An author's structured licence beats a file excerpt.
        pack_meta.license = Some(manifest_licence);
    }
    let mut done = 0usize;
    for e in entries {
        let Some(kind) = classify(&e.path) else {
            if e.path.to_lowercase().ends_with(".png") {
                rep.skipped += 1;
            }
            continue;
        };
        if done >= MAX_ITEMS {
            rep.notes.push(format!(
                "stopped at {MAX_ITEMS} items; import the rest from a smaller folder"
            ));
            break;
        }
        let Ok((_, w, _h)) = crate::convert::decode_rgba(&e.bytes) else {
            rep.skipped += 1;
            continue;
        };
        let asset = assets.put(&e.bytes)?;
        let squares = (w as f32 / PX_PER_SQUARE).clamp(0.25, 12.0);
        let (id, k, size) = match kind {
            Kind::Stamp => (format!("stp_{}", &asset[..12]), "stamp", squares.max(0.3)),
            Kind::Material => (
                format!("mat_{}", &asset[..12]),
                "material",
                squares.max(0.5),
            ),
        };
        let mut meta = pack_meta.clone();
        if let Some(o) = overrides.assets.get(&e.path) {
            if let Some(f) = &o.footprint {
                meta.footprint = Some(ndcart_core::assetmeta::Footprint(f.clone()));
            }
            if let Some(c) = o.clearance {
                meta.clearance = c;
            }
            if let Some(p) = &o.placement {
                meta.placement = p.clone();
            }
            if let Some(tags) = &o.style_tags {
                if !tags.is_empty() {
                    meta.style_tags = tags.clone();
                }
            }
            if let Some(l) = &o.license {
                meta.license = Some(l.clone());
            }
        }
        assets.register(Custom {
            id,
            name: pretty(&e.path),
            kind: k.into(),
            asset,
            size,
            origin: origin.clone(),
            tags: Vec::new(),
            meta,
        })?;
        match kind {
            Kind::Stamp => rep.stamps += 1,
            Kind::Material => rep.materials += 1,
        }
        done += 1;
    }
    if rep.stamps + rep.materials == 0 {
        rep.notes.push("no props or textures were found (looked for folders named objects, symbols, terrain, textures or tiles)".into());
    }
    Ok(rep)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn build_pck(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
        super::write_pck(files)
    }

    pub(crate) fn png(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
        let mut pm = tiny_skia::Pixmap::new(w, h).unwrap();
        pm.fill(tiny_skia::Color::from_rgba8(
            rgba[0], rgba[1], rgba[2], rgba[3],
        ));
        pm.encode_png().unwrap()
    }

    fn store() -> Assets {
        crate::assets::new_for_tests()
    }

    #[test]
    fn a_dungeondraft_pack_reads_back_exactly_and_bad_ones_are_refused() {
        let files = [
            (
                "pack.json",
                br#"{"name":"Cave Pack","author":"A. Mapper"}"#.to_vec(),
            ),
            ("textures/objects/rock.png", png(8, 8, [90, 90, 90, 255])),
        ];
        let pck = build_pck(&files);
        let got = read_pck(&pck).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[1].path, "textures/objects/rock.png");
        assert_eq!(got[1].bytes, files[1].1);
        assert!(read_pck(b"PK\x03\x04 zip")
            .unwrap_err()
            .contains("not a Dungeondraft pack"));
        // Truncated anywhere: an error, never a panic.
        for cut in [10, 40, 90, 120, pck.len() - 5] {
            assert!(read_pck(&pck[..cut]).is_err(), "cut at {cut}");
        }
        // A data range outside the file, and a newer archive format.
        let mut bad = pck.clone();
        let last = bad.len() - 1;
        bad.truncate(last - files[1].1.len() + 4);
        assert!(read_pck(&bad).unwrap_err().contains("outside the pack"));
        let mut v2 = pck.clone();
        v2[4..8].copy_from_slice(&2u32.to_le_bytes());
        assert!(read_pck(&v2).unwrap_err().contains("not supported"));
    }

    #[test]
    fn objects_become_props_textures_become_materials_and_the_rest_is_left_alone() {
        let files = [
            (
                "pack.json",
                br#"{"name":"Cave Pack","author":"A. Mapper"}"#.to_vec(),
            ),
            (
                "textures/objects/big_rock.png",
                png(512, 256, [90, 90, 90, 255]),
            ),
            (
                "textures/objects/small-crate.png",
                png(128, 128, [140, 90, 40, 255]),
            ),
            (
                "textures/terrain/moss.png",
                png(512, 512, [40, 120, 50, 255]),
            ),
            ("textures/walls/brick.png", png(64, 64, [1, 1, 1, 255])),
            (
                "textures/objects/thumbnail.png",
                png(32, 32, [1, 2, 3, 255]),
            ),
            ("textures/objects/broken.png", b"not a picture".to_vec()),
        ];
        let a = store();
        let rep = import(&read_pck(&build_pck(&files)).unwrap(), "fallback", &a).unwrap();
        assert_eq!(
            (rep.pack.as_str(), rep.stamps, rep.materials, rep.skipped),
            ("Cave Pack", 2, 1, 3),
            "{rep:?}"
        );
        let stamps = a.customs("stamp");
        let rock = stamps
            .iter()
            .find(|c| c.name == "Big rock")
            .expect("pretty name");
        assert_eq!(rock.size, 2.0, "512 px is two 256 px squares");
        assert!(stamps
            .iter()
            .any(|c| c.name == "Small crate" && c.size == 0.5));
        assert_eq!(rock.origin.prompt, "from pack Cave Pack by A. Mapper");
        assert_eq!(rock.origin.source, "imported");
        let mats = a.customs("material");
        assert_eq!(
            (mats.len(), mats[0].name.as_str(), mats[0].size),
            (1, "Moss", 2.0)
        );
        // Importing again changes nothing (same pictures, same ids).
        import(&read_pck(&build_pck(&files)).unwrap(), "fallback", &a).unwrap();
        assert_eq!(
            (a.customs("stamp").len(), a.customs("material").len()),
            (2, 1)
        );
        // A pack with nothing usable says why.
        let empty = import(
            &[Entry {
                path: "readme.txt".into(),
                bytes: vec![],
            }],
            "x",
            &a,
        )
        .unwrap();
        assert!(empty.notes[0].contains("no props or textures"));
    }

    #[test]
    fn your_own_art_exports_as_a_pack_that_reads_back_and_borrowed_art_stays_out() {
        let a = store();
        let mk = |kind: &str, name: &str, prompt: &str, rgba: [u8; 4]| {
            let asset = a.put(&png(16, 16, rgba)).unwrap();
            a.register(Custom {
                meta: Default::default(),
                id: format!(
                    "{}_{}",
                    if kind == "stamp" { "stp" } else { "mat" },
                    &asset[..12]
                ),
                name: name.into(),
                kind: kind.into(),
                asset,
                size: 2.0,
                tags: vec![],
                origin: Provenance {
                    source: "imported".into(),
                    prompt: prompt.into(),
                    seed: -1,
                    date: "now".into(),
                },
            })
            .unwrap();
        };
        mk("stamp", "My Lantern!", "", [200, 180, 40, 255]);
        mk("stamp", "My Lantern!", "", [200, 181, 40, 255]); // same name: gets a distinct file
        mk("material", "Red Tiles", "red tiles", [180, 30, 30, 255]);
        mk(
            "stamp",
            "Borrowed",
            "from pack Someone Else by X",
            [1, 2, 3, 255],
        );
        let (pck, stamps, mats) = export_own(&a, "Harbour Set", false).unwrap();
        assert_eq!((stamps, mats), (2, 1), "the borrowed prop is not included");
        let back = read_pck(&pck).unwrap();
        let paths: Vec<&str> = back.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths[0], "pack.json");
        assert!(
            paths.contains(&"textures/objects/my_lantern.png")
                && paths.contains(&"textures/objects/my_lantern_2.png"),
            "{paths:?}"
        );
        assert!(paths.contains(&"textures/terrain/red_tiles.png"));
        let info: serde_json::Value = serde_json::from_slice(&back[0].bytes).unwrap();
        assert_eq!(
            (info["name"].as_str(), info["id"].as_str()),
            (Some("Harbour Set"), Some("harbour_set"))
        );
        // Importing it into a fresh store gives the same art back.
        let b = store();
        let rep = import(&back, "x", &b).unwrap();
        assert_eq!(
            (rep.pack.as_str(), rep.stamps, rep.materials),
            ("Harbour Set", 2, 1)
        );
        // Opting in includes borrowed art; an empty library says what to do.
        assert_eq!(export_own(&a, "All", true).unwrap().1, 3);
        assert!(export_own(&store(), "Empty", false)
            .unwrap_err()
            .contains("nothing of your own"));
    }

    #[test]
    fn v2_metadata_rides_import_and_export() {
        let dir = std::env::temp_dir().join(format!("ndcart-packv2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("objects")).unwrap();
        std::fs::write(
            dir.join("objects/table.png"),
            png(256, 128, [180, 140, 90, 255]),
        )
        .unwrap();
        std::fs::write(
            dir.join("LICENSE.txt"),
            "CC-BY-4.0: share with attribution, no standalone redistribution.",
        )
        .unwrap();
        std::fs::write(
            dir.join("ndpack.toml"),
            [
                "[assets.\"objects/table.png\"]",
                "clearance = 0.5",
                "footprint = [[-1.5, -0.5], [1.5, -0.5], [1.5, 0.5]]",
                "",
                "[assets.\"objects/table.png\".placement]",
                "align_to_wall = true",
                "",
                "[assets.\"objects/table.png\".license]",
                "identifier = \"CC-BY-4.0\"",
                "commercial_map = \"allowed\"",
                "redistribution = \"forbidden\"",
                "attribution_required = true",
            ]
            .join(
                "
",
            ),
        )
        .unwrap();
        let entries = read_dir(&dir).unwrap();
        let a = store();
        let rep = import(&entries, "Ink Workshop", &a).unwrap();
        assert_eq!(rep.stamps, 1);

        let stamp = a.customs("stamp").into_iter().next().unwrap();
        assert_eq!(stamp.meta.pack.as_deref(), Some("Ink Workshop"));
        assert!(
            !stamp.meta.style_tags.is_empty(),
            "drafted from the pack name"
        );
        let lic = stamp
            .meta
            .license
            .as_ref()
            .expect("the manifest licence wins");
        assert_eq!(lic.identifier, "CC-BY-4.0");
        assert_eq!(lic.commercial_map, ndcart_core::assetmeta::Rights::Allowed);
        let fp = stamp.meta.footprint.as_ref().expect("override applied");
        assert_eq!(fp.0.len(), 3);
        assert!(stamp.meta.placement.align_to_wall);

        // The exported pack carries the manifest back out.
        let (pck, stamps_n, _m) = export_own(&a, "Ink Workshop", true).unwrap();
        assert_eq!(stamps_n, 1);
        let out = read_pck(&pck).unwrap();
        assert!(out.iter().any(|e| e.path == "ndpack.toml"));
        let toml_text =
            String::from_utf8_lossy(&out.iter().find(|e| e.path == "ndpack.toml").unwrap().bytes);
        assert!(toml_text.contains("footprint"), "{toml_text}");
        assert!(toml_text.contains("CC-BY-4.0"));
    }

    #[test]
    fn a_plain_folder_imports_the_same_way() {
        let dir = std::env::temp_dir().join(format!("ndcart-pack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("symbols")).unwrap();
        std::fs::create_dir_all(dir.join("textures")).unwrap();
        std::fs::write(
            dir.join("symbols/tower.png"),
            png(256, 256, [200, 200, 200, 255]),
        )
        .unwrap();
        std::fs::write(
            dir.join("textures/sand.png"),
            png(256, 256, [220, 200, 120, 255]),
        )
        .unwrap();
        std::fs::write(dir.join("notes.txt"), "ignored").unwrap();
        let entries = read_dir(&dir).unwrap();
        assert_eq!(
            entries.len(),
            2,
            "pictures and pack json; notes.txt stays out (only licence/manifest txt-toml would ride)"
        );
        let a = store();
        let rep = import(&entries, "My folder", &a).unwrap();
        assert_eq!(
            (rep.pack.as_str(), rep.stamps, rep.materials),
            ("My folder", 1, 1)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
