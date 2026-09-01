//! Unit-generic lock-free dynamic DAWG core.
//!
//! This core is the non-blocking counterpart to [`super::core::DawgCore`].
//! Nodes are reference-counted and immutable after publication. Readers retain
//! an atomically published graph revision; writers path-copy the affected route
//! and publish a new revision with a CAS loop.

#[cfg(any(feature = "serialization", test))]
use super::core::DawgCore;
use crate::nonblocking::CasBackoff;
use crate::value::DictionaryValue;
use crate::CharUnit;
use arc_swap::ArcSwap;
use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, OnceLock};

/// A node's outgoing edges: `(key unit, target)` pairs, inline up to four.
///
/// Four covers the overwhelming majority of DAWG nodes without touching the heap;
/// wider nodes spill to a `Vec` transparently.
type LockFreeEdges<U, V> = SmallVec<[(U, Arc<LockFreeDawgNode<U, V>>); 4]>;

const EDGE_LINEAR_SCAN_LIMIT: usize = 16;

#[inline]
fn next_revision(revision: u64) -> u64 {
    revision
        .checked_add(1)
        .expect("DynamicDAWG graph revision space exhausted")
}

/// Immutable sorted edge list published atomically by a node.
#[derive(Clone)]
pub(crate) struct LockFreeEdgeList<U: CharUnit, V: DictionaryValue> {
    pub(crate) edges: LockFreeEdges<U, V>,
}

impl<U: CharUnit, V: DictionaryValue> Default for LockFreeEdgeList<U, V> {
    fn default() -> Self {
        Self {
            edges: SmallVec::new(),
        }
    }
}

impl<U: CharUnit, V: DictionaryValue> LockFreeEdgeList<U, V> {
    #[inline]
    fn new() -> Self {
        Self::default()
    }

    #[inline]
    pub(crate) fn find(&self, label: U) -> Option<&Arc<LockFreeDawgNode<U, V>>> {
        if self.edges.len() < EDGE_LINEAR_SCAN_LIMIT {
            self.edges
                .iter()
                .find(|(edge_label, _)| *edge_label == label)
                .map(|(_, node)| node)
        } else {
            self.edges
                .binary_search_by_key(&label, |(edge_label, _)| *edge_label)
                .ok()
                .map(|idx| &self.edges[idx].1)
        }
    }

    fn with_edge(&self, label: U, node: Arc<LockFreeDawgNode<U, V>>) -> Self {
        crate::causal_perf::record_edge_lists_cloned(1);
        crate::causal_perf::record_edge_arcs_cloned(self.edges.len() as u64);
        let mut edges = self.edges.clone();
        match edges.binary_search_by_key(&label, |(edge_label, _)| *edge_label) {
            Ok(pos) => edges[pos] = (label, node),
            Err(pos) => edges.insert(pos, (label, node)),
        }
        Self { edges }
    }
}

/// Lock-free DAWG node.
pub(crate) struct LockFreeDawgNode<U: CharUnit, V: DictionaryValue> {
    pub(crate) edges: LockFreeEdgeList<U, V>,
    pub(crate) is_final: bool,
    pub(crate) value: Option<Arc<V>>,
    /// Dense identity assigned only by a fully minimized freeze-once build.
    /// Path-copy mutations deliberately create an identity-less root, which
    /// selects the snapshot arena's sequential fallback until recompaction.
    pub(crate) snapshot_id: Option<crate::SnapshotNodeIdentity>,
}

struct LockFreeDawgNodeSummary<'a, U: CharUnit, V: DictionaryValue>(&'a LockFreeDawgNode<U, V>);

impl<U: CharUnit, V: DictionaryValue> std::fmt::Debug for LockFreeDawgNodeSummary<'_, U, V> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LockFreeDawgNodeSummary")
            .field("edge_count", &self.0.edges.edges.len())
            .field("is_final", &self.0.is_final)
            .field("has_value", &self.0.value.is_some())
            .field("snapshot_id", &self.0.snapshot_id)
            .finish()
    }
}

struct LockFreeDawgEdgesSummary<'a, U: CharUnit, V: DictionaryValue>(&'a LockFreeEdges<U, V>);

impl<U: CharUnit, V: DictionaryValue> std::fmt::Debug for LockFreeDawgEdgesSummary<'_, U, V> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_map()
            .entries(
                self.0
                    .iter()
                    .map(|(label, target)| (label, LockFreeDawgNodeSummary(target.as_ref()))),
            )
            .finish()
    }
}

impl<U: CharUnit, V: DictionaryValue> std::fmt::Debug for LockFreeEdgeList<U, V> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LockFreeEdgeList")
            .field("edges", &LockFreeDawgEdgesSummary(&self.edges))
            .finish()
    }
}

impl<U: CharUnit, V: DictionaryValue> std::fmt::Debug for LockFreeDawgNode<U, V> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LockFreeDawgNode")
            .field("edges", &self.edges)
            .field("is_final", &self.is_final)
            .field("has_value", &self.value.is_some())
            .field("snapshot_id", &self.snapshot_id)
            .finish()
    }
}

/// One atomically published dictionary revision.
///
/// Nodes reachable from a published revision are never mutated.  A reader can
/// therefore retain this `Arc` (or just its root) for as long as it needs and
/// observe an exact query-start snapshot while writers publish newer roots.
#[derive(Debug)]
struct GraphVersion<U: CharUnit, V: DictionaryValue> {
    root: Arc<LockFreeDawgNode<U, V>>,
    cursor_graph: OnceLock<Option<Arc<FrozenTraversalGraph<U, V>>>>,
    term_count: usize,
    needs_compaction: bool,
    revision: u64,
}

/// Contiguous query projection of one fully minimized immutable graph.
///
/// The mutation representation remains the persistent Arc graph. This compact
/// projection is built only at a freeze/compaction boundary and gives captured
/// query cursors branch-free edge ranges without per-node allocations or
/// per-edge reference counting.
pub(crate) type FrozenTraversalGraph<U, V> =
    crate::SnapshotTraversalGraph<U, super::DynamicDawgSnapshotCursor<U, V>>;

/// One atomically captured immutable root and its optional compact projection.
type RootWithCursorGraph<U, V> = (
    Arc<LockFreeDawgNode<U, V>>,
    Option<Arc<FrozenTraversalGraph<U, V>>>,
);

/// Result of atomically publishing a privately built frozen graph.
#[cfg(any(feature = "bindings-core", test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PublishIfEmpty {
    /// The graph was published and contains this many distinct terms.
    Published(usize),
    /// A competing writer made the destination nonempty first.
    NonEmpty,
}

pub(crate) fn frozen_traversal_graph_from_root<U: CharUnit, V: DictionaryValue>(
    root: &Arc<LockFreeDawgNode<U, V>>,
) -> Option<FrozenTraversalGraph<U, V>> {
    frozen_traversal_graph_from_snapshot_ids(root)
        .or_else(|| frozen_traversal_graph_from_pointers(root))
}

/// Fast projection for freeze-built graphs whose identities are already dense.
fn frozen_traversal_graph_from_snapshot_ids<U: CharUnit, V: DictionaryValue>(
    root: &Arc<LockFreeDawgNode<U, V>>,
) -> Option<FrozenTraversalGraph<U, V>> {
    let root_index = usize::try_from(root.snapshot_id?.get().checked_sub(1)?).ok()?;
    let node_count = root_index.checked_add(1)?;
    u32::try_from(node_count).ok()?;

    let mut nodes = vec![None; node_count];
    let mut scheduled = vec![false; node_count];
    let mut edges = Vec::new();
    let mut stack = vec![Arc::clone(root)];
    scheduled[root_index] = true;

    while let Some(node) = stack.pop() {
        let node_index = usize::try_from(node.snapshot_id?.get().checked_sub(1)?).ok()?;
        if node_index >= node_count {
            return None;
        }
        let edge_start = u32::try_from(edges.len()).ok()?;
        for (label, child) in &node.edges.edges {
            let child_index = usize::try_from(child.snapshot_id?.get().checked_sub(1)?).ok()?;
            if child_index >= node_count {
                return None;
            }
            edges.push(crate::SnapshotTraversalEdge::new(
                *label,
                u32::try_from(child_index).ok()?,
            ));
            if !scheduled[child_index] {
                scheduled[child_index] = true;
                stack.push(Arc::clone(child));
            }
        }
        nodes[node_index] = Some(crate::SnapshotTraversalNode {
            edge_start,
            edge_len: u32::try_from(node.edges.edges.len()).ok()?,
            is_final: node.is_final,
            value_handle: LockFreeDawgNode::traversal_cursor(&node),
        });
    }

    let nodes: Option<Vec<_>> = nodes.into_iter().collect();
    crate::SnapshotTraversalGraph::new(nodes?, edges, u32::try_from(root_index).ok()?)
}

