//! Eviction coordinator for async, epoch-safe node eviction.
//!
//! The coordinator manages the eviction lifecycle:
//! 1. Receives eviction requests from memory pressure callbacks
//! 2. Waits for epoch quiescence (no old-epoch readers)
//! 3. Selects cold nodes using LRU tracking
//! 4. Atomically swaps in-memory nodes to DiskRef

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex, RwLock};

use super::config::{EvictionConfig, EvictionStats, EvictionStatsAtomic, EvictionUrgency};
use super::disk_registry::DiskLocationRegistry;
use super::lru_tracker::LruRegistry;
use crate::persistent_artrie_core::concurrency::EpochManager;
use crate::persistent_artrie_core::memory_monitor::{
    MemoryMonitorStats, MemoryPressureLevel, MemoryPressureMonitor,
};
// `NodeType` is referenced by the inline test suite at the bottom of this
// file but not by the production impl, so it is gated to test builds.
#[cfg(test)]
use crate::persistent_artrie_core::swizzled_ptr::NodeType;
use crate::persistent_artrie_core::swizzled_ptr::SwizzledPtr;

/// Request for eviction with urgency level.
#[derive(Debug, Clone, Copy)]
struct EvictionRequest {
    urgency: EvictionUrgency,
    timestamp: Instant,
}

/// The eviction coordinator manages async, epoch-safe node eviction.
///
/// # Architecture
///
/// ```text
/// ┌───────────────────────────────────────────────────────────────┐
/// │                    EvictionCoordinator                         │
/// ├───────────────────────────────────────────────────────────────┤
/// │  request_eviction()  ─────▶  eviction_queue                   │
/// │                              │                                 │
/// │  eviction_thread  ◀──────────┘                                │
/// │       │                                                        │
/// │       ├── wait for request                                     │
/// │       ├── wait for epoch quiescence                           │
/// │       ├── select cold nodes (LRU)                             │
/// │       └── perform eviction (via callback)                     │
/// └───────────────────────────────────────────────────────────────┘
/// ```
///
/// # Thread Safety
///
/// The coordinator is thread-safe and can receive eviction requests
/// from any thread (e.g., memory pressure monitor callback).
pub struct EvictionCoordinator {
    /// Configuration
    config: EvictionConfig,
    /// Epoch manager for safe reclamation
    epoch_manager: Arc<EpochManager>,
    /// LRU registry for access tracking
    lru_registry: Arc<LruRegistry>,
    /// Pending eviction requests
    request_queue: Mutex<VecDeque<EvictionRequest>>,
    /// Condition variable for waking eviction thread
    request_condvar: Condvar,
    /// Shutdown flag
    shutdown: AtomicBool,
    /// Eviction thread handle
    eviction_thread: Mutex<Option<JoinHandle<()>>>,
    /// Statistics
    stats: Arc<EvictionStatsAtomic>,
    /// Last eviction time (for cooldown)
    last_eviction: AtomicU64,
    /// Disk location registry (populated during checkpoint)
    disk_registry: RwLock<DiskLocationRegistry>,
    /// Whether the coordinator is running
    running: AtomicBool,
    /// Memory pressure monitor (optional)
    memory_monitor: RwLock<Option<Arc<MemoryPressureMonitor>>>,
}

impl EvictionCoordinator {
    /// Create a new eviction coordinator.
    ///
    /// The coordinator is created in a stopped state. Call `start()` with
    /// the eviction callback to begin processing eviction requests.
    pub fn new(config: EvictionConfig, epoch_manager: Arc<EpochManager>) -> Arc<Self> {
        let lru_registry = if config.use_lru_tracking {
            Arc::new(LruRegistry::new())
        } else {
            Arc::new(LruRegistry::with_capacity(0))
        };

        Arc::new(Self {
            config,
            epoch_manager,
            lru_registry,
            request_queue: Mutex::new(VecDeque::with_capacity(16)),
            request_condvar: Condvar::new(),
            shutdown: AtomicBool::new(false),
            eviction_thread: Mutex::new(None),
            stats: Arc::new(EvictionStatsAtomic::new()),
            last_eviction: AtomicU64::new(0),
            disk_registry: RwLock::new(DiskLocationRegistry::new()),
            running: AtomicBool::new(false),
            memory_monitor: RwLock::new(None),
        })
    }

