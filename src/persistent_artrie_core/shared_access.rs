//! **F4 — the lock-collapse compat shim.**
//!
//! Phase F collapses `SharedARTrie<V> = Arc<RwLock<PersistentARTrie<V>>>` and
//! `SharedCharARTrie<V,S> = Arc<RwLock<PersistentARTrieChar<V,S>>>` down to a bare
//! `Arc<…>` (the outer `RwLock` is deleted), so overlay reads AND writes are fully
//! lock-free — every live write target is the overlay (a lock-free CAS root), and
//! the only operations that still need *any* mutual exclusion (concurrent
//! checkpoints, the dormant owned path, eviction) take their own dedicated inner
//! locks (`checkpoint_lock` / the wrapped `owned_root` `RwLock` / the
//! `eviction_coordinator` `Mutex`), never the trie handle.
//!
//! ## Why a shim
//! There are ~270 in-repo `handle.read()` / `handle.write()` call sites (tests,
//! benches, examples, and the `ARTrie`/`Dictionary` trait bodies) plus the
//! cross-repo `liblevenshtein-rust` sibling (a path-dependency this change MUST
//! NOT edit). Rewriting every site is both a large blast radius and a back-compat
//! hazard. Instead, this module supplies a backward-compatible `.read()` /
//! `.write()` API on the collapsed `Arc<T>` handle via the [`SharedTrieAccess`]
//! extension trait: both return a transparent [`TrieAccessGuard`] that simply
//! `Deref`s to `&T`. **There is no lock** — both `read()` and `write()` hand back a
//! shared `&T`, and every method the guard forwards to is now `&self` (the
//! mutators route to lock-free CAS internally). So an existing
//! `let mut g = handle.write(); g.insert(term)` keeps compiling unchanged: `g`
//! derefs to `&T`, and `g.insert(term)` auto-refs the now-`&self` `insert`.
//!
//! ## Guard semantics
//! [`TrieAccessGuard`] is `Deref`-only (no `DerefMut`) because there is no `&mut T`
//! to hand out — the whole point of the collapse is that no caller holds the trie
//! exclusively. Any residual site that genuinely needed `&mut T` through the old
//! write guard (there was exactly one per variant: the `enable_eviction`
//! `guard.eviction_coordinator = Some(..)` field assignment) is rewritten to go
//! through the field's new interior-mutability wrapper.
//!
//! ## Lock hierarchy (deadlock-freedom — enforced project-wide)
//! The collapse introduces no new ordering hazard *because* the inner locks obey a
//! strict, acyclic order: **`CK > merge_lock > OR > EC`** (acquire only in that
//! order). `EC` (the eviction-coordinator `Mutex`) is a **leaf**: it is NEVER held
//! across acquiring `CK`/`merge_lock`/`OR`, and NEVER held across a worker
//! `.join()` (the drop-before-join discipline). Formally exercised by
//! `tests/persistent_lockfree_f4_lock_hierarchy_loom.rs`.

use std::ops::Deref;
use std::sync::atomic::{AtomicU8, Ordering};

/// A small `Copy` enum that round-trips through a single `u8` discriminant, so it
/// can live in an [`AtomicEnumCell`] for cheap lock-free `&self` reads/writes.
///
/// Implemented for `OverlayWriteMode` (the hot `route_overlay()` predicate) and
/// `DurabilityPolicy` (read on every durable write) — the two F4 Tier-2 fields
/// that are plain `Copy` enums rather than `Option<Arc<…>>` handles.
pub trait U8Enum: Copy {
    /// Stable `u8` discriminant for this value.
    fn as_u8(self) -> u8;
    /// Inverse of [`Self::as_u8`]. MUST be total over the discriminants
    /// [`Self::as_u8`] can produce (a corrupt byte is a logic error — the cell
    /// only ever stores values it wrote).
    fn from_u8(v: u8) -> Self;
}

/// An interior-mutable cell holding a [`U8Enum`] as an `AtomicU8`.
///
/// F4: replaces a plain `Copy`-enum field (`overlay_write_mode` /
/// `durability_policy`) so the now-`&self` lifecycle setters (`kill_switch_to_owned`,
/// `set_durability_policy`) and the hot-path readers (`route_overlay`,
/// `durability_policy`) work without the outer trie `RwLock`. A single relaxed
/// atomic load/store — strictly cheaper than the old `RwLock`-guarded field read.
#[derive(Debug)]
pub struct AtomicEnumCell<E: U8Enum> {
    bits: AtomicU8,
    _marker: std::marker::PhantomData<E>,
}

