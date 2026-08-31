//! Regression tests for `PersistentVocabARTrie` / `SharedVocabARTrie`
//! `DictionaryNode` traversal — the libgrammstein "returns childless nodes" bug.
//!
//! Before the fix, `VocabTrieNodeRef` was a one-level SNAPSHOT node whose
//! `transition` / `edges` stamped every returned child with an EMPTY children
//! vector (it held no handle to the trie, so it could not fetch a child's
//! children). Any generic `DictionaryNode` walk — a liblevenshtein transducer's
//! `edges()` frontier expansion, a libgrammstein DFS — therefore died at depth 1:
//! only the root exposed children, so every term of length >= 2 was unreachable.
//!
//! The fix makes `VocabTrieNodeRef` an alias of the shared, `Arc`-holding
//! `OverlayDictionaryNode<CharKey, u64>` (the same handle the byte/char tries
//! use), which navigates the lock-free overlay lazily and descends the FULL depth
//! of the trie.
//!
//! These tests drive ONLY the `Dictionary` / `DictionaryNode` / `MappedDictionaryNode`
//! trait surface — NOT vocab's inherent overlay walk, which always worked and is
//! exactly why the bug was invisible to vocab's own tests (both `Dictionary` impls
//! override `contains` to delegate to that inherent walk). Every property is
//! checked for BOTH the bare `PersistentVocabARTrie` and the `Arc`-shared
//! `SharedVocabARTrie` handle.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::vocab::{PersistentVocabARTrie, SharedVocabARTrie};
use libdictenstein::{Dictionary, DictionaryNode, MappedDictionaryNode};
use std::collections::BTreeSet;
use std::sync::Arc;
use tempfile::tempdir;

/// The inserted corpus: the empty string (root-final), single chars, shared
/// prefixes that fan out (`ab`/`abc`, `apple`/`apply`), a multi-byte Latin term
/// (`café`), and a multi-byte CJK term (`日本語`) — every one at length >= 2 was
/// unreachable before the fix. Each term carries an explicit vocabulary index so
/// `value()` can be checked.
fn corpus() -> Vec<(&'static str, u64)> {
    vec![
        ("", 0),
        ("a", 1),
        ("ab", 2),
        ("abc", 3),
        ("apple", 4),
        ("apply", 5),
        ("banana", 6),
        ("café", 7),
        ("日本語", 8),
    ]
}

fn corpus_terms() -> BTreeSet<String> {
    corpus().into_iter().map(|(t, _)| t.to_string()).collect()
}

/// Build a fresh persistent vocab at `path` populated with `corpus()`.
fn build(path: &std::path::Path) -> PersistentVocabARTrie {
    let vocab = PersistentVocabARTrie::create(path).expect("create vocab");
    for (term, index) in corpus() {
        vocab
            .insert_with_index(term, index)
            .unwrap_or_else(|error| panic!("insert_with_index({term:?}, {index}): {error}"));
    }
    vocab
}

/// Generic depth-first walk of the `DictionaryNode` graph — the SAME shape a
/// transducer's frontier expansion drives (`edges()` re-expanded per node).
/// Collects the path of every final node. Generic over `N` to prove the fix works
/// for ANY `DictionaryNode<Unit = char>` walker, not merely a concrete vocab
/// helper — which is precisely the external surface the bug broke.
fn collect_terms<N: DictionaryNode<Unit = char>>(
    node: &N,
    prefix: &mut String,
    out: &mut BTreeSet<String>,
) {
    let initial_prefix_len = prefix.len();
    let mut pending = vec![(node.clone(), initial_prefix_len, None)];

    while let Some((node, parent_prefix_len, incoming)) = pending.pop() {
        prefix.truncate(parent_prefix_len);
        if let Some(ch) = incoming {
            prefix.push(ch);
        }

        if node.is_final() {
            out.insert(prefix.clone());
        }

        let child_prefix_len = prefix.len();
        let mut children: Vec<_> = node.edges().collect();
        while let Some((ch, child)) = children.pop() {
            pending.push((child, child_prefix_len, Some(ch)));
        }
    }

    prefix.truncate(initial_prefix_len);
}

/// Walk from `root` to the node at `term` using ONLY repeated `transition(char)`
/// (one overlay edge per `char` — the un-path-compressed overlay), returning the
/// terminal node, or `None` if any step dead-ends.
fn walk<N: DictionaryNode<Unit = char>>(root: N, term: &str) -> Option<N> {
    let mut node = root;
    for ch in term.chars() {
        node = node.transition(ch)?;
    }
    Some(node)
}

/// Reproduces the DEFAULT `Dictionary::contains` — which vocab OVERRIDES with its
/// inherent overlay walk — by driving the fixed `transition` chain, so it
/// exercises exactly the multi-level descent the bug broke.
fn trait_contains<N: DictionaryNode<Unit = char>>(root: N, term: &str) -> bool {
    match walk(root, term) {
        Some(node) => node.is_final(),
        None => false,
    }
}

