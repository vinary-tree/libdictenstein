//! Lock-free, `𝒪(1)`-from-focus PathMap dictionary adapters built on `TrieRef`.
//!
//! This module is the foundation of the TrieRef-based PathMap integration. It
//! replaces the historical *path-replay* adapters — which re-walked the trie
//! from the root under a fresh read lock on **every** operation — with thin
//! value-type handles over PathMap's own [`TrieRefOwned`] / [`TrieRefBorrowed`]
//! node references.
//!
//! # Why TrieRef
//!
//! The previous `PathMapNode` stored `{ Arc<RwLock<PathMap>>, Arc<Vec<u8>> path }`
//! and, for each `is_final` / `transition` / `edges` / `value` call, acquired a
//! read lock and called `read_zipper_at_path(path)`, descending the **entire**
//! path from the root again. Walking a term of length `n` therefore cost
//! `𝒪(n²)` byte-steps plus `n` lock round-trips, and `edges()` additionally
//! scanned all 256 possible child bytes and re-validated each survivor with yet
//! another lock + root replay.
//!
//! `TrieRef` (pathmap ≥ 0.2.2) is a cheap, `Clone` + `Send` + `Sync`, **lock-free**
//! handle on a trie *node*. Descending one byte is `𝒪(1)` from the focus — no
//! root replay, no lock, no path copy. [`TrieRefLike::child_mask`] yields the
//! exact set of existing child bytes, so [`ByteMask::iter`] (word-skipping)
//! drives edge iteration with no per-child re-validation.
//!
//! # The sealed adapter trait
//!
//! [`TrieRefLike`] is a small **sealed** trait that insulates the rest of the
//! crate from pathmap's lifetime plumbing and from API drift between pathmap
//! 0.2.x and 0.3.x. It is implemented only for [`TrieRefOwned`] (owned `𝒪(1)`
//! snapshot handle) and [`TrieRefBorrowed`] (zero-copy borrow of a live map),
//! and exposes exactly the six operations the dictionary nodes need. Because it
//! is sealed, no downstream crate can implement it, and we own every point of
//! contact with pathmap's read-only subtrie API.
//!
//! # Snapshot semantics
//!
//! A [`TrieRefOwned`] captures the trie at the moment it is created (an `𝒪(1)`
//! refcount bump on PathMap's persistent, copy-on-write nodes). A node or zipper
//! built from it observes a **consistent snapshot**: concurrent mutations to the
//! originating map are not seen mid-traversal. This *replaces* the previous
//! torn-traversal hazard (a separate lock acquisition per operation over a live,
//! mutating map) with proper snapshot isolation — strictly better-defined and
//! aligned with the persistent-data-structure model PathMap already provides.

use crate::value::DictionaryValue;
use crate::{DictionaryNode, MappedDictionaryNode};
use pathmap::utils::ByteMask;
use pathmap::zipper::{
    TrieRefBorrowed, TrieRefOwned, Zipper, ZipperReadOnlySubtries, ZipperValues,
};
use pathmap::PathMap;
use std::marker::PhantomData;

mod sealed {
    /// Seals [`super::TrieRefLike`] so it can only ever be implemented for the
    /// pathmap handle types this module provides impls for.
    pub trait Sealed {}
}

/// A uniform, **sealed**, lock-free view over a pathmap trie *node*.
///
/// Implemented for [`TrieRefOwned`] (owned snapshot handle; `𝒪(1)` `Clone`) and
/// [`TrieRefBorrowed`] (zero-copy borrow of a live map). Every method maps to a
/// constant-time pathmap operation on the handle's focus node — there is no lock
/// acquisition and no root replay anywhere in this trait.
///
/// All bounds required by the dictionary node abstraction
/// (`Clone + Send + Sync`) are satisfied by both implementors for any
/// `V: DictionaryValue` (which is itself `Clone + Send + Sync + Unpin + 'static`).
pub trait TrieRefLike<V>: Clone + Send + Sync + sealed::Sealed {
    /// `true` if the focus sits on a path that exists in the trie.
    fn path_exists(&self) -> bool;

    /// `true` if a value is stored at the focus (i.e. this is a final node).
    fn is_val(&self) -> bool;

    /// The value at the focus, cloned, or `None` if the focus is not final.
    fn val_cloned(&self) -> Option<V>;

    /// 256-bit mask of the child bytes branching from the focus.
    ///
    /// Membership in this mask is proof that descending the corresponding byte
    /// lands on an existing path, so callers need not re-validate.
    fn child_mask(&self) -> ByteMask;

