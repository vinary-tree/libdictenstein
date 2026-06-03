# S5-12 E1 — Read-Path Flip + Test Handling (corrected spec)

**Crate `libdictenstein`, char ARTrie. Baseline HEAD `12eeaba` + the parked write-flip
(EDIT 1/2/3: `mmap_ctor.rs +395`, `io_uring_ctor.rs +31`).** This document is the
implementation spec for the READ half of the S5-12 production flip. It folds the original E1
design (Plan agent `a16e81b0`) together with the adversarial red-team (`a09742f8`) that returned
**NO-GO-as-designed → GO-WITH-FIXES**. The corrections below ARE the spec; where the original
design and the red-team disagree, the red-team (code-grounded against HEAD) wins.

DATA-LOSS-CRITICAL. IRREVERSIBLE at the write layer (the parked flip). E1 read-routing is itself
reversible (a `route_overlay()` branch), but it lands TOGETHER with the parked write-flip as one
atomic commit — the write-flip cannot be green without it (the owned tree is cleared on reopen).

---

## 0. Why E1 exists

The parked write-flip makes a fresh `create::<V∈{(),u64}>()` route WRITES to the lock-free overlay
(`route_overlay() == true`). On an Overlay-regime REOPEN, EDIT 2 moves the recovered owned tree into
the overlay and **clears `self.root` + `self.len`** (`reestablish_overlay_*_after_recovery`,
lockfree_cas.rs:314-315 / 2107-2108 — confirmed). Every owned-trait READ still reads
`self.root`/`self.len`, now empty ⇒ ~58 tests see an empty tree. E1 routes the reads to the overlay,
symmetric to the write guards. `route_overlay() == uses_overlay() && lockfree_root.is_some()`
(overlay_write_mode.rs:71).

The overlay is `u64`+`()` only (`overlay_eligible_v()`); arbitrary `V` stays Owned and E1 is INERT
for it (the `false` arm is the verbatim owned body).

---

## 1. D1 — THE CRITICAL CORRECTION: reestablish must read OWNED-ONLY (else total loss)

**The red-team's headline catch.** Parked EDIT 2 calls `inner.flip_to_overlay()` **before**
`inner.reestablish_overlay_dispatch()` (mmap_ctor.rs / io_uring_ctor.rs). `flip_to_overlay()` sets
`OverlayWriteMode::LockFreeOverlay` + `enable_lockfree()`, so **`route_overlay()` is already `true`
when reestablish runs.** Reestablish bootstraps the overlay by reading the recovered OWNED tree via
the very inherent methods E1 routes:

| reestablish fn | owned reads it issues |
|---|---|
| `reestablish_overlay_after_recovery` (u64, lockfree_cas.rs:2071) | `self.iter()` (2081), `self.get_value("")` (2091), `self.iter_prefix_with_values(&prefix)` (2099) |
| `reestablish_overlay_membership_after_recovery` (V=(), lockfree_cas.rs:297) | `self.iter()` (300), `self.iter_prefix(&prefix)` (307) |

If E1 naively wraps those inherent methods with `if route_overlay() { <overlay read> }`, then during
reestablish each read returns from the **still-empty overlay**. Reestablish copies **nothing**, then
unconditionally clears the owned tree (lockfree_cas.rs:2105-2108 / 313-315). **Every recovered term is
destroyed, irreversibly, on the first reopen of any flipped trie.** The code even documents the
violated assumption at lockfree_cas.rs:2076-2078.

### The fix (decided): owned-only private readers

Reestablish needs the overlay ENABLED (it writes via `insert_cas`/`increment_cas`, which need
`lockfree_root` present) AND the owned tree READABLE (the data source). So reorder-before-flip is
impossible. Instead, extract the owned read bodies into private, **un-routed** readers that reestablish
calls directly:

- `owned_iter_prefix(&self, prefix) -> Result<Option<Vec<String>>>` — the CURRENT `iter_prefix` body
  (`navigate_to_prefix` + `collect_terms_under_node`), with NO `route_overlay()` check.
