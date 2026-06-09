//! **L3.2 — `::new()` installs an empty overlay (WAL-less) + the kill-switch guard.**
//!
//! Verifies (a) the in-memory `::new()` constructor is now overlay-routed
//! (`route_overlay() == true`) — the precondition for deleting the owned tree at L3.3 — and
//! (b) the data-loss guard: `kill_switch_to_owned()` on a WAL-less `::new()` trie is a NO-OP
//! (the overlay stays engaged), because such a trie writes to the overlay and has an EMPTY
//! owned tree, so flipping it to Owned would route reads to nothing = silent total loss.
#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::PersistentARTrie;
use libdictenstein::persistent_artrie_char::PersistentARTrieChar;
use libdictenstein::{Dictionary, MappedDictionary};

#[test]
fn byte_new_installs_overlay_and_kill_switch_is_guarded_noop() {
    #[allow(deprecated)]
    let trie = PersistentARTrie::<u64>::new();
    assert!(
        trie.route_overlay(),
        "L3.2: ::new() must install the overlay (route_overlay == true)"
    );

    assert!(trie.insert_with_value("apple", 1));
    assert!(trie.insert_with_value("application", 2));
    assert!(trie.insert("member")); // term-only
    assert_eq!(MappedDictionary::get_value(&trie, "apple"), Some(1));
    assert_eq!(MappedDictionary::get_value(&trie, "application"), Some(2));
    assert!(Dictionary::contains(&trie, "member"));
    assert!(!Dictionary::contains(&trie, "absent"));

    // A WAL-less ::new() trie has an EMPTY owned tree; kill-switch MUST be a guarded no-op.
    trie.kill_switch_to_owned();
    assert!(
        trie.route_overlay(),
        "L3.2: kill-switch must NOT disengage the overlay on a WAL-less ::new() trie"
    );
    assert_eq!(
        MappedDictionary::get_value(&trie, "apple"),
        Some(1),
        "data survives the guarded kill-switch (no silent loss)"
    );
    assert!(Dictionary::contains(&trie, "member"));
}

#[test]
fn char_new_installs_overlay_and_kill_switch_is_guarded_noop() {
    let trie = PersistentARTrieChar::<u64>::new();
    assert!(
        trie.route_overlay(),
        "L3.2: char ::new() must install the overlay"
    );

    assert!(trie.insert_with_value("café", 1).expect("ins"));
    assert!(trie.insert_with_value("日本語", 2).expect("ins"));
    assert!(trie.insert("🦀").expect("ins"));
    assert_eq!(MappedDictionary::get_value(&trie, "café"), Some(1));
    assert_eq!(MappedDictionary::get_value(&trie, "日本語"), Some(2));
    assert!(Dictionary::contains(&trie, "🦀"));

    trie.kill_switch_to_owned();
    assert!(
        trie.route_overlay(),
        "L3.2: char kill-switch must be a guarded no-op on a WAL-less trie"
    );
    assert_eq!(
        MappedDictionary::get_value(&trie, "café"),
        Some(1),
        "char data survives the guarded kill-switch"
    );
    assert!(Dictionary::contains(&trie, "🦀"));
}
