//! **The make-or-break u64-restoration data-loss proof.**
//!
//! The persistent ART-trie counter is a full `u64` (byte AND char). The historical
//! increment path did read-modify-write arithmetic in the `i64` domain
//! (`i64::from_le_bytes` + `checked_add`), which (a) SPURIOUSLY REJECTED a valid
//! increment that crossed `i64::MAX`, and (b) SILENTLY WRAPPED near `u64::MAX`. The
//! fix routes every counter-leaf read/write/arithmetic through `counter_codec` (an
//! `i128` substrate, range-checked into `[0, u64::MAX]`).
//!
//! This test is the end-to-end witness on a REAL durable trie:
//!   1. increment a counter PAST `i64::MAX` (start at `i64::MAX - 2`, `+10` across
//!      several calls) → the live value is the exact unsigned magnitude (> `i64::MAX`),
//!      NOT a reject and NOT a negative/wrapped value;
//!   2. CHECKPOINT + REOPEN → the value survives THROUGH THE CHECKPOINT IMAGE as the
//!      exact `u64` (> `i64::MAX`);
//!   3. DECREMENT back across the `i64::MAX` boundary → exact;
//!   4. push to `u64::MAX`, then `+1` → graceful `Err` (NOT a silent wrap to 0);
//!   5. a below-zero decrement → graceful `Err` (NOT a u64 underflow wrap).
//!
//! Scratch lives on REAL DISK (`target/test-tmp`), never `/tmp` (tmpfs on this host)
//! and never `tempdir()` — disk-backed tries must not fill RAM.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::char::PersistentARTrieChar;
use libdictenstein::persistent_artrie::{PersistentARTrie, WalRecord, WalWriter};
use libdictenstein::{Dictionary, MappedDictionary};
use std::path::Path;

fn scratch(prefix: &str) -> tempfile::TempDir {
    std::fs::create_dir_all("target/test-tmp").ok();
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in("target/test-tmp")
        .expect("scratch tempdir under target/test-tmp")
}

/// Inject a RANKED absolute `Increment` into the trie's existing Overlay-regime WAL.
///
/// `WalWriter::open` resumes the LSN at `last_lsn + 1` (above any drop-checkpoint, so
/// the F5 reconcile — which replays `lsn > checkpoint_lsn` only, and only on an
/// Overlay-regime WAL — picks it up). The paired `CommitRank` (referencing the
/// Increment's own LSN) RANKS the record: the F5 `(generation, lsn)` reconcile drops an
/// UNRANKED record as an orphan once any ranked record exists, so the rank is what an
/// organic overlay write would have written alongside it. `result_bp` is the i64
/// bit-pattern of the absolute post-increment count (negative for a u64 > i64::MAX).
fn inject_ranked_absolute_increment(wal_path: &Path, term: &[u8], result_bp: i64) {
    let writer = WalWriter::open(wal_path).expect("open the trie's existing WAL for append");
    writer
        .set_overlay_regime()
        .expect("ensure Overlay regime on the WAL header");
    let data_lsn = writer
        .append(WalRecord::Increment {
            term: term.to_vec(),
            delta: result_bp,
            result: result_bp,
        })
        .expect("append absolute Increment");
    writer
        .append(WalRecord::CommitRank {
            data_lsn,
            term: term.to_vec(),
            generation: 1,
        })
        .expect("append CommitRank ranking the Increment");
    writer.sync().expect("sync WAL");
}

/// The first `u64` value that does NOT fit an `i64` (`i64::MAX + 1`), built without a
/// numeric cast.
const FIRST_OVER_I64: u64 = (u64::MAX / 2) + 1;