/// General projection for path-copied revisions whose nodes deliberately lack
/// dense snapshot identities. Pointer identity is stable because the retained
/// immutable root transitively owns every node for the projection lifetime.
fn frozen_traversal_graph_from_pointers<U: CharUnit, V: DictionaryValue>(
    root: &Arc<LockFreeDawgNode<U, V>>,
) -> Option<FrozenTraversalGraph<U, V>> {
    let mut indices = FxHashMap::<std::ptr::NonNull<LockFreeDawgNode<U, V>>, u32>::default();
    let mut discovered = vec![Arc::clone(root)];
    indices.insert(std::ptr::NonNull::from(Arc::as_ref(root)), 0);
    let mut descriptions = Vec::new();
    let mut index = 0usize;

    while index < discovered.len() {
        let node = Arc::clone(&discovered[index]);
        let mut node_edges = Vec::with_capacity(node.edges.edges.len());
        for (label, child) in &node.edges.edges {
            let pointer = std::ptr::NonNull::from(Arc::as_ref(child));
            let target = match indices.get(&pointer).copied() {
                Some(target) => target,
                None => {
                    let target = u32::try_from(discovered.len()).ok()?;
                    indices.insert(pointer, target);
                    discovered.push(Arc::clone(child));
                    target
                }
            };
            node_edges.push((*label, target));
        }
        descriptions.push((
            node.is_final,
            LockFreeDawgNode::traversal_cursor(&node),
            node_edges,
        ));
        index += 1;
    }

    let mut nodes = Vec::with_capacity(descriptions.len());
    let edge_count = descriptions
        .iter()
        .try_fold(0usize, |total, (_, _, edges)| {
            total.checked_add(edges.len())
        })?;
    let mut edges = Vec::with_capacity(edge_count);
    for (is_final, value_handle, node_edges) in descriptions {
        let edge_start = u32::try_from(edges.len()).ok()?;
        let edge_len = u32::try_from(node_edges.len()).ok()?;
        edges.extend(
            node_edges
                .into_iter()
                .map(|(label, target)| crate::SnapshotTraversalEdge::new(label, target)),
        );
        nodes.push(crate::SnapshotTraversalNode::new(
            edge_start,
            edge_len,
            is_final,
            value_handle,
        ));
    }
    crate::SnapshotTraversalGraph::new(nodes, edges, 0)
}

struct Rewrite<U: CharUnit, V: DictionaryValue> {
    node: Arc<LockFreeDawgNode<U, V>>,
    changed: bool,
    inserted: bool,
}

impl<U: CharUnit, V: DictionaryValue> LockFreeDawgNode<U, V> {
    fn new(is_final: bool) -> Self {
        crate::causal_perf::record_nodes_created(1);
        Self {
            edges: LockFreeEdgeList::new(),
            is_final,
            value: None,
            snapshot_id: None,
        }
    }

    #[inline]
    pub(crate) fn is_final(&self) -> bool {
        self.is_final
    }

    #[inline]
    pub(crate) fn value(&self) -> Option<V> {
        if !self.is_final() {
            return None;
        }

        self.value.as_ref().map(|value| (**value).clone())
    }

    /// Encode an immutable node address as a revision-scoped traversal cursor.
    /// The captured root `Arc` transitively retains every reachable child.
    #[inline]
    pub(crate) fn traversal_cursor(node: &Arc<Self>) -> super::DynamicDawgSnapshotCursor<U, V> {
        let pointer = std::ptr::NonNull::from(Arc::as_ref(node));
        super::DynamicDawgSnapshotCursor::from_node(pointer)
    }

    /// Traverse a cursor while the caller retains the root `Arc` that produced
    /// it. Child cursors borrow that same transitive ownership and do not touch
    /// atomic reference counts.
    ///
    /// # Safety
    ///
    /// `cursor` must identify this node or one of its descendants in the exact
    /// immutable graph revision retained by the caller.
    #[inline]
    pub(crate) unsafe fn filter_map_cursor_edges_and_finality<T, P, F>(
        cursor: super::DynamicDawgSnapshotCursor<U, V>,
        mut project: P,
        mut visitor: F,
    ) -> bool
    where
        P: FnMut(U) -> Option<T>,
        F: FnMut(U, super::DynamicDawgSnapshotCursor<U, V>, T),
    {
        // SAFETY: the method contract ties the cursor to this exact node type
        // and to a still-retained immutable root revision.
        let pointer = unsafe { cursor.node_pointer::<Self>() };
        // SAFETY: upheld by the method contract. Published graph nodes are
        // immutable, the pointer retains its original provenance, and the
        // captured root owns every reachable child Arc.
        let node = unsafe { pointer.as_ref() };
        for (label, child) in &node.edges.edges {
            if let Some(projected) = project(*label) {
                visitor(*label, Self::traversal_cursor(child), projected);
            }
        }
        node.is_final
    }

    /// Visit one native page of immutable outgoing edges without cloning any
    /// owning child handle or re-enumerating edges outside the requested page.
    ///
    /// # Safety
    ///
    /// `cursor` must satisfy [`Self::filter_map_cursor_edges_and_finality`]'s
    /// retained-revision contract.
    #[inline]
    pub(crate) unsafe fn visit_cursor_edge_page<F>(
        cursor: super::DynamicDawgSnapshotCursor<U, V>,
        start: usize,
        capacity: usize,
        mut visitor: F,
    ) -> (bool, usize)
    where
        F: FnMut(U, super::DynamicDawgSnapshotCursor<U, V>),
    {
        // SAFETY: the method contract ties the cursor to this exact node type
        // and to a still-retained immutable root revision.
        let pointer = unsafe { cursor.node_pointer::<Self>() };
        // SAFETY: upheld by the method contract. The retained root transitively
        // owns every node reached by this immutable cursor.
        let node = unsafe { pointer.as_ref() };
        let total = node.edges.edges.len();
        if capacity == 1 {
            if let Some((label, child)) = node.edges.edges.get(start) {
                visitor(*label, Self::traversal_cursor(child));
            }
            return (node.is_final, total);
        }
        let page_start = start.min(total);
        let page_end = start.saturating_add(capacity).min(total);
        for (label, child) in &node.edges.edges[page_start..page_end] {
            visitor(*label, Self::traversal_cursor(child));
        }
        (node.is_final, total)
    }

    /// Observe one native edge index without callback or owning-handle traffic.
    ///
    /// # Safety
    ///
    /// `cursor` must satisfy [`Self::filter_map_cursor_edges_and_finality`]'s
    /// retained-revision contract.
    #[inline]
    pub(crate) unsafe fn cursor_edge_at(
        cursor: super::DynamicDawgSnapshotCursor<U, V>,
        index: usize,
    ) -> crate::SnapshotCursorEdgeObservation<U, super::DynamicDawgSnapshotCursor<U, V>> {
        // SAFETY: the method contract ties the cursor to this exact node type
        // and to a still-retained immutable root revision.
        let pointer = unsafe { cursor.node_pointer::<Self>() };
        // SAFETY: the retained root transitively owns this immutable node and
        // every child referenced by its stable edge storage.
        let node = unsafe { pointer.as_ref() };
        let total = node.edges.edges.len();
        let edge = node
            .edges
            .edges
            .get(index)
            .map(|(label, child)| (*label, Self::traversal_cursor(child)));
        crate::SnapshotCursorEdgeObservation::new(node.is_final, total, edge)
    }

    /// Begin one zero-copy traversal over immutable native edge storage.
    ///
    /// # Safety
    ///
    /// `cursor` must identify a node transitively owned by the retained root
    /// revision. That owner must outlive the returned first cursor and range.
    #[inline]
    pub(crate) unsafe fn cursor_edge_range_start(
        cursor: super::DynamicDawgSnapshotCursor<U, V>,
    ) -> crate::SnapshotEdgeRangeStart<U, super::DynamicDawgSnapshotCursor<U, V>, Self> {
        // SAFETY: the method contract ties the cursor to this exact node type.
        let pointer = unsafe { cursor.node_pointer::<Self>() };
        // SAFETY: the retained root transitively owns this immutable node.
        let node = unsafe { pointer.as_ref() };
        let edges = node.edges.edges.as_slice();
        let total = edges.len();
        let first = edges
            .first()
            .map(|(label, child)| (*label, Self::traversal_cursor(child)));
        let remaining = if total >= 2 {
            // SAFETY: `total >= 2` proves both `base.add(1)` and the one-past
            // `base.add(total)` are within the same immutable edge allocation.
            let current =
                unsafe { std::ptr::NonNull::new_unchecked(edges.as_ptr().add(1) as *mut ()) };
            // SAFETY: identical allocation/bounds argument; one-past pointers
            // are valid to retain and compare but are never dereferenced.
            let end =
                unsafe { std::ptr::NonNull::new_unchecked(edges.as_ptr().add(total) as *mut ()) };
            // SAFETY: published `SmallVec` storage never moves or mutates.
            // The retained root owns both inline and spilled representations
            // until the complete traversal and every continuation are dropped.
            Some(unsafe { crate::SnapshotEdgeRangeToken::from_raw_parts(current, end) })
        } else {
            None
        };
        crate::SnapshotEdgeRangeStart::new(node.is_final, total, first, remaining)
    }

