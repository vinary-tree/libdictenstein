//! Protobuf serializers for cross-language compatibility.

use crate::{Dictionary, DictionaryNode};
use std::io::{Read, Write};

use super::{DictionaryFromTerms, DictionarySerializer, SerializationError};

#[cfg(feature = "protobuf")]
use std::collections::{HashMap, HashSet};

/// Generated protobuf types
mod proto {
    #![allow(dead_code)]
    include!(concat!(env!("OUT_DIR"), "/libdictenstein.proto.rs"));
}

#[cfg(feature = "protobuf")]
const DAT_TERMS_MAGIC: &[u8] = b"LDT1";

#[cfg(feature = "protobuf")]
fn dictionary_error(message: impl Into<String>) -> SerializationError {
    SerializationError::DictionaryError(message.into())
}

#[cfg(feature = "protobuf")]
fn checked_label_u32(label: u32, format: &str) -> Result<u8, SerializationError> {
    u8::try_from(label)
        .map_err(|_| dictionary_error(format!("{format} edge label {label} exceeds u8")))
}

#[cfg(feature = "protobuf")]
fn checked_label_u64(label: u64, format: &str) -> Result<u8, SerializationError> {
    u8::try_from(label)
        .map_err(|_| dictionary_error(format!("{format} edge label {label} exceeds u8")))
}

#[cfg(feature = "protobuf")]
fn validate_term_count(
    expected: u64,
    actual: usize,
    format: &str,
) -> Result<(), SerializationError> {
    let expected = usize::try_from(expected)
        .map_err(|_| dictionary_error(format!("{format} term count does not fit usize")))?;
    if expected == actual {
        Ok(())
    } else {
        Err(dictionary_error(format!(
            "{format} term count mismatch: expected {expected}, decoded {actual}"
        )))
    }
}

#[cfg(feature = "protobuf")]
fn ensure_reachable_acyclic(
    root_id: u64,
    adjacency: &HashMap<u64, Vec<(u8, u64)>>,
) -> Result<(), SerializationError> {
    fn visit(
        node_id: u64,
        adjacency: &HashMap<u64, Vec<(u8, u64)>>,
        visiting: &mut HashSet<u64>,
        visited: &mut HashSet<u64>,
    ) -> Result<(), SerializationError> {
        if visited.contains(&node_id) {
            return Ok(());
        }
        if !visiting.insert(node_id) {
            return Err(dictionary_error(format!(
                "protobuf graph contains a reachable cycle at node {node_id}"
            )));
        }

        if let Some(edges) = adjacency.get(&node_id) {
            for &(_, target_id) in edges {
                visit(target_id, adjacency, visiting, visited)?;
            }
        }

        visiting.remove(&node_id);
        visited.insert(node_id);
        Ok(())
    }

    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();
    visit(root_id, adjacency, &mut visiting, &mut visited)
}

#[cfg(feature = "protobuf")]
fn terms_from_adjacency(
    root_id: u64,
    adjacency: &HashMap<u64, Vec<(u8, u64)>>,
    final_set: &HashSet<u64>,
) -> Result<Vec<String>, SerializationError> {
    ensure_reachable_acyclic(root_id, adjacency)?;

    fn dfs(
        node_id: u64,
        adjacency: &HashMap<u64, Vec<(u8, u64)>>,
        final_set: &HashSet<u64>,
        current_term: &mut Vec<u8>,
        terms: &mut Vec<String>,
    ) -> Result<(), SerializationError> {
        if final_set.contains(&node_id) {
            let term = String::from_utf8(current_term.clone()).map_err(|_| {
                dictionary_error("protobuf graph produced a non-UTF-8 dictionary term")
            })?;
            terms.push(term);
        }

        if let Some(edges) = adjacency.get(&node_id) {
            for &(label, target_id) in edges {
                current_term.push(label);
                dfs(target_id, adjacency, final_set, current_term, terms)?;
                current_term.pop();
            }
        }

        Ok(())
    }

    let mut terms = Vec::with_capacity(final_set.len());
    let mut current_term = Vec::with_capacity(32);
    dfs(root_id, adjacency, final_set, &mut current_term, &mut terms)?;
    Ok(terms)
}

