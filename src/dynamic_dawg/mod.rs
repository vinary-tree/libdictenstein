//! Dynamic DAWG dictionary family — incrementally updatable automata.
//!
//! - [`ascii`] — byte-level (`u8`) [`DynamicDawg`].
//! - [`mod@char`] — Unicode (`char`) [`DynamicDawgChar`].
//! - [`mod@u64`] — `u64`-labeled [`DynamicDawgU64`] (time-series / sequence keys).
//! - [`zipper`] / [`char_zipper`] / [`u64_zipper`] — zipper navigators.
//! - [`core`] — the unit-generic minimization core ([`DawgCore`], [`DawgNode`])
//!   shared by all three variants.

pub mod ascii;
pub mod char;
pub mod char_zipper;
pub mod core;
pub(crate) mod lockfree;
pub mod u64;
pub mod u64_zipper;
pub mod zipper;

pub use ascii::{DynamicDawg, DynamicDawgNode};
pub use char::{DynamicDawgChar, DynamicDawgCharNode};
pub use char_zipper::DynamicDawgCharZipper;
// `self::` disambiguates the child module `core` from the `core` crate.
pub use self::core::{DawgCore, DawgNode};
pub use u64::{DynamicDawgU64, DynamicDawgU64Node};
pub use u64_zipper::DynamicDawgU64Zipper;
pub use zipper::DynamicDawgZipper;

/// Opaque provenance-bearing cursor into one immutable DynamicDAWG revision.
///
/// This type is deliberately distinct from [`crate::DenseSnapshotCursor`]. It
/// cannot be converted to or from an integer and therefore cannot cross the
/// dense snapshot ABI accidentally. Its producing node retains the exact root
/// revision that owns the pointed-to allocation.
#[repr(transparent)]
pub struct DynamicDawgSnapshotCursor<U, V> {
    pointer: std::ptr::NonNull<()>,
    marker: std::marker::PhantomData<fn() -> (U, V)>,
}

impl<U, V> Copy for DynamicDawgSnapshotCursor<U, V> {}

impl<U, V> Clone for DynamicDawgSnapshotCursor<U, V> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<U, V> std::fmt::Debug for DynamicDawgSnapshotCursor<U, V> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("DynamicDawgSnapshotCursor(..)")
    }
}

// SAFETY: safe code cannot construct or dereference this cursor. Every
// in-crate constructor receives a pointer to a published immutable node whose
// unit and value types are both Send + Sync, and every dereference additionally
// requires the producing root revision to remain retained.
unsafe impl<U: Send + Sync, V: Send + Sync> Send for DynamicDawgSnapshotCursor<U, V> {}
// SAFETY: identical to the `Send` contract; immutable node data may be read
// concurrently while the producing revision remains retained.
unsafe impl<U: Send + Sync, V: Send + Sync> Sync for DynamicDawgSnapshotCursor<U, V> {}

impl<U, V> DynamicDawgSnapshotCursor<U, V> {
    /// Preserve the provenance of one node pointer behind an opaque type.
    #[inline]
    pub(crate) fn from_node<T>(pointer: std::ptr::NonNull<T>) -> Self {
        Self {
            pointer: pointer.cast(),
            marker: std::marker::PhantomData,
        }
    }

    /// Recover the original provenance for the producing backend.
    ///
    /// # Safety
    ///
    /// `T` must be the exact immutable node type used by `from_node`, and the
    /// retained root revision that produced this cursor must remain alive.
    #[inline]
    pub(crate) unsafe fn node_pointer<T>(self) -> std::ptr::NonNull<T> {
        self.pointer.cast()
    }
}
