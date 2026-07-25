#![cfg(feature = "persistent-artrie")]

//! Benchmarks for native persistent suffix/SCDAWG storage.
//!
//! Control: benchmark-local reconstruction of the replaced ARTrie-encoded
//! suffix store: namespaced suffix keys in `PersistentARTrie` /
//! `PersistentARTrieChar`, with serialized suffix payloads and source records.
//! Treatment: the native suffix graph snapshot/WAL now used by
//! `PersistentSuffixAutomaton{,Char}`, `PersistentSuffixTree{,Char}`, and
//! `PersistentScdawg{,Char}`.
//!
//! Fixed-sample mode prints raw per-round samples for pgmcp/Welch testing:
//!
//! ```bash
//! PERSISTENT_SUFFIX_FIXED_SAMPLES=1 cargo bench --bench persistent_suffix_native_benchmarks --features persistent-artrie
//! ```

use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use criterion::{BenchmarkId, Criterion, Throughput};
use libdictenstein::persistent_artrie::char::PersistentARTrieChar;
use libdictenstein::persistent_artrie::{
    PersistentARTrie, PersistentScdawg, PersistentScdawgChar, PersistentSuffixAutomaton,
    PersistentSuffixAutomatonChar, PersistentSuffixTree, PersistentSuffixTreeChar,
};
use serde::{Deserialize, Serialize};
use std::hint::black_box;

const BYTE_DATA_TAG: u8 = 0;
const BYTE_SOURCE_TAG: u8 = 1;
const CHAR_DATA_TAG: char = '\u{E000}';
const CHAR_SOURCE_TAG: char = '\u{E001}';

const TEXT_COUNT: usize = 256;
const TEXT_LEN: usize = 48;
const QUERY_COUNT: usize = 2_048;
const QUERY_LEN: usize = 12;
const FIXED_SAMPLES: usize = 51;
const FIXED_WARMUPS: usize = 3;
const PARALLEL_READERS: usize = 4;
const OPS_PER_READER: usize = 2_000;
const WRITES_PER_SAMPLE: usize = 128;
const DISK_SAMPLE_TEXT_COUNT: usize = 32;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
enum LegacySuffixValue {
    #[default]
    Empty,
    Suffix {
        positions: Vec<LegacyPosition>,
    },
    Source {
        id: u64,
        text: String,
        active: bool,
    },
}

impl libdictenstein::DictionaryValue for LegacySuffixValue {}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct LegacyPosition {
    source_id: u64,
    start_byte: usize,
}

struct LegacyByteSuffix {
    inner: PersistentARTrie<LegacySuffixValue>,
    next_source_id: AtomicU64,
}

struct LegacyCharSuffix {
    inner: PersistentARTrieChar<LegacySuffixValue>,
    next_source_id: AtomicU64,
}

fn mix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn ascii_texts(count: usize, len: usize) -> Vec<String> {
    ascii_texts_with_seed(count, len, 0)
}

fn ascii_texts_with_seed(count: usize, len: usize, seed: u64) -> Vec<String> {
    let alphabet = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut texts = Vec::with_capacity(count);
    for i in 0..count {
        let mut text = String::with_capacity(len);
        for j in 0..len {
            let value = mix64(seed ^ ((i as u64) << 32) ^ j as u64) as usize;
            text.push(alphabet[value % alphabet.len()] as char);
        }
        texts.push(text);
    }
    texts
}

fn unicode_texts(count: usize, len: usize) -> Vec<String> {
    unicode_texts_with_seed(count, len, 0)
}

fn unicode_texts_with_seed(count: usize, len: usize, seed: u64) -> Vec<String> {
    const ALPHABET: &[char] = &[
        'a', 'b', 'c', 'é', 'ï', 'ñ', '日', '本', '語', '東', '京', '文', 'ß', 'ø', 'λ', 'Ж',
    ];
    let mut texts = Vec::with_capacity(count);
    for i in 0..count {
        let mut text = String::new();
        for j in 0..len {
            let value = mix64(seed ^ ((i as u64) << 32) ^ j as u64) as usize;
            text.push(ALPHABET[value % ALPHABET.len()]);
        }
        texts.push(text);
    }
    texts
}

fn byte_queries(texts: &[String], count: usize, len: usize) -> Vec<String> {
    let mut queries = Vec::with_capacity(count);
    for i in 0..count {
        let text = &texts[i % texts.len()];
        let max_start = text.len().saturating_sub(len);
        let start = mix64(i as u64) as usize % (max_start + 1);
        let mut query = text[start..start + len].to_string();
        if i % 2 == 1 {
            query.replace_range(0..1, "~");
        }
        queries.push(query);
    }
    queries
}

fn char_queries(texts: &[String], count: usize, len: usize) -> Vec<String> {
    let mut queries = Vec::with_capacity(count);
    for i in 0..count {
        let chars: Vec<char> = texts[i % texts.len()].chars().collect();
        let max_start = chars.len().saturating_sub(len);
        let start = mix64(i as u64) as usize % (max_start + 1);
        let mut query: String = chars[start..start + len].iter().collect();
        if i % 2 == 1 {
            query.replace_range(..query.chars().next().unwrap().len_utf8(), "Ω");
        }
        queries.push(query);
    }
    queries
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

impl LegacyByteSuffix {
    fn new() -> Self {
        #[allow(deprecated)]
        let inner = PersistentARTrie::new();
        Self {
            inner,
            next_source_id: AtomicU64::new(0),
        }
    }

    fn create(path: &Path) -> Self {
        Self {
            inner: PersistentARTrie::create(path).expect("create legacy byte suffix trie"),
            next_source_id: AtomicU64::new(0),
        }
    }

    fn insert(&self, text: &str) {
        let source_id = self.next_source_id.fetch_add(1, Ordering::AcqRel);
        if text.is_empty() {
            self.merge_suffix_position(&byte_data_key(&[]), source_id, 0);
        } else {
            for start in sorted_byte_suffix_starts(text) {
                self.merge_suffix_position(
                    &byte_data_key(&text.as_bytes()[start..]),
                    source_id,
                    start,
                );
            }
        }
        self.inner
            .upsert_bytes(
                &byte_source_key(source_id),
                LegacySuffixValue::Source {
                    id: source_id,
                    text: text.to_string(),
                    active: true,
                },
            )
            .expect("upsert legacy byte source");
    }

    fn merge_suffix_position(&self, key: &[u8], source_id: u64, start_byte: usize) {
        let mut positions = match self.inner.get_value_bytes(key) {
            Some(LegacySuffixValue::Suffix { positions }) => positions,
            _ => Vec::new(),
        };
        if !positions
            .iter()
            .any(|position| position.source_id == source_id && position.start_byte == start_byte)
        {
            positions.push(LegacyPosition {
                source_id,
                start_byte,
            });
        }
        self.inner
            .upsert_bytes(key, LegacySuffixValue::Suffix { positions })
            .expect("upsert legacy byte suffix");
    }

    fn active_source_ids(&self) -> HashSet<u64> {
        let Some(iter) = self.inner.iter_prefix_with_values(&[BYTE_SOURCE_TAG]) else {
            return HashSet::new();
        };
        iter.filter_map(|(_, value)| match value {
            LegacySuffixValue::Source { id, active, .. } if active => Some(id),
            _ => None,
        })
        .collect()
    }

    fn source_texts(&self) -> Vec<String> {
        let Some(iter) = self.inner.iter_prefix_with_values(&[BYTE_SOURCE_TAG]) else {
            return Vec::new();
        };
        let mut records: Vec<_> = iter
            .filter_map(|(_, value)| match value {
                LegacySuffixValue::Source { id, text, .. } => Some((id, text)),
                _ => None,
            })
            .collect();
        records.sort_by_key(|(id, _)| *id);
        records.into_iter().map(|(_, text)| text).collect()
    }

    fn match_positions(&self, substring: &str) -> Vec<(usize, usize)> {
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
            let LegacySuffixValue::Suffix { positions } = value else {
                continue;
            };
            for position in positions {
                if !active.contains(&position.source_id) {
                    continue;
                }
                if let Ok(source_id) = usize::try_from(position.source_id) {
                    result.push((source_id, position.start_byte + substring.len()));
                }
            }
        }
        result.sort_unstable();
        result.dedup();
        result
    }

    fn locations(&self, pattern: &str) -> Vec<(String, usize)> {
        let texts = self.source_texts();
        self.match_positions(pattern)
            .into_iter()
            .filter_map(|(source_id, finish)| {
                let term = texts.get(source_id)?;
                Some((term.clone(), finish.checked_sub(pattern.len())?))
            })
            .collect()
    }

    fn checkpoint(&self) {
        self.inner.checkpoint().expect("legacy byte checkpoint");
    }
}