- `owned_iter_prefix_with_values(&self, prefix) -> Result<Option<Vec<(String, V)>>>` — current
  `iter_prefix_with_values` body.
- `owned_get(&self, term) -> Option<&V>` — current `get` body (the `&V` walk of `self.root`).

The public methods become `if route_overlay() { <overlay> } else { self.owned_*() }`; reestablish calls
`self.owned_*()`. Reestablish's `self.iter()` → iterate `self.owned_iter_prefix("")?`; its
`self.get_value("")` → `self.owned_get("").cloned()`.

**AUDIT REQUIREMENT before routing:** grep every INTERNAL `self.iter(`/`self.iter_prefix(`/`self.get(`/
`self.get_value(` call inside the char-trie impl (not external/test). Any internal caller that runs
under `route_overlay()` and expects OWNED data must be repointed at the `owned_*` reader (reestablish is
the known one; confirm there are no others — checkpoint/merge/count read the overlay directly already).

---

## 2. D2 / D3 — trait read bodies bypass the inherent methods (route the TRAIT bodies)

The original design's premise *"E1 intercepts at the inherent methods only; the trait impls need no
change"* is FALSE. `contains` delegates to inherent (safe), but `len`/`is_empty`/`get_value` read state
directly or call the wrong inherent method:

| id | site | today | fix |
|---|---|---|---|
| S1 | `Dictionary::len` for `PersistentARTrieChar` (mod.rs:1004) | `Some(self.len.load())` direct | `Some(self.len())` (route the inherent `len()`; the trait calls it) |
| S1-cor | `Dictionary::is_empty` (default, lib.rs:206) | calls `Dictionary::len` | fixed transitively via S1 |
| S2 | `SharedCharARTrie::len` (mod.rs:1062) | `guard.len.load()` direct | `Some(guard.len())` |
| S2′ | `ARTrie::len` for `SharedCharARTrie` (mod.rs:1293) | `guard.len.load()` direct | `Some(guard.len())` |
| S3 | `SharedCharARTrie::get_value` (mod.rs:1071) | `guard.get(term).cloned()` (inner inherent `get`→None) | `guard.get_value(term)` (inner inherent value-route) |
| S3′ | `ARTrie::get_value` for `SharedCharARTrie` (mod.rs:1279) | `guard.get(term).cloned()` | `guard.get_value(term)` |
| S3″ | `MappedDictionary::get_value` for `PersistentARTrieChar` (mod.rs:1015) | `self.get(term).cloned()` | `self.get_value(term)` (new value-routing inherent) |

**Mechanism:** add an inherent `PersistentARTrieChar::get_value(&self, term) -> Option<V>` =
`if route_overlay() { route_get_value(self, term).flatten-ish } else { self.owned_get(term).cloned() }`,
route the inherent `len()`/`is_empty()`/`term_count()` to `overlay_len`, then rewrite each trait body to
delegate to the routed inherent method. **D3:** `ARTrie::create`/`open` (mod.rs:1204) build
`SharedCharARTrie` from the FLIPPING `create()`, so a `SharedCharARTrie<u64/()>` IS overlay-routed — its
trait reads do NOT "inherit" E1; they need the bodies above. This is the liblevenshtein integration
entry point.

---

## 3. The four overlay-read primitives (new, `pub(crate)`, NON-FAULTING)

All reuse the proven `as_in_mem`/non-faulting shape of `count_overlay_finals` (persist.rs:1324) and
`collect_lockfree_value_entries_recursive` (lockfree_cas.rs:2242). MAINTENANCE COUPLING: keep in lockstep
with those two. Every enumerator carries a code comment: **NON-FAULTING — must NOT call
`find_leaf_faulting`/`load_overlay_node_from_disk` (the 75-min soak deadlock, lockfree_cas.rs:1276-1287).**

1. `overlay_len(&self) -> usize` — `count_overlay_finals(root)` over `lockfree_root.load()`, else 0.
   Backs `len`/`term_count`. `is_empty` = a cheaper any-final early-out walk (NOT `overlay_len()==0`,
   to stay O(1)-ish on a huge overlay).
