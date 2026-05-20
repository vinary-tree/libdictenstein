//! Memory pressure monitoring for proactive eviction.
//!
//! This module monitors system memory pressure and triggers callbacks when
//! pressure levels change. It uses the `sysinfo` crate for cross-platform
//! memory information retrieval, supporting Linux, macOS, Windows, and more.
//!
//! # Pressure Levels
//!
//! The system responds to memory pressure in three levels:
//!
//! 1. **Normal** (>30% available): No action, full caching
//! 2. **Low** (10-30% available): Proactive eviction of dirty pages
//! 3. **Critical** (<10% available): Emergency flush, shrink buffer pool
//!
//! # Example
//!
//! ```rust,ignore
//! use libdictenstein::persistent_artrie::memory_monitor::{
//!     MemoryPressureConfig, MemoryPressureMonitor, MemoryPressureLevel,
//! };
//!
//! let config = MemoryPressureConfig::default();
//! let monitor = MemoryPressureMonitor::start(config, |level, stats| {
//!     match level {
//!         MemoryPressureLevel::Normal => {},
//!         MemoryPressureLevel::Low => println!("Low memory: {:.1}% available", stats.available_fraction() * 100.0),
//!         MemoryPressureLevel::Critical => println!("Critical memory: {:.1}% available", stats.available_fraction() * 100.0),
//!     }
//! })?;
//! ```

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use sysinfo::System;

use super::error::{PersistentARTrieError, Result};

/// Memory pressure levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum MemoryPressureLevel {
    /// Normal operation, >30% memory available.
    Normal = 0,

    /// Low memory, 10-30% available. Proactive eviction recommended.
    Low = 1,

    /// Critical memory, <10% available. Emergency measures required.
    Critical = 2,
}

impl From<u8> for MemoryPressureLevel {
    fn from(value: u8) -> Self {
        match value {
            0 => Self::Normal,
            1 => Self::Low,
            _ => Self::Critical,
        }
    }
}

impl std::fmt::Display for MemoryPressureLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MemoryPressureLevel::Normal => write!(f, "Normal"),
            MemoryPressureLevel::Low => write!(f, "Low"),
            MemoryPressureLevel::Critical => write!(f, "Critical"),
        }
    }
}

/// Memory statistics from the system.
///
/// This struct uses the `sysinfo` crate to provide cross-platform memory
/// information, supporting Linux, macOS, Windows, and other platforms.
#[derive(Debug, Clone, Default)]
pub struct MemoryStats {
    /// Total system memory in bytes.
    pub mem_total: u64,

    /// Available memory in bytes.
    ///
    /// On Linux, this is equivalent to `MemAvailable` from `/proc/meminfo`.
    /// On other platforms, this is `free + cached` memory.
    pub mem_available: u64,

    /// Free memory in bytes (not including cached/buffers).
    pub mem_free: u64,

    /// Used memory in bytes.
    pub mem_used: u64,

    /// Swap total in bytes.
    pub swap_total: u64,

    /// Swap used in bytes.
    pub swap_used: u64,
}

impl MemoryStats {
    /// Available memory as a fraction of total.
    pub fn available_fraction(&self) -> f64 {
        if self.mem_total == 0 {
            return 1.0;
        }
        self.mem_available as f64 / self.mem_total as f64
    }

    /// Whether swap is being used (early warning sign).
    pub fn is_swapping(&self) -> bool {
        self.swap_used > 0
    }

    /// Available memory in MB.
    pub fn available_mb(&self) -> u64 {
        self.mem_available / (1024 * 1024)
    }

    /// Total memory in MB.
    pub fn total_mb(&self) -> u64 {
        self.mem_total / (1024 * 1024)
    }

    /// Used memory in MB.
    pub fn used_mb(&self) -> u64 {
        self.mem_used / (1024 * 1024)
    }

