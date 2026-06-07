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
use super::wal::WalRecord;
use crate::persistent_artrie_core::key_encoding::ByteKey;
use crate::persistent_artrie_core::overlay::durable_write::DurableOverlayWrite;
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
            base ^ (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0))
        };

        // C2 tx-ii: under the overlay, SKIP the orphan BeginTx WAL append — it would
        // burn an un-`mark_committed` LSN that stalls the committed watermark, and the
        // overlay `commit_document` is per-op durable (NOT bracketed). The owned arm
        // keeps BeginTx (reconcile_lww ignores the bracket on replay regardless).
        if !self.route_overlay() {
            if let Some(ref wal) = self.wal_writer {
                wal.append(WalRecord::BeginTx { tx_id }).map_err(|e| {
                    PersistentARTrieError::io_error(
                        "begin_tx",
                        "WAL",
                        std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                    )
                })?;
            }
        }

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

        let current: i64 = if let Some(pos) = tx.shadow_terms.iter().rposition(|(k, _)| k == term) {
            if let Some(ref v) = tx.shadow_terms[pos].1 {
                Self::i64_from_value_lossy(v)
            } else {
                0
            }
        } else {
            match self.get_value_impl(term) {
                Some(v) => Self::i64_from_value_lossy(&v),
                None => 0,
            }
        };

        let new_value = match current.checked_add(delta) {
            Some(value) => value,
            None => {
                let reason = format!(
                    "transaction increment overflow for term {:?}: {} + {} exceeds i64 range",
                    String::from_utf8_lossy(term),
                    current,
                    delta
                );
                tx.mark_failed(reason.clone());
                return Err(PersistentARTrieError::InvalidOperation(reason));
            }
        };
        let v = match Self::value_from_i64_checked(new_value) {
            Ok(value) => value,
            Err(error) => {
                tx.mark_failed(error.to_string());
                return Err(error);
            }
        };
        tx.shadow_terms.push((term.to_vec(), Some(v)));
        Ok(())
    }

    /// Commit a document transaction, atomically applying all buffered terms.
    ///
    /// **M3 reject (BROKEN-BY-DESIGN, audit §B #8).** Applies the buffered terms via
    /// `insert_impl_core` (owned absolute write). Reject under `route_overlay()` (the
    /// second entry-point guard; `begin_document` already rejects, but a transaction
    /// could have been opened on the owned path then flipped — fail loud here too).
    pub fn commit_document(&mut self, mut tx: DocumentTransaction<V>) -> Result<usize>
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

        // C2 tx-ii (overlay arm): apply each shadow term via the proven Order-A overlay
        // primitive — upsert for a valued entry, membership insert for `None`. Per-op
        // durable, NOT batch-atomic (matches the owned path, whose reconcile_lww ignores
        // tx brackets on replay). DROP BeginTx/CommitTx/sync — each primitive writes its
        // own durable, ranked record. byte has no `increments` field (increments were
        // folded into shadow_terms as absolute SETs at buffer time), so this is
        // upsert(shadow_terms) only.
        if self.route_overlay() {
            let mut applied = 0usize;
            for (term, value) in tx.shadow_terms.drain(..) {
                let newly = match value {
                    Some(v) => <Self as DurableOverlayWrite<ByteKey, V, S>>::upsert_cas_durable_default(
                        self, &term, v,
                    )?,
                    None => self.insert_cas_durable(&term)?,
                };
                if newly {
                    applied += 1;
                }
            }
            tx.state = TransactionState::Committed;
            return Ok(applied);
        }

        let count = tx.shadow_terms.len();

        if count == 0 {
            if let Some(ref wal) = self.wal_writer {
                let commit_lsn = wal
                    .append(WalRecord::CommitTx { tx_id: tx.tx_id })
                    .map_err(|e| {
                        PersistentARTrieError::io_error(
                            "commit_tx",
                            "WAL",
                            std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                        )
                    })?;
                self.sync_wal_after_append(commit_lsn, "commit_tx_sync")?;
            }
            tx.state = TransactionState::Committed;
            return Ok(0);
        }

        let mut wal_entries = Vec::with_capacity(count);
        for (term, value) in &tx.shadow_terms {
            let value_bytes = match value.as_ref() {
                Some(v) => Some(crate::serialization::bincode_compat::serialize(v).map_err(
                    |e| PersistentARTrieError::internal(format!("Serialization error: {}", e)),
                )?),
                None => None,
            };
            wal_entries.push((term.clone(), value_bytes));
        }

        if let Some(ref wal) = self.wal_writer {
            wal.append_batch(&wal_entries).map_err(|e| {
                PersistentARTrieError::io_error(
                    "commit_tx_batch",
                    "WAL",
                    std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                )
            })?;
            let commit_lsn = wal
                .append(WalRecord::CommitTx { tx_id: tx.tx_id })
                .map_err(|e| {
                    PersistentARTrieError::io_error(
                        "commit_tx",
                        "WAL",
                        std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                    )
                })?;
            self.sync_wal_after_append(commit_lsn, "commit_tx_sync")?;
        }

        let mut inserted = 0;
        for (term, value) in tx.shadow_terms.drain(..) {
            if self.insert_impl_core(&term, value) {
                inserted += 1;
            }
        }

        tx.state = TransactionState::Committed;
        Ok(inserted)
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

        // C2 tx-ii: under the overlay, skip the AbortTx WAL append — no BeginTx was
        // written and the overlay tx buffered nothing visible, so there is nothing to
        // bracket-abort. Owned arm keeps AbortTx.
        if !self.route_overlay() {
            if let Some(ref wal) = self.wal_writer {
                wal.append(WalRecord::AbortTx { tx_id: tx.tx_id })
                    .map_err(|e| {
                        PersistentARTrieError::io_error(
                            "abort_tx",
                            "WAL",
                            std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                        )
                    })?;
            }
        }

        tx.shadow_terms.clear();
        tx.state = TransactionState::Aborted;

        Ok(())
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
