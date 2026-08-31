//! Shared types for PersistentARTrieChar implementations.
//!
//! This module contains the core types used by both the in-memory and disk-backed
//! variants of the character-level trie.

use crate::persistent_artrie::swizzled_ptr::{NodeType, SwizzledPtr};
use crate::value::DictionaryValue;
use smallvec::SmallVec;
use std::cell::Cell;
use std::marker::PhantomData;
use std::ptr::NonNull;

use super::nodes::{AddChildError, CharNode, CharNodeChildCursor};

/// Magic bytes for char trie file
pub const CHAR_TRIE_MAGIC: [u8; 4] = *b"ARTC";

/// File header size in bytes
pub const CHAR_FILE_HEADER_SIZE: usize = 64;

/// Header format version 1 (original, no checksum)
pub const CHAR_HEADER_VERSION_V1: u8 = 1;

/// Header format version 2 (with checksum for crash recovery)
pub const CHAR_HEADER_VERSION_V2: u8 = 2;

/// Default buffer pool size (number of pages)
pub const DEFAULT_CHAR_BUFFER_POOL_SIZE: usize = 256;

/// Reference to a node in the trie for parent pointer backtracking.
///
/// Used for reverse lookups (value → term) by storing the location
/// of the node that contains each value, enabling O(k) reconstruction
/// of the term by backtracking parent pointers.
///
/// # Layout (8 bytes)
///
/// ```text
/// ┌─────────────────┬─────────────────┐
/// │ arena_id (u32)  │ slot_index (u32)│
/// └─────────────────┴─────────────────┘
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(C)]
pub struct NodeRef {
    /// Arena ID where the node resides (u32::MAX = NULL)
    pub arena_id: u32,
    /// Slot index within the arena (u32::MAX = NULL)
    pub slot_index: u32,
}

impl NodeRef {
    /// Null reference (no node)
    pub const NULL: Self = Self {
        arena_id: u32::MAX,
        slot_index: u32::MAX,
    };

    /// Create a new NodeRef from arena and slot indices.
    #[inline]
    pub const fn new(arena_id: u32, slot_index: u32) -> Self {
        Self {
            arena_id,
            slot_index,
        }
    }

    /// Check if this is a null reference.
    #[inline]
    pub const fn is_null(&self) -> bool {
        self.arena_id == u32::MAX && self.slot_index == u32::MAX
    }

    /// Convert to bytes for serialization.
    #[inline]
    pub fn to_bytes(&self) -> [u8; 8] {
        let mut bytes = [0u8; 8];
        bytes[0..4].copy_from_slice(&self.arena_id.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.slot_index.to_le_bytes());
        bytes
    }

    /// Create from bytes for deserialization.
    #[inline]
    pub fn from_bytes(bytes: &[u8; 8]) -> Self {
        Self {
            arena_id: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            slot_index: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
        }
    }
}

impl Default for NodeRef {
    fn default() -> Self {
        Self::NULL
    }
}

/// Mode of enhanced recovery (with epoch/per-node logging integration).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnhancedRecoveryMode {
    /// File was created new (didn't exist before)
    CreatedNew,
    /// Normal open, no recovery needed
    Normal,
    /// Recovered from WAL after last checkpoint
    WalReplay,
    /// Rebuilt from WAL archive segments
    RebuiltFromWal,
    /// Rebuilt from WAL archive files
    RebuiltFromArchives,
    /// Recovered using epoch-based checkpointing
    EpochRecovery,
    /// Recovered using per-node logging (O(dirty nodes))
    PerNodeRecovery,
}

impl EnhancedRecoveryMode {
    /// Returns true if recovery required rebuilding from WAL
    pub fn required_rebuild(&self) -> bool {
        matches!(
            self,
            EnhancedRecoveryMode::RebuiltFromWal | EnhancedRecoveryMode::RebuiltFromArchives
        )
    }

    /// Returns true if this was a normal open (no recovery)
    pub fn is_normal(&self) -> bool {
        matches!(
            self,
            EnhancedRecoveryMode::Normal | EnhancedRecoveryMode::CreatedNew
        )
    }
}

/// Statistics from enhanced recovery.
#[derive(Debug, Clone)]
pub struct EnhancedRecoveryStats {
    /// The recovery mode used
    pub mode: EnhancedRecoveryMode,
    /// Total time for recovery in milliseconds
    pub duration_ms: u64,
    /// Number of WAL records replayed
    pub records_replayed: usize,
    /// Number of epochs recovered (for epoch-based recovery)
    pub epochs_recovered: usize,
    /// Number of dirty nodes recovered (for per-node logging)
    pub dirty_nodes_recovered: usize,
    /// Number of archive segments used
    pub archive_segments_used: usize,
}

impl EnhancedRecoveryStats {
    /// Create stats for normal open (no recovery)
    pub fn normal() -> Self {
        Self {
            mode: EnhancedRecoveryMode::Normal,
            duration_ms: 0,
            records_replayed: 0,
            epochs_recovered: 0,
            dirty_nodes_recovered: 0,
            archive_segments_used: 0,
        }
    }

    /// Create stats for new file creation
    pub fn created_new() -> Self {
        Self {
            mode: EnhancedRecoveryMode::CreatedNew,
            duration_ms: 0,
            records_replayed: 0,
            epochs_recovered: 0,
            dirty_nodes_recovered: 0,
            archive_segments_used: 0,
        }
    }
}

/// File header for disk-backed char trie
///
/// # Layout (64 bytes total)
///
/// ```text
/// Offset  Size  Field
/// ------  ----  -----
///   0       4   magic ("ARTC")
///   4       1   version (1 = no checksum, 2 = with checksum)
///   5       3   reserved
///   8       8   root_ptr (block ID of root node)
///  16       8   entry_count
///  24       8   checkpoint_lsn
///  32       4   header_checksum (V2+: CRC32 of bytes 0-31)
///  36      28   padding
/// ```
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CharTrieFileHeader {
    /// Magic bytes "ARTC"
    pub magic: [u8; 4],
    /// Format version (1 = no checksum, 2 = with checksum)
    pub version: u8,
    /// Reserved bytes
    pub _reserved: [u8; 3],
    /// Root node pointer (block ID)
    pub root_ptr: u64,
    /// Number of entries in the trie
    pub entry_count: u64,
    /// Checkpoint LSN (for WAL truncation)
    pub checkpoint_lsn: u64,
    /// CRC32 checksum of bytes 0-31 (V2+ only, 0 for V1)
    pub header_checksum: u32,
    /// Padding to 64 bytes
    pub _padding: [u8; 28],
}

impl CharTrieFileHeader {
    /// Create a new file header (V2 format with checksum)
    pub fn new() -> Self {
        Self {
            magic: CHAR_TRIE_MAGIC,
            version: CHAR_HEADER_VERSION_V2,
            _reserved: [0; 3],
            root_ptr: 0,
            entry_count: 0,
            checkpoint_lsn: 0,
            header_checksum: 0,
            _padding: [0; 28],
        }
    }

    /// Create a V1 header (for backward compatibility testing)
    #[cfg(test)]
    pub fn new_v1() -> Self {
        Self {
            magic: CHAR_TRIE_MAGIC,
            version: CHAR_HEADER_VERSION_V1,
            _reserved: [0; 3],
            root_ptr: 0,
            entry_count: 0,
            checkpoint_lsn: 0,
            header_checksum: 0,
            _padding: [0; 28],
        }
    }

    /// Check if this header version supports checksums
    pub fn has_checksum(&self) -> bool {
        self.version >= CHAR_HEADER_VERSION_V2
    }

    /// Compute the header checksum (CRC32 of bytes 0-31)
    pub fn compute_checksum(&self) -> u32 {
        let mut bytes = [0u8; 32];
        bytes[0..4].copy_from_slice(&self.magic);
        bytes[4] = self.version;
        bytes[5..8].copy_from_slice(&self._reserved);
        bytes[8..16].copy_from_slice(&self.root_ptr.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.entry_count.to_le_bytes());
        bytes[24..32].copy_from_slice(&self.checkpoint_lsn.to_le_bytes());
        crc32_header(&bytes)
    }

    /// Update the checksum to match current header values
    pub fn finalize_checksum(&mut self) {
        if self.has_checksum() {
            self.header_checksum = self.compute_checksum();
        }
    }

    /// Verify the header checksum
    ///
    /// Returns true if:
    /// - V1 header (no checksum, always valid)
    /// - V2+ header with matching checksum
    pub fn verify_checksum(&self) -> bool {
        if !self.has_checksum() {
            // V1 headers don't have checksums, consider valid
            return true;
        }
        self.header_checksum == self.compute_checksum()
    }

    /// Serialize to bytes (does NOT auto-finalize checksum)
    ///
    /// Call `finalize_checksum()` before serializing to ensure checksum is valid.
    pub fn to_bytes(&self) -> [u8; CHAR_FILE_HEADER_SIZE] {
        let mut bytes = [0u8; CHAR_FILE_HEADER_SIZE];
        bytes[0..4].copy_from_slice(&self.magic);
        bytes[4] = self.version;
        bytes[5..8].copy_from_slice(&self._reserved);
        bytes[8..16].copy_from_slice(&self.root_ptr.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.entry_count.to_le_bytes());
        bytes[24..32].copy_from_slice(&self.checkpoint_lsn.to_le_bytes());
        bytes[32..36].copy_from_slice(&self.header_checksum.to_le_bytes());
        bytes[36..64].copy_from_slice(&self._padding);
        bytes
    }

    /// Serialize to bytes with checksum finalization
    pub fn to_bytes_with_checksum(&mut self) -> [u8; CHAR_FILE_HEADER_SIZE] {
        self.finalize_checksum();
        self.to_bytes()
    }