    /// Consume one edge from a nonempty native range without Arc traffic.
    ///
    /// # Safety
    ///
    /// `token` must originate from [`Self::cursor_edge_range_start`] or an
    /// earlier step for the same retained root revision.
    #[inline]
    pub(crate) unsafe fn cursor_edge_range_step(
        token: crate::SnapshotEdgeRangeToken<Self>,
    ) -> (
        U,
        super::DynamicDawgSnapshotCursor<U, V>,
        Option<crate::SnapshotEdgeRangeToken<Self>>,
    ) {
        let (current, end) = token.into_raw_parts();
        let current = current.cast::<(U, Arc<Self>)>();
        let end = end.cast::<(U, Arc<Self>)>();
        debug_assert_ne!(current, end, "retained edge ranges are nonempty");

        // SAFETY: the token contract proves `current` is aligned, initialized,
        // strictly before `end`, and owned by the retained immutable revision.
        let (label, child) = unsafe { current.as_ref() };
        // SAFETY: advancing one element from a nonempty range yields either
        // another initialized element or its same-allocation one-past pointer.
        let advanced = unsafe { current.as_ptr().add(1) };
        let remaining = if advanced == end.as_ptr() {
            None
        } else {
            // SAFETY: `advanced != end` plus the input range invariant proves
            // a nonempty suffix with the same allocation and element type.
            Some(unsafe {
                crate::SnapshotEdgeRangeToken::from_raw_parts(
                    std::ptr::NonNull::new_unchecked(advanced).cast(),
                    end.cast(),
                )
            })
        };
        (*label, Self::traversal_cursor(child), remaining)
    }

    /// Read a value through a cursor retained by the captured root revision.
    ///
    /// # Safety
    ///
    /// `cursor` must satisfy [`Self::filter_map_cursor_edges_and_finality`]'s
    /// retained-revision contract.
    #[inline]
    pub(crate) unsafe fn cursor_value(cursor: super::DynamicDawgSnapshotCursor<U, V>) -> Option<V> {
        // SAFETY: the method contract ties the cursor to this exact node type
        // and to a still-retained immutable root revision.
        let pointer = unsafe { cursor.node_pointer::<Self>() };
        // SAFETY: upheld by the method contract; `pointer` retained its Arc
        // provenance and the captured root keeps the allocation alive.
        let node = unsafe { pointer.as_ref() };
        node.value()
    }

    /// Clone one owning `Arc` from a cursor while its captured revision is alive.
    ///
    /// # Safety
    ///
    /// `cursor` must satisfy [`Self::filter_map_cursor_edges_and_finality`]'s
    /// retained-revision contract.
    #[inline]
    pub(crate) unsafe fn arc_from_cursor(
        cursor: super::DynamicDawgSnapshotCursor<U, V>,
    ) -> Arc<Self> {
        // SAFETY: the method contract ties the cursor to this exact node type
        // and to a still-retained immutable root revision.
        let pointer = unsafe { cursor.node_pointer::<Self>() };
        // SAFETY: the retained root transitively owns this allocation, so its
        // strong count is non-zero for the duration of this operation.
        unsafe { Arc::increment_strong_count(pointer.as_ptr()) };
        // SAFETY: the increment above created exactly one owned strong count.
        unsafe { Arc::from_raw(pointer.as_ptr()) }
    }
}

impl<U: CharUnit, V: DictionaryValue> Drop for LockFreeDawgNode<U, V> {
    fn drop(&mut self) {
        crate::causal_perf::record_nodes_dropped(1);
        let edges = std::mem::take(&mut self.edges);
        let mut stack = Vec::with_capacity(edges.edges.len());
        for (_, child) in edges.edges {
            if let Ok(child) = Arc::try_unwrap(child) {
                stack.push(child);
            }
        }

        while let Some(mut node) = stack.pop() {
            let edges = std::mem::take(&mut node.edges);
            for (_, child) in edges.edges {
                if let Ok(child) = Arc::try_unwrap(child) {
                    stack.push(child);
                }
            }
        }
    }
}

struct PendingBuildNode<U: CharUnit, V: DictionaryValue> {
    incoming_label: Option<U>,
    is_final: bool,
    value: Option<V>,
    edges: LockFreeEdges<U, V>,
}

impl<U: CharUnit, V: DictionaryValue> PendingBuildNode<U, V> {
    fn root() -> Self {
        Self {
            incoming_label: None,
            is_final: false,
            value: None,
            edges: SmallVec::new(),
        }
    }

    fn child(incoming_label: U) -> Self {
        Self {
            incoming_label: Some(incoming_label),
            is_final: false,
            value: None,
            edges: SmallVec::new(),
        }
    }
}

#[derive(Clone)]
struct MergeSignature<U: CharUnit, V: DictionaryValue> {
    is_final: bool,
    edges: Vec<(U, std::ptr::NonNull<LockFreeDawgNode<U, V>>)>,
}

impl<U: CharUnit, V: DictionaryValue> PartialEq for MergeSignature<U, V> {
    fn eq(&self, other: &Self) -> bool {
        self.is_final == other.is_final && self.edges == other.edges
    }
}

impl<U: CharUnit, V: DictionaryValue> Eq for MergeSignature<U, V> {}

impl<U: CharUnit, V: DictionaryValue> Hash for MergeSignature<U, V> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.is_final.hash(state);
        self.edges.hash(state);
    }
}

/// Private ordered builder for a minimal immutable graph.
///
/// The pending stack is the unchecked suffix from Daciuk et al.'s incremental
/// construction. Lexicographic input guarantees that a suffix can be frozen
/// as soon as the next term diverges. Frozen nodes are interned by right
/// language and attached to their still-mutable parent. Only the root is
/// published, once, after the final suffix has been minimized.
struct SortedDawgBuilder<U: CharUnit, V: DictionaryValue> {
    pending: Vec<PendingBuildNode<U, V>>,
    interned: FxHashMap<MergeSignature<U, V>, Arc<LockFreeDawgNode<U, V>>>,
    previous: Vec<U>,
    term_count: usize,
    next_snapshot_id: u64,
}

impl<U: CharUnit, V: DictionaryValue> SortedDawgBuilder<U, V> {
    fn new() -> Self {
        Self {
            pending: vec![PendingBuildNode::root()],
            interned: FxHashMap::default(),
            previous: Vec::new(),
            term_count: 0,
            next_snapshot_id: 1,
        }
    }

    fn insert(&mut self, units: &[U], value: Option<V>) {
        crate::causal_perf::record_term_insert_attempts(1);
        crate::causal_perf::record_input_units(units.len() as u64);

        let common_prefix = self
            .previous
            .iter()
            .zip(units)
            .take_while(|(left, right)| left == right)
            .count();
        let ordered = common_prefix == self.previous.len()
            || (common_prefix < units.len() && self.previous[common_prefix] < units[common_prefix]);
        assert!(
            ordered,
            "from_sorted_terms requires lexicographically nondecreasing input"
        );

        self.minimize_to(common_prefix);
        for &label in &units[common_prefix..] {
            self.pending.push(PendingBuildNode::child(label));
        }

        let terminal = self
            .pending
            .last_mut()
            .expect("the pending builder always contains its root");
        if !terminal.is_final {
            self.term_count += 1;
        }
        terminal.is_final = true;
        terminal.value = value;

        self.previous.clear();
        self.previous.extend_from_slice(units);
    }

    fn minimize_to(&mut self, prefix_len: usize) {
        while self.pending.len() > prefix_len + 1 {
            let pending = self
                .pending
                .pop()
                .expect("a minimized suffix always has a pending node");
            let label = pending
                .incoming_label
                .expect("only the root lacks an incoming label");
            let frozen = self.freeze(pending);
            let parent = self
                .pending
                .last_mut()
                .expect("a minimized suffix always has a parent");
            debug_assert!(
                parent
                    .edges
                    .last()
                    .is_none_or(|(previous_label, _)| *previous_label < label),
                "ordered construction must append parent edges in label order"
            );
            parent.edges.push((label, frozen));
        }
    }

