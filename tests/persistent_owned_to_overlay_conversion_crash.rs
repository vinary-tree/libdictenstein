//! **F7-S4 — crash-safety proptest for the Owned→Overlay conversion-on-reopen.**
//!
//! Implements the MANDATORY crash-safety verification of
//! `docs/design/f7-owned-to-overlay-rotation.md` (v4 / Round-5 CONVERGED). The converter
//! `convert_owned_to_overlay_on_reopen` consults the runtime fail points in
//! [`libdictenstein::persistent_artrie_core::overlay::f7_failpoint`] (DISARMED in
//! production; armed here) to simulate a power-cut at each conversion step. After each
//! injected crash we reopen and assert EVERY committed term + value + the empty term `""`
//! + the unranked owned entries survive and the final regime is Overlay.
//!
//! Crash points swept: `{BeforeRotate, AfterRotateBeforeStamp, AfterStampBeforeBuild,
//! DuringDrain, None}`.
//!
//! CRITICAL CASES (the exact red-team concerns):
//! - (a) the >=11x `AfterRotateBeforeStamp` crash-LOOP — the real Owned tail is NEVER
//!   pruned/lost (FIX D: the converter classifies the records-empty-but-high-next_lsn
//!   active as the CHEAP path and never re-rotates, so `prune_segments_if_needed` never
//!   evicts the one real un-subsumed archive segment).
//! - (b) a `BatchIncrement` DELTA applied EXACTLY ONCE across a checkpoint+crash (FIX C
//!   base-seed + OBL-2 image checkpoint_lsn) — exercised on the POST-conversion Overlay
//!   trie (owned increments are absolute; only the overlay durable increment logs a
//!   commutative `BatchIncrement` delta).
//! - (c) an injected prefix-gap → RES-3 fail-loud (FIX E), not a silent incomplete rebuild.
//!
//! Real-disk scratch under `target/test-tmp/` (NEVER tmpfs `/tmp` — that host's `/tmp` is
//! RAM, which hides fsync/durability behavior). Byte + char × `V ∈ {(), counter, String}`.

#![cfg(feature = "persistent-artrie")]

use std::collections::BTreeMap;
use std::path::Path;

use libdictenstein::persistent_artrie::PersistentARTrie;
use libdictenstein::persistent_artrie_char::PersistentARTrieChar;
use libdictenstein::persistent_artrie_core::overlay::f7_failpoint::{self, FailPoint};
use libdictenstein::persistent_artrie_core::wal::{RankRegime, WalReader};
use libdictenstein::value::DictionaryValue;

use serde::{Deserialize, Serialize};

/// The F7 crash-injection fail point is a PROCESS-GLOBAL atomic (one per process). Cargo
/// runs `#[test]` fns concurrently, so every test that arms the fail point OR reopens a
/// converter (which CONSULTS the atomic) must hold this lock to avoid one test's arm
/// leaking into another's reopen. We serialize all converter-exercising tests through it.
/// `parking_lot`-free (std `Mutex`); poisoning is irrelevant (we only guard ordering).
static FAILPOINT_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Acquire the global serialization lock (ignoring poison — we only need mutual exclusion).
fn failpoint_guard() -> std::sync::MutexGuard<'static, ()> {
    FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Real-disk scratch dir under `target/test-tmp` (NOT tmpfs). Each test gets a unique
/// subdir; the `TempDir` cleans up on drop.
fn scratch(tag: &str) -> tempfile::TempDir {
    std::fs::create_dir_all("target/test-tmp").ok();
    tempfile::Builder::new()
        .prefix(&format!("f7-convert-crash-{tag}-"))
        .tempdir_in("target/test-tmp")
        .expect("real-disk scratch under target/test-tmp")
}

/// Assert the on-disk WAL header is stamped to `regime`.
fn assert_regime(path: &Path, regime: RankRegime, ctx: &str) {
    let wal = path.with_extension("wal");
    let actual = WalReader::read_header(&wal)
        .map(|h| h.regime())
        .unwrap_or(RankRegime::Owned);
    assert_eq!(actual, regime, "{ctx}: WAL regime mismatch ({wal:?})");
}

/// A small derive-everything struct value (the "arbitrary struct V" arm).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Small {
    a: u32,
    b: String,
}
impl DictionaryValue for Small {}

/// The full sweep of converter fail points.
const ALL_FAILPOINTS: [FailPoint; 5] = [
    FailPoint::None,
    FailPoint::BeforeRotate,
    FailPoint::AfterRotateBeforeStamp,
    FailPoint::AfterStampBeforeBuild,
    FailPoint::DuringDrain,
];

/// `true` iff this fail point injects a simulated crash (every point except `None`).
fn injects(fp: FailPoint) -> bool {
    fp != FailPoint::None
}

