#![cfg(feature = "persistent-artrie")]
//! **Regression: vocab checkpoint at scale (multi-arena) round-trips through reopen.**
//!
//! Guards the fixed data-loss bug where the generic `allocate_block` misread vocab's `VOCB` header
//! `checkpoint_lsn` (bytes 32..40) as the standard `FileHeader.free_list_head` (identical offset).
//! After the first checkpoint set a non-zero `checkpoint_lsn`, any subsequent checkpoint that
//! allocated a NEW block — i.e. the serialized image spanned multiple arenas (~a few thousand
//! terms) — received a bogus `block_id` (`checkpoint_lsn >> 40` = 0), overwrote the header block,
//! and lost ALL data on reopen (`CorruptedFile: Invalid arena ID N`). Every prior vocab test used a
//! single arena, so it went unnoticed. Fixed by gating the free-list on the `FileHeader` magic in
//! `MmapDiskManager`/`IoUringDiskManager::{allocate_block, free_block}`. `N` is chosen large enough
//! to span multiple arenas for both the base checkpoint path and `fork_to`. Single-threaded.

use libdictenstein::persistent_artrie::vocab::PersistentVocabARTrie;

fn scratch(tag: &str) -> std::path::PathBuf {
    std::fs::create_dir_all("target/test-tmp").ok();
    let p = std::path::PathBuf::from(format!(
        "target/test-tmp/scale-repro-{tag}-{}.vocab",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&p);
    let _ = std::fs::remove_file(p.with_extension("vocab.wal"));
    p
}

const N: u64 = 12_000;

#[test]
fn base_checkpoint_reopen_at_scale() {
    let path = scratch("base");
    let trie = PersistentVocabARTrie::create(&path).unwrap();
    for i in 0..N {
        trie.insert(&format!("term-{i:08}")).unwrap();
    }
    let n = trie.len();
    trie.checkpoint().unwrap();
    drop(trie);

    let (re, _r) = PersistentVocabARTrie::open_with_recovery(&path).expect("base reopen");
    assert_eq!(re.len(), n, "base reopen len");
    for i in 0..N {
        assert!(re.contains(&format!("term-{i:08}")), "base missing {i}");
    }
    drop(re);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("vocab.wal"));
}

#[test]
fn fork_at_scale_no_concurrency() {
    let src_path = scratch("forksrc");
    let src = PersistentVocabARTrie::create(&src_path).unwrap();
    for i in 0..N {
        src.insert(&format!("term-{i:08}")).unwrap();
    }
    let n = src.len();

    let fork_path = scratch("fork");
    let fork = src.fork_to(&fork_path).expect("fork_to");
    assert_eq!(fork.len(), n, "fork len");
    drop(fork);

    let (re, _r) = PersistentVocabARTrie::open_with_recovery(&fork_path).expect("fork reopen");
    assert_eq!(re.len(), n, "fork reopen len");
    for i in 0..N {
        assert!(re.contains(&format!("term-{i:08}")), "fork missing {i}");
    }
    drop(re);
    drop(src);
    let _ = std::fs::remove_file(&src_path);
    let _ = std::fs::remove_file(src_path.with_extension("vocab.wal"));
    let _ = std::fs::remove_file(&fork_path);
    let _ = std::fs::remove_file(fork_path.with_extension("vocab.wal"));
}
