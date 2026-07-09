//! `PersistentARTrieChar::compact` — file-rewrite compaction (the char twin of byte's
//! [`crate::persistent_artrie::compaction_impl`]).
//!
//! Incremental checkpointing (checkpoint-while-inserting) accumulates dead space: [dirty-skip]
//! (`compressed_serialize`) elides re-appending UNCHANGED nodes, but every CHANGED node — and the
//! eviction-path-copied spine — is still appended to a fresh arena slot, superseding its prior
//! slot, and the append-only arena never reclaims the superseded bytes. `compact()` rebuilds a
//! fresh, dense image containing only the LIVE term set and atomically swaps it in, reclaiming all
//! of that dead space.
//!
//! # Correctness on EVICTED tries
//!
//! The source term set is enumerated with the FAULTING overlay reader
//! ([`PersistentARTrieChar::iter_prefix_with_values`] → the Phase-A `overlay_collect_with_values`
//! that loads `Child::OnDisk` subtrees), so a trie whose cold nodes were evicted to disk is
//! compacted WITHOUT losing the evicted terms. A value-faithful verify-by-reopen confirms the
//! rebuilt image before the swap. (`self.len()` / `overlay_len` is NOT used as a completeness oracle
//! because it counts only RESIDENT finals — it undercounts evicted terms; the faulting enumeration,
//! proven complete by the Phase-A prefix-fault work + the evict→compact→reopen test, is the source
//! of truth.)
//!
//! # RAM cost — NOT RAM-bounded
//!
//! Compaction MATERIALIZES the full live set: the enumerated `(term, value)` vector, the rebuilt
//! fully-resident overlay, and the verify snapshots all reside in memory at once. Peak memory is a
//! small multiple of the LIVE data size — it is **not** bounded by any `resident_budget_bytes`.
//! A caller must have `RAM ≳ live-set`. A post-enumeration guard (based on the ACTUAL live-data
//! size) fails loud rather than OOM if the rebuild+verify would exceed available memory; for a trie
//! whose live set is LARGER than RAM, compaction is not possible via this path
//! (a streaming disk-to-disk compactor would be required — see the design doc). This is why
//! compaction is an EXPLICIT, caller-invoked maintenance operation (never auto-triggered inside
//! `checkpoint()`).

use std::time::Instant;

use crate::persistent_artrie::compaction::{CompactionConfig, CompactionProgress, CompactionStats};
use crate::persistent_artrie::compaction_paths::{
    in_place_temp_path, remove_file_if_exists, stale_wal_backup_path, wal_sidecar_path,
};
use crate::persistent_artrie::core::durability::DurabilityPolicy;
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::persistent_artrie::WalConfig;
use crate::value::DictionaryValue;

/// Peak-memory multiplier over the ACTUAL enumerated live-data size, for the post-enumeration RAM
/// guard. Accounts for the concurrent in-RAM copies at the rebuild+verify peak: the enumerated
/// `(term, value)` vector, the `expected` verify snapshot, the rebuilt fully-resident overlay, and
/// the reopened `got` snapshot — plus per-node structural overhead. Using the enumerated data size
/// (not `self.len()`, which undercounts evicted terms, nor the file size, which over-counts dead
/// space) keeps the estimate accurate.
const COMPACT_PEAK_MEM_FACTOR: usize = 4;

