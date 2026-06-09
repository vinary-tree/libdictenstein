//! **L2.1 — CX-compacted byte image reopens losslessly through the FULL `open()` path.**
//!
//! L2.1 flips byte `compact()` onto the CX path-compressing serializer
//! (`serialize_overlay_snapshot_compressed`), which emits **node-header prefix** chunks. The
//! pre-existing `cx_roundtrip_*` in-crate tests only read the compressed image back via the
//! overlay fault loader (`load_overlay_node_from_disk`); they NEVER exercised the production
//! reopen (`open()` → F5 `load_root_immutable` → `build_overlay_root_from_owned`), which routes
//! through the OWNED readers. Those readers were prefix-BLIND, so a CX-compacted term lost its
//! chunk prefix on reopen (e.g. "single" → "se", value dropped). These tests pin the fixed
//! behavior end-to-end: compact → DROP → cold `open()` → every term + value survives byte-exact.
//!
//! Scratch is REAL disk (`target/test-tmp`), never tmpfs.
#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::{CompactionConfig, PersistentARTrie};
use libdictenstein::Dictionary;
use std::collections::BTreeMap;

fn scratch(tag: &str) -> tempfile::TempDir {
    std::fs::create_dir_all("target/test-tmp").ok();
    tempfile::Builder::new()
        .prefix(tag)
        .tempdir_in("target/test-tmp")
        .expect("real-disk scratch under target/test-tmp")
}

/// A normalized snapshot of a trie's observable state: every term mapped to its value
/// (`None` for a term-only member), plus the term count.
fn snapshot(trie: &PersistentARTrie<u64>) -> (Option<usize>, BTreeMap<String, Option<u64>>) {
    let mut entries = BTreeMap::new();
    // The inherent byte iterator enumerates EVERY term as raw bytes; these str tests use
    // UTF-8 terms, so decode each. (Reading values via `get_value_bytes` keeps the snapshot
    // on the byte API end-to-end, independent of the str routing.)
    let terms: Vec<Vec<u8>> = match trie.iter_prefix(b"".as_slice()) {
        Some(it) => it.collect(),
        None => Vec::new(),
    };
    for term in terms {
        let value = trie.get_value_bytes(&term);
        let key = String::from_utf8(term).expect("utf8 term in snapshot");
        entries.insert(key, value);
    }
    (trie.len(), entries)
}

/// Build a `u64` trie from `(term, Option<value>)` pairs (None ⇒ a term-only member),
/// checkpoint it, then compact IN PLACE (CX path, since an eligible-`V` trie is
/// overlay-routed after `create`). Returns the path's directory + the path so the caller
/// can DROP and cold-`open()`.
fn build_and_compact(
    tag: &str,
    entries: &[(&str, Option<u64>)],
) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = scratch(tag);
    let path = dir.path().join("t.artb");
    {
        let mut trie = PersistentARTrie::<u64>::create(&path).expect("create");
        for (term, value) in entries {
            let ok = match value {
                Some(v) => trie.insert_with_value(term, *v),
                None => trie.insert(term),
            };
            assert!(ok, "insert {term:?}");
        }
        trie.sync().expect("sync");
        trie.checkpoint().expect("checkpoint");

        // Capture the live (pre-compaction) snapshot for the equivalence assertion.
        let (live_len, live) = snapshot(&trie);

        // Compact IN PLACE — exercises the CX serializer + the `*self = open()` reopen.
        trie.compact(CompactionConfig::default(), |_| {})
            .expect("compact");

        // After in-place compaction `trie` IS the reopened compacted trie.
        let (post_len, post) = snapshot(&trie);
        assert_eq!(
            live_len, post_len,
            "{tag}: term count changed across in-place compaction"
        );
        assert_eq!(
            live, post,
            "{tag}: term-set/values changed across in-place compaction"
        );
    }
    (dir, path)
}

/// The expected snapshot from the input pairs (for the cold-reopen assertion).
fn expected(entries: &[(&str, Option<u64>)]) -> BTreeMap<String, Option<u64>> {
    entries.iter().map(|(t, v)| (t.to_string(), *v)).collect()
}

/// Cold reopen the compacted file in a FRESH handle and assert it equals `want`.
fn assert_cold_reopen(path: &std::path::Path, want: &BTreeMap<String, Option<u64>>, tag: &str) {
    let reopened = PersistentARTrie::<u64>::open(path).expect("cold open");
    let (len, got) = snapshot(&reopened);
    assert_eq!(len, Some(want.len()), "{tag}: cold-reopen len");
    assert_eq!(&got, want, "{tag}: cold-reopen term-set/values");
}

/// The exact bug case: a single compressible chain ("single" ⇒ one chunk node, prefix "ingl")
/// carrying a value MUST reopen with its value intact (the bug reopened it as `Some(None)`).
/// Mixes valued + term-only members so the membership∪value union is exercised.
#[test]
fn l21_cx_reopen_byte_equivalence() {
    let entries = &[
        ("single", Some(42u64)),
        ("member", None), // term-only (membership, no value)
        ("singleton", Some(7)),
        ("si", Some(1)), // a proper prefix of "single"/"singleton"
    ];
    let (_dir, path) = build_and_compact("l21-equiv", entries);
    assert_cold_reopen(&path, &expected(entries), "l21-equiv");
}

