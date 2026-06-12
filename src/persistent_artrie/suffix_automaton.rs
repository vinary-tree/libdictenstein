//! Persistent SuffixAutomaton-compatible dictionaries backed by the ARTrie overlay.
//!
//! These types are persistent counterparts to [`crate::suffix_automaton::SuffixAutomaton`]
//! and [`crate::suffix_automaton::SuffixAutomatonChar`]. They deliberately live inside
//! the persistent ARTrie family so they can use the same lock-free overlay, WAL,
//! checkpoint, recovery, and value-publication seams as the byte, char, and vocab
//! persistent tries.

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::char::PersistentARTrieChar;
use crate::persistent_artrie::core::key_encoding::{ByteKey, CharKey};
use crate::persistent_artrie::core::overlay::durable_write::DurableOverlayWrite;
use crate::persistent_artrie::disk_manager::MmapDiskManager;
use crate::persistent_artrie::error::Result;
use crate::persistent_artrie::{PersistentARTrie, RecoveryReport};
use crate::value::DictionaryValue;
use crate::{
    Dictionary, DictionaryNode, MappedDictionary, MappedDictionaryNode, MutableDictionary,
    MutableMappedDictionary, SyncStrategy,
};

const BYTE_DATA_TAG: u8 = 0;
const BYTE_SOURCE_TAG: u8 = 1;
const BYTE_META_TAG: u8 = 2;
const BYTE_STATE_KEY: &[u8] = &[BYTE_META_TAG, b'S'];

const CHAR_DATA_TAG: char = '\u{E000}';
const CHAR_SOURCE_TAG: char = '\u{E001}';
const CHAR_META_TAG: char = '\u{E002}';
const CHAR_STATE_SUFFIX: &str = "S";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct SuffixPosition {
    source_id: u64,
    start_byte: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(bound(
    serialize = "V: serde::Serialize",
    deserialize = "V: serde::de::DeserializeOwned"
))]
pub(crate) struct SuffixPayload<V: DictionaryValue> {
    positions: Vec<SuffixPosition>,
    value: Option<V>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(bound(
    serialize = "V: serde::Serialize",
    deserialize = "V: serde::de::DeserializeOwned"
))]
pub(crate) struct SourceRecord<V: DictionaryValue> {
    id: u64,
    text: String,
    value: Option<V>,
    active: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct SuffixState {
    needs_compaction: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(bound(
    serialize = "V: serde::Serialize",
    deserialize = "V: serde::de::DeserializeOwned"
))]
pub(crate) enum PersistentSuffixValue<V: DictionaryValue> {
    Suffix(SuffixPayload<V>),
    Source(SourceRecord<V>),
    State(SuffixState),
}

impl<V: DictionaryValue> Default for PersistentSuffixValue<V> {
    fn default() -> Self {
        Self::Suffix(SuffixPayload::default())
    }
}

impl<V: DictionaryValue> DictionaryValue for PersistentSuffixValue<V> {}

/// Byte/u8 persistent suffix automaton compatible with
/// [`crate::suffix_automaton::SuffixAutomaton`].
pub struct PersistentSuffixAutomaton<V: DictionaryValue = (), S: BlockStorage = MmapDiskManager> {
    inner: PersistentARTrie<PersistentSuffixValue<V>, S>,
    next_source_id: AtomicU64,
}

/// Character/u32 persistent suffix automaton compatible with
/// [`crate::suffix_automaton::SuffixAutomatonChar`].
pub struct PersistentSuffixAutomatonChar<V: DictionaryValue = (), S: BlockStorage = MmapDiskManager>
{
    inner: PersistentARTrieChar<PersistentSuffixValue<V>, S>,
    next_source_id: AtomicU64,
}

/// Byte-level node handle that hides the internal suffix-data namespace.
#[derive(Clone, Debug)]
pub struct PersistentSuffixAutomatonNode<V: DictionaryValue = ()> {
    inner: Option<crate::persistent_artrie::PersistentARTrieNode<PersistentSuffixValue<V>>>,
}

/// Character-level node handle that hides the internal suffix-data namespace.
#[derive(Clone, Debug)]
pub struct PersistentSuffixAutomatonCharNode<V: DictionaryValue = ()> {
    inner:
        Option<crate::persistent_artrie::char::PersistentARTrieCharNode<PersistentSuffixValue<V>>>,
}

fn byte_data_key(suffix: &[u8]) -> Vec<u8> {
    let mut key = Vec::with_capacity(suffix.len() + 1);
    key.push(BYTE_DATA_TAG);
    key.extend_from_slice(suffix);
    key
}

fn byte_source_key(source_id: u64) -> Vec<u8> {
    let mut key = Vec::with_capacity(9);
    key.push(BYTE_SOURCE_TAG);
    key.extend_from_slice(&source_id.to_be_bytes());
    key
}

fn char_data_key(suffix: &str) -> String {
    let mut key = String::with_capacity(CHAR_DATA_TAG.len_utf8() + suffix.len());
    key.push(CHAR_DATA_TAG);
    key.push_str(suffix);
    key
}

fn char_source_key(source_id: u64) -> String {
    let mut key = String::with_capacity(CHAR_SOURCE_TAG.len_utf8() + 16);
    key.push(CHAR_SOURCE_TAG);
    use std::fmt::Write as _;
    let _ = write!(&mut key, "{source_id:016x}");
    key
}

fn char_state_key() -> String {
    let mut key = String::with_capacity(CHAR_META_TAG.len_utf8() + CHAR_STATE_SUFFIX.len());
    key.push(CHAR_META_TAG);
    key.push_str(CHAR_STATE_SUFFIX);
    key
}

fn sorted_byte_suffix_starts(text: &str) -> Vec<usize> {
    let bytes = text.as_bytes();
    let mut starts: Vec<usize> = (0..bytes.len()).collect();
    starts.sort_by(|left, right| bytes[*left..].cmp(&bytes[*right..]));
    starts
}

fn sorted_char_suffix_starts(text: &str) -> Vec<usize> {
    let mut starts: Vec<usize> = text.char_indices().map(|(idx, _)| idx).collect();
    starts.sort_by(|left, right| text[*left..].cmp(&text[*right..]));
    starts
}

impl<V: DictionaryValue> PersistentSuffixAutomaton<V> {
    /// Create an in-memory persistent-suffix instance.
    pub fn new() -> Self {
        #[allow(deprecated)]
        let inner = PersistentARTrie::new();
        Self::from_inner(inner)
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

impl<V: DictionaryValue> PersistentSuffixAutomaton<V, MmapDiskManager> {
    pub fn create<P: AsRef<Path>>(path: P) -> Result<Self> {
        PersistentARTrie::create(path).map(Self::from_inner)
    }

    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        PersistentARTrie::open(path).map(Self::from_inner)
    }

    pub fn open_with_recovery<P: AsRef<Path>>(path: P) -> Result<(Self, RecoveryReport)> {
        PersistentARTrie::open_with_recovery(path)
            .map(|(inner, report)| (Self::from_inner(inner), report))
    }
}

impl<V: DictionaryValue, S: BlockStorage> PersistentSuffixAutomaton<V, S> {
    fn from_inner(inner: PersistentARTrie<PersistentSuffixValue<V>, S>) -> Self {
        let dict = Self {
            inner,
            next_source_id: AtomicU64::new(0),
        };
        dict.next_source_id
            .store(dict.derive_next_source_id(), Ordering::Release);
        let _ = dict.ensure_state();
        dict
    }

