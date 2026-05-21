//! BloomFilter support for `PersistentVocabARTrie<S>`.
//!
//! Split out of vocab `dict_impl.rs` (lines ~986-1101, ~116 LOC) as
//! a Phase-6 vocab sub-module. Methods covered:
//!
//! - `bloom_filter_path` — derive the `.bloom` sidecar path
//! - `save_bloom_filter` / `load_bloom_filter` (already pub(super))
//! - `rebuild_bloom_filter` / `enable_bloom_filter` / `disable_bloom_filter`
//! - `might_contain` / `get_index_with_bloom`
//! - `has_bloom_filter` / `bloom_filter` (accessor)

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use crate::bloom_filter::BloomFilter;
use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::error::{PersistentARTrieError, Result};

impl<S: BlockStorage> super::dict_impl::PersistentVocabARTrie<S> {
    // ========================================================================
    // BloomFilter Support
    // ========================================================================

    /// Get the bloom filter file path.
    fn bloom_filter_path(&self) -> PathBuf {
        self.path.with_extension("vocab.bloom")
    }

    /// Save bloom filter to disk using bincode.
    pub(super) fn save_bloom_filter(&self, bloom: &BloomFilter) -> Result<()> {
        let bloom_path = self.bloom_filter_path();
        let encoded = crate::serialization::bincode_compat::serialize(bloom).map_err(|e| {
            PersistentARTrieError::io_error(
                "serialize bloom filter",
                bloom_path.to_string_lossy(),
                std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()),
            )
        })?;
        std::fs::write(&bloom_path, encoded).map_err(|e| {
            PersistentARTrieError::io_error("write bloom filter", bloom_path.to_string_lossy(), e)
        })?;
        Ok(())
    }

    /// Load bloom filter from disk using bincode.
    pub(super) fn load_bloom_filter(path: &Path) -> Result<Option<BloomFilter>> {
        let bloom_path = path.with_extension("vocab.bloom");
        if !bloom_path.exists() {
            return Ok(None);
        }
        let data = std::fs::read(&bloom_path).map_err(|e| {
            PersistentARTrieError::io_error("read bloom filter", bloom_path.to_string_lossy(), e)
        })?;
        let bloom: BloomFilter = crate::serialization::bincode_compat::deserialize(&data).map_err(|e| {
            PersistentARTrieError::io_error(
                "deserialize bloom filter",
                bloom_path.to_string_lossy(),
                std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()),
            )
        })?;
        Ok(Some(bloom))
    }

    /// Rebuild bloom filter from all terms in the vocabulary.
    ///
    /// This is useful when opening a vocabulary that doesn't have a persisted
    /// bloom filter, or when the bloom filter file is corrupted.
    pub fn rebuild_bloom_filter(&mut self, expected_elements: usize) {
        let mut bloom = BloomFilter::new(expected_elements);
        for term in self.iter_terms() {
            bloom.insert(&term);
        }
        self.bloom_filter = Some(bloom);
    }

    /// Enable bloom filter with the specified capacity.
    ///
    /// If the vocabulary already has entries, rebuilds the bloom filter
    /// from existing terms.
    pub fn enable_bloom_filter(&mut self, expected_elements: usize) {
        if self.entry_count.load(Ordering::Acquire) > 0 {
            self.rebuild_bloom_filter(expected_elements);
        } else {
            self.bloom_filter = Some(BloomFilter::new(expected_elements));
        }
    }

    /// Disable bloom filter and remove persisted file if present.
    pub fn disable_bloom_filter(&mut self) -> Result<()> {
        self.bloom_filter = None;
        let bloom_path = self.bloom_filter_path();
        if bloom_path.exists() {
            std::fs::remove_file(&bloom_path).map_err(|e| {
                PersistentARTrieError::io_error(
                    "remove bloom filter",
                    bloom_path.to_string_lossy(),
                    e,
                )
            })?;
        }
        Ok(())
    }

    /// Check if word might be in vocabulary (O(1) fast path).
    ///
    /// Returns `false` = definitely NOT in vocabulary (use for fast rejection).
    /// Returns `true` = might be in vocabulary (must verify with `get_index()`).
    ///
    /// If no BloomFilter configured, always returns `true`.
    #[inline]
    pub fn might_contain(&self, term: &str) -> bool {
        match &self.bloom_filter {
            Some(bloom) => bloom.might_contain(term),
            None => true,
        }
    }

    /// Get index with BloomFilter fast path.
    ///
    /// Uses BloomFilter for O(1) rejection of OOV words before trie traversal.
    #[inline]
    pub fn get_index_with_bloom(&self, term: &str) -> Option<u64> {
        if !self.might_contain(term) {
            return None;
        }
        self.get_index(term)
    }

    /// Returns true if BloomFilter is enabled.
    #[inline]
    pub fn has_bloom_filter(&self) -> bool {
        self.bloom_filter.is_some()
    }

    /// Get a reference to the bloom filter if present.
    #[inline]
    pub fn bloom_filter(&self) -> Option<&BloomFilter> {
        self.bloom_filter.as_ref()
    }
}
