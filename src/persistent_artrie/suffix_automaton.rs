//! Native persistent suffix-index dictionaries.
//!
//! These types provide the persistent counterparts to the byte and Unicode
//! suffix automaton APIs without encoding suffixes as namespaced
//! `PersistentARTrie` keys. The durable representation is a native suffix graph
//! snapshot plus a length-prefixed operation WAL. Readers traverse immutable
//! graph snapshots; writers publish copy-on-write graph revisions.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use arc_swap::ArcSwap;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::disk_manager::MmapDiskManager;
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::persistent_artrie::RecoveryReport;
use crate::serialization::bincode_compat;
use crate::value::DictionaryValue;
use crate::{
    CharUnit, Dictionary, DictionaryNode, MappedDictionary, MappedDictionaryNode,
    MutableDictionary, MutableMappedDictionary, SyncStrategy,
};

const BYTE_MAGIC: [u8; 8] = *b"PSUFU8N1";
const CHAR_MAGIC: [u8; 8] = *b"PSUFCHR1";
const SNAPSHOT_VERSION: u32 = 1;
const MAX_WAL_RECORD_BYTES: u64 = 64 * 1024 * 1024;

pub trait PersistentSuffixUnit:
    CharUnit + Serialize + serde::de::DeserializeOwned + fmt::Debug + Send + Sync
{
    const MAGIC: [u8; 8];

    fn suffix_starts(text: &str) -> Vec<usize>;

    fn suffix_units(text: &str, start_byte: usize) -> Vec<Self>;

    fn term_units(text: &str) -> Vec<Self> {
        Self::from_str(text)
    }
}

impl PersistentSuffixUnit for u8 {
    const MAGIC: [u8; 8] = BYTE_MAGIC;

    fn suffix_starts(text: &str) -> Vec<usize> {
        let bytes = text.as_bytes();
        let mut starts: Vec<usize> = (0..bytes.len()).collect();
        starts.sort_by(|left, right| bytes[*left..].cmp(&bytes[*right..]));
        starts
    }

    fn suffix_units(text: &str, start_byte: usize) -> Vec<Self> {
        text.as_bytes()[start_byte..].to_vec()
    }
}

impl PersistentSuffixUnit for char {
    const MAGIC: [u8; 8] = CHAR_MAGIC;

    fn suffix_starts(text: &str) -> Vec<usize> {
        let mut starts: Vec<usize> = text.char_indices().map(|(idx, _)| idx).collect();
        starts.sort_by(|left, right| text[*left..].cmp(&text[*right..]));
        starts
    }

