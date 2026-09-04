//! Persistent suffix-tree-compatible dictionaries backed by native compact graphs.
//!
//! The byte and Unicode variants persist source/value records plus operation
//! WAL records, then rebuild and publish immutable, path-compressed suffix-tree
//! snapshots. New writes use atomically-published prepare/commit segment files;
//! recovery also reads the historical length-prefixed monolithic WAL. Reads
//! traverse the currently loaded graph without taking a writer lock. Retryable
//! mutations construct a new graph and publish it with pointer-identity CAS
//! after a prepared WAL record, then append a commit marker before
//! acknowledging the caller.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::iter::FusedIterator;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use arc_swap::ArcSwap;
use serde::{Deserialize, Serialize};

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::disk_manager::MmapDiskManager;
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::persistent_artrie::suffix_array::{
    sorted_byte_suffix_starts, sorted_char_boundary_suffix_starts,
};
use crate::persistent_artrie::RecoveryReport;
use crate::serialization::bincode_compat;
use crate::substring::{SubstringDictionary, SubstringMatch};
use crate::value::DictionaryValue;
use crate::{
    CharUnit, Dictionary, DictionaryNode, MappedDictionary, MappedDictionaryNode,
    MutableDictionary, MutableMappedDictionary, SyncStrategy,
};

const BYTE_MAGIC: [u8; 8] = *b"PSTREEB1";
const CHAR_MAGIC: [u8; 8] = *b"PSTREEC1";
const SNAPSHOT_VERSION: u32 = 2;
const LEGACY_SNAPSHOT_VERSION: u32 = 1;
const MAX_WAL_RECORD_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CAS_RETRIES: usize = 64;

trait PersistentSuffixTreeUnit:
    CharUnit + Serialize + serde::de::DeserializeOwned + fmt::Debug + Send + Sync
{
    const MAGIC: [u8; 8];

    fn suffix_starts(text: &str) -> Vec<usize>;

    fn suffix_units(text: &str, start_byte: usize) -> Vec<Self>;

    fn term_units(text: &str) -> Vec<Self> {
        Self::from_str(text)
    }
}

impl PersistentSuffixTreeUnit for u8 {
    const MAGIC: [u8; 8] = BYTE_MAGIC;

    fn suffix_starts(text: &str) -> Vec<usize> {
        sorted_byte_suffix_starts(text.as_bytes())
    }

    fn suffix_units(text: &str, start_byte: usize) -> Vec<Self> {
        text.as_bytes()[start_byte..].to_vec()
    }
}

impl PersistentSuffixTreeUnit for char {
    const MAGIC: [u8; 8] = CHAR_MAGIC;

    fn suffix_starts(text: &str) -> Vec<usize> {
        sorted_char_boundary_suffix_starts(text)
    }

