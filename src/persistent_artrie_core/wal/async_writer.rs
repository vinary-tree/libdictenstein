//! Background segment-sync manager + async-capable WAL writer + the
//! `collect_all_segments` recovery helper.
//!
//! Split out of the monolithic `wal.rs` (lines ~211-1145, ~935 LOC) as the
//! final Phase-4 wal extraction. The three items must move together because
//! `SegmentSyncManager` and `AsyncWalWriter` share private state across the
//! rotation lifecycle, and `collect_all_segments` is the recovery-side
//! companion that walks the same archive + pending + active directory
//! layout.

use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::{
    AsyncWalConfig, AsyncWalError, Lsn, PendingSegment, StdFsync, SyncHandle, WalConfig, WalError,
    WalHeader, WalRecord, WalSyncBackend, WalWriter,
};

/// Manages background segment synchronization.
///
/// The sync manager owns a background thread that processes pending segments
/// in strict FIFO order, ensuring that `global_synced_lsn` always represents
/// a contiguous range from LSN 1 (no gaps).
///
/// # Ordering Guarantee
///
/// The single sync thread + FIFO queue ensures that if segment B was rotated
/// after segment A, then A's fsync completes before B's. This prevents the
/// situation where B syncs first and we incorrectly report A's LSNs as durable.
pub struct SegmentSyncManager {
    /// Queue of segments awaiting sync (oldest first).
    pending_segments: Mutex<VecDeque<PendingSegment>>,
    /// Total bytes in pending segments (for backpressure).
    pending_bytes: AtomicU64,
    /// The highest LSN that is confirmed durable across all synced segments.
    ///
    /// This value always represents a contiguous range: all LSNs from 1 to
    /// `global_synced_lsn` are durable. No gaps are possible due to FIFO
    /// processing.
    pub global_synced_lsn: AtomicU64,
    /// Condvar to notify waiters when a segment is synced.
    sync_complete: Condvar,
    /// Mutex for condvar wait.
    sync_mutex: Mutex<()>,
    /// Flag to signal the sync thread to stop.
    running: AtomicBool,
    /// Handle to the background sync thread.
    sync_thread: Mutex<Option<JoinHandle<()>>>,
    /// Configuration.
    config: AsyncWalConfig,
    /// Archive configuration for moving synced segments.
    archive_config: WalConfig,
    /// Path to the active WAL (for archive directory resolution).
    wal_path: PathBuf,
    /// Backend for performing durable fsync operations.
    ///
    /// Defaults to `StdFsync` (standard `file.sync_all()`).
    /// Can be replaced with `IoUringFsync` for io_uring-based fsync.
    sync_backend: Arc<dyn WalSyncBackend>,
}

impl SegmentSyncManager {
    /// Create a new sync manager and start the background thread.
    ///
    /// Uses `StdFsync` (standard `file.sync_all()`) as the sync backend.
    pub fn new(
        config: AsyncWalConfig,
        archive_config: WalConfig,
        wal_path: PathBuf,
        initial_synced_lsn: Lsn,
    ) -> Arc<Self> {
        Self::with_sync_backend(
            config,
            archive_config,
            wal_path,
            initial_synced_lsn,
            Arc::new(StdFsync),
        )
    }

