//! Eviction-path Benchmarks for io_uring Pre-registered Buffers
//!
//! These benchmarks specifically measure I/O performance during buffer pool
//! eviction, where ReadFixed/WriteFixed are expected to show measurable
//! improvement by eliminating kernel-side buffer copies.
//!
//! # Strategy
//!
//! Create a `BufferManager` with a small pool (8 frames) and a dataset much
//! larger than the pool (128 blocks). Sequential access through all blocks
//! guarantees every access past the initial pool fill causes eviction.
//!
//! # Benchmark Groups
//!
//! 1. **eviction_read_only** — Clean eviction (1 I/O per eviction: read only)
//! 2. **eviction_dirty_writeback** — Dirty eviction (2 I/Os per eviction: write-back + read)
//! 3. **eviction_concurrent** — Multi-threaded eviction contention
//!
//! # Running
//!
//! ```bash
//! taskset -c 0-3 cargo bench --bench eviction_benchmarks --features bench-internals
//! ```

use criterion::{
    black_box, criterion_group, criterion_main, Criterion, Throughput,
};
use hdrhistogram::Histogram;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::tempdir;

use libdictenstein::persistent_artrie::block_storage::{AlignedBlock, BlockStorage};
use libdictenstein::persistent_artrie::buffer_manager::BufferManager;
use libdictenstein::persistent_artrie::disk_manager::{MmapDiskManager, BLOCK_SIZE};
use libdictenstein::persistent_artrie::IoUringDiskManager;

/// Small buffer pool to force eviction on every access past the pool size.
const EVICTION_POOL_SIZE: usize = 8;

/// Number of blocks — much larger than pool to ensure every sequential pass
/// causes eviction for every block after the first `EVICTION_POOL_SIZE`.
const EVICTION_BLOCK_COUNT: u32 = 128;

/// Number of threads for concurrent eviction benchmarks.
const EVICTION_THREAD_COUNT: usize = 4;

/// Fixed seed for deterministic pseudorandom order.
const RANDOM_SEED: u64 = 0xDEAD_BEEF_CAFE_BABE;

