//! Sequence-keyed persistent trie for native `u64` units.
//!
//! The live representation is the same immutable, lock-free overlay architecture
//! used by the byte and char persistent ARTrie variants:
//! `AtomicNodePtr<OverlayNode<U64Key<PREFIX>, V>>`.  The u64 variant keeps native
//! 64-bit labels all the way through insertion, lookup, checkpoint capture, and
//! reopen.  It does not keep the former native bincode snapshot/WAL format; the
//! WAL is the shared `WalRecord` codec and checkpoint capture uses the shared CX
//! overlay compressor with u64-specific node projection. Durable writes use the
//! same Order-A shape as byte/char: log before CAS publication, append
//! `CommitRank` after the winning CAS, advance `CommittedWatermark`, and retain
//! WAL records beyond the checkpoint watermark for recovery.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::core::committed_watermark::CommittedWatermark;
use crate::persistent_artrie::core::key_encoding::{KeyEncoding, U64Key};
use crate::persistent_artrie::core::overlay::atomic_ptr::AtomicNodePtr;
use crate::persistent_artrie::core::overlay::compressed_serialize::OverlayCompressedSerialize;
use crate::persistent_artrie::core::overlay::dict_node::OverlayDictionaryNode;
use crate::persistent_artrie::core::overlay::node::{Child, OverlayNode};
use crate::persistent_artrie::core::recovery::{reconcile_lww, RecoveredOperation};
use crate::persistent_artrie::core::wal::{Lsn, WalReader, WalRecord, WalWriter};
use crate::persistent_artrie::disk_manager::MmapDiskManager;
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::persistent_artrie::swizzled_ptr::{NodeType, SwizzledPtr};
use crate::persistent_artrie::{PersistentARTrie, RecoveryReport};
use crate::serialization::bincode_compat;
use crate::value::DictionaryValue;
use crate::{
    CharUnit, Dictionary, MappedDictionary, MutableDictionary, MutableMappedDictionary,
    SyncStrategy,
};

const SNAPSHOT_MAGIC: [u8; 8] = *b"AR64CX01";
const SNAPSHOT_VERSION: u32 = 1;
const NONE_VALUE_LEN: u64 = u64::MAX;
const MAX_VALUE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_NODE_COUNT: u64 = 16 * 1024 * 1024;
const MAX_PREFIX_UNITS: u32 = 4096;
const MAX_CHILDREN_PER_NODE: u32 = 1_000_000;

/// CX prefix budget for the prefix-3 compatibility/baseline profile.
pub const U64_CX_PREFIX_COMPAT: usize = 3;

/// CX prefix budget for the disk-compact default profile.
pub const U64_CX_PREFIX_COMPACT: usize = 4;

type U64Node<V, const PREFIX: usize> = OverlayNode<U64Key<PREFIX>, V>;

/// Persistent trie keyed by native `u64` sequences.
pub struct PersistentARTrieU64<
    V: DictionaryValue = (),
    S: BlockStorage = MmapDiskManager,
    const PREFIX: usize = U64_CX_PREFIX_COMPACT,
> {
    root: AtomicNodePtr<U64Key<PREFIX>, V>,
    term_count: AtomicUsize,
    path: Option<PathBuf>,
    wal_writer: Option<Arc<WalWriter>>,
    committed_watermark: CommittedWatermark,
    commit_seq: AtomicU64,
    checkpoint_lock: Arc<Mutex<()>>,
    _storage: PhantomData<S>,
}

/// Node handle for [`PersistentARTrieU64`].
pub type PersistentARTrieU64Node<V = (), const PREFIX: usize = U64_CX_PREFIX_COMPACT> =
    OverlayDictionaryNode<U64Key<PREFIX>, V>;

/// Disk-compact u64 profile.
///
/// This is the current default profile (`PREFIX = 4`).  It keeps one native
/// `u64` edge per transition and uses the wider CX prefix budget measured to
/// reduce checkpoint bytes while preserving lookup performance.
pub type PersistentARTrieU64Compact<V = (), S = MmapDiskManager> =
    PersistentARTrieU64<V, S, U64_CX_PREFIX_COMPACT>;

/// Prefix-3 u64 profile kept for compatibility and benchmark baselines.
///
/// Use this alias when opening prefix-3 CX checkpoint files or when comparing
/// the old prefix budget against [`PersistentARTrieU64Compact`].
pub type PersistentARTrieU64Prefix3Compat<V = (), S = MmapDiskManager> =
    PersistentARTrieU64<V, S, U64_CX_PREFIX_COMPAT>;

/// Node handle for [`PersistentARTrieU64Compact`].
pub type PersistentARTrieU64CompactNode<V = ()> = PersistentARTrieU64Node<V, U64_CX_PREFIX_COMPACT>;

/// Node handle for [`PersistentARTrieU64Prefix3Compat`].
pub type PersistentARTrieU64Prefix3CompatNode<V = ()> =
    PersistentARTrieU64Node<V, U64_CX_PREFIX_COMPAT>;

