//! Swizzled pointer implementation for transparent memory/disk addressing.
//!
//! A swizzled pointer uses a single 64-bit value to represent either:
//! - A memory pointer (when the node is loaded in RAM)
//! - A disk reference (block_id + offset when the node is on disk)
//!
//! The MSB (bit 63) discriminates between the two states:
//! - MSB = 1: Memory pointer (remaining 63 bits are the address)
//! - MSB = 0: Disk reference (encoded block_id + offset + flags)
//!
//! This design enables lazy loading: start with disk references, swizzle to
//! memory pointers on first access, and the transition is atomic.

use std::sync::atomic::{AtomicU64, Ordering};

use super::error::SwizzleError;

/// The swizzle flag is the MSB (bit 63).
/// When set, the pointer is in memory; when clear, it's a disk reference.
const SWIZZLE_FLAG: u64 = 1 << 63;

/// Mask to extract the memory address (clear the MSB).
const PTR_MASK: u64 = !SWIZZLE_FLAG;

/// Bit layout for disk references (when MSB = 0):
/// - Bits 62-40: Block ID (23 bits = 8M blocks)
/// - Bits 39-18: Offset within block (22 bits = 4MB offset)
/// - Bits 17-0: Flags including node type (18 bits)
const BLOCK_ID_SHIFT: u32 = 40;
const OFFSET_SHIFT: u32 = 18;
const BLOCK_ID_BITS: u32 = 23;
const OFFSET_BITS: u32 = 22;
const FLAGS_BITS: u32 = 18;

const BLOCK_ID_MASK: u64 = (1 << BLOCK_ID_BITS) - 1; // 0x7FFFFF (23 bits)
const OFFSET_MASK: u64 = (1 << OFFSET_BITS) - 1; // 0x3FFFFF (22 bits)
const FLAGS_MASK: u64 = (1 << FLAGS_BITS) - 1; // 0x3FFFF (18 bits)

/// Maximum block ID (8M - 1).
pub const MAX_BLOCK_ID: u32 = (1 << BLOCK_ID_BITS) - 1;

/// Maximum offset within a block (4MB - 1).
pub const MAX_OFFSET: u32 = (1 << OFFSET_BITS) - 1;

/// Node type identifiers stored in the flags field.
///
/// # Byte-Level Nodes (0-99)
///
/// These are used by `PersistentARTrie` with u8 keys:
/// - `Node4`: 1-4 children, linear scan
/// - `Node16`: 5-16 children, SSE4.1 SIMD
/// - `Node48`: 17-48 children, indexed lookup
/// - `Node256`: 49-256 children, direct array
/// - `Bucket`: Leaf bucket with strings
///
/// # Char-Level Nodes (100-199)
///
/// These are used by `PersistentARTrieChar` with u32 keys:
/// - `CharNode4`: 1-4 children, linear scan
/// - `CharNode16`: 5-16 children, AVX2 SIMD (8×u32)
/// - `CharNode48`: 17-48 children, binary search
/// - `CharBucket`: >48 children, HashMap-like
///
/// Note: CharNode256 is impossible for u32 keys (would require 4GB array).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum NodeType {
    // === Byte-Level Nodes (0-99) ===
    /// Node with 1-4 children, linear scan lookup (byte-level).
    Node4 = 4,
    /// Node with 5-16 children, SIMD lookup (byte-level).
    Node16 = 16,
    /// Node with 17-48 children, indexed lookup (byte-level).
    Node48 = 48,
    /// Node with 49-256 children, direct array lookup (byte-level).
    Node256 = 0, // Use 0 since 256 doesn't fit in u8 nicely
    /// Leaf bucket containing multiple strings (byte-level).
    Bucket = 1,

    // === Char-Level Nodes (100-199) ===
    /// Char node with 1-4 children, linear scan lookup (char-level).
    CharNode4 = 104,
    /// Char node with 5-16 children, AVX2 SIMD lookup (char-level).
    CharNode16 = 116,
    /// Char node with 17-48 children, binary search lookup (char-level).
    CharNode48 = 148,
    /// Char bucket with >48 children, HashMap-like (char-level).
    CharBucket = 101,
}

