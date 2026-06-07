//! Public mutation API for `PersistentARTrie<V, S>`.
//!
//! Split out of byte `dict_impl.rs` (lines ~1109-1463, ~355 LOC) as
//! the seventeenth Phase-5 byte sub-module. These public methods
//! form the insert/remove surface:
//!
//! - `insert` / `insert_with_value`
//! - `insert_batch` / `insert_batch_bytes`
//! - `insert_batch_sorted` / `insert_batch_bytes_sorted`
//! - `insert_batch_arena_grouped` / `insert_batch_grouped`
//! - `remove`
//! - `remove_prefix` / `remove_prefix_batched`
//!
//! The core implementations (`insert_impl`, `insert_impl_core`,
//! `remove_impl`) stay in `dict_impl.rs` as `pub(super)` so this
//! sibling can call them.

use log::warn;

use crate::persistent_artrie_core::key_encoding::ByteKey;
use crate::persistent_artrie_core::overlay::durable_write::DurableOverlayWrite;
use crate::value::DictionaryValue;

use super::block_storage::BlockStorage;
use super::dict_impl::PersistentARTrie;
use super::error::Result;

impl<V: DictionaryValue, S: BlockStorage> PersistentARTrie<V, S> {
    /// Insert a term into the dictionary.
    ///
    /// **M3 write-flip (C5):** under `route_overlay()` this routes to the proven
    /// Order-A [`insert_cas_durable`](Self::insert_cas_durable) (membership; value-
    /// free, so safe for ALL `V`). The public byte signature returns `bool`, so a
    /// durable failure (wrong policy / on-disk-child-blocked) is logged and reported
    /// as `false` (no insert) rather than panicking — consistent with `insert_batch`'s
    /// fail-soft WAL handling. The owned arm is the verbatim pre-flip body.
    pub fn insert(&mut self, term: &str) -> bool {
        if self.route_overlay() {
            return self
                .insert_cas_durable(term.as_bytes())
                .unwrap_or_else(|e| {
                    warn!("insert overlay route failed (reporting no-insert): {:?}", e);
                    false
                });
        }
        self.insert_impl(term.as_bytes(), None)
    }

    /// Insert a term with an associated value.
    ///
    /// **Semantics — UPSERT (overwrite on duplicate):** the canonical map "insert or
    /// update" ([`crate::MutableMappedDictionary`]); the owned body overwrites an
    /// existing term's value, matching every other backend and the map laws. Returns
    /// `true` iff the term was newly inserted (`false` = an existing value overwritten,
    /// or a durable error logged as no-insert).
    ///
    /// **Flip routing (design §2 + C0):** under `route_overlay()` this routes to the
    /// generic Order-A [`upsert_cas_durable_default`](DurableOverlayWrite::upsert_cas_durable_default)
    /// for ANY `V` (overwrite = last-writer-wins root-CAS) — NEVER falling through to
    /// owned (the NH1 data-loss fix). (C0 fix: previously routed to the insert-once
    /// `insert_cas_with_value_durable_default`, diverging from the owned overwrite
    /// semantics — a silent overlay↔owned mismatch on duplicate keys.) A durable
    /// failure is logged and reported `false` (byte's `bool` signature).
    pub fn insert_with_value(&mut self, term: &str, value: V) -> bool {
        if self.route_overlay() {
            return <Self as DurableOverlayWrite<ByteKey, V, S>>::upsert_cas_durable_default(
                self,
                term.as_bytes(),
                value,
            )
            .unwrap_or_else(|e| {
                warn!(
                    "insert_with_value overlay route failed (reporting no-insert): {:?}",
                    e
                );
                false
            });
        }
        self.insert_impl(term.as_bytes(), Some(value))
    }

