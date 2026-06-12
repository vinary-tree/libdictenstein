//! **F5 — the generic, compression-aware dense→overlay builder.**
//!
//! Builds an `Arc<OverlayNode<K, V>>` (the un-path-compressed lock-free overlay
//! representation) from an enumeration of `(units, Option<V>, is_final)` terms — the
//! form the per-variant OWNED readers (`owned_units_under` /
//! `owned_units_with_values_under` / `owned_has_empty_term_value`, all D1 un-routed and
//! **compression-aware**: they expand `StringBucket` suffixes + compressed ART-node
//! prefixes) already produce.
//!
//! # Why term-enumeration (not a node-structural walk)
//!
//! An Overlay-regime dense image is USUALLY un-path-compressed (the overlay serializer
//! writes one node per unit), but a **COMPACTED** Overlay file is path-compressed (C-opt-1:
//! `compact()` rebuilds a dense owned image via the owned-staging path, which uses buckets
//! + compressed prefixes, then re-stamps the Overlay regime). So a node-structural
//! converter operating on the raw owned nodes would have to re-implement bucket/prefix
//! expansion — new, subtle, data-loss-critical code. Instead we build from the
//! already-expanded `(term-units, value)` enumeration the proven owned readers yield, so
//! the compression handling lives ONCE in the existing readers.
//!
//! # Two phases, both deep-term safe (NO recursion with key length)
//!
//! 1. **Insert** each `(units, Option<V>)` into a mutable [`OverlayBuilderNode`] tree —
//!    an explicit per-unit loop (no recursion). The builder tree is un-path-compressed
//!    (one node per unit), mirroring the overlay.
//! 2. **Convert** the builder tree to `Arc<OverlayNode<K, V>>` bottom-up on an explicit
//!    work-stack (the iterative post-order of `inner_to_overlay` per node:
//!    `as_final`/`with_value`/`Child::InMem`). `OverlayNode::Drop` is itself iterative,
//!    so the deep result also drops cleanly; the builder tree's `Drop` is the
//!    `BTreeMap`/`Box` default — also a concern at extreme depth, so the builder uses an
//!    explicit iterative `Drop` too (below).

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::persistent_artrie::core::key_encoding::KeyEncoding;
use crate::persistent_artrie::core::overlay::node::{Child, OverlayNode};
use crate::value::DictionaryValue;

/// A node in the mutable F5 builder tree (un-path-compressed: one node per unit).
struct OverlayBuilderNode<K: KeyEncoding, V> {
    is_final: bool,
    /// `Some(v)` iff a VALUE was set for this term (membership-only finals leave it
    /// `None`, exactly as the overlay's `as_final`-without-`with_value`).
    value: Option<V>,
    /// Children keyed by the single edge unit (sorted — `BTreeMap` — so the converted
    /// overlay's child order matches `iter_children` ascending, the production order).
    children: BTreeMap<K::Unit, Box<OverlayBuilderNode<K, V>>>,
}

impl<K: KeyEncoding, V> OverlayBuilderNode<K, V> {
    fn new() -> Self {
        Self {
            is_final: false,
            value: None,
            children: BTreeMap::new(),
        }
    }
}

// Iterative Drop — a deep builder spine (one node per unit) would otherwise overflow the
// stack on the compiler-generated recursive drop (drop node → drop its `children` map →
// drop each `Box<child>` → recurse). Flatten the descent onto a heap worklist. Zero
// `unsafe`; each `Box` has exactly one owner, so no double-free / leak.
impl<K: KeyEncoding, V> Drop for OverlayBuilderNode<K, V> {
    fn drop(&mut self) {
        let mut worklist: Vec<Box<OverlayBuilderNode<K, V>>> = Vec::new();
        for (_, child) in std::mem::take(&mut self.children) {
            worklist.push(child);
        }
        while let Some(mut node) = worklist.pop() {
            for (_, child) in std::mem::take(&mut node.children) {
                worklist.push(child);
            }
            // `node` drops here with an empty `children` map → no recursion.
        }
    }
}

