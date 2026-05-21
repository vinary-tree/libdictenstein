//! Value diff zipper for identifying terms with differing values.
//!
//! This module provides a `ValueDiffZipper` that iterates over terms present in BOTH
//! dictionaries where the associated values DIFFER. This is useful for detecting
//! changes between versions of a valued dictionary.
//!
//! # Use Cases
//!
//! - **Vocabulary version diff**: Identify terms whose frequency/count changed
//! - **Configuration comparison**: Find settings with different values
//! - **Index delta computation**: Detect modified entries between snapshots
//!
//! # Examples
//!
//! ## Finding Terms with Changed Values
//!
//! ```rust
//! use libdictenstein::prelude::*;
//! use libdictenstein::value_diff_zipper::{ValueDiffZipper, ValueDiffZipperExt};
//! use libdictenstein::double_array_trie_zipper::DoubleArrayTrieZipper;
//!
//! // Two versions of a frequency dictionary
//! let version1 = DoubleArrayTrie::from_terms_with_values(
//!     vec![("cat", 10usize), ("dog", 20), ("fish", 30)].into_iter()
//! );
//! let version2 = DoubleArrayTrie::from_terms_with_values(
//!     vec![("cat", 10usize), ("dog", 25), ("fish", 35)].into_iter()
//! );
//!
//! let z1 = DoubleArrayTrieZipper::new_from_dict(&version1);
//! let z2 = DoubleArrayTrieZipper::new_from_dict(&version2);
//!
//! // Find terms with different values
//! let diff = z1.value_diff_with(z2);
//!
//! let mut results: Vec<_> = diff.iter_diffs()
//!     .map(|d| (String::from_utf8(d.path).unwrap(), d.left_value, d.right_value))
//!     .collect();
//! results.sort_by(|a, b| a.0.cmp(&b.0));
//!
//! // "cat" has same value (10), excluded
//! // "dog" changed from 20 to 25
//! // "fish" changed from 30 to 35
//! assert_eq!(results, vec![
//!     ("dog".to_string(), 20, 25),
//!     ("fish".to_string(), 30, 35),
//! ]);
//! ```
//!
//! ## Navigation for Targeted Comparison
//!
//! ```rust
//! use libdictenstein::prelude::*;
//! use libdictenstein::value_diff_zipper::{ValueDiffZipper, ValueDiffZipperExt};
//! use libdictenstein::double_array_trie_zipper::DoubleArrayTrieZipper;
//! use libdictenstein::zipper::DictZipper;
//!
//! let dict1 = DoubleArrayTrie::from_terms_with_values(
//!     vec![("score", 85u32), ("count", 100)].into_iter()
//! );
//! let dict2 = DoubleArrayTrie::from_terms_with_values(
//!     vec![("score", 92u32), ("count", 100)].into_iter()
//! );
//!
//! let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
//! let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);
//!
//! let diff = z1.value_diff_with(z2);
//!
//! // Navigate to "score" - values differ, so is_final returns true
//! let score = diff.descend(b's')
//!     .and_then(|z| z.descend(b'c'))
//!     .and_then(|z| z.descend(b'o'))
//!     .and_then(|z| z.descend(b'r'))
//!     .and_then(|z| z.descend(b'e'))
//!     .unwrap();
//! assert!(score.is_final()); // Values differ (85 vs 92)
//!
//! // Navigate to "count" - values are same, so is_final returns false
//! let count = diff.descend(b'c')
//!     .and_then(|z| z.descend(b'o'))
//!     .and_then(|z| z.descend(b'u'))
//!     .and_then(|z| z.descend(b'n'))
//!     .and_then(|z| z.descend(b't'))
//!     .unwrap();
//! assert!(!count.is_final()); // Values are same (100 = 100)
//! ```
//!
//! # Output Format
//!
//! The iterator yields `ValueDiff<U, V>` structs containing:
//! - `path: Vec<U>` - The term as a sequence of units
//! - `left_value: V` - Value from the left/first dictionary
//! - `right_value: V` - Value from the right/second dictionary
//!
//! # Performance
//!
//! - **Navigation**: O(k) where k = prefix length (parallel descend in both)
//! - **Children**: O(c) where c = children in intersection
//! - **Iteration**: O(m) where m = terms in intersection
//! - **Per-term check**: O(1) value comparison
//!
//! # Backend Compatibility
//!
//! Works uniformly across all dictionary backends via the `ValuedDictZipper` trait.

