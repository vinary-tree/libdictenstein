//! Lock-free flip benchmark — CONTROL (owned `Arc<RwLock>` tree + write→read-
//! downgrade checkpoint) vs TREATMENT (lock-free overlay `insert_cas_durable` +
//! immutable-snapshot checkpoint), under a concurrent W-writer + R-reader + 1-
//! checkpointer workload.
//!
//! This is the PRE-REGISTERED experiment for the irreversible "lock-free flip"
//! go/no-go. The full design (hypothesis, metrics, fixed design, analysis plan,
//! stopping rule) is frozen in
//! `docs/experiments/lockfree-flip-benchmark-ledger.md`. NO flip is performed;
//! both paths already exist in the crate. The TREATMENT checkpoint is reached
//! via the reversible `bench-internals`-gated `bench_immutable_checkpoint`.
//!
//! It does NOT use the criterion statistical machinery for the primary metric:
//! instead it runs exactly `K` measured rounds per arm and writes one CSV line
//! per round to a log, so the per-round throughputs can be analyzed offline with
//! the pre-registered Welch's t-test (run once, analyze offline — no re-running
//! for different output slices). A tiny criterion harness wraps it only to be a
//! cargo `[[bench]]` target; the real output is the teed stdout CSV.
//!
//! Run (pinned, capped):
//! ```bash
//! mkdir -p target/test-tmp target/bench-scratch
//! systemd-run --user --scope -p MemoryMax=32G --quiet \
//!   env TMPDIR=$PWD/target/test-tmp \
//!   taskset -c 0-15 \
//!   cargo bench --bench lockfree_flip_benchmark \
//!     --features persistent-artrie,bench-internals -- --measure \
//!   | tee docs/experiments/lockfree-flip-raw-combined.log
//! ```

#![cfg(all(feature = "persistent-artrie", feature = "bench-internals"))]

use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use libdictenstein::artrie_trait::EvictableARTrie;
use libdictenstein::persistent_artrie::eviction::EvictionConfig;
use libdictenstein::persistent_artrie::WalConfig;
use libdictenstein::persistent_artrie_char::{PersistentARTrieChar, SharedCharARTrie};
use libdictenstein::persistent_artrie_core::durability::DurabilityPolicy;
use libdictenstein::{ARTrie, Dictionary};

// ───────────────────────────── FROZEN DESIGN CONSTANTS ─────────────────────────────
// (mirror docs/experiments/lockfree-flip-benchmark-ledger.md §5)

const W_WRITERS: usize = 8;
const R_READERS: usize = 4;
// CALIBRATION NOTE (pre-data, see ledger §5 amendment): each CONTROL checkpoint
// re-serializes the reachable tree into FRESHLY-allocated arena blocks (the
// `.artc` data file grows monotonically per checkpoint — copy-on-serialize), so
// op-count × checkpoint-count must be bounded or the data file balloons (a
// single 20k×8-write / ~1000-checkpoint round grew the file to 16 GB). The
// constants below bound each round's data file to well under ~1 GB.
const WRITES_PER_ROUND: usize = 2_000; // per writer thread → 16k writes/round
                                       // FROZEN at K=30 (ledger §8): power for two-sided Welch α=0.05 to detect the
                                       // pre-registered effect floor d=0.8 at ≈0.9 power needs ≈27/arm → K=30 margin.
                                       // This is a pre-registered stopping rule — DO NOT change post-data.
const K_MEASURED_ROUNDS: usize = 30;
const WARMUP_ROUNDS: usize = 2;
const CHECKPOINT_THROTTLE: Duration = Duration::from_millis(20);
/// Safety cap on checkpoints per round (bounds data-file growth; each checkpoint
/// re-serializes the tree into fresh arenas). The checkpointer otherwise loops
/// `checkpoint + throttle` for the WHOLE writer window (so checkpoints overlap
/// the entire timed region), stopping when writers finish or this cap is hit.
const MAX_CHECKPOINTS_PER_ROUND: usize = 40;
const READS_PER_LOOP: usize = 2_000;
/// Fixed PRNG seed (SplitMix64) — deterministic key generation, both arms.
const PRNG_SEED: u64 = 0x9E37_79B9_7F4A_7C15;
/// Hard scratch ceiling (bytes). Abort a round if `target/bench-scratch` exceeds.
const SCRATCH_CEILING_BYTES: u64 = 5 * 1024 * 1024 * 1024; // 5 GiB

/// SplitMix64 — tiny deterministic PRNG (no external dep, reproducible).
#[inline]
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Workload variant (ledger §4). `Disjoint` = variant A (PRIMARY, overlay best
/// case: each writer owns a disjoint prefix range). `Contended` = variant B
/// (SECONDARY/S4: all writers under ONE hot prefix → max CONTROL lock contention
/// AND TREATMENT root-CAS retries — the most likely TREATMENT regression site).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Variant {
    Disjoint,
    Contended,
}

impl Variant {
    fn label(self) -> &'static str {
        match self {
            Variant::Disjoint => "disjoint",
            Variant::Contended => "contended",
        }
    }
    /// Build the key for (writer `t`, op `i`) under this variant.
    #[inline]
    fn key(self, t: usize, i: usize) -> String {
        match self {
            Variant::Disjoint => writer_key(t, i),
            Variant::Contended => writer_key_hot(t, i),
        }
    }
}

/// Which arm a single-process run measures (ledger §11.1(b), fixes the v1 RSS
/// VmHWM cross-contamination bug C10). `Both` = the default interleaved K-round
/// run; `Control`/`Treatment` = single-arm-per-process (RSS + perf passes).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ArmSelect {
    Both,
    Control,
    Treatment,
}

/// Deterministic distinct key for (writer `t`, op `i`). Disjoint per-writer
/// range (offset by `t * WRITES_PER_ROUND`) salted through SplitMix64 so the
/// exact same 160k keys are inserted in the same per-thread order every round
/// for BOTH arms. Multi-byte UTF-8 suffix exercises the char path.
#[inline]
fn writer_key(t: usize, i: usize) -> String {
    let base = (t * WRITES_PER_ROUND + i) as u64;
    let mut s = base ^ PRNG_SEED;
    let h = splitmix64(&mut s);
    format!("ngram-{:016x}-語{}", h, base)
}

/// Variant B (CONTENDED hot prefix, ledger §4). ALL writers insert under ONE
/// shared fixed prefix + a disjoint SplitMix64-salted suffix, so every term is
/// still new (monotone-final, `lockfree_cas.rs:300-321`) but they all collide on
/// the same root spine → maximal CONTROL write-lock contention AND TREATMENT
/// root-CAS retries / Arc-refcount ping-pong. Same key set + per-thread order
/// for both arms every round.
#[inline]
fn writer_key_hot(t: usize, i: usize) -> String {
    let base = (t * WRITES_PER_ROUND + i) as u64;
    let mut s = base ^ PRNG_SEED;
    let h = splitmix64(&mut s);
    format!("hot-prefix-fixed/{:08x}", h)
}