    fn suffix_units(text: &str, start_byte: usize) -> Vec<Self> {
        text[start_byte..].chars().collect()
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct TreePosition {
    source_id: u64,
    start_byte: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(bound(
    serialize = "V: serde::Serialize",
    deserialize = "V: serde::de::DeserializeOwned"
))]
struct TreeSourceRecord<V: DictionaryValue> {
    id: u64,
    text: String,
    value: Option<V>,
    active: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(bound(
    serialize = "U: serde::Serialize",
    deserialize = "U: serde::de::DeserializeOwned"
))]
struct CompactSuffixTreeEdge<U: PersistentSuffixTreeUnit> {
    label: Vec<U>,
    target: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(bound(
    serialize = "U: serde::Serialize, V: serde::Serialize",
    deserialize = "U: serde::de::DeserializeOwned, V: serde::de::DeserializeOwned"
))]
struct CompactSuffixTreeNode<U: PersistentSuffixTreeUnit, V: DictionaryValue> {
    edges: Vec<CompactSuffixTreeEdge<U>>,
    positions: Vec<TreePosition>,
    value: Option<V>,
}

impl<U: PersistentSuffixTreeUnit, V: DictionaryValue> CompactSuffixTreeNode<U, V> {
    fn new() -> Self {
        Self {
            edges: Vec::new(),
            positions: Vec::new(),
            value: None,
        }
    }

    fn find_edge(&self, label: U) -> Option<usize> {
        self.edges
            .binary_search_by_key(&label, |edge| edge.label[0])
            .ok()
    }
}

struct RawSuffixTreeNode<U: PersistentSuffixTreeUnit, V: DictionaryValue> {
    children: BTreeMap<U, usize>,
    positions: Vec<TreePosition>,
    value: Option<V>,
}

impl<U: PersistentSuffixTreeUnit, V: DictionaryValue> RawSuffixTreeNode<U, V> {
    fn new() -> Self {
        Self {
            children: BTreeMap::new(),
            positions: Vec::new(),
            value: None,
        }
    }

    fn is_compression_boundary(&self) -> bool {
        self.value.is_some() || !self.positions.is_empty() || self.children.len() != 1
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(bound(
    serialize = "U: serde::Serialize, V: serde::Serialize",
    deserialize = "U: serde::de::DeserializeOwned, V: serde::de::DeserializeOwned"
))]
struct NativeSuffixTreeGraph<U: PersistentSuffixTreeUnit, V: DictionaryValue> {
    nodes: Vec<CompactSuffixTreeNode<U, V>>,
    sources: Vec<TreeSourceRecord<V>>,
    #[serde(skip)]
    sorted_active_source_indices: Vec<usize>,
    explicit_values: BTreeMap<Vec<U>, V>,
    needs_compaction: bool,
}

enum LocatedPath {
    Node(usize),
    InsideEdge {
        parent: usize,
        edge_index: usize,
        offset: usize,
    },
}

impl<U: PersistentSuffixTreeUnit, V: DictionaryValue> NativeSuffixTreeGraph<U, V> {
    fn new() -> Self {
        Self {
            nodes: vec![CompactSuffixTreeNode::new()],
            sources: Vec::new(),
            sorted_active_source_indices: Vec::new(),
            explicit_values: BTreeMap::new(),
            needs_compaction: false,
        }
    }

    fn from_snapshot_parts(
        sources: Vec<TreeSourceRecord<V>>,
        explicit_values: BTreeMap<Vec<U>, V>,
        needs_compaction: bool,
    ) -> Self {
        let mut graph = Self {
            nodes: Vec::new(),
            sources,
            sorted_active_source_indices: Vec::new(),
            explicit_values,
            needs_compaction,
        };
        graph.rebuild_record_index();
        graph.rebuild_nodes();
        graph
    }

    fn node_count(&self) -> usize {
        self.nodes.len()
    }

    fn edge_count(&self) -> usize {
        self.nodes.iter().map(|node| node.edges.len()).sum()
    }

    fn active_count(&self) -> usize {
        self.sorted_active_source_indices.len()
    }

    fn rebuild_record_index(&mut self) {
        self.sorted_active_source_indices = self
            .sources
            .iter()
            .enumerate()
            .filter_map(|(index, record)| record.active.then_some(index))
            .collect();
        self.sorted_active_source_indices.sort_by(|&left, &right| {
            self.sources[left]
                .text
                .cmp(&self.sources[right].text)
                .then_with(|| self.sources[left].id.cmp(&self.sources[right].id))
        });
    }

    fn source_texts(&self) -> Vec<String> {
        self.sources
            .iter()
            .map(|record| record.text.clone())
            .collect()
    }

    fn active_texts(&self) -> Vec<String> {
        self.sources
            .iter()
            .filter(|record| record.active)
            .map(|record| record.text.clone())
            .collect()
    }

    fn active_source_ids(&self) -> HashSet<u64> {
        self.sources
            .iter()
            .filter(|record| record.active)
            .map(|record| record.id)
            .collect()
    }

    fn insert_source(&mut self, text: &str, value: Option<V>) -> bool {
        let id = self.sources.len() as u64;
        self.sources.push(TreeSourceRecord {
            id,
            text: text.to_string(),
            value,
            active: true,
        });
        self.rebuild_record_index();
        self.rebuild_nodes();
        true
    }

    fn remove_source(&mut self, text: &str) -> bool {
        if let Some(record) = self
            .sources
            .iter_mut()
            .find(|record| record.active && record.text == text)
        {
            record.active = false;
            self.needs_compaction = true;
            self.rebuild_record_index();
            self.rebuild_nodes();
            return true;
        }
        false
    }

    fn clear(&mut self) {
        self.sources.clear();
        self.sorted_active_source_indices.clear();
        self.explicit_values.clear();
        self.needs_compaction = false;
        self.nodes = vec![CompactSuffixTreeNode::new()];
    }

    fn compact(&mut self) -> usize {
        if !self.needs_compaction {
            return 0;
        }

        let before = self.sources.len();
        let mut compacted = Vec::with_capacity(self.active_count());
        for record in self.sources.iter().filter(|record| record.active) {
            let mut record = record.clone();
            record.id = compacted.len() as u64;
            compacted.push(record);
        }
        self.sources = compacted;
        self.rebuild_record_index();
        self.needs_compaction = false;
        self.rebuild_nodes();
        before.saturating_sub(self.sources.len())
    }

    fn set_value(&mut self, text: &str, value: V) {
        let units = U::term_units(text);
        if self.path_exists_units(&units) {
            self.explicit_values.insert(units, value);
            self.rebuild_nodes();
        } else {
            self.insert_source(text, Some(value));
        }
    }

    fn update_or_insert<F>(&mut self, text: &str, default_value: V, update_fn: F) -> (bool, V)
    where
        F: Fn(&mut V),
    {
        let units = U::term_units(text);
        if let Some(mut value) = self.get_value_units(&units) {
            update_fn(&mut value);
            self.explicit_values.insert(units, value.clone());
            self.rebuild_nodes();
            return (false, value);
        }

        if self.contains_live_units(&units) {
            self.explicit_values.insert(units, default_value.clone());
            self.rebuild_nodes();
            return (true, default_value);
        }

        self.insert_source(text, Some(default_value.clone()));
        (true, default_value)
    }

    fn get_value_text(&self, text: &str) -> Option<V> {
        self.get_value_units(&U::term_units(text))
    }

    fn get_value_units(&self, units: &[U]) -> Option<V> {
        match self.locate_units(units)? {
            LocatedPath::Node(node) => self.nodes.get(node)?.value.clone(),
            LocatedPath::InsideEdge { .. } => None,
        }
    }

    fn path_exists_units(&self, units: &[U]) -> bool {
        self.locate_units(units).is_some()
    }

    fn contains_live_units(&self, units: &[U]) -> bool {
        if units.is_empty() {
            return true;
        }

        let Some(located) = self.locate_units(units) else {
            return false;
        };
        let active = self.active_source_ids();
        match located {
            LocatedPath::Node(node) => self.subtree_has_active_position_or_value(node, &active),
            LocatedPath::InsideEdge {
                parent, edge_index, ..
            } => {
                let Some(edge) = self
                    .nodes
                    .get(parent)
                    .and_then(|node| node.edges.get(edge_index))
                else {
                    return false;
                };
                self.subtree_has_active_position_or_value(edge.target, &active)
            }
        }
    }

    fn contains_live_text(&self, text: &str) -> bool {
        self.contains_live_units(&U::term_units(text))
    }

    fn next_units_after_path(&self, units: &[U]) -> Vec<U> {
        match self.locate_units(units) {
            Some(LocatedPath::Node(node)) => self
                .nodes
                .get(node)
                .map(|node| node.edges.iter().map(|edge| edge.label[0]).collect())
                .unwrap_or_default(),
            Some(LocatedPath::InsideEdge {
                parent,
                edge_index,
                offset,
            }) => self
                .nodes
                .get(parent)
                .and_then(|node| node.edges.get(edge_index))
                .and_then(|edge| edge.label.get(offset).copied())
                .into_iter()
                .collect(),
            None => Vec::new(),
        }
    }

    fn match_positions(&self, substring: &str) -> Vec<(usize, usize)> {
        if substring.is_empty() {
            return Vec::new();
        }

        let units = U::term_units(substring);
        let Some(located) = self.locate_units(&units) else {
            return Vec::new();
        };

        let mut positions = Vec::new();
        match located {
            LocatedPath::Node(node) => self.collect_subtree_positions(node, &mut positions),
            LocatedPath::InsideEdge {
                parent, edge_index, ..
            } => {
                if let Some(edge) = self
                    .nodes
                    .get(parent)
                    .and_then(|node| node.edges.get(edge_index))
                {
                    self.collect_subtree_positions(edge.target, &mut positions);
                }
            }
        }

        let active = self.active_source_ids();
        let mut result = Vec::new();
        for position in positions {
            if !active.contains(&position.source_id) {
                continue;
            }
            if let Ok(source_id) = usize::try_from(position.source_id) {
                result.push((source_id, position.start_byte + substring.len()));
            }
        }
        result.sort_unstable();
        result.dedup();
        result
    }

    fn locate_units(&self, units: &[U]) -> Option<LocatedPath> {
        if units.is_empty() {
            return Some(LocatedPath::Node(0));
        }

        let mut node = 0usize;
        let mut consumed = 0usize;
        while consumed < units.len() {
            let edge_index = self.nodes.get(node)?.find_edge(units[consumed])?;
            let edge = self.nodes.get(node)?.edges.get(edge_index)?;
            let mut offset = 0usize;
            while offset < edge.label.len() && consumed + offset < units.len() {
                if edge.label[offset] != units[consumed + offset] {
                    return None;
                }
                offset += 1;
            }

            if consumed + offset == units.len() {
                if offset == edge.label.len() {
                    return Some(LocatedPath::Node(edge.target));
                }
                return Some(LocatedPath::InsideEdge {
                    parent: node,
                    edge_index,
                    offset,
                });
            }

            if offset != edge.label.len() {
                return None;
            }

            consumed += offset;
            node = edge.target;
        }

        Some(LocatedPath::Node(node))
    }

    fn subtree_has_active_position_or_value(&self, node: usize, active: &HashSet<u64>) -> bool {
        let mut stack = vec![node];
        while let Some(node) = stack.pop() {
            let Some(node_ref) = self.nodes.get(node) else {
                continue;
            };
            if node_ref.value.is_some() {
                return true;
            }
            if node_ref
                .positions
                .iter()
                .any(|position| active.contains(&position.source_id))
            {
                return true;
            }
            stack.extend(node_ref.edges.iter().map(|edge| edge.target));
        }
        false
    }

    fn collect_subtree_positions(&self, node: usize, out: &mut Vec<TreePosition>) {
        let mut stack = vec![node];
        while let Some(node) = stack.pop() {
            let Some(node_ref) = self.nodes.get(node) else {
                continue;
            };
            out.extend(node_ref.positions.iter().cloned());
            stack.extend(node_ref.edges.iter().map(|edge| edge.target));
        }
    }

    fn rebuild_nodes(&mut self) {
        let mut raw = vec![RawSuffixTreeNode::<U, V>::new()];
        for record in self.sources.iter().filter(|record| record.active) {
            insert_source_suffixes::<U, V>(&mut raw, record);
        }
        for (units, value) in &self.explicit_values {
            insert_raw_value(&mut raw, units, value.clone());
        }

        let mut nodes = Vec::new();
        compress_raw_node(&raw, 0, &mut nodes);
        if nodes.is_empty() {
            nodes.push(CompactSuffixTreeNode::new());
        }
        self.nodes = nodes;
    }
}

fn insert_source_suffixes<U, V>(
    raw: &mut Vec<RawSuffixTreeNode<U, V>>,
    record: &TreeSourceRecord<V>,
) where
    U: PersistentSuffixTreeUnit,
    V: DictionaryValue,
{
    if record.text.is_empty() {
        insert_raw_suffix(
            raw,
            &[],
            TreePosition {
                source_id: record.id,
                start_byte: 0,
            },
            record.value.clone(),
        );
        return;
    }

    for start in U::suffix_starts(&record.text) {
        let suffix = U::suffix_units(&record.text, start);
        let suffix_value = if start == 0 {
            record.value.clone()
        } else {
            None
        };
        insert_raw_suffix(
            raw,
            &suffix,
            TreePosition {
                source_id: record.id,
                start_byte: start,
            },
            suffix_value,
        );
    }
}

fn insert_raw_suffix<U, V>(
    raw: &mut Vec<RawSuffixTreeNode<U, V>>,
    units: &[U],
    position: TreePosition,
    value: Option<V>,
) where
    U: PersistentSuffixTreeUnit,
    V: DictionaryValue,
{
    let mut node = 0usize;
    for &unit in units {
        let next = match raw[node].children.get(&unit).copied() {
            Some(next) => next,
            None => {
                let next = raw.len();
                raw.push(RawSuffixTreeNode::new());
                raw[node].children.insert(unit, next);
                next
            }
        };
        node = next;
    }

    if !raw[node].positions.iter().any(|existing| {
        existing.source_id == position.source_id && existing.start_byte == position.start_byte
    }) {
        raw[node].positions.push(position);
    }
    if let Some(value) = value {
        raw[node].value = Some(value);
    }
}

fn insert_raw_value<U, V>(raw: &mut Vec<RawSuffixTreeNode<U, V>>, units: &[U], value: V)
where
    U: PersistentSuffixTreeUnit,
    V: DictionaryValue,
{
    let mut node = 0usize;
    for &unit in units {
        let next = match raw[node].children.get(&unit).copied() {
            Some(next) => next,
            None => {
                let next = raw.len();
                raw.push(RawSuffixTreeNode::new());
                raw[node].children.insert(unit, next);
                next
            }
        };
        node = next;
    }
    raw[node].value = Some(value);
}

fn compress_raw_node<U, V>(
    raw: &[RawSuffixTreeNode<U, V>],
    raw_index: usize,
    nodes: &mut Vec<CompactSuffixTreeNode<U, V>>,
) -> usize
where
    U: PersistentSuffixTreeUnit,
    V: DictionaryValue,
{
    let root_compact_index = nodes.len();
    let raw_node = &raw[raw_index];
    nodes.push(CompactSuffixTreeNode {
        edges: Vec::new(),
        positions: raw_node.positions.clone(),
        value: raw_node.value.clone(),
    });
    // A task identifies one raw edge whose compressed target has not yet been
    // materialized. Reverse-pushing ordered children makes `pop` reproduce the
    // recursive depth-first order without retaining iterator-heavy frames.
    let mut pending: smallvec::SmallVec<[(usize, U, usize); 16]> = smallvec::SmallVec::new();
    pending.extend(
        raw_node
            .children
            .iter()
            .rev()
            .map(|(&unit, &child)| (root_compact_index, unit, child)),
    );

    while let Some((parent_compact_index, unit, child)) = pending.pop() {
        let mut label = vec![unit];
        let mut cursor = child;
        while !raw[cursor].is_compression_boundary() {
            let (&next_unit, &next_child) = raw[cursor]
                .children
                .iter()
                .next()
                .expect("non-boundary raw suffix-tree node has one child");
            label.push(next_unit);
            cursor = next_child;
        }

        let compact_index = nodes.len();
        nodes.push(CompactSuffixTreeNode {
            edges: Vec::new(),
            positions: raw[cursor].positions.clone(),
            value: raw[cursor].value.clone(),
        });
        nodes[parent_compact_index]
            .edges
            .push(CompactSuffixTreeEdge {
                label,
                target: compact_index,
            });
        pending.extend(
            raw[cursor]
                .children
                .iter()
                .rev()
                .map(|(&unit, &child)| (compact_index, unit, child)),
        );
    }

    root_compact_index
}

#[cfg(test)]
mod compression_stack_safety_tests {
    use super::{compress_raw_node, RawSuffixTreeNode};

    #[test]
    fn one_hundred_thousand_boundary_nodes_compress_without_native_recursion() {
        const DEPTH: usize = 100_000;
        let mut raw = Vec::<RawSuffixTreeNode<u8, ()>>::with_capacity(DEPTH + 1);
        raw.push(RawSuffixTreeNode::new());
        for parent in 0..DEPTH {
            raw.push(RawSuffixTreeNode::new());
            raw[parent].children.insert(b'x', parent + 1);
            // A value makes every node a compression boundary, forcing the
            // explicit frame machine to reach the full graph depth.
            raw[parent + 1].value = Some(());
        }

        let mut compact = Vec::new();
        assert_eq!(compress_raw_node(&raw, 0, &mut compact), 0);
        assert_eq!(compact.len(), DEPTH + 1);
        for (index, node) in compact.iter().take(DEPTH).enumerate() {
            assert_eq!(node.edges.len(), 1);
            assert_eq!(node.edges[0].label, vec![b'x']);
            assert_eq!(node.edges[0].target, index + 1);
        }
        assert!(compact[DEPTH].edges.is_empty());
        assert_eq!(compact[DEPTH].value, Some(()));
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(bound(
    serialize = "U: serde::Serialize, V: serde::Serialize",
    deserialize = "U: serde::de::DeserializeOwned, V: serde::de::DeserializeOwned"
))]
struct NativeSuffixTreeSnapshot<U: PersistentSuffixTreeUnit, V: DictionaryValue> {
    magic: [u8; 8],
    version: u32,
    sources: Vec<TreeSourceRecord<V>>,
    explicit_values: Vec<(Vec<U>, V)>,
    needs_compaction: bool,
    checkpoint_op_id: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(bound(
    serialize = "U: serde::Serialize, V: serde::Serialize",
    deserialize = "U: serde::de::DeserializeOwned, V: serde::de::DeserializeOwned"
))]
struct NativeSuffixTreeSnapshotV1<U: PersistentSuffixTreeUnit, V: DictionaryValue> {
    magic: [u8; 8],
    version: u32,
    sources: Vec<TreeSourceRecord<V>>,
    explicit_values: Vec<(Vec<U>, V)>,
    needs_compaction: bool,
}

struct LoadedSuffixTreeSnapshot<U: PersistentSuffixTreeUnit, V: DictionaryValue> {
    graph: NativeSuffixTreeGraph<U, V>,
    checkpoint_op_id: u64,
    folds_legacy_wal: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(bound(
    serialize = "V: serde::Serialize",
    deserialize = "V: serde::de::DeserializeOwned"
))]
enum NativeSuffixTreeWalOp<V: DictionaryValue> {
    Insert { text: String, value: Option<V> },
    Remove { text: String },
    SetValue { text: String, value: V },
    Clear,
    Compact,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(bound(
    serialize = "V: serde::Serialize",
    deserialize = "V: serde::de::DeserializeOwned"
))]
enum NativeSuffixTreeWalRecord<V: DictionaryValue> {
    Insert {
        text: String,
        value: Option<V>,
    },
    Remove {
        text: String,
    },
    SetValue {
        text: String,
        value: V,
    },
    Clear,
    Compact,
    Prepare {
        op_id: u64,
        op: NativeSuffixTreeWalOp<V>,
    },
    Commit {
        op_id: u64,
    },
}

#[derive(Debug, Default)]
struct SuffixTreeWalReplayReport {
    replayed: u64,
    max_op_id: u64,
}

struct NativeSuffixTreeIndex<U: PersistentSuffixTreeUnit, V: DictionaryValue> {
    graph: ArcSwap<NativeSuffixTreeGraph<U, V>>,
    path: Option<PathBuf>,
    next_op_id: AtomicU64,
    committed_op_id: AtomicU64,
    inflight_publications: AtomicUsize,
}

fn wal_path(path: &Path) -> PathBuf {
    let mut wal = path.to_path_buf();
    wal.set_extension("streewal");
    wal
}

fn wal_segment_dir(path: &Path) -> PathBuf {
    let mut dir = path.to_path_buf();
    dir.set_extension("streewal.d");
    dir
}

fn wal_segment_dir_from_wal(path: &Path) -> PathBuf {
    let mut dir = path.to_path_buf();
    dir.set_extension("streewal.d");
    dir
}

fn tmp_snapshot_path(path: &Path) -> PathBuf {
    let mut tmp = path.to_path_buf();
    tmp.set_extension("streetmp");
    tmp
}

fn io_error(operation: impl Into<String>, path: &Path, source: io::Error) -> PersistentARTrieError {
    PersistentARTrieError::io_error(operation, path.display().to_string(), source)
}

fn codec_error(context: &str, error: impl fmt::Display) -> PersistentARTrieError {
    PersistentARTrieError::corrupted(format!("{context}: {error}"))
}

fn serialize_bytes<T: Serialize>(context: &str, value: &T) -> Result<Vec<u8>> {
    bincode_compat::serialize(value).map_err(|error| codec_error(context, error))
}

fn deserialize_bytes<T: serde::de::DeserializeOwned>(context: &str, bytes: &[u8]) -> Result<T> {
    bincode_compat::deserialize(bytes).map_err(|error| codec_error(context, error))
}

impl<U: PersistentSuffixTreeUnit, V: DictionaryValue> NativeSuffixTreeIndex<U, V> {
    fn new_in_memory() -> Self {
        Self {
            graph: ArcSwap::from_pointee(NativeSuffixTreeGraph::new()),
            path: None,
            next_op_id: AtomicU64::new(1),
            committed_op_id: AtomicU64::new(0),
            inflight_publications: AtomicUsize::new(0),
        }
    }

    fn create(path: &Path) -> Result<Self> {
        let graph = NativeSuffixTreeGraph::new();
        write_snapshot_file::<U, V>(path, &graph, 0)?;
        truncate_wal(&wal_path(path))?;
        Ok(Self {
            graph: ArcSwap::from_pointee(graph),
            path: Some(path.to_path_buf()),
            next_op_id: AtomicU64::new(1),
            committed_op_id: AtomicU64::new(0),
            inflight_publications: AtomicUsize::new(0),
        })
    }

    fn open(path: &Path) -> Result<(Self, u64)> {
        let loaded = read_snapshot_file::<U, V>(path)?;
        let mut graph = loaded.graph;
        let report = replay_wal::<U, V>(
            &mut graph,
            &wal_path(path),
            loaded.checkpoint_op_id,
            loaded.folds_legacy_wal,
        )?;
        let max_op_id = loaded.checkpoint_op_id.max(report.max_op_id);
        Ok((
            Self {
                graph: ArcSwap::from_pointee(graph),
                path: Some(path.to_path_buf()),
                next_op_id: AtomicU64::new(max_op_id.saturating_add(1)),
                committed_op_id: AtomicU64::new(max_op_id),
                inflight_publications: AtomicUsize::new(0),
            },
            report.replayed,
        ))
    }

    fn load(&self) -> Arc<NativeSuffixTreeGraph<U, V>> {
        self.graph.load_full()
    }

    fn append_record(&self, record: &NativeSuffixTreeWalRecord<V>) -> Result<()> {
        let Some(path) = self.path.as_ref() else {
            return Ok(());
        };
        append_wal_segment(&wal_segment_dir(path), record)
    }

    fn apply_op(graph: &mut NativeSuffixTreeGraph<U, V>, op: NativeSuffixTreeWalOp<V>) -> bool {
        match op {
            NativeSuffixTreeWalOp::Insert { text, value } => graph.insert_source(&text, value),
            NativeSuffixTreeWalOp::Remove { text } => graph.remove_source(&text),
            NativeSuffixTreeWalOp::SetValue { text, value } => {
                graph.set_value(&text, value);
                true
            }
            NativeSuffixTreeWalOp::Clear => {
                graph.clear();
                true
            }
            NativeSuffixTreeWalOp::Compact => graph.compact() > 0,
        }
    }

    fn mutate_retryable(&self, op: NativeSuffixTreeWalOp<V>) -> Result<bool> {
        if self.path.is_none() {
            loop {
                let current = self.graph.load_full();
                let mut next = (*current).clone();
                let result = Self::apply_op(&mut next, op.clone());
                let previous = self.graph.compare_and_swap(&current, Arc::new(next));
                if Arc::ptr_eq(&previous, &current) {
                    return Ok(result);
                }
            }
        }

        for _ in 0..MAX_CAS_RETRIES {
            let op_id = self.next_op_id.fetch_add(1, Ordering::Relaxed);
            self.append_record(&NativeSuffixTreeWalRecord::Prepare {
                op_id,
                op: op.clone(),
            })?;
            let current = self.graph.load_full();
            let mut next = (*current).clone();
            let result = Self::apply_op(&mut next, op.clone());
            self.inflight_publications.fetch_add(1, Ordering::SeqCst);
            let previous = self.graph.compare_and_swap(&current, Arc::new(next));
            if Arc::ptr_eq(&previous, &current) {
                let commit_result =
                    self.append_record(&NativeSuffixTreeWalRecord::Commit { op_id });
                if commit_result.is_ok() {
                    self.committed_op_id.fetch_max(op_id, Ordering::SeqCst);
                }
                self.inflight_publications.fetch_sub(1, Ordering::SeqCst);
                commit_result?;
                return Ok(result);
            }
            self.inflight_publications.fetch_sub(1, Ordering::SeqCst);
        }

        Err(PersistentARTrieError::internal(format!(
            "native suffix tree CAS failed after {MAX_CAS_RETRIES} retries"
        )))
    }

    fn insert(&self, text: &str, value: Option<V>) -> Result<bool> {
        self.mutate_retryable(NativeSuffixTreeWalOp::Insert {
            text: text.to_string(),
            value,
        })
    }

    fn remove(&self, text: &str) -> Result<bool> {
        self.mutate_retryable(NativeSuffixTreeWalOp::Remove {
            text: text.to_string(),
        })
    }

    fn clear(&self) -> Result<()> {
        self.mutate_retryable(NativeSuffixTreeWalOp::Clear)
            .map(|_| ())
    }

    fn compact(&self) -> Result<usize> {
        self.mutate_retryable(NativeSuffixTreeWalOp::Compact)
            .map(usize::from)
    }

    fn update_or_insert<F>(&self, text: &str, default_value: V, update_fn: F) -> Result<bool>
    where
        F: Fn(&mut V),
    {
        if self.path.is_none() {
            loop {
                let current = self.graph.load_full();
                let mut next = (*current).clone();
                let (was_new, _) = next.update_or_insert(text, default_value.clone(), &update_fn);
                let previous = self.graph.compare_and_swap(&current, Arc::new(next));
                if Arc::ptr_eq(&previous, &current) {
                    return Ok(was_new);
                }
            }
        }

        for _ in 0..MAX_CAS_RETRIES {
            let current = self.graph.load_full();
            let mut next = (*current).clone();
            let (was_new, value) = next.update_or_insert(text, default_value.clone(), &update_fn);
            let op_id = self.next_op_id.fetch_add(1, Ordering::Relaxed);
            self.append_record(&NativeSuffixTreeWalRecord::Prepare {
                op_id,
                op: NativeSuffixTreeWalOp::SetValue {
                    text: text.to_string(),
                    value,
                },
            })?;
            self.inflight_publications.fetch_add(1, Ordering::SeqCst);
            let previous = self.graph.compare_and_swap(&current, Arc::new(next));
            if Arc::ptr_eq(&previous, &current) {
                let commit_result =
                    self.append_record(&NativeSuffixTreeWalRecord::Commit { op_id });
                if commit_result.is_ok() {
                    self.committed_op_id.fetch_max(op_id, Ordering::SeqCst);
                }
                self.inflight_publications.fetch_sub(1, Ordering::SeqCst);
                commit_result?;
                return Ok(was_new);
            }
            self.inflight_publications.fetch_sub(1, Ordering::SeqCst);
        }

        Err(PersistentARTrieError::internal(format!(
            "native suffix tree value update CAS failed after {MAX_CAS_RETRIES} retries"
        )))
    }

    fn checkpoint(&self) -> Result<()> {
        let Some(path) = self.path.as_ref() else {
            return Ok(());
        };
        for _ in 0..MAX_CAS_RETRIES {
            if self.inflight_publications.load(Ordering::SeqCst) != 0 {
                std::thread::yield_now();
                continue;
            }
            let committed_before = self.committed_op_id.load(Ordering::SeqCst);
            let graph = self.graph.load_full();
            let committed_after = self.committed_op_id.load(Ordering::SeqCst);
            if committed_before == committed_after
                && self.inflight_publications.load(Ordering::SeqCst) == 0
            {
                write_snapshot_file::<U, V>(path, graph.as_ref(), committed_after)?;
                prune_wal_segments(&wal_segment_dir(path), committed_after)?;
                return Ok(());
            }
            std::thread::yield_now();
        }
        log::debug!(
            "native suffix tree checkpoint skipped after {MAX_CAS_RETRIES} unstable attempts"
        );
        Ok(())
    }
}

fn write_snapshot_file<U: PersistentSuffixTreeUnit, V: DictionaryValue>(
    path: &Path,
    graph: &NativeSuffixTreeGraph<U, V>,
    checkpoint_op_id: u64,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            io_error(
                "create suffix-tree snapshot parent directory",
                parent,
                error,
            )
        })?;
    }

    let snapshot = NativeSuffixTreeSnapshot {
        magic: U::MAGIC,
        version: SNAPSHOT_VERSION,
        sources: graph.sources.clone(),
        explicit_values: graph
            .explicit_values
            .iter()
            .map(|(units, value)| (units.clone(), value.clone()))
            .collect(),
        needs_compaction: graph.needs_compaction,
        checkpoint_op_id,
    };
    let bytes = serialize_bytes("serialize native suffix-tree snapshot", &snapshot)?;
    let tmp = tmp_snapshot_path(path);
    {
        let mut file = File::create(&tmp)
            .map_err(|error| io_error("create suffix-tree snapshot", &tmp, error))?;
        file.write_all(&bytes)
            .map_err(|error| io_error("write suffix-tree snapshot", &tmp, error))?;
        file.sync_all()
            .map_err(|error| io_error("sync suffix-tree snapshot", &tmp, error))?;
    }
    fs::rename(&tmp, path).map_err(|error| io_error("install suffix-tree snapshot", path, error))
}

fn read_snapshot_file<U: PersistentSuffixTreeUnit, V: DictionaryValue>(
    path: &Path,
) -> Result<LoadedSuffixTreeSnapshot<U, V>> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|error| io_error("open suffix-tree snapshot", path, error))?
        .read_to_end(&mut bytes)
        .map_err(|error| io_error("read suffix-tree snapshot", path, error))?;
    if bytes.len() < 12 {
        return Err(PersistentARTrieError::corrupted(format!(
            "native suffix-tree snapshot is too short: {} bytes",
            bytes.len()
        )));
    }
    let magic: [u8; 8] = bytes[0..8]
        .try_into()
        .expect("slice length checked for suffix-tree snapshot magic");
    if magic != U::MAGIC {
        return Err(PersistentARTrieError::InvalidMagic {
            expected: u64::from_le_bytes(U::MAGIC),
            found: u64::from_le_bytes(magic),
        });
    }
    let version = u32::from_le_bytes(
        bytes[8..12]
            .try_into()
            .expect("slice length checked for suffix-tree snapshot version"),
    );
    match version {
        SNAPSHOT_VERSION => {
            let snapshot: NativeSuffixTreeSnapshot<U, V> =
                deserialize_bytes("deserialize native suffix-tree snapshot", &bytes)?;
            let explicit_values = snapshot.explicit_values.into_iter().collect();
            Ok(LoadedSuffixTreeSnapshot {
                graph: NativeSuffixTreeGraph::from_snapshot_parts(
                    snapshot.sources,
                    explicit_values,
                    snapshot.needs_compaction,
                ),
                checkpoint_op_id: snapshot.checkpoint_op_id,
                folds_legacy_wal: true,
            })
        }
        LEGACY_SNAPSHOT_VERSION => {
            let snapshot: NativeSuffixTreeSnapshotV1<U, V> =
                deserialize_bytes("deserialize native suffix-tree snapshot v1", &bytes)?;
            let explicit_values = snapshot.explicit_values.into_iter().collect();
            Ok(LoadedSuffixTreeSnapshot {
                graph: NativeSuffixTreeGraph::from_snapshot_parts(
                    snapshot.sources,
                    explicit_values,
                    snapshot.needs_compaction,
                ),
                checkpoint_op_id: 0,
                folds_legacy_wal: false,
            })
        }
        found => Err(PersistentARTrieError::UnsupportedVersion {
            max_supported: SNAPSHOT_VERSION,
            found,
        }),
    }
}

