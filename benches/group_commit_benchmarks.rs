//! Benchmarks for Group Commit performance.
//!
//! This benchmark measures:
//! - Single-threaded write throughput with/without group commit
//! - Multi-threaded write throughput (batching efficiency)
//! - Latency distribution (p50, p99)
//! - fsync reduction ratio
//!
//! Run with:
//! ```bash
//! cargo bench --bench group_commit_benchmarks --features persistent-artrie
//! ```

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use libdictenstein::persistent_artrie::{
    GroupCommitConfig, GroupCommitCoordinator, WalRecord, WalWriter,
};
use parking_lot::RwLock;
use std::sync::Arc;
use std::thread;
use tempfile::tempdir;

/// Number of operations per benchmark iteration
const OPS_PER_ITER: u64 = 100;

/// Benchmark single-threaded WAL writes without group commit (baseline).
fn bench_wal_direct(c: &mut Criterion) {
    let mut group = c.benchmark_group("wal_direct");
    group.throughput(Throughput::Elements(OPS_PER_ITER));

    group.bench_function("sequential_sync", |b| {
        let dir = tempdir().expect("create temp dir");
        let wal_path = dir.path().join("bench.wal");
        let wal = WalWriter::create(&wal_path).expect("create WAL");

        b.iter(|| {
            for i in 0..OPS_PER_ITER {
                let record = WalRecord::Insert {
                    term: format!("term{}", i).into_bytes(),
                    value: None,
                };
                let _ = black_box(wal.append(record).expect("append"));
            }
            wal.sync().expect("sync");
        });
    });

    group.bench_function("sync_per_op", |b| {
        let dir = tempdir().expect("create temp dir");
        let wal_path = dir.path().join("bench.wal");
        let wal = WalWriter::create(&wal_path).expect("create WAL");

        b.iter(|| {
            for i in 0..OPS_PER_ITER {
                let record = WalRecord::Insert {
                    term: format!("term{}", i).into_bytes(),
                    value: None,
                };
                let _ = black_box(wal.append(record).expect("append"));
                wal.sync().expect("sync");
            }
        });
    });

    group.finish();
}

/// Benchmark group commit with varying batch sizes (multi-threaded to actually trigger batching).
fn bench_group_commit_batch_sizes(c: &mut Criterion) {
    let mut group = c.benchmark_group("group_commit_batch_size");
    let num_threads = 4;
    let ops_per_thread = OPS_PER_ITER / num_threads as u64;
    group.throughput(Throughput::Elements(OPS_PER_ITER));

    for batch_size in [10, 50, 100].iter() {
        group.bench_with_input(
            BenchmarkId::from_parameter(batch_size),
            batch_size,
            |b, &batch_size| {
                let dir = tempdir().expect("create temp dir");
                let wal_path = dir.path().join("bench.wal");
                let wal = Arc::new(RwLock::new(WalWriter::create(&wal_path).expect("create WAL")));

                let config = GroupCommitConfig {
                    max_batch_size: batch_size,
                    max_batch_delay_us: 1_000, // 1ms max delay for quick batching
                    dedicated_commit_thread: true,
                    adaptive_batching: false,
                    ..Default::default()
                };

                let coordinator = Arc::new(
                    GroupCommitCoordinator::new(Arc::clone(&wal), config).expect("create coord"),
                );

                b.iter(|| {
                    let mut handles = Vec::with_capacity(num_threads);
                    for thread_id in 0..num_threads {
                        let coord = Arc::clone(&coordinator);
                        let handle = thread::spawn(move || {
                            for i in 0..ops_per_thread {
                                let record = WalRecord::Insert {
                                    term: format!("t{}i{}", thread_id, i).into_bytes(),
                                    value: None,
                                };
                                let _ = black_box(coord.append_with_sync(record).expect("append"));
                            }
                        });
                        handles.push(handle);
                    }
                    for handle in handles {
                        handle.join().expect("join");
                    }
                });
            },
        );
    }

    group.finish();
}

