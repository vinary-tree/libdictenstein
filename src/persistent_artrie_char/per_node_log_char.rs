//! Per-Node Logging for Char Trie (4-Byte Keys).
//!
//! This module adapts the per-node logging system for the 4-byte char trie.
//! The primary difference from the 1-byte implementation is that child keys
//! are `u32` (Unicode code points) instead of `u8` (ASCII bytes).
//!
//! # Key Differences from 1-Byte Implementation
//!
//! | Aspect | 1-Byte (ASCII) | 4-Byte (Char) |
//! |--------|----------------|---------------|
//! | Child key type | `u8` | `u32` |
//! | InsertChild size | 10 bytes | 13 bytes |
//! | RemoveChild size | 2 bytes | 5 bytes |
//! | Prefix type | `Vec<u8>` | `Vec<u32>` |
//!
//! # Re-exported Types
//!
//! The following types are re-exported from the 1-byte implementation as they
//! are node-agnostic:
//! - `NodeId`, `PageId` - Type aliases
//! - `PerNodeLogConfig` - Configuration
//! - `InlineLog` - Log storage (works with raw bytes)
//! - `DirtyNodeTracker` - Tracks dirty nodes
//! - `PerNodeLogStats`, `PerNodeLogStatsAtomic` - Statistics

// Re-export node-agnostic types from the 1-byte implementation
pub use crate::persistent_artrie::per_node_log::{
    DirtyNodeTracker, NodeId, NodeRecoveryResult, PageId, PerNodeLogConfig,
    PerNodeLogStats, PerNodeLogStatsAtomic, RecoveryResult,
};

/// Inline log for char node entries (4-byte keys).
///
/// This is the char-specific version of InlineLog that handles CharNodeLogEntry
/// serialization. The main differences from the 1-byte version:
/// - Stores u32 keys instead of u8 keys
/// - InsertChild entries are 13 bytes (vs 10 bytes)
/// - RemoveChild entries are 5 bytes (vs 2 bytes)
/// - SetPrefix stores u32 code points
#[derive(Debug, Clone)]
pub struct CharInlineLog {
    /// Raw log data
    data: Vec<u8>,
    /// Maximum capacity
    capacity: usize,
    /// Current used length
    len: usize,
    /// Number of log entries
    entry_count: usize,
}

impl CharInlineLog {
    /// Create a new inline log with the given capacity.
    pub fn new(capacity: usize) -> Self {
        Self {
            data: vec![0u8; capacity],
            capacity,
            len: 0,
            entry_count: 0,
        }
    }

    /// Create from existing data.
    pub fn from_data(data: Vec<u8>, entry_count: usize) -> Self {
        let len = data.len();
        let capacity = data.capacity().max(len);
        Self {
            data,
            capacity,
            len,
            entry_count,
        }
    }

    /// Available space in bytes.
    pub fn available_space(&self) -> usize {
        self.capacity.saturating_sub(self.len)
    }

    /// Current used space in bytes.
    pub fn used_space(&self) -> usize {
        self.len
    }

    /// Number of log entries.
    pub fn entry_count(&self) -> usize {
        self.entry_count
    }

    /// Check if log is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Get the log data as a slice.
    pub fn as_slice(&self) -> &[u8] {
        &self.data[..self.len]
    }

    /// Try to append an entry to the inline log.
    ///
    /// Returns true if successful, false if not enough space.
    pub fn try_append(&mut self, entry: &CharNodeLogEntry) -> bool {
        let serialized = entry.serialize();
        if serialized.len() > self.available_space() {
            return false;
        }

        let start = self.len;
        let end = start + serialized.len();

        // Extend data if needed
        if end > self.data.len() {
            self.data.resize(end.max(self.capacity), 0);
        }

        self.data[start..end].copy_from_slice(&serialized);
        self.len = end;
        self.entry_count += 1;
        true
    }

    /// Clear the log (resets to empty).
    pub fn clear(&mut self) {
        self.len = 0;
        self.entry_count = 0;
    }