use std::collections::HashSet;

use crate::value::DictionaryValue;
use crate::zipper::{DictZipper, ValuedDictZipper};
use crate::CharUnit;

// =============================================================================
// ValueDiff
// =============================================================================

/// A single value difference entry between two dictionaries.
///
/// Represents a term that exists in both dictionaries but has different
/// associated values.
///
/// # Type Parameters
///
/// * `U` - The character unit type (u8, char, or u64)
/// * `V` - The value type
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueDiff<U: CharUnit, V: DictionaryValue> {
    /// Path to the differing term (as accumulated units).
    ///
    /// For byte-level dictionaries: `Vec<u8>` (convert with `String::from_utf8`)
    /// For char-level dictionaries: `Vec<char>` (convert with `iter().collect()`)
    /// For u64 dictionaries: `Vec<u64>`
    pub path: Vec<U>,

    /// Value in the left (first) dictionary.
    pub left_value: V,

    /// Value in the right (second) dictionary.
    pub right_value: V,
}

impl<U: CharUnit, V: DictionaryValue> ValueDiff<U, V> {
    /// Create a new value diff entry.
    pub fn new(path: Vec<U>, left_value: V, right_value: V) -> Self {
        Self {
            path,
            left_value,
            right_value,
        }
    }

    /// Get the path as a string (for byte-level dictionaries).
    ///
    /// Uses lossy UTF-8 decoding for non-UTF-8 byte sequences.
    pub fn path_string(&self) -> String {
        U::to_string(&self.path)
    }
}

// =============================================================================
// ValueDiffZipper
// =============================================================================

/// A zipper for finding terms with differing values in two dictionaries.
///
/// `ValueDiffZipper` navigates through the intersection of two dictionaries
/// and identifies positions where both are final but have different values.
///
/// # Type Parameters
///
/// * `Z` - The underlying valued zipper type
///
/// # Navigation Semantics
///
/// - `is_final()` returns true if BOTH are final AND values are DIFFERENT
/// - `descend(label)` requires BOTH to have the path (intersection navigation)
/// - `children()` returns intersection of children from both dictionaries
///
/// # Value Access
///
/// Use `left_value()` and `right_value()` to access individual values,
/// or `values()` to get both as a tuple.
#[derive(Clone, Debug)]
pub struct ValueDiffZipper<Z: ValuedDictZipper> {
    /// The left (first) dictionary zipper.
    left: Option<Z>,

    /// The right (second) dictionary zipper.
    right: Option<Z>,

    /// Path from root to current position.
    path: Vec<Z::Unit>,
}