    /// Create a new sync manager with a custom sync backend.
    pub fn with_sync_backend(
        config: AsyncWalConfig,
        archive_config: WalConfig,
        wal_path: PathBuf,
        initial_synced_lsn: Lsn,
        sync_backend: Arc<dyn WalSyncBackend>,
    ) -> Arc<Self> {
        let manager = Arc::new(Self {
            pending_segments: Mutex::new(VecDeque::new()),
            pending_bytes: AtomicU64::new(0),
            global_synced_lsn: AtomicU64::new(initial_synced_lsn),
            sync_complete: Condvar::new(),
            sync_mutex: Mutex::new(()),
            running: AtomicBool::new(true),
            sync_thread: Mutex::new(None),
            config,
            archive_config,
            wal_path,
            sync_backend,
        });

        // The worker holds only a `Weak` ref so it can never keep the manager
        // alive past its owner's drop. Previously it captured a strong
        // `Arc::clone(&manager)`; combined with `running` being cleared only in
        // `stop()`/`Drop`, that was a self-sustaining cycle (the thread kept the
        // Arc alive, so `Drop`/`stop` never ran, so the thread looped forever,
        // leaking the OS thread). Upgrade per iteration; exit when the owner is
        // gone or `running` is cleared, releasing the strong ref before sleeping.
        let idle_ms = manager.config.idle_check_interval_ms;
        let weak = Arc::downgrade(&manager);
        let handle = thread::Builder::new()
            .name("wal-sync".to_string())
            .spawn(move || {
                loop {
                    let Some(this) = weak.upgrade() else { break };
                    if !this.running.load(Ordering::Relaxed) {
                        break;
                    }
                    let did_work = this.sync_once();
                    drop(this); // release the strong ref BEFORE the idle sleep
                    if !did_work {
                        thread::sleep(Duration::from_millis(idle_ms));
                    }
                }
            })
            .expect("Failed to spawn WAL sync thread");

        *manager
            .sync_thread
            .lock()
            .expect("sync_thread lock poisoned") = Some(handle);

        manager
    }

    /// Enqueue a segment for background sync.
    pub fn enqueue(&self, segment: PendingSegment) {
        let size = segment.size_bytes;
        let mut queue = self
            .pending_segments
            .lock()
            .expect("pending_segments lock poisoned");
        queue.push_back(segment);
        self.pending_bytes.fetch_add(size, Ordering::AcqRel);
    }

    /// Get the number of pending segments.
    pub fn pending_count(&self) -> usize {
        self.pending_segments
            .lock()
            .expect("pending_segments lock poisoned")
            .len()
    }

    /// Get the total bytes in pending segments.
    pub fn pending_bytes(&self) -> u64 {
        self.pending_bytes.load(Ordering::Acquire)
    }

    /// Wait until the pending count drops below the limit.
    pub fn wait_for_backpressure(&self) -> Result<(), AsyncWalError> {
        loop {
            let count = self.pending_count();
            let bytes = self.pending_bytes();

            if count < self.config.max_pending_segments && bytes < self.config.max_pending_bytes {
                return Ok(());
            }

            if !self.running.load(Ordering::Acquire) {
                return Err(AsyncWalError::SyncThreadPanicked);
            }

            let guard = self.sync_mutex.lock().expect("sync_mutex lock poisoned");
            let _ = self
                .sync_complete
                .wait_timeout(guard, Duration::from_millis(100));
        }
    }

    /// Wait for a specific LSN to be synced.
    pub fn wait_for_lsn(&self, target_lsn: Lsn) -> Result<(), AsyncWalError> {
        loop {
            if self.global_synced_lsn.load(Ordering::Acquire) >= target_lsn {
                return Ok(());
            }

            if !self.running.load(Ordering::Acquire) {
                if self.global_synced_lsn.load(Ordering::Acquire) >= target_lsn {
                    return Ok(());
                }
                return Err(AsyncWalError::SyncThreadPanicked);
            }

            let guard = self.sync_mutex.lock().expect("sync_mutex lock poisoned");
            let _ = self
                .sync_complete
                .wait_timeout(guard, Duration::from_millis(100));
        }
    }

    /// Wait for a specific LSN to be synced with timeout.
    pub fn wait_for_lsn_timeout(
        &self,
        target_lsn: Lsn,
        timeout: Duration,
    ) -> Result<bool, AsyncWalError> {
        let deadline = Instant::now() + timeout;

        loop {
            if self.global_synced_lsn.load(Ordering::Acquire) >= target_lsn {
                return Ok(true);
            }

            if !self.running.load(Ordering::Acquire) {
                if self.global_synced_lsn.load(Ordering::Acquire) >= target_lsn {
                    return Ok(true);
                }
                return Err(AsyncWalError::SyncThreadPanicked);
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(false);
            }

            let guard = self.sync_mutex.lock().expect("sync_mutex lock poisoned");
            let _ = self
                .sync_complete
                .wait_timeout(guard, remaining.min(Duration::from_millis(100)));
        }
    }

