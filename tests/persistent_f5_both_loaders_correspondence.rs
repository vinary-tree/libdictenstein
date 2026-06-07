//! **Slice 3 / F5 — the both-loaders correspondence gate.**
//!
//! F5 adds a direct dense→overlay reopen loader (`load_root_immutable` +
//! `replay_records_lww_overlay`) ALONGSIDE the legacy owned-loader→reestablish reopen
//! path. This suite is the F5 GATE (must pass before the S3 switch): it proves the two
//! loaders reopen to IDENTICAL observable state for byte + char × `V ∈ {(), counter,
//! String, a small struct}` × mixed {valued, term-only, empty-""} entries, on a
//! deep (~100k-unit) key, and on a crash-without-checkpoint WAL tail (with the
//! unranked-drop negative control).
//!
//! The two paths are reached via the GATE-INDEPENDENT ctors `open_with_legacy_loader`
//! (forces the owned-loader→reestablish path) and `open_with_f5_loader` (forces the
//! direct dense→overlay F5 path), so this suite is a meaningful legacy-vs-F5 oracle
//! whether [`LockFreeOverlay::USE_F5_REOPEN_LOADER`] is ON (S3, current) or OFF (S1/S2).
//! The production `open` follows the gate; after the S3 switch it uses F5.
//!
//! Real-disk scratch under `target/test-tmp/` (NOT tmpfs `tempdir()`), per the
//! project's disk-backed-test discipline.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::PersistentARTrie;
use libdictenstein::persistent_artrie_char::PersistentARTrieChar;
use libdictenstein::persistent_artrie_core::wal::{RankRegime, WalReader};
use libdictenstein::value::DictionaryValue;
use libdictenstein::{Dictionary, MappedDictionary};
use proptest::prelude::*;
use serde::{Deserialize, Serialize};

fn scratch(tag: &str) -> tempfile::TempDir {
    std::fs::create_dir_all("target/test-tmp").ok();
    tempfile::Builder::new()
        .prefix(tag)
        .tempdir_in("target/test-tmp")
        .expect("real-disk scratch under target/test-tmp")
}

/// A small `derive`-everything value struct (the proptest's "arbitrary struct V").
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Small {
    a: u32,
    b: String,
    c: bool,
}

// `DictionaryValue` is not blanket-impl'd; opt `Small` in (all super-bounds —
// Clone/Default/Send/Sync/Unpin/'static/Serialize/DeserializeOwned — are derived).
impl DictionaryValue for Small {}

/// One entry in a proptest term-set: a value, a term-only membership, or removed.
#[derive(Clone, Debug)]
enum Entry<V> {
    Valued(V),
    Member,
}

/// A normalized snapshot of a trie's OBSERVABLE state, loader-independent:
/// the term count, every term (as raw key bytes, sorted), and per-term the bincode of
/// its value (or `None` for a term-only member). Two loaders correspond iff their
/// snapshots are equal.
#[derive(Debug, PartialEq)]
struct Snapshot {
    len: Option<usize>,
    /// (term-bytes → bincode(value) or None for membership), sorted by term.
    entries: std::collections::BTreeMap<Vec<u8>, Option<Vec<u8>>>,
}

// ============================================================================
// Per-variant probes: build / reopen-legacy / reopen-f5 / snapshot. The byte
// `insert*` return `bool`; char return `Result<bool>` — so each variant gets its own
// small closure set, and the shared `assert_corresponds` compares the normalized
// `Snapshot`s.
// ============================================================================

/// Assert the WAL file is stamped Overlay (so the F5 loader actually runs its F5 arm —
/// F5 is a NO-OP fallthrough for an Owned-regime file). Eligible-`V` `create()`
/// auto-flips to Overlay, so this holds for every trie this suite builds.
fn assert_overlay_regime(path: &std::path::Path) {
    let wal = path.with_extension("wal");
    let regime = WalReader::read_header(&wal)
        .map(|h| h.regime())
        .unwrap_or(RankRegime::Owned);
    assert_eq!(
        regime,
        RankRegime::Overlay,
        "F5 requires an Overlay-regime WAL: {wal:?}"
    );
}

/// Compare the legacy-loader and F5-loader reopens of the SAME built file.
fn assert_corresponds(legacy: &Snapshot, f5: &Snapshot, ctx: &str) {
    assert_eq!(
        legacy.len, f5.len,
        "{ctx}: len differs (legacy {:?} vs F5 {:?})",
        legacy.len, f5.len
    );
    assert_eq!(
        legacy.entries, f5.entries,
        "{ctx}: term-set / values differ between legacy and F5 loaders"
    );
}

// ---- char probe ----

