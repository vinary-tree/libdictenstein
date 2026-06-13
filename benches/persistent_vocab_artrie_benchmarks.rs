#![cfg(feature = "persistent-artrie")]

//! Fixed-sample and Criterion benchmarks for `PersistentVocabARTrie`.
//!
//! Fixed-sample mode:
//!
//! ```bash
//! PART_VOCAB_FIXED_SAMPLES=1 cargo bench --bench persistent_vocab_artrie_benchmarks --features persistent-artrie
//! ```

use criterion::{black_box, BenchmarkId, Criterion, Throughput};
use libdictenstein::persistent_artrie::vocab::PersistentVocabARTrie;
use rand::distributions::{Distribution, WeightedIndex};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

const FIXED_SAMPLES: usize = 51;
const FIXED_WARMUPS: usize = 3;
const FIXED_LOOKUP_SIZE: usize = 8_192;
const FIXED_QUERY_COUNT: usize = 16_384;
const FIXED_PARALLEL_KEYS: usize = 8_192;
const FIXED_OPS_PER_READER: usize = 12_000;
const FIXED_WRITES_PER_SAMPLE: usize = 2_000;
const FIXED_READER_COUNT: usize = 8;
const FIXED_SEED: u64 = 0x5041_5254_564F_4341;

fn generate_vocab_terms(size: usize) -> Vec<String> {
    if let Some(terms) = load_explicit_corpus_terms(size) {
        return terms;
    }
    eprintln!("persistent_vocab_corpus,source=seeded_markov,seed={FIXED_SEED},terms={size}");
    generate_seeded_vocab_terms(size)
}

fn load_explicit_corpus_terms(size: usize) -> Option<Vec<String>> {
    let path = std::env::var("PART_VOCAB_CORPUS").ok().map(PathBuf::from)?;
    if !path.exists() {
        return None;
    }
    if let Some(terms) = load_terms_from_file(&path, size) {
        eprintln!(
            "persistent_vocab_corpus,source={},terms={}",
            path.display(),
            terms.len()
        );
        Some(terms)
    } else {
        None
    }
}

#[allow(dead_code)]
fn corpus_candidate_paths() -> Vec<PathBuf> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut paths = Vec::new();
    if let Ok(path) = std::env::var("PART_VOCAB_CORPUS") {
        paths.push(PathBuf::from(path));
    }
    paths.push(default_corpus_cache_path());
    paths.push(manifest.join("benches/data/en_50k.txt"));
    paths.push(manifest.join("benches/data/english_words.txt"));
    paths.push(manifest.join("tests/fixtures/en_50k.txt"));
    paths.push(manifest.join("tests/fixtures/english_words.txt"));
    paths.push(manifest.join("data/en_50k.txt"));
    paths.push(manifest.join("data/english_words.txt"));
    paths
}

fn default_corpus_cache_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("target/bench-corpora/en_50k.txt")
}

fn load_terms_from_file(path: &Path, size: usize) -> Option<Vec<String>> {
    let content = fs::read_to_string(path).ok()?;
    let mut terms = Vec::with_capacity(size);
    let mut seen = HashSet::with_capacity(size * 2);
    for line in content.lines() {
        let Some(term) = normalize_corpus_line(line) else {
            continue;
        };
        if seen.insert(term.clone()) {
            terms.push(term);
            if terms.len() >= size {
                return Some(terms);
            }
        }
    }
    if terms.len() >= size / 2 {
        while terms.len() < size {
            let idx = terms.len();
            terms.push(format!("corpus-fill-{idx:06}"));
        }
        Some(terms)
    } else {
        None
    }
}

fn normalize_corpus_line(line: &str) -> Option<String> {
    let token = line.split_whitespace().next()?.trim();
    if token.len() < 2 || token.len() > 64 {
        return None;
    }
    if token.chars().any(|ch| ch.is_control()) {
        return None;
    }
    let normalized = token
        .trim_matches(|ch: char| !ch.is_alphanumeric() && ch != '\'' && ch != '-')
        .to_lowercase();
    if normalized.len() < 2
        || normalized.chars().all(|ch| ch.is_ascii_digit())
        || normalized.chars().any(char::is_whitespace)
    {
        None
    } else {
        Some(normalized)
    }
}

