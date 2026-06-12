//! Efficient prefix-exclusion-based navigation and iteration for dictionary zippers.
//!
//! This module provides extension traits for dictionary zippers that enable
//! efficient iteration while skipping entire subtrees matching excluded prefixes.
//! Excluded subtrees are pruned at O(1) per prefix check during DFS traversal.
//!
//! **Primary use case**: Exclude paths starting with `\x00` (metadata/sentinel marker)
//! from normal iteration while still being able to iterate all visible entries.
//!
//! # Design
//!
//! The iterator filters children **before** pushing them to the DFS stack, skipping
//! entire excluded subtrees at O(1) per prefix check. This is significantly faster
//! than post-iteration filtering, which would still visit all excluded nodes.
//!
//! # Examples
//!
//! ## Basic Exclusion
//!
//! ```rust
//! use libdictenstein::prelude::*;
//! use libdictenstein::excluding_prefix_zipper::ExcludingPrefixZipper;
//! use libdictenstein::double_array_trie::zipper::DoubleArrayTrieZipper;
//!
//! let terms = vec!["\x00meta", "\x00index", "hello", "world"];
//! let dict = DoubleArrayTrie::from_terms(terms.iter());
//!
//! let zipper = DoubleArrayTrieZipper::new_from_dict(&dict);
//!
//! // Exclude null-prefixed terms
//! let excluded: &[&[u8]] = &[b"\x00"];
//! let mut results: Vec<String> = zipper
//!     .iter_excluding(excluded)
//!     .map(|(path, _)| String::from_utf8(path).unwrap())
//!     .collect();
//!
//! results.sort();
//! assert_eq!(results, vec!["hello", "world"]);
//! // Never visits \x00meta or \x00index subtrees
//! ```
//!
//! ## Multiple Exclusions
//!
//! ```rust
//! use libdictenstein::prelude::*;
//! use libdictenstein::excluding_prefix_zipper::ExcludingPrefixZipper;
//! use libdictenstein::double_array_trie::zipper::DoubleArrayTrieZipper;
//!
//! let terms = vec!["_internal", "_hidden", "public", "visible"];
//! let dict = DoubleArrayTrie::from_terms(terms.iter());
//!
//! let zipper = DoubleArrayTrieZipper::new_from_dict(&dict);
//!
//! // Exclude underscore-prefixed and certain other terms
//! let excluded: &[&[u8]] = &[b"_", b"hid"];
//! let mut results: Vec<String> = zipper
//!     .iter_excluding(excluded)
//!     .map(|(path, _)| String::from_utf8(path).unwrap())
//!     .collect();
//!
//! results.sort();
//! assert_eq!(results, vec!["public", "visible"]);
//! ```
//!
//! ## Combined with Prefix Inclusion
//!
//! ```rust
//! use libdictenstein::prelude::*;
//! use libdictenstein::excluding_prefix_zipper::ExcludingPrefixZipper;
//! use libdictenstein::double_array_trie::zipper::DoubleArrayTrieZipper;
//!
//! let terms = vec!["api_v1", "api_v2", "api__internal", "web_v1"];
//! let dict = DoubleArrayTrie::from_terms(terms.iter());
//!
//! let zipper = DoubleArrayTrieZipper::new_from_dict(&dict);
//!
//! // Iterate under "api_" but exclude double-underscore internal APIs
//! let excluded: &[&[u8]] = &[b"api__"];
//! if let Some(iter) = zipper.with_prefix_excluding(b"api_", excluded) {
//!     let mut results: Vec<String> = iter
//!         .map(|(path, _)| String::from_utf8(path).unwrap())
//!         .collect();
//!     results.sort();
//!     assert_eq!(results, vec!["api_v1", "api_v2"]);
//! }
//! ```
//!
//! # Performance
//!
//! - **Subtree pruning**: Excluded subtrees are never visited (O(1) skip per child)
//! - **Per-child cost**: O(e) where e = number of excluded prefixes
//! - **For `\x00` exclusion**: e=1, single-byte check = O(1) per root child
//!
//! # Backend Compatibility
//!
//! Works uniformly across all dictionary backends:
//! - `DoubleArrayTrie` (byte and char variants)
//! - `DynamicDawg` (byte and char variants)
//! - `PathMapDictionary` (byte variant)
//! - `SuffixAutomaton` (byte and char variants)
//!
//! No backend-specific code required - uses generic `DictZipper` API.

