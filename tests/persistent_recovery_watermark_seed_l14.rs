//! **Recovery double-apply regression guards (#47 — FIXED via C2).** A corruption/
//! archive-rebuilt trie carrying a BatchIncrement DELTA must apply the delta EXACTLY ONCE
//! across `recovery → checkpoint() → drop → reopen`.
//!
//! ## The fixed bug (was a confirmed PRODUCTION data-loss bug for `V=u64`)
//! For `V=u64` (libgrammstein's n-gram count monomorph) the overlay BatchIncrement-DELTA
//! applier works (`increment_cas`), so the bug was NOT masked: u64
//! `open_with_recovery_config`/`recover_from_archives` → `checkpoint()` → reopen yielded
//! **Some(8)** for a recovered `+4` (DOUBLE-APPLY). Root cause: the recovery ctors return
//! the apply-loop trie directly (no re-open ⇒ no `open_inner` "F7 FIX C" watermark seed),
//! so `watermark()==0` → the post-recovery checkpoint recorded `checkpoint_lsn=0` → the
//! surviving rebuild archive RE-DRAINED the delta on reopen.
//!
//! ## The C2 fix (commit — see docs/design/recovery-double-apply-fix-c2-design-2026-06-08.md)
//! The recovery ctors stash `max_applied_lsn` (the LSN they ACTUALLY applied — NOT
//! `max_lsn_in_segments`, which reads past interior corruption and would over-claim →
//! silent LOSS) via `CommittedWatermark::set_recovery_image_coverage`. The first
//! post-recovery `checkpoint()` records `checkpoint_lsn = max(watermark, coverage)` in the
//! on-disk WAL `Checkpoint` record — the IMAGE-COVERAGE fact that drives the reopen
//! drain-skip — WITHOUT inflating the in-memory durability watermark, so the #41
//! capture-ordering assert (`watermark ≤ synced_frontier`) stays untouched. The reopen then
//! skips the already-checkpointed archive → the delta applies exactly once.
//!
//! The `V=i64` guards exercise the recovery/checkpoint/reopen path on the byte i64 monomorph
//! (which, separately, is NOT overlay-eligible for counters the way u64 is). They are GREEN
//! both before and after C2.
//!
//! Uses a BARE BatchIncrement (no preceding SET) so a re-drain IS visible (a `[SET, +delta]`
//! archive self-cancels — the re-drained SET masks the delta).
#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::char::PersistentARTrieChar;
use libdictenstein::persistent_artrie::core::disk_manager::FileHeader;
use libdictenstein::persistent_artrie::core::durability::DurabilityPolicy;
use libdictenstein::persistent_artrie::{
    PersistentARTrie, RecoveryMode, WalConfig, WalHeader, WalReader, WalRecord, WalWriter,
};
use libdictenstein::MappedDictionary;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::tempdir;

/// Corrupt the CRC of the record at `record_index` (so the WAL reader stops just before it).
fn corrupt_record_crc(path: &Path, record_index: usize) {
    use std::io::{Read, Seek, SeekFrom, Write};
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("open WAL for corruption");
    let mut record_offset = WalHeader::SIZE as u64;
    for _ in 0..record_index {
        file.seek(SeekFrom::Start(record_offset + 4))
            .expect("seek len");
        let mut length = [0u8; 4];
        file.read_exact(&mut length).expect("read len");
        record_offset += u32::from_le_bytes(length) as u64;
    }
    file.seek(SeekFrom::Start(record_offset)).expect("seek crc");
    let mut crc = [0u8; 1];
    file.read_exact(&mut crc).expect("read crc");
    crc[0] ^= 0x80;
    file.seek(SeekFrom::Start(record_offset))
        .expect("seek crc write");
    file.write_all(&crc).expect("write bad crc");
    file.sync_all().expect("sync corruption");
}

/// Corrupt the LAST WAL record (models a crash that durably wrote the image but NOT the final
/// `Checkpoint` record — the #48 torn window).
fn corrupt_last_wal_record(path: &Path) {
    let count = WalReader::new(path).expect("wal reader").iter().count();
    assert!(count >= 1, "expected >= 1 WAL record to corrupt");
    corrupt_record_crc(path, count - 1);
}

