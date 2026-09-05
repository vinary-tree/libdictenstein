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
use std::thread::{self, JoinHandle, ThreadId};
use std::time::{Duration, Instant};

use arc_swap::ArcSwapOption;
use parking_lot::{Mutex, MutexGuard, RwLock};

use super::config::{EvictionConfig, EvictionStats, EvictionStatsAtomic, EvictionUrgency};
use super::disk_registry::{
    CompactEvictionBatch, DiskLocationRegistry, PreparedPackedResidency, PublishedRegistryCatalog,
    RegistryBuildError, RegistryFamily, RegistryStructuralCapture, RegistryStructuralSource,
    STRUCT_OVERHEAD_BYTE, STRUCT_OVERHEAD_CHAR,
};
use super::lru_tracker::LruRegistry;
use super::publication_gate::RegistryPublicationGate;
use crate::persistent_artrie::core::concurrency::EpochManager;
use crate::persistent_artrie::core::key_encoding::{ByteKey, CharKey, KeyEncoding};
use crate::persistent_artrie::core::memory_monitor::{
    MemoryMonitorStats, MemoryPressureLevel, MemoryPressureMonitor,
};
use crate::persistent_artrie::core::overlay::{
    AtomicNodePtr, DeferredDurableStamp, PreparedBoundRootTransition, PreparedRootBinding,
    PreparedRootDetachment, RootRevision,
};
// `NodeType` is referenced by the inline test suite at the bottom of this
// file but not by the production impl, so it is gated to test builds.
#[cfg(test)]
use crate::persistent_artrie::core::swizzled_ptr::NodeType;
use crate::persistent_artrie::core::swizzled_ptr::SwizzledPtr;
use crate::value::DictionaryValue;

/// Result of attempting to publish a fully durable eviction registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RegistryPublicationOutcome {
    Published,
    RootAdvanced,
    CoordinatorChanged,
    AuthorityLost,
    CoordinatorRetired,
}

/// Unforgeable proof that the coordinator lifecycle gate is held for an exact
/// root/registry transition. Its field is private to this module; the atomic
/// root layer can require the type but cannot construct it.
pub(crate) struct RegistryTransitionAuthority<'a> {
    _lifecycle: MutexGuard<'a, ()>,
}

/// Result of one generation-qualified eviction transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExactEvictionOutcome {
    Committed(usize, usize),
    RootAdvanced,
    AuthorityLost,
}

/// Result of one generation-qualified fault-in transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExactFaultOutcome {
    Committed,
    RootAdvanced,
    AuthorityLost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailedExactCommitOutcome {
    RootAdvanced,
    AuthorityLost,
}

/// Classify a failed exact root CAS from the defeating revision already
/// returned by `ArcSwap::compare_and_swap`.
///
/// A peer exact transaction on the same generation is retryable. A semantic
/// successor, retirement fence, or replacement generation has withdrawn the
/// captured capability and must terminate. This executes only on the CAS-loser
/// path and performs no additional root load, allocation, lock, or atomic RMW.
#[inline(always)]
fn classify_failed_exact_commit<K: KeyEncoding, V>(
    actual: Option<&RootRevision<K, V>>,
    required_binding: &crate::persistent_artrie::core::overlay::EvictionBinding,
) -> FailedExactCommitOutcome {
    match actual.and_then(RootRevision::eviction_binding) {
        Some(actual_binding) if actual_binding.same_publication(required_binding) => {
            FailedExactCommitOutcome::RootAdvanced
        }
        Some(_) | None => FailedExactCommitOutcome::AuthorityLost,
    }
}

/// Result of terminally retiring a coordinator from its owning trie.
///
/// Retirement is deliberately infallible: the caller owns the sole coordinator
/// slot, so even an inconsistent binding must be removed before that slot can be
/// exposed as empty. `InvariantRepaired` makes that exceptional recovery visible
/// to the trie API without preserving an orphaned root binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RetirementOutcome {
    ExactBindingDetached,
    AlreadyUnbound,
    InvariantRepaired,
}

impl RetirementOutcome {
    #[inline]
    pub(crate) fn repaired_invariant(self) -> bool {
        self == Self::InvariantRepaired
    }
}

type LegacyByteEviction = (u64, Vec<u8>, SwizzledPtr);
type LegacyCharEviction = (u64, Vec<char>, SwizzledPtr);

/// Detached, immutable catalog for deprecated materialized callbacks.
///
/// This wrapper is intentionally distinct from the exact coordinator catalog:
/// possessing it cannot satisfy any exact root-transition API.  The callback
/// retains an `Arc` snapshot across concurrent replacement.
struct DetachedCompatibilityRegistry {
    registry: DiskLocationRegistry,
}

struct PreparedLegacyCallback<T> {
    entries: Vec<T>,
    candidate_count: usize,
    selected_bytes: usize,
    _catalog: Arc<DetachedCompatibilityRegistry>,
}

/// Allocation-complete registry/stamp/root transaction produced by one frozen
/// checkpoint snapshot.
pub(crate) struct PreparedRegistryPublication<K: KeyEncoding, V: DictionaryValue> {
    coordinator: Arc<EvictionCoordinator>,
    registry: DiskLocationRegistry,
    root_binding: PreparedRootBinding<K, V>,
    stamps: Vec<DeferredDurableStamp<K, V>>,
}

impl<K: RegistryFamily, V: DictionaryValue> PreparedRegistryPublication<K, V> {
    #[cfg(test)]
    pub(crate) fn char_len(&self) -> usize {
        self.registry.char_len()
    }

    pub(crate) fn try_new(
        coordinator: Arc<EvictionCoordinator>,
        captured_root: &RootRevision<K, V>,
        mut registry: DiskLocationRegistry,
        stamps: Vec<DeferredDurableStamp<K, V>>,
    ) -> std::result::Result<Self, RegistryBuildError> {
        registry.try_finalize_for_publication()?;
        if !registry.is_publication_candidate() {
            return Err(RegistryBuildError::DestinationInvariant(
                "prepared publication registry was not detached",
            ));
        }
        let (resident_nodes, resident_serialized_bytes) = K::builder_resident_totals(&registry);
        let catalog = Arc::new(PublishedRegistryCatalog::try_from_builder(&registry)?);
        let root_binding = AtomicNodePtr::prepare_checkpoint_binding(
            captured_root,
            catalog,
            resident_nodes,
            resident_serialized_bytes,
        );
        Ok(Self {
            coordinator,
            registry,
            root_binding,
            stamps,
        })
    }

    /// Publish while the owning trie's coordinator-slot mutex is held.
    pub(crate) fn publish(
        self,
        installed: &Arc<EvictionCoordinator>,
        root: &AtomicNodePtr<K, V>,
    ) -> RegistryPublicationOutcome {
        if !Arc::ptr_eq(installed, &self.coordinator) {
            return RegistryPublicationOutcome::CoordinatorChanged;
        }
        self.coordinator.publish_prepared_registry(
            root,
            self.root_binding,
            self.registry,
            self.stamps,
        )
    }
}

/// Request for eviction with urgency level.
#[derive(Debug, Clone, Copy)]
struct EvictionRequest {
    urgency: EvictionUrgency,
    timestamp: Instant,
}

struct WorkerRunningGuard {
    coordinator: Weak<EvictionCoordinator>,
}

impl Drop for WorkerRunningGuard {
    fn drop(&mut self) {
        if let Some(coordinator) = self.coordinator.upgrade() {
            coordinator.running.store(false, Ordering::SeqCst);
        }
    }
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
    /// Detached advisory catalog used only by deprecated materialized callback
    /// APIs. Checkpoint publication never writes this slot, and exact eviction
    /// never reads it.
    detached_compatibility_catalog: ArcSwapOption<DetachedCompatibilityRegistry>,
    /// Stable trie-lifetime publication exclusion shared by every replacement
    /// coordinator installed for the same trie.
    publication_gate: Arc<RegistryPublicationGate>,
    /// Permanent detachment bit. Once set, stale `Arc`s cannot republish or
    /// authorize exact/compatibility transitions after trie-level disable.
    retired: AtomicBool,
    /// Whether the coordinator is running
    running: AtomicBool,
    /// Memory pressure monitor (optional)
    memory_monitor: RwLock<Option<Arc<MemoryPressureMonitor>>>,
}

impl EvictionCoordinator {
    fn publish_prepared_registry<K: KeyEncoding, V: DictionaryValue>(
        &self,
        root: &AtomicNodePtr<K, V>,
        root_binding: PreparedRootBinding<K, V>,
        registry: DiskLocationRegistry,
        stamps: Vec<DeferredDurableStamp<K, V>>,
    ) -> RegistryPublicationOutcome {
        self.publish_prepared_registry_with_stamp_action(
            root,
            root_binding,
            registry,
            stamps,
            |prepared_stamps| {
                for stamp in prepared_stamps {
                    stamp.apply();
                }
            },
        )
    }

    /// Monomorphized publication core. The stamp action is an inlineable seam
    /// used by deterministic concurrency tests; production passes the direct
    /// atomic-store loop and pays no trait-object or callback allocation.
    fn publish_prepared_registry_with_stamp_action<K: KeyEncoding, V: DictionaryValue, Apply>(
        &self,
        root: &AtomicNodePtr<K, V>,
        root_binding: PreparedRootBinding<K, V>,
        mut registry: DiskLocationRegistry,
        stamps: Vec<DeferredDurableStamp<K, V>>,
        apply_stamps: Apply,
    ) -> RegistryPublicationOutcome
    where
        Apply: FnOnce(&[DeferredDurableStamp<K, V>]),
    {
        debug_assert!(root_binding.binding().same_publication(&registry.binding()));
        debug_assert!(registry.topologies_are_finalized());
        let publication_binding = registry.binding();
        let lifecycle = self.publication_gate.lock_lifecycle();
        if self.retired.load(Ordering::Acquire) {
            return RegistryPublicationOutcome::CoordinatorRetired;
        }
        registry.begin_prepared_publication();
        let mut registry_slot = self.disk_registry.write();
        let previous = std::mem::replace(&mut *registry_slot, registry);
        let (outcome, retired) = if root.publish_checkpoint_binding(&root_binding).is_err() {
            let rejected = std::mem::replace(&mut *registry_slot, previous);
            drop(registry_slot);
            (RegistryPublicationOutcome::RootAdvanced, rejected)
        } else {
            drop(registry_slot);
            apply_stamps(&stamps);
            let mut registry_slot = self.disk_registry.write();
            let root_remains_bound = root.load_revision().is_some_and(|revision| {
                revision
                    .eviction_binding()
                    .is_some_and(|binding| binding.same_publication(&publication_binding))
            });
            let activated = root_remains_bound
                && registry_slot.try_finish_prepared_publication(&publication_binding);
            if !activated {
                registry_slot.invalidate();
            }
            drop(registry_slot);
            (
                if activated {
                    RegistryPublicationOutcome::Published
                } else {
                    RegistryPublicationOutcome::AuthorityLost
                },
                previous,
            )
        };
        drop(lifecycle);
        drop(retired);
        drop(stamps);
        outcome
    }

    /// Create a new eviction coordinator.
    ///
    /// The coordinator is created in a stopped state. Call `start()` with
    /// the eviction callback to begin processing eviction requests.
    pub fn new(config: EvictionConfig, epoch_manager: Arc<EpochManager>) -> Arc<Self> {
        Self::new_with_publication_gate(config, epoch_manager, RegistryPublicationGate::new())
    }

