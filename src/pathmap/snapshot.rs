//! Zero-plumbing, MORK-facing PathMap dictionaries.
//!
//! These types let a caller that already holds a `PathMap<V>` (such as MORK's
//! `Space.btm`) run fuzzy queries against it **without copying the trie and
//! without taking it behind a lock**. They are thin `Dictionary` /
//! `MappedDictionary` wrappers over a single [`TrieRefOwned`] / [`TrieRefBorrowed`]
//! root handle (see [`crate::pathmap::core`]).
//!
//! | Type                  | Root handle              | Cost to construct | Lifetime |
//! |·······················|··························|···················|··········|
//! | [`PathMapSnapshot`]   | [`TrieRefOwned`]         | `𝒪(1)` CoW bump   | owned    |
//! | [`PathMapRef`]        | [`TrieRefBorrowed`]      | zero-copy borrow  | `'a`     |
//! | [`PathMapSnapshotChar`] | [`TrieRefOwned`]       | `𝒪(1)` CoW bump   | owned    |
//! | [`PathMapRefChar`]    | [`TrieRefBorrowed`]      | zero-copy borrow  | `'a`     |
//!
//! # Example (MORK-style)
//!
//! ```ignore
//! // Zero-copy borrow of a live map:
//! let dict = PathMapRef::from_map(&space.btm);
//! // …or an 𝒪(1) copy-on-write snapshot decoupled from later mutations:
//! let dict = PathMapSnapshot::from_map_ref(&space.btm);
//! // …or scope the search to a subtrie rooted at some prefix:
//! let dict = PathMapRef::from_trie_ref(space.btm.trie_ref_at_path(prefix));
//!
//! Transducer::new(dict, Algorithm::Standard).query_with_distance("fooo", 1);
//! ```
//!
//! # `len()` is best-effort
//!
//! A bare `PathMap` does not carry a term count, so these wrappers report
//! `len() == None` (unknown) unless one is supplied via
//! [`PathMapSnapshot::with_len`] and friends. With an unknown length,
//! [`Dictionary::is_empty`] conservatively returns `false`; querying an empty
//! snapshot is still correct (the root simply has no children).

use super::core::{
    trie_ref_root, trie_ref_root_borrowed, TrieRefLike, TrieRefNode, TrieRefNodeChar,
};
use crate::value::DictionaryValue;
use crate::{Dictionary, MappedDictionary, SyncStrategy};
use pathmap::zipper::{
    ReadZipperOwned, ReadZipperUntracked, TrieRefBorrowed, TrieRefOwned, ZipperReadOnlySubtries,
};
use pathmap::PathMap;

/// Look up `term`'s value by descending the term's UTF-8 bytes from `root`.
///
/// Shared by every wrapper in this module: terms are stored as UTF-8 bytes in
/// PathMap regardless of whether the node abstraction is byte- or char-level,
/// so a byte-path descent is the correct lookup in both cases.
#[inline]
fn value_at<V, R>(root: &R, term: &str) -> Option<V>
where
    V: DictionaryValue,
    R: TrieRefLike<V>,
{
    root.descend_bytes(term.as_bytes()).val_cloned()
}

// =============================================================================
// PathMapSnapshot<V> — owned 𝒪(1) snapshot, byte-level nodes
// =============================================================================

/// An owned, immutable snapshot of a `PathMap`, exposing byte-level
/// (`Unit = u8`) traversal.
///
/// Construction is `𝒪(1)`: it captures the map's persistent root by reference
/// count. Subsequent mutations of the originating map (which copy shared nodes
/// on write) are not observed through this snapshot.
#[derive(Clone)]
pub struct PathMapSnapshot<V: DictionaryValue> {
    root: TrieRefOwned<V>,
    len: Option<usize>,
}

impl<V: DictionaryValue> PathMapSnapshot<V> {
    /// Snapshot an owned `PathMap` (consumes it).
    #[inline]
    pub fn from_map(map: PathMap<V>) -> Self {
        Self {
            root: trie_ref_root(map),
            len: None,
        }
    }

    /// Take an `𝒪(1)` copy-on-write snapshot of a borrowed `PathMap`.
    ///
    /// `PathMap::clone` is a root refcount bump; writes to the original map copy
    /// shared nodes, so this snapshot is never observed mid-mutation.
    #[inline]
    pub fn from_map_ref(map: &PathMap<V>) -> Self {
        Self {
            root: trie_ref_root(map.clone()),
            len: None,
        }
    }

    /// Wrap an existing owned TrieRef root (e.g. a subtrie handle).
    #[inline]
    pub fn from_trie_ref(root: TrieRefOwned<V>) -> Self {
        Self { root, len: None }
    }

