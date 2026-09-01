//! `OverlayDictionaryNode<K, V>` — the shared, key-encoding-generic overlay-backed
//! [`DictionaryNode`] handle (G5.1 unification).
//!
//! The byte (`u8`) and char (`u32`) lock-free overlays used to carry
//! token-for-token-identical `DictionaryNode` handles that differed ONLY in the key
//! encoding (`ByteKey` vs `CharKey`) and the public unit type they presented (`u8`
//! vs `char`). G5.1 collapses both into this single generic handle, parameterized
//! over `K: KeyEncoding`:
//!
//! ```text
//! // byte:  pub type PersistentARTrieNode<V = ()>     = OverlayDictionaryNode<ByteKey, V>;
//! // char:  pub type PersistentARTrieCharNode<V = ()> = OverlayDictionaryNode<CharKey, V>;
//! ```
//!
//! The public [`DictionaryNode::Unit`] is `K::Token` (`u8` for byte, `char` for
//! char), so the public surface — and the transducer / zipper integration that
//! depends on `Unit = u8` / `Unit = char` — is byte-for-byte preserved. The handle
//! stores the compact internal `K::Unit` in the overlay child map and converts at
//! the public boundary via [`KeyEncoding::token_to_unit`] /
//! [`KeyEncoding::unit_to_token`].
//!
//! # Thread safety (the −2 unsafe delta)
//!
//! The handle holds ONLY owned `Arc`s: an `Arc<OverlayNode<K, V>>` snapshot
//! (immutable + reference-counted, so descent needs no pin / no `unsafe` — the `Arc`
//! keeps the node + its in-memory subtree alive regardless of the trie's fate) and
//! an optional `Arc<dyn OverlayFaulter<K, V>>`. Because [`OverlayFaulter`] carries a
//! `Send + Sync` supertrait bound (see `faulter.rs`), `Arc<dyn OverlayFaulter<K, V>>`
//! is itself `Send + Sync`, so this struct **auto-derives** `Send`/`Sync`. The two
//! hand-written `unsafe impl Send/Sync for PersistentARTrieCharNode` the char variant
//! used to need (because its prior bespoke handle had no such supertrait route) are
//! therefore deleted — a clean `−2` against the strict unsafe-inventory set-equality
//! gate, with ZERO new `unsafe` introduced.
//!
//! Lives in `persistent_artrie::core` so the layering invariant holds: it imports the
//! shared [`OverlayNode`] / [`OverlayFaulter`] (canonical here) and the crate-root
//! [`DictionaryNode`] / [`MappedDictionaryNode`] traits with **zero** upward
//! reference to a variant module.

use std::sync::{Arc, OnceLock};

use crate::persistent_artrie::core::key_encoding::KeyEncoding;
use crate::persistent_artrie::core::overlay::node::{Child, OverlayNode};
use crate::persistent_artrie::core::overlay::OverlayFaulter;
use crate::value::DictionaryValue;
use crate::{DictionaryNode, MappedDictionaryNode, SnapshotTraversalCursor};

/// Immutable, revision-scoped dense projection of one retained overlay root.
///
/// Overlay nodes are already immutable path-copy nodes, but their child slots can
/// name durable nodes that have not yet been faulted into memory. Building this
/// arena once gives query schedulers a compact cursor with direct transition and
/// paging while retaining every resolved child `Arc` for the lifetime of the
/// captured root. Nodes are deliberately recorded once per *path*, rather than
/// deduplicated by pointer: the overlay is a trie and this preserves exact
/// root-relative key reconstruction even if a future storage optimization shares
/// an immutable subtree.
struct OverlaySnapshotCursorArena<K: KeyEncoding, V: DictionaryValue> {
    nodes: Vec<OverlaySnapshotCursorNode<K, V>>,
    edges: Box<[(K::Token, usize)]>,
}

struct OverlaySnapshotCursorNode<K: KeyEncoding, V: DictionaryValue> {
    overlay: Arc<OverlayNode<K, V>>,
    edge_start: usize,
    edge_len: usize,
    parent: Option<(usize, K::Token)>,
    is_final: bool,
    value: Option<V>,
}

