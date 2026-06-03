//! S5-12 **E1 read-flip** — overlay-backed reads for `PersistentARTrieChar<V, S>`.
//!
//! When `route_overlay()` is true (the production write/read path is the immutable
//! lock-free overlay; `V ∈ {(), u64}` only), the public read methods route here
//! instead of walking the owned tree `self.root` (which is CLEARED on an
//! overlay-regime reopen — `reestablish_overlay_*_after_recovery`). These walks are
//! the read-side symmetry of the Order-A write guards.
//!
//! # NON-FAULTING — DO NOT add disk fault-in
//!
//! Every walk here descends **in-memory children only** (`Child::as_in_mem`), never
//! resolving `Child::OnDisk`. This is deliberate and load-bearing: a faulting read
//! racing a checkpoint/eviction that holds the buffer-manager lock is the
//! lock-ordering inversion that deadlocked the soak for 75+ minutes
//! (`find_leaf_faulting`, lockfree_cas.rs:1276; memory
//! `feedback_production-deadlock-is-costly`). A future maintainer MUST NOT "fix" the
//! eviction-undercount (below) by adding `find_leaf_faulting`/
//! `load_overlay_node_from_disk` to these enumerators.
//!
//! # Resident-finals semantics (E1-iter-A)
//!
//! Because the walks skip `OnDisk` children, `len`/`iter`/`iter_prefix` are
//! **resident-finals / last-checkpoint-consistent**: exact while no overlay node is
//! evicted, and an UNDER-count once overlay eviction runs. Overlay eviction is
//! currently `#[cfg(feature = "bench-internals")]`/test-only (NOT a default-build
//! production path), so the count is exact for this release. Faithful-under-eviction
//! enumeration (descending `OnDisk` via the lazy loader WITHOUT the faulting
//! deadlock) is the E1-iter-B prerequisite that MUST land before overlay eviction is
//! un-gated to production.
//!
//! # MAINTENANCE COUPLING
//!
//! `overlay_count_finals` mirrors `persist::count_overlay_finals`; the value walks
//! mirror `lockfree_cas::collect_lockfree_value_entries_recursive`. Keep in lockstep.

use std::any::{Any, TypeId};
use std::sync::Arc;

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::error::Result;
use crate::value::DictionaryValue;

use super::nodes::persistent_node::PersistentCharNode;

/// `Some(())` re-wrapped as `V` iff `V == ()`, else `None`.
///
/// Membership finals (`V = ()`) carry `value: None` (`OverlayNode::as_final` leaves
/// the value unset), but the owned-tree semantics give every membership term the
/// value `()`. So when iterating values for the `V = ()` monomorph we SYNTHESIZE
/// `()` for each final via a SAFE `Any` re-wrap (the same zero-`unsafe` pattern as
/// `lockfree_value_route`).
#[inline]
fn unit_as_v<V: DictionaryValue>() -> Option<V> {
    let unit = ();
    (&unit as &dyn Any).downcast_ref::<V>().cloned()
}

impl<V: DictionaryValue, S: BlockStorage> super::PersistentARTrieChar<V, S> {
    /// The current immutable overlay root (a hazard-protected `Arc` snapshot), or
    /// `None` if the lock-free overlay is not installed.
    #[inline]
    fn overlay_root(&self) -> Option<Arc<PersistentCharNode<V>>> {
        self.lockfree_root.as_ref().and_then(|r| r.load())
    }

    // ===== count / emptiness (back `len`/`term_count`/`is_empty`) =====

    /// Term count of the overlay (number of finalized nodes). Resident-finals only
    /// (see the module doc). `self.len` tracks the owned tree, which is empty/cleared
    /// under the overlay regime — hence this walk.
    pub(crate) fn overlay_len(&self) -> usize {
        match self.overlay_root() {
            Some(root) => Self::overlay_count_finals(&root) as usize,
            None => 0,
        }
    }

    /// Mirrors `persist::count_overlay_finals` (keep in lockstep). Recurses by key
    /// length (the overlay is un-path-compressed); depth-safe at the same bound as
    /// the production lock-free point reads.
    fn overlay_count_finals(node: &Arc<PersistentCharNode<V>>) -> u64 {
        let mut count = u64::from(node.is_final());
        for (_, child) in node.iter_children() {
            if let Some(child_arc) = child.as_in_mem() {
                count += Self::overlay_count_finals(child_arc);
            }
        }
        count
    }

    /// Cheap emptiness check — an early-out "any final?" walk, NOT `overlay_len() ==
    /// 0` (which would be O(N) on a large overlay).
    pub(crate) fn overlay_is_empty(&self) -> bool {
        match self.overlay_root() {
            Some(root) => !Self::overlay_has_final(&root),
            None => true,
        }
    }

    fn overlay_has_final(node: &Arc<PersistentCharNode<V>>) -> bool {
        if node.is_final() {
            return true;
        }
        for (_, child) in node.iter_children() {
            if let Some(child_arc) = child.as_in_mem() {
                if Self::overlay_has_final(child_arc) {
                    return true;
                }
            }
        }
        false
    }

    // ===== prefix navigation + collection (back `iter`/`iter_prefix*`) =====

