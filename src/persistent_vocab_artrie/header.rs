//! Vocabulary-specific file-header reader.
//!
//! Previously lived as a free function in
//! `persistent_artrie_core::block_storage::read_vocab_header` (and as
//! convenience methods on `MmapDiskManager::read_vocab_header` and
//! `IoUringDiskManager::read_vocab_header`). Those sites coupled the
//! variant-agnostic core storage layer to `VocabTrieFileHeader`, a
//! vocabulary-specific type. The function lives in the vocab module so the
//! coupling runs in the correct direction (vocab → core only).

use crate::persistent_artrie_core::block_storage::BlockStorage;
use crate::persistent_artrie_core::error::Result;
use crate::persistent_vocab_artrie::types::{VocabTrieFileHeader, VOCAB_FILE_HEADER_SIZE};

/// Read a [`VocabTrieFileHeader`] from block 0 of any `BlockStorage`.
///
/// This reads the 96-byte extended header used by `PersistentVocabARTrie`,
/// which is wider than the regular 64-byte `FileHeader` used by byte/char
/// tries.
pub fn read_vocab_header(storage: &impl BlockStorage) -> Result<VocabTrieFileHeader> {
    let mut bytes = [0u8; VOCAB_FILE_HEADER_SIZE];
    storage.read_header_bytes(&mut bytes)?;
    Ok(VocabTrieFileHeader::from_bytes(&bytes))
}
