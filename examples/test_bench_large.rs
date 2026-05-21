//! Test create/open cycle with 1M terms

use libdictenstein::persistent_artrie_char::PersistentARTrieChar;
use std::time::Instant;
use tempfile::TempDir;

fn main() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let trie_path = temp_dir.path().join("test_trie");

    println!("Creating trie with 1M terms at: {:?}", trie_path);

    // Create and populate trie
    {
        let mut trie =
            PersistentARTrieChar::<u64>::create(&trie_path).expect("Failed to create trie");

        let start = Instant::now();
        for i in 0..1_000_000 {
            let term = match i % 10 {
                0..=3 => format!("term{:05}", i),
                4..=6 => format!("prefix{}suffix{:06}end", i % 100, i),
                7..=8 => format!(
                    "very_long_prefix_{}_middle_section_{:08}_suffix_ending",
                    i % 1000,
                    i
                ),
                _ => format!("日本語テスト{:05}", i),
            };
            trie.upsert(&term, i as u64).expect("Failed to insert");
        }
        println!("Inserted 1M terms in {:?}", start.elapsed());

        let start = Instant::now();
        trie.checkpoint().expect("Failed to checkpoint");
        println!("Checkpoint complete in {:?}", start.elapsed());
    }

    // Open multiple times
    for attempt in 1..=5 {
        println!("\nAttempt {}: Opening trie...", attempt);
        let start = Instant::now();
        match PersistentARTrieChar::<u64>::open(&trie_path) {
            Ok(trie) => {
                let elapsed = start.elapsed();
                let found = trie.contains("term00050");
                println!(
                    "  Open succeeded in {:?}! contains(term00050) = {}",
                    elapsed, found
                );
            }
            Err(e) => {
                println!("  Open FAILED: {:?}", e);
            }
        }
    }

    println!("\nDone!");
}
