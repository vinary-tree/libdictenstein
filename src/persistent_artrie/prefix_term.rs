//! Page-aware prefix-iteration result types.
//!
//! Split out of byte `dict_impl.rs` (lines ~343-367) as part of the Phase-5
//! decomposition. These two structs are the public return types of
//! `iter_prefix_with_arena()` and `iter_prefix_with_values_and_arena()`,
//! which group terms by arena/page for I/O-efficient batch operations on
//! disk-resident tries.

/// A term with its arena location for page-aware batching.
///
/// Used by `iter_prefix_with_arena()` to enable I/O-efficient batch operations
/// by grouping terms that reside in the same disk arena/page.
#[derive(Debug, Clone)]
pub struct PrefixTermWithArena {
    /// The term bytes
    pub term: Vec<u8>,
    /// The arena ID where this term's node resides (None for in-memory nodes)
    pub arena_id: Option<u32>,
}

/// A term with its value and arena location for page-aware merge operations.
///
/// Used by `iter_prefix_with_values_and_arena()` to enable I/O-efficient batch
/// operations by grouping terms that reside in the same disk arena/page.
#[derive(Debug, Clone)]
pub struct PrefixTermWithValueAndArena<V> {
    /// The term bytes
    pub term: Vec<u8>,
    /// The value associated with this term
    pub value: V,
    /// The arena ID where this term's node resides (None for in-memory nodes)
    pub arena_id: Option<u32>,
}
