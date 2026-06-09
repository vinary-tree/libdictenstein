//! Byte seam impl of the shared [`LockFreeOverlay`] flip + thin inherent
//! wrappers preserving the byte public surface.
//!
//! This is the BYTE twin of `persistent_artrie_char::overlay_write_mode`. The
//! overlay-flip genericization (`docs/design/overlay-durable-architecture.md`)
//! extracted the lock-free-overlay flip (route predicate + non-faulting RCU read
//! engine + flip + reestablish folds) into the SHARED GENERIC
//! [`LockFreeOverlay`](crate::persistent_artrie_core::overlay::flip::LockFreeOverlay)
//! trait. This module holds only:
//!
//! 1. the byte SEAM impl `impl LockFreeOverlay<ByteKey, V, S> for
//!    PersistentARTrie<V, S>` (per-variant owned readers / overlay publishers /
//!    WAL accessors / `CounterValue = u64`, matching char post-u64-restoration);
//! 2. thin inherent wrappers (`route_overlay` / `flip_to_overlay` /
//!    `overlay_eligible_v`) that DELEGATE to the trait, so the byte call sites and
//!    the byte correspondence tests can use inherent syntax.
//!
//! **L3.3 — the overlay is the SOLE representation.** Every byte constructor
//! installs the lock-free overlay, so `route_overlay()` is universally true; the
//! owned tree, the `OverlayWriteMode` kill-switch enum, and `kill_switch_to_owned`
//! were deleted. This module gives byte the trait DEFAULTS: the route predicate,
//! the read engine (`overlay_len`/`overlay_iter_prefix*`/`overlay_get_value`), the
//! flip, and the no-WAL reestablish folds.
//!
//! # D1 — the #1 data-loss risk (READ BEFORE EDITING A SEAM IMPL)
//!
//! The `owned_*` seam methods MUST read the OWNED tree (`self.root`) directly.
//! The reestablish folds run while `route_overlay()` is ALREADY TRUE, so routing
//! an owned read through a public `iter`/`get`/`contains`/`get_value` (the routed
//! readers) would read the EMPTY overlay, publish nothing, then clear the owned
//! tree LAST = TOTAL IRREVERSIBLE LOSS. A CI grep gate FAILS if any `fn owned_*`
//! body references `route_overlay`/`iter_prefix(`/`self.get(`/`get_value(`/
//! `contains(`. Byte's owned enumerators below
//! ([`PersistentARTrie::unrouted_collect_under`] /
//! [`PersistentARTrie::unrouted_collect_with_values_under`]) walk `self.root`
//! directly.
//!
//! # H1/H2 — UNCAPPED owned enumerator (silent-truncation risk)
//!
//! Byte's public arena iterators (`iter_prefix_with_arena`) cap at
//! `DEFAULT_LIMIT = 100_000` AND are the methods that the M3 production read-flip
//! later ROUTES. Reusing them for the `owned_*` seams would (a) truncate
//! reestablish at 100k terms = silent loss, and (b) couple the owned readers to
//! the routed path. So the `owned_*` seams instead use the PRIVATE, UN-routed,
//! UNCAPPED walks below — a fresh DFS of `self.root` to completion, no limit, no
//! arena tracking, no `route_overlay()` check.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use super::block_storage::BlockStorage;
use super::bucket::StringBucket;
use super::dict_impl::{PersistentARTrie, TrieRoot};
use super::error::{PersistentARTrieError, Result};
use super::transitions::ChildNode;
use crate::persistent_artrie_core::durability::DurabilityPolicy;
use crate::persistent_artrie_core::key_encoding::{ByteKey, KeyEncoding};
use crate::persistent_artrie_core::overlay::durable_write::{
    DurableOverlayWrite, ValuePublishOutcome, ValueWriteMode,
};
use crate::persistent_artrie_core::overlay::flip::LockFreeOverlay;
use crate::persistent_artrie_core::wal::{Lsn, RankRegime, WalRecord};
use crate::value::DictionaryValue;

// ============================================================================
// Private UN-ROUTED, UNCAPPED owned enumerators (D1 + H1/H2).
//
// These walk `self.root` (the OWNED tree) directly — never the overlay, never a
// routed public read — and to COMPLETION (no `DEFAULT_LIMIT` cap, unlike
// `iter_prefix_with_arena`). They are the foundation of the `owned_*` seams.
// ============================================================================