// ============================================================================
// BYTE probe set. The byte counter monomorph is `i64`. The byte `insert*`/`upsert*`
// return `bool`/`Result`; reads route to the overlay under `route_overlay()`.
// ============================================================================

/// Build an OWNED-regime byte fixture: `create` (auto-flips Overlay for eligible V) then
/// `kill_switch_to_owned` (restamps Owned on the still-fresh WAL) so all writes land in
/// the owned tree under the Owned regime. Writes the membership/value entries (incl. the
/// empty term `""`), checkpoints (writing a dense image + truncating the WAL — the image
/// covers <= checkpoint_lsn), then writes a post-checkpoint TAIL (the un-checkpointed,
/// records-non-empty active = the converter's ROTATE-path input; these are the "unranked"
/// owned records that orphan-KEEP must preserve). Returns at drop a file that reopens into
/// the converter.
fn byte_build_owned_fixture<V>(
    path: &Path,
    pre_ckpt: &[(Vec<u8>, Option<V>)],
    tail: &[(Vec<u8>, Option<V>)],
) where
    V: DictionaryValue + Clone + PartialEq,
{
    let mut trie = PersistentARTrie::<V>::create(path).expect("byte create");
    trie.kill_switch_to_owned();
    assert_regime(
        path,
        RankRegime::Owned,
        "byte fixture: kill_switch should stamp Owned",
    );
    let write = |trie: &mut PersistentARTrie<V>, term: &[u8], value: &Option<V>| match value {
        Some(v) => {
            trie.upsert_bytes(term, v.clone())
                .expect("byte owned upsert");
        }
        None => {
            // Term-only membership by bytes (no value) → single-entry batch insert.
            let n = trie.insert_batch_bytes(&[(term, None)]);
            assert_eq!(n, 1, "byte owned membership insert");
        }
    };
    for (term, value) in pre_ckpt {
        write(&mut trie, term, value);
    }
    trie.checkpoint().expect("byte checkpoint");
    // Post-checkpoint TAIL (un-checkpointed; records-non-empty active).
    for (term, value) in tail {
        write(&mut trie, term, value);
    }
    trie.sync().expect("byte sync");
    // Drop WITHOUT a clean close → the post-checkpoint tail is the un-clean recovery state.
}

/// Normalized observable snapshot: (term-bytes → bincode(value) or None for membership).
fn byte_snapshot<V>(trie: &PersistentARTrie<V>) -> BTreeMap<Vec<u8>, Option<Vec<u8>>>
where
    V: DictionaryValue + Serialize,
{
    let mut out = BTreeMap::new();
    let terms = trie
        .iter_prefix(b"")
        .map(|it| it.collect::<Vec<_>>())
        .unwrap_or_default();
    for term in terms {
        let v = trie.get_value_bytes(&term);
        let encoded =
            v.map(|v| libdictenstein::serialization::bincode_compat::serialize(&v).expect("enc"));
        out.insert(term, encoded);
    }
    out
}

// ============================================================================
// CHAR probe set. The char counter monomorph is `u64`. `insert*` return `Result<bool>`.
// ============================================================================

fn char_build_owned_fixture<V>(
    path: &Path,
    pre_ckpt: &[(String, Option<V>)],
    tail: &[(String, Option<V>)],
) where
    V: DictionaryValue + Clone + PartialEq + Serialize,
{
    let trie = PersistentARTrieChar::<V>::create(path).expect("char create");
    trie.kill_switch_to_owned();
    assert_regime(
        path,
        RankRegime::Owned,
        "char fixture: kill_switch should stamp Owned",
    );
    for (term, value) in pre_ckpt {
        match value {
            Some(v) => {
                trie.insert_with_value(term, v.clone())
                    .expect("char owned insert_with_value");
            }
            None => {
                trie.insert(term).expect("char owned insert");
            }
        }
    }
    trie.checkpoint().expect("char checkpoint");
    for (term, value) in tail {
        match value {
            Some(v) => {
                trie.insert_with_value(term, v.clone())
                    .expect("char owned insert_with_value tail");
            }
            None => {
                trie.insert(term).expect("char owned insert tail");
            }
        }
    }
    trie.sync().expect("char sync");
}

fn char_snapshot<V>(trie: &PersistentARTrieChar<V>) -> BTreeMap<Vec<u8>, Option<Vec<u8>>>
where
    V: DictionaryValue + Serialize,
{
    let mut out = BTreeMap::new();
    let terms = trie
        .iter_prefix("")
        .expect("char iter_prefix")
        .unwrap_or_default();
    for term in terms {
        let v = trie.get_value(&term);
        let encoded =
            v.map(|v| libdictenstein::serialization::bincode_compat::serialize(&v).expect("enc"));
        out.insert(term.into_bytes(), encoded);
    }
    out
}

