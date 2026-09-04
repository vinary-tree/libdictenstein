//! Persistent SCDAWG dictionaries backed by native compact suffix graphs.
//!
//! The byte and Unicode variants persist active term records plus operation WAL
//! records, then rebuild and publish immutable [`ScdawgCoreInner`] snapshots.
//! New writes use atomically-published prepare/commit segment files; recovery
//! also reads the historical length-prefixed monolithic WAL. Reads traverse the
//! compact SCDAWG graph without taking a writer lock. Retryable mutations
//! rebuild a compact graph and publish it with pointer-identity CAS after a
//! prepared WAL record, then append a commit marker before acknowledging the
//! caller.

use std::collections::{HashMap, HashSet};
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
const SNAPSHOT_VERSION: u32 = 2;
const LEGACY_SNAPSHOT_VERSION: u32 = 1;
const MAX_WAL_RECORD_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CAS_RETRIES: usize = 64;

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
    sorted_active_record_indices: Vec<usize>,
    needs_compaction: bool,
}

impl<U: PersistentScdawgUnit, V: DictionaryValue> NativeScdawgGraph<U, V> {
    fn new() -> Self {
        Self::from_records(Vec::new(), false)
    }

    fn from_records(records: Vec<ScdawgTermRecord<V>>, needs_compaction: bool) -> Self {
        let mut sorted_active_record_indices: Vec<_> = records
            .iter()
            .enumerate()
            .filter_map(|(index, record)| record.active.then_some(index))
            .collect();
        sorted_active_record_indices.sort_by(|&left, &right| {
            records[left]
                .term
                .cmp(&records[right].term)
                .then_with(|| left.cmp(&right))
        });
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
            sorted_active_record_indices,
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
        self.sorted_active_record_indices.len()
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
        F: Fn(&mut V),
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
    checkpoint_op_id: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(bound(
    serialize = "V: serde::Serialize",
    deserialize = "V: serde::de::DeserializeOwned"
))]
struct NativeScdawgSnapshotV1<V: DictionaryValue> {
    magic: [u8; 8],
    version: u32,
    records: Vec<ScdawgTermRecord<V>>,
    needs_compaction: bool,
}

struct LoadedScdawgSnapshot<V: DictionaryValue> {
    records: Vec<ScdawgTermRecord<V>>,
    needs_compaction: bool,
    checkpoint_op_id: u64,
    folds_legacy_wal: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(bound(
    serialize = "V: serde::Serialize",
    deserialize = "V: serde::de::DeserializeOwned"
))]
enum NativeScdawgWalOp<V: DictionaryValue> {
    Insert { term: String, value: Option<V> },
    Remove { term: String },
    SetValue { term: String, value: V },
    Clear,
    Compact,
}

#[derive(Serialize, Deserialize)]
#[serde(bound(
    serialize = "V: serde::Serialize",
    deserialize = "V: serde::de::DeserializeOwned"
))]
enum NativeScdawgWalRecord<V: DictionaryValue> {
    Insert {
        term: String,
        value: Option<V>,
    },
    Remove {
        term: String,
    },
    SetValue {
        term: String,
        value: V,
    },
    Clear,
    Compact,
    Prepare {
        op_id: u64,
        op: NativeScdawgWalOp<V>,
    },
    Commit {
        op_id: u64,
    },
}

#[derive(Debug, Default)]
struct ScdawgWalReplayReport {
    replayed: u64,
    max_op_id: u64,
}

struct NativeScdawgIndex<U: PersistentScdawgUnit, V: DictionaryValue> {
    graph: ArcSwap<NativeScdawgGraph<U, V>>,
    path: Option<PathBuf>,
    next_op_id: AtomicU64,
    committed_op_id: AtomicU64,
    inflight_publications: AtomicUsize,
}

fn wal_path(path: &Path) -> PathBuf {
    let mut wal = path.to_path_buf();
    wal.set_extension("scdawgwal");
    wal
}

fn wal_segment_dir(path: &Path) -> PathBuf {
    let mut dir = path.to_path_buf();
    dir.set_extension("scdawgwal.d");
    dir
}

