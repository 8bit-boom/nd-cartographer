//! Floor statistics (plan §21.6): the pacing dashboard's numbers, computed
//! from data the document already holds. Pure and testable - the server
//! serves them, the UI shows them.

use crate::doc::FloorDoc;
use crate::grid::{CellKey, SpaceKind};
use serde::Serialize;

#[derive(Serialize, Clone, PartialEq, Debug, Default)]
pub struct RoomStat {
    pub id: String,
    pub name: String,
    pub kind: &'static str,
    /// Painted squares.
    pub squares: usize,
    /// Whether an encounter waits here (plan 21.3's studio half).
    pub encounter: bool,
    /// The room's own props (whose centre cell falls inside it).
    pub props: usize,
}

#[derive(Serialize, Clone, PartialEq, Debug, Default)]
pub struct FloorStats {
    pub name: String,
    pub floors_total_squares: usize,
    pub rooms: Vec<RoomStat>,
    pub corridors: usize,
    pub areas: usize,
    pub doors: usize,
    pub windows: usize,
    pub secrets: usize,
    pub lights: usize,
    pub lights_on: usize,
    /// Squares any on-light reaches (rough pacing signal, cell centres).
    pub lit_squares: usize,
    pub props: usize,
    pub props_generated: usize,
    pub props_pinned: usize,
    pub labels: usize,
    pub labels_gm_only: usize,
    pub encounters: usize,
    /// Rooms with no link to the world (the lore coverage gap).
    pub rooms_unlinked: usize,
}

fn kind_name(k: SpaceKind) -> &'static str {
    match k {
        SpaceKind::Room => "room",
        SpaceKind::Corridor => "corridor",
        SpaceKind::Area => "area",
    }
}

/// Compute the stats for one floor. `cell_size` converts lights (stored in
/// pixels) to cells, the same conversion everywhere else.
pub fn floor_stats(floor: &FloorDoc, cell_size: f64) -> FloorStats {
    let grid = &floor.grid;
    let mut out = FloorStats {
        name: floor.name.clone(),
        ..Default::default()
    };

    // Squares per space, and the cell -> space index for light coverage and
    // prop attribution.
    let mut squares: std::collections::BTreeMap<String, usize> = Default::default();
    let mut cell_space: std::collections::BTreeMap<(i32, i32), &str> = Default::default();
    for (c, s) in grid.cells() {
        *squares.entry(s.to_string()).or_default() += 1;
        cell_space.insert((c.x, c.y), s);
    }
    out.floors_total_squares = grid.cell_count();

    let encounter_in = |space: &str| floor.encounters.iter().any(|e| e.space == space);
    let linked = |space: &str| floor.entity_links.iter().any(|l| l.space == space);
    let props_in = |space: &str| -> usize {
        floor
            .stamps
            .iter()
            .filter(|s| {
                cell_space
                    .get(&(s.x.floor() as i32, s.y.floor() as i32))
                    .is_some_and(|own| *own == space)
            })
            .count()
    };

    for (id, space) in &grid.spaces {
        let stat = RoomStat {
            id: id.clone(),
            name: space.name.clone(),
            kind: kind_name(space.kind),
            squares: squares.get(id).copied().unwrap_or(0),
            encounter: encounter_in(id),
            props: props_in(id),
        };
        match space.kind {
            SpaceKind::Room => {
                out.rooms.push(stat);
                if !linked(id) {
                    out.rooms_unlinked += 1;
                }
            }
            SpaceKind::Corridor => out.corridors += 1,
            SpaceKind::Area => out.areas += 1,
        }
    }
    out.rooms.sort_by_key(|r| std::cmp::Reverse(r.squares));

    // Doors and kin: every mark, counted once per edge (marks are mirrored
    // between two painted cells, so the canonical id is the unordered cell
    // pair the edge separates).
    let mut seen: std::collections::BTreeSet<(i32, i32, i32, i32)> = Default::default();
    for (key, mark) in &grid.marks {
        let (a, b) = match key.edge {
            crate::grid::Edge::West => ((key.cell.x - 1, key.cell.y), (key.cell.x, key.cell.y)),
            crate::grid::Edge::East => ((key.cell.x, key.cell.y), (key.cell.x + 1, key.cell.y)),
            crate::grid::Edge::North => ((key.cell.x, key.cell.y - 1), (key.cell.x, key.cell.y)),
            crate::grid::Edge::South => ((key.cell.x, key.cell.y), (key.cell.x, key.cell.y + 1)),
        };
        if !seen.insert((a.0, a.1, b.0, b.1)) {
            continue;
        }
        match mark.kind {
            crate::grid::MarkKind::Door => out.doors += 1,
            crate::grid::MarkKind::Window => out.windows += 1,
            crate::grid::MarkKind::Secret => out.secrets += 1,
            crate::grid::MarkKind::Opening => {}
        }
    }

    // Lights and their coverage: a cell is lit when an on light's centre is
    // within its range (in cells) of the cell centre.
    out.lights = floor.lights.len();
    out.lights_on = floor.lights.iter().filter(|l| l.on).count();
    if out.lights_on > 0 && grid.cell_count() > 0 {
        let mut lit = std::collections::BTreeSet::new();
        for l in floor.lights.iter().filter(|l| l.on) {
            let (lx, ly) = (l.x / cell_size, l.y / cell_size);
            let r = l.range.ceil() as i32;
            let cx = lx.floor() as i32;
            let cy = ly.floor() as i32;
            for x in cx - r..=cx + r {
                for y in cy - r..=cy + r {
                    let dx = f64::from(x) + 0.5 - lx;
                    let dy = f64::from(y) + 0.5 - ly;
                    if dx * dx + dy * dy <= l.range * l.range && cell_space.contains_key(&(x, y)) {
                        lit.insert((x, y));
                    }
                }
            }
        }
        out.lit_squares = lit.len();
    }

    out.props = floor.stamps.len();
    out.props_generated = floor
        .stamps
        .iter()
        .filter(|s| s.gen.as_ref().is_some_and(|g| !g.pinned))
        .count();
    out.props_pinned = floor
        .stamps
        .iter()
        .filter(|s| s.gen.as_ref().is_some_and(|g| g.pinned))
        .count();
    out.labels = floor.labels.len();
    out.labels_gm_only = floor.labels.iter().filter(|l| l.gm_only).count();
    out.encounters = floor.encounters.len();
    out
}