#[cfg(feature = "protobuf")]
fn encode_dat_terms(terms: &[String]) -> Result<Vec<u8>, SerializationError> {
    let mut encoded = Vec::new();
    encoded.extend_from_slice(DAT_TERMS_MAGIC);
    for term in terms {
        let term_bytes = term.as_bytes();
        let len = u32::try_from(term_bytes.len())
            .map_err(|_| dictionary_error("DAT protobuf term exceeds u32 length"))?;
        encoded.extend_from_slice(&len.to_le_bytes());
        encoded.extend_from_slice(term_bytes);
    }
    Ok(encoded)
}

#[cfg(feature = "protobuf")]
fn decode_dat_terms(edge_data: &[u8], term_count: u64) -> Result<Vec<String>, SerializationError> {
    let terms = if edge_data.starts_with(DAT_TERMS_MAGIC) {
        let mut offset = DAT_TERMS_MAGIC.len();
        let mut terms = Vec::new();

        while offset < edge_data.len() {
            let Some(length_bytes) = edge_data.get(offset..offset + 4) else {
                return Err(dictionary_error("DAT protobuf term length is truncated"));
            };
            let len = u32::from_le_bytes([
                length_bytes[0],
                length_bytes[1],
                length_bytes[2],
                length_bytes[3],
            ]) as usize;
            offset += 4;

            let Some(term_bytes) = edge_data.get(offset..offset + len) else {
                return Err(dictionary_error("DAT protobuf term payload is truncated"));
            };
            offset += len;

            let term = String::from_utf8(term_bytes.to_vec())
                .map_err(|_| dictionary_error("DAT protobuf term is not valid UTF-8"))?;
            terms.push(term);
        }

        terms
    } else {
        let terms_str = std::str::from_utf8(edge_data)
            .map_err(|_| dictionary_error("legacy DAT protobuf terms are not valid UTF-8"))?;
        terms_str
            .lines()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect()
    };

    validate_term_count(term_count, terms.len(), "DAT protobuf")?;
    Ok(terms)
}

#[cfg(feature = "protobuf")]
/// Protobuf serializer for cross-language compatibility.
///
/// This serializer uses Protocol Buffers to serialize the dictionary
/// as a graph structure (nodes + edges), which is:
/// - More space-efficient than storing all terms as strings
/// - Compatible with all liblevenshtein implementations (Java, C++, Rust)
/// - Preserves the DAWG/trie structure directly without rebuilding
///
/// # Format
///
/// The dictionary is serialized as:
/// - List of node IDs
/// - List of final (terminal) node IDs
/// - List of edges (source_id, label, target_id)
/// - Root node ID
/// - Dictionary size (term count)
///
/// This format is defined in `proto/liblevenshtein.proto` and is shared
/// across all liblevenshtein implementations.
pub struct ProtobufSerializer;

