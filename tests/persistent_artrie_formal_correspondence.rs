//! Executable correspondence checks for the formal verification models.
//!
//! These tests keep the CI surface small while checking the Rust behavior that
//! the Rocq and TLA+ models rely on: sorted buckets, binary-search partitions,
//! split/merge preservation, transactional visibility, WAL fail-closed reads,
//! end-to-end crash-prefix recovery, version-GC reader protection, and
//! group-commit LSN correspondence.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::nodes::PersistentNode;
use libdictenstein::persistent_artrie::wal::WalError;
use libdictenstein::persistent_artrie::{
    AsyncWalConfig, BucketError, MmapDiskManager, PendingSegment, PersistentARTrie,
    SegmentSyncManager, StringBucket, WalConfig, WalHeader, WalReader, WalRecord, WalRecordType,
    WalSyncBackend, WalWriter,
};
use libdictenstein::persistent_artrie_char::nodes::PersistentCharNode;
use libdictenstein::persistent_artrie_char::{CharTrieNodeInner, PersistentARTrieCharNode};
use libdictenstein::persistent_artrie_core::concurrency::OptimisticCell;
use libdictenstein::persistent_artrie_core::mvcc::ReadTransaction;
use libdictenstein::persistent_vocab_artrie::{
    ConcurrentVocabARTrie, LockFreeVocab, NodeRef, PersistentVocabARTrie, VocabTrieNode,
};
use libdictenstein::serialization::bincode_compat;
use libdictenstein::{Dictionary, MappedDictionary};
use proptest::prelude::*;
use rand::{rngs::StdRng, Rng, SeedableRng};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

fn byte_key_strategy() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(0u8..=127, 0..=8)
}

fn byte_value_strategy() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(0u8..=127, 1..=8)
}

fn bucket_entry_strategy() -> impl Strategy<Value = (Vec<u8>, Vec<u8>)> {
    (byte_key_strategy(), byte_value_strategy())
}

fn bucket_entries(bucket: &StringBucket) -> Vec<(Vec<u8>, Option<Vec<u8>>)> {
    (0..bucket.len())
        .map(|index| {
            let entry = bucket.get_entry(index).expect("valid bucket index");
            (
                bucket.get_suffix(&entry).to_vec(),
                bucket.get_value(&entry).map(|value| value.to_vec()),
            )
        })
        .collect()
}

fn build_bucket(entries: BTreeMap<Vec<u8>, Vec<u8>>) -> (StringBucket, Vec<Vec<u8>>) {
    let mut bucket = StringBucket::with_values();

    for (key, value) in &entries {
        bucket.insert(key, value).expect("test bucket has capacity");
    }

    let sorted_keys = entries.keys().cloned().collect();
    (bucket, sorted_keys)
}

fn assert_search_partition(bucket: &StringBucket, sorted_keys: &[Vec<u8>], probe: &[u8]) {
    match bucket.search(probe) {
        Ok(index) => {
            assert!(index < sorted_keys.len());
            assert_eq!(sorted_keys[index].as_slice(), probe);
        }
        Err(index) => {
            assert!(index <= sorted_keys.len());
            assert!(sorted_keys[..index]
                .iter()
                .all(|key| key.as_slice() < probe));
            assert!(sorted_keys[index..]
                .iter()
                .all(|key| key.as_slice() > probe));
        }
    }
}

struct FailingSyncBackend {
    attempts: Arc<AtomicUsize>,
}

impl WalSyncBackend for FailingSyncBackend {
    fn sync_file(&self, _file: &File) -> std::io::Result<()> {
        self.attempts.fetch_add(1, Ordering::AcqRel);
        Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            "injected fsync failure",
        ))
    }
}

fn assert_trie_matches_reference(
    dict: &PersistentARTrie<i32>,
    expected: &BTreeMap<String, i32>,
    seen: &BTreeSet<String>,
) {
    assert_eq!(dict.len(), Some(expected.len()));

    for (term, value) in expected {
        assert!(dict.contains(term), "expected term is visible: {}", term);
        assert_eq!(dict.get_value(term), Some(*value), "value for {}", term);
    }

    for term in seen {
        if !expected.contains_key(term) {
            assert!(
                !dict.contains(term),
                "removed term stayed visible: {}",
                term
            );
        }
    }
}

fn assert_send<T: Send>() {}

fn assert_send_sync<T: Send + Sync>() {}

#[derive(Debug, Clone)]
enum TrieOp {
    Insert(String, i32),
    Remove(String),
}

#[derive(Debug, Clone)]
enum CertifiedTraceCommand {
    Put(String, i32),
    Remove(String),
}

#[derive(Debug, Clone)]
struct CertifiedTraceStep {
    before_digest: u64,
    command: CertifiedTraceCommand,
    after_digest: u64,
}

fn fnv1a_update(mut hash: u64, bytes: &[u8]) -> u64 {
    const FNV_PRIME: u64 = 0x100_0000_01b3;

    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }

    hash
}

fn reference_digest(entries: &BTreeMap<String, i32>) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

    entries.iter().fold(FNV_OFFSET, |hash, (key, value)| {
        let hash = fnv1a_update(hash, &(key.len() as u64).to_le_bytes());
        let hash = fnv1a_update(hash, key.as_bytes());
        fnv1a_update(hash, &value.to_le_bytes())
    })
}

fn apply_certified_command(entries: &mut BTreeMap<String, i32>, command: &CertifiedTraceCommand) {
    match command {
        CertifiedTraceCommand::Put(term, value) => {
            entries.insert(term.clone(), *value);
        }
        CertifiedTraceCommand::Remove(term) => {
            entries.remove(term);
        }
    }
}

fn build_certified_trace(commands: Vec<CertifiedTraceCommand>) -> Vec<CertifiedTraceStep> {
    let mut entries = BTreeMap::new();
    let mut trace = Vec::with_capacity(commands.len());

    for command in commands {
        let before_digest = reference_digest(&entries);
        apply_certified_command(&mut entries, &command);
        let after_digest = reference_digest(&entries);
        trace.push(CertifiedTraceStep {
            before_digest,
            command,
            after_digest,
        });
    }

    trace
}

fn verify_certified_trace(
    initial: BTreeMap<String, i32>,
    trace: &[CertifiedTraceStep],
) -> Result<BTreeMap<String, i32>, &'static str> {
    let mut entries = initial;

    for step in trace {
        if reference_digest(&entries) != step.before_digest {
            return Err("before digest mismatch");
        }

        apply_certified_command(&mut entries, &step.command);

        if reference_digest(&entries) != step.after_digest {
            return Err("after digest mismatch");
        }
    }

    Ok(entries)
}

fn ascii_term_strategy() -> impl Strategy<Value = String> {
    "[a-z]{1,16}"
}

fn trie_op_strategy() -> impl Strategy<Value = TrieOp> {
    prop_oneof![
        (ascii_term_strategy(), any::<i32>()).prop_map(|(term, value)| TrieOp::Insert(term, value)),
        ascii_term_strategy().prop_map(TrieOp::Remove),
    ]
}

