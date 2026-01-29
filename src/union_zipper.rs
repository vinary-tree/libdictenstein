//! Union zipper for multi-dictionary iteration.
//!
//! This module provides a `UnionZipper` that presents multiple dictionaries as a unified
//! view, allowing iteration over the union of terms as if they were merged. Includes
//! configurable value merge strategies for handling duplicate terms.
//!
//! # Use Cases
//!
//! - **Multiple scopes in code completion**: Local + global dictionaries
//! - **Layered dictionaries**: Base + overrides
//! - **Multiple data sources**: Unified view of separate indexes
//!
//! # Examples
//!
//! ## Basic Union of Two Dictionaries
//!
//! ```rust
//! use libdictenstein::prelude::*;
//! use libdictenstein::union_zipper::{UnionZipper, UnionZipperExt};
//! use libdictenstein::double_array_trie_zipper::DoubleArrayTrieZipper;
//!
//! let dict1 = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());
//! let dict2 = DoubleArrayTrie::from_terms(vec!["cat", "fish"].iter());
//!
//! let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
//! let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);
//!
//! // Create union using extension trait
//! let union = z1.union_with(z2);
//!
//! // Iterate all unique terms (cat appears only once)
//! let mut results: Vec<String> = union.iter()
//!     .map(|(path, _)| String::from_utf8(path).unwrap())
//!     .collect();
//! results.sort();
//! assert_eq!(results, vec!["cat", "dog", "fish"]);
//! ```
//!
//! ## Navigating the Union
//!
//! ```rust
//! use libdictenstein::prelude::*;
//! use libdictenstein::union_zipper::{UnionZipper, UnionZipperExt};
//! use libdictenstein::double_array_trie_zipper::DoubleArrayTrieZipper;
//! use libdictenstein::zipper::DictZipper;
//!
//! let dict1 = DoubleArrayTrie::from_terms(vec!["cat", "car"].iter());
//! let dict2 = DoubleArrayTrie::from_terms(vec!["cab", "can"].iter());
//!
//! let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
//! let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);
//!
//! let union = z1.union_with(z2);
//!
//! // Descend to 'c' -> 'a' and see children from both dictionaries
//! let ca = union.descend(b'c').and_then(|z| z.descend(b'a')).unwrap();
//! let children: Vec<u8> = ca.children().map(|(label, _)| label).collect();
//! assert!(children.contains(&b't')); // from dict1: "cat"
//! assert!(children.contains(&b'r')); // from dict1: "car"
//! assert!(children.contains(&b'b')); // from dict2: "cab"
//! assert!(children.contains(&b'n')); // from dict2: "can"
//! ```
//!
//! ## Value Merge Strategies
//!
//! ```rust
//! use libdictenstein::prelude::*;
//! use libdictenstein::union_zipper::{UnionZipper, FirstWins, LastWins};
//! use libdictenstein::double_array_trie_zipper::DoubleArrayTrieZipper;
//! use libdictenstein::zipper::{DictZipper, ValuedDictZipper};
//!
//! let dict1 = DoubleArrayTrie::from_terms_with_values(vec![("cat", 1), ("dog", 2)].into_iter());
//! let dict2 = DoubleArrayTrie::from_terms_with_values(vec![("cat", 10), ("fish", 3)].into_iter());
//!
//! let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
//! let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);
//!
//! // FirstWins (default): "cat" -> 1
//! let union = UnionZipper::new(vec![z1.clone(), z2.clone()]);
//! let cat = union.descend(b'c')
//!     .and_then(|z| z.descend(b'a'))
//!     .and_then(|z| z.descend(b't'))
//!     .unwrap();
//! assert_eq!(cat.value(), Some(1));
//!
//! // LastWins: "cat" -> 10
//! let union = UnionZipper::with_strategy(vec![z1, z2], LastWins);
//! let cat = union.descend(b'c')
//!     .and_then(|z| z.descend(b'a'))
//!     .and_then(|z| z.descend(b't'))
//!     .unwrap();
//! assert_eq!(cat.value(), Some(10));
//! ```
//!
//! ## Composable with PrefixZipper
//!
//! ```rust
//! use libdictenstein::prelude::*;
//! use libdictenstein::union_zipper::UnionZipperExt;
//! use libdictenstein::prefix_zipper::PrefixZipper;
//! use libdictenstein::double_array_trie_zipper::DoubleArrayTrieZipper;
//!
//! let dict1 = DoubleArrayTrie::from_terms(vec!["process", "produce"].iter());
//! let dict2 = DoubleArrayTrie::from_terms(vec!["product", "program"].iter());
//!
//! let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
//! let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);
//!
//! let union = z1.union_with(z2);
//!
//! // Use PrefixZipper on the union
//! let mut results: Vec<String> = union.with_prefix(b"pro")
//!     .unwrap()
//!     .map(|(path, _)| String::from_utf8(path).unwrap())
//!     .collect();
//! results.sort();
//! assert_eq!(results, vec!["process", "produce", "product", "program"]);
//! ```
//!
//! # Performance
//!
//! - **Navigation**: O(k × n) where k = prefix length, n = number of dictionaries
//! - **Children collection**: O(c × n) where c = max children per node, n = dictionaries
//! - **Iteration**: O(m) where m = total terms in union (with deduplication)
//! - **Memory**: O(n) for zipper storage + O(d) stack depth during iteration
//!
//! # Backend Compatibility
//!
//! Works uniformly across all dictionary backends via the `DictZipper` trait:
//! - `DoubleArrayTrie` (byte and char variants)
//! - `DynamicDawg` (byte and char variants)
//! - `PathMapDictionary` (byte variant)
//! - `SuffixAutomaton` (byte and char variants)