fn char_build<V: libdictenstein::value::DictionaryValue + Serialize>(
    path: &std::path::Path,
    entries: &[(String, Entry<V>)],
    checkpoint: bool,
) {
    let trie = PersistentARTrieChar::<V>::create(path).expect("char create");
    for (term, entry) in entries {
        match entry {
            Entry::Valued(v) => {
                trie.insert_with_value(term, v.clone())
                    .expect("char insert_with_value");
            }
            Entry::Member => {
                trie.insert(term).expect("char insert");
            }
        }
    }
    trie.sync().expect("char sync");
    if checkpoint {
        trie.checkpoint().expect("char checkpoint");
    }
}

fn char_snapshot<V: DictionaryValue + Serialize>(trie: &PersistentARTrieChar<V>) -> Snapshot {
    let mut entries = std::collections::BTreeMap::new();
    let terms = trie
        .iter_prefix("")
        .expect("char iter_prefix")
        .unwrap_or_default();
    for term in terms {
        let key = term.as_bytes().to_vec();
        // `get_value` is `Some` for a valued term, `None` for a term-only member.
        let val_bytes = MappedDictionary::get_value(trie, &term)
            .map(|v| libdictenstein::serialization::bincode_compat::serialize(&v).expect("ser"));
        entries.insert(key, val_bytes);
    }
    Snapshot {
        // The trie's own `len()` (the `Dictionary` trait method, `Option<usize>`).
        len: Dictionary::len(trie),
        entries,
    }
}

// ---- byte probe ----

fn byte_build<V: libdictenstein::value::DictionaryValue + Serialize>(
    path: &std::path::Path,
    entries: &[(String, Entry<V>)],
    checkpoint: bool,
) {
    let trie = PersistentARTrie::<V>::create(path).expect("byte create");
    for (term, entry) in entries {
        match entry {
            Entry::Valued(v) => {
                trie.insert_with_value(term, v.clone());
            }
            Entry::Member => {
                trie.insert(term);
            }
        }
    }
    trie.sync().expect("byte sync");
    if checkpoint {
        trie.checkpoint().expect("byte checkpoint");
    }
}

fn byte_snapshot<V: DictionaryValue + Serialize>(trie: &PersistentARTrie<V>) -> Snapshot {
    let mut entries = std::collections::BTreeMap::new();
    // Byte `iter_prefix(&[u8])` yields `Vec<u8>` terms. The empty prefix enumerates all.
    if let Some(iter) = trie.iter_prefix(b"") {
        for term in iter {
            let term_str = String::from_utf8_lossy(&term).to_string();
            let val_bytes = MappedDictionary::get_value(trie, &term_str).map(|v| {
                libdictenstein::serialization::bincode_compat::serialize(&v).expect("ser")
            });
            entries.insert(term, val_bytes);
        }
    }
    Snapshot {
        // The trie's own `len()` (the `Dictionary` trait method, NOT the inherent
        // `usize` one — UFCS disambiguates).
        len: Dictionary::len(trie),
        entries,
    }
}

// ============================================================================
// The proptest — byte + char × V ∈ {(), counter, String, Small} × mixed entries.
// ============================================================================

/// A proptest strategy: a small set of (term, Entry) over the value generator `vg`,
/// INCLUDING a mix of valued, term-only members, and the empty term "". Used for the
/// general (`()`, `String`, struct) V types — whose reestablish folds handle term-only
/// members correctly.
fn entries_strategy<V: Clone + std::fmt::Debug + 'static>(
    vg: impl Strategy<Value = V> + Clone + 'static,
) -> impl Strategy<Value = Vec<(String, Entry<V>)>> {
    let term = prop::collection::vec(prop::sample::select(vec!['a', 'b', 'c', 'é', '東']), 0..6)
        .prop_map(|cs| cs.into_iter().collect::<String>());
    let entry = prop_oneof![vg.clone().prop_map(Entry::Valued), Just(Entry::Member)];
    // A map (term → entry) so a term appears at most once (last-writer is the map value);
    // this keeps the build deterministic for the legacy-vs-F5 comparison.
    prop::collection::btree_map(term, entry, 0..12).prop_map(|m| m.into_iter().collect::<Vec<_>>())
}

