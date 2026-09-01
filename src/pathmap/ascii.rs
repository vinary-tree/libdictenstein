//! PathMap-backed dictionary implementation.

use super::core::{trie_ref_root, PathMapState, TrieRefNode};
use super::snapshot::PathMapSnapshot;
use super::zipper::PathMapZipper;
use crate::iterator::DictionaryIterator;
use crate::nonblocking::CasBackoff;
use crate::value::DictionaryValue;
use crate::{Dictionary, MappedDictionary, SyncStrategy};
use arc_swap::ArcSwap;
// NOTE: Serialization support (DictionaryFromTerms impl) is provided in liblevenshtein
// since the trait lives there.
use pathmap::zipper::TrieRefOwned;
use pathmap::PathMap;
use std::fmt;
use std::sync::Arc;

/// PathMap-backed dictionary for approximate string matching.
///
/// This implementation uses PathMap as the underlying trie structure,
/// providing efficient memory usage through structural sharing.
///
/// The dictionary publishes immutable PathMap snapshots through an atomic
/// pointer. Readers clone one snapshot and never block; writers clone the
/// current persistent root, mutate the clone, and publish it with CAS.
///
/// # Generic Values
///
/// The dictionary can map terms to arbitrary values via the `V` type parameter:
/// - `PathMapDictionary<()>`: No values (backward compatible)
/// - `PathMapDictionary<u32>`: Map terms to scope IDs
/// - `PathMapDictionary<Vec<String>>`: Map terms to lists of metadata
///
/// # Examples
///
/// ```
/// use libdictenstein::pathmap::PathMapDictionary;
///
/// // Simple dictionary (no values)
/// let dict: PathMapDictionary<()> = PathMapDictionary::new();
///
/// // Dictionary with scope IDs
/// let dict_with_scopes: PathMapDictionary<u32> = PathMapDictionary::new();
/// ```
#[derive(Clone)]
pub struct PathMapDictionary<V: DictionaryValue = ()> {
    pub(crate) state: Arc<ArcSwap<PathMapState<V>>>,
}

impl<V: DictionaryValue> fmt::Debug for PathMapDictionary<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PathMapDictionary")
            .field("term_count", &self.term_count())
            .finish()
    }
}

impl<V: DictionaryValue> PathMapDictionary<V> {
    #[inline]
    fn from_state(map: PathMap<V>, len: usize) -> Self {
        Self {
            state: Arc::new(ArcSwap::from_pointee(PathMapState::new(map, len))),
        }
    }

    #[inline]
    pub(crate) fn load_state(&self) -> Arc<PathMapState<V>> {
        self.state.load_full()
    }

    #[inline]
    fn compare_store_state(&self, current: &Arc<PathMapState<V>>, next: PathMapState<V>) -> bool {
        let previous = self.state.compare_and_swap(current, Arc::new(next));
        Arc::ptr_eq(&previous, current)
    }

    fn from_byte_entries(entries: Vec<(Vec<u8>, V)>) -> Self {
        let mut map = PathMap::new();
        let mut len = 0;
        for (key, value) in entries {
            if map.insert(&key, value).is_none() {
                len += 1;
            }
        }
        Self::from_state(map, len)
    }

    fn extend_byte_entries(&self, entries: Vec<(Vec<u8>, V)>) -> usize {
        if entries.is_empty() {
            return 0;
        }
        let mut backoff = CasBackoff::new();
        loop {
            let current = self.load_state();
            let mut map = current.map.clone();
            let mut inserted = 0;
            for (key, value) in &entries {
                if map.insert(key, value.clone()).is_none() {
                    inserted += 1;
                }
            }
            if self.compare_store_state(&current, PathMapState::new(map, current.len + inserted)) {
                return inserted;
            }
            backoff.snooze();
        }
    }

    /// Create a new empty dictionary
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

    /// Serialize to PathMap's native .paths format
    ///
    /// # Thread Safety
    ///
    /// Serialization reads one stable snapshot.
    pub fn serialize_paths<W: std::io::Write>(&self, writer: &mut W) -> std::io::Result<()> {
        use pathmap::paths_serialization::serialize_paths;
        let state = self.load_state();
        serialize_paths(state.map.read_zipper(), writer)?;
        Ok(())
    }

