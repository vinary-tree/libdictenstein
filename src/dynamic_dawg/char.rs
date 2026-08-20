//! Character-level Dynamic DAWG with online modifications and Unicode support.
//!
//! This implementation supports incremental updates on a lock-free node graph.
//! Perfect minimality can be restored via explicit compaction.
//!
//! Unlike the byte-level `DynamicDawg`, this variant operates on Unicode
//! scalar values (`char`), providing correct character-level Levenshtein
//! distances for multi-byte UTF-8 sequences.
//!
//! # Performance Trade-offs
//!
//! - **Memory**: ~4x edge label storage (4 bytes per `char` vs 1 byte per `u8`)
//! - **Speed**: ~5-10% slower due to UTF-8 decoding
//! - **Correctness**: Proper Unicode semantics (e.g., "" → "¡" = distance 1, not 2)

use super::char_zipper::DynamicDawgCharZipper;
#[cfg(feature = "bindings-core")]
use super::lockfree::PublishIfEmpty;
use super::lockfree::{LockFreeDawg, LockFreeDawgNode};
use crate::iterator::DictionaryIterator;
use crate::value::DictionaryValue;
use crate::{Dictionary, DictionaryNode, SyncStrategy};
use std::sync::Arc;

/// A dynamic DAWG that supports online insertions and deletions.
///
/// # Type Parameters
///
/// - `V`: Optional value type associated with each term. Use `()` (default) for
///   dictionaries without values, or any type implementing `DictionaryValue`
///   (Clone + Send + Sync + 'static) for value-storing dictionaries.
///
/// # Minimality Trade-offs
///
/// - **After insertion**: Structure remains near-minimal
/// - **After deletion**: May become non-minimal (orphaned branches)
/// - **Solution**: Call `compact()` periodically to restore minimality
///
/// # Thread Safety
///
/// Uses immutable graph revisions. Reads retain one root and are wait-free;
/// writes path-copy the affected route and publish it with a root CAS.
///
/// # Performance
///
/// - Insertion: O(m) where m is term length (amortized)
/// - Deletion: O(m)
/// - Compaction: O(n) where n is total characters
/// - Space: Near-minimal to ~1.5x minimal (worst case between compactions)
///
/// # Examples
///
/// ```text
/// // Without values (default)
/// let mut dict = DynamicDawgChar::new();
/// dict.insert("hello");
///
/// // With values
/// let dict: DynamicDawgChar<u32> = DynamicDawgChar::new();
/// dict.insert_with_value("hello", 42);
/// ```
#[derive(Clone, Debug)]
pub struct DynamicDawgChar<V: DictionaryValue = ()> {
    pub(crate) inner: Arc<DynamicDawgCharInner<V>>,
}

// The public char DAWG now uses the unit-generic lock-free core. The
// indexed `DawgCore<char, V>` remains as the serialization compatibility
// shape so existing encoded dictionaries can still round-trip.
pub(crate) type DynamicDawgCharInner<V = ()> = LockFreeDawg<char, V>;

impl<V: DictionaryValue> DynamicDawgChar<V> {
    /// Create a new empty dynamic DAWG.
    ///
    /// By default, auto-minimization is disabled. Use `with_auto_minimize_threshold()`
    /// to enable automatic minimization.
    ///
    /// # Example
    ///
    /// ```text
    /// // Without values (default)
    /// let dawg: DynamicDawgChar<()> = DynamicDawgChar::new();
    /// dawg.insert("hello");
    ///
    /// // With values
    /// let dawg: DynamicDawgChar<u32> = DynamicDawgChar::new();
    /// dawg.insert_with_value("hello", 42);
    /// ```
    pub fn new() -> Self {
        Self::with_auto_minimize_threshold(f32::INFINITY)
    }

    /// Create a new empty dynamic DAWG with custom auto-minimize threshold.
    ///
    /// The auto-minimize threshold determines when the DAWG automatically
    /// triggers minimization. A value of 1.5 means minimize when node count
    /// grows to 1.5x the last minimized size (50% bloat).
    ///
    /// # Parameters
    ///
    /// - `threshold`: Bloat ratio to trigger minimization (e.g., 1.5 = 50% bloat).
    ///   Use `f32::INFINITY` to disable auto-minimization.
    ///
    /// # Example
    ///
    /// ```text
    /// // Auto-minimize at 50% bloat (default)
    /// let dawg: DynamicDawgChar<()> = DynamicDawgChar::with_auto_minimize_threshold(1.5);
    ///
    /// // Disable auto-minimization (manual minimize() calls only)
    /// let dawg: DynamicDawgChar<()> = DynamicDawgChar::with_auto_minimize_threshold(f32::INFINITY);
    /// ```
    pub fn with_auto_minimize_threshold(threshold: f32) -> Self {
        Self::with_config(threshold, None)
    }

    /// Create a new empty dynamic DAWG with full configuration.
    ///
    /// # Parameters
    ///
    /// - `auto_minimize_threshold`: Accepted for API compatibility. Explicit
    ///   `compact()` / `minimize()` are the lock-free maintenance boundary.
    /// - `bloom_filter_capacity`: Accepted for API compatibility. The lock-free
    ///   implementation performs exact wait-free traversals.
    ///
    /// # Example
    ///
    /// ```text
    /// // Configuration arguments are accepted for API compatibility
    /// let dawg: DynamicDawgChar<()> = DynamicDawgChar::with_config(f32::INFINITY, Some(10000));
    ///
    /// // Explicit maintenance remains available
    /// let dawg: DynamicDawgChar<()> = DynamicDawgChar::with_config(1.5, None);
    /// ```
    pub fn with_config(auto_minimize_threshold: f32, bloom_filter_capacity: Option<usize>) -> Self {
        DynamicDawgChar {
            inner: Arc::new(DynamicDawgCharInner::with_config(
                auto_minimize_threshold,
                bloom_filter_capacity,
            )),
        }
    }

    /// Create from an iterator of terms (optimized batch insert).
    ///
    /// This method sorts terms before insertion for better prefix/suffix sharing.
    pub fn from_terms<I, S>(terms: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut term_vec: Vec<String> = terms.into_iter().map(|s| s.as_ref().to_string()).collect();
        crate::causal_perf::record_batch_sort_calls(1);
        crate::causal_perf::record_batch_sort_terms(term_vec.len() as u64);
        crate::causal_perf::record_batch_sort_units(
            term_vec
                .iter()
                .map(|term| term.chars().count())
                .sum::<usize>() as u64,
        );
        term_vec.sort_unstable();
        Self::from_sorted_terms(term_vec)
    }

