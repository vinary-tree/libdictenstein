//! Public mutation API for `PersistentVocabARTrie<S>` — OVERLAY-ONLY (V6).
//!
//! - `insert` — term → auto-assigned write-once u64 index
//! - `insert_batch` — bulk insert (each term is a durable lock-free Order-A insert)
//! - `insert_with_index` — insert at a specific vocabulary index
//!
//! All route through the lock-free overlay (`insert_overlay` / `insert_with_index_overlay`);
//! the owned tree and its WAL helpers are deleted. The public `&mut self` mutators (the
//! `MutableMappedDictionary` contract) are thin wrappers over the `&self` overlay inserts.

use std::sync::atomic::Ordering;

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::error::{PersistentARTrieError, Result};

impl<S: BlockStorage> super::dict_impl::PersistentVocabARTrie<S> {
    /// Insert a term and auto-assign the next vocabulary index. Returns the assigned index.
    ///
    /// Lock-free + concurrent-safe (`&self`): multiple threads may insert through a shared
    /// `Arc<PersistentVocabARTrie>` with no external locking (the single lock-free impl —
    /// no `enable_lockfree` toggle, no `ConcurrentVocabARTrie` wrapper).
    pub fn insert(&self, term: &str) -> Result<u64> {
        self.insert_overlay(term)
    }

    /// Lock-free Order-A overlay insert — the write path (`&self`, concurrent-safe).
    ///
    /// Allocates a WRITE-ONCE id (`next_index.fetch_add` — nearly-dense: a lost InsertOnce
    /// race burns one id, rare) and durably publishes `(term -> id)` via the proven generic
    /// insert-once orchestrator (Order-A: WAL `Insert{value:id}` -> overlay root-CAS ->
    /// CommitRank -> mark_committed), then mirrors it into the lock-free reverse map. An
    /// existing term keeps its id (no id burned). Idempotent on a lost race (the durable
    /// orchestrator's present-hoist returns `false`; the burned id's WAL Insert is a benign
    /// replay no-op under InsertOnce).
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

    /// Bulk insert multiple terms; each is a durable lock-free Order-A insert.
    /// Returns the assigned indices (existing terms return their existing index).
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # use libdictenstein::persistent_vocab_artrie::PersistentVocabARTrie;
    /// let mut vocab = PersistentVocabARTrie::create("vocab.vocab")?;
    /// let indices = vocab.insert_batch(&["apple", "banana", "cherry"])?;
    /// assert_eq!(indices, vec![0, 1, 2]);
    /// # Ok(())
    /// # }
    /// ```
    pub fn insert_batch(&self, terms: &[&str]) -> Result<Vec<u64>> {
        terms.iter().map(|&t| self.insert_overlay(t)).collect()
    }

    /// Insert a term with a specific vocabulary index (lock-free, `&self`). Returns `true` iff
    /// newly inserted.
    pub fn insert_with_index(&self, term: &str, index: u64) -> Result<bool> {
        self.insert_with_index_overlay(term, index)
    }

    /// Lock-free Order-A overlay insert at a SPECIFIC id. Validates (id >= start_index; term
    /// not already at a different id; id not already assigned to a different term), durably
    /// publishes `term -> index` write-once, mirrors the reverse map, and raises the id floor.
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
}
