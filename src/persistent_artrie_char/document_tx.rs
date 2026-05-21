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

        tx.increments.push((term.as_bytes().to_vec(), delta));
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

        tx.increments.push((term_bytes.to_vec(), delta));
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

        let set_count = tx.shadow_terms.len();
        let increment_count = tx.increments.len();

        if set_count == 0 && increment_count == 0 {
            // Empty transaction - just log commit (routes through group commit if enabled)
            tx.state = TransactionState::Committed;
            self.append_to_wal(WalRecord::CommitTx { tx_id: tx.tx_id })?;
            // Sync WAL to ensure CommitTx is durable (ACID Durability)
            self.sync_wal()?;
            return Ok(0);
        }

        let mut total_operations = 0;

        // 1. Process SET operations (BatchInsert)
        if set_count > 0 {
            // Log all SET entries as a single batch WAL record
            let wal_entries: Vec<(Vec<u8>, Option<Vec<u8>>)> = tx
                .shadow_terms
                .iter()
                .map(|(term, value)| {
                    let value_bytes = value.as_ref().and_then(|v| crate::serialization::bincode_compat::serialize(v).ok());
                    (term.clone(), value_bytes)
                })
                .collect();

            let batch_record = WalRecord::BatchInsert {
                entries: wal_entries,
            };
            self.append_to_wal(batch_record)?;

            // Apply each SET entry without individual WAL logging
            for (term_bytes, value) in tx.shadow_terms.drain(..) {
                let term_str = String::from_utf8_lossy(&term_bytes);
                if let Some(v) = value {
                    self.insert_impl_no_wal_with_value(&term_str, v);
                } else {
                    self.insert_impl_no_wal(&term_str);
                }
            }

            total_operations += set_count;
        }

        // 2. Process INCREMENT operations (BatchIncrement)
        if increment_count > 0 {
            // Aggregate increments for the same term within the transaction
            // This combines multiple increments to the same term into a single delta
            let mut aggregated: std::collections::HashMap<Vec<u8>, i64> =
                std::collections::HashMap::with_capacity(increment_count);
            for (term_bytes, delta) in &tx.increments {
                *aggregated.entry(term_bytes.clone()).or_insert(0) += delta;
            }

            // Convert to WAL entries
            let wal_entries: Vec<(Vec<u8>, i64)> = aggregated
                .iter()
                .map(|(term, delta)| (term.clone(), *delta))
                .collect();

            // Log all INCREMENT entries as a single batch WAL record
            let batch_record = WalRecord::BatchIncrement {
                entries: wal_entries,
            };
            self.append_to_wal(batch_record)?;

            // Apply each aggregated increment without individual WAL logging
            for (term_bytes, delta) in aggregated {
                let term_str = String::from_utf8_lossy(&term_bytes);
                // Use internal increment logic without WAL logging
                self.increment_impl_no_wal(&term_str, delta);
            }

            total_operations += increment_count;
        }

        // Log CommitTx (routes through group commit if enabled)
        self.append_to_wal(WalRecord::CommitTx { tx_id: tx.tx_id })?;
        // Sync WAL to ensure CommitTx is durable (ACID Durability)
        self.sync_wal()?;

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
}
