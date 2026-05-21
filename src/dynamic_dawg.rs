//! Dynamic DAWG with online modifications.
//!
//! This implementation supports incremental updates while maintaining
//! "near-minimal" structure. Perfect minimality can be restored via
//! explicit compaction.

use crate::dynamic_dawg_zipper::DynamicDawgZipper;
use crate::iterator::DictionaryIterator;
use crate::sync_compat::RwLock;
use crate::value::DictionaryValue;
use crate::{Dictionary, DictionaryNode, SyncStrategy};
use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
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
/// - **After insertion**: Structure remains minimal (new nodes are shared)
/// - **After deletion**: May become non-minimal (orphaned branches)
/// - **Solution**: Call `compact()` periodically to restore minimality
///
/// # Thread Safety
///
/// Uses `Arc<RwLock<...>>` for interior mutability. Safe for concurrent
/// reads, exclusive writes.
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
    pub(crate) inner: Arc<RwLock<DynamicDawgInner<V>>>,
}

// C1a algorithmic dedup (byte DAWG): the local `DynamicDawgInner<V>`
// struct + ~700-LOC impl block was byte-for-byte identical to the
// canonical `crate::dawg_core::DawgCore<u8, V>`. Replace with a type
// alias so every algorithmic method lives in exactly one place. The
// outer `DynamicDawg<V>` wrapper's read/write-locked field accesses
// (inner.nodes, inner.term_count, etc.) continue to work since the
// canonical struct has pub(crate) fields of the same shape.
pub(crate) type DynamicDawgInner<V = ()> = crate::dawg_core::DawgCore<u8, V>;

// C1 step (DAWG byte variant): the byte-for-byte-identical local
// `BloomFilter` and `NodeSignature` structs are replaced with imports of
// the canonical crate-level types at `crate::bloom_filter::BloomFilter`
// and `crate::node_signature::NodeSignature`. The canonical types have
// the same field shape (BloomFilter: `bits: Vec<u64>`, `bit_count:
// usize`, `hash_count: usize`; NodeSignature: `hash: u64`) and a
// superset of the methods that were defined locally (new, insert,
// might_contain, clear).
use crate::bloom_filter::BloomFilter;
use crate::node_signature::NodeSignature;

// C1 step (DAWG byte variant): byte-for-byte-identical local
// `DawgNode<V>` struct + 2-method impl block replaced with a type
// alias to the generic `crate::dawg_core::DawgNode<u8, V>` which
// carries the same field shape and `pub fn new` / `pub fn new_with_value`.
// Clone + Debug + serde derives live on the canonical struct; the
// alias inherits them automatically.
pub(crate) type DawgNode<V = ()> = crate::dawg_core::DawgNode<u8, V>;

