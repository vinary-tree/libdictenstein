//! 64-bit Dynamic DAWG with lock-free operations and 8-byte edge labels.
//!
//! This implementation uses lock-free algorithms with atomic operations for
//! concurrent access. Reads are wait-free (no blocking, no retries), and
//! writes use CAS (compare-and-swap) loops for lock-free progress guarantees.
//!
//! The byte, character, and u64 variants share one immutable-revision core.
//! Writers path-copy only the affected route and atomically publish a new
//! root; readers and iterators retain the revision they started from. This
//! gives every traversal exact query-start snapshot semantics without holding
//! a read lock or copying the whole dictionary.
//!
//! Unlike the byte-level `DynamicDawg` (u8 edges) or character-level
//! `DynamicDawgChar` (char/u32 edges), this variant uses 64-bit labels (u64),
//! enabling:
//!
//! - **Token sequences**: Vocabulary IDs, hash-based tokens
//! - **Time series**: f64 values encoded via `f64::to_bits()` / `f64::from_bits()`
//! - **Binary data**: Any 8-byte aligned data
//!
//! # Primary API
//!
//! The primary API uses direct sequence operations:
//!
//! - [`insert_sequence`](DynamicDawgU64::insert_sequence): Insert a u64 sequence
//! - [`contains_sequence`](DynamicDawgU64::contains_sequence): Check if sequence exists
//! - [`insert_f64`](DynamicDawgU64::insert_f64): Insert f64 series (convenience)
//! - [`contains_f64`](DynamicDawgU64::contains_f64): Check f64 series (convenience)
//!
//! The string-based API (via `CharUnit` trait) is available but secondary.
//!
//! # Thread Safety
//!
//! - **Reads**: Wait-free - multiple readers never block
//! - **Writes**: Lock-free CAS publication for insert/remove/update and
//!   compaction, with retry if another writer publishes first.
//! - **Memory**: Arc-based with automatic reclamation via arc-swap

#[cfg(feature = "bindings-core")]
use super::lockfree::PublishIfEmpty;
use super::lockfree::{LockFreeDawg, LockFreeDawgNode};
use super::u64_zipper::DynamicDawgU64Zipper;
use crate::value::DictionaryValue;
use crate::{Dictionary, DictionaryNode, SyncStrategy};
use std::sync::Arc;

pub(crate) type DawgNodeU64<V> = LockFreeDawgNode<u64, V>;

/// A dynamic DAWG with lock-free concurrent access.
///
/// # Type Parameters
///
/// - `V`: Optional value type associated with each term. Use `()` (default) for
///   dictionaries without values, or any type implementing `DictionaryValue`
///   (Clone + Send + Sync + 'static) for value-storing dictionaries.
///
/// # Thread Safety
///
/// - **Reads**: Wait-free - no locks, no retries, no blocking
/// - **Writes**: Lock-free - uses CAS loops, guaranteed progress
///
/// # Performance
///
/// - Insertion: O(m) where m is term length (amortized, with CAS retries)
/// - Lookup: O(m) - wait-free
/// - Space: Higher than the compact mutable core due to Arc overhead per node
///
/// # Examples
///
/// ```text
/// use std::thread;
/// use std::sync::Arc;
///
/// let dict = Arc::new(DynamicDawgU64::<()>::new());
///
/// // Concurrent reads and writes
/// let handles: Vec<_> = (0..10).map(|i| {
///     let d = dict.clone();
///     thread::spawn(move || {
///         d.insert_sequence(&[i, i+1, i+2]);
///         d.contains_sequence(&[i, i+1, i+2])
///     })
/// }).collect();
///
/// for h in handles {
///     assert!(h.join().unwrap());
/// }
/// ```
pub struct DynamicDawgU64<V: DictionaryValue = ()> {
    core: LockFreeDawg<u64, V>,
}

impl<V: DictionaryValue> Clone for DynamicDawgU64<V> {
    fn clone(&self) -> Self {
        Self {
            core: self.core.clone(),
        }
    }
}

impl<V: DictionaryValue> std::fmt::Debug for DynamicDawgU64<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DynamicDawgU64")
            .field("term_count", &self.term_count())
            .field("needs_compaction", &self.needs_compaction())
            .finish()
    }
}

impl<V: DictionaryValue> Default for DynamicDawgU64<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V: DictionaryValue> DynamicDawgU64<V> {
    /// Create a new empty dynamic DAWG.
    ///
    /// # Example
    ///
    /// ```text
    /// let dawg: DynamicDawgU64<()> = DynamicDawgU64::new();
    /// dawg.insert_sequence(&[1, 2, 3]);
    /// ```
    pub fn new() -> Self {
        Self {
            core: LockFreeDawg::new(),
        }
    }

    /// Get the root node Arc of the DAWG.
    ///
    /// This is primarily used by zippers and iterators for navigation.
    #[inline]
    pub(crate) fn root_arc(&self) -> Arc<DawgNodeU64<V>> {
        self.core.root_arc()
    }

    /// Create a new empty dynamic DAWG with custom auto-minimize threshold.
    ///
    /// The u64 lock-free variant keeps explicit maintenance as the publication
    /// boundary: callers invoke [`compact`](Self::compact) or
    /// [`minimize`](Self::minimize) when they want to rebuild dead paths. The
    /// threshold parameter is accepted so generic DAWG builders can share this
    /// constructor across byte, char, and u64 variants.
    pub fn with_auto_minimize_threshold(_threshold: f32) -> Self {
        Self::new()
    }

    /// Create a new empty dynamic DAWG with full configuration.
    ///
    /// The u64 lock-free variant performs exact wait-free traversals and does
    /// not install a Bloom filter. The configuration parameters are accepted
    /// for API compatibility with generic DAWG construction code; explicit
    /// compaction remains the maintenance mechanism.
    pub fn with_config(auto_minimize_threshold: f32, bloom_filter_capacity: Option<usize>) -> Self {
        Self {
            core: LockFreeDawg::with_config(auto_minimize_threshold, bloom_filter_capacity),
        }
    }

    /// Create from an iterator of terms.
    pub fn from_terms<I, S>(terms: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut sequences: Vec<Vec<u64>> = terms
            .into_iter()
            .map(|term| <u64 as crate::CharUnit>::from_str(term.as_ref()))
            .collect();
        crate::causal_perf::record_batch_sort_calls(1);
        crate::causal_perf::record_batch_sort_terms(sequences.len() as u64);
        crate::causal_perf::record_batch_sort_units(
            sequences.iter().map(Vec::len).sum::<usize>() as u64
        );
        sequences.sort_unstable();
        Self {
            core: LockFreeDawg::from_sorted_terms_by(sequences, |sequence, units| {
                units.extend_from_slice(sequence);
            }),
        }
    }

    /// Create from sorted terms.
    ///
    /// # Panics
    ///
    /// Panics if the packed-u64 encodings of the supplied terms are not in
    /// lexicographically nondecreasing order.
    pub fn from_sorted_terms<I, S>(terms: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self {
            core: LockFreeDawg::from_sorted_terms_by(terms, |term, units| {
                units.extend(<u64 as crate::CharUnit>::from_str(term.as_ref()));
            }),
        }
    }

