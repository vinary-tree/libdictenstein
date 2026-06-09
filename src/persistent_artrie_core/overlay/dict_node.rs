//! `OverlayDictionaryNode<K, V>` — the shared, key-encoding-generic overlay-backed
//! [`DictionaryNode`] handle (G5.1 unification).
//!
//! The byte (`u8`) and char (`u32`) lock-free overlays used to carry
//! token-for-token-identical `DictionaryNode` handles that differed ONLY in the key
//! encoding (`ByteKey` vs `CharKey`) and the public unit type they presented (`u8`
//! vs `char`). G5.1 collapses both into this single generic handle, parameterized
//! over `K: KeyEncoding`:
//!
//! ```text
//! // byte:  pub type PersistentARTrieNode<V = ()>     = OverlayDictionaryNode<ByteKey, V>;
//! // char:  pub type PersistentARTrieCharNode<V = ()> = OverlayDictionaryNode<CharKey, V>;
//! ```
//!
//! The public [`DictionaryNode::Unit`] is `K::Token` (`u8` for byte, `char` for
//! char), so the public surface — and the transducer / zipper integration that
//! depends on `Unit = u8` / `Unit = char` — is byte-for-byte preserved. The handle
//! stores the compact internal `K::Unit` in the overlay child map and converts at
//! the public boundary via [`KeyEncoding::token_to_unit`] /
//! [`KeyEncoding::unit_to_token`].
//!
//! # Thread safety (the −2 unsafe delta)
//!
//! The handle holds ONLY owned `Arc`s: an `Arc<OverlayNode<K, V>>` snapshot
//! (immutable + reference-counted, so descent needs no pin / no `unsafe` — the `Arc`
//! keeps the node + its in-memory subtree alive regardless of the trie's fate) and
//! an optional `Arc<dyn OverlayFaulter<K, V>>`. Because [`OverlayFaulter`] carries a
//! `Send + Sync` supertrait bound (see `faulter.rs`), `Arc<dyn OverlayFaulter<K, V>>`
//! is itself `Send + Sync`, so this struct **auto-derives** `Send`/`Sync`. The two
//! hand-written `unsafe impl Send/Sync for PersistentARTrieCharNode` the char variant
//! used to need (because its prior bespoke handle had no such supertrait route) are
//! therefore deleted — a clean `−2` against the strict unsafe-inventory set-equality
//! gate, with ZERO new `unsafe` introduced.
//!
//! Lives in `persistent_artrie_core` so the layering invariant holds: it imports the
//! shared [`OverlayNode`] / [`OverlayFaulter`] (canonical here) and the crate-root
//! [`DictionaryNode`] / [`MappedDictionaryNode`] traits with **zero** upward
//! reference to a variant module.

use std::sync::Arc;

use crate::persistent_artrie_core::key_encoding::KeyEncoding;
use crate::persistent_artrie_core::overlay::node::{Child, OverlayNode};
use crate::persistent_artrie_core::overlay::OverlayFaulter;
use crate::value::DictionaryValue;
use crate::{DictionaryNode, MappedDictionaryNode};

/// Shared overlay-backed [`DictionaryNode`] handle, generic over the key encoding
/// `K` (`ByteKey` / `CharKey`) and the value `V`.
///
/// `Clone` is derived (both fields are `Clone`); `Debug` is hand-written (below)
/// because `Arc<dyn OverlayFaulter<K, V>>` is not `Debug`. `Send`/`Sync`
/// auto-derive (see the module doc).
#[derive(Clone)]
pub struct OverlayDictionaryNode<K: KeyEncoding, V: DictionaryValue = ()> {
    /// Owned overlay node snapshot — the handle navigates the lock-free overlay
    /// (returned by `root()`). The `Arc` keeps the node + its in-memory subtree
    /// alive, so descent needs no pin / no `unsafe`. `Some` for every constructed
    /// handle; `None` is the inert default a method returns its empty value for.
    overlay: Option<Arc<OverlayNode<K, V>>>,
    /// SAFE fault-in capability for `Child::OnDisk` overlay children, or `None` for a
    /// resident-only walk (the inherent `&self` `root()`, where eviction — hence an
    /// OnDisk overlay child — is impossible). `Arc<dyn ..>` (owned), so it keeps the
    /// trie alive for the walk and clones cheaply. No raw pointer, no `unsafe`.
    overlay_faulter: Option<Arc<dyn OverlayFaulter<K, V>>>,
}

// Hand-written `Debug` (the derived one cannot see through `Arc<dyn OverlayFaulter>`):
// summarize whichever arm is active without recursing or dereferencing raw pointers.
impl<K: KeyEncoding, V: DictionaryValue> std::fmt::Debug for OverlayDictionaryNode<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OverlayDictionaryNode")
            .field("overlay", &self.overlay)
            .field("has_faulter", &self.overlay_faulter.is_some())
            .finish()
    }
}

impl<K: KeyEncoding, V: DictionaryValue> OverlayDictionaryNode<K, V> {
    /// Create an **overlay-backed** root node (the node returned by `root()`).
    /// Navigates the lock-free overlay lazily. `overlay_faulter` is the SAFE fault-in
    /// capability for `Child::OnDisk` overlay children (or `None` for a resident-only
    /// walk).
    pub(crate) fn from_overlay_root(
        node: Arc<OverlayNode<K, V>>,
        overlay_faulter: Option<Arc<dyn OverlayFaulter<K, V>>>,
    ) -> Self {
        Self {
            overlay: Some(node),
            overlay_faulter,
        }
    }

