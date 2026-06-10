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
use crate::persistent_artrie_core::durability::DurabilityPolicy;
use crate::persistent_artrie_core::eviction::{EvictionConfig, EvictionCoordinator, EvictionStats};
// F4: the `.read()/.write()` compat shim on the collapsed `Arc<PersistentARTrie>`.
use crate::persistent_artrie_core::shared_access::SharedTrieAccess;
use crate::value::DictionaryValue;

use super::dict_impl::PersistentARTrie;
use super::error::{PersistentARTrieError, Result};
use super::recovery::RecoveryReport;
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
        // L3.3c: the overlay is the sole representation; route to the routed inherent
        // `insert` (→ `insert_cas_durable`). The durable membership insert is value-free.
        self.write().insert(term)
    }

    fn insert_with_value(&self, term: &str, value: Self::Value) -> bool {
        // L3.3c: route to the routed inherent `insert_with_value` (overlay upsert).
        self.write().insert_with_value(term, value)
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
        // L3.3c: route to the routed inherent `remove` (→ `remove_cas_durable`).
        self.write().remove(term)
    }

    #[inline]
    fn len(&self) -> usize {
        // The lock-free overlay is the SOLE representation, so count its resident finals.
        // (The owned `term_count` is no longer maintained — it was cleared on reopen — and
        // `route_overlay()` is universally true, so the old owned-fallback branch was dead.)
        self.read().overlay_len()
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
        // L3.3c: the overlay is the sole representation; route to the routed inherent
        // `remove_prefix_batched` (overlay remove-CAS).
        self.write().remove_prefix_batched(prefix.as_bytes(), 1024)
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

        // Phase 6 (byte epoch-share, mirror char): SHARE this trie's OWN epoch manager
        // with the coordinator (was a SEPARATE `Arc::new(EpochManager::new())`). The
        // field is now `Arc<EpochManager>`, and `SharedARTrie = Arc<PersistentARTrie>`
        // derefs to it directly. The overlay read/write paths + the lifted overlay
        // evictor (`OverlayEvictable::{find_leaf_faulting, evict_overlay_node_at_path}`)
        // pin THIS same manager via `enter_read`, so the coordinator's quiescence drain
        // genuinely waits on the live overlay readers (honest reader accounting; not a
        // correctness change — overlay reclamation is by `Arc` refcount, not EBR).
        let epoch_manager = Arc::clone(&self.epoch_manager);
        let coordinator = EvictionCoordinator::new(config.clone(), epoch_manager);
        let self_weak = Arc::downgrade(self);

        coordinator
            .start(move |nodes_to_evict| {
                let Some(trie) = self_weak.upgrade() else {
                    return (0, 0);
                };
                // Phase 7.5: route_overlay-GATED. Under the overlay regime reclaim the
                // OVERLAY (the inline evict_node_at_path owned loop below is a no-op on the
                // EMPTY owned tree there); in owned mode keep the proven owned-tree loop
                // (preserves owned + ineligible-V eviction). evict_overlay_nodes locks EC
                // for its LRU remove — safe here (the loop holds no EC, same as the owned
                // loop's EC discipline).
                // L0.1/L3.3: always reclaim the overlay (the owned tree is gone).
                // `evict_overlay_nodes` locks EC for its LRU remove; safe here (this
                // callback holds no EC).
                crate::persistent_artrie::overlay_fault::evict_overlay_nodes(
                    &trie,
                    nodes_to_evict,
                    4,
                )
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
        // L0.1: always reclaim the OVERLAY (the owned select-and-count arm was deleted).
        // `force_eviction_bytes` returns the EVICTED count, not the candidate count.
        let trie = Arc::clone(self);
        Ok(
            coordinator.force_eviction_bytes(target_bytes, move |nodes| {
                crate::persistent_artrie::overlay_fault::evict_overlay_nodes(&trie, nodes, 4)
            }),
        )
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
