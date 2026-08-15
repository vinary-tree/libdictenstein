//! Stable binding resources owned by libdictenstein.
//!
//! This module is the producer half of the `vt.dictionary.v1` contract.
//! Concrete dictionaries and their CRUD APIs remain in this crate while
//! consumers retain a small project-neutral resource. Capturing a query
//! revision clones an immutable root in O(1); nodes are assigned ABI-local
//! identifiers only when a consumer traverses their incoming edge.

use crate::double_array_trie::char::DoubleArrayTrieChar;
use crate::double_array_trie::DoubleArrayTrie;
use crate::dynamic_dawg::char::DynamicDawgChar;
use crate::dynamic_dawg::u64::DynamicDawgU64;
use crate::dynamic_dawg::DynamicDawg;
use crate::scdawg::char::ScdawgChar;
use crate::scdawg::Scdawg;
use crate::{Dictionary, DictionaryNode, MappedDictionaryNode};
use std::ffi::c_void;
#[cfg(feature = "persistent-artrie")]
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};
use vinary_tree_interop::{
    dictionary_flags, VtDictionaryEdge, VtDictionaryVTable, VtDictionaryVisitVTable, VtInterfaceId,
    VtOptionalU64, VtResource, VtResourceVTable, VtStatus, VtUnitDomain, VtValueDomain,
    VT_ABI_VERSION, VT_DICTIONARY_INTERFACE_ID, VT_DICTIONARY_INTERFACE_VERSION,
    VT_DICTIONARY_VISIT_INTERFACE_ID, VT_DICTIONARY_VISIT_INTERFACE_VERSION,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[cfg_attr(
    feature = "persistent-artrie",
    derive(serde::Serialize, serde::Deserialize)
)]
struct BindingValue {
    value: u64,
    has_value: bool,
}

impl crate::DictionaryValue for BindingValue {}

impl BindingValue {
    fn present(value: u64) -> Self {
        Self {
            value,
            has_value: true,
        }
    }

    fn from_option(value: Option<u64>) -> Self {
        Self {
            value: value.unwrap_or_default(),
            has_value: value.is_some(),
        }
    }

    fn into_option(self) -> Option<u64> {
        self.has_value.then_some(self.value)
    }
}

/// Concrete unit domain selected for a binding-owned dictionary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum BindingUnitDomain {
    /// Arbitrary byte sequences.
    Byte = 1,
    /// UTF-8 strings traversed as Unicode scalar values.
    UnicodeScalar = 2,
    /// Arbitrary unsigned 64-bit token sequences.
    U64 = 3,
}

impl From<BindingUnitDomain> for VtUnitDomain {
    fn from(value: BindingUnitDomain) -> Self {
        match value {
            BindingUnitDomain::Byte => Self::Byte,
            BindingUnitDomain::UnicodeScalar => Self::UnicodeScalar,
            BindingUnitDomain::U64 => Self::U64,
        }
    }
}

/// Error returned by the safe binding-owned dictionary API.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BindingError {
    /// An operation used a term representation incompatible with the dictionary.
    DomainMismatch,
    /// A Unicode dictionary received malformed UTF-8.
    InvalidUtf8,
    /// A requested backend operation is not available for this dictionary kind.
    Unsupported,
    /// A persistent dictionary operation failed.
    Io(String),
}

impl std::fmt::Display for BindingError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DomainMismatch => formatter.write_str("dictionary unit domain mismatch"),
            Self::InvalidUtf8 => formatter.write_str("dictionary term is not valid UTF-8"),
            Self::Unsupported => formatter.write_str("dictionary operation is unsupported"),
            Self::Io(message) => write!(
                formatter,
                "persistent dictionary operation failed: {message}"
            ),
        }
    }
}

impl std::error::Error for BindingError {}

enum DynamicBackend {
    Byte(DynamicDawg<BindingValue>),
    Unicode(DynamicDawgChar<BindingValue>),
    U64(DynamicDawgU64<BindingValue>),
}

impl DynamicBackend {
    fn new(domain: BindingUnitDomain) -> Self {
        match domain {
            BindingUnitDomain::Byte => Self::Byte(DynamicDawg::new()),
            BindingUnitDomain::UnicodeScalar => Self::Unicode(DynamicDawgChar::new()),
            BindingUnitDomain::U64 => Self::U64(DynamicDawgU64::new()),
        }
    }

    fn domain(&self) -> BindingUnitDomain {
        match self {
            Self::Byte(_) => BindingUnitDomain::Byte,
            Self::Unicode(_) => BindingUnitDomain::UnicodeScalar,
            Self::U64(_) => BindingUnitDomain::U64,
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::Byte(dictionary) => dictionary.term_count(),
            Self::Unicode(dictionary) => dictionary.term_count(),
            Self::U64(dictionary) => dictionary.term_count(),
        }
    }

    fn snapshot(&self) -> Arc<dyn SnapshotOps> {
        // Root and length MUST come from one published revision: separate
        // `root()` + `len()` calls perform two independent lock-free version
        // loads, and a writer between them produces a torn capture (finding
        // LDICT-B4, reproduced at ~2% of captures under churn).
        match self {
            Self::Byte(dictionary) => {
                let (root, term_count) = dictionary.root_with_term_count();
                Arc::new(TraversalSnapshot::new(
                    root,
                    Some(term_count),
                    VtUnitDomain::Byte,
                    false,
                ))
            }
            Self::Unicode(dictionary) => {
                let (root, term_count) = dictionary.root_with_term_count();
                Arc::new(TraversalSnapshot::new(
                    root,
                    Some(term_count),
                    VtUnitDomain::UnicodeScalar,
                    false,
                ))
            }
            Self::U64(dictionary) => {
                let (root, term_count) = dictionary.root_with_term_count();
                Arc::new(TraversalSnapshot::new(
                    root,
                    Some(term_count),
                    VtUnitDomain::U64,
                    false,
                ))
            }
        }
    }
}

struct SharedDictionary {
    backend: RwLock<DynamicBackend>,
}

#[cfg(feature = "persistent-artrie")]
enum PersistentBackend {
    Byte(crate::persistent_artrie::PersistentARTrie<BindingValue>),
    Unicode(crate::persistent_artrie::char::PersistentARTrieChar<BindingValue>),
    U64(crate::persistent_artrie::u64::PersistentARTrieU64<BindingValue>),
    Vocab(crate::persistent_artrie::vocab::PersistentVocabARTrie),
}

#[cfg(feature = "persistent-artrie")]
impl PersistentBackend {
    fn domain(&self) -> BindingUnitDomain {
        match self {
            Self::Byte(_) => BindingUnitDomain::Byte,
            Self::Unicode(_) | Self::Vocab(_) => BindingUnitDomain::UnicodeScalar,
            Self::U64(_) => BindingUnitDomain::U64,
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::Byte(dictionary) => dictionary.overlay_len(),
            Self::Unicode(dictionary) => dictionary.len(),
            Self::U64(dictionary) => dictionary.term_count(),
            Self::Vocab(dictionary) => dictionary.len(),
        }
    }

    fn snapshot(&self) -> Arc<dyn SnapshotOps> {
        // Coherence over convenience (finding LDICT-B4, persistent family):
        // the overlay publishes its root and its term counters in SEPARATE
        // atomic steps, so any (root, count) pair assembled from two loads
        // can tear under a concurrent writer (reproduced at ~0.1% of
        // captures under churn), and a binding-side retry protocol is
        // provably unsound against post-flip counter updates. The interop
        // contract has the honest affordance for exactly this situation:
        // `len` may report out_known = 0. Persistent captures therefore pin
        // the ROOT ONLY — coherent by construction, still O(1) — and report
        // the count as not-cheaply-known. Restoring out_known = 1 for this
        // family requires a coherent (root, count) publication inside the
        // overlay flip itself (persistent-core design work, tracked in
        // docs/bindings/FINDINGS_LEDGER.md LDICT-B4).
        match self {
            Self::Byte(dictionary) => Arc::new(TraversalSnapshot::new(
                dictionary.root(),
                None,
                VtUnitDomain::Byte,
                false,
            )),
            Self::Unicode(dictionary) => Arc::new(TraversalSnapshot::new(
                dictionary.root(),
                None,
                VtUnitDomain::UnicodeScalar,
                false,
            )),
            Self::U64(dictionary) => Arc::new(TraversalSnapshot::new(
                dictionary.root(),
                None,
                VtUnitDomain::U64,
                false,
            )),
            Self::Vocab(dictionary) => Arc::new(TraversalSnapshot::new(
                dictionary.root(),
                None,
                VtUnitDomain::UnicodeScalar,
                false,
            )),
        }
    }
}

/// Disk-backed persistent ARTrie family exposed to foreign-language bindings.
#[cfg(feature = "persistent-artrie")]
#[derive(Clone)]
pub struct PersistentARTrieBinding {
    shared: Arc<PersistentBackend>,
}

#[cfg(feature = "persistent-artrie")]
impl PersistentARTrieBinding {
    /// Create a new byte, Unicode-scalar, or native-u64 persistent trie.
    pub fn create(path: impl AsRef<Path>, domain: BindingUnitDomain) -> Result<Self, BindingError> {
        let shared = match domain {
            BindingUnitDomain::Byte => Arc::new(PersistentBackend::Byte(
                crate::persistent_artrie::PersistentARTrie::create(path).map_err(io_error)?,
            )),
            BindingUnitDomain::UnicodeScalar => Arc::new(PersistentBackend::Unicode(
                crate::persistent_artrie::char::PersistentARTrieChar::create(path)
                    .map_err(io_error)?,
            )),
            BindingUnitDomain::U64 => Arc::new(PersistentBackend::U64(
                crate::persistent_artrie::u64::PersistentARTrieU64::create(path)
                    .map_err(io_error)?,
            )),
        };
        Ok(Self { shared })
    }

    /// Open an existing byte, Unicode-scalar, or native-u64 persistent trie.
    pub fn open(path: impl AsRef<Path>, domain: BindingUnitDomain) -> Result<Self, BindingError> {
        let shared = match domain {
            BindingUnitDomain::Byte => Arc::new(PersistentBackend::Byte(
                crate::persistent_artrie::PersistentARTrie::open(path).map_err(io_error)?,
            )),
            BindingUnitDomain::UnicodeScalar => Arc::new(PersistentBackend::Unicode(
                crate::persistent_artrie::char::PersistentARTrieChar::open(path)
                    .map_err(io_error)?,
            )),
            BindingUnitDomain::U64 => Arc::new(PersistentBackend::U64(
                crate::persistent_artrie::u64::PersistentARTrieU64::open(path).map_err(io_error)?,
            )),
        };
        Ok(Self { shared })
    }

