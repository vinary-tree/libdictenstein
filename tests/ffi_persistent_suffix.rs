//! Typed persistent suffix families retain native revisions across FFI calls.

#![cfg(feature = "ffi")]

use libdictenstein::ffi::*;
use std::ptr;

#[test]
fn suffix_crash_writer_process() {
    let Some(directory) = std::env::var_os("LIBDICT_TEST_SUFFIX_CRASH_DIR") else {
        return;
    };
    for family in 1..=3 {
        for domain in 1..=2 {
            let path =
                std::path::Path::new(&directory).join(format!("crash-{family}-{domain}.ldict"));
            let path = path.to_str().unwrap().as_bytes();
            unsafe {
                let mut index = ptr::null_mut();
                assert_eq!(
                    ldict_suffix_index_persistent_create(
                        family,
                        domain,
                        path.as_ptr(),
                        path.len(),
                        &mut index,
                    ),
                    LdictStatus::Ok
                );
                insert(index, "committed-π", 19);
            }
        }
    }
    // No destructors or checkpoint calls run on this exit path.
    std::process::exit(0);
}

#[test]
fn committed_suffix_wal_survives_process_exit_for_all_families() {
    let dir = tempfile::tempdir().unwrap();
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "suffix_crash_writer_process", "--nocapture"])
        .env("LIBDICT_TEST_SUFFIX_CRASH_DIR", dir.path())
        .status()
        .unwrap();
    assert!(child.success(), "suffix crash writer failed: {child}");
    for family in 1..=3 {
        for domain in 1..=2 {
            let path = dir.path().join(format!("crash-{family}-{domain}.ldict"));
            let path = path.to_str().unwrap().as_bytes();
            unsafe {
                let mut index = ptr::null_mut();
                let mut report = ptr::null_mut();
                assert_eq!(
                    ldict_suffix_index_persistent_open_with_recovery(
                        family,
                        domain,
                        path.as_ptr(),
                        path.len(),
                        &mut index,
                        &mut report,
                    ),
                    LdictStatus::Ok
                );
                let mut snapshot = ptr::null_mut();
                assert_eq!(
                    ldict_suffix_index_snapshot(index, &mut snapshot),
                    LdictStatus::Ok
                );
                assert!(contains(snapshot, "committed-π"));
                ldict_suffix_snapshot_free(snapshot);
                ldict_recovery_report_free(report);
                ldict_suffix_index_free(index);
            }
        }
    }
}

unsafe fn insert(index: *mut LdictSuffixIndex, text: &str, value: u64) {
    assert_eq!(
        ldict_suffix_index_insert_text_value(index, text.as_ptr(), text.len(), value, 1),
        LdictStatus::Ok
    );
}

unsafe fn contains(snapshot: *mut LdictSuffixSnapshot, text: &str) -> bool {
    let mut found = 0;
    assert_eq!(
        ldict_suffix_snapshot_contains_source(snapshot, text.as_ptr(), text.len(), &mut found),
        LdictStatus::Ok
    );
    found == 1
}

