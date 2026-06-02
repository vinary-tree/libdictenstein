//! WAL file header layout.
//!
//! Split out of the monolithic `wal.rs` (lines 829-900) as part of the
//! Phase-4 wal decomposition. Carries the `PARTWAL\0` magic, version, and
//! checkpoint-LSN bookkeeping that lives in the first 64 bytes of every
//! WAL file.

use super::error::WalError;
use super::Lsn;

/// WAL file header.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct WalHeader {
    /// Magic number: "PARTWAL\0"
    pub magic: [u8; 8],
    /// Version number
    pub version: u32,
    /// Last checkpoint LSN
    pub checkpoint_lsn: Lsn,
    /// Reserved for future use
    pub reserved: [u8; 44],
}

impl WalHeader {
    /// Magic number for WAL files.
    pub const MAGIC: [u8; 8] = *b"PARTWAL\0";
    /// Current (written) version.
    ///
    /// **Bumped 1 → 2** for the Order-A replay-order fix (design C′, §3.4): a
    /// version-2 WAL may contain the new additive `WalRecord::CommitRank=15`
    /// records. The bump gives **fail-closed forward compatibility** — an older
    /// binary (which only knows version 1) refuses a version-2 file via the
    /// `version > VERSION` check below rather than silently mis-reading the new
    /// records. This is the one intentionally one-way change in the design
    /// (documented in GAP_LEDGER / UNSAFE_BOUNDARY); it gates an opt-in pre-flip
    /// feature, so no released on-disk format is broken.
    pub const VERSION: u32 = 2;
    /// Oldest WAL version this build can still READ.
    ///
    /// **Backward compatibility (new code, old WAL):** a version-1 WAL has no
    /// `CommitRank` records, so replay falls back to `generation_of = lsn` =
    /// the pre-fix in-order behavior — correct for every log that can exist
    /// pre-fix (insert-only / pre-remove). No migration is required; we accept
    /// any version in `[MIN_SUPPORTED_VERSION, VERSION]`.
    pub const MIN_SUPPORTED_VERSION: u32 = 1;
    /// Header size in bytes.
    pub const SIZE: usize = 64;

    /// Create a new header.
    pub fn new() -> Self {
        WalHeader {
            magic: Self::MAGIC,
            version: Self::VERSION,
            checkpoint_lsn: 0,
            reserved: [0; 44],
        }
    }

    /// Serialize header to bytes.
    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut buf = [0u8; Self::SIZE];
        buf[0..8].copy_from_slice(&self.magic);
        buf[8..12].copy_from_slice(&self.version.to_le_bytes());
        buf[12..20].copy_from_slice(&self.checkpoint_lsn.to_le_bytes());
        buf[20..64].copy_from_slice(&self.reserved);
        buf
    }

    /// Deserialize header from bytes.
    pub fn from_bytes(buf: &[u8; Self::SIZE]) -> Result<Self, WalError> {
        let magic: [u8; 8] = buf[0..8].try_into().unwrap();
        if magic != Self::MAGIC {
            return Err(WalError::CorruptedRecord("Invalid WAL magic number".into()));
        }
        let version = u32::from_le_bytes(buf[8..12].try_into().unwrap());
        // Accept any version in [MIN_SUPPORTED_VERSION, VERSION]:
        //   * a too-OLD version (< MIN_SUPPORTED) is unreadable;
        //   * a too-NEW version (> VERSION) is refused FAIL-CLOSED so an old
        //     binary never silently mis-reads a newer file (design §3.4).
        // A version-1 WAL is read unchanged (no CommitRank → lsn-order fallback).
        if version < Self::MIN_SUPPORTED_VERSION || version > Self::VERSION {
            return Err(WalError::CorruptedRecord(format!(
                "Unsupported WAL version: {} (supported range {}..={})",
                version,
                Self::MIN_SUPPORTED_VERSION,
                Self::VERSION
            )));
        }
        let checkpoint_lsn = u64::from_le_bytes(buf[12..20].try_into().unwrap());
        let reserved: [u8; 44] = buf[20..64].try_into().unwrap();

        Ok(WalHeader {
            magic,
            version,
            checkpoint_lsn,
            reserved,
        })
    }
}

impl Default for WalHeader {
    fn default() -> Self {
        Self::new()
    }
}
