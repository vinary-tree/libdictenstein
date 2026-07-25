//! Benchmarks for PersistentARTrieChar (Persistent Adaptive Radix Trie - Character Level)
//!
//! This benchmark suite evaluates PersistentARTrieChar performance for:
//! - Unicode term construction/insertion throughput
//! - Exact lookup throughput with Unicode terms
//! - Edge traversal at character level
//! - Optimistic read performance
//! - Disk I/O with Unicode data
//!
//! Run with: cargo bench --bench persistent_artrie_char_benchmarks --features persistent-artrie

use criterion::{BenchmarkId, Criterion, Throughput};
use libdictenstein::{persistent_artrie::char::PersistentARTrieChar, DictionaryNode};
use rand::distr::{weighted::WeightedIndex, Distribution};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::collections::HashSet;
use std::hint::{black_box, black_box as bb};
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
const FIXED_SEED: u64 = 0x5041_5254_4348_4152;

/// Generate realistic Unicode dictionary terms for benchmarking
fn generate_unicode_terms(size: usize) -> Vec<String> {
    let mut terms = Vec::with_capacity(size);

    // English prefixes and suffixes
    let en_prefixes = ["pre", "un", "re", "dis", "over", "anti", "auto"];
    let en_roots = ["test", "code", "data", "work", "play", "read", "write"];
    let en_suffixes = ["ing", "ed", "er", "ly", "ness", "ment", "tion"];

    // Greek letters (common in technical text)
    let greek = ["αλφα", "βητα", "γαμμα", "δελτα", "σιγμα", "ωμεγα"];

    // Japanese words (hiragana/katakana mix)
    let japanese = ["にほん", "コンピュータ", "データ", "プログラム", "テスト"];

    // Chinese words
    let chinese = ["数据", "程序", "测试", "代码", "编程", "算法"];

    // Emoji sequences
    let emoji = ["🚀", "⭐", "🎉", "💡", "🔥", "✨"];

    for i in 0..size {
        let term = match i % 6 {
            0 => {
                // English word combinations
                let prefix_idx = i % en_prefixes.len();
                let root_idx = (i / en_prefixes.len()) % en_roots.len();
                let suffix_idx = (i / (en_prefixes.len() * en_roots.len())) % en_suffixes.len();
                format!(
                    "{}{}{}",
                    en_prefixes[prefix_idx], en_roots[root_idx], en_suffixes[suffix_idx]
                )
            }
            1 => {
                // Greek terms
                let idx = i % greek.len();
                format!("{}{}", greek[idx], i)
            }
            2 => {
                // Japanese terms
                let idx = i % japanese.len();
                format!("{}{}", japanese[idx], i)
            }
            3 => {
                // Chinese terms
                let idx = i % chinese.len();
                format!("{}{}", chinese[idx], i)
            }
            4 => {
                // Mixed script
                let en_idx = i % en_roots.len();
                let emoji_idx = i % emoji.len();
                format!("{}{}{}", en_roots[en_idx], emoji[emoji_idx], i)
            }
            _ => {
                // Plain ASCII for baseline comparison
                format!("term{:06}", i)
            }
        };
        terms.push(term);
    }

    terms.sort();
    terms.dedup();
    terms
}

/// Generate query terms (mix of existing and non-existing)
fn generate_queries(terms: &[String], count: usize) -> Vec<String> {
    let mut queries = Vec::with_capacity(count);

    for i in 0..count {
        if i % 2 == 0 && i / 2 < terms.len() {
            queries.push(terms[i / 2].clone());
        } else {
            // Create a "typo" by appending a character
            let base = &terms[i % terms.len()];
            queries.push(format!("{}x", base));
        }
    }

    queries
}

#[derive(Clone, Copy)]
enum CharClass {
    LatinConsonant,
    LatinVowel,
    Digit,
    Separator,
    Cjk,
    Kana,
    Greek,
}