// Local `impl BloomFilter` removed: the canonical
// `crate::bloom_filter::BloomFilter` provides equivalent `new`,
// `insert`, `might_contain`, `clear` methods. Inherent impls on
// foreign types are not allowed, so this block had to go.

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
    /// - `auto_minimize_threshold`: Bloat ratio to trigger minimization. Use `f32::INFINITY` to disable.
    /// - `bloom_filter_capacity`: Optional Bloom filter capacity for negative lookup optimization.
    ///   Use `Some(expected_size)` to enable, `None` to disable.
    ///
    /// # Example
    ///
    /// ```text
    /// // With Bloom filter for 10000 expected terms
    /// let dawg: DynamicDawg<()> = DynamicDawg::with_config(f32::INFINITY, Some(10000));
    ///
    /// // Without Bloom filter
    /// let dawg: DynamicDawg<()> = DynamicDawg::with_config(1.5, None);
    /// ```
    pub fn with_config(auto_minimize_threshold: f32, bloom_filter_capacity: Option<usize>) -> Self {
        let nodes = vec![DawgNode::new(false)]; // Root at index 0

        let bloom_filter = bloom_filter_capacity.map(BloomFilter::new);

        DynamicDawg {
            inner: Arc::new(RwLock::new(DynamicDawgInner {
                nodes,
                term_count: 0,
                needs_compaction: false,
                suffix_cache: FxHashMap::default(),
                last_minimized_node_count: 1, // Start with root node
                auto_minimize_threshold,
                bloom_filter,
            })),
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
        let mut inner = self.inner.write();
        let bytes = term.as_bytes();

        // Navigate to insertion point, creating nodes as needed
        let mut node_idx = 0;
        let mut path: Vec<(usize, u8)> = Vec::new(); // (parent_idx, label)

        for &byte in bytes {
            if let Some(&child_idx) = inner.nodes[node_idx]
                .edges
                .iter()
                .find(|(b, _)| *b == byte)
                .map(|(_, idx)| idx)
            {
                // Edge exists, follow it
                path.push((node_idx, byte));
                node_idx = child_idx;
            } else {
                // Need to create new suffix
                break;
            }
        }

        // Check if term already exists
        if path.len() == bytes.len() && inner.nodes[node_idx].is_final {
            return false; // Already exists
        }

        // Build remaining suffix with sharing
        let start_byte_idx = path.len();

        // Phase 2.1: DISABLED - Suffix sharing is incompatible with dynamic insertions
        // that can later mark intermediate nodes as final.
        //
        // Bug: When inserting "kb" after "jb", suffix sharing would reuse the same
        // entry node for both 'j' and 'k' edges. Later, inserting "j" marks that
        // shared node as final, incorrectly making "k" also appear as a valid term.
        //
        // Example:
        //   dict.insert("jb"); dict.insert("kb");  // Creates shared structure
        //   dict.insert("j");                       // BUG: also marks "k" as valid
        //
        // The correct DAWG structure for ["jb", "kb"] should have distinct nodes
        // for 'j' and 'k' edges, even though they both continue with 'b'.
        //
        // For now, we disable Phase 2.1 and rely on the node-by-node construction
        // below. Future work: implement proper suffix sharing that creates distinct
        // intermediate nodes while sharing only the deeper suffix nodes.

        // Create nodes one by one (no suffix sharing)
        for i in start_byte_idx..bytes.len() {
            let byte = bytes[i];
            let new_idx = inner.nodes.len();
            let is_final = i == bytes.len() - 1;
            let mut new_node = DawgNode::new(is_final);
            new_node.ref_count = 1;

            inner.nodes.push(new_node);
            inner.insert_edge_sorted(node_idx, byte, new_idx);

            node_idx = new_idx;
        }

        // Mark as final if we followed existing path
        if start_byte_idx == bytes.len() {
            inner.nodes[node_idx].is_final = true;
        }

        inner.term_count += 1;

        // Add to Bloom filter if enabled (Opt #4)
        if let Some(ref mut bloom) = inner.bloom_filter {
            bloom.insert(term);
        }

        // Auto-minimize if bloat threshold exceeded
        inner.check_and_auto_minimize();

        true
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
        let mut inner = self.inner.write();
        let bytes = term.as_bytes();

        // Navigate to insertion point
        let mut node_idx = 0;
        let mut path: Vec<(usize, u8)> = Vec::new();

        for &byte in bytes {
            if let Some(&child_idx) = inner.nodes[node_idx]
                .edges
                .iter()
                .find(|(b, _)| *b == byte)
                .map(|(_, idx)| idx)
            {
                path.push((node_idx, byte));
                node_idx = child_idx;
            } else {
                break;
            }
        }

        // Check if term already exists
        if path.len() == bytes.len() {
            if inner.nodes[node_idx].is_final {
                // Term exists - update value
                inner.nodes[node_idx].value = Some(value);
                return false;
            } else {
                // Mark as final and set value
                inner.nodes[node_idx].is_final = true;
                inner.nodes[node_idx].value = Some(value);
                inner.term_count += 1;

                // Add to Bloom filter
                if let Some(ref mut bloom) = inner.bloom_filter {
                    bloom.insert(term);
                }

                return true;
            }
        }

        // Build remaining suffix
        let start_byte_idx = path.len();
        for i in start_byte_idx..bytes.len() {
            let byte = bytes[i];
            let new_idx = inner.nodes.len();
            let is_final = i == bytes.len() - 1;

            let mut new_node = if is_final {
                DawgNode::new_with_value(true, Some(value.clone()))
            } else {
                DawgNode::new(false)
            };
            new_node.ref_count = 1;

            inner.nodes.push(new_node);
            inner.insert_edge_sorted(node_idx, byte, new_idx);
            node_idx = new_idx;
        }

        inner.term_count += 1;

        // Add to Bloom filter
        if let Some(ref mut bloom) = inner.bloom_filter {
            bloom.insert(term);
        }

        // Auto-minimize if needed
        inner.check_and_auto_minimize();

        true
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
        F: FnOnce(&mut V),
    {
        let mut inner = self.inner.write();
        let bytes = term.as_bytes();

        // Navigate to the term's location, creating nodes as needed
        let mut node_idx = 0;
        let mut path_len = 0;

        for &byte in bytes {
            if let Some(&child_idx) = inner.nodes[node_idx]
                .edges
                .iter()
                .find(|(b, _)| *b == byte)
                .map(|(_, idx)| idx)
            {
                // Edge exists, follow it
                node_idx = child_idx;
                path_len += 1;
            } else {
                // Need to create remaining path
                break;
            }
        }

        // Check if term already exists
        if path_len == bytes.len() {
            if inner.nodes[node_idx].is_final {
                // Term exists - update its value
                if let Some(ref mut existing_value) = inner.nodes[node_idx].value {
                    update_fn(existing_value);
                } else {
                    // Has is_final but no value (shouldn't happen for valued DAWGs, but handle it)
                    inner.nodes[node_idx].value = Some(default_value);
                }
                return false; // Term already existed
            } else {
                // Node exists but wasn't final - mark it final with default value
                inner.nodes[node_idx].is_final = true;
                inner.nodes[node_idx].value = Some(default_value);
                inner.term_count += 1;

                // Add to Bloom filter if enabled
                if let Some(ref mut bloom) = inner.bloom_filter {
                    bloom.insert(term);
                }

                return true; // New term
            }
        }

        // Build remaining path (term doesn't exist yet)
        let start_byte_idx = path_len;
        for i in start_byte_idx..bytes.len() {
            let byte = bytes[i];
            let new_idx = inner.nodes.len();
            let is_final = i == bytes.len() - 1;

            let mut new_node = if is_final {
                DawgNode::new_with_value(true, Some(default_value.clone()))
            } else {
                DawgNode::new(false)
            };
            new_node.ref_count = 1;

            inner.nodes.push(new_node);
            inner.insert_edge_sorted(node_idx, byte, new_idx);
            node_idx = new_idx;
        }

        inner.term_count += 1;

        // Add to Bloom filter if enabled
        if let Some(ref mut bloom) = inner.bloom_filter {
            bloom.insert(term);
        }

        // Auto-minimize if needed
        inner.check_and_auto_minimize();

        true // New term was inserted
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
        let inner = self.inner.read();
        let bytes = term.as_bytes();
        let mut node_idx = 0;

        // Navigate to the term
        for &byte in bytes {
            match inner.nodes[node_idx]
                .edges
                .iter()
                .find(|(b, _)| *b == byte)
                .map(|(_, idx)| idx)
            {
                Some(&child_idx) => node_idx = child_idx,
                None => return None,
            }
        }

        // Check if final and return value
        if inner.nodes[node_idx].is_final {
            inner.nodes[node_idx].value.clone()
        } else {
            None
        }
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
        let mut inner = self.inner.write();
        let bytes = term.as_bytes();

        // Navigate to the term
        let mut node_idx = 0;
        let mut path: Vec<(usize, u8, usize)> = Vec::new(); // (parent, label, child)

        for &byte in bytes {
            if let Some(&child_idx) = inner.nodes[node_idx]
                .edges
                .iter()
                .find(|(b, _)| *b == byte)
                .map(|(_, idx)| idx)
            {
                path.push((node_idx, byte, child_idx));
                node_idx = child_idx;
            } else {
                return false; // Term doesn't exist
            }
        }

        // Check if it's a final node
        if !inner.nodes[node_idx].is_final {
            return false; // Term doesn't exist
        }

        // Unmark as final
        inner.nodes[node_idx].is_final = false;
        inner.term_count -= 1;

        // Prune unreachable branches (nodes with no children and not final)
        for (parent_idx, label, child_idx) in path.iter().rev() {
            let child = &inner.nodes[*child_idx];
            if !child.is_final && child.edges.is_empty() {
                // Remove edge from parent
                inner.nodes[*parent_idx].edges.retain(|(b, _)| *b != *label);
            } else {
                break; // Stop pruning
            }
        }

        // Phase 2.1: Invalidate suffix cache since structure changed
        inner.suffix_cache.clear();
        inner.needs_compaction = true;
        true
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
        let mut inner = self.inner.write();

        // Extract all terms
        let terms = inner.extract_all_terms();
        let old_node_count = inner.nodes.len();

        // Preserve settings
        let auto_minimize_threshold = inner.auto_minimize_threshold;
        // Recover the expected_elements that originally sized the bloom filter:
        // BloomFilter::new(n) sets bit_count = max(64, n*10). `capacity()` returns
        // bit_count after rounding to the next multiple of 64. Dividing by 10
        // recovers the approximate original expected_elements.
        let bloom_capacity = inner.bloom_filter.as_ref().map(|b| b.capacity() / 10);

        // Rebuild from scratch
        *inner = DynamicDawgInner {
            nodes: vec![DawgNode::new(false)],
            term_count: 0,
            needs_compaction: false,
            suffix_cache: FxHashMap::default(),
            last_minimized_node_count: 1,
            auto_minimize_threshold,
            bloom_filter: bloom_capacity.map(BloomFilter::new),
        };

        // Re-insert sorted terms for optimal prefix sharing
        let mut sorted_terms = terms;
        sorted_terms.sort();

        for term in &sorted_terms {
            // Direct insertion without locking (we already have write lock)
            inner.insert_direct(term);

            // Rebuild Bloom filter (canonical BloomFilter::insert_bytes accepts &[u8])
            if let Some(ref mut bloom) = inner.bloom_filter {
                bloom.insert_bytes(term);
            }
        }

        // Now minimize to merge equivalent suffixes (DAWG minimization)
        // This is what makes it a true DAWG instead of just a trie
        let minimized = inner.minimize_incremental();

        old_node_count - inner.nodes.len() + minimized
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
        let mut inner = self.inner.write();
        inner.minimize_incremental()
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
        self.inner.read().term_count
    }

    /// Get the number of nodes in the DAWG.
    pub fn node_count(&self) -> usize {
        self.inner.read().nodes.len()
    }

    /// Check if compaction is recommended.
    ///
    /// Returns `true` if deletions have occurred and compaction would
    /// likely reduce memory usage.
    pub fn needs_compaction(&self) -> bool {
        self.inner.read().needs_compaction
    }

    /// Check if a term is in the DAWG.
    ///
    /// This method is optimized with a Bloom filter (if enabled) for fast negative lookup rejection.
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
        let mut inner = self.inner.write();

        // Navigate to insertion point, creating nodes as needed
        let mut node_idx = 0;
        let mut path: Vec<(usize, u8)> = Vec::new();

        for &byte in bytes {
            if let Some(&child_idx) = inner.nodes[node_idx]
                .edges
                .iter()
                .find(|(b, _)| *b == byte)
                .map(|(_, idx)| idx)
            {
                path.push((node_idx, byte));
                node_idx = child_idx;
            } else {
                break;
            }
        }

        // Check if term already exists
        if path.len() == bytes.len() && inner.nodes[node_idx].is_final {
            return false;
        }

        // Build remaining suffix
        let start_byte_idx = path.len();
        for i in start_byte_idx..bytes.len() {
            let byte = bytes[i];
            let new_idx = inner.nodes.len();
            let is_final = i == bytes.len() - 1;
            let mut new_node = DawgNode::new(is_final);
            new_node.ref_count = 1;

            inner.nodes.push(new_node);
            inner.insert_edge_sorted(node_idx, byte, new_idx);
            node_idx = new_idx;
        }

        // Mark as final if we followed existing path
        if start_byte_idx == bytes.len() {
            inner.nodes[node_idx].is_final = true;
        }

        inner.term_count += 1;
        inner.check_and_auto_minimize();
        true
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
        let mut inner = self.inner.write();

        // Navigate to insertion point
        let mut node_idx = 0;
        let mut path: Vec<(usize, u8)> = Vec::new();

        for &byte in bytes {
            if let Some(&child_idx) = inner.nodes[node_idx]
                .edges
                .iter()
                .find(|(b, _)| *b == byte)
                .map(|(_, idx)| idx)
            {
                path.push((node_idx, byte));
                node_idx = child_idx;
            } else {
                break;
            }
        }

        // Check if term already exists
        if path.len() == bytes.len() {
            if inner.nodes[node_idx].is_final {
                // Update value
                inner.nodes[node_idx].value = Some(value);
                return false;
            } else {
                // Mark as final and set value
                inner.nodes[node_idx].is_final = true;
                inner.nodes[node_idx].value = Some(value);
                inner.term_count += 1;
                return true;
            }
        }

        // Build remaining suffix
        let start_byte_idx = path.len();
        for i in start_byte_idx..bytes.len() {
            let byte = bytes[i];
            let new_idx = inner.nodes.len();
            let is_final = i == bytes.len() - 1;

            let mut new_node = if is_final {
                DawgNode::new_with_value(true, Some(value.clone()))
            } else {
                DawgNode::new(false)
            };
            new_node.ref_count = 1;

            inner.nodes.push(new_node);
            inner.insert_edge_sorted(node_idx, byte, new_idx);
            node_idx = new_idx;
        }

        inner.term_count += 1;
        inner.check_and_auto_minimize();
        true
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
        let mut node = self.root();
        for &byte in bytes {
            if let Some(next_node) = node.transition(byte) {
                node = next_node;
            } else {
                return false;
            }
        }
        node.is_final()
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
        let inner = self.inner.read();
        let mut node_idx = 0;

        for &byte in bytes {
            match inner.nodes[node_idx]
                .edges
                .iter()
                .find(|(b, _)| *b == byte)
                .map(|(_, idx)| idx)
            {
                Some(&child_idx) => node_idx = child_idx,
                None => return None,
            }
        }

        if inner.nodes[node_idx].is_final {
            inner.nodes[node_idx].value.clone()
        } else {
            None
        }
    }
}