    /// Process at most one pending segment.
    ///
    /// Returns `true` if a segment was dequeued (and synced + archived),
    /// `false` if the queue was empty (the caller then performs the idle
    /// sleep). Extracted from the old `sync_loop` so the spawned worker can
    /// drive the loop through a `Weak` handle and release its strong ref
    /// before the idle sleep — see `with_sync_backend`.
    fn sync_once(&self) -> bool {
        let segment = {
            let mut queue = self
                .pending_segments
                .lock()
                .expect("pending_segments lock poisoned");
            queue.pop_front()
        };

        let Some(segment) = segment else {
            return false;
        };

        let size = segment.size_bytes;
        let lsn_end = segment.lsn_range.1;
        let path = segment.path.clone();

        let mut attempts = 0u32;
        loop {
            attempts += 1;
            match self.sync_backend.sync_file(&segment.file) {
                Ok(()) => {
                    log::debug!(
                        "Synced segment {} (LSN {}-{}) in {} attempts",
                        path.display(),
                        segment.lsn_range.0,
                        lsn_end,
                        attempts
                    );
                    break;
                }
                Err(e) => {
                    log::error!(
                        "Sync failed for {} (attempt {}): {:?}",
                        path.display(),
                        attempts,
                        e
                    );
                    thread::sleep(Duration::from_millis(100));

                    if attempts >= 10 {
                        log::error!(
                            "WARNING: {} sync attempts failed for {}. Will keep retrying.",
                            attempts,
                            path.display()
                        );
                    }
                }
            }

            if !self.running.load(Ordering::Relaxed) {
                log::warn!(
                    "Sync thread stopping with unsynced segment: {}",
                    path.display()
                );
                return true;
            }
        }

        self.pending_bytes.fetch_sub(size, Ordering::AcqRel);
        self.global_synced_lsn.store(lsn_end, Ordering::Release);

        if self.archive_config.archive_enabled {
            let archive_dir = if self.archive_config.archive_dir.is_absolute() {
                self.archive_config.archive_dir.clone()
            } else {
                self.wal_path
                    .parent()
                    .unwrap_or(Path::new("."))
                    .join(&self.archive_config.archive_dir)
            };

            if let Err(e) = fs::create_dir_all(&archive_dir) {
                log::warn!("Failed to create archive directory: {}", e);
            } else {
                let archive_path = WalWriter::unique_archive_segment_path(&archive_dir);

                if let Err(e) = fs::rename(&path, &archive_path) {
                    log::warn!(
                        "Failed to move synced segment to archive: {} -> {}: {}",
                        path.display(),
                        archive_path.display(),
                        e
                    );
                } else if let Err(e) =
                    WalWriter::prune_segments_if_needed(&archive_dir, &self.archive_config)
                {
                    log::warn!("Failed to prune WAL archive segments: {}", e);
                }
            }
        } else {
            if let Err(e) = fs::remove_file(&path) {
                log::warn!("Failed to remove synced segment {}: {}", path.display(), e);
            }
        }

        let _guard = self.sync_mutex.lock().expect("sync_mutex lock poisoned");
        self.sync_complete.notify_all();
        true
    }

