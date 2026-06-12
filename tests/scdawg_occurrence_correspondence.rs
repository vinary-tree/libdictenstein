//! Executable correspondence checks for SCDAWG occurrence-construction laws.

use std::collections::BTreeSet;

use libdictenstein::scdawg::char::ScdawgChar;
use libdictenstein::scdawg::Scdawg;
use libdictenstein::substring::{BidirectionalDictionaryNode, SubstringDictionary};
use libdictenstein::{Dictionary, MappedDictionary};
use proptest::prelude::*;

type OccurrenceKey = (String, usize);

fn byte_occurrences(terms: &BTreeSet<String>, pattern: &str) -> BTreeSet<OccurrenceKey> {
    let pattern_bytes = pattern.as_bytes();
    let mut expected = BTreeSet::new();
    for term in terms {
        let term_bytes = term.as_bytes();
        if pattern_bytes.len() > term_bytes.len() {
            continue;
        }

        for start in 0..=term_bytes.len() - pattern_bytes.len() {
            if &term_bytes[start..start + pattern_bytes.len()] == pattern_bytes {
                expected.insert((term.clone(), start));
            }
        }
    }
    expected
}

fn char_occurrences(terms: &BTreeSet<String>, pattern: &str) -> BTreeSet<OccurrenceKey> {
    let pattern_chars: Vec<char> = pattern.chars().collect();
    let mut expected = BTreeSet::new();
    for term in terms {
        let term_chars: Vec<char> = term.chars().collect();
        if pattern_chars.len() > term_chars.len() {
            continue;
        }

        for start in 0..=term_chars.len() - pattern_chars.len() {
            if term_chars[start..start + pattern_chars.len()] == pattern_chars {
                expected.insert((term.clone(), start));
            }
        }
    }
    expected
}

fn location_set(locations: &[(String, usize)]) -> BTreeSet<OccurrenceKey> {
    locations.iter().cloned().collect()
}

fn byte_match_set<N>(
    matches: &[libdictenstein::substring::SubstringMatch<N>],
    pattern: &str,
) -> BTreeSet<OccurrenceKey>
where
    N: libdictenstein::DictionaryNode,
{
    matches
        .iter()
        .map(|m| {
            assert_eq!(m.length, pattern.len());
            assert_eq!(m.matched_substring(), pattern);
            (m.term.clone(), m.position)
        })
        .collect()
}

fn char_match_set<N>(
    matches: &[libdictenstein::substring::SubstringMatch<N>],
    pattern: &str,
) -> BTreeSet<OccurrenceKey>
where
    N: libdictenstein::DictionaryNode,
{
    let pattern_len = pattern.chars().count();
    matches
        .iter()
        .map(|m| {
            assert_eq!(m.length, pattern_len);
            assert_eq!(m.matched_substring(), pattern);
            (m.term.clone(), m.position)
        })
        .collect()
}

fn assert_limited_prefix<D>(dict: &D, pattern: &str)
where
    D: SubstringDictionary,
{
    let full = dict.find_exact_substring(pattern);
    for limit in 0..=full.len() + 2 {
        let limited = dict.find_exact_substring_limited(pattern, limit);
        assert!(limited.len() <= limit);
        assert_eq!(
            limited
                .iter()
                .map(|m| (m.term.clone(), m.position, m.length))
                .collect::<Vec<_>>(),
            full.iter()
                .take(limit)
                .map(|m| (m.term.clone(), m.position, m.length))
                .collect::<Vec<_>>()
        );
    }
}

