//! The outliner's data (plan §7.3, §17.8): the floor as a tree — rooms with
//! their size and extent, lights, labels. Computed here so the UI only renders
//! it (no business math in `ui/`), and so the same summary can feed AI prompts
//! later. "The outliner is the accessible map": a structure is already text.

use crate::doc::FloorDoc;
use crate::grid::SpaceKind;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Serialize, Clone, PartialEq, Debug)]
pub struct OutlineSpace {
    pub id: String,
    pub name: String,
    pub kind: SpaceKind,
    pub color: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub material: Option<String>,
    pub cells: usize,
    /// `[min_x, min_y, max_x, max_y]` in cells; `None` when nothing is painted.
    pub bbox: Option<[i32; 4]>,
}

#[derive(Serialize, Clone, PartialEq, Debug)]
pub struct OutlineLight {
    pub id: String,
    pub label: String,
    pub on: bool,
    /// The cell the light sits in (for "go to").
    pub cell: [i32; 2],
}

#[derive(Serialize, Clone, PartialEq, Debug)]
pub struct OutlineLabel {
    pub id: String,
    pub text: String,
    pub gm_only: bool,
    pub cell: [i32; 2],
}

#[derive(Serialize, Clone, PartialEq, Debug)]
pub struct OutlineStamp {
    pub id: String,
    pub asset: String,
    pub cell: [i32; 2],
}

#[derive(Serialize, Clone, PartialEq, Debug)]
pub struct Outline {
    pub stamps: Vec<OutlineStamp>,
    pub spaces: Vec<OutlineSpace>,
    pub lights: Vec<OutlineLight>,
    pub labels: Vec<OutlineLabel>,
}

fn cell_of(x: f64, y: f64, cell_size: f64) -> [i32; 2] {
    [
        (x / cell_size).floor() as i32,
        (y / cell_size).floor() as i32,
    ]
}

/// Rooms sort by name (then id) so the tree is stable between edits.
pub fn outline(floor: &FloorDoc, cell_size: f64) -> Outline {
    let mut acc: BTreeMap<&str, (usize, [i32; 4])> = BTreeMap::new();
    for (c, space) in floor.grid.cells() {
        let e = acc
            .entry(space)
            .or_insert((0, [i32::MAX, i32::MAX, i32::MIN, i32::MIN]));
        e.0 += 1;
        e.1 = [
            e.1[0].min(c.x),
            e.1[1].min(c.y),
            e.1[2].max(c.x),
            e.1[3].max(c.y),
        ];
    }
    let mut spaces: Vec<OutlineSpace> = floor
        .grid
        .spaces
        .values()
        .map(|s| {
            let (cells, bbox) = acc
                .get(s.id.as_str())
                .map(|(n, b)| (*n, Some(*b)))
                .unwrap_or((0, None));
            OutlineSpace {
                id: s.id.clone(),
                name: s.name.clone(),
                kind: s.kind,
                color: s.color.clone(),
                material: s.material.clone(),
                cells,
                bbox,
            }
        })
        .collect();
    spaces.sort_by(|a, b| (a.name.to_lowercase(), &a.id).cmp(&(b.name.to_lowercase(), &b.id)));
    Outline {
        stamps: floor
            .stamps
            .iter()
            .map(|s| OutlineStamp {
                id: s.id.clone(),
                asset: s.asset.clone(),
                cell: [s.x.floor() as i32, s.y.floor() as i32],
            })
            .collect(),
        spaces,
        lights: floor
            .lights
            .iter()
            .map(|l| OutlineLight {
                id: l.id.clone(),
                label: l.label.clone().unwrap_or_else(|| format!("Light {}", l.id)),
                on: l.on,
                cell: cell_of(l.x, l.y, cell_size),
            })
            .collect(),
        labels: floor
            .labels
            .iter()
            .map(|l| OutlineLabel {
                id: l.id.clone(),
                text: l.text.clone(),
                gm_only: l.gm_only,
                // Labels are in cells already (lights are in lattice pixels).
                cell: [l.x.floor() as i32, l.y.floor() as i32],
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{Label, Light};
    use crate::grid::{default_space, CellKey};

    #[test]
    fn rooms_have_counts_and_extents_sorted_by_name() {
        let mut f = FloorDoc::new("f".into(), "G".into());
        f.grid
            .upsert_space(default_space("z", "armoury", SpaceKind::Room))
            .unwrap();
        f.grid
            .upsert_space(default_space("a", "Zed Hall", SpaceKind::Room))
            .unwrap();
        f.grid
            .upsert_space(default_space("e", "Empty", SpaceKind::Area))
            .unwrap();
        f.grid
            .paint(
                &[CellKey::new(2, 1), CellKey::new(3, 1), CellKey::new(3, 4)],
                Some("z"),
            )
            .unwrap();
        f.grid.paint(&[CellKey::new(0, 0)], Some("a")).unwrap();
        let o = outline(&f, 32.0);
        let names: Vec<&str> = o.spaces.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            ["armoury", "Empty", "Zed Hall"],
            "case-insensitive by name"
        );
        assert_eq!(o.spaces[0].cells, 3);
        assert_eq!(o.spaces[0].bbox, Some([2, 1, 3, 4]));
        assert_eq!((o.spaces[1].cells, o.spaces[1].bbox), (0, None));
    }

    #[test]
    fn lights_and_labels_point_at_their_cells() {
        let mut f = FloorDoc::new("f".into(), "G".into());
        f.add_light(Light {
            id: "l1".into(),
            x: 100.0,
            y: 70.0,
            range: 4.0,
            color: "#ffffff".into(),
            intensity: 1.0,
            on: false,
            label: None,
        })
        .unwrap();
        f.upsert_label(Label {
            id: "t".into(),
            x: 1.5,
            y: 0.25,
            text: "Gate".into(),
            size: 12.0,
            color: "#ffffff".into(),
            bold: false,
            gm_only: true,
            path: vec![],
            halo: true,
            tracking: 0.0,
            rotation: 0.0,
        })
        .unwrap();
        let o = outline(&f, 32.0);
        assert_eq!(o.lights[0].cell, [3, 2]);
        assert_eq!(o.lights[0].label, "Light l1");
        assert!(!o.lights[0].on);
        assert_eq!((o.labels[0].cell, o.labels[0].gm_only), ([1, 0], true));
    }
}