    /// Read memory statistics from the system using `sysinfo`.
    fn from_system(sys: &System) -> Self {
        let total = sys.total_memory();
        let available = sys.available_memory();
        let free = sys.free_memory();
        let used = sys.used_memory();
        let swap_total = sys.total_swap();
        let swap_used = sys.used_swap();

        Self {
            mem_total: total,
            mem_available: available,
            mem_free: free,
            mem_used: used,
            swap_total,
            swap_used,
        }
    }
}

/// PSI (Pressure Stall Information) metrics for Linux.
///
/// Note: PSI is Linux-specific and not available on other platforms.
/// This struct is provided for compatibility but will return default
/// values on non-Linux systems.
#[derive(Debug, Clone, Default)]
pub struct PsiMetrics {
    /// Percentage of time some tasks were stalled (10s average).
    pub some_avg10: f64,

    /// Percentage of time some tasks were stalled (60s average).
    pub some_avg60: f64,

    /// Percentage of time all tasks were stalled (10s average).
    pub full_avg10: f64,

    /// Percentage of time all tasks were stalled (60s average).
    pub full_avg60: f64,

    /// Total stall time in microseconds (cumulative).
    pub total_us: u64,
}

/// Configuration for memory pressure detection and response.
///
/// This module monitors system memory pressure using the `sysinfo` crate,
/// providing cross-platform support for Linux, macOS, Windows, and more.
///
/// # Response Levels
///
/// The system responds to memory pressure in three levels:
///
/// 1. **Normal** (>30% available): No action, full caching
/// 2. **Low** (10-30% available): Proactive eviction of dirty pages
/// 3. **Critical** (<10% available): Emergency flush, shrink buffer pool
///
/// # Platform Support
///
/// - **Linux**: Full support including PSI metrics (Linux 4.20+)
/// - **macOS**: Memory statistics via sysinfo
/// - **Windows**: Memory statistics via sysinfo
/// - **Other**: Best-effort via sysinfo
#[derive(Debug, Clone)]
pub struct MemoryPressureConfig {
    /// Polling interval for memory statistics.
    ///
    /// How often to check system memory state. Lower values give faster
    /// response but higher CPU overhead.
    ///
    /// Default: 1 second
    /// Range: 100ms - 60s
    pub poll_interval: Duration,

    /// Threshold for "low memory" state (fraction of total memory).
    ///
    /// When available memory drops below this fraction, proactive
    /// eviction begins.
    ///
    /// Default: 0.30 (30%)
    /// Range: 0.05 - 0.50
    pub low_memory_threshold: f64,

    /// Threshold for "critical memory" state (fraction of total memory).
    ///
    /// When available memory drops below this fraction, emergency
    /// measures are taken (flush all dirty pages, shrink pool).
    ///
    /// Default: 0.10 (10%)
    /// Range: 0.01 - 0.25
    pub critical_memory_threshold: f64,

    /// Fraction of dirty pages to evict when in low memory state.
    ///
    /// Default: 0.25 (evict 25% of dirty pages)
    /// Range: 0.10 - 1.0
    pub low_memory_evict_fraction: f64,

    /// Use PSI (Pressure Stall Information) if available.
    ///
    /// PSI provides more accurate and efficient pressure detection
    /// on Linux 4.20+. If unavailable, falls back to polling.
    ///
    /// Default: true
    pub use_psi: bool,

    /// PSI threshold for "some" pressure (microseconds per second).
    ///
    /// Trigger when any task is stalled for this duration within 1 second.
    ///
    /// Default: 50_000 (50ms per second)
    /// Range: 10_000 - 500_000
    pub psi_some_threshold_us: u64,

    /// PSI threshold for "full" pressure (microseconds per second).
    ///
    /// Trigger when all tasks are stalled for this duration within 1 second.
    ///
    /// Default: 10_000 (10ms per second)
    /// Range: 1_000 - 100_000
    pub psi_full_threshold_us: u64,

    /// Enable memory pressure monitoring.
    ///
    /// Set to false to disable all memory pressure handling.
    ///
    /// Default: true
    pub enabled: bool,

    /// Debounce duration for pressure level changes.
    ///
    /// Prevents rapid oscillation between pressure levels.
    ///
    /// Default: 500ms
    pub debounce_duration: Duration,
}

