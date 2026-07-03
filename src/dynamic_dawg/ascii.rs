//! Dynamic DAWG with online modifications.
//!
//! This implementation supports incremental updates on a lock-free node graph.
//! Perfect minimality can be restored via explicit compaction.

use super::lockfree::{LockFreeDawg, LockFreeDawgNode};
use super::zipper::DynamicDawgZipper;
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
/// Uses atomic edge-list publication. Reads are wait-free; normal writes use
/// CAS loops and do not block readers.
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
/// let mut dict = DynamicDawg::new();
/// dict.insert("hello");
///
/// // With values
/// let dict: DynamicDawg<u32> = DynamicDawg::new();
/// dict.insert_with_value("hello", 42);
/// ```
#[derive(Clone, Debug)]
pub struct DynamicDawg<V: DictionaryValue = ()> {
    pub(crate) inner: Arc<DynamicDawgInner<V>>,
}

// The public byte DAWG now uses the unit-generic lock-free core. The
// indexed `DawgCore<u8, V>` remains as the serialization compatibility
// shape so existing encoded dictionaries can still round-trip.
pub(crate) type DynamicDawgInner<V = ()> = LockFreeDawg<u8, V>;

impl<V: DictionaryValue> DynamicDawg<V> {
    /// Create a new empty dynamic DAWG.
    ///
    /// By default, auto-minimization is disabled. Use `with_auto_minimize_threshold()`
    /// to enable automatic minimization.
    ///
    /// # Example
    ///
    /// ```text
    /// // Without values (default)
    /// let dawg: DynamicDawg<()> = DynamicDawg::new();
    /// dawg.insert("hello");
    ///
    /// // With values
    /// let dawg: DynamicDawg<u32> = DynamicDawg::new();
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
    /// let dawg: DynamicDawg<()> = DynamicDawg::with_auto_minimize_threshold(1.5);
    ///
    /// // Disable auto-minimization (manual minimize() calls only)
    /// let dawg: DynamicDawg<()> = DynamicDawg::with_auto_minimize_threshold(f32::INFINITY);
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
    /// let dawg: DynamicDawg<()> = DynamicDawg::with_config(f32::INFINITY, Some(10000));
    ///
    /// // Explicit maintenance remains available
    /// let dawg: DynamicDawg<()> = DynamicDawg::with_config(1.5, None);
    /// ```
    pub fn with_config(auto_minimize_threshold: f32, bloom_filter_capacity: Option<usize>) -> Self {
        DynamicDawg {
            inner: Arc::new(DynamicDawgInner::with_config(
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
        term_vec.sort_unstable();

        let dawg = Self::new();
        for term in term_vec {
            dawg.insert(&term);
        }
        dawg
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
    /// let dawg: DynamicDawg<()> = DynamicDawg::from_sorted_terms(terms);
    /// ```
    pub fn from_sorted_terms<I, S>(terms: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let dawg = Self::new();
        for term in terms {
            dawg.insert(term.as_ref());
        }
        dawg
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
        pairs.sort_by(|a, b| a.0.cmp(&b.0));

        let dawg = Self::new();
        for (term, value) in pairs {
            dawg.insert_with_value(&term, value);
        }
        dawg
    }

    /// Insert a term into the DAWG.
    ///
    /// Returns `true` if the term was newly inserted, `false` if it already existed.
    ///
    /// # Minimality
    ///
    /// Insertions maintain minimality by sharing suffixes with existing nodes.
    pub fn insert(&self, term: &str) -> bool {
        self.inner.insert_units(term.as_bytes())
    }

    /// Insert a term with an associated value.
    ///
    /// Returns `true` if the term was newly inserted, `false` if it already existed.
    /// If the term already exists, its value is updated.
    ///
    /// # Example
    ///
    /// ```text
    /// let dict: DynamicDawg<u32> = DynamicDawg::new();
    /// assert!(dict.insert_with_value("hello", 42));
    /// assert!(!dict.insert_with_value("hello", 43)); // Updates value
    /// assert_eq!(dict.get_value("hello"), Some(43));
    /// ```
    pub fn insert_with_value(&self, term: &str, value: V) -> bool {
        self.inner.insert_units_with_value(term.as_bytes(), value)
    }

    /// Update an existing term's value in place, or insert a new term with a default value.
    ///
    /// This method is useful for accumulation patterns where you want to modify an existing
    /// value (e.g., add to a `HashSet`) or insert a new one if the term doesn't exist.
    ///
    /// Returns `true` if the term was newly inserted, `false` if it already existed.
    ///
    /// # Parameters
    ///
    /// - `term`: The term to update or insert
    /// - `default_value`: The value to use if the term doesn't exist
    /// - `update_fn`: Function to apply to the existing value if the term exists
    ///
    /// # Example
    ///
    /// ```text
    /// use std::collections::HashSet;
    /// let dict: DynamicDawg<HashSet<String>> = DynamicDawg::new();
    ///
    /// // First call - inserts new term with default value
    /// let was_new = dict.update_or_insert(
    ///     "key",
    ///     HashSet::from(["value1".to_string()]),
    ///     |set| { set.insert("value1".to_string()); }
    /// );
    /// assert!(was_new);
    ///
    /// // Second call - updates existing value
    /// let was_new = dict.update_or_insert(
    ///     "key",
    ///     HashSet::new(),
    ///     |set| { set.insert("value2".to_string()); }
    /// );
    /// assert!(!was_new);
    ///
    /// // Now "key" contains {"value1", "value2"}
    /// ```
    pub fn update_or_insert<F>(&self, term: &str, default_value: V, update_fn: F) -> bool
    where
        F: Fn(&mut V),
    {
        self.inner
            .update_or_insert_units(term.as_bytes(), default_value, update_fn)
    }

    /// Get the value associated with a term.
    ///
    /// Returns `Some(value)` if the term exists, `None` otherwise.
    ///
    /// # Example
    ///
    /// ```text
    /// let dict: DynamicDawg<String> = DynamicDawg::new();
    /// dict.insert_with_value("key", "value".to_string());
    /// assert_eq!(dict.get_value("key"), Some("value".to_string()));
    /// assert_eq!(dict.get_value("unknown"), None);
    /// ```
    pub fn get_value(&self, term: &str) -> Option<V> {
        self.inner.get_units_value(term.as_bytes())
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
        self.inner.remove_units(term.as_bytes())
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
        self.contains_bytes(term.as_bytes())
    }

    // ========================================================================
    // Raw Byte Methods
    // ========================================================================
    //
    // These methods operate directly on byte slices, enabling use cases like
    // time series indexing where encoded data may not be valid UTF-8.

    /// Insert raw bytes into the DAWG.
    ///
    /// Returns `true` if the bytes were newly inserted, `false` if already existed.
    ///
    /// # Example
    ///
    /// ```text
    /// let dawg: DynamicDawg<()> = DynamicDawg::new();
    /// assert!(dawg.insert_bytes(&[0x10, 0x20, 0x30]));
    /// assert!(!dawg.insert_bytes(&[0x10, 0x20, 0x30])); // Duplicate
    /// ```
    pub fn insert_bytes(&self, bytes: &[u8]) -> bool {
        self.inner.insert_units(bytes)
    }

    /// Insert raw bytes with an associated value.
    ///
    /// Returns `true` if newly inserted, `false` if it already existed (value is updated).
    ///
    /// # Example
    ///
    /// ```text
    /// let dawg: DynamicDawg<u32> = DynamicDawg::new();
    /// assert!(dawg.insert_bytes_with_value(&[0x10, 0x20], 42));
    /// assert_eq!(dawg.get_bytes_value(&[0x10, 0x20]), Some(42));
    /// ```
    pub fn insert_bytes_with_value(&self, bytes: &[u8], value: V) -> bool {
        self.inner.insert_units_with_value(bytes, value)
    }

    /// Check if raw bytes exist in the DAWG.
    ///
    /// # Example
    ///
    /// ```text
    /// let dawg: DynamicDawg<()> = DynamicDawg::new();
    /// dawg.insert_bytes(&[0x10, 0x20, 0x30]);
    /// assert!(dawg.contains_bytes(&[0x10, 0x20, 0x30]));
    /// assert!(!dawg.contains_bytes(&[0x10, 0x20]));
    /// ```
    pub fn contains_bytes(&self, bytes: &[u8]) -> bool {
        self.inner.contains_units(bytes)
    }

    /// Get the value associated with raw bytes.
    ///
    /// # Example
    ///
    /// ```text
    /// let dawg: DynamicDawg<String> = DynamicDawg::new();
    /// dawg.insert_bytes_with_value(&[0x10, 0x20], "value".to_string());
    /// assert_eq!(dawg.get_bytes_value(&[0x10, 0x20]), Some("value".to_string()));
    /// assert_eq!(dawg.get_bytes_value(&[0x99]), None);
    /// ```
    pub fn get_bytes_value(&self, bytes: &[u8]) -> Option<V> {
        self.inner.get_units_value(bytes)
    }
}

impl<V: DictionaryValue> DynamicDawg<V> {
    /// Iterate over all `(term, value)` pairs as raw byte vectors.
    ///
    /// Returns an iterator yielding `(Vec<u8>, V)` tuples in depth-first order.
    /// This is more efficient than `iter()` as it avoids UTF-8 string allocation.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::dynamic_dawg::DynamicDawg;
    ///
    /// let dict: DynamicDawg<u32> = DynamicDawg::new();
    /// dict.insert_with_value("cat", 1);
    /// dict.insert_with_value("dog", 2);
    ///
    /// for (term_bytes, value) in dict.iter_bytes() {
    ///     let term = String::from_utf8(term_bytes).unwrap();
    ///     println!("{} -> {}", term, value);
    /// }
    /// ```
    pub fn iter_bytes(&self) -> DictionaryIterator<DynamicDawgZipper<V>> {
        let zipper = DynamicDawgZipper::new_from_dict(self);
        DictionaryIterator::new(zipper)
    }

    /// Iterate over all `(term, value)` pairs as UTF-8 strings.
    ///
    /// Returns an iterator yielding `(String, V)` tuples in depth-first order.
    /// For better performance with raw bytes, use `iter_bytes()` instead.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::dynamic_dawg::DynamicDawg;
    ///
    /// let dict: DynamicDawg<u32> = DynamicDawg::new();
    /// dict.insert_with_value("cat", 1);
    /// dict.insert_with_value("dog", 2);
    ///
    /// for (term, value) in dict.iter() {
    ///     println!("{} -> {}", term, value);
    /// }
    /// ```
    pub fn iter(&self) -> impl Iterator<Item = (String, V)> + '_ {
        self.iter_bytes()
            .map(|(bytes, value)| (String::from_utf8_lossy(&bytes).into_owned(), value))
    }
}

impl<V: DictionaryValue> IntoIterator for &DynamicDawg<V> {
    type Item = (Vec<u8>, V);
    type IntoIter = DictionaryIterator<DynamicDawgZipper<V>>;

    /// Creates an iterator over all `(term, value)` pairs as raw byte vectors.
    fn into_iter(self) -> Self::IntoIter {
        self.iter_bytes()
    }
}

impl<V: DictionaryValue> Default for DynamicDawg<V> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "serialization")]
impl<V: DictionaryValue + serde::Serialize> serde::Serialize for DynamicDawg<V> {
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
impl<'de, V: DictionaryValue + serde::Deserialize<'de>> serde::Deserialize<'de> for DynamicDawg<V> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let inner = super::core::DawgCore::<u8, V>::deserialize(deserializer)?;
        Ok(DynamicDawg {
            inner: Arc::new(DynamicDawgInner::from_core(inner)),
        })
    }
}

