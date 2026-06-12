//! Character-level PathMap dictionary for proper Unicode support.
//!
//! This module provides a character-based variant of PathMapDictionary that operates
//! at the Unicode character level rather than byte level. This ensures correct edit
//! distance semantics for multi-byte UTF-8 sequences.
//!
//! ## Differences from PathMapDictionary
//!
//! - Edge labels are `char` instead of `u8`
//! - Distance calculations count characters, not bytes
//! - Correct semantics: "" → "¡" is distance 1, not 2
//!
//! ## Performance Trade-offs
//!
//! - **Memory**: Minimal overhead (~5% for character position tracking)
//! - **Speed**: Slightly slower (~10-15%) due to UTF-8 decoding during traversal
//! - **Correctness**: Proper Unicode semantics for Levenshtein distance
//!
//! ## Use Cases
//!
//! Use `PathMapDictionaryChar` when:
//! - Dictionary contains non-ASCII Unicode characters
//! - Edit distance must be measured in characters, not bytes
//! - Fuzzy matching requires correct Unicode semantics
//! - Value-based filtering is needed with Unicode content

use super::core::{trie_ref_root, TrieRefNodeChar};
use super::snapshot::PathMapSnapshotChar;
use crate::value::DictionaryValue;
use crate::{Dictionary, MappedDictionary, SyncStrategy};
use pathmap::zipper::TrieRefOwned;
use pathmap::PathMap;
use std::sync::Arc;

use crate::sync_compat::RwLock;

/// Character-level PathMap dictionary for proper Unicode support.
///
/// This variant operates at the Unicode character level, ensuring correct
/// edit distance calculations for multi-byte UTF-8 sequences.
///
/// # Storage
///
/// Terms are stored as UTF-8 bytes in PathMap (unchanged from byte-level version).
/// The character-level abstraction is provided through traversal logic that
/// decodes UTF-8 sequences on-the-fly.
///
/// # Thread Safety
///
/// Uses `RwLock` for interior mutability:
/// - Multiple concurrent readers (queries)
/// - Exclusive write access for modifications (insert/remove)
///
/// # Examples
///
/// ```
/// use libdictenstein::pathmap::char::PathMapDictionaryChar;
/// use libdictenstein::Dictionary;
/// use libdictenstein::prelude::*;
///
/// // Dictionary with Unicode terms
/// let dict: PathMapDictionaryChar<()> = PathMapDictionaryChar::from_terms(vec![
///     "café", "naïve", "中文", "🎉"
/// ]);
///
/// assert!(dict.contains("café"));
/// assert!(dict.contains("中文"));
/// assert!(dict.contains("🎉"));
/// assert!(!dict.contains("hello"));
/// ```
#[derive(Clone, Debug)]
pub struct PathMapDictionaryChar<V: DictionaryValue = ()> {
    map: Arc<RwLock<PathMap<V>>>,
    term_count: Arc<RwLock<usize>>,
}

impl<V: DictionaryValue> PathMapDictionaryChar<V> {
    /// Create a new empty character-level dictionary
    pub fn new() -> Self
    where
        V: Default,
    {
        Self {
            map: Arc::new(RwLock::new(PathMap::new())),
            term_count: Arc::new(RwLock::new(0)),
        }
    }

