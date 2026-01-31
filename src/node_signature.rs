//! Hash-based node signatures for efficient DAWG minimization.
//!
//! This module provides [`NodeSignature`], a hash-based representation of a node's
//! "right language" - the set of strings that can be formed from this node to any
//! final state. Two nodes with identical signatures are equivalent and can be merged.
//!
//! # Design Rationale
//!
//! Instead of storing recursive `Box<NodeSignature>` structures (which would require
//! ~3000 heap allocations for a 1000-node DAWG), we use a single `u64` hash. This
//! provides O(1) signature comparisons and eliminates expensive recursive allocations.
//!
//! # Hash Collisions
//!
//! Since we use a 64-bit hash, collisions are possible (birthday paradox). The
//! minimization algorithm must verify structural equality when hash signatures match
//! to prevent false merges.

use crate::CharUnit;
use rustc_hash::FxHasher;
use smallvec::SmallVec;
use std::hash::{Hash, Hasher};

/// Hash-based node signature for efficient minimization.
///
/// A signature represents the "right language" of a node - the set of strings
/// that can be formed from this node to any final state. The signature is
/// computed as `FxHash(is_final, sorted[(label, child_signature_hash), ...])`.
///
/// Two nodes with identical signatures are candidates for merging. Due to
/// possible hash collisions, structural equality should be verified before
/// actual merging.
#[derive(Clone, Debug, Copy, PartialEq, Eq, Hash)]
pub struct NodeSignature {
    /// Hash representing (is_final, sorted edges with child hashes)
    pub hash: u64,
}

impl NodeSignature {
    /// Create a new node signature with the given hash value.
    pub fn new(hash: u64) -> Self {
        NodeSignature { hash }
    }

    /// Create a zero signature (used as placeholder before computation).
    pub fn zero() -> Self {
        NodeSignature { hash: 0 }
    }

    /// Compute a signature for a node given its properties and children's signatures.
    ///
    /// # Arguments
    ///
    /// * `is_final` - Whether this node marks the end of a valid term
    /// * `edges` - Iterator of (label, child_signature) pairs
    ///
    /// # Type Parameters
    ///
    /// * `U` - The character unit type (u8, char, or u64)
    pub fn compute<U, I>(is_final: bool, edges: I) -> Self
    where
        U: CharUnit,
        I: IntoIterator<Item = (U, NodeSignature)>,
    {
        let mut hasher = FxHasher::default();

        // Hash the is_final flag
        is_final.hash(&mut hasher);

        // Collect and sort edges for consistent hashing
        let mut edge_hashes: SmallVec<[(U, u64); 4]> = edges
            .into_iter()
            .map(|(label, child_sig)| (label, child_sig.hash))
            .collect();

        // Sort by label to ensure consistent hashing regardless of insertion order
        edge_hashes.sort_unstable_by_key(|(label, _)| *label);

        // Hash each (label, child_hash) pair
        for (label, child_hash) in &edge_hashes {
            label.hash(&mut hasher);
            child_hash.hash(&mut hasher);
        }

        NodeSignature {
            hash: hasher.finish(),
        }
    }

    /// Compute a signature for a node with optional value.
    ///
    /// When nodes can have associated values, the value affects the signature.
    /// Two nodes with the same structure but different values should have
    /// different signatures.
    ///
    /// # Arguments
    ///
    /// * `is_final` - Whether this node marks the end of a valid term
    /// * `value_hash` - Optional hash of the node's associated value
    /// * `edges` - Iterator of (label, child_signature) pairs
    pub fn compute_with_value<U, I>(is_final: bool, value_hash: Option<u64>, edges: I) -> Self
    where
        U: CharUnit,
        I: IntoIterator<Item = (U, NodeSignature)>,
    {
        let mut hasher = FxHasher::default();

        // Hash the is_final flag
        is_final.hash(&mut hasher);

        // Hash the value if present
        if let Some(vh) = value_hash {
            true.hash(&mut hasher); // has_value marker
            vh.hash(&mut hasher);
        } else {
            false.hash(&mut hasher); // no value marker
        }

        // Collect and sort edges for consistent hashing
        let mut edge_hashes: SmallVec<[(U, u64); 4]> = edges
            .into_iter()
            .map(|(label, child_sig)| (label, child_sig.hash))
            .collect();

        edge_hashes.sort_unstable_by_key(|(label, _)| *label);

        // Hash each (label, child_hash) pair
        for (label, child_hash) in &edge_hashes {
            label.hash(&mut hasher);
            child_hash.hash(&mut hasher);
        }

        NodeSignature {
            hash: hasher.finish(),
        }
    }
}

impl Default for NodeSignature {
    fn default() -> Self {
        Self::zero()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_signature_basic() {
        // Two leaf nodes with same is_final should have same signature
        let sig1 = NodeSignature::compute::<u8, _>(true, std::iter::empty());
        let sig2 = NodeSignature::compute::<u8, _>(true, std::iter::empty());
        assert_eq!(sig1, sig2);

        // Different is_final should produce different signature
        let sig3 = NodeSignature::compute::<u8, _>(false, std::iter::empty());
        assert_ne!(sig1, sig3);
    }

    #[test]
    fn test_signature_with_edges() {
        let child1 = NodeSignature::compute::<u8, _>(true, std::iter::empty());
        let child2 = NodeSignature::compute::<u8, _>(false, std::iter::empty());

        // Same edges should produce same signature
        let sig1 = NodeSignature::compute::<u8, _>(false, vec![(b'a', child1), (b'b', child2)]);
        let sig2 = NodeSignature::compute::<u8, _>(false, vec![(b'a', child1), (b'b', child2)]);
        assert_eq!(sig1, sig2);

        // Edge order shouldn't matter (they get sorted)
        let sig3 = NodeSignature::compute::<u8, _>(false, vec![(b'b', child2), (b'a', child1)]);
        assert_eq!(sig1, sig3);

        // Different edges should produce different signature
        let sig4 = NodeSignature::compute::<u8, _>(false, vec![(b'a', child1)]);
        assert_ne!(sig1, sig4);
    }

    #[test]
    fn test_signature_with_value() {
        // Nodes with same structure but different values should differ
        let sig1 = NodeSignature::compute_with_value::<u8, _>(true, Some(42), std::iter::empty());
        let sig2 = NodeSignature::compute_with_value::<u8, _>(true, Some(99), std::iter::empty());
        assert_ne!(sig1, sig2);

        // Same value should produce same signature
        let sig3 = NodeSignature::compute_with_value::<u8, _>(true, Some(42), std::iter::empty());
        assert_eq!(sig1, sig3);

        // No value vs some value should differ
        let sig4 = NodeSignature::compute_with_value::<u8, _>(true, None, std::iter::empty());
        assert_ne!(sig1, sig4);
    }

    #[test]
    fn test_signature_char_unit() {
        // Test with char unit type
        let child = NodeSignature::compute::<char, _>(true, std::iter::empty());
        let sig = NodeSignature::compute::<char, _>(false, vec![('a', child), ('é', child)]);
        assert_ne!(sig.hash, 0);
    }

    #[test]
    fn test_signature_u64_unit() {
        // Test with u64 unit type
        let child = NodeSignature::compute::<u64, _>(true, std::iter::empty());
        let sig = NodeSignature::compute::<u64, _>(false, vec![(1u64, child), (1000u64, child)]);
        assert_ne!(sig.hash, 0);
    }
}
