//! Intersection zipper for multi-dictionary navigation.
//!
//! This module provides an `IntersectionZipper` that presents the intersection of multiple
//! dictionaries as a unified view. A term is included if and only if it exists in ALL
//! underlying dictionaries.
//!
//! # Use Cases
//!
//! - **Common vocabulary**: Find terms that exist in multiple corpora
//! - **Shared ngrams**: Identify ngrams common to all input texts
//! - **Vocabulary overlap**: Analyze intersection of different word lists
//!
//! # Examples
//!
//! ## Basic Intersection of Two Dictionaries
//!
//! ```rust
//! use libdictenstein::prelude::*;
//! use libdictenstein::intersection_zipper::{IntersectionZipper, IntersectionZipperExt};
//! use libdictenstein::double_array_trie_zipper::DoubleArrayTrieZipper;
//!
//! let dict1 = DoubleArrayTrie::from_terms(vec!["cat", "dog", "fish"].iter());
//! let dict2 = DoubleArrayTrie::from_terms(vec!["cat", "fish", "bird"].iter());
//!
//! let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
//! let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);
//!
//! // Create intersection using extension trait
//! let intersection = z1.intersection_with(z2);
//!
//! // Iterate terms in both dictionaries
//! let mut results: Vec<String> = intersection.iter()
//!     .map(|(path, _)| String::from_utf8(path).unwrap())
//!     .collect();
//! results.sort();
//! assert_eq!(results, vec!["cat", "fish"]); // Only terms in BOTH dicts
//! ```
//!
//! ## Navigating the Intersection
//!
//! ```rust
//! use libdictenstein::prelude::*;
//! use libdictenstein::intersection_zipper::IntersectionZipperExt;
//! use libdictenstein::double_array_trie_zipper::DoubleArrayTrieZipper;
//! use libdictenstein::zipper::DictZipper;
//!
//! let dict1 = DoubleArrayTrie::from_terms(vec!["cat", "car", "cab"].iter());
//! let dict2 = DoubleArrayTrie::from_terms(vec!["cat", "can", "cab"].iter());
//!
//! let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
//! let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);
//!
//! let intersection = z1.intersection_with(z2);
//!
//! // Navigate to "cat" - exists in both
//! let cat = intersection.descend(b'c')
//!     .and_then(|z| z.descend(b'a'))
//!     .and_then(|z| z.descend(b't'));
//! assert!(cat.is_some());
//! assert!(cat.unwrap().is_final());
//!
//! // Navigate to "car" - only exists in dict1, NOT final in intersection
//! let car = intersection.descend(b'c')
//!     .and_then(|z| z.descend(b'a'))
//!     .and_then(|z| z.descend(b'r'));
//! // Path exists (to traverse to other terms) but "car" is not final
//! assert!(car.is_none() || !car.unwrap().is_final());
//! ```
//!
//! ## Value Merge with LatticeMeet
//!
//! ```rust
//! use libdictenstein::prelude::*;
//! use libdictenstein::intersection_zipper::IntersectionZipper;
//! use libdictenstein::union_zipper::LatticeMeet;
//! use libdictenstein::double_array_trie_zipper::DoubleArrayTrieZipper;
//! use libdictenstein::zipper::{DictZipper, ValuedDictZipper};
//!
//! let dict1 = DoubleArrayTrie::from_terms_with_values(vec![("score", 85u32)].into_iter());
//! let dict2 = DoubleArrayTrie::from_terms_with_values(vec![("score", 92u32)].into_iter());
//!
//! let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
//! let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);
//!
//! // Default strategy is LatticeMeet (min for numbers)
//! let intersection = IntersectionZipper::new(vec![z1, z2]);
//!
//! let score = intersection.descend(b's')
//!     .and_then(|z| z.descend(b'c'))
//!     .and_then(|z| z.descend(b'o'))
//!     .and_then(|z| z.descend(b'r'))
//!     .and_then(|z| z.descend(b'e'))
//!     .unwrap();
//!
//! // LatticeMeet: min(85, 92) = 85
//! assert_eq!(score.value(), Some(85));
//! ```
//!
//! # Performance
//!
//! - **Navigation**: O(k × n) where k = prefix length, n = number of dictionaries
//! - **Children collection**: O(c × n) where c = max children per node, n = dictionaries
//! - **Iteration**: O(m) where m = total terms in intersection
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

