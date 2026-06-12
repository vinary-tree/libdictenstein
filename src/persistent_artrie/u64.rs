//! Sequence-keyed persistent trie for `u64` units.
//!
//! `PersistentARTrieU64` uses a native `u64` edge representation instead of
//! encoding every public unit as eight byte-level trie transitions. The native
//! storage format is a bincode snapshot of `u64` paths plus a length-prefixed
//! native operation log. The byte-encoded facade is retained as
//! [`EncodedPersistentARTrieU64`] for benchmarks and migration comparisons.

use std::collections::HashMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use arc_swap::{ArcSwap, ArcSwapOption};
use parking_lot::Mutex;
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::disk_manager::MmapDiskManager;
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::persistent_artrie::{PersistentARTrie, RecoveryReport};
use crate::serialization::bincode_compat;
use crate::value::DictionaryValue;
use crate::{
    CharUnit, Dictionary, DictionaryNode, MappedDictionary, MappedDictionaryNode,
    MutableDictionary, MutableMappedDictionary, SyncStrategy,
};

const SNAPSHOT_MAGIC: [u8; 8] = *b"PARTU64N";
const SNAPSHOT_VERSION: u32 = 1;
const MAX_WAL_RECORD_BYTES: u64 = 64 * 1024 * 1024;
const INLINE_EDGE_LIMIT: usize = 16;
const SORTED_EDGE_LIMIT: usize = 128;

/// Persistent trie keyed by native `u64` sequences.
pub struct PersistentARTrieU64<V: DictionaryValue = (), S: BlockStorage = MmapDiskManager> {
    inner: Arc<NativeU64Trie<V>>,
    path: Option<PathBuf>,
    checkpoint_lock: Arc<Mutex<()>>,
    wal_lock: Arc<Mutex<()>>,
    _storage: PhantomData<S>,
}

/// Node handle for [`PersistentARTrieU64`].
#[derive(Clone)]
pub struct PersistentARTrieU64Node<V: DictionaryValue = ()> {
    inner: Arc<NativeU64Node<V>>,
    path: Vec<u64>,
}

pub struct NativeU64Trie<V: DictionaryValue> {
    root: Arc<NativeU64Node<V>>,
    term_count: AtomicUsize,
}

struct NativeU64Node<V: DictionaryValue> {
    edges: ArcSwap<NativeU64EdgeStore<V>>,
    is_final: AtomicBool,
    value: ArcSwapOption<V>,
}

enum NativeU64EdgeStore<V: DictionaryValue> {
    Inline(SmallVec<[(u64, Arc<NativeU64Node<V>>); INLINE_EDGE_LIMIT]>),
    Sorted(Vec<(u64, Arc<NativeU64Node<V>>)>),
    Hash(FxHashMap<u64, Arc<NativeU64Node<V>>>),
}

impl<V: DictionaryValue> NativeU64EdgeStore<V> {
    fn new() -> Self {
        Self::Inline(SmallVec::new())
    }

    fn len(&self) -> usize {
        match self {
            Self::Inline(edges) => edges.len(),
            Self::Sorted(edges) => edges.len(),
            Self::Hash(edges) => edges.len(),
        }
    }

    fn find(&self, label: u64) -> Option<&Arc<NativeU64Node<V>>> {
        match self {
            Self::Inline(edges) => edges
                .binary_search_by_key(&label, |(edge, _)| *edge)
                .ok()
                .map(|index| &edges[index].1),
            Self::Sorted(edges) => edges
                .binary_search_by_key(&label, |(edge, _)| *edge)
                .ok()
                .map(|index| &edges[index].1),
            Self::Hash(edges) => edges.get(&label),
        }
    }

    fn with_edge(&self, label: u64, child: Arc<NativeU64Node<V>>) -> Self {
        match self {
            Self::Inline(edges) => {
                let mut next = edges.clone();
                match next.binary_search_by_key(&label, |(edge, _)| *edge) {
                    Ok(index) => next[index].1 = child,
                    Err(index) => next.insert(index, (label, child)),
                }

                if next.len() <= INLINE_EDGE_LIMIT {
                    Self::Inline(next)
                } else {
                    Self::Sorted(next.into_iter().collect())
                }
            }
            Self::Sorted(edges) => {
                let mut next = edges.clone();
                match next.binary_search_by_key(&label, |(edge, _)| *edge) {
                    Ok(index) => next[index].1 = child,
                    Err(index) => next.insert(index, (label, child)),
                }

                if next.len() <= SORTED_EDGE_LIMIT {
                    Self::Sorted(next)
                } else {
                    let mut hash = FxHashMap::default();
                    hash.reserve(next.len());
                    for (edge, child) in next {
                        hash.insert(edge, child);
                    }
                    Self::Hash(hash)
                }
            }
            Self::Hash(edges) => {
                let mut next = edges.clone();
                next.insert(label, child);
                Self::Hash(next)
            }
        }
    }

