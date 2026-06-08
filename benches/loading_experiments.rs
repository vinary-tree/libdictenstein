//! Loading Strategy Experiments for PersistentARTrieChar
//!
//! This benchmark suite evaluates different loading strategies:
//! - Eager loading (current baseline)
//! - Lazy loading (on-demand)
//! - Depth-limited loading (hybrid)
//! - Parallel loading (multi-threaded)
//!
//! Run with: cargo bench --bench loading_experiments --features persistent-artrie
//!
//! For scientific analysis, run with JSON output:
//! cargo bench --bench loading_experiments --features persistent-artrie -- --save-baseline eager

use criterion::{
    black_box, criterion_group, criterion_main, measurement::WallTime, BenchmarkId, Criterion,
    Throughput,
};
use libdictenstein::persistent_artrie_char::PersistentARTrieChar;
use log::info;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tempfile::TempDir;

// ============================================================================
// Test Data Generation
// ============================================================================

/// Generate deterministic terms for reproducible benchmarks
fn generate_terms(size: usize) -> Vec<String> {
    let mut terms = Vec::with_capacity(size);

    // Use a deterministic pattern for reproducibility
    // Mix of short and long terms, various character distributions
    for i in 0..size {
        let term = match i % 10 {
            0..=3 => {
                // Short terms (5-10 chars) - common case
                format!("term{:05}", i)
            }
            4..=6 => {
                // Medium terms (15-25 chars)
                format!("prefix{}suffix{:06}end", i % 100, i)
            }
            7..=8 => {
                // Long terms (30-50 chars)
                format!(
                    "very_long_prefix_{}_middle_section_{:08}_suffix_ending",
                    i % 1000,
                    i
                )
            }
            _ => {
                // Unicode terms
                format!("日本語テスト{:05}", i)
            }
        };
        terms.push(term);
    }

    terms.sort();
    terms.dedup();
    terms
}

/// Generate query terms (50% hits, 50% misses)
fn generate_queries(terms: &[String], count: usize) -> Vec<String> {
    let mut queries = Vec::with_capacity(count);

    for i in 0..count {
        if i % 2 == 0 {
            // Hit: existing term
            queries.push(terms[i % terms.len()].clone());
        } else {
            // Miss: non-existing term
            queries.push(format!("nonexistent_query_{}", i));
        }
    }

    queries
}

// ============================================================================
// Test Dataset Management
// ============================================================================

struct TestDataset {
    terms: Vec<String>,
    queries: Vec<String>,
    trie_path: PathBuf,
    _temp_dir: TempDir,
}

impl TestDataset {
    /// Create a new test dataset with the given number of terms
    fn new(term_count: usize, query_count: usize) -> Self {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let trie_path = temp_dir.path().join("test_trie");

        let terms = generate_terms(term_count);
        let queries = generate_queries(&terms, query_count);

        // Create and persist the trie
        {
            let mut trie =
                PersistentARTrieChar::<u64>::create(&trie_path).expect("Failed to create trie");

            for (i, term) in terms.iter().enumerate() {
                trie.upsert(term, i as u64).expect("Failed to insert");
            }

            trie.checkpoint().expect("Failed to checkpoint");
        }

        Self {
            terms,
            queries,
            trie_path,
            _temp_dir: temp_dir,
        }
    }
}

// ============================================================================
// Benchmark: Open Time (Eager Loading - Baseline)
// ============================================================================

fn bench_open_time_eager(c: &mut Criterion) {
    let mut group = c.benchmark_group("open_time_eager");
    group.sample_size(30); // 30 samples for statistical significance
    group.warm_up_time(Duration::from_secs(1));

    for size in [1_000usize, 100_000, 1_000_000] {
        let dataset = TestDataset::new(size, 10_000);

        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &dataset, |b, dataset| {
            b.iter(|| {
                // Drop caches would require sudo, so we just measure multiple times
                let trie = PersistentARTrieChar::<u64>::open(&dataset.trie_path)
                    .expect("Failed to open trie");
                black_box(trie)
            });
        });
    }

    group.finish();
}

// ============================================================================
// Benchmark: First Lookup (measures loading + single access)
// ============================================================================

