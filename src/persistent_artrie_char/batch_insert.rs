//! Batch-insert API for `PersistentARTrieChar<V, S>`.
//!
//! Split out of char `dict_impl_char.rs` (lines ~531-855, ~325 LOC)
//! as a Phase-6 char sub-module. Methods covered:
//!
//! - `insert_batch` / `insert_batch_chars` / `insert_batch_bytes`
//! - `insert_batch_sorted` / `insert_batch_chars_sorted` /
//!   `insert_batch_bytes_sorted`
//! - `insert_batch_grouped` / `insert_batch_chars_grouped` /
//!   `insert_batch_bytes_grouped`
//! - `insert_batch_arena_grouped` (alias for byte_grouped)

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::wal::WalRecord;
use crate::value::DictionaryValue;

impl<V: DictionaryValue, S: BlockStorage> super::PersistentARTrieChar<V, S> {
    pub fn insert_batch(&mut self, entries: &[(String, Option<V>)]) -> usize {
        if entries.is_empty() {
            return 0;
        }

        // First, log all entries as a single batch WAL record (routes through group commit if enabled)
        let wal_entries: Vec<(Vec<u8>, Option<Vec<u8>>)> = entries
            .iter()
            .map(|(term, value)| {
                let term_bytes = term.as_bytes().to_vec();
                let value_bytes = value.as_ref().and_then(|v| {
                    bincode::serialize(v).ok()
                });
                (term_bytes, value_bytes)
            })
            .collect();

        let batch_record = WalRecord::BatchInsert { entries: wal_entries };
        if let Err(e) = self.append_to_wal(batch_record) {
            log::warn!("Failed to log batch insert to WAL: {:?}", e);
        }

        // Then insert each entry without individual WAL logging
        let mut inserted_count = 0;
        for (term, value) in entries {
            if let Some(v) = value {
                if self.insert_impl_no_wal_with_value(term, v.clone()) {
                    inserted_count += 1;
                }
            } else {
                if self.insert_impl_no_wal(term) {
                    inserted_count += 1;
                }
            }
        }

        inserted_count
    }

    /// Insert multiple terms (as char slices) with optional values in a single batch operation.
    ///
    /// This method is useful when you have pre-parsed Unicode characters and want
    /// to avoid UTF-8 encoding overhead for each term individually.
    ///
    /// # Arguments
    ///
    /// * `entries` - Slice of (char_slice, optional_value) pairs to insert
    ///
    /// # Returns
    ///
    /// The number of terms that were newly inserted (not updates).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let entries = vec![
    ///     (&['日', '本', '語'][..], Some(1)),
    ///     (&['中', '文'][..], Some(2)),
    /// ];
    /// let count = trie.insert_batch_chars(&entries)?;
    /// ```
    pub fn insert_batch_chars(&mut self, entries: &[(&[char], Option<V>)]) -> usize {
        if entries.is_empty() {
            return 0;
        }

        // Convert char slices to strings for WAL and insertion
        let string_entries: Vec<(String, Option<V>)> = entries
            .iter()
            .map(|(chars, value)| {
                let term: String = chars.iter().collect();
                (term, value.clone())
            })
            .collect();

        self.insert_batch(&string_entries)
    }

    /// Insert multiple byte-slice terms in a single batch operation.
    ///
    /// This is the byte-slice version of `insert_batch()` for when you already
    /// have byte data and want to avoid string conversion overhead.
    ///
    /// # Arguments
    ///
    /// * `entries` - Slice of (term_bytes, optional_value) pairs to insert
    ///
    /// # Returns
    ///
    /// The number of terms that were newly inserted.
    pub fn insert_batch_bytes(&mut self, entries: &[(&[u8], Option<V>)]) -> usize {
        if entries.is_empty() {
            return 0;
        }

        // First, log all entries as a single batch WAL record (routes through group commit if enabled)
        let wal_entries: Vec<(Vec<u8>, Option<Vec<u8>>)> = entries
            .iter()
            .map(|(term, value)| {
                let value_bytes = value.as_ref().and_then(|v| {
                    bincode::serialize(v).ok()
                });
                (term.to_vec(), value_bytes)
            })
            .collect();

        let batch_record = WalRecord::BatchInsert { entries: wal_entries };
        if let Err(e) = self.append_to_wal(batch_record) {
            log::warn!("Failed to log batch insert to WAL: {:?}", e);
        }

        // Then insert each entry without individual WAL logging
        let mut inserted_count = 0;
        for (term, value) in entries {
            let term_str = String::from_utf8_lossy(term);
            if let Some(v) = value {
                if self.insert_impl_no_wal_with_value(&term_str, v.clone()) {
                    inserted_count += 1;
                }
            } else {
                if self.insert_impl_no_wal(&term_str) {
                    inserted_count += 1;
                }
            }
        }

        inserted_count
    }