    fn edges_vec(&self) -> Vec<(u64, Arc<NativeU64Node<V>>)> {
        match self {
            Self::Inline(edges) => edges
                .iter()
                .map(|(label, child)| (*label, child.clone()))
                .collect(),
            Self::Sorted(edges) => edges
                .iter()
                .map(|(label, child)| (*label, child.clone()))
                .collect(),
            Self::Hash(edges) => {
                let mut out: Vec<_> = edges
                    .iter()
                    .map(|(label, child)| (*label, child.clone()))
                    .collect();
                out.sort_by_key(|(label, _)| *label);
                out
            }
        }
    }
}

impl<V: DictionaryValue> NativeU64Node<V> {
    fn new(is_final: bool) -> Self {
        Self {
            edges: ArcSwap::from_pointee(NativeU64EdgeStore::new()),
            is_final: AtomicBool::new(is_final),
            value: ArcSwapOption::empty(),
        }
    }

    fn new_with_value(is_final: bool, value: V) -> Self {
        Self {
            edges: ArcSwap::from_pointee(NativeU64EdgeStore::new()),
            is_final: AtomicBool::new(is_final),
            value: ArcSwapOption::from_pointee(Some(value)),
        }
    }
}

impl<V: DictionaryValue> NativeU64Trie<V> {
    fn new() -> Self {
        Self {
            root: Arc::new(NativeU64Node::new(false)),
            term_count: AtomicUsize::new(0),
        }
    }

    fn term_count(&self) -> usize {
        self.term_count.load(Ordering::Relaxed)
    }

    fn root(&self) -> Arc<NativeU64Node<V>> {
        self.root.clone()
    }

    fn insert_sequence(&self, sequence: &[u64]) -> bool {
        if sequence.is_empty() {
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

        let mut current = self.root.clone();
        for (index, &label) in sequence.iter().enumerate() {
            let is_last = index == sequence.len() - 1;
            loop {
                let edges = current.edges.load();
                if let Some(child) = edges.find(label) {
                    if is_last {
                        if child
                            .is_final
                            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
                            .is_ok()
                        {
                            self.term_count.fetch_add(1, Ordering::Relaxed);
                            return true;
                        }
                        return false;
                    }
                    current = child.clone();
                    break;
                }

                let new_node = Arc::new(NativeU64Node::new(is_last));
                let new_edges = Arc::new(edges.with_edge(label, new_node.clone()));
                let previous = current.edges.compare_and_swap(&edges, new_edges);
                if Arc::ptr_eq(&previous, &edges) {
                    if is_last {
                        self.term_count.fetch_add(1, Ordering::Relaxed);
                        return true;
                    }
                    current = new_node;
                    break;
                }
            }
        }

        true
    }

    fn insert_sequence_with_value(&self, sequence: &[u64], value: V) -> bool {
        if sequence.is_empty() {
            if self
                .root
                .is_final
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                self.root.value.store(Some(Arc::new(value)));
                self.term_count.fetch_add(1, Ordering::Relaxed);
                return true;
            }
            self.root.value.store(Some(Arc::new(value)));
            return false;
        }

        let mut current = self.root.clone();
        for (index, &label) in sequence.iter().enumerate() {
            let is_last = index == sequence.len() - 1;
            loop {
                let edges = current.edges.load();
                if let Some(child) = edges.find(label) {
                    if is_last {
                        if child
                            .is_final
                            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
                            .is_ok()
                        {
                            child.value.store(Some(Arc::new(value)));
                            self.term_count.fetch_add(1, Ordering::Relaxed);
                            return true;
                        }
                        child.value.store(Some(Arc::new(value)));
                        return false;
                    }
                    current = child.clone();
                    break;
                }

                let new_node = Arc::new(if is_last {
                    NativeU64Node::new_with_value(true, value.clone())
                } else {
                    NativeU64Node::new(false)
                });
                let new_edges = Arc::new(edges.with_edge(label, new_node.clone()));
                let previous = current.edges.compare_and_swap(&edges, new_edges);
                if Arc::ptr_eq(&previous, &edges) {
                    if is_last {
                        self.term_count.fetch_add(1, Ordering::Relaxed);
                        return true;
                    }
                    current = new_node;
                    break;
                }
            }
        }

        true
    }

