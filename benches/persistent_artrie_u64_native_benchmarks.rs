#![cfg(feature = "persistent-artrie")]

//! Benchmarks for the native `PersistentARTrieU64` representation.
//!
//! Control: `EncodedPersistentARTrieU64`, which maps every public `u64` unit
//! onto eight byte transitions in the byte persistent ARTrie.
//! Treatment: `PersistentARTrieU64`, which stores one native edge per `u64`.
//!
//! The fixed-sample mode is intended for the pgmcp experiment protocol:
//!
//! ```bash
//! PART_U64_FIXED_SAMPLES=1 cargo bench --bench persistent_artrie_u64_native_benchmarks --features persistent-artrie
//! ```
//!
//! It prints raw per-round samples for Welch's t-test. Criterion mode remains
//! available for the usual local performance workflow.

use criterion::{black_box, BenchmarkId, Criterion, Throughput};
use libdictenstein::persistent_artrie::u64::EncodedPersistentARTrieU64;
use libdictenstein::persistent_artrie::PersistentARTrieU64;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

const LOOKUP_SIZE: usize = 8_192;
const LOOKUP_LEN: usize = 12;
const LOOKUP_QUERIES: usize = 16_384;
const FIXED_SAMPLES: usize = 51;
const FIXED_WARMUPS: usize = 3;
const PARALLEL_KEYS: usize = 8_192;
const OPS_PER_READER: usize = 12_000;
const WRITES_PER_SAMPLE: usize = 2_000;
const READER_COUNTS: &[usize] = &[1, 4, 8];

#[derive(Clone, Copy)]
enum Arm {
    Native,
    Encoded,
}

fn mix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn generate_sequences(count: usize, len: usize) -> Vec<Vec<u64>> {
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let mut sequence = Vec::with_capacity(len);
        for j in 0..len {
            sequence.push(mix64((i as u64) << 32 | j as u64));
        }
        out.push(sequence);
    }
    out
}

fn generate_queries(sequences: &[Vec<u64>], count: usize) -> Vec<Vec<u64>> {
    let mut queries = Vec::with_capacity(count);
    for i in 0..count {
        let base = &sequences[i % sequences.len()];
        if i % 2 == 0 {
            queries.push(base.clone());
        } else {
            let mut miss = base.clone();
            let last = miss.len() - 1;
            miss[last] ^= 0x8000_0000_0000_0000;
            queries.push(miss);
        }
    }
    queries
}

fn build_native(sequences: &[Vec<u64>]) -> PersistentARTrieU64<()> {
    let trie = PersistentARTrieU64::new();
    for sequence in sequences {
        trie.insert_sequence(sequence);
    }
    trie
}

fn build_encoded(sequences: &[Vec<u64>]) -> EncodedPersistentARTrieU64<()> {
    let trie = EncodedPersistentARTrieU64::new();
    for sequence in sequences {
        trie.insert_sequence(sequence);
    }
    trie
}

fn lookup_native(trie: &PersistentARTrieU64<()>, queries: &[Vec<u64>]) -> usize {
    let mut hits = 0usize;
    for query in queries {
        if trie.contains_sequence(black_box(query)) {
            hits += 1;
        }
    }
    black_box(hits)
}

