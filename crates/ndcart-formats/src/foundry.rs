//! Foundry VTT scene data (the JSON of a Scene document), written for Foundry
//! v11/v12: wall restrictions are 0 none / 10 limited / 20 normal, the grid and
//! background are objects, light settings live under `config`. A scene refers to
//! its picture by a path inside Foundry's data folder, so the picture travels
//! separately (export it as WebP and upload it, then give its path here).
//! Not exercised against a running Foundry: the shapes follow its documented
//! data model and round-trip through this module's own reader.

use ndcart_core::{Light, Wall, WallKind, WallState};
use serde_json::{json, Value};

/// What a scene export needs, in picture pixels at `grid_px` pixels a square.
pub struct SceneOut<'a> {
    pub name: &'a str,
    pub grid_px: f64,
    pub cols: u32,
    pub rows: u32,
    pub walls: &'a [Wall],
    pub lights: &'a [Light],
    /// Where the picture will live in Foundry, e.g. `maps/crypt.webp`.
    pub image_path: &'a str,
    /// What one square is worth, e.g. 5 and "ft".
    pub distance: f64,
    pub units: &'a str,
}

fn restriction(kind: WallKind) -> (u8, u8) {
    // (move, sense): 20 blocks, 10 is limited (sight through a window), 0 nothing.
    match kind {
        WallKind::Window => (20, 10),
        _ => (20, 20),
    }
}

pub fn to_scene(s: &SceneOut) -> Value {
    let (w, h) = (f64::from(s.cols) * s.grid_px, f64::from(s.rows) * s.grid_px);
    let mut walls = Vec::new();
    for wall in s.walls {
        for pair in wall.pts.windows(2) {
            let (mv, sense) = restriction(wall.kind);
            let (door, ds) = match wall.kind {
                WallKind::Door => (1, i32::from(wall.state == Some(WallState::Open))),
                WallKind::Secret => (2, i32::from(wall.state == Some(WallState::Open))),
                _ => (0, 0),
            };
            walls.push(json!({
                "c": [pair[0][0].round(), pair[0][1].round(), pair[1][0].round(), pair[1][1].round()],
                "move": mv, "sense": sense, "light": sense, "sound": sense,
                "dir": 0, "door": door, "ds": ds,
            }));
        }
    }
    let lights: Vec<Value> = s
        .lights
        .iter()
        .filter(|l| l.on)
        .map(|l| {
            let dim = l.range * s.distance;
            json!({
                "x": l.x.round(), "y": l.y.round(), "rotation": 0,
                "config": {
                    "dim": dim, "bright": dim / 2.0, "angle": 360,
                    "color": l.color, "alpha": l.intensity.clamp(0.0, 1.0),
                },
            })
        })
        .collect();
    json!({
        "name": s.name,
        "navigation": true,
        "width": w, "height": h, "padding": 0, "shiftX": 0, "shiftY": 0,
        "background": {"src": s.image_path},
        "grid": {"type": 1, "size": s.grid_px, "distance": s.distance, "units": s.units, "color": "#000000", "alpha": 0.2},
        "tokenVision": true,
        "fog": {"exploration": true},
        "globalLight": false, "darkness": 0,
        "walls": walls,
        "lights": lights,
    })
}

#[derive(Debug)]
pub struct SceneIn {
    pub name: String,
    pub grid_px: f64,
    pub cols: u32,
    pub rows: u32,
    /// In picture pixels, relative to the scene's top-left.
    pub walls: Vec<Wall>,
    pub lights: Vec<Light>,
    pub image_path: String,
}

fn num(v: &Value, path: &[&str]) -> Option<f64> {
    let mut cur = v;
    for k in path {
        cur = cur.get(*k)?;
    }
    cur.as_f64().filter(|x| x.is_finite())
}