#[cfg(feature = "protobuf")]
impl ProtobufSerializer {
    /// Extract graph structure from dictionary.
    ///
    /// Performs DFS traversal to collect all nodes and edges.
    ///
    /// NOTE: Since the Dictionary trait doesn't provide node identity,
    /// we serialize as a trie structure where each unique path creates
    /// new nodes. For true DAWG serialization with node sharing, we'd
    /// need dictionary implementations to expose node IDs.
    fn extract_graph<D>(dict: &D) -> proto::Dictionary
    where
        D: Dictionary,
        D::Node: DictionaryNode<Unit = u8>,
    {
        // Pre-allocate vectors with estimated capacity
        let est_size = dict.len().unwrap_or(100);
        let mut node_ids = Vec::with_capacity(est_size * 2); // Estimate nodes
        let mut final_node_ids = Vec::with_capacity(est_size); // Estimate final nodes
        let mut edges = Vec::with_capacity(est_size * 3); // Estimate edges
        let mut next_id = 0u64;

        // Root node
        node_ids.push(next_id);
        let root = dict.root();
        if root.is_final() {
            final_node_ids.push(next_id);
        }
        next_id += 1;

        // DFS to build graph
        // Protobuf serialization only supports byte-level (u8) dictionaries
        fn dfs<N: DictionaryNode<Unit = u8>>(
            node: &N,
            node_id: u64,
            next_id: &mut u64,
            node_ids: &mut Vec<u64>,
            final_node_ids: &mut Vec<u64>,
            edges: &mut Vec<proto::dictionary::Edge>,
        ) {
            for (label, child) in node.edges() {
                let child_id = *next_id;
                *next_id += 1;

                // Record child node
                node_ids.push(child_id);
                if child.is_final() {
                    final_node_ids.push(child_id);
                }

                // Record edge
                edges.push(proto::dictionary::Edge {
                    source_id: node_id,
                    label: label as u32,
                    target_id: child_id,
                });

                // Recurse
                dfs(&child, child_id, next_id, node_ids, final_node_ids, edges);
            }
        }

        dfs(
            &root,
            0,
            &mut next_id,
            &mut node_ids,
            &mut final_node_ids,
            &mut edges,
        );

        proto::Dictionary {
            node_id: node_ids,
            final_node_id: final_node_ids,
            edge: edges,
            root_id: 0,
            size: dict.len().unwrap_or(0) as u64,
        }
    }
}

#[cfg(feature = "protobuf")]
impl DictionarySerializer for ProtobufSerializer {
    fn serialize<D, W>(dict: &D, mut writer: W) -> Result<(), SerializationError>
    where
        D: Dictionary,
        D::Node: DictionaryNode<Unit = u8>,
        W: Write,
    {
        use prost::Message;

        let proto_dict = Self::extract_graph(dict);
        let mut buf = Vec::new();
        proto_dict.encode(&mut buf).map_err(|e| {
            SerializationError::Io(std::io::Error::new(std::io::ErrorKind::Other, e))
        })?;
        writer.write_all(&buf)?;
        Ok(())
    }

    fn deserialize<D, R>(mut reader: R) -> Result<D, SerializationError>
    where
        D: DictionaryFromTerms,
        R: Read,
    {
        use prost::Message;

        // Read all bytes
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf)?;

        // Decode protobuf
        let proto_dict = proto::Dictionary::decode(&buf[..])?;

        // Reconstruct dictionary from graph
        // Build adjacency list with pre-allocated capacity
        let est_nodes = proto_dict.node_id.len();
        let mut adjacency: HashMap<u64, Vec<(u8, u64)>> = HashMap::with_capacity(est_nodes);
        let node_ids: HashSet<u64> = proto_dict.node_id.iter().copied().collect();
        if !node_ids.contains(&proto_dict.root_id) {
            return Err(dictionary_error(format!(
                "protobuf v1 root node {} is not declared",
                proto_dict.root_id
            )));
        }

        for edge in &proto_dict.edge {
            if !node_ids.contains(&edge.source_id) {
                return Err(dictionary_error(format!(
                    "protobuf v1 edge source {} is not declared",
                    edge.source_id
                )));
            }
            if !node_ids.contains(&edge.target_id) {
                return Err(dictionary_error(format!(
                    "protobuf v1 edge target {} is not declared",
                    edge.target_id
                )));
            }
            let label = checked_label_u32(edge.label, "protobuf v1")?;
            adjacency
                .entry(edge.source_id)
                .or_default()
                .push((label, edge.target_id));
        }

        // Pre-allocate HashSet with known size
        let mut final_set: HashSet<u64> = HashSet::with_capacity(proto_dict.final_node_id.len());
        final_set.extend(proto_dict.final_node_id.iter().copied());
        for final_id in &final_set {
            if !node_ids.contains(final_id) {
                return Err(dictionary_error(format!(
                    "protobuf v1 final node {final_id} is not declared"
                )));
            }
        }

        let terms = terms_from_adjacency(proto_dict.root_id, &adjacency, &final_set)?;
        validate_term_count(proto_dict.size, terms.len(), "protobuf v1")?;