impl LegacyCharSuffix {
    fn new() -> Self {
        Self {
            inner: PersistentARTrieChar::new(),
            next_source_id: AtomicU64::new(0),
        }
    }

    fn create(path: &Path) -> Self {
        Self {
            inner: PersistentARTrieChar::create(path).expect("create legacy char suffix trie"),
            next_source_id: AtomicU64::new(0),
        }
    }

    fn insert(&self, text: &str) {
        let source_id = self.next_source_id.fetch_add(1, Ordering::AcqRel);
        if text.is_empty() {
            self.merge_suffix_position(&char_data_key(""), source_id, 0);
        } else {
            for start in sorted_char_suffix_starts(text) {
                self.merge_suffix_position(&char_data_key(&text[start..]), source_id, start);
            }
        }
        self.inner
            .upsert(
                &char_source_key(source_id),
                LegacySuffixValue::Source {
                    id: source_id,
                    text: text.to_string(),
                    active: true,
                },
            )
            .expect("upsert legacy char source");
    }

    fn merge_suffix_position(&self, key: &str, source_id: u64, start_byte: usize) {
        let mut positions = match self.inner.get_value(key) {
            Some(LegacySuffixValue::Suffix { positions }) => positions,
            _ => Vec::new(),
        };
        if !positions
            .iter()
            .any(|position| position.source_id == source_id && position.start_byte == start_byte)
        {
            positions.push(LegacyPosition {
                source_id,
                start_byte,
            });
        }
        self.inner
            .upsert(key, LegacySuffixValue::Suffix { positions })
            .expect("upsert legacy char suffix");
    }

    fn active_source_ids(&self) -> HashSet<u64> {
        let prefix = CHAR_SOURCE_TAG.to_string();
        let Ok(Some(entries)) = self.inner.iter_prefix_with_values(&prefix) else {
            return HashSet::new();
        };
        entries
            .into_iter()
            .filter_map(|(_, value)| match value {
                LegacySuffixValue::Source { id, active, .. } if active => Some(id),
                _ => None,
            })
            .collect()
    }

    fn source_texts(&self) -> Vec<String> {
        let prefix = CHAR_SOURCE_TAG.to_string();
        let Ok(Some(entries)) = self.inner.iter_prefix_with_values(&prefix) else {
            return Vec::new();
        };
        let mut records: Vec<_> = entries
            .into_iter()
            .filter_map(|(_, value)| match value {
                LegacySuffixValue::Source { id, text, .. } => Some((id, text)),
                _ => None,
            })
            .collect();
        records.sort_by_key(|(id, _)| *id);
        records.into_iter().map(|(_, text)| text).collect()
    }

    fn match_positions(&self, substring: &str) -> Vec<(usize, usize)> {
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
            let LegacySuffixValue::Suffix { positions } = value else {
                continue;
            };
            for position in positions {
                if !active.contains(&position.source_id) {
                    continue;
                }
                if let Ok(source_id) = usize::try_from(position.source_id) {
                    result.push((source_id, position.start_byte + substring.len()));
                }
            }
        }
        result.sort_unstable();
        result.dedup();
        result
    }

    fn locations(&self, pattern: &str) -> Vec<(String, usize)> {
        let texts = self.source_texts();
        self.match_positions(pattern)
            .into_iter()
            .filter_map(|(source_id, finish_byte)| {
                let term = texts.get(source_id)?;
                let start_byte = finish_byte.checked_sub(pattern.len())?;
                if start_byte > term.len() || !term.is_char_boundary(start_byte) {
                    return None;
                }
                Some((term.clone(), term[..start_byte].chars().count()))
            })
            .collect()
    }

    fn checkpoint(&self) {
        self.inner.checkpoint().expect("legacy char checkpoint");
    }
}

fn build_native_byte_suffix(texts: &[String]) -> PersistentSuffixAutomaton<()> {
    let dict = PersistentSuffixAutomaton::new();
    for text in texts {
        dict.insert(text);
    }
    dict
}

fn build_native_char_suffix(texts: &[String]) -> PersistentSuffixAutomatonChar<()> {
    let dict = PersistentSuffixAutomatonChar::new();
    for text in texts {
        dict.insert(text);
    }
    dict
}

fn build_native_byte_suffix_tree(texts: &[String]) -> PersistentSuffixTree<()> {
    let dict = PersistentSuffixTree::new();
    for text in texts {
        dict.insert(text);
    }
    dict
}

fn build_native_char_suffix_tree(texts: &[String]) -> PersistentSuffixTreeChar<()> {
    let dict = PersistentSuffixTreeChar::new();
    for text in texts {
        dict.insert(text);
    }
    dict
}

fn build_native_byte_scdawg(texts: &[String]) -> PersistentScdawg<()> {
    let dict = PersistentScdawg::new();
    for text in texts {
        dict.insert(text);
    }
    dict
}

fn build_native_char_scdawg(texts: &[String]) -> PersistentScdawgChar<()> {
    let dict = PersistentScdawgChar::new();
    for text in texts {
        dict.insert(text);
    }
    dict
}

fn build_legacy_byte(texts: &[String]) -> LegacyByteSuffix {
    let dict = LegacyByteSuffix::new();
    for text in texts {
        dict.insert(text);
    }
    dict
}

fn build_legacy_char(texts: &[String]) -> LegacyCharSuffix {
    let dict = LegacyCharSuffix::new();
    for text in texts {
        dict.insert(text);
    }
    dict
}