    /// Insert multiple terms with optional values in sorted order for cache locality.
    ///
    /// This method sorts the entries lexicographically before inserting them,
    /// which improves cache hit rates since consecutive terms share trie prefix
    /// paths. For large batches, this can improve throughput by 5-20%.
    ///
    /// All entries are logged as a single batch WAL record before insertion.
    ///
    /// # Arguments
    ///
    /// * `entries` - Vector of (term, optional_value) pairs to insert
    ///
    /// # Returns
    ///
    /// The number of terms that were newly inserted.
    pub fn insert_batch_sorted(&mut self, mut entries: Vec<(String, Option<V>)>) -> usize {
        if entries.is_empty() {
            return 0;
        }

        // Sort by term lexicographically for cache locality
        entries.sort_by(|a, b| a.0.cmp(&b.0));

        // Delegate to insert_batch
        self.insert_batch(&entries)
    }

    /// Insert multiple char-slice terms with optional values in sorted order for cache locality.
    ///
    /// This method sorts the entries lexicographically before inserting them,
    /// which improves cache hit rates since consecutive terms share trie prefix
    /// paths. For large batches, this can improve throughput by 5-20%.
    ///
    /// All entries are logged as a single batch WAL record before insertion.
    ///
    /// # Arguments
    ///
    /// * `entries` - Vector of (char_vec, optional_value) pairs to insert
    ///
    /// # Returns
    ///
    /// The number of terms that were newly inserted.
    pub fn insert_batch_chars_sorted(&mut self, mut entries: Vec<(Vec<char>, Option<V>)>) -> usize {
        if entries.is_empty() {
            return 0;
        }

        // Sort by chars lexicographically for cache locality
        entries.sort_by(|a, b| a.0.cmp(&b.0));

        // Convert to references for insert_batch_chars
        let refs: Vec<(&[char], Option<V>)> = entries
            .iter()
            .map(|(chars, value)| (chars.as_slice(), value.clone()))
            .collect();
        self.insert_batch_chars(&refs)
    }

    /// Insert multiple byte terms with optional values in sorted order for cache locality.
    ///
    /// This method sorts the entries lexicographically before inserting them,
    /// which improves cache hit rates since consecutive terms share trie prefix
    /// paths. For large batches, this can improve throughput by 5-20%.
    ///
    /// All entries are logged as a single batch WAL record before insertion.
    ///
    /// # Arguments
    ///
    /// * `entries` - Vector of (term_bytes, optional_value) pairs to insert
    ///
    /// # Returns
    ///
    /// The number of terms that were newly inserted.
    pub fn insert_batch_bytes_sorted(&mut self, mut entries: Vec<(Vec<u8>, Option<V>)>) -> usize {
        if entries.is_empty() {
            return 0;
        }

        // Sort by term lexicographically for cache locality
        entries.sort_by(|a, b| a.0.cmp(&b.0));

        // Convert to references for insert_batch_bytes
        let refs: Vec<(&[u8], Option<V>)> = entries
            .iter()
            .map(|(term, value)| (term.as_slice(), value.clone()))
            .collect();
        self.insert_batch_bytes(&refs)
    }

