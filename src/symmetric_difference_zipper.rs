//! Symmetric difference zipper for set-theoretic dictionary operations.
//!
//! This module provides a `SymmetricDifferenceZipper` that computes A Δ B,
//! yielding terms that exist in EXACTLY ONE of the dictionaries (the XOR of sets).
//!
//! # Mathematical Definition
//!
//! A Δ B = (A \ B) ∪ (B \ A) = (A ∪ B) \ (A ∩ B)
//!
//! # Use Cases
//!
//! - **Version diff**: Find terms that changed between versions (added or removed)
//! - **Vocabulary comparison**: Identify terms unique to each corpus
//! - **Change detection**: Detect additions and deletions in incremental updates
//!
//! # Examples
//!
//! ## Basic Symmetric Difference
//!
//! ```rust
//! use libdictenstein::prelude::*;
//! use libdictenstein::symmetric_difference_zipper::{SymmetricDifferenceZipper, SymmetricDifferenceZipperExt};
//! use libdictenstein::double_array_trie_zipper::DoubleArrayTrieZipper;
//!
//! let dict_a = DoubleArrayTrie::from_terms(vec!["cat", "dog", "fish"].iter());
//! let dict_b = DoubleArrayTrie::from_terms(vec!["dog", "fish", "bird"].iter());
//!
//! let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
//! let z_b = DoubleArrayTrieZipper::new_from_dict(&dict_b);
//!
//! // Create symmetric difference: A Δ B
//! let sym_diff = z_a.symmetric_difference_with(z_b);
//!
//! // Iterate terms in exactly one dictionary
//! let mut results: Vec<String> = sym_diff.iter()
//!     .map(|(path, _)| String::from_utf8(path).unwrap())
//!     .collect();
//! results.sort();
//! assert_eq!(results, vec!["bird", "cat"]); // "dog" and "fish" are in BOTH, excluded
//! ```
//!
//! ## Version Diff Pattern
//!
//! ```rust
//! use libdictenstein::prelude::*;
//! use libdictenstein::symmetric_difference_zipper::SymmetricDifferenceZipperExt;
//! use libdictenstein::double_array_trie_zipper::DoubleArrayTrieZipper;
//!
//! // Vocabulary at two different versions
//! let version_1 = DoubleArrayTrie::from_terms(vec!["print", "input", "read"].iter());
//! let version_2 = DoubleArrayTrie::from_terms(vec!["print", "input", "write"].iter());
//!
//! let z1 = DoubleArrayTrieZipper::new_from_dict(&version_1);
//! let z2 = DoubleArrayTrieZipper::new_from_dict(&version_2);
//!
//! // Find changed terms (added + removed)
//! let changed = z1.symmetric_difference_with(z2);
//!
//! let mut results: Vec<String> = changed.iter()
//!     .map(|(path, _)| String::from_utf8(path).unwrap())
//!     .collect();
//! results.sort();
//! assert_eq!(results, vec!["read", "write"]); // "read" removed, "write" added
//! ```
//!
//! ## Multi-Dictionary Symmetric Difference
//!
//! ```rust
//! use libdictenstein::prelude::*;
//! use libdictenstein::symmetric_difference_zipper::SymmetricDifferenceZipperExt;
//! use libdictenstein::double_array_trie_zipper::DoubleArrayTrieZipper;
//!
//! let dict1 = DoubleArrayTrie::from_terms(vec!["a", "b", "c"].iter());
//! let dict2 = DoubleArrayTrie::from_terms(vec!["b", "c", "d"].iter());
//! let dict3 = DoubleArrayTrie::from_terms(vec!["c", "d", "e"].iter());
//!
//! let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
//! let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);
//! let z3 = DoubleArrayTrieZipper::new_from_dict(&dict3);
//!
//! // Terms in exactly one dictionary
//! let sym_diff = z1.symmetric_difference_all(vec![z2, z3]);
//!
//! let mut results: Vec<String> = sym_diff.iter()
//!     .map(|(path, _)| String::from_utf8(path).unwrap())
//!     .collect();
//! results.sort();
//! // "a" only in dict1, "e" only in dict3; "b" in 1,2; "c" in all 3; "d" in 2,3
//! assert_eq!(results, vec!["a", "e"]);
//! ```
//!
//! # Performance
//!
//! - **Navigation**: O(k × n) where k = prefix length, n = number of dictionaries
//! - **Children collection**: O(c × n) where c = max children per node
//! - **Iteration**: O(m × n) where m = total unique terms
//! - **Memory**: O(n) for zipper storage + O(d) stack depth during iteration
//!
//! # Backend Compatibility
//!
//! Works uniformly across all dictionary backends via the `DictZipper` trait.