// ============================================================================
// The expected snapshot (the durable LWW state of the fixture), independent of the
// loader. The owned fixture applies pre_ckpt then tail in order; the tail's later writes
// for the same term win (LWW). The empty term "" is included.
// ============================================================================

fn expected_byte<V>(
    pre_ckpt: &[(Vec<u8>, Option<V>)],
    tail: &[(Vec<u8>, Option<V>)],
) -> BTreeMap<Vec<u8>, Option<Vec<u8>>>
where
    V: DictionaryValue + Serialize + Clone,
{
    let mut out: BTreeMap<Vec<u8>, Option<Vec<u8>>> = BTreeMap::new();
    for (term, value) in pre_ckpt.iter().chain(tail.iter()) {
        let enc = value
            .as_ref()
            .map(|v| libdictenstein::serialization::bincode_compat::serialize(v).expect("enc"));
        out.insert(term.clone(), enc);
    }
    out
}

fn expected_char<V>(
    pre_ckpt: &[(String, Option<V>)],
    tail: &[(String, Option<V>)],
) -> BTreeMap<Vec<u8>, Option<Vec<u8>>>
where
    V: DictionaryValue + Serialize + Clone,
{
    let mut out: BTreeMap<Vec<u8>, Option<Vec<u8>>> = BTreeMap::new();
    for (term, value) in pre_ckpt.iter().chain(tail.iter()) {
        let enc = value
            .as_ref()
            .map(|v| libdictenstein::serialization::bincode_compat::serialize(v).expect("enc"));
        out.insert(term.clone().into_bytes(), enc);
    }
    out
}

// ============================================================================
// Fixture data (byte + char, per V monomorph). Each includes: the empty term "",
// term-only members (the `None`-valued "unranked" entries), valued entries, a shared
// prefix family, and a deep key. pre_ckpt → dense image; tail → un-checkpointed active.
// ============================================================================

fn byte_fixture_unit() -> (Vec<(Vec<u8>, Option<()>)>, Vec<(Vec<u8>, Option<()>)>) {
    let pre: Vec<(Vec<u8>, Option<()>)> = vec![
        (b"".to_vec(), Some(())), // the empty term ""
        (b"alpha".to_vec(), Some(())),
        (b"alphabet".to_vec(), Some(())),
        (b"beta".to_vec(), Some(())),
        (vec![b'z'; 300], Some(())), // deep key
    ];
    let tail: Vec<(Vec<u8>, Option<()>)> = vec![
        (b"gamma".to_vec(), Some(())), // un-checkpointed (unranked Owned) entry
        (b"\x00\x01".to_vec(), Some(())),
        (b"alpine".to_vec(), Some(())),
    ];
    (pre, tail)
}

fn byte_fixture_i64() -> (Vec<(Vec<u8>, Option<i64>)>, Vec<(Vec<u8>, Option<i64>)>) {
    let pre: Vec<(Vec<u8>, Option<i64>)> = vec![
        (b"".to_vec(), Some(13)), // valued empty term
        (b"apple".to_vec(), Some(3)),
        (b"application".to_vec(), Some(17)),
        (b"member_only".to_vec(), None), // term-only member MIXED with valued
        (vec![b'q'; 257], Some(42)),
    ];
    let tail: Vec<(Vec<u8>, Option<i64>)> = vec![
        (b"banana".to_vec(), Some(5000)),
        (b"apple".to_vec(), Some(99)), // LWW overwrite of a pre-ckpt term in the tail
        (b"tail_only".to_vec(), Some(-7)),
    ];
    (pre, tail)
}

fn byte_fixture_string() -> (
    Vec<(Vec<u8>, Option<String>)>,
    Vec<(Vec<u8>, Option<String>)>,
) {
    let pre: Vec<(Vec<u8>, Option<String>)> = vec![
        (b"".to_vec(), Some("EMPTY".to_string())),
        (b"key1".to_vec(), Some("v1".to_string())),
        (b"key_member".to_vec(), None),
        (b"key2".to_vec(), Some("v2".to_string())),
    ];
    let tail: Vec<(Vec<u8>, Option<String>)> = vec![
        (b"key3".to_vec(), Some("v3".to_string())),
        (b"key1".to_vec(), Some("v1-updated".to_string())),
    ];
    (pre, tail)
}

