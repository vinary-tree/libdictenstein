//! Public mutation API for `PersistentVocabARTrie<S>`.
//!
//! Split out of vocab `dict_impl.rs` (lines ~696-925, ~230 LOC) as
//! a Phase-6 vocab sub-module. Methods covered:
//!
//! - `insert` — term → auto-assigned u64 index (with BloomFilter fast path)
//! - `insert_batch` — bulk insert with WAL batch record
//! - `insert_with_index` — insert at a specific vocabulary index

use std::collections::HashMap;
use std::sync::atomic::Ordering;

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::dict_impl::DurabilityPolicy;
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::persistent_artrie::wal::{Lsn, WalRecord};
use crate::persistent_artrie_char::types::NodeRef;

use super::types::{VocabTrieNode, VocabTrieRoot};

impl<S: BlockStorage> super::dict_impl::PersistentVocabARTrie<S> {
    fn wal_unavailable_error(operation: &str) -> PersistentARTrieError {
        PersistentARTrieError::Wal(format!(
            "cannot {operation}: persistent vocabulary WAL writer is unavailable"
        ))
    }

    fn map_wal_error(operation: &str, error: impl std::fmt::Display) -> PersistentARTrieError {
        PersistentARTrieError::Wal(format!("{operation} failed: {error}"))
    }

    fn append_vocab_insert_wal(&mut self, term: &str, index: u64) -> Result<Lsn> {
        let wal = self
            .wal_writer
            .as_ref()
            .ok_or_else(|| Self::wal_unavailable_error("append insert WAL record"))?;
        let record = WalRecord::Insert {
            term: term.as_bytes().to_vec(),
            value: Some(index.to_le_bytes().to_vec()),
        };
        let lsn = wal
            .append(record)
            .map_err(|e| Self::map_wal_error("append vocabulary insert WAL record", e))?;
        self.next_lsn.fetch_max(lsn + 1, Ordering::AcqRel);
        self.sync_vocab_wal_after_append(lsn)?;
        Ok(lsn)
    }

    fn append_vocab_batch_wal(
        &mut self,
        entries: &[(Vec<u8>, Option<Vec<u8>>)],
    ) -> Result<Option<Lsn>> {
        if entries.is_empty() {
            return Ok(None);
        }

        let wal = self
            .wal_writer
            .as_ref()
            .ok_or_else(|| Self::wal_unavailable_error("append batch WAL record"))?;
        let lsn = wal
            .append_batch(entries)
            .map_err(|e| Self::map_wal_error("append vocabulary batch WAL record", e))?;
        self.next_lsn.fetch_max(lsn + 1, Ordering::AcqRel);
        self.sync_vocab_wal_after_append(lsn)?;
        Ok(Some(lsn))
    }

    fn sync_vocab_wal_after_append(&mut self, appended_lsn: Lsn) -> Result<()> {
        match self.durability_policy {
            DurabilityPolicy::Immediate | DurabilityPolicy::GroupCommit => {}
            DurabilityPolicy::Periodic | DurabilityPolicy::None => return Ok(()),
        }

        let wal = self
            .wal_writer
            .as_ref()
            .ok_or_else(|| Self::wal_unavailable_error("sync WAL after insert"))?;
        let synced_lsn = wal
            .sync()
            .map_err(|e| Self::map_wal_error("sync vocabulary WAL", e))?;
        if synced_lsn < appended_lsn {
            return Err(PersistentARTrieError::Wal(format!(
                "sync vocabulary WAL failed to cover appended LSN {appended_lsn}; synced {synced_lsn}"
            )));
        }
        self.synced_lsn.fetch_max(synced_lsn, Ordering::AcqRel);
        Ok(())
    }

    fn next_unassigned_index_from(&self, mut candidate: u64) -> u64 {
        while self.contains_index(candidate) {
            candidate = candidate.saturating_add(1);
        }
        candidate
    }

