#![allow(deprecated)]

//! Benchmarks for PersistentARTrie (Persistent Adaptive Radix Trie)
//!
//! This benchmark suite compares PersistentARTrie against other dictionary
//! implementations to measure:
//! - Construction/insertion throughput
//! - Exact lookup throughput
//! - Levenshtein query performance
//! - Memory and disk efficiency
//!
//! Run with: cargo bench --bench persistent_artrie_benchmarks --features persistent-artrie

use criterion::{black_box, BenchmarkId, Criterion, Throughput};
use libdictenstein::{
    double_array_trie::DoubleArrayTrie, dynamic_dawg::DynamicDawg,
    persistent_artrie::PersistentARTrie, Dictionary, DictionaryNode,
};
use rand::distr::{weighted::WeightedIndex, Distribution};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::collections::HashSet;
use std::hint::black_box as bb;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

const FIXED_SAMPLES: usize = 51;
const FIXED_WARMUPS: usize = 3;
const FIXED_LOOKUP_SIZE: usize = 8_192;
const FIXED_QUERY_COUNT: usize = 16_384;
const FIXED_PARALLEL_KEYS: usize = 8_192;
const FIXED_OPS_PER_READER: usize = 12_000;
const FIXED_WRITES_PER_SAMPLE: usize = 2_000;
const FIXED_READER_COUNT: usize = 8;
const FIXED_SEED: u64 = 0x5041_5254_4259_5445;

/// Generate realistic dictionary terms for benchmarking
fn generate_terms(size: usize) -> Vec<String> {
    let mut terms = Vec::with_capacity(size);

    // Common English prefixes and suffixes for realistic dictionary
    let prefixes = [
        "pre", "un", "re", "in", "dis", "en", "non", "over", "mis", "sub", "anti", "auto", "bio",
        "co", "counter", "de", "ex", "hyper", "inter", "multi",
    ];
    let roots = [
        "test", "code", "data", "work", "play", "read", "write", "run", "walk", "talk", "think",
        "make", "take", "give", "find", "look", "know", "want", "seem", "feel",
    ];
    let suffixes = [
        "ing", "ed", "er", "est", "ly", "ness", "ment", "tion", "able", "ful", "less", "ize",
        "ify", "ward", "wise", "ous", "ive", "al", "ary", "ory",
    ];

    // Generate realistic word combinations
    for i in 0..size {
        let prefix_idx = i % prefixes.len();
        let root_idx = (i / prefixes.len()) % roots.len();
        let suffix_idx = (i / (prefixes.len() * roots.len())) % suffixes.len();

        // Mix of word lengths
        let word = match i % 4 {
            0 => format!("{}{}", roots[root_idx], suffixes[suffix_idx]),
            1 => format!("{}{}", prefixes[prefix_idx], roots[root_idx]),
            2 => format!(
                "{}{}{}",
                prefixes[prefix_idx], roots[root_idx], suffixes[suffix_idx]
            ),
            _ => roots[root_idx].to_string(),
        };

        terms.push(word);

        // Add some numeric suffixes for variety
        if i % 10 == 0 {
            terms.push(format!("{}{}", roots[root_idx], i));
        }
    }

    terms.sort();
    terms.dedup();
    terms
}

/// Generate query terms (mix of existing and non-existing)
fn generate_queries(terms: &[String], count: usize) -> Vec<String> {
    let mut queries = Vec::with_capacity(count);

    // Half from dictionary, half are typos
    for i in 0..count {
        if i % 2 == 0 && i / 2 < terms.len() {
            queries.push(terms[i / 2].clone());
        } else {
            // Create a "typo" by appending or modifying
            let base = &terms[i % terms.len()];
            if base.len() > 2 {
                // Single character substitution
                let mut chars: Vec<char> = base.chars().collect();
                chars[1] = 'x';
                queries.push(chars.into_iter().collect());
            } else {
                queries.push(format!("{}x", base));
            }
        }
    }

    queries
}

#[derive(Clone, Copy)]
enum ByteClass {
    Consonant,
    Vowel,
    Digit,
    Separator,
}

fn sample_weighted_index(rng: &mut StdRng, weights: &[u32]) -> usize {
    WeightedIndex::new(weights)
        .expect("valid benchmark weights")
        .sample(rng)
}