    /// Stop the background sync thread.
    pub fn stop(&self) {
        self.running.store(false, Ordering::Release);

        let _guard = self.sync_mutex.lock().expect("sync_mutex lock poisoned");
        self.sync_complete.notify_all();
        drop(_guard);

        let mut handle = self.sync_thread.lock().expect("sync_thread lock poisoned");
        if let Some(h) = handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for SegmentSyncManager {
    fn drop(&mut self) {
        self.stop();
    }
}

/// An async-capable WAL writer that allows writes during sync.
pub struct AsyncWalWriter {
    /// The underlying WAL writer.
    writer: Mutex<WalWriter>,
    /// Next LSN to assign (mirrors writer.next_lsn but allows non-blocking reads).
    next_lsn: AtomicU64,
    /// Last synced LSN (updated after each sync completes).
    synced_lsn: AtomicU64,
    /// Segment sync manager for background operations.
    sync_manager: Arc<SegmentSyncManager>,
    /// Configuration.
    config: AsyncWalConfig,
    /// Archive configuration.
    archive_config: WalConfig,
    /// Path to the WAL file.
    path: PathBuf,
    /// Counter for pending segment naming.
    pending_counter: AtomicU64,
}

impl AsyncWalWriter {
    /// Create a new async WAL file.
    pub fn create(
        path: impl AsRef<Path>,
        config: AsyncWalConfig,
        archive_config: WalConfig,
    ) -> Result<Self, AsyncWalError> {
        let path = path.as_ref().to_path_buf();

        let pending_dir = if config.pending_dir.is_absolute() {
            config.pending_dir.clone()
        } else {
            path.parent()
                .unwrap_or(Path::new("."))
                .join(&config.pending_dir)
        };
        fs::create_dir_all(&pending_dir).map_err(|e| AsyncWalError::RotationFailed {
            reason: "Failed to create pending directory".to_string(),
            source: Some(e),
        })?;

        let writer = WalWriter::create(&path)?;
        let sync_manager =
            SegmentSyncManager::new(config.clone(), archive_config.clone(), path.clone(), 0);

        Ok(Self {
            next_lsn: AtomicU64::new(writer.current_lsn()),
            synced_lsn: AtomicU64::new(writer.synced_lsn()),
            writer: Mutex::new(writer),
            sync_manager,
            config,
            archive_config,
            path,
            pending_counter: AtomicU64::new(0),
        })
    }

    /// Open an existing async WAL file.
    pub fn open(
        path: impl AsRef<Path>,
        config: AsyncWalConfig,
        archive_config: WalConfig,
    ) -> Result<Self, AsyncWalError> {
        let path = path.as_ref().to_path_buf();

        let pending_dir = if config.pending_dir.is_absolute() {
            config.pending_dir.clone()
        } else {
            path.parent()
                .unwrap_or(Path::new("."))
                .join(&config.pending_dir)
        };
        fs::create_dir_all(&pending_dir).map_err(|e| AsyncWalError::RotationFailed {
            reason: "Failed to create pending directory".to_string(),
            source: Some(e),
        })?;

        let writer = WalWriter::open(&path)?;
        let mut segments_for_lsn =
            collect_all_segments(&path, &archive_config, &config).unwrap_or_else(|_| Vec::new());
        if !segments_for_lsn.is_empty() {
            WalWriter::sort_segments_by_first_lsn(&mut segments_for_lsn);
            if let Some(max_lsn) = WalWriter::max_lsn_in_segments(&segments_for_lsn) {
                writer.set_min_lsn(max_lsn.saturating_add(1));
                writer.set_min_synced_lsn(max_lsn);
            }
        }
        let synced_lsn = writer.synced_lsn();
        let sync_manager = SegmentSyncManager::new(
            config.clone(),
            archive_config.clone(),
            path.clone(),
            synced_lsn,
        );

        Ok(Self {
            next_lsn: AtomicU64::new(writer.current_lsn()),
            synced_lsn: AtomicU64::new(synced_lsn),
            writer: Mutex::new(writer),
            sync_manager,
            config,
            archive_config,
            path,
            pending_counter: AtomicU64::new(0),
        })
    }

    /// Open or create an async WAL file.
    pub fn open_or_create(
        path: impl AsRef<Path>,
        config: AsyncWalConfig,
        archive_config: WalConfig,
    ) -> Result<Self, AsyncWalError> {
        let path = path.as_ref();
        if path.exists() {
            Self::open(path, config, archive_config)
        } else {
            Self::create(path, config, archive_config)
        }
    }

    /// Append a record to the WAL.
    pub fn append(&self, record: WalRecord) -> Result<Lsn, AsyncWalError> {
        let writer = self.writer.lock().expect("WAL writer lock poisoned");
        let lsn = writer.append(record)?;
        self.next_lsn
            .fetch_max(writer.current_lsn(), Ordering::AcqRel);
        Ok(lsn)
    }

    /// Append a record using an LSN that was reserved by `allocate_lsn`.
    #[cfg(feature = "group-commit")]
    pub(crate) fn append_with_lsn(
        &self,
        lsn: Lsn,
        record: WalRecord,
    ) -> Result<Lsn, AsyncWalError> {
        let writer = self.writer.lock().expect("WAL writer lock poisoned");
        let written_lsn = writer.append_with_lsn(lsn, record)?;
        self.next_lsn
            .fetch_max(writer.current_lsn(), Ordering::AcqRel);
        Ok(written_lsn)
    }

    /// Append a batch of inserts as a single WAL record.
    pub fn append_batch(
        &self,
        entries: &[(Vec<u8>, Option<Vec<u8>>)],
    ) -> Result<Lsn, AsyncWalError> {
        let writer = self.writer.lock().expect("WAL writer lock poisoned");
        let lsn = writer.append_batch(entries)?;
        self.next_lsn
            .fetch_max(writer.current_lsn(), Ordering::AcqRel);
        Ok(lsn)
    }

    /// Initiate an async sync and return a handle to track completion.
    pub fn sync_async(&self) -> Result<SyncHandle, AsyncWalError> {
        let current_lsn = self.next_lsn.load(Ordering::Acquire).saturating_sub(1);
        let synced_lsn = self.sync_manager.global_synced_lsn.load(Ordering::Acquire);

        if current_lsn <= synced_lsn {
            return Ok(SyncHandle::already_synced(
                current_lsn,
                Arc::clone(&self.sync_manager),
            ));
        }

        self.sync_manager.wait_for_backpressure()?;

        self.rotate_for_sync(current_lsn)?;

        Ok(SyncHandle::new(current_lsn, Arc::clone(&self.sync_manager)))
    }

    /// Blocking sync — waits for all current data to be durable.
    pub fn sync(&self) -> Result<Lsn, AsyncWalError> {
        let writer = self.writer.lock().expect("WAL writer lock poisoned");
        let lsn = writer.sync()?;
        self.synced_lsn.store(lsn, Ordering::Release);
        self.sync_manager
            .global_synced_lsn
            .store(lsn, Ordering::Release);
        Ok(lsn)
    }

    /// Sync with segment rotation for async writes during sync.
    pub fn sync_with_rotation(&self) -> Result<Lsn, AsyncWalError> {
        let handle = self.sync_async()?;
        handle.wait()?;
        Ok(handle.target_lsn())
    }

    /// Get the current (next) LSN.
    pub fn current_lsn(&self) -> Lsn {
        self.next_lsn.load(Ordering::Acquire)
    }

    /// Get the last synced LSN.
    pub fn synced_lsn(&self) -> Lsn {
        self.sync_manager.global_synced_lsn.load(Ordering::Acquire)
    }

    /// Allocate the next LSN without writing a record.
    pub fn allocate_lsn(&self) -> Lsn {
        self.next_lsn.fetch_add(1, Ordering::AcqRel)
    }

    /// Set the minimum starting LSN for subsequent records.
    pub fn set_min_lsn(&self, min_lsn: Lsn) {
        {
            let writer = self.writer.lock().expect("WAL writer lock poisoned");
            writer.set_min_lsn(min_lsn);
        }

        loop {
            let current = self.next_lsn.load(Ordering::Acquire);
            if current >= min_lsn {
                break;
            }
            if self
                .next_lsn
                .compare_exchange(current, min_lsn, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                break;
            }
        }
    }

    /// Returns `true` iff no records have been appended via THIS async writer
    /// (`next_lsn == 1`). The async writer owns its own `next_lsn` (the inner sync
    /// writer's counter may lag), so the regime-stamp guards must consult the async
    /// counter, not the sync writer's.
    pub fn is_empty_after_header(&self) -> bool {
        self.next_lsn.load(Ordering::Acquire) == 1
    }

    /// Stamp the WAL header to the Overlay regime (S4). **ENFORCED to be EMPTY**
    /// (S5-3) on the async counter, then delegates to the inner sync writer (which
    /// re-checks its own counter).
    pub fn set_overlay_regime(&self) -> Result<(), WalError> {
        if !self.is_empty_after_header() {
            return Err(WalError::InvalidRegimeStamp(
                "set_overlay_regime on a non-empty async WAL (records already appended)"
                    .to_string(),
            ));
        }
        let writer = self.writer.lock().expect("WAL writer lock poisoned");
        writer.set_overlay_regime()
    }

    /// The header's current rank-regime (S4).
    pub fn rank_regime(&self) -> super::RankRegime {
        let writer = self.writer.lock().expect("WAL writer lock poisoned");
        writer.rank_regime()
    }

    /// Stamp the WAL header BACK to the Owned regime (S5-4 kill-switch). **ENFORCED to
    /// be EMPTY** on the async counter, then delegates to the inner sync writer.
    pub fn set_owned_regime(&self) -> Result<(), WalError> {
        if !self.is_empty_after_header() {
            return Err(WalError::InvalidRegimeStamp(
                "set_owned_regime on a non-empty async WAL (records already appended)".to_string(),
            ));
        }
        let writer = self.writer.lock().expect("WAL writer lock poisoned");
        writer.set_owned_regime()
    }

    /// Get the path to the WAL file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Get the sync manager for advanced operations.
    pub fn sync_manager(&self) -> &Arc<SegmentSyncManager> {
        &self.sync_manager
    }

    /// Write a checkpoint record.
    pub fn checkpoint(&self, checkpoint_lsn: Lsn) -> Result<Lsn, AsyncWalError> {
        let writer = self.writer.lock().expect("WAL writer lock poisoned");
        let lsn = writer.checkpoint(checkpoint_lsn)?;
        self.next_lsn.store(writer.current_lsn(), Ordering::Release);
        Ok(lsn)
    }

    /// Truncate the WAL, discarding all records after the header.
    pub fn truncate(&self) -> Result<(), AsyncWalError> {
        let writer = self.writer.lock().expect("WAL writer lock poisoned");
        writer.truncate()?;
        self.next_lsn.store(writer.current_lsn(), Ordering::Release);
        let synced_lsn = writer.synced_lsn();
        self.synced_lsn.store(synced_lsn, Ordering::Release);
        self.sync_manager
            .global_synced_lsn
            .store(synced_lsn, Ordering::Release);
        Ok(())
    }

    /// Rotate WAL to archive directory — O(1) filesystem rename.
    pub fn rotate_to_archive(&self, config: &WalConfig) -> Result<Option<PathBuf>, AsyncWalError> {
        if !config.archive_enabled {
            self.truncate()?;
            return Ok(None);
        }
        let writer = self.writer.lock().expect("WAL writer lock poisoned");
        let path = writer.rotate_to_archive(config)?;
        Ok(Some(path))
    }

    /// Convert the async writer back to a synchronous writer.
    pub fn into_sync(self) -> Result<WalWriter, AsyncWalError> {
        let current_lsn = self.next_lsn.load(Ordering::Acquire).saturating_sub(1);
        if current_lsn > 0 {
            self.sync_manager.wait_for_lsn(current_lsn)?;
        }

        self.sync_manager.stop();

        let writer = WalWriter::open(&self.path)?;

        Ok(writer)
    }

    /// Internal: Rotate the current WAL segment for async sync.
    fn rotate_for_sync(&self, last_lsn: Lsn) -> Result<(), AsyncWalError> {
        let writer = self.writer.lock().expect("WAL writer lock poisoned");

        if let Err(e) = writer.file.lock().expect("file lock poisoned").flush() {
            return Err(AsyncWalError::RotationFailed {
                reason: "Failed to flush buffer".to_string(),
                source: Some(e),
            });
        }

        let counter = self.pending_counter.fetch_add(1, Ordering::Relaxed);
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let pending_name = format!("wal_pending_{}_{}.segment", timestamp, counter);
        let pending_dir = if self.config.pending_dir.is_absolute() {
            self.config.pending_dir.clone()
        } else {
            self.path
                .parent()
                .unwrap_or(Path::new("."))
                .join(&self.config.pending_dir)
        };
        let pending_path = pending_dir.join(pending_name);

        let size_bytes = fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);

        let first_lsn = self.synced_lsn.load(Ordering::Acquire) + 1;

        fs::rename(&self.path, &pending_path).map_err(|e| AsyncWalError::RotationFailed {
            reason: "Failed to rename WAL to pending".to_string(),
            source: Some(e),
        })?;

        let pending_file = OpenOptions::new()
            .read(true)
            .open(&pending_path)
            .map_err(|e| {
                let _ = fs::rename(&pending_path, &self.path);
                AsyncWalError::RotationFailed {
                    reason: "Failed to open pending segment".to_string(),
                    source: Some(e),
                }
            })?;

        let new_file = match OpenOptions::new()
            .create_new(true)
            .write(true)
            .read(true)
            .open(&self.path)
        {
            Ok(f) => f,
            Err(e) => {
                let _ = fs::rename(&pending_path, &self.path);
                return Err(AsyncWalError::RotationFailed {
                    reason: "Failed to create new WAL file".to_string(),
                    source: Some(e),
                });
            }
        };

        let mut new_writer = BufWriter::new(new_file);

        let header = WalHeader::new();
        if let Err(e) = new_writer.write_all(&header.to_bytes()) {
            let _ = fs::remove_file(&self.path);
            let _ = fs::rename(&pending_path, &self.path);
            return Err(AsyncWalError::RotationFailed {
                reason: "Failed to write header".to_string(),
                source: Some(e),
            });
        }
        if let Err(e) = new_writer.flush() {
            let _ = fs::remove_file(&self.path);
            let _ = fs::rename(&pending_path, &self.path);
            return Err(AsyncWalError::RotationFailed {
                reason: "Failed to flush header".to_string(),
                source: Some(e),
            });
        }

        *writer.file.lock().expect("file lock poisoned") = new_writer;
        *writer.header.lock().expect("header lock poisoned") = header;

        self.synced_lsn.store(last_lsn, Ordering::Release);

        let pending_segment = PendingSegment {
            path: pending_path,
            lsn_range: (first_lsn, last_lsn),
            file: pending_file,
            rotated_at: Instant::now(),
            size_bytes,
        };
        self.sync_manager.enqueue(pending_segment);

        Ok(())
    }

    /// Stop and join the background WAL-sync thread.
    ///
    /// Public, idempotent wrapper around the (private) `SegmentSyncManager`
    /// so an owner that holds only `&AsyncWalWriter` (e.g.
    /// `PersistentARTrieChar::close`) can deterministically tear the worker
    /// down without waiting for Arc-refcount drop order.
    pub fn stop_sync(&self) {
        self.sync_manager.stop();
    }
}

impl Drop for AsyncWalWriter {
    fn drop(&mut self) {
        // Stop + join the background sync thread first so it cannot race the
        // final fsync, and so the wal-sync thread is reclaimed deterministically
        // from this (the owning) thread rather than via Arc-refcount drop order.
        self.sync_manager.stop();
        if let Ok(writer) = self.writer.lock() {
            if let Err(e) = writer.sync() {
                log::warn!("Failed to sync WAL on drop: {:?}", e);
            }
        }
    }
}

/// Collect all WAL segments including pending segments for recovery.
///
/// Returns paths to all WAL segments in chronological order:
/// 1. Archived segments (oldest)
/// 2. Pending segments (awaiting sync)
/// 3. Active WAL (newest)
pub fn collect_all_segments(
    wal_path: &Path,
    config: &WalConfig,
    async_config: &AsyncWalConfig,
) -> Result<Vec<PathBuf>, WalError> {
    let mut segments = Vec::new();
    let parent = wal_path.parent().unwrap_or(Path::new("."));

    let archive_dir = if config.archive_dir.is_absolute() {
        config.archive_dir.clone()
    } else {
        parent.join(&config.archive_dir)
    };

    if archive_dir.exists() {
        for entry in fs::read_dir(&archive_dir).map_err(WalError::Io)? {
            let entry = entry.map_err(WalError::Io)?;
            let path = entry.path();
            if path.extension().map_or(false, |ext| ext == "segment") {
                segments.push(path);
            }
        }
    }

    let pending_dir = if async_config.pending_dir.is_absolute() {
        async_config.pending_dir.clone()
    } else {
        parent.join(&async_config.pending_dir)
    };

    if pending_dir.exists() {
        for entry in fs::read_dir(&pending_dir).map_err(WalError::Io)? {
            let entry = entry.map_err(WalError::Io)?;
            let path = entry.path();
            if path.extension().map_or(false, |ext| ext == "segment") {
                segments.push(path);
            }
        }
    }

    if wal_path.exists() {
        let metadata = fs::metadata(wal_path).map_err(WalError::Io)?;
        if metadata.len() > WalHeader::SIZE as u64 {
            segments.push(wal_path.to_path_buf());
        }
    }

    WalWriter::sort_segments_by_first_lsn(&mut segments);

    Ok(segments)
}