impl NodeType {
    /// Convert from u8, returning None for invalid values.
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            // Byte-level nodes
            4 => Some(NodeType::Node4),
            16 => Some(NodeType::Node16),
            48 => Some(NodeType::Node48),
            0 => Some(NodeType::Node256),
            1 => Some(NodeType::Bucket),
            // Char-level nodes
            104 => Some(NodeType::CharNode4),
            116 => Some(NodeType::CharNode16),
            148 => Some(NodeType::CharNode48),
            101 => Some(NodeType::CharBucket),
            _ => None,
        }
    }

    /// Check if this is a byte-level node type.
    #[inline]
    pub fn is_byte_level(&self) -> bool {
        matches!(
            self,
            NodeType::Node4
                | NodeType::Node16
                | NodeType::Node48
                | NodeType::Node256
                | NodeType::Bucket
        )
    }

    /// Check if this is a char-level node type.
    #[inline]
    pub fn is_char_level(&self) -> bool {
        matches!(
            self,
            NodeType::CharNode4
                | NodeType::CharNode16
                | NodeType::CharNode48
                | NodeType::CharBucket
        )
    }
}

impl TryFrom<u8> for NodeType {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        NodeType::from_u8(value).ok_or(())
    }
}

/// A swizzled pointer that can represent either a memory address or a disk location.
///
/// This is the core mechanism for lazy loading: nodes start as disk references
/// and are swizzled to memory pointers when first accessed.
///
/// # Thread Safety
///
/// All operations use atomic instructions with appropriate memory ordering.
/// Multiple threads can safely race to swizzle the same pointer; only one
/// will succeed, and all will observe the same final state.
#[derive(Debug)]
#[repr(transparent)]
pub struct SwizzledPtr(AtomicU64);

impl SwizzledPtr {
    /// Create a null pointer (neither in memory nor on disk).
    pub const fn null() -> Self {
        Self(AtomicU64::new(0))
    }

    /// Create a new unswizzled (on-disk) pointer.
    ///
    /// # Arguments
    ///
    /// * `block_id` - The block containing the node (max 8M - 1)
    /// * `offset` - Offset within the block (max 4MB - 1)
    /// * `node_type` - The type of node at this location
    ///
    /// # Panics
    ///
    /// Panics in debug mode if block_id or offset exceed their maximum values.
    pub fn on_disk(block_id: u32, offset: u32, node_type: NodeType) -> Self {
        debug_assert!(
            block_id <= MAX_BLOCK_ID,
            "block_id {} exceeds maximum {}",
            block_id,
            MAX_BLOCK_ID
        );
        debug_assert!(
            offset <= MAX_OFFSET,
            "offset {} exceeds maximum {}",
            offset,
            MAX_OFFSET
        );

        let encoded = ((block_id as u64 & BLOCK_ID_MASK) << BLOCK_ID_SHIFT)
            | ((offset as u64 & OFFSET_MASK) << OFFSET_SHIFT)
            | (node_type as u64 & FLAGS_MASK);

        debug_assert!(
            encoded & SWIZZLE_FLAG == 0,
            "disk reference must not have swizzle flag set"
        );

        Self(AtomicU64::new(encoded))
    }

    /// Create a new swizzled (in-memory) pointer.
    ///
    /// # Safety
    ///
    /// The pointer must be valid and have bit 63 clear (which is true for
    /// all user-space pointers on modern 64-bit systems).
    ///
    /// # Panics
    ///
    /// Panics if the pointer has bit 63 set (which would conflict with the swizzle flag).
    pub fn in_memory<T>(ptr: *const T) -> Self {
        let addr = ptr as u64;
        assert!(
            addr & SWIZZLE_FLAG == 0,
            "pointer address has bit 63 set, which conflicts with swizzle flag"
        );
        Self(AtomicU64::new(addr | SWIZZLE_FLAG))
    }

    /// Check if this pointer is null.
    #[inline]
    pub fn is_null(&self) -> bool {
        self.0.load(Ordering::Acquire) == 0
    }

