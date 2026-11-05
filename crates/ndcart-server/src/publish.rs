//! `ndcart publish` — publish a stored document to nd-world (plan §9.2 A).
//!
//! ndcart publish DOC --server URL [--world SLUG] --email EMAIL
//!                [--floor ID | --all-floors] [--ppc 32] [--format webp|avif|png] [--data-dir DIR]
//!
//! The password is read from `ND_CARTO_PASSWORD` or the file named by
//! `ND_CARTO_PASSWORD_FILE` — never an argument (it would land in shell history
//! and process lists) and never logged. A two-step code comes from
//! `ND_CARTO_TOTP`. DOC is a document id or an exact name.

use crate::{store, tiles};
use ndcart_bridge::{rooms, Bridge, BridgeError, PublishMap, PublishRegion};
use ndcart_core::{for_audience, Audience, NdWorldLink};
use std::path::PathBuf;

/// nd-world's default upload cap (`max_upload_mb`, adjustable per instance).
const DEFAULT_UPLOAD_CAP_MB: usize = 20;
const MIN_PPC: i64 = 8; // nd-world's room grid needs cells of at least 8 px

pub struct Args {
    pub all_floors: bool,
    pub doc: String,
    pub server: String,
    pub world: Option<String>,
    pub email: String,
    pub floor: Option<String>,
    pub ppc: i64,
    pub format: tiles::ImageFormat,
    pub data_dir: PathBuf,
    /// The server's upload cap in MB (`max_upload_mb`); the picture is fitted under it.
    pub max_mb: usize,
    /// `--audience player` publishes what the players may see (secret doors
    /// become plain walls, GM-only notes are dropped).
    pub audience: Audience,
    /// World maps: on a re-publish leave the markers and regions in nd-world alone.
    pub keep_overlay: bool,
}