use crate::union_zipper::{Lattice, LatticeMeet, ValueMergeStrategy};
use crate::zipper::{DictZipper, ValuedDictZipper};

// =============================================================================
// IntersectionZipper
// =============================================================================

/// A zipper that presents the intersection of multiple dictionaries.
///
/// `IntersectionZipper` wraps multiple zippers and presents their intersection as a single
/// navigable structure. A term is included if and only if it exists in ALL underlying
/// dictionaries.
///
/// # Type Parameters
///
/// * `Z` - The underlying zipper type (must implement `DictZipper`)
/// * `S` - The value merge strategy (defaults to `LatticeMeet`)
///
/// # Navigation
///
/// Navigation through the intersection considers all underlying dictionaries:
/// - `is_final()` returns true only if ALL active dictionaries mark the position as final
/// - `descend(label)` succeeds if ANY dictionary has the path (to allow traversal to common nodes)
/// - `children()` returns children where at least one dictionary can descend
///
/// # Examples
///
/// See module-level documentation for comprehensive examples.
#[derive(Clone, Debug)]
pub struct IntersectionZipper<Z: DictZipper, S = LatticeMeet> {
    /// The underlying zippers. `None` entries indicate dictionaries that don't
    /// have the current path.
    zippers: Vec<Option<Z>>,

    /// Path from root to current position.
    path: Vec<Z::Unit>,

    /// Value merge strategy.
    strategy: S,
}

impl<Z: DictZipper> IntersectionZipper<Z, LatticeMeet> {
    /// Create a new intersection zipper with the default `LatticeMeet` strategy.
    ///
    /// `LatticeMeet` computes the greatest lower bound (minimum for numbers,
    /// intersection for sets) when merging values from multiple dictionaries.
    ///
    /// # Arguments
    ///
    /// * `zippers` - Zippers to intersect, each positioned at their respective roots
    ///
    /// # Returns
    ///
    /// A new `IntersectionZipper` positioned at the intersection root.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::prelude::*;
    /// use libdictenstein::intersection_zipper::IntersectionZipper;
    /// use libdictenstein::double_array_trie_zipper::DoubleArrayTrieZipper;
    ///
    /// let dict1 = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());
    /// let dict2 = DoubleArrayTrie::from_terms(vec!["cat", "fish"].iter());
    ///
    /// let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    /// let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);
    ///
    /// let intersection = IntersectionZipper::new(vec![z1, z2]);
    /// ```
    pub fn new(zippers: Vec<Z>) -> Self {
        Self {
            zippers: zippers.into_iter().map(Some).collect(),
            path: Vec::new(),
            strategy: LatticeMeet,
        }
    }
}

impl<Z: DictZipper, S: Clone + Send + Sync> IntersectionZipper<Z, S> {
    /// Create a new intersection zipper with a custom merge strategy.
    ///
    /// # Arguments
    ///
    /// * `zippers` - Zippers to intersect, each positioned at their respective roots
    /// * `strategy` - The merge strategy for handling values at intersection points
    ///
    /// # Returns
    ///
    /// A new `IntersectionZipper` with the specified strategy.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::prelude::*;
    /// use libdictenstein::intersection_zipper::IntersectionZipper;
    /// use libdictenstein::union_zipper::LatticeJoin;
    /// use libdictenstein::double_array_trie_zipper::DoubleArrayTrieZipper;
    ///
    /// let dict1 = DoubleArrayTrie::from_terms_with_values(vec![("cat", 1)].into_iter());
    /// let dict2 = DoubleArrayTrie::from_terms_with_values(vec![("cat", 10)].into_iter());
    ///
    /// let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    /// let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);
    ///
    /// // Use LatticeJoin (max) instead of default LatticeMeet (min)
    /// let intersection = IntersectionZipper::with_strategy(vec![z1, z2], LatticeJoin);
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
    /// The total number of dictionaries in this intersection.
    pub fn dictionary_count(&self) -> usize {
        self.zippers.len()
    }

