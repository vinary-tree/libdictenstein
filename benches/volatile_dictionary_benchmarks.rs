//! Benchmarks for volatile in-memory dictionary backends.
//!
//! This suite is deliberately separate from the persistent ARTrie benchmarks so
//! lock-free snapshot publication, static trie representations, suffix indexes,
//! and zipper-facing traversal costs can be measured without disk or WAL noise.
//!
//! Run with:
//!
//! ```text
//! cargo bench --bench volatile_dictionary_benchmarks
//! cargo bench --bench volatile_dictionary_benchmarks --features pathmap-backend
//! ```

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use libdictenstein::double_array_trie::{DoubleArrayTrie, DoubleArrayTrieChar};
use libdictenstein::dynamic_dawg::{DynamicDawg, DynamicDawgChar};
use libdictenstein::factory::{DictionaryBackend, DictionaryContainer, DictionaryFactory};
use libdictenstein::scdawg::{Scdawg, ScdawgChar};
use libdictenstein::suffix_automaton::{SuffixAutomaton, SuffixAutomatonChar};
use libdictenstein::{Dictionary, DictionaryNode};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

const SIZES: &[usize] = &[256, 4_096, 16_384];
const QUERY_MULTIPLIER: usize = 4;
const CONTENTION_SEED_TERMS: usize = 2_048;
const CONTENTION_NEW_TERMS: usize = 512;
const CONTENTION_READERS: usize = 6;
const CONTENTION_QUERIES_PER_READER: usize = 8_192;

fn byte_backends() -> Vec<DictionaryBackend> {
    let mut backends = Vec::with_capacity(7);
    #[cfg(feature = "pathmap-backend")]
    backends.push(DictionaryBackend::PathMap);
    backends.extend([
        DictionaryBackend::DoubleArrayTrie,
        DictionaryBackend::DynamicDawg,
        DictionaryBackend::DynamicDawgU64,
        DictionaryBackend::SuffixAutomaton,
        DictionaryBackend::Scdawg,
    ]);
    backends
}

fn unicode_backends() -> Vec<DictionaryBackend> {
    let mut backends = Vec::with_capacity(6);
    #[cfg(feature = "pathmap-backend")]
    backends.push(DictionaryBackend::PathMapChar);
    backends.extend([
        DictionaryBackend::DoubleArrayTrieChar,
        DictionaryBackend::DynamicDawgChar,
        DictionaryBackend::SuffixAutomatonChar,
        DictionaryBackend::ScdawgChar,
    ]);
    backends
}

fn generate_byte_terms(size: usize) -> Vec<String> {
    let prefixes = [
        "pre", "post", "anti", "hyper", "micro", "macro", "geo", "bio", "lex", "morph",
    ];
    let roots = [
        "graph", "token", "term", "state", "edge", "node", "query", "cache", "index", "value",
    ];
    let suffixes = [
        "ing", "ed", "er", "able", "less", "ful", "tion", "ment", "wise", "scope",
    ];

    let mut terms = Vec::with_capacity(size);
    let mut i = 0usize;
    while terms.len() < size {
        let prefix = prefixes[i % prefixes.len()];
        let root = roots[(i / prefixes.len()) % roots.len()];
        let suffix = suffixes[(i / (prefixes.len() * roots.len())) % suffixes.len()];
        terms.push(format!("{prefix}_{root}_{suffix}_{i:08x}"));
        i += 1;
    }
    terms
}

fn generate_unicode_terms(size: usize) -> Vec<String> {
    let scripts = [
        "cafe",
        "naive",
        "résumé",
        "東京",
        "mañana",
        "θήτα",
        "данные",
        "مفتاح",
    ];
    let domains = [
        "graph",
        "辞書",
        "automate",
        "ключ",
        "indice",
        "δοκιμή",
        "بحث",
        "値",
    ];
    let mut terms = Vec::with_capacity(size);
    for i in 0..size {
        let left = scripts[i % scripts.len()];
        let right = domains[(i / scripts.len()) % domains.len()];
        terms.push(format!("{left}-{right}-{i:08x}"));
    }
    terms
}

