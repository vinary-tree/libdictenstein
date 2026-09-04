//! Deterministic capsule-local vocabulary interning.

use std::collections::BTreeMap;

/// Dense identifier assigned by an [`InternedVocabulary`].
pub type InternedId = u64;

/// Validation failures at the vocabulary boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterningError {
    /// The ID is not present in this vocabulary generation.
    UnknownId(InternedId),
    /// The sequence belongs to a different vocabulary generation.
    GenerationMismatch { expected: u64, actual: u64 },
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
    pub fn validate_sequence(
        &self,
        sequence: &InternedSequence,
    ) -> Result<(), InterningError> {
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
        assert_eq!(vocabulary.iter().map(|(id, _)| id).collect::<Vec<_>>(), vec![0, 1]);
        let sequence = vocabulary.intern_sequence([
            Uleb128::from_u64(42),
            Uleb128::from_u64(1 << 63),
        ]);
        assert_eq!(sequence.as_ids(), &[first, second]);
        assert_eq!(sequence.generation(), 0);
        let resolved: Vec<_> = vocabulary.resolve_sequence(&sequence).unwrap().collect();
        assert_eq!(resolved.len(), 2);
        assert!(vocabulary.resolve_iter(&sequence).all(|value| value.is_some()));
        let unknown = InternedSequence::from_ids([99]);
        assert_eq!(vocabulary.resolve_iter(&unknown).next(), Some(None));
        assert_eq!(
            vocabulary.validate_sequence(&sequence),
            Ok(())
        );
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
