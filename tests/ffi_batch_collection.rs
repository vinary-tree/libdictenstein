//! Ordered, one-crossing batch lookup and removal at the C boundary.

#![cfg(feature = "ffi")]

mod ffi_common;

use ffi_common::{insert_text, insert_u64, DictGuard, DOMAIN_BYTE, DOMAIN_U64, DOMAIN_UNICODE};
use libdictenstein::ffi::{
    ldict_dictionary_get_text_batch, ldict_dictionary_get_u64_batch,
    ldict_dictionary_remove_text_batch, ldict_dictionary_remove_u64_batch, LdictOptionalU64,
    LdictStatus, LdictTextKey, LdictU64Key,
};

fn text_descriptors(keys: &[Vec<u8>]) -> Vec<LdictTextKey> {
    keys.iter()
        .map(|key| LdictTextKey {
            data: key.as_ptr(),
            len: key.len(),
        })
        .collect()
}

fn u64_descriptors(keys: &[Vec<u64>]) -> Vec<LdictU64Key> {
    keys.iter()
        .map(|key| LdictU64Key {
            data: key.as_ptr(),
            len: key.len(),
        })
        .collect()
}

#[test]
fn text_batches_preserve_order_values_duplicates_and_empty_keys() {
    for domain in [DOMAIN_BYTE, DOMAIN_UNICODE] {
        let dictionary = DictGuard::dynamic(domain);
        assert_eq!(insert_text(dictionary.ptr(), b"", None).0, LdictStatus::Ok);
        assert_eq!(
            insert_text(dictionary.ptr(), b"a", Some(0)).0,
            LdictStatus::Ok
        );
        assert_eq!(
            insert_text(dictionary.ptr(), b"b", Some(9)).0,
            LdictStatus::Ok
        );

        let keys = vec![
            b"b".to_vec(),
            b"a".to_vec(),
            b"a".to_vec(),
            b"missing".to_vec(),
            Vec::new(),
        ];
        let descriptors = text_descriptors(&keys);
        let mut found = vec![7u8; keys.len()];
        let mut values = vec![LdictOptionalU64::default(); keys.len()];
        assert_eq!(
            unsafe {
                ldict_dictionary_get_text_batch(
                    dictionary.ptr(),
                    descriptors.as_ptr(),
                    descriptors.len(),
                    found.as_mut_ptr(),
                    values.as_mut_ptr(),
                )
            },
            LdictStatus::Ok
        );
        assert_eq!(found, [1, 1, 1, 0, 1]);
        assert_eq!(
            values
                .iter()
                .map(|value| value.has_value)
                .collect::<Vec<_>>(),
            [1, 1, 1, 0, 0]
        );
        assert_eq!(
            values.iter().map(|value| value.value).collect::<Vec<_>>(),
            [9, 0, 0, 0, 0]
        );

        let mut removed = vec![7u8; keys.len()];
        assert_eq!(
            unsafe {
                ldict_dictionary_remove_text_batch(
                    dictionary.ptr(),
                    descriptors.as_ptr(),
                    descriptors.len(),
                    removed.as_mut_ptr(),
                )
            },
            LdictStatus::Ok
        );
        assert_eq!(removed, [1, 1, 0, 0, 1]);
        assert_eq!(
            unsafe {
                ldict_dictionary_get_text_batch(
                    dictionary.ptr(),
                    descriptors.as_ptr(),
                    descriptors.len(),
                    found.as_mut_ptr(),
                    values.as_mut_ptr(),
                )
            },
            LdictStatus::Ok
        );
        assert_eq!(found, [0, 0, 0, 0, 0]);
        assert_eq!(
            unsafe {
                ldict_dictionary_get_text_batch(
                    dictionary.ptr(),
                    std::ptr::null(),
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            LdictStatus::Ok
        );
    }
}

#[test]
fn u64_batches_preserve_order_and_optional_values() {
    let dictionary = DictGuard::dynamic(DOMAIN_U64);
    assert_eq!(insert_u64(dictionary.ptr(), &[], None).0, LdictStatus::Ok);
    assert_eq!(
        insert_u64(dictionary.ptr(), &[0, 2], Some(0)).0,
        LdictStatus::Ok
    );
    let keys = vec![vec![0, 2], vec![5], vec![], vec![0, 2]];
    let descriptors = u64_descriptors(&keys);
    let mut found = vec![7u8; keys.len()];
    let mut values = vec![LdictOptionalU64::default(); keys.len()];
    assert_eq!(
        unsafe {
            ldict_dictionary_get_u64_batch(
                dictionary.ptr(),
                descriptors.as_ptr(),
                keys.len(),
                found.as_mut_ptr(),
                values.as_mut_ptr(),
            )
        },
        LdictStatus::Ok
    );
    assert_eq!(found, [1, 0, 1, 1]);
    assert_eq!(
        values
            .iter()
            .map(|value| value.has_value)
            .collect::<Vec<_>>(),
        [1, 0, 0, 1]
    );
    let mut removed = vec![7u8; keys.len()];
    assert_eq!(
        unsafe {
            ldict_dictionary_remove_u64_batch(
                dictionary.ptr(),
                descriptors.as_ptr(),
                keys.len(),
                removed.as_mut_ptr(),
            )
        },
        LdictStatus::Ok
    );
    assert_eq!(removed, [1, 0, 1, 0]);
}

#[test]
fn validation_precedes_removal_and_does_not_publish_partial_lookup() {
    let unicode = DictGuard::dynamic(DOMAIN_UNICODE);
    assert_eq!(
        insert_text(unicode.ptr(), b"valid", Some(1)).0,
        LdictStatus::Ok
    );
    let keys = vec![b"valid".to_vec(), vec![0xff]];
    let descriptors = text_descriptors(&keys);
    let mut removed = [7u8; 2];
    assert_eq!(
        unsafe {
            ldict_dictionary_remove_text_batch(
                unicode.ptr(),
                descriptors.as_ptr(),
                descriptors.len(),
                removed.as_mut_ptr(),
            )
        },
        LdictStatus::InvalidUtf8
    );
    assert_eq!(removed, [7, 7]);
    let valid_keys = [b"valid".to_vec()];
    let valid = text_descriptors(&valid_keys);
    let mut found = [0u8];
    let mut value = [LdictOptionalU64::default()];
    assert_eq!(
        unsafe {
            ldict_dictionary_get_text_batch(
                unicode.ptr(),
                valid.as_ptr(),
                1,
                found.as_mut_ptr(),
                value.as_mut_ptr(),
            )
        },
        LdictStatus::Ok
    );
    assert_eq!(found, [1]);
    assert_eq!(value[0].value, 1);

    let wrong_domain_keys = [vec![1]];
    let wrong_domain = u64_descriptors(&wrong_domain_keys);
    assert_eq!(
        unsafe {
            ldict_dictionary_get_u64_batch(
                unicode.ptr(),
                wrong_domain.as_ptr(),
                1,
                found.as_mut_ptr(),
                value.as_mut_ptr(),
            )
        },
        LdictStatus::DomainMismatch
    );
    assert_eq!(found, [1]);
    assert_eq!(
        unsafe {
            ldict_dictionary_get_text_batch(
                unicode.ptr(),
                descriptors.as_ptr(),
                2,
                std::ptr::null_mut(),
                value.as_mut_ptr(),
            )
        },
        LdictStatus::NullPointer
    );
}
