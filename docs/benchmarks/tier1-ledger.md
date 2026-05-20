# libdictenstein — ARTrie Tech-Debt Repair Ledger

Scientific-method ledger per `~/.claude/CLAUDE.md`. One entry per Tier 1/Move fix.
Authoritative plan: `/home/dylon/.claude/plans/rust-backtrace-1-rust-log-debug-cargo-n-purrfect-lemon.md`.

## Hardware

See `/home/dylon/.claude/hardware-specifications.md` for the canonical machine spec
used in every entry below. Any deviation (different machine, different governor) must be
noted in the entry's `Setup:` field.

## Methodology

- **CPU affinity**: `taskset -c 0-15 <cmd>` (or per-entry override).
- **Frequency**: `sudo cpufreq-set -g performance` (verify with `cpufreq-info`).
- **Disable turbo if measuring tail-latency stability**: `echo 1 | sudo tee /sys/devices/system/cpu/intel_pstate/no_turbo`.
  Note in `Setup:` whether turbo was on/off.
- **Output capture**: pipe with `tee`; never re-run a command to recover a different field of the output.
- **Multiple iterations**: criterion's default is fine for throughput; for tail-latency record `--measurement-time 60` and confirm sample size ≥ 100.
- **`perf record --call-graph lbr`** for CPU profiles; flamegraphs saved under `docs/benchmarks/artifacts/`.
- **`valgrind --tool=massif`** for memory profiles where relevant.

## Entry template

```markdown
## [Phase N] — [Item, e.g. T1-4 GroupCommitCoordinator wiring]

**Date:** YYYY-MM-DD
**Commit (before):** <sha>
**Commit (after):** <sha>
**Hypothesis:** <what you expect to change and by how much>
**Setup:**
- Hardware: per `~/.claude/hardware-specifications.md`
- Governor: performance / turbo: <on|off>
- CPU affinity: <cores>
- Bench command: <verbatim>
- Workload params: <dictionary size, term count, thread count, etc.>

**Before:**
- <metric>: <value> (p50 / p99 / throughput)
- <metric>: <value>

**After:**
- <metric>: <value>
- <metric>: <value>

**Result:** <hypothesis confirmed | partially confirmed | disconfirmed>; <delta vs. expected>

**Decision:** <ship | hold | re-test with X>

**Artifacts:**
- `docs/benchmarks/artifacts/<date>-<item>-flame.svg`
- `docs/benchmarks/artifacts/<date>-<item>-raw.log`
- (optional) massif: `<date>-<item>-massif.out`

**Notes / follow-ups:** <anything surprising; new hypotheses for the next entry>
```

## Entries

### [Phase 0] — Hygiene baseline

**Date:** 2026-05-20
**Commit (before):** ab0ec59 (master HEAD pre-plan)
**Commit (after):** (Phase 0 not yet committed)
**Scope:** No code paths touched; only `.pgmcp.toml` creation, docs dead-link fix, and this ledger.
**Verification:** `cargo check --all-features` passes (recorded in Phase-0 task #5).

**Audit findings deferred to later phases:**

- `src/persistent_artrie/memory_monitor.rs:497 read_psi_metrics` is `#[cfg(target_os = "linux")]` *and* `#[allow(dead_code)]`. The function is genuinely unwired — `grep` confirms zero callers. The `#[allow(dead_code)]` is honest; will be removed when the function is wired into `MemoryMonitor::poll()` during a later phase (Tier 4 cleanup, currently scheduled in Phase 1's core-extraction window because `memory_monitor.rs` moves to core then). No change in Phase 0.
- `src/persistent_artrie/concurrency.rs:411 EpochGuard.epoch` is captured at construction (`enter_read()` returns it) but never read — including in `Drop`. The plan's framing ("read by epoch-debug assertions") does not match current source. The honest action is to either (a) actually use the field (add a debug assertion in `Drop` comparing the captured epoch against `manager.current_epoch()` for snapshot consistency), or (b) remove the field. Both are out of scope for Phase 0 (the latter is destructive; the former is a behavior change). Deferred to Phase 1's `concurrency.rs` move to core, where the right call can be made once the surrounding MVCC machinery is in its final home. No change in Phase 0.

(no further entries yet)
