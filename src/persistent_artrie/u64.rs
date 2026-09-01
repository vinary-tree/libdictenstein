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

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::core::adaptive_edge_store::SortedUniqueEntries;
use crate::persistent_artrie::core::committed_watermark::CommittedWatermark;
use crate::persistent_artrie::core::key_encoding::{KeyEncoding, U64Key};
use crate::persistent_artrie::core::overlay::atomic_ptr::{AtomicNodePtr, RootRevision};
use crate::persistent_artrie::core::overlay::compressed_serialize::{
    OverlayCompressedSerialize, OverlaySerializationBuild,
};
use crate::persistent_artrie::core::overlay::dict_node::OverlayDictionaryNode;
use crate::persistent_artrie::core::overlay::node::{Child, OverlayNode};
use crate::persistent_artrie::core::recovery::{reconcile_lww_with_regime, RecoveredOperation};
use crate::persistent_artrie::core::wal::{
    Lsn, RankRegime, WalConfig, WalReader, WalRecord, WalWriter,
};
use crate::persistent_artrie::disk_manager::MmapDiskManager;
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::persistent_artrie::swizzled_ptr::{NodeType, SwizzledPtr, MAX_OFFSET};
use crate::persistent_artrie::{PersistentARTrie, RecoveryReport};
use crate::serialization::bincode_compat;
use crate::value::DictionaryValue;
use crate::{
    CharUnit, Dictionary, MappedDictionary, MutableDictionary, MutableMappedDictionary,
    SyncStrategy,
};
use smallvec::SmallVec;

const SNAPSHOT_MAGIC: [u8; 8] = *b"AR64CX01";
const SNAPSHOT_VERSION: u32 = 1;
const SNAPSHOT_FLAG_IS_FINAL: u8 = 0b0000_0001;
const SNAPSHOT_FLAG_HAS_VALUE: u8 = 0b0000_0010;
const SNAPSHOT_KNOWN_FLAGS: u8 = SNAPSHOT_FLAG_IS_FINAL | SNAPSHOT_FLAG_HAS_VALUE;
const NONE_VALUE_LEN: u64 = u64::MAX;
const MAX_VALUE_BYTES: u64 = 64 * 1024 * 1024;
/// Maximum number of records whose zero-based indices fit the 22-bit disk
/// offset carried by [`SwizzledPtr`].
const MAX_NODE_COUNT: u64 = MAX_OFFSET as u64 + 1;
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

/// Boxed fallible iterator returned by native-u64 prefix traversal.
pub type PersistentARTrieU64TrySequenceIter<'a> = Box<dyn Iterator<Item = Result<Vec<u64>>> + 'a>;

/// Boxed fallible valued iterator returned by native-u64 prefix traversal.
pub type PersistentARTrieU64TryValuedSequenceIter<'a, V> =
    Box<dyn Iterator<Item = Result<(Vec<u64>, Option<V>)>> + 'a>;

struct U64Projected {
    is_final: bool,
    prefix: Vec<u64>,
    value: Option<Vec<u8>>,
    children: Vec<(u64, u64)>,
}

struct U64DiskNode {
    is_final: bool,
    prefix: Vec<u64>,
    value: Option<Vec<u8>>,
    children: Vec<(u64, u64)>,
}

/// A format-bounded, validated index into the native-u64 checkpoint table.
/// The AR64CX01 pointer field is 22 bits, so `u32` is both sufficient and
/// materially smaller than `usize` in traversal frames on 64-bit targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct U64NodeIndex(u32);

impl U64NodeIndex {
    #[inline]
    fn as_usize(self) -> usize {
        self.0 as usize
    }
}

/// Reader-side record after pointer canonicality, range, and child-label order
/// have been validated exactly once.
struct U64DecodedNode {
    is_final: bool,
    prefix: Vec<u64>,
    value: Option<Vec<u8>>,
    children: SortedUniqueEntries<u64, U64NodeIndex>,
}

#[derive(Clone, Copy)]
enum U64PointerRole {
    Root,
    Child {
        node: usize,
        ordinal: usize,
    },
    #[cfg(test)]
    Test,
}

impl fmt::Display for U64PointerRole {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Root => formatter.write_str("root"),
            Self::Child { node, ordinal } => write!(formatter, "node {node} child {ordinal}"),
            #[cfg(test)]
            Self::Test => formatter.write_str("test"),
        }
    }
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
    fn into_nodes(self) -> Result<Vec<U64DiskNode>> {
        self.nodes
            .into_inner()
            .map_err(|_| PersistentARTrieError::LockPoisoned {
                resource: "u64 snapshot builder".to_string(),
            })
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
        _registry_path: crate::persistent_artrie::core::eviction::RegistryPathId,
        _registry: Option<&mut crate::persistent_artrie::eviction::DiskLocationRegistry>,
    ) -> Result<SwizzledPtr> {
        let mut nodes = self
            .nodes
            .lock()
            .map_err(|_| PersistentARTrieError::LockPoisoned {
                resource: "u64 snapshot builder".to_string(),
            })?;
        let index = checkpoint_node_index(nodes.len())?;
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
}

