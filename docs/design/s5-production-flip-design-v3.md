# S5 v3 — §4 redesign (FileHeader checkpoint_lsn) + the re-red-team MUST-fixes

**Crate `libdictenstein`, char ARTrie. 2026-06-03. Design only — NO code edited.** Baseline: committed
reversible core S0–S4 (HEAD `97faa02`). Supersedes the §4 of `s5-production-flip-design-v2.md` (which
the re-red-team proved would CORRUPT the data file) and folds in every other v2-§16 MUST-fix. Pending a
final red-team (§9). The v2 §1/§2/§3/§5/§6/§7/§8 mechanisms are UNCHANGED and still apply; this doc only
rewrites the broken/under-specified parts.

## 0. The v2 break (recap) + the corrected ground truth

v2 §4.3 said "write `checkpoint_lsn` to the data header at bytes 24..32." The re-red-team showed the char
DATA file header is `FileHeader` (magic `"PART"`, `disk_manager.rs:76`), NOT the `#[cfg(test)]`-only
`CharTrieFileHeader` (`"ARTC"`). **Verified on-disk `FileHeader` byte map (disk_manager.rs:180–208):**

```
0..8   magic (u64, "PART"+v)        24..28 block_count (u32)     48..56 checksum (u64)
8..12  version (u32)                28..32 _pad1 (u32, =0)        56..64 RESERVED (=0, per to_bytes:191)
12..16 flags (u32)                  32..40 free_list_head (u64)
16..24 root_ptr (u64)              40..48 entry_count (u64)
```

