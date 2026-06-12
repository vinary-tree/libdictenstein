//! Persistent suffix-tree-compatible dictionaries backed by native suffix indexes.
//!
//! The byte and Unicode variants expose a suffix tree API over the same native
//! suffix graph snapshot/WAL storage used by the persistent suffix automata.
//! Reads traverse immutable graph snapshots while writers publish copy-on-write
//! revisions, so read-side traversal is non-blocking with respect to mutation.

use std::path::Path;

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::disk_manager::MmapDiskManager;
use crate::persistent_artrie::error::Result;
use crate::persistent_artrie::{
    PersistentSuffixAutomaton, PersistentSuffixAutomatonChar, PersistentSuffixAutomatonCharNode,
    PersistentSuffixAutomatonNode, RecoveryReport,
};
use crate::substring::{SubstringDictionary, SubstringMatch};
use crate::value::DictionaryValue;
use crate::{
    Dictionary, DictionaryNode, MappedDictionary, MappedDictionaryNode, MutableDictionary,
    MutableMappedDictionary, SyncStrategy,
};

/// Byte/u8 persistent suffix-tree-compatible substring index.
pub struct PersistentSuffixTree<V: DictionaryValue = (), S: BlockStorage = MmapDiskManager> {
    inner: PersistentSuffixAutomaton<V, S>,
}

/// Unicode/u32 persistent suffix-tree-compatible substring index.
pub struct PersistentSuffixTreeChar<V: DictionaryValue = (), S: BlockStorage = MmapDiskManager> {
    inner: PersistentSuffixAutomatonChar<V, S>,
}

/// Byte-level persistent suffix tree node handle.
#[derive(Clone, Debug)]
pub struct PersistentSuffixTreeNode<V: DictionaryValue = ()> {
    inner: PersistentSuffixAutomatonNode<V>,
    path: Vec<u8>,
}

/// Character-level persistent suffix tree node handle.
#[derive(Clone, Debug)]
pub struct PersistentSuffixTreeCharNode<V: DictionaryValue = ()> {
    inner: PersistentSuffixAutomatonCharNode<V>,
    path: String,
}

fn byte_match_start(finish_byte: usize, pattern: &str) -> Option<usize> {
    finish_byte.checked_sub(pattern.len())
}

fn char_match_start(term: &str, finish_byte: usize, pattern: &str) -> Option<usize> {
    let start_byte = finish_byte.checked_sub(pattern.len())?;
    if start_byte > term.len() || !term.is_char_boundary(start_byte) {
        return None;
    }
    Some(term[..start_byte].chars().count())
}

impl<V: DictionaryValue> PersistentSuffixTree<V> {
    /// Create an in-memory persistent suffix tree.
    pub fn new() -> Self {
        Self {
            inner: PersistentSuffixAutomaton::new(),
        }
    }

    pub fn from_text(text: &str) -> Self {
        let dict = Self::new();
        dict.insert(text);
        dict
    }

    pub fn from_texts<I, T>(texts: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: AsRef<str>,
    {
        let dict = Self::new();
        for text in texts {
            dict.insert(text.as_ref());
        }
        dict
    }
}

impl<V: DictionaryValue> PersistentSuffixTree<V, MmapDiskManager> {
    pub fn create<P: AsRef<Path>>(path: P) -> Result<Self> {
        PersistentSuffixAutomaton::create(path).map(|inner| Self { inner })
    }

    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        PersistentSuffixAutomaton::open(path).map(|inner| Self { inner })
    }

    pub fn open_with_recovery<P: AsRef<Path>>(path: P) -> Result<(Self, RecoveryReport)> {
        PersistentSuffixAutomaton::open_with_recovery(path)
            .map(|(inner, report)| (Self { inner }, report))
    }
}

impl<V: DictionaryValue, S: BlockStorage> PersistentSuffixTree<V, S> {
    pub fn inner(&self) -> &PersistentSuffixAutomaton<V, S> {
        &self.inner
    }

    pub fn insert(&self, text: &str) -> bool {
        self.inner.insert(text)
    }

    pub fn insert_with_value(&self, text: &str, value: V) -> bool {
        self.inner.insert_with_value(text, value)
    }