fn sample_weighted_index(rng: &mut StdRng, weights: &[u32]) -> usize {
    WeightedIndex::new(weights)
        .expect("valid benchmark weights")
        .sample(rng)
}

fn sample_char_class(rng: &mut StdRng, previous: Option<CharClass>) -> CharClass {
    let weights = match previous {
        None => [32, 18, 3, 0, 20, 12, 8],
        Some(CharClass::LatinConsonant) => [14, 34, 3, 1, 3, 2, 2],
        Some(CharClass::LatinVowel) => [38, 10, 3, 2, 3, 2, 2],
        Some(CharClass::Digit) => [15, 12, 20, 2, 2, 1, 1],
        Some(CharClass::Separator) => [26, 14, 2, 0, 8, 5, 4],
        Some(CharClass::Cjk) => [3, 2, 1, 1, 34, 6, 1],
        Some(CharClass::Kana) => [3, 2, 1, 1, 8, 30, 1],
        Some(CharClass::Greek) => [5, 3, 1, 1, 1, 1, 30],
    };
    match sample_weighted_index(rng, &weights) {
        0 => CharClass::LatinConsonant,
        1 => CharClass::LatinVowel,
        2 => CharClass::Digit,
        3 => CharClass::Separator,
        4 => CharClass::Cjk,
        5 => CharClass::Kana,
        _ => CharClass::Greek,
    }
}

fn sample_char(rng: &mut StdRng, class: CharClass) -> char {
    const LATIN_CONSONANTS: &[char] = &[
        'b', 'c', 'd', 'f', 'g', 'h', 'j', 'k', 'l', 'm', 'n', 'p', 'q', 'r', 's', 't', 'v', 'w',
        'x', 'y', 'z',
    ];
    const LATIN_VOWELS: &[char] = &['a', 'e', 'i', 'o', 'u', 'á', 'é', 'í', 'ó', 'ú', 'ü'];
    const DIGITS: &[char] = &['0', '1', '2', '3', '4', '5', '6', '7', '8', '9'];
    const CJK: &[char] = &[
        '数', '据', '结', '构', '算', '法', '模', '型', '索', '引', '検', '索', '辞', '書', '言',
        '語', '東', '京', '京', '都',
    ];
    const KANA: &[char] = &[
        'ア', 'イ', 'ウ', 'エ', 'オ', 'カ', 'キ', 'ク', 'ケ', 'コ', 'サ', 'シ', 'ス', 'セ', 'ソ',
        'ン', 'ー',
    ];
    const GREEK: &[char] = &[
        'α', 'β', 'γ', 'δ', 'ε', 'η', 'θ', 'ι', 'κ', 'λ', 'μ', 'ν', 'ο', 'π', 'ρ', 'σ', 'τ',
    ];
    match class {
        CharClass::LatinConsonant => LATIN_CONSONANTS[rng.random_range(0..LATIN_CONSONANTS.len())],
        CharClass::LatinVowel => LATIN_VOWELS[rng.random_range(0..LATIN_VOWELS.len())],
        CharClass::Digit => DIGITS[rng.random_range(0..DIGITS.len())],
        CharClass::Separator => ['-', '_'][rng.random_range(0..2)],
        CharClass::Cjk => CJK[rng.random_range(0..CJK.len())],
        CharClass::Kana => KANA[rng.random_range(0..KANA.len())],
        CharClass::Greek => GREEK[rng.random_range(0..GREEK.len())],
    }
}

fn sample_char_term(rng: &mut StdRng) -> String {
    let lengths = [3usize, 4, 5, 6, 7, 8, 10, 12, 16];
    let weights = [4, 9, 13, 16, 15, 12, 8, 4, 1];
    let len = lengths[sample_weighted_index(rng, &weights)];
    let mut out = String::new();
    let mut previous = None;
    for index in 0..len {
        let mut class = sample_char_class(rng, previous);
        if (index == 0 || index + 1 == len) && matches!(class, CharClass::Separator) {
            class = CharClass::LatinConsonant;
        }
        out.push(sample_char(rng, class));
        previous = Some(class);
    }
    out
}

