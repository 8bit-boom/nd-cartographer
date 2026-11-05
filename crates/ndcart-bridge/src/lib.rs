//! ndcart-bridge — the optional nd-world client (plan §9). Nothing else in
//! the studio depends on it; with no server configured it is never used.

pub mod client;
pub mod rooms;

pub use client::{Bridge, BridgeError, Probe, PublishMap, PublishReport};