    /// Create a persistent bidirectional term/index vocabulary.
    pub fn create_vocab(path: impl AsRef<Path>) -> Result<Self, BindingError> {
        Ok(Self {
            shared: Arc::new(PersistentBackend::Vocab(
                crate::persistent_artrie::vocab::PersistentVocabARTrie::create(path)
                    .map_err(io_error)?,
            )),
        })
    }

    /// Open an existing persistent bidirectional term/index vocabulary.
    pub fn open_vocab(path: impl AsRef<Path>) -> Result<Self, BindingError> {
        Ok(Self {
            shared: Arc::new(PersistentBackend::Vocab(
                crate::persistent_artrie::vocab::PersistentVocabARTrie::open(path)
                    .map_err(io_error)?,
            )),
        })
    }

    /// Unit domain used by dictionary transitions.
    pub fn domain(&self) -> BindingUnitDomain {
        self.shared.domain()
    }

    /// Number of visible terms in the current revision.
    pub fn len(&self) -> usize {
        self.shared.len()
    }

    /// Whether the current revision has no visible terms.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether this is the bidirectional vocabulary ARTrie variant.
    pub fn is_vocab(&self) -> bool {
        matches!(self.shared.as_ref(), PersistentBackend::Vocab(_))
    }

    /// Insert/update a byte or Unicode term and optional u64 metadata.
    pub fn insert_text(&self, term: &[u8], value: Option<u64>) -> Result<bool, BindingError> {
        match self.shared.as_ref() {
            PersistentBackend::Byte(dictionary) => dictionary
                .upsert_bytes(term, BindingValue::from_option(value))
                .map_err(io_error),
            PersistentBackend::Unicode(dictionary) => {
                let term = std::str::from_utf8(term).map_err(|_| BindingError::InvalidUtf8)?;
                dictionary
                    .insert_with_value(term, BindingValue::from_option(value))
                    .map_err(io_error)
            }
            PersistentBackend::Vocab(dictionary) => {
                let term = std::str::from_utf8(term).map_err(|_| BindingError::InvalidUtf8)?;
                if let Some(index) = value {
                    dictionary.insert_with_index(term, index).map_err(io_error)
                } else {
                    let existed = dictionary.contains(term);
                    dictionary.insert(term).map_err(io_error)?;
                    Ok(!existed)
                }
            }
            PersistentBackend::U64(_) => Err(BindingError::DomainMismatch),
        }
    }

    /// Remove a byte or Unicode term where the selected variant supports removal.
    pub fn remove_text(&self, term: &[u8]) -> Result<bool, BindingError> {
        match self.shared.as_ref() {
            PersistentBackend::Byte(dictionary) => {
                dictionary.remove_cas_durable(term).map_err(io_error)
            }
            PersistentBackend::Unicode(dictionary) => {
                let term = std::str::from_utf8(term).map_err(|_| BindingError::InvalidUtf8)?;
                dictionary.remove(term).map_err(io_error)
            }
            PersistentBackend::Vocab(_) => Err(BindingError::Unsupported),
            PersistentBackend::U64(_) => Err(BindingError::DomainMismatch),
        }
    }

    /// Test byte or Unicode exact membership.
    pub fn contains_text(&self, term: &[u8]) -> Result<bool, BindingError> {
        match self.shared.as_ref() {
            PersistentBackend::Byte(dictionary) => Ok(dictionary.contains_bytes(term)),
            PersistentBackend::Unicode(dictionary) => {
                let term = std::str::from_utf8(term).map_err(|_| BindingError::InvalidUtf8)?;
                Ok(dictionary.contains(term))
            }
            PersistentBackend::Vocab(dictionary) => {
                let term = std::str::from_utf8(term).map_err(|_| BindingError::InvalidUtf8)?;
                Ok(dictionary.contains(term))
            }
            PersistentBackend::U64(_) => Err(BindingError::DomainMismatch),
        }
    }

    /// Read byte or Unicode metadata, preserving absent versus valueless terms.
    pub fn value_text(&self, term: &[u8]) -> Result<Option<Option<u64>>, BindingError> {
        match self.shared.as_ref() {
            PersistentBackend::Byte(dictionary) => Ok(dictionary
                .get_value_bytes(term)
                .map(BindingValue::into_option)),
            PersistentBackend::Unicode(dictionary) => {
                let term = std::str::from_utf8(term).map_err(|_| BindingError::InvalidUtf8)?;
                Ok(dictionary.get_value(term).map(BindingValue::into_option))
            }
            PersistentBackend::Vocab(dictionary) => {
                let term = std::str::from_utf8(term).map_err(|_| BindingError::InvalidUtf8)?;
                Ok(dictionary.get_index(term).map(Some))
            }
            PersistentBackend::U64(_) => Err(BindingError::DomainMismatch),
        }
    }

    /// Insert/update a native-u64 term.
    ///
    /// Engine write failures propagate as I/O errors exactly like the byte
    /// and Unicode profiles; the infallible `insert_sequence_with_value`
    /// wrapper (which logs and returns `false`) is deliberately not used
    /// here — the ABI must report `IO_ERROR`, never a silent no-op `OK`.
    pub fn insert_u64(&self, term: &[u64], value: Option<u64>) -> Result<bool, BindingError> {
        match self.shared.as_ref() {
            PersistentBackend::U64(dictionary) => dictionary
                .try_insert_sequence_with_value(term, BindingValue::from_option(value))
                .map_err(io_error),
            _ => Err(BindingError::DomainMismatch),
        }
    }

    /// Remove a native-u64 term.
    ///
    /// Engine write failures propagate as I/O errors (see `insert_u64`).
    pub fn remove_u64(&self, term: &[u64]) -> Result<bool, BindingError> {
        match self.shared.as_ref() {
            PersistentBackend::U64(dictionary) => {
                dictionary.try_remove_sequence(term).map_err(io_error)
            }
            _ => Err(BindingError::DomainMismatch),
        }
    }

    /// Test native-u64 exact membership.
    pub fn contains_u64(&self, term: &[u64]) -> Result<bool, BindingError> {
        match self.shared.as_ref() {
            PersistentBackend::U64(dictionary) => Ok(dictionary.contains_sequence(term)),
            _ => Err(BindingError::DomainMismatch),
        }
    }

    /// Read native-u64 metadata, preserving absent versus valueless terms.
    pub fn value_u64(&self, term: &[u64]) -> Result<Option<Option<u64>>, BindingError> {
        match self.shared.as_ref() {
            PersistentBackend::U64(dictionary) => Ok(dictionary
                .get_sequence_value(term)
                .map(BindingValue::into_option)),
            _ => Err(BindingError::DomainMismatch),
        }
    }

    /// Atomically checkpoint the current revision to disk.
    pub fn checkpoint(&self) -> Result<(), BindingError> {
        match self.shared.as_ref() {
            PersistentBackend::Byte(dictionary) => dictionary.checkpoint().map_err(io_error),
            PersistentBackend::Unicode(dictionary) => dictionary.checkpoint().map_err(io_error),
            PersistentBackend::U64(dictionary) => dictionary.checkpoint().map_err(io_error),
            PersistentBackend::Vocab(dictionary) => dictionary.checkpoint().map_err(io_error),
        }
    }

    /// Look up a vocabulary term by its stable index.
    pub fn vocab_term(&self, index: u64) -> Result<Option<String>, BindingError> {
        match self.shared.as_ref() {
            PersistentBackend::Vocab(dictionary) => Ok(dictionary.get_term(index)),
            _ => Err(BindingError::Unsupported),
        }
    }

    /// Borrow a retained interoperable dictionary resource.
    pub fn resource(&self) -> OwnedDictionaryResource {
        OwnedDictionaryResource::new(ResourcePayload::Persistent(Arc::clone(&self.shared)))
    }
}

#[cfg(feature = "persistent-artrie")]
fn io_error(error: impl std::fmt::Display) -> BindingError {
    BindingError::Io(error.to_string())
}

enum SecondaryBackend {
    DoubleArrayByte(DoubleArrayTrie<BindingValue>),
    DoubleArrayUnicode(DoubleArrayTrieChar<BindingValue>),
    ScdawgByte(Scdawg<BindingValue>),
    ScdawgUnicode(ScdawgChar<BindingValue>),
}

impl SecondaryBackend {
    fn domain(&self) -> BindingUnitDomain {
        match self {
            Self::DoubleArrayByte(_) | Self::ScdawgByte(_) => BindingUnitDomain::Byte,
            Self::DoubleArrayUnicode(_) | Self::ScdawgUnicode(_) => {
                BindingUnitDomain::UnicodeScalar
            }
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::DoubleArrayByte(dictionary) => dictionary.len().unwrap_or_default(),
            Self::DoubleArrayUnicode(dictionary) => dictionary.len().unwrap_or_default(),
            Self::ScdawgByte(dictionary) => dictionary.term_count(),
            Self::ScdawgUnicode(dictionary) => dictionary.term_count(),
        }
    }

    fn suffix(&self) -> bool {
        matches!(self, Self::ScdawgByte(_) | Self::ScdawgUnicode(_))
    }

    fn snapshot(&self) -> Arc<dyn SnapshotOps> {
        match self {
            // DoubleArrayTrie backends are immutable after construction, so
            // separate root()/len() reads cannot tear (no writer exists).
            Self::DoubleArrayByte(dictionary) => Arc::new(TraversalSnapshot::new(
                dictionary.root(),
                dictionary.len(),
                VtUnitDomain::Byte,
                false,
            )),
            Self::DoubleArrayUnicode(dictionary) => Arc::new(TraversalSnapshot::new(
                dictionary.root(),
                dictionary.len(),
                VtUnitDomain::UnicodeScalar,
                false,
            )),
            // SCDAWGs are mutable: pair the root with the count from ONE
            // published revision (finding LDICT-B4).
            Self::ScdawgByte(dictionary) => {
                let (root, term_count) = dictionary.root_with_term_count();
                Arc::new(TraversalSnapshot::new(
                    root,
                    Some(term_count),
                    VtUnitDomain::Byte,
                    true,
                ))
            }
            Self::ScdawgUnicode(dictionary) => {
                let (root, term_count) = dictionary.root_with_term_count();
                Arc::new(TraversalSnapshot::new(
                    root,
                    Some(term_count),
                    VtUnitDomain::UnicodeScalar,
                    true,
                ))
            }
        }
    }
}

