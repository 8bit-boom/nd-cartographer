//! The asset store (plan §5): pictures are content-addressed by SHA-256 and a
//! document only holds the id. Bytes are validated as images on the way in (a
//! file that does not decode is refused), written atomically when a data
//! directory is configured, and kept in memory otherwise. Decoded pixmaps are
//! cached for the baker.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

/// Where a custom asset came from (kept with it, plan §8.5 provenance).
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Provenance {
    /// `ai` or `imported`.
    pub source: String,
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub seed: i64,
    pub date: String,
}

/// A picture the GM turned into a paintable material or a placeable prop.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Custom {
    /// `mat_<12 hex>` or `stp_<12 hex>`: usable as a space's material or a stamp's asset.
    pub id: String,
    pub name: String,
    /// `material` or `stamp`.
    pub kind: String,
    pub asset: String,
    /// A material: cells per repeat of the picture. A stamp: width in cells.
    pub size: f32,
    pub origin: Provenance,
    /// Short search words the GM has confirmed (suggested by the AI, never applied unasked).
    #[serde(default)]
    pub tags: Vec<String>,
}

pub struct Assets {
    dir: Option<PathBuf>,
    custom: Mutex<Vec<Custom>>,
    bytes: Mutex<HashMap<String, Arc<Vec<u8>>>>,
    decoded: Mutex<HashMap<String, Arc<tiny_skia::Pixmap>>>,
}

static GLOBAL: OnceLock<Assets> = OnceLock::new();

/// Configure the process-wide store (the first call wins; later calls return it).
pub fn init(dir: Option<PathBuf>) -> &'static Assets {
    GLOBAL.get_or_init(|| {
        if let Some(d) = &dir {
            let _ = std::fs::create_dir_all(d);
        }
        let custom = dir
            .as_ref()
            .and_then(|d| std::fs::read_to_string(d.join("custom.json")).ok())
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        Assets {
            dir,
            custom: Mutex::new(custom),
            bytes: Mutex::new(HashMap::new()),
            decoded: Mutex::new(HashMap::new()),
        }
    })
}

/// A fresh in-memory store (tests that must not share the process-wide one).
#[cfg(test)]
pub(crate) fn new_for_tests() -> Assets {
    Assets {
        dir: None,
        custom: Mutex::new(Vec::new()),
        bytes: Mutex::new(HashMap::new()),
        decoded: Mutex::new(HashMap::new()),
    }
}

/// The store; in-memory only if `init` was never called (tests).
pub fn global() -> &'static Assets {
    init(None)
}

impl Assets {
    /// Validate, hash and keep `data`. Returns its id.
    pub fn put(&self, data: &[u8]) -> Result<String, String> {
        // Refuse anything that is not a decodable image within the pixel budget.
        crate::convert::decode_rgba(data)?;
        let id = ndcart_core::hash::sha256_hex(data);
        if let Some(dir) = &self.dir {
            let path = dir.join(&id);
            if !path.exists() {
                crate::store::write_atomic(&path, data)
                    .map_err(|e| format!("cannot store the asset: {e}"))?;
            }
        }
        self.bytes
            .lock()
            .expect("assets")
            .insert(id.clone(), Arc::new(data.to_vec()));
        Ok(id)
    }

    /// Add (or replace) a custom material/stamp and save the list.
    pub fn register(&self, c: Custom) -> Result<(), String> {
        if !ndcart_core::hash::is_asset_id(&c.asset) || self.get_bytes(&c.asset).is_none() {
            return Err("that picture is not in the asset store".into());
        }
        let mut list = self.custom.lock().expect("custom");
        list.retain(|x| x.id != c.id);
        list.push(c);
        if list.len() > 500 {
            list.remove(0);
        }
        if let Some(dir) = &self.dir {
            let body = serde_json::to_string(&*list).map_err(|e| e.to_string())?;
            crate::store::write_atomic(&dir.join("custom.json"), body.as_bytes())
                .map_err(|e| format!("cannot save the library: {e}"))?;
        }
        Ok(())
    }

    /// Replace a custom item's tags (already validated) and save.
    pub fn set_tags(&self, id: &str, tags: Vec<String>) -> Result<(), String> {
        let mut list = self.custom.lock().expect("custom");
        let c = list.iter_mut().find(|c| c.id == id).ok_or("no such item")?;
        c.tags = tags;
        if let Some(dir) = &self.dir {
            let body = serde_json::to_string(&*list).map_err(|e| e.to_string())?;
            crate::store::write_atomic(&dir.join("custom.json"), body.as_bytes())
                .map_err(|e| format!("cannot save the library: {e}"))?;
        }
        Ok(())
    }

