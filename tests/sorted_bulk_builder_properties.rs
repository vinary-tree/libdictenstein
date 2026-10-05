//! Reference laws for the shared sorted, freeze-once DAWG builder.
//!
//! The oracle computes residual languages from the input terms. A minimal
//! acyclic dictionary has exactly one state per distinct residual language.

use libdictenstein::dynamic_dawg::{
    DynamicDawg, DynamicDawgChar, DynamicDawgCharZipper, DynamicDawgU64, DynamicDawgU64Zipper,
    DynamicDawgZipper,
};
use libdictenstein::{CharUnit, DictionaryTermIterator};
use proptest::prelude::*;
use std::collections::{BTreeMap, BTreeSet};

fn residual_language_count<U: Clone + Ord>(terms: &BTreeSet<Vec<U>>) -> usize {
    let mut prefixes = BTreeSet::from([Vec::new()]);
    for term in terms {
        for end in 0..=term.len() {
            prefixes.insert(term[..end].to_vec());
        }
    }

    prefixes
        .into_iter()
        .map(|prefix| {
            terms
                .iter()
                .filter_map(|term| term.strip_prefix(prefix.as_slice()).map(<[U]>::to_vec))
                .collect::<BTreeSet<_>>()
        })
        .collect::<BTreeSet<_>>()
        .len()
}

