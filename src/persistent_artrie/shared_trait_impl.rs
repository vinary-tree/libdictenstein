//! `ARTrie` + `EvictableARTrie` trait implementations for `SharedARTrie<V>`.
//!
//! Split out of byte `dict_impl.rs` (lines ~5675-6074, ~400 LOC) as
//! the tenth Phase-5 byte sub-module. The trait blocks plus the
//! evict-node helpers (`evict_node_at_path` + `find_parent_mut`) move
//! here; the per-method semantics are unchanged.

use std::path::Path;
use std::sync::atomic::Ordering as AtomicOrdering;
use std::sync::Arc;

use crate::artrie_trait::{ARTrie, EvictableARTrie};
use crate::persistent_artrie_core::concurrency::EpochManager;
use crate::persistent_artrie_core::durability::DurabilityPolicy;
use crate::persistent_artrie_core::eviction::{EvictionConfig, EvictionCoordinator, EvictionStats};
// F4: the `.read()/.write()` compat shim on the collapsed `Arc<PersistentARTrie>`.
use crate::persistent_artrie_core::shared_access::SharedTrieAccess;
use crate::value::DictionaryValue;

use super::block_storage::BlockStorage;
use super::dict_impl::{PersistentARTrie, TrieRoot};
use super::error::{PersistentARTrieError, Result};
use super::recovery::RecoveryReport;
use super::swizzled_ptr::SwizzledPtr;
use super::transitions::ChildNode;
use super::SharedARTrie;

impl<V: DictionaryValue> ARTrie for SharedARTrie<V> {
    type Unit = u8;
    type Value = V;

    fn create<P: AsRef<Path>>(path: P) -> Result<Self> {
        PersistentARTrie::create(path).map(Arc::new)
    }

    fn create_with_slot_tracking<P: AsRef<Path>>(path: P) -> Result<Self> {
        PersistentARTrie::create_with_slot_tracking(path).map(Arc::new)
    }

    fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        PersistentARTrie::open(path).map(Arc::new)
    }

    fn open_with_slot_tracking<P: AsRef<Path>>(path: P) -> Result<Self> {
        PersistentARTrie::open_with_slot_tracking(path).map(Arc::new)
    }

    fn open_with_recovery<P: AsRef<Path>>(path: P) -> Result<(Self, RecoveryReport)> {
        PersistentARTrie::open_with_recovery(path).map(|(t, r)| (Arc::new(t), r))
    }

    fn open_with_recovery_and_slot_tracking<P: AsRef<Path>>(
        path: P,
    ) -> Result<(Self, RecoveryReport)> {
        let (trie, report) = PersistentARTrie::open_with_recovery(path)?;
        if let Some(ref am) = trie.arena_manager {
            am.write().enable_slot_tracking();
        }
        Ok((Arc::new(trie), report))
    }

    fn enable_slot_tracking(&self) {
        let guard = self.read();
        if let Some(ref am) = guard.arena_manager {
            am.write().enable_slot_tracking();
        }
    }

    fn flush_sequential(&self) -> Result<()> {
        let guard = self.read();
        if let Some(ref am) = guard.arena_manager {
            am.write().flush_sequential()?;
        }
        Ok(())
    }

    fn insert(&self, term: &str) -> bool
    where
        Self::Value: Default,
    {
        // M3 (C5): delegate to the routed inherent `insert` (routes to
        // `insert_cas_durable` under the flip), NOT `insert_impl` (owned-only). The
        // owned default-value insert is preserved by the routed inherent method's
        // owned arm; under the overlay the durable membership insert is value-free.
        let guard = self.write();
        if guard.route_overlay() {
            return guard.insert(term);
        }
        guard.insert_impl(term.as_bytes(), Some(V::default()))
    }

    fn insert_with_value(&self, term: &str, value: Self::Value) -> bool {
        // M3 (C5): route to the routed inherent `insert_with_value` under the flip.
        let guard = self.write();
        if guard.route_overlay() {
            return guard.insert_with_value(term, value);
        }
        guard.insert_impl(term.as_bytes(), Some(value))
    }

    fn contains(&self, term: &str) -> bool {
        // M3 (C6): delegate to the routed `contains_bytes` (this read `contains_impl`
        // directly, bypassing the overlay route).
        let guard = self.read();
        guard.contains_bytes(term.as_bytes())
    }

    fn get_value(&self, term: &str) -> Option<Self::Value> {
        // M3 (C6): delegate to the routed `get_value_bytes` (value-routes to the
        // overlay incl. the empty-term owned exception), NOT `get_value_impl`.
        let guard = self.read();
        guard.get_value_bytes(term.as_bytes())
    }

    fn remove(&self, term: &str) -> bool {
        // M3 (C5): route to the routed inherent `remove` (→ `remove_cas_durable`).
        let guard = self.write();
        if guard.route_overlay() {
            return guard.remove(term);
        }
        guard.remove_impl(term.as_bytes())
    }

    #[inline]
    fn len(&self) -> usize {
        // M3 (C6): under the overlay count resident finals (the owned `term_count` is
        // cleared on reopen); this read `term_count` directly, bypassing the route.
        let guard = self.read();
        if guard.route_overlay() {
            return guard.overlay_len();
        }
        guard.term_count.load(AtomicOrdering::Acquire)
    }

    fn checkpoint(&self) -> Result<()> {
        // **F3 / NF-3 — serialize concurrent checkpoints** (byte twin). Today byte's
        // checkpoint holds the outer `self.write()` for the whole body, so checkpoints
        // are ALREADY serialized and this lock is redundant-but-harmless; but the F4
        // `Arc<RwLock>`→`Arc` collapse drops the write lock, and this `checkpoint_lock`
        // then becomes the sole serializer (forward-correct; same lock the char arm
        // uses). Cloned out of a brief read guard so we don't hold the trie lock while
        // acquiring it. Formally verified (ConcurrentCheckpointSerialization.tla).
        let ckpt_lock = self.read().checkpoint_lock.clone();
        let _ckpt_guard = ckpt_lock.lock();
        let guard = self.write();
        guard.checkpoint()
    }

    #[inline]
    fn is_dirty(&self) -> bool {
        let guard = self.read();
        guard.dirty.load(AtomicOrdering::Acquire)
    }

    fn remove_prefix(&self, prefix: &str) -> usize {
        let prefix_bytes = prefix.as_bytes();

        // M3 (C5/H4): under the flip the owned `iter_prefix`+`remove_impl` loop would
        // enumerate the OVERLAY but delete from the EMPTY owned tree = a silent no-op.
        // Route to the routed inherent `remove_prefix_batched` (overlay remove-CAS).
        {
            let guard = self.write();
            if guard.route_overlay() {
                return guard.remove_prefix_batched(prefix_bytes, 1024);
            }
        }

        let batch_size = 1024;
        let mut total_removed = 0;

        loop {
            let batch: Vec<Vec<u8>> = {
                let guard = self.read();
                guard
                    .iter_prefix(prefix_bytes)
                    .map(|iter| iter.take(batch_size).collect())
                    .unwrap_or_default()
            };

            if batch.is_empty() {
                break;
            }

            let guard = self.write();
            for term in batch {
                if guard.remove_impl(&term) {
                    total_removed += 1;
                }
            }
        }

        total_removed
    }

    fn iter_prefix(&self, prefix: &str) -> Option<Box<dyn Iterator<Item = String> + '_>> {
        // M3 (C6): `iter_prefix_with_arena` is routed at its public top, so this trait
        // body is overlay-routed automatically under the flip (the terms come from the
        // overlay; lossy-UTF8 mapping is unchanged).
        let guard = self.read();
        let terms = guard.iter_prefix_with_arena(prefix.as_bytes()).ok()??;
        Some(Box::new(
            terms
                .into_iter()
                .map(|t| String::from_utf8_lossy(&t.term).into_owned()),
        ))
    }

    fn sync(&self) -> Result<()> {
        let guard = self.read();
        guard.sync()
    }

    fn current_lsn(&self) -> u64 {
        let guard = self.read();
        guard.current_lsn()
    }

    fn synced_lsn(&self) -> Option<u64> {
        let guard = self.read();
        guard.synced_lsn()
    }

    fn durability_policy(&self) -> DurabilityPolicy {
        let guard = self.read();
        guard.durability_policy()
    }

    fn upsert(&self, term: &str, value: Self::Value) -> Result<bool> {
        let guard = self.write();
        guard.upsert(term, value)
    }

    // C1: `increment` removed from the `ARTrie` trait (now an inherent `V: Counter`
    // method on PersistentARTrie). Delegation commented out (not deleted) per
    // convention; counter callers use the inner inherent method, e.g.
    // `trie.write().increment(..)` on a `<i64>`/`<u64>` trie.
    // fn increment(&self, term: &str, delta: i64) -> Result<i64> {
    //     let guard = self.write();
    //     guard.increment(term, delta)
    // }
}

