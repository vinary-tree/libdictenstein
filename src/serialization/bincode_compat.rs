//! Compatibility shim for the bincode 2.x-style serde adapter.
//!
//! bincode 2.x dropped the bincode 1.x crate-root `serialize_into` /
//! `deserialize_from` / `serialize` / `deserialize` functions in favor
//! of the [`bincode::serde`] sub-module with a `Config` parameter and
//! `EncodeError` / `DecodeError` error types. This shim exposes the
//! old 1.x API surface on top of that so the rest of the crate
//! can migrate one call-site at a time without re-architecting the
//! error chain.
//!
//! # `bincode` here is `bincode-next`
//!
//! The `bincode` path used throughout this module resolves to the
//! **`bincode-next`** crate, declared under the original name via a Cargo
//! package rename. The original `bincode` is unmaintained
//! (RUSTSEC-2025-0141) with no fixed version — its own 3.0.0 is a tombstone
//! release containing only a compiler error — so the advisory could only be
//! closed by leaving the crate. `bincode-next` is the maintained fork and
//! preserves the API exactly, which is why nothing below changed.
//!
//! Byte compatibility was **measured, not assumed**: a probe depending on
//! bincode 1.3, bincode 2.0 and bincode-next 3 simultaneously produced
//! identical `config::legacy()` output across scalars, signed integers,
//! sequences, nested structs and payload-carrying enums. The
//! `wire_format_pins` tests at the bottom of this file enforce that
//! permanently.
//!
//! The config used everywhere here is `bincode::config::legacy()`, which
//! is **fixed-int little-endian** (NOT `standard()`'s varint encoding):
//! every integer is written as its full little-endian byte image (a `u64`/
//! `i64` is exactly 8 LE bytes), with the bincode-1.x default-strict
//! trailing-bytes check. This fixint-LE layout is load-bearing for the
//! persistent ART-trie counter leaf, which the `counter_codec` module
//! decodes as exactly 8 little-endian bytes (so a non-negative `u64` and
//! an `i64` of the same value are byte-identical on disk).

#![cfg(feature = "serialization")]

use serde::de::DeserializeOwned;
use serde::Serialize;
use std::io::{Read, Write};