struct U64Projected {
    is_final: bool,
    prefix: Vec<u64>,
    value: Option<Vec<u8>>,
    children: Vec<(u64, u64)>,
}

#[derive(Clone)]
struct U64DiskNode {
    is_final: bool,
    prefix: Vec<u64>,
    value: Option<Vec<u8>>,
    children: Vec<(u64, u64)>,
}

enum U64CasOutcome {
    Published { inserted: bool, generation: u64 },
    Idempotent,
}

#[derive(Default)]
struct U64CxSnapshotBuilder {
    nodes: Mutex<Vec<U64DiskNode>>,
}

impl U64CxSnapshotBuilder {
    fn into_nodes(self) -> Vec<U64DiskNode> {
        self.nodes
            .into_inner()
            .expect("u64 snapshot builder mutex poisoned")
    }
}

impl<V: DictionaryValue, const PREFIX: usize> OverlayCompressedSerialize<U64Key<PREFIX>, V>
    for U64CxSnapshotBuilder
{
    type Projected = U64Projected;

    fn project_node(
        node: &U64Node<V, PREFIX>,
        child_disk_ptrs: &[(u64, SwizzledPtr)],
    ) -> Result<Self::Projected> {
        let value = match node.get_value() {
            Some(value) => Some(bincode_compat::serialize(&value).map_err(|error| {
                PersistentARTrieError::internal(format!("serialize u64 overlay value: {error}"))
            })?),
            None => None,
        };
        Ok(U64Projected {
            is_final: node.is_final(),
            prefix: Vec::new(),
            value,
            children: child_disk_ptrs
                .iter()
                .map(|(label, ptr)| (*label, ptr.to_raw()))
                .collect(),
        })
    }

    fn project_chunk(
        _synth: &U64Node<V, PREFIX>,
        child_disk_ptrs: &[(u64, SwizzledPtr)],
        prefix: &[u64],
    ) -> Result<Self::Projected> {
        Ok(U64Projected {
            is_final: false,
            prefix: prefix.to_vec(),
            value: None,
            children: child_disk_ptrs
                .iter()
                .map(|(label, ptr)| (*label, ptr.to_raw()))
                .collect(),
        })
    }

    fn serialize_projected_node(
        &self,
        projected: &Self::Projected,
        _child_disk_ptrs: &[(u64, SwizzledPtr)],
        _path: &[u64],
        _registry: Option<&mut crate::persistent_artrie::eviction::DiskLocationRegistry>,
    ) -> Result<SwizzledPtr> {
        let mut nodes = self
            .nodes
            .lock()
            .expect("u64 snapshot builder mutex poisoned");
        if nodes.len() as u64 >= MAX_NODE_COUNT {
            return Err(PersistentARTrieError::corrupted(format!(
                "u64 CX checkpoint exceeds maximum node count {MAX_NODE_COUNT}"
            )));
        }
        let index = nodes.len() as u32;
        nodes.push(U64DiskNode {
            is_final: projected.is_final,
            prefix: projected.prefix.clone(),
            value: projected.value.clone(),
            children: projected.children.clone(),
        });
        Ok(SwizzledPtr::on_disk(0, index, NodeType::CharBucket))
    }

    fn new_synth_node() -> U64Node<V, PREFIX> {
        U64Node::<V, PREFIX>::new()
    }

    fn stamp_durable(live: &U64Node<V, PREFIX>, raw: u64) {
        live.set_durable_stamp(raw);
    }
}

fn encode_sequence(sequence: &[u64]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(sequence.len() * 8);
    for unit in sequence {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

fn decode_sequence(bytes: &[u8]) -> Option<Vec<u64>> {
    U64Key::<3>::units_from_bytes(bytes).map(|units| units.into_iter().collect())
}

fn wal_path(path: &Path) -> PathBuf {
    let mut wal = path.to_path_buf();
    wal.set_extension("wal");
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

fn wal_error(context: &str, error: impl std::fmt::Display) -> PersistentARTrieError {
    PersistentARTrieError::internal(format!("{context}: {error}"))
}

fn codec_error(context: &str, error: impl std::fmt::Display) -> PersistentARTrieError {
    PersistentARTrieError::corrupted(format!("{context}: {error}"))
}

fn ensure_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .map_err(|error| io_error("create parent directory", parent, error))?;
        }
    }
    Ok(())
}

fn write_u8(out: &mut Vec<u8>, value: u8) {
    out.push(value);
}

fn write_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn write_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn write_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(bytes);
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or_else(|| PersistentARTrieError::corrupted("u64 snapshot cursor overflow"))?;
        if end > self.bytes.len() {
            return Err(PersistentARTrieError::corrupted(
                "truncated u64 checkpoint image",
            ));
        }
        let out = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32> {
        let mut buf = [0u8; 4];
        buf.copy_from_slice(self.take(4)?);
        Ok(u32::from_le_bytes(buf))
    }

    fn u64(&mut self) -> Result<u64> {
        let mut buf = [0u8; 8];
        buf.copy_from_slice(self.take(8)?);
        Ok(u64::from_le_bytes(buf))
    }

    fn finish(self) -> Result<()> {
        if self.pos == self.bytes.len() {
            Ok(())
        } else {
            Err(PersistentARTrieError::corrupted(format!(
                "u64 checkpoint has {} trailing bytes",
                self.bytes.len() - self.pos
            )))
        }
    }
}