    pub fn update_or_insert<F>(&self, term: &str, default_value: V, update_fn: F) -> bool
    where
        F: FnOnce(&mut V),
    {
        self.inner.update_or_insert(term, default_value, update_fn)
    }

    pub fn remove(&self, text: &str) -> bool {
        self.inner.remove(text)
    }

    pub fn clear(&self) {
        self.inner.clear();
    }

    pub fn compact(&self) {
        self.inner.compact();
    }

    pub fn needs_compaction(&self) -> bool {
        self.inner.needs_compaction()
    }

    pub fn string_count(&self) -> usize {
        self.inner.string_count()
    }

    pub fn source_texts(&self) -> Vec<String> {
        self.inner.source_texts()
    }

    pub fn active_texts(&self) -> Vec<String> {
        let source_texts = self.inner.source_texts();
        let mut active = Vec::new();
        let mut active_non_empty = 0usize;
        let mut empty_texts = Vec::new();

        for (source_id, text) in source_texts.into_iter().enumerate() {
            if text.is_empty() {
                empty_texts.push(text);
                continue;
            }
            let finish = text.len();
            if self
                .inner
                .match_positions(&text)
                .into_iter()
                .any(|pos| pos == (source_id, finish))
            {
                active_non_empty += 1;
                active.push(text);
            }
        }

        let active_empty = self.inner.string_count().saturating_sub(active_non_empty);
        active.extend(empty_texts.into_iter().take(active_empty));
        active
    }

    pub fn match_positions(&self, pattern: &str) -> Vec<(usize, usize)> {
        self.inner.match_positions(pattern)
    }

    pub fn contains_substring(&self, pattern: &str) -> bool {
        pattern.is_empty() || !self.inner.match_positions(pattern).is_empty()
    }

    pub fn find(&self, pattern: &str) -> Option<PersistentSuffixTreeNode<V>> {
        if !self.contains_substring(pattern) {
            return None;
        }
        let mut node = self.root();
        for byte in pattern.bytes() {
            node = node.transition(byte)?;
        }
        Some(node)
    }

    pub fn freq(&self, pattern: &str) -> usize {
        if pattern.is_empty() {
            return self.active_texts().iter().map(|text| text.len() + 1).sum();
        }
        self.locations(pattern).len()
    }

    pub fn freq_at(&self, handle: &PersistentSuffixTreeNode<V>) -> usize {
        match std::str::from_utf8(&handle.path) {
            Ok(pattern) => self.freq(pattern),
            Err(_) => 0,
        }
    }

    pub fn locations(&self, pattern: &str) -> Vec<(String, usize)> {
        let texts = self.inner.source_texts();
        if pattern.is_empty() {
            return self
                .active_texts()
                .into_iter()
                .map(|text| (text, 0))
                .collect();
        }

        let mut locations = Vec::new();
        for (source_id, finish_byte) in self.inner.match_positions(pattern) {
            let Some(text) = texts.get(source_id) else {
                continue;
            };
            let Some(start) = byte_match_start(finish_byte, pattern) else {
                continue;
            };
            locations.push((text.clone(), start));
        }
        locations
    }

    pub fn locations_at(
        &self,
        handle: &PersistentSuffixTreeNode<V>,
        pattern_len: usize,
    ) -> Vec<(String, usize)> {
        if pattern_len > handle.path.len() {
            return Vec::new();
        }
        let start = handle.path.len() - pattern_len;
        match std::str::from_utf8(&handle.path[start..]) {
            Ok(pattern) => self.locations(pattern),
            Err(_) => Vec::new(),
        }
    }

    pub fn checkpoint(&self) -> Result<()> {
        self.inner.checkpoint()
    }

    pub fn close(&self) {
        self.inner.close();
    }
}

impl<V: DictionaryValue> PersistentSuffixTreeChar<V> {
    pub fn new() -> Self {
        Self {
            inner: PersistentSuffixAutomatonChar::new(),
        }
    }

    pub fn from_text(text: &str) -> Self {
        let dict = Self::new();
        dict.insert(text);
        dict
    }

