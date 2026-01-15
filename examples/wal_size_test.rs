use libdictenstein::persistent_artrie::{PersistentARTrie, WalWriter, WalConfig};
use tempfile::tempdir;
use std::fs;

fn main() {
    let size = 10000;
    
    // Generate terms
    let terms: Vec<String> = (0..size)
        .map(|i| format!("term_{:08}", i))
        .collect();
    
    // Test individual inserts
    let dir1 = tempdir().expect("create temp dir");
    let path1 = dir1.path().join("individual.artrie");
    {
        let mut trie: PersistentARTrie<()> = PersistentARTrie::create(&path1).expect("create trie");
        for term in &terms {
            trie.insert(term);
        }
        trie.sync().expect("sync");
    }
    let wal_path1 = dir1.path().join("individual.wal");
    let individual_size = fs::metadata(&wal_path1).map(|m| m.len()).unwrap_or(0);
    
    // Test batch inserts
    let dir2 = tempdir().expect("create temp dir");
    let path2 = dir2.path().join("batch.artrie");
    {
        let mut trie: PersistentARTrie<()> = PersistentARTrie::create(&path2).expect("create trie");
        let entries: Vec<(String, Option<()>)> = terms.iter().map(|t| (t.clone(), None)).collect();
        trie.insert_batch(&entries);
        trie.sync().expect("sync");
    }
    let wal_path2 = dir2.path().join("batch.wal");
    let batch_size = fs::metadata(&wal_path2).map(|m| m.len()).unwrap_or(0);
    
    // Calculate header overhead
    // Individual: 17-byte header per record = 17 * 10000 = 170,000 bytes of headers
    // Batch: 21-byte header for one record = 21 bytes of headers
    let individual_header_overhead = 17 * size;
    let batch_header_overhead = 21;
    
    println!("=== WAL Size Comparison for {} terms ===", size);
    println!();
    println!("Individual inserts WAL size: {} bytes", individual_size);
    println!("Batch insert WAL size:       {} bytes", batch_size);
    println!();
    println!("Size reduction: {} bytes ({:.1}%)", 
             individual_size as i64 - batch_size as i64,
             (1.0 - (batch_size as f64 / individual_size as f64)) * 100.0);
    println!();
    println!("Expected header overhead (individual): {} bytes", individual_header_overhead);
    println!("Expected header overhead (batch):      {} bytes", batch_header_overhead);
    println!("Header overhead reduction:             {:.1}%", 
             (1.0 - (batch_header_overhead as f64 / individual_header_overhead as f64)) * 100.0);
}
