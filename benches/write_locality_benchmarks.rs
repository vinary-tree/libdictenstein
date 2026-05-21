//! Benchmarks comparing sorted vs unsorted batch insert performance.
//!
//! This tests Optimization 3: Write Locality (Prefix Sorting)
//!
//! Hypothesis: Sorting terms lexicographically before batch insert improves
//! cache locality because consecutive terms share trie prefix paths, leading
//! to better CPU cache utilization.
//!
//! Target improvement: +5-20% insert throughput

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::time::Duration;
use tempfile::tempdir;

use libdictenstein::persistent_artrie::PersistentARTrie;

/// Generate test terms in sorted order
fn generate_sorted_terms(count: usize) -> Vec<String> {
    (0..count).map(|i| format!("term_{:08}", i)).collect()
}

/// Generate test terms in random/shuffled order (simulating real-world unsorted input)
fn generate_shuffled_terms(count: usize) -> Vec<String> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut terms: Vec<String> = (0..count).map(|i| format!("term_{:08}", i)).collect();

    // Deterministic shuffle using hash for reproducibility
    let len = terms.len();
    for i in 0..len {
        let mut hasher = DefaultHasher::new();
        i.hash(&mut hasher);
        let j = (hasher.finish() as usize) % len;
        terms.swap(i, j);
    }

    terms
}

/// Generate terms with varied prefixes (more realistic workload)
fn generate_varied_prefix_terms(count: usize) -> Vec<String> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let prefixes = [
        "alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta", "iota", "kappa",
    ];

    let mut terms: Vec<String> = (0..count)
        .map(|i| {
            let prefix = prefixes[i % prefixes.len()];
            format!("{}_{:08}", prefix, i)
        })
        .collect();

    // Deterministic shuffle
    let len = terms.len();
    for i in 0..len {
        let mut hasher = DefaultHasher::new();
        i.hash(&mut hasher);
        let j = (hasher.finish() as usize) % len;
        terms.swap(i, j);
    }

    terms
}

/// Benchmark unsorted batch inserts (baseline)
fn bench_unsorted_batch_inserts(c: &mut Criterion) {
    let mut group = c.benchmark_group("write_locality_unsorted");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(30);

    for size in [1000, 5000, 10000] {
        let shuffled_terms = generate_shuffled_terms(size);
        let entries: Vec<(String, Option<()>)> =
            shuffled_terms.iter().map(|t| (t.clone(), None)).collect();

        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &entries, |b, entries| {
            b.iter_with_setup(
                || {
                    let dir = tempdir().expect("create temp dir");
                    let path = dir.path().join("test.artrie");
                    let trie: PersistentARTrie<()> =
                        PersistentARTrie::create(&path).expect("create trie");
                    (dir, trie, entries.clone())
                },
                |(_dir, mut trie, entries)| {
                    trie.insert_batch(&entries);
                    trie.sync().ok();
                },
            );
        });
    }

    group.finish();
}

/// Benchmark sorted batch inserts (optimization under test)
fn bench_sorted_batch_inserts(c: &mut Criterion) {
    let mut group = c.benchmark_group("write_locality_sorted");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(30);

    for size in [1000, 5000, 10000] {
        let shuffled_terms = generate_shuffled_terms(size);
        let entries: Vec<(String, Option<()>)> =
            shuffled_terms.iter().map(|t| (t.clone(), None)).collect();

        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &entries, |b, entries| {
            b.iter_with_setup(
                || {
                    let dir = tempdir().expect("create temp dir");
                    let path = dir.path().join("test.artrie");
                    let trie: PersistentARTrie<()> =
                        PersistentARTrie::create(&path).expect("create trie");
                    (dir, trie, entries.clone())
                },
                |(_dir, mut trie, entries)| {
                    // Use the sorted batch insert which sorts internally
                    trie.insert_batch_sorted(entries);
                    trie.sync().ok();
                },
            );
        });
    }

    group.finish();
}

/// Benchmark with varied prefixes (more realistic workload)
fn bench_varied_prefix_unsorted(c: &mut Criterion) {
    let mut group = c.benchmark_group("write_locality_varied_unsorted");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(30);

    for size in [1000, 5000, 10000] {
        let terms = generate_varied_prefix_terms(size);
        let entries: Vec<(String, Option<()>)> = terms.iter().map(|t| (t.clone(), None)).collect();

        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &entries, |b, entries| {
            b.iter_with_setup(
                || {
                    let dir = tempdir().expect("create temp dir");
                    let path = dir.path().join("test.artrie");
                    let trie: PersistentARTrie<()> =
                        PersistentARTrie::create(&path).expect("create trie");
                    (dir, trie, entries.clone())
                },
                |(_dir, mut trie, entries)| {
                    trie.insert_batch(&entries);
                    trie.sync().ok();
                },
            );
        });
    }

    group.finish();
}

/// Benchmark with varied prefixes sorted
fn bench_varied_prefix_sorted(c: &mut Criterion) {
    let mut group = c.benchmark_group("write_locality_varied_sorted");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(30);

    for size in [1000, 5000, 10000] {
        let terms = generate_varied_prefix_terms(size);
        let entries: Vec<(String, Option<()>)> = terms.iter().map(|t| (t.clone(), None)).collect();

        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &entries, |b, entries| {
            b.iter_with_setup(
                || {
                    let dir = tempdir().expect("create temp dir");
                    let path = dir.path().join("test.artrie");
                    let trie: PersistentARTrie<()> =
                        PersistentARTrie::create(&path).expect("create trie");
                    (dir, trie, entries.clone())
                },
                |(_dir, mut trie, entries)| {
                    trie.insert_batch_sorted(entries);
                    trie.sync().ok();
                },
            );
        });
    }

    group.finish();
}