    fn derive_next_source_id(&self) -> u64 {
        self.source_records()
            .into_iter()
            .map(|record| record.id.saturating_add(1))
            .max()
            .unwrap_or(0)
    }

    fn source_records(&self) -> Vec<SourceRecord<V>> {
        let mut records = Vec::new();
        if let Some(iter) = self.inner.iter_prefix_with_values(&[BYTE_SOURCE_TAG]) {
            for (_, value) in iter {
                if let PersistentSuffixValue::Source(record) = value {
                    records.push(record);
                }
            }
        }
        records.sort_by_key(|record| record.id);
        records
    }

    fn active_source_ids(&self) -> HashSet<u64> {
        self.source_records()
            .into_iter()
            .filter(|record| record.active)
            .map(|record| record.id)
            .collect()
    }

    fn state(&self) -> SuffixState {
        match self.inner.get_value_bytes(BYTE_STATE_KEY) {
            Some(PersistentSuffixValue::State(state)) => state,
            _ => SuffixState::default(),
        }
    }

    fn ensure_state(&self) -> Result<()> {
        if self.inner.get_value_bytes(BYTE_STATE_KEY).is_none() {
            self.upsert_bytes(
                BYTE_STATE_KEY,
                PersistentSuffixValue::State(SuffixState::default()),
            )?;
        }
        Ok(())
    }

    fn set_state(&self, state: SuffixState) -> Result<()> {
        self.upsert_bytes(BYTE_STATE_KEY, PersistentSuffixValue::State(state))?;
        Ok(())
    }

    fn upsert_bytes(&self, key: &[u8], value: PersistentSuffixValue<V>) -> Result<bool> {
        <PersistentARTrie<PersistentSuffixValue<V>, S> as DurableOverlayWrite<
            ByteKey,
            PersistentSuffixValue<V>,
            S,
        >>::upsert_cas_durable_default(&self.inner, key, value)
    }

    fn cas_update_bytes<F>(&self, key: &[u8], mut update: F) -> Result<bool>
    where
        F: FnMut(Option<PersistentSuffixValue<V>>) -> PersistentSuffixValue<V>,
    {
        loop {
            let current = self.inner.get_value_bytes(key);
            let new_value = update(current.clone());
            let swapped = <PersistentARTrie<PersistentSuffixValue<V>, S> as DurableOverlayWrite<
                ByteKey,
                PersistentSuffixValue<V>,
                S,
            >>::compare_and_swap_cas_durable_default(
                &self.inner, key, current, new_value
            )?;
            if swapped {
                return Ok(true);
            }
            std::hint::spin_loop();
        }
    }

    fn compare_and_swap_bytes(
        &self,
        key: &[u8],
        current: Option<PersistentSuffixValue<V>>,
        new_value: PersistentSuffixValue<V>,
    ) -> Result<bool> {
        <PersistentARTrie<PersistentSuffixValue<V>, S> as DurableOverlayWrite<
            ByteKey,
            PersistentSuffixValue<V>,
            S,
        >>::compare_and_swap_cas_durable_default(&self.inner, key, current, new_value)
    }