    /// Create a dictionary from an iterator of terms with a default value
    pub fn from_terms<I, S>(terms: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
        V: Default,
    {
        let mut map = PathMap::new();
        let mut count = 0;

        for term in terms {
            let bytes = term.as_ref().as_bytes();
            if map.insert(bytes, V::default()).is_none() {
                count += 1;
            }
        }

        Self {
            map: Arc::new(RwLock::new(map)),
            term_count: Arc::new(RwLock::new(count)),
        }
    }

    /// Create a dictionary from an iterator of (term, value) pairs
    pub fn from_terms_with_values<I, S>(terms: I) -> Self
    where
        I: IntoIterator<Item = (S, V)>,
        S: AsRef<str>,
    {
        let mut map = PathMap::new();
        let mut count = 0;

        for (term, value) in terms {
            let bytes = term.as_ref().as_bytes();
            if map.insert(bytes, value).is_none() {
                count += 1;
            }
        }

        Self {
            map: Arc::new(RwLock::new(map)),
            term_count: Arc::new(RwLock::new(count)),
        }
    }

    /// Insert a term with a default value into the dictionary
    ///
    /// Returns `true` if the term was newly inserted, `false` if it already existed.
    ///
    /// # Thread Safety
    ///
    /// This method acquires a write lock, blocking concurrent reads and writes.
    ///
    /// # Panics
    ///
    /// Panics if the lock is poisoned (another thread panicked while holding the lock).
    pub fn insert(&self, term: &str) -> bool
    where
        V: Default,
    {
        self.insert_with_value(term, V::default())
    }

    /// Insert a term with a specific value into the dictionary
    ///
    /// Returns `true` if the term was newly inserted, `false` if it already existed.
    /// If the term already existed, its value is updated.
    ///
    /// # Thread Safety
    ///
    /// This method acquires a write lock, blocking concurrent reads and writes.
    ///
    /// # Panics
    ///
    /// Panics if the lock is poisoned (another thread panicked while holding the lock).
    pub fn insert_with_value(&self, term: &str, value: V) -> bool {
        let bytes = term.as_bytes();
        let mut map = self.map.write();
        let mut count = self.term_count.write();

        if map.insert(bytes, value).is_none() {
            *count += 1;
            true
        } else {
            false
        }
    }

    /// Remove a term from the dictionary
    ///
    /// Returns `true` if the term was present and removed, `false` if it didn't exist.
    ///
    /// # Thread Safety
    ///
    /// This method acquires a write lock, blocking concurrent reads and writes.
    ///
    /// # Panics
    ///
    /// Panics if the lock is poisoned.
    pub fn remove(&self, term: &str) -> bool {
        let bytes = term.as_bytes();
        let mut map = self.map.write();
        let mut count = self.term_count.write();

        if map.remove_val_at(bytes, true).is_some() {
            *count = count.saturating_sub(1);
            true
        } else {
            false
        }
    }

    /// Clear all terms from the dictionary
    ///
    /// # Thread Safety
    ///
    /// This method acquires a write lock, blocking concurrent reads and writes.
    ///
    /// # Panics
    ///
    /// Panics if the lock is poisoned.
    pub fn clear(&self) {
        let mut map = self.map.write();
        let mut count = self.term_count.write();

        *map = PathMap::new();
        *count = 0;
    }

    /// Get the current number of terms in the dictionary
    ///
    /// # Thread Safety
    ///
    /// This method acquires a read lock.
    ///
    /// # Panics
    ///
    /// Panics if the lock is poisoned.
    pub fn term_count(&self) -> usize {
        *self.term_count.read()
    }

    /// Get the value associated with a term
    ///
    /// Returns `None` if the term doesn't exist in the dictionary.
    ///
    /// # Thread Safety
    ///
    /// This method acquires a read lock.
    ///
    /// # Panics
    ///
    /// Panics if the lock is poisoned.
    pub fn get_value(&self, term: &str) -> Option<V> {
        let bytes = term.as_bytes();
        let map = self.map.read();
        map.get_val_at(bytes).cloned()
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
    /// # Thread Safety
    ///
    /// This method acquires a write lock, blocking concurrent reads and writes.
    ///
    /// # Panics
    ///
    /// Panics if the lock is poisoned.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use std::collections::HashSet;
    /// use libdictenstein::pathmap::char::PathMapDictionaryChar;
    ///
    /// let dict: PathMapDictionaryChar<HashSet<String>> = PathMapDictionaryChar::new();
    ///
    /// // First call - inserts new term with default value
    /// let was_new = dict.update_or_insert(
    ///     "café",
    ///     HashSet::from(["meaning1".to_string()]),
    ///     |set| { set.insert("meaning1".to_string()); }
    /// );
    /// assert!(was_new);
    ///
    /// // Second call - updates existing value
    /// let was_new = dict.update_or_insert(
    ///     "café",
    ///     HashSet::new(),
    ///     |set| { set.insert("meaning2".to_string()); }
    /// );
    /// assert!(!was_new);
    ///
    /// // Now "café" contains {"meaning1", "meaning2"}
    /// ```
    pub fn update_or_insert<F>(&self, term: &str, default_value: V, update_fn: F) -> bool
    where
        F: FnOnce(&mut V),
    {
        let bytes = term.as_bytes();
        let mut map = self.map.write();
        let mut count = self.term_count.write();

        // Check if term exists
        let existed = map.get_val_at(bytes).is_some();
        // Get mutable reference, creating with default if needed
        let value = map.get_val_or_set_mut_at(bytes, default_value);
        update_fn(value);

        if !existed {
            *count += 1;
        }
        !existed
    }

    /// Take an `𝒪(1)` copy-on-write [`PathMapSnapshotChar`] of the current contents.
    ///
    /// The snapshot is decoupled from later mutations and can be queried
    /// lock-free; the current term count is captured for an exact
    /// [`Dictionary::len`](crate::Dictionary::len).
    pub fn snapshot(&self) -> PathMapSnapshotChar<V> {
        PathMapSnapshotChar::from_map(self.map.read().clone()).with_len(self.term_count())
    }
}

impl<V: DictionaryValue + Default> Default for PathMapDictionaryChar<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V: DictionaryValue> Dictionary for PathMapDictionaryChar<V> {
    type Node = PathMapNodeChar<V>;

    #[inline]
    fn root(&self) -> Self::Node {
        // 𝒪(1) copy-on-write snapshot; character-level traversal then runs
        // lock-free, decoding UTF-8 by descending locally from the focus.
        let snapshot = self.map.read().clone();
        TrieRefNodeChar::new(trie_ref_root(snapshot))
    }

    #[inline]
    fn len(&self) -> Option<usize> {
        Some(self.term_count())
    }

    #[inline]
    fn sync_strategy(&self) -> SyncStrategy {
        SyncStrategy::ExternalSync
    }
}

impl<V: DictionaryValue> MappedDictionary for PathMapDictionaryChar<V> {
    type Value = V;

    fn get_value(&self, term: &str) -> Option<Self::Value> {
        PathMapDictionaryChar::get_value(self, term)
    }
}

impl<V: DictionaryValue + Default> crate::MutableDictionary for PathMapDictionaryChar<V> {
    fn insert(&self, term: &str) -> bool {
        PathMapDictionaryChar::insert(self, term)
    }

    fn remove(&self, term: &str) -> bool {
        PathMapDictionaryChar::remove(self, term)
    }
}

impl<V: DictionaryValue> crate::MutableMappedDictionary for PathMapDictionaryChar<V> {
    fn insert_with_value(&self, term: &str, value: Self::Value) -> bool {
        PathMapDictionaryChar::insert_with_value(self, term, value)
    }

    fn update_or_insert<F>(&self, term: &str, default_value: Self::Value, update_fn: F) -> bool
    where
        F: FnOnce(&mut Self::Value),
    {
        PathMapDictionaryChar::update_or_insert(self, term, default_value, update_fn)
    }

    fn union_with<F>(&self, other: &Self, merge_fn: F) -> usize
    where
        F: Fn(&Self::Value, &Self::Value) -> Self::Value,
        Self::Value: Clone,
    {
        let other_map = other.map.read();
        let mut self_map = self.map.write();
        let mut self_term_count = self.term_count.write();

        let mut processed = 0;

        // Iterate over all entries in other
        for (key_bytes, other_value) in other_map.iter() {
            processed += 1;

            if let Some(self_value) = self_map.get(&key_bytes) {
                // Key exists: merge the values
                let merged = merge_fn(self_value, other_value);
                self_map.insert(&key_bytes, merged);
            } else {
                // Key doesn't exist: insert from other
                self_map.insert(&key_bytes, other_value.clone());
                *self_term_count += 1;
            }
        }

        processed
    }
}

/// Character-level dictionary node for [`PathMapDictionaryChar`].
///
/// A thin [`TrieRefNodeChar`] over an owned, `𝒪(1)` copy-on-write snapshot
/// ([`TrieRefOwned`]) of the map. Terms remain stored as UTF-8 bytes; this node
/// decodes UTF-8 on the fly so edge labels and edit distances are measured in
/// `char`s. Continuation bytes are discovered by descending **locally from the
/// focus** and reading child masks — never by replaying the byte path from the
/// root, and never under a per-operation lock (cf. the path-replay node it
/// replaces). Binds to a consistent snapshot at
/// [`Dictionary::root`](crate::Dictionary::root) time (snapshot isolation).
pub type PathMapNodeChar<V> = TrieRefNodeChar<V, TrieRefOwned<V>>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DictionaryNode, MappedDictionaryNode};

