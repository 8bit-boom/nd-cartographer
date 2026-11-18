//! The AI instruction pack: the schemas, prompts, reference and examples that tell a model (or
//! an external agent) how to write a spec. The files live in `assets/ai/` and are compiled in
//! with `include_str!`; this module serves them. Everything here is pure (no network, no
//! model), so it works with or without the `bridge` feature.
//!
//! Text files hold `{{PLACEHOLDERS}}` that [`render`] fills from the real constants (limits,
//! material ids, furnishing keywords, ...), so a prompt can never drift from the code. The
//! `sync` tests fail when the pack and the code disagree.

// The HTTP and MCP routes (AI 2.3, 2.4) use the rest of this API.
#![allow(dead_code)]

use serde_json::Value;
use std::sync::LazyLock;

/// Every file of the pack, by its path under `assets/ai/`. `pack.toml` lists the same paths.
pub const FILES: &[(&str, &str)] = &[
    ("system_map.md", include_str!("../assets/ai/system_map.md")),
    (
        "system_world.md",
        include_str!("../assets/ai/system_world.md"),
    ),
    (
        "system_refine.md",
        include_str!("../assets/ai/system_refine.md"),
    ),
    (
        "system_tags.md",
        include_str!("../assets/ai/system_tags.md"),
    ),
    (
        "system_theme_pack.md",
        include_str!("../assets/ai/system_theme_pack.md"),
    ),
    ("reference.md", include_str!("../assets/ai/reference.md")),
    ("repair.md", include_str!("../assets/ai/repair.md")),
    ("mistakes.md", include_str!("../assets/ai/mistakes.md")),
    (
        "mapspec.schema.json",
        include_str!("../assets/ai/mapspec.schema.json"),
    ),
    (
        "worldspec.schema.json",
        include_str!("../assets/ai/worldspec.schema.json"),
    ),
    (
        "examples/map-shop.json",
        include_str!("../assets/ai/examples/map-shop.json"),
    ),
    (
        "examples/map-metro.json",
        include_str!("../assets/ai/examples/map-metro.json"),
    ),
    (
        "examples/map-crypt.json",
        include_str!("../assets/ai/examples/map-crypt.json"),
    ),
    (
        "examples/map-ship.json",
        include_str!("../assets/ai/examples/map-ship.json"),
    ),
    (
        "examples/map-tower.json",
        include_str!("../assets/ai/examples/map-tower.json"),
    ),
    (
        "examples/map-city-block.json",
        include_str!("../assets/ai/examples/map-city-block.json"),
    ),
    (
        "examples/map-cave.json",
        include_str!("../assets/ai/examples/map-cave.json"),
    ),
    (
        "examples/map-refine.json",
        include_str!("../assets/ai/examples/map-refine.json"),
    ),
    (
        "examples/world-archipelago.json",
        include_str!("../assets/ai/examples/world-archipelago.json"),
    ),
    (
        "examples/world-river-kingdom.json",
        include_str!("../assets/ai/examples/world-river-kingdom.json"),
    ),
    (
        "examples/world-desert.json",
        include_str!("../assets/ai/examples/world-desert.json"),
    ),
];

/// `pack.toml`: the version and the file list.
pub const MANIFEST: &str = include_str!("../assets/ai/pack.toml");

/// The JSON Schema (draft 2020-12) a MapSpec reply must follow.
pub const MAPSPEC_SCHEMA: &str = include_str!("../assets/ai/mapspec.schema.json");
/// The JSON Schema a WorldSpec reply must follow.
pub const WORLDSPEC_SCHEMA: &str = include_str!("../assets/ai/worldspec.schema.json");

#[derive(serde::Deserialize)]
pub struct Manifest {
    pub version: String,
    pub schema_version: u32,
    pub files: Vec<String>,
}

static PACK: LazyLock<Manifest> =
    LazyLock::new(|| toml::from_str(MANIFEST).expect("assets/ai/pack.toml is valid"));

/// The pack's own version (`pack.toml`).
pub fn version() -> &'static str {
    &PACK.version
}

pub fn schema_version() -> u32 {
    PACK.schema_version
}

pub fn manifest() -> &'static Manifest {
    &PACK
}

/// One file of the pack, as written (placeholders not filled).
pub fn raw(path: &str) -> Option<&'static str> {
    FILES.iter().find(|(p, _)| *p == path).map(|(_, t)| *t)
}

/// Which prompt.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Map,
    World,
    Refine,
    Tags,
    ThemePack,
}

