//! Epoch-deferred reclamation for the durable char trie's evicted subtrees.
//!
//! A `DictionaryNode` walk over a [`SharedCharARTrie`](super::SharedCharARTrie)
//! hands out `Send + Sync + 'static` node handles holding raw pointers into
//! trie-owned `CharTrieNodeInner` boxes, and those handles escape the `root()`
//! read guard. The only operation that frees such a box concurrently with a live
//! walk is eviction. To make eviction non-blocking AND safe, it does not free
//! inline; instead it:
//!
//! 1. `unswizzle`s the parent slot — UNLINKING the (possibly non-leaf) evicted
//!    subtree from the in-memory tree, so no NEW reader can reach it (a new reader
//!    re-faults a fresh box from disk);
//! 2. RETIRES the subtree root here;
//! 3. frees the retired subtrees only after a successful epoch quiescence drain
//!    (`active_readers == 0` observed after the unlink), or inline when no reader
//!    is active at all.
//!
//! Because eviction targets non-leaf nodes, an evicted node may have resident
//! descendants; retiring the subtree ROOT (not each box) and freeing it via the
//! existing recursive [`Drop`](super::types::CharTrieNodeInner) once it is private
//! frees the whole subtree exactly once. A reader that descended into the subtree
//! before the unlink holds the epoch pin (`active_readers > 0`), so reclamation
//! waits for it to drain — covering descendants as well as the root.

use super::types::CharTrieNodeInner;
use crate::DictionaryValue;
use parking_lot::Mutex;

/// A retired (unlinked, not-yet-freed) subtree root.
struct Retired<V: DictionaryValue>(*mut CharTrieNodeInner<V>);

// Safety: a retired pointer was unlinked from the tree (its parent slot was
// `unswizzle`d to an on-disk reference) before being placed here, so it is
// reachable ONLY through this list — never aliased by the live tree or another
// thread except via this list. Transferring ownership between the eviction thread
// (retiring) and a reclaimer (freeing) is therefore sound.
unsafe impl<V: DictionaryValue> Send for Retired<V> {}

/// Epoch-deferred retire list for evicted char subtrees. One per trie.
pub(crate) struct CharRetireList<V: DictionaryValue> {
    pending: Mutex<Vec<Retired<V>>>,
}

impl<V: DictionaryValue> CharRetireList<V> {
    pub(crate) fn new() -> Self {
        Self {
            pending: Mutex::new(Vec::new()),
        }
    }

    /// Retire an unlinked subtree root. The caller MUST have already unlinked
    /// `root` from the tree (its parent slot is now an on-disk reference), so no
    /// new reader can reach it.
    pub(crate) fn retire(&self, root: *mut CharTrieNodeInner<V>) {
        self.pending.lock().push(Retired(root));
    }

    /// Number of subtrees currently awaiting reclamation (observability/tests).
    pub(crate) fn pending_len(&self) -> usize {
        self.pending.lock().len()
    }

    /// Free every currently-retired subtree.
    ///
    /// # Safety
    ///
    /// The caller MUST guarantee that no live reader holds a pointer into any
    /// retired subtree — i.e. this is called only after a successful epoch
    /// quiescence drain (`active_readers == 0` observed AFTER the unlink, with the
    /// `SeqCst` ordering of [`EpochManager`](crate::persistent_artrie_core::concurrency::EpochManager)),
    /// or when the trie is being dropped (no handles remain). The recursive `Drop`
    /// on each boxed root frees the whole subtree.
    pub(crate) unsafe fn reclaim_all(&self) {
        // Take the batch out under the lock, then free OUTSIDE the lock so the
        // (potentially O(subtree)) recursive frees do not hold the retire-list
        // mutex.
        let drained = std::mem::take(&mut *self.pending.lock());
        for Retired(ptr) in drained {
            // SAFETY: `ptr` came from `Box::into_raw` during insertion, is unlinked
            // (sole owner is this list), and the caller guarantees no live reader
            // holds it. Re-box + drop runs the recursive subtree free exactly once.
            drop(unsafe { Box::from_raw(ptr) });
        }
    }
}

impl<V: DictionaryValue> Drop for CharRetireList<V> {
    fn drop(&mut self) {
        // The trie — and every `DictionaryNode` handle, each of which keeps the
        // trie alive via its `CharWalkGuard` keep-alive Arc — is gone by the time
        // this field drops, so no reader can hold a pointer into a retired subtree.
        // SAFETY: no live readers remain at trie drop (see above).
        unsafe { self.reclaim_all() };
    }
}