impl<K: KeyEncoding, V: DictionaryValue> OverlaySnapshotCursorArena<K, V> {
    fn build(
        root: Arc<OverlayNode<K, V>>,
        overlay_faulter: &Option<Arc<dyn OverlayFaulter<K, V>>>,
    ) -> Self {
        let mut nodes = vec![OverlaySnapshotCursorNode {
            is_final: root.is_final(),
            value: root.get_value(),
            overlay: root,
            edge_start: 0,
            edge_len: 0,
            parent: None,
        }];
        let mut node_edges = vec![Vec::new()];

        // Breadth-first materialization keeps construction stack-safe for very
        // deep keys. Child iteration is already lexicographic, so every retained
        // edge slice has the exact ordering of the overlay's native edge store.
        let mut node_index = 0usize;
        while node_index < nodes.len() {
            let overlay = Arc::clone(&nodes[node_index].overlay);
            let mut edges = Vec::with_capacity(overlay.num_children());
            for (&unit, child) in overlay.iter_children() {
                let Some(label) = K::unit_to_token(unit) else {
                    continue;
                };
                let Some(child_overlay) =
                    OverlayDictionaryNode::<K, V>::resolve_overlay_child(child, overlay_faulter)
                else {
                    continue;
                };
                let child_index = nodes.len();
                nodes.push(OverlaySnapshotCursorNode {
                    is_final: child_overlay.is_final(),
                    value: child_overlay.get_value(),
                    overlay: child_overlay,
                    edge_start: 0,
                    edge_len: 0,
                    parent: Some((node_index, label)),
                });
                node_edges.push(Vec::new());
                edges.push((label, child_index));
            }
            node_edges[node_index] = edges;
            node_index += 1;
        }

        // Flatten once so cursor paging is a direct slice and cursor capture
        // performs one edge allocation rather than one allocation per branch.
        let edge_count = node_edges.iter().map(Vec::len).sum();
        let mut flat_edges = Vec::with_capacity(edge_count);
        for (node, edges) in nodes.iter_mut().zip(node_edges) {
            node.edge_start = flat_edges.len();
            node.edge_len = edges.len();
            flat_edges.extend(edges);
        }

        Self {
            nodes,
            edges: flat_edges.into_boxed_slice(),
        }
    }

    #[inline]
    fn node(&self, cursor: SnapshotTraversalCursor) -> Option<&OverlaySnapshotCursorNode<K, V>> {
        self.nodes.get(cursor.index())
    }

    #[inline]
    fn node_edges(&self, node: &OverlaySnapshotCursorNode<K, V>) -> &[(K::Token, usize)] {
        &self.edges[node.edge_start..node.edge_start + node.edge_len]
    }
}

/// Shared overlay-backed [`DictionaryNode`] handle, generic over the key encoding
/// `K` (`ByteKey` / `CharKey`) and the value `V`.
///
/// `Clone` is derived (both fields are `Clone`); `Debug` is hand-written (below)
/// because `Arc<dyn OverlayFaulter<K, V>>` is not `Debug`. `Send`/`Sync`
/// auto-derive (see the module doc).
#[derive(Clone)]
pub struct OverlayDictionaryNode<K: KeyEncoding, V: DictionaryValue = ()> {
    /// Owned overlay node snapshot — the handle navigates the lock-free overlay
    /// (returned by `root()`). The `Arc` keeps the node + its in-memory subtree
    /// alive, so descent needs no pin / no `unsafe`. `Some` for every constructed
    /// handle; `None` is the inert default a method returns its empty value for.
    overlay: Option<Arc<OverlayNode<K, V>>>,
    /// SAFE fault-in capability for `Child::OnDisk` overlay children, or `None` for a
    /// resident-only walk (the inherent `&self` `root()`, where eviction — hence an
    /// OnDisk overlay child — is impossible). `Arc<dyn ..>` (owned), so it keeps the
    /// trie alive for the walk and clones cheaply. No raw pointer, no `unsafe`.
    overlay_faulter: Option<Arc<dyn OverlayFaulter<K, V>>>,
    /// Lazily built direct-cursor arena for this exact retained root. Clones of
    /// this handle share the arena; a descended/materialized node starts a new
    /// arena so its root-relative keys never include labels above that subtree.
    snapshot_cursor_arena: Arc<OnceLock<OverlaySnapshotCursorArena<K, V>>>,
}