impl Kind {
    pub fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "map" => Kind::Map,
            "world" => Kind::World,
            "refine" => Kind::Refine,
            "tags" => Kind::Tags,
            "theme_pack" | "theme-pack" => Kind::ThemePack,
            _ => return None,
        })
    }
    fn file(self) -> &'static str {
        match self {
            Kind::Map => "system_map.md",
            Kind::World => "system_world.md",
            Kind::Refine => "system_refine.md",
            Kind::Tags => "system_tags.md",
            Kind::ThemePack => "system_theme_pack.md",
        }
    }
}

fn code_list<'a>(items: impl Iterator<Item = &'a str>) -> String {
    items
        .map(|i| format!("`{i}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Fill the `{{PLACEHOLDERS}}` a pack text may use from the real constants. Unknown
/// placeholders (`{{problems}}` in `repair.md`) are left for the caller.
pub fn render(text: &str) -> String {
    use ndcart_core::mapspec::{MAX_FLOORS, MAX_ROOMS, ROOM_MAX, ROOM_MIN};
    let furnish = ndcart_core::furnish::KEYWORDS
        .iter()
        .map(|(group, words)| format!("- {group}: {}", code_list(words.iter().copied())))
        .collect::<Vec<_>>()
        .join("\n");
    let subs: [(&str, String); 12] = [
        ("PACK_VERSION", version().to_string()),
        ("SCHEMA_VERSION", schema_version().to_string()),
        ("CARGO_PKG_VERSION", env!("CARGO_PKG_VERSION").to_string()),
        ("MAX_FLOORS", MAX_FLOORS.to_string()),
        ("MAX_ROOMS", MAX_ROOMS.to_string()),
        ("ROOM_MIN", ROOM_MIN.to_string()),
        ("ROOM_MAX", ROOM_MAX.to_string()),
        ("MAX_LIGHTS", ndcart_core::MAX_LIGHTS.to_string()),
        ("WALL_CAP", ndcart_core::ND_WORLD_WALL_CAP.to_string()),
        (
            "MATERIALS",
            code_list(ndcart_paint::starter().iter().map(|m| m.id)),
        ),
        (
            "STAMPS",
            code_list(ndcart_paint::stamps().iter().map(|s| s.id)),
        ),
        ("FURNISH", furnish),
    ];
    let mut out = text.trim_end().to_string();
    for (k, v) in &subs {
        out = out.replace(&format!("{{{{{k}}}}}"), v);
    }
    out
}

static SYSTEMS: LazyLock<[String; 5]> = LazyLock::new(|| {
    [
        Kind::Map,
        Kind::World,
        Kind::Refine,
        Kind::Tags,
        Kind::ThemePack,
    ]
    .map(|k| render(raw(k.file()).expect("pack file")))
});

/// The rendered system prompt for `kind`.
pub fn system(kind: Kind) -> &'static str {
    let i = match kind {
        Kind::Map => 0,
        Kind::World => 1,
        Kind::Refine => 2,
        Kind::Tags => 3,
        Kind::ThemePack => 4,
    };
    &SYSTEMS[i]
}

static REFERENCE: LazyLock<String> = LazyLock::new(|| render(raw("reference.md").expect("pack")));
static MISTAKES: LazyLock<String> = LazyLock::new(|| render(raw("mistakes.md").expect("pack")));

/// The full reference (rendered).
pub fn reference() -> &'static str {
    &REFERENCE
}

/// The common-mistakes list (rendered).
pub fn mistakes() -> &'static str {
    &MISTAKES
}

/// The repair-turn text with the problems filled in.
pub fn repair(problems: &str) -> String {
    render(raw("repair.md").expect("pack")).replace("{{problems}}", problems)
}

/// The schema text for a spec kind.
pub fn schema(kind: crate::specshape::SpecKind) -> &'static str {
    match kind {
        crate::specshape::SpecKind::Map => MAPSPEC_SCHEMA,
        crate::specshape::SpecKind::World => WORLDSPEC_SCHEMA,
    }
}

/// One worked example: what was asked, the spec that answers it, and why it looks so.
#[derive(Clone, Debug)]
pub struct Example {
    pub file: &'static str,
    pub request: String,
    pub why: String,
    pub spec: Value,
}

/// The worked examples, in file order (map examples first; `world-*` are WorldSpecs and
/// `map-refine` is a one-floor revision).
pub fn examples() -> Vec<Example> {
    FILES
        .iter()
        .filter(|(p, _)| p.starts_with("examples/"))
        .map(|(p, t)| {
            let v: Value = serde_json::from_str(t).expect("example is valid JSON");
            let text = |k: &str| v[k].as_str().unwrap_or_default().to_string();
            Example {
                file: p,
                request: text("request"),
                why: text("why"),
                spec: v["spec"].clone(),
            }
        })
        .collect()
}

