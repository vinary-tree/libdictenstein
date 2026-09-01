#![cfg(feature = "persistent-artrie")]

//! Benchmarks for the native-key overlay `PersistentARTrieU64` representation.
//!
//! Control: `EncodedPersistentARTrieU64`, which maps every public `u64` unit
//! onto eight byte transitions in the byte persistent ARTrie.
//! Control: `PersistentARTrieU64Prefix3Compat`, the previous native-key CX
//! prefix budget.
//! Treatment: `PersistentARTrieU64Compact`, which stores one native edge per
//! `u64` and uses the widened CX prefix budget.
//!
//! The fixed workload is a seeded time-series stream: stream/metric identifiers,
//! monotonic timestamp/delta tokens, and IEEE-754 `f64::to_bits()` observations.
//! Terminal values are stored as `u64` float bits to match the intended payload
//! shape without adding floating-point equality semantics to `DictionaryValue`.
//!
//! The fixed-sample mode is intended for the pgmcp experiment protocol:
//!
//! ```bash
//! PART_U64_FIXED_SAMPLES=1 cargo bench --bench persistent_artrie_u64_native_benchmarks --features persistent-artrie
//! ```
//!
//! It prints raw per-round samples for Welch's t-test, including the encoded
//! control for the parallel reader/writer metric. Criterion mode remains
//! available for the usual local performance workflow.

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput};
use libdictenstein::persistent_artrie::u64::EncodedPersistentARTrieU64;
use libdictenstein::persistent_artrie::{
    PersistentARTrieU64Compact, PersistentARTrieU64Prefix3Compat,
};
use rand::distr::{weighted::WeightedIndex, Distribution};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::collections::HashSet;
use std::fs;
use std::hint::black_box;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

const LOOKUP_SIZE: usize = 8_192;
const LOOKUP_LEN: usize = 12;
const LOOKUP_QUERIES: usize = 16_384;
const MUTATION_SIZE: usize = 2_048;
const FIXED_SAMPLES: usize = 51;
const FIXED_WARMUPS: usize = 3;
const PARALLEL_KEYS: usize = 8_192;
const OPS_PER_READER: usize = 12_000;
const WRITES_PER_SAMPLE: usize = 2_000;
const READER_COUNTS: &[usize] = &[1, 4, 8];
const FIXED_SEED: u64 = 0x5041_5254_5536_3455;

#[derive(Clone, Copy)]
enum Arm {
    Native,
    NativePrefix3,
    Encoded,
}

#[derive(Clone, Copy)]
enum U64Class {
    StreamId,
    MetricId,
    TimestampNanos,
    DeltaNanos,
    FloatBits,
    EventCode,
}

fn sample_weighted_index(rng: &mut StdRng, weights: &[u32]) -> usize {
    WeightedIndex::new(weights)
        .expect("valid benchmark weights")
        .sample(rng)
}

fn sample_u64_class(rng: &mut StdRng, previous: Option<U64Class>, position: usize) -> U64Class {
    if position == 0 {
        return U64Class::StreamId;
    }
    if position == 1 {
        return U64Class::MetricId;
    }
    if position == 2 {
        return U64Class::TimestampNanos;
    }

    let weights = match previous {
        None => [0, 0, 60, 20, 20, 0],
        Some(U64Class::StreamId) => [3, 75, 17, 3, 2, 0],
        Some(U64Class::MetricId) => [0, 5, 72, 12, 10, 1],
        Some(U64Class::TimestampNanos) => [0, 0, 18, 38, 38, 6],
        Some(U64Class::DeltaNanos) => [0, 0, 25, 14, 56, 5],
        Some(U64Class::FloatBits) => [0, 4, 28, 42, 21, 5],
        Some(U64Class::EventCode) => [0, 5, 38, 24, 28, 5],
    };
    match sample_weighted_index(rng, &weights) {
        0 => U64Class::StreamId,
        1 => U64Class::MetricId,
        2 => U64Class::TimestampNanos,
        3 => U64Class::DeltaNanos,
        4 => U64Class::FloatBits,
        _ => U64Class::EventCode,
    }
}