fn lookup_native_byte_suffix(dict: &PersistentSuffixAutomaton<()>, queries: &[String]) -> usize {
    let mut hits = 0usize;
    for query in queries {
        if !dict.match_positions(black_box(query)).is_empty() {
            hits += 1;
        }
    }
    black_box(hits)
}

fn lookup_native_char_suffix(
    dict: &PersistentSuffixAutomatonChar<()>,
    queries: &[String],
) -> usize {
    let mut hits = 0usize;
    for query in queries {
        if !dict.match_positions(black_box(query)).is_empty() {
            hits += 1;
        }
    }
    black_box(hits)
}

fn lookup_native_byte_suffix_tree(dict: &PersistentSuffixTree<()>, queries: &[String]) -> usize {
    let mut hits = 0usize;
    for query in queries {
        if !dict.locations(black_box(query)).is_empty() {
            hits += 1;
        }
    }
    black_box(hits)
}

fn lookup_native_char_suffix_tree(
    dict: &PersistentSuffixTreeChar<()>,
    queries: &[String],
) -> usize {
    let mut hits = 0usize;
    for query in queries {
        if !dict.locations(black_box(query)).is_empty() {
            hits += 1;
        }
    }
    black_box(hits)
}

fn lookup_legacy_byte_suffix(dict: &LegacyByteSuffix, queries: &[String]) -> usize {
    let mut hits = 0usize;
    for query in queries {
        if !dict.match_positions(black_box(query)).is_empty() {
            hits += 1;
        }
    }
    black_box(hits)
}

fn lookup_legacy_char_suffix(dict: &LegacyCharSuffix, queries: &[String]) -> usize {
    let mut hits = 0usize;
    for query in queries {
        if !dict.match_positions(black_box(query)).is_empty() {
            hits += 1;
        }
    }
    black_box(hits)
}

fn lookup_native_byte_scdawg(dict: &PersistentScdawg<()>, queries: &[String]) -> usize {
    let mut hits = 0usize;
    for query in queries {
        if !dict.locations(black_box(query)).is_empty() {
            hits += 1;
        }
    }
    black_box(hits)
}

fn lookup_native_char_scdawg(dict: &PersistentScdawgChar<()>, queries: &[String]) -> usize {
    let mut hits = 0usize;
    for query in queries {
        if !dict.locations(black_box(query)).is_empty() {
            hits += 1;
        }
    }
    black_box(hits)
}

fn lookup_legacy_byte_scdawg(dict: &LegacyByteSuffix, queries: &[String]) -> usize {
    let mut hits = 0usize;
    for query in queries {
        if !dict.locations(black_box(query)).is_empty() {
            hits += 1;
        }
    }
    black_box(hits)
}

fn lookup_legacy_char_scdawg(dict: &LegacyCharSuffix, queries: &[String]) -> usize {
    let mut hits = 0usize;
    for query in queries {
        if !dict.locations(black_box(query)).is_empty() {
            hits += 1;
        }
    }
    black_box(hits)
}

fn directory_bytes(path: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(path) = stack.pop() {
        let Ok(meta) = fs::metadata(&path) else {
            continue;
        };
        if meta.is_file() {
            total += meta.len();
        } else if meta.is_dir() {
            let Ok(entries) = fs::read_dir(&path) else {
                continue;
            };
            for entry in entries.flatten() {
                stack.push(entry.path());
            }
        }
    }
    total
}

fn scratch_dir() -> std::path::PathBuf {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/bench-scratch");
    fs::create_dir_all(&path).expect("create bench scratch dir");
    path
}

fn checkpoint_native_byte_bytes(texts: &[String]) -> u64 {
    let dir = tempfile::Builder::new()
        .prefix("native_suffix_byte")
        .tempdir_in(scratch_dir())
        .expect("native suffix byte tempdir");
    let path = dir.path().join("native.psuf");
    let dict = PersistentSuffixAutomaton::<()>::create(&path).expect("create native byte suffix");
    for text in texts {
        dict.insert(text);
    }
    dict.checkpoint().expect("native byte suffix checkpoint");
    directory_bytes(dir.path())
}

fn checkpoint_legacy_byte_bytes(texts: &[String]) -> u64 {
    let dir = tempfile::Builder::new()
        .prefix("legacy_suffix_byte")
        .tempdir_in(scratch_dir())
        .expect("legacy suffix byte tempdir");
    let path = dir.path().join("legacy.part");
    let dict = LegacyByteSuffix::create(&path);
    for text in texts {
        dict.insert(text);
    }
    dict.checkpoint();
    directory_bytes(dir.path())
}

fn checkpoint_native_char_bytes(texts: &[String]) -> u64 {
    let dir = tempfile::Builder::new()
        .prefix("native_suffix_char")
        .tempdir_in(scratch_dir())
        .expect("native suffix char tempdir");
    let path = dir.path().join("native.psufc");
    let dict =
        PersistentSuffixAutomatonChar::<()>::create(&path).expect("create native char suffix");
    for text in texts {
        dict.insert(text);
    }
    dict.checkpoint().expect("native char suffix checkpoint");
    directory_bytes(dir.path())
}

fn checkpoint_native_byte_suffix_tree_bytes(texts: &[String]) -> u64 {
    let dir = tempfile::Builder::new()
        .prefix("native_suffix_tree_byte")
        .tempdir_in(scratch_dir())
        .expect("native suffix tree byte tempdir");
    let path = dir.path().join("native.pstree");
    let dict = PersistentSuffixTree::<()>::create(&path).expect("create native byte suffix tree");
    for text in texts {
        dict.insert(text);
    }
    dict.checkpoint()
        .expect("native byte suffix tree checkpoint");
    directory_bytes(dir.path())
}

fn checkpoint_native_char_suffix_tree_bytes(texts: &[String]) -> u64 {
    let dir = tempfile::Builder::new()
        .prefix("native_suffix_tree_char")
        .tempdir_in(scratch_dir())
        .expect("native suffix tree char tempdir");
    let path = dir.path().join("native.pstreec");
    let dict =
        PersistentSuffixTreeChar::<()>::create(&path).expect("create native char suffix tree");
    for text in texts {
        dict.insert(text);
    }
    dict.checkpoint()
        .expect("native char suffix tree checkpoint");
    directory_bytes(dir.path())
}

fn checkpoint_native_byte_scdawg_bytes(texts: &[String]) -> u64 {
    let dir = tempfile::Builder::new()
        .prefix("native_scdawg_byte")
        .tempdir_in(scratch_dir())
        .expect("native scdawg byte tempdir");
    let path = dir.path().join("native.pscdawg");
    let dict = PersistentScdawg::<()>::create(&path).expect("create native byte scdawg");
    for text in texts {
        dict.insert(text);
    }
    dict.checkpoint().expect("native byte scdawg checkpoint");
    directory_bytes(dir.path())
}

