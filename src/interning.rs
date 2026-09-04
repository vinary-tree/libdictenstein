//! Deterministic capsule-local vocabulary interning.

use std::collections::BTreeMap;

/// Dense identifier assigned by an [`InternedVocabulary`].
pub type InternedId = u64;

/// Compact capsule-local sequence of vocabulary IDs.
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct InternedSequence {
    ids: Vec<InternedId>,
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
        }
    }

    /// Borrow the compact ID representation.
    #[inline]
    pub fn as_ids(&self) -> &[InternedId] {
        &self.ids
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
}

impl<K: Ord + Clone> Default for InternedVocabulary<K> {
    fn default() -> Self {
        Self {
            forward: BTreeMap::new(),
            reverse: Vec::new(),
        }
    }
}

impl<K: Ord + Clone> InternedVocabulary<K> {
    /// Construct an empty vocabulary.
    #[inline]
    pub fn new() -> Self {
        Self::default()
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
        InternedSequence::from_ids(keys.into_iter().map(|key| self.intern(key)))
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
        let resolved: Vec<_> = vocabulary.resolve_sequence(&sequence).unwrap().collect();
        assert_eq!(resolved.len(), 2);
    }
}