    /// Create from an iterator of `(term, value)` pairs.
    pub fn from_terms_with_values<I, S>(entries: I) -> Self
    where
        I: IntoIterator<Item = (S, V)>,
        S: AsRef<str>,
    {
        let mut sequences: Vec<(Vec<u64>, V)> = entries
            .into_iter()
            .map(|(term, value)| (<u64 as crate::CharUnit>::from_str(term.as_ref()), value))
            .collect();
        crate::causal_perf::record_batch_sort_calls(1);
        crate::causal_perf::record_batch_sort_terms(sequences.len() as u64);
        crate::causal_perf::record_batch_sort_units(
            sequences
                .iter()
                .map(|(sequence, _)| sequence.len())
                .sum::<usize>() as u64,
        );
        // Stable ordering retains the original order among duplicate token
        // sequences, preserving last-value-wins semantics.
        sequences.sort_by(|left, right| left.0.cmp(&right.0));
        Self {
            core: LockFreeDawg::from_sorted_entries_by(
                sequences
                    .into_iter()
                    .map(|(sequence, value)| (sequence, Some(value))),
                |sequence, units| units.extend_from_slice(sequence),
            ),
        }
    }

    /// Create from token-encoding-ordered `(term, value)` pairs.
    ///
    /// This skips sorting and constructs one immutable minimal graph. Duplicate
    /// terms are allowed and the last value wins.
    ///
    /// # Panics
    ///
    /// Panics if packed-u64 term encodings are not in lexicographically
    /// nondecreasing order.
    pub fn from_sorted_terms_with_values<I, S>(entries: I) -> Self
    where
        I: IntoIterator<Item = (S, V)>,
        S: AsRef<str>,
    {
        Self {
            core: LockFreeDawg::from_sorted_entries_by(
                entries.into_iter().map(|(term, value)| (term, Some(value))),
                |term, units| units.extend(<u64 as crate::CharUnit>::from_str(term.as_ref())),
            ),
        }
    }

    /// Crate-internal unit-native variant used by binding batch rebuilds.
    #[cfg(feature = "bindings-core")]
    pub(crate) fn from_sorted_sequence_entries<I>(entries: I) -> Self
    where
        I: IntoIterator<Item = (Vec<u64>, Option<V>)>,
    {
        Self {
            core: LockFreeDawg::from_sorted_entries_by(entries, |sequence, units| {
                units.extend_from_slice(sequence);
            }),
        }
    }

    /// Insert a term into the DAWG (string-based API).
    ///
    /// Returns `true` if the term was newly inserted, `false` if it already existed.
    pub fn insert(&self, term: &str) -> bool {
        let sequence: Vec<u64> = crate::CharUnit::from_str(term);
        self.insert_sequence(&sequence)
    }

    /// Insert a term with an associated value.
    ///
    /// Returns `true` if the term was newly inserted, `false` if it already existed.
    /// If the term already exists, its value is updated.
    pub fn insert_with_value(&self, term: &str, value: V) -> bool {
        let sequence: Vec<u64> = crate::CharUnit::from_str(term);
        self.insert_sequence_with_value(&sequence, value)
    }

    /// Update an existing term's value in place, or insert with default value.
    pub fn update_or_insert<F>(&self, term: &str, default_value: V, update_fn: F) -> bool
    where
        F: Fn(&mut V),
    {
        let sequence: Vec<u64> = crate::CharUnit::from_str(term);
        self.update_or_insert_sequence(&sequence, default_value, update_fn)
    }

    /// Get the value associated with a term.
    pub fn get_value(&self, term: &str) -> Option<V> {
        let sequence: Vec<u64> = crate::CharUnit::from_str(term);
        self.get_sequence_value(&sequence)
    }

    /// Remove a term from the DAWG.
    ///
    /// Returns `true` if the term was present and removed, `false` otherwise.
    pub fn remove(&self, term: &str) -> bool {
        let sequence: Vec<u64> = crate::CharUnit::from_str(term);
        self.remove_sequence(&sequence)
    }

    /// Compact the DAWG by rebuilding a canonical graph from currently visible
    /// final sequences.
    ///
    /// Reads remain wait-free while compaction builds the replacement graph.
    /// Publication uses the same revision CAS as normal writes, so a racing
    /// compaction retries rather than overwriting a newer update.
    pub fn compact(&self) -> usize {
        self.core.compact()
    }

    /// Minimize the DAWG by rebuilding and interning equivalent valueless
    /// suffix subgraphs.
    pub fn minimize(&self) -> usize {
        self.core.minimize()
    }

