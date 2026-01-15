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
    (0..count)
        .map(|i| format!("term_{:08}", i))
        .collect()
}

/// Generate test terms in random/shuffled order (simulating real-world unsorted input)
fn generate_shuffled_terms(count: usize) -> Vec<String> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut terms: Vec<String> = (0..count)
        .map(|i| format!("term_{:08}", i))
        .collect();

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

    let prefixes = ["alpha", "beta", "gamma", "delta", "epsilon",
                    "zeta", "eta", "theta", "iota", "kappa"];

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
        let entries: Vec<(String, Option<()>)> = shuffled_terms
            .iter()
            .map(|t| (t.clone(), None))
            .collect();

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
        let entries: Vec<(String, Option<()>)> = shuffled_terms
            .iter()
            .map(|t| (t.clone(), None))
            .collect();

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
        let entries: Vec<(String, Option<()>)> = terms
            .iter()
            .map(|t| (t.clone(), None))
            .collect();

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
        let entries: Vec<(String, Option<()>)> = terms
            .iter()
            .map(|t| (t.clone(), None))
            .collect();

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
    group.sample_size(50);  // More samples for statistical confidence

    let size = 10000;
    let shuffled_terms = generate_shuffled_terms(size);
    let entries: Vec<(String, Option<()>)> = shuffled_terms
        .iter()
        .map(|t| (t.clone(), None))
        .collect();

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

criterion_group!(
    benches,
    bench_unsorted_batch_inserts,
    bench_sorted_batch_inserts,
    bench_varied_prefix_unsorted,
    bench_varied_prefix_sorted,
    bench_direct_comparison,
);
criterion_main!(benches);