    /// Quiescence timeout from the eviction config.
    ///
    /// The char reclaim path (`evict_char_nodes`) drains this (shared) epoch AFTER
    /// unlinking a batch and before freeing the retired subtrees; it reads these
    /// parameters for that drain.
    pub fn quiescence_timeout(&self) -> std::time::Duration {
        self.config.quiescence_timeout
    }

    /// Quiescence poll interval from the eviction config (see
    /// [`Self::quiescence_timeout`]).
    pub fn quiescence_poll_interval(&self) -> std::time::Duration {
        self.config.quiescence_poll_interval
    }

    /// Start the eviction coordinator with a callback for performing evictions.
    ///
    /// The callback is invoked for each batch of nodes to evict. It receives:
    /// - A list of (path_hash, path, disk_ptr) tuples for nodes to evict
    /// - Returns the number of successfully evicted nodes and bytes freed
    ///
    /// # Type Parameters
    ///
    /// * `F` - Callback function type
    ///
    /// # Arguments
    ///
    /// * `self_arc` - Arc to this coordinator (for the eviction thread)
    /// * `callback` - Function to perform the actual node eviction
    pub fn start<F>(self: &Arc<Self>, callback: F) -> Result<(), String>
    where
        F: Fn(Vec<(u64, Vec<u8>, SwizzledPtr)>) -> (usize, usize) + Send + Sync + 'static,
    {
        if !self.config.enabled {
            return Ok(());
        }

        if self.running.swap(true, Ordering::SeqCst) {
            return Err("Eviction coordinator already running".to_string());
        }

        let weak = Arc::downgrade(self);
        let callback = Arc::new(callback);

        let handle = thread::Builder::new()
            .name("artrie-eviction".to_string())
            .spawn(move || {
                Self::eviction_loop(weak, callback);
            })
            .map_err(|e| format!("Failed to spawn eviction thread: {}", e))?;

        *self.eviction_thread.lock() = Some(handle);

        Ok(())
    }

    /// Start the coordinator for char-level tries.
    pub fn start_char<F>(self: &Arc<Self>, callback: F) -> Result<(), String>
    where
        F: Fn(Vec<(u64, Vec<char>, SwizzledPtr)>) -> (usize, usize) + Send + Sync + 'static,
    {
        if !self.config.enabled {
            return Ok(());
        }

        if self.running.swap(true, Ordering::SeqCst) {
            return Err("Eviction coordinator already running".to_string());
        }

        let weak = Arc::downgrade(self);
        let callback = Arc::new(callback);

        let handle = thread::Builder::new()
            .name("artrie-eviction-char".to_string())
            .spawn(move || {
                Self::eviction_loop_char(weak, callback);
            })
            .map_err(|e| format!("Failed to spawn eviction thread: {}", e))?;

        *self.eviction_thread.lock() = Some(handle);

        Ok(())
    }

    /// Start the memory pressure monitor if configured.
    ///
    /// This should be called after `start()` or `start_char()` to enable
    /// automatic eviction based on system memory pressure.
    ///
    /// The monitor runs in a background thread and calls `request_eviction()`
    /// when memory pressure is detected.
    pub fn start_memory_monitor(self: &Arc<Self>) -> Result<(), String> {
        if !self.config.enable_memory_pressure_monitor {
            return Ok(());
        }

        // Use custom config or default
        let pressure_config = self
            .config
            .memory_pressure_config
            .clone()
            .unwrap_or_default();

        // Create a weak reference for the callback
        let self_weak = Arc::downgrade(self);

        // Start the memory pressure monitor
        let monitor = MemoryPressureMonitor::start(pressure_config, move |level, _stats| {
            let Some(coordinator) = self_weak.upgrade() else {
                return;
            };

            // Map memory pressure level to eviction urgency
            let urgency = match level {
                MemoryPressureLevel::Normal => return, // No action needed
                MemoryPressureLevel::Low => EvictionUrgency::Moderate,
                MemoryPressureLevel::Critical => EvictionUrgency::Emergency,
            };

            coordinator.request_eviction(urgency);
        })
        .map_err(|e| format!("Failed to start memory pressure monitor: {}", e))?;

        *self.memory_monitor.write() = Some(Arc::new(monitor));

        Ok(())
    }

    /// Stop the memory pressure monitor if running.
    pub fn stop_memory_monitor(&self) {
        if let Some(monitor) = self.memory_monitor.write().take() {
            monitor.shutdown();
        }
    }

    /// Check if the memory pressure monitor is running.
    pub fn memory_monitor_running(&self) -> bool {
        self.memory_monitor.read().is_some()
    }