fn generate_queries(terms: &[String], count: usize) -> Vec<String> {
    let mut queries = Vec::with_capacity(count);
    for i in 0..count {
        if i % 4 == 0 {
            queries.push(format!("{}-miss", terms[i % terms.len()]));
        } else {
            queries.push(terms[i % terms.len()].clone());
        }
    }
    queries
}

fn count_node_terms<N: DictionaryNode>(root: N) -> usize {
    let mut count = 0usize;
    let mut stack = Vec::with_capacity(128);
    stack.push(root);

    while let Some(node) = stack.pop() {
        if node.is_final() {
            count += 1;
        }
        stack.extend(node.edges().map(|(_, child)| child));
    }

    count
}

fn count_dictionary_terms<D: Dictionary>(dict: &D) -> usize {
    count_node_terms(dict.root())
}

fn count_container_terms(container: &DictionaryContainer) -> usize {
    match container {
        #[cfg(feature = "pathmap-backend")]
        DictionaryContainer::PathMap(dict) => count_dictionary_terms(dict),
        #[cfg(feature = "pathmap-backend")]
        DictionaryContainer::PathMapChar(dict) => count_dictionary_terms(dict),
        DictionaryContainer::DoubleArrayTrie(dict) => count_dictionary_terms(dict),
        DictionaryContainer::DoubleArrayTrieChar(dict) => count_dictionary_terms(dict),
        DictionaryContainer::DynamicDawg(dict) => count_dictionary_terms(dict),
        DictionaryContainer::DynamicDawgChar(dict) => count_dictionary_terms(dict),
        DictionaryContainer::DynamicDawgU64(dict) => count_dictionary_terms(dict),
        DictionaryContainer::SuffixAutomaton(dict) => count_dictionary_terms(dict),
        DictionaryContainer::SuffixAutomatonChar(dict) => count_dictionary_terms(dict),
        DictionaryContainer::Scdawg(dict) => count_dictionary_terms(dict),
        DictionaryContainer::ScdawgChar(dict) => count_dictionary_terms(dict),
    }
}

fn bench_construction(c: &mut Criterion) {
    let mut group = c.benchmark_group("volatile_construct");

    for size in SIZES {
        let byte_terms = generate_byte_terms(*size);
        let unicode_terms = generate_unicode_terms(*size);
        group.throughput(Throughput::Elements(*size as u64));

        for backend in byte_backends() {
            group.bench_with_input(
                BenchmarkId::new(backend.to_string(), size),
                &byte_terms,
                |b, terms| {
                    b.iter(|| {
                        let dict = DictionaryFactory::create(backend, black_box(terms.iter()));
                        black_box(dict.len());
                    });
                },
            );
        }

        for backend in unicode_backends() {
            group.bench_with_input(
                BenchmarkId::new(backend.to_string(), size),
                &unicode_terms,
                |b, terms| {
                    b.iter(|| {
                        let dict = DictionaryFactory::create(backend, black_box(terms.iter()));
                        black_box(dict.len());
                    });
                },
            );
        }
    }

    group.finish();
}

fn bench_lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("volatile_lookup");

    for size in SIZES {
        let byte_terms = generate_byte_terms(*size);
        let unicode_terms = generate_unicode_terms(*size);
        let byte_queries = generate_queries(&byte_terms, size * QUERY_MULTIPLIER);
        let unicode_queries = generate_queries(&unicode_terms, size * QUERY_MULTIPLIER);
        group.throughput(Throughput::Elements(byte_queries.len() as u64));

        for backend in byte_backends() {
            let dict = DictionaryFactory::create(backend, byte_terms.iter());
            group.bench_with_input(
                BenchmarkId::new(backend.to_string(), size),
                &byte_queries,
                |b, queries| {
                    b.iter(|| {
                        let mut found = 0usize;
                        for query in queries {
                            found += usize::from(dict.contains(black_box(query)));
                        }
                        black_box(found);
                    });
                },
            );
        }

        for backend in unicode_backends() {
            let dict = DictionaryFactory::create(backend, unicode_terms.iter());
            group.bench_with_input(
                BenchmarkId::new(backend.to_string(), size),
                &unicode_queries,
                |b, queries| {
                    b.iter(|| {
                        let mut found = 0usize;
                        for query in queries {
                            found += usize::from(dict.contains(black_box(query)));
                        }
                        black_box(found);
                    });
                },
            );
        }
    }

    group.finish();
}