    /// Snapshot the subtrie at an owned read zipper's focus.
    ///
    /// Pair with `map.clone().into_read_zipper(prefix)` to scope a fuzzy search
    /// to a prefix-rooted subtrie.
    #[inline]
    pub fn from_read_zipper(zipper: ReadZipperOwned<V>) -> Self {
        Self {
            root: zipper.trie_ref_at_path::<&[u8]>(&[]),
            len: None,
        }
    }

    /// Attach a known term count so [`Dictionary::len`] / [`Dictionary::is_empty`]
    /// report exact answers.
    #[inline]
    pub fn with_len(mut self, len: usize) -> Self {
        self.len = Some(len);
        self
    }

    /// Borrow the underlying owned root handle.
    #[inline]
    pub fn root_ref(&self) -> &TrieRefOwned<V> {
        &self.root
    }
}

impl<V: DictionaryValue> Dictionary for PathMapSnapshot<V> {
    type Node = TrieRefNode<V, TrieRefOwned<V>>;

    #[inline]
    fn root(&self) -> Self::Node {
        TrieRefNode::new(self.root.clone())
    }

    #[inline]
    fn len(&self) -> Option<usize> {
        self.len
    }

    #[inline]
    fn sync_strategy(&self) -> SyncStrategy {
        SyncStrategy::Persistent
    }
}

impl<V: DictionaryValue> MappedDictionary for PathMapSnapshot<V> {
    type Value = V;

    #[inline]
    fn get_value(&self, term: &str) -> Option<Self::Value> {
        value_at(&self.root, term)
    }
}

// =============================================================================
// PathMapRef<'a, V> — borrowed zero-copy view, byte-level nodes
// =============================================================================

/// A borrowed, zero-copy view of a `PathMap`, exposing byte-level
/// (`Unit = u8`) traversal. Holds no lock and copies nothing; it simply reads
/// through the borrow for its lifetime `'a`.
#[derive(Clone)]
pub struct PathMapRef<'a, V: DictionaryValue> {
    root: TrieRefBorrowed<'a, V>,
    len: Option<usize>,
}

impl<'a, V: DictionaryValue> PathMapRef<'a, V> {
    /// Borrow a `PathMap` directly (zero copy).
    #[inline]
    pub fn from_map(map: &'a PathMap<V>) -> Self {
        Self {
            root: trie_ref_root_borrowed(map),
            len: None,
        }
    }

    /// Wrap an existing borrowed TrieRef root (e.g. `map.trie_ref_at_path(prefix)`).
    #[inline]
    pub fn from_trie_ref(root: TrieRefBorrowed<'a, V>) -> Self {
        Self { root, len: None }
    }

    /// View the subtrie at a borrowed read zipper's focus (e.g. `map.read_zipper()`).
    #[inline]
    pub fn from_read_zipper(zipper: &ReadZipperUntracked<'a, 'static, V>) -> Self {
        Self {
            root: zipper.trie_ref_at_path::<&[u8]>(&[]),
            len: None,
        }
    }

    /// Attach a known term count.
    #[inline]
    pub fn with_len(mut self, len: usize) -> Self {
        self.len = Some(len);
        self
    }

    /// Borrow the underlying borrowed root handle.
    #[inline]
    pub fn root_ref(&self) -> &TrieRefBorrowed<'a, V> {
        &self.root
    }
}

impl<'a, V: DictionaryValue> Dictionary for PathMapRef<'a, V> {
    type Node = TrieRefNode<V, TrieRefBorrowed<'a, V>>;

    #[inline]
    fn root(&self) -> Self::Node {
        // `TrieRefBorrowed` is `Copy`; reading the field copies it.
        TrieRefNode::new(self.root)
    }

    #[inline]
    fn len(&self) -> Option<usize> {
        self.len
    }

    #[inline]
    fn sync_strategy(&self) -> SyncStrategy {
        SyncStrategy::Persistent
    }
}

impl<'a, V: DictionaryValue> MappedDictionary for PathMapRef<'a, V> {
    type Value = V;

    #[inline]
    fn get_value(&self, term: &str) -> Option<Self::Value> {
        value_at(&self.root, term)
    }
}

// =============================================================================
// PathMapSnapshotChar<V> — owned 𝒪(1) snapshot, char-level nodes
// =============================================================================

/// An owned, immutable snapshot of a `PathMap`, exposing character-level
/// (`Unit = char`) traversal with correct Unicode edit-distance semantics.
#[derive(Clone)]
pub struct PathMapSnapshotChar<V: DictionaryValue> {
    root: TrieRefOwned<V>,
    len: Option<usize>,
}