    /// Number of child branches from the focus.
    fn child_count(&self) -> usize;

    /// Descend `bytes` **from the focus** (not from the root), returning a new
    /// handle. This is `𝒪(bytes.len())` from the current position and performs
    /// no locking. Descending a non-existent path yields a handle whose
    /// [`path_exists`](Self::path_exists) is `false` (pathmap stores a dangling
    /// remainder of up to 48 bytes as a node key; a longer remainder yields an
    /// invalid sentinel handle) — never a panic.
    fn descend_bytes(&self, bytes: &[u8]) -> Self;
}

impl<V: DictionaryValue> sealed::Sealed for TrieRefOwned<V> {}
impl<V: DictionaryValue> TrieRefLike<V> for TrieRefOwned<V> {
    #[inline]
    fn path_exists(&self) -> bool {
        Zipper::path_exists(self)
    }
    #[inline]
    fn is_val(&self) -> bool {
        Zipper::is_val(self)
    }
    #[inline]
    fn val_cloned(&self) -> Option<V> {
        ZipperValues::val(self).cloned()
    }
    #[inline]
    fn child_mask(&self) -> ByteMask {
        Zipper::child_mask(self)
    }
    #[inline]
    fn child_count(&self) -> usize {
        Zipper::child_count(self)
    }
    #[inline]
    fn descend_bytes(&self, bytes: &[u8]) -> Self {
        // `TrieRefOwned::TrieRefT == TrieRefOwned`, and the returned handle owns
        // its focus node (a refcount clone), so it is independent of `self`.
        self.trie_ref_at_path(bytes)
    }
}

impl<V: DictionaryValue> sealed::Sealed for TrieRefBorrowed<'_, V> {}
impl<'a, V: DictionaryValue> TrieRefLike<V> for TrieRefBorrowed<'a, V> {
    #[inline]
    fn path_exists(&self) -> bool {
        Zipper::path_exists(self)
    }
    #[inline]
    fn is_val(&self) -> bool {
        Zipper::is_val(self)
    }
    #[inline]
    fn val_cloned(&self) -> Option<V> {
        ZipperValues::val(self).cloned()
    }
    #[inline]
    fn child_mask(&self) -> ByteMask {
        Zipper::child_mask(self)
    }
    #[inline]
    fn child_count(&self) -> usize {
        Zipper::child_count(self)
    }
    #[inline]
    fn descend_bytes(&self, bytes: &[u8]) -> Self {
        // `TrieRefBorrowed<'a>::TrieRefT == TrieRefBorrowed<'a>`: the descent
        // borrows from the underlying map for `'a`, NOT from `self`, so the
        // child outlives the `&self` borrow.
        self.trie_ref_at_path(bytes)
    }
}

/// Build an **owned** TrieRef root from a (cheaply cloned) `PathMap`.
///
/// Uses the portable two-step path that is identical in pathmap 0.2.2 and 0.3.0:
/// `map.into_read_zipper(&[]).trie_ref_at_path(&[])`. `into_read_zipper` calls
/// `ensure_root()`, so an empty map is safe. The returned [`TrieRefOwned`] owns
/// its focus node, so the temporary zipper may drop immediately.
#[inline]
pub fn trie_ref_root<V: DictionaryValue>(map: PathMap<V>) -> TrieRefOwned<V> {
    map.into_read_zipper::<&[u8]>(&[])
        .trie_ref_at_path::<&[u8]>(&[])
}

/// Build a **borrowed** TrieRef root that reads directly from `map` with zero
/// copying. The returned [`TrieRefBorrowed`] borrows `map` for its lifetime.
#[inline]
pub fn trie_ref_root_borrowed<V: DictionaryValue>(map: &PathMap<V>) -> TrieRefBorrowed<'_, V> {
    map.trie_ref_at_path::<&[u8]>(&[])
}

/// Length in bytes of the UTF-8 sequence whose leading byte is `first_byte`,
/// or `None` if `first_byte` is not a valid UTF-8 leading byte (e.g. a stray
/// continuation byte `0b10xx_xxxx`).
///
/// Moved here from `pathmap_char.rs` so both the byte- and char-level nodes can
/// share it.
#[inline]
pub(crate) fn utf8_sequence_len(first_byte: u8) -> Option<usize> {
    if first_byte & 0b1000_0000 == 0 {
        Some(1)
    } else if first_byte & 0b1110_0000 == 0b1100_0000 {
        Some(2)
    } else if first_byte & 0b1111_0000 == 0b1110_0000 {
        Some(3)
    } else if first_byte & 0b1111_1000 == 0b1111_0000 {
        Some(4)
    } else {
        None
    }
}

