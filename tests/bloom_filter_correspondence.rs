//! Executable correspondence checks for Bloom filter safety laws.
//!
//! These tests instantiate the semantic obligations from
//! `formal-verification/rocq/Spec/BloomFilterSpec.v` against the public
//! `BloomFilter` API.  They check no-false-negative behavior, parameter
//! normalization, byte/string API refinement, clear/reinsert traces, and the
//! Bloom-backed DynamicDawg lookup path.  They do not assert a probabilistic
//! false-positive rate.

use libdictenstein::dynamic_dawg::DynamicDawg;
use libdictenstein::dynamic_dawg_char::DynamicDawgChar;
use libdictenstein::BloomFilter;
use proptest::prelude::*;
use rustc_hash::FxHasher;
use std::collections::BTreeSet;
use std::hash::{Hash, Hasher};

#[derive(Debug, Clone)]
struct ReferenceBloom {
    bits: BTreeSet<usize>,
    bit_count: usize,
    hash_count: usize,
}

impl ReferenceBloom {
    fn new(expected_elements: usize) -> Self {
        Self::with_params(expected_elements.saturating_mul(10), 3)
    }

    fn with_params(bit_count: usize, hash_count: usize) -> Self {
        let requested_bits = bit_count.max(64);
        let chunk_count = (requested_bits + 63) / 64;

        Self {
            bits: BTreeSet::new(),
            bit_count: chunk_count * 64,
            hash_count: hash_count.max(1),
        }
    }

    fn insert_bytes(&mut self, bytes: &[u8]) {
        for seed in 0..self.hash_count {
            self.bits.insert(self.bit_index(bytes, seed));
        }
    }

    fn might_contain_bytes(&self, bytes: &[u8]) -> bool {
        (0..self.hash_count).all(|seed| self.bits.contains(&self.bit_index(bytes, seed)))
    }

    fn clear(&mut self) {
        self.bits.clear();
    }

    fn bit_index(&self, bytes: &[u8], seed: usize) -> usize {
        (hash_with_seed(bytes, seed as u64) % self.bit_count as u64) as usize
    }
}

fn hash_with_seed(bytes: &[u8], seed: u64) -> u64 {
    let mut hasher = FxHasher::default();
    seed.hash(&mut hasher);
    bytes.hash(&mut hasher);
    hasher.finish()
}

fn assert_matches_reference(filter: &BloomFilter, reference: &ReferenceBloom, probes: &[Vec<u8>]) {
    assert_eq!(filter.capacity(), reference.bit_count);
    assert_eq!(filter.hash_count(), reference.hash_count);

    for probe in probes {
        assert_eq!(
            filter.might_contain_bytes(probe),
            reference.might_contain_bytes(probe),
            "public BloomFilter diverged from reference model for bytes: {:?}",
            probe
        );
    }
}

fn fixed_byte_cases() -> Vec<Vec<u8>> {
    vec![
        Vec::new(),
        b"alpha".to_vec(),
        b"alphabet".to_vec(),
        b"beta".to_vec(),
        b"beta\0suffix".to_vec(),
        vec![0x00, 0xff, 0x10, 0x80],
        "b\u{00e9}ta".as_bytes().to_vec(),
        "\u{4e00}\u{4e8c}\u{4e09}".as_bytes().to_vec(),
    ]
}

fn generated_byte_sequence() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..=32)
}

fn generated_unicode_string() -> impl Strategy<Value = String> {
    prop_oneof![
        prop::string::string_regex("[a-zA-Z]{0,16}").expect("valid ASCII regex"),
        prop::sample::select(vec![
            String::new(),
            "alpha".to_string(),
            "b\u{00e9}ta".to_string(),
            "\u{03b3}\u{03ac}\u{03bc}\u{03bc}\u{03b1}".to_string(),
            "\u{4e00}\u{4e8c}\u{4e09}".to_string(),
        ]),
    ]
}

#[test]
fn constructors_normalize_public_parameters() {
    let zero_custom = BloomFilter::with_params(0, 0);
    assert_eq!(zero_custom.capacity(), 64);
    assert_eq!(zero_custom.hash_count(), 1);

    let rounded_custom = BloomFilter::with_params(65, 0);
    assert_eq!(rounded_custom.capacity(), 128);
    assert_eq!(rounded_custom.hash_count(), 1);

    let empty_new = BloomFilter::new(0);
    assert_eq!(empty_new.capacity(), 64);
    assert_eq!(empty_new.hash_count(), 3);

    let rounded_new = BloomFilter::new(7);
    assert_eq!(rounded_new.capacity(), 128);
    assert_eq!(rounded_new.hash_count(), 3);
}

#[test]
fn byte_api_matches_reference_bitset_model() {
    let mut filter = BloomFilter::with_params(130, 5);
    let mut reference = ReferenceBloom::with_params(130, 5);
    let mut probes = fixed_byte_cases();
    probes.extend([
        b"absent".to_vec(),
        b"alp".to_vec(),
        b"alphabetic".to_vec(),
        vec![0x01, 0x02, 0x03],
    ]);

    assert_matches_reference(&filter, &reference, &probes);

    for bytes in fixed_byte_cases() {
        filter.insert_bytes(&bytes);
        reference.insert_bytes(&bytes);
        assert!(filter.might_contain_bytes(&bytes));
        assert_matches_reference(&filter, &reference, &probes);
    }
}

