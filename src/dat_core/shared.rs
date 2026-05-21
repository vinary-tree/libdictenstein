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

// =============================================================================
// Algorithmic methods generic over U: CharUnit
// =============================================================================
//
// Methods that operate on the BASE/CHECK arrays in unit-agnostic ways. The
// byte and char DAT variants used to each carry their own copy of these
// (under different unit-type bounds); now both delegate to these.

impl<U: CharUnit, V: DictionaryValue> DATCoreShared<U, V> {
    /// Walk the trie starting at `root_state` and return whether `term`
    /// reaches a final state.
    ///
    /// The byte DAT uses `root_state = 1` (state 0 is a sentinel); the
    /// char DAT uses `root_state = 0`. Pass whichever convention your
    /// builder uses. Generic over the unit type via
    /// [`CharUnit::iter_str`] + [`CharUnit::to_dat_offset`].
    pub fn contains_term_from(&self, term: &str, root_state: usize) -> bool {
        let mut state: usize = root_state;
        for unit in U::iter_str(term) {
            if state >= self.base.len() {
                return false;
            }
            let base = self.base[state];
            if base < 0 {
                return false;
            }
            let next = (base as usize).wrapping_add(unit.to_dat_offset());
            if next >= self.check.len() || self.check[next] != state as i32 {
                return false;
            }
            state = next;
        }
        state < self.is_final.len() && self.is_final[state]
    }

    /// Walk the trie from `root_state` and return the value at the final
    /// state if `term` is present; `None` otherwise.
    pub fn term_value_from(&self, term: &str, root_state: usize) -> Option<V>
    where
        V: Clone,
    {
        let mut state: usize = root_state;
        for unit in U::iter_str(term) {
            if state >= self.base.len() {
                return None;
            }
            let base = self.base[state];
            if base < 0 {
                return None;
            }
            let next = (base as usize).wrapping_add(unit.to_dat_offset());
            if next >= self.check.len() || self.check[next] != state as i32 {
                return None;
            }
            state = next;
        }
        if state < self.is_final.len() && self.is_final[state] {
            self.values.get(state).and_then(|v| v.clone())
        } else {
            None
        }
    }

    /// `contains_term_from` with byte-DAT's `root_state = 1` convention.
    #[inline]
    pub fn contains_term(&self, term: &str) -> bool {
        self.contains_term_from(term, 1)
    }

    /// `term_value_from` with byte-DAT's `root_state = 1` convention.
    #[inline]
    pub fn term_value(&self, term: &str) -> Option<V>
    where
        V: Clone,
    {
        self.term_value_from(term, 1)
    }
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