/// A key that is guaranteed present for readers to look up (read the lowest
/// few keys of writer 0's range — they get inserted early). Readers are LOAD,
/// not the timed primary, so hit/miss does not bias the writer measurement.
#[inline]
fn reader_key(j: usize) -> String {
    writer_key(0, j % 64)
}

// ── §F (--evict-real): the COLD prefix set (real overlay reclamation) ──────────
// The fixed COLD term family: inserted + checkpointed ONCE at round start, then
// NEVER re-touched. `--evict-real` feeds ONLY these to the evictor (cold-only, so
// the absent fault-in path is never needed; SF5(ii) `faultin_count == 0`). They
// share a `cold-` prefix disjoint from every writer key (`ngram-`/`hot-prefix-`/
// `warm-`), so the evictor and the live writers operate on disjoint subtrees.
const COLD_PREFIX_CHAR: char = 'c';
const COLD_SET_SIZE: usize = 4_000;

/// Deterministic COLD key `i`. Multi-char `cold-…` so each sits well below the
/// default `min_eviction_depth` and forms an evictable subtree.
#[inline]
fn cold_key(i: usize) -> String {
    let mut s = (i as u64) ^ PRNG_SEED ^ 0xC01D_C01D_C01D_C01D;
    let h = splitmix64(&mut s);
    format!("cold-{h:016x}-{i:05}")
}

/// The `cold_filter` for [`bench_evict_overlay_cold_nodes`]: a registry path is
/// COLD iff its first edge is the COLD prefix char. ONLY cold subtrees are ever
/// evicted (never a re-touchable LIVE node).
#[inline]
fn is_cold_path(path: &[char]) -> bool {
    path.first() == Some(&COLD_PREFIX_CHAR)
}

/// Real-disk scratch base under `target/` (NEVER `/tmp` = tmpfs on this host).
fn scratch_base() -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/bench-scratch");
    std::fs::create_dir_all(&p).expect("create real-disk scratch base");
    p
}

/// `du`-equivalent recursive byte size of a dir (scratch ceiling guard).
fn dir_size_bytes(path: &Path) -> u64 {
    fn walk(p: &Path, acc: &mut u64) {
        if let Ok(rd) = std::fs::read_dir(p) {
            for entry in rd.flatten() {
                let md = match entry.metadata() {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                if md.is_dir() {
                    walk(&entry.path(), acc);
                } else {
                    *acc += md.len();
                }
            }
        }
    }
    let mut acc = 0u64;
    walk(path, &mut acc);
    acc
}

/// Process peak RSS (VmHWM) in KiB from /proc/self/status.
fn peak_rss_kib() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            let kib = rest
                .split_whitespace()
                .next()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0);
            return kib;
        }
    }
    0
}

/// `q`-quantile (0.0..=1.0) of a slice of nanosecond latencies (sort on a clone).
fn quantile_nanos(samples: &[u64], q: f64) -> u64 {
    if samples.is_empty() {
        return 0;
    }
    let mut v = samples.to_vec();
    v.sort_unstable();
    let idx = ((v.len() as f64) * q).ceil() as usize;
    let idx = idx.saturating_sub(1).min(v.len() - 1);
    v[idx]
}

/// p99 of a slice of nanosecond latencies (compat shim over `quantile_nanos`).
fn p99_nanos(samples: &[u64]) -> u64 {
    quantile_nanos(samples, 0.99)
}

/// Per-round measurement record (one CSV line). Column order is FROZEN to the
/// ledger §7 schema (the analysis script `analyze_lockfree_flip.py` parses by
/// this exact header):
/// `arm,round,is_warmup,write_ops,elapsed_secs,write_ops_per_sec,read_ops,
///  checkpoint_rounds,ckpt_pause_mean_us,ckpt_pause_p99_us,write_p50_us,
///  write_p99_us,write_p999_us,cas_retries,peak_rss_kib,round_dir_bytes,
///  variant,durability`.
struct RoundResult {
    arm: &'static str,
    round: usize,
    is_warmup: bool,
    write_ops: u64,
    elapsed_secs: f64,
    write_ops_per_sec: f64,
    read_ops: u64,
    checkpoint_rounds: u64,
    checkpoint_pause_mean_us: f64,
    checkpoint_pause_p99_us: f64,
    write_p50_us: f64,
    write_p99_us: f64,
    write_p999_us: f64,
    cas_retries: u64,
    peak_rss_kib: u64,
    variant: &'static str,
    durability: &'static str,
    /// EVICTION-ON (`--eviction`) only: the published `evictable_node_count()`
    /// observed at the end of the round (registry size). 0 when eviction is off
    /// (design §E). Emitted as a TRAILING CSV column so the FROZEN §7 18-column
    /// schema + the frozen `analyze_lockfree_flip.py` (which maps by header name
    /// and `min(len(fields), len(header))`-truncates) parse unchanged.
    evictable_node_count: u64,
    /// §F (`--evict-real`) col 20: COLD overlay (TREATMENT) / owned-tree (CONTROL)
    /// nodes ACTUALLY reclaimed by the matched per-checkpoint eviction this round.
    /// 0 unless `--evict-real`. The honest reclamation witness (HF3).
    overlay_reclaimed_nodes: u64,
    /// §F col 21: nominal bytes freed (registry `size_bytes`-style sum / ~256 B per
    /// node estimate). Nominal — the single-arm peak-RSS pass is the physical
    /// witness (HF2). 0 unless `--evict-real`.
    evict_bytes_nominal: u64,
    /// §F col 22: fault-in operation count. Identically 0 — the overlay has NO
    /// fault-in path (the cold-only invariant), so evicting a COLD subtree never
    /// triggers a disk read-back. Any non-zero value would mean a hot node was
    /// wrongly evicted; the real correctness gate is the reopen-exact SF5 check.
    faultin_count: u64,
}

impl RoundResult {
    /// Emit the full canonical §7 CSV line. `round_dir_bytes` is supplied by the
    /// driver (measured AFTER the round) and slotted between `peak_rss_kib` and
    /// `variant` to keep the frozen column order the analysis script expects.
    /// `evictable_node_count` is appended as a TRAILING 19th column (design §E);
    /// the frozen analysis maps the leading 18 by name and ignores the extra.
    fn csv_line(&self, round_dir_bytes: u64) -> String {
        format!(
            "{arm},{round},{warm},{wops},{secs:.6},{ops_s:.1},{rops},{ckpts},\
             {cmean:.1},{cp99:.1},{wp50:.1},{wp99:.1},{wp999:.1},{cas},{rss},\
             {dirb},{variant},{dur},{evict},{reclaimed},{ebytes},{faultin}",
            arm = self.arm,
            round = self.round,
            warm = self.is_warmup as u8,
            wops = self.write_ops,
            secs = self.elapsed_secs,
            ops_s = self.write_ops_per_sec,
            rops = self.read_ops,
            ckpts = self.checkpoint_rounds,
            cmean = self.checkpoint_pause_mean_us,
            cp99 = self.checkpoint_pause_p99_us,
            wp50 = self.write_p50_us,
            wp99 = self.write_p99_us,
            wp999 = self.write_p999_us,
            cas = self.cas_retries,
            rss = self.peak_rss_kib,
            dirb = round_dir_bytes,
            variant = self.variant,
            dur = self.durability,
            evict = self.evictable_node_count,
            reclaimed = self.overlay_reclaimed_nodes,
            ebytes = self.evict_bytes_nominal,
            faultin = self.faultin_count,
        )
    }
}

