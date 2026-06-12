//! Value merge strategies for unioning dictionaries.
//!
//! Extracted from `union_zipper.rs` (C6 dedup) into its own module so the
//! merge-strategy surface is easier to find and extend. Re-exported from
//! [`crate::union_zipper`] for back-compat.

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
/// For value types that form a lattice, see
/// [`super::lattice::LatticeJoin`] and [`super::lattice::LatticeMeet`].
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
/// # use libdictenstein::double_array_trie::zipper::DoubleArrayTrieZipper;
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
/// # use libdictenstein::double_array_trie::zipper::DoubleArrayTrieZipper;
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
