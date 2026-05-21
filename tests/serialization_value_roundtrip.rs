//! Value-preserving serialization round-trip tests.
//!
//! Trait law under test:
//!
//! ```text
//! ∀ (term, value) in dict:
//!     dict.get_value(term) == Some(value)
//!     →
//!     deserialize(serialize(dict)).get_value(term) == Some(value)
//! ```
//!
//! The legacy `serialize` / `deserialize` path silently drops values (it
//! shipped a `Vec<String>` over the wire). A3 adds `serialize_with_values` /
//! `deserialize_with_values` that round-trip a `Vec<(String, V)>`.

#![cfg(feature = "serialization")]

// A3 wires value-preserving serialization for `Unit = u8` backends.
// B2 extends this to `Unit = char` (DAWGChar, DAT-Char, SuffixAutomatonChar,
// ScdawgChar, PathMap-Char) via `serialize_with_values_char` +
// `extract_terms_with_values_char` parallel helpers. Deserialization uses
// the same `deserialize_with_values` for both unit types because the wire
// format (`Vec<(String, V)>`) is unit-agnostic.
//
// u64 (`DynamicDawgU64`) still has no `*_with_values` path — u64 doesn't
// trivially round-trip through `String`, and the appropriate format
// (JSON array of u64s, separator-delimited tokens, …) needs design input.

use libdictenstein::double_array_trie::DoubleArrayTrie;
use libdictenstein::double_array_trie_char::DoubleArrayTrieChar;
use libdictenstein::dynamic_dawg::DynamicDawg;
use libdictenstein::dynamic_dawg_char::DynamicDawgChar;
use libdictenstein::serialization::{BincodeSerializer, JsonSerializer, PlainTextSerializer};
use libdictenstein::MappedDictionary;

fn pairs_u32() -> Vec<(&'static str, u32)> {
    vec![("apple", 10), ("banana", 20), ("cherry", 30)]
}

fn pairs_string() -> Vec<(&'static str, String)> {
    vec![
        ("alpha", "first".to_string()),
        ("beta", "second".to_string()),
        ("gamma", "third".to_string()),
    ]
}

// ---------- DynamicDawg ----------

#[test]
fn dynamic_dawg_bincode_roundtrip_values() {
    let dict: DynamicDawg<u32> = DynamicDawg::from_terms_with_values(pairs_u32());

    let mut buf = Vec::new();
    BincodeSerializer::serialize_with_values(&dict, &mut buf).unwrap();
    let loaded: DynamicDawg<u32> = BincodeSerializer::deserialize_with_values(&buf[..]).unwrap();

    for (t, v) in pairs_u32() {
        assert_eq!(loaded.get_value(t), Some(v), "term {t}");
    }
}

#[test]
fn dynamic_dawg_json_roundtrip_values() {
    let dict: DynamicDawg<u32> = DynamicDawg::from_terms_with_values(pairs_u32());

    let mut buf = Vec::new();
    JsonSerializer::serialize_with_values(&dict, &mut buf).unwrap();
    let loaded: DynamicDawg<u32> = JsonSerializer::deserialize_with_values(&buf[..]).unwrap();

    for (t, v) in pairs_u32() {
        assert_eq!(loaded.get_value(t), Some(v), "term {t}");
    }
}

#[test]
fn dynamic_dawg_plaintext_roundtrip_values() {
    let dict: DynamicDawg<String> = DynamicDawg::from_terms_with_values(pairs_string());

    let mut buf = Vec::new();
    PlainTextSerializer::serialize_with_values(&dict, &mut buf).unwrap();
    let loaded: DynamicDawg<String> =
        PlainTextSerializer::deserialize_with_values(&buf[..]).unwrap();

    for (t, v) in pairs_string() {
        assert_eq!(loaded.get_value(t).as_deref(), Some(v.as_str()), "term {t}");
    }
}

// ---------- DoubleArrayTrie ----------

#[test]
fn dat_bincode_roundtrip_values() {
    let dict: DoubleArrayTrie<u32> = DoubleArrayTrie::from_terms_with_values(pairs_u32());

    let mut buf = Vec::new();
    BincodeSerializer::serialize_with_values(&dict, &mut buf).unwrap();
    let loaded: DoubleArrayTrie<u32> =
        BincodeSerializer::deserialize_with_values(&buf[..]).unwrap();

    for (t, v) in pairs_u32() {
        assert_eq!(loaded.get_value(t), Some(v), "term {t}");
    }
}

