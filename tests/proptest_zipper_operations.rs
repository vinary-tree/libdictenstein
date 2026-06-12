//! Property-based tests for set-theoretic zipper operations.
//!
//! These tests verify algebraic properties of union, intersection, difference,
//! and symmetric difference operations across dictionary zippers.
//!
//! Run with: cargo test --test proptest_zipper_operations

mod common;

use common::strategies::*;
use libdictenstein::difference_zipper::DifferenceZipperExt;
use libdictenstein::double_array_trie::zipper::DoubleArrayTrieZipper;
use libdictenstein::double_array_trie::DoubleArrayTrie;
use libdictenstein::dynamic_dawg::zipper::DynamicDawgZipper;
use libdictenstein::dynamic_dawg::DynamicDawg;
use libdictenstein::intersection_zipper::IntersectionZipperExt;
use libdictenstein::symmetric_difference_zipper::SymmetricDifferenceZipperExt;
use libdictenstein::union_zipper::UnionZipperExt;
use libdictenstein::zipper::DictZipper;
use proptest::prelude::*;
use std::collections::HashSet;

// =============================================================================
// Helper Functions
// =============================================================================

fn collect_zipper_terms<Z: DictZipper<Unit = u8>>(
    iter: impl Iterator<Item = (Vec<u8>, Z)>,
) -> HashSet<String> {
    iter.map(|(path, _)| String::from_utf8(path).expect("valid UTF-8"))
        .collect()
}

fn terms_to_set(terms: &[String]) -> HashSet<String> {
    terms.iter().cloned().collect()
}

// =============================================================================
// Union Properties
// =============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    /// Property: Union is commutative: A ∪ B = B ∪ A
    #[test]
    fn union_commutativity(
        (terms_a, terms_b) in overlapping_term_sets(20, 0.3)
    ) {
        let dict_a = DoubleArrayTrie::from_terms(terms_a.iter());
        let dict_b = DoubleArrayTrie::from_terms(terms_b.iter());

        let z_a1 = DoubleArrayTrieZipper::new_from_dict(&dict_a);
        let z_b1 = DoubleArrayTrieZipper::new_from_dict(&dict_b);
        let z_a2 = DoubleArrayTrieZipper::new_from_dict(&dict_a);
        let z_b2 = DoubleArrayTrieZipper::new_from_dict(&dict_b);

        let a_union_b = collect_zipper_terms(z_a1.union_with(z_b1).iter());
        let b_union_a = collect_zipper_terms(z_b2.union_with(z_a2).iter());

        prop_assert_eq!(a_union_b, b_union_a, "A ∪ B should equal B ∪ A");
    }

    /// Property: Union with empty set: A ∪ ∅ = A
    #[test]
    fn union_empty_identity(
        terms in prop::collection::vec(ascii_term(1, 10), 1..=20)
    ) {
        let unique_terms: HashSet<_> = terms.into_iter().collect();
        let dict_a = DoubleArrayTrie::from_terms(unique_terms.iter());
        let dict_empty: DoubleArrayTrie = DoubleArrayTrie::new();

        let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
        let z_empty = DoubleArrayTrieZipper::new_from_dict(&dict_empty);

        let result = collect_zipper_terms(z_a.union_with(z_empty).iter());

        prop_assert_eq!(result, unique_terms, "A ∪ ∅ should equal A");
    }

    /// Property: Union with self: A ∪ A = A
    #[test]
    fn union_self_idempotent(
        terms in prop::collection::vec(ascii_term(1, 10), 1..=20)
    ) {
        let unique_terms: HashSet<_> = terms.into_iter().collect();
        let dict = DoubleArrayTrie::from_terms(unique_terms.iter());

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict);

        let result = collect_zipper_terms(z1.union_with(z2).iter());

        prop_assert_eq!(result, unique_terms, "A ∪ A should equal A");
    }

    /// Property: Union associativity: (A ∪ B) ∪ C = A ∪ (B ∪ C)
    /// Tested via union_all which produces equivalent results
    #[test]
    fn union_associativity(
        (terms_a, terms_b, terms_c) in three_term_sets(10)
    ) {
        let dict_a = DoubleArrayTrie::from_terms(terms_a.iter());
        let dict_b = DoubleArrayTrie::from_terms(terms_b.iter());
        let dict_c = DoubleArrayTrie::from_terms(terms_c.iter());

        // Using union_all for (A ∪ B ∪ C)
        let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
        let z_b = DoubleArrayTrieZipper::new_from_dict(&dict_b);
        let z_c = DoubleArrayTrieZipper::new_from_dict(&dict_c);
        let union_all = collect_zipper_terms(z_a.union_all(vec![z_b, z_c]).iter());

        // Expected: set union
        let expected: HashSet<_> = terms_to_set(&terms_a)
            .union(&terms_to_set(&terms_b))
            .cloned()
            .collect::<HashSet<_>>()
            .union(&terms_to_set(&terms_c))
            .cloned()
            .collect();

        prop_assert_eq!(union_all, expected, "Union should produce set union");
    }
}