/// Bincode 2.x error wrapper unifying encode + decode failures into a
/// single type, so the legacy `SerializationError::Bincode` variant
/// can `#[from]` it. Mirrors what bincode 1.x exposed as `bincode::Error`.
#[derive(Debug, thiserror::Error)]
pub enum BincodeError {
    #[error("bincode encode error: {0}")]
    Encode(#[from] bincode::error::EncodeError),
    #[error("bincode decode error: {0}")]
    Decode(#[from] bincode::error::DecodeError),
}

/// Drop-in replacement for `bincode::serialize_into` (bincode 1.x).
pub fn serialize_into<W: Write, T: Serialize>(
    writer: &mut W,
    value: &T,
) -> Result<(), BincodeError> {
    let config = bincode::config::legacy();
    bincode::serde::encode_into_std_write(value, writer, config)?;
    Ok(())
}

/// Drop-in replacement for `bincode::deserialize_from` (bincode 1.x).
pub fn deserialize_from<R: Read, T: DeserializeOwned>(reader: &mut R) -> Result<T, BincodeError> {
    let config = bincode::config::legacy();
    Ok(bincode::serde::decode_from_std_read(reader, config)?)
}

/// Drop-in replacement for `bincode::serialize` (bincode 1.x).
pub fn serialize<T: Serialize>(value: &T) -> Result<Vec<u8>, BincodeError> {
    let config = bincode::config::legacy();
    Ok(bincode::serde::encode_to_vec(value, config)?)
}

/// Drop-in replacement for `bincode::deserialize` (bincode 1.x).
pub fn deserialize<T: DeserializeOwned>(slice: &[u8]) -> Result<T, BincodeError> {
    let config = bincode::config::legacy();
    let (value, _consumed): (T, usize) = bincode::serde::decode_from_slice(slice, config)?;
    Ok(value)
}

/// Byte-level pins for the `legacy()` wire format.
///
/// These are the regression net for swapping the underlying bincode implementation.
/// Until they existed, nothing in this crate checked the *encoding* — every
/// round-trip test writes and reads within one process, so a wholesale format
/// change would round-trip perfectly while silently invalidating every persisted
/// ARTrie, WAL segment, snapshot and checkpoint already on disk.
///
/// Each assertion below pins a property that some other part of the codebase
/// depends on structurally, not merely incidentally.
#[cfg(test)]
mod wire_format_pins {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct SnapshotHeader {
        magic: [u8; 8],
        version: u32,
    }

    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    enum Payload {
        Empty,
        Bytes(Vec<u8>),
        Keyed { id: u64 },
    }

    /// Integers are fixed-width little-endian, never varint.
    ///
    /// `persistent_artrie::core::counter_codec` decodes a counter leaf as exactly
    /// eight little-endian bytes and relies on a non-negative `u64` and an `i64` of
    /// the same value being byte-identical. `standard()` would encode these as
    /// varints and break both properties.
    #[test]
    fn integers_are_fixed_width_little_endian() {
        assert_eq!(serialize(&1u64).expect("u64"), [1, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(serialize(&1u32).expect("u32"), [1, 0, 0, 0]);
        assert_eq!(serialize(&300u64).expect("u64"), [44, 1, 0, 0, 0, 0, 0, 0]);
        assert_eq!(
            serialize(&7i64).expect("i64"),
            serialize(&7u64).expect("u64"),
            "counter_codec depends on u64/i64 byte-identity for non-negative values"
        );
        assert_eq!(
            serialize(&-7i64).expect("i64"),
            [249, 255, 255, 255, 255, 255, 255, 255]
        );
    }

    /// A fixed-size byte array is written inline with no length prefix, and a `u32`
    /// immediately after it occupies exactly four bytes.
    ///
    /// `persistent_artrie::{suffix_automaton, scdawg, suffix_tree}` read the magic
    /// and version back by slicing the *encoded* buffer at `[0..8]` and `[8..12]`
    /// before decoding anything. If `legacy()` ever length-prefixed the array or
    /// varint-encoded the `u32`, those reads would silently address the wrong bytes
    /// and every existing snapshot would be rejected as `InvalidMagic`.
    #[test]
    fn snapshot_header_is_readable_by_raw_offset() {
        let header = SnapshotHeader {
            magic: *b"ARTC0001",
            version: 2,
        };
        let bytes = serialize(&header).expect("encode header");

        assert_eq!(
            &bytes[0..8],
            b"ARTC0001",
            "magic must be inline, unprefixed"
        );
        assert_eq!(
            u32::from_le_bytes(bytes[8..12].try_into().expect("4 bytes")),
            2,
            "version must be 4 LE bytes immediately after the magic"
        );
        assert_eq!(bytes.len(), 12, "no padding or trailing metadata");
    }

    /// Sequences carry a `u64` little-endian length prefix, and `Option`/enum
    /// discriminants keep their bincode 1.x widths (1 byte and 4 bytes).
    #[test]
    fn sequence_option_and_enum_encodings_are_pinned() {
        assert_eq!(
            serialize(&vec![1u8, 2, 3]).expect("vec"),
            [3, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3]
        );
        assert_eq!(serialize(&Option::<u8>::None).expect("none"), [0]);
        assert_eq!(serialize(&Some(9u8)).expect("some"), [1, 9]);
        assert_eq!(
            serialize(&Payload::Empty).expect("unit variant"),
            [0, 0, 0, 0]
        );
        assert_eq!(
            serialize(&Payload::Keyed { id: 9 }).expect("struct variant"),
            [2, 0, 0, 0, 9, 0, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(
            serialize(&"ab".to_string()).expect("string"),
            [2, 0, 0, 0, 0, 0, 0, 0, b'a', b'b']
        );
    }

    /// The writer and slice paths must agree, and both must round-trip.
    #[test]
    fn writer_and_slice_paths_agree() {
        let value = Payload::Bytes(vec![4, 5, 6]);
        let via_slice = serialize(&value).expect("serialize");

        let mut via_writer = Vec::new();
        serialize_into(&mut via_writer, &value).expect("serialize_into");
        assert_eq!(via_writer, via_slice);

        assert_eq!(
            deserialize::<Payload>(&via_slice).expect("deserialize"),
            value
        );
        assert_eq!(
            deserialize_from::<_, Payload>(&mut via_slice.as_slice()).expect("deserialize_from"),
            value
        );
    }
}