impl<V: DictionaryValue, S: BlockStorage> PersistentARTrie<V, S> {
    /// **UN-ROUTED, UNCAPPED owned DFS** under a child node: push every final
    /// term (membership) into `out`, recursing through in-memory children AND
    /// resolving on-disk children (owned terms may be disk-resident). No limit.
    ///
    /// # Safety (data-loss)
    ///
    /// Reads only `self.root`-reachable owned state (via `child`); never the
    /// overlay. See the module-level D1 note.
    fn unrouted_collect_terms_under_child(
        &self,
        child: &ChildNode,
        mut prefix: Vec<u8>,
        out: &mut Vec<Vec<u8>>,
    ) {
        match child {
            ChildNode::Bucket(bucket) => {
                for i in 0..bucket.len() {
                    if let Some(entry) = bucket.get_entry(i) {
                        let suffix = bucket.get_suffix(&entry);
                        let mut term = prefix.clone();
                        term.extend_from_slice(suffix);
                        out.push(term);
                    }
                }
            }
            ChildNode::ArtNode {
                node,
                is_final,
                children,
                ..
            } => {
                // CX (#43 / L2.1): fold THIS node's compressed prefix into the path BEFORE
                // recording finality or descending. `prefix` on entry already carries the
                // incoming edge to this node; a path-compressed image (the CX-serialized
                // compacted overlay) stores a chunk's inter-edges as `node.prefix()`, so they
                // MUST be appended here or the reconstructed term is truncated (data loss — the
                // chunk prefix would be dropped, e.g. "single" → "se"). A NORMAL owned image
                // never sets `prefix_len > 0` (the owned write path compresses via StringBucket
                // suffixes, never node-header prefixes), so for every non-CX tree this is a
                // strict no-op. Mirrors the proven `load_overlay_node_from_disk` expansion.
                let plen = node.header().prefix_len as usize;
                if plen > 0 {
                    prefix.extend_from_slice(&node.prefix().bytes[..plen]);
                }
                if *is_final {
                    out.push(prefix.clone());
                }
                for (edge, grandchild) in children {
                    let mut child_prefix = prefix.clone();
                    child_prefix.push(*edge);
                    self.unrouted_collect_terms_under_child(grandchild, child_prefix, out);
                }
            }
            ChildNode::DiskRef { ptr } => {
                if let Some(disk_location) = ptr.disk_location() {
                    if let Ok(resolved) = self.resolve_disk_ref(&disk_location) {
                        self.unrouted_collect_terms_under_child(&resolved, prefix, out);
                    }
                }
            }
        }
    }

    /// **UN-ROUTED, UNCAPPED owned DFS** under a child node, carrying values.
    /// The valued twin of [`Self::unrouted_collect_terms_under_child`].
    ///
    /// # Safety (data-loss)
    ///
    /// Reads only `self.root`-reachable owned state; never the overlay.
    fn unrouted_collect_terms_with_values_under_child(
        &self,
        child: &ChildNode,
        mut prefix: Vec<u8>,
        out: &mut Vec<(Vec<u8>, V)>,
    ) where
        V: Clone,
    {
        match child {
            ChildNode::Bucket(bucket) => {
                for i in 0..bucket.len() {
                    if let Some(entry) = bucket.get_entry(i) {
                        let suffix = bucket.get_suffix(&entry);
                        let mut term = prefix.clone();
                        term.extend_from_slice(suffix);
                        if let Some(value_bytes) = bucket.get_value(&entry) {
                            if let Ok(value) =
                                crate::serialization::bincode_compat::deserialize::<V>(value_bytes)
                            {
                                out.push((term, value));
                            }
                        }
                    }
                }
            }
            ChildNode::ArtNode {
                node,
                is_final,
                value,
                children,
            } => {
                // CX (#43 / L2.1): fold THIS node's compressed prefix into the path BEFORE
                // recording the value or descending — the value twin of the membership
                // collector's fold. Without this a CX-compacted chunk prefix is dropped and the
                // value is attributed to the truncated term (data loss: the real term reopens
                // as `Some(None)`). No-op for normal owned images (`prefix_len == 0`).
                let plen = node.header().prefix_len as usize;
                if plen > 0 {
                    prefix.extend_from_slice(&node.prefix().bytes[..plen]);
                }
                if *is_final {
                    if let Some(value_bytes) = value {
                        if let Ok(v) =
                            crate::serialization::bincode_compat::deserialize::<V>(value_bytes)
                        {
                            out.push((prefix.clone(), v));
                        }
                    }
                }
                for (edge, grandchild) in children {
                    let mut child_prefix = prefix.clone();
                    child_prefix.push(*edge);
                    self.unrouted_collect_terms_with_values_under_child(
                        grandchild,
                        child_prefix,
                        out,
                    );
                }
            }
            ChildNode::DiskRef { ptr } => {
                if let Some(disk_location) = ptr.disk_location() {
                    if let Ok(resolved) = self.resolve_disk_ref(&disk_location) {
                        self.unrouted_collect_terms_with_values_under_child(&resolved, prefix, out);
                    }
                }
            }
        }
    }