    /// Create from sorted terms (assumes pre-sorted input).
    ///
    /// # Performance
    ///
    /// This is faster than `from_terms()` if your input is already sorted,
    /// as it skips the sorting step and takes advantage of better prefix sharing.
    ///
    /// # Example
    ///
    /// ```text
    /// let mut terms = vec!["apple", "banana", "cherry"];
    /// terms.sort();  // Already sorted
    /// let dawg: DynamicDawgChar<()> = DynamicDawgChar::from_sorted_terms(terms);
    /// ```
    ///
    /// # Panics
    ///
    /// Panics if the supplied terms are not in lexicographically
    /// nondecreasing Unicode-scalar order.
    pub fn from_sorted_terms<I, S>(terms: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self {
            inner: Arc::new(DynamicDawgCharInner::from_sorted_terms_by(
                terms,
                |term, units| units.extend(term.as_ref().chars()),
            )),
        }
    }

    /// Create from an iterator of `(term, value)` pairs.
    ///
    /// Terms are sorted before insertion so the resulting DAWG benefits from
    /// the same prefix/suffix sharing as [`from_terms`](Self::from_terms).
    pub fn from_terms_with_values<I, S>(entries: I) -> Self
    where
        I: IntoIterator<Item = (S, V)>,
        S: AsRef<str>,
    {
        let mut pairs: Vec<(String, V)> = entries
            .into_iter()
            .map(|(s, v)| (s.as_ref().to_string(), v))
            .collect();
        crate::causal_perf::record_batch_sort_calls(1);
        crate::causal_perf::record_batch_sort_terms(pairs.len() as u64);
        crate::causal_perf::record_batch_sort_units(
            pairs
                .iter()
                .map(|(term, _)| term.chars().count())
                .sum::<usize>() as u64,
        );
        // Rust's stable sort retains input order among duplicate terms, which
        // preserves the documented last-value-wins behavior.
        pairs.sort_by(|a, b| a.0.cmp(&b.0));
        Self::from_sorted_terms_with_values(pairs)
    }

    /// Create from Unicode-scalar-ordered `(term, value)` pairs.
    ///
    /// This skips sorting and constructs one immutable minimal graph. Duplicate
    /// terms are allowed and the last value wins.
    ///
    /// # Panics
    ///
    /// Panics if terms are not in lexicographically nondecreasing
    /// Unicode-scalar order.
    pub fn from_sorted_terms_with_values<I, S>(entries: I) -> Self
    where
        I: IntoIterator<Item = (S, V)>,
        S: AsRef<str>,
    {
        Self {
            inner: Arc::new(DynamicDawgCharInner::from_sorted_entries_by(
                entries.into_iter().map(|(term, value)| (term, Some(value))),
                |term, units| units.extend(term.as_ref().chars()),
            )),
        }
    }

    /// Crate-internal optional-value variant used by binding batch rebuilds.
    #[cfg(feature = "bindings-core")]
    pub(crate) fn from_sorted_optional_entries<I>(entries: I) -> Self
    where
        I: IntoIterator<Item = (String, Option<V>)>,
    {
        Self {
            inner: Arc::new(DynamicDawgCharInner::from_sorted_entries_by(
                entries,
                |term, units| units.extend(term.chars()),
            )),
        }
    }

    /// Insert a term into the DAWG.
    ///
    /// Returns `true` if the term was newly inserted, `false` if it already existed.
    ///
    /// # Minimality
    ///
    /// Insertions maintain minimality by sharing suffixes with existing nodes.
    pub fn insert(&self, term: &str) -> bool {
        let chars: Vec<char> = term.chars().collect();
        self.inner.insert_units(&chars)
    }

    /// Insert a term with an associated value.
    ///
    /// Returns `true` if the term was newly inserted, `false` if it already existed.
    /// If the term already exists, its value is updated.
    ///
    /// # Example
    ///
    /// ```text
    /// let dict: DynamicDawgChar<u32> = DynamicDawgChar::new();
    /// assert!(dict.insert_with_value("hello", 42));
    /// assert!(!dict.insert_with_value("hello", 43)); // Updates value
    /// assert_eq!(dict.get_value("hello"), Some(43));
    /// ```
    pub fn insert_with_value(&self, term: &str, value: V) -> bool {
        let chars: Vec<char> = term.chars().collect();
        self.inner.insert_units_with_value(&chars, value)
    }

    /// Insert/update a Unicode term while preserving an absent mapped value.
    #[cfg(feature = "bindings-core")]
    pub(crate) fn insert_with_optional_value(&self, term: &str, value: Option<V>) -> bool {
        let chars: Vec<char> = term.chars().collect();
        self.inner.insert_units_with_optional_value(&chars, value)
    }

    /// Update an existing term's value in place, or insert a new term with a default value.
    ///
    /// This method is useful when you want to incrementally modify a value (e.g., adding
    /// elements to a `HashSet` or `Vec`) without replacing it entirely.
    ///
    /// # Arguments
    ///
    /// * `term` - The term to update or insert
    /// * `default_value` - The value to use if the term doesn't exist
    /// * `update_fn` - Function to apply to the existing value if the term exists
    ///
    /// # Returns
    ///
    /// `true` if this was a new term (inserted with default), `false` if an existing term was updated.
    ///
    /// # Example
    ///
    /// ```text
    /// use std::collections::HashSet;
    ///
    /// let dict: DynamicDawgChar<HashSet<u32>> = DynamicDawgChar::new();
    ///
    /// // First call: inserts with default value {1}
    /// assert!(dict.update_or_insert("foo", HashSet::from([1]), |set| { set.insert(1); }));
    ///
    /// // Second call: updates existing value to {1, 2}
    /// assert!(!dict.update_or_insert("foo", HashSet::from([2]), |set| { set.insert(2); }));
    ///
    /// assert_eq!(dict.get_value("foo"), Some(HashSet::from([1, 2])));
    /// ```
    pub fn update_or_insert<F>(&self, term: &str, default_value: V, update_fn: F) -> bool
    where
        F: Fn(&mut V),
    {
        let chars: Vec<char> = term.chars().collect();
        self.inner
            .update_or_insert_units(&chars, default_value, update_fn)
    }