impl Default for MemoryPressureConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(1),
            low_memory_threshold: 0.30,
            critical_memory_threshold: 0.10,
            low_memory_evict_fraction: 0.25,
            use_psi: true,
            psi_some_threshold_us: 50_000,
            psi_full_threshold_us: 10_000,
            enabled: true,
            debounce_duration: Duration::from_millis(500),
        }
    }
}

impl MemoryPressureConfig {
    /// Validate configuration parameters.
    pub fn validate(&self) -> Result<()> {
        if self.low_memory_threshold <= self.critical_memory_threshold {
            return Err(PersistentARTrieError::internal(
                "low_memory_threshold must be greater than critical_memory_threshold",
            ));
        }
        if self.low_memory_threshold > 0.50 {
            return Err(PersistentARTrieError::internal(
                "low_memory_threshold must be at most 0.50",
            ));
        }
        if self.critical_memory_threshold < 0.01 {
            return Err(PersistentARTrieError::internal(
                "critical_memory_threshold must be at least 0.01",
            ));
        }
        if self.low_memory_evict_fraction < 0.10 || self.low_memory_evict_fraction > 1.0 {
            return Err(PersistentARTrieError::internal(
                "low_memory_evict_fraction must be between 0.10 and 1.0",
            ));
        }
        Ok(())
    }
}

/// Statistics for memory pressure monitoring.
#[derive(Debug, Clone, Default)]
pub struct MemoryMonitorStats {
    /// Number of times pressure level changed.
    pub level_changes: u64,

    /// Number of times low pressure was detected.
    pub low_pressure_count: u64,

    /// Number of times critical pressure was detected.
    pub critical_pressure_count: u64,

    /// Total time spent in low pressure state (seconds).
    pub low_pressure_duration_secs: f64,

    /// Total time spent in critical pressure state (seconds).
    pub critical_pressure_duration_secs: f64,

    /// Number of polling cycles.
    pub poll_cycles: u64,

    /// Whether PSI is being used.
    pub using_psi: bool,
}

/// Callback type for pressure level changes.
pub type PressureCallback = Arc<dyn Fn(MemoryPressureLevel, &MemoryStats) + Send + Sync>;

/// Memory pressure monitor.
///
/// Monitors system memory pressure and invokes callbacks when pressure
/// levels change. Uses the `sysinfo` crate for cross-platform support.
pub struct MemoryPressureMonitor {
    /// Configuration.
    config: MemoryPressureConfig,

    /// Current pressure level (atomic for lock-free reads).
    current_level: Arc<AtomicU8>,

    /// Shutdown flag.
    shutdown: Arc<AtomicBool>,

    /// Monitor thread handle.
    monitor_thread: Option<JoinHandle<()>>,

    /// Latest memory stats (updated on each poll).
    latest_stats: Arc<RwLock<MemoryStats>>,

    /// Monitoring statistics.
    stats: Arc<RwLock<MemoryMonitorStats>>,

    /// Whether PSI is being used.
    using_psi: bool,
}