fn wal_segment_dir_from_wal(path: &Path) -> PathBuf {
    let mut dir = path.to_path_buf();
    dir.set_extension("scdawgwal.d");
    dir
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
            next_op_id: AtomicU64::new(1),
            committed_op_id: AtomicU64::new(0),
            inflight_publications: AtomicUsize::new(0),
        }
    }

    fn create(path: &Path) -> Result<Self> {
        let graph = NativeScdawgGraph::new();
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
        let loaded = read_snapshot_records::<U, V>(path)?;
        let mut records = loaded.records;
        let report = replay_wal::<V>(
            &mut records,
            &wal_path(path),
            loaded.checkpoint_op_id,
            loaded.folds_legacy_wal,
        )?;
        let max_op_id = loaded.checkpoint_op_id.max(report.max_op_id);
        let graph = NativeScdawgGraph::from_records(records, loaded.needs_compaction);
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

    fn load(&self) -> Arc<NativeScdawgGraph<U, V>> {
        self.graph.load_full()
    }

    fn append_record(&self, record: &NativeScdawgWalRecord<V>) -> Result<()> {
        let Some(path) = self.path.as_ref() else {
            return Ok(());
        };
        append_wal_segment(&wal_segment_dir(path), record)
    }

    fn apply_op(graph: &mut NativeScdawgGraph<U, V>, op: NativeScdawgWalOp<V>) -> bool {
        match op {
            NativeScdawgWalOp::Insert { term, value } => graph.insert_record(&term, value),
            NativeScdawgWalOp::Remove { term } => graph.remove_record(&term),
            NativeScdawgWalOp::SetValue { term, value } => {
                if let Some(record) = graph
                    .records
                    .iter_mut()
                    .find(|record| record.active && record.term == term)
                {
                    record.value = Some(value);
                } else {
                    graph.records.push(ScdawgTermRecord {
                        term,
                        value: Some(value),
                        active: true,
                    });
                }
                true
            }
            NativeScdawgWalOp::Clear => {
                graph.clear();
                true
            }
            NativeScdawgWalOp::Compact => graph.compact() > 0,
        }
    }

    fn rebuild_from_current(&self) -> (Arc<NativeScdawgGraph<U, V>>, NativeScdawgGraph<U, V>) {
        let current = self.graph.load_full();
        let next =
            NativeScdawgGraph::from_records(current.records.clone(), current.needs_compaction);
        (current, next)
    }

    fn mutate_retryable(&self, op: NativeScdawgWalOp<V>) -> Result<bool> {
        if self.path.is_none() {
            loop {
                let (current, mut next) = self.rebuild_from_current();
                let result = Self::apply_op(&mut next, op.clone());
                let rebuilt = NativeScdawgGraph::from_records(next.records, next.needs_compaction);
                let previous = self.graph.compare_and_swap(&current, Arc::new(rebuilt));
                if Arc::ptr_eq(&previous, &current) {
                    return Ok(result);
                }
            }
        }

        for _ in 0..MAX_CAS_RETRIES {
            let op_id = self.next_op_id.fetch_add(1, Ordering::Relaxed);
            self.append_record(&NativeScdawgWalRecord::Prepare {
                op_id,
                op: op.clone(),
            })?;
            let (current, mut next) = self.rebuild_from_current();
            let result = Self::apply_op(&mut next, op.clone());
            let rebuilt = NativeScdawgGraph::from_records(next.records, next.needs_compaction);
            self.inflight_publications.fetch_add(1, Ordering::SeqCst);
            let previous = self.graph.compare_and_swap(&current, Arc::new(rebuilt));
            if Arc::ptr_eq(&previous, &current) {
                let commit_result = self.append_record(&NativeScdawgWalRecord::Commit { op_id });
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
            "native SCDAWG CAS failed after {MAX_CAS_RETRIES} retries"
        )))
    }

    fn insert(&self, term: &str, value: Option<V>) -> Result<bool> {
        self.mutate_retryable(NativeScdawgWalOp::Insert {
            term: term.to_string(),
            value,
        })
    }

    fn remove(&self, term: &str) -> Result<bool> {
        self.mutate_retryable(NativeScdawgWalOp::Remove {
            term: term.to_string(),
        })
    }

    fn clear(&self) -> Result<()> {
        self.mutate_retryable(NativeScdawgWalOp::Clear).map(|_| ())
    }

    fn compact(&self) -> Result<usize> {
        self.mutate_retryable(NativeScdawgWalOp::Compact)
            .map(usize::from)
    }

    fn update_or_insert<F>(&self, term: &str, default_value: V, update_fn: F) -> Result<bool>
    where
        F: Fn(&mut V),
    {
        if self.path.is_none() {
            loop {
                let (current, mut next) = self.rebuild_from_current();
                let was_new = next.update_or_insert(term, default_value.clone(), &update_fn);
                let rebuilt = NativeScdawgGraph::from_records(next.records, next.needs_compaction);
                let previous = self.graph.compare_and_swap(&current, Arc::new(rebuilt));
                if Arc::ptr_eq(&previous, &current) {
                    return Ok(was_new);
                }
            }
        }

        for _ in 0..MAX_CAS_RETRIES {
            let (current, mut next) = self.rebuild_from_current();
            let was_new = next.update_or_insert(term, default_value.clone(), &update_fn);
            let value = next
                .records
                .iter()
                .find(|record| record.active && record.term == term)
                .and_then(|record| record.value.clone())
                .expect("update_or_insert installs a value");
            let op_id = self.next_op_id.fetch_add(1, Ordering::Relaxed);
            self.append_record(&NativeScdawgWalRecord::Prepare {
                op_id,
                op: NativeScdawgWalOp::SetValue {
                    term: term.to_string(),
                    value,
                },
            })?;
            let rebuilt = NativeScdawgGraph::from_records(next.records, next.needs_compaction);
            self.inflight_publications.fetch_add(1, Ordering::SeqCst);
            let previous = self.graph.compare_and_swap(&current, Arc::new(rebuilt));
            if Arc::ptr_eq(&previous, &current) {
                let commit_result = self.append_record(&NativeScdawgWalRecord::Commit { op_id });
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
            "native SCDAWG value update CAS failed after {MAX_CAS_RETRIES} retries"
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
        log::debug!("native SCDAWG checkpoint skipped after {MAX_CAS_RETRIES} unstable attempts");
        Ok(())
    }
}

fn write_snapshot_file<U: PersistentScdawgUnit, V: DictionaryValue>(
    path: &Path,
    graph: &NativeScdawgGraph<U, V>,
    checkpoint_op_id: u64,
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
        checkpoint_op_id,
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
) -> Result<LoadedScdawgSnapshot<V>> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|error| io_error("open SCDAWG snapshot", path, error))?
        .read_to_end(&mut bytes)
        .map_err(|error| io_error("read SCDAWG snapshot", path, error))?;

    if bytes.len() < 12 {
        return Err(PersistentARTrieError::corrupted(format!(
            "native SCDAWG snapshot is too short: {} bytes",
            bytes.len()
        )));
    }
    let magic: [u8; 8] = bytes[0..8]
        .try_into()
        .expect("slice length checked for SCDAWG snapshot magic");
    if magic != U::MAGIC {
        return Err(PersistentARTrieError::InvalidMagic {
            expected: u64::from_le_bytes(U::MAGIC),
            found: u64::from_le_bytes(magic),
        });
    }
    let version = u32::from_le_bytes(
        bytes[8..12]
            .try_into()
            .expect("slice length checked for SCDAWG snapshot version"),
    );
    match version {
        SNAPSHOT_VERSION => {
            let snapshot: NativeScdawgSnapshot<V> =
                deserialize_bytes("deserialize native SCDAWG snapshot", &bytes)?;
            Ok(LoadedScdawgSnapshot {
                records: snapshot.records,
                needs_compaction: snapshot.needs_compaction,
                checkpoint_op_id: snapshot.checkpoint_op_id,
                folds_legacy_wal: true,
            })
        }
        LEGACY_SNAPSHOT_VERSION => {
            let snapshot: NativeScdawgSnapshotV1<V> =
                deserialize_bytes("deserialize native SCDAWG snapshot v1", &bytes)?;
            Ok(LoadedScdawgSnapshot {
                records: snapshot.records,
                needs_compaction: snapshot.needs_compaction,
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
            .map_err(|error| io_error("create SCDAWG WAL parent directory", parent, error))?;
    }
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(|error| io_error("truncate SCDAWG WAL", path, error))?;
    file.sync_all()
        .map_err(|error| io_error("sync SCDAWG WAL", path, error))?;
    let segment_dir = wal_segment_dir_from_wal(path);
    if segment_dir.exists() {
        fs::remove_dir_all(&segment_dir)
            .map_err(|error| io_error("clear SCDAWG WAL segment directory", &segment_dir, error))?;
    }
    fs::create_dir_all(&segment_dir)
        .map_err(|error| io_error("create SCDAWG WAL segment directory", &segment_dir, error))
}

fn wal_segment_id_and_kind<V: DictionaryValue>(
    record: &NativeScdawgWalRecord<V>,
) -> Result<(u64, &'static str)> {
    match record {
        NativeScdawgWalRecord::Prepare { op_id, .. } => Ok((*op_id, "prepare")),
        NativeScdawgWalRecord::Commit { op_id } => Ok((*op_id, "commit")),
        _ => Err(PersistentARTrieError::internal(
            "native SCDAWG segment WAL only accepts prepare/commit records".to_string(),
        )),
    }
}

fn append_wal_segment<V: DictionaryValue>(
    dir: &Path,
    record: &NativeScdawgWalRecord<V>,
) -> Result<()> {
    fs::create_dir_all(dir)
        .map_err(|error| io_error("create SCDAWG WAL segment directory", dir, error))?;
    let payload = serialize_bytes("serialize native SCDAWG WAL record", record)?;
    let len = payload.len() as u64;
    let mut frame = Vec::with_capacity(8 + payload.len());
    frame.extend_from_slice(&len.to_le_bytes());
    frame.extend_from_slice(&payload);
    let (op_id, kind) = wal_segment_id_and_kind(record)?;
    let final_path = dir.join(format!("{op_id:020}.{kind}.wal"));
    let tmp_path = dir.join(format!("{op_id:020}.{kind}.{}.tmp", std::process::id()));
    let _ = fs::remove_file(&tmp_path);
    let mut file = File::create(&tmp_path)
        .map_err(|error| io_error("create SCDAWG WAL segment", &tmp_path, error))?;
    file.write_all(&frame)
        .map_err(|error| io_error("write SCDAWG WAL segment", &tmp_path, error))?;
    file.sync_all()
        .map_err(|error| io_error("sync SCDAWG WAL segment", &tmp_path, error))?;
    drop(file);
    fs::rename(&tmp_path, &final_path)
        .map_err(|error| io_error("publish SCDAWG WAL segment", &final_path, error))?;
    File::open(dir)
        .and_then(|dir_file| dir_file.sync_all())
        .map_err(|error| io_error("sync SCDAWG WAL segment directory", dir, error))
}

fn prune_wal_segments(dir: &Path, checkpoint_op_id: u64) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)
        .map_err(|error| io_error("read SCDAWG WAL segment directory", dir, error))?
    {
        let entry = entry
            .map_err(|error| io_error("read SCDAWG WAL segment directory entry", dir, error))?;
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
                io_error("remove checkpointed SCDAWG WAL segment", &path, error)
            })?;
        }
    }
    File::open(dir)
        .and_then(|dir_file| dir_file.sync_all())
        .map_err(|error| io_error("sync pruned SCDAWG WAL segment directory", dir, error))
}