impl<Z: ValuedDictZipper> ValueDiffZipper<Z>
where
    Z::Value: PartialEq,
{
    /// Create a new value diff zipper from two dictionaries.
    ///
    /// # Arguments
    ///
    /// * `left` - The left/first dictionary zipper
    /// * `right` - The right/second dictionary zipper
    ///
    /// # Returns
    ///
    /// A new `ValueDiffZipper` for comparing values between the dictionaries.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::prelude::*;
    /// use libdictenstein::value_diff_zipper::ValueDiffZipper;
    /// use libdictenstein::double_array_trie_zipper::DoubleArrayTrieZipper;
    ///
    /// let dict1 = DoubleArrayTrie::from_terms_with_values(
    ///     vec![("key", 1usize)].into_iter()
    /// );
    /// let dict2 = DoubleArrayTrie::from_terms_with_values(
    ///     vec![("key", 2usize)].into_iter()
    /// );
    ///
    /// let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    /// let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);
    ///
    /// let diff = ValueDiffZipper::new(z1, z2);
    /// ```
    pub fn new(left: Z, right: Z) -> Self {
        Self {
            left: Some(left),
            right: Some(right),
            path: Vec::new(),
        }
    }

    /// Check if both dictionaries are active at the current position.
    pub fn both_active(&self) -> bool {
        self.left.is_some() && self.right.is_some()
    }

    /// Check if both dictionaries mark the current position as final.
    pub fn both_final(&self) -> bool {
        self.left.as_ref().is_some_and(|z| z.is_final())
            && self.right.as_ref().is_some_and(|z| z.is_final())
    }

    /// Get the value from the left dictionary at the current position.
    ///
    /// Returns `None` if left is not active or not final.
    pub fn left_value(&self) -> Option<Z::Value> {
        self.left
            .as_ref()
            .filter(|z| z.is_final())
            .and_then(|z| z.value())
    }

    /// Get the value from the right dictionary at the current position.
    ///
    /// Returns `None` if right is not active or not final.
    pub fn right_value(&self) -> Option<Z::Value> {
        self.right
            .as_ref()
            .filter(|z| z.is_final())
            .and_then(|z| z.value())
    }

    /// Get both values as a tuple if both dictionaries are final here.
    ///
    /// Returns `Some((left_value, right_value))` if both are final with values,
    /// `None` otherwise.
    pub fn values(&self) -> Option<(Z::Value, Z::Value)> {
        match (self.left_value(), self.right_value()) {
            (Some(l), Some(r)) => Some((l, r)),
            _ => None,
        }
    }

    /// Check if values differ at the current position.
    ///
    /// Returns `true` if both are final with different values, `false` otherwise.
    pub fn values_differ(&self) -> bool {
        match self.values() {
            Some((l, r)) => l != r,
            None => false,
        }
    }

    /// Create an iterator over all value differences.
    ///
    /// # Returns
    ///
    /// An iterator yielding `ValueDiff` structs for each term with differing values.
    pub fn iter_diffs(&self) -> ValueDiffIterator<Z> {
        ValueDiffIterator::new(self.clone())
    }
}

impl<Z: ValuedDictZipper> DictZipper for ValueDiffZipper<Z>
where
    Z::Value: PartialEq,
{
    type Unit = Z::Unit;

    fn is_final(&self) -> bool {
        // Final in value diff means: both are final AND values are DIFFERENT
        self.values_differ()
    }

    fn descend(&self, label: Self::Unit) -> Option<Self> {
        // Both must be able to descend (intersection semantics)
        let new_left = self.left.as_ref().and_then(|z| z.descend(label));
        let new_right = self.right.as_ref().and_then(|z| z.descend(label));

        // Require both to have the path
        if new_left.is_some() && new_right.is_some() {
            let mut new_path = self.path.clone();
            new_path.push(label);

            Some(Self {
                left: new_left,
                right: new_right,
                path: new_path,
            })
        } else {
            None
        }
    }

    fn children(&self) -> impl Iterator<Item = (Self::Unit, Self)> {
        // Only include children present in BOTH dictionaries
        let left_labels: HashSet<Z::Unit> = self
            .left
            .as_ref()
            .map(|z| z.children().map(|(label, _)| label).collect())
            .unwrap_or_default();

        let right_labels: HashSet<Z::Unit> = self
            .right
            .as_ref()
            .map(|z| z.children().map(|(label, _)| label).collect())
            .unwrap_or_default();

        // Intersection of children
        let common_labels: Vec<Z::Unit> = {
            let mut labels: Vec<_> = left_labels.intersection(&right_labels).copied().collect();
            labels.sort_by(|a, b| format!("{:?}", a).cmp(&format!("{:?}", b)));
            labels
        };

        let self_clone = self.clone();
        common_labels
            .into_iter()
            .filter_map(move |label| self_clone.descend(label).map(|child| (label, child)))
    }

    fn path(&self) -> Vec<Self::Unit> {
        self.path.clone()
    }
}

// Note: We don't implement ValuedDictZipper for ValueDiffZipper because
// the semantics are different - we want to return BOTH values, not a merged one.
// Use `values()`, `left_value()`, or `right_value()` instead.

// =============================================================================
// ValueDiffIterator
// =============================================================================

