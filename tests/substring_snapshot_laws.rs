use libdictenstein::scdawg::char::ScdawgChar;
use libdictenstein::scdawg::Scdawg;
use libdictenstein::{Dictionary, SubstringDictionary};
use std::collections::BTreeSet;

fn terms<N: libdictenstein::DictionaryNode>(
    matches: Vec<libdictenstein::SubstringMatch<N>>,
) -> BTreeSet<String> {
    matches.into_iter().map(|matched| matched.term).collect()
}

fn assert_borrowed_cursor_matches_empty_search<D: SubstringDictionary>(snapshot: &D::Node) {
    let eager: Vec<_> = D::find_exact_substring_in_snapshot(snapshot, "")
        .into_iter()
        .map(|matched| matched.term)
        .collect();
    let mut cursor = 0;
    let borrowed: Vec<_> = std::iter::from_fn(|| {
        D::next_complete_term_in_snapshot(snapshot, &mut cursor).map(str::to_owned)
    })
    .collect();
    assert_eq!(borrowed, eager);
    assert_eq!(
        D::next_complete_term_in_snapshot(snapshot, &mut cursor),
        None
    );
}

#[test]
fn in_memory_byte_scdawg_searches_one_retained_revision() {
    let dictionary = Scdawg::<()>::from_terms(["abcd"]);
    let snapshot = dictionary.root();
    assert!(dictionary.insert("abxd"));

    assert_borrowed_cursor_matches_empty_search::<Scdawg<()>>(&snapshot);
    assert_borrowed_cursor_matches_empty_search::<Scdawg<()>>(&dictionary.root());

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

    assert_borrowed_cursor_matches_empty_search::<ScdawgChar<()>>(&snapshot);
    assert_borrowed_cursor_matches_empty_search::<ScdawgChar<()>>(&dictionary.root());

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
    use libdictenstein::MutableDictionary;

    fn assert_removed_term_is_snapshot_local<D: SubstringDictionary + MutableDictionary>(
        dictionary: &D,
    ) {
        let before = dictionary.root();
        assert!(dictionary.remove("middle"));
        let after = dictionary.root();

        let mut before_cursor = 0;
        let before_terms: Vec<_> = std::iter::from_fn(|| {
            D::next_complete_term_in_snapshot(&before, &mut before_cursor).map(str::to_owned)
        })
        .collect();
        let mut after_cursor = 0;
        let after_terms: Vec<_> = std::iter::from_fn(|| {
            D::next_complete_term_in_snapshot(&after, &mut after_cursor).map(str::to_owned)
        })
        .collect();

        assert_eq!(before_terms, ["first", "middle", "last"]);
        assert_eq!(after_terms, ["first", "last"]);
        assert_borrowed_cursor_matches_empty_search::<D>(&before);
        assert_borrowed_cursor_matches_empty_search::<D>(&after);
    }

    #[test]
    fn borrowed_cursors_skip_tombstones_in_all_persistent_backends() {
        assert_removed_term_is_snapshot_local(&PersistentScdawg::<()>::from_terms([
            "first", "middle", "last",
        ]));
        assert_removed_term_is_snapshot_local(&PersistentScdawgChar::<()>::from_terms([
            "first", "middle", "last",
        ]));
        assert_removed_term_is_snapshot_local(&PersistentSuffixTree::<()>::from_texts([
            "first", "middle", "last",
        ]));
        assert_removed_term_is_snapshot_local(&PersistentSuffixTreeChar::<()>::from_texts([
            "first", "middle", "last",
        ]));
    }

    #[test]
    fn byte_scdawg_searches_one_retained_revision() {
        let dictionary = PersistentScdawg::<()>::from_terms(["abcd"]);
        let snapshot = dictionary.root();
        assert!(dictionary.insert("abxd"));

        assert_borrowed_cursor_matches_empty_search::<PersistentScdawg<()>>(&snapshot);
        assert_borrowed_cursor_matches_empty_search::<PersistentScdawg<()>>(&dictionary.root());

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

        assert_borrowed_cursor_matches_empty_search::<PersistentScdawgChar<()>>(&snapshot);
        assert_borrowed_cursor_matches_empty_search::<PersistentScdawgChar<()>>(&dictionary.root());

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

        assert_borrowed_cursor_matches_empty_search::<PersistentSuffixTree<()>>(&snapshot);
        assert_borrowed_cursor_matches_empty_search::<PersistentSuffixTree<()>>(&dictionary.root());

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

        assert_borrowed_cursor_matches_empty_search::<PersistentSuffixTreeChar<()>>(&snapshot);
        assert_borrowed_cursor_matches_empty_search::<PersistentSuffixTreeChar<()>>(
            &dictionary.root(),
        );

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
