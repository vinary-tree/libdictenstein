//! Generic Double-Array Trie core shared between `DoubleArrayTrie<V>`
//! (byte-keyed) and `DoubleArrayTrieChar<V>` (Unicode-keyed).
//!
//! # Background
//!
//! The byte and char DAT variants used to be separate ~1200-LOC files
//! with the same algorithm differing only in edge-label type
//! (`Vec<Vec<u8>>` vs `Vec<Vec<char>>`). This module hosts the shared
//! generic type [`DATCoreShared<U, V>`] so both variants can share the
//! BASE/CHECK array storage and serde plumbing.
//!
//! The remaining algorithmic methods (insert, lookup, BASE-placement
//! search) are migration targets — see
//! `docs/benchmarks/c5-dat-generic-handoff.md` for the step-by-step
//! plan. Each algorithm method becomes a `DATCoreShared::method<U>`
//! once the byte/char variants are migrated.
//!
//! # Layout
//!
//! ```text
//! DATCoreShared<U, V>
//! ├── base:     Arc<Vec<i32>>            (state → BASE offset)
//! ├── check:    Arc<Vec<i32>>            (state → parent state, for verification)
//! ├── is_final: Arc<Vec<bool>>           (state → terminal flag)
//! ├── edges:    Arc<Vec<Vec<U>>>         (state → outgoing edge labels)
//! └── values:   Arc<Vec<Option<V>>>      (state → optional value)
//! ```

pub mod shared;

pub use shared::DATCoreShared;
