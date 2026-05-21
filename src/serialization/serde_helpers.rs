//! serde helper functions shared across dictionary backends.
//!
//! Both `DoubleArrayTrie` and `DoubleArrayTrieChar` need to (de)serialize
//! `Arc<Vec<T>>` and `Arc<Vec<Vec<T>>>` fields. The serde derive macros
//! reference these helpers by name via `#[serde(serialize_with = "…",
//! deserialize_with = "…")]`. Centralising them here removes the
//! byte-for-byte duplication that lived in each DAT file (C2 dedup).
//!
//! Callers `use` the helper they need and reference it unqualified in the
//! serde attribute string, e.g.
//!
//! ```ignore
//! use crate::serialization::serde_helpers::{serialize_arc_vec, deserialize_arc_vec};
//!
//! #[derive(Serialize, Deserialize)]
//! struct Foo {
//!     #[serde(serialize_with = "serialize_arc_vec",
//!             deserialize_with = "deserialize_arc_vec")]
//!     buf: Arc<Vec<u32>>,
//! }
//! ```
//!
//! Both compile-time-only — the entire module is gated on
//! `#[cfg(feature = "serialization")]`.

#![cfg(feature = "serialization")]

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::sync::Arc;

/// Custom serialization for `Arc<Vec<T>>` — serializes the inner `Vec`
/// directly without re-wrapping in serde's standard `Arc` handling.
pub fn serialize_arc_vec<S, T>(arc: &Arc<Vec<T>>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
    T: Serialize,
{
    arc.as_ref().serialize(serializer)
}

/// Custom deserialization for `Arc<Vec<T>>` — wraps the deserialized `Vec`
/// back into an `Arc`.
pub fn deserialize_arc_vec<'de, D, T>(deserializer: D) -> Result<Arc<Vec<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Vec::<T>::deserialize(deserializer).map(Arc::new)
}

/// Custom serialization for `Arc<Vec<Vec<T>>>`.
pub fn serialize_arc_vec_vec<S, T>(
    arc: &Arc<Vec<Vec<T>>>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
    T: Serialize,
{
    arc.as_ref().serialize(serializer)
}

/// Custom deserialization for `Arc<Vec<Vec<T>>>`.
pub fn deserialize_arc_vec_vec<'de, D, T>(
    deserializer: D,
) -> Result<Arc<Vec<Vec<T>>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Vec::<Vec<T>>::deserialize(deserializer).map(Arc::new)
}