/// Iterator over all value differences between two dictionaries.
///
/// This iterator performs depth-first traversal through the intersection
/// of two dictionaries and yields `ValueDiff` structs for terms where
/// values differ.
///
/// # Type Parameters
///
/// * `Z` - The underlying valued zipper type
///
/// # Iterator Item
///
/// Returns `ValueDiff<Z::Unit, Z::Value>`:
/// - `path` - Complete path (term) as sequence of units
/// - `left_value` - Value from the left dictionary
/// - `right_value` - Value from the right dictionary
pub struct ValueDiffIterator<Z: ValuedDictZipper> {
    /// DFS traversal stack
    stack: Vec<ValueDiffZipper<Z>>,

    /// Paths already yielded (for deduplication)
    seen: HashSet<Vec<Z::Unit>>,
}

impl<Z: ValuedDictZipper> ValueDiffIterator<Z>
where
    Z::Value: PartialEq,
{
    /// Create a new iterator starting from the given value diff zipper.
    fn new(zipper: ValueDiffZipper<Z>) -> Self {
        let mut stack = Vec::with_capacity(16);
        stack.push(zipper);
        Self {
            stack,
            seen: HashSet::new(),
        }
    }
}

impl<Z: ValuedDictZipper> Iterator for ValueDiffIterator<Z>
where
    Z::Value: PartialEq,
{
    type Item = ValueDiff<Z::Unit, Z::Value>;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(zipper) = self.stack.pop() {
            // Push all children onto stack for continued DFS traversal
            for (_label, child) in zipper.children() {
                self.stack.push(child);
            }

            // If this is a value diff (both final, values differ) and unseen, yield it
            if zipper.is_final() {
                let path = zipper.path();
                if self.seen.insert(path.clone()) {
                    // We know values() will return Some because is_final() was true
                    if let Some((left_value, right_value)) = zipper.values() {
                        return Some(ValueDiff::new(path, left_value, right_value));
                    }
                }
            }
        }

        None
    }
}

// =============================================================================
// ValueDiffZipperExt Extension Trait
// =============================================================================

/// Extension trait for ergonomic value diff zipper creation.
///
/// This trait is automatically implemented for all `ValuedDictZipper` types
/// where values support equality comparison.
///
/// # Examples
///
/// ```rust
/// use libdictenstein::prelude::*;
/// use libdictenstein::value_diff_zipper::ValueDiffZipperExt;
/// use libdictenstein::double_array_trie_zipper::DoubleArrayTrieZipper;
///
/// let dict1 = DoubleArrayTrie::from_terms_with_values(
///     vec![("key", 1usize), ("other", 5)].into_iter()
/// );
/// let dict2 = DoubleArrayTrie::from_terms_with_values(
///     vec![("key", 2usize), ("other", 5)].into_iter()
/// );
///
/// let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
/// let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);
///
/// // Find terms with different values
/// let diff = z1.value_diff_with(z2);
/// ```
pub trait ValueDiffZipperExt: ValuedDictZipper + Sized
where
    Self::Value: PartialEq,
{
    /// Create a value diff zipper comparing this dictionary with another.
    ///
    /// Returns a zipper that identifies terms present in both dictionaries
    /// where the values differ.
    ///
    /// # Arguments
    ///
    /// * `other` - The dictionary to compare against
    ///
    /// # Returns
    ///
    /// A `ValueDiffZipper` for finding value differences.
    fn value_diff_with(self, other: Self) -> ValueDiffZipper<Self> {
        ValueDiffZipper::new(self, other)
    }

    /// Iterate over value differences directly.
    ///
    /// Convenience method equivalent to `self.value_diff_with(other).iter_diffs()`.
    ///
    /// # Arguments
    ///
    /// * `other` - The dictionary to compare against
    ///
    /// # Returns
    ///
    /// An iterator over `ValueDiff` structs.
    fn iter_value_diffs(self, other: Self) -> ValueDiffIterator<Self> {
        ValueDiffZipper::new(self, other).iter_diffs()
    }
}

/// Blanket implementation: all ValuedDictZippers with PartialEq values get ValueDiffZipperExt.
impl<Z: ValuedDictZipper> ValueDiffZipperExt for Z where Z::Value: PartialEq {}