    /// Descend the overlay to the node at `prefix`, in-memory only. Returns `None`
    /// iff a prefix code point has no in-memory edge — reproducing the owned
    /// `navigate_to_prefix` `Ok(None)` (absent path) vs `Ok(Some(_))` (present node,
    /// possibly childless) distinction. The empty prefix yields the root.
    fn overlay_navigate_prefix(&self, prefix: &str) -> Option<Arc<PersistentCharNode<V>>> {
        let mut node = self.overlay_root()?;
        for c in prefix.chars() {
            let child = node.find_child(c as u32)?;
            let child_arc = child.as_in_mem()?;
            node = Arc::clone(child_arc);
        }
        Some(node)
    }

    /// Overlay analogue of `iter_prefix`: `Ok(None)` if the prefix path is absent,
    /// else `Ok(Some(terms))` (possibly empty). Matches owned DFS pre-order.
    pub(crate) fn overlay_iter_prefix(&self, prefix: &str) -> Result<Option<Vec<String>>> {
        match self.overlay_navigate_prefix(prefix) {
            None => Ok(None),
            Some(node) => {
                let mut terms = Vec::new();
                Self::overlay_collect_finals(&node, prefix.to_string(), &mut terms);
                Ok(Some(terms))
            }
        }
    }

    /// Pre-order DFS mirroring the owned `collect_terms_under_node` (final-first,
    /// then children in key order). Recurses by key length; depth-safe at the
    /// production point-read bound. (A heap work-stack would lift the bound entirely
    /// — recommended defense-in-depth, deferred since there is no new crash risk.)
    fn overlay_collect_finals(
        node: &Arc<PersistentCharNode<V>>,
        prefix: String,
        out: &mut Vec<String>,
    ) {
        if node.is_final() {
            out.push(prefix.clone());
        }
        for (key, child) in node.iter_children() {
            if let Some(child_arc) = child.as_in_mem() {
                let c = char::from_u32(*key).unwrap_or('\u{FFFD}');
                let mut child_prefix = prefix.clone();
                child_prefix.push(c);
                Self::overlay_collect_finals(child_arc, child_prefix, out);
            }
        }
    }

    /// Overlay analogue of `iter_prefix_with_values`. For `V = u64` each final's
    /// value is its counter (`get_value`); for `V = ()` each final's value is the
    /// synthesized `()` (membership finals carry no stored value — see `unit_as_v`).
    pub(crate) fn overlay_iter_prefix_with_values(
        &self,
        prefix: &str,
    ) -> Result<Option<Vec<(String, V)>>> {
        match self.overlay_navigate_prefix(prefix) {
            None => Ok(None),
            Some(node) => {
                let mut entries = Vec::new();
                Self::overlay_collect_with_values(&node, prefix.to_string(), &mut entries);
                Ok(Some(entries))
            }
        }
    }

    fn overlay_collect_with_values(
        node: &Arc<PersistentCharNode<V>>,
        prefix: String,
        out: &mut Vec<(String, V)>,
    ) {
        if node.is_final() {
            // `get_value()` for a `u64` final is `Some(counter)`; for a `()` final it
            // is `None`, so fall back to the synthesized `()` for the `V == ()`
            // monomorph. For an ineligible `V` (never `route_overlay()`) both are
            // `None` and the final is skipped — harmless, as this path is unreachable
            // for ineligible `V`.
            if let Some(value) = node.get_value().or_else(unit_as_v::<V>) {
                out.push((prefix.clone(), value));
            }
        }
        for (key, child) in node.iter_children() {
            if let Some(child_arc) = child.as_in_mem() {
                let c = char::from_u32(*key).unwrap_or('\u{FFFD}');
                let mut child_prefix = prefix.clone();
                child_prefix.push(c);
                Self::overlay_collect_with_values(child_arc, child_prefix, out);
            }
        }
    }

    // ===== value point-read (backs `get_value`) =====

    /// Route `get_value(term)` to the overlay for `V ∈ {u64, ()}` via a SAFE `Any`
    /// dispatch (the `lockfree_value_route` pattern; zero `unsafe`). Returns:
    /// - `Some(Some(v))` — the term is present with value `v` (the `u64` counter, or
    ///   `()` for membership), re-wrapped as `V`;
    /// - `Some(None)` — handled by the overlay, term absent;
    /// - `None` — `V` is neither `u64` nor `()` (arbitrary `V`); the caller runs its
    ///   owned-tree body. (Unreachable under `route_overlay()`, which is gated to the
    ///   eligible monomorphs, but kept as a correct fall-through.)
    pub(crate) fn overlay_get_value(&self, term: &str) -> Option<Option<V>>
    where
        S: 'static,
    {
        // `V == u64`: read the counter via the lock-free point read, re-wrap as `V`.
        if let Some(trie_u64) =
            (self as &dyn Any).downcast_ref::<super::PersistentARTrieChar<u64, S>>()
        {
            let v = trie_u64.get_lockfree(term);
            return Some(v.map(|u| {
                let any: &dyn Any = &u;
                any.downcast_ref::<V>()
                    .cloned()
                    .expect("V == u64 in this routed branch")
            }));
        }
        // `V == ()`: membership — present ⇒ `Some(())` re-wrapped as `V`.
        if TypeId::of::<V>() == TypeId::of::<()>() {
            return Some(if self.contains_lockfree(term) {
                unit_as_v::<V>()
            } else {
                None
            });
        }
        None
    }
}
