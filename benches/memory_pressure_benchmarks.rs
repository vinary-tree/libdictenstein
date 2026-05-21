//! Benchmarks for Memory Pressure Monitoring.
//!
//! This benchmark measures:
//! - Overhead of synchronous pressure check (check_now)
//! - Overhead of cached level query (current_level)
//! - Background monitor polling impact
//! - Monitor lifecycle overhead
//!
//! Run with:
//! ```bash
//! cargo bench --bench memory_pressure_benchmarks --features persistent-artrie
//! ```

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use libdictenstein::persistent_artrie::{
    MemoryPressureConfig, MemoryPressureLevel, MemoryPressureMonitor, MemoryStats,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Number of operations per benchmark iteration
const OPS_PER_ITER: u64 = 1000;

/// Create a no-op callback for benchmarks.
fn noop_callback() -> impl Fn(MemoryPressureLevel, &MemoryStats) + Send + Sync + 'static {
    |_, _| {}
}

// ============================================================================
// Synchronous Pressure Check (check_now - reads /proc/meminfo)
// ============================================================================

/// Benchmark synchronous pressure check overhead.
fn bench_check_now(c: &mut Criterion) {
    let mut group = c.benchmark_group("check_now");
    group.throughput(Throughput::Elements(OPS_PER_ITER));
    group.sample_size(50);

    // Create a disabled monitor (no background thread)
    let config = MemoryPressureConfig {
        enabled: false,
        ..Default::default()
    };
    let monitor = MemoryPressureMonitor::start(config, noop_callback()).expect("create monitor");

    group.bench_function("check_now", |b| {
        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let start = std::time::Instant::now();
                for _ in 0..OPS_PER_ITER {
                    let level = black_box(monitor.check_now());
                    black_box(level);
                }
                total_duration += start.elapsed();
            }

            total_duration
        });
    });

    group.finish();
}

// ============================================================================
// Current Level Query (cached atomic read)
// ============================================================================

/// Benchmark querying current pressure level (cached atomic read).
fn bench_current_level(c: &mut Criterion) {
    let mut group = c.benchmark_group("current_level");
    group.throughput(Throughput::Elements(OPS_PER_ITER));
    group.sample_size(50);

    // Create monitor with background thread
    let config = MemoryPressureConfig {
        enabled: true,
        poll_interval: Duration::from_secs(60), // Long interval to avoid interference
        ..Default::default()
    };
    let monitor = MemoryPressureMonitor::start(config, noop_callback()).expect("create monitor");

    group.bench_function("cached_level", |b| {
        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let start = std::time::Instant::now();
                for _ in 0..OPS_PER_ITER {
                    let level = black_box(monitor.current_level());
                    black_box(level);
                }
                total_duration += start.elapsed();
            }

            total_duration
        });
    });

    group.finish();
}

// ============================================================================
// Memory Stats Query
// ============================================================================

/// Benchmark querying current memory statistics.
fn bench_current_stats(c: &mut Criterion) {
    let mut group = c.benchmark_group("current_stats");
    group.throughput(Throughput::Elements(OPS_PER_ITER));
    group.sample_size(50);

    let config = MemoryPressureConfig {
        enabled: true,
        poll_interval: Duration::from_secs(60),
        ..Default::default()
    };
    let monitor = MemoryPressureMonitor::start(config, noop_callback()).expect("create monitor");

    group.bench_function("current_stats", |b| {
        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let start = std::time::Instant::now();
                for _ in 0..OPS_PER_ITER {
                    let stats = black_box(monitor.current_stats());
                    black_box(stats);
                }
                total_duration += start.elapsed();
            }

            total_duration
        });
    });

    group.finish();
}

// ============================================================================
// Monitor Start/Stop Overhead
// ============================================================================

