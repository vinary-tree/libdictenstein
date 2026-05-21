//! Generic `DATCoreShared<U, V>` storage struct shared by the byte and
//! char DAT variants.
//!
//! Mirrors the conditional serde bound pattern used by
//! [`crate::dawg_core::DawgNode`] so the same struct works under both
//! the `serialization`-only and `persistent-artrie` configurations.

use std::sync::Arc;

use crate::value::DictionaryValue;
use crate::CharUnit;

/// Shared storage for Double-Array Trie states.
///
/// Holds the four parallel arrays (BASE, CHECK, IS_FINAL, edges) plus
/// optional terminal values. All fields are `Arc<Vec<…>>` so clone is
/// cheap (no deep copy) and multiple readers can navigate the trie
/// concurrently.
///
/// # Type parameters
///
/// - `U`: edge label type (`u8` for byte-keyed DAT, `char` for
///   Unicode-keyed DAT). Must implement [`CharUnit`].
/// - `V`: value type associated with terminal states. Must implement
///   [`DictionaryValue`].
///
/// # Serialization
///
/// Custom serde plumbing routes through
/// [`crate::serialization::serde_helpers`] so the on-disk format
/// matches the previous byte-for-byte layout used by both
/// `DoubleArrayTrie<V>` and `DoubleArrayTrieChar<V>`.
#[cfg_attr(feature = "serialization", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    all(feature = "serialization", not(feature = "persistent-artrie")),
    serde(bound(
        serialize = "U: serde::Serialize, V: serde::Serialize",
        deserialize = "U: serde::Deserialize<'de>, V: serde::Deserialize<'de>",
    ))
)]
#[cfg_attr(
    all(feature = "serialization", feature = "persistent-artrie"),
    serde(bound(
        serialize = "U: serde::Serialize, V: serde::Serialize",
        deserialize = "U: serde::de::DeserializeOwned, V: serde::de::DeserializeOwned",
    ))
)]
#[derive(Clone, Debug)]
pub struct DATCoreShared<U: CharUnit, V: DictionaryValue = ()> {
    /// BASE array: offset for computing next state.
    ///
    /// Transition from state `s` on label `u` lands at
    /// `base[s] + (u as i32)`. The CHECK array verifies the parent.
    #[cfg_attr(
        feature = "serialization",
        serde(
            serialize_with = "crate::serialization::serde_helpers::serialize_arc_vec",
            deserialize_with = "crate::serialization::serde_helpers::deserialize_arc_vec"
        )
    )]
    pub base: Arc<Vec<i32>>,

    /// CHECK array: parent state verification.
    ///
    /// A computed child state `c = base[parent] + u` is only valid
    /// when `check[c] == parent`.
    #[cfg_attr(
        feature = "serialization",
        serde(
            serialize_with = "crate::serialization::serde_helpers::serialize_arc_vec",
            deserialize_with = "crate::serialization::serde_helpers::deserialize_arc_vec"
        )
    )]
    pub check: Arc<Vec<i32>>,

    /// Final-state markers (terminal flag per state).
    #[cfg_attr(
        feature = "serialization",
        serde(
            serialize_with = "crate::serialization::serde_helpers::serialize_arc_vec",
            deserialize_with = "crate::serialization::serde_helpers::deserialize_arc_vec"
        )
    )]
    pub is_final: Arc<Vec<bool>>,

    /// Edge lists per state: the actual outgoing edge labels at each
    /// state. Avoids scanning all 256 (byte) or 1.1M (char) possible
    /// labels during iteration.
    #[cfg_attr(
        feature = "serialization",
        serde(
            serialize_with = "crate::serialization::serde_helpers::serialize_arc_vec_vec",
            deserialize_with = "crate::serialization::serde_helpers::deserialize_arc_vec_vec"
        )
    )]
    pub edges: Arc<Vec<Vec<U>>>,

    /// Values associated with final states.
    ///
    /// Indexed by state number; only final states may hold `Some(v)`.
    #[cfg_attr(
        feature = "serialization",
        serde(
            serialize_with = "crate::serialization::serde_helpers::serialize_arc_vec",
            deserialize_with = "crate::serialization::serde_helpers::deserialize_arc_vec"
        )
    )]
    pub values: Arc<Vec<Option<V>>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dat_core_shared_default_byte() {
        // Smoke test: constructs the generic shared struct over u8.
        let shared: DATCoreShared<u8, ()> = DATCoreShared {
            base: Arc::new(vec![0]),
            check: Arc::new(vec![0]),
            is_final: Arc::new(vec![false]),
            edges: Arc::new(vec![vec![]]),
            values: Arc::new(vec![None]),
        };
        assert_eq!(shared.base.len(), 1);
        assert_eq!(shared.edges.len(), 1);
    }

    #[test]
    fn dat_core_shared_default_char() {
        // Smoke test over char (Unicode-keyed).
        let shared: DATCoreShared<char, u32> = DATCoreShared {
            base: Arc::new(vec![0]),
            check: Arc::new(vec![0]),
            is_final: Arc::new(vec![false]),
            edges: Arc::new(vec![vec![]]),
            values: Arc::new(vec![None]),
        };
        assert_eq!(shared.edges[0].len(), 0);
        assert!(shared.values[0].is_none());
    }
}
