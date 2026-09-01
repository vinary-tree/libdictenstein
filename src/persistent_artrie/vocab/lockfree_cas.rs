//! Lock-free CAS path helpers for `PersistentVocabARTrie` — the OVERLAY write primitives (V6).
//!
//! `install_overlay` is now `pub(crate)` (the overlay-install primitive, called by the ctors via
//! `install_overlay_on_create`) +
//! the owned `insert_cas`/`is_lockfree_enabled`/`merge_lockfree_to_persistent` are deleted. The
//! immutable-trie CAS walk (`try_insert_lockfree_path` / `create_lockfree_path` /
//! `find_in_lockfree_trie`) is RETAINED — it is the structural-sharing insert/lookup the overlay
//! write path (`overlay_write_mode`) builds on. Path copying uses an explicit heap spine and a
//! reverse unwind, so native stack use is independent of term depth.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use dashmap::DashMap;

use crate::persistent_artrie::char::nodes::persistent_node::Child as ChildGeneric;
use crate::persistent_artrie::char::nodes::{
    AtomicNodePtr as AtomicNodePtrGeneric, PersistentCharNode as PersistentCharNodeGeneric,
};
use crate::persistent_artrie::error::PersistentARTrieError;

// Vocab overlay node value = `u64` vocabulary index (G1: char overlay node is now
// generic; the vocab instantiates it at `V = u64`).
type Child = ChildGeneric<u64>;
type AtomicNodePtr = AtomicNodePtrGeneric<u64>;
type PersistentCharNode = PersistentCharNodeGeneric<u64>;

/// Failure to construct one immutable vocabulary path-copy candidate.
///
/// A duplicate is the only normal insert-once refusal. Representation failures
/// remain distinct from duplicates and from allocation failures, preventing a
/// malformed path from masquerading as the stable vocabulary id `0`.
#[derive(Debug)]
pub(super) enum VocabPathInsertError {
    AlreadyExists(u64),
    FinalNodeMissingValue { depth: usize },
    NullChild { depth: usize, unit: u32 },
    UnexpectedOnDiskChild { depth: usize, unit: u32, raw: u64 },
    Failure(PersistentARTrieError),
}

impl VocabPathInsertError {
    pub(super) fn into_persistent_error(self) -> PersistentARTrieError {
        match self {
            Self::AlreadyExists(index) => PersistentARTrieError::internal(format!(
                "duplicate vocab path with index {index} reached an error-only conversion"
            )),
            Self::FinalNodeMissingValue { depth } => PersistentARTrieError::internal(format!(
                "vocab overlay invariant violation: final node at depth {depth} has no value"
            )),
            Self::NullChild { depth, unit } => PersistentARTrieError::internal(format!(
                "vocab overlay invariant violation: null child for unit U+{unit:04X} at depth {depth}"
            )),
            Self::UnexpectedOnDiskChild { depth, unit, raw } => {
                PersistentARTrieError::internal(format!(
                    "vocab overlay invariant violation: on-disk child {raw:#x} for unit U+{unit:04X} at depth {depth}; vocab overlays must remain resident"
                ))
            }
            Self::Failure(error) => error,
        }
    }
}