/// Benchmark group commit with varying thread counts.
fn bench_group_commit_concurrency(c: &mut Criterion) {
    let mut group = c.benchmark_group("group_commit_concurrency");
    let ops_per_thread = 50; // Smaller for faster benchmarks

    for num_threads in [1, 2, 4, 8].iter() {
        let ops_total = ops_per_thread * (*num_threads as u64);
        group.throughput(Throughput::Elements(ops_total));

        group.bench_with_input(
            BenchmarkId::from_parameter(num_threads),
            num_threads,
            |b, &num_threads| {
                let dir = tempdir().expect("create temp dir");
                let wal_path = dir.path().join("bench.wal");
                let wal = Arc::new(RwLock::new(WalWriter::create(&wal_path).expect("create WAL")));

                let config = GroupCommitConfig {
                    max_batch_size: 100,
                    max_batch_delay_us: 1_000, // 1ms max delay
                    dedicated_commit_thread: true,
                    adaptive_batching: false,
                    ..Default::default()
                };

                let coordinator = Arc::new(
                    GroupCommitCoordinator::new(Arc::clone(&wal), config).expect("create coord"),
                );

                b.iter(|| {
                    let mut handles = Vec::with_capacity(num_threads);

                    for thread_id in 0..num_threads {
                        let coord = Arc::clone(&coordinator);
                        let handle = thread::spawn(move || {
                            for i in 0..ops_per_thread {
                                let record = WalRecord::Insert {
                                    term: format!("t{}i{}", thread_id, i).into_bytes(),
                                    value: None,
                                };
                                let _ = black_box(coord.append_with_sync(record).expect("append"));
                            }
                        });
                        handles.push(handle);
                    }

                    for handle in handles {
                        handle.join().expect("join");
                    }
                });
            },
        );
    }

    group.finish();
}

/// Benchmark adaptive vs non-adaptive batching (multi-threaded).
fn bench_adaptive_batching(c: &mut Criterion) {
    let mut group = c.benchmark_group("group_commit_adaptive");
    let num_threads = 4;
    let ops_per_thread = OPS_PER_ITER / num_threads as u64;
    group.throughput(Throughput::Elements(OPS_PER_ITER));

    for adaptive in [false, true].iter() {
        let name = if *adaptive { "adaptive" } else { "fixed" };

        group.bench_function(name, |b| {
            let dir = tempdir().expect("create temp dir");
            let wal_path = dir.path().join("bench.wal");
            let wal = Arc::new(RwLock::new(WalWriter::create(&wal_path).expect("create WAL")));

            let config = GroupCommitConfig {
                max_batch_size: 100,
                max_batch_delay_us: 1_000, // 1ms
                dedicated_commit_thread: true,
                adaptive_batching: *adaptive,
                adaptive_latency_target_us: 2_000,
                ..Default::default()
            };

            let coordinator = Arc::new(
                GroupCommitCoordinator::new(Arc::clone(&wal), config).expect("create coord"),
            );

            b.iter(|| {
                let mut handles = Vec::with_capacity(num_threads);
                for thread_id in 0..num_threads {
                    let coord = Arc::clone(&coordinator);
                    let handle = thread::spawn(move || {
                        for i in 0..ops_per_thread {
                            let record = WalRecord::Insert {
                                term: format!("t{}i{}", thread_id, i).into_bytes(),
                                value: None,
                            };
                            let _ = black_box(coord.append_with_sync(record).expect("append"));
                        }
                    });
                    handles.push(handle);
                }
                for handle in handles {
                    handle.join().expect("join");
                }
            });
        });
    }

    group.finish();
}

/// Benchmark batching efficiency (records per fsync).
fn bench_batching_efficiency(c: &mut Criterion) {
    let mut group = c.benchmark_group("batching_efficiency");
    let num_threads = 8;
    let ops_per_thread = 100; // Reduced for faster benchmarking
    let total_ops = (num_threads * ops_per_thread) as u64;
    group.throughput(Throughput::Elements(total_ops));

    group.bench_function("8_threads_100_ops", |b| {
        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let dir = tempdir().expect("create temp dir");
                let wal_path = dir.path().join("bench.wal");
                let wal = Arc::new(RwLock::new(WalWriter::create(&wal_path).expect("create WAL")));

                let config = GroupCommitConfig {
                    max_batch_size: 100,
                    max_batch_delay_us: 1_000, // 1ms
                    dedicated_commit_thread: true,
                    adaptive_batching: false,
                    ..Default::default()
                };

                let coordinator = Arc::new(
                    GroupCommitCoordinator::new(Arc::clone(&wal), config).expect("create coord"),
                );

                let start = std::time::Instant::now();
                let mut handles = Vec::with_capacity(num_threads);

                for thread_id in 0..num_threads {
                    let coord = Arc::clone(&coordinator);
                    let handle = thread::spawn(move || {
                        for i in 0..ops_per_thread {
                            let record = WalRecord::Insert {
                                term: format!("t{}i{}", thread_id, i).into_bytes(),
                                value: None,
                            };
                            coord.append_with_sync(record).expect("append");
                        }
                    });
                    handles.push(handle);
                }

                for handle in handles {
                    handle.join().expect("join");
                }

                total_duration += start.elapsed();

                // Report efficiency
                let stats = coordinator.stats();
                let efficiency = stats.batching_efficiency();
                black_box(efficiency);
            }

            total_duration
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_wal_direct,
    bench_group_commit_batch_sizes,
    bench_group_commit_concurrency,
    bench_adaptive_batching,
    bench_batching_efficiency,
);

criterion_main!(benches);
