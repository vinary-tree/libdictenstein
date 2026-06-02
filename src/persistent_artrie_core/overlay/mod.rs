//! Shared lock-free overlay node (G4 unification).
//!
//! The byte (`u8`) and char (`u32`) lock-free overlays used to carry
//! token-for-token-identical node implementations (`persistent_node.rs` /
//! `atomic_ptr.rs`) that differed only in the key-unit type, `MAX_PREFIX_LEN`
//! (12 vs 6), the inline zero filler (`0u8` vs `0u32`), and prose. G4 collapses
//! both into a single generic [`OverlayNode<K, V>`] / [`AtomicNodePtr<K, V>`]
//! parameterized over `K: KeyEncoding` (its `Unit` is the key-unit width) and
//! the value `V`. The variants alias it:
//!
//! ```text
//! // byte:  pub type PersistentNode<V = ()>     = OverlayNode<ByteKey, V>;
//! //        pub type AtomicNodePtr<V = ()>      = overlay::AtomicNodePtr<ByteKey, V>;
//! // char:  pub type PersistentCharNode<V = ()> = OverlayNode<CharKey, V>;
//! //        pub type AtomicNodePtr<V = ()>      = overlay::AtomicNodePtr<CharKey, V>;
//! // vocab: consumes the char alias at <u64> (unchanged).
//! ```
//!
//! Lives in `persistent_artrie_core` so the layering invariant holds: `SwizzledPtr`
//! is canonically `persistent_artrie_core::swizzled_ptr`, so this module imports it
//! with **zero** upward reference. Zero `unsafe` — `Send`/`Sync` auto-derive.

pub mod atomic_ptr;
pub mod node;

pub use atomic_ptr::AtomicNodePtr;
pub use node::{flags, Child, OverlayNode};

use std::sync::Arc;

use crate::persistent_artrie_core::key_encoding::KeyEncoding;
use crate::persistent_artrie_core::mvcc::TrieRoot;

/// G4 Phase 6 (DRY bonus): the single `TrieRoot` impl for the unified overlay
/// node, replacing the two near-identical per-variant impls (byte
/// `persistent_artrie::mvcc` and char `persistent_artrie_char::mvcc`).
///
/// `Key = K::Unit` (`u8` for byte, `u32` for char — both satisfy `Key: Copy`);
/// `Value = V`. For `OverlayNode<ByteKey, i64>` this yields `Key=u8, Value=i64`
/// (identical to the old hand-written byte impl) and for `OverlayNode<CharKey, V>`
/// it yields `Key=u32, Value=V` (identical to the old char impl) — so the blanket
/// subsumes both exactly. Coherence holds: both `TrieRoot` and `OverlayNode` live
/// in `persistent_artrie_core`, so the blanket is canonical here (no orphan-rule
/// issue, single crate).
impl<K: KeyEncoding, V: Clone + Send + Sync + 'static> TrieRoot for OverlayNode<K, V> {
    type Key = K::Unit;
    type Value = V;

    fn is_final(&self) -> bool {
        OverlayNode::is_final(self)
    }

    fn find_child(&self, key: K::Unit) -> Option<Arc<Self>> {
        // `as_in_mem` yields `None` for an on-disk (or absent) child, so this MVCC
        // snapshot read simply borrows the owned child `Arc` and clones it — the
        // old raw-pointer smuggling (`as_ptr` + `unsafe Arc::from_raw`) is gone.
        OverlayNode::find_child(self, key).and_then(|child| child.as_in_mem().map(Arc::clone))
    }

    fn get_value(&self) -> Option<V> {
        OverlayNode::get_value(self)
    }
}