// Hand-written `Debug` (the derived one cannot see through `Arc<dyn OverlayFaulter>`):
// summarize whichever arm is active without recursing or dereferencing raw pointers.
impl<K: KeyEncoding, V: DictionaryValue> std::fmt::Debug for OverlayDictionaryNode<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OverlayDictionaryNode")
            .field("overlay", &self.overlay)
            .field("has_faulter", &self.overlay_faulter.is_some())
            .field(
                "has_snapshot_cursor_arena",
                &self.snapshot_cursor_arena.get().is_some(),
            )
            .finish()
    }
}

impl<K: KeyEncoding, V: DictionaryValue> OverlayDictionaryNode<K, V> {
    /// Create an **overlay-backed** root node (the node returned by `root()`).
    /// Navigates the lock-free overlay lazily. `overlay_faulter` is the SAFE fault-in
    /// capability for `Child::OnDisk` overlay children (or `None` for a resident-only
    /// walk).
    pub(crate) fn from_overlay_root(
        node: Arc<OverlayNode<K, V>>,
        overlay_faulter: Option<Arc<dyn OverlayFaulter<K, V>>>,
    ) -> Self {
        Self {
            overlay: Some(node),
            overlay_faulter,
            snapshot_cursor_arena: Arc::new(OnceLock::new()),
        }
    }

    /// Create an overlay child node, inheriting the parent's overlay faulter.
    pub(crate) fn from_overlay_node(
        node: Arc<OverlayNode<K, V>>,
        overlay_faulter: Option<Arc<dyn OverlayFaulter<K, V>>>,
    ) -> Self {
        Self {
            overlay: Some(node),
            overlay_faulter,
            snapshot_cursor_arena: Arc::new(OnceLock::new()),
        }
    }

    #[inline]
    fn resolve_overlay_child(
        child: &Child<K, V>,
        overlay_faulter: &Option<Arc<dyn OverlayFaulter<K, V>>>,
    ) -> Option<Arc<OverlayNode<K, V>>> {
        if let Some(child_arc) = child.as_in_mem() {
            return Some(Arc::clone(child_arc));
        }
        let on_disk = child.as_on_disk()?;
        if on_disk.is_null() {
            return None;
        }
        overlay_faulter.as_ref()?.fault_overlay_slot(on_disk)
    }

    /// Resolve an overlay child slot into a child overlay node, faulting a
    /// `Child::OnDisk` slot in via `overlay_faulter` (never dropping it). Returns
    /// `None` for a null/absent slot, or an OnDisk slot that cannot be faulted in
    /// (no faulter / I/O error) — the same conservative degrade the production
    /// point-read uses (liveness-only, never a fabricated term).
    pub(crate) fn overlay_child_node(
        child: &Child<K, V>,
        overlay_faulter: &Option<Arc<dyn OverlayFaulter<K, V>>>,
    ) -> Option<Self> {
        Self::resolve_overlay_child(child, overlay_faulter)
            .map(|node| Self::from_overlay_node(node, overlay_faulter.clone()))
    }

    #[inline]
    fn snapshot_cursor_arena(&self) -> Option<&OverlaySnapshotCursorArena<K, V>> {
        let root = Arc::clone(self.overlay.as_ref()?);
        Some(
            self.snapshot_cursor_arena
                .get_or_init(|| OverlaySnapshotCursorArena::build(root, &self.overlay_faulter)),
        )
    }
}

impl<K: KeyEncoding, V: DictionaryValue> DictionaryNode for OverlayDictionaryNode<K, V> {
    /// The PUBLIC unit a caller (transducer / zipper) traverses by — `K::Token`
    /// (`u8` for byte, `char` for char). The internal overlay child map is keyed by
    /// the compact `K::Unit`; this handle converts at the boundary.
    type Unit = K::Token;
    type SnapshotCursor = crate::SnapshotTraversalCursor;
    type SnapshotGraphValueHandle = crate::SnapshotTraversalCursor;

    #[inline]
    fn snapshot_root_cursor(&self) -> Option<Self::SnapshotCursor> {
        let arena = self.snapshot_cursor_arena()?;
        (!arena.nodes.is_empty())
            .then(|| SnapshotTraversalCursor::from_index(0))
            .flatten()
    }

    #[inline]
    fn snapshot_cursor_requires_full_projection(&self) -> bool {
        self.snapshot_cursor_arena.get().is_none()
    }

    #[inline]
    fn contains_snapshot_cursor(&self, cursor: Self::SnapshotCursor) -> bool {
        self.snapshot_cursor_arena
            .get()
            .is_some_and(|arena| cursor.index() < arena.nodes.len())
    }

