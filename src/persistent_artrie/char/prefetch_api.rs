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
}
