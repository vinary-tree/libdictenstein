//! Benchmarks for Per-Node Logging.
//!
//! This benchmark measures:
//! - Log entry serialization/deserialization overhead
//! - Inline log append performance
//! - Dirty node tracker performance
//! - Recovery time simulation (per-node vs global WAL)
//!
//! Run with:
//! ```bash
//! cargo bench --bench per_node_log_benchmarks --features persistent-artrie
//! ```

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use libdictenstein::persistent_artrie::{
    DirtyNodeTracker, InlineLog, NodeLogEntry, PerNodeLogStatsAtomic,
};
use std::sync::Arc;

/// Number of operations per benchmark iteration
const OPS_PER_ITER: u64 = 10_000;

// ============================================================================
// Log Entry Serialization
// ============================================================================

/// Benchmark log entry serialization overhead.
fn bench_entry_serialization(c: &mut Criterion) {
    let mut group = c.benchmark_group("entry_serialization");
    group.throughput(Throughput::Elements(OPS_PER_ITER));
    group.sample_size(50);

    // InsertChild - 10 bytes
    group.bench_function("insert_child", |b| {
        let entry = NodeLogEntry::InsertChild {
            key: 0x42,
            child_id: 12345678901234567890,
        };

        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let start = std::time::Instant::now();
                for _ in 0..OPS_PER_ITER {
                    let serialized = black_box(entry.serialize());
                    black_box(serialized);
                }
                total_duration += start.elapsed();
            }

            total_duration
        });
    });

    // RemoveChild - 2 bytes
    group.bench_function("remove_child", |b| {
        let entry = NodeLogEntry::RemoveChild { key: 0xFF };

        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let start = std::time::Instant::now();
                for _ in 0..OPS_PER_ITER {
                    let serialized = black_box(entry.serialize());
                    black_box(serialized);
                }
                total_duration += start.elapsed();
            }

            total_duration
        });
    });

    // SetValue - variable (3 + len bytes)
    group.bench_function("set_value_small", |b| {
        let entry = NodeLogEntry::SetValue {
            value: vec![1, 2, 3, 4, 5, 6, 7, 8], // 8 bytes
        };

        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let start = std::time::Instant::now();
                for _ in 0..OPS_PER_ITER {
                    let serialized = black_box(entry.serialize());
                    black_box(serialized);
                }
                total_duration += start.elapsed();
            }

            total_duration
        });
    });

    // SetValue large - 256 bytes
    group.bench_function("set_value_large", |b| {
        let entry = NodeLogEntry::SetValue {
            value: vec![0xAB; 256],
        };

        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let start = std::time::Instant::now();
                for _ in 0..OPS_PER_ITER {
                    let serialized = black_box(entry.serialize());
                    black_box(serialized);
                }
                total_duration += start.elapsed();
            }

            total_duration
        });
    });

    // ClearValue - 1 byte
    group.bench_function("clear_value", |b| {
        let entry = NodeLogEntry::ClearValue;

        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let start = std::time::Instant::now();
                for _ in 0..OPS_PER_ITER {
                    let serialized = black_box(entry.serialize());
                    black_box(serialized);
                }
                total_duration += start.elapsed();
            }

            total_duration
        });
    });

    // SetPrefix - 2 + len bytes
    group.bench_function("set_prefix", |b| {
        let entry = NodeLogEntry::SetPrefix {
            prefix: b"hellowor".to_vec(), // 8 bytes
        };

        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let start = std::time::Instant::now();
                for _ in 0..OPS_PER_ITER {
                    let serialized = black_box(entry.serialize());
                    black_box(serialized);
                }
                total_duration += start.elapsed();
            }

            total_duration
        });
    });

    group.finish();
}

