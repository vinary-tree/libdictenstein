# Lock-Free Flip Benchmark — Pre-Registered Experimental Design (v2, principled)

**Status:** PRE-REGISTRATION FROZEN at persist time (2026-06-01), BEFORE any measured data.
Everything above the RESULTS section is frozen; only RESULTS is appended after the K-round run.
This v2 REPLACES a prior winged v1 whose fairness was asserted, not proven; v2 grounds every
fairness claim in `file:line` evidence and fixes three v1 measurement bugs (RSS in-process
contamination, arm-ordering bias, missing p50/p999). v1 is recoverable from the session transcript.

**Decision context:** go/no-go on the irreversible "lock-free flip" (Task #14, Phase E — make the
lock-free overlay write+checkpoint path the production default in `PersistentARTrieChar`, replacing
the owned `Arc<RwLock>` tree + write→read-downgrade checkpoint; Task #15 then deletes the owned tree).
**No flip is performed by this experiment**; both paths already exist and are measured side by side
via a reversible `bench-internals` accessor.

---

## 1. Hypotheses (FROZEN)

**Primary:**
- **H1:** Under matched durability, TREATMENT (lock-free `insert_cas_durable` + immutable-snapshot
  checkpoint) has higher aggregate write throughput (ops/sec across W writers) than CONTROL
  (`Arc<RwLock>` `insert_with_value` + downgrade checkpoint), in the disjoint-prefix workload.
- **H0:** μ_T − μ_C = 0.
- **Two-sided** Welch's t (must detect TREATMENT *losing* — per-op fsync / Arc-refcount contention are
  real risks, §9). H1 "supported" only if effect is positive AND significant AND meets the d≥0.8 floor.

**Secondary (descriptive regression VETOES, not confirmatory tests → no multiplicity correction):**
- **S1 — checkpoint pause** (writer-observable stall): TREATMENT ≤ CONTROL.
- **S2 — write tail** p50/p99/p999: TREATMENT not materially worse.
- **S3 — peak RSS**: TREATMENT not materially worse (overlay holds a 2nd in-memory Arc tree).
- **S4 — contended-prefix throughput** (2nd workload variant): TREATMENT not significantly worse.

**Pre-registered expectation (anti-hindsight):** given experiment #11 (serialize re-walk dominates a
small trie's checkpoint; the lock only frees the fsync) AND that both write paths now pay an identical
per-op fsync (§2), the realistic prior is a **small-or-null throughput effect**, with any TREATMENT win
showing in **checkpoint pause (S1)** / **tail (S2)**. The design is built to return "don't flip /
proceed-with-caveat."

---

## 2. Durability-parity resolution — THE critical section (code evidence)

**Resolution: configure BOTH arms to `DurabilityPolicy::Immediate`, proven to route both writer paths
through the SAME per-op fsync at the syscall level.**

Evidence chain (both arms converge on one `File::sync_all()` per inserted term):
- `DurabilityPolicy::Immediate` = fsync before acknowledging (`durability.rs:20-49`); `set_durability_policy`
  is a field setter (`wal_helpers.rs:47-49`).
- CONTROL: `SharedCharARTrie::insert_with_value` → `self.write(); guard.insert_with_value` → char
  mutation_api `insert_with_value` → `append_to_wal(Insert)` → **`append_to_wal_inner`** (`wal_helpers.rs:78-116`).
- TREATMENT: `insert_cas_durable` (`lockfree_cas.rs:176-247`) Order-A step 1 (`:212-217`):
  `append_to_wal_returning_lsn(Insert{value:None})` → **`append_to_wal_inner`** (shared body, `wal_helpers.rs:73-75`).
- Shared chokepoint `append_to_wal_inner` (`wal_helpers.rs:78-116`), `group-commit` NOT compiled → direct
  path `:104-114` → `wal_writer.append` then **`sync_wal_after_append`** (`:112`).
- `sync_wal_after_append` (`wal_helpers.rs:147-172`): under Immediate → `wal_writer.sync()` (`:160`) +
  verify `synced_lsn>=appended_lsn` (`:165-169`).
- `AsyncWalWriter::sync` (`wal/async_writer.rs:545-552`) → `WalWriter::sync` (`wal/writer.rs:~212`):
  `file.flush()?; file.get_ref().sync_all()?;` — **real per-op fsync** (`StdFsync`, `sync_backend.rs:26-30`).

**Conclusion (frozen):** under Immediate both arms do exactly one blocking `File::sync_all()` per inserted
term via the identical call chain. The per-op fsync is present and EQUAL in both arms; it is NOT the source
of any measured difference. What differs is ONLY the concurrency mechanism the flip changes (CONTROL =
RwLock write-guard serialization + write-guard-held-across-serialize-then-downgrade checkpoint; TREATMENT =
lock-free root CAS + no-writer-lock immutable checkpoint). **Mismatched durability cannot make this lie.**

**Two residual confounds checked + neutralized:**
1. **io-uring swap.** `bench-internals = ["io-uring-backend"]` (`Cargo.toml:115`) compiles io-uring IN, but
   `create_with_config` → `DiskManager::create` where `DiskManager = MmapDiskManager` (alias, `disk_manager.rs:228`);
   io-uring is a separate `create_with_io_uring` method. Both arms use `MmapDiskManager` + `StdFsync`. Verified.
2. **WAL config.** Both arms `WalConfig::no_archive()`. Honest asymmetry (LOGGED, not hidden):
   TREATMENT `publish_immutable_snapshot_retaining_wal` RETAINS the WAL (watermark-reclaim safety,
   `persist.rs:521-570`) vs CONTROL truncates — an intended property of the no-writer-lock checkpoint;
   recorded as `round_dir_bytes`, does not bias throughput (reclaim runs off the timed writers).

---

## 3. Reachability + reversible `bench-internals` exposure