fn checkpoint_native_char_scdawg_bytes(texts: &[String]) -> u64 {
    let dir = tempfile::Builder::new()
        .prefix("native_scdawg_char")
        .tempdir_in(scratch_dir())
        .expect("native scdawg char tempdir");
    let path = dir.path().join("native.pscdawgc");
    let dict = PersistentScdawgChar::<()>::create(&path).expect("create native char scdawg");
    for text in texts {
        dict.insert(text);
    }
    dict.checkpoint().expect("native char scdawg checkpoint");
    directory_bytes(dir.path())
}

fn checkpoint_legacy_char_bytes(texts: &[String]) -> u64 {
    let dir = tempfile::Builder::new()
        .prefix("legacy_suffix_char")
        .tempdir_in(scratch_dir())
        .expect("legacy suffix char tempdir");
    let path = dir.path().join("legacy.partc");
    let dict = LegacyCharSuffix::create(&path);
    for text in texts {
        dict.insert(text);
    }
    dict.checkpoint();
    directory_bytes(dir.path())
}

fn time_parallel_dictionary<T, Build, Read, Write>(
    texts: &[String],
    queries: &[String],
    build: Build,
    read: Read,
    write: Write,
) -> Duration
where
    T: Send + Sync + 'static,
    Build: Fn(&[String]) -> T,
    Read: Fn(&T, &str) -> bool + Copy + Send + Sync + 'static,
    Write: Fn(&T, &str) + Copy + Send + Sync + 'static,
{
    let dict = Arc::new(build(&texts[..texts.len() / 2]));
    let stop = Arc::new(AtomicBool::new(false));
    let barrier = Arc::new(Barrier::new(PARALLEL_READERS + 2));
    let mut handles = Vec::with_capacity(PARALLEL_READERS);

    for reader in 0..PARALLEL_READERS {
        let dict = Arc::clone(&dict);
        let barrier = Arc::clone(&barrier);
        let queries = queries.to_vec();
        handles.push(thread::spawn(move || {
            barrier.wait();
            let mut hits = 0usize;
            for op in 0..OPS_PER_READER {
                let index =
                    op.wrapping_mul(2_654_435_761).wrapping_add(reader * 17) % queries.len();
                if read(&dict, &queries[index]) {
                    hits += 1;
                }
            }
            black_box(hits)
        }));
    }

    let writer = {
        let dict = Arc::clone(&dict);
        let barrier = Arc::clone(&barrier);
        let stop = Arc::clone(&stop);
        let texts = texts.to_vec();
        thread::spawn(move || {
            barrier.wait();
            let mut writes = 0usize;
            while !stop.load(Ordering::Relaxed) && writes < WRITES_PER_SAMPLE {
                let index = texts.len() / 2 + writes % (texts.len() / 2);
                write(&dict, &texts[index]);
                writes += 1;
            }
            black_box(writes)
        })
    };

    barrier.wait();
    let start = Instant::now();
    for handle in handles {
        let _ = handle.join();
    }
    let elapsed = start.elapsed();
    stop.store(true, Ordering::Relaxed);
    let _ = writer.join();
    elapsed
}

fn time_parallel_native_byte(texts: &[String], queries: &[String]) -> Duration {
    time_parallel_dictionary(
        texts,
        queries,
        build_native_byte_suffix,
        |dict, query| !dict.match_positions(query).is_empty(),
        |dict, text| {
            dict.insert(text);
        },
    )
}

fn time_parallel_native_char(texts: &[String], queries: &[String]) -> Duration {
    time_parallel_dictionary(
        texts,
        queries,
        build_native_char_suffix,
        |dict, query| !dict.match_positions(query).is_empty(),
        |dict, text| {
            dict.insert(text);
        },
    )
}

fn time_parallel_native_byte_suffix_tree(texts: &[String], queries: &[String]) -> Duration {
    time_parallel_dictionary(
        texts,
        queries,
        build_native_byte_suffix_tree,
        |dict, query| !dict.locations(query).is_empty(),
        |dict, text| {
            dict.insert(text);
        },
    )
}

fn time_parallel_native_char_suffix_tree(texts: &[String], queries: &[String]) -> Duration {
    time_parallel_dictionary(
        texts,
        queries,
        build_native_char_suffix_tree,
        |dict, query| !dict.locations(query).is_empty(),
        |dict, text| {
            dict.insert(text);
        },
    )
}

fn time_parallel_native_byte_scdawg(texts: &[String], queries: &[String]) -> Duration {
    time_parallel_dictionary(
        texts,
        queries,
        build_native_byte_scdawg,
        |dict, query| !dict.locations(query).is_empty(),
        |dict, text| {
            dict.insert(text);
        },
    )
}

fn time_parallel_native_char_scdawg(texts: &[String], queries: &[String]) -> Duration {
    time_parallel_dictionary(
        texts,
        queries,
        build_native_char_scdawg,
        |dict, query| !dict.locations(query).is_empty(),
        |dict, text| {
            dict.insert(text);
        },
    )
}

fn time_parallel_legacy_byte(texts: &[String], queries: &[String]) -> Duration {
    time_parallel_dictionary(
        texts,
        queries,
        build_legacy_byte,
        |dict, query| !dict.match_positions(query).is_empty(),
        |dict, text| dict.insert(text),
    )
}

fn time_parallel_legacy_char(texts: &[String], queries: &[String]) -> Duration {
    time_parallel_dictionary(
        texts,
        queries,
        build_legacy_char,
        |dict, query| !dict.match_positions(query).is_empty(),
        |dict, text| dict.insert(text),
    )
}

fn time_parallel_legacy_byte_locations(texts: &[String], queries: &[String]) -> Duration {
    time_parallel_dictionary(
        texts,
        queries,
        build_legacy_byte,
        |dict, query| !dict.locations(query).is_empty(),
        |dict, text| dict.insert(text),
    )
}

fn time_parallel_legacy_char_locations(texts: &[String], queries: &[String]) -> Duration {
    time_parallel_dictionary(
        texts,
        queries,
        build_legacy_char,
        |dict, query| !dict.locations(query).is_empty(),
        |dict, text| dict.insert(text),
    )
}

fn print_sample_line(metric: &str, arm: &str, unit: &str, samples: &[f64]) {
    print!("metric={metric},arm={arm},unit={unit},samples=");
    for (index, sample) in samples.iter().enumerate() {
        if index > 0 {
            print!(";");
        }
        print!("{sample:.6}");
    }
    println!();
}

fn collect_samples<F>(mut f: F, divisor: f64) -> Vec<f64>
where
    F: FnMut() -> Duration,
{
    let mut samples = Vec::with_capacity(FIXED_SAMPLES);
    for round in 0..(FIXED_WARMUPS + FIXED_SAMPLES) {
        let elapsed = f();
        if round >= FIXED_WARMUPS {
            samples.push(elapsed.as_nanos() as f64 / divisor);
        }
    }
    samples
}

fn collect_scalar_samples<F>(mut f: F) -> Vec<f64>
where
    F: FnMut(usize) -> f64,
{
    let mut samples = Vec::with_capacity(FIXED_SAMPLES);
    for round in 0..FIXED_SAMPLES {
        samples.push(f(round));
    }
    samples
}

