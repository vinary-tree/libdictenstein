//! IndexedVocabulary: Optimized bijective map for sequential u64 indices.
//!
//! This module provides [`IndexedVocabulary`], a high-performance bijective dictionary
//! specialized for mapping terms to auto-incremented `u64` indices. It achieves:
//!
//! - **O(k) forward lookup** via pluggable trie backend (where k = term length)
//! - **O(1) reverse lookup** via Vec indexing
//! - **Low memory overhead**: ~1x trie size + n×avg_term_length bytes
//!
//! # Use Cases
//!
//! - Embedding vocabularies (word2vec, BERT tokenizers)
//! - Token-to-ID mappings for neural networks
//! - Symbol tables with numeric identifiers
//! - Any vocabulary where terms need sequential numeric IDs

use crate::bijective::BijectiveDictionary;
use crate::double_array_trie_char::DoubleArrayTrieChar;
use crate::dynamic_dawg_char::DynamicDawgChar;
use crate::sync_compat::RwLock;
use crate::{Dictionary, MappedDictionary, MutableMappedDictionary};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[cfg(feature = "persistent-artrie")]
use crate::persistent_artrie_char::PersistentARTrieChar;

/// A bijective vocabulary mapping terms to sequential `u64` indices.
///
/// `IndexedVocabulary` provides efficient bidirectional lookup between terms (strings)
/// and auto-incremented `u64` indices. It's generic over the underlying trie backend,
/// allowing you to choose the best trade-off between performance and features.
///
/// # Type Parameter
///
/// - `D`: The dictionary backend implementing [`MappedDictionary<Value = u64>`] and
///   [`MutableMappedDictionary<Value = u64>`]. Common choices:
///   - [`DoubleArrayTrieChar<u64>`]: Best read performance, insert-only (default)
///   - [`DynamicDawgChar<u64>`]: Good performance with suffix sharing
///   - [`PersistentARTrieChar<u64>`]: ACID-compliant with disk persistence
///
/// # Thread Safety
///
/// `IndexedVocabulary` is fully thread-safe:
/// - Index assignment uses [`AtomicU64`] for lock-free increments
/// - Reverse lookup uses [`Arc<RwLock<Vec<String>>>`] for concurrent access
/// - Forward operations delegate to the thread-safe backend
///
/// # Examples
///
/// ```rust
/// use libdictenstein::bijective::IndexedVocabulary;
///
/// // Build vocabulary from terms
/// let vocab = IndexedVocabulary::from_terms(["hello", "world", "rust"]);
///
/// // Forward lookup: term → index
/// assert_eq!(vocab.get_index("hello"), Some(0));
/// assert_eq!(vocab.get_index("world"), Some(1));
/// assert_eq!(vocab.get_index("rust"), Some(2));
///
/// // Reverse lookup: index → term
/// assert_eq!(vocab.get_term(0), Some("hello"));
/// assert_eq!(vocab.get_term(1), Some("world"));
/// assert_eq!(vocab.get_term(2), Some("rust"));
///
/// // Dynamic insertion
/// let vocab = IndexedVocabulary::new();
/// let idx1 = vocab.insert("new_term");  // Returns 0
/// let idx2 = vocab.insert("another");   // Returns 1
/// ```
#[derive(Debug)]
pub struct IndexedVocabulary<D = DynamicDawgChar<u64>>
where
    D: MappedDictionary<Value = u64> + MutableMappedDictionary<Value = u64>,
{
    /// Forward mapping: term → index (pluggable backend)
    forward: D,

    /// Reverse mapping: index → term
    /// Indexed by (index - start_index), so reverse[0] holds the term for start_index
    reverse: Arc<RwLock<Vec<String>>>,

    /// Next index to assign (atomically incremented)
    next_index: AtomicU64,

    /// Starting index value (configurable, default: 0)
    start_index: u64,
}

/// Type alias for IndexedVocabulary with DoubleArrayTrieChar backend.
///
/// Use this when you need maximum read performance and your vocabulary
/// is built once then used for many lookups.
pub type IndexedVocabularyDAT = IndexedVocabulary<DoubleArrayTrieChar<u64>>;

/// Type alias for IndexedVocabulary with DynamicDawgChar backend.
///
/// Use this when you need dynamic insertions with good performance
/// and want suffix sharing to reduce memory usage.
pub type IndexedVocabularyDAWG = IndexedVocabulary<DynamicDawgChar<u64>>;

