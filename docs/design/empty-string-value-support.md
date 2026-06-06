# Empty-String Key (`""`) Value Support — Across ALL ARTrie Implementations

**Status:** ✅ **IMPLEMENTED & GATED** (owner-approved 2026-06-06). Designed + 4-round-red-teamed to
convergence (V5), then implemented phase-by-phase, the full suite + formal-correspondence + 0-new-unsafe
green at EACH phase. The empty term "" is now a FULL first-class key carrying a value (membership /
counter / arbitrary-V) on byte, char, and vocab.

**Per-phase commit ledger:**
| Phase | What | Commit |
|---|---|---|
| P0 | byte root-value serialize/load codec + H7 (bucket→ART split) | `43cc671` |
| P1 | byte overlay checkpoint carries root value (H2) | `8bb44f3` |
| P2 | byte coupled write+read+reestablish flip (H3+H4+H5) + shared publishers + loom gate | `e9a7384` |
| P3 | char write-guard reroutes (H4) + char matrix (char = just-H4) | `127f1b0` |
| P4 | vocab reopen + compaction + bucket→ART-split tests | `864b395` |
| P5 | stale-doc cleanup + final full gate | (this commit) |

Final gate: `cargo nextest --features persistent-artrie` all green incl. the loom data-loss gate +
the 107s no-lost-write durable loom; `verify-formal-correspondence.sh` exit 0; 0 new unsafe.

**Design provenance (pre-implementation):** CONVERGED at V5 — confirmed by a clean terminating round
(Round 4): TWO independent adversarial passes BOTH returned CONVERGED with zero blocker/data-loss/high
holes, every cited file:line verified exact against live code.
**Provenance:** V1 (Plan) → red-team (3 blockers) → V2 (trait-unified, all-impls) → red-team Round 1
(2 data-loss + write-time-drop + clear-owned + proof-gap) → V3 → red-team Round 2 (keystone
orthogonal-field-preservation VERIFIED sound; 2 narrow data-loss + 2 test-reclassifications) → V4 →
red-team Round 3 (write/concurrency CONVERGED; one missed char test) → V5 → **red-team Round 4 (dual
CONVERGED — clean terminating round; 2 non-material nits folded in: H1-load construction site disk_load.rs:788,
eviction root-safety note)** → **this doc (CONVERGED, implementation-ready, owner-gated).**

**V5 deltas (from Round-3):** §5.4 adds the missed char `test_insert_cas_empty_term` (dict_impl_char.rs:3042)
to the UPDATE inventory (else P3 RED); §1.1 maps the `claim_commit_seq()`/`note_cas_retry()` placeholders to
the existing `commit_seq`/`cas_retries` fields + pins the `u64↔i64` increment seam (non-material precision).
**V4 deltas (from Round-2):** §1.1 `overlay_increment_root` returns `(count, generation)` + claims
`commit_seq` (closes the unranked-drop data-loss); `overlay_publish_root_value` uses always-publish
`changed=|_,_|true` (drops the unavailable `V: PartialEq`); H1-load enumerated as 3 edit sites (iterative
loader); §5.4 reclassifies `m3_empty_term_*` as UPDATE + adds the 2nd `m2a` drop-assertion; H8 downgraded
to test-only; §8 keystone CLOSED-affirmative; §5.1 loom gate names its two model fns + pins the negative control.

**Goal:** `""` becomes a full first-class key carrying a value (membership / counter / arbitrary-`V`)
that round-trips write → durable WAL → checkpoint → reopen (checkpoint-reopen AND pure-WAL-replay) →
read, with NO data loss, on **byte**, **char**, and **vocab**, lock-freedom & Order-A preserved, the
empty-term decision unified in the existing overlay trait family (DRY).

---

## 0. The keystone invariant (why V2 had two data-loss holes)

**`""` is the unit-sequence `&[]`, which navigates to the overlay ROOT.** Reads already handle this:
`find_leaf_recursive(root, b"", 0)` / `find_in_lockfree_trie` hit `depth >= term.len()` at depth 0 and
read `root.is_final()` / `root.get_value()` (verified lockfree_cas.rs:447-448, 528-533). So the overlay
root CAN represent membership and value for `""` — the stale doc comments claiming otherwise
(atomic_ops.rs:116-147) are WRONG.

**The one thing that makes the root special — and broke V2:** the root is the *unique* node that a
concurrent non-empty insert **copies** rather than **shares**. `OverlayNode::with_child` (node.rs:765-774)
snapshots `flags: AtomicU8::new(self.flags.load())` into a fresh node. For a non-root node `N`, a
concurrent insert path-copies `N`'s *parent* and `Arc::clone`-shares `N` itself, so an in-place
`N.try_set_final()` is preserved. For the **root**, a concurrent `insert("a")` path-copies the root
itself (with_child, line 359) and CAS-publishes the copy — **discarding any in-place `root.try_set_final()`**.

