//! Core error type. Everything the journal or derivations can refuse.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreError {
    /// A coordinate or size is outside the bounds the studio stores.
    OutOfBounds(String),
    /// A referenced object (space id, light id, floor id) does not exist.
    NotFound(String),
    /// A field value is not acceptable (non-finite number, bad color, ...).
    Invalid(String),
    /// A hard cap would be exceeded (nd-world publish caps: 60 lights).
    CapExceeded(String),
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CoreError::OutOfBounds(m) => write!(f, "out of bounds: {m}"),
            CoreError::NotFound(m) => write!(f, "not found: {m}"),
            CoreError::Invalid(m) => write!(f, "invalid: {m}"),
            CoreError::CapExceeded(m) => write!(f, "cap exceeded: {m}"),
        }
    }
}

impl std::error::Error for CoreError {}

pub type CoreResult<T> = Result<T, CoreError>;

/// Cells may be painted anywhere in ±`MAX_CELL` — documents live on a very
/// large plane, but a wild number (the D1 lesson: `NaN` once bricked a live
/// map) is refused at the boundary, never stored.
pub const MAX_CELL: i32 = 10_000;
/// nd-world accepts at most 60 lights per schematic; core enforces the same
/// cap so a document is always publishable (the linter warns earlier, this is
/// the hard stop).
pub const MAX_LIGHTS: usize = 60;