use std::collections::HashSet;
use std::hash::Hash;

use crate::zipper::{DictZipper, ValuedDictZipper};

// =============================================================================
// Value Merge Strategies
// =============================================================================

/// Strategy for merging values when the same term exists in multiple dictionaries.
///
/// This trait allows customization of how duplicate values are handled during
/// union iteration.
///
/// # Built-in Strategies
///
/// - [`FirstWins`]: Keep the value from the first dictionary (default)
/// - [`LastWins`]: Keep the value from the last dictionary
///
/// # Custom Strategies
///
/// Implement this trait for custom merge logic:
///
/// ```rust
/// use libdictenstein::union_zipper::ValueMergeStrategy;
///
/// #[derive(Clone)]
/// struct Sum;
///
/// impl ValueMergeStrategy<i32> for Sum {
///     fn merge(&self, existing: i32, new: i32) -> i32 {
///         existing + new
///     }
/// }
/// ```
pub trait ValueMergeStrategy<V>: Clone + Send + Sync {
    /// Merge two values, returning the result.
    ///
    /// # Arguments
    ///
    /// * `existing` - The value already accumulated (from earlier dictionaries)
    /// * `new` - The new value to merge (from the current dictionary)
    ///
    /// # Returns
    ///
    /// The merged value.
    fn merge(&self, existing: V, new: V) -> V;
}

/// Keep the first value seen (from earlier dictionaries in the union).
///
/// This is the default strategy. When a term exists in multiple dictionaries,
/// the value from the first dictionary (lowest index) wins.
///
/// # Example
///
/// ```rust
/// use libdictenstein::union_zipper::{UnionZipper, FirstWins};
/// # use libdictenstein::prelude::*;
/// # use libdictenstein::double_array_trie_zipper::DoubleArrayTrieZipper;
///
/// // dict1 has "cat" -> 1, dict2 has "cat" -> 10
/// // With FirstWins, "cat" -> 1
/// ```
#[derive(Clone, Copy, Debug, Default)]
pub struct FirstWins;

impl<V> ValueMergeStrategy<V> for FirstWins {
    #[inline]
    fn merge(&self, existing: V, _new: V) -> V {
        existing
    }
}

/// Keep the last value seen (from later dictionaries in the union).
///
/// When a term exists in multiple dictionaries, the value from the last
/// dictionary (highest index) wins.
///
/// # Example
///
/// ```rust
/// use libdictenstein::union_zipper::{UnionZipper, LastWins};
/// # use libdictenstein::prelude::*;
/// # use libdictenstein::double_array_trie_zipper::DoubleArrayTrieZipper;
///
/// // dict1 has "cat" -> 1, dict2 has "cat" -> 10
/// // With LastWins, "cat" -> 10
/// ```
#[derive(Clone, Copy, Debug, Default)]
pub struct LastWins;

impl<V> ValueMergeStrategy<V> for LastWins {
    #[inline]
    fn merge(&self, _existing: V, new: V) -> V {
        new
    }
}

// =============================================================================
// Lattice Trait and Adapters
// =============================================================================

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

// =============================================================================
// lling-llang Integration (Feature-gated)
// =============================================================================

/// Marker trait for types that implement Lattice via IdempotentSemiring.
///
/// When the `lling-llang` feature is enabled, any `IdempotentSemiring` automatically
/// implements `Lattice` where `join = plus` (⊕). This is because for idempotent
/// semirings, the `plus` operation satisfies the join-semilattice properties.
///
/// **Note**: The `meet` operation cannot be derived from semirings because the
/// semiring `times` (⊗) operation represents path composition, not lattice meet.
/// For semirings that need both join and meet, implement `Lattice` explicitly.
#[cfg(feature = "lling-llang")]
pub trait SemiringLattice: lling_llang::semiring::Semiring + lling_llang::semiring::IdempotentSemiring {}

#[cfg(feature = "lling-llang")]
impl<S> SemiringLattice for S where S: lling_llang::semiring::Semiring + lling_llang::semiring::IdempotentSemiring {}

/// Adapter for using IdempotentSemiring as a join-only Lattice.
///
/// This wraps a semiring value and provides `Lattice` implementation where:
/// - `join` = semiring `plus` (⊕)
/// - `meet` = semiring `times` (⊗) - **Note**: This may not be semantically correct
///   for all semirings since `times` is typically path composition, not lattice meet.
///
/// For proper lattice semantics, consider implementing `Lattice` directly on your type.
#[cfg(feature = "lling-llang")]
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(
    all(feature = "lling-llang", feature = "persistent-artrie"),
    derive(serde::Serialize, serde::Deserialize)
)]
#[cfg_attr(
    all(feature = "lling-llang", feature = "persistent-artrie"),
    serde(transparent)
)]
pub struct SemiringLatticeWrapper<S>(pub S);

#[cfg(feature = "lling-llang")]
impl<S: lling_llang::semiring::Semiring + lling_llang::semiring::IdempotentSemiring + Clone + Send + Sync> Lattice
    for SemiringLatticeWrapper<S>
{
    #[inline]
    fn join(&self, other: &Self) -> Self {
        SemiringLatticeWrapper(self.0.plus(&other.0))
    }

    #[inline]
    fn meet(&self, other: &Self) -> Self {
        // Note: times is path composition, not necessarily lattice meet.
        // This works for some semirings (e.g., Boolean where times = AND)
        // but may not have correct semantics for others (e.g., Tropical where times = +).
        SemiringLatticeWrapper(self.0.times(&other.0))
    }
}