    /// Insert multiple terms in a single batch operation.
    ///
    /// This method is optimized for bulk insertions by:
    /// 1. Writing a single BatchInsert WAL record for all entries
    ///    (reduces header overhead by ~99%)
    /// 2. Syncing only once after all entries are logged
    ///
    /// Returns the number of terms that were newly inserted (excluding
    /// updates to existing terms).
    ///
    /// **M3 write-flip (C5):** under `route_overlay()` each entry routes to the
    /// proven Order-A durable overlay insert (the audit's "loop insert_cas_durable"
    /// — no batch-durable overlay primitive exists, and a per-record durable insert
    /// preserves the WAL-then-CAS ordering). A `None` value → membership
    /// [`insert_cas_durable`](Self::insert_cas_durable); an `i64` value →
    /// [`insert_cas_with_value_durable`](Self::insert_cas_with_value_durable). The
    /// owned arm below is the verbatim pre-flip batch path.
    pub fn insert_batch(&mut self, entries: &[(String, Option<V>)]) -> usize {
        if entries.is_empty() {
            return 0;
        }

        if self.route_overlay() {
            let mut inserted = 0usize;
            for (term, value) in entries {
                if self.insert_batch_entry_overlay(term.as_bytes(), value.as_ref()) {
                    inserted += 1;
                }
            }
            return inserted;
        }

        let mut wal_entries = Vec::with_capacity(entries.len());
        for (term, value) in entries {
            let value_bytes = match value.as_ref() {
                Some(v) => match crate::serialization::bincode_compat::serialize(v) {
                    Ok(bytes) => Some(bytes),
                    Err(e) => {
                        warn!("Failed to serialize batch insert value for WAL: {:?}", e);
                        return 0;
                    }
                },
                None => None,
            };
            wal_entries.push((term.as_bytes().to_vec(), value_bytes));
        }

        if let Err(e) = self.append_batch_mutation_wal_record(&wal_entries, "batch_insert") {
            warn!("Failed to log batch insert to WAL: {:?}", e);
            return 0;
        }

        let mut inserted_count = 0;
        for (term, value) in entries {
            if self.insert_impl_core(term.as_bytes(), value.clone()) {
                inserted_count += 1;
            }
        }

        inserted_count
    }

    /// Insert multiple byte-slice terms in a single batch operation.
    ///
    /// This is the byte-slice version of `insert_batch()` for when you
    /// already have byte data and want to avoid string conversion overhead.
    ///
    /// **M3 write-flip (C5):** under `route_overlay()` each entry routes to the
    /// proven Order-A durable overlay insert (per-record; see [`insert_batch`](Self::insert_batch)).
    /// The owned arm below is the verbatim pre-flip batch path.
    pub fn insert_batch_bytes(&mut self, entries: &[(&[u8], Option<V>)]) -> usize {
        if entries.is_empty() {
            return 0;
        }

        if self.route_overlay() {
            let mut inserted = 0usize;
            for (term, value) in entries {
                if self.insert_batch_entry_overlay(term, value.as_ref()) {
                    inserted += 1;
                }
            }
            return inserted;
        }

        let mut wal_entries = Vec::with_capacity(entries.len());
        for (term, value) in entries {
            let value_bytes = match value.as_ref() {
                Some(v) => match crate::serialization::bincode_compat::serialize(v) {
                    Ok(bytes) => Some(bytes),
                    Err(e) => {
                        warn!("Failed to serialize batch insert value for WAL: {:?}", e);
                        return 0;
                    }
                },
                None => None,
            };
            wal_entries.push((term.to_vec(), value_bytes));
        }

        if let Err(e) = self.append_batch_mutation_wal_record(&wal_entries, "batch_insert_bytes") {
            warn!("Failed to log batch insert to WAL: {:?}", e);
            return 0;
        }

        let mut inserted_count = 0;
        for (term, value) in entries {
            if self.insert_impl_core(term, value.clone()) {
                inserted_count += 1;
            }
        }

        inserted_count
    }

    /// Insert multiple terms with optional values in sorted order for cache locality.
    ///
    /// This method sorts the entries lexicographically before inserting them,
    /// which improves cache hit rates since consecutive terms share trie prefix
    /// paths. For large batches, this can improve throughput by 5-20%.
    pub fn insert_batch_sorted(&mut self, mut entries: Vec<(String, Option<V>)>) -> usize {
        if entries.is_empty() {
            return 0;
        }

        entries.sort_by(|a, b| a.0.cmp(&b.0));

        let refs: Vec<(String, Option<V>)> = entries;
        self.insert_batch(&refs)
    }

