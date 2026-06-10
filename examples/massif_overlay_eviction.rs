//! Phase-8 heap-bound witness + `STRUCT_OVERHEAD` calibration for the production
//! overlay-eviction resident budget (`resident_budget_bytes`).
//!
//! Builds an eviction-enabled char counter trie, inserts N n-gram-like keys with
//! periodic checkpoints (so the budget tail fires), for a BUDGETED run and a
//! NO-BUDGET control, and prints — for each — the resident node estimate, the
//! evictable-node count, and the process RSS. The BUDGETED run's RSS must be
//! materially below the control's (the libgrammstein "<16 GB" goal, down-scaled).
//!
//! Run (fast, allocator RSS via /proc/self/statm):
//!   cargo run --release --example massif_overlay_eviction --features persistent-artrie -- 200000 4000000
//!
//! Precise heap attribution + the inter-checkpoint TRANSIENT peak (the margin must
//! cover it — Phase-8 C.1) via valgrind massif (slower; --pages-as-heap = true RSS):
//!   valgrind --tool=massif --pages-as-heap=yes \
//!     target/release/examples/massif_overlay_eviction 200000 4000000
//!   ms_print massif.out.* | less
//!
//! CALIBRATION: STRUCT_OVERHEAD_{BYTE,CHAR} (disk_registry.rs) ≈
//!   (measured resident RSS attributable to the overlay − Σ on-disk size_bytes) / node_count.
//! Use the NO-BUDGET control (full resident set) so node_count == evictable_node_count
//! and Σ size_bytes == the registry's on-disk total.
//!
//! Scratch is REAL DISK under `target/` (NEVER `/tmp` — tmpfs would not exercise the
//! disk-backed image the eviction faults from).

#[cfg(feature = "persistent-artrie")]
fn rss_bytes() -> usize {
    // /proc/self/statm field 2 = resident pages.
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|s| s.split_whitespace().nth(1).map(str::to_owned))
        .and_then(|p| p.parse::<usize>().ok())
        .map(|pages| pages * 4096)
        .unwrap_or(0)
}

#[cfg(feature = "persistent-artrie")]
fn run(n: usize, budget: Option<usize>) -> (usize, usize, usize) {
    use libdictenstein::artrie_trait::EvictableARTrie;
    use libdictenstein::persistent_artrie::eviction::EvictionConfig;
    use libdictenstein::persistent_artrie::WalConfig;
    use libdictenstein::persistent_artrie_char::PersistentARTrieChar;
    use libdictenstein::persistent_artrie_core::durability::DurabilityPolicy;

    // Clean the whole scratch dir (the trie has a WAL sidecar + descriptor, not just
    // the .artc — a stale WAL would fail `create` with Wal(AlreadyExists)).
    let scratch = "target/massif-scratch";
    let _ = std::fs::remove_dir_all(scratch);
    std::fs::create_dir_all(scratch).expect("scratch dir");
    let path = format!(
        "{scratch}/massif-{}.artc",
        if budget.is_some() {
            "budgeted"
        } else {
            "control"
        }
    );

    let mut trie: PersistentARTrieChar<u64> =
        PersistentARTrieChar::create_with_config(&path, WalConfig::no_archive()).expect("create");
    trie.set_durability_policy(DurabilityPolicy::Immediate);
    // PRODUCTION API (not the bench shim): `enable_eviction` installs the coordinator,
    // and `checkpoint()` route-splits to `publish_overlay_snapshot_retaining_with_eviction`
    // (the budget tail) under `route_overlay()` + a coordinator — the exact path that
    // fixes the libgrammstein OOM.
    let trie = std::sync::Arc::new(trie);
    let config = EvictionConfig {
        resident_budget_bytes: budget,
        ..EvictionConfig::without_memory_monitor()
    };
    trie.enable_eviction(config).expect("enable eviction");

    // Insert N keys; checkpoint every 10k so the production budget tail fires repeatedly.
    for i in 0..n {
        let term = format!("ngram-{i:08}");
        trie.try_increment_cas_durable(&term, 1).expect("increment");
        if i % 10_000 == 9_999 {
            trie.checkpoint().expect("checkpoint");
        }
    }
    trie.checkpoint().expect("final checkpoint");

    let nodes = trie.evictable_node_count().unwrap_or(0);
    let rss = rss_bytes();
    trie.disable_eviction().expect("disable eviction");
    drop(trie);
    let _ = std::fs::remove_file(&path);
    (nodes, rss, n)
}

#[cfg(feature = "persistent-artrie")]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let n: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(50_000);
    let budget: usize = args
        .get(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(2_000_000);

    let base_rss = rss_bytes();
    println!("baseline RSS (pre-build): {} KiB", base_rss / 1024);

    let (b_nodes, b_rss, _) = run(n, Some(budget));
    println!(
        "BUDGETED   N={n} budget={budget}B : evictable_nodes={b_nodes}  RSS={} KiB",
        b_rss / 1024
    );

    let (c_nodes, c_rss, _) = run(n, None);
    println!(
        "CONTROL    N={n} (no budget)      : evictable_nodes={c_nodes}  RSS={} KiB",
        c_rss / 1024
    );

    println!(
        "BOUND WITNESS: budgeted RSS {} KiB  vs  control RSS {} KiB  (budgeted should be lower)",
        b_rss / 1024,
        c_rss / 1024
    );
    println!(
        "CALIBRATION (control, full resident): node_count={c_nodes} — run under valgrind massif for \
         the resident-bytes/node to refine STRUCT_OVERHEAD_CHAR (disk_registry.rs)."
    );
}

#[cfg(not(feature = "persistent-artrie"))]
fn main() {
    eprintln!("massif_overlay_eviction requires --features persistent-artrie");
}
