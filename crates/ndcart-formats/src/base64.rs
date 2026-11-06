//! Base64 now lives in `ndcart-core` (documents need it for chunk data);
//! re-exported here so existing paths keep working.

pub use ndcart_core::base64::{decode, encode};
