//! Persistent SCDAWG dictionaries backed by native compact suffix graphs.
//!
//! The byte and Unicode variants persist active term records plus a
//! length-prefixed operation WAL, then rebuild and publish immutable
//! [`ScdawgCoreInner`] snapshots. Reads traverse the compact SCDAWG graph
//! without taking the writer lock; writers rebuild a new compact graph and
//! publish it copy-on-write.

use std::collections::HashSet;
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
use crate::scdawg::core::{ScdawgCoreInner, ScdawgNode};
use crate::serialization::bincode_compat;
use crate::substring::{SubstringDictionary, SubstringMatch};
use crate::value::DictionaryValue;
use crate::{
    CharUnit, Dictionary, DictionaryNode, MappedDictionary, MappedDictionaryNode,
    MutableDictionary, MutableMappedDictionary, SyncStrategy,
};

const BYTE_MAGIC: [u8; 8] = *b"PSCDAWG8";
const CHAR_MAGIC: [u8; 8] = *b"PSCDAWGC";
const SNAPSHOT_VERSION: u32 = 1;
const MAX_WAL_RECORD_BYTES: u64 = 64 * 1024 * 1024;

trait PersistentScdawgUnit:
    CharUnit + Serialize + serde::de::DeserializeOwned + fmt::Debug + Send + Sync
{
    const MAGIC: [u8; 8];
}

impl PersistentScdawgUnit for u8 {
    const MAGIC: [u8; 8] = BYTE_MAGIC;
}

impl PersistentScdawgUnit for char {
    const MAGIC: [u8; 8] = CHAR_MAGIC;
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(bound(
    serialize = "V: serde::Serialize",
    deserialize = "V: serde::de::DeserializeOwned"
))]
struct ScdawgTermRecord<V: DictionaryValue> {
    term: String,
    value: Option<V>,
    active: bool,
}

struct NativeScdawgGraph<U: PersistentScdawgUnit, V: DictionaryValue> {
    core: ScdawgCoreInner<U, V>,
    records: Vec<ScdawgTermRecord<V>>,
    needs_compaction: bool,
}

impl<U: PersistentScdawgUnit, V: DictionaryValue> NativeScdawgGraph<U, V> {
    fn new() -> Self {
        Self::from_records(Vec::new(), false)
    }

    fn from_records(records: Vec<ScdawgTermRecord<V>>, needs_compaction: bool) -> Self {
        let active_records: Vec<_> = records.iter().filter(|record| record.active).collect();
        let total_units = active_records
            .iter()
            .map(|record| U::from_str(&record.term).len())
            .sum();
        let mut core = ScdawgCoreInner::with_capacity(active_records.len(), total_units);
        for record in active_records {
            if let Some(value) = record.value.clone() {
                core.insert_with_value(&record.term, value);
            } else {
                core.insert(&record.term);
            }
        }
        core.compute_left_edges();
        Self {
            core,
            records,
            needs_compaction,
        }
    }

    fn active_records(&self) -> impl Iterator<Item = &ScdawgTermRecord<V>> {
        self.records.iter().filter(|record| record.active)
    }

    fn active_terms(&self) -> Vec<String> {
        self.active_records()
            .map(|record| record.term.clone())
            .collect()
    }

    fn term_count(&self) -> usize {
        self.core.term_count()
    }

    fn contains(&self, term: &str) -> bool {
        self.core.contains(term)
    }

    fn contains_substring(&self, pattern: &str) -> bool {
        self.core.contains_substring(pattern)
    }

    fn locations(&self, pattern: &str) -> Vec<(String, usize)> {
        self.core.find_exact_substring(pattern)
    }

    fn freq(&self, pattern: &str) -> usize {
        self.core.frequency(pattern)
    }

    fn get_value(&self, term: &str) -> Option<V> {
        self.core.term_values.get(term).cloned()
    }

    fn compact_records(&self) -> Vec<ScdawgTermRecord<V>> {
        self.active_records().cloned().collect()
    }

    fn insert_record(&mut self, term: &str, value: Option<V>) -> bool {
        if let Some(record) = self
            .records
            .iter_mut()
            .find(|record| record.active && record.term == term)
        {
            if let Some(value) = value {
                record.value = Some(value);
            }
            return false;
        }

        self.records.push(ScdawgTermRecord {
            term: term.to_string(),
            value,
            active: true,
        });
        true
    }

    fn remove_record(&mut self, term: &str) -> bool {
        if let Some(record) = self
            .records
            .iter_mut()
            .find(|record| record.active && record.term == term)
        {
            record.active = false;
            self.needs_compaction = true;
            return true;
        }
        false
    }

