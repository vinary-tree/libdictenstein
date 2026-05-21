//! Prefetch helpers for `PersistentARTrieChar<V, S>`.
//!
//! Split out of char `dict_impl_char.rs` (lines ~343-418, ~76 LOC)
//! as the twenty-first Phase-6 char sub-module. Methods covered:
//!
//! - `prefetch_stats` — snapshot of prefetcher counters
//! - `prefetch_disk_refs_bounded` — depth-bounded multi-level prefetch
//!   for DiskRef children (pub(super) so query_api etc. can drive it)

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::value::DictionaryValue;

impl<V: DictionaryValue, S: BlockStorage> super::PersistentARTrieChar<V, S> {
    // ==================== Prefetching Methods ====================

    /// Get a snapshot of prefetch statistics.
    ///
    /// Returns statistics about prefetch performance including:
    /// - Total requests submitted
    /// - Cache hits (prefetched data was already in memory)
    /// - I/O operations issued
    /// - Dropped requests (queue overflow)
    ///
    /// # Example
    ///
    /// ```ignore
    /// let stats = trie.prefetch_stats();
    /// println!("Prefetch hit rate: {:.1}%", stats.hit_rate() * 100.0);
    /// println!("Drop rate: {:.1}%", stats.drop_rate() * 100.0);
    /// ```
    pub fn prefetch_stats(&self) -> crate::persistent_artrie::prefetch::PrefetchStatsSnapshot {
        self.prefetcher.stats().snapshot()
    }

    // DISABLED — `prefetch_disk_refs` was the original depth-0 convenience
    // wrapper for `prefetch_disk_refs_bounded`; it is fully superseded by
    // the bounded variant immediately below, which all callers in this
    // file already use directly (lines 2533, 2573, 3453, 3495). Kept here
    // commented out per CLAUDE.md to preserve the rename audit trail.
    //
    // fn prefetch_disk_refs<'a>(
    //     &self,
    //     children: impl Iterator<Item = (u32, &'a crate::persistent_artrie::swizzled_ptr::SwizzledPtr)>,
    // ) {
    //     self.prefetch_disk_refs_bounded(children, 0);
    // }

    /// Prefetch disk-resident children with depth bounds for multi-level prefetching.
    ///
    /// This method extends prefetching to all traversal levels, not just the root.
    /// When the prefetcher is configured with `DepthLimited(n)` strategy, prefetching
    /// will be disabled for nodes deeper than `n` levels, preventing excessive I/O
    /// for very deep tries.
    ///
    /// # Performance
    ///
    /// Multi-level prefetching improves cold lookup performance by 15-30% by
    /// initiating I/O for nodes at depth D while processing nodes at depth D-1.
    /// With default `DepthLimited(3)`, prefetching occurs for the first 4 levels.
    ///
    /// # Arguments
    ///
    /// * `children` - Iterator over (char_codepoint, &SwizzledPtr) pairs to potentially prefetch
    /// * `depth` - Current traversal depth (0 = root level)
    pub(super) fn prefetch_disk_refs_bounded<'a>(
        &self,
        children: impl Iterator<Item = (u32, &'a crate::persistent_artrie::swizzled_ptr::SwizzledPtr)>,
        depth: u16,
    ) {
        // Collect disk-resident children for prefetching
        // Use low byte of codepoint as key proxy for the prefetcher
        let disk_children: Vec<(u8, crate::persistent_artrie::swizzled_ptr::SwizzledPtr)> = children
            .filter_map(|(codepoint, ptr)| {
                if ptr.is_on_disk() {
                    // Use low byte of codepoint as routing key
                    let key_byte = (codepoint & 0xFF) as u8;
                    Some((key_byte, ptr.clone()))
                } else {
                    None
                }
            })
            .collect();

        if !disk_children.is_empty() {
            self.prefetcher.prefetch_children_bounded(&disk_children, depth);
        }
    }
}