    fn validate_index_insert(&self, term: &str, index: u64) -> Result<Option<u64>> {
        if index < self.start_index {
            return Err(PersistentARTrieError::InvalidOperation(format!(
                "vocabulary index {index} is below start index {}",
                self.start_index
            )));
        }

        if let Some(existing_index) = self.get_index(term) {
            if existing_index == index {
                return Ok(Some(existing_index));
            }
            return Err(PersistentARTrieError::InvalidOperation(format!(
                "term {term:?} is already assigned index {existing_index}, not {index}"
            )));
        }

        if let Some(existing_term) = self.get_term(index) {
            return Err(PersistentARTrieError::InvalidOperation(format!(
                "vocabulary index {index} is already assigned to term {existing_term:?}"
            )));
        }

        Ok(None)
    }

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
    pub fn insert(&mut self, term: &str) -> Result<u64> {
        // Flip routing: under route_overlay() the lock-free Order-A overlay insert is the
        // live write path (the owned body below is dead post-flip; deleted at V6).
        if self.route_overlay() {
            return self.insert_overlay(term);
        }
        // Fast path: bloom filter says definitely NOT in vocabulary
        // This skips the O(k) trie traversal for new terms
        let is_definitely_new = self
            .bloom_filter
            .as_ref()
            .map(|b| !b.might_contain(term))
            .unwrap_or(false);

        if !is_definitely_new {
            // Might exist: check trie first
            if let Some(idx) = self.get_index(term) {
                return Ok(idx);
            }
        }

        let index = self.next_unassigned_index_from(self.next_index.load(Ordering::Acquire));
        self.append_vocab_insert_wal(term, index)?;

        let inserted = self.insert_with_index_no_wal(term, index)?;
        debug_assert!(inserted, "newly allocated vocab term should insert");

        Ok(index)
    }