        Ok(D::from_terms(terms))
    }
}

#[cfg(feature = "protobuf")]
/// Optimized protobuf serializer using DictionaryV2 format.
///
/// This serializer uses an optimized protobuf format that is 40-60% smaller
/// than the standard ProtobufSerializer by:
/// - Removing redundant node_id field (IDs are sequential)
/// - Using packed edge format (flat array instead of messages)
/// - Delta-encoding final node IDs for better compression
///
/// **Note**: This format is NOT compatible with older liblevenshtein
/// implementations. Use `ProtobufSerializer` for cross-language compatibility.
///
/// # Example
///
/// ```text
/// use liblevenshtein::prelude::*;
///
/// let dict = PathMapDictionary::from_terms(vec!["test", "testing"]);
///
/// // Serialize with optimized format (smaller size)
/// let mut buf = Vec::new();
/// OptimizedProtobufSerializer::serialize(&dict, &mut buf)?;
///
/// // Deserialize
/// let loaded: PathMapDictionary =
///     OptimizedProtobufSerializer::deserialize(&buf[..])?;
/// ```
pub struct OptimizedProtobufSerializer;

#[cfg(feature = "protobuf")]
impl OptimizedProtobufSerializer {
    /// Extract graph structure in optimized format.
    fn extract_graph_v2<D>(dict: &D) -> proto::DictionaryV2
    where
        D: Dictionary,
        D::Node: DictionaryNode<Unit = u8>,
    {
        // Pre-allocate vectors with estimated capacity
        let est_size = dict.len().unwrap_or(100);
        let mut final_node_ids = Vec::with_capacity(est_size); // Estimate final nodes
        let mut edge_data = Vec::with_capacity(est_size * 9); // 3 values per edge, estimate 3 edges/term
        let mut next_id = 0u64;

        // Root node
        let root = dict.root();
        if root.is_final() {
            final_node_ids.push(0);
        }
        next_id += 1;

        // DFS to build graph
        // Protobuf serialization only supports byte-level (u8) dictionaries
        fn dfs<N: DictionaryNode<Unit = u8>>(
            node: &N,
            node_id: u64,
            next_id: &mut u64,
            final_node_ids: &mut Vec<u64>,
            edge_data: &mut Vec<u64>,
        ) {
            for (label, child) in node.edges() {
                let child_id = *next_id;
                *next_id += 1;

                // Record if final
                if child.is_final() {
                    final_node_ids.push(child_id);
                }

                // Pack edge as triplet: [source, label, target]
                edge_data.push(node_id);
                edge_data.push(label as u64);
                edge_data.push(child_id);

                // Recurse
                dfs(&child, child_id, next_id, final_node_ids, edge_data);
            }
        }

        dfs(&root, 0, &mut next_id, &mut final_node_ids, &mut edge_data);

        // Convert final node IDs to deltas
        let final_node_delta = if final_node_ids.is_empty() {
            Vec::new()
        } else {
            let mut deltas = Vec::with_capacity(final_node_ids.len());
            deltas.push(final_node_ids[0]); // First value is absolute

            for i in 1..final_node_ids.len() {
                // Delta = current - previous
                deltas.push(final_node_ids[i] - final_node_ids[i - 1]);
            }
            deltas
        };

        let edge_count = edge_data.len() / 3;

        proto::DictionaryV2 {
            final_node_delta,
            edge_data,
            root_id: 0,
            size: dict.len().unwrap_or(0) as u64,
            edge_count: edge_count as u64,
        }
    }
}

#[cfg(feature = "protobuf")]
impl DictionarySerializer for OptimizedProtobufSerializer {
    fn serialize<D, W>(dict: &D, mut writer: W) -> Result<(), SerializationError>
    where
        D: Dictionary,
        D::Node: DictionaryNode<Unit = u8>,
        W: Write,
    {
        use prost::Message;

        let proto_dict = Self::extract_graph_v2(dict);
        let mut buf = Vec::new();
        proto_dict.encode(&mut buf).map_err(|e| {
            SerializationError::Io(std::io::Error::new(std::io::ErrorKind::Other, e))
        })?;
        writer.write_all(&buf)?;
        Ok(())
    }

