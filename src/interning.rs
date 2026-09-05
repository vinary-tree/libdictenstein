//! Deterministic capsule-local vocabulary interning.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::dynamic_dawg::{DynamicDawgGeneric, DynamicDawgU32};
use crate::Uleb128;
use crate::{CharUnit, DictionaryValue};

/// Dense identifier assigned by an [`InternedVocabulary`].
pub type InternedId = u64;

/// Lossless snapshot rows exported by a coordinated vocabulary/ID dictionary.
pub type InternedEntries<K, V> = Vec<(Vec<K>, Option<V>)>;

/// Validation failures at the vocabulary boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterningError {
    /// The ID is not present in this vocabulary generation.
    UnknownId(InternedId),
    /// The sequence belongs to a different vocabulary generation.
    GenerationMismatch { expected: u64, actual: u64 },
    /// A caller attempted to use an atom that has not been interned.
    UnknownKey,
    /// The coordinated vocabulary lock was poisoned by a prior panic.
    Poisoned,
    /// No representable local ID remains.
    IdExhausted,
}

/// Compact capsule-local sequence of vocabulary IDs.
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct InternedSequence {
    ids: Vec<InternedId>,
    generation: u64,
}

impl InternedSequence {
    /// Construct an empty ID sequence.
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct from already assigned IDs.
    pub fn from_ids<I>(ids: I) -> Self
    where
        I: IntoIterator<Item = InternedId>,
    {
        Self {
            ids: ids.into_iter().collect(),
            generation: 0,
        }
    }

    /// Construct IDs bound to an explicit vocabulary generation.
    pub fn from_ids_with_generation<I>(generation: u64, ids: I) -> Self
    where
        I: IntoIterator<Item = InternedId>,
    {
        Self {
            ids: ids.into_iter().collect(),
            generation,
        }
    }

    /// Borrow the compact ID representation.
    #[inline]
    pub fn as_ids(&self) -> &[InternedId] {
        &self.ids
    }

    /// Vocabulary generation that owns these IDs.
    #[inline]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Whether this sequence belongs to the supplied vocabulary generation.
    #[inline]
    pub const fn is_bound_to(&self, generation: u64) -> bool {
        self.generation == generation
    }

    /// Iterate IDs without allocation.
    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = InternedId> + '_ {
        self.ids.iter().copied()
    }

    /// Number of logical symbols.
    #[inline]
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// Whether the sequence is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
}

/// Bidirectional vocabulary with monotonic, never-reused local IDs.
///
/// IDs are intentionally scoped to this vocabulary instance.  Callers that
/// persist or exchange them must bind the vocabulary's own profile and
/// snapshot identity; an ID alone is never a semantic identity.
#[derive(Clone, Debug)]
pub struct InternedVocabulary<K: Ord + Clone> {
    forward: BTreeMap<K, InternedId>,
    reverse: Vec<K>,
    generation: u64,
}

/// Immutable vocabulary view captured at one generation boundary.
///
/// The snapshot owns the ID-to-symbol table, so readers can resolve IDs
/// without retaining the vocabulary mutex or observing later insertions. IDs
/// remain meaningful only with this snapshot's generation.
#[derive(Clone, Debug)]
pub struct InternedVocabularySnapshot<K> {
    generation: u64,
    reverse: Arc<[K]>,
}

impl<K> InternedVocabularySnapshot<K> {
    /// Generation identity bound to every ID in this snapshot.
    #[inline]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Number of symbols visible in this snapshot.
    #[inline]
    pub fn len(&self) -> usize {
        self.reverse.len()
    }

    /// Whether this snapshot contains no symbols.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.reverse.is_empty()
    }

    /// Resolve an ID without allocation or locking.
    #[inline]
    pub fn value(&self, id: InternedId) -> Option<&K> {
        self.reverse.get(usize::try_from(id).ok()?)
    }

    /// Validate a generation-bound sequence against this immutable snapshot.
    pub fn validate_sequence(&self, sequence: &InternedSequence) -> Result<(), InterningError> {
        if sequence.generation != self.generation {
            return Err(InterningError::GenerationMismatch {
                expected: self.generation,
                actual: sequence.generation,
            });
        }
        sequence
            .ids
            .iter()
            .copied()
            .find(|&id| self.value(id).is_none())
            .map_or(Ok(()), |id| Err(InterningError::UnknownId(id)))
    }

    /// Resolve IDs without allocating; an unknown ID is represented as
    /// `None` and can be handled by the caller's fail-closed policy.
    pub fn resolve_iter<'a>(
        &'a self,
        sequence: &'a InternedSequence,
    ) -> impl Iterator<Item = Option<&'a K>> {
        sequence.ids.iter().map(|&id| self.value(id))
    }

    /// Iterate stable ID/value pairs in ID order.
    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = (InternedId, &K)> {
        self.reverse
            .iter()
            .enumerate()
            .map(|(id, value)| (id as InternedId, value))
    }
}

