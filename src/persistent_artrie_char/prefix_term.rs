//! Page-aware prefix-iteration result types for the char trie.
//!
//! Split out of char `dict_impl_char.rs` (lines ~406-431) as the first
//! piece of the Phase-6 char/vocab decomposition, mirroring the byte
//! variant's `persistent_artrie::prefix_term`. These two structs are the
//! public return types of `iter_prefix_with_arena()` and
//! `iter_prefix_with_values_and_arena()` on the char trie. Char terms are
//! `String` (UTF-8) rather than `Vec<u8>` as in the byte variant.

/// A term with its arena location for page-aware batching.
///
/// Used by `iter_prefix_with_arena()` to enable I/O-efficient batch operations
/// by grouping terms that reside in the same disk arena/page.
#[derive(Debug, Clone)]
pub struct PrefixTermWithArena {
    /// The term string
    pub term: String,
    /// The arena ID where this term's node resides (None for in-memory nodes)
    pub arena_id: Option<u32>,
}

/// A term with its value and arena location for page-aware merge operations.
///
/// Used by `iter_prefix_with_values_and_arena()` to enable I/O-efficient batch
/// operations by grouping terms that reside in the same disk arena/page.
/// This is the same pattern used by `remove_prefix_batched()`.
#[derive(Debug, Clone)]
pub struct PrefixTermWithValueAndArena<V> {
    /// The term string
    pub term: String,
    /// The value associated with this term
    pub value: V,
    /// The arena ID where this term's node resides (None for in-memory nodes)
    pub arena_id: Option<u32>,
}
