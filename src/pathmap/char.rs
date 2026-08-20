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

use super::core::{trie_ref_root, PathMapState, TrieRefNodeChar};
use super::snapshot::PathMapSnapshotChar;
use crate::nonblocking::CasBackoff;
use crate::value::DictionaryValue;
use crate::{Dictionary, MappedDictionary, SyncStrategy};
use arc_swap::ArcSwap;
use pathmap::zipper::TrieRefOwned;
use pathmap::PathMap;
use std::fmt;
use std::sync::Arc;

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
/// Publishes immutable PathMap snapshots through an atomic pointer:
/// - Readers clone one snapshot and never block
/// - Writers mutate a cloned persistent root and install it with CAS
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
#[derive(Clone)]
pub struct PathMapDictionaryChar<V: DictionaryValue = ()> {
    state: Arc<ArcSwap<PathMapState<V>>>,
}

impl<V: DictionaryValue> fmt::Debug for PathMapDictionaryChar<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PathMapDictionaryChar")
            .field("term_count", &self.term_count())
            .finish()
    }
}

impl<V: DictionaryValue> PathMapDictionaryChar<V> {
    #[inline]
    fn from_state(map: PathMap<V>, len: usize) -> Self {
        Self {
            state: Arc::new(ArcSwap::from_pointee(PathMapState::new(map, len))),
        }
    }

    #[inline]
    fn load_state(&self) -> Arc<PathMapState<V>> {
        self.state.load_full()
    }

    #[inline]
    fn compare_store_state(&self, current: &Arc<PathMapState<V>>, next: PathMapState<V>) -> bool {
        let previous = self.state.compare_and_swap(current, Arc::new(next));
        Arc::ptr_eq(&previous, current)
    }

    fn from_char_entries(entries: Vec<(Vec<char>, V)>) -> Self {
        let mut map = PathMap::new();
        let mut len = 0;
        for (key, value) in entries {
            let key: String = key.into_iter().collect();
            if map.insert(key.as_bytes(), value).is_none() {
                len += 1;
            }
        }
        Self::from_state(map, len)
    }

    fn extend_char_entries(&self, entries: Vec<(Vec<char>, V)>) -> usize {
        if entries.is_empty() {
            return 0;
        }
        let mut backoff = CasBackoff::new();
        loop {
            let current = self.load_state();
            let mut map = current.map.clone();
            let mut inserted = 0;
            for (key, value) in &entries {
                let key: String = key.iter().collect();
                if map.insert(key.as_bytes(), value.clone()).is_none() {
                    inserted += 1;
                }
            }
            if self.compare_store_state(&current, PathMapState::new(map, current.len + inserted)) {
                return inserted;
            }
            backoff.snooze();
        }
    }

    /// Create a new empty character-level dictionary
    pub fn new() -> Self
    where
        V: Default,
    {
        Self::from_state(PathMap::new(), 0)
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

        Self::from_state(map, count)
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

        Self::from_state(map, count)
    }

    /// Insert a term with a default value into the dictionary
    ///
    /// Returns `true` if the term was newly inserted, `false` if it already existed.
    ///
    /// # Thread Safety
    ///
    /// Writers publish a cloned PathMap root with CAS. Readers observe either
    /// the old or new snapshot without waiting.
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
    /// Writers publish a cloned PathMap root with CAS. Readers observe either
    /// the old or new snapshot without waiting.
    pub fn insert_with_value(&self, term: &str, value: V) -> bool {
        let bytes = term.as_bytes();
        let mut backoff = CasBackoff::new();
        loop {
            let current = self.load_state();
            let mut next_map = current.map.clone();
            let inserted = next_map.insert(bytes, value.clone()).is_none();
            let next_len = current.len + usize::from(inserted);

            if self.compare_store_state(&current, PathMapState::new(next_map, next_len)) {
                return inserted;
            }

            backoff.snooze();
        }
    }

    /// Remove a term from the dictionary
    ///
    /// Returns `true` if the term was present and removed, `false` if it didn't exist.
    ///
    /// # Thread Safety
    ///
    /// Removal publishes a cloned PathMap root with CAS and never blocks
    /// readers.
    pub fn remove(&self, term: &str) -> bool {
        let bytes = term.as_bytes();
        let mut backoff = CasBackoff::new();
        loop {
            let current = self.load_state();
            let mut next_map = current.map.clone();
            if next_map.remove_val_at(bytes, true).is_none() {
                return false;
            }
            let next_len = current.len.saturating_sub(1);

            if self.compare_store_state(&current, PathMapState::new(next_map, next_len)) {
                return true;
            }

            backoff.snooze();
        }
    }