    /// Batch insert multiple terms.
    pub fn extend<I, S>(&self, terms: I) -> usize
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut added = 0;
        for term in terms {
            if self.insert(term.as_ref()) {
                added += 1;
            }
        }
        added
    }

    /// Batch remove multiple terms.
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
        removed
    }

    /// Get the number of terms in the DAWG.
    #[inline]
    pub fn term_count(&self) -> usize {
        self.core.term_count()
    }

    /// Capture the current root together with its term count from one
    /// atomically published revision.
    ///
    /// Calling [`Dictionary::root`] and [`Dictionary::len`] separately
    /// performs two independent revision loads, so a concurrent writer can
    /// tear the pair (finding LDICT-B4). Snapshot capture uses this
    /// coherent accessor instead.
    pub fn root_with_term_count(&self) -> (DynamicDawgU64Node<V>, usize) {
        let (root, term_count) = self.core.root_arc_with_term_count();
        (DynamicDawgU64Node { node: root }, term_count)
    }

    #[cfg(feature = "bindings-core")]
    pub(crate) fn root_with_term_count_revision(&self) -> (DynamicDawgU64Node<V>, usize, u64) {
        let (root, term_count, revision) = self.core.root_arc_with_term_count_revision();
        (DynamicDawgU64Node { node: root }, term_count, revision)
    }

    #[cfg(feature = "bindings-core")]
    pub(crate) fn clear_graph(&self) -> bool {
        self.core.clear()
    }

    #[cfg(feature = "bindings-core")]
    pub(crate) fn try_publish_if_empty(&self, frozen: &Self) -> PublishIfEmpty {
        self.core.try_publish_if_empty(&frozen.core)
    }

    /// Get the number of nodes in the DAWG.
    pub fn node_count(&self) -> usize {
        self.core.node_count()
    }

    /// Check if compaction is recommended.
    #[inline]
    pub fn needs_compaction(&self) -> bool {
        self.core.needs_compaction()
    }

    /// Check if a term is in the DAWG (string-based API).
    ///
    /// This is a wait-free operation.
    pub fn contains(&self, term: &str) -> bool {
        let sequence: Vec<u64> = crate::CharUnit::from_str(term);
        self.contains_sequence(&sequence)
    }

    // =========================================================================
    // Sequence-based API (primary for u64 usage)
    // =========================================================================

    /// Insert a u64 sequence directly (lock-free).
    ///
    /// Returns `true` if the sequence was newly inserted, `false` if it already existed.
    ///
    /// # Lock-Free Guarantee
    ///
    /// This method uses CAS loops to atomically update edge lists. At least one
    /// concurrent writer always makes progress, preventing livelock.
    pub fn insert_sequence(&self, sequence: &[u64]) -> bool {
        self.core.insert_units(sequence)
    }

    /// Insert a sequence with an associated value.
    pub fn insert_sequence_with_value(&self, sequence: &[u64], value: V) -> bool {
        self.core.insert_units_with_value(sequence, value)
    }

    /// Insert/update a token sequence while preserving an absent mapped value.
    #[cfg(feature = "bindings-core")]
    pub(crate) fn insert_sequence_with_optional_value(
        &self,
        sequence: &[u64],
        value: Option<V>,
    ) -> bool {
        self.core.insert_units_with_optional_value(sequence, value)
    }

    /// Update or insert a sequence with value.
    pub fn update_or_insert_sequence<F>(
        &self,
        sequence: &[u64],
        default_value: V,
        update_fn: F,
    ) -> bool
    where
        F: Fn(&mut V),
    {
        self.core
            .update_or_insert_units(sequence, default_value, update_fn)
    }

    /// Get the value for a sequence.
    pub fn get_sequence_value(&self, sequence: &[u64]) -> Option<V> {
        self.core.get_units_value(sequence)
    }

    /// Read membership and optional value from one immutable graph revision.
    #[cfg(feature = "bindings-core")]
    pub(crate) fn get_sequence_optional_value(&self, sequence: &[u64]) -> Option<Option<V>> {
        self.core.get_units_optional_value(sequence)
    }

    /// Check if a sequence exists in the DAWG (wait-free).
    ///
    /// This is a wait-free operation - no locks, no retries, no blocking.
    #[inline]
    pub fn contains_sequence(&self, sequence: &[u64]) -> bool {
        self.core.contains_units(sequence)
    }

    /// Remove a sequence from the DAWG.
    ///
    /// Note: This only unmarks the node as final. The node structure remains
    /// for potential future use. Call `compact()` to reclaim unused nodes.
    pub fn remove_sequence(&self, sequence: &[u64]) -> bool {
        self.core.remove_units(sequence)
    }

    // =========================================================================
    // f64 convenience API
    // =========================================================================

    /// Insert an f64 series as bit patterns.
    ///
    /// The f64 values are converted to their IEEE 754 bit representation
    /// using `f64::to_bits()`, then stored as a u64 sequence.
    pub fn insert_f64(&self, series: &[f64]) -> bool {
        let sequence: Vec<u64> = series.iter().map(|f| f.to_bits()).collect();
        self.insert_sequence(&sequence)
    }

    /// Insert an f64 series with an associated value.
    pub fn insert_f64_with_value(&self, series: &[f64], value: V) -> bool {
        let sequence: Vec<u64> = series.iter().map(|f| f.to_bits()).collect();
        self.insert_sequence_with_value(&sequence, value)
    }

    /// Check if an f64 series exists in the DAWG.
    pub fn contains_f64(&self, series: &[f64]) -> bool {
        let sequence: Vec<u64> = series.iter().map(|f| f.to_bits()).collect();
        self.contains_sequence(&sequence)
    }

    /// Get the value for an f64 series.
    pub fn get_f64_value(&self, series: &[f64]) -> Option<V> {
        let sequence: Vec<u64> = series.iter().map(|f| f.to_bits()).collect();
        self.get_sequence_value(&sequence)
    }

    /// Remove an f64 series from the DAWG.
    pub fn remove_f64(&self, series: &[f64]) -> bool {
        let sequence: Vec<u64> = series.iter().map(|f| f.to_bits()).collect();
        self.remove_sequence(&sequence)
    }

    // =========================================================================
    // Iterator support
    // =========================================================================

    /// Create a zipper at the root of the DAWG.
    pub fn zipper(&self) -> DynamicDawgU64Zipper<V> {
        DynamicDawgU64Zipper::new_from_dict(self)
    }

    /// Iterate over all terms in the DAWG.
    pub fn iter(&self) -> impl Iterator<Item = Vec<u64>> + '_ {
        DawgIterator::new(self)
    }

    /// Iterate over all terms that have values.
    ///
    /// This legacy mapped-only iterator omits present sequences whose value is
    /// `None`. Use `(&dictionary).into_iter()` or
    /// [`DictionaryEntries::entries`](crate::DictionaryEntries::entries) for
    /// lossless [`DictionaryEntry`](crate::DictionaryEntry) snapshots.
    pub fn iter_with_values(&self) -> impl Iterator<Item = (Vec<u64>, V)> + '_ {
        DawgIteratorWithValues::new(self)
    }
}

impl<V: DictionaryValue> std::iter::FromIterator<String> for DynamicDawgU64<V> {
    /// Builds one minimal immutable revision instead of repeatedly publishing
    /// path-copied revisions through [`insert`](Self::insert).
    fn from_iter<I: IntoIterator<Item = String>>(iter: I) -> Self {
        Self::from_terms(iter)
    }
}

impl<'a, V: DictionaryValue> std::iter::FromIterator<&'a str> for DynamicDawgU64<V> {
    fn from_iter<I: IntoIterator<Item = &'a str>>(iter: I) -> Self {
        Self::from_terms(iter)
    }
}

impl<V: DictionaryValue> std::iter::FromIterator<Vec<u64>> for DynamicDawgU64<V> {
    fn from_iter<I: IntoIterator<Item = Vec<u64>>>(iter: I) -> Self {
        let mut sequences: Vec<Vec<u64>> = iter.into_iter().collect();
        crate::causal_perf::record_batch_sort_calls(1);
        crate::causal_perf::record_batch_sort_terms(sequences.len() as u64);
        crate::causal_perf::record_batch_sort_units(
            sequences.iter().map(Vec::len).sum::<usize>() as u64
        );
        sequences.sort_unstable();
        Self {
            core: LockFreeDawg::from_sorted_terms_by(sequences, |sequence, units| {
                units.extend_from_slice(sequence);
            }),
        }
    }
}

impl<'a, V: DictionaryValue> std::iter::FromIterator<&'a [u64]> for DynamicDawgU64<V> {
    fn from_iter<I: IntoIterator<Item = &'a [u64]>>(iter: I) -> Self {
        iter.into_iter().map(<[u64]>::to_vec).collect()
    }
}

impl<V: DictionaryValue> std::iter::FromIterator<(String, V)> for DynamicDawgU64<V> {
    fn from_iter<I: IntoIterator<Item = (String, V)>>(iter: I) -> Self {
        Self::from_terms_with_values(iter)
    }
}

impl<'a, V: DictionaryValue> std::iter::FromIterator<(&'a str, V)> for DynamicDawgU64<V> {
    fn from_iter<I: IntoIterator<Item = (&'a str, V)>>(iter: I) -> Self {
        Self::from_terms_with_values(iter)
    }
}

impl<V: DictionaryValue> std::iter::FromIterator<(Vec<u64>, V)> for DynamicDawgU64<V> {
    fn from_iter<I: IntoIterator<Item = (Vec<u64>, V)>>(iter: I) -> Self {
        let mut entries: Vec<(Vec<u64>, V)> = iter.into_iter().collect();
        crate::causal_perf::record_batch_sort_calls(1);
        crate::causal_perf::record_batch_sort_terms(entries.len() as u64);
        crate::causal_perf::record_batch_sort_units(
            entries.iter().map(|(key, _)| key.len()).sum::<usize>() as u64,
        );
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        Self {
            core: LockFreeDawg::from_sorted_entries_by(
                entries.into_iter().map(|(key, value)| (key, Some(value))),
                |key, units| units.extend_from_slice(key),
            ),
        }
    }
}

