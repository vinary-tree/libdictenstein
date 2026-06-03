//! WAL file header layout.
//!
//! Split out of the monolithic `wal.rs` (lines 829-900) as part of the
//! Phase-4 wal decomposition. Carries the `PARTWAL\0` magic, version, and
//! checkpoint-LSN bookkeeping that lives in the first 64 bytes of every
//! WAL file.

use super::error::WalError;
use super::Lsn;

/// Rank-regime of a WAL file (D2.8 §1.1): a dedicated, durable, per-FILE marker
/// distinguishing the ranked lock-free **overlay** producer (every confirmed op
/// carries a `CommitRank` ⇒ an unranked record is a two-append orphan ⇒ DROP on
/// replay) from the **owned**/legacy/base/vocab producer (no ranks ⇒ every
/// unranked record is a confirmed in-order append ⇒ KEEP). Stored in ONE header
/// byte (offset 28); defaults `Owned = 0`, so every existing/base/vocab file and
/// any header an older binary wrote (zeroed `reserved`) reads as `Owned` — which
/// is why D2.8 keys the replay drop rule on THIS (not on a global VERSION bump,
/// which would have bricked base/vocab — the cross-codebase F1 break).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RankRegime {
    /// Owned/legacy/base/vocab producer — unranked records are KEPT in LSN order.
    Owned = 0,
    /// Lock-free overlay producer — unranked records are two-append orphans, DROPPED.
    Overlay = 1,
}

impl RankRegime {
    /// Decode a stored regime byte. Unknown bytes map to `Owned` (fail-safe:
    /// "keep everything", the non-destructive direction).
    pub fn from_u8(b: u8) -> Self {
        match b {
            1 => RankRegime::Overlay,
            _ => RankRegime::Owned,
        }
    }
}

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
    /// Durable commit-sequence floor (D2.8 D4): the max `commit_seq` subsumed by
    /// the last checkpoint; seeds the global counter on open so post-checkpoint
    /// ops out-rank pre-checkpoint survivors, and is CARRIED across rotate/
    /// truncate. Bytes 20..28. `0` = no floor (the pre-DG0 / Owned default).
    pub commit_seq_floor: u64,
    /// Rank-regime marker (D2.8 §1.1), see [`RankRegime`]. Byte 28. `0` = `Owned`
    /// (default for every existing/base/vocab file and any older-binary header).
    pub rank_regime: u8,
    /// Reserved for future use (bytes 29..64).
    pub reserved: [u8; 35],
}

impl WalHeader {
    /// Magic number for WAL files (the standard / Owned-regime magic).
    pub const MAGIC: [u8; 8] = *b"PARTWAL\0";
    /// Magic for Overlay-regime WAL files (D2.8 D8-2 dual-magic tripwire). The
    /// lock-free-overlay flip stamps THIS (alongside `rank_regime = Overlay`) on the
    /// fresh active file, so an OLD binary that only knows [`Self::MAGIC`] FAIL-CLOSES
    /// on an Overlay file (magic mismatch) instead of silently mis-reading its ranked
    /// records — WITHOUT a global VERSION bump, so base/vocab/un-flipped-char files
    /// (which keep `MAGIC`) are untouched. NEW binaries accept BOTH (see `from_bytes`).
    /// Greenfield: nothing writes this yet (the flip ctor is a later DG-RECON step), so
    /// the accept-set is inert until then.
    pub const MAGIC_OVERLAY: [u8; 8] = *b"PARTWALO";
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
            commit_seq_floor: 0,
            rank_regime: RankRegime::Owned as u8,
            reserved: [0; 35],
        }
    }

    /// Serialize header to bytes.
    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut buf = [0u8; Self::SIZE];
        buf[0..8].copy_from_slice(&self.magic);
        buf[8..12].copy_from_slice(&self.version.to_le_bytes());
        buf[12..20].copy_from_slice(&self.checkpoint_lsn.to_le_bytes());
        buf[20..28].copy_from_slice(&self.commit_seq_floor.to_le_bytes());
        buf[28] = self.rank_regime;
        buf[29..64].copy_from_slice(&self.reserved);
        buf
    }

    /// Deserialize header from bytes.
    pub fn from_bytes(buf: &[u8; Self::SIZE]) -> Result<Self, WalError> {
        let magic: [u8; 8] = buf[0..8].try_into().unwrap();
        // Dual-magic accept-set (D2.8 D8-2): a NEW binary reads BOTH the standard and
        // the Overlay magic; an OLD binary (only `MAGIC`) fail-closes on an Overlay
        // file. ADDITIVE — every existing file (`MAGIC`) parses exactly as before, so
        // base/vocab/char recovery is unchanged. The regime itself is still read from
        // the `rank_regime` byte (28); the flip writes both consistently.
        if magic != Self::MAGIC && magic != Self::MAGIC_OVERLAY {
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
        // DG0 (D2.8 §1.1/D4): the floor + regime live in what was `reserved[0..9]`.
        // A pre-DG0 header zeroed those bytes ⇒ floor=0, rank_regime=Owned — so an
        // old file reads back as Owned/no-floor (DG0 is fully reversible, no bump).
        let commit_seq_floor = u64::from_le_bytes(buf[20..28].try_into().unwrap());
        let rank_regime = buf[28];
        let reserved: [u8; 35] = buf[29..64].try_into().unwrap();

        Ok(WalHeader {
            magic,
            version,
            checkpoint_lsn,
            commit_seq_floor,
            rank_regime,
            reserved,
        })
    }

    /// The decoded rank-regime of this file (D2.8 §1.1).
    pub fn regime(&self) -> RankRegime {
        RankRegime::from_u8(self.rank_regime)
    }
}

