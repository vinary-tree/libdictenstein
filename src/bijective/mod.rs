//! Bijective (bidirectional) dictionary types with 1:1 key-value correspondence.
//!
//! This module provides dictionary implementations that support efficient lookups in both
//! directions: forward (term → value) and reverse (value → term). This is useful for:
//!
//! - **Embedding vocabularies**: Map tokens to sequential indices and back
//! - **Tokenization pipelines**: Efficiently convert between strings and numeric IDs
//! - **Symbolic mappings**: Bidirectional lookup for symbol tables, enum mappings, etc.
//!
//! # Key Types
//!
//! - [`BijectiveDictionary`]: Trait extending `MappedDictionary` with reverse lookup capability
//! - [`IndexedVocabulary`]: Optimized for sequential `u64` indices starting at a configurable value
//! - [`BijectiveMap`]: Generic bijective map for arbitrary hashable value types
//!
//! # Design
//!
//! The bijection invariant is strictly enforced:
//!
//! ```text
//! ∀ (k, v) in dictionary:
//!     get_value(k) == Some(v) ⟺ get_term(&v) == Some(k)
//! ```
//!
//! **Duplicate handling**: Inserting a duplicate term or value causes a panic to preserve
//! the bijection invariant. Use `get_or_insert` for idempotent insertions.
//!
//! # Examples
//!
//! ## IndexedVocabulary (Recommended for Embeddings)
//!
//! ```rust
//! use libdictenstein::bijective::IndexedVocabulary;
//!
//! // Create vocabulary with auto-incrementing indices starting at 0
//! let vocab = IndexedVocabulary::from_terms(["apple", "banana", "cherry"]);
//!
//! // Forward lookup: O(k) where k = term length
//! assert_eq!(vocab.get_index("apple"), Some(0));
//! assert_eq!(vocab.get_index("banana"), Some(1));
//!
//! // Reverse lookup: O(1)
//! assert_eq!(vocab.get_term(0), Some("apple"));
//! assert_eq!(vocab.get_term(1), Some("banana"));
//!
//! // Custom start index (e.g., reserve 0 for special tokens)
//! let vocab = IndexedVocabulary::from_terms_with_start(["apple", "banana"], 1);
//! assert_eq!(vocab.get_index("apple"), Some(1));
//! assert_eq!(vocab.start_index(), 1);
//! ```
//!
//! ## BijectiveMap (Generic Values)
//!
//! ```rust
//! use libdictenstein::bijective::BijectiveMap;
//!
//! let bimap: BijectiveMap<String> = BijectiveMap::new();
//! bimap.insert("key1", "value1".to_string());
//! bimap.insert("key2", "value2".to_string());
//!
//! // Forward lookup
//! assert_eq!(bimap.get_value("key1"), Some("value1".to_string()));
//!
//! // Reverse lookup
//! assert_eq!(bimap.get_term(&"value1".to_string()), Some("key1".to_string()));
//! ```
//!
//! # Thread Safety
//!
//! Both `IndexedVocabulary` and `BijectiveMap` are thread-safe:
//! - Multiple concurrent reads are allowed
//! - Writes use atomic operations or locks for synchronization
//! - The bijection invariant is maintained across concurrent operations

mod bijective_map;
mod indexed_vocab;

pub use bijective_map::{BijectiveMap, InsertError};
pub use indexed_vocab::{IndexedVocabulary, IndexedVocabularyDAT, IndexedVocabularyDAWG};

#[cfg(feature = "persistent-artrie")]
pub use indexed_vocab::IndexedVocabularyART;

use crate::value::DictionaryValue;
use crate::MappedDictionary;

/// A dictionary with 1:1 key-value correspondence supporting bidirectional lookup.
///
/// This trait extends [`MappedDictionary`] to provide reverse lookup capability,
/// enabling efficient value-to-term queries in addition to term-to-value queries.
///
/// # Bijection Invariant
///
/// For all `(k, v)` pairs in the dictionary:
///
/// ```text
/// get_value(k) == Some(v) ⟺ get_term(&v) == Some(k)
/// ```
///
/// This means:
/// - Every term maps to exactly one value
/// - Every value maps to exactly one term
/// - No two terms can share the same value
/// - No value exists without a corresponding term
///
/// # Performance
///
/// - Forward lookup (`get_value`): Inherited from [`MappedDictionary`], typically O(k)
/// - Reverse lookup (`get_term`): Implementation-dependent, often O(1) or O(log n)
///
/// # Examples
///
/// ```rust
/// use libdictenstein::bijective::{BijectiveDictionary, IndexedVocabulary};
///
/// let vocab = IndexedVocabulary::from_terms(["hello", "world"]);
///
/// // Forward lookup via MappedDictionary trait
/// use libdictenstein::MappedDictionary;
/// assert_eq!(vocab.get_value("hello"), Some(0u64));
///
/// // Reverse lookup via inherent method (takes value directly)
/// assert_eq!(vocab.get_term(0u64), Some("hello"));
///
/// // Reverse lookup via BijectiveDictionary trait (takes reference)
/// assert_eq!(BijectiveDictionary::get_term(&vocab, &0u64), Some("hello"));
/// ```
pub trait BijectiveDictionary: MappedDictionary
where
    Self::Value: DictionaryValue,
{
    /// Look up the term associated with a value.
    ///
    /// Returns `None` if no term maps to this value.
    ///
    /// # Performance
    ///
    /// Implementation-dependent. For `IndexedVocabulary<u64>`, this is O(1).
    /// For `BijectiveMap<V>`, this is O(1) average via hash lookup.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::bijective::{BijectiveDictionary, IndexedVocabulary};
    ///
    /// let vocab = IndexedVocabulary::from_terms(["cat", "dog"]);
    ///
    /// // Use the inherent method (takes value directly)
    /// assert_eq!(vocab.get_term(0), Some("cat"));
    /// assert_eq!(vocab.get_term(1), Some("dog"));
    /// assert_eq!(vocab.get_term(999), None);  // No term at this index
    ///
    /// // Or use the trait method explicitly (takes reference)
    /// assert_eq!(BijectiveDictionary::get_term(&vocab, &0), Some("cat"));
    /// ```
    fn get_term(&self, value: &Self::Value) -> Option<&str>;

    /// Check if a value exists in the dictionary.
    ///
    /// This is a convenience method equivalent to `get_term(value).is_some()`.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::bijective::{BijectiveDictionary, IndexedVocabulary};
    ///
    /// let vocab = IndexedVocabulary::from_terms(["hello"]);
    ///
    /// // Use inherent method (takes value directly)
    /// assert!(vocab.contains_index(0));
    /// assert!(!vocab.contains_index(999));
    ///
    /// // Or use trait method (takes reference)
    /// assert!(BijectiveDictionary::contains_value(&vocab, &0));
    /// assert!(!BijectiveDictionary::contains_value(&vocab, &999));
    /// ```
    fn contains_value(&self, value: &Self::Value) -> bool {
        self.get_term(value).is_some()
    }

    /// Get the number of term-value pairs in the dictionary.
    ///
    /// For bijective dictionaries, this equals both the number of unique terms
    /// and the number of unique values.
    fn bijection_len(&self) -> usize;
}