    /// Deserialize from bytes
    pub fn from_bytes(bytes: &[u8; CHAR_FILE_HEADER_SIZE]) -> Self {
        Self {
            magic: [bytes[0], bytes[1], bytes[2], bytes[3]],
            version: bytes[4],
            _reserved: [bytes[5], bytes[6], bytes[7]],
            root_ptr: u64::from_le_bytes([
                bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14],
                bytes[15],
            ]),
            entry_count: u64::from_le_bytes([
                bytes[16], bytes[17], bytes[18], bytes[19], bytes[20], bytes[21], bytes[22],
                bytes[23],
            ]),
            checkpoint_lsn: u64::from_le_bytes([
                bytes[24], bytes[25], bytes[26], bytes[27], bytes[28], bytes[29], bytes[30],
                bytes[31],
            ]),
            header_checksum: u32::from_le_bytes([bytes[32], bytes[33], bytes[34], bytes[35]]),
            _padding: {
                let mut arr = [0u8; 28];
                arr.copy_from_slice(&bytes[36..64]);
                arr
            },
        }
    }

    /// Deserialize from bytes and verify checksum
    ///
    /// Returns `Err` if checksum verification fails (V2+ only).
    pub fn from_bytes_verified(
        bytes: &[u8; CHAR_FILE_HEADER_SIZE],
    ) -> crate::persistent_artrie::error::Result<Self> {
        use crate::persistent_artrie::error::PersistentARTrieError;

        let header = Self::from_bytes(bytes);
        if header.has_checksum() && !header.verify_checksum() {
            return Err(PersistentARTrieError::CorruptedFile {
                reason: format!(
                    "Header checksum mismatch: stored={:#x}, computed={:#x}",
                    header.header_checksum,
                    header.compute_checksum()
                ),
            });
        }
        Ok(header)
    }

    /// Validate the header (magic + version + checksum)
    pub fn validate(&self) -> crate::persistent_artrie::error::Result<()> {
        use crate::persistent_artrie::error::PersistentARTrieError;

        if self.magic != CHAR_TRIE_MAGIC {
            // Convert [u8; 4] to u64 for the error type
            let expected = u64::from_le_bytes([
                CHAR_TRIE_MAGIC[0],
                CHAR_TRIE_MAGIC[1],
                CHAR_TRIE_MAGIC[2],
                CHAR_TRIE_MAGIC[3],
                0,
                0,
                0,
                0,
            ]);
            let found = u64::from_le_bytes([
                self.magic[0],
                self.magic[1],
                self.magic[2],
                self.magic[3],
                0,
                0,
                0,
                0,
            ]);
            return Err(PersistentARTrieError::InvalidMagic { expected, found });
        }
        if self.has_checksum() && !self.verify_checksum() {
            return Err(PersistentARTrieError::CorruptedFile {
                reason: format!(
                    "Header checksum mismatch: stored={:#x}, computed={:#x}",
                    self.header_checksum,
                    self.compute_checksum()
                ),
            });
        }
        Ok(())
    }

    /// Upgrade V1 header to V2 format with checksum
    pub fn upgrade_to_v2(&mut self) {
        if self.version < CHAR_HEADER_VERSION_V2 {
            self.version = CHAR_HEADER_VERSION_V2;
            self.finalize_checksum();
        }
    }
}

impl Default for CharTrieFileHeader {
    fn default() -> Self {
        Self::new()
    }
}

/// CRC32 checksum (IEEE polynomial) for header integrity verification
fn crc32_header(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFFFFFF;
    for byte in data {
        crc ^= *byte as u32;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB88320;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

/// A term with its arena location for page-aware batching.
///
/// Used by `iter_prefix_with_arena()` to enable I/O-efficient batch operations
/// by grouping terms that reside in the same disk arena/page.
#[derive(Debug, Clone)]
pub struct PrefixTermWithArena {
    /// The term string
    pub term: String,
    /// The arena ID where this term's node resides (None for in-memory nodes)
    pub arena_id: Option<u32>,
}

/// A term with its value and arena location for page-aware merge operations.
///
/// Used by `iter_prefix_with_values_and_arena()` to enable I/O-efficient batch
/// operations by grouping terms that reside in the same disk arena/page.
/// This is the same pattern used by `remove_prefix_batched()`.
#[derive(Debug, Clone)]
pub struct PrefixTermWithValueAndArena<V> {
    /// The term string
    pub term: String,
    /// The value associated with this term
    pub value: V,
    /// The arena ID where this term's node resides (None for in-memory nodes)
    pub arena_id: Option<u32>,
}

/// A trie node for the char trie (CharNode-based implementation)
///
/// Uses adaptive CharNode types (CharNode4/16/48/CharBucket) for efficient
/// child storage. Each child is stored as a raw pointer to a heap-allocated
/// CharTrieNodeInner, with the pointer stored in the CharNode's child slots.
///
/// # Memory Layout
///
/// Children are stored as raw `*mut CharTrieNodeInner<V>` pointers within
/// the CharNode structure. This enables:
/// - Adaptive node sizing (N4 → N16 → N48 → Bucket as children grow)
/// - Efficient SIMD lookups for CharNode16
/// - Binary search for CharNode48
/// - HashMap for CharBucket (>48 children)
///
/// # Safety
///
/// The raw pointers are managed carefully:
/// - Created via `Box::into_raw()` when inserting children
/// - Recovered via `Box::from_raw()` when dropping or removing
/// - The `Drop` implementation ensures all children are properly freed
/// - This legacy owned projection is deliberately `Send` but not `Sync`: safe
///   cloning and borrowed traversal require exclusive ownership of its pointer
///   topology. Concurrent persistent access uses the immutable overlay types.
/// - `CharNode::value_ptr` is persistent, non-owning location metadata. The
///   optional `value` field below is the node's sole owned value.
///
/// Destruction preserves the former recursive traversal semantics without
/// consuming the native call stack: each child's complete subtree and value
/// are dropped before the next child, and this node's value is dropped last.
/// Sibling order is the native iteration order of the adaptive node variant;
/// for `CharBucket`, that order remains representation-defined by its hash
/// table rather than sorted by key.
///
/// ```compile_fail
/// use libdictenstein::persistent_artrie::char::CharTrieNodeInner;
/// fn requires_sync<T: Sync>() {}
/// requires_sync::<CharTrieNodeInner<()>>();
/// ```
///
/// The owning topology is intentionally sealed from downstream code. In
/// particular, a caller cannot install an arbitrary raw pointer into a child
/// slot that this type would later reclaim as a `Box`:
///
/// ```compile_fail
/// use libdictenstein::persistent_artrie::char::{CharNode, CharTrieNodeInner};
/// let mut root = CharTrieNodeInner::<()>::new();
/// root.node = CharNode::new_node4();
/// ```
///
/// Raw child-pointer installation is likewise not a public operation:
///
/// ```compile_fail
/// use libdictenstein::persistent_artrie::char::CharTrieNodeInner;
/// use libdictenstein::persistent_artrie::SwizzledPtr;
/// let mut root = CharTrieNodeInner::<()>::new();
/// root.insert_child_ptr('x', SwizzledPtr::null());
/// ```
pub struct CharTrieNodeInner<V: DictionaryValue> {
    /// The adaptive radix node structure (N4/N16/N48/Bucket)
    /// Children are stored as raw pointers encoded in the CharNode's SwizzledPtr fields.
    node: CharNode,
    /// Optional value associated with this node (stored separately from CharNode)
    pub value: Option<V>,
    /// Prevent shared cross-thread access to the raw owned topology while
    /// preserving `Send` for ownership transfer.
    _not_sync: PhantomData<Cell<()>>,
}

/// A failed resident-child insertion together with the unconsumed child.
///
/// Returning the child makes failure ownership explicit: callers never lose a
/// value merely because the adaptive representation rejected a structural
/// insertion.
pub struct InsertCharChildError<V: DictionaryValue> {
    source: AddChildError,
    child: CharTrieNodeInner<V>,
}

/// Raw-only ownership guard for a child that has not yet been published.
///
/// The pointer is derived exactly once from `Box::into_raw`. Keeping ownership
/// in this guard rather than a live `Box` prevents a later unique-reference
/// retag from invalidating the pointer stored in [`SwizzledPtr`]. On failure or
/// unwind the armed guard reclaims the allocation; successful publication
/// disarms it without reconstructing a `Box`.
struct PendingOwnedCharChild<V: DictionaryValue> {
    pointer: Option<NonNull<CharTrieNodeInner<V>>>,
}

impl<V: DictionaryValue> PendingOwnedCharChild<V> {
    fn new(child: CharTrieNodeInner<V>) -> Self {
        Self {
            pointer: Some(
                NonNull::new(Box::into_raw(Box::new(child)))
                    .expect("Box::into_raw always returns a non-null pointer"),
            ),
        }
    }

    #[inline]
    fn pointer(&self) -> NonNull<CharTrieNodeInner<V>> {
        self.pointer.expect("pending child ownership remains armed")
    }

    #[inline]
    fn swizzled_pointer(&self) -> SwizzledPtr {
        SwizzledPtr::in_memory_nonnull(self.pointer())
    }

    #[inline]
    fn publish(mut self) -> NonNull<CharTrieNodeInner<V>> {
        self.pointer
            .take()
            .expect("pending child ownership remains armed")
    }

    fn into_child(mut self) -> CharTrieNodeInner<V> {
        let pointer = self
            .pointer
            .take()
            .expect("pending child ownership remains armed");
        // SAFETY: the guard is the allocation's sole owner and has just been
        // disarmed, so the returned value receives ownership exactly once.
        *unsafe { Box::from_raw(pointer.as_ptr()) }
    }
}

impl<V: DictionaryValue> Drop for PendingOwnedCharChild<V> {
    fn drop(&mut self) {
        if let Some(pointer) = self.pointer.take() {
            // SAFETY: an armed guard is the allocation's sole owner. No child
            // slot can own it unless `publish` has consumed and disarmed us.
            unsafe { drop(Box::from_raw(pointer.as_ptr())) };
        }
    }
}

impl<V: DictionaryValue> InsertCharChildError<V> {
    /// The adaptive-node failure that prevented insertion.
    pub fn source_error(&self) -> AddChildError {
        self.source
    }

    /// Borrow the child that was not consumed.
    pub fn child(&self) -> &CharTrieNodeInner<V> {
        &self.child
    }

    /// Recover the child that was not consumed.
    pub fn into_child(self) -> CharTrieNodeInner<V> {
        self.child
    }
}

impl<V: DictionaryValue> std::fmt::Debug for InsertCharChildError<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InsertCharChildError")
            .field("source", &self.source)
            .field("child", &self.child)
            .finish()
    }
}

