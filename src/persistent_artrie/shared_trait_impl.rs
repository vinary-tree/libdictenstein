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
use crate::sync_compat::RwLock;
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
        PersistentARTrie::create(path).map(|t| Arc::new(RwLock::new(t)))
    }

    fn create_with_slot_tracking<P: AsRef<Path>>(path: P) -> Result<Self> {
        PersistentARTrie::create_with_slot_tracking(path).map(|t| Arc::new(RwLock::new(t)))
    }

    fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        PersistentARTrie::open(path).map(|t| Arc::new(RwLock::new(t)))
    }

    fn open_with_slot_tracking<P: AsRef<Path>>(path: P) -> Result<Self> {
        PersistentARTrie::open_with_slot_tracking(path).map(|t| Arc::new(RwLock::new(t)))
    }

    fn open_with_recovery<P: AsRef<Path>>(path: P) -> Result<(Self, RecoveryReport)> {
        PersistentARTrie::open_with_recovery(path).map(|(t, r)| (Arc::new(RwLock::new(t)), r))
    }

    fn open_with_recovery_and_slot_tracking<P: AsRef<Path>>(
        path: P,
    ) -> Result<(Self, RecoveryReport)> {
        let (trie, report) = PersistentARTrie::open_with_recovery(path)?;
        if let Some(ref am) = trie.arena_manager {
            am.write().enable_slot_tracking();
        }
        Ok((Arc::new(RwLock::new(trie)), report))
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
        let mut guard = self.write();
        if guard.route_overlay() {
            return guard.insert(term);
        }
        guard.insert_impl(term.as_bytes(), Some(V::default()))
    }

    fn insert_with_value(&self, term: &str, value: Self::Value) -> bool {
        // M3 (C5): route to the routed inherent `insert_with_value` under the flip.
        let mut guard = self.write();
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
        let mut guard = self.write();
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
        let mut guard = self.write();
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
            let mut guard = self.write();
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

            let mut guard = self.write();
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
        let mut guard = self.write();
        guard.upsert(term, value)
    }

    fn increment(&self, term: &str, delta: i64) -> Result<i64> {
        let mut guard = self.write();
        guard.increment(term, delta)
    }
}

impl<V: DictionaryValue> EvictableARTrie for SharedARTrie<V> {
    fn enable_eviction(&self, config: EvictionConfig) -> Result<()> {
        config
            .validate()
            .map_err(|e| PersistentARTrieError::internal(&e))?;

        let mut guard = self.write();

        if guard.eviction_coordinator.is_some() {
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

                let mut guard = trie.write();
                let mut evicted_count = 0;
                let mut bytes_freed = 0;

                for (_path_hash, path, disk_ptr) in nodes_to_evict {
                    if guard.evict_node_at_path(&path, disk_ptr.clone()) {
                        evicted_count += 1;
                        bytes_freed += 256;

                        if let Some(ref coordinator) = guard.eviction_coordinator {
                            coordinator.lru_registry().remove(&path);
                        }
                    }
                }

                (evicted_count, bytes_freed)
            })
            .map_err(|e| PersistentARTrieError::internal(&e))?;

        coordinator
            .start_memory_monitor()
            .map_err(|e| PersistentARTrieError::internal(&e))?;

        guard.eviction_coordinator = Some(coordinator);

        Ok(())
    }

    fn disable_eviction(&self) -> Result<()> {
        // Take the coordinator out under a short-lived write guard, then RELEASE
        // the guard before `shutdown()` joins the eviction thread: the eviction
        // callback itself takes `trie.write()`, so joining while holding the trie
        // lock deadlocks (the same rule `force_eviction` already documents).
        let coordinator = self.write().eviction_coordinator.take();
        if let Some(coordinator) = coordinator {
            coordinator.shutdown();
        }
        Ok(())
    }

    fn eviction_enabled(&self) -> bool {
        let guard = self.read();
        guard.eviction_coordinator.is_some()
    }

    fn eviction_stats(&self) -> EvictionStats {
        let guard = self.read();
        guard
            .eviction_coordinator
            .as_ref()
            .map(|c| c.stats())
            .unwrap_or_default()
    }

    fn force_eviction(&self, target_bytes: usize) -> Result<(usize, usize)> {
        let guard = self.read();

        let Some(coordinator) = &guard.eviction_coordinator else {
            return Ok((0, 0));
        };

        Ok(coordinator.force_eviction(target_bytes))
    }

    fn touch_node(&self, path: &[Self::Unit]) {
        let guard = self.read();
        if let Some(coordinator) = &guard.eviction_coordinator {
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
    pub(crate) fn evict_node_at_path(&mut self, path: &[u8], disk_ptr: SwizzledPtr) -> bool {
        if path.is_empty() {
            return false;
        }

        let parent_path = &path[..path.len() - 1];
        let target_edge = path[path.len() - 1];

        match self.find_parent_mut(parent_path) {
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

    /// Find the children vector of the node at the given path.
    ///
    /// Returns `Some(&mut Vec<(u8, ChildNode)>)` if found, `None` if the path
    /// doesn't exist or leads to a bucket/disk ref.
    fn find_parent_mut(&mut self, path: &[u8]) -> Option<&mut Vec<(u8, ChildNode)>> {
        if path.is_empty() {
            match &mut self.root {
                TrieRoot::Bucket(_) => None,
                TrieRoot::ArtNode { children, .. } => Some(children),
            }
        } else {
            let mut current_children = match &mut self.root {
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