fn sample_byte_class(rng: &mut StdRng, previous: Option<ByteClass>) -> ByteClass {
    let weights = match previous {
        None => [7, 4, 1, 0],
        Some(ByteClass::Consonant) => [3, 8, 1, 0],
        Some(ByteClass::Vowel) => [8, 2, 1, 1],
        Some(ByteClass::Digit) => [4, 3, 6, 1],
        Some(ByteClass::Separator) => [7, 3, 1, 0],
    };
    match sample_weighted_index(rng, &weights) {
        0 => ByteClass::Consonant,
        1 => ByteClass::Vowel,
        2 => ByteClass::Digit,
        _ => ByteClass::Separator,
    }
}

fn sample_byte_char(rng: &mut StdRng, class: ByteClass) -> char {
    const CONSONANTS: &[u8] = b"bcdfghjklmnpqrstvwxyz";
    const VOWELS: &[u8] = b"aeiou";
    const DIGITS: &[u8] = b"0123456789";
    match class {
        ByteClass::Consonant => CONSONANTS[rng.random_range(0..CONSONANTS.len())] as char,
        ByteClass::Vowel => VOWELS[rng.random_range(0..VOWELS.len())] as char,
        ByteClass::Digit => DIGITS[rng.random_range(0..DIGITS.len())] as char,
        ByteClass::Separator => ['-', '_'][rng.random_range(0..2)],
    }
}

fn sample_byte_term(rng: &mut StdRng) -> String {
    let lengths = [4usize, 5, 6, 7, 8, 9, 10, 12, 16, 20];
    let weights = [3, 7, 11, 14, 16, 15, 12, 9, 5, 2];
    let len = lengths[sample_weighted_index(rng, &weights)];
    let mut out = String::with_capacity(len);
    let mut previous = None;
    for index in 0..len {
        let mut class = sample_byte_class(rng, previous);
        if index == 0 || index + 1 == len {
            if matches!(class, ByteClass::Separator) {
                class = ByteClass::Consonant;
            }
        }
        out.push(sample_byte_char(rng, class));
        previous = Some(class);
    }
    out
}

fn generate_statistical_byte_terms(size: usize) -> Vec<String> {
    let mut rng = StdRng::seed_from_u64(FIXED_SEED);
    let mut terms = Vec::with_capacity(size);
    let mut seen = HashSet::with_capacity(size * 2);
    while terms.len() < size {
        let term = sample_byte_term(&mut rng);
        if seen.insert(term.clone()) {
            terms.push(term);
        }
    }
    terms
}

fn generate_statistical_byte_queries(terms: &[String], count: usize) -> Vec<String> {
    let mut rng = StdRng::seed_from_u64(FIXED_SEED ^ 0x5155_4552_4945_5300);
    let mut queries = Vec::with_capacity(count);
    let hot_len = (terms.len() / 10).max(1);
    for i in 0..count {
        match sample_weighted_index(&mut rng, &[70, 20, 10]) {
            0 => queries.push(terms[rng.random_range(0..hot_len)].clone()),
            1 => queries.push(terms[rng.random_range(0..terms.len())].clone()),
            _ => {
                let base = &terms[i % terms.len()];
                queries.push(format!("{base}x"));
            }
        }
    }
    queries
}

fn build_fixed_byte_trie(terms: &[String]) -> PersistentARTrie<()> {
    let dict = PersistentARTrie::new();
    for term in terms {
        let _ = dict.insert(term);
    }
    dict
}

fn lookup_fixed_byte(dict: &PersistentARTrie<()>, queries: &[String]) -> usize {
    let mut found = 0usize;
    for query in queries {
        if dict.contains(bb(query)) {
            found += 1;
        }
    }
    black_box(found)
}

fn time_lookup_sample(terms: &[String], queries: &[String]) -> Duration {
    let dict = build_fixed_byte_trie(terms);
    let start = Instant::now();
    lookup_fixed_byte(&dict, queries);
    start.elapsed()
}

