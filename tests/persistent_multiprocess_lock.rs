//! First TRUE cross-process test: the Tier-1 advisory lock on the `.wlock` sidecar must reject a
//! SECOND OS process opening the same backing file with `FileLocked`, and admit it once the first
//! process releases (drops) its handle. The standard library maps this to `flock` on Unix and
//! `LockFileEx` on Windows.
//!
//! Mechanism: the test binary re-invokes ITSELF as the child (`current_exe` + `--exact` this test),
//! selected by the `LOCK_CHILD_OPEN_PATH` env var. In child mode it attempts to open the trie at
//! that path and reports the outcome via its process EXIT CODE (42 = FileLocked, 43 = opened,
//! 1 = other error). The parent asserts: locked while it holds the handle, then openable after it
//! drops. (Distinct non-zero success/locked codes so a mis-filtered "0 tests" child — which exits
//! 0 — trips both asserts rather than passing silently.)
//!
//! The lock lives in the shared `DiskManager`, so it covers the byte/char tries identically; the
//! whole persistent-artrie test suite passing with the lock in place verifies that uniformity. The
//! vocab handle is exercised here as the representative case.
//!
//! Real disk only (the child opens the same inode — see the tmpfs gotcha; uses `target/test-tmp`,
//! never a tmpfs tempdir).

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::vocab::PersistentVocabARTrie;
use libdictenstein::persistent_artrie::PersistentARTrieError;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

const CHILD_PATH_ENV: &str = "LOCK_CHILD_OPEN_PATH";
const EXIT_LOCKED: i32 = 42;
const EXIT_OPENED: i32 = 43;
const EXIT_OTHER: i32 = 1;
const TEST_NAME: &str = "multiprocess_lock_second_process_rejected";

/// A real-disk (NOT tmpfs) scratch directory unique to this process + instant.
fn scratch_dir() -> PathBuf {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("test-tmp");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = base.join(format!("mplock_{}_{}", std::process::id(), nanos));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Child role: if re-invoked with `LOCK_CHILD_OPEN_PATH` set, try to open the trie there and report
/// the outcome via the process exit code. Never returns in child mode.
fn run_as_child_if_selected() {
    let Ok(path) = std::env::var(CHILD_PATH_ENV) else {
        return; // parent mode
    };
    let code = match PersistentVocabARTrie::open_with_recovery(&path) {
        Ok(_) => EXIT_OPENED,
        Err(PersistentARTrieError::FileLocked { .. }) => EXIT_LOCKED,
        Err(_) => EXIT_OTHER,
    };
    std::process::exit(code);
}

/// Spawn this test binary as a child pointed at `path`; return its exit code.
fn spawn_child(path: &Path) -> i32 {
    let exe = std::env::current_exe().expect("current_exe");
    Command::new(exe)
        .args(["--exact", TEST_NAME, "--test-threads=1"])
        .env(CHILD_PATH_ENV, path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn child test process")
        .code()
        .expect("child terminated by signal (no exit code)")
}

#[test]
fn multiprocess_lock_second_process_rejected() {
    // Child mode (re-invoked by the parent) — do the child work and exit before any parent logic.
    run_as_child_if_selected();

    let dir = scratch_dir();
    let path = dir.join("v.vocab");

    // Parent opens (and thereby holds the Tier-1 exclusive lock on `<path>.wlock`).
    let trie = PersistentVocabARTrie::create(&path).expect("parent create");
    trie.insert("alpha").expect("seed insert");
    trie.checkpoint().expect("checkpoint");

    // (1) While the parent holds the handle, a second OS process must be rejected with FileLocked.
    assert_eq!(
        spawn_child(&path),
        EXIT_LOCKED,
        "a second process opening the same file must be rejected with FileLocked"
    );

    // Release the parent's handle → the lock is freed on drop.
    drop(trie);

    // (2) Now a second OS process must be able to open the file.
    assert_eq!(
        spawn_child(&path),
        EXIT_OPENED,
        "after the owner drops its handle, a second process must be able to open the file"
    );

    // Best-effort cleanup (target/test-tmp is real disk, not an auto-removed tempdir).
    let _ = std::fs::remove_dir_all(&dir);
}
