//! I/O Backend Benchmarks - Phase 1 Baseline (mmap) + Phase 3 Comparison
//!
//! Measures per-operation latency distributions for the mmap backend using
//! HdrHistogram (nanosecond precision, 3 significant digits). When the
//! `io-uring-backend` feature is enabled, also benchmarks the io_uring backend.
//!
//! # Scenarios
//!
//! | Scenario              | Description                                          | Why It Matters                    |
//! |-----------------------|------------------------------------------------------|-----------------------------------|
//! | `seq_read_block`      | Allocate N blocks, read sequentially                 | Baseline throughput               |
//! | `rand_read_block`     | Same N blocks, pseudorandom order (fixed seed)       | Page fault impact                 |
//! | `seq_write_block`     | Sequential writes to N blocks                        | Write throughput                  |
//! | `rand_write_block`    | Pseudorandom writes                                  | Write scatter impact              |
//! | `sync_latency`        | Write 100 blocks, measure sync()                     | fsync cost                        |
//! | `memory_pressure_read`| 16-frame pool, N-block dataset, random reads          | Eviction-heavy, double-caching    |
//! | `mixed_pressure`      | 80% reads / 20% writes, 16-frame pool, N blocks      | Real-world pressure               |
//!
//! # Running
//!
//! ```bash
//! # mmap baseline only
//! taskset -c 0-3 cargo bench --bench io_backend_benchmarks --features persistent-artrie
//!
//! # With perf profiling
//! taskset -c 0-3 perf stat -e page-faults,minor-faults,major-faults,dTLB-load-misses,LLC-load-misses \
//!   cargo bench --bench io_backend_benchmarks --features persistent-artrie
//! ```

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use hdrhistogram::Histogram;
use std::time::{Duration, Instant};
use tempfile::tempdir;

use libdictenstein::persistent_artrie::block_storage::{AlignedBlock, BlockStorage};
use libdictenstein::persistent_artrie::buffer_manager::BufferManager;
use libdictenstein::persistent_artrie::disk_manager::MmapDiskManager;
#[cfg(feature = "io-uring-backend")]
use libdictenstein::persistent_artrie::disk_manager::BLOCK_SIZE;

/// Number of blocks for standard scenarios (256KB * 1024 = 256MB dataset).
const STANDARD_BLOCK_COUNT: u32 = 1024;

/// Number of blocks for memory pressure scenarios (256KB * 4096 = 1GB dataset).
const PRESSURE_BLOCK_COUNT: u32 = 4096;

/// Small buffer pool for memory pressure testing (16 frames = 4MB).
const PRESSURE_POOL_SIZE: usize = 16;

/// Number of sync operations to average.
const SYNC_OPS: usize = 100;

/// Fixed seed for deterministic pseudorandom order.
const RANDOM_SEED: u64 = 0xDEAD_BEEF_CAFE_BABE;

/// Generate a deterministic pseudorandom permutation of block IDs.
fn shuffled_block_ids(count: u32, seed: u64) -> Vec<u32> {
    let mut ids: Vec<u32> = (1..=count).collect();
    // Simple xorshift-based Fisher-Yates shuffle with fixed seed
    let mut state = seed;
    let len = ids.len();
    for i in (1..len).rev() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let j = (state as usize) % (i + 1);
        ids.swap(i, j);
    }
    ids
}

/// Create a MmapDiskManager with pre-allocated blocks.
fn setup_mmap(block_count: u32) -> (tempfile::TempDir, MmapDiskManager) {
    let dir = tempdir().expect("create temp dir");
    let path = dir.path().join("bench_mmap.part");
    let dm = MmapDiskManager::create(&path).expect("create mmap disk manager");

    // Pre-allocate blocks
    for _ in 0..block_count {
        dm.allocate_block().expect("allocate block");
    }
    dm.sync().expect("sync after allocation");

    (dir, dm)
}