fn wal_codec_reference_records() -> Vec<WalRecord> {
    vec![
        WalRecord::Insert {
            term: b"alpha".to_vec(),
            value: Some(vec![1, 2, 3, 4]),
        },
        WalRecord::Insert {
            term: b"beta".to_vec(),
            value: None,
        },
        WalRecord::Remove {
            term: b"gamma".to_vec(),
        },
        WalRecord::Checkpoint {
            checkpoint_lsn: 42,
            timestamp: 1_700_000_000,
        },
        WalRecord::BeginTx { tx_id: 11 },
        WalRecord::CommitTx { tx_id: 11 },
        WalRecord::AbortTx { tx_id: 12 },
        WalRecord::Increment {
            term: b"counter".to_vec(),
            delta: -3,
            result: 9,
        },
        WalRecord::Upsert {
            term: b"delta".to_vec(),
            value: vec![9, 8, 7],
        },
        WalRecord::CompareAndSwap {
            term: b"epsilon".to_vec(),
            expected: Some(vec![1]),
            new_value: vec![2],
            success: true,
        },
        WalRecord::CompareAndSwap {
            term: b"zeta".to_vec(),
            expected: None,
            new_value: vec![3],
            success: false,
        },
        WalRecord::BatchInsert {
            entries: vec![(b"eta".to_vec(), Some(vec![1])), (b"theta".to_vec(), None)],
        },
        WalRecord::BatchIncrement {
            entries: vec![(b"iota".to_vec(), 5), (b"kappa".to_vec(), -2)],
        },
        WalRecord::VersionUpdate {
            version_id: 7,
            root_ptr: 0x10_20_30,
            node_count: 19,
            timestamp: 1_700_000_001,
        },
        WalRecord::VersionDurable {
            version_id: 7,
            checksum: 0x1234_5678,
        },
        WalRecord::VersionGc {
            version_ids: vec![1, 2, 3],
        },
    ]
}

fn deterministic_trace_key(rng: &mut StdRng, step: usize) -> String {
    const PREFIXES: [&str; 8] = ["aa", "ab", "bucket", "doc", "edge", "node", "wal", "zz"];
    let prefix = PREFIXES[step % PREFIXES.len()];
    let shard = rng.gen_range(0..64);
    let suffix = rng.gen_range(0..512);
    format!("{prefix}-{shard:02}-{suffix:03}")
}

fn encoded_i32(value: i32) -> Vec<u8> {
    bincode_compat::serialize(&value).expect("serialize i32 WAL value")
}

fn wal_insert(term: &str, value: i32) -> WalRecord {
    WalRecord::Insert {
        term: term.as_bytes().to_vec(),
        value: Some(encoded_i32(value)),
    }
}

fn wal_remove(term: &str) -> WalRecord {
    WalRecord::Remove {
        term: term.as_bytes().to_vec(),
    }
}

fn crash_prefix_records() -> Vec<WalRecord> {
    vec![
        wal_insert("alpha", 1),
        wal_insert("beta", 2),
        wal_remove("alpha"),
        wal_insert("gamma", 3),
        wal_insert("beta", 22),
    ]
}

fn crash_prefix_expectations() -> Vec<BTreeMap<String, i32>> {
    let mut expectations = Vec::new();
    let mut expected = BTreeMap::new();
    expectations.push(expected.clone());

    expected.insert("alpha".to_string(), 1);
    expectations.push(expected.clone());

    expected.insert("beta".to_string(), 2);
    expectations.push(expected.clone());

    expected.remove("alpha");
    expectations.push(expected.clone());

    expected.insert("gamma".to_string(), 3);
    expectations.push(expected.clone());

    expected.insert("beta".to_string(), 22);
    expectations.push(expected);

    expectations
}

fn crash_prefix_seen_terms() -> BTreeSet<String> {
    ["alpha", "beta", "gamma"]
        .into_iter()
        .map(String::from)
        .collect()
}

fn write_wal_fixture(wal_path: &Path, records: &[WalRecord]) -> Vec<u8> {
    let writer = WalWriter::create(wal_path).expect("create WAL fixture");

    for record in records {
        writer
            .append(record.clone())
            .expect("append fixture record");
    }

    writer.sync().expect("sync WAL fixture");
    drop(writer);

    std::fs::read(wal_path).expect("read WAL fixture bytes")
}

fn wal_record_spans(wal_bytes: &[u8]) -> Vec<(usize, usize)> {
    let mut offset = WalHeader::SIZE;
    let mut spans = Vec::new();

    assert!(
        wal_bytes.len() >= WalHeader::SIZE,
        "WAL bytes must include the header"
    );

    while offset < wal_bytes.len() {
        assert!(
            offset + WalWriter::RECORD_HEADER_SIZE <= wal_bytes.len(),
            "WAL fixture ended inside a record header"
        );

        let length = u32::from_le_bytes([
            wal_bytes[offset + 4],
            wal_bytes[offset + 5],
            wal_bytes[offset + 6],
            wal_bytes[offset + 7],
        ]) as usize;
        assert!(
            length >= WalWriter::RECORD_HEADER_SIZE,
            "WAL fixture record length is too small"
        );

        let end = offset
            .checked_add(length)
            .expect("WAL record end offset overflowed");
        assert!(end <= wal_bytes.len(), "WAL fixture record exceeds file");

        spans.push((offset, end));
        offset = end;
    }

    spans
}

fn copy_base_with_wal_bytes(
    base_path: &Path,
    parent: &Path,
    case_name: &str,
    wal_bytes: &[u8],
) -> PersistentARTrie<i32> {
    let case_dir = parent.join(case_name);
    std::fs::create_dir_all(&case_dir).expect("create crash-prefix case directory");
    let case_path = case_dir.join("case.part");

    std::fs::copy(base_path, &case_path).expect("copy base persistent trie file");
    std::fs::write(case_path.with_extension("wal"), wal_bytes).expect("write case WAL");

    PersistentARTrie::<i32>::open(&case_path).expect("reopen crash-prefix case")
}

#[test]
fn unsafe_send_sync_contracts_remain_explicit() {
    assert_send_sync::<PersistentNode>();
    assert_send_sync::<PersistentCharNode>();
    assert_send_sync::<PersistentARTrieCharNode<u64>>();
    assert_send_sync::<OptimisticCell<u64>>();
    assert_send_sync::<PersistentVocabARTrie<MmapDiskManager>>();
    assert_send_sync::<ConcurrentVocabARTrie>();
    assert_send_sync::<LockFreeVocab>();
    // G4: the byte/char overlay nodes are now generic over `V`; `TrieRoot` (the
    // MVCC snapshot read bound) is impl'd on the byte node at `<i64>` (its
    // persistence/counter domain) and on the char node at any `<V>`. Pin the byte
    // `ReadTransaction` to the `<i64>` instantiation that carries the impl.
    assert_send::<ReadTransaction<PersistentNode<i64>>>();
    assert_send::<ReadTransaction<PersistentCharNode>>();

    #[cfg(feature = "io-uring-backend")]
    assert_send_sync::<libdictenstein::persistent_artrie::IoUringDiskManager>();
}

#[test]
fn vocab_child_remove_transfers_box_ownership_once() {
    let mut root = VocabTrieNode::new();
    let root_ref = NodeRef::new(0, 0);

    root.get_or_create_child('a', root_ref).set_value(7);
    let raw_before = root.get_child('a').expect("child exists") as *const VocabTrieNode;

    let removed = root.remove_child('a').expect("child removed");
    let raw_after = removed.as_ref() as *const VocabTrieNode;

    assert_eq!(raw_before, raw_after);
    assert_eq!(removed.get_value(), Some(7));
    assert!(root.get_child('a').is_none());
    assert!(root.remove_child('a').is_none());
}

#[test]
fn vocab_insert_child_replaces_without_aliasing_old_box() {
    let mut root = VocabTrieNode::new();
    let root_ref = NodeRef::new(0, 0);

    let mut first = VocabTrieNode::with_parent(root_ref, 'x');
    first.set_value(11);
    assert!(root.insert_child('x', first).is_none());

    let first_raw = root.get_child('x').expect("first child exists") as *const VocabTrieNode;

    let mut second = VocabTrieNode::with_parent(root_ref, 'x');
    second.set_value(22);
    let replaced = root
        .insert_child('x', second)
        .expect("old child returned on replacement");

    let second_raw = root.get_child('x').expect("second child exists") as *const VocabTrieNode;
    assert_eq!(replaced.as_ref() as *const VocabTrieNode, first_raw);
    assert_ne!(second_raw, first_raw);
    assert_eq!(replaced.get_value(), Some(11));
    assert_eq!(
        root.get_child('x').and_then(VocabTrieNode::get_value),
        Some(22)
    );
}

