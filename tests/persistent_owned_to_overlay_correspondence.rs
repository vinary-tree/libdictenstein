//! **F7-S4 — Owned→Overlay conversion correspondence.** The converted-reopen
//! (production `open`, which converts an Owned-regime eligible file INTO the overlay) must
//! observe EXACTLY the same term-set + values as the pre-F7 owned-reopen ORACLE
//! (`open_with_legacy_loader`, which keeps the legacy owned-loader stay-owned path on an
//! Owned file and reads via the OWNED tree). This pins the converter's recovered state to
//! the proven owned reopen, including the empty term `""`, term-only ("unranked" Owned)
//! members, and counters (incl. char `u64` counts above `i64::MAX`).
//!
//! Because the converting `open` MUTATES the file (rotate the Owned tail → archive + stamp
//! Overlay), the oracle and the converted reopen run on SEPARATE byte-identical copies of
//! the same fixture (built twice from the same data).
//!
//! Real-disk scratch under `target/test-tmp/` (NEVER tmpfs).

#![cfg(feature = "persistent-artrie")]

use std::collections::BTreeMap;
use std::path::Path;

use libdictenstein::persistent_artrie::PersistentARTrie;
use libdictenstein::persistent_artrie_char::PersistentARTrieChar;
use libdictenstein::persistent_artrie_core::wal::{RankRegime, WalReader};
use libdictenstein::value::DictionaryValue;

use serde::{Deserialize, Serialize};