fn parallel_read_write_sample(readers: usize, terms: &[String]) -> Duration {
    let dict = Arc::new(build_fixed_byte_trie(&terms[..FIXED_PARALLEL_KEYS / 2]));
    let stop = Arc::new(AtomicBool::new(false));
    let ready = Arc::new(Barrier::new(readers + 2));
    let start_gate = Arc::new(Barrier::new(readers + 2));

    let mut handles = Vec::with_capacity(readers);
    for reader in 0..readers {
        let dict = Arc::clone(&dict);
        let ready = Arc::clone(&ready);
        let start_gate = Arc::clone(&start_gate);
        let keys = terms.to_vec();
        handles.push(thread::spawn(move || {
            ready.wait();
            start_gate.wait();
            let mut hits = 0usize;
            for op in 0..FIXED_OPS_PER_READER {
                let index = op.wrapping_mul(2_654_435_761).wrapping_add(reader * 17) % keys.len();
                if dict.contains(&keys[index]) {
                    hits += 1;
                }
            }
            black_box(hits)
        }));
    }

    let writer = {
        let dict = Arc::clone(&dict);
        let ready = Arc::clone(&ready);
        let start_gate = Arc::clone(&start_gate);
        let stop = Arc::clone(&stop);
        let keys = terms.to_vec();
        thread::spawn(move || {
            ready.wait();
            start_gate.wait();
            let mut writes = 0usize;
            while !stop.load(Ordering::Relaxed) && writes < FIXED_WRITES_PER_SAMPLE {
                let index = (FIXED_PARALLEL_KEYS / 2) + (writes % (FIXED_PARALLEL_KEYS / 2));
                let _ = dict.insert(&keys[index]);
                writes += 1;
            }
            black_box(writes)
        })
    };

    ready.wait();
    let start = Instant::now();
    start_gate.wait();
    for handle in handles {
        let _ = handle.join();
    }
    let elapsed = start.elapsed();
    stop.store(true, Ordering::Relaxed);
    let _ = writer.join();
    elapsed
}

fn fixed_arm_label() -> &'static str {
    if cfg!(part_legacy_edge_store) {
        "control_legacy_edge_store"
    } else {
        "treatment_adaptive_edge_store"
    }
}

fn print_sample_line(metric: &str, unit: &str, samples: &[f64]) {
    print!(
        "metric={metric},arm={},unit={unit},samples=",
        fixed_arm_label()
    );
    for (index, sample) in samples.iter().enumerate() {
        if index > 0 {
            print!(";");
        }
        print!("{sample:.6}");
    }
    println!();
}

fn collect_samples<F>(mut f: F, divisor: f64) -> Vec<f64>
where
    F: FnMut() -> Duration,
{
    let mut samples = Vec::with_capacity(FIXED_SAMPLES);
    for round in 0..(FIXED_WARMUPS + FIXED_SAMPLES) {
        let elapsed = f();
        if round >= FIXED_WARMUPS {
            samples.push(elapsed.as_nanos() as f64 / divisor);
        }
    }
    samples
}

fn run_fixed_samples() {
    let terms = generate_statistical_byte_terms(FIXED_LOOKUP_SIZE);
    let queries = generate_statistical_byte_queries(&terms, FIXED_QUERY_COUNT);

    let lookup = collect_samples(
        || time_lookup_sample(&terms, &queries),
        FIXED_QUERY_COUNT as f64,
    );
    let parallel = collect_samples(
        || parallel_read_write_sample(FIXED_READER_COUNT, &terms),
        (FIXED_READER_COUNT * FIXED_OPS_PER_READER) as f64,
    );

    print_sample_line("lookup_ns_per_query", "ns/query", &lookup);
    print_sample_line("parallel_ns_per_read", "ns/read", &parallel);
}

// ============================================================================
// Construction Benchmarks
// ============================================================================

/// Benchmark PersistentARTrie construction via insertions
fn bench_part_construction(c: &mut Criterion) {
    let mut group = c.benchmark_group("part_construction");
    group.sample_size(20); // Fewer samples due to I/O

    for size in [100, 500, 1000, 5000].iter() {
        let terms = generate_terms(*size);

        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(BenchmarkId::new("persistent_artrie", size), size, |b, _| {
            b.iter(|| {
                let dict: PersistentARTrie<()> = PersistentARTrie::new();
                for term in &terms {
                    let _ = dict.insert(bb(term));
                }
                black_box(dict)
            });
        });
    }
    group.finish();
}

/// Benchmark DynamicDawg construction for comparison
fn bench_dynamic_dawg_construction(c: &mut Criterion) {
    let mut group = c.benchmark_group("dynamic_dawg_construction");

    for size in [100, 500, 1000, 5000].iter() {
        let terms = generate_terms(*size);

        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(BenchmarkId::new("dynamic_dawg", size), size, |b, _| {
            b.iter(|| {
                let dict = DynamicDawg::<()>::default();
                for term in &terms {
                    dict.insert(bb(term));
                }
                black_box(dict)
            });
        });
    }
    group.finish();
}