// ───────────────────────────── CONTROL ARM ─────────────────────────────

/// One CONTROL round: build a fresh `SharedCharARTrie<()>` under `Immediate`
/// durability, run W writers (`insert_with_value` — write lock per op), R readers
/// (`contains` — read lock), and 1 checkpointer (`ARTrie::checkpoint` — the
/// non-blocking write→read-downgrade production path). Times the writers.
fn control_round(
    round: usize,
    is_warmup: bool,
    scratch_round: &Path,
    variant: Variant,
    eviction: bool,
    evict_real: bool,
) -> RoundResult {
    let path = scratch_round.join("control.artc");
    // `no_archive`: checkpoints TRUNCATE the WAL (bounded, space-reclaiming
    // production mode) rather than the default archive-on-checkpoint (which grows
    // disk per checkpoint under the tight checkpoint loop). The default `create`
    // path's `WalConfig::default()` has `archive_enabled = true`.
    let mut owned: PersistentARTrieChar<()> =
        PersistentARTrieChar::create_with_config(&path, WalConfig::no_archive())
            .expect("create control trie");
    owned.set_durability_policy(DurabilityPolicy::Immediate);
    // SharedCharARTrie = Arc<RwLock<PersistentARTrieChar>>; this is the
    // production handle whose checkpoint is the non-blocking downgrade path.
    let trie: SharedCharARTrie<()> = Arc::new(parking_lot::RwLock::new(owned));

    // EVICTION-ON (`--eviction`, design §E): enable eviction on the owned tree.
    // The CONTROL checkpoint (`ARTrie::checkpoint` → `publish_durable_and_reclaim`,
    // mod.rs:1304) then PUBLISHES the registry (coordinator.rs:123-127) after
    // verify — the owned-tree eviction-ON checkpoint being compared. Deterministic
    // (`without_memory_monitor`).
    if eviction {
        trie.enable_eviction(EvictionConfig::without_memory_monitor())
            .expect("control enable_eviction");
    }

    // §F (--evict-real): seed the fixed COLD set ONCE + checkpoint it durable, so
    // the registry holds real owned-tree disk pointers for the cold subtrees, then
    // NEVER touch COLD again. `force_eviction` (owned tree) then genuinely reclaims
    // these cold in-memory boxes after each checkpoint (matched to TREATMENT).
    if evict_real {
        for i in 0..COLD_SET_SIZE {
            let _ = ARTrie::insert(&trie, &cold_key(i));
        }
        ARTrie::checkpoint(&trie).expect("control cold-seed checkpoint");
    }

    let stop = Arc::new(AtomicBool::new(false));
    let read_ops = Arc::new(AtomicU64::new(0));
    let ckpt_rounds = Arc::new(AtomicU64::new(0));
    let ckpt_pauses_ns: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::with_capacity(256)));
    // §F: matched reclamation accounting (overlay_reclaimed_nodes / bytes).
    let reclaimed_nodes = Arc::new(AtomicU64::new(0));
    let reclaimed_bytes = Arc::new(AtomicU64::new(0));
    // +1 main, +R readers, +1 checkpointer, +W writers.
    let barrier = Arc::new(Barrier::new(W_WRITERS + R_READERS + 1 + 1));

    // Writers (timed primary): each owns a disjoint key range; record per-op latency.
    let mut writers = Vec::with_capacity(W_WRITERS);
    for t in 0..W_WRITERS {
        let trie = Arc::clone(&trie);
        let barrier = Arc::clone(&barrier);
        writers.push(thread::spawn(move || {
            let mut lat = Vec::with_capacity(WRITES_PER_ROUND);
            barrier.wait();
            for i in 0..WRITES_PER_ROUND {
                let key = variant.key(t, i);
                let t0 = Instant::now();
                // insert_with_value takes the trie write lock per op (CONTROL).
                let _ = ARTrie::insert_with_value(&trie, &key, ());
                lat.push(t0.elapsed().as_nanos() as u64);
            }
            lat
        }));
    }

    // Readers (load): loop contains until writers stop.
    let mut readers = Vec::with_capacity(R_READERS);
    for _r in 0..R_READERS {
        let trie = Arc::clone(&trie);
        let barrier = Arc::clone(&barrier);
        let stop = Arc::clone(&stop);
        let read_ops = Arc::clone(&read_ops);
        readers.push(thread::spawn(move || {
            barrier.wait();
            let mut local = 0u64;
            while !stop.load(Ordering::Relaxed) {
                for j in 0..READS_PER_LOOP {
                    if Dictionary::contains(&trie, &reader_key(j)) {
                        local += 1;
                    }
                }
            }
            read_ops.fetch_add(local, Ordering::Relaxed);
            black_box(local);
        }));
    }

    // Checkpointer: loop the non-blocking production checkpoint until writers stop.
    let checkpointer = {
        let trie = Arc::clone(&trie);
        let barrier = Arc::clone(&barrier);
        let stop = Arc::clone(&stop);
        let ckpt_rounds = Arc::clone(&ckpt_rounds);
        let ckpt_pauses_ns = Arc::clone(&ckpt_pauses_ns);
        let reclaimed_nodes = Arc::clone(&reclaimed_nodes);
        let reclaimed_bytes = Arc::clone(&reclaimed_bytes);
        thread::spawn(move || {
            barrier.wait();
            let mut rounds = 0u64;
            let mut pauses = Vec::with_capacity(MAX_CHECKPOINTS_PER_ROUND);
            // Loop checkpoints across the WHOLE writer window so the checkpoint
            // overlaps the entire timed region; stop when writers finish (`stop`)
            // or the disk-bounding cap is reached.
            while !stop.load(Ordering::Relaxed) && (rounds as usize) < MAX_CHECKPOINTS_PER_ROUND {
                let t0 = Instant::now();
                let _ = ARTrie::checkpoint(&trie);
                pauses.push(t0.elapsed().as_nanos() as u64);
                rounds += 1;
                // §F (--evict-real): MATCHED owned-tree reclamation AFTER the
                // checkpoint publishes the registry. The cold subtrees are durable
                // (cold-seed checkpoint), so `force_eviction` genuinely reclaims
                // their in-memory boxes. A concurrent writer may invalidate the
                // registry → 0 this round (liveness-not-safety), matched to
                // TREATMENT's same behavior.
                if evict_real {
                    if let Ok((n, b)) = trie.force_eviction(1 << 20) {
                        reclaimed_nodes.fetch_add(n as u64, Ordering::Relaxed);
                        reclaimed_bytes.fetch_add(b as u64, Ordering::Relaxed);
                    }
                }
                thread::sleep(CHECKPOINT_THROTTLE);
            }
            ckpt_rounds.store(rounds, Ordering::Relaxed);
            *ckpt_pauses_ns.lock().expect("ckpt pauses lock") = pauses;
        })
    };

    barrier.wait();
    let start = Instant::now();
    let mut all_lat: Vec<u64> = Vec::with_capacity(W_WRITERS * WRITES_PER_ROUND);
    for w in writers {
        all_lat.extend(w.join().expect("writer join"));
    }
    let elapsed = start.elapsed();
    stop.store(true, Ordering::Relaxed);
    for r in readers {
        let _ = r.join();
    }
    let _ = checkpointer.join();

    // EVICTION-ON: record the published registry size, then disable eviction so
    // the background eviction thread is joined before the Arc drops.
    let evict_count = if eviction {
        let n = trie.read().evictable_node_count().unwrap_or(0) as u64;
        let _ = trie.disable_eviction();
        n
    } else {
        0
    };

    finalize(
        "control",
        round,
        is_warmup,
        elapsed,
        &all_lat,
        &read_ops,
        &ckpt_rounds,
        &ckpt_pauses_ns,
        0, // CONTROL has no lock-free CAS path → zero retries.
        variant.label(),
        evict_count,
        reclaimed_nodes.load(Ordering::Relaxed),
        reclaimed_bytes.load(Ordering::Relaxed),
        // CONTROL owned-tree eviction faults nodes back in on read; but COLD is
        // never re-read, so no fault-in occurs. Recorded 0 (the reopen-exact SF5
        // check is the real correctness gate).
        0,
    )
}