use crate::zipper::{DictZipper, ValuedDictZipper};

/// Extension trait for efficient prefix-exclusion-based iteration in dictionaries.
///
/// This trait enables O(n) iteration over dictionary terms while excluding entire
/// subtrees matching specified prefixes at O(1) per prefix per child.
///
/// # Type Parameters
///
/// - `Self: DictZipper` - Any dictionary zipper type
/// - `Self::Unit` - Character unit type (u8 for byte-level, char for character-level)
///
/// # Performance
///
/// - **Per-child check**: O(e) where e = number of excluded prefixes
/// - **Subtree skip**: O(1) - excluded children are never pushed to stack
/// - **Per-result iteration**: O(1) amortized (DFS traversal)
///
/// # Thread Safety
///
/// The returned iterator is Send/Sync if the underlying zipper is Send/Sync.
/// This is automatically satisfied for all standard backends.
pub trait ExcludingPrefixZipper: DictZipper {
    /// Iterate all terms in the dictionary, excluding subtrees matching any prefix
    /// in the exclusion list.
    ///
    /// This method creates an iterator positioned at the root that will yield all
    /// complete terms, skipping any children whose paths match excluded prefixes.
    ///
    /// # Arguments
    ///
    /// * `excluded` - Slice of prefixes to exclude. Each prefix can be any type
    ///   that implements `AsRef<[Self::Unit]>` (e.g., `&[u8]`, `Vec<u8>`, `&str`).
    ///
    /// # Returns
    ///
    /// An iterator yielding `(path, zipper)` pairs for non-excluded terms.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::prelude::*;
    /// use libdictenstein::excluding_prefix_zipper::ExcludingPrefixZipper;
    /// use libdictenstein::double_array_trie::zipper::DoubleArrayTrieZipper;
    ///
    /// let dict = DoubleArrayTrie::from_terms(vec!["hello", "\x00meta", "world"].iter());
    /// let zipper = DoubleArrayTrieZipper::new_from_dict(&dict);
    ///
    /// // Exclude null-prefixed terms
    /// let excluded: &[&[u8]] = &[b"\x00"];
    /// let count = zipper.iter_excluding(excluded).count();
    /// assert_eq!(count, 2); // "hello" and "world", not "\x00meta"
    /// ```
    ///
    /// # Performance
    ///
    /// - Excluded subtrees are pruned during traversal, never visited
    /// - Per-child cost is O(e) for e excluded prefixes
    /// - For typical use with e=1 (null-byte prefix), cost is O(1) per child
    fn iter_excluding<'a, P>(&self, excluded: &'a [P]) -> ExcludingIterator<'a, Self, P>
    where
        P: AsRef<[Self::Unit]>,
        Self: Sized,
    {
        ExcludingIterator::new(self.clone(), excluded)
    }

    /// Navigate to the given prefix and create an iterator over matching terms,
    /// excluding subtrees matching any prefix in the exclusion list.
    ///
    /// Combines prefix navigation with exclusion filtering for efficient
    /// scoped iteration with visibility rules.
    ///
    /// # Arguments
    ///
    /// * `prefix` - Byte/character sequence to navigate to first
    /// * `excluded` - Slice of prefixes to exclude from iteration
    ///
    /// # Returns
    ///
    /// - `Some(iterator)` - If the prefix exists in the dictionary
    /// - `None` - If the prefix doesn't exist
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::prelude::*;
    /// use libdictenstein::excluding_prefix_zipper::ExcludingPrefixZipper;
    /// use libdictenstein::double_array_trie::zipper::DoubleArrayTrieZipper;
    ///
    /// let terms = vec!["api_v1", "api_v2", "api__internal", "web_v1"];
    /// let dict = DoubleArrayTrie::from_terms(terms.iter());
    /// let zipper = DoubleArrayTrieZipper::new_from_dict(&dict);
    ///
    /// // Iterate under "api_" but exclude internal APIs
    /// let excluded: &[&[u8]] = &[b"api__"];
    /// let mut results: Vec<String> = zipper
    ///     .with_prefix_excluding(b"api_", excluded)
    ///     .unwrap()
    ///     .map(|(path, _)| String::from_utf8(path).unwrap())
    ///     .collect();
    ///
    /// results.sort();
    /// assert_eq!(results, vec!["api_v1", "api_v2"]);
    /// ```
    fn with_prefix_excluding<'a, P>(
        &self,
        prefix: &[Self::Unit],
        excluded: &'a [P],
    ) -> Option<ExcludingIterator<'a, Self, P>>
    where
        P: AsRef<[Self::Unit]>,
        Self: Sized,
    {
        // Navigate to prefix position
        let mut zipper = self.clone();
        for &unit in prefix {
            zipper = zipper.descend(unit)?;
        }

        // Prefix exists, create excluding iterator starting from this position
        Some(ExcludingIterator::new(zipper, excluded))
    }
}

