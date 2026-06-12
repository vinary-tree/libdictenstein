//! Property-based traces for persistent suffix automata.
//!
//! The oracle models the public contract:
//! - active source texts contribute substring membership and match positions;
//! - explicit mapped values contribute exact values and prefix membership;
//! - removal only deactivates sources; compaction preserves the visible language
//!   and explicit values.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::{PersistentSuffixAutomaton, PersistentSuffixAutomatonChar};
use libdictenstein::{Dictionary, MappedDictionary};
use proptest::prelude::*;
use std::collections::{BTreeMap, BTreeSet};
use tempfile::TempDir;

#[derive(Clone, Debug)]
enum Op {
    Insert(String),
    InsertValue(String, i32),
    Remove(String),
    UpdateOrInsert(String, i32),
    Compact,
    Clear,
    CheckpointReopen,
}

#[derive(Clone, Debug)]
struct Source {
    text: String,
    active: bool,
}

#[derive(Clone, Debug, Default)]
struct Oracle {
    sources: Vec<Source>,
    values: BTreeMap<String, i32>,
}

impl Oracle {
    fn insert(&mut self, text: String) -> bool {
        self.sources.push(Source { text, active: true });
        true
    }

    fn insert_value(&mut self, text: String, value: i32) -> bool {
        self.sources.push(Source {
            text: text.clone(),
            active: true,
        });
        self.values.insert(text, value);
        true
    }

    fn remove(&mut self, text: &str) -> bool {
        for source in &mut self.sources {
            if source.active && source.text == text {
                source.active = false;
                return true;
            }
        }
        false
    }

    fn update_or_insert(&mut self, term: String, default_value: i32) -> bool {
        if let Some(value) = self.values.get_mut(&term) {
            *value += 1;
            false
        } else if self.contains(&term) {
            self.values.insert(term, default_value);
            true
        } else {
            self.insert_value(term, default_value)
        }
    }

    fn clear(&mut self) {
        self.sources.clear();
        self.values.clear();
    }

    fn string_count(&self) -> usize {
        self.sources.iter().filter(|source| source.active).count()
    }

    fn source_texts(&self) -> Vec<String> {
        self.sources
            .iter()
            .map(|source| source.text.clone())
            .collect()
    }

    fn contains(&self, term: &str) -> bool {
        term.is_empty()
            || self
                .sources
                .iter()
                .any(|source| source.active && source.text.contains(term))
            || self.values.keys().any(|key| key.starts_with(term))
    }

    fn get_value(&self, term: &str) -> Option<i32> {
        self.values.get(term).copied()
    }

    fn byte_match_positions(&self, term: &str) -> Vec<(usize, usize)> {
        if term.is_empty() {
            return Vec::new();
        }
        let needle = term.as_bytes();
        let mut positions = Vec::new();
        for (source_id, source) in self.sources.iter().enumerate() {
            if !source.active || needle.len() > source.text.len() {
                continue;
            }
            for start in 0..=source.text.len() - needle.len() {
                if source.text.as_bytes()[start..].starts_with(needle) {
                    positions.push((source_id, start + needle.len()));
                }
            }
        }
        positions.sort_unstable();
        positions.dedup();
        positions
    }

    fn char_match_positions(&self, term: &str) -> Vec<(usize, usize)> {
        if term.is_empty() {
            return Vec::new();
        }
        let mut positions = Vec::new();
        for (source_id, source) in self.sources.iter().enumerate() {
            if !source.active {
                continue;
            }
            for (start, _) in source.text.char_indices() {
                if source.text[start..].starts_with(term) {
                    positions.push((source_id, start + term.len()));
                }
            }
        }
        positions.sort_unstable();
        positions.dedup();
        positions
    }

    fn byte_probes(&self, extra: &str) -> BTreeSet<String> {
        let mut probes = self.common_probes(extra);
        for source in &self.sources {
            let bytes = source.text.as_bytes();
            for start in 0..bytes.len() {
                for end in start + 1..=bytes.len() {
                    probes.insert(String::from_utf8_lossy(&bytes[start..end]).to_string());
                }
            }
        }
        probes
    }

