//! Spec round-trip (plan §21.19): read a painted floor back as a MapSpec, so
//! a map stays re-layoutable — the painted rooms become the spec's rooms
//! (sizes, kinds, names), its doors and openings become links, its stairs
//! become stairs. Re-layout = compile the extracted spec with a new seed into
//! a proposal; the everyday floor is untouched until the GM accepts.

use crate::doc::FloorDoc;
use crate::error::{CoreError, CoreResult};
use crate::grid::{CellKey, MarkKind};
use crate::mapspec::{FloorSpec, LinkKind, LinkSpec, RoomSpec, StairSpec};
use std::collections::BTreeSet;

/// Read a floor back as a floor spec. Warnings report what could not be
/// carried across (a room over the 30-square spec cap was clamped; a stair
/// whose landing has no room was dropped).
pub fn extract_floor_spec(
    floor: &FloorDoc,
    floors: &[FloorDoc],
    cell_size: f64,
) -> CoreResult<(FloorSpec, Vec<String>)> {
    let mut warnings = Vec::new();
    // Lights are stored in pixels at the document's cell size; rooms and
    // markers live in cells.
    let cell = |x: f32, y: f32| {
        CellKey::new(
            (f64::from(x) / cell_size).floor() as i32,
            (f64::from(y) / cell_size).floor() as i32,
        )
    };

    // Rooms: one per space, sized by its painted bounding box.
    let mut rooms = Vec::new();
    for (id, space) in &floor.grid.spaces {
        let cells: Vec<CellKey> = floor
            .grid
            .cells()
            .filter(|(_, s)| *s == id)
            .map(|(c, _)| c)
            .collect();
        if cells.is_empty() {
            warnings.push(format!(
                "space '{}' ({id}) has no painted cells and was skipped",
                space.name
            ));
            continue;
        }
        let (min_x, min_y) = (
            cells.iter().map(|c| c.x).min().unwrap(),
            cells.iter().map(|c| c.y).min().unwrap(),
        );
        let (max_x, max_y) = (
            cells.iter().map(|c| c.x).max().unwrap(),
            cells.iter().map(|c| c.y).max().unwrap(),
        );
        let w = (max_x - min_x + 1).clamp(2, 30);
        let h = (max_y - min_y + 1).clamp(2, 30);
        if max_x - min_x + 1 > 30 || max_y - min_y + 1 > 30 {
            warnings.push(format!(
                "room '{}' is bigger than the spec's 30-square cap and was clamped to {w}x{h}",
                space.name
            ));
        }
        // A lamp in the room: any light whose cell falls inside the box.
        let lit = floor.lights.iter().any(|l| {
            let c = cell(l.x as f32, l.y as f32);
            c.x >= min_x && c.x <= max_x && c.y >= min_y && c.y <= max_y
        });
        let room_id = ok_id(id).ok_or_else(|| {
            CoreError::Invalid(format!(
                "space id '{id}' is not usable in a spec (1..=40 chars of a-z 0-9 _ -)"
            ))
        })?;
        rooms.push(RoomSpec {
            id: room_id,
            name: if space.name.is_empty() {
                id.clone()
            } else {
                space.name.clone()
            },
            kind: space.kind,
            w,
            h,
            lit,
        });
    }
    if rooms.is_empty() {
        return Err(CoreError::Invalid(
            "nothing to extract: the floor has no painted rooms".into(),
        ));
    }

    // Links: edges between two different spaces that carry a mark. A plain
    // shared edge is a wall, not a connection.
    let kind_of = |m: crate::grid::Mark| match m.kind {
        MarkKind::Door => Some(LinkKind::Door),
        MarkKind::Secret => Some(LinkKind::Secret),
        MarkKind::Opening => Some(LinkKind::Opening),
        MarkKind::Window => None,
    };
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    let mut links = Vec::new();
    for (cell, space) in floor.grid.cells() {
        for (dir, edge) in [
            (cell.east(), crate::grid::Edge::East),
            (cell.south(), crate::grid::Edge::South),
        ] {
            let Some(other) = floor.grid.cell_space(dir) else {
                continue;
            };
            if other == space {
                continue;
            }
            let Some(mark) = floor.grid.mark_between(cell, edge) else {
                continue;
            };
            let Some(kind) = kind_of(mark) else { continue };
            let (a, b) = (space.to_string(), other.to_string());
            let pair = if a < b { (a, b) } else { (b, a) };
            if !seen.insert(pair.clone()) {
                continue;
            }
            links.push(LinkSpec {
                a: pair.0,
                b: pair.1,
                kind,
            });
        }
    }

    // Stairs: markers that lead to another floor of this document.
    let mut stairs = Vec::new();
    for m in &floor.markers {
        let Some(l) = &m.links_to else { continue };
        let here = CellKey::new(m.x.floor() as i32, m.y.floor() as i32);
        let Some(room) = floor.grid.cell_space(here) else {
            warnings.push(format!(
                "stair marker '{}' is not on a painted room and was skipped",
                m.label
            ));
            continue;
        };
        let Some(target) = floors.iter().find(|f| f.id == l.floor) else {
            warnings.push(format!(
                "stair marker '{}' leads to a floor that no longer exists and was skipped",
                m.label
            ));
            continue;
        };
        let to_room =
            l.x.zip(l.y)
                .map(|(x, y)| CellKey::new(x.floor() as i32, y.floor() as i32))
                .and_then(|c| floor_land_room(target, c));
        stairs.push(StairSpec {
            room: ok_id(room).unwrap_or_else(|| "hall".into()),
            to_floor: target.name.clone(),
            to_room,
            label: Some(m.label.clone()).filter(|s| !s.is_empty()),
        });
    }

    Ok((
        FloorSpec {
            name: floor.name.clone(),
            rooms,
            links,
            stairs,
        },
        warnings,
    ))
}

