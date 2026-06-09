//! Arena-aware prefix iteration for `PersistentARTrie<V, S>`.
//!
//! Split out of byte `dict_impl.rs` (Phase-5 byte sub-module). The public entry
//! points:
//!
//! - `iter_prefix_with_arena` / `iter_prefix_with_values_and_arena` —
//!   public prefix-enumeration entry points
//!
//! **L3.3c:** the owned tree is gone, so these now enumerate the lock-free overlay
//! directly (`arena_id` is `None` for every resident overlay term); the owned DFS
//! collectors + `navigate_to_prefix_with_arena` were deleted.

use crate::value::DictionaryValue;

use super::block_storage::BlockStorage;
use super::dict_impl::{PersistentARTrie, PrefixTermWithArena, PrefixTermWithValueAndArena};
use super::error::Result;

impl<V: DictionaryValue, S: BlockStorage> PersistentARTrie<V, S> {
    // =========================================================================
    // Arena-aware iteration and merge operations
    // =========================================================================

    // L3.3c: removed — `navigate_to_prefix_with_arena` + `collect_terms_with_arena`
    // + `collect_terms_with_values_and_arena` walked the deleted owned `self.root` /
    // `TrieRoot` / `ChildNode` representation. The public `iter_prefix_with_arena` /
    // `iter_prefix_with_values_and_arena` chokepoints now enumerate the overlay
    // directly.

    /// Iterate over all terms with the given prefix, including arena locations.
    ///
    /// Returns all terms matching the prefix along with their disk arena IDs.
    /// This enables page-aware batch operations by grouping terms by arena.
    ///
    /// # Arguments
    ///
    /// * `prefix` - The byte prefix to search for
    ///
    /// # Returns
    ///
    /// - `Ok(Some(vec))` - Vector of terms with arena info
    /// - `Ok(None)` - The prefix path doesn't exist
    /// - `Err` - An I/O error occurred
    pub fn iter_prefix_with_arena(
        &self,
        prefix: &[u8],
    ) -> Result<Option<Vec<PrefixTermWithArena>>> {
        // **M3 read-flip (C6) — L3.3c collapse.** This is the byte read CHOKEPOINT:
        // `iter` / `iter_prefix` / `compaction_snapshot` / the merge readers all funnel
        // through it. The owned tree is gone (L3.3c), so this is now unconditionally the
        // overlay enumeration. Overlay nodes are all resident (in-memory), so `arena_id`
        // is `None` for every term; arena grouping is a disk-page-locality no-op for the
        // in-memory overlay, but the TERMS are faithful (resident-finals, non-faulting).
        Ok(self.overlay_iter_prefix(prefix).map(|terms| {
            terms
                .into_iter()
                .map(|term| PrefixTermWithArena {
                    term,
                    arena_id: None,
                })
                .collect()
        }))
    }

    /// Iterate over all terms with values and arena locations for the given prefix.
    ///
    /// Returns all (term, value, arena_id) tuples matching the prefix.
    /// This enables page-locality optimized merge operations.
    ///
    /// **M3 read-flip (C6 + the audit's §C.2 VALUE-CARRYING rule).** Under
    /// `route_overlay()` this routes to the VALUE-CARRYING overlay enumerator
    /// [`overlay_iter_prefix_with_values`](Self::overlay_iter_prefix_with_values) —
    /// NOT enumerate-overlay-then-value-owned (which would re-read each value from
    /// the EMPTY owned tree). `arena_id` is `None` for every resident overlay term.
    /// This is the value-carrying read chokepoint: `iter_prefix_with_values` funnels
    /// through it, so routing it here routes that surface too. The owned arm below is
    /// the verbatim pre-flip walk.
    pub fn iter_prefix_with_values_and_arena(
        &self,
        prefix: &[u8],
    ) -> Result<Option<Vec<PrefixTermWithValueAndArena<V>>>>
    where
        V: Clone,
    {
        // **M3 read-flip (C6 + §C.2 VALUE-CARRYING rule) — L3.3c collapse.** The owned
        // tree is gone (L3.3c), so this routes unconditionally to the VALUE-CARRYING
        // overlay enumerator
        // [`overlay_iter_prefix_with_values`](Self::overlay_iter_prefix_with_values).
        // `arena_id` is `None` for every resident overlay term. This is the
        // value-carrying read chokepoint: `iter_prefix_with_values` funnels through it.
        Ok(self.overlay_iter_prefix_with_values(prefix).map(|entries| {
            entries
                .into_iter()
                .map(|(term, value)| PrefixTermWithValueAndArena {
                    term,
                    value,
                    arena_id: None,
                })
                .collect()
        }))
    }
}