    /// Deserialize from PathMap's native .paths format
    ///
    /// Creates a new dictionary from the serialized data.
    pub fn deserialize_paths<R: std::io::Read>(reader: R) -> std::io::Result<Self>
    where
        V: Default,
    {
        use pathmap::paths_serialization::deserialize_paths;
        use pathmap::zipper::ZipperIteration;

        let mut map = PathMap::new();
        deserialize_paths(map.write_zipper(), reader, V::default())?;

        // Count terms to populate term_count
        let count = {
            let mut rz = map.read_zipper();
            let mut count = 0;
            while rz.to_next_val() {
                count += 1;
            }
            count
        };

        Ok(Self::from_state(map, count))
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
    /// use libdictenstein::pathmap::PathMapDictionary;
    ///
    /// let dict: PathMapDictionary<HashSet<String>> = PathMapDictionary::new();
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

    /// Iterate over all `(term, value)` pairs as raw byte vectors.
    ///
    /// Returns an iterator yielding `(Vec<u8>, V)` tuples in depth-first order.
    /// This is more efficient than `iter()` as it avoids UTF-8 string allocation.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use libdictenstein::pathmap::PathMapDictionary;
    ///
    /// let mut dict = PathMapDictionary::<u32>::new();
    /// dict.insert_with_value("cat", 1);
    /// dict.insert_with_value("dog", 2);
    ///
    /// for (term_bytes, value) in dict.iter_bytes() {
    ///     let term = String::from_utf8(term_bytes).unwrap();
    ///     println!("{} -> {}", term, value);
    /// }
    /// ```
    pub fn iter_bytes(&self) -> DictionaryIterator<PathMapZipper<V>> {
        let zipper = PathMapZipper::new_from_dict(self);
        DictionaryIterator::new(zipper)
    }

    /// Iterate over all `(term, value)` pairs as UTF-8 strings.
    ///
    /// Returns an iterator yielding `(String, V)` tuples in depth-first order.
    /// For better performance with raw bytes, use `iter_bytes()` instead.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use libdictenstein::pathmap::PathMapDictionary;
    ///
    /// let mut dict = PathMapDictionary::<u32>::new();
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

    /// Take an `𝒪(1)` copy-on-write [`PathMapSnapshot`] of the current contents.
    ///
    /// The snapshot is decoupled from later mutations of this dictionary and
    /// can be queried lock-free (e.g. handed to a `Transducer`). The current
    /// term count is captured so the snapshot reports an exact
    /// [`Dictionary::len`].
    pub fn snapshot(&self) -> PathMapSnapshot<V> {
        let state = self.load_state();
        PathMapSnapshot::from_map(state.map.clone()).with_len(state.len)
    }
}

impl<V: DictionaryValue + Default> FromIterator<String> for PathMapDictionary<V> {
    fn from_iter<I: IntoIterator<Item = String>>(iter: I) -> Self {
        Self::from_terms(iter)
    }
}

impl<'a, V: DictionaryValue + Default> FromIterator<&'a str> for PathMapDictionary<V> {
    fn from_iter<I: IntoIterator<Item = &'a str>>(iter: I) -> Self {
        Self::from_terms(iter)
    }
}

impl<V: DictionaryValue + Default> FromIterator<Vec<u8>> for PathMapDictionary<V> {
    fn from_iter<I: IntoIterator<Item = Vec<u8>>>(iter: I) -> Self {
        Self::from_byte_entries(iter.into_iter().map(|key| (key, V::default())).collect())
    }
}

impl<'a, V: DictionaryValue + Default> FromIterator<&'a [u8]> for PathMapDictionary<V> {
    fn from_iter<I: IntoIterator<Item = &'a [u8]>>(iter: I) -> Self {
        iter.into_iter().map(<[u8]>::to_vec).collect()
    }
}

impl<V: DictionaryValue> FromIterator<(String, V)> for PathMapDictionary<V> {
    fn from_iter<I: IntoIterator<Item = (String, V)>>(iter: I) -> Self {
        Self::from_terms_with_values(iter)
    }
}

impl<'a, V: DictionaryValue> FromIterator<(&'a str, V)> for PathMapDictionary<V> {
    fn from_iter<I: IntoIterator<Item = (&'a str, V)>>(iter: I) -> Self {
        Self::from_terms_with_values(iter)
    }
}

impl<V: DictionaryValue> FromIterator<(Vec<u8>, V)> for PathMapDictionary<V> {
    fn from_iter<I: IntoIterator<Item = (Vec<u8>, V)>>(iter: I) -> Self {
        Self::from_byte_entries(iter.into_iter().collect())
    }
}