// =============================================================================
// Byte-level node (Unit = u8)
// =============================================================================

/// Byte-level dictionary node backed by a [`TrieRefLike`] handle.
///
/// This is the TrieRef realization of the PathMap dictionary node the PathMap
/// developer suggested ("`PathMapNode` should be replaceable by `TrieRef`").
/// Defaulting `R` to [`TrieRefOwned`] makes `TrieRefNode<V>` the owned-snapshot
/// node used by the lock-based `PathMapDictionary`; choosing
/// `R = TrieRefBorrowed<'a, V>` yields a zero-copy node over a borrowed map.
///
/// Each operation is a constant-time, lock-free call on the wrapped handle.
pub struct TrieRefNode<V: DictionaryValue, R: TrieRefLike<V> = TrieRefOwned<V>> {
    r: R,
    _v: PhantomData<fn() -> V>,
}

impl<V: DictionaryValue, R: TrieRefLike<V>> TrieRefNode<V, R> {
    /// Wrap a TrieRef handle as a dictionary node.
    #[inline]
    pub fn new(r: R) -> Self {
        Self { r, _v: PhantomData }
    }

    /// Borrow the underlying TrieRef handle.
    #[inline]
    pub fn trie_ref(&self) -> &R {
        &self.r
    }
}

// `#[derive(Clone)]` would spuriously require `V: Clone`; clone is just the
// handle's refcount bump (`V` lives only in `PhantomData<fn() -> V>`).
impl<V: DictionaryValue, R: TrieRefLike<V>> Clone for TrieRefNode<V, R> {
    #[inline]
    fn clone(&self) -> Self {
        Self {
            r: self.r.clone(),
            _v: PhantomData,
        }
    }
}

impl<V: DictionaryValue, R: TrieRefLike<V>> DictionaryNode for TrieRefNode<V, R> {
    type Unit = u8;

    #[inline]
    fn is_final(&self) -> bool {
        self.r.is_val()
    }

    #[inline]
    fn transition(&self, label: u8) -> Option<Self> {
        let child = self.r.descend_bytes(&[label]);
        if child.path_exists() {
            Some(Self::new(child))
        } else {
            None
        }
    }

    fn edges(&self) -> Box<dyn Iterator<Item = (u8, Self)> + '_> {
        // The child mask already proves each byte lands on an existing path, so
        // we descend directly with no re-validation. `ByteMask::iter()` is
        // word-skipping, not a 256-way bit scan.
        let r = self.r.clone();
        Box::new(self.r.child_mask().iter().map(move |byte| {
            let child = r.descend_bytes(&[byte]);
            (byte, Self::new(child))
        }))
    }

    #[inline]
    fn edge_count(&self) -> Option<usize> {
        Some(self.r.child_count())
    }
}

impl<V: DictionaryValue, R: TrieRefLike<V>> MappedDictionaryNode for TrieRefNode<V, R> {
    type Value = V;

    #[inline]
    fn value(&self) -> Option<Self::Value> {
        self.r.val_cloned()
    }
}

// =============================================================================
// Character-level node (Unit = char)
// =============================================================================

/// Character-level dictionary node backed by a [`TrieRefLike`] handle.
///
/// Terms are stored as UTF-8 bytes in PathMap (unchanged); this node decodes
/// UTF-8 on the fly so edge labels and distances are measured in `char`s, not
/// bytes. Unlike the historical `PathMapNodeChar`, continuation bytes are
/// discovered by descending **locally from the focus** and reading child masks,
/// never by replaying the whole byte path from the root.
pub struct TrieRefNodeChar<V: DictionaryValue, R: TrieRefLike<V> = TrieRefOwned<V>> {
    r: R,
    _v: PhantomData<fn() -> V>,
}

impl<V: DictionaryValue, R: TrieRefLike<V>> TrieRefNodeChar<V, R> {
    /// Wrap a TrieRef handle as a character-level dictionary node.
    #[inline]
    pub fn new(r: R) -> Self {
        Self { r, _v: PhantomData }
    }

    /// Borrow the underlying TrieRef handle.
    #[inline]
    pub fn trie_ref(&self) -> &R {
        &self.r
    }
}

impl<V: DictionaryValue, R: TrieRefLike<V>> Clone for TrieRefNodeChar<V, R> {
    #[inline]
    fn clone(&self) -> Self {
        Self {
            r: self.r.clone(),
            _v: PhantomData,
        }
    }
}

impl<V: DictionaryValue, R: TrieRefLike<V>> DictionaryNode for TrieRefNodeChar<V, R> {
    type Unit = char;

