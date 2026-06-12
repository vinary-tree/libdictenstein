//! Lock-free char-ARTrie overlay write-throughput benchmark.
//!
//! REPLACES the original owned-`Arc<RwLock>` CONTROL vs lock-free-overlay TREATMENT comparison: that
//! "flip" experiment is OBSOLETE — post-C2/V6 there is no owned tree to flip from, the lock-free
//! overlay is the SOLE representation (`SharedCharARTrie` is a bare `Arc`, reads + writes are
//! lock-free). This now measures the overlay's insert + checkpoint throughput DIRECTLY — the metric
//! the flip experiment ultimately cared about. (The original control/treatment harness is preserved
//! in git history at the pre-rewrite commit; rebuilding it is impossible without the deleted owned
//! tree.)
//!
//! Scratch is real disk (`target/test-tmp`), never `/tmp` (tmpfs on this host — the mmap arena cannot
//! be tmpfs-backed).

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use std::path::{Path, PathBuf};

use libdictenstein::artrie_trait::ARTrie;
use libdictenstein::persistent_artrie::char::SharedCharARTrie;
use libdictenstein::MutableMappedDictionary;

static SCRATCH_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Real-disk scratch path (the mmap arena cannot live on tmpfs; `/tmp` is tmpfs here). UNIQUE per
/// call: `iter_batched` recreates the trie every batch, and a reused path hits `Wal(AlreadyExists)`
/// once a prior WAL outlives a missed cleanup.
fn scratch(tag: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp");
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let seq = SCRATCH_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    dir.join(format!(
        "lockfree-overlay-{tag}-{}-{seq}.artc",
        std::process::id()
    ))
}

fn cleanup(path: &Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(path.with_extension("wal"));
    // The WAL is `<path>.wal` (extension APPENDED, not replaced) — remove that form too.
    let mut wal = path.as_os_str().to_owned();
    wal.push(".wal");
    let _ = std::fs::remove_file(PathBuf::from(wal));
}

fn terms(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("term_{i:08}")).collect()
}

/// Lock-free overlay insert throughput (the obsolete flip benchmark's real subject): insert `n`
/// distinct terms into a fresh `SharedCharARTrie` (bare `Arc`, no external lock).
fn bench_overlay_insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("lockfree_overlay_insert");
    for &n in &[1_000usize, 10_000] {
        let data = terms(n);
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &data, |b, data| {
            b.iter_batched(
                || {
                    let path = scratch(&format!("ins{n}"));
                    cleanup(&path);
                    let trie: SharedCharARTrie<u64> =
                        ARTrie::create(&path).expect("create char trie");
                    (path, trie)
                },
                |(path, trie)| {
                    for (i, t) in data.iter().enumerate() {
                        let _ =
                            MutableMappedDictionary::insert_with_value(&trie, t.as_str(), i as u64);
                    }
                    cleanup(&path);
                },
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

/// Checkpoint throughput: publish the populated overlay as the path-compressed on-disk image
/// (`&self` checkpoint — CX-universal compressed serialize).
fn bench_overlay_checkpoint(c: &mut Criterion) {
    let mut group = c.benchmark_group("lockfree_overlay_checkpoint");
    let n = 10_000usize;
    let data = terms(n);
    group.throughput(Throughput::Elements(n as u64));
    group.bench_function(BenchmarkId::from_parameter(n), |b| {
        b.iter_batched(
            || {
                let path = scratch("ckpt");
                cleanup(&path);
                let trie: SharedCharARTrie<u64> = ARTrie::create(&path).expect("create char trie");
                for (i, t) in data.iter().enumerate() {
                    let _ = MutableMappedDictionary::insert_with_value(&trie, t.as_str(), i as u64);
                }
                (path, trie)
            },
            |(path, trie)| {
                trie.checkpoint().expect("checkpoint");
                cleanup(&path);
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

criterion_group!(benches, bench_overlay_insert, bench_overlay_checkpoint);
criterion_main!(benches);