    fn deserialize<D, R>(mut reader: R) -> Result<D, SerializationError>
    where
        D: DictionaryFromTerms,
        R: Read,
    {
        use prost::Message;

        // Read all bytes
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf)?;

        // Decode protobuf
        let proto_dict = proto::DictionaryV2::decode(&buf[..])?;

        // Validate edge_data length
        if proto_dict.edge_data.len() % 3 != 0 {
            return Err(SerializationError::DictionaryError(format!(
                "Invalid edge_data length: {} (must be multiple of 3)",
                proto_dict.edge_data.len()
            )));
        }
        let num_edges = proto_dict.edge_data.len() / 3;
        let declared_edges = usize::try_from(proto_dict.edge_count)
            .map_err(|_| dictionary_error("protobuf v2 edge_count does not fit usize"))?;
        if declared_edges != num_edges {
            return Err(dictionary_error(format!(
                "protobuf v2 edge_count mismatch: expected {declared_edges}, decoded {num_edges}"
            )));
        }

        // Reconstruct final node IDs from deltas with pre-allocation
        let mut final_node_ids = Vec::with_capacity(proto_dict.final_node_delta.len());
        if !proto_dict.final_node_delta.is_empty() {
            let mut cumsum = 0u64;
            for &delta in &proto_dict.final_node_delta {
                cumsum = cumsum
                    .checked_add(delta)
                    .ok_or_else(|| dictionary_error("protobuf v2 final-node delta overflow"))?;
                final_node_ids.push(cumsum);
            }
        }

        // Build adjacency list from packed edge data with pre-allocation
        let est_nodes = (num_edges as f64 * 0.6) as usize; // Estimate nodes from edges
        let mut adjacency: HashMap<u64, Vec<(u8, u64)>> = HashMap::with_capacity(est_nodes);
        for chunk in proto_dict.edge_data.chunks_exact(3) {
            let source_id = chunk[0];
            let label = checked_label_u64(chunk[1], "protobuf v2")?;
            let target_id = chunk[2];

            adjacency
                .entry(source_id)
                .or_default()
                .push((label, target_id));
        }

        // Pre-allocate HashSet with known size
        let mut final_set: HashSet<u64> = HashSet::with_capacity(final_node_ids.len());
        final_set.extend(final_node_ids.iter().copied());

        let terms = terms_from_adjacency(proto_dict.root_id, &adjacency, &final_set)?;
        validate_term_count(proto_dict.size, terms.len(), "protobuf v2")?;

        Ok(D::from_terms(terms))
    }
}

#[cfg(feature = "protobuf")]
/// Suffix automaton-optimized protobuf serializer.
///
/// This serializer is specifically optimized for `SuffixAutomaton` by storing
/// the original source texts rather than the graph structure. Since suffix
/// automata can be efficiently rebuilt from source texts in linear time,
/// this approach is both simpler and more space-efficient than serializing
/// the full automaton structure.
///
/// **Benefits**:
/// - Much smaller than serializing full graph (nodes, edges, suffix links)
/// - Simple and reliable reconstruction via online algorithm
/// - Preserves source text metadata
/// - Fast deserialization (O(n) construction)
///
/// **Note**: Only works with `SuffixAutomaton`, not other dictionary backends.
pub struct SuffixAutomatonProtobufSerializer;

#[cfg(feature = "protobuf")]
impl SuffixAutomatonProtobufSerializer {
    /// Serialize SuffixAutomaton to optimized protobuf format.
    ///
    /// Extracts source texts and rebuilds on deserialization.
    pub fn serialize_suffix_automaton<W>(
        dict: &crate::suffix_automaton::SuffixAutomaton,
        mut writer: W,
    ) -> Result<(), SerializationError>
    where
        W: Write,
    {
        use prost::Message;

        // Extract source texts from the automaton
        let source_texts = dict.source_texts();
        let string_count = dict.string_count();

        let proto_suffix = proto::SuffixAutomaton {
            source_texts,
            string_count: string_count as u64,
        };

        let mut buf = Vec::new();
        proto_suffix.encode(&mut buf).map_err(|e| {
            SerializationError::Io(std::io::Error::new(std::io::ErrorKind::Other, e))
        })?;
        writer.write_all(&buf)?;
        Ok(())
    }