// Retained for hand-constructing i64-valued WAL `Insert`/`Update` records in this recovery
// suite (the i64 sibling of the inline `BatchIncrement` deltas); not currently referenced.
#[allow(dead_code)]
fn ser_i64(value: i64) -> Vec<u8> {
    libdictenstein::serialization::bincode_compat::serialize(&value).expect("serialize i64")
}

fn recovery_config() -> WalConfig {
    WalConfig {
        archive_enabled: true,
        archive_dir: PathBuf::from("wal_archive"),
        max_segments: 16,
        max_archive_bytes: 16 * 1024 * 1024,
    }
}

fn write_wal(path: &Path, records: Vec<WalRecord>) {
    let writer = WalWriter::create(path).expect("create WAL");
    for record in records {
        writer.append(record).expect("append WAL record");
    }
    writer.sync().expect("sync WAL");
}

fn corrupt_header_magic(path: &Path) {
    use std::io::{Seek, SeekFrom, Write};
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open trie file for corruption");
    file.seek(SeekFrom::Start(0)).expect("seek header magic");
    file.write_all(b"BAD!").expect("corrupt header magic");
    file.sync_all().expect("sync header corruption");
}

/// Rewrite the on-disk `FileHeader` to FORMAT_VERSION 1 (the pre-#48 format whose checksum does
/// NOT cover the image-coverage field), recomputing the v1 checksum. Models an OLD file created
/// before #48: the reopen reads `image_checkpoint_lsn` as 0 ⇒ `eff = wal_record` = pre-#48
/// behavior. (#48 T6 back-compat.)
fn rewrite_header_to_v1(path: &Path) {
    use std::io::{Read, Seek, SeekFrom, Write};
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("open trie file for v1 downgrade");
    let mut bytes = [0u8; 64];
    file.read_exact(&mut bytes).expect("read header");
    let mut header = FileHeader::from_bytes(&bytes);
    header.version = 1;
    header
        .image_checkpoint_lsn
        .store(0, std::sync::atomic::Ordering::SeqCst);
    header.update_checksum();
    file.seek(SeekFrom::Start(0)).expect("seek header");
    file.write_all(&header.to_bytes()).expect("write v1 header");
    file.sync_all().expect("sync v1 header");
}

/// Corrupt ONLY the image-coverage bytes (`bytes[56..64]`) of the on-disk v2 `FileHeader` WITHOUT
/// repairing the checksum — models a torn coverage write. A v2 reopen must fail-closed
/// (`ChecksumMismatch`), never read a plausible-but-wrong coverage. (#48 T8.)
fn corrupt_header_image_coverage(path: &Path) {
    use std::io::{Read, Seek, SeekFrom, Write};
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("open trie file for image-coverage corruption");
    file.seek(SeekFrom::Start(56)).expect("seek coverage");
    let mut b = [0u8; 1];
    file.read_exact(&mut b).expect("read coverage byte");
    b[0] ^= 0x80;
    file.seek(SeekFrom::Start(56)).expect("seek coverage write");
    file.write_all(&b).expect("write corrupt coverage");
    file.sync_all().expect("sync coverage corruption");
}

/// Byte `open_with_recovery_config` (corruption rebuild): a bare BatchIncrement delta
/// must be applied exactly once across recovery → checkpoint → reopen.
#[test]
fn l1_byte_corruption_rebuild_no_delta_double_apply_across_checkpoint_reopen() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("l1_byte.part");
    {
        let _trie = PersistentARTrie::<i64>::create(&path).expect("create byte trie");
    }

    fs::remove_file(path.with_extension("wal")).expect("replace active WAL");
    write_wal(
        &path.with_extension("wal"),
        vec![WalRecord::BatchIncrement {
            entries: vec![(b"counter".to_vec(), 4)],
        }],
    );
    corrupt_header_magic(&path);

    {
        let (recovered, report) =
            PersistentARTrie::<i64>::open_with_recovery_config(&path, recovery_config())
                .expect("rebuild byte trie");
        assert_eq!(report.mode, RecoveryMode::RebuildFromWal);
        assert_eq!(
            recovered.get_value("counter"),
            Some(4),
            "rebuild must accumulate the bare +4 delta to 4"
        );
        recovered.checkpoint().expect("post-recovery checkpoint");
    }

    let reopened = PersistentARTrie::<i64>::open(&path).expect("reopen");
    assert_eq!(
        reopened.get_value("counter"),
        Some(4),
        "the BatchIncrement delta must be applied EXACTLY ONCE across recovery→checkpoint→reopen \
         (C2 records checkpoint_lsn=max_applied_lsn so the reopen skips the archived delta)"
    );
}

