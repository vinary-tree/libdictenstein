//! Bloom filter for fast negative lookup rejection.
//!
//! This module provides a simple Bloom filter implementation optimized for
//! dictionary membership testing. It is designed to quickly reject terms that
//! are definitely not in the dictionary without traversing the full structure.
//!
//! # Characteristics
//!
//! - **False positives**: Possible (requires full dictionary traversal to confirm)
//! - **False negatives**: Never (guaranteed correct rejection)
//! - **Target false positive rate**: ~1% with 3 hash functions
//! - **Memory usage**: ~1.2 bytes per expected element (10 bits per element)

use rustc_hash::FxHasher;
use std::hash::{Hash, Hasher};

/// Simple Bloom filter for fast negative lookup rejection.
///
/// Uses 3 hash functions and a bit vector to probabilistically test membership.
/// - False positives: Possible (requires full DAWG/trie traversal)
/// - False negatives: Never (guaranteed correct rejection)
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serialization", derive(serde::Serialize, serde::Deserialize))]
pub struct BloomFilter {
    bits: Vec<u64>, // Bit vector (64-bit chunks for efficiency)
    bit_count: usize,
    hash_count: usize,
}

impl BloomFilter {
    /// Create a new Bloom filter with specified capacity.
    ///
    /// Uses ~1.2 bytes per expected element with 3 hash functions.
    /// Target false positive rate: ~1%
    ///
    /// # Arguments
    ///
    /// * `expected_elements` - The expected number of elements to be inserted
    pub fn new(expected_elements: usize) -> Self {
        // Use 10 bits per element for ~1% false positive rate with 3 hash functions
        let bit_count = expected_elements.saturating_mul(10).max(64);
        let chunk_count = (bit_count + 63) / 64; // Round up to nearest u64

        BloomFilter {
            bits: vec![0u64; chunk_count],
            bit_count: chunk_count * 64,
            hash_count: 3,
        }
    }

    /// Create a new Bloom filter with custom parameters.
    ///
    /// # Arguments
    ///
    /// * `bit_count` - Total number of bits in the filter
    /// * `hash_count` - Number of hash functions to use
    pub fn with_params(bit_count: usize, hash_count: usize) -> Self {
        let bit_count = bit_count.max(64);
        let chunk_count = (bit_count + 63) / 64;

        BloomFilter {
            bits: vec![0u64; chunk_count],
            bit_count: chunk_count * 64,
            hash_count: hash_count.max(1),
        }
    }

    /// Add a term (as string) to the Bloom filter.
    #[inline]
    pub fn insert(&mut self, term: &str) {
        self.insert_bytes(term.as_bytes());
    }

    /// Add raw bytes to the Bloom filter.
    #[inline]
    pub fn insert_bytes(&mut self, bytes: &[u8]) {
        for i in 0..self.hash_count {
            let hash = self.hash_with_seed(bytes, i as u64);
            let bit_index = (hash % self.bit_count as u64) as usize;
            let chunk_index = bit_index / 64;
            let bit_offset = bit_index % 64;
            self.bits[chunk_index] |= 1u64 << bit_offset;
        }
    }

    /// Check if a term (as string) might be in the set.
    ///
    /// # Returns
    ///
    /// - `false`: Definitely NOT in set (fast rejection)
    /// - `true`: Might be in set (requires full check)
    #[inline]
    pub fn might_contain(&self, term: &str) -> bool {
        self.might_contain_bytes(term.as_bytes())
    }

    /// Check if raw bytes might be in the set.
    ///
    /// # Returns
    ///
    /// - `false`: Definitely NOT in set (fast rejection)
    /// - `true`: Might be in set (requires full check)
    #[inline]
    pub fn might_contain_bytes(&self, bytes: &[u8]) -> bool {
        for i in 0..self.hash_count {
            let hash = self.hash_with_seed(bytes, i as u64);
            let bit_index = (hash % self.bit_count as u64) as usize;
            let chunk_index = bit_index / 64;
            let bit_offset = bit_index % 64;
            if (self.bits[chunk_index] & (1u64 << bit_offset)) == 0 {
                return false; // Definitely not in set
            }
        }
        true // Might be in set
    }

    /// Clear all bits in the Bloom filter.
    pub fn clear(&mut self) {
        self.bits.fill(0);
    }

    /// Get the capacity (total bit count) of this filter.
    pub fn capacity(&self) -> usize {
        self.bit_count
    }

    /// Get the number of hash functions used.
    pub fn hash_count(&self) -> usize {
        self.hash_count
    }

    /// Hash bytes with a seed using FxHash.
    #[inline]
    fn hash_with_seed(&self, bytes: &[u8], seed: u64) -> u64 {
        let mut hasher = FxHasher::default();
        seed.hash(&mut hasher);
        bytes.hash(&mut hasher);
        hasher.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bloom_filter_basic() {
        let mut bloom = BloomFilter::new(100);

        bloom.insert("hello");
        bloom.insert("world");
        bloom.insert("test");

        // Terms that were inserted should return true
        assert!(bloom.might_contain("hello"));
        assert!(bloom.might_contain("world"));
        assert!(bloom.might_contain("test"));
    }

    #[test]
    fn test_bloom_filter_no_false_negatives() {
        let mut bloom = BloomFilter::new(1000);
        let terms: Vec<String> = (0..100).map(|i| format!("term{}", i)).collect();

        for term in &terms {
            bloom.insert(term);
        }

        // All inserted terms must return true (no false negatives)
        for term in &terms {
            assert!(bloom.might_contain(term), "False negative for: {}", term);
        }
    }

    #[test]
    fn test_bloom_filter_clear() {
        let mut bloom = BloomFilter::new(100);

        bloom.insert("hello");
        assert!(bloom.might_contain("hello"));

        bloom.clear();

        // After clear, filter should be empty
        // Note: might_contain could return false or true (false in this case)
        // We can't guarantee false negatives for non-members, but clearing
        // should reset all bits
        let all_zeros = bloom.bits.iter().all(|&chunk| chunk == 0);
        assert!(all_zeros, "Bloom filter not fully cleared");
    }

    #[test]
    fn test_bloom_filter_bytes() {
        let mut bloom = BloomFilter::new(100);

        bloom.insert_bytes(&[0x10, 0x20, 0x30]);

        assert!(bloom.might_contain_bytes(&[0x10, 0x20, 0x30]));
    }

    #[test]
    fn test_bloom_filter_custom_params() {
        let bloom = BloomFilter::with_params(256, 5);

        assert_eq!(bloom.capacity(), 256);
        assert_eq!(bloom.hash_count(), 5);
    }
}