fn write_snapshot_file<V: DictionaryValue, const PREFIX: usize>(
    path: &Path,
    root: &Arc<U64Node<V, PREFIX>>,
    term_count: usize,
) -> Result<()> {
    ensure_parent(path)?;

    let builder = U64CxSnapshotBuilder::default();
    let root_ptr = builder.serialize_compressed_loop(root, None)?;
    let nodes = builder.into_nodes();

    let mut bytes = Vec::new();
    write_bytes(&mut bytes, &SNAPSHOT_MAGIC);
    write_u32(&mut bytes, SNAPSHOT_VERSION);
    write_u32(&mut bytes, PREFIX as u32);
    write_u64(&mut bytes, term_count as u64);
    write_u64(&mut bytes, root_ptr.to_raw());
    write_u64(&mut bytes, nodes.len() as u64);

    for node in nodes {
        let mut flags = 0u8;
        if node.is_final {
            flags |= 0b0000_0001;
        }
        if node.value.is_some() {
            flags |= 0b0000_0010;
        }
        write_u8(&mut bytes, flags);
        write_u32(&mut bytes, node.prefix.len() as u32);
        for unit in node.prefix {
            write_u64(&mut bytes, unit);
        }
        match node.value {
            Some(value) => {
                write_u64(&mut bytes, value.len() as u64);
                write_bytes(&mut bytes, &value);
            }
            None => write_u64(&mut bytes, NONE_VALUE_LEN),
        }
        write_u32(&mut bytes, node.children.len() as u32);
        for (label, raw_ptr) in node.children {
            write_u64(&mut bytes, label);
            write_u64(&mut bytes, raw_ptr);
        }
    }

    let tmp = tmp_snapshot_path(path);
    {
        let mut file =
            File::create(&tmp).map_err(|error| io_error("create u64 checkpoint", &tmp, error))?;
        file.write_all(&bytes)
            .map_err(|error| io_error("write u64 checkpoint", &tmp, error))?;
        file.sync_all()
            .map_err(|error| io_error("sync u64 checkpoint", &tmp, error))?;
    }
    fs::rename(&tmp, path).map_err(|error| io_error("install u64 checkpoint", path, error))
}

fn read_snapshot_file<V: DictionaryValue, const PREFIX: usize>(
    path: &Path,
) -> Result<(Arc<U64Node<V, PREFIX>>, usize)> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|error| io_error("open u64 checkpoint", path, error))?
        .read_to_end(&mut bytes)
        .map_err(|error| io_error("read u64 checkpoint", path, error))?;

    let mut cursor = Cursor::new(&bytes);
    let magic = cursor.take(8)?;
    if magic != SNAPSHOT_MAGIC {
        let mut found = [0u8; 8];
        found.copy_from_slice(magic);
        return Err(PersistentARTrieError::InvalidMagic {
            expected: u64::from_le_bytes(SNAPSHOT_MAGIC),
            found: u64::from_le_bytes(found),
        });
    }
    let version = cursor.u32()?;
    if version > SNAPSHOT_VERSION {
        return Err(PersistentARTrieError::UnsupportedVersion {
            max_supported: SNAPSHOT_VERSION,
            found: version,
        });
    }
    let prefix = cursor.u32()? as usize;
    if prefix != PREFIX {
        return Err(PersistentARTrieError::corrupted(format!(
            "u64 checkpoint prefix budget mismatch: file={prefix}, type={PREFIX}"
        )));
    }
    let term_count = cursor.u64()? as usize;
    let root_raw = cursor.u64()?;
    let node_count = cursor.u64()?;
    if node_count > MAX_NODE_COUNT {
        return Err(PersistentARTrieError::corrupted(format!(
            "u64 checkpoint node count {node_count} exceeds maximum {MAX_NODE_COUNT}"
        )));
    }

    let mut nodes = Vec::with_capacity(node_count as usize);
    for _ in 0..node_count {
        let flags = cursor.u8()?;
        let prefix_len = cursor.u32()?;
        if prefix_len > MAX_PREFIX_UNITS {
            return Err(PersistentARTrieError::corrupted(format!(
                "u64 checkpoint prefix length {prefix_len} exceeds maximum {MAX_PREFIX_UNITS}"
            )));
        }
        let mut prefix = Vec::with_capacity(prefix_len as usize);
        for _ in 0..prefix_len {
            prefix.push(cursor.u64()?);
        }
        let value_len = cursor.u64()?;
        let value = if value_len == NONE_VALUE_LEN {
            None
        } else {
            if value_len > MAX_VALUE_BYTES {
                return Err(PersistentARTrieError::corrupted(format!(
                    "u64 checkpoint value length {value_len} exceeds maximum {MAX_VALUE_BYTES}"
                )));
            }
            Some(cursor.take(value_len as usize)?.to_vec())
        };
        if flags & 0b0000_0010 != 0 && value.is_none() {
            return Err(PersistentARTrieError::corrupted(
                "u64 checkpoint value flag set without value bytes",
            ));
        }
        let child_count = cursor.u32()?;
        if child_count > MAX_CHILDREN_PER_NODE {
            return Err(PersistentARTrieError::corrupted(format!(
                "u64 checkpoint child count {child_count} exceeds maximum {MAX_CHILDREN_PER_NODE}"
            )));
        }
        let mut children = Vec::with_capacity(child_count as usize);
        for _ in 0..child_count {
            children.push((cursor.u64()?, cursor.u64()?));
        }
        nodes.push(U64DiskNode {
            is_final: flags & 0b0000_0001 != 0,
            prefix,
            value,
            children,
        });
    }
    cursor.finish()?;

    let mut memo: Vec<Option<Arc<U64Node<V, PREFIX>>>> = vec![None; nodes.len()];
    let root = build_overlay_from_disk::<V, PREFIX>(root_raw, &nodes, &mut memo)?;
    Ok((root, term_count))
}