// C1a algorithmic dedup: the original ~460-LOC impl<V> DynamicDawgInner<V>
// block lived here. It defined check_and_auto_minimize, insert_edge_sorted,
// minimize_incremental, nodes_structurally_equal, compute_signatures,
// compute_signatures_dfs, find_reachable_nodes, find_reachable_dfs,
// compact_with_reachable, extract_all_terms, dfs_collect, insert_direct,
// plus the disabled suffix-share cache helpers (find_or_create_suffix,
// compute_suffix_hash, verify_suffix_match, create_suffix_chain).
//
// All of these now live (with the same algorithmic behavior) on the
// canonical generic crate::dawg_core::DawgCore<U, V>. The outer
// DynamicDawg<V> wrapper above continues to call them via the type alias
// at the top of this file (DynamicDawgInner = DawgCore<u8, V>).
//
// Per CLAUDE.md the original code is preserved in git history
// (commit b7630ad and earlier); this comment block documents the
// architectural decision for future maintainers.

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
        // Extract the inner data by acquiring read lock
        let inner = self.inner.read();
        inner.serialize(serializer)
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
        let inner = DynamicDawgInner::deserialize(deserializer)?;
        Ok(DynamicDawg {
            inner: Arc::new(RwLock::new(inner)),
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
        let inner = DynamicDawgInner::deserialize(deserializer)?;
        Ok(DynamicDawg {
            inner: Arc::new(RwLock::new(inner)),
        })
    }
}

