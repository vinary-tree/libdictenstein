//! Logical ULEB128 boundary for the persistent byte ART.

use crate::{Dictionary, DictionaryValue, Uleb128Error, Uleb128Sequence};

use super::PersistentARTrie;

/// Persistent byte ART adapter whose public keys are complete canonical ULEB128
/// sequences. The wrapped trie stores encoded bytes, while this boundary rejects
/// malformed images and never exposes continuation bytes as logical symbols.
#[derive(Debug)]
pub struct PersistentARTrieUleb128<V: DictionaryValue = ()> {
    inner: PersistentARTrie<V>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Uleb128;

    #[test]
    fn preserves_logical_boundaries_and_values() {
        let dictionary = PersistentARTrieUleb128::<u16>::new();
        let first = Uleb128Sequence::from_atoms([Uleb128::from_u64(624_485)]);
        let second = Uleb128Sequence::from_atoms([
            Uleb128::from_u64(624_485),
            Uleb128::from_u64(1u64 << 63),
        ]);
        assert!(dictionary.insert_with_value(&first, 7));
        assert!(dictionary.insert(&second));
        assert!(dictionary.contains(&first));
        assert_eq!(dictionary.get_value(&first), Some(7));
        assert_eq!(dictionary.term_count(), 2);
        assert_eq!(dictionary.try_term_count().unwrap(), 2);
        assert!(!dictionary.try_is_empty().unwrap());
        let entries = dictionary.visible_entries().unwrap();
        assert_eq!(entries.len(), 2);
        assert!(dictionary.contains_encoded(&first.to_encoded()).unwrap());
        assert!(dictionary.contains_encoded(&[0x80]).is_err());
        assert!(dictionary.remove(&first));
        assert!(!dictionary.contains(&first));
    }
}

impl<V: DictionaryValue> Default for PersistentARTrieUleb128<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V: DictionaryValue> PersistentARTrieUleb128<V> {
    /// Construct an in-memory adapter from complete ULEB sequences.
    pub fn from_sequences<I>(sequences: I) -> Self
    where
        I: IntoIterator<Item = Uleb128Sequence>,
    {
        let dictionary = Self::new();
        for sequence in sequences {
            dictionary.insert(&sequence);
        }
        dictionary
    }

    /// Construct an in-memory adapter from complete sequences and values.
    pub fn from_sequences_with_values<I>(entries: I) -> Self
    where
        I: IntoIterator<Item = (Uleb128Sequence, V)>,
    {
        let dictionary = Self::new();
        for (sequence, value) in entries {
            dictionary.insert_with_value(&sequence, value);
        }
        dictionary
    }

    /// Create a fresh persistent ULEB dictionary at `path`.
    pub fn create<P: AsRef<std::path::Path>>(path: P) -> crate::persistent_artrie::Result<Self> {
        Ok(Self::from_inner(PersistentARTrie::create(path)?))
    }

    /// Open an existing persistent ULEB dictionary from `path`.
    pub fn open<P: AsRef<std::path::Path>>(path: P) -> crate::persistent_artrie::Result<Self> {
        Ok(Self::from_inner(PersistentARTrie::open(path)?))
    }

    /// Construct an empty in-memory adapter.
    #[allow(deprecated)]
    pub fn new() -> Self {
        Self {
            inner: PersistentARTrie::new(),
        }
    }

    /// Wrap an existing persistent byte ART without copying its storage.
    pub fn from_inner(inner: PersistentARTrie<V>) -> Self {
        Self { inner }
    }

    /// Recover the wrapped byte ART for persistence operations.
    pub fn into_inner(self) -> PersistentARTrie<V> {
        self.inner
    }

    /// Borrow the wrapped ART for checkpoint/recovery controls.
    #[inline]
    pub fn inner(&self) -> &PersistentARTrie<V> {
        &self.inner
    }

    /// Number of complete logical ULEB sequences.
    #[inline]
    pub fn term_count(&self) -> usize {
        self.inner.len().unwrap_or(0)
    }