/// The space id under a stair's landing cell, if any.
fn floor_land_room(target: &FloorDoc, c: CellKey) -> Option<String> {
    target
        .grid
        .cell_space(c)
        .map(|s| ok_id(s).unwrap_or_else(|| "hall".into()))
}

/// Spec ids are 1..=40 chars of a-z 0-9 _ - (the same rule mapspec applies).
fn ok_id(s: &str) -> Option<String> {
    let ok = !s.is_empty()
        && s.len() <= 40
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    ok.then(|| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::{default_space, SpaceKind};
    use crate::journal::Op;

    #[test]
    fn a_painted_floor_reads_back_as_a_spec() {
        let mut f = FloorDoc::new("f1".into(), "Vault".into());
        let ops: Vec<Op> = serde_json::from_value(serde_json::json!([
            {"t":"upsert_space","space":{"id":"hall","name":"Hall","color":"#7f5539","kind":"room"}},
            {"t":"upsert_space","space":{"id":"store","name":"Store","color":"#4a4a4a","kind":"room"}},
            {"t":"paint_cells","cells":[{"x":0,"y":0},{"x":1,"y":0},{"x":2,"y":0}],"space":"hall"},
            {"t":"paint_cells","cells":[{"x":3,"y":0}],"space":"store"},
            {"t":"set_mark","edge":{"cell":{"x":2,"y":0},"edge":"east"},
             "mark":{"kind":"door","state":"closed"}},
            {"t":"add_light","light":{"id":"l","x":16.0,"y":16.0,"range":4.0,"color":"#ffcc88","intensity":0.8,"on":true}}
        ]))
        .unwrap();
        for op in &ops {
            crate::journal::apply_op(&mut f, op).unwrap();
        }

        let (spec, warnings) = extract_floor_spec(&f, std::slice::from_ref(&f), 32.0).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(spec.rooms.len(), 2);
        let hall = spec.rooms.iter().find(|r| r.id == "hall").unwrap();
        // The spec's minimum room is 2 squares; the 1-tall hall clamps up.
        assert_eq!((hall.w, hall.h), (3, 2));
        assert!(hall.lit, "the lamp sits inside the hall");
        assert_eq!(spec.links.len(), 1);
        assert_eq!(spec.links[0].a, "hall");
        assert_eq!(spec.links[0].b, "store");
        assert_eq!(spec.links[0].kind, LinkKind::Door);

        // The extracted spec compiles: the round trip closes.
        let compiled = crate::mapspec::compile_floor(&spec, 7).unwrap();
        assert_eq!(compiled.name, "Vault");
    }

    #[test]
    fn stairs_and_openings_come_across() {
        let mut a = FloorDoc::new("fa".into(), "Ground".into());
        let mut b = FloorDoc::new("fb".into(), "Cellar".into());
        let ops: Vec<Op> = serde_json::from_value(serde_json::json!([
            {"t":"upsert_space","space":{"id":"hall","name":"Hall","color":"#7f5539","kind":"room"}},
            {"t":"paint_cells","cells":[{"x":0,"y":0},{"x":1,"y":0}],"space":"hall"},
            {"t":"set_mark","edge":{"cell":{"x":0,"y":0},"edge":"north"},
             "mark":{"kind":"opening","state":"open"}},
            {"t":"put_overlay","item":{"kind":"marker","id":"st1","x":0.5,"y":0.5,"label":"Down",
             "note":"","color":"#e8b030","links_to":{"floor":"fb","x":0.5,"y":0.5}}}
        ]))
        .unwrap();
        for op in &ops {
            crate::journal::apply_op(&mut a, op).unwrap();
        }
        b.grid
            .upsert_space(default_space("cellar", "Cellar", SpaceKind::Room))
            .unwrap();
        b.grid.paint(&[CellKey::new(0, 0)], Some("cellar")).unwrap();

        let (spec, warnings) = extract_floor_spec(&a, &[a.clone(), b.clone()], 32.0).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(spec.links.len(), 0, "an opening to nowhere is no room link");
        assert_eq!(spec.stairs.len(), 1);
        assert_eq!(spec.stairs[0].room, "hall");
        assert_eq!(spec.stairs[0].to_floor, "Cellar");
        assert_eq!(spec.stairs[0].to_room.as_deref(), Some("cellar"));
    }

    #[test]
    fn big_rooms_are_clamped_with_a_warning() {
        let mut f = FloorDoc::new("f".into(), "Wide".into());
        f.grid
            .upsert_space(default_space("a", "Wide Hall", SpaceKind::Corridor))
            .unwrap();
        let cells: Vec<CellKey> = (0..40).map(|x| CellKey::new(x, 0)).collect();
        f.grid.paint(&cells, Some("a")).unwrap();
        let (spec, warnings) = extract_floor_spec(&f, std::slice::from_ref(&f), 32.0).unwrap();
        assert_eq!(spec.rooms[0].w, 30);
        assert!(warnings.iter().any(|w| w.contains("clamped")));
    }
}