impl<'a, V: DictionaryValue> std::iter::FromIterator<(&'a [u64], V)> for DynamicDawgU64<V> {
    fn from_iter<I: IntoIterator<Item = (&'a [u64], V)>>(iter: I) -> Self {
        iter.into_iter()
            .map(|(key, value)| (key.to_vec(), value))
            .collect()
    }
}

impl<V: DictionaryValue> std::iter::Extend<String> for DynamicDawgU64<V> {
    fn extend<I: IntoIterator<Item = String>>(&mut self, iter: I) {
        let _ = DynamicDawgU64::extend(self, iter);
    }
}

impl<'a, V: DictionaryValue> std::iter::Extend<&'a str> for DynamicDawgU64<V> {
    fn extend<I: IntoIterator<Item = &'a str>>(&mut self, iter: I) {
        let _ = DynamicDawgU64::extend(self, iter);
    }
}

impl<V: DictionaryValue> std::iter::Extend<Vec<u64>> for DynamicDawgU64<V> {
    fn extend<I: IntoIterator<Item = Vec<u64>>>(&mut self, iter: I) {
        let mut sequences: Vec<Vec<u64>> = iter.into_iter().collect();
        sequences.sort_unstable();
        for sequence in sequences {
            self.insert_sequence(&sequence);
        }
    }
}

impl<'a, V: DictionaryValue> std::iter::Extend<&'a [u64]> for DynamicDawgU64<V> {
    fn extend<I: IntoIterator<Item = &'a [u64]>>(&mut self, iter: I) {
        <Self as std::iter::Extend<Vec<u64>>>::extend(self, iter.into_iter().map(<[u64]>::to_vec));
    }
}

impl<V: DictionaryValue> std::iter::Extend<(String, V)> for DynamicDawgU64<V> {
    fn extend<I: IntoIterator<Item = (String, V)>>(&mut self, iter: I) {
        let mut entries: Vec<(String, V)> = iter.into_iter().collect();
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        for (term, value) in entries {
            self.insert_with_value(&term, value);
        }
    }
}

impl<'a, V: DictionaryValue> std::iter::Extend<(&'a str, V)> for DynamicDawgU64<V> {
    fn extend<I: IntoIterator<Item = (&'a str, V)>>(&mut self, iter: I) {
        <Self as std::iter::Extend<(String, V)>>::extend(
            self,
            iter.into_iter()
                .map(|(term, value)| (term.to_owned(), value)),
        );
    }
}

impl<V: DictionaryValue> std::iter::Extend<(Vec<u64>, V)> for DynamicDawgU64<V> {
    fn extend<I: IntoIterator<Item = (Vec<u64>, V)>>(&mut self, iter: I) {
        let mut entries: Vec<(Vec<u64>, V)> = iter.into_iter().collect();
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        for (key, value) in entries {
            self.insert_sequence_with_value(&key, value);
        }
    }
}

impl<'a, V: DictionaryValue> std::iter::Extend<(&'a [u64], V)> for DynamicDawgU64<V> {
    fn extend<I: IntoIterator<Item = (&'a [u64], V)>>(&mut self, iter: I) {
        <Self as std::iter::Extend<(Vec<u64>, V)>>::extend(
            self,
            iter.into_iter().map(|(key, value)| (key.to_vec(), value)),
        );
    }
}

/// Iterator over DAWG terms.
struct DawgIterator<'a, V: DictionaryValue> {
    #[allow(dead_code)]
    dawg: &'a DynamicDawgU64<V>,
    stack: Vec<(Arc<DawgNodeU64<V>>, Vec<u64>, usize)>,
}

impl<'a, V: DictionaryValue> DawgIterator<'a, V> {
    fn new(dawg: &'a DynamicDawgU64<V>) -> Self {
        DawgIterator {
            dawg,
            stack: vec![(dawg.root_arc(), Vec::new(), 0)],
        }
    }
}

impl<V: DictionaryValue> Iterator for DawgIterator<'_, V> {
    type Item = Vec<u64>;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some((node, path, edge_idx)) = self.stack.pop() {
            // Visit children
            if edge_idx < node.edges.edges.len() {
                let (label, child) = &node.edges.edges[edge_idx];
                let mut new_path = path.clone();
                new_path.push(*label);

                // Push current node back with next edge index
                self.stack.push((node.clone(), path, edge_idx + 1));
                // Push child to visit
                self.stack.push((child.clone(), new_path, 0));
            } else if node.is_final {
                // All children visited, and this is a final node - return the path
                return Some(path);
            }
        }
        None
    }
}

/// Iterator over DAWG terms with values.
struct DawgIteratorWithValues<'a, V: DictionaryValue> {
    #[allow(dead_code)]
    dawg: &'a DynamicDawgU64<V>,
    stack: Vec<(Arc<DawgNodeU64<V>>, Vec<u64>, usize)>,
}

impl<'a, V: DictionaryValue> DawgIteratorWithValues<'a, V> {
    fn new(dawg: &'a DynamicDawgU64<V>) -> Self {
        DawgIteratorWithValues {
            dawg,
            stack: vec![(dawg.root_arc(), Vec::new(), 0)],
        }
    }
}

impl<V: DictionaryValue> Iterator for DawgIteratorWithValues<'_, V> {
    type Item = (Vec<u64>, V);

    fn next(&mut self) -> Option<Self::Item> {
        while let Some((node, path, edge_idx)) = self.stack.pop() {
            if edge_idx < node.edges.edges.len() {
                let (label, child) = &node.edges.edges[edge_idx];
                let mut new_path = path.clone();
                new_path.push(*label);

                self.stack.push((node.clone(), path, edge_idx + 1));
                self.stack.push((child.clone(), new_path, 0));
            } else if node.is_final {
                if let Some(v) = &node.value {
                    return Some((path, (**v).clone()));
                }
            }
        }
        None
    }
}

// =========================================================================
// Dictionary trait implementation
// =========================================================================

/// Node wrapper for Dictionary trait.
pub struct DynamicDawgU64Node<V: DictionaryValue> {
    node: Arc<DawgNodeU64<V>>,
}

impl<V: DictionaryValue> Clone for DynamicDawgU64Node<V> {
    fn clone(&self) -> Self {
        DynamicDawgU64Node {
            node: self.node.clone(),
        }
    }
}

impl<V: DictionaryValue> DictionaryNode for DynamicDawgU64Node<V> {
    type Unit = u64;
    type SnapshotCursor = super::DynamicDawgSnapshotCursor<u64, V>;
    type SnapshotGraphValueHandle = super::DynamicDawgSnapshotCursor<u64, V>;

    #[inline]
    fn snapshot_node_identity(&self) -> Option<crate::SnapshotNodeIdentity> {
        self.node.snapshot_id
    }