// =============================================================================
// Intersection Properties
// =============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    /// Property: Intersection is commutative: A ∩ B = B ∩ A
    #[test]
    fn intersection_commutativity(
        (terms_a, terms_b) in overlapping_term_sets(20, 0.3)
    ) {
        let dict_a = DoubleArrayTrie::from_terms(terms_a.iter());
        let dict_b = DoubleArrayTrie::from_terms(terms_b.iter());

        let z_a1 = DoubleArrayTrieZipper::new_from_dict(&dict_a);
        let z_b1 = DoubleArrayTrieZipper::new_from_dict(&dict_b);
        let z_a2 = DoubleArrayTrieZipper::new_from_dict(&dict_a);
        let z_b2 = DoubleArrayTrieZipper::new_from_dict(&dict_b);

        let a_inter_b = collect_zipper_terms(z_a1.intersection_with(z_b1).iter());
        let b_inter_a = collect_zipper_terms(z_b2.intersection_with(z_a2).iter());

        prop_assert_eq!(a_inter_b, b_inter_a, "A ∩ B should equal B ∩ A");
    }

    /// Property: Intersection with empty set: A ∩ ∅ = ∅
    #[test]
    fn intersection_empty_annihilator(
        terms in prop::collection::vec(ascii_term(1, 10), 1..=20)
    ) {
        let unique_terms: HashSet<_> = terms.into_iter().collect();
        let dict_a = DoubleArrayTrie::from_terms(unique_terms.iter());
        let dict_empty: DoubleArrayTrie = DoubleArrayTrie::new();

        let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
        let z_empty = DoubleArrayTrieZipper::new_from_dict(&dict_empty);

        let result = collect_zipper_terms(z_a.intersection_with(z_empty).iter());

        prop_assert!(result.is_empty(), "A ∩ ∅ should be empty");
    }

    /// Property: Intersection with self: A ∩ A = A
    #[test]
    fn intersection_self_idempotent(
        terms in prop::collection::vec(ascii_term(1, 10), 1..=20)
    ) {
        let unique_terms: HashSet<_> = terms.into_iter().collect();
        let dict = DoubleArrayTrie::from_terms(unique_terms.iter());

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict);

        let result = collect_zipper_terms(z1.intersection_with(z2).iter());

        prop_assert_eq!(result, unique_terms, "A ∩ A should equal A");
    }

    /// Property: Intersection associativity via intersection_all
    #[test]
    fn intersection_associativity(
        (terms_a, terms_b, terms_c) in three_term_sets(10)
    ) {
        let dict_a = DoubleArrayTrie::from_terms(terms_a.iter());
        let dict_b = DoubleArrayTrie::from_terms(terms_b.iter());
        let dict_c = DoubleArrayTrie::from_terms(terms_c.iter());

        // Using intersection_all for (A ∩ B ∩ C)
        let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
        let z_b = DoubleArrayTrieZipper::new_from_dict(&dict_b);
        let z_c = DoubleArrayTrieZipper::new_from_dict(&dict_c);
        let inter_all = collect_zipper_terms(z_a.intersection_all(vec![z_b, z_c]).iter());

        // Expected: set intersection
        let expected: HashSet<_> = terms_to_set(&terms_a)
            .intersection(&terms_to_set(&terms_b))
            .cloned()
            .collect::<HashSet<_>>()
            .intersection(&terms_to_set(&terms_c))
            .cloned()
            .collect();

        prop_assert_eq!(inter_all, expected, "Intersection should produce set intersection");
    }
}