`compute_checksum` (disk_manager.rs:131–167) is FNV-1a over **{magic, version, flags, root_ptr,
block_count, free_list_head, entry_count}** — it does NOT hash `_pad1`, the checksum field, or the
reserved bytes 56..64. So:
- writing `checkpoint_lsn` at 24..32 (v2's plan) CLOBBERS `block_count`+`_pad1` ⇒ checksum mismatch ⇒
  **unopenable** (the v2 bug). CONFIRMED-DEAD.
- bytes **56..64 are genuinely free** (written 0, not checksummed) — the home IF a header field were
  used; but §1's re-red-team chose the WAL-record source instead, needing no header field at all.

## 1. §4 v3 — checkpoint_lsn from the WAL `Checkpoint` record (the headline — REVISED by re-red-team)

The re-red-team (code-grounded, §9) found the §4 problem is NOT "where to store checkpoint_lsn in the
data header" — it is "**stop reading checkpoint_lsn from the data header at all.**" Root cause + fix:

**RA-14 confirmed, and it is a LIVE pre-existing bug (independent of S5):** `get_checkpoint_lsn`
(recovery.rs:574) opens `trie_path`, reads block 0 (a 64-byte `FileHeader`, magic "PART") and parses it
as a `CharTrieFileHeader` (magic "ARTC") via `from_bytes`, which pulls `checkpoint_lsn` from **bytes
24..32 WITHOUT any magic check** (file_header.rs:159). In a `FileHeader`, bytes 24..32 are
`block_count`(u32, 24..28) ⧺ `_pad1`(=0, 28..32) ⇒ `get_checkpoint_lsn` returns **`Some(block_count)`**.
`replay_wal_after_checkpoint` (recovery.rs:464–499) then **`continue`s past every WAL record with
`lsn <= block_count`** (line 484) ⇒ if `block_count > real_checkpoint_lsn`, acked tail records are
silently DROPPED (loss); if `<`, already-folded counter increments re-apply (double-count).
`CharTrieFileHeader` is **never written to the data file in production** (write-site grep empty), so the
read is always garbage. CONFIRMED-DEAD as a source.

**The correct source already exists and is already used by normal open: the WAL `Checkpoint` record.**
Both ctors derive checkpoint_lsn by scanning the WAL for `max(WalRecord::Checkpoint{checkpoint_lsn})`
(mmap_ctor.rs:319, io_uring_ctor.rs:142). The checkpoint protocol appends that record (then fsync, then
rotate) AFTER publishing the data-file image, so it is the authoritative "image reflects ≤ this LSN"
marker. v3 §4:
- Add a shared helper `latest_checkpoint_lsn_from_wal(wal_path) -> Result<Lsn>` (extract the
  mmap_ctor.rs:309–323 scan; the ctors call it too — DRY).
- `replay_wal_after_checkpoint` sources checkpoint_lsn from that helper (the WAL), NOT
  `get_checkpoint_lsn`. **Delete** the `CharTrieFileHeader`-from-data-file read in `get_checkpoint_lsn`
  (or repoint it at the WAL helper); the RecoveryReport field (recovery.rs:437) uses the same helper.
- This makes the RecoveryManager tail-replay path **consistent with normal open** (same authoritative
  source, same crash-window semantics) and fixes the live RA-14 loss/double-count — with **ZERO on-disk
  format change, ZERO FileHeader edit, ZERO back-compat surface.**

**Why this beats the v2/early-v3 "put checkpoint_lsn in the header" approach** (kept as the REJECTED
alternative): a `FileHeader.checkpoint_lsn` field (bytes 56..64, which ARE free — RB-2-confirmed;
un-checksummed for back-compat) is *implementable* — `set_root_ptr`'s RMW-full-header + the single
`publish_snapshot` fsync makes it crash-atomic with the root; excluding it from the FNV keeps old files
openable; to_bytes AND from_bytes must BOTH round-trip 56..64 or `sync()`'s RMW-checksum zeros it. But it
adds an on-disk field, an un-checksummed-hint residual, and gives the corruption path DIFFERENT
(atomic-with-root) semantics than normal open (which uses the WAL record) — an inconsistency the WAL
helper avoids entirely. **Chosen: WAL-record source. Rejected: FileHeader field** (and rejected harder:
the early-v3 bytes-24..32 write, which clobbers block_count, and a FORMAT_VERSION bump, which breaks
non-flipped tries on old binaries).

**Gate:** (a) write a checkpoint, force the tail-replay path, assert every acked post-checkpoint term
survives (no skip-by-block_count loss); (b) a counter checkpointed then incremented once more reads
exactly +1 after tail-replay (no double-count); (c) `latest_checkpoint_lsn_from_wal` == the value the
ctors compute.

## 2. §4 v3 — the per-record-regime global rebuild (depends on §1)

Unchanged from v2 §4.2 in intent, now buildable on the §1 header fix: ONE global `reconcile_lww` pass
over all segments, each record tagged with ITS segment's header regime (segments single-regime;
generalize `reconcile_lww` recovery.rs:257 to a per-record regime or a `regime_of: &HashMap<Lsn,
RankRegime>`), generation-ordered globally (LSNs monotone across rotate, RA-7-VALID), using the
WAL-record checkpoint_lsn (§1, `latest_checkpoint_lsn_from_wal`) to skip the folded prefix. Rewrite `rebuild_from_wal_segments`
(recovery.rs:1469), `RecoveryManager::rebuild_from_wal` (char recovery.rs:503), `recover_from_archives`
(mmap_ctor.rs:1167). `WalRecord::Remove` is NEVER dropped under Overlay (defense-in-depth; a dropped
remove resurrects, a spurious remove is a no-op — though RA-6 confirmed real removes are ranked anyway).
Break-glass `--feature`-gated fail-closed-on-Overlay-segment. A3 floor populated at checkpoint
(`set_commit_seq_floor(commit_seq@capture)`, monotone, carried across rotate).

## 3. §1 v3 — the streaming, fallible recovery-into-overlay rebuild (closes RA-2 + the I/O swallow)

v2 §1's `reestablish_overlay_after_recovery` materialized the whole `Vec` via `iter_with_values()` which
(a) doubles peak memory at scale (RA-2 showstopper) and (b) SWALLOWS I/O errors to an empty Vec
(`mod.rs:625` `.ok().unwrap_or_default()`) ⇒ silent total loss. v3:
- **Stream by first code-point:** for each of the (bounded) first-unit partitions, call the FALLIBLE
  `iter_prefix_with_values(prefix)?`, insert each chunk into the overlay (via `insert_cas` / the new
  no-WAL valued publisher), then DROP the chunk before the next. Peak overlay-build memory is bounded by
  one partition, not the whole trie. (Boundary correctness: partitioning by the FULL first code-point
  enumerates a disjoint cover of all terms — every term has exactly one first unit, or is the empty
  term handled separately — so no term is missed or double-built. Gate: a test that the streamed rebuild
  yields the same membership/values as a single-pass enumeration.)
- **Abort on `Err`:** any `iter_prefix_with_values(prefix)?` error ABORTS the open/flip (propagate the
  `Err` from the ctor) — never the lossy `iter_with_values()`. A mid-stream abort leaves the owned tree
  UN-cleared (the clear is the LAST step, after all chunks succeed) ⇒ the trie is still owned-consistent,
  the open fails loud, no half-built-overlay-then-cleared-owned loss. (RA-1 holds: the chunked inserts
  are still durable-free.)
