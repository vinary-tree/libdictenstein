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

/// Public unit-generic dynamic DAWG surface.
///
/// The legacy string-oriented aliases remain unchanged; this type exposes the
/// shared lock-free core directly for callers that already own logical units.
#[derive(Clone, Debug)]
pub struct DynamicDawgGeneric<U: crate::CharUnit, V: crate::DictionaryValue = ()> {
    inner: std::sync::Arc<lockfree::LockFreeDawg<U, V>>,
}

impl<U: crate::CharUnit, V: crate::DictionaryValue> DynamicDawgGeneric<U, V> {
    /// Construct an empty generic DAWG.
    pub fn new() -> Self {
        Self {
            inner: std::sync::Arc::new(lockfree::LockFreeDawg::new()),
        }
    }

    /// Build from lexicographically sorted logical-unit sequences.
    pub fn from_sorted_sequences<I, S>(sequences: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<[U]>,
    {
        Self {
            inner: std::sync::Arc::new(lockfree::LockFreeDawg::from_sorted_terms_by(
                sequences,
                |sequence, units| units.extend_from_slice(sequence.as_ref()),
            )),
        }
    }

    /// Build from arbitrary logical-unit sequences, sorting once for
    /// deterministic and suffix-sharing-friendly construction.
    pub fn from_sequences<I, S>(sequences: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<[U]>,
    {
        let mut owned: Vec<Vec<U>> = sequences
            .into_iter()
            .map(|sequence| sequence.as_ref().to_vec())
            .collect();
        owned.sort_unstable();
        Self::from_sorted_sequences(owned)
    }

    /// Build a value-bearing DAWG from lexicographically sorted sequences.
    pub fn from_sorted_entries<I, S>(entries: I) -> Self
    where
        I: IntoIterator<Item = (S, V)>,
        S: AsRef<[U]>,
    {
        Self {
            inner: std::sync::Arc::new(lockfree::LockFreeDawg::from_sorted_entries_by(
                entries
                    .into_iter()
                    .map(|(sequence, value)| (sequence, Some(value))),
                |sequence, units| units.extend_from_slice(sequence.as_ref()),
            )),
        }
    }

    /// Insert one logical-unit sequence.
    #[inline]
    pub fn insert_units(&self, units: &[U]) -> bool {
        self.inner.insert_units(units)
    }

    /// Insert a logical sequence produced by a fixed-width atom profile.
    ///
    /// The profile is a compile-time witness that the sequence's atoms are
    /// the dictionary's traversal units; no encoded-byte decoding occurs in
    /// the DAWG hot path.
    #[inline]
    pub fn insert_atom_sequence<P>(&self, sequence: &crate::AtomSequence<P>) -> bool
    where
        P: crate::AtomProfile<Atom = U>,
    {
        self.insert_units(sequence.as_atoms())
    }

    /// Insert one sequence with an associated value.
    #[inline]
    pub fn insert_units_with_value(&self, units: &[U], value: V) -> bool {
        self.inner.insert_units_with_value(units, value)
    }

    /// Test membership using logical units.
    #[inline]
    pub fn contains_units(&self, units: &[U]) -> bool {
        self.inner.contains_units(units)
    }

    /// Query a profile sequence directly in logical-unit space.
    #[inline]
    pub fn contains_atom_sequence<P>(&self, sequence: &crate::AtomSequence<P>) -> bool
    where
        P: crate::AtomProfile<Atom = U>,
    {
        self.contains_units(sequence.as_atoms())
    }

    /// Read the value associated with a logical-unit sequence.
    #[inline]
    pub fn get_units_value(&self, units: &[U]) -> Option<V> {
        self.inner.get_units_value(units)
    }

    /// Remove a logical-unit sequence.
    #[inline]
    pub fn remove_units(&self, units: &[U]) -> bool {
        self.inner.remove_units(units)
    }

    /// Remove every logical-unit sequence from the current revision.
    #[inline]
    pub fn clear(&self) -> bool {
        self.inner.clear()
    }

    /// Number of visible terminal sequences.
    #[inline]
    pub fn term_count(&self) -> usize {
        self.inner.term_count()
    }

    /// Number of physical nodes in the current graph revision.
    #[inline]
    pub fn node_count(&self) -> usize {
        self.inner.node_count()
    }

    /// Whether the current revision has pending non-minimal structure.
    #[inline]
    pub fn needs_compaction(&self) -> bool {
        self.inner.needs_compaction()
    }

    /// Collect visible logical-unit entries in deterministic lexicographic
    /// order for snapshot/export boundaries.
    pub fn visible_entries(&self) -> Vec<(Vec<U>, Option<V>)> {
        self.inner.collect_visible_entries()
    }

    /// Compact/minimize the current immutable graph.
    #[inline]
    pub fn compact(&self) -> usize {
        self.inner.compact()
    }
}

impl<U: crate::CharUnit, V: crate::DictionaryValue> Default for DynamicDawgGeneric<U, V> {
    fn default() -> Self {
        Self::new()
    }
}

/// Alias emphasizing that this wrapper accepts profile-defined units.
pub type DynamicDawgProfile<U, V = ()> = DynamicDawgGeneric<U, V>;

/// Source-compatible alias for native 32-bit logical units.
pub type DynamicDawgU32<V = ()> = DynamicDawgGeneric<u32, V>;

#[cfg(test)]
mod generic_tests {
    use super::DynamicDawgGeneric;

    #[test]
    fn generic_surface_uses_logical_units_directly() {
        let dictionary = DynamicDawgGeneric::<u32, u32>::new();
        assert!(dictionary.insert_units(&[1, 2, 3]));
        assert!(dictionary.contains_units(&[1, 2, 3]));
        assert!(!dictionary.contains_units(&[1, 2]));
        assert!(dictionary.remove_units(&[1, 2, 3]));
        assert!(!dictionary.contains_units(&[1, 2, 3]));
        assert!(dictionary.insert_units_with_value(&[4], 99));
        assert_eq!(dictionary.get_units_value(&[4]), Some(99));
        assert!(dictionary.node_count() > 0);
        assert!(dictionary.clear());
        assert_eq!(dictionary.term_count(), 0);
    }

    #[test]
    fn generic_batch_constructor_uses_sorted_logical_sequences() {
        let dictionary =
            DynamicDawgGeneric::<u32>::from_sorted_sequences([vec![1u32, 2], vec![1, 3]]);
        assert!(dictionary.contains_units(&[1, 2]));
        assert!(dictionary.contains_units(&[1, 3]));
        assert_eq!(dictionary.term_count(), 2);
        let unsorted = DynamicDawgGeneric::<u32>::from_sequences([vec![1u32, 3], vec![1, 2]]);
        assert!(unsorted.contains_units(&[1, 2]));
        assert!(unsorted.contains_units(&[1, 3]));
        let valued = DynamicDawgGeneric::<u32, u32>::from_sorted_entries([
            (vec![1u32, 2], 10),
            (vec![1, 3], 20),
        ]);
        assert_eq!(valued.get_units_value(&[1, 2]), Some(10));
        assert_eq!(valued.get_units_value(&[1, 3]), Some(20));
        assert_eq!(
            valued.visible_entries(),
            vec![(vec![1, 2], Some(10)), (vec![1, 3], Some(20))]
        );
    }

    #[test]
    fn profile_sequences_are_consumed_without_encoded_byte_decoding() {
        let dictionary = DynamicDawgGeneric::<u32>::new();
        let sequence = crate::AtomSequence::<crate::U32>::from_atoms([7, 11, 13]);
        assert!(dictionary.insert_atom_sequence(&sequence));
        assert!(dictionary.contains_atom_sequence(&sequence));
        assert!(!dictionary.contains_units(&[7, 11]));
    }
}

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