/// **THE HEADLINE #47 GUARD (u64, the production counter monomorph).** RED before the C2
/// fix (`Some(8)` — the confirmed double-apply), GREEN after (the post-recovery checkpoint
/// records `checkpoint_lsn = max_applied_lsn`, so the reopen drain-skip drops the archived
/// delta exactly once). Unlike i64, u64's delta arm is NOT masked by the u64-only no-op.
#[test]
fn l1_byte_u64_recovery_checkpoint_reopen_applies_delta_once() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("u64_byte.part");
    {
        let _t = PersistentARTrie::<u64>::create(&path).expect("create");
    }
    fs::remove_file(path.with_extension("wal")).expect("replace active WAL");
    write_wal(
        &path.with_extension("wal"),
        vec![WalRecord::BatchIncrement {
            entries: vec![(b"counter".to_vec(), 4)],
        }],
    );
    corrupt_header_magic(&path);
    {
        let (recovered, report) =
            PersistentARTrie::<u64>::open_with_recovery_config(&path, recovery_config())
                .expect("rebuild");
        assert_eq!(report.mode, RecoveryMode::RebuildFromWal);
        assert_eq!(
            recovered.get_value("counter"),
            Some(4),
            "recovery accumulates +4 to 4"
        );
        recovered.checkpoint().expect("checkpoint");
    }
    let reopened = PersistentARTrie::<u64>::open(&path).expect("reopen");
    assert_eq!(
        reopened.get_value("counter"),
        Some(4),
        "#47: u64 BatchIncrement delta must apply EXACTLY ONCE across recovery→checkpoint→reopen \
         (Some(8) = the confirmed double-apply; C2 records checkpoint_lsn=max_applied_lsn so the reopen skips it)"
    );
}

/// #47 idempotence across a SECOND checkpoint+reopen cycle (guards against a residual that
/// only masks the first reopen).
#[test]
fn l1_byte_u64_recovery_double_checkpoint_reopen_idempotent() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("u64_byte2.part");
    {
        let _t = PersistentARTrie::<u64>::create(&path).expect("create");
    }
    fs::remove_file(path.with_extension("wal")).expect("replace active WAL");
    write_wal(
        &path.with_extension("wal"),
        vec![WalRecord::BatchIncrement {
            entries: vec![(b"counter".to_vec(), 4)],
        }],
    );
    corrupt_header_magic(&path);
    {
        let (recovered, _r) =
            PersistentARTrie::<u64>::open_with_recovery_config(&path, recovery_config())
                .expect("rebuild");
        recovered.checkpoint().expect("checkpoint 1");
    }
    {
        let re1 = PersistentARTrie::<u64>::open(&path).expect("reopen 1");
        assert_eq!(re1.get_value("counter"), Some(4), "after reopen 1");
        re1.checkpoint().expect("checkpoint 2");
    }
    let re2 = PersistentARTrie::<u64>::open(&path).expect("reopen 2");
    assert_eq!(
        re2.get_value("counter"),
        Some(4),
        "#47: stable across two checkpoint/reopen cycles"
    );
}