/// Type alias for IndexedVocabulary with PersistentARTrieChar backend.
///
/// Use this when you need ACID compliance, disk persistence, and crash recovery.
/// Available only with the `persistent-artrie` feature.
#[cfg(feature = "persistent-artrie")]
pub type IndexedVocabularyART = IndexedVocabulary<PersistentARTrieChar<u64>>;

impl<D> Clone for IndexedVocabulary<D>
where
    D: MappedDictionary<Value = u64> + MutableMappedDictionary<Value = u64> + Clone,
{
    fn clone(&self) -> Self {
        Self {
            forward: self.forward.clone(),
            reverse: Arc::clone(&self.reverse),
            next_index: AtomicU64::new(self.next_index.load(Ordering::SeqCst)),
            start_index: self.start_index,
        }
    }
}

impl IndexedVocabulary<DynamicDawgChar<u64>> {
    /// Create an empty vocabulary with the default backend (DynamicDawgChar).
    ///
    /// Indices start at 0.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::bijective::IndexedVocabulary;
    ///
    /// let vocab = IndexedVocabulary::new();
    /// vocab.insert("hello");  // Gets index 0
    /// vocab.insert("world");  // Gets index 1
    /// ```
    pub fn new() -> Self {
        Self::with_start_index(0)
    }

    /// Create an empty vocabulary with indices starting at the specified value.
    ///
    /// This is useful when you need to reserve lower indices for special tokens
    /// (e.g., 0 for padding, 1 for unknown).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::bijective::IndexedVocabulary;
    ///
    /// // Reserve indices 0 and 1 for special tokens
    /// let vocab = IndexedVocabulary::with_start_index(2);
    /// let idx = vocab.insert("hello");  // Gets index 2
    /// assert_eq!(vocab.start_index(), 2);
    /// ```
    pub fn with_start_index(start_index: u64) -> Self {
        Self {
            forward: DynamicDawgChar::new(),
            reverse: Arc::new(RwLock::new(Vec::new())),
            next_index: AtomicU64::new(start_index),
            start_index,
        }
    }

    /// Build a vocabulary from an iterator of terms.
    ///
    /// Terms are assigned indices starting from 0 in iteration order.
    /// Duplicate terms are silently ignored (only the first occurrence gets an index).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::bijective::IndexedVocabulary;
    ///
    /// let vocab = IndexedVocabulary::from_terms(["apple", "banana", "cherry"]);
    ///
    /// assert_eq!(vocab.len(), 3);
    /// assert_eq!(vocab.get_index("apple"), Some(0));
    /// assert_eq!(vocab.get_index("banana"), Some(1));
    /// assert_eq!(vocab.get_index("cherry"), Some(2));
    /// ```
    pub fn from_terms<I, S>(terms: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self::from_terms_with_start(terms, 0)
    }

    /// Build a vocabulary from an iterator of terms with a custom start index.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::bijective::IndexedVocabulary;
    ///
    /// // Start indices at 1 (reserve 0 for unknown/padding)
    /// let vocab = IndexedVocabulary::from_terms_with_start(["apple", "banana"], 1);
    ///
    /// assert_eq!(vocab.get_index("apple"), Some(1));
    /// assert_eq!(vocab.get_index("banana"), Some(2));
    /// assert_eq!(vocab.get_term(1), Some("apple"));
    /// ```
    pub fn from_terms_with_start<I, S>(terms: I, start_index: u64) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let vocab = Self::with_start_index(start_index);
        for term in terms {
            // Use get_or_insert to handle duplicates gracefully
            vocab.get_or_insert(term.as_ref());
        }
        vocab
    }
}

impl Default for IndexedVocabulary<DynamicDawgChar<u64>> {
    fn default() -> Self {
        Self::new()
    }
}

impl<D> IndexedVocabulary<D>
where
    D: MappedDictionary<Value = u64> + MutableMappedDictionary<Value = u64> + Default,
{
    /// Create an empty vocabulary with a specific backend type.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::bijective::IndexedVocabulary;
    /// use libdictenstein::dynamic_dawg_char::DynamicDawgChar;
    ///
    /// // Using DynamicDawgChar backend (implements MutableMappedDictionary)
    /// let vocab: IndexedVocabulary<DynamicDawgChar<u64>> =
    ///     IndexedVocabulary::with_backend_default();
    /// ```
    pub fn with_backend_default() -> Self {
        Self::with_backend_and_start(D::default(), 0)
    }
}

