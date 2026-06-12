//! TraversalContext — re-export from core.
//!
//! The original byte-local copy was key-agnostic and a near-duplicate of
//! char's local copy (similarity 0.98 per audit). Phase-3 Move-2:
//! relocated to `persistent_artrie::core::traversal_context` so both
//! byte and char variants share a single canonical implementation.
//! Re-exported here under the original path so existing call-sites
//! continue to resolve.

pub use crate::persistent_artrie::core::traversal_context::*;