// Implement DictionaryValue for SemiringLatticeWrapper so it can be used with dictionaries
// When persistent-artrie is NOT enabled: basic bounds only
#[cfg(all(feature = "lling-llang", not(feature = "persistent-artrie")))]
impl<S: Clone + Send + Sync + Unpin + 'static> crate::value::DictionaryValue
    for SemiringLatticeWrapper<S>
{
}

// When persistent-artrie IS enabled: require Serialize + DeserializeOwned
#[cfg(all(feature = "lling-llang", feature = "persistent-artrie"))]
impl<S: Clone + Send + Sync + Unpin + 'static + serde::Serialize + serde::de::DeserializeOwned>
    crate::value::DictionaryValue for SemiringLatticeWrapper<S>
{
}

// =============================================================================
// UnionZipper
// =============================================================================

/// A zipper that presents multiple dictionaries as a unified view.
///
/// `UnionZipper` wraps multiple zippers and presents their union as a single
/// navigable structure. Terms that exist in multiple dictionaries appear only
/// once during iteration.
///
/// # Type Parameters
///
/// * `Z` - The underlying zipper type (must implement `DictZipper`)
/// * `S` - The value merge strategy (defaults to `FirstWins`)
///
/// # Navigation
///
/// Navigation through the union considers all underlying dictionaries:
/// - `is_final()` returns true if ANY dictionary marks the position as final
/// - `descend(label)` succeeds if ANY dictionary has the path
/// - `children()` returns the union of all children from all dictionaries
///
/// # Examples
///
/// See module-level documentation for comprehensive examples.
#[derive(Clone, Debug)]
pub struct UnionZipper<Z: DictZipper, S = FirstWins> {
    /// The underlying zippers. `None` entries indicate dictionaries that don't
    /// have the current path.
    zippers: Vec<Option<Z>>,

    /// Path from root to current position.
    path: Vec<Z::Unit>,

    /// Value merge strategy.
    strategy: S,
}

impl<Z: DictZipper> UnionZipper<Z, FirstWins> {
    /// Create a new union zipper with the default `FirstWins` strategy.
    ///
    /// # Arguments
    ///
    /// * `zippers` - Zippers to union, each positioned at their respective roots
    ///
    /// # Returns
    ///
    /// A new `UnionZipper` positioned at the union root.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::prelude::*;
    /// use libdictenstein::union_zipper::UnionZipper;
    /// use libdictenstein::double_array_trie_zipper::DoubleArrayTrieZipper;
    ///
    /// let dict1 = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());
    /// let dict2 = DoubleArrayTrie::from_terms(vec!["fish", "bird"].iter());
    ///
    /// let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    /// let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);
    ///
    /// let union = UnionZipper::new(vec![z1, z2]);
    /// ```
    pub fn new(zippers: Vec<Z>) -> Self {
        Self {
            zippers: zippers.into_iter().map(Some).collect(),
            path: Vec::new(),
            strategy: FirstWins,
        }
    }
}

impl<Z: DictZipper, S: Clone + Send + Sync> UnionZipper<Z, S> {
    /// Create a new union zipper with a custom merge strategy.
    ///
    /// # Arguments
    ///
    /// * `zippers` - Zippers to union, each positioned at their respective roots
    /// * `strategy` - The merge strategy for handling duplicate values
    ///
    /// # Returns
    ///
    /// A new `UnionZipper` with the specified strategy.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::prelude::*;
    /// use libdictenstein::union_zipper::{UnionZipper, LastWins};
    /// use libdictenstein::double_array_trie_zipper::DoubleArrayTrieZipper;
    ///
    /// let dict1 = DoubleArrayTrie::from_terms_with_values(vec![("cat", 1)].into_iter());
    /// let dict2 = DoubleArrayTrie::from_terms_with_values(vec![("cat", 10)].into_iter());
    ///
    /// let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    /// let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);
    ///
    /// let union = UnionZipper::with_strategy(vec![z1, z2], LastWins);
    /// ```
    pub fn with_strategy(zippers: Vec<Z>, strategy: S) -> Self {
        Self {
            zippers: zippers.into_iter().map(Some).collect(),
            path: Vec::new(),
            strategy,
        }
    }

    /// Get the number of underlying dictionaries.
    ///
    /// # Returns
    ///
    /// The total number of dictionaries in this union.
    pub fn dictionary_count(&self) -> usize {
        self.zippers.len()
    }

    /// Get the number of active dictionaries at the current position.
    ///
    /// A dictionary is "active" if it has the current path. This count decreases
    /// as you descend into paths that only exist in some dictionaries.
    ///
    /// # Returns
    ///
    /// The number of dictionaries that have the current path.
    pub fn active_dictionary_count(&self) -> usize {
        self.zippers.iter().filter(|z| z.is_some()).count()
    }

    /// Create an iterator over all terms in the union.
    ///
    /// Terms are yielded exactly once even if they exist in multiple dictionaries.
    /// The iteration order follows a depth-first traversal with sorted labels.
    ///
    /// # Returns
    ///
    /// An iterator yielding `(path, zipper)` pairs for each term.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::prelude::*;
    /// use libdictenstein::union_zipper::UnionZipperExt;
    /// use libdictenstein::double_array_trie_zipper::DoubleArrayTrieZipper;
    ///
    /// let dict1 = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());
    /// let dict2 = DoubleArrayTrie::from_terms(vec!["cat", "fish"].iter());
    ///
    /// let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    /// let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);
    ///
    /// let union = z1.union_with(z2);
    /// let count = union.iter().count();
    /// assert_eq!(count, 3); // cat, dog, fish (no duplicates)
    /// ```
    pub fn iter(&self) -> UnionIterator<Z, S> {
        UnionIterator::new(self.clone())
    }
}

