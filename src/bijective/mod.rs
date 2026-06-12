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
//! - [`BijectiveMap`]: Generic bijective map for arbitrary hashable value types
//!
//! For vocabulary use cases with sequential `u64` indices, use
//! [`PersistentVocabARTrie`](crate::persistent_artrie::vocab::PersistentVocabARTrie) which provides
//! correct persistent bidirectional lookups via parent pointers and LRU caching.
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
//! the bijection invariant. Use `try_insert` for non-panicking insertion.
//!
//! # Examples
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
//! assert_eq!(bimap.get_term(&"value1".to_string()).as_deref(), Some("key1"));
//! ```
//!
//! # Thread Safety
//!
//! `BijectiveMap` is thread-safe:
//! - Multiple concurrent reads are allowed
//! - Writes use locks for synchronization
//! - The bijection invariant is maintained across concurrent operations

mod bijective_map;

pub use bijective_map::{BijectiveMap, InsertError};

use std::borrow::Cow;

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
/// use libdictenstein::bijective::{BijectiveDictionary, BijectiveMap};
///
/// let bimap = BijectiveMap::from_pairs([("hello", 0u64), ("world", 1u64)]);
///
/// // Forward lookup via MappedDictionary trait
/// use libdictenstein::MappedDictionary;
/// assert_eq!(bimap.get_value("hello"), Some(0u64));
///
/// // Reverse lookup via BijectiveDictionary trait (returns Cow<str>)
/// let term = BijectiveDictionary::get_term(&bimap, &0u64);
/// assert_eq!(term.as_deref(), Some("hello"));
/// ```
pub trait BijectiveDictionary: MappedDictionary
where
    Self::Value: DictionaryValue,
{
    /// Look up the term associated with a value.
    ///
    /// Returns `None` if no term maps to this value.
    ///
    /// The return type is `Option<Cow<'_, str>>` so impls can return either a
    /// borrowed `&str` (when the term is stored verbatim and the lifetime can
    /// be tied to `&self`) or an owned `String` (when the term must be
    /// reconstructed on-the-fly, as in persistent-vocab tries). The previous
    /// `Option<&str>` signature forced unsafe pointer dereference in
    /// `BijectiveMap` and forced persistent-vocab impls to stub the method to
    /// `None` — both are fixed by the `Cow` return type.
    ///
    /// # Performance
    ///
    /// Implementation-dependent. For `BijectiveMap<V>`, this is O(1) average
    /// via hash lookup and a single `String` clone.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use std::borrow::Cow;
    /// use libdictenstein::bijective::{BijectiveDictionary, BijectiveMap};
    ///
    /// let bimap = BijectiveMap::from_pairs([("cat", 0), ("dog", 1)]);
    ///
    /// // Use the inherent method (returns owned String)
    /// assert_eq!(bimap.get_term(&0), Some("cat".to_string()));
    /// assert_eq!(bimap.get_term(&1), Some("dog".to_string()));
    /// assert_eq!(bimap.get_term(&999), None);  // No term at this value
    ///
    /// // Or use the trait method explicitly (returns Cow<str>)
    /// let cat = BijectiveDictionary::get_term(&bimap, &0);
    /// assert_eq!(cat.as_deref(), Some("cat"));
    /// // BijectiveMap returns Cow::Owned (clones from the reverse HashMap).
    /// assert!(matches!(cat, Some(Cow::Owned(_))));
    /// ```
    fn get_term(&self, value: &Self::Value) -> Option<Cow<'_, str>>;

    /// Check if a value exists in the dictionary.
    ///
    /// This is a convenience method equivalent to `get_term(value).is_some()`.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::bijective::{BijectiveDictionary, BijectiveMap};
    ///
    /// let bimap = BijectiveMap::from_pairs([("hello", 42)]);
    ///
    /// // Use inherent method
    /// assert!(bimap.contains_value(&42));
    /// assert!(!bimap.contains_value(&999));
    ///
    /// // Or use trait method
    /// assert!(BijectiveDictionary::contains_value(&bimap, &42));
    /// assert!(!BijectiveDictionary::contains_value(&bimap, &999));
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