impl Default for WalHeader {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod dg0_header_tests {
    use super::*;

    #[test]
    fn header_roundtrips_floor_and_regime() {
        let mut h = WalHeader::new();
        h.commit_seq_floor = 0x0102_0304_0506_0708;
        h.rank_regime = RankRegime::Overlay as u8;
        h.checkpoint_lsn = 42;
        let bytes = h.to_bytes();
        let h2 = WalHeader::from_bytes(&bytes).expect("roundtrip");
        assert_eq!(h2.commit_seq_floor, 0x0102_0304_0506_0708);
        assert_eq!(h2.regime(), RankRegime::Overlay);
        assert_eq!(h2.checkpoint_lsn, 42);
        assert_eq!(h2.version, WalHeader::VERSION);
    }

    /// A pre-DG0 header carries the floor/regime bytes inside the old zeroed
    /// `reserved[44]` — DG0 must read it back as Owned / no-floor (reversible).
    #[test]
    fn old_zeroed_reserved_reads_as_owned_no_floor() {
        let mut buf = WalHeader::new().to_bytes();
        for b in buf[20..64].iter_mut() {
            *b = 0; // simulate an old writer's zeroed reserved region
        }
        let h = WalHeader::from_bytes(&buf).expect("old header reads");
        assert_eq!(h.commit_seq_floor, 0);
        assert_eq!(h.regime(), RankRegime::Owned);
    }

    /// An unknown regime byte must map to Owned (fail-safe = keep-everything).
    #[test]
    fn unknown_regime_byte_is_owned_failsafe() {
        let mut buf = WalHeader::new().to_bytes();
        buf[28] = 0xFF;
        let h = WalHeader::from_bytes(&buf).expect("reads");
        assert_eq!(h.regime(), RankRegime::Owned);
    }

    /// D8-2 dual-magic: the standard magic STILL parses (additive ⇒ base/vocab/char
    /// unchanged); an Overlay-magic header parses in a new binary; an unknown magic
    /// still fail-closes (and, by construction, an OLD binary that only knows MAGIC
    /// would fail-close on MAGIC_OVERLAY — the tripwire).
    #[test]
    fn dual_magic_accept_set() {
        // Standard MAGIC still parses — the critical "no regression for existing
        // base/vocab/char files" property.
        let std = WalHeader::new().to_bytes();
        assert!(
            WalHeader::from_bytes(&std).is_ok(),
            "standard MAGIC must still parse (additive)"
        );

        // An Overlay-magic header parses in the NEW binary, with Overlay regime.
        let mut over = WalHeader::new();
        over.magic = WalHeader::MAGIC_OVERLAY;
        over.rank_regime = RankRegime::Overlay as u8;
        let bytes = over.to_bytes();
        let h = WalHeader::from_bytes(&bytes).expect("MAGIC_OVERLAY must parse in a new binary");
        assert_eq!(h.magic, WalHeader::MAGIC_OVERLAY);
        assert_eq!(h.regime(), RankRegime::Overlay);

        // An unknown magic still fail-closes.
        let mut bad = WalHeader::new().to_bytes();
        bad[0..8].copy_from_slice(b"NOTAWAL!");
        assert!(
            WalHeader::from_bytes(&bad).is_err(),
            "unknown magic must still fail-close"
        );
    }
}