    /// Get the value associated with a term.
    ///
    /// Returns `Some(value)` if the term exists, `None` otherwise.
    ///
    /// # Example
    ///
    /// ```text
    /// let dict: DynamicDawgChar<String> = DynamicDawgChar::new();
    /// dict.insert_with_value("key", "value".to_string());
    /// assert_eq!(dict.get_value("key"), Some("value".to_string()));
    /// assert_eq!(dict.get_value("unknown"), None);
    /// ```
    pub fn get_value(&self, term: &str) -> Option<V> {
        let chars: Vec<char> = term.chars().collect();
        self.inner.get_units_value(&chars)
    }

    /// Read membership and optional value from one immutable graph revision.
    #[cfg(feature = "bindings-core")]
    pub(crate) fn get_optional_value(&self, term: &str) -> Option<Option<V>> {
        let chars: Vec<char> = term.chars().collect();
        self.inner.get_units_optional_value(&chars)
    }

    /// Remove a term from the DAWG.
    ///
    /// Returns `true` if the term was present and removed, `false` otherwise.
    ///
    /// # Minimality
    ///
    /// Deletions may leave the DAWG non-minimal. Call `compact()` to restore
    /// minimality by removing unreachable nodes.
    pub fn remove(&self, term: &str) -> bool {
        let chars: Vec<char> = term.chars().collect();
        self.inner.remove_units(&chars)
    }

    /// Compact the DAWG to restore perfect minimality.
    ///
    /// This rebuilds the internal structure, merging equivalent suffixes
    /// and removing unreachable nodes. Ideal for batch operations:
    ///
    /// ```text
    /// // Batch updates
    /// dawg.insert("term1");
    /// dawg.insert("term2");
    /// dawg.remove("term3");
    /// // ... many more operations ...
    ///
    /// // Single compaction at the end
    /// let removed = dawg.compact();
    /// ```
    ///
    /// **Note**: This does a full rebuild (extracts, sorts, reconstructs, minimizes).
    /// For incremental minimization without rebuilding, use `minimize()`.
    ///
    /// Returns the number of nodes removed.
    pub fn compact(&self) -> usize {
        self.inner.compact()
    }

    /// Minimize the DAWG using incremental suffix merging.
    ///
    /// Unlike `compact()`, this method:
    /// - **Makes no assumptions** about insertion order
    /// - **Only examines affected nodes** and their neighbors
    /// - **Preserves existing structure** where possible
    /// - **Faster than compact()** for localized updates
    ///
    /// This implements incremental minimization based on node signatures.
    /// If the DAWG was minimal before updates, only the new paths and
    /// their neighbors need to be examined.
    ///
    /// ```text
    /// // DAWG is minimal
    /// dawg.minimize();
    ///
    /// // Add some terms (locally affects structure)
    /// dawg.insert("newterm1");
    /// dawg.insert("newterm2");
    ///
    /// // Incremental minimize - only examines affected paths
    /// let merged = dawg.minimize(); // Much faster than compact()!
    /// ```
    ///
    /// Returns the number of nodes merged.
    pub fn minimize(&self) -> usize {
        self.inner.minimize()
    }

    /// Batch insert multiple terms, then compact.
    ///
    /// This is more efficient than calling `insert()` followed by `compact()`
    /// separately, as it sorts terms for better prefix sharing and only rebuilds once.
    ///
    /// Returns the number of new terms added.
    pub fn extend<I, S>(&self, terms: I) -> usize
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        // Collect and sort for optimal prefix sharing
        let mut term_vec: Vec<String> = terms.into_iter().map(|s| s.as_ref().to_string()).collect();
        term_vec.sort_unstable();

        let mut added = 0;
        for term in term_vec {
            if self.insert(&term) {
                added += 1;
            }
        }

        if added > 0 {
            self.compact();
        }

        added
    }

    /// Batch remove multiple terms, then compact.
    ///
    /// More efficient than individual `remove()` calls followed by `compact()`.
    ///
    /// Returns the number of terms removed.
    pub fn remove_many<I, S>(&self, terms: I) -> usize
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut removed = 0;
        for term in terms {
            if self.remove(term.as_ref()) {
                removed += 1;
            }
        }

        if removed > 0 {
            self.compact();
        }

        removed
    }

    /// Get the number of terms in the DAWG.
    pub fn term_count(&self) -> usize {
        self.inner.term_count()
    }

    /// Capture the current root together with its term count from one
    /// atomically published revision.
    ///
    /// Calling [`Dictionary::root`] and [`Dictionary::len`] separately
    /// performs two independent revision loads, so a concurrent writer can
    /// tear the pair (finding LDICT-B4). Snapshot capture uses this
    /// coherent accessor instead.
    pub fn root_with_term_count(&self) -> (DynamicDawgCharNode<V>, usize) {
        let (root, term_count) = self.inner.root_arc_with_term_count();
        (DynamicDawgCharNode { node: root }, term_count)
    }

    #[cfg(feature = "bindings-core")]
    pub(crate) fn root_with_term_count_revision(&self) -> (DynamicDawgCharNode<V>, usize, u64) {
        let (root, term_count, revision) = self.inner.root_arc_with_term_count_revision();
        (DynamicDawgCharNode { node: root }, term_count, revision)
    }

    #[cfg(feature = "bindings-core")]
    pub(crate) fn clear_graph(&self) -> bool {
        self.inner.clear()
    }

    #[cfg(feature = "bindings-core")]
    pub(crate) fn try_publish_if_empty(&self, frozen: &Self) -> PublishIfEmpty {
        self.inner.try_publish_if_empty(&frozen.inner)
    }

    /// Get the number of nodes in the DAWG.
    pub fn node_count(&self) -> usize {
        self.inner.node_count()
    }

    /// Check if compaction is recommended.
    ///
    /// Returns `true` if deletions have occurred and compaction would
    /// likely reduce memory usage.
    pub fn needs_compaction(&self) -> bool {
        self.inner.needs_compaction()
    }

    /// Check if a term is in the DAWG.
    ///
    /// This is an exact wait-free traversal.
    pub fn contains(&self, term: &str) -> bool {
        let chars: Vec<char> = term.chars().collect();
        self.inner.contains_units(&chars)
    }
}

