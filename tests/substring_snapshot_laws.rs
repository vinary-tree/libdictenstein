use libdictenstein::scdawg::char::ScdawgChar;
use libdictenstein::scdawg::Scdawg;
use libdictenstein::{Dictionary, SubstringDictionary};
use std::collections::BTreeSet;

fn terms<N: libdictenstein::DictionaryNode>(
    matches: Vec<libdictenstein::SubstringMatch<N>>,
) -> BTreeSet<String> {
    matches.into_iter().map(|matched| matched.term).collect()
}

#[test]
fn in_memory_byte_scdawg_searches_one_retained_revision() {
    let dictionary = Scdawg::<()>::from_terms(["abcd"]);
    let snapshot = dictionary.root();
    assert!(dictionary.insert("abxd"));

    assert_eq!(
        terms(Scdawg::find_exact_substring_in_snapshot(&snapshot, "ab")),
        BTreeSet::from(["abcd".to_owned()])
    );
    assert_eq!(
        terms(dictionary.find_exact_substring("ab")),
        BTreeSet::from(["abcd".to_owned(), "abxd".to_owned()])
    );
}

#[test]
fn in_memory_unicode_scdawg_searches_one_retained_revision() {
    let dictionary = ScdawgChar::<()>::from_terms(["café"]);
    let snapshot = dictionary.root();
    assert!(dictionary.insert("camp"));

    assert_eq!(
        terms(ScdawgChar::find_exact_substring_in_snapshot(
            &snapshot, "ca"
        )),
        BTreeSet::from(["café".to_owned()])
    );
    assert_eq!(
        terms(dictionary.find_exact_substring("ca")),
        BTreeSet::from(["café".to_owned(), "camp".to_owned()])
    );
}

#[cfg(feature = "persistent-artrie")]
mod persistent {
    use super::*;
    use libdictenstein::persistent_artrie::scdawg::{PersistentScdawg, PersistentScdawgChar};
    use libdictenstein::persistent_artrie::suffix_tree::{
        PersistentSuffixTree, PersistentSuffixTreeChar,
    };

    #[test]
    fn byte_scdawg_searches_one_retained_revision() {
        let dictionary = PersistentScdawg::<()>::from_terms(["abcd"]);
        let snapshot = dictionary.root();
        assert!(dictionary.insert("abxd"));

        assert_eq!(
            terms(
                <PersistentScdawg<()> as SubstringDictionary>::find_exact_substring_in_snapshot(
                    &snapshot, "ab",
                ),
            ),
            BTreeSet::from(["abcd".to_owned()])
        );
        assert_eq!(
            terms(dictionary.find_exact_substring("ab")),
            BTreeSet::from(["abcd".to_owned(), "abxd".to_owned()])
        );
    }

    #[test]
    fn unicode_scdawg_searches_one_retained_revision() {
        let dictionary = PersistentScdawgChar::<()>::from_terms(["café"]);
        let snapshot = dictionary.root();
        assert!(dictionary.insert("camp"));

        assert_eq!(
            terms(
                <PersistentScdawgChar<()> as SubstringDictionary>::find_exact_substring_in_snapshot(
                    &snapshot, "ca",
                ),
            ),
            BTreeSet::from(["café".to_owned()])
        );
        assert_eq!(
            terms(dictionary.find_exact_substring("ca")),
            BTreeSet::from(["café".to_owned(), "camp".to_owned()])
        );
    }

    #[test]
    fn byte_suffix_tree_searches_one_retained_revision() {
        let dictionary = PersistentSuffixTree::<()>::from_text("abcd");
        let snapshot = dictionary.root();
        assert!(dictionary.insert("abxd"));

        assert_eq!(
            terms(
                <PersistentSuffixTree<()> as SubstringDictionary>::find_exact_substring_in_snapshot(
                    &snapshot, "ab",
                ),
            ),
            BTreeSet::from(["abcd".to_owned()])
        );
        assert_eq!(
            terms(dictionary.find_exact_substring("ab")),
            BTreeSet::from(["abcd".to_owned(), "abxd".to_owned()])
        );
    }

    #[test]
    fn unicode_suffix_tree_searches_one_retained_revision() {
        let dictionary = PersistentSuffixTreeChar::<()>::from_text("café");
        let snapshot = dictionary.root();
        assert!(dictionary.insert("camp"));

        assert_eq!(
            terms(
                <PersistentSuffixTreeChar<()> as SubstringDictionary>::find_exact_substring_in_snapshot(
                    &snapshot, "ca",
                ),
            ),
            BTreeSet::from(["café".to_owned()])
        );
        assert_eq!(
            terms(dictionary.find_exact_substring("ca")),
            BTreeSet::from(["café".to_owned(), "camp".to_owned()])
        );
    }
}