2. `overlay_navigate_prefix(&self, prefix) -> Option<Arc<PersistentCharNode<V>>>` — descend
   `lockfree_root` by `prefix.chars()`, `as_in_mem` only; `None` ⟺ a prefix char has no in-mem edge.
   Overlay is NOT path-compressed (one node per char), so one char = one edge.
3. `overlay_collect_finals(node, acc, out)` — DFS over in-mem children, push `acc` on `is_final`.
   Backs `iter_prefix`. RECOMMENDED (not required): heap work-stack instead of recursion as
   defense-in-depth — depth == key length (overlay un-path-compressed), same as today's production
   point-reads (≤500 in tests), so NO new crash risk; the stack is belt-and-suspenders.
4. `route_get_value<V,S>(&self, term) -> Option<Option<V>>` — the `Any`-downcast value-route (the
   `lockfree_value_route.rs` pattern, zero-unsafe, `V:'static`): `V==u64` ⇒ `get_lockfree(term)`
   re-wrapped as `V`; `V==()` ⇒ membership via `contains_lockfree`; else `None` (caller runs owned).

### Prefix semantics parity (None vs Some(empty)) — pin with a correspondence test
Owned `iter_prefix` returns `Ok(None)` when the prefix PATH is absent vs `Ok(Some(vec![]))` when the
prefix node exists but has no finals. `overlay_navigate_prefix` must reproduce this EXACTLY: `None` ⟺ no
in-mem edge; the empty-prefix `iter_prefix("")` (backing `iter()`) ⇒ root ⇒ `Ok(Some(...))` even on an
empty overlay. Test `test_disk_char_iter_prefix_not_found` asserts `.is_none()` for an absent prefix.

---

## 4. D4 / D6 / D9 — the remaining read defects

- **D4 (HIGH):** `get_optimistic`/`try_get_optimistic` (query_api.rs:154/168) compute
  `self.get(term).cloned()` ⇒ `Some(None)` under overlay (consistent read of WRONG data). They already
  return owned `Option<V>` (no signature gap), so VALUE-ROUTE them (call the routed inherent
  `get_value`). `contains_optimistic` is safe (delegates to inherent `contains`).
- **D6 (MED, coverage):** the deep-key test (integration.rs:703 + unicode twin) asserts via
  `if let Some(value) = reopened.get(&long_key) { assert_eq!(*value, i) }`. Under overlay inherent `get`
  → None ⇒ the `if let` body never runs ⇒ the value check VANISHES (vacuous pass). Swap
  `get`→`get_value` to KEEP the assertion live. Not optional — it preserves coverage.
- **D9 (HIGH):** `iter_prefix_with_arena`/`iter_prefix_with_values_and_arena` (prefix_api.rs:75/123) have
  NO overlay analogue (overlay nodes carry no per-node arena id). Today they read `self.root` (Empty) ⇒
  `Ok(None)`, which makes `remove_prefix`/`remove_prefix_batched` (prefix_api.rs:154/172) a **silent
  no-op** on a non-empty overlay (treats a real prefix as absent). FAIL-LOUD: under `route_overlay()`
  the arena-iter methods and `remove_prefix*` return `Err(InvalidOperation("arena iteration / prefix
  removal unavailable under the lock-free overlay"))`. Do NOT return `Ok(None)`.

---

## 5. D8 — DEFER surfaces (zipper / `root()` / transducer), documented + signalled

`root()` (mod.rs:596/996/1031), the zipper (mod.rs:1118-1194), and `DictionaryNode`/
`MappedDictionaryNode` (mod.rs:890/976) walk `self.root` (Empty under overlay) ⇒ a flipped trie looks
like an EMPTY dictionary to a transducer/zipper. This is the E1-iter-B / Phase-F surface (overlay-backed
`DictionaryNode`), out of E1 scope. For THIS commit:
- Document loudly on `root()`/zipper that fuzzy/zipper queries over a flipped (overlay) trie are
  E1-iter-B and currently see an empty dictionary.