    pub fn from_texts<I, T>(texts: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: AsRef<str>,
    {
        let dict = Self::new();
        for text in texts {
            dict.insert(text.as_ref());
        }
        dict
    }
}

impl<V: DictionaryValue> PersistentSuffixTreeChar<V, MmapDiskManager> {
    pub fn create<P: AsRef<Path>>(path: P) -> Result<Self> {
        PersistentSuffixAutomatonChar::create(path).map(|inner| Self { inner })
    }

    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        PersistentSuffixAutomatonChar::open(path).map(|inner| Self { inner })
    }

    pub fn open_with_recovery<P: AsRef<Path>>(path: P) -> Result<(Self, RecoveryReport)> {
        PersistentSuffixAutomatonChar::open_with_recovery(path)
            .map(|(inner, report)| (Self { inner }, report))
    }
}

impl<V: DictionaryValue, S: BlockStorage> PersistentSuffixTreeChar<V, S> {
    pub fn inner(&self) -> &PersistentSuffixAutomatonChar<V, S> {
        &self.inner
    }

    pub fn insert(&self, text: &str) -> bool {
        self.inner.insert(text)
    }

    pub fn insert_with_value(&self, text: &str, value: V) -> bool {
        self.inner.insert_with_value(text, value)
    }

    pub fn update_or_insert<F>(&self, term: &str, default_value: V, update_fn: F) -> bool
    where
        F: FnOnce(&mut V),
    {
        self.inner.update_or_insert(term, default_value, update_fn)
    }

    pub fn remove(&self, text: &str) -> bool {
        self.inner.remove(text)
    }

    pub fn clear(&self) {
        self.inner.clear();
    }

    pub fn compact(&self) {
        self.inner.compact();
    }

    pub fn needs_compaction(&self) -> bool {
        self.inner.needs_compaction()
    }

    pub fn string_count(&self) -> usize {
        self.inner.string_count()
    }

    pub fn source_texts(&self) -> Vec<String> {
        self.inner.source_texts()
    }

    pub fn active_texts(&self) -> Vec<String> {
        let source_texts = self.inner.source_texts();
        let mut active = Vec::new();
        let mut active_non_empty = 0usize;
        let mut empty_texts = Vec::new();

        for (source_id, text) in source_texts.into_iter().enumerate() {
            if text.is_empty() {
                empty_texts.push(text);
                continue;
            }
            let finish = text.len();
            if self
                .inner
                .match_positions(&text)
                .into_iter()
                .any(|pos| pos == (source_id, finish))
            {
                active_non_empty += 1;
                active.push(text);
            }
        }

        let active_empty = self.inner.string_count().saturating_sub(active_non_empty);
        active.extend(empty_texts.into_iter().take(active_empty));
        active
    }

    pub fn match_positions(&self, pattern: &str) -> Vec<(usize, usize)> {
        self.inner.match_positions(pattern)
    }

    pub fn contains_substring(&self, pattern: &str) -> bool {
        pattern.is_empty() || !self.inner.match_positions(pattern).is_empty()
    }

    pub fn find(&self, pattern: &str) -> Option<PersistentSuffixTreeCharNode<V>> {
        if !self.contains_substring(pattern) {
            return None;
        }
        let mut node = self.root();
        for ch in pattern.chars() {
            node = node.transition(ch)?;
        }
        Some(node)
    }

    pub fn freq(&self, pattern: &str) -> usize {
        if pattern.is_empty() {
            return self
                .active_texts()
                .iter()
                .map(|text| text.chars().count() + 1)
                .sum();
        }
        self.locations(pattern).len()
    }

    pub fn freq_at(&self, handle: &PersistentSuffixTreeCharNode<V>) -> usize {
        self.freq(&handle.path)
    }

    pub fn locations(&self, pattern: &str) -> Vec<(String, usize)> {
        let texts = self.inner.source_texts();
        if pattern.is_empty() {
            return self
                .active_texts()
                .into_iter()
                .map(|text| (text, 0))
                .collect();
        }

        let mut locations = Vec::new();
        for (source_id, finish_byte) in self.inner.match_positions(pattern) {
            let Some(text) = texts.get(source_id) else {
                continue;
            };
            let Some(start) = char_match_start(text, finish_byte, pattern) else {
                continue;
            };
            locations.push((text.clone(), start));
        }
        locations
    }