impl<V: DictionaryValue> std::fmt::Display for InsertCharChildError<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "resident child insertion failed: {}", self.source)
    }
}

impl<V: DictionaryValue> std::error::Error for InsertCharChildError<V> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// A validated non-owning child slot used only by persistence projections.
#[derive(Debug, Clone)]
pub(crate) struct NonResidentCharChild(SwizzledPtr);

impl NonResidentCharChild {
    /// Consume the validated wrapper and recover its non-owning pointer value.
    pub(crate) fn into_pointer(self) -> SwizzledPtr {
        self.0
    }
}

impl TryFrom<SwizzledPtr> for NonResidentCharChild {
    type Error = InvalidNonResidentCharChild;

    fn try_from(pointer: SwizzledPtr) -> std::result::Result<Self, Self::Error> {
        if pointer.is_null() || pointer.disk_location().is_some() {
            Ok(Self(pointer))
        } else {
            Err(InvalidNonResidentCharChild { pointer })
        }
    }
}

/// A pointer state that cannot be represented by a non-owning child slot.
#[derive(Debug)]
pub(crate) struct InvalidNonResidentCharChild {
    pointer: SwizzledPtr,
}

impl std::fmt::Display for InvalidNonResidentCharChild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "pointer state {:#018x} is neither null nor a validated disk location",
            self.pointer.to_raw()
        )
    }
}

impl std::error::Error for InvalidNonResidentCharChild {}

/// Failure to add a validated nonresident child to an absent slot.
#[derive(Debug)]
pub(crate) struct AddNonResidentCharChildError {
    source: AddChildError,
    child: NonResidentCharChild,
}

impl std::fmt::Display for AddNonResidentCharChildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "nonresident child insertion failed for pointer state {:#018x}: {}",
            self.child.0.to_raw(),
            self.source
        )
    }
}

impl std::error::Error for AddNonResidentCharChildError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Failure to copy a semantic prefix into the sealed owned representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CharPrefixError {
    /// The prefix exceeds the fixed character-prefix capacity.
    TooLong { length: usize, capacity: usize },
    /// A character-trie prefix unit is not a Unicode scalar value.
    InvalidScalar { index: usize, unit: u32 },
}

impl std::fmt::Display for CharPrefixError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLong { length, capacity } => {
                write!(
                    f,
                    "character prefix length {length} exceeds capacity {capacity}"
                )
            }
            Self::InvalidScalar { index, unit } => write!(
                f,
                "character prefix unit {index} ({unit:#x}) is not a Unicode scalar value"
            ),
        }
    }
}

impl std::error::Error for CharPrefixError {}

/// Failure to project the sealed topology into a non-owning serialization node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EncodedCharNodeError {
    /// The header tag does not identify the sealed enum representation.
    HeaderTypeMismatch { expected: NodeType, actual: u8 },
    /// Resolved children do not cover the exact source child set.
    ChildCountMismatch { expected: usize, actual: usize },
    /// A fixed representation's header count exceeds its physical capacity.
    RepresentationCapacityExceeded {
        node_type: NodeType,
        count: usize,
        capacity: usize,
    },
    /// A bucket header does not describe its physical entry set.
    BucketCardinalityMismatch { header: usize, entries: usize },
    /// Bucket projections must supply unique keys in canonical ascending order.
    NonCanonicalChildOrder {
        index: usize,
        previous: u32,
        actual: u32,
    },
    /// A supplied bucket key is absent from the sealed node.
    MissingSourceChild { index: usize, key: u32 },
    /// A resolved child was paired with a different source edge.
    ChildKeyMismatch {
        index: usize,
        expected: u32,
        actual: u32,
    },
    /// Serialized child slots must name durable records, not null or memory.
    NonDiskChild { index: usize, raw: u64 },
    /// The sealed node itself must already contain a durable child reference.
    NonDiskSourceChild { index: usize, raw: u64 },
    /// The sealed node and resolved child view name different durable records.
    ChildPointerMismatch {
        index: usize,
        expected_raw: u64,
        actual_raw: u64,
    },
}

impl std::fmt::Display for EncodedCharNodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HeaderTypeMismatch { expected, actual } => write!(
                f,
                "sealed character-node representation {expected:?} has header tag {actual}"
            ),
            Self::ChildCountMismatch { expected, actual } => write!(
                f,
                "resolved child count {actual} does not match sealed topology count {expected}"
            ),
            Self::RepresentationCapacityExceeded {
                node_type,
                count,
                capacity,
            } => write!(
                f,
                "sealed {node_type:?} child count {count} exceeds physical capacity {capacity}"
            ),
            Self::BucketCardinalityMismatch { header, entries } => write!(
                f,
                "sealed bucket header count {header} does not match physical entry count {entries}"
            ),
            Self::NonCanonicalChildOrder {
                index,
                previous,
                actual,
            } => write!(
                f,
                "resolved bucket child {index} has non-canonical key {actual:#x} after \
                 {previous:#x}"
            ),
            Self::MissingSourceChild { index, key } => write!(
                f,
                "resolved bucket child {index} key {key:#x} is absent from the sealed node"
            ),
            Self::ChildKeyMismatch {
                index,
                expected,
                actual,
            } => write!(
                f,
                "resolved child {index} has key {actual:#x}, expected {expected:#x}"
            ),
            Self::NonDiskChild { index, raw } => write!(
                f,
                "resolved child {index} has non-disk pointer state {raw:#018x}"
            ),
            Self::NonDiskSourceChild { index, raw } => write!(
                f,
                "sealed child {index} has non-disk pointer state {raw:#018x}"
            ),
            Self::ChildPointerMismatch {
                index,
                expected_raw,
                actual_raw,
            } => write!(
                f,
                "resolved child {index} names {actual_raw:#018x}, but the sealed node names \
                 {expected_raw:#018x}"
            ),
        }
    }
}

impl std::error::Error for EncodedCharNodeError {}

/// Sealed capability proving that a thread-local projected node exactly
/// matches its ordered durable-child view.
///
/// `Cell` keeps the capability `!Sync`; serialization cannot outlive or race
/// mutation of the projected [`CharTrieNodeInner`]. The underlying node is not
/// exposed outside the internal character serializer.
#[derive(Debug)]
pub(crate) struct ValidatedBorrowedCharNode<'a> {
    node: &'a CharNode,
    _not_sync: PhantomData<Cell<()>>,
}

impl ValidatedBorrowedCharNode<'_> {
    #[inline]
    pub(super) fn as_node(&self) -> &CharNode {
        self.node
    }

    #[inline]
    pub(super) fn representation_type(&self) -> NodeType {
        self.node.representation_type()
    }
}

#[inline]
fn validate_serialization_pointer_pair(
    index: usize,
    source_pointer: &SwizzledPtr,
    resolved_pointer: &SwizzledPtr,
) -> std::result::Result<(), EncodedCharNodeError> {
    let source_location =
        source_pointer
            .disk_location()
            .ok_or_else(|| EncodedCharNodeError::NonDiskSourceChild {
                index,
                raw: source_pointer.to_raw(),
            })?;
    let resolved_location =
        resolved_pointer
            .disk_location()
            .ok_or_else(|| EncodedCharNodeError::NonDiskChild {
                index,
                raw: resolved_pointer.to_raw(),
            })?;
    if source_location != resolved_location {
        return Err(EncodedCharNodeError::ChildPointerMismatch {
            index,
            expected_raw: source_pointer.to_raw(),
            actual_raw: resolved_pointer.to_raw(),
        });
    }
    Ok(())
}

// Manual Debug implementation to avoid requiring Debug on V
impl<V: DictionaryValue> std::fmt::Debug for CharTrieNodeInner<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CharTrieNodeInner")
            .field("is_final", &self.node.is_final())
            .field("children_count", &self.node.num_children())
            .field("has_value", &self.value.is_some())
            .finish()
    }
}

// Manual Clone implementation - deep clones all children
impl<V: DictionaryValue> Clone for CharTrieNodeInner<V> {
    fn clone(&self) -> Self {
        struct CloneFrame<'a, V: DictionaryValue> {
            target: CharTrieNodeInner<V>,
            source_children: CharNodeChildCursor<'a>,
            incoming_key: Option<u32>,
        }

        impl<'a, V: DictionaryValue> CloneFrame<'a, V> {
            fn from_source(source: &'a CharTrieNodeInner<V>, incoming_key: Option<u32>) -> Self {
                Self {
                    target: source.clone_shell_without_in_memory_children(),
                    source_children: source.node.child_cursor(),
                    incoming_key,
                }
            }