/// Benchmark DoubleArrayTrie construction for comparison
fn bench_dat_construction(c: &mut Criterion) {
    let mut group = c.benchmark_group("dat_construction");

    for size in [100, 500, 1000, 5000].iter() {
        let terms = generate_terms(*size);

        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(BenchmarkId::new("double_array_trie", size), size, |b, _| {
            b.iter(|| {
                let dict = DoubleArrayTrie::from_terms(bb(&terms));
                black_box(dict)
            });
        });
    }
    group.finish();
}

// ============================================================================
// Lookup Benchmarks
// ============================================================================

/// Benchmark PersistentARTrie exact lookup
fn bench_part_lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("part_lookup");
    group.sample_size(50);

    for size in [100, 1000, 5000].iter() {
        let terms = generate_terms(*size);
        let queries = generate_queries(&terms, 100);

        let dict: PersistentARTrie<()> = PersistentARTrie::new();
        for term in &terms {
            let _ = dict.insert(term);
        }

        group.throughput(Throughput::Elements(100));
        group.bench_with_input(BenchmarkId::new("persistent_artrie", size), size, |b, _| {
            b.iter(|| {
                let mut found = 0;
                for query in &queries {
                    if dict.contains(bb(query)) {
                        found += 1;
                    }
                }
                black_box(found)
            });
        });
    }
    group.finish();
}

/// Benchmark DynamicDawg exact lookup for comparison
fn bench_dynamic_dawg_lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("dynamic_dawg_lookup");

    for size in [100, 1000, 5000].iter() {
        let terms = generate_terms(*size);
        let queries = generate_queries(&terms, 100);

        let dict = DynamicDawg::<()>::default();
        for term in &terms {
            dict.insert(term);
        }

        group.throughput(Throughput::Elements(100));
        group.bench_with_input(BenchmarkId::new("dynamic_dawg", size), size, |b, _| {
            b.iter(|| {
                let mut found = 0;
                for query in &queries {
                    if dict.contains(bb(query)) {
                        found += 1;
                    }
                }
                black_box(found)
            });
        });
    }
    group.finish();
}

/// Benchmark DoubleArrayTrie exact lookup for comparison
fn bench_dat_lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("dat_lookup");

    for size in [100, 1000, 5000].iter() {
        let terms = generate_terms(*size);
        let queries = generate_queries(&terms, 100);

        let dict = DoubleArrayTrie::from_terms(&terms);

        group.throughput(Throughput::Elements(100));
        group.bench_with_input(BenchmarkId::new("double_array_trie", size), size, |b, _| {
            b.iter(|| {
                let mut found = 0;
                for query in &queries {
                    if dict.contains(bb(query)) {
                        found += 1;
                    }
                }
                black_box(found)
            });
        });
    }
    group.finish();
}

// ============================================================================
// Edge Traversal Benchmarks (critical for Levenshtein automata)
// ============================================================================

/// Benchmark PersistentARTrie edge traversal
fn bench_part_edge_traversal(c: &mut Criterion) {
    let mut group = c.benchmark_group("part_edge_traversal");
    group.sample_size(50);

    for size in [100, 1000, 5000].iter() {
        let terms = generate_terms(*size);

        let dict: PersistentARTrie<()> = PersistentARTrie::new();
        for term in &terms {
            let _ = dict.insert(term);
        }

        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(BenchmarkId::new("persistent_artrie", size), size, |b, _| {
            b.iter(|| {
                // DFS traversal counting all edges
                let mut count = 0usize;
                let mut stack = vec![dict.root()];
                while let Some(node) = stack.pop() {
                    for (_, child) in node.edges() {
                        count += 1;
                        stack.push(child);
                    }
                }
                black_box(count)
            });
        });
    }
    group.finish();
}

/// Benchmark DynamicDawg edge traversal for comparison
fn bench_dynamic_dawg_edge_traversal(c: &mut Criterion) {
    let mut group = c.benchmark_group("dynamic_dawg_edge_traversal");

    for size in [100, 1000, 5000].iter() {
        let terms = generate_terms(*size);

        let dict = DynamicDawg::<()>::default();
        for term in &terms {
            dict.insert(term);
        }

        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(BenchmarkId::new("dynamic_dawg", size), size, |b, _| {
            b.iter(|| {
                // DFS traversal counting all edges
                let mut count = 0usize;
                let mut stack = vec![dict.root()];
                while let Some(node) = stack.pop() {
                    for (_, child) in node.edges() {
                        count += 1;
                        stack.push(child);
                    }
                }
                black_box(count)
            });
        });
    }
    group.finish();
}