/// Benchmark monitor creation and destruction overhead.
fn bench_monitor_lifecycle(c: &mut Criterion) {
    let mut group = c.benchmark_group("monitor_lifecycle");
    group.sample_size(30);

    // Enabled monitor (with background thread)
    group.bench_function("start_enabled", |b| {
        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let config = MemoryPressureConfig {
                    enabled: true,
                    poll_interval: Duration::from_secs(60),
                    ..Default::default()
                };

                let start = std::time::Instant::now();
                let monitor =
                    MemoryPressureMonitor::start(config, noop_callback()).expect("create monitor");
                total_duration += start.elapsed();

                drop(monitor);
            }

            total_duration
        });
    });

    // Disabled monitor (no background thread)
    group.bench_function("start_disabled", |b| {
        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let config = MemoryPressureConfig {
                    enabled: false,
                    ..Default::default()
                };

                let start = std::time::Instant::now();
                let monitor =
                    MemoryPressureMonitor::start(config, noop_callback()).expect("create monitor");
                total_duration += start.elapsed();

                drop(monitor);
            }

            total_duration
        });
    });

    // Stop (enabled monitor, includes thread join)
    group.bench_function("stop_enabled", |b| {
        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let config = MemoryPressureConfig {
                    enabled: true,
                    poll_interval: Duration::from_secs(60),
                    ..Default::default()
                };
                let monitor =
                    MemoryPressureMonitor::start(config, noop_callback()).expect("create monitor");

                let start = std::time::Instant::now();
                drop(monitor);
                total_duration += start.elapsed();
            }

            total_duration
        });
    });

    group.finish();
}

// ============================================================================
// Polling Interval Impact
// ============================================================================

/// Benchmark impact of different polling intervals on system overhead.
fn bench_polling_intervals(c: &mut Criterion) {
    let mut group = c.benchmark_group("polling_intervals");
    group.throughput(Throughput::Elements(OPS_PER_ITER));
    group.sample_size(30);

    // Test different polling intervals - measure cached level access
    for interval_ms in [100, 500, 1000, 5000].iter() {
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}ms", interval_ms)),
            interval_ms,
            |b, &interval_ms| {
                b.iter_custom(|iters| {
                    let mut total_duration = std::time::Duration::ZERO;

                    for _ in 0..iters {
                        let config = MemoryPressureConfig {
                            enabled: true,
                            poll_interval: Duration::from_millis(interval_ms as u64),
                            ..Default::default()
                        };
                        let monitor = MemoryPressureMonitor::start(config, noop_callback())
                            .expect("create monitor");

                        // Measure time for many cached level reads
                        let start = std::time::Instant::now();
                        for _ in 0..OPS_PER_ITER {
                            let level = black_box(monitor.current_level());
                            black_box(level);
                        }
                        total_duration += start.elapsed();

                        drop(monitor);
                    }

                    total_duration
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// Callback Invocation
// ============================================================================

/// Benchmark callback overhead under simulated pressure transitions.
fn bench_callback_invocation(c: &mut Criterion) {
    let mut group = c.benchmark_group("callback_invocation");
    group.sample_size(30);

    // Measure overhead of callback registration
    group.bench_function("with_callback", |b| {
        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let callback_count = Arc::new(AtomicUsize::new(0));
                let count_clone = Arc::clone(&callback_count);

                let config = MemoryPressureConfig {
                    enabled: false,
                    ..Default::default()
                };

                let start = std::time::Instant::now();
                let monitor = MemoryPressureMonitor::start(config, move |_, _| {
                    count_clone.fetch_add(1, Ordering::Relaxed);
                })
                .expect("create monitor");
                total_duration += start.elapsed();

                black_box(callback_count.load(Ordering::Relaxed));
                drop(monitor);
            }

            total_duration
        });
    });

    group.finish();
}

// ============================================================================
// Stats Query Overhead
// ============================================================================

/// Benchmark stats query overhead.
fn bench_stats_query(c: &mut Criterion) {
    let mut group = c.benchmark_group("stats_query");
    group.throughput(Throughput::Elements(OPS_PER_ITER));
    group.sample_size(50);

    let config = MemoryPressureConfig {
        enabled: true,
        poll_interval: Duration::from_secs(60),
        ..Default::default()
    };
    let monitor = MemoryPressureMonitor::start(config, noop_callback()).expect("create monitor");

    // Query monitor stats
    group.bench_function("monitor_stats", |b| {
        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let start = std::time::Instant::now();
                for _ in 0..OPS_PER_ITER {
                    let stats = black_box(monitor.stats());
                    black_box(stats);
                }
                total_duration += start.elapsed();
            }

            total_duration
        });
    });

    group.finish();
}