impl<S: crate::persistent_artrie::block_storage::BlockStorage>
    super::dict_impl::PersistentVocabARTrie<S>
{
    /// Install the lock-free overlay infrastructure (root + cache + reverse map).
    ///
    /// `pub(crate)`: the ONLY caller is the ctor seam (`install_overlay_on_create` →
    /// `LockFreeOverlay::install_overlay`), which every production ctor runs at construction.
    /// Returns `true` if newly installed, `false` if already installed.
    pub(crate) fn install_overlay(&mut self) -> bool {
        if self.lockfree_root.is_some() {
            return false;
        }

        let root = Arc::new(PersistentCharNode::new());
        self.lockfree_root = Some(AtomicNodePtr::new(root));
        self.lockfree_cache = Some(DashMap::new());
        // The overlay's reverse map (id -> term) -- the non-blocking inverse used by `get_term`.
        // Populated by `insert_overlay`; rebuilt from the image on reopen.
        self.reverse_term_map = Some(DashMap::new());

        true
    }

    /// Try to create a new root with the term inserted (lock-free version).
    ///
    /// Returns a new immutable root on success. Descent records a root-to-leaf
    /// spine on the heap and reverse-unwinds it, so the method is stack-safe for
    /// arbitrarily deep terms representable by the input slice.
    pub(super) fn try_insert_lockfree_path(
        &self,
        root: &Arc<PersistentCharNode>,
        chars: &[u32],
        index: u64,
    ) -> std::result::Result<Arc<PersistentCharNode>, VocabPathInsertError> {
        use crate::persistent_artrie::core::key_encoding::CharKey;
        use crate::persistent_artrie::core::overlay::cas_walk::{
            resolve_or_fault, unwind_spine, ChildResolution, FaultMode,
        };
        use crate::persistent_artrie::core::overlay::{
            try_push_overlay_path_spine, OverlayNodeHandle, OverlayPathFrame, OverlayPathSpine,
        };

        if chars.is_empty() {
            // Empty term - mark root as final
            if root.is_final() {
                return Err(match root.get_value() {
                    Some(existing) => VocabPathInsertError::AlreadyExists(existing),
                    None => VocabPathInsertError::FinalNodeMissingValue { depth: 0 },
                });
            }
            let new_root = root.as_final().with_value(index);
            return Ok(Arc::new(new_root));
        }

        let mut spine = OverlayPathSpine::<CharKey, u64>::new();
        let mut current = OverlayNodeHandle::Borrowed(root);
        let mut cursor = 0;

        loop {
            if cursor == chars.len() {
                if current.node().is_final() {
                    return Err(match current.node().get_value() {
                        Some(existing) => VocabPathInsertError::AlreadyExists(existing),
                        None => VocabPathInsertError::FinalNodeMissingValue { depth: cursor },
                    });
                }
                let leaf = Arc::new(current.node().as_final().with_value(index));
                return Ok(unwind_spine(spine, leaf));
            }

            let unit = chars[cursor];
            let child = match resolve_or_fault(
                &current,
                unit,
                FaultMode::NoFaultIn,
                |_| -> crate::persistent_artrie::core::error::Result<_> {
                    unreachable!("vocabulary insertion never faults an on-disk edge")
                },
            ) {
                ChildResolution::InMem(child) => child,
                ChildResolution::Faulted(_) | ChildResolution::FaultFailed(_) => {
                    unreachable!("no-fault vocabulary resolution cannot fault")
                }
                ChildResolution::Null => {
                    let on_disk = current
                        .node()
                        .find_child(unit)
                        .and_then(|child| child.as_on_disk());
                    return Err(match on_disk {
                        Some(pointer) if !pointer.is_null() => {
                            VocabPathInsertError::UnexpectedOnDiskChild {
                                depth: cursor,
                                unit,
                                raw: pointer.to_raw(),
                            }
                        }
                        _ => VocabPathInsertError::NullChild {
                            depth: cursor,
                            unit,
                        },
                    });
                }
                ChildResolution::Absent => {
                    let new_child = self.create_lockfree_path(&chars[cursor + 1..], index);
                    let branch = Arc::new(current.node().with_child(unit, Child::InMem(new_child)));
                    return Ok(unwind_spine(spine, branch));
                }
            };
            try_push_overlay_path_spine(
                &mut spine,
                OverlayPathFrame {
                    node: current,
                    unit,
                },
                chars.len(),
            )
            .map_err(|source| {
                VocabPathInsertError::Failure(PersistentARTrieError::allocation_failed(
                    "vocabulary overlay insert path spine",
                    chars.len(),
                    source,
                ))
            })?;
            current = child;
            cursor += 1;
        }
    }

    /// Create a new path from the remaining characters (lock-free version).
    fn create_lockfree_path(&self, chars: &[u32], index: u64) -> Arc<PersistentCharNode> {
        if chars.is_empty() {
            // Create final node with value
            let node = PersistentCharNode::new().as_final().with_value(index);
            return Arc::new(node);
        }

        // Build path bottom-up
        let mut current = Arc::new(PersistentCharNode::new().as_final().with_value(index));

        for &c in chars.iter().rev() {
            // Each parent owns its child by `Arc` (no raw-pointer smuggling).
            let parent = PersistentCharNode::new().with_child(c, Child::InMem(current));
            current = Arc::new(parent);
        }

        current
    }

    /// Find a term in the lock-free trie, returning its index if found.
    pub(super) fn find_in_lockfree_trie(
        &self,
        root: &Arc<PersistentCharNode>,
        chars: &[u32],
    ) -> Option<u64> {
        let mut current = root.clone();

        for &c in chars {
            {
                let child = current.find_child(c)?;
                if child.is_null() {
                    return None;
                }
                // On-disk children are not traversable here → `None`.
                {
                    let child_arc = child.as_in_mem()?;
                    current = Arc::clone(child_arc)
                }
            }
        }

        current.get_value()
    }

    /// Get CAS retry statistics for monitoring lock contention.
    #[inline]
    pub fn cas_retries(&self) -> u64 {
        self.cas_retries.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::super::dict_impl::PersistentVocabARTrie;
    use super::VocabPathInsertError;
    use crate::persistent_artrie::char::nodes::persistent_node::{Child, PersistentCharNode};
    use std::sync::Arc;

    #[test]
    fn one_hundred_thousand_deep_insert_and_drop_are_stack_safe() {
        const DEPTH: usize = 100_000;

        std::fs::create_dir_all("target/test-tmp").expect("create real-disk test scratch root");
        let directory = tempfile::Builder::new()
            .prefix("vocab-overlay-path-machine-deep")
            .tempdir_in("target/test-tmp")
            .expect("scratch tempdir under target/test-tmp");
        let path = directory.path().join("deep.vocab");
        let trie = PersistentVocabARTrie::create(&path).expect("create vocabulary trie");
        let root_revision = trie
            .lockfree_root
            .as_ref()
            .expect("lock-free overlay installed")
            .load_revision()
            .expect("published root revision");
        let root = Arc::clone(root_revision.node());
        let units = vec![u32::from('x'); DEPTH];

        let inserted = trie
            .try_insert_lockfree_path(&root, &units, 17)
            .unwrap_or_else(|failure| panic!("deep vocabulary path failed: {failure:?}"));

        assert_eq!(trie.find_in_lockfree_trie(&inserted, &units), Some(17));
        assert!(super::super::dict_impl::overlay_subtree_all_in_mem(
            &inserted
        ));
        assert!(
            trie.lockfree_root
                .as_ref()
                .expect("lock-free overlay installed")
                .compare_exchange_revision_counted(&root_revision, Arc::clone(&inserted), 1)
                .is_ok(),
            "publish deep vocabulary root"
        );
        let terms = trie.iter_terms().collect::<Vec<_>>();
        assert_eq!(terms.len(), 1);
        assert_eq!(terms[0].chars().count(), DEPTH);
        drop(inserted);
        drop(root);
        drop(trie);
    }

    #[test]
    fn vocabulary_insert_spill_failure_is_typed_and_non_mutating() {
        use crate::persistent_artrie::core::overlay::{
            overlay_spine_failpoint, INLINE_OVERLAY_DEPTH,
        };

        std::fs::create_dir_all("target/test-tmp").expect("create test scratch root");
        let directory = tempfile::Builder::new()
            .prefix("vocab-overlay-spill-failure")
            .tempdir_in("target/test-tmp")
            .expect("scratch tempdir");
        let path = directory.path().join("spill.vocab");
        let trie = PersistentVocabARTrie::create(&path).expect("create vocabulary trie");
        let units = vec![u32::from('x'); INLINE_OVERLAY_DEPTH + 1];
        let mut root = Arc::new(PersistentCharNode::<u64>::new().as_final().with_value(3));
        for &unit in units.iter().rev() {
            root = Arc::new(PersistentCharNode::<u64>::new().with_child(unit, Child::InMem(root)));
        }
        let root_before = Arc::clone(&root);
        let _failpoint = overlay_spine_failpoint::fail_next_spill();

        let result = trie.try_insert_lockfree_path(&root, &units, 4);

        assert!(matches!(
            result,
            Err(VocabPathInsertError::Failure(
                crate::persistent_artrie::error::PersistentARTrieError::AllocationFailed { .. }
            ))
        ));
        assert!(Arc::ptr_eq(&root, &root_before));
    }
}
