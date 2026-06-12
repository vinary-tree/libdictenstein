//! Document-transaction execution methods for `PersistentARTrieChar<V, S>`.
//!
//! Split out of char `dict_impl_char.rs` (lines ~502-829, ~328 LOC)
//! as a Phase-6 char sub-module, mirroring the byte
//! `super::document_tx` split. Methods covered:
//!
//! - `begin_document` — logs BeginTx + constructs CharDocumentTransaction
//! - `tx_insert` / `tx_insert_chars` / `tx_insert_bytes` — buffer terms
//! - `tx_increment` / `tx_increment_bytes` — buffer increment operations
//! - `commit_document` — atomically apply all buffered terms
//! - `abort_document` — discard buffered terms
//!
//! The `CharDocumentTransaction<V>` data type lives in
//! `super::transactions`.

use std::collections::HashMap;
use std::sync::atomic::Ordering as AtomicOrdering;

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::core::key_encoding::CharKey;
use crate::persistent_artrie::core::overlay::durable_write::DurableOverlayWrite;
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::value::DictionaryValue;

use super::transactions::CharDocumentTransaction;
use crate::persistent_artrie::TransactionState;

impl<V: DictionaryValue, S: BlockStorage> super::PersistentARTrieChar<V, S> {
    pub fn begin_document(&self, document_id: &str) -> Result<CharDocumentTransaction<V>> {
        // Generate a unique transaction ID
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
        // orphan BeginTx WAL append (it would burn an un-`mark_committed` LSN that stalls
        // the committed watermark and thus checkpoint reclaim).
        Ok(CharDocumentTransaction {
            tx_id,
            document_id: document_id.to_string(),
            shadow_terms: Vec::new(),
            increments: Vec::new(),
            failure: None,
            state: TransactionState::Active,
        })
    }

    /// Buffer a term in a document transaction.
    ///
    /// The term is NOT inserted into the trie yet - it's only buffered in memory.
    /// The term will be inserted when `commit_document()` is called.
    ///
    /// # Arguments
    ///
    /// * `tx` - The active transaction to buffer the term in
    /// * `term` - The term to insert (as a string)
    /// * `value` - Optional value to associate with the term
    ///
    /// # Panics
    ///
    /// Panics if the transaction is not in Active state.
    pub fn tx_insert(&self, tx: &mut CharDocumentTransaction<V>, term: &str, value: Option<V>) {
        assert!(
            tx.is_active(),
            "Cannot insert into a {} transaction",
            match tx.state {
                TransactionState::Committed => "committed",
                TransactionState::Aborted => "aborted",
                TransactionState::Active => unreachable!(),
            }
        );

        tx.shadow_terms.push((term.as_bytes().to_vec(), value));
    }

    /// Buffer a term (as char slice) in a document transaction.
    ///
    /// This method accepts a slice of characters directly, which is useful when
    /// working with pre-parsed Unicode data or when you want to avoid UTF-8
    /// encoding overhead.
    ///
    /// The term is NOT inserted into the trie yet - it's only buffered in memory.
    /// The term will be inserted when `commit_document()` is called.
    ///
    /// # Arguments
    ///
    /// * `tx` - The active transaction to buffer the term in
    /// * `chars` - The term characters to insert
    /// * `value` - Optional value to associate with the term
    ///
    /// # Panics
    ///
    /// Panics if the transaction is not in Active state.
    pub fn tx_insert_chars(
        &self,
        tx: &mut CharDocumentTransaction<V>,
        chars: &[char],
        value: Option<V>,
    ) {
        assert!(
            tx.is_active(),
            "Cannot insert into a {} transaction",
            match tx.state {
                TransactionState::Committed => "committed",
                TransactionState::Aborted => "aborted",
                TransactionState::Active => unreachable!(),
            }
        );

        // Convert chars to UTF-8 string bytes for WAL storage
        let term_str: String = chars.iter().collect();
        tx.shadow_terms.push((term_str.into_bytes(), value));
    }

