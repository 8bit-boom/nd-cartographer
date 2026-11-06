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
                if !["png", "webp", "jpg", "jpeg", "json"].contains(&ext.as_str()) {
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
        assets.register(Custom {
            id,
            name: pretty(&e.path),
            kind: k.into(),
            asset,
            size,
            origin: origin.clone(),
            tags: Vec::new(),
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

    /// A Godot 3 `.pck` holding `files`, laid out the way Dungeondraft writes them.
    pub(crate) fn build_pck(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
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
        assert_eq!(entries.len(), 2, "only pictures and json are read");
        let a = store();
        let rep = import(&entries, "My folder", &a).unwrap();
        assert_eq!(
            (rep.pack.as_str(), rep.stamps, rep.materials),
            ("My folder", 1, 1)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