/// Char `recover_from_archives` (archive rebuild): same property, with the source archive
/// == the trie's own `wal_archive` so the reopen scan genuinely covers it.
#[test]
fn l1_char_archive_recovery_no_delta_double_apply_across_checkpoint_reopen() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("l1_char.artc");
    // Source archive == the trie's own wal_archive (default archive_dir), so the post-
    // recovery reopen scans the SAME directory the recovery read from.
    let archive_dir = dir.path().join("wal_archive");
    fs::create_dir_all(&archive_dir).expect("create archive dir");
    write_wal(
        &archive_dir.join("wal_0001.segment"),
        vec![WalRecord::BatchIncrement {
            entries: vec![(b"counter".to_vec(), 4)],
        }],
    );

    {
        let (recovered, _stats) = PersistentARTrieChar::<i64>::recover_from_archives(
            &path,
            &archive_dir,
            recovery_config(),
        )
        .expect("recover char trie from archives");
        assert_eq!(
            recovered.get_value("counter"),
            Some(4),
            "archive recovery must accumulate the bare +4 delta to 4"
        );
        recovered.checkpoint().expect("post-recovery checkpoint");
    }

    let reopened = PersistentARTrieChar::<i64>::open(&path).expect("reopen");
    assert_eq!(
        reopened.get_value("counter"),
        Some(4),
        "L1: char archive recovery delta must be applied EXACTLY ONCE across \
         recovery→checkpoint→reopen"
    );
}

/// **CHAR u64 #47 guard** — genuinely exercises the char `recover_from_archives` C2 fix
/// (the i64 guard above is masked by the u64-only delta no-op). RED before C2 (`Some(8)`),
/// GREEN after (the post-recovery checkpoint records `checkpoint_lsn = max_applied_lsn`).
#[test]
fn l1_char_u64_archive_recovery_checkpoint_reopen_applies_delta_once() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("u64_char.artc");
    let archive_dir = dir.path().join("wal_archive");
    fs::create_dir_all(&archive_dir).expect("create archive dir");
    write_wal(
        &archive_dir.join("wal_0001.segment"),
        vec![WalRecord::BatchIncrement {
            entries: vec![(b"counter".to_vec(), 4)],
        }],
    );
    {
        let (recovered, _s) = PersistentARTrieChar::<u64>::recover_from_archives(
            &path,
            &archive_dir,
            recovery_config(),
        )
        .expect("recover");
        assert_eq!(
            recovered.get_value("counter"),
            Some(4),
            "recovery accumulates +4 to 4"
        );
        recovered.checkpoint().expect("checkpoint");
    }
    let reopened = PersistentARTrieChar::<u64>::open(&path).expect("reopen");
    assert_eq!(
        reopened.get_value("counter"),
        Some(4),
        "#47: char u64 archive-recovery delta must apply EXACTLY ONCE across recovery→checkpoint→reopen"
    );
}

/// **#48 — byte u64 STEADY-STATE torn-checkpoint guard.** A crash between the publisher's image
/// fsync and the WAL `Checkpoint`-record fsync must NOT double-apply the delta on reopen. Modeled
/// by checkpointing (which writes the image's self-described coverage + the Checkpoint record),
/// then corrupting the LAST WAL record (the Checkpoint) so reopen reads no valid Checkpoint
/// (`checkpoint_lsn=0`) while the durable image's coverage backstops it. GREEN with the #48 fix
/// (`Some(4)`); pre-fix this re-drains the delta → `Some(8)`.
#[test]
fn l48_byte_u64_torn_checkpoint_no_double_apply() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("torn_u64.part");
    {
        let trie = PersistentARTrie::<u64>::create(&path).expect("create");
        trie.set_durability_policy(DurabilityPolicy::Immediate);
        trie.try_increment_cas_durable(b"counter", 4)
            .expect("durable +4");
        assert_eq!(trie.get_value("counter"), Some(4), "counter is 4 after +4");
        trie.checkpoint().expect("checkpoint");
    }
    corrupt_last_wal_record(&path.with_extension("wal"));
    let reopened = PersistentARTrie::<u64>::open(&path).expect("reopen");
    assert_eq!(
        reopened.get_value("counter"),
        Some(4),
        "#48: a torn checkpoint must NOT double-apply the delta (Some(8) = the bug; the image \
         self-describes its coverage so the reopen skips the already-checkpointed delta)"
    );
}

