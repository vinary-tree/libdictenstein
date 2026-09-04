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
//! ```rust,no_run
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

use super::double_array_trie::char::DoubleArrayTrieChar;
use super::double_array_trie::{DoubleArrayTrie, DoubleArrayTrieUtf8};
use super::dynamic_dawg::char::DynamicDawgChar;
use super::dynamic_dawg::u64::DynamicDawgU64;
use super::dynamic_dawg::{DynamicDawg, DynamicDawgUtf8};
#[cfg(feature = "pathmap-backend")]
use super::pathmap::char::PathMapDictionaryChar;
#[cfg(feature = "pathmap-backend")]
use super::pathmap::PathMapDictionary;
#[cfg(feature = "pathmap-backend")]
use super::pathmap::PathMapDictionaryUtf8;
use super::scdawg::char::ScdawgChar;
use super::scdawg::Scdawg;
use super::suffix_automaton::char::SuffixAutomatonChar;
use super::suffix_automaton::SuffixAutomaton;
use super::{Dictionary, SyncStrategy};
use crate::{ProfileKind, VariableWidthProfile};

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
    /// PathMap trie whose physical bytes are validated as UTF-8 logical terms.
    #[cfg(feature = "pathmap-backend")]
    PathMapUtf8,
    /// Double-Array Trie (O(1) transitions, excellent cache, byte-keyed).
    DoubleArrayTrie,
    /// Double-Array Trie, character (Unicode) variant.
    DoubleArrayTrieChar,
    /// Byte-backed DAT with UTF-8 profile semantics.
    DoubleArrayTrieUtf8,
    /// Dynamic DAWG dictionary (space-efficient, byte-keyed, supports modifications).
    DynamicDawg,
    /// Dynamic DAWG, character (Unicode) variant.
    DynamicDawgChar,
    /// Byte-backed dynamic DAWG with UTF-8 profile semantics.
    DynamicDawgUtf8,
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

/// Edge-label unit used by a backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKeyUnit {
    /// Raw UTF-8 bytes (`u8`).
    Byte,
    /// Unicode scalar values (`char`).
    Char,
    /// Native 64-bit labels.
    U64,
}

/// Query semantics exposed by a backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendQuerySemantics {
    /// Terms are matched from the root as complete dictionary entries.
    ExactTerm,
    /// Indexed text can be matched from suffix states as substrings.
    Substring,
}

/// In-place update support exposed by the constructed backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendUpdateMode {
    /// The built dictionary is immutable; rebuild to change terms.
    Immutable,
    /// Terms can be inserted but not removed through the public backend API.
    InsertOnly,
    /// Terms can be inserted and removed.
    InsertRemove,
}

/// Machine-readable backend characteristics for selection and benchmarking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendCapabilities {
    /// Edge-label unit used by traversal.
    pub key_unit: BackendKeyUnit,
    /// Exact-term or substring semantics.
    pub query: BackendQuerySemantics,
    /// In-place update support.
    pub updates: BackendUpdateMode,
    /// Synchronization strategy advertised by the backend family.
    pub sync_strategy: SyncStrategy,
    /// Reads do not block on process-local locks.
    pub lock_free_reads: bool,
    /// Mutations do not block on process-local locks.
    pub lock_free_writes: bool,
}

/// Stable logical-profile metadata for a factory backend.
///
/// The canonical profile identity is independent of the legacy Rust backend
/// spelling and is suitable for capability negotiation and serialized
/// descriptors.  `width_bytes == None` denotes a variable-width profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendProfileDescriptor {
    /// Built-in logical profile kind.
    pub kind: ProfileKind,
    /// Canonical name/version identity for persistence and negotiation.
    pub identity: VariableWidthProfile,
    /// Fixed encoded width, or `None` for variable-width atoms.
    pub width_bytes: Option<usize>,
}

