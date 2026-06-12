//! Correspondence checks for persistent suffix automata.
//!
//! These tests pin the public behavior expected from the ARTrie-backed
//! persistent variants: they recognize the same substring language as the
//! volatile suffix automata, preserve mapped values, filter inactive source
//! records immediately, and survive disk reopen.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::{PersistentSuffixAutomaton, PersistentSuffixAutomatonChar};
use libdictenstein::suffix_automaton::{SuffixAutomaton, SuffixAutomatonChar};
use libdictenstein::{Dictionary, MappedDictionary, MutableMappedDictionary, SyncStrategy};
use serde::{Deserialize, Serialize};
use std::fs;
use tempfile::tempdir;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Metadata {
    tag: String,
    score: i32,
}

impl libdictenstein::value::DictionaryValue for Metadata {}

fn assert_byte_contains_matches(texts: &[&str], probes: &[&str]) {
    let volatile = SuffixAutomaton::<()>::from_texts(texts);
    let persistent = PersistentSuffixAutomaton::<()>::from_texts(texts);

    assert_eq!(persistent.len(), volatile.len());
    assert_eq!(persistent.sync_strategy(), SyncStrategy::InternalSync);
    assert!(persistent.is_suffix_based());

    for probe in probes {
        assert_eq!(
            persistent.contains(probe),
            volatile.contains(probe),
            "byte contains mismatch for {probe:?}"
        );
    }
}

fn assert_char_contains_matches(texts: &[&str], probes: &[&str]) {
    let volatile = SuffixAutomatonChar::<()>::from_texts(texts);
    let persistent = PersistentSuffixAutomatonChar::<()>::from_texts(texts);

    assert_eq!(persistent.len(), volatile.len());
    assert_eq!(persistent.sync_strategy(), SyncStrategy::InternalSync);
    assert!(persistent.is_suffix_based());

    for probe in probes {
        assert_eq!(
            persistent.contains(probe),
            volatile.contains(probe),
            "char contains mismatch for {probe:?}"
        );
    }
}

#[test]
fn byte_variant_matches_volatile_substring_language() {
    assert_byte_contains_matches(
        &["banana", "bandana"],
        &[
            "", "banana", "bandana", "ana", "nan", "dana", "band", "nab", "apple",
        ],
    );
}

#[test]
fn char_variant_matches_volatile_unicode_substring_language() {
    assert_char_contains_matches(
        &["café 日本", "naïve café"],
        &[
            "", "café", "fé 日", "日本", "naïve", "ïve c", "本n", "missing",
        ],
    );
}

#[test]
fn byte_match_positions_filter_removed_sources_and_compaction_rebuilds() {
    let dict = PersistentSuffixAutomaton::<()>::from_texts(["banana", "bandana"]);

    assert_eq!(dict.match_positions("ana"), vec![(0, 4), (0, 6), (1, 7)]);
    assert_eq!(dict.string_count(), 2);

    assert!(dict.remove("banana"));
    assert_eq!(dict.string_count(), 1);
    assert!(dict.needs_compaction());
    assert_eq!(dict.match_positions("ana"), vec![(1, 7)]);

    dict.compact();
    assert!(!dict.needs_compaction());
    assert_eq!(dict.string_count(), 1);
    assert!(!dict.contains("nan"));
    assert!(dict.contains("dana"));
}

#[test]
fn byte_duplicate_sources_are_removed_one_at_a_time() {
    let dict = PersistentSuffixAutomaton::<()>::from_texts(["aba", "aba", "ababa"]);

    assert_eq!(
        dict.match_positions("aba"),
        vec![(0, 3), (1, 3), (2, 3), (2, 5)]
    );
    assert_eq!(dict.string_count(), 3);

    assert!(dict.remove("aba"));
    assert_eq!(dict.string_count(), 2);
    assert_eq!(dict.match_positions("aba"), vec![(1, 3), (2, 3), (2, 5)]);

    assert!(dict.remove("aba"));
    assert_eq!(dict.string_count(), 1);
    assert_eq!(dict.match_positions("aba"), vec![(2, 3), (2, 5)]);
    assert!(!dict.remove("aba"));

    dict.compact();
    assert_eq!(dict.match_positions("aba"), vec![(2, 3), (2, 5)]);
    assert!(dict.contains("bab"));
}

#[test]
fn byte_empty_and_namespace_sentinel_strings_are_data_not_metadata() {
    let dict = PersistentSuffixAutomaton::<i32>::new();
    let sentinel_text = "\0a\u{1}b\u{2}";

    assert!(dict.insert_with_value("", 1));
    assert_eq!(dict.string_count(), 1);
    assert!(dict.contains(""));
    assert_eq!(dict.get_value(""), Some(1));

    assert!(dict.insert_with_value(sentinel_text, 2));
    assert!(dict.contains("\0a"));
    assert!(dict.contains("a\u{1}b"));
    assert!(dict.contains("\u{2}"));
    assert_eq!(dict.get_value(sentinel_text), Some(2));

    assert!(dict.update_or_insert("\0a", 7, |value| *value += 100));
    assert_eq!(dict.get_value("\0a"), Some(7));
    assert!(dict.remove(sentinel_text));
    dict.compact();
    assert_eq!(dict.get_value(""), Some(1));
    assert_eq!(dict.get_value("\0a"), Some(7));
    assert!(dict.contains("\0"));

    dict.clear();
    dict.clear();
    assert_eq!(dict.string_count(), 0);
    assert!(!dict.contains("\0a"));
    assert_eq!(dict.get_value(""), None);
}