    fn update_or_insert<F>(&mut self, term: &str, default_value: V, update_fn: F) -> bool
    where
        F: FnOnce(&mut V),
    {
        if let Some(record) = self
            .records
            .iter_mut()
            .find(|record| record.active && record.term == term)
        {
            let mut value = record.value.clone().unwrap_or(default_value);
            update_fn(&mut value);
            record.value = Some(value);
            return false;
        }

        self.records.push(ScdawgTermRecord {
            term: term.to_string(),
            value: Some(default_value),
            active: true,
        });
        true
    }

    fn clear(&mut self) {
        self.records.clear();
        self.needs_compaction = false;
    }

    fn compact(&mut self) -> usize {
        let before = self.records.len();
        self.records.retain(|record| record.active);
        self.needs_compaction = false;
        before.saturating_sub(self.records.len())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(bound(
    serialize = "V: serde::Serialize",
    deserialize = "V: serde::de::DeserializeOwned"
))]
struct NativeScdawgSnapshot<V: DictionaryValue> {
    magic: [u8; 8],
    version: u32,
    records: Vec<ScdawgTermRecord<V>>,
    needs_compaction: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(bound(
    serialize = "V: serde::Serialize",
    deserialize = "V: serde::de::DeserializeOwned"
))]
enum NativeScdawgWalRecord<V: DictionaryValue> {
    Insert { term: String, value: Option<V> },
    Remove { term: String },
    SetValue { term: String, value: V },
    Clear,
    Compact,
}

struct NativeScdawgIndex<U: PersistentScdawgUnit, V: DictionaryValue> {
    graph: ArcSwap<NativeScdawgGraph<U, V>>,
    path: Option<PathBuf>,
    write_lock: Arc<Mutex<()>>,
    wal_lock: Arc<Mutex<()>>,
}

fn wal_path(path: &Path) -> PathBuf {
    let mut wal = path.to_path_buf();
    wal.set_extension("scdawgwal");
    wal
}

fn tmp_snapshot_path(path: &Path) -> PathBuf {
    let mut tmp = path.to_path_buf();
    tmp.set_extension("scdawgtmp");
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

impl<U: PersistentScdawgUnit, V: DictionaryValue> NativeScdawgIndex<U, V> {
    fn new_in_memory() -> Self {
        Self {
            graph: ArcSwap::from_pointee(NativeScdawgGraph::new()),
            path: None,
            write_lock: Arc::new(Mutex::new(())),
            wal_lock: Arc::new(Mutex::new(())),
        }
    }

    fn create(path: &Path) -> Result<Self> {
        let graph = NativeScdawgGraph::new();
        write_snapshot_file::<U, V>(path, &graph)?;
        truncate_wal(&wal_path(path))?;
        Ok(Self {
            graph: ArcSwap::from_pointee(graph),
            path: Some(path.to_path_buf()),
            write_lock: Arc::new(Mutex::new(())),
            wal_lock: Arc::new(Mutex::new(())),
        })
    }

    fn open(path: &Path) -> Result<(Self, u64)> {
        let mut records = read_snapshot_records::<U, V>(path)?;
        let replayed = replay_wal::<V>(&mut records, &wal_path(path))?;
        let graph = NativeScdawgGraph::from_records(records, false);
        Ok((
            Self {
                graph: ArcSwap::from_pointee(graph),
                path: Some(path.to_path_buf()),
                write_lock: Arc::new(Mutex::new(())),
                wal_lock: Arc::new(Mutex::new(())),
            },
            replayed,
        ))
    }

    fn load(&self) -> Arc<NativeScdawgGraph<U, V>> {
        self.graph.load_full()
    }

    fn append_record_locked(&self, record: &NativeScdawgWalRecord<V>) -> Result<()> {
        let Some(path) = self.path.as_ref() else {
            return Ok(());
        };
        let _wal_guard = self.wal_lock.lock();
        append_wal_record(&wal_path(path), record)
    }

    fn mutate<R, F>(&self, record: Option<NativeScdawgWalRecord<V>>, mutate: F) -> Result<R>
    where
        F: FnOnce(&mut NativeScdawgGraph<U, V>) -> R,
    {
        let _write_guard = self.write_lock.lock();
        let current = self.graph.load_full();
        let mut next: NativeScdawgGraph<U, V> =
            NativeScdawgGraph::from_records(current.records.clone(), current.needs_compaction);
        let result = mutate(&mut next);
        if let Some(record) = record {
            self.append_record_locked(&record)?;
        }
        let rebuilt = NativeScdawgGraph::from_records(next.records, next.needs_compaction);
        self.graph.store(Arc::new(rebuilt));
        Ok(result)
    }

    fn insert(&self, term: &str, value: Option<V>) -> Result<bool> {
        self.mutate(
            Some(NativeScdawgWalRecord::Insert {
                term: term.to_string(),
                value: value.clone(),
            }),
            |graph| graph.insert_record(term, value),
        )
    }

    fn remove(&self, term: &str) -> Result<bool> {
        self.mutate(
            Some(NativeScdawgWalRecord::Remove {
                term: term.to_string(),
            }),
            |graph| graph.remove_record(term),
        )
    }

    fn clear(&self) -> Result<()> {
        self.mutate(Some(NativeScdawgWalRecord::Clear), |graph| graph.clear())
    }

    fn compact(&self) -> Result<usize> {
        self.mutate(Some(NativeScdawgWalRecord::Compact), |graph| {
            graph.compact()
        })
    }

    fn update_or_insert<F>(&self, term: &str, default_value: V, update_fn: F) -> Result<bool>
    where
        F: FnOnce(&mut V),
    {
        let _write_guard = self.write_lock.lock();
        let current = self.graph.load_full();
        let mut next: NativeScdawgGraph<U, V> =
            NativeScdawgGraph::from_records(current.records.clone(), current.needs_compaction);
        let was_new = next.update_or_insert(term, default_value, update_fn);
        let value = next
            .records
            .iter()
            .find(|record| record.active && record.term == term)
            .and_then(|record| record.value.clone())
            .expect("update_or_insert installs a value");
        self.append_record_locked(&NativeScdawgWalRecord::SetValue {
            term: term.to_string(),
            value,
        })?;
        let rebuilt = NativeScdawgGraph::from_records(next.records, next.needs_compaction);
        self.graph.store(Arc::new(rebuilt));
        Ok(was_new)
    }

    fn checkpoint(&self) -> Result<()> {
        let Some(path) = self.path.as_ref() else {
            return Ok(());
        };
        let _write_guard = self.write_lock.lock();
        let _wal_guard = self.wal_lock.lock();
        let graph = self.graph.load_full();
        write_snapshot_file::<U, V>(path, graph.as_ref())?;
        truncate_wal(&wal_path(path))
    }
}

fn write_snapshot_file<U: PersistentScdawgUnit, V: DictionaryValue>(
    path: &Path,
    graph: &NativeScdawgGraph<U, V>,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| io_error("create SCDAWG snapshot parent directory", parent, error))?;
    }