impl<D> IndexedVocabulary<D>
where
    D: MappedDictionary<Value = u64> + MutableMappedDictionary<Value = u64>,
{
    /// Create an empty vocabulary with a specific backend instance.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::bijective::IndexedVocabulary;
    /// use libdictenstein::dynamic_dawg_char::DynamicDawgChar;
    ///
    /// let dawg: DynamicDawgChar<u64> = DynamicDawgChar::new();
    /// let vocab = IndexedVocabulary::with_backend(dawg);
    /// ```
    pub fn with_backend(backend: D) -> Self {
        Self::with_backend_and_start(backend, 0)
    }

    /// Create an empty vocabulary with a specific backend and start index.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::bijective::IndexedVocabulary;
    /// use libdictenstein::dynamic_dawg_char::DynamicDawgChar;
    ///
    /// let dawg: DynamicDawgChar<u64> = DynamicDawgChar::new();
    /// let vocab = IndexedVocabulary::with_backend_and_start(dawg, 100);
    /// assert_eq!(vocab.start_index(), 100);
    /// ```
    pub fn with_backend_and_start(backend: D, start_index: u64) -> Self {
        Self {
            forward: backend,
            reverse: Arc::new(RwLock::new(Vec::new())),
            next_index: AtomicU64::new(start_index),
            start_index,
        }
    }

    /// Insert a term and auto-assign the next available index.
    ///
    /// Returns the assigned index.
    ///
    /// # Panics
    ///
    /// Panics if the term already exists in the vocabulary. This preserves the
    /// bijection invariant. Use [`get_or_insert`](Self::get_or_insert) for
    /// idempotent insertion.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::bijective::IndexedVocabulary;
    ///
    /// let vocab = IndexedVocabulary::new();
    /// let idx1 = vocab.insert("hello");  // Returns 0
    /// let idx2 = vocab.insert("world");  // Returns 1
    ///
    /// assert_eq!(idx1, 0);
    /// assert_eq!(idx2, 1);
    /// ```
    ///
    /// ```rust,should_panic
    /// use libdictenstein::bijective::IndexedVocabulary;
    ///
    /// let vocab = IndexedVocabulary::new();
    /// vocab.insert("hello");
    /// vocab.insert("hello");  // Panics: duplicate term
    /// ```
    pub fn insert(&self, term: &str) -> u64 {
        // Check if term already exists
        if self.forward.get_value(term).is_some() {
            panic!(
                "BijectiveDictionary::insert: duplicate term '{}' violates bijection invariant",
                term
            );
        }

        // Atomically claim the next index
        let index = self.next_index.fetch_add(1, Ordering::SeqCst);

        // Insert into forward mapping
        self.forward.insert_with_value(term, index);

        // Insert into reverse mapping
        {
            let mut reverse = self.reverse.write();
            // Ensure Vec has capacity for this index
            let vec_index = (index - self.start_index) as usize;
            if vec_index >= reverse.len() {
                reverse.resize(vec_index + 1, String::new());
            }
            reverse[vec_index] = term.to_string();
        }

        index
    }

    /// Get the index for a term, inserting it if it doesn't exist.
    ///
    /// This is an idempotent operation: calling it multiple times with the same
    /// term always returns the same index.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::bijective::IndexedVocabulary;
    ///
    /// let vocab = IndexedVocabulary::new();
    ///
    /// let idx1 = vocab.get_or_insert("hello");  // Inserts, returns 0
    /// let idx2 = vocab.get_or_insert("hello");  // Already exists, returns 0
    /// let idx3 = vocab.get_or_insert("world");  // Inserts, returns 1
    ///
    /// assert_eq!(idx1, idx2);  // Same index
    /// assert_eq!(idx3, 1);
    /// ```
    pub fn get_or_insert(&self, term: &str) -> u64 {
        // Fast path: check if term exists
        if let Some(index) = self.forward.get_value(term) {
            return index;
        }

        // Slow path: insert new term
        // Note: There's a potential race condition here between check and insert.
        // The backend's insert_with_value handles this atomically.

        // Atomically claim the next index
        let index = self.next_index.fetch_add(1, Ordering::SeqCst);

        // Try to insert into forward mapping
        let is_new = self.forward.insert_with_value(term, index);

        if is_new {
            // We won the race, insert into reverse mapping
            let mut reverse = self.reverse.write();
            let vec_index = (index - self.start_index) as usize;
            if vec_index >= reverse.len() {
                reverse.resize(vec_index + 1, String::new());
            }
            reverse[vec_index] = term.to_string();
            index
        } else {
            // Another thread inserted first, return existing index
            // Note: We wasted an index, but this is rare and acceptable
            self.forward.get_value(term).expect(
                "Term should exist after failed insert_with_value",
            )
        }
    }

    /// Get the index for a term.
    ///
    /// Returns `None` if the term is not in the vocabulary.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::bijective::IndexedVocabulary;
    ///
    /// let vocab = IndexedVocabulary::from_terms(["hello", "world"]);
    ///
    /// assert_eq!(vocab.get_index("hello"), Some(0));
    /// assert_eq!(vocab.get_index("world"), Some(1));
    /// assert_eq!(vocab.get_index("missing"), None);
    /// ```
    #[inline]
    pub fn get_index(&self, term: &str) -> Option<u64> {
        self.forward.get_value(term)
    }

    /// Get the term for an index.
    ///
    /// Returns `None` if:
    /// - The index is less than `start_index`
    /// - The index is greater than or equal to `start_index + len()`
    ///
    /// # Performance
    ///
    /// This is an O(1) operation via direct Vec indexing.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::bijective::IndexedVocabulary;
    ///
    /// let vocab = IndexedVocabulary::from_terms(["hello", "world"]);
    ///
    /// assert_eq!(vocab.get_term(0), Some("hello"));
    /// assert_eq!(vocab.get_term(1), Some("world"));
    /// assert_eq!(vocab.get_term(999), None);
    /// ```
    #[inline]
    pub fn get_term(&self, index: u64) -> Option<&str> {
        if index < self.start_index {
            return None;
        }

        let vec_index = (index - self.start_index) as usize;
        let reverse = self.reverse.read();

        if vec_index < reverse.len() {
            // SAFETY: We're returning a reference to data inside the RwLock.
            // This is safe because:
            // 1. The Vec only grows (never shrinks or reallocates existing elements)
            // 2. Strings in the Vec are never modified after insertion
            // 3. The Arc keeps the data alive
            //
            // We use a raw pointer to avoid lifetime issues with the RwLock guard.
            let term_ptr = reverse[vec_index].as_str() as *const str;
            Some(unsafe { &*term_ptr })
        } else {
            None
        }
    }

    /// Get the starting index for this vocabulary.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::bijective::IndexedVocabulary;
    ///
    /// let vocab = IndexedVocabulary::with_start_index(100);
    /// assert_eq!(vocab.start_index(), 100);
    /// ```
    #[inline]
    pub fn start_index(&self) -> u64 {
        self.start_index
    }

    /// Get the number of terms in the vocabulary.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::bijective::IndexedVocabulary;
    ///
    /// let vocab = IndexedVocabulary::from_terms(["a", "b", "c"]);
    /// assert_eq!(vocab.len(), 3);
    /// ```
    #[inline]
    pub fn len(&self) -> usize {
        self.reverse.read().len()
    }

    /// Check if the vocabulary is empty.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::bijective::IndexedVocabulary;
    ///
    /// let vocab = IndexedVocabulary::new();
    /// assert!(vocab.is_empty());
    ///
    /// vocab.insert("hello");
    /// assert!(!vocab.is_empty());
    /// ```
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.reverse.read().is_empty()
    }

    /// Check if a term exists in the vocabulary.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::bijective::IndexedVocabulary;
    ///
    /// let vocab = IndexedVocabulary::from_terms(["hello"]);
    /// assert!(vocab.contains_term("hello"));
    /// assert!(!vocab.contains_term("world"));
    /// ```
    #[inline]
    pub fn contains_term(&self, term: &str) -> bool {
        self.forward.get_value(term).is_some()
    }

    /// Check if an index exists in the vocabulary.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::bijective::IndexedVocabulary;
    ///
    /// let vocab = IndexedVocabulary::from_terms(["hello"]);
    /// assert!(vocab.contains_index(0));
    /// assert!(!vocab.contains_index(1));
    /// ```
    #[inline]
    pub fn contains_index(&self, index: u64) -> bool {
        if index < self.start_index {
            return false;
        }
        let vec_index = (index - self.start_index) as usize;
        vec_index < self.reverse.read().len()
    }

    /// Iterate over all (term, index) pairs.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::bijective::IndexedVocabulary;
    ///
    /// let vocab = IndexedVocabulary::from_terms(["a", "b", "c"]);
    ///
    /// for (term, index) in vocab.iter() {
    ///     println!("{} -> {}", term, index);
    /// }
    /// ```
    pub fn iter(&self) -> impl Iterator<Item = (String, u64)> + '_ {
        let reverse = self.reverse.read();
        let start = self.start_index;

        // Clone the data to avoid lifetime issues with the lock guard
        reverse
            .iter()
            .enumerate()
            .map(move |(i, term)| (term.clone(), start + i as u64))
            .collect::<Vec<_>>()
            .into_iter()
    }

    /// Get a reference to the underlying forward dictionary.
    ///
    /// This is useful for advanced operations like fuzzy matching with
    /// Levenshtein automata.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::bijective::IndexedVocabulary;
    /// use libdictenstein::Dictionary;
    ///
    /// let vocab = IndexedVocabulary::from_terms(["hello", "world"]);
    /// let forward = vocab.forward();
    ///
    /// // Use the forward dictionary for prefix traversal, etc.
    /// let root = forward.root();
    /// ```
    #[inline]
    pub fn forward(&self) -> &D {
        &self.forward
    }
}

