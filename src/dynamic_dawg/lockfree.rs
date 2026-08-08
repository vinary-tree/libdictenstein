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
use smallvec::SmallVec;
use std::collections::{HashMap, HashSet};
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

    #[cfg(any(feature = "serialization", test))]
    pub(crate) fn from_entries<I>(entries: I) -> Self
    where
        I: IntoIterator<Item = (Vec<U>, Option<V>)>,
    {
        let entries: Vec<_> = entries.into_iter().collect();
        let root = Self::build_minimized_root(&entries);
        Self {
            version: ArcSwap::from_pointee(GraphVersion {
                root,
                term_count: entries.len(),
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

    pub(crate) fn insert_units(&self, units: &[U]) -> bool {
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
            let current = self.version.load_full();
            let rewrite = Self::rewrite_path(&current.root, units, &terminal);
            if !rewrite.changed {
                return false;
            }
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

    pub(crate) fn insert_units_with_value(&self, units: &[U], value: V) -> bool {
        let terminal = |node: &Arc<LockFreeDawgNode<U, V>>| Rewrite {
            node: Self::copy_node(node.edges.clone(), true, Some(Arc::new(value.clone()))),
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

        // Child indices are always greater than their parents, so reverse index
        // order is a non-recursive post-order traversal. This keeps compaction
        // safe for arbitrarily long terms as well as ordinary branching tries.
        let mut interned: HashMap<MergeSignature<U>, Arc<LockFreeDawgNode<U, V>>> =
            HashMap::with_capacity(nodes.len());
        let mut built: Vec<Option<Arc<LockFreeDawgNode<U, V>>>> = vec![None; nodes.len()];
        for idx in (0..nodes.len()).rev() {
            let build = &nodes[idx];
            let mut edges = SmallVec::<[(U, Arc<LockFreeDawgNode<U, V>>); 4]>::new();
            let mut signature_edges = Vec::with_capacity(build.edges.len());

            for (label, child_idx) in &build.edges {
                let child = built[*child_idx]
                    .as_ref()
                    .expect("child must be built before its parent")
                    .clone();
                signature_edges.push((*label, Arc::as_ptr(&child) as usize));
                edges.push((*label, child));
            }

            let node = if idx != 0 && !build.is_final && build.value.is_none() {
                let signature = MergeSignature {
                    is_final: false,
                    edges: signature_edges,
                };
                if let Some(existing) = interned.get(&signature) {
                    existing.clone()
                } else {
                    let node = Arc::new(LockFreeDawgNode {
                        edges: LockFreeEdgeList { edges },
                        is_final: false,
                        value: None,
                    });
                    interned.insert(signature, node.clone());
                    node
                }
            } else {
                Arc::new(LockFreeDawgNode {
                    edges: LockFreeEdgeList { edges },
                    is_final: build.is_final,
                    value: build.value.clone().map(Arc::new),
                })
            };
            built[idx] = Some(node);
        }

        built[0].take().expect("the root is always built")
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
