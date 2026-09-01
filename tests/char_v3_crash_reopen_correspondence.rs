#![cfg(feature = "persistent-artrie")]

use std::path::Path;
use std::process::{Command, ExitStatus};

use libdictenstein::persistent_artrie::char::PersistentARTrieChar;

const CHILD_MODE: &str = "LIBDICTENSTEIN_CHAR_V3_CRASH_CHILD";
const CHILD_PATH: &str = "LIBDICTENSTEIN_CHAR_V3_CRASH_PATH";
const TEST_NAME: &str = "committed_character_checkpoint_reopens_after_exit_without_destructors";

fn entries() -> Vec<(String, u64)> {
    let mut entries = vec![
        ("λ".repeat(71), 1),
        (format!("{}終", "λ".repeat(71)), 2),
        ("🎉category-theory自然変換".to_string(), 3),
        ("é\u{301} normalization-is-explicit".to_string(), 4),
    ];
    for index in 0..64u32 {
        let first = char::from_u32(0x0400 + index).expect("valid scalar");
        entries.push((
            format!("{first}branch-{index:02}-{}", "δ".repeat(19)),
            100 + index as u64,
        ));
    }
    entries
}

fn child_checkpoint(path: &Path) -> ! {
    let trie = PersistentARTrieChar::<u64>::create(path).expect("child creates trie");
    for (term, value) in entries() {
        trie.upsert(&term, value).expect("child inserts term");
    }
    trie.checkpoint().expect("child commits checkpoint");

    // process::exit deliberately bypasses Rust destructors. The parent must
    // recover solely from bytes published before this point.
    std::process::exit(0)
}

fn assert_success(status: ExitStatus) {
    assert!(
        status.success(),
        "checkpoint child exited unsuccessfully: {status}"
    );
}

#[test]
fn committed_character_checkpoint_reopens_after_exit_without_destructors() {
    if std::env::var_os(CHILD_MODE).is_some() {
        let path = std::env::var_os(CHILD_PATH).expect("child path environment");
        child_checkpoint(Path::new(&path));
    }

    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("char-v3-crash.artc");
    let status = Command::new(std::env::current_exe().expect("current test executable"))
        .arg("--exact")
        .arg(TEST_NAME)
        .arg("--nocapture")
        .env(CHILD_MODE, "1")
        .env(CHILD_PATH, &path)
        .status()
        .expect("spawn checkpoint child");
    assert_success(status);

    let reopened = PersistentARTrieChar::<u64>::open(&path).expect("parent reopens checkpoint");
    for (term, expected) in entries() {
        assert_eq!(
            reopened.get(&term),
            Some(expected),
            "checkpoint lost or changed {term:?}"
        );
    }
}