#[cfg(feature = "io-uring-backend")]
fn setup_io_uring(
    block_count: u32,
) -> (
    tempfile::TempDir,
    libdictenstein::persistent_artrie::IoUringDiskManager,
) {
    use libdictenstein::persistent_artrie::IoUringDiskManager;

    let dir = tempdir().expect("create temp dir");
    let path = dir.path().join("bench_uring.part");
    let dm = IoUringDiskManager::create(&path).expect("create io_uring disk manager");

    // Pre-allocate blocks
    for _ in 0..block_count {
        dm.allocate_block().expect("allocate block");
    }
    dm.sync().expect("sync after allocation");

    (dir, dm)
}

/// Report histogram statistics in a structured format.
fn report_histogram(name: &str, backend: &str, hist: &Histogram<u64>) {
    let p50 = hist.value_at_quantile(0.50);
    let p99 = hist.value_at_quantile(0.99);
    let p999 = hist.value_at_quantile(0.999);
    let max = hist.max();
    let ops = hist.len();
    let mean = hist.mean();

    eprintln!(
        "  {:>25} [{:>8}]  p50={:>8}ns  p99={:>8}ns  p999={:>9}ns  max={:>9}ns  mean={:>8.0}ns  ops={}",
        name, backend, p50, p99, p999, max, mean, ops
    );
}

/// Run a block-level benchmark, returning the histogram.
fn bench_block_ops<S: BlockStorage>(storage: &S, block_ids: &[u32], read: bool) -> Histogram<u64> {
    let mut hist =
        Histogram::<u64>::new_with_bounds(1, 60_000_000_000, 3).expect("create histogram");
    let mut block = AlignedBlock::new_boxed();

    // Fill with pattern data for writes
    if !read {
        for (i, byte) in block.data.iter_mut().enumerate() {
            *byte = (i % 256) as u8;
        }
    }

    for &block_id in block_ids {
        let start = Instant::now();
        if read {
            storage
                .read_block(block_id, &mut block.data)
                .expect("read block");
        } else {
            storage
                .write_block(block_id, &block.data)
                .expect("write block");
        }
        let elapsed_ns = start.elapsed().as_nanos() as u64;
        hist.record(elapsed_ns).ok();
    }

    hist
}

// =============================================================================
// Benchmark: Sequential Read
// =============================================================================

fn bench_seq_read_block(c: &mut Criterion) {
    let mut group = c.benchmark_group("seq_read_block");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(10));
    group.throughput(Throughput::Elements(STANDARD_BLOCK_COUNT as u64));

    let block_ids: Vec<u32> = (1..=STANDARD_BLOCK_COUNT).collect();

    group.bench_function("mmap", |b| {
        let (_dir, dm) = setup_mmap(STANDARD_BLOCK_COUNT);
        // Warm up: write data to all blocks first
        let block = AlignedBlock::new_boxed();
        for &id in &block_ids {
            dm.write_block(id, &block.data).expect("write");
        }
        dm.sync().expect("sync");

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let hist = bench_block_ops(&dm, &block_ids, true);
                total += Duration::from_nanos(hist.mean() as u64 * STANDARD_BLOCK_COUNT as u64);
            }
            total
        });
    });

    #[cfg(feature = "io-uring-backend")]
    group.bench_function("io_uring", |b| {
        let (_dir, dm) = setup_io_uring(STANDARD_BLOCK_COUNT);
        let block = AlignedBlock::new_boxed();
        for &id in &block_ids {
            dm.write_block(id, &block.data).expect("write");
        }
        dm.sync().expect("sync");

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let hist = bench_block_ops(&dm, &block_ids, true);
                total += Duration::from_nanos(hist.mean() as u64 * STANDARD_BLOCK_COUNT as u64);
            }
            total
        });
    });

    group.finish();
}

// =============================================================================
// Benchmark: Random Read
// =============================================================================