fn truncate_wal(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| io_error("create suffix-tree WAL parent directory", parent, error))?;
    }
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(|error| io_error("truncate suffix-tree WAL", path, error))?;
    file.sync_all()
        .map_err(|error| io_error("sync suffix-tree WAL", path, error))?;
    let segment_dir = wal_segment_dir_from_wal(path);
    if segment_dir.exists() {
        fs::remove_dir_all(&segment_dir).map_err(|error| {
            io_error(
                "clear suffix-tree WAL segment directory",
                &segment_dir,
                error,
            )
        })?;
    }
    fs::create_dir_all(&segment_dir).map_err(|error| {
        io_error(
            "create suffix-tree WAL segment directory",
            &segment_dir,
            error,
        )
    })
}

fn wal_segment_id_and_kind<V: DictionaryValue>(
    record: &NativeSuffixTreeWalRecord<V>,
) -> Result<(u64, &'static str)> {
    match record {
        NativeSuffixTreeWalRecord::Prepare { op_id, .. } => Ok((*op_id, "prepare")),
        NativeSuffixTreeWalRecord::Commit { op_id } => Ok((*op_id, "commit")),
        _ => Err(PersistentARTrieError::internal(
            "native suffix-tree segment WAL only accepts prepare/commit records".to_string(),
        )),
    }
}

