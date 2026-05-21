//! Adaptive Buffer Pool Sizing
//!
//! This module implements dynamic buffer pool sizing using a PID controller
//! that adjusts pool size based on cache hit rate and available memory.
//!
//! # Design
//!
//! The adaptive pool controller monitors:
//! - Cache hit rate (target: 95%)
//! - Available system memory (target: use 25%)
//! - Memory pressure level (from memory_monitor)
//!
//! It then adjusts the buffer pool size using a PID control algorithm:
//! - P (Proportional): Responds to current error
//! - I (Integral): Eliminates steady-state error
//! - D (Derivative): Dampens oscillation
//!
//! # Stability Features
//!
//! - Hysteresis zone prevents thrashing
//! - Maximum step sizes limit rapid changes
//! - Memory pressure overrides normal growth
//! - Anti-windup on integral term

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use parking_lot::RwLock;

use super::block_storage::BlockStorage;
use super::buffer_manager::BufferManager;
use super::disk_manager::MmapDiskManager;
use super::memory_monitor::{MemoryPressureLevel, MemoryPressureMonitor};

/// Configuration for adaptive buffer pool sizing.
///
/// The adaptive pool controller uses a PID-like algorithm to adjust
/// the buffer pool size based on cache hit rate and available memory.
///
/// # Algorithm
///
/// Every `adjustment_interval`, the controller:
/// 1. Measures current cache hit rate and available memory
/// 2. Computes error from target values
/// 3. Adjusts pool size proportionally (bounded by step limits)
///
/// # Stability
///
/// The controller includes safeguards against oscillation:
/// - Maximum step sizes limit rapid changes
/// - Hysteresis zone around target prevents thrashing
/// - Memory pressure overrides normal growth
#[derive(Debug, Clone)]
pub struct AdaptivePoolConfig {
    /// Minimum pool size in frames.
    ///
    /// The pool will never shrink below this size.
    ///
    /// Default: 16 (4MB with 256KB frames)
    pub min_pool_size: usize,

    /// Maximum pool size in frames.
    ///
    /// The pool will never grow beyond this size, even if memory
    /// is available. Set based on workload requirements.
    ///
    /// Default: 1024 (256MB with 256KB frames)
    pub max_pool_size: usize,

    /// Target fraction of available system memory to use.
    ///
    /// The controller tries to keep pool size at this fraction
    /// of available memory. Lower values leave more headroom
    /// for other applications.
    ///
    /// Default: 0.25 (25%)
    pub target_memory_fraction: f64,

    /// Target cache hit rate.
    ///
    /// The controller will grow the pool if hit rate falls below
    /// this target (and memory is available).
    ///
    /// Default: 0.95 (95%)
    pub target_hit_rate: f64,

    /// Hit rate threshold below which growth is considered.
    ///
    /// Pool only grows if hit rate is below this AND below target.
    /// This creates a hysteresis zone to prevent oscillation.
    ///
    /// Default: 0.90 (90%)
    pub min_hit_rate_for_growth: f64,

    /// Interval between pool size adjustments.
    ///
    /// Default: 10 seconds
    pub adjustment_interval: Duration,

    /// Maximum frames to add per adjustment.
    ///
    /// Limits how fast the pool can grow.
    ///
    /// Default: 16
    pub max_growth_step: usize,

    /// Maximum frames to remove per adjustment.
    ///
    /// Limits how fast the pool can shrink (slower than growth
    /// to avoid thrashing).
    ///
    /// Default: 8
    pub max_shrink_step: usize,

    /// Proportional gain for PID controller.
    ///
    /// Higher values give faster response but may cause oscillation.
    ///
    /// Default: 0.5
    pub kp: f64,

    /// Integral gain for PID controller.
    ///
    /// Helps eliminate steady-state error. Set to 0 to disable.
    ///
    /// Default: 0.1
    pub ki: f64,

    /// Derivative gain for PID controller.
    ///
    /// Dampens oscillation. Set to 0 to disable.
    ///
    /// Default: 0.05
    pub kd: f64,

    /// Enable adaptive sizing.
    ///
    /// Set to false to use a fixed pool size.
    ///
    /// Default: true
    pub enabled: bool,
}

