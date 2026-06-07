//! Compatibility shim for bincode 2.x's serde adapter.
//!
//! bincode 2.x dropped the bincode 1.x crate-root `serialize_into` /
//! `deserialize_from` / `serialize` / `deserialize` functions in favor
//! of the [`bincode::serde`] sub-module with a `Config` parameter and
//! `EncodeError` / `DecodeError` error types. This shim exposes the
//! old 1.x API surface on top of bincode 2.x so the rest of the crate
//! can migrate one call-site at a time without re-architecting the
//! error chain.
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