fn char_fixture_unit() -> (Vec<(String, Option<()>)>, Vec<(String, Option<()>)>) {
    let pre: Vec<(String, Option<()>)> = vec![
        ("".into(), Some(())),
        ("alpha".into(), Some(())),
        ("alphabet".into(), Some(())),
        ("日本".into(), Some(())),
        ("🎉x".into(), Some(())),
    ];
    let tail: Vec<(String, Option<()>)> =
        vec![("gamma".into(), Some(())), ("alpine".into(), Some(()))];
    (pre, tail)
}

fn char_fixture_u64() -> (Vec<(String, Option<u64>)>, Vec<(String, Option<u64>)>) {
    let pre: Vec<(String, Option<u64>)> = vec![
        ("".into(), Some(13)),
        ("apple".into(), Some(3)),
        ("member_only".into(), None),
        ("日本".into(), Some(99)),
        // A u64 count above i64::MAX (the bit-pattern-faithful decode regression).
        ("huge".into(), Some(u64::MAX - 5)),
    ];
    let tail: Vec<(String, Option<u64>)> = vec![
        ("banana".into(), Some(5000)),
        ("apple".into(), Some(77)),
        ("tail_only".into(), Some(1)),
    ];
    (pre, tail)
}

fn char_fixture_string() -> (Vec<(String, Option<String>)>, Vec<(String, Option<String>)>) {
    let pre: Vec<(String, Option<String>)> = vec![
        ("".into(), Some("EMPTY".to_string())),
        ("alpha".into(), Some("A".to_string())),
        ("alps".into(), Some("B".to_string())),
        ("member".into(), None),
    ];
    let tail: Vec<(String, Option<String>)> = vec![
        ("beta".into(), Some("C".to_string())),
        ("alpha".into(), Some("A2".to_string())),
    ];
    (pre, tail)
}

// ============================================================================
// The core crash-cycle drivers (byte + char). For each fail point: build the fixture,
// arm, attempt the converting reopen (expect Err for an injected point), drop, disarm,
// reopen (expect Ok), then assert the snapshot == expected + regime Overlay.
// ============================================================================

fn byte_run_one<V>(
    tag: &str,
    fp: FailPoint,
    pre: &[(Vec<u8>, Option<V>)],
    tail: &[(Vec<u8>, Option<V>)],
) where
    V: DictionaryValue + Serialize + Clone + PartialEq,
{
    // Serialize: the fail point is process-global; hold the lock across this whole
    // arm→reopen→clean-reopen cycle so a parallel test cannot clobber the armed state.
    let _lock = failpoint_guard();
    let dir = scratch(tag);
    let path = dir.path().join("t.part");
    byte_build_owned_fixture::<V>(&path, pre, tail);

    // Armed reopen — the converter injects a crash at `fp` (an Err aborts `open`).
    {
        let _guard = f7_failpoint::arm(fp);
        let result = PersistentARTrie::<V>::open(&path);
        if injects(fp) {
            assert!(
                result.is_err(),
                "{tag}/{fp:?}: armed converter reopen must fail (simulated crash)"
            );
        }
        // _guard disarms on drop; the failed trie (if Ok for None) drops here too.
    }
    f7_failpoint::disarm();

    // Clean reopen — the converter must now complete and recover EVERY committed datum.
    let trie = PersistentARTrie::<V>::open(&path).expect("byte clean reopen after crash");
    let got = byte_snapshot(&trie);
    let want = expected_byte::<V>(pre, tail);
    assert_eq!(
        got, want,
        "{tag}/{fp:?}: byte snapshot mismatch after converting reopen"
    );
    assert_regime(
        &path,
        RankRegime::Overlay,
        &format!("{tag}/{fp:?}: final regime must be Overlay"),
    );
    // Idempotence: a SECOND reopen yields the same snapshot (cheap path, no loss).
    drop(trie);
    let trie2 = PersistentARTrie::<V>::open(&path).expect("byte second reopen");
    assert_eq!(
        byte_snapshot(&trie2),
        want,
        "{tag}/{fp:?}: byte second-reopen idempotence"
    );
}

fn char_run_one<V>(
    tag: &str,
    fp: FailPoint,
    pre: &[(String, Option<V>)],
    tail: &[(String, Option<V>)],
) where
    V: DictionaryValue + Serialize + Clone + PartialEq,
{
    // Serialize: the fail point is process-global (see `byte_run_one`).
    let _lock = failpoint_guard();
    let dir = scratch(tag);
    let path = dir.path().join("t.artc");
    char_build_owned_fixture::<V>(&path, pre, tail);

    {
        let _guard = f7_failpoint::arm(fp);
        let result = PersistentARTrieChar::<V>::open(&path);
        if injects(fp) {
            assert!(
                result.is_err(),
                "{tag}/{fp:?}: armed converter reopen must fail (simulated crash)"
            );
        }
    }
    f7_failpoint::disarm();

    let trie = PersistentARTrieChar::<V>::open(&path).expect("char clean reopen after crash");
    let got = char_snapshot(&trie);
    let want = expected_char::<V>(pre, tail);
    assert_eq!(
        got, want,
        "{tag}/{fp:?}: char snapshot mismatch after converting reopen"
    );
    assert_regime(
        &path,
        RankRegime::Overlay,
        &format!("{tag}/{fp:?}: final regime must be Overlay"),
    );
    drop(trie);
    let trie2 = PersistentARTrieChar::<V>::open(&path).expect("char second reopen");
    assert_eq!(
        char_snapshot(&trie2),
        want,
        "{tag}/{fp:?}: char second-reopen idempotence"
    );
}