impl<V: DictionaryValue> PathMapSnapshotChar<V> {
    /// Snapshot an owned `PathMap` (consumes it).
    #[inline]
    pub fn from_map(map: PathMap<V>) -> Self {
        Self {
            root: trie_ref_root(map),
            len: None,
        }
    }

    /// Take an `𝒪(1)` copy-on-write snapshot of a borrowed `PathMap`.
    #[inline]
    pub fn from_map_ref(map: &PathMap<V>) -> Self {
        Self {
            root: trie_ref_root(map.clone()),
            len: None,
        }
    }

    /// Wrap an existing owned TrieRef root.
    #[inline]
    pub fn from_trie_ref(root: TrieRefOwned<V>) -> Self {
        Self { root, len: None }
    }

    /// Snapshot the subtrie at an owned read zipper's focus.
    #[inline]
    pub fn from_read_zipper(zipper: ReadZipperOwned<V>) -> Self {
        Self {
            root: zipper.trie_ref_at_path::<&[u8]>(&[]),
            len: None,
        }
    }

    /// Attach a known term count.
    #[inline]
    pub fn with_len(mut self, len: usize) -> Self {
        self.len = Some(len);
        self
    }
}

impl<V: DictionaryValue> Dictionary for PathMapSnapshotChar<V> {
    type Node = TrieRefNodeChar<V, TrieRefOwned<V>>;

    #[inline]
    fn root(&self) -> Self::Node {
        TrieRefNodeChar::new(self.root.clone())
    }

    #[inline]
    fn len(&self) -> Option<usize> {
        self.len
    }

    #[inline]
    fn sync_strategy(&self) -> SyncStrategy {
        SyncStrategy::Persistent
    }
}

impl<V: DictionaryValue> MappedDictionary for PathMapSnapshotChar<V> {
    type Value = V;

    #[inline]
    fn get_value(&self, term: &str) -> Option<Self::Value> {
        value_at(&self.root, term)
    }
}

// =============================================================================
// PathMapRefChar<'a, V> — borrowed zero-copy view, char-level nodes
// =============================================================================

/// A borrowed, zero-copy view of a `PathMap`, exposing character-level
/// (`Unit = char`) traversal with correct Unicode edit-distance semantics.
#[derive(Clone)]
pub struct PathMapRefChar<'a, V: DictionaryValue> {
    root: TrieRefBorrowed<'a, V>,
    len: Option<usize>,
}

impl<'a, V: DictionaryValue> PathMapRefChar<'a, V> {
    /// Borrow a `PathMap` directly (zero copy).
    #[inline]
    pub fn from_map(map: &'a PathMap<V>) -> Self {
        Self {
            root: trie_ref_root_borrowed(map),
            len: None,
        }
    }

    /// Wrap an existing borrowed TrieRef root.
    #[inline]
    pub fn from_trie_ref(root: TrieRefBorrowed<'a, V>) -> Self {
        Self { root, len: None }
    }

    /// View the subtrie at a borrowed read zipper's focus.
    #[inline]
    pub fn from_read_zipper(zipper: &ReadZipperUntracked<'a, 'static, V>) -> Self {
        Self {
            root: zipper.trie_ref_at_path::<&[u8]>(&[]),
            len: None,
        }
    }

    /// Attach a known term count.
    #[inline]
    pub fn with_len(mut self, len: usize) -> Self {
        self.len = Some(len);
        self
    }
}

impl<'a, V: DictionaryValue> Dictionary for PathMapRefChar<'a, V> {
    type Node = TrieRefNodeChar<V, TrieRefBorrowed<'a, V>>;

    #[inline]
    fn root(&self) -> Self::Node {
        // `TrieRefBorrowed` is `Copy`; reading the field copies it.
        TrieRefNodeChar::new(self.root)
    }

    #[inline]
    fn len(&self) -> Option<usize> {
        self.len
    }

    #[inline]
    fn sync_strategy(&self) -> SyncStrategy {
        SyncStrategy::Persistent
    }
}

impl<'a, V: DictionaryValue> MappedDictionary for PathMapRefChar<'a, V> {
    type Value = V;