    let snapshot = NativeScdawgSnapshot {
        magic: U::MAGIC,
        version: SNAPSHOT_VERSION,
        records: graph.compact_records(),
        needs_compaction: false,
    };
    let bytes = serialize_bytes("serialize native SCDAWG snapshot", &snapshot)?;
    let tmp = tmp_snapshot_path(path);
    {
        let mut file =
            File::create(&tmp).map_err(|error| io_error("create SCDAWG snapshot", &tmp, error))?;
        file.write_all(&bytes)
            .map_err(|error| io_error("write SCDAWG snapshot", &tmp, error))?;
        file.sync_all()
            .map_err(|error| io_error("sync SCDAWG snapshot", &tmp, error))?;
    }
    fs::rename(&tmp, path).map_err(|error| io_error("install SCDAWG snapshot", path, error))
}

fn read_snapshot_records<U: PersistentScdawgUnit, V: DictionaryValue>(
    path: &Path,
) -> Result<Vec<ScdawgTermRecord<V>>> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|error| io_error("open SCDAWG snapshot", path, error))?
        .read_to_end(&mut bytes)
        .map_err(|error| io_error("read SCDAWG snapshot", path, error))?;
    let snapshot: NativeScdawgSnapshot<V> =
        deserialize_bytes("deserialize native SCDAWG snapshot", &bytes)?;
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
    Ok(snapshot.records)
}

fn truncate_wal(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| io_error("create SCDAWG WAL parent directory", parent, error))?;
    }
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(|error| io_error("truncate SCDAWG WAL", path, error))?;
    file.sync_all()
        .map_err(|error| io_error("sync SCDAWG WAL", path, error))
}

fn append_wal_record<V: DictionaryValue>(
    path: &Path,
    record: &NativeScdawgWalRecord<V>,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| io_error("create SCDAWG WAL parent directory", parent, error))?;
    }
    let payload = serialize_bytes("serialize native SCDAWG WAL record", record)?;
    let len = payload.len() as u64;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| io_error("open SCDAWG WAL", path, error))?;
    file.write_all(&len.to_le_bytes())
        .map_err(|error| io_error("write SCDAWG WAL record length", path, error))?;
    file.write_all(&payload)
        .map_err(|error| io_error("write SCDAWG WAL record", path, error))?;
    file.sync_all()
        .map_err(|error| io_error("sync SCDAWG WAL", path, error))
}

