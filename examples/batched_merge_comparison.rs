//! Quick comparison of regular merge vs batched merge throughput

use libdictenstein::persistent_artrie::PersistentARTrie;
use libdictenstein::MutableMappedDictionary;
use std::time::Instant;
use tempfile::tempdir;

fn main() {
    println!("=== Batched Merge Comparison ===\n");

    let size = 50_000;
    println!("Test size: {} terms\n", size);

    // Create source trie
    let source_dir = tempdir().expect("create temp dir");
    let source_path = source_dir.path().join("source.artrie");
    let source: PersistentARTrie<i64> = PersistentARTrie::create(&source_path).expect("create source");

    for i in 0..size {
        source.insert_with_value(&format!("term_{:08}", i), i as i64);
    }
    source.sync().ok();
    println!("Source trie populated: {} terms", size);

    // Test 1: Regular merge
    {
        let target_dir = tempdir().expect("create temp dir");
        let target_path = target_dir.path().join("target.artrie");
        let target: PersistentARTrie<i64> = PersistentARTrie::create(&target_path).expect("create target");

        // Add 50% overlap
        for i in (size / 2)..(size + size / 2) {
            target.insert_with_value(&format!("term_{:08}", i), (i * 10) as i64);
        }
        target.sync().ok();

        let start = Instant::now();
        let count = target.merge_from(&source, |a, b| a + b).expect("regular merge");
        let elapsed = start.elapsed();

        println!("\nRegular merge:");
        println!("  Time: {:.2} ms", elapsed.as_secs_f64() * 1000.0);
        println!("  Terms processed: {}", count);
        println!("  Throughput: {:.2} Kelem/s", count as f64 / elapsed.as_secs_f64() / 1000.0);
    }

    // Test 2: Batched merge (batch_size=1000)
    {
        let target_dir = tempdir().expect("create temp dir");
        let target_path = target_dir.path().join("target.artrie");
        let target: PersistentARTrie<i64> = PersistentARTrie::create(&target_path).expect("create target");

        for i in (size / 2)..(size + size / 2) {
            target.insert_with_value(&format!("term_{:08}", i), (i * 10) as i64);
        }
        target.sync().ok();

        let start = Instant::now();
        let count = target.merge_from_batched(&source, |a, b| a + b, 1000).expect("batched merge");
        let elapsed = start.elapsed();

        println!("\nBatched merge (batch_size=1000):");
        println!("  Time: {:.2} ms", elapsed.as_secs_f64() * 1000.0);
        println!("  Terms processed: {}", count);
        println!("  Throughput: {:.2} Kelem/s", count as f64 / elapsed.as_secs_f64() / 1000.0);
    }

    // Test 3: Batched merge (default batch_size via 0)
    {
        let target_dir = tempdir().expect("create temp dir");
        let target_path = target_dir.path().join("target.artrie");
        let target: PersistentARTrie<i64> = PersistentARTrie::create(&target_path).expect("create target");

        for i in (size / 2)..(size + size / 2) {
            target.insert_with_value(&format!("term_{:08}", i), (i * 10) as i64);
        }
        target.sync().ok();

        let start = Instant::now();
        let count = target.merge_from_batched(&source, |a, b| a + b, 0).expect("batched merge");
        let elapsed = start.elapsed();

        println!("\nBatched merge (default batch_size=5000):");
        println!("  Time: {:.2} ms", elapsed.as_secs_f64() * 1000.0);
        println!("  Terms processed: {}", count);
        println!("  Throughput: {:.2} Kelem/s", count as f64 / elapsed.as_secs_f64() / 1000.0);
    }
}