#[test]
fn vocab_clone_deep_copies_child_boxes() {
    let mut original = VocabTrieNode::new();
    let root_ref = NodeRef::new(0, 0);

    original.get_or_create_child('z', root_ref).set_value(33);
    let mut cloned = original.clone();

    let original_child = original.remove_child('z').expect("original child removed");
    let cloned_child = cloned.remove_child('z').expect("cloned child removed");

    assert_ne!(
        original_child.as_ref() as *const VocabTrieNode,
        cloned_child.as_ref() as *const VocabTrieNode
    );
    assert_eq!(original_child.get_value(), Some(33));
    assert_eq!(cloned_child.get_value(), Some(33));
    assert!(original.get_child('z').is_none());
    assert!(cloned.get_child('z').is_none());
}

#[test]
fn vocab_get_or_create_child_mutation_keeps_unique_raw_borrow() {
    let mut root = VocabTrieNode::new();
    let root_ref = NodeRef::new(0, 0);
    let child_ref = NodeRef::new(0, 1);

    {
        let child = root.get_or_create_child('語', root_ref);
        child.set_value(101);
        child.get_or_create_child('尾', child_ref).set_value(202);
    }

    let child_raw = root.get_child('語').expect("child exists") as *const VocabTrieNode;
    {
        let same_child = root.get_or_create_child('語', root_ref);
        assert_eq!(same_child as *const VocabTrieNode, child_raw);
        same_child.set_value(303);
    }

    let removed = root.remove_child('語').expect("child removed");
    assert_eq!(removed.as_ref() as *const VocabTrieNode, child_raw);
    assert_eq!(removed.get_value(), Some(303));
    assert_eq!(
        removed.get_child('尾').and_then(VocabTrieNode::get_value),
        Some(202)
    );
    assert!(root.get_child('語').is_none());
}

#[test]
fn char_child_remove_transfers_box_ownership_once() {
    let mut root = CharTrieNodeInner::<i32>::new();

    root.get_or_create_child('λ').set_final(true);
    let raw_before = root.get_child('λ').expect("child exists") as *const CharTrieNodeInner<i32>;

    let removed = root.remove_child('λ').expect("child removed");
    let raw_after = removed.as_ref() as *const CharTrieNodeInner<i32>;

    assert_eq!(raw_before, raw_after);
    assert!(removed.is_final());
    assert!(root.get_child('λ').is_none());
    assert!(root.remove_child('λ').is_none());
}

#[test]
fn char_insert_child_replaces_without_aliasing_old_box() {
    let mut root = CharTrieNodeInner::<i32>::new();

    let mut first = CharTrieNodeInner::new();
    first.set_final(true);
    assert!(root.insert_child('ß', first).is_none());

    let first_raw =
        root.get_child('ß').expect("first child exists") as *const CharTrieNodeInner<i32>;

    let mut second = CharTrieNodeInner::new();
    second.set_final(false);
    second.get_or_create_child('ø').set_final(true);
    let replaced = root
        .insert_child('ß', second)
        .expect("old child returned on replacement");

    let second_raw =
        root.get_child('ß').expect("second child exists") as *const CharTrieNodeInner<i32>;
    assert_eq!(
        replaced.as_ref() as *const CharTrieNodeInner<i32>,
        first_raw
    );
    assert_ne!(second_raw, first_raw);
    assert!(replaced.is_final());
    assert!(!root.get_child('ß').expect("replacement child").is_final());
    assert!(root
        .get_child('ß')
        .and_then(|child| child.get_child('ø'))
        .expect("replacement grandchild")
        .is_final());
}

#[test]
fn char_clone_deep_copies_child_boxes() {
    let mut original = CharTrieNodeInner::<i32>::new();

    original.get_or_create_child('Ж').set_final(true);
    let mut cloned = original.clone();

    let original_child = original.remove_child('Ж').expect("original child removed");
    let cloned_child = cloned.remove_child('Ж').expect("cloned child removed");

    assert_ne!(
        original_child.as_ref() as *const CharTrieNodeInner<i32>,
        cloned_child.as_ref() as *const CharTrieNodeInner<i32>
    );
    assert!(original_child.is_final());
    assert!(cloned_child.is_final());
    assert!(original.get_child('Ж').is_none());
    assert!(cloned.get_child('Ж').is_none());
}

#[test]
fn char_get_or_create_child_mutation_keeps_unique_raw_borrow() {
    let mut root = CharTrieNodeInner::<i32>::new();

    {
        let child = root.get_or_create_child('語');
        child.set_final(true);
        child.get_or_create_child('尾').set_final(true);
    }

    let child_raw = root.get_child('語').expect("child exists") as *const CharTrieNodeInner<i32>;
    {
        let same_child = root.get_or_create_child('語');
        assert_eq!(same_child as *const CharTrieNodeInner<i32>, child_raw);
        same_child.set_final(false);
    }

    let removed = root.remove_child('語').expect("child removed");
    assert_eq!(removed.as_ref() as *const CharTrieNodeInner<i32>, child_raw);
    assert!(!removed.is_final());
    assert!(removed
        .get_child('尾')
        .expect("grandchild remains owned by removed subtree")
        .is_final());
    assert!(root.get_child('語').is_none());
}

#[test]
fn vocab_checkpoint_reopen_preserves_unicode_bijection() {
    let temp_dir = TempDir::new().expect("temp dir");
    let path = temp_dir.path().join("vocab_reopen_unicode.vocab");
    let terms = [
        "alpha",
        "résumé",
        "東京",
        "emoji😀",
        "antidisestablishmentarianism",
        "antidisestablishment",
    ];

    {
        let mut vocab = PersistentVocabARTrie::create(&path).expect("create vocab trie");
        for (expected_index, term) in terms.iter().enumerate() {
            assert_eq!(
                vocab.insert(term).expect("insert vocab term"),
                expected_index as u64
            );
        }
        assert_eq!(vocab.insert("résumé").expect("insert duplicate"), 1);
        vocab.checkpoint().expect("checkpoint vocab trie");
    }

    let vocab = PersistentVocabARTrie::open(&path).expect("reopen vocab trie");
    assert_eq!(vocab.len(), terms.len());

    for (expected_index, term) in terms.iter().enumerate() {
        let index = expected_index as u64;
        assert_eq!(vocab.get_index(term), Some(index), "term -> index: {term}");
        assert_eq!(
            vocab.get_term(index),
            Some((*term).to_string()),
            "index -> term: {index}"
        );
    }
}