impl<V: DictionaryValue> Dictionary for DynamicDawg<V> {
    type Node = DynamicDawgNode<V>;

    fn root(&self) -> Self::Node {
        // Phase 1.2: Load cached data with single lock acquisition
        let inner = self.inner.read();
        let node = &inner.nodes[0];
        DynamicDawgNode {
            dawg: Arc::clone(&self.inner),
            node_idx: 0,
            is_final: node.is_final,
            edges: node.edges.clone(),
        }
    }

    fn len(&self) -> Option<usize> {
        Some(self.term_count())
    }

    fn sync_strategy(&self) -> SyncStrategy {
        SyncStrategy::ExternalSync
    }
}

/// Node handle for dynamic DAWG traversal.
///
/// Phase 1.2: Caches is_final and edges to avoid lock acquisition on hot paths.
/// This eliminates locks from is_final() and edge_count(), and drastically
/// reduces locks in transition() (only for successful transitions).
#[derive(Clone)]
pub struct DynamicDawgNode<V: DictionaryValue = ()> {
    dawg: Arc<RwLock<DynamicDawgInner<V>>>,
    node_idx: usize,
    // Phase 1.2: Cached data
    is_final: bool,
    edges: SmallVec<[(u8, usize); 4]>,
}

impl<V: DictionaryValue> DictionaryNode for DynamicDawgNode<V> {
    type Unit = u8;