    /// Insert multiple string terms grouped by first character for arena locality.
    ///
    /// This method groups inserts by their first character before inserting,
    /// which improves I/O locality for disk-resident tries. Terms with the same
    /// first character tend to land in nearby arenas because arenas fill
    /// sequentially during loading.
    ///
    /// # Performance
    ///
    /// Expected improvement: 5-10% faster batch inserts for disk-resident tries
    /// due to improved I/O locality. The first-character heuristic provides ~60-80%
    /// of the benefit of full arena prediction with O(1) complexity.
    ///
    /// # Arguments
    ///
    /// * `entries` - Vector of (term, optional_value) pairs to insert
    ///
    /// # Returns
    ///
    /// The number of terms that were newly inserted.
    pub fn insert_batch_grouped(&mut self, mut entries: Vec<(String, Option<V>)>) -> usize {
        if entries.is_empty() {
            return 0;
        }

        // Sort by first character (arena proxy) then by full term for within-group locality
        entries.sort_by(|a, b| {
            let a_prefix = a.0.chars().next().unwrap_or('\0');
            let b_prefix = b.0.chars().next().unwrap_or('\0');
            a_prefix.cmp(&b_prefix).then_with(|| a.0.cmp(&b.0))
        });

        // Delegate to insert_batch
        self.insert_batch(&entries)
    }

    /// Insert multiple char-slice terms grouped by first character for arena locality.
    ///
    /// This is the char-slice variant of `insert_batch_grouped`. See that method
    /// for detailed documentation on the arena grouping strategy.
    ///
    /// # Arguments
    ///
    /// * `entries` - Vector of (char_vec, optional_value) pairs to insert
    ///
    /// # Returns
    ///
    /// The number of terms that were newly inserted.
    pub fn insert_batch_chars_grouped(&mut self, mut entries: Vec<(Vec<char>, Option<V>)>) -> usize {
        if entries.is_empty() {
            return 0;
        }

        // Sort by first character (arena proxy) then by full term
        entries.sort_by(|a, b| {
            let a_prefix = a.0.first().copied().unwrap_or('\0');
            let b_prefix = b.0.first().copied().unwrap_or('\0');
            a_prefix.cmp(&b_prefix).then_with(|| a.0.cmp(&b.0))
        });

        // Convert to references for insert_batch_chars
        let refs: Vec<(&[char], Option<V>)> = entries
            .iter()
            .map(|(chars, value)| (chars.as_slice(), value.clone()))
            .collect();
        self.insert_batch_chars(&refs)
    }

    /// Insert multiple byte terms grouped by first byte for arena locality.
    ///
    /// This method groups inserts by their first byte prefix before inserting,
    /// which improves I/O locality for disk-resident tries.
    ///
    /// # Arguments
    ///
    /// * `entries` - Vector of (term_bytes, optional_value) pairs to insert
    ///
    /// # Returns
    ///
    /// The number of terms that were newly inserted.
    pub fn insert_batch_bytes_grouped(&mut self, mut entries: Vec<(Vec<u8>, Option<V>)>) -> usize {
        if entries.is_empty() {
            return 0;
        }

        // Sort by first byte (arena proxy) then by full term for within-group locality
        entries.sort_by(|a, b| {
            let a_prefix = a.0.first().copied().unwrap_or(0);
            let b_prefix = b.0.first().copied().unwrap_or(0);
            a_prefix.cmp(&b_prefix).then_with(|| a.0.cmp(&b.0))
        });

        // Convert to references for insert_batch_bytes
        let refs: Vec<(&[u8], Option<V>)> = entries
            .iter()
            .map(|(term, value)| (term.as_slice(), value.clone()))
            .collect();
        self.insert_batch_bytes(&refs)
    }

    /// Alias for `insert_batch_bytes_grouped` for API consistency with PersistentARTrie.
    ///
    /// See [`insert_batch_bytes_grouped`](Self::insert_batch_bytes_grouped) for documentation.
    #[inline]
    pub fn insert_batch_arena_grouped(&mut self, entries: Vec<(Vec<u8>, Option<V>)>) -> usize {
        self.insert_batch_bytes_grouped(entries)
    }
}
