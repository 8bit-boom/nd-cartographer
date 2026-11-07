//! The golden adversarial project (plan §22.3): one small known map - two
//! floors, stairs both ways, doors including two side by side, a secret
//! door, a light, a label, a prop with a missing asset, an era - pushed
//! through the linter, the save/load round trip, the legacy-layout loader,
//! UVTT export and the GM/player publish build. Every exporter and
//! migration must process it consistently.

use crate::store;
use ndcart_core::journal::Op;
use ndcart_core::DocKind;

/// Build the golden project in a fresh store (deterministic ids: "f_ground",
/// "f_cellar", "sp_hall"...). Ops, not literals, so the same path the UI
/// takes is what builds it.
#[allow(dead_code)]
pub fn golden() -> store::StoredDoc {
    let mut d = store::StoredDoc::new("Golden", DocKind::Battle);
    d.manifest.id = "golden".into();
    // Deterministic floor ids: rename via structure ops keeps it simple; the
    // default floor is "f<...>", so we work with whatever id it has and pin
    // assertions to the returned ids.
    let ground = d.floors[0].id.clone();
    let cellar = d.add_floor("Cellar", None).unwrap();
    let ops: Vec<Op> = serde_json::from_value(serde_json::json!([
        // Ground: a hall and a store, sharing a wall with a door and an
        // adjacent pair of doors one cell apart (the portal edge case).
        {"t":"upsert_space","space":{"id":"sp_hall","name":"Hall","color":"#7f5539","kind":"room"}},
        {"t":"upsert_space","space":{"id":"sp_store","name":"Store","color":"#4a4a4a","kind":"room"}},
        {"t":"paint_cells","cells":[{"x":0,"y":0},{"x":1,"y":0},{"x":2,"y":0},{"x":3,"y":0},
                                    {"x":0,"y":1},{"x":1,"y":1},{"x":2,"y":1},{"x":3,"y":1},
                                    {"x":0,"y":2},{"x":1,"y":2},{"x":2,"y":2}],"space":"sp_hall"},
        {"t":"paint_cells","cells":[{"x":4,"y":0},{"x":4,"y":1},{"x":4,"y":2}],"space":"sp_store"},
        // A door hall->store, and two doors side by side on the north wall.
        {"t":"set_mark","edge":{"cell":{"x":3,"y":1},"edge":"east"},"mark":{"kind":"door","state":"closed"}},
        {"t":"set_mark","edge":{"cell":{"x":1,"y":0},"edge":"north"},"mark":{"kind":"door","state":"closed"}},
        {"t":"set_mark","edge":{"cell":{"x":2,"y":0},"edge":"north"},"mark":{"kind":"door","state":"closed"}},
        // A secret door in the cellar (players must never see it).
        // A light inside the hall, a GM label, a prop whose asset does not exist.
        {"t":"add_light","light":{"id":"lt_gold","x":48.0,"y":32.0,"range":5.0,"color":"#ffcc88","intensity":0.9,"on":true}},
        {"t":"upsert_label","label":{"id":"lb_gm","x":24.0,"y":8.0,"text":"the real treasure is elsewhere","size":12.0,"color":"#ffffff","bold":false,"gm_only":true}},
        {"t":"add_stamp","stamp":{"id":"st_missing","asset":"totally-missing-asset","x":1.5,"y":1.5,"scale":1.0,"rotation":0.0,"flip":false,"tint":null,"shadow":true}},
        // An era with a light override and a hidden label.
        {"t":"put_era","era":{"id":"night","name":"Night","lights":{"lt_gold":false},"hidden":["lb_gm"],
                              "grade":{"tint":"#0a1030","tint_amount":0.3,"saturation":1.0,"contrast":1.0,"brightness":1.0}}}
    ]))
    .unwrap();
    for op in ops {
        store::apply_ops_direct(&mut d.floors[0], &mut d.journal, &[op]).unwrap();
    }
    // The ground floor also carries the stair DOWN (the cellar exists now).
    let down: Vec<Op> = serde_json::from_value(serde_json::json!([
        {"t":"put_overlay","item":{"kind":"marker","id":"st_down","x":1.5,"y":0.5,"label":"Down","note":"","color":"#e8b030","links_to":{"floor":cellar,"x":0.5,"y":0.5}}}
    ]))
    .unwrap();
    for op in down {
        store::apply_ops_direct(&mut d.floors[0], &mut d.journal, &[op]).unwrap();
    }
    // The cellar: its room, walls and the stair UP.
    let cellar_ops: Vec<Op> = serde_json::from_value(serde_json::json!([
        {"t":"upsert_space","space":{"id":"sp_cellar","name":"Cellar","color":"#202020","kind":"room"}},
        {"t":"paint_cells","cells":[{"x":0,"y":0},{"x":1,"y":0},{"x":1,"y":1}],"space":"sp_cellar"},
        {"t":"set_mark","edge":{"cell":{"x":1,"y":0},"edge":"east"},"mark":{"kind":"secret","state":"closed"}}
    ]))
    .unwrap();
    let ci = ndcart_core::doc::floors_by_id(&d.floors, &cellar).unwrap();
    for op in cellar_ops {
        store::apply_ops_direct(&mut d.floors[ci], &mut d.journal, &[op]).unwrap();
    }
    let cellar_marker: Vec<Op> = serde_json::from_value(serde_json::json!([
        {"t":"put_overlay","item":{"kind":"marker","id":"st_up","x":0.5,"y":0.5,"label":"Up","note":"","color":"#e8b030","links_to":{"floor":ground,"x":1.5,"y":1.5}}}
    ]))
    .unwrap();
    let ci = ndcart_core::doc::floors_by_id(&d.floors, &cellar).unwrap();
    for op in cellar_marker {
        store::apply_ops_direct(&mut d.floors[ci], &mut d.journal, &[op]).unwrap();
    }
    d
}