Therefore:

> **INVARIANT (the fix):** every empty-term mutation (membership-insert, value-insert/upsert,
> increment, remove) MUST publish a **fresh root** node (`root.as_final()` / `.with_value(v)` /
> `.as_non_final()`) via the root `compare_exchange` — the SAME single-arbiter CAS every non-empty
> write already uses — and MUST NOT use in-place `try_set_final`/`try_set_value` on the live root, and
> MUST NOT route through the guarded leaf primitives (`insert_cas`/`increment_cas`, which no-op on `""`).

This single discipline fixes all three Round-1 material holes: the lost-update (fresh-root CAS
linearizes against concurrent root path-copies), the dropped counter (the fresh root carries the
value), and the `clear_owned()` erasure (the value lives on the overlay root, not owned).

### Why V2's two mechanisms were both wrong (verified)
- V2's `overlay_set_root_final` = in-place `root.try_set_final()` → **lost-update** (above).
- V2's reestablish publisher = `insert_cas(&[])`/`increment_cas(&[])` → hits the empty guards
  (lockfree_cas.rs:254/1017) → **no-op**; and even with the guard removed, `insert_cas("")`'s depth-0
  path returns the existing root + in-place `try_set_final` (build_path_recursive:330-342, insert_cas:266)
  → the SAME lost-update.

---

## 1. THE TRAIT DESIGN (centerpiece, revised)

