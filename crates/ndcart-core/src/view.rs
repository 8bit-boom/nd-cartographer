//! What the players may see — the ONE filter every player-facing output goes
//! through (plan §17.7: "one secrets filter"): player exports, player
//! publishes, and later the true-preview. A second implementation anywhere
//! would be the leak, so nothing else strips secrets.
//!
//! - a secret door is a plain wall to the players (its mark is dropped, so the
//!   edge between two spaces derives as an ordinary wall);
//! - GM-only labels are removed.

use crate::doc::FloorDoc;
use crate::grid::MarkKind;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Audience {
    Gm,
    Player,
}

impl Audience {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "gm" => Some(Self::Gm),
            "player" | "players" => Some(Self::Player),
            _ => None,
        }
    }
}

/// The floor as `audience` may see it (a clone; the document is untouched).
pub fn for_audience(floor: &FloorDoc, audience: Audience) -> FloorDoc {
    let mut f = floor.clone();
    if audience == Audience::Gm {
        return f;
    }
    f.grid.marks.retain(|_, m| m.kind != MarkKind::Secret);
    // An imported secret door is a plain wall to the players, same as a derived one.
    for w in &mut f.imported_walls {
        if w.kind == crate::walls::WallKind::Secret {
            w.kind = crate::walls::WallKind::Wall;
            w.state = None;
        }
    }
    f.labels.retain(|l| !l.gm_only);
    f
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::Label;
    use crate::grid::{default_space, CellKey, Edge, EdgeKey, Mark, SpaceKind};
    use crate::walls::{walls_from_grid, WallKind};

    fn floor() -> FloorDoc {
        let mut f = FloorDoc::new("f".into(), "G".into());
        f.grid
            .upsert_space(default_space("a", "Hall", SpaceKind::Room))
            .unwrap();
        f.grid
            .upsert_space(default_space("b", "Vault", SpaceKind::Room))
            .unwrap();
        f.grid.paint(&[CellKey::new(0, 0)], Some("a")).unwrap();
        f.grid.paint(&[CellKey::new(1, 0)], Some("b")).unwrap();
        f.grid
            .set_mark(
                EdgeKey {
                    cell: CellKey::new(0, 0),
                    edge: Edge::East,
                },
                Some(Mark::new(MarkKind::Secret)),
            )
            .unwrap();
        f.grid
            .set_mark(
                EdgeKey {
                    cell: CellKey::new(0, 0),
                    edge: Edge::North,
                },
                Some(Mark::new(MarkKind::Door)),
            )
            .unwrap();
        for (id, gm_only) in [("pub", false), ("secret", true)] {
            f.upsert_label(Label {
                id: id.into(),
                x: 1.0,
                y: 1.0,
                text: id.into(),
                size: 12.0,
                color: "#ffffff".into(),
                bold: false,
                gm_only,
                path: vec![],
                halo: true,
                tracking: 0.0,
                rotation: 0.0,
            })
            .unwrap();
        }
        f
    }

    #[test]
    fn players_see_a_secret_door_as_a_plain_wall_and_no_gm_notes() {
        let f = floor();
        let gm = walls_from_grid(&f.grid, 32.0).unwrap();
        assert!(gm.iter().any(|w| w.kind == WallKind::Secret));

        let p = for_audience(&f, Audience::Player);
        let walls = walls_from_grid(&p.grid, 32.0).unwrap();
        assert!(
            !walls.iter().any(|w| w.kind == WallKind::Secret),
            "{walls:?}"
        );
        // The wall is still there, just unremarkable; ordinary doors survive.
        assert!(walls.iter().any(|w| w.kind == WallKind::Door));
        let between = walls.iter().any(|w| {
            w.kind == WallKind::Wall && w.pts.iter().all(|pt| (pt[0] - 32.0).abs() < 1e-6)
        });
        assert!(
            between,
            "the secret door's edge must still be a wall: {walls:?}"
        );
        assert_eq!(p.labels.len(), 1);
        assert_eq!(p.labels[0].id, "pub");
        // The GM view is untouched, and so is the source document.
        assert_eq!(for_audience(&f, Audience::Gm), f);
        assert_eq!(f.labels.len(), 2);
    }

    #[test]
    fn an_imported_secret_door_is_a_plain_wall_to_players() {
        use crate::doc::ImportedWall;
        use crate::walls::{floor_walls, WallState};
        let mut f = FloorDoc::new("f".into(), "G".into());
        f.set_imported_walls(vec![ImportedWall {
            pts: vec![[1.0, 1.0], [2.0, 1.0]],
            kind: WallKind::Secret,
            state: Some(WallState::Closed),
        }])
        .unwrap();
        assert!(floor_walls(&f, 32.0)
            .unwrap()
            .iter()
            .any(|w| w.kind == WallKind::Secret));
        let p = for_audience(&f, Audience::Player);
        let walls = floor_walls(&p, 32.0).unwrap();
        assert_eq!(walls.len(), 1);
        assert_eq!((walls[0].kind, walls[0].state), (WallKind::Wall, None));
        assert!(!serde_json::to_string(&p).unwrap().contains("secret"));
    }

    #[test]
    fn no_secret_survives_anywhere_in_the_player_state() {
        let p = for_audience(&floor(), Audience::Player);
        let json = serde_json::to_string(&p).unwrap();
        assert!(!json.contains("secret"), "{json}");
    }
}