impl<V: DictionaryValue> EvictableARTrie for SharedARTrie<V> {
    fn enable_eviction(&self, config: EvictionConfig) -> Result<()> {
        config
            .validate()
            .map_err(|e| PersistentARTrieError::internal(&e))?;

        // F4 (EC leaf): the coordinator field is a `Mutex<Option<Arc<…>>>`. Check +
        // install under a BRIEF EC lock; the coordinator is fully built + started
        // OUTSIDE the lock so EC is never held across thread spawns or any other
        // lock. Already-enabled ⇒ error (no old Arc to drop, so no re-arm join).
        if self
            .eviction_coordinator
            .lock()
            .expect("eviction_coordinator mutex poisoned")
            .is_some()
        {
            return Err(PersistentARTrieError::internal("Eviction already enabled"));
        }

        let epoch_manager = Arc::new(EpochManager::new());
        let coordinator = EvictionCoordinator::new(config.clone(), epoch_manager);
        let self_weak = Arc::downgrade(self);

        coordinator
            .start(move |nodes_to_evict| {
                let Some(trie) = self_weak.upgrade() else {
                    return (0, 0);
                };
                // Clone the coordinator Arc out under a BRIEF EC lock (leaf), then
                // release EC BEFORE taking OR (the lock order is OR > EC, so we must
                // not hold EC while `evict_node_at_path` takes the OR root lock).
                let coordinator = {
                    match trie
                        .eviction_coordinator
                        .lock()
                        .expect("eviction_coordinator mutex poisoned")
                        .as_ref()
                    {
                        Some(c) => Arc::clone(c),
                        None => return (0, 0),
                    }
                };
                let mut evicted_count = 0;
                let mut bytes_freed = 0;
                for (_path_hash, path, disk_ptr) in nodes_to_evict {
                    // `evict_node_at_path` takes the OR write lock internally (the
                    // owned-tree unswizzle); the LRU removal hits the already-cloned
                    // coordinator (no EC re-lock under OR).
                    if trie.evict_node_at_path(&path, disk_ptr.clone()) {
                        evicted_count += 1;
                        bytes_freed += 256;
                        coordinator.lru_registry().remove(&path);
                    }
                }
                (evicted_count, bytes_freed)
            })
            .map_err(|e| PersistentARTrieError::internal(&e))?;

        coordinator
            .start_memory_monitor()
            .map_err(|e| PersistentARTrieError::internal(&e))?;

        // Install under a brief EC lock (re-check in case of a concurrent enable —
        // first writer wins; a loser shuts its own coordinator down outside EC).
        let mut slot = self
            .eviction_coordinator
            .lock()
            .expect("eviction_coordinator mutex poisoned");
        if slot.is_some() {
            drop(slot);
            coordinator.shutdown();
            return Err(PersistentARTrieError::internal("Eviction already enabled"));
        }
        *slot = Some(coordinator);
        Ok(())
    }

    fn disable_eviction(&self) -> Result<()> {
        // **F4 drop-before-join (GAP 1 / V11.3 site 1):** take the coordinator out
        // of the EC `Mutex` into a statement-temporary so the EC guard DROPS before
        // `shutdown()` joins the eviction thread — the eviction callback takes OR
        // (and briefly EC), so joining while holding EC would deadlock (the worker
        // waits on EC; disable holds EC + joins).
        let coordinator = self
            .eviction_coordinator
            .lock()
            .expect("eviction_coordinator mutex poisoned")
            .take();
        // EC guard dropped here.
        if let Some(coordinator) = coordinator {
            coordinator.shutdown();
        }
        Ok(())
    }

