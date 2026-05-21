//! Dictionary factory for creating different backend implementations.
//!
//! This module provides a unified interface for creating dictionary instances
//! across all in-memory backends in the crate. Persistent backends
//! (`PersistentARTrie`, `PersistentARTrieChar`, `PersistentVocabARTrie`)
//! require a file path and a different construction protocol, so they live
//! outside the factory.
//!
//! # Example
//!
//! ```rust,ignore
//! use libdictenstein::factory::{DictionaryFactory, DictionaryBackend};
//!
//! // Create a DoubleArrayTrie dictionary
//! let dict = DictionaryFactory::create(
//!     DictionaryBackend::DoubleArrayTrie,
//!     vec!["test", "testing", "tested"],
//! );
//!
//! // Create a DynamicDawgChar (Unicode) dictionary
//! let dict = DictionaryFactory::create(
//!     DictionaryBackend::DynamicDawgChar,
//!     vec!["café", "naïve"],
//! );
//! ```

use super::double_array_trie::DoubleArrayTrie;
use super::double_array_trie_char::DoubleArrayTrieChar;
use super::dynamic_dawg::DynamicDawg;
use super::dynamic_dawg_char::DynamicDawgChar;
use super::dynamic_dawg_u64::DynamicDawgU64;
#[cfg(feature = "pathmap-backend")]
use super::pathmap::PathMapDictionary;
#[cfg(feature = "pathmap-backend")]
use super::pathmap_char::PathMapDictionaryChar;
use super::scdawg::Scdawg;
use super::scdawg_char::ScdawgChar;
use super::suffix_automaton::SuffixAutomaton;
use super::suffix_automaton_char::SuffixAutomatonChar;
use super::Dictionary;

/// Dictionary backend types.
///
/// Covers all in-memory backends. Persistent ARTrie variants
/// (`PersistentARTrie{,Char}`, `PersistentVocabARTrie`) are not included
/// here because they require file paths and richer configuration than the
/// factory exposes — construct them directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DictionaryBackend {
    /// PathMap-based trie dictionary (fastest for queries, highest memory).
    #[cfg(feature = "pathmap-backend")]
    PathMap,
    /// PathMap-based trie, character (Unicode) variant.
    #[cfg(feature = "pathmap-backend")]
    PathMapChar,
    /// Double-Array Trie (O(1) transitions, excellent cache, byte-keyed).
    DoubleArrayTrie,
    /// Double-Array Trie, character (Unicode) variant.
    DoubleArrayTrieChar,
    /// Dynamic DAWG dictionary (space-efficient, byte-keyed, supports modifications).
    DynamicDawg,
    /// Dynamic DAWG, character (Unicode) variant.
    DynamicDawgChar,
    /// Dynamic DAWG keyed on `u64` sequences (token sequences, time series).
    DynamicDawgU64,
    /// Suffix automaton dictionary (substring matching, byte-keyed, dynamic).
    SuffixAutomaton,
    /// Suffix automaton, character (Unicode) variant.
    SuffixAutomatonChar,
    /// Compact Suffix DAWG (substring matching, byte-keyed, batch-build).
    Scdawg,
    /// Compact Suffix DAWG, character (Unicode) variant.
    ScdawgChar,
}

impl std::fmt::Display for DictionaryBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(feature = "pathmap-backend")]
            DictionaryBackend::PathMap => write!(f, "PathMap"),
            #[cfg(feature = "pathmap-backend")]
            DictionaryBackend::PathMapChar => write!(f, "PathMapChar"),
            DictionaryBackend::DoubleArrayTrie => write!(f, "DoubleArrayTrie"),
            DictionaryBackend::DoubleArrayTrieChar => write!(f, "DoubleArrayTrieChar"),
            DictionaryBackend::DynamicDawg => write!(f, "DynamicDAWG"),
            DictionaryBackend::DynamicDawgChar => write!(f, "DynamicDAWGChar"),
            DictionaryBackend::DynamicDawgU64 => write!(f, "DynamicDAWGU64"),
            DictionaryBackend::SuffixAutomaton => write!(f, "SuffixAutomaton"),
            DictionaryBackend::SuffixAutomatonChar => write!(f, "SuffixAutomatonChar"),
            DictionaryBackend::Scdawg => write!(f, "Scdawg"),
            DictionaryBackend::ScdawgChar => write!(f, "ScdawgChar"),
        }
    }
}