impl BackendCapabilities {
    /// Returns true for Unicode scalar-value backends.
    pub fn is_unicode(self) -> bool {
        self.key_unit == BackendKeyUnit::Char
    }

    /// Returns true when the backend supports substring search semantics.
    pub fn supports_substring_search(self) -> bool {
        self.query == BackendQuerySemantics::Substring
    }

    /// Returns true when the backend supports removal through its public API.
    pub fn supports_removal(self) -> bool {
        self.updates == BackendUpdateMode::InsertRemove
    }

    /// Returns true when reads and supported writes are both lock-free.
    pub fn is_fully_lock_free_for_supported_operations(self) -> bool {
        self.lock_free_reads
            && (self.updates == BackendUpdateMode::Immutable || self.lock_free_writes)
    }
}

impl DictionaryBackend {
    /// Return the stable logical profile represented by this legacy backend.
    pub const fn profile_descriptor(self) -> BackendProfileDescriptor {
        let kind = match self {
            #[cfg(feature = "pathmap-backend")]
            Self::PathMap
            | Self::DynamicDawg
            | Self::DoubleArrayTrie
            | Self::SuffixAutomaton
            | Self::Scdawg => ProfileKind::Bytes,
            #[cfg(feature = "pathmap-backend")]
            Self::PathMapChar
            | Self::DynamicDawgChar
            | Self::DoubleArrayTrieChar
            | Self::SuffixAutomatonChar
            | Self::ScdawgChar => ProfileKind::UnicodeScalar,
            #[cfg(feature = "pathmap-backend")]
            Self::PathMapUtf8 => ProfileKind::Utf8,
            Self::DoubleArrayTrieUtf8 | Self::DynamicDawgUtf8 => ProfileKind::Utf8,
            #[cfg(not(feature = "pathmap-backend"))]
            Self::DynamicDawg | Self::DoubleArrayTrie | Self::SuffixAutomaton | Self::Scdawg => {
                ProfileKind::Bytes
            }
            #[cfg(not(feature = "pathmap-backend"))]
            Self::DynamicDawgChar
            | Self::DoubleArrayTrieChar
            | Self::SuffixAutomatonChar
            | Self::ScdawgChar => ProfileKind::UnicodeScalar,
            Self::DynamicDawgU64 => ProfileKind::U64,
        };
        BackendProfileDescriptor {
            kind,
            identity: kind.identity(),
            width_bytes: kind.width_bytes(),
        }
    }