    fn suffix_units(text: &str, start_byte: usize) -> Vec<Self> {
        text[start_byte..].chars().collect()
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct SuffixPosition {
    source_id: u64,
    start_byte: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(bound(
    serialize = "V: serde::Serialize",
    deserialize = "V: serde::de::DeserializeOwned"
))]
pub(crate) struct SourceRecord<V: DictionaryValue> {
    id: u64,
    text: String,
    value: Option<V>,
    active: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(bound(
    serialize = "U: serde::Serialize, V: serde::Serialize",
    deserialize = "U: serde::de::DeserializeOwned, V: serde::de::DeserializeOwned"
))]
struct NativeSuffixNode<U: PersistentSuffixUnit, V: DictionaryValue> {
    edges: Vec<(U, usize)>,
    positions: Vec<SuffixPosition>,
    value: Option<V>,
}

impl<U: PersistentSuffixUnit, V: DictionaryValue> NativeSuffixNode<U, V> {
    fn new() -> Self {
        Self {
            edges: Vec::new(),
            positions: Vec::new(),
            value: None,
        }
    }

    fn find_edge(&self, label: U) -> Option<usize> {
        if self.edges.len() < 16 {
            self.edges
                .iter()
                .find(|(unit, _)| *unit == label)
                .map(|(_, target)| *target)
        } else {
            self.edges
                .binary_search_by_key(&label, |(unit, _)| *unit)
                .ok()
                .map(|index| self.edges[index].1)
        }
    }

    fn add_edge(&mut self, label: U, target: usize) {
        match self.edges.binary_search_by_key(&label, |(unit, _)| *unit) {
            Ok(index) => self.edges[index].1 = target,
            Err(index) => self.edges.insert(index, (label, target)),
        }
    }

    fn is_final(&self) -> bool {
        self.value.is_some() || !self.positions.is_empty()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(bound(
    serialize = "U: serde::Serialize, V: serde::Serialize",
    deserialize = "U: serde::de::DeserializeOwned, V: serde::de::DeserializeOwned"
))]
struct NativeSuffixGraph<U: PersistentSuffixUnit, V: DictionaryValue> {
    nodes: Vec<NativeSuffixNode<U, V>>,
    sources: Vec<SourceRecord<V>>,
    explicit_values: HashMap<Vec<U>, V>,
    needs_compaction: bool,
}

impl<U: PersistentSuffixUnit, V: DictionaryValue> NativeSuffixGraph<U, V> {
    fn new() -> Self {
        Self {
            nodes: vec![NativeSuffixNode::new()],
            sources: Vec::new(),
            explicit_values: HashMap::new(),
            needs_compaction: false,
        }
    }

    fn next_source_id(&self) -> u64 {
        self.sources
            .iter()
            .map(|record| record.id.saturating_add(1))
            .max()
            .unwrap_or(0)
    }

    fn active_source_ids(&self) -> HashSet<u64> {
        self.sources
            .iter()
            .filter(|record| record.active)
            .map(|record| record.id)
            .collect()
    }

    fn active_count(&self) -> usize {
        self.sources.iter().filter(|record| record.active).count()
    }

    fn source_texts(&self) -> Vec<String> {
        self.sources
            .iter()
            .map(|record| record.text.clone())
            .collect()
    }

    fn traverse_units(&self, units: &[U]) -> Option<usize> {
        let mut state = 0usize;
        for &unit in units {
            state = self.nodes.get(state)?.find_edge(unit)?;
        }
        Some(state)
    }

    fn insert_suffix_units(&mut self, units: &[U], position: SuffixPosition, value: Option<V>) {
        let mut state = 0usize;
        for &unit in units {
            let next = match self.nodes[state].find_edge(unit) {
                Some(next) => next,
                None => {
                    let next = self.nodes.len();
                    self.nodes.push(NativeSuffixNode::new());
                    self.nodes[state].add_edge(unit, next);
                    next
                }
            };
            state = next;
        }

        if !self.nodes[state].positions.iter().any(|existing| {
            existing.source_id == position.source_id && existing.start_byte == position.start_byte
        }) {
            self.nodes[state].positions.push(position);
        }
        if let Some(value) = value {
            self.nodes[state].value = Some(value.clone());
            self.explicit_values.insert(units.to_vec(), value);
        }
    }

    fn insert_source_with_id(&mut self, source_id: u64, text: String, value: Option<V>) {
        if text.is_empty() {
            self.insert_suffix_units(
                &[],
                SuffixPosition {
                    source_id,
                    start_byte: 0,
                },
                value.clone(),
            );
        } else {
            for start in U::suffix_starts(&text) {
                let suffix = U::suffix_units(&text, start);
                let suffix_value = if start == 0 { value.clone() } else { None };
                self.insert_suffix_units(
                    &suffix,
                    SuffixPosition {
                        source_id,
                        start_byte: start,
                    },
                    suffix_value,
                );
            }
        }

        self.sources.push(SourceRecord {
            id: source_id,
            text,
            value,
            active: true,
        });
    }

    fn insert_source(&mut self, text: &str, value: Option<V>) -> bool {
        let source_id = self.next_source_id();
        self.insert_source_with_id(source_id, text.to_string(), value);
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
            return true;
        }
        false
    }

    fn clear(&mut self) {
        *self = Self::new();
    }

    fn set_value(&mut self, text: &str, value: V) {
        let units = U::term_units(text);
        let Some(state) = self.traverse_units(&units) else {
            self.insert_source(text, Some(value));
            return;
        };
        self.nodes[state].value = Some(value.clone());
        self.explicit_values.insert(units, value);
    }

    fn update_or_insert<F>(&mut self, text: &str, default_value: V, update_fn: F) -> (bool, V)
    where
        F: FnOnce(&mut V),
    {
        let units = U::term_units(text);
        if let Some(state) = self.traverse_units(&units) {
            if let Some(mut value) = self.nodes[state].value.clone() {
                update_fn(&mut value);
                self.nodes[state].value = Some(value.clone());
                self.explicit_values.insert(units, value.clone());
                return (false, value);
            }

            if self.contains_live_units(&units) {
                self.nodes[state].value = Some(default_value.clone());
                self.explicit_values.insert(units, default_value.clone());
                return (true, default_value);
            }
        }

        self.insert_source(text, Some(default_value.clone()));
        (true, default_value)
    }

    fn get_value(&self, text: &str) -> Option<V> {
        let units = U::term_units(text);
        let state = self.traverse_units(&units)?;
        self.nodes[state].value.clone()
    }

    fn contains_live_units(&self, units: &[U]) -> bool {
        if units.is_empty() {
            return true;
        }
        let Some(state) = self.traverse_units(units) else {
            return false;
        };
        if self.nodes[state].value.is_some() {
            return true;
        }
        let active = self.active_source_ids();
        self.subtree_has_active_position_or_value(state, &active)
    }

    fn contains_live_text(&self, text: &str) -> bool {
        self.contains_live_units(&U::term_units(text))
    }

    fn subtree_has_active_position_or_value(&self, state: usize, active: &HashSet<u64>) -> bool {
        let mut stack = vec![state];
        while let Some(state) = stack.pop() {
            let node = &self.nodes[state];
            if node.value.is_some() {
                return true;
            }
            if node
                .positions
                .iter()
                .any(|position| active.contains(&position.source_id))
            {
                return true;
            }
            for &(_, child) in &node.edges {
                stack.push(child);
            }
        }
        false
    }

    fn collect_subtree_positions(&self, state: usize, out: &mut Vec<SuffixPosition>) {
        let mut stack = vec![state];
        while let Some(state) = stack.pop() {
            let node = &self.nodes[state];
            out.extend(node.positions.iter().cloned());
            for &(_, child) in &node.edges {
                stack.push(child);
            }
        }
    }

    fn match_positions(&self, substring: &str) -> Vec<(usize, usize)> {
        if substring.is_empty() {
            return Vec::new();
        }

        let units = U::term_units(substring);
        let Some(state) = self.traverse_units(&units) else {
            return Vec::new();
        };

        let active = self.active_source_ids();
        let mut positions = Vec::new();
        self.collect_subtree_positions(state, &mut positions);

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

    fn compact(&mut self) -> usize {
        if !self.needs_compaction {
            return 0;
        }

        let old_nodes = self.nodes.len();
        let sources = self.sources.clone();
        let values = self.explicit_values.clone();
        self.nodes = vec![NativeSuffixNode::new()];

        for record in sources.iter().filter(|record| record.active) {
            if record.text.is_empty() {
                self.insert_suffix_units(
                    &[],
                    SuffixPosition {
                        source_id: record.id,
                        start_byte: 0,
                    },
                    record.value.clone(),
                );
            } else {
                for start in U::suffix_starts(&record.text) {
                    let suffix = U::suffix_units(&record.text, start);
                    let suffix_value = if start == 0 {
                        record.value.clone()
                    } else {
                        None
                    };
                    self.insert_suffix_units(
                        &suffix,
                        SuffixPosition {
                            source_id: record.id,
                            start_byte: start,
                        },
                        suffix_value,
                    );
                }
            }
        }

        for (units, value) in values {
            let mut state = 0usize;
            for unit in &units {
                let next = match self.nodes[state].find_edge(*unit) {
                    Some(next) => next,
                    None => {
                        let next = self.nodes.len();
                        self.nodes.push(NativeSuffixNode::new());
                        self.nodes[state].add_edge(*unit, next);
                        next
                    }
                };
                state = next;
            }
            self.nodes[state].value = Some(value.clone());
            self.explicit_values.insert(units, value);
        }

        self.sources = sources;
        self.needs_compaction = false;
        old_nodes.saturating_sub(self.nodes.len())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(bound(
    serialize = "U: serde::Serialize, V: serde::Serialize",
    deserialize = "U: serde::de::DeserializeOwned, V: serde::de::DeserializeOwned"
))]
struct NativeSuffixSnapshot<U: PersistentSuffixUnit, V: DictionaryValue> {
    magic: [u8; 8],
    version: u32,
    graph: NativeSuffixGraph<U, V>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(bound(
    serialize = "V: serde::Serialize",
    deserialize = "V: serde::de::DeserializeOwned"
))]
enum NativeSuffixWalRecord<V: DictionaryValue> {
    Insert { text: String, value: Option<V> },
    Remove { text: String },
    SetValue { text: String, value: V },
    Clear,
    Compact,
}

pub struct NativeSuffixIndex<U: PersistentSuffixUnit, V: DictionaryValue> {
    graph: ArcSwap<NativeSuffixGraph<U, V>>,
    path: Option<PathBuf>,
    write_lock: Arc<Mutex<()>>,
    wal_lock: Arc<Mutex<()>>,
}

fn wal_path(path: &Path) -> PathBuf {
    let mut wal = path.to_path_buf();
    wal.set_extension("suffixwal");
    wal
}

fn tmp_snapshot_path(path: &Path) -> PathBuf {
    let mut tmp = path.to_path_buf();
    tmp.set_extension("suffixtmp");
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

impl<U: PersistentSuffixUnit, V: DictionaryValue> NativeSuffixIndex<U, V> {
    fn new_in_memory() -> Self {
        Self {
            graph: ArcSwap::from_pointee(NativeSuffixGraph::new()),
            path: None,
            write_lock: Arc::new(Mutex::new(())),
            wal_lock: Arc::new(Mutex::new(())),
        }
    }

    fn create(path: &Path) -> Result<Self> {
        let graph = NativeSuffixGraph::new();
        write_snapshot_file::<U, V>(path, graph.clone())?;
        truncate_wal(&wal_path(path))?;
        Ok(Self {
            graph: ArcSwap::from_pointee(graph),
            path: Some(path.to_path_buf()),
            write_lock: Arc::new(Mutex::new(())),
            wal_lock: Arc::new(Mutex::new(())),
        })
    }

    fn open(path: &Path) -> Result<(Self, u64)> {
        let graph = read_snapshot_file::<U, V>(path)?;
        let mut replay_graph = graph.clone();
        let replayed = replay_wal::<U, V>(&mut replay_graph, &wal_path(path))?;
        Ok((
            Self {
                graph: ArcSwap::from_pointee(replay_graph),
                path: Some(path.to_path_buf()),
                write_lock: Arc::new(Mutex::new(())),
                wal_lock: Arc::new(Mutex::new(())),
            },
            replayed,
        ))
    }

    fn load(&self) -> Arc<NativeSuffixGraph<U, V>> {
        self.graph.load_full()
    }

    fn append_record_locked(&self, record: &NativeSuffixWalRecord<V>) -> Result<()> {
        let Some(path) = self.path.as_ref() else {
            return Ok(());
        };
        let _wal_guard = self.wal_lock.lock();
        append_wal_record(&wal_path(path), record)
    }

    fn mutate<R, F>(&self, record: Option<NativeSuffixWalRecord<V>>, mutate: F) -> Result<R>
    where
        F: FnOnce(&mut NativeSuffixGraph<U, V>) -> R,
    {
        let _write_guard = self.write_lock.lock();
        let current = self.graph.load_full();
        let mut next = (*current).clone();
        let result = mutate(&mut next);
        if let Some(record) = record {
            self.append_record_locked(&record)?;
        }
        self.graph.store(Arc::new(next));
        Ok(result)
    }

    fn insert(&self, text: &str, value: Option<V>) -> Result<bool> {
        self.mutate(
            Some(NativeSuffixWalRecord::Insert {
                text: text.to_string(),
                value: value.clone(),
            }),
            |graph| graph.insert_source(text, value),
        )
    }

    fn remove(&self, text: &str) -> Result<bool> {
        self.mutate(
            Some(NativeSuffixWalRecord::Remove {
                text: text.to_string(),
            }),
            |graph| graph.remove_source(text),
        )
    }

    fn clear(&self) -> Result<()> {
        self.mutate(Some(NativeSuffixWalRecord::Clear), |graph| graph.clear())
    }

    fn compact(&self) -> Result<usize> {
        self.mutate(Some(NativeSuffixWalRecord::Compact), |graph| {
            graph.compact()
        })
    }

    fn update_or_insert<F>(&self, text: &str, default_value: V, update_fn: F) -> Result<bool>
    where
        F: FnOnce(&mut V),
    {
        let _write_guard = self.write_lock.lock();
        let current = self.graph.load_full();
        let mut next = (*current).clone();
        let (was_new, value) = next.update_or_insert(text, default_value, update_fn);
        self.append_record_locked(&NativeSuffixWalRecord::SetValue {
            text: text.to_string(),
            value,
        })?;
        self.graph.store(Arc::new(next));
        Ok(was_new)
    }

    fn checkpoint(&self) -> Result<()> {
        let Some(path) = self.path.as_ref() else {
            return Ok(());
        };
        let _write_guard = self.write_lock.lock();
        let _wal_guard = self.wal_lock.lock();
        let graph = (*self.graph.load_full()).clone();
        write_snapshot_file::<U, V>(path, graph)?;
        truncate_wal(&wal_path(path))
    }
}

fn write_snapshot_file<U: PersistentSuffixUnit, V: DictionaryValue>(
    path: &Path,
    graph: NativeSuffixGraph<U, V>,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| io_error("create suffix snapshot parent directory", parent, error))?;
    }

    let snapshot = NativeSuffixSnapshot {
        magic: U::MAGIC,
        version: SNAPSHOT_VERSION,
        graph,
    };
    let bytes = serialize_bytes("serialize native suffix snapshot", &snapshot)?;
    let tmp = tmp_snapshot_path(path);
    {
        let mut file =
            File::create(&tmp).map_err(|error| io_error("create suffix snapshot", &tmp, error))?;
        file.write_all(&bytes)
            .map_err(|error| io_error("write suffix snapshot", &tmp, error))?;
        file.sync_all()
            .map_err(|error| io_error("sync suffix snapshot", &tmp, error))?;
    }
    fs::rename(&tmp, path).map_err(|error| io_error("install suffix snapshot", path, error))
}

fn read_snapshot_file<U: PersistentSuffixUnit, V: DictionaryValue>(
    path: &Path,
) -> Result<NativeSuffixGraph<U, V>> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|error| io_error("open suffix snapshot", path, error))?
        .read_to_end(&mut bytes)
        .map_err(|error| io_error("read suffix snapshot", path, error))?;
    let snapshot: NativeSuffixSnapshot<U, V> =
        deserialize_bytes("deserialize native suffix snapshot", &bytes)?;
    if snapshot.magic != U::MAGIC {
        return Err(PersistentARTrieError::InvalidMagic {
            expected: u64::from_le_bytes(U::MAGIC),
            found: u64::from_le_bytes(snapshot.magic),
        });
    }
    if snapshot.version > SNAPSHOT_VERSION {
        return Err(PersistentARTrieError::UnsupportedVersion {
            max_supported: SNAPSHOT_VERSION,
            found: snapshot.version,
        });
    }
    Ok(snapshot.graph)
}

fn truncate_wal(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| io_error("create suffix WAL parent directory", parent, error))?;
    }
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(|error| io_error("truncate suffix WAL", path, error))?;
    file.sync_all()
        .map_err(|error| io_error("sync suffix WAL", path, error))
}