fn checkpoint_node_index(node_count: usize) -> Result<u32> {
    let index = u64::try_from(node_count).map_err(|_| {
        PersistentARTrieError::corrupted("u64 CX checkpoint node count does not fit u64")
    })?;
    if index >= MAX_NODE_COUNT {
        return Err(PersistentARTrieError::corrupted(format!(
            "u64 CX checkpoint exceeds maximum node count {MAX_NODE_COUNT}"
        )));
    }
    u32::try_from(index).map_err(|_| {
        PersistentARTrieError::corrupted("u64 CX checkpoint node index does not fit u32")
    })
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

fn snapshot_u32_len(section: &str, length: usize, maximum: u32) -> Result<u32> {
    let encoded = u32::try_from(length).map_err(|_| {
        PersistentARTrieError::InvalidOperation(format!(
            "cannot encode u64 CX checkpoint: {section} length {length} does not fit u32"
        ))
    })?;
    if encoded > maximum {
        return Err(PersistentARTrieError::InvalidOperation(format!(
            "cannot encode u64 CX checkpoint: {section} length {encoded} exceeds format limit {maximum}"
        )));
    }
    Ok(encoded)
}

fn snapshot_u64_len(section: &str, length: usize, maximum: u64) -> Result<u64> {
    let encoded = u64::try_from(length).map_err(|_| {
        PersistentARTrieError::InvalidOperation(format!(
            "cannot encode u64 CX checkpoint: {section} length {length} does not fit u64"
        ))
    })?;
    if encoded > maximum {
        return Err(PersistentARTrieError::InvalidOperation(format!(
            "cannot encode u64 CX checkpoint: {section} length {encoded} exceeds format limit {maximum}"
        )));
    }
    Ok(encoded)
}

fn validate_snapshot_flags(flags: u8) -> Result<()> {
    let unknown = flags & !SNAPSHOT_KNOWN_FLAGS;
    if unknown != 0 {
        return Err(PersistentARTrieError::corrupted(format!(
            "u64 checkpoint contains unknown node flag bits 0b{unknown:08b}"
        )));
    }
    Ok(())
}

fn validate_snapshot_value_flag(flags: u8, value_present: bool) -> Result<()> {
    let flagged = flags & SNAPSHOT_FLAG_HAS_VALUE != 0;
    if flagged != value_present {
        return Err(PersistentARTrieError::corrupted(format!(
            "u64 checkpoint value flag disagrees with value bytes: flag={flagged}, bytes={value_present}"
        )));
    }
    Ok(())
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

    #[inline]
    fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
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
    let mut serialization = OverlaySerializationBuild::dag_disabled();
    let root_ptr = builder.serialize_compressed_loop(root, &mut serialization)?;
    let nodes = builder.into_nodes()?;

    let encoded_prefix = u32::try_from(PREFIX).map_err(|_| {
        PersistentARTrieError::InvalidOperation(format!(
            "cannot encode u64 CX checkpoint: prefix budget {PREFIX} does not fit u32"
        ))
    })?;
    let encoded_term_count = u64::try_from(term_count).map_err(|_| {
        PersistentARTrieError::InvalidOperation(format!(
            "cannot encode u64 CX checkpoint: term count {term_count} does not fit u64"
        ))
    })?;
    let encoded_node_count = snapshot_u64_len("node table", nodes.len(), MAX_NODE_COUNT)?;

    let mut bytes = Vec::new();
    write_bytes(&mut bytes, &SNAPSHOT_MAGIC);
    write_u32(&mut bytes, SNAPSHOT_VERSION);
    write_u32(&mut bytes, encoded_prefix);
    write_u64(&mut bytes, encoded_term_count);
    write_u64(&mut bytes, root_ptr.to_raw());
    write_u64(&mut bytes, encoded_node_count);

    for node in nodes {
        let mut flags = 0u8;
        if node.is_final {
            flags |= SNAPSHOT_FLAG_IS_FINAL;
        }
        if node.value.is_some() {
            flags |= SNAPSHOT_FLAG_HAS_VALUE;
        }
        write_u8(&mut bytes, flags);
        let prefix_len = snapshot_u32_len("node prefix", node.prefix.len(), MAX_PREFIX_UNITS)?;
        write_u32(&mut bytes, prefix_len);
        for unit in node.prefix {
            write_u64(&mut bytes, unit);
        }
        match node.value {
            Some(value) => {
                let value_len = snapshot_u64_len("node value", value.len(), MAX_VALUE_BYTES)?;
                write_u64(&mut bytes, value_len);
                write_bytes(&mut bytes, &value);
            }
            None => write_u64(&mut bytes, NONE_VALUE_LEN),
        }
        let child_count = snapshot_u32_len(
            "node child table",
            node.children.len(),
            MAX_CHILDREN_PER_NODE,
        )?;
        write_u32(&mut bytes, child_count);
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
    let term_count = cursor.u64()?;
    let root_raw = cursor.u64()?;
    let encoded_node_count = cursor.u64()?;
    if encoded_node_count > MAX_NODE_COUNT {
        return Err(PersistentARTrieError::corrupted(format!(
            "u64 checkpoint node count {encoded_node_count} exceeds maximum {MAX_NODE_COUNT}"
        )));
    }
    let node_count = usize::try_from(encoded_node_count).map_err(|_| {
        PersistentARTrieError::corrupted(format!(
            "u64 checkpoint node count {encoded_node_count} does not fit this target"
        ))
    })?;
    let term_count_usize = usize::try_from(term_count).map_err(|_| {
        PersistentARTrieError::corrupted(format!(
            "u64 checkpoint term count {term_count} does not fit this target"
        ))
    })?;
    let root_index = decode_node_index(root_raw, node_count, U64PointerRole::Root)?;

    // Every node needs flags, prefix length, value length, and child count even
    // when all variable sections are empty. Prove the minimum encoded extent
    // before reserving from an untrusted count so a tiny truncated image cannot
    // trigger a multi-million-element allocation.
    const MIN_NODE_BYTES: usize = 1 + 4 + 8 + 4;
    ensure_encoded_extent(cursor.remaining(), node_count, MIN_NODE_BYTES, "node table")?;

    let mut nodes = Vec::new();
    try_reserve_exact(&mut nodes, node_count, "u64 checkpoint node table")?;
    for node_index in 0..node_count {
        let flags = cursor.u8()?;
        validate_snapshot_flags(flags)?;
        let prefix_len = cursor.u32()?;
        if prefix_len > MAX_PREFIX_UNITS {
            return Err(PersistentARTrieError::corrupted(format!(
                "u64 checkpoint prefix length {prefix_len} exceeds maximum {MAX_PREFIX_UNITS}"
            )));
        }
        let prefix_len = prefix_len as usize;
        ensure_encoded_extent(cursor.remaining(), prefix_len, 8, "node prefix")?;
        let mut prefix = Vec::new();
        try_reserve_exact(&mut prefix, prefix_len, "u64 checkpoint node prefix")?;
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
            let value_len = usize::try_from(value_len).map_err(|_| {
                PersistentARTrieError::corrupted(
                    "u64 checkpoint value length does not fit this target",
                )
            })?;
            let encoded = cursor.take(value_len)?;
            let mut value = Vec::new();
            try_reserve_exact(&mut value, value_len, "u64 checkpoint value")?;
            value.extend_from_slice(encoded);
            Some(value)
        };
        validate_snapshot_value_flag(flags, value.is_some())?;
        let child_count = cursor.u32()?;
        if child_count > MAX_CHILDREN_PER_NODE {
            return Err(PersistentARTrieError::corrupted(format!(
                "u64 checkpoint child count {child_count} exceeds maximum {MAX_CHILDREN_PER_NODE}"
            )));
        }
        let child_count = child_count as usize;
        ensure_encoded_extent(cursor.remaining(), child_count, 16, "child table")?;
        let mut children = Vec::new();
        try_reserve_exact(&mut children, child_count, "u64 checkpoint child table")?;
        for child_ordinal in 0..child_count {
            let label = cursor.u64()?;
            let child_raw = cursor.u64()?;
            let child_index = decode_node_index(
                child_raw,
                node_count,
                U64PointerRole::Child {
                    node: node_index,
                    ordinal: child_ordinal,
                },
            )?;
            children.push((label, child_index));
        }
        let children = SortedUniqueEntries::try_new(children).map_err(|bad_index| {
            PersistentARTrieError::corrupted(format!(
                "u64 checkpoint node {node_index} child labels are not strictly ascending and unique at position {bad_index}"
            ))
        })?;
        nodes.push(U64DecodedNode {
            is_final: flags & SNAPSHOT_FLAG_IS_FINAL != 0,
            prefix,
            value,
            children,
        });
    }
    cursor.finish()?;

    let root = build_overlay_from_disk::<V, PREFIX>(root_index, &nodes, term_count)?;
    Ok((root, term_count_usize))
}

fn ensure_encoded_extent(
    remaining: usize,
    count: usize,
    bytes_per_item: usize,
    section: &str,
) -> Result<()> {
    let required = count.checked_mul(bytes_per_item).ok_or_else(|| {
        PersistentARTrieError::corrupted(format!("u64 checkpoint {section} encoded-size overflow"))
    })?;
    if required > remaining {
        return Err(PersistentARTrieError::corrupted(format!(
            "truncated u64 checkpoint {section}: need at least {required} bytes, have {remaining}"
        )));
    }
    Ok(())
}

fn try_reserve_exact<T>(items: &mut Vec<T>, count: usize, section: &str) -> Result<()> {
    items
        .try_reserve_exact(count)
        .map_err(|error| PersistentARTrieError::allocation_failed(section, count, error))
}

fn decode_node_index(raw: u64, node_count: usize, role: U64PointerRole) -> Result<U64NodeIndex> {
    let ptr = SwizzledPtr::from_raw(raw);
    let loc = ptr.disk_location().ok_or_else(|| {
        PersistentARTrieError::corrupted(format!(
            "u64 checkpoint {role} contains a null, memory, or invalid disk pointer"
        ))
    })?;
    if loc.block_id != 0 || loc.node_type != NodeType::CharBucket {
        return Err(PersistentARTrieError::corrupted(format!(
            "u64 checkpoint {role} pointer is noncanonical: block={}, type={:?}",
            loc.block_id, loc.node_type
        )));
    }
    let canonical = SwizzledPtr::on_disk(0, loc.offset, NodeType::CharBucket).to_raw();
    if raw != canonical {
        return Err(PersistentARTrieError::corrupted(format!(
            "u64 checkpoint {role} pointer has noncanonical reserved flag bits"
        )));
    }
    let index = loc.offset as usize;
    if index >= node_count {
        return Err(PersistentARTrieError::corrupted(format!(
            "u64 checkpoint {role} pointer index {index} out of {node_count}"
        )));
    }
    Ok(U64NodeIndex(loc.offset))
}

fn build_overlay_from_disk<V: DictionaryValue, const PREFIX: usize>(
    root_index: U64NodeIndex,
    nodes: &[U64DecodedNode],
    expected_term_count: u64,
) -> Result<Arc<U64Node<V, PREFIX>>> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Visit {
        Unseen,
        Visiting,
        Done,
    }

    struct Frame {
        index: U64NodeIndex,
        next_child: u32,
    }

    // Phase 1 validates the complete root-reachable pointer graph before value
    // deserialization or Arc construction. Each frame is two u32 values rather
    // than two machine words or a topology-sized OverlayNode.
    let mut visits = Vec::new();
    try_reserve_exact(&mut visits, nodes.len(), "u64 checkpoint DFS colors")?;
    visits.resize(nodes.len(), Visit::Unseen);
    let mut postorder = Vec::new();
    try_reserve_exact(&mut postorder, nodes.len(), "u64 checkpoint DFS postorder")?;
    let mut preorder = Vec::new();
    try_reserve_exact(&mut preorder, nodes.len(), "u64 checkpoint DFS preorder")?;
    visits[root_index.as_usize()] = Visit::Visiting;
    preorder.push(root_index);
    let mut frames: SmallVec<[Frame; 32]> = SmallVec::new();
    frames.push(Frame {
        index: root_index,
        next_child: 0,
    });

    while let Some(frame) = frames.last_mut() {
        let node_index = frame.index.as_usize();
        let disk = &nodes[node_index];
        let children = disk.children.as_slice();
        if (frame.next_child as usize) < children.len() {
            let edge_index = frame.next_child as usize;
            let (_, child_index) = children[edge_index];
            frame.next_child += 1;
            match visits[child_index.as_usize()] {
                Visit::Unseen => {
                    visits[child_index.as_usize()] = Visit::Visiting;
                    preorder.push(child_index);
                    frames.push(Frame {
                        index: child_index,
                        next_child: 0,
                    });
                }
                Visit::Visiting => {
                    return Err(PersistentARTrieError::corrupted(format!(
                        "u64 checkpoint contains a cycle from node {} to node {child_index}",
                        frame.index.0,
                        child_index = child_index.0
                    )));
                }
                Visit::Done => {}
            }
            continue;
        }

        let completed_index = frame.index;
        frames.pop();
        visits[completed_index.as_usize()] = Visit::Done;
        postorder.push(completed_index);
    }

    // Count logical accepted paths over the validated DAG before invoking any
    // user-provided value deserializer. Shared nodes contribute once per incoming
    // path, which is the trie language cardinality rather than the record count.
    let mut logical_counts = Vec::new();
    try_reserve_exact(
        &mut logical_counts,
        nodes.len(),
        "u64 checkpoint logical term counts",
    )?;
    logical_counts.resize(nodes.len(), 0u64);
    for &index in &postorder {
        let disk = &nodes[index.as_usize()];
        let mut count = u64::from(disk.is_final);
        for &(_, child_index) in disk.children.as_slice() {
            count = count
                .checked_add(logical_counts[child_index.as_usize()])
                .ok_or_else(|| {
                    PersistentARTrieError::corrupted(
                        "u64 checkpoint logical term count overflows u64",
                    )
                })?;
        }
        logical_counts[index.as_usize()] = count;
    }
    let computed_term_count = logical_counts[root_index.as_usize()];
    if computed_term_count != expected_term_count {
        return Err(PersistentARTrieError::corrupted(format!(
            "u64 checkpoint term count mismatch: header={expected_term_count}, graph={computed_term_count}"
        )));
    }

    // Preserve the recursive reader's parent-before-children value decode and
    // error order while retaining the stronger all-structure-before-user-code
    // boundary. Values are held in a flat vector until postorder construction.
    let mut values = Vec::new();
    try_reserve_exact(&mut values, nodes.len(), "u64 checkpoint decoded values")?;
    values.resize_with(nodes.len(), || None);
    for &index in &preorder {
        if let Some(value_bytes) = &nodes[index.as_usize()].value {
            let value = bincode_compat::deserialize(value_bytes)
                .map_err(|error| codec_error("deserialize u64 checkpoint value", error))?;
            values[index.as_usize()] = Some(value);
        }
    }

    // Phase 2 constructs each reachable node once, in child-before-parent order.
    // `from_sorted_children` chooses the adaptive tier in O(degree) instead of
    // performing O(degree^2) copy-on-write insertions on wide nodes.
    let mut memo = Vec::new();
    try_reserve_exact(
        &mut memo,
        nodes.len(),
        "u64 checkpoint materialization memo",
    )?;
    memo.resize_with(nodes.len(), || None);
    for index in postorder {
        let node_index = index.as_usize();
        let disk = &nodes[node_index];
        let children = disk.children.try_map(
            |child_index| {
                let child = memo[child_index.as_usize()].as_ref().ok_or_else(|| {
                    PersistentARTrieError::corrupted(format!(
                        "u64 checkpoint materializer lost completed node {}",
                        child_index.0
                    ))
                })?;
                Ok(Child::InMem(Arc::clone(child)))
            },
            |error| {
                PersistentARTrieError::allocation_failed(
                    "u64 checkpoint materialized children",
                    disk.children.as_slice().len(),
                    error,
                )
            },
        )?;
        let node = U64Node::<V, PREFIX>::try_from_sorted_children(
            disk.is_final,
            values[node_index].take(),
            children,
        )
        .map_err(|error| {
            PersistentARTrieError::allocation_failed(
                format!("u64 checkpoint node {node_index} sparse child index"),
                disk.children.as_slice().len(),
                error,
            )
        })?;
        let mut completed = Arc::new(node);
        for &unit in disk.prefix.iter().rev() {
            completed =
                Arc::new(U64Node::<V, PREFIX>::new().with_child(unit, Child::InMem(completed)));
        }
        memo[node_index] = Some(completed);
    }

    memo[root_index.as_usize()]
        .as_ref()
        .map(Arc::clone)
        .ok_or_else(|| {
            PersistentARTrieError::corrupted(
                "u64 checkpoint materializer did not construct the root",
            )
        })
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
    let writer =
        WalWriter::open_or_create(&wal).map_err(|error| wal_error("open u64 shared WAL", error))?;
    if writer.records_empty_on_disk() {
        writer
            .set_overlay_regime_records_empty()
            .map_err(|error| wal_error("stamp empty u64 WAL overlay regime", error))?;
    }
    Ok(Arc::new(writer))
}

