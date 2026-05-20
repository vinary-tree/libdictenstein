//! Document-transaction data type for the char trie.
//!
//! Split out of char `dict_impl_char.rs` (lines ~124-200) as part of the
//! Phase-6 decomposition, mirroring the byte variant's
//! `persistent_artrie::transactions::DocumentTransaction`. The commit /
//! abort machinery (begin_document, tx_insert, tx_insert_chars,
//! commit_document, abort_document) stays on `PersistentARTrieChar` in
//! `dict_impl_char.rs`; only the data carrier lives here.

use crate::persistent_artrie::TransactionState;
use crate::value::DictionaryValue;

/// A document transaction for per-document atomicity in the character trie.
///
/// This struct buffers all terms for a single document in memory. When the
/// document processing succeeds, `commit_document()` atomically applies all
/// terms to the trie with a single batch WAL write. If processing fails,
/// `abort_document()` discards the buffer without polluting the trie or WAL.
///
/// # Character vs Byte Handling
///
/// This transaction stores terms as both string bytes (for WAL serialization)
/// and allows direct `char` slice insertion. Internally, characters are stored
/// as UTF-8 bytes for WAL compatibility with the 1-byte trie format.
///
/// # Example
///
/// ```rust,ignore
/// use libdictenstein::persistent_artrie_char::{PersistentARTrieChar, CharDocumentTransaction};
///
/// let mut trie = PersistentARTrieChar::<u64>::create("unicode_docs.trie")?;
///
/// // Start a transaction for a document
/// let mut tx = trie.begin_document("doc_001")?;
///
/// // Buffer terms (not yet committed)
/// trie.tx_insert(&mut tx, "日本語", Some(1));
/// trie.tx_insert(&mut tx, "中文", Some(2));
/// trie.tx_insert_chars(&mut tx, &['한', '글'], Some(3));
///
/// // Commit all terms atomically
/// let count = trie.commit_document(tx)?;
/// assert_eq!(count, 3);
/// ```
#[derive(Debug)]
pub struct CharDocumentTransaction<V: DictionaryValue> {
    /// Unique transaction identifier
    pub tx_id: u64,
    /// Document identifier (for debugging/logging)
    pub document_id: String,
    /// Buffered terms to be applied on commit (term as bytes, optional value)
    pub(crate) shadow_terms: Vec<(Vec<u8>, Option<V>)>,
    /// Buffered increment operations (term as bytes, delta)
    pub(crate) increments: Vec<(Vec<u8>, i64)>,
    /// Current state of the transaction
    pub state: TransactionState,
}

impl<V: DictionaryValue> CharDocumentTransaction<V> {
    /// Returns the number of buffered operations in this transaction.
    pub fn len(&self) -> usize {
        self.shadow_terms.len() + self.increments.len()
    }

    /// Returns true if no operations have been buffered.
    pub fn is_empty(&self) -> bool {
        self.shadow_terms.is_empty() && self.increments.is_empty()
    }

    /// Returns the number of buffered SET operations.
    pub fn set_count(&self) -> usize {
        self.shadow_terms.len()
    }

    /// Returns the number of buffered INCREMENT operations.
    pub fn increment_count(&self) -> usize {
        self.increments.len()
    }

    /// Returns the document ID associated with this transaction.
    pub fn document_id(&self) -> &str {
        &self.document_id
    }

    /// Returns true if the transaction is still active.
    pub fn is_active(&self) -> bool {
        self.state == TransactionState::Active
    }
}