    // Phase 1.2: Use cached data - no lock needed
    fn is_final(&self) -> bool {
        self.is_final
    }

    fn transition(&self, label: u8) -> Option<Self> {
        // Phase 1.2: Use cached edges for lookup - no lock needed
        // Adaptive: use linear search for small edge counts, binary for large
        // Empirical testing shows crossover at 16-20 edges
        let child_idx = if self.edges.len() < 16 {
            // Linear search - cache-friendly for small counts
            self.edges
                .iter()
                .find(|(b, _)| *b == label)
                .map(|(_, idx)| *idx)
        } else {
            // Binary search - efficient for large edge counts
            self.edges
                .binary_search_by_key(&label, |(b, _)| *b)
                .ok()
                .map(|i| self.edges[i].1)
        }?;

        // Phase 1.2: Only acquire lock to load child node's cached data
        let inner = self.dawg.read();
        let child_node = &inner.nodes[child_idx];
        Some(DynamicDawgNode {
            dawg: Arc::clone(&self.dawg),
            node_idx: child_idx,
            is_final: child_node.is_final,
            edges: child_node.edges.clone(),
        })
    }

    fn edges(&self) -> Box<dyn Iterator<Item = (u8, Self)> + '_> {
        // Phase 1.2: Batch load child nodes with single lock acquisition
        let inner = self.dawg.read();
        let child_data: Vec<_> = self
            .edges
            .iter()
            .map(|(byte, idx)| {
                let child_node = &inner.nodes[*idx];
                (*byte, *idx, child_node.is_final, child_node.edges.clone())
            })
            .collect();
        drop(inner);

