//! Sequence-keyed persistent ARTrie facade for `u64` units.
//!
//! The on-disk representation is the established byte persistent ARTrie with
//! each public `u64` unit encoded as eight little-endian bytes. This preserves
//! the existing swizzled-pointer state machine, WAL, checkpoint/reopen, and CX
//! checkpoint compression path while exposing u64-native sequence operations.

use std::path::Path;

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::disk_manager::MmapDiskManager;
use crate::persistent_artrie::error::Result;
use crate::persistent_artrie::{PersistentARTrie, PersistentARTrieNode, RecoveryReport};
use crate::value::DictionaryValue;
use crate::{
    CharUnit, Dictionary, DictionaryNode, MappedDictionary, MappedDictionaryNode,
    MutableDictionary, MutableMappedDictionary, SyncStrategy,
};

/// Persistent ARTrie keyed by `u64` sequences.
pub struct PersistentARTrieU64<V: DictionaryValue = (), S: BlockStorage = MmapDiskManager> {
    inner: PersistentARTrie<V, S>,
}

/// Node handle for [`PersistentARTrieU64`].
#[derive(Clone, Debug)]
pub struct PersistentARTrieU64Node<V: DictionaryValue = ()> {
    inner: PersistentARTrieNode<V>,
    path: Vec<u64>,
}