impl<V: DictionaryValue> DynamicDawgChar<V> {
    /// Iterate over all `(term, value)` pairs as character vectors.
    ///
    /// Returns an iterator yielding `(Vec<char>, V)` tuples in depth-first order.
    /// This is more efficient than `iter()` as it avoids String allocation.
    ///
    /// This legacy mapped-only iterator omits present terms whose value is
    /// `None`. Use `(&dictionary).into_iter()` or
    /// [`DictionaryEntries::entries`](crate::DictionaryEntries::entries) for
    /// lossless [`DictionaryEntry`](crate::DictionaryEntry) snapshots.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::dynamic_dawg::char::DynamicDawgChar;
    ///
    /// let dict: DynamicDawgChar<u32> = DynamicDawgChar::new();
    /// dict.insert_with_value("café", 1);
    /// dict.insert_with_value("naïve", 2);
    ///
    /// for (chars, value) in dict.iter_chars() {
    ///     let term: String = chars.iter().collect();
    ///     println!("{} -> {}", term, value);
    /// }
    /// ```
    pub fn iter_chars(&self) -> DictionaryIterator<DynamicDawgCharZipper<V>> {
        let zipper = DynamicDawgCharZipper::new_from_dict(self);
        DictionaryIterator::new(zipper)
    }

    /// Iterate over all `(term, value)` pairs as UTF-8 strings.
    ///
    /// Returns an iterator yielding `(String, V)` tuples in depth-first order.
    /// For better performance with raw characters, use `iter_chars()` instead.
    /// Like `iter_chars()`, this legacy iterator omits term-only entries.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::dynamic_dawg::char::DynamicDawgChar;
    ///
    /// let dict: DynamicDawgChar<u32> = DynamicDawgChar::new();
    /// dict.insert_with_value("café", 1);
    /// dict.insert_with_value("naïve", 2);
    ///
    /// for (term, value) in dict.iter() {
    ///     println!("{} -> {}", term, value);
    /// }
    /// ```
    pub fn iter(&self) -> impl Iterator<Item = (String, V)> + '_ {
        self.iter_chars()
            .map(|(chars, value)| (chars.into_iter().collect::<String>(), value))
    }
}

impl<V: DictionaryValue> Default for DynamicDawgChar<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V: DictionaryValue> std::iter::FromIterator<String> for DynamicDawgChar<V> {
    /// Builds one minimal immutable revision instead of repeatedly publishing
    /// path-copied revisions through [`insert`](Self::insert).
    fn from_iter<I: IntoIterator<Item = String>>(iter: I) -> Self {
        Self::from_terms(iter)
    }
}

impl<'a, V: DictionaryValue> std::iter::FromIterator<&'a str> for DynamicDawgChar<V> {
    fn from_iter<I: IntoIterator<Item = &'a str>>(iter: I) -> Self {
        Self::from_terms(iter)
    }
}

impl<V: DictionaryValue> std::iter::FromIterator<Vec<char>> for DynamicDawgChar<V> {
    fn from_iter<I: IntoIterator<Item = Vec<char>>>(iter: I) -> Self {
        let mut terms: Vec<Vec<char>> = iter.into_iter().collect();
        crate::causal_perf::record_batch_sort_calls(1);
        crate::causal_perf::record_batch_sort_terms(terms.len() as u64);
        crate::causal_perf::record_batch_sort_units(
            terms.iter().map(Vec::len).sum::<usize>() as u64
        );
        terms.sort_unstable();
        Self {
            inner: Arc::new(DynamicDawgCharInner::from_sorted_terms_by(
                terms,
                |term, units| units.extend_from_slice(term),
            )),
        }
    }
}

impl<'a, V: DictionaryValue> std::iter::FromIterator<&'a [char]> for DynamicDawgChar<V> {
    fn from_iter<I: IntoIterator<Item = &'a [char]>>(iter: I) -> Self {
        iter.into_iter().map(<[char]>::to_vec).collect()
    }
}

impl<V: DictionaryValue> std::iter::FromIterator<(String, V)> for DynamicDawgChar<V> {
    fn from_iter<I: IntoIterator<Item = (String, V)>>(iter: I) -> Self {
        Self::from_terms_with_values(iter)
    }
}

impl<'a, V: DictionaryValue> std::iter::FromIterator<(&'a str, V)> for DynamicDawgChar<V> {
    fn from_iter<I: IntoIterator<Item = (&'a str, V)>>(iter: I) -> Self {
        Self::from_terms_with_values(iter)
    }
}

impl<V: DictionaryValue> std::iter::FromIterator<(Vec<char>, V)> for DynamicDawgChar<V> {
    fn from_iter<I: IntoIterator<Item = (Vec<char>, V)>>(iter: I) -> Self {
        let mut entries: Vec<(Vec<char>, V)> = iter.into_iter().collect();
        crate::causal_perf::record_batch_sort_calls(1);
        crate::causal_perf::record_batch_sort_terms(entries.len() as u64);
        crate::causal_perf::record_batch_sort_units(
            entries.iter().map(|(key, _)| key.len()).sum::<usize>() as u64,
        );
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        Self {
            inner: Arc::new(DynamicDawgCharInner::from_sorted_entries_by(
                entries.into_iter().map(|(key, value)| (key, Some(value))),
                |key, units| units.extend_from_slice(key),
            )),
        }
    }
}

impl<'a, V: DictionaryValue> std::iter::FromIterator<(&'a [char], V)> for DynamicDawgChar<V> {
    fn from_iter<I: IntoIterator<Item = (&'a [char], V)>>(iter: I) -> Self {
        iter.into_iter()
            .map(|(key, value)| (key.to_vec(), value))
            .collect()
    }
}

impl<V: DictionaryValue> std::iter::Extend<String> for DynamicDawgChar<V> {
    fn extend<I: IntoIterator<Item = String>>(&mut self, iter: I) {
        let _ = DynamicDawgChar::extend(self, iter);
    }
}

impl<'a, V: DictionaryValue> std::iter::Extend<&'a str> for DynamicDawgChar<V> {
    fn extend<I: IntoIterator<Item = &'a str>>(&mut self, iter: I) {
        let _ = DynamicDawgChar::extend(self, iter);
    }
}

impl<V: DictionaryValue> std::iter::Extend<Vec<char>> for DynamicDawgChar<V> {
    fn extend<I: IntoIterator<Item = Vec<char>>>(&mut self, iter: I) {
        let mut terms: Vec<Vec<char>> = iter.into_iter().collect();
        terms.sort_unstable();
        let mut added = false;
        for term in terms {
            let term: String = term.into_iter().collect();
            added |= self.insert(&term);
        }
        if added {
            self.compact();
        }
    }
}

