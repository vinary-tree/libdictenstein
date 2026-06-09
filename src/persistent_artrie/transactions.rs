//! Document-transaction data types.
//!
//! Split out of byte `dict_impl.rs` (lines ~369-450) as part of the Phase-5
//! decomposition. The actual commit/abort machinery — `begin_document`,
//! `tx_insert`, `commit_document`, `abort_document` — still lives on
//! `PersistentARTrie<V>` in `dict_impl.rs`; only the data carriers
//! (`DocumentTransaction<V>` + `TransactionState`) live here so the
//! transaction type's invariants can be navigated independently.

use crate::value::DictionaryValue;

/// State of a document transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionState {
    /// Transaction is active and accepting operations
    Active,
    /// Transaction has been committed
    Committed,
    /// Transaction has been aborted
    Aborted,
}

/// A document transaction for per-document atomicity.
///
/// This struct buffers all terms for a single document in memory. When the
/// document processing succeeds, `commit_document()` atomically applies all
/// terms to the trie with a single batch WAL write. If processing fails,
/// `abort_document()` discards the buffer without polluting the trie or WAL.
///
/// # Example
///
/// ```text
/// use libdictenstein::persistent_artrie::PersistentARTrie;
///
/// let trie: PersistentARTrie<i64> = PersistentARTrie::create("my.artrie")?;
///
/// // Begin transaction for a document
/// let mut tx = trie.begin_document("document_123")?;
///
/// // Buffer terms (not yet in trie)
/// trie.tx_insert(&mut tx, "term1", Some(1));
/// trie.tx_insert(&mut tx, "term2", Some(2));
///
/// // On success: atomically apply all terms
/// let count = trie.commit_document(tx)?;
///
/// // On failure: discard all buffered terms
/// // trie.abort_document(tx)?;
/// ```
pub struct DocumentTransaction<V: DictionaryValue> {
    /// Unique transaction identifier
    pub tx_id: u64,
    /// Document identifier (for debugging/logging)
    pub document_id: String,
    /// Buffered terms to be applied on commit
    pub(crate) shadow_terms: Vec<(Vec<u8>, Option<V>)>,
    /// Buffered counter increments `(term, raw delta)`. Applied on commit via the
    /// add-only overlay counter (ACCUMULATE — owner decision 2026-06-09), NOT folded
    /// into `shadow_terms` as absolute SETs (which silently overwrote the live count).
    pub(crate) increments: Vec<(Vec<u8>, i64)>,
    /// Deferred failure for compatibility methods that cannot return `Result`.
    pub(crate) failure: Option<String>,
    /// Current state of the transaction
    pub state: TransactionState,
}

impl<V: DictionaryValue> DocumentTransaction<V> {
    /// Construct a new Active-state transaction. Used by
    /// `PersistentARTrie::begin_document` in the sibling
    /// `document_tx` module (which cannot otherwise build a value
    /// since `shadow_terms` is `pub(crate)`).
    pub(crate) fn new_active(tx_id: u64, document_id: String) -> Self {
        Self {
            tx_id,
            document_id,
            shadow_terms: Vec::new(),
            increments: Vec::new(),
            failure: None,
            state: TransactionState::Active,
        }
    }

    /// Returns the number of buffered terms in this transaction.
    pub fn len(&self) -> usize {
        self.shadow_terms.len()
    }

    /// Returns true if no terms have been buffered.
    pub fn is_empty(&self) -> bool {
        self.shadow_terms.is_empty()
    }

    /// Returns the document ID associated with this transaction.
    pub fn document_id(&self) -> &str {
        &self.document_id
    }

    /// Returns true if the transaction is still active.
    pub fn is_active(&self) -> bool {
        self.state == TransactionState::Active
    }

    pub(crate) fn mark_failed(&mut self, reason: impl Into<String>) {
        if self.failure.is_none() {
            self.failure = Some(reason.into());
        }
    }

    pub(crate) fn failure_reason(&self) -> Option<&str> {
        self.failure.as_deref()
    }
}
