//! Benchmarks comparing sequential vs parallel merge performance.
//!
//! This tests Optimization 4: Parallel Merge using rayon
//!
//! Hypothesis: Parallelizing the merge computation across multiple cores
//! provides 4-6x speedup on 8 cores for large merges (100K+ terms).
//!
//! Target improvement: +4-6x merge throughput on 8 cores

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::time::Duration;
use tempfile::tempdir;

use libdictenstein::persistent_artrie::PersistentARTrie;
use libdictenstein::MutableMappedDictionary;

/// Generate test terms with values for merge testing
fn generate_terms_with_values(count: usize, value_offset: i64) -> Vec<(String, i64)> {
    (0..count)
        .map(|i| (format!("term_{:08}", i), (i as i64) + value_offset))
        .collect()
}

/// Create a trie populated with test terms
fn create_populated_trie(terms: &[(String, i64)]) -> (tempfile::TempDir, PersistentARTrie<i64>) {
    let dir = tempdir().expect("create temp dir");
    let path = dir.path().join("test.artrie");
    let trie: PersistentARTrie<i64> = PersistentARTrie::create(&path).expect("create trie");

    for (term, value) in terms {
        trie.insert_with_value(term, *value);
    }
    trie.sync().ok();

    (dir, trie)
}

/// Benchmark sequential merge (baseline)
fn bench_sequential_merge(c: &mut Criterion) {
    let mut group = c.benchmark_group("parallel_merge_sequential");
    group.measurement_time(Duration::from_secs(15));
    group.sample_size(20);

    for size in [1000, 5000, 10000, 50000] {
        let source_terms = generate_terms_with_values(size, 0);
        // Target has overlapping terms (50% overlap) with different values
        let target_terms: Vec<(String, i64)> = (size / 2..size + size / 2)
            .map(|i| (format!("term_{:08}", i), (i * 10) as i64))
            .collect();

        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter_with_setup(
                || {
                    let (_source_dir, source) = create_populated_trie(&source_terms);
                    let (_target_dir, target) = create_populated_trie(&target_terms);
                    // Keep temp dirs alive
                    (_source_dir, source, _target_dir, target)
                },
                |(_sd, source, _td, target)| {
                    target.merge_from(&source, |a, b| a + b).expect("merge");
                },
            );
        });
    }

    group.finish();
}

/// Benchmark parallel merge (optimization under test)
#[cfg(feature = "parallel-merge")]
fn bench_parallel_merge(c: &mut Criterion) {
    let mut group = c.benchmark_group("parallel_merge_parallel");
    group.measurement_time(Duration::from_secs(15));
    group.sample_size(20);

    for size in [1000, 5000, 10000, 50000] {
        let source_terms = generate_terms_with_values(size, 0);
        // Target has overlapping terms (50% overlap) with different values
        let target_terms: Vec<(String, i64)> = (size / 2..size + size / 2)
            .map(|i| (format!("term_{:08}", i), (i * 10) as i64))
            .collect();

        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter_with_setup(
                || {
                    let (_source_dir, source) = create_populated_trie(&source_terms);
                    let (_target_dir, target) = create_populated_trie(&target_terms);
                    // Keep temp dirs alive
                    (_source_dir, source, _target_dir, target)
                },
                |(_sd, source, _td, target)| {
                    target
                        .merge_from_parallel(&source, |a, b| a + b)
                        .expect("merge");
                },
            );
        });
    }

    group.finish();
}

/// Direct comparison at 10K for clearer results
fn bench_direct_comparison(c: &mut Criterion) {
    let mut group = c.benchmark_group("parallel_merge_comparison_10k");
    group.measurement_time(Duration::from_secs(20));
    group.sample_size(30);

    let size = 10000;
    let source_terms = generate_terms_with_values(size, 0);
    let target_terms: Vec<(String, i64)> = (size / 2..size + size / 2)
        .map(|i| (format!("term_{:08}", i), (i * 10) as i64))
        .collect();

    group.throughput(Throughput::Elements(size as u64));

    group.bench_function("sequential", |b| {
        b.iter_with_setup(
            || {
                let (_source_dir, source) = create_populated_trie(&source_terms);
                let (_target_dir, target) = create_populated_trie(&target_terms);
                (_source_dir, source, _target_dir, target)
            },
            |(_sd, source, _td, target)| {
                target.merge_from(&source, |a, b| a + b).expect("merge");
            },
        );
    });

    #[cfg(feature = "parallel-merge")]
    group.bench_function("parallel", |b| {
        b.iter_with_setup(
            || {
                let (_source_dir, source) = create_populated_trie(&source_terms);
                let (_target_dir, target) = create_populated_trie(&target_terms);
                (_source_dir, source, _target_dir, target)
            },
            |(_sd, source, _td, target)| {
                target
                    .merge_from_parallel(&source, |a, b| a + b)
                    .expect("merge");
            },
        );
    });

    group.finish();
}

