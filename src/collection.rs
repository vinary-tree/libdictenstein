//! Snapshot-consistent, backend-generic dictionary collection traversal.
//!
//! The iterators in this module capture exactly one immutable dictionary
//! revision. They select the best traversal capability once: a compact graph,
//! a backend-native cursor, or an owned-node compatibility walk. No iterator
//! retains a dictionary lock while user code runs.

use crate::{
    Dictionary, DictionaryNode, DictionaryTraversalRoot, MappedDictionary, MappedDictionaryNode,
    SnapshotTraversalCursor, SnapshotTraversalGraph,
};
use smallvec::SmallVec;
use std::iter::FusedIterator;
use std::sync::Arc;

const INLINE_EDGES: usize = 8;
const INITIAL_DEPTH: usize = 16;

/// One lossless dictionary entry.
///
/// An entry exists because it was emitted. `value == None` therefore means
/// "present without an associated value", rather than "key absent".
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DictionaryEntry<U, V> {
    /// Root-relative key units.
    pub key: Vec<U>,
    /// Optional mapped value at this final key.
    pub value: Option<V>,
}

impl<U, V> DictionaryEntry<U, V> {
    /// Decompose this entry into its key and optional mapped value.
    pub fn into_pair(self) -> (Vec<U>, Option<V>) {
        (self.key, self.value)
    }
}

struct GraphFrame {
    cursor: SnapshotTraversalCursor,
    next_edge: usize,
    entered: bool,
    restore_path_len: usize,
}

struct NativeFrame<N: DictionaryNode> {
    cursor: N::SnapshotCursor,
    edges: SmallVec<[(N::Unit, N::SnapshotCursor); INLINE_EDGES]>,
    next_edge: usize,
    is_final: bool,
    entered: bool,
    restore_path_len: usize,
}

struct OwnedEdge<N: DictionaryNode> {
    label: N::Unit,
    child: Option<N>,
}

struct OwnedFrame<N: DictionaryNode> {
    node: N,
    edges: SmallVec<[OwnedEdge<N>; INLINE_EDGES]>,
    next_edge: usize,
    is_final: bool,
    entered: bool,
    restore_path_len: usize,
}

enum TraversalMode<N: DictionaryNode> {
    Graph {
        graph: Arc<SnapshotTraversalGraph<N::Unit, N::SnapshotGraphValueHandle>>,
        owner: N,
        stack: Vec<GraphFrame>,
    },
    Native {
        owner: N,
        stack: Vec<NativeFrame<N>>,
    },
    Owned {
        finality_owner: Option<N>,
        stack: Vec<OwnedFrame<N>>,
    },
}

enum Terminal<N: DictionaryNode> {
    Graph(SnapshotTraversalCursor),
    Native(N::SnapshotCursor),
    Owned(N),
}

struct SnapshotTraversal<N: DictionaryNode> {
    mode: TraversalMode<N>,
    path: Vec<N::Unit>,
    exhausted: bool,
}

impl<N: DictionaryNode> SnapshotTraversal<N> {
    fn for_terms(root: DictionaryTraversalRoot<N>) -> Self {
        let (graph, owner) = root.into_parts().into_projection_and_root();
        Self::from_parts(graph, owner, true, true)
    }

    fn for_entries(root: DictionaryTraversalRoot<N>) -> Self
    where
        N: MappedDictionaryNode,
    {
        let (graph, owner) = root.into_parts().into_projection_and_root();
        let graph_values = owner.supports_snapshot_graph_values();
        let cursor_values = owner.supports_snapshot_cursor_values();
        Self::from_parts(graph, owner, graph_values, cursor_values)
    }

    fn from_parts(
        graph: Option<Arc<SnapshotTraversalGraph<N::Unit, N::SnapshotGraphValueHandle>>>,
        owner: N,
        graph_allowed: bool,
        native_allowed: bool,
    ) -> Self {
        if let Some(graph) = graph.filter(|_| graph_allowed) {
            let root_cursor = graph.root_cursor();
            return Self {
                mode: TraversalMode::Graph {
                    graph,
                    owner,
                    stack: vec![GraphFrame {
                        cursor: root_cursor,
                        next_edge: 0,
                        entered: false,
                        restore_path_len: 0,
                    }],
                },
                path: Vec::with_capacity(INITIAL_DEPTH),
                exhausted: false,
            };
        }

        if !owner.snapshot_cursor_requires_full_projection() && native_allowed {
            if let Some(cursor) = owner.snapshot_root_cursor() {
                let frame = Self::native_frame(&owner, cursor, 0);
                return Self {
                    mode: TraversalMode::Native {
                        owner,
                        stack: vec![frame],
                    },
                    path: Vec::with_capacity(INITIAL_DEPTH),
                    exhausted: false,
                };
            }
        }

        let finality_owner = owner.requires_final_units().then(|| owner.clone());
        let frame = Self::owned_frame(owner, 0);
        Self {
            mode: TraversalMode::Owned {
                finality_owner,
                stack: vec![frame],
            },
            path: Vec::with_capacity(INITIAL_DEPTH),
            exhausted: false,
        }
    }

    fn native_frame(
        owner: &N,
        cursor: N::SnapshotCursor,
        restore_path_len: usize,
    ) -> NativeFrame<N> {
        let mut edges: SmallVec<[(N::Unit, N::SnapshotCursor); INLINE_EDGES]> = SmallVec::new();
        // SAFETY: the root cursor comes from `owner`; descendants are supplied
        // by earlier calls on that same retained immutable owner.
        let is_final = unsafe {
            owner
                .filter_map_snapshot_cursor_edges_and_finality(
                    cursor,
                    |_| Some(()),
                    |label, child, ()| edges.push((label, child)),
                )
                .expect("a published snapshot cursor supports traversal")
        };
        edges.sort_unstable_by_key(|(label, _)| *label);
        NativeFrame {
            cursor,
            edges,
            next_edge: 0,
            is_final,
            entered: false,
            restore_path_len,
        }
    }

    fn owned_frame(node: N, restore_path_len: usize) -> OwnedFrame<N> {
        let mut edges: SmallVec<[OwnedEdge<N>; INLINE_EDGES]> = SmallVec::new();
        let is_final = node.visit_edges_and_finality(|label, child| {
            edges.push(OwnedEdge {
                label,
                child: Some(child),
            });
        });
        edges.sort_unstable_by_key(|edge| edge.label);
        OwnedFrame {
            node,
            edges,
            next_edge: 0,
            is_final,
            entered: false,
            restore_path_len,
        }
    }

    fn next_terminal(&mut self) -> Option<Terminal<N>> {
        if self.exhausted {
            return None;
        }

        loop {
            let terminal = match &mut self.mode {
                TraversalMode::Graph {
                    graph,
                    owner,
                    stack,
                } => {
                    let Some(frame) = stack.last_mut() else {
                        self.exhausted = true;
                        return None;
                    };
                    // SAFETY: every frame cursor is the graph root or an edge
                    // target produced by this exact validated graph.
                    let edges = unsafe { graph.edges_and_finality_unchecked(frame.cursor) };
                    if !frame.entered {
                        frame.entered = true;
                        if edges.is_final() && owner.accepts_final_units(&self.path) {
                            Some(Terminal::Graph(frame.cursor))
                        } else {
                            None
                        }
                    } else if let Some(edge) = edges.edges().get(frame.next_edge).copied() {
                        frame.next_edge += 1;
                        let restore_path_len = self.path.len();
                        self.path.push(edge.label());
                        stack.push(GraphFrame {
                            cursor: edge.target_cursor(),
                            next_edge: 0,
                            entered: false,
                            restore_path_len,
                        });
                        None
                    } else {
                        let restore_path_len = frame.restore_path_len;
                        stack.pop();
                        self.path.truncate(restore_path_len);
                        None
                    }
                }
                TraversalMode::Native { owner, stack } => {
                    let Some(frame) = stack.last_mut() else {
                        self.exhausted = true;
                        return None;
                    };
                    if !frame.entered {
                        frame.entered = true;
                        if frame.is_final && owner.accepts_final_units(&self.path) {
                            Some(Terminal::Native(frame.cursor))
                        } else {
                            None
                        }
                    } else if let Some((label, cursor)) = frame.edges.get(frame.next_edge).copied()
                    {
                        frame.next_edge += 1;
                        let restore_path_len = self.path.len();
                        self.path.push(label);
                        stack.push(Self::native_frame(owner, cursor, restore_path_len));
                        None
                    } else {
                        let restore_path_len = frame.restore_path_len;
                        stack.pop();
                        self.path.truncate(restore_path_len);
                        None
                    }
                }
                TraversalMode::Owned {
                    finality_owner,
                    stack,
                } => {
                    let Some(frame) = stack.last_mut() else {
                        self.exhausted = true;
                        return None;
                    };
                    if !frame.entered {
                        frame.entered = true;
                        let visible = finality_owner
                            .as_ref()
                            .is_none_or(|owner| owner.accepts_final_units(&self.path));
                        if frame.is_final && visible {
                            Some(Terminal::Owned(frame.node.clone()))
                        } else {
                            None
                        }
                    } else if frame.next_edge < frame.edges.len() {
                        let edge_index = frame.next_edge;
                        frame.next_edge += 1;
                        let edge = &mut frame.edges[edge_index];
                        let label = edge.label;
                        let child = edge
                            .child
                            .take()
                            .expect("an owned traversal edge is consumed once");
                        let restore_path_len = self.path.len();
                        self.path.push(label);
                        stack.push(Self::owned_frame(child, restore_path_len));
                        None
                    } else {
                        let restore_path_len = frame.restore_path_len;
                        stack.pop();
                        self.path.truncate(restore_path_len);
                        None
                    }
                }
            };

            if terminal.is_some() {
                return terminal;
            }
        }
    }