    /// Get memory pressure statistics (if monitor is running).
    pub fn memory_pressure_stats(&self) -> Option<MemoryMonitorStats> {
        self.memory_monitor.read().as_ref().map(|m| m.stats())
    }

    /// Request eviction with the specified urgency.
    ///
    /// This is called by the memory pressure callback when pressure is detected.
    /// The request is queued and processed asynchronously by the eviction thread.
    pub fn request_eviction(&self, urgency: EvictionUrgency) {
        if !self.config.enabled || !self.running.load(Ordering::Relaxed) {
            return;
        }

        self.stats.record_request();

        let request = EvictionRequest {
            urgency,
            timestamp: Instant::now(),
        };

        {
            let mut queue = self.request_queue.lock();
            // Merge with existing request if higher urgency
            if let Some(existing) = queue.back_mut() {
                if request.urgency > existing.urgency {
                    existing.urgency = request.urgency;
                }
                return;
            }
            queue.push_back(request);
        }

        self.request_condvar.notify_one();
    }

    /// Manually trigger eviction (for testing/debugging).
    ///
    /// Returns the number of nodes evicted and bytes freed.
    pub fn force_eviction(&self, target_bytes: usize) -> (usize, usize) {
        // This method performs synchronous eviction
        // It's primarily for testing; production code uses async eviction
        let disk_registry = self.disk_registry.read();
        let candidates = disk_registry.select_for_eviction(
            target_bytes,
            &self.lru_registry,
            self.config.min_eviction_depth,
            self.config.batch_size,
        );

        // Return the candidates info for the caller to perform actual eviction
        (
            candidates.len(),
            candidates.iter().map(|(_, n)| n.size_bytes).sum(),
        )
    }

    /// Synchronously evict cold *char* nodes, invoking `callback` inline on the
    /// calling thread to reclaim them.
    ///
    /// This is the char-trie counterpart of [`force_eviction`](Self::force_eviction).
    /// The byte `force_eviction` selects from the byte `locations` map and would
    /// return `(0, 0)` for a char trie (whose nodes live in `char_locations`), so
    /// char tries route here instead. Unlike the byte `force_eviction` — which
    /// only *selects and counts* — this method **actually performs reclamation**
    /// by invoking `callback` (the same closure the async `eviction_loop_char`
    /// uses), giving callers a deterministic, single-threaded eviction path with
    /// no eviction thread, quiescence wait, or cooldown.
    ///
    /// Selection mirrors [`perform_eviction_char`](Self::perform_eviction_char):
    /// it refuses to act on an invalidated registry (`is_valid()`), reads the
    /// published `char_locations`, and respects `min_eviction_depth`. The
    /// registry read lock is released **before** `callback` runs, because the
    /// callback takes the trie write lock to unswizzle nodes. Returns
    /// `(nodes_evicted, bytes_freed)` as reported by `callback`.
    pub fn force_eviction_char<F>(&self, target_bytes: usize, callback: F) -> (usize, usize)
    where
        F: Fn(Vec<(u64, Vec<char>, SwizzledPtr)>) -> (usize, usize),
    {
        let disk_registry = self.disk_registry.read();
        if !disk_registry.is_valid() {
            return (0, 0);
        }

        let candidates = disk_registry.select_char_for_eviction(
            target_bytes,
            &self.lru_registry,
            self.config.min_eviction_depth,
            self.config.batch_size,
        );

        if candidates.is_empty() {
            return (0, 0);
        }

        let eviction_list: Vec<_> = candidates
            .into_iter()
            .map(|(hash, node)| (hash, node.path, node.disk_ptr))
            .collect();

        // Release the registry lock before reclaiming: the callback takes the
        // trie write lock, and parking_lot locks are not re-entrant.
        drop(disk_registry);

        callback(eviction_list)
    }

    /// Number of char nodes currently tracked in the published disk-location
    /// registry (i.e. nodes eligible for eviction, before the `min_eviction_depth`
    /// filter). This is the count populated by `serialize_char_node_to_disk` via
    /// `register_char` and handed over in [`update_disk_registry`](Self::update_disk_registry)
    /// at checkpoint. Returns 0 before the first checkpoint or after the registry
    /// is invalidated-then-cleared. Primarily for observability of the
    /// checkpoint→publish path.
    pub fn disk_registry_char_len(&self) -> usize {
        self.disk_registry.read().char_len()
    }