    /// Lock-free Order-A overlay insert — the flip write path (`&self`, concurrent-safe).
    ///
    /// Allocates a WRITE-ONCE id (`next_index.fetch_add` — nearly-dense: a lost InsertOnce
    /// race burns one id, rare) and durably publishes `(term -> id)` via the proven generic
    /// insert-once orchestrator (Order-A: WAL `Insert{value:id}` -> overlay root-CAS ->
    /// CommitRank -> mark_committed), then mirrors it into the lock-free reverse map. An
    /// existing term keeps its id (no id burned). Idempotent on a lost race (the durable
    /// orchestrator's own present-hoist returns `false`; the burned id's WAL Insert is a
    /// benign replay no-op under InsertOnce).
    fn insert_overlay(&self, term: &str) -> Result<u64> {
        if let Some(id) = self.get_index_lockfree(term) {
            return Ok(id);
        }
        let index = self.next_index.fetch_add(1, Ordering::AcqRel);
        let newly =
            <Self as crate::persistent_artrie_core::overlay::durable_write::DurableOverlayWrite<
                crate::persistent_artrie_core::key_encoding::CharKey,
                u64,
                S,
            >>::insert_cas_with_value_durable_default(self, term.as_bytes(), index)?;
        if newly {
            if let Some(ref rev) = self.reverse_term_map {
                rev.insert(index, term.to_string());
            }
            self.entry_count.fetch_add(1, Ordering::AcqRel);
            Ok(index)
        } else {
            // A concurrent insert won the term between the hoist and the CAS: return the
            // winner's id; our `index` is a benign gap.
            Ok(self.get_index_lockfree(term).unwrap_or(index))
        }
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
    /// ```rust,no_run
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # use libdictenstein::persistent_vocab_artrie::PersistentVocabARTrie;
    /// let mut vocab = PersistentVocabARTrie::create("vocab.vocab")?;
    /// let indices = vocab.insert_batch(&["apple", "banana", "cherry"])?;
    /// assert_eq!(indices, vec![0, 1, 2]);
    ///
    /// // Duplicate terms return existing indices
    /// let indices2 = vocab.insert_batch(&["apple", "date"])?;
    /// assert_eq!(indices2, vec![0, 3]); // "apple" already at 0
    /// # Ok(())
    /// # }
    /// ```
    pub fn insert_batch(&mut self, terms: &[&str]) -> Result<Vec<u64>> {
        // Flip routing: under route_overlay() each term takes the lock-free Order-A overlay
        // insert (the durable unit; no owned batch WAL record). Dead owned body below post-flip.
        if self.route_overlay() {
            return terms.iter().map(|&t| self.insert_overlay(t)).collect();
        }
        if terms.is_empty() {
            return Ok(Vec::new());
        }

        let mut indices = Vec::with_capacity(terms.len());
        let mut new_entries: Vec<(Vec<u8>, Option<Vec<u8>>)> = Vec::new();
        let mut new_term_indices: Vec<(&str, u64)> = Vec::new();
        let mut assigned_in_batch: HashMap<&str, u64> = HashMap::new();
        let mut next_candidate = self.next_index.load(Ordering::Acquire);

        // Phase 1: Collect indices, separating existing vs new terms
        for term in terms.iter().copied() {
            if let Some(index) = assigned_in_batch.get(term).copied() {
                indices.push(index);
                continue;
            }

            // Fast path: bloom filter says definitely NOT in vocabulary
            let is_definitely_new = self
                .bloom_filter
                .as_ref()
                .map(|b| !b.might_contain(term))
                .unwrap_or(false);

            if !is_definitely_new {
                // Might exist: check trie first
                if let Some(idx) = self.get_index(term) {
                    assigned_in_batch.insert(term, idx);
                    indices.push(idx);
                    continue;
                }
            }

            let index = self.next_unassigned_index_from(next_candidate);
            next_candidate = index.saturating_add(1);

            // Prepare for batch WAL record
            new_entries.push((term.as_bytes().to_vec(), Some(index.to_le_bytes().to_vec())));

            assigned_in_batch.insert(term, index);
            new_term_indices.push((term, index));
            indices.push(index);
        }

        // Phase 2: Log all new entries as single BatchInsert WAL record
        if !new_entries.is_empty() {
            self.append_vocab_batch_wal(&new_entries)?;

            // Phase 3: Insert new terms into trie (no individual WAL logging)
            for (term, index) in &new_term_indices {
                self.insert_with_index_no_wal(term, *index)?;
            }

            self.dirty.store(true, Ordering::Release);
        }

        Ok(indices)
    }

    /// Insert a term with a specific vocabulary index.
    ///
    /// # Returns
    ///
    /// `true` if the term was newly inserted, `false` if it already existed.
    pub fn insert_with_index(&mut self, term: &str, index: u64) -> Result<bool> {
        if self.route_overlay() {
            return self.insert_with_index_overlay(term, index);
        }
        if self.validate_index_insert(term, index)?.is_some() {
            return Ok(false);
        }

        self.append_vocab_insert_wal(term, index)?;
        self.insert_with_index_no_wal(term, index)
    }

    /// Lock-free Order-A overlay insert at a SPECIFIC id (the flip path for `insert_with_index`).
    /// Validates (id >= start_index; term not already at a different id; id not already assigned
    /// to a different term), durably publishes `term -> index` write-once, mirrors the reverse
    /// map, and raises the id floor. Returns `true` iff newly inserted.
    fn insert_with_index_overlay(&self, term: &str, index: u64) -> Result<bool> {
        if index < self.start_index {
            return Err(PersistentARTrieError::InvalidOperation(format!(
                "vocabulary index {index} is below start index {}",
                self.start_index
            )));
        }
        if let Some(existing) = self.get_index_lockfree(term) {
            if existing == index {
                return Ok(false);
            }
            return Err(PersistentARTrieError::InvalidOperation(format!(
                "term {term:?} is already assigned index {existing}, not {index}"
            )));
        }
        if let Some(ref rev) = self.reverse_term_map {
            if let Some(entry) = rev.get(&index) {
                if entry.value() != term {
                    return Err(PersistentARTrieError::InvalidOperation(format!(
                        "vocabulary index {index} is already assigned to term {:?}",
                        entry.value()
                    )));
                }
            }
        }
        let newly =
            <Self as crate::persistent_artrie_core::overlay::durable_write::DurableOverlayWrite<
                crate::persistent_artrie_core::key_encoding::CharKey,
                u64,
                S,
            >>::insert_cas_with_value_durable_default(self, term.as_bytes(), index)?;
        if newly {
            if let Some(ref rev) = self.reverse_term_map {
                rev.insert(index, term.to_string());
            }
            self.entry_count.fetch_add(1, Ordering::AcqRel);
            self.next_index.fetch_max(index + 1, Ordering::AcqRel);
        }
        Ok(newly)
    }

    pub(super) fn insert_with_index_no_wal(&mut self, term: &str, index: u64) -> Result<bool> {
        if self.validate_index_insert(term, index)?.is_some() {
            return Ok(false);
        }

        let chars: Vec<char> = term.chars().collect();
        let root_ref = NodeRef::new(0, 0);
        let mut ptr_to_ref = HashMap::with_capacity(self.node_map.len());
        for (node_ref, node_ptr) in &self.node_map {
            if ptr_to_ref.insert(*node_ptr, *node_ref).is_some() {
                return Err(PersistentARTrieError::CorruptedFile {
                    reason: "node_map assigns multiple NodeRefs to one live vocabulary node"
                        .to_string(),
                });
            }
        }

        match &mut self.root {
            VocabTrieRoot::Empty => {
                return Err(PersistentARTrieError::CorruptedFile {
                    reason: "Cannot insert into empty vocabulary root".to_string(),
                });
            }
            VocabTrieRoot::Node(root) => {
                // Navigate/create path to the term
                let mut current = root.as_mut();
                let mut current_ref = root_ref;

                for &c in chars.iter() {
                    if let Some(existing_child_ptr) = current
                        .get_child(c)
                        .map(|child| child as *const VocabTrieNode)
                    {
                        let child_ref =
                            ptr_to_ref
                                .get(&existing_child_ptr)
                                .copied()
                                .ok_or_else(|| PersistentARTrieError::CorruptedFile {
                                    reason: format!(
                                        "node_map missing live vocabulary child for edge {c:?}"
                                    ),
                                })?;
                        current_ref = child_ref;
                        current = current
                            .get_child_mut(c)
                            .expect("existing vocabulary child should be mutable");
                        continue;
                    }

                    // Assign a NodeRef only for a newly-created child.
                    let slot = self.next_slot;
                    self.next_slot += 1;
                    let child_ref = NodeRef::new(0, slot as u32);

                    // Get or create child with parent pointer
                    let child = current.get_or_create_child(c, current_ref);

                    // Update node map
                    self.node_map
                        .insert(child_ref, child as *const VocabTrieNode);
                    ptr_to_ref.insert(child as *const VocabTrieNode, child_ref);

                    current_ref = child_ref;
                    current = child;
                }

                // Check if already final
                if current.is_final() {
                    return Ok(false);
                }

                // Set value and mark final
                current.set_value(index);

                // Update reverse index
                if let Some(ref mut rev_idx) = self.reverse_index {
                    rev_idx.set(index, current_ref)?;
                }

                // Cache the term
                self.reverse_cache.put(index, term.to_string());

                // Update bloom filter
                if let Some(ref mut bloom) = self.bloom_filter {
                    bloom.insert(term);
                }

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
                        current,
                        new_val,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => break,
                        Err(_) => continue, // Retry
                    }
                }

                Ok(true)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::dict_impl::PersistentVocabARTrie;
    use std::sync::atomic::Ordering;
    use tempfile::tempdir;

    #[test]
    #[ignore = "no-WAL insert; the single-lock-free overlay makes the WAL mandatory (durable inserts need it), so this no-WAL scenario is unsupported; removed at V6/single-lock-free"]
    fn insert_without_wal_writer_preserves_state_and_allocator() {
        let dir = tempdir().expect("temp dir");
        let path = dir.path().join("wal_missing_insert.vocab");
        let mut vocab = PersistentVocabARTrie::create(&path).expect("create vocab");

        vocab.wal_writer = None;

        let error = vocab
            .insert("lost")
            .expect_err("missing WAL writer must reject insert");
        assert!(error.to_string().contains("WAL"));
        assert_eq!(vocab.len(), 0);
        assert_eq!(vocab.get_index("lost"), None);
        assert_eq!(vocab.get_term(0), None);
        assert_eq!(vocab.next_index.load(Ordering::Acquire), 0);
    }

    #[test]
    #[ignore = "drops the WAL mid-life; the overlay flip makes the WAL mandatory (route_overlay + durable inserts need it), so this no-WAL scenario is unsupported post-flip; removed at single-lock-free"]
    fn batch_without_wal_writer_preserves_state_and_allocator() {
        let dir = tempdir().expect("temp dir");
        let path = dir.path().join("wal_missing_batch.vocab");
        let mut vocab = PersistentVocabARTrie::create(&path).expect("create vocab");
        assert_eq!(vocab.insert("existing").expect("insert existing"), 0);

        vocab.wal_writer = None;
        let before_next = vocab.next_index.load(Ordering::Acquire);

        let error = vocab
            .insert_batch(&["new", "new", "another"])
            .expect_err("missing WAL writer must reject batch");
        assert!(error.to_string().contains("WAL"));
        assert_eq!(vocab.len(), 1);
        assert_eq!(vocab.get_index("existing"), Some(0));
        assert_eq!(vocab.get_index("new"), None);
        assert_eq!(vocab.get_index("another"), None);
        assert_eq!(vocab.next_index.load(Ordering::Acquire), before_next);
    }
}
