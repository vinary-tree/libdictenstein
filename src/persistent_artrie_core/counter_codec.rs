//! Shared counter-leaf codec: the SINGLE arithmetic substrate for the `{i64, u64}`
//! atomic counter values stored in the persistent ART tries.
//!
//! # Why this module exists (the u64 restoration)
//!
//! A counter leaf is 8 little-endian bytes (`bincode::config::legacy()` = fixint LE,
//! so `i64`/`u64` of the same non-negative value are byte-identical on disk). The
//! historical increment path read the leaf via `i64::from_le_bytes` and did the
//! read-modify-write arithmetic in the **i64 domain** (`checked_add`). For a `u64`
//! counter whose value exceeds `i64::MAX` that is wrong two ways:
//!   * a valid increment near `i64::MAX` is SPURIOUSLY REJECTED (i64 overflow), and
//!   * an increment near `u64::MAX` SILENTLY WRAPS — the i64 `checked_add` only
//!     checks the i64 range, missing the true u64 overflow.
//!
//! The fix: every counter-leaf read/write/arithmetic routes through this module,
//! which decodes the leaf into an **`i128`** (wide enough to hold any `u64` and any
//! `i64` exactly, and wide enough that `u64_value + i64_delta` cannot overflow), does
//! the arithmetic in `i128`, and re-encodes with an explicit per-type range check
//! (`u64`: `[0, u64::MAX]`; `i64`: `[i64::MIN, i64::MAX]`).
//!
//! # The completeness gate (v6 — `docs/design/counter-u64-restoration.md`)
//!
//! These functions are the ONLY place the crate may convert between a counter leaf
//! and an integer. A counter leaf is touched by exactly two mechanisms — manual byte
//! ops (`i64::from_le_bytes` / `as i64` / `as u64`) and a `bincode` round-trip
//! (`bincode_compat::(de)serialize`) — so the grep gate "neither token group appears
//! in any counter-leaf function OUTSIDE this module" is the mechanical proof that no
//! counter-leaf access bypasses the `i128` substrate. The `from_le_bytes`, `as i64`,
//! `as u64`, and `bincode_compat` calls below are the sanctioned originals.

use std::any::TypeId;

use crate::serialization::bincode_compat;
use serde::de::DeserializeOwned;
use serde::Serialize;

/// Decode an 8-byte little-endian counter leaf into an `i128`, keyed by the counter
/// type `V`.
///
/// * `V = u64` → `u64::from_le_bytes(..) as i128` (the full unsigned magnitude).
/// * `V = i64` → `i64::from_le_bytes(..) as i128` (sign-extended).
/// * any other `V`, or `le_bytes.len() != 8` → `None` (graceful: a non-counter `V`
///   that reaches a counter helper is a no-op the caller handles, never a panic).
#[inline]
pub fn counter_leaf_to_i128<V: 'static>(le_bytes: &[u8]) -> Option<i128> {
    if le_bytes.len() != 8 {
        return None;
    }
    let mut word = [0u8; 8];
    word.copy_from_slice(le_bytes);
    let tid = TypeId::of::<V>();
    if tid == TypeId::of::<u64>() {
        Some(u64::from_le_bytes(word) as i128)
    } else if tid == TypeId::of::<i64>() {
        Some(i64::from_le_bytes(word) as i128)
    } else {
        None
    }
}

/// Decode a typed counter value `V` into an `i128` (via its `bincode` leaf image).
///
/// Confines the `bincode_compat::serialize` to this module (the gate). Returns `None`
/// for a non-counter `V` or a serialization failure.
#[inline]
pub fn counter_value_to_i128<V: 'static + Serialize>(value: &V) -> Option<i128> {
    let bytes = bincode_compat::serialize(value).ok()?;
    counter_leaf_to_i128::<V>(&bytes)
}

/// Encode an `i128` into the 8-byte little-endian counter leaf for type `V`,
/// range-checked.
///
/// Returns `None` if `n` is out of `V`'s representable range (`u64`:
/// `n < 0 || n > u64::MAX`; `i64`: `n < i64::MIN || n > i64::MAX`) or `V` is not a
/// counter type. The returned bytes are byte-identical to
/// `bincode_compat::serialize(&(n as V))` (legacy/fixint LE) — so this is a
/// drop-in, format-preserving replacement for the old serialize-then-store path.
#[inline]
pub fn i128_to_counter_leaf<V: 'static>(n: i128) -> Option<Vec<u8>> {
    let tid = TypeId::of::<V>();
    if tid == TypeId::of::<u64>() {
        if n < 0 || n > u64::MAX as i128 {
            return None;
        }
        Some((n as u64).to_le_bytes().to_vec())
    } else if tid == TypeId::of::<i64>() {
        if n < i64::MIN as i128 || n > i64::MAX as i128 {
            return None;
        }
        Some((n as i64).to_le_bytes().to_vec())
    } else {
        None
    }
}