    #[inline]
    fn is_final(&self) -> bool {
        self.r.is_val()
    }

    fn transition(&self, label: char) -> Option<Self> {
        let mut buf = [0u8; 4];
        let bytes = label.encode_utf8(&mut buf).as_bytes();
        let child = self.r.descend_bytes(bytes);
        if child.path_exists() {
            Some(Self::new(child))
        } else {
            None
        }
    }

    fn edges(&self) -> Box<dyn Iterator<Item = (char, Self)> + '_> {
        // Decode each outgoing UTF-8 sequence by walking continuation bytes
        // locally from the focus: read the focus child mask for leading bytes,
        // then for each multi-byte lead descend the partial sequence and read
        // *its* child mask for valid continuation bytes (`0b10xx_xxxx`).
        let mut char_edges: Vec<(char, R)> = Vec::new();

        for first_byte in self.r.child_mask().iter() {
            let seq_len = match utf8_sequence_len(first_byte) {
                Some(len) => len,
                None => continue, // stray continuation/invalid lead byte
            };

            // Grow complete UTF-8 byte sequences one continuation byte at a time.
            let mut partials: Vec<Vec<u8>> = vec![vec![first_byte]];
            for _ in 1..seq_len {
                let mut next_partials = Vec::new();
                for partial in &partials {
                    let node = self.r.descend_bytes(partial);
                    if !node.path_exists() {
                        continue;
                    }
                    for cont_byte in node.child_mask().iter() {
                        if (cont_byte & 0b1100_0000) == 0b1000_0000 {
                            let mut extended = Vec::with_capacity(partial.len() + 1);
                            extended.extend_from_slice(partial);
                            extended.push(cont_byte);
                            next_partials.push(extended);
                        }
                    }
                }
                partials = next_partials;
            }

            for utf8_bytes in partials {
                if utf8_bytes.len() != seq_len {
                    continue;
                }
                if let Ok(s) = std::str::from_utf8(&utf8_bytes) {
                    let mut chars = s.chars();
                    if let (Some(c), None) = (chars.next(), chars.next()) {
                        char_edges.push((c, self.r.descend_bytes(&utf8_bytes)));
                    }
                }
            }
        }

        Box::new(
            char_edges
                .into_iter()
                .map(|(c, child)| (c, Self::new(child))),
        )
    }

    #[inline]
    fn edge_count(&self) -> Option<usize> {
        // Character count requires UTF-8 decoding, so reuse `edges()`.
        Some(self.edges().count())
    }
}

impl<V: DictionaryValue, R: TrieRefLike<V>> MappedDictionaryNode for TrieRefNodeChar<V, R> {
    type Value = V;