fn run_fixed_samples() {
    let byte_texts = ascii_texts(TEXT_COUNT, TEXT_LEN);
    let char_texts = unicode_texts(TEXT_COUNT, TEXT_LEN);
    let byte_queries = byte_queries(&byte_texts, QUERY_COUNT, QUERY_LEN);
    let char_queries = char_queries(&char_texts, QUERY_COUNT, QUERY_LEN);

    let native_byte_suffix = build_native_byte_suffix(&byte_texts);
    let legacy_byte_suffix = build_legacy_byte(&byte_texts);
    let native_char_suffix = build_native_char_suffix(&char_texts);
    let legacy_char_suffix = build_legacy_char(&char_texts);
    let native_byte_suffix_tree = build_native_byte_suffix_tree(&byte_texts);
    let native_char_suffix_tree = build_native_char_suffix_tree(&char_texts);
    let native_byte_scdawg = build_native_byte_scdawg(&byte_texts);
    let native_char_scdawg = build_native_char_scdawg(&char_texts);

    let suffix_byte_control = collect_samples(
        || {
            let start = Instant::now();
            lookup_legacy_byte_suffix(&legacy_byte_suffix, &byte_queries);
            start.elapsed()
        },
        QUERY_COUNT as f64,
    );
    let suffix_byte_treatment = collect_samples(
        || {
            let start = Instant::now();
            lookup_native_byte_suffix(&native_byte_suffix, &byte_queries);
            start.elapsed()
        },
        QUERY_COUNT as f64,
    );
    let suffix_char_control = collect_samples(
        || {
            let start = Instant::now();
            lookup_legacy_char_suffix(&legacy_char_suffix, &char_queries);
            start.elapsed()
        },
        QUERY_COUNT as f64,
    );
    let suffix_char_treatment = collect_samples(
        || {
            let start = Instant::now();
            lookup_native_char_suffix(&native_char_suffix, &char_queries);
            start.elapsed()
        },
        QUERY_COUNT as f64,
    );
    let suffix_tree_byte_control = collect_samples(
        || {
            let start = Instant::now();
            lookup_legacy_byte_scdawg(&legacy_byte_suffix, &byte_queries);
            start.elapsed()
        },
        QUERY_COUNT as f64,
    );
    let suffix_tree_byte_treatment = collect_samples(
        || {
            let start = Instant::now();
            lookup_native_byte_suffix_tree(&native_byte_suffix_tree, &byte_queries);
            start.elapsed()
        },
        QUERY_COUNT as f64,
    );
    let suffix_tree_char_control = collect_samples(
        || {
            let start = Instant::now();
            lookup_legacy_char_scdawg(&legacy_char_suffix, &char_queries);
            start.elapsed()
        },
        QUERY_COUNT as f64,
    );
    let suffix_tree_char_treatment = collect_samples(
        || {
            let start = Instant::now();
            lookup_native_char_suffix_tree(&native_char_suffix_tree, &char_queries);
            start.elapsed()
        },
        QUERY_COUNT as f64,
    );
    let scdawg_byte_control = collect_samples(
        || {
            let start = Instant::now();
            lookup_legacy_byte_scdawg(&legacy_byte_suffix, &byte_queries);
            start.elapsed()
        },
        QUERY_COUNT as f64,
    );
    let scdawg_byte_treatment = collect_samples(
        || {
            let start = Instant::now();
            lookup_native_byte_scdawg(&native_byte_scdawg, &byte_queries);
            start.elapsed()
        },
        QUERY_COUNT as f64,
    );
    let scdawg_char_control = collect_samples(
        || {
            let start = Instant::now();
            lookup_legacy_char_scdawg(&legacy_char_suffix, &char_queries);
            start.elapsed()
        },
        QUERY_COUNT as f64,
    );
    let scdawg_char_treatment = collect_samples(
        || {
            let start = Instant::now();
            lookup_native_char_scdawg(&native_char_scdawg, &char_queries);
            start.elapsed()
        },
        QUERY_COUNT as f64,
    );
    let suffix_byte_parallel_control = collect_samples(
        || time_parallel_legacy_byte(&byte_texts, &byte_queries),
        (PARALLEL_READERS * OPS_PER_READER) as f64,
    );
    let suffix_byte_parallel_treatment = collect_samples(
        || time_parallel_native_byte(&byte_texts, &byte_queries),
        (PARALLEL_READERS * OPS_PER_READER) as f64,
    );
    let suffix_char_parallel_control = collect_samples(
        || time_parallel_legacy_char(&char_texts, &char_queries),
        (PARALLEL_READERS * OPS_PER_READER) as f64,
    );
    let suffix_char_parallel_treatment = collect_samples(
        || time_parallel_native_char(&char_texts, &char_queries),
        (PARALLEL_READERS * OPS_PER_READER) as f64,
    );
    let suffix_tree_byte_parallel_control = collect_samples(
        || time_parallel_legacy_byte_locations(&byte_texts, &byte_queries),
        (PARALLEL_READERS * OPS_PER_READER) as f64,
    );
    let suffix_tree_parallel_treatment = collect_samples(
        || time_parallel_native_byte_suffix_tree(&byte_texts, &byte_queries),
        (PARALLEL_READERS * OPS_PER_READER) as f64,
    );
    let suffix_tree_char_parallel_control = collect_samples(
        || time_parallel_legacy_char_locations(&char_texts, &char_queries),
        (PARALLEL_READERS * OPS_PER_READER) as f64,
    );
    let suffix_tree_char_parallel_treatment = collect_samples(
        || time_parallel_native_char_suffix_tree(&char_texts, &char_queries),
        (PARALLEL_READERS * OPS_PER_READER) as f64,
    );
    let scdawg_byte_parallel_control = collect_samples(
        || time_parallel_legacy_byte_locations(&byte_texts, &byte_queries),
        (PARALLEL_READERS * OPS_PER_READER) as f64,
    );
    let scdawg_byte_parallel_treatment = collect_samples(
        || time_parallel_native_byte_scdawg(&byte_texts, &byte_queries),
        (PARALLEL_READERS * OPS_PER_READER) as f64,
    );
    let scdawg_char_parallel_control = collect_samples(
        || time_parallel_legacy_char_locations(&char_texts, &char_queries),
        (PARALLEL_READERS * OPS_PER_READER) as f64,
    );
    let scdawg_char_parallel_treatment = collect_samples(
        || time_parallel_native_char_scdawg(&char_texts, &char_queries),
        (PARALLEL_READERS * OPS_PER_READER) as f64,
    );
    let suffix_byte_disk_control = collect_scalar_samples(|round| {
        let texts =
            ascii_texts_with_seed(DISK_SAMPLE_TEXT_COUNT, TEXT_LEN, 0x5f00_0000 ^ round as u64);
        checkpoint_legacy_byte_bytes(&texts) as f64
    });
    let suffix_byte_disk_treatment = collect_scalar_samples(|round| {
        let texts =
            ascii_texts_with_seed(DISK_SAMPLE_TEXT_COUNT, TEXT_LEN, 0x5f00_0000 ^ round as u64);
        checkpoint_native_byte_bytes(&texts) as f64
    });
    let suffix_char_disk_control = collect_scalar_samples(|round| {
        let texts =
            unicode_texts_with_seed(DISK_SAMPLE_TEXT_COUNT, TEXT_LEN, 0x5f00_c000 ^ round as u64);
        checkpoint_legacy_char_bytes(&texts) as f64
    });
    let suffix_char_disk_treatment = collect_scalar_samples(|round| {
        let texts =
            unicode_texts_with_seed(DISK_SAMPLE_TEXT_COUNT, TEXT_LEN, 0x5f00_c000 ^ round as u64);
        checkpoint_native_char_bytes(&texts) as f64
    });
    let suffix_tree_byte_disk_control = collect_scalar_samples(|round| {
        let texts =
            ascii_texts_with_seed(DISK_SAMPLE_TEXT_COUNT, TEXT_LEN, 0x57ee_0000 ^ round as u64);
        checkpoint_legacy_byte_bytes(&texts) as f64
    });
    let suffix_tree_char_disk_control = collect_scalar_samples(|round| {
        let texts =
            unicode_texts_with_seed(DISK_SAMPLE_TEXT_COUNT, TEXT_LEN, 0x57ee_c000 ^ round as u64);
        checkpoint_legacy_char_bytes(&texts) as f64
    });
    let suffix_tree_byte_disk_treatment = collect_scalar_samples(|round| {
        let texts =
            ascii_texts_with_seed(DISK_SAMPLE_TEXT_COUNT, TEXT_LEN, 0x57ee_0000 ^ round as u64);
        checkpoint_native_byte_suffix_tree_bytes(&texts) as f64
    });
    let suffix_tree_char_disk_treatment = collect_scalar_samples(|round| {
        let texts =
            unicode_texts_with_seed(DISK_SAMPLE_TEXT_COUNT, TEXT_LEN, 0x57ee_c000 ^ round as u64);
        checkpoint_native_char_suffix_tree_bytes(&texts) as f64
    });
    let scdawg_byte_disk_control = collect_scalar_samples(|round| {
        let texts =
            ascii_texts_with_seed(DISK_SAMPLE_TEXT_COUNT, TEXT_LEN, 0x5cda_0000 ^ round as u64);
        checkpoint_legacy_byte_bytes(&texts) as f64
    });
    let scdawg_byte_disk_treatment = collect_scalar_samples(|round| {
        let texts =
            ascii_texts_with_seed(DISK_SAMPLE_TEXT_COUNT, TEXT_LEN, 0x5cda_0000 ^ round as u64);
        checkpoint_native_byte_scdawg_bytes(&texts) as f64
    });
    let scdawg_char_disk_control = collect_scalar_samples(|round| {
        let texts =
            unicode_texts_with_seed(DISK_SAMPLE_TEXT_COUNT, TEXT_LEN, 0x5cda_c000 ^ round as u64);
        checkpoint_legacy_char_bytes(&texts) as f64
    });
    let scdawg_char_disk_treatment = collect_scalar_samples(|round| {
        let texts =
            unicode_texts_with_seed(DISK_SAMPLE_TEXT_COUNT, TEXT_LEN, 0x5cda_c000 ^ round as u64);
        checkpoint_native_char_scdawg_bytes(&texts) as f64
    });

    print_sample_line(
        "suffix_byte_match_positions_ns_per_query",
        "control_encoded_suffix_artrie",
        "ns/query",
        &suffix_byte_control,
    );
    print_sample_line(
        "suffix_byte_match_positions_ns_per_query",
        "treatment_native_suffix_graph",
        "ns/query",
        &suffix_byte_treatment,
    );
    print_sample_line(
        "suffix_char_match_positions_ns_per_query",
        "control_encoded_suffix_artrie_char",
        "ns/query",
        &suffix_char_control,
    );
    print_sample_line(
        "suffix_char_match_positions_ns_per_query",
        "treatment_native_suffix_graph_char",
        "ns/query",
        &suffix_char_treatment,
    );
    print_sample_line(
        "suffix_tree_byte_locations_ns_per_query",
        "control_encoded_suffix_tree_artrie",
        "ns/query",
        &suffix_tree_byte_control,
    );
    print_sample_line(
        "suffix_tree_byte_locations_ns_per_query",
        "treatment_native_suffix_tree_graph",
        "ns/query",
        &suffix_tree_byte_treatment,
    );
    print_sample_line(
        "suffix_tree_char_locations_ns_per_query",
        "control_encoded_suffix_tree_artrie_char",
        "ns/query",
        &suffix_tree_char_control,
    );
    print_sample_line(
        "suffix_tree_char_locations_ns_per_query",
        "treatment_native_suffix_tree_graph_char",
        "ns/query",
        &suffix_tree_char_treatment,
    );
    print_sample_line(
        "scdawg_byte_locations_ns_per_query",
        "control_encoded_scdawg_artrie",
        "ns/query",
        &scdawg_byte_control,
    );
    print_sample_line(
        "scdawg_byte_locations_ns_per_query",
        "treatment_native_scdawg_graph",
        "ns/query",
        &scdawg_byte_treatment,
    );
    print_sample_line(
        "scdawg_char_locations_ns_per_query",
        "control_encoded_scdawg_artrie_char",
        "ns/query",
        &scdawg_char_control,
    );
    print_sample_line(
        "scdawg_char_locations_ns_per_query",
        "treatment_native_scdawg_graph_char",
        "ns/query",
        &scdawg_char_treatment,
    );
    print_sample_line(
        "suffix_byte_parallel_read_write_ns_per_read",
        "control_encoded_suffix_artrie",
        "ns/read",
        &suffix_byte_parallel_control,
    );
    print_sample_line(
        "suffix_byte_parallel_read_write_ns_per_read",
        "treatment_native_suffix_graph",
        "ns/read",
        &suffix_byte_parallel_treatment,
    );
    print_sample_line(
        "suffix_char_parallel_read_write_ns_per_read",
        "control_encoded_suffix_artrie_char",
        "ns/read",
        &suffix_char_parallel_control,
    );
    print_sample_line(
        "suffix_char_parallel_read_write_ns_per_read",
        "treatment_native_suffix_graph_char",
        "ns/read",
        &suffix_char_parallel_treatment,
    );
    print_sample_line(
        "suffix_tree_byte_parallel_read_write_ns_per_read",
        "control_encoded_suffix_tree_artrie",
        "ns/read",
        &suffix_tree_byte_parallel_control,
    );
    print_sample_line(
        "suffix_tree_byte_parallel_read_write_ns_per_read",
        "treatment_native_suffix_tree_graph",
        "ns/read",
        &suffix_tree_parallel_treatment,
    );
    print_sample_line(
        "suffix_tree_char_parallel_read_write_ns_per_read",
        "control_encoded_suffix_tree_artrie_char",
        "ns/read",
        &suffix_tree_char_parallel_control,
    );
    print_sample_line(
        "suffix_tree_char_parallel_read_write_ns_per_read",
        "treatment_native_suffix_tree_graph_char",
        "ns/read",
        &suffix_tree_char_parallel_treatment,
    );
    print_sample_line(
        "scdawg_byte_parallel_read_write_ns_per_read",
        "control_encoded_scdawg_artrie",
        "ns/read",
        &scdawg_byte_parallel_control,
    );
    print_sample_line(
        "scdawg_byte_parallel_read_write_ns_per_read",
        "treatment_native_scdawg_graph",
        "ns/read",
        &scdawg_byte_parallel_treatment,
    );
    print_sample_line(
        "scdawg_char_parallel_read_write_ns_per_read",
        "control_encoded_scdawg_artrie_char",
        "ns/read",
        &scdawg_char_parallel_control,
    );
    print_sample_line(
        "scdawg_char_parallel_read_write_ns_per_read",
        "treatment_native_scdawg_graph_char",
        "ns/read",
        &scdawg_char_parallel_treatment,
    );
    print_sample_line(
        "suffix_byte_checkpoint_disk_bytes",
        "control_encoded_suffix_artrie",
        "bytes",
        &suffix_byte_disk_control,
    );
    print_sample_line(
        "suffix_byte_checkpoint_disk_bytes",
        "treatment_native_suffix_graph",
        "bytes",
        &suffix_byte_disk_treatment,
    );
    print_sample_line(
        "suffix_char_checkpoint_disk_bytes",
        "control_encoded_suffix_artrie_char",
        "bytes",
        &suffix_char_disk_control,
    );
    print_sample_line(
        "suffix_char_checkpoint_disk_bytes",
        "treatment_native_suffix_graph_char",
        "bytes",
        &suffix_char_disk_treatment,
    );
    print_sample_line(
        "suffix_tree_byte_checkpoint_disk_bytes",
        "control_encoded_suffix_tree_artrie",
        "bytes",
        &suffix_tree_byte_disk_control,
    );
    print_sample_line(
        "suffix_tree_byte_checkpoint_disk_bytes",
        "treatment_native_suffix_tree_graph",
        "bytes",
        &suffix_tree_byte_disk_treatment,
    );
    print_sample_line(
        "suffix_tree_char_checkpoint_disk_bytes",
        "control_encoded_suffix_tree_artrie_char",
        "bytes",
        &suffix_tree_char_disk_control,
    );
    print_sample_line(
        "suffix_tree_char_checkpoint_disk_bytes",
        "treatment_native_suffix_tree_graph_char",
        "bytes",
        &suffix_tree_char_disk_treatment,
    );
    print_sample_line(
        "scdawg_byte_checkpoint_disk_bytes",
        "control_encoded_scdawg_artrie",
        "bytes",
        &scdawg_byte_disk_control,
    );
    print_sample_line(
        "scdawg_byte_checkpoint_disk_bytes",
        "treatment_native_scdawg_graph",
        "bytes",
        &scdawg_byte_disk_treatment,
    );
    print_sample_line(
        "scdawg_char_checkpoint_disk_bytes",
        "control_encoded_scdawg_artrie_char",
        "bytes",
        &scdawg_char_disk_control,
    );
    print_sample_line(
        "scdawg_char_checkpoint_disk_bytes",
        "treatment_native_scdawg_graph_char",
        "bytes",
        &scdawg_char_disk_treatment,
    );
}

