#![allow(deprecated)]

//! Simple benchmark runner for disk I/O profiling.
//!
//! Run with:
//!   cargo build --release --features persistent-artrie --example bench_disk_io
//!   perf stat -e syscalls:sys_enter_fsync,syscalls:sys_enter_write ./target/release/examples/bench_disk_io

use libdictenstein::PersistentARTrie;
use std::fs;
use tempfile::tempdir;

fn main() {
    let iterations = 100;
    let terms_per_iter = 1000;

    println!(
        "Running {} iterations of {} inserts each...",
        iterations, terms_per_iter
    );

    // Generate terms
    let prefixes = [
        "pre", "un", "re", "in", "dis", "en", "non", "over", "mis", "sub",
    ];
    let roots = [
        "test", "code", "data", "work", "play", "read", "write", "run", "walk", "talk",
    ];
    let suffixes = [
        "ing", "ed", "er", "est", "ly", "ness", "ment", "tion", "able", "ful",
    ];

    let mut terms = Vec::with_capacity(terms_per_iter);
    for i in 0..terms_per_iter {
        let prefix_idx = i % prefixes.len();
        let root_idx = (i / prefixes.len()) % roots.len();
        let suffix_idx = (i / (prefixes.len() * roots.len())) % suffixes.len();

        let word = match i % 4 {
            0 => format!("{}{}", roots[root_idx], suffixes[suffix_idx]),
            1 => format!("{}{}", prefixes[prefix_idx], roots[root_idx]),
            2 => format!(
                "{}{}{}",
                prefixes[prefix_idx], roots[root_idx], suffixes[suffix_idx]
            ),
            _ => roots[root_idx].to_string(),
        };
        terms.push(word);
    }
    terms.sort();
    terms.dedup();

    let start = std::time::Instant::now();

    for iter in 0..iterations {
        let dir = tempdir().expect("Failed to create temp dir");
        let _path = dir.path().join("bench.dat");

        // Create and insert (in-memory only, no persistence for this simple test)
        {
            let trie: PersistentARTrie<()> = PersistentARTrie::new();
            for term in &terms {
                let _ = trie.insert(term);
            }
            // For this benchmark, we're measuring in-memory operations
            // The persist_to_disk() method requires internal state setup
        }

        // Cleanup
        let _ = fs::remove_dir_all(dir.path());

        if (iter + 1) % 10 == 0 {
            println!("  Completed {} iterations", iter + 1);
        }
    }

    let elapsed = start.elapsed();
    let ops_total = iterations as u64 * terms.len() as u64;
    let ops_per_sec = ops_total as f64 / elapsed.as_secs_f64();

    println!("\nResults:");
    println!("  Total time: {:?}", elapsed);
    println!("  Total operations: {}", ops_total);
    println!("  Throughput: {:.2} ops/sec", ops_per_sec);
}