    /// Buffer a term (as bytes) in a document transaction.
    ///
    /// This method accepts raw UTF-8 bytes, which is useful when you already
    /// have byte data and want to avoid conversion overhead.
    ///
    /// # Arguments
    ///
    /// * `tx` - The active transaction to buffer the term in
    /// * `term_bytes` - The term bytes to insert (must be valid UTF-8)
    /// * `value` - Optional value to associate with the term
    ///
    /// # Panics
    ///
    /// Panics if the transaction is not in Active state.
    pub fn tx_insert_bytes(
        &self,
        tx: &mut CharDocumentTransaction<V>,
        term_bytes: &[u8],
        value: Option<V>,
    ) {
        assert!(
            tx.is_active(),
            "Cannot insert into a {} transaction",
            match tx.state {
                TransactionState::Committed => "committed",
                TransactionState::Aborted => "aborted",
                TransactionState::Active => unreachable!(),
            }
        );

        tx.shadow_terms.push((term_bytes.to_vec(), value));
    }

    /// Buffer an increment operation in a document transaction.
    ///
    /// Unlike `tx_insert()` which uses SET semantics, this accumulates the delta
    /// with any existing value when the transaction commits. Multiple increments
    /// to the same term within a transaction are aggregated.
    ///
    /// # Arguments
    ///
    /// * `tx` - The active transaction to buffer the increment in
    /// * `term` - The term to increment
    /// * `delta` - The amount to add (can be negative)
    ///
    /// # Panics
    ///
    /// Panics if the transaction is not in Active state.
    ///
    /// # Example
    ///
    /// ```text
    /// let mut tx = trie.begin_document("file1")?;
    /// trie.tx_increment(&mut tx, "the|quick", 100);
    /// trie.tx_increment(&mut tx, "the|quick", 50);  // Accumulates: will add 150
    /// trie.commit_document(tx)?;  // Adds 150 to existing value
    /// ```
    pub fn tx_increment(&self, tx: &mut CharDocumentTransaction<V>, term: &str, delta: i64) {
        assert!(
            tx.is_active(),
            "Cannot increment in a {} transaction",
            match tx.state {
                TransactionState::Committed => "committed",
                TransactionState::Aborted => "aborted",
                TransactionState::Active => unreachable!(),
            }
        );

        if let Err(error) = self.try_tx_increment(tx, term, delta) {
            tx.mark_failed(error.to_string());
        }
    }

    /// Checked variant of [`Self::tx_increment`].
    pub fn try_tx_increment(
        &self,
        tx: &mut CharDocumentTransaction<V>,
        term: &str,
        delta: i64,
    ) -> Result<()> {
        self.try_tx_increment_bytes(tx, term.as_bytes(), delta)
    }

    /// Buffer an increment operation (as bytes) in a document transaction.
    ///
    /// This variant accepts raw UTF-8 bytes directly.
    ///
    /// # Arguments
    ///
    /// * `tx` - The active transaction to buffer the increment in
    /// * `term_bytes` - The term bytes to increment (must be valid UTF-8)
    /// * `delta` - The amount to add (can be negative)
    ///
    /// # Panics
    ///
    /// Panics if the transaction is not in Active state.
    pub fn tx_increment_bytes(
        &self,
        tx: &mut CharDocumentTransaction<V>,
        term_bytes: &[u8],
        delta: i64,
    ) {
        assert!(
            tx.is_active(),
            "Cannot increment in a {} transaction",
            match tx.state {
                TransactionState::Committed => "committed",
                TransactionState::Aborted => "aborted",
                TransactionState::Active => unreachable!(),
            }
        );

        if let Err(error) = self.try_tx_increment_bytes(tx, term_bytes, delta) {
            tx.mark_failed(error.to_string());
        }
    }

    /// Checked byte-key variant of [`Self::tx_increment_bytes`].
    pub fn try_tx_increment_bytes(
        &self,
        tx: &mut CharDocumentTransaction<V>,
        term_bytes: &[u8],
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

        let pending_delta =
            tx.increments
                .iter()
                .try_fold(0_i64, |acc, (existing_term, existing_delta)| {
                    if existing_term == term_bytes {
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
                    String::from_utf8_lossy(term_bytes)
                );
                tx.mark_failed(reason.clone());
                return Err(PersistentARTrieError::InvalidOperation(reason));
            }
        };

        let _ = aggregate;
        tx.increments.push((term_bytes.to_vec(), delta));
        Ok(())
    }