/// **#48 — CHAR u64 STEADY-STATE torn-checkpoint guard (THE PRODUCTION counter monomorph).** The
/// char overlay `<u64>` counter is the libgrammstein steady-state path, so this is the
/// production-critical variant of the byte guard above. A crash between the publisher's image
/// fsync and the WAL `Checkpoint`-record fsync must NOT double-apply the delta on reopen. Modeled
/// by checkpointing (which writes the image's self-described coverage into the shared `FileHeader`
/// + the WAL `Checkpoint` record), then corrupting the LAST WAL record (the `Checkpoint`) so the
/// reopen reads no valid `Checkpoint` (`checkpoint_lsn=0`) while the durable image's coverage
/// (`image_checkpoint_lsn`) backstops it via `eff = max(0, coverage)`. No `enable_lockfree()` —
/// `create` auto-flips to the overlay exactly as production does. GREEN with the #48 fix
/// (`Some(4)`); pre-fix this re-drains the delta → `Some(8)`.
#[test]
fn l48_char_u64_torn_checkpoint_no_double_apply() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("torn_u64_char.artc");
    {
        let trie = PersistentARTrieChar::<u64>::create(&path).expect("create");
        trie.set_durability_policy(DurabilityPolicy::Immediate);
        trie.try_increment_cas_durable("counter", 4)
            .expect("durable +4");
        assert_eq!(trie.get_value("counter"), Some(4), "counter is 4 after +4");
        trie.checkpoint().expect("checkpoint");
    }
    corrupt_last_wal_record(&path.with_extension("wal"));
    let reopened = PersistentARTrieChar::<u64>::open(&path).expect("reopen");
    assert_eq!(
        reopened.get_value("counter"),
        Some(4),
        "#48 (char/production monomorph): a torn checkpoint must NOT double-apply the delta \
         (Some(8) = the bug; the image's `image_checkpoint_lsn` self-describes its coverage so \
         the reopen skips the already-checkpointed delta)"
    );
}

/// **#48 T4 — #47 ∘ #48 compound (closes the case C2 left open).** A post-recovery checkpoint (the
/// #47/C2 scenario) FOLLOWED by a steady-state increment + checkpoint whose `Checkpoint` record is
/// then torn. Both deltas must apply EXACTLY ONCE (4 from archive-recovery + 3 steady-state = 7).
/// Pre-#48 the steady +3 re-drains → 10.
#[test]
fn l48_char_u64_compound_recovery_then_torn_steady_checkpoint() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("compound_u64.artc");
    let archive_dir = dir.path().join("wal_archive");
    fs::create_dir_all(&archive_dir).expect("create archive dir");
    write_wal(
        &archive_dir.join("wal_0001.segment"),
        vec![WalRecord::BatchIncrement {
            entries: vec![(b"counter".to_vec(), 4)],
        }],
    );
    // Phase A (#47): archive-recovery → post-recovery checkpoint records coverage = max_applied_lsn.
    {
        let (recovered, _s) = PersistentARTrieChar::<u64>::recover_from_archives(
            &path,
            &archive_dir,
            recovery_config(),
        )
        .expect("recover");
        assert_eq!(recovered.get_value("counter"), Some(4));
        recovered.checkpoint().expect("post-recovery checkpoint");
    }
    // Phase B (#48/3a): steady durable increment → checkpoint (folds +3, advances image coverage).
    {
        let trie = PersistentARTrieChar::<u64>::open(&path).expect("reopen for steady increment");
        trie.set_durability_policy(DurabilityPolicy::Immediate);
        trie.try_increment_cas_durable("counter", 3)
            .expect("durable +3");
        assert_eq!(trie.get_value("counter"), Some(7));
        trie.checkpoint().expect("steady checkpoint");
    }
    // Tear the steady checkpoint's `Checkpoint` record (the #48 crash window).
    corrupt_last_wal_record(&path.with_extension("wal"));
    let reopened = PersistentARTrieChar::<u64>::open(&path).expect("reopen");
    assert_eq!(
        reopened.get_value("counter"),
        Some(7),
        "#48 T4: recovery delta (+4) AND steady delta (+3) each apply exactly once across a torn \
         steady checkpoint (Some(10) = the steady delta re-draining = the bug)"
    );
}

