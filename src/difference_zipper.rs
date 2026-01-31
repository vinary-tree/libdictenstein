//! Difference zipper for set-theoretic dictionary operations.
//!
//! This module provides a `DifferenceZipper` that computes the set difference A \ B,
//! yielding terms that exist in the first dictionary but NOT in the second.
//!
//! # Use Cases
//!
//! - **Vocabulary delta**: Find new terms added to a corpus
//! - **Stopword removal**: Remove common words from a term list
//! - **Differential updates**: Identify terms unique to one version
//!
//! # Examples
//!
//! ## Basic Difference of Two Dictionaries
//!
//! ```rust
//! use libdictenstein::prelude::*;
//! use libdictenstein::difference_zipper::{DifferenceZipper, DifferenceZipperExt};
//! use libdictenstein::double_array_trie_zipper::DoubleArrayTrieZipper;
//!
//! let dict_a = DoubleArrayTrie::from_terms(vec!["cat", "dog", "fish"].iter());
//! let dict_b = DoubleArrayTrie::from_terms(vec!["dog", "bird"].iter());
//!
//! let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
//! let z_b = DoubleArrayTrieZipper::new_from_dict(&dict_b);
//!
//! // Create difference: A \ B
//! let difference = z_a.difference_from(z_b);
//!
//! // Iterate terms in A but NOT in B
//! let mut results: Vec<String> = difference.iter()
//!     .map(|(path, _)| String::from_utf8(path).unwrap())
//!     .collect();
//! results.sort();
//! assert_eq!(results, vec!["cat", "fish"]); // "dog" is excluded (in B)
//! ```
//!
//! ## Stopword Removal Pattern
//!
//! ```rust
//! use libdictenstein::prelude::*;
//! use libdictenstein::difference_zipper::DifferenceZipperExt;
//! use libdictenstein::double_array_trie_zipper::DoubleArrayTrieZipper;
//!
//! let vocabulary = DoubleArrayTrie::from_terms(vec!["the", "cat", "sat", "on", "a", "mat"].iter());
//! let stopwords = DoubleArrayTrie::from_terms(vec!["the", "on", "a"].iter());
//!
//! let vocab_z = DoubleArrayTrieZipper::new_from_dict(&vocabulary);
//! let stop_z = DoubleArrayTrieZipper::new_from_dict(&stopwords);
//!
//! // Remove stopwords
//! let filtered = vocab_z.difference_from(stop_z);
//!
//! let mut results: Vec<String> = filtered.iter()
//!     .map(|(path, _)| String::from_utf8(path).unwrap())
//!     .collect();
//! results.sort();
//! assert_eq!(results, vec!["cat", "mat", "sat"]);
//! ```
//!
//! ## Navigation Semantics
//!
//! ```rust
//! use libdictenstein::prelude::*;
//! use libdictenstein::difference_zipper::DifferenceZipperExt;
//! use libdictenstein::double_array_trie_zipper::DoubleArrayTrieZipper;
//! use libdictenstein::zipper::DictZipper;
//!
//! let dict_a = DoubleArrayTrie::from_terms(vec!["cat", "car"].iter());
//! let dict_b = DoubleArrayTrie::from_terms(vec!["cat"].iter());
//!
//! let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
//! let z_b = DoubleArrayTrieZipper::new_from_dict(&dict_b);
//!
//! let difference = z_a.difference_from(z_b);
//!
//! // "cat" is in both, so NOT in difference
//! let cat = difference.descend(b'c')
//!     .and_then(|z| z.descend(b'a'))
//!     .and_then(|z| z.descend(b't'));
//! assert!(cat.is_some());
//! assert!(!cat.unwrap().is_final()); // Not final because it's in B
//!
//! // "car" is only in A, so IS in difference
//! let car = difference.descend(b'c')
//!     .and_then(|z| z.descend(b'a'))
//!     .and_then(|z| z.descend(b'r'));
//! assert!(car.is_some());
//! assert!(car.unwrap().is_final()); // Final because it's in A but not B
//! ```
//!
//! # Performance
//!
//! - **Navigation**: O(k) where k = prefix length (parallel descend in both zippers)
//! - **Children collection**: O(c) where c = children in A (only A's children matter)
//! - **Iteration**: O(m) where m = terms in A (must check each against B)
//! - **Memory**: O(2) for zipper storage + O(d) stack depth during iteration
//!
//! # Backend Compatibility
//!
//! Works uniformly across all dictionary backends via the `DictZipper` trait.