    fn find_node(&self, sequence: &[u64]) -> Option<Arc<NativeU64Node<V>>> {
        let mut current = self.root.clone();
        for &label in sequence {
            let edges = current.edges.load();
            current = edges.find(label)?.clone();
        }
        Some(current)
    }

    fn contains_sequence(&self, sequence: &[u64]) -> bool {
        self.find_node(sequence)
            .is_some_and(|node| node.is_final.load(Ordering::Acquire))
    }

    fn get_sequence_value(&self, sequence: &[u64]) -> Option<V> {
        let node = self.find_node(sequence)?;
        if !node.is_final.load(Ordering::Acquire) {
            return None;
        }
        let value = node.value.load();
        value.as_ref().map(|value| (**value).clone())
    }

    fn remove_sequence(&self, sequence: &[u64]) -> bool {
        let Some(node) = self.find_node(sequence) else {
            return false;
        };
        if node
            .is_final
            .compare_exchange(true, false, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
        {
            node.value.store(None);
            self.term_count.fetch_sub(1, Ordering::Relaxed);
            return true;
        }
        false
    }

    fn iter(&self) -> impl Iterator<Item = Vec<u64>> {
        let mut out = Vec::with_capacity(self.term_count());
        let mut path = Vec::new();
        self.collect_sequences(&self.root, &mut path, &mut out);
        out.into_iter()
    }

    fn iter_with_values(&self) -> impl Iterator<Item = (Vec<u64>, V)> {
        let mut out = Vec::new();
        let mut path = Vec::new();
        self.collect_sequences_with_values(&self.root, &mut path, &mut out);
        out.into_iter()
    }

    fn collect_sequences(
        &self,
        node: &Arc<NativeU64Node<V>>,
        path: &mut Vec<u64>,
        out: &mut Vec<Vec<u64>>,
    ) {
        if node.is_final.load(Ordering::Acquire) {
            out.push(path.clone());
        }
        for (label, child) in node.edges.load().edges_vec() {
            path.push(label);
            self.collect_sequences(&child, path, out);
            path.pop();
        }
    }

    fn collect_sequences_with_values(
        &self,
        node: &Arc<NativeU64Node<V>>,
        path: &mut Vec<u64>,
        out: &mut Vec<(Vec<u64>, V)>,
    ) {
        if node.is_final.load(Ordering::Acquire) {
            let value = node.value.load();
            if let Some(value) = value.as_ref() {
                out.push((path.clone(), (**value).clone()));
            }
        }
        for (label, child) in node.edges.load().edges_vec() {
            path.push(label);
            self.collect_sequences_with_values(&child, path, out);
            path.pop();
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct U64Snapshot<V> {
    magic: [u8; 8],
    version: u32,
    entries: Vec<U64SnapshotEntry<V>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct U64SnapshotEntry<V> {
    sequence: Vec<u64>,
    value: Option<V>,
}

#[derive(Debug, Serialize, Deserialize)]
enum U64WalRecord<V> {
    Insert { sequence: Vec<u64> },
    Upsert { sequence: Vec<u64>, value: V },
    Remove { sequence: Vec<u64> },
}

fn encode_sequence(sequence: &[u64]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(sequence.len() * 8);
    for unit in sequence {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

fn decode_sequence(bytes: &[u8]) -> Option<Vec<u64>> {
    if bytes.len() % 8 != 0 {
        return None;
    }
    Some(
        bytes
            .chunks_exact(8)
            .map(|chunk| {
                let mut word = [0u8; 8];
                word.copy_from_slice(chunk);
                u64::from_le_bytes(word)
            })
            .collect(),
    )
}

fn wal_path(path: &Path) -> PathBuf {
    let mut wal = path.to_path_buf();
    wal.set_extension("u64wal");
    wal
}

fn tmp_snapshot_path(path: &Path) -> PathBuf {
    let mut tmp = path.to_path_buf();
    tmp.set_extension("u64tmp");
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

fn write_snapshot_file<V: DictionaryValue>(
    path: &Path,
    entries: Vec<U64SnapshotEntry<V>>,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| io_error("create parent directory", parent, error))?;
    }

    let snapshot = U64Snapshot {
        magic: SNAPSHOT_MAGIC,
        version: SNAPSHOT_VERSION,
        entries,
    };
    let bytes = serialize_bytes("serialize native u64 snapshot", &snapshot)?;
    let tmp = tmp_snapshot_path(path);

    {
        let mut file = File::create(&tmp)
            .map_err(|error| io_error("create native u64 snapshot", &tmp, error))?;
        file.write_all(&bytes)
            .map_err(|error| io_error("write native u64 snapshot", &tmp, error))?;
        file.sync_all()
            .map_err(|error| io_error("sync native u64 snapshot", &tmp, error))?;
    }

    fs::rename(&tmp, path).map_err(|error| io_error("install native u64 snapshot", path, error))?;
    Ok(())
}

fn read_snapshot_file<V: DictionaryValue>(path: &Path) -> Result<Vec<U64SnapshotEntry<V>>> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|error| io_error("open native u64 snapshot", path, error))?
        .read_to_end(&mut bytes)
        .map_err(|error| io_error("read native u64 snapshot", path, error))?;

    let snapshot: U64Snapshot<V> = deserialize_bytes("deserialize native u64 snapshot", &bytes)?;
    if snapshot.magic != SNAPSHOT_MAGIC {
        return Err(PersistentARTrieError::InvalidMagic {
            expected: u64::from_le_bytes(SNAPSHOT_MAGIC),
            found: u64::from_le_bytes(snapshot.magic),
        });
    }
    if snapshot.version > SNAPSHOT_VERSION {
        return Err(PersistentARTrieError::UnsupportedVersion {
            max_supported: SNAPSHOT_VERSION,
            found: snapshot.version,
        });
    }
    Ok(snapshot.entries)
}

fn truncate_wal(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| io_error("create WAL parent directory", parent, error))?;
    }

    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(|error| io_error("truncate native u64 WAL", path, error))?;
    file.sync_all()
        .map_err(|error| io_error("sync truncated native u64 WAL", path, error))
}

fn append_wal_record<V: DictionaryValue>(path: &Path, record: &U64WalRecord<V>) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| io_error("create WAL parent directory", parent, error))?;
    }

    let payload = serialize_bytes("serialize native u64 WAL record", record)?;
    let len = payload.len() as u64;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| io_error("open native u64 WAL", path, error))?;
    file.write_all(&len.to_le_bytes())
        .map_err(|error| io_error("write native u64 WAL record length", path, error))?;
    file.write_all(&payload)
        .map_err(|error| io_error("write native u64 WAL record", path, error))?;
    file.sync_all()
        .map_err(|error| io_error("sync native u64 WAL", path, error))
}

