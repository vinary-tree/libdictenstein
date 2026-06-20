# Counter u64 restoration — EXECUTION SPEC (v6 converged + Result<V> API)

This is the precise, per-file execution spec for the CONVERGED design
(`counter-u64-restoration.md`). The shared helper is ALREADY built + unit-tested
(`src/persistent_artrie_core/counter_codec.rs`, 9 tests green). This spec routes every
counter-leaf access through it, migrates the byte counter monomorph to `<u64>`, and
makes `increment`/`fetch_add` return `Result<V>`.

## The helper (DONE — use it, do not re-create)
`crate::persistent_artrie_core::counter_codec`:
- `counter_leaf_to_i128::<V>(&[u8]) -> Option<i128>` — decode 8-LE-byte leaf (u64/i64; None else/len≠8)
- `counter_value_to_i128::<V>(&V) -> Option<i128>` — decode a typed V (confines `serialize`)
- `i128_to_counter_leaf::<V>(i128) -> Option<Vec<u8>>` — encode → leaf bytes, range-checked
  (u64: `[0,u64::MAX]`; i64: `[i64::MIN,i64::MAX]`); byte-identical to `serialize(&(n as V))`
- `i128_to_counter_value::<V>(i128) -> Option<V>` — encode → typed V (confines `deserialize`)
- `split_u64_delta_to_i64_chunks(u64) -> Vec<i64>` — ≤3 i64-bounded chunks summing to the u64 (merge)
- `counter_return_i64(i128) -> i64` — bit-pattern narrow (kept ONLY for any i64-typed internal seam;
  NOT used on the public `Result<V>` path)

## Locked decisions
- **Public API:** `increment(_bytes)` / `fetch_add` return **`Result<V>`** (V: Counter); `delta` STAYS
  `i64` (signed; decrement a u64 counter via negative delta — only a NEGATIVE RESULT is rejected).
- **byte counter monomorph:** migrate `impl<S> PersistentARTrie<i64,S>` (lockfree_cas.rs) →
  `<u64,S>`; `LockFreeOverlay::CounterValue = i64` → `u64`; all `<i64,S>` Any-downcasts → `<u64,S>`.