    #[inline]
    fn snapshot_root_cursor(&self) -> Option<Self::SnapshotCursor> {
        Some(DawgNodeU64::traversal_cursor(&self.node))
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
        P: FnMut(u64) -> Option<T>,
        F: FnMut(u64, Self::SnapshotCursor, T),
    {
        // SAFETY: the trait contract requires every cursor to originate from
        // this retained root revision.
        Some(unsafe {
            DawgNodeU64::<V>::filter_map_cursor_edges_and_finality(cursor, project, visitor)
        })
    }

    fn is_final(&self) -> bool {
        self.node.is_final
    }

    fn transition(&self, label: Self::Unit) -> Option<Self> {
        self.node.edges.find(label).map(|child| DynamicDawgU64Node {
            node: child.clone(),
        })
    }

    fn edges(&self) -> Box<dyn Iterator<Item = (Self::Unit, Self)> + '_> {
        let edges_vec: Vec<_> = self
            .node
            .edges
            .edges
            .iter()
            .map(|(label, child)| {
                (
                    *label,
                    DynamicDawgU64Node {
                        node: child.clone(),
                    },
                )
            })
            .collect();
        Box::new(edges_vec.into_iter())
    }

    #[inline]
    fn for_each_edge<F>(&self, mut visitor: F)
    where
        F: FnMut(Self::Unit, Self),
    {
        for (label, child) in &self.node.edges.edges {
            visitor(
                *label,
                DynamicDawgU64Node {
                    node: child.clone(),
                },
            );
        }
    }

