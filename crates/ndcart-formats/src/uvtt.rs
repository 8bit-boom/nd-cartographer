//! Universal VTT read/write.
//!
//! Mapping (all UVTT geometry is in grid squares; ours is map pixels):
//! - `wall` / `secret` segments -> `line_of_sight` polylines (they block sight)
//! - `door` segments -> `portals` (position = midpoint, bounds = endpoints,
//!   `closed` from the door state)
//! - `window` segments are not sight blockers in UVTT; they ride along in the
//!   `x_ndcart.windows` extension so an ndcart -> ndcart round trip is exact
//!   (other tools ignore unknown keys).
//! - lights: `range` squares, `color` as `aarrggbb`, intensity 0.1..=1.0.
//!
//! Every number read is validated (finite, bounded) — plan §17.7.

use ndcart_core::{Light, Wall, WallKind, WallState};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const UVTT_FORMAT: f64 = 0.3;
const MAX_POINTS: usize = 200_000;
const MAX_CELLS: f64 = 20_000.0;

#[derive(Debug)]
pub enum UvttError {
    Json(String),
    Invalid(String),
}

impl std::fmt::Display for UvttError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UvttError::Json(m) => write!(f, "not valid JSON: {m}"),
            UvttError::Invalid(m) => write!(f, "invalid UVTT: {m}"),
        }
    }
}
impl std::error::Error for UvttError {}