    #[test]
    fn test_pathmap_char_creation() {
        let dict: PathMapDictionaryChar<()> =
            PathMapDictionaryChar::from_terms(vec!["café", "中文", "🎉"]);
        assert_eq!(dict.len(), Some(3));
    }

    #[test]
    fn test_pathmap_char_contains() {
        let dict: PathMapDictionaryChar<()> =
            PathMapDictionaryChar::from_terms(vec!["café", "naïve"]);
        assert!(dict.contains("café"));
        assert!(dict.contains("naïve"));
        assert!(!dict.contains("cafe")); // Without accent
    }

    #[test]
    fn test_pathmap_char_unicode_terms() {
        let dict: PathMapDictionaryChar<()> =
            PathMapDictionaryChar::from_terms(vec!["hello", "café", "中文", "🎉", "test123"]);

        assert!(dict.contains("hello"));
        assert!(dict.contains("café"));
        assert!(dict.contains("中文"));
        assert!(dict.contains("🎉"));
        assert!(dict.contains("test123"));
        assert!(!dict.contains("missing"));
    }

    #[test]
    fn test_pathmap_char_node_traversal() {
        let dict: PathMapDictionaryChar<()> = PathMapDictionaryChar::from_terms(vec!["café"]);
        let root = dict.root();

        // Navigate: c -> a -> f -> é
        let c = root.transition('c').expect("should have 'c'");
        let a = c.transition('a').expect("should have 'a'");
        let f = a.transition('f').expect("should have 'f'");
        let e_acute = f.transition('é').expect("should have 'é'");

        assert!(e_acute.is_final(), "'café' should be final");
        assert!(!f.is_final(), "'caf' should not be final");
    }