fn append_and_sync(wal_writer: &WalWriter, record: WalRecord) -> Result<Lsn> {
    let lsn = wal_writer
        .append_record_segment(record)
        .map_err(|error| wal_error("append u64 shared WAL", error))?;
    wal_writer
        .sync_record_segments()
        .map_err(|error| wal_error("sync u64 shared WAL", error))?;
    Ok(lsn)
}

struct U64SequenceFrame<V: DictionaryValue, const PREFIX: usize> {
    node: Arc<U64Node<V, PREFIX>>,
    next_child: usize,
    path_len: usize,
    emitted: bool,
}

struct U64SequenceIterator<V: DictionaryValue, const PREFIX: usize> {
    frames: SmallVec<[U64SequenceFrame<V, PREFIX>; 16]>,
    path: Vec<u64>,
}

fn nonresident_u64_edge(label: u64) -> PersistentARTrieError {
    PersistentARTrieError::corrupted(format!(
        "native-u64 topology contains unresolved on-disk edge with label {label}; native-u64 snapshots must be fully resident"
    ))
}

fn assume_resident_u64_topology<T>(result: Result<T>) -> T {
    result.unwrap_or_else(|error| {
        panic!("PersistentARTrieU64 resident-topology invariant violated: {error}")
    })
}

impl<V: DictionaryValue, const PREFIX: usize> U64SequenceIterator<V, PREFIX> {
    fn new(root: Arc<U64Node<V, PREFIX>>, path: Vec<u64>) -> Self {
        let path_len = path.len();
        let mut frames = SmallVec::new();
        frames.push(U64SequenceFrame {
            node: root,
            next_child: 0,
            path_len,
            emitted: false,
        });
        Self { frames, path }
    }
}