    /// Iterate over entries.
    pub fn iter(&self) -> CharInlineLogIter<'_> {
        CharInlineLogIter::new(self.as_slice())
    }

    /// Compact the log by removing redundant entries.
    ///
    /// This uses the `cancels()` and `supersedes()` methods on CharNodeLogEntry
    /// to eliminate redundant entries.
    pub fn compact(&mut self) {
        if self.entry_count <= 1 {
            return;
        }

        // Collect all entries
        let entries: Vec<CharNodeLogEntry> = self.iter().collect();

        // Keep only entries that aren't cancelled or superseded by later entries
        let mut kept = Vec::with_capacity(entries.len());
        for (i, entry) in entries.iter().enumerate() {
            let mut keep = true;
            // Check if any later entry cancels or supersedes this one
            for later_entry in &entries[i + 1..] {
                if later_entry.cancels(entry) || later_entry.supersedes(entry) {
                    keep = false;
                    break;
                }
            }
            if keep {
                kept.push(entry.clone());
            }
        }

        // Rebuild the log with kept entries
        self.clear();
        for entry in kept {
            self.try_append(&entry);
        }
    }
}

/// Log entry for per-node redo logging (4-byte char keys).
///
/// Each entry represents a single modification to a char trie node.
/// These entries are stored inline with nodes or in overflow pages,
/// enabling O(dirty nodes) recovery.
///
/// # Serialized Format
///
/// | Entry Type | Format | Size |
/// |------------|--------|------|
/// | InsertChild | `[0x01][key:4][child_id:8]` | 13 bytes |
/// | RemoveChild | `[0x02][key:4]` | 5 bytes |
/// | SetValue | `[0x03][len:2][value:len]` | 3 + len bytes |
/// | ClearValue | `[0x04]` | 1 byte |
/// | SetPrefix | `[0x05][len:2][prefix:len*4]` | 3 + len*4 bytes |
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CharNodeLogEntry {
    /// Insert a child edge.
    InsertChild {
        /// Key (Unicode code point) for the child edge
        key: u32,
        /// Node ID of the child
        child_id: NodeId,
    },

    /// Remove a child edge.
    RemoveChild {
        /// Key (Unicode code point) for the child edge to remove
        key: u32,
    },

    /// Update the node's value (for leaf nodes).
    SetValue {
        /// Serialized value bytes
        value: Vec<u8>,
    },

    /// Clear the node's value.
    ClearValue,

    /// Update prefix (path compression).
    SetPrefix {
        /// New prefix (Unicode code points)
        prefix: Vec<u32>,
    },
}

/// Log entry type discriminators for serialization.
mod log_entry_type {
    pub const INSERT_CHILD: u8 = 0x01;
    pub const REMOVE_CHILD: u8 = 0x02;
    pub const SET_VALUE: u8 = 0x03;
    pub const CLEAR_VALUE: u8 = 0x04;
    pub const SET_PREFIX: u8 = 0x05;
}

impl CharNodeLogEntry {
    /// Serialize the log entry to bytes.
    ///
    /// # Format
    /// - InsertChild: `[0x01][key:4 LE][child_id:8 LE]` = 13 bytes
    /// - RemoveChild: `[0x02][key:4 LE]` = 5 bytes
    /// - SetValue: `[0x03][len:2 LE][value:len]` = 3 + len bytes
    /// - ClearValue: `[0x04]` = 1 byte
    /// - SetPrefix: `[0x05][len:2 LE][prefix:len*4 LE]` = 3 + len*4 bytes
    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(16);

        match self {
            CharNodeLogEntry::InsertChild { key, child_id } => {
                buf.push(log_entry_type::INSERT_CHILD);
                buf.extend_from_slice(&key.to_le_bytes());
                buf.extend_from_slice(&child_id.to_le_bytes());
            }
            CharNodeLogEntry::RemoveChild { key } => {
                buf.push(log_entry_type::REMOVE_CHILD);
                buf.extend_from_slice(&key.to_le_bytes());
            }
            CharNodeLogEntry::SetValue { value } => {
                buf.push(log_entry_type::SET_VALUE);
                let len = value.len() as u16;
                buf.extend_from_slice(&len.to_le_bytes());
                buf.extend_from_slice(value);
            }
            CharNodeLogEntry::ClearValue => {
                buf.push(log_entry_type::CLEAR_VALUE);
            }
            CharNodeLogEntry::SetPrefix { prefix } => {
                buf.push(log_entry_type::SET_PREFIX);
                let len = prefix.len() as u16;
                buf.extend_from_slice(&len.to_le_bytes());
                for &cp in prefix {
                    buf.extend_from_slice(&cp.to_le_bytes());
                }
            }
        }