/// Generate a deterministic pseudorandom permutation of block IDs.
fn shuffled_block_ids(count: u32, seed: u64) -> Vec<u32> {
    let mut ids: Vec<u32> = (1..=count).collect();
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

fn report_histogram(name: &str, backend: &str, hist: &Histogram<u64>) {
    let p50 = hist.value_at_quantile(0.50);
    let p99 = hist.value_at_quantile(0.99);
    let p999 = hist.value_at_quantile(0.999);
    let max = hist.max();
    let mean = hist.mean();
    let ops = hist.len();
    let ops_per_sec = if mean > 0.0 {
        1_000_000_000.0 / mean
    } else {
        0.0
    };

    eprintln!(
        "  {:>35} [{:>12}]  p50={:>8}ns  p99={:>8}ns  p999={:>9}ns  max={:>9}ns  mean={:>8.0}ns  ops/s={:>10.0}  n={}",
        name, backend, p50, p99, p999, max, mean, ops_per_sec, ops
    );
}

// =============================================================================
// Setup helpers
// =============================================================================

/// Create an mmap disk manager with `block_count` pre-allocated, pre-written blocks.
fn setup_mmap_with_data(block_count: u32) -> (tempfile::TempDir, MmapDiskManager) {
    let dir = tempdir().expect("create temp dir");
    let path = dir.path().join("evict_mmap.part");
    let dm = MmapDiskManager::create(&path).expect("create mmap disk manager");

    let block = AlignedBlock::new_boxed();
    for i in 0..block_count {
        let id = dm.allocate_block().expect("allocate block");
        // Write a marker byte so reads are deterministic
        let mut write_buf = AlignedBlock::new_boxed();
        write_buf.data[0] = (i & 0xFF) as u8;
        dm.write_block(id, &write_buf.data).expect("write block");
    }
    dm.sync().expect("sync");
    (dir, dm)
}

/// Create an io_uring disk manager with `block_count` pre-allocated, pre-written blocks.
fn setup_io_uring_with_data(block_count: u32) -> (tempfile::TempDir, IoUringDiskManager) {
    let dir = tempdir().expect("create temp dir");
    let path = dir.path().join("evict_uring.part");
    let dm = IoUringDiskManager::create(&path).expect("create io_uring disk manager");

    for i in 0..block_count {
        let id = dm.allocate_block().expect("allocate block");
        let mut write_buf = AlignedBlock::new_boxed();
        write_buf.data[0] = (i & 0xFF) as u8;
        dm.write_block(id, &write_buf.data).expect("write block");
    }
    dm.sync().expect("sync");
    (dir, dm)
}

/// Create an io_uring disk manager with a configurable ring pool size.
fn setup_io_uring_with_ring_pool(
    block_count: u32,
    ring_count: usize,
) -> (tempfile::TempDir, IoUringDiskManager) {
    let dir = tempdir().expect("create temp dir");
    let path = dir.path().join("evict_uring_pool.part");
    let dm = IoUringDiskManager::create_with_ring_pool_size(&path, ring_count)
        .expect("create io_uring disk manager with ring pool");

    for i in 0..block_count {
        let id = dm.allocate_block().expect("allocate block");
        let mut write_buf = AlignedBlock::new_boxed();
        write_buf.data[0] = (i & 0xFF) as u8;
        dm.write_block(id, &write_buf.data).expect("write block");
    }
    dm.sync().expect("sync");
    (dir, dm)
}

// =============================================================================
// Group 1: Read-only eviction (1 I/O per eviction)
// =============================================================================

fn eviction_read_only(c: &mut Criterion) {
    let mut group = c.benchmark_group("eviction_read_only");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(10));

    let num_reads = EVICTION_BLOCK_COUNT as u64;
    group.throughput(Throughput::Elements(num_reads));

    let read_ids = shuffled_block_ids(EVICTION_BLOCK_COUNT, RANDOM_SEED);

    // --- mmap ---
    group.bench_function("mmap", |b| {
        let (_dir, dm) = setup_mmap_with_data(EVICTION_BLOCK_COUNT);
        let bm = BufferManager::new(dm, EVICTION_POOL_SIZE);

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let start = Instant::now();
                for &id in &read_ids {
                    let guard = bm.fetch_page(id).expect("fetch page");
                    black_box(guard.data());
                }
                total += start.elapsed();
            }
            total
        });
    });

    // --- io_uring with registration (ReadFixed on eviction path) ---
    group.bench_function("io_uring_fixed", |b| {
        let (_dir, dm) = setup_io_uring_with_data(EVICTION_BLOCK_COUNT);
        let bm = BufferManager::new(dm, EVICTION_POOL_SIZE);

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let start = Instant::now();
                for &id in &read_ids {
                    let guard = bm.fetch_page(id).expect("fetch page");
                    black_box(guard.data());
                }
                total += start.elapsed();
            }
            total
        });
    });

    // --- io_uring without registration (standard Read on eviction path) ---
    group.bench_function("io_uring_standard", |b| {
        let (_dir, dm) = setup_io_uring_with_data(EVICTION_BLOCK_COUNT);
        let bm = BufferManager::new_without_registration(dm, EVICTION_POOL_SIZE);

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let start = Instant::now();
                for &id in &read_ids {
                    let guard = bm.fetch_page(id).expect("fetch page");
                    black_box(guard.data());
                }
                total += start.elapsed();
            }
            total
        });
    });

    group.finish();

    // Print HdrHistogram summary for the read-only eviction path
    eprintln!("\n=== Eviction Read-Only Latency Distribution ===");

    // mmap
    {
        let (_dir, dm) = setup_mmap_with_data(EVICTION_BLOCK_COUNT);
        let bm = BufferManager::new(dm, EVICTION_POOL_SIZE);
        let mut hist = Histogram::<u64>::new_with_bounds(1, 100_000_000, 3)
            .expect("create histogram");
        for &id in &read_ids {
            let start = Instant::now();
            let guard = bm.fetch_page(id).expect("fetch page");
            black_box(guard.data());
            drop(guard);
            let _ = hist.record(start.elapsed().as_nanos() as u64);
        }
        report_histogram("eviction_read_only", "mmap", &hist);
    }

    // io_uring fixed
    {
        let (_dir, dm) = setup_io_uring_with_data(EVICTION_BLOCK_COUNT);
        let bm = BufferManager::new(dm, EVICTION_POOL_SIZE);
        let mut hist = Histogram::<u64>::new_with_bounds(1, 100_000_000, 3)
            .expect("create histogram");
        for &id in &read_ids {
            let start = Instant::now();
            let guard = bm.fetch_page(id).expect("fetch page");
            black_box(guard.data());
            drop(guard);
            let _ = hist.record(start.elapsed().as_nanos() as u64);
        }
        report_histogram("eviction_read_only", "io_uring_fixed", &hist);
    }

    // io_uring standard
    {
        let (_dir, dm) = setup_io_uring_with_data(EVICTION_BLOCK_COUNT);
        let bm = BufferManager::new_without_registration(dm, EVICTION_POOL_SIZE);
        let mut hist = Histogram::<u64>::new_with_bounds(1, 100_000_000, 3)
            .expect("create histogram");
        for &id in &read_ids {
            let start = Instant::now();
            let guard = bm.fetch_page(id).expect("fetch page");
            black_box(guard.data());
            drop(guard);
            let _ = hist.record(start.elapsed().as_nanos() as u64);
        }
        report_histogram("eviction_read_only", "io_uring_std", &hist);
    }
}