    /// Get the LRU registry for access tracking.
    pub fn lru_registry(&self) -> &Arc<LruRegistry> {
        &self.lru_registry
    }

    /// Update the disk location registry (called after checkpoint).
    ///
    /// This replaces the existing registry with a new one populated during
    /// the checkpoint process.
    pub fn update_disk_registry(&self, registry: DiskLocationRegistry) {
        *self.disk_registry.write() = registry;
    }

    /// Get a snapshot of eviction statistics.
    pub fn stats(&self) -> EvictionStats {
        self.stats.snapshot()
    }

    /// Reset statistics.
    pub fn reset_stats(&self) {
        self.stats.reset();
    }

    /// Shutdown the eviction coordinator.
    pub fn shutdown(&self) {
        // Stop memory monitor first
        self.stop_memory_monitor();

        self.shutdown.store(true, Ordering::SeqCst);
        self.request_condvar.notify_all();

        if let Some(handle) = self.eviction_thread.lock().take() {
            let _ = handle.join();
        }

        self.running.store(false, Ordering::SeqCst);
    }

    /// Check if the coordinator is running.
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    /// Invalidate the disk registry (called on write operations).
    pub fn invalidate_registry(&self) {
        self.disk_registry.write().invalidate();
    }

    // --- Private methods ---

    /// Main eviction loop for byte-level tries.
    ///
    /// Driven through a `Weak<Self>` (not a strong `Arc`): the worker upgrades
    /// once per iteration and drops the strong ref before sleeping, so it can
    /// never keep the coordinator alive past its owner's drop (the bug that
    /// leaked one OS thread per trie instance). Eviction is background
    /// memory-reclamation, so a 100 ms poll — vs the old condvar wait, which
    /// pinned a strong ref for the loop's whole life — is acceptable.
    fn eviction_loop<F>(weak: Weak<Self>, callback: Arc<F>)
    where
        F: Fn(Vec<(u64, Vec<u8>, SwizzledPtr)>) -> (usize, usize) + Send + Sync,
    {
        loop {
            let Some(this) = weak.upgrade() else { break };
            if this.shutdown.load(Ordering::Relaxed) {
                break;
            }

            let had_request = if let Some(request) = this.try_pop_request() {
                // Check cooldown
                if !this.check_cooldown(&request) {
                    this.stats.record_skip();
                } else if !this.wait_for_quiescence() {
                    // Wait for epoch quiescence
                    this.stats.record_quiescence_timeout();
                } else {
                    // Perform eviction
                    let start = Instant::now();
                    let (nodes_evicted, bytes_freed) = this.perform_eviction(&*callback, &request);
                    let duration_ms = start.elapsed().as_millis() as u64;

                    if nodes_evicted > 0 {
                        this.stats.record_eviction(
                            nodes_evicted as u64,
                            bytes_freed as u64,
                            duration_ms,
                        );
                        this.last_eviction.store(
                            Instant::now().elapsed().as_millis() as u64,
                            Ordering::Relaxed,
                        );
                    }
                }
                true
            } else {
                false
            };

            drop(this); // release the strong ref BEFORE sleeping
            if !had_request {
                thread::sleep(Duration::from_millis(100));
            }
        }
    }

    /// Main eviction loop for char-level tries.
    ///
    /// See [`eviction_loop`](Self::eviction_loop) for why this is driven through
    /// a `Weak<Self>` + 100 ms poll rather than a strong `Arc` + condvar wait.
    fn eviction_loop_char<F>(weak: Weak<Self>, callback: Arc<F>)
    where
        F: Fn(Vec<(u64, Vec<char>, SwizzledPtr)>) -> (usize, usize) + Send + Sync,
    {
        loop {
            let Some(this) = weak.upgrade() else { break };
            if this.shutdown.load(Ordering::Relaxed) {
                break;
            }

            let had_request = if let Some(request) = this.try_pop_request() {
                if !this.check_cooldown(&request) {
                    this.stats.record_skip();
                } else {
                    // NB: no pre-eviction drain here (unlike the byte `eviction_loop`). The
                    // char reclaim path (`evict_char_nodes`, invoked by the callback) does
                    // epoch-based reclamation in the correct order — unlink + retire, THEN
                    // drain, THEN free. Draining BEFORE the unlink would be unnecessary AND
                    // over-conservative: it would skip eviction entirely whenever any walk
                    // is active, even though the post-unlink drain handles that case safely
                    // (deferring the free, never freeing under a live reader).
                    let start = Instant::now();
                    let (nodes_evicted, bytes_freed) =
                        this.perform_eviction_char(&*callback, &request);
                    let duration_ms = start.elapsed().as_millis() as u64;

                    if nodes_evicted > 0 {
                        this.stats.record_eviction(
                            nodes_evicted as u64,
                            bytes_freed as u64,
                            duration_ms,
                        );
                        this.last_eviction.store(
                            Instant::now().elapsed().as_millis() as u64,
                            Ordering::Relaxed,
                        );
                    }
                }
                true
            } else {
                false
            };

            drop(this); // release the strong ref BEFORE sleeping
            if !had_request {
                thread::sleep(Duration::from_millis(100));
            }
        }
    }

