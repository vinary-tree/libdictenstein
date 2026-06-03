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
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::persistent_artrie::wal::WalRecord;
use crate::value::DictionaryValue;

use super::transactions::CharDocumentTransaction;
use crate::persistent_artrie::TransactionState;

impl<V: DictionaryValue, S: BlockStorage> super::PersistentARTrieChar<V, S> {
    pub fn begin_document(&self, document_id: &str) -> Result<CharDocumentTransaction<V>> {
        // Generate a unique transaction ID
        let tx_id = {
            let base = self.next_lsn.load(AtomicOrdering::Acquire);
            // Combine LSN with a random component for uniqueness
            base ^ (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0))
        };

        // Log BeginTx to WAL (routes through group commit if enabled)
        self.append_to_wal(WalRecord::BeginTx { tx_id })?;

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
    pub fn commit_document(&mut self, mut tx: CharDocumentTransaction<V>) -> Result<usize>
    where
        V: Clone,
    {
        // Flip gap (design §1, named residual): multi-op document transactions are
        // UNSUPPORTED under the lock-free overlay — the apply step below writes the
        // OWNED tree (via `try_insert_impl_no_wal*`), which the overlay-default read
        // path and the overlay checkpoint would NOT see (silent data loss). Rather
        // than risk that, reject the commit under `route_overlay()` (PS3-guarded).
        // Overlay-native document-tx apply ("same WAL records, overlay apply") is a
        // named follow-on. Callers needing transactions use the OwnedTree kill-
        // switch mode. `abort_document` stays valid (it applies nothing).
        if self.route_overlay() {
            return Err(PersistentARTrieError::InvalidOperation(
                "document transactions are not supported under the lock-free overlay write mode \
                 (commit_document applies to the owned tree, which the overlay read/checkpoint \
                 path does not observe); use OverlayWriteMode::OwnedTree for transactions, or \
                 the single-op insert/increment/upsert which route to the overlay"
                    .to_string(),
            ));
        }

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

        if set_count == 0 && increment_count == 0 {
            // Empty transaction - just log commit (routes through group commit if enabled)
            self.append_to_wal(WalRecord::CommitTx { tx_id: tx.tx_id })?;
            // Sync WAL to ensure CommitTx is durable (ACID Durability)
            self.sync_wal()?;
            tx.state = TransactionState::Committed;
            return Ok(0);
        }

        let total_operations = set_count + increment_count;
        let mut set_wal_entries = Vec::with_capacity(set_count);
        let mut prepared_sets = Vec::with_capacity(set_count);
        let mut prepared_set_i64: HashMap<Vec<u8>, i64> = HashMap::with_capacity(set_count);

        for (term_bytes, value) in &tx.shadow_terms {
            let term_str = String::from_utf8_lossy(term_bytes).into_owned();
            if value.is_some() {
                self.preflight_insert_with_value_no_wal(&term_str)?;
            } else {
                let _ = self.preflight_insert_no_wal(&term_str)?;
            }

            let value_bytes = match value.as_ref() {
                Some(v) => Some(crate::serialization::bincode_compat::serialize(v).map_err(
                    |e| {
                        PersistentARTrieError::internal(format!(
                            "Failed to serialize transaction value: {}",
                            e
                        ))
                    },
                )?),
                None => None,
            };
            set_wal_entries.push((term_bytes.clone(), value_bytes));
            prepared_sets.push((term_str, value.clone()));
            prepared_set_i64.insert(
                term_bytes.clone(),
                value.as_ref().map(Self::i64_from_value_lossy).unwrap_or(0),
            );
        }

        // Aggregate increments for the same term within the transaction.
        let mut aggregated_increments: HashMap<Vec<u8>, i64> =
            HashMap::with_capacity(increment_count);
        for (term_bytes, delta) in &tx.increments {
            let entry = aggregated_increments.entry(term_bytes.clone()).or_insert(0);
            *entry = entry.checked_add(*delta).ok_or_else(|| {
                PersistentARTrieError::InvalidOperation(format!(
                    "transaction increment aggregate overflow for term {:?}",
                    String::from_utf8_lossy(term_bytes)
                ))
            })?;
        }

        let mut increment_wal_entries = Vec::with_capacity(aggregated_increments.len());
        let mut prepared_increments = Vec::with_capacity(aggregated_increments.len());
        for (term_bytes, delta) in aggregated_increments {
            let term_str = String::from_utf8_lossy(&term_bytes).into_owned();
            self.preflight_insert_with_value_no_wal(&term_str)?;
            let current = prepared_set_i64
                .get(&term_bytes)
                .copied()
                .unwrap_or_else(|| self.current_i64_for_increment(&term_str));
            let new_value = current.checked_add(delta).ok_or_else(|| {
                PersistentARTrieError::InvalidOperation(format!(
                    "transaction increment overflow for term {:?}: {} + {} exceeds i64 range",
                    term_str, current, delta
                ))
            })?;
            let value = Self::value_from_i64_checked(new_value)?;
            increment_wal_entries.push((term_bytes, delta));
            prepared_increments.push((term_str, value));
        }

        if !set_wal_entries.is_empty() {
            self.append_to_wal(WalRecord::BatchInsert {
                entries: set_wal_entries,
            })?;
        }

        if !increment_wal_entries.is_empty() {
            let batch_record = WalRecord::BatchIncrement {
                entries: increment_wal_entries,
            };
            self.append_to_wal(batch_record)?;
        }

        // Log CommitTx (routes through group commit if enabled)
        self.append_to_wal(WalRecord::CommitTx { tx_id: tx.tx_id })?;
        // Sync WAL to ensure CommitTx is durable (ACID Durability)
        self.sync_wal()?;

        for (term, value) in prepared_sets {
            if let Some(v) = value {
                self.try_insert_impl_no_wal_with_value(&term, v)?;
            } else {
                self.try_insert_impl_no_wal(&term)?;
            }
        }

        for (term, value) in prepared_increments {
            self.try_insert_impl_no_wal_with_value(&term, value)?;
        }

        tx.shadow_terms.clear();
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

        // Log AbortTx to WAL (routes through group commit if enabled)
        self.append_to_wal(WalRecord::AbortTx { tx_id: tx.tx_id })?;

        // Discard buffered terms (happens automatically via drop)
        tx.state = TransactionState::Aborted;
        Ok(())
    }

    fn current_i64_for_increment(&self, term: &str) -> i64 {
        self.get(term).map(Self::i64_from_value_lossy).unwrap_or(0)
    }

    fn i64_from_value_lossy(value: &V) -> i64 {
        let bytes = crate::serialization::bincode_compat::serialize(value).unwrap_or_default();
        if bytes.len() == 8 {
            let raw: [u8; 8] = bytes.try_into().expect("expected 8 bytes");
            i64::from_le_bytes(raw)
        } else {
            crate::serialization::bincode_compat::deserialize::<i64>(&bytes).unwrap_or(0)
        }
    }

    fn value_from_i64_checked(value: i64) -> Result<V> {
        let value_bytes =
            crate::serialization::bincode_compat::serialize(&value).map_err(|error| {
                PersistentARTrieError::internal(format!(
                    "Failed to serialize transaction increment value: {}",
                    error
                ))
            })?;
        crate::serialization::bincode_compat::deserialize(&value_bytes).map_err(|error| {
            PersistentARTrieError::internal(format!(
                "Failed to deserialize transaction increment value as V: {}",
                error
            ))
        })
    }
}