fn bad<T>(m: impl Into<String>) -> Result<T, UvttError> {
    Err(UvttError::Invalid(m.into()))
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
pub struct Pt {
    pub x: f64,
    pub y: f64,
}

/// A map as UVTT sees it, expressed in our units (pixels) on the way out of
/// `from_uvtt` and into `to_uvtt`.
#[derive(Clone, PartialEq, Debug)]
pub struct UvttMap {
    pub cols: u32,
    pub rows: u32,
    pub cell_size: f64,
    pub walls: Vec<Wall>,
    pub lights: Vec<Light>,
    /// Base64 PNG of the map image, passed through untouched.
    pub image_b64: Option<String>,
}

fn finite(v: f64, what: &str) -> Result<f64, UvttError> {
    if v.is_finite() {
        Ok(v)
    } else {
        bad(format!("{what} is not finite"))
    }
}

fn num(v: &Value, what: &str) -> Result<f64, UvttError> {
    match v.as_f64() {
        Some(n) => finite(n, what),
        None => bad(format!("{what} must be a number")),
    }
}

fn pt(v: &Value, what: &str) -> Result<Pt, UvttError> {
    Ok(Pt {
        x: num(&v["x"], what)?,
        y: num(&v["y"], what)?,
    })
}

fn argb_to_hex(c: &str) -> String {
    let s = c.trim_start_matches('#');
    match s.len() {
        8 => format!("#{}", &s[2..]),
        6 => format!("#{s}"),
        _ => "#ffd27a".to_string(),
    }
}

fn hex_to_argb(c: &str) -> String {
    format!("ff{}", c.trim_start_matches('#'))
}

/// Export. Walls are in map pixels; `cell_size` converts to squares.
pub fn to_uvtt(map: &UvttMap) -> Value {
    let g = map.cell_size;
    let sq = |p: &[f64; 2]| json!({"x": p[0] / g, "y": p[1] / g});
    let mut los = Vec::new();
    let mut portals = Vec::new();
    let mut windows = Vec::new();
    for w in &map.walls {
        match w.kind {
            WallKind::Wall | WallKind::Secret => {
                los.push(Value::Array(w.pts.iter().map(sq).collect()))
            }
            WallKind::Door if w.pts.len() >= 2 => {
                let (a, b) = (w.pts[0], *w.pts.last().unwrap());
                let rot = (b[1] - a[1]).atan2(b[0] - a[0]);
                portals.push(json!({
                    "position": {"x": (a[0] + b[0]) / 2.0 / g, "y": (a[1] + b[1]) / 2.0 / g},
                    "bounds": [sq(&a), sq(&b)],
                    "rotation": rot,
                    "closed": w.state != Some(WallState::Open),
                    "freestanding": false,
                }));
            }
            WallKind::Window => windows.push(Value::Array(w.pts.iter().map(sq).collect())),
            _ => {}
        }
    }
    let lights: Vec<Value> = map
        .lights
        .iter()
        .filter(|l| l.on)
        .map(|l| {
            json!({
                "position": {"x": l.x / g, "y": l.y / g},
                "range": l.range,
                "intensity": l.intensity,
                "color": hex_to_argb(&l.color),
                "shadows": true,
            })
        })
        .collect();
    json!({
        "format": UVTT_FORMAT,
        "resolution": {
            "map_origin": {"x": 0, "y": 0},
            "map_size": {"x": map.cols, "y": map.rows},
            "pixels_per_grid": map.cell_size.round() as u32,
        },
        "line_of_sight": los,
        "portals": portals,
        "environment": {"baked_lighting": false, "ambient_light": "ffffffff"},
        "lights": lights,
        "x_ndcart": {"windows": windows},
        "image": map.image_b64.clone().unwrap_or_default(),
    })
}

/// Import. Rooms cannot be reconstructed from line segments, so the result is
/// walls + lights + lattice; the image (if any) becomes the underlay.
pub fn from_uvtt(text: &str) -> Result<UvttMap, UvttError> {
    let v: Value = serde_json::from_str(text).map_err(|e| UvttError::Json(e.to_string()))?;
    let res = &v["resolution"];
    if !res.is_object() {
        return bad("missing resolution");
    }
    let origin = pt(&res["map_origin"], "map_origin").unwrap_or(Pt { x: 0.0, y: 0.0 });
    let cols = num(&res["map_size"]["x"], "map_size.x")?;
    let rows = num(&res["map_size"]["y"], "map_size.y")?;
    let ppg = num(&res["pixels_per_grid"], "pixels_per_grid")?;
    if !(1.0..=MAX_CELLS).contains(&cols) || !(1.0..=MAX_CELLS).contains(&rows) {
        return bad("map_size out of range");
    }
    if !(4.0..=1024.0).contains(&ppg) {
        return bad("pixels_per_grid must be 4..=1024");
    }
    let g = ppg;
    let px = |p: Pt| [(p.x - origin.x) * g, (p.y - origin.y) * g];

    let mut points = 0usize;
    let mut walls = Vec::new();
    let polylines = |key: &str, points: &mut usize| -> Result<Vec<Vec<[f64; 2]>>, UvttError> {
        let mut out = Vec::new();
        for line in v[key].as_array().map(Vec::as_slice).unwrap_or(&[]) {
            let arr = line
                .as_array()
                .ok_or_else(|| UvttError::Invalid(format!("{key} entry is not a list")))?;
            *points += arr.len();
            if *points > MAX_POINTS {
                return bad("too many points");
            }
            if arr.len() < 2 {
                continue;
            }
            let mut pts = Vec::with_capacity(arr.len());
            for p in arr {
                pts.push(px(pt(p, key)?));
            }
            out.push(pts);
        }
        Ok(out)
    };
    for pts in polylines("line_of_sight", &mut points)? {
        walls.push(Wall {
            id: ndcart_core::ids::new_id("w"),
            pts,
            kind: WallKind::Wall,
            state: None,
        });
    }
    for pts in polylines_of(&v["x_ndcart"]["windows"], &px)? {
        walls.push(Wall {
            id: ndcart_core::ids::new_id("w"),
            pts,
            kind: WallKind::Window,
            state: None,
        });
    }
    for p in v["portals"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let b = p["bounds"].as_array().filter(|b| b.len() >= 2);
        let Some(b) = b else { continue };
        let a = px(pt(&b[0], "portal bound")?);
        let z = px(pt(&b[1], "portal bound")?);
        let closed = p["closed"].as_bool().unwrap_or(true);
        walls.push(Wall {
            id: ndcart_core::ids::new_id("w"),
            pts: vec![a, z],
            kind: WallKind::Door,
            state: Some(if closed {
                WallState::Closed
            } else {
                WallState::Open
            }),
        });
    }
    if walls.len() > ndcart_core::ND_WORLD_WALL_CAP * 20 {
        return bad("too many wall segments");
    }

    let mut lights = Vec::new();
    for l in v["lights"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
        if lights.len() >= ndcart_core::MAX_LIGHTS {
            break; // nd-world's publish cap; the rest are dropped, not guessed
        }
        let pos = px(pt(&l["position"], "light position")?);
        let light = Light {
            id: ndcart_core::ids::new_id("l"),
            x: pos[0],
            y: pos[1],
            range: num(&l["range"], "light range")?.clamp(0.5, 200.0),
            color: argb_to_hex(l["color"].as_str().unwrap_or("")),
            intensity: l["intensity"]
                .as_f64()
                .filter(|i| i.is_finite())
                .unwrap_or(1.0)
                .clamp(0.1, 1.0),
            on: true,
            label: None,
        };
        lights.push(light);
    }

    let image_b64 = v["image"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    Ok(UvttMap {
        cols: cols as u32,
        rows: rows as u32,
        cell_size: g,
        walls,
        lights,
        image_b64,
    })
}

fn polylines_of(v: &Value, px: &dyn Fn(Pt) -> [f64; 2]) -> Result<Vec<Vec<[f64; 2]>>, UvttError> {
    let mut out = Vec::new();
    for line in v.as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let Some(arr) = line.as_array() else { continue };
        if arr.len() < 2 {
            continue;
        }
        let mut pts = Vec::new();
        for p in arr {
            pts.push(px(pt(p, "window")?));
        }
        out.push(pts);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ndcart_core::grid::{
        default_space, CellKey, Edge, EdgeKey, Mark, MarkKind, RoomGrid, SpaceKind,
    };
    use ndcart_core::walls_from_grid;

    fn sample() -> UvttMap {
        let mut g = RoomGrid::default();
        g.upsert_space(default_space("a", "Hall", SpaceKind::Room))
            .unwrap();
        let cells: Vec<CellKey> = (0..6)
            .flat_map(|x| (0..4).map(move |y| CellKey::new(x, y)))
            .collect();
        g.paint(&cells, Some("a")).unwrap();
        g.set_mark(
            EdgeKey {
                cell: CellKey::new(2, 0),
                edge: Edge::North,
            },
            Some(Mark::new(MarkKind::Door)),
        )
        .unwrap();
        g.set_mark(
            EdgeKey {
                cell: CellKey::new(0, 1),
                edge: Edge::West,
            },
            Some(Mark::new(MarkKind::Window)),
        )
        .unwrap();
        let walls = walls_from_grid(&g, 70.0).unwrap();
        let lights = vec![Light {
            id: "l1".into(),
            x: 140.0,
            y: 70.0,
            range: 6.0,
            color: "#ffaa00".into(),
            intensity: 0.8,
            on: true,
            label: None,
        }];
        UvttMap {
            cols: 6,
            rows: 4,
            cell_size: 70.0,
            walls,
            lights,
            image_b64: Some("AAAA".into()),
        }
    }

    fn key(w: &Wall) -> (WallKind, Vec<(i64, i64)>, Option<WallState>) {
        let mut pts: Vec<(i64, i64)> = w
            .pts
            .iter()
            .map(|p| (p[0].round() as i64, p[1].round() as i64))
            .collect();
        if pts.first() > pts.last() {
            pts.reverse();
        }
        (w.kind, pts, w.state)
    }

    #[test]
    fn round_trip_is_exact() {
        let m = sample();
        let text = to_uvtt(&m).to_string();
        let back = from_uvtt(&text).unwrap();
        assert_eq!((back.cols, back.rows, back.cell_size), (6, 4, 70.0));
        let mut a: Vec<_> = m.walls.iter().map(key).collect();
        let mut b: Vec<_> = back.walls.iter().map(key).collect();
        a.sort();
        b.sort();
        assert_eq!(a, b);
        assert_eq!(back.lights.len(), 1);
        assert_eq!(back.lights[0].color, "#ffaa00");
        assert_eq!(back.lights[0].x, 140.0);
        assert_eq!(back.image_b64.as_deref(), Some("AAAA"));
    }

    #[test]
    fn doors_become_portals_windows_do_not_block_sight() {
        let v = to_uvtt(&sample());
        assert_eq!(v["portals"].as_array().unwrap().len(), 1);
        assert_eq!(v["portals"][0]["closed"], true);
        assert_eq!(v["x_ndcart"]["windows"].as_array().unwrap().len(), 1);
        assert_eq!(v["format"], 0.3);
    }

    #[test]
    fn foreign_file_without_extension_imports() {
        let t = r#"{"format":0.3,"resolution":{"map_origin":{"x":0,"y":0},"map_size":{"x":10,"y":10},"pixels_per_grid":100},
          "line_of_sight":[[{"x":0,"y":0},{"x":5,"y":0}]],"portals":[{"bounds":[{"x":1,"y":0},{"x":2,"y":0}],"closed":false}],
          "lights":[{"position":{"x":1,"y":1},"range":4,"intensity":2.0,"color":"ffff0000"}],"image":""}"#;
        let m = from_uvtt(t).unwrap();
        assert_eq!(m.walls.len(), 2);
        assert_eq!(m.walls[1].state, Some(WallState::Open));
        assert_eq!(m.lights[0].color, "#ff0000");
        assert_eq!(m.lights[0].intensity, 1.0);
        assert!(m.image_b64.is_none());
    }

    #[test]
    fn hostile_input_is_refused() {
        assert!(from_uvtt("nope").is_err());
        assert!(from_uvtt("{}").is_err());
        let huge = r#"{"resolution":{"map_size":{"x":1e9,"y":10},"pixels_per_grid":100}}"#;
        assert!(from_uvtt(huge).is_err());
        let tiny = r#"{"resolution":{"map_size":{"x":10,"y":10},"pixels_per_grid":0}}"#;
        assert!(from_uvtt(tiny).is_err());
        let nan = r#"{"resolution":{"map_size":{"x":10,"y":10},"pixels_per_grid":50},"line_of_sight":[[{"x":"a","y":0},{"x":1,"y":1}]]}"#;
        assert!(from_uvtt(nan).is_err());
    }
}
