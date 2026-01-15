use libdictenstein::persistent_artrie::PersistentARTrie;
use libdictenstein::{Dictionary, MappedDictionary, MutableMappedDictionary};
use tempfile::tempdir;

fn main() {
    println!("=== Streaming Merge Test ===\n");

    // Create source trie
    let dir1 = tempdir().expect("create temp dir");
    let path1 = dir1.path().join("source.artrie");
    let source: PersistentARTrie<i64> = PersistentARTrie::create(&path1).expect("create source");

    // Insert terms with values
    for i in 0..1000 {
        source.insert_with_value(&format!("term_{:08}", i), i as i64);
    }

    println!("Source trie: {} terms", source.len().unwrap_or(0));

    // Create target trie
    let dir2 = tempdir().expect("create temp dir");
    let path2 = dir2.path().join("target.artrie");
    let target: PersistentARTrie<i64> = PersistentARTrie::create(&path2).expect("create target");

    // Add some overlapping terms with different values
    for i in 500..1500 {
        target.insert_with_value(&format!("term_{:08}", i), (i * 10) as i64);
    }

    println!("Target trie before merge: {} terms", target.len().unwrap_or(0));

    // Test 1: Regular merge (loads all at once)
    let dir3 = tempdir().expect("create temp dir");
    let path3 = dir3.path().join("regular.artrie");
    let regular: PersistentARTrie<i64> = PersistentARTrie::create(&path3).expect("create regular");

    for i in 500..1500 {
        regular.insert_with_value(&format!("term_{:08}", i), (i * 10) as i64);
    }

    let regular_count = regular.merge_from(&source, |a, b| a + b).expect("regular merge");
    println!("\nRegular merge: {} terms processed", regular_count);
    println!("Regular trie after merge: {} terms", regular.len().unwrap_or(0));

    // Test 2: Batched merge with batch_size=100
    let dir4 = tempdir().expect("create temp dir");
    let path4 = dir4.path().join("batched.artrie");
    let batched: PersistentARTrie<i64> = PersistentARTrie::create(&path4).expect("create batched");

    for i in 500..1500 {
        batched.insert_with_value(&format!("term_{:08}", i), (i * 10) as i64);
    }

    let batched_count = batched.merge_from_batched(&source, |a, b| a + b, 100).expect("batched merge");
    println!("\nBatched merge (batch_size=100): {} terms processed", batched_count);
    println!("Batched trie after merge: {} terms", batched.len().unwrap_or(0));

    // Verify correctness - check a few values
    println!("\n=== Verification ===");

    // Check a term only in source (term_0000)
    let val1_regular = regular.get_value("term_00000000");
    let val1_batched = batched.get_value("term_00000000");
    println!("term_00000000: regular={:?}, batched={:?}", val1_regular, val1_batched);

    // Check a term only in target (term_1200)
    let val2_regular = regular.get_value("term_00001200");
    let val2_batched = batched.get_value("term_00001200");
    println!("term_00001200: regular={:?}, batched={:?}", val2_regular, val2_batched);

    // Check an overlapping term (term_600) - should have merged values
    let val3_regular = regular.get_value("term_00000600");
    let val3_batched = batched.get_value("term_00000600");
    println!("term_00000600 (overlap): regular={:?}, batched={:?}", val3_regular, val3_batched);
    // Expected: 600 (from source) + 6000 (from target: 600*10) = 6600

    // Final check
    if regular.len() == batched.len() {
        println!("\n✓ Both tries have same length");
    } else {
        println!("\n✗ Length mismatch: regular={}, batched={}",
                 regular.len().unwrap_or(0), batched.len().unwrap_or(0));
    }

    if val3_regular == val3_batched {
        println!("✓ Merged values match");
    } else {
        println!("✗ Merged values differ");
    }
}