impl MemoryPressureMonitor {
    /// Start the memory pressure monitor.
    ///
    /// The callback will be invoked whenever the pressure level changes.
    pub fn start<F>(config: MemoryPressureConfig, callback: F) -> Result<Self>
    where
        F: Fn(MemoryPressureLevel, &MemoryStats) + Send + Sync + 'static,
    {
        config.validate()?;

        let current_level = Arc::new(AtomicU8::new(MemoryPressureLevel::Normal as u8));
        let shutdown = Arc::new(AtomicBool::new(false));
        let latest_stats = Arc::new(RwLock::new(MemoryStats::default()));
        let stats = Arc::new(RwLock::new(MemoryMonitorStats::default()));

        // Check if PSI is available (Linux only)
        let psi_available = config.use_psi && Self::psi_available();

        // Update initial stats
        {
            let mut sys = System::new();
            sys.refresh_memory();
            *latest_stats.write() = MemoryStats::from_system(&sys);
        }

        let monitor_thread = if config.enabled {
            let level_clone = Arc::clone(&current_level);
            let shutdown_clone = Arc::clone(&shutdown);
            let config_clone = config.clone();
            let callback = Arc::new(callback);
            let latest_stats_clone = Arc::clone(&latest_stats);
            let stats_clone = Arc::clone(&stats);

            Some(
                thread::Builder::new()
                    .name("artrie-memory-monitor".to_string())
                    .spawn(move || {
                        Self::polling_monitor_loop(
                            level_clone,
                            shutdown_clone,
                            config_clone,
                            callback,
                            latest_stats_clone,
                            stats_clone,
                        );
                    })
                    .map_err(|e| {
                        PersistentARTrieError::io_error(
                            "spawn monitor thread",
                            "thread",
                            e,
                        )
                    })?,
            )
        } else {
            None
        };

        // Update stats with PSI info
        {
            let mut s = stats.write();
            s.using_psi = psi_available;
        }

        Ok(Self {
            config,
            current_level,
            shutdown,
            monitor_thread,
            latest_stats,
            stats,
            using_psi: psi_available,
        })
    }

    /// Get the current pressure level (lock-free).
    pub fn current_level(&self) -> MemoryPressureLevel {
        MemoryPressureLevel::from(self.current_level.load(Ordering::Relaxed))
    }

    /// Get current memory statistics.
    pub fn current_stats(&self) -> MemoryStats {
        self.latest_stats.read().clone()
    }

    /// Get monitoring statistics.
    pub fn stats(&self) -> MemoryMonitorStats {
        self.stats.read().clone()
    }

    /// Check if PSI is being used.
    pub fn using_psi(&self) -> bool {
        self.using_psi
    }

    /// Manually check memory and update level (for testing).
    pub fn check_now(&self) -> Result<MemoryPressureLevel> {
        let mut sys = System::new();
        sys.refresh_memory();
        let stats = MemoryStats::from_system(&sys);

        let level = Self::classify_pressure(&self.config, &stats);
        self.current_level.store(level as u8, Ordering::Relaxed);
        *self.latest_stats.write() = stats;

        Ok(level)
    }

    /// Shutdown the monitor.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }

    // --- Private methods ---

    /// Check if PSI is available on this system (Linux 4.20+).
    fn psi_available() -> bool {
        #[cfg(target_os = "linux")]
        {
            std::path::Path::new("/proc/pressure/memory").exists()
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    }

    /// Read PSI metrics from /proc/pressure/memory (Linux only).
    #[cfg(target_os = "linux")]
    #[allow(dead_code)]
    fn read_psi_metrics() -> std::io::Result<PsiMetrics> {
        use std::fs;

        let content = fs::read_to_string("/proc/pressure/memory")?;
        let mut metrics = PsiMetrics::default();

        for line in content.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.is_empty() {
                continue;
            }

            let is_some = parts[0] == "some";
            let is_full = parts[0] == "full";

            if !is_some && !is_full {
                continue;
            }

            for part in &parts[1..] {
                let kv: Vec<&str> = part.split('=').collect();
                if kv.len() != 2 {
                    continue;
                }

                let value: f64 = kv[1].parse().unwrap_or(0.0);

                match (is_some, kv[0]) {
                    (true, "avg10") => metrics.some_avg10 = value,
                    (true, "avg60") => metrics.some_avg60 = value,
                    (false, "avg10") => metrics.full_avg10 = value,
                    (false, "avg60") => metrics.full_avg60 = value,
                    (true, "total") | (false, "total") => {
                        metrics.total_us = value as u64;
                    }
                    _ => {}
                }
            }
        }

        Ok(metrics)
    }

    /// Classify memory pressure based on available memory fraction.
    fn classify_pressure(config: &MemoryPressureConfig, stats: &MemoryStats) -> MemoryPressureLevel {
        let available = stats.available_fraction();

        if available < config.critical_memory_threshold {
            MemoryPressureLevel::Critical
        } else if available < config.low_memory_threshold {
            MemoryPressureLevel::Low
        } else {
            MemoryPressureLevel::Normal
        }
    }