    fn freeze(&mut self, pending: PendingBuildNode<U, V>) -> Arc<LockFreeDawgNode<U, V>> {
        let signature = MergeSignature {
            is_final: pending.is_final,
            edges: pending
                .edges
                .iter()
                .map(|(label, child)| (*label, std::ptr::NonNull::from(Arc::as_ref(child))))
                .collect(),
        };

        // DictionaryValue deliberately does not require Eq + Hash. Valueless
        // nodes, including final nodes, are safe to merge by right language.
        // Valued nodes remain distinct so arbitrary user values stay exact.
        if pending.value.is_none() {
            if let Some(existing) = self.interned.get(&signature) {
                return existing.clone();
            }
        }

        crate::causal_perf::record_nodes_created(1);
        let snapshot_id = crate::SnapshotNodeIdentity::new(self.next_snapshot_id)
            .expect("sorted snapshot identities start at one");
        self.next_snapshot_id = self
            .next_snapshot_id
            .checked_add(1)
            .expect("sorted snapshot node identity space exhausted");
        let node = Arc::new(LockFreeDawgNode {
            edges: LockFreeEdgeList {
                edges: pending.edges,
            },
            is_final: pending.is_final,
            value: pending.value.map(Arc::new),
            snapshot_id: Some(snapshot_id),
        });
        if node.value.is_none() {
            self.interned.insert(signature, node.clone());
        }
        node
    }

    fn finish(mut self) -> (Arc<LockFreeDawgNode<U, V>>, usize) {
        self.minimize_to(0);
        let root = self
            .pending
            .pop()
            .expect("the pending builder always contains its root");
        debug_assert!(root.incoming_label.is_none());
        debug_assert!(self.pending.is_empty());

        crate::causal_perf::record_nodes_created(1);
        let snapshot_id = crate::SnapshotNodeIdentity::new(self.next_snapshot_id)
            .expect("sorted snapshot identities start at one");
        let root = Arc::new(LockFreeDawgNode {
            edges: LockFreeEdgeList { edges: root.edges },
            is_final: root.is_final,
            value: root.value.map(Arc::new),
            snapshot_id: Some(snapshot_id),
        });
        (root, self.term_count)
    }
}

/// Unit-generic lock-free dynamic DAWG.
pub(crate) struct LockFreeDawg<U: CharUnit, V: DictionaryValue> {
    version: ArcSwap<GraphVersion<U, V>>,
}

impl<U: CharUnit, V: DictionaryValue> std::fmt::Debug for LockFreeDawg<U, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LockFreeDawg")
            .field("term_count", &self.term_count())
            .field("needs_compaction", &self.needs_compaction())
            .finish()
    }
}

impl<U: CharUnit, V: DictionaryValue> Default for LockFreeDawg<U, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<U: CharUnit, V: DictionaryValue> Clone for LockFreeDawg<U, V> {
    fn clone(&self) -> Self {
        Self {
            version: ArcSwap::from(self.version.load_full()),
        }
    }
}

impl<U: CharUnit, V: DictionaryValue> LockFreeDawg<U, V> {
    pub(crate) fn new() -> Self {
        let root = Arc::new(LockFreeDawgNode::new(false));
        crate::causal_perf::record_graph_versions_created(1);
        Self {
            version: ArcSwap::from_pointee(GraphVersion {
                root,
                cursor_graph: OnceLock::new(),
                term_count: 0,
                needs_compaction: false,
                revision: 0,
            }),
        }
    }

    pub(crate) fn with_config(
        _auto_minimize_threshold: f32,
        _bloom_filter_capacity: Option<usize>,
    ) -> Self {
        Self::new()
    }

    /// Build from ordered terms through one unit-generic minimization kernel.
    ///
    /// The adapter decodes each public wrapper's input into a reusable buffer.
    /// The builder itself is shared by byte, Unicode-scalar, and u64 DAWGs, so
    /// representation adapters do not duplicate the algorithm.
    pub(crate) fn from_sorted_terms_by<I, S, F>(terms: I, mut append_units: F) -> Self
    where
        I: IntoIterator<Item = S>,
        F: FnMut(&S, &mut Vec<U>),
    {
        Self::from_sorted_entries_by(
            terms.into_iter().map(|term| (term, None)),
            move |term, units| append_units(term, units),
        )
    }

    /// Build from ordered term/value pairs through the shared minimal kernel.
    ///
    /// Values are moved into terminal nodes. Since [`DictionaryValue`] does
    /// not require equality or hashing, valued terminals remain distinct;
    /// valueless internal suffixes are still interned by right language.
    pub(crate) fn from_sorted_entries_by<I, S, F>(entries: I, mut append_units: F) -> Self
    where
        I: IntoIterator<Item = (S, Option<V>)>,
        F: FnMut(&S, &mut Vec<U>),
    {
        let mut builder = SortedDawgBuilder::new();
        let mut units = Vec::new();
        for (term, value) in entries {
            units.clear();
            append_units(&term, &mut units);
            builder.insert(&units, value);
        }
        let (root, term_count) = builder.finish();
        crate::causal_perf::record_graph_versions_created(1);
        Self {
            version: ArcSwap::from_pointee(GraphVersion {
                root,
                cursor_graph: OnceLock::new(),
                term_count,
                needs_compaction: false,
                revision: 0,
            }),
        }
    }

    #[cfg(any(feature = "serialization", test))]
    pub(crate) fn from_entries<I>(entries: I) -> Self
    where
        I: IntoIterator<Item = (Vec<U>, Option<V>)>,
    {
        let entries: Vec<_> = entries.into_iter().collect();
        let (root, term_count) = Self::build_minimized_parts(&entries);
        Self {
            version: ArcSwap::from_pointee(GraphVersion {
                root,
                cursor_graph: OnceLock::new(),
                term_count,
                needs_compaction: false,
                revision: 0,
            }),
        }
    }

    #[cfg(any(feature = "serialization", test))]
    pub(crate) fn from_core(core: DawgCore<U, V>) -> Self {
        Self::from_entries(core.extract_all_entries())
    }

    #[cfg(any(feature = "serialization", test))]
    pub(crate) fn to_core(&self) -> DawgCore<U, V> {
        let mut core = DawgCore::new();
        for (units, value) in self.collect_visible_entries() {
            core.insert_direct_with_value(&units, value);
        }
        core
    }

    #[inline]
    pub(crate) fn root_arc(&self) -> Arc<LockFreeDawgNode<U, V>> {
        self.version.load().root.clone()
    }

    /// Capture one published root and its optional compact cursor projection
    /// from the same immutable revision.
    #[inline]
    pub(crate) fn root_arc_with_cursor_graph(&self) -> RootWithCursorGraph<U, V> {
        let version = self.version.load();
        let graph = version
            .cursor_graph
            .get_or_init(|| frozen_traversal_graph_from_root(&version.root).map(Arc::new))
            .clone();
        (version.root.clone(), graph)
    }

    /// Load the published root together with its term count from ONE
    /// version load.
    ///
    /// `root_arc()` followed by `term_count()` performs two independent
    /// `version` loads, so a writer CAS between them pairs one revision's
    /// root with a neighbouring revision's count — a torn capture
    /// (reproduced at ~2% of captures under insert/remove churn; see
    /// docs/bindings/FINDINGS_LEDGER.md finding LDICT-B4). Snapshot capture
    /// must read both fields from the same `DawgVersion`.
    pub(crate) fn root_arc_with_term_count(&self) -> (Arc<LockFreeDawgNode<U, V>>, usize) {
        let version = self.version.load();
        (version.root.clone(), version.term_count)
    }

    /// Capture the root, count, and identity of exactly one graph generation.
    ///
    /// Bindings use the returned revision as their snapshot identity, avoiding
    /// an outer mutation counter whose observation could be torn from this
    /// immutable root. One `ArcSwap` load is the sole linearization point.
    #[cfg(any(feature = "bindings-core", test))]
    pub(crate) fn root_arc_with_term_count_revision(
        &self,
    ) -> (Arc<LockFreeDawgNode<U, V>>, usize, u64) {
        let version = self.version.load();
        (version.root.clone(), version.term_count, version.revision)
    }

    /// Remove every term with one graph-generation CAS.
    ///
    /// A retained expected `Arc` is the CAS token, so an allocator cannot
    /// recycle its address while this attempt is live (pointer-ABA safety).
    #[cfg(any(feature = "bindings-core", test))]
    pub(crate) fn clear(&self) -> bool {
        let mut backoff = CasBackoff::new();
        loop {
            let current = self.version.load_full();
            if current.term_count == 0 {
                return false;
            }
            let next = Arc::new(GraphVersion {
                root: Arc::new(LockFreeDawgNode::new(false)),
                cursor_graph: OnceLock::new(),
                term_count: 0,
                needs_compaction: false,
                revision: next_revision(current.revision),
            });
            let previous = self.version.compare_and_swap(&current, next);
            if Arc::ptr_eq(&previous, &current) {
                return true;
            }
            backoff.snooze();
        }
    }

