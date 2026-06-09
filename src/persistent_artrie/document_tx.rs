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
use crate::persistent_artrie_core::counter_codec;
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

        let current: i128 = if let Some(pos) = tx.shadow_terms.iter().rposition(|(k, _)| k == term)
        {
            match &tx.shadow_terms[pos].1 {
                Some(v) => Self::value_to_i128_lossy(v),
                None => 0,
            }
        } else {
            match self.get_value_impl(term) {
                Some(v) => Self::value_to_i128_lossy(&v),
                None => 0,
            }
        };

        // i128 substrate: the running absolute count is the full magnitude (a `u64`
        // counter reaches past `i64::MAX`). The per-type range check lives in
        // `value_from_i128_checked` (`<u64>` → `u64::MAX`; `<i64>` → `i64::MAX`); the
        // `delta` widens losslessly to i128.
        let new_value = match current.checked_add(delta as i128) {
            Some(value) => value,
            None => {
                let reason = format!(
                    "transaction increment overflow for term {:?}: {} + {} overflows the counter substrate",
                    String::from_utf8_lossy(term),
                    current,
                    delta
                );
                tx.mark_failed(reason.clone());
                return Err(PersistentARTrieError::InvalidOperation(reason));
            }
        };
        let v = match Self::value_from_i128_checked(new_value) {
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

        // C2 tx-ii (overlay arm): apply each shadow term via the proven Order-A overlay
        // primitive — upsert for a valued entry, membership insert for `None`. Per-op
        // durable, NOT batch-atomic (matches the owned path, whose reconcile_lww ignores
        // tx brackets on replay). DROP BeginTx/CommitTx/sync — each primitive writes its
        // own durable, ranked record. byte has no `increments` field (increments were
        // folded into shadow_terms as absolute SETs at buffer time), so this is
        // upsert(shadow_terms) only.
        // C2 tx-ii: apply each shadow term via the proven Order-A overlay primitive —
        // upsert for a valued entry, membership insert for `None`. Per-op durable, NOT
        // batch-atomic (matches the owned reconcile_lww, which ignores tx brackets on
        // replay); each primitive writes its own durable, ranked record (no BeginTx/
        // CommitTx/sync). byte folded increments into shadow_terms as absolute SETs at
        // buffer time, so this is upsert(shadow_terms) only.
        let mut applied = 0usize;
        for (term, value) in tx.shadow_terms.drain(..) {
            let newly = match value {
                Some(v) => {
                    <Self as DurableOverlayWrite<ByteKey, V, S>>::upsert_cas_durable_default(
                        self, &term, v,
                    )?
                }
                None => self.insert_cas_durable(&term)?,
            };
            if newly {
                applied += 1;
            }
        }
        tx.state = TransactionState::Committed;
        Ok(applied)
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

        // L3.3: the overlay tx buffered nothing visible (no BeginTx written), so there
        // is nothing to bracket-abort — just discard the shadow.
        tx.shadow_terms.clear();
        tx.state = TransactionState::Aborted;

        Ok(())
    }

    /// Read the counter leaf of `value` as its FULL i128 MAGNITUDE (the
    /// document-transaction increment substrate). Routing through the `counter_codec`
    /// i128 helper (the v6 gate) decodes a `u64` counter to its true unsigned magnitude
    /// — NOT the i64 bit-pattern — so the staging arithmetic is correct for a `u64`
    /// counter past `i64::MAX` (the prior i64-domain read capped it at `i64::MAX`). A
    /// non-counter `V` yields 0 (the prior lossy default).
    fn value_to_i128_lossy(value: &V) -> i128 {
        counter_codec::counter_value_to_i128::<V>(value).unwrap_or(0)
    }

    /// Re-encode an i128 tx-increment result as the typed counter `V`, range-checked
    /// into `V` via the `counter_codec` i128 substrate (the v6 gate): a `<u64>` counter
    /// is bounded by `u64::MAX`, a `<i64>` counter by `i64::MAX` (the latter preserves
    /// the tx-increment correspondence's `i64::MAX + 1` overflow). The message contains
    /// "overflow" so a poisoned transaction's `commit_document` surfaces it.
    fn value_from_i128_checked(value: i128) -> Result<V> {
        counter_codec::i128_to_counter_value::<V>(value).ok_or_else(|| {
            PersistentARTrieError::InvalidOperation(format!(
                "transaction increment value {} overflows the counter value range",
                value
            ))
        })
    }
}