impl<V: DictionaryValue> super::PersistentARTrieChar<V> {
    /// Compact the char trie in place (or to `config.output_path`), reclaiming the dead space that
    /// incremental checkpointing accumulates. See the [module docs](self) for correctness on
    /// evicted tries and the (non-RAM-bounded) memory cost.
    ///
    /// Returns [`CompactionStats`] (terms copied, before/after sizes, space saved, duration).
    pub fn compact<F>(
        &mut self,
        config: CompactionConfig,
        mut progress: F,
    ) -> Result<CompactionStats>
    where
        V: Clone,
        F: FnMut(CompactionProgress),
    {
        let start = Instant::now();

        // ---- Resolve paths ----
        let original_path = self
            .buffer_manager
            .as_ref()
            .map(|bm| std::path::PathBuf::from(bm.read().storage().path()))
            .ok_or_else(|| {
                PersistentARTrieError::io_error(
                    "compact",
                    "",
                    std::io::Error::new(
                        std::io::ErrorKind::Unsupported,
                        "Cannot compact an in-memory trie (no disk backing)",
                    ),
                )
            })?;
        let original_bytes = std::fs::metadata(&original_path)
            .map(|m| m.len())
            .unwrap_or(0);
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

        // Clean any stale temp artifacts from a previous aborted run.
        if temp_path.exists() {
            std::fs::remove_file(&temp_path).map_err(|e| {
                PersistentARTrieError::io_error("compact", temp_path.display().to_string(), e)
            })?;
        }
        remove_file_if_exists(&temp_wal_path, "compact_remove_temp_wal")?;
        remove_file_if_exists(&stale_wal_backup_path, "compact_remove_stale_wal_backup")?;

        // ---- Enumerate the full live term set (FAULTS evicted subtrees via Phase-A) ----
        // `iter_prefix_with_values` loads `Child::OnDisk` subtrees, so evicted terms ARE included;
        // the end-to-end evict→compact→reopen test is the completeness witness. NB: `self.len()` /
        // `overlay_len` counts only RESIDENT finals, so it CANNOT serve as an "expected" total for
        // an evicted trie — the faulting enumeration is the source of truth.
        let terms: Vec<(String, V)> = self.iter_prefix_with_values("")?.unwrap_or_default();
        let terms_processed = terms.len() as u64;
        progress(CompactionProgress {
            phase: "copying",
            terms_processed,
            estimated_total: terms_processed,
            percent_complete: 100.0,
        });

        // Value-serialize PRE-CHECK + capture the DISK-faithful expected snapshot (fail before
        // publishing if any value blob cannot serialize).
        let mut expected: std::collections::BTreeMap<String, Vec<u8>> =
            std::collections::BTreeMap::new();
        let mut live_data_bytes: usize = 0;
        for (term, value) in &terms {
            let bytes = crate::serialization::bincode_compat::serialize(value).map_err(|e| {
                PersistentARTrieError::CheckpointVerificationFailed {
                    reason: format!(
                        "Failed to serialize value for term {:?} during compaction: {}",
                        term, e
                    ),
                }
            })?;
            live_data_bytes = live_data_bytes.saturating_add(term.len() + bytes.len());
            expected.insert(term.clone(), bytes);
        }

        // ---- RAM guard (post-enumeration, ACCURATE) ----
        // The rebuild + verify hold several copies of the live data in memory at once. Compaction
        // is NOT RAM-bounded: fail LOUD here rather than OOM mid-rebuild. Based on the ACTUAL
        // enumerated data size (see COMPACT_PEAK_MEM_FACTOR). Enumeration already fit in RAM, so at
        // least ~1× live is available; this guards the additional rebuild+verify copies.
        {
            let available = {
                let mut sys = sysinfo::System::new();
                sys.refresh_memory();
                sys.available_memory() as usize
            };
            let projected_peak = live_data_bytes.saturating_mul(COMPACT_PEAK_MEM_FACTOR);
            if available > 0 && projected_peak > available {
                return Err(PersistentARTrieError::InvalidOperation(format!(
                    "compaction of {} terms needs ~{} MiB but only ~{} MiB is available; compact() \
                     is not RAM-bounded (needs RAM ≳ live-set). Compact on a larger host, or accept \
                     dirty-skip's bounded growth without compacting.",
                    terms_processed,
                    projected_peak / (1024 * 1024),
                    available / (1024 * 1024),
                )));
            }
        }

        // ---- Rebuild a fresh, DENSE, fully-resident trie at the temp path ----
        progress(CompactionProgress {
            phase: "checkpointing",
            terms_processed,
            estimated_total: terms_processed,
            percent_complete: 100.0,
        });
        {
            let mut new_trie = Self::create_with_config(&temp_path, WalConfig::no_archive())?;
            new_trie.set_durability_policy(DurabilityPolicy::Immediate);
            new_trie.install_overlay();
            for (term, value) in terms {
                new_trie.insert_with_value(&term, value)?;
            }
            new_trie.checkpoint()?;
            // Release the staging trie's WAL (records-empty after checkpoint) before verify/rename.
            new_trie.wal_writer = None;
            remove_file_if_exists(&temp_wal_path, "compact_remove_temp_wal")?;
        }

        let compacted_bytes = std::fs::metadata(&temp_path).map(|m| m.len()).unwrap_or(0);

        // ---- Verify by reopen: the published image must reopen to the EXACT term+value set ----
        if config.verify_after_compact {
            progress(CompactionProgress {
                phase: "verifying",
                terms_processed,
                estimated_total: terms_processed,
                percent_complete: 100.0,
            });
            let reopened = Self::open(&temp_path)?;
            let mut got: std::collections::BTreeMap<String, Vec<u8>> =
                std::collections::BTreeMap::new();
            for (term, value) in reopened.iter_prefix_with_values("")?.unwrap_or_default() {
                let bytes =
                    crate::serialization::bincode_compat::serialize(&value).map_err(|e| {
                        PersistentARTrieError::CheckpointVerificationFailed {
                            reason: format!(
                                "Failed to serialize reopened value for {:?}: {}",
                                term, e
                            ),
                        }
                    })?;
                got.insert(term, bytes);
            }
            drop(reopened);
            if got != expected {
                let _ = remove_file_if_exists(&temp_path, "compact_cleanup_temp");
                let _ = remove_file_if_exists(&temp_wal_path, "compact_cleanup_temp_wal");
                return Err(PersistentARTrieError::CheckpointVerificationFailed {
                    reason: format!(
                        "Snapshot mismatch after compaction: expected {} terms, got {} terms",
                        expected.len(),
                        got.len()
                    ),
                });
            }
        }

        // ---- Finalize: in-place atomic rename with WAL-sidecar stash + crash recovery ----
        if is_in_place {
            progress(CompactionProgress {
                phase: "finalizing",
                terms_processed,
                estimated_total: terms_processed,
                percent_complete: 100.0,
            });
            // Release this trie's file handles BEFORE renaming over its file.
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

            // Reopen `self` onto the compacted image (a records-empty WAL; the F5 dense→overlay
            // loader rebuilds a fully-resident overlay).
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

#[cfg(test)]
mod tests {
    use super::super::PersistentARTrieChar;
    use crate::persistent_artrie::compaction::CompactionConfig;
    use crate::persistent_artrie::compaction_paths::{in_place_temp_path, stale_wal_backup_path};
    use crate::persistent_artrie::core::durability::DurabilityPolicy;
    use crate::persistent_artrie::eviction::EvictionConfig;
    use crate::persistent_artrie::WalConfig;
    use proptest::prelude::*;

    fn scratch(prefix: &str) -> tempfile::TempDir {
        std::fs::create_dir_all("target/test-tmp").ok();
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in("target/test-tmp")
            .expect("scratch tempdir under target/test-tmp")
    }

    /// Reclamation: repeatedly OVERWRITING values supersedes their on-disk node versions (dead
    /// space that dirty-skip does NOT reclaim, since each overwrite genuinely changes the node), so
    /// the file bloats to several times the live size. `compact()` rebuilds a dense image and
    /// reclaims that dead space — compacted << original — while preserving the final values.
    #[test]
    fn compact_reclaims_dead_space() {
        let dir = scratch("compact-reclaim");
        let path = dir.path().join("c.artc");
        let n: i64 = 2_000;
        let mut owned: PersistentARTrieChar<i64> =
            PersistentARTrieChar::create_with_config(&path, WalConfig::no_archive())
                .expect("create");
        owned.set_durability_policy(DurabilityPolicy::Immediate);
        owned.install_overlay();
        // No eviction: isolate DEAD-SPACE reclamation. Insert, then overwrite ALL values 8 times
        // (each round + checkpoint supersedes the previous on-disk node versions).
        for i in 0..n {
            owned
                .insert_with_value(&format!("symbol_{i:08}"), i)
                .expect("insert");
        }
        owned.checkpoint().expect("checkpoint");
        for round in 1..=8i64 {
            for i in 0..n {
                owned
                    .insert_with_value(&format!("symbol_{i:08}"), i + round * 10_000_000)
                    .expect("overwrite");
            }
            owned.checkpoint().expect("checkpoint");
        }
        // Restore final values.
        for i in 0..n {
            owned
                .insert_with_value(&format!("symbol_{i:08}"), i)
                .expect("restore");
        }
        owned.checkpoint().expect("final checkpoint");
        let original_bytes = std::fs::metadata(&path).expect("stat").len();

        let stats = owned
            .compact(CompactionConfig::default(), |_p| {})
            .expect("compact");
        assert_eq!(stats.terms_copied, n as u64);
        assert!(
            stats.compacted_bytes < original_bytes,
            "compaction must reclaim dead space (original {} → compacted {})",
            original_bytes,
            stats.compacted_bytes
        );
        assert!(
            stats.space_savings_percent > 0.0,
            "space_savings_percent should be positive, got {}",
            stats.space_savings_percent
        );
        for i in 0..n {
            assert_eq!(
                owned.get_value(&format!("symbol_{i:08}")),
                Some(i),
                "term {i} lost after compact"
            );
        }
    }

    /// Evicted-trie correctness: compacting a trie whose cold nodes were evicted to disk preserves
    /// EVERY term (the faulting enumeration recovers evicted subtrees) and never grows the file.
    #[test]
    fn compact_evicted_trie_preserves_all_terms() {
        let dir = scratch("compact-evicted");
        let path = dir.path().join("c.artc");
        let n: i64 = 4_000;
        {
            let mut owned: PersistentARTrieChar<i64> =
                PersistentARTrieChar::create_with_config(&path, WalConfig::no_archive())
                    .expect("create");
            owned.set_durability_policy(DurabilityPolicy::Immediate);
            owned.install_overlay();
            owned
                .bench_enable_eviction(EvictionConfig {
                    resident_budget_bytes: Some(32 * 1024),
                    ..EvictionConfig::without_memory_monitor()
                })
                .expect("enable eviction");
            for i in 0..n {
                owned
                    .insert_with_value(&format!("symbol_{i:08}"), i)
                    .expect("insert");
                if i % 200 == 0 {
                    owned.checkpoint().expect("checkpoint");
                }
            }
            owned.checkpoint().expect("final checkpoint");
            let original_bytes = std::fs::metadata(&path).expect("stat").len();

            let stats = owned
                .compact(CompactionConfig::default(), |_p| {})
                .expect("compact");
            assert_eq!(
                stats.terms_copied, n as u64,
                "all (incl. evicted) terms copied"
            );
            assert!(
                stats.compacted_bytes <= original_bytes,
                "compaction must never grow the file ({} → {})",
                original_bytes,
                stats.compacted_bytes
            );

            // Lossless in-process (`self` reopened onto the compacted image).
            for i in 0..n {
                assert_eq!(
                    owned.get_value(&format!("symbol_{i:08}")),
                    Some(i),
                    "term {i} lost after compact"
                );
            }
        }
        // Durable across process boundary.
        let reopened: PersistentARTrieChar<i64> =
            PersistentARTrieChar::open(&path).expect("reopen compacted");
        for i in 0..n {
            assert_eq!(
                reopened.get_value(&format!("symbol_{i:08}")),
                Some(i),
                "term {i} lost after reopen"
            );
        }
    }

    /// `output_path` mode writes a separate compacted file and leaves the original untouched.
    #[test]
    fn compact_to_output_path_leaves_original_intact() {
        let dir = scratch("compact-outpath");
        let path = dir.path().join("src.artc");
        let out = dir.path().join("out.artc");
        {
            let mut owned: PersistentARTrieChar<u64> =
                PersistentARTrieChar::create_with_config(&path, WalConfig::no_archive())
                    .expect("create");
            owned.set_durability_policy(DurabilityPolicy::Immediate);
            owned.install_overlay();
            for i in 0..500u64 {
                owned
                    .insert_with_value(&format!("t{i:05}"), i)
                    .expect("insert");
            }
            owned.checkpoint().expect("checkpoint");

            let stats = owned
                .compact(
                    CompactionConfig {
                        output_path: Some(out.clone()),
                        ..CompactionConfig::default()
                    },
                    |_p| {},
                )
                .expect("compact");
            assert_eq!(stats.terms_copied, 500);

            // Original still open + intact (output-path mode does NOT reopen `self`).
            for i in 0..500u64 {
                assert_eq!(owned.get_value(&format!("t{i:05}")), Some(i));
            }
        }
        // Both files reopen to the full set.
        let orig: PersistentARTrieChar<u64> =
            PersistentARTrieChar::open(&path).expect("reopen original");
        let compacted: PersistentARTrieChar<u64> =
            PersistentARTrieChar::open(&out).expect("reopen output");
        for i in 0..500u64 {
            assert_eq!(
                orig.get_value(&format!("t{i:05}")),
                Some(i),
                "orig term {i}"
            );
            assert_eq!(
                compacted.get_value(&format!("t{i:05}")),
                Some(i),
                "compacted term {i}"
            );
        }
    }

    /// Crash-recovery: a crash AFTER the original WAL was stashed + the temp file written, but
    /// BEFORE the atomic rename, is rolled back on the next reopen (the recovery finalizer runs at
    /// the head of `open`). No terms are lost.
    #[test]
    fn compact_crash_before_rename_recovers_on_reopen() {
        let dir = scratch("compact-crash");
        let path = dir.path().join("c.artc");
        {
            let mut t: PersistentARTrieChar<i64> =
                PersistentARTrieChar::create_with_config(&path, WalConfig::no_archive())
                    .expect("create");
            t.set_durability_policy(DurabilityPolicy::Immediate);
            for i in 0..60i64 {
                t.insert_with_value(&format!("k{i:04}"), i).expect("insert");
            }
            t.checkpoint().expect("checkpoint");
        }
        // Fabricate an interrupted-compaction state: stash the WAL to the stale-backup and write a
        // partial temp file, as `compact()` would just before the (never-reached) rename.
        let wal = path.with_extension("wal");
        let stale = stale_wal_backup_path(&wal);
        let temp = in_place_temp_path(&path);
        if wal.exists() {
            std::fs::rename(&wal, &stale).expect("stash wal");
        }
        std::fs::write(&temp, b"partial-compacted-image").expect("write temp");

        // Reopen → the recovery finalizer restores the stashed WAL and removes the temp.
        let reopened: PersistentARTrieChar<i64> =
            PersistentARTrieChar::open(&path).expect("reopen recovers");
        for i in 0..60i64 {
            assert_eq!(
                reopened.get_value(&format!("k{i:04}")),
                Some(i),
                "term {i} lost after crash-recovery"
            );
        }
        assert!(!temp.exists(), "recovery must remove the temp image");
        assert!(
            !stale.exists(),
            "recovery must consume the stale-WAL backup marker"
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 12, ..ProptestConfig::default() })]

        /// PROPERTY: for an arbitrary set of terms + values (built with eviction, so cold nodes go
        /// to disk), compaction preserves the EXACT (term → value) map across a reopen.
        #[test]
        fn compact_preserves_term_value_map(
            count in 1usize..600,
            seed in any::<u64>(),
        ) {
            let dir = scratch("compact-prop");
            let path = dir.path().join("p.artc");
            // Deterministic pseudo-random values from `seed` (no Rng dep needed).
            let value_of = |i: usize| -> i64 {
                let x = (seed ^ (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)).rotate_left(17);
                (x % 1_000_003) as i64
            };
            {
                let mut owned: PersistentARTrieChar<i64> =
                    PersistentARTrieChar::create_with_config(&path, WalConfig::no_archive())
                        .expect("create");
                owned.set_durability_policy(DurabilityPolicy::Immediate);
                owned.install_overlay();
                owned
                    .bench_enable_eviction(EvictionConfig {
                        resident_budget_bytes: Some(8 * 1024),
                        ..EvictionConfig::without_memory_monitor()
                    })
                    .expect("enable eviction");
                for i in 0..count {
                    owned
                        .insert_with_value(&format!("term_{i:06}"), value_of(i))
                        .expect("insert");
                    if i % 64 == 0 {
                        owned.checkpoint().expect("checkpoint");
                    }
                }
                owned.checkpoint().expect("final checkpoint");
                let stats = owned.compact(CompactionConfig::default(), |_p| {}).expect("compact");
                prop_assert_eq!(stats.terms_copied, count as u64);
                for i in 0..count {
                    prop_assert_eq!(
                        owned.get_value(&format!("term_{i:06}")),
                        Some(value_of(i))
                    );
                }
            }
            let reopened: PersistentARTrieChar<i64> =
                PersistentARTrieChar::open(&path).expect("reopen");
            for i in 0..count {
                prop_assert_eq!(
                    reopened.get_value(&format!("term_{i:06}")),
                    Some(value_of(i))
                );
            }
        }
    }
}