/// **Valued-only** variant of [`entries_strategy`] for the COUNTER V types (`u64`/`i64`).
///
/// A term-only MEMBER on a counter trie (`insert(t)` with no value) is a degenerate case
/// the LEGACY counter reestablish fold (`reestablish_overlay_counter`) DROPS on reopen —
/// it only republishes terms carrying a value (see the documenting test
/// `counter_term_only_member_f5_keeps_what_legacy_drops`). F5 keeps it (strictly more
/// correct). To keep this gate a clean F5≡legacy oracle on the SHARED-correct domain, the
/// counter proptest generates valued entries only (incl. the empty term "" with a value).
/// The `()`/`String`/struct proptests above exercise term-only members + empty-member
/// exhaustively (those folds are correct).
fn valued_entries_strategy<V: Clone + std::fmt::Debug + 'static>(
    vg: impl Strategy<Value = V> + Clone + 'static,
) -> impl Strategy<Value = Vec<(String, Entry<V>)>> {
    let term = prop::collection::vec(prop::sample::select(vec!['a', 'b', 'c', 'é', '東']), 0..6)
        .prop_map(|cs| cs.into_iter().collect::<String>());
    let entry = vg.clone().prop_map(Entry::Valued);
    prop::collection::btree_map(term, entry, 0..12).prop_map(|m| m.into_iter().collect::<Vec<_>>())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// char × `()` (membership-only V).
    #[test]
    fn char_unit_both_loaders_correspond(entries in entries_strategy(Just(()))) {
        let dir = scratch("f5-char-unit");
        let path = dir.path().join("t.artc");
        char_build::<()>(&path, &entries, /* checkpoint */ true);
        assert_overlay_regime(&path);
        let legacy = char_snapshot(&PersistentARTrieChar::<()>::open_with_legacy_loader(&path).expect("legacy open"));
        let f5 = char_snapshot(&PersistentARTrieChar::<()>::open_with_f5_loader(&path).expect("f5 open"));
        assert_corresponds(&legacy, &f5, "char<()>");
    }

    /// char × `u64` (the char counter monomorph). Valued-only (see
    /// `valued_entries_strategy`); values bounded to the counter domain.
    #[test]
    fn char_u64_both_loaders_correspond(entries in valued_entries_strategy(0u64..1_000_000)) {
        let dir = scratch("f5-char-u64");
        let path = dir.path().join("t.artc");
        char_build::<u64>(&path, &entries, true);
        assert_overlay_regime(&path);
        let legacy = char_snapshot(&PersistentARTrieChar::<u64>::open_with_legacy_loader(&path).expect("legacy open"));
        let f5 = char_snapshot(&PersistentARTrieChar::<u64>::open_with_f5_loader(&path).expect("f5 open"));
        assert_corresponds(&legacy, &f5, "char<u64>");
    }

    /// char × `String` (an arbitrary non-counter V).
    #[test]
    fn char_string_both_loaders_correspond(entries in entries_strategy("[a-c]{0,4}".prop_map(String::from))) {
        let dir = scratch("f5-char-string");
        let path = dir.path().join("t.artc");
        char_build::<String>(&path, &entries, true);
        assert_overlay_regime(&path);
        let legacy = char_snapshot(&PersistentARTrieChar::<String>::open_with_legacy_loader(&path).expect("legacy open"));
        let f5 = char_snapshot(&PersistentARTrieChar::<String>::open_with_f5_loader(&path).expect("f5 open"));
        assert_corresponds(&legacy, &f5, "char<String>");
    }

    /// char × `Small` (an arbitrary derive-everything struct V).
    #[test]
    fn char_struct_both_loaders_correspond(
        entries in entries_strategy(
            (any::<u32>(), "[a-c]{0,3}".prop_map(String::from), any::<bool>())
                .prop_map(|(a, b, c)| Small { a, b, c })
        )
    ) {
        let dir = scratch("f5-char-struct");
        let path = dir.path().join("t.artc");
        char_build::<Small>(&path, &entries, true);
        assert_overlay_regime(&path);
        let legacy = char_snapshot(&PersistentARTrieChar::<Small>::open_with_legacy_loader(&path).expect("legacy open"));
        let f5 = char_snapshot(&PersistentARTrieChar::<Small>::open_with_f5_loader(&path).expect("f5 open"));
        assert_corresponds(&legacy, &f5, "char<Small>");
    }

    /// byte × `()` (membership-only V).
    #[test]
    fn byte_unit_both_loaders_correspond(entries in entries_strategy(Just(()))) {
        let dir = scratch("f5-byte-unit");
        let path = dir.path().join("t.part");
        byte_build::<()>(&path, &entries, true);
        assert_overlay_regime(&path);
        let legacy = byte_snapshot(&PersistentARTrie::<()>::open_with_legacy_loader(&path).expect("legacy open"));
        let f5 = byte_snapshot(&PersistentARTrie::<()>::open_with_f5_loader(&path).expect("f5 open"));
        assert_corresponds(&legacy, &f5, "byte<()>");
    }

    /// byte × `i64` (the byte counter monomorph). Valued-only (see
    /// `valued_entries_strategy`).
    #[test]
    fn byte_i64_both_loaders_correspond(entries in valued_entries_strategy(0i64..1_000_000)) {
        let dir = scratch("f5-byte-i64");
        let path = dir.path().join("t.part");
        byte_build::<i64>(&path, &entries, true);
        assert_overlay_regime(&path);
        let legacy = byte_snapshot(&PersistentARTrie::<i64>::open_with_legacy_loader(&path).expect("legacy open"));
        let f5 = byte_snapshot(&PersistentARTrie::<i64>::open_with_f5_loader(&path).expect("f5 open"));
        assert_corresponds(&legacy, &f5, "byte<i64>");
    }

    /// byte × `String`.
    #[test]
    fn byte_string_both_loaders_correspond(entries in entries_strategy("[a-c]{0,4}".prop_map(String::from))) {
        let dir = scratch("f5-byte-string");
        let path = dir.path().join("t.part");
        byte_build::<String>(&path, &entries, true);
        assert_overlay_regime(&path);
        let legacy = byte_snapshot(&PersistentARTrie::<String>::open_with_legacy_loader(&path).expect("legacy open"));
        let f5 = byte_snapshot(&PersistentARTrie::<String>::open_with_f5_loader(&path).expect("f5 open"));
        assert_corresponds(&legacy, &f5, "byte<String>");
    }

    /// byte × `Small`.
    #[test]
    fn byte_struct_both_loaders_correspond(
        entries in entries_strategy(
            (any::<u32>(), "[a-c]{0,3}".prop_map(String::from), any::<bool>())
                .prop_map(|(a, b, c)| Small { a, b, c })
        )
    ) {
        let dir = scratch("f5-byte-struct");
        let path = dir.path().join("t.part");
        byte_build::<Small>(&path, &entries, true);
        assert_overlay_regime(&path);
        let legacy = byte_snapshot(&PersistentARTrie::<Small>::open_with_legacy_loader(&path).expect("legacy open"));
        let f5 = byte_snapshot(&PersistentARTrie::<Small>::open_with_f5_loader(&path).expect("f5 open"));
        assert_corresponds(&legacy, &f5, "byte<Small>");
    }
}

