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
    type Term = Vec<u8>;
    type Token = u8;
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

    fn units_from_bytes(bytes: &[u8]) -> Option<SmallVec<[Self::Unit; 32]>> {
        // Byte keys ARE the raw bytes — identity copy, always valid.
        Some(bytes.iter().copied().collect())
    }

    fn units_to_term(units: &[u8]) -> Vec<u8> {
        units.to_vec()
    }

    #[inline]
    fn token_to_unit(token: u8) -> u8 {
        token
    }

    #[inline]
    fn unit_to_token(unit: u8) -> Option<u8> {
        Some(unit)
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
    type Term = String;
    type Token = char;
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

    fn units_from_bytes(bytes: &[u8]) -> Option<SmallVec<[Self::Unit; 32]>> {
        // Char keys are stored as UTF-8 in the WAL (writers log `term.as_bytes()`).
        // Decode back to code points; a non-UTF-8 byte sequence cannot have been
        // produced by a char-trie writer (None ⇒ the F5 applier skips it).
        std::str::from_utf8(bytes)
            .ok()
            .map(|s| s.chars().map(|c| c as u32).collect())
    }

    fn units_to_term(units: &[u32]) -> String {
        units
            .iter()
            .map(|&u| char::from_u32(u).unwrap_or('\u{FFFD}'))
            .collect()
    }

    #[inline]
    fn token_to_unit(token: char) -> u32 {
        token as u32
    }

    #[inline]
    fn unit_to_token(unit: u32) -> Option<char> {
        char::from_u32(unit)
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

    // G5.0: `Token` ↔ `Unit` conversions for the shared `OverlayDictionaryNode`.
    #[test]
    fn byte_key_token_unit_roundtrip() {
        for u in 0u8..=255 {
            assert_eq!(ByteKey::token_to_unit(u), u);
            assert_eq!(ByteKey::unit_to_token(u), Some(u));
        }
    }

    #[test]
    fn char_key_token_unit_roundtrip() {
        for c in [
            'a',
            'Z',
            '\u{E9}',
            '\u{65E5}',
            '\u{1F980}',
            '\u{0}',
            '\u{10FFFF}',
        ] {
            let u = CharKey::token_to_unit(c);
            assert_eq!(u, c as u32);
            assert_eq!(CharKey::unit_to_token(u), Some(c));
        }
        // Surrogate code points are not valid tokens → skipped (`None`), exactly
        // as the prior char `edges()` `char::from_u32` filter did.
        assert_eq!(CharKey::unit_to_token(0xD800), None);
        assert_eq!(CharKey::unit_to_token(0xDFFF), None);
    }

    // The `units_from_str` ∘ `units_to_term` round-trip invariant (the formal
    // statement that routing reads through the shared engine cannot change terms).
    #[test]
    fn char_units_to_term_roundtrips_str() {
        for s in [
            "",
            "hello",
            "日本語",
            "h\u{1F600}x",
            "\u{10FFFF}",
            "mixed 日 a \u{1F389} b",
        ] {
            let units = CharKey::units_from_str(s);
            assert_eq!(
                CharKey::units_to_term(&units),
                s.to_string(),
                "char str->units->term must round-trip for {s:?}"
            );
        }
    }

    #[test]
    fn byte_units_to_term_roundtrips_bytes() {
        for s in ["", "hello", "日本語", "\u{1F600}"] {
            let units = ByteKey::units_from_str(s);
            assert_eq!(
                ByteKey::units_to_term(&units),
                s.as_bytes().to_vec(),
                "byte str->units->term must equal s.as_bytes() for {s:?}"
            );
        }
        // Arbitrary (non-UTF-8) byte sequences round-trip too (identity copy).
        let raw: Vec<u8> = vec![0, 1, 255, 128, 42];
        assert_eq!(ByteKey::units_to_term(&raw), raw.clone());
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

    /// The public term type this encoding reconstructs to: `String` for char
    /// (Unicode), `Vec<u8>` for byte (arbitrary byte strings). The shared
    /// overlay-read engine enumerates `Vec<Self::Unit>` and the variant's public
    /// API converts each to `Self::Term` via [`units_to_term`](Self::units_to_term).
    type Term: Clone + Debug;

    /// The PUBLIC `DictionaryNode::Unit` token a caller (transducer / zipper)
    /// traverses by. For byte this equals [`Unit`](Self::Unit) (`u8`); for char it
    /// is `char` while `Unit` is the `u32` code point. The split lets the shared
    /// `OverlayDictionaryNode<K, V>` present each variant's natural public unit
    /// while storing the compact internal `Unit` in the overlay child map.
    ///
    /// Bound by [`CharUnit`](crate::char_unit::CharUnit) because the shared
    /// `OverlayDictionaryNode`'s `DictionaryNode::Unit = Self::Token`, and
    /// `DictionaryNode::Unit: CharUnit`. Both `u8` (`ByteKey`) and `char` (`CharKey`)
    /// implement `CharUnit`, so this is exactly the prior per-variant `Unit = u8` /
    /// `Unit = char` bound — no new constraint on any real implementor.
    type Token: crate::char_unit::CharUnit;

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

    /// Decode RAW WAL key bytes into a sequence of edge units, or `None` if the
    /// bytes are not a valid key for this encoding (F5 WAL-tail-into-overlay applier).
    ///
    /// For `ByteKey` the key bytes ARE the units (identity copy, always `Some`); for
    /// `CharKey` the WAL stores the term as UTF-8 (writers log `term.as_bytes()`), so
    /// this decodes UTF-8 → code points and returns `None` for a non-UTF-8 sequence
    /// (which a char-trie writer cannot have produced — the applier skips it).
    fn units_from_bytes(bytes: &[u8]) -> Option<SmallVec<[Self::Unit; 32]>>;

    /// Reverse of [`units_from_str`](Self::units_from_str): reconstruct the public
    /// term from a unit sequence. Char maps each code point via
    /// `char::from_u32(_).unwrap_or('\u{FFFD}')`; byte returns the raw `Vec<u8>`
    /// (byte terms are arbitrary byte strings — NO UTF-8 re-decode; UTF-8
    /// interpretation is the caller's concern). Invariant on the valid domain:
    /// `units_to_term(&units_from_str(s))` equals `s`'s term form (`s` for char,
    /// `s.as_bytes()` for byte).
    fn units_to_term(units: &[Self::Unit]) -> Self::Term;

    /// Lower a public [`Token`](Self::Token) to the internal storage
    /// [`Unit`](Self::Unit) (the overlay child-map key). Byte: identity; char:
    /// `token as u32`. Total — every token has a unit.
    fn token_to_unit(token: Self::Token) -> Self::Unit;

    /// Raise an internal [`Unit`](Self::Unit) back to a public
    /// [`Token`](Self::Token), or `None` when the unit is not a valid token (a
    /// `u32` that is not a Unicode scalar value — a surrogate). `None` units are
    /// SKIPPED by the shared node's `edges()` (never fabricated into a transition),
    /// preserving the prior char `char::from_u32` filter. Byte: always `Some`.
    fn unit_to_token(unit: Self::Unit) -> Option<Self::Token>;

    /// Encode `unit` as up to 4 little-endian bytes. `u8` keys pad with
    /// zeros; `u32` keys use the full 4 bytes. Returned slice is always
    /// `KEY_BYTES` long.
    fn unit_to_le_bytes(unit: Self::Unit) -> [u8; 4];

    /// Decode a unit from at least `KEY_BYTES` of little-endian bytes.
    /// Panics if `bytes.len() < KEY_BYTES`.
    fn unit_from_le_bytes(bytes: &[u8]) -> Self::Unit;
}