    fn next_term(&mut self) -> Option<Vec<N::Unit>> {
        self.next_terminal().map(|_| self.path.clone())
    }

    fn next_entry_with<R>(
        &mut self,
        project: impl FnOnce(&[N::Unit], Option<N::Value>) -> R,
    ) -> Option<R>
    where
        N: MappedDictionaryNode,
    {
        let terminal = self.next_terminal()?;
        let value = match (&self.mode, terminal) {
            (TraversalMode::Graph { graph, owner, .. }, Terminal::Graph(cursor)) => {
                // SAFETY: the cursor was just produced by this graph and the
                // graph was captured with this retained owner.
                unsafe {
                    owner
                        .snapshot_graph_cursor_value_with_units(graph, cursor, &self.path)
                        .expect("graph-value capability was checked at iterator construction")
                }
            }
            (TraversalMode::Native { owner, .. }, Terminal::Native(cursor)) => {
                // SAFETY: the cursor belongs to this retained owner revision.
                unsafe {
                    owner
                        .snapshot_cursor_value_with_units(cursor, &self.path)
                        .expect("cursor-value capability was checked at iterator construction")
                }
            }
            (TraversalMode::Owned { .. }, Terminal::Owned(node)) => {
                node.value_at_final_with_units(&self.path)
            }
            _ => unreachable!("terminal kinds never cross traversal modes"),
        };
        Some(project(&self.path, value))
    }

    fn next_entry(&mut self) -> Option<DictionaryEntry<N::Unit, N::Value>>
    where
        N: MappedDictionaryNode,
    {
        self.next_entry_with(|key, value| DictionaryEntry {
            key: key.to_vec(),
            value,
        })
    }
}

/// Iterator over the accepting keys of one captured dictionary revision.
pub struct SnapshotTermIterator<N: DictionaryNode> {
    traversal: SnapshotTraversal<N>,
}

impl<N: DictionaryNode> SnapshotTermIterator<N> {
    /// Capture a traversal root and begin lexicographic iteration.
    pub fn new(root: DictionaryTraversalRoot<N>) -> Self {
        Self {
            traversal: SnapshotTraversal::for_terms(root),
        }
    }
}

impl<N: DictionaryNode> Iterator for SnapshotTermIterator<N> {
    type Item = Vec<N::Unit>;

    fn next(&mut self) -> Option<Self::Item> {
        self.traversal.next_term()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, None)
    }
}

impl<N: DictionaryNode> FusedIterator for SnapshotTermIterator<N> {}

/// Iterator over lossless entries of one captured dictionary revision.
pub struct SnapshotEntryIterator<N: MappedDictionaryNode> {
    traversal: SnapshotTraversal<N>,
    remaining: Option<usize>,
}

impl<N: MappedDictionaryNode> SnapshotEntryIterator<N> {
    /// Capture a traversal root and begin lexicographic iteration.
    pub fn new(root: DictionaryTraversalRoot<N>) -> Self {
        Self {
            traversal: SnapshotTraversal::for_entries(root),
            remaining: None,
        }
    }

    /// Capture a traversal root with a cardinality from the same revision.
    ///
    /// The iterator reports an exact runtime size hint. Use
    /// [`ExactSnapshotEntryIterator`] when the iterator type itself must
    /// implement [`ExactSizeIterator`].
    pub fn with_len(root: DictionaryTraversalRoot<N>, len: usize) -> Self {
        Self {
            traversal: SnapshotTraversal::for_entries(root),
            remaining: Some(len),
        }
    }

    /// Visit entries with a reusable borrowed key buffer.
    ///
    /// The slice is valid only for the duration of the callback. This path
    /// avoids allocating one `Vec` per key and is intended for reducers,
    /// serializers, and foreign batch packers.
    pub fn visit(mut self, mut visitor: impl FnMut(&[N::Unit], Option<N::Value>)) {
        while self
            .traversal
            .next_entry_with(|key, value| visitor(key, value))
            .is_some()
        {}
    }

    /// Fallible allocation-reusing entry visitation.
    pub fn try_visit<E>(
        mut self,
        mut visitor: impl FnMut(&[N::Unit], Option<N::Value>) -> Result<(), E>,
    ) -> Result<(), E> {
        while let Some(result) = self
            .traversal
            .next_entry_with(|key, value| visitor(key, value))
        {
            result?;
        }
        Ok(())
    }
}

impl<N: MappedDictionaryNode> Iterator for SnapshotEntryIterator<N> {
    type Item = DictionaryEntry<N::Unit, N::Value>;

    fn next(&mut self) -> Option<Self::Item> {
        let entry = self.traversal.next_entry();
        if let Some(remaining) = &mut self.remaining {
            if entry.is_some() {
                *remaining = remaining
                    .checked_sub(1)
                    .expect("captured dictionary cardinality under-counted its entries");
            } else {
                debug_assert_eq!(
                    *remaining, 0,
                    "captured dictionary cardinality over-counted its entries"
                );
                *remaining = 0;
            }
        }
        entry
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.remaining
            .map_or((0, None), |remaining| (remaining, Some(remaining)))
    }
}

impl<N: MappedDictionaryNode> FusedIterator for SnapshotEntryIterator<N> {}

/// Exact-sized entry iterator for a revision captured with coherent cardinality.
pub struct ExactSnapshotEntryIterator<N: MappedDictionaryNode> {
    inner: SnapshotEntryIterator<N>,
    remaining: usize,
}

impl<N: MappedDictionaryNode> ExactSnapshotEntryIterator<N> {
    /// Capture a traversal root and its cardinality from the same revision.
    pub fn new(root: DictionaryTraversalRoot<N>, len: usize) -> Self {
        Self {
            inner: SnapshotEntryIterator::new(root),
            remaining: len,
        }
    }

    /// Capture an owned root node and its coherent revision cardinality.
    pub fn from_node(root: N, len: usize) -> Self {
        Self::new(DictionaryTraversalRoot::owned(root), len)
    }

    /// Visit entries with a reusable borrowed key buffer.
    pub fn visit(self, visitor: impl FnMut(&[N::Unit], Option<N::Value>)) {
        self.inner.visit(visitor);
    }

    /// Fallible allocation-reusing entry visitation.
    pub fn try_visit<E>(
        self,
        visitor: impl FnMut(&[N::Unit], Option<N::Value>) -> Result<(), E>,
    ) -> Result<(), E> {
        self.inner.try_visit(visitor)
    }
}

impl<N: MappedDictionaryNode> Iterator for ExactSnapshotEntryIterator<N> {
    type Item = DictionaryEntry<N::Unit, N::Value>;