    #[inline]
    fn supports_snapshot_cursor_nodes(&self) -> bool {
        true
    }

    #[inline]
    fn supports_snapshot_cursor_key_units(&self) -> bool {
        true
    }

    #[inline]
    unsafe fn snapshot_cursor_key_units(
        &self,
        cursor: Self::SnapshotCursor,
    ) -> Option<Vec<Self::Unit>> {
        let arena = self.snapshot_cursor_arena.get()?;
        let mut index = cursor.index();
        arena.nodes.get(index)?;
        let mut reverse = Vec::new();
        while let Some((parent, label)) = arena.nodes[index].parent {
            reverse.push(label);
            index = parent;
        }
        reverse.reverse();
        Some(reverse)
    }

    #[inline]
    unsafe fn snapshot_cursor_node(&self, cursor: Self::SnapshotCursor) -> Option<Self> {
        let node = self.snapshot_cursor_arena.get()?.node(cursor)?;
        Some(Self::from_overlay_node(
            Arc::clone(&node.overlay),
            self.overlay_faulter.clone(),
        ))
    }

    #[inline]
    unsafe fn filter_map_snapshot_cursor_edges_and_finality<T, P, F>(
        &self,
        cursor: Self::SnapshotCursor,
        mut project: P,
        mut visitor: F,
    ) -> Option<bool>
    where
        P: FnMut(Self::Unit) -> Option<T>,
        F: FnMut(Self::Unit, Self::SnapshotCursor, T),
    {
        let arena = self.snapshot_cursor_arena.get()?;
        let node = arena.node(cursor)?;
        for &(label, child_index) in arena.node_edges(node) {
            let Some(projected) = project(label) else {
                continue;
            };
            visitor(
                label,
                SnapshotTraversalCursor::from_index(child_index)?,
                projected,
            );
        }
        Some(node.is_final)
    }

    #[inline]
    unsafe fn snapshot_cursor_is_final(&self, cursor: Self::SnapshotCursor) -> Option<bool> {
        Some(self.snapshot_cursor_arena.get()?.node(cursor)?.is_final)
    }

    #[inline]
    unsafe fn snapshot_cursor_transition(
        &self,
        cursor: Self::SnapshotCursor,
        wanted: Self::Unit,
    ) -> Option<Option<Self::SnapshotCursor>> {
        let arena = self.snapshot_cursor_arena.get()?;
        let node = arena.node(cursor)?;
        let edges = arena.node_edges(node);
        let found = edges
            .binary_search_by_key(&wanted, |&(label, _)| label)
            .ok()
            .and_then(|edge_index| SnapshotTraversalCursor::from_index(edges[edge_index].1));
        Some(found)
    }

    #[inline]
    fn supports_efficient_snapshot_cursor_edge_paging(&self) -> bool {
        true
    }

    #[inline]
    unsafe fn visit_snapshot_cursor_edge_page<F>(
        &self,
        cursor: Self::SnapshotCursor,
        start: usize,
        capacity: usize,
        mut visitor: F,
    ) -> Option<(bool, usize)>
    where
        F: FnMut(Self::Unit, Self::SnapshotCursor),
    {
        let arena = self.snapshot_cursor_arena.get()?;
        let node = arena.node(cursor)?;
        let edges = arena.node_edges(node);
        let total = edges.len();
        let end = start.saturating_add(capacity).min(total);
        if start < end {
            for &(label, child_index) in &edges[start..end] {
                visitor(label, SnapshotTraversalCursor::from_index(child_index)?);
            }
        }
        Some((node.is_final, total))
    }

    fn is_final(&self) -> bool {
        // overlay-only: pure owned-`Arc` read, no pin / no `unsafe`.
        match &self.overlay {
            Some(node) => node.is_final(),
            None => false,
        }
    }

    fn transition(&self, label: K::Token) -> Option<Self> {
        // overlay-only: one `K::Unit` edge per overlay child (un-path-compressed).
        // Lower the public token to the internal storage unit, then look it up.
        // InMem ⇒ wrap directly; OnDisk ⇒ fault in via the SAFE overlay faulter
        // (never dropped). No pin / no `unsafe`.
        let node = self.overlay.as_ref()?;
        let child = node.find_child(K::token_to_unit(label))?;
        Self::overlay_child_node(child, &self.overlay_faulter)
    }

