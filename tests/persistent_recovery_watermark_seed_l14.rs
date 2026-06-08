//! **L1 recovery regression guard (Slice 3 / Level 3).** A corruption/archive-rebuilt
//! trie carrying a BatchIncrement DELTA must NOT double-apply the delta across
//! `recovery → checkpoint() → drop → reopen`.
//!
//! ## ⚠️ Why these are GREEN for V=i64 — a MASKING ARTIFACT, not correctness
//! These guards use `V=i64` and CURRENTLY PASS, but **only because the overlay
//! BatchIncrement-DELTA applier (`apply_recovered_operation_overlay`, flip.rs:1166) is
//! u64-monomorph-only** (`overlay_publish_counter` `Any`-downcasts to `<u64,S>`,
//! overlay_write_mode.rs:547) and **silently NO-OPS for i64** — so the reopen's archive
//! re-drain of the delta is dropped, leaving the counter at the checkpoint-image value
//! (4). The "no double-apply" is the bug masking the bug.
//!
//! ## CONFIRMED PRODUCTION DATA-LOSS BUG (u64 counter monomorph)
//! For `V=u64` (libgrammstein's n-gram count monomorph) the delta arm ALWAYS works
//! (`increment_cas`), so it is NOT masked. A `diag` run confirmed: u64
//! `open_with_recovery_config` → `checkpoint()` → reopen yields **Some(8)** for a
//! recovered `+4` (DOUBLE-APPLIED). Root cause: the recovery ctors lack the open_inner
//! "F7 FIX C" watermark seed, so the post-recovery checkpoint records `checkpoint_lsn=0`
//! and the surviving archive RE-DRAINS the delta on reopen. The naive seed
//! `mark_committed(max_lsn_in_segments)` is INVALID here — it violates the #41
//! capture-ordering invariant (`watermark ≤ synced WAL frontier`; the recovery ctor's
//! fresh WAL frontier is 0), panicking at overlay_checkpoint.rs:295. The fix is a deep,
//! #41-aware watermark/archive-lifecycle change (its own red-team). See
//! docs/design/slice3-l1-recovery-redirect-design-2026-06-08.md +
//! docs/design/recovery-checkpoint-reopen-double-apply-bug-2026-06-08.md.
//!
//! These i64 guards are retained: they currently pass via the masking no-op, and will
//! FLIP TO RED the moment the overlay delta arm is genericized over V (the L1 redirect
//! precondition) — correctly catching the double-apply until the #41-aware seed lands.
//!
//! Uses a BARE BatchIncrement (no preceding SET) so a re-drain IS visible
//! (a `[SET, +delta]` archive self-cancels because the re-drained SET masks the delta).
#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::{
    PersistentARTrie, RecoveryMode, WalConfig, WalRecord, WalWriter,
};
use libdictenstein::persistent_artrie_char::PersistentARTrieChar;
use libdictenstein::MappedDictionary;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::tempdir;

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
        "L1: the BatchIncrement delta must be applied EXACTLY ONCE across \
         recovery→checkpoint→reopen (Some(8) = the hypothesized watermark-0 re-drain bug, \
         REFUTED — the checkpoint image subsumes the archive via LWW generation)"
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