            fn next_in_memory_child(&mut self) -> Option<(u32, &'a CharTrieNodeInner<V>)> {
                self.source_children.find_map(|(key, child)| {
                    child
                        .as_ptr::<CharTrieNodeInner<V>>()
                        // SAFETY: cloning this legacy owned projection requires a
                        // quiescent tree. Every in-memory slot then points to a live
                        // `Box` retained by the borrowed source root for `'a`.
                        .map(|pointer| (key, unsafe { &*pointer }))
                })
            }
        }

        fn attach_cloned_child<V: DictionaryValue>(
            parent: &mut CharTrieNodeInner<V>,
            key: u32,
            child: CharTrieNodeInner<V>,
        ) {
            let slot = parent
                .node
                .find_child_mut(key)
                .expect("an exact clone shell retains every source key");
            debug_assert!(slot.is_null(), "in-memory clone slot must be sanitized");
            let child = Box::new(child);
            *slot = SwizzledPtr::in_memory(Box::into_raw(child));
        }

        let mut frames: SmallVec<[CloneFrame<'_, V>; 8]> = SmallVec::new();
        frames.push(CloneFrame::from_source(self, None));

        loop {
            if let Some((key, child)) = frames
                .last_mut()
                .expect("the root frame remains until completion")
                .next_in_memory_child()
            {
                frames.push(CloneFrame::from_source(child, Some(key)));
                continue;
            }

            let completed = frames
                .pop()
                .expect("the completed clone frame remains present");
            if let Some(parent) = frames.last_mut() {
                attach_cloned_child(
                    &mut parent.target,
                    completed
                        .incoming_key
                        .expect("every non-root clone frame has an incoming key"),
                    completed.target,
                );
            } else {
                debug_assert!(completed.incoming_key.is_none());
                return completed.target;
            }
        }
    }
}

impl<V: DictionaryValue> CharTrieNodeInner<V> {
    /// Clone the exact node representation while removing only the duplicated
    /// in-memory ownership pointers from the target shell.
    fn clone_shell_without_in_memory_children(&self) -> Self {
        let mut node = self.node.clone();
        match &mut node {
            CharNode::N4(node) => {
                for slot in &mut node.children[..node.header.num_children as usize] {
                    if slot.as_ptr::<Self>().is_some() {
                        *slot = SwizzledPtr::null();
                    }
                }
            }
            CharNode::N16(node) => {
                for slot in &mut node.children[..node.header.num_children as usize] {
                    if slot.as_ptr::<Self>().is_some() {
                        *slot = SwizzledPtr::null();
                    }
                }
            }
            CharNode::N48(node) => {
                for slot in &mut node.children[..node.header.num_children as usize] {
                    if slot.as_ptr::<Self>().is_some() {
                        *slot = SwizzledPtr::null();
                    }
                }
            }
            CharNode::Bucket(node) => {
                for slot in node.entries.values_mut() {
                    if slot.as_ptr::<Self>().is_some() {
                        *slot = SwizzledPtr::null();
                    }
                }
            }
        }

        Self {
            node,
            value: self.value.clone(),
            _not_sync: PhantomData,
        }
    }

    /// Move every owned in-memory child into `pending` and leave this node childless.
    fn drain_owned_children(&mut self, pending: &mut SmallVec<[Box<Self>; 32]>) {
        fn adopt<V: DictionaryValue>(
            pointer: SwizzledPtr,
            pending: &mut SmallVec<[Box<CharTrieNodeInner<V>>; 32]>,
        ) {
            if let Some(pointer) = pointer.as_ptr::<CharTrieNodeInner<V>>() {
                // SAFETY: a live in-memory child slot owns exactly one allocation
                // produced by `Box::into_raw`. The caller moved that slot out before
                // this conversion, so `pending` becomes its sole owner.
                pending.push(unsafe { Box::from_raw(pointer.cast_mut()) });
            }
        }

        let owned_children = self
            .node
            .child_cursor()
            .filter(|(_, child)| child.as_ptr::<Self>().is_some())
            .count();
        // Reserve before moving any raw ownership slot. Once detachment starts,
        // every push is allocation-free, so an allocation failure cannot strand
        // unconverted pointers in a partially drained Bucket.
        pending.reserve(owned_children);
        let first_new_child = pending.len();

        match &mut self.node {
            CharNode::N4(node) => {
                let count = node.header.num_children as usize;
                for index in 0..count {
                    adopt(std::mem::take(&mut node.children[index]), pending);
                    node.keys[index] = 0;
                }
                node.header.num_children = 0;
            }
            CharNode::N16(node) => {
                let count = node.header.num_children as usize;
                for index in 0..count {
                    adopt(std::mem::take(&mut node.children[index]), pending);
                    node.keys[index] = 0;
                }
                node.header.num_children = 0;
            }
            CharNode::N48(node) => {
                let count = node.header.num_children as usize;
                for index in 0..count {
                    adopt(std::mem::take(&mut node.children[index]), pending);
                    node.keys[index] = 0;
                }
                node.header.num_children = 0;
            }
            CharNode::Bucket(node) => {
                for (_, pointer) in std::mem::take(&mut node.entries) {
                    adopt(pointer, pending);
                }
                node.header.num_children = 0;
            }
        }

        // Children were detached in the node's native iteration order. Reverse
        // only this newly appended range so LIFO processing visits the first
        // child first, matching the former recursive sibling traversal while
        // leaving already-pending ancestor siblings undisturbed.
        pending[first_new_child..].reverse();
    }
}

// Drop implementation - must free all child nodes
impl<V: DictionaryValue> Drop for CharTrieNodeInner<V> {
    fn drop(&mut self) {
        if self.node.num_children() == 0 {
            return;
        }

        enum DropWork<V: DictionaryValue> {
            Enter(Box<CharTrieNodeInner<V>>),
            Exit(Box<CharTrieNodeInner<V>>),
        }

        fn owned_child_count<V: DictionaryValue>(node: &CharTrieNodeInner<V>) -> usize {
            node.node
                .child_cursor()
                .filter(|(_, child)| child.as_ptr::<CharTrieNodeInner<V>>().is_some())
                .count()
        }

        let root_child_count = owned_child_count(self);
        let mut pending: SmallVec<[DropWork<V>; 32]> = SmallVec::new();
        let mut children: SmallVec<[Box<CharTrieNodeInner<V>>; 32]> = SmallVec::new();
        pending.reserve(root_child_count);
        children.reserve(root_child_count);
        self.drain_owned_children(&mut children);
        pending.extend(children.drain(..).map(DropWork::Enter));

        while let Some(work) = pending.pop() {
            match work {
                DropWork::Enter(mut node) => {
                    debug_assert!(children.is_empty());
                    let child_count = owned_child_count(&node);
                    let required = child_count
                        .checked_add(1)
                        .expect("a live node cannot own usize::MAX children");

                    // Reserve both continuations and detached owners before
                    // moving any raw pointer out of the node. The Exit frame
                    // retains the parent until every child subtree completes,
                    // exactly lowering recursive postorder into a heap PDA.
                    pending.reserve(required);
                    children.reserve(child_count);
                    node.drain_owned_children(&mut children);
                    pending.push(DropWork::Exit(node));
                    pending.extend(children.drain(..).map(DropWork::Enter));
                }
                DropWork::Exit(node) => {
                    // The node is childless, so re-entrant Drop takes the
                    // constant-depth leaf path before its value is destroyed.
                    drop(node);
                }
            }
        }
    }
}

impl<V: DictionaryValue> Default for CharTrieNodeInner<V> {
    fn default() -> Self {
        Self {
            node: CharNode::new_node4(), // Start with smallest node type
            value: None,
            _not_sync: PhantomData,
        }
    }
}

impl<V: DictionaryValue> CharTrieNodeInner<V> {
    /// Create a new empty node
    pub fn new() -> Self {
        Self::default()
    }

    /// Check if this node is final (accepting state)
    #[inline]
    pub fn is_final(&self) -> bool {
        self.node.is_final()
    }

    /// Set the final flag
    #[inline]
    pub fn set_final(&mut self, is_final: bool) {
        self.node.header_mut().set_final(is_final);
    }

    /// Get the number of children
    #[inline]
    pub fn num_children(&self) -> usize {
        self.node.num_children()
    }

    /// Get a child by character
    pub fn get_child(&self, c: char) -> Option<&CharTrieNodeInner<V>> {
        self.node
            .find_child(c as u32)
            .and_then(|ptr| ptr.as_ptr::<CharTrieNodeInner<V>>())
            .map(|ptr| {
                // Safety: We control all SwizzledPtr creation; ptr is valid
                unsafe { &*ptr }
            })
    }

    /// Get a child mutably by character
    pub fn get_child_mut(&mut self, c: char) -> Option<&mut CharTrieNodeInner<V>> {
        self.node
            .find_child(c as u32)
            .and_then(|ptr| ptr.as_ptr::<CharTrieNodeInner<V>>())
            .map(|ptr| {
                // Safety: We control all SwizzledPtr creation; ptr is valid
                // Note: This is technically unsound for shared access, but
                // the mutable borrow of self prevents concurrent access
                unsafe { &mut *(ptr as *mut CharTrieNodeInner<V>) }
            })
    }