        buf
    }

    /// Deserialize a log entry from bytes.
    ///
    /// Returns `Some((entry, consumed_bytes))` on success, `None` on failure.
    pub fn deserialize(data: &[u8]) -> Option<(Self, usize)> {
        if data.is_empty() {
            return None;
        }

        let entry_type = data[0];
        match entry_type {
            log_entry_type::INSERT_CHILD => {
                if data.len() < 13 {
                    return None;
                }
                let key = u32::from_le_bytes([data[1], data[2], data[3], data[4]]);
                let child_id = u64::from_le_bytes([
                    data[5], data[6], data[7], data[8], data[9], data[10], data[11], data[12],
                ]);
                Some((CharNodeLogEntry::InsertChild { key, child_id }, 13))
            }
            log_entry_type::REMOVE_CHILD => {
                if data.len() < 5 {
                    return None;
                }
                let key = u32::from_le_bytes([data[1], data[2], data[3], data[4]]);
                Some((CharNodeLogEntry::RemoveChild { key }, 5))
            }
            log_entry_type::SET_VALUE => {
                if data.len() < 3 {
                    return None;
                }
                let len = u16::from_le_bytes([data[1], data[2]]) as usize;
                if data.len() < 3 + len {
                    return None;
                }
                let value = data[3..3 + len].to_vec();
                Some((CharNodeLogEntry::SetValue { value }, 3 + len))
            }
            log_entry_type::CLEAR_VALUE => Some((CharNodeLogEntry::ClearValue, 1)),
            log_entry_type::SET_PREFIX => {
                if data.len() < 3 {
                    return None;
                }
                let len = u16::from_le_bytes([data[1], data[2]]) as usize;
                if data.len() < 3 + len * 4 {
                    return None;
                }
                let mut prefix = Vec::with_capacity(len);
                for i in 0..len {
                    let offset = 3 + i * 4;
                    let cp = u32::from_le_bytes([
                        data[offset],
                        data[offset + 1],
                        data[offset + 2],
                        data[offset + 3],
                    ]);
                    prefix.push(cp);
                }
                Some((CharNodeLogEntry::SetPrefix { prefix }, 3 + len * 4))
            }
            _ => None,
        }
    }

    /// Returns the serialized size of this entry without actually serializing.
    pub fn serialized_size(&self) -> usize {
        match self {
            CharNodeLogEntry::InsertChild { .. } => 13,
            CharNodeLogEntry::RemoveChild { .. } => 5,
            CharNodeLogEntry::SetValue { value } => 3 + value.len(),
            CharNodeLogEntry::ClearValue => 1,
            CharNodeLogEntry::SetPrefix { prefix } => 3 + prefix.len() * 4,
        }
    }

    /// Returns true if this entry cancels/reverses another entry.
    ///
    /// Used during log compaction to eliminate redundant entries.
    pub fn cancels(&self, other: &Self) -> bool {
        match (self, other) {
            // RemoveChild cancels InsertChild for the same key
            (
                CharNodeLogEntry::RemoveChild { key: k1 },
                CharNodeLogEntry::InsertChild { key: k2, .. },
            ) => k1 == k2,
            // ClearValue cancels SetValue
            (CharNodeLogEntry::ClearValue, CharNodeLogEntry::SetValue { .. }) => true,
            _ => false,
        }
    }

    /// Returns true if this entry supersedes another entry.
    ///
    /// When both entries modify the same field, the newer one supersedes.
    pub fn supersedes(&self, other: &Self) -> bool {
        match (self, other) {
            // InsertChild supersedes previous InsertChild for same key
            (
                CharNodeLogEntry::InsertChild { key: k1, .. },
                CharNodeLogEntry::InsertChild { key: k2, .. },
            ) => k1 == k2,
            // SetValue supersedes previous SetValue
            (CharNodeLogEntry::SetValue { .. }, CharNodeLogEntry::SetValue { .. }) => true,
            // SetPrefix supersedes previous SetPrefix
            (CharNodeLogEntry::SetPrefix { .. }, CharNodeLogEntry::SetPrefix { .. }) => true,
            _ => false,
        }
    }
}

/// Iterator over log entries in an inline log (4-byte char keys).
pub struct CharInlineLogIter<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> CharInlineLogIter<'a> {
    /// Create a new iterator over the given log data.
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, offset: 0 }
    }
}

impl<'a> Iterator for CharInlineLogIter<'a> {
    type Item = CharNodeLogEntry;

    fn next(&mut self) -> Option<Self::Item> {
        if self.offset >= self.data.len() {
            return None;
        }

        let (entry, consumed) = CharNodeLogEntry::deserialize(&self.data[self.offset..])?;
        self.offset += consumed;
        Some(entry)
    }
}

/// Helper trait to write log entries to an inline log.
pub trait CharLogWriter {
    /// Append a log entry. Returns true if successful, false if not enough space.
    fn append_char_entry(&mut self, entry: &CharNodeLogEntry) -> bool;
}

