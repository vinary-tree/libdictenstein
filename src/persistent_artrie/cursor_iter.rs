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
        // **C6 — L3.3c collapse.** This is the memory-bounded merge-read chokepoint
        // (the batched merges + the parallel merge funnel through it). The overlay is
        // the sole representation, so enumerate the prefix from the VALUE-CARRYING
        // overlay (non-faulting, resident-finals; `arena_id` None), then apply the
        // cursor (exclusive `> cursor`) + limit. The value-carrying route satisfies the
        // audit §C.2 rule (no owned value re-read).
        if limit == 0 {
            return Ok(Vec::new());
        }

        let mut entries = Vec::with_capacity(limit);
        for (term, value) in self
            .overlay_iter_prefix_with_values(prefix)
            .unwrap_or_default()
        {
            if !match cursor {
                Some(c) => bytes_gt(term.as_slice(), c),
                None => true,
            } {
                continue;
            }
            entries.push(PrefixTermWithValueAndArena {
                term,
                value,
                arena_id: None,
            });
            if entries.len() == limit {
                break;
            }
        }

        Ok(entries)
    }

    // L3.3c: removed — `collect_terms_from_cursor` + `collect_terms_with_cursor_and_arena`
    // walked the deleted owned `self.root` / `TrieRoot` / `ChildNode` representation. The
    // public `iter_prefix_from_cursor` above enumerates the value-carrying overlay.
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(prefix: &str) -> tempfile::TempDir {
        std::fs::create_dir_all("target/test-tmp").expect("create target/test-tmp");
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in("target/test-tmp")
            .expect("scratch tempdir under target/test-tmp")
    }

    #[test]
    fn cursor_iteration_preserves_overlay_order_and_stops_at_limit() {
        let dir = scratch("byte-cursor-iter-order-");
        let trie: PersistentARTrie<u64> =
            PersistentARTrie::create(dir.path().join("dict.part")).expect("create trie");
        for (term, value) in [
            ("az", 5),
            ("aa", 1),
            ("ac", 3),
            ("ab", 2),
            ("ad", 4),
            ("b", 6),
        ] {
            trie.insert_with_value(term, value);
        }

        let entries = trie
            .iter_prefix_from_cursor(b"a", Some(b"aa"), 3)
            .expect("cursor iteration");
        let got: Vec<_> = entries
            .into_iter()
            .map(|entry| (String::from_utf8(entry.term).expect("utf8"), entry.value))
            .collect();

        assert_eq!(
            got,
            vec![
                ("ab".to_string(), 2),
                ("ac".to_string(), 3),
                ("ad".to_string(), 4),
            ]
        );
    }

    #[test]
    fn cursor_iteration_zero_limit_returns_empty_without_prefix_lookup() {
        let dir = scratch("byte-cursor-iter-zero-limit-");
        let trie: PersistentARTrie<u64> =
            PersistentARTrie::create(dir.path().join("dict.part")).expect("create trie");
        trie.insert_with_value("a", 1);

        let entries = trie
            .iter_prefix_from_cursor(b"missing", None, 0)
            .expect("zero-limit cursor iteration");

        assert!(entries.is_empty());
    }
}