/// A chain LONGER than `MAX_PREFIX_LEN` (=12) forces ≥2 chunk nodes; the reconstruction must
/// not drop or duplicate a unit at the seam between one chunk's edge and the next chunk's
/// prefix. Includes chunk-boundary-aligned lengths (13, 14, 25, 26, 40) per the red-team's
/// seam-correctness concern, all carrying distinct values.
#[test]
fn l21_cx_reopen_long_chain_multi_chunk() {
    let alphabet = "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMN"; // 50 chars
                                                                         // Distinct single chains of boundary-relevant lengths (no shared prefixes ⇒ each is a
                                                                         // pure single-child chunk chain to its terminus).
    let lengths = [13usize, 14, 25, 26, 40];
    let owned: Vec<(String, Option<u64>)> = lengths
        .iter()
        .enumerate()
        .map(|(i, &len)| {
            // Distinct first byte per term so they don't share a prefix.
            let first = (b'A' + i as u8) as char;
            let body: String = alphabet.chars().take(len.saturating_sub(1)).collect();
            (format!("{first}{body}"), Some(1000 + i as u64))
        })
        .collect();
    let entries: Vec<(&str, Option<u64>)> = owned.iter().map(|(t, v)| (t.as_str(), *v)).collect();

    let (_dir, path) = build_and_compact("l21-longchain", &entries);
    assert_cold_reopen(&path, &expected(&entries), "l21-longchain");
}

/// The empty term "" (valued and the term-only forms) + a normal term must survive the CX
/// compaction reopen — exercises the `compact_publish_compressed_overlay` root-finality /
/// `ROOT_TYPE_BUCKET` path, which is separate from the `_under_child` prefix fold.
#[test]
fn l21_cx_reopen_empty_term_valued() {
    let entries = &[("", Some(999u64)), ("alpha", Some(1)), ("beta", None)];
    let (_dir, path) = build_and_compact("l21-empty-valued", entries);
    assert_cold_reopen(&path, &expected(entries), "l21-empty-valued");
}

#[test]
fn l21_cx_reopen_empty_term_membership() {
    let entries = &[("", None), ("gamma", Some(3))];
    let (_dir, path) = build_and_compact("l21-empty-member", entries);
    assert_cold_reopen(&path, &expected(entries), "l21-empty-member");
}

/// Branching terms sharing prefixes + siblings + a long chain BELOW a final branch node —
/// the structural shape the in-crate `cx_roundtrip_branching_and_shared_prefix` covers, but
/// here through the FULL `open()` reopen with VALUES.
#[test]
fn l21_cx_reopen_branching_and_shared_prefix() {
    let entries = &[
        ("a", Some(1u64)),
        ("ab", Some(2)),
        ("abc", Some(3)),
        ("abd", None), // term-only sibling
        ("b", Some(5)),
        ("bcdefghijklmnop", Some(6)), // long chain below "b"
        ("xyz", Some(7)),
    ];
    let (_dir, path) = build_and_compact("l21-branch", entries);
    assert_cold_reopen(&path, &expected(entries), "l21-branch");
}

/// The output-file (NOT in-place) compaction path must ALSO reopen losslessly — it publishes
/// the CX image to a separate file the caller then opens.
#[test]
fn l21_cx_reopen_to_new_file() {
    let entries = &[
        ("single", Some(42u64)),
        ("singular", Some(8)),
        ("plural", None),
    ];
    let dir = scratch("l21-newfile");
    let src_path = dir.path().join("src.artb");
    let out_path = dir.path().join("out.artb");
    {
        let mut trie = PersistentARTrie::<u64>::create(&src_path).expect("create");
        for (term, value) in entries {
            let ok = match value {
                Some(v) => trie.insert_with_value(term, *v),
                None => trie.insert(term),
            };
            assert!(ok, "insert {term:?}");
        }
        trie.sync().expect("sync");
        trie.checkpoint().expect("checkpoint");
        let config = CompactionConfig {
            output_path: Some(out_path.clone()),
            progress_interval: 10,
            verify_after_compact: true,
        };
        trie.compact(config, |_| {}).expect("compact to new file");
    }
    assert_cold_reopen(&out_path, &expected(entries), "l21-newfile");
}

/// Non-UTF-8 byte keys (the byte trie is `u8`-keyed, not `str`) survive the CX reopen — guards
/// against any UTF-8 assumption sneaking into the prefix fold.
#[test]
fn l21_cx_reopen_non_utf8_keys() {
    let dir = scratch("l21-nonutf8");
    let path = dir.path().join("t.artb");
    let keys: &[(&[u8], u64)] = &[
        (&[0xFF, 0x00, 0x80, 0x12, 0x34, 0x56, 0x78], 1),
        (&[0xFF, 0x00, 0x80, 0x12, 0x34, 0x56, 0x79], 2), // shares a 6-byte prefix ⇒ chunk
        (&[0x00], 3),
    ];
    {
        let mut trie = PersistentARTrie::<u64>::create(&path).expect("create");
        let batch: Vec<(&[u8], Option<u64>)> = keys.iter().map(|(k, v)| (*k, Some(*v))).collect();
        assert_eq!(
            trie.insert_batch_bytes(&batch),
            keys.len(),
            "all byte keys inserted"
        );
        trie.sync().expect("sync");
        trie.checkpoint().expect("checkpoint");
        trie.compact(CompactionConfig::default(), |_| {})
            .expect("compact");
        for (k, v) in keys {
            assert_eq!(
                trie.get_value_bytes(k),
                Some(*v),
                "in-place: byte key {k:?} survives"
            );
        }
    }
    let reopened = PersistentARTrie::<u64>::open(&path).expect("cold open");
    for (k, v) in keys {
        assert_eq!(
            reopened.get_value_bytes(k),
            Some(*v),
            "cold-reopen: byte key {k:?} survives"
        );
    }
    assert_eq!(Dictionary::len(&reopened), Some(keys.len()));
}