fn replay_wal<V: DictionaryValue>(
    records: &mut Vec<ScdawgTermRecord<V>>,
    path: &Path,
) -> Result<u64> {
    if !path.exists() {
        return Ok(0);
    }
    let mut file =
        File::open(path).map_err(|error| io_error("open SCDAWG WAL for replay", path, error))?;
    let mut replayed = 0;
    loop {
        let mut len_buf = [0u8; 8];
        match file.read_exact(&mut len_buf) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(error) => return Err(io_error("read SCDAWG WAL record length", path, error)),
        }
        let len = u64::from_le_bytes(len_buf);
        if len > MAX_WAL_RECORD_BYTES {
            return Err(PersistentARTrieError::corrupted(format!(
                "native SCDAWG WAL record is too large: {len} bytes"
            )));
        }
        let mut payload = vec![0u8; len as usize];
        match file.read_exact(&mut payload) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(error) => return Err(io_error("read SCDAWG WAL record payload", path, error)),
        }
        let record: NativeScdawgWalRecord<V> =
            deserialize_bytes("deserialize native SCDAWG WAL record", &payload)?;
        apply_wal_record(records, record);
        replayed += 1;
    }
    Ok(replayed)
}

fn apply_wal_record<V: DictionaryValue>(
    records: &mut Vec<ScdawgTermRecord<V>>,
    record: NativeScdawgWalRecord<V>,
) {
    match record {
        NativeScdawgWalRecord::Insert { term, value } => {
            if let Some(existing) = records
                .iter_mut()
                .find(|record| record.active && record.term == term)
            {
                if value.is_some() {
                    existing.value = value;
                }
            } else {
                records.push(ScdawgTermRecord {
                    term,
                    value,
                    active: true,
                });
            }
        }
        NativeScdawgWalRecord::Remove { term } => {
            if let Some(existing) = records
                .iter_mut()
                .find(|record| record.active && record.term == term)
            {
                existing.active = false;
            }
        }
        NativeScdawgWalRecord::SetValue { term, value } => {
            if let Some(existing) = records
                .iter_mut()
                .find(|record| record.active && record.term == term)
            {
                existing.value = Some(value);
            } else {
                records.push(ScdawgTermRecord {
                    term,
                    value: Some(value),
                    active: true,
                });
            }
        }
        NativeScdawgWalRecord::Clear => records.clear(),
        NativeScdawgWalRecord::Compact => records.retain(|record| record.active),
    }
}

/// Byte/u8 persistent SCDAWG dictionary.
pub struct PersistentScdawg<V: DictionaryValue = (), S: BlockStorage = MmapDiskManager> {
    index: NativeScdawgIndex<u8, V>,
    _storage: PhantomData<S>,
}

/// Unicode/u32 persistent SCDAWG dictionary.
pub struct PersistentScdawgChar<V: DictionaryValue = (), S: BlockStorage = MmapDiskManager> {
    index: NativeScdawgIndex<char, V>,
    _storage: PhantomData<S>,
}

/// Byte-level persistent SCDAWG node handle.
#[derive(Clone)]
pub struct PersistentScdawgNode<V: DictionaryValue = ()> {
    graph: Arc<NativeScdawgGraph<u8, V>>,
    node_idx: Option<usize>,
    path: Vec<u8>,
}

/// Character-level persistent SCDAWG node handle.
#[derive(Clone)]
pub struct PersistentScdawgCharNode<V: DictionaryValue = ()> {
    graph: Arc<NativeScdawgGraph<char, V>>,
    node_idx: Option<usize>,
    path: String,
}

impl<V: DictionaryValue> fmt::Debug for PersistentScdawgNode<V> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersistentScdawgNode")
            .field("node_idx", &self.node_idx)
            .field("path", &String::from_utf8_lossy(&self.path))
            .finish()
    }
}

impl<V: DictionaryValue> fmt::Debug for PersistentScdawgCharNode<V> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersistentScdawgCharNode")
            .field("node_idx", &self.node_idx)
            .field("path", &self.path)
            .finish()
    }
}

fn unique_terms<I>(terms: I) -> Vec<String>
where
    I: IntoIterator<Item = String>,
{
    let mut seen = HashSet::new();
    let mut unique = Vec::new();
    for term in terms {
        if seen.insert(term.clone()) {
            unique.push(term);
        }
    }
    unique
}

fn node<U: PersistentScdawgUnit, V: DictionaryValue>(
    graph: &NativeScdawgGraph<U, V>,
    node_idx: Option<usize>,
) -> Option<&ScdawgNode<U, V>> {
    graph.core.nodes.get(node_idx?)
}

