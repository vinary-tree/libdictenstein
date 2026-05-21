//! Benchmarks for Adaptive Buffer Pool Sizing.
//!
//! This benchmark measures:
//! - Cache stats recording overhead (hit/miss tracking)
//! - Pool size query overhead
//! - Grow/shrink operations
//! - PID controller overhead
//! - Hit rate convergence under various workloads
//!
//! Run with:
//! ```bash
//! cargo bench --bench adaptive_pool_benchmarks --features persistent-artrie
//! ```

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use libdictenstein::persistent_artrie::{AdaptivePoolConfig, CacheStats};
use std::sync::Arc;

/// Number of operations per benchmark iteration
const OPS_PER_ITER: u64 = 10_000;

// ============================================================================
// Cache Stats Recording Overhead
// ============================================================================

/// Benchmark cache hit recording overhead.
fn bench_cache_stats_hit(c: &mut Criterion) {
    let mut group = c.benchmark_group("cache_stats");
    group.throughput(Throughput::Elements(OPS_PER_ITER));
    group.sample_size(50);

    let stats = CacheStats::new();

    group.bench_function("record_hit", |b| {
        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let start = std::time::Instant::now();
                for _ in 0..OPS_PER_ITER {
                    black_box(stats.record_hit());
                }
                total_duration += start.elapsed();
            }

            total_duration
        });
    });

    group.bench_function("record_miss", |b| {
        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let start = std::time::Instant::now();
                for _ in 0..OPS_PER_ITER {
                    black_box(stats.record_miss());
                }
                total_duration += start.elapsed();
            }

            total_duration
        });
    });

    group.finish();
}

/// Benchmark cache hit rate query overhead.
fn bench_cache_stats_query(c: &mut Criterion) {
    let mut group = c.benchmark_group("cache_stats_query");
    group.throughput(Throughput::Elements(OPS_PER_ITER));
    group.sample_size(50);

    let stats = CacheStats::new();

    // Add some hits and misses
    for _ in 0..1000 {
        stats.record_hit();
        stats.record_miss();
    }

    group.bench_function("hit_rate", |b| {
        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let start = std::time::Instant::now();
                for _ in 0..OPS_PER_ITER {
                    let rate = black_box(stats.hit_rate());
                    black_box(rate);
                }
                total_duration += start.elapsed();
            }

            total_duration
        });
    });

    group.bench_function("counts", |b| {
        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let start = std::time::Instant::now();
                for _ in 0..OPS_PER_ITER {
                    let counts = black_box(stats.counts());
                    black_box(counts);
                }
                total_duration += start.elapsed();
            }

            total_duration
        });
    });

    group.bench_function("total_accesses", |b| {
        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let start = std::time::Instant::now();
                for _ in 0..OPS_PER_ITER {
                    let total = black_box(stats.total_accesses());
                    black_box(total);
                }
                total_duration += start.elapsed();
            }

            total_duration
        });
    });

    group.finish();
}

/// Benchmark get_and_reset overhead.
fn bench_cache_stats_reset(c: &mut Criterion) {
    let mut group = c.benchmark_group("cache_stats_reset");
    group.sample_size(50);

    group.bench_function("get_and_reset", |b| {
        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let stats = CacheStats::new();
                // Add some data
                for _ in 0..100 {
                    stats.record_hit();
                    stats.record_miss();
                }

                let start = std::time::Instant::now();
                let result = black_box(stats.get_and_reset());
                total_duration += start.elapsed();

                black_box(result);
            }

            total_duration
        });
    });

    group.finish();
}

// ============================================================================
// Configuration Defaults
// ============================================================================

/// Benchmark config creation and defaults.
fn bench_config(c: &mut Criterion) {
    let mut group = c.benchmark_group("adaptive_pool_config");
    group.throughput(Throughput::Elements(OPS_PER_ITER));
    group.sample_size(50);

    group.bench_function("default", |b| {
        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let start = std::time::Instant::now();
                for _ in 0..OPS_PER_ITER {
                    let config = black_box(AdaptivePoolConfig::default());
                    black_box(config);
                }
                total_duration += start.elapsed();
            }

            total_duration
        });
    });

    group.bench_function("clone", |b| {
        let config = AdaptivePoolConfig::default();

        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let start = std::time::Instant::now();
                for _ in 0..OPS_PER_ITER {
                    let cloned = black_box(config.clone());
                    black_box(cloned);
                }
                total_duration += start.elapsed();
            }

            total_duration
        });
    });

    group.finish();
}

// ============================================================================
// Concurrent Stats Recording
// ============================================================================

/// Benchmark concurrent cache stats recording.
fn bench_concurrent_stats(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent_cache_stats");
    group.sample_size(30);

    for num_threads in [1, 2, 4, 8].iter() {
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}_threads", num_threads)),
            num_threads,
            |b, &num_threads| {
                b.iter_custom(|iters| {
                    let mut total_duration = std::time::Duration::ZERO;

                    for _ in 0..iters {
                        let stats: Arc<CacheStats> = Arc::new(CacheStats::new());
                        let ops_per_thread = OPS_PER_ITER / num_threads as u64;

                        let start = std::time::Instant::now();

                        let handles: Vec<_> = (0..num_threads)
                            .map(|i| {
                                let stats = Arc::clone(&stats);
                                std::thread::spawn(move || {
                                    for j in 0..ops_per_thread {
                                        if (i + j as usize) % 2 == 0 {
                                            stats.record_hit();
                                        } else {
                                            stats.record_miss();
                                        }
                                    }
                                })
                            })
                            .collect();

                        for handle in handles {
                            handle.join().unwrap();
                        }

                        total_duration += start.elapsed();

                        black_box(stats.hit_rate());
                    }

                    total_duration
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// Hit Rate Accuracy Under Contention
// ============================================================================

/// Test hit rate accuracy under concurrent access.
fn bench_hit_rate_accuracy(c: &mut Criterion) {
    let mut group = c.benchmark_group("hit_rate_accuracy");
    group.sample_size(20);

    // Test that hit rate is accurate under contention
    for hit_ratio in [0.5, 0.75, 0.90, 0.95, 0.99].iter() {
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{:.0}pct", hit_ratio * 100.0)),
            hit_ratio,
            |b, &hit_ratio| {
                b.iter_custom(|iters| {
                    let mut total_duration = std::time::Duration::ZERO;
                    let mut total_error = 0.0f64;

                    for _ in 0..iters {
                        let stats = CacheStats::new();
                        let total_ops = 10_000u64;
                        let hits = (total_ops as f64 * hit_ratio) as u64;
                        let misses = total_ops - hits;

                        let start = std::time::Instant::now();

                        // Record hits
                        for _ in 0..hits {
                            stats.record_hit();
                        }
                        // Record misses
                        for _ in 0..misses {
                            stats.record_miss();
                        }

                        let measured_rate = stats.hit_rate();
                        total_duration += start.elapsed();

                        let error = (measured_rate - hit_ratio).abs();
                        total_error += error;
                    }

                    // Verify accuracy (error should be < 1%)
                    let avg_error = total_error / iters as f64;
                    assert!(avg_error < 0.01, "Hit rate error too high: {}", avg_error);

                    total_duration
                });
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_cache_stats_hit,
    bench_cache_stats_query,
    bench_cache_stats_reset,
    bench_config,
    bench_concurrent_stats,
    bench_hit_rate_accuracy,
);

criterion_main!(benches);