fn sample_delta_nanos(rng: &mut StdRng) -> u64 {
    const BASE_DELTAS: &[u64] = &[
        1_000,             // 1 us
        1_000_000,         // 1 ms
        10_000_000,        // 10 ms
        100_000_000,       // 100 ms
        1_000_000_000,     // 1 s
        60_000_000_000,    // 1 min
        3_600_000_000_000, // 1 h
    ];
    const WEIGHTS: &[u32] = &[3, 20, 24, 22, 20, 8, 3];
    let base = BASE_DELTAS[sample_weighted_index(rng, WEIGHTS)];
    base.saturating_add(rng.random_range(0..=(base / 20).max(1)))
}

fn quantized_f64_bits(value: f64) -> u64 {
    let quantized = (value * 1_000_000.0).round() / 1_000_000.0;
    quantized.to_bits()
}

fn sample_u64_sequence(rng: &mut StdRng, len: usize) -> Vec<u64> {
    const TAG_STREAM: u64 = 0x1000_0000_0000_0000;
    const TAG_METRIC: u64 = 0x2000_0000_0000_0000;
    const TAG_TIMESTAMP: u64 = 0x3000_0000_0000_0000;
    const TAG_DELTA: u64 = 0x4000_0000_0000_0000;
    const TAG_EVENT: u64 = 0x5000_0000_0000_0000;
    const TAG_MASK: u64 = 0x0fff_ffff_ffff_ffff;

    let stream_id = rng.random_range(0..2_048u64);
    let mut metric_id = rng.random_range(0..512u64);
    let mut timestamp = 1_700_000_000_000_000_000u64
        + rng.random_range(0..86_400_000_000_000u64)
        + stream_id * 1_000_000;
    let seasonal_phase = rng.random_range(0.0..std::f64::consts::TAU);
    let mut value = rng.random_range(-20.0..20.0) + metric_id as f64 * 0.025;
    let mut previous = None;
    let mut sequence = Vec::with_capacity(len);
    for position in 0..len {
        let mut class = sample_u64_class(rng, previous, position);
        if position + 1 == len {
            class = U64Class::FloatBits;
        }
        let label = match class {
            U64Class::StreamId => TAG_STREAM | stream_id,
            U64Class::MetricId => {
                if position > 1 && rng.random_bool(0.18) {
                    metric_id = (metric_id + rng.random_range(1..17)) & 0x01ff;
                }
                TAG_METRIC | metric_id
            }
            U64Class::TimestampNanos => {
                timestamp = timestamp.saturating_add(sample_delta_nanos(rng));
                TAG_TIMESTAMP | (timestamp & TAG_MASK)
            }
            U64Class::DeltaNanos => {
                let delta = sample_delta_nanos(rng);
                timestamp = timestamp.saturating_add(delta);
                TAG_DELTA | (delta & TAG_MASK)
            }
            U64Class::FloatBits => {
                let drift = rng.random_range(-0.75..0.75);
                let seasonal = ((position as f64 * 0.37) + seasonal_phase).sin() * 0.35;
                value += drift + seasonal + metric_id as f64 * 0.0005;
                quantized_f64_bits(value)
            }
            U64Class::EventCode => {
                let code = rng.random_range(0..128u64);
                TAG_EVENT | ((metric_id & 0xffff) << 16) | code
            }
        };
        sequence.push(label);
        previous = Some(class);
    }
    sequence
}

fn terminal_value_bits(sequence: &[u64]) -> u64 {
    sequence.last().copied().unwrap_or_else(|| 0.0f64.to_bits())
}

fn generate_sequences_with_salt(count: usize, len: usize, salt: u64) -> Vec<Vec<u64>> {
    let mut rng = StdRng::seed_from_u64(FIXED_SEED ^ ((count as u64) << 16) ^ len as u64 ^ salt);
    let mut out = Vec::with_capacity(count);
    let mut seen = HashSet::with_capacity(count * 2);
    while out.len() < count {
        let sequence = sample_u64_sequence(&mut rng, len);
        if seen.insert(sequence.clone()) {
            out.push(sequence);
        }
    }
    out
}

fn generate_sequences(count: usize, len: usize) -> Vec<Vec<u64>> {
    generate_sequences_with_salt(count, len, 0)
}