    /// Machine-readable backend characteristics.
    pub fn capabilities(self) -> BackendCapabilities {
        match self {
            #[cfg(feature = "pathmap-backend")]
            DictionaryBackend::PathMap => BackendCapabilities {
                key_unit: BackendKeyUnit::Byte,
                query: BackendQuerySemantics::ExactTerm,
                updates: BackendUpdateMode::InsertRemove,
                sync_strategy: SyncStrategy::InternalSync,
                lock_free_reads: true,
                lock_free_writes: true,
            },
            #[cfg(feature = "pathmap-backend")]
            DictionaryBackend::PathMapChar => BackendCapabilities {
                key_unit: BackendKeyUnit::Char,
                query: BackendQuerySemantics::ExactTerm,
                updates: BackendUpdateMode::InsertRemove,
                sync_strategy: SyncStrategy::InternalSync,
                lock_free_reads: true,
                lock_free_writes: true,
            },
            #[cfg(feature = "pathmap-backend")]
            DictionaryBackend::PathMapUtf8 => BackendCapabilities {
                key_unit: BackendKeyUnit::Byte,
                query: BackendQuerySemantics::ExactTerm,
                updates: BackendUpdateMode::InsertRemove,
                sync_strategy: SyncStrategy::InternalSync,
                lock_free_reads: true,
                lock_free_writes: true,
            },
            DictionaryBackend::DoubleArrayTrie => BackendCapabilities {
                key_unit: BackendKeyUnit::Byte,
                query: BackendQuerySemantics::ExactTerm,
                updates: BackendUpdateMode::Immutable,
                sync_strategy: SyncStrategy::Persistent,
                lock_free_reads: true,
                lock_free_writes: false,
            },
            DictionaryBackend::DoubleArrayTrieChar => BackendCapabilities {
                key_unit: BackendKeyUnit::Char,
                query: BackendQuerySemantics::ExactTerm,
                updates: BackendUpdateMode::Immutable,
                sync_strategy: SyncStrategy::Persistent,
                lock_free_reads: true,
                lock_free_writes: false,
            },
            DictionaryBackend::DoubleArrayTrieUtf8 => BackendCapabilities {
                key_unit: BackendKeyUnit::Byte,
                query: BackendQuerySemantics::ExactTerm,
                updates: BackendUpdateMode::Immutable,
                sync_strategy: SyncStrategy::Persistent,
                lock_free_reads: true,
                lock_free_writes: false,
            },
            DictionaryBackend::DynamicDawg => BackendCapabilities {
                key_unit: BackendKeyUnit::Byte,
                query: BackendQuerySemantics::ExactTerm,
                updates: BackendUpdateMode::InsertRemove,
                sync_strategy: SyncStrategy::InternalSync,
                lock_free_reads: true,
                lock_free_writes: true,
            },
            DictionaryBackend::DynamicDawgChar => BackendCapabilities {
                key_unit: BackendKeyUnit::Char,
                query: BackendQuerySemantics::ExactTerm,
                updates: BackendUpdateMode::InsertRemove,
                sync_strategy: SyncStrategy::InternalSync,
                lock_free_reads: true,
                lock_free_writes: true,
            },
            DictionaryBackend::DynamicDawgUtf8 => BackendCapabilities {
                key_unit: BackendKeyUnit::Byte,
                query: BackendQuerySemantics::ExactTerm,
                updates: BackendUpdateMode::InsertRemove,
                sync_strategy: SyncStrategy::InternalSync,
                lock_free_reads: true,
                lock_free_writes: true,
            },
            DictionaryBackend::DynamicDawgU64 => BackendCapabilities {
                key_unit: BackendKeyUnit::U64,
                query: BackendQuerySemantics::ExactTerm,
                updates: BackendUpdateMode::InsertRemove,
                sync_strategy: SyncStrategy::InternalSync,
                lock_free_reads: true,
                lock_free_writes: true,
            },
            DictionaryBackend::SuffixAutomaton => BackendCapabilities {
                key_unit: BackendKeyUnit::Byte,
                query: BackendQuerySemantics::Substring,
                updates: BackendUpdateMode::InsertRemove,
                sync_strategy: SyncStrategy::InternalSync,
                lock_free_reads: true,
                lock_free_writes: true,
            },
            DictionaryBackend::SuffixAutomatonChar => BackendCapabilities {
                key_unit: BackendKeyUnit::Char,
                query: BackendQuerySemantics::Substring,
                updates: BackendUpdateMode::InsertRemove,
                sync_strategy: SyncStrategy::InternalSync,
                lock_free_reads: true,
                lock_free_writes: true,
            },
            DictionaryBackend::Scdawg => BackendCapabilities {
                key_unit: BackendKeyUnit::Byte,
                query: BackendQuerySemantics::Substring,
                updates: BackendUpdateMode::InsertOnly,
                sync_strategy: SyncStrategy::InternalSync,
                lock_free_reads: true,
                lock_free_writes: true,
            },
            DictionaryBackend::ScdawgChar => BackendCapabilities {
                key_unit: BackendKeyUnit::Char,
                query: BackendQuerySemantics::Substring,
                updates: BackendUpdateMode::InsertOnly,
                sync_strategy: SyncStrategy::InternalSync,
                lock_free_reads: true,
                lock_free_writes: true,
            },
        }
    }
}