    /// Publish a privately built minimal graph if the destination is empty.
    ///
    /// Building and sorting happen before this method. If another writer has
    /// inserted a term, the candidate is rejected and the caller can merge its
    /// entries through ordinary path-copy insertion. Losing to another empty
    /// revision (for example, a concurrent clear) simply retries the same
    /// immutable candidate.
    #[cfg(any(feature = "bindings-core", test))]
    pub(crate) fn try_publish_if_empty(&self, frozen: &Self) -> PublishIfEmpty {
        let candidate = frozen.version.load_full();
        let mut backoff = CasBackoff::new();
        loop {
            let current = self.version.load_full();
            if current.term_count != 0 {
                return PublishIfEmpty::NonEmpty;
            }
            let next = Arc::new(GraphVersion {
                root: candidate.root.clone(),
                cursor_graph: OnceLock::new(),
                term_count: candidate.term_count,
                needs_compaction: candidate.needs_compaction,
                revision: next_revision(current.revision),
            });
            let previous = self.version.compare_and_swap(&current, next);
            if Arc::ptr_eq(&previous, &current) {
                return PublishIfEmpty::Published(candidate.term_count);
            }
            backoff.snooze();
        }
    }

    pub(crate) fn insert_units(&self, units: &[U]) -> bool {
        crate::causal_perf::record_term_insert_attempts(1);
        crate::causal_perf::record_input_units(units.len() as u64);
        let terminal = |node: &Arc<LockFreeDawgNode<U, V>>| {
            if node.is_final() {
                return Rewrite {
                    node: node.clone(),
                    changed: false,
                    inserted: false,
                };
            }
            Rewrite {
                node: Self::copy_node(node.edges.clone(), true, node.value.clone()),
                changed: true,
                inserted: true,
            }
        };

        let mut backoff = CasBackoff::new();
        loop {
            crate::causal_perf::record_version_loads(1);
            let current = self.version.load_full();
            let rewrite = Self::rewrite_path(&current.root, units, &terminal);
            if !rewrite.changed {
                return false;
            }
            let inserted = rewrite.inserted;
            crate::causal_perf::record_graph_versions_created(1);
            let next = Arc::new(GraphVersion {
                root: rewrite.node,
                cursor_graph: OnceLock::new(),
                term_count: current.term_count + usize::from(inserted),
                needs_compaction: current.needs_compaction,
                revision: next_revision(current.revision),
            });
            let previous = self.version.compare_and_swap(&current, next);
            if Arc::ptr_eq(&previous, &current) {
                crate::causal_perf::record_cas_publications(1);
                return inserted;
            }
            crate::causal_perf::record_cas_retries(1);
            backoff.snooze();
        }
    }

    pub(crate) fn insert_units_with_value(&self, units: &[U], value: V) -> bool {
        self.insert_units_with_optional_value(units, Some(value))
    }

    /// Insert or update a terminal together with its optional mapped value.
    ///
    /// This internal form is used by bindings whose wire model distinguishes
    /// an existing valueless term from an absent term. Keeping valueless
    /// terminals as `None` also lets the minimal builder intern equivalent
    /// final suffixes instead of manufacturing a sentinel value.
    pub(crate) fn insert_units_with_optional_value(&self, units: &[U], value: Option<V>) -> bool {
        crate::causal_perf::record_term_insert_attempts(1);
        crate::causal_perf::record_input_units(units.len() as u64);
        let terminal = |node: &Arc<LockFreeDawgNode<U, V>>| Rewrite {
            node: Self::copy_node(node.edges.clone(), true, value.clone().map(Arc::new)),
            changed: true,
            inserted: !node.is_final(),
        };

        let mut backoff = CasBackoff::new();
        loop {
            let current = self.version.load_full();
            let rewrite = Self::rewrite_path(&current.root, units, &terminal);
            let inserted = rewrite.inserted;
            let next = Arc::new(GraphVersion {
                root: rewrite.node,
                cursor_graph: OnceLock::new(),
                term_count: current.term_count + usize::from(inserted),
                needs_compaction: current.needs_compaction,
                revision: next_revision(current.revision),
            });
            let previous = self.version.compare_and_swap(&current, next);
            if Arc::ptr_eq(&previous, &current) {
                return inserted;
            }
            backoff.snooze();
        }
    }

    pub(crate) fn update_or_insert_units<F>(
        &self,
        units: &[U],
        default_value: V,
        update_fn: F,
    ) -> bool
    where
        F: Fn(&mut V),
    {
        let mut backoff = CasBackoff::new();
        loop {
            let current = self.version.load_full();
            let terminal = |node: &Arc<LockFreeDawgNode<U, V>>| {
                let inserted = !node.is_final();
                let next_value = if node.is_final() {
                    if let Some(value) = &node.value {
                        let mut updated = (**value).clone();
                        update_fn(&mut updated);
                        updated
                    } else {
                        default_value.clone()
                    }
                } else {
                    default_value.clone()
                };
                Rewrite {
                    node: Self::copy_node(node.edges.clone(), true, Some(Arc::new(next_value))),
                    changed: true,
                    inserted,
                }
            };
            let rewrite = Self::rewrite_path(&current.root, units, &terminal);
            let inserted = rewrite.inserted;
            let next = Arc::new(GraphVersion {
                root: rewrite.node,
                cursor_graph: OnceLock::new(),
                term_count: current.term_count + usize::from(inserted),
                needs_compaction: current.needs_compaction,
                revision: next_revision(current.revision),
            });
            let previous = self.version.compare_and_swap(&current, next);
            if Arc::ptr_eq(&previous, &current) {
                return inserted;
            }
            backoff.snooze();
        }
    }

    fn rewrite_path<F>(
        node: &Arc<LockFreeDawgNode<U, V>>,
        units: &[U],
        terminal: &F,
    ) -> Rewrite<U, V>
    where
        F: Fn(&Arc<LockFreeDawgNode<U, V>>) -> Rewrite<U, V>,
    {
        crate::causal_perf::record_path_units_walked(units.len() as u64);
        // Retain the original nodes and their immutable edge lists while walking
        // down the path, then path-copy bottom-up. Keeping this iterative is
        // important because terms supplied through bindings are not necessarily
        // small enough to fit safely on the Rust call stack.
        let mut current = node.clone();
        let mut frames = Vec::with_capacity(units.len());
        for &label in units {
            let child = current
                .edges
                .find(label)
                .cloned()
                .unwrap_or_else(|| Arc::new(LockFreeDawgNode::new(false)));
            frames.push((current, label));
            current = child;
        }

        let mut rewrite = terminal(&current);
        if !rewrite.changed {
            return Rewrite {
                node: node.clone(),
                changed: false,
                inserted: false,
            };
        }

        for (parent, label) in frames.into_iter().rev() {
            let new_edges = parent.edges.with_edge(label, rewrite.node);
            rewrite.node = Self::copy_node(new_edges, parent.is_final(), parent.value.clone());
        }
        rewrite
    }

    fn copy_node(
        edges: LockFreeEdgeList<U, V>,
        is_final: bool,
        value: Option<Arc<V>>,
    ) -> Arc<LockFreeDawgNode<U, V>> {
        crate::causal_perf::record_nodes_created(1);
        Arc::new(LockFreeDawgNode {
            edges,
            is_final,
            value,
            snapshot_id: None,
        })
    }

    fn find_node_from(
        root: &Arc<LockFreeDawgNode<U, V>>,
        units: &[U],
    ) -> Option<Arc<LockFreeDawgNode<U, V>>> {
        let mut current = root.clone();
        for &label in units {
            let child = current.edges.find(label)?.clone();
            current = child;
        }
        Some(current)
    }

    pub(crate) fn get_units_value(&self, units: &[U]) -> Option<V> {
        let version = self.version.load_full();
        let terminal = Self::find_node_from(&version.root, units)?;
        terminal.value()
    }

    /// Read term presence and its optional mapped value from one published
    /// revision. The outer option is membership; the inner option is value
    /// presence. Binding APIs use this to preserve `absent | valueless |
    /// valued` without pairing observations from different roots.
    #[cfg(any(test, feature = "bindings-core"))]
    pub(crate) fn get_units_optional_value(&self, units: &[U]) -> Option<Option<V>> {
        let version = self.version.load_full();
        let terminal = Self::find_node_from(&version.root, units)?;
        terminal.is_final.then(|| terminal.value())
    }

    #[inline]
    pub(crate) fn contains_units(&self, units: &[U]) -> bool {
        let version = self.version.load_full();
        Self::find_node_from(&version.root, units).is_some_and(|node| node.is_final)
    }

    pub(crate) fn remove_units(&self, units: &[U]) -> bool {
        let terminal = |node: &Arc<LockFreeDawgNode<U, V>>| {
            if !node.is_final() {
                return Rewrite {
                    node: node.clone(),
                    changed: false,
                    inserted: false,
                };
            }
            Rewrite {
                node: Self::copy_node(node.edges.clone(), false, None),
                changed: true,
                inserted: false,
            }
        };

        let mut backoff = CasBackoff::new();
        loop {
            let current = self.version.load_full();
            let rewrite = Self::rewrite_path(&current.root, units, &terminal);
            if !rewrite.changed {
                return false;
            }
            let next = Arc::new(GraphVersion {
                root: rewrite.node,
                cursor_graph: OnceLock::new(),
                term_count: current.term_count.saturating_sub(1),
                needs_compaction: true,
                revision: next_revision(current.revision),
            });
            let previous = self.version.compare_and_swap(&current, next);
            if Arc::ptr_eq(&previous, &current) {
                return true;
            }
            backoff.snooze();
        }
    }