- **`LOCKFREE_COUNTER_MAX`** (both variants) → `u64::MAX`. Remove the now-vacuous `delta > MAX` /
  `v <= MAX` checks (they'd be tautologies on u64 → clippy warns); rely on `checked_add` for overflow.
- **Decrement routing:** `route_increment(_bytes)` returns `None` when `delta < 0` (→ caller's
  value-CAS path), exactly like char's existing `route_increment`. The add-only fast seam is
  non-negative-only.
- **Merge:** full-u64 support via `split_u64_delta_to_i64_chunks` on the WAL delta (BatchIncrement is
  commutative; replay sums in i128). The WAL `BatchIncrement`/`Increment` delta field STAYS `i64`.
- **bincode_compat.rs:11-13 doc** is STALE (says standard()/varint) — fix to legacy()/fixint LE.

## The v6 completeness GATE (the convergence proof — must hold after the sweep)
ZERO occurrences, in any counter code path OUTSIDE `counter_codec.rs`, of:
  `i64::from_le_bytes` · `as i64` · `as u64` · `deserialize::<i64>` · `deserialize::<u64>` ·
  bare `bincode_compat::serialize`/`deserialize` inside a counter-leaf function.
Counter code paths = the counter blocks/funnels in: byte+char `{lockfree_cas, atomic_ops,
overlay_write_mode, lockfree_value_route, mutation_core, document_tx}.rs`, `overlay/flip.rs` counter
arms. (Widening `as i128` is NOT forbidden — it is lossless.) Use the helper everywhere instead.

## SCOPE FENCE — do NOT migrate to <u64> (leave <i64>):
- `tests/`, `benches/`, `examples/` `PersistentARTrie<i64>` / `PersistentARTrieChar<i64>` (public-API
  consumers + correspondence). ESPECIALLY `tests/persistent_transaction_increment_correspondence.rs`
  (its <i64> tries pin the i64::MAX-stop recovery semantics — they MUST stay <i64> and keep passing).
- The `Counter` trait stays sealed `{i64, u64}` (value.rs) — i64 counters remain first-class.

---

## PER-FILE transformations

### A. `src/persistent_artrie/lockfree_cas.rs` (byte counter monomorph — the big one)
1. `const LOCKFREE_COUNTER_MAX: u64 = i64::MAX as u64;` (:49) → `= u64::MAX;` Update the doc comments
   (:26-28, :1143-1147) to say the byte counter is now a `u64` (matching char).
2. `impl<S: BlockStorage> PersistentARTrie<i64, S>` (:1151) → `PersistentARTrie<u64, S>`. Update the
   block doc (:1143-1150).
3. `get_lockfree` (:1160): leaf is now u64 → drop `.map(|v| v as u64)` (get_value() already u64).
4. `try_increment_cas_inner` (:1212): `PersistentNode::<i64>` → `<u64>`; drop `.map(|v| v as u64)`
   (:1259); the overflow guard becomes `match cur.checked_add(delta) { Some(v) => v, None =>
   overflow }` (remove the `> MAX` early-return :1227-1229 and `v <= MAX` :1264 — vacuous on u64);
   `build_value_path_recursive(&root, key, 0, new_val as i64)` (:1277) → `..., new_val)` (u64 leaf).
5. `try_increment_cas`/`increment_cas` (:1192/:1321): `delta: u64` unchanged; returns u64.
6. `try_increment_cas_durable` (:1347): delegates to the trait default which now returns
   `Self::CounterValue = u64`; sig becomes `(key, delta: i64) -> Result<u64>`? NO — keep it taking the
   CounterValue per the trait. Check the trait default signature: `try_increment_cas_durable_default(
   key, delta: Self::CounterValue) -> Result<Self::CounterValue>`. After migration CounterValue=u64,
   so this is `(key, delta: u64) -> Result<u64>` — MATCHING char. The inherent wrapper at :1347 must
   match (currently `(key, delta: i64) -> Result<i64>`): change to `(key, delta: u64) -> Result<u64>`
   and update the `<Self as DurableOverlayWrite<ByteKey, i64, S>>` to `<.. u64 ..>`. The UTF-8 key
   guard stays. (Callers: `route_increment_bytes` — see C.)
7. `insert_cas_with_value_durable` (:1376) / `upsert_cas_durable` (:1402): `value: i64` → `value: u64`.
8. **Merge readers:** `current_i64_for_lockfree_merge` (:1513) → `current_i128_for_lockfree_merge(
   term) -> Result<i128>` using `counter_value_to_i128::<u64>(&self.get_value_impl(term)
   .unwrap_or(0))` (or read the leaf via the helper); `prepare_lockfree_value_merge` (:1487): do
   `new_value_i128 = current_i128 + delta_i128` in i128, bound via `i128_to_counter_leaf::<u64>`
   (reject overflow), prepared value via `i128_to_counter_value::<u64>`; for the WAL delta, replace
   `lockfree_delta_to_i64` (:1519, which i64::try_from-rejects >i64::MAX) with
   `split_u64_delta_to_i64_chunks(delta)` pushing ≤3 `(key, chunk)` wal_entries. `prepared_values`
   stays one entry/key (the final u64 value). `collect_lockfree_entries_recursive` (:1531): leaf is
   u64 → drop `value as u64` (:1538).
9. The in-file `#[cfg(test)] mod` (≈:1640+): the counter tests use `PersistentARTrie::<i64,_>` and
   `try_increment_cas_durable(b"..", N)`. These test the byte COUNTER → migrate to `<u64,_>` and
   `delta` as u64 where the durable sig now takes u64. The `c4_negative_value_is_rejected_not_wrapped`
   test (≈:1909) tested the i64 reject; reframe it to assert the value-domain reject still holds for
   u64 (a value-CAS decrement below 0 is rejected; the durable add-only seam rejects a negative delta
   via `bound_increment_delta`). KEEP every test GREEN; adjust expected types i64→u64.

### B. `src/persistent_artrie/overlay_write_mode.rs` (byte seam)
1. `type CounterValue = i64;` (:407) → `u64`. Update the doc (:403-406).
2. `overlay_publish_counter(units, value: i64)` (:535): sig `value: u64`; the `value >= 0` debug/guard
   is now vacuous (u64) → drop it; downcast `<i64,S>` (:559) → `<u64,S>`; `increment_cas(units,
   value as u64)` → `increment_cas(units, value)` (already u64).
3. `overlay_counter_get(units) -> Option<i64>` (:565) → `-> Option<u64>`; downcast `<u64,S>`; drop
   `.map(|count| count as i64)` (:573) → return `get_lockfree(units)` directly (Option<u64>).
4. `bound_increment_delta(key, delta: i64) -> Result<i64>` (:696): KEEP — rejects negative delta
   (the add-only seam). Doc: the byte counter is now u64 (the i64::MAX-cap remark is obsolete; the
   reject is the NEGATIVE delta only).
5. `increment_publish_inner(key, delta) -> Result<(CounterValue, u64)>` (:726): downcast `<i64,S>`
   (:741) → `<u64,S>`; `try_increment_cas_inner(key.as_bytes(), delta as u64)` — `delta` is now the
   CounterValue=u64 (the trait sig changed), so DROP `as u64` (already u64); `Ok((new_val as i64, gen))`
   (:745) → `Ok((new_val, gen))` (CounterValue=u64). `None => Ok((0,0))` stays.
   NB the trait method sig (`durable_write.rs`) is `delta: Self::CounterValue` → u64; the byte impl
   header `impl<V: DictionaryValue, S> DurableOverlayWrite<ByteKey, V, S>` stays generic-V; the
   counter seams operate at CounterValue=u64.

### C. `src/persistent_artrie/lockfree_value_route.rs`
`route_increment_bytes<V,S>(trie, term, delta: i64) -> Option<Result<i128>>` (:45): downcast
`<u64,S>` (:54); **if `delta < 0` return `None`** (decrement → caller value-CAS); `let delta_u64 =
u64::try_from(delta).ok()?;` (NO `as u64`); `Some(trie_u64.try_increment_cas_durable(term, delta_u64)
.map(|count| count as i128))` (widening `as i128` is allowed). (Return i128 so the V-aware caller in
atomic_ops converts via `i128_to_counter_value::<V>`.)

### D. `src/persistent_artrie/atomic_ops.rs` (byte public API)
1. `increment` (:35) / `increment_bytes` (:56): `-> Result<V>` (was `Result<i64>`). Route branch:
   `if let Some(routed) = route_increment_bytes(self, term, delta) { return routed.and_then(|n|
   counter_codec::i128_to_counter_value::<V>(n).ok_or_else(|| ..overflow..)); }` then
   `return self.increment_bytes_via_value_cas(term, delta);`. **Owned body** (:72-119): read current
   via `counter_value_to_i128::<V>(&v)` (replaces serialize→from_le_bytes :74-88); `let new_i128 =
   cur_i128.checked_add(delta as i128)...` (delta widening `as i128` ok); store via
   `i128_to_counter_value::<V>(new_i128)` (replaces serialize(&new_value)→deserialize :102-107);
   the WAL `Increment{delta, result}` — `result` field is i64: use `counter_return_i64(new_i128)`
   (bit-pattern; recovery recomputes so this is informational); return `i128_to_counter_value::<V>(
   new_i128)`.
2. `increment_bytes_via_value_cas` (:129): `-> Result<V>`; read cur via `counter_value_to_i128::<V>`,
   arith i128, new_v via `i128_to_counter_value::<V>`, CAS, return the V.
3. `fetch_add` (:394): `-> Result<V>`; `let new_v = self.increment(term, delta)?; let new_i128 =
   counter_value_to_i128::<V>(&new_v)...; i128_to_counter_value::<V>(new_i128 - delta as i128)
   .ok_or(..)`.
4. Add `use crate::persistent_artrie_core::counter_codec;`.

### E. char files — symmetric (already `<u64>`, so NO type migration; just helper + gate + Result<V>)
- `lockfree_cas.rs`: `LOCKFREE_COUNTER_MAX` (:27) → `u64::MAX`; remove vacuous `> MAX`/`<= MAX`
  checks (:1739,:1792,:1930,:1955 — keep the ones that are real value-domain rejects, drop tautologies);
  merge readers (:2098 `current_i64_for_lockfree_merge`, :2113 `value_from_i64_for_lockfree_merge`,
  :2122 `lockfree_delta_to_i64`) → i128 helper + `split_u64_delta_to_i64_chunks` (mirror A.8);
  `current_i64_for_lockfree_merge` drops the `i64::try_from(value)` reject (:2103) — read full u64 via
  helper.
- `atomic_ops.rs`: `increment`/`increment_via_value_cas`/`fetch_add` → `Result<V>` + helper (mirror D);
  `try_increment_impl_no_wal` (:194, recovery replay applier) → route the owned read+write through the
  helper (`owned_get`→`counter_value_to_i128::<V>`, arith i128, `i128_to_counter_value::<V>`); it can
  keep returning `Result<i64>` internally (recovery) via `counter_return_i64`, OR return `Result<V>` —
  pick whichever compiles cleanly with its caller (`apply_core_recovered_operation_no_wal`).
- `lockfree_value_route.rs::route_increment` (:79): return `Option<Result<i128>>`; keep the `delta<0
  → None`; `u64::try_from(delta)` (drop `delta as u64` :94); drop `.map(|v| v as i64)` (:100) → return
  the count `as i128`.
- `overlay_write_mode.rs`: char `CounterValue` is already u64; `bound_increment_delta` (:343) keep;
  any `increment_publish_inner` mirror — route through helper, no truncating casts.

### F. `src/persistent_artrie_core/overlay/flip.rs`
`counter_value_from_i64(v: i64) -> Option<Self::CounterValue>` (:1119): the reconcile carries an
absolute i64; re-encode via the helper: `counter_codec::i128_to_counter_value::<Self::CounterValue>(
v as i128)` (drop the bare `bincode_compat::deserialize::<Self::CounterValue>` :1121). The callers
(:1039,:1073) pass an i64 — fine (i64 widens to i128). The `overlay_counter_get`/`overlay_route_get_value`
(:625) absent-None split is UNCHANGED.

### G. char `mutation_core.rs` / `document_tx.rs` + byte `mutation_core.rs` / `document_tx.rs`
- `i64_from_value(_lossy)` (byte mutation_core:660, byte document_tx:327, char document_tx:565):
  replace serialize→`i64::from_le_bytes` (:664,:331,:569) with `counter_value_to_i128::<V>(value)`
  → return i128 (rename to `*_to_i128`) OR keep i64 return via `counter_return_i64(..)` if the caller
  needs i64. Trace the caller; prefer i128 end-to-end.
- `value_from_i64(byte mutation_core:670)`, `value_from_recovered_i64(char mutation_core:406)`,
  `value_from_i64_checked(byte document_tx:337, char document_tx:575)`: replace the
  serialize(&i64)→deserialize::<V> body with `counter_codec::i128_to_counter_value::<V>(value as i128)`
  (or accept i128). `recompute_recovered_increment` (byte mutation_core:652): do the read+add in i128.

### H. `src/serialization/bincode_compat.rs`
Fix the module doc (:10-13): `bincode::config::legacy()` = **fixint little-endian** (NOT standard()/
varint). This is load-bearing for the counter leaf being 8 fixed LE bytes.

### I. byte incidental src refs (artrie_trait.rs, dict_impl.rs, transactions.rs, in-src test mods
`overlay_correspondence_tests.rs`, `overlay_routing_tests.rs`): compiler-driven. Where a ref is the
byte COUNTER monomorph, migrate `<i64>`→`<u64>` (and the in-src counter tests' expected types). Where
it's an incidental value type, leave it. Let `cargo check` flag each; classify by whether it feeds the
counter path.

---

## TESTS to ADD
1. `tests/persistent_counter_u64_above_i64max_correspondence.rs` (NEW, real-disk scratch under
   `target/test-tmp/`, NEVER tmpfs): byte+char `<u64>` trie → increment a key PAST i64::MAX (e.g.
   start at i64::MAX-2, +10 across calls), checkpoint, reopen → `get_value`/`get_lockfree` returns the
   exact u64 (> i64::MAX), NOT a wrapped/negative value; then DECREMENT back across the boundary →
   exact; then a u64-overflow (push to u64::MAX, +1) → graceful Err (not silent wrap); then a
   below-zero decrement → Err. This is the make-or-break data-loss proof.
2. Adjust the in-file c4 test (A.9) to the u64 domain.

## VERIFICATION (all tee'd to docs/benchmarks/counter-u64-*.txt; run ONCE)
1. `cargo check --features persistent-artrie` AND `--no-default-features` (feature-off) — 0 errors.
2. The v6 gate grep (see above) returns ZERO outside counter_codec.rs. Tee the grep output as proof.
3. `cargo nextest run --features persistent-artrie --no-fail-fast` — full suite green (baseline was
   ~2474; expect ≥ that, +the new test). Tee FULL output (not just tail).
4. Sibling build-check (READ-ONLY, no edits): `cargo check` in ../libgrammstein and
   ../liblevenshtein-rust — stay green (they use the public API; libgrammstein's `<i64>` char counter
   now returns `Result<i64>` via Result<V>, unchanged).
5. `scripts/verify-formal-correspondence.sh` exit 0 (unsafe-inventory unchanged — this adds NO unsafe).
6. `rustfmt` clean.

## NON-NEGOTIABLE constraints (CLAUDE.md)
- `.expect("…")` not `unwrap()`; preallocate; pattern-match over predicates where it helps.
- Disk tests: real-disk scratch under `target/test-tmp/`, NEVER /tmp/tmpfs/tempdir.
- Do NOT delete code to "disable" it (comment with reason). Do NOT git-reset/stash. No new `unsafe`.
- Prefer pgmcp (`mcp__pgmcp__*`) for search; fall back to `rg`/`fd` if `curl -m 0.3
  http://localhost:3100/health` ≠ 200.
- If a site is genuinely ambiguous, apply the helper pattern + leave a clear comment; do NOT skip it
  and do NOT leave a TODO. Implement EVERYTHING — no deferrals.
