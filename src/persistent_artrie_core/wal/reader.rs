//! WAL reader for recovery.
//!
//! Split out of the monolithic `wal.rs` (lines ~1447-1557) as part of the
//! Phase-4 wal decomposition. Provides `WalReader` (sequential next_record
//! API) and `WalRecordIterator` (Iterator adapter).

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use super::{crc32, Lsn, WalError, WalHeader, WalRecord, WalRecordType, WalWriter};

/// WAL reader for recovery.
pub struct WalReader {
    reader: BufReader<File>,
    #[allow(dead_code)]
    path: PathBuf,
}

impl WalReader {
    /// Open a WAL file for reading.
    pub fn new(path: impl AsRef<Path>) -> Result<Self, WalError> {
        let path = path.as_ref().to_path_buf();
        let file = File::open(&path)?;
        let mut reader = BufReader::new(file);

        // Skip header
        reader.seek(SeekFrom::Start(WalHeader::SIZE as u64))?;

        Ok(WalReader { reader, path })
    }

    /// Read the next record from the WAL.
    ///
    /// Returns `None` at end of file, `Some(Err(...))` on error.
    pub fn next_record(&mut self) -> Option<Result<(Lsn, WalRecord), WalError>> {
        // Read header: CRC (4) + Length (4) + LSN (8) + Type (1)
        let mut header_buf = [0u8; WalWriter::RECORD_HEADER_SIZE];
        match self.reader.read_exact(&mut header_buf) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return None,
            Err(e) => return Some(Err(WalError::Io(e))),
        }

        let stored_crc = u32::from_le_bytes(header_buf[0..4].try_into().unwrap());
        let length = u32::from_le_bytes(header_buf[4..8].try_into().unwrap()) as usize;
        let lsn = u64::from_le_bytes(header_buf[8..16].try_into().unwrap());
        let record_type_byte = header_buf[16];

        // Validate length
        if length < WalWriter::RECORD_HEADER_SIZE {
            return Some(Err(WalError::CorruptedRecord(
                "Record length too small".into(),
            )));
        }

        let payload_len = length - WalWriter::RECORD_HEADER_SIZE;

        // Read payload
        let mut payload = vec![0u8; payload_len];
        if !payload.is_empty() {
            match self.reader.read_exact(&mut payload) {
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                    return Some(Err(WalError::UnexpectedEof))
                }
                Err(e) => return Some(Err(WalError::Io(e))),
            }
        }

        // Verify CRC
        let mut crc_data = Vec::with_capacity(length - 4);
        crc_data.extend_from_slice(&header_buf[4..]);
        crc_data.extend_from_slice(&payload);
        let computed_crc = crc32(&crc_data);

        if stored_crc != computed_crc {
            return Some(Err(WalError::CorruptedRecord(format!(
                "CRC mismatch: stored={:#x}, computed={:#x}",
                stored_crc, computed_crc
            ))));
        }

        // Parse record type
        let record_type = match WalRecordType::try_from(record_type_byte) {
            Ok(t) => t,
            Err(e) => return Some(Err(e)),
        };

        // Deserialize record
        match WalRecord::deserialize(record_type, &payload) {
            Ok(record) => Some(Ok((lsn, record))),
            Err(e) => Some(Err(e)),
        }
    }

    /// Get an iterator over all records.
    pub fn iter(self) -> WalRecordIterator {
        WalRecordIterator { reader: self }
    }

    /// Read the header from the WAL file.
    pub fn read_header(path: impl AsRef<Path>) -> Result<WalHeader, WalError> {
        let file = File::open(path.as_ref())?;
        let mut reader = BufReader::new(file);
        let mut header_buf = [0u8; WalHeader::SIZE];
        reader.read_exact(&mut header_buf)?;
        WalHeader::from_bytes(&header_buf)
    }
}

/// Iterator over WAL records.
pub struct WalRecordIterator {
    reader: WalReader,
}

impl Iterator for WalRecordIterator {
    type Item = Result<(Lsn, WalRecord), WalError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.reader.next_record()
    }
}