    fn edges(&self) -> Box<dyn Iterator<Item = (K::Token, Self)> + '_> {
        // overlay-only: one edge per overlay child slot (InMem direct, OnDisk faulted
        // in — never dropped). Each internal unit is raised back to a public token via
        // `K::unit_to_token`; a unit that is NOT a valid token (a `u32` surrogate —
        // impossible for real char data, total for byte) is SKIPPED, exactly as the
        // prior char `char::from_u32` filter / byte identity did. Preallocated to the
        // known child count. No `unsafe`.
        let Some(node) = &self.overlay else {
            return Box::new(std::iter::empty());
        };
        let mut edges = Vec::with_capacity(node.num_children());
        for (&unit, child) in node.iter_children() {
            let Some(token) = K::unit_to_token(unit) else {
                continue;
            };
            if let Some(child_node) = Self::overlay_child_node(child, &self.overlay_faulter) {
                edges.push((token, child_node));
            }
        }
        Box::new(edges.into_iter())
    }

    #[inline]
    fn for_each_edge<F>(&self, mut visitor: F)
    where
        F: FnMut(K::Token, Self),
    {
        let Some(node) = &self.overlay else {
            return;
        };
        for (&unit, child) in node.iter_children() {
            let Some(token) = K::unit_to_token(unit) else {
                continue;
            };
            if let Some(child_node) = Self::overlay_child_node(child, &self.overlay_faulter) {
                visitor(token, child_node);
            }
        }
    }

    #[inline]
    fn filter_map_edges<T, P, F>(&self, mut project: P, mut visitor: F)
    where
        P: FnMut(K::Token) -> Option<T>,
        F: FnMut(K::Token, Self, T),
    {
        let Some(node) = &self.overlay else {
            return;
        };
        for (&unit, child) in node.iter_children() {
            let Some(token) = K::unit_to_token(unit) else {
                continue;
            };
            let Some(projected) = project(token) else {
                continue;
            };
            // Resolve or fault an on-disk child only after the query automaton
            // accepts its label.
            if let Some(child_node) = Self::overlay_child_node(child, &self.overlay_faulter) {
                visitor(token, child_node, projected);
            }
        }
    }

    fn edge_count(&self) -> Option<usize> {
        // overlay-only: the overlay node's child count is exact and O(1).
        self.overlay.as_ref().map(|node| node.num_children())
    }
}

impl<K: KeyEncoding, V: DictionaryValue> MappedDictionaryNode for OverlayDictionaryNode<K, V> {
    type Value = V;

    /// The value stored at this node (if it terminates a key). Reads the overlay
    /// leaf's `Option<V>` directly (owned `Arc`, no pin / no `unsafe`). For `V = ()`
    /// membership finals this is `None`. This unlocks liblevenshtein's value-aware
    /// transducer queries over the persistent tries.
    fn value(&self) -> Option<V> {
        self.overlay.as_ref().and_then(|node| node.get_value())
    }

    #[inline]
    fn supports_snapshot_cursor_values(&self) -> bool {
        true
    }

    #[inline]
    unsafe fn snapshot_cursor_value(&self, cursor: Self::SnapshotCursor) -> Option<Option<V>> {
        Some(
            self.snapshot_cursor_arena
                .get()?
                .node(cursor)?
                .value
                .clone(),
        )
    }
}

// G5.1 compile-time Send/Sync assertion (the crate has no `static_assertions` dep, so
// this is the trivial generic-fn form). It monomorphizes only when called; the
// `#[allow(dead_code)]` `_assert` invocation below forces that monomorphization at
// compile time WITHOUT running anything. This is the in-crate witness that the
// unified node AUTO-DERIVES `Send + Sync` (so the prior char `unsafe impl`s are
// genuinely unnecessary). The `DictionaryNode: Clone + Send + Sync` supertrait at the
// crate root ALSO transitively requires this, so a regression would already break the
// trait impl above — this assertion just localizes the failure.
#[allow(dead_code)]
fn _assert_overlay_dictionary_node_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    use crate::persistent_artrie::core::key_encoding::{ByteKey, CharKey};
    assert_send_sync::<OverlayDictionaryNode<ByteKey, ()>>();
    assert_send_sync::<OverlayDictionaryNode<ByteKey, u64>>();
    assert_send_sync::<OverlayDictionaryNode<CharKey, ()>>();
    assert_send_sync::<OverlayDictionaryNode<CharKey, u64>>();
}