fn absorb_wal_record<V: DictionaryValue>(
    record: NativeScdawgWalRecord<V>,
    report: &mut ScdawgWalReplayReport,
    prepared: &mut HashMap<u64, NativeScdawgWalOp<V>>,
    committed: &mut HashSet<u64>,
    legacy_records: &mut Vec<NativeScdawgWalOp<V>>,
) {
    match record {
        NativeScdawgWalRecord::Insert { term, value } => {
            legacy_records.push(NativeScdawgWalOp::Insert { term, value });
        }
        NativeScdawgWalRecord::Remove { term } => {
            legacy_records.push(NativeScdawgWalOp::Remove { term });
        }
        NativeScdawgWalRecord::SetValue { term, value } => {
            legacy_records.push(NativeScdawgWalOp::SetValue { term, value });
        }
        NativeScdawgWalRecord::Clear => legacy_records.push(NativeScdawgWalOp::Clear),
        NativeScdawgWalRecord::Compact => legacy_records.push(NativeScdawgWalOp::Compact),
        NativeScdawgWalRecord::Prepare { op_id, op } => {
            report.max_op_id = report.max_op_id.max(op_id);
            prepared.insert(op_id, op);
        }
        NativeScdawgWalRecord::Commit { op_id } => {
            report.max_op_id = report.max_op_id.max(op_id);
            committed.insert(op_id);
        }
    }
    report.replayed += 1;
}

