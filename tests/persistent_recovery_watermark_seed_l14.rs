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