pub fn parse(args: Vec<String>) -> Result<Args, String> {
    let (mut doc, mut server, mut world, mut email) = (None, None, None, None);
    let (mut floor, mut ppc, mut format, mut data_dir) = (None, 32i64, "webp".to_string(), None);
    let mut max_mb = DEFAULT_UPLOAD_CAP_MB;
    let mut audience = Audience::Gm;
    let mut keep_overlay = false;
    let mut all_floors = false;
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        let mut val = |name: &str| it.next().ok_or(format!("{name} needs a value"));
        match a.as_str() {
            "--server" => server = Some(val("--server")?),
            "--world" => world = Some(val("--world")?),
            "--email" => email = Some(val("--email")?),
            "--floor" => floor = Some(val("--floor")?),
            "--ppc" => {
                ppc = val("--ppc")?
                    .parse()
                    .map_err(|_| "--ppc must be a number")?
            }
            "--format" => format = val("--format")?,
            "--data-dir" => data_dir = Some(PathBuf::from(val("--data-dir")?)),
            "--keep-overlay" => keep_overlay = true,
            "--all-floors" => all_floors = true,
            "--audience" => {
                audience =
                    Audience::parse(&val("--audience")?).ok_or("--audience must be gm or player")?
            }
            "--max-mb" => {
                max_mb = val("--max-mb")?
                    .parse()
                    .map_err(|_| "--max-mb must be a number")?
            }
            other if !other.starts_with("--") && doc.is_none() => doc = Some(other.to_string()),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    let data_dir = data_dir
        .or_else(|| std::env::var("ND_CARTO_DATA_DIR").ok().map(PathBuf::from))
        .ok_or("--data-dir (or ND_CARTO_DATA_DIR) is required")?;
    let format = tiles::ImageFormat::parse(&format).ok_or("--format must be webp, avif or png")?;
    if all_floors && floor.is_some() {
        return Err("--all-floors and --floor exclude each other".into());
    }
    Ok(Args {
        all_floors,
        doc: doc.ok_or("name the document (id or exact name)")?,
        server: server.ok_or("--server is required")?,
        world,
        email: email.ok_or("--email is required")?,
        floor,
        ppc,
        format,
        data_dir,
        max_mb,
        audience,
        keep_overlay,
    })
}

fn password() -> Result<String, String> {
    if let Ok(p) = std::env::var("ND_CARTO_PASSWORD") {
        return Ok(p);
    }
    if let Ok(f) = std::env::var("ND_CARTO_PASSWORD_FILE") {
        return std::fs::read_to_string(&f)
            .map(|s| s.trim_end_matches(['\r', '\n']).to_string())
            .map_err(|e| format!("cannot read ND_CARTO_PASSWORD_FILE: {e}"));
    }
    Err("set ND_CARTO_PASSWORD (or ND_CARTO_PASSWORD_FILE) in your own shell".into())
}

/// nd-world has no floors: with several, each map is named "Doc – Floor".
fn map_name(doc: &store::StoredDoc, floor_name: &str) -> String {
    if doc.floors.len() > 1 {
        format!("{} – {}", doc.manifest.name, floor_name)
    } else {
        doc.manifest.name.clone()
    }
}

/// Build what nd-world needs from a stored document. Pure (no network) so the
/// picture, walls, lights and rooms are testable.
pub fn build_map(
    doc: &store::StoredDoc,
    floor_id: Option<&str>,
    ppc: i64,
    format: tiles::ImageFormat,
    audience: Audience,
) -> Result<PublishMap, String> {
    let floor = match floor_id {
        Some(id) => doc
            .floors
            .iter()
            .find(|f| f.id == id)
            .ok_or(format!("no floor {id}"))?,
        None => doc.floors.first().ok_or("document has no floors")?,
    };
    let floor_name = floor.name.clone();
    // Everything below is built from the audience's view of the floor: the
    // picture, walls, lights AND the room state (which carries edge marks).
    let filtered = for_audience(floor, audience);
    let floor = &filtered;
    let cell_size = doc.manifest.cell_size;
    let walls = tiles::derive_walls(floor, cell_size);
    let (image, w, h) = tiles::bake_full_as(floor, &walls, cell_size, ppc, format, 80)?;
    let ppc_f = ppc as f64;
    Ok(PublishMap {
        name: map_name(doc, &floor_name),
        canvas_w: w,
        canvas_h: h,
        cell_px: ppc_f,
        image,
        image_ext: format.ext().to_string(),
        // Walls and lights are in picture pixels: re-derive at the picture's scale.
        walls: tiles::derive_walls(floor, ppc_f),
        lights: floor
            .lights
            .iter()
            .map(|l| {
                let mut l = l.clone();
                let k = ppc_f / cell_size;
                l.x *= k;
                l.y *= k;
                l
            })
            .collect(),
        rooms: Some(rooms::to_nd_world(&floor.grid, ppc_f)?),
    })
}

/// Build the map, stepping the resolution down (by a quarter each time, never
/// below 8 px per square) until the picture fits under `cap_bytes`. Returns the
/// map and the pixels-per-square it ended at. The lattice, walls, lights and
/// rooms are all re-derived at the new scale, so they stay aligned.
pub fn build_map_fitted(
    doc: &store::StoredDoc,
    floor_id: Option<&str>,
    ppc: i64,
    format: tiles::ImageFormat,
    cap_bytes: usize,
    audience: Audience,
) -> Result<(PublishMap, i64), String> {
    let mut ppc = ppc;
    loop {
        let map = build_map(doc, floor_id, ppc, format, audience)?;
        if map.image.len() <= cap_bytes {
            return Ok((map, ppc));
        }
        if ppc <= MIN_PPC {
            return Err(format!(
                "even at {MIN_PPC} px per square the picture is {} bytes, over the {} MB cap; use --format avif or raise max_upload_mb in nd-world.",
                map.image.len(),
                cap_bytes / 1_048_576
            ));
        }
        ppc = (ppc * 3 / 4).max(MIN_PPC);
    }
}

/// A world map for nd-world's image-map viewer. The picture is the whole baked
/// floor; markers and regions become the viewer's overlay JSON in Leaflet
/// CRS.Simple (`lat` counts up from the bottom of the picture, `lng` along it).
/// Routes are in the picture only: nd-world has no route overlay.
pub fn build_region(
    doc: &store::StoredDoc,
    floor_id: Option<&str>,
    ppc: i64,
    format: tiles::ImageFormat,
    replace_overlay: bool,
    audience: Audience,
) -> Result<PublishRegion, String> {
    let floor = match floor_id {
        Some(id) => doc
            .floors
            .iter()
            .find(|f| f.id == id)
            .ok_or(format!("no floor {id}"))?,
        None => doc.floors.first().ok_or("document has no floors")?,
    };
    let floor_name = floor.name.clone();
    let filtered = for_audience(floor, audience);
    let floor = &filtered;
    let cell_size = doc.manifest.cell_size;
    let walls = tiles::derive_walls(floor, cell_size);
    let (image, w, h) = tiles::bake_full_as(floor, &walls, cell_size, ppc, format, 80)?;
    let s = ppc as f64;
    let (hf, round) = (f64::from(h), |v: f64| (v * 1000.0).round() / 1000.0);
    let ll = |x: f32, y: f32| (round(hf - f64::from(y) * s), round(f64::from(x) * s)); // (lat, lng)
    Ok(PublishRegion {
        name: map_name(doc, &floor_name),
        width_px: w,
        height_px: h,
        image,
        image_ext: format.ext().to_string(),
        markers: floor
            .markers
            .iter()
            .map(|m| {
                let (lat, lng) = ll(m.x, m.y);
                let mut v = serde_json::json!({
                    "lat": lat, "lng": lng, "label": m.label, "note": m.note, "color": m.color,
                });
                if let Some(link) = &m.link_schematic {
                    v["link_schematic"] = serde_json::json!(link);
                }
                v
            })
            .collect(),
        regions: floor
            .regions
            .iter()
            .map(|r| {
                serde_json::json!({
                    "points": r.points.iter().map(|p| { let (lat, lng) = ll(p[0], p[1]); [lat, lng] }).collect::<Vec<_>>(),
                    "name": r.name, "color": r.color, "opacity": r.opacity,
                })
            })
            .collect(),
        replace_overlay,
    })
}

/// Like [`build_map_fitted`] for a world map: step the resolution down until the
/// picture fits under the upload cap.
pub fn build_region_fitted(
    doc: &store::StoredDoc,
    floor_id: Option<&str>,
    ppc: i64,
    format: tiles::ImageFormat,
    cap_bytes: usize,
    replace_overlay: bool,
    audience: Audience,
) -> Result<(PublishRegion, i64), String> {
    let mut ppc = ppc;
    loop {
        let map = build_region(doc, floor_id, ppc, format, replace_overlay, audience)?;
        if map.image.len() <= cap_bytes {
            return Ok((map, ppc));
        }
        if ppc <= 1 {
            return Err(format!(
                "even at 1 px per cell the picture is {} bytes, over the {} MB cap; use --format avif or raise max_upload_mb in nd-world.",
                map.image.len(),
                cap_bytes / 1_048_576
            ));
        }
        ppc = (ppc * 3 / 4).max(1);
    }
}

enum Payload {
    Battle(PublishMap),
    Region(PublishRegion),
}

pub async fn run(args: Vec<String>) -> Result<(), String> {
    let a = parse(args)?;
    let mut st = store::Store::default();
    st.load_dir(&a.data_dir).map_err(|e| e.to_string())?;
    let id = st
        .docs
        .values()
        .find(|d| d.manifest.id == a.doc || d.manifest.name == a.doc)
        .map(|d| d.manifest.id.clone())
        .ok_or(format!(
            "no document {:?} in {}",
            a.doc,
            a.data_dir.display()
        ))?;
    let is_region = st.docs[&id].manifest.kind == ndcart_core::DocKind::Region;
    // Which floors go out: all of them, the named one, or the first.
    let targets: Vec<String> = {
        let doc = &st.docs[&id];
        if a.all_floors {
            doc.floors.iter().map(|f| f.id.clone()).collect()
        } else {
            let f = match a.floor.as_deref() {
                Some(f) => doc.floors.iter().find(|x| x.id == f),
                None => doc.floors.first(),
            }
            .ok_or("no such floor")?;
            vec![f.id.clone()]
        }
    };
    let first_floor = st.docs[&id].floors.first().map(|f| f.id.clone());
    let cap = a.max_mb * 1_048_576;

    // Build everything before any network call, so a bad floor fails the run
    // before the first map is touched.
    let mut jobs: Vec<(String, Option<String>, Payload)> = Vec::new();
    for fid in &targets {
        let doc = &st.docs[&id];
        let floor = doc.floors.iter().find(|x| &x.id == fid).expect("floor");
        let problems = ndcart_core::lint(floor, doc.manifest.cell_size);
        for p in &problems {
            println!(
                "{} [{}]: {:?}: {}",
                doc.manifest.name, floor.name, p.severity, p.message
            );
        }
        if ndcart_core::blocks_publish(&problems) {
            return Err(format!(
                "fix the errors above first (nd-world would refuse floor '{}')",
                floor.name
            ));
        }
        let link = doc
            .manifest
            .nd_world
            .as_ref()
            .filter(|l| l.server == a.server && l.world == a.world);
        // The legacy single slug belongs to the first floor.
        let existing = link.and_then(|l| {
            l.floor_slugs
                .get(fid)
                .cloned()
                .or_else(|| (Some(fid) == first_floor.as_ref()).then(|| l.slug.clone()))
        });
        let (payload, used_ppc) = if is_region {
            let replace = !(a.keep_overlay && existing.is_some());
            let (m, p) =
                build_region_fitted(doc, Some(fid), a.ppc, a.format, cap, replace, a.audience)?;
            if existing.is_some() && replace {
                println!("note: this replaces the map's markers and regions in nd-world (hand-made ones are not kept; use --keep-overlay to leave them)");
            }
            (Payload::Region(m), p)
        } else {
            let (m, p) = build_map_fitted(doc, Some(fid), a.ppc, a.format, cap, a.audience)?;
            (Payload::Battle(m), p)
        };
        if used_ppc != a.ppc {
            println!(
                "note: lowered '{}' from {} to {used_ppc} px per square to fit the {} MB upload cap",
                floor.name, a.ppc, a.max_mb
            );
        }
        jobs.push((fid.clone(), existing, payload));
    }

    let pw = password()?;
    let totp = std::env::var("ND_CARTO_TOTP").ok();
    let bridge = Bridge::new(&a.server, a.world.as_deref()).map_err(|e| e.to_string())?;
    let probe = bridge.probe().await.map_err(|e| e.to_string())?;
    println!("nd-world build {} ({} mode)", probe.build, probe.mode);
    if !probe.read_back {
        println!("note: this image cannot hand a map's walls/rooms back (Rust-only); the studio keeps its own copy, so publishing is unaffected");
    }
    bridge
        .login(&a.email, &pw, totp.as_deref())
        .await
        .map_err(|e| e.to_string())?;

    let mut failed: Option<String> = None;
    for (fid, existing, payload) in &jobs {
        let outcome = match payload {
            Payload::Battle(m) => bridge.publish(m, existing.as_deref()).await,
            Payload::Region(m) => bridge.publish_region(m, existing.as_deref()).await,
        };
        let (slug, report) = match outcome {
            Ok(r) => (r.slug.clone(), Some(r)),
            Err(BridgeError::Partial { slug, step, cause }) => {
                // The map exists: remember it so the next run finishes this
                // one instead of creating a duplicate.
                failed = Some(format!(
                    "map '{slug}' was created but the {step} step failed: {cause}. Run publish again to finish it."
                ));
                (slug, None)
            }
            Err(e) => {
                failed = Some(e.to_string());
                break;
            }
        };
        // Save after every floor: a later failure must not lose earlier slugs.
        let doc = st.docs.get_mut(&id).expect("doc");
        let mut link = doc
            .manifest
            .nd_world
            .clone()
            .filter(|l| l.server == a.server && l.world == a.world)
            .unwrap_or(NdWorldLink {
                server: a.server.clone(),
                world: a.world.clone(),
                slug: slug.clone(),
                floor_slugs: Default::default(),
            });
        if Some(fid) == first_floor.as_ref() {
            link.slug = slug.clone();
        }
        link.floor_slugs.insert(fid.clone(), slug.clone());
        doc.manifest.nd_world = Some(link);
        store::save_doc(Some(&a.data_dir), doc);
        if let Some(report) = report {
            let name = doc
                .floors
                .iter()
                .find(|f| &f.id == fid)
                .map(|f| f.name.as_str())
                .unwrap_or("");
            println!(
                "{} '{}' ({}) at {}/maps/{}{}",
                if report.created {
                    "published"
                } else {
                    "updated"
                },
                doc.manifest.name,
                name,
                a.server.trim_end_matches('/'),
                if is_region { "" } else { "schematic/" },
                report.slug
            );
            for w in &report.warnings {
                println!("warning: {w}");
            }
        }
        if failed.is_some() {
            break;
        }
    }
    match failed {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ndcart_core::journal::Op;
    use ndcart_core::DocKind;

    fn doc() -> store::StoredDoc {
        let mut d = store::StoredDoc::new("Vault", DocKind::Battle);
        let fid = d.floors[0].id.clone();
        let ops: Vec<Op> = serde_json::from_value(serde_json::json!([
            {"t":"upsert_space","space":{"id":"s","name":"Hall","color":"#7f5539","kind":"room"}},
            {"t":"paint_cells","cells":[{"x":0,"y":0},{"x":1,"y":0},{"x":1,"y":1}],"space":"s"},
            {"t":"add_light","light":{"id":"l","x":32.0,"y":32.0,"range":4.0,"color":"#ffaa00","intensity":0.9,"on":true}}
        ]))
        .unwrap();
        d.apply_ops(&fid, &ops).unwrap();
        d
    }

    #[test]
    fn each_floor_of_a_multi_floor_document_is_its_own_named_map() {
        let mut d = doc();
        assert_eq!(
            build_map(&d, None, 16, tiles::ImageFormat::Png, Audience::Gm)
                .unwrap()
                .name,
            "Vault"
        );
        let up = d.add_floor("Upper", None).unwrap();
        let m0 = build_map(&d, None, 16, tiles::ImageFormat::Png, Audience::Gm).unwrap();
        let m1 = build_map(&d, Some(&up), 16, tiles::ImageFormat::Png, Audience::Gm);
        assert_eq!(m0.name, "Vault \u{2013} Ground");
        // The empty upper floor has no picture to publish, and says so.
        assert!(m1.is_err() || m1.unwrap().name == "Vault \u{2013} Upper");
    }

    #[test]
    fn all_floors_and_one_floor_exclude_each_other() {
        let args = |v: &[&str]| parse(v.iter().map(|s| s.to_string()).collect());
        let base = [
            "d",
            "--server",
            "http://x",
            "--email",
            "e",
            "--data-dir",
            ".",
        ];
        let mut both = base.to_vec();
        both.extend(["--all-floors", "--floor", "f"]);
        assert!(args(&both).is_err());
        let mut all = base.to_vec();
        all.push("--all-floors");
        assert!(args(&all).unwrap().all_floors);
    }

    #[test]
    fn map_is_in_picture_pixels_and_matches_the_canvas() {
        let m = build_map(&doc(), None, 16, tiles::ImageFormat::Webp, Audience::Gm).unwrap();
        // 2x2 cells at 16 px.
        assert_eq!((m.canvas_w, m.canvas_h), (32, 32));
        assert_eq!(m.cell_px, 16.0);
        assert_eq!(&m.image[8..12], b"WEBP");
        // The light was at 1 cell (32 px lattice) -> 16 px in the picture.
        assert_eq!((m.lights[0].x, m.lights[0].y), (16.0, 16.0));
        // Every wall point lies on the canvas.
        for w in &m.walls {
            for p in &w.pts {
                assert!(
                    p[0] >= 0.0 && p[0] <= 32.0 && p[1] >= 0.0 && p[1] <= 32.0,
                    "{p:?}"
                );
            }
        }
        assert_eq!(m.rooms.as_ref().unwrap()["cols"], 2);
    }

    #[test]
    fn a_player_publish_carries_no_secret_in_walls_or_rooms() {
        let mut d = doc();
        let fid = d.floors[0].id.clone();
        let ops: Vec<Op> = serde_json::from_value(serde_json::json!([
            {"t":"upsert_space","space":{"id":"v","name":"Vault","color":"#5a7d9a","kind":"room"}},
            {"t":"paint_cells","cells":[{"x":2,"y":0}],"space":"v"},
            {"t":"set_mark","edge":{"cell":{"x":1,"y":0},"edge":"east"},"mark":{"kind":"secret","state":"closed"}}
        ]))
        .unwrap();
        d.apply_ops(&fid, &ops).unwrap();
        let gm = build_map(&d, None, 16, tiles::ImageFormat::Png, Audience::Gm).unwrap();
        let pl = build_map(&d, None, 16, tiles::ImageFormat::Png, Audience::Player).unwrap();
        let payload =
            |m: &PublishMap| serde_json::json!({"walls": m.walls, "rooms": m.rooms}).to_string();
        assert!(
            payload(&gm).contains("secret"),
            "the GM publish keeps the secret door"
        );
        assert!(!payload(&pl).contains("secret"), "{}", payload(&pl));
        assert_ne!(
            gm.image, pl.image,
            "the picture differs too (the door is drawn)"
        );
    }

    #[test]
    fn the_picture_is_fitted_under_the_cap_and_stays_aligned() {
        let d = doc();
        let big = build_map(&d, None, 64, tiles::ImageFormat::Png, Audience::Gm).unwrap();
        let cap = big.image.len() / 2;
        let (m, ppc) =
            build_map_fitted(&d, None, 64, tiles::ImageFormat::Png, cap, Audience::Gm).unwrap();
        assert!((MIN_PPC..64).contains(&ppc), "{ppc}");
        assert!(m.image.len() <= cap);
        // Everything was re-derived at the new scale: canvas, cell, light, walls.
        assert_eq!(m.cell_px, ppc as f64);
        assert_eq!(m.canvas_w as i64, 2 * ppc);
        assert_eq!(m.lights[0].x, ppc as f64); // the light sat at 1 cell
        for w in &m.walls {
            for p in &w.pts {
                assert!(p[0] <= m.canvas_w as f64 && p[1] <= m.canvas_h as f64);
            }
        }
        // An impossible cap is refused with advice, not looped on.
        let e = build_map_fitted(&d, None, 64, tiles::ImageFormat::Png, 10, Audience::Gm)
            .err()
            .unwrap();
        assert!(e.contains("avif") && e.contains("max_upload_mb"), "{e}");
        // A roomy cap changes nothing.
        let (_, same) = build_map_fitted(
            &d,
            None,
            64,
            tiles::ImageFormat::Png,
            usize::MAX,
            Audience::Gm,
        )
        .unwrap();
        assert_eq!(same, 64);
    }

    #[test]
    fn strokes_reach_the_published_picture_but_not_the_walls_or_rooms() {
        let plain = doc();
        let mut painted = doc();
        let fid = painted.floors[0].id.clone();
        let ops: Vec<Op> = serde_json::from_value(serde_json::json!([
            {"t":"add_stroke","stroke":{"id":"s1","kind":"paint","brush":"round","material":"grass",
             "color":"#5a8a3c","size":1.5,"opacity":1,"seed":3,
             "points":[{"x":0.2,"y":0.5},{"x":1.8,"y":0.5}]}}
        ]))
        .unwrap();
        painted.apply_ops(&fid, &ops).unwrap();
        let a = build_map(&plain, None, 16, tiles::ImageFormat::Png, Audience::Gm).unwrap();
        let b = build_map(&painted, None, 16, tiles::ImageFormat::Png, Audience::Gm).unwrap();
        assert_ne!(a.image, b.image, "the stroke is in the picture");
        // Strokes are cosmetic: geometry nd-world stores is identical.
        // (Wall ids are fresh on every derivation, so compare geometry without them.)
        let geom = |m: &PublishMap| {
            let walls: Vec<_> = m
                .walls
                .iter()
                .map(|w| (w.kind, w.pts.clone(), w.state))
                .collect();
            serde_json::json!({"w": format!("{walls:?}"), "l": m.lights, "r": m.rooms})
        };
        assert_eq!(geom(&a), geom(&b));
        // The player copy keeps strokes (they are the look), as the filter only removes secrets.
        let p = build_map(
            &painted,
            None,
            16,
            tiles::ImageFormat::Png,
            Audience::Player,
        )
        .unwrap();
        assert_eq!(p.image, b.image);
    }

    fn world_doc() -> store::StoredDoc {
        let mut d = store::StoredDoc::new("Atlas", DocKind::Region);
        let fid = d.floors[0].id.clone();
        let ops: Vec<Op> = serde_json::from_value(serde_json::json!([
            {"t":"set_canvas","canvas":[20,10]},
            {"t":"terrain_stroke","stroke":{"mode":"raise","points":[[10.0,5.0]],"radius":6.0,"strength":1.0,"seed":1}},
            {"t":"put_overlay","item":{"kind":"marker","id":"m1","x":5.0,"y":2.0,"label":"Port Neon","note":"smugglers","color":"#e8b030","link_schematic":"harbour"}},
            {"t":"put_overlay","item":{"kind":"marker","id":"m2","x":15.5,"y":7.25,"label":"Ruins"}},
            {"t":"put_overlay","item":{"kind":"region","id":"r1","name":"Kingdom","points":[[0,0],[10,0],[10,5]],"color":"#c0504d","opacity":0.3}},
            {"t":"put_overlay","item":{"kind":"route","id":"t1","name":"Road","points":[[1,1],[9,4]]}}
        ]))
        .unwrap();
        d.apply_ops(&fid, &ops).unwrap();
        d
    }

    #[test]
    fn a_world_map_becomes_an_image_map_with_leaflet_coordinates() {
        let m = build_region(
            &world_doc(),
            None,
            16,
            tiles::ImageFormat::Png,
            true,
            Audience::Gm,
        )
        .unwrap();
        assert_eq!(
            (m.width_px, m.height_px),
            (320, 160),
            "the canvas at 16 px per cell"
        );
        assert_eq!(&m.image[1..4], b"PNG");
        // lat counts UP from the bottom: y = 2 cells of 10 -> 160 - 2*16; lng is x in pixels.
        assert_eq!(m.markers[0]["lat"], 128.0);
        assert_eq!(m.markers[0]["lng"], 80.0);
        assert_eq!(m.markers[0]["label"], "Port Neon");
        assert_eq!(
            m.markers[0]["link_schematic"], "harbour",
            "a link to a battle map rides along"
        );
        assert_eq!(
            (
                m.markers[1]["lat"].as_f64().unwrap(),
                m.markers[1]["lng"].as_f64().unwrap()
            ),
            (160.0 - 7.25 * 16.0, 15.5 * 16.0)
        );
        assert!(
            m.markers[1].get("link_schematic").is_none(),
            "no link, no field"
        );
        // A region: [lat, lng] pairs, name, colour, opacity.
        assert_eq!(m.regions.len(), 1);
        assert_eq!(
            m.regions[0]["points"],
            serde_json::json!([[160.0, 0.0], [160.0, 160.0], [80.0, 160.0]])
        );
        assert_eq!(
            (
                m.regions[0]["name"].as_str(),
                m.regions[0]["opacity"].as_f64()
            ),
            (Some("Kingdom"), Some(f64::from(0.3f32)))
        );
        // Routes are baked into the picture; nd-world has no route overlay to send them to.
        assert!(!serde_json::to_string(&m.markers).unwrap().contains("Road"));
        assert!(!serde_json::to_string(&m.regions).unwrap().contains("Road"));
        // Scaling the picture scales the coordinates with it.
        let big = build_region(
            &world_doc(),
            None,
            32,
            tiles::ImageFormat::Png,
            true,
            Audience::Gm,
        )
        .unwrap();
        assert_eq!(big.markers[0]["lng"], 160.0);
        assert_eq!(big.markers[0]["lat"], 320.0 - 64.0);
    }

    #[test]
    fn a_world_map_picture_is_fitted_under_the_cap_and_the_flag_parses() {
        let d = world_doc();
        let big = build_region(&d, None, 32, tiles::ImageFormat::Png, true, Audience::Gm).unwrap();
        let (m, ppc) = build_region_fitted(
            &d,
            None,
            32,
            tiles::ImageFormat::Png,
            big.image.len() / 2,
            true,
            Audience::Gm,
        )
        .unwrap();
        assert!(ppc < 32 && m.image.len() <= big.image.len() / 2, "{ppc}");
        // Coordinates follow the fitted size.
        assert_eq!(m.markers[0]["lng"], 5.0 * ppc as f64);
        assert!(build_region_fitted(
            &d,
            None,
            32,
            tiles::ImageFormat::Png,
            10,
            true,
            Audience::Gm
        )
        .is_err());
        let a = parse(
            [
                "Atlas",
                "--server",
                "http://x",
                "--email",
                "a@b.c",
                "--data-dir",
                "/d",
                "--keep-overlay",
            ]
            .map(String::from)
            .to_vec(),
        )
        .unwrap();
        assert!(a.keep_overlay);
    }

    #[test]
    fn password_never_comes_from_arguments() {
        let a = parse(
            [
                "Vault",
                "--server",
                "http://x",
                "--email",
                "a@b.c",
                "--data-dir",
                "/d",
            ]
            .map(String::from)
            .to_vec(),
        )
        .unwrap();
        assert_eq!(a.email, "a@b.c");
        let e = parse(
            ["Vault", "--password", "hunter2"]
                .map(String::from)
                .to_vec(),
        )
        .err();
        assert!(e.unwrap().contains("unknown argument"));
    }
}