impl Default for AdaptivePoolConfig {
    fn default() -> Self {
        Self {
            min_pool_size: 16,
            max_pool_size: 1024,
            target_memory_fraction: 0.25,
            target_hit_rate: 0.95,
            min_hit_rate_for_growth: 0.90,
            adjustment_interval: Duration::from_secs(10),
            max_growth_step: 16,
            max_shrink_step: 8,
            kp: 0.5,
            ki: 0.1,
            kd: 0.05,
            enabled: true,
        }
    }
}

/// Cache hit/miss counters for hit rate calculation.
#[derive(Debug)]
pub struct CacheStats {
    /// Number of cache hits.
    hits: AtomicU64,
    /// Number of cache misses.
    misses: AtomicU64,
}

impl Default for CacheStats {
    fn default() -> Self {
        Self::new()
    }
}

impl CacheStats {
    /// Create a new cache stats tracker.
    pub fn new() -> Self {
        Self {
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    /// Record a cache hit.
    #[inline]
    pub fn record_hit(&self) {
        self.hits.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a cache miss.
    #[inline]
    pub fn record_miss(&self) {
        self.misses.fetch_add(1, Ordering::Relaxed);
    }

    /// Get hit rate and reset counters.
    ///
    /// Returns (hit_rate, hits, misses).
    pub fn get_and_reset(&self) -> (f64, u64, u64) {
        let hits = self.hits.swap(0, Ordering::Relaxed);
        let misses = self.misses.swap(0, Ordering::Relaxed);

        let total = hits + misses;
        let hit_rate = if total == 0 {
            1.0 // No accesses means no misses
        } else {
            hits as f64 / total as f64
        };

        (hit_rate, hits, misses)
    }

    /// Get current hit rate without resetting.
    pub fn hit_rate(&self) -> f64 {
        let hits = self.hits.load(Ordering::Relaxed);
        let misses = self.misses.load(Ordering::Relaxed);
        let total = hits + misses;

        if total == 0 {
            1.0
        } else {
            hits as f64 / total as f64
        }
    }

    /// Get total number of accesses (hits + misses).
    pub fn total_accesses(&self) -> u64 {
        self.hits.load(Ordering::Relaxed) + self.misses.load(Ordering::Relaxed)
    }

    /// Get current counts without resetting.
    pub fn counts(&self) -> (u64, u64) {
        (
            self.hits.load(Ordering::Relaxed),
            self.misses.load(Ordering::Relaxed),
        )
    }
}

/// PID controller state.
#[derive(Debug)]
struct PidController {
    /// Proportional gain.
    kp: f64,
    /// Integral gain.
    ki: f64,
    /// Derivative gain.
    kd: f64,

    /// Integral accumulator.
    integral: f64,

    /// Previous error for derivative calculation.
    prev_error: f64,

    /// Anti-windup: minimum integral value.
    integral_min: f64,
    /// Anti-windup: maximum integral value.
    integral_max: f64,
}

impl PidController {
    /// Create a new PID controller.
    fn new(kp: f64, ki: f64, kd: f64) -> Self {
        Self {
            kp,
            ki,
            kd,
            integral: 0.0,
            prev_error: 0.0,
            integral_min: -100.0,
            integral_max: 100.0,
        }
    }

    /// Compute PID output for given error and time delta.
    fn compute(&mut self, error: f64, dt: f64) -> f64 {
        // Proportional term
        let p = self.kp * error;

        // Integral term with anti-windup
        self.integral += error * dt;
        self.integral = self.integral.clamp(self.integral_min, self.integral_max);
        let i = self.ki * self.integral;

        // Derivative term
        let d = if dt > 0.0 {
            self.kd * (error - self.prev_error) / dt
        } else {
            0.0
        };
        self.prev_error = error;

        p + i + d
    }

    /// Reset the controller state.
    fn reset(&mut self) {
        self.integral = 0.0;
        self.prev_error = 0.0;
    }
}

/// Statistics from the adaptive pool controller.
#[derive(Debug, Clone, Copy)]
pub struct AdaptivePoolStats {
    /// Current pool size in frames.
    pub current_size: usize,
    /// Number of adjustments made.
    pub adjustments: u64,
    /// Number of grow operations.
    pub grows: u64,
    /// Number of shrink operations.
    pub shrinks: u64,
    /// Last recorded hit rate.
    pub last_hit_rate: f64,
    /// Last recorded memory pressure level.
    pub last_pressure: MemoryPressureLevel,
}

/// Adaptive buffer pool controller.
///
/// Uses a PID control algorithm to dynamically adjust buffer pool size
/// based on cache hit rate and available memory.
pub struct AdaptivePoolController<S: BlockStorage + 'static = MmapDiskManager> {
    /// Configuration.
    config: AdaptivePoolConfig,

    /// Buffer manager reference.
    buffer_manager: Arc<BufferManager<S>>,

    /// Cache statistics.
    cache_stats: Arc<CacheStats>,

    /// Memory pressure monitor.
    memory_monitor: Arc<MemoryPressureMonitor>,

    /// Current pool size.
    current_size: AtomicUsize,

    /// Number of adjustments made.
    adjustments: AtomicU64,

    /// Number of grow operations.
    grows: AtomicU64,

    /// Number of shrink operations.
    shrinks: AtomicU64,

    /// Last recorded hit rate.
    last_hit_rate: RwLock<f64>,

    /// Last recorded pressure level.
    last_pressure: RwLock<MemoryPressureLevel>,

    /// Controller thread handle.
    controller_thread: Option<JoinHandle<()>>,

    /// Shutdown flag.
    shutdown: Arc<AtomicBool>,
}

impl<S: BlockStorage + 'static> AdaptivePoolController<S> {
    /// Create a new adaptive pool controller.
    ///
    /// # Arguments
    /// * `config` - Controller configuration
    /// * `buffer_manager` - Reference to the buffer manager to control
    /// * `cache_stats` - Shared cache statistics tracker
    /// * `memory_monitor` - Memory pressure monitor
    pub fn new(
        config: AdaptivePoolConfig,
        buffer_manager: Arc<BufferManager<S>>,
        cache_stats: Arc<CacheStats>,
        memory_monitor: Arc<MemoryPressureMonitor>,
    ) -> Self {
        let initial_size = buffer_manager.pool_size();

        Self {
            config,
            buffer_manager,
            cache_stats,
            memory_monitor,
            current_size: AtomicUsize::new(initial_size),
            adjustments: AtomicU64::new(0),
            grows: AtomicU64::new(0),
            shrinks: AtomicU64::new(0),
            last_hit_rate: RwLock::new(1.0),
            last_pressure: RwLock::new(MemoryPressureLevel::Normal),
            controller_thread: None,
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Start the controller background thread.
    ///
    /// Does nothing if adaptive sizing is disabled in config.
    pub fn start(&mut self) {
        if !self.config.enabled {
            return;
        }

        let config = self.config.clone();
        let buffer_manager = Arc::clone(&self.buffer_manager);
        let cache_stats = Arc::clone(&self.cache_stats);
        let memory_monitor = Arc::clone(&self.memory_monitor);
        let current_size = Arc::new(AtomicUsize::new(self.current_size.load(Ordering::Relaxed)));
        let adjustments = Arc::new(AtomicU64::new(0));
        let grows = Arc::new(AtomicU64::new(0));
        let shrinks = Arc::new(AtomicU64::new(0));
        let shutdown = Arc::clone(&self.shutdown);

        // Clone Arcs for stats updates
        let current_size_clone = Arc::clone(&current_size);
        let adjustments_clone = Arc::clone(&adjustments);
        let grows_clone = Arc::clone(&grows);
        let shrinks_clone = Arc::clone(&shrinks);

        self.controller_thread = Some(
            thread::Builder::new()
                .name("artrie-adaptive-pool".to_string())
                .spawn(move || {
                    Self::control_loop(
                        config,
                        buffer_manager,
                        cache_stats,
                        memory_monitor,
                        current_size_clone,
                        adjustments_clone,
                        grows_clone,
                        shrinks_clone,
                        shutdown,
                    );
                })
                .expect("failed to spawn adaptive pool controller thread"),
        );
    }

    /// Get current pool size.
    pub fn pool_size(&self) -> usize {
        self.current_size.load(Ordering::Relaxed)
    }

    /// Get controller statistics.
    pub fn stats(&self) -> AdaptivePoolStats {
        let (last_hit_rate, last_pressure) =
            { (*self.last_hit_rate.read(), *self.last_pressure.read()) };

        AdaptivePoolStats {
            current_size: self.current_size.load(Ordering::Relaxed),
            adjustments: self.adjustments.load(Ordering::Relaxed),
            grows: self.grows.load(Ordering::Relaxed),
            shrinks: self.shrinks.load(Ordering::Relaxed),
            last_hit_rate,
            last_pressure,
        }
    }

    /// Check if the controller is running.
    pub fn is_running(&self) -> bool {
        self.controller_thread.is_some() && !self.shutdown.load(Ordering::Relaxed)
    }

    /// Stop the controller.
    pub fn stop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(handle) = self.controller_thread.take() {
            let _ = handle.join();
        }
    }

    /// Main control loop running in background thread.
    fn control_loop(
        config: AdaptivePoolConfig,
        buffer_manager: Arc<BufferManager<S>>,
        cache_stats: Arc<CacheStats>,
        memory_monitor: Arc<MemoryPressureMonitor>,
        current_size: Arc<AtomicUsize>,
        adjustments: Arc<AtomicU64>,
        grows: Arc<AtomicU64>,
        shrinks: Arc<AtomicU64>,
        shutdown: Arc<AtomicBool>,
    ) {
        let mut pid = PidController::new(config.kp, config.ki, config.kd);
        let mut last_time = Instant::now();
        let mut size = current_size.load(Ordering::Relaxed);

        // Frame size for memory calculations (256KB)
        const FRAME_SIZE: usize = 256 * 1024;

        while !shutdown.load(Ordering::Relaxed) {
            thread::sleep(config.adjustment_interval);

            if shutdown.load(Ordering::Relaxed) {
                break;
            }

            // Collect metrics
            let (hit_rate, hits, misses) = cache_stats.get_and_reset();
            let memory_stats = memory_monitor.current_stats();
            let pressure = memory_monitor.current_level();

            // Calculate time delta
            let now = Instant::now();
            let dt = now.duration_since(last_time).as_secs_f64();
            last_time = now;

            // Skip if no activity
            if hits + misses == 0 {
                continue;
            }

            // Calculate target size based on available memory
            let available_memory = memory_stats.mem_available as f64;
            let memory_target_size =
                ((available_memory * config.target_memory_fraction) / FRAME_SIZE as f64) as usize;

            // Calculate adjustment based on hit rate error
            let hit_rate_error = config.target_hit_rate - hit_rate;
            let pid_output = pid.compute(hit_rate_error, dt);

            // Convert PID output to size change (proportional to current size)
            let size_delta = (pid_output * size as f64) as isize;

            // Apply constraints based on memory pressure
            let new_size = match pressure {
                MemoryPressureLevel::Critical => {
                    // Emergency: shrink to minimum immediately
                    pid.reset(); // Reset PID to avoid windup
                    config.min_pool_size
                }
                MemoryPressureLevel::Low => {
                    // Under pressure: don't grow, may shrink
                    let shrink = (size_delta.min(0).unsigned_abs()).min(config.max_shrink_step);
                    size.saturating_sub(shrink).max(config.min_pool_size)
                }
                MemoryPressureLevel::Normal => {
                    // Normal: apply PID control
                    if size_delta > 0 && hit_rate < config.min_hit_rate_for_growth {
                        // Grow (bounded by step size, max pool size, and memory target)
                        let grow = (size_delta as usize).min(config.max_growth_step);
                        (size + grow)
                            .min(config.max_pool_size)
                            .min(memory_target_size)
                    } else if size_delta < 0 {
                        // Shrink (bounded by step size and min pool size)
                        let shrink = (size_delta.unsigned_abs()).min(config.max_shrink_step);
                        size.saturating_sub(shrink).max(config.min_pool_size)
                    } else {
                        size
                    }
                }
            };

            // Apply size change if different
            if new_size != size {
                adjustments.fetch_add(1, Ordering::Relaxed);

                if new_size > size {
                    let delta = new_size - size;
                    if buffer_manager.grow_pool(delta).is_ok() {
                        grows.fetch_add(1, Ordering::Relaxed);
                        size = new_size;
                    }
                } else {
                    let delta = size - new_size;
                    if buffer_manager.shrink_pool(delta).is_ok() {
                        shrinks.fetch_add(1, Ordering::Relaxed);
                        size = new_size;
                    }
                }

                current_size.store(size, Ordering::Relaxed);
            }
        }
    }
}

impl<S: BlockStorage + 'static> Drop for AdaptivePoolController<S> {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_stats_new() {
        let stats = CacheStats::new();
        assert_eq!(stats.hit_rate(), 1.0); // No accesses = 100% hit rate
        assert_eq!(stats.total_accesses(), 0);
    }