fn replay_wal<V: DictionaryValue>(inner: &NativeU64Trie<V>, path: &Path) -> Result<u64> {
    if !path.exists() {
        return Ok(0);
    }

    let mut file = File::open(path)
        .map_err(|error| io_error("open native u64 WAL for replay", path, error))?;
    let mut replayed = 0;

    loop {
        let mut len_buf = [0u8; 8];
        match file.read_exact(&mut len_buf) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(error) => return Err(io_error("read native u64 WAL record length", path, error)),
        }

        let len = u64::from_le_bytes(len_buf);
        if len > MAX_WAL_RECORD_BYTES {
            return Err(PersistentARTrieError::corrupted(format!(
                "native u64 WAL record is too large: {len} bytes"
            )));
        }

        let mut payload = vec![0u8; len as usize];
        match file.read_exact(&mut payload) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(error) => return Err(io_error("read native u64 WAL record payload", path, error)),
        }

        let record: U64WalRecord<V> =
            deserialize_bytes("deserialize native u64 WAL record", &payload)?;
        match record {
            U64WalRecord::Insert { sequence } => {
                inner.insert_sequence(&sequence);
            }
            U64WalRecord::Upsert { sequence, value } => {
                inner.insert_sequence_with_value(&sequence, value);
            }
            U64WalRecord::Remove { sequence } => {
                inner.remove_sequence(&sequence);
            }
        }
        replayed += 1;
    }

    Ok(replayed)
}

fn build_native<V: DictionaryValue>(entries: Vec<U64SnapshotEntry<V>>) -> Arc<NativeU64Trie<V>> {
    let trie = Arc::new(NativeU64Trie::new());
    for entry in entries {
        match entry.value {
            Some(value) => {
                trie.insert_sequence_with_value(&entry.sequence, value);
            }
            None => {
                trie.insert_sequence(&entry.sequence);
            }
        }
    }
    trie
}

