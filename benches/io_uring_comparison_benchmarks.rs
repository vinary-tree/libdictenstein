//! io_uring vs mmap Head-to-Head Comparison Benchmarks
//!
//! This benchmark file focuses exclusively on comparing the two I/O backends
//! under conditions designed to highlight their differences:
//!
//! 1. **Memory pressure** (small pool, large dataset) — eviction-heavy
//! 2. **WAL fsync** — StdFsync vs IoUringFsync
//! 3. **Batch I/O** — sequential vs io_uring SQE batching
//! 4. **Trie-level operations** — insert/query through full stack
//!
//! Reports latency distributions (p50/p99/p999/max) via HdrHistogram.
//!
//! ```bash
//! taskset -c 0-3 cargo bench --bench io_uring_comparison_benchmarks --features io-uring-backend
//! ```

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use hdrhistogram::Histogram;
use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::tempdir;

use libdictenstein::persistent_artrie::block_storage::{AlignedBlock, BlockStorage};
use libdictenstein::persistent_artrie::buffer_manager::BufferManager;
use libdictenstein::persistent_artrie::disk_manager::{MmapDiskManager, BLOCK_SIZE};
use libdictenstein::persistent_artrie::IoUringDiskManager;
use libdictenstein::persistent_artrie::{IoUringFsync, PersistentARTrie, StdFsync, WalSyncBackend};
use libdictenstein::Dictionary;

/// Number of blocks for memory pressure scenarios.
const PRESSURE_BLOCK_COUNT: u32 = 4096;

/// Small buffer pool to force eviction (4MB with 256KB blocks).
const PRESSURE_POOL_SIZE: usize = 16;

/// Number of blocks for standard comparison.
const STANDARD_BLOCK_COUNT: u32 = 1024;

/// Number of WAL records per fsync benchmark iteration.
const WAL_RECORDS_PER_ITER: usize = 1000;

/// Pool size for pre-registered buffer benchmarks.
///
/// Must fit within `RLIMIT_MEMLOCK` (default 8MB on most systems).
/// 16 × 256KB = 4MB, well under the 8MB limit. This is large enough
/// for meaningful latency measurements without triggering ENOMEM from
/// `register_buffers`.
const FIXED_POOL_SIZE: usize = 16;

/// Number of blocks for fixed-buffer benchmarks (matches FIXED_POOL_SIZE).
const FIXED_BLOCK_COUNT: u32 = 16;

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

fn setup_mmap(block_count: u32) -> (tempfile::TempDir, MmapDiskManager) {
    let dir = tempdir().expect("create temp dir");
    let path = dir.path().join("cmp_mmap.part");
    let dm = MmapDiskManager::create(&path).expect("create mmap disk manager");
    for _ in 0..block_count {
        dm.allocate_block().expect("allocate block");
    }
    dm.sync().expect("sync");
    (dir, dm)
}

fn setup_io_uring(block_count: u32) -> (tempfile::TempDir, IoUringDiskManager) {
    let dir = tempdir().expect("create temp dir");
    let path = dir.path().join("cmp_uring.part");
    let dm = IoUringDiskManager::create(&path).expect("create io_uring disk manager");
    for _ in 0..block_count {
        dm.allocate_block().expect("allocate block");
    }
    dm.sync().expect("sync");
    (dir, dm)
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
        "  {:>30} [{:>8}]  p50={:>8}ns  p99={:>8}ns  p999={:>9}ns  max={:>9}ns  mean={:>8.0}ns  ops/s={:>10.0}  n={}",
        name, backend, p50, p99, p999, max, mean, ops_per_sec, ops
    );
}

// =============================================================================
// Comparison 1: Random Read Under Memory Pressure
// =============================================================================

fn cmp_memory_pressure_rand_read(c: &mut Criterion) {
    let mut group = c.benchmark_group("cmp_pressure_rand_read");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(15));

    let num_reads = PRESSURE_BLOCK_COUNT as u64;
    group.throughput(Throughput::Elements(num_reads));

    let read_ids = shuffled_block_ids(PRESSURE_BLOCK_COUNT, RANDOM_SEED);

    // --- mmap ---
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

                for &block_id in &read_ids {
                    let start = Instant::now();
                    let guard = bm.fetch_page(block_id).expect("fetch page");
                    black_box(guard.data());
                    drop(guard);
                    hist.record(start.elapsed().as_nanos() as u64).ok();
                }

                report_histogram("pressure_rand_read", "mmap", &hist);
                total += Duration::from_nanos(hist.mean() as u64 * num_reads);
            }
            total
        });
    });

    // --- io_uring ---
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

                report_histogram("pressure_rand_read", "io_uring", &hist);
                total += Duration::from_nanos(hist.mean() as u64 * num_reads);
            }
            total
        });
    });

    group.finish();
}