impl<V: DictionaryValue> PersistentScdawg<V> {
    pub fn new() -> Self {
        Self {
            index: NativeScdawgIndex::new_in_memory(),
            _storage: PhantomData,
        }
    }

    pub fn from_text(text: &str) -> Self {
        let dict = Self::new();
        dict.insert(text);
        dict
    }

    pub fn from_terms<I, T>(terms: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: AsRef<str>,
    {
        let dict = Self::new();
        for term in terms {
            dict.insert(term.as_ref());
        }
        dict
    }

    pub fn from_terms_with_values<I, T>(entries: I) -> Self
    where
        I: IntoIterator<Item = (T, V)>,
        T: AsRef<str>,
    {
        let dict = Self::new();
        for (term, value) in entries {
            dict.insert_with_value(term.as_ref(), value);
        }
        dict
    }
}

impl<V: DictionaryValue> PersistentScdawg<V, MmapDiskManager> {
    pub fn create<P: AsRef<Path>>(path: P) -> Result<Self> {
        Ok(Self {
            index: NativeScdawgIndex::create(path.as_ref())?,
            _storage: PhantomData,
        })
    }

    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let (index, _) = NativeScdawgIndex::open(path.as_ref())?;
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
        let (index, replayed) = NativeScdawgIndex::open(path)?;
        let dict = Self {
            index,
            _storage: PhantomData,
        };
        let report = if replayed == 0 {
            RecoveryReport::normal()
        } else {
            RecoveryReport::rebuild_from_wal(
                path.to_path_buf(),
                "native SCDAWG WAL replay".to_string(),
                replayed,
                dict.term_count() as u64,
                Vec::new(),
                start.elapsed().as_millis() as u64,
            )
        };
        Ok((dict, report))
    }
}

impl<V: DictionaryValue, S: BlockStorage> PersistentScdawg<V, S> {
    pub fn insert(&self, term: &str) -> bool {
        self.index.insert(term, None).unwrap_or_else(|error| {
            log::warn!("PersistentScdawg::insert failed: {error}");
            false
        })
    }

    pub fn insert_with_value(&self, term: &str, value: V) -> bool {
        self.index
            .insert(term, Some(value))
            .unwrap_or_else(|error| {
                log::warn!("PersistentScdawg::insert_with_value failed: {error}");
                false
            })
    }

    pub fn update_or_insert<F>(&self, term: &str, default_value: V, update_fn: F) -> bool
    where
        F: FnOnce(&mut V),
    {
        self.index
            .update_or_insert(term, default_value, update_fn)
            .unwrap_or_else(|error| {
                log::warn!("PersistentScdawg::update_or_insert failed: {error}");
                false
            })
    }

    pub fn remove(&self, term: &str) -> bool {
        self.index.remove(term).unwrap_or_else(|error| {
            log::warn!("PersistentScdawg::remove failed: {error}");
            false
        })
    }

    pub fn clear(&self) {
        if let Err(error) = self.index.clear() {
            log::warn!("PersistentScdawg::clear failed: {error}");
        }
    }

    pub fn compact(&self) {
        if let Err(error) = self.index.compact() {
            log::warn!("PersistentScdawg::compact failed: {error}");
        }
    }

    pub fn needs_compaction(&self) -> bool {
        self.index.load().needs_compaction
    }

    pub fn term_count(&self) -> usize {
        self.index.load().term_count()
    }

    pub fn string_count(&self) -> usize {
        self.term_count()
    }

