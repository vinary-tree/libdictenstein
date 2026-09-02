//! Public-API stack-safety correspondence for durable byte and Unicode tries.
//!
//! Internal codec and overlay tests prove the individual iterative machines.
//! These end-to-end tests bind those proofs to the exported create, mutation,
//! checkpoint, destruction, reopen, lookup, removal, and second-checkpoint
//! lifecycle. The selected depth is an adversarial test witness, not a library
//! limit; callers retain authority over their own resource policy.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::char::PersistentARTrieChar;
use libdictenstein::persistent_artrie::PersistentARTrie;
use libdictenstein::{Dictionary, MappedDictionary};
use tempfile::{Builder, TempDir};

const DEEP_PUBLIC_PATH: usize = 100_000;

fn persistent_test_scratch(prefix: &str) -> TempDir {
    std::fs::create_dir_all("target/test-tmp")
        .expect("create real-disk persistent test scratch root");
    Builder::new()
        .prefix(prefix)
        .tempdir_in("target/test-tmp")
        .expect("create real-disk persistent test scratch directory")
}

#[test]
fn byte_public_checkpoint_reopen_lifecycle_is_stack_safe_at_100k_depth() {
    let directory = persistent_test_scratch("public-byte-deep-");
    let path = directory.path().join("byte-public-deep.artrie");
    let first = "x".repeat(DEEP_PUBLIC_PATH);
    let mut sibling = first.clone().into_bytes();
    sibling[DEEP_PUBLIC_PATH - 1] = b'y';
    let sibling = String::from_utf8(sibling).expect("ASCII sibling is valid UTF-8");

    {
        let trie = PersistentARTrie::<u64>::create(&path).expect("create byte trie");
        assert!(trie.insert_with_value(&first, 11));
        assert!(trie.insert_with_value(&sibling, 13));
        assert_eq!(trie.get_value(&first), Some(11));
        assert_eq!(trie.get_value(&sibling), Some(13));
        trie.checkpoint().expect("checkpoint byte deep paths");
    }

    {
        let trie = PersistentARTrie::<u64>::open(&path).expect("reopen byte trie");
        assert_eq!(trie.get_value(&first), Some(11));
        assert_eq!(trie.get_value(&sibling), Some(13));
        assert!(trie.remove(&first));
        assert!(!trie.contains(&first));
        assert_eq!(trie.get_value(&sibling), Some(13));
        trie.checkpoint()
            .expect("checkpoint byte trie after deep removal");
    }

    let reopened = PersistentARTrie::<u64>::open(&path).expect("reopen byte trie after removal");
    assert!(!reopened.contains(&first));
    assert_eq!(reopened.get_value(&sibling), Some(13));
}

#[test]
fn char_public_checkpoint_reopen_lifecycle_is_stack_safe_at_100k_depth() {
    let directory = persistent_test_scratch("public-char-deep-");
    let path = directory.path().join("char-public-deep.artrie");
    let first = "λ".repeat(DEEP_PUBLIC_PATH);
    let mut sibling: Vec<char> = first.chars().collect();
    sibling[DEEP_PUBLIC_PATH - 1] = '樹';
    let sibling: String = sibling.into_iter().collect();

    {
        let trie = PersistentARTrieChar::<u64>::create(&path).expect("create character trie");
        assert!(trie
            .insert_with_value(&first, 17)
            .expect("insert first character path"));
        assert!(trie
            .insert_with_value(&sibling, 19)
            .expect("insert sibling character path"));
        assert_eq!(trie.get_value(&first), Some(17));
        assert_eq!(trie.get_value(&sibling), Some(19));
        trie.checkpoint().expect("checkpoint character deep paths");
    }

    {
        let trie = PersistentARTrieChar::<u64>::open(&path).expect("reopen character trie");
        assert_eq!(trie.get_value(&first), Some(17));
        assert_eq!(trie.get_value(&sibling), Some(19));
        assert!(trie.remove(&first).expect("remove first character path"));
        assert!(!trie.contains(&first));
        assert_eq!(trie.get_value(&sibling), Some(19));
        trie.checkpoint()
            .expect("checkpoint character trie after deep removal");
    }

    let reopened =
        PersistentARTrieChar::<u64>::open(&path).expect("reopen character trie after removal");
    assert!(!reopened.contains(&first));
    assert_eq!(reopened.get_value(&sibling), Some(19));
}
