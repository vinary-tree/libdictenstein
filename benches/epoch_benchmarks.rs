//! Benchmarks for Epoch-Based Checkpointing.
//!
//! This benchmark measures:
//! - Write throughput with/without epoch checkpointing
//! - Epoch duration impact on throughput
//! - WAL size bounding effectiveness
//! - Recovery time from epoch checkpoints
//!
//! Run with:
//! ```bash
//! cargo bench --bench epoch_benchmarks --features persistent-artrie
//! ```

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use libdictenstein::persistent_artrie::{
    CheckpointManager, EpochConfig, WalWriter, WalRecord,
};
use std::time::Duration;
use tempfile::tempdir;

/// Number of operations per benchmark iteration
const OPS_PER_ITER: u64 = 1000;

/// Generate terms for benchmarking
fn generate_terms(count: usize) -> Vec<String> {
    (0..count).map(|i| format!("term_{:06}", i)).collect()
}

// ============================================================================
// Baseline: Direct WAL writes (no epoch management)
// ============================================================================

/// Benchmark direct WAL writes without epoch checkpointing (baseline).
fn bench_wal_baseline(c: &mut Criterion) {
    let mut group = c.benchmark_group("epoch_baseline");
    group.throughput(Throughput::Elements(OPS_PER_ITER));
    group.sample_size(30);

    let terms = generate_terms(OPS_PER_ITER as usize);

    group.bench_function("wal_direct", |b| {
        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let dir = tempdir().expect("create temp dir");
                let wal_path = dir.path().join("bench.wal");
                let wal = WalWriter::create(&wal_path).expect("create WAL");

                let start = std::time::Instant::now();
                for term in &terms {
                    let record = WalRecord::Insert {
                        term: term.as_bytes().to_vec(),
                        value: None,
                    };
                    let _ = black_box(wal.append(record).expect("append"));
                }
                wal.sync().expect("sync");
                total_duration += start.elapsed();
            }

            total_duration
        });
    });

    group.finish();
}

// ============================================================================
// Epoch-Based Checkpointing Throughput
// ============================================================================

/// Benchmark epoch-based checkpointing with varying epoch durations.
fn bench_epoch_duration(c: &mut Criterion) {
    let mut group = c.benchmark_group("epoch_duration");
    group.throughput(Throughput::Elements(OPS_PER_ITER));
    group.sample_size(30);

    // Test different epoch durations
    for duration_ms in [10, 50, 100, 500].iter() {
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}ms", duration_ms)),
            duration_ms,
            |b, &duration_ms| {
                b.iter_custom(|iters| {
                    let mut total_duration = std::time::Duration::ZERO;

                    for _ in 0..iters {
                        let dir = tempdir().expect("create temp dir");
                        let config = EpochConfig {
                            epoch_duration: Duration::from_millis(duration_ms as u64),
                            max_ops_per_epoch: 500, // Force epoch transitions
                            background_checkpoint: false, // Synchronous for measurement
                            ..Default::default()
                        };

                        let manager = CheckpointManager::new(dir.path(), config)
                            .expect("create manager");

                        let start = std::time::Instant::now();
                        for _ in 0..OPS_PER_ITER {
                            // Simulate write operation (record_operation + WAL)
                            manager.record_operation(100); // ~100 bytes per op
                        }
                        total_duration += start.elapsed();

                        black_box(manager.stats());
                    }

                    total_duration
                });
            },
        );
    }

    group.finish();
}