/// Immutable, cache-local DoubleArrayTrie exposed to foreign-language bindings.
#[derive(Clone)]
pub struct DoubleArrayTrieBinding {
    shared: Arc<SecondaryBackend>,
}

impl DoubleArrayTrieBinding {
    /// Build a byte-transition DAT from UTF-8 terms and optional metadata.
    pub fn from_byte_terms<I, S>(entries: I) -> Self
    where
        I: IntoIterator<Item = (S, Option<u64>)>,
        S: AsRef<str>,
    {
        let dictionary = DoubleArrayTrie::from_terms_with_values(
            entries
                .into_iter()
                .map(|(term, value)| (term, BindingValue::from_option(value))),
        );
        Self {
            shared: Arc::new(SecondaryBackend::DoubleArrayByte(dictionary)),
        }
    }

    /// Build a Unicode-scalar DAT from terms and optional metadata.
    pub fn from_unicode_terms<I, S>(entries: I) -> Self
    where
        I: IntoIterator<Item = (S, Option<u64>)>,
        S: AsRef<str>,
    {
        let dictionary = DoubleArrayTrieChar::from_terms_with_values(
            entries
                .into_iter()
                .map(|(term, value)| (term, BindingValue::from_option(value))),
        );
        Self {
            shared: Arc::new(SecondaryBackend::DoubleArrayUnicode(dictionary)),
        }
    }

    /// Unit domain used by transitions.
    pub fn domain(&self) -> BindingUnitDomain {
        self.shared.domain()
    }

    /// Number of exact dictionary terms.
    pub fn len(&self) -> usize {
        self.shared.len()
    }

    /// Whether there are no exact dictionary terms.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Test exact membership.
    pub fn contains(&self, term: &str) -> bool {
        match self.shared.as_ref() {
            SecondaryBackend::DoubleArrayByte(dictionary) => dictionary.contains(term),
            SecondaryBackend::DoubleArrayUnicode(dictionary) => dictionary.contains(term),
            _ => unreachable!("DoubleArrayTrieBinding contains only DAT backends"),
        }
    }

    /// Read optional metadata while preserving absent versus valueless terms.
    pub fn value(&self, term: &str) -> Option<Option<u64>> {
        match self.shared.as_ref() {
            SecondaryBackend::DoubleArrayByte(dictionary) => {
                dictionary.get_value(term).map(BindingValue::into_option)
            }
            SecondaryBackend::DoubleArrayUnicode(dictionary) => {
                dictionary.get_value(term).map(BindingValue::into_option)
            }
            _ => unreachable!("DoubleArrayTrieBinding contains only DAT backends"),
        }
    }

    /// Borrow a retained interoperable dictionary resource.
    pub fn resource(&self) -> OwnedDictionaryResource {
        OwnedDictionaryResource::new(ResourcePayload::Secondary(Arc::clone(&self.shared)))
    }
}

/// Mutable SCDAWG binding with exact-term and substring operations.
#[derive(Clone)]
pub struct ScdawgBinding {
    shared: Arc<SecondaryBackend>,
}

impl ScdawgBinding {
    /// Construct an empty byte-transition SCDAWG.
    pub fn new_byte() -> Self {
        Self {
            shared: Arc::new(SecondaryBackend::ScdawgByte(Scdawg::new())),
        }
    }

    /// Construct an empty Unicode-scalar SCDAWG.
    pub fn new_unicode() -> Self {
        Self {
            shared: Arc::new(SecondaryBackend::ScdawgUnicode(ScdawgChar::new())),
        }
    }

    /// Unit domain used by transitions.
    pub fn domain(&self) -> BindingUnitDomain {
        self.shared.domain()
    }

    /// Number of indexed exact terms.
    pub fn len(&self) -> usize {
        self.shared.len()
    }

    /// Whether no exact terms are indexed.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Insert or update a term and optional metadata.
    pub fn insert(&self, term: &str, value: Option<u64>) -> bool {
        let value = BindingValue::from_option(value);
        match self.shared.as_ref() {
            SecondaryBackend::ScdawgByte(dictionary) => dictionary.insert_with_value(term, value),
            SecondaryBackend::ScdawgUnicode(dictionary) => {
                dictionary.insert_with_value(term, value)
            }
            _ => unreachable!("ScdawgBinding contains only SCDAWG backends"),
        }
    }

    /// Test exact-term membership.
    pub fn contains(&self, term: &str) -> bool {
        match self.shared.as_ref() {
            SecondaryBackend::ScdawgByte(dictionary) => dictionary.contains(term),
            SecondaryBackend::ScdawgUnicode(dictionary) => dictionary.contains(term),
            _ => unreachable!("ScdawgBinding contains only SCDAWG backends"),
        }
    }

    /// Test whether a pattern occurs as a substring of an indexed term.
    pub fn contains_substring(&self, pattern: &str) -> bool {
        match self.shared.as_ref() {
            SecondaryBackend::ScdawgByte(dictionary) => dictionary.contains_substring(pattern),
            SecondaryBackend::ScdawgUnicode(dictionary) => dictionary.contains_substring(pattern),
            _ => unreachable!("ScdawgBinding contains only SCDAWG backends"),
        }
    }

    /// Count occurrences of a substring across indexed terms.
    pub fn frequency(&self, pattern: &str) -> usize {
        match self.shared.as_ref() {
            SecondaryBackend::ScdawgByte(dictionary) => dictionary.freq(pattern),
            SecondaryBackend::ScdawgUnicode(dictionary) => dictionary.freq(pattern),
            _ => unreachable!("ScdawgBinding contains only SCDAWG backends"),
        }
    }

    /// Read optional metadata while preserving absent versus valueless terms.
    pub fn value(&self, term: &str) -> Option<Option<u64>> {
        match self.shared.as_ref() {
            SecondaryBackend::ScdawgByte(dictionary) => {
                dictionary.get_value(term).map(BindingValue::into_option)
            }
            SecondaryBackend::ScdawgUnicode(dictionary) => {
                dictionary.get_value(term).map(BindingValue::into_option)
            }
            _ => unreachable!("ScdawgBinding contains only SCDAWG backends"),
        }
    }

    /// Borrow a retained interoperable dictionary resource.
    pub fn resource(&self) -> OwnedDictionaryResource {
        OwnedDictionaryResource::new(ResourcePayload::Secondary(Arc::clone(&self.shared)))
    }
}

/// Mutable DynamicDAWG exposed to foreign-language bindings.
///
/// Clones share the same atomically published revisions. A resource borrowed
/// from this object remains usable after the object is dropped whenever a
/// consumer has retained it through the shared ABI.
#[derive(Clone)]
pub struct DynamicDawgBinding {
    shared: Arc<SharedDictionary>,
}

impl std::fmt::Debug for DynamicDawgBinding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DynamicDawgBinding")
            .field("domain", &self.domain())
            .field("len", &self.len())
            .finish()
    }
}

impl DynamicDawgBinding {
    /// Construct an empty DynamicDAWG for one term domain.
    pub fn new(domain: BindingUnitDomain) -> Self {
        Self {
            shared: Arc::new(SharedDictionary {
                backend: RwLock::new(DynamicBackend::new(domain)),
            }),
        }
    }

    /// Return the dictionary's immutable unit domain.
    pub fn domain(&self) -> BindingUnitDomain {
        self.shared
            .backend
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .domain()
    }

    /// Return the number of visible terms.
    pub fn len(&self) -> usize {
        self.shared
            .backend
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }

    /// Return whether the dictionary is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Insert or update a UTF-8/byte term and optional value.
    pub fn insert_text(&self, term: &[u8], value: Option<u64>) -> Result<bool, BindingError> {
        let backend = self
            .shared
            .backend
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match &*backend {
            DynamicBackend::Byte(dictionary) => {
                Ok(dictionary
                    .insert_bytes_with_optional_value(term, value.map(BindingValue::present)))
            }
            DynamicBackend::Unicode(dictionary) => {
                let term = std::str::from_utf8(term).map_err(|_| BindingError::InvalidUtf8)?;
                Ok(dictionary.insert_with_optional_value(term, value.map(BindingValue::present)))
            }
            DynamicBackend::U64(_) => Err(BindingError::DomainMismatch),
        }
    }