#[derive(Clone, Copy)]
enum VocabClass {
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

fn sample_vocab_class(rng: &mut StdRng, previous: Option<VocabClass>) -> VocabClass {
    let weights = match previous {
        None => [40, 24, 2, 0, 14, 8, 5],
        Some(VocabClass::LatinConsonant) => [16, 42, 2, 1, 2, 1, 1],
        Some(VocabClass::LatinVowel) => [45, 12, 2, 2, 2, 1, 1],
        Some(VocabClass::Digit) => [20, 14, 16, 1, 1, 1, 1],
        Some(VocabClass::Separator) => [36, 18, 2, 0, 5, 3, 2],
        Some(VocabClass::Cjk) => [4, 2, 1, 1, 38, 5, 1],
        Some(VocabClass::Kana) => [4, 2, 1, 1, 7, 34, 1],
        Some(VocabClass::Greek) => [6, 3, 1, 1, 1, 1, 32],
    };
    match sample_weighted_index(rng, &weights) {
        0 => VocabClass::LatinConsonant,
        1 => VocabClass::LatinVowel,
        2 => VocabClass::Digit,
        3 => VocabClass::Separator,
        4 => VocabClass::Cjk,
        5 => VocabClass::Kana,
        _ => VocabClass::Greek,
    }
}

fn sample_vocab_char(rng: &mut StdRng, class: VocabClass) -> char {
    const LATIN_CONSONANTS: &[char] = &[
        'b', 'c', 'd', 'f', 'g', 'h', 'j', 'k', 'l', 'm', 'n', 'p', 'q', 'r', 's', 't', 'v', 'w',
        'x', 'y', 'z',
    ];
    const LATIN_VOWELS: &[char] = &['a', 'e', 'i', 'o', 'u', 'á', 'é', 'í', 'ó', 'ú', 'ü'];
    const DIGITS: &[char] = &['0', '1', '2', '3', '4', '5', '6', '7', '8', '9'];
    const CJK: &[char] = &[
        '数', '据', '結', '構', '算', '法', '模', '型', '索', '引', '検', '索', '辞', '書', '言',
        '語', '東', '京', '都', '京',
    ];
    const KANA: &[char] = &[
        'ア', 'イ', 'ウ', 'エ', 'オ', 'カ', 'キ', 'ク', 'ケ', 'コ', 'サ', 'シ', 'ス', 'セ', 'ソ',
        'タ', 'テ', 'ン', 'ー',
    ];
    const GREEK: &[char] = &[
        'α', 'β', 'γ', 'δ', 'ε', 'η', 'θ', 'ι', 'κ', 'λ', 'μ', 'ν', 'ο', 'π', 'ρ', 'σ', 'τ',
    ];
    match class {
        VocabClass::LatinConsonant => LATIN_CONSONANTS[rng.gen_range(0..LATIN_CONSONANTS.len())],
        VocabClass::LatinVowel => LATIN_VOWELS[rng.gen_range(0..LATIN_VOWELS.len())],
        VocabClass::Digit => DIGITS[rng.gen_range(0..DIGITS.len())],
        VocabClass::Separator => ['-', '_'][rng.gen_range(0..2)],
        VocabClass::Cjk => CJK[rng.gen_range(0..CJK.len())],
        VocabClass::Kana => KANA[rng.gen_range(0..KANA.len())],
        VocabClass::Greek => GREEK[rng.gen_range(0..GREEK.len())],
    }
}

fn sample_seeded_vocab_term(rng: &mut StdRng) -> String {
    let lengths = [3usize, 4, 5, 6, 7, 8, 9, 10, 12, 16, 20];
    let weights = [2, 7, 12, 16, 17, 15, 11, 8, 5, 2, 1];
    let len = lengths[sample_weighted_index(rng, &weights)];
    let mut out = String::new();
    let mut previous = None;
    for index in 0..len {
        let mut class = sample_vocab_class(rng, previous);
        if index == 0 || index + 1 == len {
            if matches!(class, VocabClass::Separator) {
                class = VocabClass::LatinConsonant;
            }
        }
        out.push(sample_vocab_char(rng, class));
        previous = Some(class);
    }
    out
}

fn generate_seeded_vocab_terms(size: usize) -> Vec<String> {
    let mut rng = StdRng::seed_from_u64(FIXED_SEED);
    let mut terms = Vec::with_capacity(size);
    let mut seen = HashSet::with_capacity(size * 2);
    while terms.len() < size {
        let term = sample_seeded_vocab_term(&mut rng);
        if seen.insert(term.clone()) {
            terms.push(term);
        }
    }
    terms
}

#[allow(dead_code)]
fn fallback_vocab_terms(size: usize) -> Vec<String> {
    let prefixes = [
        "anti", "auto", "bio", "co", "counter", "de", "eco", "electro", "geo", "hyper", "inter",
        "macro", "micro", "multi", "neo", "non", "over", "photo", "poly", "post", "pre", "pro",
        "re", "semi", "sub", "super", "trans", "ultra", "under",
    ];
    let roots = [
        "account",
        "address",
        "algorithm",
        "analysis",
        "archive",
        "article",
        "asset",
        "balance",
        "billing",
        "cache",
        "capacity",
        "catalog",
        "channel",
        "client",
        "cluster",
        "column",
        "compiler",
        "context",
        "contract",
        "customer",
        "dataset",
        "decision",
        "document",
        "edition",
        "engine",
        "event",
        "feature",
        "filter",
        "gateway",
        "identity",
        "invoice",
        "journal",
        "keyword",
        "language",
        "ledger",
        "message",
        "metric",
        "model",
        "network",
        "object",
        "option",
        "package",
        "partition",
        "policy",
        "profile",
        "project",
        "query",
        "record",
        "region",
        "release",
        "report",
        "request",
        "response",
        "schema",
        "search",
        "segment",
        "session",
        "signal",
        "snapshot",
        "storage",
        "stream",
        "summary",
        "tenant",
        "token",
        "transaction",
        "update",
        "upload",
        "vector",
        "version",
        "workflow",
    ];
    let suffixes = [
        "", "able", "al", "ance", "ation", "ed", "er", "ers", "ful", "ing", "ion", "ism", "ist",
        "ity", "ive", "ize", "less", "ly", "ment", "ness", "or", "ory", "s", "ship",
    ];
    let domain_terms = [
        "access-control",
        "audit-log",
        "batch-window",
        "change-feed",
        "checkpoint",
        "content-address",
        "data-plane",
        "dead-letter",
        "feature-flag",
        "full-text",
        "idempotency-key",
        "inverted-index",
        "lease-token",
        "merge-policy",
        "object-store",
        "query-plan",
        "rate-limit",
        "read-replica",
        "schema-registry",
        "service-mesh",
        "snapshot-isolation",
        "token-budget",
        "write-ahead-log",
    ];
    let multilingual = [
        "算法",
        "数据",
        "模型",
        "索引",
        "検索",
        "辞書",
        "言語",
        "テスト",
        "데이터",
        "검색",
        "사전",
        "δοκιμή",
        "γλώσσα",
        "δεδομένα",
        "модель",
        "поиск",
        "словарь",
        "árbol",
        "búsqueda",
        "índice",
        "ação",
        "versão",
        "résumé",
        "naïve",
        "façade",
        "über",
        "東京",
        "京都",
        "서울",
        "Αθήνα",
    ];
    let identifier_heads = [
        "get",
        "set",
        "load",
        "save",
        "parse",
        "render",
        "merge",
        "split",
        "encode",
        "decode",
        "index",
        "search",
        "stream",
        "commit",
        "rollback",
        "publish",
        "subscribe",
        "validate",
    ];
    let identifier_tails = [
        "Account", "Address", "Batch", "Cache", "Cursor", "Document", "Entry", "Graph", "Index",
        "Ledger", "Message", "Node", "Policy", "Query", "Record", "Snapshot", "Token", "Vector",
    ];

    let mut terms = Vec::with_capacity(size);
    let mut i = 0usize;
    while terms.len() < size {
        let term = match i % 8 {
            0 => {
                let root = roots[(i / 8) % roots.len()];
                let suffix = suffixes[(i / (8 * roots.len())) % suffixes.len()];
                format!("{root}{suffix}")
            }
            1 => {
                let prefix = prefixes[(i / 8) % prefixes.len()];
                let root = roots[(i / (8 * prefixes.len())) % roots.len()];
                format!("{prefix}{root}")
            }
            2 => {
                let prefix = prefixes[(i / 8) % prefixes.len()];
                let root = roots[(i / (8 * prefixes.len())) % roots.len()];
                let suffix = suffixes[(i / (8 * prefixes.len() * roots.len())) % suffixes.len()];
                format!("{prefix}-{root}{suffix}")
            }
            3 => domain_terms[(i / 8) % domain_terms.len()].to_string(),
            4 => {
                let head = identifier_heads[(i / 8) % identifier_heads.len()];
                let tail =
                    identifier_tails[(i / (8 * identifier_heads.len())) % identifier_tails.len()];
                format!("{head}{tail}")
            }
            5 => {
                let term = multilingual[(i / 8) % multilingual.len()];
                let root = roots[(i / (8 * multilingual.len())) % roots.len()];
                format!("{term}-{root}")
            }
            6 => {
                let root = roots[(i / 8) % roots.len()];
                let shard = i % 997;
                format!("{root}_{shard:03}")
            }
            _ => {
                let root = roots[(i / 8) % roots.len()];
                let domain = domain_terms[(i / (8 * roots.len())) % domain_terms.len()];
                format!("{root}.{domain}")
            }
        };
        terms.push(term);
        i += 1;
    }
    terms.sort();
    terms.dedup();
    while terms.len() < size {
        let root = roots[terms.len() % roots.len()];
        let domain = domain_terms[(terms.len() / roots.len()) % domain_terms.len()];
        terms.push(format!("{root}-{domain}-{}", terms.len()));
    }
    terms
}

fn generate_queries(terms: &[String], count: usize) -> Vec<String> {
    let mut rng = StdRng::seed_from_u64(FIXED_SEED ^ 0x5155_4552_4945_5300);
    let mut queries = Vec::with_capacity(count);
    let hot_len = (terms.len() / 10).max(1);
    for i in 0..count {
        match sample_weighted_index(&mut rng, &[70, 20, 10]) {
            0 => queries.push(terms[rng.gen_range(0..hot_len)].clone()),
            1 => queries.push(terms[rng.gen_range(0..terms.len())].clone()),
            _ => {
                let base = &terms[i % terms.len()];
                queries.push(format!("{base}x"));
            }
        }
    }
    queries
}

fn create_vocab(terms: &[String]) -> (TempDir, PersistentVocabARTrie) {
    let dir = tempfile::Builder::new()
        .prefix("persistent_vocab_bench")
        .tempdir_in(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target"))
        .expect("create vocab bench tempdir");
    let path = dir.path().join("vocab.part");
    let vocab = PersistentVocabARTrie::create(&path).expect("create vocab trie");
    for term in terms {
        vocab.insert(term).expect("insert vocab term");
    }
    (dir, vocab)
}

fn lookup_vocab(vocab: &PersistentVocabARTrie, queries: &[String]) -> usize {
    let mut hits = 0usize;
    for query in queries {
        if vocab.get_index(black_box(query)).is_some() {
            hits += 1;
        }
    }
    black_box(hits)
}

fn duplicate_insert_vocab(vocab: &PersistentVocabARTrie, terms: &[String]) -> usize {
    let mut assigned = 0usize;
    for term in terms {
        if vocab.insert(black_box(term)).is_ok() {
            assigned += 1;
        }
    }
    black_box(assigned)
}

fn parallel_read_write_sample(readers: usize, terms: &[String]) -> Duration {
    let (_dir, vocab) = create_vocab(&terms[..FIXED_PARALLEL_KEYS / 2]);
    let vocab = Arc::new(vocab);
    let stop = Arc::new(AtomicBool::new(false));
    let ready = Arc::new(Barrier::new(readers + 2));
    let start_gate = Arc::new(Barrier::new(readers + 2));

    let mut handles = Vec::with_capacity(readers);
    for reader in 0..readers {
        let vocab = Arc::clone(&vocab);
        let ready = Arc::clone(&ready);
        let start_gate = Arc::clone(&start_gate);
        let keys = terms.to_vec();
        handles.push(thread::spawn(move || {
            ready.wait();
            start_gate.wait();
            let mut hits = 0usize;
            for op in 0..FIXED_OPS_PER_READER {
                let index = op.wrapping_mul(2_654_435_761).wrapping_add(reader * 17) % keys.len();
                if vocab.get_index(&keys[index]).is_some() {
                    hits += 1;
                }
            }
            black_box(hits)
        }));
    }

    let writer = {
        let vocab = Arc::clone(&vocab);
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
                let _ = vocab.insert(&keys[index]);
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
    let terms = generate_vocab_terms(FIXED_LOOKUP_SIZE);
    let queries = generate_queries(&terms, FIXED_QUERY_COUNT);
    let (_dir, vocab) = create_vocab(&terms);

    let lookup = collect_samples(
        || {
            let start = Instant::now();
            lookup_vocab(&vocab, &queries);
            start.elapsed()
        },
        FIXED_QUERY_COUNT as f64,
    );

    let duplicate_insert = collect_samples(
        || {
            let start = Instant::now();
            duplicate_insert_vocab(&vocab, &terms[..1_024]);
            start.elapsed()
        },
        1_024.0,
    );

    let parallel = collect_samples(
        || parallel_read_write_sample(FIXED_READER_COUNT, &terms),
        (FIXED_READER_COUNT * FIXED_OPS_PER_READER) as f64,
    );

    print_sample_line("get_index_ns_per_query", "ns/query", &lookup);
    print_sample_line("duplicate_insert_ns_per_term", "ns/term", &duplicate_insert);
    print_sample_line("parallel_ns_per_read", "ns/read", &parallel);
}

fn bench_vocab_lookup(c: &mut Criterion) {
    let terms = generate_vocab_terms(5_000);
    let queries = generate_queries(&terms, 1_000);
    let (_dir, vocab) = create_vocab(&terms);

    let mut group = c.benchmark_group("persistent_vocab_lookup");
    group.throughput(Throughput::Elements(queries.len() as u64));
    group.bench_with_input(
        BenchmarkId::new("get_index", terms.len()),
        &queries,
        |b, q| b.iter(|| lookup_vocab(&vocab, q)),
    );
    group.finish();
}

fn bench_vocab_duplicate_insert(c: &mut Criterion) {
    let terms = generate_vocab_terms(5_000);
    let (_dir, vocab) = create_vocab(&terms);

    let mut group = c.benchmark_group("persistent_vocab_duplicate_insert");
    group.throughput(Throughput::Elements(terms.len() as u64));
    group.bench_with_input(
        BenchmarkId::new("duplicate", terms.len()),
        &terms,
        |b, terms| b.iter(|| duplicate_insert_vocab(&vocab, terms)),
    );
    group.finish();
}

fn run_criterion() {
    let mut criterion = Criterion::default().configure_from_args();
    bench_vocab_lookup(&mut criterion);
    bench_vocab_duplicate_insert(&mut criterion);
    criterion.final_summary();
}

fn main() {
    if std::env::var_os("PART_VOCAB_FIXED_SAMPLES").is_some() {
        run_fixed_samples();
    } else {
        run_criterion();
    }
}