/// Blanket implementation: all DictZippers automatically get ExcludingPrefixZipper support.
impl<Z: DictZipper> ExcludingPrefixZipper for Z {}

/// Iterator over all terms, excluding subtrees matching specified prefixes.
///
/// This iterator performs depth-first traversal, filtering children **before**
/// pushing to the stack to achieve O(1) subtree pruning for excluded prefixes.
///
/// # Type Parameters
///
/// - `Z: DictZipper` - The underlying zipper type
/// - `P: AsRef<[Z::Unit]>` - Prefix type for exclusion patterns
///
/// # Iterator Item
///
/// Returns `(Vec<Z::Unit>, Z)`:
/// - `Vec<Z::Unit>` - Complete path (term) as sequence of units
/// - `Z` - Zipper positioned at the final node (useful for further queries)
///
/// # Performance
///
/// - **Amortized per-result**: O(1) - DFS with stack-based traversal
/// - **Per-child filtering**: O(e) where e = number of excluded prefixes
/// - **Memory**: O(d) where d = maximum depth of matching terms
pub struct ExcludingIterator<'a, Z: DictZipper, P> {
    /// DFS traversal stack: stores only zippers (paths reconstructed on demand).
    stack: Vec<Z>,

    /// Prefixes to exclude during traversal.
    excluded_prefixes: &'a [P],
}

impl<'a, Z: DictZipper, P: AsRef<[Z::Unit]>> ExcludingIterator<'a, Z, P> {
    /// Create a new excluding iterator starting from the given zipper position.
    ///
    /// # Arguments
    ///
    /// * `start_zipper` - Zipper at the starting position (usually root or prefix)
    /// * `excluded_prefixes` - Slice of prefixes to exclude
    ///
    /// # Returns
    ///
    /// Iterator ready to yield all non-excluded terms.
    fn new(start_zipper: Z, excluded_prefixes: &'a [P]) -> Self {
        // Pre-allocate stack capacity to avoid reallocations during DFS traversal.
        // Capacity 16 covers typical tree depths (10-15) while avoiding excessive
        // over-allocation.
        let mut stack = Vec::with_capacity(16);

        // Only push if the starting position itself is not excluded
        let start_path = start_zipper.path();
        if !Self::starts_with_any_excluded_static(&start_path, excluded_prefixes) {
            stack.push(start_zipper);
        }

        Self {
            stack,
            excluded_prefixes,
        }
    }

    /// Check if a path starts with any of the excluded prefixes.
    ///
    /// This is the core filtering logic, called for each child before pushing
    /// to the stack. By filtering early, we achieve O(1) subtree pruning.
    ///
    /// # Arguments
    ///
    /// * `path` - The path to check
    ///
    /// # Returns
    ///
    /// `true` if the path matches any excluded prefix, `false` otherwise.
    #[inline]
    fn starts_with_any_excluded(&self, path: &[Z::Unit]) -> bool {
        Self::starts_with_any_excluded_static(path, self.excluded_prefixes)
    }

    /// Static version for use in `new` before `self` is constructed.
    #[inline]
    fn starts_with_any_excluded_static(path: &[Z::Unit], excluded_prefixes: &[P]) -> bool {
        excluded_prefixes.iter().any(|excl| {
            let excl = excl.as_ref();
            path.len() >= excl.len() && path[..excl.len()] == *excl
        })
    }
}