    /// Check if this pointer is swizzled (pointing to memory).
    #[inline]
    pub fn is_swizzled(&self) -> bool {
        self.0.load(Ordering::Acquire) & SWIZZLE_FLAG != 0
    }

    /// Check if this pointer is unswizzled (pointing to disk).
    #[inline]
    pub fn is_on_disk(&self) -> bool {
        let val = self.0.load(Ordering::Acquire);
        val != 0 && val & SWIZZLE_FLAG == 0
    }

    /// Get the memory pointer (fast path for swizzled pointers).
    ///
    /// # Safety
    ///
    /// The caller must ensure that `is_swizzled()` returns true before calling this.
    /// The returned pointer is valid as long as the node is not evicted.
    #[inline]
    pub unsafe fn as_ptr_unchecked<T>(&self) -> *const T {
        let val = self.0.load(Ordering::Acquire);
        debug_assert!(val & SWIZZLE_FLAG != 0, "pointer is not swizzled");
        (val & PTR_MASK) as *const T
    }

    /// Get the memory pointer, returning None if not swizzled.
    #[inline]
    pub fn as_ptr<T>(&self) -> Option<*const T> {
        let val = self.0.load(Ordering::Acquire);
        if val & SWIZZLE_FLAG != 0 {
            Some((val & PTR_MASK) as *const T)
        } else {
            None
        }
    }

    /// Decode the disk location from an unswizzled pointer.
    ///
    /// Returns None if the pointer is swizzled (in memory) or null.
    pub fn disk_location(&self) -> Option<DiskLocation> {
        let val = self.0.load(Ordering::Acquire);
        if val == 0 || val & SWIZZLE_FLAG != 0 {
            return None;
        }

        let block_id = ((val >> BLOCK_ID_SHIFT) & BLOCK_ID_MASK) as u32;
        let offset = ((val >> OFFSET_SHIFT) & OFFSET_MASK) as u32;
        let type_byte = (val & FLAGS_MASK) as u8;
        let node_type = NodeType::from_u8(type_byte)?;

        Some(DiskLocation {
            block_id,
            offset,
            node_type,
        })
    }

    /// Atomically swizzle: replace a disk reference with a memory pointer.
    ///
    /// This operation is atomic and thread-safe. If multiple threads race to
    /// swizzle the same pointer, only one will succeed.
    ///
    /// # Arguments
    ///
    /// * `ptr` - The memory pointer to swizzle to
    ///
    /// # Returns
    ///
    /// - `Ok(())` if the swizzle succeeded
    /// - `Err(AlreadySwizzled)` if the pointer was already in memory
    /// - `Err(RaceCondition)` if another thread swizzled first
    pub fn swizzle<T>(&self, ptr: *const T) -> Result<(), SwizzleError> {
        let old = self.0.load(Ordering::Acquire);

        // Already swizzled?
        if old & SWIZZLE_FLAG != 0 {
            return Err(SwizzleError::AlreadySwizzled);
        }

        // Null pointer?
        if old == 0 {
            return Err(SwizzleError::AlreadyUnswizzled);
        }

        let addr = ptr as u64;
        assert!(addr & SWIZZLE_FLAG == 0, "pointer address has bit 63 set");

        let new = addr | SWIZZLE_FLAG;

        self.0
            .compare_exchange(old, new, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| SwizzleError::RaceCondition)
    }