#[cfg(test)]
mod snapshot_cursor_tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Barrier};

    use super::*;
    use crate::persistent_artrie::core::key_encoding::{ByteKey, CharKey, U64Key};

    fn insert<K: KeyEncoding>(
        root: &Arc<OverlayNode<K, u64>>,
        units: &[K::Unit],
        value: u64,
    ) -> Arc<OverlayNode<K, u64>> {
        if let Some((&head, tail)) = units.split_first() {
            let child = root
                .find_child(head)
                .and_then(Child::as_in_mem)
                .cloned()
                .unwrap_or_else(|| Arc::new(OverlayNode::new()));
            let child = insert(&child, tail, value);
            Arc::new(root.with_child(head, Child::InMem(child)))
        } else {
            Arc::new(root.as_final().with_value(value))
        }
    }

    fn follow<K: KeyEncoding>(
        owner: &OverlayDictionaryNode<K, u64>,
        mut cursor: SnapshotTraversalCursor,
        units: &[K::Token],
    ) -> Option<SnapshotTraversalCursor> {
        for &unit in units {
            // SAFETY: `cursor` starts at this owner and every subsequent cursor
            // is returned by a transition on the same retained revision.
            cursor = unsafe { owner.snapshot_cursor_transition(cursor, unit)? }?;
        }
        Some(cursor)
    }

    fn assert_exact_cursor_surface<K: KeyEncoding>(
        root: Arc<OverlayNode<K, u64>>,
        expected: BTreeMap<Vec<K::Token>, u64>,
    ) {
        let owner = OverlayDictionaryNode::<K, u64>::from_overlay_root(root, None);
        assert!(owner.snapshot_cursor_requires_full_projection());
        let root_cursor = owner.snapshot_root_cursor().expect("direct root cursor");
        assert!(!owner.snapshot_cursor_requires_full_projection());
        assert!(owner.contains_snapshot_cursor(root_cursor));
        assert!(owner.supports_snapshot_cursor_nodes());
        assert!(owner.supports_snapshot_cursor_key_units());
        assert!(owner.supports_snapshot_cursor_values());
        assert!(owner.supports_efficient_snapshot_cursor_edge_paging());

        let mut pending = vec![(Vec::new(), root_cursor)];
        while let Some((path, cursor)) = pending.pop() {
            let expected_value = expected.get(&path).copied();
            // SAFETY: every pending cursor descends from this retained root.
            assert_eq!(
                unsafe { owner.snapshot_cursor_key_units(cursor) },
                Some(path.clone())
            );
            // SAFETY: same retained-revision cursor provenance.
            assert_eq!(
                unsafe { owner.snapshot_cursor_is_final(cursor) },
                Some(expected_value.is_some())
            );
            // SAFETY: same retained-revision cursor provenance.
            assert_eq!(
                unsafe { owner.snapshot_cursor_value(cursor) },
                Some(expected_value)
            );

            // SAFETY: overlay cursors retain an exact node `Arc` for materialization.
            let materialized = unsafe { owner.snapshot_cursor_node(cursor) }.expect("cursor node");
            assert_eq!(materialized.is_final(), expected_value.is_some());
            assert_eq!(materialized.value(), expected_value);

            let mut edges = Vec::new();
            // SAFETY: same retained-revision cursor provenance.
            let finality = unsafe {
                owner.filter_map_snapshot_cursor_edges_and_finality(
                    cursor,
                    Some,
                    |label, child, label_again| {
                        assert_eq!(label, label_again);
                        edges.push((label, child));
                    },
                )
            };
            assert_eq!(finality, Some(expected_value.is_some()));
            assert!(edges.windows(2).all(|pair| pair[0].0 < pair[1].0));

            let mut paged = Vec::new();
            for start in 0..=edges.len() {
                let mut page = Vec::new();
                // SAFETY: same retained-revision cursor provenance.
                let metadata = unsafe {
                    owner.visit_snapshot_cursor_edge_page(cursor, start, 1, |label, child| {
                        page.push((label, child));
                    })
                };
                assert_eq!(metadata, Some((expected_value.is_some(), edges.len())));
                if start < edges.len() {
                    assert_eq!(page, vec![edges[start]]);
                    paged.extend(page);
                } else {
                    assert!(page.is_empty());
                }
            }
            assert_eq!(paged, edges);

            for &(label, child) in edges.iter().rev() {
                // SAFETY: same retained-revision cursor provenance.
                assert_eq!(
                    unsafe { owner.snapshot_cursor_transition(cursor, label) },
                    Some(Some(child))
                );
                let mut child_path = path.clone();
                child_path.push(label);
                pending.push((child_path, child));
            }
        }

        for (term, value) in expected {
            let cursor = follow(&owner, root_cursor, &term).expect("expected term cursor");
            // SAFETY: `follow` retains this owner's cursor provenance.
            assert_eq!(
                unsafe { owner.snapshot_cursor_value(cursor) },
                Some(Some(value))
            );
        }
    }

    #[test]
    fn byte_char_u64_and_vocabulary_overlay_cursors_are_exact_and_ordered() {
        let mut byte_root = Arc::new(OverlayNode::<ByteKey, u64>::new());
        let byte_terms = [
            (b"z".as_slice(), 1),
            (b"ant".as_slice(), 2),
            (b"an".as_slice(), 3),
        ];
        for (term, value) in byte_terms {
            byte_root = insert(&byte_root, term, value);
        }
        assert_exact_cursor_surface::<ByteKey>(
            byte_root,
            BTreeMap::from([
                (b"an".to_vec(), 3),
                (b"ant".to_vec(), 2),
                (b"z".to_vec(), 1),
            ]),
        );

        let mut char_root = Arc::new(OverlayNode::<CharKey, u64>::new());
        let char_terms = [
            (vec!['雪' as u32], vec!['雪'], 5),
            (vec!['a' as u32, 'β' as u32], vec!['a', 'β'], 7),
            (vec!['a' as u32], vec!['a'], 11),
        ];
        let mut char_expected = BTreeMap::new();
        for (units, tokens, value) in char_terms {
            char_root = insert(&char_root, &units, value);
            char_expected.insert(tokens, value);
        }
        // `CharKey + u64` is also the vocabulary backend's exact overlay
        // monomorphization, so this exercises both char and vocabulary cursors.
        assert_exact_cursor_surface::<CharKey>(char_root, char_expected);

        let mut u64_root = Arc::new(OverlayNode::<U64Key, u64>::new());
        let u64_terms = [(vec![99], 13), (vec![1, 8], 17), (vec![1], 19)];
        let mut u64_expected = BTreeMap::new();
        for (term, value) in u64_terms {
            u64_root = insert(&u64_root, &term, value);
            u64_expected.insert(term, value);
        }
        assert_exact_cursor_surface::<U64Key>(u64_root, u64_expected);
    }

    #[test]
    fn overlay_cursor_revisions_are_isolated_during_concurrent_capture_and_writes() {
        let empty = Arc::new(OverlayNode::<ByteKey, u64>::new());
        let old_root = insert(&empty, b"ant", 7);
        let old_owner = Arc::new(OverlayDictionaryNode::<ByteKey, u64>::from_overlay_root(
            Arc::clone(&old_root),
            None,
        ));
        let participants = 9;
        let barrier = Arc::new(Barrier::new(participants));
        let mut threads = Vec::new();

        for _ in 0..8 {
            let owner = Arc::clone(&old_owner);
            let barrier = Arc::clone(&barrier);
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                for _ in 0..128 {
                    let root = owner.snapshot_root_cursor().expect("old root cursor");
                    let ant = follow(&owner, root, b"ant").expect("old term");
                    // SAFETY: `ant` descends from this retained old revision.
                    assert_eq!(unsafe { owner.snapshot_cursor_value(ant) }, Some(Some(7)));
                    // SAFETY: same retained old revision; `z` was never present.
                    assert_eq!(
                        unsafe { owner.snapshot_cursor_transition(root, b'z') },
                        Some(None)
                    );
                }
            }));
        }

        barrier.wait();
        let mut newest = old_root;
        for value in 0..128 {
            newest = insert(&newest, b"zoo", value);
        }
        let fresh_owner = OverlayDictionaryNode::<ByteKey, u64>::from_overlay_root(newest, None);
        let fresh_root = fresh_owner
            .snapshot_root_cursor()
            .expect("fresh root cursor");
        let zoo = follow(&fresh_owner, fresh_root, b"zoo").expect("fresh term");
        // SAFETY: `zoo` descends from the independently retained fresh revision.
        assert_eq!(
            unsafe { fresh_owner.snapshot_cursor_value(zoo) },
            Some(Some(127))
        );

        for thread in threads {
            thread.join().expect("snapshot reader");
        }
    }
}