impl<'a, Z: DictZipper, P: AsRef<[Z::Unit]>> Iterator for ExcludingIterator<'a, Z, P> {
    type Item = (Vec<Z::Unit>, Z);

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(zipper) = self.stack.pop() {
            // Push children, SKIPPING excluded subtrees.
            // This is where the O(1) subtree pruning happens - excluded children
            // are never pushed to the stack, so their entire subtrees are skipped.
            for (_label, child) in zipper.children() {
                let child_path = child.path();
                if !self.starts_with_any_excluded(&child_path) {
                    self.stack.push(child);
                }
            }

            // If this is a complete term, return it
            if zipper.is_final() {
                return Some((zipper.path(), zipper));
            }
        }

        None
    }
}

/// Extension of ExcludingPrefixZipper for dictionaries with associated values.
///
/// This trait enables prefix-exclusion iteration that also yields the values
/// associated with matching terms.
///
/// # Type Parameters
///
/// - `Self: ValuedDictZipper` - Any valued dictionary zipper type
/// - `Self::Value` - The value type associated with terms
///
/// # Examples
///
/// ```rust
/// use libdictenstein::prelude::*;
/// use libdictenstein::excluding_prefix_zipper::ValuedExcludingPrefixZipper;
/// use libdictenstein::double_array_trie::zipper::DoubleArrayTrieZipper;
///
/// let terms_with_values = vec![
///     ("cat", 1),
///     ("cats", 2),
///     ("\x00meta", 99),
///     ("dog", 3),
/// ];
/// let dict = DoubleArrayTrie::from_terms_with_values(terms_with_values.into_iter());
/// let zipper = DoubleArrayTrieZipper::new_from_dict(&dict);
///
/// let excluded: &[&[u8]] = &[b"\x00"];
/// let mut results: Vec<(String, usize)> = zipper
///     .iter_values_excluding(excluded)
///     .map(|(path, val)| (String::from_utf8(path).unwrap(), val))
///     .collect();
///
/// results.sort();
/// assert_eq!(results, vec![
///     ("cat".to_string(), 1),
///     ("cats".to_string(), 2),
///     ("dog".to_string(), 3),
/// ]);
/// ```
pub trait ValuedExcludingPrefixZipper: ValuedDictZipper {
    /// Iterate all terms with their values, excluding subtrees matching any prefix
    /// in the exclusion list.
    ///
    /// Similar to `ExcludingPrefixZipper::iter_excluding()` but also returns the
    /// values associated with matching terms.
    ///
    /// # Arguments
    ///
    /// * `excluded` - Slice of prefixes to exclude
    ///
    /// # Returns
    ///
    /// An iterator yielding `(path, value)` pairs for non-excluded terms.
    fn iter_values_excluding<'a, P>(
        &self,
        excluded: &'a [P],
    ) -> ValuedExcludingIterator<'a, Self, P>
    where
        P: AsRef<[Self::Unit]>,
        Self: Sized,
    {
        ValuedExcludingIterator {
            inner: ExcludingIterator::new(self.clone(), excluded),
        }
    }

    /// Navigate to the given prefix and create an iterator over (term, value) pairs,
    /// excluding subtrees matching any prefix in the exclusion list.
    ///
    /// Combines prefix navigation with value extraction and exclusion filtering.
    ///
    /// # Arguments
    ///
    /// * `prefix` - Byte/character sequence to navigate to first
    /// * `excluded` - Slice of prefixes to exclude from iteration
    ///
    /// # Returns
    ///
    /// - `Some(iterator)` - If the prefix exists in the dictionary
    /// - `None` - If the prefix doesn't exist
    fn with_prefix_values_excluding<'a, P>(
        &self,
        prefix: &[Self::Unit],
        excluded: &'a [P],
    ) -> Option<ValuedExcludingIterator<'a, Self, P>>
    where
        P: AsRef<[Self::Unit]>,
        Self: Sized,
    {
        // Navigate to prefix position
        let mut zipper = self.clone();
        for &unit in prefix {
            zipper = zipper.descend(unit)?;
        }

        Some(ValuedExcludingIterator {
            inner: ExcludingIterator::new(zipper, excluded),
        })
    }
}

/// Blanket implementation: all ValuedDictZippers automatically get ValuedExcludingPrefixZipper support.
impl<Z: ValuedDictZipper> ValuedExcludingPrefixZipper for Z {}

