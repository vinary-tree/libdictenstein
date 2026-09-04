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

    /// Build from owned logical sequences supplied by an [`AtomProfile`].
    ///
    /// The profile is consumed only at the API boundary; the DAWG stores and
    /// traverses the profile's logical units directly, so no wire decoding is
    /// introduced into the lookup hot path.
    pub fn from_atom_sequences<P, I>(sequences: I) -> Self
    where
        P: crate::AtomProfile<Atom = U>,
        I: IntoIterator<Item = crate::AtomSequence<P>>,
    {
        Self::from_sequences(
            sequences
                .into_iter()
                .map(|sequence| sequence.as_atoms().to_vec()),
        )
    }

    /// Build a value-bearing DAWG from profile sequences and their values.
    pub fn from_atom_sequences_with_values<P, I>(entries: I) -> Self
    where
        P: crate::AtomProfile<Atom = U>,
        I: IntoIterator<Item = (crate::AtomSequence<P>, V)>,
    {
        Self::from_sorted_entries(
            entries
                .into_iter()
                .map(|(sequence, value)| (sequence.as_atoms().to_vec(), value)),
        )
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

    /// Insert a profile sequence with an associated mapped value.
    #[inline]
    pub fn insert_atom_sequence_with_value<P>(
        &self,
        sequence: &crate::AtomSequence<P>,
        value: V,
    ) -> bool
    where
        P: crate::AtomProfile<Atom = U>,
    {
        self.insert_units_with_value(sequence.as_atoms(), value)
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

    /// Read a mapped value for a profile sequence directly in logical-unit
    /// space.
    #[inline]
    pub fn get_atom_sequence_value<P>(&self, sequence: &crate::AtomSequence<P>) -> Option<V>
    where
        P: crate::AtomProfile<Atom = U>,
    {
        self.get_units_value(sequence.as_atoms())
    }

    /// Remove a profile sequence directly in logical-unit space.
    #[inline]
    pub fn remove_atom_sequence<P>(&self, sequence: &crate::AtomSequence<P>) -> bool
    where
        P: crate::AtomProfile<Atom = U>,
    {
        self.remove_units(sequence.as_atoms())
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

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.term_count() == 0
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

/// Named aliases for the common profile unit specializations.
pub type DynamicDawgByteProfile<V = ()> = DynamicDawgGeneric<u8, V>;
pub type DynamicDawgCharProfile<V = ()> = DynamicDawgGeneric<char, V>;

/// Source-compatible alias for native 32-bit logical units.
pub type DynamicDawgU32<V = ()> = DynamicDawgGeneric<u32, V>;

/// Source-compatible alias for native 64-bit logical units.
pub type DynamicDawgU64Profile<V = ()> = DynamicDawgGeneric<u64, V>;

/// Variable-width ULEB128 DAWG boundary.
///
/// Canonical ULEB atoms are packed into the byte-oriented core for storage,
/// while this wrapper accepts and returns complete logical atom sequences. The
/// encoded continuation bytes are never exposed as dictionary transitions.
#[derive(Clone, Debug)]
pub struct DynamicDawgUleb128<V: crate::DictionaryValue = ()> {
    inner: DynamicDawgGeneric<u8, V>,
}

/// Variable-width UTF-8 dictionary boundary.
///
/// UTF-8 bytes are retained by the byte-oriented core, while this wrapper
/// validates and exposes complete Unicode strings so continuation bytes never
/// become logical transitions.
#[derive(Clone, Debug)]
pub struct DynamicDawgUtf8<V: crate::DictionaryValue = ()> {
    inner: DynamicDawgGeneric<u8, V>,
}

impl<V: crate::DictionaryValue> Default for DynamicDawgUtf8<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V: crate::DictionaryValue> DynamicDawgUtf8<V> {
    pub fn new() -> Self {
        Self {
            inner: DynamicDawgGeneric::new(),
        }
    }

    pub fn from_terms<I, S>(terms: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self {
            inner: DynamicDawgGeneric::from_sequences(
                terms.into_iter().map(|s| s.as_ref().as_bytes().to_vec()),
            ),
        }
    }

    pub fn from_terms_with_values<I, S>(entries: I) -> Self
    where
        I: IntoIterator<Item = (S, V)>,
        S: AsRef<str>,
    {
        let mut encoded = entries
            .into_iter()
            .map(|(s, v)| (s.as_ref().as_bytes().to_vec(), v))
            .collect::<Vec<_>>();
        encoded.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        Self {
            inner: DynamicDawgGeneric::from_sorted_entries(encoded),
        }
    }

    #[inline]
    pub fn insert(&self, term: &str) -> bool {
        self.inner.insert_units(term.as_bytes())
    }
    #[inline]
    pub fn insert_with_value(&self, term: &str, value: V) -> bool {
        self.inner.insert_units_with_value(term.as_bytes(), value)
    }
    #[inline]
    pub fn contains(&self, term: &str) -> bool {
        self.inner.contains_units(term.as_bytes())
    }
    #[inline]
    pub fn get_value(&self, term: &str) -> Option<V> {
        self.inner.get_units_value(term.as_bytes())
    }
    #[inline]
    pub fn remove(&self, term: &str) -> bool {
        self.inner.remove_units(term.as_bytes())
    }
    #[inline]
    pub fn term_count(&self) -> usize {
        self.inner.term_count()
    }
    #[inline]
    pub fn node_count(&self) -> usize {
        self.inner.node_count()
    }

    pub fn contains_encoded(&self, encoded: &[u8]) -> Result<bool, std::str::Utf8Error> {
        std::str::from_utf8(encoded)?;
        Ok(self.inner.contains_units(encoded))
    }

    pub fn visible_entries(&self) -> Result<Vec<(String, Option<V>)>, std::str::Utf8Error> {
        self.inner
            .visible_entries()
            .into_iter()
            .map(|(bytes, value)| std::str::from_utf8(&bytes).map(|term| (term.to_owned(), value)))
            .collect()
    }
}

impl<V: crate::DictionaryValue> Default for DynamicDawgUleb128<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V: crate::DictionaryValue> DynamicDawgUleb128<V> {
    /// Construct an empty ULEB128 dictionary.
    pub fn new() -> Self {
        Self {
            inner: DynamicDawgGeneric::new(),
        }
    }

    /// Build from complete canonical ULEB128 sequences.
    pub fn from_sequences<I>(sequences: I) -> Self
    where
        I: IntoIterator<Item = crate::Uleb128Sequence>,
    {
        let inner = DynamicDawgGeneric::from_sequences(
            sequences.into_iter().map(|sequence| sequence.to_encoded()),
        );
        Self { inner }
    }

    /// Build a value-bearing dictionary from complete ULEB128 sequences.
    pub fn from_sequences_with_values<I>(entries: I) -> Self
    where
        I: IntoIterator<Item = (crate::Uleb128Sequence, V)>,
    {
        let mut encoded: Vec<(Vec<u8>, V)> = entries
            .into_iter()
            .map(|(sequence, value)| (sequence.to_encoded(), value))
            .collect();
        encoded.sort_unstable_by(|left, right| left.0.cmp(&right.0));
        let inner = DynamicDawgGeneric::from_sorted_entries(encoded);
        Self { inner }
    }

    /// Insert one complete ULEB128 sequence.
    #[inline]
    pub fn insert(&self, sequence: &crate::Uleb128Sequence) -> bool {
        self.inner.insert_units(&sequence.to_encoded())
    }

    /// Insert one complete ULEB128 sequence with a mapped value.
    #[inline]
    pub fn insert_with_value(&self, sequence: &crate::Uleb128Sequence, value: V) -> bool {
        self.inner
            .insert_units_with_value(&sequence.to_encoded(), value)
    }

    /// Test membership of one complete ULEB128 sequence.
    #[inline]
    pub fn contains(&self, sequence: &crate::Uleb128Sequence) -> bool {
        self.inner.contains_units(&sequence.to_encoded())
    }

    /// Test a complete canonical encoded sequence without first allocating an
    /// owned [`Uleb128Sequence`].  Validation is kept at this boundary so
    /// continuation bytes can never become visible DAWG transitions.
    pub fn contains_encoded(&self, encoded: &[u8]) -> Result<bool, crate::Uleb128Error> {
        crate::Uleb128Sequence::from_encoded(encoded)?;
        Ok(self.inner.contains_units(encoded))
    }

    /// Read a mapped value for one complete ULEB128 sequence.
    #[inline]
    pub fn get_value(&self, sequence: &crate::Uleb128Sequence) -> Option<V> {
        self.inner.get_units_value(&sequence.to_encoded())
    }

    /// Read a value for a complete canonical encoded sequence without
    /// materializing its decoded atoms.
    pub fn get_encoded_value(&self, encoded: &[u8]) -> Result<Option<V>, crate::Uleb128Error> {
        crate::Uleb128Sequence::from_encoded(encoded)?;
        Ok(self.inner.get_units_value(encoded))
    }

    /// Remove one complete ULEB128 sequence.
    #[inline]
    pub fn remove(&self, sequence: &crate::Uleb128Sequence) -> bool {
        self.inner.remove_units(&sequence.to_encoded())
    }

    /// Number of visible logical sequences.
    #[inline]
    pub fn term_count(&self) -> usize {
        self.inner.term_count()
    }

    /// Number of physical byte nodes.
    #[inline]
    pub fn node_count(&self) -> usize {
        self.inner.node_count()
    }

    /// Export logical sequences, rejecting any malformed internal image.
    pub fn visible_entries(
        &self,
    ) -> Result<Vec<(crate::Uleb128Sequence, Option<V>)>, crate::Uleb128Error> {
        self.inner
            .visible_entries()
            .into_iter()
            .map(|(bytes, value)| {
                crate::Uleb128Sequence::from_encoded(&bytes).map(|sequence| (sequence, value))
            })
            .collect()
    }
}

#[cfg(test)]
mod generic_tests {
    use super::DynamicDawgGeneric;

    #[test]
    fn uleb_wrapper_preserves_atom_boundaries() {
        let first = crate::Uleb128::from_u64(624_485);
        let second = crate::Uleb128::from_payload_digits(&[3, 4]).unwrap();
        let sequence = crate::Uleb128Sequence::from_atoms([first, second]);
        let dictionary = super::DynamicDawgUleb128::<u16>::new();
        assert!(dictionary.insert_with_value(&sequence, 9));
        assert!(dictionary.contains(&sequence));
        assert_eq!(dictionary.get_value(&sequence), Some(9));
        let entries = dictionary.visible_entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, sequence);
        assert!(dictionary.remove(&sequence));
        assert!(!dictionary.contains(&sequence));
    }

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
    fn encoded_lookup_rejects_malformed_and_preserves_zero_copy_boundary() {
        let atom = crate::Uleb128::from_u64(624_485);
        let sequence = crate::Uleb128Sequence::from_atoms([atom]);
        let dictionary = super::DynamicDawgUleb128::<u16>::from_sequences([sequence.clone()]);
        assert_eq!(
            dictionary.contains_encoded(sequence.to_encoded().as_slice()),
            Ok(true)
        );
        assert!(dictionary.contains_encoded(&[0x80]).is_err());
    }

    #[test]
    fn utf8_wrapper_preserves_scalar_boundaries() {
        let dictionary =
            super::DynamicDawgUtf8::<u16>::from_terms_with_values([("λ🎉", 4), ("a", 1)]);
        assert!(dictionary.contains("λ🎉"));
        assert_eq!(dictionary.get_value("λ🎉"), Some(4));
        assert_eq!(dictionary.visible_entries().unwrap().len(), 2);
        assert!(dictionary.contains_encoded("λ🎉".as_bytes()).unwrap());
        assert!(dictionary.contains_encoded(&[0x80]).is_err());
        assert!(!dictionary.is_empty());
    }

    #[test]
    fn generic_surface_builds_from_profile_sequences() {
        let dictionary = DynamicDawgGeneric::<u32>::from_atom_sequences::<crate::U32, _>([
            crate::AtomSequence::<crate::U32>::from_atoms([7, 11]),
            crate::AtomSequence::<crate::U32>::from_atoms([7, 13]),
        ]);
        assert!(dictionary.contains_units(&[7, 11]));
        assert!(dictionary.contains_units(&[7, 13]));
    }

    #[test]
    fn generic_surface_builds_profile_sequences_with_values() {
        let dictionary = DynamicDawgGeneric::<u32, u16>::from_atom_sequences_with_values::<
            crate::U32,
            _,
        >([(crate::AtomSequence::from_atoms([3, 5]), 42)]);
        assert_eq!(dictionary.get_units_value(&[3, 5]), Some(42));
        let sequence = crate::AtomSequence::<crate::U32>::from_atoms([3, 5]);
        assert_eq!(dictionary.get_atom_sequence_value(&sequence), Some(42));
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
        let dictionary = DynamicDawgGeneric::<u32, u32>::new();
        let sequence = crate::AtomSequence::<crate::U32>::from_atoms([7, 11, 13]);
        assert!(dictionary.insert_atom_sequence_with_value(&sequence, 41));
        assert!(dictionary.contains_atom_sequence(&sequence));
        assert_eq!(dictionary.get_units_value(sequence.as_atoms()), Some(41));
        assert!(dictionary.remove_atom_sequence(&sequence));
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
