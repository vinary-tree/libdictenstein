//! Unit-generic lock-free dynamic DAWG core.
//!
//! This core is the non-blocking counterpart to [`super::core::DawgCore`].
//! Nodes are reference-counted and publish immutable edge-list snapshots via
//! atomic pointer swaps. Readers only perform atomic loads and `Arc` clones;
//! writers install new edge-list snapshots with CAS loops.

#[cfg(any(feature = "serialization", test))]
use super::core::DawgCore;
use crate::nonblocking::CasBackoff;
use crate::value::DictionaryValue;
use crate::CharUnit;
use arc_swap::{ArcSwap, ArcSwapOption};
use smallvec::SmallVec;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

const EDGE_LINEAR_SCAN_LIMIT: usize = 16;

/// Immutable sorted edge list published atomically by a node.
#[derive(Clone, Debug)]
pub(crate) struct LockFreeEdgeList<U: CharUnit, V: DictionaryValue> {
    pub(crate) edges: SmallVec<[(U, Arc<LockFreeDawgNode<U, V>>); 4]>,
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
    pub(crate) edges: ArcSwap<LockFreeEdgeList<U, V>>,
    pub(crate) is_final: AtomicBool,
    pub(crate) value: ArcSwapOption<V>,
}

impl<U: CharUnit, V: DictionaryValue> LockFreeDawgNode<U, V> {
    fn new(is_final: bool) -> Self {
        Self {
            edges: ArcSwap::from_pointee(LockFreeEdgeList::new()),
            is_final: AtomicBool::new(is_final),
            value: ArcSwapOption::empty(),
        }
    }

    #[inline]
    pub(crate) fn is_final(&self) -> bool {
        self.is_final.load(Ordering::Acquire)
    }

    #[inline]
    pub(crate) fn value(&self) -> Option<V> {
        if !self.is_final() {
            return None;
        }

        self.value.load().as_ref().map(|value| (**value).clone())
    }
}

impl<U: CharUnit, V: DictionaryValue> Drop for LockFreeDawgNode<U, V> {
    fn drop(&mut self) {
        let edges = self.edges.load_full();
        self.edges.store(Arc::new(LockFreeEdgeList::new()));

        let Ok(edge_list) = Arc::try_unwrap(edges) else {
            return;
        };

        let mut stack = Vec::with_capacity(edge_list.edges.len());
        for (_, child) in edge_list.edges {
            if let Ok(child) = Arc::try_unwrap(child) {
                stack.push(child);
            }
        }

        while let Some(node) = stack.pop() {
            let edges = node.edges.load_full();
            node.edges.store(Arc::new(LockFreeEdgeList::new()));

            if let Ok(edge_list) = Arc::try_unwrap(edges) {
                for (_, child) in edge_list.edges {
                    if let Ok(child) = Arc::try_unwrap(child) {
                        stack.push(child);
                    }
                }
            }
        }
    }
}

#[derive(Clone, Debug, Default)]
struct BuildNode<U: CharUnit, V: DictionaryValue> {
    is_final: bool,
    value: Option<V>,
    edges: Vec<(U, usize)>,
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

/// Unit-generic lock-free dynamic DAWG.
pub(crate) struct LockFreeDawg<U: CharUnit, V: DictionaryValue> {
    root: Arc<LockFreeDawgNode<U, V>>,
    term_count: AtomicUsize,
    needs_compaction: AtomicBool,
    active_writers: AtomicUsize,
    compaction_active: AtomicBool,
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
            root: Arc::new(Self::deep_clone_node(&self.root)),
            term_count: AtomicUsize::new(self.term_count()),
            needs_compaction: AtomicBool::new(self.needs_compaction()),
            active_writers: AtomicUsize::new(0),
            compaction_active: AtomicBool::new(false),
        }
    }
}

struct WriteGuard<'a, U: CharUnit, V: DictionaryValue> {
    dawg: &'a LockFreeDawg<U, V>,
}

impl<U: CharUnit, V: DictionaryValue> Drop for WriteGuard<'_, U, V> {
    fn drop(&mut self) {
        self.dawg.active_writers.fetch_sub(1, Ordering::AcqRel);
    }
}

impl<U: CharUnit, V: DictionaryValue> LockFreeDawg<U, V> {
    pub(crate) fn new() -> Self {
        Self {
            root: Arc::new(LockFreeDawgNode::new(false)),
            term_count: AtomicUsize::new(0),
            needs_compaction: AtomicBool::new(false),
            active_writers: AtomicUsize::new(0),
            compaction_active: AtomicBool::new(false),
        }
    }