    /// Clear all terms from the dictionary
    ///
    /// # Thread Safety
    ///
    /// Clear atomically publishes an empty snapshot.
    pub fn clear(&self) {
        let mut backoff = CasBackoff::new();
        loop {
            let current = self.load_state();
            if current.len == 0 {
                return;
            }

            if self.compare_store_state(&current, PathMapState::new(PathMap::new(), 0)) {
                return;
            }

            backoff.snooze();
        }
    }

    /// Get the current number of terms in the dictionary
    ///
    /// # Thread Safety
    ///
    /// This method atomically loads the current snapshot.
    pub fn term_count(&self) -> usize {
        self.load_state().len
    }

    /// Get the value associated with a term
    ///
    /// Returns `None` if the term doesn't exist in the dictionary.
    ///
    /// # Thread Safety
    ///
    /// This method atomically loads one snapshot and performs a read-only lookup.
    pub fn get_value(&self, term: &str) -> Option<V> {
        let bytes = term.as_bytes();
        let state = self.load_state();
        state.map.get_val_at(bytes).cloned()
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
    /// The update is applied to a cloned value in a cloned PathMap root and
    /// published with CAS. The closure may be re-run if a competing writer wins
    /// the race first.
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
        F: Fn(&mut V),
    {
        let bytes = term.as_bytes();
        let mut backoff = CasBackoff::new();
        loop {
            let current = self.load_state();
            let mut next_map = current.map.clone();
            let existed = next_map.get_val_at(bytes).is_some();
            let value = next_map.get_val_or_set_mut_at(bytes, default_value.clone());
            update_fn(value);
            let next_len = current.len + usize::from(!existed);

            if self.compare_store_state(&current, PathMapState::new(next_map, next_len)) {
                return !existed;
            }

            backoff.snooze();
        }
    }

    /// Take an `𝒪(1)` copy-on-write [`PathMapSnapshotChar`] of the current contents.
    ///
    /// The snapshot is decoupled from later mutations and can be queried
    /// lock-free; the current term count is captured for an exact
    /// [`Dictionary::len`].
    pub fn snapshot(&self) -> PathMapSnapshotChar<V> {
        let state = self.load_state();
        PathMapSnapshotChar::from_map(state.map.clone()).with_len(state.len)
    }
}

impl<V: DictionaryValue + Default> FromIterator<String> for PathMapDictionaryChar<V> {
    fn from_iter<I: IntoIterator<Item = String>>(iter: I) -> Self {
        Self::from_terms(iter)
    }
}

impl<'a, V: DictionaryValue + Default> FromIterator<&'a str> for PathMapDictionaryChar<V> {
    fn from_iter<I: IntoIterator<Item = &'a str>>(iter: I) -> Self {
        Self::from_terms(iter)
    }
}

impl<V: DictionaryValue + Default> FromIterator<Vec<char>> for PathMapDictionaryChar<V> {
    fn from_iter<I: IntoIterator<Item = Vec<char>>>(iter: I) -> Self {
        Self::from_char_entries(iter.into_iter().map(|key| (key, V::default())).collect())
    }
}

impl<'a, V: DictionaryValue + Default> FromIterator<&'a [char]> for PathMapDictionaryChar<V> {
    fn from_iter<I: IntoIterator<Item = &'a [char]>>(iter: I) -> Self {
        iter.into_iter().map(<[char]>::to_vec).collect()
    }
}

impl<V: DictionaryValue> FromIterator<(String, V)> for PathMapDictionaryChar<V> {
    fn from_iter<I: IntoIterator<Item = (String, V)>>(iter: I) -> Self {
        Self::from_terms_with_values(iter)
    }
}

impl<'a, V: DictionaryValue> FromIterator<(&'a str, V)> for PathMapDictionaryChar<V> {
    fn from_iter<I: IntoIterator<Item = (&'a str, V)>>(iter: I) -> Self {
        Self::from_terms_with_values(iter)
    }
}

impl<V: DictionaryValue> FromIterator<(Vec<char>, V)> for PathMapDictionaryChar<V> {
    fn from_iter<I: IntoIterator<Item = (Vec<char>, V)>>(iter: I) -> Self {
        Self::from_char_entries(iter.into_iter().collect())
    }
}