// ───────────────────────────── TREATMENT ARM ─────────────────────────────

/// One TREATMENT round: build a fresh `PersistentARTrieChar<()>` under `Immediate`
/// durability, `enable_lockfree`, share via a bare `Arc` (NO RwLock). W writers
/// (`insert_cas_durable` — Order-A, no lock), R readers (`contains_lockfree`), and
/// 1 checkpointer (`bench_immutable_checkpoint` — immutable snapshot, no writer
/// lock). Times the writers.
fn treatment_round(
    round: usize,
    is_warmup: bool,
    scratch_round: &Path,
    variant: Variant,
    eviction: bool,
    evict_real: bool,
) -> RoundResult {
    let path = scratch_round.join("treatment.artc");
    // `no_archive` for parity with the CONTROL arm's WAL config. NOTE: the
    // TREATMENT checkpoint (`publish_immutable_snapshot_retaining_wal[_with_eviction]`)
    // RETAINS the WAL by design (watermark-reclaim safety) regardless of this flag,
    // so its WAL still grows with the round's inserts (bounded: 160k records/round).
    let mut owned: PersistentARTrieChar<()> =
        PersistentARTrieChar::create_with_config(&path, WalConfig::no_archive())
            .expect("create treatment trie");
    owned.set_durability_policy(DurabilityPolicy::Immediate);
    owned.enable_lockfree();
    // EVICTION-ON (`--eviction`, design §E): install the coordinator on the BARE
    // trie (before the Arc wrap — `bench_enable_eviction` takes `&mut self`). The
    // TREATMENT checkpoint (`bench_immutable_checkpoint_with_eviction`) then builds
    // the registry over the overlay snapshot + publishes it (no writer lock). The
    // reclaim callback is a structural no-op over the overlay (owned root empty),
    // matching the proven T1 correspondence behavior. Deterministic.
    if eviction {
        owned
            .bench_enable_eviction(EvictionConfig::without_memory_monitor())
            .expect("treatment bench_enable_eviction");
    }
    // Bare Arc — the lock-free path needs only &self; the struct is Send+Sync.
    let trie: Arc<PersistentARTrieChar<()>> = Arc::new(owned);

    // §F (--evict-real): seed the fixed COLD set ONCE into the OVERLAY + checkpoint
    // it (publishing the overlay registry with real disk pointers for the cold
    // subtrees), then NEVER touch COLD again. `bench_evict_overlay_cold_nodes` then
    // genuinely reclaims those cold overlay nodes after each checkpoint — the REAL
    // reclamation that the §E no-op never did.
    if evict_real {
        for i in 0..COLD_SET_SIZE {
            let _ = trie
                .insert_cas_durable(&cold_key(i))
                .expect("cold-seed insert");
        }
        trie.bench_immutable_checkpoint_with_eviction()
            .expect("treatment cold-seed checkpoint");
    }

    let stop = Arc::new(AtomicBool::new(false));
    let read_ops = Arc::new(AtomicU64::new(0));
    let ckpt_rounds = Arc::new(AtomicU64::new(0));
    let ckpt_pauses_ns: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::with_capacity(256)));
    // §F: matched reclamation accounting (overlay_reclaimed_nodes / bytes).
    let reclaimed_nodes = Arc::new(AtomicU64::new(0));
    let reclaimed_bytes = Arc::new(AtomicU64::new(0));
    let barrier = Arc::new(Barrier::new(W_WRITERS + R_READERS + 1 + 1));

    let mut writers = Vec::with_capacity(W_WRITERS);
    for t in 0..W_WRITERS {
        let trie = Arc::clone(&trie);
        let barrier = Arc::clone(&barrier);
        writers.push(thread::spawn(move || {
            let mut lat = Vec::with_capacity(WRITES_PER_ROUND);
            barrier.wait();
            for i in 0..WRITES_PER_ROUND {
                let key = variant.key(t, i);
                let t0 = Instant::now();
                // Order-A durable lock-free insert (TREATMENT) — no write lock.
                let _ = trie.insert_cas_durable(&key).expect("insert_cas_durable");
                lat.push(t0.elapsed().as_nanos() as u64);
            }
            lat
        }));
    }

    let mut readers = Vec::with_capacity(R_READERS);
    for _r in 0..R_READERS {
        let trie = Arc::clone(&trie);
        let barrier = Arc::clone(&barrier);
        let stop = Arc::clone(&stop);
        let read_ops = Arc::clone(&read_ops);
        readers.push(thread::spawn(move || {
            barrier.wait();
            let mut local = 0u64;
            while !stop.load(Ordering::Relaxed) {
                for j in 0..READS_PER_LOOP {
                    if trie.contains_lockfree(&reader_key(j)) {
                        local += 1;
                    }
                }
            }
            read_ops.fetch_add(local, Ordering::Relaxed);
            black_box(local);
        }));
    }

    let checkpointer = {
        let trie = Arc::clone(&trie);
        let barrier = Arc::clone(&barrier);
        let stop = Arc::clone(&stop);
        let ckpt_rounds = Arc::clone(&ckpt_rounds);
        let ckpt_pauses_ns = Arc::clone(&ckpt_pauses_ns);
        let reclaimed_nodes = Arc::clone(&reclaimed_nodes);
        let reclaimed_bytes = Arc::clone(&reclaimed_bytes);
        thread::spawn(move || {
            barrier.wait();
            let mut rounds = 0u64;
            let mut pauses = Vec::with_capacity(MAX_CHECKPOINTS_PER_ROUND);
            while !stop.load(Ordering::Relaxed) && (rounds as usize) < MAX_CHECKPOINTS_PER_ROUND {
                let t0 = Instant::now();
                // Immutable-snapshot checkpoint (TREATMENT) — no writer-excluding
                // lock. Eviction-ON routes to the registry-publishing variant.
                let _ = if eviction {
                    trie.bench_immutable_checkpoint_with_eviction()
                } else {
                    trie.bench_immutable_checkpoint()
                };
                pauses.push(t0.elapsed().as_nanos() as u64);
                rounds += 1;
                // §F (--evict-real): MATCHED COLD overlay reclamation AFTER the
                // checkpoint publishes the registry — the REAL reclamation (vs the
                // §E no-op). Cold-only (`is_cold_path`): a LIVE node is never
                // evicted. A concurrent writer may invalidate the registry → 0 this
                // round (liveness-not-safety), matched to CONTROL.
                if evict_real {
                    let n = trie.bench_evict_overlay_cold_nodes(1 << 20, is_cold_path);
                    reclaimed_nodes.fetch_add(n as u64, Ordering::Relaxed);
                    // Nominal ~256 B/node (the single-arm peak-RSS pass is the
                    // physical witness, HF2).
                    reclaimed_bytes.fetch_add((n as u64) * 256, Ordering::Relaxed);
                }
                thread::sleep(CHECKPOINT_THROTTLE);
            }
            ckpt_rounds.store(rounds, Ordering::Relaxed);
            *ckpt_pauses_ns.lock().expect("ckpt pauses lock") = pauses;
        })
    };

    barrier.wait();
    let start = Instant::now();
    let mut all_lat: Vec<u64> = Vec::with_capacity(W_WRITERS * WRITES_PER_ROUND);
    for w in writers {
        all_lat.extend(w.join().expect("writer join"));
    }
    let elapsed = start.elapsed();
    stop.store(true, Ordering::Relaxed);
    // Capture this round's CAS-retry count (cumulative on the fresh-per-round
    // trie → equals this round's retries). The accessor reads a Relaxed atomic;
    // all writers have joined so it is a stable post-join read.
    let cas_retries = trie.cas_retry_count();
    for r in readers {
        let _ = r.join();
    }
    let _ = checkpointer.join();

    // EVICTION-ON: record the published registry size (post-join stable read).
    // The coordinator's eviction thread is joined automatically when this round's
    // `trie` Arc drops (`PersistentARTrieChar::Drop` → `close()` → shutdown).
    let evict_count = if eviction {
        trie.evictable_node_count().unwrap_or(0) as u64
    } else {
        0
    };

    finalize(
        "treatment",
        round,
        is_warmup,
        elapsed,
        &all_lat,
        &read_ops,
        &ckpt_rounds,
        &ckpt_pauses_ns,
        cas_retries,
        variant.label(),
        evict_count,
        reclaimed_nodes.load(Ordering::Relaxed),
        reclaimed_bytes.load(Ordering::Relaxed),
        // The overlay has NO fault-in path; evicting COLD never reads back from
        // disk, and COLD is never re-read → faultin_count is identically 0 (the
        // cold-only invariant). The reopen-exact SF5 check is the correctness gate.
        0,
    )
}