// =============================================================================
// Unit Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::double_array_trie::DoubleArrayTrie;
    use crate::double_array_trie_zipper::DoubleArrayTrieZipper;

    fn sorted_diffs<U: CharUnit + Ord, V: DictionaryValue + Ord>(
        mut diffs: Vec<ValueDiff<U, V>>,
    ) -> Vec<ValueDiff<U, V>> {
        diffs.sort_by(|a, b| a.path.cmp(&b.path));
        diffs
    }

    #[test]
    fn test_value_diff_basic() {
        let dict1 = DoubleArrayTrie::from_terms_with_values(
            vec![("cat", 10usize), ("dog", 20), ("fish", 30)].into_iter(),
        );
        let dict2 = DoubleArrayTrie::from_terms_with_values(
            vec![("cat", 10usize), ("dog", 25), ("fish", 35)].into_iter(),
        );

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

        let diff = ValueDiffZipper::new(z1, z2);

        let results = sorted_diffs(diff.iter_diffs().collect());

        // "cat" has same value (10), excluded
        // "dog" changed from 20 to 25
        // "fish" changed from 30 to 35
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].path, b"dog".to_vec());
        assert_eq!(results[0].left_value, 20);
        assert_eq!(results[0].right_value, 25);
        assert_eq!(results[1].path, b"fish".to_vec());
        assert_eq!(results[1].left_value, 30);
        assert_eq!(results[1].right_value, 35);
    }

    #[test]
    fn test_value_diff_identical() {
        let dict1 = DoubleArrayTrie::from_terms_with_values(
            vec![("cat", 10usize), ("dog", 20)].into_iter(),
        );
        let dict2 = DoubleArrayTrie::from_terms_with_values(
            vec![("cat", 10usize), ("dog", 20)].into_iter(),
        );

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

        let diff = z1.value_diff_with(z2);

        let count = diff.iter_diffs().count();

        // All values are the same, no diffs
        assert_eq!(count, 0);
    }

    #[test]
    fn test_value_diff_all_different() {
        let dict1 = DoubleArrayTrie::from_terms_with_values(
            vec![("a", 1usize), ("b", 2), ("c", 3)].into_iter(),
        );
        let dict2 = DoubleArrayTrie::from_terms_with_values(
            vec![("a", 10usize), ("b", 20), ("c", 30)].into_iter(),
        );

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

        let diff = z1.value_diff_with(z2);

        let count = diff.iter_diffs().count();

        // All values differ
        assert_eq!(count, 3);
    }

    #[test]
    fn test_value_diff_disjoint_dicts() {
        let dict1 =
            DoubleArrayTrie::from_terms_with_values(vec![("cat", 1usize), ("dog", 2)].into_iter());
        let dict2 = DoubleArrayTrie::from_terms_with_values(
            vec![("fish", 3usize), ("bird", 4)].into_iter(),
        );

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

        let diff = z1.value_diff_with(z2);

        let count = diff.iter_diffs().count();

        // No common terms, no diffs
        assert_eq!(count, 0);
    }

    #[test]
    fn test_value_diff_partial_overlap() {
        let dict1 = DoubleArrayTrie::from_terms_with_values(
            vec![("cat", 1usize), ("dog", 2), ("fish", 3)].into_iter(),
        );
        let dict2 = DoubleArrayTrie::from_terms_with_values(
            vec![("dog", 20usize), ("bird", 4)].into_iter(),
        );

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

        let diff = z1.value_diff_with(z2);

        let results: Vec<_> = diff.iter_diffs().collect();

        // Only "dog" is in both, and values differ (2 vs 20)
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, b"dog".to_vec());
        assert_eq!(results[0].left_value, 2);
        assert_eq!(results[0].right_value, 20);
    }

    #[test]
    fn test_value_diff_descend() {
        let dict1 = DoubleArrayTrie::from_terms_with_values(
            vec![("score", 85u32), ("count", 100)].into_iter(),
        );
        let dict2 = DoubleArrayTrie::from_terms_with_values(
            vec![("score", 92u32), ("count", 100)].into_iter(),
        );

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

        let diff = z1.value_diff_with(z2);

        // Navigate to "score" - values differ
        let score = diff
            .descend(b's')
            .and_then(|z| z.descend(b'c'))
            .and_then(|z| z.descend(b'o'))
            .and_then(|z| z.descend(b'r'))
            .and_then(|z| z.descend(b'e'))
            .unwrap();

        assert!(score.is_final()); // Values differ
        assert_eq!(score.left_value(), Some(85));
        assert_eq!(score.right_value(), Some(92));
        assert_eq!(score.values(), Some((85, 92)));

        // Navigate to "count" - values are same
        let count = diff
            .descend(b'c')
            .and_then(|z| z.descend(b'o'))
            .and_then(|z| z.descend(b'u'))
            .and_then(|z| z.descend(b'n'))
            .and_then(|z| z.descend(b't'))
            .unwrap();

        assert!(!count.is_final()); // Values are same
        assert!(count.both_final()); // Both are final
        assert!(!count.values_differ()); // But values don't differ
    }

    #[test]
    fn test_value_diff_children() {
        let dict1 = DoubleArrayTrie::from_terms_with_values(
            vec![("ab", 1usize), ("ac", 2), ("ad", 3)].into_iter(),
        );
        let dict2 = DoubleArrayTrie::from_terms_with_values(
            vec![("ab", 10usize), ("ac", 2), ("ae", 5)].into_iter(),
        );

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

        let diff = z1.value_diff_with(z2);

        // Navigate to 'a'
        let a = diff.descend(b'a').unwrap();

        // Children should be intersection: b, c (d only in dict1, e only in dict2)
        let mut children: Vec<u8> = a.children().map(|(label, _)| label).collect();
        children.sort();

        assert_eq!(children, vec![b'b', b'c']);
    }

    #[test]
    fn test_value_diff_path_string() {
        let dict1 = DoubleArrayTrie::from_terms_with_values(vec![("hello", 1usize)].into_iter());
        let dict2 = DoubleArrayTrie::from_terms_with_values(vec![("hello", 2usize)].into_iter());

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

        let diff = z1.value_diff_with(z2);

        let results: Vec<_> = diff.iter_diffs().collect();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path_string(), "hello");
    }

    #[test]
    fn test_value_diff_extension_trait() {
        let dict1 = DoubleArrayTrie::from_terms_with_values(vec![("key", 1usize)].into_iter());
        let dict2 = DoubleArrayTrie::from_terms_with_values(vec![("key", 2usize)].into_iter());

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

        // Using iter_value_diffs directly
        let results: Vec<_> = z1.iter_value_diffs(z2).collect();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].left_value, 1);
        assert_eq!(results[0].right_value, 2);
    }

    #[test]
    fn test_value_diff_with_strings() {
        let dict1 =
            DoubleArrayTrie::from_terms_with_values(vec![("key", "old".to_string())].into_iter());
        let dict2 =
            DoubleArrayTrie::from_terms_with_values(vec![("key", "new".to_string())].into_iter());

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

        let diff = z1.value_diff_with(z2);

        let results: Vec<_> = diff.iter_diffs().collect();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].left_value, "old".to_string());
        assert_eq!(results[0].right_value, "new".to_string());
    }

    #[test]
    fn test_value_diff_nested_prefixes() {
        let dict1 = DoubleArrayTrie::from_terms_with_values(
            vec![("app", 1usize), ("apple", 2), ("application", 3)].into_iter(),
        );
        let dict2 = DoubleArrayTrie::from_terms_with_values(
            vec![("app", 1usize), ("apple", 20), ("application", 3)].into_iter(),
        );

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

        let diff = z1.value_diff_with(z2);

        let results: Vec<_> = diff.iter_diffs().collect();

        // Only "apple" differs (2 vs 20)
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, b"apple".to_vec());
        assert_eq!(results[0].left_value, 2);
        assert_eq!(results[0].right_value, 20);
    }

    #[test]
    fn test_both_active_and_final() {
        let dict1 = DoubleArrayTrie::from_terms_with_values(vec![("cat", 1usize)].into_iter());
        let dict2 = DoubleArrayTrie::from_terms_with_values(vec![("cat", 2usize)].into_iter());

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

        let diff = z1.value_diff_with(z2);

        assert!(diff.both_active());

        let cat = diff
            .descend(b'c')
            .and_then(|z| z.descend(b'a'))
            .and_then(|z| z.descend(b't'))
            .unwrap();

        assert!(cat.both_active());
        assert!(cat.both_final());
        assert!(cat.values_differ());
    }
}