/// **BYTE `<u64>` — the full data-loss matrix.**
#[test]
fn byte_counter_crosses_i64_max_survives_checkpoint_reopen_and_rejects_overflow() {
    let dir = scratch("u64-above-i64max-byte");
    let path = dir.path().join("t.part");

    // Start two below i64::MAX, then increment +10 across several calls so the running
    // count CROSSES i64::MAX (the old i64-domain path spuriously rejected this).
    let start: u64 = FIRST_OVER_I64 - 3; // == i64::MAX - 2
    let crossed: u64 = start + 10; // == i64::MAX + 7  (> i64::MAX)
    assert!(crossed > FIRST_OVER_I64, "the fixture must cross i64::MAX");

    {
        let mut trie = PersistentARTrie::<u64>::create(&path).expect("create<u64>");
        // Seed the base value via upsert (a direct u64 set — value-seam publish, no
        // i64 WAL delta), then increment across the i64::MAX boundary.
        trie.upsert_bytes(b"ctr", start).expect("seed base value");
        assert_eq!(trie.get_value_bytes(b"ctr"), Some(start));

        // +10 across several calls (each a small positive delta).
        let mut last = start;
        for _ in 0..10 {
            last = trie.increment_bytes(b"ctr", 1).expect("increment +1");
        }
        assert_eq!(
            last, crossed,
            "increment crossing i64::MAX must return the exact unsigned magnitude (not a reject/wrap)"
        );
        // The live read is the true u64 (> i64::MAX), NOT a negative/wrapped value.
        assert_eq!(trie.get_value_bytes(b"ctr"), Some(crossed));
        assert!(
            trie.get_value_bytes(b"ctr").expect("present") > FIRST_OVER_I64,
            "the stored count must exceed i64::MAX"
        );

        // CHECKPOINT — fold the value into the durable IMAGE (so the reopen reads the
        // image, not WAL replay), then DROP.
        trie.checkpoint().expect("checkpoint");
    }

    // REOPEN — the > i64::MAX value MUST survive through the checkpoint image as the
    // exact u64 (NOT wrapped/negative). This is the headline data-loss assertion.
    {
        let mut trie = PersistentARTrie::<u64>::open(&path).expect("reopen<u64>");
        assert_eq!(
            MappedDictionary::get_value(&trie, "ctr"),
            Some(crossed),
            "the > i64::MAX counter MUST survive checkpoint→reopen exactly (u64 restoration)"
        );
        assert_eq!(trie.get_value_bytes(b"ctr"), Some(crossed));

        // (3) DECREMENT back across the i64::MAX boundary → exact. The public signed
        // `increment` with a negative delta routes to the value-CAS path.
        let back = trie
            .increment_bytes(b"ctr", -10)
            .expect("decrement back across i64::MAX");
        assert_eq!(
            back, start,
            "decrement across i64::MAX must land on the exact value (no underflow/wrap)"
        );
        assert_eq!(trie.get_value_bytes(b"ctr"), Some(start));
    }

    // (4) u64 OVERFLOW — push to u64::MAX, then +1 → graceful Err (NOT a silent wrap).
    {
        let mut trie = PersistentARTrie::<u64>::open(&path).expect("reopen<u64> for overflow");
        trie.upsert_bytes(b"max", u64::MAX).expect("set u64::MAX");
        assert_eq!(trie.get_value_bytes(b"max"), Some(u64::MAX));
        let overflow = trie.increment_bytes(b"max", 1);
        assert!(
            overflow.is_err(),
            "incrementing past u64::MAX must be a graceful Err, not a silent wrap to 0; got {:?}",
            overflow
        );
        assert_eq!(
            trie.get_value_bytes(b"max"),
            Some(u64::MAX),
            "the rejected overflow must leave the counter at u64::MAX (no partial write)"
        );

        // (5) BELOW-ZERO decrement → graceful Err (NOT a u64 underflow wrap).
        trie.upsert_bytes(b"low", 5).expect("set small value");
        let underflow = trie.increment_bytes(b"low", -10);
        assert!(
            underflow.is_err(),
            "a below-zero decrement must be a graceful Err, not a u64 underflow wrap; got {:?}",
            underflow
        );
        assert_eq!(
            trie.get_value_bytes(b"low"),
            Some(5),
            "the rejected below-zero decrement must leave the counter unchanged"
        );
    }
}