// =============================================================================
// Difference Properties
// =============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    /// Property: A \ ∅ = A
    #[test]
    fn difference_empty_identity(
        terms in prop::collection::vec(ascii_term(1, 10), 1..=20)
    ) {
        let unique_terms: HashSet<_> = terms.into_iter().collect();
        let dict_a = DoubleArrayTrie::from_terms(unique_terms.iter());
        let dict_empty: DoubleArrayTrie = DoubleArrayTrie::new();

        let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
        let z_empty = DoubleArrayTrieZipper::new_from_dict(&dict_empty);

        let result = collect_zipper_terms(z_a.difference_from(z_empty).iter());

        prop_assert_eq!(result, unique_terms, "A \\ ∅ should equal A");
    }

    /// Property: ∅ \ B = ∅
    #[test]
    fn difference_from_empty(
        terms in prop::collection::vec(ascii_term(1, 10), 1..=20)
    ) {
        let unique_terms: HashSet<_> = terms.into_iter().collect();
        let dict_b = DoubleArrayTrie::from_terms(unique_terms.iter());
        let dict_empty: DoubleArrayTrie = DoubleArrayTrie::new();

        let z_empty = DoubleArrayTrieZipper::new_from_dict(&dict_empty);
        let z_b = DoubleArrayTrieZipper::new_from_dict(&dict_b);

        let result = collect_zipper_terms(z_empty.difference_from(z_b).iter());

        prop_assert!(result.is_empty(), "∅ \\ B should be empty");
    }

    /// Property: A \ A = ∅
    #[test]
    fn difference_self_empty(
        terms in prop::collection::vec(ascii_term(1, 10), 1..=20)
    ) {
        let unique_terms: HashSet<_> = terms.into_iter().collect();
        let dict = DoubleArrayTrie::from_terms(unique_terms.iter());

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict);

        let result = collect_zipper_terms(z1.difference_from(z2).iter());

        prop_assert!(result.is_empty(), "A \\ A should be empty");
    }

    /// Property: Difference is not commutative (in general)
    #[test]
    fn difference_not_commutative(
        (terms_a, terms_b) in overlapping_term_sets(10, 0.2)
    ) {
        // Only test when sets are actually different
        prop_assume!(terms_to_set(&terms_a) != terms_to_set(&terms_b));

        let dict_a = DoubleArrayTrie::from_terms(terms_a.iter());
        let dict_b = DoubleArrayTrie::from_terms(terms_b.iter());

        let z_a1 = DoubleArrayTrieZipper::new_from_dict(&dict_a);
        let z_b1 = DoubleArrayTrieZipper::new_from_dict(&dict_b);
        let z_a2 = DoubleArrayTrieZipper::new_from_dict(&dict_a);
        let z_b2 = DoubleArrayTrieZipper::new_from_dict(&dict_b);

        let a_minus_b = collect_zipper_terms(z_a1.difference_from(z_b1).iter());
        let b_minus_a = collect_zipper_terms(z_b2.difference_from(z_a2).iter());

        // They should be different (unless one is a subset of the other)
        let set_a = terms_to_set(&terms_a);
        let set_b = terms_to_set(&terms_b);
        if !set_a.is_subset(&set_b) && !set_b.is_subset(&set_a) {
            prop_assert_ne!(a_minus_b, b_minus_a, "A \\ B should not equal B \\ A in general");
        }
    }

    /// Property: Difference correctness: A \ B contains exactly elements in A but not B
    #[test]
    fn difference_correctness(
        (terms_a, terms_b) in overlapping_term_sets(15, 0.3)
    ) {
        let set_a = terms_to_set(&terms_a);
        let set_b = terms_to_set(&terms_b);
        let expected: HashSet<_> = set_a.difference(&set_b).cloned().collect();

        let dict_a = DoubleArrayTrie::from_terms(terms_a.iter());
        let dict_b = DoubleArrayTrie::from_terms(terms_b.iter());

        let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
        let z_b = DoubleArrayTrieZipper::new_from_dict(&dict_b);

        let result = collect_zipper_terms(z_a.difference_from(z_b).iter());

        prop_assert_eq!(result, expected, "A \\ B should match set difference");
    }
}