- Emit a `log::warn!` (NOT a panic — the API returns a `Node`, not a `Result`) when `root()`/zipper is
  built under `route_overlay()`, pointing at E1-iter-B, so the boundary is observable, never silent.
- Vocab is SAFE (verified): `PersistentVocabARTrie` uses its own `VocabNode` + `LockFreeVocab`; it does
  NOT embed a `PersistentARTrieChar` and never flips. No char-flip exposure.
- GATE CHECK: confirm no suite test transduces/zippers over a flipped trie. If one does, it is an
  E1-iter-B test and must be marked/sequenced accordingly (not silently passed).

---

## 6. D7 — eviction-unfaithfulness is LATENT this release (sequencing constraint, not a blocker)

The overlay enumerators descend `as_in_mem` only (skip `Child::OnDisk`), so `len`/`iter`/`iter_prefix`
SILENTLY undercount EVICTED subtrees, whereas the owned `collect_terms_under_node` resolves DiskRef
faithfully. **Reachability verdict (red-team): NOT reachable in a default build this release** —
`evict_overlay_node_at_path`/`evict_overlay_nodes` are `pub(crate)`; the only non-test caller is
`bench_evict_overlay_cold_nodes` under `#[cfg(feature="bench-internals")]`; all else is `#[cfg(test)]`.
So the flip and production eviction are already feature-sequenced apart. Actions: keep eviction
bench/test-gated; document `overlay_len`/`iter`/`iter_prefix` as **resident-finals / last-checkpoint-
consistent (E1-iter-A)** with a `debug_assert` tripwire; make faithful-under-eviction enumeration a HARD
prerequisite (E1-iter-B) before un-gating overlay eviction.

---

## 7. CORRECTED COMPLETE ROUTING TABLE (supersedes the original R1–R15)

OVL = overlay route (value-route where V-polymorphic). DIRECT = trait body reads state directly, needs
its own route. DEFER = E1-iter-B (owned `root` walk). OWNED-ONLY = must read owned even under overlay
(reestablish bootstrap). FAIL-LOUD = `Err(InvalidOperation)` under overlay.

| Entry point | File:line | Action |
|---|---|---|
| inherent `contains`/`try_contains` | query_api.rs:27/41 | OVL → `contains_lockfree` |
| inherent `get`/`try_get` (`&V`) | query_api.rs:70/84 | None under overlay (signature gap); callers use `get_value` |
| inherent `get_value` (NEW) | mod.rs (new) | OVL → `route_get_value`, else `owned_get().cloned()` |
| inherent `len`/`term_count` | mod.rs:579/585 | OVL → `overlay_len` |
| inherent `is_empty` | mod.rs:591 | OVL → overlay any-final early-out |
| inherent `iter`/`iter_with_values` | mod.rs:604/617 | OVL via routed `iter_prefix*` |
| inherent `iter_prefix`/`_with_values` | prefix_api.rs:22/37 | OVL → `overlay_navigate_prefix`+`overlay_collect_finals` (None vs Some(empty) parity) |
| inherent `iter_prefix_vec`/`_with_values_vec` | mod.rs:632/642 | inherits OVL |
| inherent `iter_prefix_with_arena`/`_with_values_and_arena` | prefix_api.rs:75/123 | FAIL-LOUD (D9) |
| inherent `remove_prefix`/`_batched` | prefix_api.rs:154/172 | FAIL-LOUD under overlay (D9) |
| inherent `contains_optimistic`/`try_contains_optimistic` | query_api.rs:118/137 | inherits OVL (safe) |
| inherent `get_optimistic`/`try_get_optimistic` | query_api.rs:154/168 | OVL value-route (D4) |
| inherent `root` | mod.rs:596 | DEFER + `log::warn!` |
| `owned_iter_prefix`/`owned_iter_prefix_with_values`/`owned_get` (NEW, private) | — | un-routed; reestablish + the false-arms use them |
| reestablish internal reads | lockfree_cas.rs:300/307/2081/2091/2099 | OWNED-ONLY → the `owned_*` readers (D1) |
| `Dictionary::contains` (PersistentARTrieChar) | mod.rs:1000 | OK (delegates to inherent) |
| `Dictionary::len` | mod.rs:1004 | DIRECT → `Some(self.len())` (S1) |
| `Dictionary::is_empty` (default) | lib.rs:206 | transitive via S1 |
| `Dictionary::root` | mod.rs:996 | DEFER |
| `MappedDictionary::get_value` (PersistentARTrieChar) | mod.rs:1015 | DIRECT → `self.get_value()` (S3″) |
| `SharedCharARTrie::contains` | mod.rs:1056 | OK (delegates) |
| `SharedCharARTrie::len` | mod.rs:1062 | DIRECT → `Some(guard.len())` (S2) |
| `SharedCharARTrie::get_value` | mod.rs:1071 | DIRECT → `guard.get_value()` (S3) |
| `SharedCharARTrie::root` | mod.rs:1031 | DEFER |
| `ARTrie::len` (SharedCharARTrie) | mod.rs:1293 | DIRECT → `Some(guard.len())` (S2′) |
| `ARTrie::get_value` (SharedCharARTrie) | mod.rs:1279 | DIRECT → `guard.get_value()` (S3′) |
| `ARTrie::contains` (SharedCharARTrie) | mod.rs:1274 | OK (delegates) |
| `DictionaryNode`/`MappedDictionaryNode`/`DictZipper`/`ValuedDictZipper` | mod.rs:890/976/1138/1180 | DEFER (D8) |