fn bench_graph_traversal(c: &mut Criterion) {
    let mut group = c.benchmark_group("volatile_graph_traversal");

    for size in SIZES {
        let byte_terms = generate_byte_terms(*size);
        let unicode_terms = generate_unicode_terms(*size);
        group.throughput(Throughput::Elements(*size as u64));

        for backend in byte_backends() {
            let dict = DictionaryFactory::create(backend, byte_terms.iter());
            group.bench_function(BenchmarkId::new(backend.to_string(), size), |b| {
                b.iter(|| black_box(count_container_terms(black_box(&dict))));
            });
        }

        for backend in unicode_backends() {
            let dict = DictionaryFactory::create(backend, unicode_terms.iter());
            group.bench_function(BenchmarkId::new(backend.to_string(), size), |b| {
                b.iter(|| black_box(count_container_terms(black_box(&dict))));
            });
        }
    }

    group.finish();
}

fn run_dynamic_contention_round() -> usize {
    let seed_terms = generate_byte_terms(CONTENTION_SEED_TERMS);
    let new_terms = Arc::new(generate_byte_terms(CONTENTION_NEW_TERMS));
    let queries = Arc::new(generate_queries(
        &seed_terms,
        CONTENTION_READERS * CONTENTION_QUERIES_PER_READER,
    ));
    let dict = Arc::new(DynamicDawg::<()>::from_terms(seed_terms.iter()));
    let start = Arc::new(Barrier::new(CONTENTION_READERS + 1));
    let mut handles = Vec::with_capacity(CONTENTION_READERS);

    for reader_id in 0..CONTENTION_READERS {
        let dict = Arc::clone(&dict);
        let queries = Arc::clone(&queries);
        let start = Arc::clone(&start);
        handles.push(thread::spawn(move || {
            start.wait();
            let offset = reader_id * CONTENTION_QUERIES_PER_READER;
            let mut found = 0usize;
            for query in &queries[offset..offset + CONTENTION_QUERIES_PER_READER] {
                found += usize::from(dict.contains(black_box(query)));
            }
            found
        }));
    }

    start.wait();
    for term in new_terms.iter() {
        dict.insert(term);
    }

    let mut found = 0usize;
    for handle in handles {
        found += handle.join().expect("reader thread must finish");
    }
    found + dict.len().unwrap_or_default()
}

fn run_dynamic_char_contention_round() -> usize {
    let seed_terms = generate_unicode_terms(CONTENTION_SEED_TERMS);
    let new_terms = Arc::new(generate_unicode_terms(CONTENTION_NEW_TERMS));
    let queries = Arc::new(generate_queries(
        &seed_terms,
        CONTENTION_READERS * CONTENTION_QUERIES_PER_READER,
    ));
    let dict = Arc::new(DynamicDawgChar::<()>::from_terms(seed_terms.iter()));
    let start = Arc::new(Barrier::new(CONTENTION_READERS + 1));
    let mut handles = Vec::with_capacity(CONTENTION_READERS);

    for reader_id in 0..CONTENTION_READERS {
        let dict = Arc::clone(&dict);
        let queries = Arc::clone(&queries);
        let start = Arc::clone(&start);
        handles.push(thread::spawn(move || {
            start.wait();
            let offset = reader_id * CONTENTION_QUERIES_PER_READER;
            let mut found = 0usize;
            for query in &queries[offset..offset + CONTENTION_QUERIES_PER_READER] {
                found += usize::from(dict.contains(black_box(query)));
            }
            found
        }));
    }

    start.wait();
    for term in new_terms.iter() {
        dict.insert(term);
    }

    let mut found = 0usize;
    for handle in handles {
        found += handle.join().expect("reader thread must finish");
    }
    found + dict.len().unwrap_or_default()
}

