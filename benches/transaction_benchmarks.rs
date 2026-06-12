//! Benchmarks for per-document transaction operations
//!
//! Tests the performance of:
//! - Transaction commit vs abort overhead
//! - Single-term vs batch transaction performance
//! - Memory overhead during transaction buffering

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use libdictenstein::persistent_artrie::PersistentARTrie;
use tempfile::tempdir;

/// Benchmark: Transaction commit performance
fn bench_transaction_commit(c: &mut Criterion) {
    let mut group = c.benchmark_group("transaction_commit");

    for &count in &[10, 100, 1000, 5000] {
        group.throughput(Throughput::Elements(count as u64));

        group.bench_with_input(BenchmarkId::new("commit", count), &count, |b, &count| {
            let dir = tempdir().expect("create temp dir");
            let path = dir.path().join("bench.artrie");
            let trie: PersistentARTrie<i64> = PersistentARTrie::create(&path).expect("create");

            // Pre-generate terms
            let terms: Vec<String> = (0..count).map(|i| format!("term_{:08}", i)).collect();

            b.iter(|| {
                let mut tx = trie.begin_document("bench_doc").expect("begin");
                for (i, term) in terms.iter().enumerate() {
                    trie.tx_insert(&mut tx, term, Some(i as i64));
                }
                let result = trie.commit_document(tx).expect("commit");
                black_box(result)
            });
        });
    }

    group.finish();
}

/// Benchmark: Transaction abort performance
fn bench_transaction_abort(c: &mut Criterion) {
    let mut group = c.benchmark_group("transaction_abort");

    for &count in &[10, 100, 1000, 5000] {
        group.throughput(Throughput::Elements(count as u64));

        group.bench_with_input(BenchmarkId::new("abort", count), &count, |b, &count| {
            let dir = tempdir().expect("create temp dir");
            let path = dir.path().join("bench.artrie");
            let trie: PersistentARTrie<i64> = PersistentARTrie::create(&path).expect("create");

            // Pre-generate terms
            let terms: Vec<String> = (0..count).map(|i| format!("term_{:08}", i)).collect();

            b.iter(|| {
                let mut tx = trie.begin_document("bench_doc").expect("begin");
                for (i, term) in terms.iter().enumerate() {
                    trie.tx_insert(&mut tx, term, Some(i as i64));
                }
                trie.abort_document(tx).expect("abort");
            });
        });
    }

    group.finish();
}

/// Benchmark: Compare commit vs abort overhead
fn bench_commit_vs_abort(c: &mut Criterion) {
    let mut group = c.benchmark_group("commit_vs_abort");

    let count = 1000;
    group.throughput(Throughput::Elements(count as u64));

    // Pre-generate terms
    let terms: Vec<String> = (0..count).map(|i| format!("term_{:08}", i)).collect();

    group.bench_function("commit_1000", |b| {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("bench.artrie");
        let trie: PersistentARTrie<i64> = PersistentARTrie::create(&path).expect("create");

        b.iter(|| {
            let mut tx = trie.begin_document("bench_doc").expect("begin");
            for (i, term) in terms.iter().enumerate() {
                trie.tx_insert(&mut tx, term, Some(i as i64));
            }
            let result = trie.commit_document(tx).expect("commit");
            black_box(result)
        });
    });

    group.bench_function("abort_1000", |b| {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("bench.artrie");
        let trie: PersistentARTrie<i64> = PersistentARTrie::create(&path).expect("create");

        b.iter(|| {
            let mut tx = trie.begin_document("bench_doc").expect("begin");
            for (i, term) in terms.iter().enumerate() {
                trie.tx_insert(&mut tx, term, Some(i as i64));
            }
            trie.abort_document(tx).expect("abort");
        });
    });

    group.finish();
}

/// Benchmark: Transaction overhead vs direct insert_batch
fn bench_transaction_vs_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("transaction_vs_batch");

    let count = 1000;
    group.throughput(Throughput::Elements(count as u64));

    // Pre-generate terms
    let terms: Vec<(String, Option<i64>)> = (0..count)
        .map(|i| (format!("term_{:08}", i), Some(i as i64)))
        .collect();

    group.bench_function("transaction_commit", |b| {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("bench.artrie");
        let trie: PersistentARTrie<i64> = PersistentARTrie::create(&path).expect("create");

        b.iter(|| {
            let mut tx = trie.begin_document("bench_doc").expect("begin");
            for (term, value) in terms.iter() {
                trie.tx_insert(&mut tx, term, value.clone());
            }
            let result = trie.commit_document(tx).expect("commit");
            black_box(result)
        });
    });

    group.bench_function("direct_insert_batch", |b| {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("bench.artrie");
        let trie: PersistentARTrie<i64> = PersistentARTrie::create(&path).expect("create");

        b.iter(|| {
            let result = trie.insert_batch(&terms);
            black_box(result)
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_transaction_commit,
    bench_transaction_abort,
    bench_commit_vs_abort,
    bench_transaction_vs_batch,
);

criterion_main!(benches);