/// **CHAR `<u64>` — the full data-loss matrix (UTF-8 keys).**
#[test]
fn char_counter_crosses_i64_max_survives_checkpoint_reopen_and_rejects_overflow() {
    let dir = scratch("u64-above-i64max-char");
    let path = dir.path().join("t.artc");

    let start: u64 = FIRST_OVER_I64 - 3; // == i64::MAX - 2
    let crossed: u64 = start + 10; // == i64::MAX + 7  (> i64::MAX)
    assert!(crossed > FIRST_OVER_I64, "the fixture must cross i64::MAX");

    {
        let mut trie = PersistentARTrieChar::<u64>::create(&path).expect("create<u64>");
        trie.upsert("ctr", start).expect("seed base value");
        assert_eq!(trie.get_value("ctr"), Some(start));

        let mut last = start;
        for _ in 0..10 {
            last = trie.increment("ctr", 1).expect("increment +1");
        }
        assert_eq!(
            last, crossed,
            "increment crossing i64::MAX must return the exact unsigned magnitude (not a reject/wrap)"
        );
        assert_eq!(trie.get_value("ctr"), Some(crossed));
        assert!(
            trie.get_value("ctr").expect("present") > FIRST_OVER_I64,
            "the stored count must exceed i64::MAX"
        );

        trie.checkpoint().expect("checkpoint");
    }

    {
        let mut trie = PersistentARTrieChar::<u64>::open(&path).expect("reopen<u64>");
        assert_eq!(
            MappedDictionary::get_value(&trie, "ctr"),
            Some(crossed),
            "the > i64::MAX char counter MUST survive checkpoint→reopen exactly (u64 restoration)"
        );
        assert!(Dictionary::contains(&trie, "ctr"));

        let back = trie
            .increment("ctr", -10)
            .expect("decrement back across i64::MAX");
        assert_eq!(
            back, start,
            "decrement across i64::MAX must land on the exact value (no underflow/wrap)"
        );
        assert_eq!(trie.get_value("ctr"), Some(start));
    }

    {
        let mut trie = PersistentARTrieChar::<u64>::open(&path).expect("reopen<u64> for overflow");
        trie.upsert("max", u64::MAX).expect("set u64::MAX");
        assert_eq!(trie.get_value("max"), Some(u64::MAX));
        let overflow = trie.increment("max", 1);
        assert!(
            overflow.is_err(),
            "incrementing past u64::MAX must be a graceful Err, not a silent wrap to 0; got {:?}",
            overflow
        );
        assert_eq!(trie.get_value("max"), Some(u64::MAX));

        trie.upsert("low", 5).expect("set small value");
        let underflow = trie.increment("low", -10);
        assert!(
            underflow.is_err(),
            "a below-zero decrement must be a graceful Err, not a u64 underflow wrap; got {:?}",
            underflow
        );
        assert_eq!(trie.get_value("low"), Some(5));
    }
}

/// **Pure-WAL-replay (NO checkpoint) twin — the Order-A durability witness for a
/// > i64::MAX count.** A durable increment crossing i64::MAX survives a reopen with
/// NO checkpoint via the delta-based `BatchIncrement` WAL records (deltas are
/// commutative; recovery sums them in the i128 substrate). This exercises the
/// recovery applier's bit-pattern-faithful absolute/delta decode for a count whose
/// i64 WAL `result` field is NEGATIVE.
#[test]
fn byte_counter_above_i64_max_survives_pure_wal_replay() {
    use libdictenstein::persistent_artrie::core::durability::DurabilityPolicy;
    let dir = scratch("u64-above-i64max-byte-wal");
    let path = dir.path().join("t.part");

    let start: u64 = FIRST_OVER_I64 - 3;
    let crossed: u64 = start + 10;

    {
        let mut trie = PersistentARTrie::<u64>::create(&path).expect("create<u64>");
        trie.set_durability_policy(DurabilityPolicy::Immediate);
        // Seed via a durable increment of a large (but i64-fitting) delta, then +10.
        let seed_delta = i64::try_from(start).expect("start fits i64");
        trie.increment_bytes(b"ctr", seed_delta)
            .expect("durable seed");
        let mut last = start;
        for _ in 0..10 {
            last = trie.increment_bytes(b"ctr", 1).expect("durable +1");
        }
        assert_eq!(last, crossed);
        // NO checkpoint — durability rests entirely on the WAL.
    }

    let trie = PersistentARTrie::<u64>::open(&path).expect("reopen<u64>");
    assert_eq!(
        trie.get_value_bytes(b"ctr"),
        Some(crossed),
        "a > i64::MAX counter MUST survive pure-WAL-replay reopen exactly (Order-A + u64 restoration)"
    );
}