    /// Create an overlay child node, inheriting the parent's overlay faulter.
    pub(crate) fn from_overlay_node(
        node: Arc<OverlayNode<K, V>>,
        overlay_faulter: Option<Arc<dyn OverlayFaulter<K, V>>>,
    ) -> Self {
        Self {
            overlay: Some(node),
            overlay_faulter,
        }
    }

    /// Resolve an overlay child slot into a child overlay node, faulting a
    /// `Child::OnDisk` slot in via `overlay_faulter` (never dropping it). Returns
    /// `None` for a null/absent slot, or an OnDisk slot that cannot be faulted in
    /// (no faulter / I/O error) — the same conservative degrade the production
    /// point-read uses (liveness-only, never a fabricated term).
    pub(crate) fn overlay_child_node(
        child: &Child<K, V>,
        overlay_faulter: &Option<Arc<dyn OverlayFaulter<K, V>>>,
    ) -> Option<Self> {
        if let Some(child_arc) = child.as_in_mem() {
            return Some(Self::from_overlay_node(
                Arc::clone(child_arc),
                overlay_faulter.clone(),
            ));
        }
        if let Some(on_disk) = child.as_on_disk() {
            if !on_disk.is_null() {
                let loaded = overlay_faulter.as_ref()?.fault_overlay_slot(on_disk)?;
                return Some(Self::from_overlay_node(loaded, overlay_faulter.clone()));
            }
        }
        None
    }
}

impl<K: KeyEncoding, V: DictionaryValue> DictionaryNode for OverlayDictionaryNode<K, V> {
    /// The PUBLIC unit a caller (transducer / zipper) traverses by — `K::Token`
    /// (`u8` for byte, `char` for char). The internal overlay child map is keyed by
    /// the compact `K::Unit`; this handle converts at the boundary.
    type Unit = K::Token;

    fn is_final(&self) -> bool {
        // overlay-only: pure owned-`Arc` read, no pin / no `unsafe`.
        match &self.overlay {
            Some(node) => node.is_final(),
            None => false,
        }
    }

    fn transition(&self, label: K::Token) -> Option<Self> {
        // overlay-only: one `K::Unit` edge per overlay child (un-path-compressed).
        // Lower the public token to the internal storage unit, then look it up.
        // InMem ⇒ wrap directly; OnDisk ⇒ fault in via the SAFE overlay faulter
        // (never dropped). No pin / no `unsafe`.
        let node = self.overlay.as_ref()?;
        let child = node.find_child(K::token_to_unit(label))?;
        Self::overlay_child_node(child, &self.overlay_faulter)
    }

    fn edges(&self) -> Box<dyn Iterator<Item = (K::Token, Self)> + '_> {
        // overlay-only: one edge per overlay child slot (InMem direct, OnDisk faulted
        // in — never dropped). Each internal unit is raised back to a public token via
        // `K::unit_to_token`; a unit that is NOT a valid token (a `u32` surrogate —
        // impossible for real char data, total for byte) is SKIPPED, exactly as the
        // prior char `char::from_u32` filter / byte identity did. Preallocated to the
        // known child count. No `unsafe`.
        let Some(node) = &self.overlay else {
            return Box::new(std::iter::empty());
        };
        let mut edges = Vec::with_capacity(node.num_children());
        for (&unit, child) in node.iter_children() {
            let Some(token) = K::unit_to_token(unit) else {
                continue;
            };
            if let Some(child_node) = Self::overlay_child_node(child, &self.overlay_faulter) {
                edges.push((token, child_node));
            }
        }
        Box::new(edges.into_iter())
    }

    fn edge_count(&self) -> Option<usize> {
        // overlay-only: the overlay node's child count is exact and O(1).
        self.overlay.as_ref().map(|node| node.num_children())
    }
}

impl<K: KeyEncoding, V: DictionaryValue> MappedDictionaryNode for OverlayDictionaryNode<K, V> {
    type Value = V;

    /// The value stored at this node (if it terminates a key). Reads the overlay
    /// leaf's `Option<V>` directly (owned `Arc`, no pin / no `unsafe`). For `V = ()`
    /// membership finals this is `None`. This unlocks liblevenshtein's value-aware
    /// transducer queries over the persistent tries.
    fn value(&self) -> Option<V> {
        self.overlay.as_ref().and_then(|node| node.get_value())
    }
}

// G5.1 compile-time Send/Sync assertion (the crate has no `static_assertions` dep, so
// this is the trivial generic-fn form). It monomorphizes only when called; the
// `#[allow(dead_code)]` `_assert` invocation below forces that monomorphization at
// compile time WITHOUT running anything. This is the in-crate witness that the
// unified node AUTO-DERIVES `Send + Sync` (so the prior char `unsafe impl`s are
// genuinely unnecessary). The `DictionaryNode: Clone + Send + Sync` supertrait at the
// crate root ALSO transitively requires this, so a regression would already break the
// trait impl above — this assertion just localizes the failure.
#[allow(dead_code)]
fn _assert_overlay_dictionary_node_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    use crate::persistent_artrie_core::key_encoding::{ByteKey, CharKey};
    assert_send_sync::<OverlayDictionaryNode<ByteKey, ()>>();
    assert_send_sync::<OverlayDictionaryNode<ByteKey, u64>>();
    assert_send_sync::<OverlayDictionaryNode<CharKey, ()>>();
    assert_send_sync::<OverlayDictionaryNode<CharKey, u64>>();
}