// ============================================================================
// Deep-term: a single ~100k-unit key must NOT stack-overflow the walk-converter and
// must round-trip identically under both loaders.
// ============================================================================

// The full F5 reopen+read at ~100k units exercises PRE-EXISTING recursive read /
// reestablish / owned-Drop paths (orthogonal to F5 — F5's own walk-converter is
// iterative, proven by the white-box `deep_term_converter_tests` in
// `src/persistent_artrie_char/f5_loader.rs`). Those recursive paths need a large stack
// at this depth, so the end-to-end deep-term cases run on a 512 MiB-stack thread (the
// project's discipline for deep recursive paths). The assertion: F5 reopen does NOT
// overflow (its work-stack converter holds) and round-trips IDENTICALLY to legacy.
const DEEP_STACK: usize = 512 * 1024 * 1024;
const DEEP_UNITS: usize = 100_000;

#[test]
fn char_deep_term_f5_no_stack_overflow_and_roundtrips() {
    std::thread::Builder::new()
        .stack_size(DEEP_STACK)
        .spawn(|| {
            let dir = scratch("f5-char-deep");
            let path = dir.path().join("t.artc");
            // ~100k code points (the overlay spine is 1 node/unit ⇒ ~100k deep).
            let deep: String = std::iter::repeat('a').take(DEEP_UNITS).collect();
            char_build::<u64>(&path, &[(deep.clone(), Entry::Valued(7))], true);
            assert_overlay_regime(&path);
            let legacy = char_snapshot(
                &PersistentARTrieChar::<u64>::open_with_legacy_loader(&path).expect("legacy open"),
            );
            let f5 = char_snapshot(
                &PersistentARTrieChar::<u64>::open_with_f5_loader(&path).expect("f5 open"),
            );
            assert_corresponds(&legacy, &f5, "char deep-term");
            assert_eq!(
                f5.entries.get(deep.as_bytes()),
                Some(&Some(
                    libdictenstein::serialization::bincode_compat::serialize(&7u64).unwrap()
                ))
            );
        })
        .expect("spawn deep-stack thread")
        .join()
        .expect("char deep-term thread");
}