/// **The flip.rs `counter_value_from_i64` overlay-reconcile regression (BLOCKER).**
///
/// An ABSOLUTE `WalRecord::Increment` carries the post-increment count in its `i64`
/// `result` field as the BIT-PATTERN (`counter_return_i64`), which is NEGATIVE for a
/// `u64` count > `i64::MAX`. When the F5 reopen reconciles such a record INTO THE
/// OVERLAY (`replay_records_lww_overlay` → `apply_recovered_operation_overlay` →
/// `counter_value_from_i64`), the decode MUST go via the LEAF BYTES (recovering the
/// `u64` magnitude), NOT `v as i128` (which keeps it negative → `i128_to_counter_value::
/// <u64>` rejects it → the record is silently DROPPED → the counter reverts = data
/// loss). This is the regime the unit/checkpoint/pure-WAL-delta tests above do NOT
/// reach (overlay increments log `BatchIncrement` deltas, never an absolute
/// `Increment`), so the bug survived 2670 green tests until the diff red-team.
///
/// We construct it directly: a valid Overlay-regime trie file (empty image, no
/// checkpoint so `checkpoint_lsn == 0`) + an injected Overlay-regime WAL whose sole
/// record is an absolute `Increment` with the negative bit-pattern `result`; reopen
/// via the F5 loader and assert the counter decodes to the exact `u64`.
#[test]
fn byte_overlay_reconcile_of_absolute_increment_recovers_u64_above_i64max() {
    let dir = scratch("u64-above-i64max-byte-overlay-reconcile");
    let path = dir.path().join("t.part");
    let crossed: u64 = FIRST_OVER_I64 + 6; // == i64::MAX + 7  (> i64::MAX)

    // A valid Overlay-regime trie file with an empty image (eligible-V `create`
    // auto-flips to Overlay; dropping without a checkpoint leaves `checkpoint_lsn == 0`
    // so the injected WAL tail at LSN ≥ 1 is replayed).
    {
        let _ = PersistentARTrie::<u64>::create(&path).expect("create overlay <u64>");
    }

    // The i64 `result` bit-pattern of a u64 > i64::MAX is NEGATIVE.
    let result_bp = i64::from_le_bytes(crossed.to_le_bytes());
    assert!(
        result_bp < 0,
        "fixture invariant: a u64 > i64::MAX has a negative i64 bit-pattern"
    );
    inject_ranked_absolute_increment(&path.with_extension("wal"), b"ctr", result_bp);

    let trie = PersistentARTrie::<u64>::open_with_f5_loader(&path).expect("f5 overlay reopen");
    assert_eq!(
        trie.get_value_bytes(b"ctr"),
        Some(crossed),
        "an absolute WalRecord::Increment of a u64 > i64::MAX reconciled INTO THE OVERLAY must \
         leaf-decode to the exact magnitude (flip.rs fix), not be dropped (the `v as i128` bug \
         would silently revert it)"
    );
}

