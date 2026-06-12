#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::{PersistentScdawg, PersistentScdawgChar};
use libdictenstein::scdawg::{Scdawg, ScdawgChar};
use libdictenstein::{Dictionary, MappedDictionary, SubstringDictionary, SyncStrategy};
use proptest::prelude::*;
use tempfile::tempdir;

fn sorted_locations(mut locations: Vec<(String, usize)>) -> Vec<(String, usize)> {
    locations.sort();
    locations
}

fn assert_byte_parity(terms: Vec<String>, probes: Vec<String>) {
    let volatile = Scdawg::<()>::from_terms(terms.iter());
    let persistent = PersistentScdawg::<()>::from_terms(terms.iter());

    assert_eq!(persistent.len(), volatile.len());
    assert_eq!(persistent.sync_strategy(), SyncStrategy::InternalSync);
    assert!(persistent.is_suffix_based());

    for probe in probes {
        assert_eq!(
            persistent.contains(&probe),
            volatile.contains(&probe),
            "exact contains mismatch for {probe:?}"
        );
        assert_eq!(
            persistent.contains_substring(&probe),
            volatile.contains_substring(&probe),
            "substring contains mismatch for {probe:?}"
        );
        assert_eq!(
            persistent.freq(&probe),
            volatile.freq(&probe),
            "freq mismatch for {probe:?}"
        );
        assert_eq!(
            sorted_locations(persistent.locations(&probe)),
            sorted_locations(volatile.locations(&probe)),
            "locations mismatch for {probe:?}"
        );
        assert_eq!(
            sorted_locations(
                persistent
                    .find_exact_substring(&probe)
                    .into_iter()
                    .map(|m| (m.term, m.position))
                    .collect()
            ),
            sorted_locations(
                volatile
                    .find_exact_substring(&probe)
                    .into_iter()
                    .map(|m| (m.term, m.position))
                    .collect()
            ),
            "substring match mismatch for {probe:?}"
        );
    }
}

#[test]
fn byte_persistent_scdawg_matches_volatile_scdawg_contract() {
    assert_byte_parity(
        vec![
            "banana".to_string(),
            "bandana".to_string(),
            "cabana".to_string(),
        ],
        vec![
            "".to_string(),
            "banana".to_string(),
            "ana".to_string(),
            "ban".to_string(),
            "dana".to_string(),
            "xyz".to_string(),
        ],
    );
}

#[test]
fn byte_values_duplicates_removal_compaction_and_reopen() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("persistent_scdawg_byte.artrie");

    {
        let dict = PersistentScdawg::<i32>::create(&path).expect("create persistent scdawg");
        assert!(dict.insert_with_value("banana", 7));
        assert!(!dict.insert_with_value("banana", 11));
        assert_eq!(dict.get_value("banana"), Some(11));
        assert!(dict.insert("bandana"));
        assert!(dict.contains_substring("ana"));
        assert!(dict.contains("banana"));
        assert!(!dict.contains("ana"));

        let handle = dict.find("ana").expect("find ana");
        assert_eq!(dict.freq_at(&handle), dict.freq("ana"));
        assert_eq!(
            sorted_locations(dict.locations_at(&handle, 3)),
            sorted_locations(dict.locations("ana"))
        );

        assert!(dict.remove("banana"));
        assert!(!dict.contains("banana"));
        assert!(dict.contains_substring("dana"));
        assert!(dict.needs_compaction());
        dict.compact();
        assert!(!dict.needs_compaction());
        assert_eq!(dict.source_texts(), vec!["bandana".to_string()]);
        dict.checkpoint().expect("checkpoint");
        dict.close();
    }

    let reopened = PersistentScdawg::<i32>::open(&path).expect("reopen persistent scdawg");
    assert_eq!(reopened.term_count(), 1);
    assert!(reopened.contains("bandana"));
    assert!(!reopened.contains("banana"));
    assert!(reopened.contains_substring("dana"));
}

#[test]
fn char_persistent_scdawg_matches_unicode_positions() {
    let volatile = ScdawgChar::<()>::from_terms(["café 日本", "naïve café"]);
    let persistent = PersistentScdawgChar::<()>::from_terms(["café 日本", "naïve café"]);

    for probe in ["", "café", "fé 日", "日本", "ïve", "missing"] {
        assert_eq!(persistent.contains(probe), volatile.contains(probe));
        assert_eq!(
            persistent.contains_substring(probe),
            volatile.contains_substring(probe)
        );
        assert_eq!(persistent.freq(probe), volatile.freq(probe));
        assert_eq!(
            sorted_locations(persistent.locations(probe)),
            sorted_locations(volatile.locations(probe)),
            "char locations mismatch for {probe:?}"
        );
    }

    let matches = persistent.find_exact_substring("fé");
    assert!(matches
        .iter()
        .any(|m| m.term == "café 日本" && m.position == 2 && m.length == 2));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn byte_persistent_scdawg_property_parity(
        terms in prop::collection::vec("[abc]{0,5}", 0..8),
        probes in prop::collection::vec("[abc]{0,4}", 0..8),
    ) {
        assert_byte_parity(terms, probes);
    }
}