/// Benchmark log entry deserialization overhead.
fn bench_entry_deserialization(c: &mut Criterion) {
    let mut group = c.benchmark_group("entry_deserialization");
    group.throughput(Throughput::Elements(OPS_PER_ITER));
    group.sample_size(50);

    // InsertChild
    group.bench_function("insert_child", |b| {
        let entry = NodeLogEntry::InsertChild {
            key: 0x42,
            child_id: 12345678901234567890,
        };
        let serialized = entry.serialize();

        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let start = std::time::Instant::now();
                for _ in 0..OPS_PER_ITER {
                    let result = black_box(NodeLogEntry::deserialize(&serialized));
                    black_box(result);
                }
                total_duration += start.elapsed();
            }

            total_duration
        });
    });

    // RemoveChild
    group.bench_function("remove_child", |b| {
        let entry = NodeLogEntry::RemoveChild { key: 0xFF };
        let serialized = entry.serialize();

        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let start = std::time::Instant::now();
                for _ in 0..OPS_PER_ITER {
                    let result = black_box(NodeLogEntry::deserialize(&serialized));
                    black_box(result);
                }
                total_duration += start.elapsed();
            }

            total_duration
        });
    });

    // SetValue
    group.bench_function("set_value_small", |b| {
        let entry = NodeLogEntry::SetValue {
            value: vec![1, 2, 3, 4, 5, 6, 7, 8],
        };
        let serialized = entry.serialize();

        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let start = std::time::Instant::now();
                for _ in 0..OPS_PER_ITER {
                    let result = black_box(NodeLogEntry::deserialize(&serialized));
                    black_box(result);
                }
                total_duration += start.elapsed();
            }

            total_duration
        });
    });

    group.finish();
}

// ============================================================================
// Inline Log Operations
// ============================================================================

/// Benchmark inline log append performance.
fn bench_inline_log_append(c: &mut Criterion) {
    let mut group = c.benchmark_group("inline_log_append");
    group.sample_size(50);

    // Single append
    group.bench_function("single_entry", |b| {
        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let mut log = InlineLog::new(64);
                let entry = NodeLogEntry::InsertChild {
                    key: 0x42,
                    child_id: 100,
                };

                let start = std::time::Instant::now();
                let result = black_box(log.try_append(&entry));
                total_duration += start.elapsed();

                black_box(result);
            }

            total_duration
        });
    });

    // Fill log to capacity
    for capacity in [32, 64, 128, 256].iter() {
        group.bench_with_input(
            BenchmarkId::new("fill_capacity", capacity),
            capacity,
            |b, &capacity| {
                b.iter_custom(|iters| {
                    let mut total_duration = std::time::Duration::ZERO;

                    for _ in 0..iters {
                        let mut log = InlineLog::new(capacity);
                        let entry = NodeLogEntry::RemoveChild { key: 0x42 }; // 2 bytes each

                        let start = std::time::Instant::now();
                        while log.try_append(&entry) {
                            black_box(&log);
                        }
                        total_duration += start.elapsed();
                    }

                    total_duration
                });
            },
        );
    }

    group.finish();
}

