# Slice 3 / F5 — direct dense→overlay reopen loader (implementation notes)

Status: S1 (land, gated OFF) → S2 (prove) → S3 (switch). This document records the
**code-verified** F5-B implementation that lands alongside the existing reopen path
(F7 — owned-tree deletion — is a LATER chunk and is NOT done here).

Authoritative plan: `docs/design/slice3-f5-f7-execution-plan.md` (F5 section).

## (a) F5-B + the walk-converter design (FINAL — term-enumeration converter)

We ship **F5-B** (the plan's recommendation, minimal new data-loss-critical surface):

1. **`load_root_immutable`** (per variant, `src/persistent_artrie{,_char}/f5_loader.rs`) —
   reuse the EXISTING owned loader `load_root_from_disk` (char, `eager_depth =
   Some(usize::MAX)`) / `load_root_from_disk_with_arena` (byte, already fully eager) to
   decode the dense disk image into the owned `CharTrieRoot`/`TrieRoot` as a **TRANSIENT
   scratch in `self.root`**, then call the generic converter, install the result, and
   **CLEAR the transient owned tree** (so reopen does NOT leave the owned tree as a
   production representation — the F7 prerequisite).

2. **The converter — `LockFreeOverlay::build_overlay_root_from_owned` (generic, in
   `overlay/flip.rs`).** ⚠️ **PIVOTED from node-structural to term-enumeration during
   implementation** because of compaction (see below). It enumerates every owned
   `(term-units, Option<V>)` + the empty term "" via the SAME D1 un-routed owned seam
   readers (`owned_first_units` / `owned_units_under` / `owned_units_with_values_under` /
   `owned_has_empty_term_value`) the reestablish folds use, UNIONING the membership stream
   (every final, incl. term-only) with the value stream (valued finals only) — the same
   flag-2 fix `reestablish_overlay_value` has. It feeds that enumeration to the iterative,
   deep-term-safe builder `overlay/f5_build.rs::build_overlay_root_from_terms`, which:
   - **Phase 1**: inserts each `(units, Option<V>)` into a mutable `OverlayBuilderNode`
     tree via an explicit per-unit LOOP (no recursion), one node per unit.
   - **Phase 2**: converts the builder tree → `Arc<OverlayNode<K,V>>` bottom-up on an
     explicit work-stack (the iterative post-order of `inner_to_overlay` per node:
     `as_final`/`with_value`/`Child::InMem`).
   Both phases — and the builder-node `Drop` + the `OverlayNode` `Drop` — are **iterative
   (NO recursion with key length)**, proven by the white-box test
   `f5_loader.rs::deep_term_converter_tests` (builds + drops a 100k-unit single-key
   overlay on the default test stack). Generic over `V` (single-node value is `Option<V>`;
   no counter/i64/u64 specialization).

   **WHY term-enumeration, not a node-structural walk (the compaction discovery).** The
   original plan + my first implementation assumed an Overlay-regime dense image is ALWAYS
   un-path-compressed (the overlay serializer writes one node per unit). **That is FALSE
   for a COMPACTED file** (C-opt-1): `compact()` rebuilds the dense image via the
   owned-staging path, which uses `StringBucket` suffix compression + compressed ART-node
   prefixes, then re-stamps the Overlay regime. A node-structural converter on the raw
   owned nodes fail-closed-rejected a compacted `StringBucket` root — 4 compaction
   correspondence tests caught this immediately when the gate was flipped ON. The
   term-enumeration converter is robust: the proven owned readers ALREADY expand all
   compression (buckets + prefixes), so the converter handles BOTH un-compressed and
   compacted Overlay images with the compression logic living ONCE in the existing readers
   — no new data-loss-critical expansion code.

3. **Install the pre-built root.** `flip_to_overlay`/`enable_lockfree` create an EMPTY
   overlay root. F5 installs the PRE-BUILT root via the generic
   `install_prebuilt_overlay_root(root)` (a tiny per-variant seam
   `install_prebuilt_overlay_root_seam` sets `self.lockfree_root =
   Some(AtomicNodePtr::new(root))` + a fresh cache), then selects `LockFreeOverlay` and
   verifies the Overlay regime exactly as `flip_to_overlay` does (V-2 re-check;
   HARD-ERROR on a `false` — an Owned-regime WAL under overlay routing would resurrect
   unranked orphans on a later reopen). It does NOT touch the owned tree.

## (b) WAL-replay-into-overlay (THE data-loss-critical path) — be precise

Today reopen replays the WAL tail through OWNED mutators (`replay_records_lww` →
`apply_core_recovered_operation_no_wal` / `apply_recovered_operation_no_wal`). F5 must
replay it INTO THE OVERLAY. We add the overlay twin **`replay_records_lww_overlay`**:

1. **Winners.** Reuse the EXISTING `reconcile_lww` (char) / `reconcile_lww(raw_records…)`
   (byte) — the SAME call the owned replay makes — to compute the per-term last-writer
   winners as representation-agnostic `Vec<RecoveredOperation>` (`(term: Vec<u8>, op)`).
   The **Overlay-regime unranked-orphan DROP is INHERITED** from `reconcile_lww`
   (`rank_regime = Overlay` ⇒ unranked two-append-window orphans are dropped); we do NOT
   re-derive it. The checkpoint-subsumed skip (`lsn <= checkpoint_lsn`) is likewise
   inherited.
   - Byte note: byte's owned `replay_records_lww` takes BOTH `tx_filtered_ops` and
     `raw_records` (its `Owned` arm uses the tx-filtered stream for transaction
     semantics). F5 runs ONLY for the `Overlay` arm, whose winners come from
     `reconcile_lww(raw_records, …)` — the durable overlay-write path is never
     transactional, so the raw records carry the SAME data ops. So
     `replay_records_lww_overlay` takes the raw records and calls `reconcile_lww` with
     `RankRegime::Overlay` directly.

2. **Apply each winner INTO THE OVERLAY via the no-WAL publishers** — the SAME publishers
   `reestablish_overlay_value`/`_membership`/`_counter` use. We add
   `apply_recovered_operation_overlay(op)` (the overlay twin of the owned
   `apply_*_recovered_operation_no_wal`), routed via the shared `LockFreeOverlay` seam so
   it is generic over `V`:
   - `Insert{term, value: Some(bytes)}` → deserialize `V`, `overlay_publish_value(units, v)`.
   - `Insert{term, value: None}` (term-only membership) → `overlay_publish_membership(units)`.
   - `Remove{term}` → `overlay_remove(units)` (the generic no-WAL overlay remove): for
     non-empty terms the per-variant seam `overlay_try_remove_path` (a retry loop over the
     EXISTING single-arbiter `try_remove_lockfree_path` — no WAL, no rank, no watermark);
     for "" a fresh non-final root CAS (`publish_root_cas(as_non_final, is_final)`).
     **REQUIRED for correctness** — a term inserted into the dense image then removed in
     the un-checkpointed WAL tail MUST be cleared from the rebuilt overlay or it
     RESURRECTS (the exact data-loss class F5 must not introduce; the task's named
     publisher list omitted Remove, but the inserted-then-removed-in-tail case forced it).
     Proven by `char_wal_tail_remove_does_not_resurrect_under_f5`.
   - `Increment`/`Upsert`/`CompareAndSwap(success)` → resolve to a value SET (deserialize
     `V` / build from `i64`), then `overlay_publish_value`. `Increment{result:None}`
     (a delta from `BatchIncrement`) ACCUMULATES onto the overlay's current value via the
     counter-monomorph `increment_cas` (the same accumulation the owned applier does),
     dispatched through the counter seam; `Increment{result:Some(v)}` SETs to `v`.
   - **Empty term "" → the RANKED root publisher**, NEVER the unranked
     `overlay_publish_root_value` path-bypass: a valued "" publishes via
     `overlay_publish_root_value` (the reestablish-style fresh-root-CAS value publisher
     — §2.2/G5-NEW-4 data-loss fix), a term-only "" via `overlay_publish_root_membership`,
     a removed "" via the root non-final publisher. (At reopen there is NO concurrency and
     NO new WAL — these are the same publishers reestablish uses, which are correct for
     the empty term; the publishers internally use the fresh-root-CAS discipline.)

   **Ordering.** `reconcile_lww` returns winners in `(generation, lsn)` (commit-
   visibility) order, so applying them in order into the overlay reproduces the
   last-writer-wins final state — IDENTICAL to the owned applier consuming the same
   winner list. The overlay applier is single-threaded at reopen (no concurrent writers),
   so each publisher's CAS sees no contention.

This `apply_recovered_operation_overlay` is the ONLY new data-loss-critical code; it is
commented thoroughly in-line and proven by the S2 WAL-tail tests (incl. the unranked-drop
negative control).

## (c) The gate mechanism

F5 selection is a `const USE_F5_REOPEN_LOADER: bool` on the `LockFreeOverlay` trait
(no struct field ⇒ zero ctor churn). **Current state: `true` (S3 — switched ON).** Every
reopen ctor (char `open`/`open_with_depth`/`open_with_io_uring`, byte `open`/
`open_with_io_uring`) reads it and branches its Overlay-regime arm:

```text
let use_f5 = USE_F5_REOPEN_LOADER && rank_regime == Overlay && overlay_eligible_v();
if use_f5 {
    // F5: load_root_immutable (dense→overlay, owned NOT installed)
    //     + replay_records_lww_overlay (WAL tail INTO the overlay)
} else {
    // LEGACY (unchanged): owned dense-load + replay_records_lww (owned)
    //     + flip + reestablish_overlay_dispatch
}
```

An Owned-regime file (legacy/un-flipped) ALWAYS uses the owned loader (F5 runs only for
Overlay). Flipping the const back to `false` restores the proven legacy path with zero
other changes — **fully reversible**.

**Gate-independent test ctors.** Because `open` follows the gate, the both-loaders
correspondence proptest cannot use `open` for "legacy" once the gate is ON. Each variant
exposes two `pub` test ctors that bypass the gate: `open_with_legacy_loader` (forces the
owned-loader→reestablish path) and `open_with_f5_loader` (forces F5). The gate test
compares these directly, so it is a meaningful legacy-vs-F5 oracle whether the gate is ON
or OFF — and stays a stable regression oracle after F7.

## What F5 does NOT touch (owner constraints)

- The `checkpoint_lsn = committed watermark` capture ordering is UNTOUCHED (F5 is
  reopen-side only; checkpoint capture is not edited).
- The owned tree / mutators / `replay_records_lww` / reopen path are NOT deleted (that is
  F7). F5 adds the loader ALONGSIDE (the legacy arm stays, selected when the gate is OFF
  or the regime is Owned).
- NO new `unsafe` (the converter + builder use safe `Arc`/`OverlayNode`/`BTreeMap`
  builders; the only `Any` downcasts are the EXISTING seam ones). NO edits to sibling
  repos. The unsafe-inventory set-equality gate stays green.

## Residual / honest notes

- **F5 fixes a pre-existing counter data-loss bug.** A term-only MEMBER (`insert(t)` with
  no value) on a `u64`/`i64` COUNTER trie was DROPPED on reopen by the legacy
  `reestablish_overlay_counter` (it republished only valued terms). F5's converter unions
  the membership + value streams, so it KEEPS the member. The S3 switch therefore CHANGES
  counter-trie reopen behavior — strictly more correct (no data loss). Pinned by
  `counter_term_only_member_survives_reopen_under_f5`.
- **Deep-term envelope.** The F5 converter is iterative (100k-safe), but the FULL reopen+
  read at extreme depth is still bounded by PRE-EXISTING recursive paths
  (`find_leaf_recursive`, `overlay_count_finals`, the owned-tree `Drop`, the overlay
  insert `build_value_path_recursive`) — orthogonal to F5. The integration deep-term tests
  run on a 512 MiB-stack thread to get past those; the converter-in-isolation white-box
  test runs on the default stack. F5 is never the deep-term bottleneck.
- **F5-B materializes the dense owned tree transiently** (eager-load → convert → clear),
  so reopen-side owned materialization is not yet eliminated — F5-A (direct arena→overlay
  parser) would, but the plan scopes that to a later effort. F5's contribution is: the WAL
  TAIL is replayed into the overlay (not owned), and the owned tree is not left installed.