Fold into the EXISTING overlay trait family in `src/persistent_artrie_core/overlay/`; do NOT add a
standalone trait (it would duplicate the `K::Unit`/`CounterValue`/owned-reader/publisher plumbing
`LockFreeOverlay` already carries, recreating the divergence that caused V2's guard miscount).

### 1.1 New shared DEFAULT publishers (the fresh-root-CAS discipline, written ONCE)

Add to `LockFreeOverlay<K,V,S>` (flip.rs) three default methods + a shared CAS helper. They are the
*sole* empty-term publication path; every live-write guard and every reestablish fold routes here.

```rust
/// Publish a fresh ROOT via compare_exchange, transformed by `f` (which clones the
/// loaded root and returns a new root Arc). Bounded-retry lock-free loop — the SAME
/// arbiter as every non-empty write. Returns Ok(true) iff THIS call changed the root
/// state (per `changed`). NEVER mutates the live root in place.
fn publish_root_cas(
    &self,
    f: impl Fn(&Arc<OverlayNode<K,V>>) -> Arc<OverlayNode<K,V>>,
    changed: impl Fn(&OverlayNode<K,V>, &OverlayNode<K,V>) -> bool, // (old,new)->did-state-change
) -> Result<bool> {
    let root_ptr = self.lockfree_root().ok_or(/* enable_lockfree */)?;
    loop {
        let old = match root_ptr.load() { Some(r) => r, None => { let _ = root_ptr.try_init(Arc::new(OverlayNode::new())); continue; } };
        let new = f(&old);
        if !changed(&old, &new) { return Ok(false); }     // idempotent no-op (e.g. already final)
        match root_ptr.compare_exchange(&old, new) {
            Ok(_) => return Ok(true),
            Err(_) => { self.note_cas_retry(); continue; } // another writer advanced the root; rebase
        }
    }
}

/// Membership "": publish root.as_final() iff not already final.
fn overlay_publish_root_membership(&self) -> Result<bool> {
    self.publish_root_cas(|r| Arc::new(r.as_final()), |o,_| !o.is_final())
}
/// Value/upsert "": publish root.as_final().with_value(v). ALWAYS publishes (LWW
/// upsert overwrites unconditionally). NOTE: the `changed` predicate is `|_,_| true`
/// — we do NOT compare `o.get_value() != n.get_value()` because `DictionaryValue` does
/// NOT bound `PartialEq` (value.rs:71-73). A redundant CAS on an identical value is
/// correctness-neutral (Round-2 C5c fix). The insert-vs-update RETURN flag is computed
/// by the caller's present-check, NOT by this publisher.
fn overlay_publish_root_value(&self, v: V) -> Result<()> {
    self.publish_root_cas(move |r| Arc::new(r.as_final().with_value(v.clone())), |_,_| true)
        .map(|_| ())
}
/// Remove "": publish root.as_non_final() iff currently final (un-final + drop value).
fn overlay_unpublish_root(&self) -> Result<bool> {
    self.publish_root_cas(|r| Arc::new(r.as_non_final()), |o,_| o.is_final())
}
```

`lockfree_root()`, `compare_exchange`/`try_init`/`load` (AtomicNodePtr), `as_final`/`with_value`/
`as_non_final` (node.rs:809/866/850 — all path-copy + version+1), `get_value`/`is_final` all already
exist and are generic over `K: KeyEncoding, V: DictionaryValue`. The default body monomorphizes for
`OverlayNode<ByteKey,_>` and `OverlayNode<CharKey,_>` with no new bound (the node tests already
instantiate both, node.rs:951-954).

For the **counter increment** (read-modify-write on the root), use a CAS loop that reads the loaded
root's value and writes `old_count + delta` (bounded). **It MUST return BOTH the new count AND the
winning commit generation** — the durable template (`try_increment_cas_durable_default`,
durable_write.rs:220-271) is Order-A: step 2 calls `increment_publish_inner -> (count, generation)`
(durable_write.rs:146-150/265), step 3 `commit_rank_and_mark(lsn, key_bytes, generation)` (:269). If the
empty-term increment returns only the count and claims no generation, the durable `BatchIncrement{""}`
record is left **UNRANKED**, and Overlay-regime `reconcile_lww` **DROPS unranked records on reopen**
(recovery.rs:265-266, 1631) → `increment("")` lost across restart (Round-2 C2c data-loss). So mirror the
existing non-durable `try_increment_cas_inner` (lockfree_cas.rs:1008-1054): claim `commit_seq` at the
loop top and return it as the generation of the winning CAS.

```rust
// (count, generation) — matches `increment_publish_inner`'s seam contract exactly.
fn overlay_increment_root(&self, delta: Self::CounterValue) -> Result<(Self::CounterValue, u64)> {
    let root_ptr = self.lockfree_root().ok_or(/* enable_lockfree */)?;
    loop {
        let generation = self.claim_commit_seq();              // loop-top, re-claimed per iter (mirrors :1035)
        let old = match root_ptr.load() { Some(r)=>r, None=>{ let _=root_ptr.try_init(Arc::new(OverlayNode::new())); continue; } };
        let cur = self.counter_of(old.get_value());            // root.get_value()|0 ; preserved by with_child (C1a)
        let new = self.bound_increment(cur, delta)?;           // i64/u64 seam + overflow bound
        let new_root = Arc::new(old.as_final().with_value(self.counter_to_v(new)));
        match root_ptr.compare_exchange(&old, new_root) {
            Ok(_)  => return Ok((new, generation)),
            Err(_) => { self.note_cas_retry(); continue; }     // rebase: next iter re-reads the NEW root's count
        }
    }
}
```
The byte/char `counter_of`/`counter_to_v`/`bound_increment` are the existing per-variant seams
(overlay_write_mode.rs:645-668 / lockfree_cas.rs:1509); `claim_commit_seq()` maps to the existing field
`self.commit_seq.fetch_add(1, AcqRel) + 1` (lockfree_cas.rs:1035), and `note_cas_retry()` to the existing
`self.cas_retries.fetch_add(1, Relaxed)` (observability only; correctness-neutral, may be omitted). The
durable caller threads the returned `generation` into `commit_rank_and_mark` (step 3), so the empty-term
increment is RANKED like every other durable write — closing the unranked-drop.
**Implementation note (Round-3):** keep the empty-term RMW at the existing inner's `u64`/`&[u8]` domain
(`try_increment_cas_inner` returns `Result<(u64,u64)>`) and convert to `Self::CounterValue` (`i64` byte) at
the SAME `increment_publish_inner` seam (`delta as u64` in / `new_val as i64` out, overlay_write_mode.rs:647-648)
— do NOT literally substitute the generic signature inside the inner.

### 1.2 Per-variant SEAMS shrink to almost nothing
The only per-variant code is the `V ↔ Self::CounterValue` conversion (byte `i64`, char `u64`) inside
`overlay_publish_root_value`/`overlay_increment_root` — already an existing seam
(`bound_increment_delta`, the counter monomorph). The concrete root type is the shared
`OverlayNode<K,V>` (post-G4), so the publishers themselves are **shared defaults**, not seams. This is
strictly more DRY than V2 (which had per-variant publisher bodies).

### 1.3 Reads need NO change (verified): `overlay_route_get_value(&[])` (flip.rs:518) and
`overlay_contains`/`overlay_counter_get` already walk to the root. Byte's ONE read exception
(`atomic_ops.rs:128` `&& !term.is_empty()`) is removed (H5); char has none.

---

## 2. ALL-IMPLEMENTATIONS SCOPE

| Impl | Path | `V` | Empty-term home | Production change set |
|---|---|---|---|---|
| **Byte** | `src/persistent_artrie/` | `()`,`i64` | overlay root | H1, H2, H3, H4 (guards→fresh-root-CAS publishers), H5, H7 |
| **Char** | `src/persistent_artrie_char/` | `()`,`u64` | overlay root | H3 (shared default — free), H4 (guards→publishers). NO H1/H2 (char threads root value already), NO H5 (reader empty-clean) |
| **Vocab** | `src/persistent_vocab_artrie/` | index | vocab root value + reverse index | **NONE** (verified end-to-end correct) + 1 reopen test |
| other ART | — | — | — | none exist (`ARTrie` trait = {byte,char}; vocab separate) |

Vocab confirmed correct end-to-end by Round-1 (insert→root value+reverse-index, save disk_io.rs:266,
load disk_io.rs:130-141, bijection `get_index("")`/`get_term(0)` via `NodeRef(0,0)` round-trip). Test-only.

---

## 3. PER-HOLE FIXES (updated with Round-1 corrections; verify file:line at impl time — lines drift)

### H4 [data-loss] — empty guards → fresh-root-CAS publishers (the corrected core)
Each empty guard is REPLACED by a route to the §1.1 publisher (NOT removed-to-fall-through-existing-logic,
which would hit the in-place `try_set_final` lost-update). Byte (`lockfree_cas.rs`):

| Guard | Method | Replace with |
|---|---|---|
| :254 | `insert_cas` (non-durable membership) | `return self.overlay_publish_root_membership().unwrap_or(false)` (no WAL) |
| :601 | `insert_cas_durable` (durable membership; used by `insert`/`insert_batch`) | Order-A: present-check `root.is_final`; if absent → append WAL `Insert{term:vec![]}`, `overlay_publish_root_membership()`, `commit_rank_and_mark` |
| :699 | `remove_cas_durable` | Order-A: append WAL `Remove{term:vec![]}`, `overlay_unpublish_root()`, commit_rank |
| :1017 | `try_increment_cas_inner` | `overlay_increment_root(delta)` (fresh-root-CAS RMW) |
| :1224 | `insert_cas_with_value_durable` | Order-A: present-check; if absent → WAL `Insert{term:vec![],value}`, `overlay_publish_root_value(v)`, commit_rank |
| :1321 | `upsert_cas_durable` | Order-A: WAL `Upsert{term:vec![],value}`, `overlay_publish_root_value(v)` (always), commit_rank |
| `durable_write.rs:243` | `try_increment_cas_durable_default` (shared) | remove the short-circuit; template flows to `increment_publish_inner`→`overlay_increment_root` |

**Coupling (R1):** `durable_write.rs:243` + `:1017` (char `:1519`) MUST change together — else `increment("")`
appends a durable delta then publishes nothing (durable-but-invisible). With both routed to
`overlay_increment_root`, the append (step 1), the fresh-root-CAS publish (step 2), and commit_rank
(step 3) all target the root in Order-A order.

**DO-NOT-TOUCH (structural recursion base cases, NOT empty-key rejects — verified):** byte `:389`
`create_lockfree_path`, `:848` `create_lockfree_path_final`; char `:1025`. And the `entries.is_empty()`
batch guards (byte `:1428`, char `:2074`).

Char full guard set (verified Round-1): `lockfree_cas.rs:242,403,589,1519,1726,1842` + shared `:243`,
each → the same publishers (char's seam supplies the `u64` conversion).

### H3 [data-loss] — reestablish republishes `""` via the §1.1 publishers (shared default, fixes byte+char)
`reestablish_overlay_membership` (flip.rs:424) and `reestablish_overlay_counter` (flip.rs:455): the
empty-term partition must call the NEW §1.1 publishers DIRECTLY — `overlay_publish_root_membership()` /
`overlay_publish_root_value(v)` — and NOT the existing `overlay_publish_membership(&[])` /
`overlay_publish_counter(&[], v)` seams (overlay_write_mode.rs:518-552), which route through the guarded
`insert_cas`/`increment_cas` and would no-op on `""` (Round-2 nit: this rewiring is the substance of H3 —
the existing `reestablish_overlay_counter` at flip.rs:458-462 calls `overlay_publish_counter(&[], v)`,
which must be REPLACED by `overlay_publish_root_value(v)`). The publish runs BEFORE `clear_owned()`. Because
the §1.1 publishers are fresh-root-CAS and reestablish runs single-threaded during reopen (ctor
flips→reestablishes→returns; mmap_ctor.rs:480-487/598; verified Round-1 C4), the republish is race-free and
survives `clear_owned` (value is on the overlay root). The membership fold (flip.rs:430) currently DROPS
`_has_empty_term` — it MUST now honor it via `overlay_publish_root_membership()` (one shared edit fixes
both variants).

### H1 [data-loss] — root value serialize + load (byte; the owned/recovery/checkpoint image)
- **Serialize:** `overlay_checkpoint.rs::serialize_root` (the LIVE durable serializer for BOTH capture
  arms — `capture_owned_snapshot` + `capture_overlay_snapshot`) destructures `value: _` and calls
  value-less `serialize_node_to_disk`. FIX: bincode the root `Option<V>` (propagate error, NOT `.ok()` —
  H7), call `serialize_node_to_disk_with_value`. Also fix `serialize_impl.rs::persist_to_disk` (vocab/test path).
- **Load (THREE coordinated edits — Round-2 C1 undercount fix; the live arena root flows through the
  ITERATIVE loader, not a single function):**
  1. `disk_load.rs::load_single_art_node_data` (~:618; the root-node loader; ONE caller at ~:783) — add
     `read_node_value(root_record)` (it currently does NOT, unlike `load_single_child_data` ~:695) and
     return the `Option<Vec<u8>>` blob (signature gains a field).
  2. `disk_load.rs::LoadedInfo::RootNode` (~:747) — add a `value: Option<Vec<u8>>` field; populate it at the
     CONSTRUCTION site (~:788, where `LoadedInfo::RootNode{..}` is built — adding the field makes this a
     COMPILER-FORCED edit, so it can't be silently dropped — Round-4 nit), and thread it through the
     iterative `load_art_node_with_children_from_arena_iterative` (~:730) root-extract arm (~:894-916),
     which today returns only `(node, children)` and would otherwise DROP the blob here.
  3. `disk_load.rs::load_root_from_disk_with_arena` (~:448) — replace the hardcoded
     `let root_value: Option<V> = None;` by consuming the threaded blob and `bincode_compat::deserialize`
     into `Option<V>` (propagate error, NOT `.ok()` — H7).
  **Combine with the descriptor's `is_final`** (the root's authoritative finality is `descriptor[1]`, not
  the node-header bit; the value blob is independent of IS_FINAL since `read_node_value` keys only on
  HAS_VALUE byte 7 — back-compat holds). Editing ONLY (1) leaves the value dropped at (2)/(3) → the cycle
  does not close.
- **Dead loader:** `disk_load.rs:load_root_from_disk` (+ its `load_art_node_with_children` subtree) is
  genuinely DEAD (no live caller; verified Round-1). Do NOT spend effort threading it; optionally
  comment-out per CLAUDE.md (with reason). Leave its `None` honest.
- **Back-compat (verified):** value-less root = byte-identical (`append_node_value(buf,None)` returns
  buf unchanged); old file → HAS_VALUE clear → `None`; no descriptor/version bump; trailing blob ignored
  by node-parse-only readers.

### H2 [data-loss] — byte `overlay_root_to_owned` carries the root value (checkpoint capture)
`overlay_checkpoint.rs:overlay_root_to_owned` drops `_value_bytes` (`root_value=None`). FIX: deserialize
into `Option<V>`, pass into BOTH `TrieRoot::ArtNode` arms (children-present + childless-final). Now
*non-vacuous* because H4 populates the overlay root value. (Char's `overlay_to_inner`/`inner_to_overlay`
already thread the root value — no char change.)

### H5 [High] — byte read router flips with the writes (atomic phase)
Remove `&& !term.is_empty()` at `atomic_ops.rs:128` so `get_value("")` reads the overlay root; preserve
the ineligible-`V` fall-through (`None`=ineligible→owned read; `Some(None)`=absent). Update the stale
docs (:116-147). Sequenced AFTER H4 within the same per-variant phase (else acknowledged overlay `""`
write is invisible to `get_value`; `contains`/`get_value` would disagree). Char reader already correct.

### H7 [High — recompute severity] — secondary value drops (owned + RECOVERY-REPLAY path)
- `convert_bucket_to_art` (mutation_core.rs:~340 `value:None`) drops the root value on a root
  Bucket→ART split, even though `bucket_to_art_node` computes `final_value` (transitions.rs:~143). FIX:
  use `result.final_value` — **note the type seam (Round-2 nit):** `final_value` is `Option<Vec<u8>>`
  (serialized bytes) but `TrieRoot::ArtNode.value` is `Option<V>`, so the fix needs a fallible
  `bincode_compat::deserialize` (propagate error, NOT `.ok()`). **Reachable on the WAL-replay recovery
  path** (`insert_impl_no_wal`→`insert_impl_core`→split), not only owned-mode — a production reopen
  correctness fix. (Not reachable for overlay-default *new writes*, which bypass the owned tree.)
- bincode `.ok()` swallowing on the new root-value (de)serialize paths (overlay_children_to_owned:~519
  and the H1 serialize/load) — propagate or `log::warn!`; do not silently drop on a data-loss path.

### H8 [Round-1 C5 → Round-2: NO FIX NEEDED, test-only] — compaction round-trips `""`
Round-2 VERIFIED: `compact()` rejects under `route_overlay()` (compaction_impl.rs:~129-136) so it runs
ONLY in owned mode, and `compaction_snapshot`'s `iter_prefix_with_arena(b"")` DOES enumerate the empty
term — the owned ART arm (arena_iter.rs:~407-414: `if prefix.is_empty() && *is_final { push(Vec::new()) }`)
and the Bucket arm both yield `""`. So once H1/H7 fix the owned-root value, compaction round-trips `""`
with no compaction-code change. **Add a compaction-`""` test** (P4) to pin it; no production fix.

---

## 4. ARCHITECTURE FLOW (root value, byte, both reopen paths)
```
WRITE (overlay default): insert_with_value("",v)/increment("",δ)
  → H4 route: Order-A  1) append+sync WAL Insert/Upsert/BatchIncrement{term:vec![],value}
                       2) overlay_publish_root_value(v)/overlay_increment_root(δ)   [FRESH-ROOT CAS]
                       3) commit_rank_and_mark
READ: get_value("") → (H5) overlay_route_get_value(&[]) → get_lockfree(b"") → root.get_value() == Some(v)
CHECKPOINT: capture_*_snapshot → overlay_root_to_owned (H2 carries value) → serialize_root (H1 value blob on disk)
REOPEN-checkpoint: load_root_from_disk_with_arena (H1 read_node_value → TrieRoot.value)
  → flip_to_overlay → reestablish_overlay_counter: owned_has_empty_term_value()==Some(v)
       → overlay_publish_root_value(v) [FRESH-ROOT CAS] → clear_owned   ⇒ overlay root carries v
REOPEN-WAL-replay: reconcile_lww (raw-bytes key b"" distinct) → apply insert_impl_core("",v) into owned
  → flip_to_overlay → reestablish republishes → clear_owned
```
Membership `""` is the same with `as_final()`/no value. Remove `""` = `as_non_final()` + WAL Remove +
reestablish honors the un-final (owned remove un-finalizes; reestablish does not republish). Vocab uses
its own independent (already-correct) root-value+reverse-index lifecycle.

---

## 5. VERIFICATION (memory-efficient first; real-disk `target/` scratch, NEVER tmpfs)

### 5.1 The NEW loom gate (the headline — closes Round-1 proof-gap #3)
`tests/persistent_lockfree_overlay_loom.rs`: a schedule witnessing **the root as simultaneously the
CAS target and a concurrent path-copy target** — `insert("") ‖ insert("a") ‖ remove("")` (≤3 threads;
the harness already runs 3-thread `preemption_bound=Some(3)` schedules, so within budget — Round-2 C4).
Add TWO new model fns mirroring §1.1: `publish_root_final(root)` = the POSITIVE (fresh `as_final` root via
`ModelRootSlot::compare_exchange`, rebasing on Err) and `finalize_root_in_place(root)` = the NEGATIVE
CONTROL (`root.load().try_set_final()`, the V2 lost-update). The harness's `ModelNode::with_child` already
snapshots `is_final` at the OLD value (loom file ~:83), so the negative control is observable. The gate:
the positive schedule MUST PASS (no lost `""`); the negative control MUST FAIL — pin it as an expected
failure (`#[should_panic]`/asserted-loss). This is a **gate**, not optional — the existing schedules only
clear/finalize CHILD leaves, never the root.

### 5.2 TLA+ note
`LockFreeOverlayDurableReplay.tla` / `…RemoveCas.tla` abstract terms as an opaque `present` set with
every transition "published by the root CAS." With §1.1 (empty term published by the root CAS, like
every other term), `""` is just another member of `present` — **no new spec needed**. (The V2-style
in-place finalize was OUTSIDE the spec; §1.1 brings `""` back inside it.) State this explicitly; the
loom gate (5.1) is the executable witness.

### 5.3 Decisive test matrix (byte + char; overlay-default + kill-switched-owned; checkpoint-reopen +
pure-WAL-replay): valued `insert_with_value("",v)`→checkpoint→reopen→`get_value("")==Some(v)`;
`increment("")`×N→reopen→count; upsert LWW; **membership `insert("")`→reopen→`contains("")==true`** (H3);
**remove `""`→reopen→`contains("")==false`** (symmetry); back-compat (old value-less file → `""`→None);
codec root-value round-trip + byte-identical-when-`None`; empty value WITH children; **concurrent
root-value race** (N threads `increment("")` → count == sum) (R1); compaction-`""` (H8); vocab
`""`→index→reopen.

### 5.4 Tests to UPDATE (corrected inventory — Round-1 C5 + Round-2 C5 corrections)
These tests PIN the old dropped-`""` behavior and **must be UPDATED IN the same phase that changes the
behavior** (P2 byte / P3 char), or that phase goes RED — the doc's green-every-phase invariant requires it.
- `overlay_correspondence_tests.rs` `m2a_reestablish_counter_round_trip` — **TWO** drop-assertions, not one
  (Round-2 C5d): (i) `overlay_get_value(b"")==Some(None)` (~:402) → **UPDATE** to `Some(Some(count))`; AND
  (ii) the iter-exclusion assertion `overlay_after == nonempty_before` (~:395, which asserts the overlay
  enumeration EXCLUDES `""`) → **UPDATE** to expect `(vec![], count)` included. Missing (ii) turns P2 RED.
- `overlay_routing_tests.rs` `m3_empty_term_get_value_reads_owned_under_overlay` (~:295) →
  **UPDATE/REMOVE — NOT keep** (Round-2 C5c corrects the V3 mis-classification). It writes `""` to OWNED,
  flips overlay IN-SESSION, and asserts `get_value_bytes(b"")==Some(77)` reads the OWNED arm *because of*
  the `!term.is_empty()` exception (atomic_ops.rs:128) that **H5 deletes**. After H5 the read routes to the
  overlay root → the assertion fails. Rewrite it to write `""` POST-flip and assert the overlay-root read.
- `overlay_correspondence_tests.rs` `m2a_reestablish_membership_round_trip` (~:281) — does NOT insert
  `""`; re-confirm green after the H3 membership-fold change (informational).
- `persistent_artrie_char/dict_impl_char.rs` `test_insert_cas_empty_term` (~:3042-3053) — asserts
  `assert!(!trie.insert_cas(""))`, pinning char's OLD dropped-`""` behavior (the `chars.is_empty()` guard
  at char `lockfree_cas.rs:242`). After P3 reroutes that guard to `overlay_publish_root_membership()`,
  `insert_cas("")` publishes a final root and returns `true` → this assertion flips → **UPDATE IN P3**
  (assert `insert_cas("")==true` and `contains("")==true`; rename e.g.
  `test_insert_cas_empty_term_publishes_root`). Missing this turns P3 RED (Round-3 C2c). (Byte has no
  symmetric `insert_cas(b"")`-returns-false unit test — the miss is char-only.)
