//! Write-Ahead Log writer.
//!
//! Split out of the monolithic `wal.rs` (lines ~105-715, ~608 LOC) as part
//! of the Phase-4 wal decomposition. The `WalWriter` struct owns the
//! on-disk WAL file handle and is the primary write entry point — append,
//! sync, batch, checkpoint, truncate, and segment rotation all live here.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
#[cfg(all(unix, not(target_os = "wasi")))]
use std::os::unix::fs::FileExt;
#[cfg(windows)]
use std::os::windows::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::Mutex;

use super::{crc32, Lsn, RankRegime, WalConfig, WalError, WalHeader, WalReader, WalRecord};
use crate::nonblocking::CasBackoff;

static ARCHIVE_SEGMENT_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Write-Ahead Log writer.
///
/// Handles appending records to the log with optional group commit.
pub struct WalWriter {
    /// Path to the WAL file
    path: PathBuf,
    /// File handle.
    ///
    /// `pub(super)` so the still-inline async-writer cluster in `wal.rs` can
    /// poke at the file lock during segment rotation. Will tighten back to
    /// private once the async-writer cluster also moves into its own
    /// sub-module.
    pub(super) file: Mutex<BufWriter<File>>,
    /// Positional-write handle used by the ARTrie durable-write hot path.
    ///
    /// Loaded lock-free by appends; swapped only by truncate/rotation lifecycle
    /// operations.
    segment_file: arc_swap::ArcSwap<File>,
    /// WASI Preview 1 does not expose stable `pwrite`; serialize seek+write on
    /// a cloned descriptor while retaining native lock-free positional writes.
    #[cfg(target_os = "wasi")]
    wasi_segment_write: Mutex<()>,
    /// Next byte offset for positional WAL appends.
    segment_next_offset: AtomicU64,
    /// Current LSN (next LSN to assign)
    next_lsn: AtomicU64,
    /// Last synced LSN
    synced_lsn: AtomicU64,
    /// Highest contiguous LSN durably published through the lock-free record path.
    ///
    /// The buffered append path still obtains durability from [`Self::sync`].
    /// Positional appenders reserve WAL offsets independently, so they need an
    /// explicit contiguous frontier: LSN N+1 cannot be reported durable while LSN N
    /// is still missing.
    segment_durable_lsn: AtomicU64,
    /// Positional-write LSNs that reached disk before an earlier LSN completed.
    segment_durable_pending: Mutex<BTreeSet<Lsn>>,
    /// Lowest positional-write LSN whose publication failed after reservation.
    segment_failed_lsn: AtomicU64,
    /// Header (cached). Same visibility rationale as `file`.
    pub(super) header: Mutex<WalHeader>,
}

impl WalWriter {
    /// Record header size: CRC32 (4) + Length (4) + LSN (8) + Type (1) = 17 bytes
    pub const RECORD_HEADER_SIZE: usize = 17;