#[test]
fn byte_deep_term_f5_no_stack_overflow_and_roundtrips() {
    std::thread::Builder::new()
        .stack_size(DEEP_STACK)
        .spawn(|| {
            let dir = scratch("f5-byte-deep");
            let path = dir.path().join("t.part");
            let deep: String = std::iter::repeat('a').take(DEEP_UNITS).collect();
            byte_build::<i64>(&path, &[(deep.clone(), Entry::Valued(9))], true);
            assert_overlay_regime(&path);
            let legacy = byte_snapshot(
                &PersistentARTrie::<i64>::open_with_legacy_loader(&path).expect("legacy open"),
            );
            let f5 = byte_snapshot(
                &PersistentARTrie::<i64>::open_with_f5_loader(&path).expect("f5 open"),
            );
            assert_corresponds(&legacy, &f5, "byte deep-term");
            assert_eq!(
                f5.entries.get(deep.as_bytes()),
                Some(&Some(
                    libdictenstein::serialization::bincode_compat::serialize(&9i64).unwrap()
                ))
            );
        })
        .expect("spawn deep-stack thread")
        .join()
        .expect("byte deep-term thread");
}

// ============================================================================
// WAL-tail: write past a checkpoint, then drop WITHOUT a final checkpoint, so the WAL
// has an un-checkpointed tail. F5 reopen must replay the tail INTO THE OVERLAY (no
// value loss) — identical to the legacy reopen.
// ============================================================================

#[test]
fn char_wal_tail_replayed_into_overlay_matches_legacy() {
    let dir = scratch("f5-char-waltail");
    let path = dir.path().join("t.artc");
    {
        let trie = PersistentARTrieChar::<String>::create(&path).expect("create");
        // Checkpointed (dense) state.
        trie.insert_with_value("alpha", "A".to_string())
            .expect("ins");
        trie.insert_with_value("beta", "B".to_string())
            .expect("ins");
        trie.insert_with_value("", "ROOT".to_string())
            .expect("ins ''");
        trie.sync().expect("sync");
        trie.checkpoint().expect("checkpoint");
        // WAL TAIL past the checkpoint — NO final checkpoint.
        trie.insert_with_value("gamma", "G".to_string())
            .expect("ins tail");
        trie.insert_with_value("alpha", "A2".to_string())
            .expect("update tail"); // overwrite
        trie.insert("delta").expect("ins membership tail"); // term-only in the tail
        trie.sync().expect("sync tail");
        // DROP without a final checkpoint.
    }
    assert_overlay_regime(&path);
    let legacy = char_snapshot(
        &PersistentARTrieChar::<String>::open_with_legacy_loader(&path).expect("legacy open"),
    );
    let f5 = char_snapshot(
        &PersistentARTrieChar::<String>::open_with_f5_loader(&path).expect("f5 open"),
    );
    assert_corresponds(&legacy, &f5, "char WAL-tail");
    // Spot-check the tail actually replayed into the F5 overlay (no value loss).
    let bser = |s: &str| {
        Some(libdictenstein::serialization::bincode_compat::serialize(&s.to_string()).unwrap())
    };
    assert_eq!(
        f5.entries.get("gamma".as_bytes()),
        Some(&bser("G")),
        "tail insert replayed"
    );
    assert_eq!(
        f5.entries.get("alpha".as_bytes()),
        Some(&bser("A2")),
        "tail overwrite replayed"
    );
    assert_eq!(
        f5.entries.get("".as_bytes()),
        Some(&bser("ROOT")),
        "checkpointed empty-term survives"
    );
    assert_eq!(
        f5.entries.get("delta".as_bytes()),
        Some(&None),
        "tail term-only membership replayed"
    );
}

#[test]
fn byte_wal_tail_replayed_into_overlay_matches_legacy() {
    let dir = scratch("f5-byte-waltail");
    let path = dir.path().join("t.part");
    {
        let trie = PersistentARTrie::<String>::create(&path).expect("create");
        trie.insert_with_value("alpha", "A".to_string());
        trie.insert_with_value("", "ROOT".to_string());
        trie.sync().expect("sync");
        trie.checkpoint().expect("checkpoint");
        trie.insert_with_value("gamma", "G".to_string());
        trie.insert_with_value("alpha", "A2".to_string());
        trie.insert("delta");
        trie.sync().expect("sync tail");
    }
    assert_overlay_regime(&path);
    let legacy = byte_snapshot(
        &PersistentARTrie::<String>::open_with_legacy_loader(&path).expect("legacy open"),
    );
    let f5 =
        byte_snapshot(&PersistentARTrie::<String>::open_with_f5_loader(&path).expect("f5 open"));
    assert_corresponds(&legacy, &f5, "byte WAL-tail");
    let bser = |s: &str| {
        Some(libdictenstein::serialization::bincode_compat::serialize(&s.to_string()).unwrap())
    };
    assert_eq!(
        f5.entries.get("gamma".as_bytes()),
        Some(&bser("G")),
        "tail insert replayed"
    );
    assert_eq!(
        f5.entries.get("alpha".as_bytes()),
        Some(&bser("A2")),
        "tail overwrite replayed"
    );
    assert_eq!(
        f5.entries.get("".as_bytes()),
        Some(&bser("ROOT")),
        "checkpointed empty-term survives"
    );
    assert_eq!(
        f5.entries.get("delta".as_bytes()),
        Some(&None),
        "tail term-only membership replayed"
    );
}

