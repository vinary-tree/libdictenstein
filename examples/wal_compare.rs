use libdictenstein::persistent_artrie::{PersistentARTrie, WalReader};
use std::fs;
use tempfile::tempdir;

fn main() {
    for size in [100, 1000, 10000] {
        println!("=== Testing with {} terms ===", size);

        // Generate terms
        let terms: Vec<String> = (0..size).map(|i| format!("term_{:08}", i)).collect();

        // Test individual inserts
        let dir1 = tempdir().expect("create temp dir");
        let path1 = dir1.path().join("individual.artrie");
        let wal_path1 = dir1.path().join("individual.wal");

        {
            let trie: PersistentARTrie<()> = PersistentARTrie::create(&path1).expect("create trie");
            for term in &terms {
                trie.insert(term);
            }
            trie.sync().expect("sync");
        }

        let individual_size = fs::metadata(&wal_path1).map(|m| m.len()).unwrap_or(0);
        let reader1 = WalReader::new(&wal_path1).expect("open WAL");
        let individual_records = reader1.iter().count();

        // Test batch inserts
        let dir2 = tempdir().expect("create temp dir");
        let path2 = dir2.path().join("batch.artrie");
        let wal_path2 = dir2.path().join("batch.wal");

        {
            let trie: PersistentARTrie<()> = PersistentARTrie::create(&path2).expect("create trie");
            let entries: Vec<(String, Option<()>)> =
                terms.iter().map(|t| (t.clone(), None)).collect();
            trie.insert_batch(&entries);
            trie.sync().expect("sync");
        }

        let batch_size = fs::metadata(&wal_path2).map(|m| m.len()).unwrap_or(0);
        let reader2 = WalReader::new(&wal_path2).expect("open WAL");
        let batch_records = reader2.iter().count();

        println!(
            "  Individual: {} bytes, {} WAL records ({:.1} bytes/term)",
            individual_size,
            individual_records,
            individual_size as f64 / size as f64
        );
        println!(
            "  Batch:      {} bytes, {} WAL record(s) ({:.1} bytes/term)",
            batch_size,
            batch_records,
            batch_size as f64 / size as f64
        );

        let size_diff = batch_size as i64 - individual_size as i64;
        if size_diff > 0 {
            println!(
                "  Result: Batch is {} bytes LARGER ({:.1}% larger)",
                size_diff,
                (size_diff as f64 / individual_size as f64) * 100.0
            );
        } else {
            println!(
                "  Result: Batch is {} bytes SMALLER ({:.1}% reduction)",
                -size_diff,
                (-size_diff as f64 / individual_size as f64) * 100.0
            );
        }
        println!();
    }
}