#[test]
fn char_private_use_sentinels_and_byte_offsets_are_preserved() {
    let dict = PersistentSuffixAutomatonChar::<i32>::new();
    let sentinel_text = "\u{E000}a\u{E001}β\u{E002}";
    let unicode_text = "aé日a";

    assert!(dict.insert_with_value(sentinel_text, 11));
    assert!(dict.insert(unicode_text));
    assert!(dict.contains("\u{E000}a"));
    assert!(dict.contains("a\u{E001}β"));
    assert_eq!(dict.get_value(sentinel_text), Some(11));

    assert_eq!(dict.match_positions("é日"), vec![(1, 6)]);
    assert_eq!(dict.match_positions("日"), vec![(1, 6)]);
    assert_eq!(dict.match_positions("a"), vec![(0, 4), (1, 1), (1, 7)]);
}

#[test]
fn mapped_value_api_matches_suffix_automaton_contract() {
    let dict = PersistentSuffixAutomaton::<i32>::from_text("banana");

    assert_eq!(dict.get_value("banana"), None);
    assert!(dict.update_or_insert("banana", 7, |value| *value += 1));
    assert_eq!(dict.get_value("banana"), Some(7));

    assert!(!dict.update_or_insert("banana", 7, |value| *value += 1));
    assert_eq!(dict.get_value("banana"), Some(8));

    assert_eq!(dict.get_value("nan"), None);
    assert!(dict.update_or_insert("nan", 3, |value| *value += 1));
    assert_eq!(dict.get_value("nan"), Some(3));

    let other = PersistentSuffixAutomaton::<i32>::new();
    assert!(other.insert_with_value("banana", 10));
    assert!(other.insert_with_value("bandana", 20));

    assert_eq!(dict.union_with(&other, |left, right| left + right), 2);
    assert_eq!(dict.get_value("banana"), Some(18));
    assert_eq!(dict.get_value("bandana"), Some(20));
}

#[test]
fn mapped_values_survive_compaction_and_reopen() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("persistent_suffix_values.artrie");

    {
        let dict =
            PersistentSuffixAutomaton::<Metadata>::create(&path).expect("create byte suffix");
        assert!(dict.insert_with_value(
            "active",
            Metadata {
                tag: "source".to_string(),
                score: 1,
            },
        ));
        assert!(dict.insert("remove-me"));
        assert!(dict.update_or_insert(
            "ct",
            Metadata {
                tag: "substring".to_string(),
                score: 10,
            },
            |value| value.score += 1,
        ));
        assert!(!dict.update_or_insert("ct", Metadata::default(), |value| value.score += 5,));
        assert!(dict.remove("remove-me"));
        dict.compact();
        assert_eq!(
            dict.get_value("active"),
            Some(Metadata {
                tag: "source".to_string(),
                score: 1,
            })
        );
        assert_eq!(
            dict.get_value("ct"),
            Some(Metadata {
                tag: "substring".to_string(),
                score: 15,
            })
        );
        assert!(!dict.contains("remove"));
        dict.checkpoint().expect("checkpoint values");
        dict.close();
    }

    let reopened = PersistentSuffixAutomaton::<Metadata>::open(&path).expect("reopen values");
    assert_eq!(reopened.string_count(), 1);
    assert_eq!(
        reopened.get_value("ct"),
        Some(Metadata {
            tag: "substring".to_string(),
            score: 15,
        })
    );
    assert!(reopened.contains("c"));
    assert!(!reopened.contains("remove"));
}

#[test]
fn byte_disk_backed_suffix_state_survives_reopen() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("persistent_suffix_byte.artrie");

    {
        let dict = PersistentSuffixAutomaton::<i32>::create(&path).expect("create byte suffix");
        assert!(dict.insert_with_value("abracadabra", 11));
        assert!(dict.insert("cad"));
        assert!(dict.contains("racad"));
        assert_eq!(dict.match_positions("abra"), vec![(0, 4), (0, 11)]);
        dict.checkpoint().expect("checkpoint byte suffix");
        dict.close();
    }

    let reopened = PersistentSuffixAutomaton::<i32>::open(&path).expect("open byte suffix");
    assert_eq!(reopened.string_count(), 2);
    assert_eq!(
        reopened.source_texts(),
        vec!["abracadabra".to_string(), "cad".to_string()]
    );
    assert!(reopened.contains("racad"));
    assert!(reopened.contains("cad"));
    assert_eq!(reopened.get_value("abracadabra"), Some(11));
    assert_eq!(reopened.get_value("cad"), None);
    assert_eq!(reopened.match_positions("abra"), vec![(0, 4), (0, 11)]);
}