    /// Atomically unswizzle: replace a memory pointer with a disk reference.
    ///
    /// This is used during eviction to convert an in-memory node back to
    /// a disk reference.
    ///
    /// # Arguments
    ///
    /// * `block_id` - The block where the node is stored
    /// * `offset` - Offset within the block
    /// * `node_type` - The type of node
    ///
    /// # Returns
    ///
    /// - `Ok(old_ptr)` with the previous memory pointer if successful
    /// - `Err(AlreadyUnswizzled)` if the pointer was already on disk
    /// - `Err(RaceCondition)` if another thread modified the pointer
    pub fn unswizzle<T>(
        &self,
        block_id: u32,
        offset: u32,
        node_type: NodeType,
    ) -> Result<*const T, SwizzleError> {
        if block_id > MAX_BLOCK_ID {
            return Err(SwizzleError::BlockIdOverflow { block_id });
        }
        if offset > MAX_OFFSET {
            return Err(SwizzleError::OffsetOverflow { offset });
        }

        let old = self.0.load(Ordering::Acquire);

        // Not swizzled?
        if old & SWIZZLE_FLAG == 0 {
            return Err(SwizzleError::AlreadyUnswizzled);
        }

        let new = ((block_id as u64 & BLOCK_ID_MASK) << BLOCK_ID_SHIFT)
            | ((offset as u64 & OFFSET_MASK) << OFFSET_SHIFT)
            | (node_type as u64 & FLAGS_MASK);

        self.0
            .compare_exchange(old, new, Ordering::AcqRel, Ordering::Acquire)
            .map(|v| (v & PTR_MASK) as *const T)
            .map_err(|_| SwizzleError::RaceCondition)
    }

    /// Get the raw u64 value for serialization.
    ///
    /// This returns the internal representation which can be stored
    /// and later restored with `from_raw`.
    pub fn to_raw(&self) -> u64 {
        self.0.load(Ordering::Acquire)
    }

    /// Create a SwizzledPtr from a raw u64 value.
    ///
    /// This is the inverse of `to_raw` and is used for deserialization.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `raw` was produced by a previous
    /// call to `to_raw` and represents a valid pointer state.
    pub fn from_raw(raw: u64) -> Self {
        Self(AtomicU64::new(raw))
    }

    /// Convert to ArenaSlot for relative encoding.
    ///
    /// This extracts the logical (arena_id, slot_id) from a disk reference
    /// where block_id maps to arena_id (arena N = block N+1) and offset
    /// is used to store slot_id.
    ///
    /// Returns None if the pointer is swizzled (in memory) or null.
    ///
    /// # Mapping
    ///
    /// - arena_id = block_id - 1 (block 0 is header, arenas start at block 1)
    /// - slot_id = offset field (repurposed for slot-based addressing)
    pub fn as_arena_slot(&self) -> Option<super::arena_slot::ArenaSlot> {
        let loc = self.disk_location()?;
        // Arena N is stored in Block N+1
        let arena_id = loc.block_id.checked_sub(1)?;
        Some(super::arena_slot::ArenaSlot::new(arena_id, loc.offset))
    }

    /// Create a SwizzledPtr from an ArenaSlot.
    ///
    /// This creates a disk reference from a logical (arena_id, slot_id) pair.
    ///
    /// # Mapping
    ///
    /// - block_id = arena_id + 1 (arena N is stored in block N+1)
    /// - offset = slot_id (slot-based addressing)
    ///
    /// # Panics
    ///
    /// Panics in debug mode if arena_id + 1 exceeds MAX_BLOCK_ID or
    /// slot_id exceeds MAX_OFFSET.
    pub fn from_arena_slot(slot: super::arena_slot::ArenaSlot, node_type: NodeType) -> Self {
        // Arena N is stored in Block N+1
        let block_id = slot.arena_id.saturating_add(1);
        Self::on_disk(block_id, slot.slot_id, node_type)
    }

    // =========================================================================
    // Lock-Free CAS Operations for Concurrent Access
    // =========================================================================

    /// Atomically load the raw pointer value.
    ///
    /// Uses Acquire ordering to ensure all prior writes are visible.
    #[inline]
    pub fn load_raw(&self) -> u64 {
        self.0.load(Ordering::Acquire)
    }

    /// Atomically store a raw pointer value.
    ///
    /// Uses Release ordering to ensure all prior writes are visible to
    /// subsequent reads.
    #[inline]
    pub fn store_raw(&self, value: u64) {
        self.0.store(value, Ordering::Release)
    }