/// Direct comparison at 50K for larger scale
fn bench_direct_comparison_50k(c: &mut Criterion) {
    let mut group = c.benchmark_group("parallel_merge_comparison_50k");
    group.measurement_time(Duration::from_secs(30));
    group.sample_size(20);

    let size = 50000;
    let source_terms = generate_terms_with_values(size, 0);
    let target_terms: Vec<(String, i64)> = (size / 2..size + size / 2)
        .map(|i| (format!("term_{:08}", i), (i * 10) as i64))
        .collect();

    group.throughput(Throughput::Elements(size as u64));

    group.bench_function("sequential", |b| {
        b.iter_with_setup(
            || {
                let (_source_dir, source) = create_populated_trie(&source_terms);
                let (_target_dir, target) = create_populated_trie(&target_terms);
                (_source_dir, source, _target_dir, target)
            },
            |(_sd, source, _td, target)| {
                target.merge_from(&source, |a, b| a + b).expect("merge");
            },
        );
    });

    #[cfg(feature = "parallel-merge")]
    group.bench_function("parallel", |b| {
        b.iter_with_setup(
            || {
                let (_source_dir, source) = create_populated_trie(&source_terms);
                let (_target_dir, target) = create_populated_trie(&target_terms);
                (_source_dir, source, _target_dir, target)
            },
            |(_sd, source, _td, target)| {
                target
                    .merge_from_parallel(&source, |a, b| a + b)
                    .expect("merge");
            },
        );
    });

    group.finish();
}

/// Generate terms with varied prefixes for arena-grouping benchmarks
/// Terms are distributed across different first-byte groups (a-z)
fn generate_varied_prefix_terms_with_values(count: usize, value_offset: i64) -> Vec<(String, i64)> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let prefixes = [
        "alpha", "beta", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india", "juliet",
        "kilo", "lima", "mike", "november", "oscar", "papa", "quebec", "romeo", "sierra", "tango",
        "uniform", "victor", "whiskey", "xray", "yankee", "zulu",
    ];

    let mut terms: Vec<(String, i64)> = (0..count)
        .map(|i| {
            let prefix = prefixes[i % prefixes.len()];
            (format!("{}_{:08}", prefix, i), (i as i64) + value_offset)
        })
        .collect();

    // Shuffle for realistic unsorted input
    let len = terms.len();
    for i in 0..len {
        let mut hasher = DefaultHasher::new();
        i.hash(&mut hasher);
        let j = (hasher.finish() as usize) % len;
        terms.swap(i, j);
    }

    terms
}

/// Drop page caches to force disk I/O (requires root or appropriate permissions)
/// Falls back to no-op if unavailable
fn drop_caches() {
    // Try to drop caches using Linux-specific sysctl
    // This requires the benchmark to be run with appropriate permissions
    #[cfg(target_os = "linux")]
    {
        use std::fs::OpenOptions;
        use std::io::Write;

        // Try sync first to flush dirty pages
        std::process::Command::new("sync").output().ok();

        // Try to drop caches (requires root or CAP_SYS_ADMIN)
        if let Ok(mut f) = OpenOptions::new()
            .write(true)
            .open("/proc/sys/vm/drop_caches")
        {
            let _ = f.write_all(b"3\n");
        }
    }
}