#[test]
fn clear_rejects_prior_evidence_and_reinsert_restores_membership() {
    let mut filter = BloomFilter::with_params(512, 4);
    let mut reference = ReferenceBloom::with_params(512, 4);
    let terms = fixed_byte_cases();

    for term in &terms {
        filter.insert_bytes(term);
        reference.insert_bytes(term);
    }

    assert_matches_reference(&filter, &reference, &terms);

    filter.clear();
    reference.clear();

    for term in &terms {
        assert!(!filter.might_contain_bytes(term));
    }
    assert_matches_reference(&filter, &reference, &terms);

    filter.insert_bytes(b"alpha");
    reference.insert_bytes(b"alpha");
    assert!(filter.might_contain_bytes(b"alpha"));
    assert_matches_reference(&filter, &reference, &terms);
}

#[test]
fn string_api_refines_byte_api_for_unicode_payloads() {
    let terms = [
        "",
        "alpha",
        "b\u{00e9}ta",
        "\u{03b3}\u{03ac}\u{03bc}\u{03bc}\u{03b1}",
        "\u{4e00}\u{4e8c}\u{4e09}",
    ];

    let mut string_filter = BloomFilter::with_params(512, 3);
    let mut byte_filter = BloomFilter::with_params(512, 3);

    for term in terms {
        string_filter.insert(term);
        byte_filter.insert_bytes(term.as_bytes());
    }

    for term in terms {
        assert_eq!(
            string_filter.might_contain(term),
            byte_filter.might_contain_bytes(term.as_bytes()),
            "string API must refine byte API for {term:?}"
        );
        assert_eq!(
            string_filter.might_contain(term),
            string_filter.might_contain_bytes(term.as_bytes()),
            "same filter string/byte query mismatch for {term:?}"
        );
    }
}

#[test]
fn duplicate_inserts_preserve_no_false_negative_guarantee() {
    let mut filter = BloomFilter::with_params(256, 3);
    let mut reference = ReferenceBloom::with_params(256, 3);
    let term = b"duplicate";

    for _ in 0..8 {
        filter.insert_bytes(term);
        reference.insert_bytes(term);
        assert!(filter.might_contain_bytes(term));
        assert_matches_reference(&filter, &reference, &[term.to_vec()]);
    }
}

#[test]
fn bloom_backed_dynamic_dawg_lookup_never_rejects_inserted_terms() {
    let byte: DynamicDawg<i32> = DynamicDawg::with_config(1.05, Some(128));
    let char_dawg: DynamicDawgChar<i32> = DynamicDawgChar::with_config(1.05, Some(128));
    let terms = [
        ("alpha", 1),
        ("alphabet", 2),
        ("alphanumeric", 3),
        ("b\u{00e9}ta", 4),
        ("\u{4e00}\u{4e8c}\u{4e09}", 5),
        ("suffix", 6),
        ("presuffix", 7),
    ];

    for (term, value) in terms {
        assert!(byte.insert_with_value(term, value));
        assert!(char_dawg.insert_with_value(term, value));
    }

    assert!(!byte.update_or_insert("alphabet", 0, |value| *value += 10));
    assert!(!char_dawg.update_or_insert("alphabet", 0, |value| *value += 10));
    assert!(byte.remove("suffix"));
    assert!(char_dawg.remove("suffix"));

    let _ = byte.compact();
    let _ = byte.minimize();
    let _ = char_dawg.compact();
    let _ = char_dawg.minimize();

    for (term, original_value) in terms {
        if term == "suffix" {
            assert!(!byte.contains(term));
            assert!(!char_dawg.contains(term));
            assert_eq!(byte.get_value(term), None);
            assert_eq!(char_dawg.get_value(term), None);
        } else {
            let expected = if term == "alphabet" {
                original_value + 10
            } else {
                original_value
            };
            assert!(byte.contains(term), "byte DAWG lost {term}");
            assert!(char_dawg.contains(term), "char DAWG lost {term}");
            assert_eq!(byte.get_value(term), Some(expected));
            assert_eq!(char_dawg.get_value(term), Some(expected));
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn generated_byte_traces_have_no_false_negatives_and_match_reference(
        mut terms in prop::collection::vec(generated_byte_sequence(), 0..=40),
        requested_bits in 0usize..=512,
        requested_hashes in 0usize..=8,
    ) {
        terms.extend(fixed_byte_cases());
        let mut filter = BloomFilter::with_params(requested_bits, requested_hashes);
        let mut reference = ReferenceBloom::with_params(requested_bits, requested_hashes);

        for term in &terms {
            filter.insert_bytes(term);
            reference.insert_bytes(term);
            prop_assert!(filter.might_contain_bytes(term), "false negative for {:?}", term);
        }

        assert_matches_reference(&filter, &reference, &terms);

        filter.clear();
        reference.clear();
        for term in &terms {
            prop_assert!(!filter.might_contain_bytes(term), "clear left evidence for {:?}", term);
        }
        assert_matches_reference(&filter, &reference, &terms);
    }

    #[test]
    fn generated_unicode_strings_refine_bytes_and_keep_membership(
        terms in prop::collection::vec(generated_unicode_string(), 0..=32),
    ) {
        let mut string_filter = BloomFilter::new(terms.len());
        let mut byte_filter = BloomFilter::new(terms.len());
        let mut reference = ReferenceBloom::new(terms.len());

        for term in &terms {
            string_filter.insert(term);
            byte_filter.insert_bytes(term.as_bytes());
            reference.insert_bytes(term.as_bytes());
            prop_assert!(string_filter.might_contain(term), "string false negative for {:?}", term);
            prop_assert!(
                byte_filter.might_contain_bytes(term.as_bytes()),
                "byte false negative for {:?}",
                term
            );
        }

        for term in &terms {
            prop_assert_eq!(
                string_filter.might_contain(term),
                byte_filter.might_contain_bytes(term.as_bytes()),
                "string/byte refinement mismatch for {:?}",
                term
            );
            prop_assert_eq!(
                string_filter.might_contain(term),
                reference.might_contain_bytes(term.as_bytes()),
                "string API diverged from reference for {:?}",
                term
            );
        }
    }
}