impl<K: Ord + Clone> Default for InternedVocabulary<K> {
    fn default() -> Self {
        Self {
            forward: BTreeMap::new(),
            reverse: Vec::new(),
            generation: 0,
        }
    }
}

impl<K: Ord + Clone> InternedVocabulary<K> {
    /// Construct an empty vocabulary.
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct an empty vocabulary with an explicit generation identity.
    pub const fn with_generation(generation: u64) -> Self {
        Self {
            forward: BTreeMap::new(),
            reverse: Vec::new(),
            generation,
        }
    }

    /// Generation identity to bind alongside every persisted ID sequence.
    #[inline]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Return the existing ID or assign the next monotonic ID.
    pub fn intern(&mut self, key: K) -> InternedId {
        self.try_intern(key)
            .expect("InternedVocabulary ID space exhausted")
    }

    /// Return the existing ID or assign the next ID, reporting exhaustion.
    pub fn try_intern(&mut self, key: K) -> Result<InternedId, InterningError> {
        if let Some(&id) = self.forward.get(&key) {
            return Ok(id);
        }
        let id =
            InternedId::try_from(self.reverse.len()).map_err(|_| InterningError::IdExhausted)?;
        self.forward.insert(key.clone(), id);
        self.reverse.push(key);
        Ok(id)
    }

    /// Intern a logical sequence and return its compact ID representation.
    pub fn intern_sequence<I>(&mut self, keys: I) -> InternedSequence
    where
        I: IntoIterator<Item = K>,
    {
        InternedSequence::from_ids_with_generation(
            self.generation,
            keys.into_iter().map(|key| self.intern(key)),
        )
    }

    /// Fallible sequence interning with typed ID exhaustion.
    pub fn try_intern_sequence<I>(&mut self, keys: I) -> Result<InternedSequence, InterningError>
    where
        I: IntoIterator<Item = K>,
    {
        let ids = keys
            .into_iter()
            .map(|key| self.try_intern(key))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(InternedSequence::from_ids_with_generation(
            self.generation,
            ids,
        ))
    }

    /// Resolve every ID in a sequence, failing if one ID is not in this
    /// vocabulary generation.
    pub fn resolve_sequence<'a>(
        &'a self,
        sequence: &'a InternedSequence,
    ) -> Option<impl Iterator<Item = &'a K>> {
        let values: Option<Vec<&'a K>> = sequence.ids.iter().map(|&id| self.value(id)).collect();
        values.map(Vec::into_iter)
    }

    /// Validate that every ID belongs to this vocabulary generation.
    pub fn validate_sequence(&self, sequence: &InternedSequence) -> Result<(), InterningError> {
        if sequence.generation != self.generation {
            return Err(InterningError::GenerationMismatch {
                expected: self.generation,
                actual: sequence.generation,
            });
        }
        sequence
            .ids
            .iter()
            .copied()
            .find(|&id| self.value(id).is_none())
            .map_or(Ok(()), |id| Err(InterningError::UnknownId(id)))
    }

    /// Borrow each resolved value in ID order without allocating.  A `None`
    /// item denotes an unknown ID and must be treated as a vocabulary-boundary
    /// error by consumers.
    pub fn resolve_iter<'a>(
        &'a self,
        sequence: &'a InternedSequence,
    ) -> impl Iterator<Item = Option<&'a K>> {
        sequence.ids.iter().map(|&id| self.value(id))
    }

    /// Look up an ID without mutating the vocabulary.
    #[inline]
    pub fn id_of(&self, key: &K) -> Option<InternedId> {
        self.forward.get(key).copied()
    }

    /// Resolve an ID without mutating the vocabulary.
    #[inline]
    pub fn value(&self, id: InternedId) -> Option<&K> {
        let index = usize::try_from(id).ok()?;
        self.reverse.get(index)
    }

    /// Number of interned values.
    #[inline]
    pub fn len(&self) -> usize {
        self.reverse.len()
    }

    /// Whether no values have been interned.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.reverse.is_empty()
    }

    /// Iterate IDs and values in deterministic ID order.
    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = (InternedId, &K)> {
        self.reverse
            .iter()
            .enumerate()
            .map(|(id, key)| (id as InternedId, key))
    }

    /// Capture an immutable ID-to-symbol snapshot for lock-free readers.
    pub fn snapshot(&self) -> InternedVocabularySnapshot<K> {
        InternedVocabularySnapshot {
            generation: self.generation,
            reverse: self.reverse.clone().into(),
        }
    }
}

