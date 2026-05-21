//! Document-transaction execution methods for `PersistentARTrie<V, S>`.
//!
//! Split out of byte `dict_impl.rs` (lines ~5698-5938, ~250 LOC) as part
//! of the Phase-5 decomposition. The `DocumentTransaction<V>` /
//! `TransactionState` data carriers themselves live in
//! `super::transactions`; this file holds the trie-side `begin_document`
//! / `tx_insert` / `tx_insert_bytes` / `tx_increment_bytes` /
//! `commit_document` / `abort_document` methods that operate on those
//! transactions.

use std::sync::atomic::Ordering as AtomicOrdering;

use super::block_storage::BlockStorage;
use super::dict_impl::PersistentARTrie;
use super::error::{PersistentARTrieError, Result};
use super::transactions::{DocumentTransaction, TransactionState};
use super::wal::WalRecord;
use crate::persistent_artrie_core::durability::DurabilityPolicy;
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

        if let Some(ref wal) = self.wal_writer {
            wal.append(WalRecord::BeginTx { tx_id }).map_err(|e| {
                PersistentARTrieError::io_error(
                    "begin_tx",
                    "WAL",
                    std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                )
            })?;
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

        let current: i64 = if let Some(pos) = tx.shadow_terms.iter().rposition(|(k, _)| k == term) {
            if let Some(ref v) = tx.shadow_terms[pos].1 {
                let bytes = crate::serialization::bincode_compat::serialize(v).unwrap_or_default();
                if bytes.len() == 8 {
                    i64::from_le_bytes(bytes.try_into().expect("expected 8 bytes"))
                } else {
                    crate::serialization::bincode_compat::deserialize::<i64>(&bytes).unwrap_or(0)
                }
            } else {
                0
            }
        } else {
            match self.get_value_impl(term) {
                Some(v) => {
                    let bytes = crate::serialization::bincode_compat::serialize(&v).unwrap_or_default();
                    if bytes.len() == 8 {
                        i64::from_le_bytes(bytes.try_into().expect("expected 8 bytes"))
                    } else {
                        crate::serialization::bincode_compat::deserialize::<i64>(&bytes).unwrap_or(0)
                    }
                }
                None => 0,
            }
        };

        let new_value = current + delta;
        let value_bytes = crate::serialization::bincode_compat::serialize(&new_value).expect("failed to serialize i64");
        let v: V = crate::serialization::bincode_compat::deserialize(&value_bytes).expect("failed to deserialize i64 as V");
        tx.shadow_terms.push((term.to_vec(), Some(v)));
    }

    /// Commit a document transaction, atomically applying all buffered terms.
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

        let count = tx.shadow_terms.len();

        if count == 0 {
            tx.state = TransactionState::Committed;
            if let Some(ref wal) = self.wal_writer {
                wal.append(WalRecord::CommitTx { tx_id: tx.tx_id })
                    .map_err(|e| {
                        PersistentARTrieError::io_error(
                            "commit_tx",
                            "WAL",
                            std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                        )
                    })?;
                if self.durability_policy == DurabilityPolicy::Immediate {
                    wal.sync().map_err(|e| {
                        PersistentARTrieError::io_error(
                            "commit_tx_sync",
                            "WAL",
                            std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                        )
                    })?;
                }
            }
            return Ok(0);
        }

        let entries: Vec<(String, Option<V>)> = tx
            .shadow_terms
            .drain(..)
            .map(|(term, value)| {
                let term_str = String::from_utf8_lossy(&term).to_string();
                (term_str, value)
            })
            .collect();

        let inserted = self.insert_batch(&entries);

        if let Some(ref wal) = self.wal_writer {
            wal.append(WalRecord::CommitTx { tx_id: tx.tx_id })
                .map_err(|e| {
                    PersistentARTrieError::io_error(
                        "commit_tx",
                        "WAL",
                        std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                    )
                })?;
            if self.durability_policy == DurabilityPolicy::Immediate {
                wal.sync().map_err(|e| {
                    PersistentARTrieError::io_error(
                        "commit_tx_sync",
                        "WAL",
                        std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                    )
                })?;
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

        tx.shadow_terms.clear();
        tx.state = TransactionState::Aborted;

        Ok(())
    }
}