use std::collections::HashSet;

use crate::zipper::{DictZipper, ValuedDictZipper};

// =============================================================================
// DifferenceZipper
// =============================================================================

/// A zipper that computes the set difference A \ B.
///
/// `DifferenceZipper` yields terms that exist in the left dictionary (A) but NOT
/// in the right dictionary (B).
///
/// # Type Parameters
///
/// * `Z` - The underlying zipper type (must implement `DictZipper`)
///
/// # Navigation
///
/// Navigation through the difference considers both dictionaries:
/// - `is_final()` returns true if A is final AND B is NOT final
/// - `descend(label)` succeeds if A can descend (we must traverse A's structure)
/// - `children()` returns A's children (B is only used for exclusion checking)
///
/// # Value Semantics
///
/// Since the difference only contains terms from A (that aren't in B), values
/// come directly from A with no merging required.
#[derive(Clone, Debug)]
pub struct DifferenceZipper<Z: DictZipper> {
    /// The left zipper (A) - source of terms
    left: Option<Z>,

    /// The right zipper (B) - exclusion set
    right: Option<Z>,

    /// Path from root to current position
    path: Vec<Z::Unit>,
}

impl<Z: DictZipper> DifferenceZipper<Z> {
    /// Create a new difference zipper computing A \ B.
    ///
    /// # Arguments
    ///
    /// * `left` - The left dictionary (A) - terms will come from here
    /// * `right` - The right dictionary (B) - terms to exclude
    ///
    /// # Returns
    ///
    /// A new `DifferenceZipper` computing A \ B.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::prelude::*;
    /// use libdictenstein::difference_zipper::DifferenceZipper;
    /// use libdictenstein::double_array_trie_zipper::DoubleArrayTrieZipper;
    ///
    /// let dict_a = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());
    /// let dict_b = DoubleArrayTrie::from_terms(vec!["dog"].iter());
    ///
    /// let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
    /// let z_b = DoubleArrayTrieZipper::new_from_dict(&dict_b);
    ///
    /// let difference = DifferenceZipper::new(z_a, z_b);
    /// // difference contains "cat" (in A, not in B)
    /// ```
    pub fn new(left: Z, right: Z) -> Self {
        Self {
            left: Some(left),
            right: Some(right),
            path: Vec::new(),
        }
    }

    /// Create a difference zipper with a potentially empty right side.
    ///
    /// This is useful when the exclusion dictionary may not have any terms.
    ///
    /// # Arguments
    ///
    /// * `left` - The left dictionary (A) - terms will come from here
    /// * `right` - Optional right dictionary (B) - if None, difference equals A
    pub fn new_with_optional_right(left: Z, right: Option<Z>) -> Self {
        Self {
            left: Some(left),
            right,
            path: Vec::new(),
        }
    }

    /// Check if the left (A) dictionary is active at the current position.
    pub fn left_is_active(&self) -> bool {
        self.left.is_some()
    }

    /// Check if the right (B) dictionary is active at the current position.
    pub fn right_is_active(&self) -> bool {
        self.right.is_some()
    }

    /// Create an iterator over all terms in the difference.
    ///
    /// # Returns
    ///
    /// An iterator yielding `(path, zipper)` pairs for each term in A \ B.
    pub fn iter(&self) -> DifferenceIterator<Z> {
        DifferenceIterator::new(self.clone())
    }
}

impl<Z: DictZipper> DictZipper for DifferenceZipper<Z> {
    type Unit = Z::Unit;