    /// Insert/update a complete text batch, selecting the freeze-once builder
    /// when the dictionary is empty.
    ///
    /// Ordered input is detected in linear time and skips sorting. Unordered
    /// input is stably sorted so duplicate entries retain last-value-wins
    /// semantics. Replacing the backend under the binding write lock preserves
    /// the shared resource identity; snapshots captured before the replacement
    /// keep their immutable roots.
    pub fn insert_text_batch<'a, I>(&self, entries: I) -> Result<usize, BindingError>
    where
        I: IntoIterator<Item = (&'a [u8], Option<u64>)>,
    {
        let entries: Vec<_> = entries.into_iter().collect();
        let mut backend = self
            .shared
            .backend
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        if backend.len() == 0 {
            let replacement = match &*backend {
                DynamicBackend::Byte(_) => {
                    let mut owned: Vec<_> = entries
                        .iter()
                        .map(|(term, value)| (term.to_vec(), value.map(BindingValue::present)))
                        .collect();
                    if !owned.windows(2).all(|pair| pair[0].0 <= pair[1].0) {
                        crate::causal_perf::record_batch_sort_calls(1);
                        crate::causal_perf::record_batch_sort_terms(owned.len() as u64);
                        crate::causal_perf::record_batch_sort_units(
                            owned.iter().map(|(term, _)| term.len()).sum::<usize>() as u64,
                        );
                        owned.sort_by(|left, right| left.0.cmp(&right.0));
                    }
                    DynamicBackend::Byte(DynamicDawg::from_sorted_byte_entries(owned))
                }
                DynamicBackend::Unicode(_) => {
                    let mut owned = Vec::with_capacity(entries.len());
                    for (term, value) in &entries {
                        let term = std::str::from_utf8(term)
                            .map_err(|_| BindingError::InvalidUtf8)?
                            .to_owned();
                        owned.push((term, value.map(BindingValue::present)));
                    }
                    if !owned.windows(2).all(|pair| pair[0].0 <= pair[1].0) {
                        crate::causal_perf::record_batch_sort_calls(1);
                        crate::causal_perf::record_batch_sort_terms(owned.len() as u64);
                        crate::causal_perf::record_batch_sort_units(
                            owned
                                .iter()
                                .map(|(term, _)| term.chars().count())
                                .sum::<usize>() as u64,
                        );
                        owned.sort_by(|left, right| left.0.cmp(&right.0));
                    }
                    DynamicBackend::Unicode(DynamicDawgChar::from_sorted_optional_entries(owned))
                }
                DynamicBackend::U64(_) => return Err(BindingError::DomainMismatch),
            };
            *backend = replacement;
            return Ok(backend.len());
        }
        drop(backend);

        let mut inserted = 0usize;
        for (term, value) in entries {
            inserted += usize::from(self.insert_text(term, value)?);
        }
        Ok(inserted)
    }

    /// Remove a UTF-8/byte term.
    pub fn remove_text(&self, term: &[u8]) -> Result<bool, BindingError> {
        let backend = self
            .shared
            .backend
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match &*backend {
            DynamicBackend::Byte(dictionary) => Ok(dictionary.remove_bytes(term)),
            DynamicBackend::Unicode(dictionary) => {
                let term = std::str::from_utf8(term).map_err(|_| BindingError::InvalidUtf8)?;
                Ok(dictionary.remove(term))
            }
            DynamicBackend::U64(_) => Err(BindingError::DomainMismatch),
        }
    }

    /// Test membership for a UTF-8/byte term.
    pub fn contains_text(&self, term: &[u8]) -> Result<bool, BindingError> {
        let backend = self
            .shared
            .backend
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match &*backend {
            DynamicBackend::Byte(dictionary) => Ok(dictionary.contains_bytes(term)),
            DynamicBackend::Unicode(dictionary) => {
                let term = std::str::from_utf8(term).map_err(|_| BindingError::InvalidUtf8)?;
                Ok(dictionary.contains(term))
            }
            DynamicBackend::U64(_) => Err(BindingError::DomainMismatch),
        }
    }

    /// Read the optional value for a UTF-8/byte term.
    pub fn value_text(&self, term: &[u8]) -> Result<Option<Option<u64>>, BindingError> {
        let backend = self
            .shared
            .backend
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match &*backend {
            DynamicBackend::Byte(dictionary) => Ok(dictionary
                .get_bytes_optional_value(term)
                .map(|value| value.and_then(BindingValue::into_option))),
            DynamicBackend::Unicode(dictionary) => {
                let term = std::str::from_utf8(term).map_err(|_| BindingError::InvalidUtf8)?;
                Ok(dictionary
                    .get_optional_value(term)
                    .map(|value| value.and_then(BindingValue::into_option)))
            }
            DynamicBackend::U64(_) => Err(BindingError::DomainMismatch),
        }
    }

    /// Insert or update a u64-token term and optional value.
    pub fn insert_u64(&self, term: &[u64], value: Option<u64>) -> Result<bool, BindingError> {
        let backend = self
            .shared
            .backend
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match &*backend {
            DynamicBackend::U64(dictionary) => Ok(dictionary
                .insert_sequence_with_optional_value(term, value.map(BindingValue::present))),
            _ => Err(BindingError::DomainMismatch),
        }
    }

    /// Insert/update a complete u64 batch with the same empty-dictionary
    /// freeze-once fast path as [`insert_text_batch`](Self::insert_text_batch).
    pub fn insert_u64_batch<'a, I>(&self, entries: I) -> Result<usize, BindingError>
    where
        I: IntoIterator<Item = (&'a [u64], Option<u64>)>,
    {
        let entries: Vec<_> = entries.into_iter().collect();
        let mut backend = self
            .shared
            .backend
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        if backend.len() == 0 {
            if !matches!(&*backend, DynamicBackend::U64(_)) {
                return Err(BindingError::DomainMismatch);
            }
            let mut owned: Vec<_> = entries
                .iter()
                .map(|(term, value)| (term.to_vec(), value.map(BindingValue::present)))
                .collect();
            if !owned.windows(2).all(|pair| pair[0].0 <= pair[1].0) {
                crate::causal_perf::record_batch_sort_calls(1);
                crate::causal_perf::record_batch_sort_terms(owned.len() as u64);
                crate::causal_perf::record_batch_sort_units(
                    owned.iter().map(|(term, _)| term.len()).sum::<usize>() as u64,
                );
                owned.sort_by(|left, right| left.0.cmp(&right.0));
            }
            *backend = DynamicBackend::U64(DynamicDawgU64::from_sorted_sequence_entries(owned));
            return Ok(backend.len());
        }
        drop(backend);

        let mut inserted = 0usize;
        for (term, value) in entries {
            inserted += usize::from(self.insert_u64(term, value)?);
        }
        Ok(inserted)
    }

    /// Remove a u64-token term.
    pub fn remove_u64(&self, term: &[u64]) -> Result<bool, BindingError> {
        let backend = self
            .shared
            .backend
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match &*backend {
            DynamicBackend::U64(dictionary) => Ok(dictionary.remove_sequence(term)),
            _ => Err(BindingError::DomainMismatch),
        }
    }

    /// Test membership for a u64-token term.
    pub fn contains_u64(&self, term: &[u64]) -> Result<bool, BindingError> {
        let backend = self
            .shared
            .backend
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match &*backend {
            DynamicBackend::U64(dictionary) => Ok(dictionary.contains_sequence(term)),
            _ => Err(BindingError::DomainMismatch),
        }
    }

    /// Read the optional value for a u64-token term.
    pub fn value_u64(&self, term: &[u64]) -> Result<Option<Option<u64>>, BindingError> {
        let backend = self
            .shared
            .backend
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match &*backend {
            DynamicBackend::U64(dictionary) => Ok(dictionary
                .get_sequence_optional_value(term)
                .map(|value| value.and_then(BindingValue::into_option))),
            _ => Err(BindingError::DomainMismatch),
        }
    }

    /// Remove every term by atomically replacing the current empty revision.
    pub fn clear(&self) {
        let mut backend = self
            .shared
            .backend
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let domain = backend.domain();
        *backend = DynamicBackend::new(domain);
    }

    /// Restore compact DynamicDAWG structure and return reclaimed nodes.
    pub fn compact(&self) -> usize {
        let backend = self
            .shared
            .backend
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match &*backend {
            DynamicBackend::Byte(dictionary) => dictionary.compact(),
            DynamicBackend::Unicode(dictionary) => dictionary.compact(),
            DynamicBackend::U64(dictionary) => dictionary.compact(),
        }
    }

    /// Borrow a two-word resource. The returned resource owns one retain.
    pub fn resource(&self) -> OwnedDictionaryResource {
        OwnedDictionaryResource::new(ResourcePayload::Live(Arc::clone(&self.shared)))
    }
}

trait AbiUnit: Copy + Send + Sync + 'static {
    fn to_abi(self) -> u64;
}

trait AbiValue: crate::DictionaryValue {
    fn into_abi_value(self) -> Option<u64>;
}

impl AbiValue for BindingValue {
    fn into_abi_value(self) -> Option<u64> {
        self.into_option()
    }
}

impl AbiValue for u64 {
    fn into_abi_value(self) -> Option<u64> {
        Some(self)
    }
}

impl AbiUnit for u8 {
    fn to_abi(self) -> u64 {
        u64::from(self)
    }
}

impl AbiUnit for char {
    fn to_abi(self) -> u64 {
        u64::from(u32::from(self))
    }
}

impl AbiUnit for u64 {
    fn to_abi(self) -> u64 {
        self
    }
}

struct NodeArena<N> {
    nodes: Vec<N>,
    edges: Vec<Option<Vec<(u64, u64)>>>,
}

struct TraversalSnapshot<N> {
    arena: Mutex<NodeArena<N>>,
    len: Option<usize>,
    domain: VtUnitDomain,
    suffix: bool,
}

impl<N> TraversalSnapshot<N> {
    fn new(root: N, len: Option<usize>, domain: VtUnitDomain, suffix: bool) -> Self {
        crate::causal_perf::record_resource_snapshots_created(1);
        crate::causal_perf::record_resource_nodes_materialized(1);
        Self {
            arena: Mutex::new(NodeArena {
                nodes: vec![root],
                edges: vec![None],
            }),
            len,
            domain,
            suffix,
        }
    }
}

trait SnapshotOps: Send + Sync {
    fn domain(&self) -> VtUnitDomain;
    fn suffix(&self) -> bool;
    fn len(&self) -> Option<usize>;
    fn is_final(&self, node: u64) -> Result<bool, VtStatus>;
    fn value(&self, node: u64) -> Result<Option<u64>, VtStatus>;
    fn transition(&self, node: u64, label: u64) -> Result<Option<u64>, VtStatus>;
    fn copy_edges(
        &self,
        node: u64,
        start: usize,
        output: &mut [VtDictionaryEdge],
    ) -> Result<(usize, usize), VtStatus>;
    fn copy_node(
        &self,
        node: u64,
        start: usize,
        output: &mut [VtDictionaryEdge],
    ) -> Result<(bool, usize, usize), VtStatus> {
        let is_final = self.is_final(node)?;
        let (written, total) = self.copy_edges(node, start, output)?;
        Ok((is_final, written, total))
    }
}

impl<N> TraversalSnapshot<N>
where
    N: MappedDictionaryNode + 'static,
    N::Unit: AbiUnit,
    N::Value: AbiValue,
{
    fn ensure_edges(arena: &mut NodeArena<N>, node: usize) -> Result<(), VtStatus> {
        if node >= arena.nodes.len() {
            return Err(VtStatus::InvalidArgument);
        }
        if arena.edges[node].is_some() {
            return Ok(());
        }
        let children = crate::collect_node_edges(&arena.nodes[node]);
        crate::causal_perf::record_resource_native_edges_enumerated(children.len() as u64);
        crate::causal_perf::record_resource_nodes_materialized(children.len() as u64);
        let mut descriptors = Vec::with_capacity(children.len());
        for (label, child) in children {
            let child_id = u64::try_from(arena.nodes.len()).map_err(|_| VtStatus::LimitExceeded)?;
            arena.nodes.push(child);
            arena.edges.push(None);
            descriptors.push((label.to_abi(), child_id));
        }
        arena.edges[node] = Some(descriptors);
        Ok(())
    }
}

impl<N> SnapshotOps for TraversalSnapshot<N>
where
    N: MappedDictionaryNode + 'static,
    N::Unit: AbiUnit,
    N::Value: AbiValue,
{
    fn domain(&self) -> VtUnitDomain {
        self.domain
    }

    fn suffix(&self) -> bool {
        self.suffix
    }

    fn len(&self) -> Option<usize> {
        self.len
    }

    fn is_final(&self, node: u64) -> Result<bool, VtStatus> {
        let node = usize::try_from(node).map_err(|_| VtStatus::InvalidArgument)?;
        crate::causal_perf::record_resource_is_final_calls(1);
        crate::causal_perf::record_resource_arena_locks(1);
        let arena = self
            .arena
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        arena
            .nodes
            .get(node)
            .map(DictionaryNode::is_final)
            .ok_or(VtStatus::InvalidArgument)
    }

    fn value(&self, node: u64) -> Result<Option<u64>, VtStatus> {
        let node = usize::try_from(node).map_err(|_| VtStatus::InvalidArgument)?;
        crate::causal_perf::record_resource_value_calls(1);
        crate::causal_perf::record_resource_arena_locks(1);
        let arena = self
            .arena
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        arena
            .nodes
            .get(node)
            .map(|value| value.value().and_then(AbiValue::into_abi_value))
            .ok_or(VtStatus::InvalidArgument)
    }

    fn transition(&self, node: u64, label: u64) -> Result<Option<u64>, VtStatus> {
        let node = usize::try_from(node).map_err(|_| VtStatus::InvalidArgument)?;
        crate::causal_perf::record_resource_arena_locks(1);
        let mut arena = self
            .arena
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let cache_miss = arena
            .edges
            .get(node)
            .is_some_and(|descriptors| descriptors.is_none());
        Self::ensure_edges(&mut arena, node)?;
        if cache_miss {
            crate::causal_perf::record_resource_edge_cache_misses(1);
        }
        Ok(arena.edges[node].as_ref().and_then(|edges| {
            edges
                .iter()
                .find_map(|edge| (edge.0 == label).then_some(edge.1))
        }))
    }

    fn copy_edges(
        &self,
        node: u64,
        start: usize,
        output: &mut [VtDictionaryEdge],
    ) -> Result<(usize, usize), VtStatus> {
        let node = usize::try_from(node).map_err(|_| VtStatus::InvalidArgument)?;
        crate::causal_perf::record_resource_edges_calls(1);
        crate::causal_perf::record_resource_arena_locks(1);
        let mut arena = self
            .arena
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let cache_miss = arena
            .edges
            .get(node)
            .is_some_and(|descriptors| descriptors.is_none());
        Self::ensure_edges(&mut arena, node)?;
        if cache_miss {
            crate::causal_perf::record_resource_edge_cache_misses(1);
        }
        let descriptors = arena.edges[node].as_deref().unwrap_or_default();
        let total = descriptors.len();
        let page = descriptors.iter().skip(start).zip(output.iter_mut());
        let mut written = 0usize;
        for ((label, child), slot) in page {
            *slot = VtDictionaryEdge {
                label: *label,
                node: *child,
            };
            written += 1;
        }
        Ok((written, total))
    }

    fn copy_node(
        &self,
        node: u64,
        start: usize,
        output: &mut [VtDictionaryEdge],
    ) -> Result<(bool, usize, usize), VtStatus> {
        let node = usize::try_from(node).map_err(|_| VtStatus::InvalidArgument)?;
        crate::causal_perf::record_resource_is_final_calls(1);
        crate::causal_perf::record_resource_edges_calls(1);
        crate::causal_perf::record_resource_arena_locks(1);
        let mut arena = self
            .arena
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let cache_miss = arena
            .edges
            .get(node)
            .is_some_and(|descriptors| descriptors.is_none());
        let is_final = arena
            .nodes
            .get(node)
            .map(DictionaryNode::is_final)
            .ok_or(VtStatus::InvalidArgument)?;
        Self::ensure_edges(&mut arena, node)?;
        if cache_miss {
            crate::causal_perf::record_resource_edge_cache_misses(1);
        }
        let descriptors = arena.edges[node].as_deref().unwrap_or_default();
        let total = descriptors.len();
        let page = descriptors.iter().skip(start).zip(output.iter_mut());
        let mut written = 0usize;
        for ((label, child), slot) in page {
            *slot = VtDictionaryEdge {
                label: *label,
                node: *child,
            };
            written += 1;
        }
        Ok((is_final, written, total))
    }
}

enum ResourcePayload {
    Live(Arc<SharedDictionary>),
    Secondary(Arc<SecondaryBackend>),
    #[cfg(feature = "persistent-artrie")]
    Persistent(Arc<PersistentBackend>),
    Snapshot(Arc<dyn SnapshotOps>),
}

struct ResourceContext {
    payload: ResourcePayload,
}

impl ResourceContext {
    fn domain(&self) -> VtUnitDomain {
        match &self.payload {
            ResourcePayload::Live(dictionary) => dictionary
                .backend
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .domain()
                .into(),
            ResourcePayload::Secondary(dictionary) => dictionary.domain().into(),
            #[cfg(feature = "persistent-artrie")]
            ResourcePayload::Persistent(dictionary) => dictionary.domain().into(),
            ResourcePayload::Snapshot(snapshot) => snapshot.domain(),
        }
    }

