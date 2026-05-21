//! BijectiveMap: Generic bijective dictionary for arbitrary hashable values.
//!
//! This module provides [`BijectiveMap`], a bidirectional dictionary that maps
//! terms to arbitrary hashable values while maintaining the bijection invariant.
//!
//! `BijectiveMap` supports any value type that implements `DictionaryValue + Eq + Hash`.
//! For vocabulary use cases with sequential `u64` indices, use
//! [`PersistentVocabARTrie`](crate::persistent_vocab_artrie::PersistentVocabARTrie).
//!
//! # Use Cases
//!
//! - Symbol tables with arbitrary IDs
//! - Bidirectional mappings between strings and enums
//! - Any bijection where values aren't sequential integers

use crate::bijective::BijectiveDictionary;
use crate::dynamic_dawg_char::DynamicDawgChar;
use crate::sync_compat::RwLock;
use crate::value::DictionaryValue;
use crate::{Dictionary, MappedDictionary};
use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;

/// A bijective dictionary mapping terms to arbitrary hashable values.
///
/// `BijectiveMap` provides efficient bidirectional lookup between terms (strings)
/// and values of any type that implements `DictionaryValue + Eq + Hash`.
///
/// # Type Parameter
///
/// - `V`: The value type. Must implement [`DictionaryValue`], [`Eq`], and [`Hash`].
///
/// # Performance
///
/// - Forward lookup (`get_value`): O(k) where k = term length (via DAWG traversal)
/// - Reverse lookup (`get_term`): O(1) average (via hash lookup)
/// - Memory: ~1.5x (DAWG + HashMap)
///
/// # Thread Safety
///
/// `BijectiveMap` is fully thread-safe:
/// - Forward lookups use the thread-safe `DynamicDawgChar` backend
/// - Reverse lookups use `Arc<RwLock<HashMap<V, String>>>`
///
/// # Examples
///
/// ```rust
/// use libdictenstein::bijective::BijectiveMap;
///
/// let bimap: BijectiveMap<String> = BijectiveMap::new();
///
/// bimap.insert("key1", "value1".to_string());
/// bimap.insert("key2", "value2".to_string());
///
/// // Forward lookup
/// assert_eq!(bimap.get_value("key1"), Some("value1".to_string()));
///
/// // Reverse lookup
/// assert_eq!(bimap.get_term(&"value1".to_string()), Some("key1".to_string()));
/// ```
#[derive(Debug)]
pub struct BijectiveMap<V: DictionaryValue + Eq + Hash> {
    /// Forward mapping: term → value (using DynamicDawgChar)
    forward: DynamicDawgChar<V>,

    /// Reverse mapping: value → term
    reverse: Arc<RwLock<HashMap<V, String>>>,
}

impl<V: DictionaryValue + Eq + Hash> Clone for BijectiveMap<V> {
    fn clone(&self) -> Self {
        Self {
            forward: self.forward.clone(),
            reverse: Arc::new(RwLock::new(self.reverse.read().clone())),
        }
    }
}

impl<V: DictionaryValue + Eq + Hash> Default for BijectiveMap<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V: DictionaryValue + Eq + Hash> BijectiveMap<V> {
    /// Create an empty bijective map.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::bijective::BijectiveMap;
    ///
    /// let bimap: BijectiveMap<i32> = BijectiveMap::new();
    /// assert!(bimap.is_empty());
    /// ```
    pub fn new() -> Self {
        Self {
            forward: DynamicDawgChar::new(),
            reverse: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Create a bijective map with pre-allocated capacity.
    ///
    /// This can improve performance when you know the approximate number
    /// of entries in advance.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::bijective::BijectiveMap;
    ///
    /// let bimap: BijectiveMap<String> = BijectiveMap::with_capacity(1000);
    /// ```
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            forward: DynamicDawgChar::new(),
            reverse: Arc::new(RwLock::new(HashMap::with_capacity(capacity))),
        }
    }

    /// Build a bijective map from an iterator of (term, value) pairs.
    ///
    /// # Panics
    ///
    /// Panics if:
    /// - Any term appears more than once
    /// - Any value appears more than once
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::bijective::BijectiveMap;
    ///
    /// let bimap = BijectiveMap::from_pairs([
    ///     ("a", 1),
    ///     ("b", 2),
    ///     ("c", 3),
    /// ]);
    ///
    /// assert_eq!(bimap.get_value("a"), Some(1));
    /// assert_eq!(bimap.get_term(&1), Some("a".to_string()));
    /// ```
    pub fn from_pairs<I, S>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (S, V)>,
        S: AsRef<str>,
    {
        let bimap = Self::new();
        for (term, value) in pairs {
            bimap.insert(term.as_ref(), value);
        }
        bimap
    }

