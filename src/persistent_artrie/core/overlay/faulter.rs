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
//! through the generation-qualified compact-batch driver. Byte and character
//! variants both have production eviction and read/write fault-in paths. The
//! faulter is supplied where eviction is possible (the `Shared*ARTrie` walks, which hold
//! the trie behind an `Arc<..>` and can call the faulting loader); the inherent
//! `root(&self)` walks pass `None` (eviction is impossible on an owned trie, so
//! no OnDisk child can appear). A
//! `None` faulter degrades an (unreachable) OnDisk slot to "no transition" — never
//! a fabricated term, never UB — exactly as the production point-read degrades when
//! fault-in is unavailable.

use std::sync::Arc;

use crate::persistent_artrie::core::error::Result;
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
    /// located non-null `Child::OnDisk` slot. The exact typed failure is retained
    /// so durable mutations can distinguish storage failure from proven absence.
    /// The returned node is a fresh owned overlay `Arc` whose children remain
    /// `Child::OnDisk` (single-level / lazy fault granularity). The fault writes
    /// nothing to disk and advances no watermark.
    fn try_fault_overlay_slot(&self, slot: &SwizzledPtr) -> Result<Arc<OverlayNode<K, V>>>;

    /// Best-effort adapter for infallible read/traversal interfaces whose contract
    /// explicitly permits an unavailable child to appear absent. Durable mutation
    /// and any API that promises exact error reporting must call
    /// [`Self::try_fault_overlay_slot`] instead.
    #[inline]
    fn fault_overlay_slot(&self, slot: &SwizzledPtr) -> Option<Arc<OverlayNode<K, V>>> {
        self.try_fault_overlay_slot(slot).ok()
    }
}