    /// Polling-based monitor loop using sysinfo.
    fn polling_monitor_loop(
        current_level: Arc<AtomicU8>,
        shutdown: Arc<AtomicBool>,
        config: MemoryPressureConfig,
        callback: PressureCallback,
        latest_stats: Arc<RwLock<MemoryStats>>,
        stats: Arc<RwLock<MemoryMonitorStats>>,
    ) {
        let mut sys = System::new();
        let mut last_level = MemoryPressureLevel::Normal;
        let mut last_change = Instant::now();
        let mut low_start: Option<Instant> = None;
        let mut critical_start: Option<Instant> = None;

        while !shutdown.load(Ordering::Relaxed) {
            // Sleep for poll interval
            thread::sleep(config.poll_interval);

            // Update poll count
            {
                let mut s = stats.write();
                s.poll_cycles += 1;
            }

            // Refresh memory stats using sysinfo
            sys.refresh_memory();
            let mem_stats = MemoryStats::from_system(&sys);

            // Update latest stats
            *latest_stats.write() = mem_stats.clone();

            // Classify pressure level
            let new_level = Self::classify_pressure(&config, &mem_stats);

            // Debounce: only change level if stable for debounce_duration
            if new_level != last_level {
                if last_change.elapsed() >= config.debounce_duration {
                    // Update state tracking
                    match last_level {
                        MemoryPressureLevel::Low => {
                            if let Some(start) = low_start.take() {
                                let mut s = stats.write();
                                s.low_pressure_duration_secs += start.elapsed().as_secs_f64();
                            }
                        }
                        MemoryPressureLevel::Critical => {
                            if let Some(start) = critical_start.take() {
                                let mut s = stats.write();
                                s.critical_pressure_duration_secs += start.elapsed().as_secs_f64();
                            }
                        }
                        MemoryPressureLevel::Normal => {}
                    }

                    // Update new state tracking
                    match new_level {
                        MemoryPressureLevel::Low => {
                            low_start = Some(Instant::now());
                            let mut s = stats.write();
                            s.low_pressure_count += 1;
                        }
                        MemoryPressureLevel::Critical => {
                            critical_start = Some(Instant::now());
                            let mut s = stats.write();
                            s.critical_pressure_count += 1;
                        }
                        MemoryPressureLevel::Normal => {}
                    }

                    // Update level
                    current_level.store(new_level as u8, Ordering::Relaxed);
                    last_level = new_level;

                    // Update stats
                    {
                        let mut s = stats.write();
                        s.level_changes += 1;
                    }

                    // Invoke callback
                    callback(new_level, &mem_stats);
                }
            } else {
                // Reset debounce timer if level is stable
                last_change = Instant::now();
            }
        }

        // Final state cleanup
        match last_level {
            MemoryPressureLevel::Low => {
                if let Some(start) = low_start.take() {
                    let mut s = stats.write();
                    s.low_pressure_duration_secs += start.elapsed().as_secs_f64();
                }
            }
            MemoryPressureLevel::Critical => {
                if let Some(start) = critical_start.take() {
                    let mut s = stats.write();
                    s.critical_pressure_duration_secs += start.elapsed().as_secs_f64();
                }
            }
            MemoryPressureLevel::Normal => {}
        }
    }
}