fn lookup_encoded(trie: &EncodedPersistentARTrieU64<()>, queries: &[Vec<u64>]) -> usize {
    let mut hits = 0usize;
    for query in queries {
        if trie.contains_sequence(black_box(query)) {
            hits += 1;
        }
    }
    black_box(hits)
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

fn checkpoint_bytes_native(sequences: &[Vec<u64>]) -> u64 {
    let dir = tempfile::Builder::new()
        .prefix("part_u64_native")
        .tempdir_in(scratch_dir())
        .expect("native tempdir");
    let path = dir.path().join("native.partu64");
    let trie = PersistentARTrieU64::<()>::create(&path).expect("create native trie");
    for sequence in sequences {
        trie.insert_sequence(sequence);
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
    let trie = EncodedPersistentARTrieU64::<()>::create(&path).expect("create encoded trie");
    for sequence in sequences {
        trie.insert_sequence(sequence);
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
    let barrier = Arc::new(Barrier::new(readers + 2));

    let mut handles = Vec::with_capacity(readers);
    for reader in 0..readers {
        let trie = Arc::clone(&trie);
        let barrier = Arc::clone(&barrier);
        let keys = sequences.to_vec();
        handles.push(thread::spawn(move || {
            barrier.wait();
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
        let barrier = Arc::clone(&barrier);
        let stop = Arc::clone(&stop);
        let keys = sequences.to_vec();
        thread::spawn(move || {
            barrier.wait();
            let mut writes = 0usize;
            while !stop.load(Ordering::Relaxed) && writes < WRITES_PER_SAMPLE {
                let index = (PARALLEL_KEYS / 2) + (writes % (PARALLEL_KEYS / 2));
                trie.insert_sequence(&keys[index]);
                writes += 1;
            }
            black_box(writes)
        })
    };

    barrier.wait();
    let start = Instant::now();
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
    let barrier = Arc::new(Barrier::new(readers + 2));

    let mut handles = Vec::with_capacity(readers);
    for reader in 0..readers {
        let trie = Arc::clone(&trie);
        let barrier = Arc::clone(&barrier);
        let keys = sequences.to_vec();
        handles.push(thread::spawn(move || {
            barrier.wait();
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
        let barrier = Arc::clone(&barrier);
        let stop = Arc::clone(&stop);
        let keys = sequences.to_vec();
        thread::spawn(move || {
            barrier.wait();
            let mut writes = 0usize;
            while !stop.load(Ordering::Relaxed) && writes < WRITES_PER_SAMPLE {
                let index = (PARALLEL_KEYS / 2) + (writes % (PARALLEL_KEYS / 2));
                trie.insert_sequence(&keys[index]);
                writes += 1;
            }
            black_box(writes)
        })
    };

    barrier.wait();
    let start = Instant::now();
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

    group.bench_function(BenchmarkId::new("native_u64", LOOKUP_LEN), |b| {
        b.iter(|| lookup_native(&native, &queries));
    });
    group.bench_function(BenchmarkId::new("encoded_u64_as_bytes", LOOKUP_LEN), |b| {
        b.iter(|| lookup_encoded(&encoded, &queries));
    });

    group.finish();
}

fn bench_insert(c: &mut Criterion) {
    let sequences = generate_sequences(LOOKUP_SIZE, LOOKUP_LEN);
    let mut group = c.benchmark_group("persistent_artrie_u64_insert");
    group.sample_size(20);
    group.throughput(Throughput::Elements(LOOKUP_SIZE as u64));

    group.bench_function(BenchmarkId::new("native_u64", LOOKUP_LEN), |b| {
        b.iter(|| {
            let trie = PersistentARTrieU64::<()>::new();
            for sequence in &sequences {
                trie.insert_sequence(black_box(sequence));
            }
            black_box(trie)
        });
    });
    group.bench_function(BenchmarkId::new("encoded_u64_as_bytes", LOOKUP_LEN), |b| {
        b.iter(|| {
            let trie = EncodedPersistentARTrieU64::<()>::new();
            for sequence in &sequences {
                trie.insert_sequence(black_box(sequence));
            }
            black_box(trie)
        });
    });

    group.finish();
}

fn bench_checkpoint_disk_bytes(c: &mut Criterion) {
    let sequences = generate_sequences(2_048, LOOKUP_LEN);
    let native_bytes = checkpoint_bytes_native(&sequences);
    let encoded_bytes = checkpoint_bytes_encoded(&sequences);

    let mut group = c.benchmark_group("persistent_artrie_u64_checkpoint_bytes");
    group.sample_size(10);
    group.bench_function("native_u64_bytes", |b| {
        b.iter(|| black_box(native_bytes));
    });
    group.bench_function("encoded_u64_as_bytes_bytes", |b| {
        b.iter(|| black_box(encoded_bytes));
    });
    group.finish();

    eprintln!(
        "persistent_artrie_u64_checkpoint_bytes,native={},encoded={}",
        native_bytes, encoded_bytes
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

fn run_fixed_samples() {
    let sequences = generate_sequences(LOOKUP_SIZE, LOOKUP_LEN);
    let queries = generate_queries(&sequences, LOOKUP_QUERIES);

    let mut native_lookup = Vec::with_capacity(FIXED_SAMPLES);
    let mut encoded_lookup = Vec::with_capacity(FIXED_SAMPLES);

    for round in 0..(FIXED_WARMUPS + FIXED_SAMPLES) {
        let encoded = time_lookup_sample(Arm::Encoded, &sequences, &queries);
        let native = time_lookup_sample(Arm::Native, &sequences, &queries);

        if round >= FIXED_WARMUPS {
            encoded_lookup.push(encoded.as_nanos() as f64 / LOOKUP_QUERIES as f64);
            native_lookup.push(native.as_nanos() as f64 / LOOKUP_QUERIES as f64);
        }
    }

    let native_bytes = checkpoint_bytes_native(&sequences[..2_048]);
    let encoded_bytes = checkpoint_bytes_encoded(&sequences[..2_048]);

    print_sample_line(
        "lookup_ns_per_query",
        "control_encoded_u64_as_bytes",
        "ns/query",
        &encoded_lookup,
    );
    print_sample_line(
        "lookup_ns_per_query",
        "treatment_native_u64",
        "ns/query",
        &native_lookup,
    );
    println!("metric=checkpoint_disk_bytes,arm=control_encoded_u64_as_bytes,unit=bytes,samples={encoded_bytes}");
    println!(
        "metric=checkpoint_disk_bytes,arm=treatment_native_u64,unit=bytes,samples={native_bytes}"
    );
}

fn run_criterion() {
    let mut criterion = Criterion::default().configure_from_args();
    bench_lookup(&mut criterion);
    bench_insert(&mut criterion);
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
