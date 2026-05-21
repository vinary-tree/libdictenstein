use libdictenstein::persistent_artrie::{PersistentARTrie, WalReader};
use libdictenstein::Dictionary;
use tempfile::tempdir;

fn main() {
    let dir = tempdir().expect("create temp dir");
    let path = dir.path().join("test.artrie");

    let mut trie: PersistentARTrie<()> = PersistentARTrie::create(&path).expect("create trie");

    // Insert and trace
    for i in 0..300 {
        let term = format!("term_{:08}", i);
        let inserted = trie.insert(&term);
        if i >= 254 && i <= 260 {
            println!("Insert #{}: '{}' -> inserted={}", i, term, inserted);
        }
    }

    trie.sync().expect("sync");

    // Check WAL
    let wal_path = dir.path().join("test.wal");
    let reader = WalReader::new(&wal_path).expect("open WAL");
    let records: Vec<_> = reader.iter().collect();

    println!("\nTotal WAL records: {}", records.len());
    println!("Total terms in trie: {:?}", trie.len());
}