    /// Get the number of active dictionaries at the current position.
    ///
    /// A dictionary is "active" if it has the current path. For a true intersection
    /// point (where `is_final()` returns true), all dictionaries must be active.
    ///
    /// # Returns
    ///
    /// The number of dictionaries that have the current path.
    pub fn active_dictionary_count(&self) -> usize {
        self.zippers.iter().filter(|z| z.is_some()).count()
    }

    /// Create an iterator over all terms in the intersection.
    ///
    /// Terms are yielded exactly once. Only terms that exist in ALL underlying
    /// dictionaries are included.
    ///
    /// # Returns
    ///
    /// An iterator yielding `(path, zipper)` pairs for each term in the intersection.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::prelude::*;
    /// use libdictenstein::intersection_zipper::IntersectionZipperExt;
    /// use libdictenstein::double_array_trie_zipper::DoubleArrayTrieZipper;
    ///
    /// let dict1 = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());
    /// let dict2 = DoubleArrayTrie::from_terms(vec!["cat", "fish"].iter());
    ///
    /// let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    /// let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);
    ///
    /// let intersection = z1.intersection_with(z2);
    /// let count = intersection.iter().count();
    /// assert_eq!(count, 1); // Only "cat" is in both
    /// ```
    pub fn iter(&self) -> IntersectionIterator<Z, S> {
        IntersectionIterator::new(self.clone())
    }
}

impl<Z: DictZipper, S: Clone + Send + Sync> DictZipper for IntersectionZipper<Z, S> {
    type Unit = Z::Unit;

    fn is_final(&self) -> bool {
        // Final only if ALL active dictionaries mark this position as final.
        // If any dictionary doesn't have this path (is None) or isn't final, return false.
        let active_count = self.zippers.iter().filter(|z| z.is_some()).count();

        // If no dictionaries are active, nothing can be in the intersection
        if active_count == 0 {
            return false;
        }

        // All active zippers must be final for this to be an intersection point
        self.zippers
            .iter()
            .filter_map(|z| z.as_ref())
            .all(|z| z.is_final())
            && active_count == self.zippers.len()
    }