    #[test]
    fn test_cache_stats_recording() {
        let stats = CacheStats::new();

        stats.record_hit();
        stats.record_hit();
        stats.record_miss();

        let (hits, misses) = stats.counts();
        assert_eq!(hits, 2);
        assert_eq!(misses, 1);
        assert!((stats.hit_rate() - 0.667).abs() < 0.01);
    }

    #[test]
    fn test_cache_stats_reset() {
        let stats = CacheStats::new();

        stats.record_hit();
        stats.record_miss();

        let (hit_rate, hits, misses) = stats.get_and_reset();
        assert_eq!(hits, 1);
        assert_eq!(misses, 1);
        assert!((hit_rate - 0.5).abs() < 0.001);

        // After reset, should be empty
        assert_eq!(stats.total_accesses(), 0);
        assert_eq!(stats.hit_rate(), 1.0);
    }

    #[test]
    fn test_pid_controller_proportional() {
        let mut pid = PidController::new(1.0, 0.0, 0.0); // P only

        let output = pid.compute(0.1, 1.0);
        assert!((output - 0.1).abs() < 0.001);

        let output = pid.compute(-0.2, 1.0);
        assert!((output - (-0.2)).abs() < 0.001);
    }

    #[test]
    fn test_pid_controller_integral() {
        let mut pid = PidController::new(0.0, 1.0, 0.0); // I only

        // First step
        let output1 = pid.compute(0.1, 1.0);
        assert!((output1 - 0.1).abs() < 0.001);

        // Second step - integral accumulates
        let output2 = pid.compute(0.1, 1.0);
        assert!((output2 - 0.2).abs() < 0.001);
    }

