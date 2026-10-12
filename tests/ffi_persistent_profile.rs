//! The u64 CX prefix budget is an on-disk format choice, not a query hint.

#![cfg(feature = "ffi")]

use libdictenstein::ffi::*;
use std::ptr;

#[test]
fn crash_writer_process() {
    let Some(path) = std::env::var_os("LIBDICT_TEST_CRASH_WRITER_PATH") else {
        return;
    };
    let path = path.to_str().unwrap().as_bytes();
    let key = "durable-π".as_bytes();
    unsafe {
        let mut dictionary = ptr::null_mut();
        assert_eq!(
            ldict_persistent_artrie_create_profile(
                2,
                0,
                path.as_ptr(),
                path.len(),
                &mut dictionary,
            ),
            LdictStatus::Ok
        );
        let mut inserted = 0;
        assert_eq!(
            ldict_dictionary_insert_text_value(
                dictionary,
                key.as_ptr(),
                key.len(),
                42,
                1,
                &mut inserted,
            ),
            LdictStatus::Ok
        );
        assert_eq!(inserted, 1);
    }
    // Simulate process death without Drop, close, or checkpoint.
    std::process::exit(0);
}

#[test]
fn committed_wal_write_survives_process_exit_without_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("crash-recovery.ldict");
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "crash_writer_process", "--nocapture"])
        .env("LIBDICT_TEST_CRASH_WRITER_PATH", &path)
        .status()
        .unwrap();
    assert!(child.success(), "crash writer failed: {child}");

    let path = path.to_str().unwrap().as_bytes();
    let key = "durable-π".as_bytes();
    unsafe {
        let mut dictionary = ptr::null_mut();
        let mut report = ptr::null_mut();
        assert_eq!(
            ldict_persistent_artrie_open_with_recovery(
                2,
                0,
                path.as_ptr(),
                path.len(),
                &mut dictionary,
                &mut report,
            ),
            LdictStatus::Ok
        );
        let mut found = 0;
        let mut value = LdictOptionalU64::default();
        assert_eq!(
            ldict_dictionary_get_text(dictionary, key.as_ptr(), key.len(), &mut found, &mut value),
            LdictStatus::Ok
        );
        assert_eq!((found, value.has_value, value.value), (1, 1, 42));
        ldict_recovery_report_free(report);
        ldict_dictionary_free(dictionary);
    }
}

#[test]
fn prefix3_profile_survives_checkpoint_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("prefix3.ldict");
    let path = path.to_str().unwrap().as_bytes();

    unsafe {
        let mut dictionary = ptr::null_mut();
        assert_eq!(
            ldict_persistent_artrie_create_profile(
                3,
                2,
                path.as_ptr(),
                path.len(),
                &mut dictionary
            ),
            LdictStatus::InvalidArgument
        );
        assert!(dictionary.is_null());
        assert_eq!(
            ldict_persistent_artrie_create_profile(
                1,
                1,
                path.as_ptr(),
                path.len(),
                &mut dictionary
            ),
            LdictStatus::Unsupported
        );
        assert!(dictionary.is_null());

        assert_eq!(
            ldict_persistent_artrie_create_profile(
                3,
                1,
                path.as_ptr(),
                path.len(),
                &mut dictionary
            ),
            LdictStatus::Ok
        );
        let key = [0, u64::MAX, 3];
        let mut inserted = 0;
        assert_eq!(
            ldict_dictionary_insert_u64_value(
                dictionary,
                key.as_ptr(),
                key.len(),
                0,
                1,
                &mut inserted,
            ),
            LdictStatus::Ok
        );
        assert_eq!(inserted, 1);
        assert_eq!(ldict_dictionary_checkpoint(dictionary), LdictStatus::Ok);
        ldict_dictionary_free(dictionary);

        dictionary = ptr::null_mut();
        assert_eq!(
            ldict_persistent_artrie_open_profile(3, 1, path.as_ptr(), path.len(), &mut dictionary),
            LdictStatus::Ok
        );
        let mut found = 0;
        let mut value = LdictOptionalU64::default();
        assert_eq!(
            ldict_dictionary_get_u64(dictionary, key.as_ptr(), key.len(), &mut found, &mut value),
            LdictStatus::Ok
        );
        assert_eq!((found, value.has_value, value.value), (1, 1, 0));
        ldict_dictionary_free(dictionary);
    }
}

#[test]
fn recovery_report_is_owned_and_preserves_creation_mode() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("recover-prefix3.ldict");
    let path = path.to_str().unwrap().as_bytes();

    unsafe {
        let mut dictionary = ptr::null_mut();
        let mut report = ptr::null_mut();
        assert_eq!(
            ldict_persistent_artrie_open_with_recovery(
                3,
                1,
                path.as_ptr(),
                path.len(),
                &mut dictionary,
                &mut report,
            ),
            LdictStatus::Ok
        );
        assert!(!dictionary.is_null());
        assert!(!report.is_null());
        let (mut mode, mut records, mut terms, mut duration, mut segments) = (99, 99, 99, 99, 99);
        assert_eq!(
            ldict_recovery_report_summary(
                report,
                &mut mode,
                &mut records,
                &mut terms,
                &mut duration,
                &mut segments,
            ),
            LdictStatus::Ok
        );
        assert_eq!((mode, records, terms, segments), (3, 0, 0, 0));
        let mut data = ptr::null();
        let mut len = 99;
        let mut present = 99;
        assert_eq!(
            ldict_recovery_report_text(report, 1, 0, &mut data, &mut len, &mut present),
            LdictStatus::Ok
        );
        assert_eq!((data, len, present), (ptr::null(), 0, 0));
        assert_eq!(
            ldict_recovery_report_text(report, 3, 0, &mut data, &mut len, &mut present),
            LdictStatus::End
        );
        ldict_recovery_report_free(report);

        let key = [0, u64::MAX];
        let mut inserted = 0;
        assert_eq!(
            ldict_dictionary_insert_u64_value(
                dictionary,
                key.as_ptr(),
                key.len(),
                5,
                1,
                &mut inserted
            ),
            LdictStatus::Ok
        );
        assert_eq!(ldict_dictionary_checkpoint(dictionary), LdictStatus::Ok);
        ldict_dictionary_free(dictionary);
    }
}