/// Unified dictionary container that can hold any backend type.
///
/// Carries only `()`-valued (set-like) dictionaries — for value-bearing
/// dictionaries (`DynamicDawg<V>`, etc.) construct the backend directly.
#[derive(Debug)]
pub enum DictionaryContainer {
    #[cfg(feature = "pathmap-backend")]
    PathMap(PathMapDictionary),
    #[cfg(feature = "pathmap-backend")]
    PathMapChar(PathMapDictionaryChar),
    DoubleArrayTrie(DoubleArrayTrie),
    DoubleArrayTrieChar(DoubleArrayTrieChar),
    DynamicDawg(DynamicDawg),
    DynamicDawgChar(DynamicDawgChar),
    DynamicDawgU64(DynamicDawgU64),
    SuffixAutomaton(SuffixAutomaton),
    SuffixAutomatonChar(SuffixAutomatonChar),
    Scdawg(Scdawg),
    ScdawgChar(ScdawgChar),
}

impl DictionaryContainer {
    /// Get the backend type of this container.
    pub fn backend(&self) -> DictionaryBackend {
        match self {
            #[cfg(feature = "pathmap-backend")]
            DictionaryContainer::PathMap(_) => DictionaryBackend::PathMap,
            #[cfg(feature = "pathmap-backend")]
            DictionaryContainer::PathMapChar(_) => DictionaryBackend::PathMapChar,
            DictionaryContainer::DoubleArrayTrie(_) => DictionaryBackend::DoubleArrayTrie,
            DictionaryContainer::DoubleArrayTrieChar(_) => DictionaryBackend::DoubleArrayTrieChar,
            DictionaryContainer::DynamicDawg(_) => DictionaryBackend::DynamicDawg,
            DictionaryContainer::DynamicDawgChar(_) => DictionaryBackend::DynamicDawgChar,
            DictionaryContainer::DynamicDawgU64(_) => DictionaryBackend::DynamicDawgU64,
            DictionaryContainer::SuffixAutomaton(_) => DictionaryBackend::SuffixAutomaton,
            DictionaryContainer::SuffixAutomatonChar(_) => DictionaryBackend::SuffixAutomatonChar,
            DictionaryContainer::Scdawg(_) => DictionaryBackend::Scdawg,
            DictionaryContainer::ScdawgChar(_) => DictionaryBackend::ScdawgChar,
        }
    }

    /// Get the number of terms in the dictionary.
    pub fn len(&self) -> Option<usize> {
        match self {
            #[cfg(feature = "pathmap-backend")]
            DictionaryContainer::PathMap(d) => d.len(),
            #[cfg(feature = "pathmap-backend")]
            DictionaryContainer::PathMapChar(d) => d.len(),
            DictionaryContainer::DoubleArrayTrie(d) => d.len(),
            DictionaryContainer::DoubleArrayTrieChar(d) => d.len(),
            DictionaryContainer::DynamicDawg(d) => d.len(),
            DictionaryContainer::DynamicDawgChar(d) => d.len(),
            DictionaryContainer::DynamicDawgU64(d) => d.len(),
            DictionaryContainer::SuffixAutomaton(d) => d.len(),
            DictionaryContainer::SuffixAutomatonChar(d) => d.len(),
            DictionaryContainer::Scdawg(d) => d.len(),
            DictionaryContainer::ScdawgChar(d) => d.len(),
        }
    }

