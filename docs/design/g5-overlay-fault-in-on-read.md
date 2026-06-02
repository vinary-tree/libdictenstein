# Design: Fault-In-On-Read/Write for the Lock-Free Char-ARTrie Overlay

**Repo:** `/home/dylon/Workspace/f1r3fly.io/libdictenstein` · 2026-06-02 · **Scope:** the load+deserialize+CAS-install
primitive that turns `Child::OnDisk(SwizzledPtr)` back into `Child::InMem(Arc<OverlayNode>)` on the overlay READ
AND WRITE paths, so any evicted node can later be read/written correctly. **ZERO new `unsafe`.** Reversible,
gated, green-gated. Persisted from the Plan-agent design. **The flip (production routing) remains a separate,
later, owner-gated commit.**

## (1) Feasibility + the OnDisk short-circuit gap (code-cited)
**FEASIBLE; the load+deserialize half ALREADY EXISTS (reused, not built).**
**Read gap (terms under an evicted prefix reported ABSENT):** `find_in_lockfree_trie` (`lockfree_cas.rs:459-468`)
+ `find_leaf_recursive` (`:534-538`) use `as_in_mem()`→`None` on OnDisk ⇒ `contains_lockfree` false / `get_lockfree`
None / `try_increment_cas` reads `cur=0` (silent counter reset).
**Write gap (writes mis-reported/lost):** `build_path_recursive` (`:325-347`) OnDisk→`Err(())`→`AlreadyExists` ⇒
`insert_cas`/`insert_cas_durable` of a NEW term under an evicted prefix returns false/`Ok(false)` + not cached ⇒
`merge_lockfree_to_persistent` never persists it = **silently dropped acknowledged write (data-loss-critical)**.
`build_value_path_recursive` OnDisk→`None`→`try_increment_cas` treats as `Conflict`→**spins forever** (pre-existing
latent liveness bug). Notes confirm fault-in deferred to the flip (`lockfree_cas.rs:1186-1189`, `:327-330`); the
eviction driver's cold-only `faultin_count==0` SF5 gate exists precisely because of this — this design removes the
restriction.
**Why feasible (reuse):** `load_char_node_from_disk_lazy` (`disk_io.rs:296-379`): `SwizzledPtr`→`disk_location()`→
`arena_id`→`ArenaSlot`→`arena_manager.read().read(slot)`→`deserialize_char_node_v2` → owned `CharTrieNodeInner<V>`
with children as OnDisk SwizzledPtrs (single-level lazy — exactly the overlay granularity). `DeserializationContext`
reconstructed from the node's own slot (`:329`), no parent context. Owned install-race pattern: `resolve_swizzled_ptr`
(`disk_io.rs:857-930`) — load, try-install, loser drops + re-reads.

