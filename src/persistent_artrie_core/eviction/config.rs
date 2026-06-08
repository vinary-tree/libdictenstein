//! Configuration and statistics types for node eviction.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Node eviction configuration.
///
/// Controls how the eviction system responds to memory pressure.
///
/// # Example
///
/// ```text
/// use libdictenstein::persistent_artrie::EvictionConfig;
///
/// let config = EvictionConfig {
///     enabled: true,
///     target_memory_fraction: 0.70,  // Evict until 70% memory available
///     min_eviction_depth: 2,         // Keep root and first-level children in memory
///     batch_size: 256,               // Evict up to 256 nodes per pass
///     ..Default::default()
/// };
/// ```
#[derive(Debug, Clone)]
pub struct EvictionConfig {
    /// Enable memory pressure-driven eviction.
    ///
    /// When `false`, eviction is disabled and nodes stay in memory
    /// until the trie is closed. This is useful for testing or when
    /// memory is abundant.
    ///
    /// Default: `true`
    pub enabled: bool,

    /// Target memory usage fraction after eviction.
    ///
    /// When memory pressure triggers eviction, nodes are evicted until
    /// available memory reaches this fraction of total system memory.
    ///
    /// Default: `0.70` (70% available)
    /// Range: `0.50` - `0.90`
    pub target_memory_fraction: f64,

    /// Minimum depth to evict (0=all, 1=keep root children).
    ///
    /// Nodes at depths less than this value are never evicted.
    /// This keeps frequently-accessed prefix nodes in memory.
    ///
    /// - `0`: All nodes can be evicted (except root)
    /// - `1`: Keep root's direct children in memory
    /// - `2`: Keep root and first two levels in memory
    ///
    /// Default: `1`
    pub min_eviction_depth: usize,

    /// Batch size for eviction (nodes per pass).
    ///
    /// Larger batches are more efficient but may cause longer pauses.
    /// The eviction thread processes this many nodes per cycle.
    ///
    /// Default: `256`
    /// Range: `16` - `4096`
    pub batch_size: usize,

    /// Maximum time to wait for epoch quiescence.
    ///
    /// If readers don't drain within this duration, the eviction
    /// pass is skipped to avoid deadlock.
    ///
    /// Default: `100ms`
    pub quiescence_timeout: Duration,

    /// Polling interval when waiting for epoch quiescence.
    ///
    /// How often to check if old-epoch readers have completed.
    ///
    /// Default: `100µs`
    pub quiescence_poll_interval: Duration,

    /// Eviction cooldown period.
    ///
    /// Minimum time between eviction cycles to prevent thrashing.
    ///
    /// Default: `100ms`
    pub cooldown_period: Duration,

    /// Whether to track LRU (least recently used) for smarter eviction.
    ///
    /// When enabled, nodes are evicted in LRU order (coldest first).
    /// When disabled, eviction uses a simpler depth-first order.
    ///
    /// Default: `true`
    pub use_lru_tracking: bool,

    /// Whether to enable automatic memory pressure monitoring.
    ///
    /// When enabled, a background memory pressure monitor will automatically
    /// trigger eviction when system memory becomes low. The monitor uses
    /// the `sysinfo` crate for cross-platform memory statistics.
    ///
    /// Default: `true`
    pub enable_memory_pressure_monitor: bool,

    /// Memory pressure configuration (used when `enable_memory_pressure_monitor` is true).
    ///
    /// Controls thresholds for triggering eviction based on system memory availability.
    /// If `None`, default memory pressure thresholds are used.
    ///
    /// Default: `None` (use default thresholds)
    pub memory_pressure_config:
        Option<crate::persistent_artrie::memory_monitor::MemoryPressureConfig>,

    /// Optional resident-heap budget (in on-disk-equivalent + per-node-overhead bytes,
    /// the `*_resident_estimate_bytes` unit). `None` (default) = today's UNBOUNDED
    /// behavior (full back-compat; overlay nodes are reclaimed only via the async
    /// memory-pressure loop / explicit `force_eviction`). `Some(b)` = after every
    /// checkpoint, the tail evicts the COLDEST registered overlay nodes down to `b`.
    ///
    /// NOTE: this bounds the POST-CHECKPOINT cold/quiescent resident set. A hot working
    /// set continuously overwritten cannot be evicted (the 1c stamp guard refuses a
    /// node overwritten since its checkpoint), and the inter-checkpoint transient
    /// (superseded path-copy versions, freed lazily) rides above `b` until the next
    /// checkpoint — size `b` below the process limit by that margin (checkpoint more
    /// frequently to shrink it).
    ///
    /// Default: `None`
    pub resident_budget_bytes: Option<usize>,