    /// Insert a resident child, returning a replaced resident child if present.
    ///
    /// Replacement occurs directly in the occupied slot, preserving the exact
    /// adaptive representation, child count, and sibling order. A vacant-key
    /// insertion retains the new `Box` as an ownership guard until the fallible
    /// structural insertion succeeds.
    pub fn insert_child(
        &mut self,
        c: char,
        child: CharTrieNodeInner<V>,
    ) -> std::result::Result<Option<Box<CharTrieNodeInner<V>>>, InsertCharChildError<V>> {
        let key = c as u32;
        let pending = PendingOwnedCharChild::new(child);
        let new_pointer = pending.swizzled_pointer();

        if let Some(slot) = self.node.find_child_mut(key) {
            let old_pointer = std::mem::replace(slot, new_pointer);
            // The occupied slot is now the allocation's sole owner. There is no
            // fallible operation between publication and this ownership transfer.
            let _ = pending.publish();
            let old_child = old_pointer
                .as_ptr::<CharTrieNodeInner<V>>()
                // SAFETY: slot replacement detached the sole owning edge. The
                // private topology prevents safe code from forging or duplicating it.
                .map(|pointer| unsafe { Box::from_raw(pointer.cast_mut()) });
            return Ok(old_child);
        }

        match self.node.add_child_growing(key, new_pointer) {
            Ok(grown) => {
                let _ = pending.publish();
                if let Some(grown) = grown {
                    self.node = grown;
                }
                Ok(None)
            }
            Err(source) => Err(InsertCharChildError {
                source,
                child: pending.into_child(),
            }),
        }
    }

    /// Insert a decoded null/on-disk child into an absent slot.
    ///
    /// The method is deliberately crate-internal and rejects resident or
    /// transitional pointer states. It never replaces an existing slot, so it
    /// cannot silently transfer or discard raw ownership.
    pub(crate) fn try_add_nonresident_child(
        &mut self,
        c: char,
        child: NonResidentCharChild,
    ) -> std::result::Result<(), AddNonResidentCharChildError> {
        let key = c as u32;
        match self.node.add_child_growing(key, child.0.clone()) {
            Ok(grown) => {
                if let Some(grown) = grown {
                    self.node = grown;
                }
                Ok(())
            }
            Err(source) => Err(AddNonResidentCharChildError { source, child }),
        }
    }

    /// Replace the pointer-free compressed-prefix metadata.
    pub(super) fn set_compressed_prefix(
        &mut self,
        prefix: &[u32],
    ) -> std::result::Result<(), CharPrefixError> {
        let capacity = super::nodes::CHAR_MAX_PREFIX_LEN;
        if prefix.len() > capacity {
            return Err(CharPrefixError::TooLong {
                length: prefix.len(),
                capacity,
            });
        }
        for (index, &unit) in prefix.iter().enumerate() {
            if char::from_u32(unit).is_none() {
                return Err(CharPrefixError::InvalidScalar { index, unit });
            }
        }

        self.node.header_mut().prefix_len = prefix.len() as u8;
        *self.node.prefix_mut() = super::nodes::CharCompressedPrefix::from_chars(prefix);
        Ok(())
    }

    /// Borrow the pointer-free compressed-prefix units.
    pub(super) fn compressed_prefix(&self) -> &[u32] {
        let length = self.node.header().prefix_len as usize;
        self.node.prefix().as_slice(length)
    }