/// Iterator over (term, value) pairs, excluding subtrees matching specified prefixes.
///
/// This iterator wraps `ExcludingIterator` and extracts values from final
/// nodes, yielding `(path, value)` tuples.
///
/// # Type Parameters
///
/// - `Z: ValuedDictZipper` - The underlying valued zipper type
/// - `P: AsRef<[Z::Unit]>` - Prefix type for exclusion patterns
///
/// # Iterator Item
///
/// Returns `(Vec<Z::Unit>, Z::Value)`:
/// - `Vec<Z::Unit>` - Complete path (term) as sequence of units
/// - `Z::Value` - Associated value for this term
pub struct ValuedExcludingIterator<'a, Z: ValuedDictZipper, P> {
    inner: ExcludingIterator<'a, Z, P>,
}

impl<'a, Z: ValuedDictZipper, P: AsRef<[Z::Unit]>> Iterator for ValuedExcludingIterator<'a, Z, P> {
    type Item = (Vec<Z::Unit>, Z::Value);

    fn next(&mut self) -> Option<Self::Item> {
        // Get next term from inner iterator
        let (path, zipper) = self.inner.next()?;

        // Extract value from final node
        let value = zipper.value()?;

        Some((path, value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::double_array_trie::zipper::DoubleArrayTrieZipper;
    use crate::double_array_trie::DoubleArrayTrie;

    #[test]
    fn test_single_exclusion_null_prefix() {
        let terms = vec!["\x00meta", "\x00index", "hello", "world"];
        let dict = DoubleArrayTrie::from_terms(terms.iter());

        let zipper = DoubleArrayTrieZipper::new_from_dict(&dict);
        let excluded: &[&[u8]] = &[b"\x00"];
        let mut results: Vec<String> = zipper
            .iter_excluding(excluded)
            .map(|(path, _)| String::from_utf8(path).unwrap())
            .collect();

        results.sort();
        assert_eq!(results, vec!["hello", "world"]);
    }

    #[test]
    fn test_empty_exclusion_returns_all() {
        let terms = vec!["cat", "dog", "fish"];
        let dict = DoubleArrayTrie::from_terms(terms.iter());

        let zipper = DoubleArrayTrieZipper::new_from_dict(&dict);
        let excluded: &[&[u8]] = &[];
        let mut results: Vec<String> = zipper
            .iter_excluding(excluded)
            .map(|(path, _)| String::from_utf8(path).unwrap())
            .collect();

        results.sort();
        assert_eq!(results, vec!["cat", "dog", "fish"]);
    }

    #[test]
    fn test_multiple_exclusions() {
        let terms = vec!["_internal", "_hidden", "public", "visible", "hidden_file"];
        let dict = DoubleArrayTrie::from_terms(terms.iter());

        let zipper = DoubleArrayTrieZipper::new_from_dict(&dict);
        let excluded: &[&[u8]] = &[b"_", b"hid"];
        let mut results: Vec<String> = zipper
            .iter_excluding(excluded)
            .map(|(path, _)| String::from_utf8(path).unwrap())
            .collect();

        results.sort();
        assert_eq!(results, vec!["public", "visible"]);
    }

    #[test]
    fn test_with_prefix_excluding() {
        let terms = vec!["api_v1", "api_v2", "api__internal", "web_v1"];
        let dict = DoubleArrayTrie::from_terms(terms.iter());

        let zipper = DoubleArrayTrieZipper::new_from_dict(&dict);
        let excluded: &[&[u8]] = &[b"api__"];
        let mut results: Vec<String> = zipper
            .with_prefix_excluding(b"api_", excluded)
            .unwrap()
            .map(|(path, _)| String::from_utf8(path).unwrap())
            .collect();

        results.sort();
        assert_eq!(results, vec!["api_v1", "api_v2"]);
    }

    #[test]
    fn test_valued_excluding_iterator() {
        let terms_with_values = vec![("cat", 1usize), ("cats", 2), ("\x00meta", 99), ("dog", 3)];
        let dict = DoubleArrayTrie::from_terms_with_values(terms_with_values.into_iter());

        let zipper = DoubleArrayTrieZipper::new_from_dict(&dict);
        let excluded: &[&[u8]] = &[b"\x00"];
        let mut results: Vec<(String, usize)> = zipper
            .iter_values_excluding(excluded)
            .map(|(path, val)| (String::from_utf8(path).unwrap(), val))
            .collect();

        results.sort();
        assert_eq!(
            results,
            vec![
                ("cat".to_string(), 1),
                ("cats".to_string(), 2),
                ("dog".to_string(), 3),
            ]
        );
    }
}
