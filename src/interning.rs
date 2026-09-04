//! Deterministic capsule-local vocabulary interning.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::dynamic_dawg::DynamicDawgU32;
use crate::DictionaryValue;
use crate::Uleb128;

/// Dense identifier assigned by an [`InternedVocabulary`].
pub type InternedId = u64;

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
        if let Some(&id) = self.forward.get(&key) {
            return id;
        }
        let id = self.reverse.len() as InternedId;
        self.forward.insert(key.clone(), id);
        self.reverse.push(key);
        id
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
        self.reverse.get(id as usize)
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

    /// Access the ID-native dictionary for hot-loop consumers.
    ///
    /// Its sequences are meaningful only with this instance's vocabulary and
    /// generation.  The vocabulary remains the authority for constructing
    /// valid sequences.
    #[inline]
    pub fn id_dictionary(&self) -> &DynamicDawgU32<V> {
        &self.id_dictionary
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
        let sequence = vocabulary.intern_sequence(atoms);
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
    use super::{InternedSequenceDictionary, InternedUlebSequenceDictionary};
    use crate::Uleb128;

    #[test]
    fn coordinates_atoms_and_id_sequences() {
        let dictionary = InternedSequenceDictionary::<u32, u32>::with_generation(7);
        assert!(dictionary.insert([10, 20], Some(99)).unwrap());
        assert!(dictionary.contains([10, 20]).unwrap());
        assert_eq!(dictionary.vocabulary().unwrap().generation(), 7);
        assert_eq!(dictionary.id_dictionary().term_count(), 1);
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
}
