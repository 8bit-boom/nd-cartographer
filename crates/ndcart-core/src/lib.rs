//! ndcart-core — the pure document model for nd-cartographer: the sparse
//! room grid, walls-from-rooms derivation, lights/labels, and the command
//! journal. No IO, no clock reads at call time beyond id generation.
//!
//! Domain vocabulary deliberately matches nd-world's room-tool state
//! (`cells` + `spaces` + `marks`; walls `{id, pts, kind, state}`; lights in
//! squares with `range`/`intensity`), so publishing is a pass-through.

pub mod doc;
pub mod error;
pub mod grid;
pub mod hash;
pub mod ids;
pub mod journal;
pub mod lint;
pub mod outline;
pub mod stamp;
pub mod stroke;
pub mod view;
pub mod walls;

pub use doc::{DocKind, FloorDoc, ImportedWall, Label, Light, Manifest, NdWorldLink, Underlay};
pub use error::{CoreError, CoreResult, MAX_CELL, MAX_LIGHTS};
pub use grid::{CellKey, Edge, EdgeKey, Mark, MarkKind, MarkState, RoomGrid, Space, SpaceKind};
pub use journal::{apply_op, Journal, Op};
pub use lint::{blocks_publish, lint, Problem, Severity};
pub use outline::{outline, Outline};
pub use stamp::Stamp;
pub use stroke::{BrushKind, Stroke, StrokeKind, StrokePoint};
pub use view::{for_audience, Audience};
pub use walls::{floor_walls, walls_from_grid, Wall, WallKind, WallState, ND_WORLD_WALL_CAP};
