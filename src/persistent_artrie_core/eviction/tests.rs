//! Integration tests for the eviction module.

use super::*;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::persistent_artrie_core::concurrency::EpochManager;
use crate::persistent_artrie_core::swizzled_ptr::{NodeType, SwizzledPtr};

#[test]
fn test_eviction_config_presets() {
    // Test all presets are valid
    let presets = [
        EvictionConfig::default(),
        EvictionConfig::disabled(),
        EvictionConfig::memory_constrained(),
        EvictionConfig::read_optimized(),
    ];

    for preset in &presets {
        if preset.enabled {
            assert!(preset.validate().is_ok(), "Preset should be valid");
        }
    }
}

#[test]
fn test_lru_coldness_ordering() {
    let registry = LruRegistry::new();

    // Create nodes with different access patterns
    registry.touch(b"cold");
    thread::sleep(Duration::from_micros(100));

    registry.touch(b"medium");
    thread::sleep(Duration::from_micros(100));

    registry.touch(b"hot");
    // Touch hot multiple times
    for _ in 0..5 {
        registry.touch(b"hot");
    }

    // Get coldness scores
    let cold_score = registry.coldness_score(b"cold");
    let medium_score = registry.coldness_score(b"medium");
    let hot_score = registry.coldness_score(b"hot");

    // Cold should have highest score (evicted first)
    // Hot should have lowest score (kept in memory)
    assert!(
        cold_score > medium_score,
        "Cold should be colder than medium"
    );
    assert!(medium_score > hot_score, "Medium should be colder than hot");
}

#[test]
fn test_disk_registry_eviction_selection() {
    let mut registry = DiskLocationRegistry::new();
    let lru = LruRegistry::new();

    // Add nodes at different depths
    for i in 0..10 {
        let path = format!("node_{}", i).into_bytes();
        registry.register(
            path.clone(),
            SwizzledPtr::on_disk(1, i * 100, NodeType::Node16),
            256,
            (i % 3) as usize, // Depths 0, 1, 2, 0, 1, 2, ...
            NodeType::Node16,
        );

        // Touch in LRU with varying access counts
        for _ in 0..i {
            lru.touch(&path);
        }
    }

    // Select with min_depth = 1 (should exclude depth 0 nodes)
    let selected = registry.select_for_eviction(1024, &lru, 1, 5);

    // Should have selected some nodes
    assert!(!selected.is_empty());

    // All selected should be depth >= 1
    for (_, node) in &selected {
        assert!(node.depth >= 1, "Selected node depth should be >= 1");
    }
}

#[test]
fn test_coordinator_with_mock_eviction() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let epoch_manager = Arc::new(EpochManager::new());
    let config = EvictionConfig {
        enabled: true,
        batch_size: 16,
        quiescence_timeout: Duration::from_millis(50),
        cooldown_period: Duration::from_millis(10),
        ..Default::default()
    };
    let coordinator = EvictionCoordinator::new(config, epoch_manager);

    // Set up disk registry with some nodes
    let mut registry = DiskLocationRegistry::new();
    for i in 0..20 {
        let path = format!("path_{}", i).into_bytes();
        registry.register(
            path,
            SwizzledPtr::on_disk(1, i * 100, NodeType::Node16),
            256,
            2, // Depth 2 so it can be evicted
            NodeType::Node16,
        );
    }
    coordinator.update_disk_registry(registry);

    // Track evictions
    let eviction_count = Arc::new(AtomicUsize::new(0));
    let bytes_freed = Arc::new(AtomicUsize::new(0));
    let count_clone = Arc::clone(&eviction_count);
    let bytes_clone = Arc::clone(&bytes_freed);

    // Start coordinator
    coordinator
        .start(move |nodes| {
            let count = nodes.len();
            let bytes = count * 256;
            count_clone.fetch_add(count, Ordering::SeqCst);
            bytes_clone.fetch_add(bytes, Ordering::SeqCst);
            (count, bytes)
        })
        .expect("start coordinator");

    // Request eviction
    coordinator.request_eviction(EvictionUrgency::Moderate);

    // Wait for eviction to process
    thread::sleep(Duration::from_millis(200));

    // Shutdown
    coordinator.shutdown();

    // Check stats
    let stats = coordinator.stats();
    assert!(stats.eviction_requests >= 1);
}

#[test]
fn test_eviction_urgency_levels() {
    // Test that higher urgency results in larger batches
    let base_batch = 100;

    let moderate = EvictionUrgency::Moderate.batch_multiplier();
    let urgent = EvictionUrgency::Urgent.batch_multiplier();
    let emergency = EvictionUrgency::Emergency.batch_multiplier();

    assert_eq!(base_batch * moderate, 100);
    assert_eq!(base_batch * urgent, 200);
    assert_eq!(base_batch * emergency, 400);
}