fn encode_sequence(sequence: &[u64]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(sequence.len() * 8);
    for unit in sequence {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

fn decode_sequence(bytes: &[u8]) -> Option<Vec<u64>> {
    if bytes.len() % 8 != 0 {
        return None;
    }
    Some(
        bytes
            .chunks_exact(8)
            .map(|chunk| {
                let mut word = [0u8; 8];
                word.copy_from_slice(chunk);
                u64::from_le_bytes(word)
            })
            .collect(),
    )
}

fn collect_u64_edges<V: DictionaryValue>(
    node: &PersistentARTrieNode<V>,
    depth: usize,
    bytes: &mut [u8; 8],
    out: &mut Vec<(u64, PersistentARTrieNode<V>)>,
) {
    for (byte, child) in node.edges() {
        bytes[depth] = byte;
        if depth == 7 {
            out.push((u64::from_le_bytes(*bytes), child));
        } else {
            collect_u64_edges(&child, depth + 1, bytes, out);
        }
    }
}

impl<V: DictionaryValue> PersistentARTrieU64<V> {
    /// Create an in-memory persistent u64 trie.
    pub fn new() -> Self {
        #[allow(deprecated)]
        let inner = PersistentARTrie::new();
        Self { inner }
    }

    pub fn from_sequences<I, T>(sequences: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: AsRef<[u64]>,
    {
        let trie = Self::new();
        for sequence in sequences {
            trie.insert_sequence(sequence.as_ref());
        }
        trie
    }

    pub fn from_sequences_with_values<I, T>(entries: I) -> Self
    where
        I: IntoIterator<Item = (T, V)>,
        T: AsRef<[u64]>,
    {
        let trie = Self::new();
        for (sequence, value) in entries {
            trie.insert_sequence_with_value(sequence.as_ref(), value);
        }
        trie
    }

    pub fn from_terms<I, T>(terms: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: AsRef<str>,
    {
        let trie = Self::new();
        for term in terms {
            trie.insert(term.as_ref());
        }
        trie
    }

    pub fn from_terms_with_values<I, T>(entries: I) -> Self
    where
        I: IntoIterator<Item = (T, V)>,
        T: AsRef<str>,
    {
        let trie = Self::new();
        for (term, value) in entries {
            trie.insert_with_value(term.as_ref(), value);
        }
        trie
    }
}

impl<V: DictionaryValue> PersistentARTrieU64<V, MmapDiskManager> {
    pub fn create<P: AsRef<Path>>(path: P) -> Result<Self> {
        PersistentARTrie::create(path).map(|inner| Self { inner })
    }

    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        PersistentARTrie::open(path).map(|inner| Self { inner })
    }

    pub fn open_with_recovery<P: AsRef<Path>>(path: P) -> Result<(Self, RecoveryReport)> {
        PersistentARTrie::open_with_recovery(path).map(|(inner, report)| (Self { inner }, report))
    }
}

impl<V: DictionaryValue, S: BlockStorage> PersistentARTrieU64<V, S> {
    pub fn inner(&self) -> &PersistentARTrie<V, S> {
        &self.inner
    }

    pub fn try_insert_sequence(&self, sequence: &[u64]) -> Result<bool> {
        let key = encode_sequence(sequence);
        self.inner.insert_cas_durable(&key)
    }

    pub fn insert_sequence(&self, sequence: &[u64]) -> bool {
        self.try_insert_sequence(sequence).unwrap_or_else(|error| {
            log::warn!("PersistentARTrieU64::insert_sequence failed: {error}");
            false
        })
    }

    pub fn try_insert_sequence_with_value(&self, sequence: &[u64], value: V) -> Result<bool> {
        let key = encode_sequence(sequence);
        self.inner.upsert_bytes(&key, value)
    }

    pub fn insert_sequence_with_value(&self, sequence: &[u64], value: V) -> bool {
        self.try_insert_sequence_with_value(sequence, value)
            .unwrap_or_else(|error| {
                log::warn!("PersistentARTrieU64::insert_sequence_with_value failed: {error}");
                false
            })
    }

    pub fn update_or_insert_sequence<F>(
        &self,
        sequence: &[u64],
        default_value: V,
        update_fn: F,
    ) -> bool
    where
        F: FnOnce(&mut V),
    {
        let key = encode_sequence(sequence);
        if let Some(mut value) = self.inner.get_value_bytes(&key) {
            update_fn(&mut value);
            self.inner
                .upsert_bytes(&key, value)
                .unwrap_or_else(|error| {
                    log::warn!("PersistentARTrieU64::update_or_insert_sequence failed: {error}");
                    false
                });
            false
        } else {
            self.insert_sequence_with_value(sequence, default_value)
        }
    }

    pub fn contains_sequence(&self, sequence: &[u64]) -> bool {
        let key = encode_sequence(sequence);
        self.inner.contains_bytes(&key)
    }

    pub fn get_sequence_value(&self, sequence: &[u64]) -> Option<V> {
        let key = encode_sequence(sequence);
        self.inner.get_value_bytes(&key)
    }

    pub fn try_remove_sequence(&self, sequence: &[u64]) -> Result<bool> {
        let key = encode_sequence(sequence);
        self.inner.remove_cas_durable(&key)
    }

    pub fn remove_sequence(&self, sequence: &[u64]) -> bool {
        self.try_remove_sequence(sequence).unwrap_or_else(|error| {
            log::warn!("PersistentARTrieU64::remove_sequence failed: {error}");
            false
        })
    }

    pub fn term_count(&self) -> usize {
        self.iter_sequences().count()
    }

    pub fn iter_sequences(&self) -> impl Iterator<Item = Vec<u64>> + '_ {
        self.inner.iter().filter_map(|term| decode_sequence(&term))
    }

    pub fn iter_sequences_with_values(&self) -> impl Iterator<Item = (Vec<u64>, Option<V>)> + '_ {
        self.inner
            .iter_with_values()
            .filter_map(|(term, value)| decode_sequence(&term).map(|sequence| (sequence, value)))
    }

    pub fn iter_sequence_prefix(&self, prefix: &[u64]) -> Box<dyn Iterator<Item = Vec<u64>> + '_> {
        let encoded = encode_sequence(prefix);
        match self.inner.iter_prefix(&encoded) {
            Some(iter) => Box::new(iter.filter_map(|term| decode_sequence(&term))),
            None => Box::new(std::iter::empty()),
        }
    }

    pub fn iter_sequence_prefix_with_values(
        &self,
        prefix: &[u64],
    ) -> Box<dyn Iterator<Item = (Vec<u64>, Option<V>)> + '_> {
        let encoded = encode_sequence(prefix);
        match self.inner.iter_prefix(&encoded) {
            Some(iter) => Box::new(iter.filter_map(move |term| {
                let value = self.inner.get_value_bytes(&term);
                decode_sequence(&term).map(|sequence| (sequence, value))
            })),
            None => Box::new(std::iter::empty()),
        }
    }

    pub fn insert_f64(&self, series: &[f64]) -> bool {
        let sequence: Vec<u64> = series.iter().map(|value| value.to_bits()).collect();
        self.insert_sequence(&sequence)
    }

    pub fn insert_f64_with_value(&self, series: &[f64], value: V) -> bool {
        let sequence: Vec<u64> = series.iter().map(|value| value.to_bits()).collect();
        self.insert_sequence_with_value(&sequence, value)
    }

    pub fn contains_f64(&self, series: &[f64]) -> bool {
        let sequence: Vec<u64> = series.iter().map(|value| value.to_bits()).collect();
        self.contains_sequence(&sequence)
    }

    pub fn get_f64_value(&self, series: &[f64]) -> Option<V> {
        let sequence: Vec<u64> = series.iter().map(|value| value.to_bits()).collect();
        self.get_sequence_value(&sequence)
    }

    pub fn remove_f64(&self, series: &[f64]) -> bool {
        let sequence: Vec<u64> = series.iter().map(|value| value.to_bits()).collect();
        self.remove_sequence(&sequence)
    }

    pub fn insert(&self, term: &str) -> bool {
        let sequence = <u64 as CharUnit>::from_str(term);
        self.insert_sequence(&sequence)
    }

    pub fn insert_with_value(&self, term: &str, value: V) -> bool {
        let sequence = <u64 as CharUnit>::from_str(term);
        self.insert_sequence_with_value(&sequence, value)
    }

    pub fn contains(&self, term: &str) -> bool {
        let sequence = <u64 as CharUnit>::from_str(term);
        self.contains_sequence(&sequence)
    }

    pub fn get_value(&self, term: &str) -> Option<V> {
        let sequence = <u64 as CharUnit>::from_str(term);
        self.get_sequence_value(&sequence)
    }

    pub fn remove(&self, term: &str) -> bool {
        let sequence = <u64 as CharUnit>::from_str(term);
        self.remove_sequence(&sequence)
    }

    pub fn checkpoint(&self) -> Result<()> {
        self.inner.checkpoint()
    }

    pub fn close(&self) {
        self.inner.close();
    }
}