#[test]
fn dat_json_roundtrip_values() {
    let dict: DoubleArrayTrie<u32> = DoubleArrayTrie::from_terms_with_values(pairs_u32());

    let mut buf = Vec::new();
    JsonSerializer::serialize_with_values(&dict, &mut buf).unwrap();
    let loaded: DoubleArrayTrie<u32> = JsonSerializer::deserialize_with_values(&buf[..]).unwrap();

    for (t, v) in pairs_u32() {
        assert_eq!(loaded.get_value(t), Some(v), "term {t}");
    }
}

// ---------- Char-Unit backends (B2 — Unicode value preservation) ----------

#[test]
fn dynamic_dawg_char_bincode_roundtrip_values_unicode() {
    let pairs: Vec<(&str, u32)> = vec![("café", 1), ("naïve", 2), ("日本語", 3)];
    let dict: DynamicDawgChar<u32> = DynamicDawgChar::from_terms_with_values(pairs.clone());

    let mut buf = Vec::new();
    BincodeSerializer::serialize_with_values_char(&dict, &mut buf).unwrap();
    let loaded: DynamicDawgChar<u32> =
        BincodeSerializer::deserialize_with_values(&buf[..]).unwrap();

    for (t, v) in pairs {
        assert_eq!(loaded.get_value(t), Some(v), "term {t}");
    }
}

#[test]
fn dat_char_json_roundtrip_values_unicode() {
    let pairs: Vec<(&str, u32)> = vec![("café", 10), ("naïve", 20), ("日本語", 30)];
    let dict: DoubleArrayTrieChar<u32> = DoubleArrayTrieChar::from_terms_with_values(pairs.clone());

    let mut buf = Vec::new();
    JsonSerializer::serialize_with_values_char(&dict, &mut buf).unwrap();
    let loaded: DoubleArrayTrieChar<u32> =
        JsonSerializer::deserialize_with_values(&buf[..]).unwrap();

    for (t, v) in pairs {
        assert_eq!(loaded.get_value(t), Some(v), "term {t}");
    }
}

#[test]
fn dynamic_dawg_char_plaintext_roundtrip_values_unicode() {
    let pairs: Vec<(&str, String)> = vec![
        ("café", "coffee".to_string()),
        ("naïve", "innocent".to_string()),
        ("日本語", "japanese".to_string()),
    ];
    let dict: DynamicDawgChar<String> = DynamicDawgChar::from_terms_with_values(pairs.clone());

    let mut buf = Vec::new();
    PlainTextSerializer::serialize_with_values_char(&dict, &mut buf).unwrap();
    let loaded: DynamicDawgChar<String> =
        PlainTextSerializer::deserialize_with_values(&buf[..]).unwrap();

    for (t, v) in pairs {
        assert_eq!(loaded.get_value(t).as_deref(), Some(v.as_str()), "term {t}");
    }
}

// ---------- Legacy path silently drops values (regression guard) ----------

/// The original `serialize`/`deserialize` path dropped values silently. This
/// test pins that legacy behavior so the new `_with_values` path can be
/// validated as the value-preserving counterpart. If `serialize` ever starts
/// preserving values too (e.g. via a future format-version bump), update this
/// test.
#[test]
fn legacy_path_drops_values_documented() {
    use libdictenstein::serialization::DictionarySerializer;

    let dict: DynamicDawg<u32> = DynamicDawg::from_terms_with_values(pairs_u32());

    // Sanity check: original dict has the values.
    for (t, v) in pairs_u32() {
        assert_eq!(dict.get_value(t), Some(v));
    }

    let mut buf = Vec::new();
    BincodeSerializer::serialize(&dict, &mut buf).unwrap();
    let loaded: DynamicDawg<u32> = BincodeSerializer::deserialize(&buf[..]).unwrap();

    // Terms survive (`contains` true), but values are lost: the reloaded
    // dict has no value association at all — `get_value` returns None even
    // though the term is present.
    for (t, _) in pairs_u32() {
        assert!(
            loaded.contains(t),
            "term {t} should survive legacy round-trip"
        );
        assert_eq!(
            loaded.get_value(t),
            None,
            "legacy serializer drops values (term present, value None) for term {t}"
        );
    }
}
