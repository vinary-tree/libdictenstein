#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::{DurabilityPolicy, PersistentARTrie};
use libdictenstein::persistent_artrie_char::PersistentARTrieChar;
use libdictenstein::persistent_vocab_artrie::PersistentVocabARTrie;
use tempfile::tempdir;

fn assert_synced_covers_tail(label: &str, next_lsn: u64, synced_lsn: Option<u64>) {
    let tail_lsn = next_lsn.saturating_sub(1);
    assert!(
        tail_lsn > 0,
        "{label}: test expected at least one appended WAL record"
    );
    let synced_lsn = synced_lsn.unwrap_or(0);
    assert!(
        synced_lsn >= tail_lsn,
        "{label}: synced LSN {synced_lsn} must cover acknowledged WAL tail {tail_lsn}"
    );
}

fn assert_synced_before_tail(label: &str, next_lsn: u64, synced_lsn: Option<u64>) {
    let tail_lsn = next_lsn.saturating_sub(1);
    assert!(
        tail_lsn > 0,
        "{label}: test expected at least one appended WAL record"
    );
    let synced_lsn = synced_lsn.unwrap_or(0);
    assert!(
        synced_lsn < tail_lsn,
        "{label}: periodic policy should not claim synced LSN {synced_lsn} covers tail {tail_lsn}"
    );
}

#[test]
fn byte_immediate_public_mutations_ack_only_after_wal_tail_is_synced() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("byte_immediate.part");
    let mut trie: PersistentARTrie<i64> =
        PersistentARTrie::create(&path).expect("create byte trie");

    // **M4b REFRAME.** A fresh `create::<i64>()` now create-flips to the overlay, but
    // this test verifies OWNED-path public-mutation durability (it uses
    // `begin_document` below, which the overlay rejects). Force the owned regime.
    trie.kill_switch_to_owned();

    trie.set_durability_policy(DurabilityPolicy::Immediate);

    assert!(trie.insert_with_value("alpha", 1));
    assert_synced_covers_tail("byte insert", trie.current_lsn(), trie.synced_lsn());

    assert_eq!(trie.increment("alpha", 2).expect("increment"), 3);
    assert_synced_covers_tail("byte increment", trie.current_lsn(), trie.synced_lsn());

    let entries = vec![
        ("beta".to_string(), Some(4)),
        ("gamma".to_string(), Some(5)),
    ];
    assert_eq!(trie.insert_batch(&entries), 2);
    assert_synced_covers_tail("byte batch", trie.current_lsn(), trie.synced_lsn());

    let mut tx = trie.begin_document("doc").expect("begin tx");
    trie.tx_insert(&mut tx, "delta", Some(6));
    assert_eq!(trie.commit_document(tx).expect("commit tx"), 1);
    assert_synced_covers_tail(
        "byte transaction commit",
        trie.current_lsn(),
        trie.synced_lsn(),
    );

    assert!(trie.remove("beta"));
    assert_synced_covers_tail("byte remove", trie.current_lsn(), trie.synced_lsn());
}

#[test]
fn byte_group_commit_policy_public_mutation_ack_is_synced() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("byte_group.part");
    let trie: PersistentARTrie<i64> = PersistentARTrie::create(&path).expect("create byte trie");

    trie.set_durability_policy(DurabilityPolicy::GroupCommit);

    assert!(trie.insert_with_value("alpha", 1));
    assert_synced_covers_tail(
        "byte group-commit insert",
        trie.current_lsn(),
        trie.synced_lsn(),
    );

    trie.sync().expect("blocking sync");
    assert_synced_covers_tail(
        "byte group-commit sync",
        trie.current_lsn(),
        trie.synced_lsn(),
    );
}

#[test]
fn periodic_policy_does_not_overclaim_public_mutation_durability() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("byte_periodic.part");
    let trie: PersistentARTrie<i64> = PersistentARTrie::create(&path).expect("create byte trie");

    // **M4b REFRAME.** A fresh `create::<i64>()` now create-flips to the overlay, but
    // this test verifies OWNED-path durability under the PERIODIC policy (the overlay's
    // Order-A durable write requires a SYNCHRONOUS policy — Immediate/GroupCommit — so
    // a Periodic public write is gated out under the overlay). Force the owned regime.
    trie.kill_switch_to_owned();

    trie.set_durability_policy(DurabilityPolicy::Periodic);

    assert!(trie.insert_with_value("periodic", 7));
    assert_synced_before_tail(
        "byte periodic insert",
        trie.current_lsn(),
        trie.synced_lsn(),
    );
}

