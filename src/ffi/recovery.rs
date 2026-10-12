//! Owned recovery reports for persistent binding constructors.

use super::{
    binding, boundary, domain, persistent_path, persistent_wal_config, LdictBinding,
    LdictDictionary, LdictStatus,
};
use crate::bindings::{BindingUnitDomain, PersistentARTrieBinding};
use crate::persistent_artrie::{RecoveryMode, RecoveryReport};
use std::ptr;

/// Opaque report whose borrowed text ranges remain valid until free.
pub struct LdictRecoveryReport {
    report: RecoveryReport,
    corrupted_file: Option<Vec<u8>>,
    corruption_reason: Option<Vec<u8>>,
    archive_segments: Vec<Vec<u8>>,
}

impl LdictRecoveryReport {
    fn new(report: RecoveryReport) -> Self {
        let corrupted_file = report
            .corrupted_file
            .as_ref()
            .map(|path| path.as_os_str().as_encoded_bytes().to_vec());
        let corruption_reason = report
            .corruption_reason
            .as_ref()
            .map(|reason| reason.as_bytes().to_vec());
        let archive_segments = report
            .archive_segments_used
            .iter()
            .map(|path| path.as_os_str().as_encoded_bytes().to_vec())
            .collect();
        Self {
            report,
            corrupted_file,
            corruption_reason,
            archive_segments,
        }
    }
}

/// Free a report after copying any borrowed path or diagnostic bytes.
///
/// # Safety
/// `report` must be null or an allocated report that has not been freed.
#[cfg(feature = "persistent-artrie")]
#[no_mangle]
pub unsafe extern "C" fn ldict_recovery_report_free(report: *mut LdictRecoveryReport) {
    if !report.is_null() {
        drop(Box::from_raw(report));
    }
}

/// Read the complete scalar recovery summary and archive-segment count.
///
/// # Safety
/// All pointers must be valid and outputs writable.
#[cfg(feature = "persistent-artrie")]
#[no_mangle]
pub unsafe extern "C" fn ldict_recovery_report_summary(
    report: *const LdictRecoveryReport,
    out_mode: *mut u32,
    out_records: *mut u64,
    out_terms: *mut u64,
    out_duration_ms: *mut u64,
    out_archive_count: *mut usize,
) -> LdictStatus {
    boundary(|| {
        if !out_mode.is_null() {
            out_mode.write(0);
        }
        if !out_records.is_null() {
            out_records.write(0);
        }
        if !out_terms.is_null() {
            out_terms.write(0);
        }
        if !out_duration_ms.is_null() {
            out_duration_ms.write(0);
        }
        if !out_archive_count.is_null() {
            out_archive_count.write(0);
        }
        if report.is_null()
            || out_mode.is_null()
            || out_records.is_null()
            || out_terms.is_null()
            || out_duration_ms.is_null()
            || out_archive_count.is_null()
        {
            return Err((
                LdictStatus::NullPointer,
                "recovery report pointer is null".into(),
            ));
        }
        let report = &*report;
        out_mode.write(match report.report.mode {
            RecoveryMode::Normal => 0,
            RecoveryMode::RebuildFromWal => 1,
            RecoveryMode::RepairInPlace => 2,
            RecoveryMode::CreatedNew => 3,
        });
        out_records.write(report.report.records_replayed);
        out_terms.write(report.report.terms_recovered);
        out_duration_ms.write(report.report.duration_ms);
        out_archive_count.write(report.archive_segments.len());
        Ok(LdictStatus::Ok)
    })
}

/// Borrow one exact byte range from a report. Field 1 is the optional
/// corrupted-file path; field 2 is the optional reason; field 3 selects an
/// archive segment by `index`. All bytes remain valid until report free.
///
/// # Safety
/// All pointers must be valid and outputs writable.
#[cfg(feature = "persistent-artrie")]
#[no_mangle]
pub unsafe extern "C" fn ldict_recovery_report_text(
    report: *const LdictRecoveryReport,
    field: u32,
    index: usize,
    out_data: *mut *const u8,
    out_len: *mut usize,
    out_present: *mut u8,
) -> LdictStatus {
    boundary(|| {
        if !out_data.is_null() {
            out_data.write(ptr::null());
        }
        if !out_len.is_null() {
            out_len.write(0);
        }
        if !out_present.is_null() {
            out_present.write(0);
        }
        if report.is_null() || out_data.is_null() || out_len.is_null() || out_present.is_null() {
            return Err((
                LdictStatus::NullPointer,
                "recovery text pointer is null".into(),
            ));
        }
        let report = &*report;
        let bytes = match field {
            1 if index == 0 => report.corrupted_file.as_deref(),
            2 if index == 0 => report.corruption_reason.as_deref(),
            3 => match report.archive_segments.get(index) {
                Some(bytes) => Some(bytes.as_slice()),
                None => return Ok(LdictStatus::End),
            },
            1 | 2 => {
                return Err((
                    LdictStatus::InvalidArgument,
                    "scalar recovery field requires index zero".into(),
                ))
            }
            _ => {
                return Err((
                    LdictStatus::InvalidArgument,
                    "unknown recovery field".into(),
                ))
            }
        };
        if let Some(bytes) = bytes {
            out_data.write(bytes.as_ptr());
            out_len.write(bytes.len());
            out_present.write(1);
        }
        Ok(LdictStatus::Ok)
    })
}