// =============================================================================
// Comparison 2: Mixed R/W Under Memory Pressure
// =============================================================================

fn cmp_memory_pressure_mixed(c: &mut Criterion) {
    let mut group = c.benchmark_group("cmp_pressure_mixed");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(15));

    let num_ops = PRESSURE_BLOCK_COUNT as u64;
    group.throughput(Throughput::Elements(num_ops));

    let op_ids = shuffled_block_ids(PRESSURE_BLOCK_COUNT, RANDOM_SEED + 1);

    // --- mmap ---
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

                report_histogram("pressure_mixed_80r20w", "mmap", &hist);
                total += Duration::from_nanos(hist.mean() as u64 * num_ops);
            }
            total
        });
    });

    // --- io_uring ---
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

                report_histogram("pressure_mixed_80r20w", "io_uring", &hist);
                total += Duration::from_nanos(hist.mean() as u64 * num_ops);
            }
            total
        });
    });

    group.finish();
}

// =============================================================================
// Comparison 3: Batch Read (SQE batching vs sequential)
// =============================================================================

fn cmp_batch_read(c: &mut Criterion) {
    let mut group = c.benchmark_group("cmp_batch_read");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(10));

    let batch_size = 64u32;
    group.throughput(Throughput::Elements(batch_size as u64));

    // --- mmap (sequential read_block calls) ---
    group.bench_function("mmap_sequential", |b| {
        let (_dir, dm) = setup_mmap(batch_size);
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

                let start = Instant::now();
                for (i, buf) in buffers.iter_mut().enumerate() {
                    dm.read_block((i + 1) as u32, &mut buf.data).expect("read");
                }
                total += start.elapsed();
                black_box(&buffers);
            }
            total
        });
    });

    // --- io_uring (batch SQE submission) ---
    group.bench_function("io_uring_batch", |b| {
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

    // --- io_uring (sequential for comparison) ---
    group.bench_function("io_uring_sequential", |b| {
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

                let start = Instant::now();
                for (i, buf) in buffers.iter_mut().enumerate() {
                    dm.read_block((i + 1) as u32, &mut buf.data).expect("read");
                }
                total += start.elapsed();
                black_box(&buffers);
            }
            total
        });
    });

    group.finish();
}

// =============================================================================
// Comparison 4: WAL fsync (StdFsync vs IoUringFsync)
// =============================================================================