    fn flags(&self) -> u64 {
        dictionary_flags::PARALLEL_REENTRANT
            | match &self.payload {
                ResourcePayload::Live(_) => 0,
                ResourcePayload::Secondary(dictionary) => {
                    if dictionary.suffix() {
                        dictionary_flags::SUFFIX_BASED
                    } else {
                        0
                    }
                }
                #[cfg(feature = "persistent-artrie")]
                ResourcePayload::Persistent(_) => 0,
                ResourcePayload::Snapshot(snapshot) => {
                    dictionary_flags::IMMUTABLE
                        | if snapshot.suffix() {
                            dictionary_flags::SUFFIX_BASED
                        } else {
                            0
                        }
                }
            }
    }

    fn snapshot(&self) -> Arc<dyn SnapshotOps> {
        match &self.payload {
            ResourcePayload::Live(dictionary) => dictionary
                .backend
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .snapshot(),
            ResourcePayload::Secondary(dictionary) => dictionary.snapshot(),
            #[cfg(feature = "persistent-artrie")]
            ResourcePayload::Persistent(dictionary) => dictionary.snapshot(),
            ResourcePayload::Snapshot(snapshot) => Arc::clone(snapshot),
        }
    }

    fn immutable(&self) -> Result<&dyn SnapshotOps, VtStatus> {
        match &self.payload {
            ResourcePayload::Snapshot(snapshot) => Ok(snapshot.as_ref()),
            ResourcePayload::Live(_) | ResourcePayload::Secondary(_) => {
                Err(VtStatus::InvalidArgument)
            }
            #[cfg(feature = "persistent-artrie")]
            ResourcePayload::Persistent(_) => Err(VtStatus::InvalidArgument),
        }
    }
}

/// Owned retain of a `vt.dictionary.v1` resource.
pub struct OwnedDictionaryResource {
    raw: VtResource,
}

impl OwnedDictionaryResource {
    fn new(payload: ResourcePayload) -> Self {
        let context = Arc::new(ResourceContext { payload });
        Self {
            raw: VtResource {
                context: Arc::into_raw(context).cast_mut().cast(),
                vtable: &RESOURCE_VTABLE,
            },
        }
    }