    /// Create a new WAL file.
    ///
    /// Uses atomic exclusive creation (`O_CREAT | O_EXCL` via `create_new(true)`)
    /// to eliminate TOCTOU race conditions. This matches the formal model's
    /// `open_create` operation in `FileSystem.v`.
    ///
    /// # Errors
    ///
    /// - `WalError::AlreadyExists` - File already exists (atomic check)
    /// - `WalError::ParentNotFound` - Parent directory doesn't exist
    /// - `WalError::Io` - Other I/O errors
    pub fn create(path: impl AsRef<Path>) -> Result<Self, WalError> {
        let path = path.as_ref().to_path_buf();

        // Ensure parent directory exists (idempotent, matches formal mkdir_all)
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    if e.kind() == io::ErrorKind::NotFound {
                        WalError::ParentNotFound(parent.to_path_buf())
                    } else {
                        WalError::Io(e)
                    }
                })?;
            }
        }

        // Atomic exclusive creation - eliminates TOCTOU race
        let file = match OpenOptions::new()
            .create_new(true)
            .write(true)
            .read(true)
            .open(&path)
        {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                return Err(WalError::AlreadyExists);
            }
            Err(e) => return Err(WalError::Io(e)),
        };

        let mut writer = BufWriter::new(file);

        // Write header
        let header = WalHeader::new();
        writer.write_all(&header.to_bytes())?;
        writer.flush()?;
        let segment_file = OpenOptions::new().read(true).write(true).open(&path)?;

        Ok(WalWriter {
            path,
            file: Mutex::new(writer),
            segment_file: arc_swap::ArcSwap::from_pointee(segment_file),
            #[cfg(target_os = "wasi")]
            wasi_segment_write: Mutex::new(()),
            segment_next_offset: AtomicU64::new(WalHeader::SIZE as u64),
            next_lsn: AtomicU64::new(1), // LSN 0 reserved for "no LSN"
            synced_lsn: AtomicU64::new(0),
            segment_durable_lsn: AtomicU64::new(0),
            segment_durable_pending: Mutex::new(BTreeSet::new()),
            segment_failed_lsn: AtomicU64::new(Lsn::MAX),
            header: Mutex::new(header),
        })
    }

    /// Open an existing WAL file for appending.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, WalError> {
        let path = path.as_ref().to_path_buf();

        let file = match OpenOptions::new().read(true).write(true).open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(WalError::NotFound);
            }
            Err(e) => return Err(WalError::Io(e)),
        };

        // Read header
        let mut reader = BufReader::new(&file);
        let mut header_buf = [0u8; WalHeader::SIZE];
        reader.read_exact(&mut header_buf)?;
        let header = WalHeader::from_bytes(&header_buf)?;

        // Find the last LSN by scanning the active log and any legacy record segments.
        let mut last_lsn: Lsn = 0;
        let mut reader = WalReader::new(path.clone())?;
        while let Some(result) = reader.next_record() {
            if let Ok((lsn, _)) = result {
                last_lsn = last_lsn.max(lsn);
            }
        }
        let record_segments = Self::collect_record_segments_for_path(&path)?;
        if let Some(max_segment_lsn) = Self::max_lsn_in_segments(&record_segments) {
            last_lsn = last_lsn.max(max_segment_lsn);
        }
        last_lsn = last_lsn.max(header.checkpoint_lsn);

        // Seek to end for appending
        let file = OpenOptions::new().read(true).write(true).open(&path)?;
        let mut writer = BufWriter::new(file);
        writer.seek(SeekFrom::End(0))?;
        let segment_offset = fs::metadata(&path)
            .map(|metadata| metadata.len().max(WalHeader::SIZE as u64))
            .unwrap_or(WalHeader::SIZE as u64);
        let segment_file = OpenOptions::new().read(true).write(true).open(&path)?;

        Ok(WalWriter {
            path,
            file: Mutex::new(writer),
            segment_file: arc_swap::ArcSwap::from_pointee(segment_file),
            #[cfg(target_os = "wasi")]
            wasi_segment_write: Mutex::new(()),
            segment_next_offset: AtomicU64::new(segment_offset),
            next_lsn: AtomicU64::new(last_lsn + 1),
            synced_lsn: AtomicU64::new(last_lsn),
            segment_durable_lsn: AtomicU64::new(last_lsn),
            segment_durable_pending: Mutex::new(BTreeSet::new()),
            segment_failed_lsn: AtomicU64::new(Lsn::MAX),
            header: Mutex::new(header),
        })
    }

    /// TOCTOU-safe open or create.
    pub fn open_or_create(path: impl AsRef<Path>) -> Result<Self, WalError> {
        let path = path.as_ref().to_path_buf();

        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    if e.kind() == io::ErrorKind::NotFound {
                        WalError::ParentNotFound(parent.to_path_buf())
                    } else {
                        WalError::Io(e)
                    }
                })?;
            }
        }

        match Self::open(&path) {
            Ok(writer) => Ok(writer),
            Err(WalError::NotFound) => match Self::create(&path) {
                Ok(writer) => Ok(writer),
                Err(WalError::AlreadyExists) => Self::open(&path),
                Err(e) => Err(e),
            },
            Err(e) => Err(e),
        }
    }

    /// Append a record to the WAL.
    pub fn append(&self, record: WalRecord) -> Result<Lsn, WalError> {
        let lsn = self.next_lsn.fetch_add(1, Ordering::AcqRel);
        self.write_record_at_lsn(lsn, record)?;
        Ok(lsn)
    }

    /// Append a record as an independent, valid one-record WAL segment.
    ///
    /// This is the ARTrie durable-write hot path. It reserves the LSN atomically
    /// and never takes the active WAL file mutex while publishing the record; the
    /// only shared state touched after the filesystem rename is the small
    /// contiguous-durability frontier.
    pub fn append_record_segment(&self, record: WalRecord) -> Result<Lsn, WalError> {
        let lsn = self.next_lsn.fetch_add(1, Ordering::AcqRel);
        if let Err(error) = self.write_record_segment_at_lsn(lsn, record) {
            self.mark_segment_failed(lsn);
            return Err(error);
        }
        self.mark_segment_durable(lsn);
        self.wait_for_segment_durable_lsn(lsn)?;
        Ok(lsn)
    }

    /// Append a lock-free record using an LSN reserved by the caller.
    #[cfg(feature = "group-commit")]
    pub(crate) fn append_record_segment_with_lsn(
        &self,
        lsn: Lsn,
        record: WalRecord,
    ) -> Result<Lsn, WalError> {
        let current = self.current_lsn();
        if lsn != current {
            return Err(WalError::CorruptedRecord(format!(
                "reserved LSN {} does not match next lock-free WAL LSN {}",
                lsn, current
            )));
        }

        self.next_lsn.fetch_add(1, Ordering::AcqRel);
        if let Err(error) = self.write_record_segment_at_lsn(lsn, record) {
            self.mark_segment_failed(lsn);
            return Err(error);
        }
        self.mark_segment_durable(lsn);
        self.wait_for_segment_durable_lsn(lsn)?;
        Ok(lsn)
    }

    fn write_record_at_lsn(&self, lsn: Lsn, record: WalRecord) -> Result<(), WalError> {
        let buf = Self::encode_record_frame(lsn, record);

        let mut file = self.file.lock().expect("WAL lock poisoned");
        file.write_all(&buf)?;

        Ok(())
    }

    fn encode_record_frame(lsn: Lsn, record: WalRecord) -> Vec<u8> {
        let payload = record.serialize_payload();
        let record_type = record.record_type() as u8;

        let total_len = Self::RECORD_HEADER_SIZE + payload.len();
        let mut buf = Vec::with_capacity(total_len);

        buf.extend_from_slice(&[0u8; 4]);
        buf.extend_from_slice(&(total_len as u32).to_le_bytes());
        buf.extend_from_slice(&lsn.to_le_bytes());
        buf.push(record_type);
        buf.extend_from_slice(&payload);

        let crc = crc32(&buf[4..]);
        buf[0..4].copy_from_slice(&crc.to_le_bytes());

        buf
    }

    fn write_record_segment_at_lsn(&self, lsn: Lsn, record: WalRecord) -> Result<(), WalError> {
        let frame = Self::encode_record_frame(lsn, record);
        let offset = self
            .segment_next_offset
            .fetch_add(frame.len() as u64, Ordering::AcqRel);
        let file = self.segment_file.load();
        #[cfg(all(unix, not(target_os = "wasi")))]
        file.write_all_at(&frame, offset).map_err(WalError::Io)?;
        #[cfg(windows)]
        {
            let mut written = 0usize;
            while written < frame.len() {
                let count = file
                    .seek_write(&frame[written..], offset + written as u64)
                    .map_err(WalError::Io)?;
                if count == 0 {
                    return Err(WalError::Io(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "short positional WAL write",
                    )));
                }
                written += count;
            }
        }
        #[cfg(target_os = "wasi")]
        {
            let _guard = self
                .wasi_segment_write
                .lock()
                .expect("WASI segment write lock poisoned");
            // WASI Preview 1 has no descriptor-dup operation, but `&File`
            // implements seek/write over the original descriptor. The target-local
            // mutex above gives that shared cursor exclusive access.
            let mut positioned = &**file;
            positioned
                .seek(SeekFrom::Start(offset))
                .map_err(WalError::Io)?;
            positioned.write_all(&frame).map_err(WalError::Io)?;
        }
        #[cfg(not(target_os = "wasi"))]
        file.sync_data().map_err(WalError::Io)?;
        // Node's WASI Preview 1 maps fd_sync but may reject fd_datasync.
        #[cfg(target_os = "wasi")]
        file.sync_all().map_err(WalError::Io)?;

        Ok(())
    }

    fn mark_segment_durable(&self, lsn: Lsn) {
        let mut pending = self
            .segment_durable_pending
            .lock()
            .expect("segment durable frontier lock poisoned");
        pending.insert(lsn);

        let mut next = self.segment_durable_lsn.load(Ordering::Acquire) + 1;
        while pending.remove(&next) {
            self.segment_durable_lsn.store(next, Ordering::Release);
            next += 1;
        }
    }

    fn mark_segment_failed(&self, lsn: Lsn) {
        self.segment_failed_lsn.fetch_min(lsn, Ordering::AcqRel);
    }

    fn wait_for_segment_durable_lsn(&self, lsn: Lsn) -> Result<(), WalError> {
        let mut backoff = CasBackoff::new();
        loop {
            if self.segment_durable_lsn.load(Ordering::Acquire) >= lsn {
                return Ok(());
            }
            let failed = self.segment_failed_lsn.load(Ordering::Acquire);
            if failed <= lsn {
                return Err(WalError::CorruptedRecord(format!(
                    "cannot publish contiguous WAL segment frontier through LSN {lsn}; \
                     earlier reserved LSN {failed} failed to publish"
                )));
            }
            backoff.snooze();
        }
    }

    /// Sync (fsync) the WAL to disk.
    pub fn sync(&self) -> Result<Lsn, WalError> {
        let mut file = self.file.lock().expect("WAL lock poisoned");
        file.flush()?;
        file.get_ref().sync_all()?;

        let current_lsn = self.next_lsn.load(Ordering::Acquire) - 1;
        self.synced_lsn.store(current_lsn, Ordering::Release);

        Ok(current_lsn)
    }

    /// Sync the active WAL and report the contiguous lock-free durable frontier.
    ///
    /// Used by ARTrie variants that publish operation records through positional
    /// writes instead of serializing through the buffered append lock.
    pub fn sync_record_segments(&self) -> Result<Lsn, WalError> {
        self.segment_file.load().sync_all().map_err(WalError::Io)?;

        let durable_lsn = self.segment_durable_lsn.load(Ordering::Acquire);
        self.synced_lsn.store(durable_lsn, Ordering::Release);

        Ok(durable_lsn)
    }

    /// Append a batch of inserts as a single WAL record.
    pub fn append_batch(&self, entries: &[(Vec<u8>, Option<Vec<u8>>)]) -> Result<Lsn, WalError> {
        if entries.is_empty() {
            return self.append(WalRecord::BatchInsert {
                entries: Vec::new(),
            });
        }

        let record = WalRecord::BatchInsert {
            entries: entries.to_vec(),
        };
        self.append(record)
    }

    /// Append a batch of inserts and sync in a single operation.
    pub fn append_batch_and_sync(
        &self,
        entries: &[(Vec<u8>, Option<Vec<u8>>)],
    ) -> Result<Lsn, WalError> {
        self.append_batch(entries)?;
        self.sync()
    }

    /// Get the current (next) LSN.
    pub fn current_lsn(&self) -> Lsn {
        self.next_lsn.load(Ordering::Acquire)
    }

    /// Get the last synced LSN.
    pub fn synced_lsn(&self) -> Lsn {
        self.synced_lsn.load(Ordering::Acquire)
    }

    /// Get the path to the WAL file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Allocate a new LSN without writing a record.
    pub fn allocate_lsn(&self) -> Lsn {
        self.next_lsn.fetch_add(1, Ordering::AcqRel)
    }

    /// Set the minimum starting LSN for subsequent records.
    pub fn set_min_lsn(&self, min_lsn: Lsn) {
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

    /// Set the minimum synced LSN when durable retained segments are known.
    pub(super) fn set_min_synced_lsn(&self, min_lsn: Lsn) {
        loop {
            let current = self.synced_lsn.load(Ordering::Acquire);
            if current >= min_lsn {
                break;
            }
            if self
                .synced_lsn
                .compare_exchange(current, min_lsn, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                break;
            }
        }
        loop {
            let current = self.segment_durable_lsn.load(Ordering::Acquire);
            if current >= min_lsn {
                break;
            }
            if self
                .segment_durable_lsn
                .compare_exchange(current, min_lsn, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                break;
            }
        }
    }

    /// Write a checkpoint record through the lock-free path and update the active header.
    pub fn checkpoint_record_segment(&self, checkpoint_lsn: Lsn) -> Result<Lsn, WalError> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let record = WalRecord::Checkpoint {
            checkpoint_lsn,
            timestamp,
        };

        let lsn = self.append_record_segment(record)?;
        self.sync_record_segments()?;
        self.write_checkpoint_header(checkpoint_lsn)?;
        Ok(lsn)
    }

    /// Write a checkpoint record and update the header.
    pub fn checkpoint(&self, checkpoint_lsn: Lsn) -> Result<Lsn, WalError> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let record = WalRecord::Checkpoint {
            checkpoint_lsn,
            timestamp,
        };

        let lsn = self.append(record)?;
        self.sync()?;

        self.write_checkpoint_header(checkpoint_lsn)?;

        Ok(lsn)
    }

    fn write_checkpoint_header(&self, checkpoint_lsn: Lsn) -> Result<(), WalError> {
        let mut file = self.file.lock().expect("WAL lock poisoned");
        let mut header = self.header.lock().expect("header lock poisoned");
        header.checkpoint_lsn = checkpoint_lsn;

        file.seek(SeekFrom::Start(0))?;
        file.write_all(&header.to_bytes())?;
        file.flush()?;
        file.get_ref().sync_all()?;

        file.seek(SeekFrom::End(0))?;
        Ok(())
    }

    /// Get the last checkpoint LSN.
    pub fn checkpoint_lsn(&self) -> Lsn {
        let header = self.header.lock().expect("header lock poisoned");
        header.checkpoint_lsn
    }

    /// Durably raise the commit-sequence floor (D2.8 D4). **Monotone** (raise-only,
    /// like [`Self::set_min_lsn`]): a lower-domain checkpoint can never lower it.
    /// Persisted in the header (fsync) so it survives reopen + carries across
    /// rotate/truncate. Set at checkpoint time (DG2) to the max `commit_seq`
    /// subsumed by the checkpoint, so a post-checkpoint op out-ranks every
    /// pre-checkpoint survivor. Mirrors [`Self::checkpoint`]'s file→header lock order.
    pub fn set_commit_seq_floor(&self, floor: u64) -> Result<(), WalError> {
        let mut file = self.file.lock().expect("WAL lock poisoned");
        let mut header = self.header.lock().expect("header lock poisoned");
        if floor <= header.commit_seq_floor {
            return Ok(()); // monotone: never lower the floor
        }
        header.commit_seq_floor = floor;

        file.seek(SeekFrom::Start(0))?;
        file.write_all(&header.to_bytes())?;
        file.flush()?;
        file.get_ref().sync_all()?;
        file.seek(SeekFrom::End(0))?;
        Ok(())
    }

    /// Get the durable commit-sequence floor (D2.8 D4).
    pub fn commit_seq_floor(&self) -> u64 {
        let header = self.header.lock().expect("header lock poisoned");
        header.commit_seq_floor
    }

    /// Returns `true` iff no records have been appended since creation/truncation
    /// (`next_lsn == 1`) — i.e. the WAL is header-only ("empty after header"). This is
    /// the authoritative emptiness check used by the regime-stamp guards; it is robust
    /// to `BufWriter` buffering (a buffered-but-unflushed record still bumps `next_lsn`),
    /// unlike a raw on-disk file length.
    pub fn is_empty_after_header(&self) -> bool {
        self.next_lsn.load(Ordering::Acquire) == 1
    }

    /// **F7 (FIX A/FIX D) — RECORDS-EMPTY-ON-DISK predicate.** Returns `true` iff the
    /// on-disk WAL file is exactly header-sized (`len == WalHeader::SIZE`), i.e. it
    /// carries NO records — regardless of the `next_lsn` counter.
    ///
    /// This is the load-bearing distinction from [`Self::is_empty_after_header`]
    /// (`next_lsn == 1`): after a crash-AFTER-rotate-BEFORE-stamp, `rotate_to_archive`
    /// restores the OLD (high) `next_lsn` onto the fresh header-only active, so
    /// `is_empty_after_header()` is FALSE even though the file has no records. The F7
    /// converter must classify that file as RECORDS-EMPTY (the cheap path) to avoid the
    /// crash-loop that mints an empty archive segment per reopen (v4 FIX D). It is also
    /// the gate for the post-rotate Overlay stamp (FIX A — admit a header-only,
    /// HIGH-`next_lsn` active without resetting `next_lsn`).
    ///
    /// Flushes the `BufWriter` first so the on-disk length reflects any buffered records
    /// (a buffered-but-unsynced append makes the file non-empty). Safe on the post-rotate
    /// active (no buffered records possible in that window — the design's soundness
    /// argument), and conservative everywhere else (it reports non-empty if anything was
    /// written). On a flush/metadata error it returns `false` (the SAFE direction:
    /// "assume records present", so the converter rotates rather than stamps-in-place).
    pub fn records_empty_on_disk(&self) -> bool {
        let mut file = self.file.lock().expect("WAL lock poisoned");
        if file.flush().is_err() {
            return false; // SAFE: assume records present on a flush error.
        }
        match file.get_ref().metadata() {
            Ok(meta) => meta.len() == WalHeader::SIZE as u64,
            Err(_) => false, // SAFE: assume records present if we cannot stat.
        }
    }

    /// Stamp the header to the Overlay regime (`MAGIC_OVERLAY` + `rank_regime=Overlay`)
    /// and persist it (S4 / N-S4-1). **ENFORCED to be EMPTY** (S5-3): an in-place
    /// magic+regime restamp of a NON-empty file would (a) torn-write the magic without
    /// the regime ⇒ Overlay-magic-Owned-regime ⇒ orphans wrongly KEPT, and (b) place
    /// pre-existing Owned records under the Overlay drop; the non-empty case needs a WAL
    /// rotation. Previously a doc-only contract; now returns
    /// [`WalError::InvalidRegimeStamp`] if the WAL is non-empty. Idempotent on an
    /// already-Overlay header.
    pub fn set_overlay_regime(&self) -> Result<(), WalError> {
        if !self.is_empty_after_header() {
            return Err(WalError::InvalidRegimeStamp(
                "set_overlay_regime on a non-empty WAL (records already appended); \
                 the non-empty Owned→Overlay transition requires a rotation"
                    .to_string(),
            ));
        }
        let mut file = self.file.lock().expect("WAL lock poisoned");
        let mut header = self.header.lock().expect("header lock poisoned");
        if header.rank_regime == RankRegime::Overlay as u8 {
            return Ok(()); // already Overlay
        }
        header.magic = WalHeader::MAGIC_OVERLAY;
        header.rank_regime = RankRegime::Overlay as u8;

        file.seek(SeekFrom::Start(0))?;
        file.write_all(&header.to_bytes())?;
        file.flush()?;
        file.get_ref().sync_all()?;
        file.seek(SeekFrom::End(0))?;
        debug_assert_eq!(header.rank_regime, RankRegime::Overlay as u8);
        Ok(())
    }

    /// **F7 (FIX A widening) — stamp the header to the Overlay regime gated on
    /// RECORDS-EMPTY-ON-DISK** (file len == `WalHeader::SIZE`), NOT on the `next_lsn==1`
    /// counter that [`Self::set_overlay_regime`] uses.
    ///
    /// The converter's CHEAP path lands on a header-only active that may carry a HIGH
    /// `next_lsn` (a post-crash-after-rotate active restores the OLD counter via
    /// `rotate_to_archive`; a post-owned-checkpoint active was `set_min_lsn`'d above 1).
    /// `set_overlay_regime`'s `next_lsn==1` gate would WRONGLY reject such an active even
    /// though it is genuinely records-empty (no records to mis-interpret under the Overlay
    /// drop rule). This method gates on the on-disk record-emptiness instead and stamps the
    /// header directly + fsyncs (the cheap-path durable commit point). Idempotent on an
    /// already-Overlay header. Returns [`WalError::InvalidRegimeStamp`] if the file carries
    /// records.
    pub fn set_overlay_regime_records_empty(&self) -> Result<(), WalError> {
        let mut file = self.file.lock().expect("WAL lock poisoned");
        file.flush()?;
        if file.get_ref().metadata()?.len() != WalHeader::SIZE as u64 {
            return Err(WalError::InvalidRegimeStamp(
                "set_overlay_regime_records_empty on a WAL that carries records on disk; \
                 the non-empty Owned→Overlay transition requires a rotation"
                    .to_string(),
            ));
        }
        let mut header = self.header.lock().expect("header lock poisoned");
        if header.rank_regime == RankRegime::Overlay as u8 {
            return Ok(()); // already Overlay
        }
        header.magic = WalHeader::MAGIC_OVERLAY;
        header.rank_regime = RankRegime::Overlay as u8;

        file.seek(SeekFrom::Start(0))?;
        file.write_all(&header.to_bytes())?;
        file.flush()?;
        file.get_ref().sync_all()?;
        file.seek(SeekFrom::End(0))?;
        debug_assert_eq!(header.rank_regime, RankRegime::Overlay as u8);
        Ok(())
    }

    /// Stamp the header BACK to the Owned regime (`MAGIC` + `rank_regime=Owned`) and
    /// persist it — the inverse of [`set_overlay_regime`], for the S5 kill-switch
    /// (Overlay→Owned rollback). **ENFORCED to be EMPTY** (S5-4) for the symmetric
    /// reason: an in-place restamp of a non-empty Overlay WAL would place its Overlay
    /// records under the Owned keep-all rule (resurrecting dropped orphans). Returns
    /// [`WalError::InvalidRegimeStamp`] if non-empty. Idempotent on an already-Owned
    /// header.
    pub fn set_owned_regime(&self) -> Result<(), WalError> {
        if !self.is_empty_after_header() {
            return Err(WalError::InvalidRegimeStamp(
                "set_owned_regime on a non-empty WAL (records already appended); \
                 the non-empty Overlay→Owned transition requires a rotation"
                    .to_string(),
            ));
        }
        let mut file = self.file.lock().expect("WAL lock poisoned");
        let mut header = self.header.lock().expect("header lock poisoned");
        if header.rank_regime == RankRegime::Owned as u8 {
            return Ok(()); // already Owned
        }
        header.magic = WalHeader::MAGIC;
        header.rank_regime = RankRegime::Owned as u8;

        file.seek(SeekFrom::Start(0))?;
        file.write_all(&header.to_bytes())?;
        file.flush()?;
        file.get_ref().sync_all()?;
        file.seek(SeekFrom::End(0))?;
        debug_assert_eq!(header.rank_regime, RankRegime::Owned as u8);
        Ok(())
    }

    /// The header's current rank-regime (S4).
    pub fn rank_regime(&self) -> RankRegime {
        let header = self.header.lock().expect("header lock poisoned");
        header.regime()
    }

    /// Truncate the WAL file, removing all records.
    pub fn truncate(&self) -> Result<(), WalError> {
        let mut file = self.file.lock().expect("WAL lock poisoned");

        file.flush()?;

        let inner_file = file.get_mut();
        inner_file.set_len(WalHeader::SIZE as u64)?;

        file.seek(SeekFrom::Start(WalHeader::SIZE as u64))?;

        self.next_lsn.store(1, Ordering::Release);
        self.synced_lsn.store(0, Ordering::Release);
        self.segment_durable_lsn.store(0, Ordering::Release);
        self.segment_failed_lsn.store(Lsn::MAX, Ordering::Release);
        self.segment_next_offset
            .store(WalHeader::SIZE as u64, Ordering::Release);
        self.segment_durable_pending
            .lock()
            .expect("segment durable frontier lock poisoned")
            .clear();
        let record_segment_dir = self.record_segment_dir();
        if record_segment_dir.exists() {
            fs::remove_dir_all(&record_segment_dir).map_err(WalError::Io)?;
        }

        {
            let mut header = self.header.lock().expect("header lock poisoned");
            header.checkpoint_lsn = 0;

            file.seek(SeekFrom::Start(0))?;
            file.write_all(&header.to_bytes())?;
            file.flush()?;
            file.get_ref().sync_all()?;

            file.seek(SeekFrom::Start(WalHeader::SIZE as u64))?;
        }

        Ok(())
    }

    pub(super) fn unique_archive_segment_path(archive_dir: &Path) -> PathBuf {
        loop {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let counter = ARCHIVE_SEGMENT_COUNTER.fetch_add(1, Ordering::AcqRel);
            let segment_name = format!(
                "wal_{:020}_{}_{}.segment",
                nanos,
                std::process::id(),
                counter
            );
            let candidate = archive_dir.join(segment_name);
            if !candidate.exists() {
                return candidate;
            }
        }
    }

    pub(super) fn record_segment_dir_for_path(path: &Path) -> PathBuf {
        let mut dir = path.to_path_buf();
        let extension = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| format!("{ext}.d"))
            .unwrap_or_else(|| "wal.d".to_string());
        dir.set_extension(extension);
        dir
    }

    pub fn record_segment_dir(&self) -> PathBuf {
        Self::record_segment_dir_for_path(&self.path)
    }

    pub(super) fn collect_record_segments_for_path(path: &Path) -> Result<Vec<PathBuf>, WalError> {
        let segment_dir = Self::record_segment_dir_for_path(path);
        let mut segments = Vec::new();
        if segment_dir.exists() {
            for entry in fs::read_dir(&segment_dir).map_err(WalError::Io)? {
                let entry = entry.map_err(WalError::Io)?;
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "segment") {
                    segments.push(path);
                }
            }
        }
        Self::sort_segments_by_first_lsn(&mut segments);
        Ok(segments)
    }

    pub(super) fn sort_segments_by_first_lsn(segments: &mut [PathBuf]) {
        segments.sort_by(|a, b| {
            let lsn_a = Self::segment_first_lsn(a);
            let lsn_b = Self::segment_first_lsn(b);
            match (lsn_a, lsn_b) {
                (Some(lsn_a), Some(lsn_b)) => lsn_a.cmp(&lsn_b).then_with(|| a.cmp(b)),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => a.cmp(b),
            }
        });
    }

    /// First-LSN sort for the `(path, size)` tuples the pruner uses (F7). Same ordering as
    /// [`Self::sort_segments_by_first_lsn`]: ascending first LSN; an unreadable first LSN
    /// sorts LAST (treated as newest = keep, the SAFE direction for the prune exemption).
    fn sort_segments_by_first_lsn_tagged(segments: &mut [(PathBuf, u64)]) {
        segments.sort_by(|a, b| {
            let lsn_a = Self::segment_first_lsn(&a.0);
            let lsn_b = Self::segment_first_lsn(&b.0);
            match (lsn_a, lsn_b) {
                (Some(lsn_a), Some(lsn_b)) => lsn_a.cmp(&lsn_b).then_with(|| a.0.cmp(&b.0)),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => a.0.cmp(&b.0),
            }
        });
    }

    pub(super) fn max_lsn_in_segments(segments: &[PathBuf]) -> Option<Lsn> {
        let mut max_lsn = None;
        for path in segments {
            let Ok(reader) = WalReader::new(path) else {
                continue;
            };
            for result in reader.iter() {
                let Ok((lsn, _)) = result else {
                    continue;
                };
                max_lsn = Some(max_lsn.map_or(lsn, |current: Lsn| current.max(lsn)));
            }
        }
        max_lsn
    }

    fn segment_first_lsn(segment_path: &Path) -> Option<Lsn> {
        let mut reader = WalReader::new(segment_path).ok()?;
        reader
            .next_record()
            .and_then(|result| result.ok())
            .map(|(lsn, _)| lsn)
    }

    /// Rotate WAL to archive directory - O(1) filesystem rename operation.
    pub fn rotate_to_archive(&self, config: &WalConfig) -> Result<PathBuf, WalError> {
        self.sync()?;
        let next_lsn_after_rotation = self.next_lsn.load(Ordering::Acquire);
        let synced_lsn_after_rotation = self.synced_lsn.load(Ordering::Acquire);

        let archive_dir = if config.archive_dir.is_absolute() {
            config.archive_dir.clone()
        } else {
            self.path
                .parent()
                .unwrap_or(Path::new("."))
                .join(&config.archive_dir)
        };
        fs::create_dir_all(&archive_dir).map_err(WalError::Io)?;

        let archive_path = Self::unique_archive_segment_path(&archive_dir);

        let mut file = self.file.lock().expect("WAL lock poisoned");

        file.flush()?;
        file.get_ref().sync_all()?;

        drop(file);

        fs::rename(&self.path, &archive_path).map_err(WalError::Io)?;

        let new_file = OpenOptions::new()
            .create(true)
            .write(true)
            .read(true)
            // A rotated WAL must start empty. The rename above normally leaves nothing
            // at this path, so this is a no-op -- but if a stale file were ever present,
            // appending to it would produce a WAL whose records predate the rotation.
            // Truncating enforces the invariant instead of assuming it.
            .truncate(true)
            .open(&self.path)?;

        let mut writer = BufWriter::new(new_file);

        // DG0 carry (D2.8 §2.3): the new active file CONTINUES the rotated file's
        // regime + floor. `WalHeader::new()` would zero both — losing the durable
        // commit_seq_floor (⇒ post-checkpoint reseed regression) and the
        // rank_regime (⇒ a rotated Overlay active mis-reads as Owned). Read the
        // old header BEFORE swapping it (below) and carry the two fields.
        let (carried_floor, carried_regime, carried_checkpoint_lsn) = {
            let old = self.header.lock().expect("header lock poisoned");
            (old.commit_seq_floor, old.rank_regime, old.checkpoint_lsn)
        };
        let mut header = WalHeader::new();
        header.commit_seq_floor = carried_floor;
        header.rank_regime = carried_regime;
        writer.write_all(&header.to_bytes())?;
        writer.flush()?;

        *self.file.lock().expect("WAL lock poisoned") = writer;
        self.segment_file.store(Arc::new(
            OpenOptions::new().read(true).write(true).open(&self.path)?,
        ));
        self.segment_next_offset
            .store(WalHeader::SIZE as u64, Ordering::Release);
        self.next_lsn
            .store(next_lsn_after_rotation, Ordering::Release);
        self.synced_lsn
            .store(synced_lsn_after_rotation, Ordering::Release);
        *self.header.lock().expect("header lock poisoned") = header;

        // F7 FIX-D belt-and-suspenders: prune with the OLD header's `checkpoint_lsn` (the
        // durable image redo frontier at rotation time) so un-subsumed segments
        // (`first_lsn > checkpoint_lsn`) are NEVER pruned. (`carried_checkpoint_lsn == 0`
        // when no durable frontier is known — e.g. the converter's rotate or a
        // post-owned-truncate active — which conservatively retains ALL segments.)
        let _ = Self::prune_segments_if_needed(&archive_dir, config, carried_checkpoint_lsn);

        Ok(archive_path)
    }

    /// **F7 (S1+S2) — rotate the Owned tail to archive, then RE-STAMP the fresh active
    /// to the Overlay regime and re-assert the carried commit-seq floor, fsyncing the
    /// fresh Overlay header (the DURABLE COMMIT POINT).**
    ///
    /// The non-empty-Owned-WAL conversion primitive. Under the (caller-held) writer lock
    /// the converter runs:
    ///
    /// 1. [`Self::rotate_to_archive`] — archives the Owned tail and recreates a
    ///    header-only active that CARRIES the OLD (high) `next_lsn` (DG0 monotonicity — do
    ///    NOT reset; archive LSNs stay strictly below all future active LSNs), the
    ///    `commit_seq_floor`, and the `rank_regime` (still Owned here). After this the
    ///    fresh active is RECORDS-EMPTY-ON-DISK (header-only) even though `next_lsn` is high.
    /// 2. [`Self::set_overlay_regime`] gated on [`Self::records_empty_on_disk`] (FIX A —
    ///    a header-only-but-high-`next_lsn` active is a legitimate stamp target; the
    ///    `next_lsn`-counter gate in `set_overlay_regime` would wrongly reject it, so we
    ///    re-stamp the header directly here after asserting records-empty).
    /// 3. re-assert [`Self::set_commit_seq_floor`] with the carried floor (idempotent;
    ///    `rotate_to_archive` already carried it, but a defensive re-assert costs one
    ///    header write and guarantees the floor survives even if a future rotate impl
    ///    changes the carry).
    /// 4. **`sync_all()` the fresh Overlay header** (OBL-1 — `rotate_to_archive` only
    ///    `flush()`es the fresh header; the EMPTY-Overlay header is the S2 durable commit
    ///    point and MUST be fsync-durable across a power cut).
    ///
    /// Returns the archived segment path (the just-rotated Owned tail) so the caller's
    /// FIX-B drain can scan it.
    pub fn rotate_and_restamp_overlay(&self, config: &WalConfig) -> Result<PathBuf, WalError> {
        // S1: archive the Owned tail; the fresh active carries the high next_lsn + floor
        // + (Owned) regime.
        let archive_path = self.rotate_to_archive(config)?;

        // F7 crash-injection (test-only; disarmed = no-op): simulate a power-cut AFTER the
        // durable archive rename but BEFORE the Overlay stamp+fsync — the v4 FIX-D torn
        // window (the fresh active is records-empty + still Owned-regime, carrying the high
        // next_lsn). The converter must classify this as records-empty on the next reopen
        // (the CHEAP path) and NOT re-rotate, so the crash-loop never prunes the real tail.
        if crate::persistent_artrie::core::overlay::flip::f7_failpoint::armed()
            == crate::persistent_artrie::core::overlay::flip::f7_failpoint::FailPoint::AfterRotateBeforeStamp
        {
            return Err(WalError::InvalidRegimeStamp(
                "F7 fail-point AfterRotateBeforeStamp (simulated crash after rotate, before stamp)"
                    .to_string(),
            ));
        }

        // The fresh active MUST be records-empty-on-disk now (header-only). If not, a
        // concurrent append slipped in (the caller must hold the writer lock — this is a
        // defensive guard); refuse rather than mis-stamp.
        if !self.records_empty_on_disk() {
            return Err(WalError::InvalidRegimeStamp(
                "rotate_and_restamp_overlay: fresh active is not records-empty after rotate \
                 (records appended concurrently?); refusing the Overlay stamp"
                    .to_string(),
            ));
        }

        // S2a: stamp the fresh (records-empty, possibly high-next_lsn) active to Overlay.
        // Stamp the header DIRECTLY here (not via `set_overlay_regime`, whose `next_lsn==1`
        // gate would reject the carried high counter — FIX A). Records-empty-on-disk has
        // been asserted, so stamping Overlay is unambiguous (no Owned records to
        // mis-interpret under the Overlay drop rule).
        let carried_floor = {
            let mut file = self.file.lock().expect("WAL lock poisoned");
            let mut header = self.header.lock().expect("header lock poisoned");
            header.magic = WalHeader::MAGIC_OVERLAY;
            header.rank_regime = RankRegime::Overlay as u8;
            // S2b: re-assert the carried floor on the same in-memory header.
            let floor = header.commit_seq_floor;
            file.seek(SeekFrom::Start(0))?;
            file.write_all(&header.to_bytes())?;
            file.flush()?;
            // OBL-1: fsync the fresh Overlay header — the S2 durable commit point.
            file.get_ref().sync_all()?;
            file.seek(SeekFrom::End(0))?;
            floor
        };

        // S2c: defensive idempotent re-assert of the floor (monotone raise-only; a no-op
        // because the carried floor was just written, but guards against a future rotate
        // impl that drops the carry). `set_commit_seq_floor` fsyncs again.
        self.set_commit_seq_floor(carried_floor)?;

        Ok(archive_path)
    }

    /// Collect all WAL segments (archived + active) in chronological order.
    pub fn collect_wal_segments(&self, config: &WalConfig) -> Result<Vec<PathBuf>, WalError> {
        let mut segments = Vec::new();

        let archive_dir = if config.archive_dir.is_absolute() {
            config.archive_dir.clone()
        } else {
            self.path
                .parent()
                .unwrap_or(Path::new("."))
                .join(&config.archive_dir)
        };

        if archive_dir.exists() {
            for entry in fs::read_dir(&archive_dir).map_err(WalError::Io)? {
                let entry = entry.map_err(WalError::Io)?;
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "segment") {
                    segments.push(path);
                }
            }
        }

        segments.extend(Self::collect_record_segments_for_path(&self.path)?);

        if self.path.exists() {
            let metadata = fs::metadata(&self.path).map_err(WalError::Io)?;
            if metadata.len() > WalHeader::SIZE as u64 {
                segments.push(self.path.clone());
            }
        }

        Self::sort_segments_by_first_lsn(&mut segments);

        Ok(segments)
    }

    /// Prune old WAL segments to stay within limits.
    pub(super) fn prune_segments_if_needed(
        archive_dir: &Path,
        config: &WalConfig,
        checkpoint_lsn: Lsn,
    ) -> Result<(), WalError> {
        if !archive_dir.exists() {
            return Ok(());
        }

        let mut segments: Vec<(PathBuf, u64)> = Vec::new();
        for entry in fs::read_dir(archive_dir).map_err(WalError::Io)? {
            let entry = entry.map_err(WalError::Io)?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "segment") {
                let size = fs::metadata(&path).map_or(0, |m| m.len());
                segments.push((path, size));
            }
        }

        // Order by FIRST LSN (oldest committed data first) so "oldest-first" pruning + the
        // F7 un-subsumed exemption's "break on the first un-subsumed segment" are sound (an
        // unreadable first LSN sorts last = treated as newest/keep). Path order
        // (`wal_{nanos}_{pid}_{counter}`) is usually first-LSN order, but sort explicitly so
        // the exemption never mis-classifies a segment.
        Self::sort_segments_by_first_lsn_tagged(&mut segments);

        let total_size: u64 = segments.iter().map(|(_, size)| size).sum();

        let mut current_size = total_size;
        let mut to_remove = Vec::new();

        for (i, (path, size)) in segments.iter().enumerate() {
            let remaining_count = segments.len() - i;

            if remaining_count <= 1 {
                break;
            }

            // **F7 FIX-D belt-and-suspenders — NEVER prune an UN-SUBSUMED segment.** A
            // segment whose FIRST LSN is above the durable image redo frontier
            // (`first_lsn > checkpoint_lsn`) holds committed records the dense image does
            // NOT cover; pruning it would silently lose the converted/under-load tail (the
            // crash-loop data-loss class the cheap-vs-rotate FIX D already prevents — this
            // is the second line of defense). A `checkpoint_lsn == 0` (no durable frontier
            // known, e.g. post-owned-truncate) treats EVERY segment as un-subsumed, so
            // nothing is pruned — the SAFE direction (retain committed data; correctness
            // rests on the reconcile's lsn-skip, not on removal). Pruning resumes once a
            // later checkpoint raises the frontier above a segment's first LSN.
            let first_lsn = Self::segment_first_lsn(path);
            let subsumed =
                matches!(first_lsn, Some(fl) if fl <= checkpoint_lsn) && checkpoint_lsn > 0;
            if !subsumed {
                // Un-subsumed (or unreadable first LSN — the SAFE keep direction): never
                // prune. Segments are first-LSN-ordered, so once we hit an un-subsumed one
                // every later (higher-LSN) segment is also un-subsumed — stop scanning.
                break;
            }

            let over_count = remaining_count > config.max_segments;
            let over_size = current_size > config.max_archive_bytes;

            if over_count || over_size {
                to_remove.push(path.clone());
                current_size = current_size.saturating_sub(*size);
            } else {
                break;
            }
        }

        for path in to_remove {
            let _ = fs::remove_file(path);
        }

        Ok(())
    }
}

