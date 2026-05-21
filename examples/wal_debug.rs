use libdictenstein::persistent_artrie::{PersistentARTrie, WalReader};
use std::fs;
use tempfile::tempdir;

fn main() {
    let size = 100; // Small test

    // Generate terms
    let terms: Vec<String> = (0..size).map(|i| format!("term_{:08}", i)).collect();

    // Test individual inserts
    let dir = tempdir().expect("create temp dir");
    let path = dir.path().join("test.artrie");
    let wal_path = dir.path().join("test.wal");

    println!("=== Testing Individual Inserts ===");
    {
        let mut trie: PersistentARTrie<()> = PersistentARTrie::create(&path).expect("create trie");
        for term in &terms {
            trie.insert(term);
        }
        trie.sync().expect("sync");
    }

    let wal_size = fs::metadata(&wal_path).map(|m| m.len()).unwrap_or(0);
    println!("WAL file size: {} bytes", wal_size);

    // Read and count WAL records
    let reader = WalReader::new(&wal_path).expect("open WAL");
    let records: Vec<_> = reader.iter().collect();
    println!("Number of WAL records: {}", records.len());

    // Show first few records
    for (i, result) in records.iter().take(3).enumerate() {
        match result {
            Ok((lsn, record)) => println!("Record {}: LSN={}, type={:?}", i, lsn, record),
            Err(e) => println!("Record {}: Error {:?}", i, e),
        }
    }

    // Calculate expected vs actual size
    let expected_header_size = 17 * size; // 17 bytes header per record
    let expected_payload_size = size * (4 + 13 + 1); // term_len + "term_XXXXXXXX" + has_value
    let expected_total = expected_header_size + expected_payload_size;

    println!();
    println!("Expected header size: {} bytes", expected_header_size);
    println!("Expected payload size: {} bytes", expected_payload_size);
    println!("Expected total: {} bytes", expected_total);
    println!("Actual size: {} bytes", wal_size);

    if records.len() != size {
        println!(
            "\n*** WARNING: Expected {} records but found {}! ***",
            size,
            records.len()
        );
    }
}
