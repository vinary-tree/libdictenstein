//! Persistent SCDAWG-compatible dictionaries backed by persistent suffix indexes.
//!
//! The byte and Unicode variants intentionally reuse the persistent suffix
//! automaton storage path. That keeps WAL ordering, checkpoint/reopen, CX
//! compression, and pointer swizzling on the established persistent ARTrie
//! architecture while presenting the public SCDAWG substring API.

use std::collections::HashSet;
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

/// Byte/u8 persistent SCDAWG-compatible dictionary.
pub struct PersistentScdawg<V: DictionaryValue = (), S: BlockStorage = MmapDiskManager> {
    inner: PersistentSuffixAutomaton<V, S>,
}

/// Unicode/u32 persistent SCDAWG-compatible dictionary.
pub struct PersistentScdawgChar<V: DictionaryValue = (), S: BlockStorage = MmapDiskManager> {
    inner: PersistentSuffixAutomatonChar<V, S>,
}

/// Byte-level persistent SCDAWG node handle.
#[derive(Clone, Debug)]
pub struct PersistentScdawgNode<V: DictionaryValue = ()> {
    inner: PersistentSuffixAutomatonNode<V>,
    path: Vec<u8>,
}

/// Character-level persistent SCDAWG node handle.
#[derive(Clone, Debug)]
pub struct PersistentScdawgCharNode<V: DictionaryValue = ()> {
    inner: PersistentSuffixAutomatonCharNode<V>,
    path: String,
}

