//! Logical UTF-8 boundary for the persistent byte ART.

use crate::{Dictionary, DictionaryValue};

use super::PersistentARTrie;

/// Persistent byte ART adapter whose public keys are validated UTF-8 strings.
/// The wrapped trie remains byte-backed; decoding occurs only at this boundary.
#[derive(Debug)]
pub struct PersistentARTrieUtf8<V: DictionaryValue = ()> {
    inner: PersistentARTrie<V>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_utf8_boundary_and_rejects_malformed_bytes() {
        let dictionary = PersistentARTrieUtf8::<u16>::new();
        assert!(dictionary.insert_with_value("λ🎉", 9));
        assert!(dictionary.contains("λ🎉"));
        assert_eq!(dictionary.get_value("λ🎉"), Some(9));
        assert!(dictionary.contains_encoded("λ🎉".as_bytes()).unwrap());
        assert_eq!(dictionary.try_term_count().unwrap(), 1);
        assert!(!dictionary.try_is_empty().unwrap());
        assert!(dictionary.contains_encoded(&[0x80]).is_err());
        assert_eq!(dictionary.visible_entries().unwrap().len(), 1);
        assert!(dictionary.remove_encoded("λ🎉".as_bytes()).unwrap());
        assert!(dictionary.is_empty());
        assert!(dictionary.try_is_empty().unwrap());
    }
}

impl<V: DictionaryValue> Default for PersistentARTrieUtf8<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V: DictionaryValue> PersistentARTrieUtf8<V> {
    /// Construct an in-memory adapter from UTF-8 terms.
    pub fn from_terms<I, T>(terms: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: AsRef<str>,
    {
        let dictionary = Self::new();
        for term in terms {
            dictionary.insert(term.as_ref());
        }
        dictionary
    }

    /// Construct an in-memory adapter from UTF-8 terms and values.
    pub fn from_terms_with_values<I, T>(entries: I) -> Self
    where
        I: IntoIterator<Item = (T, V)>,
        T: AsRef<str>,
    {
        let dictionary = Self::new();
        for (term, value) in entries {
            dictionary.insert_with_value(term.as_ref(), value);
        }
        dictionary
    }

    /// Create a fresh persistent UTF-8 dictionary at `path`.
    pub fn create<P: AsRef<std::path::Path>>(path: P) -> crate::persistent_artrie::Result<Self> {
        Ok(Self::from_inner(PersistentARTrie::create(path)?))
    }

    /// Open an existing persistent UTF-8 dictionary from `path`.
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

    /// Recover the wrapped ART for persistence operations.
    pub fn into_inner(self) -> PersistentARTrie<V> {
        self.inner
    }

    /// Borrow the wrapped ART for checkpoint/recovery controls.
    #[inline]
    pub fn inner(&self) -> &PersistentARTrie<V> {
        &self.inner
    }

    /// Number of complete UTF-8 terms.
    #[inline]
    pub fn term_count(&self) -> usize {
        self.inner.len().unwrap_or(0)
    }

    /// Count complete UTF-8 terms with an explicit traversal result.
    ///
    /// Unlike the compatibility [`term_count`](Self::term_count) accessor,
    /// this method never converts an unavailable/corrupt traversal into zero.
    pub fn try_term_count(&self) -> crate::persistent_artrie::Result<usize> {
        Ok(self
            .inner
            .iter_prefix_with_arena(b"")?
            .map_or(0, |entries| entries.len()))
    }

    /// Whether no complete UTF-8 terms are stored.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.term_count() == 0
    }

    /// Checked emptiness query; storage failures remain errors.
    pub fn try_is_empty(&self) -> crate::persistent_artrie::Result<bool> {
        Ok(self.try_term_count()? == 0)
    }

    /// Checked insertion preserving persistence failures.
    pub fn try_insert(&self, term: &str) -> crate::persistent_artrie::Result<bool> {
        self.inner.try_insert(term)
    }

    /// Insert a term, reporting only whether it was newly added.
    #[inline]
    pub fn insert(&self, term: &str) -> bool {
        self.inner.insert(term)
    }

    /// Checked value insertion preserving persistence failures.
    pub fn try_insert_with_value(
        &self,
        term: &str,
        value: V,
    ) -> crate::persistent_artrie::Result<bool> {
        self.inner.try_insert_with_value(term, value)
    }

    /// Insert or update a term with a mapped value.
    #[inline]
    pub fn insert_with_value(&self, term: &str, value: V) -> bool {
        self.inner.insert_with_value(term, value)
    }

    /// Test membership of a UTF-8 term.
    #[inline]
    pub fn contains(&self, term: &str) -> bool {
        self.inner.contains_bytes(term.as_bytes())
    }

    /// Read a mapped value for a UTF-8 term.
    #[inline]
    pub fn get_value(&self, term: &str) -> Option<V> {
        self.inner.get_value_bytes(term.as_bytes())
    }

    /// Checked removal preserving persistence failures.
    pub fn try_remove(&self, term: &str) -> crate::persistent_artrie::Result<bool> {
        self.inner.try_remove_bytes(term.as_bytes())
    }

    /// Remove a UTF-8 term.
    #[inline]
    pub fn remove(&self, term: &str) -> bool {
        self.inner.remove_bytes(term.as_bytes())
    }

    /// Validate and query an already encoded UTF-8 term.
    pub fn contains_encoded(&self, encoded: &[u8]) -> Result<bool, std::str::Utf8Error> {
        std::str::from_utf8(encoded)?;
        Ok(self.inner.contains_bytes(encoded))
    }

    /// Validate and remove an already encoded UTF-8 term.
    pub fn remove_encoded(&self, encoded: &[u8]) -> Result<bool, std::str::Utf8Error> {
        std::str::from_utf8(encoded)?;
        Ok(self.inner.remove_bytes(encoded))
    }

    /// Enumerate complete UTF-8 terms and values in byte-lexicographic order.
    ///
    /// Traversal failures and malformed persisted bytes are returned explicitly;
    /// neither is converted into an empty result.
    pub fn visible_entries(&self) -> crate::persistent_artrie::Result<Vec<(String, Option<V>)>> {
        let entries = self.inner.iter_prefix_with_arena(b"")?.unwrap_or_default();
        entries
            .into_iter()
            .map(|entry| {
                let value = self.inner.get_value_bytes(&entry.term);
                std::str::from_utf8(&entry.term)
                    .map(|term| (term.to_owned(), value))
                    .map_err(|error| {
                        crate::persistent_artrie::PersistentARTrieError::CorruptedFile {
                            reason: format!("invalid UTF-8 entry: {error}"),
                        }
                    })
            })
            .collect()
    }
}