/// Benchmark DoubleArrayTrie edge traversal for comparison
fn bench_dat_edge_traversal(c: &mut Criterion) {
    let mut group = c.benchmark_group("dat_edge_traversal");

    for size in [100, 1000, 5000].iter() {
        let terms = generate_terms(*size);
        let dict = DoubleArrayTrie::from_terms(&terms);

        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(BenchmarkId::new("double_array_trie", size), size, |b, _| {
            b.iter(|| {
                // DFS traversal counting all edges
                let mut count = 0usize;
                let mut stack = vec![dict.root()];
                while let Some(node) = stack.pop() {
                    for (_, child) in node.edges() {
                        count += 1;
                        stack.push(child);
                    }
                }
                black_box(count)
            });
        });
    }
    group.finish();
}

// ============================================================================
// Node Transition Benchmarks (single character lookup)
// ============================================================================

/// Benchmark PersistentARTrie single transitions along known paths
fn bench_part_transitions(c: &mut Criterion) {
    let mut group = c.benchmark_group("part_transitions");
    group.sample_size(100);

    for size in [100, 1000, 5000].iter() {
        let terms = generate_terms(*size);
        let queries: Vec<_> = terms.iter().take(100).collect();

        let dict: PersistentARTrie<()> = PersistentARTrie::new();
        for term in &terms {
            let _ = dict.insert(term);
        }

        group.throughput(Throughput::Elements(100));
        group.bench_with_input(BenchmarkId::new("persistent_artrie", size), size, |b, _| {
            b.iter(|| {
                let mut transitions = 0usize;
                for query in &queries {
                    let mut node = dict.root();
                    for &byte in query.as_bytes() {
                        if let Some(next) = node.transition(bb(byte)) {
                            node = next;
                            transitions += 1;
                        } else {
                            break;
                        }
                    }
                }
                black_box(transitions)
            });
        });
    }
    group.finish();
}

/// Benchmark DynamicDawg single transitions for comparison
fn bench_dynamic_dawg_transitions(c: &mut Criterion) {
    let mut group = c.benchmark_group("dynamic_dawg_transitions");

    for size in [100, 1000, 5000].iter() {
        let terms = generate_terms(*size);
        let queries: Vec<_> = terms.iter().take(100).collect();

        let dict = DynamicDawg::<()>::default();
        for term in &terms {
            dict.insert(term);
        }

        group.throughput(Throughput::Elements(100));
        group.bench_with_input(BenchmarkId::new("dynamic_dawg", size), size, |b, _| {
            b.iter(|| {
                let mut transitions = 0usize;
                for query in &queries {
                    let mut node = dict.root();
                    for &byte in query.as_bytes() {
                        if let Some(next) = node.transition(bb(byte)) {
                            node = next;
                            transitions += 1;
                        } else {
                            break;
                        }
                    }
                }
                black_box(transitions)
            });
        });
    }
    group.finish();
}

/// Benchmark DoubleArrayTrie single transitions for comparison
fn bench_dat_transitions(c: &mut Criterion) {
    let mut group = c.benchmark_group("dat_transitions");

    for size in [100, 1000, 5000].iter() {
        let terms = generate_terms(*size);
        let queries: Vec<_> = terms.iter().take(100).collect();
        let dict = DoubleArrayTrie::from_terms(&terms);

        group.throughput(Throughput::Elements(100));
        group.bench_with_input(BenchmarkId::new("double_array_trie", size), size, |b, _| {
            b.iter(|| {
                let mut transitions = 0usize;
                for query in &queries {
                    let mut node = dict.root();
                    for &byte in query.as_bytes() {
                        if let Some(next) = node.transition(bb(byte)) {
                            node = next;
                            transitions += 1;
                        } else {
                            break;
                        }
                    }
                }
                black_box(transitions)
            });
        });
    }
    group.finish();
}

// ============================================================================
// Memory Layout Benchmarks
// ============================================================================

