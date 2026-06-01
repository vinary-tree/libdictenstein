# Non-Blocking Checkpoint for the Persistent Character ARTrie

Status: **IMPLEMENTED** via the `RwLock` write→read **downgrade**; correct + formally verified; the
mean-throughput gain is modest (see Results).
Date: 2026-06-01
Tracking: pgmcp experiment **#11** (`l1-non-blocking-checkpoint-for-persistent-char-artrie`)

## Problem

`SharedCharARTrie<V> = Arc<RwLock<PersistentARTrieChar<V>>>` (the trie lock = **L1**).
- Reads (`Dictionary::contains` / `get_value`, `mod.rs:1006/1021`) take `L1.read()` for the whole
  lookup.
- `checkpoint`/`sync`/`insert` (`mod.rs:1248` …) take `L1.write()` and hold it across serialize +
  arena flush + `bm.flush_all()` + `dm.sync()` + WAL append/sync/rotate.

So a checkpoint **excludes all concurrent reads** for its entire duration (two fsyncs + a full
re-serialize). Verified: a concurrent checkpoint starved readers so hard the bench sample could not
complete; readers-only ≈ 2.19 Melem/s. L1 is the gate, not the buffer-manager `lifecycle_lock`
(which is below it and only on the cold node-load path). **Goal: let read traversals run during a
checkpoint, without sacrificing crash/recovery correctness.**

## Verified structural facts (the design rests on these)

- **F1 — copy-on-serialize (CONFIRMED, `persist.rs:324-472`).** Every checkpoint walks the in-memory
  tree and `arena_manager.write().allocate(&data)`s a *fresh* slot for each node (the only `update`
  targets a slot just allocated this run). It never overwrites a slot a reader holds and never
  reassigns `self.root` boxes ⇒ the captured arena image + root `SwizzledPtr` is a frozen,
  self-consistent snapshot. (Also why a tight checkpoint loop balloons the arena.)
- **F2 — root boxes stable.** Serialization never reassigns/frees `self.root`; `structural_generation`
  is not bumped by checkpoint. A resident reader's raw node pointer stays valid across a checkpoint.
- **F3 — recovery is LSN-windowed.** The open path (`mmap_ctor.rs:284-313`) derives
  `checkpoint_lsn = max(WAL Checkpoint records)` and replays `lsn > checkpoint_lsn` from the **active**
  WAL (archives are read only by the corruption-rebuild path).
- **F4 — block-0 publish is the linearization point.** `set_root_ptr`+`set_entry_count`+header
  checksum, made durable by `dm.sync()`; `verify_checkpoint` re-reads it. Readers never read block 0
  (only `load_root_from_disk` at open), so there is no reader/descriptor race.

## Shipped design — write→read downgrade

`SharedCharARTrie::checkpoint` (`mod.rs`) drives:

```
let guard = self.write();                       // exclusive: excludes inserts
debug_assert!(guard.lockfree_root.is_none());   // C2 guard (see below)
let snapshot = {                                // Phase A — serialize under L1.write
    let _pin = EpochGuard::new(epoch_manager);  //   epoch-pinned (F1/F2 + EBR safety)
    guard.capture_snapshot()?                    //   tree -> fresh arenas + flush_dirty_slots
};                                              // pin dropped (B/C touch no in-memory nodes)
let read = RwLockWriteGuard::downgrade(guard);  // ATOMIC write->read (no release window)
read.publish_durable_and_reclaim(snapshot)      // Phase B+C — descriptor+flush+fsync, WAL
```

`persist.rs` was split into `capture_snapshot` / `publish_snapshot` / `publish_durable_and_reclaim`
(all `&self`); the owned `PersistentARTrieChar::checkpoint(&mut self)` (and the experiment's control
arm) call the same phases under a held `&mut self`, i.e. blocking.

**Why this is correct and needs no special WAL handling:**
- The downgrade admits concurrent `L1.read()` readers during the fsync-bound publish, while
  `L1.write()` **inserts stay excluded for the whole checkpoint** (write during A, read during B/C —
  read excludes write), and the downgrade is atomic (no release window).
- Therefore **no writer can race the checkpoint** ⇒ `next_lsn` is unchanged from capture to WAL
  publish ⇒ `checkpoint_lsn = next_lsn` (the original convention) stays exact and
  `rotate_to_archive` only ever archives covered records. There is **no GAP_LEDGER #41 data-loss
  window** and **no frontier-bounded WAL reclaim** is needed. A `debug_assert_eq!(next_lsn,
  snapshot.next_lsn_at_capture)` in `publish_durable_and_reclaim` fails loudly if that invariant is
  ever violated.
- **C2 guard:** the lock-free overlay's `insert_cas` bypasses `L1.write` — but it is never exposed on
  `SharedCharARTrie` and never touches `self.root`/the WAL (it mutates a separate `lockfree_root`).
  The `debug_assert!(lockfree_root.is_none())` documents and enforces that.
- **No separate checkpoint mutex / ctor changes:** two checkpoints serialize on the write lock (the
  second's Phase-A `write()` is blocked by the first's downgraded read guard).