    /// Optional per-checkpoint cap on how many overlay nodes the budget tail evicts in
    /// ONE pass (a NODE count = the `max_count` of the cold-set selection, which bounds
    /// the O(depth) spine-rebuild + root-CAS work done while `checkpoint_lock` is held).
    /// `None` (default) = UNCAPPED (`usize::MAX`): one pass evicts the entire coldest set
    /// down to the budget — budget-precise, but the FIRST over-budget checkpoint after a
    /// bulk load may hold `checkpoint_lock` for the duration of that one-time large
    /// eviction (the eviction itself is non-blocking loser-safe root-CAS, so concurrent
    /// writers proceed). `Some(n)` = a latency limiter for operators who MEASURED their
    /// per-checkpoint cold growth: it converges over checkpoints ONLY IF `n` ≥ that
    /// growth, else resident accumulates unbounded (the budget never converges). Set it
    /// from a measured number, not a guess.
    ///
    /// Default: `None` (uncapped)
    pub resident_budget_eviction_cap: Option<usize>,
}

impl Default for EvictionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            target_memory_fraction: 0.70,
            min_eviction_depth: 1,
            batch_size: 256,
            quiescence_timeout: Duration::from_millis(100),
            quiescence_poll_interval: Duration::from_micros(100),
            cooldown_period: Duration::from_millis(100),
            use_lru_tracking: true,
            enable_memory_pressure_monitor: true,
            memory_pressure_config: None,
            resident_budget_bytes: None,
            resident_budget_eviction_cap: None,
        }
    }
}

impl EvictionConfig {
    /// Create a configuration with eviction disabled.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            enable_memory_pressure_monitor: false,
            ..Default::default()
        }
    }

    /// Create a configuration optimized for memory-constrained environments.
    ///
    /// Uses aggressive eviction settings to minimize memory usage.
    pub fn memory_constrained() -> Self {
        Self {
            enabled: true,
            target_memory_fraction: 0.80,
            min_eviction_depth: 0,
            batch_size: 512,
            quiescence_timeout: Duration::from_millis(50),
            quiescence_poll_interval: Duration::from_micros(50),
            cooldown_period: Duration::from_millis(50),
            use_lru_tracking: true,
            enable_memory_pressure_monitor: true,
            memory_pressure_config: Some(
                crate::persistent_artrie::memory_monitor::MemoryPressureConfig {
                    low_memory_threshold: 0.20,
                    critical_memory_threshold: 0.05,
                    ..Default::default()
                },
            ),
            resident_budget_bytes: None,
            resident_budget_eviction_cap: None,
        }
    }

    /// Create a configuration optimized for read-heavy workloads.
    ///
    /// Keeps more nodes in memory to reduce disk I/O.
    pub fn read_optimized() -> Self {
        Self {
            enabled: true,
            target_memory_fraction: 0.50,
            min_eviction_depth: 3,
            batch_size: 128,
            quiescence_timeout: Duration::from_millis(200),
            quiescence_poll_interval: Duration::from_micros(200),
            cooldown_period: Duration::from_millis(200),
            use_lru_tracking: true,
            enable_memory_pressure_monitor: true,
            memory_pressure_config: None,
            resident_budget_bytes: None,
            resident_budget_eviction_cap: None,
        }
    }

    /// Create a configuration without memory pressure monitoring.
    ///
    /// Eviction only happens via manual `force_eviction()` calls or explicit
    /// `request_eviction()` calls from external code.
    pub fn without_memory_monitor() -> Self {
        Self {
            enabled: true,
            enable_memory_pressure_monitor: false,
            ..Default::default()
        }
    }

    /// Validate configuration parameters.
    pub fn validate(&self) -> Result<(), String> {
        if self.target_memory_fraction < 0.50 || self.target_memory_fraction > 0.90 {
            return Err(format!(
                "target_memory_fraction must be between 0.50 and 0.90, got {}",
                self.target_memory_fraction
            ));
        }
        if self.batch_size < 16 || self.batch_size > 4096 {
            return Err(format!(
                "batch_size must be between 16 and 4096, got {}",
                self.batch_size
            ));
        }
        if self.quiescence_timeout < Duration::from_millis(10) {
            return Err(format!(
                "quiescence_timeout must be at least 10ms, got {:?}",
                self.quiescence_timeout
            ));
        }
        Ok(())
    }
}