#[test]
fn test_access_tracker_thread_safety() {
    let tracker = Arc::new(AccessTracker::new());
    let handles: Vec<_> = (0..4)
        .map(|i| {
            let t = Arc::clone(&tracker);
            thread::spawn(move || {
                for j in 0..1000 {
                    t.touch((i * 1000 + j) as u64);
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().expect("thread panicked");
    }

    // Should have tracked many accesses
    assert!(tracker.access_count() >= 4000);
}

#[test]
fn test_registry_invalidation_on_write() {
    let epoch_manager = Arc::new(EpochManager::new());
    let config = EvictionConfig::default();
    let coordinator = EvictionCoordinator::new(config, epoch_manager);

    // Set up valid registry
    let mut registry = DiskLocationRegistry::new();
    registry.register(
        b"test".to_vec(),
        SwizzledPtr::on_disk(1, 100, NodeType::Node16),
        256,
        1,
        NodeType::Node16,
    );
    coordinator.update_disk_registry(registry);

    // Before invalidation, eviction should find candidates
    let (count_before, _) = coordinator.force_eviction(1024);
    assert_eq!(count_before, 1);

    // Invalidate (simulating a write operation)
    coordinator.invalidate_registry();

    // After invalidation, no candidates
    let (count_after, _) = coordinator.force_eviction(1024);
    assert_eq!(count_after, 0);
}

#[test]
fn test_eviction_respects_min_depth() {
    let mut registry = DiskLocationRegistry::new();
    let lru = LruRegistry::new();

    // Add nodes at depth 0 (should not be evicted with min_depth=1)
    for i in 0..5 {
        let path = format!("root_child_{}", i).into_bytes();
        registry.register(
            path.clone(),
            SwizzledPtr::on_disk(1, i * 100, NodeType::Node16),
            256,
            0, // Depth 0
            NodeType::Node16,
        );
        lru.touch(&path);
    }

    // Add nodes at depth 2 (should be evicted)
    for i in 0..5 {
        let path = format!("deep_node_{}", i).into_bytes();
        registry.register(
            path.clone(),
            SwizzledPtr::on_disk(1, (i + 5) * 100, NodeType::Node16),
            256,
            2, // Depth 2
            NodeType::Node16,
        );
        lru.touch(&path);
    }

    // Select with min_depth=1
    let selected = registry.select_for_eviction(10000, &lru, 1, 100);

    // Should only have depth 2 nodes
    assert_eq!(selected.len(), 5);
    for (_, node) in &selected {
        assert!(node.depth >= 1);
    }
}

#[test]
fn test_coordinator_stats_accuracy() {
    let epoch_manager = Arc::new(EpochManager::new());
    let config = EvictionConfig {
        enabled: true,
        quiescence_timeout: Duration::from_millis(50),
        cooldown_period: Duration::from_millis(1),
        ..Default::default()
    };
    let coordinator = EvictionCoordinator::new(config, epoch_manager);

    // Initial stats should be zero
    let stats = coordinator.stats();
    assert_eq!(stats.nodes_evicted, 0);
    assert_eq!(stats.bytes_freed, 0);
    assert_eq!(stats.eviction_cycles, 0);
    assert_eq!(stats.eviction_requests, 0);

    // Start and make requests
    coordinator
        .start(|nodes| (nodes.len(), nodes.len() * 256))
        .unwrap();

    for _ in 0..5 {
        coordinator.request_eviction(EvictionUrgency::Moderate);
    }

    thread::sleep(Duration::from_millis(100));

    let stats = coordinator.stats();
    // Requests may be merged, but at least 1 should be recorded
    assert!(stats.eviction_requests >= 1);

    coordinator.shutdown();
}

#[test]
fn test_eviction_config_with_memory_monitor() {
    // Test config presets include memory monitor settings
    let default = EvictionConfig::default();
    assert!(default.enable_memory_pressure_monitor);
    assert!(default.memory_pressure_config.is_none()); // Uses default

    let disabled = EvictionConfig::disabled();
    assert!(!disabled.enable_memory_pressure_monitor);

    let constrained = EvictionConfig::memory_constrained();
    assert!(constrained.enable_memory_pressure_monitor);
    assert!(constrained.memory_pressure_config.is_some());

    let no_monitor = EvictionConfig::without_memory_monitor();
    assert!(!no_monitor.enable_memory_pressure_monitor);
    assert!(no_monitor.enabled); // Eviction still enabled
}

#[test]
fn test_coordinator_memory_monitor_disabled() {
    let epoch_manager = Arc::new(EpochManager::new());
    let config = EvictionConfig::without_memory_monitor();
    let coordinator = EvictionCoordinator::new(config, epoch_manager);

    // Start coordinator
    coordinator.start(|_| (0, 0)).expect("start coordinator");

    // Start memory monitor should succeed but not actually start
    assert!(coordinator.start_memory_monitor().is_ok());
    assert!(!coordinator.memory_monitor_running());

    coordinator.shutdown();
}

#[test]
fn test_coordinator_memory_pressure_stats_none_when_disabled() {
    let epoch_manager = Arc::new(EpochManager::new());
    let config = EvictionConfig::without_memory_monitor();
    let coordinator = EvictionCoordinator::new(config, epoch_manager);

    // Stats should be None when monitor not running
    assert!(coordinator.memory_pressure_stats().is_none());
}

#[test]
fn test_lru_char_path_tracking() {
    use super::lru_tracker::hash_char_path;

    let registry = LruRegistry::new();

    // Track char paths
    let path1: Vec<char> = "hello".chars().collect();
    let path2: Vec<char> = "world".chars().collect();
    let path3: Vec<char> = "日本語".chars().collect();

    let hash1 = hash_char_path(&path1);
    let hash2 = hash_char_path(&path2);
    let hash3 = hash_char_path(&path3);

    // Different paths should have different hashes
    assert_ne!(hash1, hash2);
    assert_ne!(hash1, hash3);
    assert_ne!(hash2, hash3);

    // Touch via hash
    registry.touch_hash(hash1);
    registry.touch_hash(hash2);
    registry.touch_hash(hash3);

    assert_eq!(registry.len(), 3);
}

#[test]
fn test_char_disk_registry_eviction_selection() {
    let mut registry = DiskLocationRegistry::new();
    let lru = LruRegistry::new();

    // Add char nodes at different depths
    for i in 0..10 {
        let path: Vec<char> = format!("node_{}", i).chars().collect();
        registry.register_char(
            path.clone(),
            SwizzledPtr::on_disk(1, i * 100, NodeType::Node16),
            256,
            (i % 3) as usize, // Depths 0, 1, 2, 0, 1, 2, ...
            NodeType::Node16,
        );

        // Touch in LRU with varying access counts
        use super::lru_tracker::hash_char_path;
        for _ in 0..i {
            lru.touch_hash(hash_char_path(&path));
        }
    }

    // Select with min_depth = 1 (should exclude depth 0 nodes)
    let selected = registry.select_char_for_eviction(1024, &lru, 1, 5);

    // Should have selected some nodes
    assert!(!selected.is_empty());

    // All selected should be depth >= 1
    for (_, node) in &selected {
        assert!(node.depth >= 1, "Selected char node depth should be >= 1");
    }
}

#[test]
fn test_coordinator_char_eviction_loop() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let epoch_manager = Arc::new(EpochManager::new());
    let config = EvictionConfig {
        enabled: true,
        batch_size: 16,
        quiescence_timeout: Duration::from_millis(50),
        cooldown_period: Duration::from_millis(10),
        enable_memory_pressure_monitor: false, // Don't start monitor for this test
        ..Default::default()
    };
    let coordinator = EvictionCoordinator::new(config, epoch_manager);

    // Set up disk registry with char nodes
    let mut registry = DiskLocationRegistry::new();
    for i in 0..20 {
        let path: Vec<char> = format!("path_{}", i).chars().collect();
        registry.register_char(
            path,
            SwizzledPtr::on_disk(1, i * 100, NodeType::Node16),
            256,
            2, // Depth 2 so it can be evicted
            NodeType::Node16,
        );
    }
    coordinator.update_disk_registry(registry);

    // Track evictions
    let eviction_count = Arc::new(AtomicUsize::new(0));
    let count_clone = Arc::clone(&eviction_count);

    // Start coordinator with char callback
    coordinator
        .start_char(move |nodes| {
            let count = nodes.len();
            count_clone.fetch_add(count, Ordering::SeqCst);
            (count, count * 256)
        })
        .expect("start coordinator");

    // Request eviction
    coordinator.request_eviction(EvictionUrgency::Urgent);

    // Wait for eviction to process
    thread::sleep(Duration::from_millis(200));

    // Shutdown
    coordinator.shutdown();

    // Check stats
    let stats = coordinator.stats();
    assert!(stats.eviction_requests >= 1);
}

#[test]
fn test_eviction_stats_calculations() {
    use super::config::EvictionStats;

    let stats = EvictionStats {
        nodes_evicted: 1000,
        bytes_freed: 1024 * 1024, // 1 MB
        eviction_cycles: 10,
        eviction_requests: 15,
        skipped_evictions: 3,
        ..Default::default()
    };

    assert_eq!(stats.nodes_per_cycle(), 100.0);
    // 1024 * 1024 / 10 = 104857.6
    assert!((stats.bytes_per_cycle() - 104857.6).abs() < 1.0);
    assert!((stats.skip_rate() - 0.2).abs() < 0.001);
}