    /// Compare-and-swap the pointer value.
    ///
    /// Atomically compares the current value with `expected`, and if they match,
    /// replaces the value with `new`. This is the core primitive for lock-free
    /// child pointer updates during concurrent inserts.
    ///
    /// # Arguments
    ///
    /// * `expected` - The expected current value
    /// * `new` - The new value to store if expected matches
    ///
    /// # Returns
    ///
    /// - `Ok(expected)` if the swap succeeded (current was `expected`)
    /// - `Err(actual)` if the swap failed (current was `actual`)
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use libdictenstein::persistent_artrie::swizzled_ptr::{SwizzledPtr, NodeType};
    ///
    /// let ptr = SwizzledPtr::null();
    /// let new_child = SwizzledPtr::on_disk(1, 100, NodeType::Node4);
    ///
    /// // CAS from null (0) to new child
    /// match ptr.compare_exchange_raw(0, new_child.to_raw()) {
    ///     Ok(_) => println!("Successfully inserted child"),
    ///     Err(actual) => println!("Another thread inserted first: {}", actual),
    /// }
    /// ```
    #[inline]
    pub fn compare_exchange_raw(&self, expected: u64, new: u64) -> Result<u64, u64> {
        self.0
            .compare_exchange(expected, new, Ordering::AcqRel, Ordering::Acquire)
    }

    /// Weak compare-and-swap the pointer value.
    ///
    /// Like `compare_exchange_raw`, but may spuriously fail even when the
    /// comparison would succeed. Use this in a loop for better performance
    /// on some architectures.
    ///
    /// # Arguments
    ///
    /// * `expected` - The expected current value
    /// * `new` - The new value to store if expected matches
    ///
    /// # Returns
    ///
    /// - `Ok(expected)` if the swap succeeded
    /// - `Err(actual)` if the swap failed (or spuriously failed)
    #[inline]
    pub fn compare_exchange_weak_raw(&self, expected: u64, new: u64) -> Result<u64, u64> {
        self.0
            .compare_exchange_weak(expected, new, Ordering::AcqRel, Ordering::Acquire)
    }

    /// Atomically CAS to set a null pointer to a new child pointer.
    ///
    /// This is a convenience wrapper for the common pattern of adding a child
    /// to an empty slot. It only succeeds if the current value is null (0).
    ///
    /// # Arguments
    ///
    /// * `new_child` - The new child pointer to insert
    ///
    /// # Returns
    ///
    /// - `Ok(())` if the null pointer was successfully replaced with `new_child`
    /// - `Err(actual)` if the slot was not null (another thread inserted first)
    #[inline]
    pub fn try_insert_child(&self, new_child: &SwizzledPtr) -> Result<(), u64> {
        let new_raw = new_child.load_raw();
        match self.compare_exchange_raw(0, new_raw) {
            Ok(_) => Ok(()),
            Err(actual) => Err(actual),
        }
    }

    /// Atomically CAS to update a child pointer.
    ///
    /// This is used when replacing an existing child (e.g., during node growth).
    ///
    /// # Arguments
    ///
    /// * `expected_child` - The expected current child pointer
    /// * `new_child` - The new child pointer to insert
    ///
    /// # Returns
    ///
    /// - `Ok(())` if the swap succeeded
    /// - `Err(actual)` if the current value didn't match `expected_child`
    #[inline]
    pub fn try_update_child(
        &self,
        expected_child: &SwizzledPtr,
        new_child: &SwizzledPtr,
    ) -> Result<(), u64> {
        let expected_raw = expected_child.load_raw();
        let new_raw = new_child.load_raw();
        match self.compare_exchange_raw(expected_raw, new_raw) {
            Ok(_) => Ok(()),
            Err(actual) => Err(actual),
        }
    }

    /// Check if this pointer is null without synchronization.
    ///
    /// This is faster than `is_null()` when used in contexts where
    /// the value is known not to be changing (e.g., during single-threaded init).
    #[inline]
    pub fn is_null_relaxed(&self) -> bool {
        self.0.load(Ordering::Relaxed) == 0
    }
}

impl Clone for SwizzledPtr {
    fn clone(&self) -> Self {
        Self(AtomicU64::new(self.0.load(Ordering::Acquire)))
    }
}

impl Default for SwizzledPtr {
    fn default() -> Self {
        Self::null()
    }
}

