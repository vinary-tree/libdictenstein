//! Public prefix-iteration + remove-prefix API for `PersistentARTrieChar<V, S>`.
//!
//! Methods covered:
//!
//! - `iter_prefix` (Result<Option<Vec<String>>>)
//! - `iter_prefix_with_values` (Result<Option<Vec<(String, V)>>>)
//! - `iter_prefix_with_arena` (Result<Option<Vec<PrefixTermWithArena>>>)
//! - `iter_prefix_with_values_and_arena`
//! - `remove_prefix` / `remove_prefix_batched`
//!
//! **L3.3c:** the owned tree is deleted; every method enumerates / removes via the
//! lock-free overlay (the sole representation). Arena grouping was an owned-tree
//! disk-locality optimization with no overlay analogue, so `arena_id` is `None`.

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::error::Result;
use crate::value::DictionaryValue;

use super::prefix_term::{PrefixTermWithArena, PrefixTermWithValueAndArena};

impl<V: DictionaryValue, S: BlockStorage> super::PersistentARTrieChar<V, S> {
    pub fn iter_prefix(&self, prefix: &str) -> Result<Option<Vec<String>>> {
        // L3.3c: the overlay is the sole representation; enumerate the immutable overlay
        // (non-faulting, resident-finals — see `overlay_read`).
        self.overlay_iter_prefix(prefix)
    }

    /// Iterate over all (term, value) pairs with the given prefix.
    ///
    /// Returns `Ok(None)` if the prefix path doesn't exist, `Ok(Some(vec))` otherwise.
    pub fn iter_prefix_with_values(&self, prefix: &str) -> Result<Option<Vec<(String, V)>>>
    where
        V: Clone,
    {
        // L3.3c: enumerate the overlay (the `u64` counter per final, or the synthesized
        // `()` for membership — see `overlay_read`).
        self.overlay_iter_prefix_with_values(prefix)
    }

    /// Iterate over all terms with the given prefix, including arena information.
    ///
    /// **L3.3c:** overlay nodes are all resident (in-memory), so `arena_id` is `None` for
    /// every term — exactly the value the owned path returned for not-yet-persisted nodes.
    /// Arena grouping was a disk-page-locality optimization that is a no-op for the
    /// in-memory overlay; the TERMS are returned faithfully (resident-finals, non-faulting).
    pub fn iter_prefix_with_arena(&self, prefix: &str) -> Result<Option<Vec<PrefixTermWithArena>>> {
        Ok(self.overlay_iter_prefix(prefix)?.map(|terms| {
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
    /// **L3.3c:** see [`Self::iter_prefix_with_arena`] — overlay terms are resident,
    /// `arena_id` is `None`.
    pub fn iter_prefix_with_values_and_arena(
        &self,
        prefix: &str,
    ) -> Result<Option<Vec<PrefixTermWithValueAndArena<V>>>>
    where
        V: Clone,
    {
        Ok(self
            .overlay_iter_prefix_with_values(prefix)?
            .map(|entries| {
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

    /// Remove all terms with the given prefix. Uses a default batch size of 1024.
    pub fn remove_prefix(&self, prefix: &str) -> Result<usize> {
        self.remove_prefix_batched(prefix, 1024)
    }

    /// **L3.3c overlay prefix removal:** enumerate the prefix subtree from the immutable
    /// overlay (non-faulting, resident-finals — see `overlay_read`) and durably remove each
    /// term via the Order-A `remove_cas_durable` path. Durable, so reopen sees the removals.
    fn remove_prefix_overlay(&self, prefix: &str) -> Result<usize> {
        // Snapshot the matching terms first (one resident enumeration), then remove each —
        // `remove_cas_durable` republishes the overlay root per call, so we must not hold a
        // borrow of the tree across the removals.
        let terms = match self.overlay_iter_prefix(prefix)? {
            Some(terms) => terms,
            None => return Ok(0),
        };
        let mut removed = 0usize;
        for term in &terms {
            if self.remove_cas_durable(term)? {
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// Remove all terms with the given prefix.
    ///
    /// **L3.3c:** there is no owned arena to group by, so remove via the overlay (the
    /// arena page-locality grouping was an owned-tree disk-layout optimization with no
    /// overlay analogue; the removal SEMANTICS are fully preserved). `batch_size` is
    /// vestigial under the overlay.
    pub fn remove_prefix_batched(&self, prefix: &str, _batch_size: usize) -> Result<usize> {
        self.remove_prefix_overlay(prefix)
    }
}
