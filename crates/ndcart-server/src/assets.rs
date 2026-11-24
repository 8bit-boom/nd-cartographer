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
    /// Pack-asset metadata v2 (plan 22.1): footprint, clearance, placement,
    /// style tags, licence, pack. Defaults keep every v1 manifest loading.
    #[serde(default)]
    pub meta: ndcart_core::assetmeta::AssetMeta,
}

/// A bounded cache with least-recently-used eviction. The decoded-pixmap cache
/// used to clear *wholesale* at 24 entries, re-decoding everything under load;
/// an LRU keeps the hot pictures and drops only the coldest.
struct Lru<V> {
    cap: usize,
    map: HashMap<String, (V, u64)>,
    tick: u64,
}

impl<V: Clone> Lru<V> {
    fn new(cap: usize) -> Self {
        Lru {
            cap,
            map: HashMap::new(),
            tick: 0,
        }
    }
    fn get(&mut self, key: &str) -> Option<V> {
        self.tick += 1;
        let tick = self.tick;
        let entry = self.map.get_mut(key)?;
        entry.1 = tick;
        Some(entry.0.clone())
    }
    fn insert(&mut self, key: String, value: V) {
        self.tick += 1;
        let tick = self.tick;
        self.map.insert(key, (value, tick));
        if self.map.len() > self.cap {
            if let Some(oldest) = self
                .map
                .iter()
                .min_by_key(|(_, (_, t))| *t)
                .map(|(k, _)| k.clone())
            {
                self.map.remove(&oldest);
            }
        }
    }
}

/// Keep the resident byte cache under a budget by dropping entries (never the
/// one just requested). Only called when a data directory backs the store, so
/// an evicted picture can be re-read from disk; in-memory mode must keep all.
fn evict_bytes_over_budget(cache: &mut HashMap<String, Arc<Vec<u8>>>, keep: &str) {
    const BUDGET: usize = 128 << 20; // 128 MiB
    let mut total: usize = cache.values().map(|v| v.len()).sum();
    if total <= BUDGET {
        return;
    }
    let keys: Vec<String> = cache.keys().cloned().collect();
    for k in keys {
        if total <= BUDGET {
            break;
        }
        if k == keep {
            continue;
        }
        if let Some(v) = cache.remove(&k) {
            total -= v.len();
        }
    }
}