fn bench_rand_read_block(c: &mut Criterion) {
    let mut group = c.benchmark_group("rand_read_block");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(10));
    group.throughput(Throughput::Elements(STANDARD_BLOCK_COUNT as u64));

    let block_ids = shuffled_block_ids(STANDARD_BLOCK_COUNT, RANDOM_SEED);

    group.bench_function("mmap", |b| {
        let (_dir, dm) = setup_mmap(STANDARD_BLOCK_COUNT);
        let block = AlignedBlock::new_boxed();
        for id in 1..=STANDARD_BLOCK_COUNT {
            dm.write_block(id, &block.data).expect("write");
        }
        dm.sync().expect("sync");

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let hist = bench_block_ops(&dm, &block_ids, true);
                total += Duration::from_nanos(hist.mean() as u64 * STANDARD_BLOCK_COUNT as u64);
            }
            total
        });
    });

    #[cfg(feature = "io-uring-backend")]
    group.bench_function("io_uring", |b| {
        let (_dir, dm) = setup_io_uring(STANDARD_BLOCK_COUNT);
        let block = AlignedBlock::new_boxed();
        for id in 1..=STANDARD_BLOCK_COUNT {
            dm.write_block(id, &block.data).expect("write");
        }
        dm.sync().expect("sync");

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let hist = bench_block_ops(&dm, &block_ids, true);
                total += Duration::from_nanos(hist.mean() as u64 * STANDARD_BLOCK_COUNT as u64);
            }
            total
        });
    });

    group.finish();
}

// =============================================================================
// Benchmark: Sequential Write
// =============================================================================

fn bench_seq_write_block(c: &mut Criterion) {
    let mut group = c.benchmark_group("seq_write_block");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(10));
    group.throughput(Throughput::Elements(STANDARD_BLOCK_COUNT as u64));

    let block_ids: Vec<u32> = (1..=STANDARD_BLOCK_COUNT).collect();

    group.bench_function("mmap", |b| {
        let (_dir, dm) = setup_mmap(STANDARD_BLOCK_COUNT);

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let hist = bench_block_ops(&dm, &block_ids, false);
                total += Duration::from_nanos(hist.mean() as u64 * STANDARD_BLOCK_COUNT as u64);
            }
            total
        });
    });

    #[cfg(feature = "io-uring-backend")]
    group.bench_function("io_uring", |b| {
        let (_dir, dm) = setup_io_uring(STANDARD_BLOCK_COUNT);

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let hist = bench_block_ops(&dm, &block_ids, false);
                total += Duration::from_nanos(hist.mean() as u64 * STANDARD_BLOCK_COUNT as u64);
            }
            total
        });
    });

    group.finish();
}

// =============================================================================
// Benchmark: Random Write
// =============================================================================

fn bench_rand_write_block(c: &mut Criterion) {
    let mut group = c.benchmark_group("rand_write_block");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(10));
    group.throughput(Throughput::Elements(STANDARD_BLOCK_COUNT as u64));

    let block_ids = shuffled_block_ids(STANDARD_BLOCK_COUNT, RANDOM_SEED);

    group.bench_function("mmap", |b| {
        let (_dir, dm) = setup_mmap(STANDARD_BLOCK_COUNT);

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let hist = bench_block_ops(&dm, &block_ids, false);
                total += Duration::from_nanos(hist.mean() as u64 * STANDARD_BLOCK_COUNT as u64);
            }
            total
        });
    });

    #[cfg(feature = "io-uring-backend")]
    group.bench_function("io_uring", |b| {
        let (_dir, dm) = setup_io_uring(STANDARD_BLOCK_COUNT);

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let hist = bench_block_ops(&dm, &block_ids, false);
                total += Duration::from_nanos(hist.mean() as u64 * STANDARD_BLOCK_COUNT as u64);
            }
            total
        });
    });

    group.finish();
}

// =============================================================================
// Benchmark: Sync Latency
// =============================================================================

