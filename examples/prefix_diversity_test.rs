use libdictenstein::persistent_artrie::{PersistentARTrie, WalReader};
use libdictenstein::Dictionary;
use tempfile::tempdir;

fn main() {
    // Test 1: Same prefix (current failing case)
    println!("=== Test 1: Same prefix (term_XXXXXXXX) ===");
    {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test.artrie");
        let mut trie: PersistentARTrie<()> = PersistentARTrie::create(&path).expect("create");

        for i in 0..300 {
            trie.insert(&format!("term_{:08}", i));
        }
        trie.sync().ok();

        let wal_path = dir.path().join("test.wal");
        let records = WalReader::new(&wal_path).expect("read").iter().count();
        println!("Terms in trie: {:?}, WAL records: {}", trie.len(), records);
    }

    // Test 2: Diverse first characters
    println!("\n=== Test 2: Diverse first characters (a...z + A...Z) ===");
    {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test.artrie");
        let mut trie: PersistentARTrie<()> = PersistentARTrie::create(&path).expect("create");

        // 52 different first characters
        for c in ('a'..='z').chain('A'..='Z') {
            for i in 0..10 {
                trie.insert(&format!("{}_{:08}", c, i));
            }
        }
        trie.sync().ok();

        let wal_path = dir.path().join("test.wal");
        let records = WalReader::new(&wal_path).expect("read").iter().count();
        println!("Terms in trie: {:?}, WAL records: {}", trie.len(), records);
    }

    // Test 3: Hash-style diverse keys
    println!("\n=== Test 3: Hash-style keys (varied prefixes) ===");
    {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test.artrie");
        let mut trie: PersistentARTrie<()> = PersistentARTrie::create(&path).expect("create");

        // Use numeric prefix to spread across buckets
        for i in 0..500 {
            trie.insert(&format!("{:03}_{:08}", i % 256, i));
        }
        trie.sync().ok();

        let wal_path = dir.path().join("test.wal");
        let records = WalReader::new(&wal_path).expect("read").iter().count();
        println!("Terms in trie: {:?}, WAL records: {}", trie.len(), records);
    }
}