    fn char_probes(&self, extra: &str) -> BTreeSet<String> {
        let mut probes = self.common_probes(extra);
        for source in &self.sources {
            let starts: Vec<_> = source
                .text
                .char_indices()
                .map(|(idx, _)| idx)
                .chain(std::iter::once(source.text.len()))
                .collect();
            for left in 0..starts.len().saturating_sub(1) {
                for right in left + 1..starts.len() {
                    probes.insert(source.text[starts[left]..starts[right]].to_string());
                }
            }
        }
        probes
    }

    fn common_probes(&self, extra: &str) -> BTreeSet<String> {
        let mut probes = BTreeSet::from(["".to_string(), extra.to_string()]);
        for source in &self.sources {
            probes.insert(source.text.clone());
        }
        for key in self.values.keys() {
            probes.insert(key.clone());
            let mut prefix = String::new();
            for ch in key.chars() {
                prefix.push(ch);
                probes.insert(prefix.clone());
            }
        }
        probes
    }
}

fn scratch_dir(prefix: &str) -> TempDir {
    std::fs::create_dir_all("target/test-tmp").ok();
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in("target/test-tmp")
        .expect("scratch tempdir under target/test-tmp")
}

fn byte_text_strategy() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![
            Just('a'),
            Just('b'),
            Just('c'),
            Just('\0'),
            Just('\u{1}'),
            Just('\u{2}'),
        ],
        0..=8,
    )
    .prop_map(|chars| chars.into_iter().collect())
}

fn char_text_strategy() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![
            Just('a'),
            Just('b'),
            Just('é'),
            Just('日'),
            Just('🙂'),
            Just('\u{E000}'),
            Just('\u{E001}'),
            Just('\u{E002}'),
        ],
        0..=8,
    )
    .prop_map(|chars| chars.into_iter().collect())
}

fn byte_op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => byte_text_strategy().prop_map(Op::Insert),
        3 => (byte_text_strategy(), -32i32..=32).prop_map(|(text, value)| Op::InsertValue(text, value)),
        3 => byte_text_strategy().prop_map(Op::Remove),
        3 => (byte_text_strategy(), -32i32..=32).prop_map(|(text, value)| Op::UpdateOrInsert(text, value)),
        1 => Just(Op::Compact),
        1 => Just(Op::Clear),
        1 => Just(Op::CheckpointReopen),
    ]
}

fn char_op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => char_text_strategy().prop_map(Op::Insert),
        3 => (char_text_strategy(), -32i32..=32).prop_map(|(text, value)| Op::InsertValue(text, value)),
        3 => char_text_strategy().prop_map(Op::Remove),
        3 => (char_text_strategy(), -32i32..=32).prop_map(|(text, value)| Op::UpdateOrInsert(text, value)),
        1 => Just(Op::Compact),
        1 => Just(Op::Clear),
        1 => Just(Op::CheckpointReopen),
    ]
}

fn assert_byte_matches_oracle(dict: &PersistentSuffixAutomaton<i32>, oracle: &Oracle, extra: &str) {
    assert_eq!(dict.string_count(), oracle.string_count());
    assert_eq!(dict.source_texts(), oracle.source_texts());
    for probe in oracle.byte_probes(extra) {
        assert_eq!(
            dict.contains(&probe),
            oracle.contains(&probe),
            "byte contains mismatch for {probe:?}"
        );
        assert_eq!(
            dict.get_value(&probe),
            oracle.get_value(&probe),
            "byte value mismatch for {probe:?}"
        );
        assert_eq!(
            dict.match_positions(&probe),
            oracle.byte_match_positions(&probe),
            "byte match positions mismatch for {probe:?}"
        );
    }
}

fn assert_char_matches_oracle(
    dict: &PersistentSuffixAutomatonChar<i32>,
    oracle: &Oracle,
    extra: &str,
) {
    assert_eq!(dict.string_count(), oracle.string_count());
    assert_eq!(dict.source_texts(), oracle.source_texts());
    for probe in oracle.char_probes(extra) {
        assert_eq!(
            dict.contains(&probe),
            oracle.contains(&probe),
            "char contains mismatch for {probe:?}"
        );
        assert_eq!(
            dict.get_value(&probe),
            oracle.get_value(&probe),
            "char value mismatch for {probe:?}"
        );
        assert_eq!(
            dict.match_positions(&probe),
            oracle.char_match_positions(&probe),
            "char match positions mismatch for {probe:?}"
        );
    }
}