fn replay_wal_segments<V: DictionaryValue>(
    dir: &Path,
    report: &mut ScdawgWalReplayReport,
    prepared: &mut HashMap<u64, NativeScdawgWalOp<V>>,
    committed: &mut HashSet<u64>,
    legacy_records: &mut Vec<NativeScdawgWalOp<V>>,
) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    let mut files = Vec::new();
    for entry in fs::read_dir(dir)
        .map_err(|error| io_error("read SCDAWG WAL segment directory", dir, error))?
    {
        let entry = entry
            .map_err(|error| io_error("read SCDAWG WAL segment directory entry", dir, error))?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) == Some("wal") {
            files.push(path);
        }
    }
    files.sort();
    for path in files {
        let mut file =
            File::open(&path).map_err(|error| io_error("open SCDAWG WAL segment", &path, error))?;
        let mut len_buf = [0u8; 8];
        file.read_exact(&mut len_buf)
            .map_err(|error| io_error("read SCDAWG WAL segment length", &path, error))?;
        let len = u64::from_le_bytes(len_buf);
        if len > MAX_WAL_RECORD_BYTES {
            return Err(PersistentARTrieError::corrupted(format!(
                "native SCDAWG WAL segment is too large: {len} bytes"
            )));
        }
        let mut payload = vec![0u8; len as usize];
        file.read_exact(&mut payload)
            .map_err(|error| io_error("read SCDAWG WAL segment payload", &path, error))?;
        let record: NativeScdawgWalRecord<V> =
            deserialize_bytes("deserialize native SCDAWG WAL segment", &payload)?;
        absorb_wal_record(record, report, prepared, committed, legacy_records);
    }
    Ok(())
}

fn replay_wal<V: DictionaryValue>(
    records: &mut Vec<ScdawgTermRecord<V>>,
    path: &Path,
    checkpoint_op_id: u64,
    folds_legacy_wal: bool,
) -> Result<ScdawgWalReplayReport> {
    let mut report = ScdawgWalReplayReport::default();
    let mut prepared = HashMap::new();
    let mut committed = HashSet::new();
    let mut legacy_records = Vec::new();

    if path.exists() {
        let mut file = File::open(path)
            .map_err(|error| io_error("open SCDAWG WAL for replay", path, error))?;
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
            apply_wal_op(records, op);
        }
    }
    let mut committed_ops: Vec<_> = committed
        .into_iter()
        .filter(|op_id| *op_id > checkpoint_op_id)
        .filter_map(|op_id| prepared.remove(&op_id).map(|op| (op_id, op)))
        .collect();
    committed_ops.sort_by_key(|(op_id, _)| *op_id);
    for (_, op) in committed_ops {
        apply_wal_op(records, op);
    }
    Ok(report)
}

