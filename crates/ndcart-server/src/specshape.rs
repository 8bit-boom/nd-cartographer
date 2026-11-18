//! The exact field names a MapSpec and a WorldSpec accept, and a check for any others.
//!
//! The serde structs ignore unknown fields (a pasted spec stays lenient), but a model that
//! invents `"props"` or `"x"` should be told so instead of having them dropped silently.
//! `unknown_fields` is used by the AI repair path (`draft::parse_reply`) and by the spec
//! check. The allow-lists are kept honest by a sync test against the real structs.

#![cfg_attr(not(feature = "bridge"), allow(dead_code))] // only the spec check (AI 2.3) uses it without AI

use serde_json::Value;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SpecKind {
    Map,
    World,
}

pub const MAP_FIELDS: &[&str] = &["name", "floors"];
pub const FLOOR_FIELDS: &[&str] = &["name", "rooms", "links", "stairs"];
pub const ROOM_FIELDS: &[&str] = &["id", "name", "kind", "w", "h", "lit"];
pub const LINK_FIELDS: &[&str] = &["a", "b", "kind"];
pub const STAIR_FIELDS: &[&str] = &["room", "to_floor", "to_room", "label"];

pub const WORLD_FIELDS: &[&str] = &[
    "name",
    "cols",
    "rows",
    "land",
    "continent",
    "ridges",
    "rivers",
    "biomes",
    "places",
    "regions",
];
pub const BIOME_FIELDS: &[&str] = &["biome", "x", "y", "rx", "ry", "density"];
pub const PLACE_FIELDS: &[&str] = &["name", "note", "x", "y"];
pub const REGION_FIELDS: &[&str] = &["name", "points", "color"];

/// One level of the shape: what it is called in messages, its allowed keys, and which keys
/// hold arrays of child objects.
struct Level {
    label: &'static str,
    allowed: &'static [&'static str],
    children: &'static [(&'static str, &'static Level)],
}

static STAIR: Level = Level {
    label: "a stair",
    allowed: STAIR_FIELDS,
    children: &[],
};
static LINK: Level = Level {
    label: "a link",
    allowed: LINK_FIELDS,
    children: &[],
};
static ROOM: Level = Level {
    label: "a room",
    allowed: ROOM_FIELDS,
    children: &[],
};
static FLOOR: Level = Level {
    label: "a floor",
    allowed: FLOOR_FIELDS,
    children: &[("rooms", &ROOM), ("links", &LINK), ("stairs", &STAIR)],
};
static MAP: Level = Level {
    label: "the map",
    allowed: MAP_FIELDS,
    children: &[("floors", &FLOOR)],
};
static BIOME: Level = Level {
    label: "a biome",
    allowed: BIOME_FIELDS,
    children: &[],
};
static PLACE: Level = Level {
    label: "a place",
    allowed: PLACE_FIELDS,
    children: &[],
};
static REGION: Level = Level {
    label: "a region",
    allowed: REGION_FIELDS,
    children: &[],
};
static WORLD: Level = Level {
    label: "the world",
    allowed: WORLD_FIELDS,
    children: &[("biomes", &BIOME), ("places", &PLACE), ("regions", &REGION)],
};

fn walk(v: &Value, level: &'static Level, path: &str, out: &mut Vec<(String, &'static Level)>) {
    let Some(obj) = v.as_object() else { return };
    for (k, child) in obj {
        let here = if path.is_empty() {
            k.clone()
        } else {
            format!("{path}.{k}")
        };
        if !level.allowed.contains(&k.as_str()) {
            out.push((here, level));
        } else if let Some((_, sub)) = level.children.iter().find(|(name, _)| name == k) {
            if let Some(items) = child.as_array() {
                for (i, item) in items.iter().enumerate() {
                    walk(item, sub, &format!("{here}[{i}]"), out);
                }
            }
        }
    }
}

/// Paths (like `floors[0].rooms[2].props`) of every field a spec of `kind` does not have.
#[allow(dead_code)] // the paths alone are for the spec check (AI 2.3)
pub fn unknown_fields(v: &Value, kind: SpecKind) -> Vec<String> {
    unknown_with_levels(v, kind)
        .into_iter()
        .map(|(p, _)| p)
        .collect()
}

fn unknown_with_levels(v: &Value, kind: SpecKind) -> Vec<(String, &'static Level)> {
    let mut out = Vec::new();
    walk(
        v,
        match kind {
            SpecKind::Map => &MAP,
            SpecKind::World => &WORLD,
        },
        "",
        &mut out,
    );
    out
}

