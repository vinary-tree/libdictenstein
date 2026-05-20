//! `KeyEncoding` — the seam that lets shared modules be generic over the
//! key-unit width of each persistent ARTrie variant.
//!
//! Variants implement this trait on a marker type (e.g. `ByteKey`, `CharKey`)
//! and the shared modules in `persistent_artrie_core` use the trait's
//! associated `Unit` and `KEY_BYTES` to operate on byte (`u8`) or char (`u32`)
//! keys uniformly.
//!
//! Phase 1 only defines the trait. The `ByteKey` / `CharKey` impls and the
//! generification of `arena_manager`, `dedup`, `traversal_context`,
//! `relative_encoding`, `per_node_log`, `mvcc`, `version_checkpoint`,
//! `version_gc`, and `recovery` against this trait happen in Phase 3.

use std::fmt::Debug;
use std::hash::Hash;

use smallvec::SmallVec;

/// Marker trait identifying the key-unit type of a persistent ARTrie variant.
///
/// Implementors are zero-sized marker types (e.g. `ByteKey`, `CharKey`).
pub trait KeyEncoding: 'static + Copy + Send + Sync + Debug {
    /// The unit type stored at each edge of the trie.
    ///
    /// `u8` for byte tries; `u32` (Unicode code points) for char tries.
    type Unit: Copy + Eq + Ord + Hash + Send + Sync + 'static + Debug;

    /// Width of `Self::Unit` in bytes (1 for `u8`, 4 for `u32`).
    const KEY_BYTES: usize;

    /// 8-byte arena magic prefix used in V1 header layouts.
    const ARENA_MAGIC: u64;

    /// 8-byte arena magic prefix used in V2 header layouts.
    const ARENA_MAGIC_V2: u64;

    /// 8-byte arena magic prefix used in V3 header layouts.
    const ARENA_MAGIC_V3: u64;

    /// 4-byte file-header magic identifying this variant's `.artrie` file.
    const FILE_MAGIC: u32;

    /// Human-readable name used in diagnostics and panic messages.
    const NAME: &'static str;

    /// Decode `s` into a sequence of edge units.
    ///
    /// For `ByteKey` this returns `s.as_bytes()`; for `CharKey` it returns
    /// the iterator of Unicode code points as `u32`s.
    fn units_from_str(s: &str) -> SmallVec<[Self::Unit; 32]>;

    /// Encode `unit` as up to 4 little-endian bytes. `u8` keys pad with
    /// zeros; `u32` keys use the full 4 bytes. Returned slice is always
    /// `KEY_BYTES` long.
    fn unit_to_le_bytes(unit: Self::Unit) -> [u8; 4];

    /// Decode a unit from at least `KEY_BYTES` of little-endian bytes.
    /// Panics if `bytes.len() < KEY_BYTES`.
    fn unit_from_le_bytes(bytes: &[u8]) -> Self::Unit;
}