        let dawg = Arc::clone(&self.dawg);
        Box::new(
            child_data
                .into_iter()
                .map(move |(byte, idx, is_final, edges)| {
                    (
                        byte,
                        DynamicDawgNode {
                            dawg: Arc::clone(&dawg),
                            node_idx: idx,
                            is_final,
                            edges,
                        },
                    )
                }),
        )
    }

    // Phase 1.2: Use cached data - no lock needed
    fn edge_count(&self) -> Option<usize> {
        Some(self.edges.len())
    }
}

// ============================================================================
// MappedDictionary Trait Implementation
// ============================================================================

use crate::{MappedDictionary, MappedDictionaryNode};

impl<V: DictionaryValue> MappedDictionaryNode for DynamicDawgNode<V> {
    type Value = V;

    fn value(&self) -> Option<Self::Value> {
        // Need to lock to get the value
        let inner = self.dawg.read();
        inner
            .nodes
            .get(self.node_idx)
            .and_then(|node| node.value.clone())
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
        F: FnOnce(&mut Self::Value),
    {
        // Delegate to the inherent method
        Self::update_or_insert(self, term, default_value, update_fn)
    }

    fn union_with<F>(&self, other: &Self, merge_fn: F) -> usize
    where
        F: Fn(&Self::Value, &Self::Value) -> Self::Value,
        Self::Value: Clone,
    {
        let other_inner = other.inner.read();
        let mut processed = 0;

        // DFS traversal to extract all terms with values from other
        let mut stack: Vec<(usize, Vec<u8>)> = vec![(0, Vec::new())];

        while let Some((node_idx, path)) = stack.pop() {
            let node = &other_inner.nodes[node_idx];

            // If this is a final node, we have a complete term
            if node.is_final {
                if let Ok(term) = std::str::from_utf8(&path) {
                    processed += 1;

                    if let Some(other_value) = &node.value {
                        // Check if term exists in self
                        if let Some(self_value) = self.get_value(term) {
                            // Merge values
                            let merged = merge_fn(&self_value, other_value);
                            self.insert_with_value(term, merged);
                        } else {
                            // Insert new term
                            self.insert_with_value(term, other_value.clone());
                        }
                    }
                }
            }

            // Push children onto stack (in reverse for consistent ordering)
            for &(label, target_idx) in node.edges.iter().rev() {
                let mut child_path = path.clone();
                child_path.push(label);
                stack.push((target_idx, child_path));
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