// ============================================================================
// TEST 1 — the full fail-point sweep, byte + char × V ∈ {(), counter, String}.
// ============================================================================

#[test]
fn byte_unit_convert_crash_sweep() {
    let (pre, tail) = byte_fixture_unit();
    for fp in ALL_FAILPOINTS {
        byte_run_one::<()>("byte-unit", fp, &pre, &tail);
    }
}

#[test]
fn byte_i64_convert_crash_sweep() {
    let (pre, tail) = byte_fixture_i64();
    for fp in ALL_FAILPOINTS {
        byte_run_one::<i64>("byte-i64", fp, &pre, &tail);
    }
}

#[test]
fn byte_string_convert_crash_sweep() {
    let (pre, tail) = byte_fixture_string();
    for fp in ALL_FAILPOINTS {
        byte_run_one::<String>("byte-string", fp, &pre, &tail);
    }
}

#[test]
fn char_unit_convert_crash_sweep() {
    let (pre, tail) = char_fixture_unit();
    for fp in ALL_FAILPOINTS {
        char_run_one::<()>("char-unit", fp, &pre, &tail);
    }
}

#[test]
fn char_u64_convert_crash_sweep() {
    let (pre, tail) = char_fixture_u64();
    for fp in ALL_FAILPOINTS {
        char_run_one::<u64>("char-u64", fp, &pre, &tail);
    }
}

#[test]
fn char_string_convert_crash_sweep() {
    let (pre, tail) = char_fixture_string();
    for fp in ALL_FAILPOINTS {
        char_run_one::<String>("char-string", fp, &pre, &tail);
    }
}

// ============================================================================
// TEST 2 (CRITICAL CASE a) — the >=11x AfterRotateBeforeStamp crash-LOOP (FIX D). The
// real un-subsumed Owned tail must be NEVER pruned/lost across many crash-reopens.
//
// FIX D witness: with `AfterRotateBeforeStamp` armed, the FIRST reopen rotates the tail to
// archive then crashes BEFORE the stamp — leaving a records-EMPTY-but-HIGH-next_lsn active.
// EVERY subsequent reopen classifies that active as records-empty-on-disk and takes the
// CHEAP path (which does NOT rotate, so it never hits the `AfterRotateBeforeStamp` fail
// point inside `rotate_and_restamp_overlay`), CONVERGING. A BUGGY converter that keyed
// cheap-vs-rotate on `next_lsn==1` (NOT records-empty-on-disk) would MIS-classify the
// high-next_lsn active as non-empty, RE-ROTATE on every loop, mint an empty archive segment
// each time, and `prune_segments_if_needed` (oldest-first, max_segments=10) would evict the
// ONE real un-subsumed segment after ~10 loops → silent tail loss.
//
// We therefore run >10 reopen attempts under the armed fail point and assert the INVARIANT
// the FIX guarantees: the archive segment count NEVER exceeds 1 (no accumulation), and a
// final clean reopen recovers EVERY committed datum (the real tail survived).
// ============================================================================

/// Count `.segment` files in the default `wal_archive` dir next to `path`.
fn archive_segment_count(path: &Path) -> usize {
    let adir = path.parent().expect("parent").join("wal_archive");
    if !adir.exists() {
        return 0;
    }
    std::fs::read_dir(&adir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.path().extension().map_or(false, |x| x == "segment"))
                .count()
        })
        .unwrap_or(0)
}