#[allow(clippy::too_many_arguments)]
fn finalize(
    arm: &'static str,
    round: usize,
    is_warmup: bool,
    elapsed: Duration,
    all_lat: &[u64],
    read_ops: &AtomicU64,
    ckpt_rounds: &AtomicU64,
    ckpt_pauses_ns: &Mutex<Vec<u64>>,
    cas_retries: u64,
    variant: &'static str,
    evictable_node_count: u64,
    overlay_reclaimed_nodes: u64,
    evict_bytes_nominal: u64,
    faultin_count: u64,
) -> RoundResult {
    let write_ops = (W_WRITERS * WRITES_PER_ROUND) as u64;
    let elapsed_secs = elapsed.as_secs_f64();
    let write_ops_per_sec = write_ops as f64 / elapsed_secs;
    let pauses = ckpt_pauses_ns.lock().expect("ckpt pauses lock").clone();
    let ckpt_n = pauses.len().max(1) as f64;
    let ckpt_mean_us = pauses.iter().map(|&n| n as f64).sum::<f64>() / ckpt_n / 1000.0;
    let ckpt_p99_us = p99_nanos(&pauses) as f64 / 1000.0;
    let write_p50_us = quantile_nanos(all_lat, 0.50) as f64 / 1000.0;
    let write_p99_us = quantile_nanos(all_lat, 0.99) as f64 / 1000.0;
    let write_p999_us = quantile_nanos(all_lat, 0.999) as f64 / 1000.0;
    RoundResult {
        arm,
        round,
        is_warmup,
        write_ops,
        elapsed_secs,
        write_ops_per_sec,
        read_ops: read_ops.load(Ordering::Relaxed),
        checkpoint_rounds: ckpt_rounds.load(Ordering::Relaxed),
        checkpoint_pause_mean_us: ckpt_mean_us,
        checkpoint_pause_p99_us: ckpt_p99_us,
        write_p50_us,
        write_p99_us,
        write_p999_us,
        cas_retries,
        peak_rss_kib: peak_rss_kib(),
        // Both arms run under DurabilityPolicy::Immediate (ledger §2 durability
        // parity); recorded for the analysis schema / provenance.
        durability: "Immediate",
        variant,
        evictable_node_count,
        overlay_reclaimed_nodes,
        evict_bytes_nominal,
        faultin_count,
    }
}

// ───────────────────────────── DRIVER ─────────────────────────────

/// The frozen §7 CSV header (printed once per measured run). Mirrors
/// `RoundResult::csv_line` field order EXACTLY; `analyze_lockfree_flip.py`
/// parses by these names.
const CSV_HEADER: &str = "csv_header,arm,round,is_warmup,write_ops,elapsed_secs,\
     write_ops_per_sec,read_ops,checkpoint_rounds,ckpt_pause_mean_us,\
     ckpt_pause_p99_us,write_p50_us,write_p99_us,write_p999_us,cas_retries,\
     peak_rss_kib,round_dir_bytes,variant,durability,evictable_node_count,\
     overlay_reclaimed_nodes,evict_bytes_nominal,faultin_count";

/// Run one round of one arm, emit its CSV line, clean its scratch, and enforce
/// the 5 GiB scratch ceiling pre+post. Returns `false` if a ceiling abort fired.
#[allow(clippy::too_many_arguments)]
fn run_one_round(
    arm: &'static str,
    round: usize,
    is_warmup: bool,
    variant: Variant,
    eviction: bool,
    evict_real: bool,
    base: &Path,
    counter: &AtomicUsize,
) -> bool {
    use std::io::Write;
    let n = counter.fetch_add(1, Ordering::Relaxed);
    let scratch_round = base.join(format!("r{round}_{arm}_{n}"));
    std::fs::create_dir_all(&scratch_round).expect("round scratch dir");

    // PRE-ROUND ceiling guard.
    let pre_sz = dir_size_bytes(base);
    if pre_sz > SCRATCH_CEILING_BYTES {
        eprintln!("ABORT(pre): scratch {pre_sz} B exceeds ceiling {SCRATCH_CEILING_BYTES} B");
        return false;
    }

    let res = match arm {
        "control" => control_round(
            round,
            is_warmup,
            &scratch_round,
            variant,
            eviction,
            evict_real,
        ),
        _ => treatment_round(
            round,
            is_warmup,
            &scratch_round,
            variant,
            eviction,
            evict_real,
        ),
    };
    let round_dir_bytes = dir_size_bytes(&scratch_round);
    println!("csv,{}", res.csv_line(round_dir_bytes));
    let _ = std::io::stdout().flush();

    // Clean this round's scratch immediately (bounded disk).
    let _ = std::fs::remove_dir_all(&scratch_round);

    // POST-ROUND ceiling guard.
    let sz = dir_size_bytes(base);
    if sz > SCRATCH_CEILING_BYTES {
        eprintln!("ABORT(post): scratch {sz} B exceeds ceiling {SCRATCH_CEILING_BYTES} B");
        return false;
    }
    true
}