    /// Insert a term-value pair.
    ///
    /// # Panics
    ///
    /// Panics if:
    /// - The term already exists in the map (would break key uniqueness)
    /// - The value already exists in the map (would break value uniqueness)
    ///
    /// This strict behavior ensures the bijection invariant is never violated.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::bijective::BijectiveMap;
    ///
    /// let bimap: BijectiveMap<i32> = BijectiveMap::new();
    /// bimap.insert("one", 1);
    /// bimap.insert("two", 2);
    ///
    /// assert_eq!(bimap.get_value("one"), Some(1));
    /// assert_eq!(bimap.get_term(&1), Some("one".to_string()));
    /// ```
    ///
    /// ```rust,should_panic
    /// use libdictenstein::bijective::BijectiveMap;
    ///
    /// let bimap: BijectiveMap<i32> = BijectiveMap::new();
    /// bimap.insert("one", 1);
    /// bimap.insert("one", 2);  // Panics: duplicate term
    /// ```
    ///
    /// ```rust,should_panic
    /// use libdictenstein::bijective::BijectiveMap;
    ///
    /// let bimap: BijectiveMap<i32> = BijectiveMap::new();
    /// bimap.insert("one", 1);
    /// bimap.insert("uno", 1);  // Panics: duplicate value
    /// ```
    pub fn insert(&self, term: &str, value: V) {
        // Check for duplicate term
        if self.forward.get_value(term).is_some() {
            panic!(
                "BijectiveMap::insert: duplicate term '{}' violates bijection invariant",
                term
            );
        }

        // Check for duplicate value
        {
            let reverse = self.reverse.read();
            if reverse.contains_key(&value) {
                panic!("BijectiveMap::insert: duplicate value violates bijection invariant");
            }
        }

        // Insert into both mappings
        self.forward.insert_with_value(term, value.clone());

        {
            let mut reverse = self.reverse.write();
            reverse.insert(value, term.to_string());
        }
    }

    /// Try to insert a term-value pair, returning an error if it would violate
    /// the bijection invariant.
    ///
    /// This is a non-panicking alternative to [`insert`](Self::insert).
    ///
    /// # Returns
    ///
    /// - `Ok(())` if the insertion succeeded
    /// - `Err(InsertError::DuplicateTerm)` if the term already exists
    /// - `Err(InsertError::DuplicateValue)` if the value already exists
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::bijective::{BijectiveMap, InsertError};
    ///
    /// let bimap: BijectiveMap<i32> = BijectiveMap::new();
    ///
    /// assert!(bimap.try_insert("one", 1).is_ok());
    /// assert_eq!(bimap.try_insert("one", 2), Err(InsertError::DuplicateTerm));
    /// assert_eq!(bimap.try_insert("uno", 1), Err(InsertError::DuplicateValue));
    /// ```
    pub fn try_insert(&self, term: &str, value: V) -> Result<(), InsertError> {
        // Check for duplicate term
        if self.forward.get_value(term).is_some() {
            return Err(InsertError::DuplicateTerm);
        }

        // Check for duplicate value
        {
            let reverse = self.reverse.read();
            if reverse.contains_key(&value) {
                return Err(InsertError::DuplicateValue);
            }
        }

        // Insert into both mappings
        self.forward.insert_with_value(term, value.clone());

        {
            let mut reverse = self.reverse.write();
            reverse.insert(value, term.to_string());
        }

        Ok(())
    }

    /// Get the value associated with a term.
    ///
    /// Returns `None` if the term is not in the map.
    ///
    /// # Performance
    ///
    /// O(k) where k = term length (DAWG traversal).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::bijective::BijectiveMap;
    ///
    /// let bimap: BijectiveMap<i32> = BijectiveMap::new();
    /// bimap.insert("hello", 42);
    ///
    /// assert_eq!(bimap.get_value("hello"), Some(42));
    /// assert_eq!(bimap.get_value("missing"), None);
    /// ```
    #[inline]
    pub fn get_value(&self, term: &str) -> Option<V> {
        self.forward.get_value(term)
    }