#[test]
fn byte_i64_after_rotate_crash_loop_never_prunes_tail() {
    let _lock = failpoint_guard();
    let (pre, tail) = byte_fixture_i64();
    let dir = scratch("byte-i64-rotateloop");
    let path = dir.path().join("t.part");
    byte_build_owned_fixture::<i64>(&path, &pre, &tail);

    // FIX-D INVARIANT: across many crash-reopens the converter rotates AT MOST ONCE total,
    // so the archive grows by at most ONE segment beyond the pre-loop baseline (the byte
    // owned checkpoint truncates rather than archiving, so the baseline is 0 here, but we
    // compute it for robustness/parity with char whose owned checkpoint DOES archive a
    // subsumed segment). A buggy re-rotating converter would mint a segment EACH loop and
    // grow without bound.
    let baseline = archive_segment_count(&path);
    let mut crashes = 0usize;
    for i in 0..13 {
        let _guard = f7_failpoint::arm(FailPoint::AfterRotateBeforeStamp);
        let result = PersistentARTrie::<i64>::open(&path);
        if result.is_err() {
            crashes += 1;
        }
        drop(result);
        let segs = archive_segment_count(&path);
        assert!(
            segs <= baseline + 1,
            "loop {i}: archive accumulated {segs} segments (> baseline {baseline} + 1) — FIX D \
             violated (a re-rotating converter would prune the real tail)"
        );
    }
    f7_failpoint::disarm();
    assert!(
        crashes >= 1,
        "the FIRST armed AfterRotateBeforeStamp reopen must have crashed (rotate happened)"
    );

    // A clean reopen recovers EVERY committed datum — the real tail was never pruned.
    let trie = PersistentARTrie::<i64>::open(&path).expect("clean reopen after 13x crash loop");
    let got = byte_snapshot(&trie);
    let want = expected_byte::<i64>(&pre, &tail);
    assert_eq!(
        got, want,
        "AfterRotateBeforeStamp x13 must NOT prune/lose the real un-subsumed Owned tail (FIX D)"
    );
    assert_regime(
        &path,
        RankRegime::Overlay,
        "rotate-loop: final regime Overlay",
    );
}

#[test]
fn char_u64_after_rotate_crash_loop_never_prunes_tail() {
    let _lock = failpoint_guard();
    let (pre, tail) = char_fixture_u64();
    let dir = scratch("char-u64-rotateloop");
    let path = dir.path().join("t.artc");
    char_build_owned_fixture::<u64>(&path, &pre, &tail);

    // FIX-D INVARIANT (char): the char owned checkpoint ROTATES the pre-checkpoint records
    // to a (checkpoint-subsumed) archive segment, so the pre-loop baseline is >= 1. The
    // converter must still rotate AT MOST ONCE more (the real tail), so the count never
    // exceeds baseline + 1 across the crash-loop.
    let baseline = archive_segment_count(&path);
    let mut crashes = 0usize;
    for i in 0..13 {
        let _guard = f7_failpoint::arm(FailPoint::AfterRotateBeforeStamp);
        let result = PersistentARTrieChar::<u64>::open(&path);
        if result.is_err() {
            crashes += 1;
        }
        drop(result);
        let segs = archive_segment_count(&path);
        assert!(
            segs <= baseline + 1,
            "loop {i}: char archive accumulated {segs} segments (> baseline {baseline} + 1) — FIX D violated"
        );
    }
    f7_failpoint::disarm();
    assert!(crashes >= 1, "the FIRST armed reopen must have crashed");

    let trie = PersistentARTrieChar::<u64>::open(&path).expect("clean reopen after 13x loop");
    let got = char_snapshot(&trie);
    let want = expected_char::<u64>(&pre, &tail);
    assert_eq!(
        got, want,
        "char AfterRotateBeforeStamp x13 must NOT prune/lose the real Owned tail (FIX D)"
    );
    assert_regime(
        &path,
        RankRegime::Overlay,
        "char rotate-loop: final regime Overlay",
    );
}

// ============================================================================
// TEST 3 (CRITICAL CASE b) — a BatchIncrement DELTA applied EXACTLY ONCE across a
// checkpoint + crash (FIX C base-seed + OBL-2 image checkpoint_lsn). Owned increments are
// ABSOLUTE; only the POST-conversion overlay durable increment logs a commutative
// `BatchIncrement` delta. So: build an Owned fixture, reopen (→ Overlay via the
// converter), do an overlay durable increment (logs the delta), checkpoint (the overlay
// retaining publisher records checkpoint_lsn = watermark covering the delta), then SIMULATE
// A CRASH (drop without further clean shutdown) and reopen — the F5 archive-aware drain
// must SKIP the checkpoint-subsumed delta (it is already folded into the dense image), so
// the counter equals the post-increment value, NOT double.
// ============================================================================

