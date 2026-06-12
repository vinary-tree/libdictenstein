//! Experiment harness for pgmcp experiment #11 — non-blocking checkpoint.
//!
//! Emits RAW per-replicate resident-reader throughput samples (Melem/s) measured
//! while a checkpoint thread runs concurrently, for two arms:
//!   - `control`   : blocking checkpoint (`{ let mut g = trie.write(); g.checkpoint() }`)
//!                   — holds the trie write lock across its entire I/O.
//!   - `treatment` : `ARTrie::checkpoint(&trie)` — the (to-be) non-blocking wrapper.
//!
//! Output: one line per sample `arm reader_count throughput_melem_per_s`, plus
//! warm-up samples flagged. These feed `experiment_record_measurement` and the
//! pre-registered Welch's t-test (`experiment_decide`).
//!
//! RESOURCE SAFETY (mandatory): the disk-backed trie lives under
//! `target/bench-scratch` (REAL disk, never tmpfs/`/tmp`); the checkpoint loop is
//! throttled + hard-capped; a disk-usage ceiling aborts the run if exceeded.
//!
//! Run (pinned, memory-capped):
//! ```bash
//! TMPDIR="$PWD/target/test-tmp" systemd-run --user --scope -p MemoryMax=32G --quiet \
//!   taskset -c 0-15 cargo run --release --example exp_checkpoint_throughput --features persistent-artrie
//! ```

#![cfg(feature = "persistent-artrie")]

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use libdictenstein::persistent_artrie::char::SharedCharARTrie;
// F4: the `.read()/.write()` compat shim on the collapsed handle.
use libdictenstein::persistent_artrie::core::shared_access::SharedTrieAccess;
use libdictenstein::{ARTrie, Dictionary};

const KEY_COUNT: usize = 5_000;
const OPS_PER_READER: usize = 20_000;
const READER_COUNTS: &[usize] = &[4, 8];
const SAMPLES_PER_ARM: usize = 60; // power analysis requires >= 51 non-warmup/arm
const WARMUP: usize = 3;
const CHECKPOINT_THROTTLE: Duration = Duration::from_millis(5);
const MAX_CHECKPOINT_ROUNDS: usize = 1_000;
/// Hard ceiling on scratch-dir size; abort rather than fill the disk.
const SCRATCH_CEILING_BYTES: u64 = 2 * 1024 * 1024 * 1024; // 2 GiB

#[derive(Clone, Copy)]
enum Arm {
    Control,
    Treatment,
}

impl Arm {
    #[allow(dead_code)]
    fn label(self) -> &'static str {
        match self {
            Arm::Control => "control",
            Arm::Treatment => "treatment",
        }
    }
    /// Dirty the trie then checkpoint, using the arm's locking discipline.
    fn checkpoint_round(self, trie: &SharedCharARTrie<i64>, round: usize) {
        ARTrie::insert_with_value(trie, &key_for(round % KEY_COUNT), round as i64);
        match self {
            // Blocking baseline: hold the trie write lock across the whole checkpoint.
            Arm::Control => {
                let g = trie.write();
                g.checkpoint().expect("control checkpoint");
            }
            // The wrapper (non-blocking after the change; identical to control before it).
            Arm::Treatment => {
                ARTrie::checkpoint(trie).expect("treatment checkpoint");
            }
        }
    }
}

fn key_for(i: usize) -> String {
    format!("term-{:08}-キー", i)
}

fn scratch_dir() -> std::path::PathBuf {
    let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/bench-scratch");
    std::fs::create_dir_all(&d).expect("create real-disk scratch dir");
    d
}

fn dir_size(path: &Path) -> u64 {
    let mut total = 0;
    if let Ok(entries) = std::fs::read_dir(path) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                total += dir_size(&p);
            } else if let Ok(m) = e.metadata() {
                total += m.len();
            }
        }
    }
    total
}

