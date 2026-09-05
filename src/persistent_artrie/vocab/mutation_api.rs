//! Public mutation API for `PersistentVocabARTrie<S>` — OVERLAY-ONLY (V6).
//!
//! - `insert` — term → auto-assigned write-once u64 index
//! - `insert_batch` — bulk insert (each term is a durable lock-free Order-A insert)
//! - `insert_with_index` — insert at a specific vocabulary index
//!
//! All route through the lock-free overlay (`insert_overlay` / `insert_with_index_overlay`);
//! the owned tree and its WAL helpers are deleted. The public `&mut self` mutators (the
//! `MutableMappedDictionary` contract) are thin wrappers over the `&self` overlay inserts.

use std::path::Path;
use std::sync::atomic::Ordering;

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::{AtomProfile, AtomSequence};

impl<S: BlockStorage> super::dict_impl::PersistentVocabARTrie<S> {
    /// Insert a term and auto-assign the next vocabulary index. Returns the assigned index.
    ///
    /// Lock-free + concurrent-safe (`&self`): multiple threads may insert through a shared
    /// `Arc<PersistentVocabARTrie>` with no external locking (the single lock-free impl —
    /// no `install_overlay` toggle, no `ConcurrentVocabARTrie` wrapper).
    pub fn insert(&self, term: &str) -> Result<u64> {
        self.insert_overlay(term)
    }

    /// Insert one profile-owned Unicode-scalar sequence using the durable
    /// vocabulary insertion path.
    pub fn insert_atom_sequence<P>(&self, sequence: &AtomSequence<P>) -> Result<u64>
    where
        P: AtomProfile<Atom = char>,
    {
        let term: String = sequence.as_atoms().iter().collect();
        self.insert(&term)
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
            <Self as crate::persistent_artrie::core::overlay::durable_write::DurableOverlayWrite<
                crate::persistent_artrie::core::key_encoding::CharKey,
                u64,
                S,
            >>::insert_cas_with_value_durable_default(self, term.as_bytes(), index)?;
        if newly {
            if let Some(ref rev) = self.reverse_term_map {
                rev.insert(index, term.to_string());
            }
            self.entry_count.fetch_add(1, Ordering::AcqRel);
            self.dirty.store(true, Ordering::Release);
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
    /// # use libdictenstein::persistent_artrie::vocab::PersistentVocabARTrie;
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
            <Self as crate::persistent_artrie::core::overlay::durable_write::DurableOverlayWrite<
                crate::persistent_artrie::core::key_encoding::CharKey,
                u64,
                S,
            >>::insert_cas_with_value_durable_default(self, term.as_bytes(), index)?;
        if newly {
            if let Some(ref rev) = self.reverse_term_map {
                rev.insert(index, term.to_string());
            }
            self.entry_count.fetch_add(1, Ordering::AcqRel);
            self.next_index.fetch_max(index + 1, Ordering::AcqRel);
            self.dirty.store(true, Ordering::Release);
        }
        Ok(newly)
    }

    /// Fork this vocabulary into a NEW, fully independent on-disk copy at `path`.
    ///
    /// Unlike [`Clone`](Self::clone) (a read-only in-memory snapshot that shares the immutable
    /// overlay and is detached from storage), `fork_to` creates a fresh mmap-backed trie with its
    /// OWN file, WAL, arena, and buffer manager, then replays every `(term, id)` pair — so the
    /// fork is independently writable and separately persistable, and shares NOTHING with `self`
    /// (either can be mutated and dropped without affecting the other). It works on any backend
    /// `S`, including a storage-less snapshot.
    ///
    /// The exact id assignment is preserved (including burned-id gaps), as is the durability
    /// policy (adopted AFTER replay, so a `Periodic`/`None` source still replays under the fork's
    /// default `Immediate`). On any error mid-replay the partial fork files are removed, so a
    /// failed fork leaves nothing behind. `path` must not already exist.
    pub fn fork_to<P: AsRef<Path>>(
        &self,
        path: P,
    ) -> Result<super::dict_impl::PersistentVocabARTrie> {
        // Fresh independent storage + WAL at the new path (errors if `path` exists — no clobber).
        let fork = super::dict_impl::PersistentVocabARTrie::create_with_start_index(
            path.as_ref(),
            self.start_index,
        )?;

        // Replay + finalize; on any error, clean up the partial fork below.
        let outcome = (|| -> Result<()> {
            // Point-in-time consistent capture of the source overlay's `(term-units, id)` pairs
            // (a single DFS over the immutable root snapshot).
            let pairs = <Self as crate::persistent_artrie::core::overlay::flip::LockFreeOverlay<
                crate::persistent_artrie::core::key_encoding::CharKey,
                u64,
                S,
            >>::overlay_collect_units_with_values(self, &[])
            .unwrap_or_default();

            for (units, id) in pairs {
                let term: String = units.iter().filter_map(|&c| char::from_u32(c)).collect();
                // Id-preserving insert (validates the bijection; each is a distinct new term).
                fork.insert_with_index(&term, id)?;
            }

            // Preserve the exact next-id frontier (replay only raised it to max(id)+1; a source
            // that burned ids past its highest term keeps its higher frontier).
            fork.next_index
                .fetch_max(self.next_index.load(Ordering::Acquire), Ordering::AcqRel);

            // Adopt the source's durability policy AFTER replay.
            fork.set_durability_policy(self.durability_policy());

            // Publish an independent, reopenable checkpoint image to the fork's own file.
            fork.checkpoint()
        })();

        match outcome {
            Ok(()) => {
                debug_assert_eq!(
                    fork.entry_count.load(Ordering::Acquire),
                    fork.reverse_term_map.as_ref().map_or(0, |m| m.len()),
                    "fork entry_count must equal its reverse-map size"
                );
                Ok(fork)
            }
            Err(error) => {
                // Remove the partial fork so a mid-replay failure leaves nothing behind. Drop the
                // fork first (releases the file + WAL handles), then unlink the image + WAL.
                let fork_path = fork.path.clone();
                drop(fork);
                let _ = std::fs::remove_file(&fork_path);
                let _ = std::fs::remove_file(fork_path.with_extension("vocab.wal"));
                Err(error)
            }
        }
    }

    /// Take a lossless in-memory read-only snapshot — an alias for [`Clone::clone`] that names the
    /// intent. The snapshot shares the immutable overlay and is detached from storage; use
    /// [`Self::fork_to`] for an independent, writable, separately-persistable copy.
    pub fn snapshot(&self) -> Self {
        self.clone()
    }
}