## (2) Load + deserialize (disk → OverlayNode)
**REUSE:** `load_char_node_from_disk_lazy` (the production/recovery-tested decoder — do NOT hand-roll a byte reader).
**BUILD (the one new deserialize component): `inner_to_overlay`** — the inverse of `overlay_to_inner`
(`persist.rs:1143-1180`), for ONE node (children stay OnDisk):
```rust
fn inner_to_overlay<V: DictionaryValue>(inner: &CharTrieNodeInner<V>) -> PersistentCharNode<V> {
    let mut node = PersistentCharNode::<V>::new(); // or with_prefix(inner.node.prefix()) if non-empty
    if inner.is_final() { node = node.as_final(); }            // overlay/node.rs:809
    if let Some(v) = inner.value.clone() { node = node.with_value(v); }   // :821
    for (key, ptr) in inner.node.iter_children() { if !ptr.is_null() {
        node = node.with_child(key, Child::OnDisk(ptr.clone())); } }      // :765
    node
}
```
Mirror of `overlay_to_inner`'s `Child::OnDisk` arm reversed; non-recursive (lazy). **Combined primitive:**
```rust
fn load_overlay_node_from_disk(&self, disk_ptr: &SwizzledPtr) -> Result<Arc<PersistentCharNode<V>>> {
    let bm = self.buffer_manager.as_ref().ok_or(/*internal*/)?;
    let inner = self.load_char_node_from_disk_lazy(bm, disk_ptr)?;   // disk_io.rs:296 reused
    Ok(Arc::new(inner_to_overlay::<V>(&inner)))
}
```
**Round-trip equivalence (NoLostWrite half):** bytes at `disk_ptr` were written by `serialize_char_node_to_disk`
(`persist.rs:902`) from `overlay_to_inner(n)`; `load_char_node_from_disk_lazy` is its proven inverse decoder;
`inner_to_overlay` is the structural inverse builder ⇒ `load(serialize(overlay_to_inner(n))) ≡ n` for
finality/value/child-set. Checked byte-for-byte by the Phase-2 unit test + OE5. **Layering preserved** (all on
`impl PersistentARTrieChar<V,S>` in the char layer; consumes the generic node's public API only).

## (3) Read-path fault-in (CAS-install; idempotent; loser-safe)
Reads are `&self`; fault-in republishes a new root (path-copied spine splicing the faulted node InMem) via the
loser-safe root CAS, rebased from the published root each attempt (mirrors `resolve_swizzled_ptr` settle-and-reread,
arc-swap instead of swizzle).
```rust
fn find_leaf_faulting(&self, root_slot: &AtomicNodePtr<V>, chars: &[u32], max_faultin_retries: usize)
    -> Result<Option<Arc<PersistentCharNode<V>>>>
```
Per attempt: `enter_read()`; `old_root = root_slot.load()`; walk top-down collecting spine; at each edge:
`None`⇒absent (`Ok(None)`); InMem⇒descend; **OnDisk⇒fault**: `loaded = load_overlay_node_from_disk(ptr)?`, rebuild
spine bottom-up splicing `Child::InMem(loaded)` (exactly `evict_overlay_node_at_path`'s shape, `mod.rs:1499-1518`,
but InMem not OnDisk), `root_slot.compare_exchange(&old_root, new_root)` → Ok: rebase+continue; Err: drop `loaded`
(refcount) + rebase. Terminal: leaf-by-`is_final` (as `find_leaf_recursive:526-531`). On retry exhaustion: one
read-only walk of the fresh root (still-OnDisk reads absent — liveness-only, durable, later retry).
**Idempotent:** two faulters each load their own Arc; exactly one CAS wins (`Arc::ptr_eq`, `atomic_ptr.rs:141-144`);
loser drops + re-reads the now-InMem child. **Loser-safe:** CAS vs `old_root` ⇒ a concurrent insert that published
makes our CAS fail ⇒ we rebase, never clobber. **Vs re-eviction:** single root slot arbitrates; every published
root has the node InMem XOR OnDisk, never both. **Wiring (`&self`):** `contains_lockfree`/`get_lockfree`/
`try_increment_cas`-read route through it; DashMap fast-path unchanged; disk I/O only when an OnDisk slot is hit
(all-InMem walk byte-identical + one cheap discriminant/hop).

## (4) Write-path fault-in (data-loss-critical half)
ONE site (`build_path_recursive` OnDisk arm, `lockfree_cas.rs:334-347`): replace `Err(())` with fault-then-descend:
```rust
c if c.as_on_disk().map_or(false,|p| !p.is_null()) => {
    let loaded = self.load_overlay_node_from_disk(c.as_on_disk().unwrap())?;
    let (new_child, leaf) = self.build_path_recursive(&loaded, chars, depth+1)?;
    Ok((Arc::new(node.with_child(key, Child::InMem(new_child))), leaf))
}
```
**Correct, not a lost update:** `build_path_recursive` builds a NEW spine; splicing InMem(faulted+extended) at `key`
is identical in shape to an in-mem child ⇒ **the single root CAS in `insert_lockfree_recursive` (`:407-419`) remains
the sole arbiter.** CAS wins⇒faulted-in + new term, durable (Order-A WAL before CAS, `insert_cas_durable:214`),
visible. CAS loses (writer/evictor)⇒`Conflict`⇒existing retry from fresh root, dropped spine (no leak/clobber); on
retry the slot may be InMem (racer faulted) ⇒ descend without reload. **The silent-drop bug is eliminated.**
Counter write `build_value_path_recursive`: same edit (fault then descend) — fixes the infinite-spin; its read step
(`:657`) routes through `find_leaf_faulting` so `cur` is the faulted value not 0. **Signature impact:** thread the
buffer-manager I/O error out — add `LockfreeInsertResult::IoError(e)` (smaller blast radius than widening the
recursive `Err`); `insert_cas_durable`→`Err(e)`, `insert_cas`→bounded retry/false.

## (5) No-lost-write + no-UAF + evict‖fault-in‖writer race
**No-lost-write PRESERVED — fault-in is read-only wrt durable state:** writes nothing to disk, no watermark advance,
no WAL truncate ⇒ `LockFreeDurableCheckpoint.tla` `NoLostWrite` unaffected; faulted node == durable image (§2) ⇒
can't manufacture/drop a term; write-path still commits via Order-A (WAL before CAS) regardless of faulting.
**No-UAF (ZERO new unsafe):** only `AtomicNodePtr::{load,compare_exchange}` (arc-swap hazard-protected), pure node
copies, Arc clone/drop, and the EXISTING lazy loader (its unsafe pre-existing, called through a safe `&self`
boundary). Losing-CAS Arc dropped by refcount; pinned-snapshot readers keep old structure alive. New unsafe in
changed regions = 0 ⇒ inventory gate exit-0.
**Three-way race:** all three = path-copy + single root CAS on `lockfree_root` (a total order of versions; every CAS
loser-safe by `Arc::ptr_eq`). Faulter‖Writer: one wins, other rebases (no lost write/double-link). Faulter‖Evictor
on `n`: CAS arbitrates; `n` InMem-XOR-OnDisk at every root (`LinkedAndOnDiskDisjoint`); loser re-faults/re-evicts
(idempotent, thrash = liveness only). Safety depends only on the single-arbiter CAS (unchanged).

## (6) REVERSIBILITY VERDICT (the owner's go/no-go key)
**Fault-in CAN be added as a reversible, independently-testable primitive that does NOT flip production. There is a
clean reversible step before the flip; the flip is NOT forced to be next.** Evidence:
1. The methods it modifies (`insert_cas`/`insert_cas_durable`/`contains_lockfree`/`get_lockfree`/`try_increment_cas`)
   are the OVERLAY API, reached only after `enable_lockfree()`; production still routes through the owned tree +
   `checkpoint()`. Fault-in changes overlay behavior only — `checkpoint()`/owned `self.root`/default path untouched.
2. Testable without the flip: the already-merged reversible eviction driver (`evict_overlay_node_at_path`/
   `evict_overlay_nodes`) produces real OnDisk overlay nodes under test ⇒ insert→checkpoint→evict→read/write-through
   →assert-restored is a closed loop within the existing reversible surface (OE5-OE9).
3. Honest scope: more reversible than the flip, less than a no-op (it changes the overlay read/write RESULT — but
   that change is strictly a correctness FIX, reachable only via the overlay API, and mechanically revertible per
   phase §8).
4. NOT "only meaningful post-flip": the eviction work already created the OnDisk-under-test condition that makes
   fault-in exercisable + necessary-to-prove in isolation. The flip CONSUMES this primitive; it does not gate it.
**Consequence:** land fault-in as the next reversible commit; then the flip delta is ONLY the routing switch
(overlay-as-default + overlay-capturing `checkpoint()` + watermark WAL-rotate), resting on a proven eviction+fault-in
round-trip. Re-assess the flip with both green + the TLA round-trip checked.

## (7) Formal plan
**Extend `OverlayEvictionCas.tla` (don't fork):** add `FaultInCas(n)` (enabled iff `n∈onDisk∩durable`: `root'=root+1`,
`linkedInMem'∪{n}`, `onDisk'\{n}`; lose=stutter) — dual of `EvictCasSucceed`; new var `durable` (=cold at Init);
new invariant `FaultEqualsDurable == \A n∈linkedInMem: (n∈cold => n∈durable)`; strengthen `NoLostAck` with writer
terms through faulted prefixes. **Decisive relaxation:** with `FaultInCas` present, DROP `EvictTouchesOnlyCold` from
safety, ADD `ReadNeverMissesCommitted == \A n∈(acked∪cold): (Reachable(n) \/ n∈durable)` (eviction may touch ANY
node because fault-in recovers it). **Negative control** (`_Unsafe.cfg`, repurpose to `FAULT_IN_ENABLED=FALSE` +
unrestricted evict): TLC must VIOLATE `ReadNeverMissesCommitted` (an acked node evicted with no fault-in is
permanently unreachable) ⇒ proves fault-in REQUIRED once eviction unrestricted. CONSTANTS `Nodes={n1,n2,n3}`,
`Lsns={1,2}`, `live={n1}`, `cold=durable={n2,n3}`, `CHECK_DEADLOCK FALSE`. Register in
`scripts/verify-formal-correspondence.sh` SANY/RUN_TLC/_Unsafe (already present; cfg semantics change). Durability
specs unchanged.
**Rust tests** (`#[cfg(test)] mod overlay_faultin_correspondence`, real-disk `target/test-tmp`):
- **OE5 `evict_then_read_faults_in_exact_value`** (read headline): insert→checkpoint→evict→`contains/get_lockfree`
  returns EXACT pre-evict value (fails without fix).
- **OE6 `evict_then_write_under_evicted_prefix_reopen_loses_nothing`** (write, data-loss-critical): insert `ab`→
  checkpoint→evict `ab`→`insert_cas_durable("abcd")` returns `Ok(true)`→reopen→both present.
- **OE7 `concurrent_reader_writer_evictor_faulter_no_uaf_and_complete`** (three-way race, under sanitizers): no
  panic/UAF; every acked term present; no spurious-absent for a committed term.
- **OE8 `evict_faultin_evict_thrash_terminates`** (liveness): tight evict-then-read loop terminates within
  `max_faultin_retries` (regression-guards the counter infinite-spin).
- **OE9 `faultin_double_install_one_wins`** (loom, extend `persistent_lockfree_overlay_loom.rs`): 2 faulters + 1
  writer, one install CAS wins, loser drops, final InMem+correct.

## (8) Phased migration (each GREEN: nextest ≥2480 + verify-formal-correspondence exit 0; 0 new unsafe; real-disk only)
1. **TLA first:** extend `OverlayEvictionCas.tla` + repurpose `_Unsafe.cfg`. Gate: SANY ok, RUN_TLC holds new+retained
   invariants, `_Unsafe` FAILS, exit 0. Rollback: revert spec/cfg.
2. **Load+converter (`cfg(any(test, bench-internals))`):** `inner_to_overlay` + `load_overlay_node_from_disk` + a
   round-trip unit test (`load(serialize(overlay_to_inner(n)))≡n`). Gate: nextest +1, verify exit 0. Rollback: delete.
3. **Read-path:** `find_leaf_faulting`; route `contains_lockfree`/`get_lockfree`/`try_increment_cas`-read; OE5/OE8/OE9.
   Gate: nextest +3 (≥2483), verify exit 0. Rollback: restore `as_in_mem()?` walks; delete helper+tests.
4. **Write-path:** patch `build_path_recursive`+`build_value_path_recursive` OnDisk arm; add `LockfreeInsertResult::
   IoError`; thread through insert/increment; OE6/OE7. Gate: nextest +2, verify exit 0; OE6 reopen-loses-nothing; OE7
   sanitizers no-UAF+complete. Rollback: restore `Err(())`/`None` arms; remove variant; delete tests.
5. **(Optional, pre-flip) promote gate test→bench-internals** so `--evict-real` can assert correct fault-in counts
   (drop the SF5 `faultin==0` abort). Gate: release benches build + default nextest unaffected + verify exit 0.
   Rollback: narrow cfg. **Does NOT flip production.**
Production flip = separate later owner-gated commit on the proven eviction+fault-in round-trip.

## (9) Honest risks
1. **Write-path fault-in correctness (highest stakes — lost/shadowed write):** mitigated — one arm preserving the
   single-root-CAS arbiter (no new commit point); OE6+OE7+loom#1+TLA `NoLostAck`/`ReadNeverMissesCommitted`
   triangulate. Residual: the `IoError` channel touches insert internals (review item). **If it couldn't preserve the
   arbiter it would force the flip; it does, so it doesn't.**
2. **Buffer-manager read cost on hot read path:** fault-in does disk I/O only when an OnDisk slot is hit; loaded node
   installed InMem (cost once per node/eviction-epoch); buffer pool caches the page. Residual: evict/read interleave
   may lose the install CAS + re-pay deserialize (bounded by max_faultin_retries, measured by OE8). The flip's read
   path accepts this regardless.
3. **Fault-in‖evict thrash (liveness not safety):** bounded retries; production evictor is coldness-driven (a
   hot-read node isn't a top candidate); OE8 guards termination. Residual: adversarial schedule degrades throughput.
4. **Counter-write infinite-spin is a PRE-EXISTING latent bug this FIXES (not introduces):** flagged so the reviewer
   sees it's a fix; OE8 bounds it.
5. **Deserialize-format coupling:** reuse the production lazy loader (single source of truth); round-trip unit test +
   OE3-analogue pin `load(serialize(overlay_to_inner(n)))≡n` and fail loudly on drift; valued-node OE5 covers the
   bincode `V` path.
6. **Monomorphization:** same per-(V,S) as the existing eviction driver + lazy loader; negligible.
7. **Lazy-vs-eager granularity:** one node per fault (N fetches for N-deep cold path) — matches the owned lazy
   discipline; buffer manager amortizes co-located arena pages; eager-subtree fault is a future opt, out of scope.
8. **Maintenance coupling:** `find_leaf_faulting` mirrors `evict_overlay_node_at_path`; `inner_to_overlay` mirrors
   `overlay_to_inner` — same crate, cross-ref doc-comments; flagged.

**Bottom line:** write-path fault-in CAN be done WITHOUT the flip — preserves the single-root-CAS arbiter, zero
unsafe, exercisable+provable on the already-merged reversible eviction surface. Clean reversible step before the
flip; the flip is not forced next. Load+deserialize already exists (reused); only new deserialize code is the small
`inner_to_overlay`; round-trip equivalence makes "faulted node equals durable bytes" checkable.

### Critical files
- `src/persistent_artrie_char/lockfree_cas.rs` (read gap :459-468/:534-538; write gap :325-347 + :675-684; add
  `find_leaf_faulting`; wire contains/get/increment; OE5-OE9)
- `src/persistent_artrie_char/disk_io.rs` (REUSE `load_char_node_from_disk_lazy`:296-379; install-race pattern
  `resolve_swizzled_ptr`:857-930)
- `src/persistent_artrie_char/persist.rs` (add `inner_to_overlay`+`load_overlay_node_from_disk` beside
  `overlay_to_inner`:1143-1180; format owner `serialize_char_node_to_disk`:902)
- `src/persistent_artrie_char/mod.rs` (mirror `evict_overlay_node_at_path`:1439-1527; `OverlayEvictOutcome`:1391)
- `formal-verification/tla+/OverlayEvictionCas.tla` (+`_Unsafe.cfg`; extend with FaultInCas/durable/FaultEqualsDurable/
  ReadNeverMissesCommitted; registered in `scripts/verify-formal-correspondence.sh`:250/300/325)