#[test]
fn vocab_duplicate_insert_keeps_stable_index_after_reopen() {
    let temp_dir = TempDir::new().expect("temp dir");
    let path = temp_dir.path().join("vocab_duplicate_reopen.vocab");

    {
        let mut vocab = PersistentVocabARTrie::create(&path).expect("create vocab trie");
        assert_eq!(vocab.insert("duplicate").expect("insert duplicate"), 0);
        assert_eq!(vocab.insert("delta").expect("insert delta"), 1);
        assert_eq!(
            vocab.insert("duplicate").expect("insert duplicate again"),
            0
        );
        vocab.checkpoint().expect("checkpoint vocab trie");
    }

    let mut reopened = PersistentVocabARTrie::open(&path).expect("reopen vocab trie");
    assert_eq!(reopened.insert("duplicate").expect("insert duplicate"), 0);
    assert_eq!(reopened.get_index("duplicate"), Some(0));
    assert_eq!(reopened.get_term(0), Some("duplicate".to_string()));
    assert_eq!(
        reopened.insert("epsilon").expect("insert epsilon"),
        2,
        "duplicate replay must not consume a new index"
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn bucket_search_matches_sorted_reference(
        entries in prop::collection::vec(bucket_entry_strategy(), 1..=64)
    ) {
        let expected: BTreeMap<Vec<u8>, Vec<u8>> = entries.into_iter().collect();
        let (bucket, sorted_keys) = build_bucket(expected.clone());

        let expected_entries: Vec<(Vec<u8>, Option<Vec<u8>>)> = expected
            .iter()
            .map(|(key, value)| (key.clone(), Some(value.clone())))
            .collect();

        prop_assert_eq!(bucket_entries(&bucket), expected_entries);

        for key in &sorted_keys {
            assert_search_partition(&bucket, &sorted_keys, key);
        }

        for probe in [Vec::new(), vec![0], vec![64], vec![127], vec![255]] {
            assert_search_partition(&bucket, &sorted_keys, &probe);
        }
    }

    #[test]
    fn bucket_split_and_merge_preserve_reference_order(
        entries in prop::collection::vec(bucket_entry_strategy(), 2..=80)
    ) {
        let expected: BTreeMap<Vec<u8>, Vec<u8>> = entries.into_iter().collect();
        prop_assume!(expected.len() >= 2);

        let (bucket, _) = build_bucket(expected);
        let before = bucket_entries(&bucket);
        let split = bucket.split().expect("non-trivial bucket splits");

        let left = bucket_entries(&split.left);
        let right = bucket_entries(&split.right);
        prop_assert!(!left.is_empty());
        prop_assert!(!right.is_empty());
        prop_assert_eq!(split.split_key, right[0].0.clone());

        let combined: Vec<_> = left.iter().chain(right.iter()).cloned().collect();
        prop_assert_eq!(combined, before.clone());

        let mut merged = split.left;
        merged.merge(&split.right).expect("split halves fit original page");
        prop_assert_eq!(bucket_entries(&merged), before);
    }

    #[test]
    fn bucket_page_roundtrip_preserves_reference_entries(
        entries in prop::collection::vec(bucket_entry_strategy(), 1..=64)
    ) {
        let expected: BTreeMap<Vec<u8>, Vec<u8>> = entries.into_iter().collect();
        let (bucket, _) = build_bucket(expected.clone());
        let page = bucket.as_bytes().to_vec();
        let restored = StringBucket::from_bytes(&page).expect("bucket page decodes");

        let expected_entries: Vec<(Vec<u8>, Option<Vec<u8>>)> = expected
            .iter()
            .map(|(key, value)| (key.clone(), Some(value.clone())))
            .collect();

        prop_assert_eq!(bucket_entries(&restored), expected_entries);
    }

    #[test]
    fn persistent_artrie_trace_matches_btreemap(
        ops in prop::collection::vec(trie_op_strategy(), 1..=80)
    ) {
        let temp_dir = TempDir::new().expect("temp dir");
        let path = temp_dir.path().join("trace.part");
        let dict = PersistentARTrie::<i32>::create(&path).expect("create trie");
        let mut expected = BTreeMap::new();
        let mut seen = BTreeSet::new();

        for op in ops {
            match op {
                TrieOp::Insert(term, value) => {
                    dict.insert_with_value(&term, value);
                    expected.insert(term.clone(), value);
                    seen.insert(term);
                }
                TrieOp::Remove(term) => {
                    dict.remove(&term);
                    expected.remove(&term);
                    seen.insert(term);
                }
            }

            prop_assert_eq!(dict.len(), Some(expected.len()));

            for (term, value) in &expected {
                prop_assert!(dict.contains(term), "expected term is visible: {}", term);
                prop_assert_eq!(dict.get_value(term), Some(*value));
            }

            for term in &seen {
                if !expected.contains_key(term) {
                    prop_assert!(!dict.contains(term), "removed term stayed visible: {}", term);
                }
            }
        }
    }
}

#[test]
fn deterministic_large_trace_matches_btreemap_reference() {
    let temp_dir = TempDir::new().expect("temp dir");
    let path = temp_dir.path().join("large_trace.part");
    let dict = PersistentARTrie::<i32>::create(&path).expect("create trie");
    let mut expected = BTreeMap::new();
    let mut seen = BTreeSet::new();
    let mut rng = StdRng::seed_from_u64(0xA7_71E5_2026);

    for step in 0..2_048 {
        let term = deterministic_trace_key(&mut rng, step);
        seen.insert(term.clone());

        match rng.gen_range(0..10) {
            0..=6 => {
                let value = rng.gen_range(-100_000..=100_000);
                dict.insert_with_value(&term, value);
                expected.insert(term, value);
            }
            7..=8 => {
                dict.remove(&term);
                expected.remove(&term);
            }
            _ => {
                assert_eq!(dict.get_value(&term), expected.get(&term).copied());
            }
        }

        if step % 31 == 0 {
            assert_trie_matches_reference(&dict, &expected, &seen);
        }
    }

    assert_trie_matches_reference(&dict, &expected, &seen);
}

#[test]
fn deterministic_reopen_trace_matches_btreemap_reference() {
    let temp_dir = TempDir::new().expect("temp dir");
    let path = temp_dir.path().join("reopen_trace.part");
    let mut expected = BTreeMap::new();
    let mut seen = BTreeSet::new();
    let mut rng = StdRng::seed_from_u64(0xC0_55EC_7ED);

    {
        let dict = PersistentARTrie::<i32>::create(&path).expect("create trie");

        for step in 0..768 {
            let term = deterministic_trace_key(&mut rng, step * 13);
            seen.insert(term.clone());

            if rng.gen_bool(0.72) {
                let value = rng.gen_range(-50_000..=50_000);
                dict.insert_with_value(&term, value);
                expected.insert(term, value);
            } else {
                dict.remove(&term);
                expected.remove(&term);
            }

            if step % 97 == 0 {
                assert_trie_matches_reference(&dict, &expected, &seen);
            }
        }

        dict.sync().expect("sync WAL and data");
    }

    let reopened = PersistentARTrie::<i32>::open(&path).expect("reopen trie");
    assert_trie_matches_reference(&reopened, &expected, &seen);
}

#[test]
fn wal_header_roundtrip_and_rejection_matches_format_contract() {
    let mut header = WalHeader::new();
    header.checkpoint_lsn = 123;
    header.reserved[0] = 0xa5;

    let bytes = header.to_bytes();
    let restored = WalHeader::from_bytes(&bytes).expect("valid WAL header");
    assert_eq!(restored.magic, WalHeader::MAGIC);
    assert_eq!(restored.version, WalHeader::VERSION);
    assert_eq!(restored.checkpoint_lsn, 123);
    assert_eq!(restored.reserved[0], 0xa5);

    let mut bad_magic = bytes;
    bad_magic[0] ^= 0xff;
    assert!(matches!(
        WalHeader::from_bytes(&bad_magic),
        Err(WalError::CorruptedRecord(_))
    ));

    let mut bad_version = bytes;
    bad_version[8..12].copy_from_slice(&(WalHeader::VERSION + 1).to_le_bytes());
    assert!(matches!(
        WalHeader::from_bytes(&bad_version),
        Err(WalError::CorruptedRecord(_))
    ));
}

#[test]
fn wal_record_payload_codec_roundtrips_all_formal_variants() {
    for record in wal_codec_reference_records() {
        let payload = record.serialize_payload();
        let restored =
            WalRecord::deserialize(record.record_type(), &payload).expect("payload roundtrip");

        assert_eq!(restored, record);
        assert_eq!(
            record.serialized_size(),
            WalWriter::RECORD_HEADER_SIZE + payload.len()
        );
    }
}

#[test]
fn wal_record_payload_truncation_is_rejected() {
    for record in wal_codec_reference_records() {
        let payload = record.serialize_payload();
        for cut in 0..payload.len() {
            assert!(
                WalRecord::deserialize(record.record_type(), &payload[..cut]).is_err(),
                "truncated {:?} payload at {} was accepted",
                record.record_type(),
                cut
            );
        }
    }

    assert!(matches!(
        WalRecordType::try_from(0xff),
        Err(WalError::InvalidRecordType(0xff))
    ));
}

#[test]
fn bucket_page_parser_rejects_malformed_headers() {
    let bucket = StringBucket::with_values();
    let bytes = bucket.as_bytes().to_vec();
    assert!(StringBucket::from_bytes(&bytes).is_ok());

    let short = vec![0u8; bytes.len() - 1];
    assert!(matches!(
        StringBucket::from_bytes(&short),
        Err(BucketError::InvalidSize { .. })
    ));

    let mut bad_magic = bytes.clone();
    bad_magic[0] ^= 0xff;
    assert!(matches!(
        StringBucket::from_bytes(&bad_magic),
        Err(BucketError::InvalidMagic { .. })
    ));

    let mut bad_version = bytes;
    bad_version[8..10].copy_from_slice(&u16::MAX.to_le_bytes());
    assert!(matches!(
        StringBucket::from_bytes(&bad_version),
        Err(BucketError::UnsupportedVersion { .. })
    ));
}

#[test]
fn wal_reader_reports_truncated_payload_after_durable_prefix() {
    let temp_dir = TempDir::new().expect("temp dir");
    let wal_path = temp_dir.path().join("truncated_payload.wal");
    let writer = WalWriter::create(&wal_path).expect("create WAL");

    writer
        .append(WalRecord::Insert {
            term: b"alpha".to_vec(),
            value: Some(vec![1]),
        })
        .expect("append first record");
    writer
        .append(WalRecord::Insert {
            term: b"beta".to_vec(),
            value: Some(vec![0xaa; 32]),
        })
        .expect("append second record");
    writer.sync().expect("sync WAL");
    drop(writer);

    let full_len = std::fs::metadata(&wal_path).expect("WAL metadata").len();
    OpenOptions::new()
        .write(true)
        .open(&wal_path)
        .expect("open WAL for truncation")
        .set_len(full_len - 3)
        .expect("truncate inside second payload");

    let mut reader = WalReader::new(&wal_path).expect("open WAL reader");
    let first = reader
        .next_record()
        .expect("first record")
        .expect("first record is intact");
    assert_eq!(first.0, 1);
    assert_eq!(
        first.1,
        WalRecord::Insert {
            term: b"alpha".to_vec(),
            value: Some(vec![1]),
        }
    );

    assert!(matches!(
        reader.next_record().expect("truncated second record"),
        Err(WalError::UnexpectedEof)
    ));
}

#[test]
fn wal_reader_ignores_torn_trailing_header_after_durable_prefix() {
    let temp_dir = TempDir::new().expect("temp dir");
    let wal_path = temp_dir.path().join("torn_header.wal");
    let writer = WalWriter::create(&wal_path).expect("create WAL");

    writer
        .append(WalRecord::Remove {
            term: b"alpha".to_vec(),
        })
        .expect("append durable record");
    writer.sync().expect("sync WAL");
    drop(writer);

    let mut file = OpenOptions::new()
        .append(true)
        .open(&wal_path)
        .expect("open WAL for torn header append");
    file.write_all(&[0xde, 0xad, 0xbe, 0xef])
        .expect("append partial record header");
    file.sync_all().expect("sync torn WAL tail");

    let mut reader = WalReader::new(&wal_path).expect("open WAL reader");
    let first = reader
        .next_record()
        .expect("first record")
        .expect("first record is intact");
    assert_eq!(first.0, 1);
    assert_eq!(
        first.1,
        WalRecord::Remove {
            term: b"alpha".to_vec(),
        }
    );
    assert!(reader.next_record().is_none());
}

#[test]
fn segment_sync_failure_does_not_advance_durable_frontier() {
    let temp_dir = TempDir::new().expect("temp dir");
    let wal_path = temp_dir.path().join("active.wal");
    let segment_path = temp_dir.path().join("pending.segment");
    std::fs::write(&segment_path, b"pending segment bytes").expect("write segment");
    let segment_file = File::open(&segment_path).expect("open pending segment");
    let size_bytes = std::fs::metadata(&segment_path)
        .expect("segment metadata")
        .len();

    let attempts = Arc::new(AtomicUsize::new(0));
    let manager = SegmentSyncManager::with_sync_backend(
        AsyncWalConfig {
            idle_check_interval_ms: 1,
            pending_dir: temp_dir.path().join("pending"),
            ..AsyncWalConfig::default()
        },
        WalConfig::no_archive(),
        wal_path,
        0,
        Arc::new(FailingSyncBackend {
            attempts: Arc::clone(&attempts),
        }),
    );

    manager.enqueue(PendingSegment {
        path: segment_path,
        lsn_range: (1, 4),
        file: segment_file,
        rotated_at: Instant::now(),
        size_bytes,
    });

    let deadline = Instant::now() + Duration::from_secs(1);
    while attempts.load(Ordering::Acquire) == 0 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }

    assert!(
        attempts.load(Ordering::Acquire) > 0,
        "sync backend was not exercised"
    );
    assert_eq!(
        manager.global_synced_lsn.load(Ordering::Acquire),
        0,
        "failed fsync advanced the durable frontier"
    );
    assert!(
        !manager
            .wait_for_lsn_timeout(4, Duration::from_millis(20))
            .expect("timeout wait should not fail while manager is running"),
        "failed fsync reported the target LSN as durable"
    );

    manager.stop();
}

