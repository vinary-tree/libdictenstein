//! Executable correspondence checks for substring candidate laws.

use std::collections::BTreeSet;

use libdictenstein::scdawg::char::ScdawgChar;
use libdictenstein::scdawg::Scdawg;
use libdictenstein::substring::{SubstringDictionary, SubstringMatch};
use libdictenstein::{Dictionary, DictionaryNode};
use proptest::prelude::*;

type MatchKey = (String, usize, usize);

fn byte_occurrences(terms: &BTreeSet<String>, pattern: &str) -> BTreeSet<MatchKey> {
    if pattern.is_empty() {
        return terms.iter().map(|term| (term.clone(), 0, 0)).collect();
    }

    let pattern_bytes = pattern.as_bytes();
    let mut expected = BTreeSet::new();
    for term in terms {
        let term_bytes = term.as_bytes();
        if pattern_bytes.len() > term_bytes.len() {
            continue;
        }

        for start in 0..=term_bytes.len() - pattern_bytes.len() {
            if &term_bytes[start..start + pattern_bytes.len()] == pattern_bytes {
                expected.insert((term.clone(), start, pattern_bytes.len()));
            }
        }
    }
    expected
}

fn char_occurrences(terms: &BTreeSet<String>, pattern: &str) -> BTreeSet<MatchKey> {
    let pattern_chars: Vec<char> = pattern.chars().collect();
    if pattern_chars.is_empty() {
        return terms.iter().map(|term| (term.clone(), 0, 0)).collect();
    }

    let mut expected = BTreeSet::new();
    for term in terms {
        let term_chars: Vec<char> = term.chars().collect();
        if pattern_chars.len() > term_chars.len() {
            continue;
        }

        for start in 0..=term_chars.len() - pattern_chars.len() {
            if term_chars[start..start + pattern_chars.len()] == pattern_chars {
                expected.insert((term.clone(), start, pattern_chars.len()));
            }
        }
    }
    expected
}

fn match_keys<N: DictionaryNode>(matches: &[SubstringMatch<N>]) -> Vec<MatchKey> {
    matches
        .iter()
        .map(|m| (m.term.clone(), m.position, m.length))
        .collect()
}

fn match_key_set<N: DictionaryNode>(matches: &[SubstringMatch<N>]) -> BTreeSet<MatchKey> {
    match_keys(matches).into_iter().collect()
}

fn char_slice(term: &str, start: usize, len: usize) -> String {
    term.chars().skip(start).take(len).collect()
}

fn char_prefix(term: &str, len: usize) -> String {
    term.chars().take(len).collect()
}

fn char_suffix(term: &str, start: usize) -> String {
    term.chars().skip(start).collect()
}

fn assert_byte_match_contract<N: DictionaryNode>(m: &SubstringMatch<N>, pattern: &str) {
    assert_eq!(m.length, pattern.len());
    assert_eq!(m.matched_substring(), pattern);
    assert_eq!(m.prefix(), &m.term[..m.position]);
    assert_eq!(m.suffix(), &m.term[m.position + m.length..]);
    assert_eq!(m.left_context_len(), m.position);
    assert_eq!(
        m.right_context_len(),
        m.term.len().saturating_sub(m.position + m.length)
    );
}

fn assert_char_match_contract<N: DictionaryNode>(m: &SubstringMatch<N>, pattern: &str) {
    let pattern_len = pattern.chars().count();
    assert_eq!(m.length, pattern_len);
    assert_eq!(m.matched_substring(), pattern);
    assert_eq!(m.prefix(), char_prefix(&m.term, m.position));
    assert_eq!(m.suffix(), char_suffix(&m.term, m.position + pattern_len));
    assert_eq!(m.left_context_len(), m.position);
    assert_eq!(
        m.right_context_len(),
        m.term
            .chars()
            .count()
            .saturating_sub(m.position + pattern_len)
    );
    assert_eq!(char_slice(&m.term, m.position, pattern_len), pattern);
}

fn assert_limited_prefix<N: DictionaryNode, D: SubstringDictionary<Node = N>>(
    dict: &D,
    pattern: &str,
    full: &[SubstringMatch<N>],
) {
    for limit in 0..=full.len() + 2 {
        let limited = dict.find_exact_substring_limited(pattern, limit);
        assert!(limited.len() <= limit);
        assert_eq!(
            match_keys(&limited),
            match_keys(&full.iter().take(limit).cloned().collect::<Vec<_>>())
        );
    }
}