impl<V: DictionaryValue, const PREFIX: usize> Iterator for U64SequenceIterator<V, PREFIX> {
    type Item = Result<(Vec<u64>, Option<V>)>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            // Native-u64 checkpoint reopen materializes every reachable edge.  Keep
            // that cross-module invariant executable: an unresolved edge is a typed,
            // terminal corruption result, never a silently omitted language branch.
            let action = {
                let frame = self.frames.last_mut()?;
                if !frame.emitted {
                    frame.emitted = true;
                    if frame.node.is_final() {
                        return Some(Ok((self.path.clone(), frame.node.get_value())));
                    }
                }

                match frame.node.child_at(frame.next_child) {
                    Some((&label, child)) => {
                        frame.next_child += 1;
                        match child {
                            Child::InMem(child) => {
                                Ok(Some((label, Arc::clone(child), frame.path_len)))
                            }
                            Child::OnDisk(_) => Err(nonresident_u64_edge(label)),
                        }
                    }
                    None => Ok(None),
                }
            };

            match action {
                Ok(Some((label, child, parent_path_len))) => {
                    self.path.truncate(parent_path_len);
                    self.path.push(label);
                    self.frames.push(U64SequenceFrame {
                        node: child,
                        next_child: 0,
                        path_len: self.path.len(),
                        emitted: false,
                    });
                }
                Ok(None) => {
                    self.frames.pop();
                    let parent_path_len =
                        self.frames.last().map(|frame| frame.path_len).unwrap_or(0);
                    self.path.truncate(parent_path_len);
                }
                Err(error) => {
                    self.frames.clear();
                    self.path.clear();
                    return Some(Err(error));
                }
            }
        }
    }
}