fn bench_sync_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("sync_latency");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(10));
    group.throughput(Throughput::Elements(SYNC_OPS as u64));

    group.bench_function("mmap", |b| {
        let (_dir, dm) = setup_mmap(SYNC_OPS as u32);
        let block = AlignedBlock::new_boxed();

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let mut hist = Histogram::<u64>::new_with_bounds(1, 60_000_000_000, 3)
                    .expect("create histogram");

                for id in 1..=(SYNC_OPS as u32) {
                    dm.write_block(id, &block.data).expect("write");
                    let start = Instant::now();
                    dm.sync().expect("sync");
                    hist.record(start.elapsed().as_nanos() as u64).ok();
                }

                report_histogram("sync_latency", "mmap", &hist);
                total += Duration::from_nanos(hist.mean() as u64 * SYNC_OPS as u64);
            }
            total
        });
    });

    #[cfg(feature = "io-uring-backend")]
    group.bench_function("io_uring", |b| {
        let (_dir, dm) = setup_io_uring(SYNC_OPS as u32);
        let block = AlignedBlock::new_boxed();

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let mut hist = Histogram::<u64>::new_with_bounds(1, 60_000_000_000, 3)
                    .expect("create histogram");

                for id in 1..=(SYNC_OPS as u32) {
                    dm.write_block(id, &block.data).expect("write");
                    let start = Instant::now();
                    dm.sync().expect("sync");
                    hist.record(start.elapsed().as_nanos() as u64).ok();
                }

                report_histogram("sync_latency", "io_uring", &hist);
                total += Duration::from_nanos(hist.mean() as u64 * SYNC_OPS as u64);
            }
            total
        });
    });

    group.finish();
}

// =============================================================================
// Benchmark: Memory Pressure Read (via BufferManager)
// =============================================================================

fn bench_memory_pressure_read(c: &mut Criterion) {
    let mut group = c.benchmark_group("memory_pressure_read");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(15));

    let num_reads = 1000u64;
    group.throughput(Throughput::Elements(num_reads));

    let block_ids = shuffled_block_ids(PRESSURE_BLOCK_COUNT, RANDOM_SEED);
    // Only use first `num_reads` block IDs for each iteration
    let read_ids: Vec<u32> = block_ids.iter().copied().take(num_reads as usize).collect();

    group.bench_function("mmap", |b| {
        let (_dir, dm) = setup_mmap(PRESSURE_BLOCK_COUNT);
        // Write data to all blocks
        let block = AlignedBlock::new_boxed();
        for id in 1..=PRESSURE_BLOCK_COUNT {
            dm.write_block(id, &block.data).expect("write");
        }
        dm.sync().expect("sync");

        // Create BufferManager with tiny pool to force eviction
        let bm = BufferManager::new(dm, PRESSURE_POOL_SIZE);

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let mut hist = Histogram::<u64>::new_with_bounds(1, 60_000_000_000, 3)
                    .expect("create histogram");

                for &block_id in &read_ids {
                    let start = Instant::now();
                    let guard = bm.fetch_page(block_id).expect("fetch page");
                    black_box(guard.data());
                    drop(guard);
                    hist.record(start.elapsed().as_nanos() as u64).ok();
                }

                total += Duration::from_nanos(hist.mean() as u64 * num_reads);
            }
            total
        });
    });

    #[cfg(feature = "io-uring-backend")]
    group.bench_function("io_uring", |b| {
        let (_dir, dm) = setup_io_uring(PRESSURE_BLOCK_COUNT);
        let block = AlignedBlock::new_boxed();
        for id in 1..=PRESSURE_BLOCK_COUNT {
            dm.write_block(id, &block.data).expect("write");
        }
        dm.sync().expect("sync");

        let bm = BufferManager::new(dm, PRESSURE_POOL_SIZE);

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let mut hist = Histogram::<u64>::new_with_bounds(1, 60_000_000_000, 3)
                    .expect("create histogram");

                for &block_id in &read_ids {
                    let start = Instant::now();
                    let guard = bm.fetch_page(block_id).expect("fetch page");
                    black_box(guard.data());
                    drop(guard);
                    hist.record(start.elapsed().as_nanos() as u64).ok();
                }

                total += Duration::from_nanos(hist.mean() as u64 * num_reads);
            }
            total
        });
    });

    group.finish();
}