    /// **UN-ROUTED, UNCAPPED owned prefix enumeration.** Navigate `self.root` to
    /// `prefix` (a single byte for the reestablish folds, but works for any
    /// prefix); return `None` if the prefix path is absent, else `Some(terms)`
    /// (possibly empty), each a `Vec<u8>` term. Reproduces the owned
    /// `iter_prefix_with_arena` SET but with NO 100k cap and NO arena tracking —
    /// a fresh DFS to completion (H1/H2). The empty prefix enumerates the whole
    /// owned tree.
    ///
    /// # Safety (data-loss)
    ///
    /// Reads only `self.root` (the OWNED tree); never the overlay, never a routed
    /// public read. See the module-level D1 note.
    fn unrouted_collect_under(&self, prefix: &[u8]) -> Option<Vec<Vec<u8>>> {
        // F4 (OR read): the owned readers take the inner `root` RwLock for read.
        // Called only pre-share by `reestablish_*` (single-threaded, uncontended).
        match &*self.root.read() {
            TrieRoot::Bucket(bucket) => {
                Self::unrouted_collect_bucket_terms(bucket, prefix).filter(|v| {
                    // Match the arena iterator's `None`-vs-`Some(empty)` shape:
                    // an absent prefix in a root bucket yields `None`.
                    !v.is_empty()
                })
            }
            TrieRoot::ArtNode {
                is_final, children, ..
            } => {
                let mut out = Vec::new();
                if prefix.is_empty() {
                    if *is_final {
                        out.push(Vec::new());
                    }
                    for (edge, child) in children {
                        self.unrouted_collect_terms_under_child(child, vec![*edge], &mut out);
                    }
                    return Some(out);
                }
                // Navigate the owned spine to the prefix node, then collect.
                match self.unrouted_navigate_art(children, prefix) {
                    None => None,
                    Some((node, path)) => {
                        self.unrouted_collect_terms_under_child(node, path, &mut out);
                        Some(out)
                    }
                }
            }
        }
    }

    /// **UN-ROUTED, UNCAPPED owned (term, value) prefix enumeration** — the
    /// valued twin of [`Self::unrouted_collect_under`].
    ///
    /// # Safety (data-loss)
    ///
    /// Reads only `self.root` (the OWNED tree); never the overlay.
    fn unrouted_collect_with_values_under(&self, prefix: &[u8]) -> Option<Vec<(Vec<u8>, V)>>
    where
        V: Clone,
    {
        // F4 (OR read): owned reader, pre-share only.
        match &*self.root.read() {
            TrieRoot::Bucket(bucket) => {
                Self::unrouted_collect_bucket_terms_with_values(bucket, prefix)
                    .filter(|v| !v.is_empty())
            }
            TrieRoot::ArtNode {
                is_final,
                value,
                children,
                ..
            } => {
                let mut out = Vec::new();
                if prefix.is_empty() {
                    if *is_final {
                        if let Some(v) = value {
                            out.push((Vec::new(), v.clone()));
                        }
                    }
                    for (edge, child) in children {
                        self.unrouted_collect_terms_with_values_under_child(
                            child,
                            vec![*edge],
                            &mut out,
                        );
                    }
                    return Some(out);
                }
                match self.unrouted_navigate_art(children, prefix) {
                    None => None,
                    Some((node, path)) => {
                        self.unrouted_collect_terms_with_values_under_child(node, path, &mut out);
                        Some(out)
                    }
                }
            }
        }
    }

    /// Navigate the owned ART spine from a root-children list down to `prefix`.
    /// Returns the child node at the prefix plus the path taken (so the collector
    /// can rebuild full terms), or `None` if the path is absent. Bucket-on-path
    /// and on-disk-on-path are handled by returning the resolved/early node — the
    /// collectors then DFS from there. (UN-routed: reads only owned state.)
    fn unrouted_navigate_art<'a>(
        &self,
        root_children: &'a [(u8, ChildNode)],
        prefix: &[u8],
    ) -> Option<(&'a ChildNode, Vec<u8>)> {
        let first_byte = prefix[0];
        let remaining = &prefix[1..];
        let (_, child) = root_children.iter().find(|(b, _)| *b == first_byte)?;
        let mut current = child;
        let mut path = vec![first_byte];
        for &byte in remaining {
            match current {
                ChildNode::ArtNode { children, .. } => {
                    let (_, next_child) = children.iter().find(|(b, _)| *b == byte)?;
                    current = next_child;
                    path.push(byte);
                }
                // A bucket / on-disk node on the prefix path: the collector DFS
                // from here re-checks each leaf's full suffix against the prefix
                // tail. To preserve the arena iterator's behavior (which, for a
                // bucket-on-path, filters by the remaining suffix), stop here and
                // let the collector enumerate; the bucket collector filters.
                ChildNode::Bucket(_) | ChildNode::DiskRef { .. } => {
                    return Some((current, path));
                }
            }
        }
        Some((current, path))
    }

    /// Collect every bucket term whose suffix starts with `prefix` (root-bucket
    /// case). Returns `Some(terms)` (the terms with the prefix retained, matching
    /// the arena iterator) — the caller maps `None`-vs-`Some(empty)`.
    fn unrouted_collect_bucket_terms(bucket: &StringBucket, prefix: &[u8]) -> Option<Vec<Vec<u8>>> {
        let mut terms = Vec::new();
        for i in 0..bucket.len() {
            if let Some(entry) = bucket.get_entry(i) {
                let suffix = bucket.get_suffix(&entry);
                if suffix.starts_with(prefix) {
                    terms.push(suffix.to_vec());
                }
            }
        }
        Some(terms)
    }

    /// Valued twin of [`Self::unrouted_collect_bucket_terms`].
    fn unrouted_collect_bucket_terms_with_values(
        bucket: &StringBucket,
        prefix: &[u8],
    ) -> Option<Vec<(Vec<u8>, V)>>
    where
        V: Clone,
    {
        let mut terms = Vec::new();
        for i in 0..bucket.len() {
            if let Some(entry) = bucket.get_entry(i) {
                let suffix = bucket.get_suffix(&entry);
                if suffix.starts_with(prefix) {
                    if let Some(value_bytes) = bucket.get_value(&entry) {
                        if let Ok(value) =
                            crate::serialization::bincode_compat::deserialize::<V>(value_bytes)
                        {
                            terms.push((suffix.to_vec(), value));
                        }
                    }
                }
            }
        }
        Some(terms)
    }

    /// **UN-ROUTED** owned empty-term value read — the value of `""` from
    /// `self.root` directly (the root bucket's empty entry, or the ART root's
    /// `value` when `is_final`). The foundation of the `owned_has_empty_term_value`
    /// seam; named off the `owned_*` prefix so the D1 `fn owned_*` body grep gate
    /// (and any future naive-substring automation of it) stays clean of the
    /// `bucket.get_value(` substring.
    ///
    /// # Safety (data-loss)
    ///
    /// Reads only `self.root` (the OWNED tree); never the overlay.
    fn unrouted_empty_term_value(&self) -> Option<V> {
        // F4 (OR read): owned reader, pre-share only.
        match &*self.root.read() {
            TrieRoot::Bucket(bucket) => match bucket.search(&[]) {
                Ok(idx) => bucket
                    .get_entry(idx)
                    .and_then(|entry| bucket.get_value(&entry))
                    .and_then(|value_bytes| {
                        crate::serialization::bincode_compat::deserialize::<V>(value_bytes).ok()
                    }),
                Err(_) => None,
            },
            TrieRoot::ArtNode {
                is_final, value, ..
            } => {
                if *is_final {
                    value.clone()
                } else {
                    None
                }
            }
        }
    }
}