/// **#49 — steady-state checkpoint-gap watermark under-claim (clean reopen, NO corruption).** This
/// test was authored as #48 T5 (#41 no-panic) and EXPOSED a distinct pre-existing data-loss bug: a
/// live `+4 → checkpoint → +5 → checkpoint → reopen` read `Some(14)` (the +5 re-drained). Root
/// cause: `WalRecord::Checkpoint` consumes a WAL LSN but was never `mark_committed`'d, so the
/// `CommittedWatermark` contiguous prefix stalled behind the first checkpoint record's LSN; the
/// second checkpoint then recorded that stalled watermark as its image coverage → under-claimed →
/// the post-checkpoint delta re-drained. FIX (#49): the retain-WAL overlay publishers now
/// `mark_committed` the Checkpoint record's LSN after its sync. GREEN `Some(9)`; pre-fix `Some(14)`.
/// (Also still guards #41: the two checkpoints must not panic on the capture-order assert.) See
/// docs/design/checkpoint-record-lsn-watermark-gap-49-design-2026-06-08.md.
#[test]
fn l48_char_u64_double_checkpoint_no_capture_order_panic() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("nopanic_u64.artc");
    {
        let trie = PersistentARTrieChar::<u64>::create(&path).expect("create");
        trie.set_durability_policy(DurabilityPolicy::Immediate);
        trie.try_increment_cas_durable("counter", 4)
            .expect("durable +4");
        trie.checkpoint().expect("checkpoint 1");
        trie.try_increment_cas_durable("counter", 5)
            .expect("durable +5");
        trie.checkpoint().expect("checkpoint 2");
        assert_eq!(trie.get_value("counter"), Some(9));
    }
    let reopened = PersistentARTrieChar::<u64>::open(&path).expect("reopen");
    assert_eq!(
        reopened.get_value("counter"),
        Some(9),
        "#48 T5: two image-coverage checkpoints stay correct (no #41 capture-order regression)"
    );
}

/// **#48 T6 — v1 back-compat (end-to-end).** A file whose on-disk `FileHeader` is FORMAT_VERSION 1
/// (no image-coverage field in the checksum; `image_checkpoint_lsn` read as 0) must reopen exactly
/// as pre-#48: `eff = max(wal_record, 0) = wal_record`. The intact WAL `Checkpoint` record carries
/// the skip frontier, so the delta applies once (Some(4)). Proves the `open` path accepts v1.
#[test]
fn l48_char_u64_v1_header_reopens_back_compat() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("v1compat_u64.artc");
    {
        let trie = PersistentARTrieChar::<u64>::create(&path).expect("create");
        trie.set_durability_policy(DurabilityPolicy::Immediate);
        trie.try_increment_cas_durable("counter", 4)
            .expect("durable +4");
        trie.checkpoint().expect("checkpoint");
    }
    rewrite_header_to_v1(&path);
    let reopened = PersistentARTrieChar::<u64>::open(&path).expect("v1 file must reopen");
    assert_eq!(
        reopened.get_value("counter"),
        Some(4),
        "#48 T6: a v1-header file reopens with pre-#48 behavior (eff = the intact WAL Checkpoint record)"
    );
}

/// **#48 T7 — repeated-torn convergence (idempotence).** Two successive {checkpoint → tear the
/// `Checkpoint` record → reopen} cycles must converge to the single correct value (4), never
/// accumulating (8, 12, …). Confirms the image-coverage skip is stable across repeated torn
/// publishes, not a one-shot.
#[test]
fn l48_char_u64_repeated_torn_checkpoint_converges() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("converge_u64.artc");
    {
        let trie = PersistentARTrieChar::<u64>::create(&path).expect("create");
        trie.set_durability_policy(DurabilityPolicy::Immediate);
        trie.try_increment_cas_durable("counter", 4)
            .expect("durable +4");
        trie.checkpoint().expect("checkpoint 1");
    }
    corrupt_last_wal_record(&path.with_extension("wal"));
    {
        let r1 = PersistentARTrieChar::<u64>::open(&path).expect("reopen 1");
        assert_eq!(r1.get_value("counter"), Some(4), "first torn reopen = 4");
        r1.checkpoint().expect("checkpoint 2");
    }
    corrupt_last_wal_record(&path.with_extension("wal"));
    let r2 = PersistentARTrieChar::<u64>::open(&path).expect("reopen 2");
    assert_eq!(
        r2.get_value("counter"),
        Some(4),
        "#48 T7: repeated torn checkpoints converge to 4 (never 8/12 — no re-accumulation)"
    );
}

