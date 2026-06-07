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
        // `compaction_snapshot` now sources BOTH the term enumeration AND the values
        // from the overlay when it serves reads (the enumeration already routes via the
        // `iter_prefix_with_arena` chokepoint; the value read is routed below through
        // `overlay_get_value`), so the rebuilt image is FAITHFUL — not the former
        // "counters-lost empty-owned-tree" image the reject guarded against. The
        // STAGING trie stays OWNED (path-compressed ⇒ dense; the overlay's
        // un-path-compressed one-node-per-unit spine would BLOAT the compacted file),
        // and the in-place reopen RE-FLIPS to the overlay so the write regime is
        // preserved across compaction (mirrors `open`'s reestablish, mmap_ctor.rs).
        // `compact` takes `&mut self` ⇒ EXCLUSIVE access ⇒ no concurrent writers ⇒ the
        // snapshot captures every committed term and there is NO past-snapshot WAL tail
        // to lose on the atomic rename (the data-loss footgun the reject feared).
        let was_overlay = self.route_overlay();

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
        // **M4b:** `create` now create-flips eligible V (`{(), i64}`) to the lock-free
        // overlay. The compaction STAGING trie must be OWNED: it is populated below via
        // `insert_impl_no_wal` (owned-tree writes) and then `checkpoint()`ed — under the
        // overlay the inserts would land in the owned tree while `checkpoint()` captured
        // the EMPTY overlay, so verification would read 0 terms and every compaction of
        // an eligible-V trie would fail (silent total loss of the compacted image).
        // Force the staging trie to the owned regime (the kill-switch also restamps the
        // fresh temp WAL Owned, so the post-rename reopen of the compacted file stays
        // owned until the caller's own create-flip path governs it). This keeps the
        // proven owned compaction pipeline intact for the eligible monomorphs.
        new_trie.kill_switch_to_owned();

        let mut terms_processed = 0u64;

        let terms_to_copy = self.compaction_snapshot()?;
        let expected_snapshot = Self::serialized_snapshot_from_terms(&terms_to_copy)?;

        for (term, value) in terms_to_copy {
            if let Some(ref value) = value {
                if let Err(e) = crate::serialization::bincode_compat::serialize(value) {
                    drop(new_trie);
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

            if !new_trie.insert_impl_no_wal(&term, value) {
                drop(new_trie);
                let _ = remove_file_if_exists(&temp_path, "compact_cleanup_temp");
                let _ = remove_file_if_exists(&temp_wal_path, "compact_cleanup_temp_wal");
                return Err(PersistentARTrieError::CheckpointVerificationFailed {
                    reason: format!("Failed to copy term {:?} during compaction", term),
                });
            }
            terms_processed += 1;

            if config.progress_interval > 0
                && terms_processed % config.progress_interval as u64 == 0
            {
                let percent = if estimated_total > 0 {
                    (terms_processed as f32 / estimated_total as f32) * 100.0
                } else {
                    100.0
                };
                progress(CompactionProgress {
                    phase: "copying",
                    terms_processed,
                    estimated_total,
                    percent_complete: percent,
                });
            }
        }

        progress(CompactionProgress {
            phase: "checkpointing",
            terms_processed,
            estimated_total,
            percent_complete: 100.0,
        });
        new_trie.checkpoint()?;

        let compacted_bytes = std::fs::metadata(&temp_path).map(|m| m.len()).unwrap_or(0);

        if config.verify_after_compact {
            progress(CompactionProgress {
                phase: "verifying",
                terms_processed,
                estimated_total,
                percent_complete: 100.0,
            });

            let compacted_snapshot = new_trie.compaction_serialized_snapshot()?;

            if expected_snapshot != compacted_snapshot {
                drop(new_trie);
                let _ = std::fs::remove_file(&temp_path);
                let _ = std::fs::remove_file(&temp_wal_path);

                return Err(PersistentARTrieError::CheckpointVerificationFailed {
                    reason: format!(
                        "Snapshot mismatch after compaction: expected {} terms, got {} terms",
                        expected_snapshot.len(),
                        compacted_snapshot.len()
                    ),
                });
            }
        }

        new_trie.wal_writer = None;
        remove_file_if_exists(&temp_wal_path, "compact_remove_temp_wal")?;

        if is_in_place {
            progress(CompactionProgress {
                phase: "finalizing",
                terms_processed,
                estimated_total,
                percent_complete: 100.0,
            });

            drop(new_trie);

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

            *self = Self::open(&original_path)?;

            // F6: the staging trie was OWNED (for path-compressed density), so the
            // dense image `*self` just reopened is `OwnedTree`. If the trie was
            // overlay-routed before compaction, RE-FLIP to preserve the regime — the
            // SAME two calls `open` uses for an Overlay-regime file (mmap_ctor.rs:602):
            // `flip_to_overlay` (restamps the fresh post-reopen WAL Overlay, lsn==1)
            // then `reestablish_overlay_dispatch` (publishes the recovered owned tree
            // into the overlay, clears owned LAST). Durable: the next reopen sees the
            // Overlay stamp and auto-flips.
            if was_overlay {
                let took = self.flip_to_overlay();
                debug_assert!(
                    took,
                    "F6: compact in-place must re-flip eligible-V to overlay"
                );
                self.reestablish_overlay_dispatch()?;
            }
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
                // F6: route the VALUE read to the overlay when it serves reads (the
                // enumeration above already routed via `iter_prefix_with_arena`).
                // `overlay_get_value` returns `Some(Option<V>)` when the overlay
                // handled the term — including `Some(None)` for a term-only member
                // (membership preserved, value absent) — and `None` only for an
                // ineligible `V`, where the owned read is the correct fallback.
                let value = if self.route_overlay() {
                    self.overlay_get_value(&term)
                        .unwrap_or_else(|| self.get_value_impl(&term))
                } else {
                    self.get_value_impl(&term)
                };
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