#[cfg(test)]
mod sync {
    //! The hand-written schemas must agree with the validators, field for field. Each test
    //! fails when the code changes and the schema (and therefore the pack) does not.
    use super::*;
    use crate::specshape::*;
    use crate::worldspec::WorldSpec;
    use ndcart_core::mapspec::MapSpec;
    use serde_json::{json, Value};
    use std::collections::BTreeSet;

    const ID_PATTERN: &str = "^[A-Za-z0-9_-]{1,40}$";
    const COLOR_PATTERN: &str = "^#[0-9a-fA-F]{6}$";

    fn schema(kind: SpecKind) -> Value {
        serde_json::from_str(match kind {
            SpecKind::Map => MAPSPEC_SCHEMA,
            SpecKind::World => WORLDSPEC_SCHEMA,
        })
        .expect("the schema is valid JSON")
    }

    /// A spec with every optional field set, so every schema path exists in it.
    fn full_example(kind: SpecKind) -> Value {
        match kind {
            SpecKind::Map => json!({"name":"Metro","floors":[
                {"name":"Upper","rooms":[
                    {"id":"hall","name":"Hall","kind":"room","w":8,"h":6,"lit":true},
                    {"id":"pass","name":"Passage","kind":"corridor","w":3,"h":6,"lit":false}],
                 "links":[{"a":"hall","b":"pass","kind":"door"}],
                 "stairs":[{"room":"hall","to_floor":"Lower","to_room":"vault","label":"down"}]},
                {"name":"Lower","rooms":[{"id":"vault","name":"Vault","kind":"area","w":6,"h":6,"lit":true}],
                 "links":[],"stairs":[]}]}),
            SpecKind::World => {
                json!({"name":"Isles","cols":64,"rows":48,"land":0.4,"continent":0.5,
                "ridges":0.5,"rivers":3,
                "biomes":[{"biome":"forest","x":0.5,"y":0.5,"rx":0.1,"ry":0.1,"density":1.5}],
                "places":[{"name":"Port","note":"a harbour","x":0.5,"y":0.5}],
                "regions":[{"name":"Crown","points":[[0.1,0.1],[0.9,0.1],[0.5,0.9]],"color":"#aa3300"}]})
            }
        }
    }

    /// Does the studio accept this (parse, then validate)?
    fn accepts(kind: SpecKind, v: &Value) -> bool {
        match kind {
            SpecKind::Map => serde_json::from_value::<MapSpec>(v.clone())
                .map(|s| s.validate().is_ok())
                .unwrap_or(false),
            SpecKind::World => serde_json::from_value::<WorldSpec>(v.clone())
                .map(|s| s.validate().is_ok())
                .unwrap_or(false),
        }
    }

    /// Does it parse (every field without a serde default present)? Validation is not asked.
    fn parses(kind: SpecKind, v: &Value) -> bool {
        match kind {
            SpecKind::Map => serde_json::from_value::<MapSpec>(v.clone()).is_ok(),
            SpecKind::World => serde_json::from_value::<WorldSpec>(v.clone()).is_ok(),
        }
    }

    const KINDS: [SpecKind; 2] = [SpecKind::Map, SpecKind::World];

    #[test]
    fn the_examples_used_here_are_valid() {
        for k in KINDS {
            assert!(accepts(k, &full_example(k)), "{k:?}");
        }
    }