impl<Z: DictZipper, S: Clone + Send + Sync> DictZipper for UnionZipper<Z, S> {
    type Unit = Z::Unit;

    fn is_final(&self) -> bool {
        // Final if ANY dictionary marks this position as final
        self.zippers
            .iter()
            .any(|z| z.as_ref().is_some_and(|z| z.is_final()))
    }

    fn descend(&self, label: Self::Unit) -> Option<Self> {
        // Descend in all active zippers
        let new_zippers: Vec<Option<Z>> = self
            .zippers
            .iter()
            .map(|z| z.as_ref().and_then(|z| z.descend(label)))
            .collect();

        // Return new union if at least one zipper has the path
        if new_zippers.iter().any(|z| z.is_some()) {
            let mut new_path = self.path.clone();
            new_path.push(label);

            Some(Self {
                zippers: new_zippers,
                path: new_path,
                strategy: self.strategy.clone(),
            })
        } else {
            None
        }
    }

    fn children(&self) -> impl Iterator<Item = (Self::Unit, Self)> {
        // Collect unique labels from all active zippers
        let mut labels: Vec<Z::Unit> = self
            .zippers
            .iter()
            .filter_map(|z| z.as_ref())
            .flat_map(|z| z.children().map(|(label, _)| label))
            .collect();

        // Remove duplicates and sort for deterministic ordering
        labels.sort_by(|a, b| {
            // Use Debug trait for comparison since CharUnit doesn't require Ord
            // This works for u8, char, and u64 which all have natural ordering
            format!("{:?}", a).cmp(&format!("{:?}", b))
        });
        labels.dedup();

        // Create child zippers for each unique label
        let self_clone = self.clone();
        labels
            .into_iter()
            .filter_map(move |label| self_clone.descend(label).map(|child| (label, child)))
    }

    fn path(&self) -> Vec<Self::Unit> {
        self.path.clone()
    }
}

impl<Z: ValuedDictZipper, S: ValueMergeStrategy<Z::Value> + Clone + Send + Sync> ValuedDictZipper
    for UnionZipper<Z, S>
{
    type Value = Z::Value;

    fn value(&self) -> Option<Self::Value> {
        // Collect values from all active zippers that are final
        let mut result: Option<Z::Value> = None;

        for zipper in self.zippers.iter().filter_map(|z| z.as_ref()) {
            if let Some(v) = zipper.value() {
                result = Some(match result {
                    Some(existing) => self.strategy.merge(existing, v),
                    None => v,
                });
            }
        }

        result
    }
}

// =============================================================================
// UnionIterator
// =============================================================================

/// Iterator over all terms in a union of dictionaries.
///
/// This iterator performs depth-first traversal and yields each unique term
/// exactly once, even if it exists in multiple underlying dictionaries.
///
/// # Type Parameters
///
/// * `Z` - The underlying zipper type
/// * `S` - The value merge strategy
///
/// # Iterator Item
///
/// Returns `(Vec<Z::Unit>, UnionZipper<Z, S>)`:
/// - `Vec<Z::Unit>` - Complete path (term) as sequence of units
/// - `UnionZipper<Z, S>` - Zipper positioned at the final node
///
/// # Deduplication
///
/// Deduplication is path-based: when a term is yielded, its path is recorded
/// in a HashSet to prevent duplicate yields.
pub struct UnionIterator<Z: DictZipper, S = FirstWins> {
    /// DFS traversal stack
    stack: Vec<UnionZipper<Z, S>>,

    /// Paths already yielded (for deduplication)
    seen: HashSet<Vec<Z::Unit>>,
}

impl<Z: DictZipper, S: Clone + Send + Sync> UnionIterator<Z, S> {
    /// Create a new iterator starting from the given union zipper.
    fn new(zipper: UnionZipper<Z, S>) -> Self {
        let mut stack = Vec::with_capacity(16);
        stack.push(zipper);
        Self {
            stack,
            seen: HashSet::new(),
        }
    }
}

impl<Z: DictZipper, S: Clone + Send + Sync> Iterator for UnionIterator<Z, S> {
    type Item = (Vec<Z::Unit>, UnionZipper<Z, S>);

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(zipper) = self.stack.pop() {
            // Push all children onto stack for continued DFS traversal
            for (_label, child) in zipper.children() {
                self.stack.push(child);
            }

            // If this is a complete term and we haven't seen it, yield it
            if zipper.is_final() {
                let path = zipper.path();
                if self.seen.insert(path.clone()) {
                    return Some((path, zipper));
                }
            }
        }

        None
    }
}

// =============================================================================
// ValuedUnionIterator
// =============================================================================

/// Iterator over (term, value) pairs in a union of valued dictionaries.
///
/// This iterator wraps `UnionIterator` and extracts merged values from final
/// nodes using the configured merge strategy.
///
/// # Type Parameters
///
/// * `Z` - The underlying valued zipper type
/// * `S` - The value merge strategy
///
/// # Iterator Item
///
/// Returns `(Vec<Z::Unit>, Z::Value)`:
/// - `Vec<Z::Unit>` - Complete path (term) as sequence of units
/// - `Z::Value` - Merged value for this term
pub struct ValuedUnionIterator<Z: ValuedDictZipper, S> {
    inner: UnionIterator<Z, S>,
}

impl<Z: ValuedDictZipper, S: ValueMergeStrategy<Z::Value> + Clone + Send + Sync>
    ValuedUnionIterator<Z, S>
{
    /// Create a new valued iterator from a union zipper.
    pub fn new(zipper: UnionZipper<Z, S>) -> Self {
        Self {
            inner: UnionIterator::new(zipper),
        }
    }
}