/// **The durability linchpin — RAW (non-WAL) `increment_cas` + checkpoint + reopen.**
///
/// libgrammstein's hot path increments n-gram counts via the lock-free, NON-durable
/// `increment_cas` (no WAL record per call) and relies entirely on `checkpoint()` to
/// fold the overlay into the durable image. This asserts that pattern works for a
/// `u64` count that CROSSES `i64::MAX` via small `+1` `increment_cas` calls (the exact
/// case the old i64-domain seam spuriously rejected): the raw increments succeed, the
/// live `get_lockfree` is the exact magnitude, and the count SURVIVES checkpoint→drop→
/// reopen as the exact `u64` (read back via both `get_lockfree` and `get_value_bytes`,
/// which now read the same overlay leaf — the single source of truth).
#[test]
fn byte_raw_increment_cas_above_i64max_survives_checkpoint_reopen() {
    let dir = scratch("u64-raw-incrementcas-byte");
    let path = dir.path().join("t.part");
    let seed: u64 = FIRST_OVER_I64 - 3; // == i64::MAX - 2 (fits i64)
    let crossed: u64 = seed + 9; // == i64::MAX + 7  (> i64::MAX)
    assert!(crossed > FIRST_OVER_I64, "fixture must cross i64::MAX");

    {
        let trie = PersistentARTrie::<u64>::create(&path).expect("create<u64>");
        // RAW lock-free increment (NO WAL) — `increment_cas` takes `&self`.
        let after_seed = trie.increment_cas(b"ng", seed);
        assert_eq!(after_seed, seed, "first raw increment_cas sets the seed");
        let mut last = after_seed;
        for _ in 0..9 {
            last = trie.increment_cas(b"ng", 1); // each +1 CROSSES i64::MAX mid-way
        }
        assert_eq!(
            last, crossed,
            "raw increment_cas crossing i64::MAX must return the exact magnitude (old i64 seam rejected)"
        );
        assert_eq!(trie.get_lockfree(b"ng"), Some(crossed));
        // The durability linchpin: fold the NON-WAL overlay writes into the image.
        trie.checkpoint()
            .expect("checkpoint folds the non-WAL overlay into the image");
    }

    // Reopen — the non-WAL increment_cas count MUST survive via the checkpoint image.
    let trie = PersistentARTrie::<u64>::open(&path).expect("reopen<u64>");
    assert_eq!(
        trie.get_lockfree(b"ng"),
        Some(crossed),
        "#4: a non-WAL increment_cas u64 > i64::MAX MUST survive checkpoint→reopen (durability linchpin)"
    );
    assert_eq!(
        trie.get_value_bytes(b"ng"),
        Some(crossed),
        "get_value_bytes reads the same overlay leaf as get_lockfree (single source of truth, no double-count)"
    );
}

/// Char twin of the flip.rs overlay-reconcile regression.
#[test]
fn char_overlay_reconcile_of_absolute_increment_recovers_u64_above_i64max() {
    let dir = scratch("u64-above-i64max-char-overlay-reconcile");
    let path = dir.path().join("t.artc");
    let crossed: u64 = FIRST_OVER_I64 + 6;

    {
        let _ = PersistentARTrieChar::<u64>::create(&path).expect("create overlay <u64>");
    }

    let result_bp = i64::from_le_bytes(crossed.to_le_bytes());
    assert!(result_bp < 0, "fixture invariant: negative i64 bit-pattern");
    inject_ranked_absolute_increment(&path.with_extension("wal"), "ctr".as_bytes(), result_bp);

    let trie = PersistentARTrieChar::<u64>::open_with_f5_loader(&path).expect("f5 overlay reopen");
    assert_eq!(
        trie.get_value("ctr"),
        Some(crossed),
        "char absolute WalRecord::Increment of a u64 > i64::MAX reconciled INTO THE OVERLAY must \
         leaf-decode to the exact magnitude (flip.rs fix), not be dropped"
    );
}