/// Bare handle: the `DictionaryNode` graph descends the full depth of the trie.
#[test]
fn persistent_vocab_dictionary_node_descends_full_depth() {
    let dir = tempdir().expect("tempdir");
    let vocab = build(&dir.path().join("vocab.dict"));

    // (regression) the exact reported failure: root -> 'a' -> 'b' must reach a
    // real, final node carrying its index (previously `.transition('b')` on the
    // childless depth-1 node returned `None`).
    let ab = walk(Dictionary::root(&vocab), "ab")
        .expect("root().transition('a').transition('b') dead-ended (childless-node bug)");
    assert!(ab.is_final(), "'ab' is a member, so its node must be final");
    assert_eq!(ab.value(), Some(2), "'ab' must carry its vocabulary index");

    // Deep multi-byte descent: every `char` step must resolve.
    let cafe =
        walk(Dictionary::root(&vocab), "café").expect("multi-byte 'café' path lost after fix");
    assert!(cafe.is_final());
    assert_eq!(cafe.value(), Some(7));
    let cjk =
        walk(Dictionary::root(&vocab), "日本語").expect("multi-byte '日本語' path lost after fix");
    assert!(cjk.is_final());
    assert_eq!(cjk.value(), Some(8));

    // A generic DFS over `edges()` must enumerate EVERY inserted term (depth >= 2
    // reachable) — the property that was truncated to length-1 terms.
    let mut got = BTreeSet::new();
    collect_terms(&Dictionary::root(&vocab), &mut String::new(), &mut got);
    assert_eq!(
        got,
        corpus_terms(),
        "generic DictionaryNode DFS != inserted term set"
    );

    // Empty-string membership => the root itself is final (was hardcoded `false`),
    // and its `value()` matches the inherent index.
    let root = Dictionary::root(&vocab);
    assert!(
        root.is_final(),
        "empty string is a member, so root must be final"
    );
    assert_eq!(
        root.value(),
        vocab.get_index(""),
        "root value() must equal the inherent get_index(\"\")"
    );

    // A prefix that is not itself a member is reached but non-final.
    let ap = walk(Dictionary::root(&vocab), "ap").expect("'ap' is a real prefix path");
    assert!(!ap.is_final(), "'ap' is a prefix, not an inserted member");

    // No fabricated edges for an absent path (no spurious transitions).
    assert!(
        walk(Dictionary::root(&vocab), "az").is_none(),
        "spurious transition for absent edge 'a'->'z'"
    );
    assert!(
        Dictionary::root(&vocab).transition('z').is_none(),
        "spurious transition for absent root edge 'z'"
    );
}

/// Bare handle: the default-`contains` contract (driven through `transition`)
/// agrees with vocab's inherent `contains` for members and non-members alike.
#[test]
fn persistent_vocab_trait_contains_matches_inherent() {
    let dir = tempdir().expect("tempdir");
    let vocab = build(&dir.path().join("vocab.dict"));

    for (term, _) in corpus() {
        assert!(
            trait_contains(Dictionary::root(&vocab), term),
            "trait-walk contains({term:?}) should be true for a member"
        );
        assert!(
            vocab.contains(term),
            "inherent contains({term:?}) should be true for a member"
        );
    }

    for absent in ["z", "az", "abcd", "appl", "ba", "caf", "日本", "日本語X"] {
        assert!(
            !trait_contains(Dictionary::root(&vocab), absent),
            "trait-walk contains({absent:?}) should be false"
        );
        assert_eq!(
            vocab.contains(absent),
            trait_contains(Dictionary::root(&vocab), absent),
            "trait vs inherent contains disagree for {absent:?}"
        );
    }
}

/// Shared (`Arc`) handle: same full-depth descent through the `Dictionary` impl on
/// `SharedVocabARTrie`. The returned node owns the overlay-root `Arc`, so it
/// outlives the transient no-lock `read()` shim guard.
#[test]
fn shared_vocab_dictionary_node_descends_full_depth() {
    let dir = tempdir().expect("tempdir");
    let shared: SharedVocabARTrie = Arc::new(build(&dir.path().join("vocab.dict")));

    let ab = walk(<SharedVocabARTrie as Dictionary>::root(&shared), "ab")
        .expect("shared root().transition('a').transition('b') dead-ended");
    assert!(ab.is_final());
    assert_eq!(ab.value(), Some(2));

    let cjk = walk(<SharedVocabARTrie as Dictionary>::root(&shared), "日本語")
        .expect("shared multi-byte '日本語' path lost");
    assert!(cjk.is_final());
    assert_eq!(cjk.value(), Some(8));

    let mut got = BTreeSet::new();
    collect_terms(
        &<SharedVocabARTrie as Dictionary>::root(&shared),
        &mut String::new(),
        &mut got,
    );
    assert_eq!(
        got,
        corpus_terms(),
        "generic DictionaryNode DFS via shared handle != inserted term set"
    );

    assert!(
        <SharedVocabARTrie as Dictionary>::root(&shared).is_final(),
        "empty-string root must be final via the shared handle"
    );
}

/// The trait-driven DFS agrees with vocab's inherent `iter_terms()` oracle — the
/// always-correct overlay enumeration path. Compared on non-empty terms so the
/// result is independent of whether `iter_terms` emits the empty string; the full
/// set (including `""`) is separately pinned to the inserted corpus.
#[test]
fn dictionary_node_walk_matches_inherent_iter_terms_oracle() {
    let dir = tempdir().expect("tempdir");
    let vocab = build(&dir.path().join("vocab.dict"));

    let mut walked = BTreeSet::new();
    collect_terms(&Dictionary::root(&vocab), &mut String::new(), &mut walked);

    let oracle: BTreeSet<String> = vocab.iter_terms().collect();

    let walked_nonempty: BTreeSet<String> =
        walked.iter().filter(|t| !t.is_empty()).cloned().collect();
    let oracle_nonempty: BTreeSet<String> =
        oracle.iter().filter(|t| !t.is_empty()).cloned().collect();

    assert_eq!(
        walked_nonempty, oracle_nonempty,
        "trait DFS != inherent iter_terms oracle (non-empty terms)"
    );
    assert_eq!(
        walked,
        corpus_terms(),
        "trait DFS != inserted corpus (full set, incl. empty string)"
    );
}
