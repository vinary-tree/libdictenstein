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

/// **u64 restoration: crossing `i64::MAX` is NO LONGER a spurious reject.** Before
/// the u64 restoration the lock-free counter `try_increment_cas` rejected any count
/// past `i64::MAX` (the old `LOCKFREE_COUNTER_MAX = i64::MAX as u64` bound) — the
/// exact corruption the fix removes. Now the counter is a full `u64` (both byte and
/// char): incrementing PAST `i64::MAX` SUCCEEDS and reads back the true unsigned
/// magnitude (NOT a wrap/negative), and overflow fires ONLY at the genuine `u64::MAX`
/// boundary. (Renamed-in-spirit from the old `*_rejects_signed_persistence_domain_*`
/// to reflect the corrected semantics.)
#[test]
fn checked_increment_cas_rejects_signed_persistence_domain_overflow() {
    let dir = tempdir();
    let byte_path = dir.path().join("byte_checked.part");
    let char_path = dir.path().join("char_checked.artc");
    let i64_max_as_u64 = i64::MAX as u64;

    // BYTE (`V = u64` post-restoration): seed to i64::MAX, then +10 CROSSES the
    // boundary and SUCCEEDS (the old code spuriously rejected this).
    let mut byte_trie = PersistentARTrie::<u64>::create(&byte_path).expect("create byte trie");
    byte_trie.enable_lockfree();
    assert_eq!(
        byte_trie
            .try_increment_cas(b"counter", i64_max_as_u64)
            .expect("increment to i64::MAX"),
        i64_max_as_u64
    );
    assert_eq!(
        byte_trie
            .try_increment_cas(b"counter", 10)
            .expect("crossing i64::MAX must now SUCCEED (u64 restoration)"),
        i64_max_as_u64 + 10,
        "the count past i64::MAX must be the true unsigned magnitude, not a reject/wrap"
    );
    assert_eq!(
        byte_trie.get_lockfree(b"counter"),
        Some(i64_max_as_u64 + 10)
    );
    // A genuine u64 overflow (push to u64::MAX, +1) IS still rejected (not wrapped).
    let mut byte_overflow = PersistentARTrie::<u64>::create(&dir.path().join("byte_of.part"))
        .expect("create byte overflow trie");
    byte_overflow.enable_lockfree();
    byte_overflow
        .try_increment_cas(b"m", u64::MAX)
        .expect("increment to u64::MAX");
    let error = byte_overflow
        .try_increment_cas(b"m", 1)
        .expect_err("crossing u64::MAX must fail");
    assert!(error.to_string().contains("overflow"));
    assert_eq!(byte_overflow.get_lockfree(b"m"), Some(u64::MAX));

    // CHAR (`V = u64`): identical corrected semantics — crossing i64::MAX succeeds,
    // overflow fires only at u64::MAX.
    let mut char_trie = PersistentARTrieChar::<u64>::create(&char_path).expect("create char trie");
    char_trie.enable_lockfree();
    assert_eq!(
        char_trie
            .try_increment_cas("counter", i64_max_as_u64)
            .expect("increment to i64::MAX"),
        i64_max_as_u64
    );
    assert_eq!(
        char_trie
            .try_increment_cas("counter", 10)
            .expect("crossing i64::MAX must now SUCCEED (u64 restoration)"),
        i64_max_as_u64 + 10
    );
    assert_eq!(char_trie.get_lockfree("counter"), Some(i64_max_as_u64 + 10));
    let mut char_overflow = PersistentARTrieChar::<u64>::create(&dir.path().join("char_of.artc"))
        .expect("create char overflow trie");
    char_overflow.enable_lockfree();
    char_overflow
        .try_increment_cas("m", u64::MAX)
        .expect("increment to u64::MAX");
    let error = char_overflow
        .try_increment_cas("m", 1)
        .expect_err("crossing u64::MAX must fail");
    assert!(error.to_string().contains("overflow"));
    assert_eq!(char_overflow.get_lockfree("m"), Some(u64::MAX));
}