    /// Get the term associated with a value.
    ///
    /// Returns `None` if the value is not in the map.
    ///
    /// # Performance
    ///
    /// O(1) average (hash lookup).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::bijective::BijectiveMap;
    ///
    /// let bimap: BijectiveMap<i32> = BijectiveMap::new();
    /// bimap.insert("hello", 42);
    ///
    /// assert_eq!(bimap.get_term(&42), Some("hello".to_string()));
    /// assert_eq!(bimap.get_term(&999), None);
    /// ```
    #[inline]
    pub fn get_term(&self, value: &V) -> Option<String> {
        self.reverse.read().get(value).cloned()
    }

    /// Check if a term exists in the map.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::bijective::BijectiveMap;
    ///
    /// let bimap: BijectiveMap<i32> = BijectiveMap::new();
    /// bimap.insert("hello", 42);
    ///
    /// assert!(bimap.contains_term("hello"));
    /// assert!(!bimap.contains_term("missing"));
    /// ```
    #[inline]
    pub fn contains_term(&self, term: &str) -> bool {
        self.forward.get_value(term).is_some()
    }

    /// Check if a value exists in the map.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::bijective::BijectiveMap;
    ///
    /// let bimap: BijectiveMap<i32> = BijectiveMap::new();
    /// bimap.insert("hello", 42);
    ///
    /// assert!(bimap.contains_value(&42));
    /// assert!(!bimap.contains_value(&999));
    /// ```
    #[inline]
    pub fn contains_value(&self, value: &V) -> bool {
        self.reverse.read().contains_key(value)
    }

    /// Get the number of term-value pairs in the map.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::bijective::BijectiveMap;
    ///
    /// let bimap: BijectiveMap<i32> = BijectiveMap::new();
    /// assert_eq!(bimap.len(), 0);
    ///
    /// bimap.insert("a", 1);
    /// bimap.insert("b", 2);
    /// assert_eq!(bimap.len(), 2);
    /// ```
    #[inline]
    pub fn len(&self) -> usize {
        self.reverse.read().len()
    }

    /// Check if the map is empty.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::bijective::BijectiveMap;
    ///
    /// let bimap: BijectiveMap<i32> = BijectiveMap::new();
    /// assert!(bimap.is_empty());
    ///
    /// bimap.insert("a", 1);
    /// assert!(!bimap.is_empty());
    /// ```
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.reverse.read().is_empty()
    }

    /// Iterate over all (term, value) pairs.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::bijective::BijectiveMap;
    ///
    /// let bimap = BijectiveMap::from_pairs([
    ///     ("a", 1),
    ///     ("b", 2),
    /// ]);
    ///
    /// for (term, value) in bimap.iter() {
    ///     println!("{} -> {}", term, value);
    /// }
    /// ```
    pub fn iter(&self) -> impl Iterator<Item = (String, V)> + '_ {
        // Clone the data to avoid lifetime issues with the lock guard
        let reverse = self.reverse.read();
        reverse
            .iter()
            .map(|(v, t)| (t.clone(), v.clone()))
            .collect::<Vec<_>>()
            .into_iter()
    }

    /// Iterate over all terms.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::bijective::BijectiveMap;
    ///
    /// let bimap = BijectiveMap::from_pairs([("a", 1), ("b", 2)]);
    ///
    /// let terms: Vec<_> = bimap.terms().collect();
    /// assert!(terms.contains(&"a".to_string()));
    /// assert!(terms.contains(&"b".to_string()));
    /// ```
    pub fn terms(&self) -> impl Iterator<Item = String> + '_ {
        let reverse = self.reverse.read();
        reverse.values().cloned().collect::<Vec<_>>().into_iter()
    }

    /// Iterate over all values.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::bijective::BijectiveMap;
    ///
    /// let bimap = BijectiveMap::from_pairs([("a", 1), ("b", 2)]);
    ///
    /// let values: Vec<_> = bimap.values().collect();
    /// assert!(values.contains(&1));
    /// assert!(values.contains(&2));
    /// ```
    pub fn values(&self) -> impl Iterator<Item = V> + '_ {
        let reverse = self.reverse.read();
        reverse.keys().cloned().collect::<Vec<_>>().into_iter()
    }

    /// Get a reference to the underlying forward dictionary.
    ///
    /// This is useful for advanced operations like fuzzy matching with
    /// Levenshtein automata.
    #[inline]
    pub fn forward(&self) -> &DynamicDawgChar<V> {
        &self.forward
    }
}