fn ptr_index(raw: u64, node_count: usize) -> Result<usize> {
    let ptr = SwizzledPtr::from_raw(raw);
    let loc = ptr.disk_location().ok_or_else(|| {
        PersistentARTrieError::corrupted("u64 checkpoint contains null or memory pointer")
    })?;
    let index = loc.offset as usize;
    if index >= node_count {
        return Err(PersistentARTrieError::corrupted(format!(
            "u64 checkpoint child pointer index {index} out of {node_count}"
        )));
    }
    Ok(index)
}

fn build_overlay_from_disk<V: DictionaryValue, const PREFIX: usize>(
    raw: u64,
    nodes: &[U64DiskNode],
    memo: &mut [Option<Arc<U64Node<V, PREFIX>>>],
) -> Result<Arc<U64Node<V, PREFIX>>> {
    let index = ptr_index(raw, nodes.len())?;
    if let Some(node) = &memo[index] {
        return Ok(Arc::clone(node));
    }

    let disk = nodes[index].clone();
    let mut node = U64Node::<V, PREFIX>::new();
    if disk.is_final {
        node = node.as_final();
    }
    if let Some(value_bytes) = disk.value {
        let value: V = bincode_compat::deserialize(&value_bytes)
            .map_err(|error| codec_error("deserialize u64 checkpoint value", error))?;
        node = node.with_value(value);
    }
    for (label, child_raw) in disk.children {
        let child = build_overlay_from_disk::<V, PREFIX>(child_raw, nodes, memo)?;
        node = node.with_child(label, Child::InMem(child));
    }
    let mut current = Arc::new(node);
    for unit in disk.prefix.into_iter().rev() {
        let wrapper = U64Node::<V, PREFIX>::new().with_child(unit, Child::InMem(current));
        current = Arc::new(wrapper);
    }
    memo[index] = Some(Arc::clone(&current));
    Ok(current)
}

fn create_wal(path: &Path) -> Result<Arc<WalWriter>> {
    let wal = wal_path(path);
    ensure_parent(&wal)?;
    if wal.exists() {
        fs::remove_file(&wal).map_err(|error| io_error("remove existing u64 WAL", &wal, error))?;
    }
    let writer =
        WalWriter::create(&wal).map_err(|error| wal_error("create u64 shared WAL", error))?;
    writer
        .set_overlay_regime()
        .map_err(|error| wal_error("stamp u64 WAL overlay regime", error))?;
    Ok(Arc::new(writer))
}

fn open_wal(path: &Path) -> Result<Arc<WalWriter>> {
    let wal = wal_path(path);
    WalWriter::open_or_create(&wal)
        .map(Arc::new)
        .map_err(|error| wal_error("open u64 shared WAL", error))
}

fn append_and_sync(wal_writer: &WalWriter, record: WalRecord) -> Result<Lsn> {
    let lsn = wal_writer
        .append(record)
        .map_err(|error| wal_error("append u64 shared WAL", error))?;
    wal_writer
        .sync()
        .map_err(|error| wal_error("sync u64 shared WAL", error))?;
    Ok(lsn)
}

fn count_overlay_finals<V: DictionaryValue, const PREFIX: usize>(
    root: &Arc<U64Node<V, PREFIX>>,
) -> usize {
    let mut count = 0usize;
    let mut stack = vec![Arc::clone(root)];
    while let Some(node) = stack.pop() {
        if node.is_final() {
            count += 1;
        }
        for (_, child) in node.iter_children() {
            if let Some(child) = child.as_in_mem() {
                stack.push(Arc::clone(child));
            }
        }
    }
    count
}

