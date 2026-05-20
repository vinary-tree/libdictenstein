//! Public mutation API for `PersistentVocabARTrie<S>`.
//!
//! Split out of vocab `dict_impl.rs` (lines ~696-925, ~230 LOC) as
//! a Phase-6 vocab sub-module. Methods covered:
//!
//! - `insert` — term → auto-assigned u64 index (with BloomFilter fast path)
//! - `insert_batch` — bulk insert with WAL batch record
//! - `insert_with_index` — insert at a specific vocabulary index

use std::sync::atomic::Ordering;

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::dict_impl::DurabilityPolicy;
use crate::persistent_artrie::error::PersistentARTrieError;
use crate::persistent_artrie::wal::WalRecord;
use crate::persistent_artrie_char::types::NodeRef;

use super::types::{VocabTrieNode, VocabTrieRoot};

impl<S: BlockStorage> super::dict_impl::PersistentVocabARTrie<S> {
    /// Insert a term and auto-assign the next vocabulary index.
    ///
    /// # Returns
    ///
    /// The assigned vocabulary index.
    ///
    /// # Performance
    ///
    /// When a BloomFilter is enabled, new terms are detected in O(1) time,
    /// skipping the O(k) trie traversal for existence checking. This provides
    /// significant speedup during bulk vocabulary building where most terms
    /// are new.
    pub fn insert(&mut self, term: &str) -> u64 {
        // Fast path: bloom filter says definitely NOT in vocabulary
        // This skips the O(k) trie traversal for new terms
        let is_definitely_new = self.bloom_filter
            .as_ref()
            .map(|b| !b.might_contain(term))
            .unwrap_or(false);

        if !is_definitely_new {
            // Might exist: check trie first
            if let Some(idx) = self.get_index(term) {
                return idx;
            }
        }

        // New term: atomically claim the next index
        let index = self.next_index.fetch_add(1, Ordering::AcqRel);

        // Write WAL record BEFORE modifying trie
        if let Some(ref wal) = self.wal_writer {
            let record = WalRecord::Insert {
                term: term.as_bytes().to_vec(),
                value: Some(index.to_le_bytes().to_vec()),
            };
            if let Ok(lsn) = wal.append(record) {
                self.next_lsn.fetch_max(lsn + 1, Ordering::AcqRel);

                // Sync if immediate durability policy
                if self.durability_policy == DurabilityPolicy::Immediate {
                    let _ = wal.sync();
                    self.synced_lsn.fetch_max(lsn, Ordering::AcqRel);
                }
            }
        }

        // Insert into trie
        self.insert_with_index(term, index);

        // Update bloom filter
        if let Some(ref mut bloom) = self.bloom_filter {
            bloom.insert(term);
        }

        index
    }