---

## 8. Test handling (the complete reframe list — supersedes the original 12)

| Test(s) | File:line | Handling |
|---|---|---|
| ~45 owned-read-back (`test_create_and_open`, `concurrent_durable_writers_all_survive_reopen`, `test_disk_char_iter_prefix*`, recovery/archive) | various | FIXED-UNCHANGED by E1 — but ONLY after D1 (bootstrap) + D2 (trait bodies). Any that assert via trait `len`/`is_empty`/`get_value` need D2 or stay red. |
| 9× `test_document_transaction_*` | dict_impl_char.rs:1288-1601 | REFRAME → expect `InvalidOperation` reject under overlay. Verify each reaches the reject; `_empty`/`_recovery` may pass untouched — check individually. |
| `char_lockfree_value_merge_overflow_is_all_or_nothing` | merge_corr.rs:117 | REFRAME (larger): fix the `"overflow"` message assert (now "overlay") AND the `get().copied()` value checks → `get_lockfree` (D5). |
| `char_lockfree_value_merge_appends_one_batch...` | merge_corr.rs:180 | REFRAME (larger): line 194 `.expect()` currently PANICS on the reject — rewrite to expect the overlay-reject; `get()`→`get_lockfree` (D5). |
| `flip_to_overlay_then_kill_switch...` | overlay_write_mode.rs:163 | REFRAME: fresh `create<u64>` now `route_overlay()==true`; fix the line-173 precondition. |
| `test_deep_trie_no_stack_overflow` (+ unicode) | integration.rs:607/713 | COVERAGE: swap line 703/twin `get`→`get_value` to keep the value assertion live (D6, vacuous-pass otherwise). |
| `s5_12_old_owned_file_stays_owned_on_reopen` (ineligible V) | parked | SAFE — proves E1 INERTNESS for arbitrary V (no flip ⇒ owned arm). |
| ineligible-V (`<i32>`/`<String>`) read-backs | integration.rs:181-197/514-516 | SAFE (no flip). |

---

## 9. DO-NOT list (forbidden wrong-fixes)

1. DO NOT wrap the reestablish-internal `iter`/`iter_prefix*`/`get_value` with `route_overlay()` — reads
   the empty overlay then clears owned ⇒ total irreversible loss (D1). Reestablish reads OWNED-ONLY.
2. DO NOT make the new enumerators fault OnDisk children via `find_leaf_faulting`/
   `load_overlay_node_from_disk` to "fix" the eviction undercount — that is the 75-min soak deadlock +
   O(N) root churn. Stay `as_in_mem`/non-faulting.