/// Benchmark inline log iteration.
fn bench_inline_log_iteration(c: &mut Criterion) {
    let mut group = c.benchmark_group("inline_log_iteration");
    group.sample_size(50);

    for entry_count in [5, 10, 20, 30].iter() {
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}_entries", entry_count)),
            entry_count,
            |b, &entry_count| {
                // Prepare a log with entries
                let mut log = InlineLog::new(256);
                for i in 0..entry_count {
                    let entry = NodeLogEntry::RemoveChild { key: i as u8 };
                    log.try_append(&entry);
                }

                b.iter_custom(|iters| {
                    let mut total_duration = std::time::Duration::ZERO;

                    for _ in 0..iters {
                        let start = std::time::Instant::now();
                        for entry in log.iter() {
                            black_box(entry);
                        }
                        total_duration += start.elapsed();
                    }

                    total_duration
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// Dirty Node Tracker
// ============================================================================

/// Benchmark dirty node tracker operations.
fn bench_dirty_tracker(c: &mut Criterion) {
    let mut group = c.benchmark_group("dirty_tracker");
    group.throughput(Throughput::Elements(OPS_PER_ITER));
    group.sample_size(50);

    // Mark dirty
    group.bench_function("mark_dirty", |b| {
        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let stats = Arc::new(PerNodeLogStatsAtomic::new());
                let tracker = DirtyNodeTracker::new(Arc::clone(&stats));

                let start = std::time::Instant::now();
                for i in 0..OPS_PER_ITER {
                    tracker.mark_dirty(i);
                }
                total_duration += start.elapsed();
            }

            total_duration
        });
    });

    // Mark clean
    group.bench_function("mark_clean", |b| {
        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let stats = Arc::new(PerNodeLogStatsAtomic::new());
                let tracker = DirtyNodeTracker::new(Arc::clone(&stats));

                // Pre-populate
                for i in 0..OPS_PER_ITER {
                    tracker.mark_dirty(i);
                }

                let start = std::time::Instant::now();
                for i in 0..OPS_PER_ITER {
                    tracker.mark_clean(i);
                }
                total_duration += start.elapsed();
            }

            total_duration
        });
    });

    // Check is_dirty
    group.bench_function("is_dirty_check", |b| {
        let stats = Arc::new(PerNodeLogStatsAtomic::new());
        let tracker = DirtyNodeTracker::new(Arc::clone(&stats));

        // Pre-populate half
        for i in 0..OPS_PER_ITER / 2 {
            tracker.mark_dirty(i);
        }

        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let start = std::time::Instant::now();
                for i in 0..OPS_PER_ITER {
                    let result = black_box(tracker.is_dirty(i));
                    black_box(result);
                }
                total_duration += start.elapsed();
            }

            total_duration
        });
    });

    // Get dirty nodes
    group.bench_function("get_dirty_nodes", |b| {
        let stats = Arc::new(PerNodeLogStatsAtomic::new());
        let tracker = DirtyNodeTracker::new(Arc::clone(&stats));

        // Pre-populate
        for i in 0..1000 {
            tracker.mark_dirty(i);
        }

        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let start = std::time::Instant::now();
                let dirty = black_box(tracker.get_dirty_nodes());
                total_duration += start.elapsed();

                black_box(dirty);
            }

            total_duration
        });
    });

    group.finish();
}

// ============================================================================
// Recovery Simulation
// ============================================================================

/// Benchmark simulated recovery time comparison.
///
/// This simulates the difference between:
/// - Global WAL: Replay ALL operations
/// - Per-Node: Replay only dirty node logs
fn bench_recovery_simulation(c: &mut Criterion) {
    let mut group = c.benchmark_group("recovery_simulation");
    group.sample_size(30);

    // Simulate recovery with varying dirty node ratios
    for (total_ops, dirty_ratio) in [
        (10_000, 0.01),  // 1% dirty (100 nodes)
        (10_000, 0.05),  // 5% dirty (500 nodes)
        (10_000, 0.10),  // 10% dirty (1000 nodes)
        (100_000, 0.01), // 1% dirty (1000 nodes)
        (100_000, 0.05), // 5% dirty (5000 nodes)
    ]
    .iter()
    {
        let dirty_count = (*total_ops as f64 * dirty_ratio) as u64;
        let label = format!("{}_ops_{:.0}pct_dirty", total_ops, dirty_ratio * 100.0);

        // Simulate global WAL recovery (replay all)
        group.bench_with_input(
            BenchmarkId::new("global_wal", &label),
            &(total_ops, dirty_count),
            |b, &(total_ops, _)| {
                // Prepare entries to replay
                let entries: Vec<_> = (0..*total_ops)
                    .map(|i| NodeLogEntry::InsertChild {
                        key: (i % 256) as u8,
                        child_id: i,
                    })
                    .collect();

                b.iter_custom(|iters| {
                    let mut total_duration = std::time::Duration::ZERO;

                    for _ in 0..iters {
                        let start = std::time::Instant::now();

                        // Simulate global WAL: deserialize and process ALL entries
                        for entry in entries.iter() {
                            let serialized = entry.serialize();
                            let (deserialized, _) = NodeLogEntry::deserialize(&serialized).unwrap();
                            black_box(deserialized);
                        }

                        total_duration += start.elapsed();
                    }

                    total_duration
                });
            },
        );

        // Simulate per-node recovery (replay only dirty)
        group.bench_with_input(
            BenchmarkId::new("per_node", &label),
            &(total_ops, dirty_count),
            |b, &(_, dirty_count)| {
                // Prepare entries for dirty nodes only
                let entries: Vec<_> = (0..dirty_count)
                    .map(|i| NodeLogEntry::InsertChild {
                        key: (i % 256) as u8,
                        child_id: i,
                    })
                    .collect();

                b.iter_custom(|iters| {
                    let mut total_duration = std::time::Duration::ZERO;

                    for _ in 0..iters {
                        let start = std::time::Instant::now();

                        // Simulate per-node: only deserialize dirty node entries
                        for entry in entries.iter() {
                            let serialized = entry.serialize();
                            let (deserialized, _) = NodeLogEntry::deserialize(&serialized).unwrap();
                            black_box(deserialized);
                        }

                        total_duration += start.elapsed();
                    }

                    total_duration
                });
            },
        );
    }

    group.finish();
}