fn collect_sequences<V: DictionaryValue, const PREFIX: usize>(
    root: Arc<U64Node<V, PREFIX>>,
) -> Vec<(Vec<u64>, Option<V>)> {
    let mut out = Vec::new();
    let mut stack = vec![(root, Vec::<u64>::new())];
    while let Some((node, path)) = stack.pop() {
        if node.is_final() {
            out.push((path.clone(), node.get_value()));
        }
        let mut children = Vec::new();
        for (&label, child) in node.iter_children() {
            if let Some(child) = child.as_in_mem() {
                children.push((label, Arc::clone(child)));
            }
        }
        children.reverse();
        for (label, child) in children {
            let mut child_path = path.clone();
            child_path.push(label);
            stack.push((child, child_path));
        }
    }
    out.sort_by(|left, right| left.0.cmp(&right.0));
    out
}

struct U64ReplayPlan {
    operations: Vec<RecoveredOperation>,
    max_lsn: Lsn,
    commit_seq_seed: u64,
}

fn read_replay_plan(wal_writer: &WalWriter, path: &Path) -> Result<U64ReplayPlan> {
    let wal = wal_path(path);
    if !wal.exists() {
        return Ok(U64ReplayPlan {
            operations: Vec::new(),
            max_lsn: 0,
            commit_seq_seed: wal_writer.commit_seq_floor(),
        });
    }

    let checkpoint_lsn = wal_writer.checkpoint_lsn();
    let rank_regime = wal_writer.rank_regime();
    let mut max_lsn = 0u64;
    let mut max_commit_generation = wal_writer.commit_seq_floor();
    let mut records = Vec::new();
    let mut reader =
        WalReader::new(&wal).map_err(|error| wal_error("open u64 shared WAL reader", error))?;
    while let Some(record) = reader.next_record() {
        let (lsn, record) =
            record.map_err(|error| wal_error("read u64 shared WAL record", error))?;
        max_lsn = max_lsn.max(lsn);
        if let WalRecord::CommitRank { generation, .. } = &record {
            max_commit_generation = max_commit_generation.max(*generation);
        }
        records.push((lsn, record));
    }

    let operations = reconcile_lww(records, true, checkpoint_lsn, rank_regime);
    Ok(U64ReplayPlan {
        operations,
        max_lsn,
        commit_seq_seed: max_commit_generation,
    })
}

impl<V: DictionaryValue, const PREFIX: usize> PersistentARTrieU64<V, MmapDiskManager, PREFIX> {
    pub fn create<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let wal_writer = create_wal(&path)?;
        let root = Arc::new(U64Node::<V, PREFIX>::new());
        write_snapshot_file::<V, PREFIX>(&path, &root, 0)?;
        Ok(Self {
            root: AtomicNodePtr::new(root),
            term_count: AtomicUsize::new(0),
            path: Some(path),
            wal_writer: Some(wal_writer),
            committed_watermark: CommittedWatermark::new(0),
            commit_seq: AtomicU64::new(0),
            checkpoint_lock: Arc::new(Mutex::new(())),
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
                "u64 shared WAL replay".to_string(),
                records_replayed,
                trie.term_count() as u64,
                Vec::new(),
                start.elapsed().as_millis() as u64,
            );
        }
        Ok((trie, report))
    }

    fn open_loaded(path: &Path) -> Result<(Self, u64)> {
        let (root, term_count) = read_snapshot_file::<V, PREFIX>(path)?;
        let wal_writer = open_wal(path)?;
        let replay_plan = read_replay_plan(&wal_writer, path)?;
        let trie = Self {
            root: AtomicNodePtr::new(root),
            term_count: AtomicUsize::new(term_count),
            path: Some(path.to_path_buf()),
            wal_writer: Some(wal_writer),
            committed_watermark: CommittedWatermark::new(replay_plan.max_lsn),
            commit_seq: AtomicU64::new(replay_plan.commit_seq_seed),
            checkpoint_lock: Arc::new(Mutex::new(())),
            _storage: PhantomData,
        };
        let records_replayed = trie.apply_replay_plan(replay_plan)?;
        Ok((trie, records_replayed))
    }
}