impl<V: DictionaryValue> PersistentARTrieU64<V> {
    /// Create an in-memory persistent u64 trie.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(NativeU64Trie::new()),
            path: None,
            checkpoint_lock: Arc::new(Mutex::new(())),
            wal_lock: Arc::new(Mutex::new(())),
            _storage: PhantomData,
        }
    }

    pub fn from_sequences<I, T>(sequences: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: AsRef<[u64]>,
    {
        let trie = Self::new();
        for sequence in sequences {
            trie.insert_sequence(sequence.as_ref());
        }
        trie
    }

    pub fn from_sequences_with_values<I, T>(entries: I) -> Self
    where
        I: IntoIterator<Item = (T, V)>,
        T: AsRef<[u64]>,
    {
        let trie = Self::new();
        for (sequence, value) in entries {
            trie.insert_sequence_with_value(sequence.as_ref(), value);
        }
        trie
    }

    pub fn from_terms<I, T>(terms: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: AsRef<str>,
    {
        let trie = Self::new();
        for term in terms {
            trie.insert(term.as_ref());
        }
        trie
    }

    pub fn from_terms_with_values<I, T>(entries: I) -> Self
    where
        I: IntoIterator<Item = (T, V)>,
        T: AsRef<str>,
    {
        let trie = Self::new();
        for (term, value) in entries {
            trie.insert_with_value(term.as_ref(), value);
        }
        trie
    }
}

impl<V: DictionaryValue> PersistentARTrieU64<V, MmapDiskManager> {
    pub fn create<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        write_snapshot_file::<V>(&path, Vec::new())?;
        truncate_wal(&wal_path(&path))?;
        Ok(Self {
            inner: Arc::new(NativeU64Trie::new()),
            path: Some(path),
            checkpoint_lock: Arc::new(Mutex::new(())),
            wal_lock: Arc::new(Mutex::new(())),
            _storage: PhantomData,
        })
    }

    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let (trie, _) = Self::open_loaded(path.as_ref())?;
        Ok(trie)
    }

    pub fn open_with_recovery<P: AsRef<Path>>(path: P) -> Result<(Self, RecoveryReport)> {
        let start = Instant::now();
        let path_ref = path.as_ref();
        if !path_ref.exists() {
            let trie = Self::create(path_ref)?;
            return Ok((trie, RecoveryReport::created_new()));
        }

        let (trie, records_replayed) = Self::open_loaded(path_ref)?;
        let mut report = RecoveryReport::normal();
        if records_replayed > 0 {
            report = RecoveryReport::rebuild_from_wal(
                path_ref.to_path_buf(),
                "native u64 WAL replay".to_string(),
                records_replayed,
                trie.term_count() as u64,
                Vec::new(),
                start.elapsed().as_millis() as u64,
            );
        }
        Ok((trie, report))
    }

    fn open_loaded(path: &Path) -> Result<(Self, u64)> {
        let entries = read_snapshot_file(path)?;
        let inner = build_native(entries);
        let records_replayed = replay_wal(&inner, &wal_path(path))?;
        Ok((
            Self {
                inner,
                path: Some(path.to_path_buf()),
                checkpoint_lock: Arc::new(Mutex::new(())),
                wal_lock: Arc::new(Mutex::new(())),
                _storage: PhantomData,
            },
            records_replayed,
        ))
    }
}

impl<V: DictionaryValue, S: BlockStorage> PersistentARTrieU64<V, S> {
    pub fn inner(&self) -> &NativeU64Trie<V> {
        &self.inner
    }

    pub fn storage_path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    fn persist_record(&self, record: &U64WalRecord<V>) -> Result<()> {
        let Some(path) = self.path.as_ref() else {
            return Ok(());
        };
        let _guard = self.wal_lock.lock();
        append_wal_record(&wal_path(path), record)
    }

    pub fn try_insert_sequence(&self, sequence: &[u64]) -> Result<bool> {
        self.persist_record(&U64WalRecord::Insert {
            sequence: sequence.to_vec(),
        })?;
        Ok(self.inner.insert_sequence(sequence))
    }

    pub fn insert_sequence(&self, sequence: &[u64]) -> bool {
        self.try_insert_sequence(sequence).unwrap_or_else(|error| {
            log::warn!("PersistentARTrieU64::insert_sequence failed: {error}");
            false
        })
    }

    pub fn try_insert_sequence_with_value(&self, sequence: &[u64], value: V) -> Result<bool> {
        self.persist_record(&U64WalRecord::Upsert {
            sequence: sequence.to_vec(),
            value: value.clone(),
        })?;
        Ok(self.inner.insert_sequence_with_value(sequence, value))
    }

    pub fn insert_sequence_with_value(&self, sequence: &[u64], value: V) -> bool {
        self.try_insert_sequence_with_value(sequence, value)
            .unwrap_or_else(|error| {
                log::warn!("PersistentARTrieU64::insert_sequence_with_value failed: {error}");
                false
            })
    }

