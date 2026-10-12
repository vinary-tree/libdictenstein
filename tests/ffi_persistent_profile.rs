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

#[test]
fn vocabulary_auto_ids_and_ordered_batch_are_returned_in_one_crossing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vocabulary.ldict");
    let path = path.to_str().unwrap().as_bytes();
    unsafe {
        let mut dictionary = ptr::null_mut();
        assert_eq!(
            ldict_persistent_vocab_create(path.as_ptr(), path.len(), &mut dictionary),
            LdictStatus::Ok
        );
        let mut index = u64::MAX;
        assert_eq!(
            ldict_vocab_insert_term(dictionary, b"alpha".as_ptr(), 5, &mut index),
            LdictStatus::Ok
        );
        assert_eq!(index, 0);
        assert_eq!(
            ldict_vocab_insert_term(dictionary, b"alpha".as_ptr(), 5, &mut index),
            LdictStatus::Ok
        );
        assert_eq!(index, 0);

        let beta = "β".as_bytes();
        let gamma = b"gamma";
        let keys = [
            LdictTextKey {
                data: beta.as_ptr(),
                len: beta.len(),
            },
            LdictTextKey {
                data: b"alpha".as_ptr(),
                len: 5,
            },
            LdictTextKey {
                data: gamma.as_ptr(),
                len: gamma.len(),
            },
        ];
        let mut assigned = [u64::MAX; 3];
        let mut completed = usize::MAX;
        assert_eq!(
            ldict_vocab_insert_batch(
                dictionary,
                keys.as_ptr(),
                keys.len(),
                assigned.as_mut_ptr(),
                &mut completed,
            ),
            LdictStatus::Ok
        );
        assert_eq!(completed, 3);
        assert_eq!(assigned, [1, 0, 2]);

        let invalid = [0xff];
        let malformed = [LdictTextKey {
            data: invalid.as_ptr(),
            len: 1,
        }];
        assigned = [u64::MAX; 3];
        completed = usize::MAX;
        assert_eq!(
            ldict_vocab_insert_batch(
                dictionary,
                malformed.as_ptr(),
                1,
                assigned.as_mut_ptr(),
                &mut completed
            ),
            LdictStatus::InvalidUtf8
        );
        assert_eq!((assigned[0], completed), (0, 0));
        assert_eq!(ldict_dictionary_checkpoint(dictionary), LdictStatus::Ok);
        ldict_dictionary_free(dictionary);

        dictionary = ptr::null_mut();
        assert_eq!(
            ldict_persistent_vocab_open(path.as_ptr(), path.len(), &mut dictionary),
            LdictStatus::Ok
        );
        let mut found = 0;
        let mut value = LdictOptionalU64::default();
        assert_eq!(
            ldict_dictionary_get_text(
                dictionary,
                beta.as_ptr(),
                beta.len(),
                &mut found,
                &mut value
            ),
            LdictStatus::Ok
        );
        assert_eq!((found, value.has_value, value.value), (1, 1, 1));
        let mut buffer = [0u8; 8];
        let mut len = 0;
        assert_eq!(
            ldict_vocab_get_term(
                dictionary,
                2,
                buffer.as_mut_ptr(),
                buffer.len(),
                &mut len,
                &mut found
            ),
            LdictStatus::Ok
        );
        assert_eq!((found, &buffer[..len]), (1, gamma.as_slice()));
        ldict_dictionary_free(dictionary);
    }
}
