//! The AI instruction pack: the schemas, prompts, reference and examples that tell a model (or
//! an external agent) how to write a spec. The files live in `assets/ai/` and are compiled in
//! with `include_str!`; this module serves them. Everything here is pure (no network, no
//! model), so it works with or without the `bridge` feature.

// Used by the draft prompts and the HTTP/MCP routes as the pack grows (AI 2.2 - 2.4).
#![allow(dead_code)]

/// The JSON Schema (draft 2020-12) a MapSpec reply must follow.
pub const MAPSPEC_SCHEMA: &str = include_str!("../assets/ai/mapspec.schema.json");
/// The JSON Schema a WorldSpec reply must follow.
pub const WORLDSPEC_SCHEMA: &str = include_str!("../assets/ai/worldspec.schema.json");

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