    /// Bulk insert multiple terms with a single WAL record.
    ///
    /// This is more efficient than individual `insert()` calls because:
    /// 1. Logs all entries as a single `BatchInsert` WAL record
    /// 2. Reduces WAL header overhead by ~99% for large batches
    /// 3. Single disk sync for the entire batch
    ///
    /// # Arguments
    ///
    /// * `terms` - Slice of terms to insert
    ///
    /// # Returns
    ///
    /// Vector of assigned indices (same order as input terms).
    /// Terms that already exist return their existing indices.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let mut vocab = PersistentVocabARTrie::create("vocab.vocab")?;
    /// let indices = vocab.insert_batch(&["apple", "banana", "cherry"]);
    /// assert_eq!(indices, vec![0, 1, 2]);
    ///
    /// // Duplicate terms return existing indices
    /// let indices2 = vocab.insert_batch(&["apple", "date"]);
    /// assert_eq!(indices2, vec![0, 3]); // "apple" already at 0
    /// ```
    pub fn insert_batch(&mut self, terms: &[&str]) -> Vec<u64> {
        if terms.is_empty() {
            return Vec::new();
        }

        let mut indices = Vec::with_capacity(terms.len());
        let mut new_entries: Vec<(Vec<u8>, Option<Vec<u8>>)> = Vec::new();
        let mut new_term_indices: Vec<(usize, u64)> = Vec::new(); // (position, index)

        // Phase 1: Collect indices, separating existing vs new terms
        for (pos, term) in terms.iter().enumerate() {
            // Fast path: bloom filter says definitely NOT in vocabulary
            let is_definitely_new = self.bloom_filter
                .as_ref()
                .map(|b| !b.might_contain(term))
                .unwrap_or(false);

            if !is_definitely_new {
                // Might exist: check trie first
                if let Some(idx) = self.get_index(term) {
                    indices.push(idx);
                    continue;
                }
            }

            // New term: atomically claim the next index
            let index = self.next_index.fetch_add(1, Ordering::AcqRel);

            // Prepare for batch WAL record
            new_entries.push((
                term.as_bytes().to_vec(),
                Some(index.to_le_bytes().to_vec()),
            ));

            new_term_indices.push((pos, index));
            indices.push(index);
        }

        // Phase 2: Log all new entries as single BatchInsert WAL record
        if !new_entries.is_empty() {
            if let Some(ref wal) = self.wal_writer {
                if let Ok(lsn) = wal.append_batch(&new_entries) {
                    self.next_lsn.fetch_max(lsn + 1, Ordering::AcqRel);

                    // Sync if immediate durability policy
                    if self.durability_policy == DurabilityPolicy::Immediate {
                        let _ = wal.sync();
                        self.synced_lsn.fetch_max(lsn, Ordering::AcqRel);
                    }
                }
            }

            // Phase 3: Insert new terms into trie (no individual WAL logging)
            for (pos, index) in &new_term_indices {
                let term = terms[*pos];
                self.insert_with_index(term, *index);

                // Update bloom filter
                if let Some(ref mut bloom) = self.bloom_filter {
                    bloom.insert(term);
                }
            }

            self.dirty.store(true, Ordering::Release);
        }

        indices
    }


    /// Insert a term with a specific vocabulary index.
    ///
    /// # Returns
    ///
    /// `true` if the term was newly inserted, `false` if it already existed.
    pub fn insert_with_index(&mut self, term: &str, index: u64) -> bool {
        let chars: Vec<char> = term.chars().collect();
        let root_ref = NodeRef::new(0, 0);

        match &mut self.root {
            VocabTrieRoot::Empty => {
                return false;
            }
            VocabTrieRoot::Node(root) => {
                // Navigate/create path to the term
                let mut current = root.as_mut();
                let mut current_ref = root_ref;

                for &c in chars.iter() {
                    // Assign NodeRef for current node if not already
                    let slot = self.next_slot;
                    self.next_slot += 1;
                    let child_ref = NodeRef::new(0, slot as u32);

                    // Get or create child with parent pointer
                    let child = current.get_or_create_child(c, current_ref);

                    // Update node map
                    if !self.node_map.contains_key(&child_ref) {
                        self.node_map.insert(child_ref, child as *const VocabTrieNode);
                    }

                    current_ref = child_ref;
                    current = child;
                }

                // Check if already final
                if current.is_final() {
                    return false;
                }

                // Set value and mark final
                current.set_value(index);

                // Update reverse index
                if let Some(ref mut rev_idx) = self.reverse_index {
                    let _ = rev_idx.set(index, current_ref);
                }

                // Cache the term
                self.reverse_cache.put(index, term.to_string());

                // Update counts atomically
                self.entry_count.fetch_add(1, Ordering::AcqRel);
                self.dirty.store(true, Ordering::Release);

                // Update next_index if needed atomically (for merge_into to work correctly)
                loop {
                    let current = self.next_index.load(Ordering::Acquire);
                    if index < current {
                        break; // Another thread already advanced it
                    }
                    let new_val = index + 1;
                    match self.next_index.compare_exchange(
                        current, new_val, Ordering::AcqRel, Ordering::Acquire
                    ) {
                        Ok(_) => break,
                        Err(_) => continue, // Retry
                    }
                }

                true
            }
        }
    }
}