/// Light coverage as a fraction (0..=1) of the painted squares.
pub fn lit_fraction(stats: &FloorStats) -> f32 {
    if stats.floors_total_squares == 0 {
        return 0.0;
    }
    stats.lit_squares as f32 / stats.floors_total_squares as f32
}

/// A prop placed by the plan's stamp model covers the cell its centre is in
/// (footprint boxes are metadata v2's furnishing concern, not the stats').
pub fn cells_of(cells: impl Iterator<Item = CellKey>) -> usize {
    cells.count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::Op;
    use crate::Journal;

    fn kitchen_floor() -> FloorDoc {
        let mut f = FloorDoc::new("f".into(), "Vault".into());
        let mut j = Journal::default();
        let ops: Vec<Op> = serde_json::from_value(serde_json::json!([
            {"t":"upsert_space","space":{"id":"hall","name":"Great Hall","color":"#7f5539","kind":"room"}},
            {"t":"upsert_space","space":{"id":"pass","name":"Passage","color":"#333333","kind":"corridor"}},
            {"t":"paint_cells","cells":[{"x":0,"y":0},{"x":1,"y":0},{"x":0,"y":1},{"x":1,"y":1}],"space":"hall"},
            {"t":"paint_cells","cells":[{"x":2,"y":0}],"space":"pass"},
            {"t":"set_mark","edge":{"cell":{"x":1,"y":0},"edge":"east"},"mark":{"kind":"door","state":"closed"}},
            {"t":"set_mark","edge":{"cell":{"x":2,"y":0},"edge":"north"},"mark":{"kind":"window","state":"closed"}},
            {"t":"add_light","light":{"id":"l","x":32.0,"y":32.0,"range":5.0,"color":"#ffcc88","intensity":0.9,"on":true}},
            {"t":"add_stamp","stamp":{"id":"s1","asset":"crate","x":0.5,"y":0.5,"scale":1.0,"rotation":0.0,"flip":false,"tint":null,"shadow":true}},
            {"t":"upsert_label","label":{"id":"n1","x":8.0,"y":8.0,"text":"spooky","size":12.0,"color":"#ffffff","bold":false,"gm_only":true}}
        ]))
        .unwrap();
        for op in ops {
            j.push(&mut f, &op).unwrap();
        }
        f
    }

    #[test]
    fn the_dashboard_counts_what_the_floor_holds() {
        let f = kitchen_floor();
        let s = floor_stats(&f, 32.0);
        assert_eq!(s.floors_total_squares, 5);
        assert_eq!(s.rooms.len(), 1);
        assert_eq!(s.rooms[0].name, "Great Hall");
        assert_eq!(s.rooms[0].squares, 4);
        assert_eq!(s.corridors, 1);
        assert_eq!(s.doors, 1);
        assert_eq!(s.windows, 1);
        assert_eq!((s.lights, s.lights_on), (1, 1));
        // The lamp at cell (1,1) with range 5 covers both painted rooms.
        assert_eq!(s.lit_squares, 5);
        assert_eq!((s.props, s.labels, s.labels_gm_only), (1, 1, 1));
        assert_eq!(s.rooms_unlinked, 1, "no entity link yet");
        // Lit fraction sanity.
        assert!((lit_fraction(&s) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn encounters_and_links_show_up_per_room() {
        let mut f = kitchen_floor();
        let ops: Vec<Op> = serde_json::from_value(serde_json::json!([
            {"t":"link_entity","link":{"space":"hall","entity_id":9,"name":"The Gilded Mug","kind":"location"}},
            {"t":"put_encounter","encounter":{"id":"e","space":"pass","creatures":["rat swarm"],"note":""}}
        ]))
        .unwrap();
        for op in ops {
            f.put_encounter_adopt(&op);
        }
        let s = floor_stats(&f, 32.0);
        assert!(!s.rooms[0].encounter);
        assert_eq!(s.rooms_unlinked, 0, "the hall is linked now");
        assert_eq!(s.encounters, 1);
    }

    #[test]
    fn an_off_light_does_not_light_anything() {
        let mut f = kitchen_floor();
        for l in &mut f.lights {
            l.on = false;
        }
        let s = floor_stats(&f, 32.0);
        assert_eq!(s.lights_on, 0);
        assert_eq!(s.lit_squares, 0);
    }

    // A tiny adapter so the second test can apply link/encounter ops without
    // a journal: they are ordinary ops and apply_op handles them.
    trait ApplyExt {
        fn put_encounter_adopt(&mut self, op: &Op);
    }
    impl ApplyExt for FloorDoc {
        fn put_encounter_adopt(&mut self, op: &Op) {
            crate::journal::apply_op(self, op).unwrap();
        }
    }
}