    pub fn customs(&self, kind: &str) -> Vec<Custom> {
        self.custom
            .lock()
            .expect("custom")
            .iter()
            .filter(|c| c.kind == kind)
            .cloned()
            .collect()
    }

    pub fn find_custom(&self, id: &str, kind: &str) -> Option<Custom> {
        self.custom
            .lock()
            .expect("custom")
            .iter()
            .find(|c| c.id == id && c.kind == kind)
            .cloned()
    }

    pub fn get_bytes(&self, id: &str) -> Option<Arc<Vec<u8>>> {
        if !ndcart_core::hash::is_asset_id(id) {
            return None; // also keeps `../` out of the filesystem path below
        }
        if let Some(b) = self.bytes.lock().expect("assets").get(id) {
            return Some(b.clone());
        }
        let data = std::fs::read(self.dir.as_ref()?.join(id)).ok()?;
        let data = Arc::new(data);
        self.bytes
            .lock()
            .expect("assets")
            .insert(id.to_string(), data.clone());
        Some(data)
    }

    /// The decoded picture as a premultiplied pixmap, cached.
    #[allow(unknown_lints, clippy::chunks_exact_to_as_chunks)]
    pub fn get_pixmap(&self, id: &str) -> Option<Arc<tiny_skia::Pixmap>> {
        if let Some(p) = self.decoded.lock().expect("assets").get(id) {
            return Some(p.clone());
        }
        let bytes = self.get_bytes(id)?;
        let (rgba, w, h) = crate::convert::decode_rgba(&bytes).ok()?;
        let mut pm = tiny_skia::Pixmap::new(w, h)?;
        for (dst, px) in pm.pixels_mut().iter_mut().zip(rgba.chunks_exact(4)) {
            *dst = tiny_skia::ColorU8::from_rgba(px[0], px[1], px[2], px[3]).premultiply();
        }
        let pm = Arc::new(pm);
        let mut cache = self.decoded.lock().expect("assets");
        if cache.len() >= 24 {
            cache.clear();
        }
        cache.insert(id.to_string(), pm.clone());
        Some(pm)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn red_png(w: u32, h: u32) -> Vec<u8> {
        let mut pm = tiny_skia::Pixmap::new(w, h).unwrap();
        pm.fill(tiny_skia::Color::from_rgba8(255, 0, 0, 255));
        pm.encode_png().unwrap()
    }

    #[test]
    fn put_hashes_dedupes_and_round_trips() {
        let a = Assets {
            dir: None,
            custom: Mutex::new(Vec::new()),
            bytes: Mutex::new(HashMap::new()),
            decoded: Mutex::new(HashMap::new()),
        };
        let png = red_png(4, 3);
        let id = a.put(&png).unwrap();
        assert_eq!(id, ndcart_core::hash::sha256_hex(&png));
        assert_eq!(a.put(&png).unwrap(), id, "same bytes, same id");
        assert_eq!(&**a.get_bytes(&id).unwrap(), &png);
        let pm = a.get_pixmap(&id).unwrap();
        assert_eq!((pm.width(), pm.height()), (4, 3));
        assert_eq!(pm.pixel(1, 1).unwrap().red(), 255);
    }

    #[test]
    fn junk_and_traversal_are_refused() {
        let a = Assets {
            dir: None,
            custom: Mutex::new(Vec::new()),
            bytes: Mutex::new(HashMap::new()),
            decoded: Mutex::new(HashMap::new()),
        };
        assert!(a.put(b"not an image").is_err());
        assert!(a.put(b"").is_err());
        for bad in [
            "../../etc/passwd",
            "short",
            &"G".repeat(64),
            &"A".repeat(64),
        ] {
            assert!(a.get_bytes(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn a_data_dir_persists_the_asset_across_stores() {
        let dir = std::env::temp_dir().join(format!("ndcart-assets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = Assets {
            dir: Some(dir.clone()),
            custom: Mutex::new(Vec::new()),
            bytes: Mutex::new(HashMap::new()),
            decoded: Mutex::new(HashMap::new()),
        };
        let id = a.put(&red_png(2, 2)).unwrap();
        let b = Assets {
            dir: Some(dir.clone()),
            custom: Mutex::new(Vec::new()),
            bytes: Mutex::new(HashMap::new()),
            decoded: Mutex::new(HashMap::new()),
        };
        assert!(
            b.get_bytes(&id).is_some(),
            "a fresh store reads it back from disk"
        );
        assert!(b.get_pixmap(&id).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
