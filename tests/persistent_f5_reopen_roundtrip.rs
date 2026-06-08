//! **F5 reopen round-trip + soak (rescued from the deleted `persistent_f5_both_loaders_correspondence`).**
//!
//! L1.3 deleted the legacy owned-loader reopen path (`open_with_legacy_loader`), so the old
//! "legacy-loader ≡ F5-loader" oracle is vacuous. These two tests do NOT compare against the legacy
//! loader — they validate the PRODUCTION F5 reopen (`open` == `open_with_f5_loader` under the gate)
//! directly, and are kept as regression guards:
//!   - `counter_term_only_member_survives_reopen_under_f5` pins the term-only-counter-member fix (the
//!     old counter-fold DROPPED a counter term inserted with no value).
//!   - `char_f5_reopen_recovery_soak` is a real-disk multi-cycle reopen soak (every key survives N
//!     reopen/checkpoint/WAL-tail cycles).
//! Scratch is real disk (`target/test-tmp`), never tmpfs.
#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie_char::PersistentARTrieChar;
use libdictenstein::persistent_artrie_core::wal::{RankRegime, WalReader};
use libdictenstein::{Dictionary, MappedDictionary};

fn scratch(tag: &str) -> tempfile::TempDir {
    std::fs::create_dir_all("target/test-tmp").ok();
    tempfile::Builder::new()
        .prefix(tag)
        .tempdir_in("target/test-tmp")
        .expect("real-disk scratch under target/test-tmp")
}

/// A normalized snapshot of a trie's observable state: per-term the bincode of its value (or `None`
/// for a term-only member), plus the term count.
#[derive(Debug, PartialEq)]
struct Snapshot {
    len: Option<usize>,
    entries: std::collections::BTreeMap<Vec<u8>, Option<Vec<u8>>>,
}

/// Assert the WAL is stamped Overlay (so the F5 loader actually runs its F5 arm). Eligible-`V`
/// `create()` auto-flips to Overlay, so this holds for every trie here.
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

fn assert_corresponds(a: &Snapshot, b: &Snapshot, ctx: &str) {
    assert_eq!(
        a.len, b.len,
        "{ctx}: len differs ({:?} vs {:?})",
        a.len, b.len
    );
    assert_eq!(a.entries, b.entries, "{ctx}: term-set / values differ");
}

fn char_snapshot(trie: &PersistentARTrieChar<u64>) -> Snapshot {
    let mut entries = std::collections::BTreeMap::new();
    let terms = trie
        .iter_prefix("")
        .expect("char iter_prefix")
        .unwrap_or_default();
    for term in terms {
        let key = term.as_bytes().to_vec();
        let val_bytes = MappedDictionary::get_value(trie, &term)
            .map(|v| libdictenstein::serialization::bincode_compat::serialize(&v).expect("ser"));
        entries.insert(key, val_bytes);
    }
    Snapshot {
        len: Dictionary::len(trie),
        entries,
    }
}

/// The term-only-counter-member regression guard: a counter (`V=u64`) term inserted with NO value
/// (a pure membership) must survive an F5 reopen — the old counter-fold dropped it.
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

/// Real-disk reopen/recovery SOAK: write N keys + checkpoint, then reopen via the production `open`
/// (= F5 under the S3 gate) several cycles, each adding a post-checkpoint WAL tail and
/// re-checkpointing, asserting EVERY key survives every cycle.
#[test]
fn char_f5_reopen_recovery_soak() {
    let dir = scratch("f5-char-soak");
    let path = dir.path().join("t.artc");
    const N: usize = 2_000;
    const CYCLES: usize = 6;

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
        let trie = PersistentARTrieChar::<u64>::open(&path).expect("soak reopen");
        for i in 1..N {
            assert_eq!(
                MappedDictionary::get_value(&trie, &format!("term-{i:05}")),
                Some(i as u64),
                "cycle {cycle}: term-{i:05} survives reopen"
            );
        }
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
        let ckey = format!("cycle-{cycle}");
        trie.insert_with_value(&ckey, cycle as u64)
            .expect("ins cycle key");
        trie.insert_with_value("term-00000", 1_000 + cycle as u64)
            .expect("overwrite");
        trie.sync().expect("sync tail");
        if cycle % 2 == 0 {
            trie.checkpoint().expect("re-checkpoint");
        }
        assert_eq!(
            MappedDictionary::get_value(&trie, "term-00000"),
            Some(1_000 + cycle as u64)
        );
        assert_eq!(
            MappedDictionary::get_value(&trie, &ckey),
            Some(cycle as u64)
        );
    }

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