    pub fn locations_at(
        &self,
        handle: &PersistentSuffixTreeCharNode<V>,
        pattern_len: usize,
    ) -> Vec<(String, usize)> {
        let chars: Vec<char> = handle.path.chars().collect();
        if pattern_len > chars.len() {
            return Vec::new();
        }
        let pattern: String = chars[chars.len() - pattern_len..].iter().collect();
        self.locations(&pattern)
    }

    pub fn checkpoint(&self) -> Result<()> {
        self.inner.checkpoint()
    }

    pub fn close(&self) {
        self.inner.close();
    }
}

impl<V: DictionaryValue> DictionaryNode for PersistentSuffixTreeNode<V> {
    type Unit = u8;

    fn is_final(&self) -> bool {
        self.inner.is_final()
    }

    fn transition(&self, label: Self::Unit) -> Option<Self> {
        let inner = self.inner.transition(label)?;
        let mut path = self.path.clone();
        path.push(label);
        Some(Self { inner, path })
    }

    fn edges(&self) -> Box<dyn Iterator<Item = (Self::Unit, Self)> + '_> {
        let path = self.path.clone();
        let edges: Vec<_> = self
            .inner
            .edges()
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
        self.inner.edge_count()
    }
}

impl<V: DictionaryValue> MappedDictionaryNode for PersistentSuffixTreeNode<V> {
    type Value = V;

    fn value(&self) -> Option<Self::Value> {
        self.inner.value()
    }
}

impl<V: DictionaryValue> DictionaryNode for PersistentSuffixTreeCharNode<V> {
    type Unit = char;

    fn is_final(&self) -> bool {
        self.inner.is_final()
    }

    fn transition(&self, label: Self::Unit) -> Option<Self> {
        let inner = self.inner.transition(label)?;
        let mut path = self.path.clone();
        path.push(label);
        Some(Self { inner, path })
    }

    fn edges(&self) -> Box<dyn Iterator<Item = (Self::Unit, Self)> + '_> {
        let path = self.path.clone();
        let edges: Vec<_> = self
            .inner
            .edges()
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
        self.inner.edge_count()
    }
}

impl<V: DictionaryValue> MappedDictionaryNode for PersistentSuffixTreeCharNode<V> {
    type Value = V;

    fn value(&self) -> Option<Self::Value> {
        self.inner.value()
    }
}

impl<V: DictionaryValue, S: BlockStorage> Dictionary for PersistentSuffixTree<V, S> {
    type Node = PersistentSuffixTreeNode<V>;

    fn root(&self) -> Self::Node {
        Self::Node {
            inner: self.inner.root(),
            path: Vec::new(),
        }
    }

    fn contains(&self, term: &str) -> bool {
        self.contains_substring(term)
    }

    fn len(&self) -> Option<usize> {
        Some(self.string_count())
    }

    fn sync_strategy(&self) -> SyncStrategy {
        SyncStrategy::InternalSync
    }

    fn is_suffix_based(&self) -> bool {
        true
    }
}

impl<V: DictionaryValue, S: BlockStorage> MappedDictionary for PersistentSuffixTree<V, S> {
    type Value = V;

    fn get_value(&self, term: &str) -> Option<Self::Value> {
        self.inner.get_value(term)
    }
}

impl<V: DictionaryValue, S: BlockStorage> MutableDictionary for PersistentSuffixTree<V, S> {
    fn insert(&self, term: &str) -> bool {
        PersistentSuffixTree::insert(self, term)
    }

    fn remove(&self, term: &str) -> bool {
        PersistentSuffixTree::remove(self, term)
    }
}

impl<V: DictionaryValue, S: BlockStorage> MutableMappedDictionary for PersistentSuffixTree<V, S> {
    fn insert_with_value(&self, term: &str, value: Self::Value) -> bool {
        PersistentSuffixTree::insert_with_value(self, term, value)
    }

    fn update_or_insert<F>(&self, term: &str, default_value: Self::Value, update_fn: F) -> bool
    where
        F: FnOnce(&mut Self::Value),
    {
        PersistentSuffixTree::update_or_insert(self, term, default_value, update_fn)
    }