    #[test]
    fn test_pid_controller_derivative() {
        let mut pid = PidController::new(0.0, 0.0, 1.0); // D only

        // First step - no previous error
        let output1 = pid.compute(0.1, 1.0);
        assert!((output1 - 0.1).abs() < 0.001);

        // Second step with same error - derivative is 0
        let output2 = pid.compute(0.1, 1.0);
        assert!(output2.abs() < 0.001);

        // Third step with larger error - positive derivative
        let output3 = pid.compute(0.2, 1.0);
        assert!((output3 - 0.1).abs() < 0.001);
    }

    #[test]
    fn test_pid_controller_reset() {
        let mut pid = PidController::new(0.0, 1.0, 0.0);

        pid.compute(0.1, 1.0);
        pid.compute(0.1, 1.0);

        pid.reset();

        // After reset, integral should be 0
        let output = pid.compute(0.1, 1.0);
        assert!((output - 0.1).abs() < 0.001);
    }

    #[test]
    fn test_pid_anti_windup() {
        let mut pid = PidController::new(0.0, 1.0, 0.0);

        // Large error for many steps - integral should be clamped
        for _ in 0..1000 {
            pid.compute(10.0, 1.0);
        }

        // Output should be clamped to integral_max
        let output = pid.compute(0.0, 1.0);
        assert!(output <= 100.0);
    }