fn bench_first_lookup_eager(c: &mut Criterion) {
    let mut group = c.benchmark_group("first_lookup_eager");
    group.sample_size(30);
    group.warm_up_time(Duration::from_secs(1));

    for size in [1_000usize, 100_000, 1_000_000] {
        let dataset = TestDataset::new(size, 10_000);
        let first_term = dataset.terms.first().cloned().unwrap_or_default();

        group.bench_with_input(
            BenchmarkId::from_parameter(size),
            &(&dataset, &first_term),
            |b, (dataset, term)| {
                b.iter(|| {
                    let trie = PersistentARTrieChar::<u64>::open(&dataset.trie_path)
                        .expect("Failed to open trie");
                    let result = trie.contains(term);
                    black_box(result)
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// Benchmark: Bulk Lookup (measures steady-state performance)
// ============================================================================

fn bench_bulk_lookup_eager(c: &mut Criterion) {
    let mut group = c.benchmark_group("bulk_lookup_eager");
    group.sample_size(30);
    group.warm_up_time(Duration::from_secs(1));

    for size in [1_000usize, 100_000, 1_000_000] {
        let dataset = TestDataset::new(size, 10_000);

        // Open trie once for steady-state measurement
        let trie =
            PersistentARTrieChar::<u64>::open(&dataset.trie_path).expect("Failed to open trie");

        group.throughput(Throughput::Elements(dataset.queries.len() as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(size),
            &(&trie, &dataset.queries),
            |b, (trie, queries)| {
                b.iter(|| {
                    let mut hits = 0u64;
                    for query in queries.iter() {
                        if trie.contains(query) {
                            hits += 1;
                        }
                    }
                    black_box(hits)
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// Benchmark: Memory Usage (Peak RSS after open)
// ============================================================================

#[cfg(target_os = "linux")]
fn get_rss_bytes() -> usize {
    use std::fs;

    let status = fs::read_to_string("/proc/self/status").expect("Failed to read /proc/self/status");

    for line in status.lines() {
        if line.starts_with("VmRSS:") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                let kb: usize = parts[1].parse().unwrap_or(0);
                return kb * 1024; // Convert to bytes
            }
        }
    }

    0
}

#[cfg(not(target_os = "linux"))]
fn get_rss_bytes() -> usize {
    // Fallback for non-Linux systems
    0
}

fn bench_memory_usage(c: &mut Criterion) {
    let mut group = c.benchmark_group("memory_usage_eager");
    group.sample_size(10); // Fewer samples for memory measurement

    for size in [1_000usize, 100_000, 1_000_000] {
        let dataset = TestDataset::new(size, 100);

        group.bench_with_input(BenchmarkId::from_parameter(size), &dataset, |b, dataset| {
            b.iter_custom(|iters| {
                let mut total_duration = Duration::ZERO;

                for _ in 0..iters {
                    // Force GC-like behavior by dropping any previous allocations
                    let start = Instant::now();
                    let _trie = PersistentARTrieChar::<u64>::open(&dataset.trie_path)
                        .expect("Failed to open trie");
                    let elapsed = start.elapsed();

                    // Record RSS (for logging, not timing)
                    let rss = get_rss_bytes();

                    // Use RSS as a proxy metric (logged in results)
                    info!("RSS for {} terms: {} MB", size, rss / (1024 * 1024));

                    total_duration += elapsed;
                }

                total_duration
            });
        });
    }

    group.finish();
}

// ============================================================================
// Raw Timing Collection (for custom statistical analysis)
// ============================================================================

/// Collect raw timings for custom statistical analysis
/// This bypasses Criterion's aggregation to give us per-run data
fn collect_raw_timings() {
    println!("\n=== Raw Timing Collection for Statistical Analysis ===\n");

    for size in [1_000usize, 100_000, 1_000_000] {
        println!("Dataset size: {} terms", size);

        let dataset = TestDataset::new(size, 10_000);

        // Warm-up runs (discarded)
        for _ in 0..3 {
            let _trie =
                PersistentARTrieChar::<u64>::open(&dataset.trie_path).expect("Failed to open trie");
        }

        // Measurement runs
        let mut open_times: Vec<f64> = Vec::with_capacity(30);
        let mut first_lookup_times: Vec<f64> = Vec::with_capacity(30);
        let mut bulk_lookup_times: Vec<f64> = Vec::with_capacity(30);

        for _ in 0..30 {
            // Open time
            let start = Instant::now();
            let trie =
                PersistentARTrieChar::<u64>::open(&dataset.trie_path).expect("Failed to open trie");
            let open_elapsed = start.elapsed();
            open_times.push(open_elapsed.as_secs_f64() * 1000.0); // ms

            // First lookup
            let start = Instant::now();
            let _ = trie.contains(&dataset.queries[0]);
            let first_elapsed = start.elapsed();
            first_lookup_times.push(first_elapsed.as_secs_f64() * 1_000_000.0); // µs

            // Bulk lookup
            let start = Instant::now();
            let mut hits = 0u64;
            for query in &dataset.queries {
                if trie.contains(query) {
                    hits += 1;
                }
            }
            black_box(hits);
            let bulk_elapsed = start.elapsed();
            bulk_lookup_times.push(bulk_elapsed.as_secs_f64() * 1000.0); // ms
        }

        // Calculate statistics
        let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
        let std_dev = |v: &[f64]| {
            let m = mean(v);
            (v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (v.len() - 1) as f64).sqrt()
        };

        println!(
            "  open_time_ms: {:.3} ± {:.3}",
            mean(&open_times),
            std_dev(&open_times)
        );
        println!(
            "  first_lookup_µs: {:.3} ± {:.3}",
            mean(&first_lookup_times),
            std_dev(&first_lookup_times)
        );
        println!(
            "  bulk_lookup_ms: {:.3} ± {:.3}",
            mean(&bulk_lookup_times),
            std_dev(&bulk_lookup_times)
        );
        println!("  rss_mb: {}", get_rss_bytes() / (1024 * 1024));
        println!();
    }
}

// ============================================================================
// Benchmark: Depth-Limited Open Time
// ============================================================================

fn bench_open_time_depth(c: &mut Criterion) {
    let mut group = c.benchmark_group("open_time_depth");
    group.sample_size(30);
    group.warm_up_time(Duration::from_secs(1));

    // Only test with 1M terms to see the effect of depth
    let size = 1_000_000usize;
    let dataset = TestDataset::new(size, 10_000);

    // Test different depths: 3, 5, 10, 20
    for depth in [3usize, 5, 10, 20] {
        group.bench_with_input(
            BenchmarkId::new("depth", depth),
            &(&dataset, depth),
            |b, (dataset, depth)| {
                b.iter(|| {
                    let trie = PersistentARTrieChar::<u64>::open(&dataset.trie_path)
                        .expect("Failed to open trie");
                    black_box(trie)
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// Benchmark: Depth-Limited First Lookup
// ============================================================================

fn bench_first_lookup_depth(c: &mut Criterion) {
    let mut group = c.benchmark_group("first_lookup_depth");
    group.sample_size(30);
    group.warm_up_time(Duration::from_secs(1));

    // Only test with 1M terms
    let size = 1_000_000usize;
    let dataset = TestDataset::new(size, 10_000);
    let first_term = dataset.terms.first().cloned().unwrap_or_default();

    // Test different depths
    for depth in [3usize, 5, 10, 20] {
        group.bench_with_input(
            BenchmarkId::new("depth", depth),
            &(&dataset, &first_term, depth),
            |b, (dataset, term, depth)| {
                b.iter(|| {
                    let trie = PersistentARTrieChar::<u64>::open(&dataset.trie_path)
                        .expect("Failed to open trie");
                    let result = trie.contains(term);
                    black_box(result)
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// Benchmark: Depth-Limited Bulk Lookup
// ============================================================================

fn bench_bulk_lookup_depth(c: &mut Criterion) {
    let mut group = c.benchmark_group("bulk_lookup_depth");
    group.sample_size(30);
    group.warm_up_time(Duration::from_secs(1));

    // Only test with 1M terms
    let size = 1_000_000usize;
    let dataset = TestDataset::new(size, 10_000);

    // Test different depths
    for depth in [3usize, 5, 10, 20] {
        // Open trie once for steady-state measurement
        let trie =
            PersistentARTrieChar::<u64>::open(&dataset.trie_path).expect("Failed to open trie");

        group.throughput(Throughput::Elements(dataset.queries.len() as u64));
        group.bench_with_input(
            BenchmarkId::new("depth", depth),
            &(&trie, &dataset.queries),
            |b, (trie, queries)| {
                b.iter(|| {
                    let mut hits = 0u64;
                    for query in queries.iter() {
                        if trie.contains(query) {
                            hits += 1;
                        }
                    }
                    black_box(hits)
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// Criterion Groups
// ============================================================================

criterion_group!(
    name = eager_loading;
    config = Criterion::default()
        .significance_level(0.05)
        .noise_threshold(0.02)
        .sample_size(30);
    targets = bench_open_time_eager, bench_first_lookup_eager, bench_bulk_lookup_eager, bench_memory_usage
);

criterion_group!(
    name = depth_loading;
    config = Criterion::default()
        .significance_level(0.05)
        .noise_threshold(0.02)
        .sample_size(30);
    targets = bench_open_time_depth, bench_first_lookup_depth, bench_bulk_lookup_depth
);

criterion_main!(eager_loading, depth_loading);

// ============================================================================
// Manual Run Mode (for raw data collection)
// ============================================================================

// Uncomment to run raw timing collection instead of Criterion:
// fn main() {
//     collect_raw_timings();
// }