fn generate_queries(sequences: &[Vec<u64>], count: usize) -> Vec<Vec<u64>> {
    let mut rng = StdRng::seed_from_u64(FIXED_SEED ^ 0x5155_4552_4945_5300);
    let mut queries = Vec::with_capacity(count);
    let hot_len = (sequences.len() / 10).max(1);
    for i in 0..count {
        match sample_weighted_index(&mut rng, &[70, 20, 10]) {
            0 => queries.push(sequences[rng.random_range(0..hot_len)].clone()),
            1 => queries.push(sequences[rng.random_range(0..sequences.len())].clone()),
            _ => {
                let base = &sequences[i % sequences.len()];
                let mut miss = base.clone();
                let last = miss.len() - 1;
                miss[last] = miss[last].wrapping_add(0x9e37_79b9_7f4a_7c15);
                queries.push(miss);
            }
        }
    }
    queries
}

fn build_native(sequences: &[Vec<u64>]) -> PersistentARTrieU64Compact<u64> {
    let trie = PersistentARTrieU64Compact::<u64>::new();
    for sequence in sequences {
        trie.insert_sequence_with_value(sequence, terminal_value_bits(sequence));
    }
    trie
}

fn build_native_prefix3(sequences: &[Vec<u64>]) -> PersistentARTrieU64Prefix3Compat<u64> {
    let trie = PersistentARTrieU64Prefix3Compat::<u64>::new();
    for sequence in sequences {
        trie.insert_sequence_with_value(sequence, terminal_value_bits(sequence));
    }
    trie
}

fn build_encoded(sequences: &[Vec<u64>]) -> EncodedPersistentARTrieU64<u64> {
    let trie = EncodedPersistentARTrieU64::<u64>::new();
    for sequence in sequences {
        trie.insert_sequence_with_value(sequence, terminal_value_bits(sequence));
    }
    trie
}

fn lookup_native(trie: &PersistentARTrieU64Compact<u64>, queries: &[Vec<u64>]) -> usize {
    let mut hits = 0usize;
    for query in queries {
        if trie.contains_sequence(black_box(query)) {
            hits += 1;
        }
    }
    black_box(hits)
}

fn lookup_native_prefix3(
    trie: &PersistentARTrieU64Prefix3Compat<u64>,
    queries: &[Vec<u64>],
) -> usize {
    let mut hits = 0usize;
    for query in queries {
        if trie.contains_sequence(black_box(query)) {
            hits += 1;
        }
    }
    black_box(hits)
}

fn lookup_encoded(trie: &EncodedPersistentARTrieU64<u64>, queries: &[Vec<u64>]) -> usize {
    let mut hits = 0usize;
    for query in queries {
        if trie.contains_sequence(black_box(query)) {
            hits += 1;
        }
    }
    black_box(hits)
}

fn consume_sequences(sequences: impl Iterator<Item = Vec<u64>>) -> (usize, u64) {
    let mut count = 0usize;
    let mut checksum = 0u64;
    for sequence in sequences {
        count += 1;
        checksum = checksum.rotate_left(7) ^ sequence.len() as u64;
        if let Some(&first) = sequence.first() {
            checksum ^= first;
        }
        if let Some(&last) = sequence.last() {
            checksum = checksum.rotate_left(11) ^ last;
        }
    }
    black_box((count, checksum))
}

fn directory_bytes(path: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(path) = stack.pop() {
        let Ok(meta) = fs::metadata(&path) else {
            continue;
        };
        if meta.is_file() {
            total += meta.len();
        } else if meta.is_dir() {
            let Ok(entries) = fs::read_dir(&path) else {
                continue;
            };
            for entry in entries.flatten() {
                stack.push(entry.path());
            }
        }
    }
    total
}

fn scratch_dir() -> std::path::PathBuf {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/bench-scratch");
    fs::create_dir_all(&path).expect("create bench scratch dir");
    path
}

fn checkpoint_bytes_native_compact(sequences: &[Vec<u64>]) -> u64 {
    let dir = tempfile::Builder::new()
        .prefix("part_u64_native")
        .tempdir_in(scratch_dir())
        .expect("native tempdir");
    let path = dir.path().join("native.partu64");
    let trie = PersistentARTrieU64Compact::<u64>::create(&path).expect("create compact u64 trie");
    for sequence in sequences {
        trie.insert_sequence_with_value(sequence, terminal_value_bits(sequence));
    }
    trie.checkpoint().expect("native checkpoint");
    directory_bytes(dir.path())
}

