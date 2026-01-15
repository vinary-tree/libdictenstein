//! Benchmarks comparing individual vs batch WAL insert performance.
//!
//! These benchmarks measure:
//! 1. Individual insert performance (current approach)
//! 2. Batch insert performance (new approach)
//! 3. WAL size comparison to verify header overhead reduction

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::time::Duration;
use tempfile::tempdir;

use libdictenstein::persistent_artrie::PersistentARTrie;

/// Generate test terms for benchmarking
fn generate_terms(count: usize) -> Vec<String> {
    (0..count)
        .map(|i| format!("term_{:08}", i))
        .collect()
}

/// Benchmark individual inserts with WAL logging
fn bench_individual_inserts(c: &mut Criterion) {
    let mut group = c.benchmark_group("wal_insert_individual");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(30);

    for size in [100, 1000, 10000] {
        let terms = generate_terms(size);

        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &terms, |b, terms| {
            b.iter_with_setup(
                || {
                    let dir = tempdir().expect("create temp dir");
                    let path = dir.path().join("test.artrie");
                    let trie: PersistentARTrie<()> =
                        PersistentARTrie::create(&path).expect("create trie");
                    (dir, trie, terms.clone())
                },
                |(_dir, mut trie, terms)| {
                    for term in terms {
                        trie.insert(&term);
                    }
                    // Force sync to ensure WAL is flushed
                    trie.sync().ok();
                },
            );
        });
    }

    group.finish();
}

/// Benchmark batch inserts with single WAL record
fn bench_batch_inserts(c: &mut Criterion) {
    let mut group = c.benchmark_group("wal_insert_batch");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(30);

    for size in [100, 1000, 10000] {
        let terms = generate_terms(size);
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
                    // Force sync to ensure WAL is flushed
                    trie.sync().ok();
                },
            );
        });
    }

    group.finish();
}

/// Benchmark that measures WAL size for individual vs batch inserts
fn bench_wal_size_comparison(c: &mut Criterion) {
    let mut group = c.benchmark_group("wal_size_comparison");
    group.measurement_time(Duration::from_secs(5));
    group.sample_size(10);

    let size = 1000;
    let terms = generate_terms(size);
    let entries: Vec<(String, Option<()>)> = terms
        .iter()
        .map(|t| (t.clone(), None))
        .collect();

    // Measure individual insert WAL size
    group.bench_function("individual_wal_size", |b| {
        b.iter_with_setup(
            || {
                let dir = tempdir().expect("create temp dir");
                let path = dir.path().join("test.artrie");
                let wal_path = dir.path().join("test.wal");
                let trie: PersistentARTrie<()> =
                    PersistentARTrie::create(&path).expect("create trie");
                (dir, trie, wal_path, terms.clone())
            },
            |(_dir, mut trie, wal_path, terms)| {
                for term in terms {
                    trie.insert(&term);
                }
                trie.sync().ok();
                // Return WAL file size
                std::fs::metadata(&wal_path).map(|m| m.len()).unwrap_or(0)
            },
        );
    });

    // Measure batch insert WAL size
    group.bench_function("batch_wal_size", |b| {
        b.iter_with_setup(
            || {
                let dir = tempdir().expect("create temp dir");
                let path = dir.path().join("test.artrie");
                let wal_path = dir.path().join("test.wal");
                let trie: PersistentARTrie<()> =
                    PersistentARTrie::create(&path).expect("create trie");
                (dir, trie, wal_path, entries.clone())
            },
            |(_dir, mut trie, wal_path, entries)| {
                trie.insert_batch(&entries);
                trie.sync().ok();
                // Return WAL file size
                std::fs::metadata(&wal_path).map(|m| m.len()).unwrap_or(0)
            },
        );
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_individual_inserts,
    bench_batch_inserts,
    bench_wal_size_comparison,
);
criterion_main!(benches);