fn bench_suffix_lookup(c: &mut Criterion) {
    let byte_texts = ascii_texts(TEXT_COUNT, TEXT_LEN);
    let char_texts = unicode_texts(TEXT_COUNT, TEXT_LEN);
    let byte_queries = byte_queries(&byte_texts, QUERY_COUNT, QUERY_LEN);
    let char_queries = char_queries(&char_texts, QUERY_COUNT, QUERY_LEN);
    let native_byte = build_native_byte_suffix(&byte_texts);
    let legacy_byte = build_legacy_byte(&byte_texts);
    let native_char = build_native_char_suffix(&char_texts);
    let legacy_char = build_legacy_char(&char_texts);

    let mut group = c.benchmark_group("persistent_suffix_native_lookup");
    group.sample_size(20);
    group.throughput(Throughput::Elements(QUERY_COUNT as u64));
    group.bench_function(BenchmarkId::new("control_encoded_byte", QUERY_LEN), |b| {
        b.iter(|| lookup_legacy_byte_suffix(&legacy_byte, &byte_queries));
    });
    group.bench_function(BenchmarkId::new("treatment_native_byte", QUERY_LEN), |b| {
        b.iter(|| lookup_native_byte_suffix(&native_byte, &byte_queries));
    });
    group.bench_function(BenchmarkId::new("control_encoded_char", QUERY_LEN), |b| {
        b.iter(|| lookup_legacy_char_suffix(&legacy_char, &char_queries));
    });
    group.bench_function(BenchmarkId::new("treatment_native_char", QUERY_LEN), |b| {
        b.iter(|| lookup_native_char_suffix(&native_char, &char_queries));
    });
    group.finish();
}