    #[inline]
    pub(crate) fn term_count(&self) -> usize {
        self.version.load().term_count
    }

    pub(crate) fn node_count(&self) -> usize {
        let version = self.version.load_full();
        Self::count_unique_nodes_from(&version.root)
    }

    #[inline]
    pub(crate) fn needs_compaction(&self) -> bool {
        self.version.load().needs_compaction
    }

    pub(crate) fn compact(&self) -> usize {
        self.rebuild_from_visible_entries()
    }

    pub(crate) fn minimize(&self) -> usize {
        self.rebuild_from_visible_entries()
    }

    fn rebuild_from_visible_entries(&self) -> usize {
        let mut backoff = CasBackoff::new();
        loop {
            let current = self.version.load_full();
            let old_node_count = Self::count_unique_nodes_from(&current.root);
            let entries = Self::collect_visible_entries_from(&current.root, current.term_count);
            let (new_root, _) = Self::build_minimized_parts(&entries);
            let new_node_count = Self::count_unique_nodes_from(&new_root);
            let next = Arc::new(GraphVersion {
                root: new_root,
                cursor_graph: OnceLock::new(),
                term_count: entries.len(),
                needs_compaction: false,
                revision: next_revision(current.revision),
            });

            let previous = self.version.compare_and_swap(&current, next);
            if Arc::ptr_eq(&previous, &current) {
                return old_node_count.saturating_sub(new_node_count);
            }
            backoff.snooze();
        }
    }

    pub(crate) fn collect_visible_entries(&self) -> Vec<(Vec<U>, Option<V>)> {
        let version = self.version.load_full();
        Self::collect_visible_entries_from(&version.root, version.term_count)
    }

    fn collect_visible_entries_from(
        root: &Arc<LockFreeDawgNode<U, V>>,
        term_count: usize,
    ) -> Vec<(Vec<U>, Option<V>)> {
        let mut entries = Vec::with_capacity(term_count);
        let mut path = Vec::with_capacity(32);

        struct Frame<U: CharUnit, V: DictionaryValue> {
            children: Vec<(U, Arc<LockFreeDawgNode<U, V>>)>,
            depth: usize,
        }

        if root.is_final {
            let value = root.value.as_ref().map(|value| (**value).clone());
            entries.push((path.clone(), value));
        }

        let mut stack = Vec::with_capacity(64);
        let mut root_children: Vec<_> = root.edges.edges.iter().cloned().collect();
        root_children.reverse();
        stack.push(Frame {
            children: root_children,
            depth: 0,
        });

        while let Some(frame) = stack.last_mut() {
            match frame.children.pop() {
                Some((label, child)) => {
                    let parent_depth = path.len();
                    path.push(label);
                    if child.is_final {
                        let value = child.value.as_ref().map(|value| (**value).clone());
                        entries.push((path.clone(), value));
                    }

                    let mut children: Vec<_> = child.edges.edges.iter().cloned().collect();
                    children.reverse();
                    stack.push(Frame {
                        children,
                        depth: parent_depth,
                    });
                }
                None => {
                    path.truncate(frame.depth);
                    stack.pop();
                }
            }
        }

        entries
    }

    fn build_minimized_parts(
        entries: &[(Vec<U>, Option<V>)],
    ) -> (Arc<LockFreeDawgNode<U, V>>, usize) {
        let mut sorted_entries = entries.to_vec();
        // Keep duplicate entries in source order so the builder's overwrite
        // rule remains deterministic: the last serialized/compacted value for
        // a term wins, matching the public bulk constructors.
        sorted_entries.sort_by(|(left, _), (right, _)| left.cmp(right));
        let mut builder = SortedDawgBuilder::new();
        for (units, value) in sorted_entries {
            builder.insert(&units, value);
        }
        builder.finish()
    }