fn checkpoint_bytes_native_prefix3_compat(sequences: &[Vec<u64>]) -> u64 {
    let dir = tempfile::Builder::new()
        .prefix("part_u64_native")
        .tempdir_in(scratch_dir())
        .expect("native tempdir");
    let path = dir.path().join("native.partu64");
    let trie =
        PersistentARTrieU64Prefix3Compat::<u64>::create(&path).expect("create prefix3 u64 trie");
    for sequence in sequences {
        trie.insert_sequence_with_value(sequence, terminal_value_bits(sequence));
    }
    trie.checkpoint().expect("native checkpoint");
    directory_bytes(dir.path())
}

fn checkpoint_bytes_encoded(sequences: &[Vec<u64>]) -> u64 {
    let dir = tempfile::Builder::new()
        .prefix("part_u64_encoded")
        .tempdir_in(scratch_dir())
        .expect("encoded tempdir");
    let path = dir.path().join("encoded.part");
    let trie = EncodedPersistentARTrieU64::<u64>::create(&path).expect("create encoded trie");
    for sequence in sequences {
        trie.insert_sequence_with_value(sequence, terminal_value_bits(sequence));
    }
    trie.checkpoint().expect("encoded checkpoint");
    directory_bytes(dir.path())
}

fn time_lookup_sample(arm: Arm, sequences: &[Vec<u64>], queries: &[Vec<u64>]) -> Duration {
    match arm {
        Arm::Native => {
            let trie = build_native(sequences);
            let start = Instant::now();
            lookup_native(&trie, queries);
            start.elapsed()
        }
        Arm::NativePrefix3 => {
            let trie = build_native_prefix3(sequences);
            let start = Instant::now();
            lookup_native_prefix3(&trie, queries);
            start.elapsed()
        }
        Arm::Encoded => {
            let trie = build_encoded(sequences);
            let start = Instant::now();
            lookup_encoded(&trie, queries);
            start.elapsed()
        }
    }
}