fn append_wal_segment<V: DictionaryValue>(
    dir: &Path,
    record: &NativeSuffixTreeWalRecord<V>,
) -> Result<()> {
    fs::create_dir_all(dir)
        .map_err(|error| io_error("create suffix-tree WAL segment directory", dir, error))?;
    let payload = serialize_bytes("serialize native suffix-tree WAL record", record)?;
    let len = payload.len() as u64;
    let mut frame = Vec::with_capacity(8 + payload.len());
    frame.extend_from_slice(&len.to_le_bytes());
    frame.extend_from_slice(&payload);
    let (op_id, kind) = wal_segment_id_and_kind(record)?;
    let final_path = dir.join(format!("{op_id:020}.{kind}.wal"));
    let tmp_path = dir.join(format!("{op_id:020}.{kind}.{}.tmp", std::process::id()));
    let _ = fs::remove_file(&tmp_path);
    let mut file = File::create(&tmp_path)
        .map_err(|error| io_error("create suffix-tree WAL segment", &tmp_path, error))?;
    file.write_all(&frame)
        .map_err(|error| io_error("write suffix-tree WAL segment", &tmp_path, error))?;
    file.sync_all()
        .map_err(|error| io_error("sync suffix-tree WAL segment", &tmp_path, error))?;
    drop(file);
    fs::rename(&tmp_path, &final_path)
        .map_err(|error| io_error("publish suffix-tree WAL segment", &final_path, error))?;
    File::open(dir)
        .and_then(|dir_file| dir_file.sync_all())
        .map_err(|error| io_error("sync suffix-tree WAL segment directory", dir, error))
}