fn bench_scdawg_lookup(c: &mut Criterion) {
    let byte_texts = ascii_texts(TEXT_COUNT, TEXT_LEN);
    let char_texts = unicode_texts(TEXT_COUNT, TEXT_LEN);
    let byte_queries = byte_queries(&byte_texts, QUERY_COUNT, QUERY_LEN);
    let char_queries = char_queries(&char_texts, QUERY_COUNT, QUERY_LEN);
    let native_byte = build_native_byte_scdawg(&byte_texts);
    let legacy_byte = build_legacy_byte(&byte_texts);
    let native_char = build_native_char_scdawg(&char_texts);
    let legacy_char = build_legacy_char(&char_texts);

    let mut group = c.benchmark_group("persistent_scdawg_native_locations");
    group.sample_size(20);
    group.throughput(Throughput::Elements(QUERY_COUNT as u64));
    group.bench_function(BenchmarkId::new("control_encoded_byte", QUERY_LEN), |b| {
        b.iter(|| lookup_legacy_byte_scdawg(&legacy_byte, &byte_queries));
    });
    group.bench_function(BenchmarkId::new("treatment_native_byte", QUERY_LEN), |b| {
        b.iter(|| lookup_native_byte_scdawg(&native_byte, &byte_queries));
    });
    group.bench_function(BenchmarkId::new("control_encoded_char", QUERY_LEN), |b| {
        b.iter(|| lookup_legacy_char_scdawg(&legacy_char, &char_queries));
    });
    group.bench_function(BenchmarkId::new("treatment_native_char", QUERY_LEN), |b| {
        b.iter(|| lookup_native_char_scdawg(&native_char, &char_queries));
    });
    group.finish();
}