fn cmp_wal_fsync(c: &mut Criterion) {
    let mut group = c.benchmark_group("cmp_wal_fsync");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(10));
    group.throughput(Throughput::Elements(WAL_RECORDS_PER_ITER as u64));

    // --- StdFsync (file.sync_all()) ---
    group.bench_function("std_fsync", |b| {
        let dir = tempdir().expect("create temp dir");
        let wal_path = dir.path().join("wal_std.log");

        let backend: Arc<dyn WalSyncBackend> = Arc::new(StdFsync);

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let mut hist = Histogram::<u64>::new_with_bounds(1, 60_000_000_000, 3)
                    .expect("create histogram");

                // Re-create file each iteration
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(&wal_path)
                    .expect("reopen WAL file");

                for i in 0..WAL_RECORDS_PER_ITER {
                    // Write a record
                    let record = format!("WAL record {}\n", i);
                    f.write_all(record.as_bytes()).expect("write WAL record");

                    // Measure fsync
                    let start = Instant::now();
                    backend.sync_file(&f).expect("sync");
                    hist.record(start.elapsed().as_nanos() as u64).ok();
                }

                report_histogram("wal_per_record_fsync", "std", &hist);
                total += Duration::from_nanos(hist.mean() as u64 * WAL_RECORDS_PER_ITER as u64);
            }
            total
        });
    });

    // --- IoUringFsync (IORING_OP_FSYNC) ---
    group.bench_function("io_uring_fsync", |b| {
        let dir = tempdir().expect("create temp dir");
        let wal_path = dir.path().join("wal_uring.log");

        let backend: Arc<dyn WalSyncBackend> =
            Arc::new(IoUringFsync::new(8).expect("create io_uring fsync backend"));

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let mut hist = Histogram::<u64>::new_with_bounds(1, 60_000_000_000, 3)
                    .expect("create histogram");

                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(&wal_path)
                    .expect("reopen WAL file");

                for i in 0..WAL_RECORDS_PER_ITER {
                    let record = format!("WAL record {}\n", i);
                    f.write_all(record.as_bytes()).expect("write WAL record");

                    let start = Instant::now();
                    backend.sync_file(&f).expect("sync");
                    hist.record(start.elapsed().as_nanos() as u64).ok();
                }

                report_histogram("wal_per_record_fsync", "io_uring", &hist);
                total += Duration::from_nanos(hist.mean() as u64 * WAL_RECORDS_PER_ITER as u64);
            }
            total
        });
    });

    // --- StdFsync batched (100 records then sync) ---
    group.bench_function("std_fsync_batched", |b| {
        let dir = tempdir().expect("create temp dir");
        let wal_path = dir.path().join("wal_std_batch.log");
        let backend: Arc<dyn WalSyncBackend> = Arc::new(StdFsync);

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(&wal_path)
                    .expect("reopen WAL file");

                // Write 100 records
                for i in 0..100 {
                    let record = format!("WAL record {}\n", i);
                    f.write_all(record.as_bytes()).expect("write WAL record");
                }

                let start = Instant::now();
                backend.sync_file(&f).expect("sync");
                total += start.elapsed();
            }
            total
        });
    });

    // --- IoUringFsync batched ---
    group.bench_function("io_uring_fsync_batched", |b| {
        let dir = tempdir().expect("create temp dir");
        let wal_path = dir.path().join("wal_uring_batch.log");
        let backend: Arc<dyn WalSyncBackend> =
            Arc::new(IoUringFsync::new(8).expect("create io_uring fsync backend"));

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(&wal_path)
                    .expect("reopen WAL file");

                for i in 0..100 {
                    let record = format!("WAL record {}\n", i);
                    f.write_all(record.as_bytes()).expect("write WAL record");
                }

                let start = Instant::now();
                backend.sync_file(&f).expect("sync");
                total += start.elapsed();
            }
            total
        });
    });

    group.finish();
}

// =============================================================================
// Comparison 5: Trie-Level Insert/Query (Full Stack)
// =============================================================================

fn cmp_trie_insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("cmp_trie_insert");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(15));

    let num_terms: usize = 10_000;
    group.throughput(Throughput::Elements(num_terms as u64));

    let terms: Vec<String> = (0..num_terms).map(|i| format!("term_{:08}", i)).collect();

    // --- mmap ---
    group.bench_function("mmap", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let dir = tempdir().expect("create temp dir");
                let path = dir.path().join("trie_mmap.part");
                let dict: PersistentARTrie<()> = PersistentARTrie::create(&path).expect("create");

                let start = Instant::now();
                for term in &terms {
                    dict.insert(term);
                }
                dict.sync().expect("sync");
                total += start.elapsed();

                black_box(&dict);
            }
            total
        });
    });

    // --- io_uring ---
    group.bench_function("io_uring", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let dir = tempdir().expect("create temp dir");
                let path = dir.path().join("trie_uring.part");
                let dict: PersistentARTrie<(), _> =
                    PersistentARTrie::create_with_io_uring(&path).expect("create");

                let start = Instant::now();
                for term in &terms {
                    dict.insert(term);
                }
                dict.sync().expect("sync");
                total += start.elapsed();

                black_box(&dict);
            }
            total
        });
    });

    group.finish();
}

fn cmp_trie_query(c: &mut Criterion) {
    let mut group = c.benchmark_group("cmp_trie_query");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(15));

    let num_terms: usize = 10_000;
    group.throughput(Throughput::Elements(num_terms as u64));

    let terms: Vec<String> = (0..num_terms).map(|i| format!("term_{:08}", i)).collect();

    // --- mmap ---
    group.bench_function("mmap", |b| {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("query_mmap.part");
        let dict: PersistentARTrie<()> = PersistentARTrie::create(&path).expect("create");
        for term in &terms {
            dict.insert(term);
        }
        dict.sync().expect("sync");

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let start = Instant::now();
                for term in &terms {
                    black_box(dict.contains(term));
                }
                total += start.elapsed();
            }
            total
        });
    });

    // --- io_uring ---
    group.bench_function("io_uring", |b| {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("query_uring.part");
        let dict: PersistentARTrie<(), _> =
            PersistentARTrie::create_with_io_uring(&path).expect("create");
        for term in &terms {
            dict.insert(term);
        }
        dict.sync().expect("sync");

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let start = Instant::now();
                for term in &terms {
                    black_box(dict.contains(term));
                }
                total += start.elapsed();
            }
            total
        });
    });

    group.finish();
}