fn generate_statistical_char_terms(size: usize) -> Vec<String> {
    let mut rng = StdRng::seed_from_u64(FIXED_SEED);
    let mut terms = Vec::with_capacity(size);
    let mut seen = HashSet::with_capacity(size * 2);
    while terms.len() < size {
        let term = sample_char_term(&mut rng);
        if seen.insert(term.clone()) {
            terms.push(term);
        }
    }
    terms
}

fn generate_statistical_char_queries(terms: &[String], count: usize) -> Vec<String> {
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

fn build_fixed_char_trie(terms: &[String]) -> PersistentARTrieChar<()> {
    let dict = PersistentARTrieChar::new();
    for term in terms {
        dict.insert(term).expect("insert fixed char term");
    }
    dict
}

fn lookup_fixed_char(dict: &PersistentARTrieChar<()>, queries: &[String]) -> usize {
    let mut found = 0usize;
    for query in queries {
        if dict.contains(bb(query)) {
            found += 1;
        }
    }
    black_box(found)
}

fn time_lookup_sample(terms: &[String], queries: &[String]) -> Duration {
    let dict = build_fixed_char_trie(terms);
    let start = Instant::now();
    lookup_fixed_char(&dict, queries);
    start.elapsed()
}

fn parallel_read_write_sample(readers: usize, terms: &[String]) -> Duration {
    let dict = Arc::new(build_fixed_char_trie(&terms[..FIXED_PARALLEL_KEYS / 2]));
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
    let terms = generate_statistical_char_terms(FIXED_LOOKUP_SIZE);
    let queries = generate_statistical_char_queries(&terms, FIXED_QUERY_COUNT);

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

/// Benchmark PersistentARTrieChar construction via insertions
fn bench_char_construction(c: &mut Criterion) {
    let mut group = c.benchmark_group("char_construction");
    group.sample_size(20);

    for size in [100, 500, 1000, 5000].iter() {
        let terms = generate_unicode_terms(*size);

        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(
            BenchmarkId::new("persistent_artrie_char", size),
            size,
            |b, _| {
                b.iter(|| {
                    let dict: PersistentARTrieChar<()> = PersistentARTrieChar::new();
                    for term in &terms {
                        dict.insert(bb(term)).expect("insert failed");
                    }
                    black_box(dict)
                });
            },
        );
    }
    group.finish();
}

/// Benchmark construction with pure ASCII (for comparison)
fn bench_char_construction_ascii(c: &mut Criterion) {
    let mut group = c.benchmark_group("char_construction_ascii");
    group.sample_size(20);

    for size in [100, 500, 1000, 5000].iter() {
        // Generate ASCII-only terms
        let terms: Vec<String> = (0..*size).map(|i| format!("term{:06}", i)).collect();

        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(
            BenchmarkId::new("persistent_artrie_char_ascii", size),
            size,
            |b, _| {
                b.iter(|| {
                    let dict: PersistentARTrieChar<()> = PersistentARTrieChar::new();
                    for term in &terms {
                        dict.insert(bb(term)).expect("insert failed");
                    }
                    black_box(dict)
                });
            },
        );
    }
    group.finish();
}

// ============================================================================
// Lookup Benchmarks
// ============================================================================

/// Benchmark PersistentARTrieChar exact lookup with Unicode
fn bench_char_lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("char_lookup");
    group.sample_size(50);

    for size in [100, 1000, 5000].iter() {
        let terms = generate_unicode_terms(*size);
        let queries = generate_queries(&terms, 100);

        let dict: PersistentARTrieChar<()> = PersistentARTrieChar::new();
        for term in &terms {
            dict.insert(term).expect("insert failed");
        }

        group.throughput(Throughput::Elements(100));
        group.bench_with_input(
            BenchmarkId::new("persistent_artrie_char", size),
            size,
            |b, _| {
                b.iter(|| {
                    let mut found = 0;
                    for query in &queries {
                        if dict.contains(bb(query)) {
                            found += 1;
                        }
                    }
                    black_box(found)
                });
            },
        );
    }
    group.finish();
}

/// Benchmark lookup with CJK characters (multibyte)
fn bench_char_lookup_cjk(c: &mut Criterion) {
    let mut group = c.benchmark_group("char_lookup_cjk");
    group.sample_size(50);

    // Generate CJK-heavy terms
    let chinese_chars: Vec<char> = "数据结构算法程序代码测试编程开发".chars().collect();
    let terms: Vec<String> = (0..1000)
        .map(|i| {
            let mut s = String::new();
            for j in 0..5 {
                s.push(chinese_chars[(i + j) % chinese_chars.len()]);
            }
            s
        })
        .collect();

    let queries: Vec<String> = terms.iter().take(100).cloned().collect();

    let dict: PersistentARTrieChar<()> = PersistentARTrieChar::new();
    for term in &terms {
        dict.insert(term).expect("insert failed");
    }

    group.throughput(Throughput::Elements(100));
    group.bench_function("cjk_lookup", |b| {
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
    group.finish();
}

// ============================================================================
// Edge Traversal Benchmarks (critical for Levenshtein automata)
// ============================================================================

/// Benchmark PersistentARTrieChar edge traversal
fn bench_char_edge_traversal(c: &mut Criterion) {
    let mut group = c.benchmark_group("char_edge_traversal");
    group.sample_size(50);

    for size in [100, 1000, 5000].iter() {
        let terms = generate_unicode_terms(*size);

        let dict: PersistentARTrieChar<()> = PersistentARTrieChar::new();
        for term in &terms {
            dict.insert(term).expect("insert failed");
        }

        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(
            BenchmarkId::new("persistent_artrie_char", size),
            size,
            |b, _| {
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
            },
        );
    }
    group.finish();
}

// ============================================================================
// Node Transition Benchmarks (character-level lookup)
// ============================================================================

/// Benchmark PersistentARTrieChar single character transitions
fn bench_char_transitions(c: &mut Criterion) {
    let mut group = c.benchmark_group("char_transitions");
    group.sample_size(100);

    for size in [100, 1000, 5000].iter() {
        let terms = generate_unicode_terms(*size);
        let queries: Vec<_> = terms.iter().take(100).collect();

        let dict: PersistentARTrieChar<()> = PersistentARTrieChar::new();
        for term in &terms {
            dict.insert(term).expect("insert failed");
        }

        group.throughput(Throughput::Elements(100));
        group.bench_with_input(
            BenchmarkId::new("persistent_artrie_char", size),
            size,
            |b, _| {
                b.iter(|| {
                    let mut transitions = 0usize;
                    for query in &queries {
                        let mut node = dict.root();
                        for ch in query.chars() {
                            if let Some(next) = node.transition(bb(ch)) {
                                node = next;
                                transitions += 1;
                            } else {
                                break;
                            }
                        }
                    }
                    black_box(transitions)
                });
            },
        );
    }
    group.finish();
}

/// Benchmark transitions with emoji (4-byte UTF-8 / supplementary plane)
fn bench_char_transitions_emoji(c: &mut Criterion) {
    let mut group = c.benchmark_group("char_transitions_emoji");
    group.sample_size(50);

    // Generate emoji sequences
    let emojis = ["🚀", "🎉", "💡", "🔥", "⭐", "✨", "🎊", "🎯", "🏆", "💻"];
    let terms: Vec<String> = (0..500)
        .map(|i| {
            let mut s = String::new();
            for j in 0..4 {
                s.push_str(emojis[(i + j) % emojis.len()]);
            }
            s
        })
        .collect();

    let queries: Vec<_> = terms.iter().take(50).collect();

    let dict: PersistentARTrieChar<()> = PersistentARTrieChar::new();
    for term in &terms {
        dict.insert(term).expect("insert failed");
    }

    group.throughput(Throughput::Elements(50));
    group.bench_function("emoji_transitions", |b| {
        b.iter(|| {
            let mut transitions = 0usize;
            for query in &queries {
                let mut node = dict.root();
                for ch in query.chars() {
                    if let Some(next) = node.transition(bb(ch)) {
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
    group.finish();
}

// ============================================================================
// Iterator Benchmarks
// ============================================================================

/// Benchmark term iteration
fn bench_char_iteration(c: &mut Criterion) {
    let mut group = c.benchmark_group("char_iteration");
    group.sample_size(30);

    for size in [100, 500, 1000].iter() {
        let terms = generate_unicode_terms(*size);

        let dict: PersistentARTrieChar<()> = PersistentARTrieChar::new();
        for term in &terms {
            dict.insert(term).expect("insert failed");
        }

        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(BenchmarkId::new("iter", size), size, |b, _| {
            b.iter(|| {
                let count = dict.iter().count();
                black_box(count)
            });
        });
    }
    group.finish();
}

// ============================================================================
// Optimistic Read Benchmarks (Phase C7 concurrency feature)
// ============================================================================

/// Benchmark optimistic contains operations
#[cfg(feature = "persistent-artrie")]
fn bench_char_optimistic_reads(c: &mut Criterion) {
    let mut group = c.benchmark_group("char_optimistic_reads");
    group.sample_size(50);

    for size in [100, 1000, 5000].iter() {
        let terms = generate_unicode_terms(*size);
        let queries = generate_queries(&terms, 100);

        let dict: PersistentARTrieChar<()> = PersistentARTrieChar::new();
        for term in &terms {
            dict.insert(term).expect("insert failed");
        }

        // Regular contains
        group.throughput(Throughput::Elements(100));
        group.bench_with_input(BenchmarkId::new("regular_contains", size), size, |b, _| {
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
// Memory Efficiency Benchmarks
// ============================================================================

/// Measure memory efficiency with Unicode terms
fn bench_char_memory_efficiency(c: &mut Criterion) {
    let mut group = c.benchmark_group("char_memory_efficiency");
    group.sample_size(10);

    for size in [1000, 5000, 10000].iter() {
        let terms = generate_unicode_terms(*size);

        group.bench_with_input(
            BenchmarkId::new("persistent_artrie_char_size", size),
            size,
            |b, _| {
                b.iter(|| {
                    let dict: PersistentARTrieChar<()> = PersistentARTrieChar::new();
                    for term in &terms {
                        dict.insert(term).expect("insert failed");
                    }
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

/// Benchmark PersistentARTrieChar with disk persistence enabled
#[cfg(feature = "persistent-artrie")]
fn bench_char_disk_io(c: &mut Criterion) {
    use std::time::Instant;
    use tempfile::tempdir;

    let mut group = c.benchmark_group("char_disk_io");
    group.sample_size(10);

    for size in [100, 500, 1000].iter() {
        let terms = generate_unicode_terms(*size);

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
                        let path = dir.path().join("bench.chartrie");

                        let start = Instant::now();
                        let dict = PersistentARTrieChar::<()>::create(&path).expect("create dict");
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
            let path = dir.path().join("bench.chartrie");
            {
                let dict = PersistentARTrieChar::<()>::create(&path).expect("create dict");
                for term in &terms {
                    let _ = dict.insert(term);
                }
                let _ = dict.sync();
            }

            b.iter_custom(|iters| {
                let mut total = std::time::Duration::ZERO;
                for _ in 0..iters {
                    let start = Instant::now();
                    let dict = PersistentARTrieChar::<()>::open(&path).expect("open dict");
                    black_box(dict.len());
                    total += start.elapsed();
                }
                total
            });
        });

        // Benchmark: Checkpoint
        group.bench_with_input(BenchmarkId::new("checkpoint", size), size, |b, _| {
            let dir = tempdir().unwrap();
            let path = dir.path().join("bench.chartrie");
            let dict = PersistentARTrieChar::<()>::create(&path).expect("create dict");
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

/// Benchmark disk I/O with CJK-heavy data
#[cfg(feature = "persistent-artrie")]
fn bench_char_disk_io_cjk(c: &mut Criterion) {
    use std::time::Instant;
    use tempfile::tempdir;

    let mut group = c.benchmark_group("char_disk_io_cjk");
    group.sample_size(10);

    // Generate CJK-heavy terms
    let chinese_chars: Vec<char> = "数据结构算法程序代码测试编程开发系统网络".chars().collect();
    let terms: Vec<String> = (0..500)
        .map(|i| {
            let mut s = String::new();
            for j in 0..6 {
                s.push(chinese_chars[(i + j) % chinese_chars.len()]);
            }
            s
        })
        .collect();

    group.throughput(Throughput::Elements(500));
    group.bench_function("cjk_create_insert_sync", |b| {
        b.iter_custom(|iters| {
            let mut total = std::time::Duration::ZERO;
            for _ in 0..iters {
                let dir = tempdir().unwrap();
                let path = dir.path().join("bench.chartrie");

                let start = Instant::now();
                let dict = PersistentARTrieChar::<()>::create(&path).expect("create dict");
                for term in &terms {
                    let _ = dict.insert(bb(term));
                }
                let _ = dict.sync();
                total += start.elapsed();
                drop(dict);
            }
            total
        });
    });
    group.finish();
}

// ============================================================================
// Atomic Operations Benchmarks
// ============================================================================

/// Benchmark atomic increment operations
#[cfg(feature = "persistent-artrie")]
fn bench_char_atomic_ops(c: &mut Criterion) {
    use tempfile::tempdir;

    let mut group = c.benchmark_group("char_atomic_ops");
    group.sample_size(30);

    // Test increment performance
    let terms: Vec<String> = (0..100).map(|i| format!("counter_{}", i)).collect();

    group.throughput(Throughput::Elements(100));
    group.bench_function("increment", |b| {
        let dir = tempdir().unwrap();
        let path = dir.path().join("atomic.chartrie");
        let mut dict = PersistentARTrieChar::<i64>::create(&path).expect("create dict");

        // Pre-populate
        for term in &terms {
            let _ = dict.increment(term, 0);
        }

        b.iter(|| {
            for term in &terms {
                let _ = dict.increment(bb(term), 1);
            }
            black_box(())
        });
    });

    group.bench_function("upsert", |b| {
        let dir = tempdir().unwrap();
        let path = dir.path().join("upsert.chartrie");
        let dict = PersistentARTrieChar::<i64>::create(&path).expect("create dict");

        b.iter(|| {
            for (i, term) in terms.iter().enumerate() {
                let _ = dict.upsert(bb(term), i as i64);
            }
            black_box(())
        });
    });

    group.finish();
}

fn run_criterion() {
    let mut criterion = Criterion::default().configure_from_args();
    bench_char_construction(&mut criterion);
    bench_char_construction_ascii(&mut criterion);
    bench_char_lookup(&mut criterion);
    bench_char_lookup_cjk(&mut criterion);
    bench_char_edge_traversal(&mut criterion);
    bench_char_transitions(&mut criterion);
    bench_char_transitions_emoji(&mut criterion);
    bench_char_iteration(&mut criterion);
    bench_char_memory_efficiency(&mut criterion);
    bench_char_optimistic_reads(&mut criterion);
    bench_char_disk_io(&mut criterion);
    bench_char_disk_io_cjk(&mut criterion);
    bench_char_atomic_ops(&mut criterion);
    criterion.final_summary();
}

fn main() {
    if std::env::var_os("PART_CHAR_FIXED_SAMPLES").is_some() {
        run_fixed_samples();
    } else {
        run_criterion();
    }
}