impl<V: DictionaryValue, S: BlockStorage, const PREFIX: usize> PersistentARTrieU64<V, S, PREFIX> {
    /// Create an in-memory persistent u64 trie.
    pub fn new() -> Self {
        Self {
            root: AtomicNodePtr::new(Arc::new(U64Node::<V, PREFIX>::new())),
            term_count: AtomicUsize::new(0),
            path: None,
            wal_writer: None,
            committed_watermark: CommittedWatermark::new(0),
            commit_seq: AtomicU64::new(0),
            checkpoint_lock: Arc::new(Mutex::new(())),
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

    pub fn storage_path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn root_arc(&self) -> Arc<U64Node<V, PREFIX>> {
        self.root
            .load()
            .unwrap_or_else(|| Arc::new(U64Node::<V, PREFIX>::new()))
    }

    fn find_node(&self, sequence: &[u64]) -> Option<Arc<U64Node<V, PREFIX>>> {
        let mut current = self.root.load()?;
        for &label in sequence {
            let child = current.find_child(label)?;
            current = Arc::clone(child.as_in_mem()?);
        }
        Some(current)
    }

    fn build_spine(sequence: &[u64], index: usize, value: Option<V>) -> Arc<U64Node<V, PREFIX>> {
        if index == sequence.len() {
            let mut node = U64Node::<V, PREFIX>::new().as_final();
            if let Some(value) = value {
                node = node.with_value(value);
            }
            return Arc::new(node);
        }
        let child = Self::build_spine(sequence, index + 1, value);
        Arc::new(U64Node::<V, PREFIX>::new().with_child(sequence[index], Child::InMem(child)))
    }

    fn build_insert_path(
        node: &Arc<U64Node<V, PREFIX>>,
        sequence: &[u64],
        index: usize,
        value: Option<V>,
    ) -> Option<(Arc<U64Node<V, PREFIX>>, bool)> {
        if index == sequence.len() {
            let inserted = !node.is_final();
            if !inserted && value.is_none() {
                return None;
            }
            let mut next = node.as_ref().clone().as_final();
            if let Some(value) = value {
                next = next.with_value(value);
            }
            return Some((Arc::new(next), inserted));
        }

        let label = sequence[index];
        let (child, inserted) = match node.find_child(label).and_then(|child| child.as_in_mem()) {
            Some(child) => Self::build_insert_path(child, sequence, index + 1, value)?,
            None => (Self::build_spine(sequence, index + 1, value), true),
        };
        Some((
            Arc::new(node.as_ref().clone().with_child(label, Child::InMem(child))),
            inserted,
        ))
    }

    fn insert_sequence_cas(&self, sequence: &[u64], value: Option<V>) -> bool {
        loop {
            let root = self.root_arc();
            let Some((new_root, inserted)) =
                Self::build_insert_path(&root, sequence, 0, value.clone())
            else {
                return false;
            };
            match self.root.compare_exchange(&root, new_root) {
                Ok(_) => {
                    if inserted {
                        self.term_count.fetch_add(1, Ordering::AcqRel);
                    }
                    return inserted;
                }
                Err(_) => continue,
            }
        }
    }

    fn insert_sequence_cas_ranked(&self, sequence: &[u64], value: Option<V>) -> U64CasOutcome {
        loop {
            let generation = self.commit_seq.fetch_add(1, Ordering::AcqRel) + 1;
            let root = self.root_arc();
            let Some((new_root, inserted)) =
                Self::build_insert_path(&root, sequence, 0, value.clone())
            else {
                return U64CasOutcome::Idempotent;
            };
            match self.root.compare_exchange(&root, new_root) {
                Ok(_) => {
                    if inserted {
                        self.term_count.fetch_add(1, Ordering::AcqRel);
                    }
                    return U64CasOutcome::Published {
                        inserted,
                        generation,
                    };
                }
                Err(_) => continue,
            }
        }
    }

    fn build_remove_path(
        node: &Arc<U64Node<V, PREFIX>>,
        sequence: &[u64],
        index: usize,
    ) -> Option<(Arc<U64Node<V, PREFIX>>, bool)> {
        if index == sequence.len() {
            if !node.is_final() {
                return None;
            }
            return Some((Arc::new(node.as_ref().clone().as_non_final()), true));
        }
        let label = sequence[index];
        let child = node.find_child(label)?.as_in_mem()?;
        let (new_child, removed) = Self::build_remove_path(child, sequence, index + 1)?;
        Some((
            Arc::new(
                node.as_ref()
                    .clone()
                    .with_child(label, Child::InMem(new_child)),
            ),
            removed,
        ))
    }

    fn remove_sequence_cas(&self, sequence: &[u64]) -> bool {
        loop {
            let root = self.root_arc();
            let Some((new_root, removed)) = Self::build_remove_path(&root, sequence, 0) else {
                return false;
            };
            match self.root.compare_exchange(&root, new_root) {
                Ok(_) => {
                    if removed {
                        self.term_count.fetch_sub(1, Ordering::AcqRel);
                    }
                    return removed;
                }
                Err(_) => continue,
            }
        }
    }

    fn remove_sequence_cas_ranked(&self, sequence: &[u64]) -> U64CasOutcome {
        loop {
            let generation = self.commit_seq.fetch_add(1, Ordering::AcqRel) + 1;
            let root = self.root_arc();
            let Some((new_root, removed)) = Self::build_remove_path(&root, sequence, 0) else {
                return U64CasOutcome::Idempotent;
            };
            match self.root.compare_exchange(&root, new_root) {
                Ok(_) => {
                    if removed {
                        self.term_count.fetch_sub(1, Ordering::AcqRel);
                    }
                    return U64CasOutcome::Published {
                        inserted: false,
                        generation,
                    };
                }
                Err(_) => continue,
            }
        }
    }

    fn commit_rank_and_mark(&self, data_lsn: Lsn, term: &[u8], generation: u64) -> Result<()> {
        let Some(wal_writer) = &self.wal_writer else {
            return Ok(());
        };
        let rank_lsn = append_and_sync(
            wal_writer,
            WalRecord::CommitRank {
                data_lsn,
                term: term.to_vec(),
                generation,
            },
        )?;
        self.committed_watermark.mark_committed(data_lsn);
        self.committed_watermark.mark_committed(rank_lsn);
        Ok(())
    }

    pub fn try_insert_sequence(&self, sequence: &[u64]) -> Result<bool> {
        if self.contains_sequence(sequence) {
            return Ok(false);
        }
        let term = encode_sequence(sequence);
        if let Some(wal_writer) = &self.wal_writer {
            let data_lsn = append_and_sync(
                wal_writer,
                WalRecord::Insert {
                    term: term.clone(),
                    value: None,
                },
            )?;
            return match self.insert_sequence_cas_ranked(sequence, None) {
                U64CasOutcome::Published {
                    inserted,
                    generation,
                } => {
                    self.commit_rank_and_mark(data_lsn, &term, generation)?;
                    Ok(inserted)
                }
                U64CasOutcome::Idempotent => {
                    self.committed_watermark.mark_committed(data_lsn);
                    Ok(false)
                }
            };
        }
        Ok(self.insert_sequence_cas(sequence, None))
    }

    pub fn insert_sequence(&self, sequence: &[u64]) -> bool {
        self.try_insert_sequence(sequence).unwrap_or_else(|error| {
            log::warn!("PersistentARTrieU64::insert_sequence failed: {error}");
            false
        })
    }

    pub fn try_insert_sequence_with_value(&self, sequence: &[u64], value: V) -> Result<bool> {
        let term = encode_sequence(sequence);
        if let Some(wal_writer) = &self.wal_writer {
            let value_bytes = bincode_compat::serialize(&value).map_err(|error| {
                PersistentARTrieError::internal(format!("serialize u64 WAL value: {error}"))
            })?;
            let data_lsn = append_and_sync(
                wal_writer,
                WalRecord::Upsert {
                    term: term.clone(),
                    value: value_bytes,
                },
            )?;
            return match self.insert_sequence_cas_ranked(sequence, Some(value)) {
                U64CasOutcome::Published {
                    inserted,
                    generation,
                } => {
                    self.commit_rank_and_mark(data_lsn, &term, generation)?;
                    Ok(inserted)
                }
                U64CasOutcome::Idempotent => {
                    self.committed_watermark.mark_committed(data_lsn);
                    Ok(false)
                }
            };
        }
        Ok(self.insert_sequence_cas(sequence, Some(value)))
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
        if let Some(mut value) = self.get_sequence_value(sequence) {
            update_fn(&mut value);
            let _ = self.insert_sequence_with_value(sequence, value);
            false
        } else {
            self.insert_sequence_with_value(sequence, default_value)
        }
    }

    pub fn contains_sequence(&self, sequence: &[u64]) -> bool {
        self.find_node(sequence).is_some_and(|node| node.is_final())
    }

    pub fn get_sequence_value(&self, sequence: &[u64]) -> Option<V> {
        let node = self.find_node(sequence)?;
        if node.is_final() {
            node.get_value()
        } else {
            None
        }
    }

    pub fn try_remove_sequence(&self, sequence: &[u64]) -> Result<bool> {
        if !self.contains_sequence(sequence) {
            return Ok(false);
        }
        let term = encode_sequence(sequence);
        if let Some(wal_writer) = &self.wal_writer {
            let data_lsn = append_and_sync(wal_writer, WalRecord::Remove { term: term.clone() })?;
            return match self.remove_sequence_cas_ranked(sequence) {
                U64CasOutcome::Published { generation, .. } => {
                    self.commit_rank_and_mark(data_lsn, &term, generation)?;
                    Ok(true)
                }
                U64CasOutcome::Idempotent => {
                    self.committed_watermark.mark_committed(data_lsn);
                    Ok(false)
                }
            };
        }
        Ok(self.remove_sequence_cas(sequence))
    }

    pub fn remove_sequence(&self, sequence: &[u64]) -> bool {
        self.try_remove_sequence(sequence).unwrap_or_else(|error| {
            log::warn!("PersistentARTrieU64::remove_sequence failed: {error}");
            false
        })
    }

    pub fn term_count(&self) -> usize {
        self.term_count.load(Ordering::Acquire)
    }

    pub fn iter_sequences(&self) -> impl Iterator<Item = Vec<u64>> + '_ {
        collect_sequences(self.root_arc())
            .into_iter()
            .map(|(sequence, _)| sequence)
    }

    pub fn iter_sequences_with_values(&self) -> impl Iterator<Item = (Vec<u64>, Option<V>)> + '_ {
        collect_sequences(self.root_arc()).into_iter()
    }