fn append_wal_record<V: DictionaryValue>(
    path: &Path,
    record: &NativeSuffixWalRecord<V>,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| io_error("create suffix WAL parent directory", parent, error))?;
    }
    let payload = serialize_bytes("serialize native suffix WAL record", record)?;
    let len = payload.len() as u64;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| io_error("open suffix WAL", path, error))?;
    file.write_all(&len.to_le_bytes())
        .map_err(|error| io_error("write suffix WAL record length", path, error))?;
    file.write_all(&payload)
        .map_err(|error| io_error("write suffix WAL record", path, error))?;
    file.sync_all()
        .map_err(|error| io_error("sync suffix WAL", path, error))
}

fn replay_wal<U: PersistentSuffixUnit, V: DictionaryValue>(
    graph: &mut NativeSuffixGraph<U, V>,
    path: &Path,
) -> Result<u64> {
    if !path.exists() {
        return Ok(0);
    }
    let mut file =
        File::open(path).map_err(|error| io_error("open suffix WAL for replay", path, error))?;
    let mut replayed = 0;
    loop {
        let mut len_buf = [0u8; 8];
        match file.read_exact(&mut len_buf) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(error) => return Err(io_error("read suffix WAL record length", path, error)),
        }
        let len = u64::from_le_bytes(len_buf);
        if len > MAX_WAL_RECORD_BYTES {
            return Err(PersistentARTrieError::corrupted(format!(
                "native suffix WAL record is too large: {len} bytes"
            )));
        }
        let mut payload = vec![0u8; len as usize];
        match file.read_exact(&mut payload) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(error) => return Err(io_error("read suffix WAL record payload", path, error)),
        }
        let record: NativeSuffixWalRecord<V> =
            deserialize_bytes("deserialize native suffix WAL record", &payload)?;
        match record {
            NativeSuffixWalRecord::Insert { text, value } => {
                graph.insert_source(&text, value);
            }
            NativeSuffixWalRecord::Remove { text } => {
                graph.remove_source(&text);
            }
            NativeSuffixWalRecord::SetValue { text, value } => {
                graph.set_value(&text, value);
            }
            NativeSuffixWalRecord::Clear => graph.clear(),
            NativeSuffixWalRecord::Compact => {
                graph.compact();
            }
        }
        replayed += 1;
    }
    Ok(replayed)
}