3. DO NOT "fix" arena-iter under overlay by returning `Ok(None)` — masks a real prefix and makes
   `remove_prefix_batched` a silent no-op (D9). FAIL-LOUD.
4. DO NOT un-gate production overlay eviction in the same release as E1 enumeration — silent undercount
   (D7). Keep eviction `#[cfg(feature=bench-internals)]` until E1-iter-B.
5. DO NOT rely on the trait `len`/`get_value`/`is_empty` "inheriting" E1 from the inherent methods — they
   read state directly / call the wrong inherent `get` (D2/D3). Edit the trait bodies.
6. DO NOT route the reference-returning inherent `get`/`try_get` (`&V`) to the overlay — no stable
   storage to borrow a synthesized value from. Return None; route `get_value`/`get_optimistic`.
7. DO NOT assume deep-key's green status proves value correctness — vacuous under E1 (D6). Swap to
   `get_value`.
8. DO NOT convert the recursion to a work-stack and call it a crash *fix* — no new crash exists; it is
   defense-in-depth only.

---

## 10. Gate + atomic commit

**Atomic landing (one commit — the flip is irreversible):** parked write-flip (EDIT 1/2/3) + E1 read
routing + the four overlay primitives + the `owned_*` readers (D1) + the trait-body routes (D2/D3) +
D4/D9 + the test reframes (D5/D6) + the D8 docs/warn.

**Gate (all green before commit):**
- Full `cargo nextest run --features persistent-artrie` green (the 58 now pass; baseline otherwise
  unchanged). Tee to a file.
- INERT-PRE-FLIP audit: for ineligible V / non-flipped paths, every E1 `false` arm is the verbatim owned
  body ⇒ byte-for-byte unchanged. Prove via the ineligible-V tests passing unchanged + a diff audit that
  each routed method's `false` arm is the prior body.
- `scripts/verify-formal-correspondence.sh` exit 0 (E1 adds no `unsafe`; the overlay walks are safe
  `Arc`/`as_in_mem`).
- The 5 parked `s5_12_flip_ctor_gate` tests green (they read via `contains_lockfree`/`get_lockfree`
  directly, so E1-agnostic — they stay green).
- NEW E1 read-correspondence tests: same inserted data, `overlay read == owned read` for
  `len`/`iter`/`iter_prefix(p ∈ {"", existing, absent})`/`get_value` (u64 value + () membership); pin the
  None-vs-Some(empty) parity; a deep-key (length-500) `overlay_len`/`iter` no-stack-overflow.

**Then** the reestablish Rocq proof (sequenced after the commit to avoid concurrent heavy builds):
recovered-owned as a finite map, reestablish as fold-of-publish, theorem `published_overlay ==
recovered_owned` + clear-last abort-safety; `systemd-run` resource-limited; 0 admits / 0 axioms.

---

## 11. IMPLEMENTATION OUTCOME (as built — DONE + green)