    pub fn update_or_insert_sequence<F>(
        &self,
        sequence: &[u64],
        default_value: V,
        update_fn: F,
    ) -> bool
    where
        F: FnOnce(&mut V),
    {
        if let Some(mut value) = self.inner.get_sequence_value(sequence) {
            update_fn(&mut value);
            self.try_insert_sequence_with_value(sequence, value)
                .unwrap_or_else(|error| {
                    log::warn!("PersistentARTrieU64::update_or_insert_sequence failed: {error}");
                    false
                });
            false
        } else {
            self.insert_sequence_with_value(sequence, default_value)
        }
    }

    pub fn contains_sequence(&self, sequence: &[u64]) -> bool {
        self.inner.contains_sequence(sequence)
    }

    pub fn get_sequence_value(&self, sequence: &[u64]) -> Option<V> {
        self.inner.get_sequence_value(sequence)
    }

    pub fn try_remove_sequence(&self, sequence: &[u64]) -> Result<bool> {
        if !self.inner.contains_sequence(sequence) {
            return Ok(false);
        }
        self.persist_record(&U64WalRecord::Remove {
            sequence: sequence.to_vec(),
        })?;
        Ok(self.inner.remove_sequence(sequence))
    }

    pub fn remove_sequence(&self, sequence: &[u64]) -> bool {
        self.try_remove_sequence(sequence).unwrap_or_else(|error| {
            log::warn!("PersistentARTrieU64::remove_sequence failed: {error}");
            false
        })
    }

    pub fn term_count(&self) -> usize {
        self.inner.term_count()
    }

    pub fn iter_sequences(&self) -> impl Iterator<Item = Vec<u64>> + '_ {
        self.inner.iter()
    }

    pub fn iter_sequences_with_values(&self) -> impl Iterator<Item = (Vec<u64>, Option<V>)> + '_ {
        let mut values: HashMap<Vec<u64>, V> = self.inner.iter_with_values().collect();
        self.inner.iter().map(move |sequence| {
            let value = values.remove(&sequence);
            (sequence, value)
        })
    }

    pub fn iter_sequence_prefix(&self, prefix: &[u64]) -> Box<dyn Iterator<Item = Vec<u64>> + '_> {
        let prefix = prefix.to_vec();
        Box::new(
            self.inner
                .iter()
                .filter(move |sequence| sequence.starts_with(&prefix)),
        )
    }

    pub fn iter_sequence_prefix_with_values(
        &self,
        prefix: &[u64],
    ) -> Box<dyn Iterator<Item = (Vec<u64>, Option<V>)> + '_> {
        let prefix = prefix.to_vec();
        Box::new(
            self.iter_sequences_with_values()
                .filter(move |(sequence, _)| sequence.starts_with(&prefix)),
        )
    }

    pub fn insert_f64(&self, series: &[f64]) -> bool {
        let sequence: Vec<u64> = series.iter().map(|value| value.to_bits()).collect();
        self.insert_sequence(&sequence)
    }

    pub fn insert_f64_with_value(&self, series: &[f64], value: V) -> bool {
        let sequence: Vec<u64> = series.iter().map(|value| value.to_bits()).collect();
        self.insert_sequence_with_value(&sequence, value)
    }

    pub fn contains_f64(&self, series: &[f64]) -> bool {
        let sequence: Vec<u64> = series.iter().map(|value| value.to_bits()).collect();
        self.contains_sequence(&sequence)
    }

    pub fn get_f64_value(&self, series: &[f64]) -> Option<V> {
        let sequence: Vec<u64> = series.iter().map(|value| value.to_bits()).collect();
        self.get_sequence_value(&sequence)
    }

    pub fn remove_f64(&self, series: &[f64]) -> bool {
        let sequence: Vec<u64> = series.iter().map(|value| value.to_bits()).collect();
        self.remove_sequence(&sequence)
    }

    pub fn insert(&self, term: &str) -> bool {
        let sequence = <u64 as CharUnit>::from_str(term);
        self.insert_sequence(&sequence)
    }

    pub fn insert_with_value(&self, term: &str, value: V) -> bool {
        let sequence = <u64 as CharUnit>::from_str(term);
        self.insert_sequence_with_value(&sequence, value)
    }

    pub fn contains(&self, term: &str) -> bool {
        let sequence = <u64 as CharUnit>::from_str(term);
        self.contains_sequence(&sequence)
    }

    pub fn get_value(&self, term: &str) -> Option<V> {
        let sequence = <u64 as CharUnit>::from_str(term);
        self.get_sequence_value(&sequence)
    }

    pub fn remove(&self, term: &str) -> bool {
        let sequence = <u64 as CharUnit>::from_str(term);
        self.remove_sequence(&sequence)
    }

    pub fn checkpoint(&self) -> Result<()> {
        let Some(path) = self.path.as_ref() else {
            return Ok(());
        };

        let _checkpoint_guard = self.checkpoint_lock.lock();
        let _wal_guard = self.wal_lock.lock();
        let mut entries: Vec<_> = self
            .iter_sequences_with_values()
            .map(|(sequence, value)| U64SnapshotEntry { sequence, value })
            .collect();
        entries.sort_by(|left, right| left.sequence.cmp(&right.sequence));
        write_snapshot_file(path, entries)?;
        truncate_wal(&wal_path(path))
    }

    pub fn close(&self) {
        if let Err(error) = self.checkpoint() {
            log::warn!("PersistentARTrieU64::close checkpoint failed: {error}");
        }
    }
}

