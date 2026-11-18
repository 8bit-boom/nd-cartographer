//! HTTP handlers, grouped by what they serve. `build_router` (in main.rs) is the
//! one place that lists every route.

pub(crate) mod ai;
pub(crate) mod assets;
pub(crate) mod docs;
pub(crate) mod edit;
pub(crate) mod export;
pub(crate) mod import;
pub(crate) mod proposals;
pub(crate) mod system;
pub(crate) mod table;
pub(crate) mod tiles;