fn prune_wal_segments(dir: &Path, checkpoint_op_id: u64) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)
        .map_err(|error| io_error("read suffix-tree WAL segment directory", dir, error))?
    {
        let entry = entry.map_err(|error| {
            io_error("read suffix-tree WAL segment directory entry", dir, error)
        })?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some((op_id, _)) = name.split_once('.') else {
            continue;
        };
        let Ok(op_id) = op_id.parse::<u64>() else {
            continue;
        };
        if op_id <= checkpoint_op_id {
            fs::remove_file(&path).map_err(|error| {
                io_error("remove checkpointed suffix-tree WAL segment", &path, error)
            })?;
        }
    }
    File::open(dir)
        .and_then(|dir_file| dir_file.sync_all())
        .map_err(|error| io_error("sync pruned suffix-tree WAL segment directory", dir, error))
}

fn absorb_wal_record<V: DictionaryValue>(
    record: NativeSuffixTreeWalRecord<V>,
    report: &mut SuffixTreeWalReplayReport,
    prepared: &mut HashMap<u64, NativeSuffixTreeWalOp<V>>,
    committed: &mut HashSet<u64>,
    legacy_records: &mut Vec<NativeSuffixTreeWalOp<V>>,
) {
    match record {
        NativeSuffixTreeWalRecord::Insert { text, value } => {
            legacy_records.push(NativeSuffixTreeWalOp::Insert { text, value })
        }
        NativeSuffixTreeWalRecord::Remove { text } => {
            legacy_records.push(NativeSuffixTreeWalOp::Remove { text });
        }
        NativeSuffixTreeWalRecord::SetValue { text, value } => {
            legacy_records.push(NativeSuffixTreeWalOp::SetValue { text, value });
        }
        NativeSuffixTreeWalRecord::Clear => legacy_records.push(NativeSuffixTreeWalOp::Clear),
        NativeSuffixTreeWalRecord::Compact => legacy_records.push(NativeSuffixTreeWalOp::Compact),
        NativeSuffixTreeWalRecord::Prepare { op_id, op } => {
            report.max_op_id = report.max_op_id.max(op_id);
            prepared.insert(op_id, op);
        }
        NativeSuffixTreeWalRecord::Commit { op_id } => {
            report.max_op_id = report.max_op_id.max(op_id);
            committed.insert(op_id);
        }
    }
    report.replayed += 1;
}

fn replay_wal_segments<V: DictionaryValue>(
    dir: &Path,
    report: &mut SuffixTreeWalReplayReport,
    prepared: &mut HashMap<u64, NativeSuffixTreeWalOp<V>>,
    committed: &mut HashSet<u64>,
    legacy_records: &mut Vec<NativeSuffixTreeWalOp<V>>,
) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    let mut files = Vec::new();
    for entry in fs::read_dir(dir)
        .map_err(|error| io_error("read suffix-tree WAL segment directory", dir, error))?
    {
        let entry = entry.map_err(|error| {
            io_error("read suffix-tree WAL segment directory entry", dir, error)
        })?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) == Some("wal") {
            files.push(path);
        }
    }
    files.sort();
    for path in files {
        let mut file = File::open(&path)
            .map_err(|error| io_error("open suffix-tree WAL segment", &path, error))?;
        let mut len_buf = [0u8; 8];
        file.read_exact(&mut len_buf)
            .map_err(|error| io_error("read suffix-tree WAL segment length", &path, error))?;
        let len = u64::from_le_bytes(len_buf);
        if len > MAX_WAL_RECORD_BYTES {
            return Err(PersistentARTrieError::corrupted(format!(
                "native suffix-tree WAL segment is too large: {len} bytes"
            )));
        }
        let mut payload = vec![0u8; len as usize];
        file.read_exact(&mut payload)
            .map_err(|error| io_error("read suffix-tree WAL segment payload", &path, error))?;
        let record: NativeSuffixTreeWalRecord<V> =
            deserialize_bytes("deserialize native suffix-tree WAL segment", &payload)?;
        absorb_wal_record(record, report, prepared, committed, legacy_records);
    }
    Ok(())
}

fn replay_wal<U: PersistentSuffixTreeUnit, V: DictionaryValue>(
    graph: &mut NativeSuffixTreeGraph<U, V>,
    path: &Path,
    checkpoint_op_id: u64,
    folds_legacy_wal: bool,
) -> Result<SuffixTreeWalReplayReport> {
    let mut report = SuffixTreeWalReplayReport::default();
    let mut prepared = HashMap::new();
    let mut committed = HashSet::new();
    let mut legacy_records = Vec::new();

    if path.exists() {
        let mut file = File::open(path)
            .map_err(|error| io_error("open suffix-tree WAL for replay", path, error))?;
        loop {
            let mut len_buf = [0u8; 8];
            match file.read_exact(&mut len_buf) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(error) => {
                    return Err(io_error("read suffix-tree WAL record length", path, error));
                }
            }
            let len = u64::from_le_bytes(len_buf);
            if len > MAX_WAL_RECORD_BYTES {
                return Err(PersistentARTrieError::corrupted(format!(
                    "native suffix-tree WAL record is too large: {len} bytes"
                )));
            }
            let mut payload = vec![0u8; len as usize];
            match file.read_exact(&mut payload) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(error) => {
                    return Err(io_error("read suffix-tree WAL record payload", path, error));
                }
            }
            let record: NativeSuffixTreeWalRecord<V> =
                deserialize_bytes("deserialize native suffix-tree WAL record", &payload)?;
            absorb_wal_record(
                record,
                &mut report,
                &mut prepared,
                &mut committed,
                &mut legacy_records,
            );
        }
    }

    replay_wal_segments(
        &wal_segment_dir_from_wal(path),
        &mut report,
        &mut prepared,
        &mut committed,
        &mut legacy_records,
    )?;

    if !folds_legacy_wal {
        for op in legacy_records {
            NativeSuffixTreeIndex::<U, V>::apply_op(graph, op);
        }
    }
    let mut committed_ops: Vec<_> = committed
        .into_iter()
        .filter(|op_id| *op_id > checkpoint_op_id)
        .filter_map(|op_id| prepared.remove(&op_id).map(|op| (op_id, op)))
        .collect();
    committed_ops.sort_by_key(|(op_id, _)| *op_id);
    for (_, op) in committed_ops {
        NativeSuffixTreeIndex::<U, V>::apply_op(graph, op);
    }
    Ok(report)
}

fn byte_match_start(finish_byte: usize, pattern: &str) -> Option<usize> {
    finish_byte.checked_sub(pattern.len())
}

fn char_match_start(term: &str, finish_byte: usize, pattern: &str) -> Option<usize> {
    let start_byte = finish_byte.checked_sub(pattern.len())?;
    if start_byte > term.len() || !term.is_char_boundary(start_byte) {
        return None;
    }
    Some(term[..start_byte].chars().count())
}

fn byte_locations_in_snapshot<V: DictionaryValue>(
    graph: &NativeSuffixTreeGraph<u8, V>,
    pattern: &str,
) -> Vec<(String, usize)> {
    if pattern.is_empty() {
        return graph
            .active_texts()
            .into_iter()
            .map(|text| (text, 0))
            .collect();
    }

    let texts = graph.source_texts();
    graph
        .match_positions(pattern)
        .into_iter()
        .filter_map(|(source_id, finish_byte)| {
            let text = texts.get(source_id)?;
            let start = byte_match_start(finish_byte, pattern)?;
            Some((text.clone(), start))
        })
        .collect()
}

fn char_locations_in_snapshot<V: DictionaryValue>(
    graph: &NativeSuffixTreeGraph<char, V>,
    pattern: &str,
) -> Vec<(String, usize)> {
    if pattern.is_empty() {
        return graph
            .active_texts()
            .into_iter()
            .map(|text| (text, 0))
            .collect();
    }

    let texts = graph.source_texts();
    graph
        .match_positions(pattern)
        .into_iter()
        .filter_map(|(source_id, finish_byte)| {
            let text = texts.get(source_id)?;
            let start = char_match_start(text, finish_byte, pattern)?;
            Some((text.clone(), start))
        })
        .collect()
}

/// Byte/u8 persistent suffix-tree-compatible substring index.
pub struct PersistentSuffixTree<V: DictionaryValue = (), S: BlockStorage = MmapDiskManager> {
    index: NativeSuffixTreeIndex<u8, V>,
    _storage: PhantomData<S>,
}

/// Unicode/u32 persistent suffix-tree-compatible substring index.
pub struct PersistentSuffixTreeChar<V: DictionaryValue = (), S: BlockStorage = MmapDiskManager> {
    index: NativeSuffixTreeIndex<char, V>,
    _storage: PhantomData<S>,
}

/// Snapshot iterator over stored byte suffix-tree source records.
pub struct PersistentSuffixTreeEntryIterator<V: DictionaryValue = ()> {
    graph: Arc<NativeSuffixTreeGraph<u8, V>>,
    index: usize,
}

/// Snapshot iterator over stored Unicode suffix-tree source records.
pub struct PersistentSuffixTreeCharEntryIterator<V: DictionaryValue = ()> {
    graph: Arc<NativeSuffixTreeGraph<char, V>>,
    index: usize,
}

macro_rules! impl_suffix_tree_record_iterator {
    ($iterator:ident) => {
        impl<V: DictionaryValue> Iterator for $iterator<V> {
            type Item = (String, Option<V>);

            fn next(&mut self) -> Option<Self::Item> {
                let record_index = *self.graph.sorted_active_source_indices.get(self.index)?;
                self.index += 1;
                let record = self
                    .graph
                    .sources
                    .get(record_index)
                    .expect("the revision record index references a source");
                Some((record.text.clone(), record.value.clone()))
            }

            fn size_hint(&self) -> (usize, Option<usize>) {
                let remaining = self
                    .graph
                    .sorted_active_source_indices
                    .len()
                    .saturating_sub(self.index);
                (remaining, Some(remaining))
            }
        }

        impl<V: DictionaryValue> ExactSizeIterator for $iterator<V> {}
        impl<V: DictionaryValue> FusedIterator for $iterator<V> {}
    };
}