    fn is_final(&self) -> bool {
        // Final in A \ B means: A is final AND B is NOT final
        let left_final = self.left.as_ref().is_some_and(|z| z.is_final());
        let right_final = self.right.as_ref().is_some_and(|z| z.is_final());

        left_final && !right_final
    }

    fn descend(&self, label: Self::Unit) -> Option<Self> {
        // We must follow A's structure. B follows along for exclusion checking.
        let new_left = self.left.as_ref().and_then(|z| z.descend(label));

        // If A can't descend, there's nothing to include in the difference
        if new_left.is_none() {
            return None;
        }

        // B descends if it can (to track parallel position for exclusion)
        let new_right = self.right.as_ref().and_then(|z| z.descend(label));

        let mut new_path = self.path.clone();
        new_path.push(label);

        Some(Self {
            left: new_left,
            right: new_right,
            path: new_path,
        })
    }

    fn children(&self) -> impl Iterator<Item = (Self::Unit, Self)> {
        // Children come from A. We descend into each, carrying B along for exclusion.
        let left_children: Vec<(Z::Unit, Z)> = self
            .left
            .as_ref()
            .map(|z| z.children().collect())
            .unwrap_or_default();

        let self_clone = self.clone();
        left_children
            .into_iter()
            .filter_map(move |(label, _)| self_clone.descend(label).map(|child| (label, child)))
    }

    fn path(&self) -> Vec<Self::Unit> {
        self.path.clone()
    }
}

impl<Z: ValuedDictZipper> ValuedDictZipper for DifferenceZipper<Z> {
    type Value = Z::Value;

    fn value(&self) -> Option<Self::Value> {
        // Value comes from A (left) only, and only if this is a valid difference term
        if self.is_final() {
            self.left.as_ref().and_then(|z| z.value())
        } else {
            None
        }
    }
}

// =============================================================================
// DifferenceIterator
// =============================================================================

/// Iterator over all terms in a set difference A \ B.
///
/// This iterator performs depth-first traversal of A and yields terms that
/// exist in A but NOT in B.
///
/// # Type Parameters
///
/// * `Z` - The underlying zipper type
///
/// # Iterator Item
///
/// Returns `(Vec<Z::Unit>, DifferenceZipper<Z>)`:
/// - `Vec<Z::Unit>` - Complete path (term) as sequence of units
/// - `DifferenceZipper<Z>` - Zipper positioned at the final node
pub struct DifferenceIterator<Z: DictZipper> {
    /// DFS traversal stack
    stack: Vec<DifferenceZipper<Z>>,

    /// Paths already yielded (for deduplication)
    seen: HashSet<Vec<Z::Unit>>,
}

impl<Z: DictZipper> DifferenceIterator<Z> {
    /// Create a new iterator starting from the given difference zipper.
    fn new(zipper: DifferenceZipper<Z>) -> Self {
        let mut stack = Vec::with_capacity(16);
        stack.push(zipper);
        Self {
            stack,
            seen: HashSet::new(),
        }
    }
}

impl<Z: DictZipper> Iterator for DifferenceIterator<Z> {
    type Item = (Vec<Z::Unit>, DifferenceZipper<Z>);

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(zipper) = self.stack.pop() {
            // Push all children onto stack for continued DFS traversal
            for (_label, child) in zipper.children() {
                self.stack.push(child);
            }

            // If this is a difference term (in A, not in B) and we haven't seen it, yield it
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
// ValuedDifferenceIterator
// =============================================================================

/// Iterator over (term, value) pairs in a set difference.
///
/// # Type Parameters
///
/// * `Z` - The underlying valued zipper type
///
/// # Iterator Item
///
/// Returns `(Vec<Z::Unit>, Z::Value)`:
/// - `Vec<Z::Unit>` - Complete path (term) as sequence of units
/// - `Z::Value` - Value from the left dictionary (A)
pub struct ValuedDifferenceIterator<Z: ValuedDictZipper> {
    inner: DifferenceIterator<Z>,
}

impl<Z: ValuedDictZipper> ValuedDifferenceIterator<Z> {
    /// Create a new valued iterator from a difference zipper.
    pub fn new(zipper: DifferenceZipper<Z>) -> Self {
        Self {
            inner: DifferenceIterator::new(zipper),
        }
    }
}

impl<Z: ValuedDictZipper> Iterator for ValuedDifferenceIterator<Z> {
    type Item = (Vec<Z::Unit>, Z::Value);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let (path, zipper) = self.inner.next()?;
            if let Some(value) = zipper.value() {
                return Some((path, value));
            }
            // Continue if no value
        }
    }
}