impl<Z: ValuedDictZipper, S: ValueMergeStrategy<Z::Value> + Clone + Send + Sync> Iterator
    for ValuedUnionIterator<Z, S>
{
    type Item = (Vec<Z::Unit>, Z::Value);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let (path, zipper) = self.inner.next()?;
            if let Some(value) = zipper.value() {
                return Some((path, value));
            }
            // Continue if no value (shouldn't happen for valid final nodes, but be safe)
        }
    }
}

// =============================================================================
// UnionZipperExt Extension Trait
// =============================================================================

/// Extension trait for ergonomic union zipper creation.
///
/// This trait is automatically implemented for all `DictZipper` types, providing
/// convenient methods to create union zippers.
///
/// # Examples
///
/// ```rust
/// use libdictenstein::prelude::*;
/// use libdictenstein::union_zipper::UnionZipperExt;
/// use libdictenstein::double_array_trie_zipper::DoubleArrayTrieZipper;
///
/// let dict1 = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());
/// let dict2 = DoubleArrayTrie::from_terms(vec!["fish", "bird"].iter());
///
/// let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
/// let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);
///
/// // Simple two-zipper union
/// let union = z1.clone().union_with(z2.clone());
///
/// // Multi-zipper union
/// let dict3 = DoubleArrayTrie::from_terms(vec!["elephant"].iter());
/// let z3 = DoubleArrayTrieZipper::new_from_dict(&dict3);
/// let multi_union = z1.union_all(vec![z2, z3]);
/// ```
pub trait UnionZipperExt: DictZipper + Sized {
    /// Create a union of this zipper with another.
    ///
    /// # Arguments
    ///
    /// * `other` - Another zipper of the same type
    ///
    /// # Returns
    ///
    /// A `UnionZipper` combining both dictionaries with `FirstWins` strategy.
    fn union_with(self, other: Self) -> UnionZipper<Self> {
        UnionZipper::new(vec![self, other])
    }

    /// Create a union of this zipper with multiple others.
    ///
    /// # Arguments
    ///
    /// * `others` - Additional zippers to include in the union
    ///
    /// # Returns
    ///
    /// A `UnionZipper` combining all dictionaries with `FirstWins` strategy.
    fn union_all(self, others: impl IntoIterator<Item = Self>) -> UnionZipper<Self> {
        let mut zippers = vec![self];
        zippers.extend(others);
        UnionZipper::new(zippers)
    }
}

/// Blanket implementation: all DictZippers automatically get UnionZipperExt support.
impl<Z: DictZipper> UnionZipperExt for Z {}

// =============================================================================
// ValuedUnionZipperExt Extension Trait
// =============================================================================

/// Extension trait for valued union zipper iteration.
///
/// This trait is automatically implemented for all `ValuedDictZipper` types,
/// providing methods to iterate with values using merge strategies.
pub trait ValuedUnionZipperExt: ValuedDictZipper + Sized {
    /// Create a union of this zipper with another, using a custom strategy.
    ///
    /// # Arguments
    ///
    /// * `other` - Another zipper of the same type
    /// * `strategy` - The merge strategy for duplicate values
    ///
    /// # Returns
    ///
    /// A `UnionZipper` with the specified strategy.
    fn union_with_strategy<S: ValueMergeStrategy<Self::Value> + Clone + Send + Sync>(
        self,
        other: Self,
        strategy: S,
    ) -> UnionZipper<Self, S> {
        UnionZipper::with_strategy(vec![self, other], strategy)
    }
}

/// Blanket implementation: all ValuedDictZippers get ValuedUnionZipperExt support.
impl<Z: ValuedDictZipper> ValuedUnionZipperExt for Z {}

