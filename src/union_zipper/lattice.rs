//! Lattice abstraction and adapters for union merging.
//!
//! A lattice is a partially-ordered set with two operations:
//! - **join** (least upper bound / supremum)
//! - **meet** (greatest lower bound / infimum)
//!
//! For union zipper purposes, [`LatticeJoin`] and [`LatticeMeet`] adapt any
//! `Lattice` impl into a [`ValueMergeStrategy`](super::merge_strategies::ValueMergeStrategy).
//!
//! Extracted from `union_zipper.rs` (C6 dedup); re-exported from
//! [`crate::union_zipper`] for back-compat.

use std::collections::HashSet;
use std::hash::Hash;

use super::merge_strategies::ValueMergeStrategy;

/// A lattice provides join (least upper bound) and meet (greatest lower bound) operations.
///
/// Lattices satisfy the following properties:
/// - **Idempotency**: `a.join(a) = a` and `a.meet(a) = a`
/// - **Commutativity**: `a.join(b) = b.join(a)` and `a.meet(b) = b.meet(a)`
/// - **Associativity**: `(a.join(b)).join(c) = a.join(b.join(c))` (same for meet)
/// - **Absorption**: `a.join(a.meet(b)) = a` and `a.meet(a.join(b)) = a`
///
/// # Use Cases
///
/// - **CRDT-style merges**: When values from multiple dictionaries should be combined
///   using lattice semantics (e.g., merging sets, taking max/min)
/// - **Conflict-free replication**: Lattice operations are commutative and associative,
///   making them suitable for distributed systems
///
/// # Relationship to Semirings
///
/// For idempotent semirings (where `a ⊕ a = a`), the `plus` operation forms a join
/// semilattice. However, `times` is generally path composition, not lattice meet.
/// See the `lling-llang` feature for automatic `Lattice` impl from `IdempotentSemiring`.
///
/// # Examples
///
/// ```rust
/// use libdictenstein::union_zipper::Lattice;
/// use std::collections::HashSet;
///
/// // HashSet: join = union, meet = intersection
/// let a: HashSet<i32> = [1, 2].into_iter().collect();
/// let b: HashSet<i32> = [2, 3].into_iter().collect();
///
/// let joined = a.join(&b);  // {1, 2, 3}
/// let met = a.meet(&b);     // {2}
///
/// assert_eq!(joined, [1, 2, 3].into_iter().collect());
/// assert_eq!(met, [2].into_iter().collect());
///
/// // Numeric: join = max, meet = min
/// assert_eq!(5u32.join(&3), 5);
/// assert_eq!(5u32.meet(&3), 3);
/// ```
pub trait Lattice: Clone + Send + Sync {
    /// Join operation (least upper bound / union / supremum).
    ///
    /// For sets, this is union. For numbers, this is max.
    fn join(&self, other: &Self) -> Self;

    /// Meet operation (greatest lower bound / intersection / infimum).
    ///
    /// For sets, this is intersection. For numbers, this is min.
    fn meet(&self, other: &Self) -> Self;
}

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

// =============================================================================
// Built-in Lattice Implementations
// =============================================================================

// Numeric types: join = max, meet = min
macro_rules! impl_lattice_for_numeric {
    ($($t:ty),+) => {
        $(
            impl Lattice for $t {
                #[inline]
                fn join(&self, other: &Self) -> Self {
                    (*self).max(*other)
                }

                #[inline]
                fn meet(&self, other: &Self) -> Self {
                    (*self).min(*other)
                }
            }
        )+
    };
}

impl_lattice_for_numeric!(u8, u16, u32, u64, u128, usize, i8, i16, i32, i64, i128, isize);

// f32: join = max, meet = min (using total ordering)
impl Lattice for f32 {
    #[inline]
    fn join(&self, other: &Self) -> Self {
        self.max(*other)
    }

    #[inline]
    fn meet(&self, other: &Self) -> Self {
        self.min(*other)
    }
}

// f64: join = max, meet = min (using total ordering)
impl Lattice for f64 {
    #[inline]
    fn join(&self, other: &Self) -> Self {
        self.max(*other)
    }

    #[inline]
    fn meet(&self, other: &Self) -> Self {
        self.min(*other)
    }
}

// bool: join = OR, meet = AND
impl Lattice for bool {
    #[inline]
    fn join(&self, other: &Self) -> Self {
        *self || *other
    }

    #[inline]
    fn meet(&self, other: &Self) -> Self {
        *self && *other
    }
}

// Option<T>: join = Some if either Some, meet = Some only if both Some
impl<T: Lattice> Lattice for Option<T> {
    #[inline]
    fn join(&self, other: &Self) -> Self {
        match (self, other) {
            (Some(a), Some(b)) => Some(a.join(b)),
            (Some(a), None) => Some(a.clone()),
            (None, Some(b)) => Some(b.clone()),
            (None, None) => None,
        }
    }

    #[inline]
    fn meet(&self, other: &Self) -> Self {
        match (self, other) {
            (Some(a), Some(b)) => Some(a.meet(b)),
            _ => None,
        }
    }
}

// HashSet<T>: join = union, meet = intersection
impl<T: Clone + Eq + Hash + Send + Sync> Lattice for HashSet<T> {
    fn join(&self, other: &Self) -> Self {
        self.union(other).cloned().collect()
    }

    fn meet(&self, other: &Self) -> Self {
        self.intersection(other).cloned().collect()
    }
}

// Vec<T>: join = concatenate + dedup (if T: Eq), meet = intersection (preserving order)
impl<T: Clone + Eq + Send + Sync> Lattice for Vec<T> {
    fn join(&self, other: &Self) -> Self {
        let mut result = self.clone();
        for item in other {
            if !result.contains(item) {
                result.push(item.clone());
            }
        }
        result
    }

    fn meet(&self, other: &Self) -> Self {
        self.iter()
            .filter(|item| other.contains(item))
            .cloned()
            .collect()
    }
}