fn scratch(tag: &str) -> tempfile::TempDir {
    std::fs::create_dir_all("target/test-tmp").ok();
    tempfile::Builder::new()
        .prefix(&format!("f7-convert-corr-{tag}-"))
        .tempdir_in("target/test-tmp")
        .expect("real-disk scratch under target/test-tmp")
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Small {
    a: u32,
    b: String,
}
impl DictionaryValue for Small {}

// ============================================================================
// Byte fixture build + snapshot (the OWNED fixture: kill-switch + owned writes incl. "",
// term-only members, valued terms, a checkpoint image, and a post-checkpoint tail).
// ============================================================================

fn byte_build_owned<V>(path: &Path, pre: &[(Vec<u8>, Option<V>)], tail: &[(Vec<u8>, Option<V>)])
where
    V: DictionaryValue + Clone,
{
    let mut trie = PersistentARTrie::<V>::create(path).expect("byte create");
    trie.kill_switch_to_owned();
    let write = |trie: &mut PersistentARTrie<V>, term: &[u8], value: &Option<V>| match value {
        Some(v) => {
            trie.upsert_bytes(term, v.clone())
                .expect("byte owned upsert");
        }
        None => {
            assert_eq!(
                trie.insert_batch_bytes(&[(term, None)]),
                1,
                "byte owned membership"
            );
        }
    };
    for (t, v) in pre {
        write(&mut trie, t, v);
    }
    trie.checkpoint().expect("byte checkpoint");
    for (t, v) in tail {
        write(&mut trie, t, v);
    }
    trie.sync().expect("byte sync");
}

/// Normalize a `get_value` encoding for the cross-loader comparison. For `V = ()` the value
/// is vacuous: `bincode(()) == []` (0 bytes), and the OWNED dense image drops the degenerate
/// `()` value (so `get_value` returns `None` for image terms) while the OVERLAY synthesizes
/// `Some(())`. Both mean "present, no meaningful value", so collapse `Some([])` (an
/// empty-bytes value = `()`) to the membership marker `None`. Every NON-`()` value bincodes
/// to >= 1 byte (e.g. `u64` = 8, `String` = 8+), so this normalization touches ONLY `()`
/// and never hides a real value difference.
fn normalize_value(enc: Option<Vec<u8>>) -> Option<Vec<u8>> {
    match enc {
        Some(bytes) if bytes.is_empty() => None,
        other => other,
    }
}

fn byte_snapshot<V>(trie: &PersistentARTrie<V>) -> BTreeMap<Vec<u8>, Option<Vec<u8>>>
where
    V: DictionaryValue + Serialize,
{
    let mut out = BTreeMap::new();
    let terms = trie
        .iter_prefix(b"")
        .map(|it| it.collect::<Vec<_>>())
        .unwrap_or_default();
    for term in terms {
        let v = trie.get_value_bytes(&term);
        let enc =
            v.map(|v| libdictenstein::serialization::bincode_compat::serialize(&v).expect("enc"));
        out.insert(term, normalize_value(enc));
    }
    out
}

fn char_build_owned<V>(path: &Path, pre: &[(String, Option<V>)], tail: &[(String, Option<V>)])
where
    V: DictionaryValue + Clone + Serialize,
{
    let trie = PersistentARTrieChar::<V>::create(path).expect("char create");
    trie.kill_switch_to_owned();
    for (t, v) in pre {
        match v {
            Some(v) => {
                trie.insert_with_value(t, v.clone())
                    .expect("char owned insert_with_value");
            }
            None => {
                trie.insert(t).expect("char owned insert");
            }
        }
    }
    trie.checkpoint().expect("char checkpoint");
    for (t, v) in tail {
        match v {
            Some(v) => {
                trie.insert_with_value(t, v.clone())
                    .expect("char owned insert_with_value tail");
            }
            None => {
                trie.insert(t).expect("char owned insert tail");
            }
        }
    }
    trie.sync().expect("char sync");
}

fn char_snapshot<V>(trie: &PersistentARTrieChar<V>) -> BTreeMap<Vec<u8>, Option<Vec<u8>>>
where
    V: DictionaryValue + Serialize,
{
    let mut out = BTreeMap::new();
    let terms = trie
        .iter_prefix("")
        .expect("char iter_prefix")
        .unwrap_or_default();
    for term in terms {
        let v = trie.get_value(&term);
        let enc =
            v.map(|v| libdictenstein::serialization::bincode_compat::serialize(&v).expect("enc"));
        out.insert(term.into_bytes(), normalize_value(enc));
    }
    out
}

fn assert_regime(path: &Path, regime: RankRegime, ctx: &str) {
    let wal = path.with_extension("wal");
    let actual = WalReader::read_header(&wal)
        .map(|h| h.regime())
        .unwrap_or(RankRegime::Owned);
    assert_eq!(actual, regime, "{ctx}: WAL regime mismatch ({wal:?})");
}

// ============================================================================
// Byte correspondence: oracle (legacy owned reopen) vs converted (production reopen).
// ============================================================================

fn byte_correspond<V>(tag: &str, pre: &[(Vec<u8>, Option<V>)], tail: &[(Vec<u8>, Option<V>)])
where
    V: DictionaryValue + Serialize + Clone + PartialEq,
{
    // L1.3: the legacy owned-loader oracle reopen is GONE. Oracle = the LWW-folded fixture content
    // (the logical state, loader-independent): apply `pre` then `tail`, last-writer-wins per term,
    // each value normalized exactly as `byte_snapshot` does (`()`/empty-bytes → membership `None`).
    let mut oracle: BTreeMap<Vec<u8>, Option<Vec<u8>>> = BTreeMap::new();
    for (t, v) in pre.iter().chain(tail.iter()) {
        let enc = v.as_ref().map(|v| {
            libdictenstein::serialization::bincode_compat::serialize(v).expect("model enc")
        });
        oracle.insert(t.clone(), normalize_value(enc));
    }

    // Converted: a byte-identical fixture reopened via production `open` (converts the
    // Owned-regime eligible file INTO the overlay; reads via the overlay).
    let dir_conv = scratch(&format!("{tag}-conv"));
    let path_conv = dir_conv.path().join("t.part");
    byte_build_owned::<V>(&path_conv, pre, tail);
    let converted = {
        let trie = PersistentARTrie::<V>::open(&path_conv).expect("byte converted reopen");
        assert_regime(
            &path_conv,
            RankRegime::Overlay,
            &format!("{tag}: converted is Overlay"),
        );
        byte_snapshot(&trie)
    };

    assert_eq!(
        converted, oracle,
        "{tag}: converted-reopen snapshot must EQUAL the pre-F7 owned-reopen oracle"
    );
}

fn char_correspond<V>(tag: &str, pre: &[(String, Option<V>)], tail: &[(String, Option<V>)])
where
    V: DictionaryValue + Serialize + Clone + PartialEq,
{
    // L1.3: the legacy owned-loader oracle reopen is GONE. Oracle = the LWW-folded fixture content
    // (see `byte_correspond`); char terms are `String` → key bytes via `into_bytes`.
    let mut oracle: BTreeMap<Vec<u8>, Option<Vec<u8>>> = BTreeMap::new();
    for (t, v) in pre.iter().chain(tail.iter()) {
        let enc = v.as_ref().map(|v| {
            libdictenstein::serialization::bincode_compat::serialize(v).expect("model enc")
        });
        oracle.insert(t.clone().into_bytes(), normalize_value(enc));
    }

    let dir_conv = scratch(&format!("{tag}-conv"));
    let path_conv = dir_conv.path().join("t.artc");
    char_build_owned::<V>(&path_conv, pre, tail);
    let converted = {
        let trie = PersistentARTrieChar::<V>::open(&path_conv).expect("char converted reopen");
        assert_regime(
            &path_conv,
            RankRegime::Overlay,
            &format!("{tag}: converted is Overlay"),
        );
        char_snapshot(&trie)
    };

    assert_eq!(
        converted, oracle,
        "{tag}: char converted-reopen snapshot must EQUAL the pre-F7 owned-reopen oracle"
    );
}

// ============================================================================
// Tests — byte + char × V ∈ {(), counter, String, Small}. Each fixture includes "",
// term-only members, valued terms, a shared prefix, a deep key, and LWW overwrites in the
// tail.
// ============================================================================

#[test]
fn byte_unit_correspondence() {
    let pre: Vec<(Vec<u8>, Option<()>)> = vec![
        (b"".to_vec(), Some(())),
        (b"alpha".to_vec(), Some(())),
        (b"alphabet".to_vec(), Some(())),
        (vec![b'z'; 250], Some(())),
    ];
    let tail: Vec<(Vec<u8>, Option<()>)> = vec![
        (b"gamma".to_vec(), Some(())),
        (b"\x00\x01".to_vec(), Some(())),
    ];
    byte_correspond::<()>("byte-unit", &pre, &tail);
}

#[test]
fn byte_i64_correspondence() {
    let pre: Vec<(Vec<u8>, Option<i64>)> = vec![
        (b"".to_vec(), Some(13)),
        (b"apple".to_vec(), Some(3)),
        (b"member_only".to_vec(), None),
        (b"application".to_vec(), Some(17)),
    ];
    let tail: Vec<(Vec<u8>, Option<i64>)> = vec![
        (b"apple".to_vec(), Some(99)), // LWW overwrite
        (b"tail_only".to_vec(), Some(-7)),
    ];
    byte_correspond::<i64>("byte-i64", &pre, &tail);
}

#[test]
fn byte_string_correspondence() {
    let pre: Vec<(Vec<u8>, Option<String>)> = vec![
        (b"".to_vec(), Some("EMPTY".into())),
        (b"k1".to_vec(), Some("v1".into())),
        (b"k_member".to_vec(), None),
    ];
    let tail: Vec<(Vec<u8>, Option<String>)> = vec![
        (b"k1".to_vec(), Some("v1b".into())),
        (b"k2".to_vec(), Some("v2".into())),
    ];
    byte_correspond::<String>("byte-string", &pre, &tail);
}

#[test]
fn byte_small_struct_correspondence() {
    let pre: Vec<(Vec<u8>, Option<Small>)> = vec![
        (
            b"".to_vec(),
            Some(Small {
                a: 1,
                b: "x".into(),
            }),
        ),
        (
            b"s1".to_vec(),
            Some(Small {
                a: 2,
                b: "y".into(),
            }),
        ),
        (b"s_member".to_vec(), None),
    ];
    let tail: Vec<(Vec<u8>, Option<Small>)> = vec![(
        b"s2".to_vec(),
        Some(Small {
            a: 3,
            b: "z".into(),
        }),
    )];
    byte_correspond::<Small>("byte-small", &pre, &tail);
}

#[test]
fn char_unit_correspondence() {
    let pre: Vec<(String, Option<()>)> = vec![
        ("".into(), Some(())),
        ("alpha".into(), Some(())),
        ("日本".into(), Some(())),
        ("🎉x".into(), Some(())),
    ];
    let tail: Vec<(String, Option<()>)> = vec![("gamma".into(), Some(()))];
    char_correspond::<()>("char-unit", &pre, &tail);
}

#[test]
fn char_u64_correspondence() {
    let pre: Vec<(String, Option<u64>)> = vec![
        ("".into(), Some(13)),
        ("apple".into(), Some(3)),
        ("member_only".into(), None),
        // u64 count above i64::MAX (the bit-pattern-faithful counter decode).
        ("huge".into(), Some(u64::MAX - 5)),
    ];
    let tail: Vec<(String, Option<u64>)> =
        vec![("apple".into(), Some(77)), ("tail_only".into(), Some(1))];
    char_correspond::<u64>("char-u64", &pre, &tail);
}

#[test]
fn char_string_correspondence() {
    let pre: Vec<(String, Option<String>)> = vec![
        ("".into(), Some("EMPTY".into())),
        ("alpha".into(), Some("A".into())),
        ("member".into(), None),
    ];
    let tail: Vec<(String, Option<String>)> = vec![
        ("alpha".into(), Some("A2".into())),
        ("beta".into(), Some("C".into())),
    ];
    char_correspond::<String>("char-string", &pre, &tail);
}

#[test]
fn char_small_struct_correspondence() {
    let pre: Vec<(String, Option<Small>)> = vec![
        (
            "".into(),
            Some(Small {
                a: 1,
                b: "x".into(),
            }),
        ),
        (
            "s1".into(),
            Some(Small {
                a: 2,
                b: "y".into(),
            }),
        ),
        ("s_member".into(), None),
    ];
    let tail: Vec<(String, Option<Small>)> = vec![(
        "s2".into(),
        Some(Small {
            a: 3,
            b: "z".into(),
        }),
    )];
    char_correspond::<Small>("char-small", &pre, &tail);
}