#[test]
fn byte_lockfree_value_merge_overflow_is_all_or_nothing() {
    let dir = tempdir();
    let path = dir.path().join("byte_merge_overflow.part");
    let mut trie = PersistentARTrie::<u64>::create(&path).expect("create byte trie");
    // Force the proven owned-tree path (pre-flip behavior) — this test exercises the
    // owned-tree `merge_lockfree_values_to_persistent` drain (rejected under the M4b
    // create-flip overlay) via an EXPLICIT enable_lockfree + try_increment_cas.
    trie.kill_switch_to_owned();

    // u64 restoration: the merge now bounds running sums at `u64::MAX` (not the old
    // i64 domain), so the overflow witness seeds `bad = u64::MAX` and merges a +1
    // delta to cross the genuine u64 boundary (crossing i64::MAX is no longer an
    // overflow — that was the bug the restoration fixes).
    trie.upsert("ok", 10).expect("seed ok");
    trie.upsert("bad", u64::MAX).expect("seed bad");
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
    assert_eq!(trie.get_value("bad"), Some(u64::MAX));
    assert_eq!(trie.get_lockfree(b"ok"), Some(5));
    assert_eq!(trie.get_lockfree(b"bad"), Some(1));
    assert!(batch_increment_terms(&path).is_empty());
}

#[test]
fn char_lockfree_value_merge_overflow_is_all_or_nothing() {
    let dir = tempdir();
    let path = dir.path().join("char_merge_overflow.artc");
    // u64 restoration: char counter overlay is `V = u64` and the merge now bounds
    // running sums at `u64::MAX` (not the old i64 domain), so the overflow witness
    // seeds `bad = u64::MAX` and merges a +1 delta to cross the genuine u64 boundary.
    let mut trie = PersistentARTrieChar::<u64>::create(&path).expect("create char trie");
    // Force the proven owned-tree path (pre-flip behavior) — this test exercises an owned/transaction/merge/archive feature that the create-flip would otherwise route to the lock-free overlay.
    trie.kill_switch_to_owned();

    trie.upsert("ok", 10).expect("seed ok");
    trie.upsert("bad", u64::MAX).expect("seed bad");
    trie.enable_lockfree();
    trie.try_increment_cas("ok", 5).expect("overlay ok");
    trie.try_increment_cas("bad", 1).expect("overlay bad");

    let before_wal = wal_len(&path);
    let error = trie
        .merge_lockfree_values_to_persistent()
        .expect_err("overflowing merge must fail");
    assert!(error.to_string().contains("overflow"));
    assert_eq!(wal_len(&path), before_wal);
    assert_eq!(trie.get("ok"), Some(10));
    assert_eq!(trie.get("bad"), Some(u64::MAX));
    assert_eq!(trie.get_lockfree("ok"), Some(5));
    assert_eq!(trie.get_lockfree("bad"), Some(1));
    assert!(batch_increment_terms(&path).is_empty());
}

#[test]
fn byte_lockfree_value_merge_appends_one_batch_and_reopens_exact_sums() {
    let dir = tempdir();
    let path = dir.path().join("byte_merge_success.part");
    let mut trie = PersistentARTrie::<u64>::create(&path).expect("create byte trie");
    // Force the proven owned-tree path (pre-flip behavior) — this test exercises the
    // owned-tree `merge_lockfree_values_to_persistent` drain (rejected under the M4b
    // create-flip overlay) via an EXPLICIT enable_lockfree + try_increment_cas.
    trie.kill_switch_to_owned();

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

    let reopened = PersistentARTrie::<u64>::open(&path).expect("reopen byte trie");
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
    assert_eq!(trie.get("alpha"), Some(15));
    assert_eq!(trie.get("日本"), Some(7));
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
    assert_eq!(reopened.get("alpha"), Some(15));
    assert_eq!(reopened.get("日本"), Some(7));
}