// =============================================================================
// Group 2: Dirty eviction (2 I/Os per eviction: write-back + read)
// =============================================================================

fn eviction_dirty_writeback(c: &mut Criterion) {
    let mut group = c.benchmark_group("eviction_dirty_writeback");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(15));

    let num_ops = EVICTION_BLOCK_COUNT as u64;
    group.throughput(Throughput::Elements(num_ops));

    let access_ids = shuffled_block_ids(EVICTION_BLOCK_COUNT, RANDOM_SEED);

    // --- mmap ---
    group.bench_function("mmap", |b| {
        let (_dir, dm) = setup_mmap_with_data(EVICTION_BLOCK_COUNT);
        let bm = BufferManager::new(dm, EVICTION_POOL_SIZE);

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let start = Instant::now();
                for &id in &access_ids {
                    // Fetch for write — marks page dirty on drop
                    let mut guard = bm.fetch_page_mut(id).expect("fetch page mut");
                    guard.data_mut()[0] = guard.data_mut()[0].wrapping_add(1);
                    drop(guard);
                    // Page is now dirty. Next eviction of this frame will write-back.
                }
                total += start.elapsed();
            }
            total
        });
    });

    // --- io_uring with registration (WriteFixed + ReadFixed on eviction path) ---
    group.bench_function("io_uring_fixed", |b| {
        let (_dir, dm) = setup_io_uring_with_data(EVICTION_BLOCK_COUNT);
        let bm = BufferManager::new(dm, EVICTION_POOL_SIZE);

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let start = Instant::now();
                for &id in &access_ids {
                    let mut guard = bm.fetch_page_mut(id).expect("fetch page mut");
                    guard.data_mut()[0] = guard.data_mut()[0].wrapping_add(1);
                    drop(guard);
                }
                total += start.elapsed();
            }
            total
        });
    });

    // --- io_uring without registration (standard Write + Read on eviction path) ---
    group.bench_function("io_uring_standard", |b| {
        let (_dir, dm) = setup_io_uring_with_data(EVICTION_BLOCK_COUNT);
        let bm = BufferManager::new_without_registration(dm, EVICTION_POOL_SIZE);

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let start = Instant::now();
                for &id in &access_ids {
                    let mut guard = bm.fetch_page_mut(id).expect("fetch page mut");
                    guard.data_mut()[0] = guard.data_mut()[0].wrapping_add(1);
                    drop(guard);
                }
                total += start.elapsed();
            }
            total
        });
    });

    group.finish();

    // Print HdrHistogram summary
    eprintln!("\n=== Eviction Dirty Write-back Latency Distribution ===");

    // mmap
    {
        let (_dir, dm) = setup_mmap_with_data(EVICTION_BLOCK_COUNT);
        let bm = BufferManager::new(dm, EVICTION_POOL_SIZE);
        let mut hist = Histogram::<u64>::new_with_bounds(1, 100_000_000, 3)
            .expect("create histogram");
        for &id in &access_ids {
            let start = Instant::now();
            let mut guard = bm.fetch_page_mut(id).expect("fetch page mut");
            guard.data_mut()[0] = guard.data_mut()[0].wrapping_add(1);
            drop(guard);
            let _ = hist.record(start.elapsed().as_nanos() as u64);
        }
        report_histogram("eviction_dirty_writeback", "mmap", &hist);
    }

    // io_uring fixed
    {
        let (_dir, dm) = setup_io_uring_with_data(EVICTION_BLOCK_COUNT);
        let bm = BufferManager::new(dm, EVICTION_POOL_SIZE);
        let mut hist = Histogram::<u64>::new_with_bounds(1, 100_000_000, 3)
            .expect("create histogram");
        for &id in &access_ids {
            let start = Instant::now();
            let mut guard = bm.fetch_page_mut(id).expect("fetch page mut");
            guard.data_mut()[0] = guard.data_mut()[0].wrapping_add(1);
            drop(guard);
            let _ = hist.record(start.elapsed().as_nanos() as u64);
        }
        report_histogram("eviction_dirty_writeback", "io_uring_fixed", &hist);
    }

    // io_uring standard
    {
        let (_dir, dm) = setup_io_uring_with_data(EVICTION_BLOCK_COUNT);
        let bm = BufferManager::new_without_registration(dm, EVICTION_POOL_SIZE);
        let mut hist = Histogram::<u64>::new_with_bounds(1, 100_000_000, 3)
            .expect("create histogram");
        for &id in &access_ids {
            let start = Instant::now();
            let mut guard = bm.fetch_page_mut(id).expect("fetch page mut");
            guard.data_mut()[0] = guard.data_mut()[0].wrapping_add(1);
            drop(guard);
            let _ = hist.record(start.elapsed().as_nanos() as u64);
        }
        report_histogram("eviction_dirty_writeback", "io_uring_std", &hist);
    }
}