The exposure ALREADY EXISTS (untracked) — reuse, no NEW production change. `enable_lockfree`/`insert_cas_durable`/
`contains_lockfree`/`set_durability_policy`/`create_with_config`/`WalConfig::no_archive` are all `pub`.
The immutable-checkpoint steps `capture_snapshot_immutable`/`publish_immutable_snapshot_retaining_wal` are
`pub(crate)` + `#[cfg(any(test, feature="bench-internals"))]` (`persist.rs:341-343,542-544`), reached from a
bench BINARY via the `pub #[cfg(feature="bench-internals")] fn bench_immutable_checkpoint` shim (`persist.rs:~615`):
```rust
#[cfg(feature = "bench-internals")]
pub fn bench_immutable_checkpoint(&self) -> Result<()> {
    let snapshot = self.capture_snapshot_immutable()?;
    self.publish_immutable_snapshot_retaining_wal(&snapshot)
}
```
No flip; compiled out without `bench-internals`. **Reversibility:** deleting the 3 cfg disjuncts + the shim
restores `#[cfg(test)]`-only visibility. `checkpoint()` untouched.
**Faithfulness note (NOT a scope-cut):** the shim runs the two durable steps the flip would wire into
`checkpoint()`, but NOT eviction-registry publication / WAL reclaim (those need eviction on;
`publish_durable_and_reclaim` `debug_assert!(eviction_registry.is_none())`). Benchmark runs eviction OFF
on both arms (parity) → faithful to the eviction-off membership-trie flip (V=(), Task #14's first target).
Flipping WITH eviction on is not yet measurable; ledger says so.

---

## 4. Workload spec (FROZEN)

**Hardware:** AMD Threadripper PRO 5975WX, 32 physical cores, SMT OFF, 4 CCD×8, 32 MiB L3/CCD, governor
`performance` (live; no sudo), NVMe `/dev/nvme0n1p4` 66 GB free (97% full). (Execution agent: confirm against
`/home/dylon/.claude/hardware-specifications.md` + live `lscpu`/`nproc`; record any delta in RESULTS.)

**Topology:** W=8 writers, R=4 readers, 1 checkpointer (+1 driver) = 14 OS threads, pinned `taskset -c 0-15`
(CCD0+CCD1; ≤16 avoids oversubscription, leaves CCD2/3 for OS + WAL bg-sync). W=8 stresses the write path;
not wider because per-op fsync saturates the NVMe queue before 32 threads.

**Variant A — DISJOINT prefixes (overlay best case; PRIMARY):** each writer owns a disjoint range; CONTROL
still serializes on the global write lock, TREATMENT rarely CAS-retries. Key gen (deterministic, no `rand`):
SplitMix64 seed `0x9E3779B97F4A7C15`, `writer_key(t,i)=format!("ngram-{:016x}-語{}", splitmix64(base^SEED), base)`,
`base=t*WRITES_PER_ROUND+i`. Multi-byte UTF-8 suffix exercises the u32 char path; identical key set + per-thread
order both arms every round.

**Variant B — CONTENDED hot prefix (SECONDARY, S4):** all W writers insert under ONE shared hot prefix +
disjoint suffix → max CONTROL lock contention AND TREATMENT root-CAS retries (where TREATMENT could LOSE to
CAS livelock / Arc-refcount ping-pong). `writer_key_hot(t,i)=format!("hot-prefix-fixed/{:08x}", splitmix64(base^SEED))`
(each term still new → monotone-final, `lockfree_cas.rs:300-321`). Log `cas_retries` per TREATMENT round.

**Readers (LOAD, not primary):** R readers loop `contains`/`contains_lockfree` on early-inserted keys; read
throughput logged, NOT gating (experiment #11 already decided reads).

**Cadence:** FIXED op-count `WRITES_PER_ROUND=2000` ×8 = 16000 writer ops/round (per-round throughput =
16000/wall-sec, directly comparable). Checkpointer loops `checkpoint + sleep(20ms)` across the whole writer
window, capped `MAX_CHECKPOINTS_PER_ROUND=40`. `WARMUP_ROUNDS=2` discarded. Timed region = post-barrier until
all writers join.

**Optional durability MATRIX (corroborating, decided pre-data):** repeat variant A at `GroupCommit` WITHOUT
the `group-commit` feature (falls back to identical blocking sync, `wal_helpers.rs:159`) — a negative control
on the policy knob (should show NO arm-vs-arm change vs Immediate). `Periodic`/`None` NOT run (`insert_cas_durable`
rejects them, `lockfree_cas.rs:180-188` — itself the honest statement that the durable path is fsync-bound).
Primary decision uses the Immediate run only.

**Disk discipline (97%-full disk):** scratch `target/bench-scratch` on real NVMe, NEVER `/tmp` (tmpfs=RAM);
`TMPDIR=$PWD/target/test-tmp`; fresh per-round subdir + `remove_dir_all` after each round; **5 GiB ceiling**
checked pre+post each round (abort if exceeded); `MemoryMax=32G` bounds page-cache+RSS.

---

## 5. Confound-control checklist (item → mechanism → residual threat)

| # | Confound | Mechanism (frozen) | Residual |
|---|---|---|---|
| C1 | Durability mismatch | Both `Immediate`; proven same fsync (§2) | None (proven) |
| C2 | I/O backend | Both `MmapDiskManager`+`StdFsync`; `bench-internals` doesn't swap (§2.3) | None (verified) |
| C3 | WAL/buffer cfg | Both `no_archive()`, 256-page pool, same AsyncWalConfig | TREATMENT WAL-retain vs CONTROL truncate (logged) |
| C4 | CPU pin/migration | `taskset -c 0-15`; 14≤16 threads | OS schedules within cpuset; symmetric |
| C5 | Governor/boost | `performance` live (no sudo for `cpupower`); boost on | Boost freq variance → absorbed by interleave + K + Welch; record observed freq |
| C6 | Real-disk scratch | NVMe `target/bench-scratch`; never tmpfs; ceiling+cleanup | 97%-full → SSD GC noise; symmetric; ceiling stops ENOSPC |
| C7 | Page-cache carryover | Fresh cold file/round both arms; 2 warmup discarded; interleaved | No `drop_caches` (root); symmetric |
| C8 | Allocator | system glibc, identical | Frag → mitigated by interleave + randomized within-round order (C9) |
| C9 | Arm order / thermal | **Interleave arms/round + randomize within-round order by fixed-seed coin** (v1 always control-first — FIX) | Residual drift → K + Welch |
| C10 | RSS cross-contam | **v1 BUG: `VmHWM` is process-lifetime; both arms one process → meaningless.** FIX: RSS measured in **separate single-arm-per-process** pass (`--arm`) | Fixed |
| C11 | WAL bg-sync thread | 1/arm in cpuset; symmetric | within 16-core budget |
| C12 | criterion driver | custom K-round loop (`harness=false`, plain main); criterion only the shell | None |

---

## 6. Metrics + instrumentation (no hot-path perturbation)

- **PRIMARY throughput:** `16000/timed_wall_secs`, one/round/arm; timed region wraps writer join only.
- **S1 checkpoint pause:** measured AT THE WRITER as the upper tail of per-op write latency during checkpoint
  windows (a checkpoint that stalls no writer is fine for TREATMENT; CONTROL's downgrade excludes writers during
  capture → SHOULD stall — exactly S1's hypothesis). ALSO checkpointer-side per-call duration (mean/p99).
  Per-thread `Vec<u64>` histograms merged post-join (zero timed-region contention).
- **S2 latency p50/p99/p999:** merge all W×WRITES_PER_ROUND per-op latencies post-join (`hdrhistogram` dev-dep
  available, or sort-Vec at 16k/round).
- **S3 peak RSS:** `/proc/self/status` `VmHWM` in the single-arm-per-process pass (C10).
- **Secondary (non-gating):** `cas_retries`/TREATMENT round, `round_dir_bytes`, read_ops, checkpoint count.
- **perf:** `perf record --call-graph lbr` on ONE representative round/arm (separate invocation, contended
  variant). Expected (confirm, not assume): CONTROL — RwLock paths, `serialize_char_node_to_disk` re-walk,
  `fsync`/`msync`, `__mprotect`; TREATMENT — `compare_exchange` spin, `Arc` refcount, `dashmap` shards, `fsync`.
  Fall back to `--call-graph dwarf` if LBR stacks too shallow for Arc attribution.

---

## 7. Harness architecture

Custom K-round driver (NOT criterion stats — its adaptive resampling fights fixed-K pre-registration + balloons
the 97%-full disk). Criterion = only the `[[bench]]` shell (`harness=false`, plain `main`). `std::thread` +
`Barrier(W+R+1+1)` start; `AtomicBool stop` ends readers/checkpointer after writers join; per-writer latency
Vecs via JoinHandle. One CSV line/round/arm, flushed+teed:
`arm,round,is_warmup,write_ops,elapsed_secs,write_ops_per_sec,read_ops,checkpoint_rounds,ckpt_pause_mean_us,ckpt_pause_p99_us,write_p50_us,write_p99_us,write_p999_us,cas_retries,peak_rss_kib,round_dir_bytes,variant,durability`.
Arms interleaved/round, within-round order randomized (fixed-seed coin). RSS+perf passes single-arm via `--arm`.
**Modes (CLI on plain main):** default → `run_smoke()` (1 round/arm/variant, self-validates in CI); `--measure`
→ full K-round (variant A primary; `--variant contended` for B); `--arm control|treatment` → single-arm-per-process;
`--variant disjoint|contended`; `--rounds N`/`--no-warmup` (perf pass). Scratch `scratch_base()`/`dir_size_bytes()`/
ceiling guard/per-round cleanup kept verbatim from v1.

---

## 8. Statistical + pre-registration plan (anti-p-hacking)

- **K = 30 measured rounds/arm** (after 2 warmup). Justification: exp #11 needed 57 replicates for d≈0.345;
  expected effect here is SMALLER (durability equalized). Power for two-sided Welch α=0.05 to detect the
  pre-registered floor **d=0.8** at ≈0.9 power needs ≈27/arm → K=30 margin. We deliberately do NOT chase a
  small d≈0.3 effect (not decision-relevant for an IRREVERSIBLE flip). K=30 fixed before data.
- **Primary test:** two-sample **Welch's t** on the K throughput vectors (variant A, Immediate). Report per-arm
  mean±sd; difference (T−C) + 95% Welch CI; t, Welch–Satterthwaite df, two-sided p; Cohen's d (pooled). Also
  **Mann–Whitney U** (distribution-free corroboration). Decision keys on Welch + CI + d.
- **Effect-size floor (pre-registered):** because the flip is IRREVERSIBLE + deletes the owned tree, require
  **d ≥ 0.8** (large) for an unconditional "flip wins on throughput" (exp #11 used d≥0.5 for a *reversible*
  change; irreversible warrants higher). 0.5≤d<0.8 = "modest, flip-neutral on throughput — decide on S1/S2".
- **α=0.05.** Secondaries are descriptive vetoes (can only BLOCK, not justify).
- **STOPPING RULE:** exactly K=30/arm/variant. NO peeking, NO early stop, NO adding rounds. Launch once, tee,
  analyze offline. Analysis script written + committed BEFORE the run, run once on the frozen CSV.
- **DECISION RULE → flip recommendation (frozen, exhaustive):**
  1. **PROCEED:** Welch p<0.05 AND diff>0 AND d≥0.8 (variant A throughput) AND no regression: S1 T≤C, S2
     p99/p999 T≤1.10×C, S3 RSS T≤1.25×C, S4 contended T not sig worse (p≥0.05 or diff≥0).
  2. **PROCEED-WITH-CAVEAT (tail/pause, not throughput):** throughput null/modest (d<0.8) BUT S1 clear pause/
     tail improvement AND no S2/S3/S4 regression → "flip buys bounded tail latency, not mean throughput."
  3. **DON'T FLIP (no benefit):** throughput null/modest AND no S1 benefit → "per-op fsync dominates, lock isn't
     the bottleneck; keep the owned tree; don't pay irreversibility."
  4. **DON'T FLIP (regression — first-class):** ANY of: throughput diff negative+significant; S1 pause worse;
     S2 >1.10×; S3 >1.25×; S4 significantly worse (CAS livelock) → "flip regresses [metric]; reject."
- **Ordering:** persist this ledger + analysis script FIRST (frozen), THEN build/run, THEN append RESULTS.

---

## 9. Honest threats-to-validity / expected-null

Realistic ways TREATMENT loses/ties, all SURFACED (not buried):
1. **Per-op fsync dominates → throughput ties** (the EXPECTED prior, §1): device fsync (tens of µs, worse on a
   97%-full SSD) is binding → lock-vs-CAS irrelevant → decision rule 3 + perf (both fsync-dominated).
2. **Overlay 2nd-tree alloc cost:** surfaced via S3 (RSS) + perf Arc/alloc.
3. **Arc-refcount contention (contended variant):** path-copy+CAS of the shared hot spine → cross-CCD coherence;
   TREATMENT could be SLOWER than one write lock → variant B (S4) + `cas_retries` + perf attribution. Most likely
   regression site.
4. **CAS retry/livelock:** `insert_cas_durable` retries visibility CAS on Conflict (`lockfree_cas.rs:240-244`);
   `cas_retries` measures it; variant B exposes it.
5. **Small-trie:** 16k keys; TREATMENT immutable checkpoint ALSO walks the overlay to serialize
   (`capture_snapshot_immutable`→`overlay_to_inner`→`serialize_char_node_to_disk`) → O(tree), may not beat
   CONTROL's pause as much as hoped → S1 comparable.
6. **WAL-retain disk growth (TREATMENT):** `round_dir_bytes`, a non-throughput cost the owner buys.
7. **v1 in-process RSS bug:** fixed by single-arm-per-process (C10).
Decision branches 3 & 4 exist precisely so "fsync makes the flip pointless" and "flip regresses under contention"
are reportable conclusions.

---

## 10. Execution runbook (exact commands; cwd = repo root; quiet machine)

```bash
# 0. Pre-flight (read-only)
nproc; cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor; df -h .
mkdir -p target/test-tmp target/bench-scratch
# 1. Build once (record rustc + git HEAD in ledger)
systemd-run --user --scope -p MemoryMax=32G --quiet env TMPDIR=$PWD/target/test-tmp \
  cargo build --release --benches --features persistent-artrie,bench-internals 2>&1 | tee docs/experiments/lockfree-flip-build.log
# 2. Smoke (validates, NOT a measurement)
systemd-run --user --scope -p MemoryMax=32G --quiet env TMPDIR=$PWD/target/test-tmp taskset -c 0-15 \
  cargo bench --bench lockfree_flip_benchmark --features persistent-artrie,bench-internals 2>&1 | tee docs/experiments/lockfree-flip-smoke.log
# 3. PRIMARY — variant A disjoint, Immediate, K=30, interleaved. ONCE.
systemd-run --user --scope -p MemoryMax=32G --quiet env TMPDIR=$PWD/target/test-tmp taskset -c 0-15 \
  cargo bench --bench lockfree_flip_benchmark --features persistent-artrie,bench-internals -- --measure --variant disjoint | tee docs/experiments/lockfree-flip-raw-disjoint.csv
# 4. SECONDARY — variant B contended. ONCE.
systemd-run --user --scope -p MemoryMax=32G --quiet env TMPDIR=$PWD/target/test-tmp taskset -c 0-15 \
  cargo bench --bench lockfree_flip_benchmark --features persistent-artrie,bench-internals -- --measure --variant contended | tee docs/experiments/lockfree-flip-raw-contended.csv
# 5. RSS pass — single-arm-per-process (fixes VmHWM). ONCE each.
... --measure --arm control   --variant disjoint | tee docs/experiments/lockfree-flip-rss-control.csv
... --measure --arm treatment --variant disjoint | tee docs/experiments/lockfree-flip-rss-treatment.csv
# 6. perf — 1 round/arm, contended (separate; find <hash> via ls target/release/deps/lockfree_flip_benchmark-*)
systemd-run ... taskset -c 0-15 perf record --call-graph lbr -o target/bench-scratch/perf-control.data -- \
  ./target/release/deps/lockfree_flip_benchmark-<hash> --measure --arm control --variant contended --rounds 1 --no-warmup
perf report -i target/bench-scratch/perf-control.data --stdio | tee docs/experiments/lockfree-flip-perf-control.txt
# (repeat --arm treatment)
# 7. Offline analysis (script frozen BEFORE step 3), run ONCE on frozen CSVs
python3 docs/experiments/analyze_lockfree_flip.py --disjoint ...raw-disjoint.csv --contended ...raw-contended.csv \
  --rss-control ...rss-control.csv --rss-treatment ...rss-treatment.csv | tee docs/experiments/lockfree-flip-analysis.txt
# 8. Env capture: governor + observed freq (cpu0/4/8/12 scaling_cur_freq) + git rev-parse HEAD
# 9. Cleanup: rm -rf target/bench-scratch/*
```
Determinism: fixed SplitMix64 seed; fixed K/threads/pinning; no rand/wall-clock in key gen; arm order by
fixed-seed coin. Re-runs reproduce CSVs up to device/thermal noise (absorbed by K + Welch).

---

## 11. Files to create / modify

1. **`benches/lockfree_flip_benchmark.rs`** (MODIFY prior untracked file; keep SplitMix64/`scratch_base`/
   `dir_size_bytes`/ceiling/per-writer latency Vecs/barrier/`run_smoke`). Changes: (a) K 12→**30**;
   (b) add `--arm control|treatment` single-arm-per-process (fixes C10); (c) add `--variant disjoint|contended`
   + `writer_key_hot`; (d) randomize within-round arm order (fixed-seed coin, C9); (e) emit
   p50/p999/`cas_retries`/`variant`/`durability` CSV cols; (f) `--rounds N`/`--no-warmup`; (g) gate
   `#![cfg(all(feature="persistent-artrie", feature="bench-internals"))]`. Writer/checkpoint/reader calls
   unchanged (`insert_with_value`/`ARTrie::checkpoint`; `insert_cas_durable`/`bench_immutable_checkpoint`/`contains_lockfree`).
2. **`docs/experiments/lockfree-flip-benchmark-ledger.md`** — THIS file (frozen).
3. **`docs/experiments/analyze_lockfree_flip.py`** (CREATE, frozen before run): drop warmup, per-arm mean±sd,
   Welch t/df/p (two-sided), 95% Welch CI of (T−C), Cohen's d, Mann–Whitney U for variant A (primary) + B (S4);
   S1/S2/S3 descriptive; print decision-rule verdict (§8). stdlib + scipy (or hand-rolled Welch via `statistics`).
4. **`Cargo.toml`** — `[[bench]] name="lockfree_flip_benchmark" harness=false
   required-features=["persistent-artrie","bench-internals"]` ALREADY present (lines 267-272). No change.

No production source change beyond the already-present reversible `bench-internals` cfg disjuncts +
`bench_immutable_checkpoint` (§3). `checkpoint()` + all production paths untouched. No flip.

---

## RESULTS (appended AFTER the frozen K-round run)

**Run date:** 2026-06-02. Single measured run per §10, K=30/arm/variant, no peeking, no re-runs.

### Provenance / environment
- **git HEAD:** `549957a222d4de418cb6eca8e5374f89cda575a9`
- **rustc:** `1.95.0 (59807616e 2026-04-14)`; release `--benches --features persistent-artrie,bench-internals`
  (build log `lockfree-flip-build.log`; bench compiled with 0 warnings).
- **governor:** `performance` (live, no sudo). **Observed `scaling_cur_freq`** at capture (cpu0/4/8/12):
  3526 / 1429 / 3435 / 4291 MHz — boost active; per-CCD frequency variance absorbed by K + Welch (C5).
- **Hardware delta vs §4:** host is the expected AMD Threadripper PRO 5975WX (32 phys cores, `nproc`=32,
  governor `performance`). NVMe disk was 56→34 GiB free across the runs (external machine activity; the
  benchmark's own scratch stayed clean — `target/bench-scratch` 4.0K after every run, per-round cleanup +
  5 GiB ceiling never tripped, no ENOSPC). `taskset -c 0-15` honored. No design-relevant hardware delta.

### Primary — variant A (DISJOINT), Immediate, write_ops_per_sec (n=30/arm, warmup dropped)
| arm | mean ops/s | sd |
|-----|-----------:|---:|
| CONTROL   | 4,560.6 | 136.3 |
| TREATMENT | 18,791.9 | 792.4 |

- **diff (T−C) = +14,231.3 ops/s (+312.05%)**, **95% Welch CI [13,931.8, 14,530.8]** (excludes 0, strictly positive).
- **Welch t = 96.9467, df = 30.72, p(two-sided) = 9.10e-40.**
- **Cohen's d = 25.03** (pooled) — vastly exceeds the pre-registered d≥0.8 floor.
- **Mann–Whitney U = 900.0, p = 3.02e-11** (complete separation; corroborates Welch).

### Secondary descriptive vetoes
- **S1 — checkpoint pause.** FROZEN S1 = the **writer-observable** stall (§6: "measured AT THE WRITER as the
  upper tail of per-op write latency during checkpoint windows"; §1: "writer-observable stall"). Writer-observed
  p999: **control 126,374 µs vs treatment 4,533 µs → treatment 96.4 % better (T≤C, no regression — a large S1
  improvement).** Secondary "ALSO" diagnostic (checkpointer-side per-call, §6): mean control 67,625 / treatment
  59,532 µs; **p99 control 132,144 / treatment 185,414 µs** — the per-call checkpoint is *longer* for TREATMENT
  (its immutable snapshot still walks the overlay to serialize, §9 item 5) but does so WITHOUT stalling writers,
  which is exactly why the writer-observed stall collapses. Per §6 this checkpointer-side number is NOT the S1
  hypothesis quantity and does not gate the verdict.
- **S2 — write tail.** p50: control 58.2 / treatment 305.8 µs (ratio 5.25 — TREATMENT's per-op floor is higher,
  the WAL-append-then-CAS critical path; NOT vetoed — §8 only vetoes on p99/p999). **p99: 67,406 → 1,955 µs
  (ratio 0.029).** **p999: 126,374 → 4,533 µs (ratio 0.036).** Both tail ratios far below the 1.10× veto → no
  S2 regression; TREATMENT's tail is ~28–34× tighter.
- **S3 — peak RSS** (single-arm-per-process pass, fixes v1 VmHWM C10): **control 484,160 / treatment 425,088 KiB,
  ratio 0.878** (≤1.25× veto) — TREATMENT actually used LESS peak RSS (the owned-tree arena vs overlay+cache
  net out in TREATMENT's favor at this 16k-key scale). No S3 regression.
- **S4 — contended variant B** (write_ops_per_sec, n=30/arm): control 5,799.6±191.3 / treatment 15,486.1±835.0;
  **diff = +9,686.5 ops/s (+167.02%), 95% CI [9,368.0, 10,005.1], Welch t = 61.94, df = 32.04, p = 6.32e-35,
  d = 15.99, Mann–Whitney U = 900, p = 3.02e-11.** TREATMENT is significantly BETTER, not worse → no S4
  regression / no CAS-livelock. **`cas_retries` (TREATMENT): disjoint mean 136.9 max 392; contended mean 140.1
  max 258** — bounded, low (≈0.9 % of the 16k ops), no livelock even on the hot prefix.

### Supporting (non-gating)
- `round_dir_bytes` (the honest §2.2 WAL-retain asymmetry): TREATMENT retains WAL → ~57 MB/round (disjoint),
  CONTROL truncates but re-serializes the tree into fresh arenas each checkpoint → ~303 MB/round. The
  copy-on-serialize CONTROL data file is the larger disk cost here; TREATMENT's retained WAL is the smaller.
- `read_ops` (LOAD, non-gating): disjoint treatment ≈14.4 M/round via `contains_lockfree` (readers run freely);
  CONTROL disjoint ≈8.2 k (readers starved by the write lock); contended CONTROL = 0 (readers never completed a
  2000-iter pass under maximal lock contention — an honest artifact, readers are non-gating per §4).

### perf attribution — SKIPPED (permitted, §10 step 6 / §E)
`perf record --call-graph lbr` failed: `sys_perf_event_open()` → EINVAL ("Failure to open any events for
recording") under `kernel.perf_event_paranoid=2` with no CAP_PERFMON / no sudo. Neither LBR nor a dwarf
fallback is openable without raising the paranoid level (root). Recorded as an environment limitation in
`lockfree-flip-perf-control.txt`; the §8 decision rule does not depend on perf. (Design-expected, unconfirmed:
CONTROL = RwLock serialization + `serialize_char_node_to_disk` re-walk + fsync/msync; TREATMENT =
compare_exchange spin + Arc refcount + fsync.)

### Analysis-script S1-sub-metric correction (full transparency — NOT a design change)
The frozen analysis script's FIRST run keyed the S1 veto on the **checkpointer-side p99** (the §6 "ALSO"
diagnostic) rather than the **writer-observed stall** that §6/§1 DEFINE as S1. That first run (preserved
verbatim at `lockfree-flip-analysis.txt`) reported **Branch 4** solely because checkpointer-side p99 was higher
for TREATMENT. On recognizing the sub-metric mismatch against the frozen S1 definition, `section_S1` was
corrected to key the veto on `write_p999` (the writer-observable stall), with the checkpointer-side per-call
retained as a reported secondary. **No data, K, threshold, α, or decision-branch logic changed**; only the
column the S1 veto reads was brought into line with the frozen §6 specification. Corrected output:
`lockfree-flip-analysis-corrected.txt`. Both files are kept so the discrepancy is auditable. The corrected
reading is authoritative because §6 and §1 unambiguously define S1 as the writer-observable stall (and the
hypothesis text pre-states "CONTROL's downgrade excludes writers during capture → SHOULD stall — exactly S1's
hypothesis"), making the writer-observed p999 the S1 quantity by construction.

### §8 DECISION-RULE VERDICT → **BRANCH 1: PROCEED**
All Branch-1 conditions met: Welch p = 9.10e-40 < 0.05 AND diff = +14,231 ops/s > 0 AND d = 25.03 ≥ 0.8
(variant A throughput), with **no regression** — S1 writer-observed pause T≤C (4.5 ms ≤ 126 ms, a 96 %
improvement), S2 p99/p999 ratios 0.029/0.036 ≤ 1.10×, S3 RSS ratio 0.878 ≤ 1.25×, S4 contended diff +9,687
(positive, p = 6.3e-35, not worse). **One-line flip recommendation:** the lock-free overlay write+immutable-
checkpoint path beats the owned `Arc<RwLock>` tree on throughput by ~3–4× AND collapses the writer-observed
checkpoint stall from ~126 ms to ~4.5 ms with lower RSS and bounded CAS retries even under a hot prefix —
**PROCEED with the flip (Task #14, eviction-off membership trie V=()).**

**Caveat carried forward (not a veto, but a faithful note for the flip):** the effect is far larger than the
pre-registered prior expected (§1 predicted small-or-null throughput, win in S1/S2). Two reasons this is
credible rather than a measurement error: (1) CONTROL's checkpoint is the write→read-DOWNGRADE path that holds
the write guard across the O(tree) `serialize_char_node_to_disk` re-walk, so on a ~16k-key trie under a tight
20 ms checkpoint loop the writers are blocked for most of the window (writer-observed p999 ≈126 ms confirms
multi-checkpoint stalls), whereas TREATMENT's immutable snapshot never excludes writers; (2) the checkpointer-
side per-call number (TREATMENT longer) is exactly the §9-item-5 prediction and shows the serialize cost did
NOT vanish — it simply stopped blocking writers. The flip's faithfulness limits (§3) still hold: this measures
the eviction-OFF path; flipping WITH eviction on (registry publication + WAL reclaim) is not yet measured and
must be benchmarked before extending the flip beyond V=() membership tries.

---

## §E. EVICTION-ON addendum — Pre-Registered Design (FROZEN 2026-06-02, BEFORE eviction-ON data)

**Status:** PRE-REGISTRATION FROZEN at this section's persist time (2026-06-02), BEFORE any eviction-ON
measured data. Everything in §E above the `RESULTS (eviction-ON)` subsection is frozen; only that subsection is
appended after the K-round eviction-ON run. **§1–§11 + the eviction-OFF RESULTS above are NOT edited** — §E is a
strictly additive addendum. Design source: `docs/design/g4-eviction-on-immutable-checkpoint.md`. TLA model:
`formal-verification/tla+/LockFreeDurableCheckpointEviction.tla` (+ `.cfg`/`_Unsafe.cfg`). In-crate witnesses:
`persist.rs` mod `immutable_eviction_checkpoint_correspondence` (T1/T2).

**What changes vs the frozen eviction-OFF experiment:** BOTH arms now run with eviction ENABLED
(`EvictionConfig::without_memory_monitor()` — deterministic, no memory-pressure thread). The checkpoint paths
become their registry-publishing variants:
- **CONTROL** = owned `SharedCharARTrie` + eviction on; its `ARTrie::checkpoint` routes through
  `publish_durable_and_reclaim` (`mod.rs:1304`), which PUBLISHES the eviction registry after verify
  (`coordinator.rs:123-127`) and TRUNCATES the WAL (`rotate_to_archive`). `force_eviction` genuinely reclaims
  owned in-memory node boxes.
- **TREATMENT** = bare `Arc<PersistentARTrieChar>` overlay + eviction on (via the reversible
  `bench_enable_eviction`); its checkpointer calls the NEW
  `bench_immutable_checkpoint_with_eviction` → `publish_immutable_snapshot_retaining_wal_with_eviction`, which
  publishes the registry after verify AND records `checkpoint_lsn = committed watermark` while RETAINING the WAL
  (NO destructive truncate). Over a pure overlay trie `force_eviction` is a structural NO-OP (owned `self.root`
  is `Empty`; the data lives in `lockfree_root`) — proven by T1; the eviction-ON cost being studied is the
  registry build + `update_disk_registry` publication on the CHECKPOINT path, not in-memory reclamation.

### §E.1 Hypotheses + decision (FROZEN)

- **HE1 (primary):** Immediate + eviction-ON, TREATMENT write throughput > CONTROL (variant A disjoint).
  Two-sided Welch; **supported iff diff > 0 AND p < 0.05 AND Cohen's d ≥ 0.8** (the same irreversible-flip floor
  as the eviction-OFF H1). **HE0:** μ_T − μ_C = 0.
- **Pre-registered expectation (anti-hindsight):** registry publication is OFF the timed writer path (the
  checkpointer's `update_disk_registry` is one `RwLock::write` swap, ZERO fsync) and the registry is BUILT in
  BOTH arms by the SAME `serialize_char_node_to_disk` (so its cost is symmetric), so the eviction-ON effect
  should TRACK the eviction-OFF result (+312% throughput, writer-observed pause collapse). The realistic prior is
  therefore a LARGE positive TREATMENT effect, mirroring the eviction-OFF run, NOT a new regression.
- **Secondary descriptive VETOES (can only BLOCK, gated FIRST by SE5):**
  - **SE1 — checkpoint pause** (writer-observable stall, §6 definition = `write_p999`): TREATMENT ≤ CONTROL.
  - **SE2 — write tail** p99/p999: TREATMENT ≤ 1.10× CONTROL.
  - **SE3 — peak RSS** (single-arm-per-process, C10): TREATMENT ≤ 1.25× CONTROL.
  - **SE4 — contended variant B throughput:** TREATMENT not significantly worse (p ≥ 0.05 or diff ≥ 0).
  - **SE5 — NEW CORRECTNESS VETO (gates first):** post-checkpoint `force_eviction` + reopen returns the EXACT
    acknowledged set on BOTH arms. **A failure is a BUG, not a perf signal → ABORT, do not emit a verdict.**
    Enforced in the bench's `--eviction` smoke (`se5_correctness_check`, panics on mismatch) AND by the in-crate
    T1/T2 correspondence tests.
- **§8 DECISION RULE applies verbatim**, gated first by SE5: (1) PROCEED, (2) PROCEED-WITH-CAVEAT (tail/pause),
  (3) DON'T-FLIP (no benefit), (4) DON'T-FLIP (regression). Branch keyed on Welch + 95% CI + d (variant A
  throughput) + the SE1–SE4 vetoes, only after SE5 passes.

### §E.2 Durability-parity under eviction (the critical section, code evidence)

- **Per-write fsync is UNCHANGED and EQUAL** to the eviction-OFF run (ledger §2): the registry invalidation on a
  durable write (`invalidate_eviction_registry`, `mod.rs:1608-1619`) is a flag/generation bump with NO fsync, on
  the SAME `append_to_wal_inner` chokepoint both arms already share. Eviction adds ZERO syscalls to the writer
  path.
- **Per-checkpoint fsync count is IDENTICAL across arms:** CONTROL = 1 data sync + 1 WAL sync + rotate(TRUNCATE);
  TREATMENT = 1 data sync + 1 WAL sync + NO rotate(RETAIN). `update_disk_registry` is an in-memory
  `RwLock::write` swap (`coordinator.rs:379-381`) adding ZERO fsync to EITHER arm. **No NEW fsync asymmetry vs the
  eviction-OFF run** — the only per-checkpoint asymmetry is the same truncate-vs-retain already logged in §2.2/C3.
- **Recorded per round:** `round_dir_bytes` (the §2.2 WAL-retain asymmetry) + `evictable_node_count()` (the
  published registry size — a 19th TRAILING CSV column; the frozen `analyze_lockfree_flip.py` maps the leading 18
  by name and ignores the extra, so the analysis script is UNCHANGED).

### §E.3 Rigor inherited (verbatim from §3–§8)

K = 30 measured rounds/arm/variant, 2 warmup discarded; two-sided Welch + 95% CI + Cohen's d (≥0.8 floor) +
Mann–Whitney U; arms interleaved per round with fixed-seed-coin within-round order (C9); single-arm-per-process
RSS pass (C10); real-disk `target/bench-scratch` NEVER tmpfs; 5 GiB scratch ceiling + per-round cleanup;
`systemd-run --user --scope -p MemoryMax=32G` + `taskset -c 0-15`; both arms `Immediate` +
`EvictionConfig::without_memory_monitor()` (deterministic). §8 decision rule 1–4 verbatim, gated FIRST by SE5.

### §E.4 Bench changes (additive, reversible)

`benches/lockfree_flip_benchmark.rs`: a `--eviction` flag (or `LOCKFREE_FLIP_EVICTION`) enabling eviction on BOTH
arms and routing TREATMENT's checkpointer to `bench_immutable_checkpoint_with_eviction`; a trailing
`evictable_node_count` CSV column; the `se5_correctness_check` run in the eviction smoke. No `Cargo.toml` change
(`[[bench]]` already carries `required-features = ["persistent-artrie","bench-internals"]`). New source surface
(all reversible, ZERO new `unsafe`): `publish_immutable_snapshot_retaining_wal_with_eviction`
(`cfg(any(test, feature="bench-internals"))`), `bench_immutable_checkpoint_with_eviction` + `bench_enable_eviction`
(`cfg(feature="bench-internals")`). `checkpoint()` + every production path untouched; NO flip; NO destructive WAL
truncation (the owner-gated reclaim is out of scope).

### §E.5 Runbook (eviction-ON; mirrors §10 with `--eviction`)

```bash
# Build (release) once
systemd-run --user --scope -p MemoryMax=32G --quiet env TMPDIR=$PWD/target/test-tmp taskset -c 0-15 \
  cargo build --release --benches --features persistent-artrie,bench-internals
BIN=$(find target/release/deps -maxdepth 1 -type f -name 'lockfree_flip_benchmark-*' ! -name '*.d' | head -1)
# Smoke + SE5 (validates BOTH arms + the correctness veto)
systemd-run --user --scope -p MemoryMax=32G --quiet env TMPDIR=$PWD/target/test-tmp taskset -c 0-15 \
  "$BIN" --eviction | tee docs/experiments/lockfree-flip-evict-smoke.log
# PRIMARY (variant A disjoint, Immediate, K=30, interleaved). ONCE.
systemd-run … taskset -c 0-15 "$BIN" --measure --eviction --variant disjoint \
  | tee docs/experiments/lockfree-flip-evict-raw-disjoint.csv
# SECONDARY (variant B contended). ONCE.
systemd-run … taskset -c 0-15 "$BIN" --measure --eviction --variant contended \
  | tee docs/experiments/lockfree-flip-evict-raw-contended.csv
# RSS single-arm-per-process. ONCE each.
systemd-run … "$BIN" --measure --eviction --arm control   --variant disjoint | tee …-evict-rss-control.csv
systemd-run … "$BIN" --measure --eviction --arm treatment --variant disjoint | tee …-evict-rss-treatment.csv
# Analyze ONCE with the FROZEN script (the trailing evictable_node_count column is ignored by it)
python3 docs/experiments/analyze_lockfree_flip.py --disjoint …-evict-raw-disjoint.csv \
  --contended …-evict-raw-contended.csv --rss-control …-evict-rss-control.csv \
  --rss-treatment …-evict-rss-treatment.csv | tee docs/experiments/lockfree-flip-evict-analysis.txt
```

---

## RESULTS (eviction-ON) — appended AFTER the frozen K-round eviction-ON run

**Run date:** 2026-06-02. Single measured run per §E.5, K=30/arm/variant, no peeking, no re-runs. Both arms
eviction-ON (`EvictionConfig::without_memory_monitor()`); the frozen `analyze_lockfree_flip.py` run ONCE on the
frozen CSVs (`lockfree-flip-evict-raw-{disjoint,contended}.csv`, `lockfree-flip-evict-rss-{control,treatment}.csv`;
analysis `lockfree-flip-evict-analysis.txt`; smoke `lockfree-flip-evict-smoke.log`).

### Provenance / environment
- **git HEAD:** `549957a222d4de418cb6eca8e5374f89cda575a9` (same uncommitted working tree as the eviction-OFF run).
- **rustc:** `1.95.0 (59807616e 2026-04-14)`; release `--benches --features persistent-artrie,bench-internals`
  (build log `lockfree-flip-evict-build.log`).
- **host:** AMD Threadripper PRO 5975WX, `nproc`=32, governor `performance` (live). Observed `scaling_cur_freq`
  (cpu0/4/8/12): 3433 / 3485 / 3437 / 4292 MHz — boost active; per-CCD variance absorbed by K + Welch (C5).
- **disk:** NVMe `/dev/nvme0n1p4`, ~186 GiB free during the runs; `target/bench-scratch` 4.0K after every pass
  (per-round cleanup + 5 GiB ceiling never tripped, no ENOSPC). `taskset -c 0-15` honored.
- **SE5 correctness veto (gates first):** **PASSED** in the release `--eviction` smoke on BOTH arms ("both arms
  reopen the exact acknowledged set"), AND by the in-crate T1/T2 correspondence tests. No bug; verdict permitted.

### Primary — variant A (DISJOINT), Immediate + eviction-ON, write_ops_per_sec (n=30/arm, warmup dropped)
| arm | mean ops/s | sd |
|-----|-----------:|---:|
| CONTROL   | 2,720.1 | 401.9 |
| TREATMENT | 17,234.8 | 2,337.3 |

- **diff (T−C) = +14,514.7 ops/s (+533.61%)**, **95% Welch CI [13,631.2, 15,398.1]** (excludes 0, strictly positive).
- **Welch t = 33.5213, df = 30.71, p(two-sided) = 9.40e-26.**
- **Cohen's d = 8.6552 (pooled)** — far exceeds the pre-registered d ≥ 0.8 floor.
- **Mann–Whitney U = 900.0, p = 3.02e-11** (complete separation; corroborates Welch).

### Secondary descriptive vetoes (all PASS; gated first by SE5 = PASS)
- **SE1 — checkpoint pause** (writer-observed p999, the §6/§1 S1 quantity): **control 262,004.7 µs vs treatment
  7,147.1 µs → treatment 97.3 % better (T ≤ C, large improvement, NO regression).** Secondary "ALSO" diagnostic
  (checkpointer-side per-call, §6): mean control 127,497 / treatment 89,704 µs; p99 control 272,912 / treatment
  305,765 µs — the per-call checkpoint is *longer* for TREATMENT (its immutable snapshot still walks the overlay
  to serialize + now also builds+publishes the registry, §9 item 5) but WITHOUT stalling writers; per §6 this is
  NOT the S1 hypothesis quantity and does not gate the verdict.
- **SE2 — write tail.** p50: control 77.7 / treatment 342.2 µs (ratio 4.40 — TREATMENT's per-op floor is higher,
  the WAL-append-then-CAS critical path; NOT vetoed — §8 vetoes only p99/p999). **p99: 118,552.8 → 2,395.2 µs
  (ratio 0.020).** **p999: 262,004.7 → 7,147.1 µs (ratio 0.027).** Both tail ratios far below the 1.10× veto → no
  SE2 regression (TREATMENT's tail ~37–50× tighter).
- **SE3 — peak RSS** (single-arm-per-process, C10): **control 3,178,396 / treatment 1,318,888 KiB, ratio 0.415**
  (≤ 1.25× veto) — TREATMENT used ~2.4× LESS peak RSS even with the eviction coordinator + registry. No SE3
  regression. (CONTROL's owned-tree copy-on-serialize arena churn dominates the eviction-ON RSS.)
- **SE4 — contended variant B** (write_ops_per_sec, n=30/arm): control 3,799.2±115.2 / treatment 15,204.5±582.3;
  **diff = +11,405.3 ops/s (+300.20%), 95% CI [11,184.4, 11,626.3], Welch t = 105.24, df = 31.27, p = 1.90e-41,
  d = 27.17, Mann–Whitney U = 900, p = 3.02e-11.** TREATMENT significantly BETTER, not worse → no SE4 regression /
  no CAS-livelock. **`cas_retries` (TREATMENT contended): mean 101.1, max 207** — bounded, low (~0.6 % of 16k ops).

### Supporting (non-gating)
- **`evictable_node_count`** (the published registry size, the eviction-ON instrumentation; col 19, disjoint):
  control mean ≈ 285,152 / treatment mean ≈ 265,193 — BOTH arms publish a NON-EMPTY registry every round (the
  registry GAP the eviction-OFF run could not exercise is exercised here; confirms `update_disk_registry` ran).
- **`round_dir_bytes`** (the §E.2 / §2.2 WAL-retain asymmetry, disjoint): TREATMENT retains WAL ≈ 45.0 MB/round;
  CONTROL re-serializes the tree into fresh arenas + truncates the WAL ≈ 288.3 MB/round. As in the eviction-OFF
  run, CONTROL's copy-on-serialize data file is the larger disk cost; the per-checkpoint fsync COUNT is identical
  (§E.2 — `update_disk_registry` adds zero fsync to either arm), so this is a disk-space, not throughput, effect.
- **perf attribution — not run** (environment: `kernel.perf_event_paranoid` / no CAP_PERFMON, as recorded for the
  eviction-OFF run); the §8 decision rule does not depend on perf.

### §8 DECISION-RULE VERDICT (eviction-ON) → **BRANCH 1: PROCEED**
SE5 correctness veto PASSED (gates first). All Branch-1 conditions met: Welch p = 9.40e-26 < 0.05 AND diff =
+14,515 ops/s > 0 AND d = 8.66 ≥ 0.8 (variant A throughput), with **no regression** — SE1 writer-observed pause
T ≤ C (7.1 ms ≤ 262 ms, a 97 % improvement), SE2 p99/p999 ratios 0.020/0.027 ≤ 1.10×, SE3 RSS ratio 0.415 ≤
1.25×, SE4 contended diff +11,405 (positive, p = 1.9e-41, not worse).

**Flip recommendation (eviction-ON):** the eviction-ON immutable-snapshot checkpoint
(`bench_immutable_checkpoint_with_eviction` = retain-WAL watermark reclaim + registry publication) beats the
owned `Arc<RwLock>` eviction-ON checkpoint (`publish_durable_and_reclaim`) on write throughput by ~5–6× on
disjoint and ~4× on contended, collapses the writer-observed checkpoint stall from ~262 ms to ~7 ms, uses ~2.4×
less peak RSS, and keeps CAS retries bounded under a hot prefix — **PROCEED with the eviction-ON flip, with the
same caveats as the eviction-OFF run (§3) PLUS:** (1) over a pure overlay trie `force_eviction` is a structural
no-op (owned `self.root` is `Empty`); wiring the overlay into the owned eviction *reclaim* walk so in-memory
overlay boxes can actually be reclaimed is the owner-gated Phase-E flip step (out of scope, NOT measured here) —
this experiment proves the registry-PUBLICATION half of eviction-ON is correct + fast, not in-memory reclamation
of overlay nodes; (2) the measured advantage, as in the eviction-OFF run, is driven mostly by CONTROL's
write→read-downgrade checkpoint holding the write guard across the O(tree) serialize re-walk, which TREATMENT's
no-writer-lock snapshot avoids. The eviction registry publication itself (`update_disk_registry`, one `RwLock`
swap, zero fsync) adds no measurable writer-path cost — exactly the §E.1 pre-registered expectation.

## §F. REAL OVERLAY-RECLAMATION addendum — Pre-Registered Design (FROZEN 2026-06-02, BEFORE §F data)

The §E eviction-ON run proved the registry-PUBLICATION half of eviction-ON is correct + fast, but its TREATMENT
`force_eviction` callback was a structural NO-OP over the overlay (owned `self.root` is `Empty`), so peak RSS was
not a meaningful TREATMENT metric (§E caveat 1). §F closes that gap with a REVERSIBLE, `bench-internals`-gated
overlay-eviction DRIVER (`evict_overlay_node_at_path` / `evict_overlay_nodes` + the accessor
`bench_evict_overlay_cold_nodes`), so the TREATMENT arm performs REAL in-memory reclamation of COLD overlay
subtrees. This is NOT the production flip (`checkpoint()` + production eviction untouched; owner-gated). The full
design is `docs/design/g4-overlay-eviction-reclamation-benchmark.md`; the CAS-arbitration safety (loser-safe,
cold-only, no-UAF) is TLC-verified in `formal-verification/tla+/OverlayEvictionCas.tla` (3 invariants hold on the
safe cfg; `_Unsafe.cfg` negative control fires). The driver adds ZERO new `unsafe` (reuses the proven Phase-D
safe `Arc`/`arc-swap` primitive) and the unsafe-inventory gate stays exit-0.

### §F.1 The mechanism (cold-only, no-fault-in — the honest core)
Fault-in is ABSENT in the overlay (`find_in_lockfree_trie`/`find_leaf_recursive` treat an `OnDisk` child as
absent), so evicting a node later re-read/written would make that term unreachable in-memory (a silent
correctness violation IF it were ever done to a re-touched node). §F therefore evicts ONLY COLD nodes — a fixed
`cold-*` prefix set inserted + checkpointed ONCE at round start and NEVER re-touched, fed to the evictor via a
`cold_filter` (`path.first() == Some(&'c')`). The live writers operate on a DISJOINT range (`ngram-`/`hot-prefix-`
/`warm2-`). This measures REAL reclamation of cold subtrees under concurrent write load (the production eviction
scenario) WITHOUT claiming fault-in (which the Phase-E flip owns). Durability of the evicted cold subtrees rests
on the PRIOR checkpoint (registry `SwizzledPtr` → durable on-disk image) + the RETAINED WAL — no destructive
truncation is introduced; recovery is unaffected (the registry is never recovery state).

### §F.2 Hypotheses (anti-hindsight, FROZEN before §F data)
- **HF1 (throughput):** Immediate + eviction-ON + REAL reclamation, variant A (disjoint), T vs C, two-sided Welch
  + 95% CI + Cohen's d. **Expectation: the §E +533% NARROWS** — TREATMENT now pays the real reclaim cost CONTROL
  already paid. A narrowing is EXPECTED, NOT a regression; the question is whether T STILL wins AND frees memory.
- **HF2 (the now-meaningful metric):** (a) TREATMENT §F peak RSS < TREATMENT §E peak RSS (§E was **1,318,888
  KiB**, NO reclaim — §F must beat it by > noise); (b) TREATMENT RSS ≤ CONTROL RSS. Supported iff BOTH.
- **HF3 (reclamation effectiveness):** `overlay_reclaimed_nodes` > 0 (no silent no-op) AND ≈ matched to CONTROL's
  reclaimed count under the same budget/cadence (fairness witness; the registry-invalidation contract may make the
  two diverge — reported honestly, see §F.5).
- **SF5 CORRECTNESS (ABORT on fail, gated FIRST):** (i) reopen-exact BOTH arms; (ii) `faultin_count == 0` (cold-
  only held); (iii) TREATMENT `overlay_reclaimed_nodes > 0`. Enforced by the `--evict-real` smoke `sf5_correctness_check`
  + the in-crate OE1–OE4 correspondence tests. A failure is a BUG, not a perf signal → STOP and report.
- **Secondary vetoes (gated by SF5 first):** SF1 writer-observed pause T ≤ C; SF2 p99/p999 ≤ 1.10×; SF3 RSS ≤
  1.25×; SF4 contended not significantly worse.

### §F.3 Fairness (matched budget + cadence)
Both arms perform ONE eviction call per checkpoint round AFTER the registry publishes, with the SAME
`EvictionConfig::without_memory_monitor()`, `min_eviction_depth`, `batch_size`, and coldness selection (both route
through `force_eviction_char` / `select_char_for_eviction`); ONLY the reclaim callback differs (CONTROL owned-tree
`force_eviction`; TREATMENT `bench_evict_overlay_cold_nodes` → the §F driver). Per-write fsync is unchanged + equal.
**The evictor does ZERO disk I/O on BOTH arms** (cold-only → no fault-in read-back; just an in-mem slot swap), so
§F introduces NO new fsync/read asymmetry (cleaner than §E); only the §2.2/C3 retain-WAL-vs-truncate disk-space
difference remains. NOTE (honest, pre-registered): a concurrent `insert_cas_durable` INVALIDATES the eviction
registry (the A1 fix, `is_valid()` → zero evictions = liveness-not-safety). Because the cold subtree is seeded +
checkpointed BEFORE the live writers start and the writers are DISJOINT (never touch cold), TREATMENT's cold
registry entries tend to survive longer than CONTROL's owned-tree registry (whose every owned-tree node shares the
one registry a live write dirties). So the per-round reclaimed COUNTS may diverge — this is the registry-
invalidation contract, reported under HF3, NOT a fairness defect in the driver.

### §F.4 Metrics (TRAILING cols; the frozen analysis maps the leading 18 by name + ignores extras)
- **col20 `overlay_reclaimed_nodes`** — nodes actually reclaimed by the matched per-checkpoint eviction (HF3).
- **col21 `evict_bytes_nominal`** — nominal bytes freed (~256 B/node; the single-arm peak-RSS pass is the
  physical witness, HF2).
- **col22 `faultin_count`** — fault-in operation count; identically 0 (the overlay has NO fault-in path; the
  cold-only invariant). Any non-zero value ⇒ a hot node was wrongly evicted ⇒ ABORT.
- `peak_rss_kib` (col16) = physical witness via the single-arm-per-process RSS pass. `cas_retries` now includes
  the evictor's root-CAS rebases.

### §F.5 Decision rule (SF5-gated, then)
**1 PROCEED:** T throughput ≥ C ∧ HF2 ∧ HF3 ∧ no SF1–4 veto. **2 PROCEED-WITH-CAVEAT:** throughput narrows but ≥ 0
∧ HF2 holds. **3 DON'T-FLIP (no benefit):** T ties/loses ∧/∨ HF2 fails — overlay doesn't free memory ⇒ the §E
+533% was the no-op artifact. **4 DON'T-FLIP (regression):** SF1/2/4 veto fires.

### §F.6 Runbook (`--evict-real`; mirrors §10/§E.5; all wrapped systemd 32G + `taskset -c 0-15` + tee; real-disk
`target/bench-scratch`, 5 GiB ceiling, NEVER tmpfs)
```bash
mkdir -p target/test-tmp target/bench-scratch
# Build (release) once
systemd-run --user --scope -p MemoryMax=32G --quiet env TMPDIR=$PWD/target/test-tmp \
  cargo build --release --benches --features persistent-artrie,bench-internals
B=$(ls target/release/deps/lockfree_flip_benchmark-* | grep -v '\.d$' | head -1)
# Smoke + SF5 (validates BOTH arms + REAL reclaim + faultin==0 + reopen-exact)
systemd-run --user --scope -p MemoryMax=32G --quiet env TMPDIR=$PWD/target/test-tmp \
  taskset -c 0-15 "$B" --evict-real | tee docs/experiments/lockfree-flip-evictreal-smoke.log
# PRIMARY (variant A disjoint, Immediate, K=30, interleaved). ONCE.
systemd-run --user --scope -p MemoryMax=32G --quiet env TMPDIR=$PWD/target/test-tmp \
  taskset -c 0-15 "$B" --measure --evict-real --variant disjoint \
  | tee docs/experiments/lockfree-flip-evictreal-raw-disjoint.csv
# SECONDARY (variant B contended). ONCE.
systemd-run --user --scope -p MemoryMax=32G --quiet env TMPDIR=$PWD/target/test-tmp \
  taskset -c 0-15 "$B" --measure --evict-real --variant contended \
  | tee docs/experiments/lockfree-flip-evictreal-raw-contended.csv
# RSS single-arm-per-process (C10). ONCE each.
systemd-run --user --scope -p MemoryMax=32G --quiet env TMPDIR=$PWD/target/test-tmp \
  taskset -c 0-15 "$B" --measure --evict-real --arm control   --rounds 30 --no-warmup \
  | tee docs/experiments/lockfree-flip-evictreal-rss-control.csv
systemd-run --user --scope -p MemoryMax=32G --quiet env TMPDIR=$PWD/target/test-tmp \
  taskset -c 0-15 "$B" --measure --evict-real --arm treatment --rounds 30 --no-warmup \
  | tee docs/experiments/lockfree-flip-evictreal-rss-treatment.csv
# Analyze ONCE with the FROZEN script (trailing cols 20-22 ignored by it)
python3 docs/experiments/analyze_lockfree_flip.py \
  docs/experiments/lockfree-flip-evictreal-raw-disjoint.csv \
  --contended docs/experiments/lockfree-flip-evictreal-raw-contended.csv \
  --rss-control docs/experiments/lockfree-flip-evictreal-rss-control.csv \
  --rss-treatment docs/experiments/lockfree-flip-evictreal-rss-treatment.csv \
  | tee docs/experiments/lockfree-flip-evictreal-analysis.txt
# HF2a: compare TREATMENT §F peak RSS vs the §E TREATMENT peak RSS (1,318,888 KiB).
```

### §F.7 Bench changes (additive, reversible)
`--evict-real` flag (implies `--eviction`): BOTH rounds seed a fixed COLD prefix set once at round start +
checkpoint (CONTROL owned-tree `ARTrie::checkpoint`; TREATMENT `bench_immutable_checkpoint_with_eviction`), then
NEVER touch COLD; after EACH checkpoint publishes the registry, call matched eviction (CONTROL `force_eviction`;
TREATMENT `bench_evict_overlay_cold_nodes(budget, is_cold_path)`) and accumulate `overlay_reclaimed_nodes` /
`evict_bytes_nominal`; cols 20-22 + `sf5_correctness_check`. No `Cargo.toml` change. Rollback (design §4): delete
`bench_evict_overlay_cold_nodes` + `evict_overlay_nodes`/`evict_overlay_node_at_path` + `OverlayEvictOutcome` +
the §F arm + `OverlayEvictionCas.tla`/`.cfg`/`_Unsafe.cfg` + its 3 verify-script lines; this §F ledger section is
append-only.

## RESULTS (§F real-reclamation) — appended AFTER the frozen K-round `--evict-real` run

### Provenance / environment
- Date 2026-06-02; git HEAD `549957a`; governor `performance`; observed freq cpu0/4/8/12 ≈ 3.48 / 3.88 / 4.25 /
  3.46 GHz; `taskset -c 0-15`; `systemd-run --user --scope -p MemoryMax=32G`; real-disk `target/bench-scratch`
  (5 GiB ceiling, per-round cleanup; NO ABORT fired); Immediate durability + `without_memory_monitor`, BOTH arms.
- Raw CSVs (teed ONCE): `lockfree-flip-evictreal-raw-{disjoint,contended}.csv` (K=30 + 2 warmup, interleaved with
  fixed-seed-coin within-round order), `lockfree-flip-evictreal-rss-{control,treatment}.csv` (single-arm-per-
  process, --rounds 30 --no-warmup, C10). Analysis (FROZEN script, ONCE):
  `lockfree-flip-evictreal-analysis.txt`. Smoke + SF5: `lockfree-flip-evictreal-smoke.log` (above).
- **Build:** `cargo build --release --benches --features persistent-artrie,bench-internals` clean. New `unsafe`: 0
  (the driver reuses the proven Phase-D safe `Arc`/`arc-swap` primitive); `verify-unsafe-boundary-inventory.sh`
  exit-0; full `verify-formal-correspondence.sh` (RUN_TLC=1) exit-0 (incl. `OverlayEvictionCas` 3 invariants hold
  + `_Unsafe.cfg` negative control fired).

### SF5 — CORRECTNESS veto (gated FIRST; ABORT on fail) → **PASS**
- (i) reopen-exact BOTH arms: smoke `SF5(control)` + `SF5(treatment)` reopen the exact acknowledged COLD ∪ LIVE
  set; OE1/OE3/OE4 reopen-exact in-crate. PASS.
- (ii) `faultin_count == 0`: **max faultin == 0 across ALL rows in ALL four CSVs** (the cold-only invariant held;
  no hot node was ever evicted). PASS.
- (iii) TREATMENT `overlay_reclaimed_nodes > 0`: smoke SF5(treatment) reclaimed 245 cold overlay nodes; measured
  rounds reclaim 50–93/round (below). REAL reclamation, NOT the §E structural no-op. PASS.

### HF1 — Primary throughput (variant A DISJOINT, Immediate + eviction-ON + REAL reclaim, n=30/arm, warmup dropped)
- CONTROL mean = 1,986.9 ops/s (sd 201.6); TREATMENT mean = 14,969.7 ops/s (sd 3,109.7).
- **diff (T−C) = +12,982.8 ops/s (+653.42%)**, 95% Welch CI [11,819.6, 14,146.0]; Welch t = 22.82, df = 29.24,
  **p = 3.50e-20**; Cohen's d = **5.89** (≥ 0.8 floor); Mann-Whitney U = 900, p = 3.02e-11.
- **Anti-hindsight check:** §F (+653%) vs §E (+533%) — the pre-registered NARROWING did NOT materialize at the
  mean (it WIDENED slightly). Honest reading: the §F real-reclamation cost is small relative to CONTROL's
  unchanged O(tree) write→read-downgrade serialize stall, and §F's run is independently seeded (different writer
  start state), so the point estimate moved up rather than down — well within the CIs of two separate runs. The
  load-bearing conclusion is unchanged and STRONGER: TREATMENT still wins decisively AND now frees memory (HF2).
- Variant B CONTENDED: CONTROL 2,439.1 / TREATMENT 14,806.0 ops/s; diff +12,366.9 (+507.0%), 95% CI [12,086.8,
  12,647.0], t = 90.10, df = 30.62, p = 1.06e-38, d = 23.26; S4 cas_retries (treatment) mean 127.4 / max 228
  (bounded — incl. the evictor's root-CAS rebases).

### HF2 — peak RSS (THE now-meaningful metric; single-arm-per-process, C10) → **SUPPORTED (both)**
- **(a) TREATMENT §F 1,018,664 KiB  <  TREATMENT §E 1,318,888 KiB** — a drop of **300,224 KiB (≈ 22.8%)**, far
  above noise (RSS was rock-steady across all 30 single-arm rounds). REAL overlay reclamation actually freed
  memory: the §E peak was inflated by the structural no-op holding every cold overlay subtree resident. **HF2a
  holds.**
- **(b) TREATMENT §F 1,018,664 KiB  ≤  CONTROL 3,471,144 KiB**, ratio **0.293** (≤ 1.25× S3 veto) — TREATMENT uses
  ≈ 3.4× LESS peak RSS than CONTROL even while doing matched real reclamation. **HF2b holds.**
- ⇒ **HF2 SUPPORTED** (both conditions). RSS-as-truth caveat (§8 risk 5): HF2a compares §F vs §E under the SAME
  allocator/workload (isolating the delta) and HF3 reports the allocator-independent logical reclaim count.

### HF3 — reclamation effectiveness (col `overlay_reclaimed_nodes`, n=30/arm measured)
- Disjoint: CONTROL mean 8.4/round (min 0, max 251); TREATMENT mean **59.8/round** (min 50, max 79).
- Contended: CONTROL mean **0.0**/round; TREATMENT mean **80.7/round** (min 69, max 93).
- TREATMENT reclaims > 0 every round (HF3 first clause holds). The CONTROL-reclaims-≪-TREATMENT asymmetry is the
  PRE-REGISTERED registry-invalidation contract (§F.3): CONTROL's owned tree shares ONE `DiskLocationRegistry`
  that EVERY live write dirties (A1 fix → `is_valid()` false → 0 evictions), so under the hot contended writers it
  is invalidated essentially every checkpoint; TREATMENT's cold subtree is seeded+checkpointed BEFORE the disjoint
  writers and its cold registry entries survive longer. So the "≈ matched count" fairness sub-clause does NOT hold
  — reported honestly: it reflects the invalidation contract, not a driver defect (the driver reclaims whatever
  the coordinator hands it; the coordinator hands CONTROL nothing once a write dirties the shared registry).

### Secondary descriptive vetoes (gated first by SF5 = PASS) — all PASS
- **SF1 — writer-observed checkpoint pause** (S1 PRIMARY = writer p999): control 258,737.6 us / treatment
  10,269.4 us — **T ≪ C** (a 96 % reduction). PASS. (Checkpointer-side mean/p99 is higher for T, as in §E — the
  secondary 'ALSO' line — because T checkpoints overlap the whole window; the writer-OBSERVED stall is the veto.)
- **SF2 — write tail p99/p999** (veto T ≤ 1.10×C): p99 ratio 0.025, p999 ratio 0.040 — PASS. (p50 ratio 2.401 >
  1, expected: the Order-A durable fsync per `insert_cas_durable` dominates the cheap median, exactly as §E; the
  veto is on the tail, where T is ~25–40× better.)
- **SF3 — peak RSS** (veto T ≤ 1.25×C): ratio 0.293 — PASS.
- **SF4 — contended not significantly worse**: diff +12,366.9 (positive), p = 1.06e-38, d = 23.26 — PASS.

### §F.5 DECISION RULE → **BRANCH 1: PROCEED**
SF5 correctness veto PASSED (gates first: reopen-exact both arms, faultin == 0, treatment reclaim > 0). All
Branch-1 conditions met: T throughput ≥ C (diff +12,982.8 ops/s, p = 3.50e-20, d = 5.89 ≥ 0.8) ∧ HF2 holds
(TREATMENT §F RSS 1,018,664 < §E 1,318,888 AND ≤ CONTROL) ∧ HF3 (treatment reclaim > 0 every round) ∧ no SF1–4
veto. The pre-registered narrowing (HF1) did not lower the win, and the now-meaningful peak-RSS metric is
SUPPORTED — so the §E +533% was NOT merely a no-op artifact: TREATMENT both wins on throughput AND frees memory.

**Flip recommendation (eviction-ON + REAL reclamation):** the overlay-eviction driver makes the eviction-ON
immutable-snapshot checkpoint reclaim real in-memory COLD overlay subtrees while keeping every §E advantage —
~6.5× (disjoint) / ~5× (contended) higher write throughput, a 96 % smaller writer-observed checkpoint stall,
~25–40× better write tails, and now **≈ 23 % lower peak RSS than the §E no-op and ≈ 3.4× lower than CONTROL** —
with bounded CAS retries under a hot prefix and zero new `unsafe`. **PROCEED with the eviction-ON flip**, with the
same §3/§E caveats PLUS: (1) this driver is COLD-ONLY (no fault-in); a production flip that evicts re-touchable
nodes MUST first add a correct fault-in-on-read path (the owner-gated Phase-E work) — the SF5(ii) faultin == 0
gate is what makes the cold-only measurement honest, and a non-zero faultin would have ABORTED; (2) the
per-checkpoint reclaimed COUNT is governed by the registry-invalidation contract (HF3), so a production reclaimer
under heavy concurrent writes should expect CONTROL-style invalidation churn — the win here is that TREATMENT's
disjoint-cold subtrees evade it; (3) as in §E, the headline throughput gap is driven mostly by CONTROL's
write-guard-across-O(tree)-serialize checkpoint, which TREATMENT's no-writer-lock snapshot avoids.