    pub fn iter_sequence_prefix(&self, prefix: &[u64]) -> Box<dyn Iterator<Item = Vec<u64>> + '_> {
        let prefix = prefix.to_vec();
        Box::new(
            self.iter_sequences()
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
        let _guard = self
            .checkpoint_lock
            .lock()
            .expect("u64 checkpoint mutex poisoned");
        let checkpoint_lsn = self.committed_watermark.watermark();
        let synced_frontier = self
            .wal_writer
            .as_ref()
            .map(|writer| writer.synced_lsn())
            .unwrap_or(0);
        assert!(
            checkpoint_lsn <= synced_frontier,
            "PersistentARTrieU64 checkpoint watermark {checkpoint_lsn} exceeds synced WAL frontier \
             {synced_frontier}"
        );
        let commit_seq_at_capture = self.commit_seq.load(Ordering::Acquire);
        let root = self.root_arc();
        let term_count = count_overlay_finals(&root);
        write_snapshot_file::<V, PREFIX>(path, &root, term_count)?;
        if let Some(wal_writer) = &self.wal_writer {
            let checkpoint_record_lsn = wal_writer
                .checkpoint(checkpoint_lsn)
                .map_err(|error| wal_error("checkpoint u64 shared WAL", error))?;
            self.committed_watermark
                .mark_committed(checkpoint_record_lsn);
            wal_writer
                .set_commit_seq_floor(commit_seq_at_capture)
                .map_err(|error| wal_error("set u64 WAL commit sequence floor", error))?;
        }
        Ok(())
    }

