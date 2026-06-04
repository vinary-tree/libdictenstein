//! Per-monomorph routing for the VALUED byte production mutators under the
//! lock-free flip (`increment_bytes` / `upsert_bytes` / `get_or_insert_bytes`).
//!
//! # Why this module exists (the byte twin of char's `lockfree_value_route`)
//!
//! The byte lock-free overlay's value-write path
//! ([`build_value_path_recursive`](super::dict_impl::PersistentARTrie)) and the
//! valued Order-A durable primitives (`insert_cas_with_value_durable` /
//! `upsert_cas_durable` / `try_increment_cas_durable` / `get_lockfree`) are all
//! defined on the `impl<S> PersistentARTrie<i64, S>` block (the byte counter
//! monomorph; char's is `u64`). So the VALUED production mutators — which live in
//! one generic `impl<V: DictionaryValue + Serialize + DeserializeOwned, S>` block
//! that cannot name `PersistentARTrie<i64, S>`'s inherent methods — must route to
//! the overlay ONLY for the `V = i64` monomorph and stay on the proven owned tree
//! for every other `V` (the design's "arbitrary V → forced OwnedTree" gap; an
//! ineligible `V` never has `route_overlay()` true anyway, since the F5 flip never
//! enables the overlay for it).
//!
//! Rust has no stable specialization, so — exactly as char — the idiomatic,
//! **zero-unsafe** solution is a free function that does a SAFE
//! [`Any`](std::any::Any) downcast of `&self` to the `i64` monomorph
//! (`DictionaryValue: 'static`, and the trie + block storage are `'static`), then
//! calls the thin Order-A overlay primitives. When the downcast fails (`V != i64`)
//! it returns `None`, signalling the caller to run its proven owned-tree body. The
//! owned arms are unchanged (the INERT-pre-flip property: with `route_overlay()`
//! false the routed branch is never entered).

use std::any::Any;

use super::block_storage::BlockStorage;
use super::dict_impl::PersistentARTrie;
use super::error::Result;
use crate::value::DictionaryValue;

/// Route `increment_bytes(term, delta)` to the overlay iff `V == i64`.
///
/// Returns `Some(result)` when handled by the overlay (`V = i64`), or `None` to
/// signal the caller should run its owned-tree body (arbitrary `V`). The byte
/// overlay counter domain is a non-negative `i64`; a NEGATIVE delta is rejected by
/// the durable primitive (the C4 value-domain bound) — UNLIKE char, byte does NOT
/// fall back to the owned tree for a negative delta, because the durable path
/// fails LOUD (the byte audit's reject discipline) rather than silently routing a
/// decrement to a different durable store. `try_increment_cas_durable` itself
/// rejects a non-UTF-8 key (the byte durable increment operates on UTF-8 keys).
pub(super) fn route_increment_bytes<V, S>(
    trie: &PersistentARTrie<V, S>,
    term: &[u8],
    delta: i64,
) -> Option<Result<i64>>
where
    V: DictionaryValue,
    S: BlockStorage,
{
    let trie_i64 = (trie as &dyn Any).downcast_ref::<PersistentARTrie<i64, S>>()?;
    // The durable primitive applies the C4 non-negative bound + the UTF-8 key
    // check; surface its result (incl. its loud rejects) directly.
    Some(trie_i64.try_increment_cas_durable(term, delta))
}

/// Route `insert_with_value(term, value)` to the overlay iff `V == i64`.
///
/// Returns `Some(result)` when handled by the overlay (`V = i64`), or `None` to
/// signal the caller should run its owned-tree body (arbitrary `V`). INSERT
/// semantics (NOT upsert): an existing term is a no-op `Ok(false)` with no WAL —
/// the durable [`insert_cas_with_value_durable`](super::dict_impl::PersistentARTrie)
/// does not overwrite an existing value. The durable path rejects a negative value
/// (C4).
pub(super) fn route_insert_with_value_bytes<V, S>(
    trie: &PersistentARTrie<V, S>,
    term: &[u8],
    value: &V,
) -> Option<Result<bool>>
where
    V: DictionaryValue,
    S: BlockStorage,
{
    let trie_i64 = (trie as &dyn Any).downcast_ref::<PersistentARTrie<i64, S>>()?;
    let v_i64 = (value as &dyn Any).downcast_ref::<i64>()?;
    Some(trie_i64.insert_cas_with_value_durable(term, *v_i64))
}

/// Route `upsert_bytes(term, value)` to the overlay iff `V == i64` (last-writer-wins).
///
/// Returns `Some(Ok(true))` if newly inserted, `Some(Ok(false))` if updated, or
/// `None` for arbitrary `V` (caller runs the owned body). The durable primitive
/// rejects a negative value (C4).
pub(super) fn route_upsert_bytes<V, S>(
    trie: &PersistentARTrie<V, S>,
    term: &[u8],
    value: &V,
) -> Option<Result<bool>>
where
    V: DictionaryValue,
    S: BlockStorage,
{
    let trie_i64 = (trie as &dyn Any).downcast_ref::<PersistentARTrie<i64, S>>()?;
    let v_i64 = (value as &dyn Any).downcast_ref::<i64>()?;
    Some(trie_i64.upsert_cas_durable(term, *v_i64))
}

/// Route `get_or_insert_bytes(term, default)` to the overlay iff `V == i64`.
///
/// Insert-if-absent (`insert_cas_with_value_durable` is a no-op + `Ok(false)` on
/// an existing term, durable), then read the resulting value back via
/// `get_lockfree`. Returns the current value (existing or the just-inserted
/// default) re-wrapped as `V`. `None` for arbitrary `V`.
pub(super) fn route_get_or_insert_bytes<V, S>(
    trie: &PersistentARTrie<V, S>,
    term: &[u8],
    default: &V,
) -> Option<Result<V>>
where
    V: DictionaryValue,
    S: BlockStorage,
{
    let trie_i64 = (trie as &dyn Any).downcast_ref::<PersistentARTrie<i64, S>>()?;
    let default_i64 = (default as &dyn Any).downcast_ref::<i64>()?;
    let result: Result<V> = (|| {
        // Insert the default if absent (no-op + Ok(false) if present, durable).
        trie_i64.insert_cas_with_value_durable(term, *default_i64)?;
        // Read the now-present value back (the existing value if it pre-existed,
        // else the default we just inserted). `get_lockfree` widens the
        // non-negative i64 count to u64; narrow back losslessly (bounded by
        // LOCKFREE_COUNTER_MAX = i64::MAX).
        let v = trie_i64
            .get_lockfree(term)
            .map(|count| count as i64)
            .unwrap_or(*default_i64);
        // Re-wrap the i64 as V via the SAFE Any path (V == i64 here).
        let v_as_any: &dyn Any = &v;
        Ok(v_as_any
            .downcast_ref::<V>()
            .cloned()
            .expect("V == i64 in this routed branch"))
    })();
    Some(result)
}