/// Direct comparison at a single size for clearer results
fn bench_direct_comparison(c: &mut Criterion) {
    let mut group = c.benchmark_group("write_locality_comparison_10k");
    group.measurement_time(Duration::from_secs(15));
    group.sample_size(50); // More samples for statistical confidence

    let size = 10000;
    let shuffled_terms = generate_shuffled_terms(size);
    let entries: Vec<(String, Option<()>)> =
        shuffled_terms.iter().map(|t| (t.clone(), None)).collect();

    group.throughput(Throughput::Elements(size as u64));

    group.bench_function("unsorted", |b| {
        b.iter_with_setup(
            || {
                let dir = tempdir().expect("create temp dir");
                let path = dir.path().join("test.artrie");
                let trie: PersistentARTrie<()> =
                    PersistentARTrie::create(&path).expect("create trie");
                (dir, trie, entries.clone())
            },
            |(_dir, mut trie, entries)| {
                trie.insert_batch(&entries);
                trie.sync().ok();
            },
        );
    });

    group.bench_function("sorted", |b| {
        b.iter_with_setup(
            || {
                let dir = tempdir().expect("create temp dir");
                let path = dir.path().join("test.artrie");
                let trie: PersistentARTrie<()> =
                    PersistentARTrie::create(&path).expect("create trie");
                (dir, trie, entries.clone())
            },
            |(_dir, mut trie, entries)| {
                trie.insert_batch_sorted(entries);
                trie.sync().ok();
            },
        );
    });

    group.finish();
}

/// Drop page caches to force disk I/O (requires root or appropriate permissions)
/// Falls back to no-op if unavailable
fn drop_caches() {
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

/// Generate entries with varied prefixes for arena-grouped insert benchmarks
/// Uses byte-vec format as required by insert_batch_arena_grouped
fn generate_varied_prefix_entries(count: usize) -> Vec<(Vec<u8>, Option<u32>)> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let prefixes = [
        "alpha", "beta", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india", "juliet",
        "kilo", "lima", "mike", "november", "oscar", "papa", "quebec", "romeo", "sierra", "tango",
        "uniform", "victor", "whiskey", "xray", "yankee", "zulu",
    ];

    let mut entries: Vec<(Vec<u8>, Option<u32>)> = (0..count)
        .map(|i| {
            let prefix = prefixes[i % prefixes.len()];
            let term = format!("{}_{:08}", prefix, i);
            (term.into_bytes(), Some(i as u32))
        })
        .collect();

    // Shuffle for realistic unsorted input
    let len = entries.len();
    for i in 0..len {
        let mut hasher = DefaultHasher::new();
        i.hash(&mut hasher);
        let j = (hasher.finish() as usize) % len;
        entries.swap(i, j);
    }

    entries
}

/// Benchmark arena-grouped insert vs lexicographically sorted insert on disk
///
/// This compares insert_batch_sorted (lexicographic sorting) against
/// insert_batch_arena_grouped (first-byte grouping for arena locality).
///
/// The benchmark drops page caches to simulate cold disk operations.
fn bench_insert_arena_grouped(c: &mut Criterion) {
    let mut group = c.benchmark_group("insert_arena_grouped");
    group.measurement_time(Duration::from_secs(20));
    group.sample_size(20);

    for size in [1000, 5000, 10000] {
        let entries = generate_varied_prefix_entries(size);

        // Convert to String format for insert_batch_sorted
        let string_entries: Vec<(String, Option<u32>)> = entries
            .iter()
            .map(|(k, v)| (String::from_utf8_lossy(k).to_string(), *v))
            .collect();

        group.throughput(Throughput::Elements(size as u64));

        group.bench_with_input(BenchmarkId::new("sorted", size), &size, |b, _| {
            b.iter_with_setup(
                || {
                    drop_caches(); // Force cold state
                    let dir = tempdir().expect("create temp dir");
                    let path = dir.path().join("test.artrie");
                    let trie: PersistentARTrie<u32> =
                        PersistentARTrie::create(&path).expect("create trie");
                    (dir, trie, string_entries.clone())
                },
                |(_dir, mut trie, e)| {
                    trie.insert_batch_sorted(e);
                    trie.sync().ok();
                },
            );
        });

        group.bench_with_input(BenchmarkId::new("arena_grouped", size), &size, |b, _| {
            b.iter_with_setup(
                || {
                    drop_caches(); // Force cold state
                    let dir = tempdir().expect("create temp dir");
                    let path = dir.path().join("test.artrie");
                    let trie: PersistentARTrie<u32> =
                        PersistentARTrie::create(&path).expect("create trie");
                    (dir, trie, entries.clone())
                },
                |(_dir, mut trie, e)| {
                    trie.insert_batch_arena_grouped(e);
                    trie.sync().ok();
                },
            );
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_unsorted_batch_inserts,
    bench_sorted_batch_inserts,
    bench_varied_prefix_unsorted,
    bench_varied_prefix_sorted,
    bench_direct_comparison,
    bench_insert_arena_grouped,
);
criterion_main!(benches);
