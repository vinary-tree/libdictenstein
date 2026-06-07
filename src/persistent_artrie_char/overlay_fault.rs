//! Char `OverlayFaulter` impls — the SAFE fault-in capability the overlay-backed
//! `DictionaryNode` uses to resolve `Child::OnDisk` overlay children during a graph
//! walk. F7 BLOCKER-1.
//!
//! Two impls:
//! - directly on `PersistentARTrieChar<V, S>` (delegating to the existing
//!   `load_overlay_node_from_disk`), and
//! - on a `SharedOverlayFaulter<V, S>` newtype wrapping the shared
//!   `Arc<PersistentARTrieChar<V, S>>` handle (F4: the outer `RwLock` is gone),
//!   which delegates directly — this is the handle the eviction-capable
//!   `SharedCharARTrie::root` attaches so a faulted OnDisk child can be loaded
//!   while the walk is in flight.
//!
//! ZERO new `unsafe`: both delegate to the existing safe `&self`
//! `load_overlay_node_from_disk`. **F4:** the shared wrapper no longer takes any
//! lock per fault — the handle is a bare `Arc<…>`, `load_overlay_node_from_disk`
//! is `&self`, and faulting is lock-free (it loads from disk into a fresh `Arc`).
//! The owned `Arc` clone keeps the trie (+ its buffer/arena managers) alive for
//! the whole walk.

use std::sync::Arc;

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

/// Fault-in handle for the SHARED (`Arc<PersistentARTrieChar<V,S>>`) char trie.
/// Holds an owned `Arc` clone of the trie (keeping its allocation — and its
/// buffer/arena managers — alive for the whole walk) and delegates each fault
/// directly (no lock — F4). This is the handle the eviction-capable
/// [`SharedCharARTrie::root`](super::SharedCharARTrie) attaches; an owned (`&self`)
/// `root()` uses no faulter (eviction, hence an OnDisk overlay child, is impossible
/// on a non-shared trie).
pub(crate) struct SharedOverlayFaulter<V: DictionaryValue, S: BlockStorage> {
    trie: Arc<PersistentARTrieChar<V, S>>,
}

impl<V: DictionaryValue, S: BlockStorage> SharedOverlayFaulter<V, S> {
    /// Wrap a shared char trie as an overlay faulter.
    pub(crate) fn new(trie: Arc<PersistentARTrieChar<V, S>>) -> Self {
        Self { trie }
    }
}

impl<V: DictionaryValue, S: BlockStorage> OverlayFaulter<CharKey, V>
    for SharedOverlayFaulter<V, S>
{
    #[inline]
    fn fault_overlay_slot(&self, slot: &SwizzledPtr) -> Option<Arc<OverlayNode<CharKey, V>>> {
        // F4: no lock — the handle is a bare `Arc`, faulting is `&self` + lock-free.
        self.trie.load_overlay_node_from_disk(slot).ok()
    }
}