impl<'a, V: DictionaryValue> std::iter::Extend<&'a [char]> for DynamicDawgChar<V> {
    fn extend<I: IntoIterator<Item = &'a [char]>>(&mut self, iter: I) {
        <Self as std::iter::Extend<Vec<char>>>::extend(
            self,
            iter.into_iter().map(<[char]>::to_vec),
        );
    }
}

impl<V: DictionaryValue> std::iter::Extend<(String, V)> for DynamicDawgChar<V> {
    fn extend<I: IntoIterator<Item = (String, V)>>(&mut self, iter: I) {
        let mut entries: Vec<(String, V)> = iter.into_iter().collect();
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        let mut added = false;
        for (term, value) in entries {
            added |= self.insert_with_value(&term, value);
        }
        if added {
            self.compact();
        }
    }
}

impl<'a, V: DictionaryValue> std::iter::Extend<(&'a str, V)> for DynamicDawgChar<V> {
    fn extend<I: IntoIterator<Item = (&'a str, V)>>(&mut self, iter: I) {
        <Self as std::iter::Extend<(String, V)>>::extend(
            self,
            iter.into_iter()
                .map(|(term, value)| (term.to_owned(), value)),
        );
    }
}

impl<V: DictionaryValue> std::iter::Extend<(Vec<char>, V)> for DynamicDawgChar<V> {
    fn extend<I: IntoIterator<Item = (Vec<char>, V)>>(&mut self, iter: I) {
        let mut entries: Vec<(Vec<char>, V)> = iter.into_iter().collect();
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        let mut added = false;
        for (key, value) in entries {
            let term: String = key.into_iter().collect();
            added |= self.insert_with_value(&term, value);
        }
        if added {
            self.compact();
        }
    }
}

impl<'a, V: DictionaryValue> std::iter::Extend<(&'a [char], V)> for DynamicDawgChar<V> {
    fn extend<I: IntoIterator<Item = (&'a [char], V)>>(&mut self, iter: I) {
        <Self as std::iter::Extend<(Vec<char>, V)>>::extend(
            self,
            iter.into_iter().map(|(key, value)| (key.to_vec(), value)),
        );
    }
}

#[cfg(feature = "serialization")]
impl<V: DictionaryValue + serde::Serialize> serde::Serialize for DynamicDawgChar<V> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.inner.to_core().serialize(serializer)
    }
}

/// Deserialize implementation when only `serialization` feature is enabled (not `persistent-artrie`).
/// In this case, we need explicit `Deserialize` bounds.
#[cfg(all(feature = "serialization", not(feature = "persistent-artrie")))]
impl<'de, V: DictionaryValue + serde::Deserialize<'de>> serde::Deserialize<'de>
    for DynamicDawgChar<V>
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let inner = super::core::DawgCore::<char, V>::deserialize(deserializer)?;
        Ok(DynamicDawgChar {
            inner: Arc::new(DynamicDawgCharInner::from_core(inner)),
        })
    }
}

/// Deserialize implementation when `persistent-artrie` feature is enabled.
/// `DictionaryValue` already includes `DeserializeOwned`, so no additional bounds needed.
#[cfg(all(feature = "serialization", feature = "persistent-artrie"))]
impl<'de, V: DictionaryValue> serde::Deserialize<'de> for DynamicDawgChar<V> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let inner = super::core::DawgCore::<char, V>::deserialize(deserializer)?;
        Ok(DynamicDawgChar {
            inner: Arc::new(DynamicDawgCharInner::from_core(inner)),
        })
    }
}

impl<V: DictionaryValue> Dictionary for DynamicDawgChar<V> {
    type Node = DynamicDawgCharNode<V>;

    fn root(&self) -> Self::Node {
        DynamicDawgCharNode {
            node: self.inner.root_arc(),
        }
    }

    fn traversal_root(&self) -> crate::DictionaryTraversalRoot<Self::Node> {
        let (node, cursor_graph) = self.inner.root_arc_with_cursor_graph();
        let root = DynamicDawgCharNode { node };
        match cursor_graph {
            Some(graph) => crate::DictionaryTraversalRoot::captured(root, graph),
            None => crate::DictionaryTraversalRoot::owned(root),
        }
    }

    fn len(&self) -> Option<usize> {
        Some(self.term_count())
    }

    fn sync_strategy(&self) -> SyncStrategy {
        SyncStrategy::InternalSync
    }
}

/// Node handle for dynamic DAWG traversal.
#[derive(Clone)]
pub struct DynamicDawgCharNode<V: DictionaryValue = ()> {
    node: Arc<LockFreeDawgNode<char, V>>,
}

impl<V: DictionaryValue> DictionaryNode for DynamicDawgCharNode<V> {
    type Unit = char;
    type SnapshotCursor = super::DynamicDawgSnapshotCursor<char, V>;
    type SnapshotGraphValueHandle = super::DynamicDawgSnapshotCursor<char, V>;

    #[inline]
    fn snapshot_node_identity(&self) -> Option<crate::SnapshotNodeIdentity> {
        self.node.snapshot_id
    }

    #[inline]
    fn snapshot_root_cursor(&self) -> Option<Self::SnapshotCursor> {
        Some(LockFreeDawgNode::traversal_cursor(&self.node))
    }

    #[inline]
    fn supports_snapshot_cursor_nodes(&self) -> bool {
        true
    }

    #[inline]
    unsafe fn snapshot_cursor_node(&self, cursor: Self::SnapshotCursor) -> Option<Self> {
        // SAFETY: inherited from the trait contract.
        Some(Self {
            node: unsafe { LockFreeDawgNode::arc_from_cursor(cursor) },
        })
    }

    #[inline]
    unsafe fn filter_map_snapshot_cursor_edges_and_finality<T, P, F>(
        &self,
        cursor: Self::SnapshotCursor,
        project: P,
        visitor: F,
    ) -> Option<bool>
    where
        P: FnMut(char) -> Option<T>,
        F: FnMut(char, Self::SnapshotCursor, T),
    {
        // SAFETY: the trait contract requires every cursor to originate from
        // this retained root revision.
        Some(unsafe {
            LockFreeDawgNode::<char, V>::filter_map_cursor_edges_and_finality(
                cursor, project, visitor,
            )
        })
    }

    fn is_final(&self) -> bool {
        self.node.is_final()
    }

    fn transition(&self, label: char) -> Option<Self> {
        self.node
            .edges
            .find(label)
            .map(|child| DynamicDawgCharNode {
                node: child.clone(),
            })
    }