// =============================================================================
// Group 3: Multi-threaded eviction contention
// =============================================================================

fn eviction_concurrent(c: &mut Criterion) {
    let mut group = c.benchmark_group("eviction_concurrent");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(15));

    let num_ops = EVICTION_BLOCK_COUNT as u64 * EVICTION_THREAD_COUNT as u64;
    group.throughput(Throughput::Elements(num_ops));

    // --- mmap, 4 threads ---
    group.bench_function("mmap_4t", |b| {
        let (_dir, dm) = setup_mmap_with_data(EVICTION_BLOCK_COUNT);
        let bm = Arc::new(BufferManager::new(dm, EVICTION_POOL_SIZE));

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let start = Instant::now();
                let handles: Vec<_> = (0..EVICTION_THREAD_COUNT)
                    .map(|t| {
                        let bm = Arc::clone(&bm);
                        std::thread::spawn(move || {
                            // Each thread reads all blocks in a different shuffled order
                            let ids = shuffled_block_ids(
                                EVICTION_BLOCK_COUNT,
                                RANDOM_SEED.wrapping_add(t as u64),
                            );
                            for &id in &ids {
                                let guard = bm.fetch_page(id).expect("fetch page");
                                black_box(guard.data());
                            }
                        })
                    })
                    .collect();
                for h in handles {
                    h.join().expect("thread panicked");
                }
                total += start.elapsed();
            }
            total
        });
    });

    // --- io_uring, 4 threads, per-thread rings ---
    group.bench_function("io_uring_4t", |b| {
        let (_dir, dm) = setup_io_uring_with_ring_pool(
            EVICTION_BLOCK_COUNT,
            EVICTION_THREAD_COUNT,
        );
        let bm = Arc::new(BufferManager::new(dm, EVICTION_POOL_SIZE));

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let start = Instant::now();
                let handles: Vec<_> = (0..EVICTION_THREAD_COUNT)
                    .map(|t| {
                        let bm = Arc::clone(&bm);
                        std::thread::spawn(move || {
                            let ids = shuffled_block_ids(
                                EVICTION_BLOCK_COUNT,
                                RANDOM_SEED.wrapping_add(t as u64),
                            );
                            for &id in &ids {
                                let guard = bm.fetch_page(id).expect("fetch page");
                                black_box(guard.data());
                            }
                        })
                    })
                    .collect();
                for h in handles {
                    h.join().expect("thread panicked");
                }
                total += start.elapsed();
            }
            total
        });
    });

    // --- io_uring, 4 threads, single ring (contention baseline) ---
    group.bench_function("io_uring_single_ring_4t", |b| {
        let (_dir, dm) = setup_io_uring_with_ring_pool(
            EVICTION_BLOCK_COUNT,
            1, // single ring — contention baseline
        );
        let bm = Arc::new(BufferManager::new(dm, EVICTION_POOL_SIZE));

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let start = Instant::now();
                let handles: Vec<_> = (0..EVICTION_THREAD_COUNT)
                    .map(|t| {
                        let bm = Arc::clone(&bm);
                        std::thread::spawn(move || {
                            let ids = shuffled_block_ids(
                                EVICTION_BLOCK_COUNT,
                                RANDOM_SEED.wrapping_add(t as u64),
                            );
                            for &id in &ids {
                                let guard = bm.fetch_page(id).expect("fetch page");
                                black_box(guard.data());
                            }
                        })
                    })
                    .collect();
                for h in handles {
                    h.join().expect("thread panicked");
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
    eviction_read_only,
    eviction_dirty_writeback,
    eviction_concurrent,
);
criterion_main!(benches);
