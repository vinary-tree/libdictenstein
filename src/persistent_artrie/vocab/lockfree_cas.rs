//! Lock-free CAS path helpers for `PersistentVocabARTrie` — the OVERLAY write primitives (V6).
//!
//! `install_overlay` is now `pub(crate)` (the overlay-install primitive, called by the ctors via
//! `install_overlay_on_create`) +
//! the owned `insert_cas`/`is_lockfree_enabled`/`merge_lockfree_to_persistent` are deleted. The
//! immutable-trie CAS walk (`try_insert_lockfree_path` / iterative path copy /
//! `create_lockfree_path` / `find_in_lockfree_trie`) is RETAINED — it is the structural-sharing
//! insert/lookup the overlay write path (`overlay_write_mode`) builds on.

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

/// Build an absent valued suffix bottom-up. This retains constant call-stack
/// space and one immutable node per input unit.
fn create_lockfree_path(chars: &[u32], index: u64) -> Arc<PersistentCharNode> {
    let mut current = Arc::new(PersistentCharNode::new().as_final().with_value(index));
    for &unit in chars.iter().rev() {
        current = Arc::new(PersistentCharNode::new().with_child(unit, Child::InMem(current)));
    }
    current
}

/// A terminal, typed outcome from building one immutable vocab insertion path.
///
/// The live vocabulary overlay has a strict never-evict invariant, so a non-null
/// on-disk child is not an ordinary cache miss: it means the representation no
/// longer matches the insertion algorithm. Keeping that outcome distinct from a
/// duplicate prevents the historical `Err(0)` sentinel from turning corruption
/// or an unsupported representation into a successful no-op for vocabulary id 0.
#[derive(Debug)]
pub(super) enum VocabPathInsertError {
    /// The complete term was already final with this stable vocabulary id.
    AlreadyExists(u64),
    /// A final vocab node violated the value=id invariant.
    FinalNodeMissingValue { depth: usize },
    /// A child slot existed but was the null filler rather than a real edge.
    NullChild { depth: usize, unit: u32 },
    /// Vocab never evicts; observing a real disk child is an invariant violation.
    UnexpectedOnDiskChild { depth: usize, unit: u32, raw: u64 },
    /// Reserving the next existing-prefix continuation frame failed.
    AllocationFailed { requested_bytes: usize },
}

impl VocabPathInsertError {
    /// Convert representation failures to a fail-closed public error. A duplicate
    /// remains a semantic outcome and is handled separately by callers.
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
            Self::AllocationFailed { requested_bytes } => {
                PersistentARTrieError::AllocationFailed { requested_bytes }
            }
        }
    }
}