/// Byte/u8 persistent suffix automaton compatible dictionary.
pub struct PersistentSuffixAutomaton<V: DictionaryValue = (), S: BlockStorage = MmapDiskManager> {
    index: NativeSuffixIndex<u8, V>,
    _storage: PhantomData<S>,
}

/// Character/u32 persistent suffix automaton compatible dictionary.
pub struct PersistentSuffixAutomatonChar<V: DictionaryValue = (), S: BlockStorage = MmapDiskManager>
{
    index: NativeSuffixIndex<char, V>,
    _storage: PhantomData<S>,
}

/// Byte-level node handle for native persistent suffix traversal.
#[derive(Clone)]
pub struct PersistentSuffixAutomatonNode<V: DictionaryValue = ()> {
    graph: Arc<NativeSuffixGraph<u8, V>>,
    state_id: Option<usize>,
}

/// Character-level node handle for native persistent suffix traversal.
#[derive(Clone)]
pub struct PersistentSuffixAutomatonCharNode<V: DictionaryValue = ()> {
    graph: Arc<NativeSuffixGraph<char, V>>,
    state_id: Option<usize>,
}

impl<V: DictionaryValue> fmt::Debug for PersistentSuffixAutomatonNode<V> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersistentSuffixAutomatonNode")
            .field("state_id", &self.state_id)
            .finish()
    }
}