/// A vocabulary and its ID-sequence dictionary as one ownership boundary.
///
/// The vocabulary is the only component that can create IDs.  The underlying
/// DAWG is private so a caller cannot insert an arbitrary local-ID sequence
/// without going through vocabulary validation.  Read-only ID access remains
/// available through [`Self::id_dictionary`] for engines whose hot loops are
/// already bound to this capsule's generation.
#[derive(Clone, Debug)]
pub struct InternedSequenceDictionary<K: Ord + Clone, V: DictionaryValue = ()> {
    vocabulary: Arc<Mutex<InternedVocabulary<K>>>,
    id_dictionary: DynamicDawgU32<V>,
}

/// Canonical arbitrary-width ULEB atoms interned to the default `u32` carrier.
/// The atom bytes remain the vocabulary's external identity; the DAWG sees
/// only generation-bound fixed-width IDs.
pub type InternedUlebSequenceDictionary<V = ()> = InternedSequenceDictionary<Uleb128, V>;

/// Raw IEEE-754 binary64 bit patterns interned into the default `u32` ID
/// carrier. Equality and identity remain bit-preserving, including signed
/// zero and distinct NaN payloads.
pub type InternedF64BitsSequenceDictionary<V = ()> = InternedSequenceDictionary<u64, V>;

/// Arbitrary-width ULEB atoms interned to the explicit `u64` local carrier.
/// This preserves the same capsule-local vocabulary and generation rules while
/// allowing more than `u32::MAX` distinct symbols in one vocabulary.
pub type InternedUlebSequenceDictionaryU64<V = ()> = InternedSequenceDictionaryU64<Uleb128, V>;

/// Raw IEEE-754 binary64 bit patterns interned into the explicit `u64` ID
/// carrier.
pub type InternedF64BitsSequenceDictionaryU64<V = ()> = InternedSequenceDictionaryU64<u64, V>;

/// Capability-limited read view of an interned ID-sequence backend.
#[derive(Clone, Copy, Debug)]
pub struct InternedIdDictionaryView<'a, U: CharUnit, V: DictionaryValue> {
    dictionary: &'a DynamicDawgGeneric<U, V>,
}

impl<'a, U: CharUnit, V: DictionaryValue> InternedIdDictionaryView<'a, U, V> {
    #[inline]
    fn new(dictionary: &'a DynamicDawgGeneric<U, V>) -> Self {
        Self { dictionary }
    }

    /// Test membership in the already-bound ID domain.
    #[inline]
    pub fn contains_units(&self, ids: &[U]) -> bool {
        self.dictionary.contains_units(ids)
    }

    /// Read a mapped value in the already-bound ID domain.
    #[inline]
    pub fn get_units_value(&self, ids: &[U]) -> Option<V> {
        self.dictionary.get_units_value(ids)
    }

    /// Number of visible ID sequences.
    #[inline]
    pub fn term_count(&self) -> usize {
        self.dictionary.term_count()
    }

    /// Number of physical nodes in the ID backend.
    #[inline]
    pub fn node_count(&self) -> usize {
        self.dictionary.node_count()
    }

    /// Export visible ID sequences in deterministic lexicographic order.
    ///
    /// This is an explicit snapshot boundary; hot-loop consumers should use
    /// `contains_units` and `get_units_value` instead of repeatedly exporting.
    pub fn visible_entries(&self) -> Vec<(Vec<U>, Option<V>)> {
        self.dictionary.visible_entries()
    }
}

/// Explicit `u64` carrier specialization for vocabularies larger than the
/// default `u32` ID domain.  The vocabulary and generation semantics are
/// identical to [`InternedSequenceDictionary`].
#[derive(Clone, Debug)]
pub struct InternedSequenceDictionaryU64<K: Ord + Clone, V: DictionaryValue = ()> {
    vocabulary: Arc<Mutex<InternedVocabulary<K>>>,
    id_dictionary: DynamicDawgGeneric<u64, V>,
}

impl<K: Ord + Clone, V: DictionaryValue> Default for InternedSequenceDictionaryU64<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Ord + Clone, V: DictionaryValue> InternedSequenceDictionaryU64<K, V> {
    /// Construct an empty coordinated dictionary with generation zero.
    pub fn new() -> Self {
        Self::with_generation(0)
    }

    /// Construct an empty coordinated dictionary with an explicit generation.
    pub fn with_generation(generation: u64) -> Self {
        Self {
            vocabulary: Arc::new(Mutex::new(InternedVocabulary::with_generation(generation))),
            id_dictionary: DynamicDawgGeneric::new(),
        }
    }