#[test]
fn proof_carrying_trace_certificate_replays_reference() {
    let commands = vec![
        CertifiedTraceCommand::Put("alpha".to_string(), 1),
        CertifiedTraceCommand::Put("beta".to_string(), 2),
        CertifiedTraceCommand::Put("alpha".to_string(), 7),
        CertifiedTraceCommand::Remove("beta".to_string()),
        CertifiedTraceCommand::Put("gamma".to_string(), 3),
    ];
    let trace = build_certified_trace(commands);
    let expected = verify_certified_trace(BTreeMap::new(), &trace).expect("valid certificate");

    let temp_dir = TempDir::new().expect("temp dir");
    let path = temp_dir.path().join("certified_trace.part");
    let dict = PersistentARTrie::<i32>::create(&path).expect("create trie");
    let mut seen = BTreeSet::new();

    for step in &trace {
        match &step.command {
            CertifiedTraceCommand::Put(term, value) => {
                seen.insert(term.clone());
                dict.insert_with_value(term, *value);
            }
            CertifiedTraceCommand::Remove(term) => {
                seen.insert(term.clone());
                dict.remove(term);
            }
        }
    }

    assert_trie_matches_reference(&dict, &expected, &seen);
}

#[test]
fn proof_carrying_trace_rejects_corrupt_certificate() {
    let mut trace = build_certified_trace(vec![
        CertifiedTraceCommand::Put("alpha".to_string(), 1),
        CertifiedTraceCommand::Put("beta".to_string(), 2),
        CertifiedTraceCommand::Remove("alpha".to_string()),
    ]);

    trace[1].after_digest ^= 0x9e37_79b9_7f4a_7c15;

    assert_eq!(
        verify_certified_trace(BTreeMap::new(), &trace),
        Err("after digest mismatch")
    );
}