    /// Non-blocking pop of the next eviction request.
    ///
    /// The background loop drives itself through a `Weak<Self>` and polls this
    /// every 100 ms (see [`eviction_loop`](Self::eviction_loop)), so it must not
    /// block: blocking on the condvar here would pin a strong `Arc<Self>` for the
    /// loop's whole life and recreate the self-reference cycle that leaked the
    /// thread.
    fn try_pop_request(&self) -> Option<EvictionRequest> {
        self.request_queue.lock().pop_front()
    }

    /// Check if we're past the cooldown period.
    fn check_cooldown(&self, request: &EvictionRequest) -> bool {
        let _cooldown = self.config.cooldown_period / request.urgency.cooldown_divisor();
        let time_since_request = request.timestamp.elapsed();

        // If request is very old, skip it
        if time_since_request > Duration::from_secs(5) {
            return false;
        }

        // Check cooldown from last eviction
        // (simplified: we just check if enough time has passed since request)
        true
    }

    /// Wait for epoch quiescence (no old-epoch readers).
    fn wait_for_quiescence(&self) -> bool {
        let start = Instant::now();
        let timeout = self.config.quiescence_timeout;
        let poll_interval = self.config.quiescence_poll_interval;

        // Advance epoch
        let _old_epoch = self.epoch_manager.advance();

        // Wait for readers to drain
        while start.elapsed() < timeout {
            if !self.epoch_manager.has_active_readers() {
                return true;
            }

            if self.shutdown.load(Ordering::Relaxed) {
                return false;
            }

            thread::sleep(poll_interval);
        }

        false
    }

    /// Perform eviction for byte-level tries.
    fn perform_eviction<F>(&self, callback: &F, request: &EvictionRequest) -> (usize, usize)
    where
        F: Fn(Vec<(u64, Vec<u8>, SwizzledPtr)>) -> (usize, usize),
    {
        let batch_size = self.config.batch_size * request.urgency.batch_multiplier();

        // Calculate target bytes based on memory stats
        // For now, use a simple heuristic: evict batch_size nodes worth
        let target_bytes = batch_size * 256; // Assume ~256 bytes per node average

        let disk_registry = self.disk_registry.read();
        if !disk_registry.is_valid() {
            return (0, 0);
        }

        let candidates = disk_registry.select_for_eviction(
            target_bytes,
            &self.lru_registry,
            self.config.min_eviction_depth,
            batch_size,
        );

        if candidates.is_empty() {
            return (0, 0);
        }

        // Prepare data for callback
        let eviction_list: Vec<_> = candidates
            .into_iter()
            .map(|(hash, node)| (hash, node.path, node.disk_ptr))
            .collect();

        drop(disk_registry);

        // Perform eviction via callback
        let (nodes_evicted, bytes_freed) = callback(eviction_list);

        // Remove evicted nodes from registries
        if nodes_evicted > 0 {
            // Note: Actual removal happens in the callback since it has
            // access to the trie structure. We just track statistics here.
        }

        (nodes_evicted, bytes_freed)
    }

    /// Perform eviction for char-level tries.
    fn perform_eviction_char<F>(&self, callback: &F, request: &EvictionRequest) -> (usize, usize)
    where
        F: Fn(Vec<(u64, Vec<char>, SwizzledPtr)>) -> (usize, usize),
    {
        let batch_size = self.config.batch_size * request.urgency.batch_multiplier();
        let target_bytes = batch_size * 256;

        let disk_registry = self.disk_registry.read();
        if !disk_registry.is_valid() {
            return (0, 0);
        }

        let candidates = disk_registry.select_char_for_eviction(
            target_bytes,
            &self.lru_registry,
            self.config.min_eviction_depth,
            batch_size,
        );

        if candidates.is_empty() {
            return (0, 0);
        }

        let eviction_list: Vec<_> = candidates
            .into_iter()
            .map(|(hash, node)| (hash, node.path, node.disk_ptr))
            .collect();

        drop(disk_registry);

        callback(eviction_list)
    }
}