// ============================================================================
// Comparison: Enabled vs Disabled Monitor
// ============================================================================

/// Compare overhead of enabled vs disabled monitor for level queries.
fn bench_enabled_vs_disabled(c: &mut Criterion) {
    let mut group = c.benchmark_group("enabled_vs_disabled");
    group.throughput(Throughput::Elements(OPS_PER_ITER));
    group.sample_size(50);

    // Disabled monitor (no background thread)
    let disabled_config = MemoryPressureConfig {
        enabled: false,
        ..Default::default()
    };
    let disabled_monitor =
        MemoryPressureMonitor::start(disabled_config, noop_callback()).expect("create monitor");

    group.bench_function("disabled", |b| {
        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let start = std::time::Instant::now();
                for _ in 0..OPS_PER_ITER {
                    let level = black_box(disabled_monitor.current_level());
                    black_box(level);
                }
                total_duration += start.elapsed();
            }

            total_duration
        });
    });

    // Enabled monitor (with background thread)
    let enabled_config = MemoryPressureConfig {
        enabled: true,
        poll_interval: Duration::from_secs(1),
        ..Default::default()
    };
    let enabled_monitor =
        MemoryPressureMonitor::start(enabled_config, noop_callback()).expect("create monitor");

    group.bench_function("enabled", |b| {
        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let start = std::time::Instant::now();
                for _ in 0..OPS_PER_ITER {
                    let level = black_box(enabled_monitor.current_level());
                    black_box(level);
                }
                total_duration += start.elapsed();
            }

            total_duration
        });
    });

    group.finish();
}

// ============================================================================
// Memory Stats Operations
// ============================================================================

/// Benchmark memory stats helper methods.
fn bench_memory_stats_helpers(c: &mut Criterion) {
    let mut group = c.benchmark_group("memory_stats_helpers");
    group.throughput(Throughput::Elements(OPS_PER_ITER));
    group.sample_size(50);

    // Create sample memory stats
    let stats = MemoryStats {
        mem_total: 16 * 1024 * 1024 * 1024,    // 16 GB
        mem_available: 8 * 1024 * 1024 * 1024, // 8 GB (50% available)
        mem_free: 4 * 1024 * 1024 * 1024,
        mem_used: 8 * 1024 * 1024 * 1024,
        swap_total: 8 * 1024 * 1024 * 1024,
        swap_used: 0,
    };

    group.bench_function("available_fraction", |b| {
        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let start = std::time::Instant::now();
                for _ in 0..OPS_PER_ITER {
                    let frac = black_box(stats.available_fraction());
                    black_box(frac);
                }
                total_duration += start.elapsed();
            }

            total_duration
        });
    });

    group.bench_function("available_mb", |b| {
        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let start = std::time::Instant::now();
                for _ in 0..OPS_PER_ITER {
                    let mb = black_box(stats.available_mb());
                    black_box(mb);
                }
                total_duration += start.elapsed();
            }

            total_duration
        });
    });

    group.bench_function("is_swapping", |b| {
        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let start = std::time::Instant::now();
                for _ in 0..OPS_PER_ITER {
                    let swap = black_box(stats.is_swapping());
                    black_box(swap);
                }
                total_duration += start.elapsed();
            }

            total_duration
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_check_now,
    bench_current_level,
    bench_current_stats,
    bench_monitor_lifecycle,
    bench_polling_intervals,
    bench_callback_invocation,
    bench_stats_query,
    bench_enabled_vs_disabled,
    bench_memory_stats_helpers,
);

criterion_main!(benches);
