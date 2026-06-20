# Fix design (#48) — torn-checkpoint counter-delta double-apply (2026-06-08)

> **Status: IMPLEMENTED + VERIFIED (full gate green: 2729 suite incl. the lock-free durable loom proof,
> 751 feature-off, 147 doctests, formal + unsafe + fmt + downstream liblevenshtein). Direction I.**
> Regression battery (this file's T1–T8) landed in `tests/persistent_recovery_watermark_seed_l14.rs` +
> 3 `FileHeader` unit tests in `disk_manager.rs`. T1/T2 RED→GREEN-confirmed (neuter→`Some(8)`,
> restore→`Some(4)`). T3 (i64) is SUBSUMED — `try_increment_cas_durable` lives in `impl
> PersistentARTrieChar<u64>` (u64-only by type), so an i64 trie has NO durable-counter-delta surface to
> double-apply; the bug is u64-specific by construction. The T5 #41-no-panic test additionally EXPOSED a
> distinct pre-existing data-loss bug **#49** (steady-state checkpoint-record LSN gap stalls the
> committed watermark), fixed alongside — see
> docs/design/checkpoint-record-lsn-watermark-gap-49-design-2026-06-08.md. Direction I.
> The bug: the retain-WAL overlay checkpoint publisher fsyncs the IMAGE descriptor BEFORE the
> WAL `Checkpoint` record — a crash between them with a durable BatchIncrement DELTA at LSN N
> (folded into the image) leaves the reopen reading the PREVIOUS `checkpoint_lsn=P<N` → the
> delta RE-DRAINS on top of the image that already contains it → double-apply (steady-state
> `<u64>` counters, libgrammstein). Source: Plan+red-team agent pass.