// =============================================================================
// Error types
// =============================================================================

/// Error returned when an insertion would violate the bijection invariant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertError {
    /// The term already exists in the map.
    DuplicateTerm,
    /// The value already exists in the map.
    DuplicateValue,
}

impl std::fmt::Display for InsertError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InsertError::DuplicateTerm => write!(f, "duplicate term violates bijection invariant"),
            InsertError::DuplicateValue => {
                write!(f, "duplicate value violates bijection invariant")
            }
        }
    }
}

impl std::error::Error for InsertError {}

// =============================================================================
// Dictionary trait implementation
// =============================================================================

impl<V: DictionaryValue + Eq + Hash> Dictionary for BijectiveMap<V> {
    type Node = <DynamicDawgChar<V> as Dictionary>::Node;

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

impl<V: DictionaryValue + Eq + Hash> MappedDictionary for BijectiveMap<V> {
    type Value = V;

    fn get_value(&self, term: &str) -> Option<Self::Value> {
        Self::get_value(self, term)
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

impl<V: DictionaryValue + Eq + Hash> BijectiveDictionary for BijectiveMap<V> {
    fn get_term(&self, value: &Self::Value) -> Option<std::borrow::Cow<'_, str>> {
        // Acquire the read guard, look up, clone the String into a Cow::Owned.
        // The clone is necessary because the read guard cannot escape this
        // function's stack frame and `Cow::Borrowed(&str)` would require a
        // borrow tied to `self`, not to the guard. The previous unsafe
        // pointer-dereference shortcut was unsound under concurrent inserts
        // (HashMap rehashing invalidates element pointers).
        let reverse = self.reverse.read();
        reverse
            .get(value)
            .map(|s| std::borrow::Cow::Owned(s.clone()))
    }

    fn contains_value(&self, value: &Self::Value) -> bool {
        Self::contains_value(self, value)
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
    fn test_empty_map() {
        let bimap: BijectiveMap<i32> = BijectiveMap::new();
        assert!(bimap.is_empty());
        assert_eq!(bimap.len(), 0);
        assert_eq!(bimap.get_value("test"), None);
        assert_eq!(bimap.get_term(&0), None);
    }

    #[test]
    fn test_single_pair() {
        let bimap: BijectiveMap<i32> = BijectiveMap::new();
        bimap.insert("hello", 42);

        assert_eq!(bimap.len(), 1);
        assert_eq!(bimap.get_value("hello"), Some(42));
        assert_eq!(bimap.get_term(&42), Some("hello".to_string()));
    }

    #[test]
    fn test_multiple_pairs() {
        let bimap = BijectiveMap::from_pairs([("a", 1), ("b", 2), ("c", 3)]);

        assert_eq!(bimap.len(), 3);
        assert_eq!(bimap.get_value("a"), Some(1));
        assert_eq!(bimap.get_value("b"), Some(2));
        assert_eq!(bimap.get_value("c"), Some(3));
        assert_eq!(bimap.get_term(&1), Some("a".to_string()));
        assert_eq!(bimap.get_term(&2), Some("b".to_string()));
        assert_eq!(bimap.get_term(&3), Some("c".to_string()));
    }

    #[test]
    #[should_panic(expected = "duplicate term")]
    fn test_duplicate_term_panics() {
        let bimap: BijectiveMap<i32> = BijectiveMap::new();
        bimap.insert("hello", 1);
        bimap.insert("hello", 2); // Should panic
    }

    #[test]
    #[should_panic(expected = "duplicate value")]
    fn test_duplicate_value_panics() {
        let bimap: BijectiveMap<i32> = BijectiveMap::new();
        bimap.insert("one", 1);
        bimap.insert("uno", 1); // Should panic
    }

    #[test]
    fn test_try_insert() {
        let bimap: BijectiveMap<i32> = BijectiveMap::new();

        assert!(bimap.try_insert("one", 1).is_ok());
        assert_eq!(bimap.try_insert("one", 2), Err(InsertError::DuplicateTerm));
        assert_eq!(bimap.try_insert("uno", 1), Err(InsertError::DuplicateValue));

        // Map should still be valid
        assert_eq!(bimap.len(), 1);
        assert_eq!(bimap.get_value("one"), Some(1));
    }

    #[test]
    fn test_contains() {
        let bimap = BijectiveMap::from_pairs([("hello", 42)]);

        assert!(bimap.contains_term("hello"));
        assert!(!bimap.contains_term("world"));
        assert!(bimap.contains_value(&42));
        assert!(!bimap.contains_value(&999));
    }

    #[test]
    fn test_unicode_terms() {
        let bimap = BijectiveMap::from_pairs([("café", 1), ("日本語", 2), ("🎉", 3), ("naïve", 4)]);

        assert_eq!(bimap.get_value("café"), Some(1));
        assert_eq!(bimap.get_value("日本語"), Some(2));
        assert_eq!(bimap.get_value("🎉"), Some(3));
        assert_eq!(bimap.get_value("naïve"), Some(4));

        assert_eq!(bimap.get_term(&1), Some("café".to_string()));
        assert_eq!(bimap.get_term(&2), Some("日本語".to_string()));
        assert_eq!(bimap.get_term(&3), Some("🎉".to_string()));
        assert_eq!(bimap.get_term(&4), Some("naïve".to_string()));
    }

    #[test]
    fn test_string_values() {
        let bimap: BijectiveMap<String> = BijectiveMap::new();
        bimap.insert("key1", "value1".to_string());
        bimap.insert("key2", "value2".to_string());

        assert_eq!(bimap.get_value("key1"), Some("value1".to_string()));
        assert_eq!(
            bimap.get_term(&"value1".to_string()),
            Some("key1".to_string())
        );
    }

    #[test]
    fn test_bijection_invariant() {
        let bimap = BijectiveMap::from_pairs([("a", 1), ("b", 2), ("c", 3)]);

        // For every (term, value) pair, the bijection should hold
        for (term, value) in bimap.iter() {
            assert_eq!(bimap.get_value(&term), Some(value.clone()));
            assert_eq!(bimap.get_term(&value), Some(term));
        }
    }

    #[test]
    fn test_iter() {
        let bimap = BijectiveMap::from_pairs([("x", 1), ("y", 2), ("z", 3)]);

        let pairs: Vec<_> = bimap.iter().collect();

        assert_eq!(pairs.len(), 3);
        assert!(pairs.contains(&("x".to_string(), 1)));
        assert!(pairs.contains(&("y".to_string(), 2)));
        assert!(pairs.contains(&("z".to_string(), 3)));
    }

    #[test]
    fn test_terms_and_values() {
        let bimap = BijectiveMap::from_pairs([("a", 1), ("b", 2)]);

        let terms: Vec<_> = bimap.terms().collect();
        let values: Vec<_> = bimap.values().collect();

        assert_eq!(terms.len(), 2);
        assert!(terms.contains(&"a".to_string()));
        assert!(terms.contains(&"b".to_string()));

        assert_eq!(values.len(), 2);
        assert!(values.contains(&1));
        assert!(values.contains(&2));
    }

    #[test]
    fn test_mapped_dictionary_trait() {
        use crate::MappedDictionary;

        let bimap = BijectiveMap::from_pairs([("test", 42)]);

        // Test via trait
        assert_eq!(MappedDictionary::get_value(&bimap, "test"), Some(42));
        assert_eq!(MappedDictionary::get_value(&bimap, "missing"), None);
    }

    #[test]
    fn test_bijective_dictionary_trait() {
        use crate::bijective::BijectiveDictionary;

        let bimap = BijectiveMap::from_pairs([("test", 42)]);

        // Test via trait (returns Option<Cow<'_, str>> now).
        let term = BijectiveDictionary::get_term(&bimap, &42);
        assert_eq!(term.as_deref(), Some("test"));
        assert!(BijectiveDictionary::get_term(&bimap, &99).is_none());
        assert!(BijectiveDictionary::contains_value(&bimap, &42));
        assert!(!BijectiveDictionary::contains_value(&bimap, &99));
        assert_eq!(BijectiveDictionary::bijection_len(&bimap), 1);
    }

    #[test]
    fn test_with_capacity() {
        let bimap: BijectiveMap<i32> = BijectiveMap::with_capacity(1000);
        assert!(bimap.is_empty());

        // Should still work normally
        bimap.insert("test", 1);
        assert_eq!(bimap.get_value("test"), Some(1));
    }
}
