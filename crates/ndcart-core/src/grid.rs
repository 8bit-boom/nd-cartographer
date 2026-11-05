//! The sparse room grid: painted cells belong to *spaces*; edges between
//! different spaces (or between a space and nothing) are walls (see
//! [`crate::walls`]); doors/windows are marks on edges.
//!
//! Field names and the domain model deliberately match nd-world's room-tool
//! state (`rooms.json`: cells + spaces + marks), so publishing a map is a
//! pass-through, not a translation. Internally storage is sparse
//! (`BTreeMap` keyed by cell), not a dense `cols*rows` array, so a 512x512
//! city map is as cheap as a 20x20 room.

use crate::error::{CoreError, CoreResult, MAX_CELL};
use crate::ids::new_id;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A cell on the document plane. `y` grows downward (screen convention,
/// matching nd-world canvas coordinates).
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct CellKey {
    pub x: i32,
    pub y: i32,
}

impl CellKey {
    pub fn new(x: i32, y: i32) -> Self {
        CellKey { x, y }
    }
    pub fn west(&self) -> CellKey {
        CellKey::new(self.x - 1, self.y)
    }
    pub fn east(&self) -> CellKey {
        CellKey::new(self.x + 1, self.y)
    }
    pub fn north(&self) -> CellKey {
        CellKey::new(self.x, self.y - 1)
    }
    pub fn south(&self) -> CellKey {
        CellKey::new(self.x, self.y + 1)
    }
    pub fn in_bounds(&self) -> bool {
        self.x.abs() <= MAX_CELL && self.y.abs() <= MAX_CELL
    }
}

/// Which edge of a cell. Canonical storage: the mark lives on an edge *of a
/// specific cell*; when both cells are painted the editor must pick one
/// side (the derivation treats either side's mark as the same edge).
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Edge {
    West,
    North,
    East,
    South,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct EdgeKey {
    pub cell: CellKey,
    pub edge: Edge,
}

/// What sits on an edge. `Opening` is a deliberate hole (an archway): no
/// wall segment is derived for it. Doors/secrets default to closed.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[serde(rename_all = "lowercase")]
pub enum MarkKind {
    Opening,
    Door,
    Window,
    Secret,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[serde(rename_all = "lowercase")]
pub enum MarkState {
    Open,
    Closed,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub struct Mark {
    pub kind: MarkKind,
    pub state: MarkState,
}

impl Mark {
    pub fn new(kind: MarkKind) -> Self {
        let state = match kind {
            MarkKind::Opening => MarkState::Open,
            _ => MarkState::Closed,
        };
        Mark { kind, state }
    }
}

/// What a painted area *is* — furnishing recipes, AI vocabulary and label
/// icons key on this; a role is data, not code.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[serde(rename_all = "lowercase")]
pub enum SpaceKind {
    Room,
    Corridor,
    Area,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct Space {
    pub id: String,
    pub name: String,
    /// `#rrggbb`.
    pub color: String,
    pub kind: SpaceKind,
}

/// One painted cell. (Serde shape: a list of triples rather than a map keyed
/// by a struct — friendlier in JSON and diffable.)
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct CellPaint {
    pub x: i32,
    pub y: i32,
    pub space: String,
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct RoomGrid {
    pub spaces: BTreeMap<String, Space>,
    /// Mark on an edge; only edges that carry one are stored.
    pub marks: BTreeMap<EdgeKey, Mark>,
    cells: BTreeMap<CellKey, String>,
}

/// The serde carrier: painted cells as a list of triples (see [`CellPaint`]).
#[derive(Serialize, Deserialize, Default)]
struct RoomGridSerde {
    #[serde(default)]
    spaces: BTreeMap<String, Space>,
    #[serde(default)]
    marks: BTreeMap<EdgeKey, Mark>,
    #[serde(default)]
    cells: Vec<CellPaint>,
}

impl From<&RoomGrid> for RoomGridSerde {
    fn from(g: &RoomGrid) -> Self {
        RoomGridSerde {
            spaces: g.spaces.clone(),
            marks: g.marks.clone(),
            cells: g
                .cells
                .iter()
                .map(|(k, v)| CellPaint {
                    x: k.x,
                    y: k.y,
                    space: v.clone(),
                })
                .collect(),
        }
    }
}

impl From<RoomGridSerde> for RoomGrid {
    fn from(s: RoomGridSerde) -> Self {
        let mut g = RoomGrid {
            spaces: s.spaces,
            marks: s.marks,
            cells: BTreeMap::new(),
        };
        for p in s.cells {
            g.cells.insert(CellKey::new(p.x, p.y), p.space);
        }
        g
    }
}

impl Serialize for RoomGrid {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        RoomGridSerde::from(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RoomGrid {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        RoomGridSerde::deserialize(deserializer).map(RoomGrid::from)
    }
}

fn valid_color(color: &str) -> bool {
    let b = color.as_bytes();
    b.len() == 7
        && b[0] == b'#'
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c) || (b'A'..=b'F').contains(c))
}

impl RoomGrid {
    pub fn cell_space(&self, cell: CellKey) -> Option<&str> {
        self.cells.get(&cell).map(String::as_str)
    }

    pub fn cells(&self) -> impl Iterator<Item = (CellKey, &str)> + '_ {
        self.cells.iter().map(|(k, v)| (*k, v.as_str()))
    }

    pub fn cell_count(&self) -> usize {
        self.cells.len()
    }

    /// Rebuild invariants after direct map manipulation (M0: unused; kept
    /// for future bulk loaders).
    pub fn fixup(&mut self) {}

    pub fn upsert_space(&mut self, space: Space) -> CoreResult<()> {
        if space.id.is_empty() {
            return Err(CoreError::Invalid("space id must not be empty".into()));
        }
        if !valid_color(&space.color) {
            return Err(CoreError::Invalid(format!(
                "color must be #rrggbb, got {}",
                space.color
            )));
        }
        self.spaces.insert(space.id.clone(), space);
        Ok(())
    }

    pub fn remove_space(&mut self, id: &str) -> CoreResult<()> {
        if !self.spaces.remove(id).is_some() {
            return Err(CoreError::NotFound(format!("space {id}")));
        }
        self.cells.retain(|_, s| s != id);
        Ok(())
    }

    /// Paint cells into a space (or clear them with `space: None`).
    pub fn paint(&mut self, cells: &[CellKey], space: Option<&str>) -> CoreResult<()> {
        if let Some(sid) = space {
            if !self.spaces.contains_key(sid) {
                return Err(CoreError::NotFound(format!("space {sid}")));
            }
            let to_paint: Vec<CellKey> = cells.to_vec();
            for c in &to_paint {
                if !c.in_bounds() {
                    return Err(CoreError::OutOfBounds(format!("cell {c:?}")));
                }
            }
            for c in to_paint {
                self.cells.insert(c, sid.to_string());
            }
        } else {
            for c in cells {
                if !c.in_bounds() {
                    return Err(CoreError::OutOfBounds(format!("cell {c:?}")));
                }
                self.cells.remove(c);
            }
        }
        Ok(())
    }

    /// Set or clear the mark on one edge. When both adjacent cells are
    /// painted, the mirror edge of the neighbor is kept in sync so the two
    /// sides can never disagree.
    pub fn set_mark(&mut self, key: EdgeKey, mark: Option<Mark>) -> CoreResult<()> {
        if !key.cell.in_bounds() {
            return Err(CoreError::OutOfBounds(format!("edge {key:?}")));
        }
        let mark = mark.map(|m| {
            // Openings carry no state; normalise (no recursion).
            if m.kind == MarkKind::Opening {
                Mark {
                    kind: MarkKind::Opening,
                    state: MarkState::Open,
                }
            } else {
                m
            }
        });
        let mirror = mirror_edge(key);
        match mark {
            Some(m) => {
                self.marks.insert(key, m);
                if self.cells.contains_key(&key.cell) && self.cells.contains_key(&mirror.cell) {
                    self.marks.insert(mirror, m);
                }
            }
            None => {
                self.marks.remove(&key);
                self.marks.remove(&mirror);
            }
        }
        Ok(())
    }

    /// The mark a derivation must honour for the edge between `cell` and its
    /// neighbor in direction `edge`: either side's stored mark counts.
    pub fn mark_between(&self, cell: CellKey, edge: Edge) -> Option<Mark> {
        let key = EdgeKey { cell, edge };
        if let Some(m) = self.marks.get(&key) {
            return Some(*m);
        }
        let (neighbor, reverse) = match edge {
            Edge::West => (cell.west(), Edge::East),
            Edge::East => (cell.east(), Edge::West),
            Edge::North => (cell.north(), Edge::South),
            Edge::South => (cell.south(), Edge::North),
        };
        self.marks
            .get(&EdgeKey {
                cell: neighbor,
                edge: reverse,
            })
            .copied()
    }
}

fn mirror_edge(key: EdgeKey) -> EdgeKey {
    let cell = match key.edge {
        Edge::West => key.cell.west(),
        Edge::East => key.cell.east(),
        Edge::North => key.cell.north(),
        Edge::South => key.cell.south(),
    };
    let edge = match key.edge {
        Edge::West => Edge::East,
        Edge::East => Edge::West,
        Edge::North => Edge::South,
        Edge::South => Edge::North,
    };
    EdgeKey { cell, edge }
}

/// A demo space palette the UI starts with (the studio's first-party colors).
pub fn default_space(id: &str, name: &str, kind: SpaceKind) -> Space {
    Space {
        id: id.to_string(),
        name: name.to_string(),
        color: "#334455".to_string(),
        kind,
    }
}

/// Generate a fresh space id (stable across save/load).
pub fn new_space_id() -> String {
    new_id("sp")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paint_requires_existing_space() {
        let mut g = RoomGrid::default();
        g.upsert_space(Space {
            id: "a".into(),
            name: "A".into(),
            color: "#112233".into(),
            kind: SpaceKind::Room,
        })
        .unwrap();
        assert!(g.paint(&[CellKey::new(0, 0)], Some("nope")).is_err());
        g.paint(&[CellKey::new(0, 0)], Some("a")).unwrap();
        assert_eq!(g.cell_space(CellKey::new(0, 0)), Some("a"));
        g.paint(&[CellKey::new(0, 0)], None).unwrap();
        assert_eq!(g.cell_space(CellKey::new(0, 0)), None);
    }

    #[test]
    fn paint_refuses_wild_cells() {
        let mut g = RoomGrid::default();
        g.upsert_space(default_space("a", "A", SpaceKind::Room))
            .unwrap();
        assert!(g
            .paint(&[CellKey::new(MAX_CELL + 1, 0)], Some("a"))
            .is_err());
        assert!(g.paint(&[CellKey::new(-99_999, 0)], Some("a")).is_err());
    }

    #[test]
    fn marks_sync_between_two_painted_cells() {
        let mut g = RoomGrid::default();
        g.upsert_space(default_space("a", "A", SpaceKind::Room))
            .unwrap();
        g.paint(&[CellKey::new(0, 0), CellKey::new(1, 0)], Some("a"))
            .unwrap();
        let west = EdgeKey {
            cell: CellKey::new(0, 0),
            edge: Edge::East,
        };
        g.set_mark(west, Some(Mark::new(MarkKind::Door))).unwrap();
        // The east cell's west edge mirrors it.
        let mirrored = EdgeKey {
            cell: CellKey::new(1, 0),
            edge: Edge::West,
        };
        assert_eq!(g.marks.get(&mirrored), g.marks.get(&west));
        assert_eq!(
            g.mark_between(CellKey::new(1, 0), Edge::West),
            Some(Mark::new(MarkKind::Door))
        );
        g.set_mark(west, None).unwrap();
        assert_eq!(g.mark_between(CellKey::new(1, 0), Edge::West), None);
    }

    #[test]
    fn serde_roundtrip_keeps_cells() {
        let mut g = RoomGrid::default();
        g.upsert_space(default_space("a", "A", SpaceKind::Corridor))
            .unwrap();
        g.paint(&[CellKey::new(3, 4), CellKey::new(-2, 7)], Some("a"))
            .unwrap();
        let s = serde_json::to_string(&g).unwrap();
        let back: RoomGrid = serde_json::from_str(&s).unwrap();
        assert_eq!(back, g);
        assert_eq!(back.cell_count(), 2);
    }
}