impl<V: DictionaryValue> fmt::Debug for PersistentSuffixAutomatonCharNode<V> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersistentSuffixAutomatonCharNode")
            .field("state_id", &self.state_id)
            .finish()
    }
}

impl<V: DictionaryValue> PersistentSuffixAutomaton<V> {
    pub fn new() -> Self {
        Self {
            index: NativeSuffixIndex::new_in_memory(),
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

impl<V: DictionaryValue> PersistentSuffixAutomaton<V, MmapDiskManager> {
    pub fn create<P: AsRef<Path>>(path: P) -> Result<Self> {
        Ok(Self {
            index: NativeSuffixIndex::create(path.as_ref())?,
            _storage: PhantomData,
        })
    }

    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let (index, _) = NativeSuffixIndex::open(path.as_ref())?;
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
        let (index, replayed) = NativeSuffixIndex::open(path)?;
        let dict = Self {
            index,
            _storage: PhantomData,
        };
        let report = if replayed == 0 {
            RecoveryReport::normal()
        } else {
            RecoveryReport::rebuild_from_wal(
                path.to_path_buf(),
                "native suffix WAL replay".to_string(),
                replayed,
                dict.string_count() as u64,
                Vec::new(),
                start.elapsed().as_millis() as u64,
            )
        };
        Ok((dict, report))
    }
}

impl<V: DictionaryValue, S: BlockStorage> PersistentSuffixAutomaton<V, S> {
    pub fn inner(&self) -> &NativeSuffixIndex<u8, V> {
        &self.index
    }

    pub fn try_insert(&self, text: &str) -> Result<bool> {
        self.index.insert(text, None)
    }

    pub fn try_insert_with_value(&self, text: &str, value: V) -> Result<bool> {
        self.index.insert(text, Some(value))
    }

    pub fn insert(&self, text: &str) -> bool {
        self.try_insert(text).unwrap_or_else(|error| {
            log::warn!("PersistentSuffixAutomaton::insert failed: {error}");
            false
        })
    }

    pub fn insert_with_value(&self, text: &str, value: V) -> bool {
        self.try_insert_with_value(text, value)
            .unwrap_or_else(|error| {
                log::warn!("PersistentSuffixAutomaton::insert_with_value failed: {error}");
                false
            })
    }

    pub fn try_remove(&self, text: &str) -> Result<bool> {
        self.index.remove(text)
    }

    pub fn remove(&self, text: &str) -> bool {
        self.try_remove(text).unwrap_or_else(|error| {
            log::warn!("PersistentSuffixAutomaton::remove failed: {error}");
            false
        })
    }

    pub fn try_clear(&self) -> Result<()> {
        self.index.clear()
    }

    pub fn clear(&self) {
        if let Err(error) = self.try_clear() {
            log::warn!("PersistentSuffixAutomaton::clear failed: {error}");
        }
    }

    pub fn try_compact(&self) -> Result<usize> {
        self.index.compact()
    }

    pub fn compact(&self) {
        if let Err(error) = self.try_compact() {
            log::warn!("PersistentSuffixAutomaton::compact failed: {error}");
        }
    }

    pub fn string_count(&self) -> usize {
        self.index.load().active_count()
    }

    pub fn needs_compaction(&self) -> bool {
        self.index.load().needs_compaction
    }

    pub fn match_positions(&self, substring: &str) -> Vec<(usize, usize)> {
        self.index.load().match_positions(substring)
    }

    pub fn update_or_insert<F>(&self, term: &str, default_value: V, update_fn: F) -> bool
    where
        F: FnOnce(&mut V),
    {
        self.index
            .update_or_insert(term, default_value, update_fn)
            .unwrap_or_else(|error| {
                log::warn!("PersistentSuffixAutomaton::update_or_insert failed: {error}");
                false
            })
    }

    pub fn source_texts(&self) -> Vec<String> {
        self.index.load().source_texts()
    }

    pub fn checkpoint(&self) -> Result<()> {
        self.index.checkpoint()
    }

    pub fn close(&self) {
        if let Err(error) = self.checkpoint() {
            log::warn!("PersistentSuffixAutomaton::close checkpoint failed: {error}");
        }
    }

    fn contains_live_suffix_prefix(&self, term: &str) -> bool {
        self.index.load().contains_live_text(term)
    }
}

impl<V: DictionaryValue> PersistentSuffixAutomatonChar<V> {
    pub fn new() -> Self {
        Self {
            index: NativeSuffixIndex::new_in_memory(),
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

impl<V: DictionaryValue> PersistentSuffixAutomatonChar<V, MmapDiskManager> {
    pub fn create<P: AsRef<Path>>(path: P) -> Result<Self> {
        Ok(Self {
            index: NativeSuffixIndex::create(path.as_ref())?,
            _storage: PhantomData,
        })
    }

    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let (index, _) = NativeSuffixIndex::open(path.as_ref())?;
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
        let (index, replayed) = NativeSuffixIndex::open(path)?;
        let dict = Self {
            index,
            _storage: PhantomData,
        };
        let report = if replayed == 0 {
            RecoveryReport::normal()
        } else {
            RecoveryReport::rebuild_from_wal(
                path.to_path_buf(),
                "native suffix char WAL replay".to_string(),
                replayed,
                dict.string_count() as u64,
                Vec::new(),
                start.elapsed().as_millis() as u64,
            )
        };
        Ok((dict, report))
    }
}

impl<V: DictionaryValue, S: BlockStorage> PersistentSuffixAutomatonChar<V, S> {
    pub fn inner(&self) -> &NativeSuffixIndex<char, V> {
        &self.index
    }

    pub fn try_insert(&self, text: &str) -> Result<bool> {
        self.index.insert(text, None)
    }

    pub fn try_insert_with_value(&self, text: &str, value: V) -> Result<bool> {
        self.index.insert(text, Some(value))
    }

    pub fn insert(&self, text: &str) -> bool {
        self.try_insert(text).unwrap_or_else(|error| {
            log::warn!("PersistentSuffixAutomatonChar::insert failed: {error}");
            false
        })
    }

    pub fn insert_with_value(&self, text: &str, value: V) -> bool {
        self.try_insert_with_value(text, value)
            .unwrap_or_else(|error| {
                log::warn!("PersistentSuffixAutomatonChar::insert_with_value failed: {error}");
                false
            })
    }

    pub fn try_remove(&self, text: &str) -> Result<bool> {
        self.index.remove(text)
    }

    pub fn remove(&self, text: &str) -> bool {
        self.try_remove(text).unwrap_or_else(|error| {
            log::warn!("PersistentSuffixAutomatonChar::remove failed: {error}");
            false
        })
    }

    pub fn try_clear(&self) -> Result<()> {
        self.index.clear()
    }

    pub fn clear(&self) {
        if let Err(error) = self.try_clear() {
            log::warn!("PersistentSuffixAutomatonChar::clear failed: {error}");
        }
    }

    pub fn try_compact(&self) -> Result<usize> {
        self.index.compact()
    }

    pub fn compact(&self) {
        if let Err(error) = self.try_compact() {
            log::warn!("PersistentSuffixAutomatonChar::compact failed: {error}");
        }
    }

    pub fn string_count(&self) -> usize {
        self.index.load().active_count()
    }

    pub fn needs_compaction(&self) -> bool {
        self.index.load().needs_compaction
    }

    pub fn match_positions(&self, substring: &str) -> Vec<(usize, usize)> {
        self.index.load().match_positions(substring)
    }

    pub fn update_or_insert<F>(&self, term: &str, default_value: V, update_fn: F) -> bool
    where
        F: FnOnce(&mut V),
    {
        self.index
            .update_or_insert(term, default_value, update_fn)
            .unwrap_or_else(|error| {
                log::warn!("PersistentSuffixAutomatonChar::update_or_insert failed: {error}");
                false
            })
    }

    pub fn source_texts(&self) -> Vec<String> {
        self.index.load().source_texts()
    }

    pub fn checkpoint(&self) -> Result<()> {
        self.index.checkpoint()
    }

    pub fn close(&self) {
        if let Err(error) = self.checkpoint() {
            log::warn!("PersistentSuffixAutomatonChar::close checkpoint failed: {error}");
        }
    }

    fn contains_live_suffix_prefix(&self, term: &str) -> bool {
        self.index.load().contains_live_text(term)
    }
}

impl<V: DictionaryValue> DictionaryNode for PersistentSuffixAutomatonNode<V> {
    type Unit = u8;

    fn is_final(&self) -> bool {
        self.state_id
            .and_then(|state| self.graph.nodes.get(state))
            .is_some_and(NativeSuffixNode::is_final)
    }

    fn transition(&self, label: Self::Unit) -> Option<Self> {
        let state = self.state_id?;
        let target = self.graph.nodes.get(state)?.find_edge(label)?;
        Some(Self {
            graph: self.graph.clone(),
            state_id: Some(target),
        })
    }

    fn edges(&self) -> Box<dyn Iterator<Item = (Self::Unit, Self)> + '_> {
        let Some(state) = self.state_id else {
            return Box::new(std::iter::empty());
        };
        let Some(node) = self.graph.nodes.get(state) else {
            return Box::new(std::iter::empty());
        };
        let graph = self.graph.clone();
        let edges: Vec<_> = node
            .edges
            .iter()
            .map(|(unit, target)| {
                (
                    *unit,
                    Self {
                        graph: graph.clone(),
                        state_id: Some(*target),
                    },
                )
            })
            .collect();
        Box::new(edges.into_iter())
    }

    fn edge_count(&self) -> Option<usize> {
        self.state_id
            .and_then(|state| self.graph.nodes.get(state))
            .map(|node| node.edges.len())
    }
}

impl<V: DictionaryValue> MappedDictionaryNode for PersistentSuffixAutomatonNode<V> {
    type Value = V;

    fn value(&self) -> Option<Self::Value> {
        self.state_id
            .and_then(|state| self.graph.nodes.get(state))
            .and_then(|node| node.value.clone())
    }
}

impl<V: DictionaryValue> DictionaryNode for PersistentSuffixAutomatonCharNode<V> {
    type Unit = char;

    fn is_final(&self) -> bool {
        self.state_id
            .and_then(|state| self.graph.nodes.get(state))
            .is_some_and(NativeSuffixNode::is_final)
    }

    fn transition(&self, label: Self::Unit) -> Option<Self> {
        let state = self.state_id?;
        let target = self.graph.nodes.get(state)?.find_edge(label)?;
        Some(Self {
            graph: self.graph.clone(),
            state_id: Some(target),
        })
    }

    fn edges(&self) -> Box<dyn Iterator<Item = (Self::Unit, Self)> + '_> {
        let Some(state) = self.state_id else {
            return Box::new(std::iter::empty());
        };
        let Some(node) = self.graph.nodes.get(state) else {
            return Box::new(std::iter::empty());
        };
        let graph = self.graph.clone();
        let edges: Vec<_> = node
            .edges
            .iter()
            .map(|(unit, target)| {
                (
                    *unit,
                    Self {
                        graph: graph.clone(),
                        state_id: Some(*target),
                    },
                )
            })
            .collect();
        Box::new(edges.into_iter())
    }

    fn edge_count(&self) -> Option<usize> {
        self.state_id
            .and_then(|state| self.graph.nodes.get(state))
            .map(|node| node.edges.len())
    }
}

impl<V: DictionaryValue> MappedDictionaryNode for PersistentSuffixAutomatonCharNode<V> {
    type Value = V;

    fn value(&self) -> Option<Self::Value> {
        self.state_id
            .and_then(|state| self.graph.nodes.get(state))
            .and_then(|node| node.value.clone())
    }
}

impl<V: DictionaryValue, S: BlockStorage> Dictionary for PersistentSuffixAutomaton<V, S> {
    type Node = PersistentSuffixAutomatonNode<V>;

    fn root(&self) -> Self::Node {
        Self::Node {
            graph: self.index.load(),
            state_id: Some(0),
        }
    }

    fn contains(&self, term: &str) -> bool {
        self.contains_live_suffix_prefix(term)
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

impl<V: DictionaryValue, S: BlockStorage> MappedDictionary for PersistentSuffixAutomaton<V, S> {
    type Value = V;

    fn get_value(&self, term: &str) -> Option<Self::Value> {
        self.index.load().get_value(term)
    }
}

impl<V: DictionaryValue, S: BlockStorage> MutableDictionary for PersistentSuffixAutomaton<V, S> {
    fn insert(&self, term: &str) -> bool {
        PersistentSuffixAutomaton::insert(self, term)
    }

    fn remove(&self, term: &str) -> bool {
        PersistentSuffixAutomaton::remove(self, term)
    }
}

impl<V: DictionaryValue, S: BlockStorage> MutableMappedDictionary
    for PersistentSuffixAutomaton<V, S>
{
    fn insert_with_value(&self, term: &str, value: Self::Value) -> bool {
        PersistentSuffixAutomaton::insert_with_value(self, term, value)
    }

    fn update_or_insert<F>(&self, term: &str, default_value: Self::Value, update_fn: F) -> bool
    where
        F: FnOnce(&mut Self::Value),
    {
        PersistentSuffixAutomaton::update_or_insert(self, term, default_value, update_fn)
    }

    fn union_with<F>(&self, other: &Self, merge_fn: F) -> usize
    where
        F: Fn(&Self::Value, &Self::Value) -> Self::Value,
        Self::Value: Clone,
    {
        let mut processed = 0;
        for term in other.source_texts() {
            if term.is_empty() {
                continue;
            }
            if let Some(other_value) = other.get_value(&term) {
                processed += 1;
                let new_value = if let Some(self_value) = self.get_value(&term) {
                    merge_fn(&self_value, &other_value)
                } else {
                    other_value.clone()
                };
                let replacement = new_value.clone();
                PersistentSuffixAutomaton::update_or_insert(self, &term, new_value, move |value| {
                    *value = replacement
                });
            }
        }
        processed
    }
}

impl<V: DictionaryValue, S: BlockStorage> Dictionary for PersistentSuffixAutomatonChar<V, S> {
    type Node = PersistentSuffixAutomatonCharNode<V>;

    fn root(&self) -> Self::Node {
        Self::Node {
            graph: self.index.load(),
            state_id: Some(0),
        }
    }

    fn contains(&self, term: &str) -> bool {
        self.contains_live_suffix_prefix(term)
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

impl<V: DictionaryValue, S: BlockStorage> MappedDictionary for PersistentSuffixAutomatonChar<V, S> {
    type Value = V;

    fn get_value(&self, term: &str) -> Option<Self::Value> {
        self.index.load().get_value(term)
    }
}

impl<V: DictionaryValue, S: BlockStorage> MutableDictionary
    for PersistentSuffixAutomatonChar<V, S>
{
    fn insert(&self, term: &str) -> bool {
        PersistentSuffixAutomatonChar::insert(self, term)
    }

    fn remove(&self, term: &str) -> bool {
        PersistentSuffixAutomatonChar::remove(self, term)
    }
}

impl<V: DictionaryValue, S: BlockStorage> MutableMappedDictionary
    for PersistentSuffixAutomatonChar<V, S>
{
    fn insert_with_value(&self, term: &str, value: Self::Value) -> bool {
        PersistentSuffixAutomatonChar::insert_with_value(self, term, value)
    }

    fn update_or_insert<F>(&self, term: &str, default_value: Self::Value, update_fn: F) -> bool
    where
        F: FnOnce(&mut Self::Value),
    {
        PersistentSuffixAutomatonChar::update_or_insert(self, term, default_value, update_fn)
    }

    fn union_with<F>(&self, other: &Self, merge_fn: F) -> usize
    where
        F: Fn(&Self::Value, &Self::Value) -> Self::Value,
        Self::Value: Clone,
    {
        let mut processed = 0;
        for term in other.source_texts() {
            if term.is_empty() {
                continue;
            }
            if let Some(other_value) = other.get_value(&term) {
                processed += 1;
                let new_value = if let Some(self_value) = self.get_value(&term) {
                    merge_fn(&self_value, &other_value)
                } else {
                    other_value.clone()
                };
                let replacement = new_value.clone();
                PersistentSuffixAutomatonChar::update_or_insert(
                    self,
                    &term,
                    new_value,
                    move |value| *value = replacement,
                );
            }
        }
        processed
    }
}

impl<V: DictionaryValue> Default for PersistentSuffixAutomaton<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V: DictionaryValue> Default for PersistentSuffixAutomatonChar<V> {
    fn default() -> Self {
        Self::new()
    }
}