#[cfg(test)]
mod dg0_carry_tests {
    use super::{WalConfig, WalWriter};
    use std::path::Path;

    /// DG0: `rotate_to_archive` must CARRY the active file's commit_seq_floor and
    /// rank_regime into the fresh active header (not zero them via WalHeader::new).
    /// REAL-disk scratch (never tmpfs — /tmp is RAM on this host).
    #[test]
    fn rotate_carries_floor_and_regime() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp/dg0_carry");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let wal_path = dir.join("wal.log");

        let writer = WalWriter::create(&wal_path).expect("create wal");
        writer.set_commit_seq_floor(777).expect("set floor");
        {
            // Simulate an Overlay-regime active file (DG-RECON sets this via the flip).
            let mut h = writer.header.lock().expect("header lock");
            h.rank_regime = 1; // RankRegime::Overlay
        }

        let cfg = WalConfig::with_archive_dir(dir.join("archive"));
        writer.rotate_to_archive(&cfg).expect("rotate");

        // The NEW active file CONTINUES the rotated file's floor + regime.
        assert_eq!(
            writer.commit_seq_floor(),
            777,
            "commit_seq_floor carried across rotate"
        );
        assert_eq!(
            writer.header.lock().expect("header lock").rank_regime,
            1,
            "rank_regime carried across rotate"
        );
    }

    /// S5-3 / S5-4: the regime-stamp guards ACCEPT an empty WAL (Overlay↔Owned
    /// round-trips, since a regime stamp writes only the header — no records — so the
    /// WAL stays empty) and REJECT a non-empty WAL (an in-place restamp would
    /// mis-classify the pre-existing records). REAL-disk scratch (never tmpfs).
    #[test]
    fn regime_stamp_guards_reject_non_empty_wal() {
        use super::{RankRegime, WalError, WalRecord};

        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp/s5_regime_guard");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let wal_path = dir.join("wal.log");

        let writer = WalWriter::create(&wal_path).expect("create wal");

        // Empty WAL: stamps succeed and round-trip (Owned → Overlay → Owned).
        assert!(
            writer.is_empty_after_header(),
            "fresh WAL is empty after header"
        );
        writer.set_overlay_regime().expect("stamp overlay on empty");
        assert_eq!(writer.rank_regime(), RankRegime::Overlay);
        assert!(
            writer.is_empty_after_header(),
            "stamping the header appends no records"
        );
        writer.set_owned_regime().expect("stamp owned on empty");
        assert_eq!(writer.rank_regime(), RankRegime::Owned);

        // Append a record → non-empty → BOTH stamps are REJECTED.
        writer
            .append(WalRecord::Insert {
                term: b"x".to_vec(),
                value: None,
            })
            .expect("append");
        assert!(
            !writer.is_empty_after_header(),
            "WAL is non-empty after an append"
        );
        assert!(
            matches!(
                writer.set_overlay_regime(),
                Err(WalError::InvalidRegimeStamp(_))
            ),
            "set_overlay_regime must reject a non-empty WAL"
        );
        assert!(
            matches!(
                writer.set_owned_regime(),
                Err(WalError::InvalidRegimeStamp(_))
            ),
            "set_owned_regime must reject a non-empty WAL"
        );
    }
}