// =============================================================================
// Benchmark: Mixed Pressure (80% reads / 20% writes)
// =============================================================================

fn bench_mixed_pressure(c: &mut Criterion) {
    let mut group = c.benchmark_group("mixed_pressure");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(15));

    let num_ops = 1000u64;
    group.throughput(Throughput::Elements(num_ops));

    let block_ids = shuffled_block_ids(PRESSURE_BLOCK_COUNT, RANDOM_SEED);
    let op_ids: Vec<u32> = block_ids.iter().copied().take(num_ops as usize).collect();

    group.bench_function("mmap", |b| {
        let (_dir, dm) = setup_mmap(PRESSURE_BLOCK_COUNT);
        let block = AlignedBlock::new_boxed();
        for id in 1..=PRESSURE_BLOCK_COUNT {
            dm.write_block(id, &block.data).expect("write");
        }
        dm.sync().expect("sync");

        let bm = BufferManager::new(dm, PRESSURE_POOL_SIZE);

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let mut hist = Histogram::<u64>::new_with_bounds(1, 60_000_000_000, 3)
                    .expect("create histogram");

                for (i, &block_id) in op_ids.iter().enumerate() {
                    let is_write = (i % 5) == 0; // 20% writes
                    let start = Instant::now();
                    if is_write {
                        let mut guard = bm.fetch_page_mut(block_id).expect("fetch page mut");
                        guard.data_mut()[0] = (i & 0xFF) as u8;
                        drop(guard);
                    } else {
                        let guard = bm.fetch_page(block_id).expect("fetch page");
                        black_box(guard.data());
                        drop(guard);
                    }
                    hist.record(start.elapsed().as_nanos() as u64).ok();
                }

                total += Duration::from_nanos(hist.mean() as u64 * num_ops);
            }
            total
        });
    });

    #[cfg(feature = "io-uring-backend")]
    group.bench_function("io_uring", |b| {
        let (_dir, dm) = setup_io_uring(PRESSURE_BLOCK_COUNT);
        let block = AlignedBlock::new_boxed();
        for id in 1..=PRESSURE_BLOCK_COUNT {
            dm.write_block(id, &block.data).expect("write");
        }
        dm.sync().expect("sync");

        let bm = BufferManager::new(dm, PRESSURE_POOL_SIZE);

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let mut hist = Histogram::<u64>::new_with_bounds(1, 60_000_000_000, 3)
                    .expect("create histogram");

                for (i, &block_id) in op_ids.iter().enumerate() {
                    let is_write = (i % 5) == 0;
                    let start = Instant::now();
                    if is_write {
                        let mut guard = bm.fetch_page_mut(block_id).expect("fetch page mut");
                        guard.data_mut()[0] = (i & 0xFF) as u8;
                        drop(guard);
                    } else {
                        let guard = bm.fetch_page(block_id).expect("fetch page");
                        black_box(guard.data());
                        drop(guard);
                    }
                    hist.record(start.elapsed().as_nanos() as u64).ok();
                }

                total += Duration::from_nanos(hist.mean() as u64 * num_ops);
            }
            total
        });
    });

    group.finish();
}

// =============================================================================
// Benchmark: Batch I/O (io_uring advantage scenario)
// =============================================================================

