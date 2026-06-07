//! Char `OverlayFaulter` impls — the SAFE fault-in capability the overlay-backed
//! `DictionaryNode` uses to resolve `Child::OnDisk` overlay children during a graph
//! walk. F7 BLOCKER-1.
//!
//! Two impls:
//! - directly on `PersistentARTrieChar<V, S>` (delegating to the existing
//!   `load_overlay_node_from_disk`), and
//! - on a `SharedOverlayFaulter<V, S>` newtype wrapping the shared
//!   `Arc<RwLock<PersistentARTrieChar<V, S>>>` form, which read-locks and delegates
//!   — this is the handle the eviction-capable `SharedCharARTrie::root` attaches so
//!   a faulted OnDisk child can be loaded while the walk is in flight.
//!
//! ZERO new `unsafe`: both delegate to the existing safe `&self`
//! `load_overlay_node_from_disk`. The shared wrapper takes a fresh *read* lock per
//! fault — the walk itself holds NO lock (its `root()` read guard is dropped after
//! the root node is built; the owned `Arc` snapshots keep the in-memory subtree
//! alive on their own), so the fresh read lock excludes no concurrent reader and
//! cannot self-deadlock (parking_lot read locks are shared).

use std::sync::Arc;

use parking_lot::RwLock;

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;
use crate::persistent_artrie_core::key_encoding::CharKey;
use crate::persistent_artrie_core::overlay::{OverlayFaulter, OverlayNode};
use crate::value::DictionaryValue;

use super::PersistentARTrieChar;

/// Direct impl: an owned `&PersistentARTrieChar` can fault an overlay slot via its
/// existing `load_overlay_node_from_disk`. An I/O / decode error degrades to `None`
/// (no child) — never UB, never a fabricated term.
impl<V: DictionaryValue, S: BlockStorage> OverlayFaulter<CharKey, V>
    for PersistentARTrieChar<V, S>
{
    #[inline]
    fn fault_overlay_slot(&self, slot: &SwizzledPtr) -> Option<Arc<OverlayNode<CharKey, V>>> {
        self.load_overlay_node_from_disk(slot).ok()
    }
}

/// Fault-in handle for the SHARED (`Arc<RwLock<..>>`) char trie. Holds an owned
/// `Arc` clone of the trie (keeping its allocation — and its buffer/arena managers
/// — alive for the whole walk) and read-locks on each fault to delegate. This is
/// the handle the eviction-capable [`SharedCharARTrie::root`](super::SharedCharARTrie)
/// attaches; an owned (`&self`) `root()` uses no faulter (eviction, hence an OnDisk
/// overlay child, is impossible on a non-shared trie).
pub(crate) struct SharedOverlayFaulter<V: DictionaryValue, S: BlockStorage> {
    trie: Arc<RwLock<PersistentARTrieChar<V, S>>>,
}

impl<V: DictionaryValue, S: BlockStorage> SharedOverlayFaulter<V, S> {
    /// Wrap a shared char trie as an overlay faulter.
    pub(crate) fn new(trie: Arc<RwLock<PersistentARTrieChar<V, S>>>) -> Self {
        Self { trie }
    }
}

impl<V: DictionaryValue, S: BlockStorage> OverlayFaulter<CharKey, V>
    for SharedOverlayFaulter<V, S>
{
    #[inline]
    fn fault_overlay_slot(&self, slot: &SwizzledPtr) -> Option<Arc<OverlayNode<CharKey, V>>> {
        // Fresh read lock per fault; the walk holds no lock, so this excludes no
        // concurrent reader and never self-deadlocks (shared read lock).
        let guard = self.trie.read();
        guard.load_overlay_node_from_disk(slot).ok()
    }
}