impl<V: DictionaryValue, const PREFIX: usize> std::iter::FusedIterator
    for U64SequenceIterator<V, PREFIX>
{
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
    let mut max_lsn = 0u64;
    let mut max_commit_generation = wal_writer.commit_seq_floor();
    let mut records = Vec::new();
    let mut regime_by_lsn = Vec::<(Lsn, RankRegime)>::new();
    let mut segments = wal_writer
        .collect_wal_segments(&WalConfig::default())
        .map_err(|error| wal_error("collect u64 shared WAL segments", error))?;
    if segments.is_empty() {
        segments.push(wal);
    }
    for segment in segments {
        let segment_regime = WalReader::read_header(&segment)
            .map(|header| header.regime())
            .unwrap_or_else(|_| wal_writer.rank_regime());
        let mut reader = WalReader::new(&segment)
            .map_err(|error| wal_error("open u64 shared WAL segment", error))?;
        while let Some(record) = reader.next_record() {
            let (lsn, record) =
                record.map_err(|error| wal_error("read u64 shared WAL record", error))?;
            max_lsn = max_lsn.max(lsn);
            regime_by_lsn.push((lsn, segment_regime));
            if let WalRecord::CommitRank { generation, .. } = &record {
                max_commit_generation = max_commit_generation.max(*generation);
            }
            records.push((lsn, record));
        }
    }

    regime_by_lsn.sort_by_key(|(lsn, _)| *lsn);
    let default_regime = wal_writer.rank_regime();
    let operations = reconcile_lww_with_regime(records, true, checkpoint_lsn, |lsn| {
        regime_by_lsn
            .binary_search_by_key(&lsn, |(record_lsn, _)| *record_lsn)
            .ok()
            .map(|index| regime_by_lsn[index].1)
            .unwrap_or(default_regime)
    });
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
            root: AtomicNodePtr::new_with_term_count(root, 0),
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
            root: AtomicNodePtr::new_with_term_count(root, term_count),
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
        Arc::clone(self.root_revision().node())
    }

    fn root_revision(&self) -> RootRevision<U64Key<PREFIX>, V> {
        loop {
            if let Some(revision) = self.root.load_revision() {
                return revision;
            }
            let _ = self.root.try_init(Arc::new(U64Node::<V, PREFIX>::new()));
        }
    }

    /// Capture a traversal root and exact cardinality from one atomic revision.
    pub(crate) fn root_with_term_count(&self) -> (PersistentARTrieU64Node<V, PREFIX>, usize) {
        let (root, term_count) = self
            .root
            .load_with_term_count()
            .unwrap_or_else(|| (Arc::new(U64Node::<V, PREFIX>::new()), 0));
        (
            PersistentARTrieU64Node::from_overlay_root(root, None),
            term_count,
        )
    }

    fn try_find_node(&self, sequence: &[u64]) -> Result<Option<Arc<U64Node<V, PREFIX>>>> {
        let Some(mut current) = self.root.load() else {
            return Ok(None);
        };
        for &label in sequence {
            let Some(child) = current.find_child(label) else {
                return Ok(None);
            };
            current = match child {
                Child::InMem(child) => Arc::clone(child),
                Child::OnDisk(_) => return Err(nonresident_u64_edge(label)),
            };
        }
        Ok(Some(current))
    }

    fn find_node(&self, sequence: &[u64]) -> Option<Arc<U64Node<V, PREFIX>>> {
        assume_resident_u64_topology(self.try_find_node(sequence))
    }

    fn build_insert_path(
        node: &Arc<U64Node<V, PREFIX>>,
        sequence: &[u64],
        index: usize,
        value: Option<V>,
    ) -> Option<(Arc<U64Node<V, PREFIX>>, bool)> {
        type Parent<'a, V, const PREFIX: usize> = &'a U64Node<V, PREFIX>;

        // The labels are already stored in `sequence[index..depth]`; retaining
        // them in every frame would double the zipper working set on 64-bit
        // targets. Sixteen borrowed parents fit inline, while arbitrary depth
        // spills to the heap without growing the native call stack.
        let mut spine: SmallVec<[Parent<'_, V, PREFIX>; 16]> = SmallVec::new();
        let mut current = node.as_ref();
        let mut depth = index;

        let unwind = |spine: SmallVec<[Parent<'_, V, PREFIX>; 16]>,
                      labels: &[u64],
                      mut child: Arc<U64Node<V, PREFIX>>| {
            debug_assert_eq!(spine.len(), labels.len());
            for (parent, &label) in spine.into_iter().rev().zip(labels.iter().rev()) {
                child = Arc::new(parent.with_child(label, Child::InMem(child)));
            }
            child
        };

        loop {
            if depth == sequence.len() {
                let inserted = !current.is_final();
                if !inserted && value.is_none() {
                    return None;
                }
                let mut terminal = current.as_final();
                if let Some(value) = value {
                    terminal = terminal.with_value(value);
                }
                return Some((
                    unwind(spine, &sequence[index..depth], Arc::new(terminal)),
                    inserted,
                ));
            }

            let label = sequence[depth];
            if let Some(child) = current.find_child(label).and_then(Child::as_in_mem) {
                spine.push(current);
                current = child.as_ref();
                depth += 1;
                continue;
            }

            let mut leaf = U64Node::<V, PREFIX>::new().as_final();
            if let Some(value) = value {
                leaf = leaf.with_value(value);
            }
            let mut child = Arc::new(leaf);
            for &unit in sequence[depth + 1..].iter().rev() {
                child = Arc::new(U64Node::<V, PREFIX>::new().with_child(unit, Child::InMem(child)));
            }
            let parent = Arc::new(current.with_child(label, Child::InMem(child)));
            return Some((unwind(spine, &sequence[index..depth], parent), true));
        }
    }

    fn insert_sequence_cas(&self, sequence: &[u64], value: Option<V>) -> bool {
        loop {
            let revision = self.root_revision();
            let root = revision.node();
            let Some((new_root, inserted)) =
                Self::build_insert_path(root, sequence, 0, value.clone())
            else {
                return false;
            };
            match self.root.compare_exchange_revision_counted(
                &revision,
                new_root,
                isize::from(inserted),
            ) {
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
            let revision = self.root_revision();
            let root = revision.node();
            let Some((new_root, inserted)) =
                Self::build_insert_path(root, sequence, 0, value.clone())
            else {
                return U64CasOutcome::Idempotent;
            };
            match self.root.compare_exchange_revision_counted(
                &revision,
                new_root,
                isize::from(inserted),
            ) {
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
        type Parent<'a, V, const PREFIX: usize> = &'a U64Node<V, PREFIX>;

        let mut spine: SmallVec<[Parent<'_, V, PREFIX>; 16]> = SmallVec::new();
        let mut current = node.as_ref();
        let mut depth = index;

        loop {
            if depth == sequence.len() {
                if !current.is_final() {
                    return None;
                }
                let mut child = Arc::new(current.as_non_final());
                debug_assert_eq!(spine.len(), depth - index);
                for (parent, &label) in spine
                    .into_iter()
                    .rev()
                    .zip(sequence[index..depth].iter().rev())
                {
                    child = Arc::new(parent.with_child(label, Child::InMem(child)));
                }
                return Some((child, true));
            }

            let label = sequence[depth];
            let child = current.find_child(label)?.as_in_mem()?;
            spine.push(current);
            current = child.as_ref();
            depth += 1;
        }
    }

    fn remove_sequence_cas(&self, sequence: &[u64]) -> bool {
        loop {
            let revision = self.root_revision();
            let root = revision.node();
            let Some((new_root, removed)) = Self::build_remove_path(root, sequence, 0) else {
                return false;
            };
            match self.root.compare_exchange_revision_counted(
                &revision,
                new_root,
                -isize::from(removed),
            ) {
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
            let revision = self.root_revision();
            let root = revision.node();
            let Some((new_root, removed)) = Self::build_remove_path(root, sequence, 0) else {
                return U64CasOutcome::Idempotent;
            };
            match self.root.compare_exchange_revision_counted(
                &revision,
                new_root,
                -isize::from(removed),
            ) {
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
        F: Fn(&mut V),
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
        self.root.term_count()
    }

    /// Fallible snapshot iterator over native-u64 sequences.
    ///
    /// Native-u64 construction and reopen produce fully resident topologies. If
    /// that invariant is violated internally, the iterator reports corruption
    /// instead of silently omitting an unresolved branch.
    pub fn try_iter_sequences(&self) -> impl Iterator<Item = Result<Vec<u64>>> + '_ {
        U64SequenceIterator::new(self.root_arc(), Vec::new())
            .map(|item| item.map(|(sequence, _)| sequence))
    }

    pub fn iter_sequences(&self) -> impl Iterator<Item = Vec<u64>> + '_ {
        self.try_iter_sequences().map(assume_resident_u64_topology)
    }

    /// Fallible valued counterpart to [`Self::iter_sequences_with_values`].
    pub fn try_iter_sequences_with_values(
        &self,
    ) -> impl Iterator<Item = Result<(Vec<u64>, Option<V>)>> + '_ {
        U64SequenceIterator::new(self.root_arc(), Vec::new())
    }

    pub fn iter_sequences_with_values(&self) -> impl Iterator<Item = (Vec<u64>, Option<V>)> + '_ {
        self.try_iter_sequences_with_values()
            .map(assume_resident_u64_topology)
    }

    /// Construct a fallible, prefix-local snapshot iterator.
    pub fn try_iter_sequence_prefix(
        &self,
        prefix: &[u64],
    ) -> Result<PersistentARTrieU64TrySequenceIter<'_>> {
        let Some(root) = self.try_find_node(prefix)? else {
            return Ok(Box::new(std::iter::empty()));
        };
        Ok(Box::new(
            U64SequenceIterator::new(root, prefix.to_vec())
                .map(|item| item.map(|(sequence, _)| sequence)),
        ))
    }

    pub fn iter_sequence_prefix(&self, prefix: &[u64]) -> Box<dyn Iterator<Item = Vec<u64>> + '_> {
        let iterator = assume_resident_u64_topology(self.try_iter_sequence_prefix(prefix));
        Box::new(iterator.map(assume_resident_u64_topology))
    }

    /// Construct a fallible valued, prefix-local snapshot iterator.
    pub fn try_iter_sequence_prefix_with_values(
        &self,
        prefix: &[u64],
    ) -> Result<PersistentARTrieU64TryValuedSequenceIter<'_, V>> {
        let Some(root) = self.try_find_node(prefix)? else {
            return Ok(Box::new(std::iter::empty()));
        };
        Ok(Box::new(U64SequenceIterator::new(root, prefix.to_vec())))
    }

    pub fn iter_sequence_prefix_with_values(
        &self,
        prefix: &[u64],
    ) -> Box<dyn Iterator<Item = (Vec<u64>, Option<V>)> + '_> {
        let iterator =
            assume_resident_u64_topology(self.try_iter_sequence_prefix_with_values(prefix));
        Box::new(iterator.map(assume_resident_u64_topology))
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
        let _guard =
            self.checkpoint_lock
                .lock()
                .map_err(|_| PersistentARTrieError::LockPoisoned {
                    resource: "u64 checkpoint".to_string(),
                })?;
        let checkpoint_lsn = self.committed_watermark.watermark();
        let synced_frontier = self
            .wal_writer
            .as_ref()
            .map(|writer| writer.synced_lsn())
            .unwrap_or(0);
        if checkpoint_lsn > synced_frontier {
            return Err(PersistentARTrieError::internal(format!(
                "PersistentARTrieU64 checkpoint watermark {checkpoint_lsn} exceeds synced WAL frontier {synced_frontier}"
            )));
        }
        let commit_seq_at_capture = self.commit_seq.load(Ordering::Acquire);
        let (root, term_count) = self.root.load_with_term_count().ok_or_else(|| {
            PersistentARTrieError::internal("persistent u64 root is not initialized")
        })?;
        write_snapshot_file::<V, PREFIX>(path, &root, term_count)?;
        if let Some(wal_writer) = &self.wal_writer {
            let checkpoint_record_lsn = wal_writer
                .checkpoint_record_segment(checkpoint_lsn)
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
        F: Fn(&mut Self::Value),
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    static ITERATOR_VALUE_CLONES: AtomicUsize = AtomicUsize::new(0);

    #[derive(Default, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct CloneObservedValue(u64);

    impl Clone for CloneObservedValue {
        fn clone(&self) -> Self {
            ITERATOR_VALUE_CLONES.fetch_add(1, Ordering::Relaxed);
            Self(self.0)
        }
    }

    impl DictionaryValue for CloneObservedValue {}

    fn disk_ptr(index: usize) -> u64 {
        SwizzledPtr::on_disk(
            0,
            u32::try_from(index).expect("test node index fits disk pointer"),
            NodeType::CharBucket,
        )
        .to_raw()
    }

    fn node_index(index: usize) -> U64NodeIndex {
        U64NodeIndex(u32::try_from(index).expect("test node index fits u32"))
    }

    fn decoded_node(children: Vec<(u64, U64NodeIndex)>, is_final: bool) -> U64DecodedNode {
        U64DecodedNode {
            is_final,
            prefix: Vec::new(),
            value: None,
            children: SortedUniqueEntries::try_new(children)
                .expect("test children are strictly ascending and unique"),
        }
    }

    #[test]
    fn disk_materializer_rejects_self_cycle() {
        let nodes = vec![decoded_node(vec![(7, node_index(0))], false)];

        let error = build_overlay_from_disk::<(), U64_CX_PREFIX_COMPACT>(node_index(0), &nodes, 0)
            .expect_err("self-cycle must be rejected");

        assert!(error.to_string().contains("cycle from node 0 to node 0"));
    }

    #[test]
    fn disk_materializer_rejects_deep_cycle_without_native_recursion() {
        const NODE_COUNT: usize = 100_000;

        let nodes = (0..NODE_COUNT)
            .map(|index| {
                let successor = if index + 1 == NODE_COUNT {
                    0
                } else {
                    index + 1
                };
                decoded_node(vec![(index as u64, node_index(successor))], false)
            })
            .collect::<Vec<_>>();

        let error = build_overlay_from_disk::<(), U64_CX_PREFIX_COMPACT>(node_index(0), &nodes, 0)
            .expect_err("deep cycle must be rejected");

        assert!(error
            .to_string()
            .contains("cycle from node 99999 to node 0"));
    }

    #[test]
    fn disk_materializer_preserves_shared_acyclic_child() {
        let nodes = vec![
            decoded_node(vec![(3, node_index(1)), (5, node_index(1))], false),
            decoded_node(Vec::new(), true),
        ];

        let root = build_overlay_from_disk::<(), U64_CX_PREFIX_COMPACT>(node_index(0), &nodes, 2)
            .expect("shared acyclic child must materialize");
        let left = root
            .find_child(3)
            .and_then(Child::as_in_mem)
            .expect("left shared edge");
        let right = root
            .find_child(5)
            .and_then(Child::as_in_mem)
            .expect("right shared edge");

        assert!(left.is_final());
        assert!(Arc::ptr_eq(left, right));
    }

    #[test]
    fn iterator_emits_every_labeled_path_to_a_shared_child() {
        let value = bincode_compat::serialize(&41u64).expect("serialize shared test value");
        let mut shared = decoded_node(Vec::new(), true);
        shared.value = Some(value);
        let nodes = vec![
            decoded_node(vec![(3, node_index(1)), (5, node_index(1))], false),
            shared,
        ];
        let root = build_overlay_from_disk::<u64, U64_CX_PREFIX_COMPACT>(node_index(0), &nodes, 2)
            .expect("shared acyclic child must materialize");

        let emitted = U64SequenceIterator::new(root, Vec::new())
            .collect::<Result<Vec<_>>>()
            .expect("resident shared DAG must enumerate exactly");

        assert_eq!(emitted, vec![(vec![3], Some(41)), (vec![5], Some(41))]);
    }

    #[test]
    fn iterator_rejects_an_unresolved_on_disk_edge_without_omission() {
        let pointer = SwizzledPtr::on_disk(0, 7, NodeType::CharBucket);
        let root = Arc::new(
            U64Node::<(), U64_CX_PREFIX_COMPACT>::new().with_child(9, Child::OnDisk(pointer)),
        );
        let mut iterator = U64SequenceIterator::new(root, Vec::new());

        let error = iterator
            .next()
            .expect("unresolved edge must produce one result")
            .expect_err("unresolved edge must fail closed");
        assert!(error
            .to_string()
            .contains("native-u64 snapshots must be fully resident"));
        assert!(
            iterator.next().is_none(),
            "failure must terminate traversal"
        );
    }

    #[test]
    fn iterator_clones_values_only_when_their_terms_are_yielded() {
        let root = Arc::new(
            U64Node::<CloneObservedValue, U64_CX_PREFIX_COMPACT>::new()
                .as_final()
                .with_value(CloneObservedValue(7)),
        );
        ITERATOR_VALUE_CLONES.store(0, Ordering::Relaxed);

        let mut iterator = U64SequenceIterator::new(root, Vec::new());
        assert_eq!(ITERATOR_VALUE_CLONES.load(Ordering::Relaxed), 0);
        assert_eq!(
            iterator.next().expect("root term").expect("resident root"),
            (Vec::new(), Some(CloneObservedValue(7)))
        );
        assert_eq!(ITERATOR_VALUE_CLONES.load(Ordering::Relaxed), 1);
        assert!(iterator.next().is_none());
    }

    #[test]
    fn iterator_preserves_order_across_every_u64_child_store_tier_boundary() {
        for degree in [4usize, 5, 16, 17, 128, 129] {
            let children = (0..degree)
                .map(|label| {
                    (
                        label as u64,
                        Child::InMem(Arc::new(
                            U64Node::<(), U64_CX_PREFIX_COMPACT>::new().as_final(),
                        )),
                    )
                })
                .collect::<Vec<_>>();
            let witness = SortedUniqueEntries::try_new(children)
                .expect("generated labels are strictly ascending");
            let root = Arc::new(U64Node::<(), U64_CX_PREFIX_COMPACT>::from_sorted_children(
                false, None, witness,
            ));

            let emitted = U64SequenceIterator::new(root, Vec::new())
                .map(|item| item.expect("tier topology is resident").0)
                .collect::<Vec<_>>();
            let expected = (0..degree)
                .map(|label| vec![label as u64])
                .collect::<Vec<_>>();
            assert_eq!(emitted, expected, "u64 child tier degree {degree}");
        }
    }

    #[test]
    fn disk_materializer_preserves_shared_path_compressed_child() {
        let mut shared = decoded_node(Vec::new(), true);
        shared.prefix = vec![7, 8];
        let nodes = vec![
            decoded_node(vec![(3, node_index(1)), (5, node_index(1))], false),
            shared,
        ];

        let root = build_overlay_from_disk::<(), U64_CX_PREFIX_COMPACT>(node_index(0), &nodes, 2)
            .expect("shared compressed child must materialize");
        let left = root
            .find_child(3)
            .and_then(Child::as_in_mem)
            .expect("left shared edge");
        let right = root
            .find_child(5)
            .and_then(Child::as_in_mem)
            .expect("right shared edge");

        assert!(Arc::ptr_eq(left, right));
        let after_seven = left
            .find_child(7)
            .and_then(Child::as_in_mem)
            .expect("first expanded prefix unit");
        let terminal = after_seven
            .find_child(8)
            .and_then(Child::as_in_mem)
            .expect("second expanded prefix unit");
        assert!(terminal.is_final());
    }

    #[test]
    fn disk_materializer_ignores_unreachable_cycle_by_version_one_policy() {
        let nodes = vec![
            decoded_node(Vec::new(), true),
            decoded_node(vec![(9, node_index(1))], false),
        ];

        let root = build_overlay_from_disk::<(), U64_CX_PREFIX_COMPACT>(node_index(0), &nodes, 1)
            .expect("unreachable records do not participate in materialization semantics");

        assert!(root.is_final());
        assert_eq!(root.num_children(), 0);
    }

    #[test]
    fn checkpoint_pointer_decoder_rejects_null_out_of_range_and_noncanonical_fields() {
        let null_error = decode_node_index(SwizzledPtr::null().to_raw(), 1, U64PointerRole::Test)
            .expect_err("null child pointer must be rejected");
        assert!(null_error.to_string().contains("null, memory, or invalid"));

        let range_error = decode_node_index(disk_ptr(1), 1, U64PointerRole::Test)
            .expect_err("out-of-range child pointer must be rejected");
        assert!(range_error.to_string().contains("index 1 out of 1"));

        let block_error = decode_node_index(
            SwizzledPtr::on_disk(1, 0, NodeType::CharBucket).to_raw(),
            1,
            U64PointerRole::Test,
        )
        .expect_err("nonzero block must be rejected");
        assert!(block_error.to_string().contains("noncanonical"));

        let type_error = decode_node_index(
            SwizzledPtr::on_disk(0, 0, NodeType::Bucket).to_raw(),
            1,
            U64PointerRole::Test,
        )
        .expect_err("wrong node type must be rejected");
        assert!(type_error.to_string().contains("noncanonical"));

        let reserved_error = decode_node_index(disk_ptr(0) | (1 << 17), 1, U64PointerRole::Test)
            .expect_err("reserved pointer bits must be rejected");
        assert!(reserved_error.to_string().contains("reserved flag bits"));
    }

    #[test]
    fn disk_materializer_rejects_term_count_mismatch_before_construction() {
        let nodes = vec![decoded_node(Vec::new(), true)];
        let error = build_overlay_from_disk::<(), U64_CX_PREFIX_COMPACT>(node_index(0), &nodes, 2)
            .expect_err("header count must match the reachable graph language");
        assert!(error.to_string().contains("term count mismatch"));
    }

    #[test]
    fn sorted_child_witness_rejects_duplicate_and_descending_labels() {
        assert!(
            SortedUniqueEntries::try_new(vec![(3u64, node_index(0)), (3, node_index(0))]).is_err()
        );
        assert!(
            SortedUniqueEntries::try_new(vec![(5u64, node_index(0)), (3, node_index(0))]).is_err()
        );
    }

    #[test]
    fn checkpoint_node_bound_matches_disk_pointer_cardinality() {
        assert_eq!(MAX_NODE_COUNT, MAX_OFFSET as u64 + 1);
        assert_eq!(
            checkpoint_node_index(MAX_OFFSET as usize).expect("maximum index is admitted"),
            MAX_OFFSET
        );
        assert!(checkpoint_node_index(MAX_NODE_COUNT as usize).is_err());
        assert_eq!(
            SwizzledPtr::on_disk(0, MAX_OFFSET, NodeType::CharBucket)
                .disk_location()
                .expect("maximum offset is representable")
                .offset,
            MAX_OFFSET
        );
    }

    #[test]
    fn checkpoint_length_encoding_enforces_reader_limits() {
        assert_eq!(
            snapshot_u32_len("prefix", MAX_PREFIX_UNITS as usize, MAX_PREFIX_UNITS)
                .expect("maximum prefix length is encodable"),
            MAX_PREFIX_UNITS
        );
        assert!(
            snapshot_u32_len("prefix", MAX_PREFIX_UNITS as usize + 1, MAX_PREFIX_UNITS,).is_err()
        );
        assert_eq!(
            snapshot_u64_len("value", MAX_VALUE_BYTES as usize, MAX_VALUE_BYTES)
                .expect("maximum value length is encodable"),
            MAX_VALUE_BYTES
        );
        assert!(snapshot_u64_len("value", MAX_VALUE_BYTES as usize + 1, MAX_VALUE_BYTES,).is_err());
    }

    #[test]
    fn checkpoint_flags_are_canonical_and_match_value_presence() {
        validate_snapshot_flags(SNAPSHOT_FLAG_IS_FINAL | SNAPSHOT_FLAG_HAS_VALUE)
            .expect("known flags are accepted");
        assert!(validate_snapshot_flags(0b1000_0000).is_err());
        validate_snapshot_value_flag(SNAPSHOT_FLAG_HAS_VALUE, true)
            .expect("flagged bytes are canonical");
        validate_snapshot_value_flag(0, false).expect("absent value is canonical");
        assert!(validate_snapshot_value_flag(SNAPSHOT_FLAG_HAS_VALUE, false).is_err());
        assert!(validate_snapshot_value_flag(0, true).is_err());
    }
}
