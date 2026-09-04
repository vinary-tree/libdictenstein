//! Deterministic capsule-local vocabulary interning.

use std::collections::BTreeMap;

/// Dense identifier assigned by an [`InternedVocabulary`].
pub type InternedId = u64;

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
    }
}
