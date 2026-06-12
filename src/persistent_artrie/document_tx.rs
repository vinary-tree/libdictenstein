//! Document-transaction execution methods for `PersistentARTrie<V, S>`.
//!
//! Split out of byte `dict_impl.rs` (lines ~5698-5938, ~250 LOC) as part
//! of the Phase-5 decomposition. The `DocumentTransaction<V>` /
//! `TransactionState` data carriers themselves live in
//! `super::transactions`; this file holds the trie-side `begin_document`
//! / `tx_insert` / `tx_insert_bytes` / `tx_increment*` /
//! `commit_document` / `abort_document` methods that operate on those
//! transactions.

use std::sync::atomic::Ordering as AtomicOrdering;

use super::block_storage::BlockStorage;
use super::dict_impl::PersistentARTrie;
use super::error::{PersistentARTrieError, Result};
use super::transactions::{DocumentTransaction, TransactionState};
use crate::persistent_artrie::core::key_encoding::ByteKey;
use crate::persistent_artrie::core::overlay::durable_write::DurableOverlayWrite;
use crate::value::DictionaryValue;

impl<V: DictionaryValue, S: BlockStorage> PersistentARTrie<V, S> {
    /// Begin a new document transaction.
    ///
    /// Allocates a unique transaction ID, logs `BeginTx` to the WAL, and
    /// returns a buffered transaction that subsequent `tx_insert*` calls
    /// can append to. The buffered terms are not visible to the trie
    /// until `commit_document` is called.
    pub fn begin_document(&self, document_id: &str) -> Result<DocumentTransaction<V>> {
        let tx_id = {
            let base = self.next_lsn.load(AtomicOrdering::Acquire);
            // tx-ID hash = LSN ⊕ (low 64 bits of the nanos timestamp). The low 8 LE
            // bytes of the u128 nanos are the same value a u64 truncation would yield;
            // taken via `from_le_bytes` (a NON-counter value) to avoid a numeric cast
            // so the counter-codec gate stays clean for this file.
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let low8: [u8; 8] = nanos.to_le_bytes()[..8]
                .try_into()
                .expect("low 8 bytes of a u128");
            base ^ u64::from_le_bytes(low8)
        };

        // L3.3: the overlay `commit_document` is per-op durable (NOT bracketed), so no
        // orphan BeginTx WAL append (it would burn an un-`mark_committed` LSN that
        // stalls the committed watermark).
        Ok(DocumentTransaction::new_active(
            tx_id,
            document_id.to_string(),
        ))
    }

    /// Buffer a term in a document transaction.
    pub fn tx_insert(&self, tx: &mut DocumentTransaction<V>, term: &str, value: Option<V>) {
        self.tx_insert_bytes(tx, term.as_bytes(), value);
    }

    /// Buffer a term (as bytes) in a document transaction.
    pub fn tx_insert_bytes(&self, tx: &mut DocumentTransaction<V>, term: &[u8], value: Option<V>) {
        assert!(
            tx.state == TransactionState::Active,
            "Cannot insert into a {} transaction",
            match tx.state {
                TransactionState::Committed => "committed",
                TransactionState::Aborted => "aborted",
                TransactionState::Active => unreachable!(),
            }
        );
        tx.shadow_terms.push((term.to_vec(), value));
    }

    /// Buffer an increment operation in a document transaction.
    ///
    /// Compatibility wrapper for [`Self::try_tx_increment`]. Arithmetic or
    /// value-conversion failures poison the transaction; `commit_document`
    /// will return the deferred error without appending commit records.
    pub fn tx_increment(&self, tx: &mut DocumentTransaction<V>, term: &str, delta: i64) {
        self.tx_increment_bytes(tx, term.as_bytes(), delta);
    }