    fn merge_suffix_position(
        &self,
        key: &[u8],
        source_id: u64,
        start_byte: usize,
        value: Option<V>,
    ) -> Result<()> {
        self.cas_update_bytes(key, |current| {
            let mut payload = match current {
                Some(PersistentSuffixValue::Suffix(payload)) => payload,
                _ => SuffixPayload::default(),
            };
            if !payload
                .positions
                .iter()
                .any(|pos| pos.source_id == source_id && pos.start_byte == start_byte)
            {
                payload.positions.push(SuffixPosition {
                    source_id,
                    start_byte,
                });
            }
            if let Some(value) = value.clone() {
                payload.value = Some(value);
            }
            PersistentSuffixValue::Suffix(payload)
        })?;
        Ok(())
    }

    fn set_suffix_value(&self, suffix: &[u8], value: V) -> Result<()> {
        let key = byte_data_key(suffix);
        self.cas_update_bytes(&key, |current| {
            let mut payload = match current {
                Some(PersistentSuffixValue::Suffix(payload)) => payload,
                _ => SuffixPayload::default(),
            };
            payload.value = Some(value.clone());
            PersistentSuffixValue::Suffix(payload)
        })?;
        Ok(())
    }

    fn explicit_suffix_values(&self) -> Vec<(Vec<u8>, V)> {
        let mut values = Vec::new();
        if let Some(iter) = self.inner.iter_prefix_with_values(&[BYTE_DATA_TAG]) {
            for (key, value) in iter {
                let PersistentSuffixValue::Suffix(payload) = value else {
                    continue;
                };
                if let Some(value) = payload.value {
                    if key.first() == Some(&BYTE_DATA_TAG) {
                        values.push((key[1..].to_vec(), value));
                    }
                }
            }
        }
        values
    }

    fn insert_suffixes_for_source(
        &self,
        source_id: u64,
        text: &str,
        value: Option<V>,
    ) -> Result<()> {
        if text.is_empty() {
            self.merge_suffix_position(&byte_data_key(&[]), source_id, 0, value)?;
            return Ok(());
        }
        for start in sorted_byte_suffix_starts(text) {
            let suffix = &text.as_bytes()[start..];
            let key = byte_data_key(suffix);
            let suffix_value = if start == 0 { value.clone() } else { None };
            self.merge_suffix_position(&key, source_id, start, suffix_value)?;
        }
        Ok(())
    }

    pub fn try_insert(&self, text: &str) -> Result<bool> {
        self.try_insert_with_value_internal(text, None)
    }

    pub fn try_insert_with_value(&self, text: &str, value: V) -> Result<bool> {
        self.try_insert_with_value_internal(text, Some(value))
    }

    fn try_insert_with_value_internal(&self, text: &str, value: Option<V>) -> Result<bool> {
        let source_id = self.next_source_id.fetch_add(1, Ordering::AcqRel);
        self.insert_suffixes_for_source(source_id, text, value.clone())?;
        let source = SourceRecord {
            id: source_id,
            text: text.to_string(),
            value,
            active: true,
        };
        self.upsert_bytes(
            &byte_source_key(source_id),
            PersistentSuffixValue::Source(source),
        )?;
        Ok(true)
    }

    pub fn insert(&self, text: &str) -> bool {
        self.try_insert(text).unwrap_or_else(|error| {
            log::warn!("PersistentSuffixAutomaton::insert failed: {error}");
            false
        })
    }

    pub fn insert_with_value(&self, text: &str, value: V) -> bool {
        self.try_insert_with_value(text, value)
            .unwrap_or_else(|error| {
                log::warn!("PersistentSuffixAutomaton::insert_with_value failed: {error}");
                false
            })
    }

