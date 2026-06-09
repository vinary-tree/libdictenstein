//! `PersistentARTrie::compact` — file-rewrite compaction.
//!
//! Split out of byte `dict_impl.rs` (lines ~4894-5136, ~243 LOC) as
//! the fourteenth Phase-5 byte sub-module. Compaction is
//! `MmapDiskManager`-specific (the file-rewrite path uses the
//! default `MmapDiskManager` storage backend). The compaction data
//! carriers (`CompactionConfig`, `CompactionProgress`,
//! `CompactionStats`) live in `super::compaction`.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering as AtomicOrdering;

use super::compaction::{CompactionConfig, CompactionProgress, CompactionStats};
use super::dict_impl::PersistentARTrie;
use super::error::{PersistentARTrieError, Result};
use crate::value::DictionaryValue;

type SerializedSnapshot = BTreeMap<Vec<u8>, Option<Vec<u8>>>;

pub(super) fn wal_sidecar_path(path: &Path) -> PathBuf {
    path.with_extension("wal")
}

pub(super) fn in_place_temp_path(original_path: &Path) -> PathBuf {
    let mut file_name = original_path
        .file_name()
        .map(OsString::from)
        .unwrap_or_else(|| OsString::from("compact"));
    file_name.push(".compacting");
    original_path.with_file_name(file_name)
}

pub(super) fn stale_wal_backup_path(original_wal_path: &Path) -> PathBuf {
    let mut file_name = original_wal_path
        .file_name()
        .map(OsString::from)
        .unwrap_or_else(|| OsString::from("compact.wal"));
    file_name.push(".compacting-stale");
    original_wal_path.with_file_name(file_name)
}