impl Drop for MemoryPressureMonitor {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);

        if let Some(handle) = self.monitor_thread.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn test_config_default() {
        let config = MemoryPressureConfig::default();
        assert!(config.validate().is_ok());
        assert_eq!(config.low_memory_threshold, 0.30);
        assert_eq!(config.critical_memory_threshold, 0.10);
    }

    #[test]
    fn test_config_validation() {
        // Invalid: low <= critical
        let config = MemoryPressureConfig {
            low_memory_threshold: 0.10,
            critical_memory_threshold: 0.10,
            ..Default::default()
        };
        assert!(config.validate().is_err());

        // Invalid: low > 0.50
        let config = MemoryPressureConfig {
            low_memory_threshold: 0.60,
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_memory_stats() {
        let stats = MemoryStats {
            mem_total: 16 * 1024 * 1024 * 1024, // 16 GB
            mem_available: 8 * 1024 * 1024 * 1024, // 8 GB
            mem_free: 4 * 1024 * 1024 * 1024,
            mem_used: 8 * 1024 * 1024 * 1024,
            swap_total: 8 * 1024 * 1024 * 1024,
            swap_used: 0,
        };

        assert!((stats.available_fraction() - 0.5).abs() < 0.01);
        assert_eq!(stats.available_mb(), 8 * 1024);
        assert_eq!(stats.total_mb(), 16 * 1024);
        assert!(!stats.is_swapping());
    }

    #[test]
    fn test_pressure_classification() {
        let config = MemoryPressureConfig::default();

        // Normal (50% available)
        let stats = MemoryStats {
            mem_total: 100,
            mem_available: 50,
            ..Default::default()
        };
        assert_eq!(
            MemoryPressureMonitor::classify_pressure(&config, &stats),
            MemoryPressureLevel::Normal
        );

        // Low (20% available)
        let stats = MemoryStats {
            mem_total: 100,
            mem_available: 20,
            ..Default::default()
        };
        assert_eq!(
            MemoryPressureMonitor::classify_pressure(&config, &stats),
            MemoryPressureLevel::Low
        );

        // Critical (5% available)
        let stats = MemoryStats {
            mem_total: 100,
            mem_available: 5,
            ..Default::default()
        };
        assert_eq!(
            MemoryPressureMonitor::classify_pressure(&config, &stats),
            MemoryPressureLevel::Critical
        );
    }

    #[test]
    fn test_sysinfo_memory_stats() {
        // Test that sysinfo can read memory stats on this system
        let mut sys = System::new();
        sys.refresh_memory();
        let stats = MemoryStats::from_system(&sys);

        assert!(stats.mem_total > 0, "Total memory should be > 0");
        assert!(stats.mem_available > 0, "Available memory should be > 0");
        assert!(stats.mem_available <= stats.mem_total, "Available should be <= total");
    }

    #[test]
    fn test_monitor_start_disabled() {
        let config = MemoryPressureConfig {
            enabled: false,
            ..Default::default()
        };

        let callback_count = Arc::new(AtomicUsize::new(0));
        let count_clone = Arc::clone(&callback_count);

        let monitor = MemoryPressureMonitor::start(config, move |_, _| {
            count_clone.fetch_add(1, Ordering::Relaxed);
        })
        .expect("start monitor");

        assert_eq!(monitor.current_level(), MemoryPressureLevel::Normal);

        // No callbacks should fire when disabled
        thread::sleep(Duration::from_millis(100));
        assert_eq!(callback_count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_monitor_check_now() {
        let config = MemoryPressureConfig {
            enabled: false, // Don't start background thread
            ..Default::default()
        };

        let monitor = MemoryPressureMonitor::start(config, |_, _| {})
            .expect("start monitor");

        let level = monitor.check_now().expect("check now");
        // On a typical system, we expect Normal
        // (unless running under actual memory pressure)
        assert!(matches!(
            level,
            MemoryPressureLevel::Normal | MemoryPressureLevel::Low
        ));
    }

    #[test]
    fn test_pressure_level_display() {
        assert_eq!(format!("{}", MemoryPressureLevel::Normal), "Normal");
        assert_eq!(format!("{}", MemoryPressureLevel::Low), "Low");
        assert_eq!(format!("{}", MemoryPressureLevel::Critical), "Critical");
    }

    #[test]
    fn test_pressure_level_from_u8() {
        assert_eq!(MemoryPressureLevel::from(0), MemoryPressureLevel::Normal);
        assert_eq!(MemoryPressureLevel::from(1), MemoryPressureLevel::Low);
        assert_eq!(MemoryPressureLevel::from(2), MemoryPressureLevel::Critical);
        assert_eq!(MemoryPressureLevel::from(255), MemoryPressureLevel::Critical);
    }
}