/// Urgency level for eviction requests.
///
/// Higher urgency levels result in more aggressive eviction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EvictionUrgency {
    /// Routine eviction - evict until target memory fraction reached.
    Moderate,
    /// Urgent eviction - evict more aggressively, use smaller cooldown.
    Urgent,
    /// Emergency eviction - evict as much as possible immediately.
    Emergency,
}

impl EvictionUrgency {
    /// Get the batch size multiplier for this urgency level.
    pub fn batch_multiplier(&self) -> usize {
        match self {
            EvictionUrgency::Moderate => 1,
            EvictionUrgency::Urgent => 2,
            EvictionUrgency::Emergency => 4,
        }
    }

    /// Get the cooldown divisor for this urgency level.
    pub fn cooldown_divisor(&self) -> u32 {
        match self {
            EvictionUrgency::Moderate => 1,
            EvictionUrgency::Urgent => 2,
            EvictionUrgency::Emergency => 4,
        }
    }
}

/// Statistics for eviction operations.
///
/// Thread-safe atomic counters for monitoring eviction activity.
///
/// # Example
///
/// ```text
/// use libdictenstein::EvictableARTrie;
///
/// let stats = trie.eviction_stats();
/// println!("Eviction cycles: {}", stats.eviction_cycles);
/// println!("Nodes evicted: {}", stats.nodes_evicted);
/// println!("Bytes freed: {} MB", stats.bytes_freed / (1024 * 1024));
/// ```
#[derive(Debug, Default)]
pub struct EvictionStatsAtomic {
    /// Total number of nodes evicted.
    pub nodes_evicted: AtomicU64,
    /// Total bytes freed by eviction.
    pub bytes_freed: AtomicU64,
    /// Number of eviction cycles completed.
    pub eviction_cycles: AtomicU64,
    /// Duration of last eviction cycle in milliseconds.
    pub last_eviction_duration_ms: AtomicU64,
    /// Number of eviction requests received.
    pub eviction_requests: AtomicU64,
    /// Number of skipped evictions (due to cooldown or timeout).
    pub skipped_evictions: AtomicU64,
    /// Number of times quiescence wait timed out.
    pub quiescence_timeouts: AtomicU64,
}

impl EvictionStatsAtomic {
    /// Create new zeroed statistics.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a successful eviction cycle.
    pub fn record_eviction(&self, nodes: u64, bytes: u64, duration_ms: u64) {
        self.nodes_evicted.fetch_add(nodes, Ordering::Relaxed);
        self.bytes_freed.fetch_add(bytes, Ordering::Relaxed);
        self.eviction_cycles.fetch_add(1, Ordering::Relaxed);
        self.last_eviction_duration_ms
            .store(duration_ms, Ordering::Relaxed);
    }