fn bench_suffix_tree_lookup(c: &mut Criterion) {
    let byte_texts = ascii_texts(TEXT_COUNT, TEXT_LEN);
    let char_texts = unicode_texts(TEXT_COUNT, TEXT_LEN);
    let byte_queries = byte_queries(&byte_texts, QUERY_COUNT, QUERY_LEN);
    let char_queries = char_queries(&char_texts, QUERY_COUNT, QUERY_LEN);
    let native_byte = build_native_byte_suffix_tree(&byte_texts);
    let legacy_byte = build_legacy_byte(&byte_texts);
    let native_char = build_native_char_suffix_tree(&char_texts);
    let legacy_char = build_legacy_char(&char_texts);

    let mut group = c.benchmark_group("persistent_suffix_tree_native_locations");
    group.sample_size(20);
    group.throughput(Throughput::Elements(QUERY_COUNT as u64));
    group.bench_function(BenchmarkId::new("control_encoded_byte", QUERY_LEN), |b| {
        b.iter(|| lookup_legacy_byte_scdawg(&legacy_byte, &byte_queries));
    });
    group.bench_function(BenchmarkId::new("treatment_native_byte", QUERY_LEN), |b| {
        b.iter(|| lookup_native_byte_suffix_tree(&native_byte, &byte_queries));
    });
    group.bench_function(BenchmarkId::new("control_encoded_char", QUERY_LEN), |b| {
        b.iter(|| lookup_legacy_char_scdawg(&legacy_char, &char_queries));
    });
    group.bench_function(BenchmarkId::new("treatment_native_char", QUERY_LEN), |b| {
        b.iter(|| lookup_native_char_suffix_tree(&native_char, &char_queries));
    });
    group.finish();
}

fn bench_parallel(c: &mut Criterion) {
    let byte_texts = ascii_texts(TEXT_COUNT, TEXT_LEN);
    let char_texts = unicode_texts(TEXT_COUNT, TEXT_LEN);
    let byte_queries = byte_queries(&byte_texts, QUERY_COUNT, QUERY_LEN);
    let char_queries = char_queries(&char_texts, QUERY_COUNT, QUERY_LEN);
    let mut group = c.benchmark_group("persistent_suffix_native_parallel_reads_writes");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(4));
    group.throughput(Throughput::Elements(
        (PARALLEL_READERS * OPS_PER_READER) as u64,
    ));
    group.bench_function("suffix_control_encoded_byte", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                total += time_parallel_legacy_byte(&byte_texts, &byte_queries);
            }
            total
        });
    });
    group.bench_function("suffix_treatment_native_byte", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                total += time_parallel_native_byte(&byte_texts, &byte_queries);
            }
            total
        });
    });
    group.bench_function("suffix_control_encoded_char", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                total += time_parallel_legacy_char(&char_texts, &char_queries);
            }
            total
        });
    });
    group.bench_function("suffix_treatment_native_char", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                total += time_parallel_native_char(&char_texts, &char_queries);
            }
            total
        });
    });
    group.bench_function("suffix_tree_control_encoded_byte", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                total += time_parallel_legacy_byte_locations(&byte_texts, &byte_queries);
            }
            total
        });
    });
    group.bench_function("suffix_tree_treatment_native_byte", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                total += time_parallel_native_byte_suffix_tree(&byte_texts, &byte_queries);
            }
            total
        });
    });
    group.bench_function("suffix_tree_control_encoded_char", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                total += time_parallel_legacy_char_locations(&char_texts, &char_queries);
            }
            total
        });
    });
    group.bench_function("suffix_tree_treatment_native_char", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                total += time_parallel_native_char_suffix_tree(&char_texts, &char_queries);
            }
            total
        });
    });
    group.bench_function("scdawg_control_encoded_byte", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                total += time_parallel_legacy_byte_locations(&byte_texts, &byte_queries);
            }
            total
        });
    });
    group.bench_function("scdawg_treatment_native_byte", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                total += time_parallel_native_byte_scdawg(&byte_texts, &byte_queries);
            }
            total
        });
    });
    group.bench_function("scdawg_control_encoded_char", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                total += time_parallel_legacy_char_locations(&char_texts, &char_queries);
            }
            total
        });
    });
    group.bench_function("scdawg_treatment_native_char", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                total += time_parallel_native_char_scdawg(&char_texts, &char_queries);
            }
            total
        });
    });
    group.finish();
}

fn bench_checkpoint_bytes(c: &mut Criterion) {
    let byte_texts = ascii_texts(128, TEXT_LEN);
    let char_texts = unicode_texts(128, TEXT_LEN);
    let legacy_byte = checkpoint_legacy_byte_bytes(&byte_texts);
    let native_byte = checkpoint_native_byte_bytes(&byte_texts);
    let native_suffix_tree_byte = checkpoint_native_byte_suffix_tree_bytes(&byte_texts);
    let native_scdawg_byte = checkpoint_native_byte_scdawg_bytes(&byte_texts);
    let legacy_char = checkpoint_legacy_char_bytes(&char_texts);
    let native_char = checkpoint_native_char_bytes(&char_texts);
    let native_suffix_tree_char = checkpoint_native_char_suffix_tree_bytes(&char_texts);
    let native_scdawg_char = checkpoint_native_char_scdawg_bytes(&char_texts);

    let mut group = c.benchmark_group("persistent_suffix_native_checkpoint_bytes");
    group.sample_size(10);
    group.bench_function("control_encoded_byte_bytes", |b| {
        b.iter(|| black_box(legacy_byte))
    });
    group.bench_function("treatment_native_byte_bytes", |b| {
        b.iter(|| black_box(native_byte))
    });
    group.bench_function("treatment_native_suffix_tree_byte_bytes", |b| {
        b.iter(|| black_box(native_suffix_tree_byte))
    });
    group.bench_function("treatment_native_scdawg_byte_bytes", |b| {
        b.iter(|| black_box(native_scdawg_byte))
    });
    group.bench_function("control_encoded_char_bytes", |b| {
        b.iter(|| black_box(legacy_char))
    });
    group.bench_function("treatment_native_char_bytes", |b| {
        b.iter(|| black_box(native_char))
    });
    group.bench_function("treatment_native_suffix_tree_char_bytes", |b| {
        b.iter(|| black_box(native_suffix_tree_char))
    });
    group.bench_function("treatment_native_scdawg_char_bytes", |b| {
        b.iter(|| black_box(native_scdawg_char))
    });
    group.finish();

    eprintln!(
        "persistent_suffix_native_checkpoint_bytes,legacy_byte={},native_byte={},native_suffix_tree_byte={},native_scdawg_byte={},legacy_char={},native_char={},native_suffix_tree_char={},native_scdawg_char={}",
        legacy_byte,
        native_byte,
        native_suffix_tree_byte,
        native_scdawg_byte,
        legacy_char,
        native_char,
        native_suffix_tree_char,
        native_scdawg_char
    );
}

fn run_criterion() {
    let mut criterion = Criterion::default().configure_from_args();
    bench_suffix_lookup(&mut criterion);
    bench_suffix_tree_lookup(&mut criterion);
    bench_scdawg_lookup(&mut criterion);
    bench_parallel(&mut criterion);
    bench_checkpoint_bytes(&mut criterion);
    criterion.final_summary();
}

fn main() {
    if std::env::var_os("PERSISTENT_SUFFIX_FIXED_SAMPLES").is_some() {
        run_fixed_samples();
    } else {
        run_criterion();
    }
}