    /// Check if the dictionary is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == Some(0)
    }

    /// Check if a term exists in the dictionary.
    pub fn contains(&self, term: &str) -> bool {
        match self {
            #[cfg(feature = "pathmap-backend")]
            DictionaryContainer::PathMap(d) => d.contains(term),
            #[cfg(feature = "pathmap-backend")]
            DictionaryContainer::PathMapChar(d) => d.contains(term),
            DictionaryContainer::DoubleArrayTrie(d) => d.contains(term),
            DictionaryContainer::DoubleArrayTrieChar(d) => d.contains(term),
            DictionaryContainer::DynamicDawg(d) => d.contains(term),
            DictionaryContainer::DynamicDawgChar(d) => d.contains(term),
            DictionaryContainer::DynamicDawgU64(d) => d.contains(term),
            DictionaryContainer::SuffixAutomaton(d) => d.contains(term),
            DictionaryContainer::SuffixAutomatonChar(d) => d.contains(term),
            DictionaryContainer::Scdawg(d) => d.contains(term),
            DictionaryContainer::ScdawgChar(d) => d.contains(term),
        }
    }
}

/// Factory for creating dictionaries with different backends.
pub struct DictionaryFactory;

impl DictionaryFactory {
    /// Create a dictionary with the specified backend.
    ///
    /// # Arguments
    ///
    /// * `backend` - The backend implementation to use
    /// * `terms` - Iterator of terms to insert
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use libdictenstein::factory::{DictionaryFactory, DictionaryBackend};
    ///
    /// let dict = DictionaryFactory::create(
    ///     DictionaryBackend::DynamicDawg,
    ///     vec!["hello", "world"],
    /// );
    /// assert!(dict.contains("hello"));
    /// ```
    pub fn create<I, S>(backend: DictionaryBackend, terms: I) -> DictionaryContainer
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        match backend {
            #[cfg(feature = "pathmap-backend")]
            DictionaryBackend::PathMap => {
                DictionaryContainer::PathMap(PathMapDictionary::from_terms(terms))
            }
            #[cfg(feature = "pathmap-backend")]
            DictionaryBackend::PathMapChar => {
                DictionaryContainer::PathMapChar(PathMapDictionaryChar::from_terms(terms))
            }
            DictionaryBackend::DoubleArrayTrie => {
                DictionaryContainer::DoubleArrayTrie(DoubleArrayTrie::from_terms(terms))
            }
            DictionaryBackend::DoubleArrayTrieChar => {
                DictionaryContainer::DoubleArrayTrieChar(DoubleArrayTrieChar::from_terms(terms))
            }
            DictionaryBackend::DynamicDawg => {
                DictionaryContainer::DynamicDawg(DynamicDawg::from_terms(terms))
            }
            DictionaryBackend::DynamicDawgChar => {
                DictionaryContainer::DynamicDawgChar(DynamicDawgChar::from_terms(terms))
            }
            DictionaryBackend::DynamicDawgU64 => {
                DictionaryContainer::DynamicDawgU64(DynamicDawgU64::from_terms(terms))
            }
            DictionaryBackend::SuffixAutomaton => {
                DictionaryContainer::SuffixAutomaton(SuffixAutomaton::from_texts(terms))
            }
            DictionaryBackend::SuffixAutomatonChar => {
                DictionaryContainer::SuffixAutomatonChar(SuffixAutomatonChar::from_texts(terms))
            }
            DictionaryBackend::Scdawg => {
                DictionaryContainer::Scdawg(Scdawg::from_terms(terms))
            }
            DictionaryBackend::ScdawgChar => {
                DictionaryContainer::ScdawgChar(ScdawgChar::from_terms(terms))
            }
        }
    }

    /// Create an empty dictionary with the specified backend.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use libdictenstein::factory::{DictionaryFactory, DictionaryBackend};
    ///
    /// let dict = DictionaryFactory::empty(DictionaryBackend::DynamicDawg);
    /// assert_eq!(dict.len(), Some(0));
    /// ```
    pub fn empty(backend: DictionaryBackend) -> DictionaryContainer {
        match backend {
            #[cfg(feature = "pathmap-backend")]
            DictionaryBackend::PathMap => DictionaryContainer::PathMap(PathMapDictionary::new()),
            #[cfg(feature = "pathmap-backend")]
            DictionaryBackend::PathMapChar => {
                DictionaryContainer::PathMapChar(PathMapDictionaryChar::new())
            }
            DictionaryBackend::DoubleArrayTrie => {
                DictionaryContainer::DoubleArrayTrie(DoubleArrayTrie::new())
            }
            DictionaryBackend::DoubleArrayTrieChar => {
                // DoubleArrayTrieChar uses `empty()` instead of `new()`.
                DictionaryContainer::DoubleArrayTrieChar(DoubleArrayTrieChar::empty())
            }
            DictionaryBackend::DynamicDawg => DictionaryContainer::DynamicDawg(DynamicDawg::new()),
            DictionaryBackend::DynamicDawgChar => {
                DictionaryContainer::DynamicDawgChar(DynamicDawgChar::new())
            }
            DictionaryBackend::DynamicDawgU64 => {
                DictionaryContainer::DynamicDawgU64(DynamicDawgU64::new())
            }
            DictionaryBackend::SuffixAutomaton => {
                DictionaryContainer::SuffixAutomaton(SuffixAutomaton::new())
            }
            DictionaryBackend::SuffixAutomatonChar => {
                DictionaryContainer::SuffixAutomatonChar(SuffixAutomatonChar::new())
            }
            DictionaryBackend::Scdawg => DictionaryContainer::Scdawg(Scdawg::new()),
            DictionaryBackend::ScdawgChar => DictionaryContainer::ScdawgChar(ScdawgChar::new()),
        }
    }

    /// List of all available backends.
    pub fn available_backends() -> Vec<DictionaryBackend> {
        vec![
            #[cfg(feature = "pathmap-backend")]
            DictionaryBackend::PathMap,
            #[cfg(feature = "pathmap-backend")]
            DictionaryBackend::PathMapChar,
            DictionaryBackend::DoubleArrayTrie,
            DictionaryBackend::DoubleArrayTrieChar,
            DictionaryBackend::DynamicDawg,
            DictionaryBackend::DynamicDawgChar,
            DictionaryBackend::DynamicDawgU64,
            DictionaryBackend::SuffixAutomaton,
            DictionaryBackend::SuffixAutomatonChar,
            DictionaryBackend::Scdawg,
            DictionaryBackend::ScdawgChar,
        ]
    }

    /// Description of a backend's characteristics.
    pub fn backend_description(backend: DictionaryBackend) -> &'static str {
        match backend {
            #[cfg(feature = "pathmap-backend")]
            DictionaryBackend::PathMap => {
                "PathMap-based byte trie. Fast queries, higher memory; in-memory only."
            }
            #[cfg(feature = "pathmap-backend")]
            DictionaryBackend::PathMapChar => {
                "PathMap-based character trie. Unicode-aware variant of PathMap."
            }
            DictionaryBackend::DoubleArrayTrie => {
                "Byte-keyed double-array trie. O(1) transitions, excellent cache locality, \
                 read-mostly. Best for static dictionaries."
            }
            DictionaryBackend::DoubleArrayTrieChar => {
                "Character-keyed double-array trie. Unicode-aware variant of DoubleArrayTrie."
            }
            DictionaryBackend::DynamicDawg => {
                "Byte-keyed dynamic DAWG. Space-efficient with full dynamic modification \
                 support. Best for evolving dictionaries."
            }
            DictionaryBackend::DynamicDawgChar => {
                "Character-keyed dynamic DAWG. Unicode-aware variant of DynamicDawg."
            }
            DictionaryBackend::DynamicDawgU64 => {
                "u64-keyed dynamic DAWG. For token-sequence dictionaries, time series, \
                 or any application keying on 64-bit symbols."
            }
            DictionaryBackend::SuffixAutomaton => {
                "Byte-keyed suffix automaton. Substring matching anywhere in indexed text. \
                 Best for full-text and code search."
            }
            DictionaryBackend::SuffixAutomatonChar => {
                "Character-keyed suffix automaton. Unicode-aware variant of SuffixAutomaton."
            }
            DictionaryBackend::Scdawg => {
                "Byte-keyed compact suffix DAWG (Blumer et al. 1987). Substring matching \
                 with smaller memory footprint than SuffixAutomaton for static inputs."
            }
            DictionaryBackend::ScdawgChar => {
                "Character-keyed compact suffix DAWG. Unicode-aware variant of Scdawg."
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(feature = "pathmap-backend")]
    fn test_factory_pathmap() {
        let dict = DictionaryFactory::create(
            DictionaryBackend::PathMap,
            vec!["test", "testing", "tested"],
        );

        assert_eq!(dict.backend(), DictionaryBackend::PathMap);
        assert_eq!(dict.len(), Some(3));
        assert!(dict.contains("test"));
        assert!(dict.contains("testing"));
        assert!(dict.contains("tested"));
        assert!(!dict.contains("tester"));
    }

    #[test]
    fn test_factory_dynamic_dawg() {
        let dict =
            DictionaryFactory::create(DictionaryBackend::DynamicDawg, vec!["foo", "bar", "baz"]);

        assert_eq!(dict.backend(), DictionaryBackend::DynamicDawg);
        assert_eq!(dict.len(), Some(3));
        assert!(dict.contains("foo"));
        assert!(dict.contains("bar"));
        assert!(dict.contains("baz"));
        assert!(!dict.contains("qux"));
    }

    #[test]
    fn test_factory_unicode_backends() {
        let unicode_terms = vec!["café", "naïve", "日本語"];

        for backend in [
            DictionaryBackend::DoubleArrayTrieChar,
            DictionaryBackend::DynamicDawgChar,
            DictionaryBackend::SuffixAutomatonChar,
            DictionaryBackend::ScdawgChar,
        ] {
            let dict = DictionaryFactory::create(backend, unicode_terms.clone());
            assert!(
                dict.contains("café"),
                "{backend} should contain 'café'"
            );
            assert!(
                dict.contains("naïve"),
                "{backend} should contain 'naïve'"
            );
            assert!(
                dict.contains("日本語"),
                "{backend} should contain '日本語'"
            );
        }
    }

    #[test]
    fn test_factory_empty() {
        for backend in DictionaryFactory::available_backends() {
            let dict = DictionaryFactory::empty(backend);
            assert_eq!(dict.len(), Some(0), "{backend}");
            assert!(dict.is_empty(), "{backend}");
        }
    }

    #[test]
    fn test_backend_display() {
        #[cfg(feature = "pathmap-backend")]
        assert_eq!(DictionaryBackend::PathMap.to_string(), "PathMap");
        assert_eq!(DictionaryBackend::DynamicDawg.to_string(), "DynamicDAWG");
        assert_eq!(
            DictionaryBackend::DoubleArrayTrieChar.to_string(),
            "DoubleArrayTrieChar"
        );
        assert_eq!(DictionaryBackend::Scdawg.to_string(), "Scdawg");
    }

    #[test]
    fn test_available_backends() {
        let backends = DictionaryFactory::available_backends();
        // 11 backends total: 4 byte + 4 char + DynamicDawgU64 + 2 scdawg.
        // PathMap and PathMapChar gated behind feature.
        #[cfg(feature = "pathmap-backend")]
        assert_eq!(backends.len(), 11);
        #[cfg(not(feature = "pathmap-backend"))]
        assert_eq!(backends.len(), 9);
        assert!(backends.contains(&DictionaryBackend::DoubleArrayTrie));
        assert!(backends.contains(&DictionaryBackend::DynamicDawg));
        assert!(backends.contains(&DictionaryBackend::DynamicDawgChar));
        assert!(backends.contains(&DictionaryBackend::SuffixAutomaton));
        assert!(backends.contains(&DictionaryBackend::Scdawg));
    }

    #[test]
    fn test_backend_descriptions() {
        for backend in DictionaryFactory::available_backends() {
            let desc = DictionaryFactory::backend_description(backend);
            assert!(!desc.is_empty(), "{backend} has empty description");
        }
    }

    #[test]
    fn test_all_backends_work() {
        let terms = vec!["apple", "banana", "cherry"];

        for backend in DictionaryFactory::available_backends() {
            let dict = DictionaryFactory::create(backend, terms.clone());
            assert!(
                dict.contains("apple"),
                "{backend} should contain 'apple'"
            );
            assert!(
                dict.contains("banana"),
                "{backend} should contain 'banana'"
            );
            assert!(
                dict.contains("cherry"),
                "{backend} should contain 'cherry'"
            );
        }
    }
}