    pub fn node_count(&self) -> usize {
        self.index.load().core.nodes.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = String> {
        unique_terms(self.index.load().active_terms().into_iter()).into_iter()
    }

    pub fn source_texts(&self) -> Vec<String> {
        self.iter().collect()
    }

    pub fn contains_substring(&self, pattern: &str) -> bool {
        self.index.load().contains_substring(pattern)
    }

    pub fn find(&self, pattern: &str) -> Option<PersistentScdawgNode<V>> {
        let graph = self.index.load();
        let node_idx = graph.core.find_substring_fast(pattern)?;
        Some(PersistentScdawgNode {
            graph,
            node_idx: Some(node_idx),
            path: pattern.as_bytes().to_vec(),
        })
    }

    pub fn freq(&self, pattern: &str) -> usize {
        self.index.load().freq(pattern)
    }

    pub fn freq_at(&self, handle: &PersistentScdawgNode<V>) -> usize {
        match std::str::from_utf8(&handle.path) {
            Ok(pattern) => self.freq(pattern),
            Err(_) => 0,
        }
    }

    pub fn locations(&self, pattern: &str) -> Vec<(String, usize)> {
        self.index.load().locations(pattern)
    }

    pub fn locations_at(
        &self,
        handle: &PersistentScdawgNode<V>,
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

    pub fn close(&self) {}
}

impl<V: DictionaryValue> PersistentScdawgChar<V> {
    pub fn new() -> Self {
        Self {
            index: NativeScdawgIndex::new_in_memory(),
            _storage: PhantomData,
        }
    }

    pub fn from_text(text: &str) -> Self {
        let dict = Self::new();
        dict.insert(text);
        dict
    }

    pub fn from_terms<I, T>(terms: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: AsRef<str>,
    {
        let dict = Self::new();
        for term in terms {
            dict.insert(term.as_ref());
        }
        dict
    }

    pub fn from_terms_with_values<I, T>(entries: I) -> Self
    where
        I: IntoIterator<Item = (T, V)>,
        T: AsRef<str>,
    {
        let dict = Self::new();
        for (term, value) in entries {
            dict.insert_with_value(term.as_ref(), value);
        }
        dict
    }
}

impl<V: DictionaryValue> PersistentScdawgChar<V, MmapDiskManager> {
    pub fn create<P: AsRef<Path>>(path: P) -> Result<Self> {
        Ok(Self {
            index: NativeScdawgIndex::create(path.as_ref())?,
            _storage: PhantomData,
        })
    }

    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let (index, _) = NativeScdawgIndex::open(path.as_ref())?;
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
        let (index, replayed) = NativeScdawgIndex::open(path)?;
        let dict = Self {
            index,
            _storage: PhantomData,
        };
        let report = if replayed == 0 {
            RecoveryReport::normal()
        } else {
            RecoveryReport::rebuild_from_wal(
                path.to_path_buf(),
                "native SCDAWG char WAL replay".to_string(),
                replayed,
                dict.term_count() as u64,
                Vec::new(),
                start.elapsed().as_millis() as u64,
            )
        };
        Ok((dict, report))
    }
}

impl<V: DictionaryValue, S: BlockStorage> PersistentScdawgChar<V, S> {
    pub fn insert(&self, term: &str) -> bool {
        self.index.insert(term, None).unwrap_or_else(|error| {
            log::warn!("PersistentScdawgChar::insert failed: {error}");
            false
        })
    }

    pub fn insert_with_value(&self, term: &str, value: V) -> bool {
        self.index
            .insert(term, Some(value))
            .unwrap_or_else(|error| {
                log::warn!("PersistentScdawgChar::insert_with_value failed: {error}");
                false
            })
    }

    pub fn update_or_insert<F>(&self, term: &str, default_value: V, update_fn: F) -> bool
    where
        F: FnOnce(&mut V),
    {
        self.index
            .update_or_insert(term, default_value, update_fn)
            .unwrap_or_else(|error| {
                log::warn!("PersistentScdawgChar::update_or_insert failed: {error}");
                false
            })
    }

    pub fn remove(&self, term: &str) -> bool {
        self.index.remove(term).unwrap_or_else(|error| {
            log::warn!("PersistentScdawgChar::remove failed: {error}");
            false
        })
    }

    pub fn clear(&self) {
        if let Err(error) = self.index.clear() {
            log::warn!("PersistentScdawgChar::clear failed: {error}");
        }
    }

    pub fn compact(&self) {
        if let Err(error) = self.index.compact() {
            log::warn!("PersistentScdawgChar::compact failed: {error}");
        }
    }

    pub fn needs_compaction(&self) -> bool {
        self.index.load().needs_compaction
    }

    pub fn term_count(&self) -> usize {
        self.index.load().term_count()
    }

    pub fn string_count(&self) -> usize {
        self.term_count()
    }

    pub fn node_count(&self) -> usize {
        self.index.load().core.nodes.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = String> {
        unique_terms(self.index.load().active_terms().into_iter()).into_iter()
    }

    pub fn source_texts(&self) -> Vec<String> {
        self.iter().collect()
    }

    pub fn contains_substring(&self, pattern: &str) -> bool {
        self.index.load().contains_substring(pattern)
    }

    pub fn find(&self, pattern: &str) -> Option<PersistentScdawgCharNode<V>> {
        let graph = self.index.load();
        let node_idx = graph.core.find_substring_fast(pattern)?;
        Some(PersistentScdawgCharNode {
            graph,
            node_idx: Some(node_idx),
            path: pattern.to_string(),
        })
    }

    pub fn freq(&self, pattern: &str) -> usize {
        self.index.load().freq(pattern)
    }

    pub fn freq_at(&self, handle: &PersistentScdawgCharNode<V>) -> usize {
        self.freq(&handle.path)
    }

    pub fn locations(&self, pattern: &str) -> Vec<(String, usize)> {
        self.index.load().locations(pattern)
    }

    pub fn locations_at(
        &self,
        handle: &PersistentScdawgCharNode<V>,
        pattern_len: usize,
    ) -> Vec<(String, usize)> {
        let chars: Vec<char> = handle.path.chars().collect();
        if pattern_len > chars.len() {
            return Vec::new();
        }
        let pattern: String = chars[chars.len() - pattern_len..].iter().collect();
        self.locations(&pattern)
    }

    pub fn checkpoint(&self) -> Result<()> {
        self.index.checkpoint()
    }

    pub fn close(&self) {}
}

impl<V: DictionaryValue> DictionaryNode for PersistentScdawgNode<V> {
    type Unit = u8;

    fn is_final(&self) -> bool {
        node(&self.graph, self.node_idx)
            .map(|node| node.is_final)
            .unwrap_or(false)
    }

    fn transition(&self, label: Self::Unit) -> Option<Self> {
        let next = node(&self.graph, self.node_idx)?.get_edge(label)?;
        let mut path = self.path.clone();
        path.push(label);
        Some(Self {
            graph: Arc::clone(&self.graph),
            node_idx: Some(next),
            path,
        })
    }

    fn edges(&self) -> Box<dyn Iterator<Item = (Self::Unit, Self)> + '_> {
        let Some(node) = node(&self.graph, self.node_idx) else {
            return Box::new(std::iter::empty());
        };
        let path = self.path.clone();
        let graph = Arc::clone(&self.graph);
        let edges: Vec<_> = node
            .forward_edges
            .iter()
            .map(|&(label, next)| {
                let mut child_path = path.clone();
                child_path.push(label);
                (
                    label,
                    Self {
                        graph: Arc::clone(&graph),
                        node_idx: Some(next),
                        path: child_path,
                    },
                )
            })
            .collect();
        Box::new(edges.into_iter())
    }

    fn edge_count(&self) -> Option<usize> {
        Some(node(&self.graph, self.node_idx)?.forward_edges.len())
    }
}

impl<V: DictionaryValue> MappedDictionaryNode for PersistentScdawgNode<V> {
    type Value = V;

    fn value(&self) -> Option<Self::Value> {
        node(&self.graph, self.node_idx)?.value.clone()
    }
}

impl<V: DictionaryValue> DictionaryNode for PersistentScdawgCharNode<V> {
    type Unit = char;

    fn is_final(&self) -> bool {
        node(&self.graph, self.node_idx)
            .map(|node| node.is_final)
            .unwrap_or(false)
    }

    fn transition(&self, label: Self::Unit) -> Option<Self> {
        let next = node(&self.graph, self.node_idx)?.get_edge(label)?;
        let mut path = self.path.clone();
        path.push(label);
        Some(Self {
            graph: Arc::clone(&self.graph),
            node_idx: Some(next),
            path,
        })
    }

    fn edges(&self) -> Box<dyn Iterator<Item = (Self::Unit, Self)> + '_> {
        let Some(node) = node(&self.graph, self.node_idx) else {
            return Box::new(std::iter::empty());
        };
        let path = self.path.clone();
        let graph = Arc::clone(&self.graph);
        let edges: Vec<_> = node
            .forward_edges
            .iter()
            .map(|&(label, next)| {
                let mut child_path = path.clone();
                child_path.push(label);
                (
                    label,
                    Self {
                        graph: Arc::clone(&graph),
                        node_idx: Some(next),
                        path: child_path,
                    },
                )
            })
            .collect();
        Box::new(edges.into_iter())
    }

    fn edge_count(&self) -> Option<usize> {
        Some(node(&self.graph, self.node_idx)?.forward_edges.len())
    }
}

impl<V: DictionaryValue> MappedDictionaryNode for PersistentScdawgCharNode<V> {
    type Value = V;