/// Benchmark epoch-based checkpointing with varying ops-per-epoch limits.
fn bench_epoch_ops_limit(c: &mut Criterion) {
    let mut group = c.benchmark_group("epoch_ops_limit");
    group.throughput(Throughput::Elements(OPS_PER_ITER));
    group.sample_size(30);

    // Test different ops-per-epoch limits
    for max_ops in [100, 250, 500, 1000].iter() {
        group.bench_with_input(
            BenchmarkId::from_parameter(max_ops),
            max_ops,
            |b, &max_ops| {
                b.iter_custom(|iters| {
                    let mut total_duration = std::time::Duration::ZERO;

                    for _ in 0..iters {
                        let dir = tempdir().expect("create temp dir");
                        let config = EpochConfig {
                            max_ops_per_epoch: max_ops,
                            background_checkpoint: false,
                            ..Default::default()
                        };

                        let manager = CheckpointManager::new(dir.path(), config)
                            .expect("create manager");

                        let start = std::time::Instant::now();
                        for _ in 0..OPS_PER_ITER {
                            manager.record_operation(100);
                        }
                        total_duration += start.elapsed();

                        black_box(manager.stats());
                    }

                    total_duration
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// WAL Size Bounding
// ============================================================================

/// Benchmark WAL size bounding under continuous load.
fn bench_wal_bounding(c: &mut Criterion) {
    let mut group = c.benchmark_group("wal_bounding");
    let ops_count = 5000u64;
    group.throughput(Throughput::Elements(ops_count));
    group.sample_size(20);

    // Test different WAL size limits
    for wal_size_mb in [1, 4, 16].iter() {
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}MB", wal_size_mb)),
            wal_size_mb,
            |b, &wal_size_mb| {
                b.iter_custom(|iters| {
                    let mut total_duration = std::time::Duration::ZERO;
                    let mut max_wal_size_observed = 0u64;

                    for _ in 0..iters {
                        let dir = tempdir().expect("create temp dir");
                        let config = EpochConfig {
                            max_wal_size_bytes: wal_size_mb * 1024 * 1024,
                            max_ops_per_epoch: 500,
                            background_checkpoint: false,
                            ..Default::default()
                        };

                        let manager = CheckpointManager::new(dir.path(), config)
                            .expect("create manager");

                        let start = std::time::Instant::now();
                        for _ in 0..ops_count {
                            manager.record_operation(200); // Larger ops to test bounding
                        }
                        total_duration += start.elapsed();

                        // Check WAL segment sizes
                        let segments = manager.find_wal_segments().unwrap_or_default();
                        let total_size: u64 = segments.iter()
                            .filter_map(|(_, path)| std::fs::metadata(path).ok())
                            .map(|m| m.len())
                            .sum();
                        max_wal_size_observed = max_wal_size_observed.max(total_size);

                        black_box(manager.stats());
                    }

                    // Print observed WAL size for verification
                    eprintln!("Max WAL size observed: {} KB", max_wal_size_observed / 1024);
                    total_duration
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// Recovery Time
// ============================================================================

/// Benchmark recovery time from epoch checkpoints.
fn bench_epoch_recovery(c: &mut Criterion) {
    let mut group = c.benchmark_group("epoch_recovery");
    group.sample_size(20);

    // Setup: Create manager with many operations
    for ops_count in [1000, 5000, 10000].iter() {
        group.throughput(Throughput::Elements(*ops_count as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(ops_count),
            ops_count,
            |b, &ops_count| {
                b.iter_custom(|iters| {
                    let mut total_duration = std::time::Duration::ZERO;

                    for _ in 0..iters {
                        let dir = tempdir().expect("create temp dir");
                        let config = EpochConfig {
                            max_ops_per_epoch: 500,
                            background_checkpoint: false,
                            retention_epochs: 3,
                            ..Default::default()
                        };

                        // Phase 1: Populate with operations
                        {
                            let manager = CheckpointManager::new(dir.path(), config.clone())
                                .expect("create manager");
                            for _ in 0..ops_count {
                                manager.record_operation(100);
                            }
                            manager.force_checkpoint().expect("checkpoint");
                        }

                        // Phase 2: Measure recovery time
                        let start = std::time::Instant::now();
                        let recovered_manager = CheckpointManager::new(dir.path(), config)
                            .expect("recover manager");
                        total_duration += start.elapsed();

                        black_box(recovered_manager.stats());
                    }

                    total_duration
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// Epoch Statistics
// ============================================================================

/// Benchmark epoch statistics accuracy (number of epochs created).
fn bench_epoch_statistics(c: &mut Criterion) {
    let mut group = c.benchmark_group("epoch_statistics");
    group.sample_size(30);

    let ops_count = 2000u64;
    group.throughput(Throughput::Elements(ops_count));

    group.bench_function("statistics_accuracy", |b| {
        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let dir = tempdir().expect("create temp dir");
                let config = EpochConfig {
                    max_ops_per_epoch: 200,
                    background_checkpoint: false,
                    ..Default::default()
                };

                let manager = CheckpointManager::new(dir.path(), config)
                    .expect("create manager");

                let start = std::time::Instant::now();
                for _ in 0..ops_count {
                    manager.record_operation(100);
                }
                let stats = manager.stats();
                total_duration += start.elapsed();

                // Verify epoch count is reasonable
                let expected_epochs = (ops_count / 200) + 1;
                assert!(
                    stats.total_epochs >= expected_epochs - 1 && stats.total_epochs <= expected_epochs + 1,
                    "Expected ~{} epochs, got {}",
                    expected_epochs,
                    stats.total_epochs
                );

                black_box(stats);
            }

            total_duration
        });
    });

    group.finish();
}

// ============================================================================
// Comparison: Epoch vs No-Epoch
// ============================================================================

/// Compare epoch-managed writes vs direct WAL writes.
fn bench_epoch_vs_direct(c: &mut Criterion) {
    let mut group = c.benchmark_group("epoch_vs_direct");
    group.throughput(Throughput::Elements(OPS_PER_ITER));
    group.sample_size(30);

    let terms = generate_terms(OPS_PER_ITER as usize);

    // Direct WAL writes (baseline)
    group.bench_function("direct_wal", |b| {
        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let dir = tempdir().expect("create temp dir");
                let wal_path = dir.path().join("bench.wal");
                let wal = WalWriter::create(&wal_path).expect("create WAL");

                let start = std::time::Instant::now();
                for term in &terms {
                    let record = WalRecord::Insert {
                        term: term.as_bytes().to_vec(),
                        value: None,
                    };
                    let _ = wal.append(record).expect("append");
                }
                wal.sync().expect("sync");
                total_duration += start.elapsed();
            }

            total_duration
        });
    });

    // Epoch-managed (with checkpointing overhead)
    group.bench_function("epoch_managed", |b| {
        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let dir = tempdir().expect("create temp dir");
                let config = EpochConfig {
                    max_ops_per_epoch: 500,
                    background_checkpoint: false,
                    ..Default::default()
                };

                let manager = CheckpointManager::new(dir.path(), config)
                    .expect("create manager");

                let start = std::time::Instant::now();
                for _ in 0..OPS_PER_ITER {
                    manager.record_operation(100);
                }
                total_duration += start.elapsed();

                black_box(manager.stats());
            }

            total_duration
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_wal_baseline,
    bench_epoch_duration,
    bench_epoch_ops_limit,
    bench_wal_bounding,
    bench_epoch_recovery,
    bench_epoch_statistics,
    bench_epoch_vs_direct,
);

criterion_main!(benches);