    /// Create a coordinator sharing one stable trie-lifetime publication gate.
    pub(crate) fn new_with_publication_gate(
        config: EvictionConfig,
        epoch_manager: Arc<EpochManager>,
        publication_gate: Arc<RegistryPublicationGate>,
    ) -> Arc<Self> {
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
            shutdown: AtomicBool::new(false),
            eviction_thread: Mutex::new(None),
            stats: Arc::new(EvictionStatsAtomic::new()),
            last_eviction: AtomicU64::new(0),
            disk_registry: RwLock::new(DiskLocationRegistry::new()),
            detached_compatibility_catalog: ArcSwapOption::empty(),
            publication_gate,
            retired: AtomicBool::new(false),
            running: AtomicBool::new(false),
            memory_monitor: RwLock::new(None),
        })
    }

    fn try_prepare_legacy_byte_callback(
        &self,
        target_bytes: usize,
        max_count: usize,
        overhead: usize,
    ) -> Option<PreparedLegacyCallback<LegacyByteEviction>> {
        let catalog = self.detached_compatibility_catalog.load_full()?;
        let registry = &catalog.registry;
        let batch = registry.select_compact_for_compatibility(
            target_bytes,
            &self.lru_registry,
            self.config.min_eviction_depth,
            max_count,
            overhead,
        );
        if batch.candidates.is_empty() {
            return None;
        }
        let mut entries = Vec::new();
        entries.try_reserve_exact(batch.candidates.len()).ok()?;
        let mut selected_bytes = 0usize;
        for candidate in &batch.candidates {
            selected_bytes =
                selected_bytes.checked_add(candidate.size_bytes.checked_add(overhead)?)?;
            entries.push((
                candidate.path_hash,
                batch.materialize_path(candidate.path_id)?,
                candidate.disk_ptr.clone(),
            ));
        }
        let candidate_count = batch.candidates.len();
        Some(PreparedLegacyCallback {
            entries,
            candidate_count,
            selected_bytes,
            _catalog: catalog,
        })
    }

    fn try_prepare_legacy_char_callback(
        &self,
        target_bytes: usize,
        max_count: usize,
        overhead: usize,
    ) -> Option<PreparedLegacyCallback<LegacyCharEviction>> {
        let catalog = self.detached_compatibility_catalog.load_full()?;
        let registry = &catalog.registry;
        let batch = registry.select_compact_char_for_compatibility(
            target_bytes,
            &self.lru_registry,
            self.config.min_eviction_depth,
            max_count,
            overhead,
        );
        if batch.candidates.is_empty() {
            return None;
        }
        let mut entries = Vec::new();
        entries.try_reserve_exact(batch.candidates.len()).ok()?;
        let mut selected_bytes = 0usize;
        for candidate in &batch.candidates {
            selected_bytes =
                selected_bytes.checked_add(candidate.size_bytes.checked_add(overhead)?)?;
            entries.push((
                candidate.path_hash,
                batch.materialize_char_path(candidate.path_id)?,
                candidate.disk_ptr.clone(),
            ));
        }
        let candidate_count = batch.candidates.len();
        Some(PreparedLegacyCallback {
            entries,
            candidate_count,
            selected_bytes,
            _catalog: catalog,
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

    fn spawn_worker<W>(self: &Arc<Self>, name: &str, worker: W) -> Result<(), String>
    where
        W: FnOnce(Weak<Self>) + Send + 'static,
    {
        if !self.config.enabled {
            return Ok(());
        }

        let mut thread_slot = self.eviction_thread.lock();
        if !self.running.load(Ordering::SeqCst) {
            if let Some(finished) = thread_slot.take() {
                if finished.join().is_err() {
                    log::warn!("prior eviction worker terminated by panic; restarting cleanly");
                }
            }
        }
        if self
            .running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err("Eviction coordinator already running".to_string());
        }
        self.shutdown.store(false, Ordering::SeqCst);

        let weak = Arc::downgrade(self);
        let guard_weak = weak.clone();
        match thread::Builder::new()
            .name(name.to_string())
            .spawn(move || {
                let _running_guard = WorkerRunningGuard {
                    coordinator: guard_weak,
                };
                worker(weak);
            }) {
            Ok(handle) => {
                *thread_slot = Some(handle);
                Ok(())
            }
            Err(error) => {
                self.running.store(false, Ordering::SeqCst);
                Err(format!("Failed to spawn eviction thread: {error}"))
            }
        }
    }

    /// Start the eviction coordinator with a callback for performing evictions.
    ///
    /// The callback is invoked for each batch of nodes to evict. It receives:
    /// - A list of (path_hash, path, disk_ptr) tuples for nodes to evict
    /// - Returns the number of successfully evicted nodes and bytes freed
    ///
    /// This is a source-compatible, untyped callback boundary over an immutable
    /// detached compatibility catalog. It cannot observe or authorize exact
    /// root-bound residency changes. New in-crate consumers use the
    /// generation-qualified compact interface instead.
    ///
    /// # Type Parameters
    ///
    /// * `F` - Callback function type
    ///
    /// # Arguments
    ///
    /// * `self_arc` - Arc to this coordinator (for the eviction thread)
    /// * `callback` - Function to perform the actual node eviction
    #[deprecated(
        note = "detached advisory callback only; use trie-level exact eviction for reclamation"
    )]
    pub fn start<F>(self: &Arc<Self>, callback: F) -> Result<(), String>
    where
        F: Fn(Vec<(u64, Vec<u8>, SwizzledPtr)>) -> (usize, usize) + Send + Sync + 'static,
    {
        let callback = Arc::new(callback);
        self.spawn_worker("artrie-eviction", move |weak| {
            Self::eviction_loop(weak, callback)
        })
    }

    /// Start the coordinator for char-level tries.
    ///
    /// This has the same detached, compatibility-only callback contract as
    /// [`Self::start`].
    #[deprecated(
        note = "detached advisory callback only; use trie-level exact eviction for reclamation"
    )]
    pub fn start_char<F>(self: &Arc<Self>, callback: F) -> Result<(), String>
    where
        F: Fn(Vec<(u64, Vec<char>, SwizzledPtr)>) -> (usize, usize) + Send + Sync + 'static,
    {
        let callback = Arc::new(callback);
        self.spawn_worker("artrie-eviction-char", move |weak| {
            Self::eviction_loop_char(weak, callback)
        })
    }

    #[cfg(any(test, feature = "bench-internals"))]
    pub(crate) fn start_compact<F>(self: &Arc<Self>, callback: F) -> Result<(), String>
    where
        F: Fn(CompactEvictionBatch<u8>) -> (usize, usize) + Send + Sync + 'static,
    {
        let callback = Arc::new(callback);
        self.spawn_worker("artrie-eviction-compact", move |weak| {
            Self::eviction_loop_compact(weak, callback)
        })
    }

    #[cfg(any(test, feature = "bench-internals"))]
    pub(crate) fn start_compact_char<F>(self: &Arc<Self>, callback: F) -> Result<(), String>
    where
        F: Fn(CompactEvictionBatch<u32>) -> (usize, usize) + Send + Sync + 'static,
    {
        let callback = Arc::new(callback);
        self.spawn_worker("artrie-eviction-char-compact", move |weak| {
            Self::eviction_loop_compact_char(weak, callback)
        })
    }

    pub(crate) fn start_root_compact<F>(self: &Arc<Self>, callback: F) -> Result<(), String>
    where
        F: Fn(usize) -> (usize, usize) + Send + Sync + 'static,
    {
        let callback = Arc::new(callback);
        self.spawn_worker("artrie-eviction-root-compact", move |weak| {
            Self::eviction_loop_root_compact(weak, callback, true)
        })
    }

    pub(crate) fn start_root_compact_char<F>(self: &Arc<Self>, callback: F) -> Result<(), String>
    where
        F: Fn(usize) -> (usize, usize) + Send + Sync + 'static,
    {
        let callback = Arc::new(callback);
        self.spawn_worker("artrie-eviction-char-root-compact", move |weak| {
            Self::eviction_loop_root_compact(weak, callback, false)
        })
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
        // The worker polls `try_pop_request` every 100 ms (it drives itself
        // through a `Weak<Self>`), so there is no condvar to notify here.
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
            0, // on-disk-unit target (async/public-batch path; resident overhead added only by the budget tail)
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
    /// Selection reads only the detached compatibility catalog installed by
    /// [`Self::try_install_detached_compatibility_catalog`]. A checkpoint's exact root-bound
    /// catalog is deliberately invisible here. The callback owns an immutable
    /// snapshot across concurrent detached-catalog replacement.
    #[deprecated(
        note = "detached advisory callback only; use trie-level exact eviction for reclamation"
    )]
    pub fn force_eviction_char<F>(&self, target_bytes: usize, callback: F) -> (usize, usize)
    where
        F: Fn(Vec<(u64, Vec<char>, SwizzledPtr)>) -> (usize, usize),
    {
        let Some(prepared) =
            self.try_prepare_legacy_char_callback(target_bytes, self.config.batch_size, 0)
        else {
            return (0, 0);
        };
        let PreparedLegacyCallback {
            entries, _catalog, ..
        } = prepared;
        callback(entries)
    }

    /// Synchronously evict cold *byte* nodes, invoking `callback` inline on the
    /// calling thread to reclaim them — the BYTE twin of
    /// [`force_eviction_char`](Self::force_eviction_char) (Phase 6).
    ///
    /// The byte-map `force_eviction` only *selects and counts*; this method also
    /// *performs reclamation* by invoking `callback` (the overlay evict driver), giving a
    /// deterministic single-threaded eviction path with no eviction thread / quiescence
    /// wait / cooldown. Selection reads only the immutable detached compatibility
    /// catalog, never a checkpoint's exact root-bound catalog. `callback`
    /// receives `(path_hash, path: Vec<u8>, disk_ptr)` per advisory candidate.
    #[deprecated(
        note = "detached advisory callback only; use trie-level exact eviction for reclamation"
    )]
    pub fn force_eviction_bytes<F>(&self, target_bytes: usize, callback: F) -> (usize, usize)
    where
        F: Fn(Vec<(u64, Vec<u8>, SwizzledPtr)>) -> (usize, usize),
    {
        let Some(prepared) =
            self.try_prepare_legacy_byte_callback(target_bytes, self.config.batch_size, 0)
        else {
            return (0, 0);
        };
        let PreparedLegacyCallback {
            entries, _catalog, ..
        } = prepared;
        callback(entries)
    }

    /// Root-qualified byte selection against the helped packed residency image.
    /// Selection copies each packed word once, then revalidates exact root
    /// identity before exposing candidates; no coordinator registry lock is
    /// involved.
    pub(crate) fn force_eviction_compact_bytes_root<V, F>(
        &self,
        root: &AtomicNodePtr<ByteKey, V>,
        target_bytes: usize,
        callback: F,
    ) -> (usize, usize)
    where
        V: DictionaryValue,
        F: Fn(CompactEvictionBatch<u8>) -> (usize, usize),
    {
        self.force_eviction_compact_bytes_root_with_max_count(
            root,
            target_bytes,
            self.config.batch_size,
            callback,
        )
    }

    pub(crate) fn force_eviction_compact_bytes_root_with_max_count<V, F>(
        &self,
        root: &AtomicNodePtr<ByteKey, V>,
        target_bytes: usize,
        max_count: usize,
        callback: F,
    ) -> (usize, usize)
    where
        V: DictionaryValue,
        F: Fn(CompactEvictionBatch<u8>) -> (usize, usize),
    {
        let Some(revision) = root.load_revision() else {
            return (0, 0);
        };
        let Some(eviction) = revision.help_eviction_revision() else {
            return (0, 0);
        };
        let Ok(snapshot) = eviction
            .catalog()
            .try_byte_selection_snapshot(eviction.ordinal())
        else {
            return (0, 0);
        };
        if !root
            .load_revision()
            .is_some_and(|current| revision.same_revision(&current))
        {
            return (0, 0);
        }
        let batch = snapshot.select_compact(
            target_bytes,
            &self.lru_registry,
            self.config.min_eviction_depth,
            max_count,
            0,
        );
        if batch.candidates.is_empty() {
            return (0, 0);
        }
        callback(batch)
    }

    pub(crate) fn force_eviction_compact_char_root<V, F>(
        &self,
        root: &AtomicNodePtr<CharKey, V>,
        target_bytes: usize,
        callback: F,
    ) -> (usize, usize)
    where
        V: DictionaryValue,
        F: Fn(CompactEvictionBatch<u32>) -> (usize, usize),
    {
        self.force_eviction_compact_char_root_with_max_count(
            root,
            target_bytes,
            self.config.batch_size,
            callback,
        )
    }

    pub(crate) fn force_eviction_compact_char_root_with_max_count<V, F>(
        &self,
        root: &AtomicNodePtr<CharKey, V>,
        target_bytes: usize,
        max_count: usize,
        callback: F,
    ) -> (usize, usize)
    where
        V: DictionaryValue,
        F: Fn(CompactEvictionBatch<u32>) -> (usize, usize),
    {
        let Some(revision) = root.load_revision() else {
            return (0, 0);
        };
        let Some(eviction) = revision.help_eviction_revision() else {
            return (0, 0);
        };
        let Ok(snapshot) = eviction
            .catalog()
            .try_char_selection_snapshot(eviction.ordinal())
        else {
            return (0, 0);
        };
        if !root
            .load_revision()
            .is_some_and(|current| revision.same_revision(&current))
        {
            return (0, 0);
        }
        let batch = snapshot.select_compact(
            target_bytes,
            &self.lru_registry,
            self.config.min_eviction_depth,
            max_count,
            0,
        );
        if batch.candidates.is_empty() {
            return (0, 0);
        }
        callback(batch)
    }

    pub(crate) fn root_resident_totals<K, V>(
        &self,
        root: &AtomicNodePtr<K, V>,
    ) -> Option<(usize, usize)>
    where
        K: RegistryFamily,
        V: DictionaryValue,
    {
        root.load_revision()?
            .help_eviction_revision()
            .map(|eviction| eviction.resident_totals())
    }

    pub(crate) fn root_resident_estimate_bytes<K, V>(
        &self,
        root: &AtomicNodePtr<K, V>,
        overhead: usize,
    ) -> Option<usize>
    where
        K: RegistryFamily,
        V: DictionaryValue,
    {
        let (nodes, serialized_bytes) = self.root_resident_totals(root)?;
        serialized_bytes.checked_add(nodes.checked_mul(overhead)?)
    }

    #[inline]
    pub(crate) fn byte_root_resident_estimate_bytes<V>(
        &self,
        root: &AtomicNodePtr<ByteKey, V>,
    ) -> Option<usize>
    where
        V: DictionaryValue,
    {
        self.root_resident_estimate_bytes(root, STRUCT_OVERHEAD_BYTE)
    }

    #[inline]
    pub(crate) fn char_root_resident_estimate_bytes<V>(
        &self,
        root: &AtomicNodePtr<CharKey, V>,
    ) -> Option<usize>
    where
        V: DictionaryValue,
    {
        self.root_resident_estimate_bytes(root, STRUCT_OVERHEAD_CHAR)
    }

    pub(crate) fn force_eviction_compact_bytes_resident_root<V, F>(
        &self,
        root: &AtomicNodePtr<ByteKey, V>,
        target_bytes: usize,
        max_count: usize,
        callback: F,
    ) -> (usize, usize)
    where
        V: DictionaryValue,
        F: Fn(CompactEvictionBatch<u8>) -> (usize, usize),
    {
        let Some(revision) = root.load_revision() else {
            return (0, 0);
        };
        let Some(eviction) = revision.help_eviction_revision() else {
            return (0, 0);
        };
        let Ok(snapshot) = eviction
            .catalog()
            .try_byte_selection_snapshot(eviction.ordinal())
        else {
            return (0, 0);
        };
        if !root
            .load_revision()
            .is_some_and(|current| revision.same_revision(&current))
        {
            return (0, 0);
        }
        let batch = snapshot.select_resident_budget(
            target_bytes,
            &self.lru_registry,
            self.config.min_eviction_depth,
            max_count,
            STRUCT_OVERHEAD_BYTE,
        );
        if batch.report.cap_exhausted {
            log::warn!(
                "overlay eviction: byte resident-budget anchor cap exhausted — planned {}B < \
                 target {target_bytes}B from {}/{} nonredundant anchors ({} eligible)",
                batch.report.planned_bytes,
                batch.report.selected_priority_count,
                batch.report.nonredundant_candidates,
                batch.report.eligible_candidates,
            );
        }
        if batch.report.eligible_exhausted {
            log::warn!(
                "overlay eviction: byte resident budget structurally unreachable — planned {}B < \
                 target {target_bytes}B after all {} eligible anchors \
                 (min_eviction_depth={} pins remaining residency)",
                batch.report.planned_bytes,
                batch.report.eligible_candidates,
                self.config.min_eviction_depth,
            );
        }
        if batch.candidates.is_empty() {
            return (0, 0);
        }
        let start = Instant::now();
        self.record_resident_eviction(start, callback(batch))
    }

    pub(crate) fn force_eviction_compact_char_resident_root<V, F>(
        &self,
        root: &AtomicNodePtr<CharKey, V>,
        target_bytes: usize,
        max_count: usize,
        callback: F,
    ) -> (usize, usize)
    where
        V: DictionaryValue,
        F: Fn(CompactEvictionBatch<u32>) -> (usize, usize),
    {
        let Some(revision) = root.load_revision() else {
            return (0, 0);
        };
        let Some(eviction) = revision.help_eviction_revision() else {
            return (0, 0);
        };
        let Ok(snapshot) = eviction
            .catalog()
            .try_char_selection_snapshot(eviction.ordinal())
        else {
            return (0, 0);
        };
        if !root
            .load_revision()
            .is_some_and(|current| revision.same_revision(&current))
        {
            return (0, 0);
        }
        let batch = snapshot.select_resident_budget(
            target_bytes,
            &self.lru_registry,
            self.config.min_eviction_depth,
            max_count,
            STRUCT_OVERHEAD_CHAR,
        );
        if batch.report.cap_exhausted {
            log::warn!(
                "overlay eviction: char resident-budget anchor cap exhausted — planned {}B < \
                 target {target_bytes}B from {}/{} nonredundant anchors ({} eligible)",
                batch.report.planned_bytes,
                batch.report.selected_priority_count,
                batch.report.nonredundant_candidates,
                batch.report.eligible_candidates,
            );
        }
        if batch.report.eligible_exhausted {
            log::warn!(
                "overlay eviction: char resident budget structurally unreachable — planned {}B < \
                 target {target_bytes}B after all {} eligible anchors \
                 (min_eviction_depth={} pins remaining residency)",
                batch.report.planned_bytes,
                batch.report.eligible_candidates,
                self.config.min_eviction_depth,
            );
        }
        if batch.candidates.is_empty() {
            return (0, 0);
        }
        let start = Instant::now();
        self.record_resident_eviction(start, callback(batch))
    }

    /// Source-compatible materialized-path resident-unit arity for external
    /// callbacks. This retains local candidate accounting and descendant-first
    /// compatibility semantics. Checkpoint tails do **not** use this boundary;
    /// they use `force_eviction_compact_char_resident_root` for exact laminar
    /// closure accounting and generation-qualified ancestor-first execution.
    #[deprecated(note = "detached advisory callback only; use trie-level exact resident eviction")]
    pub fn force_eviction_char_resident<F>(
        &self,
        target_bytes: usize,
        max_count: usize,
        callback: F,
    ) -> (usize, usize)
    where
        F: Fn(Vec<(u64, Vec<char>, SwizzledPtr)>) -> (usize, usize),
    {
        let Some(prepared) =
            self.try_prepare_legacy_char_callback(target_bytes, max_count, STRUCT_OVERHEAD_CHAR)
        else {
            return (0, 0);
        };
        let PreparedLegacyCallback {
            entries,
            candidate_count,
            selected_bytes,
            _catalog,
        } = prepared;
        // No-silent-cap: if we exhausted the eligible set (did NOT hit `max_count`) yet
        // its resident sum is below the target, the `min_eviction_depth` floor pins the
        // remainder — the budget cannot be met without lowering the floor.
        if selected_bytes < target_bytes && candidate_count < max_count {
            log::warn!(
                "overlay eviction: char resident budget unreachable — evicted all eligible \
                 {selected_bytes}B < target {target_bytes}B (min_eviction_depth={} pins shallow nodes)",
                self.config.min_eviction_depth
            );
        }
        let start = Instant::now();
        self.record_resident_eviction(start, callback(entries))
    }

    /// The CHECKPOINT-TAIL budget arity (byte) — the `Vec<u8>`-path twin of
    /// [`Self::force_eviction_char_resident`]. `STRUCT_OVERHEAD_BYTE` per node.
    #[deprecated(note = "detached advisory callback only; use trie-level exact resident eviction")]
    pub fn force_eviction_bytes_resident<F>(
        &self,
        target_bytes: usize,
        max_count: usize,
        callback: F,
    ) -> (usize, usize)
    where
        F: Fn(Vec<(u64, Vec<u8>, SwizzledPtr)>) -> (usize, usize),
    {
        let Some(prepared) =
            self.try_prepare_legacy_byte_callback(target_bytes, max_count, STRUCT_OVERHEAD_BYTE)
        else {
            return (0, 0);
        };
        let PreparedLegacyCallback {
            entries,
            candidate_count,
            selected_bytes,
            _catalog,
        } = prepared;
        if selected_bytes < target_bytes && candidate_count < max_count {
            log::warn!(
                "overlay eviction: byte resident budget unreachable — evicted all eligible \
                 {selected_bytes}B < target {target_bytes}B (min_eviction_depth={} pins shallow nodes)",
                self.config.min_eviction_depth
            );
        }
        let start = Instant::now();
        self.record_resident_eviction(start, callback(entries))
    }

    /// The configured resident-heap budget (on-disk-equivalent bytes), or `None` if
    /// unbounded (the default — the checkpoint tail evicts nothing).
    pub fn resident_budget_bytes(&self) -> Option<usize> {
        self.config.resident_budget_bytes
    }

    /// The configured per-checkpoint eviction node-cap (`None` = uncapped).
    pub fn resident_budget_eviction_cap(&self) -> Option<usize> {
        self.config.resident_budget_eviction_cap
    }

    /// Number of char nodes in the exact checkpoint registry bound to the
    /// current overlay-root generation. Detached compatibility catalogs
    /// installed through [`Self::try_install_detached_compatibility_catalog`] are deliberately
    /// excluded.
    pub fn disk_registry_char_len(&self) -> usize {
        self.disk_registry.read().char_len()
    }

    /// Number of byte nodes in the exact checkpoint registry bound to the
    /// current overlay-root generation. This is the byte twin of
    /// [`Self::disk_registry_char_len`]; detached compatibility catalogs are not
    /// authority and are deliberately excluded.
    pub fn disk_registry_len(&self) -> usize {
        self.disk_registry.read().len()
    }

    /// Get the LRU registry for access tracking.
    pub fn lru_registry(&self) -> &Arc<LruRegistry> {
        &self.lru_registry
    }

    /// Install a structurally valid detached compatibility catalog.
    ///
    /// This source-compatible API deliberately installs the registry detached
    /// from any overlay root revision. It supports inspection and materialized
    /// legacy callbacks but cannot authorize compact eviction or exact fault
    /// residency transitions. Installation is still a publication-lifecycle
    /// operation: a retired coordinator rejects the replacement without
    /// modifying the current catalog. Concurrent callbacks retain their old
    /// immutable snapshot. Checkpoint code uses
    /// the internal `PreparedRegistryPublication` transaction to atomically bind and activate exact
    /// authority.
    ///
    /// # Panics
    ///
    /// Panics when the registry is not structurally publication-ready, this
    /// coordinator has been retired, or the detached catalog cannot be
    /// installed. Concurrent or otherwise fallible callers should use
    /// [`Self::try_install_detached_compatibility_catalog`] and handle the typed error.
    pub fn install_detached_compatibility_catalog(&self, registry: DiskLocationRegistry) {
        self.try_install_detached_compatibility_catalog(registry)
            .expect("detached compatibility catalog is publication-ready");
    }

    /// Fallible detached-install boundary for hierarchical serializer catalogs
    /// whose preorder topology must be finalized before structural exposure.
    /// Installation occurs only while the coordinator is live; failure
    /// preserves the previously installed detached catalog.
    ///
    /// # Errors
    ///
    /// Returns an error when topology finalization fails or the coordinator is
    /// retired. Concurrent callbacks retain their prior immutable snapshot.
    pub fn try_install_detached_compatibility_catalog(
        &self,
        mut registry: DiskLocationRegistry,
    ) -> crate::persistent_artrie::error::Result<()> {
        registry.try_finalize_for_publication().map_err(|error| {
            crate::persistent_artrie::error::PersistentARTrieError::internal(format!(
                "finalize eviction registry before coordinator update: {error}"
            ))
        })?;
        registry.detach_for_direct_install();
        self.install_finalized_registry(registry)
    }

    /// Deprecated source-compatible detached-catalog installer.
    ///
    /// This unit-returning compatibility wrapper never panics. A malformed
    /// registry or retired coordinator rejects the candidate without publishing
    /// it. Another concurrent successful install may independently replace the
    /// discovery slot. The wrapper invokes no user-supplied logging callback, so
    /// rejection remains total. Callers that must distinguish rejection from
    /// installation use
    /// [`Self::try_install_detached_compatibility_catalog`].
    #[deprecated(
        note = "installs only a detached advisory catalog; use try_install_detached_compatibility_catalog for typed rejection"
    )]
    #[inline]
    pub fn update_disk_registry(&self, registry: DiskLocationRegistry) {
        let _ = self.try_install_detached_compatibility_catalog(registry);
    }

    /// Deprecated ambiguous name for
    /// [`Self::try_install_detached_compatibility_catalog`].
    #[deprecated(
        note = "installs only a detached advisory catalog; use try_install_detached_compatibility_catalog"
    )]
    #[inline]
    pub fn try_update_disk_registry(
        &self,
        registry: DiskLocationRegistry,
    ) -> crate::persistent_artrie::error::Result<()> {
        self.try_install_detached_compatibility_catalog(registry)
    }

    fn install_finalized_registry(
        &self,
        registry: DiskLocationRegistry,
    ) -> crate::persistent_artrie::error::Result<()> {
        debug_assert!(registry.topologies_are_finalized());
        debug_assert!(registry.is_publication_candidate());
        let previous = {
            let _lifecycle = self.publication_gate.lock_lifecycle();
            if self.retired.load(Ordering::Acquire) {
                return Err(
                    crate::persistent_artrie::error::PersistentARTrieError::InvalidOperation(
                        "cannot install a registry on a retired eviction coordinator".to_string(),
                    ),
                );
            }
            self.detached_compatibility_catalog
                .swap(Some(Arc::new(DetachedCompatibilityRegistry { registry })))
        };
        drop(previous);
        Ok(())
    }

    /// Permanently detach this coordinator from its owning trie.
    ///
    /// The trie holds its coordinator-slot mutex while calling this method. The
    /// stable lifecycle gate serializes retirement with publication and exact
    /// residency transitions. Registry authority is invalidated before the slot
    /// can become empty, and the irreversible bit prevents stale `Arc`s from
    /// recreating authority or admitting compatibility callbacks afterwards.
    pub(crate) fn retire_from_trie(&self) {
        let _lifecycle = self.publication_gate.lock_lifecycle();
        self.retired.store(true, Ordering::Release);
        self.disk_registry.write().invalidate();
        self.detached_compatibility_catalog.store(None);
    }

    /// Permanently detach this coordinator and atomically unbind its trie's root.
    ///
    /// The caller holds the coordinator-slot mutex until this method returns.
    /// The stable lifecycle gate and registry write lock exclude checkpoint,
    /// eviction, and fault commits. A racing semantic CAS is harmless because it
    /// also publishes an unbound revision; retirement retries only while the
    /// observed root remains bound.
    pub(crate) fn retire_from_trie_with_root<K, V>(
        &self,
        root: &AtomicNodePtr<K, V>,
    ) -> RetirementOutcome
    where
        K: KeyEncoding,
        V: DictionaryValue,
    {
        let authority = RegistryTransitionAuthority {
            _lifecycle: self.publication_gate.lock_lifecycle(),
        };
        self.retired.store(true, Ordering::Release);
        let mut registry = self.disk_registry.write();
        let registry_binding = registry.binding();
        let mut current = root.load_revision();
        let root_binding = current
            .as_ref()
            .and_then(|revision| revision.eviction_binding());
        let mut outcome = match root_binding {
            Some(actual) if actual.same_publication(&registry_binding) => {
                RetirementOutcome::ExactBindingDetached
            }
            None => RetirementOutcome::AlreadyUnbound,
            _ => RetirementOutcome::InvariantRepaired,
        };

        // Publish an unconditional unbound fence even when the observed root is
        // already unbound.  The fresh root identity prevents a checkpoint that
        // captured the pre-retirement revision from publishing after the
        // coordinator slot is cleared.  This is the sole extra CAS on the cold
        // disable/close path and is required by the retirement-fence model.
        while let Some(observed) = current.as_ref() {
            let prepared: PreparedRootDetachment<K, V> =
                AtomicNodePtr::prepare_retirement_detachment(observed);
            match root.publish_retirement_detachment(&prepared, &authority) {
                Ok(_) => current = None,
                Err(actual) => {
                    if actual
                        .as_ref()
                        .and_then(|revision| revision.eviction_binding())
                        .is_some()
                    {
                        outcome = RetirementOutcome::InvariantRepaired;
                    }
                    current = actual;
                }
            }
        }
        registry.invalidate();
        self.detached_compatibility_catalog.store(None);
        outcome
    }

    /// Capture the current registry's immutable structural image for a
    /// successor checkpoint. This remains available after a semantic root CAS
    /// clears exact authority: it can only drive exact metadata grafts, never
    /// residency transitions. The read lock is released before any arena I/O or
    /// serialization begins.
    pub(crate) fn registry_structural_source(
        &self,
    ) -> std::result::Result<Option<RegistryStructuralSource>, RegistryBuildError> {
        let mut byte_residency_bits = Vec::new();
        let mut char_residency_bits = Vec::new();
        loop {
            let plan = self.disk_registry.read().structural_source_capture_plan()?;
            plan.try_prepare_buffers(&mut byte_residency_bits, &mut char_residency_bits)?;
            let capture = self.disk_registry.read().try_capture_structural_source(
                &plan,
                byte_residency_bits,
                char_residency_bits,
            )?;
            match capture {
                RegistryStructuralCapture::Ready(source) => return Ok(Some(source)),
                RegistryStructuralCapture::Retry {
                    byte_residency_bits: retry_byte,
                    char_residency_bits: retry_char,
                } => {
                    byte_residency_bits = retry_byte;
                    char_residency_bits = retry_char;
                }
            }
        }
    }

    /// Prepare every fallible/allocation-bearing side effect of a successful
    /// byte overlay batch before its root CAS.
    pub(crate) fn prepare_byte_eviction_commit<K, V>(
        &self,
        root_revision: &RootRevision<K, V>,
        batch: &CompactEvictionBatch<u8>,
        successful: &mut [usize],
    ) -> Option<PreparedPackedResidency>
    where
        K: RegistryFamily<Unit = u8>,
        V: DictionaryValue,
    {
        let eviction = root_revision.help_eviction_revision()?;
        let (resident_nodes, resident_serialized_bytes) = eviction.resident_totals();
        eviction.catalog().prepare_byte_eviction_packed(
            eviction.ordinal(),
            resident_nodes,
            resident_serialized_bytes,
            batch,
            successful,
        )
    }

    /// Revalidate, publish, and commit one byte eviction as a single lifecycle
    /// transaction. Nothing after the root CAS can allocate or fail.
    pub(crate) fn commit_byte_eviction_transaction<K, V>(
        &self,
        root: &AtomicNodePtr<K, V>,
        root_transition: PreparedBoundRootTransition<K, V>,
    ) -> ExactEvictionOutcome
    where
        K: RegistryFamily<Unit = u8>,
        V: DictionaryValue,
    {
        let Some(result) = root_transition.evicted_totals() else {
            return ExactEvictionOutcome::AuthorityLost;
        };
        let outcome = match root.publish_bound_root_transition(&root_transition) {
            Ok(_) => ExactEvictionOutcome::Committed(result.0, result.1),
            Err(actual) => {
                match classify_failed_exact_commit(actual.as_ref(), root_transition.binding()) {
                    FailedExactCommitOutcome::RootAdvanced => ExactEvictionOutcome::RootAdvanced,
                    FailedExactCommitOutcome::AuthorityLost => ExactEvictionOutcome::AuthorityLost,
                }
            }
        };
        if matches!(outcome, ExactEvictionOutcome::Committed(_, _)) {
            let valid = root_transition
                .for_each_cleared_path_hash(|path_hash| self.lru_registry.remove_hash(path_hash));
            debug_assert!(valid, "published eviction delta must name catalog records");
        }
        outcome
    }

    /// Character-key twin of [`Self::prepare_byte_eviction_commit`].
    pub(crate) fn prepare_char_eviction_commit<K, V>(
        &self,
        root_revision: &RootRevision<K, V>,
        batch: &CompactEvictionBatch<u32>,
        successful: &mut [usize],
    ) -> Option<PreparedPackedResidency>
    where
        K: RegistryFamily<Unit = u32>,
        V: DictionaryValue,
    {
        let eviction = root_revision.help_eviction_revision()?;
        let (resident_nodes, resident_serialized_bytes) = eviction.resident_totals();
        eviction.catalog().prepare_char_eviction_packed(
            eviction.ordinal(),
            resident_nodes,
            resident_serialized_bytes,
            batch,
            successful,
        )
    }

    /// Character-key twin of [`Self::commit_byte_eviction_transaction`].
    pub(crate) fn commit_char_eviction_transaction<K, V>(
        &self,
        root: &AtomicNodePtr<K, V>,
        root_transition: PreparedBoundRootTransition<K, V>,
    ) -> ExactEvictionOutcome
    where
        K: RegistryFamily<Unit = u32>,
        V: DictionaryValue,
    {
        let Some(result) = root_transition.evicted_totals() else {
            return ExactEvictionOutcome::AuthorityLost;
        };
        let outcome = match root.publish_bound_root_transition(&root_transition) {
            Ok(_) => ExactEvictionOutcome::Committed(result.0, result.1),
            Err(actual) => {
                match classify_failed_exact_commit(actual.as_ref(), root_transition.binding()) {
                    FailedExactCommitOutcome::RootAdvanced => ExactEvictionOutcome::RootAdvanced,
                    FailedExactCommitOutcome::AuthorityLost => ExactEvictionOutcome::AuthorityLost,
                }
            }
        };
        if matches!(outcome, ExactEvictionOutcome::Committed(_, _)) {
            let valid = root_transition
                .for_each_cleared_path_hash(|path_hash| self.lru_registry.remove_hash(path_hash));
            debug_assert!(valid, "published eviction delta must name catalog records");
        }
        outcome
    }

    pub(crate) fn prepare_byte_fault_commit<K, V>(
        &self,
        root_revision: &RootRevision<K, V>,
        path: &[u8],
        disk_ptr: &SwizzledPtr,
    ) -> Option<PreparedPackedResidency>
    where
        K: RegistryFamily<Unit = u8>,
        V: DictionaryValue,
    {
        let eviction = root_revision.help_eviction_revision()?;
        let (resident_nodes, resident_serialized_bytes) = eviction.resident_totals();
        eviction.catalog().prepare_byte_fault_packed(
            eviction.ordinal(),
            resident_nodes,
            resident_serialized_bytes,
            path,
            disk_ptr,
        )
    }

    pub(crate) fn commit_byte_fault_transaction<K, V>(
        &self,
        root: &AtomicNodePtr<K, V>,
        root_transition: PreparedBoundRootTransition<K, V>,
    ) -> ExactFaultOutcome
    where
        K: RegistryFamily<Unit = u8>,
        V: DictionaryValue,
    {
        let outcome = match root.publish_bound_root_transition(&root_transition) {
            Ok(_) => ExactFaultOutcome::Committed,
            Err(actual) => {
                match classify_failed_exact_commit(actual.as_ref(), root_transition.binding()) {
                    FailedExactCommitOutcome::RootAdvanced => ExactFaultOutcome::RootAdvanced,
                    FailedExactCommitOutcome::AuthorityLost => ExactFaultOutcome::AuthorityLost,
                }
            }
        };
        outcome
    }

    pub(crate) fn prepare_char_fault_commit<K, V>(
        &self,
        root_revision: &RootRevision<K, V>,
        path: &[u32],
        disk_ptr: &SwizzledPtr,
    ) -> Option<PreparedPackedResidency>
    where
        K: RegistryFamily<Unit = u32>,
        V: DictionaryValue,
    {
        let eviction = root_revision.help_eviction_revision()?;
        let (resident_nodes, resident_serialized_bytes) = eviction.resident_totals();
        eviction.catalog().prepare_char_fault_packed(
            eviction.ordinal(),
            resident_nodes,
            resident_serialized_bytes,
            path,
            disk_ptr,
        )
    }
    pub(crate) fn commit_char_fault_transaction<K, V>(
        &self,
        root: &AtomicNodePtr<K, V>,
        root_transition: PreparedBoundRootTransition<K, V>,
    ) -> ExactFaultOutcome
    where
        K: RegistryFamily<Unit = u32>,
        V: DictionaryValue,
    {
        let outcome = match root.publish_bound_root_transition(&root_transition) {
            Ok(_) => ExactFaultOutcome::Committed,
            Err(actual) => {
                match classify_failed_exact_commit(actual.as_ref(), root_transition.binding()) {
                    FailedExactCommitOutcome::RootAdvanced => ExactFaultOutcome::RootAdvanced,
                    FailedExactCommitOutcome::AuthorityLost => ExactFaultOutcome::AuthorityLost,
                }
            }
        };
        outcome
    }

    /// Get a snapshot of eviction statistics, including the live resident-overlay heap
    /// gauge (`resident_bytes`) folded in from the disk registry — the atomic counters
    /// alone cannot supply it. This is the single point that makes the `eviction_stats()`
    /// trait method report resident bytes for byte, char, and vocab uniformly.
    pub fn stats(&self) -> EvictionStats {
        let mut snapshot = self.stats.snapshot();
        snapshot.resident_bytes = self.resident_estimate_bytes() as u64;
        snapshot
    }

    /// Total resident-overlay heap estimate (byte + char paths) in on-disk-equivalent
    /// bytes, under one registry read-lock. A given trie populates only one key path, so
    /// the sum is that path's estimate (the other is 0); this key-agnostic accessor lets
    /// `stats()` report resident bytes without knowing the trie's key type.
    pub fn resident_estimate_bytes(&self) -> usize {
        let registry = self.disk_registry.read();
        registry.byte_resident_estimate_bytes() + registry.char_resident_estimate_bytes()
    }

    /// Record a synchronous (checkpoint-tail) resident-budget eviction into
    /// `EvictionStats`, mirroring what the async eviction loops do for the pressure
    /// path. Shared by both key-path `force_eviction_*_resident` arities so neither
    /// under-reports `nodes_evicted`/`bytes_freed`.
    fn record_resident_eviction(&self, start: Instant, evicted: (usize, usize)) -> (usize, usize) {
        let (nodes_evicted, bytes_freed) = evicted;
        if nodes_evicted > 0 {
            self.stats.record_eviction(
                nodes_evicted as u64,
                bytes_freed as u64,
                start.elapsed().as_millis() as u64,
            );
        }
        evicted
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
        // The worker polls the `shutdown` flag every 100 ms, so no condvar wake
        // is needed; the join below completes within one poll interval.
        if let Some(handle) = self.eviction_thread.lock().take() {
            Self::join_eviction_thread(handle);
        }

        self.running.store(false, Ordering::SeqCst);
    }

    fn join_eviction_thread(handle: JoinHandle<()>) {
        if Self::should_join_thread(handle.thread().id(), thread::current().id()) {
            let _ = handle.join();
        }
    }

    fn should_join_thread(handle_thread_id: ThreadId, current_thread_id: ThreadId) -> bool {
        handle_thread_id != current_thread_id
    }

    /// Check if the coordinator is running.
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    /// Clear the detached compatibility catalog.
    ///
    /// This is one nonblocking ArcSwap publication. Concurrent callbacks retain
    /// their immutable snapshots; checkpoint-published exact authority is never
    /// observed or modified.
    #[inline]
    pub fn clear_detached_compatibility_catalog(&self) {
        self.detached_compatibility_catalog.store(None);
    }

    /// Deprecated ambiguous name for
    /// [`Self::clear_detached_compatibility_catalog`].
    #[deprecated(
        note = "clears only the detached advisory catalog; use clear_detached_compatibility_catalog"
    )]
    #[inline]
    pub fn invalidate_registry(&self) {
        self.clear_detached_compatibility_catalog();
    }

    /// Deprecated fallible wrapper around the now-infallible detached clear.
    #[deprecated(note = "detached clear is infallible; use clear_detached_compatibility_catalog")]
    #[inline]
    pub fn try_invalidate_registry(&self) -> crate::persistent_artrie::error::Result<()> {
        self.clear_detached_compatibility_catalog();
        Ok(())
    }

    // --- Private methods ---

    fn eviction_loop_driver<F, P>(
        weak: Weak<Self>,
        callback: Arc<F>,
        require_quiescence: bool,
        perform: P,
    ) where
        F: Send + Sync,
        P: Fn(&Self, &F, &EvictionRequest) -> (usize, usize),
    {
        loop {
            let Some(this) = weak.upgrade() else { break };
            if this.shutdown.load(Ordering::Relaxed) {
                break;
            }

            let had_request = if let Some(request) = this.try_pop_request() {
                if !this.check_cooldown(&request) {
                    this.stats.record_skip();
                } else if require_quiescence && !this.wait_for_quiescence() {
                    this.stats.record_quiescence_timeout();
                } else {
                    let start = Instant::now();
                    let (nodes_evicted, bytes_freed) = perform(&this, &callback, &request);
                    if nodes_evicted > 0 {
                        this.stats.record_eviction(
                            nodes_evicted as u64,
                            bytes_freed as u64,
                            start.elapsed().as_millis() as u64,
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

            drop(this);
            if !had_request {
                thread::sleep(Duration::from_millis(100));
            }
        }
    }

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
        Self::eviction_loop_driver(weak, callback, true, |this, callback, request| {
            this.perform_eviction(callback, request)
        });
    }

    /// Main eviction loop for char-level tries.
    ///
    /// See [`eviction_loop`](Self::eviction_loop) for why this is driven through
    /// a `Weak<Self>` + 100 ms poll rather than a strong `Arc` + condvar wait.
    fn eviction_loop_char<F>(weak: Weak<Self>, callback: Arc<F>)
    where
        F: Fn(Vec<(u64, Vec<char>, SwizzledPtr)>) -> (usize, usize) + Send + Sync,
    {
        Self::eviction_loop_driver(weak, callback, false, |this, callback, request| {
            this.perform_eviction_char(callback, request)
        });
    }

    #[cfg(any(test, feature = "bench-internals"))]
    fn eviction_loop_compact<F>(weak: Weak<Self>, callback: Arc<F>)
    where
        F: Fn(CompactEvictionBatch<u8>) -> (usize, usize) + Send + Sync,
    {
        Self::eviction_loop_driver(weak, callback, true, |this, callback, request| {
            this.perform_eviction_compact(callback, request)
        });
    }

    #[cfg(any(test, feature = "bench-internals"))]
    fn eviction_loop_compact_char<F>(weak: Weak<Self>, callback: Arc<F>)
    where
        F: Fn(CompactEvictionBatch<u32>) -> (usize, usize) + Send + Sync,
    {
        Self::eviction_loop_driver(weak, callback, false, |this, callback, request| {
            this.perform_eviction_compact_char(callback, request)
        });
    }

    fn eviction_loop_root_compact<F>(weak: Weak<Self>, callback: Arc<F>, require_quiescence: bool)
    where
        F: Fn(usize) -> (usize, usize) + Send + Sync,
    {
        Self::eviction_loop_driver(
            weak,
            callback,
            require_quiescence,
            |this, callback, request| {
                let Some(max_count) = this.request_max_count(request) else {
                    return (0, 0);
                };
                callback(max_count)
            },
        );
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

    fn request_max_count(&self, request: &EvictionRequest) -> Option<usize> {
        self.config
            .batch_size
            .checked_mul(request.urgency.batch_multiplier())
    }

    /// Perform eviction for byte-level tries.
    fn perform_eviction<F>(&self, callback: &F, request: &EvictionRequest) -> (usize, usize)
    where
        F: Fn(Vec<(u64, Vec<u8>, SwizzledPtr)>) -> (usize, usize),
    {
        let Some(batch_size) = self.request_max_count(request) else {
            log::error!("overlay eviction: byte request batch-size overflow");
            return (0, 0);
        };

        let Some(prepared) = self.try_prepare_legacy_byte_callback(usize::MAX, batch_size, 0)
        else {
            return (0, 0);
        };
        let PreparedLegacyCallback {
            entries, _catalog, ..
        } = prepared;
        callback(entries)
    }

    #[cfg(any(test, feature = "bench-internals"))]
    fn perform_eviction_compact<F>(&self, callback: &F, request: &EvictionRequest) -> (usize, usize)
    where
        F: Fn(CompactEvictionBatch<u8>) -> (usize, usize),
    {
        let Some(batch_size) = self.request_max_count(request) else {
            log::error!("overlay eviction: compact byte request batch-size overflow");
            return (0, 0);
        };
        let disk_registry = self.disk_registry.read();
        let batch = disk_registry.select_compact_for_eviction(
            usize::MAX,
            &self.lru_registry,
            self.config.min_eviction_depth,
            batch_size,
            0,
        );
        if batch.candidates.is_empty() {
            return (0, 0);
        }
        drop(disk_registry);
        callback(batch)
    }

    /// Perform eviction for char-level tries.
    fn perform_eviction_char<F>(&self, callback: &F, request: &EvictionRequest) -> (usize, usize)
    where
        F: Fn(Vec<(u64, Vec<char>, SwizzledPtr)>) -> (usize, usize),
    {
        let Some(batch_size) = self.request_max_count(request) else {
            log::error!("overlay eviction: char request batch-size overflow");
            return (0, 0);
        };

        let Some(prepared) = self.try_prepare_legacy_char_callback(usize::MAX, batch_size, 0)
        else {
            return (0, 0);
        };
        let PreparedLegacyCallback {
            entries, _catalog, ..
        } = prepared;
        callback(entries)
    }

    #[cfg(any(test, feature = "bench-internals"))]
    fn perform_eviction_compact_char<F>(
        &self,
        callback: &F,
        request: &EvictionRequest,
    ) -> (usize, usize)
    where
        F: Fn(CompactEvictionBatch<u32>) -> (usize, usize),
    {
        let Some(batch_size) = self.request_max_count(request) else {
            log::error!("overlay eviction: compact char request batch-size overflow");
            return (0, 0);
        };
        let disk_registry = self.disk_registry.read();
        let batch = disk_registry.select_compact_char_for_eviction(
            usize::MAX,
            &self.lru_registry,
            self.config.min_eviction_depth,
            batch_size,
            0,
        );
        if batch.candidates.is_empty() {
            return (0, 0);
        }
        drop(disk_registry);
        callback(batch)
    }
}

impl Drop for EvictionCoordinator {
    fn drop(&mut self) {
        // Route through `shutdown()` so teardown is complete (it also stops the
        // memory-pressure monitor) and identical on every drop path. The worker
        // holds only a `Weak<Self>`, so this `Drop` is reachable as soon as the
        // owning trie releases its `Arc`. If the last strong reference is the
        // worker's per-iteration `Weak::upgrade()`, this `Drop` runs on the
        // eviction thread itself; `shutdown()` must then detach that handle
        // instead of joining itself.
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    #![allow(deprecated)]

    use super::*;
    use crate::persistent_artrie::core::eviction::lru_tracker::hash_char_path;
    use crate::persistent_artrie::core::eviction::RegistryPathId;
    use crate::persistent_artrie::core::key_encoding::{ByteKey, CharKey};
    use crate::persistent_artrie::core::overlay::{EvictionBinding, OverlayNode};
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc;

    fn prepare_test_byte_root_binding<V: DictionaryValue>(
        captured: &RootRevision<ByteKey, V>,
        registry: &DiskLocationRegistry,
    ) -> PreparedRootBinding<ByteKey, V> {
        let (resident_nodes, resident_serialized_bytes) = (
            registry.byte_resident_len(),
            registry.byte_resident_serialized_bytes(),
        );
        let catalog = Arc::new(
            PublishedRegistryCatalog::try_from_builder(registry)
                .expect("build test publication catalog"),
        );
        AtomicNodePtr::prepare_checkpoint_binding(
            captured,
            catalog,
            resident_nodes,
            resident_serialized_bytes,
        )
    }

    fn publish_test_registry(
        coordinator: &Arc<EvictionCoordinator>,
        registry: DiskLocationRegistry,
    ) -> AtomicNodePtr<ByteKey, u64> {
        let root = AtomicNodePtr::new(Arc::new(OverlayNode::new()));
        publish_test_registry_on_root(coordinator, &root, registry);
        root
    }

    fn publish_test_registry_on_root(
        coordinator: &Arc<EvictionCoordinator>,
        root: &AtomicNodePtr<ByteKey, u64>,
        registry: DiskLocationRegistry,
    ) {
        let captured = root.load_revision().expect("test root revision");
        let prepared = PreparedRegistryPublication::try_new(
            Arc::clone(coordinator),
            &captured,
            registry,
            Vec::new(),
        )
        .expect("prepare test registry publication");
        assert_eq!(
            prepared.publish(coordinator, root),
            RegistryPublicationOutcome::Published
        );
    }

    fn publish_test_char_registry(
        coordinator: &Arc<EvictionCoordinator>,
        registry: DiskLocationRegistry,
    ) -> AtomicNodePtr<CharKey, u64> {
        let root = AtomicNodePtr::new(Arc::new(OverlayNode::new()));
        let captured = root.load_revision().expect("test char root revision");
        let prepared = PreparedRegistryPublication::try_new(
            Arc::clone(coordinator),
            &captured,
            registry,
            Vec::new(),
        )
        .expect("prepare test char registry publication");
        assert_eq!(
            prepared.publish(coordinator, &root),
            RegistryPublicationOutcome::Published
        );
        root
    }

    fn bind_test_byte_registry_at_max(
        registry: &DiskLocationRegistry,
    ) -> AtomicNodePtr<ByteKey, u64> {
        let root = AtomicNodePtr::new(Arc::new(OverlayNode::new()));
        let captured = root.load_revision().expect("unbound byte test root");
        let (nodes, bytes) = (
            registry.byte_resident_len(),
            registry.byte_resident_serialized_bytes(),
        );
        let catalog = Arc::new(
            PublishedRegistryCatalog::try_from_builder_at_ordinals(registry, u32::MAX, 0)
                .expect("build maximum-ordinal byte catalog"),
        );
        let prepared = AtomicNodePtr::prepare_checkpoint_binding_at_ordinal(
            &captured,
            catalog,
            u32::MAX,
            nodes,
            bytes,
        );
        assert!(root.publish_checkpoint_binding(&prepared).is_ok());
        root
    }

    fn bind_test_char_registry_at_max(
        registry: &DiskLocationRegistry,
    ) -> AtomicNodePtr<CharKey, u64> {
        let root = AtomicNodePtr::new(Arc::new(OverlayNode::new()));
        let captured = root.load_revision().expect("unbound char test root");
        let (nodes, bytes) = (
            registry.char_resident_len(),
            registry.char_resident_serialized_bytes(),
        );
        let catalog = Arc::new(
            PublishedRegistryCatalog::try_from_builder_at_ordinals(registry, 0, u32::MAX)
                .expect("build maximum-ordinal char catalog"),
        );
        let prepared = AtomicNodePtr::prepare_checkpoint_binding_at_ordinal(
            &captured,
            catalog,
            u32::MAX,
            nodes,
            bytes,
        );
        assert!(root.publish_checkpoint_binding(&prepared).is_ok());
        root
    }

    fn invalidate_bound_test_root_semantically<K, V>(
        _coordinator: &EvictionCoordinator,
        root: &AtomicNodePtr<K, V>,
    ) where
        K: KeyEncoding,
        V: DictionaryValue,
    {
        let captured = root.load_revision().expect("captured bound test root");
        assert!(root
            .compare_exchange_revision_counted(&captured, Arc::clone(captured.node()), 0)
            .is_ok());
        assert!(root
            .load_revision()
            .expect("semantic test root")
            .eviction_binding()
            .is_none());
    }

    fn prepare_test_byte_eviction_candidate(
        coordinator: &EvictionCoordinator,
        captured: &RootRevision<ByteKey, u64>,
        batch: &CompactEvictionBatch<u8>,
        successful: &[usize],
        new_root: Arc<OverlayNode<ByteKey, u64>>,
    ) -> PreparedBoundRootTransition<ByteKey, u64> {
        let mut successful = successful.to_vec();
        let packed = coordinator
            .prepare_byte_eviction_commit(captured, batch, &mut successful)
            .expect("prepare exact byte eviction");
        AtomicNodePtr::prepare_exact_root_transition(captured, new_root, packed)
            .expect("prepare exact byte root transition")
    }

    fn prepare_test_char_eviction_candidate(
        coordinator: &EvictionCoordinator,
        captured: &RootRevision<CharKey, u64>,
        batch: &CompactEvictionBatch<u32>,
        successful: &[usize],
        new_root: Arc<OverlayNode<CharKey, u64>>,
    ) -> PreparedBoundRootTransition<CharKey, u64> {
        let mut successful = successful.to_vec();
        let packed = coordinator
            .prepare_char_eviction_commit(captured, batch, &mut successful)
            .expect("prepare exact char eviction");
        AtomicNodePtr::prepare_exact_root_transition(captured, new_root, packed)
            .expect("prepare exact char root transition")
    }

    fn prepare_test_byte_fault_candidate(
        coordinator: &EvictionCoordinator,
        captured: &RootRevision<ByteKey, u64>,
        path: &[u8],
        pointer: &SwizzledPtr,
        new_root: Arc<OverlayNode<ByteKey, u64>>,
    ) -> PreparedBoundRootTransition<ByteKey, u64> {
        let packed = coordinator
            .prepare_byte_fault_commit(captured, path, pointer)
            .expect("prepare exact byte fault");
        AtomicNodePtr::prepare_exact_root_transition(captured, new_root, packed)
            .expect("prepare exact byte fault root transition")
    }

    fn prepare_test_char_fault_candidate(
        coordinator: &EvictionCoordinator,
        captured: &RootRevision<CharKey, u64>,
        path: &[u32],
        pointer: &SwizzledPtr,
        new_root: Arc<OverlayNode<CharKey, u64>>,
    ) -> PreparedBoundRootTransition<CharKey, u64> {
        let packed = coordinator
            .prepare_char_fault_commit(captured, path, pointer)
            .expect("prepare exact char fault");
        AtomicNodePtr::prepare_exact_root_transition(captured, new_root, packed)
            .expect("prepare exact char fault root transition")
    }

    fn test_root_resident_totals<K, V>(root: &AtomicNodePtr<K, V>) -> (usize, usize)
    where
        K: RegistryFamily,
        V: DictionaryValue,
    {
        root.load_revision()
            .expect("published test root")
            .help_eviction_revision()
            .expect("settled test residency")
            .resident_totals()
    }

    fn select_test_byte_root_batch(
        root: &AtomicNodePtr<ByteKey, u64>,
        coordinator: &EvictionCoordinator,
    ) -> CompactEvictionBatch<u8> {
        let revision = root.load_revision().expect("published byte test root");
        let eviction = revision
            .help_eviction_revision()
            .expect("settled byte test residency");
        eviction
            .catalog()
            .try_byte_selection_snapshot(eviction.ordinal())
            .expect("coherent byte test selection")
            .select_compact(usize::MAX, &coordinator.lru_registry, 0, usize::MAX, 0)
    }

    fn select_test_char_root_batch(
        root: &AtomicNodePtr<CharKey, u64>,
        coordinator: &EvictionCoordinator,
    ) -> CompactEvictionBatch<u32> {
        let revision = root.load_revision().expect("published char test root");
        let eviction = revision
            .help_eviction_revision()
            .expect("settled char test residency");
        eviction
            .catalog()
            .try_char_selection_snapshot(eviction.ordinal())
            .expect("coherent char test selection")
            .select_compact(usize::MAX, &coordinator.lru_registry, 0, usize::MAX, 0)
    }

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
    fn test_coordinator_does_not_join_current_thread() {
        let current = thread::current().id();
        assert!(
            !EvictionCoordinator::should_join_thread(current, current),
            "shutdown must detach, not join, when Drop runs on the worker thread"
        );

        let handle = thread::spawn(|| thread::current().id());
        let worker_id = handle.join().expect("worker id");
        assert!(
            EvictionCoordinator::should_join_thread(worker_id, current),
            "shutdown from an owner thread should still join a different worker thread"
        );
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
    fn compact_byte_worker_delivers_dense_bounded_batch() {
        let epoch_manager = Arc::new(EpochManager::new());
        let config = EvictionConfig {
            batch_size: 4,
            min_eviction_depth: 0,
            enable_memory_pressure_monitor: false,
            ..EvictionConfig::default()
        };
        let coordinator = EvictionCoordinator::new(config, epoch_manager);
        let mut registry = DiskLocationRegistry::new();
        for offset in 0..10u32 {
            let path = vec![b'a' + offset as u8];
            registry.register(
                path,
                SwizzledPtr::on_disk(1, offset, NodeType::Node4),
                10,
                1,
                NodeType::Node4,
            );
        }
        let _bound_root = publish_test_registry(&coordinator, registry);

        let (sender, receiver) = mpsc::sync_channel(1);
        coordinator
            .start_compact(move |batch| {
                let count = batch.candidates.len();
                let bytes = batch
                    .candidates
                    .iter()
                    .map(|candidate| candidate.size_bytes)
                    .sum();
                sender
                    .send((count, batch.topology.len()))
                    .expect("compact byte test receiver remains live");
                (count, bytes)
            })
            .expect("start compact byte coordinator");
        coordinator.request_eviction(EvictionUrgency::Moderate);
        let delivered = receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("compact byte worker delivered a batch");
        coordinator.shutdown();

        assert_eq!(delivered.0, 4);
        assert_eq!(delivered.1, 10);
    }

    #[test]
    fn compact_char_worker_delivers_dense_bounded_batch() {
        let epoch_manager = Arc::new(EpochManager::new());
        let config = EvictionConfig {
            batch_size: 3,
            min_eviction_depth: 0,
            enable_memory_pressure_monitor: false,
            ..EvictionConfig::default()
        };
        let coordinator = EvictionCoordinator::new(config, epoch_manager);
        let mut registry = DiskLocationRegistry::new();
        for offset in 0..8u32 {
            let unit = char::from_u32(0x03B1 + offset).expect("valid Greek scalar");
            registry.register_char(
                vec![unit],
                SwizzledPtr::on_disk(1, offset, NodeType::CharNode4),
                12,
                1,
                NodeType::CharNode4,
            );
        }
        let _bound_root = publish_test_registry(&coordinator, registry);

        let (sender, receiver) = mpsc::sync_channel(1);
        coordinator
            .start_compact_char(move |batch| {
                let count = batch.candidates.len();
                let bytes = batch
                    .candidates
                    .iter()
                    .map(|candidate| candidate.size_bytes)
                    .sum();
                sender
                    .send((count, batch.topology.len()))
                    .expect("compact char test receiver remains live");
                (count, bytes)
            })
            .expect("start compact char coordinator");
        coordinator.request_eviction(EvictionUrgency::Moderate);
        let delivered = receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("compact char worker delivered a batch");
        coordinator.shutdown();

        assert_eq!(delivered.0, 3);
        assert_eq!(delivered.1, 8);
    }

    #[test]
    fn panicking_callback_resets_running_and_allows_clean_restart() {
        let epoch_manager = Arc::new(EpochManager::new());
        let config = EvictionConfig {
            batch_size: 1,
            min_eviction_depth: 0,
            enable_memory_pressure_monitor: false,
            ..EvictionConfig::default()
        };
        let coordinator = EvictionCoordinator::new(config, epoch_manager);
        let mut registry = DiskLocationRegistry::new();
        registry.register(
            b"panic-target".to_vec(),
            SwizzledPtr::on_disk(1, 1, NodeType::Node4),
            10,
            1,
            NodeType::Node4,
        );
        let _bound_root = publish_test_registry(&coordinator, registry);

        let (entered_sender, entered_receiver) = mpsc::sync_channel(1);
        coordinator
            .start_compact(move |_| -> (usize, usize) {
                entered_sender
                    .send(())
                    .expect("panic test receiver remains live");
                panic!("intentional callback panic");
            })
            .expect("start panicking coordinator");
        coordinator.request_eviction(EvictionUrgency::Moderate);
        entered_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("panicking callback was entered");

        let deadline = Instant::now() + Duration::from_secs(2);
        while coordinator.is_running() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        assert!(
            !coordinator.is_running(),
            "worker exit guard must reset running after callback unwind"
        );

        let (restart_sender, restart_receiver) = mpsc::sync_channel(1);
        coordinator
            .start_compact(move |batch| {
                let count = batch.candidates.len();
                restart_sender
                    .send(count)
                    .expect("restart test receiver remains live");
                (count, 10)
            })
            .expect("restart after panicking callback");
        coordinator.request_eviction(EvictionUrgency::Moderate);
        assert_eq!(
            restart_receiver
                .recv_timeout(Duration::from_secs(2))
                .expect("restarted worker delivered a batch"),
            1
        );
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

        coordinator
            .try_update_disk_registry(registry)
            .expect("finalize and publish coordinator test registry");

        // A direct install populates only the detached compatibility catalog;
        // exact checkpoint authority remains empty.
        assert_eq!(coordinator.disk_registry_len(), 0);
        assert_eq!(coordinator.force_eviction(1024), (0, 0));
        assert_eq!(
            coordinator.force_eviction_bytes(1024, |entries| {
                assert_eq!(entries[0].1, b"test");
                (entries.len(), 256)
            }),
            (1, 256)
        );
    }

    #[test]
    fn failed_coordinator_update_preserves_the_published_registry() {
        let epoch_manager = Arc::new(EpochManager::new());
        let coordinator = EvictionCoordinator::new(EvictionConfig::default(), epoch_manager);
        let mut published = DiskLocationRegistry::new();
        published.register(
            b"published".to_vec(),
            SwizzledPtr::on_disk(1, 100, NodeType::Node16),
            256,
            1,
            NodeType::Node16,
        );
        coordinator
            .try_update_disk_registry(published)
            .expect("publish initial finalized registry");

        let mut unfinished = DiskLocationRegistry::new();
        let unfinished_root = unfinished
            .try_reserve_byte_path(RegistryPathId::ROOT, b"unfinished")
            .expect("unfinished topology root");
        let _open_subtree = unfinished
            .try_begin_byte_builder_subtree(unfinished_root)
            .expect("open builder subtree");

        assert!(coordinator.try_update_disk_registry(unfinished).is_err());
        assert_eq!(coordinator.disk_registry_len(), 0);
        assert_eq!(coordinator.force_eviction(usize::MAX), (0, 0));
        assert_eq!(
            coordinator.force_eviction_bytes(usize::MAX, |entries| {
                assert_eq!(entries[0].1, b"published");
                (entries.len(), 256)
            }),
            (1, 256)
        );
    }

    #[test]
    #[allow(deprecated)]
    fn legacy_coordinator_update_rejection_is_total_and_preserves_the_catalog() {
        let coordinator =
            EvictionCoordinator::new(EvictionConfig::default(), Arc::new(EpochManager::new()));
        let mut published = DiskLocationRegistry::new();
        published.register(
            b"legacy-published".to_vec(),
            SwizzledPtr::on_disk(1, 220, NodeType::Node16),
            320,
            1,
            NodeType::Node16,
        );
        coordinator
            .try_install_detached_compatibility_catalog(published)
            .expect("install the catalog preserved across rejection");

        let mut unfinished = DiskLocationRegistry::new();
        let unfinished_root = unfinished
            .try_reserve_byte_path(RegistryPathId::ROOT, b"unfinished-legacy")
            .expect("reserve unfinished compatibility path");
        let _open_subtree = unfinished
            .try_begin_byte_builder_subtree(unfinished_root)
            .expect("leave one builder subtree structurally unfinished");

        // This direct call is the regression oracle: the former implementation
        // delegated to the panicking installer and unwound here.
        coordinator.update_disk_registry(unfinished);

        assert_eq!(
            coordinator.force_eviction_bytes(usize::MAX, |entries| {
                assert_eq!(entries[0].1, b"legacy-published");
                (entries.len(), 320)
            }),
            (1, 320),
            "rejection must preserve the prior immutable compatibility catalog"
        );
    }

    #[test]
    fn detached_catalog_replacement_is_independent_of_exact_registry_state() {
        let coordinator =
            EvictionCoordinator::new(EvictionConfig::default(), Arc::new(EpochManager::new()));
        let mut initial = DiskLocationRegistry::new();
        initial.register(
            b"semantic-initial".to_vec(),
            SwizzledPtr::on_disk(1, 211, NodeType::Node4),
            81,
            1,
            NodeType::Node4,
        );
        coordinator
            .try_update_disk_registry(initial)
            .expect("install initial detached registry");
        assert_eq!(
            coordinator.force_eviction_bytes(usize::MAX, |entries| {
                assert_eq!(entries[0].1, b"semantic-initial");
                (entries.len(), 81)
            }),
            (1, 81)
        );

        let mut replacement = DiskLocationRegistry::new();
        replacement.register(
            b"semantic-replacement".to_vec(),
            SwizzledPtr::on_disk(1, 212, NodeType::Node4),
            82,
            1,
            NodeType::Node4,
        );
        coordinator
            .try_update_disk_registry(replacement)
            .expect("replace detached catalog without consulting exact state");
        assert_eq!(
            coordinator.force_eviction_bytes(usize::MAX, |entries| {
                assert_eq!(entries[0].1, b"semantic-replacement");
                (entries.len(), 82)
            }),
            (1, 82)
        );
    }

    #[test]
    fn detached_callback_retains_snapshot_during_concurrent_replacement() {
        let coordinator =
            EvictionCoordinator::new(EvictionConfig::default(), Arc::new(EpochManager::new()));
        let mut initial = DiskLocationRegistry::new();
        initial.register(
            b"legacy-initial".to_vec(),
            SwizzledPtr::on_disk(1, 214, NodeType::Node4),
            84,
            1,
            NodeType::Node4,
        );
        coordinator
            .try_update_disk_registry(initial)
            .expect("install initial detached registry");

        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let callback_coordinator = Arc::clone(&coordinator);
        let callback = thread::spawn(move || {
            callback_coordinator.force_eviction_bytes(usize::MAX, |entries| {
                entered_tx
                    .send(entries[0].1.clone())
                    .expect("announce detached callback snapshot");
                release_rx.recv().expect("release legacy callback");
                (entries.len(), 84)
            })
        });
        assert_eq!(
            entered_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("legacy callback entered"),
            b"legacy-initial"
        );

        let mut replacement = DiskLocationRegistry::new();
        replacement.register(
            b"legacy-replacement".to_vec(),
            SwizzledPtr::on_disk(1, 215, NodeType::Node4),
            85,
            1,
            NodeType::Node4,
        );
        coordinator
            .try_update_disk_registry(replacement)
            .expect("replacement does not wait for a retained detached snapshot");

        release_tx.send(()).expect("release legacy callback");
        assert_eq!(callback.join().expect("legacy callback thread"), (1, 84));
        assert_eq!(
            coordinator.force_eviction_bytes(usize::MAX, |entries| {
                assert_eq!(entries[0].1, b"legacy-replacement");
                (entries.len(), 85)
            }),
            (1, 85)
        );
    }

    #[test]
    fn prepared_publication_binds_the_exact_root_registry_and_stamps() {
        let coordinator =
            EvictionCoordinator::new(EvictionConfig::default(), Arc::new(EpochManager::new()));
        let node = Arc::new(OverlayNode::<ByteKey, u64>::new());
        let root = AtomicNodePtr::new(Arc::clone(&node));
        let captured = root.load_revision().expect("captured root revision");
        let disk_ptr = SwizzledPtr::on_disk(1, 70, NodeType::Node4);
        let mut registry = DiskLocationRegistry::new();
        registry.register(
            b"published".to_vec(),
            disk_ptr.clone(),
            80,
            1,
            NodeType::Node4,
        );
        let prepared = PreparedRegistryPublication::try_new(
            Arc::clone(&coordinator),
            &captured,
            registry,
            vec![DeferredDurableStamp::new(
                Arc::clone(&node),
                disk_ptr.to_raw(),
            )],
        )
        .expect("prepare exact registry publication");

        assert_eq!(
            prepared.publish(&coordinator, &root),
            RegistryPublicationOutcome::Published
        );
        assert_eq!(node.durable_stamp(), disk_ptr.to_raw());
        let published_root = root.load_revision().expect("published root revision");
        let published_registry = coordinator.disk_registry.read();
        assert!(published_registry.is_valid());
        assert!(published_root
            .eviction_binding()
            .is_some_and(|binding| binding.same_publication(&published_registry.binding())));
        assert!(published_registry
            .get_owned(LruRegistry::path_hash(b"published"))
            .is_some());
    }

    #[test]
    fn byte_exact_eviction_and_fault_reject_authority_loss_before_root_cas() {
        let coordinator =
            EvictionCoordinator::new(EvictionConfig::default(), Arc::new(EpochManager::new()));
        let disk_ptr = SwizzledPtr::on_disk(1, 201, NodeType::Node4);
        let mut registry = DiskLocationRegistry::new();
        registry.register(
            b"byte-race".to_vec(),
            disk_ptr.clone(),
            71,
            1,
            NodeType::Node4,
        );
        let root = publish_test_registry(&coordinator, registry);

        let batch = coordinator
            .disk_registry
            .read()
            .select_compact_for_eviction(usize::MAX, &coordinator.lru_registry, 0, usize::MAX, 0);
        let captured = root.load_revision().expect("captured byte root");
        let transition = prepare_test_byte_eviction_candidate(
            &coordinator,
            &captured,
            &batch,
            &[0],
            Arc::new(OverlayNode::new()),
        );
        let resident_before = coordinator.resident_estimate_bytes();

        invalidate_bound_test_root_semantically(&coordinator, &root);
        assert_eq!(
            coordinator.commit_byte_eviction_transaction(&root, transition),
            ExactEvictionOutcome::AuthorityLost
        );
        assert!(!captured.same_revision(&root.load_revision().expect("advanced byte root")));
        assert_eq!(coordinator.resident_estimate_bytes(), resident_before);

        let mut replacement = DiskLocationRegistry::new();
        let path_id = replacement
            .try_reserve_byte_path(RegistryPathId::ROOT, b"byte-fault")
            .expect("reserve byte fault path");
        replacement
            .register_nonresident_byte_path(
                path_id,
                disk_ptr.clone(),
                73,
                b"byte-fault".len(),
                NodeType::Node4,
            )
            .expect("register nonresident byte fault record");
        let captured = root.load_revision().expect("byte root before republish");
        let replacement = PreparedRegistryPublication::try_new(
            Arc::clone(&coordinator),
            &captured,
            replacement,
            Vec::new(),
        )
        .expect("prepare nonresident byte registry");
        assert_eq!(
            replacement.publish(&coordinator, &root),
            RegistryPublicationOutcome::Published
        );
        let captured = root.load_revision().expect("captured byte fault root");
        let transition = prepare_test_byte_fault_candidate(
            &coordinator,
            &captured,
            b"byte-fault",
            &disk_ptr,
            Arc::new(OverlayNode::new()),
        );
        invalidate_bound_test_root_semantically(&coordinator, &root);
        assert_eq!(
            coordinator.commit_byte_fault_transaction(&root, transition),
            ExactFaultOutcome::AuthorityLost
        );
        assert!(!captured.same_revision(&root.load_revision().expect("advanced byte fault root")));
        assert_eq!(coordinator.resident_estimate_bytes(), 0);
    }

    #[test]
    fn char_exact_eviction_and_fault_reject_authority_loss_before_root_cas() {
        let coordinator =
            EvictionCoordinator::new(EvictionConfig::default(), Arc::new(EpochManager::new()));
        let disk_ptr = SwizzledPtr::on_disk(1, 202, NodeType::CharNode4);
        let mut registry = DiskLocationRegistry::new();
        registry.register_char(vec!['界'], disk_ptr.clone(), 79, 1, NodeType::CharNode4);
        let root = publish_test_char_registry(&coordinator, registry);

        let batch = coordinator
            .disk_registry
            .read()
            .select_compact_char_for_eviction(
                usize::MAX,
                &coordinator.lru_registry,
                0,
                usize::MAX,
                0,
            );
        let captured = root.load_revision().expect("captured char root");
        let transition = prepare_test_char_eviction_candidate(
            &coordinator,
            &captured,
            &batch,
            &[0],
            Arc::new(OverlayNode::new()),
        );
        let resident_before = coordinator.resident_estimate_bytes();
        invalidate_bound_test_root_semantically(&coordinator, &root);
        assert_eq!(
            coordinator.commit_char_eviction_transaction(&root, transition),
            ExactEvictionOutcome::AuthorityLost
        );
        assert!(!captured.same_revision(&root.load_revision().expect("advanced char root")));
        assert_eq!(coordinator.resident_estimate_bytes(), resident_before);

        let mut replacement = DiskLocationRegistry::new();
        let path_id = replacement
            .try_reserve_char_units(RegistryPathId::ROOT, &['障' as u32])
            .expect("reserve char fault path");
        replacement
            .register_nonresident_char_path(path_id, disk_ptr.clone(), 83, 1, NodeType::CharNode4)
            .expect("register nonresident char fault record");
        let captured = root.load_revision().expect("char root before republish");
        let replacement = PreparedRegistryPublication::try_new(
            Arc::clone(&coordinator),
            &captured,
            replacement,
            Vec::new(),
        )
        .expect("prepare nonresident char registry");
        assert_eq!(
            replacement.publish(&coordinator, &root),
            RegistryPublicationOutcome::Published
        );
        let captured = root.load_revision().expect("captured char fault root");
        let transition = prepare_test_char_fault_candidate(
            &coordinator,
            &captured,
            &['障' as u32],
            &disk_ptr,
            Arc::new(OverlayNode::new()),
        );
        invalidate_bound_test_root_semantically(&coordinator, &root);
        assert_eq!(
            coordinator.commit_char_fault_transaction(&root, transition),
            ExactFaultOutcome::AuthorityLost
        );
        assert!(!captured.same_revision(&root.load_revision().expect("advanced char fault root")));
        assert_eq!(coordinator.resident_estimate_bytes(), 0);
    }

    #[test]
    fn byte_root_advance_commits_only_the_exact_eviction_and_fault_winner() {
        let coordinator =
            EvictionCoordinator::new(EvictionConfig::default(), Arc::new(EpochManager::new()));
        let first_ptr = SwizzledPtr::on_disk(11, 211, NodeType::Node4);
        let second_ptr = SwizzledPtr::on_disk(11, 223, NodeType::Node4);
        let mut registry = DiskLocationRegistry::new();
        registry.register(b"first".to_vec(), first_ptr, 31, 1, NodeType::Node4);
        registry.register(b"second".to_vec(), second_ptr, 37, 1, NodeType::Node4);
        let root = publish_test_registry(&coordinator, registry);
        coordinator.lru_registry.touch(b"first");
        coordinator.lru_registry.touch(b"second");

        let batch = coordinator
            .disk_registry
            .read()
            .select_compact_for_eviction(usize::MAX, &coordinator.lru_registry, 0, usize::MAX, 0);
        let first_index = batch
            .candidates
            .iter()
            .position(|candidate| {
                batch.materialize_path(candidate.path_id).as_deref() == Some(b"first")
            })
            .expect("selected first byte record");
        let second_index = batch
            .candidates
            .iter()
            .position(|candidate| {
                batch.materialize_path(candidate.path_id).as_deref() == Some(b"second")
            })
            .expect("selected second byte record");
        let captured = root.load_revision().expect("capture byte eviction root");
        let first_root = Arc::new(OverlayNode::new());
        let second_root = Arc::new(OverlayNode::new().as_final());
        let first_transition = prepare_test_byte_eviction_candidate(
            &coordinator,
            &captured,
            &batch,
            &[first_index],
            Arc::clone(&first_root),
        );
        let second_transition = prepare_test_byte_eviction_candidate(
            &coordinator,
            &captured,
            &batch,
            &[second_index],
            second_root,
        );

        assert!(matches!(
            coordinator.commit_byte_eviction_transaction(&root, first_transition),
            ExactEvictionOutcome::Committed(1, 31)
        ));
        assert_eq!(test_root_resident_totals(&root), (1, 37));
        assert_eq!(coordinator.lru_registry.len(), 1);
        let after_winner = root
            .load_revision()
            .expect("byte root after eviction winner");
        assert!(Arc::ptr_eq(after_winner.node(), &first_root));

        assert_eq!(
            coordinator.commit_byte_eviction_transaction(&root, second_transition),
            ExactEvictionOutcome::RootAdvanced
        );
        assert_eq!(test_root_resident_totals(&root), (1, 37));
        assert_eq!(coordinator.lru_registry.len(), 1);
        assert!(after_winner.same_revision(&root.load_revision().expect("unchanged byte root")));

        let fault_first_ptr = SwizzledPtr::on_disk(12, 227, NodeType::Node4);
        let fault_second_ptr = SwizzledPtr::on_disk(12, 229, NodeType::Node4);
        let mut fault_registry = DiskLocationRegistry::new();
        let fault_first_path = fault_registry
            .try_reserve_byte_path(RegistryPathId::ROOT, b"fault-first")
            .expect("reserve first byte fault path");
        fault_registry
            .register_nonresident_byte_path(
                fault_first_path,
                fault_first_ptr.clone(),
                41,
                b"fault-first".len(),
                NodeType::Node4,
            )
            .expect("register first byte fault record");
        let fault_second_path = fault_registry
            .try_reserve_byte_path(RegistryPathId::ROOT, b"fault-second")
            .expect("reserve second byte fault path");
        fault_registry
            .register_nonresident_byte_path(
                fault_second_path,
                fault_second_ptr.clone(),
                43,
                b"fault-second".len(),
                NodeType::Node4,
            )
            .expect("register second byte fault record");
        let captured = root
            .load_revision()
            .expect("byte root before fault registry");
        let publication = PreparedRegistryPublication::try_new(
            Arc::clone(&coordinator),
            &captured,
            fault_registry,
            Vec::new(),
        )
        .expect("prepare byte fault registry");
        assert_eq!(
            publication.publish(&coordinator, &root),
            RegistryPublicationOutcome::Published
        );
        let captured = root.load_revision().expect("capture competing byte faults");
        let first_fault_root = Arc::new(OverlayNode::new());
        first_fault_root.set_durable_stamp(fault_first_ptr.to_raw());
        let second_fault_root = Arc::new(OverlayNode::new());
        second_fault_root.set_durable_stamp(fault_second_ptr.to_raw());
        let first_fault_transition = prepare_test_byte_fault_candidate(
            &coordinator,
            &captured,
            b"fault-first",
            &fault_first_ptr,
            Arc::clone(&first_fault_root),
        );
        let second_fault_transition = prepare_test_byte_fault_candidate(
            &coordinator,
            &captured,
            b"fault-second",
            &fault_second_ptr,
            second_fault_root,
        );

        assert_eq!(
            coordinator.commit_byte_fault_transaction(&root, first_fault_transition),
            ExactFaultOutcome::Committed
        );
        assert_eq!(test_root_resident_totals(&root), (1, 41));
        let after_fault_winner = root.load_revision().expect("byte fault winner root");
        assert!(Arc::ptr_eq(after_fault_winner.node(), &first_fault_root));
        assert_eq!(
            after_fault_winner.node().durable_stamp(),
            fault_first_ptr.to_raw()
        );

        assert_eq!(
            coordinator.commit_byte_fault_transaction(&root, second_fault_transition),
            ExactFaultOutcome::RootAdvanced
        );
        assert_eq!(test_root_resident_totals(&root), (1, 41));
        assert!(after_fault_winner
            .same_revision(&root.load_revision().expect("unchanged byte fault root")));
        let reevictable = select_test_byte_root_batch(&root, &coordinator);
        assert_eq!(reevictable.candidates.len(), 1);
        assert_eq!(
            reevictable.candidates[0].disk_ptr.to_raw(),
            fault_first_ptr.to_raw()
        );
    }

    #[test]
    fn char_root_advance_commits_only_the_exact_eviction_and_fault_winner() {
        let coordinator =
            EvictionCoordinator::new(EvictionConfig::default(), Arc::new(EpochManager::new()));
        let first_ptr = SwizzledPtr::on_disk(21, 311, NodeType::CharNode4);
        let second_ptr = SwizzledPtr::on_disk(21, 313, NodeType::CharNode4);
        let mut registry = DiskLocationRegistry::new();
        registry.register_char(vec!['甲'], first_ptr, 47, 1, NodeType::CharNode4);
        registry.register_char(vec!['乙'], second_ptr, 53, 1, NodeType::CharNode4);
        let root = publish_test_char_registry(&coordinator, registry);
        coordinator.lru_registry.touch_hash(hash_char_path(&['甲']));
        coordinator.lru_registry.touch_hash(hash_char_path(&['乙']));

        let batch = coordinator
            .disk_registry
            .read()
            .select_compact_char_for_eviction(
                usize::MAX,
                &coordinator.lru_registry,
                0,
                usize::MAX,
                0,
            );
        let first_index = batch
            .candidates
            .iter()
            .position(|candidate| {
                batch.materialize_char_path(candidate.path_id).as_deref() == Some(&['甲'][..])
            })
            .expect("selected first char record");
        let second_index = batch
            .candidates
            .iter()
            .position(|candidate| {
                batch.materialize_char_path(candidate.path_id).as_deref() == Some(&['乙'][..])
            })
            .expect("selected second char record");
        let captured = root.load_revision().expect("capture char eviction root");
        let first_root = Arc::new(OverlayNode::new());
        let second_root = Arc::new(OverlayNode::new().as_final());
        let first_transition = prepare_test_char_eviction_candidate(
            &coordinator,
            &captured,
            &batch,
            &[first_index],
            Arc::clone(&first_root),
        );
        let second_transition = prepare_test_char_eviction_candidate(
            &coordinator,
            &captured,
            &batch,
            &[second_index],
            second_root,
        );

        assert!(matches!(
            coordinator.commit_char_eviction_transaction(&root, first_transition),
            ExactEvictionOutcome::Committed(1, 47)
        ));
        assert_eq!(test_root_resident_totals(&root), (1, 53));
        assert_eq!(coordinator.lru_registry.len(), 1);
        let after_winner = root
            .load_revision()
            .expect("char root after eviction winner");
        assert!(Arc::ptr_eq(after_winner.node(), &first_root));
        assert_eq!(
            coordinator.commit_char_eviction_transaction(&root, second_transition),
            ExactEvictionOutcome::RootAdvanced
        );
        assert_eq!(test_root_resident_totals(&root), (1, 53));
        assert_eq!(coordinator.lru_registry.len(), 1);
        assert!(after_winner.same_revision(&root.load_revision().expect("unchanged char root")));

        let fault_first_ptr = SwizzledPtr::on_disk(22, 317, NodeType::CharNode4);
        let fault_second_ptr = SwizzledPtr::on_disk(22, 319, NodeType::CharNode4);
        let mut fault_registry = DiskLocationRegistry::new();
        let fault_first_path = fault_registry
            .try_reserve_char_units(RegistryPathId::ROOT, &[u32::from('丙')])
            .expect("reserve first char fault path");
        fault_registry
            .register_nonresident_char_path(
                fault_first_path,
                fault_first_ptr.clone(),
                59,
                1,
                NodeType::CharNode4,
            )
            .expect("register first char fault record");
        let fault_second_path = fault_registry
            .try_reserve_char_units(RegistryPathId::ROOT, &[u32::from('丁')])
            .expect("reserve second char fault path");
        fault_registry
            .register_nonresident_char_path(
                fault_second_path,
                fault_second_ptr.clone(),
                61,
                1,
                NodeType::CharNode4,
            )
            .expect("register second char fault record");
        let captured = root
            .load_revision()
            .expect("char root before fault registry");
        let publication = PreparedRegistryPublication::try_new(
            Arc::clone(&coordinator),
            &captured,
            fault_registry,
            Vec::new(),
        )
        .expect("prepare char fault registry");
        assert_eq!(
            publication.publish(&coordinator, &root),
            RegistryPublicationOutcome::Published
        );
        let captured = root.load_revision().expect("capture competing char faults");
        let first_fault_root = Arc::new(OverlayNode::new());
        first_fault_root.set_durable_stamp(fault_first_ptr.to_raw());
        let second_fault_root = Arc::new(OverlayNode::new());
        second_fault_root.set_durable_stamp(fault_second_ptr.to_raw());
        let first_fault_transition = prepare_test_char_fault_candidate(
            &coordinator,
            &captured,
            &[u32::from('丙')],
            &fault_first_ptr,
            Arc::clone(&first_fault_root),
        );
        let second_fault_transition = prepare_test_char_fault_candidate(
            &coordinator,
            &captured,
            &[u32::from('丁')],
            &fault_second_ptr,
            second_fault_root,
        );

        assert_eq!(
            coordinator.commit_char_fault_transaction(&root, first_fault_transition),
            ExactFaultOutcome::Committed
        );
        assert_eq!(test_root_resident_totals(&root), (1, 59));
        let after_fault_winner = root.load_revision().expect("char fault winner root");
        assert!(Arc::ptr_eq(after_fault_winner.node(), &first_fault_root));
        assert_eq!(
            after_fault_winner.node().durable_stamp(),
            fault_first_ptr.to_raw()
        );
        assert_eq!(
            coordinator.commit_char_fault_transaction(&root, second_fault_transition),
            ExactFaultOutcome::RootAdvanced
        );
        assert_eq!(test_root_resident_totals(&root), (1, 59));
        assert!(after_fault_winner
            .same_revision(&root.load_revision().expect("unchanged char fault root")));
        let reevictable = select_test_char_root_batch(&root, &coordinator);
        assert_eq!(reevictable.candidates.len(), 1);
        assert_eq!(
            reevictable.candidates[0].disk_ptr.to_raw(),
            fault_first_ptr.to_raw()
        );
    }

    #[test]
    fn byte_eviction_rollover_publishes_one_fresh_catalog_and_then_advances_normally() {
        let coordinator =
            EvictionCoordinator::new(EvictionConfig::default(), Arc::new(EpochManager::new()));
        let first_ptr = SwizzledPtr::on_disk(31, 401, NodeType::Node4);
        let second_ptr = SwizzledPtr::on_disk(31, 409, NodeType::Node4);
        let mut registry = DiskLocationRegistry::new();
        registry.register(
            b"rollover-first".to_vec(),
            first_ptr,
            67,
            1,
            NodeType::Node4,
        );
        registry.register(
            b"rollover-second".to_vec(),
            second_ptr,
            71,
            1,
            NodeType::Node4,
        );
        registry
            .try_finalize_for_publication()
            .expect("finalize rollover byte registry");
        coordinator.lru_registry.touch(b"rollover-first");
        coordinator.lru_registry.touch(b"rollover-second");
        let batch = registry.select_compact_for_compatibility(
            usize::MAX,
            &coordinator.lru_registry,
            0,
            usize::MAX,
            0,
        );
        let first_index = batch
            .candidates
            .iter()
            .position(|candidate| {
                batch.materialize_path(candidate.path_id).as_deref() == Some(b"rollover-first")
            })
            .expect("first rollover candidate");
        let second_index = batch
            .candidates
            .iter()
            .position(|candidate| {
                batch.materialize_path(candidate.path_id).as_deref() == Some(b"rollover-second")
            })
            .expect("second rollover candidate");

        let root = bind_test_byte_registry_at_max(&registry);
        let captured = root.load_revision().expect("maximum-ordinal byte root");
        let predecessor_catalog = Arc::clone(
            captured
                .eviction_revision()
                .expect("bound rollover root")
                .catalog(),
        );
        let winner_root = Arc::new(OverlayNode::new());
        let winner = prepare_test_byte_eviction_candidate(
            &coordinator,
            &captured,
            &batch,
            &[first_index],
            Arc::clone(&winner_root),
        );
        let loser = prepare_test_byte_eviction_candidate(
            &coordinator,
            &captured,
            &batch,
            &[second_index],
            Arc::new(OverlayNode::new().as_final()),
        );

        assert_eq!(
            coordinator.commit_byte_eviction_transaction(&root, winner),
            ExactEvictionOutcome::Committed(1, 67)
        );
        assert_eq!(
            coordinator.commit_byte_eviction_transaction(&root, loser),
            ExactEvictionOutcome::RootAdvanced
        );
        let published = root.load_revision().expect("rebased byte root");
        let eviction = published
            .help_eviction_revision()
            .expect("rebased residency is already settled");
        assert_eq!(eviction.ordinal(), 0);
        assert_eq!(eviction.predecessor_ordinal(), 0);
        assert_eq!(eviction.resident_totals(), (1, 71));
        assert!(Arc::ptr_eq(published.node(), &winner_root));
        assert!(!Arc::ptr_eq(eviction.catalog(), &predecessor_catalog));
        assert!(eviction
            .binding()
            .same_publication(predecessor_catalog.binding()));

        captured
            .help_eviction_revision()
            .expect("old settled helper remains complete on its retained catalog");
        let remaining = select_test_byte_root_batch(&root, &coordinator);
        assert_eq!(remaining.candidates.len(), 1);
        assert_eq!(
            remaining.materialize_path(remaining.candidates[0].path_id),
            Some(b"rollover-second".to_vec())
        );

        let captured = root.load_revision().expect("ordinal-zero byte root");
        let ordinary = prepare_test_byte_eviction_candidate(
            &coordinator,
            &captured,
            &remaining,
            &[0],
            Arc::new(OverlayNode::new()),
        );
        assert_eq!(
            coordinator.commit_byte_eviction_transaction(&root, ordinary),
            ExactEvictionOutcome::Committed(1, 71)
        );
        let advanced = root.load_revision().expect("ordinary byte successor");
        assert_eq!(
            advanced
                .help_eviction_revision()
                .expect("ordinary successor materialized")
                .ordinal(),
            1
        );
    }

    #[test]
    fn char_fault_rollover_publishes_one_fresh_catalog_and_then_advances_normally() {
        let coordinator =
            EvictionCoordinator::new(EvictionConfig::default(), Arc::new(EpochManager::new()));
        let first_ptr = SwizzledPtr::on_disk(32, 419, NodeType::CharNode4);
        let second_ptr = SwizzledPtr::on_disk(32, 421, NodeType::CharNode4);
        let mut registry = DiskLocationRegistry::new();
        let first_path = registry
            .try_reserve_char_units(RegistryPathId::ROOT, &[u32::from('甲')])
            .expect("reserve first rollover fault path");
        registry
            .register_nonresident_char_path(
                first_path,
                first_ptr.clone(),
                73,
                1,
                NodeType::CharNode4,
            )
            .expect("register first rollover fault record");
        let second_path = registry
            .try_reserve_char_units(RegistryPathId::ROOT, &[u32::from('乙')])
            .expect("reserve second rollover fault path");
        registry
            .register_nonresident_char_path(
                second_path,
                second_ptr.clone(),
                79,
                1,
                NodeType::CharNode4,
            )
            .expect("register second rollover fault record");
        registry
            .try_finalize_for_publication()
            .expect("finalize rollover char registry");

        let root = bind_test_char_registry_at_max(&registry);
        let captured = root.load_revision().expect("maximum-ordinal char root");
        let predecessor_catalog = Arc::clone(
            captured
                .eviction_revision()
                .expect("bound rollover root")
                .catalog(),
        );
        let first_root = Arc::new(OverlayNode::new());
        first_root.set_durable_stamp(first_ptr.to_raw());
        let second_root = Arc::new(OverlayNode::new());
        second_root.set_durable_stamp(second_ptr.to_raw());
        let winner = prepare_test_char_fault_candidate(
            &coordinator,
            &captured,
            &[u32::from('甲')],
            &first_ptr,
            Arc::clone(&first_root),
        );
        let loser = prepare_test_char_fault_candidate(
            &coordinator,
            &captured,
            &[u32::from('乙')],
            &second_ptr,
            second_root,
        );

        assert_eq!(
            coordinator.commit_char_fault_transaction(&root, winner),
            ExactFaultOutcome::Committed
        );
        assert_eq!(
            coordinator.commit_char_fault_transaction(&root, loser),
            ExactFaultOutcome::RootAdvanced
        );
        let published = root.load_revision().expect("rebased char root");
        let eviction = published
            .help_eviction_revision()
            .expect("rebased char residency is already settled");
        assert_eq!(eviction.ordinal(), 0);
        assert_eq!(eviction.predecessor_ordinal(), 0);
        assert_eq!(eviction.resident_totals(), (1, 73));
        assert!(Arc::ptr_eq(published.node(), &first_root));
        assert!(!Arc::ptr_eq(eviction.catalog(), &predecessor_catalog));
        assert!(eviction
            .binding()
            .same_publication(predecessor_catalog.binding()));

        captured
            .help_eviction_revision()
            .expect("old char helper remains confined to the old catalog");
        let resident = select_test_char_root_batch(&root, &coordinator);
        assert_eq!(resident.candidates.len(), 1);
        assert_eq!(
            resident.materialize_char_path(resident.candidates[0].path_id),
            Some(vec!['甲'])
        );

        let captured = root.load_revision().expect("ordinal-zero char root");
        let second_root = Arc::new(OverlayNode::new());
        second_root.set_durable_stamp(second_ptr.to_raw());
        let ordinary = prepare_test_char_fault_candidate(
            &coordinator,
            &captured,
            &[u32::from('乙')],
            &second_ptr,
            second_root,
        );
        assert_eq!(
            coordinator.commit_char_fault_transaction(&root, ordinary),
            ExactFaultOutcome::Committed
        );
        let advanced = root.load_revision().expect("ordinary char successor");
        let eviction = advanced
            .help_eviction_revision()
            .expect("ordinary char successor materialized");
        assert_eq!(eviction.ordinal(), 1);
        assert_eq!(eviction.resident_totals(), (2, 152));
    }

    #[test]
    fn byte_fault_rollover_preserves_the_inactive_char_family() {
        let coordinator =
            EvictionCoordinator::new(EvictionConfig::default(), Arc::new(EpochManager::new()));
        let first_ptr = SwizzledPtr::on_disk(33, 431, NodeType::Node4);
        let second_ptr = SwizzledPtr::on_disk(33, 433, NodeType::Node4);
        let inactive_char_ptr = SwizzledPtr::on_disk(33, 439, NodeType::CharNode4);
        let mut registry = DiskLocationRegistry::new();
        let first_path = registry
            .try_reserve_byte_path(RegistryPathId::ROOT, b"byte-fault-a")
            .expect("reserve first byte rollover fault path");
        registry
            .register_nonresident_byte_path(
                first_path,
                first_ptr.clone(),
                83,
                b"byte-fault-a".len(),
                NodeType::Node4,
            )
            .expect("register first byte rollover fault record");
        let second_path = registry
            .try_reserve_byte_path(RegistryPathId::ROOT, b"byte-fault-b")
            .expect("reserve second byte rollover fault path");
        registry
            .register_nonresident_byte_path(
                second_path,
                second_ptr.clone(),
                89,
                b"byte-fault-b".len(),
                NodeType::Node4,
            )
            .expect("register second byte rollover fault record");
        registry.register_char(vec!['保'], inactive_char_ptr, 97, 1, NodeType::CharNode4);
        registry
            .try_finalize_for_publication()
            .expect("finalize mixed byte-fault rollover registry");

        let root = bind_test_byte_registry_at_max(&registry);
        let captured = root.load_revision().expect("maximum-ordinal byte root");
        let predecessor_catalog = Arc::clone(
            captured
                .eviction_revision()
                .expect("bound byte-fault rollover root")
                .catalog(),
        );
        let first_root = Arc::new(OverlayNode::new());
        first_root.set_durable_stamp(first_ptr.to_raw());
        let first = prepare_test_byte_fault_candidate(
            &coordinator,
            &captured,
            b"byte-fault-a",
            &first_ptr,
            first_root,
        );
        assert_eq!(
            coordinator.commit_byte_fault_transaction(&root, first),
            ExactFaultOutcome::Committed
        );

        let published = root.load_revision().expect("rebased byte-fault root");
        let eviction = published
            .help_eviction_revision()
            .expect("rebased byte-fault residency is settled");
        assert_eq!(eviction.ordinal(), 0);
        assert_eq!(eviction.predecessor_ordinal(), 0);
        assert_eq!(eviction.resident_totals(), (1, 83));
        assert!(!Arc::ptr_eq(eviction.catalog(), &predecessor_catalog));
        let preserved_char = eviction
            .catalog()
            .try_char_selection_snapshot(0)
            .expect("fresh catalog retains coherent char residency")
            .select_compact(usize::MAX, &coordinator.lru_registry, 0, usize::MAX, 0);
        assert_eq!(preserved_char.candidates.len(), 1);
        assert_eq!(
            preserved_char.materialize_char_path(preserved_char.candidates[0].path_id),
            Some(vec!['保'])
        );
        captured
            .help_eviction_revision()
            .expect("retained byte helper remains confined to its old catalog");

        let captured = root.load_revision().expect("ordinal-zero byte-fault root");
        let second_root = Arc::new(OverlayNode::new());
        second_root.set_durable_stamp(second_ptr.to_raw());
        let ordinary = prepare_test_byte_fault_candidate(
            &coordinator,
            &captured,
            b"byte-fault-b",
            &second_ptr,
            second_root,
        );
        assert_eq!(
            coordinator.commit_byte_fault_transaction(&root, ordinary),
            ExactFaultOutcome::Committed
        );
        let advanced = root.load_revision().expect("ordinary byte-fault successor");
        let eviction = advanced
            .help_eviction_revision()
            .expect("ordinary byte-fault successor materialized");
        assert_eq!(eviction.ordinal(), 1);
        assert_eq!(eviction.resident_totals(), (2, 172));
        assert_eq!(
            eviction
                .catalog()
                .try_char_selection_snapshot(0)
                .expect("ordinary successor retains inactive char residency")
                .select_compact(usize::MAX, &coordinator.lru_registry, 0, usize::MAX, 0,)
                .candidates
                .len(),
            1
        );
    }

    #[test]
    fn char_eviction_rollover_preserves_the_inactive_byte_family() {
        let coordinator =
            EvictionCoordinator::new(EvictionConfig::default(), Arc::new(EpochManager::new()));
        let first_ptr = SwizzledPtr::on_disk(34, 443, NodeType::CharNode4);
        let second_ptr = SwizzledPtr::on_disk(34, 449, NodeType::CharNode4);
        let inactive_byte_ptr = SwizzledPtr::on_disk(34, 457, NodeType::Node4);
        let mut registry = DiskLocationRegistry::new();
        registry.register_char(vec!['甲'], first_ptr, 101, 1, NodeType::CharNode4);
        registry.register_char(vec!['乙'], second_ptr, 103, 1, NodeType::CharNode4);
        registry.register(
            b"inactive-byte".to_vec(),
            inactive_byte_ptr,
            107,
            1,
            NodeType::Node4,
        );
        registry
            .try_finalize_for_publication()
            .expect("finalize mixed char-eviction rollover registry");
        coordinator.lru_registry.touch_hash(hash_char_path(&['甲']));
        coordinator.lru_registry.touch_hash(hash_char_path(&['乙']));
        let batch = registry.select_compact_char_for_compatibility(
            usize::MAX,
            &coordinator.lru_registry,
            0,
            usize::MAX,
            0,
        );
        let first_index = batch
            .candidates
            .iter()
            .position(|candidate| {
                batch.materialize_char_path(candidate.path_id).as_deref() == Some(&['甲'][..])
            })
            .expect("first char rollover candidate");

        let root = bind_test_char_registry_at_max(&registry);
        let captured = root.load_revision().expect("maximum-ordinal char root");
        let predecessor_catalog = Arc::clone(
            captured
                .eviction_revision()
                .expect("bound char-eviction rollover root")
                .catalog(),
        );
        let winner = prepare_test_char_eviction_candidate(
            &coordinator,
            &captured,
            &batch,
            &[first_index],
            Arc::new(OverlayNode::new()),
        );
        assert_eq!(
            coordinator.commit_char_eviction_transaction(&root, winner),
            ExactEvictionOutcome::Committed(1, 101)
        );

        let published = root.load_revision().expect("rebased char-eviction root");
        let eviction = published
            .help_eviction_revision()
            .expect("rebased char-eviction residency is settled");
        assert_eq!(eviction.ordinal(), 0);
        assert_eq!(eviction.predecessor_ordinal(), 0);
        assert_eq!(eviction.resident_totals(), (1, 103));
        assert!(!Arc::ptr_eq(eviction.catalog(), &predecessor_catalog));
        let preserved_byte = eviction
            .catalog()
            .try_byte_selection_snapshot(0)
            .expect("fresh catalog retains coherent byte residency")
            .select_compact(usize::MAX, &coordinator.lru_registry, 0, usize::MAX, 0);
        assert_eq!(preserved_byte.candidates.len(), 1);
        assert_eq!(
            preserved_byte.materialize_path(preserved_byte.candidates[0].path_id),
            Some(b"inactive-byte".to_vec())
        );
        captured
            .help_eviction_revision()
            .expect("retained char helper remains confined to its old catalog");

        let remaining = select_test_char_root_batch(&root, &coordinator);
        assert_eq!(remaining.candidates.len(), 1);
        assert_eq!(
            remaining.materialize_char_path(remaining.candidates[0].path_id),
            Some(vec!['乙'])
        );
        let captured = root
            .load_revision()
            .expect("ordinal-zero char-eviction root");
        let ordinary = prepare_test_char_eviction_candidate(
            &coordinator,
            &captured,
            &remaining,
            &[0],
            Arc::new(OverlayNode::new()),
        );
        assert_eq!(
            coordinator.commit_char_eviction_transaction(&root, ordinary),
            ExactEvictionOutcome::Committed(1, 103)
        );
        let advanced = root
            .load_revision()
            .expect("ordinary char-eviction successor");
        let eviction = advanced
            .help_eviction_revision()
            .expect("ordinary char-eviction successor materialized");
        assert_eq!(eviction.ordinal(), 1);
        assert_eq!(eviction.resident_totals(), (0, 0));
        assert_eq!(
            eviction
                .catalog()
                .try_byte_selection_snapshot(0)
                .expect("ordinary successor retains inactive byte residency")
                .select_compact(usize::MAX, &coordinator.lru_registry, 0, usize::MAX, 0,)
                .candidates
                .len(),
            1
        );
    }

    #[test]
    fn exact_checkpoint_catalog_is_invisible_to_detached_callbacks() {
        let coordinator =
            EvictionCoordinator::new(EvictionConfig::default(), Arc::new(EpochManager::new()));
        let mut exact = DiskLocationRegistry::new();
        exact.register(
            b"exact-only".to_vec(),
            SwizzledPtr::on_disk(1, 191, NodeType::Node4),
            61,
            1,
            NodeType::Node4,
        );
        let root = publish_test_registry(&coordinator, exact);
        let callback_calls = AtomicUsize::new(0);
        assert_eq!(
            coordinator.force_eviction_bytes(usize::MAX, |_| {
                callback_calls.fetch_add(1, Ordering::Relaxed);
                (1, 61)
            }),
            (0, 0),
            "an exact checkpoint must not populate detached callback discovery"
        );
        assert_eq!(callback_calls.load(Ordering::Relaxed), 0);
        assert!(root
            .load_revision()
            .expect("exact root")
            .eviction_binding()
            .is_some());
        assert!(coordinator.disk_registry.read().is_authoritative());

        let mut detached = DiskLocationRegistry::new();
        detached.register(
            b"detached-only".to_vec(),
            SwizzledPtr::on_disk(1, 192, NodeType::Node4),
            62,
            1,
            NodeType::Node4,
        );
        coordinator
            .try_update_disk_registry(detached)
            .expect("install detached callback catalog");
        assert_eq!(
            coordinator.force_eviction_bytes(usize::MAX, |entries| {
                assert_eq!(entries[0].1, b"detached-only");
                (entries.len(), 62)
            }),
            (1, 62)
        );
        assert!(root
            .load_revision()
            .expect("still exact root")
            .eviction_binding()
            .is_some());
        assert!(coordinator.disk_registry.read().is_authoritative());
    }

    #[test]
    fn retired_coordinator_cannot_republish_or_install_detached_catalogs() {
        let gate = RegistryPublicationGate::new();
        let coordinator = EvictionCoordinator::new_with_publication_gate(
            EvictionConfig::default(),
            Arc::new(EpochManager::new()),
            Arc::clone(&gate),
        );
        let root = AtomicNodePtr::new(Arc::new(OverlayNode::<ByteKey, u64>::new()));
        let captured = root.load_revision().expect("captured root");
        let mut registry = DiskLocationRegistry::new();
        registry.register(
            b"retired".to_vec(),
            SwizzledPtr::on_disk(1, 203, NodeType::Node4),
            73,
            1,
            NodeType::Node4,
        );
        let prepared = PreparedRegistryPublication::try_new(
            Arc::clone(&coordinator),
            &captured,
            registry,
            Vec::new(),
        )
        .expect("prepare pre-retirement publication");

        let mut detached = DiskLocationRegistry::new();
        detached.register(
            b"retired-detached".to_vec(),
            SwizzledPtr::on_disk(1, 204, NodeType::Node4),
            74,
            1,
            NodeType::Node4,
        );
        coordinator
            .try_update_disk_registry(detached)
            .expect("install detached registry before retirement");

        coordinator.retire_from_trie();

        assert_eq!(
            prepared.publish(&coordinator, &root),
            RegistryPublicationOutcome::CoordinatorRetired
        );
        let mut rejected = DiskLocationRegistry::new();
        rejected.register(
            b"retired-rejected".to_vec(),
            SwizzledPtr::on_disk(1, 205, NodeType::Node4),
            75,
            1,
            NodeType::Node4,
        );
        assert!(coordinator.try_update_disk_registry(rejected).is_err());
        let mut legacy_rejected = DiskLocationRegistry::new();
        legacy_rejected.register(
            b"retired-legacy-rejected".to_vec(),
            SwizzledPtr::on_disk(1, 206, NodeType::Node4),
            76,
            1,
            NodeType::Node4,
        );
        #[allow(deprecated)]
        coordinator.update_disk_registry(legacy_rejected);
        let callback_calls = AtomicUsize::new(0);
        assert_eq!(
            coordinator.force_eviction_bytes(usize::MAX, |_| {
                callback_calls.fetch_add(1, Ordering::Relaxed);
                (1, 74)
            }),
            (0, 0),
            "a retired coordinator must not admit a legacy callback"
        );
        assert_eq!(callback_calls.load(Ordering::Relaxed), 0);
        assert!(captured.same_revision(&root.load_revision().expect("unchanged root")));
        assert!(!coordinator.disk_registry.read().is_authoritative());
    }

    #[test]
    fn retirement_of_an_unbound_root_publishes_an_unbound_fence() {
        let coordinator =
            EvictionCoordinator::new(EvictionConfig::default(), Arc::new(EpochManager::new()));
        let node = Arc::new(OverlayNode::<ByteKey, u64>::new());
        let root = AtomicNodePtr::new_with_term_count(Arc::clone(&node), 7);
        let captured = root.load_revision().expect("captured unbound root");

        assert_eq!(
            coordinator.retire_from_trie_with_root(&root),
            RetirementOutcome::AlreadyUnbound
        );

        let retired = root.load_revision().expect("retired unbound root");
        assert!(
            !captured.same_revision(&retired),
            "retirement must fence publishers that captured the prior unbound revision"
        );
        assert!(Arc::ptr_eq(retired.node(), &node));
        assert_eq!(retired.term_count(), 7);
        assert!(retired.eviction_binding().is_none());
        assert!(!coordinator.disk_registry.read().is_valid());
    }

    #[test]
    fn terminal_retirement_repairs_a_foreign_binding_without_changing_the_trie() {
        let coordinator =
            EvictionCoordinator::new(EvictionConfig::default(), Arc::new(EpochManager::new()));
        let node = Arc::new(OverlayNode::<ByteKey, u64>::new());
        let root = AtomicNodePtr::new_with_term_count(Arc::clone(&node), 11);
        let captured = root.load_revision().expect("captured root");
        let foreign = EvictionBinding::new();
        let binding = AtomicNodePtr::prepare_checkpoint_binding(
            &captured,
            Arc::new(PublishedRegistryCatalog::empty_for_binding(foreign)),
            0,
            0,
        );
        assert!(root.publish_checkpoint_binding(&binding).is_ok());
        let bound = root.load_revision().expect("foreign-bound root");

        assert_eq!(
            coordinator.retire_from_trie_with_root(&root),
            RetirementOutcome::InvariantRepaired
        );

        let retired = root.load_revision().expect("repaired root");
        assert!(!bound.same_revision(&retired));
        assert!(Arc::ptr_eq(retired.node(), &node));
        assert_eq!(retired.term_count(), 11);
        assert!(retired.eviction_binding().is_none());
        assert!(!coordinator.disk_registry.read().is_valid());
    }

    #[test]
    fn retained_predecessor_callback_does_not_block_replacement_publication() {
        let gate = RegistryPublicationGate::new();
        let predecessor = EvictionCoordinator::new_with_publication_gate(
            EvictionConfig::default(),
            Arc::new(EpochManager::new()),
            Arc::clone(&gate),
        );
        let mut detached = DiskLocationRegistry::new();
        detached.register(
            b"predecessor-callback".to_vec(),
            SwizzledPtr::on_disk(1, 206, NodeType::Node4),
            76,
            1,
            NodeType::Node4,
        );
        predecessor
            .try_update_disk_registry(detached)
            .expect("install predecessor compatibility registry");

        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let callback_predecessor = Arc::clone(&predecessor);
        let callback = thread::spawn(move || {
            callback_predecessor.force_eviction_bytes(usize::MAX, |entries| {
                entered_tx
                    .send(entries.len())
                    .expect("announce predecessor callback admission");
                release_rx.recv().expect("release predecessor callback");
                (entries.len(), 76)
            })
        });
        assert_eq!(
            entered_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("predecessor callback entered"),
            1
        );
        predecessor.retire_from_trie();
        let replacement = EvictionCoordinator::new_with_publication_gate(
            EvictionConfig::default(),
            Arc::new(EpochManager::new()),
            Arc::clone(&gate),
        );
        let root = AtomicNodePtr::new(Arc::new(OverlayNode::<ByteKey, u64>::new()));
        let captured = root.load_revision().expect("captured replacement root");
        let mut prepared_registry = DiskLocationRegistry::new();
        prepared_registry.register(
            b"replacement-prepared".to_vec(),
            SwizzledPtr::on_disk(1, 207, NodeType::Node4),
            77,
            1,
            NodeType::Node4,
        );
        let prepared = PreparedRegistryPublication::try_new(
            Arc::clone(&replacement),
            &captured,
            prepared_registry,
            Vec::new(),
        )
        .expect("prepare replacement publication");
        assert_eq!(
            prepared.publish(&replacement, &root),
            RegistryPublicationOutcome::Published,
            "a detached callback retained from the predecessor cannot block exact publication"
        );

        let mut direct = DiskLocationRegistry::new();
        direct.register(
            b"replacement-direct-live".to_vec(),
            SwizzledPtr::on_disk(1, 208, NodeType::Node4),
            78,
            1,
            NodeType::Node4,
        );
        replacement
            .try_update_disk_registry(direct)
            .expect("replacement detached install does not wait for predecessor callback");

        release_tx
            .send(())
            .expect("release retained predecessor callback");
        assert_eq!(
            callback.join().expect("predecessor callback thread"),
            (1, 76)
        );
        let mut accepted = DiskLocationRegistry::new();
        accepted.register(
            b"replacement-direct-accepted".to_vec(),
            SwizzledPtr::on_disk(1, 209, NodeType::Node4),
            79,
            1,
            NodeType::Node4,
        );
        replacement
            .try_update_disk_registry(accepted)
            .expect("replace detached catalog after predecessor callback release");
        assert_eq!(
            replacement.force_eviction_bytes(usize::MAX, |entries| {
                assert_eq!(entries[0].1, b"replacement-direct-accepted");
                (entries.len(), 79)
            }),
            (1, 79)
        );
    }

    #[test]
    fn retirement_rejects_already_prepared_byte_exact_eviction() {
        let coordinator =
            EvictionCoordinator::new(EvictionConfig::default(), Arc::new(EpochManager::new()));
        let disk_ptr = SwizzledPtr::on_disk(1, 204, NodeType::Node4);
        let mut registry = DiskLocationRegistry::new();
        registry.register(
            b"selected-before-retirement".to_vec(),
            disk_ptr,
            74,
            1,
            NodeType::Node4,
        );
        let root = publish_test_registry(&coordinator, registry);
        let batch = coordinator
            .disk_registry
            .read()
            .select_compact_for_eviction(usize::MAX, &coordinator.lru_registry, 0, usize::MAX, 0);
        let captured = root.load_revision().expect("captured bound byte root");
        let transition = prepare_test_byte_eviction_candidate(
            &coordinator,
            &captured,
            &batch,
            &[0],
            Arc::new(OverlayNode::new()),
        );
        let resident_before = coordinator.resident_estimate_bytes();
        let node_before = Arc::clone(captured.node());
        let count_before = captured.term_count();

        assert_eq!(
            coordinator.retire_from_trie_with_root(&root),
            RetirementOutcome::ExactBindingDetached
        );

        assert_eq!(
            coordinator.commit_byte_eviction_transaction(&root, transition),
            ExactEvictionOutcome::AuthorityLost
        );
        let retired = root.load_revision().expect("retired byte root");
        assert!(!captured.same_revision(&retired));
        assert!(Arc::ptr_eq(retired.node(), &node_before));
        assert_eq!(retired.term_count(), count_before);
        assert!(retired.eviction_binding().is_none());
        assert_eq!(coordinator.resident_estimate_bytes(), resident_before);
    }

    #[test]
    fn retirement_rejects_already_prepared_char_exact_eviction() {
        let coordinator =
            EvictionCoordinator::new(EvictionConfig::default(), Arc::new(EpochManager::new()));
        let disk_ptr = SwizzledPtr::on_disk(1, 205, NodeType::CharNode4);
        let mut registry = DiskLocationRegistry::new();
        registry.register_char(vec!['退'], disk_ptr, 75, 1, NodeType::CharNode4);
        let root = publish_test_char_registry(&coordinator, registry);
        let batch = coordinator
            .disk_registry
            .read()
            .select_compact_char_for_eviction(
                usize::MAX,
                &coordinator.lru_registry,
                0,
                usize::MAX,
                0,
            );
        let captured = root.load_revision().expect("captured bound char root");
        let transition = prepare_test_char_eviction_candidate(
            &coordinator,
            &captured,
            &batch,
            &[0],
            Arc::new(OverlayNode::new()),
        );
        let resident_before = coordinator.resident_estimate_bytes();
        let node_before = Arc::clone(captured.node());
        let count_before = captured.term_count();

        assert_eq!(
            coordinator.retire_from_trie_with_root(&root),
            RetirementOutcome::ExactBindingDetached
        );

        assert_eq!(
            coordinator.commit_char_eviction_transaction(&root, transition),
            ExactEvictionOutcome::AuthorityLost
        );
        let retired = root.load_revision().expect("retired char root");
        assert!(!captured.same_revision(&retired));
        assert!(Arc::ptr_eq(retired.node(), &node_before));
        assert_eq!(retired.term_count(), count_before);
        assert!(retired.eviction_binding().is_none());
        assert_eq!(coordinator.resident_estimate_bytes(), resident_before);
    }

    #[test]
    fn concurrent_detached_callbacks_do_not_block_exact_publication() {
        let coordinator =
            EvictionCoordinator::new(EvictionConfig::default(), Arc::new(EpochManager::new()));
        let mut detached = DiskLocationRegistry::new();
        detached.register(
            b"detached-callback".to_vec(),
            SwizzledPtr::on_disk(1, 196, NodeType::Node4),
            66,
            1,
            NodeType::Node4,
        );
        coordinator
            .try_update_disk_registry(detached)
            .expect("install detached compatibility registry");

        let (entered_one_tx, entered_one_rx) = mpsc::sync_channel(1);
        let (release_one_tx, release_one_rx) = mpsc::sync_channel(1);
        let first_coordinator = Arc::clone(&coordinator);
        let first = thread::spawn(move || {
            first_coordinator.force_eviction_bytes(usize::MAX, |entries| {
                entered_one_tx
                    .send(entries.len())
                    .expect("report first detached callback");
                release_one_rx
                    .recv()
                    .expect("release first detached callback");
                (entries.len(), 66)
            })
        });
        assert_eq!(
            entered_one_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("first detached callback entered"),
            1
        );

        let (entered_two_tx, entered_two_rx) = mpsc::sync_channel(1);
        let (release_two_tx, release_two_rx) = mpsc::sync_channel(1);
        let second_coordinator = Arc::clone(&coordinator);
        let second = thread::spawn(move || {
            second_coordinator.force_eviction_bytes(usize::MAX, |entries| {
                entered_two_tx
                    .send(entries.len())
                    .expect("report second detached callback");
                release_two_rx
                    .recv()
                    .expect("release second detached callback");
                (entries.len(), 66)
            })
        });
        assert_eq!(
            entered_two_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("second detached callback entered"),
            1
        );
        let root = AtomicNodePtr::new(Arc::new(OverlayNode::<ByteKey, u64>::new()));
        let captured = root.load_revision().expect("captured exact root");
        let mut exact = DiskLocationRegistry::new();
        exact.register(
            b"exact-during-callbacks".to_vec(),
            SwizzledPtr::on_disk(1, 197, NodeType::Node4),
            67,
            1,
            NodeType::Node4,
        );
        let prepared = PreparedRegistryPublication::try_new(
            Arc::clone(&coordinator),
            &captured,
            exact,
            Vec::new(),
        )
        .expect("prepare exact publication during detached callbacks");
        assert_eq!(
            prepared.publish(&coordinator, &root),
            RegistryPublicationOutcome::Published
        );

        release_one_tx
            .send(())
            .expect("release first detached callback");
        assert_eq!(
            first.join().expect("first detached callback thread"),
            (1, 66)
        );

        release_two_tx
            .send(())
            .expect("release second detached callback");
        assert_eq!(
            second.join().expect("second detached callback thread"),
            (1, 66)
        );
        assert!(root
            .load_revision()
            .expect("exact root after callbacks")
            .eviction_binding()
            .is_some());
        assert!(coordinator.disk_registry.read().is_authoritative());
    }

    #[test]
    fn panicking_detached_callback_does_not_disturb_exact_authority() {
        let coordinator =
            EvictionCoordinator::new(EvictionConfig::default(), Arc::new(EpochManager::new()));
        let mut exact = DiskLocationRegistry::new();
        exact.register(
            b"panic-exact".to_vec(),
            SwizzledPtr::on_disk(1, 194, NodeType::Node4),
            64,
            1,
            NodeType::Node4,
        );
        let root = publish_test_registry(&coordinator, exact);
        let mut detached = DiskLocationRegistry::new();
        detached.register(
            b"panic-detached".to_vec(),
            SwizzledPtr::on_disk(1, 195, NodeType::Node4),
            65,
            1,
            NodeType::Node4,
        );
        coordinator
            .try_update_disk_registry(detached)
            .expect("install detached panic catalog");
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            coordinator.force_eviction_bytes(usize::MAX, |entries| -> (usize, usize) {
                assert_eq!(entries[0].1, b"panic-detached");
                panic!("intentional legacy callback panic");
            });
        }));
        assert!(unwind.is_err());
        assert!(coordinator.disk_registry.read().is_authoritative());

        let captured = root.load_revision().expect("root after callback unwind");
        let mut replacement = DiskLocationRegistry::new();
        replacement.register(
            b"after-panic".to_vec(),
            SwizzledPtr::on_disk(1, 196, NodeType::Node4),
            66,
            1,
            NodeType::Node4,
        );
        let replacement = PreparedRegistryPublication::try_new(
            Arc::clone(&coordinator),
            &captured,
            replacement,
            Vec::new(),
        )
        .expect("prepare publication after callback unwind");
        assert_eq!(
            replacement.publish(&coordinator, &root),
            RegistryPublicationOutcome::Published
        );
    }

    #[test]
    fn panicking_detached_char_worker_resets_without_disturbing_exact_authority() {
        let config = EvictionConfig {
            batch_size: 1,
            min_eviction_depth: 0,
            enable_memory_pressure_monitor: false,
            ..EvictionConfig::default()
        };
        let coordinator = EvictionCoordinator::new(config, Arc::new(EpochManager::new()));
        let mut initial = DiskLocationRegistry::new();
        initial.register_char(
            vec!['壊'],
            SwizzledPtr::on_disk(1, 197, NodeType::CharNode4),
            67,
            1,
            NodeType::CharNode4,
        );
        let root = publish_test_char_registry(&coordinator, initial);
        let mut detached = DiskLocationRegistry::new();
        detached.register_char(
            vec!['離'],
            SwizzledPtr::on_disk(1, 199, NodeType::CharNode4),
            69,
            1,
            NodeType::CharNode4,
        );
        coordinator
            .try_update_disk_registry(detached)
            .expect("install detached char callback catalog");

        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        coordinator
            .start_char(move |_| -> (usize, usize) {
                entered_tx
                    .send(())
                    .expect("char panic worker receiver remains live");
                panic!("intentional legacy char callback panic");
            })
            .expect("start legacy char panic worker");
        coordinator.request_eviction(EvictionUrgency::Moderate);
        entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("legacy char callback entered");

        let deadline = Instant::now() + Duration::from_secs(5);
        while coordinator.is_running() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        assert!(
            !coordinator.is_running(),
            "char worker exit guard must reset running after callback unwind"
        );
        assert!(coordinator.disk_registry.read().is_authoritative());

        let captured = root.load_revision().expect("char root after worker unwind");
        let mut replacement = DiskLocationRegistry::new();
        replacement.register_char(
            vec!['復'],
            SwizzledPtr::on_disk(1, 198, NodeType::CharNode4),
            68,
            1,
            NodeType::CharNode4,
        );
        let replacement = PreparedRegistryPublication::try_new(
            Arc::clone(&coordinator),
            &captured,
            replacement,
            Vec::new(),
        )
        .expect("prepare char publication after worker unwind");
        assert_eq!(
            replacement.publish(&coordinator, &root),
            RegistryPublicationOutcome::Published
        );
        coordinator.shutdown();
    }
    #[test]
    fn defensive_root_binding_loss_never_activates_registry_authority() {
        let coordinator =
            EvictionCoordinator::new(EvictionConfig::default(), Arc::new(EpochManager::new()));
        let node = Arc::new(OverlayNode::<ByteKey, u64>::new());
        let root = AtomicNodePtr::new(Arc::clone(&node));
        let captured = root.load_revision().expect("captured root revision");
        let disk_ptr = SwizzledPtr::on_disk(1, 80, NodeType::Node4);
        let mut registry = DiskLocationRegistry::new();
        registry.register(
            b"authority-loss".to_vec(),
            disk_ptr.clone(),
            101,
            1,
            NodeType::Node4,
        );
        registry
            .try_finalize_for_publication()
            .expect("finalize authority-loss registry");
        let root_binding = prepare_test_byte_root_binding(&captured, &registry);
        let stamps = vec![DeferredDurableStamp::new(
            Arc::clone(&node),
            disk_ptr.to_raw(),
        )];
        let changed = Arc::new(node.as_final());

        let outcome = coordinator.publish_prepared_registry_with_stamp_action(
            &root,
            root_binding,
            registry,
            stamps,
            |prepared_stamps| {
                for stamp in prepared_stamps {
                    stamp.apply();
                }
                let bound = root.load_revision().expect("temporarily bound root");
                assert!(root
                    .compare_exchange_revision_counted(&bound, Arc::clone(&changed), 1)
                    .is_ok());
            },
        );

        assert_eq!(outcome, RegistryPublicationOutcome::AuthorityLost);
        assert!(!coordinator.disk_registry.read().is_valid());
        assert!(root
            .load_revision()
            .expect("changed root")
            .eviction_binding()
            .is_none());
        assert_eq!(node.durable_stamp(), disk_ptr.to_raw());
    }

    #[test]
    fn root_advance_rolls_back_registry_and_never_applies_stamps() {
        let coordinator =
            EvictionCoordinator::new(EvictionConfig::default(), Arc::new(EpochManager::new()));
        let node = Arc::new(OverlayNode::<ByteKey, u64>::new());
        let root = AtomicNodePtr::new(Arc::clone(&node));
        let mut previous_registry = DiskLocationRegistry::new();
        previous_registry.register(
            b"previous".to_vec(),
            SwizzledPtr::on_disk(1, 71, NodeType::Node4),
            81,
            1,
            NodeType::Node4,
        );
        publish_test_registry_on_root(&coordinator, &root, previous_registry);
        let previous_binding = coordinator.disk_registry.read().binding();

        let captured = root.load_revision().expect("captured root revision");
        let new_ptr = SwizzledPtr::on_disk(1, 72, NodeType::Node4);
        let mut replacement_registry = DiskLocationRegistry::new();
        replacement_registry.register(
            b"replacement".to_vec(),
            new_ptr.clone(),
            82,
            1,
            NodeType::Node4,
        );
        let prepared = PreparedRegistryPublication::try_new(
            Arc::clone(&coordinator),
            &captured,
            replacement_registry,
            vec![DeferredDurableStamp::new(
                Arc::clone(&node),
                new_ptr.to_raw(),
            )],
        )
        .expect("prepare replacement registry");
        let advanced = Arc::new(node.as_final());
        assert!(root
            .compare_exchange_revision_counted(&captured, Arc::clone(&advanced), 1)
            .is_ok());

        assert_eq!(
            prepared.publish(&coordinator, &root),
            RegistryPublicationOutcome::RootAdvanced
        );
        assert_eq!(node.durable_stamp(), 0);
        let retained = coordinator.disk_registry.read();
        assert!(retained.binding().same_publication(&previous_binding));
        assert!(retained
            .get_owned(LruRegistry::path_hash(b"previous"))
            .is_some());
        assert!(retained
            .get_owned(LruRegistry::path_hash(b"replacement"))
            .is_none());
        assert!(Arc::ptr_eq(
            root.load_revision().expect("advanced revision").node(),
            &advanced
        ));
    }

    #[test]
    fn root_advance_restores_a_previously_invalid_registry_exactly() {
        let coordinator =
            EvictionCoordinator::new(EvictionConfig::default(), Arc::new(EpochManager::new()));
        let node = Arc::new(OverlayNode::<ByteKey, u64>::new());
        let root = AtomicNodePtr::new(Arc::clone(&node));
        let initial_revision = root.load_revision().expect("initial root revision");
        let mut previous_registry = DiskLocationRegistry::new();
        previous_registry.register(
            b"invalid-previous".to_vec(),
            SwizzledPtr::on_disk(1, 183, NodeType::Node4),
            83,
            1,
            NodeType::Node4,
        );
        previous_registry
            .try_finalize_for_publication()
            .expect("finalize previous exact registry");
        let initial_binding = prepare_test_byte_root_binding(&initial_revision, &previous_registry);
        assert_eq!(
            coordinator.publish_prepared_registry_with_stamp_action(
                &root,
                initial_binding,
                previous_registry,
                Vec::new(),
                |_| {
                    let bound = root.load_revision().expect("temporarily bound root");
                    assert!(root
                        .compare_exchange_revision_counted(&bound, Arc::new(node.as_final()), 1,)
                        .is_ok());
                },
            ),
            RegistryPublicationOutcome::AuthorityLost
        );
        let previous_binding = coordinator.disk_registry.read().binding();
        assert!(!coordinator.disk_registry.read().is_valid());
        assert!(coordinator
            .disk_registry
            .read()
            .get_owned(LruRegistry::path_hash(b"invalid-previous"))
            .is_some());

        let captured = root.load_revision().expect("captured root revision");
        let replacement_ptr = SwizzledPtr::on_disk(1, 184, NodeType::Node4);
        let mut replacement = DiskLocationRegistry::new();
        replacement.register(
            b"rejected-replacement".to_vec(),
            replacement_ptr.clone(),
            84,
            1,
            NodeType::Node4,
        );
        let prepared = PreparedRegistryPublication::try_new(
            Arc::clone(&coordinator),
            &captured,
            replacement,
            vec![DeferredDurableStamp::new(
                Arc::clone(&node),
                replacement_ptr.to_raw(),
            )],
        )
        .expect("prepare replacement registry");
        let advanced = Arc::new(node.as_final());
        assert!(root
            .compare_exchange_revision_counted(&captured, advanced, 1)
            .is_ok());

        assert_eq!(
            prepared.publish(&coordinator, &root),
            RegistryPublicationOutcome::RootAdvanced
        );
        assert_eq!(node.durable_stamp(), 0);
        let restored = coordinator.disk_registry.read();
        assert!(!restored.is_valid());
        assert!(restored.binding().same_publication(&previous_binding));
        assert!(restored
            .get_owned(LruRegistry::path_hash(b"invalid-previous"))
            .is_some());
        assert!(restored
            .get_owned(LruRegistry::path_hash(b"rejected-replacement"))
            .is_none());
    }

    #[test]
    fn detached_clear_preserves_exact_authority_and_allows_reinstall() {
        let coordinator =
            EvictionCoordinator::new(EvictionConfig::default(), Arc::new(EpochManager::new()));
        let mut exact = DiskLocationRegistry::new();
        exact.register(
            b"exact".to_vec(),
            SwizzledPtr::on_disk(1, 185, NodeType::Node4),
            85,
            1,
            NodeType::Node4,
        );
        let root = publish_test_registry(&coordinator, exact);
        let exact_revision = root.load_revision().expect("bound exact root");
        let exact_binding = coordinator.disk_registry.read().binding();

        let mut initial = DiskLocationRegistry::new();
        initial.register(
            b"detached-initial".to_vec(),
            SwizzledPtr::on_disk(2, 185, NodeType::Node4),
            86,
            1,
            NodeType::Node4,
        );
        coordinator
            .try_install_detached_compatibility_catalog(initial)
            .expect("install initial detached catalog");
        assert_eq!(
            coordinator.force_eviction_bytes(usize::MAX, |entries| {
                assert_eq!(entries[0].1, b"detached-initial");
                (entries.len(), 86)
            }),
            (1, 86)
        );

        coordinator.clear_detached_compatibility_catalog();
        assert_eq!(
            coordinator.force_eviction_bytes(usize::MAX, |_| {
                panic!("a cleared detached catalog must not produce candidates")
            }),
            (0, 0)
        );
        assert!(exact_revision.same_revision(&root.load_revision().expect("unchanged exact root")));
        let retained_exact = coordinator.disk_registry.read();
        assert!(retained_exact.is_authoritative());
        assert!(retained_exact.binding().same_publication(&exact_binding));
        assert!(retained_exact
            .get_owned(LruRegistry::path_hash(b"exact"))
            .is_some());
        drop(retained_exact);

        let mut replacement = DiskLocationRegistry::new();
        replacement.register(
            b"detached-replacement".to_vec(),
            SwizzledPtr::on_disk(2, 186, NodeType::Node4),
            87,
            1,
            NodeType::Node4,
        );
        coordinator
            .try_install_detached_compatibility_catalog(replacement)
            .expect("install replacement detached catalog");
        assert_eq!(
            coordinator.force_eviction_bytes(usize::MAX, |entries| {
                assert_eq!(entries[0].1, b"detached-replacement");
                (entries.len(), 87)
            }),
            (1, 87)
        );
        assert!(exact_revision.same_revision(&root.load_revision().expect("still exact root")));
    }

    #[test]
    fn coordinator_change_rejects_registry_root_and_stamps() {
        let prepared_coordinator =
            EvictionCoordinator::new(EvictionConfig::default(), Arc::new(EpochManager::new()));
        let installed_coordinator =
            EvictionCoordinator::new(EvictionConfig::default(), Arc::new(EpochManager::new()));
        let node = Arc::new(OverlayNode::<ByteKey, u64>::new());
        let root = AtomicNodePtr::new(Arc::clone(&node));
        let captured = root.load_revision().expect("captured root revision");
        let disk_ptr = SwizzledPtr::on_disk(1, 73, NodeType::Node4);
        let mut registry = DiskLocationRegistry::new();
        registry.register(
            b"unpublished".to_vec(),
            disk_ptr.clone(),
            83,
            1,
            NodeType::Node4,
        );
        let prepared = PreparedRegistryPublication::try_new(
            Arc::clone(&prepared_coordinator),
            &captured,
            registry,
            vec![DeferredDurableStamp::new(
                Arc::clone(&node),
                disk_ptr.to_raw(),
            )],
        )
        .expect("prepare coordinator-qualified publication");

        assert_eq!(
            prepared.publish(&installed_coordinator, &root),
            RegistryPublicationOutcome::CoordinatorChanged
        );
        assert_eq!(node.durable_stamp(), 0);
        assert!(captured.same_revision(&root.load_revision().expect("unchanged root")));
        assert_eq!(prepared_coordinator.disk_registry_len(), 0);
        assert_eq!(installed_coordinator.disk_registry_len(), 0);
    }

    #[test]
    fn publication_preparation_failure_preserves_current_registry_and_stamp() {
        let coordinator =
            EvictionCoordinator::new(EvictionConfig::default(), Arc::new(EpochManager::new()));
        let node = Arc::new(OverlayNode::<ByteKey, u64>::new());
        let root = AtomicNodePtr::new(Arc::clone(&node));
        let mut published = DiskLocationRegistry::new();
        published.register(
            b"retained".to_vec(),
            SwizzledPtr::on_disk(1, 74, NodeType::Node4),
            84,
            1,
            NodeType::Node4,
        );
        publish_test_registry_on_root(&coordinator, &root, published);
        let prior_binding = coordinator.disk_registry.read().binding();

        let captured = root.load_revision().expect("captured root revision");
        let mut unfinished = DiskLocationRegistry::new();
        let unfinished_root = unfinished
            .try_reserve_byte_path(RegistryPathId::ROOT, b"unfinished")
            .expect("reserve unfinished path");
        let _open = unfinished
            .try_begin_byte_builder_subtree(unfinished_root)
            .expect("begin unfinished span");
        let result = PreparedRegistryPublication::try_new(
            Arc::clone(&coordinator),
            &captured,
            unfinished,
            vec![DeferredDurableStamp::new(Arc::clone(&node), 75)],
        );

        assert!(matches!(
            result,
            Err(RegistryBuildError::TopologyInvariant(_))
        ));
        assert_eq!(node.durable_stamp(), 0);
        assert!(captured.same_revision(&root.load_revision().expect("unchanged root")));
        let retained = coordinator.disk_registry.read();
        assert!(retained.binding().same_publication(&prior_binding));
        assert!(retained
            .get_owned(LruRegistry::path_hash(b"retained"))
            .is_some());
    }

    #[test]
    fn structural_source_capture_retries_after_registry_generation_change() {
        let coordinator =
            EvictionCoordinator::new(EvictionConfig::default(), Arc::new(EpochManager::new()));
        let root = AtomicNodePtr::<ByteKey, u64>::new(Arc::new(OverlayNode::new()));
        let mut first = DiskLocationRegistry::new();
        first.register(
            b"first".to_vec(),
            SwizzledPtr::on_disk(1, 76, NodeType::Node4),
            86,
            1,
            NodeType::Node4,
        );
        publish_test_registry_on_root(&coordinator, &root, first);
        let stale_plan = coordinator
            .disk_registry
            .read()
            .structural_source_capture_plan()
            .expect("plan first capture");
        let mut byte_bits = Vec::new();
        let mut char_bits = Vec::new();
        stale_plan
            .try_prepare_buffers(&mut byte_bits, &mut char_bits)
            .expect("reserve capture buffers outside lock");

        let mut second = DiskLocationRegistry::new();
        second.register(
            b"second".to_vec(),
            SwizzledPtr::on_disk(1, 77, NodeType::Node4),
            87,
            1,
            NodeType::Node4,
        );
        publish_test_registry_on_root(&coordinator, &root, second);
        let retry = coordinator
            .disk_registry
            .read()
            .try_capture_structural_source(&stale_plan, byte_bits, char_bits)
            .expect("generation mismatch is a retry, not a hard failure");
        let (mut byte_bits, mut char_bits) = match retry {
            RegistryStructuralCapture::Retry {
                byte_residency_bits,
                char_residency_bits,
            } => (byte_residency_bits, char_residency_bits),
            RegistryStructuralCapture::Ready(_) => panic!("stale generation was captured"),
        };
        let current_plan = coordinator
            .disk_registry
            .read()
            .structural_source_capture_plan()
            .expect("plan current capture");
        current_plan
            .try_prepare_buffers(&mut byte_bits, &mut char_bits)
            .expect("reuse capture buffers");
        assert!(matches!(
            coordinator
                .disk_registry
                .read()
                .try_capture_structural_source(&current_plan, byte_bits, char_bits)
                .expect("capture current generation"),
            RegistryStructuralCapture::Ready(_)
        ));
    }

    #[test]
    fn coordinator_clear_removes_only_detached_catalog() {
        let epoch_manager = Arc::new(EpochManager::new());
        let config = EvictionConfig::default();
        let coordinator = EvictionCoordinator::new(config, epoch_manager);

        let mut registry = DiskLocationRegistry::new();
        registry.register(
            b"detached".to_vec(),
            SwizzledPtr::on_disk(1, 300, NodeType::Node4),
            30,
            1,
            NodeType::Node4,
        );
        coordinator
            .try_install_detached_compatibility_catalog(registry)
            .expect("install detached coordinator test catalog");

        coordinator.clear_detached_compatibility_catalog();

        assert_eq!(
            coordinator.force_eviction_bytes(1024, |_| {
                panic!("cleared detached catalog produced candidates")
            }),
            (0, 0)
        );
    }

    #[test]
    fn detached_clear_does_not_observe_or_orphan_exact_authority() {
        let coordinator =
            EvictionCoordinator::new(EvictionConfig::default(), Arc::new(EpochManager::new()));
        let mut registry = DiskLocationRegistry::new();
        registry.register(
            b"bound".to_vec(),
            SwizzledPtr::on_disk(1, 301, NodeType::Node4),
            31,
            1,
            NodeType::Node4,
        );
        let root = publish_test_registry(&coordinator, registry);
        let captured = root.load_revision().expect("captured bound root");

        let mut detached = DiskLocationRegistry::new();
        detached.register(
            b"detached".to_vec(),
            SwizzledPtr::on_disk(2, 301, NodeType::Node4),
            32,
            1,
            NodeType::Node4,
        );
        coordinator
            .try_install_detached_compatibility_catalog(detached)
            .expect("install detached catalog beside exact authority");
        coordinator.clear_detached_compatibility_catalog();
        assert_eq!(
            coordinator.force_eviction_bytes(usize::MAX, |_| {
                panic!("detached clear left callback candidates")
            }),
            (0, 0)
        );

        let retained = root.load_revision().expect("retained bound root");
        assert!(captured.same_revision(&retained));
        let registry = coordinator.disk_registry.read();
        assert!(registry.is_authoritative());
        assert!(retained
            .eviction_binding()
            .is_some_and(|binding| binding.same_publication(&registry.binding())));
    }
}