// =============================================================================
// Unit Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::double_array_trie::DoubleArrayTrie;
    use crate::double_array_trie_zipper::DoubleArrayTrieZipper;

    fn sorted_strings(mut v: Vec<String>) -> Vec<String> {
        v.sort();
        v
    }

    #[test]
    fn test_union_basic() {
        let dict1 = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());
        let dict2 = DoubleArrayTrie::from_terms(vec!["fish", "bird"].iter());

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

        let union = UnionZipper::new(vec![z1, z2]);

        let results: Vec<String> = sorted_strings(
            union
                .iter()
                .map(|(path, _)| String::from_utf8(path).unwrap())
                .collect(),
        );

        assert_eq!(results, vec!["bird", "cat", "dog", "fish"]);
    }

    #[test]
    fn test_union_with_overlap() {
        let dict1 = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());
        let dict2 = DoubleArrayTrie::from_terms(vec!["cat", "fish"].iter());

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

        let union = z1.union_with(z2);

        let results: Vec<String> = sorted_strings(
            union
                .iter()
                .map(|(path, _)| String::from_utf8(path).unwrap())
                .collect(),
        );

        // "cat" should appear only once
        assert_eq!(results, vec!["cat", "dog", "fish"]);
    }

    #[test]
    fn test_union_descend() {
        let dict1 = DoubleArrayTrie::from_terms(vec!["cat", "car"].iter());
        let dict2 = DoubleArrayTrie::from_terms(vec!["cab", "can"].iter());

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

        let union = z1.union_with(z2);

        // Navigate to 'c' -> 'a'
        let ca = union
            .descend(b'c')
            .and_then(|z| z.descend(b'a'))
            .expect("Should be able to descend to 'ca'");

        // Should have children from both dictionaries
        let mut children: Vec<u8> = ca.children().map(|(label, _)| label).collect();
        children.sort();

        assert_eq!(children, vec![b'b', b'n', b'r', b't']);
    }

    #[test]
    fn test_union_is_final() {
        let dict1 = DoubleArrayTrie::from_terms(vec!["cat"].iter());
        let dict2 = DoubleArrayTrie::from_terms(vec!["dog"].iter());

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

        let union = z1.union_with(z2);

        // Navigate to "cat"
        let cat = union
            .descend(b'c')
            .and_then(|z| z.descend(b'a'))
            .and_then(|z| z.descend(b't'))
            .expect("Should find 'cat'");

        assert!(cat.is_final());
        assert_eq!(cat.path(), b"cat".to_vec());
    }

    #[test]
    fn test_union_nonexistent_path() {
        let dict1 = DoubleArrayTrie::from_terms(vec!["cat"].iter());
        let dict2 = DoubleArrayTrie::from_terms(vec!["dog"].iter());

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

        let union = z1.union_with(z2);

        // Try to navigate to 'x' - doesn't exist in either
        assert!(union.descend(b'x').is_none());
    }

    #[test]
    fn test_union_empty_dictionaries() {
        let dict1: DoubleArrayTrie = DoubleArrayTrie::new();
        let dict2: DoubleArrayTrie = DoubleArrayTrie::new();

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

        let union = z1.union_with(z2);

        let count = union.iter().count();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_union_one_empty() {
        let dict1 = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());
        let dict2: DoubleArrayTrie = DoubleArrayTrie::new();

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

        let union = z1.union_with(z2);

        let results: Vec<String> = sorted_strings(
            union
                .iter()
                .map(|(path, _)| String::from_utf8(path).unwrap())
                .collect(),
        );

        assert_eq!(results, vec!["cat", "dog"]);
    }

    #[test]
    fn test_valued_union_first_wins() {
        let dict1 =
            DoubleArrayTrie::from_terms_with_values(vec![("cat", 1usize), ("dog", 2)].into_iter());
        let dict2 =
            DoubleArrayTrie::from_terms_with_values(vec![("cat", 10usize), ("fish", 3)].into_iter());

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

        let union = UnionZipper::new(vec![z1, z2]);

        // Navigate to "cat"
        let cat = union
            .descend(b'c')
            .and_then(|z| z.descend(b'a'))
            .and_then(|z| z.descend(b't'))
            .expect("Should find 'cat'");

        // FirstWins: should get value 1 from dict1
        assert_eq!(cat.value(), Some(1));
    }

    #[test]
    fn test_valued_union_last_wins() {
        let dict1 =
            DoubleArrayTrie::from_terms_with_values(vec![("cat", 1usize), ("dog", 2)].into_iter());
        let dict2 =
            DoubleArrayTrie::from_terms_with_values(vec![("cat", 10usize), ("fish", 3)].into_iter());

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

        let union = UnionZipper::with_strategy(vec![z1, z2], LastWins);

        // Navigate to "cat"
        let cat = union
            .descend(b'c')
            .and_then(|z| z.descend(b'a'))
            .and_then(|z| z.descend(b't'))
            .expect("Should find 'cat'");

        // LastWins: should get value 10 from dict2
        assert_eq!(cat.value(), Some(10));
    }

    #[test]
    fn test_valued_union_iterator() {
        let dict1 =
            DoubleArrayTrie::from_terms_with_values(vec![("cat", 1usize), ("dog", 2)].into_iter());
        let dict2 =
            DoubleArrayTrie::from_terms_with_values(vec![("cat", 10usize), ("fish", 3)].into_iter());

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

        let union = UnionZipper::new(vec![z1, z2]);
        let valued_iter = ValuedUnionIterator::new(union);

        let mut results: Vec<(String, usize)> = valued_iter
            .map(|(path, val)| (String::from_utf8(path).unwrap(), val))
            .collect();

        results.sort_by(|a, b| a.0.cmp(&b.0));

        assert_eq!(
            results,
            vec![
                ("cat".to_string(), 1),  // FirstWins
                ("dog".to_string(), 2),
                ("fish".to_string(), 3),
            ]
        );
    }

    #[test]
    fn test_union_all() {
        let dict1 = DoubleArrayTrie::from_terms(vec!["cat"].iter());
        let dict2 = DoubleArrayTrie::from_terms(vec!["dog"].iter());
        let dict3 = DoubleArrayTrie::from_terms(vec!["fish"].iter());

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);
        let z3 = DoubleArrayTrieZipper::new_from_dict(&dict3);

        let union = z1.union_all(vec![z2, z3]);

        let results: Vec<String> = sorted_strings(
            union
                .iter()
                .map(|(path, _)| String::from_utf8(path).unwrap())
                .collect(),
        );

        assert_eq!(results, vec!["cat", "dog", "fish"]);
    }

    #[test]
    fn test_dictionary_count() {
        let dict1 = DoubleArrayTrie::from_terms(vec!["cat"].iter());
        let dict2 = DoubleArrayTrie::from_terms(vec!["dog"].iter());
        let dict3 = DoubleArrayTrie::from_terms(vec!["fish"].iter());

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);
        let z3 = DoubleArrayTrieZipper::new_from_dict(&dict3);

        let union = z1.union_all(vec![z2, z3]);

        assert_eq!(union.dictionary_count(), 3);
        assert_eq!(union.active_dictionary_count(), 3);

        // After descending to 'c', only dict1 is active
        let c = union.descend(b'c').unwrap();
        assert_eq!(c.dictionary_count(), 3);
        assert_eq!(c.active_dictionary_count(), 1);
    }

    #[test]
    fn test_custom_merge_strategy() {
        #[derive(Clone)]
        struct Sum;

        impl ValueMergeStrategy<usize> for Sum {
            fn merge(&self, existing: usize, new: usize) -> usize {
                existing + new
            }
        }

        let dict1 =
            DoubleArrayTrie::from_terms_with_values(vec![("cat", 1usize)].into_iter());
        let dict2 =
            DoubleArrayTrie::from_terms_with_values(vec![("cat", 10usize)].into_iter());

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

        let union = UnionZipper::with_strategy(vec![z1, z2], Sum);

        let cat = union
            .descend(b'c')
            .and_then(|z| z.descend(b'a'))
            .and_then(|z| z.descend(b't'))
            .expect("Should find 'cat'");

        // Sum: should get 1 + 10 = 11
        assert_eq!(cat.value(), Some(11));
    }

    // =========================================================================
    // Lattice Trait Tests
    // =========================================================================

    #[test]
    fn test_lattice_numeric_u32() {
        // join = max, meet = min
        assert_eq!(5u32.join(&3), 5);
        assert_eq!(3u32.join(&5), 5);
        assert_eq!(5u32.meet(&3), 3);
        assert_eq!(3u32.meet(&5), 3);

        // Idempotency
        assert_eq!(5u32.join(&5), 5);
        assert_eq!(5u32.meet(&5), 5);
    }

    #[test]
    fn test_lattice_numeric_i32() {
        // Negative numbers
        assert_eq!((-5i32).join(&3), 3);
        assert_eq!((-5i32).meet(&3), -5);
    }

    #[test]
    fn test_lattice_numeric_f64() {
        assert_eq!(5.0f64.join(&3.0), 5.0);
        assert_eq!(5.0f64.meet(&3.0), 3.0);
    }

    #[test]
    fn test_lattice_bool() {
        // join = OR
        assert!(true.join(&false));
        assert!(false.join(&true));
        assert!(true.join(&true));
        assert!(!false.join(&false));

        // meet = AND
        assert!(true.meet(&true));
        assert!(!true.meet(&false));
        assert!(!false.meet(&true));
        assert!(!false.meet(&false));
    }

    #[test]
    fn test_lattice_option() {
        let some_5 = Some(5u32);
        let some_3 = Some(3u32);
        let none: Option<u32> = None;

        // join: Some if either Some
        assert_eq!(some_5.join(&some_3), Some(5)); // max
        assert_eq!(some_5.join(&none), Some(5));
        assert_eq!(none.join(&some_3), Some(3));
        assert_eq!(none.join(&none), None);

        // meet: Some only if both Some
        assert_eq!(some_5.meet(&some_3), Some(3)); // min
        assert_eq!(some_5.meet(&none), None);
        assert_eq!(none.meet(&some_3), None);
        assert_eq!(none.meet(&none), None);
    }

    #[test]
    fn test_lattice_hashset() {
        let set1: HashSet<i32> = [1, 2, 3].into_iter().collect();
        let set2: HashSet<i32> = [2, 3, 4].into_iter().collect();

        // join = union
        let joined = set1.join(&set2);
        assert_eq!(joined, [1, 2, 3, 4].into_iter().collect());

        // meet = intersection
        let met = set1.meet(&set2);
        assert_eq!(met, [2, 3].into_iter().collect());
    }

    #[test]
    fn test_lattice_hashset_disjoint() {
        let set1: HashSet<i32> = [1, 2].into_iter().collect();
        let set2: HashSet<i32> = [3, 4].into_iter().collect();

        let joined = set1.join(&set2);
        assert_eq!(joined, [1, 2, 3, 4].into_iter().collect());

        let met = set1.meet(&set2);
        assert!(met.is_empty());
    }

    #[test]
    fn test_lattice_vec() {
        let vec1 = vec![1, 2, 3];
        let vec2 = vec![2, 3, 4];

        // join = concat + dedup
        let joined = vec1.join(&vec2);
        assert_eq!(joined, vec![1, 2, 3, 4]);

        // meet = intersection preserving order
        let met = vec1.meet(&vec2);
        assert_eq!(met, vec![2, 3]);
    }

    #[test]
    fn test_lattice_vec_preserves_order() {
        let vec1 = vec![3, 1, 2];
        let vec2 = vec![4, 2, 1];

        // join preserves order of first, then appends new elements
        let joined = vec1.join(&vec2);
        assert_eq!(joined, vec![3, 1, 2, 4]);

        // meet preserves order of first
        let met = vec1.meet(&vec2);
        assert_eq!(met, vec![1, 2]);
    }

    // =========================================================================
    // LatticeJoin / LatticeMeet Strategy Tests
    // =========================================================================

    #[test]
    fn test_lattice_join_strategy_numeric() {
        let dict1 = DoubleArrayTrie::from_terms_with_values(vec![("score", 85u32)].into_iter());
        let dict2 = DoubleArrayTrie::from_terms_with_values(vec![("score", 92u32)].into_iter());

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

        let union = UnionZipper::with_strategy(vec![z1, z2], LatticeJoin);

        let score = union
            .descend(b's')
            .and_then(|z| z.descend(b'c'))
            .and_then(|z| z.descend(b'o'))
            .and_then(|z| z.descend(b'r'))
            .and_then(|z| z.descend(b'e'))
            .expect("Should find 'score'");

        // LatticeJoin: max(85, 92) = 92
        assert_eq!(score.value(), Some(92));
    }

    #[test]
    fn test_lattice_meet_strategy_numeric() {
        let dict1 = DoubleArrayTrie::from_terms_with_values(vec![("score", 85u32)].into_iter());
        let dict2 = DoubleArrayTrie::from_terms_with_values(vec![("score", 92u32)].into_iter());

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

        let union = UnionZipper::with_strategy(vec![z1, z2], LatticeMeet);

        let score = union
            .descend(b's')
            .and_then(|z| z.descend(b'c'))
            .and_then(|z| z.descend(b'o'))
            .and_then(|z| z.descend(b'r'))
            .and_then(|z| z.descend(b'e'))
            .expect("Should find 'score'");

        // LatticeMeet: min(85, 92) = 85
        assert_eq!(score.value(), Some(85));
    }

    #[test]
    fn test_lattice_join_strategy_hashset() {
        let dict1 = DoubleArrayTrie::from_terms_with_values(
            vec![("key", HashSet::from([1, 2]))].into_iter(),
        );
        let dict2 = DoubleArrayTrie::from_terms_with_values(
            vec![("key", HashSet::from([2, 3]))].into_iter(),
        );

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

        let union = UnionZipper::with_strategy(vec![z1, z2], LatticeJoin);

        let key = union
            .descend(b'k')
            .and_then(|z| z.descend(b'e'))
            .and_then(|z| z.descend(b'y'))
            .expect("Should find 'key'");

        // LatticeJoin: {1, 2} ∪ {2, 3} = {1, 2, 3}
        assert_eq!(key.value(), Some(HashSet::from([1, 2, 3])));
    }

    #[test]
    fn test_lattice_meet_strategy_hashset() {
        let dict1 = DoubleArrayTrie::from_terms_with_values(
            vec![("key", HashSet::from([1, 2, 3]))].into_iter(),
        );
        let dict2 = DoubleArrayTrie::from_terms_with_values(
            vec![("key", HashSet::from([2, 3, 4]))].into_iter(),
        );

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

        let union = UnionZipper::with_strategy(vec![z1, z2], LatticeMeet);

        let key = union
            .descend(b'k')
            .and_then(|z| z.descend(b'e'))
            .and_then(|z| z.descend(b'y'))
            .expect("Should find 'key'");

        // LatticeMeet: {1, 2, 3} ∩ {2, 3, 4} = {2, 3}
        assert_eq!(key.value(), Some(HashSet::from([2, 3])));
    }

    #[test]
    fn test_lattice_join_three_dicts() {
        let dict1 = DoubleArrayTrie::from_terms_with_values(
            vec![("ctx", HashSet::from([1]))].into_iter(),
        );
        let dict2 = DoubleArrayTrie::from_terms_with_values(
            vec![("ctx", HashSet::from([2]))].into_iter(),
        );
        let dict3 = DoubleArrayTrie::from_terms_with_values(
            vec![("ctx", HashSet::from([3]))].into_iter(),
        );

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);
        let z3 = DoubleArrayTrieZipper::new_from_dict(&dict3);

        let union = UnionZipper::with_strategy(vec![z1, z2, z3], LatticeJoin);

        let ctx = union
            .descend(b'c')
            .and_then(|z| z.descend(b't'))
            .and_then(|z| z.descend(b'x'))
            .expect("Should find 'ctx'");

        // LatticeJoin: {1} ∪ {2} ∪ {3} = {1, 2, 3}
        assert_eq!(ctx.value(), Some(HashSet::from([1, 2, 3])));
    }

    #[test]
    fn test_lattice_meet_three_dicts() {
        let dict1 = DoubleArrayTrie::from_terms_with_values(
            vec![("ctx", HashSet::from([1, 2, 3, 4]))].into_iter(),
        );
        let dict2 = DoubleArrayTrie::from_terms_with_values(
            vec![("ctx", HashSet::from([2, 3, 4, 5]))].into_iter(),
        );
        let dict3 = DoubleArrayTrie::from_terms_with_values(
            vec![("ctx", HashSet::from([3, 4, 5, 6]))].into_iter(),
        );

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);
        let z3 = DoubleArrayTrieZipper::new_from_dict(&dict3);

        let union = UnionZipper::with_strategy(vec![z1, z2, z3], LatticeMeet);

        let ctx = union
            .descend(b'c')
            .and_then(|z| z.descend(b't'))
            .and_then(|z| z.descend(b'x'))
            .expect("Should find 'ctx'");

        // LatticeMeet: {1,2,3,4} ∩ {2,3,4,5} ∩ {3,4,5,6} = {3, 4}
        assert_eq!(ctx.value(), Some(HashSet::from([3, 4])));
    }

    #[test]
    fn test_lattice_join_with_bool() {
        let dict1 = DoubleArrayTrie::from_terms_with_values(vec![("flag", false)].into_iter());
        let dict2 = DoubleArrayTrie::from_terms_with_values(vec![("flag", true)].into_iter());

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

        let union = UnionZipper::with_strategy(vec![z1, z2], LatticeJoin);

        let flag = union
            .descend(b'f')
            .and_then(|z| z.descend(b'l'))
            .and_then(|z| z.descend(b'a'))
            .and_then(|z| z.descend(b'g'))
            .expect("Should find 'flag'");

        // LatticeJoin (bool): false OR true = true
        assert_eq!(flag.value(), Some(true));
    }

    #[test]
    fn test_lattice_meet_with_bool() {
        let dict1 = DoubleArrayTrie::from_terms_with_values(vec![("flag", false)].into_iter());
        let dict2 = DoubleArrayTrie::from_terms_with_values(vec![("flag", true)].into_iter());

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

        let union = UnionZipper::with_strategy(vec![z1, z2], LatticeMeet);

        let flag = union
            .descend(b'f')
            .and_then(|z| z.descend(b'l'))
            .and_then(|z| z.descend(b'a'))
            .and_then(|z| z.descend(b'g'))
            .expect("Should find 'flag'");

        // LatticeMeet (bool): false AND true = false
        assert_eq!(flag.value(), Some(false));
    }
}
