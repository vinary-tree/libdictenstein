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

// ============================================================================
// Concrete `KeyEncoding` markers
// ============================================================================

/// Marker type for byte-keyed (ASCII / arbitrary-byte) tries.
#[derive(Debug, Clone, Copy)]
pub struct ByteKey;

/// Marker type for char-keyed (UTF-8 / Unicode code-point) tries.
#[derive(Debug, Clone, Copy)]
pub struct CharKey;

impl KeyEncoding for ByteKey {
    type Unit = u8;
    const KEY_BYTES: usize = 1;
    // From persistent_artrie/arena.rs:43-46:
    const ARENA_MAGIC: u64 = 0x414E4152_41545942; // "BYTARANA" in little-endian
    const ARENA_MAGIC_V2: u64 = 0x32564152_41545942; // "BYTARAV2" in little-endian
                                                     // Matches the file-header magic accepted by core/recovery.rs:1083.
    const FILE_MAGIC: [u8; 4] = *b"PART";
    const NAME: &'static str = "byte";

    // G4 (shared `OverlayNode`): byte path-compression caps at 12 units = 12 B.
    const MAX_PREFIX_LEN: usize = 12;
    const UNIT_ZERO: Self::Unit = 0u8;

    fn units_from_str(s: &str) -> SmallVec<[Self::Unit; 32]> {
        s.as_bytes().iter().copied().collect()
    }

    fn unit_to_le_bytes(unit: Self::Unit) -> [u8; 4] {
        [unit, 0, 0, 0]
    }

    fn unit_from_le_bytes(bytes: &[u8]) -> Self::Unit {
        bytes[0]
    }
}

impl KeyEncoding for CharKey {
    type Unit = u32;
    const KEY_BYTES: usize = 4;
    // From persistent_artrie_char/arena.rs:43-46:
    const ARENA_MAGIC: u64 = 0x414E5241524148_43; // "CHARARNA" in little-endian
    const ARENA_MAGIC_V2: u64 = 0x32564152_4148_43; // "CHARARV2" in little-endian
    const FILE_MAGIC: [u8; 4] = *b"ARTC";
    const NAME: &'static str = "char";

    // G4 (shared `OverlayNode`): char path-compression caps at 6 units = 24 B.
    const MAX_PREFIX_LEN: usize = 6;
    const UNIT_ZERO: Self::Unit = 0u32;

    fn units_from_str(s: &str) -> SmallVec<[Self::Unit; 32]> {
        s.chars().map(|c| c as u32).collect()
    }

    fn unit_to_le_bytes(unit: Self::Unit) -> [u8; 4] {
        unit.to_le_bytes()
    }

    fn unit_from_le_bytes(bytes: &[u8]) -> Self::Unit {
        let mut buf = [0u8; 4];
        buf.copy_from_slice(&bytes[..4]);
        u32::from_le_bytes(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_key_roundtrip() {
        for u in 0u8..=255 {
            let bytes = ByteKey::unit_to_le_bytes(u);
            assert_eq!(ByteKey::unit_from_le_bytes(&bytes), u);
        }
    }

    #[test]
    fn char_key_roundtrip() {
        for u in [0u32, 0x41, 0xFF, 0x1F600, 0x10FFFF] {
            let bytes = CharKey::unit_to_le_bytes(u);
            assert_eq!(CharKey::unit_from_le_bytes(&bytes), u);
        }
    }

    #[test]
    fn byte_key_units_from_str() {
        let units = ByteKey::units_from_str("hello");
        assert_eq!(units.as_slice(), b"hello");
    }

    #[test]
    fn char_key_units_from_str() {
        let units = CharKey::units_from_str("h\u{1F600}");
        assert_eq!(units.as_slice(), &[b'h' as u32, 0x1F600]);
    }

    // Note: assertions that ByteKey / CharKey constants match the variant
    // modules' arena ARENA_MAGIC / ARENA_MAGIC_V2 constants live in the
    // variant modules' test suites (persistent_artrie::arena::tests and
    // persistent_artrie_char::arena::tests) rather than here, so that
    // persistent_artrie_core's source set stays free of upward references
    // to its consumers.
}

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

    /// 8-byte arena magic prefix used in V1 arena-page header layouts.
    const ARENA_MAGIC: u64;

    /// 8-byte arena magic prefix used in V2 arena-page header layouts.
    const ARENA_MAGIC_V2: u64;

    /// 4-byte file-header magic identifying this variant's trie file
    /// (`*b"PART"` for byte, `*b"ARTC"` for char/vocab).
    const FILE_MAGIC: [u8; 4];

    /// Human-readable name used in diagnostics and panic messages.
    const NAME: &'static str;

    /// G4: maximum path-compression prefix length, in key units.
    ///
    /// `12` for byte (12 B), `6` for char (24 B). Consumed by the shared
    /// `persistent_artrie_core::overlay::OverlayNode` to cap its `prefix` length.
    const MAX_PREFIX_LEN: usize;

    /// G4: the zero-valued unit used as dead filler in the shared
    /// `OverlayNode`'s inline child-array `[count..]` slots (never read; only
    /// `keys[..count]` are live). `0u8` for byte, `0u32` for char.
    const UNIT_ZERO: Self::Unit;

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