#[test]
fn all_six_persistent_suffix_variants_retain_snapshots_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    for family in 1..=3 {
        for domain in 1..=2 {
            let path = dir.path().join(format!("suffix-{family}-{domain}.ldict"));
            let path = path.to_str().unwrap().as_bytes();
            unsafe {
                let mut index = ptr::null_mut();
                assert_eq!(
                    ldict_suffix_index_persistent_create(
                        family,
                        domain,
                        path.as_ptr(),
                        path.len(),
                        &mut index,
                    ),
                    LdictStatus::Ok
                );
                assert!(!index.is_null());
                insert(index, "banana", 0);
                insert(index, "banané", 7);

                let mut old = ptr::null_mut();
                assert_eq!(
                    ldict_suffix_index_snapshot(index, &mut old),
                    LdictStatus::Ok
                );
                assert!(contains(old, "banana"));
                let mut total = 0;
                assert_eq!(
                    ldict_suffix_snapshot_source_count(old, &mut total),
                    LdictStatus::Ok
                );
                assert_eq!(total, 2);
                let mut records = [LdictSuffixSourceRecord::default(); 2];
                let mut written = 0;
                assert_eq!(
                    ldict_suffix_snapshot_source_page(
                        old,
                        0,
                        records.as_mut_ptr(),
                        records.len(),
                        &mut written,
                        &mut total,
                    ),
                    LdictStatus::Ok
                );
                assert_eq!((written, total), (2, 2));
                let first = std::str::from_utf8(std::slice::from_raw_parts(
                    records[0].data,
                    records[0].len,
                ))
                .unwrap();
                assert_eq!(first, "banana");
                assert_eq!((records[0].value.has_value, records[0].value.value), (1, 0));

                let mut removed = 0;
                assert_eq!(
                    ldict_suffix_index_remove_text(index, b"banana".as_ptr(), 6, &mut removed,),
                    LdictStatus::Ok
                );
                assert_eq!(removed, 1);
                assert!(contains(old, "banana"));
                let mut fresh = ptr::null_mut();
                assert_eq!(
                    ldict_suffix_index_snapshot(index, &mut fresh),
                    LdictStatus::Ok
                );
                assert!(!contains(fresh, "banana"));
                assert!(contains(fresh, "banané"));
                let mut needed = 0;
                assert_eq!(
                    ldict_suffix_index_needs_compaction(index, &mut needed),
                    LdictStatus::Ok
                );
                assert!(needed <= 1);
                assert_eq!(ldict_suffix_index_compact(index), LdictStatus::Ok);
                assert_eq!(ldict_suffix_index_checkpoint(index), LdictStatus::Ok);
                assert_eq!(ldict_suffix_index_close(index), LdictStatus::Ok);
                ldict_suffix_index_free(index);
                assert!(contains(old, "banana"));
                assert_eq!(ldict_suffix_snapshot_close(old), LdictStatus::Ok);
                ldict_suffix_snapshot_free(old);
                ldict_suffix_snapshot_free(fresh);

                index = ptr::null_mut();
                let mut report = ptr::null_mut();
                assert_eq!(
                    ldict_suffix_index_persistent_open_with_recovery(
                        family,
                        domain,
                        path.as_ptr(),
                        path.len(),
                        &mut index,
                        &mut report,
                    ),
                    LdictStatus::Ok
                );
                assert!(!report.is_null());
                ldict_recovery_report_free(report);
                let mut replacement = ptr::null_mut();
                assert_eq!(
                    ldict_suffix_index_persistent_create(
                        family,
                        domain,
                        path.as_ptr(),
                        path.len(),
                        &mut replacement,
                    ),
                    LdictStatus::InvalidArgument
                );
                assert!(replacement.is_null());
                let mut reopened = ptr::null_mut();
                assert_eq!(
                    ldict_suffix_index_snapshot(index, &mut reopened),
                    LdictStatus::Ok
                );
                assert!(!contains(reopened, "banana"));
                assert!(contains(reopened, "banané"));
                ldict_suffix_snapshot_free(reopened);
                ldict_suffix_index_free(index);
            }
        }
    }
}

#[test]
fn invalid_persistent_suffix_domain_and_family_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("invalid.ldict");
    let path = path.to_str().unwrap().as_bytes();
    unsafe {
        let mut index = ptr::null_mut();
        assert_eq!(
            ldict_suffix_index_persistent_create(1, 3, path.as_ptr(), path.len(), &mut index),
            LdictStatus::Unsupported
        );
        assert!(index.is_null());
        assert_eq!(
            ldict_suffix_index_persistent_create(99, 1, path.as_ptr(), path.len(), &mut index),
            LdictStatus::InvalidArgument
        );
        assert!(index.is_null());
    }
}