    #[test]
    fn test_adaptive_pool_config_default() {
        let config = AdaptivePoolConfig::default();

        assert_eq!(config.min_pool_size, 16);
        assert_eq!(config.max_pool_size, 1024);
        assert!((config.target_memory_fraction - 0.25).abs() < 0.001);
        assert!((config.target_hit_rate - 0.95).abs() < 0.001);
        assert!((config.min_hit_rate_for_growth - 0.90).abs() < 0.001);
        assert_eq!(config.adjustment_interval, Duration::from_secs(10));
        assert_eq!(config.max_growth_step, 16);
        assert_eq!(config.max_shrink_step, 8);
        assert!(config.enabled);
    }

    // =========================================================================
    // Edge case tests for branch coverage
    // =========================================================================

    /// Test CacheStats::get_and_reset with zero accesses (line 207-208).
    /// When total accesses is 0, hit_rate should return 1.0.
    #[test]
    fn test_cache_stats_get_and_reset_zero_accesses() {
        let stats = CacheStats::new();

        // Test get_and_reset with zero accesses
        let (rate, hits, misses) = stats.get_and_reset();
        assert_eq!(rate, 1.0, "Hit rate should be 1.0 when no accesses");
        assert_eq!(hits, 0);
        assert_eq!(misses, 0);
    }