impl<'a, V: DictionaryValue> FromIterator<(&'a [u8], V)> for PathMapDictionary<V> {
    fn from_iter<I: IntoIterator<Item = (&'a [u8], V)>>(iter: I) -> Self {
        Self::from_byte_entries(
            iter.into_iter()
                .map(|(key, value)| (key.to_vec(), value))
                .collect(),
        )
    }
}

impl<V: DictionaryValue + Default> Extend<String> for PathMapDictionary<V> {
    fn extend<I: IntoIterator<Item = String>>(&mut self, iter: I) {
        self.extend_byte_entries(
            iter.into_iter()
                .map(|key| (key.into_bytes(), V::default()))
                .collect(),
        );
    }
}

impl<'a, V: DictionaryValue + Default> Extend<&'a str> for PathMapDictionary<V> {
    fn extend<I: IntoIterator<Item = &'a str>>(&mut self, iter: I) {
        self.extend(iter.into_iter().map(str::to_owned));
    }
}

impl<V: DictionaryValue + Default> Extend<Vec<u8>> for PathMapDictionary<V> {
    fn extend<I: IntoIterator<Item = Vec<u8>>>(&mut self, iter: I) {
        self.extend_byte_entries(iter.into_iter().map(|key| (key, V::default())).collect());
    }
}

impl<'a, V: DictionaryValue + Default> Extend<&'a [u8]> for PathMapDictionary<V> {
    fn extend<I: IntoIterator<Item = &'a [u8]>>(&mut self, iter: I) {
        self.extend(iter.into_iter().map(<[u8]>::to_vec));
    }
}

impl<V: DictionaryValue> Extend<(String, V)> for PathMapDictionary<V> {
    fn extend<I: IntoIterator<Item = (String, V)>>(&mut self, iter: I) {
        self.extend_byte_entries(
            iter.into_iter()
                .map(|(key, value)| (key.into_bytes(), value))
                .collect(),
        );
    }
}

impl<'a, V: DictionaryValue> Extend<(&'a str, V)> for PathMapDictionary<V> {
    fn extend<I: IntoIterator<Item = (&'a str, V)>>(&mut self, iter: I) {
        self.extend(iter.into_iter().map(|(key, value)| (key.to_owned(), value)));
    }
}

impl<V: DictionaryValue> Extend<(Vec<u8>, V)> for PathMapDictionary<V> {
    fn extend<I: IntoIterator<Item = (Vec<u8>, V)>>(&mut self, iter: I) {
        self.extend_byte_entries(iter.into_iter().collect());
    }
}

impl<'a, V: DictionaryValue> Extend<(&'a [u8], V)> for PathMapDictionary<V> {
    fn extend<I: IntoIterator<Item = (&'a [u8], V)>>(&mut self, iter: I) {
        self.extend_byte_entries(
            iter.into_iter()
                .map(|(key, value)| (key.to_vec(), value))
                .collect(),
        );
    }
}

impl<V: DictionaryValue + Default> Default for PathMapDictionary<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V: DictionaryValue> Dictionary for PathMapDictionary<V> {
    type Node = PathMapNode<V>;