// ============================================================================
// WAL-tail with a REMOVE: a term in the dense image, removed in the un-checkpointed
// tail, must NOT resurrect in the F5 overlay (the data-loss class F5 must avoid) —
// identical to legacy.
// ============================================================================

#[test]
fn char_wal_tail_remove_does_not_resurrect_under_f5() {
    let dir = scratch("f5-char-tail-remove");
    let path = dir.path().join("t.artc");
    {
        let mut trie = PersistentARTrieChar::<String>::create(&path).expect("create");
        trie.insert_with_value("keep", "K".to_string())
            .expect("ins");
        trie.insert_with_value("drop", "D".to_string())
            .expect("ins");
        trie.sync().expect("sync");
        trie.checkpoint().expect("checkpoint");
        // Remove "drop" in the un-checkpointed tail (durable remove via remove_cas_durable).
        assert!(trie.remove_cas_durable("drop").expect("remove tail"));
        trie.sync().expect("sync tail");
    }
    assert_overlay_regime(&path);
    let legacy = char_snapshot(
        &PersistentARTrieChar::<String>::open_with_legacy_loader(&path).expect("legacy open"),
    );
    let f5 = char_snapshot(
        &PersistentARTrieChar::<String>::open_with_f5_loader(&path).expect("f5 open"),
    );
    assert_corresponds(&legacy, &f5, "char WAL-tail remove");
    assert!(
        f5.entries.contains_key("keep".as_bytes()),
        "kept term present"
    );
    assert!(
        !f5.entries.contains_key("drop".as_bytes()),
        "removed term must NOT resurrect under F5"
    );
}

// ============================================================================
// Negative control: an UNRANKED record under the Overlay regime is DROPPED on F5
// replay (inherited from `reconcile_lww`). We craft an Insert record with NO matching
// CommitRank marker in the WAL tail; the F5 reopen must DROP it (the term is absent),
// matching the legacy reopen. This is the same counter-discipline shape the existing
// recovery tests use.
// ============================================================================

#[test]
fn char_unranked_tail_record_dropped_under_f5_matches_legacy() {
    use libdictenstein::persistent_artrie::{WalRecord, WalWriter};
    let dir = scratch("f5-char-unranked");
    let path = dir.path().join("t.artc");
    // Build a checkpointed file (dense image + Overlay regime), then drop.
    {
        let trie = PersistentARTrieChar::<String>::create(&path).expect("create");
        trie.insert_with_value("acked", "A".to_string())
            .expect("ins");
        trie.sync().expect("sync");
        trie.checkpoint().expect("checkpoint");
    }
    assert_overlay_regime(&path);
    // Append an UNRANKED Insert (NO CommitRank marker) to the WAL tail, after the
    // checkpoint. Under the Overlay regime, `reconcile_lww` DROPS an unranked record
    // (a never-acked two-append-window orphan). The legacy and F5 reopens must AGREE
    // that the orphan is dropped (absent).
    {
        let wal_path = path.with_extension("wal");
        let writer = WalWriter::open(&wal_path).expect("open wal for append");
        let value = libdictenstein::serialization::bincode_compat::serialize(&"ORPHAN".to_string())
            .expect("ser orphan");
        // NO CommitRank for this LSN ⇒ unranked.
        writer
            .append(WalRecord::Insert {
                term: "orphan".as_bytes().to_vec(),
                value: Some(value),
            })
            .expect("append orphan");
        writer.sync().expect("sync orphan");
    }
    let legacy = char_snapshot(
        &PersistentARTrieChar::<String>::open_with_legacy_loader(&path).expect("legacy open"),
    );
    let f5 = char_snapshot(
        &PersistentARTrieChar::<String>::open_with_f5_loader(&path).expect("f5 open"),
    );
    assert_corresponds(&legacy, &f5, "char unranked-drop");
    assert!(
        f5.entries.contains_key("acked".as_bytes()),
        "acked (checkpointed) term present"
    );
    assert!(
        !f5.entries.contains_key("orphan".as_bytes()),
        "an UNRANKED Overlay-regime tail record must be DROPPED under F5 (inherited reconcile_lww drop)"
    );
    assert!(
        !legacy.entries.contains_key("orphan".as_bytes()),
        "the legacy loader also drops the unranked orphan (the oracle)"
    );
}