fn assert_byte_substring_candidates(terms: &[String], pattern: &str) {
    assert!(!pattern.is_empty(), "non-empty pattern correspondence");
    let unique_terms: BTreeSet<String> = terms.iter().cloned().collect();
    let expected = byte_occurrences(&unique_terms, pattern);
    let dict = Scdawg::<()>::from_terms(terms.iter().map(String::as_str));

    let matches = dict.find_exact_substring(pattern);
    for m in &matches {
        assert_byte_match_contract(m, pattern);
    }

    let actual = match_key_set(&matches);
    assert_eq!(
        actual.len(),
        matches.len(),
        "substring results must be unique"
    );
    assert_eq!(actual, expected);
    assert_eq!(dict.contains_substring(pattern), !expected.is_empty());
    assert_eq!(dict.count_substring_matches(pattern), expected.len());
    assert_limited_prefix(&dict, pattern, &matches);

    for term in &unique_terms {
        assert!(Dictionary::contains(&dict, term));
    }
}

fn assert_char_substring_candidates(terms: &[String], pattern: &str) {
    assert!(!pattern.is_empty(), "non-empty pattern correspondence");
    let unique_terms: BTreeSet<String> = terms.iter().cloned().collect();
    let expected = char_occurrences(&unique_terms, pattern);
    let dict = ScdawgChar::<()>::from_terms(terms.iter().map(String::as_str));

    let matches = dict.find_exact_substring(pattern);
    for m in &matches {
        assert_char_match_contract(m, pattern);
    }

    let actual = match_key_set(&matches);
    assert_eq!(
        actual.len(),
        matches.len(),
        "substring results must be unique"
    );
    assert_eq!(actual, expected);
    assert_eq!(dict.contains_substring(pattern), !expected.is_empty());
    assert_eq!(dict.count_substring_matches(pattern), expected.len());
    assert_limited_prefix(&dict, pattern, &matches);

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
    fn byte_scdawg_refines_reference_substring_candidates(
        terms in prop::collection::vec("[abc]{1,6}", 0..8),
        pattern in "[abc]{1,3}",
    ) {
        assert_byte_substring_candidates(&terms, &pattern);
    }

    #[test]
    fn char_scdawg_refines_reference_substring_candidates(
        terms in prop::collection::vec(unicode_string_strategy(5), 0..8),
        pattern in unicode_string_strategy(3),
    ) {
        assert_char_substring_candidates(&terms, &pattern);
    }
}

#[test]
fn byte_scdawg_reports_overlapping_and_repeated_occurrences() {
    let terms = vec![
        "aaaa".to_string(),
        "banana".to_string(),
        "aba".to_string(),
        "banana".to_string(),
    ];

    for pattern in ["a", "aa", "ana", "na", "ba", "z"] {
        assert_byte_substring_candidates(&terms, pattern);
    }
}

#[test]
fn char_scdawg_reports_unicode_occurrences_by_character_position() {
    let terms = vec![
        "café".to_string(),
        "éé".to_string(),
        "文🎉文".to_string(),
        "a文é".to_string(),
    ];

    for pattern in ["é", "fé", "文", "🎉文", "文🎉", "a文", "zz"] {
        assert_char_substring_candidates(&terms, pattern);
    }
}

#[test]
fn empty_pattern_behavior_is_explicitly_scoped() {
    let empty_byte = Scdawg::<()>::new();
    assert!(empty_byte.contains_substring(""));
    assert!(empty_byte.find_exact_substring("").is_empty());

    let byte_terms = vec!["alpha".to_string(), "beta".to_string(), "alpha".to_string()];
    let byte = Scdawg::<()>::from_terms(byte_terms.iter().map(String::as_str));
    let byte_matches = byte.find_exact_substring("");
    assert_eq!(
        match_key_set(&byte_matches),
        BTreeSet::from([("alpha".to_string(), 0, 0), ("beta".to_string(), 0, 0)])
    );
    for m in &byte_matches {
        assert_byte_match_contract(m, "");
    }

    let empty_char = ScdawgChar::<()>::new();
    assert!(empty_char.contains_substring(""));
    assert!(empty_char.find_exact_substring("").is_empty());

    let char_terms = vec!["café".to_string(), "文🎉".to_string(), "café".to_string()];
    let char_dict = ScdawgChar::<()>::from_terms(char_terms.iter().map(String::as_str));
    let char_matches = char_dict.find_exact_substring("");
    assert_eq!(
        match_key_set(&char_matches),
        BTreeSet::from([("café".to_string(), 0, 0), ("文🎉".to_string(), 0, 0)])
    );
    for m in &char_matches {
        assert_char_match_contract(m, "");
    }
}