    fn next(&mut self) -> Option<Self::Item> {
        let entry = self.inner.next();
        if entry.is_some() {
            self.remaining = self
                .remaining
                .checked_sub(1)
                .expect("captured dictionary cardinality under-counted its entries");
        } else {
            debug_assert_eq!(
                self.remaining, 0,
                "captured dictionary cardinality over-counted its entries"
            );
            self.remaining = 0;
        }
        entry
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl<N: MappedDictionaryNode> ExactSizeIterator for ExactSnapshotEntryIterator<N> {}
impl<N: MappedDictionaryNode> FusedIterator for ExactSnapshotEntryIterator<N> {}

/// Traverse structurally accepting paths of any dictionary graph.
///
/// For exact dictionaries this is also their stored-key collection. For a
/// suffix/substring automaton it is the recognized language, which is distinct
/// from the source records used to construct the automaton.
pub trait DictionaryLanguageTerms: Dictionary {
    /// Iterate accepted paths in deterministic lexicographic unit order.
    fn language_terms(&self) -> SnapshotTermIterator<Self::Node> {
        SnapshotTermIterator::new(self.traversal_root())
    }
}

impl<D> DictionaryLanguageTerms for D where D: Dictionary + ?Sized {}

/// Traverse structurally accepting paths and their node values.
pub trait DictionaryLanguageEntries: MappedDictionary
where
    Self::Node: MappedDictionaryNode<Value = Self::Value>,
{
    /// Iterate accepted paths in deterministic lexicographic unit order.
    fn language_entries(&self) -> SnapshotEntryIterator<Self::Node> {
        SnapshotEntryIterator::new(self.traversal_root())
    }
}

impl<D> DictionaryLanguageEntries for D
where
    D: MappedDictionary + ?Sized,
    D::Node: MappedDictionaryNode<Value = D::Value>,
{
}

/// Natural stored-entry collection capability.
///
/// Implementations must enumerate the same finite collection measured by
/// their stored-entry count. Substring families therefore use immutable
/// source-record iterators rather than inheriting graph-language traversal.
pub trait DictionaryEntries {
    /// Key unit domain.
    type Unit: crate::CharUnit;
    /// Mapped value domain.
    type Value: crate::DictionaryValue;
    /// Snapshot-owning, lexicographic entry iterator.
    type Entries: Iterator<Item = DictionaryEntry<Self::Unit, Self::Value>> + FusedIterator;

    /// Capture one immutable revision and iterate its stored entries.
    fn entries(&self) -> Self::Entries;

    /// Fold stored entries while borrowing each key for only the callback.
    ///
    /// The default implementation is universally available. Backends with a
    /// native snapshot cursor override this method so one path buffer is reused
    /// for the entire traversal instead of allocating a `Vec` for every key.
    fn try_fold_entries<A, E, F>(&self, initial: A, mut fold: F) -> Result<A, E>
    where
        F: FnMut(A, &[Self::Unit], Option<Self::Value>) -> Result<A, E>,
    {
        self.entries().try_fold(initial, |accumulator, entry| {
            fold(accumulator, &entry.key, entry.value)
        })
    }

    /// Infallible allocation-reusing fold over stored entries.
    fn fold_entries<A, F>(&self, initial: A, mut fold: F) -> A
    where
        F: FnMut(A, &[Self::Unit], Option<Self::Value>) -> A,
    {
        match self.try_fold_entries(initial, |accumulator, key, value| {
            Ok::<A, std::convert::Infallible>(fold(accumulator, key, value))
        }) {
            Ok(accumulator) => accumulator,
            Err(never) => match never {},
        }
    }
}

/// Concrete stored-entry iterator selected by a dictionary backend.
pub type DictionaryEntriesIter<D> = <D as DictionaryEntries>::Entries;

/// Natural stored-key collection capability derived from lossless entries.
pub trait DictionaryTerms: DictionaryEntries {
    /// Capture one immutable revision and iterate its stored keys.
    fn terms(&self) -> impl FusedIterator<Item = Vec<Self::Unit>> {
        self.entries().map(|entry| entry.key)
    }
}

impl<D: DictionaryEntries + ?Sized> DictionaryTerms for D {}

/// Map-style key view over the natural stored-entry collection.
///
/// `keys()` and [`DictionaryTerms::terms`] are aliases. Both retain one
/// immutable revision and include term-only entries.
pub trait DictionaryKeys: DictionaryEntries {
    /// Capture one immutable revision and iterate its keys.
    fn keys(&self) -> impl FusedIterator<Item = Vec<Self::Unit>> {
        self.entries().map(|entry| entry.key)
    }
}

impl<D: DictionaryEntries + ?Sized> DictionaryKeys for D {}

/// Value view aligned one-for-one with stored entries.
///
/// Unlike legacy mapped-only iterators, this yields `None` for a present
/// term-only key instead of omitting that key.
pub trait DictionaryValues: DictionaryEntries {
    /// Capture one immutable revision and iterate optional mapped values.
    fn values(&self) -> impl FusedIterator<Item = Option<Self::Value>> {
        self.entries().map(|entry| entry.value)
    }
}

impl<D: DictionaryEntries + ?Sized> DictionaryValues for D {}

/// Shared handles preserve the underlying dictionary's snapshot collection
/// capability without adding locks or materialization.
impl<D: DictionaryEntries + ?Sized> DictionaryEntries for Arc<D> {
    type Unit = D::Unit;
    type Value = D::Value;
    type Entries = D::Entries;

    fn entries(&self) -> Self::Entries {
        self.as_ref().entries()
    }

    fn try_fold_entries<A, E, F>(&self, initial: A, fold: F) -> Result<A, E>
    where
        F: FnMut(A, &[Self::Unit], Option<Self::Value>) -> Result<A, E>,
    {
        self.as_ref().try_fold_entries(initial, fold)
    }
}

struct ZipperFinalIterator<Z: crate::DictZipper> {
    stack: Vec<Z>,
}

impl<Z: crate::DictZipper> ZipperFinalIterator<Z> {
    fn new(root: Z) -> Self {
        Self { stack: vec![root] }
    }
}

impl<Z: crate::DictZipper> Iterator for ZipperFinalIterator<Z> {
    type Item = Z;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(zipper) = self.stack.pop() {
            let mut children: SmallVec<[(Z::Unit, Z); INLINE_EDGES]> = zipper.children().collect();
            children.sort_unstable_by_key(|(unit, _)| *unit);
            self.stack
                .extend(children.into_iter().rev().map(|(_, child)| child));
            if zipper.is_final() {
                return Some(zipper);
            }
        }
        None
    }
}

impl<Z: crate::DictZipper> FusedIterator for ZipperFinalIterator<Z> {}

/// Lazy term traversal for any zipper, including set-operation zippers.
///
/// The iterator keeps only a depth-first frontier and never materializes the
/// result collection. Child labels are normalized to lexicographic order.
pub struct ZipperTermIterator<Z: crate::DictZipper> {
    inner: ZipperFinalIterator<Z>,
}

impl<Z: crate::DictZipper> ZipperTermIterator<Z> {
    /// Start at the zipper's current position.
    pub fn new(root: Z) -> Self {
        Self {
            inner: ZipperFinalIterator::new(root),
        }
    }
}

impl<Z: crate::DictZipper> Iterator for ZipperTermIterator<Z> {
    type Item = Vec<Z::Unit>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|zipper| zipper.path())
    }
}

impl<Z: crate::DictZipper> FusedIterator for ZipperTermIterator<Z> {}

/// Lazy lossless entry traversal for any valued zipper.
pub struct ZipperEntryIterator<Z: crate::ValuedDictZipper> {
    inner: ZipperFinalIterator<Z>,
}

impl<Z: crate::ValuedDictZipper> ZipperEntryIterator<Z> {
    /// Start at the zipper's current position.
    pub fn new(root: Z) -> Self {
        Self {
            inner: ZipperFinalIterator::new(root),
        }
    }
}

impl<Z: crate::ValuedDictZipper> Iterator for ZipperEntryIterator<Z> {
    type Item = DictionaryEntry<Z::Unit, Z::Value>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|zipper| DictionaryEntry {
            key: zipper.path(),
            value: zipper.value(),
        })
    }
}

impl<Z: crate::ValuedDictZipper> FusedIterator for ZipperEntryIterator<Z> {}

/// Natural lazy key collection for every dictionary zipper.
pub trait ZipperCollection: crate::DictZipper {
    /// Traverse final paths from the current zipper position.
    fn terms(&self) -> ZipperTermIterator<Self>
    where
        Self: Sized,
    {
        ZipperTermIterator::new(self.clone())
    }

    /// Map-style alias for [`Self::terms`].
    fn keys(&self) -> ZipperTermIterator<Self>
    where
        Self: Sized,
    {
        self.terms()
    }
}

impl<Z: crate::DictZipper> ZipperCollection for Z {}

/// Natural lazy entry and value collection for every valued zipper.
pub trait ValuedZipperCollection: crate::ValuedDictZipper {
    /// Traverse final paths and preserve the distinction between term-only and
    /// mapped final nodes.
    fn entries(&self) -> ZipperEntryIterator<Self>
    where
        Self: Sized,
    {
        ZipperEntryIterator::new(self.clone())
    }

    /// Iterate optional values one-for-one with final paths.
    fn values(&self) -> impl FusedIterator<Item = Option<Self::Value>>
    where
        Self: Sized,
    {
        self.entries().map(|entry| entry.value)
    }
}

impl<Z: crate::ValuedDictZipper> ValuedZipperCollection for Z {}

#[cfg(feature = "pathmap-backend")]
fn try_fold_snapshot<N, A, E, F>(
    entries: SnapshotEntryIterator<N>,
    initial: A,
    mut fold: F,
) -> Result<A, E>
where
    N: MappedDictionaryNode,
    F: FnMut(A, &[N::Unit], Option<N::Value>) -> Result<A, E>,
{
    let mut accumulator = Some(initial);
    entries.try_visit(|key, value| {
        let current = accumulator
            .take()
            .expect("entry fold accumulator is restored after every successful callback");
        let next = fold(current, key, value)?;
        accumulator = Some(next);
        Ok(())
    })?;
    Ok(accumulator.expect("entry fold retains its accumulator through exhaustion"))
}

fn try_fold_exact_snapshot<N, A, E, F>(
    entries: ExactSnapshotEntryIterator<N>,
    initial: A,
    mut fold: F,
) -> Result<A, E>
where
    N: MappedDictionaryNode,
    F: FnMut(A, &[N::Unit], Option<N::Value>) -> Result<A, E>,
{
    let mut accumulator = Some(initial);
    entries.try_visit(|key, value| {
        let current = accumulator
            .take()
            .expect("entry fold accumulator is restored after every successful callback");
        let next = fold(current, key, value)?;
        accumulator = Some(next);
        Ok(())
    })?;
    Ok(accumulator.expect("entry fold retains its accumulator through exhaustion"))
}

#[cfg(feature = "pathmap-backend")]
macro_rules! impl_node_entries {
    ($dictionary:ty, $unit:ty, $value:ident) => {
        impl<$value: crate::DictionaryValue> DictionaryEntries for $dictionary {
            type Unit = $unit;
            type Value = $value;
            type Entries = SnapshotEntryIterator<<Self as Dictionary>::Node>;

            fn entries(&self) -> Self::Entries {
                let root = self.traversal_root();
                match Dictionary::len(self) {
                    Some(len) => SnapshotEntryIterator::with_len(root, len),
                    None => SnapshotEntryIterator::new(root),
                }
            }

            fn try_fold_entries<A, E, F>(&self, initial: A, fold: F) -> Result<A, E>
            where
                F: FnMut(A, &[Self::Unit], Option<Self::Value>) -> Result<A, E>,
            {
                try_fold_snapshot(self.entries(), initial, fold)
            }
        }
    };
}

