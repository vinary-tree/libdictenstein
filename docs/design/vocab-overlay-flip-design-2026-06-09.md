# Vocab Overlay-Flip Campaign — Design (#14, 2026-06-09)

Bring the THIRD persistent-ARTrie variant (`PersistentVocabARTrie`, `src/persistent_vocab_artrie/`) to
overlay-only — mirroring the byte+char owned-tree deletion (C2) — so all three variants share the lock-free
`OverlayNode<K,V>` representation. Owner GO'd a SEPARATE campaign (after C2). Reuses the G-series shared
machinery (incl. the G5.3' CAS-walk skeleton). See [[vocab-overlay-flip-campaign]], [[l-campaign-owned-deletion-frontier]].

> STATUS: DESIGN, RED-TEAMED (round 1) → GO-WITH-FIXES. The corrections below are AUTHORITATIVE and
> SUPERSEDE any contradicting prose in the body. Exact `OverlayCasWalk` hooks finalize once G5.3' lands.

## ⚠️ RED-TEAM CORRECTIONS (2026-06-09, round 1) — AUTHORITATIVE
Round-1 red-team verdict: NO-GO as originally drafted → GO-WITH-FIXES. The destination architecture
(overlay-only forward `OverlayNode<CharKey,u64>` + a DERIVED id→term map, folding `LockFreeVocab`) is sound,
but three original premises were factually wrong + data-loss-critical. Corrections (a 2nd red-team will follow
once these land + G5.3' settles):

1. **[BLOCKER — recovery] Vocab is NOT "LSN-order / no CommitRank."** Byte DOES emit `CommitRank`
   (`persistent_artrie/overlay_write_mode.rs:246-249`; the shared Order-A skeleton calls `append_commit_rank` on
   every durable write, `durable_write.rs:151-156`). Overlay-regime recovery DROPS unranked records
   (`recovery.rs:328-334`: `RankRegime::Overlay => continue` for a `None` rank). Since the flip stamps
   `RankRegime::Overlay`, vocab **MUST emit a `CommitRank` per durable insert and ACK only after `mark_committed`**
   (full byte/char Order-A discipline) — else every just-acked insert that crashes before its CommitRank is
   silently dropped → the bijection loses acked (term,id) entries. Recovery is rank-aware `reconcile_lww`, NOT
   LSN-order. (§2/§4/§6 "LSN-order, no CommitRank, like byte" is DELETED.)
2. **[BLOCKER — id floor] Seed `next_index = max(header.next_index, max_replayed_id + 1)`** from the header +
   the rank-aware replay — NEVER from the reverse-index file (it is derived + possibly torn). Without fix #1, a
   dropped tail insert leaves `next_index` un-advanced → the next insert REUSES that id for a different term =
   cross-restart bijection corruption (worse than a gap). (§6.2 "max(reverse-index, …)" is corrected.)
3. **[BLOCKER — reverse index] Adopt OPTION D, not A.** The overlay leaf already stores `value = id`
   (`f5_loader.rs:155` round-trips `<CharKey,u64>`), so the overlay IS a complete term→id map and id→term is its
   inversion. On reopen, after `build_overlay_root_from_terms`, one `overlay_collect_units_with_values(&[])` pass
   builds the id→term map; persist it as a checkpoint-emitted mmap CACHE (O(1) reads) that is REBUILT from the
   overlay if absent/corrupt (never a source of truth). This ELIMINATES the NodeRef→term migration AND the
   torn-blob crash-safety problem. (No version-1→2 file migration needed; the old NodeRef reverse-index file is
   simply discarded on flip and rebuilt id→term from the overlay.)
4. **[RESOLVED per owner, 2026-06-09 — FULLY LOCK-FREE] Inserts are CONCURRENT lock-free durable overlay CAS**
   (`&self`, the `DurableOverlayWrite::insert_cas_with_value_durable` / `ValueWriteMode::InsertOnce` path byte/char
   use). Vocab is fully non-blocking for reads AND writes, matching byte/char (the owner's non-blocking standard).
   Id allocation = `next_index.fetch_add(1)` → NEARLY-DENSE monotonic ids: a structural CAS retry REUSES the id (no
   gap); a gap appears ONLY when two threads race to insert the SAME new term (the loser's pre-allocated id is
   unused — near-zero in single-threaded vocab building). The reverse id→term map is a NON-BLOCKING
   `DashMap<u64,String>` (NOT `RwLock<Vec>`), checkpoint-emitted to the mmap cache; gap-tolerant. The bijection +
   Order-A recovery + the id-floor (`max(header.next_index, max_replayed_id+1)`) are all gap-tolerant. `commit_seq`
   (the CommitRank generation) stays SEPARATE from the id (the id rides in the WAL `Insert` value + the overlay leaf
   value; `commit_seq` is the durable generation). **REJECTED: the single-writer/dense-scan model — NOT fully
   non-blocking.** NB the `WalkCtx::IndexAlloc` framing is moot — vocab uses the existing `value_publish_inner` /
   `InsertOnce` value seam, not a membership `OverlayCasWalk` insert hook.
5. **[DECIDE — key] Reuse `CharKey`, NOT a new `VocabKey`.** Matches G5 P7 ("vocab plugs in
   `impl OverlayCasWalk<CharKey,u64,S>`"); vocab nodes already serialize via the char arena
   (`CharKey::ARENA_MAGIC`), so a distinct key = a gratuitous arena-format fork. Vocab's file identity is its own
   96-byte `VOCB` header (`types.rs:30`, read by `read_vocab_header` via `open_without_validation`), INDEPENDENT
   of `K::FILE_MAGIC` — no file-confusion risk. (Also fix the `key_encoding.rs:279-280` doc that wrongly says
   `ARTC` covers vocab. Impl checkpoint: wire the overlay checkpoint to vocab's `VOCB` header writer, not char's.)
6. **["" empty term]** Route `""` through `overlay_publish_root_value` (ranked fresh-root-CAS, `flip.rs:706`), NOT
   an in-place root mutation; its id→term inverts to a zero-length term (distinct from absent).
7. **[fold] FOLD `LockFreeVocab`** — post-flip the overlay is the lock-free term→id rep, so the separate
   `LockFreeVocab` trie + `term_index_cache` are redundant (two sources of truth = bijection-corruption surface).
   Keep ONLY its id→term inversion role (that becomes the Option-D derived cache). Removes its `unsafe impl
   Send/Sync` (lockfree.rs:611-612) + the `unreachable!` on-disk guard. VERIFY no external consumer of the `pub`
   `LockFreeVocab` type first (deprecate-then-remove if any; `merge_into` bulk-build path).

## 1. Why vocab was NOT in C2 — it is "owned for life"
Per `docs/design/durable-global-commit-sequence-redesign-d2.7.md` §5: vocab `enable_lockfree` sets only
`lockfree_root`/`lockfree_cache` — NO fresh WAL routing, NO `overlay_write_mode`, NO `route_overlay`. The owned
`VocabTrieRoot`/`VocabTrieNode` tree is the LIVE durable production representation; `LockFreeVocab` (lockfree.rs)
is an in-memory accelerator cache (no `BufferManager`, no disk). So deleting vocab's owned tree TODAY would brick
it — vocab needs the overlay-as-default flip FIRST (the campaign below).

## 2. Current architecture
- **Forward** (term→id): the owned `VocabTrieRoot` tree (`V = u64` = the vocabulary index). `insert(term)`
  (mutation_api.rs:136) bloom-checks, allocates the next free index (`next_unassigned_index_from`), appends WAL
  `WalRecord::Insert{ term: term.as_bytes(), value: Some(index.to_le_bytes()) }` (mutation_api.rs:37-40), then
  writes the owned tree + the reverse index + the bloom filter. Bijection is validated both ways
  (`validate_index_insert`: term→index AND index→term).
- **Reverse** (id→term): `VocabReverseIndex` (reverse_index.rs) — a SEPARATE mmap file, a flat array of
  **`NodeRef{arena_id, slot_index}`** per index (reverse_index.rs:311/347). `get_term(index)` (query_api.rs:51)
  resolves the NodeRef **into the owned trie arena** and walks parent pointers to reconstruct the term.
- **WAL**: `next_lsn`/`synced_lsn` atomics; `sync_vocab_wal_after_append`. Today's owned recovery = `replay_insert`
  (disk_io.rs:455) replaying WAL `Insert` records AFTER the checkpoint LSN (mmap_ctor.rs:349). **⚠️ CORRECTION #1:
  post-flip recovery is rank-aware `reconcile_lww` (Overlay regime) and vocab MUST emit `CommitRank` per durable
  insert + ack only after `mark_committed`. The original "LSN-order / no CommitRank, like byte" claim was WRONG —
  byte emits CommitRank too, and Overlay-regime recovery DROPS unranked records (recovery.rs:332).**
- **Cache**: `LockFreeVocab` (in-memory term→id + id→term); **bloom** for fast new-term detection.

## 3. ⚠️ THE CRITICAL COMPLICATION — the reverse index points INTO the owned tree
`VocabReverseIndex` stores id→`NodeRef`(owned-arena slot). Deleting the owned tree (the flip's end goal)
**dangles every reverse-index entry** → `get_term` breaks. So, UNLIKE byte/char (which had no reverse index),
the vocab flip MUST migrate the reverse index off owned-arena NodeRefs. Options:

- **(A) id→term-bytes (denormalized) [RECOMMENDED]** — store the term string per id (header + offset table +
  term blob, or fixed-cap slots). `get_term(index)` reads the term directly; ZERO owned-tree dependency; the term
  is known at insert time. Cost: extra space (term bytes duplicated vs the forward trie). On-disk format migration
  of the reverse-index file (versioned header; one-time upgrade-on-open from the NodeRef format).
- **(B) id→overlay SwizzledPtr** — map to the term-leaf's disk location. Only valid POST-checkpoint (InMem overlay
  leaves have no SwizzledPtr until serialized); fragile across path-copy CAS. REJECT (complexity + InMem gap).
- **(C) rebuild id→term from the overlay on reopen** — walk the overlay (term→id) and invert into an in-memory
  id→term map. No reverse-index file. Cost: memory for the inverted map + a full overlay scan on reopen; random
  id→term needs the in-memory map resident. Viable for small/medium vocabs; loses O(1) mmap'd lookup.

RECOMMENDATION: **(A)** — it preserves the O(1) mmap'd id→term lookup, is overlay-independent, and the term is
free at insert time. The migration is a versioned reverse-index format (NodeRef-format files upgrade by
walking the old owned tree ONCE during the final flip, before the owned tree is deleted). Red-team must confirm
the upgrade path + the space tradeoff. (C) is the fallback if (A)'s space cost is unacceptable.

## 4. Target architecture
Overlay-only forward (`OverlayNode<VocabKey, u64>`) + migrated reverse index (option A) + reuse:
- The shared `OverlayDictionaryNode` (G5.1), the non-faulting read engine + faulting value-read (G5.2), the
  `DurableOverlayWrite` Order-A skeleton, `build_overlay_root_from_terms`, `OverlayCheckpoint`,
  `OverlayEvictable`, `OverlayFaulter`.
- The **G5.3' `OverlayCasWalk` skeleton** with vocab's specialization: `InsertResult = Result<Arc<root>, u64>`
  (the `u64` = the existing index on duplicate), `WalkCtx::IndexAlloc{index}` → terminal leaf
  `as_final().with_value(index)`, and `claim_generation` overridden to vocab's index allocation
  (`next_unassigned_index_from`/`fetch_add`). Vocab is **LSN-order recovery (byte-style)** — NO generation-ranking.
- **`VocabKey`**: a new `KeyEncoding` impl — `Unit = u32`, `Token = char` (UTF-8 terms), vocab's `ARENA_MAGIC`/
  `FILE_MAGIC`/`MAX_PREFIX_LEN`. (OPEN QUESTION for red-team: reuse `CharKey` vs a distinct `VocabKey` — vocab has
  its own on-disk magics, so likely distinct; confirm the overlay serialize/enumerate use `K::*_MAGIC`.)

## 5. Flip phases (mirror char/byte's L-series; gate each: full suite + vocab + reopen + soak + formal + unsafe + fmt)
- **V1 — durable overlay-write seam + reverse-index migration (A).** `impl LockFreeOverlay<VocabKey,u64,S> +
  DurableOverlayWrite + OverlayCasWalk` for vocab; `insert` allocates an id, writes the overlay leaf (value=id) +
  the NEW reverse-index (id→term-bytes) + WAL `Insert{term,index}` + bloom — keeping the bijection consistent.
  The WAL record is unchanged (term→index) = the source of truth for recovery.
- **V2 — overlay checkpoint route-split.** Capture the overlay (forward) + flush the reverse index consistently
  (retaining-WAL + watermark; the reverse-index file is checkpointed alongside the overlay image).
- **V3 — LSN-order regime-aware recovery.** Replay WAL `Insert{term,index}` AFTER checkpoint LSN into BOTH the
  overlay forward AND the reverse index (id→term). LSN-order (byte-style); id monotonicity seeded from
  `max(reverse-index-max-id, checkpoint)`.
- **V4 — production flip.** `route_overlay()` for vocab; `::new()`→empty overlay (WAL-less, like L3.2);
  `enable_lockfree` wires the overlay as the default. Retain `LockFreeVocab`/bloom as caches OR fold into the overlay
  (decide in red-team — the overlay IS now the lock-free rep, so `LockFreeVocab` may be redundant).
- **V5 — reopen via codec.** `enumerate_vocab_terms_with_ids_from_disk` → `build_overlay_root_from_terms`
  (forward); load OR rebuild the reverse index (id→term). BLOCKER#4 (corrupt-node-under-valid-descriptor → replay
  WAL) preserved.
- **V6 — delete the owned tree** (after the flip is production): delete `VocabTrieRoot`/`VocabTrieNode` + owned
  loaders/mutators/the owned reverse-index NodeRef resolution + prune vocab owned-tree UNSAFE rows
  (`vocab-disk-*`/`vocab-public-node-mutation`/`vocab-query-node-map`/`vocab-eviction-*`) + matching contract tags
  (set-equality gate) + migrate vocab owned white-box tests. KEEP the reverse-index mmap (now id→term) +
  Send/Sync + read-only child-map.

## 6. Data-loss-critical concerns
1. **Bijection consistency across crash.** insert(id N) writes WAL `Insert{term,N}` + overlay leaf(value=N) +
   reverse[N]=term. The WAL record is the SINGLE source of truth; the overlay + reverse index are derived. A crash
   mid-insert must recover to a consistent (term→N ∧ N→term) OR (neither) state — never half. Recovery replays the
   WAL into BOTH. The reverse-index write must NOT be acked before the WAL is durable.
2. **Id monotonicity across restart.** `next_index` must never reuse an id. Seed from `max(reverse-index, scan,
   checkpoint)` on reopen (like the `commit_seq` floor). A reused id corrupts the bijection.
3. **Reverse-index migration (A) without loss.** The one-time NodeRef→term-bytes upgrade (during V6, while the
   owned tree still exists) must capture EVERY existing id→term before the owned tree is deleted.
4. **Empty term `""`** as a vocab entry (id 0?) — handle like char/byte's `""` first-class value.

## 7. Red-team focus (repeated adversarial passes BEFORE implementing — data-loss-critical)
- The reverse-index migration (A): does the upgrade capture all mappings? Is the new format crash-safe? Does
  get_term stay O(1)? Is the space cost acceptable, or is (C) better?
- Bijection consistency: enumerate the crash windows (after WAL, before overlay; after overlay, before reverse;
  after reverse, before bloom) — does recovery reconstruct a consistent bijection from the WAL in each?
- Id monotonicity: prove no id reuse across restart (the floor seed).
- Is `LockFreeVocab` redundant post-flip (the overlay is the lock-free rep), or does it serve a distinct role
  (the id→term cache)? Fold or keep?
- `VocabKey` vs `CharKey`: on-disk magic distinctness; reopen can't confuse a vocab file for a char file.
- The G5.3' `WalkCtx::IndexAlloc` + vocab `claim_generation` (index alloc) — does it plug in cleanly, or does
  index allocation (which must be unique + monotonic) need more than the generation hook offers?

## 8. Gates (every phase + final)
full nextest + `--no-default-features` + doctests + `verify-formal-correspondence.sh` +
`verify-unsafe-boundary-inventory.sh` (set-equality; V6 prunes vocab owned-tree rows) + fmt + the vocab reopen +
bijection-consistency tests + a vocab multi-writer soak. Plus tidy the trivial pre-existing vocab dead-code
(`placeholder`, `DEFAULT_VOCAB_BUFFER_POOL_SIZE`, `shutdown`, unused `IoUringDiskManager` import).