impl std::fmt::Display for DictionaryBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(feature = "pathmap-backend")]
            DictionaryBackend::PathMap => write!(f, "PathMap"),
            #[cfg(feature = "pathmap-backend")]
            DictionaryBackend::PathMapChar => write!(f, "PathMapChar"),
            #[cfg(feature = "pathmap-backend")]
            DictionaryBackend::PathMapUtf8 => write!(f, "PathMapUtf8"),
            DictionaryBackend::DoubleArrayTrie => write!(f, "DoubleArrayTrie"),
            DictionaryBackend::DoubleArrayTrieChar => write!(f, "DoubleArrayTrieChar"),
            DictionaryBackend::DoubleArrayTrieUtf8 => write!(f, "DoubleArrayTrieUtf8"),
            DictionaryBackend::DynamicDawg => write!(f, "DynamicDAWG"),
            DictionaryBackend::DynamicDawgChar => write!(f, "DynamicDAWGChar"),
            DictionaryBackend::DynamicDawgUtf8 => write!(f, "DynamicDAWGUtf8"),
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
    #[cfg(feature = "pathmap-backend")]
    PathMapUtf8(PathMapDictionaryUtf8),
    DoubleArrayTrie(DoubleArrayTrie),
    DoubleArrayTrieChar(DoubleArrayTrieChar),
    DoubleArrayTrieUtf8(DoubleArrayTrieUtf8),
    DynamicDawg(DynamicDawg),
    DynamicDawgChar(DynamicDawgChar),
    DynamicDawgUtf8(DynamicDawgUtf8),
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
            #[cfg(feature = "pathmap-backend")]
            DictionaryContainer::PathMapUtf8(_) => DictionaryBackend::PathMapUtf8,
            DictionaryContainer::DoubleArrayTrie(_) => DictionaryBackend::DoubleArrayTrie,
            DictionaryContainer::DoubleArrayTrieChar(_) => DictionaryBackend::DoubleArrayTrieChar,
            DictionaryContainer::DoubleArrayTrieUtf8(_) => DictionaryBackend::DoubleArrayTrieUtf8,
            DictionaryContainer::DynamicDawg(_) => DictionaryBackend::DynamicDawg,
            DictionaryContainer::DynamicDawgChar(_) => DictionaryBackend::DynamicDawgChar,
            DictionaryContainer::DynamicDawgUtf8(_) => DictionaryBackend::DynamicDawgUtf8,
            DictionaryContainer::DynamicDawgU64(_) => DictionaryBackend::DynamicDawgU64,
            DictionaryContainer::SuffixAutomaton(_) => DictionaryBackend::SuffixAutomaton,
            DictionaryContainer::SuffixAutomatonChar(_) => DictionaryBackend::SuffixAutomatonChar,
            DictionaryContainer::Scdawg(_) => DictionaryBackend::Scdawg,
            DictionaryContainer::ScdawgChar(_) => DictionaryBackend::ScdawgChar,
        }
    }

    /// Return canonical logical-profile metadata for this instance.
    #[inline]
    pub fn profile_descriptor(&self) -> BackendProfileDescriptor {
        self.backend().profile_descriptor()
    }

    /// Get the number of terms in the dictionary.
    pub fn len(&self) -> Option<usize> {
        match self {
            #[cfg(feature = "pathmap-backend")]
            DictionaryContainer::PathMap(d) => d.len(),
            #[cfg(feature = "pathmap-backend")]
            DictionaryContainer::PathMapChar(d) => d.len(),
            #[cfg(feature = "pathmap-backend")]
            DictionaryContainer::PathMapUtf8(d) => Some(d.term_count()),
            DictionaryContainer::DoubleArrayTrie(d) => d.len(),
            DictionaryContainer::DoubleArrayTrieChar(d) => d.len(),
            DictionaryContainer::DoubleArrayTrieUtf8(d) => Some(d.term_count()),
            DictionaryContainer::DynamicDawg(d) => d.len(),
            DictionaryContainer::DynamicDawgChar(d) => d.len(),
            DictionaryContainer::DynamicDawgUtf8(d) => Some(d.term_count()),
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
            #[cfg(feature = "pathmap-backend")]
            DictionaryContainer::PathMapUtf8(d) => d.contains(term),
            DictionaryContainer::DoubleArrayTrie(d) => d.contains(term),
            DictionaryContainer::DoubleArrayTrieChar(d) => d.contains(term),
            DictionaryContainer::DoubleArrayTrieUtf8(d) => d.contains(term),
            DictionaryContainer::DynamicDawg(d) => d.contains(term),
            DictionaryContainer::DynamicDawgChar(d) => d.contains(term),
            DictionaryContainer::DynamicDawgUtf8(d) => d.contains(term),
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
    /// ```rust,no_run
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
            #[cfg(feature = "pathmap-backend")]
            DictionaryBackend::PathMapUtf8 => {
                DictionaryContainer::PathMapUtf8(PathMapDictionaryUtf8::from_terms(terms))
            }
            DictionaryBackend::DoubleArrayTrie => {
                DictionaryContainer::DoubleArrayTrie(DoubleArrayTrie::from_terms(terms))
            }
            DictionaryBackend::DoubleArrayTrieChar => {
                DictionaryContainer::DoubleArrayTrieChar(DoubleArrayTrieChar::from_terms(terms))
            }
            DictionaryBackend::DoubleArrayTrieUtf8 => {
                DictionaryContainer::DoubleArrayTrieUtf8(DoubleArrayTrieUtf8::from_terms(terms))
            }
            DictionaryBackend::DynamicDawg => {
                DictionaryContainer::DynamicDawg(DynamicDawg::from_terms(terms))
            }
            DictionaryBackend::DynamicDawgChar => {
                DictionaryContainer::DynamicDawgChar(DynamicDawgChar::from_terms(terms))
            }
            DictionaryBackend::DynamicDawgUtf8 => {
                DictionaryContainer::DynamicDawgUtf8(DynamicDawgUtf8::from_terms(terms))
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
            DictionaryBackend::Scdawg => DictionaryContainer::Scdawg(Scdawg::from_terms(terms)),
            DictionaryBackend::ScdawgChar => {
                DictionaryContainer::ScdawgChar(ScdawgChar::from_terms(terms))
            }
        }
    }

    /// Create an empty dictionary with the specified backend.
    ///
    /// # Example
    ///
    /// ```rust,no_run
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
            #[cfg(feature = "pathmap-backend")]
            DictionaryBackend::PathMapUtf8 => {
                DictionaryContainer::PathMapUtf8(PathMapDictionaryUtf8::new())
            }
            DictionaryBackend::DoubleArrayTrie => {
                DictionaryContainer::DoubleArrayTrie(DoubleArrayTrie::new())
            }
            DictionaryBackend::DoubleArrayTrieChar => {
                // DoubleArrayTrieChar uses `empty()` instead of `new()`.
                DictionaryContainer::DoubleArrayTrieChar(DoubleArrayTrieChar::empty())
            }
            DictionaryBackend::DoubleArrayTrieUtf8 => {
                DictionaryContainer::DoubleArrayTrieUtf8(DoubleArrayTrieUtf8::new())
            }
            DictionaryBackend::DynamicDawg => DictionaryContainer::DynamicDawg(DynamicDawg::new()),
            DictionaryBackend::DynamicDawgChar => {
                DictionaryContainer::DynamicDawgChar(DynamicDawgChar::new())
            }
            DictionaryBackend::DynamicDawgUtf8 => {
                DictionaryContainer::DynamicDawgUtf8(DynamicDawgUtf8::new())
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
            #[cfg(feature = "pathmap-backend")]
            DictionaryBackend::PathMapUtf8,
            DictionaryBackend::DoubleArrayTrie,
            DictionaryBackend::DoubleArrayTrieChar,
            DictionaryBackend::DoubleArrayTrieUtf8,
            DictionaryBackend::DynamicDawg,
            DictionaryBackend::DynamicDawgChar,
            DictionaryBackend::DynamicDawgUtf8,
            DictionaryBackend::DynamicDawgU64,
            DictionaryBackend::SuffixAutomaton,
            DictionaryBackend::SuffixAutomatonChar,
            DictionaryBackend::Scdawg,
            DictionaryBackend::ScdawgChar,
        ]
    }

    /// Machine-readable characteristics for a backend.
    pub fn backend_capabilities(backend: DictionaryBackend) -> BackendCapabilities {
        backend.capabilities()
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
            #[cfg(feature = "pathmap-backend")]
            DictionaryBackend::PathMapUtf8 => {
                "PathMap-based UTF-8 byte trie with validated logical Unicode terms."
            }
            DictionaryBackend::DoubleArrayTrie => {
                "Byte-keyed double-array trie. O(1) transitions, excellent cache locality, \
                 read-mostly. Best for static dictionaries."
            }
            DictionaryBackend::DoubleArrayTrieChar => {
                "Character-keyed double-array trie. Unicode-aware variant of DoubleArrayTrie."
            }
            DictionaryBackend::DoubleArrayTrieUtf8 => {
                "UTF-8 byte-backed double-array trie with validated logical Unicode terms."
            }
            DictionaryBackend::DynamicDawg => {
                "Byte-keyed dynamic DAWG. Space-efficient with full dynamic modification \
                 support. Best for evolving dictionaries."
            }
            DictionaryBackend::DynamicDawgChar => {
                "Character-keyed dynamic DAWG. Unicode-aware variant of DynamicDawg."
            }
            DictionaryBackend::DynamicDawgUtf8 => {
                "UTF-8 byte-backed dynamic DAWG with validated logical Unicode terms."
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
        assert_eq!(dict.profile_descriptor().kind, ProfileKind::Bytes);
    }

    #[test]
    fn test_factory_unicode_backends() {
        let unicode_terms = vec!["café", "naïve", "日本語"];

        for backend in [
            DictionaryBackend::DoubleArrayTrieChar,
            DictionaryBackend::DoubleArrayTrieUtf8,
            DictionaryBackend::DynamicDawgChar,
            DictionaryBackend::DynamicDawgUtf8,
            DictionaryBackend::SuffixAutomatonChar,
            DictionaryBackend::ScdawgChar,
        ] {
            let dict = DictionaryFactory::create(backend, unicode_terms.clone());
            assert!(dict.contains("café"), "{backend} should contain 'café'");
            assert!(dict.contains("naïve"), "{backend} should contain 'naïve'");
            assert!(dict.contains("日本語"), "{backend} should contain '日本語'");
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
        // 14 backends total with PathMap enabled: legacy backends plus two
        // byte-backed UTF-8 profile adapters; PathMap variants are feature-gated.
        // PathMap and PathMapChar gated behind feature.
        #[cfg(feature = "pathmap-backend")]
        assert_eq!(backends.len(), 14);
        #[cfg(not(feature = "pathmap-backend"))]
        assert_eq!(backends.len(), 11);
        assert!(backends.contains(&DictionaryBackend::DoubleArrayTrie));
        assert!(backends.contains(&DictionaryBackend::DynamicDawg));
        assert!(backends.contains(&DictionaryBackend::DynamicDawgChar));
        assert!(backends.contains(&DictionaryBackend::SuffixAutomaton));
        assert!(backends.contains(&DictionaryBackend::Scdawg));
        assert!(backends.contains(&DictionaryBackend::DoubleArrayTrieUtf8));
        assert!(backends.contains(&DictionaryBackend::DynamicDawgUtf8));
    }

    #[test]
    fn test_backend_descriptions() {
        for backend in DictionaryFactory::available_backends() {
            let desc = DictionaryFactory::backend_description(backend);
            assert!(!desc.is_empty(), "{backend} has empty description");
        }
    }

    #[test]
    fn test_backend_capabilities_cover_selection_axes() {
        for backend in DictionaryFactory::available_backends() {
            let caps = DictionaryFactory::backend_capabilities(backend);
            assert_eq!(caps, backend.capabilities(), "{backend}");
            assert!(
                caps.is_fully_lock_free_for_supported_operations(),
                "{backend} should be lock-free for advertised operations"
            );

            match backend {
                DictionaryBackend::DoubleArrayTrie | DictionaryBackend::DoubleArrayTrieChar => {
                    assert_eq!(caps.updates, BackendUpdateMode::Immutable, "{backend}");
                    assert_eq!(caps.sync_strategy, SyncStrategy::Persistent, "{backend}");
                }
                DictionaryBackend::SuffixAutomaton
                | DictionaryBackend::SuffixAutomatonChar
                | DictionaryBackend::Scdawg
                | DictionaryBackend::ScdawgChar => {
                    assert!(
                        caps.supports_substring_search(),
                        "{backend} should advertise substring semantics"
                    );
                }
                _ => {
                    assert!(
                        !caps.supports_substring_search(),
                        "{backend} should advertise exact-term semantics"
                    );
                }
            }
        }
    }

    #[test]
    fn test_backend_capability_key_units() {
        assert_eq!(
            DictionaryBackend::DynamicDawg.capabilities().key_unit,
            BackendKeyUnit::Byte
        );
        assert_eq!(
            DictionaryBackend::DynamicDawgChar.capabilities().key_unit,
            BackendKeyUnit::Char
        );
        assert!(DictionaryBackend::DynamicDawgChar
            .capabilities()
            .is_unicode());
        assert_eq!(
            DictionaryBackend::DynamicDawgU64.capabilities().key_unit,
            BackendKeyUnit::U64
        );
    }

    #[test]
    fn backend_profile_descriptor_uses_canonical_identity() {
        let bytes = DictionaryBackend::DynamicDawg.profile_descriptor();
        assert_eq!(bytes.kind, ProfileKind::Bytes);
        assert_eq!(bytes.identity, ProfileKind::Bytes.identity());
        assert_eq!(bytes.width_bytes, Some(1));

        let chars = DictionaryBackend::DynamicDawgChar.profile_descriptor();
        assert_eq!(chars.kind, ProfileKind::UnicodeScalar);
        assert_eq!(chars.identity.name, "unicode-scalar");
        assert_eq!(chars.width_bytes, Some(4));

        let words = DictionaryBackend::DynamicDawgU64.profile_descriptor();
        assert_eq!(words.kind, ProfileKind::U64);
        assert_eq!(words.identity.version, 1);
        assert_eq!(words.width_bytes, Some(8));

        let utf8 = DictionaryBackend::DynamicDawgUtf8.profile_descriptor();
        assert_eq!(utf8.kind, ProfileKind::Utf8);
        assert_eq!(utf8.identity.name, "utf8");
        assert_eq!(utf8.width_bytes, None);
        #[cfg(feature = "pathmap-backend")]
        assert_eq!(
            DictionaryBackend::PathMapUtf8.profile_descriptor().kind,
            ProfileKind::Utf8
        );
    }

    #[test]
    fn test_all_backends_work() {
        let terms = vec!["apple", "banana", "cherry"];

        for backend in DictionaryFactory::available_backends() {
            let dict = DictionaryFactory::create(backend, terms.clone());
            assert!(dict.contains("apple"), "{backend} should contain 'apple'");
            assert!(dict.contains("banana"), "{backend} should contain 'banana'");
            assert!(dict.contains("cherry"), "{backend} should contain 'cherry'");
        }
    }
}