    fn eviction_enabled(&self) -> bool {
        self.eviction_coordinator
            .lock()
            .expect("eviction_coordinator mutex poisoned")
            .is_some()
    }

    fn eviction_stats(&self) -> EvictionStats {
        self.eviction_coordinator
            .lock()
            .expect("eviction_coordinator mutex poisoned")
            .as_ref()
            .map(|c| c.stats())
            .unwrap_or_default()
    }

    fn force_eviction(&self, target_bytes: usize) -> Result<(usize, usize)> {
        // Clone the coordinator Arc out under a BRIEF EC lock, then release EC
        // before `force_eviction` (whose reclaim callback takes OR — order OR > EC).
        let coordinator = {
            match self
                .eviction_coordinator
                .lock()
                .expect("eviction_coordinator mutex poisoned")
                .as_ref()
            {
                Some(c) => Arc::clone(c),
                None => return Ok((0, 0)),
            }
        };
        Ok(coordinator.force_eviction(target_bytes))
    }

    fn touch_node(&self, path: &[Self::Unit]) {
        if let Some(coordinator) = self
            .eviction_coordinator
            .lock()
            .expect("eviction_coordinator mutex poisoned")
            .as_ref()
        {
            coordinator.lru_registry().touch(path);
        }
    }
}

impl<V: DictionaryValue, S: BlockStorage> PersistentARTrie<V, S> {
    /// Evict a single node at the given path, replacing it with a DiskRef.
    ///
    /// Returns `true` if the node was successfully evicted, `false` if the
    /// node was not found or was already a DiskRef.
    ///
    /// # Safety
    ///
    /// This method should only be called after epoch quiescence has been
    /// achieved, ensuring no readers from the old epoch are active.
    pub(crate) fn evict_node_at_path(&self, path: &[u8], disk_ptr: SwizzledPtr) -> bool {
        // **F4:** `&self` taking the OR write lock once. `find_parent_in_root`
        // operates on the held guard's `&mut TrieRoot` (no re-lock).
        if path.is_empty() {
            return false;
        }

        let parent_path = &path[..path.len() - 1];
        let target_edge = path[path.len() - 1];

        let mut root = self.root.write();
        match Self::find_parent_in_root(&mut root, parent_path) {
            Some(children) => {
                for (edge, child) in children.iter_mut() {
                    if *edge == target_edge {
                        match child {
                            ChildNode::DiskRef { .. } => {
                                return false;
                            }
                            ChildNode::Bucket(_) | ChildNode::ArtNode { .. } => {
                                *child = ChildNode::DiskRef { ptr: disk_ptr };
                                return true;
                            }
                        }
                    }
                }
                false
            }
            None => false,
        }
    }

    /// Find the children vector of the node at the given path, within an
    /// explicitly-held `root` (the OR guard's target — no re-lock).
    ///
    /// Returns `Some(&mut Vec<(u8, ChildNode)>)` if found, `None` if the path
    /// doesn't exist or leads to a bucket/disk ref.
    fn find_parent_in_root<'r>(
        root: &'r mut TrieRoot<V>,
        path: &[u8],
    ) -> Option<&'r mut Vec<(u8, ChildNode)>> {
        if path.is_empty() {
            match root {
                TrieRoot::Bucket(_) => None,
                TrieRoot::ArtNode { children, .. } => Some(children),
            }
        } else {
            let mut current_children = match root {
                TrieRoot::Bucket(_) => return None,
                TrieRoot::ArtNode { children, .. } => children,
            };

            for &edge in &path[..path.len().saturating_sub(1)] {
                let found = current_children.iter_mut().find(|(e, _)| *e == edge);

                match found {
                    Some((_, ChildNode::ArtNode { children, .. })) => {
                        current_children = children;
                    }
                    _ => return None,
                }
            }

            if path.is_empty() {
                return Some(current_children);
            }

            let last_edge = path[path.len() - 1];
            let found = current_children.iter_mut().find(|(e, _)| *e == last_edge);

            match found {
                Some((_, ChildNode::ArtNode { children, .. })) => Some(children),
                _ => None,
            }
        }
    }
}