/// Run the full pre-registered experiment for one `variant`. `WARMUP_ROUNDS`
/// (unless `warmup` is false) + `rounds` measured rounds. When `arm_select` is
/// `Both`, both arms run each round with the within-round ORDER randomized by a
/// fixed-seed SplitMix64 coin (ledger §11.1(d)/C9 — fixes v1's always-control-
/// first bias while staying deterministic). When `Control`/`Treatment`, only
/// that arm runs each round (single-arm-per-process — RSS/perf passes, fixes
/// the v1 VmHWM cross-contamination bug C10). Emits one CSV line per round.
fn run_measured_experiment(
    variant: Variant,
    arm_select: ArmSelect,
    rounds: usize,
    warmup: bool,
    eviction: bool,
    evict_real: bool,
) {
    let base = scratch_base();
    // Clean any stale scratch from a prior run.
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).expect("recreate scratch base");

    let warmup_rounds = if warmup { WARMUP_ROUNDS } else { 0 };
    println!(
        "# lockfree_flip_benchmark MEASURED RUN  W={W_WRITERS} R={R_READERS} \
         writes/round={WRITES_PER_ROUND} K={rounds} warmup={warmup_rounds} \
         variant={} arm_select={:?} eviction={eviction} evict_real={evict_real}",
        variant.label(),
        arm_select
    );
    println!("{CSV_HEADER}");

    let total = warmup_rounds + rounds;
    let counter = AtomicUsize::new(0);
    // Fixed-seed coin stream (deterministic, reproducible CSVs).
    let mut coin_state: u64 = PRNG_SEED;

    'outer: for round in 0..total {
        let is_warmup = round < warmup_rounds;

        // Decide this round's arm order (only matters for `Both`). The low bit
        // of a fresh SplitMix64 draw → control-first vs treatment-first.
        let control_first = (splitmix64(&mut coin_state) & 1) == 0;
        let order: &[&'static str] = match arm_select {
            ArmSelect::Both if control_first => &["control", "treatment"],
            ArmSelect::Both => &["treatment", "control"],
            ArmSelect::Control => &["control"],
            ArmSelect::Treatment => &["treatment"],
        };

        for &arm in order {
            if !run_one_round(
                arm, round, is_warmup, variant, eviction, evict_real, &base, &counter,
            ) {
                break 'outer;
            }
        }
    }

    // Final cleanup.
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).ok();
    println!("# MEASURED RUN COMPLETE");
}