    fn count_unique_nodes_from(root: &Arc<LockFreeDawgNode<U, V>>) -> usize {
        let mut visited = HashSet::<std::ptr::NonNull<LockFreeDawgNode<U, V>>>::new();
        let mut stack = vec![root.clone()];

        while let Some(node) = stack.pop() {
            let pointer = std::ptr::NonNull::from(Arc::as_ref(&node));
            if !visited.insert(pointer) {
                continue;
            }

            for (_, child) in &node.edges.edges {
                stack.push(child.clone());
            }
        }

        visited.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn provenance_cursor_is_one_word_opaque_and_revision_retained() {
        type Cursor = super::super::DynamicDawgSnapshotCursor<u8, u64>;

        assert_eq!(std::mem::size_of::<Cursor>(), std::mem::size_of::<usize>());
        assert_send_sync::<Cursor>();

        let dawg = LockFreeDawg::<u8, u64>::new();
        assert!(dawg.insert_units_with_value(b"old", 7));
        let retained_root = dawg.root_arc();
        let root_cursor = LockFreeDawgNode::traversal_cursor(&retained_root);
        assert_eq!(format!("{root_cursor:?}"), "DynamicDawgSnapshotCursor(..)");

        let mut child = None;
        // SAFETY: the cursor was produced by `retained_root`, which remains
        // alive through every child traversal and value read in this test.
        let root_final = unsafe {
            LockFreeDawgNode::filter_map_cursor_edges_and_finality(
                root_cursor,
                |label| (label == b'o').then_some(()),
                |_, cursor, ()| child = Some(cursor),
            )
        };
        assert!(!root_final);
        let old_child = child.expect("retained revision contains the first edge");

        assert!(dawg.remove_units(b"old"));
        assert!(dawg.insert_units_with_value(b"other", 11));

        let mut cursor = old_child;
        for wanted in b"ld" {
            let mut next = None;
            // SAFETY: each cursor is emitted by the same retained immutable
            // revision and `retained_root` still owns every reached node.
            unsafe {
                LockFreeDawgNode::filter_map_cursor_edges_and_finality(
                    cursor,
                    |label| (label == *wanted).then_some(()),
                    |_, child, ()| next = Some(child),
                );
            }
            cursor = next.expect("old retained path remains traversable");
        }
        // SAFETY: `cursor` belongs to the still-retained old revision.
        assert_eq!(unsafe { LockFreeDawgNode::cursor_value(cursor) }, Some(7));
    }

    #[test]
    fn capacity_one_cursor_pages_are_exact_index_observations() {
        let dawg = LockFreeDawg::<u8, ()>::from_sorted_terms_by(
            [b"a".as_slice(), b"b".as_slice(), b"c".as_slice()],
            |term, units| units.extend_from_slice(term),
        );
        let root = dawg.root_arc();
        let cursor = LockFreeDawgNode::traversal_cursor(&root);

        for start in 0..=root.edges.edges.len() {
            let expected = root
                .edges
                .edges
                .get(start)
                .map(|(label, child)| (*label, LockFreeDawgNode::traversal_cursor(child)));
            let mut observed = None;
            // SAFETY: `cursor` was obtained from `root`, which retains the
            // immutable revision through every indexed observation.
            let metadata = unsafe {
                LockFreeDawgNode::visit_cursor_edge_page(cursor, start, 1, |label, child| {
                    assert!(observed.is_none(), "capacity one emitted multiple edges");
                    observed = Some((label, child));
                })
            };

            assert_eq!(metadata, (root.is_final, root.edges.edges.len()));
            assert_eq!(
                observed.map(|(label, child)| (label, child.pointer)),
                expected.map(|(label, child)| (label, child.pointer)),
            );
        }
    }

    fn collect_native_root_edge_range(
        root: &Arc<LockFreeDawgNode<u8, ()>>,
    ) -> (bool, usize, Vec<u8>) {
        let cursor = LockFreeDawgNode::traversal_cursor(root);
        // SAFETY: `root` retains the exact immutable revision that produced
        // `cursor` and remains live until the returned range is fully drained.
        let start = unsafe { LockFreeDawgNode::cursor_edge_range_start(cursor) };
        let finality = start.is_final();
        let total = start.total_edge_count();
        let (first, mut remaining) = start.into_first_and_remaining();
        let mut labels = Vec::with_capacity(total);
        if let Some((label, _child)) = first {
            labels.push(label);
        }
        while let Some(token) = remaining {
            // SAFETY: `token` originated from `root` or the preceding step;
            // the same immutable root remains retained throughout the loop.
            let (label, _child, next) =
                unsafe { LockFreeDawgNode::<u8, ()>::cursor_edge_range_step(token) };
            labels.push(label);
            remaining = next;
        }
        (finality, total, labels)
    }

    #[test]
    fn native_edge_ranges_are_exact_across_inline_and_spilled_storage() {
        for (terms, expected) in [
            (
                vec![
                    b"a".as_slice(),
                    b"b".as_slice(),
                    b"c".as_slice(),
                    b"d".as_slice(),
                ],
                b"abcd".as_slice(),
            ),
            (
                vec![
                    b"a".as_slice(),
                    b"b".as_slice(),
                    b"c".as_slice(),
                    b"d".as_slice(),
                    b"e".as_slice(),
                ],
                b"abcde".as_slice(),
            ),
        ] {
            let dawg = LockFreeDawg::<u8, ()>::from_sorted_terms_by(terms, |term, units| {
                units.extend_from_slice(term)
            });
            let root = dawg.root_arc();
            let (finality, total, labels) = collect_native_root_edge_range(&root);
            assert!(!finality);
            assert_eq!(total, expected.len());
            assert_eq!(labels, expected);
        }
    }

    #[test]
    fn native_edge_range_start_and_steps_do_not_clone_child_arcs() {
        let dawg = LockFreeDawg::<u8, ()>::from_sorted_terms_by(
            [
                b"a".as_slice(),
                b"b".as_slice(),
                b"c".as_slice(),
                b"d".as_slice(),
                b"e".as_slice(),
            ],
            |term, units| units.extend_from_slice(term),
        );
        let root = dawg.root_arc();
        let before: Vec<_> = root
            .edges
            .edges
            .iter()
            .map(|(_, child)| Arc::strong_count(child))
            .collect();

        let (_, _, labels) = collect_native_root_edge_range(&root);

        assert_eq!(labels, b"abcde");
        assert_eq!(
            before,
            root.edges
                .edges
                .iter()
                .map(|(_, child)| Arc::strong_count(child))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn retained_old_edge_range_survives_new_root_publication() {
        let dawg = LockFreeDawg::<u8, ()>::from_sorted_terms_by(
            [
                b"a".as_slice(),
                b"b".as_slice(),
                b"c".as_slice(),
                b"d".as_slice(),
                b"e".as_slice(),
            ],
            |term, units| units.extend_from_slice(term),
        );
        let retained_root = dawg.root_arc();
        let cursor = LockFreeDawgNode::traversal_cursor(&retained_root);
        // SAFETY: `retained_root` owns the immutable storage until the range is
        // drained below, even after the DAWG publishes another root.
        let start = unsafe { LockFreeDawgNode::cursor_edge_range_start(cursor) };
        assert!(dawg.insert_units(b"z"));

        let (first, mut remaining) = start.into_first_and_remaining();
        let mut labels = vec![first.expect("old root has a first edge").0];
        while let Some(token) = remaining {
            // SAFETY: every token belongs to `retained_root`, which is still
            // live; publication path-copied instead of mutating its storage.
            let (label, _child, next) =
                unsafe { LockFreeDawgNode::<u8, ()>::cursor_edge_range_step(token) };
            labels.push(label);
            remaining = next;
        }

        assert_eq!(labels, b"abcde");
        assert_eq!(
            dawg.root_arc()
                .edges
                .edges
                .iter()
                .map(|(label, _)| *label)
                .collect::<Vec<_>>(),
            b"abcdez"
        );
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn edge_range_token_is_exactly_two_machine_words() {
        type Token = crate::SnapshotEdgeRangeToken<LockFreeDawgNode<u8, ()>>;
        assert_eq!(
            std::mem::size_of::<Token>(),
            2 * std::mem::size_of::<usize>()
        );
        assert_eq!(std::mem::align_of::<Token>(), std::mem::align_of::<usize>());
    }

    #[test]
    fn sorted_builder_interns_equivalent_final_suffixes() {
        let dawg = LockFreeDawg::<u8, ()>::from_sorted_terms_by(
            [b"ab".as_slice(), b"cb".as_slice()],
            |term, units| units.extend_from_slice(term),
        );

        assert_eq!(dawg.term_count(), 2);
        assert_eq!(dawg.node_count(), 3);
        assert!(dawg.contains_units(b"ab"));
        assert!(dawg.contains_units(b"cb"));
        assert!(!dawg.contains_units(b"b"));
    }

    #[test]
    fn sorted_builder_collapses_duplicate_terms() {
        let dawg =
            LockFreeDawg::<char, ()>::from_sorted_terms_by(["", "same", "same"], |term, units| {
                units.extend(term.chars())
            });

        assert_eq!(dawg.term_count(), 2);
        assert!(dawg.contains_units(&[]));
        assert!(dawg.contains_units(&['s', 'a', 'm', 'e']));
    }

    #[test]
    #[should_panic(expected = "requires lexicographically nondecreasing input")]
    fn sorted_builder_rejects_decreasing_input() {
        let _ = LockFreeDawg::<u8, ()>::from_sorted_terms_by(
            [b"z".as_slice(), b"a".as_slice()],
            |term, units| units.extend_from_slice(term),
        );
    }

    #[test]
    fn minimized_builder_preserves_distinct_values() {
        let dawg = LockFreeDawg::<u8, u32>::from_entries([
            (b"ab".to_vec(), Some(1)),
            (b"cb".to_vec(), Some(2)),
        ]);

        assert_eq!(dawg.term_count(), 2);
        assert_eq!(dawg.get_units_value(b"ab"), Some(1));
        assert_eq!(dawg.get_units_value(b"cb"), Some(2));
    }

    #[test]
    fn minimized_entry_rebuild_preserves_duplicate_precedence() {
        let dawg = LockFreeDawg::<u8, u32>::from_entries([
            (b"same".to_vec(), Some(1)),
            (b"other".to_vec(), Some(9)),
            (b"same".to_vec(), Some(2)),
        ]);

        assert_eq!(dawg.term_count(), 2);
        assert_eq!(dawg.get_units_value(b"same"), Some(2));
        assert_eq!(dawg.get_units_value(b"other"), Some(9));
    }

    #[test]
    fn optional_value_lookup_preserves_all_three_states() {
        let dawg = LockFreeDawg::<u8, u32>::new();
        assert_eq!(dawg.get_units_optional_value(b"cat"), None);
        assert!(dawg.insert_units_with_optional_value(b"cat", None));
        assert_eq!(dawg.get_units_optional_value(b"cat"), Some(None));
        assert!(!dawg.insert_units_with_optional_value(b"cat", Some(7)));
        assert_eq!(dawg.get_units_optional_value(b"cat"), Some(Some(7)));
        assert!(!dawg.insert_units_with_optional_value(b"cat", None));
        assert_eq!(dawg.get_units_optional_value(b"cat"), Some(None));
    }

    fn assert_generation_publication_for<U: CharUnit>(first: Vec<U>, second: Vec<U>) {
        let live = LockFreeDawg::<U, u64>::new();
        let frozen = LockFreeDawg::<U, u64>::from_sorted_entries_by(
            [(first.clone(), Some(1)), (second.clone(), None)],
            |term, units| units.extend_from_slice(term),
        );

        assert_eq!(
            live.try_publish_if_empty(&frozen),
            PublishIfEmpty::Published(2)
        );
        let (retained_root, retained_count, published_revision) =
            live.root_arc_with_term_count_revision();
        assert_eq!(retained_count, 2);
        assert_eq!(published_revision, 1);
        assert_eq!(live.get_units_optional_value(&first), Some(Some(1)));
        assert_eq!(live.get_units_optional_value(&second), Some(None));

        assert!(live.clear());
        let (_, cleared_count, cleared_revision) = live.root_arc_with_term_count_revision();
        assert_eq!(cleared_count, 0);
        assert_eq!(cleared_revision, 2);
        assert_eq!(live.get_units_optional_value(&first), None);
        let retained =
            LockFreeDawg::<U, u64>::collect_visible_entries_from(&retained_root, retained_count);
        assert_eq!(
            retained.len(),
            2,
            "a pre-clear root remains an exact snapshot"
        );

        // Clear-before-insert and insert-before-clear are both represented by
        // one total order of generation CAS publications.
        assert!(live.insert_units_with_optional_value(&first, None));
        assert_eq!(live.get_units_optional_value(&first), Some(None));
        assert_eq!(live.try_publish_if_empty(&frozen), PublishIfEmpty::NonEmpty);
        assert!(live.clear());
        assert!(!live.clear(), "clearing an empty graph does not publish");
    }

    #[test]
    fn graph_generation_operations_are_shared_by_byte_unicode_and_u64() {
        assert_generation_publication_for(vec![b'a'], vec![b'b']);
        assert_generation_publication_for(vec!['α'], vec!['β']);
        assert_generation_publication_for(vec![1_u64], vec![2_u64]);
    }

    #[test]
    fn retained_expected_arc_prevents_pointer_aba_publication() {
        let live = LockFreeDawg::<u8, ()>::new();
        let stale_expected = live.version.load_full();
        assert!(live.insert_units(b"term"));
        assert!(live.clear());
        let current = live.version.load_full();
        assert_eq!(current.term_count, 0);
        assert!(!Arc::ptr_eq(&stale_expected, &current));

        let stale_candidate = Arc::new(GraphVersion {
            root: stale_expected.root.clone(),
            cursor_graph: OnceLock::new(),
            term_count: 0,
            needs_compaction: false,
            revision: next_revision(stale_expected.revision),
        });
        let observed = live
            .version
            .compare_and_swap(&stale_expected, stale_candidate);
        assert!(Arc::ptr_eq(&observed, &current));
        assert!(Arc::ptr_eq(&live.version.load_full(), &current));
    }

    #[test]
    fn stalled_private_frozen_builder_cannot_block_shared_writers() {
        let live = Arc::new(LockFreeDawg::<u8, u64>::new());
        let (candidate_ready_tx, candidate_ready_rx) = mpsc::channel();
        let (publish_tx, publish_rx) = mpsc::channel();

        std::thread::scope(|scope| {
            let publishing_live = Arc::clone(&live);
            let publisher = scope.spawn(move || {
                let frozen = LockFreeDawg::from_sorted_entries_by(
                    [(b"batch".to_vec(), Some(1))],
                    |term, units| units.extend_from_slice(term),
                );
                candidate_ready_tx.send(()).unwrap();
                publish_rx.recv().unwrap();
                publishing_live.try_publish_if_empty(&frozen)
            });

            candidate_ready_rx.recv().unwrap();
            assert!(live.insert_units_with_value(b"writer", 2));
            publish_tx.send(()).unwrap();
            assert_eq!(publisher.join().unwrap(), PublishIfEmpty::NonEmpty);
        });
        assert_eq!(live.get_units_value(b"writer"), Some(2));
    }

    #[test]
    fn sorted_builder_handles_very_long_terms_iteratively() {
        let term = vec![b'x'; 20_000];
        let dawg =
            LockFreeDawg::<u8, ()>::from_sorted_terms_by([term.as_slice()], |term, units| {
                units.extend_from_slice(term)
            });

        assert!(dawg.contains_units(&term));
        assert_eq!(dawg.node_count(), term.len() + 1);
    }

    #[test]
    fn node_debug_and_last_owner_drop_are_stack_safe_at_one_hundred_thousand_depth() {
        const DEPTH: usize = 100_000;

        let mut root = Arc::new(LockFreeDawgNode::<u8, ()>::new(true));
        for _ in 0..DEPTH {
            let mut edges = LockFreeEdges::new();
            edges.push((b'x', root));
            root = Arc::new(LockFreeDawgNode {
                edges: LockFreeEdgeList { edges },
                is_final: false,
                value: None,
                snapshot_id: None,
            });
        }

        let rendered = format!("{root:?}");
        assert!(rendered.contains("LockFreeDawgNodeSummary"));
        assert!(
            rendered.len() < 1_024,
            "Debug must summarize immediate edges rather than traverse the graph"
        );

        drop(root);
    }

    #[test]
    fn last_owner_drop_is_stack_safe_on_a_one_hundred_thousand_depth_branching_spine() {
        const DEPTH: usize = 100_000;

        let mut spine = Arc::new(LockFreeDawgNode::<u8, ()>::new(true));
        for _ in 0..DEPTH {
            let mut edges = LockFreeEdges::new();
            edges.push((b'a', spine));
            edges.push((b'b', Arc::new(LockFreeDawgNode::new(false))));
            spine = Arc::new(LockFreeDawgNode {
                edges: LockFreeEdgeList { edges },
                is_final: false,
                value: None,
                snapshot_id: None,
            });
        }

        // The two-child spine exercises a nontrivial explicit worklist rather
        // than only the one-child linear topology.
        drop(spine);
    }

    #[test]
    fn last_owner_drop_reclaims_a_shared_dag_exactly_once() {
        fn node(edges: LockFreeEdges<u8, ()>) -> Arc<LockFreeDawgNode<u8, ()>> {
            Arc::new(LockFreeDawgNode {
                edges: LockFreeEdgeList { edges },
                is_final: false,
                value: None,
                snapshot_id: None,
            })
        }

        let leaf = node(LockFreeEdges::new());
        let leaf_weak = Arc::downgrade(&leaf);

        let mut left_edges = LockFreeEdges::new();
        left_edges.push((b'l', Arc::clone(&leaf)));
        let left = node(left_edges);
        let left_weak = Arc::downgrade(&left);

        let mut right_edges = LockFreeEdges::new();
        right_edges.push((b'r', leaf));
        let right = node(right_edges);
        let right_weak = Arc::downgrade(&right);

        let mut root_edges = LockFreeEdges::new();
        root_edges.push((b'a', left));
        root_edges.push((b'b', right));
        drop(node(root_edges));

        assert!(left_weak.upgrade().is_none());
        assert!(right_weak.upgrade().is_none());
        assert!(leaf_weak.upgrade().is_none());
    }

    #[test]
    fn update_or_insert_preserves_values_without_locking() {
        let dawg: LockFreeDawg<u8, u32> = LockFreeDawg::new();

        assert!(dawg.update_or_insert_units(b"count", 1, |value| *value += 1));
        assert_eq!(dawg.get_units_value(b"count"), Some(1));

        assert!(!dawg.update_or_insert_units(b"count", 1, |value| *value += 1));
        assert_eq!(dawg.get_units_value(b"count"), Some(2));
    }

    #[test]
    fn core_compat_round_trip_preserves_raw_bytes() {
        let dawg: LockFreeDawg<u8, u32> = LockFreeDawg::new();
        dawg.insert_units_with_value(&[0xff, 0x00, 0x80], 7);
        dawg.insert_units(b"plain");

        let core = dawg.to_core();
        let rebuilt = LockFreeDawg::from_core(core);

        assert_eq!(rebuilt.get_units_value(&[0xff, 0x00, 0x80]), Some(7));
        assert!(rebuilt.contains_units(b"plain"));
    }

    #[test]
    fn compact_reclaims_removed_branch() {
        let dawg: LockFreeDawg<char, ()> = LockFreeDawg::new();
        dawg.insert_units(&['t', 'e', 's', 't']);
        dawg.insert_units(&['t', 'e', 'a', 'm']);
        let before = dawg.node_count();

        assert!(dawg.remove_units(&['t', 'e', 'a', 'm']));
        let removed = dawg.compact();

        assert!(removed > 0 || dawg.node_count() <= before);
        assert!(dawg.contains_units(&['t', 'e', 's', 't']));
        assert!(!dawg.contains_units(&['t', 'e', 'a', 'm']));
    }

    #[test]
    fn retained_root_has_query_start_snapshot_semantics() {
        let dawg: LockFreeDawg<char, u64> = LockFreeDawg::new();
        dawg.insert_units_with_value(&['c', 'a', 't'], 1);
        dawg.insert_units_with_value(&['c', 'o', 't'], 2);
        dawg.insert_units_with_value(&['c', 'u', 't'], 3);

        let query_start_root = dawg.root_arc();

        assert!(dawg.remove_units(&['c', 'o', 't']));
        assert!(dawg.insert_units_with_value(&['c', 'i', 't'], 4));
        assert!(!dawg.insert_units_with_value(&['c', 'u', 't'], 30));
        dawg.compact();

        let mut snapshot =
            LockFreeDawg::<char, u64>::collect_visible_entries_from(&query_start_root, 3);
        snapshot.sort_by(|left, right| left.0.cmp(&right.0));
        assert_eq!(
            snapshot,
            vec![
                (vec!['c', 'a', 't'], Some(1)),
                (vec!['c', 'o', 't'], Some(2)),
                (vec!['c', 'u', 't'], Some(3)),
            ]
        );

        let mut current = dawg.collect_visible_entries();
        current.sort_by(|left, right| left.0.cmp(&right.0));
        assert_eq!(
            current,
            vec![
                (vec!['c', 'a', 't'], Some(1)),
                (vec!['c', 'i', 't'], Some(4)),
                (vec!['c', 'u', 't'], Some(30)),
            ]
        );
    }

    #[test]
    fn very_long_terms_use_iterative_path_copying_and_compaction() {
        let dawg: LockFreeDawg<u8, u64> = LockFreeDawg::new();
        let term = vec![b'x'; 20_000];

        assert!(dawg.insert_units_with_value(&term, 1));
        let query_start_root = dawg.root_arc();
        assert!(!dawg.insert_units_with_value(&term, 2));
        dawg.compact();

        assert_eq!(dawg.get_units_value(&term), Some(2));
        assert_eq!(
            LockFreeDawg::<u8, u64>::find_node_from(&query_start_root, &term)
                .and_then(|node| node.value()),
            Some(1)
        );
    }
}