use std::collections::HashSet;

use crate::zipper::{DictZipper, ValuedDictZipper};

// =============================================================================
// SymmetricDifferenceZipper
// =============================================================================

/// A zipper that computes the symmetric difference A Δ B (or multi-way XOR).
///
/// `SymmetricDifferenceZipper` yields terms that exist in EXACTLY ONE of the
/// underlying dictionaries.
///
/// # Type Parameters
///
/// * `Z` - The underlying zipper type (must implement `DictZipper`)
///
/// # Navigation
///
/// Navigation through the symmetric difference considers all dictionaries:
/// - `is_final()` returns true if EXACTLY ONE dictionary marks the position as final
/// - `descend(label)` succeeds if ANY dictionary has the path
/// - `children()` returns the union of children from all dictionaries
///
/// # Value Semantics
///
/// Since symmetric difference terms exist in exactly one dictionary, the value
/// comes from that single source with no merging required.
#[derive(Clone, Debug)]
pub struct SymmetricDifferenceZipper<Z: DictZipper> {
    /// The underlying zippers. `None` entries indicate dictionaries that don't
    /// have the current path.
    zippers: Vec<Option<Z>>,

    /// Path from root to current position.
    path: Vec<Z::Unit>,
}

impl<Z: DictZipper> SymmetricDifferenceZipper<Z> {
    /// Create a new symmetric difference zipper from multiple dictionaries.
    ///
    /// # Arguments
    ///
    /// * `zippers` - Zippers to compute symmetric difference over
    ///
    /// # Returns
    ///
    /// A new `SymmetricDifferenceZipper` yielding terms in exactly one dictionary.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::prelude::*;
    /// use libdictenstein::symmetric_difference_zipper::SymmetricDifferenceZipper;
    /// use libdictenstein::double_array_trie_zipper::DoubleArrayTrieZipper;
    ///
    /// let dict1 = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());
    /// let dict2 = DoubleArrayTrie::from_terms(vec!["dog", "fish"].iter());
    ///
    /// let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    /// let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);
    ///
    /// let sym_diff = SymmetricDifferenceZipper::new(vec![z1, z2]);
    /// // sym_diff contains "cat" and "fish" (terms in exactly one dict)
    /// ```
    pub fn new(zippers: Vec<Z>) -> Self {
        Self {
            zippers: zippers.into_iter().map(Some).collect(),
            path: Vec::new(),
        }
    }

    /// Get the number of underlying dictionaries.
    pub fn dictionary_count(&self) -> usize {
        self.zippers.len()
    }

    /// Get the number of active dictionaries at the current position.
    pub fn active_dictionary_count(&self) -> usize {
        self.zippers.iter().filter(|z| z.is_some()).count()
    }

    /// Get the number of dictionaries where the current position is final.
    ///
    /// For a valid symmetric difference term, this should be exactly 1.
    pub fn final_count(&self) -> usize {
        self.zippers
            .iter()
            .filter_map(|z| z.as_ref())
            .filter(|z| z.is_final())
            .count()
    }