/// **#48 T8 — torn IMAGE coverage fails closed.** Corrupting the on-disk v2 `FileHeader`'s
/// image-coverage bytes (without repairing the checksum) models a torn coverage write. The reopen
/// MUST fail-closed (`ChecksumMismatch`) rather than read a plausible-but-wrong coverage and
/// silently skip/lose records. (Constraint #1, end-to-end through the `open` path.)
#[test]
fn l48_char_u64_torn_image_coverage_fails_closed() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("tornimage_u64.artc");
    {
        let trie = PersistentARTrieChar::<u64>::create(&path).expect("create");
        trie.set_durability_policy(DurabilityPolicy::Immediate);
        trie.try_increment_cas_durable("counter", 4)
            .expect("durable +4");
        trie.checkpoint().expect("checkpoint");
    }
    corrupt_header_image_coverage(&path);
    let result = PersistentARTrieChar::<u64>::open(&path);
    assert!(
        result.is_err(),
        "#48 T8: a torn image-coverage write must fail-closed (ChecksumMismatch), got Ok"
    );
}

/// **#49 byte mirror** — the byte retain-WAL overlay publisher must also `mark_committed` its
/// Checkpoint record LSN. Live `+4 → checkpoint → +5 → checkpoint → reopen` ⇒ `Some(9)` (pre-fix
/// `Some(14)` = the post-first-checkpoint +5 re-draining).
#[test]
fn l49_byte_u64_double_checkpoint_no_underclaim() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("u49byte.part");
    {
        let trie = PersistentARTrie::<u64>::create(&path).expect("create");
        trie.set_durability_policy(DurabilityPolicy::Immediate);
        trie.try_increment_cas_durable(b"counter", 4)
            .expect("durable +4");
        trie.checkpoint().expect("checkpoint 1");
        trie.try_increment_cas_durable(b"counter", 5)
            .expect("durable +5");
        trie.checkpoint().expect("checkpoint 2");
        assert_eq!(trie.get_value("counter"), Some(9));
    }
    let reopened = PersistentARTrie::<u64>::open(&path).expect("reopen");
    assert_eq!(
        reopened.get_value("counter"),
        Some(9),
        "#49 byte: the post-first-checkpoint +5 must not re-drain (Some(14) = the watermark-gap bug)"
    );
}

/// **#49 N-checkpoint convergence** — the contiguous committed-watermark prefix must keep closing
/// across MANY checkpoints, not just the second. K cycles of `+1 → checkpoint`, then reopen ⇒
/// exactly K. Pre-fix the watermark froze after checkpoint 1, so every delta from +2..=+K re-drained
/// (reopen ≫ K). Guards against a fix that only closes the first gap.
#[test]
fn l49_char_u64_many_checkpoints_each_with_increment_converges() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("u49many.artc");
    const K: u64 = 6;
    {
        let trie = PersistentARTrieChar::<u64>::create(&path).expect("create");
        trie.set_durability_policy(DurabilityPolicy::Immediate);
        for i in 1..=K {
            trie.try_increment_cas_durable("counter", 1)
                .expect("durable +1");
            assert_eq!(
                trie.get_value("counter"),
                Some(i),
                "live count after {i} increments"
            );
            trie.checkpoint().expect("per-step checkpoint");
        }
    }
    let reopened = PersistentARTrieChar::<u64>::open(&path).expect("reopen");
    assert_eq!(
        reopened.get_value("counter"),
        Some(K),
        "#49: K increments each followed by a checkpoint must reopen to exactly K (no re-drain)"
    );
}