impl<V: DictionaryValue> fmt::Debug for PersistentARTrieU64Node<V> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersistentARTrieU64Node")
            .field("path", &self.path)
            .field("is_final", &self.is_final())
            .field("edge_count", &self.edge_count())
            .finish()
    }
}

impl<V: DictionaryValue> DictionaryNode for PersistentARTrieU64Node<V> {
    type Unit = u64;

    fn is_final(&self) -> bool {
        self.inner.is_final.load(Ordering::Acquire)
    }

    fn transition(&self, label: Self::Unit) -> Option<Self> {
        let edges = self.inner.edges.load();
        let inner = edges.find(label)?.clone();
        let mut path = self.path.clone();
        path.push(label);
        Some(Self { inner, path })
    }

    fn edges(&self) -> Box<dyn Iterator<Item = (Self::Unit, Self)> + '_> {
        let path = self.path.clone();
        let edges: Vec<_> = self
            .inner
            .edges
            .load()
            .edges_vec()
            .into_iter()
            .map(|(label, inner)| {
                let mut child_path = path.clone();
                child_path.push(label);
                (
                    label,
                    Self {
                        inner,
                        path: child_path,
                    },
                )
            })
            .collect();
        Box::new(edges.into_iter())
    }

    fn edge_count(&self) -> Option<usize> {
        Some(self.inner.edges.load().len())
    }
}

impl<V: DictionaryValue> MappedDictionaryNode for PersistentARTrieU64Node<V> {
    type Value = V;

    fn value(&self) -> Option<Self::Value> {
        if !self.inner.is_final.load(Ordering::Acquire) {
            return None;
        }
        let value = self.inner.value.load();
        value.as_ref().map(|value| (**value).clone())
    }
}

impl<V: DictionaryValue, S: BlockStorage> Dictionary for PersistentARTrieU64<V, S> {
    type Node = PersistentARTrieU64Node<V>;

    fn root(&self) -> Self::Node {
        Self::Node {
            inner: self.inner.root(),
            path: Vec::new(),
        }
    }

    fn contains(&self, term: &str) -> bool {
        PersistentARTrieU64::contains(self, term)
    }

    fn len(&self) -> Option<usize> {
        Some(self.term_count())
    }

    fn sync_strategy(&self) -> SyncStrategy {
        SyncStrategy::InternalSync
    }
}

impl<V: DictionaryValue, S: BlockStorage> MappedDictionary for PersistentARTrieU64<V, S> {
    type Value = V;

    fn get_value(&self, term: &str) -> Option<Self::Value> {
        PersistentARTrieU64::get_value(self, term)
    }
}

impl<V: DictionaryValue, S: BlockStorage> MutableDictionary for PersistentARTrieU64<V, S> {
    fn insert(&self, term: &str) -> bool {
        PersistentARTrieU64::insert(self, term)
    }

    fn remove(&self, term: &str) -> bool {
        PersistentARTrieU64::remove(self, term)
    }
}

impl<V: DictionaryValue, S: BlockStorage> MutableMappedDictionary for PersistentARTrieU64<V, S> {
    fn insert_with_value(&self, term: &str, value: Self::Value) -> bool {
        PersistentARTrieU64::insert_with_value(self, term, value)
    }

    fn update_or_insert<F>(&self, term: &str, default_value: Self::Value, update_fn: F) -> bool
    where
        F: FnOnce(&mut Self::Value),
    {
        let sequence = <u64 as CharUnit>::from_str(term);
        self.update_or_insert_sequence(&sequence, default_value, update_fn)
    }

