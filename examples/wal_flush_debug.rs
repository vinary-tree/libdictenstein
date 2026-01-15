use libdictenstein::persistent_artrie::{PersistentARTrie, WalReader};
use tempfile::tempdir;
use std::fs;

fn main() {
    let size = 500;
    
    let terms: Vec<String> = (0..size)
        .map(|i| format!("term_{:08}", i))
        .collect();
    
    let dir = tempdir().expect("create temp dir");
    let path = dir.path().join("test.artrie");
    let wal_path = dir.path().join("test.wal");
    
    println!("=== Insert with periodic sync every 100 terms ===");
    {
        let mut trie: PersistentARTrie<()> = PersistentARTrie::create(&path).expect("create trie");
        for (i, term) in terms.iter().enumerate() {
            trie.insert(term);
            if (i + 1) % 100 == 0 {
                trie.sync().expect("sync");
                let size = fs::metadata(&wal_path).map(|m| m.len()).unwrap_or(0);
                let reader = WalReader::new(&wal_path).expect("open WAL");
                let records = reader.iter().count();
                println!("  After {} inserts: {} WAL records, {} bytes", 
                         i + 1, records, size);
            }
        }
        trie.sync().expect("final sync");
    }
    
    let final_size = fs::metadata(&wal_path).map(|m| m.len()).unwrap_or(0);
    let reader = WalReader::new(&wal_path).expect("open WAL");
    let final_records = reader.iter().count();
    println!("\nFinal: {} WAL records, {} bytes", final_records, final_size);
    
    if final_records != size {
        println!("\n*** BUG: Expected {} records but got {} ***", size, final_records);
    }
}
