//! Per-monomorph routing for the VALUED byte production mutators under the
//! lock-free flip (`increment_bytes` / `upsert_bytes` / `get_or_insert_bytes`).
//!
//! # Why this module exists (the byte twin of char's `lockfree_value_route`)
//!
//! The byte lock-free overlay's value-write path
//! ([`build_value_path_recursive`](super::dict_impl::PersistentARTrie)) and the
//! valued Order-A durable primitives (`insert_cas_with_value_durable` /
//! `upsert_cas_durable` / `try_increment_cas_durable` / `get_lockfree`) are all
//! defined on the `impl<S> PersistentARTrie<u64, S>` block (the byte counter
//! monomorph, now `u64` matching char). So the VALUED production mutators — which
//! live in one generic `impl<V: DictionaryValue + Serialize + DeserializeOwned, S>`
//! block that cannot name `PersistentARTrie<u64, S>`'s inherent methods — must route
//! to the overlay ONLY for the `V = u64` monomorph and stay on the proven owned tree
//! for every other `V` (the design's "arbitrary V → forced OwnedTree" gap; an
//! ineligible `V` never has `route_overlay()` true anyway, since the F5 flip never
//! enables the overlay for it).
//!
//! Rust has no stable specialization, so — exactly as char — the idiomatic,
//! **zero-unsafe** solution is a free function that does a SAFE
//! [`Any`](std::any::Any) downcast of `&self` to the `u64` monomorph
//! (`DictionaryValue: 'static`, and the trie + block storage are `'static`), then
//! calls the thin Order-A overlay primitives. When the downcast fails (`V != u64`)
//! it returns `None`, signalling the caller to run its proven owned-tree body (the
//! value-CAS increment path, which handles an `i64` counter and decrements). The
//! owned arms are unchanged (the INERT-pre-flip property: with `route_overlay()`
//! false the routed branch is never entered).

use std::any::Any;

use super::block_storage::BlockStorage;
use super::dict_impl::PersistentARTrie;
use super::error::Result;
use crate::value::DictionaryValue;

/// Route `increment_bytes(term, delta)` to the overlay iff `V == u64` AND
/// `delta >= 0`.
///
/// The overlay counter is the add-only `BatchIncrement` seam, so a NEGATIVE delta
/// returns `None` (the caller falls to the value-CAS path
/// `increment_bytes_via_value_cas`, which reads → `cur + delta` → CAS and rejects a
/// below-zero result) — the byte twin of char's `route_increment`. Returns `None`
/// for arbitrary `V` too. On success the new accumulated count (a `u64`) is widened
/// to `i128` so the V-aware caller in `atomic_ops` re-encodes it via
/// `i128_to_counter_value::<V>` (the widening `as i128` is lossless and the ONE cast
/// the counter-codec gate sanctions). `try_increment_cas_durable` itself rejects a
/// non-UTF-8 key (the byte durable increment operates on UTF-8 keys).
pub(super) fn route_increment_bytes<V, S>(
    trie: &PersistentARTrie<V, S>,
    term: &[u8],
    delta: i64,
) -> Option<Result<i128>>
where
    V: DictionaryValue,
    S: BlockStorage,
{
    let trie_u64 = (trie as &dyn Any).downcast_ref::<PersistentARTrie<u64, S>>()?;
    if delta < 0 {
        // Decrement: the add-only overlay seam cannot subtract — fall back to the
        // caller's value-CAS path (which handles the below-zero reject).
        return None;
    }
    let delta_u64 = u64::try_from(delta).ok()?;
    Some(
        trie_u64
            .try_increment_cas_durable(term, delta_u64)
            // Widen the new u64 count to i128 (lossless) so the V-aware caller
            // converts via `i128_to_counter_value::<V>`.
            .map(|count| count as i128),
    )
}

// ============================================================================
// SUPERSEDED in Flip F0 (G5) — byte twin of the char supersession. The valued
// mutators (`insert_with_value`/`upsert`/`get_or_insert`/`compare_and_swap`/
// `insert_batch`) now route to the SHARED GENERIC `DurableOverlayWrite::*_default`
// methods (generic over `V` via the `value_publish_inner` seam), so these
// i64-downcast helpers are obsolete. Commented out (NOT deleted — F0 is reversible);
// only `route_increment_bytes` (above) keeps the downcast (counter is u64-only, NH3).
// The downcast-then-`None`-fallback they did is the NH1 data-loss footgun the design
// removes (arbitrary `V` → `None` → owned write → unranked → dropped on reopen).
//
// pub(super) fn route_insert_with_value_bytes<V, S>(trie, term, value) -> Option<Result<bool>>
//   { downcast to <u64>; Some(trie_u64.insert_cas_with_value_durable(term, *v_u64)) }
// pub(super) fn route_upsert_bytes<V, S>(trie, term, value) -> Option<Result<bool>>
//   { downcast to <u64>; Some(trie_u64.upsert_cas_durable(term, *v_u64)) }
// pub(super) fn route_get_or_insert_bytes<V, S>(trie, term, default) -> Option<Result<V>>
//   { downcast to <u64>; insert-if-absent then get_lockfree read-back }
// (Full bodies in git history; the generic `DurableOverlayWrite` defaults replace them.)
// ============================================================================