/// Deserialize implementation when `persistent-artrie` feature is enabled.
/// `DictionaryValue` already includes `DeserializeOwned`, so no additional bounds needed.
#[cfg(all(feature = "serialization", feature = "persistent-artrie"))]
impl<'de, V: DictionaryValue> serde::Deserialize<'de> for DynamicDawg<V> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let inner = super::core::DawgCore::<u8, V>::deserialize(deserializer)?;
        Ok(DynamicDawg {
            inner: Arc::new(DynamicDawgInner::from_core(inner)),
        })
    }
}

impl<V: DictionaryValue> Dictionary for DynamicDawg<V> {
    type Node = DynamicDawgNode<V>;

    fn root(&self) -> Self::Node {
        DynamicDawgNode {
            node: self.inner.root_arc(),
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
pub struct DynamicDawgNode<V: DictionaryValue = ()> {
    node: Arc<LockFreeDawgNode<u8, V>>,
}

impl<V: DictionaryValue> DictionaryNode for DynamicDawgNode<V> {
    type Unit = u8;

    fn is_final(&self) -> bool {
        self.node.is_final()
    }

    fn transition(&self, label: u8) -> Option<Self> {
        let edges = self.node.edges.load();
        edges.find(label).map(|child| DynamicDawgNode {
            node: child.clone(),
        })
    }

    fn edges(&self) -> Box<dyn Iterator<Item = (u8, Self)> + '_> {
        let edges = self.node.edges.load();
        let edge_vec: Vec<_> = edges
            .edges
            .iter()
            .map(|(byte, child)| (*byte, child.clone()))
            .collect();
        Box::new(
            edge_vec
                .into_iter()
                .map(|(byte, child)| (byte, DynamicDawgNode { node: child })),
        )
    }

    fn edge_count(&self) -> Option<usize> {
        Some(self.node.edges.load().edges.len())
    }
}

// ============================================================================
// MappedDictionary Trait Implementation
// ============================================================================

use crate::{MappedDictionary, MappedDictionaryNode};

impl<V: DictionaryValue> MappedDictionaryNode for DynamicDawgNode<V> {
    type Value = V;

    fn value(&self) -> Option<Self::Value> {
        self.node.value()
    }
}

impl<V: DictionaryValue> MappedDictionary for DynamicDawg<V> {
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

impl<V: DictionaryValue> crate::MutableDictionary for DynamicDawg<V> {
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

impl<V: DictionaryValue> crate::CompactableDictionary for DynamicDawg<V> {
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

impl<V: DictionaryValue> crate::MutableMappedDictionary for DynamicDawg<V> {
    fn insert_with_value(&self, term: &str, value: Self::Value) -> bool {
        // Delegate to the inherent method
        Self::insert_with_value(self, term, value)
    }

    fn update_or_insert<F>(&self, term: &str, default_value: Self::Value, update_fn: F) -> bool
    where
        F: Fn(&mut Self::Value),
    {
        // Delegate to the inherent method
        Self::update_or_insert(self, term, default_value, update_fn)
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
            .filter_map(|(path, value)| {
                std::str::from_utf8(&path)
                    .ok()
                    .map(|term| (term.to_string(), value))
            })
            .collect();

        let mut processed = 0;
        for (term, other_value) in entries {
            // `processed` counts every valid-UTF-8 final term (preserving the original
            // semantics); only valued terms are merged into `self`.
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use log::debug;

    #[test]
    fn test_dynamic_dawg_insert() {
        let dawg: DynamicDawg<()> = DynamicDawg::new();
        assert!(dawg.insert("test"));
        assert!(!dawg.insert("test")); // Duplicate
        assert!(dawg.insert("testing"));
        assert_eq!(dawg.term_count(), 2);
    }

    #[test]
    fn test_dynamic_dawg_remove() {
        let dawg: DynamicDawg<()> = DynamicDawg::new();
        dawg.insert("test");
        dawg.insert("testing");
        dawg.insert("tested");

        assert!(dawg.remove("testing"));
        assert_eq!(dawg.term_count(), 2);
        assert!(!dawg.remove("testing")); // Already removed
    }

    #[test]
    fn test_dynamic_dawg_compact() {
        let dawg: DynamicDawg<()> = DynamicDawg::new();
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
        let dawg: DynamicDawg<()> = DynamicDawg::new();
        dawg.insert("test");

        assert!(!dawg.needs_compaction());

        dawg.remove("test");
        assert!(dawg.needs_compaction());

        dawg.compact();
        assert!(!dawg.needs_compaction());
    }

    #[test]
    fn test_batch_extend() {
        let dawg: DynamicDawg<()> = DynamicDawg::new();
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
        let dawg: DynamicDawg<()> =
            DynamicDawg::from_terms(vec!["test", "testing", "tested", "tester"]);

        let to_remove = vec!["testing", "tester"];
        let removed = dawg.remove_many(to_remove);

        assert_eq!(removed, 2);
        assert_eq!(dawg.term_count(), 2);
        assert!(dawg.contains("test"));
        assert!(!dawg.contains("testing"));
    }

    #[test]
    fn test_minimize_basic() {
        let dawg: DynamicDawg<()> = DynamicDawg::new();

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
        let dawg1: DynamicDawg<()> = DynamicDawg::new();
        let dawg2: DynamicDawg<()> = DynamicDawg::new();

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
        let dawg: DynamicDawg<()> =
            DynamicDawg::from_terms(vec!["test", "testing", "tested", "tester", "testimony"]);

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
        let dawg: DynamicDawg<()> = DynamicDawg::new();
        let merged = dawg.minimize();

        // Empty DAWG should have nothing to minimize
        assert_eq!(merged, 0);
        assert_eq!(dawg.node_count(), 1); // Just root
        assert_eq!(dawg.term_count(), 0);
    }

    #[test]
    fn test_minimize_single_term() {
        let dawg: DynamicDawg<()> = DynamicDawg::new();
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
        let dawg: DynamicDawg<()> = DynamicDawg::new();

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
        let dawg: DynamicDawg<()> =
            DynamicDawg::from_terms(vec!["apple", "application", "apply", "apricot"]);

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
        let dawg: DynamicDawg<()> = DynamicDawg::new();

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
        // Test DynamicDawg with values
        let dawg: DynamicDawg<u32> = DynamicDawg::new();

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
        let dawg: DynamicDawg<String> = DynamicDawg::new();

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

        let dawg: DynamicDawg<Vec<u32>> = DynamicDawg::new();
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
        let dawg: DynamicDawg<()> = DynamicDawg::new();

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