    /// Commit a document transaction, applying all buffered operations atomically.
    ///
    /// This method writes all buffered SET and INCREMENT operations to the WAL
    /// as batch records, then applies them to the trie. This ensures that either
    /// all operations are committed or none are (crash atomicity via WAL).
    ///
    /// # Arguments
    ///
    /// * `tx` - The transaction to commit (consumed)
    ///
    /// # Returns
    ///
    /// The total number of operations committed (SETs + INCREMENTs).
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The transaction is not in Active state
    /// - WAL write fails
    /// Takes `&self` (not `&mut self`): both the overlay arm (the production default)
    /// and the owned arm apply via interior mutability, so an `Arc<PersistentARTrieChar>`
    /// can commit chunked transactions without exclusive access — required by lock-free
    /// embedders that also arm `enable_eviction` (which needs a bare `Arc`, not `&mut`).
    pub fn commit_document(&self, mut tx: CharDocumentTransaction<V>) -> Result<usize>
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

        let set_count = tx.shadow_terms.len();
        let increment_count = tx.increments.len();

        // L3.3: the overlay is the sole representation. Per-op durable, NOT batch-atomic.
        // SETs via upsert (valued) / membership insert; increments via the proven add-only
        // overlay counter (counter-monomorph only) with a NEGATIVE-aggregate reject
        // preflight (char's owned aggregation checked overflow only, not sign). No
        // BeginTx/CommitTx/sync — each primitive writes its own durable, ranked record
        // (matches owned recovery, which ignored tx brackets on replay).
        let total_operations = set_count + increment_count;
        // Aggregate increments + reject a negative aggregate BEFORE applying any SET, so a
        // rejected commit applies nothing (closer to all-or-nothing on reject).
        let mut aggregated: HashMap<Vec<u8>, i64> = HashMap::with_capacity(increment_count);
        for (term_bytes, delta) in &tx.increments {
            let e = aggregated.entry(term_bytes.clone()).or_insert(0);
            *e = e.checked_add(*delta).ok_or_else(|| {
                PersistentARTrieError::InvalidOperation(format!(
                    "transaction increment aggregate overflow for term {:?}",
                    String::from_utf8_lossy(term_bytes)
                ))
            })?;
        }
        for (term_bytes, agg) in &aggregated {
            if *agg < 0 {
                return Err(PersistentARTrieError::InvalidOperation(format!(
                    "overlay document-tx increment aggregate for term {:?} is negative \
                     ({}); the overlay counter is add-only",
                    String::from_utf8_lossy(term_bytes),
                    agg
                )));
            }
        }
        // Apply SETs: upsert (valued) / membership insert (None).
        for (term_bytes, value) in tx.shadow_terms.drain(..) {
            match value {
                Some(v) => {
                    <Self as DurableOverlayWrite<CharKey, V, S>>::upsert_cas_durable_default(
                        self,
                        &term_bytes,
                        v,
                    )?;
                }
                None => {
                    let term_str = String::from_utf8_lossy(&term_bytes).into_owned();
                    self.insert_cas_durable(&term_str)?;
                }
            }
        }
        // Apply increments (counter-monomorph only; route_increment downcasts to u64 and
        // returns None for a non-counter V).
        for (term_bytes, agg) in aggregated {
            if agg == 0 {
                continue;
            }
            let term_str = String::from_utf8_lossy(&term_bytes).into_owned();
            match super::lockfree_value_route::route_increment(self, &term_str, agg) {
                Some(r) => {
                    r?;
                }
                None => {
                    return Err(PersistentARTrieError::InvalidOperation(
                        "overlay document-tx increments require a counter value type (u64)"
                            .to_string(),
                    ));
                }
            }
        }
        tx.increments.clear();
        tx.state = TransactionState::Committed;
        Ok(total_operations)
    }

    /// Abort a document transaction, discarding all buffered terms.
    ///
    /// This method logs AbortTx to WAL and discards the buffered terms.
    /// No terms are inserted into the trie.
    ///
    /// # Arguments
    ///
    /// * `tx` - The transaction to abort (consumed)
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The transaction is not in Active state
    /// - WAL write fails
    pub fn abort_document(&self, mut tx: CharDocumentTransaction<V>) -> Result<()> {
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
        // nothing to bracket-abort — just discard the shadow (consumed `tx` drops it).
        tx.state = TransactionState::Aborted;
        Ok(())
    }
}