#[test]
fn document_transaction_visibility_matches_tla_model() {
    let temp_dir = TempDir::new().expect("temp dir");
    let path = temp_dir.path().join("document_tx.part");
    let dict = PersistentARTrie::<i32>::create(&path).expect("create trie");

    let mut tx = dict.begin_document("doc-1").expect("begin tx");
    dict.tx_insert(&mut tx, "alpha", Some(1));
    dict.tx_insert(&mut tx, "beta", Some(2));

    assert!(!dict.contains("alpha"));
    assert!(!dict.contains("beta"));

    assert_eq!(dict.commit_document(tx).expect("commit tx"), 2);
    assert_eq!(dict.get_value("alpha"), Some(1));
    assert_eq!(dict.get_value("beta"), Some(2));

    let mut aborted = dict.begin_document("doc-2").expect("begin tx");
    dict.tx_insert(&mut aborted, "alpha", Some(9));
    dict.tx_insert(&mut aborted, "gamma", Some(3));
    dict.abort_document(aborted).expect("abort tx");

    assert_eq!(dict.get_value("alpha"), Some(1));
    assert!(!dict.contains("gamma"));
}

#[test]
fn wal_crc_corruption_fails_closed() {
    let temp_dir = TempDir::new().expect("temp dir");
    let wal_path = temp_dir.path().join("corrupt.wal");
    let writer = WalWriter::create(&wal_path).expect("create WAL");

    writer
        .append(WalRecord::Insert {
            term: b"alpha".to_vec(),
            value: Some(vec![1, 2, 3]),
        })
        .expect("append first record");
    writer
        .append(WalRecord::Insert {
            term: b"beta".to_vec(),
            value: None,
        })
        .expect("append second record");
    writer.sync().expect("sync WAL");
    drop(writer);

    let mut reader = WalReader::new(&wal_path).expect("open WAL reader");
    assert!(reader.next_record().expect("first record").is_ok());
    assert!(reader.next_record().expect("second record").is_ok());
    assert!(reader.next_record().is_none());

    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&wal_path)
        .expect("open WAL for corruption");
    file.seek(SeekFrom::Start(WalHeader::SIZE as u64))
        .expect("seek to first record");
    let mut byte = [0u8; 1];
    file.read_exact(&mut byte).expect("read CRC byte");
    file.seek(SeekFrom::Start(WalHeader::SIZE as u64))
        .expect("seek to first record");
    file.write_all(&[byte[0] ^ 0xff]).expect("corrupt CRC");
    file.sync_all().expect("sync corrupted WAL");

    let mut reader = WalReader::new(&wal_path).expect("open corrupted WAL reader");
    let result = reader
        .next_record()
        .expect("corrupted record should still be present");
    assert!(result.is_err(), "corrupted WAL record was accepted");
}

#[test]
fn version_gc_respects_reader_guards_and_retention() {
    use libdictenstein::persistent_artrie::version_gc::{GcConfig, ReaderGuard, VersionGcRegistry};

    let temp_dir = TempDir::new().expect("temp dir");
    let wal_path = temp_dir.path().join("version_gc.wal");
    let mut wal = WalWriter::create(&wal_path).expect("create WAL");

    let registry = VersionGcRegistry::new(GcConfig {
        grace_period: Duration::from_millis(0),
        min_retained_versions: 0,
        background_gc: false,
        ..GcConfig::for_testing()
    });

    registry.add_gc_candidate(1, 100, 50);
    let guard = ReaderGuard::new(1, Arc::clone(&registry));
    registry.record_modification();

    assert!(registry.run_gc_cycle(&mut wal).expect("run GC").is_empty());
    assert_eq!(registry.pending_versions(), vec![1]);
    assert_eq!(registry.reader_count(1), 1);

    drop(guard);
    registry.record_modification();

    assert_eq!(registry.run_gc_cycle(&mut wal).expect("run GC"), vec![1]);
    assert_eq!(registry.pending_count(), 0);
}

#[test]
fn swizzled_pointer_state_transitions_preserve_location_contract() {
    use libdictenstein::persistent_artrie::swizzled_ptr::{MAX_BLOCK_ID, MAX_OFFSET};
    use libdictenstein::persistent_artrie::{NodeType, SwizzledPtr};

    let ptr = SwizzledPtr::on_disk(MAX_BLOCK_ID, MAX_OFFSET, NodeType::Node48);
    let restored = SwizzledPtr::from_raw(ptr.to_raw());
    let loc = restored.disk_location().expect("disk location decodes");

    assert!(restored.is_on_disk());
    assert_eq!(loc.block_id, MAX_BLOCK_ID);
    assert_eq!(loc.offset, MAX_OFFSET);
    assert_eq!(loc.node_type, NodeType::Node48);
    assert_eq!(
        loc.file_offset(256 * 1024),
        (MAX_BLOCK_ID as u64 * 256 * 1024) + MAX_OFFSET as u64
    );

    let value = 0xfeed_cafe_u64;
    restored.swizzle(&value).expect("swizzle disk pointer");
    assert!(restored.is_swizzled());
    assert!(restored.disk_location().is_none());
    assert_eq!(restored.as_ptr::<u64>(), Some(&value as *const u64));

    let memory_raw = restored.to_raw();
    let memory_raw_restored = SwizzledPtr::from_raw(memory_raw);
    assert!(
        !memory_raw_restored.is_swizzled(),
        "serialized memory-state sentinels must not fabricate pointer provenance"
    );
    assert_eq!(memory_raw_restored.as_ptr::<u64>(), None);
    assert_eq!(memory_raw_restored.disk_location(), None);

    let previous = restored
        .unswizzle::<u64>(17, 23, NodeType::Bucket)
        .expect("unswizzle memory pointer");
    assert_eq!(previous, &value as *const u64);

    let loc = restored
        .disk_location()
        .expect("unswizzled location decodes");
    assert_eq!(loc.block_id, 17);
    assert_eq!(loc.offset, 23);
    assert_eq!(loc.node_type, NodeType::Bucket);
}

#[test]
fn swizzled_pointer_raw_extraction_is_gated_by_in_memory_state() {
    use libdictenstein::persistent_artrie::{NodeType, SwizzledPtr};

    let null = SwizzledPtr::null();
    assert!(!null.is_swizzled());
    assert_eq!(null.as_ptr::<u64>(), None);
    assert_eq!(null.disk_location(), None);

    let slot = SwizzledPtr::on_disk(3, 64, NodeType::Bucket);
    assert!(slot.is_on_disk());
    assert!(!slot.is_swizzled());
    assert_eq!(slot.as_ptr::<u64>(), None);
    assert!(slot.disk_location().is_some());

    let value = Box::new(0xfeed_cafe_dead_beef_u64);
    let raw = value.as_ref() as *const u64;

    slot.swizzle(raw).expect("publish in-memory pointer");
    assert!(slot.is_swizzled());
    assert_eq!(slot.disk_location(), None);

    let safe = slot
        .as_ptr::<u64>()
        .expect("safe extraction succeeds only for in-memory pointer");
    assert_eq!(safe, raw);

    let unchecked = unsafe { slot.as_ptr_unchecked::<u64>() };
    assert_eq!(unchecked, safe);
    assert_eq!(unsafe { *unchecked }, *value);

    let previous = slot
        .unswizzle::<u64>(3, 96, NodeType::Bucket)
        .expect("unpublish in-memory pointer");
    assert_eq!(previous, raw);
    assert!(!slot.is_swizzled());
    assert_eq!(slot.as_ptr::<u64>(), None);
    assert!(slot.disk_location().is_some());
}

