//! Byte seam impl of the shared [`LockFreeOverlay`] flip (M2a) + thin inherent
//! wrappers preserving the byte public surface.
//!
//! This is the BYTE twin of `persistent_artrie_char::overlay_write_mode`. The
//! overlay-flip genericization (`docs/design/overlay-durable-architecture.md`,
//! step M2a) extracted the lock-free-overlay flip (route predicate +
//! non-faulting RCU read engine + flip/kill-switch + reestablish folds) into the
//! SHARED GENERIC
//! [`LockFreeOverlay`](crate::persistent_artrie_core::overlay::flip::LockFreeOverlay)
//! trait. This module holds only:
//!
//! 1. a re-export of [`OverlayWriteMode`] (shared in
//!    `persistent_artrie_core::overlay::write_mode`) so any internal
//!    `super::overlay_write_mode::OverlayWriteMode` site resolves;
//! 2. the byte SEAM impl `impl LockFreeOverlay<ByteKey, V, S> for
//!    PersistentARTrie<V, S>` (per-variant owned readers / overlay publishers /
//!    WAL accessors / `CounterValue = i64`);
//! 3. thin inherent wrappers (`route_overlay` / `flip_to_overlay` /
//!    `kill_switch_to_owned` / `set_overlay_write_mode` / `overlay_eligible_v`)
//!    that DELEGATE to the trait, so the byte call sites and the byte
//!    correspondence tests can use inherent syntax.
//!
//! **M2a scope (opt-in, REVERSIBLE).** No production byte ctor flips (the byte
//! field defaults to the inert [`OverlayWriteMode::OwnedTree`]); the durable
//! write/checkpoint skeletons (`DurableOverlayWrite`/`OverlayCheckpoint`) and the
//! production read/write routing are LATER steps (M2b/M3/M4). This module gives
//! byte ONLY the trait DEFAULTS: the route predicate, the read engine
//! (`overlay_len`/`overlay_iter_prefix*`/`overlay_get_value`), the flip /
//! kill-switch, and the no-WAL reestablish folds.
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

// Re-export so `super::overlay_write_mode::OverlayWriteMode` resolves everywhere
// it is used (mirrors the char module's re-export).
pub(crate) use crate::persistent_artrie_core::overlay::write_mode::OverlayWriteMode;

