//! Executable correspondence checks for fuzzy candidate coverage.
//!
//! Rocq model:
//! `formal-verification/rocq/Spec/FuzzyCandidateCoverageSpec.v`
//!
//! The checked boundary is the WallBreaker candidate phase: for a query split
//! into `budget + 1` nonempty pieces, any term within the edit budget must be
//! surfaced by at least one exact substring piece.  The edit-distance
//! transducer itself remains upstream-owned by `liblevenshtein`.

use libdictenstein::scdawg::char::ScdawgChar;
use libdictenstein::scdawg::Scdawg;
use libdictenstein::substring::SubstringDictionary;
use std::collections::BTreeSet;

fn split_ascii_query_pieces(query: &str, budget: usize) -> Option<Vec<String>> {
    let piece_count = budget.checked_add(1)?;
    let len = query.len();
    if len < piece_count {
        return None;
    }

    let base = len / piece_count;
    let remainder = len % piece_count;
    let mut pieces = Vec::with_capacity(piece_count);
    let mut start = 0;
    for index in 0..piece_count {
        let width = base + usize::from(index < remainder);
        let end = start + width;
        pieces.push(query[start..end].to_string());
        start = end;
    }
    Some(pieces)
}

fn split_char_query_pieces(query: &str, budget: usize) -> Option<Vec<String>> {
    let piece_count = budget.checked_add(1)?;
    let chars: Vec<char> = query.chars().collect();
    if chars.len() < piece_count {
        return None;
    }

    let base = chars.len() / piece_count;
    let remainder = chars.len() % piece_count;
    let mut pieces = Vec::with_capacity(piece_count);
    let mut start = 0;
    for index in 0..piece_count {
        let width = base + usize::from(index < remainder);
        let end = start + width;
        pieces.push(chars[start..end].iter().collect());
        start = end;
    }
    Some(pieces)
}

fn levenshtein_chars(left: &str, right: &str) -> usize {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0; right.len() + 1];

    for (i, left_ch) in left.iter().enumerate() {
        current[0] = i + 1;
        for (j, right_ch) in right.iter().enumerate() {
            let substitution = previous[j] + usize::from(left_ch != right_ch);
            let insertion = current[j] + 1;
            let deletion = previous[j + 1] + 1;
            current[j + 1] = substitution.min(insertion).min(deletion);
        }
        std::mem::swap(&mut previous, &mut current);
    }

    previous[right.len()]
}

fn replacement_ascii(ch: char) -> char {
    match ch {
        'a' => 'b',
        'b' => 'c',
        'c' => 'd',
        _ => 'a',
    }
}

fn replacement_unicode(ch: char) -> char {
    match ch {
        'a' => 'é',
        'é' => '文',
        '文' => '🎉',
        '🎉' => 'β',
        _ => 'a',
    }
}

fn mutation_positions(len: usize, budget: usize, seed: usize) -> BTreeSet<usize> {
    let mut positions = BTreeSet::new();
    let mut salt = 0;
    while positions.len() < budget.min(len) {
        positions.insert((seed + salt * 3) % len);
        salt += 1;
    }
    positions
}

fn substitute_chars(
    term: &str,
    budget: usize,
    seed: usize,
    replacement: fn(char) -> char,
) -> String {
    let mut chars: Vec<char> = term.chars().collect();
    for position in mutation_positions(chars.len(), budget, seed) {
        chars[position] = replacement(chars[position]);
    }
    chars.into_iter().collect()
}

fn assert_byte_candidate_coverage(terms: &[String], target: &str, query: &str, budget: usize) {
    assert!(
        levenshtein_chars(target, query) <= budget,
        "{query:?} must be within budget {budget} of {target:?}"
    );
    let pieces = split_ascii_query_pieces(query, budget)
        .expect("coverage claim requires budget + 1 nonempty query pieces");
    assert_eq!(pieces.len(), budget + 1);
    assert!(pieces.iter().all(|piece| !piece.is_empty()));

    let dict = Scdawg::<()>::from_terms(terms.iter().map(String::as_str));
    let covering_pieces: Vec<String> = pieces
        .iter()
        .filter(|piece| {
            dict.find_exact_substring(piece)
                .iter()
                .any(|candidate| candidate.term == target)
        })
        .cloned()
        .collect();

    assert!(
        !covering_pieces.is_empty(),
        "no query piece from {pieces:?} surfaced target {target:?}"
    );
}