- Char twins (`persistent_artrie_char_e1_readflip_correspondence.rs`): verified (Round-2) to use `""` only
  as a PREFIX probe (~:42), not an empty-term membership/value assertion — **no update needed**.

### 5.5 Gate every phase: `cargo nextest run --features persistent-artrie --no-fail-fast`
(~2592/3/0) + `scripts/verify-formal-correspondence.sh` exit 0 + unsafe-inventory equality (expect 0 new
unsafe). Tee full output to a file.

---

## 6. PHASING (reversible; green each phase)
- **P0 — byte serialize/load codec (H1) + H7.** Root value round-trips through owned serialize→disk→load;
  inert for value-less records (byte-identical). Reversible. Codec + owned-mode `""`+value round-trip tests.
- **P1 — byte H2 (overlay_root_to_owned carries value).** Inert until routing (overlay root never holds
  `""` yet). Reversible.
- **P2 — byte H3+H4+H5 as ONE atomic phase** (the coupled flip: guards→§1.1 publishers, reestablish
  republish, read-router) + the new loom gate (5.1) + UPDATE the pinned tests (5.4) + the decisive
  matrix (5.3). The irreducible coupling (R1/R4/H3). Revert = single commit.
- **P3 — char H3(free)+H4** (publishers via the shared default; char's `u64` seam) + char matrix.
- **P4 — vocab reopen test (H8 also: compaction-`""` for byte/char)** + the bucket→ART split (H7) test.
- **P5 — docs (kill the stale "overlay cannot represent the empty key" comments) + final gate.**

---

## 7. RISK TABLE (data-loss flagged 🔴)
| # | Change | Risk | Sev | Guard |
|---|---|---|---|---|
| R1 🔴 | H4 `:243`+`:1017`/char`:1519` together | split → durable-but-invisible increment | DATA-LOSS | remove as one edit; WAL-replay-of-`increment("")` + concurrent-race tests |
| R2 🔴 | **empty-term finalize via fresh-root CAS, NEVER in-place** | in-place = lost-update vs concurrent root copy | DATA-LOSS | §1.1 publishers + the loom gate (5.1) negative control |
| R3 🔴 | H3 reestablish republish before `clear_owned` | wrong order → `""` erased on reopen | DATA-LOSS | publish (fresh-root CAS) strictly before clear_owned; membership+counter reopen tests both paths |
| R4 🔴 | H1+H2 serialize/load/capture (byte) | partial → written-but-unread | DATA-LOSS | P0 codec round-trip BEFORE routing |
| R5 🟠 | H5 read router vs in-session owned `""` | router→empty overlay | High | sequence after H4 in P2 |
| R6 🟠 | structural base cases mis-removed (389/848/1025) | path corruption | High | DO-NOT-TOUCH table + full suite |
| R7 🟠 | remove(`""`) asymmetry | un-removable durable `""` | High | `overlay_unpublish_root` + symmetry test |
| R8 🟡 | H7 bucket→ART split (owned+replay) | `""` value loss on replay | Med | use `result.final_value`; replay test |
| R9 🟡 | H8 compaction drops `""` | `compact()` loses `""` | Med | verify `iter_prefix(b"")`; compaction test |

**Single most dangerous change: R2** — the empty-term finalize MUST be fresh-root CAS. It is the only
change that can produce a write that is acknowledged (cache/return-true) yet structurally absent
(lost-update), surviving as silent loss to the next checkpoint. **Guard:** §1.1 makes it the only
publication path; the loom gate (5.1) is an executable negative control that FAILS on any in-place
regression.

---

## 8. SELF RED-TEAM OF V3 (pre-empting Round 2)
- **Does `publish_root_cas` livelock under contention?** It's bounded-retry lock-free (each Err = another
  writer's progress, identical to every existing overlay write loop); `""` is rare so root contention is
  not worsened in practice. Correctness-neutral; a perf note only.
- **`overlay_publish_root_value` idempotence vs upsert:** the `changed` predicate returns false when value
  AND finality are unchanged, so a redundant upsert is a no-op (no spurious CAS) — but a real upsert to
  the SAME value still must return "updated"; the `changed` closure must compare correctly (value
  equality requires `V: PartialEq` — CHECK the bound; if absent, always-publish and let the CAS arbiter
  dedup, accepting one redundant CAS).
- **Increment RMW vs concurrent root path-copy:** `overlay_increment_root` reads the loaded root's count
  and CASes; a concurrent `insert("a")` advances the root → Err → rebase reads the NEW root's count
  (still the empty-term count, carried by `with_child` which preserves `value`) → re-applies δ. Verify
  `with_child` preserves the parent's `value` field (node.rs) so a child insert does not clobber the
  root counter. **← Round-2 must verify this.**
- **Does `as_non_final` (remove) drop a concurrently-set value?** remove is LWW with concurrent
  upsert/increment via the single root CAS — same semantics as non-empty remove-vs-write; acceptable.
- **Membership vs value on the same root:** `insert("")` then `upsert("",v)`: membership publishes
  `as_final()` (no value), upsert publishes `as_final().with_value(v)`; both via root CAS; final state =
  last CAS winner — consistent. A `V=()` trie never calls the value publisher.
- **Round-2 open question — CLOSED (affirmative, verified field-by-field in node.rs):** every path-copy
  mutator preserves the orthogonal fields, so empty-term root state and child structure never clobber each
  other: `with_child` preserves `value` (:770) + `flags`/`IS_FINAL` (:769) ⇒ a concurrent child insert does
  NOT drop the root's empty-term counter/membership; `as_final` preserves `store`+`value` (:812-813);
  `with_value` preserves `store`+`IS_FINAL` (:869-870); `as_non_final` retains `store` (:854), drops `value`
  (:859). Publication is one atomic `arc_swap` `compare_exchange`, so no torn (final-but-value-not-visible)
  read. **This is the keystone, and it holds — the fresh-root-CAS design is sound.**
- **Round-4 — eviction is root-safe on BOTH paths (verified):** the root carries the empty-term value, and
  the root is never evicted: `evict_node_at_path` rejects `path.is_empty()` (byte shared_trait_impl.rs:336 /
  char mod.rs:2094 / vocab mod.rs:866), AND the overlay-eviction path (char `OverlayEvictOutcome`) rebuilds
  the spine via `ancestor.with_child(..)` and never makes the root a victim — and `with_child` preserves the
  root's `value`+`flags` (keystone). No eviction-adjacent path drops the empty-term root value/finality.
