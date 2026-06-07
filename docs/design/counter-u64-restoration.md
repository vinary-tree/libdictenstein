# Counter `u64::MAX` Restoration — Execution Plan (v3, post-red-team-round-2)

Owner GO: restore a FULL-RANGE `u64::MAX` counter. v1 (naive u64::MAX) + v2 (i64::MAX cap) were
red-team-rejected. v3 folds in RT-1 + RT-2. All file:line @ HEAD `7c71fe9`.

## 0. Why this is a CORRECTNESS fix (RT-2 BLOCKER-1)
The `i64::MAX` "cap" is ALREADY bypassed: `upsert_cas_durable*` (`durable_write.rs:413`, byte
`lockfree_cas.rs:1402`) and `merge_from`→`merge_entries_overlay` (`merge_api.rs:36/57`) store a
raw `V` (any `u64`, incl. bit-63) with NO cap. Then value-CAS reads it back as `i64`
(`i64::from_le_bytes`, char `atomic_ops.rs:139`, byte `:142`) → bit-63 → negative → silent
corruption. Fix: read/write the counter leaf as `u64` everywhere; apply the `i64` delta via an
`i128`-checked sum; raise the increment gate to `u64::MAX`.

## 1. Two code-reality corrections (load-bearing)
- **C-A:** byte counter monomorph is `<i64>` on the FAST seam (`CounterValue=i64`
  `overlay_write_mode.rs:407`, block `lockfree_cas.rs:1151`, downcast `lockfree_value_route.rs:54`);
  char is `<u64>`. So byte `CounterValue i64→u64` moves byte `<i64>` increments from the fast seam
  to value-CAS, making byte==char. **byte `<u64>` becomes the canonical fast counter.**
- **C-B:** `overlay_eligible_v()` is `true` for ALL V at HEAD (`overlay_write_mode.rs:126/476`);
  the `{(),u64}`/`{(),i64}` comments are STALE. A `create::<i64>()` DOES flip; char `<i64>`
  routes to value-CAS (its `<u64>` downcast misses). The bulk_mutation test's "i64 never flips"
  reason is a doc bug (outcome correct via value-CAS overflow).

## 2. Architecture (decisions; exact file:line in §7)
1. **Read/write the leaf as `u64`; apply the `i64` delta via `i128`-checked sum; eliminate the
   11 reinterpret sites.** New shared helper (zero `unsafe`, safe `TypeId` branch):
   `counter_leaf_to_i128<V>(bytes)->i128` + `i128_to_counter_leaf<V>(i128)->Option<V>` (u64:
   reject `<0` (0-floor) / `>u64::MAX`; i64: reject `∉[i64::MIN,i64::MAX]`; else→loud). Replaces
   every `serialize(i64)→deserialize(V)` reinterpret + every `i64::from_le_bytes` counter read.
   11 sites enumerated in §7 (overlay-permanent: `flip.rs:1119` counter_value_from_i64, value-CAS
   read/write char `atomic_ops.rs:139/162` + byte `:142/164`; owned-interim/F7-deletes:
   mutation_core `value_from_i64`/`recompute_recovered_increment`, document_tx `value_from_i64_checked`).
2. **`LOCKFREE_COUNTER_MAX → u64::MAX`** (char `lockfree_cas.rs:27`, byte `:49`); fast-seam gate is
   `checked_add` against u64::MAX (drop the redundant `<=` compares).
3. **WAL `BatchIncrement` delta + `RecoveredOperation::Increment.delta` STAY `i64`** — a single
   delta is i64-bounded by the public API; only the ACCUMULATOR (leaf) is u64. NO shared-trait sig
   change (RT-B BLOCKER-1 N/A).
4. **byte `CounterValue i64→u64`**, block `<i64,S>→<u64,S>`, seam downcasts `<i64,S>→<u64,S>`
   (publisher/get/publish_inner/route_increment_bytes). byte fast seam now serves `<u64>`.
5. **KEEP `Counter={i64,u64}`** (RT-2 BLOCKER-2). libgrammstein `accumulator.rs:80`
   `PersistentARTrieChar<i64>` + `.increment`/`.increment_by(-k)` routes to value-CAS (V-generic,
   i64 semantics) — UNAFFECTED. Do NOT drop i64.