    /// Count complete logical sequences with an explicit traversal result.
    ///
    /// Unlike the compatibility [`term_count`](Self::term_count) accessor,
    /// this method never converts an unavailable/corrupt traversal into zero.
    pub fn try_term_count(&self) -> crate::persistent_artrie::Result<usize> {
        Ok(self
            .inner
            .iter_prefix_with_arena(b"")?
            .map_or(0, |entries| entries.len()))
    }

    /// Whether the logical dictionary contains no complete sequences.
    #[inline]
    pub fn is_empty(&self) -> bool {
        // A legacy bool cannot carry a storage error.  Fail closed rather
        // than turning an unavailable/corrupt image into apparent emptiness;
        // callers requiring the distinction should use `try_is_empty`.
        self.try_is_empty().unwrap_or(false)
    }

    /// Checked emptiness query; storage failures remain errors.
    pub fn try_is_empty(&self) -> crate::persistent_artrie::Result<bool> {
        Ok(self.try_term_count()? == 0)
    }

    /// Checked insertion preserving persistence failures.
    pub fn try_insert(&self, sequence: &Uleb128Sequence) -> crate::persistent_artrie::Result<bool> {
        self.inner.try_insert_bytes(&sequence.to_encoded())
    }

    /// Insert a complete canonical sequence.
    #[inline]
    pub fn insert(&self, sequence: &Uleb128Sequence) -> bool {
        self.inner.insert_bytes(&sequence.to_encoded())
    }

    /// Insert a complete canonical sequence with a value.
    #[inline]
    pub fn insert_with_value(&self, sequence: &Uleb128Sequence, value: V) -> bool {
        self.inner
            .insert_with_value_bytes(&sequence.to_encoded(), value)
    }

    /// Checked value insertion preserving persistence failures.
    pub fn try_insert_with_value(
        &self,
        sequence: &Uleb128Sequence,
        value: V,
    ) -> crate::persistent_artrie::Result<bool> {
        self.inner
            .try_insert_with_value_bytes(&sequence.to_encoded(), value)
    }

    /// Test membership of a complete sequence.
    #[inline]
    pub fn contains(&self, sequence: &Uleb128Sequence) -> bool {
        self.inner.contains_bytes(&sequence.to_encoded())
    }

    /// Read a mapped value for a complete sequence.
    #[inline]
    pub fn get_value(&self, sequence: &Uleb128Sequence) -> Option<V> {
        self.inner.get_value_bytes(&sequence.to_encoded())
    }

    /// Remove a complete sequence.
    #[inline]
    pub fn remove(&self, sequence: &Uleb128Sequence) -> bool {
        self.inner.remove_bytes(&sequence.to_encoded())
    }

    /// Checked removal preserving persistence failures.
    pub fn try_remove(&self, sequence: &Uleb128Sequence) -> crate::persistent_artrie::Result<bool> {
        self.inner.try_remove_bytes(&sequence.to_encoded())
    }

    /// Validate and query an already encoded sequence without decoding it.
    pub fn contains_encoded(&self, encoded: &[u8]) -> Result<bool, Uleb128Error> {
        Uleb128Sequence::from_encoded(encoded)?;
        Ok(self.inner.contains_bytes(encoded))
    }

    /// Validate and remove an already encoded sequence without decoding it.
    pub fn remove_encoded(&self, encoded: &[u8]) -> Result<bool, Uleb128Error> {
        Uleb128Sequence::from_encoded(encoded)?;
        Ok(self.inner.remove_bytes(encoded))
    }

    /// Enumerate complete logical sequences and values in encoded order.
    ///
    /// Traversal failures and malformed persisted codewords are returned as
    /// corruption errors; neither is converted into an empty result.
    pub fn visible_entries(
        &self,
    ) -> crate::persistent_artrie::Result<Vec<(Uleb128Sequence, Option<V>)>> {
        let entries = self.inner.iter_prefix_with_arena(b"")?.unwrap_or_default();
        entries
            .into_iter()
            .map(|entry| {
                let value = self.inner.get_value_bytes(&entry.term);
                Uleb128Sequence::from_encoded(&entry.term)
                    .map(|sequence| (sequence, value))
                    .map_err(|error| {
                        crate::persistent_artrie::PersistentARTrieError::CorruptedFile {
                            reason: format!("invalid ULEB128 entry: {error}"),
                        }
                    })
            })
            .collect()
    }
}
