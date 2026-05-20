//! `PersistentARTrie::compact` — file-rewrite compaction.
//!
//! Split out of byte `dict_impl.rs` (lines ~4894-5136, ~243 LOC) as
//! the fourteenth Phase-5 byte sub-module. Compaction is
//! `MmapDiskManager`-specific (the file-rewrite path uses the
//! default `MmapDiskManager` storage backend). The compaction data
//! carriers (`CompactionConfig`, `CompactionProgress`,
//! `CompactionStats`) live in `super::compaction`.

use std::sync::atomic::Ordering as AtomicOrdering;

use log::warn;

use super::compaction::{CompactionConfig, CompactionProgress, CompactionStats};
use super::dict_impl::PersistentARTrie;
use super::error::{PersistentARTrieError, Result};
use crate::value::DictionaryValue;

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

        let (temp_path, is_in_place) = match &config.output_path {
            Some(output) => (output.clone(), false),
            None => (original_path.with_extension("compacting"), true),
        };

        if temp_path.exists() {
            std::fs::remove_file(&temp_path).map_err(|e| {
                PersistentARTrieError::io_error("compact", temp_path.display().to_string(), e)
            })?;
        }

        let temp_wal_path = temp_path.with_extension("wal");
        if temp_wal_path.exists() {
            let _ = std::fs::remove_file(&temp_wal_path);
        }

        let mut new_trie = PersistentARTrie::<V>::create(&temp_path)?;

        let mut terms_processed = 0u64;

        let terms_to_copy: Vec<(Vec<u8>, V)> = self
            .iter_prefix_with_values(b"")
            .map(|iter| iter.collect())
            .unwrap_or_default();

        for (term, value) in terms_to_copy {
            let term_str = match std::str::from_utf8(&term) {
                Ok(s) => s,
                Err(_) => {
                    warn!("Non-UTF8 term encountered during compaction: {:?}", term);
                    continue;
                }
            };

            new_trie.insert_with_value(term_str, value);
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

            let original_count = self.term_count.load(AtomicOrdering::Acquire);
            let compacted_count = new_trie.term_count.load(AtomicOrdering::Acquire);

            if original_count != compacted_count {
                drop(new_trie);
                let _ = std::fs::remove_file(&temp_path);
                let _ = std::fs::remove_file(&temp_wal_path);

                return Err(PersistentARTrieError::CheckpointVerificationFailed {
                    reason: format!(
                        "Term count mismatch after compaction: expected {}, got {}",
                        original_count, compacted_count
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

            drop(new_trie);

            self.buffer_manager = None;
            self.wal_writer = None;
            self.arena_manager = None;

            std::fs::rename(&temp_path, &original_path).map_err(|e| {
                PersistentARTrieError::io_error("compact", original_path.display().to_string(), e)
            })?;

            let original_wal = original_path.with_extension("wal");
            let _ = std::fs::remove_file(&temp_wal_path);

            let new_wal_path = temp_path.with_extension("wal");
            if new_wal_path.exists() {
                let _ = std::fs::rename(&new_wal_path, &original_wal);
            }

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
}
