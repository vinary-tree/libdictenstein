//! Regression: under the lock-free overlay default, the legacy char `get`/`try_get`
//! readers silently returned `None` for present terms (they short-circuited to
//! `Ok(None)` under `route_overlay()`), and `update_or_insert` then OVERWROTE existing
//! values with the default. libgrammstein's n-gram counts were lost via `get`.
//!
//! Fix: `get`/`try_get` route to the overlay via the canonical `get_value`
//! (→ `overlay_route_get_value`, the shared `LockFreeOverlay` driver: i64/u64 counter,
//! `()` membership, arbitrary `V`); `update_or_insert` reads via `get_value`.
//!
//! These tests pin `get == try_get == get_value` for every value shape, live AND after
//! `checkpoint() -> open_with_recovery()` — exactly libgrammstein's pattern. Scratch is
//! real disk under `target/test-tmp` (never tmpfs/tempdir).
//!
//! Scope: BYTE uses `get_value_bytes` (already overlay-routed; no `get`/`try_get`) and
//! VOCAB has no overlay regime (reads live `self.root`) — so this bug is char-only and
//! there are deliberately no byte/vocab analogues here.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie_char::PersistentARTrieChar;
use libdictenstein::MutableMappedDictionary;

fn scratch(prefix: &str) -> tempfile::TempDir {
    std::fs::create_dir_all("target/test-tmp").ok();
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in("target/test-tmp")
        .expect("scratch tempdir under target/test-tmp")
}

/// The reported bug: an `<i64>` char counter incremented + checkpoint()'d, reopened via
/// `open_with_recovery`, read via `get` — was `None`, must be the count.
#[test]
fn char_i64_counter_get_routes_to_overlay_live_and_after_recovery() {
    let dir = scratch("char-get-i64");
    let path = dir.path().join("test.artrie");
    {
        let mut trie = PersistentARTrieChar::<i64>::create(&path).expect("create");
        trie.increment("the|quick", 1).expect("inc1");
        trie.increment("the|quick", 1).expect("inc2");
        trie.increment("quick|brown", 1).expect("inc3");
        // get == try_get == get_value, all live under the overlay.
        assert_eq!(trie.get("the|quick"), Some(2), "get live");
        assert_eq!(
            trie.try_get("the|quick").expect("try_get"),
            Some(2),
            "try_get live"
        );
        assert_eq!(trie.get_value("the|quick"), Some(2), "get_value live");
        assert_eq!(trie.get("nonexistent"), None);
        trie.checkpoint()
            .expect("checkpoint (= libgrammstein sync())");
    }
    let (trie, _report) =
        PersistentARTrieChar::<i64>::open_with_recovery(&path).expect("open_with_recovery");
    assert_eq!(trie.get("the|quick"), Some(2), "get after recovery");
    assert_eq!(trie.try_get("quick|brown").expect("try_get"), Some(1));
    assert_eq!(trie.get_value("the|quick"), Some(2));
    assert_eq!(trie.get("nonexistent"), None);
}

/// The CounterValue(u64) arm above i64::MAX — `get`/`try_get` must return the exact
/// unsigned magnitude, not None or a truncation.
#[test]
fn char_u64_counter_get_above_i64max() {
    let dir = scratch("char-get-u64");
    let path = dir.path().join("test.artrie");
    let big: u64 = (u64::MAX / 2) + 1 + 6; // i64::MAX + 7
    {
        let mut trie = PersistentARTrieChar::<u64>::create(&path).expect("create");
        trie.upsert("k", big - 9).expect("seed");
        for _ in 0..9 {
            trie.increment("k", 1).expect("inc");
        }
        assert_eq!(trie.get("k"), Some(big), "get u64 > i64::MAX live");
        assert_eq!(trie.try_get("k").expect("try_get"), Some(big));
        assert_eq!(trie.get_value("k"), Some(big));
        trie.checkpoint().expect("checkpoint");
    }
    let (trie, _r) = PersistentARTrieChar::<u64>::open_with_recovery(&path).expect("reopen");
    assert_eq!(
        trie.get("k"),
        Some(big),
        "get u64 > i64::MAX after recovery"
    );
}

/// The membership `()` arm — `get` returns `Some(())` for a member, `None` otherwise.
#[test]
fn char_unit_membership_get_routes_to_overlay() {
    let dir = scratch("char-get-unit");
    let path = dir.path().join("test.artrie");
    {
        let trie = PersistentARTrieChar::<()>::create(&path).expect("create");
        trie.insert("alpha").expect("insert");
        trie.insert("beta").expect("insert");
        assert_eq!(trie.get("alpha"), Some(()), "membership get live");
        assert_eq!(trie.try_get("beta").expect("try_get"), Some(()));
        assert_eq!(trie.get("absent"), None);
        trie.checkpoint().expect("checkpoint");
    }
    let (trie, _r) = PersistentARTrieChar::<()>::open_with_recovery(&path).expect("reopen");
    assert_eq!(trie.get("alpha"), Some(()), "membership get after recovery");
    assert_eq!(trie.get("absent"), None);
}

/// The arbitrary-`V` arm (`overlay_value_get`) — a `String` value read via `get`.
#[test]
fn char_string_value_get_routes_to_overlay() {
    let dir = scratch("char-get-string");
    let path = dir.path().join("test.artrie");
    {
        let trie = PersistentARTrieChar::<String>::create(&path).expect("create");
        trie.upsert("key", "value".to_string()).expect("upsert");
        assert_eq!(
            trie.get("key"),
            Some("value".to_string()),
            "string get live"
        );
        assert_eq!(
            trie.try_get("key").expect("try_get"),
            Some("value".to_string())
        );
        assert_eq!(trie.get_value("key"), Some("value".to_string()));
        assert_eq!(trie.get("absent"), None);
        trie.checkpoint().expect("checkpoint");
    }
    let (trie, _r) = PersistentARTrieChar::<String>::open_with_recovery(&path).expect("reopen");
    assert_eq!(
        trie.get("key"),
        Some("value".to_string()),
        "string get after recovery"
    );
}

/// `update_or_insert` must READ the existing (overlay) value — not clobber it with the
/// default. Pre-fix it read `None` and overwrote the existing value with `default`.
#[test]
fn char_update_or_insert_preserves_existing_overlay_value() {
    use libdictenstein::artrie_trait::ARTrie;
    use libdictenstein::persistent_artrie_char::SharedCharARTrie;
    let dir = scratch("char-update-or-insert");
    let path = dir.path().join("test.artrie");
    let trie: SharedCharARTrie<i64> = ARTrie::create(&path).expect("create");

    // Seed an existing value, then update_or_insert: must UPDATE (not overwrite with the
    // default), and report the term already existed.
    assert!(
        trie.update_or_insert("k", 10, |v| *v += 100),
        "first update_or_insert inserts the default (10) and returns true (new)"
    );
    assert_eq!(trie.get_value("k"), Some(10), "inserted default");

    let was_new = trie.update_or_insert("k", 999, |v| *v += 1);
    assert!(
        !was_new,
        "second update_or_insert on an existing term returns false"
    );
    assert_eq!(
        trie.get_value("k"),
        Some(11),
        "MUST update the existing value (10 -> 11), NOT clobber with the default (999)"
    );

    // A genuinely new term still inserts the default.
    assert!(
        trie.update_or_insert("new", 5, |v| *v += 1),
        "new term inserts default"
    );
    assert_eq!(trie.get_value("new"), Some(5));
}
