//! Deduplicating arena correspondence checks for the persistent byte and char
//! ARTs.
//!
//! These tests exercise the Rust side of `DedupArenaSpec.v`: verified cache
//! hits reuse only slots that still contain the requested payload, stale cache
//! entries fail closed by allocating fresh slots, clear/take remove local cache
//! evidence, and the compatibility setter cannot re-enable trusted hash hits.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::{
    ArenaManager as ByteArenaManager, ArenaSlot as ByteArenaSlot,
    BatchDeduplicator as ByteBatchDeduplicator,
    DeduplicatingArenaManager as ByteDeduplicatingArenaManager, MmapDiskManager,
};
use libdictenstein::persistent_artrie_char::{
    ArenaManager as CharArenaManager, ArenaSlot as CharArenaSlot,
    BatchDeduplicator as CharBatchDeduplicator,
    DeduplicatingArenaManager as CharDeduplicatingArenaManager,
};
use std::collections::BTreeMap;

#[test]
fn byte_verified_dedup_matches_reference_trace() {
    let arena = ByteArenaManager::<MmapDiskManager>::with_arena_size(4096);
    let mut dedup = ByteDeduplicatingArenaManager::new(arena);
    let mut reference = BTreeMap::<Vec<u8>, ByteArenaSlot>::new();
    let trace: [&[u8]; 5] = [b"alpha", b"beta", b"alpha", b"gamma", b"beta"];

    for payload in trace {
        let slot = dedup.allocate_dedup(payload).expect("dedup allocation");
        if let Some(expected) = reference.get(payload) {
            assert_eq!(slot, *expected, "equal bytes must reuse live slot");
        } else {
            reference.insert(payload.to_vec(), slot);
            assert_eq!(dedup.read(slot).unwrap(), payload);
        }
    }

    let stats = dedup.dedup_stats();
    assert_eq!(stats.cache_size, 3);
    assert_eq!(stats.hits, 2);
    assert_eq!(stats.misses, 3);
    assert_eq!(stats.collisions, 0);
    assert!((stats.hit_rate() - 0.4).abs() < f64::EPSILON);
}

#[test]
fn char_verified_dedup_matches_reference_trace() {
    let arena = CharArenaManager::<MmapDiskManager>::with_arena_size(4096);
    let mut dedup = CharDeduplicatingArenaManager::new(arena);
    let mut reference = BTreeMap::<Vec<u8>, CharArenaSlot>::new();
    let trace: [&[u8]; 5] = [
        "alpha".as_bytes(),
        "beta".as_bytes(),
        "alpha".as_bytes(),
        "gamma".as_bytes(),
        "beta".as_bytes(),
    ];

    for payload in trace {
        let slot = dedup.allocate_dedup(payload).expect("dedup allocation");
        if let Some(expected) = reference.get(payload) {
            assert_eq!(slot, *expected, "equal bytes must reuse live slot");
        } else {
            reference.insert(payload.to_vec(), slot);
            assert_eq!(dedup.read(slot).unwrap(), payload);
        }
    }

    let stats = dedup.dedup_stats();
    assert_eq!(stats.cache_size, 3);
    assert_eq!(stats.hits, 2);
    assert_eq!(stats.misses, 3);
    assert_eq!(stats.collisions, 0);
}

#[test]
fn byte_verified_stale_cache_allocates_fresh_slot() {
    let arena = ByteArenaManager::<MmapDiskManager>::with_arena_size(4096);
    let mut dedup = ByteDeduplicatingArenaManager::new(arena);

    let original = b"node-a";
    let replacement = b"node-b";
    let first = dedup.allocate_dedup(original).expect("first allocation");
    dedup
        .arena_manager_mut()
        .update(first, replacement)
        .expect("stale cached slot mutation");

    let second = dedup
        .allocate_dedup(original)
        .expect("fresh allocation after stale cache");

    assert_ne!(first, second, "verified stale hit must not alias");
    assert_eq!(dedup.read(first).unwrap(), replacement);
    assert_eq!(dedup.read(second).unwrap(), original);

    let stats = dedup.dedup_stats();
    assert_eq!(stats.hits, 0);
    assert_eq!(stats.misses, 2);
    assert_eq!(stats.collisions, 1);
}

#[test]
fn char_verified_stale_cache_allocates_fresh_slot() {
    let arena = CharArenaManager::<MmapDiskManager>::with_arena_size(4096);
    let mut dedup = CharDeduplicatingArenaManager::new(arena);

    let original = "arbor1".as_bytes();
    let replacement = "arbor2".as_bytes();
    let first = dedup.allocate_dedup(original).expect("first allocation");
    dedup
        .arena_manager_mut()
        .update(first, replacement)
        .expect("stale cached slot mutation");

    let second = dedup
        .allocate_dedup(original)
        .expect("fresh allocation after stale cache");

    assert_ne!(first, second, "verified stale hit must not alias");
    assert_eq!(dedup.read(first).unwrap(), replacement);
    assert_eq!(dedup.read(second).unwrap(), original);

    let stats = dedup.dedup_stats();
    assert_eq!(stats.hits, 0);
    assert_eq!(stats.misses, 2);
    assert_eq!(stats.collisions, 1);
}