    /// Iterate over validated non-owning child snapshots.
    ///
    /// Each yielded `SwizzledPtr` is a clone detached from the owning slot's
    /// atomics, so consumers cannot mutate the sealed topology through it.
    pub(super) fn nonresident_children(
        &self,
    ) -> impl Iterator<
        Item = std::result::Result<(u32, NonResidentCharChild), InvalidNonResidentCharChild>,
    > + '_ {
        self.node.iter_children().map(|(key, pointer)| {
            NonResidentCharChild::try_from(pointer.clone()).map(|child| (key, child))
        })
    }

    /// Borrow the exact pointer-safe node prepared for durable serialization.
    ///
    /// The checkpoint projection has already installed every resolved child in
    /// this sealed node. Validate the representation, ordered keys, durable
    /// pointer state, and exact persistent location before borrowing it. This
    /// avoids cloning the adaptive node and searching it once per child while
    /// preserving the former rebuild-and-recollect validation boundary.
    #[inline]
    pub(crate) fn validated_node_for_serialization(
        &self,
        disk_children: &[(u32, SwizzledPtr)],
    ) -> std::result::Result<ValidatedBorrowedCharNode<'_>, EncodedCharNodeError> {
        let representation_type = self.node.representation_type();
        if !self.node.has_consistent_representation_type() {
            return Err(EncodedCharNodeError::HeaderTypeMismatch {
                expected: representation_type,
                actual: self.node.header().node_type,
            });
        }

        let expected = self.node.num_children();
        match &self.node {
            CharNode::N4(_) if expected > 4 => {
                return Err(EncodedCharNodeError::RepresentationCapacityExceeded {
                    node_type: representation_type,
                    count: expected,
                    capacity: 4,
                });
            }
            CharNode::N16(_) if expected > 16 => {
                return Err(EncodedCharNodeError::RepresentationCapacityExceeded {
                    node_type: representation_type,
                    count: expected,
                    capacity: 16,
                });
            }
            CharNode::N48(_) if expected > 48 => {
                return Err(EncodedCharNodeError::RepresentationCapacityExceeded {
                    node_type: representation_type,
                    count: expected,
                    capacity: 48,
                });
            }
            CharNode::Bucket(node) if node.len() != expected => {
                return Err(EncodedCharNodeError::BucketCardinalityMismatch {
                    header: expected,
                    entries: node.len(),
                });
            }
            _ => {}
        }
        if disk_children.len() != expected {
            return Err(EncodedCharNodeError::ChildCountMismatch {
                expected,
                actual: disk_children.len(),
            });
        }

        if let CharNode::Bucket(node) = &self.node {
            let mut previous_key = None;
            for (index, (key, pointer)) in disk_children.iter().enumerate() {
                if let Some(previous) = previous_key {
                    if *key <= previous {
                        return Err(EncodedCharNodeError::NonCanonicalChildOrder {
                            index,
                            previous,
                            actual: *key,
                        });
                    }
                }
                previous_key = Some(*key);
                let source_pointer = node
                    .entries
                    .get(key)
                    .ok_or(EncodedCharNodeError::MissingSourceChild { index, key: *key })?;
                validate_serialization_pointer_pair(index, source_pointer, pointer)?;
            }
        } else {
            for (index, ((expected_key, source_pointer), (actual_key, pointer))) in self
                .node
                .child_cursor()
                .zip(disk_children.iter())
                .enumerate()
            {
                if expected_key != *actual_key {
                    return Err(EncodedCharNodeError::ChildKeyMismatch {
                        index,
                        expected: expected_key,
                        actual: *actual_key,
                    });
                }
                validate_serialization_pointer_pair(index, source_pointer, pointer)?;
            }
        }

        Ok(ValidatedBorrowedCharNode {
            node: &self.node,
            _not_sync: PhantomData,
        })
    }

    /// Get or create a child for the given character.
    ///
    /// This convenience method panics only if the sealed adaptive topology
    /// rejects an absent key. Call [`Self::try_get_or_create_child`] to handle
    /// that structural-invariant failure explicitly.
    pub fn get_or_create_child(&mut self, c: char) -> &mut CharTrieNodeInner<V> {
        self.try_get_or_create_child(c).unwrap_or_else(|error| {
            panic!(
                "CharTrieNodeInner ownership invariant violated while creating absent key \
                 {:#x}: {error}",
                c as u32
            )
        })
    }

    /// Get or create a child while reporting adaptive insertion failure.
    ///
    /// The new allocation remains guarded through every fallible structural
    /// step. A successful slot publication transfers ownership before the
    /// returned mutable reference is formed; failure and unwind reclaim the
    /// allocation exactly once.
    pub fn try_get_or_create_child(
        &mut self,
        c: char,
    ) -> std::result::Result<&mut CharTrieNodeInner<V>, AddChildError> {
        let key = c as u32;

        // Check if child already exists
        if self.node.find_child(key).is_some() {
            // Child exists, return mutable reference
            return Ok(self.get_child_mut(c).expect("child should exist"));
        }

        let pending = PendingOwnedCharChild::new(CharTrieNodeInner::new());
        let swizzled = pending.swizzled_pointer();

        // Add to node, handling potential growth
        match self.node.add_child_growing(key, swizzled) {
            Ok(Some(grown)) => {
                let published = pending.publish();
                self.node = grown;
                // SAFETY: the exact allocation was just published into the
                // sealed topology, and `&mut self` excludes competing access.
                Ok(unsafe { &mut *published.as_ptr() })
            }
            Ok(None) => {
                let published = pending.publish();
                // SAFETY: same publication and exclusive-borrow argument as
                // the growth case above.
                Ok(unsafe { &mut *published.as_ptr() })
            }
            Err(error) => Err(error),
        }
    }

    /// Remove a child by character, returning the removed child if it existed
    pub fn remove_child(&mut self, c: char) -> Option<Box<CharTrieNodeInner<V>>> {
        let key = c as u32;

        // Check if child exists and get its pointer
        let ptr = self
            .node
            .find_child(key)
            .and_then(|p| p.as_ptr::<CharTrieNodeInner<V>>())?;

        // Remove from node
        if let Some((_, Some(new_node))) = self.node.remove_child_shrinking(key) {
            self.node = new_node;
        }

        // Safety: ptr was created via Box::into_raw()
        Some(unsafe { Box::from_raw(ptr as *mut CharTrieNodeInner<V>) })
    }

    /// Iterate over children
    ///
    /// Returns an iterator over `(char, &CharTrieNodeInner<V>)` pairs.
    pub fn iter_children(&self) -> impl Iterator<Item = (char, &CharTrieNodeInner<V>)> {
        self.node.iter_children().filter_map(|(key, ptr)| {
            ptr.as_ptr::<CharTrieNodeInner<V>>().map(|p| {
                let c = char::from_u32(key).unwrap_or('\u{FFFD}');
                // Safety: We control all SwizzledPtr creation; ptr is valid
                (c, unsafe { &*p })
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistent_artrie::char::nodes::{flags, CharCompressedPrefix};
    use crate::persistent_artrie::NodeType;
    use serde::{Deserialize, Serialize};
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::sync::atomic::{AtomicUsize, Ordering};

    const CI_DEEP_LIFECYCLE_DEPTH: usize = 32_768;
    const STRESS_LIFECYCLE_DEPTH: usize = 100_000;

    fn nonresident(pointer: SwizzledPtr) -> NonResidentCharChild {
        NonResidentCharChild::try_from(pointer).expect("test pointer is null or on disk")
    }

    #[test]
    fn test_char_trie_file_header() {
        let mut header = CharTrieFileHeader::new();
        header.root_ptr = 42;
        header.entry_count = 100;
        header.checkpoint_lsn = 50;

        let bytes = header.to_bytes_with_checksum();
        let header2 = CharTrieFileHeader::from_bytes(&bytes);

        assert_eq!(header2.magic, CHAR_TRIE_MAGIC);
        assert_eq!(header2.root_ptr, 42);
        assert_eq!(header2.entry_count, 100);
        assert_eq!(header2.checkpoint_lsn, 50);
        assert!(header2.verify_checksum());
    }

    #[test]
    fn test_char_trie_node_inner() {
        let mut root = CharTrieNodeInner::<i32>::new();
        assert!(!root.is_final());
        assert_eq!(root.num_children(), 0);

        // Insert a child
        let child = CharTrieNodeInner::new();
        root.insert_child('a', child)
            .expect("vacant resident insertion succeeds");
        assert_eq!(root.num_children(), 1);

        // Get child
        let c = root.get_child('a');
        assert!(c.is_some());

        // Get or create
        let child_mut = root.get_or_create_child('b');
        child_mut.set_final(true);
        assert_eq!(root.num_children(), 2);

        // Remove child
        let removed = root.remove_child('a');
        assert!(removed.is_some());
        assert_eq!(root.num_children(), 1);
    }

    fn exercise_deep_clone_and_drop(depth: usize) {
        let mut root = CharTrieNodeInner::<u64>::new();
        let mut cursor = &mut root;
        for index in 0..depth {
            cursor = cursor.get_or_create_child('b');
            cursor.value = Some(index as u64);
        }
        cursor.set_final(true);

        let cloned = root.clone();
        let mut cursor = &cloned;
        for index in 0..depth {
            cursor = cursor.get_child('b').expect("the cloned spine is complete");
            assert_eq!(cursor.value, Some(index as u64));
        }
        assert!(cursor.is_final());

        drop(cloned);
        drop(root);
    }

    #[test]
    fn char_trie_node_clone_and_drop_are_stack_safe_in_ci() {
        exercise_deep_clone_and_drop(CI_DEEP_LIFECYCLE_DEPTH);
    }

    #[test]
    #[ignore = "resource-bounded 100,000-node lifecycle stress case"]
    fn char_trie_node_clone_and_drop_are_stack_safe_at_extreme_depth() {
        exercise_deep_clone_and_drop(STRESS_LIFECYCLE_DEPTH);
    }

    #[test]
    fn char_trie_node_clone_preserves_metadata_and_mixed_pointer_kinds() {
        let mut root = CharTrieNodeInner::<u64> {
            node: CharNode::new_node16(),
            value: Some(41),
            _not_sync: PhantomData,
        };
        root.node.header_mut().prefix_len = 3;
        root.node.header_mut().flags = flags::IS_FINAL | flags::IS_DIRTY | flags::IS_LEAF;
        root.node.header_mut().version = 0xfeed_beef;
        *root.node.prefix_mut() = CharCompressedPrefix::from_chars(&[0x03bb, 0x03b9, 0x03b2]);
        if let CharNode::N16(node) = &mut root.node {
            node.value_ptr = SwizzledPtr::on_disk(91, 7, NodeType::CharNode16);
        }

        let disk_child = SwizzledPtr::on_disk(123, 5, NodeType::CharNode4);
        root.try_add_nonresident_child('d', nonresident(disk_child.clone()))
            .expect("valid disk child installs into an absent slot");
        let mut resident_child = CharTrieNodeInner::new();
        resident_child.value = Some(99);
        resident_child.get_or_create_child('x').set_final(true);
        root.insert_child('m', resident_child)
            .expect("vacant resident insertion succeeds");

        let cloned = root.clone();

        assert!(matches!(cloned.node, CharNode::N16(_)));
        assert_eq!(cloned.value, root.value);
        assert_eq!(cloned.node.header().node_type, root.node.header().node_type);
        assert_eq!(
            cloned.node.header().prefix_len,
            root.node.header().prefix_len
        );
        assert_eq!(cloned.node.header().flags, root.node.header().flags);
        assert_eq!(cloned.node.header()._padding, root.node.header()._padding);
        assert_eq!(
            cloned.node.header().num_children,
            root.node.header().num_children
        );
        assert_eq!(cloned.node.header()._padding2, root.node.header()._padding2);
        assert_eq!(cloned.node.header().version, root.node.header().version);
        assert_eq!(cloned.node.prefix().chars, root.node.prefix().chars);
        assert_eq!(
            cloned.node.find_child('d' as u32).map(SwizzledPtr::to_raw),
            Some(disk_child.to_raw())
        );

        let original_resident = root.get_child('m').expect("original resident child");
        let cloned_resident = cloned.get_child('m').expect("cloned resident child");
        assert_ne!(
            original_resident as *const CharTrieNodeInner<u64>,
            cloned_resident as *const CharTrieNodeInner<u64>
        );
        assert_eq!(cloned_resident.value, Some(99));
        assert!(cloned_resident
            .get_child('x')
            .expect("cloned grandchild")
            .is_final());

        let original_value_ptr = match &root.node {
            CharNode::N16(node) => node.value_ptr.to_raw(),
            _ => unreachable!(),
        };
        let cloned_value_ptr = match &cloned.node {
            CharNode::N16(node) => node.value_ptr.to_raw(),
            _ => unreachable!(),
        };
        assert_eq!(cloned_value_ptr, original_value_ptr);
    }

    fn exercise_pending_frontier(depth: usize) {
        let mut root = CharTrieNodeInner::<()>::new();
        let mut cursor = &mut root;
        for _ in 0..depth {
            cursor
                .insert_child('b', CharTrieNodeInner::new())
                .expect("vacant resident insertion succeeds");
            cursor = cursor.get_or_create_child('a');
        }

        drop(root);
    }

    #[test]
    fn char_trie_node_drop_spills_pending_frontier_in_ci() {
        exercise_pending_frontier(CI_DEEP_LIFECYCLE_DEPTH);
    }

    #[test]
    #[ignore = "resource-bounded 100,000-level branching lifecycle stress case"]
    fn char_trie_node_drop_spills_one_pending_sibling_per_level() {
        exercise_pending_frontier(STRESS_LIFECYCLE_DEPTH);
    }

    fn exact_shell(node: CharNode) -> CharTrieNodeInner<u64> {
        CharTrieNodeInner {
            node,
            value: Some(7),
            _not_sync: PhantomData,
        }
    }

    fn assert_variant_clone(mut original: CharTrieNodeInner<u64>) {
        original
            .try_add_nonresident_child(
                'd',
                nonresident(SwizzledPtr::on_disk(17, 9, NodeType::CharNode4)),
            )
            .expect("valid disk child installs into an absent slot");
        let mut child = CharTrieNodeInner::new();
        child.value = Some(11);
        original
            .insert_child('m', child)
            .expect("vacant resident insertion succeeds");

        let cloned = original.clone();
        assert_eq!(
            cloned.node.header().node_type,
            original.node.header().node_type
        );
        assert_eq!(cloned.node.num_children(), original.node.num_children());
        assert_eq!(
            cloned.node.find_child('d' as u32).map(SwizzledPtr::to_raw),
            original
                .node
                .find_child('d' as u32)
                .map(SwizzledPtr::to_raw)
        );
        assert_ne!(
            cloned.get_child('m').expect("cloned resident child") as *const _,
            original.get_child('m').expect("original resident child") as *const _
        );
        assert_eq!(cloned.get_child('m').and_then(|node| node.value), Some(11));
    }

    #[test]
    fn char_trie_node_clone_preserves_every_adaptive_node_variant() {
        assert_variant_clone(exact_shell(CharNode::new_node4()));
        assert_variant_clone(exact_shell(CharNode::new_node16()));
        assert_variant_clone(exact_shell(CharNode::new_node48()));
        assert_variant_clone(exact_shell(CharNode::new_bucket()));
    }

    #[test]
    fn char_trie_node_clone_preserves_wide_bucket_layout_and_mixed_topology() {
        const FIRST_KEY: u32 = 0x1000;
        const CHILDREN: u32 = 65;
        const DEEP_INDEX: u32 = 2;

        let mut original = CharTrieNodeInner::<u64>::new();
        for index in 0..CHILDREN {
            let character =
                char::from_u32(FIRST_KEY + index).expect("test key is a Unicode scalar");
            match index % 11 {
                0 => {
                    original
                        .try_add_nonresident_child(character, nonresident(SwizzledPtr::null()))
                        .expect("null placeholder installs into an absent slot");
                }
                1 => {
                    original
                        .try_add_nonresident_child(
                            character,
                            nonresident(SwizzledPtr::on_disk(
                                1000 + index,
                                index,
                                NodeType::CharNode4,
                            )),
                        )
                        .expect("valid disk child installs into an absent slot");
                }
                _ => {
                    let mut child = CharTrieNodeInner::new();
                    child.value = Some(index as u64);
                    original
                        .insert_child(character, child)
                        .expect("vacant resident insertion succeeds");
                }
            }
        }

        let deep_key = char::from_u32(FIRST_KEY + DEEP_INDEX).expect("test key is valid");
        {
            let deep_child = original
                .get_child_mut(deep_key)
                .expect("the selected resident child exists");
            deep_child
                .try_add_nonresident_child('n', nonresident(SwizzledPtr::null()))
                .expect("null placeholder installs into an absent slot");
            deep_child
                .try_add_nonresident_child(
                    'o',
                    nonresident(SwizzledPtr::on_disk(9001, 17, NodeType::CharNode16)),
                )
                .expect("valid disk child installs into an absent slot");
            deep_child.get_or_create_child('p').value = Some(9002);
        }

        let (original_capacity, original_order) = match &original.node {
            CharNode::Bucket(bucket) => (
                bucket.entries.capacity(),
                bucket.entries.keys().copied().collect::<Vec<_>>(),
            ),
            node => panic!("65 children must use CharBucket, got {node:?}"),
        };

        let cloned = original.clone();
        let (cloned_capacity, cloned_order) = match &cloned.node {
            CharNode::Bucket(bucket) => (
                bucket.entries.capacity(),
                bucket.entries.keys().copied().collect::<Vec<_>>(),
            ),
            node => panic!("the clone must retain CharBucket, got {node:?}"),
        };
        assert_eq!(cloned.num_children(), CHILDREN as usize);
        assert_eq!(cloned_capacity, original_capacity);
        assert_eq!(cloned_order, original_order);

        for key in original_order {
            let character = char::from_u32(key).expect("test key is a Unicode scalar");
            let source = original
                .node
                .find_child(key)
                .expect("source key remains present");
            let target = cloned
                .node
                .find_child(key)
                .expect("cloned key remains present");
            match original.get_child(character) {
                Some(source_child) => {
                    let target_child = cloned
                        .get_child(character)
                        .expect("resident source children remain resident");
                    assert_ne!(source_child as *const _, target_child as *const _);
                    assert_eq!(target_child.value, source_child.value);
                }
                None => assert_eq!(target.to_raw(), source.to_raw()),
            }
        }

        let cloned_deep = cloned
            .get_child(deep_key)
            .expect("deep resident child was cloned");
        let original_deep = original
            .get_child(deep_key)
            .expect("deep resident source child remains present");
        assert!(cloned_deep
            .node
            .find_child('n' as u32)
            .expect("null child key remains present")
            .is_null());
        assert_eq!(
            cloned_deep
                .node
                .find_child('o' as u32)
                .map(SwizzledPtr::to_raw),
            original_deep
                .node
                .find_child('o' as u32)
                .map(SwizzledPtr::to_raw)
        );
        assert_eq!(
            cloned_deep.get_child('p').and_then(|node| node.value),
            Some(9002)
        );
    }

    #[test]
    fn nonresident_child_insertion_rejects_resident_pointer_without_publication() {
        let root = CharTrieNodeInner::<()>::new();
        let borrowed = CharTrieNodeInner::<()>::new();
        let forged_resident = SwizzledPtr::in_memory(std::ptr::from_ref(&borrowed));

        let rejected = NonResidentCharChild::try_from(forged_resident)
            .expect_err("resident pointer must not enter the nonresident type");
        assert!(rejected.pointer.is_swizzled());
        assert_eq!(root.num_children(), 0);
        assert!(root.node.find_child('x' as u32).is_none());
    }

    #[test]
    fn nonresident_child_type_rejects_transitional_and_malformed_disk_states() {
        const SWIZZLE_FLAG: u64 = 1 << 63;
        for raw in [SWIZZLE_FLAG | 1, SWIZZLE_FLAG | 2, 0x3_ffff] {
            let rejected = NonResidentCharChild::try_from(SwizzledPtr::from_raw(raw))
                .expect_err("transitional or malformed disk state must be rejected");
            assert_eq!(rejected.pointer.to_raw(), raw);
        }
    }

    #[test]
    fn nonresident_child_insertion_is_failure_atomic_for_duplicate_keys() {
        let mut root = CharTrieNodeInner::<()>::new();
        let original = SwizzledPtr::on_disk(17, 9, NodeType::CharNode4);
        let original_raw = original.to_raw();
        root.try_add_nonresident_child('x', nonresident(original))
            .expect("first child installs");

        let replacement = SwizzledPtr::on_disk(23, 11, NodeType::CharNode16);
        let replacement_raw = replacement.to_raw();
        let error = root
            .try_add_nonresident_child('x', nonresident(replacement))
            .expect_err("duplicate key is rejected");
        assert_eq!(error.source, AddChildError::KeyExists);
        assert_eq!(error.child.into_pointer().to_raw(), replacement_raw);
        assert_eq!(root.num_children(), 1);
        assert_eq!(
            root.node.find_child('x' as u32).map(SwizzledPtr::to_raw),
            Some(original_raw)
        );
    }

    #[test]
    fn serialization_validation_borrows_the_exact_projected_node() {
        let mut root = CharTrieNodeInner::<()>::new();
        let child = SwizzledPtr::on_disk(17, 9, NodeType::CharNode16);
        root.try_add_nonresident_child('x', nonresident(child.clone()))
            .expect("durable child installs");
        let resolved = [('x' as u32, child)];

        let validated = root
            .validated_node_for_serialization(&resolved)
            .expect("the exact ordered durable view validates");

        assert!(std::ptr::eq(validated.as_node(), &root.node));
        assert_eq!(
            validated
                .as_node()
                .find_child('x' as u32)
                .map(SwizzledPtr::to_raw),
            Some(resolved[0].1.to_raw())
        );
    }

    #[test]
    fn serialization_validation_rejects_a_different_durable_child_location() {
        let mut root = CharTrieNodeInner::<()>::new();
        let sealed = SwizzledPtr::on_disk(17, 9, NodeType::CharNode16);
        let sealed_raw = sealed.to_raw();
        root.try_add_nonresident_child('x', nonresident(sealed))
            .expect("durable child installs");
        let resolved = [(
            'x' as u32,
            SwizzledPtr::on_disk(17, 10, NodeType::CharNode16),
        )];
        let resolved_raw = resolved[0].1.to_raw();

        let error = root
            .validated_node_for_serialization(&resolved)
            .expect_err("a different durable address must not be borrowed");

        assert_eq!(
            error,
            EncodedCharNodeError::ChildPointerMismatch {
                index: 0,
                expected_raw: sealed_raw,
                actual_raw: resolved_raw,
            }
        );
    }

    fn projected_node_with_children(
        count: usize,
    ) -> (CharTrieNodeInner<()>, Vec<(u32, SwizzledPtr)>) {
        let mut root = CharTrieNodeInner::<()>::new();
        let mut resolved = Vec::with_capacity(count);
        for index in 0..count {
            let key = u32::try_from(index).expect("test key fits u32");
            let character = char::from_u32(key).expect("test key is a Unicode scalar");
            let pointer = SwizzledPtr::on_disk(
                100 + u32::try_from(index / 64).expect("test block fits u32"),
                u32::try_from(index).expect("test offset fits u32"),
                match index % 4 {
                    0 => NodeType::CharNode4,
                    1 => NodeType::CharNode16,
                    2 => NodeType::CharNode48,
                    _ => NodeType::CharBucket,
                },
            );
            root.try_add_nonresident_child(character, nonresident(pointer.clone()))
                .expect("canonical durable child installs");
            resolved.push((key, pointer));
        }
        (root, resolved)
    }

    #[test]
    fn serialization_validation_accepts_all_adaptive_representations_and_dense_buckets() {
        for count in [0, 4, 16, 48, 49, 256] {
            let (root, resolved) = projected_node_with_children(count);
            let validated = root
                .validated_node_for_serialization(&resolved)
                .expect("exact canonical view validates independently of bucket hash order");
            assert!(std::ptr::eq(validated.as_node(), &root.node));
            assert_eq!(validated.as_node().num_children(), count);
            match count {
                0..=4 => assert!(matches!(validated.as_node(), CharNode::N4(_))),
                5..=16 => assert!(matches!(validated.as_node(), CharNode::N16(_))),
                17..=48 => assert!(matches!(validated.as_node(), CharNode::N48(_))),
                _ => assert!(matches!(validated.as_node(), CharNode::Bucket(_))),
            }
        }
    }

    #[test]
    fn serialization_validation_rejects_noncanonical_bucket_keys() {
        let (root, mut resolved) = projected_node_with_children(49);
        resolved.swap(7, 8);

        assert!(matches!(
            root.validated_node_for_serialization(&resolved),
            Err(EncodedCharNodeError::NonCanonicalChildOrder { index: 8, .. })
        ));
    }

    #[test]
    fn serialization_validation_checks_fixed_capacity_before_cursor_indexing() {
        let mut root = CharTrieNodeInner::<()>::new();
        root.node.header_mut().num_children = 5;
        let resolved = projected_node_with_children(5).1;
        let error = root
            .validated_node_for_serialization(&resolved)
            .expect_err("an N4 cursor must never index beyond its physical capacity");
        root.node.header_mut().num_children = 0;

        assert_eq!(
            error,
            EncodedCharNodeError::RepresentationCapacityExceeded {
                node_type: NodeType::CharNode4,
                count: 5,
                capacity: 4,
            }
        );
    }

    #[test]
    fn serialization_validation_rejects_bucket_header_entry_divergence() {
        let (mut root, resolved) = projected_node_with_children(49);
        root.node.header_mut().num_children = 48;

        assert_eq!(
            root.validated_node_for_serialization(&resolved)
                .expect_err("bucket header and physical entry counts must agree"),
            EncodedCharNodeError::BucketCardinalityMismatch {
                header: 48,
                entries: 49,
            }
        );
    }

    fn assert_nonresident_replacement_preserves_variant(mut root: CharTrieNodeInner<u64>) {
        let representation = std::mem::discriminant(&root.node);
        root.try_add_nonresident_child('n', nonresident(SwizzledPtr::null()))
            .expect("null child installs");
        root.try_add_nonresident_child(
            'd',
            nonresident(SwizzledPtr::on_disk(31, 7, NodeType::CharNode4)),
        )
        .expect("disk child installs");
        let child_count = root.num_children();

        let mut first = CharTrieNodeInner::new();
        first.value = Some(1);
        let mut second = CharTrieNodeInner::new();
        second.value = Some(2);
        assert!(root
            .insert_child('n', first)
            .expect("null-to-resident replacement succeeds")
            .is_none());
        assert!(root
            .insert_child('d', second)
            .expect("disk-to-resident replacement succeeds")
            .is_none());

        assert_eq!(std::mem::discriminant(&root.node), representation);
        assert_eq!(root.num_children(), child_count);
        assert_eq!(root.get_child('n').and_then(|child| child.value), Some(1));
        assert_eq!(root.get_child('d').and_then(|child| child.value), Some(2));
    }

    #[test]
    fn nonresident_to_resident_replacement_preserves_every_adaptive_variant() {
        assert_nonresident_replacement_preserves_variant(exact_shell(CharNode::new_node4()));
        assert_nonresident_replacement_preserves_variant(exact_shell(CharNode::new_node16()));
        assert_nonresident_replacement_preserves_variant(exact_shell(CharNode::new_node48()));
        assert_nonresident_replacement_preserves_variant(exact_shell(CharNode::new_bucket()));
    }

    static PENDING_CHILD_DROPS: AtomicUsize = AtomicUsize::new(0);

    #[derive(Clone, Debug, Default, Serialize, Deserialize)]
    struct PendingChildDropProbe;

    impl Drop for PendingChildDropProbe {
        fn drop(&mut self) {
            PENDING_CHILD_DROPS.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl DictionaryValue for PendingChildDropProbe {}

    #[test]
    fn pending_owned_child_reclaims_or_returns_exactly_once() {
        PENDING_CHILD_DROPS.store(0, Ordering::SeqCst);

        let mut dropped = CharTrieNodeInner::new();
        dropped.value = Some(PendingChildDropProbe);
        let pending = PendingOwnedCharChild::new(dropped);
        let raw = pending.pointer().as_ptr();
        assert_eq!(
            pending
                .swizzled_pointer()
                .as_ptr::<CharTrieNodeInner<PendingChildDropProbe>>(),
            Some(raw.cast_const())
        );
        drop(pending);
        assert_eq!(PENDING_CHILD_DROPS.load(Ordering::SeqCst), 1);

        let mut returned = CharTrieNodeInner::new();
        returned.value = Some(PendingChildDropProbe);
        let returned = PendingOwnedCharChild::new(returned).into_child();
        assert_eq!(PENDING_CHILD_DROPS.load(Ordering::SeqCst), 1);
        drop(returned);
        assert_eq!(PENDING_CHILD_DROPS.load(Ordering::SeqCst), 2);
    }

    fn fill_to_growth_boundary(root: &mut CharTrieNodeInner<PendingChildDropProbe>, boundary: u32) {
        for index in 0..boundary {
            let key = char::from_u32(0x1000 + index).expect("fixture key is a Unicode scalar");
            root.try_get_or_create_child(key)
                .expect("distinct fixture child must be inserted");
        }
    }

    #[test]
    fn resident_publication_preserves_provenance_across_growth_boundaries() {
        PENDING_CHILD_DROPS.store(0, Ordering::SeqCst);
        let mut expected_drops = 0;

        for (case, boundary, expected_type) in [
            (0u32, 4u32, NodeType::CharNode16),
            (1, 16, NodeType::CharNode48),
            (2, 48, NodeType::CharBucket),
        ] {
            let mut root = CharTrieNodeInner::new();
            fill_to_growth_boundary(&mut root, boundary);
            let key = char::from_u32(0x2000 + case).expect("fixture key is a Unicode scalar");
            let published = root
                .try_get_or_create_child(key)
                .expect("growth-boundary child must be inserted");
            published.value = Some(PendingChildDropProbe);
            let published = std::ptr::from_mut(published);
            assert_eq!(root.node.representation_type(), expected_type);

            let removed = root
                .remove_child(key)
                .expect("published child is removable");
            assert_eq!(std::ptr::from_ref(removed.as_ref()), published.cast_const());
            drop(removed);
            expected_drops += 1;
            assert_eq!(PENDING_CHILD_DROPS.load(Ordering::SeqCst), expected_drops);
        }

        for (case, boundary, expected_type) in [
            (0u32, 4u32, NodeType::CharNode16),
            (1, 16, NodeType::CharNode48),
            (2, 48, NodeType::CharBucket),
        ] {
            let mut root = CharTrieNodeInner::new();
            fill_to_growth_boundary(&mut root, boundary);
            let key = char::from_u32(0x3000 + case).expect("fixture key is a Unicode scalar");
            let mut child = CharTrieNodeInner::new();
            child.value = Some(PendingChildDropProbe);
            assert!(root
                .insert_child(key, child)
                .expect("growth-boundary resident insertion must succeed")
                .is_none());
            assert_eq!(root.node.representation_type(), expected_type);
            let published = std::ptr::from_ref(
                root.get_child(key)
                    .expect("inserted child remains addressable"),
            );

            let removed = root
                .remove_child(key)
                .expect("published child is removable");
            assert_eq!(std::ptr::from_ref(removed.as_ref()), published);
            drop(removed);
            expected_drops += 1;
            assert_eq!(PENDING_CHILD_DROPS.load(Ordering::SeqCst), expected_drops);
        }

        assert_eq!(expected_drops, 6);
    }

    static LIFECYCLE_VALUES_CREATED: AtomicUsize = AtomicUsize::new(0);
    static LIFECYCLE_VALUES_DROPPED: AtomicUsize = AtomicUsize::new(0);
    static PANIC_ON_CLONE_ID: AtomicUsize = AtomicUsize::new(usize::MAX);
    static DROP_ORDER: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());

    #[derive(Debug, Serialize, Deserialize)]
    struct LifecycleValue {
        id: usize,
    }

    impl LifecycleValue {
        fn new(id: usize) -> Self {
            LIFECYCLE_VALUES_CREATED.fetch_add(1, Ordering::SeqCst);
            Self { id }
        }
    }

    impl Default for LifecycleValue {
        fn default() -> Self {
            Self::new(usize::MAX)
        }
    }

    impl Clone for LifecycleValue {
        fn clone(&self) -> Self {
            assert_ne!(
                self.id,
                PANIC_ON_CLONE_ID.load(Ordering::SeqCst),
                "injected value-clone failure"
            );
            Self::new(self.id)
        }
    }

    impl Drop for LifecycleValue {
        fn drop(&mut self) {
            LIFECYCLE_VALUES_DROPPED.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl DictionaryValue for LifecycleValue {}

    #[derive(Clone, Debug, Default, Serialize, Deserialize)]
    struct OrderedDropValue {
        id: usize,
    }

    impl Drop for OrderedDropValue {
        fn drop(&mut self) {
            DROP_ORDER
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(self.id);
        }
    }

    impl DictionaryValue for OrderedDropValue {}

    #[test]
    fn char_trie_node_drop_preserves_recursive_postorder_semantics() {
        DROP_ORDER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();

        let mut root = CharTrieNodeInner::new();
        root.value = Some(OrderedDropValue { id: 0 });
        let first = root.get_or_create_child('a');
        first.value = Some(OrderedDropValue { id: 1 });
        first.get_or_create_child('x').value = Some(OrderedDropValue { id: 2 });
        root.get_or_create_child('b').value = Some(OrderedDropValue { id: 3 });

        drop(root);

        let observed = std::mem::take(
            &mut *DROP_ORDER
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        assert_eq!(observed, [2, 1, 3, 0]);
    }

    #[test]
    fn char_trie_node_partial_clone_unwind_reclaims_every_completed_value_once() {
        LIFECYCLE_VALUES_CREATED.store(0, Ordering::SeqCst);
        LIFECYCLE_VALUES_DROPPED.store(0, Ordering::SeqCst);
        PANIC_ON_CLONE_ID.store(2, Ordering::SeqCst);

        let mut original = CharTrieNodeInner::new();
        original.value = Some(LifecycleValue::new(0));
        let child = original.get_or_create_child('a');
        child.value = Some(LifecycleValue::new(1));
        child.get_or_create_child('b').value = Some(LifecycleValue::new(2));

        let result = catch_unwind(AssertUnwindSafe(|| original.clone()));
        assert!(result.is_err());
        PANIC_ON_CLONE_ID.store(usize::MAX, Ordering::SeqCst);

        assert_eq!(LIFECYCLE_VALUES_CREATED.load(Ordering::SeqCst), 5);
        assert_eq!(LIFECYCLE_VALUES_DROPPED.load(Ordering::SeqCst), 2);
        assert_eq!(original.value.as_ref().map(|value| value.id), Some(0));
        assert_eq!(
            original
                .get_child('a')
                .and_then(|node| node.get_child('b'))
                .and_then(|node| node.value.as_ref())
                .map(|value| value.id),
            Some(2)
        );

        drop(original);
        assert_eq!(
            LIFECYCLE_VALUES_DROPPED.load(Ordering::SeqCst),
            LIFECYCLE_VALUES_CREATED.load(Ordering::SeqCst)
        );
    }

    #[allow(dead_code)]
    fn char_trie_owned_projection_remains_send() {
        fn assert_send<T: Send>() {}
        assert_send::<CharTrieNodeInner<()>>();
    }
}
