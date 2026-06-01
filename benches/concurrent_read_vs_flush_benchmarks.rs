//! Concurrent read-vs-checkpoint benchmarks.
//!
//! Measures query (`contains`) throughput on a `SharedCharARTrie` in two
//! regimes:
//!
//!   1. **readers_only** — N reader threads, no concurrent writer.
//!   2. **readers_with_checkpoint** — the same N readers plus one background
//!      thread looping `upsert + checkpoint` (holds the trie write lock across
//!      its disk I/O).
//!
//! The ratio (regime 2 throughput) / (regime 1 throughput) quantifies how much
//! a concurrent checkpoint stalls reads. Because `SharedCharARTrie::contains`
//! takes the trie `RwLock` **read** side and `checkpoint`/`insert` take the
//! **write** side, the trie `RwLock` (L1) — not the inner buffer-manager
//! `lifecycle_lock` — is the lock that serializes reads against checkpoint.
//! This bench confirms/quantifies that and is the before/after regression gate
//! for the non-blocking-checkpoint rework.
//!
//! Run with:
//! ```bash
//! taskset -c 0-15 cargo bench --bench concurrent_read_vs_flush_benchmarks --features persistent-artrie
//! ```

#![cfg(feature = "persistent-artrie")]

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use libdictenstein::persistent_artrie_char::SharedCharARTrie;
use libdictenstein::{ARTrie, Dictionary};

/// Number of distinct keys inserted into the trie before benchmarking.
const KEY_COUNT: usize = 5_000;

/// Number of `contains` lookups each reader thread performs per sample.
const OPS_PER_READER: usize = 20_000;

/// Throttle between checkpoint rounds in the `with_checkpoint` regime. Without
/// a throttle the write-lock-holding checkpointer fully starves the readers
/// (they cannot complete a sample) — itself a demonstration that the trie
/// `RwLock` serializes reads against checkpoint. The throttle lets readers make
/// progress so the per-checkpoint stall can be measured rather than just
/// observed as starvation. It also bounds WAL-segment churn.
const CHECKPOINT_THROTTLE: Duration = Duration::from_millis(5);

/// Hard cap on checkpoint rounds per sample, so a slow reader sample cannot let
/// the checkpointer accumulate an unbounded number of archived WAL segments.
const MAX_CHECKPOINT_ROUNDS: usize = 1_000;

/// Reader-thread counts to sweep.
const READER_COUNTS: &[usize] = &[1, 2, 4, 8];

/// Deterministic key for index `i` (mixed ASCII + multi-byte to exercise UTF-8).
fn key_for(i: usize) -> String {
    format!("term-{:08}-キー", i)
}

/// Build and populate a disk-backed shared char trie; returns the handle and
/// the temp dir (kept alive to retain the backing file).
fn build_trie() -> (SharedCharARTrie<i64>, tempfile::TempDir) {
    // IMPORTANT: write the disk-backed trie to a real-disk scratch dir under
    // `target/`, NOT the default temp dir — on this system `$TMPDIR` is tmpfs
    // (RAM), and a disk-backed trie (plus checkpoint WAL churn) there consumes
    // RAM and can balloon to many GB.
    let scratch = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/bench-scratch");
    std::fs::create_dir_all(&scratch).expect("create real-disk scratch dir");
    let dir = tempfile::Builder::new()
        .prefix("read_vs_flush")
        .tempdir_in(&scratch)
        .expect("temp dir on real disk");
    let path = dir.path().join("read_vs_flush.part");
    let trie: SharedCharARTrie<i64> = ARTrie::create(&path).expect("create shared char trie");
    for i in 0..KEY_COUNT {
        ARTrie::insert_with_value(&trie, &key_for(i), i as i64);
    }
    ARTrie::checkpoint(&trie).expect("initial checkpoint");
    (trie, dir)
}

/// Run `n_readers` reader threads (each doing `OPS_PER_READER` lookups). When
/// `with_checkpoint` is set, also run one background thread that upserts an
/// existing key and checkpoints in a loop until the readers finish. Returns the
/// wall-clock time the readers took (the background thread is excluded from the
/// timed region but runs concurrently with it).
fn time_readers(trie: &SharedCharARTrie<i64>, n_readers: usize, with_checkpoint: bool) -> Duration {
    let stop = Arc::new(AtomicBool::new(false));
    let extra = usize::from(with_checkpoint);
    let barrier = Arc::new(Barrier::new(n_readers + extra + 1));

    let mut readers = Vec::with_capacity(n_readers);
    for t in 0..n_readers {
        let trie = Arc::clone(trie);
        let barrier = Arc::clone(&barrier);
        readers.push(thread::spawn(move || {
            barrier.wait();
            let mut hits = 0usize;
            for op in 0..OPS_PER_READER {
                // Cheap per-thread LCG-ish index spread so threads touch
                // different keys without allocating a permutation.
                let idx = op.wrapping_mul(2_654_435_761).wrapping_add(t * 7) % KEY_COUNT;
                if Dictionary::contains(&trie, &key_for(idx)) {
                    hits += 1;
                }
            }
            black_box(hits)
        }));
    }

    let checkpointer = with_checkpoint.then(|| {
        let trie = Arc::clone(trie);
        let barrier = Arc::clone(&barrier);
        let stop = Arc::clone(&stop);
        thread::spawn(move || {
            barrier.wait();
            let mut rounds = 0usize;
            while !stop.load(Ordering::Relaxed) && rounds < MAX_CHECKPOINT_ROUNDS {
                // Upsert an existing key (dirties without growing the trie),
                // then checkpoint so the write lock is held across real I/O.
                ARTrie::insert_with_value(&trie, &key_for(rounds % KEY_COUNT), rounds as i64);
                let _ = ARTrie::checkpoint(&trie);
                rounds += 1;
                thread::sleep(CHECKPOINT_THROTTLE);
            }
            black_box(rounds)
        })
    });

    barrier.wait();
    let start = Instant::now();
    for r in readers {
        let _ = r.join();
    }
    let elapsed = start.elapsed();
    stop.store(true, Ordering::Relaxed);
    if let Some(c) = checkpointer {
        let _ = c.join();
    }
    elapsed
}

fn bench_read_vs_flush(c: &mut Criterion) {
    let (trie, _dir) = build_trie();

    let mut group = c.benchmark_group("concurrent_read_vs_flush");
    // Concurrent samples are expensive; keep the run bounded.
    group.sample_size(10);
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(5));

    for &n in READER_COUNTS {
        group.throughput(Throughput::Elements((n * OPS_PER_READER) as u64));

        group.bench_with_input(BenchmarkId::new("readers_only", n), &n, |b, &n| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    total += time_readers(&trie, n, false);
                }
                total
            });
        });

        group.bench_with_input(BenchmarkId::new("readers_with_checkpoint", n), &n, |b, &n| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    total += time_readers(&trie, n, true);
                }
                total
            });
        });
    }

    group.finish();
}

criterion_group!(benches, bench_read_vs_flush);
criterion_main!(benches);
