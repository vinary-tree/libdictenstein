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

### [Phase 2] — Tier 1 correctness + Tier 2 trait/test work (partial)

**Date:** 2026-05-20
**Commits:** 0142cd3 (T1-6, T2-4, T2-5), 7be0d0d (T2-3), 8f738fe (T1-5), 0c1420e (T1-4 partial)
**Items completed in this batch:**

- **T1-5** — Three `#[ignore]`'d stress tests (`test_stress_highly_diverse_terms`, `test_stress_mixed_operations`, `test_stress_bulk_delete` in `tests/persistent_artrie_stress.rs:148, 198, 471`) all pass cleanly when run; removed the stale `#[ignore]` markers and audit-described TODO commentary. The underlying bugs (recursive bucket splitting, mixed insert/remove length-accounting, remove length-counter drift) were fixed in source at some earlier point — only the test markers remained.
- **T1-6** — Three `panic!` sites in clone / serialization paths replaced with principled error handling:
  - `vocab/types.rs:352` and `char/types.rs:499` (Clone-time): `.expect("invariant: …")` with forensic message. The "Node*::grow rejected child key" branch is genuinely unreachable under documented invariants.
  - `char/dict_impl_char.rs:6657` (`build_disk_char_node` during serialization): function signature promoted to `Result<CharNode>`, error propagates via `?` instead of crashing. Caller updated.
- **T2-3** — `per_node_log_char.rs` now re-exports `NodeRecoveryResult` and `RecoveryResult` (previously missing from its re-export list). The audit's "add CharNodeLogManager ~150 LOC" was based on a type that does not exist in the byte implementation; no port was actually required.
- **T2-4** — Removed the entire `impl ARTrie for PersistentVocabARTrie` block. All 10 mutation methods (`insert`, `remove`, `checkpoint`, `sync`, `upsert`, `increment`, `enable_slot_tracking`, `flush_sequential`, `insert_with_value`, `remove_prefix`) silently returned `false`/`Err` because `&self` cannot mutate the trie. Verified no production caller depended on this impl; `SharedVocabARTrie` provides the same surface with working mutation via `Arc<RwLock<…>>`.
- **T2-5** — Verified the 6 commented-out `SharedCharTrie` tests at `char/dict_impl_char.rs:9353` actually need no new types: `ARTrie for SharedCharARTrie<V>` at `char/mod.rs:863+` already implements `current_lsn`, `synced_lsn`, `upsert`, `sync`. Added 4 working tests; they import `crate::artrie_trait::ARTrie` to bring the trait methods into scope.
- **T1-4 partial** — Commented out the legacy `GroupCommit` stub at `wal.rs:1741-1778` (the one whose `append_sync` synced every record despite claiming to batch) and removed its re-export from `persistent_artrie/mod.rs:295`. The actual batched-WAL path is `DurabilityPolicy::GroupCommit` → `WalWriter::sync_async` → `AsyncWalWriter`, which was already correctly wired.

**Tests:** `cargo test --features persistent-artrie --lib`: 1572 passed, 0 failed. `cargo test --all-features --lib`: 1649 passed, 0 failed.

**Result:** Items as scoped (T1-5/T1-6/T2-3/T2-4/T2-5) and the audit-named "stub" piece of T1-4 are resolved.

**Decision:** Items complete; advance to remaining Phase 2 items (T1-1 char/vocab eviction implementation, T1-2 `Dictionary::transition` disk resolution, T1-3 lockfree vocab disk resolution, and the deeper T1-4 work of wiring `GroupCommitCoordinator` through the per-record write paths).

### [Phase 1] — Mechanical core extraction (Move 1)

**Date:** 2026-05-20
**Commit (before):** bf81f7f (Phase 0)
**Hypothesis:** Relocating unit-agnostic infrastructure into `persistent_artrie_core` (file moves + import rewrites only) leaves all 1568 unit tests green with zero behavior change, and breaks the audit's cycle between `persistent_artrie` ↔ `persistent_artrie_char` / `persistent_vocab_artrie`.
**Setup:** `cargo test --features persistent-artrie --lib` after every batch of moves; `grep -rn "crate::persistent_artrie_char\|crate::persistent_vocab"` over both `src/persistent_artrie_core/` and `src/persistent_artrie/` to verify the layering invariant.
**Before:**
- Cycle: `persistent_artrie::mvcc:72 -> persistent_artrie_char::nodes::PersistentCharNode` (byte → char back-edge)
- Cycle: `persistent_artrie::block_storage::read_vocab_header -> persistent_vocab_artrie::types::VocabTrieFileHeader` (byte → vocab back-edge)
- `MmapDiskManager::read_vocab_header` and `IoUringDiskManager::read_vocab_header` convenience methods on `persistent_artrie/disk_manager.rs:1248` and `persistent_artrie/io_uring_disk_manager.rs:1150` mirror the layering violation
- `DurabilityPolicy` enum defined inside byte's `dict_impl.rs:394`; vocab imports it across variant boundary
- `ArenaSlot` defined inside byte's `arena_manager.rs:151`; would have to cross into core for `swizzled_ptr.rs` to move
- 21 files in `persistent_artrie/` that are unit-agnostic but live in the byte variant module
**After:**
- All three invariants pass empty:
  - `grep "crate::persistent_artrie_char\|crate::persistent_vocab"` in `src/persistent_artrie_core/` → empty
  - Same grep in `src/persistent_artrie/` → empty
  - `grep "use crate::persistent_artrie::"` in `src/persistent_artrie_core/` → empty
- New module `src/persistent_artrie_core/` with 21 sub-modules: `adaptive_pool`, `arena_slot`, `block_storage`, `buffer_manager`, `compact_encoding`, `concurrency`, `dirty_tracker`, `disk_manager`, `durability`, `epoch`, `error`, `eviction/`, `group_commit`, `io_uring_disk_manager`, `key_encoding`, `memory_monitor`, `mvcc`, `prefetch`, `recovery`, `swizzled_ptr`, `wal`
- `persistent_artrie_char/mvcc.rs` carries `impl TrieRoot for PersistentCharNode` (eliminates the byte → char import)
- `persistent_vocab_artrie/header.rs` carries `read_vocab_header(&impl BlockStorage)` (eliminates the byte → vocab call into vocab types)
- `byte/mvcc.rs` is now a 41-line shim with `impl TrieRoot for PersistentNode` + re-exports
- 1568 unit tests pass; zero failures, zero ignored
- `cargo check --all-features`: clean (57 pre-existing warnings, unchanged count)
**Result:** Hypothesis confirmed. Cycle broken, behavior unchanged.
**Decision:** Ship; advance to Phase 2 (Tier 1 correctness fixes).
**Artifacts:** N/A — pure structural change, no perf delta expected (and none measured: file moves cannot change runtime behavior).

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