/// Encode an `i128` into a typed counter value `V` (via the range-checked leaf image
/// + `bincode` decode).
///
/// Confines `bincode_compat::deserialize` to this module (the gate). Returns `None`
/// for out-of-range `n`, a non-counter `V`, or a decode failure.
#[inline]
pub fn i128_to_counter_value<V: 'static + DeserializeOwned>(n: i128) -> Option<V> {
    let bytes = i128_to_counter_leaf::<V>(n)?;
    bincode_compat::deserialize::<V>(&bytes).ok()
}

/// The user-facing `i64` return for a counter's new count.
///
/// The public `increment` / `fetch_add` API returns `i64` — the established
/// convention (both the byte and char tries have always returned `i64` here, and the
/// public delta argument is `i64`). For a `u64` counter whose count exceeds
/// `i64::MAX` this returns the **i64 bit-pattern** (a negative `i64`), which a
/// `u64`-typed caller recovers exactly via `as u64` (the bit patterns are identical).
/// The SOURCE OF TRUTH is the leaf (a correct `u64`); a caller that needs the full
/// unsigned magnitude reads it via `get_value` / `get_lockfree` (which return `u64`).
/// This `as i64` is confined to this module (the gate) — the ONE intentional,
/// documented, lossless-by-bit-pattern narrowing.
#[inline]
pub fn counter_return_i64(n: i128) -> i64 {
    n as i64
}