impl<E: U8Enum> AtomicEnumCell<E> {
    /// Wrap an initial value.
    #[inline]
    pub fn new(value: E) -> Self {
        Self {
            bits: AtomicU8::new(value.as_u8()),
            _marker: std::marker::PhantomData,
        }
    }

    /// Load the current value (`Acquire` — pairs with [`Self::store`]'s `Release`
    /// so a writer that publishes a mode/policy is observed by a subsequent
    /// reader; the field gates correctness-relevant routing).
    #[inline]
    pub fn load(&self) -> E {
        E::from_u8(self.bits.load(Ordering::Acquire))
    }

    /// Store a new value (`Release`). `&self` — the whole point of the F4 wrap.
    #[inline]
    pub fn store(&self, value: E) {
        self.bits.store(value.as_u8(), Ordering::Release);
    }
}

impl<E: U8Enum + Default> Default for AtomicEnumCell<E> {
    #[inline]
    fn default() -> Self {
        Self::new(E::default())
    }
}

/// A transparent, zero-cost access guard handed out by [`SharedTrieAccess::read`]
/// and [`SharedTrieAccess::write`].
///
/// Holds a shared borrow `&'a T` and `Deref`s to it. **There is no lock** behind
/// this guard — after the F4 collapse the handle is a bare `Arc<T>`, so both
/// "read" and "write" yield the same shared `&T`. The type exists solely to keep
/// the historical `handle.read()` / `handle.write()` call shape compiling against
/// the collapsed handle (the ~270-site / cross-repo blast-radius absorber).
///
/// It is deliberately `Deref`-only: there is no exclusive `&mut T` to expose
/// (every mutator is now `&self`, routing to lock-free CAS or a dedicated inner
/// lock).
#[derive(Debug)]
pub struct TrieAccessGuard<'a, T: ?Sized> {
    inner: &'a T,
}

impl<'a, T: ?Sized> TrieAccessGuard<'a, T> {
    /// Wrap a shared borrow. `pub(crate)` — only the per-variant
    /// [`SharedTrieAccess`] impls construct these.
    #[inline]
    pub(crate) fn new(inner: &'a T) -> Self {
        Self { inner }
    }
}

impl<T: ?Sized> Deref for TrieAccessGuard<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        self.inner
    }
}

/// Backward-compatible `.read()` / `.write()` accessors for the collapsed
/// `Arc<T>` trie handles (`SharedARTrie` / `SharedCharARTrie`).
///
/// Both methods return a [`TrieAccessGuard`] that derefs to `&T`. There is no
/// lock — see the module docs.
///
/// **Deliberately NOT blanket-implemented for `Arc<T>`.** A blanket
/// `impl<T> SharedTrieAccess for Arc<T>` would win method resolution over the
/// inherent `parking_lot::RwLock::{read,write}` reachable through an
/// `Arc<RwLock<…>>` (a no-deref trait method beats a one-deref inherent method),
/// hijacking EVERY `arc_of_rwlock.write()` in the crate (e.g. the
/// `Arc<RwLock<ArenaManager>>` / `Arc<RwLock<BufferManager>>` handles, and the
/// still-`RwLock`-wrapped `SharedVocabARTrie`). The impls therefore live in the
/// byte / char trie modules on the CONCRETE `Arc<PersistentARTrie<V,S>>` /
/// `Arc<PersistentARTrieChar<V,S>>` types (see
/// `impl_shared_trie_access!`), keeping core free of any upward dependency on the
/// variant crates while scoping the shim to exactly the two trie handles.
pub trait SharedTrieAccess {
    /// The wrapped trie type (`PersistentARTrie<V,S>` / `PersistentARTrieChar<V,S>`).
    type Target: ?Sized;

    /// Borrow the trie. Historically a read-lock acquisition; now a plain shared
    /// borrow (no lock — overlay reads are lock-free).
    fn read(&self) -> TrieAccessGuard<'_, Self::Target>;

    /// Borrow the trie for "writing". Historically a write-lock acquisition; now a
    /// plain shared borrow (no lock — the now-`&self` mutators route to lock-free
    /// CAS / a dedicated inner lock; nothing serializes on the handle).
    fn write(&self) -> TrieAccessGuard<'_, Self::Target>;
}

impl<'a, T: ?Sized> TrieAccessGuard<'a, T> {
    /// Construct a guard from a shared borrow (the variant modules' impls call
    /// this). Separate from [`Self::new`] only to be reachable from the byte/char
    /// `SharedTrieAccess` impls; both are crate-internal.
    #[inline]
    pub fn from_ref(inner: &'a T) -> Self {
        Self { inner }
    }
}
