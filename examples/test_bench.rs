//! Simple test to verify create/open cycle works correctly

use libdictenstein::persistent_artrie_char::DiskBackedCharTrieInner;
use tempfile::TempDir;

fn main() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let trie_path = temp_dir.path().join("test_trie");
    
    println!("Creating trie at: {:?}", trie_path);
    
    // Create and populate trie
    {
        let mut trie = DiskBackedCharTrieInner::<u64>::create(&trie_path)
            .expect("Failed to create trie");
        
        for i in 0..1000 {
            let term = format!("term{:05}", i);
            trie.upsert(&term, i as u64).expect("Failed to insert");
        }
        
        println!("Inserted 1000 terms, checkpointing...");
        trie.checkpoint().expect("Failed to checkpoint");
        println!("Checkpoint complete");
    }
    
    // Open multiple times
    for attempt in 1..=10 {
        println!("\nAttempt {}: Opening trie...", attempt);
        match DiskBackedCharTrieInner::<u64>::open(&trie_path) {
            Ok(trie) => {
                let found = trie.contains("term00050");
                println!("  Open succeeded! contains(term00050) = {}", found);
            }
            Err(e) => {
                println!("  Open FAILED: {:?}", e);
            }
        }
    }
    
    println!("\nDone!");
}
