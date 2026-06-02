# Design: The Irreversible "Lock-Free Flip" (Phase E2/E1/Checkpoint/Eviction/Recovery + Phase F)

**Repo:** `/home/dylon/Workspace/f1r3fly.io/libdictenstein` · 2026-06-02 · implementation-ready design for the
owner-gated, data-loss-critical culmination. Persisted from the Plan-agent design (full prose in the session
transcript; this file preserves every decision, gap, phase, and risk).

## (1) Scope + the genuine GAPS (decision-critical)
G4 has LANDED: `PersistentCharNode<V> = OverlayNode<CharKey,V>`, blanket `TrieRoot`, all durable primitives green
at 2489. Two trees coexist in `PersistentARTrieChar<V,S>`: OWNED `self.root` (`mod.rs:362`, raw-ptr, `&mut`, EBR,
under `Arc<RwLock>`) — the production default today; OVERLAY `lockfree_root: Option<AtomicNodePtr<V>>`
(`mod.rs:465`, immutable path-copy+root-CAS) — reached only after `enable_lockfree()`. Constructors set
`lockfree_root:None` + WAL-replay rebuilds the OWNED tree; **there is no overlay-as-default loader** (Recovery §6
is the one new production component). All production mutations funnel through `append_to_wal_inner`
(`wal_helpers.rs:86` → `invalidate_eviction_registry`) — the overlay durable path shares this chokepoint, so the
registry-invalidation contract + Order-A timing are preserved by construction.