    fn union_with<F>(&self, other: &Self, merge_fn: F) -> usize
    where
        F: Fn(&Self::Value, &Self::Value) -> Self::Value,
        Self::Value: Clone,
    {
        let mut processed = 0;
        for term in other.active_texts() {
            if term.is_empty() {
                continue;
            }
            if let Some(other_value) = other.get_value(&term) {
                processed += 1;
                let value = if let Some(self_value) = self.get_value(&term) {
                    merge_fn(&self_value, &other_value)
                } else {
                    other_value
                };
                self.insert_with_value(&term, value);
            }
        }
        processed
    }
}

impl<V: DictionaryValue, S: BlockStorage> SubstringDictionary for PersistentSuffixTree<V, S> {
    fn find_exact_substring(&self, pattern: &str) -> Vec<SubstringMatch<Self::Node>> {
        let Some(node) = self.find(pattern) else {
            return Vec::new();
        };
        self.locations(pattern)
            .into_iter()
            .map(|(term, position)| {
                SubstringMatch::new(node.clone(), term, position, pattern.len())
            })
            .collect()
    }
}

impl<V: DictionaryValue, S: BlockStorage> Dictionary for PersistentSuffixTreeChar<V, S> {
    type Node = PersistentSuffixTreeCharNode<V>;

    fn root(&self) -> Self::Node {
        Self::Node {
            inner: self.inner.root(),
            path: String::new(),
        }
    }

    fn contains(&self, term: &str) -> bool {
        self.contains_substring(term)
    }

    fn len(&self) -> Option<usize> {
        Some(self.string_count())
    }

    fn sync_strategy(&self) -> SyncStrategy {
        SyncStrategy::InternalSync
    }

    fn is_suffix_based(&self) -> bool {
        true
    }
}

impl<V: DictionaryValue, S: BlockStorage> MappedDictionary for PersistentSuffixTreeChar<V, S> {
    type Value = V;

    fn get_value(&self, term: &str) -> Option<Self::Value> {
        self.inner.get_value(term)
    }
}

impl<V: DictionaryValue, S: BlockStorage> MutableDictionary for PersistentSuffixTreeChar<V, S> {
    fn insert(&self, term: &str) -> bool {
        PersistentSuffixTreeChar::insert(self, term)
    }

    fn remove(&self, term: &str) -> bool {
        PersistentSuffixTreeChar::remove(self, term)
    }
}

impl<V: DictionaryValue, S: BlockStorage> MutableMappedDictionary
    for PersistentSuffixTreeChar<V, S>
{
    fn insert_with_value(&self, term: &str, value: Self::Value) -> bool {
        PersistentSuffixTreeChar::insert_with_value(self, term, value)
    }

    fn update_or_insert<F>(&self, term: &str, default_value: Self::Value, update_fn: F) -> bool
    where
        F: FnOnce(&mut Self::Value),
    {
        PersistentSuffixTreeChar::update_or_insert(self, term, default_value, update_fn)
    }

    fn union_with<F>(&self, other: &Self, merge_fn: F) -> usize
    where
        F: Fn(&Self::Value, &Self::Value) -> Self::Value,
        Self::Value: Clone,
    {
        let mut processed = 0;
        for term in other.active_texts() {
            if term.is_empty() {
                continue;
            }
            if let Some(other_value) = other.get_value(&term) {
                processed += 1;
                let value = if let Some(self_value) = self.get_value(&term) {
                    merge_fn(&self_value, &other_value)
                } else {
                    other_value
                };
                self.insert_with_value(&term, value);
            }
        }
        processed
    }
}

impl<V: DictionaryValue, S: BlockStorage> SubstringDictionary for PersistentSuffixTreeChar<V, S> {
    fn find_exact_substring(&self, pattern: &str) -> Vec<SubstringMatch<Self::Node>> {
        let Some(node) = self.find(pattern) else {
            return Vec::new();
        };
        let pattern_len = pattern.chars().count();
        self.locations(pattern)
            .into_iter()
            .map(|(term, position)| SubstringMatch::new(node.clone(), term, position, pattern_len))
            .collect()
    }
}

impl<V: DictionaryValue> Default for PersistentSuffixTree<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V: DictionaryValue> Default for PersistentSuffixTreeChar<V> {
    fn default() -> Self {
        Self::new()
    }
}