macro_rules! impl_dynamic_entries {
    ($dictionary:ty, $unit:ty) => {
        impl<V: crate::DictionaryValue> DictionaryEntries for $dictionary {
            type Unit = $unit;
            type Value = V;
            type Entries = ExactSnapshotEntryIterator<<Self as Dictionary>::Node>;

            fn entries(&self) -> Self::Entries {
                let (root, len) = self.root_with_term_count();
                ExactSnapshotEntryIterator::from_node(root, len)
            }

            fn try_fold_entries<A, E, F>(&self, initial: A, fold: F) -> Result<A, E>
            where
                F: FnMut(A, &[Self::Unit], Option<Self::Value>) -> Result<A, E>,
            {
                try_fold_exact_snapshot(self.entries(), initial, fold)
            }
        }
    };
}

impl_dynamic_entries!(crate::dynamic_dawg::DynamicDawg<V>, u8);
impl_dynamic_entries!(crate::dynamic_dawg::DynamicDawgChar<V>, char);
impl_dynamic_entries!(crate::dynamic_dawg::DynamicDawgU64<V>, u64);

macro_rules! impl_static_entries {
    ($dictionary:ty, $unit:ty) => {
        impl<V: crate::DictionaryValue> DictionaryEntries for $dictionary {
            type Unit = $unit;
            type Value = V;
            type Entries = ExactSnapshotEntryIterator<<Self as Dictionary>::Node>;

            fn entries(&self) -> Self::Entries {
                let len = Dictionary::len(self).expect("static dictionary cardinality is exact");
                ExactSnapshotEntryIterator::new(self.traversal_root(), len)
            }

            fn try_fold_entries<A, E, F>(&self, initial: A, fold: F) -> Result<A, E>
            where
                F: FnMut(A, &[Self::Unit], Option<Self::Value>) -> Result<A, E>,
            {
                try_fold_exact_snapshot(self.entries(), initial, fold)
            }
        }
    };
}

impl_static_entries!(crate::double_array_trie::DoubleArrayTrie<V>, u8);
impl_static_entries!(crate::double_array_trie::DoubleArrayTrieChar<V>, char);

impl<V> DictionaryEntries for crate::bijective::BijectiveMap<V>
where
    V: crate::DictionaryValue + Eq + std::hash::Hash,
{
    type Unit = char;
    type Value = V;
    type Entries = <crate::dynamic_dawg::DynamicDawgChar<V> as DictionaryEntries>::Entries;

    fn entries(&self) -> Self::Entries {
        self.forward().entries()
    }

    fn try_fold_entries<A, E, F>(&self, initial: A, fold: F) -> Result<A, E>
    where
        F: FnMut(A, &[Self::Unit], Option<Self::Value>) -> Result<A, E>,
    {
        self.forward().try_fold_entries(initial, fold)
    }
}

#[cfg(feature = "pathmap-backend")]
impl_node_entries!(crate::pathmap::PathMapSnapshot<V>, u8, V);
#[cfg(feature = "pathmap-backend")]
impl_node_entries!(crate::pathmap::PathMapSnapshotChar<V>, char, V);

#[cfg(feature = "pathmap-backend")]
impl<V: crate::DictionaryValue> DictionaryEntries for crate::pathmap::PathMapDictionary<V> {
    type Unit = u8;
    type Value = V;
    type Entries =
        ExactSnapshotEntryIterator<<crate::pathmap::PathMapSnapshot<V> as Dictionary>::Node>;

    fn entries(&self) -> Self::Entries {
        let snapshot = self.snapshot();
        let len = Dictionary::len(&snapshot).expect("dictionary snapshots carry exact length");
        ExactSnapshotEntryIterator::new(snapshot.traversal_root(), len)
    }

    fn try_fold_entries<A, E, F>(&self, initial: A, fold: F) -> Result<A, E>
    where
        F: FnMut(A, &[Self::Unit], Option<Self::Value>) -> Result<A, E>,
    {
        try_fold_exact_snapshot(self.entries(), initial, fold)
    }
}

#[cfg(feature = "pathmap-backend")]
impl<V: crate::DictionaryValue> DictionaryEntries for crate::pathmap::PathMapDictionaryChar<V> {
    type Unit = char;
    type Value = V;
    type Entries =
        ExactSnapshotEntryIterator<<crate::pathmap::PathMapSnapshotChar<V> as Dictionary>::Node>;

    fn entries(&self) -> Self::Entries {
        let snapshot = self.snapshot();
        let len = Dictionary::len(&snapshot).expect("dictionary snapshots carry exact length");
        ExactSnapshotEntryIterator::new(snapshot.traversal_root(), len)
    }

    fn try_fold_entries<A, E, F>(&self, initial: A, fold: F) -> Result<A, E>
    where
        F: FnMut(A, &[Self::Unit], Option<Self::Value>) -> Result<A, E>,
    {
        try_fold_exact_snapshot(self.entries(), initial, fold)
    }
}

#[cfg(feature = "pathmap-backend")]
impl<'a, V: crate::DictionaryValue> DictionaryEntries for crate::pathmap::PathMapRef<'a, V> {
    type Unit = u8;
    type Value = V;
    type Entries = SnapshotEntryIterator<<Self as Dictionary>::Node>;

    fn entries(&self) -> Self::Entries {
        let root = self.traversal_root();
        match Dictionary::len(self) {
            Some(len) => SnapshotEntryIterator::with_len(root, len),
            None => SnapshotEntryIterator::new(root),
        }
    }

    fn try_fold_entries<A, E, F>(&self, initial: A, fold: F) -> Result<A, E>
    where
        F: FnMut(A, &[Self::Unit], Option<Self::Value>) -> Result<A, E>,
    {
        try_fold_snapshot(self.entries(), initial, fold)
    }
}

#[cfg(feature = "pathmap-backend")]
impl<'a, V: crate::DictionaryValue> DictionaryEntries for crate::pathmap::PathMapRefChar<'a, V> {
    type Unit = char;
    type Value = V;
    type Entries = SnapshotEntryIterator<<Self as Dictionary>::Node>;

    fn entries(&self) -> Self::Entries {
        let root = self.traversal_root();
        match Dictionary::len(self) {
            Some(len) => SnapshotEntryIterator::with_len(root, len),
            None => SnapshotEntryIterator::new(root),
        }
    }

    fn try_fold_entries<A, E, F>(&self, initial: A, fold: F) -> Result<A, E>
    where
        F: FnMut(A, &[Self::Unit], Option<Self::Value>) -> Result<A, E>,
    {
        try_fold_snapshot(self.entries(), initial, fold)
    }
}

#[cfg(feature = "persistent-artrie")]
impl<V, S> DictionaryEntries for crate::persistent_artrie::PersistentARTrie<V, S>
where
    V: crate::DictionaryValue,
    S: crate::persistent_artrie::block_storage::BlockStorage,
{
    type Unit = u8;
    type Value = V;
    type Entries = ExactSnapshotEntryIterator<<Self as Dictionary>::Node>;

    fn entries(&self) -> Self::Entries {
        let (root, len) = self.root_with_term_count();
        ExactSnapshotEntryIterator::from_node(root, len)
    }

    fn try_fold_entries<A, E, F>(&self, initial: A, fold: F) -> Result<A, E>
    where
        F: FnMut(A, &[Self::Unit], Option<Self::Value>) -> Result<A, E>,
    {
        try_fold_exact_snapshot(self.entries(), initial, fold)
    }
}

#[cfg(feature = "persistent-artrie")]
impl<V, S> DictionaryEntries for crate::persistent_artrie::char::PersistentARTrieChar<V, S>
where
    V: crate::DictionaryValue,
    S: crate::persistent_artrie::block_storage::BlockStorage,
{
    type Unit = char;
    type Value = V;
    type Entries = ExactSnapshotEntryIterator<<Self as Dictionary>::Node>;

    fn entries(&self) -> Self::Entries {
        let (root, len) = self.root_with_term_count();
        ExactSnapshotEntryIterator::from_node(root, len)
    }

    fn try_fold_entries<A, E, F>(&self, initial: A, fold: F) -> Result<A, E>
    where
        F: FnMut(A, &[Self::Unit], Option<Self::Value>) -> Result<A, E>,
    {
        try_fold_exact_snapshot(self.entries(), initial, fold)
    }
}

#[cfg(feature = "persistent-artrie")]
impl<V, S, const PREFIX: usize> DictionaryEntries
    for crate::persistent_artrie::u64::PersistentARTrieU64<V, S, PREFIX>
where
    V: crate::DictionaryValue,
    S: crate::persistent_artrie::block_storage::BlockStorage,
{
    type Unit = u64;
    type Value = V;
    type Entries = ExactSnapshotEntryIterator<<Self as Dictionary>::Node>;

    fn entries(&self) -> Self::Entries {
        let (root, len) = self.root_with_term_count();
        ExactSnapshotEntryIterator::from_node(root, len)
    }

    fn try_fold_entries<A, E, F>(&self, initial: A, fold: F) -> Result<A, E>
    where
        F: FnMut(A, &[Self::Unit], Option<Self::Value>) -> Result<A, E>,
    {
        try_fold_exact_snapshot(self.entries(), initial, fold)
    }
}