#[test]
fn swizzled_pointer_losing_lazy_load_candidate_can_be_reclaimed_once() {
    use libdictenstein::persistent_artrie::error::SwizzleError;
    use libdictenstein::persistent_artrie::{NodeType, SwizzledPtr};

    let slot = SwizzledPtr::on_disk(7, 4096, NodeType::CharNode4);
    let winner = Box::into_raw(Box::new(41_u64));
    let loser = Box::into_raw(Box::new(99_u64));

    slot.swizzle(winner).expect("winner publishes loaded node");
    assert_eq!(
        slot.swizzle(loser),
        Err(SwizzleError::AlreadySwizzled),
        "losing lazy-load candidate must stay unpublished"
    );

    let recovered_loser = unsafe { Box::from_raw(loser) };
    assert_eq!(*recovered_loser, 99);

    let published = slot.as_ptr::<u64>().expect("published pointer");
    assert_eq!(published, winner as *const u64);
    let recovered_winner = unsafe { Box::from_raw(winner) };
    assert_eq!(*recovered_winner, 41);
}

#[test]
fn swizzled_pointer_null_initialization_has_single_cas_winner() {
    use libdictenstein::persistent_artrie::{NodeType, SwizzledPtr};

    let slot = Arc::new(SwizzledPtr::null());
    let barrier = Arc::new(Barrier::new(9));
    let mut handles = Vec::new();

    for thread_id in 0..8 {
        let slot = Arc::clone(&slot);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            let candidate =
                SwizzledPtr::on_disk(thread_id + 1, (thread_id + 1) * 16, NodeType::Node4);
            barrier.wait();
            slot.try_insert_child(&candidate).is_ok()
        }));
    }

    barrier.wait();
    let winners = handles
        .into_iter()
        .map(|handle| handle.join().expect("CAS thread completed"))
        .filter(|won| *won)
        .count();

    assert_eq!(winners, 1, "exactly one null-to-child CAS may succeed");

    let loc = slot
        .disk_location()
        .expect("winner installed a disk pointer");
    assert!((1..=8).contains(&loc.block_id));
    assert_eq!(loc.offset, loc.block_id * 16);
    assert_eq!(loc.node_type, NodeType::Node4);
}

#[test]
fn atomic_node_ptr_successful_cas_releases_replaced_slot_reference() {
    use libdictenstein::persistent_artrie::nodes::atomic_ptr::AtomicNodePtr;
    use libdictenstein::persistent_artrie::nodes::persistent_node::{Child, PersistentNode};
    use libdictenstein::persistent_artrie::{NodeType, SwizzledPtr};

    // G4: bare `PersistentNode::new()` no longer infers `V`; this CAS-semantics
    // test exercises the default `<()>` membership node.
    let initial = Arc::new(PersistentNode::<()>::new());
    let ptr = AtomicNodePtr::new(Arc::clone(&initial));
    assert_eq!(Arc::strong_count(&initial), 2);

    let expected = ptr.load().expect("initial pointer loads");
    assert_eq!(Arc::strong_count(&initial), 3);

    let replacement = Arc::new(expected.with_child(
        b'a',
        Child::OnDisk(SwizzledPtr::on_disk(1, 64, NodeType::Bucket)),
    ));
    let replaced = ptr
        .compare_exchange(&expected, replacement)
        .expect("CAS from loaded expected succeeds");

    assert_eq!(
        Arc::strong_count(&initial),
        3,
        "the pointer slot released its old Arc ownership and returned one owned Arc"
    );
    drop(replaced);
    assert_eq!(Arc::strong_count(&initial), 2);
    drop(expected);
    assert_eq!(Arc::strong_count(&initial), 1);

    let final_node = ptr.load().expect("replacement pointer loads");
    assert_eq!(final_node.num_children(), 1);
}

#[test]
fn atomic_node_ptr_concurrent_cas_has_single_visible_replacement() {
    use libdictenstein::persistent_artrie::nodes::atomic_ptr::AtomicNodePtr;
    use libdictenstein::persistent_artrie::nodes::persistent_node::{Child, PersistentNode};
    use libdictenstein::persistent_artrie::{NodeType, SwizzledPtr};

    // G4: pin the default `<()>` membership node (bare `::new()` no longer infers V).
    let ptr = Arc::new(AtomicNodePtr::new(Arc::new(PersistentNode::<()>::new())));
    let barrier = Arc::new(Barrier::new(9));
    let mut handles = Vec::new();

    for label in b'a'..=b'h' {
        let ptr = Arc::clone(&ptr);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            let expected = ptr.load().expect("initial pointer loads before race");
            let replacement = Arc::new(expected.with_child(
                label,
                Child::OnDisk(SwizzledPtr::on_disk(
                    u32::from(label),
                    128,
                    NodeType::Bucket,
                )),
            ));
            barrier.wait();
            ptr.compare_exchange(&expected, replacement).is_ok()
        }));
    }

    barrier.wait();
    let winners = handles
        .into_iter()
        .map(|handle| handle.join().expect("CAS thread completed"))
        .filter(|won| *won)
        .count();

    assert_eq!(
        winners, 1,
        "exactly one expected-to-replacement CAS may succeed"
    );
    assert_eq!(
        ptr.load().expect("final pointer loads").num_children(),
        1,
        "the winning replacement is the only visible child update"
    );
}

#[test]
fn optimistic_cell_concurrent_writes_preserve_version_and_value() {
    use libdictenstein::persistent_artrie::OptimisticCell;

    let cell = Arc::new(OptimisticCell::new(0usize));
    let barrier = Arc::new(Barrier::new(9));
    let mut handles = Vec::new();

    for _ in 0..8 {
        let cell = Arc::clone(&cell);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            for _ in 0..128 {
                cell.write(|value| *value += 1);
            }
        }));
    }

    barrier.wait();
    for handle in handles {
        handle.join().expect("writer completed");
    }

    assert_eq!(cell.read_with_retry(|value| *value, 16), Some(1024));
    assert_eq!(
        cell.version(),
        2048,
        "each write has begin/end version steps"
    );
    assert!(!cell.is_locked());
}

#[test]
fn persistent_reopen_ignores_torn_wal_header_after_durable_prefix() {
    let temp_dir = TempDir::new().expect("temp dir");
    let path = temp_dir.path().join("durable_prefix.part");
    let wal_path = path.with_extension("wal");
    let mut expected = BTreeMap::new();
    let mut seen = BTreeSet::new();

    {
        let dict = PersistentARTrie::<i32>::create(&path).expect("create trie");
        for (term, value) in [("alpha", 1), ("beta", 2), ("gamma", 3)] {
            dict.insert_with_value(term, value);
            expected.insert(term.to_string(), value);
            seen.insert(term.to_string());
        }
        dict.remove("beta");
        expected.remove("beta");
        dict.sync().expect("sync durable prefix");
    }

    let mut wal = OpenOptions::new()
        .append(true)
        .open(&wal_path)
        .expect("open WAL for torn append");
    wal.write_all(&[0x99, 0x88, 0x77])
        .expect("append partial trailing WAL header");
    wal.sync_all().expect("sync torn WAL tail");

    let reopened = PersistentARTrie::<i32>::open(&path).expect("reopen after torn tail");
    assert_trie_matches_reference(&reopened, &expected, &seen);
}