    /// Every object node of the schema, with the allow-list it must match.
    fn object_nodes(kind: SpecKind) -> Vec<(String, Value, &'static [&'static str])> {
        let s = schema(kind);
        match kind {
            SpecKind::Map => {
                let floor = s["properties"]["floors"]["items"].clone();
                vec![
                    ("map".into(), s.clone(), MAP_FIELDS),
                    ("floor".into(), floor.clone(), FLOOR_FIELDS),
                    (
                        "room".into(),
                        floor["properties"]["rooms"]["items"].clone(),
                        ROOM_FIELDS,
                    ),
                    (
                        "link".into(),
                        floor["properties"]["links"]["items"].clone(),
                        LINK_FIELDS,
                    ),
                    (
                        "stair".into(),
                        floor["properties"]["stairs"]["items"].clone(),
                        STAIR_FIELDS,
                    ),
                ]
            }
            SpecKind::World => vec![
                ("world".into(), s.clone(), WORLD_FIELDS),
                (
                    "biome".into(),
                    s["properties"]["biomes"]["items"].clone(),
                    BIOME_FIELDS,
                ),
                (
                    "place".into(),
                    s["properties"]["places"]["items"].clone(),
                    PLACE_FIELDS,
                ),
                (
                    "region".into(),
                    s["properties"]["regions"]["items"].clone(),
                    REGION_FIELDS,
                ),
            ],
        }
    }

    fn keys(v: &Value) -> BTreeSet<String> {
        v.as_object().expect("object").keys().cloned().collect()
    }

    #[test]
    fn a_property_sets_equal_the_allow_lists_and_nothing_else_is_allowed() {
        for k in KINDS {
            for (what, node, allowed) in object_nodes(k) {
                assert_eq!(
                    keys(&node["properties"]),
                    allowed.iter().map(|s| s.to_string()).collect(),
                    "{what}"
                );
                assert_eq!(node["additionalProperties"], json!(false), "{what}");
                assert_eq!(node["type"], "object", "{what}");
                // `required` is a subset of the properties.
                for r in node["required"].as_array().expect("required") {
                    assert!(allowed.contains(&r.as_str().unwrap()), "{what}: {r}");
                }
            }
        }
    }

    #[test]
    fn required_fields_are_exactly_the_ones_the_parser_cannot_default() {
        for k in KINDS {
            let example = full_example(k);
            for (what, node, allowed) in object_nodes(k) {
                let required: BTreeSet<&str> = node["required"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|r| r.as_str().unwrap())
                    .collect();
                for field in allowed {
                    // Remove `field` from every object of this level in the example.
                    let mut v = example.clone();
                    strip(&mut v, &what, field);
                    assert_eq!(
                        parses(k, &v),
                        !required.contains(field),
                        "{what}.{field}: required in the schema = {}",
                        required.contains(field)
                    );
                }
            }
        }
    }

    /// Delete `field` from each object of level `what` (`map`, `floor`, `room`, ...).
    fn strip(v: &mut Value, what: &str, field: &str) {
        let each = |v: &mut Value, key: &str, f: &mut dyn FnMut(&mut Value)| {
            if let Some(a) = v.get_mut(key).and_then(Value::as_array_mut) {
                a.iter_mut().for_each(f);
            }
        };
        let del = |o: &mut Value| {
            o.as_object_mut().unwrap().remove(field);
        };
        match what {
            "map" | "world" => del(v),
            "floor" => each(v, "floors", &mut |o| del(o)),
            "room" | "link" | "stair" => {
                let key = format!("{what}s");
                let key = if what == "stair" {
                    "stairs".to_string()
                } else {
                    key
                };
                each(v, "floors", &mut |fl| each(fl, &key, &mut |o| del(o)));
            }
            "biome" => each(v, "biomes", &mut |o| del(o)),
            "place" => each(v, "places", &mut |o| del(o)),
            "region" => each(v, "regions", &mut |o| del(o)),
            _ => unreachable!(),
        }
    }

    /// One path into a spec.
    #[derive(Clone, Debug)]
    enum Seg {
        Key(String),
        Idx(usize),
    }

    fn at<'a>(v: &'a mut Value, path: &[Seg]) -> &'a mut Value {
        let mut cur = v;
        for s in path {
            cur = match s {
                Seg::Key(k) => cur.get_mut(k).expect("key"),
                Seg::Idx(i) => cur.get_mut(*i).expect("index"),
            };
        }
        cur
    }

    /// Walk the schema alongside the example, listing every node that carries a bound.
    fn bounded(
        schema: &Value,
        example: &Value,
        path: &mut Vec<Seg>,
        out: &mut Vec<(Vec<Seg>, Value)>,
    ) {
        let has_bound = [
            "minimum",
            "maximum",
            "minLength",
            "maxLength",
            "minItems",
            "maxItems",
        ]
        .iter()
        .any(|k| schema.get(*k).is_some());
        if has_bound {
            out.push((path.clone(), schema.clone()));
        }
        match schema["type"].as_str() {
            Some("object") => {
                for (k, sub) in schema["properties"].as_object().unwrap() {
                    if let Some(child) = example.get(k) {
                        path.push(Seg::Key(k.clone()));
                        bounded(sub, child, path, out);
                        path.pop();
                    }
                }
            }
            Some("array") => {
                if let Some(first) = example.as_array().and_then(|a| a.first()) {
                    path.push(Seg::Idx(0));
                    bounded(&schema["items"], first, path, out);
                    path.pop();
                }
            }
            _ => {}
        }
    }

    /// Build an array of `n` items for a bounded array, or None to clone the first item.
    fn build_array(kind: SpecKind, path: &[Seg], n: usize) -> Option<Value> {
        let last = path.iter().rev().find_map(|s| match s {
            Seg::Key(k) => Some(k.as_str()),
            _ => None,
        });
        match (kind, last) {
            (SpecKind::Map, Some("floors")) => Some(Value::Array(
                (0..n)
                    .map(|i| {
                        json!({"name": format!("f{i}"),
                               "rooms":[{"id":"r","name":"Room","w":4,"h":4}]})
                    })
                    .collect(),
            )),
            _ => None,
        }
    }

    #[test]
    fn every_bound_in_the_schema_is_the_bound_the_validator_enforces() {
        let mut checked = 0;
        for k in KINDS {
            let example = full_example(k);
            let mut nodes = Vec::new();
            bounded(&schema(k), &example, &mut Vec::new(), &mut nodes);
            for (path, node) in nodes {
                let mut cases: Vec<(String, Value, bool)> = Vec::new(); // (label, replacement, in_range)
                let g = |n: &str| node.get(n).and_then(Value::as_f64);
                let int = node["type"] == "integer";
                let eps = if int { 1.0 } else { 0.01 };
                let num = |x: f64| if int { json!(x as i64) } else { json!(x) };
                if let (Some(lo), Some(hi)) = (g("minimum"), g("maximum")) {
                    cases.push(("min".into(), num(lo), true));
                    cases.push(("max".into(), num(hi), true));
                    cases.push(("below".into(), num(lo - eps), false));
                    cases.push(("above".into(), num(hi + eps), false));
                }
                if let (Some(lo), Some(hi)) = (g("minLength"), g("maxLength")) {
                    let s = |n: f64| Value::String("a".repeat(n as usize));
                    cases.push(("min".into(), s(lo), true));
                    cases.push(("max".into(), s(hi), true));
                    cases.push(("below".into(), s(lo - 1.0), false));
                    cases.push(("above".into(), s(hi + 1.0), false));
                }
                if node["type"] == "string" && g("maxLength").is_some() && g("minLength").is_none()
                {
                    // An upper bound only (a place note): at the limit is fine, one over is not.
                    let hi = g("maxLength").unwrap();
                    cases.push(("max".into(), Value::String("a".repeat(hi as usize)), true));
                    cases.push((
                        "above".into(),
                        Value::String("a".repeat(hi as usize + 1)),
                        false,
                    ));
                }
                if node["type"] == "array" {
                    let (lo, hi) = (g("minItems"), g("maxItems"));
                    let mut sizes: Vec<(String, usize, bool)> = Vec::new();
                    if let Some(lo) = lo {
                        sizes.push(("min".into(), lo as usize, true));
                        if lo >= 1.0 {
                            sizes.push(("below".into(), lo as usize - 1, false));
                        }
                    }
                    if let Some(hi) = hi {
                        sizes.push(("max".into(), hi as usize, true));
                        sizes.push(("above".into(), hi as usize + 1, false));
                    }
                    for (label, n, ok) in sizes {
                        let mut base = example.clone();
                        let cur = at(&mut base, &path).clone();
                        let arr = build_array(k, &path, n).unwrap_or_else(|| {
                            let item = cur[0].clone();
                            Value::Array(vec![item; n])
                        });
                        cases.push((label, arr, ok));
                    }
                }
                for (label, replacement, in_range) in cases {
                    let mut v = example.clone();
                    // The room list also needs links for the rooms it adds.
                    let is_rooms = path
                        .last()
                        .map(|s| matches!(s, Seg::Key(k) if k == "rooms"))
                        == Some(true);
                    *at(&mut v, &path) = if is_rooms {
                        let n = replacement.as_array().unwrap().len();
                        Value::Array(
                            (0..n)
                                .map(|i| json!({"id": format!("r{i}"), "name": format!("Room {i}"), "w": 4, "h": 4}))
                                .collect(),
                        )
                    } else {
                        replacement
                    };
                    if is_rooms {
                        // Chain-link the new rooms; drop the example's stairs and links.
                        let floor_path = &path[..path.len() - 1];
                        let n = at(&mut v, &path).as_array().unwrap().len();
                        let f = at(&mut v, floor_path);
                        f["links"] = Value::Array(
                            (1..n)
                                .map(|i| json!({"a": format!("r{}", i - 1), "b": format!("r{i}")}))
                                .collect(),
                        );
                        f["stairs"] = json!([]);
                        // The other floor's stairs target is gone with them; nothing else points here.
                    }
                    assert_eq!(
                        accepts(k, &v),
                        in_range,
                        "{k:?} {path:?} {label}: schema says in range = {in_range}"
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 100, "only {checked} bound checks ran");
    }

    #[test]
    fn id_and_colour_patterns_are_the_ones_the_validator_applies() {
        // Every pattern in a schema is one of the two this test knows how to check.
        for k in KINDS {
            fn patterns(v: &Value, out: &mut BTreeSet<String>) {
                match v {
                    Value::Object(o) => {
                        if let Some(p) = o.get("pattern").and_then(Value::as_str) {
                            out.insert(p.to_string());
                        }
                        o.values().for_each(|c| patterns(c, out));
                    }
                    Value::Array(a) => a.iter().for_each(|c| patterns(c, out)),
                    _ => {}
                }
            }
            let mut found = BTreeSet::new();
            patterns(&schema(k), &mut found);
            for p in &found {
                assert!(
                    p == ID_PATTERN || p == COLOR_PATTERN,
                    "unchecked pattern {p}"
                );
            }
        }
        // ID_PATTERN: letters, digits, `_`, `-`, 1-40 characters (so capitals are fine).
        let with_id = |id: &str| {
            let mut v = full_example(SpecKind::Map);
            v["floors"][1]["rooms"][0]["id"] = json!(id);
            v["floors"][0]["stairs"][0]["to_room"] = json!(id);
            v
        };
        for ok in ["a", "Hall_1", "A-b_9", &"x".repeat(40)] {
            assert!(accepts(SpecKind::Map, &with_id(ok)), "{ok}");
        }
        for bad in ["", "a b", "é", "a.b", &"x".repeat(41)] {
            assert!(!accepts(SpecKind::Map, &with_id(bad)), "{bad:?}");
        }
        // COLOR_PATTERN: #rrggbb.
        let with_color = |c: &str| {
            let mut v = full_example(SpecKind::World);
            v["regions"][0]["color"] = json!(c);
            v
        };
        for ok in ["#aabbcc", "#AABBCC", "#012345"] {
            assert!(accepts(SpecKind::World, &with_color(ok)), "{ok}");
        }
        for bad in ["aabbcc", "#abc", "#aabbcg", "#aabbccd"] {
            assert!(!accepts(SpecKind::World, &with_color(bad)), "{bad}");
        }
    }

    /// The serde names of every variant, from an exhaustive match so a new variant fails to
    /// compile here until it is listed (and therefore until the schema is looked at).
    fn space_kinds() -> BTreeSet<String> {
        use ndcart_core::grid::SpaceKind::*;
        [Room, Corridor, Area]
            .into_iter()
            .map(|k| {
                match k {
                    Room | Corridor | Area => {}
                }
                serde_json::to_value(k)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect()
    }
    fn link_kinds() -> BTreeSet<String> {
        use ndcart_core::mapspec::LinkKind::*;
        [Door, Opening, Secret]
            .into_iter()
            .map(|k| {
                match k {
                    Door | Opening | Secret => {}
                }
                serde_json::to_value(k)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect()
    }

    fn enum_of(v: &Value) -> BTreeSet<String> {
        v["enum"]
            .as_array()
            .expect("enum")
            .iter()
            .map(|e| e.as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn every_enum_is_the_set_of_serde_variants_and_nothing_outside_it_parses() {
        let s = schema(SpecKind::Map);
        let floor = &s["properties"]["floors"]["items"]["properties"];
        let room_kind = &floor["rooms"]["items"]["properties"]["kind"];
        let link_kind = &floor["links"]["items"]["properties"]["kind"];
        assert_eq!(enum_of(room_kind), space_kinds());
        assert_eq!(enum_of(link_kind), link_kinds());
        let w = schema(SpecKind::World);
        let biomes: BTreeSet<String> = ndcart_core::scatter::BIOMES
            .iter()
            .map(|b| b.name.to_string())
            .collect();
        assert_eq!(
            enum_of(&w["properties"]["biomes"]["items"]["properties"]["biome"]),
            biomes
        );

        // Each listed value is accepted; a value outside the enum is not.
        for kind in enum_of(room_kind) {
            let mut v = full_example(SpecKind::Map);
            v["floors"][0]["rooms"][1]["kind"] = json!(kind);
            assert!(accepts(SpecKind::Map, &v), "{kind}");
        }
        for kind in enum_of(link_kind) {
            let mut v = full_example(SpecKind::Map);
            v["floors"][0]["links"][0]["kind"] = json!(kind);
            assert!(accepts(SpecKind::Map, &v), "{kind}");
        }
        for b in biomes {
            let mut v = full_example(SpecKind::World);
            v["biomes"][0]["biome"] = json!(b);
            assert!(accepts(SpecKind::World, &v), "{b}");
        }
        let mut v = full_example(SpecKind::Map);
        v["floors"][0]["rooms"][1]["kind"] = json!("door");
        assert!(!accepts(SpecKind::Map, &v));
        let mut v = full_example(SpecKind::Map);
        v["floors"][0]["links"][0]["kind"] = json!("warp");
        assert!(!accepts(SpecKind::Map, &v));
        let mut v = full_example(SpecKind::World);
        v["biomes"][0]["biome"] = json!("lava");
        assert!(!accepts(SpecKind::World, &v));
    }

    #[test]
    fn both_schemas_are_valid_json_with_a_title_and_use_only_grammar_friendly_features() {
        for (k, title) in [(SpecKind::Map, "MapSpec"), (SpecKind::World, "WorldSpec")] {
            let s = schema(k);
            assert_eq!(s["title"], title);
            fn forbidden(v: &Value, path: &str) {
                match v {
                    Value::Object(o) => {
                        for (k, c) in o {
                            assert!(
                                !matches!(
                                    k.as_str(),
                                    "uniqueItems"
                                        | "patternProperties"
                                        | "$ref"
                                        | "$defs"
                                        | "definitions"
                                ),
                                "{path}.{k} is not supported by llama.cpp's schema converter"
                            );
                            forbidden(c, &format!("{path}.{k}"));
                        }
                    }
                    Value::Array(a) => a.iter().for_each(|c| forbidden(c, path)),
                    _ => {}
                }
            }
            forbidden(&s, "$");
        }
    }
}

#[cfg(test)]
mod pack {
    //! The pack's text files agree with the code: examples work, placeholders are filled, every
    //! linter code, material, prop and furnishing keyword is explained, and `pack.toml` lists
    //! exactly the files that exist.
    use super::*;
    use crate::specshape::{unknown_fields, SpecKind};
    use ndcart_core::mapspec::{compile, MapSpec};
    use std::collections::BTreeSet;

    fn is_world(e: &Example) -> bool {
        e.file.contains("world-")
    }

    #[test]
    fn every_example_parses_validates_compiles_and_lints_without_errors() {
        let ex = examples();
        assert!(ex.iter().filter(|e| !is_world(e)).count() >= 8);
        assert!(ex.iter().filter(|e| is_world(e)).count() >= 3);
        for e in &ex {
            assert!(!e.request.is_empty() && !e.why.is_empty(), "{}", e.file);
            if is_world(e) {
                assert!(
                    unknown_fields(&e.spec, SpecKind::World).is_empty(),
                    "{}",
                    e.file
                );
                let w: crate::worldspec::WorldSpec = serde_json::from_value(e.spec.clone())
                    .unwrap_or_else(|er| panic!("{}: {er}", e.file));
                w.validate().unwrap_or_else(|er| panic!("{}: {er}", e.file));
                crate::worldspec::compile(&w, 1, &[])
                    .unwrap_or_else(|er| panic!("{}: {er}", e.file));
            } else {
                assert!(
                    unknown_fields(&e.spec, SpecKind::Map).is_empty(),
                    "{}",
                    e.file
                );
                let m: MapSpec = serde_json::from_value(e.spec.clone())
                    .unwrap_or_else(|er| panic!("{}: {er}", e.file));
                m.validate().unwrap_or_else(|er| panic!("{}: {er}", e.file));
                if e.file.ends_with("map-refine.json") {
                    assert_eq!(m.floors.len(), 1, "a refine reply holds one floor");
                }
                let laid = compile(&m, 1).unwrap_or_else(|er| panic!("{}: {er}", e.file));
                let problems = ndcart_core::speclint::lint_compiled(&m, &laid).unwrap();
                let errors: Vec<_> = problems
                    .iter()
                    .filter(|p| p.severity == ndcart_core::Severity::Error)
                    .collect();
                assert!(errors.is_empty(), "{}: {errors:?}", e.file);
                // Other seeds lay out too (the GM can press "Another layout").
                for seed in [2, 3, 4] {
                    compile(&m, seed).unwrap_or_else(|er| panic!("{} seed {seed}: {er}", e.file));
                }
            }
        }
    }

    #[test]
    fn nothing_rendered_keeps_a_placeholder() {
        for k in [
            Kind::Map,
            Kind::World,
            Kind::Refine,
            Kind::Tags,
            Kind::ThemePack,
        ] {
            assert!(!system(k).contains("{{"), "{k:?}");
        }
        assert!(!reference().contains("{{") && !mistakes().contains("{{"));
        let r = repair("a room is unlinked");
        assert!(r.contains("a room is unlinked") && !r.contains("{{"), "{r}");
        // The raw files use only placeholders `render` knows (or `problems`).
        for (path, text) in FILES.iter().filter(|(p, _)| p.ends_with(".md")) {
            let rendered = render(text).replace("{{problems}}", "");
            assert!(
                !rendered.contains("{{"),
                "{path} has an unknown placeholder"
            );
        }
    }

    #[test]
    fn the_reference_says_which_pack_it_is_and_the_limits_come_from_code() {
        let r = reference();
        assert!(
            r.contains(&format!("pack {} · schema {}", version(), schema_version())),
            "{r}"
        );
        assert!(r.contains(env!("CARGO_PKG_VERSION")));
        assert!(r.contains(&format!("1-{} floors", ndcart_core::mapspec::MAX_FLOORS)));
        assert!(
            system(Kind::Map).contains("1-12 floors") && system(Kind::Map).contains("1-60 rooms")
        );
        assert!(system(Kind::Map).contains("\"links\"") && system(Kind::Map).contains("reachable"));
        assert!(system(Kind::World).contains("biomes") && system(Kind::World).contains("0..1"));
    }

    #[test]
    fn every_linter_code_is_explained_in_the_reference() {
        for code in ndcart_core::lint::ALL_CODES {
            assert!(reference().contains(code), "{code} is not in reference.md");
        }
    }

    #[test]
    fn every_material_prop_and_furnishing_keyword_is_in_the_reference() {
        let r = reference();
        for m in ndcart_paint::starter() {
            assert!(r.contains(m.id), "material {}", m.id);
        }
        for s in ndcart_paint::stamps() {
            assert!(r.contains(s.id), "prop {}", s.id);
        }
        for (group, words) in ndcart_core::furnish::KEYWORDS {
            assert!(r.contains(group), "furnish group {group}");
            for w in *words {
                assert!(r.contains(w), "furnish keyword {w}");
            }
        }
    }

    #[test]
    fn each_system_prompt_names_every_field_of_its_schema() {
        use crate::specshape::*;
        for f in MAP_FIELDS
            .iter()
            .chain(FLOOR_FIELDS)
            .chain(ROOM_FIELDS)
            .chain(LINK_FIELDS)
            .chain(STAIR_FIELDS)
        {
            assert!(
                system(Kind::Map).contains(&format!("\"{f}\"")),
                "map prompt lacks {f}"
            );
        }
        for f in WORLD_FIELDS
            .iter()
            .chain(BIOME_FIELDS)
            .chain(PLACE_FIELDS)
            .chain(REGION_FIELDS)
        {
            assert!(
                system(Kind::World).contains(&format!("\"{f}\"")),
                "world prompt lacks {f}"
            );
        }
        // The refine prompt shares the map schema and names its parts.
        for f in ["rooms", "links", "stairs", "id", "name", "w", "h", "kind"] {
            assert!(system(Kind::Refine).contains(f), "refine prompt lacks {f}");
        }
    }

    #[test]
    fn pack_toml_lists_exactly_the_files_present() {
        let listed: BTreeSet<&str> = manifest().files.iter().map(String::as_str).collect();
        assert_eq!(
            listed.len(),
            manifest().files.len(),
            "a file is listed twice"
        );
        let compiled_in: BTreeSet<&str> = FILES.iter().map(|(p, _)| *p).collect();
        assert_eq!(listed, compiled_in, "pack.toml and FILES differ");
        // On disk: the markdown and JSON files here, and examples/*.json (other files in this
        // folder, such as models.toml or evals/, are not part of the pack).
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/ai");
        let mut on_disk: BTreeSet<String> = BTreeSet::new();
        let mut scan = |dir: &std::path::Path, prefix: &str| {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                let ext = p.extension().and_then(|x| x.to_str()).unwrap_or("");
                if p.is_file() && matches!(ext, "md" | "json") {
                    on_disk.insert(format!("{prefix}{}", e.file_name().to_string_lossy()));
                }
            }
        };
        scan(&root, "");
        scan(&root.join("examples"), "examples/");
        let on_disk: BTreeSet<&str> = on_disk.iter().map(String::as_str).collect();
        assert_eq!(listed, on_disk, "pack.toml and the folder differ");
        for (p, t) in FILES {
            assert!(!t.trim().is_empty(), "{p} is empty");
        }
        // The version is semver-shaped.
        let parts: Vec<&str> = version().split('.').collect();
        assert!(
            parts.len() == 3 && parts.iter().all(|x| x.parse::<u32>().is_ok()),
            "{}",
            version()
        );
    }
}
