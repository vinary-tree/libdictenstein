//! Shared file-path + crash-recovery helpers for in-place compaction (byte + char).
//!
//! In-place compaction writes a compacted image to a `<name>.compacting` temp file (plus its
//! `.compacting.wal` sidecar), then atomically renames it over the original — first stashing the
//! original's WAL to `<name>.wal.compacting-stale` so a crash mid-rename is recoverable. These
//! helpers are pure `&Path` manipulation + the recovery finalizer; they are identical for the byte
//! (`.artb`) and char (`.artc`) tries, so they live here ONCE (lifted out of `compaction_impl.rs`)
//! and are shared by both variants' `compact()` and reopen paths.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::error::{PersistentARTrieError, Result};

/// The WAL sidecar path for a trie data file (`<name>` → `<name>.wal`).
pub(crate) fn wal_sidecar_path(path: &Path) -> PathBuf {
    path.with_extension("wal")
}

/// The in-place compaction temp path (`<name>` → `<name>.compacting`).
pub(crate) fn in_place_temp_path(original_path: &Path) -> PathBuf {
    let mut file_name = original_path
        .file_name()
        .map(OsString::from)
        .unwrap_or_else(|| OsString::from("compact"));
    file_name.push(".compacting");
    original_path.with_file_name(file_name)
}

/// The stashed-original-WAL backup path used across the atomic rename
/// (`<name>.wal` → `<name>.wal.compacting-stale`).
pub(crate) fn stale_wal_backup_path(original_wal_path: &Path) -> PathBuf {
    let mut file_name = original_wal_path
        .file_name()
        .map(OsString::from)
        .unwrap_or_else(|| OsString::from("compact.wal"));
    file_name.push(".compacting-stale");
    original_wal_path.with_file_name(file_name)
}

/// Remove `path` if it exists, mapping a real I/O error (not `NotFound`) to `operation`.
pub(crate) fn remove_file_if_exists(path: &Path, operation: &'static str) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(PersistentARTrieError::io_error(
            operation,
            path.display().to_string(),
            e,
        )),
    }
}

/// Finalize (roll forward or back) an in-place compaction that a crash interrupted, restoring a
/// consistent state before the trie is reopened. Idempotent + a no-op when no compaction was in
/// flight (the stale-WAL backup marker is absent). Called at the head of every reopen (byte + char).
pub(crate) fn recover_in_place_compaction_finalization(original_path: &Path) -> Result<()> {
    let original_wal_path = wal_sidecar_path(original_path);
    let stale_wal_backup_path = stale_wal_backup_path(&original_wal_path);

    if !stale_wal_backup_path.exists() {
        return Ok(());
    }

    let temp_path = in_place_temp_path(original_path);
    let temp_wal_path = wal_sidecar_path(&temp_path);

    if temp_path.exists() {
        if !original_wal_path.exists() {
            std::fs::rename(&stale_wal_backup_path, &original_wal_path).map_err(|e| {
                PersistentARTrieError::io_error(
                    "compact_restore_stale_wal",
                    original_wal_path.display().to_string(),
                    e,
                )
            })?;
        } else {
            remove_file_if_exists(&stale_wal_backup_path, "compact_remove_duplicate_stale_wal")?;
        }

        remove_file_if_exists(&temp_wal_path, "compact_recover_remove_temp_wal")?;
        remove_file_if_exists(&temp_path, "compact_recover_remove_temp")?;
    } else {
        remove_file_if_exists(&original_wal_path, "compact_recover_remove_stale_wal")?;
        remove_file_if_exists(
            &stale_wal_backup_path,
            "compact_recover_remove_stale_wal_backup",
        )?;
    }

    Ok(())
}