**Four genuine gaps (all scoped out with named follow-ons; none block the core flip):**
- **`remove` — NO overlay delete (DECISION-CRITICAL).** `OverlayNode` is monotone-final: `try_set_final` is
  `fetch_or(IS_FINAL)` (`overlay/node.rs:724`), there is NO `as_non_final`/`without_value`. Owned `remove` does
  `set_final(false); value=None` in place (`mutation_core.rs:196`) — un-doable in the overlay.
  - **R-A (RECOMMENDED first flip):** `remove` is UNSUPPORTED under `LockFreeOverlay` (errors/falls back to
    `OwnedTree`); documented; PS3-guarded. Recovery stays correct (REC-A rebuilds from the live set, §6.3). Keeps
    the flip's risk on the proven insert/increment/checkpoint/evict/fault-in/recover surface.
  - **R-B (if delete-on-overlay needed this release — gates F5):** add `as_non_final` + `remove_cas_durable`
    (Order-A: WAL `Remove` → path-copy clearing the leaf's `IS_FINAL`+value → root-CAS → mark_committed). **Breaks
    the monotone-final invariant → the loom "prefix single-arbiter"+"no-lost-update" models MUST be re-proven with
    a delete action** (highest-risk obligation, analogous to the G1 single-phase re-proof). `without_child`
    compaction out of scope (clearing finality suffices, matching owned `remove`).
  - **R-C (tombstone in `Option<V>`):** rejected (ripples into serialization + the u64 domain).
- **Negative-delta `increment` unsupported:** overlay counter is add-only `BatchIncrement` (`lockfree_cas.rs:1088`)
  → §2.4 errors/falls back on δ<0; PS3-guarded. Decrement callers use `OwnedTree`.
- **Arbitrary `V` → scope is `V∈{(),u64}`:** the value write path `build_value_path_recursive` is hardcoded
  `<u64>`; arbitrary `V` needs G1 (single-phase + `upsert_cas`) — out of scope, stays `OwnedTree` (a per-monomorph
  guard). On-disk format is already `[len][bincode(V)]` so checkpoint/recover are V-agnostic.
- **Prefix-iteration/zipper:** walk the owned `self.root`; no overlay prefix-enumeration-with-fault-in yet. Point
  reads (`contains`/`get`/`get_value`) flip (proven via `find_leaf_faulting`); **iteration/zipper are an
  E1-iter-B follow-on** — until then documented last-checkpoint-consistent.

**Feasibility:** the flip is feasible for insert/increment/checkpoint/evict/recover/**point-reads** on
`V∈{(),u64}` — every primitive built, proven, benchmarked PROCEED. The gaps above are the honest residuals.

## (2) E2 — write-path flip (kill-switch router; no logic duplicated)
Each production mutator gains ONE top-level `match self.route_overlay()` branch. `route_overlay()` =
`self.overlay_write_mode.uses_overlay() && self.lockfree_root.is_some()`. The overlay primitives already enforce
Order-A (WAL-durable BEFORE visibility CAS, `lockfree_cas.rs:243`) + `invalidate_eviction_registry` (via
`append_to_wal_inner`) + `mark_committed`-after-CAS — **the flip wires them, doesn't re-author them**, so
`NoLostWriteUnderLockFreeCommit` + the registry contract hold by construction.
- `insert`/`insert_with_value(V=())` → `insert_cas_durable`; `insert_with_value(V=u64)` → new thin
  `insert_cas_with_value_durable` (reuses `build_value_path_recursive`).
- `increment(V=u64, δ≥0)` → `try_increment_cas_durable` (u64 adapter; δ<0 errors/falls back).
- `upsert(V=u64)` → new thin `upsert_cas_durable` (last-writer = CAS winner).
- `insert_batch*` → one `BatchInsert` WAL append + N overlay CAS (existing single-append discipline).
- document-tx → same WAL records, overlay apply.
- `remove` → R-A error/fallback (or R-B).
- arbitrary V → forced `OwnedTree`.
**The irreversible default-flip** = one edit per constructor: `overlay_write_mode: LockFreeOverlay` +
`enable_lockfree()` for `V∈{(),u64}`. `set_overlay_write_mode` (new setter) = the runtime fallback (restart-time,
§8.1). `SharedCharARTrie` wrappers keep `self.write()` for E2 until Phase F (lock discipline unchanged until F).

## (3) E1 — read-path flip (after E2)
`contains`/`get`/`get_value` route to `contains_lockfree`/`get_lockfree` + `find_leaf_faulting`. **First task:
un-gate fault-in to production** (today `find_leaf_faulting` + the OnDisk arms are `cfg(any(test,bench-internals))`
→ make unconditional; production needs fault-in because evicted overlay nodes must be re-readable — the g5 design
anticipated "the flip CONSUMES this primitive"). Loser-safe, no-UAF, read-only-wrt-durable (g5-proven).
Iteration/zipper = E1-iter-B follow-on.

## (4) Checkpoint flip (+ remove C2 assert)
`checkpoint()` under `LockFreeOverlay` → `capture_snapshot_immutable` + `publish_immutable_snapshot_retaining_wal_with_eviction`
— NO write guard, NO downgrade, NO writer exclusion (writers proceed). The **C2 `debug_assert!(lockfree_root.is_none())`
(`mod.rs:1292`) moves into the `OwnedTree` arm only**; its safety role is taken by the snapshot-LSN
`debug_assert!(watermark ≤ synced_frontier)` (`persist.rs:464`) — the lock-free analogue (plan §4).
**CKPT-A (RECOMMENDED first flip): retain-WAL, the exact proven publisher** — zero new data-loss surface.
**CKPT-B (destructive `rotate_to_archive(watermark)`): the one genuinely new dangerous line — DEFERRED to F7**,
its own gate (the `ReclaimWal` TLA action already proves it under `USE_WATERMARK=TRUE`; impl gate = soak). Both
preserve `NoLostWriteUnderLockFreeCommit`; `CaptureEqualsPublishFrontier` re-derived under the relaxed gate by
`LockFreeDurableCheckpoint.tla`.

## (5) Eviction flip
Production `force_eviction` callback → `evict_overlay_nodes` (the §F-proven driver; un-gate
`bench_evict_overlay_cold_nodes` from `bench-internals`). **Drop the cold-only restriction** (E1 makes fault-in
production → eviction may touch ANY node; a later read/write faults it back) — exactly g5's "DROP
EvictTouchesOnlyCold, ADD ReadNeverMissesCommitted". Registry-invalidation + loser-safe CAS + no-UAF preserved;
production eviction now reclaims overlay memory (§F-proven).

## (6) Recovery — overlay-root rebuild + the BACK-COMPAT proof
**On-disk format UNCHANGED by the flip (proven by construction):** the overlay checkpoint serializes via
`overlay_to_inner` → the SAME `serialize_char_node_to_disk` (`persist.rs:424`); value blob `[len][bincode(V)]`
identical; root descriptor/WAL formats unchanged. **⇒ a file written pre-flip reopens post-flip and vice-versa —
the DATA is never irreversibly transformed.** This bounds the irreversibility to code/architecture (you can always
downgrade the binary + reopen the same files).
- **REC-A (RECOMMENDED first flip):** after owned recovery, rebuild `lockfree_root` from `iter()`/`iter_with_values()`
  (the safe enumerator — NO new unsafe; the literal Phase-C pattern, whose tests already prove rebuilt-overlay ==
  owned). Cost: O(terms) overlay inserts on open (doubles open-time tree-build for large tries — risk §11.3).
- **REC-B (follow-on):** lazy structural load via `inner_to_overlay` (single-level OnDisk children) + fault-in +
  WAL-tail replay. O(1)-in-image open; avoids the double build.
- `Remove` in recovery: REC-A rebuilds from the live set (`iter()` enumerates only final terms) ⇒ deleted terms
  naturally absent — **recovery correct even under R-A** (§6.3). Watermark base = recovered frontier (already wired).

## (7) Phase F — owned-tree removal + RwLock→Arc (the IRREVERSIBLE commit)
Comment-out (don't delete) `self.root` + owned mutators/checkpoint/eviction + the owned EBR retire_list +
`overlay_write_mode` + all `OwnedTree` branches. `SharedCharARTrie = Arc<RwLock<…>>` → `Arc<…>`. Add
`checkpoint_mutex: Mutex<()>` (serializes checkpoints with each other, NOT excluding writers — replaces the
RwLock's checkpoint role). `&mut self` → `&self` on every overlay-only mutator + the trait impls drop their
write/read guards.
**KEY F FINDING (Send/Sync IMPROVES):** the owned `self.root` (raw `*mut`) was `Sync`-safe only behind the RwLock;
**commenting it out removes the one field that would need a manual `unsafe impl Sync`**, so the overlay-only struct
auto-derives `Send+Sync` with ZERO new unsafe — F *improves* the safety story (owned-tree `unsafe impl` rows are
DELETED, not added; unsafe-inventory shrinks). Gate: `cargo build` + `send_sync_violations` clean + inventory exit-0.
API break (the irreversibility): callers using `SharedCharARTrie` as `Arc<RwLock>` (`.write()`/`.read()`) break.

## (8) Kill-switch + the precise REVERSIBILITY ENVELOPE
`OverlayWriteMode` fallback is a **restart-time** switch (set mode + reopen; WAL is the shared source of truth, both
trees recoverable from it) — NOT a hot toggle (under `LockFreeOverlay` the owned tree isn't written, so a hot flip
would read stale).

| Change | Reversible? | How |
|---|---|---|
| **Data on disk** | **ALWAYS** | format unchanged (§6.1); reopens under pre/post-flip binary |
| E2/E1/checkpoint(CKPT-A)/eviction | Yes (kill-switch, 1 release) | `set_overlay_write_mode(OwnedTree)` + restart |
| F5 default-flip | Yes (kill-switch) — last reversible step | flip the constructor default back |
| **F6 (delete owned tree, RwLock→Arc)** | **IRREVERSIBLE (code/API)** | only via `git revert` |
| F7 (CKPT-B WAL truncation) | one-way per checkpoint (data) | deliberately separated from F6 |

**Owner one-liner:** everything except F6 is reversible behind the kill-switch; the on-disk DATA is reversible even
across F6; F6's irreversibility is purely the source-level owned-tree removal + the type change. CKPT-B (truncation)
is the only data-affecting one-way step, deliberately split out (F7).

## (9) Formal + verification
- **Integration spec `LockFreeFlipEndToEnd.tla`** (belt-and-suspenders for an irreversible flip; reuse base-module
  actions): headline invariant `EveryCommittedTermSurvivesFullCycle` over Write∪Commit∪Checkpoint∪Evict∪FaultIn∪
  Crash∪Recover; tiny CONSTANTS (2 writers/2 terms/MaxLSN=3); `_Unsafe.cfg` re-breaks one link (capture-ordering
  reversed) → MUST violate it. The per-component specs already cover each link.
- **Production-soak PS1 (the #41 witness through the FLIPPED production API):** N writers `trie.insert`/`increment`
  + R readers `trie.contains`/`get_value` + checkpointer `trie.checkpoint()` + evictor `force_eviction`, all
  production API now routed to the overlay; reopen (overlay recovery) → EVERY acknowledged term survives exact.
  Real-disk. **PS2:** kill-switch round-trip (owned↔overlay both reopen-compatible). **PS3:** assert remove/δ<0
  return the documented error (guards the gaps).
- **Existing 2489 suite:** mode-agnostic behavioral tests RUN THROUGH the overlay (the identity oracle, as G4 used
  them); owned-internal tests re-point or run in `OwnedTree`-mode fixtures (mode-parameterized). Green at every phase.

## (10) Phased migration (each green-gated: nextest ≥2489 + verify-formal-correspondence exit 0 + unsafe-inventory exit 0)
- **F0** — routing scaffold + thin primitives (`insert_cas_with_value_durable`, `upsert_cas_durable`, overlay batch
  loop) + **un-gate fault-in to production**. No default flip. Reversible.
- **F1** — integration TLA + PS1/PS2/PS3 harness (mode-parameterized; run `OwnedTree` first = harness sound). Reversible.
- **F2** — Recovery REC-A overlay rebuild on open (still owned-default; overlay rebuilt-but-unused). Reversible.
- **F3** — checkpoint flip CKPT-A (+ move C2 into the OwnedTree arm). Reversible.
- **F4** — eviction flip (un-gate driver; drop cold-only). Reversible.
- **F5** — **set `LockFreeOverlay` default (E2+E1)**; data-loss-critical default-flip, **still reversible via
  kill-switch**. **PS1 is the headline gate** (full soak → reopen → every committed term survives).
- **F6** — **⛔ THE SINGLE IRREVERSIBLE COMMIT: Phase F** (owned tree out, RwLock→Arc, checkpoint_mutex,
  &mut→&self). Mark the commit "IRREVERSIBLE". Rollback only via `git revert`.
- **F7** (separately gated) — CKPT-B destructive WAL truncation.
- Follow-ons: R-B (overlay delete + loom re-proof), G1 (arbitrary-V), E1-iter-B (overlay iteration), REC-B (lazy
  recovery).
**F0–F5 reversible; F6 the one-way door.**

## (11) Honest risks (data-loss-critical flagged)
1. **`remove` no overlay primitive (DATA-CORRECTNESS):** R-A (unsupported, documented, PS3-guarded) first; R-B
   (proven + loom re-proof) gated follow-on. If delete-on-overlay needed this release, R-B's re-proof gates F5.
2. **Negative-delta increment unsupported (DATA-CORRECTNESS):** §2.4 error/fallback; PS3-guarded.
3. **Recovery rebuild cost (PERF):** REC-A doubles open-time tree-build for large tries; measure before F6; REC-B
   follow-on.
4. **Production soak surfacing an integration lost-write (DATA-LOSS-CRITICAL — PS1's whole point):** capture-ordering
   (watermark before root.load, `persist.rs:403<:420`) + snapshot-LSN assert are load-bearing; PS1 + integration TLA
   + negative controls. If PS1 drops a term, F5 does not land.
5. **Send/Sync at F6:** removing `self.root` IMPROVES it (zero new unsafe; auto-derive). Gate: build + send_sync +
   inventory.
6. **Kill-switch dead-code (F5→F6):** mode-parameterized fixtures keep both arms exercised; PS2 guards reopen-compat.
7. **CKPT-B truncation (DATA-LOSS-CRITICAL, deferred F7):** kept OUT of F6; landed separately, gated by ReclaimWal
   TLA + reopen-after-truncate soak.
8. **Arbitrary-V mis-route:** default-flip only in `()`/`u64` monomorphs; PS3 guard per V.
9. **Iteration consistency:** last-checkpoint (E1-iter-A) until E1-iter-B; documented.
10. **Trusted boundaries (unchanged):** kernel/fs ordering below sync(); arc-swap internals (Miri-gated).

### Critical files
- `mod.rs` (`SharedCharARTrie:343`, trait impls + `checkpoint:1278`/C2 `:1292`, eviction `:1689`/`:1775`; routing
  matches; F6 RwLock→Arc + &mut→&self + checkpoint_mutex)
- `lockfree_cas.rs` (Order-A primitives `:207`/`:1044`/`:515`/`build_value_path_recursive:1117`; add
  insert_cas_with_value_durable/upsert_cas_durable/(R-B remove_cas_durable); un-gate fault-in)
- `mutation_api.rs`/`atomic_ops.rs`/`batch_insert.rs`/`document_tx.rs` (production mutators gain the router branch)
- `persist.rs` (`capture_snapshot_immutable:343` + snapshot-LSN assert `:464`,
  `publish_immutable_snapshot_retaining_wal_with_eviction:655`, `overlay_to_inner:1143`/`inner_to_overlay:1216`,
  Phase-C recovery tests `:1571`)
- `mmap_ctor.rs`/`io_uring_ctor.rs` (constructors: flip default + enable_lockfree; WAL-replay + overlay rebuild
  REC-A), `overlay_write_mode.rs` (wire route_overlay + setter), `committed_watermark.rs` (base on open)
- `formal-verification/tla+/LockFreeFlipEndToEnd.tla` (new) + `scripts/verify-formal-correspondence.sh`;
  `tests/persistent_lockfree_flip_production_soak.rs` (PS1/PS2/PS3)