impl_suffix_tree_record_iterator!(PersistentSuffixTreeEntryIterator);
impl_suffix_tree_record_iterator!(PersistentSuffixTreeCharEntryIterator);

/// Byte-level persistent suffix tree node handle.
#[derive(Clone)]
pub struct PersistentSuffixTreeNode<V: DictionaryValue = ()> {
    graph: Arc<NativeSuffixTreeGraph<u8, V>>,
    path: Vec<u8>,
}

/// Character-level persistent suffix tree node handle.
#[derive(Clone)]
pub struct PersistentSuffixTreeCharNode<V: DictionaryValue = ()> {
    graph: Arc<NativeSuffixTreeGraph<char, V>>,
    path: Vec<char>,
}

impl<V: DictionaryValue> fmt::Debug for PersistentSuffixTreeNode<V> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersistentSuffixTreeNode")
            .field("path", &String::from_utf8_lossy(&self.path))
            .finish()
    }
}

impl<V: DictionaryValue> fmt::Debug for PersistentSuffixTreeCharNode<V> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let path: String = self.path.iter().collect();
        formatter
            .debug_struct("PersistentSuffixTreeCharNode")
            .field("path", &path)
            .finish()
    }
}

impl<V: DictionaryValue> PersistentSuffixTree<V> {
    /// Create an in-memory persistent suffix tree.
    pub fn new() -> Self {
        Self {
            index: NativeSuffixTreeIndex::new_in_memory(),
            _storage: PhantomData,
        }
    }

    pub fn from_text(text: &str) -> Self {
        let dict = Self::new();
        dict.insert(text);
        dict
    }

    pub fn from_texts<I, T>(texts: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: AsRef<str>,
    {
        let dict = Self::new();
        for text in texts {
            dict.insert(text.as_ref());
        }
        dict
    }
}

impl<V: DictionaryValue> PersistentSuffixTree<V, MmapDiskManager> {
    pub fn create<P: AsRef<Path>>(path: P) -> Result<Self> {
        Ok(Self {
            index: NativeSuffixTreeIndex::create(path.as_ref())?,
            _storage: PhantomData,
        })
    }

    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let (index, _) = NativeSuffixTreeIndex::open(path.as_ref())?;
        Ok(Self {
            index,
            _storage: PhantomData,
        })
    }

    pub fn open_with_recovery<P: AsRef<Path>>(path: P) -> Result<(Self, RecoveryReport)> {
        let start = Instant::now();
        let path = path.as_ref();
        if !path.exists() {
            return Ok((Self::create(path)?, RecoveryReport::created_new()));
        }
        let (index, replayed) = NativeSuffixTreeIndex::open(path)?;
        let dict = Self {
            index,
            _storage: PhantomData,
        };
        let report = if replayed == 0 {
            RecoveryReport::normal()
        } else {
            RecoveryReport::rebuild_from_wal(
                path.to_path_buf(),
                "native suffix-tree WAL replay".to_string(),
                replayed,
                dict.string_count() as u64,
                Vec::new(),
                start.elapsed().as_millis() as u64,
            )
        };
        Ok((dict, report))
    }
}

impl<V: DictionaryValue, S: BlockStorage> PersistentSuffixTree<V, S> {
    pub fn try_insert(&self, text: &str) -> Result<bool> {
        self.index.insert(text, None)
    }

    pub fn try_insert_with_value(&self, text: &str, value: V) -> Result<bool> {
        self.index.insert(text, Some(value))
    }

    pub fn insert(&self, text: &str) -> bool {
        self.try_insert(text).unwrap_or_else(|error| {
            log::warn!("PersistentSuffixTree::insert failed: {error}");
            false
        })
    }

    pub fn insert_with_value(&self, text: &str, value: V) -> bool {
        self.try_insert_with_value(text, value)
            .unwrap_or_else(|error| {
                log::warn!("PersistentSuffixTree::insert_with_value failed: {error}");
                false
            })
    }

    pub fn update_or_insert<F>(&self, term: &str, default_value: V, update_fn: F) -> bool
    where
        F: Fn(&mut V),
    {
        self.index
            .update_or_insert(term, default_value, update_fn)
            .unwrap_or_else(|error| {
                log::warn!("PersistentSuffixTree::update_or_insert failed: {error}");
                false
            })
    }

    pub fn try_remove(&self, text: &str) -> Result<bool> {
        self.index.remove(text)
    }

    pub fn remove(&self, text: &str) -> bool {
        self.try_remove(text).unwrap_or_else(|error| {
            log::warn!("PersistentSuffixTree::remove failed: {error}");
            false
        })
    }

    pub fn try_clear(&self) -> Result<()> {
        self.index.clear()
    }

    pub fn clear(&self) {
        if let Err(error) = self.try_clear() {
            log::warn!("PersistentSuffixTree::clear failed: {error}");
        }
    }

    pub fn try_compact(&self) -> Result<usize> {
        self.index.compact()
    }

    pub fn compact(&self) {
        if let Err(error) = self.try_compact() {
            log::warn!("PersistentSuffixTree::compact failed: {error}");
        }
    }

    pub fn needs_compaction(&self) -> bool {
        self.index.load().needs_compaction
    }

    pub fn string_count(&self) -> usize {
        self.index.load().active_count()
    }

    pub fn source_texts(&self) -> Vec<String> {
        self.index.load().source_texts()
    }

    pub fn active_texts(&self) -> Vec<String> {
        self.index.load().active_texts()
    }

    /// Iterate over stored source records from one immutable revision.
    pub fn iter_entries(&self) -> PersistentSuffixTreeEntryIterator<V> {
        PersistentSuffixTreeEntryIterator {
            graph: self.index.load(),
            index: 0,
        }
    }

    pub fn graph_node_count(&self) -> usize {
        self.index.load().node_count()
    }

    pub fn graph_edge_count(&self) -> usize {
        self.index.load().edge_count()
    }

    pub fn match_positions(&self, pattern: &str) -> Vec<(usize, usize)> {
        self.index.load().match_positions(pattern)
    }

    pub fn contains_substring(&self, pattern: &str) -> bool {
        self.index.load().contains_live_text(pattern)
    }

    pub fn find(&self, pattern: &str) -> Option<PersistentSuffixTreeNode<V>> {
        let graph = self.index.load();
        let path = pattern.as_bytes().to_vec();
        if !graph.contains_live_units(&path) {
            return None;
        }
        Some(PersistentSuffixTreeNode { graph, path })
    }

    pub fn freq(&self, pattern: &str) -> usize {
        if pattern.is_empty() {
            return self.active_texts().iter().map(|text| text.len() + 1).sum();
        }
        self.locations(pattern).len()
    }

    pub fn freq_at(&self, handle: &PersistentSuffixTreeNode<V>) -> usize {
        match std::str::from_utf8(&handle.path) {
            Ok(pattern) => self.freq(pattern),
            Err(_) => 0,
        }
    }

    pub fn locations(&self, pattern: &str) -> Vec<(String, usize)> {
        let graph = self.index.load();
        byte_locations_in_snapshot(&graph, pattern)
    }

    pub fn locations_at(
        &self,
        handle: &PersistentSuffixTreeNode<V>,
        pattern_len: usize,
    ) -> Vec<(String, usize)> {
        if pattern_len > handle.path.len() {
            return Vec::new();
        }
        let start = handle.path.len() - pattern_len;
        match std::str::from_utf8(&handle.path[start..]) {
            Ok(pattern) => self.locations(pattern),
            Err(_) => Vec::new(),
        }
    }

    pub fn checkpoint(&self) -> Result<()> {
        self.index.checkpoint()
    }

    pub fn close(&self) {
        if let Err(error) = self.checkpoint() {
            log::warn!("PersistentSuffixTree::close checkpoint failed: {error}");
        }
    }
}

impl<V: DictionaryValue> PersistentSuffixTreeChar<V> {
    pub fn new() -> Self {
        Self {
            index: NativeSuffixTreeIndex::new_in_memory(),
            _storage: PhantomData,
        }
    }

    pub fn from_text(text: &str) -> Self {
        let dict = Self::new();
        dict.insert(text);
        dict
    }

    pub fn from_texts<I, T>(texts: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: AsRef<str>,
    {
        let dict = Self::new();
        for text in texts {
            dict.insert(text.as_ref());
        }
        dict
    }

    /// Build from Unicode-scalar profile sequences while preserving logical
    /// scalar boundaries before insertion into the persistent suffix index.
    pub fn from_atom_sequences<P, I>(sequences: I) -> Self
    where
        P: crate::AtomProfile<Atom = char>,
        I: IntoIterator<Item = crate::AtomSequence<P>>,
    {
        Self::from_texts(
            sequences
                .into_iter()
                .map(|sequence| sequence.as_atoms().iter().copied().collect::<String>()),
        )
    }

    /// Build a value-bearing tree from Unicode-scalar profile sequences.
    pub fn from_atom_sequences_with_values<P, I>(entries: I) -> Self
    where
        P: crate::AtomProfile<Atom = char>,
        I: IntoIterator<Item = (crate::AtomSequence<P>, V)>,
    {
        let dict = Self::new();
        for (sequence, value) in entries {
            let text: String = sequence.as_atoms().iter().copied().collect();
            dict.insert_with_value(&text, value);
        }
        dict
    }
}

impl<V: DictionaryValue> PersistentSuffixTreeChar<V, MmapDiskManager> {
    pub fn create<P: AsRef<Path>>(path: P) -> Result<Self> {
        Ok(Self {
            index: NativeSuffixTreeIndex::create(path.as_ref())?,
            _storage: PhantomData,
        })
    }

    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let (index, _) = NativeSuffixTreeIndex::open(path.as_ref())?;
        Ok(Self {
            index,
            _storage: PhantomData,
        })
    }