/// **#7 — `iter_prefix_with_values` returns the exact u64 (> i64::MAX) for every final.**
///
/// libgrammstein enumerates the overlay via `iter_prefix_with_values` for MKN
/// statistics / shard-merge; a missed entry or a truncated value would silently
/// under-count. This pins, for BYTE `<u64>`: (a) COMPLETENESS — every final under the
/// prefix is enumerated; (b) the VALUE is the exact unsigned magnitude even ABOVE
/// i64::MAX (the read returns the stored `V` directly — no i64 truncation). Live AND
/// after checkpoint→reopen. (The E1 read-flip correspondence test covers the mechanism
/// for small u64; this nails the > i64::MAX value specifically.)
#[test]
fn byte_iter_prefix_with_values_returns_u64_above_i64max() {
    let dir = scratch("u64-iter-prefix-byte");
    let path = dir.path().join("t.part");
    let seed: u64 = FIRST_OVER_I64 - 3; // i64::MAX - 2
    let big: u64 = seed + 9; // i64::MAX + 7  (> i64::MAX)

    let collect = |trie: &PersistentARTrie<u64>| -> std::collections::BTreeMap<Vec<u8>, u64> {
        trie.iter_prefix_with_values(b"")
            .expect("prefix \"\" present")
            .collect()
    };

    {
        let mut trie = PersistentARTrie::<u64>::create(&path).expect("create<u64>");
        trie.upsert_bytes(b"big", seed).expect("seed big");
        for _ in 0..9 {
            trie.increment_bytes(b"big", 1).expect("cross i64::MAX");
        }
        trie.upsert_bytes(b"small", 42).expect("small");
        assert_eq!(trie.get_value_bytes(b"big"), Some(big));

        let map = collect(&trie);
        assert_eq!(
            map.len(),
            2,
            "iter must enumerate ALL finals (no missed entries)"
        );
        assert_eq!(
            map.get(b"big".as_slice()).copied(),
            Some(big),
            "iter_prefix_with_values must return the exact u64 > i64::MAX (no truncation)"
        );
        assert_eq!(map.get(b"small".as_slice()).copied(), Some(42));

        trie.checkpoint().expect("checkpoint");
    }

    let trie = PersistentARTrie::<u64>::open(&path).expect("reopen<u64>");
    let map = collect(&trie);
    assert_eq!(
        map.len(),
        2,
        "after reopen, iter must still enumerate ALL finals"
    );
    assert_eq!(
        map.get(b"big".as_slice()).copied(),
        Some(big),
        "after reopen, iter must still return the exact u64 > i64::MAX"
    );
}

/// Char twin of the `iter_prefix_with_values` > i64::MAX enumeration test.
#[test]
fn char_iter_prefix_with_values_returns_u64_above_i64max() {
    let dir = scratch("u64-iter-prefix-char");
    let path = dir.path().join("t.artc");
    let seed: u64 = FIRST_OVER_I64 - 3;
    let big: u64 = seed + 9;

    let collect = |trie: &PersistentARTrieChar<u64>| -> std::collections::BTreeMap<String, u64> {
        trie.iter_prefix_with_values("")
            .expect("iter ok")
            .expect("prefix \"\" present")
            .into_iter()
            .collect()
    };

    {
        let mut trie = PersistentARTrieChar::<u64>::create(&path).expect("create<u64>");
        trie.upsert("big", seed).expect("seed big");
        for _ in 0..9 {
            trie.increment("big", 1).expect("cross i64::MAX");
        }
        trie.upsert("small", 42).expect("small");
        assert_eq!(trie.get_value("big"), Some(big));

        let map = collect(&trie);
        assert_eq!(
            map.len(),
            2,
            "iter must enumerate ALL finals (no missed entries)"
        );
        assert_eq!(
            map.get("big").copied(),
            Some(big),
            "char iter_prefix_with_values must return the exact u64 > i64::MAX (no truncation)"
        );
        assert_eq!(map.get("small").copied(), Some(42));

        trie.checkpoint().expect("checkpoint");
    }

    let trie = PersistentARTrieChar::<u64>::open(&path).expect("reopen<u64>");
    let map = collect(&trie);
    assert_eq!(
        map.len(),
        2,
        "after reopen, iter must still enumerate ALL finals"
    );
    assert_eq!(
        map.get("big").copied(),
        Some(big),
        "after reopen, char iter must still return the exact u64 > i64::MAX"
    );
}