fn build_trie(dir: &Path) -> SharedCharARTrie<i64> {
    let path = dir.join("exp_checkpoint.part");
    let trie: SharedCharARTrie<i64> = ARTrie::create(&path).expect("create shared char trie");
    for i in 0..KEY_COUNT {
        ARTrie::insert_with_value(&trie, &key_for(i), i as i64);
    }
    ARTrie::checkpoint(&trie).expect("initial checkpoint");
    trie
}

/// Resident-reader throughput (Melem/s) over one sample while a checkpoint
/// thread runs the given arm's checkpoint loop concurrently.
fn sample_throughput(trie: &SharedCharARTrie<i64>, n_readers: usize, arm: Arm) -> f64 {
    let stop = Arc::new(AtomicBool::new(false));
    let barrier = Arc::new(Barrier::new(n_readers + 2)); // readers + checkpointer + main

    let mut readers = Vec::with_capacity(n_readers);
    for t in 0..n_readers {
        let trie = Arc::clone(trie);
        let barrier = Arc::clone(&barrier);
        readers.push(thread::spawn(move || {
            barrier.wait();
            let mut hits = 0usize;
            for op in 0..OPS_PER_READER {
                let idx = op.wrapping_mul(2_654_435_761).wrapping_add(t * 7) % KEY_COUNT;
                if Dictionary::contains(&trie, &key_for(idx)) {
                    hits += 1;
                }
            }
            std::hint::black_box(hits);
        }));
    }

    let checkpointer = {
        let trie = Arc::clone(trie);
        let barrier = Arc::clone(&barrier);
        let stop = Arc::clone(&stop);
        thread::spawn(move || {
            barrier.wait();
            let mut rounds = 0usize;
            while !stop.load(Ordering::Relaxed) && rounds < MAX_CHECKPOINT_ROUNDS {
                arm.checkpoint_round(&trie, rounds);
                rounds += 1;
                thread::sleep(CHECKPOINT_THROTTLE);
            }
        })
    };

    barrier.wait();
    let start = Instant::now();
    for r in readers {
        r.join().expect("reader thread");
    }
    let elapsed = start.elapsed();
    stop.store(true, Ordering::Relaxed);
    checkpointer.join().expect("checkpointer thread");

    let ops = (n_readers * OPS_PER_READER) as f64;
    ops / elapsed.as_secs_f64() / 1.0e6
}

fn main() {
    let dir = scratch_dir();
    eprintln!(
        "# exp_checkpoint_throughput: KEY_COUNT={KEY_COUNT} OPS_PER_READER={OPS_PER_READER} \
         samples/arm={SAMPLES_PER_ARM} warmup={WARMUP} scratch={}",
        dir.display()
    );
    println!("arm reader_count sample throughput_melem_per_s is_warmup");

    for &n in READER_COUNTS {
        // One trie per arm, both alive, so the arms are interleaved per
        // replicate under identical conditions — cancels thermal/background-load
        // drift that an all-control-then-all-treatment order would bias the
        // comparison with. Tempdirs auto-removed on drop.
        let ctl_dir = tempfile::Builder::new()
            .prefix(&format!("exp_control_{n}"))
            .tempdir_in(&dir)
            .expect("control tempdir");
        let trt_dir = tempfile::Builder::new()
            .prefix(&format!("exp_treatment_{n}"))
            .tempdir_in(&dir)
            .expect("treatment tempdir");
        let ctl = build_trie(ctl_dir.path());
        let trt = build_trie(trt_dir.path());

        for s in 0..SAMPLES_PER_ARM {
            let used = dir_size(&dir);
            if used > SCRATCH_CEILING_BYTES {
                eprintln!(
                    "# ABORT: scratch dir {} exceeded ceiling ({} > {} bytes)",
                    dir.display(),
                    used,
                    SCRATCH_CEILING_BYTES
                );
                std::process::exit(2);
            }
            let warm = s < WARMUP;
            // Interleave: control then treatment for the same replicate index.
            let c = sample_throughput(&ctl, n, Arm::Control);
            println!("control {} {} {:.6} {}", n, s, c, warm);
            let t = sample_throughput(&trt, n, Arm::Treatment);
            println!("treatment {} {} {:.6} {}", n, s, t, warm);
        }
    }
    eprintln!("# done");
}