fn unique_terms<I>(terms: I) -> Vec<String>
where
    I: IntoIterator<Item = String>,
{
    let mut seen = HashSet::new();
    let mut unique = Vec::new();
    for term in terms {
        if seen.insert(term.clone()) {
            unique.push(term);
        }
    }
    unique
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

impl<V: DictionaryValue> PersistentScdawg<V> {
    /// Create an in-memory persistent SCDAWG-compatible dictionary.
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

    pub fn from_terms<I, T>(terms: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: AsRef<str>,
    {
        let dict = Self::new();
        for term in terms {
            dict.insert(term.as_ref());
        }
        dict
    }

    pub fn from_terms_with_values<I, T>(entries: I) -> Self
    where
        I: IntoIterator<Item = (T, V)>,
        T: AsRef<str>,
    {
        let dict = Self::new();
        for (term, value) in entries {
            dict.insert_with_value(term.as_ref(), value);
        }
        dict
    }
}

impl<V: DictionaryValue> PersistentScdawg<V, MmapDiskManager> {
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

impl<V: DictionaryValue, S: BlockStorage> PersistentScdawg<V, S> {
    pub fn inner(&self) -> &PersistentSuffixAutomaton<V, S> {
        &self.inner
    }

    pub fn insert(&self, term: &str) -> bool {
        if self.contains(term) {
            return false;
        }
        self.inner.insert(term)
    }

    pub fn insert_with_value(&self, term: &str, value: V) -> bool {
        if self.contains(term) {
            self.inner
                .update_or_insert(term, value.clone(), |current| *current = value);
            return false;
        }
        self.inner.insert_with_value(term, value)
    }

    pub fn update_or_insert<F>(&self, term: &str, default_value: V, update_fn: F) -> bool
    where
        F: FnOnce(&mut V),
    {
        if self.contains(term) {
            self.inner.update_or_insert(term, default_value, update_fn);
            false
        } else {
            self.inner.insert_with_value(term, default_value)
        }
    }

    pub fn remove(&self, term: &str) -> bool {
        if !self.contains(term) {
            return false;
        }
        self.inner.remove(term)
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

    pub fn term_count(&self) -> usize {
        self.iter().count()
    }

    pub fn string_count(&self) -> usize {
        self.term_count()
    }

    pub fn iter(&self) -> impl Iterator<Item = String> {
        unique_terms(self.active_terms().into_iter()).into_iter()
    }

    pub fn source_texts(&self) -> Vec<String> {
        self.iter().collect()
    }

    pub fn contains_substring(&self, pattern: &str) -> bool {
        if pattern.is_empty() {
            return true;
        }
        !self.locations(pattern).is_empty()
    }

    pub fn find(&self, pattern: &str) -> Option<PersistentScdawgNode<V>> {
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
            return self.active_terms().iter().map(|term| term.len() + 1).sum();
        }
        self.locations(pattern).len()
    }

    pub fn freq_at(&self, handle: &PersistentScdawgNode<V>) -> usize {
        match std::str::from_utf8(&handle.path) {
            Ok(pattern) => self.freq(pattern),
            Err(_) => 0,
        }
    }

    pub fn locations(&self, pattern: &str) -> Vec<(String, usize)> {
        let terms = self.inner.source_texts();
        if pattern.is_empty() {
            return self
                .active_terms()
                .into_iter()
                .map(|term| (term, 0))
                .collect();
        }

        let mut locations = Vec::new();
        for (source_id, finish_byte) in self.inner.match_positions(pattern) {
            let Some(term) = terms.get(source_id) else {
                continue;
            };
            let Some(start) = byte_match_start(finish_byte, pattern) else {
                continue;
            };
            locations.push((term.clone(), start));
        }
        locations
    }

    pub fn locations_at(
        &self,
        handle: &PersistentScdawgNode<V>,
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

    fn active_terms(&self) -> Vec<String> {
        let source_texts = self.inner.source_texts();
        let mut active = Vec::new();
        let mut active_non_empty = 0usize;
        let mut empty_terms = Vec::new();

        for (source_id, term) in source_texts.into_iter().enumerate() {
            if term.is_empty() {
                empty_terms.push(term);
                continue;
            }
            let finish = term.len();
            if self
                .inner
                .match_positions(&term)
                .into_iter()
                .any(|pos| pos == (source_id, finish))
            {
                active_non_empty += 1;
                active.push(term);
            }
        }

        let active_empty = self.inner.string_count().saturating_sub(active_non_empty);
        active.extend(empty_terms.into_iter().take(active_empty));
        active
    }
}

impl<V: DictionaryValue> PersistentScdawgChar<V> {
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

    pub fn from_terms<I, T>(terms: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: AsRef<str>,
    {
        let dict = Self::new();
        for term in terms {
            dict.insert(term.as_ref());
        }
        dict
    }

    pub fn from_terms_with_values<I, T>(entries: I) -> Self
    where
        I: IntoIterator<Item = (T, V)>,
        T: AsRef<str>,
    {
        let dict = Self::new();
        for (term, value) in entries {
            dict.insert_with_value(term.as_ref(), value);
        }
        dict
    }
}

impl<V: DictionaryValue> PersistentScdawgChar<V, MmapDiskManager> {
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

impl<V: DictionaryValue, S: BlockStorage> PersistentScdawgChar<V, S> {
    pub fn inner(&self) -> &PersistentSuffixAutomatonChar<V, S> {
        &self.inner
    }

    pub fn insert(&self, term: &str) -> bool {
        if self.contains(term) {
            return false;
        }
        self.inner.insert(term)
    }

    pub fn insert_with_value(&self, term: &str, value: V) -> bool {
        if self.contains(term) {
            self.inner
                .update_or_insert(term, value.clone(), |current| *current = value);
            return false;
        }
        self.inner.insert_with_value(term, value)
    }

    pub fn update_or_insert<F>(&self, term: &str, default_value: V, update_fn: F) -> bool
    where
        F: FnOnce(&mut V),
    {
        if self.contains(term) {
            self.inner.update_or_insert(term, default_value, update_fn);
            false
        } else {
            self.inner.insert_with_value(term, default_value)
        }
    }

    pub fn remove(&self, term: &str) -> bool {
        if !self.contains(term) {
            return false;
        }
        self.inner.remove(term)
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

    pub fn term_count(&self) -> usize {
        self.iter().count()
    }

    pub fn string_count(&self) -> usize {
        self.term_count()
    }

    pub fn iter(&self) -> impl Iterator<Item = String> {
        unique_terms(self.active_terms().into_iter()).into_iter()
    }

    pub fn source_texts(&self) -> Vec<String> {
        self.iter().collect()
    }

    pub fn contains_substring(&self, pattern: &str) -> bool {
        if pattern.is_empty() {
            return true;
        }
        !self.locations(pattern).is_empty()
    }

    pub fn find(&self, pattern: &str) -> Option<PersistentScdawgCharNode<V>> {
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
                .active_terms()
                .iter()
                .map(|term| term.chars().count() + 1)
                .sum();
        }
        self.locations(pattern).len()
    }

    pub fn freq_at(&self, handle: &PersistentScdawgCharNode<V>) -> usize {
        self.freq(&handle.path)
    }

    pub fn locations(&self, pattern: &str) -> Vec<(String, usize)> {
        let terms = self.inner.source_texts();
        if pattern.is_empty() {
            return self
                .active_terms()
                .into_iter()
                .map(|term| (term, 0))
                .collect();
        }

        let mut locations = Vec::new();
        for (source_id, finish_byte) in self.inner.match_positions(pattern) {
            let Some(term) = terms.get(source_id) else {
                continue;
            };
            let Some(start) = char_match_start(term, finish_byte, pattern) else {
                continue;
            };
            locations.push((term.clone(), start));
        }
        locations
    }

    pub fn locations_at(
        &self,
        handle: &PersistentScdawgCharNode<V>,
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

    fn active_terms(&self) -> Vec<String> {
        let source_texts = self.inner.source_texts();
        let mut active = Vec::new();
        let mut active_non_empty = 0usize;
        let mut empty_terms = Vec::new();

        for (source_id, term) in source_texts.into_iter().enumerate() {
            if term.is_empty() {
                empty_terms.push(term);
                continue;
            }
            let finish = term.len();
            if self
                .inner
                .match_positions(&term)
                .into_iter()
                .any(|pos| pos == (source_id, finish))
            {
                active_non_empty += 1;
                active.push(term);
            }
        }

        let active_empty = self.inner.string_count().saturating_sub(active_non_empty);
        active.extend(empty_terms.into_iter().take(active_empty));
        active
    }
}

impl<V: DictionaryValue> DictionaryNode for PersistentScdawgNode<V> {
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

impl<V: DictionaryValue> MappedDictionaryNode for PersistentScdawgNode<V> {
    type Value = V;

    fn value(&self) -> Option<Self::Value> {
        self.inner.value()
    }
}

impl<V: DictionaryValue> DictionaryNode for PersistentScdawgCharNode<V> {
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

impl<V: DictionaryValue> MappedDictionaryNode for PersistentScdawgCharNode<V> {
    type Value = V;

    fn value(&self) -> Option<Self::Value> {
        self.inner.value()
    }
}

impl<V: DictionaryValue, S: BlockStorage> Dictionary for PersistentScdawg<V, S> {
    type Node = PersistentScdawgNode<V>;

    fn root(&self) -> Self::Node {
        Self::Node {
            inner: self.inner.root(),
            path: Vec::new(),
        }
    }

    fn contains(&self, term: &str) -> bool {
        if term.is_empty() {
            return self
                .active_terms()
                .iter()
                .any(|candidate| candidate.is_empty());
        }
        let finish = term.len();
        self.inner
            .source_texts()
            .into_iter()
            .enumerate()
            .filter(|(_, source)| source == term)
            .any(|(source_id, _)| {
                self.inner
                    .match_positions(term)
                    .into_iter()
                    .any(|pos| pos == (source_id, finish))
            })
    }

    fn len(&self) -> Option<usize> {
        Some(self.term_count())
    }

    fn sync_strategy(&self) -> SyncStrategy {
        SyncStrategy::InternalSync
    }

    fn is_suffix_based(&self) -> bool {
        true
    }
}

impl<V: DictionaryValue, S: BlockStorage> MappedDictionary for PersistentScdawg<V, S> {
    type Value = V;

    fn get_value(&self, term: &str) -> Option<Self::Value> {
        if self.contains(term) {
            self.inner.get_value(term)
        } else {
            None
        }
    }
}

impl<V: DictionaryValue, S: BlockStorage> MutableDictionary for PersistentScdawg<V, S> {
    fn insert(&self, term: &str) -> bool {
        PersistentScdawg::insert(self, term)
    }

    fn remove(&self, term: &str) -> bool {
        PersistentScdawg::remove(self, term)
    }
}

impl<V: DictionaryValue, S: BlockStorage> MutableMappedDictionary for PersistentScdawg<V, S> {
    fn insert_with_value(&self, term: &str, value: Self::Value) -> bool {
        PersistentScdawg::insert_with_value(self, term, value)
    }

    fn update_or_insert<F>(&self, term: &str, default_value: Self::Value, update_fn: F) -> bool
    where
        F: FnOnce(&mut Self::Value),
    {
        PersistentScdawg::update_or_insert(self, term, default_value, update_fn)
    }

    fn union_with<F>(&self, other: &Self, merge_fn: F) -> usize
    where
        F: Fn(&Self::Value, &Self::Value) -> Self::Value,
        Self::Value: Clone,
    {
        let mut processed = 0;
        for term in other.iter() {
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

impl<V: DictionaryValue, S: BlockStorage> SubstringDictionary for PersistentScdawg<V, S> {
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

impl<V: DictionaryValue, S: BlockStorage> Dictionary for PersistentScdawgChar<V, S> {
    type Node = PersistentScdawgCharNode<V>;

    fn root(&self) -> Self::Node {
        Self::Node {
            inner: self.inner.root(),
            path: String::new(),
        }
    }

    fn contains(&self, term: &str) -> bool {
        if term.is_empty() {
            return self
                .active_terms()
                .iter()
                .any(|candidate| candidate.is_empty());
        }
        let finish = term.len();
        self.inner
            .source_texts()
            .into_iter()
            .enumerate()
            .filter(|(_, source)| source == term)
            .any(|(source_id, _)| {
                self.inner
                    .match_positions(term)
                    .into_iter()
                    .any(|pos| pos == (source_id, finish))
            })
    }

    fn len(&self) -> Option<usize> {
        Some(self.term_count())
    }

    fn sync_strategy(&self) -> SyncStrategy {
        SyncStrategy::InternalSync
    }

    fn is_suffix_based(&self) -> bool {
        true
    }
}

impl<V: DictionaryValue, S: BlockStorage> MappedDictionary for PersistentScdawgChar<V, S> {
    type Value = V;

    fn get_value(&self, term: &str) -> Option<Self::Value> {
        if self.contains(term) {
            self.inner.get_value(term)
        } else {
            None
        }
    }
}

impl<V: DictionaryValue, S: BlockStorage> MutableDictionary for PersistentScdawgChar<V, S> {
    fn insert(&self, term: &str) -> bool {
        PersistentScdawgChar::insert(self, term)
    }

    fn remove(&self, term: &str) -> bool {
        PersistentScdawgChar::remove(self, term)
    }
}

impl<V: DictionaryValue, S: BlockStorage> MutableMappedDictionary for PersistentScdawgChar<V, S> {
    fn insert_with_value(&self, term: &str, value: Self::Value) -> bool {
        PersistentScdawgChar::insert_with_value(self, term, value)
    }

    fn update_or_insert<F>(&self, term: &str, default_value: Self::Value, update_fn: F) -> bool
    where
        F: FnOnce(&mut Self::Value),
    {
        PersistentScdawgChar::update_or_insert(self, term, default_value, update_fn)
    }

    fn union_with<F>(&self, other: &Self, merge_fn: F) -> usize
    where
        F: Fn(&Self::Value, &Self::Value) -> Self::Value,
        Self::Value: Clone,
    {
        let mut processed = 0;
        for term in other.iter() {
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

impl<V: DictionaryValue, S: BlockStorage> SubstringDictionary for PersistentScdawgChar<V, S> {
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

impl<V: DictionaryValue> Default for PersistentScdawg<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V: DictionaryValue> Default for PersistentScdawgChar<V> {
    fn default() -> Self {
        Self::new()
    }
}
