# Slice 3 (Phase F) — F5 loader + F7 owned-tree deletion: execution plan

Owner GO'd 2026-06-07: do Slice 3 now; **DELETE** genuinely-dead owned code outright (git
history is recovery), not comment-out. This plan is the code-verified (post-F2/F4/F6)
execution. Supersedes the V7 framing in `phase-f-g5-delete-owned-tree.md` §6 where noted.

## The reframe (the crux)
"Delete the owned tree" = delete the owned **RUNTIME** (the `root` field, mutators, reopen
path, owned read/write/checkpoint arms, the reestablish folds, kill-switch, route-split).
The owned **NODE TYPES** (`Node`/`ChildNode`/`StringBucket`; char `CharTrieNodeInner`/
`CharNode*`) + serialize/deserialize **ARE the on-disk format** — the overlay serializer
encodes INTO them and F5 decodes FROM them — so they STAY. `transitions.rs` (~1157 LOC)
SHRINKS (its in-memory owned-mutation methods die; its format-facing types/methods stay),
it does NOT vanish (the V7 doc's "bulk-delete transitions.rs" is wrong).

## F5 — `load_root_immutable` (direct dense→overlay reopen loader)
**F5-B (recommended):** reopen via the EXISTING owned loader (`load_root_from_disk`, eager)
→ then an explicit-work-stack walk-converter owned→overlay (iterative `inner_to_overlay`
over the whole tree, deep-term-safe). Minimal new format surface (the owned loader is reused
verbatim incl. buckets); the converter is an in-memory copy, oracle-checkable in-process.
(F5-A = direct arena→overlay parser; only needed if the owned loader is also deleted, which
C-opt-1 does NOT do — so ship F5-B.)
**The one new data-loss-critical path — WAL-replay-into-overlay:** today reopen replays the
WAL tail through OWNED mutators (`replay_records_lww`); F5 must replay it INTO THE OVERLAY via
the no-WAL overlay publishers, reusing `reconcile_lww` (winners are representation-agnostic
`(term,op)`; the Overlay-regime unranked-orphan drop is inherited). Gate + proptest hard.
**Gate (before any switch):** both-loaders correspondence proptest — byte+char × V∈{(),
u64/i64,String,struct} × {valued,term-only,empty-""} → reopen via owned-loader vs F5 → assert
identical len/term-set/get_value(incl."")/membership; + a deep (~100k-unit) key; + a WAL-tail
(crash-without-checkpoint) reopen + the unranked-drop negative control.

## F7 delete-vs-keep map
DELETE (dead once F5 reopen + C-opt-1 compaction land): owned `root` field (+ the OR lock
rung), owned mutators + `replay_records_lww` (`mutation_core.rs`), owned read fallbacks
(`owned_try_contains`/`owned_get`/`owned_try_get` + `!route_overlay()` arms), owned checkpoint
arm (+ the C2 `debug_assert!(!route_overlay())`), the reestablish folds + `clear_owned` +
`owned_*` readers + `reestablish_overlay_dispatch` + the D1 grep gate (`flip.rs:24-44`), the
owned reopen path, `kill_switch_to_owned` + `OverlayWriteMode` enum + `overlay_write_mode`
field + `route_overlay()` split (collapses — everything is overlay), owned-tree + kill-switch
tests. **Prune the removed `unsafe` rows from UNSAFE_INVENTORY.tsv + UNSAFE_CONTRACTS.tsv in
the same commit** (set-equality gate — F7 shrinks the ledger).
KEEP (load-bearing): owned NODE types + serialize/deserialize (the on-disk format), the F5
decode primitive (`load_overlay_node_from_disk`/`inner_to_overlay` + iterative loaders),
arena/buffer/swizzle infra, `overlay_to_inner` (test-only round-trip reference).

## Compaction — C-opt-1 (RESOLVED)
KEEP a minimal **transient, isolated** owned-staging build for `compact` (it already builds a
separate `new_trie` — `compaction_impl.rs:198`). The PRODUCTION trie's owned tree is still
deleted; only the staging instance retains a minimal owned representation
(`CharTrieRoot`/`TrieRoot` + owned loader + a minimal `insert_impl_no_wal` + node-building) as
the dense-staging backend. Zero new data-loss-critical code; compaction stays path-compressed
dense. With `OverlayWriteMode` gone, the staging trie is owned by a CONSTRUCTION-time
distinction (overlay-not-installed), not a runtime enum. (C-opt-2 = a path-compressing
overlay→dense serializer + an F5 that reads multi-unit-prefix overlay nodes → full owned
deletion, but a separate multi-week data-loss-critical format effort — OUT of Slice-3 scope.)

## Sub-steps (each independently green + committable; compiler-driven)
- S1 F5-land (loader + reconcile-into-overlay, gated OFF, dormant) — reversible.
- S2 F5-prove (both-loaders proptest + deep-term + WAL-tail tests) — reversible.
- S3 F5-switch (reopen Overlay-regime branch → F5; owned-regime files keep owned loader) — reversible.
- S4 delete owned reopen path + reestablish machinery + clear_owned + owned_* readers + D1 gate.
- S5 delete owned mutators + replay_records_lww (keep the minimal compaction-staging insert).
- S6 delete owned read fallbacks.
- S7 delete owned checkpoint arm (+ C2 assert).
- S8 delete owned `root` field + OR lock (reconcile compaction-staging as a distinct builder).
- S9 route-split collapse (`route_overlay()`→const true, delete owned arms, `overlay_eligible_v`).
- S10 (FINAL) delete kill_switch + OverlayWriteMode + the field; compaction-staging → construction-time owned.

## Verification (per commit + end-to-end)
Per commit: full suite feature-on AND feature-off (0 fail) + unsafe-inventory + formal gate
exit 0. F5 gate: the both-loaders correspondence + deep-term + WAL-tail proptests (before S3).
After deletions: the existing correspondence suites are the overlay-unchanged oracle. Compaction:
`compaction_tests` + correspondence + the empty-"" test + a density (size ≤ pre-compaction)
assertion. End: a high-concurrency real-disk soak (owned tree GONE) — every committed key
survives reopen (#41 witness). Cross-repo build-check (READ-ONLY, no edits): liblevenshtein-rust
+ libgrammstein + lling-llang + pgmcp all stay green (they use only the public API + the F4
`.read()/.write()` shim — none reference owned types). The `checkpoint_lsn=committed-watermark`
capture ordering is UNTOUCHED by F5/F7 (state it per commit — the #41 guard).

## Residual (honest)
The compaction-staging owned types + the on-disk-format node types are permanently KEPT (the
format + the dense-staging backend). "No literally-zero owned code" is C-opt-2 (separate
effort). transitions.rs shrinks, not vanishes. F5's reconcile-into-overlay is the one new
data-loss-critical path (gated + proptested, reuses reconcile_lww).