// ============================================================================
// DOCUMENTED F5 BEHAVIOR — a term-only MEMBER on a COUNTER trie survives reopen.
//
// `insert(t)` (membership, NO value) on a `u64`/`i64` counter trie is a degenerate case
// the OLD legacy counter reestablish fold (`reestablish_overlay_counter`) DROPPED on
// reopen: it republished only terms carrying a value (`owned_units_with_values_under`),
// silently losing the bare member. The WAL records it as `Insert{value:None}`, and F5's
// generic converter (`build_overlay_root_from_owned`, which unions the MEMBERSHIP stream
// with the VALUE stream — the same flag-2 fix the arbitrary-`V` value fold already had)
// correctly KEEPS it. So the S3 switch (`open` now uses F5) FIXES this pre-existing
// counter data-loss bug — term-only counter members now survive reopen.
//
// With the gate ON, `open` IS F5, so this test asserts the CORRECT F5 behavior (keep)
// for both `open` and `open_with_f5_loader`; it pins the fix so a future regression
// (re-dropping the member) is caught.
// ============================================================================

#[test]
fn counter_term_only_member_survives_reopen_under_f5() {
    let dir = scratch("f5-counter-member");
    let path = dir.path().join("t.artc");
    {
        let trie = PersistentARTrieChar::<u64>::create(&path).expect("create");
        trie.insert("member").expect("insert term-only member"); // NO value
        trie.insert("").expect("insert term-only empty member"); // NO value
        trie.insert_with_value("valued", 5).expect("insert valued");
        trie.sync().expect("sync");
        trie.checkpoint().expect("checkpoint");
    }
    assert_overlay_regime(&path);
    // `open` (= F5 under the S3 gate) and the explicit F5 ctor must AGREE + KEEP all.
    let via_open = char_snapshot(&PersistentARTrieChar::<u64>::open(&path).expect("open"));
    let via_f5 =
        char_snapshot(&PersistentARTrieChar::<u64>::open_with_f5_loader(&path).expect("f5 open"));
    assert_corresponds(&via_open, &via_f5, "counter term-only member");

    let v5 = Some(libdictenstein::serialization::bincode_compat::serialize(&5u64).unwrap());
    assert_eq!(
        via_f5.entries.get("valued".as_bytes()),
        Some(&v5),
        "valued term survives"
    );
    assert!(
        via_f5.entries.contains_key("member".as_bytes()),
        "F5 keeps the term-only counter member (the old counter-fold dropped it)"
    );
    assert!(
        via_f5.entries.contains_key("".as_bytes()),
        "F5 keeps the term-only empty counter member"
    );
    assert_eq!(
        via_f5.entries.get("member".as_bytes()),
        Some(&None),
        "kept as a membership (no value)"
    );
}

// ============================================================================
// Real-disk reopen/recovery SOAK (verification step 5). Write N keys + checkpoint,
// then reopen-via-the-production-`open` (= F5 under the S3 gate) several cycles, each
// time adding a post-checkpoint WAL tail and re-checkpointing, asserting EVERY key
// survives every cycle. Under `target/test-tmp` (real disk, NOT tmpfs).
// ============================================================================