impl Drop for EvictionCoordinator {
    fn drop(&mut self) {
        // Route through `shutdown()` so teardown is complete (it also stops the
        // memory-pressure monitor) and identical on every drop path. The worker
        // holds only a `Weak<Self>`, so this `Drop` is reachable as soon as the
        // owning trie releases its `Arc`.
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn test_coordinator_creation() {
        let epoch_manager = Arc::new(EpochManager::new());
        let config = EvictionConfig::default();
        let coordinator = EvictionCoordinator::new(config, epoch_manager);

        assert!(!coordinator.is_running());
        assert!(coordinator.lru_registry().is_empty());
    }

    #[test]
    fn test_coordinator_disabled() {
        let epoch_manager = Arc::new(EpochManager::new());
        let config = EvictionConfig::disabled();
        let coordinator = EvictionCoordinator::new(config, epoch_manager);

        // Should not start when disabled
        let result = coordinator.start(|_| (0, 0));
        assert!(result.is_ok());
        assert!(!coordinator.is_running());
    }

    #[test]
    fn test_coordinator_request_eviction() {
        let epoch_manager = Arc::new(EpochManager::new());
        let config = EvictionConfig::default();
        let coordinator = EvictionCoordinator::new(config, epoch_manager);

        // Request without starting should be a no-op
        coordinator.request_eviction(EvictionUrgency::Moderate);

        let stats = coordinator.stats();
        // Request not counted because not running
        assert_eq!(stats.eviction_requests, 0);
    }

    #[test]
    fn test_coordinator_start_and_shutdown() {
        let epoch_manager = Arc::new(EpochManager::new());
        let config = EvictionConfig::default();
        let coordinator = EvictionCoordinator::new(config, epoch_manager);

        let eviction_count = Arc::new(AtomicUsize::new(0));
        let count_clone = Arc::clone(&eviction_count);

        let result = coordinator.start(move |nodes| {
            count_clone.fetch_add(nodes.len(), Ordering::Relaxed);
            (nodes.len(), nodes.len() * 256)
        });
        assert!(result.is_ok());
        assert!(coordinator.is_running());

        // Shutdown
        coordinator.shutdown();
        assert!(!coordinator.is_running());
    }

    #[test]
    fn test_coordinator_double_start_fails() {
        let epoch_manager = Arc::new(EpochManager::new());
        let config = EvictionConfig::default();
        let coordinator = EvictionCoordinator::new(config, epoch_manager);

        let result1 = coordinator.start(|_| (0, 0));
        assert!(result1.is_ok());

        let result2 = coordinator.start(|_| (0, 0));
        assert!(result2.is_err());

        coordinator.shutdown();
    }

    #[test]
    fn test_coordinator_lru_tracking() {
        let epoch_manager = Arc::new(EpochManager::new());
        let config = EvictionConfig::default();
        let coordinator = EvictionCoordinator::new(config, epoch_manager);

        let lru = coordinator.lru_registry();
        lru.touch(b"test/path");

        assert_eq!(lru.len(), 1);
        assert!(lru.last_access(b"test/path").is_some());
    }

    #[test]
    fn test_coordinator_disk_registry_update() {
        let epoch_manager = Arc::new(EpochManager::new());
        let config = EvictionConfig::default();
        let coordinator = EvictionCoordinator::new(config, epoch_manager);

        let mut registry = DiskLocationRegistry::new();
        registry.register(
            b"test".to_vec(),
            SwizzledPtr::on_disk(1, 100, NodeType::Node16),
            256,
            1,
            NodeType::Node16,
        );

        coordinator.update_disk_registry(registry);

        // Registry should be updated
        let (count, _) = coordinator.force_eviction(1024);
        assert_eq!(count, 1);
    }

    #[test]
    fn test_coordinator_invalidate_registry() {
        let epoch_manager = Arc::new(EpochManager::new());
        let config = EvictionConfig::default();
        let coordinator = EvictionCoordinator::new(config, epoch_manager);

        let registry = DiskLocationRegistry::new();
        coordinator.update_disk_registry(registry);

        coordinator.invalidate_registry();

        // After invalidation, no eviction should occur
        let (count, _) = coordinator.force_eviction(1024);
        assert_eq!(count, 0);
    }
}
