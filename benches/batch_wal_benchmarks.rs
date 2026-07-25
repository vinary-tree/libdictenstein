//! Benchmarks comparing individual vs batch WAL insert performance.
//!
//! These benchmarks measure:
//! 1. Individual insert performance (current approach)
//! 2. Batch insert performance (new approach)
//! 3. WAL size comparison to verify header overhead reduction

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::hint::black_box;
use std::time::Duration;
use tempfile::tempdir;

use libdictenstein::persistent_artrie::{PersistentARTrie, WalRecord};

const WAL_RECORD_HEADER_SIZE: usize = 17;

/// Generate test terms for benchmarking
fn generate_terms(count: usize) -> Vec<String> {
    (0..count).map(|i| format!("term_{:08}", i)).collect()
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
                |(_dir, trie, terms)| {
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
                |(_dir, trie, entries)| {
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
    let entries: Vec<(String, Option<()>)> = terms.iter().map(|t| (t.clone(), None)).collect();

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
            |(_dir, trie, wal_path, terms)| {
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
            |(_dir, trie, wal_path, entries)| {
                trie.insert_batch(&entries);
                trie.sync().ok();
                // Return WAL file size
                std::fs::metadata(&wal_path).map(|m| m.len()).unwrap_or(0)
            },
        );
    });

    group.finish();
}

/// Benchmark zero-allocation WAL payload sizing against the allocating fallback.
fn bench_wal_serialized_size(c: &mut Criterion) {
    let mut group = c.benchmark_group("wal_serialized_size");
    group.measurement_time(Duration::from_secs(5));
    group.sample_size(50);

    let records = vec![
        WalRecord::Insert {
            term: b"alpha".to_vec(),
            value: Some(vec![1, 2, 3, 4]),
        },
        WalRecord::Remove {
            term: b"obsolete".to_vec(),
        },
        WalRecord::Increment {
            term: b"counter".to_vec(),
            delta: 5,
            result: 42,
        },
        WalRecord::CompareAndSwap {
            term: b"cas-key".to_vec(),
            expected: Some(b"old".to_vec()),
            new_value: b"new-value".to_vec(),
            success: true,
        },
        WalRecord::BatchInsert {
            entries: (0..64)
                .map(|idx| {
                    (
                        format!("batch_{idx:04}").into_bytes(),
                        Some(vec![idx as u8]),
                    )
                })
                .collect(),
        },
        WalRecord::BatchIncrement {
            entries: (0..64)
                .map(|idx| (format!("counter_{idx:04}").into_bytes(), idx as i64))
                .collect(),
        },
        WalRecord::VersionUpdate {
            version_id: 1,
            root_ptr: 2,
            node_count: 3,
            timestamp: 4,
        },
        WalRecord::VersionGc {
            version_ids: (0..64).collect(),
        },
        WalRecord::CommitRank {
            data_lsn: 9,
            term: b"ranked-key".to_vec(),
            generation: 10,
        },
    ];

    group.bench_function("serialized_size_method", |b| {
        b.iter(|| {
            let mut total = 0usize;
            for record in &records {
                total += black_box(record.serialized_size());
            }
            black_box(total)
        });
    });

    group.bench_function("serialize_payload_then_len", |b| {
        b.iter(|| {
            let mut total = 0usize;
            for record in &records {
                total += WAL_RECORD_HEADER_SIZE + black_box(record.serialize_payload()).len();
            }
            black_box(total)
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_individual_inserts,
    bench_batch_inserts,
    bench_wal_size_comparison,
    bench_wal_serialized_size,
);
criterion_main!(benches);