    /// Borrow the vocabulary lock for identity and reverse lookup.
    pub fn vocabulary(
        &self,
    ) -> std::sync::LockResult<std::sync::MutexGuard<'_, InternedVocabulary<K>>> {
        self.vocabulary.lock()
    }

    /// Capture the vocabulary boundary without retaining its mutex guard.
    pub fn vocabulary_snapshot(&self) -> Result<InternedVocabularySnapshot<K>, InterningError> {
        self.vocabulary
            .lock()
            .map(|vocabulary| vocabulary.snapshot())
            .map_err(|_| InterningError::Poisoned)
    }

    /// Read the generation identity without exposing vocabulary storage.
    pub fn generation(&self) -> Result<u64, InterningError> {
        self.vocabulary
            .lock()
            .map(|vocabulary| vocabulary.generation())
            .map_err(|_| InterningError::Poisoned)
    }

    /// Access the `u64` ID-native dictionary for hot-loop consumers.
    #[inline]
    pub fn id_dictionary(&self) -> InternedIdDictionaryView<'_, u64, V> {
        InternedIdDictionaryView::new(&self.id_dictionary)
    }

    /// Query a generation-bound ID sequence after validating its vocabulary.
    pub fn contains_id_sequence(
        &self,
        sequence: &InternedSequence,
    ) -> Result<bool, InterningError> {
        let vocabulary = self
            .vocabulary
            .lock()
            .map_err(|_| InterningError::Poisoned)?;
        vocabulary.validate_sequence(sequence)?;
        Ok(self.id_dictionary.contains_units(sequence.as_ids()))
    }

    /// Read a mapped value for a generation-bound ID sequence.
    pub fn get_id_sequence_value(
        &self,
        sequence: &InternedSequence,
    ) -> Result<Option<V>, InterningError> {
        let vocabulary = self
            .vocabulary
            .lock()
            .map_err(|_| InterningError::Poisoned)?;
        vocabulary.validate_sequence(sequence)?;
        Ok(self.id_dictionary.get_units_value(sequence.as_ids()))
    }

    /// Export atom sequences and mapped values in deterministic ID-dictionary
    /// order.  Every ID is resolved while the vocabulary snapshot is held;
    /// unknown IDs are reported instead of being silently omitted.
    pub fn visible_entries(&self) -> Result<InternedEntries<K, V>, InterningError> {
        let vocabulary = self
            .vocabulary
            .lock()
            .map_err(|_| InterningError::Poisoned)?;
        self.id_dictionary
            .visible_entries()
            .into_iter()
            .map(|(ids, value)| {
                ids.into_iter()
                    .map(|id| {
                        vocabulary
                            .value(id)
                            .cloned()
                            .ok_or(InterningError::UnknownId(id))
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .map(|atoms| (atoms, value))
            })
            .collect()
    }

    /// Intern atoms and insert their sequence using the `u64` carrier.
    pub fn insert<I>(&self, atoms: I, value: Option<V>) -> Result<bool, InterningError>
    where
        I: IntoIterator<Item = K>,
    {
        let mut vocabulary = self
            .vocabulary
            .lock()
            .map_err(|_| InterningError::Poisoned)?;
        let sequence = vocabulary.try_intern_sequence(atoms)?;
        let ids = sequence.as_ids().to_vec();
        Ok(match value {
            Some(value) => self.id_dictionary.insert_units_with_value(&ids, value),
            None => self.id_dictionary.insert_units(&ids),
        })
    }

    /// Intern and insert one shared logical profile sequence while retaining
    /// the vocabulary's generation and ID validation boundary.
    pub fn insert_atom_sequence<P>(
        &self,
        sequence: &crate::AtomSequence<P>,
        value: Option<V>,
    ) -> Result<bool, InterningError>
    where
        P: crate::AtomProfile<Atom = K>,
    {
        self.insert(sequence.as_atoms().iter().cloned(), value)
    }

    /// Test an atom sequence without mutating the vocabulary.
    pub fn contains<I>(&self, atoms: I) -> Result<bool, InterningError>
    where
        I: IntoIterator<Item = K>,
    {
        let vocabulary = self
            .vocabulary
            .lock()
            .map_err(|_| InterningError::Poisoned)?;
        let ids = atoms
            .into_iter()
            .map(|atom| vocabulary.id_of(&atom).ok_or(InterningError::UnknownKey))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(self.id_dictionary.contains_units(&ids))
    }

    /// Test one shared logical profile sequence without mutating the
    /// vocabulary.
    pub fn contains_atom_sequence<P>(
        &self,
        sequence: &crate::AtomSequence<P>,
    ) -> Result<bool, InterningError>
    where
        P: crate::AtomProfile<Atom = K>,
    {
        self.contains(sequence.as_atoms().iter().cloned())
    }

    /// Read a mapped value for an already-interned atom sequence.
    pub fn get_value<I>(&self, atoms: I) -> Result<Option<V>, InterningError>
    where
        I: IntoIterator<Item = K>,
    {
        let vocabulary = self
            .vocabulary
            .lock()
            .map_err(|_| InterningError::Poisoned)?;
        let ids = atoms
            .into_iter()
            .map(|atom| vocabulary.id_of(&atom).ok_or(InterningError::UnknownKey))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(self.id_dictionary.get_units_value(&ids))
    }

    /// Read a mapped value for one already-interned shared logical profile
    /// sequence.
    pub fn get_atom_sequence_value<P>(
        &self,
        sequence: &crate::AtomSequence<P>,
    ) -> Result<Option<V>, InterningError>
    where
        P: crate::AtomProfile<Atom = K>,
    {
        self.get_value(sequence.as_atoms().iter().cloned())
    }

    /// Remove an atom sequence without changing vocabulary assignments.
    pub fn remove<I>(&self, atoms: I) -> Result<bool, InterningError>
    where
        I: IntoIterator<Item = K>,
    {
        let vocabulary = self
            .vocabulary
            .lock()
            .map_err(|_| InterningError::Poisoned)?;
        let ids = atoms
            .into_iter()
            .map(|atom| vocabulary.id_of(&atom).ok_or(InterningError::UnknownKey))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(self.id_dictionary.remove_units(&ids))
    }
}

impl<K: Ord + Clone, V: DictionaryValue> Default for InternedSequenceDictionary<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Ord + Clone, V: DictionaryValue> InternedSequenceDictionary<K, V> {
    /// Construct an empty coordinated dictionary with generation zero.
    pub fn new() -> Self {
        Self::with_generation(0)
    }

    /// Construct an empty coordinated dictionary with an explicit generation.
    pub fn with_generation(generation: u64) -> Self {
        Self {
            vocabulary: Arc::new(Mutex::new(InternedVocabulary::with_generation(generation))),
            id_dictionary: DynamicDawgU32::new(),
        }
    }

    /// Borrow the vocabulary lock for read-only identity and reverse lookup.
    ///
    /// The guard is intentionally returned instead of cloning the vocabulary,
    /// preserving zero-copy access and making the lifetime of the observation
    /// explicit to callers.
    pub fn vocabulary(
        &self,
    ) -> std::sync::LockResult<std::sync::MutexGuard<'_, InternedVocabulary<K>>> {
        self.vocabulary.lock()
    }

    /// Capture the vocabulary boundary without retaining its mutex guard.
    pub fn vocabulary_snapshot(&self) -> Result<InternedVocabularySnapshot<K>, InterningError> {
        self.vocabulary
            .lock()
            .map(|vocabulary| vocabulary.snapshot())
            .map_err(|_| InterningError::Poisoned)
    }

    /// Read the generation identity without exposing vocabulary storage.
    pub fn generation(&self) -> Result<u64, InterningError> {
        self.vocabulary
            .lock()
            .map(|vocabulary| vocabulary.generation())
            .map_err(|_| InterningError::Poisoned)
    }

    /// Access the ID-native dictionary for hot-loop consumers.
    ///
    /// Its sequences are meaningful only with this instance's vocabulary and
    /// generation.  The vocabulary remains the authority for constructing
    /// valid sequences.
    #[inline]
    pub fn id_dictionary(&self) -> InternedIdDictionaryView<'_, u32, V> {
        InternedIdDictionaryView::new(&self.id_dictionary)
    }

    /// Query a generation-bound ID sequence after validating its vocabulary.
    pub fn contains_id_sequence(
        &self,
        sequence: &InternedSequence,
    ) -> Result<bool, InterningError> {
        let vocabulary = self
            .vocabulary
            .lock()
            .map_err(|_| InterningError::Poisoned)?;
        vocabulary.validate_sequence(sequence)?;
        let ids = sequence
            .as_ids()
            .iter()
            .copied()
            .map(|id| u32::try_from(id).map_err(|_| InterningError::UnknownId(id)))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(self.id_dictionary.contains_units(&ids))
    }

    /// Read a mapped value for a generation-bound ID sequence.
    pub fn get_id_sequence_value(
        &self,
        sequence: &InternedSequence,
    ) -> Result<Option<V>, InterningError> {
        let vocabulary = self
            .vocabulary
            .lock()
            .map_err(|_| InterningError::Poisoned)?;
        vocabulary.validate_sequence(sequence)?;
        let ids = sequence
            .as_ids()
            .iter()
            .copied()
            .map(|id| u32::try_from(id).map_err(|_| InterningError::UnknownId(id)))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(self.id_dictionary.get_units_value(&ids))
    }

    /// Export atom sequences and mapped values in deterministic ID-dictionary
    /// order, validating every vocabulary ID before exposing the snapshot.
    pub fn visible_entries(&self) -> Result<InternedEntries<K, V>, InterningError> {
        let vocabulary = self
            .vocabulary
            .lock()
            .map_err(|_| InterningError::Poisoned)?;
        self.id_dictionary
            .visible_entries()
            .into_iter()
            .map(|(ids, value)| {
                ids.into_iter()
                    .map(|id| {
                        vocabulary
                            .value(u64::from(id))
                            .cloned()
                            .ok_or(InterningError::UnknownId(u64::from(id)))
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .map(|atoms| (atoms, value))
            })
            .collect()
    }

    /// Intern atoms and insert their ID sequence atomically with respect to
    /// other vocabulary mutations.
    pub fn insert<I>(&self, atoms: I, value: Option<V>) -> Result<bool, InterningError>
    where
        I: IntoIterator<Item = K>,
    {
        let mut vocabulary = self
            .vocabulary
            .lock()
            .map_err(|_| InterningError::Poisoned)?;
        let sequence = vocabulary.try_intern_sequence(atoms)?;
        let ids: Vec<u32> = sequence
            .as_ids()
            .iter()
            .copied()
            .map(|id| u32::try_from(id).map_err(|_| InterningError::UnknownId(id)))
            .collect::<Result<_, _>>()?;
        let inserted = match value {
            Some(value) => self.id_dictionary.insert_units_with_value(&ids, value),
            None => self.id_dictionary.insert_units(&ids),
        };
        Ok(inserted)
    }

    /// Intern and insert one shared logical profile sequence while retaining
    /// the vocabulary's generation and ID validation boundary.
    pub fn insert_atom_sequence<P>(
        &self,
        sequence: &crate::AtomSequence<P>,
        value: Option<V>,
    ) -> Result<bool, InterningError>
    where
        P: crate::AtomProfile<Atom = K>,
    {
        self.insert(sequence.as_atoms().iter().cloned(), value)
    }

    /// Test an atom sequence without changing the vocabulary.
    pub fn contains<I>(&self, atoms: I) -> Result<bool, InterningError>
    where
        I: IntoIterator<Item = K>,
    {
        let vocabulary = self
            .vocabulary
            .lock()
            .map_err(|_| InterningError::Poisoned)?;
        let mut ids = Vec::new();
        for atom in atoms {
            let id = vocabulary.id_of(&atom).ok_or(InterningError::UnknownKey)?;
            ids.push(u32::try_from(id).map_err(|_| InterningError::UnknownId(id))?);
        }
        Ok(self.id_dictionary.contains_units(&ids))
    }

    /// Test one shared logical profile sequence without mutating the
    /// vocabulary.
    pub fn contains_atom_sequence<P>(
        &self,
        sequence: &crate::AtomSequence<P>,
    ) -> Result<bool, InterningError>
    where
        P: crate::AtomProfile<Atom = K>,
    {
        self.contains(sequence.as_atoms().iter().cloned())
    }

    /// Read a mapped value for an already-interned atom sequence.
    pub fn get_value<I>(&self, atoms: I) -> Result<Option<V>, InterningError>
    where
        I: IntoIterator<Item = K>,
    {
        let vocabulary = self
            .vocabulary
            .lock()
            .map_err(|_| InterningError::Poisoned)?;
        let ids = atoms
            .into_iter()
            .map(|atom| {
                vocabulary
                    .id_of(&atom)
                    .ok_or(InterningError::UnknownKey)
                    .and_then(|id| u32::try_from(id).map_err(|_| InterningError::UnknownId(id)))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(self.id_dictionary.get_units_value(&ids))
    }

    /// Read a mapped value for one already-interned shared logical profile
    /// sequence.
    pub fn get_atom_sequence_value<P>(
        &self,
        sequence: &crate::AtomSequence<P>,
    ) -> Result<Option<V>, InterningError>
    where
        P: crate::AtomProfile<Atom = K>,
    {
        self.get_value(sequence.as_atoms().iter().cloned())
    }

    /// Remove an atom sequence without changing vocabulary assignments.
    pub fn remove<I>(&self, atoms: I) -> Result<bool, InterningError>
    where
        I: IntoIterator<Item = K>,
    {
        let vocabulary = self
            .vocabulary
            .lock()
            .map_err(|_| InterningError::Poisoned)?;
        let mut ids = Vec::new();
        for atom in atoms {
            let id = vocabulary.id_of(&atom).ok_or(InterningError::UnknownKey)?;
            ids.push(u32::try_from(id).map_err(|_| InterningError::UnknownId(id))?);
        }
        Ok(self.id_dictionary.remove_units(&ids))
    }
}

#[cfg(test)]
mod coordinated_tests {
    use super::{
        InternedSequence, InternedSequenceDictionary, InternedSequenceDictionaryU64,
        InternedUlebSequenceDictionary, InternedUlebSequenceDictionaryU64,
    };
    use crate::Uleb128;

    #[test]
    fn coordinates_atoms_and_id_sequences() {
        let dictionary = InternedSequenceDictionary::<u32, u32>::with_generation(7);
        assert!(dictionary.insert([10, 20], Some(99)).unwrap());
        assert!(dictionary.contains([10, 20]).unwrap());
        assert_eq!(dictionary.get_value([10, 20]).unwrap(), Some(99));
        let ids = InternedSequence::from_ids_with_generation(7, [0, 1]);
        assert!(dictionary.contains_id_sequence(&ids).unwrap());
        assert_eq!(dictionary.get_id_sequence_value(&ids).unwrap(), Some(99));
        assert_eq!(dictionary.vocabulary().unwrap().generation(), 7);
        assert_eq!(dictionary.generation(), Ok(7));
        assert_eq!(dictionary.id_dictionary().term_count(), 1);
        assert_eq!(
            dictionary.visible_entries().unwrap(),
            vec![(vec![10, 20], Some(99))]
        );
        assert_eq!(
            dictionary.id_dictionary().visible_entries(),
            vec![(vec![0u32, 1u32], Some(99))]
        );
        assert!(dictionary.remove([10, 20]).unwrap());
        assert!(!dictionary.contains([10, 20]).unwrap());
    }

    #[test]
    fn unknown_atoms_fail_closed_without_mutation() {
        let dictionary = InternedSequenceDictionary::<u32>::new();
        assert_eq!(
            dictionary.contains([1]),
            Err(super::InterningError::UnknownKey)
        );
        assert_eq!(dictionary.vocabulary().unwrap().len(), 0);
    }

    #[test]
    fn canonical_uleb_atoms_use_the_same_composite_boundary() {
        let dictionary = InternedUlebSequenceDictionary::<u32>::with_generation(3);
        let atoms = [Uleb128::from_u64(624_485), Uleb128::from_u64(1u64 << 63)];
        assert!(dictionary.insert(atoms.iter().cloned(), Some(11)).unwrap());
        assert!(dictionary.contains(atoms.iter().cloned()).unwrap());
        let vocabulary = dictionary.vocabulary().unwrap();
        assert_eq!(vocabulary.len(), 2);
        assert_eq!(vocabulary.generation(), 3);
    }

    #[test]
    fn profile_sequences_use_the_same_interning_boundary() {
        let sequence = crate::AtomSequence::<crate::Uleb128Atom>::from_atoms([
            Uleb128::from_u64(624_485),
            Uleb128::from_u64(1u64 << 63),
        ]);
        let dictionary = InternedUlebSequenceDictionary::<u32>::with_generation(4);
        assert!(dictionary
            .insert_atom_sequence(&sequence, Some(31))
            .unwrap());
        assert!(dictionary.contains_atom_sequence(&sequence).unwrap());
        assert_eq!(
            dictionary.get_atom_sequence_value(&sequence).unwrap(),
            Some(31)
        );

        let wide_dictionary = InternedUlebSequenceDictionaryU64::<u32>::new();
        assert!(wide_dictionary
            .insert_atom_sequence(&sequence, Some(37))
            .unwrap());
        assert_eq!(
            wide_dictionary.get_atom_sequence_value(&sequence).unwrap(),
            Some(37)
        );
    }

    #[test]
    fn explicit_u64_carrier_preserves_generation_binding() {
        let dictionary = InternedSequenceDictionaryU64::<u32, u32>::with_generation(9);
        assert!(dictionary.insert([u32::MAX], Some(17)).unwrap());
        assert!(dictionary.contains([u32::MAX]).unwrap());
        assert_eq!(dictionary.get_value([u32::MAX]).unwrap(), Some(17));
        let ids = InternedSequence::from_ids_with_generation(9, [0]);
        assert!(dictionary.contains_id_sequence(&ids).unwrap());
        assert_eq!(dictionary.get_id_sequence_value(&ids).unwrap(), Some(17));
        assert_eq!(
            dictionary.visible_entries().unwrap(),
            vec![(vec![u32::MAX], Some(17))]
        );
        assert_eq!(dictionary.vocabulary().unwrap().generation(), 9);
        assert_eq!(dictionary.generation(), Ok(9));
        let snapshot = dictionary.vocabulary_snapshot().unwrap();
        assert_eq!(snapshot.generation(), 9);
        assert_eq!(snapshot.value(0), Some(&u32::MAX));
    }

    #[test]
    fn uleb_alias_exposes_explicit_u64_carrier() {
        let dictionary = InternedUlebSequenceDictionaryU64::<u32>::with_generation(12);
        let atoms = [Uleb128::from_u64(1u64 << 63), Uleb128::from_u64(624_485)];
        assert!(dictionary.insert(atoms.iter().cloned(), Some(23)).unwrap());
        assert_eq!(
            dictionary.get_value(atoms.iter().cloned()).unwrap(),
            Some(23)
        );
        let ids = InternedSequence::from_ids_with_generation(12, [0, 1]);
        assert!(dictionary.contains_id_sequence(&ids).unwrap());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Uleb128;

    #[test]
    fn interning_is_bijective_and_deterministic() {
        let mut vocabulary = InternedVocabulary::new();
        assert_eq!(vocabulary.generation(), 0);
        let first = vocabulary.intern(Uleb128::from_u64(42));
        assert_eq!(first, vocabulary.intern(Uleb128::from_u64(42)));
        let second = vocabulary.intern(Uleb128::from_u64(1 << 63));
        assert_eq!(vocabulary.id_of(&Uleb128::from_u64(42)), Some(first));
        assert_eq!(vocabulary.value(second), Some(&Uleb128::from_u64(1 << 63)));
        assert_eq!(vocabulary.len(), 2);
        assert_eq!(
            vocabulary.iter().map(|(id, _)| id).collect::<Vec<_>>(),
            vec![0, 1]
        );
        let sequence =
            vocabulary.intern_sequence([Uleb128::from_u64(42), Uleb128::from_u64(1 << 63)]);
        assert_eq!(sequence.as_ids(), &[first, second]);
        assert_eq!(sequence.generation(), 0);
        assert!(sequence.is_bound_to(0));
        let resolved: Vec<_> = vocabulary.resolve_sequence(&sequence).unwrap().collect();
        assert_eq!(resolved.len(), 2);
        assert!(vocabulary
            .resolve_iter(&sequence)
            .all(|value| value.is_some()));
        let unknown = InternedSequence::from_ids([99]);
        assert_eq!(vocabulary.resolve_iter(&unknown).next(), Some(None));
        assert_eq!(vocabulary.validate_sequence(&sequence), Ok(()));
        assert_eq!(
            vocabulary.validate_sequence(&unknown),
            Err(InterningError::UnknownId(99))
        );
        assert_eq!(vocabulary.value(InternedId::MAX), None);
        let other = InternedVocabulary::<Uleb128>::with_generation(7);
        assert_eq!(other.generation(), 7);
        assert_eq!(
            other.validate_sequence(&sequence),
            Err(InterningError::GenerationMismatch {
                expected: 7,
                actual: 0,
            })
        );
    }

    #[test]
    fn vocabulary_snapshot_isolated_from_later_mutation() {
        let mut vocabulary = InternedVocabulary::with_generation(17);
        let first = vocabulary.intern(Uleb128::from_u64(3));
        let snapshot = vocabulary.snapshot();
        vocabulary.intern(Uleb128::from_u64(4));

        assert_eq!(snapshot.generation(), 17);
        assert_eq!(snapshot.value(first), Some(&Uleb128::from_u64(3)));
        assert_eq!(snapshot.len(), 1);
        assert_eq!(vocabulary.len(), 2);
        let sequence = InternedSequence::from_ids_with_generation(17, [first]);
        assert_eq!(snapshot.validate_sequence(&sequence), Ok(()));
        assert_eq!(
            snapshot.resolve_iter(&sequence).collect::<Vec<_>>(),
            vec![Some(&Uleb128::from_u64(3))]
        );
        assert_eq!(
            snapshot.validate_sequence(&InternedSequence::from_ids_with_generation(18, [first])),
            Err(InterningError::GenerationMismatch {
                expected: 17,
                actual: 18,
            })
        );
    }

    #[test]
    fn f64_bits_alias_preserves_raw_identity() {
        let dictionary = InternedF64BitsSequenceDictionary::<u16>::with_generation(3);
        let atoms = [(-0.0f64).to_bits(), 0x7ff8_0000_0000_0042u64];
        assert!(dictionary.insert(atoms, Some(11)).unwrap());
        assert!(dictionary.contains(atoms).unwrap());
        assert_eq!(dictionary.get_value(atoms).unwrap(), Some(11));
        assert!(!dictionary
            .contains([0u64, 0x7ff8_0000_0000_0042u64])
            .unwrap());
    }
}