/// Build an insert-once root by descending iteratively, retaining exactly one
/// `Arc` plus one `(parent, edge)` frame per existing key unit, then rebuilding
/// the copied spine bottom-up. The input root and every snapshot sharing it stay
/// unchanged. The absent suffix is also built bottom-up.
fn build_lockfree_insert_path(
    root: &Arc<PersistentCharNode>,
    chars: &[u32],
    index: u64,
) -> std::result::Result<Arc<PersistentCharNode>, VocabPathInsertError> {
    // Grow only for existing-prefix edges actually traversed. An early divergence,
    // duplicate, or malformed child therefore does not reserve proportional to the
    // unvisited suffix. Growth is fallible before ownership is moved into the frame.
    let mut spine: Vec<(Arc<PersistentCharNode>, u32)> = Vec::new();
    let mut current = Arc::clone(root);
    let mut depth = 0usize;

    loop {
        if depth == chars.len() {
            if current.is_final() {
                return Err(match current.get_value() {
                    Some(existing) => VocabPathInsertError::AlreadyExists(existing),
                    None => VocabPathInsertError::FinalNodeMissingValue { depth },
                });
            }

            let mut rebuilt = Arc::new(current.as_final().with_value(index));
            for (parent, unit) in spine.into_iter().rev() {
                rebuilt = Arc::new(parent.with_child(unit, Child::InMem(rebuilt)));
            }
            return Ok(rebuilt);
        }

        let unit = chars[depth];
        match current.find_child(unit) {
            Some(child) => {
                if let Some(child_arc) = child.as_in_mem() {
                    let next = Arc::clone(child_arc);
                    let requested_bytes = spine
                        .len()
                        .saturating_add(1)
                        .saturating_mul(std::mem::size_of::<(Arc<PersistentCharNode>, u32)>());
                    spine
                        .try_reserve(1)
                        .map_err(|_| VocabPathInsertError::AllocationFailed { requested_bytes })?;
                    spine.push((current, unit));
                    current = next;
                    depth += 1;
                    continue;
                }

                let disk = child
                    .as_on_disk()
                    .expect("Child has exactly one InMem or OnDisk representation");
                if disk.is_null() {
                    return Err(VocabPathInsertError::NullChild { depth, unit });
                }
                return Err(VocabPathInsertError::UnexpectedOnDiskChild {
                    depth,
                    unit,
                    raw: disk.to_raw(),
                });
            }
            None => {
                let mut rebuilt = create_lockfree_path(&chars[depth + 1..], index);
                rebuilt = Arc::new(current.with_child(unit, Child::InMem(rebuilt)));
                for (parent, parent_unit) in spine.into_iter().rev() {
                    rebuilt = Arc::new(parent.with_child(parent_unit, Child::InMem(rebuilt)));
                }
                return Ok(rebuilt);
            }
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
    /// This is an immutable, iterative path copy: no input node is mutated and
    /// call-stack usage is constant in the term length. Representation failures
    /// are kept distinct from `AlreadyExists` and must fail closed at the public
    /// mutation boundary.
    pub(super) fn try_insert_lockfree_path(
        &self,
        root: &Arc<PersistentCharNode>,
        chars: &[u32],
        index: u64,
    ) -> std::result::Result<Arc<PersistentCharNode>, VocabPathInsertError> {
        build_lockfree_insert_path(root, chars, index)
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
    use super::*;
    use crate::persistent_artrie::core::swizzled_ptr::{NodeType, SwizzledPtr};

    fn value_at(root: &Arc<PersistentCharNode>, key: &[u32]) -> Option<u64> {
        let mut current = Arc::clone(root);
        for &unit in key {
            let child = current.find_child(unit)?;
            current = Arc::clone(child.as_in_mem()?);
        }
        current.is_final().then(|| current.get_value()).flatten()
    }

    #[test]
    fn iterative_insert_preserves_every_captured_root() {
        let empty = Arc::new(PersistentCharNode::new());
        let ab = ['a' as u32, 'b' as u32];
        let ac = ['a' as u32, 'c' as u32];

        let first = build_lockfree_insert_path(&empty, &ab, 7).expect("insert ab");
        let second = build_lockfree_insert_path(&first, &ac, 11).expect("insert ac");

        assert_eq!(value_at(&empty, &ab), None, "empty snapshot was mutated");
        assert_eq!(value_at(&first, &ab), Some(7));
        assert_eq!(value_at(&first, &ac), None, "first snapshot was mutated");
        assert_eq!(value_at(&second, &ab), Some(7));
        assert_eq!(value_at(&second, &ac), Some(11));
    }

    #[test]
    fn duplicate_zero_id_is_typed_and_not_a_failure_sentinel() {
        let empty = Arc::new(PersistentCharNode::new());
        let key = ['z' as u32];
        let root = build_lockfree_insert_path(&empty, &key, 0).expect("insert id zero");

        match build_lockfree_insert_path(&root, &key, 99) {
            Err(VocabPathInsertError::AlreadyExists(0)) => {}
            other => panic!("expected typed duplicate id zero, got {other:?}"),
        }
        assert_eq!(value_at(&root, &key), Some(0));
    }

    #[test]
    fn final_node_without_vocab_id_fails_closed() {
        let malformed = Arc::new(PersistentCharNode::new().as_final());
        assert!(matches!(
            build_lockfree_insert_path(&malformed, &[], 3),
            Err(VocabPathInsertError::FinalNodeMissingValue { depth: 0 })
        ));
    }

    #[test]
    fn null_and_on_disk_children_are_not_reported_as_duplicates() {
        let unit = 'q' as u32;
        let null_root = Arc::new(
            PersistentCharNode::new().with_child(unit, Child::OnDisk(SwizzledPtr::null())),
        );
        assert!(matches!(
            build_lockfree_insert_path(&null_root, &[unit], 1),
            Err(VocabPathInsertError::NullChild {
                depth: 0,
                unit: found
            }) if found == unit
        ));

        let disk_ptr = SwizzledPtr::on_disk(7, 9, NodeType::CharNode4);
        let raw = disk_ptr.to_raw();
        let disk_root =
            Arc::new(PersistentCharNode::new().with_child(unit, Child::OnDisk(disk_ptr)));
        assert!(matches!(
            build_lockfree_insert_path(&disk_root, &[unit], 1),
            Err(VocabPathInsertError::UnexpectedOnDiskChild {
                depth: 0,
                unit: found,
                raw: found_raw
            }) if found == unit && found_raw == raw
        ));
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "large constrained-stack stress is covered outside Miri"
    )]
    fn iterative_insert_and_reclamation_fit_a_constrained_stack() {
        // A recursive descent of this depth cannot fit in 64 KiB. Both the
        // insertion path-copy and OverlayNode reclamation use heap worklists.
        std::thread::Builder::new()
            .name("vocab-long-key-stack-gate".to_string())
            .stack_size(64 * 1024)
            .spawn(|| {
                let key = vec!['x' as u32; 16_384];
                let empty = Arc::new(PersistentCharNode::new());
                let inserted =
                    build_lockfree_insert_path(&empty, &key, 41).expect("long iterative insert");
                assert_eq!(value_at(&inserted, &key), Some(41));
                // `inserted` and `empty` are deliberately dropped on this
                // constrained stack to cover iterative reclamation too.
            })
            .expect("spawn constrained-stack thread")
            .join()
            .expect("iterative vocab insertion must not overflow its stack");
    }
}
