//! A large map in steps (plan 3.7): ask for an outline of the floors first, then for each
//! floor on its own, with the outline in front of the model. Each floor has its own repair
//! loop, so a broken floor 2 is asked about again without floors 1 and 3. Stairs are added
//! here, from the outline's `links_to`, because they are the one part that needs every floor.

use crate::aipack::{self, Kind};
use crate::draft::{solve, Task};
use crate::repair::Progress;
use ndcart_core::mapspec::{FloorSpec, MapSpec, StairSpec};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const MAX_OUTLINE_FLOORS: usize = 6;
pub const MAX_OUTLINE_ROOMS: usize = 20;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Outline {
    pub name: String,
    pub floors: Vec<OutlineFloor>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OutlineFloor {
    pub name: String,
    pub purpose: String,
    pub rooms: usize,
    #[serde(default)]
    pub links_to: Vec<String>,
}

/// Judge an outline reply; every complaint comes back.
pub fn judge_outline(reply: &str) -> Result<Outline, Vec<String>> {
    let json = crate::speccheck::extract_json(reply).ok_or_else(|| {
        vec!["The reply contained no JSON object. Reply with only the JSON object.".to_string()]
    })?;
    let o: Outline = serde_json::from_str(json)
        .map_err(|e| vec![format!("The JSON did not match the schema: {e}.")])?;
    let mut bad = Vec::new();
    let n = o.name.chars().count();
    if n == 0 || n > 80 {
        bad.push("name must be 1-80 characters.".to_string());
    }
    if o.floors.is_empty() || o.floors.len() > MAX_OUTLINE_FLOORS {
        bad.push(format!("floors must hold 1-{MAX_OUTLINE_FLOORS} floors."));
        return Err(bad);
    }
    let names: BTreeSet<&str> = o.floors.iter().map(|f| f.name.as_str()).collect();
    if names.len() != o.floors.len() || names.iter().any(|n| n.is_empty() || n.chars().count() > 40)
    {
        bad.push("floor names must be distinct and 1-40 characters.".to_string());
    }
    for f in &o.floors {
        if !(1..=MAX_OUTLINE_ROOMS).contains(&f.rooms) {
            bad.push(format!(
                "floor '{}': rooms must be 1-{MAX_OUTLINE_ROOMS}.",
                f.name
            ));
        }
        for l in &f.links_to {
            if l == &f.name || !names.contains(l.as_str()) {
                bad.push(format!(
                    "floor '{}': links_to names '{l}', which is not another floor of this outline.",
                    f.name
                ));
            }
        }
    }
    if bad.is_empty() {
        // Every floor must be reachable from the first through links_to, in either direction.
        let mut seen: BTreeSet<&str> = BTreeSet::from([o.floors[0].name.as_str()]);
        let mut grew = true;
        while grew {
            grew = false;
            for f in &o.floors {
                for l in &f.links_to {
                    let (a, b) = (seen.contains(f.name.as_str()), seen.contains(l.as_str()));
                    if a != b {
                        seen.insert(if a { l.as_str() } else { f.name.as_str() });
                        grew = true;
                    }
                }
            }
        }
        if let Some(f) = o.floors.iter().find(|f| !seen.contains(f.name.as_str())) {
            bad.push(format!(
                "floor '{}' is not connected to '{}' through links_to; every floor needs a stairs path from the first.",
                f.name, o.floors[0].name
            ));
        }
    }
    if bad.is_empty() {
        Ok(o)
    } else {
        Err(bad)
    }
}

/// The request for floor `index` (0-based): the whole plan, this floor's part of it, and the
/// room ids of the floors already drawn.
pub fn floor_user_turn(
    prompt: &str,
    outline: &Outline,
    index: usize,
    made: &[FloorSpec],
) -> String {
    let f = &outline.floors[index];
    let plan = serde_json::to_string(outline).unwrap_or_default();
    let mut text = format!(
        "Design this map: {}\n\nThe plan for the whole map:\n{plan}\n\nNow write ONLY floor {} of {}: \"{}\" ({}), about {} rooms. Reply with a map spec whose floors array holds exactly that one floor, named \"{}\". Do not add stairs: they are added for you once every floor exists.",
        prompt.trim(),
        index + 1,
        outline.floors.len(),
        f.name,
        f.purpose,
        f.rooms,
        f.name
    );
    if !made.is_empty() {
        text.push_str(
            "\n\nFloors already made (room ids), so names and ids stay distinct and in style:",
        );
        for m in made {
            let ids: Vec<&str> = m.rooms.iter().map(|r| r.id.as_str()).collect();
            text.push_str(&format!("\n- {}: {}", m.name, ids.join(", ")));
        }
    }
    text
}

/// Judge one floor reply: a valid one-floor spec. Its stairs are dropped (ours come later) and
/// its name is the outline's.
pub fn judge_floor(name: String) -> impl Fn(&str) -> Result<FloorSpec, Vec<String>> {
    move |reply: &str| {
        let json = crate::speccheck::extract_json(reply).ok_or_else(|| {
            vec!["The reply contained no JSON object. Reply with only the JSON object.".to_string()]
        })?;
        let mut v: serde_json::Value = serde_json::from_str(json)
            .map_err(|e| vec![format!("The JSON did not match the schema: {e}.")])?;
        if let Some(fs) = v.get_mut("floors").and_then(|f| f.as_array_mut()) {
            if fs.len() != 1 {
                return Err(vec![format!(
                    "The spec must have exactly one floor ({}), not {}.",
                    name,
                    fs.len()
                )]);
            }
            if let Some(o) = fs[0].as_object_mut() {
                o.remove("stairs");
                o.insert("name".into(), name.clone().into());
            }
        }
        let c = crate::speccheck::check_map(&v.to_string());
        if !c.errors.is_empty() {
            return Err(c.errors);
        }
        let spec = c
            .spec
            .ok_or_else(|| vec!["The spec could not be read.".to_string()])?;
        Ok(spec.floors.into_iter().next().expect("one floor"))
    }
}

/// Put the floors together and add stairs in both directions for every `links_to` pair.
pub fn merge(outline: &Outline, mut floors: Vec<FloorSpec>) -> MapSpec {
    let mut used = vec![0usize; floors.len()];
    let mut done: BTreeSet<(usize, usize)> = BTreeSet::new();
    for (i, of) in outline.floors.iter().enumerate() {
        for l in &of.links_to {
            let Some(j) = outline.floors.iter().position(|f| &f.name == l) else {
                continue;
            };
            let key = (i.min(j), i.max(j));
            if i == j || !done.insert(key) {
                continue;
            }
            let pick = |idx: usize, used: &mut Vec<usize>, floors: &[FloorSpec]| {
                let rooms = &floors[idx].rooms;
                let r = rooms[used[idx] % rooms.len()].id.clone();
                used[idx] += 1;
                r
            };
            let (ri, rj) = (pick(i, &mut used, &floors), pick(j, &mut used, &floors));
            let (ni, nj) = (floors[i].name.clone(), floors[j].name.clone());
            floors[i].stairs.push(StairSpec {
                room: ri.clone(),
                to_floor: nj,
                to_room: Some(rj.clone()),
                label: None,
            });
            floors[j].stairs.push(StairSpec {
                room: rj,
                to_floor: ni,
                to_room: Some(ri),
                label: None,
            });
        }
    }
    MapSpec {
        name: outline.name.clone(),
        floors,
    }
}

/// Plan, then draw each floor, then merge and check the whole. Returns the spec, the asks it
/// took in all, and the seconds.
pub async fn draft(
    cfg: &crate::draft::Config,
    backend: &crate::draft::Backend,
    system: &str,
    prompt: &str,
    progress: &(dyn Fn(Progress) + Sync),
) -> Result<(MapSpec, usize, f64), String> {
    progress(Progress::Step("outline".into()));
    let plan = solve(
        backend,
        Task {
            cache: &cfg.cache,
            system: aipack::system(Kind::Outline),
            kind: Kind::Outline,
            first_user: format!("Plan this map: {}", prompt.trim()),
            rounds: 2,
            what: "outline",
        },
        progress,
        judge_outline,
    )
    .await?;
    let (outline, mut attempts, mut seconds) = (plan.value, plan.attempts, plan.seconds);
    let mut floors: Vec<FloorSpec> = Vec::new();
    for i in 0..outline.floors.len() {
        progress(Progress::Step(format!(
            "floor {}/{}",
            i + 1,
            outline.floors.len()
        )));
        let out = solve(
            backend,
            Task {
                cache: &cfg.cache,
                system,
                kind: Kind::Map,
                first_user: floor_user_turn(prompt, &outline, i, &floors),
                rounds: 2,
                what: "floor",
            },
            progress,
            judge_floor(outline.floors[i].name.clone()),
        )
        .await?;
        attempts += out.attempts;
        seconds += out.seconds;
        floors.push(out.value);
    }
    let spec = merge(&outline, floors);
    let json = serde_json::to_string(&spec).map_err(|e| e.to_string())?;
    let c = crate::speccheck::check_map(&json);
    if let Some(e) = c.errors.first() {
        return Err(format!("the merged map is not valid: {e}"));
    }
    Ok((spec, attempts, seconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outline(links: &[(&str, &[&str])]) -> String {
        let floors: Vec<serde_json::Value> = links
            .iter()
            .map(|(n, l)| serde_json::json!({"name": n, "purpose": "p", "rooms": 4, "links_to": l}))
            .collect();
        serde_json::json!({"name": "Keep", "floors": floors}).to_string()
    }

    #[test]
    fn an_outline_must_name_real_floors_and_connect_them() {
        assert!(judge_outline(&outline(&[("A", &["B"]), ("B", &[])])).is_ok());
        let e = judge_outline(&outline(&[("A", &["Z"]), ("B", &[])])).unwrap_err();
        assert!(e.iter().any(|c| c.contains("'Z'")), "{e:?}");
        let e = judge_outline(&outline(&[("A", &[]), ("B", &[])])).unwrap_err();
        assert!(e.iter().any(|c| c.contains("not connected")), "{e:?}");
        let e = judge_outline(&outline(&[("A", &[]), ("A", &[])])).unwrap_err();
        assert!(e.iter().any(|c| c.contains("distinct")), "{e:?}");
        assert!(judge_outline(r#"{"name":"K","floors":[],"extra":1}"#).is_err());
    }

    #[test]
    fn merging_adds_stairs_both_ways_in_distinct_rooms() {
        let o = judge_outline(&outline(&[("A", &["B"]), ("B", &["C"]), ("C", &[])])).unwrap();
        let floor = |n: &str| FloorSpec {
            name: n.into(),
            rooms: ["r1", "r2"]
                .iter()
                .map(|id| ndcart_core::mapspec::RoomSpec {
                    id: id.to_string(),
                    name: id.to_string(),
                    kind: ndcart_core::SpaceKind::Room,
                    w: 5,
                    h: 5,
                    lit: true,
                })
                .collect(),
            links: vec![ndcart_core::mapspec::LinkSpec {
                a: "r1".into(),
                b: "r2".into(),
                kind: ndcart_core::mapspec::LinkKind::Door,
            }],
            stairs: vec![],
        };
        let m = merge(&o, vec![floor("A"), floor("B"), floor("C")]);
        assert_eq!(m.floors[0].stairs.len(), 1);
        assert_eq!(m.floors[1].stairs.len(), 2);
        assert_eq!(m.floors[2].stairs.len(), 1);
        let rooms: Vec<&str> = m.floors[1].stairs.iter().map(|s| s.room.as_str()).collect();
        assert_ne!(rooms[0], rooms[1]);
        assert!(m.validate().is_ok());
    }

    #[test]
    fn a_floor_reply_loses_its_stairs_and_takes_the_outlines_name() {
        let judge = judge_floor("Lower".into());
        let reply = r#"{"name":"X","floors":[{"name":"Whatever","rooms":[{"id":"a","name":"A","w":5,"h":5},{"id":"b","name":"B","w":5,"h":5}],"links":[{"a":"a","b":"b"}],"stairs":[{"room":"a","to_floor":"Up"}]}]}"#;
        let f = judge(reply).unwrap();
        assert_eq!((f.name.as_str(), f.stairs.len()), ("Lower", 0));
        let two = reply.replace(
            r#""floors":[{"#,
            r#""floors":[{"name":"Z","rooms":[],"links":[]},{"#,
        );
        assert!(judge(&two).unwrap_err()[0].contains("exactly one floor"));
    }
}