    /// Get the index of the single dictionary containing this term.
    ///
    /// Returns `Some(index)` if exactly one dictionary has this as a final term,
    /// `None` otherwise.
    pub fn unique_source_index(&self) -> Option<usize> {
        if self.final_count() != 1 {
            return None;
        }

        self.zippers
            .iter()
            .enumerate()
            .find(|(_, z)| z.as_ref().is_some_and(|z| z.is_final()))
            .map(|(i, _)| i)
    }

    /// Create an iterator over all terms in the symmetric difference.
    ///
    /// # Returns
    ///
    /// An iterator yielding `(path, zipper)` pairs for each unique term.
    pub fn iter(&self) -> SymmetricDifferenceIterator<Z> {
        SymmetricDifferenceIterator::new(self.clone())
    }
}

impl<Z: DictZipper> DictZipper for SymmetricDifferenceZipper<Z> {
    type Unit = Z::Unit;

    fn is_final(&self) -> bool {
        // Final in symmetric difference means EXACTLY ONE dictionary marks this as final
        self.final_count() == 1
    }

    fn descend(&self, label: Self::Unit) -> Option<Self> {
        // Descend in all active zippers (like union)
        let new_zippers: Vec<Option<Z>> = self
            .zippers
            .iter()
            .map(|z| z.as_ref().and_then(|z| z.descend(label)))
            .collect();

        // Return new zipper if at least one can descend
        if new_zippers.iter().any(|z| z.is_some()) {
            let mut new_path = self.path.clone();
            new_path.push(label);

            Some(Self {
                zippers: new_zippers,
                path: new_path,
            })
        } else {
            None
        }
    }