    /// Insert multiple byte terms with optional values in sorted order for cache locality.
    pub fn insert_batch_bytes_sorted(&mut self, mut entries: Vec<(Vec<u8>, Option<V>)>) -> usize {
        if entries.is_empty() {
            return 0;
        }

        entries.sort_by(|a, b| a.0.cmp(&b.0));

        let refs: Vec<(&[u8], Option<V>)> = entries
            .iter()
            .map(|(term, value)| (term.as_slice(), value.clone()))
            .collect();
        self.insert_batch_bytes(&refs)
    }

    /// Insert multiple byte terms grouped by first byte for arena locality.
    ///
    /// This method groups inserts by their first byte prefix before inserting,
    /// which improves I/O locality for disk-resident tries. Terms with the same
    /// first byte tend to land in nearby arenas because arenas fill sequentially
    /// during loading.
    pub fn insert_batch_arena_grouped(&mut self, mut entries: Vec<(Vec<u8>, Option<V>)>) -> usize {
        if entries.is_empty() {
            return 0;
        }

        entries.sort_by(|a, b| {
            let a_prefix = a.0.first().copied().unwrap_or(0);
            let b_prefix = b.0.first().copied().unwrap_or(0);
            a_prefix.cmp(&b_prefix).then_with(|| a.0.cmp(&b.0))
        });

        let refs: Vec<(&[u8], Option<V>)> = entries
            .iter()
            .map(|(term, value)| (term.as_slice(), value.clone()))
            .collect();
        self.insert_batch_bytes(&refs)
    }

    /// Insert multiple string terms grouped by first character for arena locality.
    pub fn insert_batch_grouped(&mut self, mut entries: Vec<(String, Option<V>)>) -> usize {
        if entries.is_empty() {
            return 0;
        }

        entries.sort_by(|a, b| {
            let a_prefix = a.0.chars().next().unwrap_or('\0');
            let b_prefix = b.0.chars().next().unwrap_or('\0');
            a_prefix.cmp(&b_prefix).then_with(|| a.0.cmp(&b.0))
        });

        self.insert_batch(&entries)
    }

    /// Remove a term from the dictionary.
    ///
    /// **M3 write-flip (C5):** under `route_overlay()` this routes to the proven
    /// Order-A [`remove_cas_durable`](Self::remove_cas_durable) (R-B: durable
    /// `Remove` → path-copy clearing the leaf's finality → root-CAS → mark_committed;
    /// value-free, safe for ALL `V`). The public `bool` signature reports a durable
    /// failure as `false` (no remove) with a log. The owned arm is verbatim pre-flip.
    pub fn remove(&mut self, term: &str) -> bool {
        if self.route_overlay() {
            return self
                .remove_cas_durable(term.as_bytes())
                .unwrap_or_else(|e| {
                    warn!("remove overlay route failed (reporting no-remove): {:?}", e);
                    false
                });
        }
        self.remove_impl(term.as_bytes())
    }

    /// Remove all terms with the given prefix (batched for memory efficiency).
    ///
    /// Returns the number of terms removed. Each removal is logged to WAL
    /// individually for crash recovery safety (no batch WAL record type).
    pub fn remove_prefix(&mut self, prefix: &[u8]) -> usize {
        self.remove_prefix_batched(prefix, 1024)
    }