// =============================================================================
// Summary Report: Side-by-side comparison
// =============================================================================

fn cmp_summary_report(c: &mut Criterion) {
    let mut group = c.benchmark_group("cmp_summary");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(5));

    group.bench_function("print_comparison", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;

            for _ in 0..iters {
                eprintln!("\n{}", "=".repeat(72));
                eprintln!("  io_uring vs mmap Head-to-Head Comparison");
                eprintln!("{}\n", "=".repeat(72));

                // Block-level random reads under pressure
                let read_ids = shuffled_block_ids(STANDARD_BLOCK_COUNT, RANDOM_SEED);

                let (_dir_m, dm_m) = setup_mmap(STANDARD_BLOCK_COUNT);
                let block = AlignedBlock::new_boxed();
                for id in 1..=STANDARD_BLOCK_COUNT {
                    dm_m.write_block(id, &block.data).expect("write");
                }
                dm_m.sync().expect("sync");

                let (_dir_u, dm_u) = setup_io_uring(STANDARD_BLOCK_COUNT);
                let block2 = AlignedBlock::new_boxed();
                for id in 1..=STANDARD_BLOCK_COUNT {
                    dm_u.write_block(id, &block2.data).expect("write");
                }
                dm_u.sync().expect("sync");

                let start = Instant::now();

                // Random reads
                let h = bench_block_ops(&dm_m, &read_ids, true);
                report_histogram("rand_read_block", "mmap", &h);

                let h = bench_block_ops(&dm_u, &read_ids, true);
                report_histogram("rand_read_block", "io_uring", &h);

                // Random writes
                let h = bench_block_ops(&dm_m, &read_ids, false);
                report_histogram("rand_write_block", "mmap", &h);

                let h = bench_block_ops(&dm_u, &read_ids, false);
                report_histogram("rand_write_block", "io_uring", &h);

                // Sync latency
                let h = bench_sync(&dm_m);
                report_histogram("sync", "mmap", &h);

                let h = bench_sync(&dm_u);
                report_histogram("sync", "io_uring", &h);

                total += start.elapsed();
            }

            total
        });
    });

    group.finish();
}

/// Run a block-level benchmark, returning the histogram.
fn bench_block_ops<S: BlockStorage>(storage: &S, block_ids: &[u32], read: bool) -> Histogram<u64> {
    let mut hist =
        Histogram::<u64>::new_with_bounds(1, 60_000_000_000, 3).expect("create histogram");
    let mut block = AlignedBlock::new_boxed();

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
        hist.record(start.elapsed().as_nanos() as u64).ok();
    }

    hist
}

/// Benchmark sync latency (write 10 blocks, then sync).
fn bench_sync<S: BlockStorage>(storage: &S) -> Histogram<u64> {
    let mut hist =
        Histogram::<u64>::new_with_bounds(1, 60_000_000_000, 3).expect("create histogram");
    let block = AlignedBlock::new_boxed();

    for _ in 0..100 {
        // Write a few blocks to make sync non-trivial
        for id in 1..=10 {
            storage.write_block(id, &block.data).expect("write");
        }

        let start = Instant::now();
        storage.sync().expect("sync");
        hist.record(start.elapsed().as_nanos() as u64).ok();
    }

    hist
}

// =============================================================================
// Comparison 6: Single-Block Read Fixed vs Read vs mmap
// =============================================================================