    pub(crate) fn with_config(
        _auto_minimize_threshold: f32,
        _bloom_filter_capacity: Option<usize>,
    ) -> Self {
        Self::new()
    }

    #[cfg(any(feature = "serialization", test))]
    pub(crate) fn from_entries<I>(entries: I) -> Self
    where
        I: IntoIterator<Item = (Vec<U>, Option<V>)>,
    {
        let entries: Vec<_> = entries.into_iter().collect();
        let root = Self::build_minimized_root(&entries);
        Self {
            root,
            term_count: AtomicUsize::new(entries.len()),
            needs_compaction: AtomicBool::new(false),
            active_writers: AtomicUsize::new(0),
            compaction_active: AtomicBool::new(false),
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

    fn deep_clone_node(node: &Arc<LockFreeDawgNode<U, V>>) -> LockFreeDawgNode<U, V> {
        let edges = node.edges.load();
        let cloned_edges: SmallVec<_> = edges
            .edges
            .iter()
            .map(|(label, child)| (*label, Arc::new(Self::deep_clone_node(child))))
            .collect();

        let value = node.value.load().as_ref().map(|value| (**value).clone());

        LockFreeDawgNode {
            edges: ArcSwap::from_pointee(LockFreeEdgeList {
                edges: cloned_edges,
            }),
            is_final: AtomicBool::new(node.is_final()),
            value: match value {
                Some(value) => ArcSwapOption::from_pointee(Some(value)),
                None => ArcSwapOption::empty(),
            },
        }
    }

    fn begin_write(&self) -> WriteGuard<'_, U, V> {
        let mut backoff = CasBackoff::new();
        loop {
            while self.compaction_active.load(Ordering::Acquire) {
                backoff.snooze();
            }

            self.active_writers.fetch_add(1, Ordering::AcqRel);
            if !self.compaction_active.load(Ordering::Acquire) {
                return WriteGuard { dawg: self };
            }
            self.active_writers.fetch_sub(1, Ordering::AcqRel);
            backoff.snooze();
        }
    }

    fn acquire_compaction(&self) -> bool {
        self.compaction_active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn release_compaction(&self) {
        self.compaction_active.store(false, Ordering::Release);
    }

    fn wait_for_writers_to_quiesce(&self) {
        let mut backoff = CasBackoff::new();
        while self.active_writers.load(Ordering::Acquire) != 0 {
            backoff.snooze();
        }
    }

    #[inline]
    pub(crate) fn root_arc(&self) -> Arc<LockFreeDawgNode<U, V>> {
        self.root.clone()
    }

    pub(crate) fn insert_units(&self, units: &[U]) -> bool {
        let _write = self.begin_write();
        if units.is_empty() {
            if self
                .root
                .is_final
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                self.term_count.fetch_add(1, Ordering::Relaxed);
                return true;
            }
            return false;
        }

        let terminal = self.find_or_create_path(units);
        if terminal
            .is_final
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
        {
            self.term_count.fetch_add(1, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    pub(crate) fn insert_units_with_value(&self, units: &[U], value: V) -> bool {
        let _write = self.begin_write();
        if units.is_empty() {
            self.root.value.store(Some(Arc::new(value.clone())));
            if self
                .root
                .is_final
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                self.term_count.fetch_add(1, Ordering::Relaxed);
                return true;
            }
            self.root.value.store(Some(Arc::new(value)));
            return false;
        }

        let terminal = self.find_or_create_path(units);
        terminal.value.store(Some(Arc::new(value.clone())));
        if terminal
            .is_final
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
        {
            self.term_count.fetch_add(1, Ordering::Relaxed);
            true
        } else {
            terminal.value.store(Some(Arc::new(value)));
            false
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
        let _write = self.begin_write();
        let terminal = if units.is_empty() {
            self.root.clone()
        } else {
            self.find_or_create_path(units)
        };

        self.update_or_insert_terminal(&terminal, &default_value, &update_fn)
    }

    fn find_or_create_path(&self, units: &[U]) -> Arc<LockFreeDawgNode<U, V>> {
        let mut current = self.root.clone();
        for &label in units {
            current = Self::find_or_create_child(&current, label);
        }
        current
    }

    fn find_or_create_child(
        current: &Arc<LockFreeDawgNode<U, V>>,
        label: U,
    ) -> Arc<LockFreeDawgNode<U, V>> {
        let mut backoff = CasBackoff::new();
        loop {
            let edges = current.edges.load();
            if let Some(child) = edges.find(label) {
                return child.clone();
            }

            let new_node = Arc::new(LockFreeDawgNode::new(false));
            let new_edges = Arc::new(edges.with_edge(label, new_node.clone()));
            let previous = current.edges.compare_and_swap(&edges, new_edges);
            if Arc::ptr_eq(&previous, &edges) {
                return new_node;
            }
            backoff.snooze();
        }
    }

    fn update_value_cas<F>(node: &Arc<LockFreeDawgNode<U, V>>, default_value: &V, update_fn: &F)
    where
        F: Fn(&mut V),
    {
        let mut backoff = CasBackoff::new();
        loop {
            let current = node.value.load_full();
            let new_value = if let Some(value) = current.as_ref() {
                let mut updated = (**value).clone();
                update_fn(&mut updated);
                updated
            } else {
                default_value.clone()
            };

            if Self::compare_store_value(node, current, Some(Arc::new(new_value))) {
                return;
            }
            backoff.snooze();
        }
    }

    fn update_or_insert_terminal<F>(
        &self,
        terminal: &Arc<LockFreeDawgNode<U, V>>,
        default_value: &V,
        update_fn: &F,
    ) -> bool
    where
        F: Fn(&mut V),
    {
        let mut backoff = CasBackoff::new();
        loop {
            if terminal.is_final.load(Ordering::Acquire) {
                Self::update_value_cas(terminal, default_value, update_fn);
                return false;
            }

            if Self::compare_store_value(terminal, None, Some(Arc::new(default_value.clone()))) {
                if terminal
                    .is_final
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    self.term_count.fetch_add(1, Ordering::Relaxed);
                    return true;
                }

                Self::update_value_cas(terminal, default_value, update_fn);
                return false;
            }

            backoff.snooze();
        }
    }

    fn compare_store_value(
        node: &LockFreeDawgNode<U, V>,
        expected: Option<Arc<V>>,
        new_value: Option<Arc<V>>,
    ) -> bool {
        match expected {
            Some(expected) => {
                let previous = node.value.compare_and_swap(&expected, new_value);
                previous
                    .as_ref()
                    .is_some_and(|actual| Arc::ptr_eq(actual, &expected))
            }
            None => {
                let expected_none = None::<Arc<V>>;
                let previous = node.value.compare_and_swap(&expected_none, new_value);
                previous.as_ref().is_none()
            }
        }
    }

    pub(crate) fn get_units_value(&self, units: &[U]) -> Option<V> {
        let terminal = self.find_node(units)?;
        terminal.value()
    }

    #[inline]
    pub(crate) fn contains_units(&self, units: &[U]) -> bool {
        self.find_node(units)
            .is_some_and(|node| node.is_final.load(Ordering::Acquire))
    }

    pub(crate) fn remove_units(&self, units: &[U]) -> bool {
        let _write = self.begin_write();
        let Some(terminal) = self.find_node(units) else {
            return false;
        };

        let previous_value = terminal.value.load_full();
        if terminal
            .is_final
            .compare_exchange(true, false, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
        {
            let _ = Self::compare_store_value(&terminal, previous_value, None);
            self.term_count.fetch_sub(1, Ordering::Relaxed);
            self.needs_compaction.store(true, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    pub(crate) fn find_node(&self, units: &[U]) -> Option<Arc<LockFreeDawgNode<U, V>>> {
        let mut current = self.root.clone();
        for &label in units {
            let edges = current.edges.load();
            let child = edges.find(label)?.clone();
            current = child;
        }
        Some(current)
    }

    #[inline]
    pub(crate) fn term_count(&self) -> usize {
        self.term_count.load(Ordering::Relaxed)
    }

    pub(crate) fn node_count(&self) -> usize {
        Self::count_unique_nodes_from(&self.root)
    }

    #[inline]
    pub(crate) fn needs_compaction(&self) -> bool {
        self.needs_compaction.load(Ordering::Relaxed)
    }

    pub(crate) fn compact(&self) -> usize {
        self.rebuild_from_visible_entries()
    }

    pub(crate) fn minimize(&self) -> usize {
        self.rebuild_from_visible_entries()
    }

    fn rebuild_from_visible_entries(&self) -> usize {
        if !self.acquire_compaction() {
            return 0;
        }

        self.wait_for_writers_to_quiesce();

        let old_node_count = self.node_count();
        let entries = self.collect_visible_entries();
        let new_root = Self::build_minimized_root(&entries);
        let new_node_count = Self::count_unique_nodes_from(&new_root);

        self.root.edges.store(new_root.edges.load_full());
        self.root.value.store(new_root.value.load_full());
        self.root
            .is_final
            .store(new_root.is_final.load(Ordering::Acquire), Ordering::Release);

        self.term_count.store(entries.len(), Ordering::Release);
        self.needs_compaction.store(false, Ordering::Release);
        self.release_compaction();

        old_node_count.saturating_sub(new_node_count)
    }

    pub(crate) fn collect_visible_entries(&self) -> Vec<(Vec<U>, Option<V>)> {
        let mut entries = Vec::with_capacity(self.term_count());
        let mut path = Vec::with_capacity(32);

        struct Frame<U: CharUnit, V: DictionaryValue> {
            children: Vec<(U, Arc<LockFreeDawgNode<U, V>>)>,
            depth: usize,
        }

        if self.root.is_final.load(Ordering::Acquire) {
            let value = self
                .root
                .value
                .load_full()
                .as_ref()
                .map(|value| (**value).clone());
            entries.push((path.clone(), value));
        }

        let mut stack = Vec::with_capacity(64);
        let mut root_children: Vec<_> = self.root.edges.load_full().edges.iter().cloned().collect();
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
                    if child.is_final.load(Ordering::Acquire) {
                        let value = child
                            .value
                            .load_full()
                            .as_ref()
                            .map(|value| (**value).clone());
                        entries.push((path.clone(), value));
                    }

                    let mut children: Vec<_> =
                        child.edges.load_full().edges.iter().cloned().collect();
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

    fn build_minimized_root(entries: &[(Vec<U>, Option<V>)]) -> Arc<LockFreeDawgNode<U, V>> {
        let max_trie_nodes = 1 + entries.iter().map(|(units, _)| units.len()).sum::<usize>();
        let mut nodes = Vec::with_capacity(max_trie_nodes);
        nodes.push(BuildNode::<U, V>::default());
        let mut sorted_entries = entries.to_vec();
        sorted_entries.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));

        for (units, value) in sorted_entries {
            let mut node_idx = 0usize;
            for unit in units {
                let next = match nodes[node_idx]
                    .edges
                    .binary_search_by_key(&unit, |(edge_label, _)| *edge_label)
                {
                    Ok(pos) => nodes[node_idx].edges[pos].1,
                    Err(pos) => {
                        let new_idx = nodes.len();
                        nodes.push(BuildNode::default());
                        nodes[node_idx].edges.insert(pos, (unit, new_idx));
                        new_idx
                    }
                };
                node_idx = next;
            }
            nodes[node_idx].is_final = true;
            nodes[node_idx].value = value;
        }

        let mut interned = HashMap::with_capacity(nodes.len());
        Self::intern_build_node(0, &nodes, &mut interned, true)
    }

    fn intern_build_node(
        idx: usize,
        nodes: &[BuildNode<U, V>],
        interned: &mut HashMap<MergeSignature<U>, Arc<LockFreeDawgNode<U, V>>>,
        is_root: bool,
    ) -> Arc<LockFreeDawgNode<U, V>> {
        let build = &nodes[idx];
        let mut edges = SmallVec::<[(U, Arc<LockFreeDawgNode<U, V>>); 4]>::new();
        let mut signature_edges = Vec::with_capacity(build.edges.len());

        for (label, child_idx) in &build.edges {
            let child = Self::intern_build_node(*child_idx, nodes, interned, false);
            signature_edges.push((*label, Arc::as_ptr(&child) as usize));
            edges.push((*label, child));
        }

        if !is_root && !build.is_final && build.value.is_none() {
            let signature = MergeSignature {
                is_final: false,
                edges: signature_edges,
            };
            if let Some(existing) = interned.get(&signature) {
                return existing.clone();
            }

            let node = Arc::new(LockFreeDawgNode {
                edges: ArcSwap::from_pointee(LockFreeEdgeList { edges }),
                is_final: AtomicBool::new(false),
                value: ArcSwapOption::empty(),
            });
            interned.insert(signature, node.clone());
            return node;
        }

        Arc::new(LockFreeDawgNode {
            edges: ArcSwap::from_pointee(LockFreeEdgeList { edges }),
            is_final: AtomicBool::new(build.is_final),
            value: match build.value.clone() {
                Some(value) => ArcSwapOption::from_pointee(Some(value)),
                None => ArcSwapOption::empty(),
            },
        })
    }

    fn count_unique_nodes_from(root: &Arc<LockFreeDawgNode<U, V>>) -> usize {
        let mut visited = HashSet::new();
        let mut stack = vec![root.clone()];

        while let Some(node) = stack.pop() {
            let ptr = Arc::as_ptr(&node) as usize;
            if !visited.insert(ptr) {
                continue;
            }

            let edges = node.edges.load_full();
            for (_, child) in edges.edges.iter() {
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
}
