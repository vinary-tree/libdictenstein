//! Dictionary serialization support.
//!
//! This module provides serialization and deserialization of dictionaries
//! using various formats (bincode, JSON, protobuf) with optional compression.
//!
//! # Example
//!
//! ```rust,ignore
//! use libdictenstein::prelude::*;
//! use libdictenstein::serialization::{BincodeSerializer, DictionarySerializer};
//! use std::fs::File;
//!
//! // Create and populate dictionary
//! let dict = DoubleArrayTrie::from_terms(vec!["test", "testing"]);
//!
//! // Serialize to file
//! let file = File::create("dict.bin")?;
//! BincodeSerializer::serialize(&dict, file)?;
//!
//! // Deserialize from file
//! let file = File::open("dict.bin")?;
//! let loaded_dict: DoubleArrayTrie = BincodeSerializer::deserialize(file)?;
//! ```

use crate::{Dictionary, DictionaryNode};
use std::io::{Read, Write};

// Serializer implementations
mod bincode_impl;
mod json_impl;
mod plaintext_impl;

#[cfg(feature = "protobuf")]
pub mod protobuf_impl;

#[cfg(feature = "compression")]
mod compression_impl;

// Shared serde helpers (`Arc<Vec<T>>` and `Arc<Vec<Vec<T>>>` round-tripping).
// `pub(crate)` so the DAT byte + char files can `use` them in serde attribute
// paths.
#[cfg(feature = "serialization")]
pub(crate) mod serde_helpers;

// Re-exports
pub use self::bincode_impl::BincodeSerializer;
pub use self::json_impl::JsonSerializer;
pub use self::plaintext_impl::PlainTextSerializer;

#[cfg(feature = "protobuf")]
pub use self::protobuf_impl::{
    DatProtobufSerializer, OptimizedProtobufSerializer, ProtobufSerializer,
    SuffixAutomatonProtobufSerializer,
};

#[cfg(feature = "compression")]
pub use self::compression_impl::GzipSerializer;

/// Trait for serializing and deserializing dictionaries.
pub trait DictionarySerializer {
    /// Serialize a dictionary to a writer.
    ///
    /// # Arguments
    ///
    /// * `dict` - The dictionary to serialize
    /// * `writer` - Where to write the serialized data
    ///
    /// # Errors
    ///
    /// Returns an error if serialization fails or writing fails.
    fn serialize<D, W>(dict: &D, writer: W) -> Result<(), SerializationError>
    where
        D: Dictionary,
        D::Node: DictionaryNode<Unit = u8>,
        W: Write;

    /// Deserialize a dictionary from a reader.
    ///
    /// # Arguments
    ///
    /// * `reader` - Where to read the serialized data from
    ///
    /// # Errors
    ///
    /// Returns an error if deserialization fails or reading fails.
    fn deserialize<D, R>(reader: R) -> Result<D, SerializationError>
    where
        D: DictionaryFromTerms,
        R: Read;
}

/// Trait for dictionaries that can be constructed from a list of terms.
///
/// This is used by the serialization system to reconstruct dictionaries
/// after deserialization.
pub trait DictionaryFromTerms: Sized {
    /// Create a dictionary from an iterator of terms.
    fn from_terms<I: IntoIterator<Item = String>>(terms: I) -> Self;
}

/// Trait for dictionaries that can be constructed from `(term, value)` pairs.
///
/// The value-preserving serializers (`*_with_values` methods on
/// [`BincodeSerializer`], [`JsonSerializer`], [`PlainTextSerializer`]) require
/// this trait. Backends that implement [`crate::MappedDictionary`] should
/// implement this trait too so values survive serialization round-trips.
///
/// The previous serializer path (`extract_terms` + `from_terms`) silently
/// dropped values for `MappedDictionary` impls because the
/// `Vec<String>`-shaped wire format had no slot for them.
pub trait DictionaryFromTermsWithValues: Sized {
    /// The value type carried by the dictionary's entries.
    type Value: crate::DictionaryValue;

    /// Create a dictionary from an iterator of `(term, value)` pairs.
    fn from_terms_with_values<I>(entries: I) -> Self
    where
        I: IntoIterator<Item = (String, Self::Value)>;
}