fn run_byte_trace(ops: Vec<Op>) {
    let dir = scratch_dir("persistent-suffix-byte-proptest");
    let path = dir.path().join("byte_suffix.art");
    let mut dict = PersistentSuffixAutomaton::<i32>::create(&path).expect("create byte suffix");
    let mut oracle = Oracle::default();

    for op in ops {
        let probe = match &op {
            Op::Insert(text)
            | Op::InsertValue(text, _)
            | Op::Remove(text)
            | Op::UpdateOrInsert(text, _) => text.clone(),
            Op::Compact | Op::Clear | Op::CheckpointReopen => String::new(),
        };
        match op {
            Op::Insert(text) => assert_eq!(dict.insert(&text), oracle.insert(text)),
            Op::InsertValue(text, value) => {
                assert_eq!(
                    dict.insert_with_value(&text, value),
                    oracle.insert_value(text, value)
                );
            }
            Op::Remove(text) => assert_eq!(dict.remove(&text), oracle.remove(&text)),
            Op::UpdateOrInsert(text, value) => {
                let expected = oracle.update_or_insert(text.clone(), value);
                let actual = dict.update_or_insert(&text, value, |existing| *existing += 1);
                assert_eq!(actual, expected);
            }
            Op::Compact => dict.compact(),
            Op::Clear => {
                dict.clear();
                oracle.clear();
            }
            Op::CheckpointReopen => {
                dict.checkpoint().expect("checkpoint byte suffix");
                dict.close();
                dict = PersistentSuffixAutomaton::<i32>::open(&path).expect("reopen byte suffix");
            }
        }
        assert_byte_matches_oracle(&dict, &oracle, &probe);
    }
}

fn run_char_trace(ops: Vec<Op>) {
    let dir = scratch_dir("persistent-suffix-char-proptest");
    let path = dir.path().join("char_suffix.art");
    let mut dict = PersistentSuffixAutomatonChar::<i32>::create(&path).expect("create char suffix");
    let mut oracle = Oracle::default();

    for op in ops {
        let probe = match &op {
            Op::Insert(text)
            | Op::InsertValue(text, _)
            | Op::Remove(text)
            | Op::UpdateOrInsert(text, _) => text.clone(),
            Op::Compact | Op::Clear | Op::CheckpointReopen => String::new(),
        };
        match op {
            Op::Insert(text) => assert_eq!(dict.insert(&text), oracle.insert(text)),
            Op::InsertValue(text, value) => {
                assert_eq!(
                    dict.insert_with_value(&text, value),
                    oracle.insert_value(text, value)
                );
            }
            Op::Remove(text) => assert_eq!(dict.remove(&text), oracle.remove(&text)),
            Op::UpdateOrInsert(text, value) => {
                let expected = oracle.update_or_insert(text.clone(), value);
                let actual = dict.update_or_insert(&text, value, |existing| *existing += 1);
                assert_eq!(actual, expected);
            }
            Op::Compact => dict.compact(),
            Op::Clear => {
                dict.clear();
                oracle.clear();
            }
            Op::CheckpointReopen => {
                dict.checkpoint().expect("checkpoint char suffix");
                dict.close();
                dict =
                    PersistentSuffixAutomatonChar::<i32>::open(&path).expect("reopen char suffix");
            }
        }
        assert_char_matches_oracle(&dict, &oracle, &probe);
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        max_shrink_iters: 2048,
        .. ProptestConfig::default()
    })]

    #[test]
    fn byte_operation_trace_matches_reference_model(
        ops in prop::collection::vec(byte_op_strategy(), 1..=64)
    ) {
        run_byte_trace(ops);
    }

    #[test]
    fn char_operation_trace_matches_reference_model(
        ops in prop::collection::vec(char_op_strategy(), 1..=64)
    ) {
        run_char_trace(ops);
    }
}