fn cmp_single_block_read_fixed(c: &mut Criterion) {
    let mut group = c.benchmark_group("cmp_single_block_read_fixed");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(10));

    // Use small pool (FIXED_POOL_SIZE=16) so register_buffers succeeds under
    // default RLIMIT_MEMLOCK (8MB). 16 × 256KB = 4MB < 8MB.
    let num_reads = FIXED_BLOCK_COUNT as u64;
    group.throughput(Throughput::Elements(num_reads));

    let read_ids = shuffled_block_ids(FIXED_BLOCK_COUNT, RANDOM_SEED);

    // --- mmap (baseline) ---
    group.bench_function("mmap", |b| {
        let (_dir, dm) = setup_mmap(FIXED_BLOCK_COUNT);
        let block = AlignedBlock::new_boxed();
        for id in 1..=FIXED_BLOCK_COUNT {
            dm.write_block(id, &block.data).expect("write");
        }
        dm.sync().expect("sync");

        let bm = BufferManager::new(dm, FIXED_POOL_SIZE);

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

                report_histogram("single_block_read", "mmap", &hist);
                total += Duration::from_nanos(hist.mean() as u64 * num_reads);
            }
            total
        });
    });

    // --- io_uring with ReadFixed (pre-registered buffers) ---
    group.bench_function("io_uring_fixed", |b| {
        let (_dir, dm) = setup_io_uring(FIXED_BLOCK_COUNT);
        let block = AlignedBlock::new_boxed();
        for id in 1..=FIXED_BLOCK_COUNT {
            dm.write_block(id, &block.data).expect("write");
        }
        dm.sync().expect("sync");

        // BufferManager::new() auto-registers the pool for ReadFixed/WriteFixed.
        // Pool of 16 × 256KB = 4MB fits within RLIMIT_MEMLOCK (8MB).
        let bm = BufferManager::new(dm, FIXED_POOL_SIZE);
        eprintln!(
            "[cmp_single_block_read_fixed] io_uring_fixed: buffers_registered={}",
            bm.storage().supports_fixed_buffers()
        );

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

                report_histogram("single_block_read", "io_uring_fixed", &hist);
                total += Duration::from_nanos(hist.mean() as u64 * num_reads);
            }
            total
        });
    });

    // --- io_uring without ReadFixed (standard Read, for comparison) ---
    group.bench_function("io_uring_standard", |b| {
        let (_dir, dm) = setup_io_uring(FIXED_BLOCK_COUNT);
        let block = AlignedBlock::new_boxed();
        for id in 1..=FIXED_BLOCK_COUNT {
            dm.write_block(id, &block.data).expect("write");
        }
        dm.sync().expect("sync");

        // Direct block reads bypass BufferManager (no registration = no fixed buffers)
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let mut hist = Histogram::<u64>::new_with_bounds(1, 60_000_000_000, 3)
                    .expect("create histogram");
                let mut buf = AlignedBlock::new_boxed();

                for &block_id in &read_ids {
                    let start = Instant::now();
                    dm.read_block(block_id, &mut buf.data).expect("read");
                    black_box(&buf.data);
                    hist.record(start.elapsed().as_nanos() as u64).ok();
                }

                report_histogram("single_block_read", "io_uring_std", &hist);
                total += Duration::from_nanos(hist.mean() as u64 * num_reads);
            }
            total
        });
    });

    group.finish();
}

// =============================================================================
// Comparison 7: Single-Block Write Fixed vs Write vs mmap
// =============================================================================

fn cmp_single_block_write_fixed(c: &mut Criterion) {
    let mut group = c.benchmark_group("cmp_single_block_write_fixed");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(10));

    // Use small pool to fit within RLIMIT_MEMLOCK
    let num_writes = FIXED_BLOCK_COUNT as u64;
    group.throughput(Throughput::Elements(num_writes));

    let write_ids = shuffled_block_ids(FIXED_BLOCK_COUNT, RANDOM_SEED + 42);

    // --- mmap (baseline) ---
    group.bench_function("mmap", |b| {
        let (_dir, dm) = setup_mmap(FIXED_BLOCK_COUNT);
        let bm = BufferManager::new(dm, FIXED_POOL_SIZE);

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let mut hist = Histogram::<u64>::new_with_bounds(1, 60_000_000_000, 3)
                    .expect("create histogram");

                for &block_id in &write_ids {
                    let start = Instant::now();
                    let mut guard = bm.fetch_page_mut(block_id).expect("fetch page mut");
                    guard.data_mut()[0] = block_id as u8;
                    drop(guard);
                    hist.record(start.elapsed().as_nanos() as u64).ok();
                }

                report_histogram("single_block_write", "mmap", &hist);
                total += Duration::from_nanos(hist.mean() as u64 * num_writes);
            }
            total
        });
    });

    // --- io_uring with WriteFixed (pre-registered buffers) ---
    group.bench_function("io_uring_fixed", |b| {
        let (_dir, dm) = setup_io_uring(FIXED_BLOCK_COUNT);
        let bm = BufferManager::new(dm, FIXED_POOL_SIZE);
        eprintln!(
            "[cmp_single_block_write_fixed] io_uring_fixed: buffers_registered={}",
            bm.storage().supports_fixed_buffers()
        );

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let mut hist = Histogram::<u64>::new_with_bounds(1, 60_000_000_000, 3)
                    .expect("create histogram");

                for &block_id in &write_ids {
                    let start = Instant::now();
                    let mut guard = bm.fetch_page_mut(block_id).expect("fetch page mut");
                    guard.data_mut()[0] = block_id as u8;
                    drop(guard);
                    hist.record(start.elapsed().as_nanos() as u64).ok();
                }

                report_histogram("single_block_write", "io_uring_fixed", &hist);
                total += Duration::from_nanos(hist.mean() as u64 * num_writes);
            }
            total
        });
    });

    group.finish();
}