#[test]
fn byte_i64_batch_increment_applied_exactly_once_across_checkpoint_crash() {
    let _lock = failpoint_guard();
    let dir = scratch("byte-i64-batchinc-once");
    let path = dir.path().join("t.part");

    // (1) Owned fixture with a counter term.
    let pre: Vec<(Vec<u8>, Option<i64>)> = vec![(b"counter".to_vec(), Some(10))];
    let tail: Vec<(Vec<u8>, Option<i64>)> = vec![];
    byte_build_owned_fixture::<i64>(&path, &pre, &tail);

    // (2) Reopen → converts to Overlay. The counter is 10.
    let final_value;
    {
        let mut trie = PersistentARTrie::<i64>::open(&path).expect("convert reopen");
        assert_regime(&path, RankRegime::Overlay, "post-convert regime Overlay");
        assert_eq!(
            trie.get_value_bytes(b"counter"),
            Some(10),
            "counter after convert"
        );
        // (3) Overlay durable increment: +3 → 13 (logs a BatchIncrement DELTA).
        let v = trie
            .increment_bytes(b"counter", 3)
            .expect("overlay increment");
        assert_eq!(v, 13, "post-increment value");
        // (4) Checkpoint (overlay retaining publisher records checkpoint_lsn = watermark
        //     covering the +3 delta; the image now folds in 13).
        trie.checkpoint().expect("post-increment checkpoint");
        final_value = 13;
        // (5) SIMULATE CRASH: drop without any further write/close.
    }

    // (6) Reopen — the F5 archive-aware drain MUST skip the checkpoint-subsumed delta
    //     (FIX C base-seed >= tail_max ⇒ checkpoint_lsn >= delta lsn ⇒ skip). The counter
    //     must be EXACTLY 13, never 16 (double-applied delta).
    let trie = PersistentARTrie::<i64>::open(&path).expect("reopen after checkpoint+crash");
    assert_eq!(
        trie.get_value_bytes(b"counter"),
        Some(final_value),
        "BatchIncrement delta must be applied EXACTLY ONCE across checkpoint+crash (FIX C/OBL-2): \
         expected {final_value}, a double-apply would be 16"
    );
    // Idempotence across a further reopen.
    drop(trie);
    let trie2 = PersistentARTrie::<i64>::open(&path).expect("second reopen");
    assert_eq!(
        trie2.get_value_bytes(b"counter"),
        Some(final_value),
        "counter stable across a second reopen (no creeping double-apply)"
    );
}

#[test]
fn char_u64_batch_increment_applied_exactly_once_across_checkpoint_crash() {
    let _lock = failpoint_guard();
    let dir = scratch("char-u64-batchinc-once");
    let path = dir.path().join("t.artc");

    let pre: Vec<(String, Option<u64>)> = vec![("counter".into(), Some(10))];
    let tail: Vec<(String, Option<u64>)> = vec![];
    char_build_owned_fixture::<u64>(&path, &pre, &tail);

    let final_value;
    {
        let mut trie = PersistentARTrieChar::<u64>::open(&path).expect("convert reopen");
        assert_regime(&path, RankRegime::Overlay, "post-convert regime Overlay");
        assert_eq!(trie.get_value("counter"), Some(10), "counter after convert");
        let v = trie.increment("counter", 3).expect("overlay increment");
        assert_eq!(v, 13, "post-increment value");
        trie.checkpoint().expect("post-increment checkpoint");
        final_value = 13;
    }

    let trie = PersistentARTrieChar::<u64>::open(&path).expect("reopen after checkpoint+crash");
    assert_eq!(
        trie.get_value("counter"),
        Some(final_value),
        "char BatchIncrement delta applied EXACTLY ONCE (FIX C/OBL-2): expected {final_value}, \
         double would be 16"
    );
    drop(trie);
    let trie2 = PersistentARTrieChar::<u64>::open(&path).expect("second reopen");
    assert_eq!(
        trie2.get_value("counter"),
        Some(final_value),
        "char counter stable"
    );
}

// ============================================================================
// TEST 4 (CRITICAL CASE c) — an injected prefix-gap → RES-3 fail-LOUD (FIX E). Construct a
// converted Overlay file whose dense image redo frontier is some checkpoint_lsn, then
// physically DELETE the archive segment that carries the contiguous tail above the
// frontier (simulating a pruned un-subsumed prefix). The next reopen's archive-aware drain
// must FAIL LOUD with a corruption error, NOT silently rebuild an incomplete trie.
// ============================================================================