fn remove_file_if_exists(path: &Path, operation: &'static str) -> Result<()> {
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

pub(super) fn recover_in_place_compaction_finalization(original_path: &Path) -> Result<()> {
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

impl<V: DictionaryValue> PersistentARTrie<V> {
    /// Compact the trie, eliminating orphaned nodes and fragmentation.
    ///
    /// Compaction performs a fresh rebuild of the trie by iterating all terms
    /// and inserting them into a new trie. This eliminates:
    ///
    /// - **Intra-Arena fragmentation**: Old node versions orphaned when updated
    /// - **Inter-Arena fragmentation**: Underutilized arenas from append-only allocation
    /// - **File-level fragmentation**: Scattered freed blocks that never coalesce
    ///
    /// # Algorithm
    ///
    /// 1. **Setup**: Record original file size, create new trie at temp path
    /// 2. **Copy**: Iterate all (term, value) pairs and insert into new trie
    /// 3. **Checkpoint**: Persist new trie to disk
    /// 4. **Verify** (optional): Confirm term counts match
    /// 5. **Finalize** (in-place mode): Atomic rename of temp file to original
    pub fn compact<F>(
        &mut self,
        config: CompactionConfig,
        mut progress: F,
    ) -> Result<CompactionStats>
    where
        V: Clone,
        F: FnMut(CompactionProgress),
    {
        use std::time::Instant;

        // **F6 — overlay-aware compaction** (replaces the M3 fail-loud reject).
        // `compaction_snapshot` sources BOTH the term enumeration AND the values from the
        // overlay (the enumeration routes via the `iter_prefix_with_arena` chokepoint; the
        // value read via `overlay_get_value`), so the rebuilt image is FAITHFUL — not the
        // former "counters-lost empty-owned-tree" image the reject guarded against. Since
        // L3.3 deleted the owned tree, the source is ALWAYS overlay-routed: the CX
        // path-compressing serializer (`compact_publish_compressed_overlay`) writes the
        // overlay snapshot DIRECTLY into the staging file as ONE dense (path-compressed)
        // image — no owned staging trie, no `kill_switch_to_owned`, no `insert_impl_no_wal`
        // loop. `compact` takes `&mut self` ⇒ EXCLUSIVE access ⇒ no concurrent writers ⇒
        // the snapshot captures every committed term and there is NO past-snapshot WAL tail
        // to lose on the atomic rename (the data-loss footgun the reject feared).

        let start = Instant::now();

        let original_path = self
            .buffer_manager
            .as_ref()
            .map(|bm| {
                let bm_guard = bm.read();
                std::path::PathBuf::from(bm_guard.storage().path())
            })
            .ok_or_else(|| {
                PersistentARTrieError::io_error(
                    "compact",
                    "",
                    std::io::Error::new(
                        std::io::ErrorKind::Unsupported,
                        "Cannot compact in-memory trie (no disk backing)",
                    ),
                )
            })?;

        let original_bytes = std::fs::metadata(&original_path)
            .map(|m| m.len())
            .unwrap_or(0);

        let estimated_total = self.term_count.load(AtomicOrdering::Acquire) as u64;

        let original_wal_path = wal_sidecar_path(&original_path);
        let (temp_path, is_in_place) = match &config.output_path {
            Some(output) => (output.clone(), false),
            None => (in_place_temp_path(&original_path), true),
        };
        let temp_wal_path = wal_sidecar_path(&temp_path);
        let stale_wal_backup_path = stale_wal_backup_path(&original_wal_path);

        if temp_path == original_path {
            return Err(PersistentARTrieError::InvalidOperation(
                "compaction output path must not be the original trie path".to_string(),
            ));
        }

        if temp_wal_path == original_wal_path {
            return Err(PersistentARTrieError::InvalidOperation(format!(
                "compaction WAL sidecar {} would collide with original WAL {}",
                temp_wal_path.display(),
                original_wal_path.display()
            )));
        }

        if temp_path.exists() {
            std::fs::remove_file(&temp_path).map_err(|e| {
                PersistentARTrieError::io_error("compact", temp_path.display().to_string(), e)
            })?;
        }

        let legacy_temp_path = original_path.with_extension("compacting");
        if is_in_place && legacy_temp_path != original_path && legacy_temp_path != temp_path {
            remove_file_if_exists(&legacy_temp_path, "compact_remove_legacy_temp")?;
        }

        remove_file_if_exists(&temp_wal_path, "compact_remove_temp_wal")?;
        remove_file_if_exists(&stale_wal_backup_path, "compact_remove_stale_wal_backup")?;

        let mut new_trie = PersistentARTrie::<V>::create(&temp_path)?;

        let terms_to_copy = self.compaction_snapshot()?;
        let expected_snapshot = Self::serialized_snapshot_from_terms(&terms_to_copy)?;
        let terms_processed = terms_to_copy.len() as u64;

        // **Copy phase.** `compaction_snapshot()` above already read the ENTIRE source term-set
        // (the "copy" from the live trie); the CX overlay-snapshot serialize (and the owned
        // fallback) then writes it into the new image as ONE bulk operation rather than
        // term-by-term. Emit a single "copying" progress tick for the whole set so the public
        // `CompactionProgress` phase contract (copying → checkpointing → verifying → finalizing)
        // is preserved regardless of which serialize path runs.
        progress(CompactionProgress {
            phase: "copying",
            terms_processed,
            estimated_total,
            percent_complete: 100.0,
        });

        // Value-serialize PRE-CHECK (data-loss guard): fail BEFORE publishing if any value blob
        // cannot serialize (preserves the old per-term insert-loop guard, but up-front).
        for (term, value) in &terms_to_copy {
            if let Some(value) = value {
                if let Err(e) = crate::serialization::bincode_compat::serialize(value) {
                    let _ = remove_file_if_exists(&temp_path, "compact_cleanup_temp");
                    let _ = remove_file_if_exists(&temp_wal_path, "compact_cleanup_temp_wal");
                    return Err(PersistentARTrieError::CheckpointVerificationFailed {
                        reason: format!(
                            "Failed to serialize value for term {:?} during compaction: {}",
                            term, e
                        ),
                    });
                }
            }
        }

        progress(CompactionProgress {
            phase: "checkpointing",
            terms_processed,
            estimated_total,
            percent_complete: 100.0,
        });

        // **L3.3 — CX serialize (unconditional).** The source is ALWAYS overlay-routed (the owned
        // tree is gone), so serialize its overlay snapshot DIRECTLY into the staging file via the
        // path-compressing CX serializer + publish the descriptor (the audited `publish_snapshot`
        // tail) — NO owned staging tree, NO `kill_switch_to_owned`, NO `insert_impl_no_wal`, density
        // preserved.
        match self.lockfree_root.as_ref().and_then(|r| r.load()) {
            Some(source_root) => {
                new_trie.compact_publish_compressed_overlay(&source_root, terms_processed)?;
            }
            None => {
                // Empty overlay (0 terms) → an empty values-bucket image.
                debug_assert_eq!(terms_processed, 0, "empty overlay root implies 0 terms");
                new_trie.compact_publish_empty(terms_processed)?;
            }
        }

        let compacted_bytes = std::fs::metadata(&temp_path).map(|m| m.len()).unwrap_or(0);

        // Release the staging trie's handles + remove its (records-empty) WAL BEFORE verifying, so the
        // verify-reopen + the post-rename reopen both see a clean dense image. The CX path wrote ONLY
        // the arena (the staging trie's in-memory overlay/owned root stays EMPTY), so the verify MUST
        // reopen the published file — an in-process read would see 0 terms and FALSELY fail.
        new_trie.wal_writer = None;
        remove_file_if_exists(&temp_wal_path, "compact_remove_temp_wal")?;
        drop(new_trie);

        if config.verify_after_compact {
            progress(CompactionProgress {
                phase: "verifying",
                terms_processed,
                estimated_total,
                percent_complete: 100.0,
            });

            // Reopen-correspondence: the published dense image must reopen to EXACTLY the source
            // term-set+values. Stronger than an in-process compare — it exercises the real descriptor
            // read + arena load + dense→overlay rebuild that production reopen uses (the first
            // end-to-end production exercise of the CX serialize→publish→reopen cycle).
            let reopened = PersistentARTrie::<V>::open(&temp_path)?;
            let compacted_snapshot = reopened.compaction_serialized_snapshot()?;
            drop(reopened);

            if expected_snapshot != compacted_snapshot {
                let _ = remove_file_if_exists(&temp_path, "compact_cleanup_temp");
                let _ = remove_file_if_exists(&temp_wal_path, "compact_cleanup_temp_wal");

                return Err(PersistentARTrieError::CheckpointVerificationFailed {
                    reason: format!(
                        "Snapshot mismatch after compaction: expected {} terms, got {} terms",
                        expected_snapshot.len(),
                        compacted_snapshot.len()
                    ),
                });
            }
        }

        if is_in_place {
            progress(CompactionProgress {
                phase: "finalizing",
                terms_processed,
                estimated_total,
                percent_complete: 100.0,
            });

            self.buffer_manager = None;
            self.wal_writer = None;
            self.arena_manager = None;

            if original_wal_path.exists() {
                std::fs::rename(&original_wal_path, &stale_wal_backup_path).map_err(|e| {
                    PersistentARTrieError::io_error(
                        "compact_backup_stale_wal",
                        stale_wal_backup_path.display().to_string(),
                        e,
                    )
                })?;
            }

            std::fs::rename(&temp_path, &original_path).map_err(|e| {
                if stale_wal_backup_path.exists() && !original_wal_path.exists() {
                    let _ = std::fs::rename(&stale_wal_backup_path, &original_wal_path);
                }

                PersistentARTrieError::io_error("compact", original_path.display().to_string(), e)
            })?;

            remove_file_if_exists(&original_wal_path, "compact_remove_stale_wal")?;
            remove_file_if_exists(&stale_wal_backup_path, "compact_remove_stale_wal_backup")?;

            // **L3.3 compaction re-point.** The compacted image was written by the CX
            // path-compressing serializer (a dense overlay-regime image) and its staging
            // WAL was removed (line ~293), so the post-rename file is an overlay-regime
            // dense image with a FRESH (records-empty) WAL. `Self::open` reopens it directly
            // onto the overlay via the F5 dense→overlay loader — no Owned→Overlay conversion
            // (the owned tree is gone), no `flip_to_overlay`, no reestablish.
            *self = Self::open(&original_path)?;

            // Post-reopen the trie is overlay-routed unconditionally (every ctor installs the
            // overlay; the owned tree no longer exists).
            debug_assert!(
                self.route_overlay(),
                "L3.3: compact in-place must leave the trie overlay-routed after reopen"
            );
        }

        let duration_ms = start.elapsed().as_millis() as u64;
        let space_savings_percent = if original_bytes > 0 {
            (1.0 - (compacted_bytes as f64 / original_bytes as f64)) * 100.0
        } else {
            0.0
        };

        Ok(CompactionStats {
            terms_copied: terms_processed,
            original_bytes,
            compacted_bytes,
            space_savings_percent,
            duration_ms,
        })
    }

    fn compaction_snapshot(&self) -> Result<Vec<(Vec<u8>, Option<V>)>>
    where
        V: Clone,
    {
        let mut terms = self
            .iter_prefix_with_arena(b"")?
            .unwrap_or_default()
            .into_iter()
            .map(|term| term.term)
            .collect::<Vec<_>>();
        terms.sort();
        terms.dedup();

        Ok(terms
            .into_iter()
            .map(|term| {
                // L3.3: the VALUE read routes to the overlay unconditionally (the owned
                // tree is gone; every `V` is overlay-eligible). `overlay_get_value` returns
                // `Some(Option<V>)` — `Some(None)` for a term-only member (membership
                // preserved, value absent). The outer `None` (overlay didn't handle it) is
                // unreachable for an eligible `V`; default it to "no value".
                let value = self.overlay_get_value(&term).unwrap_or(None);
                (term, value)
            })
            .collect())
    }

    fn serialized_snapshot_from_terms(terms: &[(Vec<u8>, Option<V>)]) -> Result<SerializedSnapshot>
    where
        V: Clone,
    {
        let mut snapshot = BTreeMap::new();

        for (term, value) in terms {
            let serialized = match value {
                Some(value) => {
                    let bytes = crate::serialization::bincode_compat::serialize(value).map_err(
                        |e| PersistentARTrieError::CheckpointVerificationFailed {
                            reason: format!(
                                "Failed to serialize value for term {:?} during compaction verification: {}",
                                term, e
                            ),
                        },
                    )?;
                    // F6: an EMPTY value blob (only `()`/unit serializes to 0 bytes) is
                    // indistinguishable from "no value" on disk — the owned store
                    // re-reads it as `None`. Normalize it here so the verify compares
                    // DISK-FAITHFUL representations: the overlay reads a `V=()` member as
                    // `Some(())` (membership-as-unit), but it persists (and reopens) as
                    // `None`. Non-`()` values never serialize empty, so this is a no-op
                    // for counters / arbitrary `V`.
                    if bytes.is_empty() {
                        None
                    } else {
                        Some(bytes)
                    }
                }
                None => None,
            };
            snapshot.insert(term.clone(), serialized);
        }

        Ok(snapshot)
    }

    fn compaction_serialized_snapshot(&self) -> Result<SerializedSnapshot>
    where
        V: Clone,
    {
        let terms = self.compaction_snapshot()?;
        Self::serialized_snapshot_from_terms(&terms)
    }
}