impl CharLogWriter for CharInlineLog {
    fn append_char_entry(&mut self, entry: &CharNodeLogEntry) -> bool {
        self.try_append(entry)
    }
}

/// Extension trait for iterating char log entries from a CharInlineLog.
pub trait CharLogIterExt {
    /// Iterate over char node log entries.
    fn char_entries(&self) -> CharInlineLogIter<'_>;
}

impl CharLogIterExt for CharInlineLog {
    fn char_entries(&self) -> CharInlineLogIter<'_> {
        self.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_insert_child_serialization() {
        let entry = CharNodeLogEntry::InsertChild {
            key: 0x4E2D, // '中' Chinese character
            child_id: 12345,
        };

        let bytes = entry.serialize();
        assert_eq!(bytes.len(), 13);
        assert_eq!(bytes[0], log_entry_type::INSERT_CHILD);

        let (deserialized, consumed) = CharNodeLogEntry::deserialize(&bytes).unwrap();
        assert_eq!(consumed, 13);
        assert_eq!(deserialized, entry);
    }

    #[test]
    fn test_remove_child_serialization() {
        let entry = CharNodeLogEntry::RemoveChild {
            key: 0x1F600, // 😀 emoji
        };

        let bytes = entry.serialize();
        assert_eq!(bytes.len(), 5);
        assert_eq!(bytes[0], log_entry_type::REMOVE_CHILD);

        let (deserialized, consumed) = CharNodeLogEntry::deserialize(&bytes).unwrap();
        assert_eq!(consumed, 5);
        assert_eq!(deserialized, entry);
    }

    #[test]
    fn test_set_value_serialization() {
        let entry = CharNodeLogEntry::SetValue {
            value: vec![1, 2, 3, 4, 5],
        };

        let bytes = entry.serialize();
        assert_eq!(bytes.len(), 8); // 1 + 2 + 5

        let (deserialized, consumed) = CharNodeLogEntry::deserialize(&bytes).unwrap();
        assert_eq!(consumed, 8);
        assert_eq!(deserialized, entry);
    }

    #[test]
    fn test_clear_value_serialization() {
        let entry = CharNodeLogEntry::ClearValue;

        let bytes = entry.serialize();
        assert_eq!(bytes.len(), 1);

        let (deserialized, consumed) = CharNodeLogEntry::deserialize(&bytes).unwrap();
        assert_eq!(consumed, 1);
        assert_eq!(deserialized, entry);
    }

    #[test]
    fn test_set_prefix_serialization() {
        let entry = CharNodeLogEntry::SetPrefix {
            prefix: vec![0x4E2D, 0x6587], // "中文" (Chinese)
        };

        let bytes = entry.serialize();
        assert_eq!(bytes.len(), 11); // 1 + 2 + 2*4

        let (deserialized, consumed) = CharNodeLogEntry::deserialize(&bytes).unwrap();
        assert_eq!(consumed, 11);
        assert_eq!(deserialized, entry);
    }

    #[test]
    fn test_serialized_size() {
        let entries = [
            (CharNodeLogEntry::InsertChild { key: 0, child_id: 0 }, 13),
            (CharNodeLogEntry::RemoveChild { key: 0 }, 5),
            (CharNodeLogEntry::SetValue { value: vec![1, 2, 3] }, 6),
            (CharNodeLogEntry::ClearValue, 1),
            (CharNodeLogEntry::SetPrefix { prefix: vec![1, 2] }, 11),
        ];

        for (entry, expected_size) in entries {
            assert_eq!(
                entry.serialized_size(),
                expected_size,
                "Size mismatch for {:?}",
                entry
            );
            assert_eq!(entry.serialize().len(), expected_size);
        }
    }

    #[test]
    fn test_cancels() {
        let insert = CharNodeLogEntry::InsertChild {
            key: 42,
            child_id: 100,
        };
        let remove = CharNodeLogEntry::RemoveChild { key: 42 };
        let remove_other = CharNodeLogEntry::RemoveChild { key: 43 };

        assert!(remove.cancels(&insert));
        assert!(!remove_other.cancels(&insert));

        let set_value = CharNodeLogEntry::SetValue { value: vec![1] };
        let clear_value = CharNodeLogEntry::ClearValue;

        assert!(clear_value.cancels(&set_value));
        assert!(!set_value.cancels(&clear_value));
    }