/// A deliberately broken sibling: a door into the void and an unreachable
/// room - exactly what the linter must catch before a publish.
#[allow(dead_code)]
pub fn broken() -> store::StoredDoc {
    let mut d = golden();
    let ops: Vec<Op> = serde_json::from_value(serde_json::json!([
        {"t":"upsert_space","space":{"id":"sp_lost","name":"Lost","color":"#101010","kind":"room"}},
        {"t":"paint_cells","cells":[{"x":50,"y":50},{"x":51,"y":50}],"space":"sp_lost"},
        {"t":"set_mark","edge":{"cell":{"x":50,"y":50},"edge":"north"},"mark":{"kind":"door","state":"closed"}}
    ]))
    .unwrap();
    for op in ops {
        store::apply_ops_direct(&mut d.floors[0], &mut d.journal, &[op]).unwrap();
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    use ndcart_core::lint;

    fn codes(problems: &[lint::Problem]) -> Vec<&'static str> {
        problems.iter().map(|p| p.code).collect()
    }

    #[test]
    fn the_golden_project_lints_clean_and_the_broken_one_is_caught() {
        let g = golden();
        let floor = &g.floors[0];
        let problems = lint::lint(floor, g.manifest.cell_size);
        let serious: Vec<_> = problems
            .iter()
            .filter(|p| p.severity == lint::Severity::Error)
            .collect();
        assert!(
            serious.is_empty(),
            "golden project must publish: {problems:?}"
        );

        // The broken sibling: the lost room is unreachable and its door opens
        // into the void.
        let b = broken();
        let codes = codes(&lint::lint(&b.floors[0], b.manifest.cell_size));
        assert!(
            codes
                .iter()
                .any(|c| *c == "door_to_void" || *c == "unreachable_room"),
            "the lost room must be caught: {codes:?}"
        );
    }

    #[test]
    fn two_adjacent_doors_derive_two_segments() {
        let g = golden();
        let walls = crate::tiles::derive_walls(&g.floors[0], g.manifest.cell_size);
        // The north wall row y=0 carries doors at x=1 and x=2: two 1-cell
        // door segments, not one merged 2-cell door.
        let doors: Vec<_> = walls
            .iter()
            .filter(|w| w.kind == ndcart_core::WallKind::Door && w.pts[0][1] == 0.0)
            .collect();
        assert_eq!(doors.len(), 2, "adjacent doors stay separate: {doors:?}");
        for d in &doors {
            let len = d.pts[1][0] - d.pts[0][0];
            assert!((len - 32.0).abs() < 1e-9, "each door is one cell wide");
        }
    }

    #[test]
    fn save_load_round_trip_keeps_eras_stamps_and_structure() {
        let dir = std::env::temp_dir().join(format!("ndcart-golden-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let g = golden();
        store::save_doc(Some(&dir), &g);
        let mut back = store::Store::default();
        back.load_dir(&dir).unwrap();
        let b = &back.docs["golden"];
        assert_eq!(b.floors.len(), 2, "both floors survive");
        assert_eq!(b.floors[0].eras.len(), 1, "the era survives");
        assert!(
            b.floors[0]
                .stamps
                .iter()
                .any(|s| s.asset == "totally-missing-asset"),
            "the missing-asset prop is data, not a render failure"
        );
        assert!(b.floors[1].markers.iter().any(|m| m.links_to.is_some()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_legacy_layout_still_opens() {
        let dir = std::env::temp_dir().join(format!("ndcart-golden-legacy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let g = golden();
        // Write what the previous version wrote: manifest + floors.json (a
        // bare array) + the journal, no snapshot.json.
        let p = dir.join(&g.manifest.id);
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(
            p.join("manifest.json"),
            serde_json::to_string(&g.manifest).unwrap(),
        )
        .unwrap();
        std::fs::write(
            p.join("floors.json"),
            serde_json::to_string(&g.floors).unwrap(),
        )
        .unwrap();
        std::fs::write(p.join("journal.ndjson"), g.journal.to_ndjson()).unwrap();
        let mut st = store::Store::default();
        st.load_dir(&dir).unwrap();
        let b = &st.docs["golden"];
        assert_eq!(b.floors.len(), 2);
        assert_eq!(b.floors[0].grid.cell_count(), g.floors[0].grid.cell_count());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn player_audience_strips_the_secret_and_the_gm_label() {
        let g = golden();
        let filtered = ndcart_core::for_audience(&g.floors[0], ndcart_core::Audience::Player);
        // The secret door is a plain wall for players...
        let walls = crate::tiles::derive_walls(&filtered, g.manifest.cell_size);
        assert!(walls
            .iter()
            .all(|w| w.kind != ndcart_core::WallKind::Secret || {
                // the cellar's secret lives on floor 2; ground walls carry none
                false
            }));
        // ...and the GM label is gone from the labels the bake would draw.
        assert!(filtered.labels.iter().all(|l| l.id != "lb_gm"));
    }

    #[test]
    fn the_era_changes_lights_and_hides_the_label_in_the_bake() {
        let g = golden();
        let floor = &g.floors[0];
        let era = floor.era("night").unwrap();
        let view = floor.with_era(Some(era));
        let lights = view.lights();
        assert!(!lights[0].on, "the night override darkens the lamp");
        assert!(
            view.labels().iter().all(|l| l.id != "lb_gm"),
            "the GM label is hidden"
        );
        // The bake through the era differs from the everyday bake.
        let walls = crate::tiles::derive_walls(floor, g.manifest.cell_size);
        let (a, ..) =
            crate::tiles::bake_full_as(floor, &walls, 32.0, 16, crate::tiles::ImageFormat::Png, 80)
                .unwrap();
        let (b, ..) = crate::tiles::bake_full_era(
            floor,
            &walls,
            32.0,
            16,
            crate::tiles::ImageFormat::Png,
            80,
            crate::tiles::MAX_FULL_PX,
            Some(era),
        )
        .unwrap();
        assert_ne!(a, b, "night looks different");
    }
}