    pub fn try_remove(&self, text: &str) -> Result<bool> {
        for record in self.source_records() {
            if record.active && record.text == text {
                let mut inactive = record.clone();
                inactive.active = false;
                let key = byte_source_key(record.id);
                let removed = self.compare_and_swap_bytes(
                    &key,
                    Some(PersistentSuffixValue::Source(record)),
                    PersistentSuffixValue::Source(inactive),
                )?;
                if removed {
                    let mut state = self.state();
                    state.needs_compaction = true;
                    self.set_state(state)?;
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    pub fn remove(&self, text: &str) -> bool {
        self.try_remove(text).unwrap_or_else(|error| {
            log::warn!("PersistentSuffixAutomaton::remove failed: {error}");
            false
        })
    }

    pub fn try_clear(&self) -> Result<()> {
        self.inner.remove_prefix(&[BYTE_DATA_TAG]);
        self.inner.remove_prefix(&[BYTE_SOURCE_TAG]);
        self.inner.remove_prefix(&[BYTE_META_TAG]);
        self.next_source_id.store(0, Ordering::Release);
        self.set_state(SuffixState::default())
    }

    pub fn clear(&self) {
        if let Err(error) = self.try_clear() {
            log::warn!("PersistentSuffixAutomaton::clear failed: {error}");
        }
    }

    pub fn try_compact(&self) -> Result<usize> {
        if !self.needs_compaction() {
            return Ok(0);
        }
        let active_sources: Vec<_> = self
            .source_records()
            .into_iter()
            .filter(|record| record.active)
            .collect();
        let explicit_values = self.explicit_suffix_values();
        let removed = self.inner.remove_prefix(&[BYTE_DATA_TAG]);
        for record in active_sources {
            self.insert_suffixes_for_source(record.id, &record.text, record.value.clone())?;
        }
        for (suffix, value) in explicit_values {
            self.set_suffix_value(&suffix, value)?;
        }
        self.set_state(SuffixState {
            needs_compaction: false,
        })?;
        Ok(removed)
    }

    pub fn compact(&self) {
        if let Err(error) = self.try_compact() {
            log::warn!("PersistentSuffixAutomaton::compact failed: {error}");
        }
    }

    pub fn string_count(&self) -> usize {
        self.source_records()
            .into_iter()
            .filter(|record| record.active)
            .count()
    }

    pub fn needs_compaction(&self) -> bool {
        self.state().needs_compaction
    }

    pub fn match_positions(&self, substring: &str) -> Vec<(usize, usize)> {
        if substring.is_empty() {
            return Vec::new();
        }
        let active = self.active_source_ids();
        let prefix = byte_data_key(substring.as_bytes());
        let Some(iter) = self.inner.iter_prefix_with_values(&prefix) else {
            return Vec::new();
        };
        let mut result = Vec::new();
        for (_, value) in iter {
            let PersistentSuffixValue::Suffix(payload) = value else {
                continue;
            };
            for pos in payload.positions {
                if active.contains(&pos.source_id) {
                    if let Ok(source_id) = usize::try_from(pos.source_id) {
                        result.push((source_id, pos.start_byte + substring.len()));
                    }
                }
            }
        }
        result.sort_unstable();
        result.dedup();
        result
    }

    fn contains_live_suffix_prefix(&self, term: &str) -> bool {
        if term.is_empty() {
            return true;
        }
        let active = self.active_source_ids();
        let prefix = byte_data_key(term.as_bytes());
        let Some(iter) = self.inner.iter_prefix_with_values(&prefix) else {
            return false;
        };
        for (_, value) in iter {
            let PersistentSuffixValue::Suffix(payload) = value else {
                continue;
            };
            if payload.value.is_some()
                || payload
                    .positions
                    .iter()
                    .any(|pos| active.contains(&pos.source_id))
            {
                return true;
            }
        }
        false
    }

    pub fn update_or_insert<F>(&self, term: &str, default_value: V, update_fn: F) -> bool
    where
        F: FnOnce(&mut V),
    {
        let key = byte_data_key(term.as_bytes());
        match self.inner.get_value_bytes(&key) {
            Some(PersistentSuffixValue::Suffix(mut payload)) => {
                if let Some(mut value) = payload.value.clone() {
                    update_fn(&mut value);
                    payload.value = Some(value);
                    self.upsert_bytes(&key, PersistentSuffixValue::Suffix(payload))
                        .unwrap_or_else(|error| {
                            log::warn!(
                                "PersistentSuffixAutomaton::update_or_insert failed: {error}"
                            );
                            false
                        });
                    false
                } else {
                    payload.value = Some(default_value);
                    match self.upsert_bytes(&key, PersistentSuffixValue::Suffix(payload)) {
                        Ok(_) => true,
                        Err(error) => {
                            log::warn!(
                                "PersistentSuffixAutomaton::update_or_insert failed: {error}"
                            );
                            false
                        }
                    }
                }
            }
            _ if self.contains(term) => match self.upsert_bytes(
                &key,
                PersistentSuffixValue::Suffix(SuffixPayload {
                    positions: Vec::new(),
                    value: Some(default_value),
                }),
            ) {
                Ok(_) => true,
                Err(error) => {
                    log::warn!("PersistentSuffixAutomaton::update_or_insert failed: {error}");
                    false
                }
            },
            _ => self.insert_with_value(term, default_value),
        }
    }

    pub fn source_texts(&self) -> Vec<String> {
        self.source_records()
            .into_iter()
            .map(|record| record.text)
            .collect()
    }

    pub fn checkpoint(&self) -> Result<()> {
        self.inner.checkpoint()
    }

    pub fn close(&self) {
        self.inner.close();
    }
}

impl<V: DictionaryValue> PersistentSuffixAutomatonChar<V> {
    pub fn new() -> Self {
        Self::from_inner(PersistentARTrieChar::new())
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

impl<V: DictionaryValue> PersistentSuffixAutomatonChar<V, MmapDiskManager> {
    pub fn create<P: AsRef<Path>>(path: P) -> Result<Self> {
        PersistentARTrieChar::create(path).map(Self::from_inner)
    }

    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        PersistentARTrieChar::open(path).map(Self::from_inner)
    }

    pub fn open_with_recovery<P: AsRef<Path>>(path: P) -> Result<(Self, RecoveryReport)> {
        PersistentARTrieChar::open_with_recovery(path)
            .map(|(inner, report)| (Self::from_inner(inner), report))
    }
}

impl<V: DictionaryValue, S: BlockStorage> PersistentSuffixAutomatonChar<V, S> {
    fn from_inner(inner: PersistentARTrieChar<PersistentSuffixValue<V>, S>) -> Self {
        let dict = Self {
            inner,
            next_source_id: AtomicU64::new(0),
        };
        dict.next_source_id
            .store(dict.derive_next_source_id(), Ordering::Release);
        let _ = dict.ensure_state();
        dict
    }

    fn derive_next_source_id(&self) -> u64 {
        self.source_records()
            .into_iter()
            .map(|record| record.id.saturating_add(1))
            .max()
            .unwrap_or(0)
    }

    fn source_prefix() -> String {
        CHAR_SOURCE_TAG.to_string()
    }

    fn source_records(&self) -> Vec<SourceRecord<V>> {
        let mut records = Vec::new();
        if let Ok(Some(entries)) = self.inner.iter_prefix_with_values(&Self::source_prefix()) {
            for (_, value) in entries {
                if let PersistentSuffixValue::Source(record) = value {
                    records.push(record);
                }
            }
        }
        records.sort_by_key(|record| record.id);
        records
    }

    fn active_source_ids(&self) -> HashSet<u64> {
        self.source_records()
            .into_iter()
            .filter(|record| record.active)
            .map(|record| record.id)
            .collect()
    }

    fn state(&self) -> SuffixState {
        match self.inner.get_value(&char_state_key()) {
            Some(PersistentSuffixValue::State(state)) => state,
            _ => SuffixState::default(),
        }
    }

    fn ensure_state(&self) -> Result<()> {
        if self.inner.get_value(&char_state_key()).is_none() {
            self.upsert_str(
                &char_state_key(),
                PersistentSuffixValue::State(SuffixState::default()),
            )?;
        }
        Ok(())
    }

    fn set_state(&self, state: SuffixState) -> Result<()> {
        self.upsert_str(&char_state_key(), PersistentSuffixValue::State(state))?;
        Ok(())
    }

    fn upsert_str(&self, key: &str, value: PersistentSuffixValue<V>) -> Result<bool> {
        <PersistentARTrieChar<PersistentSuffixValue<V>, S> as DurableOverlayWrite<
            CharKey,
            PersistentSuffixValue<V>,
            S,
        >>::upsert_cas_durable_default(&self.inner, key.as_bytes(), value)
    }

    fn cas_update_str<F>(&self, key: &str, mut update: F) -> Result<bool>
    where
        F: FnMut(Option<PersistentSuffixValue<V>>) -> PersistentSuffixValue<V>,
    {
        loop {
            let current = self.inner.get_value(key);
            let new_value = update(current.clone());
            let swapped =
                <PersistentARTrieChar<PersistentSuffixValue<V>, S> as DurableOverlayWrite<
                    CharKey,
                    PersistentSuffixValue<V>,
                    S,
                >>::compare_and_swap_cas_durable_default(
                    &self.inner,
                    key.as_bytes(),
                    current,
                    new_value,
                )?;
            if swapped {
                return Ok(true);
            }
            std::hint::spin_loop();
        }
    }

    fn compare_and_swap_str(
        &self,
        key: &str,
        current: Option<PersistentSuffixValue<V>>,
        new_value: PersistentSuffixValue<V>,
    ) -> Result<bool> {
        <PersistentARTrieChar<PersistentSuffixValue<V>, S> as DurableOverlayWrite<
            CharKey,
            PersistentSuffixValue<V>,
            S,
        >>::compare_and_swap_cas_durable_default(
            &self.inner, key.as_bytes(), current, new_value
        )
    }

    fn merge_suffix_position(
        &self,
        key: &str,
        source_id: u64,
        start_byte: usize,
        value: Option<V>,
    ) -> Result<()> {
        self.cas_update_str(key, |current| {
            let mut payload = match current {
                Some(PersistentSuffixValue::Suffix(payload)) => payload,
                _ => SuffixPayload::default(),
            };
            if !payload
                .positions
                .iter()
                .any(|pos| pos.source_id == source_id && pos.start_byte == start_byte)
            {
                payload.positions.push(SuffixPosition {
                    source_id,
                    start_byte,
                });
            }
            if let Some(value) = value.clone() {
                payload.value = Some(value);
            }
            PersistentSuffixValue::Suffix(payload)
        })?;
        Ok(())
    }

    fn set_suffix_value(&self, suffix: &str, value: V) -> Result<()> {
        let key = char_data_key(suffix);
        self.cas_update_str(&key, |current| {
            let mut payload = match current {
                Some(PersistentSuffixValue::Suffix(payload)) => payload,
                _ => SuffixPayload::default(),
            };
            payload.value = Some(value.clone());
            PersistentSuffixValue::Suffix(payload)
        })?;
        Ok(())
    }

    fn explicit_suffix_values(&self) -> Vec<(String, V)> {
        let mut values = Vec::new();
        let Ok(Some(entries)) = self
            .inner
            .iter_prefix_with_values(&CHAR_DATA_TAG.to_string())
        else {
            return values;
        };
        for (key, value) in entries {
            let PersistentSuffixValue::Suffix(payload) = value else {
                continue;
            };
            if let Some(value) = payload.value {
                let mut chars = key.chars();
                if chars.next() == Some(CHAR_DATA_TAG) {
                    values.push((chars.as_str().to_string(), value));
                }
            }
        }
        values
    }

    fn insert_suffixes_for_source(
        &self,
        source_id: u64,
        text: &str,
        value: Option<V>,
    ) -> Result<()> {
        if text.is_empty() {
            self.merge_suffix_position(&char_data_key(""), source_id, 0, value)?;
            return Ok(());
        }
        for start in sorted_char_suffix_starts(text) {
            let suffix = &text[start..];
            let key = char_data_key(suffix);
            let suffix_value = if start == 0 { value.clone() } else { None };
            self.merge_suffix_position(&key, source_id, start, suffix_value)?;
        }
        Ok(())
    }

    pub fn try_insert(&self, text: &str) -> Result<bool> {
        self.try_insert_with_value_internal(text, None)
    }

    pub fn try_insert_with_value(&self, text: &str, value: V) -> Result<bool> {
        self.try_insert_with_value_internal(text, Some(value))
    }

    fn try_insert_with_value_internal(&self, text: &str, value: Option<V>) -> Result<bool> {
        let source_id = self.next_source_id.fetch_add(1, Ordering::AcqRel);
        self.insert_suffixes_for_source(source_id, text, value.clone())?;
        let source = SourceRecord {
            id: source_id,
            text: text.to_string(),
            value,
            active: true,
        };
        self.upsert_str(
            &char_source_key(source_id),
            PersistentSuffixValue::Source(source),
        )?;
        Ok(true)
    }

    pub fn insert(&self, text: &str) -> bool {
        self.try_insert(text).unwrap_or_else(|error| {
            log::warn!("PersistentSuffixAutomatonChar::insert failed: {error}");
            false
        })
    }

    pub fn insert_with_value(&self, text: &str, value: V) -> bool {
        self.try_insert_with_value(text, value)
            .unwrap_or_else(|error| {
                log::warn!("PersistentSuffixAutomatonChar::insert_with_value failed: {error}");
                false
            })
    }

    pub fn try_remove(&self, text: &str) -> Result<bool> {
        for record in self.source_records() {
            if record.active && record.text == text {
                let mut inactive = record.clone();
                inactive.active = false;
                let key = char_source_key(record.id);
                let removed = self.compare_and_swap_str(
                    &key,
                    Some(PersistentSuffixValue::Source(record)),
                    PersistentSuffixValue::Source(inactive),
                )?;
                if removed {
                    let mut state = self.state();
                    state.needs_compaction = true;
                    self.set_state(state)?;
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    pub fn remove(&self, text: &str) -> bool {
        self.try_remove(text).unwrap_or_else(|error| {
            log::warn!("PersistentSuffixAutomatonChar::remove failed: {error}");
            false
        })
    }

    pub fn try_clear(&self) -> Result<()> {
        self.inner.remove_prefix(&CHAR_DATA_TAG.to_string())?;
        self.inner.remove_prefix(&Self::source_prefix())?;
        self.inner.remove_prefix(&CHAR_META_TAG.to_string())?;
        self.next_source_id.store(0, Ordering::Release);
        self.set_state(SuffixState::default())
    }

    pub fn clear(&self) {
        if let Err(error) = self.try_clear() {
            log::warn!("PersistentSuffixAutomatonChar::clear failed: {error}");
        }
    }

    pub fn try_compact(&self) -> Result<usize> {
        if !self.needs_compaction() {
            return Ok(0);
        }
        let active_sources: Vec<_> = self
            .source_records()
            .into_iter()
            .filter(|record| record.active)
            .collect();
        let explicit_values = self.explicit_suffix_values();
        let removed = self.inner.remove_prefix(&CHAR_DATA_TAG.to_string())?;
        for record in active_sources {
            self.insert_suffixes_for_source(record.id, &record.text, record.value.clone())?;
        }
        for (suffix, value) in explicit_values {
            self.set_suffix_value(&suffix, value)?;
        }
        self.set_state(SuffixState {
            needs_compaction: false,
        })?;
        Ok(removed)
    }

    pub fn compact(&self) {
        if let Err(error) = self.try_compact() {
            log::warn!("PersistentSuffixAutomatonChar::compact failed: {error}");
        }
    }

    pub fn string_count(&self) -> usize {
        self.source_records()
            .into_iter()
            .filter(|record| record.active)
            .count()
    }

    pub fn needs_compaction(&self) -> bool {
        self.state().needs_compaction
    }

    pub fn match_positions(&self, substring: &str) -> Vec<(usize, usize)> {
        if substring.is_empty() {
            return Vec::new();
        }
        let active = self.active_source_ids();
        let prefix = char_data_key(substring);
        let Ok(Some(entries)) = self.inner.iter_prefix_with_values(&prefix) else {
            return Vec::new();
        };
        let mut result = Vec::new();
        for (_, value) in entries {
            let PersistentSuffixValue::Suffix(payload) = value else {
                continue;
            };
            for pos in payload.positions {
                if active.contains(&pos.source_id) {
                    if let Ok(source_id) = usize::try_from(pos.source_id) {
                        result.push((source_id, pos.start_byte + substring.len()));
                    }
                }
            }
        }
        result.sort_unstable();
        result.dedup();
        result
    }

    fn contains_live_suffix_prefix(&self, term: &str) -> bool {
        if term.is_empty() {
            return true;
        }
        let active = self.active_source_ids();
        let prefix = char_data_key(term);
        let Ok(Some(entries)) = self.inner.iter_prefix_with_values(&prefix) else {
            return false;
        };
        for (_, value) in entries {
            let PersistentSuffixValue::Suffix(payload) = value else {
                continue;
            };
            if payload.value.is_some()
                || payload
                    .positions
                    .iter()
                    .any(|pos| active.contains(&pos.source_id))
            {
                return true;
            }
        }
        false
    }

    pub fn update_or_insert<F>(&self, term: &str, default_value: V, update_fn: F) -> bool
    where
        F: FnOnce(&mut V),
    {
        let key = char_data_key(term);
        match self.inner.get_value(&key) {
            Some(PersistentSuffixValue::Suffix(mut payload)) => {
                if let Some(mut value) = payload.value.clone() {
                    update_fn(&mut value);
                    payload.value = Some(value);
                    self.upsert_str(&key, PersistentSuffixValue::Suffix(payload))
                        .unwrap_or_else(|error| {
                            log::warn!(
                                "PersistentSuffixAutomatonChar::update_or_insert failed: {error}"
                            );
                            false
                        });
                    false
                } else {
                    payload.value = Some(default_value);
                    match self.upsert_str(&key, PersistentSuffixValue::Suffix(payload)) {
                        Ok(_) => true,
                        Err(error) => {
                            log::warn!(
                                "PersistentSuffixAutomatonChar::update_or_insert failed: {error}"
                            );
                            false
                        }
                    }
                }
            }
            _ if self.contains(term) => match self.upsert_str(
                &key,
                PersistentSuffixValue::Suffix(SuffixPayload {
                    positions: Vec::new(),
                    value: Some(default_value),
                }),
            ) {
                Ok(_) => true,
                Err(error) => {
                    log::warn!("PersistentSuffixAutomatonChar::update_or_insert failed: {error}");
                    false
                }
            },
            _ => self.insert_with_value(term, default_value),
        }
    }

    pub fn source_texts(&self) -> Vec<String> {
        self.source_records()
            .into_iter()
            .map(|record| record.text)
            .collect()
    }

    pub fn checkpoint(&self) -> Result<()> {
        self.inner.checkpoint()
    }

    pub fn close(&self) {
        self.inner.close();
    }
}

impl<V: DictionaryValue> DictionaryNode for PersistentSuffixAutomatonNode<V> {
    type Unit = u8;

    fn is_final(&self) -> bool {
        self.inner.as_ref().is_some_and(DictionaryNode::is_final)
    }

    fn transition(&self, label: Self::Unit) -> Option<Self> {
        self.inner
            .as_ref()?
            .transition(label)
            .map(|inner| Self { inner: Some(inner) })
    }

    fn edges(&self) -> Box<dyn Iterator<Item = (Self::Unit, Self)> + '_> {
        let Some(inner) = &self.inner else {
            return Box::new(std::iter::empty());
        };
        let edges: Vec<_> = inner
            .edges()
            .map(|(unit, child)| (unit, Self { inner: Some(child) }))
            .collect();
        Box::new(edges.into_iter())
    }

    fn edge_count(&self) -> Option<usize> {
        self.inner.as_ref().and_then(DictionaryNode::edge_count)
    }
}

impl<V: DictionaryValue> MappedDictionaryNode for PersistentSuffixAutomatonNode<V> {
    type Value = V;

    fn value(&self) -> Option<Self::Value> {
        match self.inner.as_ref()?.value()? {
            PersistentSuffixValue::Suffix(payload) => payload.value,
            _ => None,
        }
    }
}

impl<V: DictionaryValue> DictionaryNode for PersistentSuffixAutomatonCharNode<V> {
    type Unit = char;

    fn is_final(&self) -> bool {
        self.inner.as_ref().is_some_and(DictionaryNode::is_final)
    }

    fn transition(&self, label: Self::Unit) -> Option<Self> {
        self.inner
            .as_ref()?
            .transition(label)
            .map(|inner| Self { inner: Some(inner) })
    }

    fn edges(&self) -> Box<dyn Iterator<Item = (Self::Unit, Self)> + '_> {
        let Some(inner) = &self.inner else {
            return Box::new(std::iter::empty());
        };
        let edges: Vec<_> = inner
            .edges()
            .map(|(unit, child)| (unit, Self { inner: Some(child) }))
            .collect();
        Box::new(edges.into_iter())
    }

    fn edge_count(&self) -> Option<usize> {
        self.inner.as_ref().and_then(DictionaryNode::edge_count)
    }
}

impl<V: DictionaryValue> MappedDictionaryNode for PersistentSuffixAutomatonCharNode<V> {
    type Value = V;

    fn value(&self) -> Option<Self::Value> {
        match self.inner.as_ref()?.value()? {
            PersistentSuffixValue::Suffix(payload) => payload.value,
            _ => None,
        }
    }
}

impl<V: DictionaryValue, S: BlockStorage> Dictionary for PersistentSuffixAutomaton<V, S> {
    type Node = PersistentSuffixAutomatonNode<V>;

    fn root(&self) -> Self::Node {
        Self::Node {
            inner: self.inner.root().transition(BYTE_DATA_TAG),
        }
    }

    fn contains(&self, term: &str) -> bool {
        self.contains_live_suffix_prefix(term)
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

impl<V: DictionaryValue, S: BlockStorage> MappedDictionary for PersistentSuffixAutomaton<V, S> {
    type Value = V;

    fn get_value(&self, term: &str) -> Option<Self::Value> {
        let mut node = self.root();
        for byte in term.bytes() {
            node = node.transition(byte)?;
        }
        node.value()
    }
}

impl<V: DictionaryValue, S: BlockStorage> MutableDictionary for PersistentSuffixAutomaton<V, S> {
    fn insert(&self, term: &str) -> bool {
        PersistentSuffixAutomaton::insert(self, term)
    }

    fn remove(&self, term: &str) -> bool {
        PersistentSuffixAutomaton::remove(self, term)
    }
}

impl<V: DictionaryValue, S: BlockStorage> MutableMappedDictionary
    for PersistentSuffixAutomaton<V, S>
{
    fn insert_with_value(&self, term: &str, value: Self::Value) -> bool {
        PersistentSuffixAutomaton::insert_with_value(self, term, value)
    }

    fn update_or_insert<F>(&self, term: &str, default_value: Self::Value, update_fn: F) -> bool
    where
        F: FnOnce(&mut Self::Value),
    {
        PersistentSuffixAutomaton::update_or_insert(self, term, default_value, update_fn)
    }

    fn union_with<F>(&self, other: &Self, merge_fn: F) -> usize
    where
        F: Fn(&Self::Value, &Self::Value) -> Self::Value,
        Self::Value: Clone,
    {
        let mut processed = 0;
        for term in other.source_texts() {
            if term.is_empty() {
                continue;
            }
            if let Some(other_value) = other.get_value(&term) {
                processed += 1;
                let new_value = if let Some(self_value) = self.get_value(&term) {
                    merge_fn(&self_value, &other_value)
                } else {
                    other_value.clone()
                };
                let replacement = new_value.clone();
                PersistentSuffixAutomaton::update_or_insert(self, &term, new_value, move |value| {
                    *value = replacement
                });
            }
        }
        processed
    }
}

impl<V: DictionaryValue, S: BlockStorage> Dictionary for PersistentSuffixAutomatonChar<V, S> {
    type Node = PersistentSuffixAutomatonCharNode<V>;

    fn root(&self) -> Self::Node {
        Self::Node {
            inner: self.inner.root().transition(CHAR_DATA_TAG),
        }
    }

    fn contains(&self, term: &str) -> bool {
        self.contains_live_suffix_prefix(term)
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

impl<V: DictionaryValue, S: BlockStorage> MappedDictionary for PersistentSuffixAutomatonChar<V, S> {
    type Value = V;

    fn get_value(&self, term: &str) -> Option<Self::Value> {
        let mut node = self.root();
        for ch in term.chars() {
            node = node.transition(ch)?;
        }
        node.value()
    }
}

impl<V: DictionaryValue, S: BlockStorage> MutableDictionary
    for PersistentSuffixAutomatonChar<V, S>
{
    fn insert(&self, term: &str) -> bool {
        PersistentSuffixAutomatonChar::insert(self, term)
    }

    fn remove(&self, term: &str) -> bool {
        PersistentSuffixAutomatonChar::remove(self, term)
    }
}

impl<V: DictionaryValue, S: BlockStorage> MutableMappedDictionary
    for PersistentSuffixAutomatonChar<V, S>
{
    fn insert_with_value(&self, term: &str, value: Self::Value) -> bool {
        PersistentSuffixAutomatonChar::insert_with_value(self, term, value)
    }

    fn update_or_insert<F>(&self, term: &str, default_value: Self::Value, update_fn: F) -> bool
    where
        F: FnOnce(&mut Self::Value),
    {
        PersistentSuffixAutomatonChar::update_or_insert(self, term, default_value, update_fn)
    }

    fn union_with<F>(&self, other: &Self, merge_fn: F) -> usize
    where
        F: Fn(&Self::Value, &Self::Value) -> Self::Value,
        Self::Value: Clone,
    {
        let mut processed = 0;
        for term in other.source_texts() {
            if term.is_empty() {
                continue;
            }
            if let Some(other_value) = other.get_value(&term) {
                processed += 1;
                let new_value = if let Some(self_value) = self.get_value(&term) {
                    merge_fn(&self_value, &other_value)
                } else {
                    other_value.clone()
                };
                let replacement = new_value.clone();
                PersistentSuffixAutomatonChar::update_or_insert(
                    self,
                    &term,
                    new_value,
                    move |value| *value = replacement,
                );
            }
        }
        processed
    }
}

impl<V: DictionaryValue> Default for PersistentSuffixAutomaton<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V: DictionaryValue> Default for PersistentSuffixAutomatonChar<V> {
    fn default() -> Self {
        Self::new()
    }
}