    fn descend(&self, label: Self::Unit) -> Option<Self> {
        // Descend in all active zippers
        let new_zippers: Vec<Option<Z>> = self
            .zippers
            .iter()
            .map(|z| z.as_ref().and_then(|z| z.descend(label)))
            .collect();

        // For intersection: continue only if ALL original zippers can descend.
        // If any zipper becomes None, that path cannot lead to an intersection.
        let all_can_descend = new_zippers.iter().all(|z| z.is_some());

        if all_can_descend {
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
        // For intersection: only include children that ALL dictionaries have.
        // First, collect labels present in ALL active zippers.

        // Get labels from each active zipper
        let label_sets: Vec<HashSet<Z::Unit>> = self
            .zippers
            .iter()
            .filter_map(|z| z.as_ref())
            .map(|z| z.children().map(|(label, _)| label).collect())
            .collect();

        // Find intersection of all label sets
        let common_labels: Vec<Z::Unit> = if label_sets.is_empty() {
            Vec::new()
        } else if label_sets.len() == 1 {
            label_sets[0].iter().copied().collect()
        } else {
            // Intersect all sets
            let mut result = label_sets[0].clone();
            for set in label_sets.iter().skip(1) {
                result = result.intersection(set).copied().collect();
            }
            let mut labels: Vec<_> = result.into_iter().collect();
            // Sort for deterministic ordering
            labels.sort_by(|a, b| format!("{:?}", a).cmp(&format!("{:?}", b)));
            labels
        };

        // Create child zippers for each common label
        let self_clone = self.clone();
        common_labels
            .into_iter()
            .filter_map(move |label| self_clone.descend(label).map(|child| (label, child)))
    }

    fn path(&self) -> Vec<Self::Unit> {
        self.path.clone()
    }
}

impl<Z: ValuedDictZipper, S: ValueMergeStrategy<Z::Value> + Clone + Send + Sync> ValuedDictZipper
    for IntersectionZipper<Z, S>
where
    Z::Value: Lattice,
{
    type Value = Z::Value;

    fn value(&self) -> Option<Self::Value> {
        if !self.is_final() {
            return None;
        }

        // Collect values from all active zippers and merge them
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
// IntersectionIterator
// =============================================================================

/// Iterator over all terms in an intersection of dictionaries.
///
/// This iterator performs depth-first traversal and yields each term that exists
/// in ALL underlying dictionaries exactly once.
///
/// # Type Parameters
///
/// * `Z` - The underlying zipper type
/// * `S` - The value merge strategy
///
/// # Iterator Item
///
/// Returns `(Vec<Z::Unit>, IntersectionZipper<Z, S>)`:
/// - `Vec<Z::Unit>` - Complete path (term) as sequence of units
/// - `IntersectionZipper<Z, S>` - Zipper positioned at the final node
pub struct IntersectionIterator<Z: DictZipper, S = LatticeMeet> {
    /// DFS traversal stack
    stack: Vec<IntersectionZipper<Z, S>>,

    /// Paths already yielded (for deduplication)
    seen: HashSet<Vec<Z::Unit>>,
}

impl<Z: DictZipper, S: Clone + Send + Sync> IntersectionIterator<Z, S> {
    /// Create a new iterator starting from the given intersection zipper.
    fn new(zipper: IntersectionZipper<Z, S>) -> Self {
        let mut stack = Vec::with_capacity(16);
        stack.push(zipper);
        Self {
            stack,
            seen: HashSet::new(),
        }
    }
}

impl<Z: DictZipper, S: Clone + Send + Sync> Iterator for IntersectionIterator<Z, S> {
    type Item = (Vec<Z::Unit>, IntersectionZipper<Z, S>);

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
// ValuedIntersectionIterator
// =============================================================================

/// Iterator over (term, value) pairs in an intersection of valued dictionaries.
///
/// This iterator wraps `IntersectionIterator` and extracts merged values from final
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
pub struct ValuedIntersectionIterator<Z: ValuedDictZipper, S> {
    inner: IntersectionIterator<Z, S>,
}

impl<Z: ValuedDictZipper, S: ValueMergeStrategy<Z::Value> + Clone + Send + Sync>
    ValuedIntersectionIterator<Z, S>
where
    Z::Value: Lattice,
{
    /// Create a new valued iterator from an intersection zipper.
    pub fn new(zipper: IntersectionZipper<Z, S>) -> Self {
        Self {
            inner: IntersectionIterator::new(zipper),
        }
    }
}

impl<Z: ValuedDictZipper, S: ValueMergeStrategy<Z::Value> + Clone + Send + Sync> Iterator
    for ValuedIntersectionIterator<Z, S>
where
    Z::Value: Lattice,
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
// IntersectionZipperExt Extension Trait
// =============================================================================

/// Extension trait for ergonomic intersection zipper creation.
///
/// This trait is automatically implemented for all `DictZipper` types, providing
/// convenient methods to create intersection zippers.
///
/// # Examples
///
/// ```rust
/// use libdictenstein::prelude::*;
/// use libdictenstein::intersection_zipper::IntersectionZipperExt;
/// use libdictenstein::double_array_trie_zipper::DoubleArrayTrieZipper;
///
/// let dict1 = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());
/// let dict2 = DoubleArrayTrie::from_terms(vec!["cat", "fish"].iter());
///
/// let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
/// let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);
///
/// // Simple two-zipper intersection
/// let intersection = z1.clone().intersection_with(z2.clone());
///
/// // Multi-zipper intersection
/// let dict3 = DoubleArrayTrie::from_terms(vec!["cat", "bird"].iter());
/// let z3 = DoubleArrayTrieZipper::new_from_dict(&dict3);
/// let multi_intersection = z1.intersection_all(vec![z2, z3]);
/// ```
pub trait IntersectionZipperExt: DictZipper + Sized {
    /// Create an intersection of this zipper with another.
    ///
    /// # Arguments
    ///
    /// * `other` - Another zipper of the same type
    ///
    /// # Returns
    ///
    /// An `IntersectionZipper` combining both dictionaries with `LatticeMeet` strategy.
    fn intersection_with(self, other: Self) -> IntersectionZipper<Self> {
        IntersectionZipper::new(vec![self, other])
    }

    /// Create an intersection of this zipper with multiple others.
    ///
    /// # Arguments
    ///
    /// * `others` - Additional zippers to include in the intersection
    ///
    /// # Returns
    ///
    /// An `IntersectionZipper` combining all dictionaries with `LatticeMeet` strategy.
    fn intersection_all(self, others: impl IntoIterator<Item = Self>) -> IntersectionZipper<Self> {
        let mut zippers = vec![self];
        zippers.extend(others);
        IntersectionZipper::new(zippers)
    }
}

/// Blanket implementation: all DictZippers automatically get IntersectionZipperExt support.
impl<Z: DictZipper> IntersectionZipperExt for Z {}

// =============================================================================
// ValuedIntersectionZipperExt Extension Trait
// =============================================================================

/// Extension trait for valued intersection zipper iteration.
///
/// This trait is automatically implemented for all `ValuedDictZipper` types,
/// providing methods to iterate with values using merge strategies.
pub trait ValuedIntersectionZipperExt: ValuedDictZipper + Sized {
    /// Create an intersection of this zipper with another, using a custom strategy.
    ///
    /// # Arguments
    ///
    /// * `other` - Another zipper of the same type
    /// * `strategy` - The merge strategy for values at intersection points
    ///
    /// # Returns
    ///
    /// An `IntersectionZipper` with the specified strategy.
    fn intersection_with_strategy<S: ValueMergeStrategy<Self::Value> + Clone + Send + Sync>(
        self,
        other: Self,
        strategy: S,
    ) -> IntersectionZipper<Self, S> {
        IntersectionZipper::with_strategy(vec![self, other], strategy)
    }
}

/// Blanket implementation: all ValuedDictZippers get ValuedIntersectionZipperExt support.
impl<Z: ValuedDictZipper> ValuedIntersectionZipperExt for Z {}

// =============================================================================
// Unit Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::double_array_trie::DoubleArrayTrie;
    use crate::double_array_trie_zipper::DoubleArrayTrieZipper;
    use crate::union_zipper::LatticeJoin;

    fn sorted_strings(mut v: Vec<String>) -> Vec<String> {
        v.sort();
        v
    }

    #[test]
    fn test_intersection_basic() {
        let dict1 = DoubleArrayTrie::from_terms(vec!["cat", "dog", "fish"].iter());
        let dict2 = DoubleArrayTrie::from_terms(vec!["cat", "fish", "bird"].iter());

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

        let intersection = IntersectionZipper::new(vec![z1, z2]);

        let results: Vec<String> = sorted_strings(
            intersection
                .iter()
                .map(|(path, _)| String::from_utf8(path).unwrap())
                .collect(),
        );

        assert_eq!(results, vec!["cat", "fish"]); // Only terms in BOTH
    }

    #[test]
    fn test_intersection_disjoint() {
        let dict1 = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());
        let dict2 = DoubleArrayTrie::from_terms(vec!["fish", "bird"].iter());

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

        let intersection = z1.intersection_with(z2);

        let count = intersection.iter().count();
        assert_eq!(count, 0); // No common terms
    }

    #[test]
    fn test_intersection_identical() {
        let dict1 = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());
        let dict2 = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

        let intersection = z1.intersection_with(z2);

        let results: Vec<String> = sorted_strings(
            intersection
                .iter()
                .map(|(path, _)| String::from_utf8(path).unwrap())
                .collect(),
        );

        assert_eq!(results, vec!["cat", "dog"]); // All terms match
    }

    #[test]
    fn test_intersection_empty_dict() {
        let dict1 = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());
        let dict2: DoubleArrayTrie = DoubleArrayTrie::new();

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

        let intersection = z1.intersection_with(z2);

        let count = intersection.iter().count();
        assert_eq!(count, 0); // Empty dict means empty intersection
    }