impl<V: DictionaryValue> DictionaryNode for PersistentARTrieU64Node<V> {
    type Unit = u64;

    fn is_final(&self) -> bool {
        self.inner.is_final()
    }

    fn transition(&self, label: Self::Unit) -> Option<Self> {
        let mut node = self.inner.clone();
        for byte in label.to_le_bytes() {
            node = node.transition(byte)?;
        }
        let mut path = self.path.clone();
        path.push(label);
        Some(Self { inner: node, path })
    }

    fn edges(&self) -> Box<dyn Iterator<Item = (Self::Unit, Self)> + '_> {
        let mut bytes = [0u8; 8];
        let mut edges = Vec::new();
        collect_u64_edges(&self.inner, 0, &mut bytes, &mut edges);
        let path = self.path.clone();
        let edges: Vec<_> = edges
            .into_iter()
            .map(|(label, inner)| {
                let mut child_path = path.clone();
                child_path.push(label);
                (
                    label,
                    Self {
                        inner,
                        path: child_path,
                    },
                )
            })
            .collect();
        Box::new(edges.into_iter())
    }

    fn edge_count(&self) -> Option<usize> {
        Some(self.edges().count())
    }
}

impl<V: DictionaryValue> MappedDictionaryNode for PersistentARTrieU64Node<V> {
    type Value = V;

    fn value(&self) -> Option<Self::Value> {
        self.inner.value()
    }
}

impl<V: DictionaryValue, S: BlockStorage> Dictionary for PersistentARTrieU64<V, S> {
    type Node = PersistentARTrieU64Node<V>;

    fn root(&self) -> Self::Node {
        Self::Node {
            inner: self.inner.root(),
            path: Vec::new(),
        }
    }

    fn contains(&self, term: &str) -> bool {
        PersistentARTrieU64::contains(self, term)
    }

    fn len(&self) -> Option<usize> {
        Some(self.term_count())
    }

    fn sync_strategy(&self) -> SyncStrategy {
        SyncStrategy::InternalSync
    }
}

impl<V: DictionaryValue, S: BlockStorage> MappedDictionary for PersistentARTrieU64<V, S> {
    type Value = V;

    fn get_value(&self, term: &str) -> Option<Self::Value> {
        PersistentARTrieU64::get_value(self, term)
    }
}

impl<V: DictionaryValue, S: BlockStorage> MutableDictionary for PersistentARTrieU64<V, S> {
    fn insert(&self, term: &str) -> bool {
        PersistentARTrieU64::insert(self, term)
    }

    fn remove(&self, term: &str) -> bool {
        PersistentARTrieU64::remove(self, term)
    }
}

impl<V: DictionaryValue, S: BlockStorage> MutableMappedDictionary for PersistentARTrieU64<V, S> {
    fn insert_with_value(&self, term: &str, value: Self::Value) -> bool {
        PersistentARTrieU64::insert_with_value(self, term, value)
    }

    fn update_or_insert<F>(&self, term: &str, default_value: Self::Value, update_fn: F) -> bool
    where
        F: FnOnce(&mut Self::Value),
    {
        let sequence = <u64 as CharUnit>::from_str(term);
        self.update_or_insert_sequence(&sequence, default_value, update_fn)
    }

    fn union_with<F>(&self, other: &Self, merge_fn: F) -> usize
    where
        F: Fn(&Self::Value, &Self::Value) -> Self::Value,
        Self::Value: Clone,
    {
        let mut processed = 0;
        for (sequence, other_value) in other.iter_sequences_with_values() {
            let Some(other_value) = other_value else {
                continue;
            };
            processed += 1;
            let value = if let Some(self_value) = self.get_sequence_value(&sequence) {
                merge_fn(&self_value, &other_value)
            } else {
                other_value
            };
            self.insert_sequence_with_value(&sequence, value);
        }
        processed
    }
}

impl<V: DictionaryValue> Default for PersistentARTrieU64<V> {
    fn default() -> Self {
        Self::new()
    }
}