fn assert_byte_occurrence_api(terms: &[String], pattern: &str) {
    assert!(!pattern.is_empty(), "non-empty occurrence correspondence");
    let unique_terms: BTreeSet<String> = terms.iter().cloned().collect();
    let expected = byte_occurrences(&unique_terms, pattern);
    let dict = Scdawg::<()>::from_terms(terms.iter().map(String::as_str));

    let locations = dict.locations(pattern);
    assert_eq!(locations.len(), location_set(&locations).len());
    assert_eq!(location_set(&locations), expected);
    assert_eq!(dict.freq(pattern), expected.len());
    assert_eq!(dict.contains_substring(pattern), !expected.is_empty());

    let matches = dict.find_exact_substring(pattern);
    assert_eq!(matches.len(), byte_match_set(&matches, pattern).len());
    assert_eq!(byte_match_set(&matches, pattern), expected);
    assert_eq!(dict.count_substring_matches(pattern), expected.len());
    assert_limited_prefix(&dict, pattern);

    match dict.find(pattern) {
        Some(handle) => {
            assert!(!expected.is_empty());
            assert_eq!(dict.freq_at(&handle), expected.len());
            let locations_at = dict.locations_at(&handle, pattern.len());
            assert_eq!(locations_at.len(), location_set(&locations_at).len());
            assert_eq!(location_set(&locations_at), expected);
        }
        None => {
            assert!(expected.is_empty());
            assert_eq!(dict.freq(pattern), 0);
            assert!(locations.is_empty());
            assert!(matches.is_empty());
        }
    }

    for term in &unique_terms {
        assert!(Dictionary::contains(&dict, term));
    }
}

fn assert_char_occurrence_api(terms: &[String], pattern: &str) {
    assert!(!pattern.is_empty(), "non-empty occurrence correspondence");
    let unique_terms: BTreeSet<String> = terms.iter().cloned().collect();
    let expected = char_occurrences(&unique_terms, pattern);
    let dict = ScdawgChar::<()>::from_terms(terms.iter().map(String::as_str));
    let pattern_len = pattern.chars().count();

    let locations = dict.locations(pattern);
    assert_eq!(locations.len(), location_set(&locations).len());
    assert_eq!(location_set(&locations), expected);
    assert_eq!(dict.freq(pattern), expected.len());
    assert_eq!(dict.contains_substring(pattern), !expected.is_empty());

    let matches = dict.find_exact_substring(pattern);
    assert_eq!(matches.len(), char_match_set(&matches, pattern).len());
    assert_eq!(char_match_set(&matches, pattern), expected);
    assert_eq!(dict.count_substring_matches(pattern), expected.len());
    assert_limited_prefix(&dict, pattern);

    match dict.find(pattern) {
        Some(handle) => {
            assert!(!expected.is_empty());
            assert_eq!(dict.freq_at(&handle), expected.len());
            let locations_at = dict.locations_at(&handle, pattern_len);
            assert_eq!(locations_at.len(), location_set(&locations_at).len());
            assert_eq!(location_set(&locations_at), expected);
        }
        None => {
            assert!(expected.is_empty());
            assert_eq!(dict.freq(pattern), 0);
            assert!(locations.is_empty());
            assert!(matches.is_empty());
        }
    }

    for term in &unique_terms {
        assert!(Dictionary::contains(&dict, term));
    }
}

fn unicode_string_strategy(max_len: usize) -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![Just('a'), Just('é'), Just('文'), Just('🎉')],
        1..=max_len,
    )
    .prop_map(|chars| chars.into_iter().collect())
}

proptest! {
    #[test]
    fn byte_scdawg_occurrence_apis_refine_reference(
        terms in prop::collection::vec("[abc]{1,6}", 0..8),
        pattern in "[abc]{1,3}",
    ) {
        assert_byte_occurrence_api(&terms, &pattern);
    }

    #[test]
    fn char_scdawg_occurrence_apis_refine_reference(
        terms in prop::collection::vec(unicode_string_strategy(5), 0..8),
        pattern in unicode_string_strategy(3),
    ) {
        assert_char_occurrence_api(&terms, &pattern);
    }
}

#[test]
fn byte_occurrence_apis_report_repeated_and_overlapping_matches() {
    let terms = vec![
        "aaaa".to_string(),
        "banana".to_string(),
        "ababa".to_string(),
        "banana".to_string(),
    ];

    for pattern in ["a", "aa", "ana", "na", "aba", "ba", "z"] {
        assert_byte_occurrence_api(&terms, pattern);
    }
}