/// One bounded smoke round per arm × variant — validates BOTH paths run
/// concurrently on BOTH workloads with no panic and prints the footprint. SAFE:
/// exactly one round each (no iteration loop that could balloon scratch).
fn run_smoke(eviction: bool, evict_real: bool) {
    let base = scratch_base();
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).expect("recreate scratch base");
    println!(
        "# lockfree_flip_benchmark SMOKE (1 round/arm/variant) eviction={eviction} \
         evict_real={evict_real}"
    );
    for variant in [Variant::Disjoint, Variant::Contended] {
        for arm in ["control", "treatment"] {
            let dir = base.join(format!("smoke_{}_{arm}", variant.label()));
            std::fs::create_dir_all(&dir).expect("smoke dir");
            let r = match arm {
                "control" => control_round(0, true, &dir, variant, eviction, evict_real),
                _ => treatment_round(0, true, &dir, variant, eviction, evict_real),
            };
            let bytes = dir_size_bytes(&dir);
            println!(
                "smoke,{},{arm}: write_ops/s={:.0} ckpt_rounds={} \
                 ckpt_pause_mean_us={:.1} write_p50_us={:.1} write_p99_us={:.1} \
                 write_p999_us={:.1} cas_retries={} read_ops={} evictable={} \
                 reclaimed={} evict_bytes={} faultin={} dir_bytes={}",
                variant.label(),
                r.write_ops_per_sec,
                r.checkpoint_rounds,
                r.checkpoint_pause_mean_us,
                r.write_p50_us,
                r.write_p99_us,
                r.write_p999_us,
                r.cas_retries,
                r.read_ops,
                r.evictable_node_count,
                r.overlay_reclaimed_nodes,
                r.evict_bytes_nominal,
                r.faultin_count,
                bytes
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
    // EVICTION-ON: the SE5 correctness veto (design §E SE5). Post-checkpoint
    // force_eviction + reload MUST return the exact acknowledged set on BOTH arms;
    // a failure is a BUG (not a perf signal) and panics here so the smoke (which
    // gates the bench in CI) fails loudly.
    if eviction {
        se5_correctness_check(&base);
    }
    // §F (--evict-real): the SF5 REAL-reclamation correctness veto. Post-checkpoint
    // matched eviction + reopen MUST recover the exact acknowledged set on BOTH
    // arms, with TREATMENT reclaim > 0 and faultin == 0. A failure is a BUG, not a
    // perf signal — it panics here so the smoke fails loudly.
    if evict_real {
        sf5_correctness_check(&base);
    }
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).ok();
    println!("# SMOKE COMPLETE");
}

/// **SE5 — the eviction-ON correctness veto** (design §E SE5 / §8 gated-first):
/// for BOTH arms, after an eviction-ON checkpoint + a `force_eviction`, reopening
/// the trie must recover the EXACT acknowledged membership set. CONTROL exercises
/// the owned-tree eviction-ON checkpoint (`publish_durable_and_reclaim`) where
/// `force_eviction` genuinely reclaims in-memory boxes; TREATMENT exercises the
/// new `bench_immutable_checkpoint_with_eviction` publisher where `force_eviction`
/// is a structural no-op over the overlay (owned root empty) but the durable image
/// + retained WAL must still reopen losslessly. Panics on any mismatch (it is a
/// real bug, not a measurement) — the design says STOP and report.
fn se5_correctness_check(base: &Path) {
    // A tier-spanning, deterministic key set (mirrors the in-crate T1 set).
    let mut terms: Vec<String> = vec!["a", "ab", "abc", "b", "banana", "z", "日本", "🎉"]
        .into_iter()
        .map(String::from)
        .collect();
    for i in 0..40u32 {
        terms.push(format!("se5-{i:03}"));
    }

    // ── CONTROL arm (owned tree + publish_durable_and_reclaim) ──
    {
        let dir = base.join("se5_control");
        std::fs::create_dir_all(&dir).expect("se5 control dir");
        let path = dir.join("c.artc");
        {
            // Mirror `control_round`: build the bare trie with `no_archive`, then
            // wrap in the production `Arc<RwLock<…>>` handle (`SharedCharARTrie`).
            let mut owned: PersistentARTrieChar<()> =
                PersistentARTrieChar::create_with_config(&path, WalConfig::no_archive())
                    .expect("se5 control create");
            owned.set_durability_policy(DurabilityPolicy::Immediate);
            let trie: SharedCharARTrie<()> = Arc::new(parking_lot::RwLock::new(owned));
            trie.enable_eviction(EvictionConfig::without_memory_monitor())
                .expect("se5 control enable_eviction");
            for t in &terms {
                assert!(
                    ARTrie::insert(&trie, t),
                    "se5 control: insert {t:?} not newly added"
                );
            }
            ARTrie::checkpoint(&trie).expect("se5 control checkpoint");
            let (evicted, _b) = trie.force_eviction(1 << 20).expect("se5 control force");
            // Owned tree: eviction genuinely reclaims at least one node.
            assert!(
                evicted >= 1,
                "SE5(control): owned-tree force_eviction reclaimed 0 nodes (expected >=1)"
            );
            for t in &terms {
                assert!(
                    Dictionary::contains(&trie, t),
                    "SE5(control): term {t:?} unresolvable after eviction (reload broken)"
                );
            }
            trie.disable_eviction().expect("se5 control disable");
        }
        let reopened: SharedCharARTrie<()> = ARTrie::open(&path).expect("se5 control reopen");
        for t in &terms {
            assert!(
                Dictionary::contains(&reopened, t),
                "SE5(control): term {t:?} LOST after eviction-ON checkpoint reopen (BUG)"
            );
        }
        assert!(!Dictionary::contains(&reopened, "se5-absent"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── TREATMENT arm (overlay + bench_immutable_checkpoint_with_eviction) ──
    {
        let dir = base.join("se5_treatment");
        std::fs::create_dir_all(&dir).expect("se5 treatment dir");
        let path = dir.join("t.artc");
        {
            let mut owned: PersistentARTrieChar<()> =
                PersistentARTrieChar::create_with_config(&path, WalConfig::no_archive())
                    .expect("se5 treatment create");
            owned.set_durability_policy(DurabilityPolicy::Immediate);
            owned.enable_lockfree();
            owned
                .bench_enable_eviction(EvictionConfig::without_memory_monitor())
                .expect("se5 treatment bench_enable_eviction");
            let trie: Arc<PersistentARTrieChar<()>> = Arc::new(owned);
            for t in &terms {
                assert!(
                    trie.insert_cas_durable(t).expect("se5 treatment insert"),
                    "se5 treatment: insert {t:?} not newly added"
                );
            }
            trie.bench_immutable_checkpoint_with_eviction()
                .expect("se5 treatment checkpoint");
            assert!(
                trie.evictable_node_count().unwrap_or(0) > 0,
                "SE5(treatment): registry not published (evictable_node_count == 0)"
            );
            for t in &terms {
                assert!(
                    trie.contains_lockfree(t),
                    "SE5(treatment): term {t:?} unresolvable after checkpoint (overlay broken)"
                );
            }
            // The coordinator's eviction thread is joined when `trie` drops below.
            drop(trie);
        }
        let reopened = PersistentARTrieChar::<()>::open(&path).expect("se5 treatment reopen");
        for t in &terms {
            assert!(
                Dictionary::contains(&reopened, t),
                "SE5(treatment): term {t:?} LOST after eviction-ON checkpoint reopen (BUG)"
            );
        }
        assert!(!Dictionary::contains(&reopened, "se5-absent"));
        let _ = std::fs::remove_dir_all(&dir);
    }
    println!("smoke,SE5: correctness check PASSED (both arms reopen the exact acknowledged set)");
}

/// **SF5 — the REAL-reclamation correctness veto** (design §F SF5 / §F.7
/// `sf5_correctness_check`; gated FIRST before any §F perf verdict): for BOTH arms,
/// after an eviction-ON checkpoint + MATCHED real eviction, reopening the trie MUST
/// recover the EXACT acknowledged set (COLD ∪ LIVE), with (ii) `faultin_count == 0`
/// and (iii) TREATMENT `overlay_reclaimed_nodes > 0` (no silent no-op). CONTROL
/// uses the owned-tree `force_eviction`; TREATMENT uses
/// `bench_evict_overlay_cold_nodes` (the §F driver — REAL overlay reclamation, not
/// the §E no-op). Panics on any mismatch (a real BUG, not a measurement — the
/// design says STOP and report).
fn sf5_correctness_check(base: &Path) {
    // Disjoint COLD (`cold-*`) + LIVE (`live-*`) families. COLD is the evictor's
    // domain (never re-touched); LIVE is the writers'.
    let cold: Vec<String> = (0..64).map(|i| cold_key(i)).collect();
    let live: Vec<String> = (0..64).map(|i| format!("live-{i:04}")).collect();

    // ── CONTROL arm (owned tree + force_eviction real reclamation) ──
    {
        let dir = base.join("sf5_control");
        std::fs::create_dir_all(&dir).expect("sf5 control dir");
        let path = dir.join("c.artc");
        let reclaimed;
        {
            let mut owned: PersistentARTrieChar<()> =
                PersistentARTrieChar::create_with_config(&path, WalConfig::no_archive())
                    .expect("sf5 control create");
            owned.set_durability_policy(DurabilityPolicy::Immediate);
            let trie: SharedCharARTrie<()> = Arc::new(parking_lot::RwLock::new(owned));
            trie.enable_eviction(EvictionConfig::without_memory_monitor())
                .expect("sf5 control enable_eviction");
            for t in cold.iter().chain(live.iter()) {
                assert!(
                    ARTrie::insert(&trie, t),
                    "sf5 control: insert {t:?} not new"
                );
            }
            ARTrie::checkpoint(&trie).expect("sf5 control checkpoint");
            // MATCHED real eviction (owned tree).
            let (evicted, _b) = trie.force_eviction(1 << 20).expect("sf5 control force");
            assert!(
                evicted >= 1,
                "SF5(control): owned-tree force_eviction reclaimed 0 nodes (expected >= 1)"
            );
            reclaimed = evicted;
            trie.disable_eviction().expect("sf5 control disable");
        }
        let reopened: SharedCharARTrie<()> = ARTrie::open(&path).expect("sf5 control reopen");
        for t in cold.iter().chain(live.iter()) {
            assert!(
                Dictionary::contains(&reopened, t),
                "SF5(control): term {t:?} LOST after real eviction + reopen (BUG)"
            );
        }
        println!("smoke,SF5(control): reclaimed {reclaimed} nodes; reopen-exact OK");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── TREATMENT arm (overlay + bench_evict_overlay_cold_nodes REAL reclaim) ──
    {
        let dir = base.join("sf5_treatment");
        std::fs::create_dir_all(&dir).expect("sf5 treatment dir");
        let path = dir.join("t.artc");
        let reclaimed;
        {
            let mut owned: PersistentARTrieChar<()> =
                PersistentARTrieChar::create_with_config(&path, WalConfig::no_archive())
                    .expect("sf5 treatment create");
            owned.set_durability_policy(DurabilityPolicy::Immediate);
            owned.enable_lockfree();
            owned
                .bench_enable_eviction(EvictionConfig::without_memory_monitor())
                .expect("sf5 treatment bench_enable_eviction");
            let trie: Arc<PersistentARTrieChar<()>> = Arc::new(owned);
            for t in cold.iter().chain(live.iter()) {
                assert!(
                    trie.insert_cas_durable(t).expect("sf5 treatment insert"),
                    "sf5 treatment: insert {t:?} not new"
                );
            }
            trie.bench_immutable_checkpoint_with_eviction()
                .expect("sf5 treatment checkpoint");
            // MATCHED real eviction (COLD overlay subtrees — the §F driver).
            let mut evicted = 0usize;
            for _ in 0..8 {
                evicted += trie.bench_evict_overlay_cold_nodes(1 << 20, is_cold_path);
            }
            // (iii) REAL reclamation: NOT a silent no-op (0 ⇒ the §E artifact).
            assert!(
                evicted > 0,
                "SF5(treatment): overlay reclamation evicted 0 cold nodes — \
                 the driver is a no-op (the §E structural-no-op regression). STOP."
            );
            // (ii) faultin_count == 0 BY CONSTRUCTION: the overlay has no fault-in
            // path and COLD is never re-read; we additionally assert LIVE (the
            // re-touchable set) is untouched in the overlay — a wrongly-evicted hot
            // node would read absent here (the would-be fault-in site).
            for t in &live {
                assert!(
                    trie.contains_lockfree(t),
                    "SF5(treatment): LIVE term {t:?} unreadable after cold eviction — \
                     a hot node was wrongly evicted (faultin would be needed). STOP."
                );
            }
            reclaimed = evicted;
            drop(trie);
        }
        // (i) reopen-exact: every acknowledged term (COLD ∪ LIVE) recovers.
        let reopened = PersistentARTrieChar::<()>::open(&path).expect("sf5 treatment reopen");
        for t in cold.iter().chain(live.iter()) {
            assert!(
                Dictionary::contains(&reopened, t),
                "SF5(treatment): term {t:?} LOST after real overlay eviction + reopen (BUG). STOP."
            );
        }
        assert!(!Dictionary::contains(&reopened, "sf5-absent"));
        println!("smoke,SF5(treatment): reclaimed {reclaimed} cold overlay nodes; reopen-exact OK; faultin=0");
        let _ = std::fs::remove_dir_all(&dir);
    }
    println!(
        "smoke,SF5: REAL-reclamation correctness PASSED (both arms reopen-exact; \
         treatment reclaim > 0; faultin == 0)"
    );
}

/// Parse the value following a `--flag` token (e.g. `--variant contended`).
fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}

/// Plain `main` (harness = false).
///
/// * default (no `--measure`) → `run_smoke()` (1 round/arm/variant; keeps the
///   `[[bench]]` target self-validating under `cargo bench` / `--benches`).
/// * `--measure` → the full pre-registered K-round experiment ONCE, emitting the
///   §7 CSV (the real output, teed to a log).
///   - `--variant disjoint|contended` (default disjoint = variant A PRIMARY)
///   - `--arm control|treatment` → single-arm-per-process (RSS/perf passes, C10);
///     omitted → both arms interleaved with fixed-seed-coin within-round order.
///   - `--rounds N` → override K (perf pass uses `--rounds 1`); default K=30.
///   - `--no-warmup` → skip the 2 warmup rounds (perf pass).
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let measure =
        args.iter().any(|a| a == "--measure") || std::env::var("LOCKFREE_FLIP_MEASURE").is_ok();
    // `--eviction` (or LOCKFREE_FLIP_EVICTION) enables eviction on BOTH arms
    // (design §E): CONTROL `publish_durable_and_reclaim` + TREATMENT
    // `bench_immutable_checkpoint_with_eviction`. Off → the frozen eviction-OFF
    // experiment (RESULTS already recorded). The smoke runs the SE5 correctness
    // veto when this is set.
    let eviction =
        args.iter().any(|a| a == "--eviction") || std::env::var("LOCKFREE_FLIP_EVICTION").is_ok();
    // `--evict-real` (or LOCKFREE_FLIP_EVICT_REAL): the §F REAL-reclamation mode —
    // BOTH arms seed a fixed COLD set + perform matched eviction after each
    // checkpoint (CONTROL `force_eviction`; TREATMENT `bench_evict_overlay_cold_nodes`).
    // It implies `--eviction` (the coordinator must be installed), so enabling it
    // forces eviction ON. The smoke runs the SF5 correctness veto when set.
    let evict_real = args.iter().any(|a| a == "--evict-real")
        || std::env::var("LOCKFREE_FLIP_EVICT_REAL").is_ok();
    let eviction = eviction || evict_real;

    if !measure {
        run_smoke(eviction, evict_real);
        return;
    }

    let variant = match flag_value(&args, "--variant").as_deref() {
        Some("contended") => Variant::Contended,
        Some("disjoint") | None => Variant::Disjoint,
        Some(other) => {
            eprintln!("unknown --variant '{other}' (expected disjoint|contended)");
            std::process::exit(2);
        }
    };
    let arm_select = match flag_value(&args, "--arm").as_deref() {
        Some("control") => ArmSelect::Control,
        Some("treatment") => ArmSelect::Treatment,
        None => ArmSelect::Both,
        Some(other) => {
            eprintln!("unknown --arm '{other}' (expected control|treatment)");
            std::process::exit(2);
        }
    };
    let rounds = flag_value(&args, "--rounds")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(K_MEASURED_ROUNDS);
    let warmup = !args.iter().any(|a| a == "--no-warmup");

    run_measured_experiment(variant, arm_select, rounds, warmup, eviction, evict_real);
}

// (legacy criterion smoke retained below, commented out, for reference — it
// looped each round many times via iter_custom which ballooned WAL-archive
// scratch to 32 GB; replaced by the single-round `run_smoke` above.)
#[cfg(any())]
fn _legacy_criterion_smoke_disabled(c: &mut criterion::Criterion) {
    let base = scratch_base();
    let mut group = c.benchmark_group("lockfree_flip_smoke");
    group.sample_size(10);
    group.warm_up_time(Duration::from_millis(200));
    group.measurement_time(Duration::from_secs(2));
    group.bench_function("control_smoke", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let dir = base.join(format!("smoke_c_{}", rand_suffix()));
                std::fs::create_dir_all(&dir).expect("smoke dir");
                let r = control_round(0, true, &dir);
                total += Duration::from_secs_f64(r.elapsed_secs);
                let _ = std::fs::remove_dir_all(&dir);
            }
            total
        });
    });
    group.bench_function("treatment_smoke", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let dir = base.join(format!("smoke_t_{}", rand_suffix()));
                std::fs::create_dir_all(&dir).expect("smoke dir");
                let r = treatment_round(0, true, &dir);
                total += Duration::from_secs_f64(r.elapsed_secs);
                let _ = std::fs::remove_dir_all(&dir);
            }
            total
        });
    });
    group.finish();
}

/// Cheap unique suffix for smoke dirs (no rand dep). Only referenced by the
/// disabled legacy criterion smoke above.
#[cfg(any())]
fn rand_suffix() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}