    /// Borrow the two-word ABI value.
    pub fn as_raw(&self) -> VtResource {
        self.raw
    }
}

impl Drop for OwnedDictionaryResource {
    fn drop(&mut self) {
        unsafe { resource_release(self.raw.context) };
    }
}

unsafe extern "C" fn resource_retain(context: *mut c_void) {
    if !context.is_null() {
        Arc::increment_strong_count(context.cast::<ResourceContext>());
    }
}

unsafe extern "C" fn resource_release(context: *mut c_void) {
    if !context.is_null() {
        Arc::decrement_strong_count(context.cast::<ResourceContext>());
    }
}

unsafe extern "C" fn query_interface(
    context: *mut c_void,
    interface_id: *const VtInterfaceId,
    minimum_version: u32,
    out_vtable: *mut *const c_void,
) -> u32 {
    query_interface_status(context, interface_id, minimum_version, out_vtable).to_raw()
}

unsafe fn query_interface_status(
    context: *mut c_void,
    interface_id: *const VtInterfaceId,
    minimum_version: u32,
    out_vtable: *mut *const c_void,
) -> VtStatus {
    if context.is_null() || interface_id.is_null() || out_vtable.is_null() {
        return VtStatus::NullPointer;
    }
    let context = &*context.cast::<ResourceContext>();
    if (*interface_id).bytes == VT_DICTIONARY_INTERFACE_ID.bytes
        && minimum_version <= VT_DICTIONARY_INTERFACE_VERSION
    {
        out_vtable.write(dictionary_vtable(context.domain(), context.flags()).cast());
        VtStatus::Ok
    } else if (*interface_id).bytes == VT_DICTIONARY_VISIT_INTERFACE_ID.bytes
        && minimum_version <= VT_DICTIONARY_VISIT_INTERFACE_VERSION
    {
        out_vtable.write((&DICTIONARY_VISIT_VTABLE as *const VtDictionaryVisitVTable).cast());
        VtStatus::Ok
    } else {
        VtStatus::Unsupported
    }
}

unsafe extern "C" fn dictionary_snapshot(
    context: *mut c_void,
    out_snapshot: *mut VtResource,
) -> u32 {
    dictionary_snapshot_status(context, out_snapshot).to_raw()
}

unsafe fn dictionary_snapshot_status(
    context: *mut c_void,
    out_snapshot: *mut VtResource,
) -> VtStatus {
    if context.is_null() || out_snapshot.is_null() {
        return VtStatus::NullPointer;
    }
    let context = &*context.cast::<ResourceContext>();
    let owned = OwnedDictionaryResource::new(ResourcePayload::Snapshot(context.snapshot()));
    out_snapshot.write(owned.raw);
    std::mem::forget(owned);
    VtStatus::Ok
}

unsafe extern "C" fn dictionary_root(context: *mut c_void, out_node: *mut u64) -> u32 {
    dictionary_root_status(context, out_node).to_raw()
}

unsafe fn dictionary_root_status(context: *mut c_void, out_node: *mut u64) -> VtStatus {
    if context.is_null() || out_node.is_null() {
        return VtStatus::NullPointer;
    }
    let context = &*context.cast::<ResourceContext>();
    match context.immutable() {
        Ok(_) => {
            out_node.write(0);
            VtStatus::Ok
        }
        Err(status) => status,
    }
}

unsafe extern "C" fn dictionary_len(
    context: *mut c_void,
    out_len: *mut usize,
    out_known: *mut u8,
) -> u32 {
    dictionary_len_status(context, out_len, out_known).to_raw()
}

unsafe fn dictionary_len_status(
    context: *mut c_void,
    out_len: *mut usize,
    out_known: *mut u8,
) -> VtStatus {
    if context.is_null() || out_len.is_null() || out_known.is_null() {
        return VtStatus::NullPointer;
    }
    let context = &*context.cast::<ResourceContext>();
    let Ok(snapshot) = context.immutable() else {
        return VtStatus::InvalidArgument;
    };
    match snapshot.len() {
        Some(len) => {
            out_len.write(len);
            out_known.write(1);
        }
        None => {
            out_len.write(0);
            out_known.write(0);
        }
    }
    VtStatus::Ok
}

unsafe extern "C" fn dictionary_is_final(
    context: *mut c_void,
    node: u64,
    out_is_final: *mut u8,
) -> u32 {
    dictionary_is_final_status(context, node, out_is_final).to_raw()
}

unsafe fn dictionary_is_final_status(
    context: *mut c_void,
    node: u64,
    out_is_final: *mut u8,
) -> VtStatus {
    if context.is_null() || out_is_final.is_null() {
        return VtStatus::NullPointer;
    }
    let context = &*context.cast::<ResourceContext>();
    match context
        .immutable()
        .and_then(|snapshot| snapshot.is_final(node))
    {
        Ok(value) => {
            out_is_final.write(u8::from(value));
            VtStatus::Ok
        }
        Err(status) => status,
    }
}

unsafe extern "C" fn dictionary_value(
    context: *mut c_void,
    node: u64,
    out_value: *mut VtOptionalU64,
) -> u32 {
    dictionary_value_status(context, node, out_value).to_raw()
}

unsafe fn dictionary_value_status(
    context: *mut c_void,
    node: u64,
    out_value: *mut VtOptionalU64,
) -> VtStatus {
    if context.is_null() || out_value.is_null() {
        return VtStatus::NullPointer;
    }
    let context = &*context.cast::<ResourceContext>();
    match context
        .immutable()
        .and_then(|snapshot| snapshot.value(node))
    {
        Ok(value) => {
            out_value.write(VtOptionalU64 {
                value: value.unwrap_or_default(),
                has_value: u8::from(value.is_some()),
                reserved: [0; 7],
            });
            VtStatus::Ok
        }
        Err(status) => status,
    }
}

unsafe extern "C" fn dictionary_transition(
    context: *mut c_void,
    node: u64,
    label: u64,
    out_child: *mut u64,
    out_found: *mut u8,
) -> u32 {
    dictionary_transition_status(context, node, label, out_child, out_found).to_raw()
}

unsafe fn dictionary_transition_status(
    context: *mut c_void,
    node: u64,
    label: u64,
    out_child: *mut u64,
    out_found: *mut u8,
) -> VtStatus {
    if context.is_null() || out_child.is_null() || out_found.is_null() {
        return VtStatus::NullPointer;
    }
    let context = &*context.cast::<ResourceContext>();
    match context
        .immutable()
        .and_then(|snapshot| snapshot.transition(node, label))
    {
        Ok(child) => {
            out_child.write(child.unwrap_or_default());
            out_found.write(u8::from(child.is_some()));
            VtStatus::Ok
        }
        Err(status) => status,
    }
}

unsafe extern "C" fn dictionary_edges(
    context: *mut c_void,
    node: u64,
    start: usize,
    out_edges: *mut VtDictionaryEdge,
    capacity: usize,
    out_written: *mut usize,
    out_total: *mut usize,
) -> u32 {
    dictionary_edges_status(
        context,
        node,
        start,
        out_edges,
        capacity,
        out_written,
        out_total,
    )
    .to_raw()
}

unsafe fn dictionary_edges_status(
    context: *mut c_void,
    node: u64,
    start: usize,
    out_edges: *mut VtDictionaryEdge,
    capacity: usize,
    out_written: *mut usize,
    out_total: *mut usize,
) -> VtStatus {
    if context.is_null()
        || out_written.is_null()
        || out_total.is_null()
        || (capacity != 0 && out_edges.is_null())
    {
        return VtStatus::NullPointer;
    }
    let context = &*context.cast::<ResourceContext>();
    if capacity > isize::MAX as usize / std::mem::size_of::<VtDictionaryEdge>() {
        return VtStatus::LimitExceeded;
    }
    let output = if capacity == 0 {
        &mut []
    } else {
        std::slice::from_raw_parts_mut(out_edges, capacity)
    };
    let (written, total) = match context
        .immutable()
        .and_then(|snapshot| snapshot.copy_edges(node, start, output))
    {
        Ok(page) => page,
        Err(status) => return status,
    };
    out_total.write(total);
    out_written.write(written);
    VtStatus::Ok
}

unsafe extern "C" fn dictionary_visit(
    context: *mut c_void,
    node: u64,
    start: usize,
    out_is_final: *mut u8,
    out_edges: *mut VtDictionaryEdge,
    capacity: usize,
    out_written: *mut usize,
    out_total: *mut usize,
) -> u32 {
    dictionary_visit_status(
        context,
        node,
        start,
        out_is_final,
        out_edges,
        capacity,
        out_written,
        out_total,
    )
    .to_raw()
}

// Keep the status-returning helper isomorphic to the public C callback. The
// eight fields are fixed by `VtDictionaryVisitVTable`, not an internal API
// design choice, and grouping them would obscure the boundary validation.
#[allow(clippy::too_many_arguments)]
unsafe fn dictionary_visit_status(
    context: *mut c_void,
    node: u64,
    start: usize,
    out_is_final: *mut u8,
    out_edges: *mut VtDictionaryEdge,
    capacity: usize,
    out_written: *mut usize,
    out_total: *mut usize,
) -> VtStatus {
    if context.is_null()
        || out_is_final.is_null()
        || out_written.is_null()
        || out_total.is_null()
        || (capacity != 0 && out_edges.is_null())
    {
        return VtStatus::NullPointer;
    }
    if capacity > isize::MAX as usize / std::mem::size_of::<VtDictionaryEdge>() {
        return VtStatus::LimitExceeded;
    }
    let output = if capacity == 0 {
        &mut []
    } else {
        std::slice::from_raw_parts_mut(out_edges, capacity)
    };
    let context = &*context.cast::<ResourceContext>();
    let (is_final, written, total) = match context
        .immutable()
        .and_then(|snapshot| snapshot.copy_node(node, start, output))
    {
        Ok(page) => page,
        Err(status) => return status,
    };
    out_is_final.write(u8::from(is_final));
    out_written.write(written);
    out_total.write(total);
    VtStatus::Ok
}

static RESOURCE_VTABLE: VtResourceVTable = VtResourceVTable {
    struct_size: std::mem::size_of::<VtResourceVTable>(),
    abi_version: VT_ABI_VERSION,
    reserved: 0,
    retain: Some(resource_retain),
    release: Some(resource_release),
    query_interface: Some(query_interface),
};

static DICTIONARY_VISIT_VTABLE: VtDictionaryVisitVTable = VtDictionaryVisitVTable {
    struct_size: std::mem::size_of::<VtDictionaryVisitVTable>(),
    interface_version: VT_DICTIONARY_VISIT_INTERFACE_VERSION,
    reserved: 0,
    node_visit: Some(dictionary_visit),
};

macro_rules! dictionary_vtable {
    ($name:ident, $domain:expr, $flags:expr) => {
        static $name: VtDictionaryVTable = VtDictionaryVTable {
            struct_size: std::mem::size_of::<VtDictionaryVTable>(),
            interface_version: VT_DICTIONARY_INTERFACE_VERSION,
            unit_domain: $domain,
            value_domain: VtValueDomain::OptionalU64,
            flags: $flags,
            snapshot: Some(dictionary_snapshot),
            root: Some(dictionary_root),
            len: Some(dictionary_len),
            node_is_final: Some(dictionary_is_final),
            node_value_u64: Some(dictionary_value),
            node_transition: Some(dictionary_transition),
            node_edges: Some(dictionary_edges),
        };
    };
}

dictionary_vtable!(
    BYTE_LIVE,
    VtUnitDomain::Byte,
    dictionary_flags::PARALLEL_REENTRANT
);
dictionary_vtable!(
    UNICODE_LIVE,
    VtUnitDomain::UnicodeScalar,
    dictionary_flags::PARALLEL_REENTRANT
);
dictionary_vtable!(
    U64_LIVE,
    VtUnitDomain::U64,
    dictionary_flags::PARALLEL_REENTRANT
);
dictionary_vtable!(
    BYTE_SUFFIX_LIVE,
    VtUnitDomain::Byte,
    dictionary_flags::PARALLEL_REENTRANT | dictionary_flags::SUFFIX_BASED
);
dictionary_vtable!(
    UNICODE_SUFFIX_LIVE,
    VtUnitDomain::UnicodeScalar,
    dictionary_flags::PARALLEL_REENTRANT | dictionary_flags::SUFFIX_BASED
);
dictionary_vtable!(
    BYTE_SNAPSHOT,
    VtUnitDomain::Byte,
    dictionary_flags::PARALLEL_REENTRANT | dictionary_flags::IMMUTABLE
);
dictionary_vtable!(
    UNICODE_SNAPSHOT,
    VtUnitDomain::UnicodeScalar,
    dictionary_flags::PARALLEL_REENTRANT | dictionary_flags::IMMUTABLE
);
dictionary_vtable!(
    U64_SNAPSHOT,
    VtUnitDomain::U64,
    dictionary_flags::PARALLEL_REENTRANT | dictionary_flags::IMMUTABLE
);
dictionary_vtable!(
    BYTE_SUFFIX_SNAPSHOT,
    VtUnitDomain::Byte,
    dictionary_flags::PARALLEL_REENTRANT
        | dictionary_flags::IMMUTABLE
        | dictionary_flags::SUFFIX_BASED
);
dictionary_vtable!(
    UNICODE_SUFFIX_SNAPSHOT,
    VtUnitDomain::UnicodeScalar,
    dictionary_flags::PARALLEL_REENTRANT
        | dictionary_flags::IMMUTABLE
        | dictionary_flags::SUFFIX_BASED
);

fn dictionary_vtable(domain: VtUnitDomain, flags: u64) -> *const VtDictionaryVTable {
    let immutable = flags & dictionary_flags::IMMUTABLE != 0;
    let suffix = flags & dictionary_flags::SUFFIX_BASED != 0;
    match (domain, immutable, suffix) {
        (VtUnitDomain::Byte, false, false) => &BYTE_LIVE,
        (VtUnitDomain::UnicodeScalar, false, false) => &UNICODE_LIVE,
        (VtUnitDomain::U64, false, _) => &U64_LIVE,
        (VtUnitDomain::Byte, false, true) => &BYTE_SUFFIX_LIVE,
        (VtUnitDomain::UnicodeScalar, false, true) => &UNICODE_SUFFIX_LIVE,
        (VtUnitDomain::Byte, true, false) => &BYTE_SNAPSHOT,
        (VtUnitDomain::UnicodeScalar, true, false) => &UNICODE_SNAPSHOT,
        (VtUnitDomain::U64, true, false) => &U64_SNAPSHOT,
        (VtUnitDomain::Byte, true, true) => &BYTE_SUFFIX_SNAPSHOT,
        (VtUnitDomain::UnicodeScalar, true, true) => &UNICODE_SUFFIX_SNAPSHOT,
        (VtUnitDomain::U64, true, true) => &U64_SNAPSHOT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot_edges(snapshot: &dyn SnapshotOps, node: u64) -> Result<Vec<(u64, u64)>, VtStatus> {
        let (_, total) = snapshot.copy_edges(node, 0, &mut [])?;
        let mut edges = vec![VtDictionaryEdge::default(); total];
        let (written, confirmed_total) = snapshot.copy_edges(node, 0, &mut edges)?;
        assert_eq!(confirmed_total, total);
        assert_eq!(written, total);
        Ok(edges
            .into_iter()
            .map(|edge| (edge.label, edge.node))
            .collect())
    }

    #[test]
    fn resource_snapshot_keeps_the_query_start_revision() {
        let dictionary = DynamicDawgBinding::new(BindingUnitDomain::UnicodeScalar);
        dictionary.insert_text(b"cat", Some(1)).unwrap();
        dictionary.insert_text(b"cot", Some(2)).unwrap();
        let resource = dictionary.resource();

        let live = unsafe { &*resource.raw.context.cast::<ResourceContext>() };
        let snapshot = live.snapshot();
        dictionary.remove_text(b"cot").unwrap();
        dictionary.insert_text(b"cut", Some(3)).unwrap();

        assert_eq!(snapshot.len(), Some(2));
        assert_eq!(dictionary.len(), 2);
        let root_edges = snapshot_edges(&*snapshot, 0).unwrap();
        assert_eq!(root_edges.len(), 1);
    }

    #[test]
    fn empty_dynamic_batches_use_minimal_builders_in_every_unit_domain() {
        let bytes = DynamicDawgBinding::new(BindingUnitDomain::Byte);
        assert_eq!(
            bytes
                .insert_text_batch([
                    (b"cb".as_slice(), None),
                    (b"ab".as_slice(), Some(1)),
                    (b"ab".as_slice(), None),
                ])
                .unwrap(),
            2
        );
        assert_eq!(bytes.value_text(b"ab").unwrap(), Some(None));
        assert_eq!(bytes.value_text(b"cb").unwrap(), Some(None));

        let unicode = DynamicDawgBinding::new(BindingUnitDomain::UnicodeScalar);
        assert_eq!(
            unicode
                .insert_text_batch([("γβ".as_bytes(), None), ("αβ".as_bytes(), Some(7)),])
                .unwrap(),
            2
        );
        assert_eq!(unicode.value_text("αβ".as_bytes()).unwrap(), Some(Some(7)));

        let tokens = DynamicDawgBinding::new(BindingUnitDomain::U64);
        assert_eq!(
            tokens
                .insert_u64_batch([
                    ([2_u64, 9].as_slice(), None),
                    ([1_u64, 9].as_slice(), Some(11)),
                ])
                .unwrap(),
            2
        );
        assert_eq!(tokens.value_u64(&[1, 9]).unwrap(), Some(Some(11)));

        let byte_backend = bytes
            .shared
            .backend
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let DynamicBackend::Byte(byte_dawg) = &*byte_backend else {
            panic!("byte binding changed domains");
        };
        assert_eq!(byte_dawg.node_count(), 3);
        drop(byte_backend);

        let unicode_backend = unicode
            .shared
            .backend
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let DynamicBackend::Unicode(unicode_dawg) = &*unicode_backend else {
            panic!("Unicode binding changed domains");
        };
        // The two suffix nodes cannot merge because one final carries a value
        // and the other is valueless: finality plus value is part of the
        // minimized-state signature.
        assert_eq!(unicode_dawg.node_count(), 5);
        drop(unicode_backend);

        let token_backend = tokens
            .shared
            .backend
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let DynamicBackend::U64(token_dawg) = &*token_backend else {
            panic!("u64 binding changed domains");
        };
        assert_eq!(token_dawg.node_count(), 5);
    }

    #[test]
    fn dynamic_batch_replacement_preserves_snapshots_and_incremental_updates() {
        let dictionary = DynamicDawgBinding::new(BindingUnitDomain::UnicodeScalar);
        let resource = dictionary.resource();
        let live = unsafe { &*resource.raw.context.cast::<ResourceContext>() };
        let empty_snapshot = live.snapshot();

        assert_eq!(
            dictionary
                .insert_text_batch([(b"cat".as_slice(), Some(5)), (b"cot".as_slice(), None),])
                .unwrap(),
            2
        );
        assert_eq!(empty_snapshot.len(), Some(0));
        assert_eq!(dictionary.len(), 2);

        // A later batch falls back to ordinary update semantics: one new term,
        // and an existing valued term becomes explicitly valueless.
        assert_eq!(
            dictionary
                .insert_text_batch([(b"cat".as_slice(), None), (b"cut".as_slice(), Some(9)),])
                .unwrap(),
            1
        );
        assert_eq!(dictionary.value_text(b"cat").unwrap(), Some(None));
        assert_eq!(dictionary.value_text(b"cut").unwrap(), Some(Some(9)));
    }

    #[test]
    fn invalid_unicode_empty_batch_does_not_publish_a_partial_dictionary() {
        let dictionary = DynamicDawgBinding::new(BindingUnitDomain::UnicodeScalar);
        let invalid = [0xff_u8];
        assert_eq!(
            dictionary
                .insert_text_batch([(b"valid".as_slice(), None), (invalid.as_slice(), None),]),
            Err(BindingError::InvalidUtf8)
        );
        assert!(dictionary.is_empty());
    }

    #[test]
    fn every_project_owned_resource_is_parallel_and_reentrant() {
        let dynamic = DynamicDawgBinding::new(BindingUnitDomain::UnicodeScalar);
        let dynamic_resource = dynamic.resource();
        let live = unsafe { &*dynamic_resource.raw.context.cast::<ResourceContext>() };
        assert_ne!(live.flags() & dictionary_flags::PARALLEL_REENTRANT, 0);

        for &(domain, flags) in &[
            (VtUnitDomain::Byte, dictionary_flags::PARALLEL_REENTRANT),
            (
                VtUnitDomain::UnicodeScalar,
                dictionary_flags::PARALLEL_REENTRANT,
            ),
            (VtUnitDomain::U64, dictionary_flags::PARALLEL_REENTRANT),
            (
                VtUnitDomain::UnicodeScalar,
                dictionary_flags::PARALLEL_REENTRANT
                    | dictionary_flags::IMMUTABLE
                    | dictionary_flags::SUFFIX_BASED,
            ),
        ] {
            let vtable = unsafe { &*dictionary_vtable(domain, flags) };
            assert_ne!(
                vtable.flags & dictionary_flags::PARALLEL_REENTRANT,
                0,
                "project-owned vtable did not advertise parallel/reentrant access"
            );
        }
    }

    // -----------------------------------------------------------------
    // W2 producer-invariant extensions. The tests below reach the internal
    // `Arc<ResourceContext>` that realizes the ABI reference counter, which
    // no consumer can do across the boundary — this is the numeric half of
    // the LDICT-LIFE-1 balance law (the behavioural half lives in
    // tests/ffi_concurrent_snapshot_stress.rs).
    // -----------------------------------------------------------------

    /// Observe the realized reference count of a resource context.
    unsafe fn context_strong_count(raw: VtResource) -> usize {
        let context = raw.context.cast::<ResourceContext>();
        Arc::increment_strong_count(context);
        let probe = Arc::from_raw(context);
        let count = Arc::strong_count(&probe) - 1;
        drop(probe);
        count
    }

    /// A `Weak` observer that outlives every ABI retain of the context.
    unsafe fn context_weak_observer(raw: VtResource) -> std::sync::Weak<ResourceContext> {
        let context = raw.context.cast::<ResourceContext>();
        Arc::increment_strong_count(context);
        let probe = Arc::from_raw(context);
        let observer = Arc::downgrade(&probe);
        drop(probe);
        observer
    }

    /// INVARIANT-HOOK: LDICT-LIFE-1 (numeric): each `resource()` borrow is
    /// an independent context owning exactly one retain; copying the two
    /// words is not a retain; vtable retain/release move the counter by
    /// exactly one; dropping the owner destroys the context exactly once.
    #[test]
    fn owned_resources_balance_their_retains_exactly() {
        let dictionary = DynamicDawgBinding::new(BindingUnitDomain::UnicodeScalar);
        dictionary
            .insert_text(b"cat", Some(1))
            .expect("insert must succeed");

        let first = dictionary.resource();
        let second = dictionary.resource();
        assert!(
            !std::ptr::eq(first.raw.context, second.raw.context),
            "each borrow mints an independent context"
        );
        assert_eq!(unsafe { context_strong_count(first.as_raw()) }, 1);
        assert_eq!(unsafe { context_strong_count(second.as_raw()) }, 1);

        // Copying the two words never moves the counter.
        let copied = first.as_raw();
        let copied_again = copied;
        assert_eq!(unsafe { context_strong_count(copied_again) }, 1);

        // Vtable retain/release move it by exactly one.
        unsafe { resource_retain(copied.context) };
        assert_eq!(unsafe { context_strong_count(copied) }, 2);
        unsafe { resource_release(copied.context) };
        assert_eq!(unsafe { context_strong_count(copied) }, 1);

        // retain/release of NULL are documented no-ops.
        unsafe { resource_retain(std::ptr::null_mut()) };
        unsafe { resource_release(std::ptr::null_mut()) };

        // Dropping the owner is the final release: the context dies once.
        let observer = unsafe { context_weak_observer(first.as_raw()) };
        drop(first);
        assert_eq!(observer.strong_count(), 0, "drop released the last retain");
        assert!(
            observer.upgrade().is_none(),
            "context destroyed exactly once"
        );
        // The sibling context is untouched.
        assert_eq!(unsafe { context_strong_count(second.as_raw()) }, 1);
    }

    /// INVARIANT-HOOK: LDICT-LIFE-1 + LDICT-SNAP-3: `dictionary_snapshot`
    /// transfers exactly one owned retain to the caller (the `mem::forget`
    /// in the producer is the ownership handoff, not a leak), for captures
    /// of live resources and of snapshots alike.
    #[test]
    fn dictionary_snapshot_transfers_exactly_one_owned_retain() {
        let dictionary = DynamicDawgBinding::new(BindingUnitDomain::UnicodeScalar);
        dictionary
            .insert_text(b"cot", Some(2))
            .expect("insert must succeed");
        let resource = dictionary.resource();

        let mut captured = VtResource::NULL;
        let status = unsafe { dictionary_snapshot(resource.raw.context, &mut captured) };
        assert_eq!(status, VtStatus::Ok.to_raw());
        assert!(!captured.context.is_null());
        assert_eq!(unsafe { context_strong_count(captured) }, 1);
        // The source context's count is unchanged by the capture.
        assert_eq!(unsafe { context_strong_count(resource.as_raw()) }, 1);

        // Snapshot-of-snapshot: a NEW context (shared arena), one retain.
        let mut nested = VtResource::NULL;
        let status = unsafe { dictionary_snapshot(captured.context, &mut nested) };
        assert_eq!(status, VtStatus::Ok.to_raw());
        assert!(
            !std::ptr::eq(nested.context, captured.context),
            "snapshot-of-snapshot mints a fresh context"
        );
        assert_eq!(unsafe { context_strong_count(nested) }, 1);

        // Releasing the transferred retains destroys each context once.
        let captured_observer = unsafe { context_weak_observer(captured) };
        let nested_observer = unsafe { context_weak_observer(nested) };
        unsafe { resource_release(nested.context) };
        assert_eq!(nested_observer.strong_count(), 0);
        unsafe { resource_release(captured.context) };
        assert_eq!(captured_observer.strong_count(), 0);
    }

    /// ABI-local node identifiers are stable within one snapshot (repeated
    /// enumeration and transition agree) and independent across snapshots
    /// (a later snapshot's ids neither move nor validate an earlier one's
    /// id space).
    ///
    /// INVARIANT-HOOK: LDICT-ARENA-1..5 — the executable mirror of the arena
    /// laws proved in
    /// formal-verification/rocq/Spec/AbiTraversalSnapshotSpec.v.
    #[test]
    fn node_ids_are_stable_within_and_independent_across_snapshots() {
        let dictionary = DynamicDawgBinding::new(BindingUnitDomain::UnicodeScalar);
        dictionary
            .insert_text(b"ab", Some(1))
            .expect("insert must succeed");
        dictionary
            .insert_text(b"ac", Some(2))
            .expect("insert must succeed");
        let resource = dictionary.resource();
        let live = unsafe { &*resource.raw.context.cast::<ResourceContext>() };

        let first_snapshot = live.snapshot();
        let root_edges = snapshot_edges(&*first_snapshot, 0).expect("root edges");
        assert_eq!(root_edges.len(), 1, "both terms share the 'a' prefix");
        // Stability: re-enumeration returns identical (label, id) pairs.
        assert_eq!(
            snapshot_edges(&*first_snapshot, 0).expect("root edges again"),
            root_edges
        );
        // Agreement: transition resolves every listed edge to the same id.
        for (label, child) in &root_edges {
            assert_eq!(
                first_snapshot.transition(0, *label).expect("transition"),
                Some(*child)
            );
        }
        // Materialize one level deeper; ids stay stable there too.
        let a_node = root_edges[0].1;
        let deeper = snapshot_edges(&*first_snapshot, a_node).expect("deeper edges");
        assert_eq!(deeper.len(), 2, "'b' and 'c' leaves");
        assert_eq!(
            snapshot_edges(&*first_snapshot, a_node).expect("deeper again"),
            deeper
        );

        // Mutate, then capture a second snapshot: its id space is its own.
        dictionary
            .insert_text(b"zz", Some(3))
            .expect("insert must succeed");
        let second_snapshot = live.snapshot();
        let second_root = snapshot_edges(&*second_snapshot, 0).expect("second root edges");
        assert_eq!(second_root.len(), 2, "'a' and 'z' branches");
        // The first snapshot is untouched by the second's materialization.
        assert_eq!(
            snapshot_edges(&*first_snapshot, 0).expect("still stable"),
            root_edges
        );
        // An id far beyond the first snapshot's arena is invalid THERE,
        // regardless of what any other snapshot materialized.
        assert_eq!(
            first_snapshot.is_final(10_000),
            Err(VtStatus::InvalidArgument)
        );
        assert_eq!(first_snapshot.value(10_000), Err(VtStatus::InvalidArgument));
        assert_eq!(
            snapshot_edges(&*first_snapshot, 10_000),
            Err(VtStatus::InvalidArgument)
        );
    }

    /// The emitted base vtable and all ten dictionary vtables pin their
    /// struct sizes, versions, reserved fields, domains, flag sets, and
    /// fully populated operation slots.
    #[test]
    fn emitted_vtables_pin_sizes_versions_domains_and_flags() {
        assert_eq!(
            RESOURCE_VTABLE.struct_size,
            std::mem::size_of::<VtResourceVTable>()
        );
        assert_eq!(RESOURCE_VTABLE.abi_version, VT_ABI_VERSION);
        assert_eq!(RESOURCE_VTABLE.reserved, 0, "reserved must be zero");
        assert!(RESOURCE_VTABLE.retain.is_some());
        assert!(RESOURCE_VTABLE.release.is_some());
        assert!(RESOURCE_VTABLE.query_interface.is_some());

        const PR: u64 = dictionary_flags::PARALLEL_REENTRANT;
        const SUF: u64 = dictionary_flags::SUFFIX_BASED;
        const IMM: u64 = dictionary_flags::IMMUTABLE;
        // (requested domain, immutable, suffix) -> exact emitted flag set.
        // The U64 rows pin the aliasing in `dictionary_vtable`: no suffix
        // U64 vtable exists, so the suffix bit is dropped for that domain.
        let expectations = [
            (VtUnitDomain::Byte, false, false, PR),
            (VtUnitDomain::UnicodeScalar, false, false, PR),
            (VtUnitDomain::U64, false, false, PR),
            (VtUnitDomain::Byte, false, true, PR | SUF),
            (VtUnitDomain::UnicodeScalar, false, true, PR | SUF),
            (VtUnitDomain::U64, false, true, PR),
            (VtUnitDomain::Byte, true, false, PR | IMM),
            (VtUnitDomain::UnicodeScalar, true, false, PR | IMM),
            (VtUnitDomain::U64, true, false, PR | IMM),
            (VtUnitDomain::Byte, true, true, PR | IMM | SUF),
            (VtUnitDomain::UnicodeScalar, true, true, PR | IMM | SUF),
            (VtUnitDomain::U64, true, true, PR | IMM),
        ];
        for (domain, immutable, suffix, expected_flags) in expectations {
            let mut requested = PR;
            if immutable {
                requested |= IMM;
            }
            if suffix {
                requested |= SUF;
            }
            let vtable = unsafe { &*dictionary_vtable(domain, requested) };
            assert_eq!(
                vtable.struct_size,
                std::mem::size_of::<VtDictionaryVTable>(),
                "{domain:?}/{immutable}/{suffix}: struct_size"
            );
            assert_eq!(
                vtable.interface_version, VT_DICTIONARY_INTERFACE_VERSION,
                "{domain:?}/{immutable}/{suffix}: interface_version"
            );
            assert_eq!(
                vtable.unit_domain, domain,
                "{domain:?}/{immutable}/{suffix}: unit_domain"
            );
            assert_eq!(
                vtable.value_domain,
                VtValueDomain::OptionalU64,
                "{domain:?}/{immutable}/{suffix}: value_domain"
            );
            assert_eq!(
                vtable.flags, expected_flags,
                "{domain:?}/{immutable}/{suffix}: exact flag set"
            );
            assert!(vtable.snapshot.is_some(), "snapshot slot");
            assert!(vtable.root.is_some(), "root slot");
            assert!(vtable.len.is_some(), "len slot");
            assert!(vtable.node_is_final.is_some(), "node_is_final slot");
            assert!(vtable.node_value_u64.is_some(), "node_value_u64 slot");
            assert!(vtable.node_transition.is_some(), "node_transition slot");
            assert!(vtable.node_edges.is_some(), "node_edges slot");
        }

        // The U64 aliasing is pointer-level: suffix requests reuse the
        // non-suffix statics rather than minting lookalike tables.
        assert!(std::ptr::eq(
            dictionary_vtable(VtUnitDomain::U64, PR | SUF),
            dictionary_vtable(VtUnitDomain::U64, PR)
        ));
        assert!(std::ptr::eq(
            dictionary_vtable(VtUnitDomain::U64, PR | IMM | SUF),
            dictionary_vtable(VtUnitDomain::U64, PR | IMM)
        ));
    }
}
