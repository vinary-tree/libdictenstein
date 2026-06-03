//! Write-Ahead Log writer.
//!
//! Split out of the monolithic `wal.rs` (lines ~105-715, ~608 LOC) as part
//! of the Phase-4 wal decomposition. The `WalWriter` struct owns the
//! on-disk WAL file handle and is the primary write entry point — append,
//! sync, batch, checkpoint, truncate, and segment rotation all live here.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use super::{crc32, Lsn, RankRegime, WalConfig, WalError, WalHeader, WalReader, WalRecord};

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
    /// Current LSN (next LSN to assign)
    next_lsn: AtomicU64,
    /// Last synced LSN
    synced_lsn: AtomicU64,
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

        Ok(WalWriter {
            path,
            file: Mutex::new(writer),
            next_lsn: AtomicU64::new(1), // LSN 0 reserved for "no LSN"
            synced_lsn: AtomicU64::new(0),
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

        // Find the last LSN by scanning the log
        let mut last_lsn: Lsn = 0;
        let mut reader = WalReader::new(path.clone())?;
        while let Some(result) = reader.next_record() {
            if let Ok((lsn, _)) = result {
                last_lsn = lsn;
            }
        }

        // Seek to end for appending
        let file = OpenOptions::new().read(true).write(true).open(&path)?;
        let mut writer = BufWriter::new(file);
        writer.seek(SeekFrom::End(0))?;

        Ok(WalWriter {
            path,
            file: Mutex::new(writer),
            next_lsn: AtomicU64::new(last_lsn + 1),
            synced_lsn: AtomicU64::new(last_lsn),
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

    /// Append a record using an LSN that was reserved before the write.
    #[cfg(feature = "group-commit")]
    pub(crate) fn append_with_lsn(&self, lsn: Lsn, record: WalRecord) -> Result<Lsn, WalError> {
        let current = self.current_lsn();
        if lsn != current {
            return Err(WalError::CorruptedRecord(format!(
                "reserved LSN {} does not match next WAL LSN {}",
                lsn, current
            )));
        }

        self.next_lsn.fetch_add(1, Ordering::AcqRel);
        self.write_record_at_lsn(lsn, record)?;
        Ok(lsn)
    }

    fn write_record_at_lsn(&self, lsn: Lsn, record: WalRecord) -> Result<(), WalError> {
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

        let mut file = self.file.lock().expect("WAL lock poisoned");
        file.write_all(&buf)?;

        Ok(())
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

        let mut header = self.header.lock().expect("header lock poisoned");
        header.checkpoint_lsn = checkpoint_lsn;

        let mut file = self.file.lock().expect("WAL lock poisoned");
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&header.to_bytes())?;
        file.flush()?;
        file.get_ref().sync_all()?;

        file.seek(SeekFrom::End(0))?;

        Ok(lsn)
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
    /// pre-checkpoint survivor. Mirrors [`Self::checkpoint`]'s header→file lock order.
    pub fn set_commit_seq_floor(&self, floor: u64) -> Result<(), WalError> {
        let mut header = self.header.lock().expect("header lock poisoned");
        if floor <= header.commit_seq_floor {
            return Ok(()); // monotone: never lower the floor
        }
        header.commit_seq_floor = floor;

        let mut file = self.file.lock().expect("WAL lock poisoned");
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

    /// Stamp the header to the Overlay regime (`MAGIC_OVERLAY` + `rank_regime=Overlay`)
    /// and persist it (S4 / N-S4-1). **SAFE ONLY when the WAL is EMPTY** (header-only,
    /// no records) — the caller MUST guarantee this. An in-place magic+regime restamp of
    /// a NON-empty file would (a) torn-write the magic without the regime ⇒
    /// Overlay-magic-Owned-regime ⇒ orphans wrongly KEPT, and (b) place pre-existing
    /// Owned records under the Overlay drop; the non-empty case needs a WAL rotation
    /// (deferred to the S5 production flip). On an empty WAL there are no records to
    /// mis-classify, so even a torn header just re-stamps on the next open. Idempotent.
    pub fn set_overlay_regime(&self) -> Result<(), WalError> {
        let mut header = self.header.lock().expect("header lock poisoned");
        if header.rank_regime == RankRegime::Overlay as u8 {
            return Ok(()); // already Overlay
        }
        header.magic = WalHeader::MAGIC_OVERLAY;
        header.rank_regime = RankRegime::Overlay as u8;

        let mut file = self.file.lock().expect("WAL lock poisoned");
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&header.to_bytes())?;
        file.flush()?;
        file.get_ref().sync_all()?;
        file.seek(SeekFrom::End(0))?;
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
        fs::create_dir_all(&archive_dir).map_err(|e| WalError::Io(e))?;

        let archive_path = Self::unique_archive_segment_path(&archive_dir);

        let mut file = self.file.lock().expect("WAL lock poisoned");

        file.flush()?;
        file.get_ref().sync_all()?;

        drop(file);

        fs::rename(&self.path, &archive_path).map_err(|e| WalError::Io(e))?;

        let new_file = OpenOptions::new()
            .create(true)
            .write(true)
            .read(true)
            .open(&self.path)?;

        let mut writer = BufWriter::new(new_file);

        // DG0 carry (D2.8 §2.3): the new active file CONTINUES the rotated file's
        // regime + floor. `WalHeader::new()` would zero both — losing the durable
        // commit_seq_floor (⇒ post-checkpoint reseed regression) and the
        // rank_regime (⇒ a rotated Overlay active mis-reads as Owned). Read the
        // old header BEFORE swapping it (below) and carry the two fields.
        let (carried_floor, carried_regime) = {
            let old = self.header.lock().expect("header lock poisoned");
            (old.commit_seq_floor, old.rank_regime)
        };
        let mut header = WalHeader::new();
        header.commit_seq_floor = carried_floor;
        header.rank_regime = carried_regime;
        writer.write_all(&header.to_bytes())?;
        writer.flush()?;

        *self.file.lock().expect("WAL lock poisoned") = writer;
        self.next_lsn
            .store(next_lsn_after_rotation, Ordering::Release);
        self.synced_lsn
            .store(synced_lsn_after_rotation, Ordering::Release);
        *self.header.lock().expect("header lock poisoned") = header;

        let _ = Self::prune_segments_if_needed(&archive_dir, config);

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
            for entry in fs::read_dir(&archive_dir).map_err(|e| WalError::Io(e))? {
                let entry = entry.map_err(|e| WalError::Io(e))?;
                let path = entry.path();
                if path.extension().map_or(false, |ext| ext == "segment") {
                    segments.push(path);
                }
            }
        }

        if self.path.exists() {
            let metadata = fs::metadata(&self.path).map_err(|e| WalError::Io(e))?;
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
    ) -> Result<(), WalError> {
        if !archive_dir.exists() {
            return Ok(());
        }

        let mut segments: Vec<(PathBuf, u64)> = Vec::new();
        for entry in fs::read_dir(archive_dir).map_err(|e| WalError::Io(e))? {
            let entry = entry.map_err(|e| WalError::Io(e))?;
            let path = entry.path();
            if path.extension().map_or(false, |ext| ext == "segment") {
                let size = fs::metadata(&path).map_or(0, |m| m.len());
                segments.push((path, size));
            }
        }

        segments.sort_by(|a, b| a.0.cmp(&b.0));

        let total_size: u64 = segments.iter().map(|(_, size)| size).sum();

        let mut current_size = total_size;
        let mut to_remove = Vec::new();

        for (i, (path, size)) in segments.iter().enumerate() {
            let remaining_count = segments.len() - i;

            if remaining_count <= 1 {
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
        assert_eq!(writer.commit_seq_floor(), 777, "commit_seq_floor carried across rotate");
        assert_eq!(
            writer.header.lock().expect("header lock").rank_regime,
            1,
            "rank_regime carried across rotate"
        );
    }
}
