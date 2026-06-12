//! `OverlayFaulter<K, V>` — the SAFE, object-safe fault-in capability that lets an
//! overlay-backed `DictionaryNode` resolve `Child::OnDisk` overlay children during
//! a graph walk **without** naming the trie's block-storage parameter `S` and
//! **without any `unsafe`**.
//!
//! # Why this exists (and why it is `Arc<dyn ...>`, not a raw pointer)
//!
//! The overlay-backed `DictionaryNode` (byte `NodeInner::Overlay`, char
//! `PersistentARTrieCharNode`'s overlay arm) navigates the lock-free overlay by
//! holding owned `Arc<OverlayNode<K, V>>` snapshots — immutable, reference-counted,
//! so in-memory descent needs no pin and no `unsafe` (the `Arc` keeps the node and
//! its subtree alive regardless of the trie's fate).
//!
//! The one thing an owned overlay snapshot CANNOT do by itself is fault in a
//! `Child::OnDisk(SwizzledPtr)` slot — that requires the trie's buffer/arena
//! managers (`load_overlay_node_from_disk(&self, ptr)`). Rather than smuggle a raw
//! `*const dyn` into the node (as the OWNED-tree `DictionaryNode` does, guarded by
//! an epoch pin — see commit `549b068`), the overlay node carries an **owned**
//! `Arc<dyn OverlayFaulter<K, V>>`. Cloning the node clones the `Arc` (cheap); the
//! faulter (the trie) stays alive for the whole walk through this owned handle, so
//! its buffer/arena managers are valid whenever a fault-in is attempted. No raw
//! pointer, no pin, no epoch, **zero `unsafe`** — which is what keeps the strict
//! unsafe-inventory set-equality gate green (no new `unsafe` line is introduced by
//! the overlay traversal).
//!
//! # When `Child::OnDisk` overlay children actually occur
//!
//! A reader-visible OnDisk overlay child arises ONLY from overlay **eviction**
//! (`evict_overlay_nodes`, `#[cfg(feature = "bench-internals")]`/test-only). char
//! has that eviction driver + the production read/write fault-in; byte has neither
//! (its routed overlay is always fully `Child::InMem`). The faulter is supplied on
//! the paths where eviction is possible (the `Shared*ARTrie` walks, which hold the
//! trie behind an `Arc<RwLock<..>>`); the inherent `root(&self)` walks pass `None`
//! (eviction is impossible on an owned trie, so no OnDisk child can appear). A
//! `None` faulter degrades an (unreachable) OnDisk slot to "no transition" — never
//! a fabricated term, never UB — exactly as the production point-read degrades when
//! fault-in is unavailable.

use std::sync::Arc;

use crate::persistent_artrie::core::key_encoding::KeyEncoding;
use crate::persistent_artrie::core::overlay::node::OverlayNode;
use crate::persistent_artrie::core::swizzled_ptr::SwizzledPtr;

/// Object-safe (over the block-storage parameter `S`) fault-in capability for the
/// overlay-backed `DictionaryNode`.
///
/// Generic over the key encoding `K` (`ByteKey`/`CharKey`) and value `V`. Each
/// persistent ARTrie variant supplies one thin impl that delegates to its existing
/// `load_overlay_node_from_disk`. `Send + Sync` so an overlay `DictionaryNode`
/// handle (which is `Send + Sync`) can carry an `Arc<dyn OverlayFaulter<K, V>>`.
pub trait OverlayFaulter<K: KeyEncoding, V>: Send + Sync {
    /// Fault in (load + deserialize from disk) the overlay node behind an already
    /// located `Child::OnDisk` slot, returning a fresh owned overlay `Arc` whose
    /// own children stay `Child::OnDisk` (single-level / lazy — the overlay fault
    /// granularity). Returns `None` if the slot is null/unresolvable or the load
    /// fails — both degrade to "no child" (the only non-panicking mapping the
    /// infallible `DictionaryNode` API admits; a transient I/O error yields a miss,
    /// never UB). The fault writes nothing to disk and advances no watermark.
    fn fault_overlay_slot(&self, slot: &SwizzledPtr) -> Option<Arc<OverlayNode<K, V>>>;
}
