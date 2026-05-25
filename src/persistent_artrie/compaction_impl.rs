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

fn wal_sidecar_path(path: &Path) -> PathBuf {
    path.with_extension("wal")
}

fn in_place_temp_path(original_path: &Path) -> PathBuf {
    let mut file_name = original_path
        .file_name()
        .map(OsString::from)
        .unwrap_or_else(|| OsString::from("compact"));
    file_name.push(".compacting");
    original_path.with_file_name(file_name)
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

        let mut new_trie = PersistentARTrie::<V>::create(&temp_path)?;

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

            std::fs::rename(&temp_path, &original_path).map_err(|e| {
                PersistentARTrieError::io_error("compact", original_path.display().to_string(), e)
            })?;

            remove_file_if_exists(&original_wal_path, "compact_remove_stale_wal")?;

            *self = Self::open(&original_path)?;
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
                let value = self.get_value_impl(&term);
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
                Some(value) => Some(crate::serialization::bincode_compat::serialize(value).map_err(
                    |e| PersistentARTrieError::CheckpointVerificationFailed {
                        reason: format!(
                            "Failed to serialize value for term {:?} during compaction verification: {}",
                            term, e
                        ),
                    },
                )?),
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