    fn union_with<F>(&self, other: &Self, merge_fn: F) -> usize
    where
        F: Fn(&Self::Value, &Self::Value) -> Self::Value,
        Self::Value: Clone,
    {
        let mut processed = 0;
        for (sequence, other_value) in other.iter_sequences_with_values() {
            let Some(other_value) = other_value else {
                continue;
            };
            processed += 1;
            let value = if let Some(self_value) = self.get_sequence_value(&sequence) {
                merge_fn(&self_value, &other_value)
            } else {
                other_value
            };
            self.insert_sequence_with_value(&sequence, value);
        }
        processed
    }
}

impl<V: DictionaryValue> Default for PersistentARTrieU64<V> {
    fn default() -> Self {
        Self::new()
    }
}

/// Byte-encoded u64 persistent trie kept as a benchmark and migration control.
///
/// Each public `u64` is encoded as eight little-endian `u8` transitions through
/// the established byte `PersistentARTrie`. New code should use
/// [`PersistentARTrieU64`] unless it explicitly needs the encoded control.
pub struct EncodedPersistentARTrieU64<V: DictionaryValue = (), S: BlockStorage = MmapDiskManager> {
    inner: PersistentARTrie<V, S>,
}

impl<V: DictionaryValue> EncodedPersistentARTrieU64<V> {
    pub fn new() -> Self {
        #[allow(deprecated)]
        let inner = PersistentARTrie::new();
        Self { inner }
    }
}

impl<V: DictionaryValue> EncodedPersistentARTrieU64<V, MmapDiskManager> {
    pub fn create<P: AsRef<Path>>(path: P) -> Result<Self> {
        PersistentARTrie::create(path).map(|inner| Self { inner })
    }

    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        PersistentARTrie::open(path).map(|inner| Self { inner })
    }
}

impl<V: DictionaryValue, S: BlockStorage> EncodedPersistentARTrieU64<V, S> {
    pub fn inner(&self) -> &PersistentARTrie<V, S> {
        &self.inner
    }

    pub fn try_insert_sequence(&self, sequence: &[u64]) -> Result<bool> {
        let key = encode_sequence(sequence);
        self.inner.insert_cas_durable(&key)
    }

    pub fn insert_sequence(&self, sequence: &[u64]) -> bool {
        self.try_insert_sequence(sequence).unwrap_or_else(|error| {
            log::warn!("EncodedPersistentARTrieU64::insert_sequence failed: {error}");
            false
        })
    }

    pub fn try_insert_sequence_with_value(&self, sequence: &[u64], value: V) -> Result<bool> {
        let key = encode_sequence(sequence);
        self.inner.upsert_bytes(&key, value)
    }

    pub fn insert_sequence_with_value(&self, sequence: &[u64], value: V) -> bool {
        self.try_insert_sequence_with_value(sequence, value)
            .unwrap_or_else(|error| {
                log::warn!(
                    "EncodedPersistentARTrieU64::insert_sequence_with_value failed: {error}"
                );
                false
            })
    }

    pub fn contains_sequence(&self, sequence: &[u64]) -> bool {
        let key = encode_sequence(sequence);
        self.inner.contains_bytes(&key)
    }

    pub fn get_sequence_value(&self, sequence: &[u64]) -> Option<V> {
        let key = encode_sequence(sequence);
        self.inner.get_value_bytes(&key)
    }

    pub fn try_remove_sequence(&self, sequence: &[u64]) -> Result<bool> {
        let key = encode_sequence(sequence);
        self.inner.remove_cas_durable(&key)
    }

    pub fn remove_sequence(&self, sequence: &[u64]) -> bool {
        self.try_remove_sequence(sequence).unwrap_or_else(|error| {
            log::warn!("EncodedPersistentARTrieU64::remove_sequence failed: {error}");
            false
        })
    }

    pub fn term_count(&self) -> usize {
        self.iter_sequences().count()
    }

    pub fn iter_sequences(&self) -> impl Iterator<Item = Vec<u64>> + '_ {
        self.inner.iter().filter_map(|term| decode_sequence(&term))
    }

    pub fn iter_sequences_with_values(&self) -> impl Iterator<Item = (Vec<u64>, Option<V>)> + '_ {
        self.inner
            .iter_with_values()
            .filter_map(|(term, value)| decode_sequence(&term).map(|sequence| (sequence, value)))
    }

    pub fn checkpoint(&self) -> Result<()> {
        self.inner.checkpoint()
    }

    pub fn close(&self) {
        self.inner.close();
    }
}

impl<V: DictionaryValue> Default for EncodedPersistentARTrieU64<V> {
    fn default() -> Self {
        Self::new()
    }
}