/// **F5 — build the overlay root from an owned-term enumeration.**
///
/// `terms` yields `(units, Option<V>)` for every owned term: `Some(v)` for a valued term
/// (SET the value), `None` for a term-only member (membership only). `empty_term` is the
/// empty term `""`'s state: `Some(Some(v))` = valued "", `Some(None)` = term-only "",
/// `None` = "" absent. (The empty term has no first unit, so it is passed separately —
/// it sets the ROOT's finality/value, mirroring `reestablish_overlay_*`'s root publish.)
///
/// Returns the built `Arc<OverlayNode<K, V>>` (an EMPTY root if there are no terms).
/// Deep-term safe (both phases iterative). Generic over `V` (no counter specialization).
pub(crate) fn build_overlay_root_from_terms<K, V, I>(
    terms: I,
    empty_term: Option<Option<V>>,
) -> Arc<OverlayNode<K, V>>
where
    K: KeyEncoding,
    V: DictionaryValue,
    I: IntoIterator<Item = (Vec<K::Unit>, Option<V>)>,
{
    // ---- Phase 1: build the mutable builder tree (per-unit loop, no recursion). ----
    let mut root = OverlayBuilderNode::<K, V>::new();

    // Empty term "" sets the ROOT directly.
    if let Some(empty_value) = empty_term {
        root.is_final = true;
        root.value = empty_value; // Some(v) for valued "", None for membership "".
    }

    for (units, value) in terms {
        let mut cur = &mut root;
        for unit in units {
            cur = cur
                .children
                .entry(unit)
                .or_insert_with(|| Box::new(OverlayBuilderNode::<K, V>::new()));
        }
        cur.is_final = true;
        // SET the value iff the term carries one; a term-only member leaves it None.
        // (A later valued entry for the same term overwrites — last-writer; but the
        // owned enumeration yields each term once, so there is no intra-build contention.)
        if value.is_some() {
            cur.value = value;
        }
    }

    // ---- Phase 2: convert the builder tree → Arc<OverlayNode>, bottom-up (iterative
    // post-order work-stack — the inverse of the recursive build). ----
    convert_builder_to_overlay::<K, V>(root)
}

/// Convert a mutable [`OverlayBuilderNode`] tree into `Arc<OverlayNode<K, V>>`, bottom-up,
/// on an explicit work-stack (deep-term safe). Per node: a fresh `OverlayNode` with the
/// same finality (`as_final`), the same `Option<V>` value (`with_value`), and one
/// `Child::InMem(Arc<built_child>)` per edge in ascending order.
fn convert_builder_to_overlay<K, V>(root: OverlayBuilderNode<K, V>) -> Arc<OverlayNode<K, V>>
where
    K: KeyEncoding,
    V: DictionaryValue,
{
    /// A pending child slot in a parent frame: a unit edge awaiting the `Arc` its subtree
    /// will produce.
    struct Pending<K: KeyEncoding, V: DictionaryValue> {
        edge: K::Unit,
        built: Option<Arc<OverlayNode<K, V>>>,
    }
    /// A work-stack frame: one builder node mid-conversion. Owns its node's finality +
    /// value (moved out) and the children still to descend into.
    struct Frame<K: KeyEncoding, V: DictionaryValue> {
        is_final: bool,
        value: Option<V>,
        parent_edge: Option<K::Unit>,
        /// Children still to descend (drained from the builder node's map), in REVERSE
        /// ascending order so `pop()` yields ascending.
        pending_children: Vec<(K::Unit, Box<OverlayBuilderNode<K, V>>)>,
        /// Resolved child slots (ascending edge order).
        slots: Vec<Pending<K, V>>,
    }

    fn make_frame<K: KeyEncoding, V: DictionaryValue>(
        mut node: Box<OverlayBuilderNode<K, V>>,
        parent_edge: Option<K::Unit>,
    ) -> Frame<K, V> {
        // Drain children in ascending order, build slots, reverse the descent list.
        let mut pending_children: Vec<(K::Unit, Box<OverlayBuilderNode<K, V>>)> =
            std::mem::take(&mut node.children).into_iter().collect();
        let mut slots: Vec<Pending<K, V>> = Vec::with_capacity(pending_children.len());
        for (edge, _) in &pending_children {
            slots.push(Pending {
                edge: *edge,
                built: None,
            });
        }
        pending_children.reverse(); // ascending via pop()
        Frame {
            is_final: node.is_final,
            value: node.value.take(),
            parent_edge,
            pending_children,
            slots,
        }
    }

    let mut stack: Vec<Frame<K, V>> = Vec::new();
    stack.push(make_frame(Box::new(root), None));
    let mut completed: Option<(K::Unit, Arc<OverlayNode<K, V>>)> = None;

    loop {
        let frame = stack
            .last_mut()
            .expect("F5 builder→overlay: non-empty work-stack");

        if let Some((edge, built)) = completed.take() {
            let slot = frame
                .slots
                .iter_mut()
                .find(|s| s.edge == edge && s.built.is_none())
                .expect("F5 builder→overlay: completed child edge has an unfilled parent slot");
            slot.built = Some(built);
        }

        if let Some((edge, child)) = frame.pending_children.pop() {
            stack.push(make_frame(child, Some(edge)));
            continue;
        }

        // All children built → build THIS node.
        let frame = stack.pop().expect("F5 builder→overlay: frame to finalize");
        let mut node = OverlayNode::<K, V>::new();
        if frame.is_final {
            node = node.as_final();
        }
        if let Some(v) = frame.value {
            node = node.with_value(v);
        }
        for slot in frame.slots {
            let built = slot.built.expect(
                "F5 builder→overlay: every child slot is filled before its parent is built \
                 (post-order invariant)",
            );
            node = node.with_child(slot.edge, Child::InMem(built));
        }
        let node_arc = Arc::new(node);
        match frame.parent_edge {
            Some(edge) => completed = Some((edge, node_arc)),
            None => return node_arc,
        }
    }
}
