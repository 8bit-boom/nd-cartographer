//! ndcart-bridge — the optional nd-world client (plan §9). Nothing else in
//! the studio depends on it; with no server configured it is never used.

pub mod client;
pub mod direct;
pub mod rooms;

pub use client::{
    Bridge, BridgeError, ImageJob, ImageRequest, Probe, PublishMap, PublishRegion, PublishReport,
    Published,
};
pub use direct::{plain_client, DirectChat};