    fn children(&self) -> impl Iterator<Item = (Self::Unit, Self)> {
        // Collect unique labels from all active zippers (union of children)
        let mut labels: Vec<Z::Unit> = self
            .zippers
            .iter()
            .filter_map(|z| z.as_ref())
            .flat_map(|z| z.children().map(|(label, _)| label))
            .collect();

        // Remove duplicates and sort for deterministic ordering
        labels.sort_by(|a, b| format!("{:?}", a).cmp(&format!("{:?}", b)));
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

impl<Z: ValuedDictZipper> ValuedDictZipper for SymmetricDifferenceZipper<Z> {
    type Value = Z::Value;

    fn value(&self) -> Option<Self::Value> {
        // Value comes from the single dictionary containing this term
        if !self.is_final() {
            return None;
        }

        // Find the one dictionary that has this as final and return its value
        self.zippers
            .iter()
            .filter_map(|z| z.as_ref())
            .find(|z| z.is_final())
            .and_then(|z| z.value())
    }
}

// =============================================================================
// SymmetricDifferenceIterator
// =============================================================================

/// Iterator over all terms in a symmetric difference.
///
/// This iterator performs depth-first traversal and yields terms that exist
/// in exactly one underlying dictionary.
///
/// # Type Parameters
///
/// * `Z` - The underlying zipper type
///
/// # Iterator Item
///
/// Returns `(Vec<Z::Unit>, SymmetricDifferenceZipper<Z>)`:
/// - `Vec<Z::Unit>` - Complete path (term) as sequence of units
/// - `SymmetricDifferenceZipper<Z>` - Zipper positioned at the final node
pub struct SymmetricDifferenceIterator<Z: DictZipper> {
    /// DFS traversal stack
    stack: Vec<SymmetricDifferenceZipper<Z>>,

    /// Paths already yielded (for deduplication)
    seen: HashSet<Vec<Z::Unit>>,
}

impl<Z: DictZipper> SymmetricDifferenceIterator<Z> {
    /// Create a new iterator starting from the given symmetric difference zipper.
    fn new(zipper: SymmetricDifferenceZipper<Z>) -> Self {
        let mut stack = Vec::with_capacity(16);
        stack.push(zipper);
        Self {
            stack,
            seen: HashSet::new(),
        }
    }
}

impl<Z: DictZipper> Iterator for SymmetricDifferenceIterator<Z> {
    type Item = (Vec<Z::Unit>, SymmetricDifferenceZipper<Z>);

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(zipper) = self.stack.pop() {
            // Push all children onto stack for continued DFS traversal
            for (_label, child) in zipper.children() {
                self.stack.push(child);
            }

            // If this is a symmetric difference term (in exactly one dict) and unseen, yield it
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
// ValuedSymmetricDifferenceIterator
// =============================================================================

/// Iterator over (term, value) pairs in a symmetric difference.
///
/// # Type Parameters
///
/// * `Z` - The underlying valued zipper type
///
/// # Iterator Item
///
/// Returns `(Vec<Z::Unit>, Z::Value)`:
/// - `Vec<Z::Unit>` - Complete path (term) as sequence of units
/// - `Z::Value` - Value from the single dictionary containing this term
pub struct ValuedSymmetricDifferenceIterator<Z: ValuedDictZipper> {
    inner: SymmetricDifferenceIterator<Z>,
}

impl<Z: ValuedDictZipper> ValuedSymmetricDifferenceIterator<Z> {
    /// Create a new valued iterator from a symmetric difference zipper.
    pub fn new(zipper: SymmetricDifferenceZipper<Z>) -> Self {
        Self {
            inner: SymmetricDifferenceIterator::new(zipper),
        }
    }
}

impl<Z: ValuedDictZipper> Iterator for ValuedSymmetricDifferenceIterator<Z> {
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
// SymmetricDifferenceZipperExt Extension Trait
// =============================================================================

/// Extension trait for ergonomic symmetric difference zipper creation.
///
/// This trait is automatically implemented for all `DictZipper` types, providing
/// convenient methods to create symmetric difference zippers.
///
/// # Examples
///
/// ```rust
/// use libdictenstein::prelude::*;
/// use libdictenstein::symmetric_difference_zipper::SymmetricDifferenceZipperExt;
/// use libdictenstein::double_array_trie_zipper::DoubleArrayTrieZipper;
///
/// let dict1 = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());
/// let dict2 = DoubleArrayTrie::from_terms(vec!["dog", "fish"].iter());
///
/// let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
/// let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);
///
/// // Simple two-zipper symmetric difference
/// let sym_diff = z1.symmetric_difference_with(z2);
/// ```
pub trait SymmetricDifferenceZipperExt: DictZipper + Sized {
    /// Create a symmetric difference of this zipper with another.
    ///
    /// Returns a zipper yielding terms in exactly one of the dictionaries.
    ///
    /// # Arguments
    ///
    /// * `other` - Another zipper of the same type
    ///
    /// # Returns
    ///
    /// A `SymmetricDifferenceZipper` computing self Δ other.
    fn symmetric_difference_with(self, other: Self) -> SymmetricDifferenceZipper<Self> {
        SymmetricDifferenceZipper::new(vec![self, other])
    }

    /// Create a symmetric difference of this zipper with multiple others.
    ///
    /// Returns a zipper yielding terms in exactly one of all the dictionaries.
    ///
    /// # Arguments
    ///
    /// * `others` - Additional zippers to include
    ///
    /// # Returns
    ///
    /// A `SymmetricDifferenceZipper` computing the multi-way symmetric difference.
    fn symmetric_difference_all(
        self,
        others: impl IntoIterator<Item = Self>,
    ) -> SymmetricDifferenceZipper<Self> {
        let mut zippers = vec![self];
        zippers.extend(others);
        SymmetricDifferenceZipper::new(zippers)
    }
}

/// Blanket implementation: all DictZippers automatically get SymmetricDifferenceZipperExt support.
impl<Z: DictZipper> SymmetricDifferenceZipperExt for Z {}

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
    fn test_symmetric_difference_basic() {
        let dict_a = DoubleArrayTrie::from_terms(vec!["cat", "dog", "fish"].iter());
        let dict_b = DoubleArrayTrie::from_terms(vec!["dog", "fish", "bird"].iter());

        let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
        let z_b = DoubleArrayTrieZipper::new_from_dict(&dict_b);

        let sym_diff = SymmetricDifferenceZipper::new(vec![z_a, z_b]);

        let results: Vec<String> = sorted_strings(
            sym_diff
                .iter()
                .map(|(path, _)| String::from_utf8(path).unwrap())
                .collect(),
        );

        // "cat" only in A, "bird" only in B; "dog" and "fish" in both (excluded)
        assert_eq!(results, vec!["bird", "cat"]);
    }

    #[test]
    fn test_symmetric_difference_identical() {
        let dict_a = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());
        let dict_b = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());

        let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
        let z_b = DoubleArrayTrieZipper::new_from_dict(&dict_b);

        let sym_diff = z_a.symmetric_difference_with(z_b);

        let count = sym_diff.iter().count();

        // A Δ A = ∅
        assert_eq!(count, 0);
    }

    #[test]
    fn test_symmetric_difference_disjoint() {
        let dict_a = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());
        let dict_b = DoubleArrayTrie::from_terms(vec!["fish", "bird"].iter());

        let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
        let z_b = DoubleArrayTrieZipper::new_from_dict(&dict_b);

        let sym_diff = z_a.symmetric_difference_with(z_b);

        let results: Vec<String> = sorted_strings(
            sym_diff
                .iter()
                .map(|(path, _)| String::from_utf8(path).unwrap())
                .collect(),
        );

        // Disjoint sets: A Δ B = A ∪ B
        assert_eq!(results, vec!["bird", "cat", "dog", "fish"]);
    }

    #[test]
    fn test_symmetric_difference_with_empty() {
        let dict_a = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());
        let dict_b: DoubleArrayTrie = DoubleArrayTrie::new();

        let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
        let z_b = DoubleArrayTrieZipper::new_from_dict(&dict_b);

        let sym_diff = z_a.symmetric_difference_with(z_b);

        let results: Vec<String> = sorted_strings(
            sym_diff
                .iter()
                .map(|(path, _)| String::from_utf8(path).unwrap())
                .collect(),
        );

        // A Δ ∅ = A
        assert_eq!(results, vec!["cat", "dog"]);
    }