    fn value(&self) -> Option<Self::Value> {
        node(&self.graph, self.node_idx)?.value.clone()
    }
}

impl<V: DictionaryValue, S: BlockStorage> Dictionary for PersistentScdawg<V, S> {
    type Node = PersistentScdawgNode<V>;

    fn root(&self) -> Self::Node {
        Self::Node {
            graph: self.index.load(),
            node_idx: Some(0),
            path: Vec::new(),
        }
    }

    fn contains(&self, term: &str) -> bool {
        self.index.load().contains(term)
    }

    fn len(&self) -> Option<usize> {
        Some(self.term_count())
    }

    fn sync_strategy(&self) -> SyncStrategy {
        SyncStrategy::InternalSync
    }

    fn is_suffix_based(&self) -> bool {
        true
    }
}

impl<V: DictionaryValue, S: BlockStorage> MappedDictionary for PersistentScdawg<V, S> {
    type Value = V;

    fn get_value(&self, term: &str) -> Option<Self::Value> {
        let graph = self.index.load();
        if graph.contains(term) {
            graph.get_value(term)
        } else {
            None
        }
    }
}

impl<V: DictionaryValue, S: BlockStorage> MutableDictionary for PersistentScdawg<V, S> {
    fn insert(&self, term: &str) -> bool {
        PersistentScdawg::insert(self, term)
    }