#[test]
fn char_occurrence_apis_report_unicode_character_positions() {
    let terms = vec![
        "café".to_string(),
        "éé".to_string(),
        "文🎉文".to_string(),
        "a文é".to_string(),
        "café".to_string(),
    ];

    for pattern in ["é", "fé", "文", "🎉文", "文🎉", "a文", "zz"] {
        assert_char_occurrence_api(&terms, pattern);
    }
}

#[test]
fn duplicate_value_updates_preserve_occurrences() {
    let byte = Scdawg::<u32>::new();
    assert!(byte.insert_with_value("banana", 1));
    assert!(byte.insert_with_value("bandana", 2));
    assert!(!byte.insert_with_value("banana", 7));
    assert_eq!(byte.term_count(), 2);
    assert_eq!(MappedDictionary::get_value(&byte, "banana"), Some(7));
    assert_eq!(
        location_set(&byte.locations("ana")),
        BTreeSet::from([
            ("banana".to_string(), 1),
            ("banana".to_string(), 3),
            ("bandana".to_string(), 4),
        ])
    );
    assert_eq!(byte.freq("ana"), 3);

    let chr = ScdawgChar::<u32>::new();
    assert!(chr.insert_with_value("éé文", 10));
    assert!(chr.insert_with_value("aé文", 20));
    assert!(!chr.insert_with_value("éé文", 30));
    assert_eq!(chr.term_count(), 2);
    assert_eq!(MappedDictionary::get_value(&chr, "éé文"), Some(30));
    assert_eq!(
        location_set(&chr.locations("é文")),
        BTreeSet::from([("éé文".to_string(), 1), ("aé文".to_string(), 1)])
    );
    assert_eq!(chr.freq("é文"), 2);
}

#[test]
fn left_extension_handles_expose_shared_suffix_occurrences() {
    let byte = Scdawg::<()>::from_terms(["abc", "xbc", "zbc"]);
    let bc = byte.find("bc").expect("shared suffix exists");
    let byte_labels: BTreeSet<_> = bc.reverse_edges().map(|(label, _)| label).collect();
    assert!(byte_labels.contains(&b'a'));
    assert!(byte_labels.contains(&b'x'));
    assert!(byte_labels.contains(&b'z'));
    assert!(!bc.reverse_transition(b'a').is_empty());
    assert_eq!(
        location_set(&byte.locations_at(&bc, 2)),
        BTreeSet::from([
            ("abc".to_string(), 1),
            ("xbc".to_string(), 1),
            ("zbc".to_string(), 1),
        ])
    );

    let chr = ScdawgChar::<()>::from_terms(["α文", "β文", "🎉文"]);
    let wen = chr.find("文").expect("shared Unicode suffix exists");
    let char_labels: BTreeSet<_> = wen.reverse_edges().map(|(label, _)| label).collect();
    assert!(char_labels.contains(&'α'));
    assert!(char_labels.contains(&'β'));
    assert!(char_labels.contains(&'🎉'));
    assert!(!wen.reverse_transition('α').is_empty());
    assert_eq!(
        location_set(&chr.locations_at(&wen, 1)),
        BTreeSet::from([
            ("α文".to_string(), 1),
            ("β文".to_string(), 1),
            ("🎉文".to_string(), 1),
        ])
    );
}

#[test]
fn empty_pattern_behavior_is_scoped_out_of_nonempty_occurrence_laws() {
    let byte = Scdawg::<()>::from_terms(["aba", "xyz", "aba"]);
    assert!(byte.contains_substring(""));
    assert!(byte.find("").is_some());
    assert_eq!(
        location_set(&byte.locations("")),
        BTreeSet::from([("aba".to_string(), 0), ("xyz".to_string(), 0)])
    );
    assert_eq!(byte.count_substring_matches(""), 2);
    assert_eq!(byte.freq(""), 8);

    let chr = ScdawgChar::<()>::from_terms(["éé", "文🎉", "éé"]);
    assert!(chr.contains_substring(""));
    assert!(chr.find("").is_some());
    assert_eq!(
        location_set(&chr.locations("")),
        BTreeSet::from([("éé".to_string(), 0), ("文🎉".to_string(), 0)])
    );
    assert_eq!(chr.count_substring_matches(""), 2);
    assert_eq!(chr.freq(""), 6);
}
