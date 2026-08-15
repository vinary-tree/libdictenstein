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
use std::sync::Arc;

/// A node's outgoing edges: `(key unit, target)` pairs, inline up to four.
///
/// Four covers the overwhelming majority of DAWG nodes without touching the heap;
/// wider nodes spill to a `Vec` transparently.
type LockFreeEdges<U, V> = SmallVec<[(U, Arc<LockFreeDawgNode<U, V>>); 4]>;

const EDGE_LINEAR_SCAN_LIMIT: usize = 16;

/// Immutable sorted edge list published atomically by a node.
#[derive(Clone, Debug)]
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
#[derive(Debug)]
pub(crate) struct LockFreeDawgNode<U: CharUnit, V: DictionaryValue> {
    pub(crate) edges: LockFreeEdgeList<U, V>,
    pub(crate) is_final: bool,
    pub(crate) value: Option<Arc<V>>,
}

/// One atomically published dictionary revision.
///
/// Nodes reachable from a published revision are never mutated.  A reader can
/// therefore retain this `Arc` (or just its root) for as long as it needs and
/// observe an exact query-start snapshot while writers publish newer roots.
#[derive(Debug)]
struct GraphVersion<U: CharUnit, V: DictionaryValue> {
    root: Arc<LockFreeDawgNode<U, V>>,
    term_count: usize,
    needs_compaction: bool,
    revision: u64,
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

#[derive(Clone, Eq)]
struct MergeSignature<U: CharUnit> {
    is_final: bool,
    edges: Vec<(U, usize)>,
}

impl<U: CharUnit> PartialEq for MergeSignature<U> {
    fn eq(&self, other: &Self) -> bool {
        self.is_final == other.is_final && self.edges == other.edges
    }
}

impl<U: CharUnit> Hash for MergeSignature<U> {
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
    interned: FxHashMap<MergeSignature<U>, Arc<LockFreeDawgNode<U, V>>>,
    previous: Vec<U>,
    term_count: usize,
}

impl<U: CharUnit, V: DictionaryValue> SortedDawgBuilder<U, V> {
    fn new() -> Self {
        Self {
            pending: vec![PendingBuildNode::root()],
            interned: FxHashMap::default(),
            previous: Vec::new(),
            term_count: 0,
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
                .map(|(label, child)| (*label, Arc::as_ptr(child) as usize))
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
        let node = Arc::new(LockFreeDawgNode {
            edges: LockFreeEdgeList {
                edges: pending.edges,
            },
            is_final: pending.is_final,
            value: pending.value.map(Arc::new),
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
        let root = Arc::new(LockFreeDawgNode {
            edges: LockFreeEdgeList { edges: root.edges },
            is_final: root.is_final,
            value: root.value.map(Arc::new),
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
                term_count: current.term_count + usize::from(inserted),
                needs_compaction: current.needs_compaction,
                revision: current.revision.wrapping_add(1),
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
                term_count: current.term_count + usize::from(inserted),
                needs_compaction: current.needs_compaction,
                revision: current.revision.wrapping_add(1),
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
                term_count: current.term_count + usize::from(inserted),
                needs_compaction: current.needs_compaction,
                revision: current.revision.wrapping_add(1),
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
                term_count: current.term_count.saturating_sub(1),
                needs_compaction: true,
                revision: current.revision.wrapping_add(1),
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
            let new_root = Self::build_minimized_root(&entries);
            let new_node_count = Self::count_unique_nodes_from(&new_root);
            let next = Arc::new(GraphVersion {
                root: new_root,
                term_count: entries.len(),
                needs_compaction: false,
                revision: current.revision.wrapping_add(1),
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

    fn build_minimized_root(entries: &[(Vec<U>, Option<V>)]) -> Arc<LockFreeDawgNode<U, V>> {
        Self::build_minimized_parts(entries).0
    }

    fn count_unique_nodes_from(root: &Arc<LockFreeDawgNode<U, V>>) -> usize {
        let mut visited = HashSet::new();
        let mut stack = vec![root.clone()];

        while let Some(node) = stack.pop() {
            let ptr = Arc::as_ptr(&node) as usize;
            if !visited.insert(ptr) {
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