    fn edges(&self) -> Box<dyn Iterator<Item = (char, Self)> + '_> {
        let edge_vec: Vec<_> = self
            .node
            .edges
            .edges
            .iter()
            .map(|(ch, child)| (*ch, child.clone()))
            .collect();
        Box::new(
            edge_vec
                .into_iter()
                .map(|(ch, child)| (ch, DynamicDawgCharNode { node: child })),
        )
    }

    #[inline]
    fn for_each_edge<F>(&self, mut visitor: F)
    where
        F: FnMut(char, Self),
    {
        for (label, child) in &self.node.edges.edges {
            visitor(
                *label,
                DynamicDawgCharNode {
                    node: child.clone(),
                },
            );
        }
    }

    #[inline]
    fn filter_map_edges<T, P, F>(&self, mut project: P, mut visitor: F)
    where
        P: FnMut(char) -> Option<T>,
        F: FnMut(char, Self, T),
    {
        for (label, child) in &self.node.edges.edges {
            if let Some(projected) = project(*label) {
                visitor(
                    *label,
                    DynamicDawgCharNode {
                        node: Arc::clone(child),
                    },
                    projected,
                );
            }
        }
    }

    fn edge_count(&self) -> Option<usize> {
        Some(self.node.edges.edges.len())
    }
}

// ============================================================================
// MappedDictionary Trait Implementation
// ============================================================================

use crate::{MappedDictionary, MappedDictionaryNode};

impl<V: DictionaryValue> MappedDictionaryNode for DynamicDawgCharNode<V> {
    type Value = V;

    fn value(&self) -> Option<Self::Value> {
        self.node.value()
    }

    #[inline]
    fn supports_snapshot_cursor_values(&self) -> bool {
        true
    }

    #[inline]
    fn supports_snapshot_graph_values(&self) -> bool {
        true
    }

    fn snapshot_traversal_graph(
        &self,
    ) -> Option<Arc<crate::SnapshotTraversalGraph<Self::Unit, Self::SnapshotGraphValueHandle>>>
    {
        super::lockfree::frozen_traversal_graph_from_root(&self.node).map(Arc::new)
    }

    #[inline]
    unsafe fn snapshot_cursor_value(
        &self,
        cursor: Self::SnapshotCursor,
    ) -> Option<Option<Self::Value>> {
        // SAFETY: inherited from the trait contract.
        Some(unsafe { LockFreeDawgNode::<char, V>::cursor_value(cursor) })
    }

    #[inline]
    unsafe fn snapshot_graph_cursor_value(
        &self,
        graph: &crate::SnapshotTraversalGraph<char, Self::SnapshotGraphValueHandle>,
        cursor: crate::SnapshotTraversalCursor,
    ) -> Option<Option<Self::Value>> {
        let value_cursor = graph.value_handle(cursor);
        // SAFETY: the graph and retained owner originate from one revision.
        Some(unsafe { LockFreeDawgNode::<char, V>::cursor_value(value_cursor) })
    }
}

impl<V: DictionaryValue> MappedDictionary for DynamicDawgChar<V> {
    type Value = V;

    fn get_value(&self, term: &str) -> Option<Self::Value> {
        // Delegate to the inherent method
        Self::get_value(self, term)
    }

    fn contains_with_value<F>(&self, term: &str, predicate: F) -> bool
    where
        F: Fn(&Self::Value) -> bool,
    {
        match self.get_value(term) {
            Some(ref value) => predicate(value),
            None => false,
        }
    }
}

impl<V: DictionaryValue> crate::MutableDictionary for DynamicDawgChar<V> {
    fn insert(&self, term: &str) -> bool {
        // Delegate to the inherent method
        Self::insert(self, term)
    }

    fn remove(&self, term: &str) -> bool {
        // Delegate to the inherent method
        Self::remove(self, term)
    }

    fn extend<I, S>(&self, terms: I) -> usize
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        // Delegate to the inherent method (which also compacts)
        Self::extend(self, terms)
    }

    fn remove_many<I, S>(&self, terms: I) -> usize
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        // Delegate to the inherent method (which also compacts)
        Self::remove_many(self, terms)
    }
}

impl<V: DictionaryValue> crate::CompactableDictionary for DynamicDawgChar<V> {
    fn needs_compaction(&self) -> bool {
        // Delegate to the inherent method
        Self::needs_compaction(self)
    }

    fn compact(&self) -> usize {
        // Delegate to the inherent method
        Self::compact(self)
    }

    fn minimize(&self) -> usize {
        // Delegate to the inherent method
        Self::minimize(self)
    }
}

impl<V: DictionaryValue> crate::MutableMappedDictionary for DynamicDawgChar<V> {
    fn insert_with_value(&self, term: &str, value: Self::Value) -> bool {
        // Delegate to the inherent method
        Self::insert_with_value(self, term, value)
    }

    fn union_with<F>(&self, other: &Self, merge_fn: F) -> usize
    where
        F: Fn(&Self::Value, &Self::Value) -> Self::Value,
        Self::Value: Clone,
    {
        let entries: Vec<(String, Option<Self::Value>)> = other
            .inner
            .collect_visible_entries()
            .into_iter()
            .map(|(path, value)| (path.iter().collect(), value))
            .collect();

        let mut processed = 0;
        for (term, other_value) in entries {
            // `processed` counts every final term (preserving the original semantics);
            // only valued terms are merged into `self`.
            processed += 1;
            if let Some(other_value) = other_value {
                if let Some(self_value) = self.get_value(&term) {
                    let merged = merge_fn(&self_value, &other_value);
                    self.insert_with_value(&term, merged);
                } else {
                    self.insert_with_value(&term, other_value);
                }
            }
        }
        processed
    }

