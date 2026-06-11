//! Lattice adapters for union merging.
//!
//! The [`Lattice`] trait itself lives in the [`llattice`] crate and is
//! re-exported here so that `libdictenstein::union_zipper::Lattice` keeps
//! resolving. [`LatticeJoin`] and [`LatticeMeet`] adapt any `Lattice` impl into
//! a [`ValueMergeStrategy`](super::merge_strategies::ValueMergeStrategy).
//!
//! Extracted from `union_zipper.rs` (C6 dedup); the trait itself moved to the
//! `llattice` leaf crate to break a dependency cycle. Re-exported from
//! [`crate::union_zipper`] for back-compat.

use super::merge_strategies::ValueMergeStrategy;

/// The lattice trait (join / meet), re-exported from the [`llattice`] crate.
///
/// A lattice provides join (least upper bound) and meet (greatest lower bound)
/// operations. Built-in impls cover integers, floats, `bool`, `Option`,
/// `HashSet`, and `Vec`; see the [`llattice`] crate for the trait definition,
/// laws, and per-type semantics.
pub use llattice::Lattice;

/// Adapter that uses [`Lattice::join`] as the merge strategy.
///
/// Use this when you want duplicate values to be combined via their
/// lattice join operation (union/max/OR).
///
/// # Examples
///
/// ```rust
/// use libdictenstein::prelude::*;
/// use libdictenstein::union_zipper::{UnionZipper, LatticeJoin};
/// use libdictenstein::double_array_trie_zipper::DoubleArrayTrieZipper;
/// use std::collections::HashSet;
///
/// // Create dictionaries with HashSet values
/// let dict1 = DoubleArrayTrie::from_terms_with_values(
///     vec![("key", HashSet::from([1, 2]))].into_iter()
/// );
/// let dict2 = DoubleArrayTrie::from_terms_with_values(
///     vec![("key", HashSet::from([2, 3]))].into_iter()
/// );
///
/// let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
/// let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);
///
/// let union = UnionZipper::with_strategy(vec![z1, z2], LatticeJoin);
///
/// // Navigate to "key" and get the joined value
/// let key = union.descend(b'k')
///     .and_then(|z| z.descend(b'e'))
///     .and_then(|z| z.descend(b'y'))
///     .unwrap();
///
/// // LatticeJoin: {1, 2} ∪ {2, 3} = {1, 2, 3}
/// assert_eq!(key.value(), Some(HashSet::from([1, 2, 3])));
/// ```
#[derive(Clone, Copy, Debug, Default)]
pub struct LatticeJoin;

impl<V: Lattice> ValueMergeStrategy<V> for LatticeJoin {
    #[inline]
    fn merge(&self, existing: V, new: V) -> V {
        existing.join(&new)
    }
}

/// Adapter that uses [`Lattice::meet`] as the merge strategy.
///
/// Use this when you want duplicate values to be combined via their
/// lattice meet operation (intersection/min/AND).
///
/// # Examples
///
/// ```rust
/// use libdictenstein::prelude::*;
/// use libdictenstein::union_zipper::{UnionZipper, LatticeMeet};
/// use libdictenstein::double_array_trie_zipper::DoubleArrayTrieZipper;
/// use std::collections::HashSet;
///
/// // Create dictionaries with HashSet values
/// let dict1 = DoubleArrayTrie::from_terms_with_values(
///     vec![("key", HashSet::from([1, 2, 3]))].into_iter()
/// );
/// let dict2 = DoubleArrayTrie::from_terms_with_values(
///     vec![("key", HashSet::from([2, 3, 4]))].into_iter()
/// );
///
/// let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
/// let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);
///
/// let union = UnionZipper::with_strategy(vec![z1, z2], LatticeMeet);
///
/// // Navigate to "key" and get the met value
/// let key = union.descend(b'k')
///     .and_then(|z| z.descend(b'e'))
///     .and_then(|z| z.descend(b'y'))
///     .unwrap();
///
/// // LatticeMeet: {1, 2, 3} ∩ {2, 3, 4} = {2, 3}
/// assert_eq!(key.value(), Some(HashSet::from([2, 3])));
/// ```
#[derive(Clone, Copy, Debug, Default)]
pub struct LatticeMeet;

impl<V: Lattice> ValueMergeStrategy<V> for LatticeMeet {
    #[inline]
    fn merge(&self, existing: V, new: V) -> V {
        existing.meet(&new)
    }
}