fn bench_batch_read(c: &mut Criterion) {
    let mut group = c.benchmark_group("batch_read");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(10));

    let batch_size = 64u32;
    group.throughput(Throughput::Elements(batch_size as u64));

    group.bench_function("mmap", |b| {
        let (_dir, dm) = setup_mmap(batch_size);
        let block = AlignedBlock::new_boxed();
        for id in 1..=batch_size {
            dm.write_block(id, &block.data).expect("write");
        }
        dm.sync().expect("sync");

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                // Prepare batch request buffers
                let mut buffers: Vec<Box<AlignedBlock>> =
                    (0..batch_size).map(|_| AlignedBlock::new_boxed()).collect();

                let start = Instant::now();
                // Sequential read (mmap default batch impl)
                for (i, buf) in buffers.iter_mut().enumerate() {
                    dm.read_block((i + 1) as u32, &mut buf.data).expect("read");
                }
                total += start.elapsed();

                black_box(&buffers);
            }
            total
        });
    });

    #[cfg(feature = "io-uring-backend")]
    group.bench_function("io_uring", |b| {
        let (_dir, dm) = setup_io_uring(batch_size);
        let block = AlignedBlock::new_boxed();
        for id in 1..=batch_size {
            dm.write_block(id, &block.data).expect("write");
        }
        dm.sync().expect("sync");

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let mut buffers: Vec<Box<AlignedBlock>> =
                    (0..batch_size).map(|_| AlignedBlock::new_boxed()).collect();

                // Build batch request
                let mut requests: Vec<(u32, &mut [u8; BLOCK_SIZE])> = buffers
                    .iter_mut()
                    .enumerate()
                    .map(|(i, buf)| ((i + 1) as u32, &mut buf.data))
                    .collect();

                let start = Instant::now();
                dm.read_blocks_batch(&mut requests).expect("batch read");
                total += start.elapsed();

                black_box(&buffers);
            }
            total
        });
    });

    group.finish();
}

// =============================================================================
// Latency Distribution Report (standalone, not criterion)
// =============================================================================

/// Print a full latency distribution report for both backends.
/// Called from criterion's custom_bench or from a standalone scenario.
fn latency_report(c: &mut Criterion) {
    let mut group = c.benchmark_group("latency_report");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(5));

    // Single run to print histogram reports
    group.bench_function("print_distributions", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;

            for _ in 0..iters {
                eprintln!("\n=== Latency Distribution Report ===\n");

                // --- mmap ---
                let (_dir, dm) = setup_mmap(STANDARD_BLOCK_COUNT);
                let block = AlignedBlock::new_boxed();
                for id in 1..=STANDARD_BLOCK_COUNT {
                    dm.write_block(id, &block.data).expect("write");
                }
                dm.sync().expect("sync");

                let seq_ids: Vec<u32> = (1..=STANDARD_BLOCK_COUNT).collect();
                let rand_ids = shuffled_block_ids(STANDARD_BLOCK_COUNT, RANDOM_SEED);

                let start = Instant::now();
                let h = bench_block_ops(&dm, &seq_ids, true);
                report_histogram("seq_read_block", "mmap", &h);

                let h = bench_block_ops(&dm, &rand_ids, true);
                report_histogram("rand_read_block", "mmap", &h);

                let h = bench_block_ops(&dm, &seq_ids, false);
                report_histogram("seq_write_block", "mmap", &h);

                let h = bench_block_ops(&dm, &rand_ids, false);
                report_histogram("rand_write_block", "mmap", &h);

                // --- io_uring ---
                #[cfg(feature = "io-uring-backend")]
                {
                    let (_dir2, dm2) = setup_io_uring(STANDARD_BLOCK_COUNT);
                    let block2 = AlignedBlock::new_boxed();
                    for id in 1..=STANDARD_BLOCK_COUNT {
                        dm2.write_block(id, &block2.data).expect("write");
                    }
                    dm2.sync().expect("sync");

                    let h = bench_block_ops(&dm2, &seq_ids, true);
                    report_histogram("seq_read_block", "io_uring", &h);

                    let h = bench_block_ops(&dm2, &rand_ids, true);
                    report_histogram("rand_read_block", "io_uring", &h);

                    let h = bench_block_ops(&dm2, &seq_ids, false);
                    report_histogram("seq_write_block", "io_uring", &h);

                    let h = bench_block_ops(&dm2, &rand_ids, false);
                    report_histogram("rand_write_block", "io_uring", &h);
                }

                total += start.elapsed();
            }

            total
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_seq_read_block,
    bench_rand_read_block,
    bench_seq_write_block,
    bench_rand_write_block,
    bench_sync_latency,
    bench_memory_pressure_read,
    bench_mixed_pressure,
    bench_batch_read,
    latency_report,
);

criterion_main!(benches);