fn assert_char_candidate_coverage(terms: &[String], target: &str, query: &str, budget: usize) {
    assert!(
        levenshtein_chars(target, query) <= budget,
        "{query:?} must be within budget {budget} of {target:?}"
    );
    let pieces = split_char_query_pieces(query, budget)
        .expect("coverage claim requires budget + 1 nonempty query pieces");
    assert_eq!(pieces.len(), budget + 1);
    assert!(pieces.iter().all(|piece| !piece.is_empty()));

    let dict = ScdawgChar::<()>::from_terms(terms.iter().map(String::as_str));
    let covering_pieces: Vec<String> = pieces
        .iter()
        .filter(|piece| {
            dict.find_exact_substring(piece)
                .iter()
                .any(|candidate| candidate.term == target)
        })
        .cloned()
        .collect();

    assert!(
        !covering_pieces.is_empty(),
        "no query piece from {pieces:?} surfaced target {target:?}"
    );
}

#[test]
fn byte_candidate_coverage_handles_common_edit_shapes() {
    let cases = [
        ("algorithm", "algoritm", 1usize),
        ("cathedral", "catxhedral", 1),
        ("banana", "banona", 1),
        ("kitten", "sitten", 1),
        ("synchronization", "synxhronizqtion", 2),
    ];

    for (target, query, budget) in cases {
        let terms = vec![
            target.to_string(),
            "unrelated".to_string(),
            "candidate".to_string(),
        ];
        assert_byte_candidate_coverage(&terms, target, query, budget);
    }
}

#[test]
fn char_candidate_coverage_handles_unicode_edit_shapes() {
    let cases = [
        ("café文🎉delta", "café文xdelta", 1usize),
        ("東京タワー", "東京ワー", 1),
        ("naïve文", "naïve語文", 1),
        ("a文é🎉β", "x文éyβ", 2),
    ];

    for (target, query, budget) in cases {
        let terms = vec![
            target.to_string(),
            "別候補".to_string(),
            "unrelated".to_string(),
        ];
        assert_char_candidate_coverage(&terms, target, query, budget);
    }
}

#[test]
fn short_queries_are_outside_full_partition_scope() {
    assert!(split_ascii_query_pieces("ab", 2).is_none());
    assert!(split_char_query_pieces("文🎉", 2).is_none());
    assert!(split_ascii_query_pieces("abc", 2).is_some());
    assert!(split_char_query_pieces("文🎉é", 2).is_some());
}

#[test]
fn generated_byte_substitution_matrix_keeps_a_covering_piece() {
    for target in ["abcdef", "banana", "cabdab", "algorithm", "searcher"] {
        for budget in 1usize..=2 {
            for seed in 0usize..8 {
                if target.len() < budget + 1 {
                    continue;
                }
                let query = substitute_chars(target, budget, seed, replacement_ascii);
                let terms = vec![
                    target.to_string(),
                    "unrelated".to_string(),
                    "candidate".to_string(),
                ];
                assert_byte_candidate_coverage(&terms, target, &query, budget);
            }
        }
    }
}

#[test]
fn generated_unicode_substitution_matrix_keeps_a_covering_piece() {
    for target in ["aé文🎉β", "文aéβ", "東京文é", "café文", "β文🎉aé"] {
        for budget in 1usize..=2 {
            for seed in 0usize..8 {
                if target.chars().count() < budget + 1 {
                    continue;
                }
                let query = substitute_chars(target, budget, seed, replacement_unicode);
                let terms = vec![
                    target.to_string(),
                    "別候補".to_string(),
                    "unrelated".to_string(),
                ];
                assert_char_candidate_coverage(&terms, target, &query, budget);
            }
        }
    }
}