#[test]
fn persistent_reopen_matches_every_wal_record_boundary_prefix() {
    let temp_dir = TempDir::new().expect("temp dir");
    let base_path = temp_dir.path().join("empty_base.part");

    {
        let dict = PersistentARTrie::<i32>::create(&base_path).expect("create empty base");
        dict.sync().expect("sync empty base");
    }

    let records = crash_prefix_records();
    let wal_bytes = write_wal_fixture(&temp_dir.path().join("prefix_fixture.wal"), &records);
    let spans = wal_record_spans(&wal_bytes);
    assert_eq!(spans.len(), records.len());

    let expectations = crash_prefix_expectations();
    let seen = crash_prefix_seen_terms();

    for prefix_len in 0..=records.len() {
        let cut = if prefix_len == 0 {
            WalHeader::SIZE
        } else {
            spans[prefix_len - 1].1
        };
        let reopened = copy_base_with_wal_bytes(
            &base_path,
            temp_dir.path(),
            &format!("record_prefix_{prefix_len}"),
            &wal_bytes[..cut],
        );

        assert_trie_matches_reference(&reopened, &expectations[prefix_len], &seen);
    }
}

#[test]
fn persistent_reopen_ignores_torn_wal_payload_after_durable_prefix() {
    let temp_dir = TempDir::new().expect("temp dir");
    let base_path = temp_dir.path().join("empty_for_torn_payload.part");

    {
        let dict = PersistentARTrie::<i32>::create(&base_path).expect("create empty base");
        dict.sync().expect("sync empty base");
    }

    let records = crash_prefix_records();
    let wal_bytes = write_wal_fixture(&temp_dir.path().join("torn_payload_fixture.wal"), &records);
    let spans = wal_record_spans(&wal_bytes);
    assert_eq!(spans.len(), records.len());

    let torn_record_index = 3;
    let torn_cut = spans[torn_record_index].0 + WalWriter::RECORD_HEADER_SIZE + 2;
    assert!(torn_cut < spans[torn_record_index].1);

    let reopened = copy_base_with_wal_bytes(
        &base_path,
        temp_dir.path(),
        "torn_payload_after_prefix",
        &wal_bytes[..torn_cut],
    );

    let expectations = crash_prefix_expectations();
    let seen = crash_prefix_seen_terms();
    assert_trie_matches_reference(&reopened, &expectations[torn_record_index], &seen);
}

#[test]
fn persistent_reopen_replays_only_committed_wal_transactions() {
    let temp_dir = TempDir::new().expect("temp dir");
    let base_path = temp_dir.path().join("empty_for_tx.part");

    {
        let dict = PersistentARTrie::<i32>::create(&base_path).expect("create empty base");
        dict.sync().expect("sync empty base");
    }

    let incomplete_records = vec![
        wal_insert("outside", 1),
        WalRecord::BeginTx { tx_id: 7 },
        wal_insert("alpha", 10),
        wal_insert("beta", 20),
    ];
    let incomplete_wal = write_wal_fixture(
        &temp_dir.path().join("incomplete_tx_fixture.wal"),
        &incomplete_records,
    );
    let incomplete = copy_base_with_wal_bytes(
        &base_path,
        temp_dir.path(),
        "incomplete_tx",
        &incomplete_wal,
    );
    let incomplete_expected = BTreeMap::from([("outside".to_string(), 1)]);
    let seen = BTreeSet::from([
        "outside".to_string(),
        "alpha".to_string(),
        "beta".to_string(),
    ]);
    assert_trie_matches_reference(&incomplete, &incomplete_expected, &seen);

    let committed_records = vec![
        wal_insert("outside", 1),
        WalRecord::BeginTx { tx_id: 8 },
        wal_insert("alpha", 10),
        wal_insert("beta", 20),
        wal_remove("outside"),
        WalRecord::CommitTx { tx_id: 8 },
    ];
    let committed_wal = write_wal_fixture(
        &temp_dir.path().join("committed_tx_fixture.wal"),
        &committed_records,
    );
    let committed =
        copy_base_with_wal_bytes(&base_path, temp_dir.path(), "committed_tx", &committed_wal);
    let committed_expected = BTreeMap::from([("alpha".to_string(), 10), ("beta".to_string(), 20)]);
    assert_trie_matches_reference(&committed, &committed_expected, &seen);
}

#[test]
fn persistent_dictionary_law_trace_matches_reference_map() {
    let temp_dir = TempDir::new().expect("temp dir");
    let path = temp_dir.path().join("dictionary_laws.part");
    let dict = PersistentARTrie::<i32>::create(&path).expect("create trie");
    let mut expected = BTreeMap::new();
    let mut seen = BTreeSet::new();

    for (term, value) in [
        ("app", 1),
        ("apple", 2),
        ("application", 3),
        ("banana", 4),
        ("band", 5),
    ] {
        assert_eq!(
            dict.insert_with_value(term, value),
            expected.insert(term.to_string(), value).is_none()
        );
        seen.insert(term.to_string());
    }

    assert_eq!(
        dict.insert_with_value("apple", 22),
        expected.insert("apple".to_string(), 22).is_none()
    );
    assert_eq!(dict.get_value("apple"), Some(22));

    assert_eq!(dict.remove("app"), expected.remove("app").is_some());
    assert!(!dict.remove("absent"));
    assert!(dict.contains("application"));
    assert!(!dict.contains("app"));

    let removed = dict.remove_prefix(b"ban");
    let reference_removed: Vec<_> = expected
        .keys()
        .filter(|term| term.starts_with("ban"))
        .cloned()
        .collect();
    for term in &reference_removed {
        expected.remove(term);
        seen.insert(term.clone());
    }

    assert_eq!(removed, reference_removed.len());
    assert_trie_matches_reference(&dict, &expected, &seen);
}

#[cfg(feature = "group-commit")]
#[test]
fn group_commit_writes_returned_lsns_in_wal_order() {
    use libdictenstein::persistent_artrie::{
        AsyncWalConfig, AsyncWalWriter, GroupCommitConfig, GroupCommitCoordinator, WalConfig,
    };

    let temp_dir = TempDir::new().expect("temp dir");
    let wal_path = temp_dir.path().join("group_commit.wal");
    let async_config = AsyncWalConfig::with_pending_dir(temp_dir.path().join("pending"));
    let archive_config = WalConfig {
        archive_dir: temp_dir.path().join("archive"),
        ..Default::default()
    };
    let wal = Arc::new(
        AsyncWalWriter::create(&wal_path, async_config, archive_config).expect("create async WAL"),
    );
    let coordinator = GroupCommitCoordinator::new(
        Arc::clone(&wal),
        GroupCommitConfig {
            max_batch_size: 3,
            max_batch_delay_us: 100_000,
            dedicated_commit_thread: true,
            adaptive_batching: false,
            ..Default::default()
        },
    )
    .expect("create group commit coordinator");

    let mut returned_lsns = Vec::new();
    for term in ["alpha", "beta", "gamma"] {
        returned_lsns.push(
            coordinator
                .append_async(WalRecord::Insert {
                    term: term.as_bytes().to_vec(),
                    value: None,
                })
                .expect("queue WAL record"),
        );
    }

    let last_lsn = *returned_lsns.last().expect("submitted records");
    coordinator.wait_for_lsn(last_lsn);
    assert!(coordinator.synced_lsn() >= last_lsn);
    assert_eq!(coordinator.stats().records_committed, 3);
    drop(coordinator);
    drop(wal);

    let records: Vec<_> = WalReader::new(&wal_path)
        .expect("open WAL reader")
        .iter()
        .collect::<Result<_, _>>()
        .expect("read WAL records");
    let durable_lsns: Vec<_> = records.iter().map(|(lsn, _)| *lsn).collect();
    let durable_terms: Vec<_> = records
        .iter()
        .map(|(_, record)| match record {
            WalRecord::Insert { term, .. } => String::from_utf8(term.clone()).expect("UTF-8 term"),
            other => panic!("unexpected WAL record: {:?}", other),
        })
        .collect();

    assert_eq!(durable_lsns, returned_lsns);
    assert_eq!(durable_terms, vec!["alpha", "beta", "gamma"]);
}