fn bench_lockfree_contention(c: &mut Criterion) {
    let mut group = c.benchmark_group("volatile_lockfree_contention");
    group.measurement_time(Duration::from_secs(8));
    group.throughput(Throughput::Elements(
        (CONTENTION_READERS * CONTENTION_QUERIES_PER_READER + CONTENTION_NEW_TERMS) as u64,
    ));

    group.bench_function("DynamicDawg/readers_plus_writer", |b| {
        b.iter_custom(|iters| {
            let mut elapsed = Duration::ZERO;
            for _ in 0..iters {
                let start = Instant::now();
                black_box(run_dynamic_contention_round());
                elapsed += start.elapsed();
            }
            elapsed
        });
    });

    group.bench_function("DynamicDawgChar/readers_plus_writer", |b| {
        b.iter_custom(|iters| {
            let mut elapsed = Duration::ZERO;
            for _ in 0..iters {
                let start = Instant::now();
                black_box(run_dynamic_char_contention_round());
                elapsed += start.elapsed();
            }
            elapsed
        });
    });

    group.finish();
}

fn bench_static_backend_memory_surrogates(c: &mut Criterion) {
    let mut group = c.benchmark_group("volatile_static_memory_surrogates");

    for size in SIZES {
        let byte_terms = generate_byte_terms(*size);
        let unicode_terms = generate_unicode_terms(*size);
        group.throughput(Throughput::Elements(*size as u64));

        group.bench_function(BenchmarkId::new("DoubleArrayTrie/states", size), |b| {
            b.iter(|| {
                let dict = DoubleArrayTrie::<()>::from_terms(black_box(byte_terms.iter()));
                black_box((dict.len(), count_dictionary_terms(&dict)));
            });
        });
        group.bench_function(BenchmarkId::new("DoubleArrayTrieChar/states", size), |b| {
            b.iter(|| {
                let dict = DoubleArrayTrieChar::<()>::from_terms(black_box(unicode_terms.iter()));
                black_box((dict.len(), count_dictionary_terms(&dict)));
            });
        });
        group.bench_function(BenchmarkId::new("SuffixAutomaton/states", size), |b| {
            b.iter(|| {
                let dict = SuffixAutomaton::<()>::from_texts(black_box(byte_terms.iter()));
                black_box((dict.len(), count_dictionary_terms(&dict)));
            });
        });
        group.bench_function(BenchmarkId::new("SuffixAutomatonChar/states", size), |b| {
            b.iter(|| {
                let dict = SuffixAutomatonChar::<()>::from_texts(black_box(unicode_terms.iter()));
                black_box((dict.len(), count_dictionary_terms(&dict)));
            });
        });
        group.bench_function(BenchmarkId::new("Scdawg/states", size), |b| {
            b.iter(|| {
                let dict = Scdawg::<()>::from_terms(black_box(byte_terms.iter()));
                black_box((dict.len(), count_dictionary_terms(&dict)));
            });
        });
        group.bench_function(BenchmarkId::new("ScdawgChar/states", size), |b| {
            b.iter(|| {
                let dict = ScdawgChar::<()>::from_terms(black_box(unicode_terms.iter()));
                black_box((dict.len(), count_dictionary_terms(&dict)));
            });
        });
    }

    group.finish();
}

criterion_group!(
    volatile_dictionary_benches,
    bench_construction,
    bench_lookup,
    bench_graph_traversal,
    bench_lockfree_contention,
    bench_static_backend_memory_surrogates,
);
criterion_main!(volatile_dictionary_benches);