/// Split a non-negative `u64` magnitude into a minimal list of `i64`-bounded chunks
/// that sum (commutatively, in the `i128`/`u64` domain) back to it.
///
/// The WAL `BatchIncrement` delta field is `i64` (the on-disk format is unchanged),
/// but a *merge* delta is a source counter's full value, which a `u64` counter lets
/// reach up to `u64::MAX > i64::MAX`. Because `BatchIncrement` replay is commutative
/// (deltas are summed in the `i128` substrate, then bounded to `[0, u64::MAX]`), a
/// single over-`i64::MAX` delta is logged as ≤3 chunks each `≤ i64::MAX`; replay sums
/// them to the same total. This makes full-`u64` merge support complete without
/// widening the WAL format. (`u64::MAX < 3 * i64::MAX`, so ≤3 chunks always suffice.)
#[inline]
pub fn split_u64_delta_to_i64_chunks(delta: u64) -> Vec<i64> {
    if delta == 0 {
        return vec![0];
    }
    let mut remaining = delta;
    let cap = i64::MAX as u64;
    let mut chunks = Vec::with_capacity(3);
    while remaining > 0 {
        let take = remaining.min(cap);
        chunks.push(take as i64);
        remaining -= take;
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    // The make-or-break case: a u64 counter whose value EXCEEDS i64::MAX must read,
    // arithmetic, and write back correctly — the exact corruption the i64-domain
    // path produced.
    #[test]
    fn u64_above_i64_max_round_trips_and_increments() {
        let big: u64 = (i64::MAX as u64) + 10; // 9_223_372_036_854_775_817
        let leaf = big.to_le_bytes().to_vec();

        // Read as the full unsigned magnitude (NOT a negative i64).
        let n = counter_leaf_to_i128::<u64>(&leaf).expect("u64 leaf decodes");
        assert_eq!(n, big as i128);
        assert!(
            n > i64::MAX as i128,
            "must be the unsigned magnitude, not wrapped"
        );

        // Increment in the i128 domain, re-encode for u64.
        let incremented = i128_to_counter_leaf::<u64>(n + 5).expect("in range");
        assert_eq!(incremented, (big + 5).to_le_bytes().to_vec());

        // Decrement back across the i64::MAX boundary stays correct.
        let decremented = counter_leaf_to_i128::<u64>(&incremented).expect("decode") - 15;
        assert_eq!(decremented, (i64::MAX as i128) + 0);
    }

    #[test]
    fn u64_overflow_and_underflow_are_rejected() {
        // u64::MAX + 1 → out of range.
        assert_eq!(i128_to_counter_leaf::<u64>(u64::MAX as i128 + 1), None);
        // below zero → out of range (a decrement past 0).
        assert_eq!(i128_to_counter_leaf::<u64>(-1), None);
        assert_eq!(i128_to_counter_value::<u64>(-1), None);
        // u64::MAX itself round-trips.
        let at_max = i128_to_counter_leaf::<u64>(u64::MAX as i128).expect("u64::MAX ok");
        assert_eq!(at_max, u64::MAX.to_le_bytes().to_vec());
        assert_eq!(counter_leaf_to_i128::<u64>(&at_max), Some(u64::MAX as i128));
    }

    #[test]
    fn i64_range_round_trips_including_negative() {
        for v in [0i64, 1, -1, i64::MAX, i64::MIN, -42, 12345] {
            let leaf = v.to_le_bytes().to_vec();
            let n = counter_leaf_to_i128::<i64>(&leaf).expect("i64 leaf decodes");
            assert_eq!(n, v as i128);
            assert_eq!(i128_to_counter_leaf::<i64>(n), Some(leaf));
        }
        // i64 rejects out-of-i64-range (e.g. a value only u64 could hold).
        assert_eq!(i128_to_counter_leaf::<i64>(i64::MAX as i128 + 1), None);
        assert_eq!(i128_to_counter_leaf::<i64>(i64::MIN as i128 - 1), None);
    }

    #[test]
    fn non_counter_v_is_graceful_none() {
        let leaf = [1u8, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(counter_leaf_to_i128::<u32>(&leaf), None);
        assert_eq!(counter_leaf_to_i128::<String>(&leaf), None);
        assert_eq!(i128_to_counter_leaf::<u32>(1), None);
        assert_eq!(i128_to_counter_value::<u32>(1), None);
    }

    #[test]
    fn malformed_leaf_is_none() {
        assert_eq!(counter_leaf_to_i128::<u64>(&[1, 2, 3]), None); // wrong length
        assert_eq!(counter_leaf_to_i128::<u64>(&[]), None);
    }

    #[test]
    fn leaf_bytes_are_byte_identical_to_bincode() {
        // The on-disk-format invariant: the helper's leaf == bincode(&(n as V)).
        for n in [
            0i128,
            1,
            42,
            i64::MAX as i128,
            (i64::MAX as i128) + 1,
            u64::MAX as i128,
        ] {
            let helper = i128_to_counter_leaf::<u64>(n).expect("in u64 range");
            let bincode = bincode_compat::serialize(&(n as u64)).expect("bincode u64");
            assert_eq!(helper, bincode, "u64 leaf must match bincode for n={}", n);
        }
        for n in [0i128, -1, 1, i64::MAX as i128, i64::MIN as i128] {
            let helper = i128_to_counter_leaf::<i64>(n).expect("in i64 range");
            let bincode = bincode_compat::serialize(&(n as i64)).expect("bincode i64");
            assert_eq!(helper, bincode, "i64 leaf must match bincode for n={}", n);
        }
    }

    #[test]
    fn counter_value_round_trips_through_typed_v() {
        let big: u64 = (i64::MAX as u64) + 100;
        let n = counter_value_to_i128::<u64>(&big).expect("typed u64 → i128");
        assert_eq!(n, big as i128);
        let back: u64 = i128_to_counter_value::<u64>(n).expect("i128 → typed u64");
        assert_eq!(back, big);
    }

    #[test]
    fn return_i64_is_bit_pattern_recoverable_as_u64() {
        let big: u64 = (i64::MAX as u64) + 7;
        let r = counter_return_i64(big as i128);
        assert!(
            r < 0,
            "a u64 count > i64::MAX returns a negative i64 bit-pattern"
        );
        assert_eq!(r as u64, big, "recoverable via `as u64`");
        // Values within i64 range are returned verbatim.
        assert_eq!(counter_return_i64(123), 123i64);
    }

    #[test]
    fn delta_split_sums_back_and_is_i64_bounded() {
        for delta in [
            0u64,
            1,
            i64::MAX as u64,
            (i64::MAX as u64) + 1,
            u64::MAX,
            u64::MAX - 1,
        ] {
            let chunks = split_u64_delta_to_i64_chunks(delta);
            assert!(chunks.len() <= 3, "≤3 chunks for delta={}", delta);
            assert!(
                chunks.iter().all(|&c| c >= 0),
                "all chunks non-negative for delta={}",
                delta
            );
            // Sum in the i128 substrate equals the original magnitude.
            let sum: i128 = chunks.iter().map(|&c| c as i128).sum();
            assert_eq!(sum, delta as i128, "chunks sum back for delta={}", delta);
        }
    }
}