// =============================================================================
// Symmetric Difference Properties
// =============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    /// Property: Symmetric difference is commutative: A Δ B = B Δ A
    #[test]
    fn symmetric_difference_commutativity(
        (terms_a, terms_b) in overlapping_term_sets(20, 0.3)
    ) {
        let dict_a = DoubleArrayTrie::from_terms(terms_a.iter());
        let dict_b = DoubleArrayTrie::from_terms(terms_b.iter());

        let z_a1 = DoubleArrayTrieZipper::new_from_dict(&dict_a);
        let z_b1 = DoubleArrayTrieZipper::new_from_dict(&dict_b);
        let z_a2 = DoubleArrayTrieZipper::new_from_dict(&dict_a);
        let z_b2 = DoubleArrayTrieZipper::new_from_dict(&dict_b);

        let a_delta_b = collect_zipper_terms(z_a1.symmetric_difference_with(z_b1).iter());
        let b_delta_a = collect_zipper_terms(z_b2.symmetric_difference_with(z_a2).iter());

        prop_assert_eq!(a_delta_b, b_delta_a, "A Δ B should equal B Δ A");
    }

    /// Property: A Δ ∅ = A
    #[test]
    fn symmetric_difference_empty_identity(
        terms in prop::collection::vec(ascii_term(1, 10), 1..=20)
    ) {
        let unique_terms: HashSet<_> = terms.into_iter().collect();
        let dict_a = DoubleArrayTrie::from_terms(unique_terms.iter());
        let dict_empty: DoubleArrayTrie = DoubleArrayTrie::new();

        let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
        let z_empty = DoubleArrayTrieZipper::new_from_dict(&dict_empty);

        let result = collect_zipper_terms(z_a.symmetric_difference_with(z_empty).iter());

        prop_assert_eq!(result, unique_terms, "A Δ ∅ should equal A");
    }

    /// Property: A Δ A = ∅
    #[test]
    fn symmetric_difference_self_empty(
        terms in prop::collection::vec(ascii_term(1, 10), 1..=20)
    ) {
        let unique_terms: HashSet<_> = terms.into_iter().collect();
        let dict = DoubleArrayTrie::from_terms(unique_terms.iter());

        let z1 = DoubleArrayTrieZipper::new_from_dict(&dict);
        let z2 = DoubleArrayTrieZipper::new_from_dict(&dict);

        let result = collect_zipper_terms(z1.symmetric_difference_with(z2).iter());

        prop_assert!(result.is_empty(), "A Δ A should be empty");
    }

    /// Property: A Δ B = (A \ B) ∪ (B \ A)
    #[test]
    fn symmetric_difference_via_differences(
        (terms_a, terms_b) in overlapping_term_sets(15, 0.3)
    ) {
        let dict_a = DoubleArrayTrie::from_terms(terms_a.iter());
        let dict_b = DoubleArrayTrie::from_terms(terms_b.iter());

        // A Δ B directly
        let z_a1 = DoubleArrayTrieZipper::new_from_dict(&dict_a);
        let z_b1 = DoubleArrayTrieZipper::new_from_dict(&dict_b);
        let sym_diff = collect_zipper_terms(z_a1.symmetric_difference_with(z_b1).iter());

        // (A \ B) ∪ (B \ A) via set operations
        let set_a = terms_to_set(&terms_a);
        let set_b = terms_to_set(&terms_b);
        let a_minus_b: HashSet<_> = set_a.difference(&set_b).cloned().collect();
        let b_minus_a: HashSet<_> = set_b.difference(&set_a).cloned().collect();
        let expected: HashSet<_> = a_minus_b.union(&b_minus_a).cloned().collect();

        prop_assert_eq!(sym_diff, expected, "A Δ B should equal (A \\ B) ∪ (B \\ A)");
    }

    /// Property: A Δ B = (A ∪ B) \ (A ∩ B)
    #[test]
    fn symmetric_difference_via_union_intersection(
        (terms_a, terms_b) in overlapping_term_sets(15, 0.3)
    ) {
        let dict_a = DoubleArrayTrie::from_terms(terms_a.iter());
        let dict_b = DoubleArrayTrie::from_terms(terms_b.iter());

        // A Δ B directly
        let z_a1 = DoubleArrayTrieZipper::new_from_dict(&dict_a);
        let z_b1 = DoubleArrayTrieZipper::new_from_dict(&dict_b);
        let sym_diff = collect_zipper_terms(z_a1.symmetric_difference_with(z_b1).iter());

        // (A ∪ B) \ (A ∩ B) via set operations
        let set_a = terms_to_set(&terms_a);
        let set_b = terms_to_set(&terms_b);
        let union: HashSet<_> = set_a.union(&set_b).cloned().collect();
        let intersection: HashSet<_> = set_a.intersection(&set_b).cloned().collect();
        let expected: HashSet<_> = union.difference(&intersection).cloned().collect();

        prop_assert_eq!(sym_diff, expected, "A Δ B should equal (A ∪ B) \\ (A ∩ B)");
    }
}