    /// Record an eviction request.
    pub fn record_request(&self) {
        self.eviction_requests.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a skipped eviction.
    pub fn record_skip(&self) {
        self.skipped_evictions.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a quiescence timeout.
    pub fn record_quiescence_timeout(&self) {
        self.quiescence_timeouts.fetch_add(1, Ordering::Relaxed);
    }

    /// Get a snapshot of the statistics.
    pub fn snapshot(&self) -> EvictionStats {
        EvictionStats {
            nodes_evicted: self.nodes_evicted.load(Ordering::Relaxed),
            bytes_freed: self.bytes_freed.load(Ordering::Relaxed),
            eviction_cycles: self.eviction_cycles.load(Ordering::Relaxed),
            last_eviction_duration_ms: self.last_eviction_duration_ms.load(Ordering::Relaxed),
            eviction_requests: self.eviction_requests.load(Ordering::Relaxed),
            skipped_evictions: self.skipped_evictions.load(Ordering::Relaxed),
            quiescence_timeouts: self.quiescence_timeouts.load(Ordering::Relaxed),
        }
    }

    /// Reset all statistics to zero.
    pub fn reset(&self) {
        self.nodes_evicted.store(0, Ordering::Relaxed);
        self.bytes_freed.store(0, Ordering::Relaxed);
        self.eviction_cycles.store(0, Ordering::Relaxed);
        self.last_eviction_duration_ms.store(0, Ordering::Relaxed);
        self.eviction_requests.store(0, Ordering::Relaxed);
        self.skipped_evictions.store(0, Ordering::Relaxed);
        self.quiescence_timeouts.store(0, Ordering::Relaxed);
    }
}

/// Immutable snapshot of eviction statistics.
#[derive(Debug, Clone, Copy, Default)]
pub struct EvictionStats {
    /// Total number of nodes evicted.
    pub nodes_evicted: u64,
    /// Total bytes freed by eviction.
    pub bytes_freed: u64,
    /// Number of eviction cycles completed.
    pub eviction_cycles: u64,
    /// Duration of last eviction cycle in milliseconds.
    pub last_eviction_duration_ms: u64,
    /// Number of eviction requests received.
    pub eviction_requests: u64,
    /// Number of skipped evictions (due to cooldown or timeout).
    pub skipped_evictions: u64,
    /// Number of times quiescence wait timed out.
    pub quiescence_timeouts: u64,
}

impl EvictionStats {
    /// Get the average nodes evicted per cycle.
    pub fn nodes_per_cycle(&self) -> f64 {
        if self.eviction_cycles == 0 {
            0.0
        } else {
            self.nodes_evicted as f64 / self.eviction_cycles as f64
        }
    }

    /// Get the average bytes freed per cycle.
    pub fn bytes_per_cycle(&self) -> f64 {
        if self.eviction_cycles == 0 {
            0.0
        } else {
            self.bytes_freed as f64 / self.eviction_cycles as f64
        }
    }

    /// Get the skip rate (0.0 to 1.0).
    pub fn skip_rate(&self) -> f64 {
        if self.eviction_requests == 0 {
            0.0
        } else {
            self.skipped_evictions as f64 / self.eviction_requests as f64
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_eviction_config_default() {
        let config = EvictionConfig::default();
        assert!(config.enabled);
        assert_eq!(config.target_memory_fraction, 0.70);
        assert_eq!(config.min_eviction_depth, 1);
        assert_eq!(config.batch_size, 256);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_eviction_config_disabled() {
        let config = EvictionConfig::disabled();
        assert!(!config.enabled);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_eviction_config_validation() {
        // Invalid target_memory_fraction (too low)
        let config = EvictionConfig {
            target_memory_fraction: 0.40,
            ..Default::default()
        };
        assert!(config.validate().is_err());

        // Invalid target_memory_fraction (too high)
        let config = EvictionConfig {
            target_memory_fraction: 0.95,
            ..Default::default()
        };
        assert!(config.validate().is_err());

        // Invalid batch_size (too small)
        let config = EvictionConfig {
            batch_size: 8,
            ..Default::default()
        };
        assert!(config.validate().is_err());

        // Invalid batch_size (too large)
        let config = EvictionConfig {
            batch_size: 8192,
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_eviction_urgency() {
        assert_eq!(EvictionUrgency::Moderate.batch_multiplier(), 1);
        assert_eq!(EvictionUrgency::Urgent.batch_multiplier(), 2);
        assert_eq!(EvictionUrgency::Emergency.batch_multiplier(), 4);

        assert_eq!(EvictionUrgency::Moderate.cooldown_divisor(), 1);
        assert_eq!(EvictionUrgency::Urgent.cooldown_divisor(), 2);
        assert_eq!(EvictionUrgency::Emergency.cooldown_divisor(), 4);

        // Ordering
        assert!(EvictionUrgency::Moderate < EvictionUrgency::Urgent);
        assert!(EvictionUrgency::Urgent < EvictionUrgency::Emergency);
    }

    #[test]
    fn test_eviction_stats_atomic() {
        let stats = EvictionStatsAtomic::new();

        stats.record_request();
        stats.record_eviction(100, 1024 * 1024, 50);
        stats.record_request();
        stats.record_skip();

        let snapshot = stats.snapshot();
        assert_eq!(snapshot.nodes_evicted, 100);
        assert_eq!(snapshot.bytes_freed, 1024 * 1024);
        assert_eq!(snapshot.eviction_cycles, 1);
        assert_eq!(snapshot.last_eviction_duration_ms, 50);
        assert_eq!(snapshot.eviction_requests, 2);
        assert_eq!(snapshot.skipped_evictions, 1);
    }

    #[test]
    fn test_eviction_stats_calculations() {
        let stats = EvictionStats {
            nodes_evicted: 1000,
            bytes_freed: 10 * 1024 * 1024,
            eviction_cycles: 10,
            eviction_requests: 20,
            skipped_evictions: 5,
            ..Default::default()
        };

        assert_eq!(stats.nodes_per_cycle(), 100.0);
        assert_eq!(stats.bytes_per_cycle(), 1024.0 * 1024.0);
        assert_eq!(stats.skip_rate(), 0.25);
    }
}