    #[test]
    fn test_pathmap_char_node_edges() {
        let dict: PathMapDictionaryChar<()> =
            PathMapDictionaryChar::from_terms(vec!["café", "car", "cart"]);
        let root = dict.root();
        let c = root.transition('c').expect("should have 'c'");
        let a = c.transition('a').expect("should have 'a'");

        let edges: Vec<char> = a.edges().map(|(ch, _)| ch).collect();
        assert!(edges.contains(&'f'), "should have 'f' for 'café'");
        assert!(edges.contains(&'r'), "should have 'r' for 'car'");
    }

    #[test]
    fn test_pathmap_char_insert() {
        let dict: PathMapDictionaryChar<()> = PathMapDictionaryChar::from_terms(vec!["test"]);
        assert_eq!(dict.term_count(), 1);

        // Insert new Unicode term
        assert!(dict.insert("café"));
        assert_eq!(dict.term_count(), 2);
        assert!(dict.contains("café"));

        // Insert duplicate
        assert!(!dict.insert("test"));
        assert_eq!(dict.term_count(), 2);
    }

    #[test]
    fn test_pathmap_char_remove() {
        let dict: PathMapDictionaryChar<()> =
            PathMapDictionaryChar::from_terms(vec!["café", "中文", "test"]);
        assert_eq!(dict.term_count(), 3);

        // Remove Unicode term
        assert!(dict.remove("café"));
        assert_eq!(dict.term_count(), 2);
        assert!(!dict.contains("café"));
        assert!(dict.contains("中文"));
        assert!(dict.contains("test"));

        // Remove non-existent term
        assert!(!dict.remove("missing"));
        assert_eq!(dict.term_count(), 2);
    }