/// Read a scene (older scenes with a numeric `grid`, or `walls` as booleans, are tolerated).
pub fn from_scene(text: &str) -> Result<SceneIn, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("that is not JSON: {e}"))?;
    let v = if v.get("walls").is_some() || v.get("grid").is_some() {
        v
    } else {
        v.get("scene").cloned().unwrap_or(v)
    };
    let width = num(&v, &["width"]).ok_or("the scene has no width")?;
    let height = num(&v, &["height"]).ok_or("the scene has no height")?;
    let grid_px = num(&v, &["grid", "size"])
        .or_else(|| num(&v, &["grid"]))
        .ok_or("the scene has no grid size")?;
    if !(8.0..=2000.0).contains(&grid_px)
        || !(1.0..=100_000.0).contains(&width)
        || !(1.0..=100_000.0).contains(&height)
    {
        return Err("the scene's size or grid is out of range".into());
    }
    let distance = num(&v, &["grid", "distance"])
        .or_else(|| num(&v, &["gridDistance"]))
        .unwrap_or(5.0)
        .max(0.01);
    // Foundry pads the scene; walls are in canvas coordinates, so take the padding back off.
    let padding = num(&v, &["padding"]).unwrap_or(0.0).clamp(0.0, 0.5);
    let (ox, oy) = (
        (width * padding / grid_px).ceil() * grid_px,
        (height * padding / grid_px).ceil() * grid_px,
    );
    let cols = (width / grid_px).round().max(1.0) as u32;
    let rows = (height / grid_px).round().max(1.0) as u32;
    let mut walls = Vec::new();
    for (i, w) in v
        .get("walls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let Some(c) = w
            .get("c")
            .and_then(Value::as_array)
            .filter(|c| c.len() == 4)
        else {
            continue;
        };
        let n: Vec<f64> = c.iter().filter_map(Value::as_f64).collect();
        if n.len() != 4 {
            continue;
        }
        let flag = |k: &str| {
            w.get(k)
                .and_then(|x| {
                    x.as_f64()
                        .or_else(|| x.as_bool().map(|b| if b { 20.0 } else { 0.0 }))
                })
                .unwrap_or(20.0)
        };
        let (mv, sense) = (flag("move"), flag("sense"));
        let door = w.get("door").and_then(Value::as_i64).unwrap_or(0);
        let open = w.get("ds").and_then(Value::as_i64).unwrap_or(0) == 1;
        if mv == 0.0 && sense == 0.0 && door == 0 {
            continue; // an invisible/ethereal wall blocks nothing
        }
        let (kind, state) = match door {
            1 => (
                WallKind::Door,
                Some(if open {
                    WallState::Open
                } else {
                    WallState::Closed
                }),
            ),
            2 => (
                WallKind::Secret,
                Some(if open {
                    WallState::Open
                } else {
                    WallState::Closed
                }),
            ),
            _ if (sense - 10.0).abs() < f64::EPSILON => (WallKind::Window, None), // 10 is "limited" (older scenes used 1 for "normal")
            _ => (WallKind::Wall, None),
        };
        walls.push(Wall {
            id: format!("fw{i}"),
            pts: vec![[n[0] - ox, n[1] - oy], [n[2] - ox, n[3] - oy]],
            kind,
            state,
        });
    }
    let mut lights = Vec::new();
    for (i, l) in v
        .get("lights")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let (Some(x), Some(y)) = (num(l, &["x"]), num(l, &["y"])) else {
            continue;
        };
        let dim = num(l, &["config", "dim"])
            .or_else(|| num(l, &["dim"]))
            .unwrap_or(0.0);
        let bright = num(l, &["config", "bright"])
            .or_else(|| num(l, &["bright"]))
            .unwrap_or(0.0);
        let reach = dim.max(bright);
        if reach <= 0.0 {
            continue;
        }
        let color = l
            .pointer("/config/color")
            .or_else(|| l.get("tintColor"))
            .and_then(Value::as_str)
            .filter(|c| {
                c.len() == 7 && c.starts_with('#') && c[1..].bytes().all(|b| b.is_ascii_hexdigit())
            })
            .unwrap_or("#ffffff")
            .to_string();
        lights.push(Light {
            id: format!("fl{i}"),
            x: x - ox,
            y: y - oy,
            range: (reach / distance).clamp(0.5, 100.0),
            color,
            intensity: num(l, &["config", "alpha"]).unwrap_or(0.5).clamp(0.1, 1.0),
            on: true,
            label: None,
        });
    }
    Ok(SceneIn {
        name: v
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("Foundry scene")
            .chars()
            .take(80)
            .collect(),
        grid_px,
        cols,
        rows,
        walls,
        lights,
        image_path: v
            .pointer("/background/src")
            .or_else(|| v.get("img"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wall(id: &str, a: [f64; 2], b: [f64; 2], kind: WallKind, state: Option<WallState>) -> Wall {
        Wall {
            id: id.into(),
            pts: vec![a, b],
            kind,
            state,
        }
    }

    fn sample() -> (Vec<Wall>, Vec<Light>) {
        let walls = vec![
            wall("a", [0.0, 0.0], [200.0, 0.0], WallKind::Wall, None),
            wall(
                "b",
                [200.0, 0.0],
                [200.0, 100.0],
                WallKind::Door,
                Some(WallState::Open),
            ),
            wall(
                "c",
                [0.0, 100.0],
                [100.0, 100.0],
                WallKind::Secret,
                Some(WallState::Closed),
            ),
            wall("d", [100.0, 100.0], [200.0, 100.0], WallKind::Window, None),
        ];
        let lights = vec![
            Light {
                id: "l".into(),
                x: 50.0,
                y: 50.0,
                range: 4.0,
                color: "#ffaa00".into(),
                intensity: 0.8,
                on: true,
                label: None,
            },
            Light {
                id: "off".into(),
                x: 90.0,
                y: 90.0,
                range: 2.0,
                color: "#ffffff".into(),
                intensity: 0.5,
                on: false,
                label: None,
            },
        ];
        (walls, lights)
    }

    #[test]
    fn a_scene_has_foundrys_shapes_and_a_round_trip_keeps_walls_doors_windows_and_lights() {
        let (walls, lights) = sample();
        let v = to_scene(&SceneOut {
            name: "Crypt",
            grid_px: 100.0,
            cols: 4,
            rows: 3,
            walls: &walls,
            lights: &lights,
            image_path: "maps/crypt.webp",
            distance: 5.0,
            units: "ft",
        });
        assert_eq!(
            (v["width"].as_f64(), v["height"].as_f64()),
            (Some(400.0), Some(300.0))
        );
        assert_eq!(v["grid"]["size"], 100.0);
        assert_eq!(v["background"]["src"], "maps/crypt.webp");
        let w = v["walls"].as_array().unwrap();
        assert_eq!(
            (
                w[0]["move"].as_i64(),
                w[0]["sense"].as_i64(),
                w[0]["door"].as_i64()
            ),
            (Some(20), Some(20), Some(0))
        );
        assert_eq!(
            (w[1]["door"].as_i64(), w[1]["ds"].as_i64()),
            (Some(1), Some(1)),
            "an open door"
        );
        assert_eq!(w[2]["door"], 2, "a secret door");
        assert_eq!(
            (w[3]["move"].as_i64(), w[3]["sense"].as_i64()),
            (Some(20), Some(10)),
            "a window: blocks movement, limited sight"
        );
        let l = v["lights"].as_array().unwrap();
        assert_eq!(l.len(), 1, "a switched-off light is not exported");
        assert_eq!(l[0]["config"]["dim"], 20.0);
        assert_eq!(l[0]["config"]["bright"], 10.0);
        let back = from_scene(&v.to_string()).unwrap();
        assert_eq!(
            (back.cols, back.rows, back.grid_px, back.name.as_str()),
            (4, 3, 100.0, "Crypt")
        );
        assert_eq!(back.walls.len(), 4);
        assert_eq!(back.walls[1].kind, WallKind::Door);
        assert_eq!(back.walls[1].state, Some(WallState::Open));
        assert_eq!(back.walls[2].kind, WallKind::Secret);
        assert_eq!(back.walls[3].kind, WallKind::Window);
        assert_eq!(back.lights.len(), 1);
        assert_eq!(
            (back.lights[0].range, back.lights[0].color.as_str()),
            (4.0, "#ffaa00")
        );
        assert_eq!(back.image_path, "maps/crypt.webp");
    }

    #[test]
    fn padding_is_taken_off_and_odd_walls_are_skipped_or_refused() {
        let scene = r##"{"name":"Padded","width":400,"height":300,"padding":0.25,"grid":{"size":100,"distance":10},
          "walls":[{"c":[200,200,300,200],"move":1,"sense":1},{"c":[0,0,5,5],"move":0,"sense":0},{"c":[1,2],"move":20},{"c":[100,100,200,100],"door":1,"ds":2,"move":20,"sense":20}],
          "lights":[{"x":250,"y":250,"config":{"dim":0,"bright":0}},{"x":250,"y":250,"config":{"dim":30,"color":"red","alpha":0.4}}]}"##;
        let s = from_scene(scene).unwrap();
        // Padding 25% of 400 px = 100 px; of 300 px = 75 -> rounds up to 100.
        assert_eq!(
            s.walls.len(),
            2,
            "the invisible and the malformed wall are dropped"
        );
        assert_eq!(s.walls[0].pts, vec![[100.0, 100.0], [200.0, 100.0]]);
        assert_eq!(
            s.walls[0].kind,
            WallKind::Wall,
            "older numeric restrictions still block"
        );
        assert_eq!(
            s.walls[1].state,
            Some(WallState::Closed),
            "a locked door imports closed"
        );
        assert_eq!(s.lights.len(), 1);
        assert_eq!(
            (s.lights[0].x, s.lights[0].range, s.lights[0].color.as_str()),
            (150.0, 3.0, "#ffffff")
        );
        for bad in [
            "not json",
            r#"{"width":10}"#,
            r#"{"width":400,"height":300,"grid":{"size":2}}"#,
            r#"{"width":400,"height":300,"grid":{"size":100000}}"#,
        ] {
            assert!(from_scene(bad).is_err(), "{bad}");
        }
    }
}