6. **0-floor (decrement<0 → REJECT, not clamp), per-CAS-iteration** on value-CAS (i128 `new<0`),
   on the SAME fresh read as the CAS (RT-2 confirmed LP-safe). byte: ROUTE negative deltas to
   value-CAS first (mirror char's `route_increment` `if delta<0 {None}`).
7. **Counter-seam `None`:** `increment_publish_inner None=>Ok((0,0))` (byte
   `overlay_write_mode.rs:747`, char `:379`) → loud Err/`debug_assert!` (monomorph mismatch;
   Ok((0,0)) ranks a real delta at gen 0). BUT `overlay_counter_get` `None` is LEGITIMATE-absent
   (`flip.rs:629`) → keep returning None for absent (RT-2 MAJOR-4 split).
8. **byte C4 wrappers + test (RT-2 MAJOR-5):** `insert_cas_with_value_durable`/`upsert_cas_durable`
   (`lockfree_cas.rs:1376/1402`, `value:i64`, `<0` guard) → `value:u64`, drop dead guard; migrate
   `c4_negative_value_is_rejected_not_wrapped` (`:1909`) to u64 overflow/underflow asserts.
9. **Specs (RT-2 MAJOR-6):** re-model `PersistentTransactionIncrementRecovery.tla:28-29`
   (symmetric `(-Max)..Max` → `0..Max` + below-0 reject) + re-TLC; `LockFreeCounterMergeAtomicity.tla`
   + Rocq `LockFreeCounterMergeSpec.v` already non-negative (add an "arbitrary merge_fn is
   caller-checked, out of spec" over-approx note).
10. **B9 doc:** fix `bincode_compat.rs:11-13` (stale `standard()`/varint; code is `legacy()`/fixint —
    load-bearing for the i64→u64 leaf byte-identity).
11. **upsert/merge:** under read-as-u64, a bit-63 leaf is a VALID u64 — upsert/merge store any u64
    (raw set; counter checked-semantics apply to increment/decrement only). Dissolves BLOCKER-1.
    `merge_from(|a,b|a+b)` can u64-WRAP (user's merge_fn responsibility; recommend saturating).

## 3. Public API
`increment/fetch_add(delta:i64)->Result<i64>` UNCHANGED (zero blast radius). The `i64` RETURN gets
`i64::try_from(count)` (a count >i64::MAX errors LOUD on the i64 return, not wraps — an
asymmetry: storable to u64::MAX, readable via the i64 `increment` only to i64::MAX). Recommend a
future additive `increment_u64->Result<u64>` + `get_counter->Option<u64>` (out of scope; schedule).
Fix byte `overlay_counter_get` `count as i64` (`:573`) → `Option<u64>` + all counter `as i64/as u64`.

## 4. Cross-repo (build-check only; siblings unaffected)
libgrammstein `<i64>` accumulator → value-CAS (unaffected); its `<u64>` read-only dumps benefit;
`<NgramEntry>` is arbitrary-V (unaffected). pgmcp `<u64>` (read-only) + `()` membership — unaffected.
lling-llang / liblevenshtein `.fetch_add` are `std::atomic` — unaffected. Build-check all 4 = exit 0.

## 5. F7 ordering
Counter-u64 FIRST (closes the LIVE overlay corruption immediately; F7 deletes only the OWNED
bridges, not the overlay ones, so it doesn't subsume this). The 6 owned-interim bridge conversions
(mutation_core/document_tx) are throwaway — F7 deletes the functions+callsites+owned-counter-tests
together (no gap, no double-fix). Overlay-permanent: `flip.rs` reconcile + value-CAS + the helper.

## 6. Verification
Build+grep gate (no `CounterValue=i64`; no counter `<i64,S>` block; no `i64::from_le_bytes` in the
counter value-CAS/flip/doc-tx/recovery paths; `LOCKFREE_COUNTER_MAX==u64::MAX`; 0 new unsafe).
Tests: (1) **u64::MAX data-loss proof** — count crossing i64::MAX survives reopen + merge +
concurrent increment (THE test v2 couldn't pass); (2) decrement-to-0 ok / below-0 rejected, both
variants, LP-safe under contention; (3) `<i64>` tries (char + byte post-C-A) via value-CAS behave
identical to pre-v3 (+ a byte-`<i64>`-overlay migration witness: fast-seam→value-CAS, same counts);
(4) byte C4 test migrated; (5) downcast-miss → loud (not Ok((0,0))); (6) absent counter read →
None (not panic); (7) format-compat (pre-change byte counter file reopens bit-identical;
`open_with_legacy_loader`==`open_with_f5_loader`); (8) i64-return guard (count >i64::MAX errors,
not wraps); (9) TLC re-modeled spec + merge/Rocq specs green; (10) full suite 0 fail, formal gate
exit 0, all 4 siblings build.

## 7. Edit list (file:line @ 7c71fe9)
PERMANENT (overlay): `value.rs:117/120` keep both Counter impls; char `lockfree_cas.rs:27`+byte
`:49` LOCKFREE_COUNTER_MAX=u64::MAX (+drop redundant compares `:1227/1263` & char twin); byte
`overlay_write_mode.rs:407`(CounterValue=u64)/535-563(publisher <u64,S>)/565-574(get→Option<u64>)/
696-715(bound_increment_delta=char's)/726-749(publish_inner <u64,S>, None→loud); byte
`lockfree_cas.rs:1151`(block <u64,S>)/1160-1166(get_lockfree drop cast)/1192-1303(try_increment
leaf u64)/1376+1402(C4 wrappers value:u64, drop guards)/1909(C4 test migrate); byte+char
`lockfree_value_route.rs`(route downcast <u64,S> + negative→None); char+byte `atomic_ops.rs`
value-CAS (the shared i128 helper + 0-floor + i64::try_from return); `overlay_write_mode.rs`
publish_counter downcast-miss→debug_assert (char :210/byte :558); `flip.rs:1039/1073/1119`
(counter_value_from_i64: absolute u64::try_from, delta→seam, delete reinterpret+false comment).
INTERIM (owned; F7 deletes): char `mutation_core.rs:351/406`, byte `:608/652-673`, char
`document_tx.rs:482/561-573`, byte `:143/167/327-351` (u64 read + i128 aggregate). SPECS/DOCS:
`PersistentTransactionIncrementRecovery.tla:28-29`+.cfg (re-model+TLC); `LockFreeCounterMergeAtomicity.tla`
+`LockFreeCounterMergeSpec.v` (over-approx note); `bincode_compat.rs:11-13` (doc); stale-comment
fixes (char `mmap_ctor.rs:88`, byte `overlay_write_mode.rs:1019`, `persistent_bulk_mutation_correspondence.rs:391-392`).
NO change: WAL codec BatchIncrement i64, `RecoveredOperation::Increment.{delta,result}` i64, owned
`WalRecord::Increment{i64}`, public `increment/fetch_add(delta:i64)->Result<i64>` sig.

## 8. Self-red-team residuals (for round 3 to attack)
- RR-1: the bincode-bytes boundary remains (now TypeId-aware, not sign-reinterpret) — strictly
  safer; needs a loud `else` + a Counter-exhaustiveness test (a future 3rd Counter type unbranched
  → silent default).
- RR-3 (HIGHEST): byte `<i64>` overlay increment migrates fast-seam→value-CAS (counts identical,
  WAL record type BatchIncrement→Upsert). Grep byte `<i64>` tests for WAL-shape assertions before
  landing; the 3 trace/atomicity/durability suites must pass through the new route.
- RR-4: `merge_from(|a,b|a+b)` u64-wraps (caller's merge_fn responsibility; documented; specs model
  the checked merge).
- RR-5: `increment->Result<i64>` errors on counts >i64::MAX (correct, but an API asymmetry; needs
  the deferred `increment_u64`/`get_counter` accessor for full readability).
- RR-2 (benign): no genuine pre-existing-negative `<u64>` leaf exists (the corruption was always
  "valid u64 misread as i64"); read-as-u64 fixes it.
- F7: confirm the F7 plan deletes the 6 interim owned bridges + callsites + owned-counter tests
  together.

# === v4 (FINAL DESIGN — owner decisions locked, round-3 blockers resolved) ===

## Owner decisions (2026-06-07)
- **Canonical unsigned counter = `u64`** (NOT u128). u128-WIDENING NOTE: u128 is a clean future
  upgrade if a large-SUM accumulator use case ever arises (not event-counting, which u64 covers
  to ~584yr @ 1e9/s). Cost when/if taken: 16-byte leaf, a `u128`-native sign-branch helper arm
  (the `i128`-intermediate trick below doesn't fit u128), a widened public return. Documented in
  the helper + `value.rs` so the path is obvious. Do NOT implement u128 now.
- **Migrate byte fast seam `<i64>`→`<u64>`** (canonical); byte `<i64>` (signed accumulator) keeps
  working via value-CAS, like char `<i64>`.
- **Helper everywhere** (overlay + owned paths) — no interim data-loss hole.

## v4 resolves every round-3 blocker
- **BLOCKER-1 (66-callsite churn):** ACCEPTED — migrate byte `<i64>`→`<u64>` inherent-method
  test callsites (`lockfree_cas.rs` ~29 test fns / ~66 sites: `try_increment_cas_durable_survives_reopen`,
  `valued_durable_writes_survive_reopen`, `c4_*`, `durable_writes_reject_non_synchronous_policy`,
  the `:2227/2292/2394/2423/2442` fns, etc.): `s/PersistentARTrie::<i64>/::<u64>/` + drop the
  `-N`/negative-literal asserts (no sign on u64). Mechanical. No production byte-`<i64>` consumer.
- **BLOCKER-2 + MAJOR-5/7 (owned bodies/merge i64-capped):** the i128 TypeId-keyed helper is used
  in the OWNED increment bodies (char `atomic_ops.rs:68/204`, byte `:78`) + recovery appliers
  (`mutation_core` `recompute_recovered_increment`/`value_from_i64`) + doc-tx folds + the
  lockfree-merge readers (char `lockfree_cas.rs:2098/2113/2123`). Per-type range: `u64`→`[0,u64::MAX]`,
  `i64`→`[i64::MIN,i64::MAX]`. ⇒ full-range u64 for `<u64>`, i64 semantics for `<i64>`, everywhere.
  The i64-overflow recovery-stop tests (`persistent_transaction_increment_correspondence.rs:148/183`)
  STILL PASS (their tries are `<i64>` → helper's i64-branch keeps the i64::MAX stop).
- **BLOCKER-3 (route `as i64` silent wrap):** char `lockfree_value_route.rs:100` `v as i64` + byte
  `:57` → `i64::try_from(count).map_err(overflow)` (the public-return guard; a count >i64::MAX
  errors LOUD, never wraps). Plus byte `overlay_counter_get` `count as i64` → return `Option<u64>`.
- **MAJOR-4 (helper keying):** `counter_value_from_i64`/the helper at `flip.rs:1119` is on
  `V: DictionaryValue` (reachable as `()`/`String` in F5 replay), so its TypeId branch returns
  `None` GRACEFULLY for a non-`{i64,u64}` V (pass-through to the membership/value reconcile) — the
  loud `else` lives ONLY in the counter-monomorph-keyed seam (`increment_publish_inner`, which a
  non-counter V never reaches). Delete the false "negative→None" comment; the i64-sign guard is
  explicit (`if v<0`), not bincode-reliant.
- **MAJOR-6 (TLA):** re-model `PersistentTransactionIncrementRecovery.tla:28-29` to `0..MaxCounter`
  + below-0 reject; re-TLC. merge/Rocq specs get the "arbitrary merge_fn caller-checked" over-approx note.
- **B9:** fix `bincode_compat.rs:11-13` (legacy/fixint doc).

## The shared helper (the heart — zero unsafe)
`counter_leaf_to_i128<V>(le_bytes: &[u8]) -> Option<i128>`: TypeId — `V==u64` → `u64::from_le_bytes
as i128`; `V==i64` → `i64::from_le_bytes as i128`; else → `None` (graceful; non-counter V).
`i128_to_counter_leaf<V>(n: i128) -> Option<Vec<u8>>` (the new value, LE): `V==u64` → reject
`n<0`(0-floor)/`n>u64::MAX`, else `(n as u64).to_le_bytes()`; `V==i64` → reject `n∉[i64::MIN,i64::MAX]`,
else `(n as i64).to_le_bytes()`; else → `None`. Used by EVERY counter leaf read/write (overlay
value-CAS, owned increment, recovery, doc-tx, merge). The fast commutative seam stays add-only
`u64` (`checked_add` vs `u64::MAX`); decrement routes to value-CAS (the i128 helper + 0-floor,
per-CAS-iteration on the same fresh read = LP-safe). WAL `BatchIncrement` delta stays `i64`
(single delta ≤ i64::MAX); `RecoveredOperation::Increment.delta` stays `i64`.

## Verification (final gate)
The §6 v3 list, plus: the i64-overflow recovery-stop tests pass (helper i64-branch); the migrated
~66 byte tests pass on `<u64>`; a `<u64>` count crossing i64::MAX survives reopen+merge+concurrent
(THE proof); decrement-to-0 ok / below-0 rejected; downcast-miss loud only on the durable seam,
absent-read None; all 4 siblings build; TLC re-modeled spec green; full suite 0 fail; formal gate
exit 0; 0 unsafe.

## F7 ordering: counter-u64 FIRST (closes the live overlay corruption), then F7.

# === v5 (round-4 resolutions — grep-driven completeness; READY TO IMPLEMENT) ===

## The convergence mechanism: GREP-DRIVEN completeness (not enumeration)
4 red-team rounds each found a few more counter-leaf sites the design's enumeration missed.
The PRINCIPLE (the i128 TypeId-keyed helper at EVERY counter-leaf read/write) is sound; the
ENUMERATION is what keeps being incomplete. So v5 makes completeness mechanical + provable:
- **Implementation rule:** EVERY counter-leaf read (`i64::from_le_bytes`/`u64::from_le_bytes` on a
  counter value) and EVERY counter-leaf write/return (`as i64`/`as u64`/`new_val as i64` on a
  counter value) routes through `counter_leaf_to_i128<V>` / `i128_to_counter_leaf<V>`.
- **Completeness GATE (the proof):** after implementation, `rg` finds ZERO `i64::from_le_bytes`,
  `as i64`, or `as u64` in any counter path (the counter blocks in `lockfree_cas.rs`,
  `atomic_ops.rs`, `overlay_write_mode.rs`, `flip.rs` counter arms, `mutation_core.rs` counter
  recovery, `document_tx.rs` counter folds, the merge readers). This gate catches ANY missed
  site at build time — it is the convergence proof, replacing fragile hand-enumeration.

## Round-4 resolutions (added to the helper-everywhere list)
- **F1 (value-CAS) — already in scope** (v3 §7 lists char `atomic_ops:139/162`, byte `:142/164`);
  v5 reaffirms: the value-CAS increment paths (`increment_via_value_cas` char `atomic_ops:120-184`,
  `increment_bytes_via_value_cas` byte `:129-181`) use the helper + the 0-floor. These are the LIVE
  decrement seams — non-negotiable.
- **F2 (real):** `increment_publish_inner` (byte `overlay_write_mode:726`, char `:369`): the count
  return must carry full `u64` — change the returned count to `u64` (the tuple is `(count, gen)`;
  callers up to the public boundary then apply the single `i64::try_from` guard at the public
  `increment`/`fetch_add` return ONLY). Remove `new_val as i64` at `:745`.
- **F3 (real):** corrected cast inventory — the store/return truncations `lockfree_cas:1277`
  (`new_val as i64`), `overlay_write_mode:745`; plus the read casts `lockfree_cas:1165/1259/1538`,
  `overlay_write_mode:561` — ALL replaced by the helper / native u64. (No byte `:57` cast — phantom.)
- **F4 (real):** byte merge readers `lockfree_cas:1497/1513/1519` (+`:1490/1531` signatures) use the
  helper (symmetric to the char merge readers already listed). Full-range u64 merge for `<u64>`.
- **F5 (scope):** byte-`<i64>`→`<u64>` migration is ~139 refs / 27 files. SCOPE FENCE: migrate ONLY
  the inherent-counter-method callsites in `lockfree_cas.rs` (the block move) + the routing seams +
  the C4 test. The bench/example consumers (`parallel_merge_benchmarks.rs`, `transaction_benchmarks.rs`,
  `streaming_merge_test.rs`, `batched_merge_comparison.rs`) and the correspondence-test `<i64>` tries
  (esp. `persistent_transaction_increment_correspondence.rs:148/183` — the i64::MAX-recovery-STOP
  tests) use the PUBLIC API on `<i64>` tries and MUST STAY `<i64>` (they route via value-CAS, keep
  i64 semantics, and pin the i64-branch recovery-stop — do NOT `s/<i64>/<u64>/` them).

## STATUS: design CONVERGED for implementation via the grep gate. Implement v5 (counter-u64), then F7.

# === v6 (round-5 resolution — COMPLETE gate over the exhausted access-mechanism space) ===

## Round-5 finding (real, accepted): the 3-pattern gate was BLIND to the writeback funnels
A counter leaf is 8 LE bytes. It is touched by EXACTLY TWO mechanisms (proven exhaustive: a repo
scan for `from_ne_bytes`/`from_be_bytes`/`transmute`/`bytemuck` on counter values returns ZERO):
  (1) MANUAL byte ops — `i64::from_le_bytes` (read), `as i64`/`as u64` (cast). [v5's 3-pattern gate]
  (2) BINCODE round-trip — `bincode_compat::serialize(&i64_typed) → deserialize::<V>()`. [GATE-BLIND in v5]
The corruption substrate for a `<u64>` count > i64::MAX is NOT the round-trip bit-pattern (i64
two's-complement == u64 bits for the same 8 bytes — preserved); it is that the intermediate value is
typed `i64` and the arithmetic (`checked_add`) runs in the i64 domain on a value read as a NEGATIVE
i64 → wrong magnitude / spurious `exceeds i64 range`. The writeback funnels carry the i64 typing:
  flip.rs `counter_value_from_i64`; byte/char `atomic_ops` value-CAS + sync-increment stores
  (`increment*_via_value_cas`, `try_increment_impl_no_wal`); byte/char `document_tx`
  `value_from_i64_checked`/`i64_from_value_lossy`; byte/char `mutation_core`
  `value_from_i64`/`value_from_recovered_i64`.
These match none of v5's 3 patterns (no `from_le_bytes`, no `as`) → v5's gate passes green while the
i64-domain arithmetic persists. **This is why every prior round kept finding "one more site": the
hand-enumeration was chasing the bincode funnels the gate couldn't see.**

## v6 COMPLETE gate (the convergence): name BOTH mechanisms
The completeness gate is now the union, each required to return ZERO in counter code paths:
  MANUAL:  `i64::from_le_bytes` · `as i64` · `as u64`
  BINCODE: `deserialize::<i64>` · `deserialize::<u64>` · bare `bincode_compat::serialize` /
           `bincode_compat::deserialize` *inside any counter-leaf function* (the funnels listed
           above) — outside the shared helper, these must be zero.
Equivalently, the POSITIVE form (easier to audit): EVERY counter-leaf read/write goes through
`counter_leaf_to_i128<V>` / `i128_to_counter_leaf<V>`, AND the funnel functions are rewritten to
carry **i128** end-to-end (signatures change from `*_i64(x: i64)` to taking/returning i128 or `V`),
with the SINGLE `i64::try_from` / u64-range guard living ONLY in the two helpers. Because the
intermediate is i128 (not i64), the compiler itself flags any residual i64-typed funnel (type
mismatch) — the gate + the type system together are the completeness proof.

## Why v6 is CONVERGED (no third mechanism can exist)
8 on-disk bytes ⟹ read/written by manual byte ops OR a serde codec. The crate's only codec is
`bincode_compat` (legacy/fixint). Both are now gated. The scan for alternative byte decoders is 0.
∴ the gate's pattern set is complete over the access-mechanism space — closed enumeration, not a
"we think we got them all." Implementation routes all funnels through the i128 helper; the v6 gate
(both pattern groups = 0 outside the helper) + the i128 retyping (compiler-enforced) prove it.

## STATUS: design CONVERGED (v6 gate complete over exhausted access space). Implement, then F7.

# === CONVERGED (round-6 independent double-check) ===
Round-5 produced the complete v6 gate; round-6 (independent, fresh-context) double-checked and
returned CONVERGED: (A) no third access mechanism (from_ne/be_bytes/transmute/bytemuck/ptr::read=0;
only codec = bincode_compat; the u64::from_le_bytes hits decode SwizzledPtr/headers, not values),
(B) no escaping instance (counter-leaf-function set is enumerable; inferred-V writebacks caught by
the bare-bincode clause; generic-V passthrough stores raw V with no i64 arithmetic → correctly
outside the gate; sealed Counter={i64,u64} closes the TypeId-else risk). The 6-round process:
r3→v4 (blockers), r4→v5 (grep-driven), r5→v6 (complete gate over 2 mechanisms), r6 CONVERGED.
IMPLEMENTING NOW: helper → byte<u64> migration → route funnels → gate → verify; then F7.

# === API decision (owner, this session): increment/fetch_add return Result<V> ===
Owner chose the NATIVE-type return over the i64 bit-pattern convention: `increment` /
`fetch_add` return `Result<V>` (a `<u64>` trie → `u64`, a `<i64>` trie → `i64`), keeping
`Counter = {i64, u64}`. The `delta` arg STAYS `i64` (signed — decrement a u64 counter by a
negative delta; only a NEGATIVE RESULT is rejected, not a negative delta). No cross-repo
change: libgrammstein's `<i64>` char counter keeps its `i64` return. Rationale: returning i64
for a u64 counter hands back a negative bit-pattern for counts > i64::MAX and forces a manual
`as u64` — it lies about the value and undercuts the u64 restoration. Threads through:
increment(_bytes) / fetch_add / increment_via_value_cas / increment_bytes_via_value_cas →
Result<V>; route_increment(_bytes) → Option<Result<V>> (re-wrap CounterValue as V via the
flip `counter_as_value` Any-bridge); try_increment_cas_durable_default already returns
Self::CounterValue (== V for the counter monomorph). `counter_codec::counter_return_i64` is
RETIRED from the public path (kept only if an i64-typed internal seam still needs it).

# === IMPLEMENTATION COMPLETE (results) ===
Implemented the v6 design (helper-everywhere + byte→<u64> migration + Result<V> API) via a
delegated sweep, then HARDENED by review + two independent red-teams that caught THREE gaps the
sweep left (the lesson: "gate-clean + tests-green" hid two data-loss gaps):
- **doc-tx staging i64-domain (byte+char)** — a <u64> counter incremented via a document
  transaction was capped at i64::MAX. FIXED: staging absolute value computed in i128
  (value_to_i128_lossy + value_from_i128_checked); per-tx aggregated WAL delta stays i64.
- **flip.rs counter_value_from_i64 `v as i128` (DATA-LOSS BLOCKER)** — the overlay reconcile of an
  absolute WalRecord::Increment whose i64 `result` is the NEGATIVE bit-pattern of a u64 > i64::MAX
  decoded it as a negative → i128_to_counter_value::<u64> None → record DROPPED → counter reverts.
  FIXED: leaf-decode (counter_leaf_to_i128(&v.to_le_bytes())), mirroring the (correct, tested) owned
  appliers. Regression test added + PROVEN to discriminate (fails with the bug, passes with the fix).
- **char mmap_ctor.rs legacy applier (MINOR)** — raw counter-leaf bincode outside the helper
  (byte-identical leaf, not data-loss); routed through counter_codec for gate-completeness.

VERIFICATION (all green): full nextest **2673 passed / 0 failed / 3 skipped**; v6 grep gate EMPTY
outside counter_codec; `cargo check` feature-on AND feature-off clean; doctests 147 passed;
formal-correspondence gate exit 0 (unsafe-inventory MATCHES — zero new unsafe); fmt clean;
liblevenshtein-rust builds green. Four data-loss-proof tests for counts > i64::MAX: checkpoint-image
reopen, pure-WAL-delta reopen, RAW increment_cas → checkpoint → reopen (libgrammstein's hot path),
overlay-reconcile of a ranked absolute Increment.

LIBGRAMMSTEIN: the byte counter API is now `impl<S> PersistentARTrie<u64, S>` (lockfree_cas.rs) —
the root cause (#1) that forced libgrammstein's u64→i64 churn is RESOLVED; #4/#5/#6 (durability,
recovery, CAS-no-lost-increment) confirmed/tested for u64. #2 (read-routing single-source-of-truth),
#3 (merge-to-persistent obsolete), #7 (overlay iteration) are intended overlay-default-architecture
consequences = libgrammstein-side adaptations, NOT counter bugs (libdictenstein unchanged for them).
