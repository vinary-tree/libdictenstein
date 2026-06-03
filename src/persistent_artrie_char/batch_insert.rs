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

        // Flip routing (design §2): under the overlay each entry commits via the
        // proven per-op Order-A path (the overlay's discipline is per-op WAL-then-
        // CAS, not the owned tree's single-BatchInsert append). Delegate to the
        // already-routed single-op `insert` / `insert_with_value` so NO mutation
        // logic is duplicated; arbitrary-V valued entries fall back inside those.
        if self.route_overlay() {
            let mut inserted_count = 0;
            for (term, value) in entries {
                let result = match value {
                    Some(v) => self.insert_with_value(term, v.clone()),
                    None => self.insert(term),
                };
                match result {
                    Ok(true) => inserted_count += 1,
                    Ok(false) => {}
                    Err(e) => log::warn!("Failed overlay batch insert entry: {:?}", e),
                }
            }
            return inserted_count;
        }

        let mut wal_entries = Vec::with_capacity(entries.len());
        for (term, value) in entries {
            let preflight = if value.is_some() {
                self.preflight_insert_with_value_no_wal(term).map(|_| true)
            } else {
                self.preflight_insert_no_wal(term)
            };
            if let Err(e) = preflight {
                log::warn!("Failed to preflight batch insert: {:?}", e);
                return 0;
            }

            let value_bytes = match value.as_ref() {
                Some(v) => match crate::serialization::bincode_compat::serialize(v) {
                    Ok(bytes) => Some(bytes),
                    Err(e) => {
                        log::warn!("Failed to serialize batch insert value for WAL: {:?}", e);
                        return 0;
                    }
                },
                None => None,
            };
            wal_entries.push((term.as_bytes().to_vec(), value_bytes));
        }

        let batch_record = WalRecord::BatchInsert {
            entries: wal_entries,
        };
        if let Err(e) = self.append_to_wal(batch_record) {
            log::warn!("Failed to log batch insert to WAL: {:?}", e);
            return 0;
        }

        // Then insert each entry without individual WAL logging
        let mut inserted_count = 0;
        for (term, value) in entries {
            let result = if let Some(v) = value {
                self.try_insert_impl_no_wal_with_value(term, v.clone())
            } else {
                self.try_insert_impl_no_wal(term)
            };
            match result {
                Ok(true) => inserted_count += 1,
                Ok(false) => {}
                Err(e) => {
                    log::warn!("Failed to apply batch insert after WAL append: {:?}", e);
                }
            };
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
    /// ```text
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

        // Flip routing (S5-5): under the overlay each entry commits via the proven
        // per-op Order-A path (already overlay-routed `insert`/`insert_with_value`,
        // each emitting a CommitRank), NOT a single unranked `BatchInsert` append
        // that recovery would DROP as a two-append-window orphan (the A2 fix). Mirrors
        // `insert_batch`; the `_sorted`/`_grouped`/`_arena_grouped` delegators inherit
        // this. The lossy byte→String conversion matches the owned path's per-entry
        // apply (`try_insert_impl_no_wal(&term_str)`).
        if self.route_overlay() {
            let mut inserted_count = 0;
            for (term, value) in entries {
                let term_str = String::from_utf8_lossy(term).into_owned();
                let result = match value {
                    Some(v) => self.insert_with_value(&term_str, v.clone()),
                    None => self.insert(&term_str),
                };
                match result {
                    Ok(true) => inserted_count += 1,
                    Ok(false) => {}
                    Err(e) => log::warn!("Failed overlay byte batch insert entry: {:?}", e),
                }
            }
            return inserted_count;
        }

        let mut wal_entries = Vec::with_capacity(entries.len());
        let mut prepared = Vec::with_capacity(entries.len());
        for (term, value) in entries {
            let term_str = String::from_utf8_lossy(term).into_owned();
            let preflight = if value.is_some() {
                self.preflight_insert_with_value_no_wal(&term_str)
                    .map(|_| true)
            } else {
                self.preflight_insert_no_wal(&term_str)
            };
            if let Err(e) = preflight {
                log::warn!("Failed to preflight byte batch insert: {:?}", e);
                return 0;
            }

            let value_bytes = match value.as_ref() {
                Some(v) => match crate::serialization::bincode_compat::serialize(v) {
                    Ok(bytes) => Some(bytes),
                    Err(e) => {
                        log::warn!(
                            "Failed to serialize byte batch insert value for WAL: {:?}",
                            e
                        );
                        return 0;
                    }
                },
                None => None,
            };
            wal_entries.push((term.to_vec(), value_bytes));
            prepared.push((term_str, value.clone()));
        }

        let batch_record = WalRecord::BatchInsert {
            entries: wal_entries,
        };
        if let Err(e) = self.append_to_wal(batch_record) {
            log::warn!("Failed to log batch insert to WAL: {:?}", e);
            return 0;
        }

        // Then insert each entry without individual WAL logging
        let mut inserted_count = 0;
        for (term, value) in prepared {
            let result = if let Some(v) = value {
                self.try_insert_impl_no_wal_with_value(&term, v)
            } else {
                self.try_insert_impl_no_wal(&term)
            };
            match result {
                Ok(true) => inserted_count += 1,
                Ok(false) => {}
                Err(e) => {
                    log::warn!(
                        "Failed to apply byte batch insert after WAL append: {:?}",
                        e
                    );
                }
            };
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
    pub fn insert_batch_chars_grouped(
        &mut self,
        mut entries: Vec<(Vec<char>, Option<V>)>,
    ) -> usize {
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