    /// Test CacheStats::hit_rate with zero accesses (line 222-223).
    /// When total accesses is 0, hit_rate should return 1.0.
    #[test]
    fn test_cache_stats_hit_rate_zero_accesses() {
        let stats = CacheStats::new();

        // Test hit_rate with zero accesses
        assert_eq!(
            stats.hit_rate(),
            1.0,
            "Hit rate should be 1.0 when no accesses"
        );
        assert_eq!(stats.total_accesses(), 0);
    }

    /// Test PID controller with dt == 0 (line 290-294).
    /// When dt is 0, the derivative term should be 0.
    #[test]
    fn test_pid_controller_zero_dt() {
        let mut pid = PidController::new(0.5, 0.1, 0.05);

        // Test with dt == 0.0 - derivative term should be 0
        let output = pid.compute(0.1, 0.0);

        // With dt == 0:
        // P term = 0.5 * 0.1 = 0.05
        // I term = 0.1 * (0.1 * 0.0) = 0.0 (integral is 0)
        // D term = 0.0 (because dt == 0)
        // Total = 0.05
        assert!(
            (output - 0.05).abs() < 0.01,
            "Output should be ~0.05 with dt=0 (no D term): got {}",
            output
        );
    }

    /// Test PID controller with positive dt (line 290-294).
    /// When dt > 0, the derivative term should be computed.
    #[test]
    fn test_pid_controller_positive_dt() {
        let mut pid = PidController::new(0.5, 0.1, 0.05);

        // First call to establish prev_error
        let _output1 = pid.compute(0.1, 1.0);

        // Second call with same error - derivative should be ~0
        let output2 = pid.compute(0.1, 1.0);

        // P = 0.5 * 0.1 = 0.05
        // I = 0.1 * (0.1 + 0.1) = 0.02 (integral accumulated)
        // D = 0.05 * (0.1 - 0.1) / 1.0 = 0 (same error, no change)
        assert!(
            output2.abs() < 0.1,
            "Output with constant error should be small: got {}",
            output2
        );
    }

    /// Test PID controller derivative term with changing error.
    #[test]
    fn test_pid_controller_derivative_term() {
        // Use only D-term controller to isolate derivative behavior
        let mut pid = PidController::new(0.0, 0.0, 1.0);

        // First call establishes prev_error = 0.0
        let output1 = pid.compute(0.1, 1.0);
        // D = 1.0 * (0.1 - 0.0) / 1.0 = 0.1
        assert!(
            (output1 - 0.1).abs() < 0.001,
            "First D output should be 0.1: got {}",
            output1
        );

        // Second call with larger error
        let output2 = pid.compute(0.2, 1.0);
        // D = 1.0 * (0.2 - 0.1) / 1.0 = 0.1
        assert!(
            (output2 - 0.1).abs() < 0.001,
            "Second D output should be 0.1: got {}",
            output2
        );

        // Third call with same error (derivative should be 0)
        let output3 = pid.compute(0.2, 1.0);
        // D = 1.0 * (0.2 - 0.2) / 1.0 = 0.0
        assert!(
            output3.abs() < 0.001,
            "Third D output should be ~0: got {}",
            output3
        );
    }
}