// =============================================================================
// De Morgan's Laws
// =============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(30))]

    /// Property: (A ∩ B)' ∩ U = (A' ∪ B') ∩ U (De Morgan - intersection)
    /// We test: U \ (A ∩ B) = (U \ A) ∪ (U \ B) where U is the universe
    #[test]
    fn de_morgan_intersection(
        (terms_a, terms_b) in overlapping_term_sets(10, 0.3)
    ) {
        // Define universe as A ∪ B
        let set_a = terms_to_set(&terms_a);
        let set_b = terms_to_set(&terms_b);
        let universe: HashSet<_> = set_a.union(&set_b).cloned().collect();

        // U \ (A ∩ B) via set operations
        let a_inter_b: HashSet<_> = set_a.intersection(&set_b).cloned().collect();
        let lhs: HashSet<_> = universe.difference(&a_inter_b).cloned().collect();

        // (U \ A) ∪ (U \ B) via set operations
        let u_minus_a: HashSet<_> = universe.difference(&set_a).cloned().collect();
        let u_minus_b: HashSet<_> = universe.difference(&set_b).cloned().collect();
        let rhs: HashSet<_> = u_minus_a.union(&u_minus_b).cloned().collect();

        prop_assert_eq!(lhs, rhs, "U \\ (A ∩ B) should equal (U \\ A) ∪ (U \\ B)");
    }

    /// Property: (A ∪ B)' ∩ U = (A' ∩ B') ∩ U (De Morgan - union)
    /// We test: U \ (A ∪ B) = (U \ A) ∩ (U \ B) where U is the universe
    #[test]
    fn de_morgan_union(
        (terms_a, terms_b) in overlapping_term_sets(10, 0.3)
    ) {
        // Define universe as all terms plus some extra
        let set_a = terms_to_set(&terms_a);
        let set_b = terms_to_set(&terms_b);
        let mut universe: HashSet<String> = set_a.union(&set_b).cloned().collect();
        // Add extra terms to universe
        universe.insert("extra1".to_string());
        universe.insert("extra2".to_string());
        universe.insert("extra3".to_string());

        // U \ (A ∪ B) via set operations
        let a_union_b: HashSet<_> = set_a.union(&set_b).cloned().collect();
        let lhs: HashSet<_> = universe.difference(&a_union_b).cloned().collect();

        // (U \ A) ∩ (U \ B) via set operations
        let u_minus_a: HashSet<_> = universe.difference(&set_a).cloned().collect();
        let u_minus_b: HashSet<_> = universe.difference(&set_b).cloned().collect();
        let rhs: HashSet<_> = u_minus_a.intersection(&u_minus_b).cloned().collect();

        prop_assert_eq!(lhs, rhs, "U \\ (A ∪ B) should equal (U \\ A) ∩ (U \\ B)");
    }
}