// Only the feature-OFF eligibility branch uses TypeId (the feature-ON branch is
// `true`), so gate the import to avoid an unused-import warning when the feature is on.
#[cfg(not(feature = "overlay-arbitrary-v"))]
use std::any::TypeId;
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
        prefix: Vec<u8>,
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
                is_final, children, ..
            } => {
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
        prefix: Vec<u8>,
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
                is_final,
                value,
                children,
                ..
            } => {
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
                    self.unrouted_collect_terms_with_values_under_child(grandchild, child_prefix, out);
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
        match &self.root {
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
        match &self.root {
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
                        self.unrouted_collect_terms_with_values_under_child(child, vec![*edge], &mut out);
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
        match &self.root {
            TrieRoot::Bucket(bucket) => match bucket.search(&[]) {
                Ok(idx) => bucket
                    .get_entry(idx)
                    .and_then(|entry| bucket.get_value(&entry))
                    .and_then(|value_bytes| {
                        crate::serialization::bincode_compat::deserialize::<V>(value_bytes).ok()
                    }),
                Err(_) => None,
            },
            TrieRoot::ArtNode { is_final, value, .. } => {
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
    /// `i64` — the byte counter monomorph (char's is `u64`). The overlay leaf
    /// stores a non-negative `i64` (bounded by `LOCKFREE_COUNTER_MAX = i64::MAX`),
    /// so the i64↔u64 boundary conversion in `overlay_publish_counter` /
    /// `overlay_counter_get` is lossless.
    type CounterValue = i64;

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
    fn overlay_write_mode(&self) -> OverlayWriteMode {
        self.overlay_write_mode
    }

    #[inline]
    fn set_overlay_write_mode(&mut self, mode: OverlayWriteMode) {
        self.overlay_write_mode = mode;
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

    fn wal_stamp_owned_regime(&self) {
        if let Some(ref writer) = self.wal_writer {
            if let Err(e) = writer.set_owned_regime() {
                log::warn!("kill_switch_to_owned: could not stamp Owned regime: {:?}", e);
            }
        }
    }

    /// The byte eligible monomorphs `{(), i64}` (char's are `{(), u64}`) + F2 (G5).
    ///
    /// With the `overlay-arbitrary-v` Cargo feature, ANY `V: DictionaryValue` is
    /// eligible (the generic value path routes it through the overlay). WITHOUT it
    /// (production default), only `{(), i64}` — production byte-identical until the
    /// feature is deliberately enabled (owner-gated activation, design F2).
    /// `DictionaryValue: 'static` ⇒ `TypeId` is callable.
    fn overlay_eligible_v() -> bool {
        #[cfg(feature = "overlay-arbitrary-v")]
        {
            true
        }
        #[cfg(not(feature = "overlay-arbitrary-v"))]
        {
            TypeId::of::<V>() == TypeId::of::<i64>() || TypeId::of::<V>() == TypeId::of::<()>()
        }
    }

    // ---- UN-ROUTED owned readers (D1 — read the OWNED tree directly) ----

    fn owned_first_units(&self) -> Result<(Vec<u8>, bool)> {
        // Disjoint first-byte cover. D1: `unrouted_collect_under(&[])` is the
        // UN-routed, UNCAPPED owned reader (it walks `self.root`, never the
        // overlay), so it is safe even with `route_overlay()` already true.
        use std::collections::BTreeSet;
        let mut first_units: BTreeSet<u8> = BTreeSet::new();
        let mut has_empty_term = false;
        if let Some(all_terms) = self.unrouted_collect_under(&[]) {
            for term in &all_terms {
                match term.first() {
                    Some(&b) => {
                        first_units.insert(b);
                    }
                    None => has_empty_term = true,
                }
            }
        }
        Ok((first_units.into_iter().collect(), has_empty_term))
    }

    fn owned_units_under(&self, prefix: &[u8]) -> Result<Option<Vec<Vec<u8>>>> {
        // D1: UN-routed, UNCAPPED owned reader. The byte unit IS the public term
        // byte, so no boundary conversion is needed (`ByteKey::units_to_term` is
        // the identity `to_vec`).
        Ok(self.unrouted_collect_under(prefix))
    }

    fn owned_units_with_values_under(&self, prefix: &[u8]) -> Result<Option<Vec<(Vec<u8>, V)>>> {
        // D1: UN-routed, UNCAPPED owned reader.
        Ok(self.unrouted_collect_with_values_under(prefix))
    }

    fn owned_has_empty_term_value(&self) -> Option<V> {
        // D1: UN-routed owned reader. Delegates to the `unrouted_*` helper (which
        // reads `self.root` directly) so this seam body stays free of the bare
        // forbidden tokens the D1 grep gate scans for.
        self.unrouted_empty_term_value()
    }

    fn clear_owned(&mut self) {
        self.root = TrieRoot::Bucket(StringBucket::with_values());
        self.term_count.store(0, Ordering::Release);
    }

    // ---- overlay publishers (the per-variant write seam) ----

    fn overlay_publish_membership(&self, units: &[u8]) {
        // No-WAL CAS insert (recovered terms are already durable in the WAL;
        // re-logging would double-log). `units` IS the byte term.
        self.insert_cas(units);
    }

    fn overlay_publish_counter(&self, units: &[u8], value: i64) {
        // `V == i64` in this routed branch (the counter reestablish runs only for
        // the i64 monomorph via the dispatch). SAFE `Any` downcast to the nameable
        // `<i64, S>` monomorph (where `increment_cas` lives), then the no-WAL
        // increment. The leaf stores i64; the publisher API takes the delta as
        // `u64`, so widen the non-negative i64 count losslessly. A negative here
        // is a bug (the counter is non-negative, bounded by LOCKFREE_COUNTER_MAX;
        // M2b's value-bound rejects negatives upstream) — guard defensively.
        use std::any::Any;
        debug_assert!(
            value >= 0,
            "overlay_publish_counter: negative counter {} for byte term {:?} (counters are non-negative)",
            value,
            units
        );
        if value < 0 {
            log::warn!(
                "overlay_publish_counter: dropping negative counter {} for byte term {:?} (counters are non-negative)",
                value,
                units
            );
            return;
        }
        if let Some(trie_i64) =
            (self as &dyn Any).downcast_ref::<PersistentARTrie<i64, S>>()
        {
            trie_i64.increment_cas(units, value as u64);
        }
    }

    fn overlay_counter_get(&self, units: &[u8]) -> Option<i64> {
        // SAFE `Any` downcast to `<i64, S>` + the lock-free point read.
        // `get_lockfree` returns the count widened to `u64` (non-negative,
        // bounded by i64::MAX), so the narrow back to `i64` is lossless.
        use std::any::Any;
        (self as &dyn Any)
            .downcast_ref::<PersistentARTrie<i64, S>>()
            .and_then(|trie_i64| trie_i64.get_lockfree(units))
            .map(|count| count as i64)
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

    fn bound_increment_delta(&self, key: &str, delta: i64) -> Result<i64> {
        // **C4 (the byte i64 value-domain bound).** The WAL increment domain is
        // `i64` for every variant, and the byte overlay counter is non-negative
        // (the leaf stores a non-negative `i64`, the running sum bounded by
        // `LOCKFREE_COUNTER_MAX = i64::MAX`). `CounterValue = i64`, so a `delta`
        // already CANNOT exceed `i64::MAX` (that is the char `u64` reject, vacuous
        // here); the byte reject is the NEGATIVE delta — a negative would, under the
        // commutative-sum replay, drive the recovered count below 0 (an
        // un-representable state for a count) or silently subtract, so we FAIL LOUD
        // rather than wrap/panic. Returns the bounded `i64` the delta WAL record
        // carries (identity for a valid non-negative delta).
        if delta < 0 {
            return Err(PersistentARTrieError::InvalidOperation(format!(
                "try_increment_cas_durable delta for byte term {:?} must be non-negative \
                 (the overlay counter domain is a non-negative i64); got {}",
                key, delta
            )));
        }
        Ok(delta)
    }

    #[inline]
    fn build_increment_record(&self, key_bytes: &[u8], bounded: i64) -> WalRecord {
        // The delta record the proven byte counter merge path logs: a single-entry,
        // delta-based BatchIncrement (commutative on replay), exactly as char.
        WalRecord::BatchIncrement {
            entries: vec![(key_bytes.to_vec(), bounded)],
        }
    }

    fn increment_publish_inner(&self, key: &str, delta: i64) -> Result<(i64, u64)> {
        // `try_increment_cas_inner` is i64-specialized (`impl<S> ...<i64, S>`), so
        // downcast `self` to the nameable `<i64, S>` monomorph via a SAFE `Any`
        // (the same zero-`unsafe` pattern as `overlay_publish_counter`). The counter
        // durable path runs only for the i64 monomorph (the value route), so this
        // downcast always succeeds there; an ineligible `V` returns the empty result
        // (the durable increment is never reached for non-i64 `V`).
        //
        // Byte's `try_increment_cas_inner` takes `(&[u8], u64)` and returns
        // `(u64, u64)`. `delta` is non-negative here (C4 `bound_increment_delta`
        // ran first), so `delta as u64` is lossless; the returned count is bounded by
        // `LOCKFREE_COUNTER_MAX = i64::MAX`, so the narrow back to `i64` is lossless.
        // `key.as_bytes()` recovers the raw key bytes (byte's durable increment path
        // operates on UTF-8 keys — the public wrapper validates this).
        use std::any::Any;
        match (self as &dyn Any).downcast_ref::<PersistentARTrie<i64, S>>() {
            Some(trie_i64) => {
                let (new_val, generation) =
                    trie_i64.try_increment_cas_inner(key.as_bytes(), delta as u64)?;
                Ok((new_val as i64, generation))
            }
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
                        Some(v) => {
                            Some(crate::serialization::bincode_compat::serialize(v).map_err(|e| {
                                PersistentARTrieError::internal(format!(
                                    "Failed to serialize value: {}",
                                    e
                                ))
                            })?)
                        }
                        None => None,
                    };
                    if &cur_bytes != expected_bytes {
                        return Ok(ValuePublishOutcome::NotApplied);
                    }
                }
            }
            let new_root = match self.build_value_path_recursive(&root, key_bytes, 0, value.clone()) {
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
    /// checkpoint should take the lock-free overlay path for this trie. Thin
    /// delegator to [`LockFreeOverlay::route_overlay`]. **M4b: `pub` (the flip
    /// state predicate — char parity).** Since M4b a fresh `create::<()|i64>()`
    /// create-flips, so this is `true` by default for eligible V (the irreversible
    /// production flip); arbitrary V and a [`kill_switch_to_owned`](Self::kill_switch_to_owned)'d
    /// trie stay `false`.
    #[inline]
    pub fn route_overlay(&self) -> bool {
        <Self as LockFreeOverlay<ByteKey, V, S>>::route_overlay(self)
    }

    /// **Restart-time kill-switch setter.** Thin delegator to the seam.
    #[cfg_attr(not(test), allow(dead_code))]
    #[inline]
    pub(crate) fn set_overlay_write_mode(&mut self, mode: OverlayWriteMode) {
        <Self as LockFreeOverlay<ByteKey, V, S>>::set_overlay_write_mode(self, mode)
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

    /// **Kill-switch — the one-release fallback / owned-path escape hatch.** Thin
    /// delegator to [`LockFreeOverlay::kill_switch_to_owned`]. Reverts
    /// `route_overlay()` to `false` (the owned path) AND restamps the WAL Owned when
    /// the trie is still fresh (`current_lsn() == 1`), so a freshly-created (and thus
    /// create-flipped) eligible-V trie can be fully reverted to the proven owned path.
    /// **M4b: `pub` (char parity)** — the production fallback if the irreversible flip
    /// must be backed out for a given trie, and the way owned-path feature tests
    /// (doc-tx / trie-to-trie merge / compaction / CAS) force the owned regime that
    /// those features require.
    #[inline]
    pub fn kill_switch_to_owned(&mut self) {
        <Self as LockFreeOverlay<ByteKey, V, S>>::kill_switch_to_owned(self)
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
    use super::OverlayWriteMode;

    #[test]
    fn default_is_owned_tree_and_inert() {
        // The byte scaffold MUST default to the proven owned path and report that
        // the overlay is not in use — proving it changes no current behavior.
        assert_eq!(OverlayWriteMode::default(), OverlayWriteMode::OwnedTree);
        assert!(!OverlayWriteMode::default().uses_overlay());
    }

    #[test]
    fn byte_create_flip_eligible_v_routes_ineligible_v_owned() {
        // **M4b REFRAME (was `byte_default_ctor_is_inert_no_route`).** The M4b
        // create-flip made the lock-free overlay byte's production DEFAULT for the
        // eligible monomorphs `{(), i64}`: a fresh `create::<()|i64>()` is now
        // overlay-routed (`route_overlay()==true`). The M2a INERT-default property no
        // longer holds for eligible V — that is the irreversible flip. Arbitrary V
        // (ineligible) STILL stays owned (the flip is a strict no-op there), which this
        // test now pins as the surviving back-compat invariant.
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
        // F2-migrate: Bucket D (cfg-split). Ineligible V = String stays owned WITHOUT
        // `overlay-arbitrary-v`; WITH the feature it becomes eligible and create-flips.
        let path_str = dir.path().join("str.part");
        let trie_str = PersistentARTrie::<String>::create(&path_str).expect("create<String>");
        #[cfg(not(feature = "overlay-arbitrary-v"))]
        assert!(
            !trie_str.route_overlay(),
            "M4b: arbitrary V must NOT flip (stays on the owned path — back-compat)"
        );
        #[cfg(feature = "overlay-arbitrary-v")]
        assert!(
            trie_str.route_overlay(),
            "with overlay-arbitrary-v, arbitrary V (String) create-flips to the overlay"
        );
    }

    #[test]
    fn byte_eligible_v_gate() {
        // The original byte eligible monomorphs are `{(), i64}` (NOT u64 — byte's
        // counter is i64) — always eligible.
        use crate::persistent_artrie::PersistentARTrie;
        assert!(PersistentARTrie::<()>::overlay_eligible_v());
        assert!(PersistentARTrie::<i64>::overlay_eligible_v());

        // F2-migrate: Bucket D (cfg-split). The `overlay-arbitrary-v` feature makes ANY
        // `V` overlay-eligible; WITHOUT it only the `{(), i64}` monomorphs are eligible
        // (arbitrary V — `u64` is byte-arbitrary since byte's counter is i64 — stays
        // owned). Assert the eligibility contract for the active feature config.
        #[cfg(feature = "overlay-arbitrary-v")]
        {
            assert!(PersistentARTrie::<u64>::overlay_eligible_v());
            assert!(PersistentARTrie::<String>::overlay_eligible_v());
        }
        #[cfg(not(feature = "overlay-arbitrary-v"))]
        {
            assert!(!PersistentARTrie::<u64>::overlay_eligible_v());
            assert!(!PersistentARTrie::<String>::overlay_eligible_v());
        }
    }

    #[test]
    fn byte_flip_then_kill_switch_round_trips_route_overlay() {
        // **M4b PRECONDITION UPDATE.** Post-M4b a fresh `create::<()>()` ALREADY
        // create-flips (`route_overlay()==true`), so this round-trip starts from the
        // ROUTED state (not the old M2a inert default). It still proves the
        // kill-switch ↔ flip round-trip: routed → kill-switch → owned → re-flip →
        // routed.
        use crate::persistent_artrie::PersistentARTrie;
        std::fs::create_dir_all("target/test-tmp").ok();
        let dir = tempfile::Builder::new()
            .prefix("byte-m4b-flip")
            .tempdir_in("target/test-tmp")
            .expect("scratch tempdir under target/test-tmp");
        let path = dir.path().join("t.part");
        let mut trie = PersistentARTrie::<()>::create(&path).expect("create");

        assert!(
            trie.route_overlay(),
            "M4b: a fresh create::<()>() already create-flips to the overlay"
        );
        // Kill-switch reverts to owned.
        trie.kill_switch_to_owned();
        assert!(
            !trie.route_overlay(),
            "kill_switch_to_owned must revert to the owned path"
        );
        // Re-flip re-engages the overlay.
        assert!(
            trie.flip_to_overlay(),
            "flip_to_overlay must re-engage the overlay for eligible V=()"
        );
        assert!(trie.route_overlay(), "post-re-flip routes to the overlay");
        // And a second kill-switch reverts again (idempotent round-trip).
        trie.kill_switch_to_owned();
        assert!(!trie.route_overlay(), "second kill-switch reverts to owned");
    }

    #[test]
    fn byte_flip_is_noop_for_ineligible_v() {
        // V-1 gate: `flip_to_overlay` is a NO-OP for arbitrary V (which would
        // otherwise get a write-broken overlay). Arbitrary V stays owned.
        //
        // F2-migrate: Bucket D (cfg-split). WITHOUT `overlay-arbitrary-v`, `String` is
        // ineligible — the flip is a strict no-op and the trie stays owned. WITH the
        // feature, `String` is eligible — a fresh `create::<String>()` already
        // create-flips, so the trie is overlay-routed.
        use crate::persistent_artrie::PersistentARTrie;
        std::fs::create_dir_all("target/test-tmp").ok();
        let dir = tempfile::Builder::new()
            .prefix("byte-m2a-ineligible")
            .tempdir_in("target/test-tmp")
            .expect("scratch tempdir under target/test-tmp");
        let path = dir.path().join("t.part");
        #[cfg(not(feature = "overlay-arbitrary-v"))]
        {
            let mut trie = PersistentARTrie::<String>::create(&path).expect("create");
            assert!(
                !trie.flip_to_overlay(),
                "flip_to_overlay must be a no-op for arbitrary V"
            );
            assert!(
                !trie.route_overlay(),
                "an arbitrary-V trie must stay on the owned path (no broken overlay)"
            );
        }
        #[cfg(feature = "overlay-arbitrary-v")]
        {
            let trie = PersistentARTrie::<String>::create(&path).expect("create");
            assert!(
                trie.route_overlay(),
                "with overlay-arbitrary-v, a String trie create-flips to the overlay"
            );
        }
    }
}
