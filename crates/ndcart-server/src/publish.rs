//! `ndcart publish` — publish a stored document to nd-world (plan §9.2 A).
//!
//! ndcart publish DOC --server URL [--world SLUG] --email EMAIL
//!                [--floor ID] [--ppc 32] [--format webp|avif|png] [--data-dir DIR]
//!
//! The password is read from `ND_CARTO_PASSWORD` or the file named by
//! `ND_CARTO_PASSWORD_FILE` — never an argument (it would land in shell history
//! and process lists) and never logged. A two-step code comes from
//! `ND_CARTO_TOTP`. DOC is a document id or an exact name.

use crate::{store, tiles};
use ndcart_bridge::{rooms, Bridge, BridgeError, PublishMap};
use ndcart_core::NdWorldLink;
use std::path::PathBuf;

/// nd-world's default upload cap (`max_upload_mb`, adjustable per instance).
const DEFAULT_UPLOAD_CAP: usize = 20 * 1024 * 1024;

pub struct Args {
    pub doc: String,
    pub server: String,
    pub world: Option<String>,
    pub email: String,
    pub floor: Option<String>,
    pub ppc: i64,
    pub format: tiles::ImageFormat,
    pub data_dir: PathBuf,
}

pub fn parse(args: Vec<String>) -> Result<Args, String> {
    let (mut doc, mut server, mut world, mut email) = (None, None, None, None);
    let (mut floor, mut ppc, mut format, mut data_dir) = (None, 32i64, "webp".to_string(), None);
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
            other if !other.starts_with("--") && doc.is_none() => doc = Some(other.to_string()),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    let data_dir = data_dir
        .or_else(|| std::env::var("ND_CARTO_DATA_DIR").ok().map(PathBuf::from))
        .ok_or("--data-dir (or ND_CARTO_DATA_DIR) is required")?;
    let format = tiles::ImageFormat::parse(&format).ok_or("--format must be webp, avif or png")?;
    Ok(Args {
        doc: doc.ok_or("name the document (id or exact name)")?,
        server: server.ok_or("--server is required")?,
        world,
        email: email.ok_or("--email is required")?,
        floor,
        ppc,
        format,
        data_dir,
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

/// Build what nd-world needs from a stored document. Pure (no network) so the
/// picture, walls, lights and rooms are testable.
pub fn build_map(
    doc: &store::StoredDoc,
    floor_id: Option<&str>,
    ppc: i64,
    format: tiles::ImageFormat,
) -> Result<PublishMap, String> {
    let floor = match floor_id {
        Some(id) => doc
            .floors
            .iter()
            .find(|f| f.id == id)
            .ok_or(format!("no floor {id}"))?,
        None => doc.floors.first().ok_or("document has no floors")?,
    };
    let cell_size = doc.manifest.cell_size;
    let walls = tiles::derive_walls(floor, cell_size);
    let (image, w, h) = tiles::bake_full_as(floor, &walls, cell_size, ppc, format, 80)?;
    let ppc_f = ppc as f64;
    Ok(PublishMap {
        name: doc.manifest.name.clone(),
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
    let map = build_map(&st.docs[&id], a.floor.as_deref(), a.ppc, a.format)?;
    if map.image.len() > DEFAULT_UPLOAD_CAP {
        return Err(format!(
            "the picture is {} MB; nd-world's default upload cap is 20 MB. Lower --ppc or use --format avif (or raise max_upload_mb in nd-world).",
            map.image.len() / 1_048_576
        ));
    }
    let pw = password()?;
    let totp = std::env::var("ND_CARTO_TOTP").ok();
    let existing = st.docs[&id]
        .manifest
        .nd_world
        .as_ref()
        .filter(|l| l.server == a.server && l.world == a.world)
        .map(|l| l.slug.clone());

    let outcome = async {
        let bridge = Bridge::new(&a.server, a.world.as_deref()).map_err(|e| e.to_string())?;
        let probe = bridge.probe().await.map_err(|e| e.to_string())?;
        println!("nd-world build {} ({} mode)", probe.build, probe.mode);
        bridge
            .login(&a.email, &pw, totp.as_deref())
            .await
            .map_err(|e| e.to_string())?;
        Ok::<_, String>(bridge.publish(&map, existing.as_deref()).await)
    }
    .await?;
    let link = |slug: &str| NdWorldLink {
        server: a.server.clone(),
        world: a.world.clone(),
        slug: slug.to_string(),
    };
    let report = match outcome {
        Ok(r) => r,
        Err(BridgeError::Partial { slug, step, cause }) => {
            // The map exists: remember it so the next run finishes this one
            // instead of creating a duplicate.
            let doc = st.docs.get_mut(&id).expect("doc");
            doc.manifest.nd_world = Some(link(&slug));
            store::save_doc(Some(&a.data_dir), doc);
            return Err(format!(
                "map '{slug}' was created but the {step} step failed: {cause}. Run publish again to finish it."
            ));
        }
        Err(e) => return Err(e.to_string()),
    };
    let doc = st.docs.get_mut(&id).expect("doc");
    doc.manifest.nd_world = Some(link(&report.slug));
    store::save_doc(Some(&a.data_dir), doc);
    println!(
        "{} map '{}' at {}/maps/schematic/{}",
        if report.created {
            "published"
        } else {
            "updated"
        },
        doc.manifest.name,
        a.server.trim_end_matches('/'),
        report.slug
    );
    for w in &report.warnings {
        println!("warning: {w}");
    }
    Ok(())
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
    fn map_is_in_picture_pixels_and_matches_the_canvas() {
        let m = build_map(&doc(), None, 16, tiles::ImageFormat::Webp).unwrap();
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