/// Benchmark serialized_size vs serialize().len() (zero-allocation size query).
fn bench_serialized_size(c: &mut Criterion) {
    let mut group = c.benchmark_group("serialized_size");
    group.throughput(Throughput::Elements(OPS_PER_ITER));
    group.sample_size(50);

    let entries = vec![
        NodeLogEntry::InsertChild {
            key: 0x42,
            child_id: 100,
        },
        NodeLogEntry::RemoveChild { key: 0xFF },
        NodeLogEntry::SetValue {
            value: vec![1, 2, 3, 4, 5],
        },
        NodeLogEntry::ClearValue,
        NodeLogEntry::SetPrefix {
            prefix: b"hello".to_vec(),
        },
    ];

    // Using serialized_size() - no allocation
    group.bench_function("serialized_size_method", |b| {
        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let start = std::time::Instant::now();
                for _ in 0..OPS_PER_ITER {
                    for entry in entries.iter() {
                        let size = black_box(entry.serialized_size());
                        black_box(size);
                    }
                }
                total_duration += start.elapsed();
            }

            total_duration
        });
    });

    // Using serialize().len() - allocates
    group.bench_function("serialize_then_len", |b| {
        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let start = std::time::Instant::now();
                for _ in 0..OPS_PER_ITER {
                    for entry in entries.iter() {
                        let size = black_box(entry.serialize().len());
                        black_box(size);
                    }
                }
                total_duration += start.elapsed();
            }

            total_duration
        });
    });

    group.finish();
}

// ============================================================================
// Stats Recording
// ============================================================================

/// Benchmark atomic stats recording overhead.
fn bench_stats_recording(c: &mut Criterion) {
    let mut group = c.benchmark_group("stats_recording");
    group.throughput(Throughput::Elements(OPS_PER_ITER));
    group.sample_size(50);

    let stats = PerNodeLogStatsAtomic::new();

    group.bench_function("record_entry_written_inline", |b| {
        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let start = std::time::Instant::now();
                for _ in 0..OPS_PER_ITER {
                    stats.record_entry_written(10, false);
                }
                total_duration += start.elapsed();
            }

            total_duration
        });
    });

    group.bench_function("record_entry_written_overflow", |b| {
        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let start = std::time::Instant::now();
                for _ in 0..OPS_PER_ITER {
                    stats.record_entry_written(100, true);
                }
                total_duration += start.elapsed();
            }

            total_duration
        });
    });

    group.bench_function("snapshot", |b| {
        b.iter_custom(|iters| {
            let mut total_duration = std::time::Duration::ZERO;

            for _ in 0..iters {
                let start = std::time::Instant::now();
                for _ in 0..OPS_PER_ITER {
                    let snapshot = black_box(stats.snapshot());
                    black_box(snapshot);
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
    bench_entry_serialization,
    bench_entry_deserialization,
    bench_inline_log_append,
    bench_inline_log_iteration,
    bench_dirty_tracker,
    bench_recovery_simulation,
    bench_serialized_size,
    bench_stats_recording,
);

criterion_main!(benches);
