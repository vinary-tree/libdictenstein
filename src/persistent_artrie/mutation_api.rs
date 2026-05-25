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

use crate::value::DictionaryValue;

use super::block_storage::BlockStorage;
use super::dict_impl::PersistentARTrie;

impl<V: DictionaryValue, S: BlockStorage> PersistentARTrie<V, S> {
    /// Insert a term into the dictionary.
    pub fn insert(&mut self, term: &str) -> bool {
        self.insert_impl(term.as_bytes(), None)
    }

    /// Insert a term with an associated value.
    pub fn insert_with_value(&mut self, term: &str, value: V) -> bool {
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
    pub fn insert_batch(&mut self, entries: &[(String, Option<V>)]) -> usize {
        if entries.is_empty() {
            return 0;
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

        if let Some(ref wal_writer) = self.wal_writer {
            if let Err(e) = wal_writer.append_batch(&wal_entries) {
                warn!("Failed to log batch insert to WAL: {:?}", e);
                return 0;
            }
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
    pub fn insert_batch_bytes(&mut self, entries: &[(&[u8], Option<V>)]) -> usize {
        if entries.is_empty() {
            return 0;
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

        if let Some(ref wal_writer) = self.wal_writer {
            if let Err(e) = wal_writer.append_batch(&wal_entries) {
                warn!("Failed to log batch insert to WAL: {:?}", e);
                return 0;
            }
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
    pub fn remove(&mut self, term: &str) -> bool {
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
    pub fn remove_prefix_batched(&mut self, prefix: &[u8], batch_size: usize) -> usize {
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
}