## Root cause (the crux)
The reopen's `image_checkpoint_lsn` comes from the WAL `Checkpoint` RECORD (`state.stats.checkpoint_lsn`,
byte mmap_ctor.rs:452 / char :404), NEVER from the durable IMAGE. So a torn WAL record (still `P`)
poisons the drain-skip (recovery.rs:318) even though the image self-evidently covers `N`. The code
documents this exact gap: the "OBL-2 caveat" at flip.rs:1370-1383 ("the on-disk descriptor does not
record it; the only durable source is the WAL Checkpoint record").

## Direction I — image-descriptor-carried coverage (closes OBL-2)
Record the coverage frontier IN the image header (fsync'd ATOMICALLY with the image), and have the
reopen use `eff = max(wal_record_checkpoint_lsn, image_header_coverage)`. A torn WAL record is then
harmless (the durable image self-describes `coverage=N` → skip `(P,N]` correctly, they ARE in the
image → no double-apply; and the inverse — record=N but image=old — cannot happen because the single
image fsync ties coverage to the exact image bytes). #41 UNTOUCHED: image-coverage ≠ the in-memory
durability watermark (the same decoupling C2/#47 established); the capture assert overlay_checkpoint.rs:295
is not in this path. Rejected: (II) write-record-first is a silent-LOSS footgun (record=N, image=old →
skip-and-lose); no ordering of two independent fsyncs to two files is torn-safe both ways — a single
self-describing artifact is required.

**The value to write = the publisher's ALREADY-computed `checkpoint_lsn`** =
`base_watermark.max(self.committed_watermark.take_recovery_image_coverage())` (byte overlay_checkpoint.rs:346,
char persist.rs:638 — the #47/C2 line). Reusing it inherits #47's `max_applied_lsn` correctness (NO new
over-claim surface). NEVER re-derive from a file scan.

## ⚠️ DESIGN CORRECTION (verified 2026-06-08, before implementation): it is ONE SHARED format change
The agent assumed char uses its own `CharTrieFileHeader` (with a checkpoint_lsn already in the CRC). **FALSE
— `CharTrieFileHeader` is DEAD/unused** (grep: it is referenced ONLY in file_header.rs; nothing writes/reads
it on disk). The char trie uses the SAME shared `persistent_artrie_core::disk_manager::FileHeader` as byte
(its `dm.set_root_ptr`/`dm.set_entry_count` in char `persist.rs::publish_snapshot` write that shared header).
⇒ **#48 is ONE shared `FileHeader` change** (add the version-gated checksummed `image_checkpoint_lsn`), and
BOTH variants' publishers call the same `dm.set_image_checkpoint_lsn(...)` + both reopens call the same
`dm.image_checkpoint_lsn()`. Simpler than the per-variant plan below; the byte `FileHeader` edits in
disk_manager.rs are the single format change, shared by char. (Disregard the agent's "char already has the
field" claim; do NOT touch the dead `CharTrieFileHeader`.) Locate the shared header at
`src/persistent_artrie_core/disk_manager.rs` (`struct FileHeader`, `compute_checksum`, `set_root_ptr`/
`set_entry_count`, `sync`).

## Per-site edits
### CHAR (simplest — `CharTrieFileHeader.checkpoint_lsn` ALREADY exists + is in the V2 CRC, file_header.rs:48/101; the overlay path just never writes/reads it) — SUPERSEDED by the correction above; char uses the shared FileHeader.
- `persist.rs` `publish_snapshot` (:904): add `image_checkpoint_lsn: u64` param; set the header's
  `checkpoint_lsn` BEFORE `dm.sync()` (:943) — rides the same fsync. `publish_immutable_snapshot_retaining_wal`
  (:606) + `_with_eviction` (:728): compute `checkpoint_lsn` BEFORE `publish_snapshot`, pass it in.
- `mmap_ctor.rs` reopen: read `header.checkpoint_lsn` (when `was_loaded_from_disk`), `eff =
  wal_record.max(image_header.checkpoint_lsn)`, thread `eff` into the drain sites :540/:577/:614/:884/:907/:938.

### BYTE (`FileHeader` needs a new CHECKSUMMED field; version-gated for back-compat)
- `disk_manager.rs` `FileHeader`: add `image_checkpoint_lsn: u64` in the reserved bytes 56..64;
  `to_bytes`/`from_bytes` round-trip it; `compute_checksum` folds the 8 bytes ONLY when `version >= 2`
  (bump FORMAT_VERSION 1→2; v1 files validate exactly as before — the proven char V1/V2 pattern). Add
  `set_image_checkpoint_lsn`/`image_checkpoint_lsn` (mirror set/get_entry_count). `sync()`'s checksum
  refresh round-trips the field (self-consistent).
- `overlay_checkpoint.rs` publishers :329/:414: compute `checkpoint_lsn` before `publish_snapshot`,
  pass it in; `publish_snapshot` (:588) writes it via `set_image_checkpoint_lsn` BEFORE `dm.sync()` (:611).
- `mmap_ctor.rs` reopen: `image_cov = if was_loaded_from_disk { dm.image_checkpoint_lsn()? } else { 0 }`;
  `eff = checkpoint_lsn.unwrap_or(0).max(image_cov)`; thread into the drain sites :643/:672/:707.
- Add the trait method(s) to `BlockStorage` + the io_uring delegate (it shares `disk_manager::FileHeader`).

Scope: ONLY the retain-WAL OVERLAY publisher (the owned path TRUNCATES → no re-drain; owned has no delta
arm). Both byte + char.

## 4 NON-NEGOTIABLE constraints (a wrong move = silent loss)
1. The coverage field MUST be INSIDE the header checksum (byte v2-gated; char already in V2 CRC) — a torn
   coverage write must fail-closed, never yield a plausible-wrong value.
2. `I` MUST be the publisher's computed `checkpoint_lsn` (never a file re-scan — that reintroduces the
   #47 `max_lsn_in_segments` over-claim inversion).
3. The reopen MUST use `max(wal_record, image_coverage)`, NOT replace (the WAL record is authoritative
   when higher — e.g. a v1→overlay convert whose image coverage is 0).
4. Write order in `publish_snapshot`: set_root_ptr → set_entry_count → set_image_checkpoint_lsn →
   flush_all → sync (each RMW preserves prior fields; the final `dm.sync()` is the atomic linearization).

Accepted (house style): new v2 byte files fail-closed on OLD binaries (same as WAL VERSION 1→2 + char V2).

## Regression tests (RED→GREEN; construct the torn on-disk state directly — no fault-injection seam exists)
T1 byte-u64 / T2 char-u64 / T3 i64: create→increment(+4)→checkpoint→corrupt ONLY the WAL Checkpoint
record back to P (truncate/bad-CRC the post-checkpoint record; keep the delta at N in the live WAL +
the image's coverage=N intact)→reopen→assert Some(4) (pre-fix Some(8)). T4 post-#47+3a compound
(closes the case C2 left open). T5 #41-no-panic. T6 old-v1-file compat. T7 double-3a convergence. T8
torn-IMAGE → ChecksumMismatch fail-closed.