#[test]
fn byte_verify_false_request_still_fails_closed_on_stale_cache() {
    let arena = ByteArenaManager::<MmapDiskManager>::with_arena_size(4096);
    let mut dedup = ByteDeduplicatingArenaManager::new(arena);
    dedup.set_verify_on_hit(false);

    let requested = b"same";
    let stale = b"risk";
    let first = dedup.allocate_dedup(requested).expect("first allocation");
    dedup
        .arena_manager_mut()
        .update(first, stale)
        .expect("stale cached slot mutation");

    let second = dedup
        .allocate_dedup(requested)
        .expect("verified allocation after stale cache");

    assert_ne!(
        first, second,
        "verify=false request must not alias stale data"
    );
    assert_eq!(dedup.read(first).unwrap(), stale);
    assert_eq!(dedup.read(second).unwrap(), requested);

    let stats = dedup.dedup_stats();
    assert_eq!(stats.hits, 0);
    assert_eq!(stats.misses, 2);
    assert_eq!(stats.collisions, 1);
}

#[test]
fn clear_dedup_cache_removes_reuse_evidence_and_resets_stats() {
    let arena = ByteArenaManager::<MmapDiskManager>::with_arena_size(4096);
    let mut dedup = ByteDeduplicatingArenaManager::new(arena);
    let payload = b"clear-me";

    let first = dedup.allocate_dedup(payload).expect("first allocation");
    let reused = dedup.allocate_dedup(payload).expect("second allocation");
    assert_eq!(first, reused);
    assert_eq!(dedup.dedup_stats().hits, 1);

    dedup.clear_dedup_cache();

    let after_clear = dedup
        .allocate_dedup(payload)
        .expect("allocation after cache clear");
    assert_ne!(
        first, after_clear,
        "post-clear allocation must not fabricate a hit"
    );
    assert_eq!(dedup.read(after_clear).unwrap(), payload);

    let stats = dedup.dedup_stats();
    assert_eq!(stats.cache_size, 1);
    assert_eq!(stats.hits, 0);
    assert_eq!(stats.misses, 1);
    assert_eq!(stats.collisions, 0);
}

#[test]
fn direct_allocation_bypasses_cache_without_invalidating_cached_hit() {
    let arena = ByteArenaManager::<MmapDiskManager>::with_arena_size(4096);
    let mut dedup = ByteDeduplicatingArenaManager::new(arena);
    let payload = b"direct-bypass";

    let cached = dedup.allocate_dedup(payload).expect("cached allocation");
    let direct = dedup.allocate_direct(payload).expect("direct allocation");
    let reused = dedup.allocate_dedup(payload).expect("cached reuse");

    assert_ne!(cached, direct);
    assert_eq!(cached, reused);
    assert_eq!(dedup.read(direct).unwrap(), payload);

    let stats = dedup.dedup_stats();
    assert_eq!(stats.hits, 1);
    assert_eq!(stats.misses, 1);
    assert_eq!(stats.collisions, 0);
}

#[test]
fn byte_batch_take_returns_local_cache_and_clears_batch() {
    let mut batch = ByteBatchDeduplicator::new(2);
    let slot_a = ByteArenaSlot::new(0, 0);
    let slot_b = ByteArenaSlot::new(0, 1);

    batch.insert(b"a", slot_a);
    assert_eq!(batch.lookup(b"a"), Some(slot_a));
    assert!(!batch.should_merge());

    batch.insert(b"b", slot_b);
    assert!(batch.should_merge());

    let taken = batch.take();
    assert_eq!(taken.lookup(b"a"), Some(slot_a));
    assert_eq!(taken.lookup(b"b"), Some(slot_b));
    assert!(batch.is_empty());
    assert_eq!(batch.lookup(b"a"), None);
}

#[test]
fn char_batch_take_returns_local_cache_and_clears_batch() {
    let mut batch = CharBatchDeduplicator::new(1);
    let slot = CharArenaSlot::new(0, 0);

    batch.insert("leaf".as_bytes(), slot);
    assert!(batch.should_merge());

    let taken = batch.take();
    assert_eq!(taken.lookup("leaf".as_bytes()), Some(slot));
    assert!(batch.is_empty());
    assert_eq!(batch.lookup("leaf".as_bytes()), None);
}
