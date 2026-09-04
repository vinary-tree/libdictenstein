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
    pub fn visible_entries(&self) -> Result<Vec<(Uleb128Sequence, Option<V>)>, Uleb128Error> {
        self.inner
            .iter_with_values()
            .map(|(bytes, value)| Ok((Uleb128Sequence::from_encoded(&bytes)?, value)))
            .collect()
    }
}