    /// Remove all terms with the given prefix using a custom batch size.
    ///
    /// This method allows fine-tuning the memory/efficiency trade-off:
    /// smaller batch_size = less memory, more iterations.
    ///
    /// **M3 write-flip (C5/H4):** under `route_overlay()` there is no owned arena to
    /// group by; the routed `iter_prefix` would enumerate the OVERLAY while the
    /// owned `remove_impl` runs on the EMPTY owned tree = a SILENT NO-OP delete (the
    /// audit's named hazard). The `usize` signature cannot carry an `Err`, so —
    /// mirroring the char audit's resolution — this reimplements over the overlay
    /// remove-CAS: enumerate the prefix from the immutable overlay (non-faulting,
    /// resident-finals), then durably [`remove_cas_durable`](Self::remove_cas_durable)
    /// each term. Durable, NOT a no-op, NO data loss. Arena page-locality grouping is
    /// an owned-tree disk-layout optimization with no overlay analogue; the removal
    /// SEMANTICS are fully preserved. The owned arm below is verbatim pre-flip.
    pub fn remove_prefix_batched(&mut self, prefix: &[u8], batch_size: usize) -> usize {
        if self.route_overlay() {
            return self.remove_prefix_overlay(prefix);
        }

        let batch_size = batch_size.max(1);
        let mut total_removed = 0;

        loop {
            let batch: Vec<Vec<u8>> = self
                .iter_prefix(prefix)
                .map(|iter| iter.take(batch_size).collect())
                .unwrap_or_default();

            if batch.is_empty() {
                break;
            }

            let mut removed_this_round = 0;
            for term in batch {
                if self.remove_impl(&term) {
                    total_removed += 1;
                    removed_this_round += 1;
                }
            }

            if removed_this_round == 0 {
                break;
            }
        }

        total_removed
    }

    // ====================================================================
    // M3 overlay write helpers (private; only reached under `route_overlay()`).
    // ====================================================================

    /// Route a single batch-insert entry to the proven Order-A durable overlay
    /// insert. A `None` value → membership [`insert_cas_durable`](Self::insert_cas_durable);
    /// an `i64` value → [`insert_cas_with_value_durable`](Self::insert_cas_with_value_durable)
    /// via the SAFE `Any` dispatch. Returns `true` iff this call newly inserted the
    /// term (matching the owned batch's "newly inserted" count). A durable failure
    /// is logged and counted as not-inserted (byte's fail-soft batch discipline).
    fn insert_batch_entry_overlay(&self, term: &[u8], value: Option<&V>) -> bool {
        let result: Result<bool> = match value {
            // Membership: durable membership insert.
            None => self.insert_cas_durable(term),
            // Valued (C0 fix): route to the SHARED GENERIC durable UPSERT for ANY `V`
            // (overwrite on duplicate). The owned batch overwrites per entry, so the
            // overlay batch must too — routing the valued arm to the insert-once
            // `insert_cas_with_value_durable_default` left byte batch insert-once while
            // single `insert_with_value` became upsert (a silent divergence).
            // `upsert_cas_durable` returns `Ok(true)` iff newly inserted, preserving
            // the "newly inserted" count; for `V = ()` it stores a (trivial) unit value
            // (membership-equivalent); for arbitrary `V` it preserves the value.
            Some(v) => <Self as DurableOverlayWrite<ByteKey, V, S>>::upsert_cas_durable_default(
                self,
                term,
                v.clone(),
            ),
        };
        result.unwrap_or_else(|e| {
            warn!(
                "insert_batch overlay route failed for term {:?} (counting as not-inserted): {:?}",
                term, e
            );
            false
        })
    }

    /// Overlay prefix removal (C5/H4): enumerate the prefix subtree from the
    /// immutable overlay (non-faulting, resident-finals) and durably remove each
    /// term via the Order-A [`remove_cas_durable`](Self::remove_cas_durable). Durable
    /// — a reopen sees the removals (WAL recovery). The overlay republishes its root
    /// per `remove_cas_durable`, so the matching terms are SNAPSHOT first (one
    /// resident enumeration) before any removal. Returns the number removed.
    fn remove_prefix_overlay(&mut self, prefix: &[u8]) -> usize {
        let terms = match self.overlay_iter_prefix(prefix) {
            Some(terms) => terms,
            None => return 0,
        };
        let mut removed = 0usize;
        for term in &terms {
            match self.remove_cas_durable(term) {
                Ok(true) => removed += 1,
                Ok(false) => {}
                Err(e) => warn!(
                    "remove_prefix overlay route failed for term {:?}: {:?}",
                    term, e
                ),
            }
        }
        removed
    }
}