    /// Checked variant of [`Self::tx_increment`].
    pub fn try_tx_increment(
        &self,
        tx: &mut DocumentTransaction<V>,
        term: &str,
        delta: i64,
    ) -> Result<()> {
        self.try_tx_increment_bytes(tx, term.as_bytes(), delta)
    }

    /// Buffer an increment operation in a document transaction (byte key).
    pub fn tx_increment_bytes(&self, tx: &mut DocumentTransaction<V>, term: &[u8], delta: i64) {
        assert!(
            tx.is_active(),
            "Cannot increment in a {} transaction",
            match tx.state {
                TransactionState::Committed => "committed",
                TransactionState::Aborted => "aborted",
                TransactionState::Active => unreachable!(),
            }
        );

        if let Err(error) = self.try_tx_increment_bytes(tx, term, delta) {
            tx.mark_failed(error.to_string());
        }
    }

    /// Checked byte-key variant of [`Self::tx_increment_bytes`].
    pub fn try_tx_increment_bytes(
        &self,
        tx: &mut DocumentTransaction<V>,
        term: &[u8],
        delta: i64,
    ) -> Result<()> {
        if tx.state != TransactionState::Active {
            return Err(PersistentARTrieError::InvalidOperation(format!(
                "Cannot increment in a {} transaction",
                match tx.state {
                    TransactionState::Committed => "committed",
                    TransactionState::Aborted => "aborted",
                    TransactionState::Active => unreachable!(),
                }
            )));
        }

        if let Some(reason) = tx.failure_reason() {
            return Err(PersistentARTrieError::InvalidOperation(format!(
                "Cannot increment in failed transaction {}: {}",
                tx.document_id(),
                reason
            )));
        }

        // Mirror char: aggregate the per-term deltas already buffered in THIS tx + the new
        // delta and pre-check overflow while staging (a poisoned tx must not commit), then
        // buffer the RAW delta. Accumulation happens at COMMIT via the proven add-only
        // overlay counter (`route_increment_bytes`) reading the LIVE overlay value — NOT an
        // owned read (the owned tree is empty under the overlay, so the prior
        // `get_value_impl` base read 0 and the folded absolute SET silently overwrote the
        // live count). Two documents incrementing one counter now ACCUMULATE (owner decision
        // 2026-06-09), concurrency-safe (the commit-time CAS read-modify-write), matching
        // char doc-tx + the single-op `increment`.
        let pending_delta =
            tx.increments
                .iter()
                .try_fold(0_i64, |acc, (existing_term, existing_delta)| {
                    if existing_term == term {
                        acc.checked_add(*existing_delta)
                    } else {
                        Some(acc)
                    }
                });
        let aggregate = match pending_delta.and_then(|pending| pending.checked_add(delta)) {
            Some(value) => value,
            None => {
                let reason = format!(
                    "transaction increment aggregate overflow for term {:?}",
                    String::from_utf8_lossy(term)
                );
                tx.mark_failed(reason.clone());
                return Err(PersistentARTrieError::InvalidOperation(reason));
            }
        };
        let _ = aggregate;
        tx.increments.push((term.to_vec(), delta));
        Ok(())
    }

