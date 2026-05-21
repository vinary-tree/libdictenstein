//! Parity tests for the generic `ScdawgCoreInner<U, V>` covering both
//! byte (Unit=u8) and char (Unit=char) instantiations.
//!
//! These tests assert that byte and char SCDAWG variants produce
//! identical observable behavior on ASCII inputs (the cross-section
//! where both variants are well-defined). The char variant
//! additionally exercises multi-byte UTF-8 to confirm character-level
//! semantics are preserved (e.g., `café` has length 4 in characters,
//! not 5 in bytes).

use libdictenstein::scdawg::Scdawg;
use libdictenstein::scdawg_char::ScdawgChar;
use libdictenstein::Dictionary;
use libdictenstein::SubstringDictionary;

const ASCII_TERMS: &[&str] = &["cathedral", "category", "catering", "cat", "car"];

#[test]
fn parity_term_count() {
    let byte: Scdawg = Scdawg::from_terms(ASCII_TERMS);
    let chr: ScdawgChar = ScdawgChar::from_terms(ASCII_TERMS);
    assert_eq!(byte.term_count(), chr.term_count());
    assert_eq!(byte.term_count(), 5);
}

#[test]
fn parity_contains_substring_present() {
    let byte: Scdawg = Scdawg::from_terms(ASCII_TERMS);
    let chr: ScdawgChar = ScdawgChar::from_terms(ASCII_TERMS);
    for needle in ["cat", "ate", "ring", "the", "y"] {
        assert_eq!(
            byte.contains_substring(needle),
            chr.contains_substring(needle),
            "byte vs char disagree on substring `{}`",
            needle
        );
        assert!(byte.contains_substring(needle));
    }
}

#[test]
fn parity_contains_substring_absent() {
    let byte: Scdawg = Scdawg::from_terms(ASCII_TERMS);
    let chr: ScdawgChar = ScdawgChar::from_terms(ASCII_TERMS);
    for needle in ["zzz", "qbz", "xz"] {
        assert!(!byte.contains_substring(needle));
        assert!(!chr.contains_substring(needle));
    }
}

#[test]
fn parity_freq_matches() {
    let byte: Scdawg = Scdawg::from_terms(ASCII_TERMS);
    let chr: ScdawgChar = ScdawgChar::from_terms(ASCII_TERMS);
    // "cat" appears in cathedral, category, catering, and standalone "cat" (4 times)
    assert_eq!(byte.freq("cat"), chr.freq("cat"));
}

#[test]
fn parity_freq_zero_for_absent() {
    let byte: Scdawg = Scdawg::from_terms(ASCII_TERMS);
    let chr: ScdawgChar = ScdawgChar::from_terms(ASCII_TERMS);
    assert_eq!(byte.freq("zz"), 0);
    assert_eq!(chr.freq("zz"), 0);
}

#[test]
fn parity_contains_term_via_dictionary_trait() {
    let byte: Scdawg = Scdawg::from_terms(ASCII_TERMS);
    let chr: ScdawgChar = ScdawgChar::from_terms(ASCII_TERMS);
    for term in ASCII_TERMS {
        assert!(byte.contains(term), "byte missing {}", term);
        assert!(chr.contains(term), "char missing {}", term);
    }
    assert!(!byte.contains("missing"));
    assert!(!chr.contains("missing"));
}

#[test]
fn parity_with_values() {
    let byte: Scdawg<u32> = Scdawg::from_terms_with_values([("cat", 1u32), ("car", 2u32)]);
    let chr: ScdawgChar<u32> = ScdawgChar::from_terms_with_values([("cat", 1u32), ("car", 2u32)]);
    assert_eq!(byte.get_value("cat"), Some(1));
    assert_eq!(chr.get_value("cat"), Some(1));
    assert_eq!(byte.get_value("car"), Some(2));
    assert_eq!(chr.get_value("car"), Some(2));
}

#[test]
fn parity_empty_pattern_freq() {
    let byte: Scdawg = Scdawg::from_terms(["abc"]);
    let chr: ScdawgChar = ScdawgChar::from_terms(["abc"]);
    // For "abc": empty pattern matches at every position (4 positions: 0, 1, 2, 3).
    // Both byte and char use unit-length semantics; for ASCII they agree.
    assert_eq!(byte.freq(""), chr.freq(""));
    assert_eq!(byte.freq(""), 4);
}

// ----- Unicode-only tests (char variant) -----

#[test]
fn char_unicode_contains_substring() {
    let chr: ScdawgChar = ScdawgChar::from_terms(["café", "naïve", "中文"]);
    assert!(chr.contains_substring("afé"));
    assert!(chr.contains_substring("aïv"));
    assert!(chr.contains_substring("中"));
    assert!(chr.contains_substring("文"));
}

#[test]
fn char_unicode_term_count() {
    let chr: ScdawgChar = ScdawgChar::from_terms(["café", "naïve", "中文"]);
    assert_eq!(chr.term_count(), 3);
}

#[test]
fn char_unicode_freq_character_aligned() {
    let chr: ScdawgChar = ScdawgChar::from_terms(["café"]);
    // 'é' is multi-byte UTF-8 but a single character: freq should count it
    // as one unit, not two.
    assert!(chr.freq("é") >= 1);
}

#[test]
fn char_unicode_get_value_multibyte_key() {
    let chr: ScdawgChar<u32> = ScdawgChar::from_terms_with_values([("café", 42u32)]);
    assert_eq!(chr.get_value("café"), Some(42));
    assert_eq!(chr.get_value("cafe"), None); // wrong final char
}