#[test]
fn char_f5_reopen_recovery_soak() {
    let dir = scratch("f5-char-soak");
    let path = dir.path().join("t.artc");
    const N: usize = 2_000;
    const CYCLES: usize = 6;

    // Initial build + checkpoint.
    {
        let trie = PersistentARTrieChar::<u64>::create(&path).expect("create");
        for i in 0..N {
            trie.insert_with_value(&format!("term-{i:05}"), i as u64)
                .expect("ins");
        }
        trie.insert_with_value("", 999_999).expect("ins empty");
        trie.sync().expect("sync");
        trie.checkpoint().expect("ckpt");
    }
    assert_overlay_regime(&path);

    for cycle in 0..CYCLES {
        // Reopen via the PRODUCTION `open` (F5 under the gate).
        let trie = PersistentARTrieChar::<u64>::open(&path).expect("soak reopen");
        // Every original key survives (skip term-00000 — it is overwritten each cycle;
        // checked separately below).
        for i in 1..N {
            assert_eq!(
                MappedDictionary::get_value(&trie, &format!("term-{i:05}")),
                Some(i as u64),
                "cycle {cycle}: term-{i:05} survives reopen"
            );
        }
        // term-00000 reflects the PRIOR cycle's overwrite (cycle 0 sees the original 0).
        let expected_t0 = if cycle == 0 {
            0u64
        } else {
            1_000 + (cycle as u64) - 1
        };
        assert_eq!(
            MappedDictionary::get_value(&trie, "term-00000"),
            Some(expected_t0),
            "cycle {cycle}: term-00000 reflects the prior overwrite (or original at cycle 0)"
        );
        // Every prior cycle's key survives.
        for prior in 0..cycle {
            assert_eq!(
                MappedDictionary::get_value(&trie, &format!("cycle-{prior}")),
                Some(prior as u64),
                "cycle {cycle}: prior cycle-{prior} key survives"
            );
        }
        assert_eq!(
            MappedDictionary::get_value(&trie, ""),
            Some(999_999),
            "cycle {cycle}: empty-term survives"
        );
        // Add a per-cycle WAL tail (a new key + an overwrite), then re-checkpoint every
        // other cycle (so some reopens replay a tail, some load a fresh checkpoint).
        let ckey = format!("cycle-{cycle}");
        trie.insert_with_value(&ckey, cycle as u64)
            .expect("ins cycle key");
        trie.insert_with_value("term-00000", 1_000 + cycle as u64)
            .expect("overwrite");
        trie.sync().expect("sync tail");
        if cycle % 2 == 0 {
            trie.checkpoint().expect("re-checkpoint");
        }
        // The overwrite + new key are visible in-session.
        assert_eq!(
            MappedDictionary::get_value(&trie, "term-00000"),
            Some(1_000 + cycle as u64)
        );
        assert_eq!(
            MappedDictionary::get_value(&trie, &ckey),
            Some(cycle as u64)
        );
    }

    // Final reopen: every original key (except the overwritten term-00000), every
    // per-cycle key, and the empty term survive.
    let trie = PersistentARTrieChar::<u64>::open(&path).expect("final reopen");
    for i in 1..N {
        assert_eq!(
            MappedDictionary::get_value(&trie, &format!("term-{i:05}")),
            Some(i as u64),
            "final: term-{i:05} survives all {CYCLES} cycles"
        );
    }
    assert_eq!(
        MappedDictionary::get_value(&trie, "term-00000"),
        Some(1_000 + (CYCLES - 1) as u64),
        "final: term-00000 reflects the last overwrite"
    );
    for cycle in 0..CYCLES {
        assert_eq!(
            MappedDictionary::get_value(&trie, &format!("cycle-{cycle}")),
            Some(cycle as u64),
            "final: cycle-{cycle} key survives"
        );
    }
    assert_eq!(
        MappedDictionary::get_value(&trie, ""),
        Some(999_999),
        "final: empty-term survives"
    );
}

#[test]
fn byte_f5_reopen_recovery_soak() {
    let dir = scratch("f5-byte-soak");
    let path = dir.path().join("t.part");
    const N: usize = 2_000;
    const CYCLES: usize = 6;

    {
        let trie = PersistentARTrie::<i64>::create(&path).expect("create");
        for i in 0..N {
            trie.insert_with_value(&format!("term-{i:05}"), i as i64);
        }
        trie.sync().expect("sync");
        trie.checkpoint().expect("ckpt");
    }
    assert_overlay_regime(&path);

    for cycle in 0..CYCLES {
        let trie = PersistentARTrie::<i64>::open(&path).expect("soak reopen");
        for i in 1..N {
            assert_eq!(
                MappedDictionary::get_value(&trie, &format!("term-{i:05}")),
                Some(i as i64),
                "cycle {cycle}: term-{i:05} survives reopen"
            );
        }
        let expected_t0 = if cycle == 0 {
            0i64
        } else {
            1_000 + (cycle as i64) - 1
        };
        assert_eq!(
            MappedDictionary::get_value(&trie, "term-00000"),
            Some(expected_t0),
            "cycle {cycle}: term-00000 reflects the prior overwrite"
        );
        for prior in 0..cycle {
            assert_eq!(
                MappedDictionary::get_value(&trie, &format!("cycle-{prior}")),
                Some(prior as i64),
                "cycle {cycle}: prior cycle-{prior} key survives"
            );
        }
        let ckey = format!("cycle-{cycle}");
        trie.insert_with_value(&ckey, cycle as i64);
        trie.insert_with_value("term-00000", 1_000 + cycle as i64);
        trie.sync().expect("sync tail");
        if cycle % 2 == 0 {
            trie.checkpoint().expect("re-checkpoint");
        }
    }

    let trie = PersistentARTrie::<i64>::open(&path).expect("final reopen");
    for i in 1..N {
        assert_eq!(
            MappedDictionary::get_value(&trie, &format!("term-{i:05}")),
            Some(i as i64),
            "final: term-{i:05} survives all {CYCLES} cycles"
        );
    }
    assert_eq!(
        MappedDictionary::get_value(&trie, "term-00000"),
        Some(1_000 + (CYCLES - 1) as i64),
        "final: term-00000 reflects the last overwrite"
    );
    for cycle in 0..CYCLES {
        assert_eq!(
            MappedDictionary::get_value(&trie, &format!("cycle-{cycle}")),
            Some(cycle as i64),
            "final: cycle-{cycle} key survives"
        );
    }
}