unsafe fn publish_open_result(
    result: Result<(PersistentARTrieBinding, RecoveryReport), crate::bindings::BindingError>,
    out_dictionary: *mut *mut LdictDictionary,
    out_report: *mut *mut LdictRecoveryReport,
) -> Result<LdictStatus, (LdictStatus, String)> {
    let (dictionary, report) = binding(result)?;
    out_dictionary.write(Box::into_raw(Box::new(LdictDictionary::new(
        LdictBinding::Persistent(dictionary),
    ))));
    out_report.write(Box::into_raw(Box::new(LdictRecoveryReport::new(report))));
    Ok(LdictStatus::Ok)
}

/// Open a byte, Unicode, or u64 persistent ARTrie and return the full native
/// recovery report. Profile 1 is valid only for u64 keys.
///
/// # Safety
/// Input byte range and both output pointers must be valid.
#[cfg(feature = "persistent-artrie")]
#[no_mangle]
pub unsafe extern "C" fn ldict_persistent_artrie_open_with_recovery(
    unit_domain: u32,
    profile: u32,
    path_data: *const u8,
    path_len: usize,
    out_dictionary: *mut *mut LdictDictionary,
    out_report: *mut *mut LdictRecoveryReport,
) -> LdictStatus {
    boundary(|| {
        if out_dictionary.is_null() || out_report.is_null() {
            return Err((LdictStatus::NullPointer, "recovery output is null".into()));
        }
        out_dictionary.write(ptr::null_mut());
        out_report.write(ptr::null_mut());
        if profile > 1 {
            return Err((
                LdictStatus::InvalidArgument,
                "invalid persistent profile".into(),
            ));
        }
        let path = persistent_path(path_data, path_len)?;
        publish_open_result(
            PersistentARTrieBinding::open_with_recovery(
                path,
                domain(unit_domain)?,
                profile == 1,
                None,
            ),
            out_dictionary,
            out_report,
        )
    })
}

/// Open a Unicode-scalar persistent ARTrie with custom WAL archive policy and
/// return the complete native recovery report.
///
/// # Safety
/// Input byte ranges and both output pointers must be valid.
#[cfg(feature = "persistent-artrie")]
#[no_mangle]
pub unsafe extern "C" fn ldict_persistent_artrie_char_open_with_wal_recovery(
    path_data: *const u8,
    path_len: usize,
    archive_enabled: u8,
    archive_dir_data: *const u8,
    archive_dir_len: usize,
    max_segments: usize,
    max_archive_bytes: u64,
    out_dictionary: *mut *mut LdictDictionary,
    out_report: *mut *mut LdictRecoveryReport,
) -> LdictStatus {
    boundary(|| {
        if out_dictionary.is_null() || out_report.is_null() {
            return Err((LdictStatus::NullPointer, "recovery output is null".into()));
        }
        out_dictionary.write(ptr::null_mut());
        out_report.write(ptr::null_mut());
        let path = persistent_path(path_data, path_len)?;
        let config = persistent_wal_config(
            archive_enabled,
            archive_dir_data,
            archive_dir_len,
            max_segments,
            max_archive_bytes,
        )?;
        publish_open_result(
            PersistentARTrieBinding::open_with_recovery(
                path,
                BindingUnitDomain::UnicodeScalar,
                false,
                Some(config),
            ),
            out_dictionary,
            out_report,
        )
    })
}

/// Open a persistent vocabulary with its complete native recovery report.
///
/// # Safety
/// Input byte range and both output pointers must be valid.
#[cfg(feature = "persistent-artrie")]
#[no_mangle]
pub unsafe extern "C" fn ldict_persistent_vocab_open_with_recovery(
    path_data: *const u8,
    path_len: usize,
    out_dictionary: *mut *mut LdictDictionary,
    out_report: *mut *mut LdictRecoveryReport,
) -> LdictStatus {
    boundary(|| {
        if out_dictionary.is_null() || out_report.is_null() {
            return Err((LdictStatus::NullPointer, "recovery output is null".into()));
        }
        out_dictionary.write(ptr::null_mut());
        out_report.write(ptr::null_mut());
        let path = persistent_path(path_data, path_len)?;
        publish_open_result(
            PersistentARTrieBinding::open_vocab_with_recovery(path),
            out_dictionary,
            out_report,
        )
    })
}