// =============================================================================
// Comparison 8: Batched Dirty Flush (batched vs one-by-one)
// =============================================================================

fn cmp_batch_flush_dirty(c: &mut Criterion) {
    let mut group = c.benchmark_group("cmp_batch_flush_dirty");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(10));

    let dirty_count = 64u32;
    group.throughput(Throughput::Elements(dirty_count as u64));

    // --- mmap sync ---
    group.bench_function("mmap_sync", |b| {
        let (_dir, dm) = setup_mmap(dirty_count);

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                // Write dirty data via sub-block API (goes through cache)
                for id in 1..=dirty_count {
                    dm.write_bytes(id, 0, &[id as u8; 16]).expect("write bytes");
                }

                let start = Instant::now();
                dm.sync().expect("sync");
                total += start.elapsed();
            }
            total
        });
    });

    // --- io_uring batched sync (new: batched flush_dirty_cache) ---
    group.bench_function("io_uring_batched_sync", |b| {
        let (_dir, dm) = setup_io_uring(dirty_count);

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                for id in 1..=dirty_count {
                    dm.write_bytes(id, 0, &[id as u8; 16]).expect("write bytes");
                }

                let start = Instant::now();
                dm.sync().expect("sync");
                total += start.elapsed();
            }
            total
        });
    });

    group.finish();
}

// =============================================================================
// Comparison 9: BufferManager::flush_all with Fixed Buffers
// =============================================================================

fn cmp_flush_all_fixed(c: &mut Criterion) {
    let mut group = c.benchmark_group("cmp_flush_all_fixed");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(10));

    // Use FIXED_POOL_SIZE to fit within RLIMIT_MEMLOCK for actual WriteFixed usage
    let dirty_count = FIXED_BLOCK_COUNT;
    group.throughput(Throughput::Elements(dirty_count as u64));

    // --- mmap flush_all ---
    group.bench_function("mmap", |b| {
        let (_dir, dm) = setup_mmap(dirty_count);
        let bm = BufferManager::new(dm, FIXED_POOL_SIZE);

        // Pre-load pages
        for id in 1..=dirty_count {
            let mut guard = bm.fetch_page_mut(id).expect("fetch page mut");
            guard.data_mut()[0] = id as u8;
            drop(guard);
        }
        bm.flush_all().expect("initial flush");

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                // Dirty all pages
                for id in 1..=dirty_count {
                    let mut guard = bm.fetch_page_mut(id).expect("fetch page mut");
                    guard.data_mut()[0] = (id + 1) as u8;
                    drop(guard);
                }

                let start = Instant::now();
                bm.flush_all().expect("flush_all");
                total += start.elapsed();
            }
            total
        });
    });

    // --- io_uring flush_all with fixed buffers (batched WriteFixed) ---
    group.bench_function("io_uring_fixed", |b| {
        let (_dir, dm) = setup_io_uring(dirty_count);
        let bm = BufferManager::new(dm, FIXED_POOL_SIZE);
        eprintln!(
            "[cmp_flush_all_fixed] io_uring_fixed: buffers_registered={}",
            bm.storage().supports_fixed_buffers()
        );

        // Pre-load pages
        for id in 1..=dirty_count {
            let mut guard = bm.fetch_page_mut(id).expect("fetch page mut");
            guard.data_mut()[0] = id as u8;
            drop(guard);
        }
        bm.flush_all().expect("initial flush");

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                for id in 1..=dirty_count {
                    let mut guard = bm.fetch_page_mut(id).expect("fetch page mut");
                    guard.data_mut()[0] = (id + 1) as u8;
                    drop(guard);
                }

                let start = Instant::now();
                bm.flush_all().expect("flush_all");
                total += start.elapsed();
            }
            total
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    cmp_memory_pressure_rand_read,
    cmp_memory_pressure_mixed,
    cmp_batch_read,
    cmp_wal_fsync,
    cmp_trie_insert,
    cmp_trie_query,
    cmp_single_block_read_fixed,
    cmp_single_block_write_fixed,
    cmp_batch_flush_dirty,
    cmp_flush_all_fixed,
    cmp_summary_report,
);

criterion_main!(benches);