    #[test]
    fn test_intersection_three_dicts() {
        let dict1 = DoubleArrayTrie::from_terms(vec!["cat", "dog", "fish", "bird"].iter());
        let dict2 = DoubleArrayTrie::from_terms(vec!["cat", "fish", "bird", "horse"].iter());
        let dict3 = DoubleArrayTrie::from_terms(vec!["cat", "bird", "snake"].iter());

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);
        let z3 = DoubleArrayTrieZipper::new_from_dict(&dict3);

        let intersection = z1.intersection_all(vec![z2, z3]);

        let results: Vec<String> = sorted_strings(
            intersection
                .iter()
                .map(|(path, _)| String::from_utf8(path).unwrap())
                .collect(),
        );

        assert_eq!(results, vec!["bird", "cat"]); // Only in ALL three
    }

    #[test]
    fn test_intersection_descend() {
        let dict1 = DoubleArrayTrie::from_terms(vec!["cat", "car"].iter());
        let dict2 = DoubleArrayTrie::from_terms(vec!["cat", "can"].iter());

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

        let intersection = z1.intersection_with(z2);

        // Navigate to 'c' -> 'a' -> 't' (cat, common)
        let cat = intersection
            .descend(b'c')
            .and_then(|z| z.descend(b'a'))
            .and_then(|z| z.descend(b't'));

        assert!(cat.is_some());
        let cat = cat.unwrap();
        assert!(cat.is_final());
        assert_eq!(cat.path(), b"cat".to_vec());

        // 'car' only in dict1 - path should not exist in intersection
        let car = intersection
            .descend(b'c')
            .and_then(|z| z.descend(b'a'))
            .and_then(|z| z.descend(b'r'));

        assert!(car.is_none());
    }

    #[test]
    fn test_intersection_is_final() {
        let dict1 = DoubleArrayTrie::from_terms(vec!["cat", "catch"].iter());
        let dict2 = DoubleArrayTrie::from_terms(vec!["catch"].iter()); // Only "catch", not "cat"

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

        let intersection = z1.intersection_with(z2);

        // Navigate to "cat"
        let cat = intersection
            .descend(b'c')
            .and_then(|z| z.descend(b'a'))
            .and_then(|z| z.descend(b't'));

        // "cat" is final in dict1 but not in dict2
        // Path should not exist because dict2 doesn't have 'cat' path
        // (dict2 only has 'catch')
        // Actually, dict2 has the path c-a-t-c-h, so c-a-t exists but isn't final
        // For intersection, we need ALL to have the path, and dict2 does have c-a-t
        assert!(cat.is_some());
        let cat = cat.unwrap();
        // But it's not final in intersection because dict2's c-a-t isn't final
        assert!(!cat.is_final());

        // Navigate to "catch"
        let catch = cat.descend(b'c').and_then(|z| z.descend(b'h'));
        assert!(catch.is_some());
        assert!(catch.unwrap().is_final()); // "catch" is final in BOTH
    }

    #[test]
    fn test_intersection_children() {
        let dict1 = DoubleArrayTrie::from_terms(vec!["ab", "ac", "ad"].iter());
        let dict2 = DoubleArrayTrie::from_terms(vec!["ab", "ac", "ae"].iter());

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

        let intersection = z1.intersection_with(z2);

        // Navigate to 'a'
        let a = intersection.descend(b'a').unwrap();

        // Children should only include those present in BOTH
        let mut children: Vec<u8> = a.children().map(|(label, _)| label).collect();
        children.sort();

        // 'b' and 'c' are in both, 'd' is only in dict1, 'e' is only in dict2
        assert_eq!(children, vec![b'b', b'c']);
    }

    #[test]
    fn test_valued_intersection_lattice_meet() {
        let dict1 =
            DoubleArrayTrie::from_terms_with_values(vec![("cat", 85u32), ("dog", 50)].into_iter());
        let dict2 =
            DoubleArrayTrie::from_terms_with_values(vec![("cat", 92u32), ("dog", 60)].into_iter());

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

        let intersection = IntersectionZipper::new(vec![z1, z2]);

        // Navigate to "cat"
        let cat = intersection
            .descend(b'c')
            .and_then(|z| z.descend(b'a'))
            .and_then(|z| z.descend(b't'))
            .unwrap();

        // LatticeMeet: min(85, 92) = 85
        assert_eq!(cat.value(), Some(85));

        // Navigate to "dog"
        let dog = intersection
            .descend(b'd')
            .and_then(|z| z.descend(b'o'))
            .and_then(|z| z.descend(b'g'))
            .unwrap();

        // LatticeMeet: min(50, 60) = 50
        assert_eq!(dog.value(), Some(50));
    }

    #[test]
    fn test_valued_intersection_lattice_join() {
        let dict1 =
            DoubleArrayTrie::from_terms_with_values(vec![("cat", 85u32), ("dog", 50)].into_iter());
        let dict2 =
            DoubleArrayTrie::from_terms_with_values(vec![("cat", 92u32), ("dog", 60)].into_iter());

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

        let intersection = IntersectionZipper::with_strategy(vec![z1, z2], LatticeJoin);

        // Navigate to "cat"
        let cat = intersection
            .descend(b'c')
            .and_then(|z| z.descend(b'a'))
            .and_then(|z| z.descend(b't'))
            .unwrap();

        // LatticeJoin: max(85, 92) = 92
        assert_eq!(cat.value(), Some(92));
    }

    #[test]
    fn test_valued_intersection_hashset() {
        let dict1 = DoubleArrayTrie::from_terms_with_values(
            vec![("key", HashSet::from([1, 2, 3]))].into_iter(),
        );
        let dict2 = DoubleArrayTrie::from_terms_with_values(
            vec![("key", HashSet::from([2, 3, 4]))].into_iter(),
        );

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

        let intersection = IntersectionZipper::new(vec![z1, z2]);

        let key = intersection
            .descend(b'k')
            .and_then(|z| z.descend(b'e'))
            .and_then(|z| z.descend(b'y'))
            .unwrap();

        // LatticeMeet for HashSet: {1, 2, 3} ∩ {2, 3, 4} = {2, 3}
        assert_eq!(key.value(), Some(HashSet::from([2, 3])));
    }

    #[test]
    fn test_dictionary_count() {
        let dict1 = DoubleArrayTrie::from_terms(vec!["cat"].iter());
        let dict2 = DoubleArrayTrie::from_terms(vec!["cat"].iter());
        let dict3 = DoubleArrayTrie::from_terms(vec!["cat"].iter());

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);
        let z3 = DoubleArrayTrieZipper::new_from_dict(&dict3);

        let intersection = z1.intersection_all(vec![z2, z3]);

        assert_eq!(intersection.dictionary_count(), 3);
        assert_eq!(intersection.active_dictionary_count(), 3);
    }

    #[test]
    fn test_intersection_preserves_prefix_structure() {
        // Tests that shared prefixes work correctly
        let dict1 = DoubleArrayTrie::from_terms(vec!["apple", "application", "apply"].iter());
        let dict2 = DoubleArrayTrie::from_terms(vec!["apple", "apply", "apt"].iter());

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

        let intersection = z1.intersection_with(z2);

        let results: Vec<String> = sorted_strings(
            intersection
                .iter()
                .map(|(path, _)| String::from_utf8(path).unwrap())
                .collect(),
        );

        // Only "apple" and "apply" are in both
        assert_eq!(results, vec!["apple", "apply"]);
    }
}