    #[inline]
    fn root(&self) -> Self::Node {
        // 𝒪(1) copy-on-write snapshot (a root refcount bump); queries then run
        // lock-free over a consistent view.
        let state = self.load_state();
        TrieRefNode::new(trie_ref_root(state.map.clone()))
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

impl<V: DictionaryValue> MappedDictionary for PathMapDictionary<V> {
    type Value = V;

    fn get_value(&self, term: &str) -> Option<Self::Value> {
        PathMapDictionary::get_value(self, term)
    }
}

impl<V: DictionaryValue + Default> crate::MutableDictionary for PathMapDictionary<V> {
    fn insert(&self, term: &str) -> bool {
        PathMapDictionary::insert(self, term)
    }

    fn remove(&self, term: &str) -> bool {
        PathMapDictionary::remove(self, term)
    }
}

impl<V: DictionaryValue> crate::MutableMappedDictionary for PathMapDictionary<V> {
    fn insert_with_value(&self, term: &str, value: Self::Value) -> bool {
        PathMapDictionary::insert_with_value(self, term, value)
    }

    fn update_or_insert<F>(&self, term: &str, default_value: Self::Value, update_fn: F) -> bool
    where
        F: Fn(&mut Self::Value),
    {
        PathMapDictionary::update_or_insert(self, term, default_value, update_fn)
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

/// Byte-level dictionary node for [`PathMapDictionary`].
///
/// A thin [`TrieRefNode`] over an owned, `𝒪(1)` copy-on-write snapshot
/// ([`TrieRefOwned`]) of the map, taken when [`Dictionary::root`]
/// is called. Every `is_final` / `transition` / `edges` / `value` call is
/// **lock-free** and descends `𝒪(1)` per byte from the focus — no per-operation
/// lock and no replay of the path from the root. (The historical path-replay
/// node it replaces cost `𝒪(n²)` byte-steps plus `n` lock round-trips to walk a
/// term of length `n`, and rescanned all 256 child bytes in `edges()`.)
///
/// # Snapshot isolation
///
/// A node binds to the map's contents at the moment `root()` was called;
/// concurrent mutations of the originating [`PathMapDictionary`] are not
/// observed mid-traversal. This *replaces* the previous torn-traversal hazard
/// (a fresh lock per operation over a live, mutating map) with proper snapshot
/// isolation — aligned with PathMap's persistent, copy-on-write nodes.
pub type PathMapNode<V> = TrieRefNode<V, TrieRefOwned<V>>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DictionaryNode, MappedDictionaryNode};

    #[test]
    fn test_pathmap_dictionary_creation() {
        let dict: PathMapDictionary<()> =
            PathMapDictionary::from_terms(vec!["hello", "world", "test"]);
        assert_eq!(dict.len(), Some(3));
    }

    #[test]
    fn test_pathmap_dictionary_contains() {
        let dict: PathMapDictionary<()> = PathMapDictionary::from_terms(vec!["hello", "world"]);
        assert!(dict.contains("hello"));
        assert!(dict.contains("world"));
        assert!(!dict.contains("goodbye"));
    }

    #[test]
    fn test_pathmap_node_traversal() {
        let dict: PathMapDictionary<()> = PathMapDictionary::from_terms(vec!["test", "testing"]);
        let root = dict.root();

        // Navigate: t -> e -> s -> t
        let t = root.transition(b't').expect("should have 't'");
        let e = t.transition(b'e').expect("should have 'e'");
        let s = e.transition(b's').expect("should have 's'");
        let t2 = s.transition(b't').expect("should have second 't'");

        assert!(t2.is_final(), "'test' should be final");

        // Continue: i -> n -> g
        let i = t2.transition(b'i').expect("should have 'i'");
        assert!(!i.is_final(), "'testi' should not be final");
    }

    #[test]
    fn test_pathmap_node_edges() {
        let dict: PathMapDictionary<()> = PathMapDictionary::from_terms(vec!["ab", "ac", "ad"]);
        let root = dict.root();
        let a = root.transition(b'a').expect("should have 'a'");

        let edges: Vec<_> = a.edges().map(|(byte, _)| byte).collect();
        assert_eq!(edges.len(), 3);
        assert!(edges.contains(&b'b'));
        assert!(edges.contains(&b'c'));
        assert!(edges.contains(&b'd'));
    }

    #[test]
    fn test_pathmap_dictionary_insert() {
        let dict: PathMapDictionary<()> = PathMapDictionary::from_terms(vec!["test"]);
        assert_eq!(dict.term_count(), 1);

        // Insert new term
        assert!(dict.insert("testing"));
        assert_eq!(dict.term_count(), 2);
        assert!(dict.contains("testing"));

        // Insert duplicate
        assert!(!dict.insert("test"));
        assert_eq!(dict.term_count(), 2);
    }

    #[test]
    fn test_pathmap_dictionary_remove() {
        let dict: PathMapDictionary<()> =
            PathMapDictionary::from_terms(vec!["test", "testing", "tested"]);
        assert_eq!(dict.term_count(), 3);

        // Remove existing term
        assert!(dict.remove("testing"));
        assert_eq!(dict.term_count(), 2);
        assert!(!dict.contains("testing"));
        assert!(dict.contains("test"));
        assert!(dict.contains("tested"));

        // Remove non-existent term
        assert!(!dict.remove("nonexistent"));
        assert_eq!(dict.term_count(), 2);
    }

    #[test]
    fn test_pathmap_dictionary_clear() {
        let dict: PathMapDictionary<()> = PathMapDictionary::from_terms(vec!["test", "testing"]);
        assert_eq!(dict.term_count(), 2);

        dict.clear();
        assert_eq!(dict.term_count(), 0);
        assert!(!dict.contains("test"));
        assert!(!dict.contains("testing"));
    }

    #[test]
    fn test_pathmap_dictionary_concurrent_operations() {
        use std::thread;

        let dict: PathMapDictionary<()> = PathMapDictionary::from_terms(vec!["test"]);
        let dict_clone = dict.clone();

        // Spawn thread that inserts while main thread queries
        let handle = thread::spawn(move || {
            dict_clone.insert("testing");
            dict_clone.insert("tested");
        });

        // Query while other thread modifies
        let _ = dict.contains("test");

        handle.join().unwrap();

        // Verify final state
        assert!(dict.contains("test"));
        assert!(dict.contains("testing"));
        assert!(dict.contains("tested"));
        assert_eq!(dict.term_count(), 3);
    }

    #[test]
    fn test_pathmap_dictionary_with_values() {
        // Test dictionary with u32 values (scope IDs)
        let terms_with_values = vec![("hello", 1u32), ("world", 2u32), ("test", 3u32)];
        let dict: PathMapDictionary<u32> =
            PathMapDictionary::from_terms_with_values(terms_with_values);

        assert_eq!(dict.len(), Some(3));
        assert!(dict.contains("hello"));
        assert_eq!(dict.get_value("hello"), Some(1));
        assert_eq!(dict.get_value("world"), Some(2));
        assert_eq!(dict.get_value("test"), Some(3));
        assert_eq!(dict.get_value("missing"), None);
    }

    #[test]
    fn test_pathmap_dictionary_insert_with_value() {
        let dict: PathMapDictionary<u32> = PathMapDictionary::new();

        // Insert with values
        assert!(dict.insert_with_value("hello", 42));
        assert_eq!(dict.get_value("hello"), Some(42));

        // Update existing value
        assert!(!dict.insert_with_value("hello", 99));
        assert_eq!(dict.get_value("hello"), Some(99));
        assert_eq!(dict.term_count(), 1);
    }

    #[test]
    fn test_pathmap_node_value() {
        let terms_with_values = vec![("hello", 10u32), ("world", 20u32)];
        let dict: PathMapDictionary<u32> =
            PathMapDictionary::from_terms_with_values(terms_with_values);
        let root = dict.root();

        // Navigate to "hello"
        let h = root.transition(b'h').expect("should have 'h'");
        let e = h.transition(b'e').expect("should have 'e'");
        let l1 = e.transition(b'l').expect("should have first 'l'");
        let l2 = l1.transition(b'l').expect("should have second 'l'");
        let o = l2.transition(b'o').expect("should have 'o'");

        assert!(o.is_final());
        assert_eq!(o.value(), Some(10));

        // Non-final node should have no value
        assert!(!h.is_final());
        assert_eq!(h.value(), None);
    }

    #[test]
    fn test_snapshot_is_consistent_and_decoupled() {
        let dict: PathMapDictionary<u32> = PathMapDictionary::new();
        dict.insert_with_value("alpha", 1);

        // Snapshot reports an exact length and answers queries lock-free.
        let snap = dict.snapshot();
        assert_eq!(snap.len(), Some(1));
        assert!(snap.contains("alpha"));
        assert_eq!(snap.get_value("alpha"), Some(1));

        // Mutating the dictionary after snapshotting does not affect the snapshot.
        dict.insert_with_value("beta", 2);
        assert!(!snap.contains("beta"));
        assert!(dict.contains("beta"));

        // A fresh root observes the new state (the snapshot is taken at root() time).
        assert!(dict.root().transition(b'b').is_some());
    }

    #[test]
    fn test_root_snapshot_isolation_during_mutation() {
        // A root taken before a mutation keeps observing the pre-mutation trie.
        let dict: PathMapDictionary<()> = PathMapDictionary::from_terms(vec!["cat"]);
        let root_before = dict.root();
        dict.insert("car");
        // Under "ca", the pre-mutation root sees only "cat"'s 't', not "car"'s 'r'.
        let a = root_before
            .transition(b'c')
            .and_then(|c| c.transition(b'a'))
            .expect("'ca'");
        let mut labels: Vec<u8> = a.edges().map(|(b, _)| b).collect();
        labels.sort_unstable();
        assert_eq!(
            labels,
            vec![b't'],
            "pre-mutation snapshot must not see 'car'"
        );
    }
}
