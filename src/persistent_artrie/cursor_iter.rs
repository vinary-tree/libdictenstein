//! Cursor-based prefix iteration for `PersistentARTrie<V, S>`.
//!
//! Split out of byte `dict_impl.rs` (Phase-5 byte sub-module). Powers
//! memory-bounded batched iteration used by `merge_api`'s batched merge paths:
//!
//! - `iter_prefix_from_cursor` (pub) — returns up to `limit` terms with
//!   their values + arena IDs, starting strictly after `cursor`
//!
//! **L3.3c:** the owned tree is gone, so this enumerates the value-carrying overlay;
//! the owned DFS collectors (`collect_terms_from_cursor` /
//! `collect_terms_with_cursor_and_arena`) were deleted.

use crate::value::DictionaryValue;

use super::block_storage::BlockStorage;
use super::dict_impl::{bytes_gt, PersistentARTrie, PrefixTermWithValueAndArena};
use super::error::Result;

impl<V: DictionaryValue, S: BlockStorage> PersistentARTrie<V, S> {
    /// Iterate terms with values starting from a cursor position.
    ///
    /// This method enables memory-bounded iteration by returning terms in batches.
    /// The cursor allows resuming iteration from where the previous batch ended.
    ///
    /// # Arguments
    ///
    /// * `prefix` - Only return terms starting with this prefix
    /// * `cursor` - If Some, skip terms <= cursor (exclusive lower bound)
    /// * `limit` - Maximum number of terms to return
    ///
    /// # Returns
    ///
    /// A vector of terms (sorted lexicographically) starting after the cursor,
    /// up to the specified limit.
    pub fn iter_prefix_from_cursor(
        &self,
        prefix: &[u8],
        cursor: Option<&[u8]>,
        limit: usize,
    ) -> Result<Vec<PrefixTermWithValueAndArena<V>>>
    where
        V: Clone,
    {
        // **M3 read-flip (C6).** This is the memory-bounded merge-read chokepoint
        // (the batched merges + the parallel merge funnel through it). Under
        // `route_overlay()` enumerate the prefix from the VALUE-CARRYING overlay
        // (non-faulting, resident-finals; `arena_id` None), then apply the same
        // cursor (exclusive `> cursor`) + limit the owned collector applies. The
        // value-carrying route satisfies the audit §C.2 rule (no owned value
        // re-read). The owned arm below is the verbatim pre-flip cursor walk.
        let mut entries: Vec<PrefixTermWithValueAndArena<V>> = self
            .overlay_iter_prefix_with_values(prefix)
            .unwrap_or_default()
            .into_iter()
            .filter(|(term, _)| match cursor {
                Some(c) => bytes_gt(term.as_slice(), c),
                None => true,
            })
            .map(|(term, value)| PrefixTermWithValueAndArena {
                term,
                value,
                arena_id: None,
            })
            .collect();
        entries.sort_by(|a, b| a.term.cmp(&b.term));
        entries.truncate(limit);
        Ok(entries)
    }

    // L3.3c: removed — `collect_terms_from_cursor` + `collect_terms_with_cursor_and_arena`
    // walked the deleted owned `self.root` / `TrieRoot` / `ChildNode` representation. The
    // public `iter_prefix_from_cursor` above enumerates the value-carrying overlay.
}