fn parallel_native_sample(readers: usize, sequences: &[Vec<u64>]) -> Duration {
    let trie = Arc::new(build_native(&sequences[..PARALLEL_KEYS / 2]));
    let stop = Arc::new(AtomicBool::new(false));
    let ready = Arc::new(Barrier::new(readers + 2));
    let start_gate = Arc::new(Barrier::new(readers + 2));

    let mut handles = Vec::with_capacity(readers);
    for reader in 0..readers {
        let trie = Arc::clone(&trie);
        let ready = Arc::clone(&ready);
        let start_gate = Arc::clone(&start_gate);
        let keys = sequences.to_vec();
        handles.push(thread::spawn(move || {
            ready.wait();
            start_gate.wait();
            let mut hits = 0usize;
            for op in 0..OPS_PER_READER {
                let index = op.wrapping_mul(2_654_435_761).wrapping_add(reader * 17) % keys.len();
                if trie.contains_sequence(&keys[index]) {
                    hits += 1;
                }
            }
            black_box(hits)
        }));
    }

    let writer = {
        let trie = Arc::clone(&trie);
        let ready = Arc::clone(&ready);
        let start_gate = Arc::clone(&start_gate);
        let stop = Arc::clone(&stop);
        let keys = sequences.to_vec();
        thread::spawn(move || {
            ready.wait();
            start_gate.wait();
            let mut writes = 0usize;
            while !stop.load(Ordering::Relaxed) && writes < WRITES_PER_SAMPLE {
                let index = (PARALLEL_KEYS / 2) + (writes % (PARALLEL_KEYS / 2));
                trie.insert_sequence_with_value(&keys[index], terminal_value_bits(&keys[index]));
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

fn parallel_encoded_sample(readers: usize, sequences: &[Vec<u64>]) -> Duration {
    let trie = Arc::new(build_encoded(&sequences[..PARALLEL_KEYS / 2]));
    let stop = Arc::new(AtomicBool::new(false));
    let ready = Arc::new(Barrier::new(readers + 2));
    let start_gate = Arc::new(Barrier::new(readers + 2));

    let mut handles = Vec::with_capacity(readers);
    for reader in 0..readers {
        let trie = Arc::clone(&trie);
        let ready = Arc::clone(&ready);
        let start_gate = Arc::clone(&start_gate);
        let keys = sequences.to_vec();
        handles.push(thread::spawn(move || {
            ready.wait();
            start_gate.wait();
            let mut hits = 0usize;
            for op in 0..OPS_PER_READER {
                let index = op.wrapping_mul(2_654_435_761).wrapping_add(reader * 17) % keys.len();
                if trie.contains_sequence(&keys[index]) {
                    hits += 1;
                }
            }
            black_box(hits)
        }));
    }

    let writer = {
        let trie = Arc::clone(&trie);
        let ready = Arc::clone(&ready);
        let start_gate = Arc::clone(&start_gate);
        let stop = Arc::clone(&stop);
        let keys = sequences.to_vec();
        thread::spawn(move || {
            ready.wait();
            start_gate.wait();
            let mut writes = 0usize;
            while !stop.load(Ordering::Relaxed) && writes < WRITES_PER_SAMPLE {
                let index = (PARALLEL_KEYS / 2) + (writes % (PARALLEL_KEYS / 2));
                trie.insert_sequence_with_value(&keys[index], terminal_value_bits(&keys[index]));
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

fn bench_lookup(c: &mut Criterion) {
    let sequences = generate_sequences(LOOKUP_SIZE, LOOKUP_LEN);
    let queries = generate_queries(&sequences, LOOKUP_QUERIES);
    let native = build_native(&sequences);
    let encoded = build_encoded(&sequences);

    let mut group = c.benchmark_group("persistent_artrie_u64_lookup");
    group.sample_size(30);
    group.throughput(Throughput::Elements(LOOKUP_QUERIES as u64));

    group.bench_function(BenchmarkId::new("native_u64_prefix4", LOOKUP_LEN), |b| {
        b.iter(|| lookup_native(&native, &queries));
    });
    let native_prefix3 = build_native_prefix3(&sequences);
    group.bench_function(BenchmarkId::new("native_u64_prefix3", LOOKUP_LEN), |b| {
        b.iter(|| lookup_native_prefix3(&native_prefix3, &queries));
    });
    group.bench_function(BenchmarkId::new("encoded_u64_as_bytes", LOOKUP_LEN), |b| {
        b.iter(|| lookup_encoded(&encoded, &queries));
    });

    group.finish();
}

fn bench_iteration(c: &mut Criterion) {
    let sequences = generate_sequences(LOOKUP_SIZE, LOOKUP_LEN);
    let native = build_native(&sequences);
    let native_prefix3 = build_native_prefix3(&sequences);
    let encoded = build_encoded(&sequences);

    let mut group = c.benchmark_group("persistent_artrie_u64_iteration");
    group.sample_size(30);
    group.throughput(Throughput::Elements(LOOKUP_SIZE as u64));
    group.bench_function(BenchmarkId::new("native_u64_prefix4", LOOKUP_LEN), |b| {
        b.iter(|| consume_sequences(native.iter_sequences()));
    });
    group.bench_function(BenchmarkId::new("native_u64_prefix3", LOOKUP_LEN), |b| {
        b.iter(|| consume_sequences(native_prefix3.iter_sequences()));
    });
    group.bench_function(BenchmarkId::new("encoded_u64_as_bytes", LOOKUP_LEN), |b| {
        b.iter(|| consume_sequences(encoded.iter_sequences()));
    });
    group.finish();

    let mut lazy_group = c.benchmark_group("persistent_artrie_u64_iteration_laziness");
    lazy_group.sample_size(30);
    lazy_group.bench_function("construct_and_drop_without_next", |b| {
        b.iter(|| {
            let iterator = black_box(&native).iter_sequences();
            drop(black_box(iterator));
        });
    });
    lazy_group.bench_function("first_item_then_drop", |b| {
        b.iter(|| {
            let mut iterator = black_box(&native).iter_sequences();
            black_box(iterator.next());
            drop(black_box(iterator));
        });
    });
    lazy_group.bench_function("take_16_then_drop", |b| {
        b.iter(|| consume_sequences(black_box(&native).iter_sequences().take(16)));
    });
    let hit_prefix = sequences[LOOKUP_SIZE / 2][..4].to_vec();
    let miss_prefix = vec![u64::MAX; 4];
    lazy_group.bench_function("prefix_hit_complete", |b| {
        b.iter(|| {
            consume_sequences(black_box(&native).iter_sequence_prefix(black_box(&hit_prefix)))
        });
    });
    lazy_group.bench_function("prefix_miss", |b| {
        b.iter(|| {
            consume_sequences(black_box(&native).iter_sequence_prefix(black_box(&miss_prefix)))
        });
    });
    lazy_group.finish();

    const DEEP_SEQUENCE_LEN: usize = 1_024;
    let deep_sequence = (0..DEEP_SEQUENCE_LEN)
        .map(|index| u64::try_from(index).expect("benchmark index fits u64"))
        .collect::<Vec<_>>();
    let deep = build_native(std::slice::from_ref(&deep_sequence));
    let mut deep_group = c.benchmark_group("persistent_artrie_u64_deep_iteration");
    deep_group.sample_size(20);
    deep_group.throughput(Throughput::Elements(DEEP_SEQUENCE_LEN as u64));
    deep_group.bench_function("native_u64_single_sparse_terminal", |b| {
        b.iter(|| consume_sequences(deep.iter_sequences()));
    });
    deep_group.bench_function("native_u64_first_item_then_drop", |b| {
        b.iter(|| {
            let mut iterator = black_box(&deep).iter_sequences();
            black_box(iterator.next());
            drop(black_box(iterator));
        });
    });
    deep_group.finish();

    let iterator = native.iter_sequences();
    eprintln!(
        "persistent_artrie_u64_iterator_size_bytes={}",
        std::mem::size_of_val(&iterator)
    );
}

fn bench_insert(c: &mut Criterion) {
    let sequences = generate_sequences(LOOKUP_SIZE, LOOKUP_LEN);
    let mut group = c.benchmark_group("persistent_artrie_u64_insert");
    // Use the same within-process Criterion sample count as the other fixed
    // workloads. These samples estimate local harness noise; they are not
    // independent experimental runs. The paired base/treatment experiment
    // launches separately randomized processes and records those observations
    // in pgmcp.
    group.sample_size(FIXED_SAMPLES);
    group.throughput(Throughput::Elements(LOOKUP_SIZE as u64));

    group.bench_function(BenchmarkId::new("native_u64_prefix4", LOOKUP_LEN), |b| {
        b.iter(|| {
            let trie = PersistentARTrieU64Compact::<u64>::new();
            for sequence in &sequences {
                let sequence = black_box(sequence);
                trie.insert_sequence_with_value(sequence, terminal_value_bits(sequence));
            }
            black_box(trie)
        });
    });
    group.bench_function(BenchmarkId::new("native_u64_prefix3", LOOKUP_LEN), |b| {
        b.iter(|| {
            let trie = PersistentARTrieU64Prefix3Compat::<u64>::new();
            for sequence in &sequences {
                let sequence = black_box(sequence);
                trie.insert_sequence_with_value(sequence, terminal_value_bits(sequence));
            }
            black_box(trie)
        });
    });
    group.bench_function(BenchmarkId::new("encoded_u64_as_bytes", LOOKUP_LEN), |b| {
        b.iter(|| {
            let trie = EncodedPersistentARTrieU64::<u64>::new();
            for sequence in &sequences {
                let sequence = black_box(sequence);
                trie.insert_sequence_with_value(sequence, terminal_value_bits(sequence));
            }
            black_box(trie)
        });
    });

    group.finish();
}

fn bench_update_remove(c: &mut Criterion) {
    let sequences = generate_sequences(MUTATION_SIZE, LOOKUP_LEN);
    let mut group = c.benchmark_group("persistent_artrie_u64_update_remove");
    group.sample_size(20);
    group.throughput(Throughput::Elements(MUTATION_SIZE as u64));

    group.bench_function("native_u64_update_existing", |b| {
        b.iter_batched(
            || build_native(&sequences),
            |trie| {
                for sequence in &sequences {
                    let updated = terminal_value_bits(sequence).wrapping_add(1);
                    black_box(trie.insert_sequence_with_value(black_box(sequence), updated));
                }
                black_box(trie)
            },
            BatchSize::PerIteration,
        );
    });
    group.bench_function("native_u64_remove_existing", |b| {
        b.iter_batched(
            || build_native(&sequences),
            |trie| {
                for sequence in &sequences {
                    black_box(trie.remove_sequence(black_box(sequence)));
                }
                black_box(trie)
            },
            BatchSize::PerIteration,
        );
    });
    group.finish();
}

fn bench_checkpoint_reopen(c: &mut Criterion) {
    let sequences = generate_sequences(MUTATION_SIZE, LOOKUP_LEN);
    let mut group = c.benchmark_group("persistent_artrie_u64_checkpoint_reopen");
    group.sample_size(10);
    group.throughput(Throughput::Elements(MUTATION_SIZE as u64));
    group.bench_function("native_u64", |b| {
        b.iter_batched(
            || {
                let dir = tempfile::Builder::new()
                    .prefix("part_u64_checkpoint_reopen")
                    .tempdir_in(scratch_dir())
                    .expect("native checkpoint/reopen tempdir");
                let path = dir.path().join("native.partu64");
                let trie = PersistentARTrieU64Compact::<u64>::create(&path)
                    .expect("create native checkpoint/reopen trie");
                for sequence in &sequences {
                    trie.insert_sequence_with_value(sequence, terminal_value_bits(sequence));
                }
                (dir, path, trie)
            },
            |(dir, path, trie)| {
                trie.checkpoint().expect("checkpoint native u64 trie");
                drop(trie);
                let reopened =
                    PersistentARTrieU64Compact::<u64>::open(&path).expect("reopen native u64 trie");
                black_box(reopened.term_count());
                black_box(dir);
            },
            BatchSize::PerIteration,
        );
    });
    group.finish();
}

fn bench_checkpoint_disk_bytes(c: &mut Criterion) {
    let sequences = generate_sequences(2_048, LOOKUP_LEN);
    let native_bytes = checkpoint_bytes_native_compact(&sequences);
    let native_prefix3_bytes = checkpoint_bytes_native_prefix3_compat(&sequences);
    let encoded_bytes = checkpoint_bytes_encoded(&sequences);

    let mut group = c.benchmark_group("persistent_artrie_u64_checkpoint_bytes");
    group.sample_size(10);
    group.bench_function("native_u64_prefix4_bytes", |b| {
        b.iter(|| black_box(native_bytes));
    });
    group.bench_function("native_u64_prefix3_bytes", |b| {
        b.iter(|| black_box(native_prefix3_bytes));
    });
    group.bench_function("encoded_u64_as_bytes_bytes", |b| {
        b.iter(|| black_box(encoded_bytes));
    });
    group.finish();

    eprintln!(
        "persistent_artrie_u64_checkpoint_bytes,native_prefix4={},native_prefix3={},encoded={}",
        native_bytes, native_prefix3_bytes, encoded_bytes
    );
}

fn bench_parallel_reads_writes(c: &mut Criterion) {
    let sequences = generate_sequences(PARALLEL_KEYS, LOOKUP_LEN);
    let mut group = c.benchmark_group("persistent_artrie_u64_parallel_reads_writes");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(5));

    for &readers in READER_COUNTS {
        group.throughput(Throughput::Elements((readers * OPS_PER_READER) as u64));
        group.bench_with_input(
            BenchmarkId::new("native_u64", readers),
            &readers,
            |b, &readers| {
                b.iter_custom(|iters| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        total += parallel_native_sample(readers, &sequences);
                    }
                    total
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("encoded_u64_as_bytes", readers),
            &readers,
            |b, &readers| {
                b.iter_custom(|iters| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        total += parallel_encoded_sample(readers, &sequences);
                    }
                    total
                })
            },
        );
    }

    group.finish();
}

fn print_sample_line(metric: &str, arm: &str, unit: &str, samples: &[f64]) {
    print!("metric={metric},arm={arm},unit={unit},samples=");
    for (index, sample) in samples.iter().enumerate() {
        if index > 0 {
            print!(";");
        }
        print!("{sample:.6}");
    }
    println!();
}

fn native_edge_store_arm_label() -> &'static str {
    "treatment_overlay_cx_prefix4"
}

fn run_fixed_samples() {
    let sequences = generate_sequences(LOOKUP_SIZE, LOOKUP_LEN);
    let queries = generate_queries(&sequences, LOOKUP_QUERIES);

    let mut native_lookup = Vec::with_capacity(FIXED_SAMPLES);
    let mut prefix3_lookup = Vec::with_capacity(FIXED_SAMPLES);
    let mut encoded_lookup = Vec::with_capacity(FIXED_SAMPLES);
    let mut native_parallel = Vec::with_capacity(FIXED_SAMPLES);
    let mut encoded_parallel = Vec::with_capacity(FIXED_SAMPLES);
    let mut prefix3_bytes_per_entry = Vec::with_capacity(FIXED_SAMPLES);
    let mut prefix4_bytes_per_entry = Vec::with_capacity(FIXED_SAMPLES);

    for round in 0..(FIXED_WARMUPS + FIXED_SAMPLES) {
        let encoded = time_lookup_sample(Arm::Encoded, &sequences, &queries);
        let native = time_lookup_sample(Arm::Native, &sequences, &queries);
        let prefix3 = time_lookup_sample(Arm::NativePrefix3, &sequences, &queries);
        let parallel = parallel_native_sample(8, &sequences);
        let encoded_parallel_sample = parallel_encoded_sample(8, &sequences);
        let checkpoint_sequences =
            generate_sequences_with_salt(2_048, LOOKUP_LEN, 0x4348_4b50_0000_0000 ^ round as u64);
        let prefix3_bytes = checkpoint_bytes_native_prefix3_compat(&checkpoint_sequences);
        let prefix4_bytes = checkpoint_bytes_native_compact(&checkpoint_sequences);

        if round >= FIXED_WARMUPS {
            encoded_lookup.push(encoded.as_nanos() as f64 / LOOKUP_QUERIES as f64);
            native_lookup.push(native.as_nanos() as f64 / LOOKUP_QUERIES as f64);
            prefix3_lookup.push(prefix3.as_nanos() as f64 / LOOKUP_QUERIES as f64);
            native_parallel.push(parallel.as_nanos() as f64 / (8 * OPS_PER_READER) as f64);
            encoded_parallel
                .push(encoded_parallel_sample.as_nanos() as f64 / (8 * OPS_PER_READER) as f64);
            prefix3_bytes_per_entry.push(prefix3_bytes as f64 / checkpoint_sequences.len() as f64);
            prefix4_bytes_per_entry.push(prefix4_bytes as f64 / checkpoint_sequences.len() as f64);
        }
    }

    let native_bytes = checkpoint_bytes_native_compact(&sequences[..2_048]);
    let encoded_bytes = checkpoint_bytes_encoded(&sequences[..2_048]);

    print_sample_line(
        "lookup_ns_per_query",
        "control_encoded_u64_as_bytes",
        "ns/query",
        &encoded_lookup,
    );
    print_sample_line(
        "lookup_ns_per_query",
        native_edge_store_arm_label(),
        "ns/query",
        &native_lookup,
    );
    print_sample_line(
        "lookup_ns_per_query",
        "control_overlay_cx_prefix3",
        "ns/query",
        &prefix3_lookup,
    );
    print_sample_line(
        "parallel_ns_per_read",
        native_edge_store_arm_label(),
        "ns/read",
        &native_parallel,
    );
    print_sample_line(
        "parallel_ns_per_read",
        "control_encoded_u64_as_bytes",
        "ns/read",
        &encoded_parallel,
    );
    print_sample_line(
        "checkpoint_bytes_per_entry",
        "control_overlay_cx_prefix3",
        "bytes/entry",
        &prefix3_bytes_per_entry,
    );
    print_sample_line(
        "checkpoint_bytes_per_entry",
        "treatment_overlay_cx_prefix4",
        "bytes/entry",
        &prefix4_bytes_per_entry,
    );
    println!("metric=checkpoint_disk_bytes,arm=control_encoded_u64_as_bytes,unit=bytes,samples={encoded_bytes}");
    println!(
        "metric=checkpoint_disk_bytes,arm={},unit=bytes,samples={native_bytes}",
        native_edge_store_arm_label()
    );
}

fn run_criterion() {
    let mut criterion = Criterion::default().configure_from_args();
    bench_lookup(&mut criterion);
    bench_iteration(&mut criterion);
    bench_insert(&mut criterion);
    bench_update_remove(&mut criterion);
    bench_checkpoint_reopen(&mut criterion);
    bench_checkpoint_disk_bytes(&mut criterion);
    bench_parallel_reads_writes(&mut criterion);
    criterion.final_summary();
}

fn main() {
    if std::env::var_os("PART_U64_FIXED_SAMPLES").is_some() {
        run_fixed_samples();
    } else {
        run_criterion();
    }
}