    #[test]
    fn test_supersedes() {
        let insert1 = CharNodeLogEntry::InsertChild {
            key: 42,
            child_id: 100,
        };
        let insert2 = CharNodeLogEntry::InsertChild {
            key: 42,
            child_id: 200,
        };
        let insert3 = CharNodeLogEntry::InsertChild {
            key: 43,
            child_id: 300,
        };

        assert!(insert2.supersedes(&insert1));
        assert!(!insert3.supersedes(&insert1));

        let set1 = CharNodeLogEntry::SetValue { value: vec![1] };
        let set2 = CharNodeLogEntry::SetValue { value: vec![2] };

        assert!(set2.supersedes(&set1));
    }

    #[test]
    fn test_inline_log_integration() {
        let mut log = CharInlineLog::new(128);

        let entry1 = CharNodeLogEntry::InsertChild {
            key: 0x4E2D,
            child_id: 1,
        };
        let entry2 = CharNodeLogEntry::SetValue {
            value: vec![1, 2, 3],
        };
        let entry3 = CharNodeLogEntry::RemoveChild { key: 0x6587 };

        assert!(log.append_char_entry(&entry1));
        assert!(log.append_char_entry(&entry2));
        assert!(log.append_char_entry(&entry3));

        // Iterate and verify
        let entries: Vec<_> = log.char_entries().collect();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0], entry1);
        assert_eq!(entries[1], entry2);
        assert_eq!(entries[2], entry3);
    }

    #[test]
    fn test_inline_log_compact() {
        let mut log = CharInlineLog::new(256);

        // Add entries that can be compacted
        log.try_append(&CharNodeLogEntry::InsertChild { key: 42, child_id: 1 });
        log.try_append(&CharNodeLogEntry::SetValue { value: vec![1] });
        log.try_append(&CharNodeLogEntry::InsertChild { key: 42, child_id: 2 }); // supersedes first
        log.try_append(&CharNodeLogEntry::SetValue { value: vec![2] }); // supersedes second
        log.try_append(&CharNodeLogEntry::RemoveChild { key: 42 }); // cancels third

        assert_eq!(log.entry_count(), 5);

        log.compact();

        // After compaction: only the final SetValue should remain
        // - First InsertChild is superseded by third InsertChild
        // - Third InsertChild is cancelled by RemoveChild
        // - RemoveChild should be kept (no later entry cancels it)
        // - First SetValue is superseded by second SetValue
        // - Second SetValue should be kept
        let entries: Vec<_> = log.iter().collect();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], CharNodeLogEntry::SetValue { value: vec![2] });
        assert_eq!(entries[1], CharNodeLogEntry::RemoveChild { key: 42 });
    }

    #[test]
    fn test_inline_log_capacity() {
        let mut log = CharInlineLog::new(20);

        // InsertChild is 13 bytes - should fit
        assert!(log.try_append(&CharNodeLogEntry::InsertChild { key: 1, child_id: 1 }));
        assert_eq!(log.used_space(), 13);
        assert_eq!(log.available_space(), 7);

        // Another InsertChild (13 bytes) won't fit in remaining 7 bytes
        assert!(!log.try_append(&CharNodeLogEntry::InsertChild { key: 2, child_id: 2 }));
        assert_eq!(log.entry_count(), 1);

        // RemoveChild (5 bytes) should fit
        assert!(log.try_append(&CharNodeLogEntry::RemoveChild { key: 3 }));
        assert_eq!(log.entry_count(), 2);
        assert_eq!(log.used_space(), 18);
    }

    #[test]
    fn test_unicode_range() {
        // Test full Unicode range
        let entries = [
            CharNodeLogEntry::InsertChild {
                key: 0x0000, // NULL
                child_id: 1,
            },
            CharNodeLogEntry::InsertChild {
                key: 0x007F, // DEL (end of ASCII)
                child_id: 2,
            },
            CharNodeLogEntry::InsertChild {
                key: 0x0080, // Start of Latin-1 Supplement
                child_id: 3,
            },
            CharNodeLogEntry::InsertChild {
                key: 0xFFFF, // End of BMP
                child_id: 4,
            },
            CharNodeLogEntry::InsertChild {
                key: 0x10000, // Start of SMP (first surrogate pair in UTF-16)
                child_id: 5,
            },
            CharNodeLogEntry::InsertChild {
                key: 0x10FFFF, // Maximum Unicode code point
                child_id: 6,
            },
        ];

        for entry in entries {
            let bytes = entry.serialize();
            let (deserialized, _) = CharNodeLogEntry::deserialize(&bytes).unwrap();
            assert_eq!(deserialized, entry);
        }
    }
}