impl<'a, V: DictionaryValue> FromIterator<(&'a [char], V)> for PathMapDictionaryChar<V> {
    fn from_iter<I: IntoIterator<Item = (&'a [char], V)>>(iter: I) -> Self {
        Self::from_char_entries(
            iter.into_iter()
                .map(|(key, value)| (key.to_vec(), value))
                .collect(),
        )
    }
}

impl<V: DictionaryValue + Default> Extend<String> for PathMapDictionaryChar<V> {
    fn extend<I: IntoIterator<Item = String>>(&mut self, iter: I) {
        self.extend_char_entries(
            iter.into_iter()
                .map(|key| (key.chars().collect(), V::default()))
                .collect(),
        );
    }
}

impl<'a, V: DictionaryValue + Default> Extend<&'a str> for PathMapDictionaryChar<V> {
    fn extend<I: IntoIterator<Item = &'a str>>(&mut self, iter: I) {
        self.extend(iter.into_iter().map(str::to_owned));
    }
}

impl<V: DictionaryValue + Default> Extend<Vec<char>> for PathMapDictionaryChar<V> {
    fn extend<I: IntoIterator<Item = Vec<char>>>(&mut self, iter: I) {
        self.extend_char_entries(iter.into_iter().map(|key| (key, V::default())).collect());
    }
}

impl<'a, V: DictionaryValue + Default> Extend<&'a [char]> for PathMapDictionaryChar<V> {
    fn extend<I: IntoIterator<Item = &'a [char]>>(&mut self, iter: I) {
        self.extend(iter.into_iter().map(<[char]>::to_vec));
    }
}

impl<V: DictionaryValue> Extend<(String, V)> for PathMapDictionaryChar<V> {
    fn extend<I: IntoIterator<Item = (String, V)>>(&mut self, iter: I) {
        self.extend_char_entries(
            iter.into_iter()
                .map(|(key, value)| (key.chars().collect(), value))
                .collect(),
        );
    }
}

impl<'a, V: DictionaryValue> Extend<(&'a str, V)> for PathMapDictionaryChar<V> {
    fn extend<I: IntoIterator<Item = (&'a str, V)>>(&mut self, iter: I) {
        self.extend(iter.into_iter().map(|(key, value)| (key.to_owned(), value)));
    }
}

impl<V: DictionaryValue> Extend<(Vec<char>, V)> for PathMapDictionaryChar<V> {
    fn extend<I: IntoIterator<Item = (Vec<char>, V)>>(&mut self, iter: I) {
        self.extend_char_entries(iter.into_iter().collect());
    }
}

impl<'a, V: DictionaryValue> Extend<(&'a [char], V)> for PathMapDictionaryChar<V> {
    fn extend<I: IntoIterator<Item = (&'a [char], V)>>(&mut self, iter: I) {
        self.extend_char_entries(
            iter.into_iter()
                .map(|(key, value)| (key.to_vec(), value))
                .collect(),
        );
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
        let state = self.load_state();
        TrieRefNodeChar::new(trie_ref_root(state.map.clone()))
    }

    #[inline]
    fn len(&self) -> Option<usize> {
        Some(self.term_count())
    }

    #[inline]
    fn sync_strategy(&self) -> SyncStrategy {
        SyncStrategy::InternalSync
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
        F: Fn(&mut Self::Value),
    {
        PathMapDictionaryChar::update_or_insert(self, term, default_value, update_fn)
    }

    fn union_with<F>(&self, other: &Self, merge_fn: F) -> usize
    where
        F: Fn(&Self::Value, &Self::Value) -> Self::Value,
        Self::Value: Clone,
    {
        let other_state = other.load_state();
        let processed = other_state.len;

        let mut backoff = CasBackoff::new();
        loop {
            let current = self.load_state();
            let mut next_map = current.map.clone();
            let mut next_len = current.len;

            for (key_bytes, other_value) in other_state.map.iter() {
                if let Some(self_value) = next_map.get(&key_bytes) {
                    let merged = merge_fn(self_value, other_value);
                    next_map.insert(&key_bytes, merged);
                } else {
                    next_map.insert(&key_bytes, other_value.clone());
                    next_len += 1;
                }
            }

            if self.compare_store_state(&current, PathMapState::new(next_map, next_len)) {
                return processed;
            }

            backoff.snooze();
        }
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
/// [`Dictionary::root`] time (snapshot isolation).
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