#[test]
fn byte_res3_prefix_gap_fails_loud_not_silent() {
    use libdictenstein::persistent_artrie_core::wal::{WalConfig, WalRecord, WalWriter};

    let _lock = failpoint_guard();
    let dir = scratch("byte-res3-gap");
    let path = dir.path().join("t.part");

    // (1) Build a convertible file with a DENSE IMAGE, then convert to Overlay. We will
    //     CORRUPT the root descriptor below so the image FAILS to load on the next reopen
    //     (the converter falls back to an EMPTY image ⇒ `loaded_from_disk == false`), which
    //     is the case the RES-3 / FIX-E loud guard applies to: with NO image to cover the
    //     prefix, the surviving segments MUST cover from LSN 1, and a pruned prefix
    //     (min surviving LSN > 1) is a genuine unrecoverable gap.
    let pre: Vec<(Vec<u8>, Option<i64>)> = vec![
        (b"a".to_vec(), Some(1)),
        (b"b".to_vec(), Some(2)),
        (b"c".to_vec(), Some(3)),
    ];
    byte_build_owned_fixture::<i64>(&path, &pre, &[]);
    {
        let trie = PersistentARTrie::<i64>::open(&path).expect("convert reopen");
        assert_regime(&path, RankRegime::Overlay, "post-convert Overlay");
        drop(trie);
    }

    // (2) Build a GAPPED archive on the trie's WAL paths, mirroring the proven
    //     `rebuild_from_wal_segments_regime_aware` RES-3 construction: REPLACE the active
    //     WAL with a fresh Overlay-regime records-empty WAL, and create TWO archived
    //     Overlay segments (LSN 1..=3 and LSN 4..=6) via `WalWriter::rotate_to_archive`,
    //     then DELETE the LSN-1..=3 segment — a pruned-prefix gap (surviving min LSN = 4).
    let wal_path = path.with_extension("wal");
    let archive_dir = path.parent().expect("parent").join("wal_archive");
    let _ = std::fs::remove_dir_all(&archive_dir);
    let _ = std::fs::remove_file(&wal_path);
    let cfg = WalConfig::with_archive_dir(&archive_dir);
    {
        let writer = WalWriter::create(&wal_path).expect("create fresh wal");
        writer
            .set_overlay_regime()
            .expect("stamp Overlay on empty WAL");
        for t in [b"d".as_slice(), b"e", b"f"] {
            // value bytes for an i64 (bincode legacy fixint); membership None is fine for
            // the gap construction (the gap is detected before any apply).
            writer
                .append(WalRecord::Insert {
                    term: t.to_vec(),
                    value: None,
                })
                .expect("append seg1");
        }
        writer.sync().expect("sync");
        let _seg1 = writer.rotate_to_archive(&cfg).expect("rotate seg1"); // LSN 1..=3
        for t in [b"g".as_slice(), b"h", b"i"] {
            writer
                .append(WalRecord::Insert {
                    term: t.to_vec(),
                    value: None,
                })
                .expect("append seg2");
        }
        writer.sync().expect("sync");
        let _seg2 = writer.rotate_to_archive(&cfg).expect("rotate seg2"); // LSN 4..=6
                                                                          // The active WAL is now fresh + records-empty + Overlay-regime (rotate carries the
                                                                          // regime). Drop the writer (closes the active WAL file).
    }
    // Prune the OLDEST archived segment (LSN 1..=3) → surviving min LSN = 4 (the gap).
    let mut segs: Vec<std::path::PathBuf> = std::fs::read_dir(&archive_dir)
        .expect("read archive dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map_or(false, |x| x == "segment"))
        .collect();
    segs.sort();
    assert_eq!(segs.len(), 2, "test setup: expected 2 archived segments");
    std::fs::remove_file(&segs[0]).expect("prune the LSN-1..=3 segment");

    // (3) CORRUPT the root descriptor (type byte at file offset 64 → an unknown type) so the
    //     dense image FAILS to load on reopen and the converter falls back to an EMPTY image
    //     (`loaded_from_disk == false`). Now NO image covers the prefix, so the surviving
    //     segments must cover from LSN 1 — and the pruned LSN-1..=3 segment leaves a genuine
    //     unrecoverable gap (min surviving LSN 4 > 1).
    {
        use std::io::{Seek, SeekFrom, Write};
        const DESCRIPTOR_TYPE_OFFSET: u64 = 64;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("open data file to corrupt descriptor");
        f.seek(SeekFrom::Start(DESCRIPTOR_TYPE_OFFSET))
            .expect("seek descriptor type");
        f.write_all(&[99u8]).expect("write unknown descriptor type");
        f.sync_all().expect("sync corrupt descriptor");
    }

    // (4) Reopen — the image fails to load (fallback → no image), the F5 archive-aware drain
    //     sees the pruned prefix (min surviving LSN 4 > 1 with NO image) and FAILS LOUD
    //     (RES-3 / FIX E), NOT a silent incomplete rebuild.
    let result = PersistentARTrie::<i64>::open(&path);
    assert!(
        result.is_err(),
        "RES-3 (FIX E): a pruned prefix with no covering image must FAIL LOUD, not silently \
         rebuild incomplete"
    );
    let err = result.err().expect("err");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("prefix gap") || msg.contains("RES-3") || msg.contains("FIX-E"),
        "RES-3 error should name the prefix gap; got: {msg}"
    );
}
