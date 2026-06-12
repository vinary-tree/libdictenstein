//! Public-API integration tests for the zero-plumbing PathMap snapshot / ref
//! dictionaries. The in-crate unit tests in `pathmap::snapshot` cover internals;
//! these exercise the crate's *external* surface (the `Dictionary` /
//! `MappedDictionary` contract reached through `libdictenstein::pathmap::…`).
#![cfg(feature = "pathmap-backend")]

use libdictenstein::pathmap::{PathMapDictionary, PathMapRef, PathMapSnapshot};
use libdictenstein::{Dictionary, MappedDictionary};
use pathmap::PathMap;

fn assert_send_sync<T: Send + Sync>() {}

fn map_of(pairs: &[(&str, u32)]) -> PathMap<u32> {
    let mut m = PathMap::new();
    for (t, v) in pairs {
        m.insert(t.as_bytes(), *v);
    }
    m
}

#[test]
fn snapshot_from_dictionary_reports_len_and_is_decoupled() {
    let dict: PathMapDictionary<u32> = PathMapDictionary::new();
    dict.insert_with_value("alpha", 1);

    let snap: PathMapSnapshot<u32> = dict.snapshot();
    assert_eq!(snap.len(), Some(1));
    assert!(snap.contains("alpha"));
    assert_eq!(snap.get_value("alpha"), Some(1));

    // Mutating the source after snapshotting does not affect the snapshot.
    dict.insert_with_value("beta", 2);
    assert!(!snap.contains("beta"));
    assert!(dict.contains("beta"));
}

#[test]
fn borrowed_ref_reads_a_raw_map_zero_copy() {
    let map = map_of(&[("cat", 1), ("car", 2)]);
    let dict = PathMapRef::from_map(&map);
    assert!(dict.contains("cat"));
    assert!(dict.contains("car"));
    assert!(!dict.contains("ca"));
    assert_eq!(dict.get_value("car"), Some(2));
    assert_eq!(dict.get_value("missing"), None);
}

#[test]
fn snapshot_from_map_ref_is_copy_on_write() {
    let mut map = map_of(&[("x", 1)]);
    let snap = PathMapSnapshot::from_map_ref(&map);
    map.insert(b"y", 2); // mutate the original after the 𝒪(1) snapshot
    assert!(snap.contains("x"));
    assert!(!snap.contains("y"));
}

#[test]
fn from_trie_ref_scopes_to_a_subtrie() {
    let map = map_of(&[("appletree", 1), ("applet", 2), ("banana", 3)]);
    // Borrowed subtrie rooted at "apple": remaining keys are "tree" and "t".
    let sub = PathMapRef::from_trie_ref(map.trie_ref_at_path(b"apple"));
    assert!(sub.contains("tree"));
    assert!(sub.contains("t"));
    assert!(!sub.contains("banana"));
}

#[test]
fn snapshot_public_types_keep_send_sync_contract() {
    assert_send_sync::<PathMapSnapshot<u32>>();
    assert_send_sync::<PathMapRef<'static, u32>>();
}
