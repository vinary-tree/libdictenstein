//! **L3.3 — `::new()` installs an empty (WAL-less) overlay.**
//!
//! Verifies the in-memory `::new()` constructor is overlay-routed (`route_overlay() == true`)
//! — every constructor installs the lock-free overlay, the SOLE representation since L3.3
//! deleted the owned tree — and that WAL-less in-memory writes/reads round-trip through it.
#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::PersistentARTrie;
use libdictenstein::persistent_artrie_char::PersistentARTrieChar;
use libdictenstein::{Dictionary, MappedDictionary};

#[test]
fn byte_new_installs_overlay() {
    #[allow(deprecated)]
    let trie = PersistentARTrie::<u64>::new();
    assert!(
        trie.route_overlay(),
        "L3.3: ::new() must install the overlay (route_overlay == true)"
    );

    assert!(trie.insert_with_value("apple", 1));
    assert!(trie.insert_with_value("application", 2));
    assert!(trie.insert("member")); // term-only
    assert_eq!(MappedDictionary::get_value(&trie, "apple"), Some(1));
    assert_eq!(MappedDictionary::get_value(&trie, "application"), Some(2));
    assert!(Dictionary::contains(&trie, "member"));
    assert!(!Dictionary::contains(&trie, "absent"));
}

#[test]
fn char_new_installs_overlay() {
    let trie = PersistentARTrieChar::<u64>::new();
    assert!(
        trie.route_overlay(),
        "L3.3: char ::new() must install the overlay"
    );

    assert!(trie.insert_with_value("café", 1).expect("ins"));
    assert!(trie.insert_with_value("日本語", 2).expect("ins"));
    assert!(trie.insert("🦀").expect("ins"));
    assert_eq!(MappedDictionary::get_value(&trie, "café"), Some(1));
    assert_eq!(MappedDictionary::get_value(&trie, "日本語"), Some(2));
    assert!(Dictionary::contains(&trie, "🦀"));
}
