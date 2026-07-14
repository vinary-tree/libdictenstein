//! C1 (in-memory) — `DynamicDawg::update_or_insert_bytes`: the lock-free `&self`,
//! raw-byte twin of `update_or_insert`, for libgrammstein's in-memory MKN training
//! path (LEB128 term-id byte keys → arbitrary `V`). These exercise the same
//! no-lost-update / one-insert-winner invariants the `&str`/`&[u64]` variants have,
//! keyed on raw bytes (including non-UTF-8 and the empty key).

use libdictenstein::dynamic_dawg::DynamicDawg;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

/// N threads hammer the SAME byte key with an increment closure; the final value
/// equals the total number of calls — the per-node arc-swap CAS retries rather than
/// losing an update. Covers the empty key too.
#[test]
fn dawg_update_or_insert_bytes_concurrent_no_lost_updates() {
    fn assert_counter(key: Vec<u8>) {
        const WRITERS: usize = 8;
        const INCREMENTS: usize = 64;
        let dawg = Arc::new(DynamicDawg::<i64>::new());
        // Pre-insert the key so every racing call takes the UPDATE branch: this
        // isolates the no-lost-update invariant (all N calls +1 ⇒ N) from the
        // insert-winner race, which is covered by the one_insert_winner test.
        assert!(
            dawg.insert_bytes_with_value(&key, 0),
            "pre-insert seed for key {key:?}"
        );

        let key = Arc::new(key);
        let barrier = Arc::new(Barrier::new(WRITERS));
        let mut handles = Vec::with_capacity(WRITERS);
        for _ in 0..WRITERS {
            let dawg = Arc::clone(&dawg);
            let key = Arc::clone(&key);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                for _ in 0..INCREMENTS {
                    dawg.update_or_insert_bytes(&key, 0, |v| *v += 1);
                }
            }));
        }
        for h in handles {
            h.join().expect("writer thread");
        }
        assert_eq!(
            dawg.get_bytes_value(&key),
            Some((WRITERS * INCREMENTS) as i64),
            "every increment landed (no lost updates) for key {key:?}"
        );
    }
    assert_counter(b"hot-key".to_vec());
    assert_counter(Vec::new()); // the empty key IS the root
}

/// All writers race the same FRESH key; exactly one call reports `true` (the insert),
/// and the final value equals total-calls − 1 (the inserter stores the default `0`
/// without applying the closure; every other call adds 1).
#[test]
fn dawg_update_or_insert_bytes_concurrent_one_insert_winner() {
    const WRITERS: usize = 8;
    const INCREMENTS: usize = 64;
    let dawg = Arc::new(DynamicDawg::<i64>::new());

    let barrier = Arc::new(Barrier::new(WRITERS));
    let insert_winners = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::with_capacity(WRITERS);
    for _ in 0..WRITERS {
        let dawg = Arc::clone(&dawg);
        let barrier = Arc::clone(&barrier);
        let insert_winners = Arc::clone(&insert_winners);
        handles.push(thread::spawn(move || {
            barrier.wait();
            for _ in 0..INCREMENTS {
                if dawg.update_or_insert_bytes(b"race", 0, |v| *v += 1) {
                    insert_winners.fetch_add(1, Ordering::Relaxed);
                }
            }
        }));
    }
    for h in handles {
        h.join().expect("writer thread");
    }
    assert_eq!(
        insert_winners.load(Ordering::Relaxed),
        1,
        "exactly one call inserts; the rest update"
    );
    assert_eq!(
        dawg.get_bytes_value(b"race"),
        Some((WRITERS * INCREMENTS - 1) as i64),
        "insert stores the default (0) without the closure; the other calls each +1"
    );
}

/// Arbitrary byte keys — `0x00`, `0x7F`, `0x80`, `0xFF`, a mixed non-UTF-8 key, the
/// empty key, and a long (>64 B) key — each round-trips through insert-then-update.
/// First call inserts the default and returns `true`; second applies the closure and
/// returns `false`.
#[test]
fn dawg_update_or_insert_bytes_key_coverage_insert_then_update() {
    let dawg = DynamicDawg::<i64>::new();

    let keys: Vec<Vec<u8>> = vec![
        b"".to_vec(),
        b"\x00".to_vec(),
        b"\x7f".to_vec(),
        b"\x80".to_vec(),
        b"\xff".to_vec(),
        b"\x00\xff\x80\x7f".to_vec(),
        vec![0xABu8; 100], // long (>64 B) key
    ];
    for key in &keys {
        assert!(
            dawg.update_or_insert_bytes(key, 10, |v| *v += 5),
            "first call inserts key {key:?} → true"
        );
        assert_eq!(
            dawg.get_bytes_value(key),
            Some(10),
            "default stored for key {key:?}"
        );

        assert!(
            !dawg.update_or_insert_bytes(key, 999, |v| *v += 5),
            "second call updates key {key:?} → false"
        );
        assert_eq!(
            dawg.get_bytes_value(key),
            Some(15),
            "closure applied (10+5) for key {key:?}; default 999 ignored on update"
        );
    }
}

/// C2 in-memory: `iter_bytes_with_values()` yields every `(Vec<u8>, V)` pair with
/// lossless raw-byte keys — non-UTF-8 keys round-trip byte-for-byte (it is the
/// already-valued `iter_bytes` under a uniform name).
#[test]
fn dawg_iter_bytes_with_values_roundtrips_non_utf8_keys() {
    let dawg = DynamicDawg::<i64>::new();
    let expected: std::collections::BTreeMap<Vec<u8>, i64> = [
        (b"\x80".to_vec(), 1i64),
        (b"\x00\x01".to_vec(), 2),
        (b"\xff\xfe".to_vec(), 3),
        (vec![0xC0, 0x80], 4),
    ]
    .into_iter()
    .collect();
    for (k, v) in &expected {
        assert!(dawg.update_or_insert_bytes(k, *v, |_| {}));
    }
    let got: std::collections::BTreeMap<Vec<u8>, i64> = dawg.iter_bytes_with_values().collect();
    assert_eq!(
        got, expected,
        "every non-UTF-8 key round-trips byte-for-byte with its value"
    );
}