// ============================================================================
// Byte seam impl of the shared LockFreeOverlay flip (M2a).
// ============================================================================

impl<V: DictionaryValue, S: BlockStorage> LockFreeOverlay<ByteKey, V, S>
    for PersistentARTrie<V, S>
{
    /// `u64` — the byte counter monomorph (matching char, post-u64-restoration). The
    /// overlay leaf stores the count as the trie's own `u64` value, so
    /// `overlay_counter_get` needs no boundary conversion.
    type CounterValue = u64;

    // ---- small accessors ----

    #[inline]
    fn lockfree_root(
        &self,
    ) -> Option<&crate::persistent_artrie_core::overlay::AtomicNodePtr<ByteKey, V>> {
        // `super::nodes::AtomicNodePtr<V>` IS `overlay::AtomicNodePtr<ByteKey, V>`
        // (a type alias — see `nodes::atomic_ptr`), so this borrow is identity.
        self.lockfree_root.as_ref()
    }

    #[inline]
    fn enable_lockfree(&mut self) {
        // Delegate to the existing inherent `enable_lockfree` (lockfree_cas.rs),
        // which installs the `AtomicNodePtr` root + cache. NB byte's
        // `enable_lockfree` does NOT stamp the WAL Overlay regime (unlike char's);
        // the generic `flip_to_overlay` default performs the regime stamp via the
        // `wal_current_lsn() == Some(1)` empty-WAL guard, so byte's flip is still
        // durably correct.
        PersistentARTrie::enable_lockfree(self)
    }

    #[inline]
    fn wal_current_lsn(&self) -> Option<u64> {
        self.wal_writer.as_ref().map(|w| w.current_lsn())
    }

    #[inline]
    fn wal_is_overlay_regime(&self) -> bool {
        self.wal_writer
            .as_ref()
            .map(|w| w.rank_regime() == RankRegime::Overlay)
            .unwrap_or(false)
    }

    fn wal_stamp_overlay_regime(&self) {
        if let Some(ref writer) = self.wal_writer {
            if let Err(e) = writer.set_overlay_regime() {
                log::warn!("flip_to_overlay: could not stamp Overlay regime: {:?}", e);
            }
        }
    }

    /// ANY `V: DictionaryValue` is overlay-eligible (F2, design G5).
    ///
    /// Arbitrary-V overlay routing is the production default: the generic value
    /// path routes every `V` through the lock-free overlay. Since L3.3 deleted the
    /// owned tree, the overlay is the SOLE representation.
    fn overlay_eligible_v() -> bool {
        true
    }

    // ---- overlay publishers (the per-variant write seam) ----

    fn overlay_publish_membership(&self, units: &[u8]) {
        // No-WAL CAS insert (recovered terms are already durable in the WAL;
        // re-logging would double-log). `units` IS the byte term.
        self.insert_cas(units);
    }

    fn overlay_counter_get(&self, units: &[u8]) -> Option<u64> {
        // SAFE `Any` downcast to `<u64, S>` + the lock-free point read.
        // `get_lockfree` returns the count as the trie's own `u64` value (direct).
        use std::any::Any;
        (self as &dyn Any)
            .downcast_ref::<PersistentARTrie<u64, S>>()
            .and_then(|trie_u64| trie_u64.get_lockfree(units))
    }

    fn overlay_contains(&self, units: &[u8]) -> bool {
        self.contains_lockfree(units)
    }

    fn overlay_publish_value(&self, units: &[u8], value: V) {
        // G5/F1: no-WAL path-copy value SET (recovered terms are already durable).
        // Fresh overlay at reestablish ⇒ no OnDisk children, no contention. `units`
        // ARE the raw key bytes for byte.
        use super::nodes::persistent_node::PersistentNode;
        let lockfree_root = match self.lockfree_root.as_ref() {
            Some(r) => r,
            None => return,
        };
        let _epoch = self.epoch_manager.enter_read();
        loop {
            let root = match lockfree_root.load() {
                Some(r) => r,
                None => {
                    let _ = lockfree_root.try_init(Arc::new(PersistentNode::<V>::new()));
                    continue;
                }
            };
            match self.build_value_path_recursive(&root, units, 0, value.clone()) {
                Some(new_root) => match lockfree_root.compare_exchange(&root, new_root) {
                    Ok(_) => {
                        if let Some(ref cache) = self.lockfree_cache {
                            cache.insert(units.to_vec(), true);
                        }
                        return;
                    }
                    Err(_) => continue,
                },
                None => return,
            }
        }
    }

    fn overlay_value_get(&self, units: &[u8]) -> Option<V> {
        // Non-faulting leaf value read (exact: overlay finals never evicted in prod).
        let lockfree_root = self.lockfree_root.as_ref()?;
        let _epoch = self.epoch_manager.enter_read();
        self.find_leaf_lockfree(lockfree_root, units)
            .and_then(|leaf| leaf.get_value())
    }

    fn claim_commit_seq(&self) -> u64 {
        // Empty-string support: the per-iteration commit generation — the SAME
        // `self.commit_seq` the durable insert/increment paths claim (monotone in
        // the global root-CAS order, durable across restart).
        use std::sync::atomic::Ordering;
        self.commit_seq.fetch_add(1, Ordering::AcqRel) + 1
    }

    fn note_cas_retry(&self) {
        use std::sync::atomic::Ordering;
        self.cas_retries.fetch_add(1, Ordering::Relaxed);
    }

    fn install_prebuilt_overlay_root_seam(
        &mut self,
        root: Arc<crate::persistent_artrie_core::overlay::node::OverlayNode<ByteKey, V>>,
    ) {
        // `OverlayNode<ByteKey, V>` IS `PersistentNode<V>` (a type alias — see
        // `nodes::persistent_node`), so this is an identity install. Delegates to the
        // inherent F5 helper (same module as the private `lockfree_root` field).
        self.install_prebuilt_overlay_root_inherent(root)
    }

    fn overlay_try_remove_path(&self, units: &[u8]) {
        // F5 WAL-tail Remove arm: no-WAL overlay remove via the inherent helper
        // (which uses the existing single-arbiter `try_remove_lockfree_path`).
        self.overlay_remove_no_wal(units)
    }

    fn load_root_immutable_seam(&mut self, root_ptr: u64) -> Result<bool> {
        // F7/BLOCKER#4 — forward the REAL `image_loaded` from the byte codec
        // `load_root_immutable` (which falls back to an EMPTY overlay + `image_loaded = false`
        // on a corrupt/absent dense image), so the converter's drain skips nothing the absent
        // image fails to cover (corrupt-descriptor fallback parity, mirroring char). PRECONDITION
        // (converter): the WAL is already Overlay-regime, so the V-2 install check inside passes.
        let (_term_count, image_loaded) = self.load_root_immutable(root_ptr)?;
        Ok(image_loaded)
    }
}