    #[inline]
    fn filter_map_edges<T, P, F>(&self, mut project: P, mut visitor: F)
    where
        P: FnMut(u64) -> Option<T>,
        F: FnMut(u64, Self, T),
    {
        for (label, child) in &self.node.edges.edges {
            if let Some(projected) = project(*label) {
                visitor(
                    *label,
                    DynamicDawgU64Node {
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

impl<V: DictionaryValue> crate::MappedDictionaryNode for DynamicDawgU64Node<V> {
    type Value = V;

    /// The value stored at this node, if any. Values are attached only at final
    /// nodes (via `insert_sequence_with_value`); the immutable value slot is
    /// empty elsewhere, so this yields `None` for non-final nodes.
    fn value(&self) -> Option<Self::Value> {
        self.node.value.as_ref().map(|value| (**value).clone())
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
        Some(unsafe { LockFreeDawgNode::<u64, V>::cursor_value(cursor) })
    }

    #[inline]
    unsafe fn snapshot_graph_cursor_value(
        &self,
        graph: &crate::SnapshotTraversalGraph<u64, Self::SnapshotGraphValueHandle>,
        cursor: crate::SnapshotTraversalCursor,
    ) -> Option<Option<Self::Value>> {
        let value_cursor = graph.value_handle(cursor);
        // SAFETY: the graph and retained owner originate from one revision.
        Some(unsafe { LockFreeDawgNode::<u64, V>::cursor_value(value_cursor) })
    }
}

impl<V: DictionaryValue> crate::MutableDictionary for DynamicDawgU64<V> {
    fn insert(&self, term: &str) -> bool {
        // Delegate to the inherent method
        Self::insert(self, term)
    }

    fn remove(&self, term: &str) -> bool {
        // Delegate to the inherent method
        Self::remove(self, term)
    }
}

impl<V: DictionaryValue> crate::CompactableDictionary for DynamicDawgU64<V> {
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

impl<V: DictionaryValue> Dictionary for DynamicDawgU64<V> {
    type Node = DynamicDawgU64Node<V>;

    fn root(&self) -> Self::Node {
        DynamicDawgU64Node {
            node: self.root_arc(),
        }
    }

    fn traversal_root(&self) -> crate::DictionaryTraversalRoot<Self::Node> {
        let (node, cursor_graph) = self.core.root_arc_with_cursor_graph();
        let root = DynamicDawgU64Node { node };
        match cursor_graph {
            Some(graph) => crate::DictionaryTraversalRoot::captured(root, graph),
            None => crate::DictionaryTraversalRoot::owned(root),
        }
    }

    fn len(&self) -> Option<usize> {
        Some(self.term_count())
    }

    fn is_empty(&self) -> bool {
        self.term_count() == 0
    }

    fn sync_strategy(&self) -> SyncStrategy {
        SyncStrategy::InternalSync
    }
}

impl<V: DictionaryValue> crate::MappedDictionary for DynamicDawgU64<V> {
    type Value = V;

    /// String-keyed value lookup (the trait's `&str` surface). Sequence-keyed
    /// lookups use the inherent [`get_sequence_value`](DynamicDawgU64::get_sequence_value);
    /// value-yielding fuzzy queries read [`MappedDictionaryNode::value`](crate::MappedDictionaryNode::value) during the
    /// walk and never call this. `contains_with_value` uses the trait default.
    fn get_value(&self, term: &str) -> Option<Self::Value> {
        DynamicDawgU64::get_value(self, term)
    }
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_dawg_is_empty() {
        let dawg: DynamicDawgU64<()> = DynamicDawgU64::new();
        assert_eq!(dawg.term_count(), 0);
        assert!(!dawg.needs_compaction());
    }

    #[test]
    fn test_insert_sequence() {
        let dawg: DynamicDawgU64<()> = DynamicDawgU64::new();

        assert!(dawg.insert_sequence(&[1, 2, 3]));
        assert!(!dawg.insert_sequence(&[1, 2, 3])); // Duplicate
        assert!(dawg.insert_sequence(&[1, 2, 4]));
        assert!(dawg.insert_sequence(&[5, 6, 7]));

        assert_eq!(dawg.term_count(), 3);
    }

    #[test]
    fn test_contains_sequence() {
        let dawg: DynamicDawgU64<()> = DynamicDawgU64::new();
        dawg.insert_sequence(&[1, 2, 3]);
        dawg.insert_sequence(&[1, 2, 4]);

        assert!(dawg.contains_sequence(&[1, 2, 3]));
        assert!(dawg.contains_sequence(&[1, 2, 4]));
        assert!(!dawg.contains_sequence(&[1, 2])); // Prefix only
        assert!(!dawg.contains_sequence(&[1, 2, 5])); // Doesn't exist
        assert!(!dawg.contains_sequence(&[9, 9, 9]));
    }

    #[test]
    fn test_empty_sequence() {
        let dawg: DynamicDawgU64<()> = DynamicDawgU64::new();

        assert!(dawg.insert_sequence(&[]));
        assert!(!dawg.insert_sequence(&[])); // Duplicate
        assert!(dawg.contains_sequence(&[]));
        assert_eq!(dawg.term_count(), 1);
    }

    #[test]
    fn test_remove_sequence() {
        let dawg: DynamicDawgU64<()> = DynamicDawgU64::new();
        dawg.insert_sequence(&[1, 2, 3]);
        dawg.insert_sequence(&[1, 2, 4]);

        assert!(dawg.remove_sequence(&[1, 2, 3]));
        assert!(!dawg.contains_sequence(&[1, 2, 3]));
        assert!(dawg.contains_sequence(&[1, 2, 4])); // Other term still exists
        assert_eq!(dawg.term_count(), 1);

        assert!(!dawg.remove_sequence(&[1, 2, 3])); // Already removed
    }

    #[test]
    fn test_compact_rebuilds_without_removed_branches() {
        let dawg: DynamicDawgU64<()> = DynamicDawgU64::new();
        assert!(dawg.insert_sequence(&[1, 2, 3]));
        assert!(dawg.insert_sequence(&[1, 2, 4]));
        assert!(dawg.insert_sequence(&[9, 9, 9]));

        let before_remove = dawg.node_count();
        assert!(dawg.remove_sequence(&[9, 9, 9]));
        assert!(dawg.needs_compaction());
        assert_eq!(dawg.node_count(), before_remove);

        let removed = dawg.compact();
        assert!(
            removed > 0,
            "compaction should remove the dead [9,9,9] branch"
        );
        assert!(!dawg.needs_compaction());
        assert!(dawg.contains_sequence(&[1, 2, 3]));
        assert!(dawg.contains_sequence(&[1, 2, 4]));
        assert!(!dawg.contains_sequence(&[9, 9, 9]));
        assert_eq!(dawg.term_count(), 2);
    }

    #[test]
    fn test_minimize_preserves_values_and_empty_sequence() {
        let dawg: DynamicDawgU64<i64> = DynamicDawgU64::new();
        assert!(dawg.insert_sequence_with_value(&[], 7));
        assert!(dawg.insert_sequence_with_value(&[1, 2, 3], 123));
        assert!(dawg.insert_sequence_with_value(&[4, 2, 3], 456));
        assert!(dawg.insert_sequence(&[8, 8, 8]));
        assert!(dawg.remove_sequence(&[8, 8, 8]));

        let removed = dawg.minimize();
        assert!(
            removed > 0,
            "minimize should publish a rebuilt compact graph"
        );
        assert_eq!(dawg.get_sequence_value(&[]), Some(7));
        assert_eq!(dawg.get_sequence_value(&[1, 2, 3]), Some(123));
        assert_eq!(dawg.get_sequence_value(&[4, 2, 3]), Some(456));
        assert!(!dawg.contains_sequence(&[8, 8, 8]));
        assert_eq!(dawg.term_count(), 3);
    }

    #[test]
    fn test_compact_does_not_lose_concurrent_writes() {
        use std::sync::Arc as StdArc;
        use std::thread;

        let dawg = StdArc::new(DynamicDawgU64::<()>::new());
        for i in 0..128u64 {
            assert!(dawg.insert_sequence(&[i, i + 1, i + 2]));
        }
        for i in 0..64u64 {
            assert!(dawg.remove_sequence(&[i, i + 1, i + 2]));
        }

        let writer = {
            let dawg = dawg.clone();
            thread::spawn(move || {
                for i in 128..256u64 {
                    dawg.insert_sequence(&[i, i + 1, i + 2]);
                }
            })
        };
        let compactor = {
            let dawg = dawg.clone();
            thread::spawn(move || {
                let _ = dawg.compact();
            })
        };

        writer.join().expect("writer should not panic");
        compactor.join().expect("compactor should not panic");

        for i in 64..256u64 {
            assert!(
                dawg.contains_sequence(&[i, i + 1, i + 2]),
                "sequence inserted before/during compaction was lost: {i}"
            );
        }
        for i in 0..64u64 {
            assert!(
                !dawg.contains_sequence(&[i, i + 1, i + 2]),
                "removed sequence resurrected after compaction: {i}"
            );
        }
    }

    #[test]
    fn test_f64_api() {
        let dawg: DynamicDawgU64<()> = DynamicDawgU64::new();

        assert!(dawg.insert_f64(&[1.0, 2.0, 3.0]));
        assert!(dawg.insert_f64(&[1.0, 2.0, 4.0]));
        assert!(!dawg.insert_f64(&[1.0, 2.0, 3.0])); // Duplicate

        assert!(dawg.contains_f64(&[1.0, 2.0, 3.0]));
        assert!(dawg.contains_f64(&[1.0, 2.0, 4.0]));
        assert!(!dawg.contains_f64(&[1.0, 2.0, 5.0]));
    }

    #[test]
    fn test_f64_edge_cases() {
        let dawg: DynamicDawgU64<()> = DynamicDawgU64::new();

        // Test special float values
        dawg.insert_f64(&[0.0, f64::INFINITY, f64::NEG_INFINITY]);
        dawg.insert_f64(&[-0.0]); // Different bit pattern from +0.0

        assert!(dawg.contains_f64(&[0.0, f64::INFINITY, f64::NEG_INFINITY]));
        assert!(dawg.contains_f64(&[-0.0]));

        // NaN requires bit-pattern comparison
        let nan_bits = f64::NAN.to_bits();
        dawg.insert_sequence(&[nan_bits]);
        assert!(dawg.contains_sequence(&[nan_bits]));
    }

    #[test]
    fn test_valued_dawg() {
        let dawg: DynamicDawgU64<u32> = DynamicDawgU64::new();

        assert!(dawg.insert_sequence_with_value(&[1, 2, 3], 42));
        assert!(!dawg.insert_sequence_with_value(&[1, 2, 3], 99)); // Updates value

        assert_eq!(dawg.get_sequence_value(&[1, 2, 3]), Some(99));
        assert_eq!(dawg.get_sequence_value(&[1, 2, 4]), None);
    }

    #[test]
    fn test_string_api() {
        let dawg: DynamicDawgU64<()> = DynamicDawgU64::new();

        assert!(dawg.insert("hello"));
        assert!(!dawg.insert("hello")); // Duplicate
        assert!(dawg.insert("world"));

        assert!(dawg.contains("hello"));
        assert!(dawg.contains("world"));
        assert!(!dawg.contains("foo"));
    }

    #[test]
    fn test_clone() {
        let dawg: DynamicDawgU64<()> = DynamicDawgU64::new();
        dawg.insert_sequence(&[1, 2, 3]);
        dawg.insert_sequence(&[4, 5, 6]);

        let cloned = dawg.clone();
        assert_eq!(cloned.term_count(), dawg.term_count());
        assert!(cloned.contains_sequence(&[1, 2, 3]));
        assert!(cloned.contains_sequence(&[4, 5, 6]));

        // Modifications to clone don't affect original
        cloned.insert_sequence(&[7, 8, 9]);
        assert!(cloned.contains_sequence(&[7, 8, 9]));
        assert!(!dawg.contains_sequence(&[7, 8, 9]));
    }

    #[test]
    fn test_concurrent_inserts() {
        use std::sync::Arc as StdArc;
        use std::thread;

        let dawg = StdArc::new(DynamicDawgU64::<()>::new());
        let num_threads = 8;
        let sequences_per_thread = 100;

        let handles: Vec<_> = (0..num_threads)
            .map(|t| {
                let d = dawg.clone();
                thread::spawn(move || {
                    for i in 0..sequences_per_thread {
                        let seq = vec![t as u64, i as u64];
                        d.insert_sequence(&seq);
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }

        // All sequences should be present
        assert_eq!(dawg.term_count(), num_threads * sequences_per_thread);

        for t in 0..num_threads {
            for i in 0..sequences_per_thread {
                assert!(dawg.contains_sequence(&[t as u64, i as u64]));
            }
        }
    }

    #[test]
    fn test_concurrent_reads_and_writes() {
        use std::sync::Arc as StdArc;
        use std::thread;

        let dawg = StdArc::new(DynamicDawgU64::<()>::new());

        // Pre-populate with some data
        for i in 0..100 {
            dawg.insert_sequence(&[i, i + 1, i + 2]);
        }

        let handles: Vec<_> = (0..10)
            .map(|t| {
                let d = dawg.clone();
                thread::spawn(move || {
                    if t % 2 == 0 {
                        // Writer
                        for i in 100 + t * 10..100 + (t + 1) * 10 {
                            d.insert_sequence(&[i as u64, i as u64 + 1]);
                        }
                    } else {
                        // Reader
                        for i in 0..100 {
                            let _ = d.contains_sequence(&[i, i + 1, i + 2]);
                        }
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }

        // Original data should still be there
        for i in 0..100u64 {
            assert!(dawg.contains_sequence(&[i, i + 1, i + 2]));
        }
    }

    #[test]
    fn test_dictionary_trait() {
        let dawg: DynamicDawgU64<()> = DynamicDawgU64::new();
        dawg.insert_sequence(&[1, 2, 3]);

        let root = dawg.root();
        assert!(!root.is_final());

        let n1 = root.transition(1).expect("Should have transition");
        assert!(!n1.is_final());

        let n2 = n1.transition(2).expect("Should have transition");
        assert!(!n2.is_final());

        let n3 = n2.transition(3).expect("Should have transition");
        assert!(n3.is_final());

        assert!(n2.transition(9).is_none());
    }

    #[test]
    fn test_from_terms() {
        let terms = vec!["apple", "banana", "cherry"];
        let dawg: DynamicDawgU64<()> = DynamicDawgU64::from_terms(terms);

        assert!(dawg.contains("apple"));
        assert!(dawg.contains("banana"));
        assert!(dawg.contains("cherry"));
        assert_eq!(dawg.term_count(), 3);
    }

    #[test]
    fn sorted_and_unordered_bulk_builders_share_the_minimal_kernel() {
        let sorted: DynamicDawgU64<()> = DynamicDawgU64::from_sorted_terms(["ab", "cb"]);
        let unordered: DynamicDawgU64<()> = DynamicDawgU64::from_terms(["cb", "ab"]);

        for dawg in [&sorted, &unordered] {
            assert_eq!(dawg.node_count(), 2);
            assert_eq!(dawg.term_count(), 2);
            assert!(dawg.contains("ab"));
            assert!(dawg.contains("cb"));
        }
    }

    #[test]
    fn mapped_bulk_builders_preserve_values_and_duplicate_precedence() {
        let unordered =
            DynamicDawgU64::from_terms_with_values([("cb", 3_u32), ("ab", 1), ("ab", 2)]);
        let sorted =
            DynamicDawgU64::from_sorted_terms_with_values([("ab", 1_u32), ("ab", 2), ("cb", 3)]);

        for dawg in [&unordered, &sorted] {
            assert_eq!(dawg.term_count(), 2);
            assert_eq!(dawg.get_value("ab"), Some(2));
            assert_eq!(dawg.get_value("cb"), Some(3));
        }
    }

    #[test]
    #[should_panic(expected = "requires lexicographically nondecreasing input")]
    fn mapped_sorted_builder_rejects_decreasing_input() {
        let _ = DynamicDawgU64::from_sorted_terms_with_values([("z", 1_u32), ("a", 2)]);
    }

    #[test]
    fn test_extend() {
        let dawg: DynamicDawgU64<()> = DynamicDawgU64::new();
        dawg.insert("existing");

        let added = dawg.extend(vec!["new1", "new2", "existing"]);
        assert_eq!(added, 2); // Only 2 new terms
        assert_eq!(dawg.term_count(), 3);
    }

    #[test]
    fn test_node_count() {
        let dawg: DynamicDawgU64<()> = DynamicDawgU64::new();
        assert_eq!(dawg.node_count(), 1); // Just root

        dawg.insert_sequence(&[1, 2, 3]);
        assert_eq!(dawg.node_count(), 4); // root + 3 nodes

        dawg.insert_sequence(&[1, 2, 4]);
        assert_eq!(dawg.node_count(), 5); // Shares [1, 2] prefix
    }

    #[test]
    fn test_prefix_sharing() {
        let dawg: DynamicDawgU64<()> = DynamicDawgU64::new();

        dawg.insert_sequence(&[1, 2, 3]);
        dawg.insert_sequence(&[1, 2, 4]);
        dawg.insert_sequence(&[1, 2, 5]);

        // All share the [1, 2] prefix
        // root -> 1 -> 2 -> {3, 4, 5}
        // That's 1 (root) + 1 (node 1) + 1 (node 2) + 3 (nodes 3,4,5) = 6 nodes
        assert_eq!(dawg.node_count(), 6);
    }

    // ==================== Concurrency Stress Tests ====================
    // These tests verify the lock-free implementation under heavy concurrent load

    #[test]
    fn test_stress_100_concurrent_readers() {
        use std::sync::Arc as StdArc;
        use std::thread;

        // Pre-populate the DAWG
        let dawg: DynamicDawgU64<()> = DynamicDawgU64::new();
        for i in 0u64..1000 {
            dawg.insert_sequence(&[i, i + 1, i + 2]);
        }
        let dawg = StdArc::new(dawg);

        // Spawn 100 concurrent readers
        let handles: Vec<_> = (0..100)
            .map(|reader_id| {
                let dawg = StdArc::clone(&dawg);
                thread::spawn(move || {
                    // Each reader does 1000 lookups
                    for i in 0u64..1000 {
                        let seq = [i, i + 1, i + 2];
                        let found = dawg.contains_sequence(&seq);
                        assert!(found, "Reader {reader_id} failed to find sequence {i}");
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.join().expect("Reader thread panicked");
        }
    }

    #[test]
    fn test_stress_readers_and_writers() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc as StdArc;
        use std::thread;

        let dawg: DynamicDawgU64<()> = DynamicDawgU64::new();
        // Pre-populate with some data
        for i in 0u64..100 {
            dawg.insert_sequence(&[i, i + 1]);
        }
        let dawg = StdArc::new(dawg);
        let stop = StdArc::new(AtomicBool::new(false));

        // 10 reader threads
        let reader_handles: Vec<_> = (0..10)
            .map(|_| {
                let dawg = StdArc::clone(&dawg);
                let stop = StdArc::clone(&stop);
                thread::spawn(move || {
                    let mut reads = 0u64;
                    while !stop.load(Ordering::Relaxed) {
                        // Read pre-existing keys
                        for i in 0u64..100 {
                            let _ = dawg.contains_sequence(&[i, i + 1]);
                            reads += 1;
                        }
                    }
                    reads
                })
            })
            .collect();

        // 10 writer threads
        let writer_handles: Vec<_> = (0..10)
            .map(|writer_id| {
                let dawg = StdArc::clone(&dawg);
                thread::spawn(move || {
                    // Each writer inserts 100 sequences in its own range
                    let base = 1000 + (writer_id as u64 * 100);
                    for i in 0u64..100 {
                        dawg.insert_sequence(&[base + i, base + i + 1, base + i + 2]);
                    }
                })
            })
            .collect();

        // Wait for writers to complete
        for handle in writer_handles {
            handle.join().expect("Writer thread panicked");
        }

        // Signal readers to stop
        stop.store(true, Ordering::Relaxed);

        // Wait for readers
        let total_reads: u64 = reader_handles
            .into_iter()
            .map(|h| h.join().expect("Reader thread panicked"))
            .sum();

        // Verify all inserted sequences exist
        for writer_id in 0..10 {
            let base = 1000 + (writer_id as u64 * 100);
            for i in 0u64..100 {
                assert!(
                    dawg.contains_sequence(&[base + i, base + i + 1, base + i + 2]),
                    "Missing sequence from writer {writer_id} at offset {i}"
                );
            }
        }

        // Should have done many reads while writes were happening
        assert!(total_reads > 1000, "Expected many reads, got {total_reads}");
    }

    #[test]
    fn test_stress_50_writers_same_keys() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc as StdArc;
        use std::thread;

        let dawg: DynamicDawgU64<()> = DynamicDawgU64::new();
        let dawg = StdArc::new(dawg);
        let successful_inserts = StdArc::new(AtomicUsize::new(0));

        // 50 writers all trying to insert the same 100 sequences
        let handles: Vec<_> = (0..50)
            .map(|_| {
                let dawg = StdArc::clone(&dawg);
                let successful_inserts = StdArc::clone(&successful_inserts);
                thread::spawn(move || {
                    for i in 0u64..100 {
                        if dawg.insert_sequence(&[i, i + 1, i + 2]) {
                            successful_inserts.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.join().expect("Writer thread panicked");
        }

        // Exactly 100 unique sequences should exist
        assert_eq!(dawg.term_count(), 100);

        // Exactly 100 successful inserts (one per unique sequence)
        assert_eq!(successful_inserts.load(Ordering::Relaxed), 100);

        // Verify all sequences exist
        for i in 0u64..100 {
            assert!(dawg.contains_sequence(&[i, i + 1, i + 2]));
        }
    }

    #[test]
    fn test_stress_50_writers_disjoint_keys() {
        use std::sync::Arc as StdArc;
        use std::thread;

        let dawg: DynamicDawgU64<()> = DynamicDawgU64::new();
        let dawg = StdArc::new(dawg);

        // 50 writers, each inserting 100 unique sequences in disjoint ranges
        let handles: Vec<_> = (0..50)
            .map(|writer_id| {
                let dawg = StdArc::clone(&dawg);
                thread::spawn(move || {
                    let base = writer_id as u64 * 1000;
                    for i in 0u64..100 {
                        let inserted = dawg.insert_sequence(&[base + i, base + i + 1]);
                        assert!(
                            inserted,
                            "Writer {writer_id} failed to insert unique seq {i}"
                        );
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.join().expect("Writer thread panicked");
        }

        // 50 writers × 100 sequences = 5000 total
        assert_eq!(dawg.term_count(), 5000);

        // Verify all sequences exist
        for writer_id in 0u64..50 {
            let base = writer_id * 1000;
            for i in 0u64..100 {
                assert!(
                    dawg.contains_sequence(&[base + i, base + i + 1]),
                    "Missing sequence from writer {writer_id} at offset {i}"
                );
            }
        }
    }

    #[test]
    fn test_stress_valued_concurrent_writes() {
        use std::sync::Arc as StdArc;
        use std::thread;

        let dawg: DynamicDawgU64<u64> = DynamicDawgU64::new();
        let dawg = StdArc::new(dawg);

        // 20 writers, each inserting sequences with values
        let handles: Vec<_> = (0..20)
            .map(|writer_id| {
                let dawg = StdArc::clone(&dawg);
                thread::spawn(move || {
                    let base = writer_id as u64 * 100;
                    for i in 0u64..50 {
                        let value = base + i;
                        dawg.insert_sequence_with_value(&[base + i], value);
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.join().expect("Writer thread panicked");
        }

        // 20 writers × 50 sequences = 1000 total
        assert_eq!(dawg.term_count(), 1000);

        // Verify values are correct
        for writer_id in 0u64..20 {
            let base = writer_id * 100;
            for i in 0u64..50 {
                let expected_value = base + i;
                let value = dawg.get_sequence_value(&[base + i]);
                assert_eq!(
                    value,
                    Some(expected_value),
                    "Wrong value for sequence [{base} + {i}]"
                );
            }
        }
    }

    #[test]
    fn test_stress_remove_while_reading() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc as StdArc;
        use std::thread;

        let dawg: DynamicDawgU64<()> = DynamicDawgU64::new();
        // Insert 1000 sequences
        for i in 0u64..1000 {
            dawg.insert_sequence(&[i, i + 1]);
        }
        let dawg = StdArc::new(dawg);
        let stop = StdArc::new(AtomicBool::new(false));

        // 5 reader threads
        let reader_handles: Vec<_> = (0..5)
            .map(|_| {
                let dawg = StdArc::clone(&dawg);
                let stop = StdArc::clone(&stop);
                thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        // Read random sequences - some may have been removed
                        for i in 0u64..1000 {
                            let _ = dawg.contains_sequence(&[i, i + 1]);
                        }
                    }
                })
            })
            .collect();

        // 5 remover threads
        let remover_handles: Vec<_> = (0..5)
            .map(|remover_id| {
                let dawg = StdArc::clone(&dawg);
                thread::spawn(move || {
                    // Each remover removes 200 sequences in its range
                    let base = remover_id as u64 * 200;
                    for i in 0u64..200 {
                        dawg.remove_sequence(&[base + i, base + i + 1]);
                    }
                })
            })
            .collect();

        // Wait for removers
        for handle in remover_handles {
            handle.join().expect("Remover thread panicked");
        }

        // Signal readers to stop
        stop.store(true, Ordering::Relaxed);

        // Wait for readers
        for handle in reader_handles {
            handle.join().expect("Reader thread panicked");
        }

        // After 5 removers × 200 = 1000 removals from 1000 sequences
        assert_eq!(dawg.term_count(), 0);
    }

    #[test]
    fn test_stress_iterator_during_writes() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc as StdArc;
        use std::thread;

        let dawg: DynamicDawgU64<()> = DynamicDawgU64::new();
        // Pre-populate
        for i in 0u64..100 {
            dawg.insert_sequence(&[i]);
        }
        let dawg = StdArc::new(dawg);
        let stop = StdArc::new(AtomicBool::new(false));

        // Iterator thread - iterates while writes happen
        let iter_dawg = StdArc::clone(&dawg);
        let iter_stop = StdArc::clone(&stop);
        let iter_handle = thread::spawn(move || {
            let mut iteration_count = 0;
            while !iter_stop.load(Ordering::Relaxed) {
                // Collect all terms (snapshot at time of iteration start)
                let terms: Vec<_> = iter_dawg.iter().collect();
                // Should have at least the initial 100
                assert!(terms.len() >= 100);
                iteration_count += 1;
            }
            iteration_count
        });

        // Writer threads add more sequences
        let writer_handles: Vec<_> = (0..10)
            .map(|writer_id| {
                let dawg = StdArc::clone(&dawg);
                thread::spawn(move || {
                    let base = 1000 + writer_id as u64 * 100;
                    for i in 0u64..100 {
                        dawg.insert_sequence(&[base + i]);
                    }
                })
            })
            .collect();

        // Wait for writers
        for handle in writer_handles {
            handle.join().expect("Writer thread panicked");
        }

        // Signal iterator to stop
        stop.store(true, Ordering::Relaxed);

        let iterations = iter_handle.join().expect("Iterator thread panicked");
        assert!(
            iterations > 0,
            "Iterator thread should have run at least once"
        );

        // Final count: 100 initial + 10 writers × 100 = 1100
        assert_eq!(dawg.term_count(), 1100);
    }
}