/// Create a trie populated with varied-prefix terms, synced to disk
fn create_populated_trie_varied(
    terms: &[(String, i64)],
) -> (tempfile::TempDir, PersistentARTrie<i64>) {
    let dir = tempdir().expect("create temp dir");
    let path = dir.path().join("test.artrie");
    let trie: PersistentARTrie<i64> = PersistentARTrie::create(&path).expect("create trie");

    for (term, value) in terms {
        trie.insert_with_value(term, *value);
    }
    trie.sync().ok();

    (dir, trie)
}

/// Create a trie populated with varied-prefix terms, dropped from cache
/// This forces subsequent reads to come from disk
fn create_populated_trie_varied_cold(
    terms: &[(String, i64)],
) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempdir().expect("create temp dir");
    let path = dir.path().join("test.artrie");

    {
        let trie: PersistentARTrie<i64> = PersistentARTrie::create(&path).expect("create trie");
        for (term, value) in terms {
            trie.insert_with_value(term, *value);
        }
        trie.sync().ok();
        // Drop the trie to ensure all data is flushed
    }

    // Drop caches to ensure cold reads
    drop_caches();

    (dir, path)
}

/// Benchmark arena-grouped merge vs lexicographic merge on disk-resident data
///
/// This compares merge_from_batched (lexicographic order) against
/// merge_from_batched_grouped (first-byte grouping for arena locality).
///
/// The benchmark drops page caches to simulate cold reads from disk.
fn bench_merge_arena_grouped(c: &mut Criterion) {
    let mut group = c.benchmark_group("merge_arena_grouped");
    group.measurement_time(Duration::from_secs(30));
    group.sample_size(20);

    for size in [1000, 5000, 10000] {
        // Use varied prefixes to maximize arena grouping benefit
        let source_terms = generate_varied_prefix_terms_with_values(size, 0);
        // Target has 50% overlap with different values
        let target_terms: Vec<(String, i64)> = generate_varied_prefix_terms_with_values(size, 0)
            .into_iter()
            .skip(size / 2)
            .chain((0..size / 2).map(|i| (format!("unique_target_{:08}", i), (i * 100) as i64)))
            .collect();

        group.throughput(Throughput::Elements(size as u64));

        group.bench_with_input(BenchmarkId::new("lexicographic", size), &size, |b, _| {
            b.iter_with_setup(
                || {
                    // Create tries and force to disk
                    let (_source_dir, source_path) =
                        create_populated_trie_varied_cold(&source_terms);
                    let (_target_dir, target_path) =
                        create_populated_trie_varied_cold(&target_terms);

                    // Re-open with cold caches
                    let source: PersistentARTrie<i64> =
                        PersistentARTrie::open(&source_path).expect("open source");
                    let target: PersistentARTrie<i64> =
                        PersistentARTrie::open(&target_path).expect("open target");

                    (_source_dir, source, _target_dir, target)
                },
                |(_sd, source, _td, mut target)| {
                    target
                        .merge_from_batched(&source, |a, b| a + b, 1000)
                        .expect("merge");
                },
            );
        });

        group.bench_with_input(BenchmarkId::new("arena_grouped", size), &size, |b, _| {
            b.iter_with_setup(
                || {
                    // Create tries and force to disk
                    let (_source_dir, source_path) =
                        create_populated_trie_varied_cold(&source_terms);
                    let (_target_dir, target_path) =
                        create_populated_trie_varied_cold(&target_terms);

                    // Re-open with cold caches
                    let source: PersistentARTrie<i64> =
                        PersistentARTrie::open(&source_path).expect("open source");
                    let target: PersistentARTrie<i64> =
                        PersistentARTrie::open(&target_path).expect("open target");

                    (_source_dir, source, _target_dir, target)
                },
                |(_sd, source, _td, mut target)| {
                    target
                        .merge_from_batched_grouped(&source, |a, b| a + b, 1000)
                        .expect("merge");
                },
            );
        });
    }

    group.finish();
}

#[cfg(feature = "parallel-merge")]
criterion_group!(
    benches,
    bench_sequential_merge,
    bench_parallel_merge,
    bench_direct_comparison,
    bench_direct_comparison_50k,
    bench_merge_arena_grouped,
);

#[cfg(not(feature = "parallel-merge"))]
criterion_group!(
    benches,
    bench_sequential_merge,
    bench_direct_comparison,
    bench_direct_comparison_50k,
    bench_merge_arena_grouped,
);

criterion_main!(benches);