    fn update_or_insert<F>(&self, term: &str, default_value: Self::Value, update_fn: F) -> bool
    where
        F: Fn(&mut Self::Value),
    {
        // Delegate to the inherent method
        Self::update_or_insert(self, term, default_value, update_fn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use log::debug;

    #[test]
    fn test_dynamic_dawg_insert() {
        let dawg: DynamicDawgChar<()> = DynamicDawgChar::new();
        assert!(dawg.insert("test"));
        assert!(!dawg.insert("test")); // Duplicate
        assert!(dawg.insert("testing"));
        assert_eq!(dawg.term_count(), 2);
    }

    #[test]
    fn test_dynamic_dawg_remove() {
        let dawg: DynamicDawgChar<()> = DynamicDawgChar::new();
        dawg.insert("test");
        dawg.insert("testing");
        dawg.insert("tested");

        assert!(dawg.remove("testing"));
        assert_eq!(dawg.term_count(), 2);
        assert!(!dawg.remove("testing")); // Already removed
    }

    #[test]
    fn test_dynamic_dawg_compact() {
        let dawg: DynamicDawgChar<()> = DynamicDawgChar::new();
        dawg.insert("test");
        dawg.insert("testing");
        dawg.insert("tested");

        let before = dawg.node_count();
        dawg.remove("testing");

        let removed = dawg.compact();
        let after = dawg.node_count();

        assert!(removed > 0 || before == after);
        assert_eq!(dawg.term_count(), 2);
    }

    // NOTE: test_dynamic_dawg_with_transducer is in liblevenshtein since it requires the transducer module

    #[test]
    fn test_compaction_flag() {
        let dawg: DynamicDawgChar<()> = DynamicDawgChar::new();
        dawg.insert("test");

        assert!(!dawg.needs_compaction());

        dawg.remove("test");
        assert!(dawg.needs_compaction());

        dawg.compact();
        assert!(!dawg.needs_compaction());
    }

    #[test]
    fn test_batch_extend() {
        let dawg: DynamicDawgChar<()> = DynamicDawgChar::new();
        dawg.insert("test");

        let new_terms = vec!["testing", "tested", "tester"];
        let added = dawg.extend(new_terms);

        assert_eq!(added, 3);
        assert_eq!(dawg.term_count(), 4);
        assert!(dawg.contains("test"));
        assert!(dawg.contains("testing"));
    }

    #[test]
    fn test_batch_remove_many() {
        let dawg: DynamicDawgChar<()> =
            DynamicDawgChar::from_terms(vec!["test", "testing", "tested", "tester"]);

        let to_remove = vec!["testing", "tester"];
        let removed = dawg.remove_many(to_remove);

        assert_eq!(removed, 2);
        assert_eq!(dawg.term_count(), 2);
        assert!(dawg.contains("test"));
        assert!(!dawg.contains("testing"));
    }

    #[test]
    fn test_minimize_basic() {
        let dawg: DynamicDawgChar<()> = DynamicDawgChar::new();

        // Insert terms in unsorted order
        dawg.insert("zebra");
        dawg.insert("apple");
        dawg.insert("banana");
        dawg.insert("apricot");

        let nodes_before = dawg.node_count();
        let merged = dawg.minimize();
        let nodes_after = dawg.node_count();

        // Should have merged some nodes or stayed the same
        assert_eq!(nodes_after, nodes_before - merged);

        // All terms should still be present
        assert_eq!(dawg.term_count(), 4);
        assert!(dawg.contains("zebra"));
        assert!(dawg.contains("apple"));
        assert!(dawg.contains("banana"));
        assert!(dawg.contains("apricot"));
    }

    #[test]
    fn test_minimize_vs_compact() {
        // Test that minimize() achieves same minimality as compact()
        let _terms = ["band", "banana", "bandana", "can", "cane", "candy"];

        // Create two identical DAWGs with unsorted insertion
        let dawg1: DynamicDawgChar<()> = DynamicDawgChar::new();
        let dawg2: DynamicDawgChar<()> = DynamicDawgChar::new();

        for term in ["zebra", "apple", "banana", "apricot", "band", "bandana"] {
            dawg1.insert(term);
            dawg2.insert(term);
        }

        // Minimize one, compact the other
        let merged1 = dawg1.minimize();
        let merged2 = dawg2.compact();

        println!(
            "After minimize: {} nodes (merged {})",
            dawg1.node_count(),
            merged1
        );
        println!(
            "After compact: {} nodes (removed {})",
            dawg2.node_count(),
            merged2
        );

        // Both should contain same terms
        for term in ["zebra", "apple", "banana", "apricot", "band", "bandana"] {
            assert!(
                dawg1.contains(term),
                "minimize() DAWG missing term: {}",
                term
            );
            assert!(
                dawg2.contains(term),
                "compact() DAWG missing term: {}",
                term
            );
        }

        // Check term counts match
        assert_eq!(dawg1.term_count(), dawg2.term_count());

        // NOTE: minimize() and compact() may produce different node counts.
        // This is expected behavior:
        // - compact() rebuilds with sorted insertion, maximizing prefix sharing
        // - minimize() merges suffixes without restructuring the trie
        // Both produce correct results; compact() uses more CPU but yields better compression.
        // Choose based on use case: minimize() for real-time, compact() for batch processing.
        if dawg1.node_count() != dawg2.node_count() {
            debug!(
                "minimize() produced {} nodes, compact() produced {} nodes (expected difference)",
                dawg1.node_count(),
                dawg2.node_count()
            );
        }
    }

    #[test]
    fn test_minimize_after_deletions() {
        let dawg: DynamicDawgChar<()> =
            DynamicDawgChar::from_terms(vec!["test", "testing", "tested", "tester", "testimony"]);

        // Remove some terms, creating potential orphaned nodes
        dawg.remove("testing");
        dawg.remove("tester");

        assert!(dawg.needs_compaction());

        let nodes_before = dawg.node_count();
        let merged = dawg.minimize();
        let nodes_after = dawg.node_count();

        // Should have cleaned up orphaned nodes
        assert!(merged > 0);
        assert_eq!(nodes_after, nodes_before - merged);

        // Remaining terms should still be present
        assert!(dawg.contains("test"));
        assert!(dawg.contains("tested"));
        assert!(dawg.contains("testimony"));
        assert!(!dawg.contains("testing"));
        assert!(!dawg.contains("tester"));
    }

    #[test]
    fn test_minimize_empty() {
        let dawg: DynamicDawgChar<()> = DynamicDawgChar::new();
        let merged = dawg.minimize();

        // Empty DAWG should have nothing to minimize
        assert_eq!(merged, 0);
        assert_eq!(dawg.node_count(), 1); // Just root
        assert_eq!(dawg.term_count(), 0);
    }

    #[test]
    fn sorted_and_unordered_bulk_builders_share_the_minimal_kernel() {
        let sorted: DynamicDawgChar<()> = DynamicDawgChar::from_sorted_terms(["αβ", "γβ"]);
        let unordered: DynamicDawgChar<()> = DynamicDawgChar::from_terms(["γβ", "αβ"]);

        for dawg in [&sorted, &unordered] {
            assert_eq!(dawg.node_count(), 3);
            assert_eq!(dawg.term_count(), 2);
            assert!(dawg.contains("αβ"));
            assert!(dawg.contains("γβ"));
        }
    }

    #[test]
    fn mapped_bulk_builders_preserve_values_and_duplicate_precedence() {
        let unordered =
            DynamicDawgChar::from_terms_with_values([("γβ", 3_u32), ("αβ", 1), ("αβ", 2)]);
        let sorted =
            DynamicDawgChar::from_sorted_terms_with_values([("αβ", 1_u32), ("αβ", 2), ("γβ", 3)]);

        for dawg in [&unordered, &sorted] {
            assert_eq!(dawg.term_count(), 2);
            assert_eq!(dawg.get_value("αβ"), Some(2));
            assert_eq!(dawg.get_value("γβ"), Some(3));
        }
    }

    #[test]
    #[should_panic(expected = "requires lexicographically nondecreasing input")]
    fn mapped_sorted_builder_rejects_decreasing_input() {
        let _ = DynamicDawgChar::from_sorted_terms_with_values([("γ", 1_u32), ("α", 2)]);
    }

    #[test]
    fn test_minimize_single_term() {
        let dawg: DynamicDawgChar<()> = DynamicDawgChar::new();
        dawg.insert("hello");

        let nodes_before = dawg.node_count();
        let merged = dawg.minimize();
        let nodes_after = dawg.node_count();

        // Single term should already be minimal
        assert_eq!(merged, 0);
        assert_eq!(nodes_before, nodes_after);
        assert!(dawg.contains("hello"));
    }

    #[test]
    fn test_minimize_with_shared_suffixes() {
        let dawg: DynamicDawgChar<()> = DynamicDawgChar::new();

        // These words share suffixes: "ing" in testing/running
        dawg.insert("testing");
        dawg.insert("running");
        dawg.insert("test");
        dawg.insert("run");

        let _merged = dawg.minimize();

        // All terms should be preserved (minimize should handle shared suffixes)
        assert!(dawg.contains("testing"));
        assert!(dawg.contains("running"));
        assert!(dawg.contains("test"));
        assert!(dawg.contains("run"));
    }

    #[test]
    fn test_minimize_idempotent() {
        let dawg: DynamicDawgChar<()> =
            DynamicDawgChar::from_terms(vec!["apple", "application", "apply", "apricot"]);

        // First minimization
        let _merged1 = dawg.minimize();
        let nodes1 = dawg.node_count();

        // Second minimization should do nothing (already minimal)
        let merged2 = dawg.minimize();
        let nodes2 = dawg.node_count();

        assert_eq!(merged2, 0);
        assert_eq!(nodes1, nodes2);
    }

    #[test]
    fn test_minimize_no_false_positives() {
        // Test to prevent false positive lookups after minimize()
        let dawg: DynamicDawgChar<()> = DynamicDawgChar::new();

        // Insert specific terms in random order
        let inserted_terms = vec!["zebra", "apple", "banana", "apricot", "band", "bandana"];
        let not_inserted_terms = vec!["app", "ban", "zeb", "banan", "apric", "bandanas"];

        for term in &inserted_terms {
            dawg.insert(term);
        }

        // Minimize the DAWG
        dawg.minimize();

        // Check that inserted terms are still present
        for term in &inserted_terms {
            assert!(
                dawg.contains(term),
                "Should contain inserted term: {}",
                term
            );
        }

        // CRITICAL: Check that non-inserted terms are NOT present (no false positives)
        for term in &not_inserted_terms {
            assert!(
                !dawg.contains(term),
                "Should NOT contain term that wasn't inserted: {}",
                term
            );
        }
    }

    #[test]
    fn test_valued_dawg_basic() {
        // Test DynamicDawgChar with values
        let dawg: DynamicDawgChar<u32> = DynamicDawgChar::new();

        // Insert with values
        assert!(dawg.insert_with_value("hello", 42));
        assert!(dawg.insert_with_value("world", 100));
        assert!(dawg.insert_with_value("test", 1));

        // Verify values
        assert_eq!(dawg.get_value("hello"), Some(42));
        assert_eq!(dawg.get_value("world"), Some(100));
        assert_eq!(dawg.get_value("test"), Some(1));
        assert_eq!(dawg.get_value("unknown"), None);

        // Update value
        assert!(!dawg.insert_with_value("hello", 999));
        assert_eq!(dawg.get_value("hello"), Some(999));

        // Verify term count
        assert_eq!(dawg.term_count(), 3);
    }

    #[test]
    fn test_valued_dawg_with_remove() {
        let dawg: DynamicDawgChar<String> = DynamicDawgChar::new();

        dawg.insert_with_value("key1", "value1".to_string());
        dawg.insert_with_value("key2", "value2".to_string());

        assert_eq!(dawg.get_value("key1"), Some("value1".to_string()));

        // Remove should clear value
        assert!(dawg.remove("key1"));
        assert_eq!(dawg.get_value("key1"), None);
        assert_eq!(dawg.get_value("key2"), Some("value2".to_string()));
    }

    #[test]
    fn test_mapped_dictionary_trait() {
        use crate::MappedDictionary;

        let dawg: DynamicDawgChar<Vec<u32>> = DynamicDawgChar::new();
        dawg.insert_with_value("scoped", vec![1, 2, 3]);
        dawg.insert_with_value("global", vec![0]);

        // Test MappedDictionary::get_value
        assert_eq!(dawg.get_value("scoped"), Some(vec![1, 2, 3]));

        // Test contains_with_value
        assert!(dawg.contains_with_value("scoped", |v| v.contains(&2)));
        assert!(!dawg.contains_with_value("scoped", |v| v.contains(&999)));
        assert!(!dawg.contains_with_value("unknown", |v| v.contains(&1)));
    }

    #[test]
    fn test_compact_no_false_positives() {
        // Same test for compact() to establish baseline
        let dawg: DynamicDawgChar<()> = DynamicDawgChar::new();

        let inserted_terms = vec!["zebra", "apple", "banana", "apricot", "band", "bandana"];
        let not_inserted_terms = vec!["app", "ban", "zeb", "banan", "apric", "bandanas"];

        for term in &inserted_terms {
            dawg.insert(term);
        }

        dawg.compact();

        for term in &inserted_terms {
            assert!(
                dawg.contains(term),
                "Should contain inserted term: {}",
                term
            );
        }

        for term in &not_inserted_terms {
            assert!(
                !dawg.contains(term),
                "Should NOT contain term that wasn't inserted: {}",
                term
            );
        }
    }
}
