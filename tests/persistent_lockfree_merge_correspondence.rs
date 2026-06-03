//! Correspondence checks for checked lock-free counter increments and
//! all-or-nothing lock-free value merges.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::{PersistentARTrie, WalReader, WalRecord};
use libdictenstein::persistent_artrie_char::PersistentARTrieChar;
use libdictenstein::MappedDictionary;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

/// Real-disk scratch dir. `/tmp` is tmpfs (RAM) on this host, so disk-backed
/// tries must NOT use bare `tempdir()` (it once filled tmpfs to 39 GB). Always
/// allocate under `target/test-tmp`.
fn tempdir() -> tempfile::TempDir {
    std::fs::create_dir_all("target/test-tmp").ok();
    tempfile::Builder::new()
        .prefix("lockfree-merge")
        .tempdir_in("target/test-tmp")
        .expect("scratch tempdir under target/test-tmp")
}

fn wal_len(path: &Path) -> u64 {
    fs::metadata(path.with_extension("wal"))
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

fn wal_records(path: &Path) -> Vec<WalRecord> {
    WalReader::new(&path.with_extension("wal"))
        .expect("open WAL reader")
        .iter()
        .map(|record| record.expect("read WAL record").1)
        .collect()
}

fn batch_increment_terms(path: &Path) -> Vec<BTreeSet<Vec<u8>>> {
    wal_records(path)
        .into_iter()
        .filter_map(|record| match record {
            WalRecord::BatchIncrement { entries } => {
                Some(entries.into_iter().map(|(term, _)| term).collect())
            }
            _ => None,
        })
        .collect()
}

#[test]
fn checked_increment_cas_rejects_signed_persistence_domain_overflow() {
    let dir = tempdir();
    let byte_path = dir.path().join("byte_checked.part");
    let char_path = dir.path().join("char_checked.artc");
    let max = i64::MAX as u64;

    let mut byte_trie = PersistentARTrie::<i64>::create(&byte_path).expect("create byte trie");
    byte_trie.enable_lockfree();
    assert_eq!(
        byte_trie
            .try_increment_cas(b"counter", max)
            .expect("increment to signed max"),
        max
    );
    let error = byte_trie
        .try_increment_cas(b"counter", 1)
        .expect_err("crossing signed max must fail");
    assert!(error.to_string().contains("overflow"));
    assert_eq!(byte_trie.get_lockfree(b"counter"), Some(max));

    // G1: the char lock-free counter overlay is `V = u64`-specific (the overlay
    // leaf now carries an immutable `Option<u64>` count, not an `AtomicU64` in an
    // `<i64>`-valued trie). The increment domain is still bounded by
    // `LOCKFREE_COUNTER_MAX = i64::MAX as u64`, so `i64::MAX` succeeds and `+1`
    // overflows exactly as before.
    let mut char_trie = PersistentARTrieChar::<u64>::create(&char_path).expect("create char trie");
    char_trie.enable_lockfree();
    assert_eq!(
        char_trie
            .try_increment_cas("counter", max)
            .expect("increment to signed max"),
        max
    );
    let error = char_trie
        .try_increment_cas("counter", 1)
        .expect_err("crossing signed max must fail");
    assert!(error.to_string().contains("overflow"));
    assert_eq!(char_trie.get_lockfree("counter"), Some(max));
}

#[test]
fn byte_lockfree_value_merge_overflow_is_all_or_nothing() {
    let dir = tempdir();
    let path = dir.path().join("byte_merge_overflow.part");
    let mut trie = PersistentARTrie::<i64>::create(&path).expect("create byte trie");

    trie.upsert("ok", 10).expect("seed ok");
    trie.upsert("bad", i64::MAX).expect("seed bad");
    trie.enable_lockfree();
    trie.try_increment_cas(b"ok", 5).expect("overlay ok");
    trie.try_increment_cas(b"bad", 1).expect("overlay bad");

    let before_wal = wal_len(&path);
    let error = trie
        .merge_lockfree_values_to_persistent()
        .expect_err("overflowing merge must fail");
    assert!(error.to_string().contains("overflow"));
    assert_eq!(wal_len(&path), before_wal);
    assert_eq!(trie.get_value("ok"), Some(10));
    assert_eq!(trie.get_value("bad"), Some(i64::MAX));
    assert_eq!(trie.get_lockfree(b"ok"), Some(5));
    assert_eq!(trie.get_lockfree(b"bad"), Some(1));
    assert!(batch_increment_terms(&path).is_empty());
}

#[test]
fn char_lockfree_value_merge_overflow_is_all_or_nothing() {
    let dir = tempdir();
    let path = dir.path().join("char_merge_overflow.artc");
    // G1: char counter overlay is `V = u64`; the merge still bounds running sums
    // at the i64 persistence domain, so seeding `bad = i64::MAX` and merging `+1`
    // overflows that domain identically.
    let mut trie = PersistentARTrieChar::<u64>::create(&path).expect("create char trie");
    // Force the proven owned-tree path (pre-flip behavior) — this test exercises an owned/transaction/merge/archive feature that the create-flip would otherwise route to the lock-free overlay.
    trie.kill_switch_to_owned();

    trie.upsert("ok", 10).expect("seed ok");
    trie.upsert("bad", i64::MAX as u64).expect("seed bad");
    trie.enable_lockfree();
    trie.try_increment_cas("ok", 5).expect("overlay ok");
    trie.try_increment_cas("bad", 1).expect("overlay bad");

    let before_wal = wal_len(&path);
    let error = trie
        .merge_lockfree_values_to_persistent()
        .expect_err("overflowing merge must fail");
    assert!(error.to_string().contains("overflow"));
    assert_eq!(wal_len(&path), before_wal);
    assert_eq!(trie.get("ok").copied(), Some(10));
    assert_eq!(trie.get("bad").copied(), Some(i64::MAX as u64));
    assert_eq!(trie.get_lockfree("ok"), Some(5));
    assert_eq!(trie.get_lockfree("bad"), Some(1));
    assert!(batch_increment_terms(&path).is_empty());
}

#[test]
fn byte_lockfree_value_merge_appends_one_batch_and_reopens_exact_sums() {
    let dir = tempdir();
    let path = dir.path().join("byte_merge_success.part");
    let mut trie = PersistentARTrie::<i64>::create(&path).expect("create byte trie");

    trie.upsert("alpha", 10).expect("seed alpha");
    trie.enable_lockfree();
    trie.try_increment_cas(b"alpha", 5)
        .expect("overlay existing");
    trie.try_increment_cas(b"beta", 7).expect("overlay new");

    assert_eq!(
        trie.merge_lockfree_values_to_persistent()
            .expect("merge lockfree values"),
        2
    );
    assert_eq!(trie.get_value("alpha"), Some(15));
    assert_eq!(trie.get_value("beta"), Some(7));
    assert_eq!(trie.get_lockfree(b"alpha"), None);
    assert_eq!(trie.get_lockfree(b"beta"), None);

    drop(trie);
    let batches = batch_increment_terms(&path);
    assert_eq!(batches.len(), 1);
    assert!(batches
        .last()
        .expect("batch increment")
        .is_superset(&[b"alpha".to_vec(), b"beta".to_vec()].into_iter().collect()));

    let reopened = PersistentARTrie::<i64>::open(&path).expect("reopen byte trie");
    assert_eq!(reopened.get_value("alpha"), Some(15));
    assert_eq!(reopened.get_value("beta"), Some(7));
}

#[test]
fn char_lockfree_value_merge_appends_one_batch_and_reopens_exact_sums() {
    let dir = tempdir();
    let path = dir.path().join("char_merge_success.artc");
    // G1: char counter overlay is `V = u64`.
    let mut trie = PersistentARTrieChar::<u64>::create(&path).expect("create char trie");
    // Force the proven owned-tree path (pre-flip behavior) — this test exercises an owned/transaction/merge/archive feature that the create-flip would otherwise route to the lock-free overlay.
    trie.kill_switch_to_owned();

    trie.upsert("alpha", 10).expect("seed alpha");
    trie.enable_lockfree();
    trie.try_increment_cas("alpha", 5)
        .expect("overlay existing");
    trie.try_increment_cas("日本", 7).expect("overlay unicode");

    assert_eq!(
        trie.merge_lockfree_values_to_persistent()
            .expect("merge lockfree values"),
        2
    );
    assert_eq!(trie.get("alpha").copied(), Some(15));
    assert_eq!(trie.get("日本").copied(), Some(7));
    assert_eq!(trie.get_lockfree("alpha"), None);
    assert_eq!(trie.get_lockfree("日本"), None);

    drop(trie);
    let batches = batch_increment_terms(&path);
    assert_eq!(batches.len(), 1);
    assert!(batches.last().expect("batch increment").is_superset(
        &[b"alpha".to_vec(), "日本".as_bytes().to_vec()]
            .into_iter()
            .collect()
    ));

    let reopened = PersistentARTrieChar::<u64>::open(&path).expect("reopen char trie");
    assert_eq!(reopened.get("alpha").copied(), Some(15));
    assert_eq!(reopened.get("日本").copied(), Some(7));
}