    pub fn open_with_recovery<P: AsRef<Path>>(path: P) -> Result<(Self, RecoveryReport)> {
        let start = Instant::now();
        let path = path.as_ref();
        if !path.exists() {
            return Ok((Self::create(path)?, RecoveryReport::created_new()));
        }
        let (index, replayed) = NativeSuffixTreeIndex::open(path)?;
        let dict = Self {
            index,
            _storage: PhantomData,
        };
        let report = if replayed == 0 {
            RecoveryReport::normal()
        } else {
            RecoveryReport::rebuild_from_wal(
                path.to_path_buf(),
                "native suffix-tree char WAL replay".to_string(),
                replayed,
                dict.string_count() as u64,
                Vec::new(),
                start.elapsed().as_millis() as u64,
            )
        };
        Ok((dict, report))
    }
}

impl<V: DictionaryValue, S: BlockStorage> PersistentSuffixTreeChar<V, S> {
    /// Read a mapped value for a Unicode-scalar profile sequence.
    pub fn get_atom_sequence_value<P>(&self, sequence: &crate::AtomSequence<P>) -> Option<V>
    where
        P: crate::AtomProfile<Atom = char>,
    {
        let text: String = sequence.as_atoms().iter().collect();
        <Self as crate::MappedDictionary>::get_value(self, &text)
    }

    pub fn try_insert(&self, text: &str) -> Result<bool> {
        self.index.insert(text, None)
    }

    pub fn try_insert_with_value(&self, text: &str, value: V) -> Result<bool> {
        self.index.insert(text, Some(value))
    }

    pub fn insert(&self, text: &str) -> bool {
        self.try_insert(text).unwrap_or_else(|error| {
            log::warn!("PersistentSuffixTreeChar::insert failed: {error}");
            false
        })
    }

    pub fn insert_with_value(&self, text: &str, value: V) -> bool {
        self.try_insert_with_value(text, value)
            .unwrap_or_else(|error| {
                log::warn!("PersistentSuffixTreeChar::insert_with_value failed: {error}");
                false
            })
    }

    pub fn update_or_insert<F>(&self, term: &str, default_value: V, update_fn: F) -> bool
    where
        F: Fn(&mut V),
    {
        self.index
            .update_or_insert(term, default_value, update_fn)
            .unwrap_or_else(|error| {
                log::warn!("PersistentSuffixTreeChar::update_or_insert failed: {error}");
                false
            })
    }

    pub fn try_remove(&self, text: &str) -> Result<bool> {
        self.index.remove(text)
    }

    pub fn remove(&self, text: &str) -> bool {
        self.try_remove(text).unwrap_or_else(|error| {
            log::warn!("PersistentSuffixTreeChar::remove failed: {error}");
            false
        })
    }

    pub fn try_clear(&self) -> Result<()> {
        self.index.clear()
    }

    pub fn clear(&self) {
        if let Err(error) = self.try_clear() {
            log::warn!("PersistentSuffixTreeChar::clear failed: {error}");
        }
    }

    pub fn try_compact(&self) -> Result<usize> {
        self.index.compact()
    }

    pub fn compact(&self) {
        if let Err(error) = self.try_compact() {
            log::warn!("PersistentSuffixTreeChar::compact failed: {error}");
        }
    }

    pub fn needs_compaction(&self) -> bool {
        self.index.load().needs_compaction
    }

    pub fn string_count(&self) -> usize {
        self.index.load().active_count()
    }

    pub fn source_texts(&self) -> Vec<String> {
        self.index.load().source_texts()
    }

    pub fn active_texts(&self) -> Vec<String> {
        self.index.load().active_texts()
    }

    /// Iterate over stored source records from one immutable revision.
    pub fn iter_entries(&self) -> PersistentSuffixTreeCharEntryIterator<V> {
        PersistentSuffixTreeCharEntryIterator {
            graph: self.index.load(),
            index: 0,
        }
    }

    pub fn graph_node_count(&self) -> usize {
        self.index.load().node_count()
    }

    pub fn graph_edge_count(&self) -> usize {
        self.index.load().edge_count()
    }

    pub fn match_positions(&self, pattern: &str) -> Vec<(usize, usize)> {
        self.index.load().match_positions(pattern)
    }

    pub fn contains_substring(&self, pattern: &str) -> bool {
        self.index.load().contains_live_text(pattern)
    }

    pub fn find(&self, pattern: &str) -> Option<PersistentSuffixTreeCharNode<V>> {
        let graph = self.index.load();
        let path: Vec<char> = pattern.chars().collect();
        if !graph.contains_live_units(&path) {
            return None;
        }
        Some(PersistentSuffixTreeCharNode { graph, path })
    }

    pub fn freq(&self, pattern: &str) -> usize {
        if pattern.is_empty() {
            return self
                .active_texts()
                .iter()
                .map(|text| text.chars().count() + 1)
                .sum();
        }
        self.locations(pattern).len()
    }

    pub fn freq_at(&self, handle: &PersistentSuffixTreeCharNode<V>) -> usize {
        let pattern: String = handle.path.iter().collect();
        self.freq(&pattern)
    }

    pub fn locations(&self, pattern: &str) -> Vec<(String, usize)> {
        let graph = self.index.load();
        char_locations_in_snapshot(&graph, pattern)
    }

    pub fn locations_at(
        &self,
        handle: &PersistentSuffixTreeCharNode<V>,
        pattern_len: usize,
    ) -> Vec<(String, usize)> {
        if pattern_len > handle.path.len() {
            return Vec::new();
        }
        let pattern: String = handle.path[handle.path.len() - pattern_len..]
            .iter()
            .collect();
        self.locations(&pattern)
    }

    pub fn checkpoint(&self) -> Result<()> {
        self.index.checkpoint()
    }

    pub fn close(&self) {
        if let Err(error) = self.checkpoint() {
            log::warn!("PersistentSuffixTreeChar::close checkpoint failed: {error}");
        }
    }
}

// Intentionally retains the exact owned-node traversal fallback. The compact
// suffix-tree graph stores path-compressed edges, while this public node surface
// exposes every one-unit position, including positions *inside* a compact edge.
// Finality and mapped-value visibility also depend on the complete source-aware
// root-relative path. A dense native node index therefore cannot identify every
// public traversal state. Supplying a direct cursor would require an expanded
// path trie (potentially quadratic for all substring paths), defeating the native
// representation rather than accelerating it. Byte and char variants consequently
// keep `snapshot_root_cursor == None` until an exact compact `(edge, offset, path)`
// capability is available.
impl<V: DictionaryValue> DictionaryNode for PersistentSuffixTreeNode<V> {
    type Unit = u8;
    type SnapshotCursor = crate::SnapshotTraversalCursor;
    type SnapshotGraphValueHandle = crate::SnapshotTraversalCursor;

    fn is_final(&self) -> bool {
        self.graph.contains_live_units(&self.path)
    }

    fn transition(&self, label: Self::Unit) -> Option<Self> {
        let mut path = self.path.clone();
        path.push(label);
        if !self.graph.path_exists_units(&path) {
            return None;
        }
        Some(Self {
            graph: self.graph.clone(),
            path,
        })
    }

    fn edges(&self) -> Box<dyn Iterator<Item = (Self::Unit, Self)> + '_> {
        let graph = self.graph.clone();
        let path = self.path.clone();
        let edges: Vec<_> = graph
            .next_units_after_path(&path)
            .into_iter()
            .map(|label| {
                let mut child_path = path.clone();
                child_path.push(label);
                (
                    label,
                    Self {
                        graph: graph.clone(),
                        path: child_path,
                    },
                )
            })
            .collect();
        Box::new(edges.into_iter())
    }

    #[inline]
    fn for_each_edge<F>(&self, mut visitor: F)
    where
        F: FnMut(Self::Unit, Self),
    {
        for label in self.graph.next_units_after_path(&self.path) {
            let mut child_path = self.path.clone();
            child_path.push(label);
            visitor(
                label,
                Self {
                    graph: self.graph.clone(),
                    path: child_path,
                },
            );
        }
    }

    #[inline]
    fn filter_map_edges<T, P, F>(&self, mut project: P, mut visitor: F)
    where
        P: FnMut(Self::Unit) -> Option<T>,
        F: FnMut(Self::Unit, Self, T),
    {
        for label in self.graph.next_units_after_path(&self.path) {
            let Some(projected) = project(label) else {
                continue;
            };
            let mut child_path = self.path.clone();
            child_path.push(label);
            visitor(
                label,
                Self {
                    graph: self.graph.clone(),
                    path: child_path,
                },
                projected,
            );
        }
    }

    fn edge_count(&self) -> Option<usize> {
        Some(self.graph.next_units_after_path(&self.path).len())
    }
}

impl<V: DictionaryValue> MappedDictionaryNode for PersistentSuffixTreeNode<V> {
    type Value = V;

    fn value(&self) -> Option<Self::Value> {
        self.graph.get_value_units(&self.path)
    }
}

impl<V: DictionaryValue> DictionaryNode for PersistentSuffixTreeCharNode<V> {
    type Unit = char;
    type SnapshotCursor = crate::SnapshotTraversalCursor;
    type SnapshotGraphValueHandle = crate::SnapshotTraversalCursor;

    fn is_final(&self) -> bool {
        self.graph.contains_live_units(&self.path)
    }

    fn transition(&self, label: Self::Unit) -> Option<Self> {
        let mut path = self.path.clone();
        path.push(label);
        if !self.graph.path_exists_units(&path) {
            return None;
        }
        Some(Self {
            graph: self.graph.clone(),
            path,
        })
    }

