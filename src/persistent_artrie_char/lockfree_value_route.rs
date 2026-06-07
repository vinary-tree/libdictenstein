//! Per-monomorph routing for the VALUED production mutators under the lock-free
//! flip (`insert_with_value` / `increment` / `upsert`).
//!
//! # Why this module exists
//!
//! The lock-free overlay's value-write path
//! ([`build_value_path_recursive`](super::PersistentARTrieChar)) is hardcoded
//! `<u64>` (the n-gram counter domain; arbitrary `V` needs the G1 single-phase
//! genericization, out of scope for the flip — design §1). So the VALUED
//! production mutators must route to the overlay ONLY for the `V = u64` monomorph
//! and stay on the proven owned tree for every other `V` (the design's "arbitrary
//! V → forced OwnedTree" gap).
//!
//! Rust has no stable specialization, and the generic mutators live in one
//! `impl<V: DictionaryValue, S>` block that cannot name `PersistentARTrieChar<u64,
//! S>`'s inherent `*_cas_durable` methods. The idiomatic, **zero-unsafe** solution
//! is a free function that does a SAFE [`Any`](std::any::Any) downcast of `&self`
//! to the `u64` monomorph (`DictionaryValue: 'static`, and the trie + block
//! storage are `'static`), then calls the thin Order-A overlay primitives. When
//! the downcast fails (`V != u64`) it returns `None`, signalling the caller to run
//! its proven owned-tree body. The owned arms are unchanged.

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::error::Result;
use crate::value::DictionaryValue;
use std::any::Any;

// ============================================================================
// SUPERSEDED in Flip F0 (G5) — value routes are now GENERIC, not u64-downcast.
// `route_insert_with_value` / `route_upsert` / `route_get_or_insert` are commented
// out (NOT deleted — F0 is reversible) because the VALUED mutators
// (`insert_with_value`/`upsert`/`get_or_insert`/`compare_and_swap`/`insert_batch`)
// now route to the SHARED GENERIC `DurableOverlayWrite::*_default` methods, which
// work for ANY `V` via the `value_publish_inner` seam over the (now generic)
// `build_value_path_recursive`. The `Any`-downcast-to-u64 they did is the exact
// NH1 data-loss footgun the design removes: for arbitrary `V` post-flip it returned
// `None` → the caller fell through to an owned write that is unranked → dropped on
// Overlay-regime reopen. Only `route_increment` (below) keeps the downcast — the
// counter RMW is legitimately `{u64,i64}`-only (NH3).
//
// /// Route `insert_with_value(term, value)` to the overlay iff `V == u64`.
// pub(super) fn route_insert_with_value<V, S>(
//     trie: &super::PersistentARTrieChar<V, S>, term: &str, value: &V,
// ) -> Option<Result<bool>>
// where V: DictionaryValue, S: BlockStorage {
//     let trie_u64 = (trie as &dyn Any).downcast_ref::<super::PersistentARTrieChar<u64, S>>()?;
//     let v_u64 = (value as &dyn Any).downcast_ref::<u64>()?;
//     Some(trie_u64.insert_cas_with_value_durable(term, *v_u64))
// }
//
// /// Route `upsert(term, value)` to the overlay iff `V == u64` (last-writer-wins).
// pub(super) fn route_upsert<V, S>(
//     trie: &super::PersistentARTrieChar<V, S>, term: &str, value: &V,
// ) -> Option<Result<bool>>
// where V: DictionaryValue, S: BlockStorage {
//     let trie_u64 = (trie as &dyn Any).downcast_ref::<super::PersistentARTrieChar<u64, S>>()?;
//     let v_u64 = (value as &dyn Any).downcast_ref::<u64>()?;
//     Some(trie_u64.upsert_cas_durable(term, *v_u64))
// }
//
// /// Route `get_or_insert(term, default)` to the overlay iff `V == u64`.
// pub(super) fn route_get_or_insert<V, S>(
//     trie: &super::PersistentARTrieChar<V, S>, term: &str, default: &V,
// ) -> Option<Result<V>>
// where V: DictionaryValue, S: BlockStorage {
//     let trie_u64 = (trie as &dyn Any).downcast_ref::<super::PersistentARTrieChar<u64, S>>()?;
//     let default_u64 = (default as &dyn Any).downcast_ref::<u64>()?;
//     ... (insert-if-absent then read-back; see git history / the generic default) ...
// }
// ============================================================================

/// Route `increment(term, delta)` to the overlay iff `V == u64` AND `delta >= 0`.
///
/// The overlay counter is add-only `BatchIncrement`, so a NEGATIVE delta returns
/// `None` (the caller falls to the value-CAS path `increment_via_value_cas`, which
/// handles the decrement / below-zero reject). On success the new accumulated `u64`
/// is widened to `i128` (lossless — the ONE cast the counter-codec gate sanctions) so
/// the V-aware caller in `atomic_ops` re-encodes it via `i128_to_counter_value::<V>`.
/// Returns `None` for arbitrary `V`.
pub(super) fn route_increment<V, S>(
    trie: &super::PersistentARTrieChar<V, S>,
    term: &str,
    delta: i64,
) -> Option<Result<i128>>
where
    V: DictionaryValue,
    S: BlockStorage,
{
    let trie_u64 = (trie as &dyn Any).downcast_ref::<super::PersistentARTrieChar<u64, S>>()?;
    if delta < 0 {
        // Negative delta: the overlay counter cannot decrement — fall back to the
        // value-CAS path (documented gap; PS3-guarded).
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