    #[inline]
    fn get_value(&self, term: &str) -> Option<Self::Value> {
        value_at(&self.root, term)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pathmap::PathMap;

    fn assert_send_sync<T: Send + Sync>() {}

    fn map_of(pairs: &[(&str, u32)]) -> PathMap<u32> {
        let mut map = PathMap::new();
        for (term, value) in pairs {
            map.insert(term.as_bytes(), *value);
        }
        map
    }

    #[test]
    fn snapshot_contains_and_get_value() {
        let snap = PathMapSnapshot::from_map(map_of(&[("hello", 1), ("help", 2), ("world", 3)]))
            .with_len(3);
        assert_eq!(snap.len(), Some(3));
        assert!(!snap.is_empty());
        assert!(snap.contains("hello"));
        assert!(snap.contains("help"));
        assert!(!snap.contains("hel"));
        assert_eq!(snap.get_value("hello"), Some(1));
        assert_eq!(snap.get_value("world"), Some(3));
        assert_eq!(snap.get_value("missing"), None);
        assert_eq!(snap.sync_strategy(), SyncStrategy::Persistent);
    }

    #[test]
    fn cow_snapshot_is_decoupled_from_later_mutations() {
        let mut map = map_of(&[("alpha", 1)]);
        let snap = PathMapSnapshot::from_map_ref(&map);
        // Mutate the original map after snapshotting.
        map.insert(b"beta", 2);
        // The snapshot still sees only the pre-mutation state.
        assert!(snap.contains("alpha"));
        assert!(!snap.contains("beta"));
    }

    #[test]
    fn borrowed_ref_reads_live_map() {
        let map = map_of(&[("cat", 1), ("car", 2)]);
        let dict = PathMapRef::from_map(&map);
        assert!(dict.contains("cat"));
        assert!(dict.contains("car"));
        assert!(!dict.contains("ca"));
        assert_eq!(dict.get_value("cat"), Some(1));
        assert_eq!(dict.len(), None);
        // Unknown length => conservatively reported non-empty.
        assert!(!dict.is_empty());
    }

    #[test]
    fn from_trie_ref_scopes_to_subtrie() {
        let map = map_of(&[("apple", 1), ("apply", 2), ("banana", 3)]);
        // Borrowed subtrie rooted at "appl".
        let sub = PathMapRef::from_trie_ref(map.trie_ref_at_path(b"appl"));
        // Within the subtrie, the remaining suffixes are "e" and "y".
        let root = sub.root();
        use crate::DictionaryNode;
        let mut labels: Vec<u8> = root.edges().map(|(b, _)| b).collect();
        labels.sort_unstable();
        assert_eq!(labels, vec![b'e', b'y']);
    }

    #[test]
    fn from_read_zipper_owned_and_borrowed() {
        let map = map_of(&[("foo", 1), ("foobar", 2)]);

        // Borrowed read zipper over the whole map.
        let rz = map.read_zipper();
        let borrowed = PathMapRef::from_read_zipper(&rz);
        assert!(borrowed.contains("foo"));
        assert!(borrowed.contains("foobar"));
        drop(rz);

        // Owned read zipper rooted at a prefix => owned snapshot of that subtrie.
        let owned_rz = map.clone().into_read_zipper(b"foo");
        let snap = PathMapSnapshot::from_read_zipper(owned_rz);
        // Relative to "foo": "" (foo) and "bar" (foobar).
        assert!(snap.contains(""));
        assert!(snap.contains("bar"));
        assert!(!snap.contains("foo"));
    }

    #[test]
    fn char_snapshot_unicode() {
        let snap = PathMapSnapshotChar::from_map(map_of(&[("café", 1), ("中文", 2), ("🎉", 3)]));
        assert!(snap.contains("café"));
        assert!(snap.contains("中文"));
        assert!(snap.contains("🎉"));
        assert!(!snap.contains("cafe"));
        assert_eq!(snap.get_value("中文"), Some(2));
    }

    #[test]
    fn char_ref_unicode() {
        let map = map_of(&[("naïve", 1), ("日本語", 2)]);
        let dict = PathMapRefChar::from_map(&map);
        assert!(dict.contains("naïve"));
        assert!(dict.contains("日本語"));
        assert_eq!(dict.get_value("naïve"), Some(1));
    }

    #[test]
    fn empty_snapshot_queries_safely() {
        let snap = PathMapSnapshot::<u32>::from_map(PathMap::new());
        assert!(!snap.contains("anything"));
        assert_eq!(snap.get_value("anything"), None);
        assert_eq!(snap.len(), None);
    }

    #[test]
    fn snapshot_and_ref_adapters_are_send_sync() {
        assert_send_sync::<PathMapSnapshot<u32>>();
        assert_send_sync::<PathMapSnapshotChar<u32>>();
        assert_send_sync::<PathMapRef<'static, u32>>();
        assert_send_sync::<PathMapRefChar<'static, u32>>();
    }
}