// ============================================================================
// Byte seam impl of the shared DurableOverlayWrite (Order-A) skeleton
// (overlay-durable-architecture.md, trait 2, step M2b). The generic defaults own
// the data-loss-critical control flow (the durability gate, the append→publish→
// mark ORDER, the commit-rank + watermark tail, the full increment template);
// this impl supplies ONLY the per-variant seams: the WAL/watermark accessors + the
// increment's i64 value-domain bound (C4) / delta-record builder / proven path-copy
// publish. The byte counter monomorph is `i64` (`CounterValue` from `LockFreeOverlay`),
// the one divergence from char's `u64`.
//
// Byte-identical ORDER: the seams delegate to the EXISTING byte inherent helpers
// (`append_to_wal_returning_lsn`, `append_commit_rank`, `committed_watermark.mark_committed`,
// `try_increment_cas_inner`) so the CommitRank/generation/watermark ordering is the
// SAME proven sequence char uses (TLA-verified in LockFreeDurableCheckpoint.tla).
// ============================================================================

impl<V: DictionaryValue, S: BlockStorage> DurableOverlayWrite<ByteKey, V, S>
    for PersistentARTrie<V, S>
{
    #[inline]
    fn durability_policy(&self) -> DurabilityPolicy {
        // The inherent accessor (persistence_api.rs) — unchanged value.
        PersistentARTrie::durability_policy(self)
    }

    #[inline]
    fn append_durable_wal(&self, record: WalRecord) -> Result<Lsn> {
        // Order-A step 1: byte's LSN-returning durable append (persistence_api.rs).
        self.append_to_wal_returning_lsn(record)
    }

    #[inline]
    fn append_commit_rank(&self, data_lsn: Lsn, term: &[u8], generation: u64) -> Result<Lsn> {
        // Order-A step 2.5: byte's CommitRank append (persistence_api.rs).
        PersistentARTrie::append_commit_rank(self, data_lsn, term, generation)
    }

    #[inline]
    fn mark_committed(&self, lsn: Lsn) {
        // Order-A step 3: advance the committed watermark — byte's M2b field.
        self.committed_watermark.mark_committed(lsn);
    }

    // ---- increment value-domain seam (C4 — the byte i64 bound) ----

    fn bound_increment_delta(&self, key: &str, delta: u64) -> Result<i64> {
        // **C4 (the byte counter value-domain bound).** Byte-identical to char now
        // that the byte counter is a full `u64`: a SINGLE durable `BatchIncrement`
        // delta is carried in ONE `i64` WAL chunk, so a `delta > i64::MAX` cannot be
        // logged by one durable call (a magnitude above `i64::MAX` is reachable via
        // the merge chunker `split_u64_delta_to_i64_chunks` or multiple durable
        // increments, NOT a single delta). The negative-delta case is filtered
        // UPSTREAM by `route_increment_bytes` (a signed public `delta < 0` routes to
        // the value-CAS path), so by construction `delta` here is a non-negative
        // `u64`. We reject `> i64::MAX` LOUD rather than wrap. Returns the bounded
        // `i64` the delta WAL record carries.
        i64::try_from(delta).map_err(|_| {
            PersistentARTrieError::InvalidOperation(format!(
                "try_increment_cas_durable delta for byte term {:?} exceeds the i64 per-call WAL \
                 delta domain: {}",
                key, delta
            ))
        })
    }

    #[inline]
    fn build_increment_record(&self, key_bytes: &[u8], bounded: i64) -> WalRecord {
        // The delta record the proven byte counter merge path logs: a single-entry,
        // delta-based BatchIncrement (commutative on replay), exactly as char.
        WalRecord::BatchIncrement {
            entries: vec![(key_bytes.to_vec(), bounded)],
        }
    }

    fn increment_publish_inner(&self, key: &str, delta: u64) -> Result<(u64, u64)> {
        // `try_increment_cas_inner` is u64-specialized (`impl<S> ...<u64, S>`), so
        // downcast `self` to the nameable `<u64, S>` monomorph via a SAFE `Any`
        // (the same zero-`unsafe` pattern as `overlay_counter_get`). The counter
        // durable path runs only for the u64 monomorph (the value route), so this
        // downcast always succeeds there; an ineligible `V` returns the empty result
        // (the durable increment is never reached for non-u64 `V`).
        //
        // Byte's `try_increment_cas_inner` now takes `(&[u8], u64)` and returns
        // `(u64, u64)` — the byte counter is `u64`, so `delta` (already
        // `CounterValue = u64`) and the returned count flow through with NO cast.
        // `key.as_bytes()` recovers the raw key bytes (byte's durable increment path
        // operates on UTF-8 keys — the public wrapper validates this).
        use std::any::Any;
        match (self as &dyn Any).downcast_ref::<PersistentARTrie<u64, S>>() {
            Some(trie_u64) => trie_u64.try_increment_cas_inner(key.as_bytes(), delta),
            None => Ok((0, 0)),
        }
    }

    // ---- G5/F0 value seams (byte): byte keys are raw `&[u8]` (no str), the
    // counter is i64, and the byte overlay has NO write-path fault-in (overlay
    // finals are never evicted in production — RT5 — so the non-faulting walk is
    // exact). Mirrors the char seams otherwise. ----

    fn value_present_faulting(&self, key_bytes: &[u8]) -> Result<bool> {
        let lockfree_root = self.lockfree_root.as_ref().ok_or_else(|| {
            PersistentARTrieError::InvalidOperation(
                "Lock-free mode not enabled. Call enable_lockfree() first.".to_string(),
            )
        })?;
        let _epoch = self.epoch_manager.enter_read();
        Ok(self.find_leaf_lockfree(lockfree_root, key_bytes).is_some())
    }

    fn value_read_faulting(&self, key_bytes: &[u8]) -> Result<Option<V>> {
        let lockfree_root = self.lockfree_root.as_ref().ok_or_else(|| {
            PersistentARTrieError::InvalidOperation(
                "Lock-free mode not enabled. Call enable_lockfree() first.".to_string(),
            )
        })?;
        let _epoch = self.epoch_manager.enter_read();
        Ok(self
            .find_leaf_lockfree(lockfree_root, key_bytes)
            .and_then(|leaf| leaf.get_value()))
    }

    fn value_publish_inner(
        &self,
        key_bytes: &[u8],
        value: V,
        mode: ValueWriteMode,
    ) -> Result<ValuePublishOutcome> {
        use super::nodes::persistent_node::PersistentNode;
        let lockfree_root = self.lockfree_root.as_ref().ok_or_else(|| {
            PersistentARTrieError::InvalidOperation(
                "Lock-free mode not enabled. Call enable_lockfree() first.".to_string(),
            )
        })?;
        let _epoch = self.epoch_manager.enter_read();
        loop {
            let commit_seq = self.commit_seq.fetch_add(1, Ordering::AcqRel) + 1;
            let root = match lockfree_root.load() {
                Some(r) => r,
                None => {
                    let new_root = Arc::new(PersistentNode::<V>::new());
                    let _ = lockfree_root.try_init(new_root);
                    continue;
                }
            };
            // Mode pre-check on the FRESHLY-loaded root.
            match &mode {
                ValueWriteMode::InsertOnce => {
                    if self.find_leaf_recursive(&root, key_bytes, 0).is_some() {
                        return Ok(ValuePublishOutcome::NotApplied);
                    }
                }
                ValueWriteMode::Upsert => {}
                ValueWriteMode::CompareAndSwap { expected_bytes } => {
                    let cur = self
                        .find_leaf_recursive(&root, key_bytes, 0)
                        .and_then(|leaf| leaf.get_value());
                    let cur_bytes = match &cur {
                        Some(v) => Some(
                            crate::serialization::bincode_compat::serialize(v).map_err(|e| {
                                PersistentARTrieError::internal(format!(
                                    "Failed to serialize value: {}",
                                    e
                                ))
                            })?,
                        ),
                        None => None,
                    };
                    if &cur_bytes != expected_bytes {
                        return Ok(ValuePublishOutcome::NotApplied);
                    }
                }
            }
            let new_root = match self.build_value_path_recursive(&root, key_bytes, 0, value.clone())
            {
                Some(r) => r,
                None => {
                    self.cas_retries.fetch_add(1, Ordering::Relaxed);
                    return Err(PersistentARTrieError::internal(
                        "value_publish_inner: an on-disk overlay child blocked the path-copy; \
                         the record is durable and replays on reopen",
                    ));
                }
            };
            match lockfree_root.compare_exchange(&root, new_root) {
                Ok(_) => {
                    if let Some(ref cache) = self.lockfree_cache {
                        cache.insert(key_bytes.to_vec(), true);
                    }
                    return Ok(ValuePublishOutcome::Published(commit_seq));
                }
                Err(_actual) => {
                    self.cas_retries.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
            }
        }
    }
}

// ============================================================================
// Thin inherent wrappers — preserve a byte inherent surface by DELEGATING to the
// trait (so byte call sites and the byte correspondence tests use inherent
// syntax; behavior identical to the trait defaults).
// ============================================================================

impl<V: DictionaryValue, S: BlockStorage> PersistentARTrie<V, S> {
    /// **Flip F0 — production-write/read-path router.** `true` iff reads/writes/
    /// checkpoint take the lock-free overlay path for this trie. Thin delegator to
    /// [`LockFreeOverlay::route_overlay`]. Since L3.3 deleted the owned tree, every
    /// constructor installs the overlay, so this is universally `true`.
    #[inline]
    pub fn route_overlay(&self) -> bool {
        <Self as LockFreeOverlay<ByteKey, V, S>>::route_overlay(self)
    }

    /// Overlay-eligibility gate (`V ∈ {(), i64}` for byte). Thin delegator.
    #[cfg_attr(not(test), allow(dead_code))]
    #[inline]
    pub(crate) fn overlay_eligible_v() -> bool {
        <Self as LockFreeOverlay<ByteKey, V, S>>::overlay_eligible_v()
    }

    /// **Flip construction helper.** Thin delegator to
    /// [`LockFreeOverlay::flip_to_overlay`]. Opt-in, REVERSIBLE (M2a): a NO-OP
    /// returning `false` for ineligible `V`; for eligible `V` it enables the
    /// overlay, stamps the WAL Overlay regime on a fresh WAL, and makes
    /// `route_overlay()` true.
    #[cfg_attr(not(test), allow(dead_code))]
    #[inline]
    pub(crate) fn flip_to_overlay(&mut self) -> bool {
        <Self as LockFreeOverlay<ByteKey, V, S>>::flip_to_overlay(self)
    }

    // ---- inherent skins over the trait read engine (used by the M2a test) ----

    /// Overlay term count (resident-finals). Thin delegator to the read engine.
    #[cfg_attr(not(test), allow(dead_code))]
    #[inline]
    pub(crate) fn overlay_len(&self) -> usize {
        <Self as LockFreeOverlay<ByteKey, V, S>>::overlay_len(self)
    }

    /// Overlay `is_empty` (cheap early-out). Thin delegator.
    #[cfg_attr(not(test), allow(dead_code))]
    #[inline]
    pub(crate) fn overlay_is_empty(&self) -> bool {
        <Self as LockFreeOverlay<ByteKey, V, S>>::overlay_is_empty(self)
    }

    /// Overlay prefix iteration, mapped to public byte terms (`Vec<u8>`). Thin
    /// skin over the trait's `K::Unit`-space `overlay_collect_units`; for byte the
    /// unit IS the term byte, so the map is the identity `units_to_term`.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn overlay_iter_prefix(&self, prefix: &[u8]) -> Option<Vec<Vec<u8>>> {
        <Self as LockFreeOverlay<ByteKey, V, S>>::overlay_collect_units(self, prefix).map(|seqs| {
            seqs.into_iter()
                .map(|units| ByteKey::units_to_term(&units))
                .collect()
        })
    }

    /// Overlay (term, value) prefix iteration. Thin skin over the trait's
    /// `overlay_collect_units_with_values`.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn overlay_iter_prefix_with_values(
        &self,
        prefix: &[u8],
    ) -> Option<Vec<(Vec<u8>, V)>> {
        <Self as LockFreeOverlay<ByteKey, V, S>>::overlay_collect_units_with_values(self, prefix)
            .map(|seqs| {
                seqs.into_iter()
                    .map(|(units, v)| (ByteKey::units_to_term(&units), v))
                    .collect()
            })
    }

    /// Overlay value-route for a single term. `Some(Some(v))` present with value,
    /// `Some(None)` handled-and-absent, `None` ineligible `V` (caller reads owned).
    /// Thin delegator to the trait's `overlay_route_get_value`.
    #[cfg_attr(not(test), allow(dead_code))]
    #[inline]
    pub(crate) fn overlay_get_value(&self, term: &[u8]) -> Option<Option<V>> {
        <Self as LockFreeOverlay<ByteKey, V, S>>::overlay_route_get_value(self, term)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn byte_create_flips_all_v_to_overlay() {
        // L3.3: the lock-free overlay is byte's SOLE representation — every
        // constructor installs it, so a fresh `create::<V>()` is overlay-routed
        // (`route_overlay()==true`) for ALL `V` (the owned tree is gone). Pins the
        // overlay-routing default across membership (`()`), counter (`i64`), and
        // arbitrary (`String`) value types.
        use crate::persistent_artrie::PersistentARTrie;
        std::fs::create_dir_all("target/test-tmp").ok();
        let dir = tempfile::Builder::new()
            .prefix("byte-m4b-create-flip")
            .tempdir_in("target/test-tmp")
            .expect("scratch tempdir under target/test-tmp");
        // Eligible V = (): the create-flip routes to the overlay.
        let path_unit = dir.path().join("unit.part");
        let trie_unit = PersistentARTrie::<()>::create(&path_unit).expect("create<()>");
        assert!(
            trie_unit.route_overlay(),
            "M4b: a fresh create::<()>() must flip to the overlay (route_overlay true)"
        );
        // Eligible V = i64: the create-flip routes to the overlay.
        let path_i64 = dir.path().join("i64.part");
        let trie_i64 = PersistentARTrie::<i64>::create(&path_i64).expect("create<i64>");
        assert!(
            trie_i64.route_overlay(),
            "M4b: a fresh create::<i64>() must flip to the overlay (route_overlay true)"
        );
        // Arbitrary V = String is overlay-eligible (the default): a fresh
        // `create::<String>()` create-flips to the overlay.
        let path_str = dir.path().join("str.part");
        let trie_str = PersistentARTrie::<String>::create(&path_str).expect("create<String>");
        assert!(
            trie_str.route_overlay(),
            "arbitrary V (String) create-flips to the overlay (default)"
        );
    }

    #[test]
    fn byte_eligible_v_gate() {
        // Arbitrary-V overlay routing is the default: ANY `V` is overlay-eligible.
        // The byte counter monomorph is now `u64` (matching char, post-u64-
        // restoration); `()` is membership, and every other `V` (incl. `i64` and
        // `String`) is arbitrary-V — all eligible.
        use crate::persistent_artrie::PersistentARTrie;
        assert!(PersistentARTrie::<()>::overlay_eligible_v());
        assert!(PersistentARTrie::<u64>::overlay_eligible_v());
        assert!(PersistentARTrie::<i64>::overlay_eligible_v());
        assert!(PersistentARTrie::<String>::overlay_eligible_v());
    }

    #[test]
    fn byte_arbitrary_v_create_flips_to_overlay() {
        // Arbitrary-V overlay routing is the default: `String` is eligible, so a
        // fresh `create::<String>()` is overlay-routed (the overlay is the sole
        // representation since L3.3 deleted the owned tree).
        use crate::persistent_artrie::PersistentARTrie;
        std::fs::create_dir_all("target/test-tmp").ok();
        let dir = tempfile::Builder::new()
            .prefix("byte-arbitrary-v")
            .tempdir_in("target/test-tmp")
            .expect("scratch tempdir under target/test-tmp");
        let path = dir.path().join("t.part");
        let trie = PersistentARTrie::<String>::create(&path).expect("create");
        assert!(
            trie.route_overlay(),
            "a String trie create-flips to the overlay (default)"
        );
    }
}