#[test]
fn char_disk_backed_suffix_state_survives_reopen_and_compaction() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("persistent_suffix_char.artrie");

    {
        let dict = PersistentSuffixAutomatonChar::<i32>::create(&path).expect("create char suffix");
        assert!(dict.insert_with_value("naïve café", 31));
        assert!(dict.insert_with_value("東京カフェ", 41));
        assert!(dict.contains("ïve c"));
        assert!(dict.contains("カフェ"));
        assert_eq!(dict.get_value("東京カフェ"), Some(41));
        dict.checkpoint().expect("checkpoint char suffix");
        dict.close();
    }

    {
        let reopened = PersistentSuffixAutomatonChar::<i32>::open(&path).expect("open char suffix");
        assert_eq!(reopened.string_count(), 2);
        assert!(reopened.contains("ïve c"));
        assert!(reopened.contains("カフェ"));
        assert_eq!(reopened.get_value("naïve café"), Some(31));
        assert!(reopened.remove("naïve café"));
        assert_eq!(
            reopened.match_positions("café"),
            Vec::<(usize, usize)>::new()
        );
        reopened.compact();
        assert!(!reopened.contains("ïve c"));
        assert!(reopened.contains("東京"));
        reopened
            .checkpoint()
            .expect("checkpoint compacted char suffix");
        reopened.close();
    }

    let reopened =
        PersistentSuffixAutomatonChar::<i32>::open(&path).expect("reopen compacted char suffix");
    assert_eq!(reopened.string_count(), 1);
    assert!(!reopened.needs_compaction());
    assert!(!reopened.contains("ïve c"));
    assert_eq!(reopened.get_value("東京カフェ"), Some(41));
}

fn suffix_snapshot_version(path: &std::path::Path) -> u32 {
    let bytes = fs::read(path).expect("read suffix snapshot");
    assert!(
        bytes.len() >= 12,
        "suffix snapshot must contain magic and version"
    );
    u32::from_le_bytes(bytes[8..12].try_into().expect("snapshot version"))
}

#[test]
fn byte_checkpoint_uses_compact_v2_snapshot_records() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("persistent_suffix_byte_compact.psuf");
    let texts: Vec<String> = (0..48)
        .map(|idx| format!("shared-prefix-{idx:02}-abcdefghijklmnopqrstuvwxyz-{idx:02}"))
        .collect();

    {
        let dict = PersistentSuffixAutomaton::<u64>::create(&path).expect("create byte suffix");
        for (idx, text) in texts.iter().enumerate() {
            assert!(dict.insert_with_value(text, idx as u64));
        }
        assert!(dict.update_or_insert("shared-prefix", 9000, |value| *value += 1));
        dict.checkpoint().expect("checkpoint compact byte suffix");
    }

    assert_eq!(suffix_snapshot_version(&path), 2);
    let bytes = fs::metadata(&path).expect("snapshot metadata").len();
    assert!(
        bytes < 12_000,
        "compact suffix snapshot should store records, not the rebuilt graph; got {bytes} bytes"
    );

    let reopened = PersistentSuffixAutomaton::<u64>::open(&path).expect("open compact byte suffix");
    assert_eq!(reopened.string_count(), texts.len());
    assert!(reopened.contains("abcdefghijklmnopqrstuvwxyz"));
    assert_eq!(reopened.get_value(&texts[7]), Some(7));
    assert_eq!(reopened.get_value("shared-prefix"), Some(9000));
}

#[test]
fn char_checkpoint_uses_compact_v2_snapshot_records() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("persistent_suffix_char_compact.psufc");
    let texts: Vec<String> = (0..32)
        .map(|idx| format!("語彙-{idx:02}-café-naïve-日本語-{idx:02}"))
        .collect();

    {
        let dict = PersistentSuffixAutomatonChar::<u64>::create(&path).expect("create char suffix");
        for (idx, text) in texts.iter().enumerate() {
            assert!(dict.insert_with_value(text, (idx * 10) as u64));
        }
        assert!(dict.update_or_insert("日本語", 700, |value| *value += 1));
        dict.checkpoint().expect("checkpoint compact char suffix");
    }

    assert_eq!(suffix_snapshot_version(&path), 2);
    let bytes = fs::metadata(&path).expect("snapshot metadata").len();
    assert!(
        bytes < 16_000,
        "compact char suffix snapshot should store records, not the rebuilt graph; got {bytes} bytes"
    );

    let reopened =
        PersistentSuffixAutomatonChar::<u64>::open(&path).expect("open compact char suffix");
    assert_eq!(reopened.string_count(), texts.len());
    assert!(reopened.contains("café"));
    assert!(reopened.contains("日本語"));
    assert_eq!(reopened.get_value(&texts[3]), Some(30));
    assert_eq!(reopened.get_value("日本語"), Some(700));
}