fn apply_wal_op<V: DictionaryValue>(
    records: &mut Vec<ScdawgTermRecord<V>>,
    record: NativeScdawgWalOp<V>,
) {
    match record {
        NativeScdawgWalOp::Insert { term, value } => {
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
        NativeScdawgWalOp::Remove { term } => {
            if let Some(existing) = records
                .iter_mut()
                .find(|record| record.active && record.term == term)
            {
                existing.active = false;
            }
        }
        NativeScdawgWalOp::SetValue { term, value } => {
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
        NativeScdawgWalOp::Clear => records.clear(),
        NativeScdawgWalOp::Compact => records.retain(|record| record.active),
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

/// Snapshot iterator over stored byte-SCDAWG term records.
pub struct PersistentScdawgEntryIterator<V: DictionaryValue = ()> {
    graph: Arc<NativeScdawgGraph<u8, V>>,
    index: usize,
}

/// Snapshot iterator over stored Unicode-SCDAWG term records.
pub struct PersistentScdawgCharEntryIterator<V: DictionaryValue = ()> {
    graph: Arc<NativeScdawgGraph<char, V>>,
    index: usize,
}

macro_rules! impl_scdawg_record_iterator {
    ($iterator:ident) => {
        impl<V: DictionaryValue> Iterator for $iterator<V> {
            type Item = (String, Option<V>);

            fn next(&mut self) -> Option<Self::Item> {
                let record_index = *self.graph.sorted_active_record_indices.get(self.index)?;
                self.index += 1;
                let record = self
                    .graph
                    .records
                    .get(record_index)
                    .expect("the revision record index references a term");
                Some((record.term.clone(), record.value.clone()))
            }

            fn size_hint(&self) -> (usize, Option<usize>) {
                let remaining = self
                    .graph
                    .sorted_active_record_indices
                    .len()
                    .saturating_sub(self.index);
                (remaining, Some(remaining))
            }
        }

        impl<V: DictionaryValue> ExactSizeIterator for $iterator<V> {}
        impl<V: DictionaryValue> FusedIterator for $iterator<V> {}
    };
}

impl_scdawg_record_iterator!(PersistentScdawgEntryIterator);
impl_scdawg_record_iterator!(PersistentScdawgCharEntryIterator);

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
    pub fn try_insert(&self, term: &str) -> Result<bool> {
        self.index.insert(term, None)
    }

    pub fn insert(&self, term: &str) -> bool {
        self.try_insert(term).unwrap_or_else(|error| {
            log::warn!("PersistentScdawg::insert failed: {error}");
            false
        })
    }

    pub fn try_insert_with_value(&self, term: &str, value: V) -> Result<bool> {
        self.index.insert(term, Some(value))
    }

    pub fn insert_with_value(&self, term: &str, value: V) -> bool {
        self.try_insert_with_value(term, value)
            .unwrap_or_else(|error| {
                log::warn!("PersistentScdawg::insert_with_value failed: {error}");
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
        unique_terms(self.index.load().active_terms()).into_iter()
    }

    pub fn source_texts(&self) -> Vec<String> {
        self.iter().collect()
    }

    /// Iterate over stored term records from one immutable revision.
    pub fn iter_entries(&self) -> PersistentScdawgEntryIterator<V> {
        PersistentScdawgEntryIterator {
            graph: self.index.load(),
            index: 0,
        }
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

    /// Build from Unicode-scalar profile sequences without exposing UTF-8
    /// encoding bytes as logical suffix transitions.
    pub fn from_atom_sequences<P, I>(sequences: I) -> Self
    where
        P: crate::AtomProfile<Atom = char>,
        I: IntoIterator<Item = crate::AtomSequence<P>>,
    {
        Self::from_terms(
            sequences
                .into_iter()
                .map(|sequence| sequence.as_atoms().iter().copied().collect::<String>()),
        )
    }

    /// Build a value-bearing SCDAWG from Unicode-scalar profile sequences.
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
    /// Read a mapped value for a Unicode-scalar profile sequence.
    pub fn get_atom_sequence_value<P>(&self, sequence: &crate::AtomSequence<P>) -> Option<V>
    where
        P: crate::AtomProfile<Atom = char>,
    {
        let text: String = sequence.as_atoms().iter().collect();
        <Self as crate::MappedDictionary>::get_value(self, &text)
    }

    pub fn try_insert(&self, term: &str) -> Result<bool> {
        self.index.insert(term, None)
    }

    pub fn insert(&self, term: &str) -> bool {
        self.try_insert(term).unwrap_or_else(|error| {
            log::warn!("PersistentScdawgChar::insert failed: {error}");
            false
        })
    }

    pub fn try_insert_with_value(&self, term: &str, value: V) -> Result<bool> {
        self.index.insert(term, Some(value))
    }

    pub fn insert_with_value(&self, term: &str, value: V) -> bool {
        self.try_insert_with_value(term, value)
            .unwrap_or_else(|error| {
                log::warn!("PersistentScdawgChar::insert_with_value failed: {error}");
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
        unique_terms(self.index.load().active_terms()).into_iter()
    }

    pub fn source_texts(&self) -> Vec<String> {
        self.iter().collect()
    }

    /// Iterate over stored term records from one immutable revision.
    pub fn iter_entries(&self) -> PersistentScdawgCharEntryIterator<V> {
        PersistentScdawgCharEntryIterator {
            graph: self.index.load(),
            index: 0,
        }
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
    type SnapshotCursor = crate::SnapshotTraversalCursor;
    type SnapshotGraphValueHandle = crate::SnapshotTraversalCursor;

    #[inline]
    fn snapshot_node_identity(&self) -> Option<crate::SnapshotNodeIdentity> {
        self.node_idx
            .and_then(crate::SnapshotNodeIdentity::from_index)
    }

    #[inline]
    fn snapshot_root_cursor(&self) -> Option<Self::SnapshotCursor> {
        self.graph.core.snapshot_cursor(self.node_idx?)
    }

    #[inline]
    fn contains_snapshot_cursor(&self, cursor: Self::SnapshotCursor) -> bool {
        self.graph.core.contains_snapshot_cursor(cursor)
    }

    #[inline]
    unsafe fn filter_map_snapshot_cursor_edges_and_finality<T, P, F>(
        &self,
        cursor: Self::SnapshotCursor,
        project: P,
        visitor: F,
    ) -> Option<bool>
    where
        P: FnMut(Self::Unit) -> Option<T>,
        F: FnMut(Self::Unit, Self::SnapshotCursor, T),
    {
        self.graph
            .core
            .filter_map_snapshot_cursor_edges_and_finality(cursor, project, visitor)
    }

    #[inline]
    unsafe fn snapshot_cursor_is_final(&self, cursor: Self::SnapshotCursor) -> Option<bool> {
        self.graph.core.snapshot_cursor_is_final(cursor)
    }

    #[inline]
    unsafe fn snapshot_cursor_transition(
        &self,
        cursor: Self::SnapshotCursor,
        label: Self::Unit,
    ) -> Option<Option<Self::SnapshotCursor>> {
        self.graph.core.snapshot_cursor_transition(cursor, label)
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
        visitor: F,
    ) -> Option<(bool, usize)>
    where
        F: FnMut(Self::Unit, Self::SnapshotCursor),
    {
        self.graph
            .core
            .visit_snapshot_cursor_edge_page(cursor, start, capacity, visitor)
    }

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

    #[inline]
    fn for_each_edge<F>(&self, mut visitor: F)
    where
        F: FnMut(Self::Unit, Self),
    {
        let Some(node) = node(&self.graph, self.node_idx) else {
            return;
        };
        for &(label, next) in &node.forward_edges {
            let mut child_path = self.path.clone();
            child_path.push(label);
            visitor(
                label,
                Self {
                    graph: Arc::clone(&self.graph),
                    node_idx: Some(next),
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
        let Some(node) = node(&self.graph, self.node_idx) else {
            return;
        };
        for &(label, next) in &node.forward_edges {
            let Some(projected) = project(label) else {
                continue;
            };
            let mut child_path = self.path.clone();
            child_path.push(label);
            visitor(
                label,
                Self {
                    graph: Arc::clone(&self.graph),
                    node_idx: Some(next),
                    path: child_path,
                },
                projected,
            );
        }
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

    #[inline]
    fn supports_snapshot_cursor_values(&self) -> bool {
        true
    }

    #[inline]
    unsafe fn snapshot_cursor_value(
        &self,
        cursor: Self::SnapshotCursor,
    ) -> Option<Option<Self::Value>> {
        self.graph.core.snapshot_cursor_value(cursor)
    }
}

impl<V: DictionaryValue> DictionaryNode for PersistentScdawgCharNode<V> {
    type Unit = char;
    type SnapshotCursor = crate::SnapshotTraversalCursor;
    type SnapshotGraphValueHandle = crate::SnapshotTraversalCursor;

    #[inline]
    fn snapshot_node_identity(&self) -> Option<crate::SnapshotNodeIdentity> {
        self.node_idx
            .and_then(crate::SnapshotNodeIdentity::from_index)
    }

    #[inline]
    fn snapshot_root_cursor(&self) -> Option<Self::SnapshotCursor> {
        self.graph.core.snapshot_cursor(self.node_idx?)
    }

    #[inline]
    fn contains_snapshot_cursor(&self, cursor: Self::SnapshotCursor) -> bool {
        self.graph.core.contains_snapshot_cursor(cursor)
    }

    #[inline]
    unsafe fn filter_map_snapshot_cursor_edges_and_finality<T, P, F>(
        &self,
        cursor: Self::SnapshotCursor,
        project: P,
        visitor: F,
    ) -> Option<bool>
    where
        P: FnMut(Self::Unit) -> Option<T>,
        F: FnMut(Self::Unit, Self::SnapshotCursor, T),
    {
        self.graph
            .core
            .filter_map_snapshot_cursor_edges_and_finality(cursor, project, visitor)
    }

    #[inline]
    unsafe fn snapshot_cursor_is_final(&self, cursor: Self::SnapshotCursor) -> Option<bool> {
        self.graph.core.snapshot_cursor_is_final(cursor)
    }

    #[inline]
    unsafe fn snapshot_cursor_transition(
        &self,
        cursor: Self::SnapshotCursor,
        label: Self::Unit,
    ) -> Option<Option<Self::SnapshotCursor>> {
        self.graph.core.snapshot_cursor_transition(cursor, label)
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
        visitor: F,
    ) -> Option<(bool, usize)>
    where
        F: FnMut(Self::Unit, Self::SnapshotCursor),
    {
        self.graph
            .core
            .visit_snapshot_cursor_edge_page(cursor, start, capacity, visitor)
    }

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

    #[inline]
    fn for_each_edge<F>(&self, mut visitor: F)
    where
        F: FnMut(Self::Unit, Self),
    {
        let Some(node) = node(&self.graph, self.node_idx) else {
            return;
        };
        for &(label, next) in &node.forward_edges {
            let mut child_path = self.path.clone();
            child_path.push(label);
            visitor(
                label,
                Self {
                    graph: Arc::clone(&self.graph),
                    node_idx: Some(next),
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
        let Some(node) = node(&self.graph, self.node_idx) else {
            return;
        };
        for &(label, next) in &node.forward_edges {
            let Some(projected) = project(label) else {
                continue;
            };
            let mut child_path = self.path.clone();
            child_path.push(label);
            visitor(
                label,
                Self {
                    graph: Arc::clone(&self.graph),
                    node_idx: Some(next),
                    path: child_path,
                },
                projected,
            );
        }
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

    #[inline]
    fn supports_snapshot_cursor_values(&self) -> bool {
        true
    }

    #[inline]
    unsafe fn snapshot_cursor_value(
        &self,
        cursor: Self::SnapshotCursor,
    ) -> Option<Option<Self::Value>> {
        self.graph.core.snapshot_cursor_value(cursor)
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
        F: Fn(&mut Self::Value),
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
    fn find_exact_substring_in_snapshot(
        snapshot_root: &Self::Node,
        pattern: &str,
    ) -> Vec<SubstringMatch<Self::Node>> {
        debug_assert_eq!(snapshot_root.node_idx, Some(0));
        debug_assert!(snapshot_root.path.is_empty());
        let graph = Arc::clone(&snapshot_root.graph);
        let Some(node_idx) = graph.core.find_substring_fast(pattern) else {
            return Vec::new();
        };
        let node = PersistentScdawgNode {
            graph: Arc::clone(&graph),
            node_idx: Some(node_idx),
            path: pattern.as_bytes().to_vec(),
        };
        graph
            .locations(pattern)
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
        F: Fn(&mut Self::Value),
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
    fn find_exact_substring_in_snapshot(
        snapshot_root: &Self::Node,
        pattern: &str,
    ) -> Vec<SubstringMatch<Self::Node>> {
        debug_assert_eq!(snapshot_root.node_idx, Some(0));
        debug_assert!(snapshot_root.path.is_empty());
        let graph = Arc::clone(&snapshot_root.graph);
        let Some(node_idx) = graph.core.find_substring_fast(pattern) else {
            return Vec::new();
        };
        let node = PersistentScdawgCharNode {
            graph: Arc::clone(&graph),
            node_idx: Some(node_idx),
            path: pattern.to_owned(),
        };
        let pattern_len = pattern.chars().count();
        graph
            .locations(pattern)
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

#[cfg(test)]
mod snapshot_cursor_tests {
    use std::collections::HashSet;

    use super::*;

    fn assert_direct_cursor_matches_owned<N>(root: N)
    where
        N: MappedDictionaryNode<Value = u64, SnapshotCursor = crate::SnapshotTraversalCursor>
            + fmt::Debug,
    {
        let root_cursor = root.snapshot_root_cursor().expect("direct root cursor");
        assert!(root.contains_snapshot_cursor(root_cursor));
        assert!(!root.supports_snapshot_cursor_nodes());
        assert!(!root.supports_snapshot_cursor_key_units());
        assert!(root.supports_snapshot_cursor_values());
        assert!(root.supports_efficient_snapshot_cursor_edge_paging());

        let mut seen = HashSet::new();
        let mut pending = vec![(root, root_cursor)];
        while let Some((owned, cursor)) = pending.pop() {
            if !seen.insert(cursor) {
                continue;
            }
            // SAFETY: every cursor is rooted in the retained immutable graph.
            assert_eq!(
                unsafe { owned.snapshot_cursor_is_final(cursor) },
                Some(owned.is_final())
            );
            // SAFETY: same retained graph provenance.
            assert_eq!(
                unsafe { owned.snapshot_cursor_value(cursor) },
                Some(owned.value())
            );

            let mut owned_edges = Vec::new();
            owned.for_each_edge(|label, child| owned_edges.push((label, child)));
            let mut cursor_edges = Vec::new();
            // SAFETY: same retained graph provenance.
            let finality = unsafe {
                owned.filter_map_snapshot_cursor_edges_and_finality(
                    cursor,
                    Some,
                    |label, child, projected| {
                        assert_eq!(label, projected);
                        cursor_edges.push((label, child));
                    },
                )
            };
            assert_eq!(finality, Some(owned.is_final()));
            assert_eq!(
                cursor_edges.iter().map(|edge| edge.0).collect::<Vec<_>>(),
                owned_edges.iter().map(|edge| edge.0).collect::<Vec<_>>()
            );
            assert!(cursor_edges.windows(2).all(|pair| pair[0].0 < pair[1].0));

            let mut paged = Vec::new();
            for start in 0..=cursor_edges.len() {
                let mut page = Vec::new();
                // SAFETY: same retained graph provenance.
                let metadata = unsafe {
                    owned.visit_snapshot_cursor_edge_page(cursor, start, 1, |label, child| {
                        page.push((label, child));
                    })
                };
                assert_eq!(metadata, Some((owned.is_final(), cursor_edges.len())));
                if start < cursor_edges.len() {
                    assert_eq!(page, vec![cursor_edges[start]]);
                    paged.extend(page);
                } else {
                    assert!(page.is_empty());
                }
            }
            assert_eq!(paged, cursor_edges);

            for ((label, child_owned), (_, child_cursor)) in
                owned_edges.into_iter().zip(cursor_edges).rev()
            {
                // SAFETY: same retained graph provenance.
                assert_eq!(
                    unsafe { owned.snapshot_cursor_transition(cursor, label) },
                    Some(Some(child_cursor))
                );
                pending.push((child_owned, child_cursor));
            }
        }
    }

    #[test]
    fn persistent_scdawg_cursors_match_owned_traversal_and_isolate_revisions() {
        let byte = PersistentScdawg::<u64>::new();
        assert!(byte.insert_with_value("banana", 7));
        assert!(byte.insert_with_value("band", 11));
        let old_byte = byte.root();
        let old_byte_cursor = old_byte.snapshot_root_cursor().expect("old byte root");
        assert_direct_cursor_matches_owned(old_byte.clone());
        assert!(byte.insert_with_value("zoo", 13));
        let fresh_byte = byte.root();
        assert_direct_cursor_matches_owned(fresh_byte.clone());
        // SAFETY: the old cursor remains tied to the retained old graph.
        assert_eq!(
            unsafe { old_byte.snapshot_cursor_transition(old_byte_cursor, b'z') },
            Some(None)
        );
        let fresh_byte_cursor = fresh_byte.snapshot_root_cursor().expect("fresh byte root");
        // SAFETY: the fresh cursor belongs to the fresh graph.
        assert!(
            unsafe { fresh_byte.snapshot_cursor_transition(fresh_byte_cursor, b'z') }
                .flatten()
                .is_some()
        );

        let chars = PersistentScdawgChar::<u64>::new();
        assert!(chars.insert_with_value("βα", 17));
        assert!(chars.insert_with_value("βήτα", 19));
        let old_chars = chars.root();
        let old_chars_cursor = old_chars.snapshot_root_cursor().expect("old char root");
        assert_direct_cursor_matches_owned(old_chars.clone());
        assert!(chars.insert_with_value("雪豹", 23));
        let fresh_chars = chars.root();
        assert_direct_cursor_matches_owned(fresh_chars.clone());
        // SAFETY: the old cursor remains tied to the retained old graph.
        assert_eq!(
            unsafe { old_chars.snapshot_cursor_transition(old_chars_cursor, '雪') },
            Some(None)
        );
        let fresh_chars_cursor = fresh_chars.snapshot_root_cursor().expect("fresh char root");
        // SAFETY: the fresh cursor belongs to the fresh graph.
        assert!(
            unsafe { fresh_chars.snapshot_cursor_transition(fresh_chars_cursor, '雪') }
                .flatten()
                .is_some()
        );
    }

    #[test]
    fn persistent_scdawg_old_snapshot_survives_concurrent_publications() {
        let dictionary = Arc::new(PersistentScdawg::<u64>::new());
        assert!(dictionary.insert_with_value("banana", 7));
        let old = Arc::new(dictionary.root());
        let barrier = Arc::new(std::sync::Barrier::new(5));
        let mut readers = Vec::new();
        for _ in 0..4 {
            let old = Arc::clone(&old);
            let barrier = Arc::clone(&barrier);
            readers.push(std::thread::spawn(move || {
                barrier.wait();
                for _ in 0..128 {
                    let root = old.snapshot_root_cursor().expect("old root cursor");
                    // SAFETY: the retained old graph never changes.
                    assert_eq!(
                        unsafe { old.snapshot_cursor_transition(root, b'z') },
                        Some(None)
                    );
                    assert_direct_cursor_matches_owned((*old).clone());
                }
            }));
        }
        barrier.wait();
        for value in 0..32 {
            dictionary.insert_with_value(&format!("zoo{value}"), value);
        }
        for reader in readers {
            reader.join().expect("SCDAWG snapshot reader");
        }
    }
}