- **Crash safety unchanged from the prior blocking checkpoint** (same publish→sync→WAL→rotate order;
  idempotent LSN-windowed replay covers a crash between block-0 sync and WAL rotate).

## Correctness verification (all green)

- **TLA⁺** `ConcurrentCheckpointPublication.tla`: added `CaptureEqualsPublishFrontier`
  (`gate="Checkpoint" ⇒ nextLsn = ckptTarget+1` — the model-level proof that the downgrade consumes
  no LSN during a checkpoint) and admitted reads during the checkpoint publish phase. **TLC: No error,
  312 distinct states** (no growth); SANY parse clean.
- **`tests/persistent_nonblocking_checkpoint_correspondence.rs`**: concurrent readers + writer +
  checkpointer → reopen from disk → every committed key survives, no torn read. **Passes.**
- **Full suite: 2435 passed**, 1 skipped; unsafe-inventory gate green (no new unsafe — safe code).
- `parking_lot::RwLockWriteGuard::downgrade` atomicity is a trusted primitive (validated from source:
  a single `fetch_add(ONE_READER - WRITER_BIT)` with no writer-acquirable intermediate state). loom
  is N/A — it instruments custom atomics, not `parking_lot::RwLock`; the protocol is covered by TLA⁺ +
  the executable correspondence test.

> Resource principle for the formal artifacts (per project policy): memory-efficiency FIRST. TLC ran
> with tiny CONSTANTS (2 writers / 2 terms / MaxLSN=3) under a `systemd-run -p MemoryMax=…` cap; the
> downgrade additions cost zero extra states.

## Performance results (pgmcp experiment #11 — pre-registered Welch's t-test, anti-p-hacking)

Interleaved (drift-controlled), 57 non-warm-up replicates/arm, 4 concurrent resident readers during a
throttled concurrent checkpointer, real-disk scratch. Control = blocking checkpoint; treatment =
downgrade.

- **Verdict: REJECTED.** Welch t=1.84, **p=0.0345** (significant), Mann-Whitney p=0.007, but
  **Cohen's d=0.345 < the pre-registered 0.5** meaningful-effect threshold (~5% mean improvement).
- **Interpretation (the magnitude caveat):** the **serialize** phase (the F1 copy-on-serialize
  re-walk of the whole tree) still runs under the write lock and dominates a small trie's checkpoint;
  the downgrade frees only the fsync, a minor fraction. A larger win would require freeing the
  serialize too (lock-free reads), which is higher-risk and out of scope. The downgrade also bounds
  reader tail-latency (no full-checkpoint stalls — visible as the bimodal high mode), which mean
  throughput under-weights.
- **Decision: KEPT** — correct, safe, formally verified, strictly not worse, modest measurable gain +
  bounded tail latency, minimal added complexity. One-line revert available (`mod.rs` wrapper →
  `let mut g = self.write(); g.checkpoint()`).

## Alternative considered and rejected — option (b): full lock release + frontier-bounded WAL reclaim

The first design (from the initial design agent) **released** L1 entirely after the snapshot and ran
publish + WAL reclaim with no lock, so **both readers and writers** would proceed during checkpoint
I/O. Because a writer can then commit during the I/O window, recovery would lose it unless the WAL
reclaim is **frontier-bounded** — retain every record with `lsn > snapshot_lsn`, archive only the
covered prefix, and always use the *captured* `snapshot_lsn` for `checkpoint_lsn`. This reopens
exactly the data-loss window the ledger records as already-caught for the byte trie (GAP_LEDGER #41),
and it relies on the subtlety that the clean-reopen path reads only the active WAL (not archives).

**Rejected** because: (1) the in-scope goal is *readers* concurrent with checkpoint, which the
downgrade fully achieves; (2) freeing *writers* during checkpoint is out of scope; (3) option (b)
adds a data-loss-critical reclaim + a checkpoint mutex for that out-of-scope benefit, whereas the
downgrade gets the in-scope win with **no** new data-loss surface. If non-blocking *writes* during
checkpoint ever become a goal, option (b) (with the frontier reclaim + its own TLA⁺/loom proof) is
the path — it is a separate, larger work item.

## Critical files

- `src/persistent_artrie_char/persist.rs` — `capture_snapshot` / `publish_snapshot` /
  `publish_durable_and_reclaim` + `CheckpointSnapshot` (with `next_lsn_at_capture` self-check).
- `src/persistent_artrie_char/mod.rs` — `ARTrie::checkpoint` wrapper (the write→read downgrade + C2
  guard).
- `formal-verification/tla+/ConcurrentCheckpointPublication.tla` (+ `.cfg`) — `CaptureEqualsPublishFrontier`.
- `tests/persistent_nonblocking_checkpoint_correspondence.rs` — concurrency + reopen correspondence.
- `benches/concurrent_read_vs_flush_benchmarks.rs`, `examples/exp_checkpoint_throughput.rs` —
  experiment #11 harness (real-disk, bounded, ceiling-guarded).