    fn remove(&self, term: &str) -> bool {
        PersistentScdawg::remove(self, term)
    }
}

impl<V: DictionaryValue, S: BlockStorage> MutableMappedDictionary for PersistentScdawg<V, S> {
    fn insert_with_value(&self, term: &str, value: Self::Value) -> bool {
        PersistentScdawg::insert_with_value(self, term, value)
    }

    fn update_or_insert<F>(&self, term: &str, default_value: Self::Value, update_fn: F) -> bool
    where
        F: FnOnce(&mut Self::Value),
    {
        PersistentScdawg::update_or_insert(self, term, default_value, update_fn)
    }

    fn union_with<F>(&self, other: &Self, merge_fn: F) -> usize
    where
        F: Fn(&Self::Value, &Self::Value) -> Self::Value,
        Self::Value: Clone,
    {
        let mut processed = 0;
        for term in other.iter() {
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

impl<V: DictionaryValue, S: BlockStorage> SubstringDictionary for PersistentScdawg<V, S> {
    fn find_exact_substring(&self, pattern: &str) -> Vec<SubstringMatch<Self::Node>> {
        let Some(node) = self.find(pattern) else {
            return Vec::new();
        };
        self.locations(pattern)
            .into_iter()
            .map(|(term, position)| {
                SubstringMatch::new(node.clone(), term, position, pattern.len())
            })
            .collect()
    }
}

impl<V: DictionaryValue, S: BlockStorage> Dictionary for PersistentScdawgChar<V, S> {
    type Node = PersistentScdawgCharNode<V>;

    fn root(&self) -> Self::Node {
        Self::Node {
            graph: self.index.load(),
            node_idx: Some(0),
            path: String::new(),
        }
    }

    fn contains(&self, term: &str) -> bool {
        self.index.load().contains(term)
    }

    fn len(&self) -> Option<usize> {
        Some(self.term_count())
    }

    fn sync_strategy(&self) -> SyncStrategy {
        SyncStrategy::InternalSync
    }

    fn is_suffix_based(&self) -> bool {
        true
    }
}

impl<V: DictionaryValue, S: BlockStorage> MappedDictionary for PersistentScdawgChar<V, S> {
    type Value = V;

    fn get_value(&self, term: &str) -> Option<Self::Value> {
        let graph = self.index.load();
        if graph.contains(term) {
            graph.get_value(term)
        } else {
            None
        }
    }
}

impl<V: DictionaryValue, S: BlockStorage> MutableDictionary for PersistentScdawgChar<V, S> {
    fn insert(&self, term: &str) -> bool {
        PersistentScdawgChar::insert(self, term)
    }

    fn remove(&self, term: &str) -> bool {
        PersistentScdawgChar::remove(self, term)
    }
}

impl<V: DictionaryValue, S: BlockStorage> MutableMappedDictionary for PersistentScdawgChar<V, S> {
    fn insert_with_value(&self, term: &str, value: Self::Value) -> bool {
        PersistentScdawgChar::insert_with_value(self, term, value)
    }

    fn update_or_insert<F>(&self, term: &str, default_value: Self::Value, update_fn: F) -> bool
    where
        F: FnOnce(&mut Self::Value),
    {
        PersistentScdawgChar::update_or_insert(self, term, default_value, update_fn)
    }

    fn union_with<F>(&self, other: &Self, merge_fn: F) -> usize
    where
        F: Fn(&Self::Value, &Self::Value) -> Self::Value,
        Self::Value: Clone,
    {
        let mut processed = 0;
        for term in other.iter() {
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

impl<V: DictionaryValue, S: BlockStorage> SubstringDictionary for PersistentScdawgChar<V, S> {
    fn find_exact_substring(&self, pattern: &str) -> Vec<SubstringMatch<Self::Node>> {
        let Some(node) = self.find(pattern) else {
            return Vec::new();
        };
        let pattern_len = pattern.chars().count();
        self.locations(pattern)
            .into_iter()
            .map(|(term, position)| SubstringMatch::new(node.clone(), term, position, pattern_len))
            .collect()
    }
}

impl<V: DictionaryValue> Default for PersistentScdawg<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V: DictionaryValue> Default for PersistentScdawgChar<V> {
    fn default() -> Self {
        Self::new()
    }
}