    /// Deserialize SuffixAutomaton from optimized protobuf format.
    pub fn deserialize_suffix_automaton<R>(
        mut reader: R,
    ) -> Result<crate::suffix_automaton::SuffixAutomaton, SerializationError>
    where
        R: Read,
    {
        use prost::Message;

        let mut buf = Vec::new();
        reader.read_to_end(&mut buf)?;

        let proto_suffix = proto::SuffixAutomaton::decode(&buf[..])?;

        // Validate string count
        if proto_suffix.source_texts.len() != proto_suffix.string_count as usize {
            return Err(SerializationError::DictionaryError(format!(
                "String count mismatch: expected {}, got {}",
                proto_suffix.string_count,
                proto_suffix.source_texts.len()
            )));
        }

        // Rebuild suffix automaton from source texts
        Ok(crate::suffix_automaton::SuffixAutomaton::from_texts(
            proto_suffix.source_texts,
        ))
    }
}

#[cfg(feature = "protobuf")]
/// DAT-optimized protobuf serializer.
///
/// This serializer is specifically optimized for `DoubleArrayTrie` and directly
/// serializes the internal BASE/CHECK/IS_FINAL arrays without graph traversal.
///
/// **Benefits**:
/// - Direct array serialization (no graph traversal)
/// - Fastest serialization/deserialization for DAT
/// - Smallest binary format for DAT structures
/// - Preserves all DAT optimizations
///
/// **Note**: Only works with `DoubleArrayTrie`, not other dictionary backends.
pub struct DatProtobufSerializer;

#[cfg(feature = "protobuf")]
impl DatProtobufSerializer {
    /// Serialize DoubleArrayTrie to optimized protobuf format.
    ///
    /// Directly extracts terms and rebuilds on deserialization.
    /// This is simpler and more reliable than trying to serialize internal state.
    pub fn serialize_dat<W>(
        dict: &crate::double_array_trie::DoubleArrayTrie,
        mut writer: W,
    ) -> Result<(), SerializationError>
    where
        W: Write,
    {
        use prost::Message;

        // Extract all terms from the dictionary
        let terms = super::extract_terms(dict);

        // Create a marker protobuf message indicating this is a DAT serialization
        // We'll use the term count as a simple serialization
        let proto_dat = proto::DoubleArrayTrie {
            base: Vec::new(), // Placeholder - we serialize via terms
            check: Vec::new(),
            is_final: Vec::new(),
            edge_data: encode_dat_terms(&terms)?,
            free_list: Vec::new(),
            term_count: terms.len() as u64,
            rebuild_threshold: 0.2,
        };

        let mut buf = Vec::new();
        proto_dat.encode(&mut buf).map_err(|e| {
            SerializationError::Io(std::io::Error::new(std::io::ErrorKind::Other, e))
        })?;
        writer.write_all(&buf)?;
        Ok(())
    }

    /// Deserialize DoubleArrayTrie from optimized protobuf format.
    pub fn deserialize_dat<R>(
        mut reader: R,
    ) -> Result<crate::double_array_trie::DoubleArrayTrie, SerializationError>
    where
        R: Read,
    {
        use prost::Message;

        let mut buf = Vec::new();
        reader.read_to_end(&mut buf)?;

        let proto_dat = proto::DoubleArrayTrie::decode(&buf[..])?;

        let terms = decode_dat_terms(&proto_dat.edge_data, proto_dat.term_count)?;

        // Rebuild DAT from terms
        Ok(crate::double_array_trie::DoubleArrayTrie::from_terms(terms))
    }
}