// =============================================================================
// Dictionary trait implementation
// =============================================================================

impl<D> Dictionary for IndexedVocabulary<D>
where
    D: MappedDictionary<Value = u64> + MutableMappedDictionary<Value = u64> + Dictionary,
{
    type Node = D::Node;

    fn root(&self) -> Self::Node {
        self.forward.root()
    }

    fn contains(&self, term: &str) -> bool {
        self.forward.contains(term)
    }

    fn len(&self) -> Option<usize> {
        Some(self.reverse.read().len())
    }
}

// =============================================================================
// MappedDictionary trait implementation
// =============================================================================

impl<D> MappedDictionary for IndexedVocabulary<D>
where
    D: MappedDictionary<Value = u64> + MutableMappedDictionary<Value = u64>,
{
    type Value = u64;

    fn get_value(&self, term: &str) -> Option<Self::Value> {
        self.forward.get_value(term)
    }

    fn contains_with_value<F>(&self, term: &str, predicate: F) -> bool
    where
        F: Fn(&Self::Value) -> bool,
    {
        self.forward.get_value(term).is_some_and(|v| predicate(&v))
    }
}

// =============================================================================
// BijectiveDictionary trait implementation
// =============================================================================

impl<D> BijectiveDictionary for IndexedVocabulary<D>
where
    D: MappedDictionary<Value = u64> + MutableMappedDictionary<Value = u64>,
{
    fn get_term(&self, value: &Self::Value) -> Option<&str> {
        Self::get_term(self, *value)
    }

    fn contains_value(&self, value: &Self::Value) -> bool {
        self.contains_index(*value)
    }

    fn bijection_len(&self) -> usize {
        self.len()
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_vocabulary() {
        let vocab = IndexedVocabulary::new();
        assert!(vocab.is_empty());
        assert_eq!(vocab.len(), 0);
        assert_eq!(vocab.get_index("test"), None);
        assert_eq!(vocab.get_term(0), None);
    }

    #[test]
    fn test_single_term() {
        let vocab = IndexedVocabulary::new();
        let idx = vocab.insert("hello");

        assert_eq!(idx, 0);
        assert_eq!(vocab.len(), 1);
        assert_eq!(vocab.get_index("hello"), Some(0));
        assert_eq!(vocab.get_term(0), Some("hello"));
    }

    #[test]
    fn test_multiple_terms() {
        let vocab = IndexedVocabulary::from_terms(["apple", "banana", "cherry"]);

        assert_eq!(vocab.len(), 3);
        assert_eq!(vocab.get_index("apple"), Some(0));
        assert_eq!(vocab.get_index("banana"), Some(1));
        assert_eq!(vocab.get_index("cherry"), Some(2));
        assert_eq!(vocab.get_term(0), Some("apple"));
        assert_eq!(vocab.get_term(1), Some("banana"));
        assert_eq!(vocab.get_term(2), Some("cherry"));
    }

    #[test]
    fn test_custom_start_index() {
        let vocab = IndexedVocabulary::from_terms_with_start(["apple", "banana"], 10);

        assert_eq!(vocab.start_index(), 10);
        assert_eq!(vocab.get_index("apple"), Some(10));
        assert_eq!(vocab.get_index("banana"), Some(11));
        assert_eq!(vocab.get_term(10), Some("apple"));
        assert_eq!(vocab.get_term(11), Some("banana"));
        assert_eq!(vocab.get_term(9), None); // Below start_index
        assert_eq!(vocab.get_term(12), None); // Above range
    }

    #[test]
    fn test_get_or_insert() {
        let vocab = IndexedVocabulary::new();

        let idx1 = vocab.get_or_insert("hello");
        let idx2 = vocab.get_or_insert("hello"); // Same term
        let idx3 = vocab.get_or_insert("world"); // New term

        assert_eq!(idx1, 0);
        assert_eq!(idx2, 0); // Same index for same term
        assert_eq!(idx3, 1);
        assert_eq!(vocab.len(), 2);
    }

    #[test]
    #[should_panic(expected = "duplicate term")]
    fn test_insert_duplicate_panics() {
        let vocab = IndexedVocabulary::new();
        vocab.insert("hello");
        vocab.insert("hello"); // Should panic
    }

    #[test]
    fn test_contains() {
        let vocab = IndexedVocabulary::from_terms(["hello"]);

        assert!(vocab.contains_term("hello"));
        assert!(!vocab.contains_term("world"));
        assert!(vocab.contains_index(0));
        assert!(!vocab.contains_index(1));
    }

    #[test]
    fn test_unicode_terms() {
        let vocab = IndexedVocabulary::from_terms(["café", "日本語", "🎉", "naïve"]);

        assert_eq!(vocab.get_index("café"), Some(0));
        assert_eq!(vocab.get_index("日本語"), Some(1));
        assert_eq!(vocab.get_index("🎉"), Some(2));
        assert_eq!(vocab.get_index("naïve"), Some(3));

        assert_eq!(vocab.get_term(0), Some("café"));
        assert_eq!(vocab.get_term(1), Some("日本語"));
        assert_eq!(vocab.get_term(2), Some("🎉"));
        assert_eq!(vocab.get_term(3), Some("naïve"));
    }

    #[test]
    fn test_empty_string() {
        let vocab = IndexedVocabulary::from_terms(["", "a", "ab"]);

        assert_eq!(vocab.get_index(""), Some(0));
        assert_eq!(vocab.get_term(0), Some(""));
    }

    #[test]
    fn test_bijection_invariant() {
        let vocab = IndexedVocabulary::from_terms(["alpha", "beta", "gamma"]);

        // For every (term, index) pair, the bijection should hold
        for (term, idx) in vocab.iter() {
            assert_eq!(vocab.get_index(&term), Some(idx));
            assert_eq!(vocab.get_term(idx), Some(term.as_str()));
        }
    }

    #[test]
    fn test_from_terms_deduplicates() {
        let vocab = IndexedVocabulary::from_terms(["a", "b", "a", "c", "b"]);

        // Duplicates should be ignored
        assert_eq!(vocab.len(), 3);
        assert_eq!(vocab.get_index("a"), Some(0));
        assert_eq!(vocab.get_index("b"), Some(1));
        assert_eq!(vocab.get_index("c"), Some(2));
    }

    #[test]
    fn test_iter() {
        let vocab = IndexedVocabulary::from_terms(["x", "y", "z"]);

        let pairs: Vec<_> = vocab.iter().collect();

        assert_eq!(pairs.len(), 3);
        // Note: iteration order matches insertion order (Vec order)
        assert!(pairs.contains(&("x".to_string(), 0)));
        assert!(pairs.contains(&("y".to_string(), 1)));
        assert!(pairs.contains(&("z".to_string(), 2)));
    }

    #[test]
    fn test_mapped_dictionary_trait() {
        use crate::MappedDictionary;

        let vocab = IndexedVocabulary::from_terms(["test"]);

        // Test via trait
        assert_eq!(MappedDictionary::get_value(&vocab, "test"), Some(0));
        assert_eq!(MappedDictionary::get_value(&vocab, "missing"), None);
    }

    #[test]
    fn test_bijective_dictionary_trait() {
        use crate::bijective::BijectiveDictionary;

        let vocab = IndexedVocabulary::from_terms(["test"]);

        // Test via trait
        assert_eq!(BijectiveDictionary::get_term(&vocab, &0), Some("test"));
        assert_eq!(BijectiveDictionary::get_term(&vocab, &99), None);
        assert!(BijectiveDictionary::contains_value(&vocab, &0));
        assert!(!BijectiveDictionary::contains_value(&vocab, &99));
        assert_eq!(BijectiveDictionary::bijection_len(&vocab), 1);
    }
}
