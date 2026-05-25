//! Executable correspondence checks for WAL segment lifecycle safety.
//!
//! These tests cover the implementation side of the segment-lifecycle model:
//! segment ordering must follow WAL LSNs, rotations must preserve the global
//! LSN stream, reopen must continue after retained archived segments, and
//! archive pruning limits must apply to async synced segments.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::{
    collect_all_segments, AsyncWalConfig, AsyncWalWriter, WalConfig, WalReader, WalRecord,
    WalWriter,
};
use std::path::Path;
use std::time::Duration;
use tempfile::tempdir;

fn insert_record(term: &str) -> WalRecord {
    WalRecord::Insert {
        term: term.as_bytes().to_vec(),
        value: None,
    }
}

fn first_lsn(path: &Path) -> u64 {
    let mut reader = WalReader::new(path).expect("open WAL segment");
    let (lsn, _) = reader
        .next_record()
        .expect("segment contains at least one record")
        .expect("read first WAL record");
    lsn
}

fn archive_segment_count(archive_dir: &Path) -> usize {
    std::fs::read_dir(archive_dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().extension().and_then(|ext| ext.to_str()) == Some("segment"))
        .count()
}

#[test]
fn collected_segments_use_lsn_order_when_filenames_disagree() {
    let dir = tempdir().expect("tempdir");
    let wal_path = dir.path().join("active.wal");
    let archive_dir = dir.path().join("archive");
    let pending_dir = dir.path().join("pending");
    std::fs::create_dir_all(&archive_dir).expect("archive dir");

    {
        let late_by_lsn = WalWriter::create(archive_dir.join("wal_a.segment")).expect("late WAL");
        late_by_lsn.set_min_lsn(10);
        late_by_lsn
            .append(insert_record("late"))
            .expect("append late");
        late_by_lsn.sync().expect("sync late");
    }

    {
        let early_by_lsn = WalWriter::create(archive_dir.join("wal_z.segment")).expect("early WAL");
        early_by_lsn
            .append(insert_record("early"))
            .expect("append early");
        early_by_lsn.sync().expect("sync early");
    }

    let archive_config = WalConfig {
        archive_enabled: true,
        archive_dir: archive_dir.clone(),
        max_segments: 16,
        max_archive_bytes: 16 * 1024 * 1024,
    };
    let async_config = AsyncWalConfig {
        pending_dir,
        ..Default::default()
    };

    let segments =
        collect_all_segments(&wal_path, &archive_config, &async_config).expect("collect segments");
    let lsns: Vec<_> = segments.iter().map(|path| first_lsn(path)).collect();

    assert_eq!(lsns, vec![1, 10]);
}

#[test]
fn async_rotation_preserves_monotonic_lsn_across_archive_and_active() {
    let dir = tempdir().expect("tempdir");
    let wal_path = dir.path().join("rotation.wal");
    let archive_dir = dir.path().join("archive");
    let async_config = AsyncWalConfig {
        pending_dir: dir.path().join("pending"),
        ..Default::default()
    };
    let archive_config = WalConfig {
        archive_enabled: true,
        archive_dir: archive_dir.clone(),
        max_segments: 16,
        max_archive_bytes: 16 * 1024 * 1024,
    };

    let wal = AsyncWalWriter::create(&wal_path, async_config, archive_config.clone())
        .expect("create WAL");
    let first = wal.append(insert_record("before")).expect("append before");

    let archived = wal
        .rotate_to_archive(&archive_config)
        .expect("rotate")
        .expect("archive enabled");
    assert!(archived.exists(), "rotated segment should be archived");

    let second = wal.append(insert_record("after")).expect("append after");

    assert_eq!(second, first + 1);
}

#[test]
fn async_reopen_after_archive_continues_after_retained_lsn() {
    let dir = tempdir().expect("tempdir");
    let wal_path = dir.path().join("reopen.wal");
    let archive_dir = dir.path().join("archive");
    let pending_dir = dir.path().join("pending");
    let archive_config = WalConfig {
        archive_enabled: true,
        archive_dir: archive_dir.clone(),
        max_segments: 16,
        max_archive_bytes: 16 * 1024 * 1024,
    };
    let async_config = AsyncWalConfig {
        pending_dir,
        ..Default::default()
    };

    let first = {
        let wal = AsyncWalWriter::create(&wal_path, async_config.clone(), archive_config.clone())
            .expect("create WAL");
        let first = wal.append(insert_record("before")).expect("append before");
        wal.rotate_to_archive(&archive_config)
            .expect("rotate")
            .expect("archive enabled");
        first
    };

    let reopened = AsyncWalWriter::open_or_create(&wal_path, async_config, archive_config)
        .expect("reopen WAL");
    assert_eq!(
        reopened.synced_lsn(),
        first,
        "retained archived segments are already durable after reopen"
    );
    let second = reopened
        .append(insert_record("after-reopen"))
        .expect("append after reopen");

    assert_eq!(second, first + 1);
}

#[test]
fn async_archive_prunes_synced_segments_to_configured_limit() {
    let dir = tempdir().expect("tempdir");
    let wal_path = dir.path().join("prune.wal");
    let archive_dir = dir.path().join("archive");
    let async_config = AsyncWalConfig {
        pending_dir: dir.path().join("pending"),
        ..Default::default()
    };
    let archive_config = WalConfig {
        archive_enabled: true,
        archive_dir: archive_dir.clone(),
        max_segments: 2,
        max_archive_bytes: u64::MAX,
    };

    let wal = AsyncWalWriter::create(&wal_path, async_config, archive_config).expect("create WAL");

    for i in 0..5 {
        wal.append(insert_record(&format!("term-{i}")))
            .expect("append");
        let handle = wal.sync_async().expect("sync async");
        handle.wait().expect("wait for sync");
        std::thread::sleep(Duration::from_millis(5));
    }

    let count = archive_segment_count(&archive_dir);
    assert!(
        count <= 2,
        "archive pruning should keep at most 2 segments, found {count}"
    );
}