pub struct Assets {
    dir: Option<PathBuf>,
    custom: Mutex<Vec<Custom>>,
    bytes: Mutex<HashMap<String, Arc<Vec<u8>>>>,
    decoded: Mutex<Lru<Arc<tiny_skia::Pixmap>>>,
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
            decoded: Mutex::new(Lru::new(64)),
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
        decoded: Mutex::new(Lru::new(64)),
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
        let mut cache = self.bytes.lock().expect("assets");
        cache.insert(id.clone(), Arc::new(data.to_vec()));
        if self.dir.is_some() {
            evict_bytes_over_budget(&mut cache, &id);
        }
        Ok(id)
    }

    /// Add (or replace) a custom material/stamp and save the list.
    pub fn register(&self, c: Custom) -> Result<(), String> {
        c.meta
            .validate()
            .map_err(|e| format!("asset metadata: {e}"))?;

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

    /// Replace an asset's v2 metadata (validated; the asset itself is untouched).
    pub fn set_meta(
        &self,
        id: &str,
        kind: &str,
        meta: ndcart_core::assetmeta::AssetMeta,
    ) -> Result<(), String> {
        meta.validate()
            .map_err(|e| format!("asset metadata: {e}"))?;
        let mut custom = self.custom.lock().expect("custom assets");
        let c = custom
            .iter_mut()
            .find(|c| c.id == id && c.kind == kind)
            .ok_or_else(|| format!("no {kind} {id}"))?;
        c.meta = meta;
        if let Some(dir) = &self.dir {
            let body = serde_json::to_string(&*custom).map_err(|e| e.to_string())?;
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
        let mut cache = self.bytes.lock().expect("assets");
        cache.insert(id.to_string(), data.clone());
        if self.dir.is_some() {
            evict_bytes_over_budget(&mut cache, id);
        }
        Some(data)
    }

    /// The decoded picture as a premultiplied pixmap, cached.
    #[allow(unknown_lints, clippy::chunks_exact_to_as_chunks)]
    pub fn get_pixmap(&self, id: &str) -> Option<Arc<tiny_skia::Pixmap>> {
        if let Some(p) = self.decoded.lock().expect("assets").get(id) {
            return Some(p);
        }
        let bytes = self.get_bytes(id)?;
        let (rgba, w, h) = crate::convert::decode_rgba(&bytes).ok()?;
        let mut pm = tiny_skia::Pixmap::new(w, h)?;
        for (dst, px) in pm.pixels_mut().iter_mut().zip(rgba.chunks_exact(4)) {
            *dst = tiny_skia::ColorU8::from_rgba(px[0], px[1], px[2], px[3]).premultiply();
        }
        let pm = Arc::new(pm);
        self.decoded
            .lock()
            .expect("assets")
            .insert(id.to_string(), pm.clone());
        Some(pm)
    }

    /// A power-of-two downscaled level of a picture (level 0 is full size), for
    /// minifying large underlays without the aliasing and wide pixel footprint
    /// of bilinear-sampling the full resolution at low zoom. Each level is a
    /// 2x2 box filter of the one above (correct for premultiplied pixels) and
    /// is cached alongside the full pixmaps.
    pub fn get_pixmap_mip(&self, id: &str, level: u32) -> Option<Arc<tiny_skia::Pixmap>> {
        if level == 0 {
            return self.get_pixmap(id);
        }
        let key = format!("{id}:{level}");
        if let Some(p) = self.decoded.lock().expect("assets").get(&key) {
            return Some(p);
        }
        let prev = self.get_pixmap_mip(id, level - 1)?;
        let (w, h) = (prev.width() / 2, prev.height() / 2);
        if w == 0 || h == 0 {
            return Some(prev); // cannot halve further
        }
        let mut pm = tiny_skia::Pixmap::new(w, h)?;
        let src = prev.pixels();
        let pw = prev.width() as usize;
        let avg = |a: tiny_skia::PremultipliedColorU8,
                   b: tiny_skia::PremultipliedColorU8,
                   c: tiny_skia::PremultipliedColorU8,
                   d: tiny_skia::PremultipliedColorU8| {
            let m = |x: u8, y: u8, z: u8, w: u8| {
                ((u16::from(x) + u16::from(y) + u16::from(z) + u16::from(w)) / 4) as u8
            };
            tiny_skia::PremultipliedColorU8::from_rgba(
                m(a.red(), b.red(), c.red(), d.red()),
                m(a.green(), b.green(), c.green(), d.green()),
                m(a.blue(), b.blue(), c.blue(), d.blue()),
                m(a.alpha(), b.alpha(), c.alpha(), d.alpha()),
            )
            // Averaging premultiplied colours keeps every channel <= alpha, so
            // the checked constructor always succeeds.
            .expect("average of premultiplied pixels is premultiplied")
        };
        for (y, row) in pm.pixels_mut().chunks_mut(w as usize).enumerate() {
            for (x, dst) in row.iter_mut().enumerate() {
                let i = (2 * y) * pw + 2 * x;
                *dst = avg(src[i], src[i + 1], src[i + pw], src[i + pw + 1]);
            }
        }
        let pm = Arc::new(pm);
        self.decoded.lock().expect("assets").insert(key, pm.clone());
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
            decoded: Mutex::new(Lru::new(64)),
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
    fn mipmaps_halve_and_keep_the_colour() {
        let a = new_for_tests();
        let id = a.put(&red_png(8, 8)).unwrap();
        for (level, side) in [(0, 8), (1, 4), (2, 2), (3, 1)] {
            let pm = a.get_pixmap_mip(&id, level).unwrap();
            assert_eq!((pm.width(), pm.height()), (side, side), "level {level}");
            assert_eq!(pm.pixel(0, 0).unwrap().red(), 255, "still red at {level}");
        }
        // Cannot halve a 1x1 further: it stays 1x1 rather than vanishing.
        let pm = a.get_pixmap_mip(&id, 9).unwrap();
        assert_eq!((pm.width(), pm.height()), (1, 1));
    }

    #[test]
    fn the_decoded_cache_evicts_the_coldest_not_everything() {
        let a = new_for_tests();
        // Fill past the cap with distinct pictures; the first ones age out.
        let mut ids = Vec::new();
        for i in 0..70u32 {
            ids.push(a.put(&red_png(2 + i, 3)).unwrap());
        }
        for id in &ids {
            a.get_pixmap(id).unwrap();
        }
        // The most recently used picture is still cached (no decode needed);
        // the cache stayed bounded rather than clearing wholesale.
        let last = ids.last().unwrap();
        assert!(a.get_pixmap(last).is_some());
        assert!(a.decoded.lock().unwrap().map.len() <= 64);
    }

    #[test]
    fn junk_and_traversal_are_refused() {
        let a = Assets {
            dir: None,
            custom: Mutex::new(Vec::new()),
            bytes: Mutex::new(HashMap::new()),
            decoded: Mutex::new(Lru::new(64)),
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
            decoded: Mutex::new(Lru::new(64)),
        };
        let id = a.put(&red_png(2, 2)).unwrap();
        let b = Assets {
            dir: Some(dir.clone()),
            custom: Mutex::new(Vec::new()),
            bytes: Mutex::new(HashMap::new()),
            decoded: Mutex::new(Lru::new(64)),
        };
        assert!(
            b.get_bytes(&id).is_some(),
            "a fresh store reads it back from disk"
        );
        assert!(b.get_pixmap(&id).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