    pub fn close(&self) {
        if let Err(error) = self.checkpoint() {
            log::warn!("PersistentARTrieU64::close checkpoint failed: {error}");
        }
    }

    fn apply_replay_plan(&self, replay_plan: U64ReplayPlan) -> Result<u64> {
        let mut replayed = 0u64;
        for operation in replay_plan.operations {
            if self.apply_recovered_operation(operation)? {
                replayed += 1;
            }
        }
        Ok(replayed)
    }

    fn apply_recovered_operation(&self, operation: RecoveredOperation) -> Result<bool> {
        match operation {
            RecoveredOperation::Insert { term, value, .. } => {
                let Some(sequence) = decode_sequence(&term) else {
                    return Ok(false);
                };
                match value {
                    Some(bytes) => {
                        let value = bincode_compat::deserialize::<V>(&bytes).map_err(|error| {
                            codec_error("deserialize u64 WAL insert value", error)
                        })?;
                        self.insert_sequence_cas(&sequence, Some(value));
                    }
                    None => {
                        self.insert_sequence_cas(&sequence, None);
                    }
                }
                Ok(true)
            }
            RecoveredOperation::Upsert { term, value, .. } => {
                let Some(sequence) = decode_sequence(&term) else {
                    return Ok(false);
                };
                let value = bincode_compat::deserialize::<V>(&value)
                    .map_err(|error| codec_error("deserialize u64 WAL upsert value", error))?;
                self.insert_sequence_cas(&sequence, Some(value));
                Ok(true)
            }
            RecoveredOperation::Remove { term, .. } => {
                let Some(sequence) = decode_sequence(&term) else {
                    return Ok(false);
                };
                self.remove_sequence_cas(&sequence);
                Ok(true)
            }
            RecoveredOperation::Increment { .. } | RecoveredOperation::CompareAndSwap { .. } => {
                Ok(false)
            }
        }
    }
}

impl<V: DictionaryValue, S: BlockStorage, const PREFIX: usize> Dictionary
    for PersistentARTrieU64<V, S, PREFIX>
{
    type Node = PersistentARTrieU64Node<V, PREFIX>;

    fn root(&self) -> Self::Node {
        PersistentARTrieU64Node::from_overlay_root(self.root_arc(), None)
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

impl<V: DictionaryValue, S: BlockStorage, const PREFIX: usize> MappedDictionary
    for PersistentARTrieU64<V, S, PREFIX>
{
    type Value = V;

    fn get_value(&self, term: &str) -> Option<Self::Value> {
        PersistentARTrieU64::get_value(self, term)
    }
}

impl<V: DictionaryValue, S: BlockStorage, const PREFIX: usize> MutableDictionary
    for PersistentARTrieU64<V, S, PREFIX>
{
    fn insert(&self, term: &str) -> bool {
        PersistentARTrieU64::insert(self, term)
    }

    fn remove(&self, term: &str) -> bool {
        PersistentARTrieU64::remove(self, term)
    }
}

impl<V: DictionaryValue, S: BlockStorage, const PREFIX: usize> MutableMappedDictionary
    for PersistentARTrieU64<V, S, PREFIX>
{
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

impl<V: DictionaryValue, S: BlockStorage, const PREFIX: usize> Default
    for PersistentARTrieU64<V, S, PREFIX>
{
    fn default() -> Self {
        Self::new()
    }
}

/// Byte-encoded u64 persistent trie kept as a current-branch encoded control.
///
/// Each public `u64` is encoded as eight little-endian `u8` transitions through
/// the established byte `PersistentARTrie`.
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