    fn edges(&self) -> Box<dyn Iterator<Item = (Self::Unit, Self)> + '_> {
        let graph = self.graph.clone();
        let path = self.path.clone();
        let edges: Vec<_> = graph
            .next_units_after_path(&path)
            .into_iter()
            .map(|label| {
                let mut child_path = path.clone();
                child_path.push(label);
                (
                    label,
                    Self {
                        graph: graph.clone(),
                        path: child_path,
                    },
                )
            })
            .collect();
        Box::new(edges.into_iter())
    }

    #[inline]
    fn for_each_edge<F>(&self, mut visitor: F)
    where
        F: FnMut(Self::Unit, Self),
    {
        for label in self.graph.next_units_after_path(&self.path) {
            let mut child_path = self.path.clone();
            child_path.push(label);
            visitor(
                label,
                Self {
                    graph: self.graph.clone(),
                    path: child_path,
                },
            );
        }
    }

    #[inline]
    fn filter_map_edges<T, P, F>(&self, mut project: P, mut visitor: F)
    where
        P: FnMut(Self::Unit) -> Option<T>,
        F: FnMut(Self::Unit, Self, T),
    {
        for label in self.graph.next_units_after_path(&self.path) {
            let Some(projected) = project(label) else {
                continue;
            };
            let mut child_path = self.path.clone();
            child_path.push(label);
            visitor(
                label,
                Self {
                    graph: self.graph.clone(),
                    path: child_path,
                },
                projected,
            );
        }
    }

    fn edge_count(&self) -> Option<usize> {
        Some(self.graph.next_units_after_path(&self.path).len())
    }
}

impl<V: DictionaryValue> MappedDictionaryNode for PersistentSuffixTreeCharNode<V> {
    type Value = V;

    fn value(&self) -> Option<Self::Value> {
        self.graph.get_value_units(&self.path)
    }
}

impl<V: DictionaryValue, S: BlockStorage> Dictionary for PersistentSuffixTree<V, S> {
    type Node = PersistentSuffixTreeNode<V>;

    fn root(&self) -> Self::Node {
        Self::Node {
            graph: self.index.load(),
            path: Vec::new(),
        }
    }

    fn contains(&self, term: &str) -> bool {
        self.contains_substring(term)
    }

    fn len(&self) -> Option<usize> {
        Some(self.string_count())
    }

    fn sync_strategy(&self) -> SyncStrategy {
        SyncStrategy::InternalSync
    }

    fn is_suffix_based(&self) -> bool {
        true
    }
}

impl<V: DictionaryValue, S: BlockStorage> MappedDictionary for PersistentSuffixTree<V, S> {
    type Value = V;

    fn get_value(&self, term: &str) -> Option<Self::Value> {
        self.index.load().get_value_text(term)
    }
}

impl<V: DictionaryValue, S: BlockStorage> MutableDictionary for PersistentSuffixTree<V, S> {
    fn insert(&self, term: &str) -> bool {
        PersistentSuffixTree::insert(self, term)
    }

    fn remove(&self, term: &str) -> bool {
        PersistentSuffixTree::remove(self, term)
    }
}

impl<V: DictionaryValue, S: BlockStorage> MutableMappedDictionary for PersistentSuffixTree<V, S> {
    fn insert_with_value(&self, term: &str, value: Self::Value) -> bool {
        PersistentSuffixTree::insert_with_value(self, term, value)
    }

    fn update_or_insert<F>(&self, term: &str, default_value: Self::Value, update_fn: F) -> bool
    where
        F: Fn(&mut Self::Value),
    {
        PersistentSuffixTree::update_or_insert(self, term, default_value, update_fn)
    }

    fn union_with<F>(&self, other: &Self, merge_fn: F) -> usize
    where
        F: Fn(&Self::Value, &Self::Value) -> Self::Value,
        Self::Value: Clone,
    {
        let mut processed = 0;
        for term in other.active_texts() {
            if term.is_empty() {
                continue;
            }
            if let Some(other_value) = other.get_value(&term) {
                processed += 1;
                let value = if let Some(self_value) = self.get_value(&term) {
                    merge_fn(&self_value, &other_value)
                } else {
                    other_value
                };
                self.insert_with_value(&term, value);
            }
        }
        processed
    }
}

impl<V: DictionaryValue, S: BlockStorage> SubstringDictionary for PersistentSuffixTree<V, S> {
    fn find_exact_substring_in_snapshot(
        snapshot_root: &Self::Node,
        pattern: &str,
    ) -> Vec<SubstringMatch<Self::Node>> {
        debug_assert!(snapshot_root.path.is_empty());
        let graph = Arc::clone(&snapshot_root.graph);
        let path = pattern.as_bytes().to_vec();
        if !graph.contains_live_units(&path) {
            return Vec::new();
        }
        let node = PersistentSuffixTreeNode {
            graph: Arc::clone(&graph),
            path,
        };
        byte_locations_in_snapshot(&graph, pattern)
            .into_iter()
            .map(|(term, position)| {
                SubstringMatch::new(node.clone(), term, position, pattern.len())
            })
            .collect()
    }
}

impl<V: DictionaryValue, S: BlockStorage> Dictionary for PersistentSuffixTreeChar<V, S> {
    type Node = PersistentSuffixTreeCharNode<V>;

    fn root(&self) -> Self::Node {
        Self::Node {
            graph: self.index.load(),
            path: Vec::new(),
        }
    }

    fn contains(&self, term: &str) -> bool {
        self.contains_substring(term)
    }

    fn len(&self) -> Option<usize> {
        Some(self.string_count())
    }

    fn sync_strategy(&self) -> SyncStrategy {
        SyncStrategy::InternalSync
    }

    fn is_suffix_based(&self) -> bool {
        true
    }
}

impl<V: DictionaryValue, S: BlockStorage> MappedDictionary for PersistentSuffixTreeChar<V, S> {
    type Value = V;

    fn get_value(&self, term: &str) -> Option<Self::Value> {
        self.index.load().get_value_text(term)
    }
}

impl<V: DictionaryValue, S: BlockStorage> MutableDictionary for PersistentSuffixTreeChar<V, S> {
    fn insert(&self, term: &str) -> bool {
        PersistentSuffixTreeChar::insert(self, term)
    }

    fn remove(&self, term: &str) -> bool {
        PersistentSuffixTreeChar::remove(self, term)
    }
}

impl<V: DictionaryValue, S: BlockStorage> MutableMappedDictionary
    for PersistentSuffixTreeChar<V, S>
{
    fn insert_with_value(&self, term: &str, value: Self::Value) -> bool {
        PersistentSuffixTreeChar::insert_with_value(self, term, value)
    }

    fn update_or_insert<F>(&self, term: &str, default_value: Self::Value, update_fn: F) -> bool
    where
        F: Fn(&mut Self::Value),
    {
        PersistentSuffixTreeChar::update_or_insert(self, term, default_value, update_fn)
    }

    fn union_with<F>(&self, other: &Self, merge_fn: F) -> usize
    where
        F: Fn(&Self::Value, &Self::Value) -> Self::Value,
        Self::Value: Clone,
    {
        let mut processed = 0;
        for term in other.active_texts() {
            if term.is_empty() {
                continue;
            }
            if let Some(other_value) = other.get_value(&term) {
                processed += 1;
                let value = if let Some(self_value) = self.get_value(&term) {
                    merge_fn(&self_value, &other_value)
                } else {
                    other_value
                };
                self.insert_with_value(&term, value);
            }
        }
        processed
    }
}

impl<V: DictionaryValue, S: BlockStorage> SubstringDictionary for PersistentSuffixTreeChar<V, S> {
    fn find_exact_substring_in_snapshot(
        snapshot_root: &Self::Node,
        pattern: &str,
    ) -> Vec<SubstringMatch<Self::Node>> {
        debug_assert!(snapshot_root.path.is_empty());
        let graph = Arc::clone(&snapshot_root.graph);
        let path: Vec<char> = pattern.chars().collect();
        if !graph.contains_live_units(&path) {
            return Vec::new();
        }
        let node = PersistentSuffixTreeCharNode {
            graph: Arc::clone(&graph),
            path,
        };
        let pattern_len = pattern.chars().count();
        char_locations_in_snapshot(&graph, pattern)
            .into_iter()
            .map(|(term, position)| SubstringMatch::new(node.clone(), term, position, pattern_len))
            .collect()
    }
}

impl<V: DictionaryValue> Default for PersistentSuffixTree<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V: DictionaryValue> Default for PersistentSuffixTreeChar<V> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod snapshot_cursor_fallback_tests {
    use super::*;

    #[test]
    fn compressed_suffix_tree_keeps_exact_path_aware_fallback() {
        let bytes = PersistentSuffixTree::<u64>::new();
        bytes.insert_with_value("banana", 7);
        let byte_root = bytes.root();
        assert!(byte_root.snapshot_root_cursor().is_none());
        assert!(!byte_root.supports_snapshot_cursor_nodes());
        assert!(!byte_root.supports_snapshot_cursor_key_units());
        assert!(!byte_root.supports_snapshot_cursor_values());

        let chars = PersistentSuffixTreeChar::<u64>::new();
        chars.insert_with_value("雪豹", 11);
        let char_root = chars.root();
        assert!(char_root.snapshot_root_cursor().is_none());
        assert!(!char_root.supports_snapshot_cursor_nodes());
        assert!(!char_root.supports_snapshot_cursor_key_units());
        assert!(!char_root.supports_snapshot_cursor_values());

        // The fallback still represents implicit positions inside compressed
        // edges exactly, one public unit at a time.
        let ba = byte_root
            .transition(b'b')
            .and_then(|node| node.transition(b'a'));
        assert!(ba.is_some());
        let snow = char_root
            .transition('雪')
            .and_then(|node| node.transition('豹'));
        assert!(snow.is_some());
    }
}