    #[test]
    fn test_symmetric_difference_three_dicts() {
        let dict1 = DoubleArrayTrie::from_terms(vec!["a", "b", "c"].iter());
        let dict2 = DoubleArrayTrie::from_terms(vec!["b", "c", "d"].iter());
        let dict3 = DoubleArrayTrie::from_terms(vec!["c", "d", "e"].iter());

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);
        let z3 = DoubleArrayTrieZipper::new_from_dict(&dict3);

        let sym_diff = z1.symmetric_difference_all(vec![z2, z3]);

        let results: Vec<String> = sorted_strings(
            sym_diff
                .iter()
                .map(|(path, _)| String::from_utf8(path).unwrap())
                .collect(),
        );

        // Terms in exactly one dict:
        // "a" only in dict1 ✓
        // "b" in dict1, dict2 ✗
        // "c" in all three ✗
        // "d" in dict2, dict3 ✗
        // "e" only in dict3 ✓
        assert_eq!(results, vec!["a", "e"]);
    }

    #[test]
    fn test_symmetric_difference_descend() {
        let dict_a = DoubleArrayTrie::from_terms(vec!["cat", "car"].iter());
        let dict_b = DoubleArrayTrie::from_terms(vec!["cat", "cab"].iter());

        let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
        let z_b = DoubleArrayTrieZipper::new_from_dict(&dict_b);

        let sym_diff = z_a.symmetric_difference_with(z_b);

        // Navigate to "cat" - in BOTH, so NOT in symmetric difference
        let cat = sym_diff
            .descend(b'c')
            .and_then(|z| z.descend(b'a'))
            .and_then(|z| z.descend(b't'));
        assert!(cat.is_some());
        assert!(!cat.unwrap().is_final()); // In both, so excluded

        // Navigate to "car" - only in A, so IS in symmetric difference
        let car = sym_diff
            .descend(b'c')
            .and_then(|z| z.descend(b'a'))
            .and_then(|z| z.descend(b'r'));
        assert!(car.is_some());
        assert!(car.unwrap().is_final()); // Only in A

        // Navigate to "cab" - only in B, so IS in symmetric difference
        let cab = sym_diff
            .descend(b'c')
            .and_then(|z| z.descend(b'a'))
            .and_then(|z| z.descend(b'b'));
        assert!(cab.is_some());
        assert!(cab.unwrap().is_final()); // Only in B
    }

    #[test]
    fn test_symmetric_difference_children() {
        let dict_a = DoubleArrayTrie::from_terms(vec!["ab", "ac"].iter());
        let dict_b = DoubleArrayTrie::from_terms(vec!["ac", "ad"].iter());

        let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
        let z_b = DoubleArrayTrieZipper::new_from_dict(&dict_b);

        let sym_diff = z_a.symmetric_difference_with(z_b);

        // Navigate to 'a'
        let a = sym_diff.descend(b'a').unwrap();

        // Children should be union: b, c, d
        let mut children: Vec<u8> = a.children().map(|(label, _)| label).collect();
        children.sort();

        assert_eq!(children, vec![b'b', b'c', b'd']);
    }

    #[test]
    fn test_symmetric_difference_with_values() {
        let dict_a =
            DoubleArrayTrie::from_terms_with_values(vec![("cat", 1usize), ("dog", 2)].into_iter());
        let dict_b = DoubleArrayTrie::from_terms_with_values(
            vec![("dog", 20usize), ("fish", 3)].into_iter(),
        );

        let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
        let z_b = DoubleArrayTrieZipper::new_from_dict(&dict_b);

        let sym_diff = z_a.symmetric_difference_with(z_b);

        // "cat" - only in A, should have value 1
        let cat = sym_diff
            .descend(b'c')
            .and_then(|z| z.descend(b'a'))
            .and_then(|z| z.descend(b't'))
            .unwrap();
        assert!(cat.is_final());
        assert_eq!(cat.value(), Some(1));

        // "fish" - only in B, should have value 3
        let fish = sym_diff
            .descend(b'f')
            .and_then(|z| z.descend(b'i'))
            .and_then(|z| z.descend(b's'))
            .and_then(|z| z.descend(b'h'))
            .unwrap();
        assert!(fish.is_final());
        assert_eq!(fish.value(), Some(3));

        // "dog" - in both, not final
        let dog = sym_diff
            .descend(b'd')
            .and_then(|z| z.descend(b'o'))
            .and_then(|z| z.descend(b'g'))
            .unwrap();
        assert!(!dog.is_final());
        assert_eq!(dog.value(), None);
    }

    #[test]
    fn test_symmetric_difference_valued_iterator() {
        let dict_a =
            DoubleArrayTrie::from_terms_with_values(vec![("cat", 1usize), ("dog", 2)].into_iter());
        let dict_b = DoubleArrayTrie::from_terms_with_values(
            vec![("dog", 20usize), ("fish", 3)].into_iter(),
        );

        let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
        let z_b = DoubleArrayTrieZipper::new_from_dict(&dict_b);

        let sym_diff = z_a.symmetric_difference_with(z_b);
        let valued_iter = ValuedSymmetricDifferenceIterator::new(sym_diff);

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
    fn test_unique_source_index() {
        let dict_a = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());
        let dict_b = DoubleArrayTrie::from_terms(vec!["dog", "fish"].iter());

        let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
        let z_b = DoubleArrayTrieZipper::new_from_dict(&dict_b);

        let sym_diff = z_a.symmetric_difference_with(z_b);

        // "cat" is only in dict 0 (A)
        let cat = sym_diff
            .descend(b'c')
            .and_then(|z| z.descend(b'a'))
            .and_then(|z| z.descend(b't'))
            .unwrap();
        assert_eq!(cat.unique_source_index(), Some(0));

        // "fish" is only in dict 1 (B)
        let fish = sym_diff
            .descend(b'f')
            .and_then(|z| z.descend(b'i'))
            .and_then(|z| z.descend(b's'))
            .and_then(|z| z.descend(b'h'))
            .unwrap();
        assert_eq!(fish.unique_source_index(), Some(1));

        // "dog" is in both, no unique source
        let dog = sym_diff
            .descend(b'd')
            .and_then(|z| z.descend(b'o'))
            .and_then(|z| z.descend(b'g'))
            .unwrap();
        assert_eq!(dog.unique_source_index(), None);
    }

    #[test]
    fn test_dictionary_count() {
        let dict1 = DoubleArrayTrie::from_terms(vec!["a"].iter());
        let dict2 = DoubleArrayTrie::from_terms(vec!["b"].iter());
        let dict3 = DoubleArrayTrie::from_terms(vec!["c"].iter());

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);
        let z3 = DoubleArrayTrieZipper::new_from_dict(&dict3);

        let sym_diff = z1.symmetric_difference_all(vec![z2, z3]);

        assert_eq!(sym_diff.dictionary_count(), 3);
        assert_eq!(sym_diff.active_dictionary_count(), 3);
    }

    #[test]
    fn test_property_symmetric_difference_equals_union_minus_intersection() {
        // Property: A Δ B = (A ∪ B) \ (A ∩ B)
        let dict_a = DoubleArrayTrie::from_terms(vec!["cat", "dog", "fish"].iter());
        let dict_b = DoubleArrayTrie::from_terms(vec!["dog", "fish", "bird"].iter());

        let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
        let z_b = DoubleArrayTrieZipper::new_from_dict(&dict_b);

        let sym_diff = z_a.symmetric_difference_with(z_b);

        let sym_diff_terms: HashSet<String> = sym_diff
            .iter()
            .map(|(path, _)| String::from_utf8(path).unwrap())
            .collect();

        // Manually compute (A ∪ B) \ (A ∩ B)
        let a_terms: HashSet<String> = ["cat", "dog", "fish"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let b_terms: HashSet<String> = ["dog", "fish", "bird"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        let union: HashSet<String> = a_terms.union(&b_terms).cloned().collect();
        let intersection: HashSet<String> = a_terms.intersection(&b_terms).cloned().collect();
        let expected: HashSet<String> = union.difference(&intersection).cloned().collect();

        assert_eq!(sym_diff_terms, expected);
    }

    #[test]
    fn test_property_symmetric_difference_equals_difference_union() {
        // Property: A Δ B = (A \ B) ∪ (B \ A)
        let dict_a = DoubleArrayTrie::from_terms(vec!["cat", "dog", "fish"].iter());
        let dict_b = DoubleArrayTrie::from_terms(vec!["dog", "fish", "bird"].iter());

        let z_a1 = DoubleArrayTrieZipper::new_from_dict(&dict_a);
        let z_b1 = DoubleArrayTrieZipper::new_from_dict(&dict_b);
        let z_a2 = DoubleArrayTrieZipper::new_from_dict(&dict_a);
        let z_b2 = DoubleArrayTrieZipper::new_from_dict(&dict_b);

        let sym_diff = z_a1.symmetric_difference_with(z_b1);

        let sym_diff_terms: HashSet<String> = sym_diff
            .iter()
            .map(|(path, _)| String::from_utf8(path).unwrap())
            .collect();

        // Compute (A \ B) ∪ (B \ A) using DifferenceZipper
        use crate::difference_zipper::DifferenceZipperExt;

        let a_minus_b: HashSet<String> = z_a2
            .clone()
            .difference_from(z_b2.clone())
            .iter()
            .map(|(path, _)| String::from_utf8(path).unwrap())
            .collect();

        let b_minus_a: HashSet<String> = z_b2
            .difference_from(z_a2)
            .iter()
            .map(|(path, _)| String::from_utf8(path).unwrap())
            .collect();

        let expected: HashSet<String> = a_minus_b.union(&b_minus_a).cloned().collect();

        assert_eq!(sym_diff_terms, expected);
    }
}