- `insert_cas_with_value_nodurable` (new): `build_value_path_recursive` + root CAS, **zero `append_*`/
  fsync** — gate-asserted (RA-3).

## 4. The remaining v2-§16 MUST-fixes (mechanisms unchanged, now explicit)

- **cfg un-gating (S5-9):** remove `#[cfg(any(test, feature="bench-internals"))]` from
  `capture_snapshot_immutable` (persist.rs:342), `publish_immutable_snapshot_retaining_wal[_with_eviction]`
  (:547), `overlay_to_inner` (:654), `count_overlay_finals` (:1142/1250) + any helper they call. Adds NO
  new `unsafe` token (`overlay_to_inner`'s `Box::into_raw` is safe); re-run the unsafe-inventory gate.
- **`set_overlay_regime` length guard (S5-4):** add `WalWriter::is_empty_after_header()` (file length ==
  `WalHeader::SIZE`); make BOTH the sync (writer.rs:373) and async (async_writer.rs:603)
  `set_overlay_regime` AND the new `set_owned_regime` RETURN `Err` if not empty; the flip caller asserts
  it; post-stamp `assert!(rank_regime()==expected)`.
- **§9 non-faulting-first remove pre-flight:** `remove_cas_durable` (lockfree_cas.rs:553) tries
  `find_leaf_lockfree` FIRST (present-in-memory ⇒ append; absent-via-non-OnDisk-edge ⇒ skip; OnDisk edge
  ⇒ THEN `find_leaf_faulting`). Shrinks the fault window to cold-prefix removes. N-S4-3 soak mandatory.
- **`begin_document` reject under overlay:** `begin_document` (document_tx.rs:40) returns `Err` under
  `route_overlay()` (symmetry with `commit_document`) — else it burns an un-watermarked LSN ⇒ the
  committed watermark stalls ⇒ checkpoint reclaim can't advance.

## 5. Revised ordered edit list
Reversible hardening (land before owner GO), then the single irreversible flip:
- S5-1 (§1 here): add `latest_checkpoint_lsn_from_wal(wal_path)` helper (extract the mmap_ctor WAL scan);
  `replay_wal_after_checkpoint` + the RecoveryReport field source checkpoint_lsn from it; DELETE the
  `CharTrieFileHeader`-from-data-file read in `get_checkpoint_lsn`. Fixes the LIVE RA-14 loss/double-count;
  NO on-disk format change. **The redesigned headline; corruption-path now consistent with normal open.**
- S5-2 (A3 floor), S5-3 (§2 per-record-regime global rebuild + Remove-never-dropped + break-glass),
  S5-4 (length predicate + guards), S5-5 (`set_owned_regime`), S5-6 (reject negative increment/fetch_add
  + guard insert_batch_bytes family + refuse non-{(),u64} stamp), S5-7 (merge reject under Overlay),
  S5-8 (promote the 3 #41 asserts), + begin_document reject. All reversible pure rejects/guards/asserts/
  header-hint.
- S5-9 (cfg un-gate + checkpoint route-split), S5-10 (§3 streaming-fallible reestablish + flip_to_overlay
  + kill_switch_to_owned + insert_cas_with_value_nodurable; wire reestablish into both ctors gated on
  Overlay regime), S5-11 (tests). Reversible.
- **S5-12 — THE FLIP (IRREVERSIBLE, ~6 lines):** the V∈{(),u64} ctors call flip_to_overlay. Owner GO +
  full gate. Arbitrary-V UNCHANGED.

## 6. Gate additions (beyond v2 §12)
- **RA-14 tail-replay (the §4 fix):** write a checkpoint, force `replay_wal_after_checkpoint`, assert
  every acked post-checkpoint term survives (no skip-by-block_count loss) + a checkpointed-then-+1 counter
  reads exactly +1 (no double-count); unit: `latest_checkpoint_lsn_from_wal` == the ctors' value.
- **Streaming-rebuild equivalence + memory bound:** streamed rebuild == single-pass membership/values;
  peak bounded (no whole-Vec materialization).
- **I/O-error abort:** inject an `iter_prefix_with_values` `Err` mid-rebuild ⇒ open returns `Err`, owned
  tree NOT cleared, no loss.
- (plus all v2 §12 gates: V1 reopen→write→reopen, H1 negative-increment, N1 batch_bytes, V2 merge,
  H3 flip-after-checkpoint, kill-switch round-trip, A2 mixed-segment rebuild, flip-crash-at-each-step,
  N-S4-3 lock-order soak, loom + FULL TLA + recovery/char suites + verify-formal-correspondence.sh.)

## 7. Residual assumptions for the FINAL red-team
- RB-1 (RESOLVED by §9): the §4 fix is now the WAL-record source (no FileHeader field, no checksum
  question). Residual: confirm `replay_wal_after_checkpoint`'s crash-window (data image published but the
  WAL `Checkpoint` record not yet appended) yields at most the SAME double-count as normal open — i.e. the
  fix introduces no NEW divergence vs the existing normal-open semantics.
- RB-2 (RESOLVED by §9): bytes 56..64 free — confirmed (only the FileHeader-field alternative needed it;
  not used by the chosen WAL-record fix).
- RB-3: the streaming first-unit partition is a disjoint cover (no missed/double term at chunk
  boundaries; the empty term handled).
- RB-4: a mid-rebuild abort (owned NOT cleared) leaves a fully-recoverable owned-consistent trie; verify
  the ctor propagates the `Err` and no partial overlay is published.
- RB-5: un-gating the 5 overlay-checkpoint fns adds zero new `unsafe` and compiles in `--no-default-features`.
- RB-6 (RESOLVED by §9): the chosen WAL-record fix writes NO block-0 header field, so the atomic-header
  question is moot. (For the record: had the FileHeader-field alternative been chosen, RB-6 was VALIDATED
  — `set_root_ptr`/`set_entry_count` RMW the full 64-byte header and only `publish_snapshot`'s single
  `sync()` does `sync_all`, so all block-0 fields share one crash-atomic fsync; persist.rs:804–811.)
- RB-7 (RESOLVED by §9): no un-checksummed header value exists in the chosen fix.

## 8. Unchanged from v2 (still apply)
§1 V1 close (now streaming, §3 here), §2 producer gating, §3 merge reject, §5 H3 predicate, §6 V4
flip-performs-checkpoint, §7 A6 kill-switch, §8 H4 assert promotion, §10 crash table, §13 char-only.

## 9. Re-red-team verdict (DONE 2026-06-03, code-grounded — replaces the deferred external pass)

The §4 redesign was re-red-teamed against the ACTUAL source (disk_manager.rs, recovery.rs,
file_header.rs, mmap_ctor.rs). Findings:

1. **RA-14 is a CONFIRMED, LIVE, pre-existing data-loss/double-count bug** (NOT merely an S5 risk):
   `get_checkpoint_lsn` parses the data file's `FileHeader` block 0 as a `CharTrieFileHeader`, reading
   `checkpoint_lsn` from bytes 24..32 = `block_count` (no magic check); `replay_wal_after_checkpoint`
   skips all WAL records `lsn ≤ block_count`. `CharTrieFileHeader` is never written to the data file in
   production. → The early-v3 "put checkpoint_lsn in the header" framing was treating a symptom.
2. **Redesign: source checkpoint_lsn from the WAL `Checkpoint` record** (the authoritative marker normal
   open already uses), via a shared `latest_checkpoint_lsn_from_wal` helper; delete the data-file-header
   read. ZERO on-disk format change, fixes the live bug, unifies the corruption path with normal open.
3. **The FileHeader-field alternative is sound but unnecessary** — RB-2 (bytes 56..64 free) CONFIRMED,
   RB-6 (one crash-atomic publish fsync) VALIDATED, back-compat clean (exclude from FNV; round-trip
   56..64 in BOTH to_bytes/from_bytes or `sync()` zeros it) — recorded as the rejected alternative so the
   analysis isn't lost.
4. **RB-1 (normal open uses the WAL record, not any header) CONFIRMED** (mmap_ctor.rs:319,
   io_uring_ctor.rs:142), which is exactly what makes the redesign correct.

**Verdict on §4:** SAFE TO IMPLEMENT as a reversible, on-disk-format-free recovery-path fix (S5-1) — and
it should land regardless of the S5 flip, since RA-14 is a live bug. **Remaining residuals for the final
pre-FLIP red-team (S5-12 only):** RB-3 (streaming partition cover), RB-4 (mid-rebuild abort safety), RB-5
(cfg un-gate adds no unsafe), and RB-1-residual (tail-replay crash-window ≡ normal-open semantics). The
irreversible flip (S5-12) still requires owner GO + the full gate; the reversible hardening subset
(S5-1…S5-11) is unblocked.