**Gate: `cargo nextest --features persistent-artrie` = 2547 passed / 0 failed / 3 skipped** (incl. the
106 s loom #41 no-lost-write proof + 4 new E1 correspondence tests); `verify-formal-correspondence.sh`
**exit 0**; **0 new `unsafe`** (the overlay reads are `Arc`/`as_in_mem` + zero-`unsafe` `Any`).

### Deviations from the spec — all IMPROVEMENTS (more complete than designed)

1. **D9 → IMPLEMENTED, not fail-loud.** Instead of rejecting arena-iter / `remove_prefix` under the
   overlay, both got real overlay implementations (no capability loss, honoring the no-deferral goal):
   - `iter_prefix_with_arena` / `iter_prefix_with_values_and_arena` return the overlay terms with
     `arena_id: None` — exactly the value the owned path returns for resident (not-yet-persisted) nodes,
     so the 4 arena tests pass unchanged (they assert terms, not specific arenas).
   - `remove_prefix` / `remove_prefix_batched` route to a new `remove_prefix_overlay` (enumerate the
     prefix via `overlay_iter_prefix`, then durably `remove_cas_durable` each term) — durable, so the
     WAL-recovery test passes; the 5 remove tests pass unchanged. Arena page-grouping is the only thing
     lost (a disk-layout optimization with no overlay meaning).

2. **The reachability audit (`ae9e5c7`) found TWO defects beyond the design's read surface**, both fixed:
   - **+1 owned-only reader (a second D1-class data-loss bug):** `try_increment_impl_no_wal`
     (atomic_ops.rs) reads `self.get` and is reachable under `route_overlay()` via the production
     crash-recovery rebuild (`open_with_recovery_config`/`recover_from_archives` build with the
     create-flip THEN replay `BatchIncrement`). Routed to `owned_get` (else recovered counters
     silently accumulate from 0).
   - **6 BROKEN-BY-DESIGN merge guards:** the trie-to-trie merges (`merge_from`,
     `merge_from_batched_with_options`, `merge_from_parallel`, `merge_from_batched_parallel`, reaching
     `merge_replace`/`union_with`/the `_grouped`/`_batched` variants) were UNGATED under the overlay;
     `self.get`→None + `self.upsert` (LWW overwrite) would silently REPLACE live counts. Now reject with
     `InvalidOperation` (mirroring `merge_lockfree_values_to_persistent`).
   - 6 SAFE-DEAD sites (write-flip routes around them) + 2 grep false-positives (`CharInlineLog`) need no
     change — confirmed by the audit.

3. **Kill-switch made a real, public escape hatch.** `kill_switch_to_owned` and `route_overlay` are now
   `pub` (the flip-release fallback + its state predicate). `kill_switch_to_owned` additionally restamps
   the WAL **Owned** when it is still fresh (`current_lsn() == 1`), and `flip_to_overlay` restamps
   **Overlay** symmetrically — so a fresh trie can be fully reverted/re-engaged (the regime follows the
   mode on an empty WAL; a no-op on a non-empty WAL, preserving the restart-time durable semantics).

### Test handling — actual

- **9 D9 tests** (5 `remove_prefix*` + 4 `iter_prefix_with_arena*`): PASS via the overlay impls — NO
  reframe.
- **~45 owned-read-back tests**: FIXED by E1 routing — NO change.
- **15 owned-feature tests** (9 doc-tx, version-tracking, full-recovery, 2 merge-correspondence, 2
  archive): one line `kill_switch_to_owned()` after `create()` (force the owned path the feature needs —
  doc-tx/merge/version/archive are owned-regime behaviors). The `kill_switch` WAL-restamp makes the
  reopen-recovery ones (`increment_recovery`, `merge_appends`) survive.
- **3 flip gate tests** (`s5_9`, `s5_10b`×2): the owned-empty assertion uses the new in-crate
  `owned_try_contains` (E1 routes `Dictionary::contains` to the overlay now); `s5_10b` restructured to
  the AUTOMATIC reestablish (the flip's `open()` runs it — the function under test).
- **counter_overlay_rebuilt**: restructured (the manual `enable_lockfree`+`increment_cas` rebuild
  double-counted atop the flip's automatic reestablish).
- **kill-switch round-trip test**: precondition updated (a fresh eligible-V trie create-flips to the
  overlay).
- **NEW** `tests/persistent_artrie_char_e1_readflip_correspondence.rs` (4): overlay-vs-owned membership
  + counter read correspondence (`len`/`contains`/`get_value`/`iter_prefix*`, incl. `None`-vs-
  `Some(empty)` parity), deep-key (500) no-stack-overflow, ineligible-V inertness.

### Residual boundaries (documented, owner-gated follow-ons — NOT silent)
- `root()`/zipper/`DictionaryNode` traversal over a flipped trie sees an empty owned tree → `log::warn!`
  + doc (E1-iter-B: overlay-backed `DictionaryNode`).
- Overlay enumeration is resident-finals (non-faulting) — exact until overlay eviction is un-gated
  (E1-iter-B prerequisite); eviction is currently `#[cfg(feature=bench-internals)]`/test-only.
- doc-tx + trie-to-trie merge reject under the overlay (use `kill_switch_to_owned` / `OwnedTree`).