    #[inline]
    fn value(&self) -> Option<Self::Value> {
        self.r.val_cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pathmap::PathMap;

    /// Build a `PathMap<u32>` from `(term, value)` pairs.
    fn map_of(pairs: &[(&str, u32)]) -> PathMap<u32> {
        let mut map = PathMap::new();
        for (term, value) in pairs {
            map.insert(term.as_bytes(), *value);
        }
        map
    }

    #[test]
    fn owned_root_descends_and_reports_finality() {
        let map = map_of(&[("test", 1), ("testing", 2)]);
        let node = TrieRefNode::<u32>::new(trie_ref_root(map));

        // Descend t -> e -> s -> t
        let t = node.transition(b't').expect("'t'");
        let e = t.transition(b'e').expect("'e'");
        let s = e.transition(b's').expect("'s'");
        let t2 = s.transition(b't').expect("second 't'");
        assert!(t2.is_final(), "'test' is a term");
        assert_eq!(t2.value(), Some(1));

        // 'testi' exists but is not final
        let i = t2.transition(b'i').expect("'i'");
        assert!(!i.is_final());
        assert_eq!(i.value(), None);

        // No such edge
        assert!(node.transition(b'z').is_none());
    }

    #[test]
    fn owned_edges_match_child_mask_without_revalidation() {
        let map = map_of(&[("ab", 1), ("ac", 2), ("ad", 3)]);
        let node = TrieRefNode::<u32>::new(trie_ref_root(map));
        let a = node.transition(b'a').expect("'a'");

        let mut labels: Vec<u8> = a.edges().map(|(b, _)| b).collect();
        labels.sort_unstable();
        assert_eq!(labels, vec![b'b', b'c', b'd']);
        assert_eq!(a.edge_count(), Some(3));
    }

    #[test]
    fn borrowed_root_reads_without_copying() {
        let map = map_of(&[("cat", 7), ("car", 8)]);
        let node = TrieRefNode::new(trie_ref_root_borrowed(&map));

        let c = node.transition(b'c').expect("'c'");
        let a = c.transition(b'a').expect("'a'");
        let t = a.transition(b't').expect("'t'");
        assert!(t.is_final());
        assert_eq!(t.value(), Some(7));

        let mut labels: Vec<u8> = a.edges().map(|(b, _)| b).collect();
        labels.sort_unstable();
        assert_eq!(labels, vec![b'r', b't']);
    }

    #[test]
    fn empty_map_root_has_no_children_and_is_not_final() {
        let map: PathMap<u32> = PathMap::new();
        let node = TrieRefNode::<u32>::new(trie_ref_root(map));
        assert!(!node.is_final());
        assert_eq!(node.edge_count(), Some(0));
        assert_eq!(node.edges().count(), 0);
        assert!(node.transition(b'a').is_none());
    }

    #[test]
    fn root_value_and_empty_string_term() {
        // An empty-string key stores a value at the trie root.
        let mut map: PathMap<u32> = PathMap::new();
        map.insert(b"", 99);
        let node = TrieRefNode::<u32>::new(trie_ref_root(map));
        assert!(node.is_final(), "empty-string term makes the root final");
        assert_eq!(node.value(), Some(99));
    }

    #[test]
    fn descending_a_long_dangling_path_does_not_panic() {
        // > MAX_NODE_KEY_BYTES (48) dangling remainder => invalid sentinel ref;
        // all queries return false/empty/None, never a panic.
        let map = map_of(&[("hello", 1)]);
        let root = trie_ref_root(map);
        let dangling = vec![b'z'; 80];
        let node = TrieRefNode::<u32>::new(root.descend_bytes(&dangling));
        // `TrieRefOwned` implements both `Zipper` and `TrieRefLike`; qualify.
        assert!(!TrieRefLike::path_exists(&node.r));
        assert!(!node.is_final());
        assert_eq!(node.value(), None);
        assert_eq!(node.edge_count(), Some(0));
        assert_eq!(node.edges().count(), 0);

        // A short (<= 48) dangling remainder also reports a non-existent path.
        let short = trie_ref_root(map_of(&[("hello", 1)])).descend_bytes(b"hel_no");
        assert!(!TrieRefLike::path_exists(&short));
        assert!(!TrieRefLike::is_val(&short));
    }

    #[test]
    fn char_node_traverses_unicode() {
        let map = map_of(&[("café", 1), ("car", 2), ("中文", 3), ("🎉", 4)]);
        let node = TrieRefNodeChar::<u32>::new(trie_ref_root(map));

        // café (the 'é' is two UTF-8 bytes)
        let c = node.transition('c').expect("'c'");
        let a = c.transition('a').expect("'a'");
        let f = a.transition('f').expect("'f'");
        let e = f.transition('é').expect("'é'");
        assert!(e.is_final());
        assert_eq!(e.value(), Some(1));
        assert!(!f.is_final());

        // CJK (3-byte) and emoji (4-byte) leading characters as edges of root
        let zhong = node.transition('中').expect("'中'");
        let wen = zhong.transition('文').expect("'文'");
        assert!(wen.is_final());
        assert_eq!(wen.value(), Some(3));

        let party = node.transition('🎉').expect("'🎉'");
        assert!(party.is_final());
        assert_eq!(party.value(), Some(4));
    }

    #[test]
    fn char_node_edges_decode_mixed_widths() {
        let map = map_of(&[("café", 1), ("car", 2), ("cart", 3)]);
        let node = TrieRefNodeChar::<u32>::new(trie_ref_root(map));
        let c = node.transition('c').expect("'c'");
        let a = c.transition('a').expect("'a'");

        let labels: Vec<char> = a.edges().map(|(ch, _)| ch).collect();
        assert!(labels.contains(&'f'), "café branch");
        assert!(labels.contains(&'r'), "car branch");
        // 'a' has exactly two char-children even though 'f' precedes a 2-byte 'é'.
        assert_eq!(labels.len(), 2);
        assert_eq!(a.edge_count(), Some(2));
    }

    #[test]
    fn char_node_borrowed_variant() {
        let map = map_of(&[("中文", 3)]);
        let node = TrieRefNodeChar::new(trie_ref_root_borrowed(&map));
        let zhong = node.transition('中').expect("'中'");
        assert!(!zhong.is_final());
        let wen = zhong.transition('文').expect("'文'");
        assert!(wen.is_final());
        assert_eq!(wen.value(), Some(3));
    }
}