fn term() -> impl Strategy<Value = String> {
    prop_oneof![
        Just(String::new()),
        "[a-c]{0,6}",
        "[a-céαβ]{0,5}",
        Just(String::from("a\0")),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    #[test]
    fn sorted_and_unordered_bulk_builders_match_residual_language_oracles(
        input in prop::collection::vec(term(), 0..36),
    ) {
        let mut lexical = input.clone();
        lexical.sort();

        let byte_terms = input.iter().map(|term| term.as_bytes().to_vec()).collect::<BTreeSet<_>>();
        let byte_sorted = DynamicDawg::<()>::from_sorted_terms(&lexical);
        let byte_unordered = DynamicDawg::<()>::from_terms(&input);
        let byte_observed = DictionaryTermIterator::new(DynamicDawgZipper::new_from_dict(&byte_sorted))
            .collect::<BTreeSet<_>>();
        let byte_unordered_observed = DictionaryTermIterator::new(DynamicDawgZipper::new_from_dict(&byte_unordered))
            .collect::<BTreeSet<_>>();
        prop_assert_eq!(&byte_observed, &byte_terms);
        prop_assert_eq!(&byte_unordered_observed, &byte_terms);
        prop_assert_eq!(byte_sorted.term_count(), byte_terms.len());
        prop_assert_eq!(byte_unordered.term_count(), byte_terms.len());
        prop_assert_eq!(byte_sorted.node_count(), residual_language_count(&byte_terms));
        prop_assert_eq!(byte_unordered.node_count(), residual_language_count(&byte_terms));

        let char_terms = input.iter().map(|term| term.chars().collect::<Vec<_>>()).collect::<BTreeSet<_>>();
        let char_sorted = DynamicDawgChar::<()>::from_sorted_terms(&lexical);
        let char_unordered = DynamicDawgChar::<()>::from_terms(&input);
        let char_observed = DictionaryTermIterator::new(DynamicDawgCharZipper::new_from_dict(&char_sorted))
            .collect::<BTreeSet<_>>();
        let char_unordered_observed = DictionaryTermIterator::new(DynamicDawgCharZipper::new_from_dict(&char_unordered))
            .collect::<BTreeSet<_>>();
        prop_assert_eq!(&char_observed, &char_terms);
        prop_assert_eq!(&char_unordered_observed, &char_terms);
        prop_assert_eq!(char_sorted.term_count(), char_terms.len());
        prop_assert_eq!(char_unordered.term_count(), char_terms.len());
        prop_assert_eq!(char_sorted.node_count(), residual_language_count(&char_terms));
        prop_assert_eq!(char_unordered.node_count(), residual_language_count(&char_terms));

        // Packed-u64 string encoding can identify distinct strings (for example
        // "a" and "a\0"), so its oracle is a set of encoded unit sequences.
        let u64_terms = input.iter().map(|term| <u64 as CharUnit>::from_str(term)).collect::<BTreeSet<_>>();
        let mut packed_order = input.clone();
        packed_order.sort_by_key(|term| <u64 as CharUnit>::from_str(term));
        let u64_sorted = DynamicDawgU64::<()>::from_sorted_terms(&packed_order);
        let u64_unordered = DynamicDawgU64::<()>::from_terms(&input);
        let u64_observed = DictionaryTermIterator::new(DynamicDawgU64Zipper::new_from_dict(&u64_sorted))
            .collect::<BTreeSet<_>>();
        let u64_unordered_observed = DictionaryTermIterator::new(DynamicDawgU64Zipper::new_from_dict(&u64_unordered))
            .collect::<BTreeSet<_>>();
        prop_assert_eq!(&u64_observed, &u64_terms);
        prop_assert_eq!(&u64_unordered_observed, &u64_terms);
        prop_assert_eq!(u64_sorted.term_count(), u64_terms.len());
        prop_assert_eq!(u64_unordered.term_count(), u64_terms.len());
        prop_assert_eq!(u64_sorted.node_count(), residual_language_count(&u64_terms));
        prop_assert_eq!(u64_unordered.node_count(), residual_language_count(&u64_terms));
    }

    #[test]
    fn sorted_and_unordered_bulk_builders_keep_the_last_value_for_each_encoded_key(
        input in prop::collection::vec((term(), any::<i16>()), 0..36),
        probe in term(),
    ) {
        let mut lexical = input.clone();
        lexical.sort_by(|left, right| left.0.cmp(&right.0));
        let expected = input.iter().cloned().collect::<BTreeMap<_, _>>();

        let byte_sorted = DynamicDawg::<i16>::from_sorted_terms_with_values(lexical.clone());
        let byte_unordered = DynamicDawg::<i16>::from_terms_with_values(input.clone());
        let char_sorted = DynamicDawgChar::<i16>::from_sorted_terms_with_values(lexical);
        let char_unordered = DynamicDawgChar::<i16>::from_terms_with_values(input.clone());
        prop_assert_eq!(byte_sorted.term_count(), expected.len());
        prop_assert_eq!(byte_unordered.term_count(), expected.len());
        prop_assert_eq!(char_sorted.term_count(), expected.len());
        prop_assert_eq!(char_unordered.term_count(), expected.len());

        for query in input.iter().map(|(term, _)| term).chain(std::iter::once(&probe)) {
            let value = expected.get(query).copied();
            prop_assert_eq!(byte_sorted.get_value(query), value);
            prop_assert_eq!(byte_unordered.get_value(query), value);
            prop_assert_eq!(char_sorted.get_value(query), value);
            prop_assert_eq!(char_unordered.get_value(query), value);
        }

        let mut packed_order = input.clone();
        packed_order.sort_by_key(|(term, _)| <u64 as CharUnit>::from_str(term));
        let packed_expected = input.iter()
            .map(|(term, value)| (<u64 as CharUnit>::from_str(term), *value))
            .collect::<BTreeMap<_, _>>();
        let u64_sorted = DynamicDawgU64::<i16>::from_sorted_terms_with_values(packed_order);
        let u64_unordered = DynamicDawgU64::<i16>::from_terms_with_values(input.clone());
        prop_assert_eq!(u64_sorted.term_count(), packed_expected.len());
        prop_assert_eq!(u64_unordered.term_count(), packed_expected.len());

        for query in input.iter().map(|(term, _)| term).chain(std::iter::once(&probe)) {
            let value = packed_expected.get(&<u64 as CharUnit>::from_str(query)).copied();
            prop_assert_eq!(u64_sorted.get_value(query), value);
            prop_assert_eq!(u64_unordered.get_value(query), value);
        }
    }
}