    /// Commit a document transaction, atomically applying all buffered terms.
    ///
    /// **M3 reject (BROKEN-BY-DESIGN, audit §B #8).** Applies the buffered terms via
    /// `insert_impl_core` (owned absolute write). Reject under `route_overlay()` (the
    /// second entry-point guard; `begin_document` already rejects, but a transaction
    /// could have been opened on the owned path then flipped — fail loud here too).
    /// Takes `&self` (not `&mut self`): both the overlay arm (the production default)
    /// and the owned arm apply via interior mutability, so an `Arc<PersistentARTrie>`
    /// can commit chunked transactions without exclusive access — required by lock-free
    /// embedders that also arm `enable_eviction` (which needs a bare `Arc`, not `&mut`).
    pub fn commit_document(&self, mut tx: DocumentTransaction<V>) -> Result<usize>
    where
        V: Clone,
    {
        if tx.state != TransactionState::Active {
            return Err(PersistentARTrieError::InvalidOperation(format!(
                "Cannot commit a {} transaction",
                match tx.state {
                    TransactionState::Committed => "committed",
                    TransactionState::Aborted => "aborted",
                    TransactionState::Active => unreachable!(),
                }
            )));
        }

        if let Some(reason) = tx.failure_reason() {
            return Err(PersistentARTrieError::InvalidOperation(format!(
                "Cannot commit failed transaction {}: {}",
                tx.document_id(),
                reason
            )));
        }

        // Per-op durable, NOT batch-atomic (no BeginTx/CommitTx/sync — each primitive
        // writes its own durable, ranked record). SETs via upsert (valued) / membership
        // insert; increments ACCUMULATE via the proven add-only overlay counter
        // (`route_increment_bytes` — counter-monomorph u64 only, the `&self`-commit
        // constraint shared with char; a non-counter `V` or a NET-NEGATIVE aggregate is
        // rejected). The negative-aggregate reject runs BEFORE any SET so a rejected commit
        // applies nothing (closer to all-or-nothing on reject). (Owner decision 2026-06-09:
        // cross-document increments to one counter accumulate — the prior SET-from-empty-
        // owned-base silently overwrote the live count.)
        let set_count = tx.shadow_terms.len();
        let increment_count = tx.increments.len();
        let total_operations = set_count + increment_count;

        let mut aggregated: std::collections::HashMap<Vec<u8>, i64> =
            std::collections::HashMap::with_capacity(increment_count);
        for (term, delta) in &tx.increments {
            let entry = aggregated.entry(term.clone()).or_insert(0);
            *entry = entry.checked_add(*delta).ok_or_else(|| {
                PersistentARTrieError::InvalidOperation(format!(
                    "transaction increment aggregate overflow for term {:?}",
                    String::from_utf8_lossy(term)
                ))
            })?;
        }
        for (term, agg) in &aggregated {
            if *agg < 0 {
                return Err(PersistentARTrieError::InvalidOperation(format!(
                    "document-tx increment aggregate for term {:?} is negative ({}); the overlay \
                     counter is add-only",
                    String::from_utf8_lossy(term),
                    agg
                )));
            }
        }
        for (term, value) in tx.shadow_terms.drain(..) {
            match value {
                Some(v) => {
                    <Self as DurableOverlayWrite<ByteKey, V, S>>::upsert_cas_durable_default(
                        self, &term, v,
                    )?;
                }
                None => {
                    self.insert_cas_durable(&term)?;
                }
            }
        }
        for (term, agg) in aggregated {
            if agg == 0 {
                continue;
            }
            match super::lockfree_value_route::route_increment_bytes(self, &term, agg) {
                Some(r) => {
                    r?;
                }
                None => {
                    return Err(PersistentARTrieError::InvalidOperation(
                        "document-tx increments require a counter value type (u64)".to_string(),
                    ));
                }
            }
        }
        tx.increments.clear();
        tx.state = TransactionState::Committed;
        Ok(total_operations)
    }

    /// Abort a document transaction, discarding all buffered terms.
    pub fn abort_document(&self, mut tx: DocumentTransaction<V>) -> Result<()> {
        if tx.state != TransactionState::Active {
            return Err(PersistentARTrieError::InvalidOperation(format!(
                "Cannot abort a {} transaction",
                match tx.state {
                    TransactionState::Committed => "committed",
                    TransactionState::Aborted => "aborted",
                    TransactionState::Active => unreachable!(),
                }
            )));
        }

        // L3.3: the overlay tx buffered nothing visible (no BeginTx written), so there is
        // nothing to bracket-abort — just discard the shadow + increments.
        tx.shadow_terms.clear();
        tx.increments.clear();
        tx.state = TransactionState::Aborted;

        Ok(())
    }
}