/// The complaint to hand back to a model, or None when every field is known.
pub fn unknown_fields_complaint(v: &Value, kind: SpecKind) -> Option<String> {
    let found = unknown_with_levels(v, kind);
    if found.is_empty() {
        return None;
    }
    let paths: Vec<&str> = found.iter().map(|(p, _)| p.as_str()).collect();
    let mut levels: Vec<&'static Level> = Vec::new();
    for (_, l) in &found {
        if !levels.iter().any(|x| std::ptr::eq(*x, *l)) {
            levels.push(l);
        }
    }
    let allowed: Vec<String> = levels
        .iter()
        .map(|l| format!("Allowed in {}: {}.", l.label, l.allowed.join(", ")))
        .collect();
    Some(format!(
        "Unknown field(s): {}. {} Remove them.",
        paths.join(", "),
        allowed.join(" ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ndcart_core::grid::SpaceKind;
    use ndcart_core::mapspec::*;
    use serde_json::json;
    use std::collections::BTreeSet;

    fn keys(v: &Value) -> BTreeSet<String> {
        v.as_object().expect("object").keys().cloned().collect()
    }
    fn set(a: &[&str]) -> BTreeSet<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_stray_field_is_named_by_path_with_what_is_allowed() {
        let v = json!({"name":"M","floors":[{"name":"G","rooms":[
            {"id":"a","name":"A","w":4,"h":4},
            {"id":"b","name":"B","w":4,"h":4},
            {"id":"c","name":"C","w":4,"h":4,"props":[]}]}],"x":1});
        assert_eq!(
            unknown_fields(&v, SpecKind::Map),
            vec!["floors[0].rooms[2].props".to_string(), "x".to_string()]
        );
        let m = unknown_fields_complaint(&v, SpecKind::Map).unwrap();
        assert!(
            m.starts_with("Unknown field(s): floors[0].rooms[2].props, x."),
            "{m}"
        );
        assert!(
            m.contains("Allowed in a room: id, name, kind, w, h, lit."),
            "{m}"
        );
        assert!(m.contains("Allowed in the map: name, floors."), "{m}");
        let clean =
            json!({"name":"M","floors":[{"name":"G","rooms":[{"id":"a","name":"A","w":4,"h":4}]}]});
        assert!(unknown_fields_complaint(&clean, SpecKind::Map).is_none());
        let w = json!({"name":"W","places":[{"name":"P","x":0.1,"y":0.2,"icon":"star"}]});
        assert_eq!(
            unknown_fields(&w, SpecKind::World),
            vec!["places[0].icon".to_string()]
        );
    }

    #[test]
    fn the_allow_lists_match_the_real_structs() {
        let map = MapSpec {
            name: "m".into(),
            floors: vec![FloorSpec {
                name: "f".into(),
                rooms: vec![RoomSpec {
                    id: "a".into(),
                    name: "A".into(),
                    kind: SpaceKind::Room,
                    w: 4,
                    h: 4,
                    lit: true,
                }],
                links: vec![LinkSpec {
                    a: "a".into(),
                    b: "a".into(),
                    kind: LinkKind::Door,
                }],
                stairs: vec![StairSpec {
                    room: "a".into(),
                    to_floor: "g".into(),
                    to_room: Some("b".into()),
                    label: Some("up".into()),
                }],
            }],
        };
        let v = serde_json::to_value(&map).unwrap();
        assert_eq!(keys(&v), set(MAP_FIELDS));
        let f = &v["floors"][0];
        assert_eq!(keys(f), set(FLOOR_FIELDS));
        assert_eq!(keys(&f["rooms"][0]), set(ROOM_FIELDS));
        assert_eq!(keys(&f["links"][0]), set(LINK_FIELDS));
        assert_eq!(keys(&f["stairs"][0]), set(STAIR_FIELDS));
        // A fully populated spec is never reported.
        assert!(unknown_fields(&v, SpecKind::Map).is_empty());

        use crate::worldspec::*;
        let world = WorldSpec {
            name: "w".into(),
            cols: 64,
            rows: 64,
            land: 0.4,
            continent: 0.5,
            ridges: 0.5,
            rivers: 1,
            biomes: vec![BiomeMask {
                biome: "forest".into(),
                x: 0.5,
                y: 0.5,
                rx: 0.1,
                ry: 0.1,
                density: 1.0,
            }],
            places: vec![Place {
                name: "p".into(),
                note: "n".into(),
                x: 0.5,
                y: 0.5,
            }],
            regions: vec![RegionSpec {
                name: "r".into(),
                points: vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
                color: Some("#ffffff".into()),
            }],
        };
        let v = serde_json::to_value(&world).unwrap();
        assert_eq!(keys(&v), set(WORLD_FIELDS));
        assert_eq!(keys(&v["biomes"][0]), set(BIOME_FIELDS));
        assert_eq!(keys(&v["places"][0]), set(PLACE_FIELDS));
        assert_eq!(keys(&v["regions"][0]), set(REGION_FIELDS));
        assert!(unknown_fields(&v, SpecKind::World).is_empty());
    }
}