/// Errors that can occur during serialization/deserialization.
#[derive(Debug, thiserror::Error)]
pub enum SerializationError {
    /// Error during bincode serialization
    #[error("Bincode error")]
    Bincode(#[from] bincode::Error),
    /// Error during JSON serialization
    #[error("JSON error")]
    Json(#[from] serde_json::Error),
    /// Error during protobuf serialization
    #[cfg(feature = "protobuf")]
    #[error("Protobuf error")]
    Protobuf(#[from] prost::DecodeError),
    /// I/O error
    #[error("I/O error")]
    Io(#[from] std::io::Error),
    /// Dictionary iteration error
    #[error("Dictionary error: {0}")]
    DictionaryError(String),
}

/// Helper to extract all terms from a dictionary.
///
/// Performs an iterative depth-first traversal of the dictionary trie to
/// collect all valid terms. The iterative form (explicit `Vec` stack rather
/// than recursive calls) is required because pathological dictionaries — long
/// single-child chains, for instance — would otherwise overflow the thread
/// stack at depths in the ~50k-edge range.
///
/// **Note**: For suffix automata, use `extract_suffix_automaton_texts()`
/// instead, as this function would extract all possible substrings rather
/// than source texts.
pub fn extract_terms<D>(dict: &D) -> Vec<String>
where
    D: Dictionary,
    D::Node: DictionaryNode<Unit = u8>,
{
    let est_size = dict.len().unwrap_or(100);
    let mut terms: Vec<String> = Vec::with_capacity(est_size);

    // Explicit traversal stack. Each frame collects its node's outgoing edges
    // into a `Vec` (so the frame owns its children and the iterator's borrow
    // of the parent node doesn't outlive the parent across the recursion). We
    // pop edges off the back; `depth` records how many bytes of `current_term`
    // were appended by this frame's parent so we can `truncate` on backtrack.
    struct Frame<N: DictionaryNode<Unit = u8>> {
        children: Vec<(u8, N)>,
        depth: usize,
    }

    let mut current_term: Vec<u8> = Vec::with_capacity(64);
    let root = dict.root();
    push_term_if_final(&root, &current_term, &mut terms);

    let mut stack: Vec<Frame<D::Node>> = Vec::with_capacity(64);
    // Reverse so popping from the back yields edges in encounter order.
    let mut root_children: Vec<(u8, D::Node)> = root.edges().collect();
    root_children.reverse();
    stack.push(Frame {
        children: root_children,
        depth: 0,
    });

    while let Some(frame) = stack.last_mut() {
        match frame.children.pop() {
            Some((byte, child)) => {
                // Record `current_term`'s length BEFORE pushing the descent
                // byte. On backtrack we'll truncate back to this length,
                // which restores the parent's prefix.
                let parent_depth = current_term.len();
                current_term.push(byte);
                push_term_if_final(&child, &current_term, &mut terms);
                let mut child_children: Vec<(u8, D::Node)> = child.edges().collect();
                child_children.reverse();
                drop(child);
                stack.push(Frame {
                    children: child_children,
                    depth: parent_depth,
                });
            }
            None => {
                current_term.truncate(frame.depth);
                stack.pop();
            }
        }
    }

    terms
}

#[inline]
fn push_term_if_final<N: DictionaryNode<Unit = u8>>(
    node: &N,
    current_term: &[u8],
    terms: &mut Vec<String>,
) {
    if node.is_final() {
        match std::str::from_utf8(current_term) {
            Ok(s) => terms.push(s.to_string()),
            Err(_) => terms.push(String::from_utf8_lossy(current_term).into_owned()),
        }
    }
}

/// Char-Unit counterpart to [`extract_terms`].
///
/// Same iterative traversal pattern, but operating on `char` units instead
/// of bytes. Each final node yields its term as a UTF-8 `String` built
/// directly from the accumulated `Vec<char>`. Unblocks value-preserving
/// serialization for `Unit = char` backends (DAT-Char, DynamicDawg-Char,
/// SuffixAutomaton-Char, Scdawg-Char, PathMap-Char).
pub fn extract_terms_char<D>(dict: &D) -> Vec<String>
where
    D: Dictionary,
    D::Node: DictionaryNode<Unit = char>,
{
    let est_size = dict.len().unwrap_or(100);
    let mut terms: Vec<String> = Vec::with_capacity(est_size);

    struct Frame<N: DictionaryNode<Unit = char>> {
        children: Vec<(char, N)>,
        depth: usize,
    }

    let mut current_term: Vec<char> = Vec::with_capacity(64);
    let root = dict.root();
    push_char_term_if_final(&root, &current_term, &mut terms);

    let mut stack: Vec<Frame<D::Node>> = Vec::with_capacity(64);
    let mut root_children: Vec<(char, D::Node)> = root.edges().collect();
    root_children.reverse();
    stack.push(Frame {
        children: root_children,
        depth: 0,
    });

    while let Some(frame) = stack.last_mut() {
        match frame.children.pop() {
            Some((ch, child)) => {
                let parent_depth = current_term.len();
                current_term.push(ch);
                push_char_term_if_final(&child, &current_term, &mut terms);
                let mut child_children: Vec<(char, D::Node)> = child.edges().collect();
                child_children.reverse();
                drop(child);
                stack.push(Frame {
                    children: child_children,
                    depth: parent_depth,
                });
            }
            None => {
                current_term.truncate(frame.depth);
                stack.pop();
            }
        }
    }

    terms
}

#[inline]
fn push_char_term_if_final<N: DictionaryNode<Unit = char>>(
    node: &N,
    current_term: &[char],
    terms: &mut Vec<String>,
) {
    if node.is_final() {
        terms.push(current_term.iter().collect());
    }
}

/// Char-Unit counterpart to [`extract_terms_with_values`].
pub fn extract_terms_with_values_char<D>(dict: &D) -> Vec<(String, D::Value)>
where
    D: crate::MappedDictionary,
    D::Node: DictionaryNode<Unit = char>,
{
    let terms = extract_terms_char(dict);
    let mut out = Vec::with_capacity(terms.len());
    for term in terms {
        if let Some(value) = dict.get_value(&term) {
            out.push((term, value));
        }
    }
    out
}

/// Helper to extract `(term, value)` pairs from a [`crate::MappedDictionary`].
///
/// Walks the trie iteratively (same shape as [`extract_terms`]), collects all
/// final-node terms, then looks up each term's value via
/// `MappedDictionary::get_value`. Terms whose values are unexpectedly `None`
/// at lookup time (which would indicate a soundness bug in the impl) are
/// silently dropped from the resulting vector.
pub fn extract_terms_with_values<D>(dict: &D) -> Vec<(String, D::Value)>
where
    D: crate::MappedDictionary,
    D::Node: DictionaryNode<Unit = u8>,
{
    let terms = extract_terms(dict);
    let mut out = Vec::with_capacity(terms.len());
    for term in terms {
        if let Some(value) = dict.get_value(&term) {
            out.push((term, value));
        }
    }
    out
}

// Implementations of DictionaryFromTerms for each dictionary backend

impl DictionaryFromTerms for crate::double_array_trie::DoubleArrayTrie {
    fn from_terms<I: IntoIterator<Item = String>>(terms: I) -> Self {
        crate::double_array_trie::DoubleArrayTrie::from_terms(terms)
    }
}

impl DictionaryFromTerms for crate::double_array_trie_char::DoubleArrayTrieChar {
    fn from_terms<I: IntoIterator<Item = String>>(terms: I) -> Self {
        crate::double_array_trie_char::DoubleArrayTrieChar::from_terms(terms)
    }
}

impl<V: crate::DictionaryValue> DictionaryFromTerms for crate::dynamic_dawg::DynamicDawg<V> {
    fn from_terms<I: IntoIterator<Item = String>>(terms: I) -> Self {
        crate::dynamic_dawg::DynamicDawg::from_terms(terms)
    }
}

impl<V: crate::DictionaryValue> DictionaryFromTerms
    for crate::dynamic_dawg_char::DynamicDawgChar<V>
{
    fn from_terms<I: IntoIterator<Item = String>>(terms: I) -> Self {
        crate::dynamic_dawg_char::DynamicDawgChar::from_terms(terms)
    }
}

impl<V: crate::DictionaryValue> DictionaryFromTerms for crate::dynamic_dawg_u64::DynamicDawgU64<V> {
    fn from_terms<I: IntoIterator<Item = String>>(terms: I) -> Self {
        // DynamicDawgU64's from_terms accepts strings and converts them internally
        crate::dynamic_dawg_u64::DynamicDawgU64::from_terms(terms)
    }
}

impl<V: crate::DictionaryValue> DictionaryFromTerms
    for crate::suffix_automaton::SuffixAutomaton<V>
{
    fn from_terms<I: IntoIterator<Item = String>>(terms: I) -> Self {
        // SuffixAutomaton uses from_texts which expects source texts
        crate::suffix_automaton::SuffixAutomaton::from_texts(terms)
    }
}

impl<V: crate::DictionaryValue> DictionaryFromTerms
    for crate::suffix_automaton_char::SuffixAutomatonChar<V>
{
    fn from_terms<I: IntoIterator<Item = String>>(terms: I) -> Self {
        crate::suffix_automaton_char::SuffixAutomatonChar::from_texts(terms)
    }
}

impl<V: crate::DictionaryValue> DictionaryFromTerms for crate::scdawg::Scdawg<V> {
    fn from_terms<I: IntoIterator<Item = String>>(terms: I) -> Self {
        crate::scdawg::Scdawg::from_terms(terms)
    }
}

impl<V: crate::DictionaryValue> DictionaryFromTerms for crate::scdawg_char::ScdawgChar<V> {
    fn from_terms<I: IntoIterator<Item = String>>(terms: I) -> Self {
        crate::scdawg_char::ScdawgChar::from_terms(terms)
    }
}

#[cfg(feature = "pathmap-backend")]
impl<V: crate::DictionaryValue + Default> DictionaryFromTerms
    for crate::pathmap::PathMapDictionary<V>
{
    fn from_terms<I: IntoIterator<Item = String>>(terms: I) -> Self {
        crate::pathmap::PathMapDictionary::from_terms(terms)
    }
}

#[cfg(feature = "pathmap-backend")]
impl<V: crate::DictionaryValue + Default> DictionaryFromTerms
    for crate::pathmap_char::PathMapDictionaryChar<V>
{
    fn from_terms<I: IntoIterator<Item = String>>(terms: I) -> Self {
        crate::pathmap_char::PathMapDictionaryChar::from_terms(terms)
    }
}

// =============================================================================
// DictionaryFromTermsWithValues impls
// =============================================================================
//
// Each impl forwards to the backend's inherent `from_terms_with_values` method
// (added in A3, except where it predates this plan). The bound on `V` is
// whatever the backend itself requires for the `MappedDictionary` impl.

impl<V: crate::DictionaryValue> DictionaryFromTermsWithValues
    for crate::double_array_trie::DoubleArrayTrie<V>
{
    type Value = V;

    fn from_terms_with_values<I>(entries: I) -> Self
    where
        I: IntoIterator<Item = (String, Self::Value)>,
    {
        crate::double_array_trie::DoubleArrayTrie::from_terms_with_values(entries)
    }
}

impl<V: crate::DictionaryValue> DictionaryFromTermsWithValues
    for crate::double_array_trie_char::DoubleArrayTrieChar<V>
{
    type Value = V;

    fn from_terms_with_values<I>(entries: I) -> Self
    where
        I: IntoIterator<Item = (String, Self::Value)>,
    {
        crate::double_array_trie_char::DoubleArrayTrieChar::from_terms_with_values(entries)
    }
}

impl<V: crate::DictionaryValue> DictionaryFromTermsWithValues
    for crate::dynamic_dawg::DynamicDawg<V>
{
    type Value = V;

    fn from_terms_with_values<I>(entries: I) -> Self
    where
        I: IntoIterator<Item = (String, Self::Value)>,
    {
        crate::dynamic_dawg::DynamicDawg::from_terms_with_values(entries)
    }
}

impl<V: crate::DictionaryValue> DictionaryFromTermsWithValues
    for crate::dynamic_dawg_char::DynamicDawgChar<V>
{
    type Value = V;

    fn from_terms_with_values<I>(entries: I) -> Self
    where
        I: IntoIterator<Item = (String, Self::Value)>,
    {
        crate::dynamic_dawg_char::DynamicDawgChar::from_terms_with_values(entries)
    }
}

impl<V: crate::DictionaryValue> DictionaryFromTermsWithValues
    for crate::dynamic_dawg_u64::DynamicDawgU64<V>
{
    type Value = V;

    fn from_terms_with_values<I>(entries: I) -> Self
    where
        I: IntoIterator<Item = (String, Self::Value)>,
    {
        crate::dynamic_dawg_u64::DynamicDawgU64::from_terms_with_values(entries)
    }
}

impl<V: crate::DictionaryValue> DictionaryFromTermsWithValues for crate::scdawg::Scdawg<V> {
    type Value = V;

    fn from_terms_with_values<I>(entries: I) -> Self
    where
        I: IntoIterator<Item = (String, Self::Value)>,
    {
        crate::scdawg::Scdawg::from_terms_with_values(entries)
    }
}

impl<V: crate::DictionaryValue> DictionaryFromTermsWithValues
    for crate::scdawg_char::ScdawgChar<V>
{
    type Value = V;

    fn from_terms_with_values<I>(entries: I) -> Self
    where
        I: IntoIterator<Item = (String, Self::Value)>,
    {
        crate::scdawg_char::ScdawgChar::from_terms_with_values(entries)
    }
}

#[cfg(feature = "pathmap-backend")]
impl<V: crate::DictionaryValue + Default> DictionaryFromTermsWithValues
    for crate::pathmap::PathMapDictionary<V>
{
    type Value = V;

    fn from_terms_with_values<I>(entries: I) -> Self
    where
        I: IntoIterator<Item = (String, Self::Value)>,
    {
        crate::pathmap::PathMapDictionary::from_terms_with_values(entries)
    }
}

#[cfg(feature = "pathmap-backend")]
impl<V: crate::DictionaryValue + Default> DictionaryFromTermsWithValues
    for crate::pathmap_char::PathMapDictionaryChar<V>
{
    type Value = V;

    fn from_terms_with_values<I>(entries: I) -> Self
    where
        I: IntoIterator<Item = (String, Self::Value)>,
    {
        crate::pathmap_char::PathMapDictionaryChar::from_terms_with_values(entries)
    }
}

// Tests
#[cfg(test)]
mod tests {
    use super::*;
    use crate::double_array_trie::DoubleArrayTrie;

    #[test]
    fn test_bincode_roundtrip() {
        let dict = DoubleArrayTrie::from_terms(vec!["hello", "world", "test"]);
        let mut buffer = Vec::new();

        BincodeSerializer::serialize(&dict, &mut buffer).unwrap();
        let loaded: DoubleArrayTrie = BincodeSerializer::deserialize(&buffer[..]).unwrap();

        assert!(loaded.contains("hello"));
        assert!(loaded.contains("world"));
        assert!(loaded.contains("test"));
        assert!(!loaded.contains("missing"));
    }

    #[test]
    fn test_json_roundtrip() {
        let dict = DoubleArrayTrie::from_terms(vec!["alpha", "beta", "gamma"]);
        let mut buffer = Vec::new();

        JsonSerializer::serialize(&dict, &mut buffer).unwrap();
        let loaded: DoubleArrayTrie = JsonSerializer::deserialize(&buffer[..]).unwrap();

        assert!(loaded.contains("alpha"));
        assert!(loaded.contains("beta"));
        assert!(loaded.contains("gamma"));
        assert!(!loaded.contains("delta"));
    }

    #[test]
    fn test_extract_terms() {
        let dict = DoubleArrayTrie::from_terms(vec!["apple", "apply", "application"]);
        let terms = extract_terms(&dict);

        assert_eq!(terms.len(), 3);
        assert!(terms.contains(&"apple".to_string()));
        assert!(terms.contains(&"apply".to_string()));
        assert!(terms.contains(&"application".to_string()));
    }

    #[test]
    fn test_extract_terms_deep_chain_does_not_stack_overflow() {
        // Pathological single-child chain — a long all-'a' term forms an
        // N-edge path in the trie. The previous recursive `dfs` would
        // overflow the ~8MB default thread stack at this depth (each frame
        // ~100 bytes); the iterative form survives. We pick 1024 because
        // DoubleArrayTrie's internal arena sizes itself for typical
        // dictionaries; the goal of this test is to exercise iterative
        // traversal under a long single-child chain, not to stress DAT.
        const DEPTH: usize = 1024;
        let long_term: String = std::iter::repeat('a').take(DEPTH).collect();

        let dict = DoubleArrayTrie::from_terms(vec![long_term.clone()]);
        let terms = extract_terms(&dict);

        assert_eq!(terms.len(), 1, "expected exactly one term; got {:?}", terms);
        assert_eq!(terms[0].len(), DEPTH);
        assert_eq!(terms[0], long_term);
    }

    #[test]
    fn test_extract_terms_deep_chain_dynamic_dawg() {
        // Same pathological case but via DynamicDawg, which doesn't have
        // DAT's arena-size constraints. We pick 50k to actually demonstrate
        // the stack-safety property the iterative rewrite was needed for.
        use crate::dynamic_dawg::DynamicDawg;

        const DEPTH: usize = 50_000;
        let long_term: String = std::iter::repeat('a').take(DEPTH).collect();

        let dict: DynamicDawg<()> = DynamicDawg::from_terms(vec![long_term.clone()]);
        let terms = extract_terms(&dict);

        assert_eq!(
            terms.len(),
            1,
            "expected exactly one term; got {:?} entries",
            terms.len()
        );
        assert_eq!(terms[0].len(), DEPTH);
        assert_eq!(terms[0], long_term);
    }

    #[test]
    fn test_suffix_automaton_serialization() {
        use crate::suffix_automaton::SuffixAutomaton;

        let texts = vec!["hello world".to_string(), "test string".to_string()];
        let dict = SuffixAutomaton::from_texts(texts.clone());

        // Test bincode serialization using specialized methods
        let mut buffer = Vec::new();
        BincodeSerializer::serialize_suffix_automaton(&dict, &mut buffer).unwrap();

        let loaded = BincodeSerializer::deserialize_suffix_automaton(&buffer[..]).unwrap();

        // Verify the loaded automaton works
        assert!(loaded.contains("hello"));
        assert!(loaded.contains("world"));
        assert!(loaded.contains("test"));
        assert!(loaded.contains("string"));
        assert!(!loaded.contains("missing"));

        // Verify source texts are preserved
        let sources = loaded.source_texts();
        assert_eq!(sources.len(), 2);
        assert!(sources.contains(&"hello world".to_string()));
        assert!(sources.contains(&"test string".to_string()));
    }

    #[cfg(feature = "protobuf")]
    #[test]
    fn test_suffix_automaton_protobuf_serialization() {
        use crate::serialization::SuffixAutomatonProtobufSerializer;
        use crate::suffix_automaton::SuffixAutomaton;

        let texts = vec!["hello world".to_string(), "test string".to_string()];
        let dict = SuffixAutomaton::from_texts(texts.clone());

        // Test protobuf serialization
        let mut buffer = Vec::new();
        SuffixAutomatonProtobufSerializer::serialize_suffix_automaton(&dict, &mut buffer).unwrap();

        let loaded =
            SuffixAutomatonProtobufSerializer::deserialize_suffix_automaton(&buffer[..]).unwrap();

        // Verify the loaded automaton works
        assert!(loaded.contains("hello"));
        assert!(loaded.contains("world"));
        assert!(loaded.contains("test"));
        assert!(loaded.contains("string"));
        assert!(!loaded.contains("missing"));

        // Verify source texts are preserved
        let sources = loaded.source_texts();
        assert_eq!(sources.len(), 2);
        assert!(sources.contains(&"hello world".to_string()));
        assert!(sources.contains(&"test string".to_string()));
    }

    #[cfg(feature = "protobuf")]
    #[test]
    fn test_dat_protobuf_serialization() {
        use crate::serialization::DatProtobufSerializer;

        let dict = DoubleArrayTrie::from_terms(vec!["apple", "apply", "application"]);

        // Test protobuf serialization
        let mut buffer = Vec::new();
        DatProtobufSerializer::serialize_dat(&dict, &mut buffer).unwrap();

        let loaded = DatProtobufSerializer::deserialize_dat(&buffer[..]).unwrap();

        // Verify the loaded dictionary works
        assert!(loaded.contains("apple"));
        assert!(loaded.contains("apply"));
        assert!(loaded.contains("application"));
        assert!(!loaded.contains("app"));
        assert!(!loaded.contains("banana"));
    }

    #[cfg(feature = "protobuf")]
    #[test]
    fn test_protobuf_roundtrip() {
        let dict = DoubleArrayTrie::from_terms(vec!["test", "testing", "tested"]);
        let mut buffer = Vec::new();

        ProtobufSerializer::serialize(&dict, &mut buffer).unwrap();
        let loaded: DoubleArrayTrie = ProtobufSerializer::deserialize(&buffer[..]).unwrap();

        assert!(loaded.contains("test"));
        assert!(loaded.contains("testing"));
        assert!(loaded.contains("tested"));
        assert!(!loaded.contains("tester"));
    }

    #[cfg(feature = "protobuf")]
    #[test]
    fn test_optimized_protobuf_roundtrip() {
        let dict = DoubleArrayTrie::from_terms(vec!["alpha", "beta", "gamma"]);
        let mut buffer = Vec::new();

        OptimizedProtobufSerializer::serialize(&dict, &mut buffer).unwrap();
        let loaded: DoubleArrayTrie =
            OptimizedProtobufSerializer::deserialize(&buffer[..]).unwrap();

        assert!(loaded.contains("alpha"));
        assert!(loaded.contains("beta"));
        assert!(loaded.contains("gamma"));
        assert!(!loaded.contains("delta"));
    }

    #[cfg(feature = "protobuf")]
    #[test]
    fn test_protobuf_format_comparison() {
        // Compare serialization sizes for different protobuf formats
        let dict = DoubleArrayTrie::from_terms(vec![
            "test",
            "testing",
            "tested",
            "tester",
            "tests",
            "apple",
            "apply",
            "application",
            "applicable",
        ]);

        // Standard ProtobufSerializer (V1 format)
        let mut buf_v1 = Vec::new();
        ProtobufSerializer::serialize(&dict, &mut buf_v1).unwrap();

        // OptimizedProtobufSerializer (V2 format)
        let mut buf_v2 = Vec::new();
        OptimizedProtobufSerializer::serialize(&dict, &mut buf_v2).unwrap();

        // DatProtobufSerializer (term extraction)
        let mut buf_dat = Vec::new();
        DatProtobufSerializer::serialize_dat(&dict, &mut buf_dat).unwrap();

        // V2 should be smaller than V1 (delta encoding + packed format)
        assert!(
            buf_v2.len() < buf_v1.len(),
            "V2 ({} bytes) should be smaller than V1 ({} bytes)",
            buf_v2.len(),
            buf_v1.len()
        );

        // DAT format should be competitive
        println!("Protobuf V1 size: {} bytes", buf_v1.len());
        println!("Protobuf V2 size: {} bytes", buf_v2.len());
        println!("DAT format size: {} bytes", buf_dat.len());

        // Verify all formats deserialize correctly
        let loaded_v1: DoubleArrayTrie = ProtobufSerializer::deserialize(&buf_v1[..]).unwrap();
        let loaded_v2: DoubleArrayTrie =
            OptimizedProtobufSerializer::deserialize(&buf_v2[..]).unwrap();
        let loaded_dat = DatProtobufSerializer::deserialize_dat(&buf_dat[..]).unwrap();

        for term in ["test", "testing", "apple", "application"] {
            assert!(loaded_v1.contains(term));
            assert!(loaded_v2.contains(term));
            assert!(loaded_dat.contains(term));
        }
    }
}