#[cfg(feature = "persistent-artrie")]
impl DictionaryEntries for crate::persistent_artrie::vocab::PersistentVocabARTrie {
    type Unit = char;
    type Value = u64;
    type Entries = ExactSnapshotEntryIterator<<Self as Dictionary>::Node>;

    fn entries(&self) -> Self::Entries {
        let (root, len) = self.root_with_term_count();
        ExactSnapshotEntryIterator::from_node(root, len)
    }

    fn try_fold_entries<A, E, F>(&self, initial: A, fold: F) -> Result<A, E>
    where
        F: FnMut(A, &[Self::Unit], Option<Self::Value>) -> Result<A, E>,
    {
        try_fold_exact_snapshot(self.entries(), initial, fold)
    }
}

/// Adapter from a backend-native UTF-8 record iterator to byte dictionary entries.
pub struct SuffixByteRecordEntryIterator<I, V> {
    inner: I,
    value: std::marker::PhantomData<V>,
}

impl<I, V> Iterator for SuffixByteRecordEntryIterator<I, V>
where
    I: Iterator<Item = (String, Option<V>)>,
{
    type Item = DictionaryEntry<u8, V>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|(text, value)| DictionaryEntry {
            key: text.into_bytes(),
            value,
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<I, V> ExactSizeIterator for SuffixByteRecordEntryIterator<I, V> where
    I: ExactSizeIterator<Item = (String, Option<V>)>
{
}

impl<I, V> FusedIterator for SuffixByteRecordEntryIterator<I, V> where
    I: FusedIterator<Item = (String, Option<V>)>
{
}

/// Adapter from a backend-native UTF-8 record iterator to Unicode dictionary entries.
pub struct SuffixCharRecordEntryIterator<I, V> {
    inner: I,
    value: std::marker::PhantomData<V>,
}

impl<I, V> Iterator for SuffixCharRecordEntryIterator<I, V>
where
    I: Iterator<Item = (String, Option<V>)>,
{
    type Item = DictionaryEntry<char, V>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|(text, value)| DictionaryEntry {
            key: text.chars().collect(),
            value,
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<I, V> ExactSizeIterator for SuffixCharRecordEntryIterator<I, V> where
    I: ExactSizeIterator<Item = (String, Option<V>)>
{
}

impl<I, V> FusedIterator for SuffixCharRecordEntryIterator<I, V> where
    I: FusedIterator<Item = (String, Option<V>)>
{
}

impl<V: crate::DictionaryValue> DictionaryEntries for crate::suffix_automaton::SuffixAutomaton<V> {
    type Unit = u8;
    type Value = V;
    type Entries = SuffixByteRecordEntryIterator<
        crate::suffix_automaton::ascii::SuffixAutomatonEntryIterator<V>,
        V,
    >;

    fn entries(&self) -> Self::Entries {
        SuffixByteRecordEntryIterator {
            inner: self.iter_entries(),
            value: std::marker::PhantomData,
        }
    }
}

impl<V: crate::DictionaryValue> DictionaryEntries
    for crate::suffix_automaton::char::SuffixAutomatonChar<V>
{
    type Unit = char;
    type Value = V;
    type Entries = SuffixCharRecordEntryIterator<
        crate::suffix_automaton::char::SuffixAutomatonCharEntryIterator<V>,
        V,
    >;

    fn entries(&self) -> Self::Entries {
        SuffixCharRecordEntryIterator {
            inner: self.iter_entries(),
            value: std::marker::PhantomData,
        }
    }
}

#[cfg(feature = "persistent-artrie")]
macro_rules! impl_persistent_suffix_entries {
    ($dictionary:path, $iterator:path, byte) => {
        impl<V, S> DictionaryEntries for $dictionary
        where
            V: crate::DictionaryValue,
            S: crate::persistent_artrie::block_storage::BlockStorage,
        {
            type Unit = u8;
            type Value = V;
            type Entries = SuffixByteRecordEntryIterator<$iterator, V>;

            fn entries(&self) -> Self::Entries {
                SuffixByteRecordEntryIterator {
                    inner: self.iter_entries(),
                    value: std::marker::PhantomData,
                }
            }
        }
    };
    ($dictionary:path, $iterator:path, char) => {
        impl<V, S> DictionaryEntries for $dictionary
        where
            V: crate::DictionaryValue,
            S: crate::persistent_artrie::block_storage::BlockStorage,
        {
            type Unit = char;
            type Value = V;
            type Entries = SuffixCharRecordEntryIterator<$iterator, V>;

            fn entries(&self) -> Self::Entries {
                SuffixCharRecordEntryIterator {
                    inner: self.iter_entries(),
                    value: std::marker::PhantomData,
                }
            }
        }
    };
}

#[cfg(feature = "persistent-artrie")]
impl_persistent_suffix_entries!(
    crate::persistent_artrie::suffix_automaton::PersistentSuffixAutomaton<V, S>,
    crate::persistent_artrie::suffix_automaton::PersistentSuffixAutomatonEntryIterator<V>,
    byte
);
#[cfg(feature = "persistent-artrie")]
impl_persistent_suffix_entries!(
    crate::persistent_artrie::suffix_automaton::PersistentSuffixAutomatonChar<V, S>,
    crate::persistent_artrie::suffix_automaton::PersistentSuffixAutomatonCharEntryIterator<V>,
    char
);
#[cfg(feature = "persistent-artrie")]
impl_persistent_suffix_entries!(
    crate::persistent_artrie::suffix_tree::PersistentSuffixTree<V, S>,
    crate::persistent_artrie::suffix_tree::PersistentSuffixTreeEntryIterator<V>,
    byte
);
#[cfg(feature = "persistent-artrie")]
impl_persistent_suffix_entries!(
    crate::persistent_artrie::suffix_tree::PersistentSuffixTreeChar<V, S>,
    crate::persistent_artrie::suffix_tree::PersistentSuffixTreeCharEntryIterator<V>,
    char
);
#[cfg(feature = "persistent-artrie")]
impl_persistent_suffix_entries!(
    crate::persistent_artrie::scdawg::PersistentScdawg<V, S>,
    crate::persistent_artrie::scdawg::PersistentScdawgEntryIterator<V>,
    byte
);
#[cfg(feature = "persistent-artrie")]
impl_persistent_suffix_entries!(
    crate::persistent_artrie::scdawg::PersistentScdawgChar<V, S>,
    crate::persistent_artrie::scdawg::PersistentScdawgCharEntryIterator<V>,
    char
);

/// Exact byte-SCDAWG entry iterator backed by one immutable record revision.
pub struct ScdawgEntryIterator<V: crate::DictionaryValue> {
    inner: crate::scdawg::ascii::ScdawgEntryIterator<V>,
}

impl<V: crate::DictionaryValue> Iterator for ScdawgEntryIterator<V> {
    type Item = DictionaryEntry<u8, V>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|(term, value)| DictionaryEntry {
            key: term.into_bytes(),
            value,
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<V: crate::DictionaryValue> ExactSizeIterator for ScdawgEntryIterator<V> {}
impl<V: crate::DictionaryValue> FusedIterator for ScdawgEntryIterator<V> {}

impl<V: crate::DictionaryValue> DictionaryEntries for crate::scdawg::Scdawg<V> {
    type Unit = u8;
    type Value = V;
    type Entries = ScdawgEntryIterator<V>;

    fn entries(&self) -> Self::Entries {
        ScdawgEntryIterator {
            inner: self.iter_entries(),
        }
    }
}

/// Exact Unicode-SCDAWG entry iterator backed by one immutable record revision.
pub struct ScdawgCharEntryIterator<V: crate::DictionaryValue> {
    inner: crate::scdawg::char::ScdawgCharEntryIterator<V>,
}

impl<V: crate::DictionaryValue> Iterator for ScdawgCharEntryIterator<V> {
    type Item = DictionaryEntry<char, V>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|(term, value)| DictionaryEntry {
            key: term.chars().collect(),
            value,
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<V: crate::DictionaryValue> ExactSizeIterator for ScdawgCharEntryIterator<V> {}
impl<V: crate::DictionaryValue> FusedIterator for ScdawgCharEntryIterator<V> {}

impl<V: crate::DictionaryValue> DictionaryEntries for crate::scdawg::ScdawgChar<V> {
    type Unit = char;
    type Value = V;
    type Entries = ScdawgCharEntryIterator<V>;

    fn entries(&self) -> Self::Entries {
        ScdawgCharEntryIterator {
            inner: self.iter_entries(),
        }
    }
}

macro_rules! impl_borrowed_entries {
    ($dictionary:ty) => {
        impl<'a, V: crate::DictionaryValue> IntoIterator for &'a $dictionary {
            type Item = DictionaryEntry<
                <$dictionary as DictionaryEntries>::Unit,
                <$dictionary as DictionaryEntries>::Value,
            >;
            type IntoIter = DictionaryEntriesIter<$dictionary>;

            fn into_iter(self) -> Self::IntoIter {
                DictionaryEntries::entries(self)
            }
        }
    };
}

impl_borrowed_entries!(crate::dynamic_dawg::DynamicDawg<V>);
impl_borrowed_entries!(crate::dynamic_dawg::DynamicDawgChar<V>);
impl_borrowed_entries!(crate::dynamic_dawg::DynamicDawgU64<V>);
impl_borrowed_entries!(crate::double_array_trie::DoubleArrayTrie<V>);
impl_borrowed_entries!(crate::double_array_trie::DoubleArrayTrieChar<V>);
impl_borrowed_entries!(crate::suffix_automaton::SuffixAutomaton<V>);
impl_borrowed_entries!(crate::suffix_automaton::SuffixAutomatonChar<V>);
impl_borrowed_entries!(crate::scdawg::Scdawg<V>);
impl_borrowed_entries!(crate::scdawg::ScdawgChar<V>);

impl<V> IntoIterator for &crate::bijective::BijectiveMap<V>
where
    V: crate::DictionaryValue + Eq + std::hash::Hash,
{
    type Item = DictionaryEntry<char, V>;
    type IntoIter = DictionaryEntriesIter<crate::bijective::BijectiveMap<V>>;

    fn into_iter(self) -> Self::IntoIter {
        DictionaryEntries::entries(self)
    }
}

#[cfg(feature = "pathmap-backend")]
impl_borrowed_entries!(crate::pathmap::PathMapDictionary<V>);
#[cfg(feature = "pathmap-backend")]
impl_borrowed_entries!(crate::pathmap::PathMapDictionaryChar<V>);
#[cfg(feature = "pathmap-backend")]
impl_borrowed_entries!(crate::pathmap::PathMapSnapshot<V>);
#[cfg(feature = "pathmap-backend")]
impl_borrowed_entries!(crate::pathmap::PathMapSnapshotChar<V>);

#[cfg(feature = "pathmap-backend")]
impl<V: crate::DictionaryValue> IntoIterator for crate::pathmap::PathMapSnapshot<V> {
    type Item = DictionaryEntry<u8, V>;
    type IntoIter = DictionaryEntriesIter<Self>;

    fn into_iter(self) -> Self::IntoIter {
        DictionaryEntries::entries(&self)
    }
}

#[cfg(feature = "pathmap-backend")]
impl<V: crate::DictionaryValue> IntoIterator for crate::pathmap::PathMapSnapshotChar<V> {
    type Item = DictionaryEntry<char, V>;
    type IntoIter = DictionaryEntriesIter<Self>;

    fn into_iter(self) -> Self::IntoIter {
        DictionaryEntries::entries(&self)
    }
}

#[cfg(feature = "pathmap-backend")]
impl<'map, V: crate::DictionaryValue> IntoIterator for &crate::pathmap::PathMapRef<'map, V> {
    type Item = DictionaryEntry<u8, V>;
    type IntoIter = DictionaryEntriesIter<crate::pathmap::PathMapRef<'map, V>>;

    fn into_iter(self) -> Self::IntoIter {
        DictionaryEntries::entries(self)
    }
}

#[cfg(feature = "pathmap-backend")]
impl<'map, V: crate::DictionaryValue> IntoIterator for &crate::pathmap::PathMapRefChar<'map, V> {
    type Item = DictionaryEntry<char, V>;
    type IntoIter = DictionaryEntriesIter<crate::pathmap::PathMapRefChar<'map, V>>;

    fn into_iter(self) -> Self::IntoIter {
        DictionaryEntries::entries(self)
    }
}

#[cfg(feature = "persistent-artrie")]
macro_rules! impl_borrowed_persistent_entries {
    ($dictionary:ty) => {
        impl<'a, V, S> IntoIterator for &'a $dictionary
        where
            V: crate::DictionaryValue,
            S: crate::persistent_artrie::block_storage::BlockStorage,
        {
            type Item = DictionaryEntry<
                <$dictionary as DictionaryEntries>::Unit,
                <$dictionary as DictionaryEntries>::Value,
            >;
            type IntoIter = DictionaryEntriesIter<$dictionary>;

            fn into_iter(self) -> Self::IntoIter {
                DictionaryEntries::entries(self)
            }
        }
    };
}

#[cfg(feature = "persistent-artrie")]
impl_borrowed_persistent_entries!(crate::persistent_artrie::PersistentARTrie<V, S>);
#[cfg(feature = "persistent-artrie")]
impl_borrowed_persistent_entries!(crate::persistent_artrie::char::PersistentARTrieChar<V, S>);
#[cfg(feature = "persistent-artrie")]
impl_borrowed_persistent_entries!(
    crate::persistent_artrie::suffix_automaton::PersistentSuffixAutomaton<V, S>
);
#[cfg(feature = "persistent-artrie")]
impl_borrowed_persistent_entries!(
    crate::persistent_artrie::suffix_automaton::PersistentSuffixAutomatonChar<V, S>
);
#[cfg(feature = "persistent-artrie")]
impl_borrowed_persistent_entries!(
    crate::persistent_artrie::suffix_tree::PersistentSuffixTree<V, S>
);
#[cfg(feature = "persistent-artrie")]
impl_borrowed_persistent_entries!(
    crate::persistent_artrie::suffix_tree::PersistentSuffixTreeChar<V, S>
);
#[cfg(feature = "persistent-artrie")]
impl_borrowed_persistent_entries!(crate::persistent_artrie::scdawg::PersistentScdawg<V, S>);
#[cfg(feature = "persistent-artrie")]
impl_borrowed_persistent_entries!(crate::persistent_artrie::scdawg::PersistentScdawgChar<V, S>);

#[cfg(feature = "persistent-artrie")]
impl<V, S, const PREFIX: usize> IntoIterator
    for &crate::persistent_artrie::u64::PersistentARTrieU64<V, S, PREFIX>
where
    V: crate::DictionaryValue,
    S: crate::persistent_artrie::block_storage::BlockStorage,
{
    type Item = DictionaryEntry<u64, V>;
    type IntoIter =
        DictionaryEntriesIter<crate::persistent_artrie::u64::PersistentARTrieU64<V, S, PREFIX>>;

    fn into_iter(self) -> Self::IntoIter {
        DictionaryEntries::entries(self)
    }
}

#[cfg(feature = "persistent-artrie")]
impl IntoIterator for &crate::persistent_artrie::vocab::PersistentVocabARTrie {
    type Item = DictionaryEntry<char, u64>;
    type IntoIter = DictionaryEntriesIter<crate::persistent_artrie::vocab::PersistentVocabARTrie>;

    fn into_iter(self) -> Self::IntoIter {
        DictionaryEntries::entries(self)
    }
}

#[cfg(feature = "persistent-artrie")]
macro_rules! impl_try_string_collection {
    ($dictionary:ty, $default_dictionary:ty) => {
        impl<V, S> $dictionary
        where
            V: crate::DictionaryValue,
            S: crate::persistent_artrie::block_storage::BlockStorage,
        {
            /// Fallibly append terms in iterator order.
            ///
            /// Successful writes preceding an error remain committed. The returned
            /// count includes operations for which the backend's insertion primitive
            /// returned `true`: unique-key families count new keys, while source-record
            /// suffix families count appended records.
            pub fn try_extend<I, T>(&self, terms: I) -> crate::persistent_artrie::Result<usize>
            where
                I: IntoIterator<Item = T>,
                T: AsRef<str>,
            {
                let mut inserted = 0;
                for term in terms {
                    inserted += usize::from(self.try_insert(term.as_ref())?);
                }
                Ok(inserted)
            }

            /// Sort terms before applying [`Self::try_extend`].
            ///
            /// Sorting is stable. On error, the successfully written sorted prefix
            /// remains committed.
            pub fn try_extend_sorted<I, T>(
                &self,
                terms: I,
            ) -> crate::persistent_artrie::Result<usize>
            where
                I: IntoIterator<Item = T>,
                T: AsRef<str>,
            {
                let mut terms: Vec<String> = terms
                    .into_iter()
                    .map(|term| term.as_ref().to_owned())
                    .collect();
                terms.sort();
                self.try_extend(terms)
            }

            /// Fallibly append mapped entries in iterator order.
            ///
            /// Exact-key families upsert duplicate keys; source-record suffix families
            /// append another record. Successful writes preceding an error remain
            /// committed.
            pub fn try_extend_entries<I, T>(
                &self,
                entries: I,
            ) -> crate::persistent_artrie::Result<usize>
            where
                I: IntoIterator<Item = (T, V)>,
                T: AsRef<str>,
            {
                let mut inserted = 0;
                for (term, value) in entries {
                    inserted += usize::from(self.try_insert_with_value(term.as_ref(), value)?);
                }
                Ok(inserted)
            }

            /// Stably sort mapped entries by key before applying them.
            ///
            /// Stable sorting preserves input order among duplicate keys, so the
            /// ordinary last-value-wins law is retained. On error, the sorted prefix
            /// remains committed.
            pub fn try_extend_entries_sorted<I, T>(
                &self,
                entries: I,
            ) -> crate::persistent_artrie::Result<usize>
            where
                I: IntoIterator<Item = (T, V)>,
                T: AsRef<str>,
            {
                let mut entries: Vec<(String, V)> = entries
                    .into_iter()
                    .map(|(term, value)| (term.as_ref().to_owned(), value))
                    .collect();
                entries.sort_by(|left, right| left.0.cmp(&right.0));
                self.try_extend_entries(entries)
            }
        }

        impl<V: crate::DictionaryValue> $default_dictionary {
            /// Build an in-memory persistent backend without hiding insertion errors.
            ///
            /// If construction fails, the private partial dictionary is dropped and
            /// no partially built value is returned.
            #[allow(deprecated)]
            pub fn try_from_iter<I, T>(terms: I) -> crate::persistent_artrie::Result<Self>
            where
                I: IntoIterator<Item = T>,
                T: AsRef<str>,
            {
                let dictionary = Self::new();
                dictionary.try_extend(terms)?;
                Ok(dictionary)
            }

            /// Sort terms before fallible in-memory construction.
            #[allow(deprecated)]
            pub fn try_from_iter_sorted<I, T>(terms: I) -> crate::persistent_artrie::Result<Self>
            where
                I: IntoIterator<Item = T>,
                T: AsRef<str>,
            {
                let dictionary = Self::new();
                dictionary.try_extend_sorted(terms)?;
                Ok(dictionary)
            }

            /// Build an in-memory persistent backend from mapped entries.
            #[allow(deprecated)]
            pub fn try_from_entries<I, T>(entries: I) -> crate::persistent_artrie::Result<Self>
            where
                I: IntoIterator<Item = (T, V)>,
                T: AsRef<str>,
            {
                let dictionary = Self::new();
                dictionary.try_extend_entries(entries)?;
                Ok(dictionary)
            }

            /// Stably sort mapped entries before fallible in-memory construction.
            #[allow(deprecated)]
            pub fn try_from_entries_sorted<I, T>(
                entries: I,
            ) -> crate::persistent_artrie::Result<Self>
            where
                I: IntoIterator<Item = (T, V)>,
                T: AsRef<str>,
            {
                let dictionary = Self::new();
                dictionary.try_extend_entries_sorted(entries)?;
                Ok(dictionary)
            }
        }
    };
}

#[cfg(feature = "persistent-artrie")]
impl_try_string_collection!(
    crate::persistent_artrie::PersistentARTrie<V, S>,
    crate::persistent_artrie::PersistentARTrie<V>
);
#[cfg(feature = "persistent-artrie")]
impl_try_string_collection!(
    crate::persistent_artrie::char::PersistentARTrieChar<V, S>,
    crate::persistent_artrie::char::PersistentARTrieChar<V>
);
#[cfg(feature = "persistent-artrie")]
impl_try_string_collection!(
    crate::persistent_artrie::suffix_automaton::PersistentSuffixAutomaton<V, S>,
    crate::persistent_artrie::suffix_automaton::PersistentSuffixAutomaton<V>
);
#[cfg(feature = "persistent-artrie")]
impl_try_string_collection!(
    crate::persistent_artrie::suffix_automaton::PersistentSuffixAutomatonChar<V, S>,
    crate::persistent_artrie::suffix_automaton::PersistentSuffixAutomatonChar<V>
);
#[cfg(feature = "persistent-artrie")]
impl_try_string_collection!(
    crate::persistent_artrie::suffix_tree::PersistentSuffixTree<V, S>,
    crate::persistent_artrie::suffix_tree::PersistentSuffixTree<V>
);
#[cfg(feature = "persistent-artrie")]
impl_try_string_collection!(
    crate::persistent_artrie::suffix_tree::PersistentSuffixTreeChar<V, S>,
    crate::persistent_artrie::suffix_tree::PersistentSuffixTreeChar<V>
);
#[cfg(feature = "persistent-artrie")]
impl_try_string_collection!(
    crate::persistent_artrie::scdawg::PersistentScdawg<V, S>,
    crate::persistent_artrie::scdawg::PersistentScdawg<V>
);
#[cfg(feature = "persistent-artrie")]
impl_try_string_collection!(
    crate::persistent_artrie::scdawg::PersistentScdawgChar<V, S>,
    crate::persistent_artrie::scdawg::PersistentScdawgChar<V>
);

#[cfg(feature = "persistent-artrie")]
impl<V, S, const PREFIX: usize> crate::persistent_artrie::u64::PersistentARTrieU64<V, S, PREFIX>
where
    V: crate::DictionaryValue,
    S: crate::persistent_artrie::block_storage::BlockStorage,
{
    /// Fallibly append native u64 sequences in iterator order.
    ///
    /// Successful writes preceding an error remain committed.
    pub fn try_extend<I, T>(&self, sequences: I) -> crate::persistent_artrie::Result<usize>
    where
        I: IntoIterator<Item = T>,
        T: AsRef<[u64]>,
    {
        let mut inserted = 0;
        for sequence in sequences {
            inserted += usize::from(self.try_insert_sequence(sequence.as_ref())?);
        }
        Ok(inserted)
    }

    /// Sort native u64 sequences lexicographically before applying them.
    pub fn try_extend_sorted<I, T>(&self, sequences: I) -> crate::persistent_artrie::Result<usize>
    where
        I: IntoIterator<Item = T>,
        T: AsRef<[u64]>,
    {
        let mut sequences: Vec<Vec<u64>> = sequences
            .into_iter()
            .map(|sequence| sequence.as_ref().to_vec())
            .collect();
        sequences.sort();
        self.try_extend(sequences)
    }

    /// Fallibly append mapped native u64 sequences in iterator order.
    pub fn try_extend_entries<I, T>(&self, entries: I) -> crate::persistent_artrie::Result<usize>
    where
        I: IntoIterator<Item = (T, V)>,
        T: AsRef<[u64]>,
    {
        let mut inserted = 0;
        for (sequence, value) in entries {
            inserted += usize::from(self.try_insert_sequence_with_value(sequence.as_ref(), value)?);
        }
        Ok(inserted)
    }

    /// Stably sort mapped u64 sequences before applying them.
    pub fn try_extend_entries_sorted<I, T>(
        &self,
        entries: I,
    ) -> crate::persistent_artrie::Result<usize>
    where
        I: IntoIterator<Item = (T, V)>,
        T: AsRef<[u64]>,
    {
        let mut entries: Vec<(Vec<u64>, V)> = entries
            .into_iter()
            .map(|(sequence, value)| (sequence.as_ref().to_vec(), value))
            .collect();
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        self.try_extend_entries(entries)
    }

    /// Fallibly build an in-memory native u64 trie.
    pub fn try_from_iter<I, T>(sequences: I) -> crate::persistent_artrie::Result<Self>
    where
        I: IntoIterator<Item = T>,
        T: AsRef<[u64]>,
    {
        let dictionary = Self::new();
        dictionary.try_extend(sequences)?;
        Ok(dictionary)
    }

    /// Sort sequences before fallible in-memory construction.
    pub fn try_from_iter_sorted<I, T>(sequences: I) -> crate::persistent_artrie::Result<Self>
    where
        I: IntoIterator<Item = T>,
        T: AsRef<[u64]>,
    {
        let dictionary = Self::new();
        dictionary.try_extend_sorted(sequences)?;
        Ok(dictionary)
    }

    /// Fallibly build an in-memory native u64 trie from mapped sequences.
    pub fn try_from_entries<I, T>(entries: I) -> crate::persistent_artrie::Result<Self>
    where
        I: IntoIterator<Item = (T, V)>,
        T: AsRef<[u64]>,
    {
        let dictionary = Self::new();
        dictionary.try_extend_entries(entries)?;
        Ok(dictionary)
    }

    /// Stably sort mapped sequences before fallible in-memory construction.
    pub fn try_from_entries_sorted<I, T>(entries: I) -> crate::persistent_artrie::Result<Self>
    where
        I: IntoIterator<Item = (T, V)>,
        T: AsRef<[u64]>,
    {
        let dictionary = Self::new();
        dictionary.try_extend_entries_sorted(entries)?;
        Ok(dictionary)
    }
}

#[cfg(feature = "persistent-artrie")]
impl<S> crate::persistent_artrie::vocab::PersistentVocabARTrie<S>
where
    S: crate::persistent_artrie::block_storage::BlockStorage,
{
    /// Fallibly append vocabulary terms in iterator order.
    ///
    /// Successful assignments preceding an error remain committed.
    pub fn try_extend<I, T>(&self, terms: I) -> crate::persistent_artrie::Result<usize>
    where
        I: IntoIterator<Item = T>,
        T: AsRef<str>,
    {
        let mut applied = 0;
        for term in terms {
            self.insert(term.as_ref())?;
            applied += 1;
        }
        Ok(applied)
    }

    /// Sort vocabulary terms before applying them.
    pub fn try_extend_sorted<I, T>(&self, terms: I) -> crate::persistent_artrie::Result<usize>
    where
        I: IntoIterator<Item = T>,
        T: AsRef<str>,
    {
        let mut terms: Vec<String> = terms
            .into_iter()
            .map(|term| term.as_ref().to_owned())
            .collect();
        terms.sort();
        self.try_extend(terms)
    }

    /// Fallibly append explicit term/index assignments.
    ///
    /// Successful assignments preceding an error remain committed.
    pub fn try_extend_entries<I, T>(&self, entries: I) -> crate::persistent_artrie::Result<usize>
    where
        I: IntoIterator<Item = (T, u64)>,
        T: AsRef<str>,
    {
        let mut inserted = 0;
        for (term, index) in entries {
            inserted += usize::from(self.insert_with_index(term.as_ref(), index)?);
        }
        Ok(inserted)
    }

    /// Stably sort explicit assignments by term before applying them.
    pub fn try_extend_entries_sorted<I, T>(
        &self,
        entries: I,
    ) -> crate::persistent_artrie::Result<usize>
    where
        I: IntoIterator<Item = (T, u64)>,
        T: AsRef<str>,
    {
        let mut entries: Vec<(String, u64)> = entries
            .into_iter()
            .map(|(term, index)| (term.as_ref().to_owned(), index))
            .collect();
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        self.try_extend_entries(entries)
    }
}

#[cfg(feature = "persistent-artrie")]
impl crate::persistent_artrie::vocab::PersistentVocabARTrie {
    /// Create a vocabulary file and fallibly populate it in iterator order.
    ///
    /// If population fails, the path may contain the successfully committed
    /// prefix; the error is returned and no live handle is returned.
    pub fn try_from_iter<P, I, T>(path: P, terms: I) -> crate::persistent_artrie::Result<Self>
    where
        P: AsRef<std::path::Path>,
        I: IntoIterator<Item = T>,
        T: AsRef<str>,
    {
        let dictionary = Self::create(path)?;
        dictionary.try_extend(terms)?;
        Ok(dictionary)
    }

    /// Create a vocabulary file and populate it in sorted term order.
    ///
    /// If population fails, the path may contain the successfully committed
    /// sorted prefix.
    pub fn try_from_iter_sorted<P, I, T>(
        path: P,
        terms: I,
    ) -> crate::persistent_artrie::Result<Self>
    where
        P: AsRef<std::path::Path>,
        I: IntoIterator<Item = T>,
        T: AsRef<str>,
    {
        let dictionary = Self::create(path)?;
        dictionary.try_extend_sorted(terms)?;
        Ok(dictionary)
    }

    /// Create a vocabulary file from explicit term/index assignments.
    ///
    /// If population fails, the path may contain the successfully committed
    /// prefix.
    pub fn try_from_entries<P, I, T>(path: P, entries: I) -> crate::persistent_artrie::Result<Self>
    where
        P: AsRef<std::path::Path>,
        I: IntoIterator<Item = (T, u64)>,
        T: AsRef<str>,
    {
        let dictionary = Self::create(path)?;
        dictionary.try_extend_entries(entries)?;
        Ok(dictionary)
    }

    /// Create a vocabulary file from assignments stably sorted by term.
    ///
    /// If population fails, the path may contain the successfully committed
    /// sorted prefix.
    pub fn try_from_entries_sorted<P, I, T>(
        path: P,
        entries: I,
    ) -> crate::persistent_artrie::Result<Self>
    where
        P: AsRef<std::path::Path>,
        I: IntoIterator<Item = (T, u64)>,
        T: AsRef<str>,
    {
        let dictionary = Self::create(path)?;
        dictionary.try_extend_entries_sorted(entries)?;
        Ok(dictionary)
    }
}

#[cfg(feature = "persistent-artrie")]
impl<V, S> crate::persistent_artrie::u64::EncodedPersistentARTrieU64<V, S>
where
    V: crate::DictionaryValue,
    S: crate::persistent_artrie::block_storage::BlockStorage,
{
    /// Fallibly append encoded u64 sequences in iterator order.
    ///
    /// Successful writes preceding an error remain committed.
    pub fn try_extend<I, T>(&self, sequences: I) -> crate::persistent_artrie::Result<usize>
    where
        I: IntoIterator<Item = T>,
        T: AsRef<[u64]>,
    {
        let mut inserted = 0;
        for sequence in sequences {
            inserted += usize::from(self.try_insert_sequence(sequence.as_ref())?);
        }
        Ok(inserted)
    }

    /// Sort encoded u64 sequences lexicographically before applying them.
    pub fn try_extend_sorted<I, T>(&self, sequences: I) -> crate::persistent_artrie::Result<usize>
    where
        I: IntoIterator<Item = T>,
        T: AsRef<[u64]>,
    {
        let mut sequences: Vec<Vec<u64>> = sequences
            .into_iter()
            .map(|sequence| sequence.as_ref().to_vec())
            .collect();
        sequences.sort();
        self.try_extend(sequences)
    }

    /// Fallibly append mapped encoded u64 sequences in iterator order.
    pub fn try_extend_entries<I, T>(&self, entries: I) -> crate::persistent_artrie::Result<usize>
    where
        I: IntoIterator<Item = (T, V)>,
        T: AsRef<[u64]>,
    {
        let mut inserted = 0;
        for (sequence, value) in entries {
            inserted += usize::from(self.try_insert_sequence_with_value(sequence.as_ref(), value)?);
        }
        Ok(inserted)
    }

    /// Stably sort mapped encoded u64 sequences before applying them.
    pub fn try_extend_entries_sorted<I, T>(
        &self,
        entries: I,
    ) -> crate::persistent_artrie::Result<usize>
    where
        I: IntoIterator<Item = (T, V)>,
        T: AsRef<[u64]>,
    {
        let mut entries: Vec<(Vec<u64>, V)> = entries
            .into_iter()
            .map(|(sequence, value)| (sequence.as_ref().to_vec(), value))
            .collect();
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        self.try_extend_entries(entries)
    }
}

#[cfg(feature = "persistent-artrie")]
impl<V: crate::DictionaryValue> crate::persistent_artrie::u64::EncodedPersistentARTrieU64<V> {
    /// Fallibly build an in-memory encoded u64 trie.
    pub fn try_from_iter<I, T>(sequences: I) -> crate::persistent_artrie::Result<Self>
    where
        I: IntoIterator<Item = T>,
        T: AsRef<[u64]>,
    {
        let dictionary = Self::new();
        dictionary.try_extend(sequences)?;
        Ok(dictionary)
    }

    /// Sort sequences before fallible in-memory construction.
    pub fn try_from_iter_sorted<I, T>(sequences: I) -> crate::persistent_artrie::Result<Self>
    where
        I: IntoIterator<Item = T>,
        T: AsRef<[u64]>,
    {
        let dictionary = Self::new();
        dictionary.try_extend_sorted(sequences)?;
        Ok(dictionary)
    }

    /// Fallibly build an in-memory encoded u64 trie from mapped sequences.
    pub fn try_from_entries<I, T>(entries: I) -> crate::persistent_artrie::Result<Self>
    where
        I: IntoIterator<Item = (T, V)>,
        T: AsRef<[u64]>,
    {
        let dictionary = Self::new();
        dictionary.try_extend_entries(entries)?;
        Ok(dictionary)
    }

    /// Stably sort mapped sequences before fallible in-memory construction.
    pub fn try_from_entries_sorted<I, T>(entries: I) -> crate::persistent_artrie::Result<Self>
    where
        I: IntoIterator<Item = (T, V)>,
        T: AsRef<[u64]>,
    {
        let dictionary = Self::new();
        dictionary.try_extend_entries_sorted(entries)?;
        Ok(dictionary)
    }
}

#[cfg(test)]
mod tests {
    use super::{DictionaryEntries, DictionaryTerms};
    use crate::dynamic_dawg::DynamicDawg;

    #[test]
    fn entries_are_lexicographic_lossless_and_fused() {
        let dictionary = DynamicDawg::<u64>::new();
        dictionary.insert("beta");
        dictionary.insert_with_value("alpha", 7);
        dictionary.insert_with_value("alphabet", 11);

        let mut entries = DictionaryEntries::entries(&dictionary);
        let got: Vec<_> = entries
            .by_ref()
            .map(|entry| (String::from_utf8(entry.key).unwrap(), entry.value))
            .collect();
        assert_eq!(
            got,
            vec![
                ("alpha".to_owned(), Some(7)),
                ("alphabet".to_owned(), Some(11)),
                ("beta".to_owned(), None),
            ]
        );
        assert_eq!(entries.next(), None);
        assert_eq!(entries.next(), None);
    }

    #[test]
    fn traversal_owns_one_query_start_revision() {
        let dictionary = DynamicDawg::<()>::from_terms(["alpha", "beta"]);
        let terms = DictionaryTerms::terms(&dictionary);
        dictionary.insert("gamma");

        let frozen: Vec<_> = terms.map(|term| String::from_utf8(term).unwrap()).collect();
        assert_eq!(frozen, ["alpha", "beta"]);
        assert_eq!(DictionaryTerms::terms(&dictionary).count(), 3);
    }

    #[test]
    fn borrowed_visitor_is_lossless_and_stops_on_error() {
        let dictionary = DynamicDawg::<u64>::new();
        dictionary.insert_with_value("alpha", 1);
        dictionary.insert("beta");
        dictionary.insert_with_value("gamma", 3);

        let mut visited = Vec::new();
        let result = DictionaryEntries::entries(&dictionary).try_visit(|key, value| {
            let key = String::from_utf8(key.to_vec()).unwrap();
            visited.push((key.clone(), value));
            if key == "beta" {
                Err("stop")
            } else {
                Ok(())
            }
        });
        assert_eq!(result, Err("stop"));
        assert_eq!(
            visited,
            [("alpha".to_owned(), Some(1)), ("beta".to_owned(), None)]
        );
    }
}