// =============================================================================
// DifferenceZipperExt Extension Trait
// =============================================================================

/// Extension trait for ergonomic difference zipper creation.
///
/// This trait is automatically implemented for all `DictZipper` types, providing
/// convenient methods to create difference zippers.
///
/// # Examples
///
/// ```rust
/// use libdictenstein::prelude::*;
/// use libdictenstein::difference_zipper::DifferenceZipperExt;
/// use libdictenstein::double_array_trie_zipper::DoubleArrayTrieZipper;
///
/// let dict_a = DoubleArrayTrie::from_terms(vec!["cat", "dog", "fish"].iter());
/// let dict_b = DoubleArrayTrie::from_terms(vec!["dog"].iter());
///
/// let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
/// let z_b = DoubleArrayTrieZipper::new_from_dict(&dict_b);
///
/// // Compute A \ B
/// let difference = z_a.difference_from(z_b);
/// ```
pub trait DifferenceZipperExt: DictZipper + Sized {
    /// Create a difference zipper computing self \ other.
    ///
    /// Returns a zipper yielding terms in `self` that are NOT in `other`.
    ///
    /// # Arguments
    ///
    /// * `other` - The dictionary to subtract
    ///
    /// # Returns
    ///
    /// A `DifferenceZipper` computing self \ other.
    fn difference_from(self, other: Self) -> DifferenceZipper<Self> {
        DifferenceZipper::new(self, other)
    }

    /// Create a difference zipper with optional exclusion.
    ///
    /// If `other` is `None`, returns a zipper equivalent to `self`.
    ///
    /// # Arguments
    ///
    /// * `other` - Optional dictionary to subtract
    ///
    /// # Returns
    ///
    /// A `DifferenceZipper` computing self \ other (or self if other is None).
    fn difference_from_optional(self, other: Option<Self>) -> DifferenceZipper<Self> {
        DifferenceZipper::new_with_optional_right(self, other)
    }
}