#[test]
fn async_sync_handle_completion_covers_target_lsn() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("byte_async.part");
    let trie: PersistentARTrie<i64> = PersistentARTrie::create(&path).expect("create byte trie");

    // **M4b REFRAME.** A fresh `create::<i64>()` now create-flips to the overlay, but
    // this test verifies the OWNED-path async sync-handle under the PERIODIC policy
    // (the overlay's Order-A durable write requires a synchronous policy). Force owned.
    trie.kill_switch_to_owned();

    trie.set_durability_policy(DurabilityPolicy::Periodic);
    assert!(trie.insert_with_value("async-tail", 11));
    let target_lsn = trie.current_lsn().saturating_sub(1);

    let handle = trie
        .sync_async()
        .expect("start async sync")
        .expect("WAL-backed trie returns a sync handle");
    assert_eq!(handle.target_lsn(), target_lsn);
    handle.wait().expect("wait for async sync");

    assert_synced_covers_tail(
        "byte async sync handle",
        trie.current_lsn(),
        trie.synced_lsn(),
    );
}

#[test]
fn char_full_policy_public_mutations_ack_only_after_wal_tail_is_synced() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("char_immediate.part");
    let mut trie: PersistentARTrieChar<i64> =
        PersistentARTrieChar::create(&path).expect("create char trie");

    trie.set_durability_policy(DurabilityPolicy::Immediate);

    assert!(trie.insert_with_value("alpha", 1).expect("insert"));
    assert_synced_covers_tail("char insert", trie.current_lsn(), trie.synced_lsn());

    assert_eq!(trie.increment("alpha", 2).expect("increment"), 3);
    assert_synced_covers_tail("char increment", trie.current_lsn(), trie.synced_lsn());

    assert!(trie.upsert("beta", 4).expect("upsert"));
    assert_synced_covers_tail("char upsert", trie.current_lsn(), trie.synced_lsn());

    assert!(trie.remove("beta").expect("remove"));
    assert_synced_covers_tail("char remove", trie.current_lsn(), trie.synced_lsn());
}

#[test]
fn char_group_commit_policy_without_coordinator_still_waits_for_sync() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("char_group.part");
    let trie: PersistentARTrieChar<i64> =
        PersistentARTrieChar::create(&path).expect("create char trie");

    trie.set_durability_policy(DurabilityPolicy::GroupCommit);

    assert!(trie.insert_with_value("alpha", 1).expect("insert"));
    assert_synced_covers_tail(
        "char group-commit insert",
        trie.current_lsn(),
        trie.synced_lsn(),
    );
}

#[test]
fn vocab_full_policy_public_mutations_ack_only_after_wal_tail_is_synced() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("vocab_immediate.vocab");
    let mut vocab = PersistentVocabARTrie::create(&path).expect("create vocab trie");

    vocab.set_durability_policy(DurabilityPolicy::Immediate);

    assert_eq!(vocab.insert("alpha").expect("insert"), 0);
    assert_synced_covers_tail("vocab insert", vocab.current_lsn(), vocab.synced_lsn());

    let assigned = vocab
        .insert_batch(&["beta", "gamma"])
        .expect("batch insert");
    assert_eq!(assigned, vec![1, 2]);
    assert_synced_covers_tail("vocab batch", vocab.current_lsn(), vocab.synced_lsn());
}

#[test]
fn vocab_group_commit_policy_public_mutation_ack_is_synced() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("vocab_group.vocab");
    let mut vocab = PersistentVocabARTrie::create(&path).expect("create vocab trie");

    vocab.set_durability_policy(DurabilityPolicy::GroupCommit);

    assert_eq!(vocab.insert("alpha").expect("insert"), 0);
    assert_synced_covers_tail(
        "vocab group-commit insert",
        vocab.current_lsn(),
        vocab.synced_lsn(),
    );
}