/// Decoded disk location from an unswizzled pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskLocation {
    /// Block ID (0 to 8M - 1).
    pub block_id: u32,
    /// Offset within the block (0 to 4MB - 1).
    pub offset: u32,
    /// Type of node at this location.
    pub node_type: NodeType,
}

impl DiskLocation {
    /// Calculate the absolute byte offset in the file.
    ///
    /// # Arguments
    ///
    /// * `block_size` - Size of each block in bytes
    pub fn file_offset(&self, block_size: usize) -> u64 {
        (self.block_id as u64 * block_size as u64) + self.offset as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_null_pointer() {
        let ptr = SwizzledPtr::null();
        assert!(ptr.is_null());
        assert!(!ptr.is_swizzled());
        assert!(!ptr.is_on_disk());
    }

    #[test]
    fn test_disk_reference() {
        let ptr = SwizzledPtr::on_disk(1234, 5678, NodeType::Node16);
        assert!(!ptr.is_null());
        assert!(!ptr.is_swizzled());
        assert!(ptr.is_on_disk());

        let loc = ptr.disk_location().expect("should have disk location");
        assert_eq!(loc.block_id, 1234);
        assert_eq!(loc.offset, 5678);
        assert_eq!(loc.node_type, NodeType::Node16);
    }

    #[test]
    fn test_memory_pointer() {
        let data: u64 = 42;
        let ptr = SwizzledPtr::in_memory(&data);
        assert!(!ptr.is_null());
        assert!(ptr.is_swizzled());
        assert!(!ptr.is_on_disk());

        let retrieved: *const u64 = ptr.as_ptr().expect("should have memory pointer");
        assert_eq!(unsafe { *retrieved }, 42);
    }

    #[test]
    fn test_swizzle() {
        let ptr = SwizzledPtr::on_disk(100, 200, NodeType::Node4);
        assert!(ptr.is_on_disk());

        let data: u64 = 12345;
        ptr.swizzle(&data).expect("swizzle should succeed");

        assert!(ptr.is_swizzled());
        let retrieved: *const u64 = ptr.as_ptr().expect("should have memory pointer");
        assert_eq!(unsafe { *retrieved }, 12345);
    }

    #[test]
    fn test_double_swizzle_fails() {
        let ptr = SwizzledPtr::on_disk(100, 200, NodeType::Node4);
        let data: u64 = 42;
        ptr.swizzle(&data).expect("first swizzle should succeed");

        let result = ptr.swizzle(&data);
        assert_eq!(result, Err(SwizzleError::AlreadySwizzled));
    }

    #[test]
    fn test_unswizzle() {
        let data: u64 = 42;
        let ptr = SwizzledPtr::in_memory(&data);
        assert!(ptr.is_swizzled());

        let old_ptr: *const u64 = ptr
            .unswizzle(500, 600, NodeType::Bucket)
            .expect("unswizzle should succeed");

        assert!(ptr.is_on_disk());
        assert_eq!(unsafe { *old_ptr }, 42);

        let loc = ptr.disk_location().expect("should have disk location");
        assert_eq!(loc.block_id, 500);
        assert_eq!(loc.offset, 600);
        assert_eq!(loc.node_type, NodeType::Bucket);
    }

    #[test]
    fn test_max_values() {
        let ptr = SwizzledPtr::on_disk(MAX_BLOCK_ID, MAX_OFFSET, NodeType::Node256);
        let loc = ptr.disk_location().expect("should have disk location");
        assert_eq!(loc.block_id, MAX_BLOCK_ID);
        assert_eq!(loc.offset, MAX_OFFSET);
    }

    #[test]
    fn test_file_offset_calculation() {
        let loc = DiskLocation {
            block_id: 10,
            offset: 1024,
            node_type: NodeType::Node16,
        };

        // With 256KB blocks
        let block_size = 256 * 1024;
        let expected = 10 * block_size as u64 + 1024;
        assert_eq!(loc.file_offset(block_size), expected);
    }

    #[test]
    fn test_node_type_roundtrip() {
        for node_type in [
            NodeType::Node4,
            NodeType::Node16,
            NodeType::Node48,
            NodeType::Node256,
            NodeType::Bucket,
        ] {
            let ptr = SwizzledPtr::on_disk(123, 456, node_type);
            let loc = ptr.disk_location().expect("should decode");
            assert_eq!(loc.node_type, node_type);
        }
    }

    mod arena_slot_tests {
        use super::*;
        use crate::persistent_artrie_core::arena_slot::ArenaSlot;

        #[test]
        fn test_from_arena_slot() {
            // Arena 0 should map to block 1
            let slot = ArenaSlot::new(0, 100);
            let ptr = SwizzledPtr::from_arena_slot(slot, NodeType::Node4);

            assert!(ptr.is_on_disk());
            let loc = ptr.disk_location().expect("should have disk location");
            assert_eq!(loc.block_id, 1); // arena 0 -> block 1
            assert_eq!(loc.offset, 100);
            assert_eq!(loc.node_type, NodeType::Node4);
        }

        #[test]
        fn test_as_arena_slot() {
            // Block 5 should map to arena 4
            let ptr = SwizzledPtr::on_disk(5, 200, NodeType::Node16);

            let slot = ptr.as_arena_slot().expect("should convert to arena slot");
            assert_eq!(slot.arena_id, 4); // block 5 -> arena 4
            assert_eq!(slot.slot_id, 200);
        }

        #[test]
        fn test_arena_slot_roundtrip() {
            let original = ArenaSlot::new(42, 12345);
            let ptr = SwizzledPtr::from_arena_slot(original, NodeType::Node48);
            let recovered = ptr.as_arena_slot().expect("should convert back");

            assert_eq!(recovered.arena_id, original.arena_id);
            assert_eq!(recovered.slot_id, original.slot_id);
        }

        #[test]
        fn test_as_arena_slot_returns_none_for_memory() {
            let data: u64 = 42;
            let ptr = SwizzledPtr::in_memory(&data);

            assert!(ptr.as_arena_slot().is_none());
        }

        #[test]
        fn test_as_arena_slot_returns_none_for_null() {
            let ptr = SwizzledPtr::null();

            assert!(ptr.as_arena_slot().is_none());
        }

        #[test]
        fn test_as_arena_slot_returns_none_for_block_zero() {
            // Block 0 is the header, arena_id would be -1 which is invalid
            let ptr = SwizzledPtr::on_disk(0, 100, NodeType::Node4);

            assert!(ptr.as_arena_slot().is_none());
        }
    }

    // =========================================================================
    // CAS Operation Tests
    // =========================================================================

    mod cas_tests {
        use super::*;
        use std::sync::Arc;
        use std::thread;

        #[test]
        fn test_load_store_raw() {
            let ptr = SwizzledPtr::null();
            assert_eq!(ptr.load_raw(), 0);

            let child = SwizzledPtr::on_disk(1, 100, NodeType::Node4);
            ptr.store_raw(child.load_raw());

            assert_eq!(ptr.load_raw(), child.load_raw());
            assert!(ptr.is_on_disk());
        }

        #[test]
        fn test_compare_exchange_raw_success() {
            let ptr = SwizzledPtr::null();
            let new_child = SwizzledPtr::on_disk(1, 100, NodeType::Node4);

            // CAS from null (0) to new child should succeed
            let result = ptr.compare_exchange_raw(0, new_child.load_raw());
            assert!(result.is_ok());
            assert_eq!(result.unwrap(), 0);
            assert_eq!(ptr.load_raw(), new_child.load_raw());
        }

        #[test]
        fn test_compare_exchange_raw_failure() {
            let ptr = SwizzledPtr::on_disk(1, 100, NodeType::Node4);
            let new_child = SwizzledPtr::on_disk(2, 200, NodeType::Node16);

            // CAS expecting null (0) should fail because ptr is not null
            let result = ptr.compare_exchange_raw(0, new_child.load_raw());
            assert!(result.is_err());

            // Should return the actual value
            let actual = result.unwrap_err();
            assert_eq!(actual, ptr.load_raw());
        }

        #[test]
        fn test_try_insert_child_success() {
            let slot = SwizzledPtr::null();
            let child = SwizzledPtr::on_disk(1, 100, NodeType::Node4);

            let result = slot.try_insert_child(&child);
            assert!(result.is_ok());
            assert_eq!(slot.load_raw(), child.load_raw());
        }

        #[test]
        fn test_try_insert_child_failure() {
            let existing = SwizzledPtr::on_disk(1, 100, NodeType::Node4);
            let slot = SwizzledPtr::from_raw(existing.load_raw());
            let new_child = SwizzledPtr::on_disk(2, 200, NodeType::Node16);

            // Should fail because slot is not null
            let result = slot.try_insert_child(&new_child);
            assert!(result.is_err());

            // Original value should be unchanged
            assert_eq!(slot.load_raw(), existing.load_raw());
        }

        #[test]
        fn test_try_update_child_success() {
            let old_child = SwizzledPtr::on_disk(1, 100, NodeType::Node4);
            let slot = SwizzledPtr::from_raw(old_child.load_raw());
            let new_child = SwizzledPtr::on_disk(2, 200, NodeType::Node16);

            let result = slot.try_update_child(&old_child, &new_child);
            assert!(result.is_ok());
            assert_eq!(slot.load_raw(), new_child.load_raw());
        }

        #[test]
        fn test_try_update_child_failure() {
            let old_child = SwizzledPtr::on_disk(1, 100, NodeType::Node4);
            let actual_child = SwizzledPtr::on_disk(3, 300, NodeType::Node48);
            let slot = SwizzledPtr::from_raw(actual_child.load_raw());
            let new_child = SwizzledPtr::on_disk(2, 200, NodeType::Node16);

            // Should fail because expected doesn't match
            let result = slot.try_update_child(&old_child, &new_child);
            assert!(result.is_err());

            // Original value should be unchanged
            assert_eq!(slot.load_raw(), actual_child.load_raw());
        }

        #[test]
        fn test_is_null_relaxed() {
            let null_ptr = SwizzledPtr::null();
            assert!(null_ptr.is_null_relaxed());

            let non_null = SwizzledPtr::on_disk(1, 100, NodeType::Node4);
            assert!(!non_null.is_null_relaxed());
        }

        #[test]
        fn test_concurrent_try_insert_child() {
            // Test that exactly one thread wins when multiple try to insert
            let slot = Arc::new(SwizzledPtr::null());
            let num_threads = 10;

            let handles: Vec<_> = (0..num_threads)
                .map(|i| {
                    let s = Arc::clone(&slot);
                    thread::spawn(move || {
                        let child = SwizzledPtr::on_disk(1, i as u32, NodeType::Node4);
                        s.try_insert_child(&child).is_ok()
                    })
                })
                .collect();

            let results: Vec<bool> = handles
                .into_iter()
                .map(|h| h.join().expect("thread should complete"))
                .collect();

            // Exactly one thread should have won
            let winners = results.iter().filter(|&&x| x).count();
            assert_eq!(winners, 1, "exactly one thread should win try_insert_child");

            // Slot should not be null
            assert!(!slot.is_null());
        }

        #[test]
        fn test_concurrent_compare_exchange() {
            // Test competing CAS operations
            let ptr = Arc::new(SwizzledPtr::null());
            let num_threads = 20;

            let handles: Vec<_> = (0..num_threads)
                .map(|i| {
                    let p = Arc::clone(&ptr);
                    thread::spawn(move || {
                        let new_val = SwizzledPtr::on_disk(1, i as u32, NodeType::Node4);
                        p.compare_exchange_raw(0, new_val.load_raw()).is_ok()
                    })
                })
                .collect();

            let results: Vec<bool> = handles
                .into_iter()
                .map(|h| h.join().expect("thread should complete"))
                .collect();

            // Exactly one thread should have won
            let winners = results.iter().filter(|&&x| x).count();
            assert_eq!(winners, 1, "exactly one thread should win CAS");

            // Verify the winner's value is stored
            let final_val = ptr.load_raw();
            assert_ne!(final_val, 0, "final value should not be null");

            // The value should be a valid disk reference
            let disk_ptr = SwizzledPtr::from_raw(final_val);
            assert!(disk_ptr.is_on_disk());
        }
    }
}