/// Blanket implementation: all DictZippers automatically get DifferenceZipperExt support.
impl<Z: DictZipper> DifferenceZipperExt for Z {}

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
    fn test_difference_basic() {
        let dict_a = DoubleArrayTrie::from_terms(vec!["cat", "dog", "fish"].iter());
        let dict_b = DoubleArrayTrie::from_terms(vec!["dog", "bird"].iter());

        let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
        let z_b = DoubleArrayTrieZipper::new_from_dict(&dict_b);

        let difference = DifferenceZipper::new(z_a, z_b);

        let results: Vec<String> = sorted_strings(
            difference
                .iter()
                .map(|(path, _)| String::from_utf8(path).unwrap())
                .collect(),
        );

        // "cat" and "fish" are in A but not B; "dog" is in both so excluded
        assert_eq!(results, vec!["cat", "fish"]);
    }

    #[test]
    fn test_difference_a_minus_empty() {
        let dict_a = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());
        let dict_b: DoubleArrayTrie = DoubleArrayTrie::new();

        let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
        let z_b = DoubleArrayTrieZipper::new_from_dict(&dict_b);

        let difference = z_a.difference_from(z_b);

        let results: Vec<String> = sorted_strings(
            difference
                .iter()
                .map(|(path, _)| String::from_utf8(path).unwrap())
                .collect(),
        );

        // A \ ∅ = A
        assert_eq!(results, vec!["cat", "dog"]);
    }

    #[test]
    fn test_difference_empty_minus_b() {
        let dict_a: DoubleArrayTrie = DoubleArrayTrie::new();
        let dict_b = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());

        let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
        let z_b = DoubleArrayTrieZipper::new_from_dict(&dict_b);

        let difference = z_a.difference_from(z_b);

        let count = difference.iter().count();

        // ∅ \ B = ∅
        assert_eq!(count, 0);
    }

    #[test]
    fn test_difference_a_minus_a() {
        let dict_a = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());
        let dict_b = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());

        let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
        let z_b = DoubleArrayTrieZipper::new_from_dict(&dict_b);

        let difference = z_a.difference_from(z_b);

        let count = difference.iter().count();

        // A \ A = ∅
        assert_eq!(count, 0);
    }

    #[test]
    fn test_difference_disjoint() {
        let dict_a = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());
        let dict_b = DoubleArrayTrie::from_terms(vec!["fish", "bird"].iter());

        let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
        let z_b = DoubleArrayTrieZipper::new_from_dict(&dict_b);

        let difference = z_a.difference_from(z_b);

        let results: Vec<String> = sorted_strings(
            difference
                .iter()
                .map(|(path, _)| String::from_utf8(path).unwrap())
                .collect(),
        );

        // Disjoint sets: A \ B = A
        assert_eq!(results, vec!["cat", "dog"]);
    }

    #[test]
    fn test_difference_b_superset_of_a() {
        let dict_a = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());
        let dict_b = DoubleArrayTrie::from_terms(vec!["cat", "dog", "fish", "bird"].iter());

        let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
        let z_b = DoubleArrayTrieZipper::new_from_dict(&dict_b);

        let difference = z_a.difference_from(z_b);

        let count = difference.iter().count();

        // B ⊇ A → A \ B = ∅
        assert_eq!(count, 0);
    }

    #[test]
    fn test_difference_descend() {
        let dict_a = DoubleArrayTrie::from_terms(vec!["cat", "car", "cab"].iter());
        let dict_b = DoubleArrayTrie::from_terms(vec!["cat", "can"].iter());

        let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
        let z_b = DoubleArrayTrieZipper::new_from_dict(&dict_b);

        let difference = z_a.difference_from(z_b);

        // Navigate to "cat" - in both, so NOT in difference
        let cat = difference
            .descend(b'c')
            .and_then(|z| z.descend(b'a'))
            .and_then(|z| z.descend(b't'));
        assert!(cat.is_some());
        assert!(!cat.unwrap().is_final()); // In B, so excluded

        // Navigate to "car" - only in A, so IS in difference
        let car = difference
            .descend(b'c')
            .and_then(|z| z.descend(b'a'))
            .and_then(|z| z.descend(b'r'));
        assert!(car.is_some());
        assert!(car.unwrap().is_final()); // Not in B, so included

        // Navigate to "cab" - only in A, so IS in difference
        let cab = difference
            .descend(b'c')
            .and_then(|z| z.descend(b'a'))
            .and_then(|z| z.descend(b'b'));
        assert!(cab.is_some());
        assert!(cab.unwrap().is_final());
    }

    #[test]
    fn test_difference_children() {
        let dict_a = DoubleArrayTrie::from_terms(vec!["ab", "ac", "ad"].iter());
        let dict_b = DoubleArrayTrie::from_terms(vec!["ac"].iter());

        let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
        let z_b = DoubleArrayTrieZipper::new_from_dict(&dict_b);

        let difference = z_a.difference_from(z_b);

        // Navigate to 'a'
        let a = difference.descend(b'a').unwrap();

        // Children come from A (all of b, c, d)
        let mut children: Vec<u8> = a.children().map(|(label, _)| label).collect();
        children.sort();

        assert_eq!(children, vec![b'b', b'c', b'd']);
    }

    #[test]
    fn test_difference_with_values() {
        let dict_a = DoubleArrayTrie::from_terms_with_values(
            vec![("cat", 1usize), ("dog", 2), ("fish", 3)].into_iter(),
        );
        let dict_b = DoubleArrayTrie::from_terms_with_values(
            vec![("dog", 0usize)].into_iter(),
        );

        let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
        let z_b = DoubleArrayTrieZipper::new_from_dict(&dict_b);

        let difference = z_a.difference_from(z_b);

        // Navigate to "cat" - should have value from A
        let cat = difference
            .descend(b'c')
            .and_then(|z| z.descend(b'a'))
            .and_then(|z| z.descend(b't'))
            .unwrap();

        assert!(cat.is_final());
        assert_eq!(cat.value(), Some(1));

        // Navigate to "dog" - should not be final (excluded by B)
        let dog = difference
            .descend(b'd')
            .and_then(|z| z.descend(b'o'))
            .and_then(|z| z.descend(b'g'))
            .unwrap();

        assert!(!dog.is_final());
        assert_eq!(dog.value(), None); // Not final, so no value
    }

    #[test]
    fn test_difference_valued_iterator() {
        let dict_a = DoubleArrayTrie::from_terms_with_values(
            vec![("cat", 1usize), ("dog", 2), ("fish", 3)].into_iter(),
        );
        let dict_b = DoubleArrayTrie::from_terms_with_values(
            vec![("dog", 0usize)].into_iter(),
        );

        let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
        let z_b = DoubleArrayTrieZipper::new_from_dict(&dict_b);

        let difference = z_a.difference_from(z_b);
        let valued_iter = ValuedDifferenceIterator::new(difference);

        let mut results: Vec<(String, usize)> = valued_iter
            .map(|(path, val)| (String::from_utf8(path).unwrap(), val))
            .collect();

        results.sort_by(|a, b| a.0.cmp(&b.0));

        assert_eq!(
            results,
            vec![("cat".to_string(), 1), ("fish".to_string(), 3),]
        );
    }

    #[test]
    fn test_difference_nested_prefix() {
        // Test case: A has "app" and "apple", B has "apple"
        // Result should include "app" but not "apple"
        let dict_a = DoubleArrayTrie::from_terms(vec!["app", "apple"].iter());
        let dict_b = DoubleArrayTrie::from_terms(vec!["apple"].iter());

        let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
        let z_b = DoubleArrayTrieZipper::new_from_dict(&dict_b);

        let difference = z_a.difference_from(z_b);

        let results: Vec<String> = sorted_strings(
            difference
                .iter()
                .map(|(path, _)| String::from_utf8(path).unwrap())
                .collect(),
        );

        // "app" is in A but not in B (B only has "apple")
        assert_eq!(results, vec!["app"]);
    }

    #[test]
    fn test_difference_optional_right() {
        let dict_a = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());

        let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);

        // No exclusion set
        let difference = z_a.difference_from_optional(None);

        let results: Vec<String> = sorted_strings(
            difference
                .iter()
                .map(|(path, _)| String::from_utf8(path).unwrap())
                .collect(),
        );

        // With no exclusion, difference equals A
        assert_eq!(results, vec!["cat", "dog"]);
    }

    #[test]
    fn test_left_right_active() {
        let dict_a = DoubleArrayTrie::from_terms(vec!["cat", "car"].iter());
        let dict_b = DoubleArrayTrie::from_terms(vec!["cat"].iter());

        let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
        let z_b = DoubleArrayTrieZipper::new_from_dict(&dict_b);

        let difference = z_a.difference_from(z_b);

        assert!(difference.left_is_active());
        assert!(difference.right_is_active());

        // Navigate to "car" - B doesn't have this path
        let car = difference
            .descend(b'c')
            .and_then(|z| z.descend(b'a'))
            .and_then(|z| z.descend(b'r'))
            .unwrap();

        assert!(car.left_is_active());
        assert!(!car.right_is_active()); // B can't descend to 'r'
    }
}
