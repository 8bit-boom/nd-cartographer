//! WorldSpec: an overland map described in words-as-numbers (plan §8): how much
//! land, how mountainous, how many rivers, where the forests and swamps are,
//! which named places sit where. The terrain, rivers and scatter are our own
//! deterministic code; the spec is what an AI (or a person) writes and what is
//! validated before anything is drawn. All coordinates are 0..1 across the map.

use ndcart_core::overlay::{Marker, OverlayItem, Region};
use ndcart_core::scatter;
use ndcart_core::{CoreError, CoreResult, Op};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct WorldSpec {
    pub name: String,
    #[serde(default = "d_cols")]
    pub cols: u32,
    #[serde(default = "d_rows")]
    pub rows: u32,
    #[serde(default = "d_land")]
    pub land: f32,
    #[serde(default = "d_cont")]
    pub continent: f32,
    #[serde(default = "d_ridges")]
    pub ridges: f32,
    #[serde(default = "d_rivers")]
    pub rivers: u32,
    /// Where each biome grows (an ellipse: centre and radii, 0..1). Props
    /// only land on dry ground, so a forest mask half over the sea is half a forest.
    #[serde(default)]
    pub biomes: Vec<BiomeMask>,
    #[serde(default)]
    pub places: Vec<Place>,
    /// Named areas (kingdoms, wilds): polygons of 3+ points.
    #[serde(default)]
    pub regions: Vec<RegionSpec>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct BiomeMask {
    pub biome: String,
    pub x: f32,
    pub y: f32,
    pub rx: f32,
    pub ry: f32,
    #[serde(default = "one")]
    pub density: f32,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Place {
    pub name: String,
    #[serde(default)]
    pub note: String,
    pub x: f32,
    pub y: f32,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct RegionSpec {
    pub name: String,
    pub points: Vec<[f32; 2]>,
    #[serde(default)]
    pub color: Option<String>,
}

fn d_cols() -> u32 {
    160
}
fn d_rows() -> u32 {
    110
}
fn d_land() -> f32 {
    0.42
}
fn d_cont() -> f32 {
    0.6
}
fn d_ridges() -> f32 {
    0.6
}
fn d_rivers() -> u32 {
    7
}
fn one() -> f32 {
    1.0
}

const REGION_COLOURS: [&str; 5] = ["#c0504d", "#4f81bd", "#9bbb59", "#8064a2", "#f79646"];

impl WorldSpec {
    /// Every rule a spec must meet; the messages are written to be handed
    /// straight back to an AI that produced a bad one.
    pub fn validate(&self) -> CoreResult<()> {
        let bad = |m: String| Err(CoreError::Invalid(m));
        let unit = |v: f32| v.is_finite() && (0.0..=1.0).contains(&v);
        let n = self.name.chars().count();
        if n == 0 || n > 80 {
            return bad("name must be 1-80 characters".into());
        }
        if !(16..=512).contains(&self.cols) || !(16..=512).contains(&self.rows) {
            return bad("cols and rows must be 16-512".into());
        }
        if !(0.05..=0.95).contains(&self.land) {
            return bad("land must be 0.05-0.95 (the fraction that is dry)".into());
        }
        if !unit(self.continent) || !unit(self.ridges) {
            return bad("continent and ridges must be 0-1".into());
        }
        if self.rivers > 40 {
            return bad("rivers must be 0-40".into());
        }
        if self.biomes.len() > 30 || self.places.len() > 60 || self.regions.len() > 20 {
            return bad("at most 30 biomes, 60 places and 20 regions".into());
        }
        for b in &self.biomes {
            if scatter::biome(&b.biome).is_none() {
                let names: Vec<&str> = scatter::BIOMES.iter().map(|x| x.name).collect();
                return bad(format!(
                    "unknown biome '{}'; use one of {}",
                    b.biome,
                    names.join(", ")
                ));
            }
            if !unit(b.x)
                || !unit(b.y)
                || !(0.02..=0.5).contains(&b.rx)
                || !(0.02..=0.5).contains(&b.ry)
            {
                return bad(format!(
                    "biome '{}': x and y must be 0-1 and rx, ry 0.02-0.5",
                    b.biome
                ));
            }
            if !(0.5..=4.0).contains(&b.density) {
                return bad("biome density must be 0.5-4 (higher = sparser)".into());
            }
        }
        for p in &self.places {
            let len = p.name.chars().count();
            if len == 0 || len > 80 || !unit(p.x) || !unit(p.y) {
                return bad(format!(
                    "place '{}': name 1-80 characters, x and y 0-1",
                    p.name
                ));
            }
            if p.note.chars().count() > 500 {
                return bad("a place note is at most 500 characters".into());
            }
        }
        for r in &self.regions {
            let len = r.name.chars().count();
            if len == 0 || len > 80 || r.points.len() < 3 || r.points.len() > 100 {
                return bad(format!(
                    "region '{}': name 1-80 characters and 3-100 points",
                    r.name
                ));
            }
            if r.points.iter().any(|p| !unit(p[0]) || !unit(p[1])) {
                return bad(format!("region '{}': points must be 0-1", r.name));
            }
            if let Some(c) = &r.color {
                let b = c.as_bytes();
                if !(b.len() == 7 && b[0] == b'#' && b[1..].iter().all(u8::is_ascii_hexdigit)) {
                    return bad(format!("region '{}': color must be #rrggbb", r.name));
                }
            }
        }
        Ok(())
    }
}

/// The ops that draw this world onto a floor (replacing its terrain and
/// rivers): one batch, so it is one undo step. `old_rivers` are removed first.
pub fn compile(spec: &WorldSpec, seed: u32, old_rivers: &[String]) -> CoreResult<Vec<Op>> {
    spec.validate()?;
    let world = ndcart_terrain::generate(&ndcart_terrain::GenParams {
        seed,
        cols: spec.cols,
        rows: spec.rows,
        land: spec.land,
        continent: spec.continent,
        ridges: spec.ridges,
        rivers: spec.rivers,
    });
    let t = &world.terrain;
    let (cw, ch) = (spec.cols as f32, spec.rows as f32);
    // Dry ground: samples are 4 per cell.
    let sea = t.sea_level();
    let land = |x: f32, y: f32| t.get((x * 4.0) as i32, (y * 4.0) as i32) > sea;
    let mut ops: Vec<Op> = old_rivers
        .iter()
        .map(|id| Op::RemoveRiver { id: id.clone() })
        .collect();
    ops.push(Op::ClearTerrain);
    ops.push(Op::SetCanvas {
        canvas: Some([spec.cols, spec.rows]),
    });
    ops.push(Op::TerrainSet {
        fill: t.fill,
        chunks: t
            .chunk_keys()
            .into_iter()
            .map(|(cx, cy)| t.snapshot_chunk(cx, cy))
            .collect(),
    });
    ops.push(Op::SetTerrainStyle {
        style: t.style.clone(),
    });
    ops.extend(world.rivers.into_iter().map(|river| Op::AddRiver {
        river,
        carve: false,
        index: None,
    }));

    // Biome masks: props on dry ground inside each ellipse.
    let mut placed = 0usize;
    for (i, m) in spec.biomes.iter().enumerate() {
        let b = scatter::biome(&m.biome).expect("validated");
        let poly: Vec<[f32; 2]> = (0..24)
            .map(|k| {
                // A 24-gon; the angle table avoids sin/cos so it is identical everywhere.
                let (c, s) = UNIT24[k];
                [(m.x + m.rx * c) * cw, (m.y + m.ry * s) * ch]
            })
            .collect();
        match scatter::scatter_biome_where(
            &poly,
            b,
            m.density,
            u64::from(seed) * 31 + i as u64,
            placed,
            false,
            &land,
        ) {
            Ok(mut one) => {
                if let Some(Op::Batch { ops: inner }) = one.pop() {
                    placed += inner.len();
                    ops.extend(inner);
                }
            }
            // A mask entirely over water places nothing; that is not an error.
            Err(CoreError::Invalid(m)) if m.contains("nothing fitted") => {}
            Err(e) => return Err(e),
        }
    }
    // Named places: markers, moved to the nearest dry ground if the spec put them in the sea.
    for (i, p) in spec.places.iter().enumerate() {
        let (mut x, mut y) = (p.x * cw, p.y * ch);
        if !land(x, y) {
            let mut best: Option<(f32, f32)> = None;
            'ring: for r in 1..=12 {
                for k in 0..16 {
                    let (c, s) = UNIT24[k * 24 / 16];
                    let (cx, cy) = (x + c * r as f32, y + s * r as f32);
                    if cx >= 0.0 && cy >= 0.0 && cx < cw && cy < ch && land(cx, cy) {
                        best = Some((cx, cy));
                        break 'ring;
                    }
                }
            }
            let Some((nx, ny)) = best else {
                return Err(CoreError::Invalid(format!(
                    "place '{}' is out at sea; move it onto land (try x, y closer to the middle)",
                    p.name
                )));
            };
            (x, y) = (nx, ny);
        }
        ops.push(Op::PutOverlay {
            item: OverlayItem::Marker(Marker {
                id: format!("pl{i}"),
                x,
                y,
                label: p.name.clone(),
                note: p.note.clone(),
                color: "#e8b030".into(),
                link_schematic: None,
                links_to: None,
            }),
            index: None,
        });
    }
    for (i, r) in spec.regions.iter().enumerate() {
        ops.push(Op::PutOverlay {
            item: OverlayItem::Region(Region {
                id: format!("rg{i}"),
                name: r.name.clone(),
                points: r.points.iter().map(|p| [p[0] * cw, p[1] * ch]).collect(),
                color: r
                    .color
                    .clone()
                    .unwrap_or_else(|| REGION_COLOURS[i % REGION_COLOURS.len()].to_string()),
                opacity: 0.22,
                hatch: false,
            }),
            index: None,
        });
    }
    Ok(vec![Op::Batch { ops }])
}

/// cos/sin of k * 15 degrees, k = 0..24, written out so no trig runs.
const UNIT24: [(f32, f32); 24] = [
    (1.0, 0.0),
    (0.965_925_8, 0.258_819_04),
    (0.866_025_4, 0.5),
    (
        std::f32::consts::FRAC_1_SQRT_2,
        std::f32::consts::FRAC_1_SQRT_2,
    ),
    (0.5, 0.866_025_4),
    (0.258_819_04, 0.965_925_8),
    (0.0, 1.0),
    (-0.258_819_04, 0.965_925_8),
    (-0.5, 0.866_025_4),
    (
        -std::f32::consts::FRAC_1_SQRT_2,
        std::f32::consts::FRAC_1_SQRT_2,
    ),
    (-0.866_025_4, 0.5),
    (-0.965_925_8, 0.258_819_04),
    (-1.0, 0.0),
    (-0.965_925_8, -0.258_819_04),
    (-0.866_025_4, -0.5),
    (
        -std::f32::consts::FRAC_1_SQRT_2,
        -std::f32::consts::FRAC_1_SQRT_2,
    ),
    (-0.5, -0.866_025_4),
    (-0.258_819_04, -0.965_925_8),
    (0.0, -1.0),
    (0.258_819_04, -0.965_925_8),
    (0.5, -0.866_025_4),
    (
        std::f32::consts::FRAC_1_SQRT_2,
        -std::f32::consts::FRAC_1_SQRT_2,
    ),
    (0.866_025_4, -0.5),
    (0.965_925_8, -0.258_819_04),
];

#[cfg(test)]
mod tests {
    use super::*;
    use ndcart_core::{FloorDoc, Journal};

    fn spec() -> WorldSpec {
        serde_json::from_value(serde_json::json!({
            "name": "Cold Archipelago", "cols": 80, "rows": 60, "land": 0.4, "continent": 0.3, "rivers": 4,
            "biomes": [{"biome": "forest", "x": 0.5, "y": 0.5, "rx": 0.25, "ry": 0.2},
                       {"biome": "swamp", "x": 0.25, "y": 0.4, "rx": 0.1, "ry": 0.1, "density": 2.0}],
            "places": [{"name": "Port Neon", "x": 0.5, "y": 0.5, "note": "smugglers"}],
            "regions": [{"name": "The Wilds", "points": [[0.1,0.1],[0.9,0.1],[0.9,0.5]]}]
        }))
        .unwrap()
    }

    fn apply(ops: &[Op]) -> FloorDoc {
        let mut f = FloorDoc::new("f".into(), "World".into());
        let mut j = Journal::default();
        for op in ops {
            j.push(&mut f, op).unwrap();
        }
        f
    }

    #[test]
    fn a_world_spec_draws_terrain_props_on_dry_ground_places_and_regions() {
        let ops = compile(&spec(), 7, &[]).unwrap();
        let f = apply(&ops);
        assert_eq!(f.canvas, Some([80, 60]));
        assert!(f.terrain.is_some() && !f.rivers.is_empty());
        assert!(f.stamps.len() > 20, "{}", f.stamps.len());
        // Every prop stands on land.
        let t = f.terrain.as_ref().unwrap();
        for s in &f.stamps {
            assert!(
                t.get((s.x * 4.0) as i32, (s.y * 4.0) as i32) > t.sea_level(),
                "prop at sea {} {}",
                s.x,
                s.y
            );
        }
        assert_eq!(f.markers.len(), 1);
        let m = &f.markers[0];
        assert!(
            t.get((m.x * 4.0) as i32, (m.y * 4.0) as i32) > t.sea_level(),
            "the port is on land"
        );
        assert_eq!(f.regions[0].name, "The Wilds");
        // Deterministic, and another seed is another world.
        assert_eq!(f, apply(&compile(&spec(), 7, &[]).unwrap()));
        assert_ne!(f.stamps, apply(&compile(&spec(), 8, &[]).unwrap()).stamps);
        // One undo step takes the whole thing back.
        let mut g = FloorDoc::new("g".into(), "W".into());
        let mut j = Journal::default();
        for op in &ops {
            j.push(&mut g, op).unwrap();
        }
        j.undo_last(&mut g).unwrap();
        assert!(g.stamps.is_empty() && g.markers.is_empty() && g.rivers.is_empty());
    }

    #[test]
    fn bad_specs_say_what_to_fix() {
        let msg = |edit: &dyn Fn(&mut WorldSpec)| {
            let mut s = spec();
            edit(&mut s);
            s.validate().unwrap_err().to_string()
        };
        assert!(msg(&|s| s.land = 1.5).contains("land must be"));
        assert!(msg(&|s| s.biomes[0].biome = "lava".into()).contains("use one of forest"));
        assert!(msg(&|s| s.biomes[0].rx = 0.9).contains("rx, ry"));
        assert!(msg(&|s| s.places[0].x = 3.0).contains("0-1"));
        assert!(msg(&|s| s.regions[0].points.truncate(2)).contains("3-100 points"));
        assert!(msg(&|s| s.cols = 4).contains("16-512"));
        // Defaults fill in, and a place far out at sea is refused with a hint.
        let tiny: WorldSpec = serde_json::from_str(r#"{"name":"T"}"#).unwrap();
        tiny.validate().unwrap();
        let mut sea = spec();
        sea.land = 0.06;
        sea.continent = 1.0;
        sea.places[0].x = 0.0;
        sea.places[0].y = 0.0;
        sea.biomes.clear();
        match compile(&sea, 3, &[]) {
            // Moved onto dry ground...
            Ok(ops) => {
                let f = apply(&ops);
                let t = f.terrain.as_ref().unwrap();
                let m = &f.markers[0];
                assert!(t.get((m.x * 4.0) as i32, (m.y * 4.0) as i32) > t.sea_level());
            }
            // ...or refused with a hint when no land is near.
            Err(e) => assert!(e.to_string().contains("out at sea"), "{e}"),
        }
    }
}