    #[test]
    fn test_pathmap_char_with_values() {
        let terms_with_values = vec![("café", 1u32), ("中文", 2u32), ("🎉", 3u32)];
        let dict: PathMapDictionaryChar<u32> =
            PathMapDictionaryChar::from_terms_with_values(terms_with_values);

        assert_eq!(dict.len(), Some(3));
        assert_eq!(dict.get_value("café"), Some(1));
        assert_eq!(dict.get_value("中文"), Some(2));
        assert_eq!(dict.get_value("🎉"), Some(3));
        assert_eq!(dict.get_value("missing"), None);
    }

    #[test]
    fn test_pathmap_char_node_value() {
        let terms_with_values = vec![("café", 10u32), ("中文", 20u32)];
        let dict: PathMapDictionaryChar<u32> =
            PathMapDictionaryChar::from_terms_with_values(terms_with_values);
        let root = dict.root();

        // Navigate to "café"
        let c = root.transition('c').expect("should have 'c'");
        let a = c.transition('a').expect("should have 'a'");
        let f = a.transition('f').expect("should have 'f'");
        let e_acute = f.transition('é').expect("should have 'é'");

        assert!(e_acute.is_final());
        assert_eq!(e_acute.value(), Some(10));

        // Non-final node should have no value
        assert!(!c.is_final());
        assert_eq!(c.value(), None);
    }

    #[test]
    fn test_pathmap_char_emoji() {
        let dict: PathMapDictionaryChar<()> =
            PathMapDictionaryChar::from_terms(vec!["hello🎉", "world🌍"]);

        assert!(dict.contains("hello🎉"));
        assert!(dict.contains("world🌍"));

        let root = dict.root();
        let h = root.transition('h').unwrap();
        let e = h.transition('e').unwrap();
        let l1 = e.transition('l').unwrap();
        let l2 = l1.transition('l').unwrap();
        let o = l2.transition('o').unwrap();
        let emoji = o.transition('🎉').expect("should have emoji");

        assert!(emoji.is_final());
    }

    #[test]
    fn test_pathmap_char_cjk() {
        let dict: PathMapDictionaryChar<()> =
            PathMapDictionaryChar::from_terms(vec!["中文", "日本語"]);

        assert!(dict.contains("中文"));
        assert!(dict.contains("日本語"));

        let root = dict.root();
        let zhong = root.transition('中').expect("should have '中'");
        let wen = zhong.transition('文').expect("should have '文'");

        assert!(wen.is_final());
    }

    #[test]
    fn test_char_snapshot_isolation() {
        let dict: PathMapDictionaryChar<u32> = PathMapDictionaryChar::new();
        dict.insert_with_value("café", 1);
        let snap = dict.snapshot();
        assert_eq!(snap.len(), Some(1));
        assert!(snap.contains("café"));
        assert_eq!(snap.get_value("café"), Some(1));

        // A mutation after the snapshot is not observed by the snapshot.
        dict.insert_with_value("car", 2);
        assert!(!snap.contains("car"));
        assert!(dict.contains("car"));
    }
}
