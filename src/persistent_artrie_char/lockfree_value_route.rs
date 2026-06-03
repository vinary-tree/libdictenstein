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

/// Route `insert_with_value(term, value)` to the overlay iff `V == u64`.
///
/// Returns `Some(result)` when handled by the overlay (`V = u64`), or `None` to
/// signal the caller should run its owned-tree body (arbitrary `V`). INSERT
/// semantics: an existing term is a no-op `Ok(false)` with no WAL.
pub(super) fn route_insert_with_value<V, S>(
    trie: &super::PersistentARTrieChar<V, S>,
    term: &str,
    value: &V,
) -> Option<Result<bool>>
where
    V: DictionaryValue,
    S: BlockStorage,
{
    let trie_u64 = (trie as &dyn Any).downcast_ref::<super::PersistentARTrieChar<u64, S>>()?;
    let v_u64 = (value as &dyn Any).downcast_ref::<u64>()?;
    Some(trie_u64.insert_cas_with_value_durable(term, *v_u64))
}

/// Route `upsert(term, value)` to the overlay iff `V == u64` (last-writer-wins).
///
/// Returns `Some(Ok(true))` if newly inserted, `Some(Ok(false))` if updated, or
/// `None` for arbitrary `V` (caller runs the owned body).
pub(super) fn route_upsert<V, S>(
    trie: &super::PersistentARTrieChar<V, S>,
    term: &str,
    value: &V,
) -> Option<Result<bool>>
where
    V: DictionaryValue,
    S: BlockStorage,
{
    let trie_u64 = (trie as &dyn Any).downcast_ref::<super::PersistentARTrieChar<u64, S>>()?;
    let v_u64 = (value as &dyn Any).downcast_ref::<u64>()?;
    Some(trie_u64.upsert_cas_durable(term, *v_u64))
}

/// Route `get_or_insert(term, default)` to the overlay iff `V == u64`.
///
/// Insert-if-absent (`insert_cas_with_value_durable` is a no-op on an existing
/// term), then read the resulting value back via `get_lockfree`. Returns the
/// current value (existing or the just-inserted default). `None` for arbitrary `V`.
pub(super) fn route_get_or_insert<V, S>(
    trie: &super::PersistentARTrieChar<V, S>,
    term: &str,
    default: &V,
) -> Option<Result<V>>
where
    V: DictionaryValue,
    S: BlockStorage,
{
    let trie_u64 = (trie as &dyn Any).downcast_ref::<super::PersistentARTrieChar<u64, S>>()?;
    let default_u64 = (default as &dyn Any).downcast_ref::<u64>()?;
    let result: Result<V> = (|| {
        // Insert the default if absent (no-op + Ok(false) if present, durable).
        trie_u64.insert_cas_with_value_durable(term, *default_u64)?;
        // Read the now-present value back (the existing value if it pre-existed,
        // else the default we just inserted).
        let v = trie_u64.get_lockfree(term).unwrap_or(*default_u64);
        // Re-wrap the u64 as V via the SAFE Any path (V == u64 here).
        let v_as_any: &dyn Any = &v;
        Ok(v_as_any
            .downcast_ref::<V>()
            .cloned()
            .expect("V == u64 in this routed branch"))
    })();
    Some(result)
}

/// Route `increment(term, delta)` to the overlay iff `V == u64` AND `delta >= 0`.
///
/// The overlay counter is add-only `BatchIncrement`, so a NEGATIVE delta returns
/// `None` (caller falls back to the owned tree — the design's "negative-delta
/// increment unsupported" gap). On success the new accumulated `u64` is widened to
/// `i64` (bounded by `LOCKFREE_COUNTER_MAX = i64::MAX`, so the widening never
/// overflows). Returns `None` for arbitrary `V`.
pub(super) fn route_increment<V, S>(
    trie: &super::PersistentARTrieChar<V, S>,
    term: &str,
    delta: i64,
) -> Option<Result<i64>>
where
    V: DictionaryValue,
    S: BlockStorage,
{
    let trie_u64 = (trie as &dyn Any).downcast_ref::<super::PersistentARTrieChar<u64, S>>()?;
    if delta < 0 {
        // Negative delta: the overlay counter cannot decrement — fall back to the
        // owned tree (documented gap; PS3-guarded).
        return None;
    }
    let delta_u64 = delta as u64;
    Some(
        trie_u64
            .try_increment_cas_durable(term, delta_u64)
            // The overlay value domain is bounded by i64::MAX, so the widening is
            // always lossless.
            .map(|v| v as i64),
    )
}
