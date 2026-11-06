//! ndcart-terrain — shading and generation for region maps (plan §7.2).
//! Pure and deterministic: the same heights, style and seed always give the
//! same colours, and the same seed always gives the same world.

mod generate;
mod shade;

pub use generate::{generate, GenParams, Generated};
pub use shade::Shader;