/// Measure memory efficiency of different dictionary sizes
fn bench_memory_efficiency(c: &mut Criterion) {
    let mut group = c.benchmark_group("memory_efficiency");
    group.sample_size(10);

    for size in [1000, 5000, 10000].iter() {
        let terms = generate_terms(*size);

        // PersistentARTrie
        group.bench_with_input(
            BenchmarkId::new("persistent_artrie_size", size),
            size,
            |b, _| {
                b.iter(|| {
                    let dict: PersistentARTrie<()> = PersistentARTrie::new();
                    for term in &terms {
                        let _ = dict.insert(term);
                    }
                    // Return dict to prevent optimization
                    black_box(dict.len())
                });
            },
        );

        // DynamicDawg
        group.bench_with_input(BenchmarkId::new("dynamic_dawg_size", size), size, |b, _| {
            b.iter(|| {
                let dict = DynamicDawg::<()>::default();
                for term in &terms {
                    dict.insert(term);
                }
                black_box(dict.len())
            });
        });

        // DoubleArrayTrie
        group.bench_with_input(
            BenchmarkId::new("double_array_trie_size", size),
            size,
            |b, _| {
                b.iter(|| {
                    let dict = DoubleArrayTrie::from_terms(&terms);
                    black_box(dict.len())
                });
            },
        );
    }
    group.finish();
}

// ============================================================================
// Disk I/O Benchmarks (requires persistent-artrie feature)
// ============================================================================

/// Benchmark PersistentARTrie with disk persistence enabled
fn bench_part_disk_io(c: &mut Criterion) {
    use std::time::Instant;
    use tempfile::tempdir;

    let mut group = c.benchmark_group("part_disk_io");
    group.sample_size(10); // Fewer samples due to I/O

    for size in [100, 500, 1000].iter() {
        let terms = generate_terms(*size);

        // Benchmark: Create + Insert + Sync
        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(
            BenchmarkId::new("create_insert_sync", size),
            size,
            |b, _| {
                b.iter_custom(|iters| {
                    let mut total = std::time::Duration::ZERO;
                    for _ in 0..iters {
                        let dir = tempdir().unwrap();
                        let path = dir.path().join("bench.part");

                        let start = Instant::now();
                        let dict = PersistentARTrie::<()>::create(&path).unwrap();
                        for term in &terms {
                            let _ = dict.insert(bb(term));
                        }
                        let _ = dict.sync();
                        total += start.elapsed();
                        drop(dict);
                    }
                    total
                });
            },
        );

        // Benchmark: Recovery time
        group.bench_with_input(BenchmarkId::new("recovery", size), size, |b, _| {
            // Setup: create and populate dictionary
            let dir = tempdir().unwrap();
            let path = dir.path().join("bench.part");
            {
                let dict = PersistentARTrie::<()>::create(&path).unwrap();
                for term in &terms {
                    let _ = dict.insert(term);
                }
                let _ = dict.sync();
            }

            b.iter_custom(|iters| {
                let mut total = std::time::Duration::ZERO;
                for _ in 0..iters {
                    let start = Instant::now();
                    let dict = PersistentARTrie::<()>::open(&path).unwrap();
                    black_box(dict.len());
                    total += start.elapsed();
                }
                total
            });
        });

        // Benchmark: Checkpoint
        group.bench_with_input(BenchmarkId::new("checkpoint", size), size, |b, _| {
            let dir = tempdir().unwrap();
            let path = dir.path().join("bench.part");
            let dict = PersistentARTrie::<()>::create(&path).unwrap();
            for term in &terms {
                let _ = dict.insert(term);
            }

            b.iter(|| {
                let _ = dict.checkpoint();
                black_box(())
            });
        });
    }
    group.finish();
}

fn run_criterion() {
    let mut criterion = Criterion::default().configure_from_args();
    bench_part_construction(&mut criterion);
    bench_dynamic_dawg_construction(&mut criterion);
    bench_dat_construction(&mut criterion);
    bench_part_lookup(&mut criterion);
    bench_dynamic_dawg_lookup(&mut criterion);
    bench_dat_lookup(&mut criterion);
    bench_part_edge_traversal(&mut criterion);
    bench_dynamic_dawg_edge_traversal(&mut criterion);
    bench_dat_edge_traversal(&mut criterion);
    bench_part_transitions(&mut criterion);
    bench_dynamic_dawg_transitions(&mut criterion);
    bench_dat_transitions(&mut criterion);
    bench_memory_efficiency(&mut criterion);
    bench_part_disk_io(&mut criterion);
    criterion.final_summary();
}

fn main() {
    if std::env::var_os("PART_BYTE_FIXED_SAMPLES").is_some() {
        run_fixed_samples();
    } else {
        run_criterion();
    }
}
