//! **Byte overlay deep-key read smoke test.**
//!
//! The owned-vs-overlay correspondence + reestablish-from-owned round-trip suite that
//! once lived here was retired with the owned tree (L3.3): the overlay is now the SOLE
//! representation, so an "overlay vs owned" comparison is vacuous. What remains is the
//! one overlay-only property worth pinning in-crate — the overlay is NOT path-compressed
//! (one node per byte), so a length-N key is an N-deep spine and the read-engine DFS
//! walks must not overflow the stack at depth 500.
//!
//! Scratch lives on real disk (`target/test-tmp`), never `/tmp` (tmpfs on this host).

#![cfg(test)]

use std::collections::BTreeSet;

use crate::persistent_artrie::PersistentARTrie;

fn scratch(prefix: &str) -> tempfile::TempDir {
    std::fs::create_dir_all("target/test-tmp").ok();
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in("target/test-tmp")
        .expect("scratch tempdir under target/test-tmp")
}

/// The overlay is NOT path-compressed (one node per byte), so a length-N key is
/// an N-deep overlay spine; the enumerators recurse by depth. A length-500 key
/// must not overflow the stack in `overlay_len`/`overlay_get_value`/
/// `overlay_iter_prefix`.
#[test]
fn deep_key_overlay_reads_no_stack_overflow() {
    let dir = scratch("byte-deep-key");
    let path = dir.path().join("deep.part");

    let deep: Vec<u8> = vec![b'a'; 500];

    let overlay = PersistentARTrie::<u64>::create(&path).expect("create");
    assert!(overlay.route_overlay(), "create installs the overlay");
    overlay.increment_cas(&deep, 11);

    // Count / value walks at depth 500 must not overflow.
    assert_eq!(overlay.overlay_len(), 1);
    assert_eq!(overlay.overlay_get_value(&deep), Some(Some(11)));

    // Whole-tree enumeration (the deepest DFS) at depth 500 must not overflow.
    let all: BTreeSet<Vec<u8>> = overlay
        .overlay_iter_prefix(b"")
        .expect("overlay_iter_prefix")
        .into_iter()
        .collect();
    assert_eq!(all.len(), 1);
    assert!(all.contains(&deep));

    // A deep prefix walk (navigate 499 levels, then collect).
    let prefix: Vec<u8> = vec![b'a'; 499];
    let under = overlay
        .overlay_iter_prefix(&prefix)
        .expect("overlay_iter_prefix deep");
    assert_eq!(under, vec![deep.clone()], "deep prefix walk");
}