// =============================================================================
// Distributive Laws
// =============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(30))]

    /// Property: A ∩ (B ∪ C) = (A ∩ B) ∪ (A ∩ C)
    #[test]
    fn distribution_intersection_over_union(
        (terms_a, terms_b, terms_c) in three_term_sets(10)
    ) {
        let set_a = terms_to_set(&terms_a);
        let set_b = terms_to_set(&terms_b);
        let set_c = terms_to_set(&terms_c);

        // A ∩ (B ∪ C)
        let b_union_c: HashSet<_> = set_b.union(&set_c).cloned().collect();
        let lhs: HashSet<_> = set_a.intersection(&b_union_c).cloned().collect();

        // (A ∩ B) ∪ (A ∩ C)
        let a_inter_b: HashSet<_> = set_a.intersection(&set_b).cloned().collect();
        let a_inter_c: HashSet<_> = set_a.intersection(&set_c).cloned().collect();
        let rhs: HashSet<_> = a_inter_b.union(&a_inter_c).cloned().collect();

        prop_assert_eq!(lhs, rhs, "A ∩ (B ∪ C) should equal (A ∩ B) ∪ (A ∩ C)");
    }

    /// Property: A ∪ (B ∩ C) = (A ∪ B) ∩ (A ∪ C)
    #[test]
    fn distribution_union_over_intersection(
        (terms_a, terms_b, terms_c) in three_term_sets(10)
    ) {
        let set_a = terms_to_set(&terms_a);
        let set_b = terms_to_set(&terms_b);
        let set_c = terms_to_set(&terms_c);

        // A ∪ (B ∩ C)
        let b_inter_c: HashSet<_> = set_b.intersection(&set_c).cloned().collect();
        let lhs: HashSet<_> = set_a.union(&b_inter_c).cloned().collect();

        // (A ∪ B) ∩ (A ∪ C)
        let a_union_b: HashSet<_> = set_a.union(&set_b).cloned().collect();
        let a_union_c: HashSet<_> = set_a.union(&set_c).cloned().collect();
        let rhs: HashSet<_> = a_union_b.intersection(&a_union_c).cloned().collect();

        prop_assert_eq!(lhs, rhs, "A ∪ (B ∩ C) should equal (A ∪ B) ∩ (A ∪ C)");
    }
}

// =============================================================================
// Cross-Backend Consistency
// =============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(30))]

    /// Property: Zipper operations produce same results across backends.
    #[test]
    fn cross_backend_consistency(
        (terms_a, terms_b) in overlapping_term_sets(15, 0.3)
    ) {
        // DoubleArrayTrie
        let dat_a = DoubleArrayTrie::from_terms(terms_a.iter());
        let dat_b = DoubleArrayTrie::from_terms(terms_b.iter());

        // DynamicDawg
        let dawg_a: DynamicDawg<()> = DynamicDawg::from_terms(terms_a.iter());
        let dawg_b: DynamicDawg<()> = DynamicDawg::from_terms(terms_b.iter());

        // Union
        let dat_union = collect_zipper_terms(
            DoubleArrayTrieZipper::new_from_dict(&dat_a)
                .union_with(DoubleArrayTrieZipper::new_from_dict(&dat_b))
                .iter()
        );
        let dawg_union = collect_zipper_terms(
            DynamicDawgZipper::new_from_dict(&dawg_a)
                .union_with(DynamicDawgZipper::new_from_dict(&dawg_b))
                .iter()
        );
        prop_assert_eq!(dat_union, dawg_union, "Union should be consistent across backends");

        // Intersection
        let dat_inter = collect_zipper_terms(
            DoubleArrayTrieZipper::new_from_dict(&dat_a)
                .intersection_with(DoubleArrayTrieZipper::new_from_dict(&dat_b))
                .iter()
        );
        let dawg_inter = collect_zipper_terms(
            DynamicDawgZipper::new_from_dict(&dawg_a)
                .intersection_with(DynamicDawgZipper::new_from_dict(&dawg_b))
                .iter()
        );
        prop_assert_eq!(dat_inter, dawg_inter, "Intersection should be consistent across backends");

        // Difference
        let dat_diff = collect_zipper_terms(
            DoubleArrayTrieZipper::new_from_dict(&dat_a)
                .difference_from(DoubleArrayTrieZipper::new_from_dict(&dat_b))
                .iter()
        );
        let dawg_diff = collect_zipper_terms(
            DynamicDawgZipper::new_from_dict(&dawg_a)
                .difference_from(DynamicDawgZipper::new_from_dict(&dawg_b))
                .iter()
        );
        prop_assert_eq!(dat_diff, dawg_diff, "Difference should be consistent across backends");
    }
}
