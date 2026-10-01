//! Immutable, active-source views of suffix automata.
//!
//! Graph paths may survive removal. These queries therefore use the active
//! source-record index, not graph reachability, for membership and frequency.

use std::sync::Arc;

use super::core::SuffixAutomatonInner;
use crate::value::DictionaryValue;
use crate::CharUnit;

/// Unit-specific source-text scanning for the two suffix-automaton families.
///
/// Source text is always valid UTF-8. Byte mode measures source lengths and
/// empty-pattern boundaries in bytes; nonempty patterns are valid UTF-8 and
/// match by their byte sequence. Char mode counts Unicode scalar boundaries.
pub trait SuffixIndexUnit: CharUnit {
    /// Number of transition units in the source text.
    fn source_len(text: &str) -> usize;

    /// Number of overlapping occurrences, including unit boundaries for empty.
    fn occurrences(source: &str, pattern: &str) -> usize;

    /// Whether a pattern occurs, stopping at its first occurrence.
    fn has_occurrence(source: &str, pattern: &str) -> bool;
}

impl SuffixIndexUnit for u8 {
    #[inline]
    fn source_len(text: &str) -> usize {
        text.len()
    }

    fn occurrences(source: &str, pattern: &str) -> usize {
        if pattern.is_empty() {
            return source.len() + 1;
        }
        source
            .as_bytes()
            .windows(pattern.len())
            .filter(|window| *window == pattern.as_bytes())
            .count()
    }

    fn has_occurrence(source: &str, pattern: &str) -> bool {
        if pattern.is_empty() {
            return true;
        }
        source
            .as_bytes()
            .windows(pattern.len())
            .any(|window| window == pattern.as_bytes())
    }
}

impl SuffixIndexUnit for char {
    #[inline]
    fn source_len(text: &str) -> usize {
        text.chars().count()
    }

    fn occurrences(source: &str, pattern: &str) -> usize {
        if pattern.is_empty() {
            return source.chars().count() + 1;
        }
        source
            .char_indices()
            .filter(|(start, _)| source[*start..].starts_with(pattern))
            .count()
    }

    fn has_occurrence(source: &str, pattern: &str) -> bool {
        if pattern.is_empty() {
            return true;
        }
        source
            .char_indices()
            .any(|(start, _)| source[start..].starts_with(pattern))
    }
}

/// One active insertion record. Equal source texts remain distinct records.
#[derive(Debug, Clone, Copy)]
pub struct SuffixSourceRecord<'a, V> {
    /// Stable insertion ID within this producer's history.
    pub source_id: usize,
    /// Valid UTF-8 source text.
    pub text: &'a str,
    /// Optional value attached to this particular insertion.
    pub value: Option<&'a V>,
}

/// One immutable source-index revision, independent of later mutations.
///
/// The producer ID is process-local. Revision starts at zero and advances once
/// per successful state publication. Neither identifier is serialized.
///
/// # Example
///
/// ```rust
/// use libdictenstein::suffix_automaton::SuffixAutomaton;
///
/// let index = SuffixAutomaton::<()>::from_texts(["banana", "bandana"]);
/// let snapshot = index.source_snapshot();
/// assert_eq!(snapshot.source_count(), 2);
/// assert!(!snapshot.contains_source("ana"));
/// assert_eq!(snapshot.substring_frequency("ana"), 3);
/// index.remove("banana");
/// assert_eq!(snapshot.substring_frequency("ana"), 3); // captured revision
/// assert_eq!(index.source_snapshot().substring_frequency("ana"), 1);
/// ```
pub struct SuffixSourceSnapshot<U: SuffixIndexUnit, V: DictionaryValue> {
    inner: Arc<SuffixAutomatonInner<U, V>>,
}

impl<U: SuffixIndexUnit, V: DictionaryValue> Clone for SuffixSourceSnapshot<U, V> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<U: SuffixIndexUnit, V: DictionaryValue> SuffixSourceSnapshot<U, V> {
    pub(crate) fn from_root(inner: Arc<SuffixAutomatonInner<U, V>>) -> Self {
        Self { inner }
    }

    /// Process-local identity of the producing automaton.
    pub fn producer_id(&self) -> u64 {
        self.inner.producer_id
    }

    /// Number of successful publications visible in this captured root.
    pub fn revision(&self) -> u64 {
        self.inner.revision
    }

    /// Number of currently active insertion records, including duplicates.
    pub fn source_count(&self) -> usize {
        self.inner.sorted_source_indices.len()
    }

    /// Active record at a lexicographic rank (ties ordered by insertion ID).
    pub fn source_at(&self, rank: usize) -> Option<SuffixSourceRecord<'_, V>> {
        let source_id = *self.inner.sorted_source_indices.get(rank)?;
        Some(SuffixSourceRecord {
            source_id,
            text: &self.inner.source_texts[source_id],
            value: self.inner.source_values[source_id].as_ref(),
        })
    }

    /// Iterate active records in lexicographic order, preserving multiplicity.
    pub fn records(&self) -> impl ExactSizeIterator<Item = SuffixSourceRecord<'_, V>> + '_ {
        (0..self.source_count()).map(|rank| {
            self.source_at(rank)
                .expect("rank came from the captured active index")
        })
    }

    /// Whether at least one active record has exactly this source text.
    pub fn contains_source(&self, text: &str) -> bool {
        let ids = &self.inner.sorted_source_indices;
        let first = ids.partition_point(|&id| self.inner.source_texts[id].as_str() < text);
        ids.get(first)
            .is_some_and(|&id| self.inner.source_texts[id] == text)
    }

    /// Occurrence count over active records, including overlaps and duplicates.
    ///
    /// The empty pattern appears at every unit boundary: a source of length
    /// $`n`$ contributes $`n + 1`$, including one occurrence in an empty source.
    pub fn substring_frequency(&self, pattern: &str) -> usize {
        self.inner
            .sorted_source_indices
            .iter()
            .fold(0, |count, &id| {
                let source = &self.inner.source_texts[id];
                let next = if pattern.is_empty() {
                    U::source_len(source)
                        .checked_add(1)
                        .expect("suffix-index source length overflow")
                } else {
                    U::occurrences(source, pattern)
                };
                count
                    .checked_add(next)
                    .expect("suffix-index frequency overflow")
            })
    }

    /// Whether a pattern occurs in any active source record.
    ///
    /// An empty pattern is present iff at least one active record exists.
    pub fn contains_substring(&self, pattern: &str) -> bool {
        if pattern.is_empty() {
            return self.source_count() != 0;
        }
        self.inner
            .sorted_source_indices
            .iter()
            .any(|&id| U::has_occurrence(&self.inner.source_texts[id], pattern))
    }
}

#[cfg(test)]
mod tests {
    use super::SuffixIndexUnit;
    use crate::suffix_automaton::{SuffixAutomaton, SuffixAutomatonChar};
    use crate::MutableMappedDictionary;

    #[test]
    fn duplicate_records_values_removal_and_old_revisions() {
        let index = SuffixAutomaton::<u32>::new();
        let initial = index.source_snapshot();
        assert_eq!(initial.revision(), 0);
        assert!(!initial.contains_substring(""));
        assert_eq!(initial.substring_frequency(""), 0);

        index.insert_with_value("aba", 0);
        index.insert("aba");
        index.insert_with_value("ababa", 7);
        index.insert("");
        let captured = index.source_snapshot();
        assert_eq!(captured.revision(), 4);
        assert_eq!(captured.source_count(), 4);
        let records: Vec<_> = captured
            .records()
            .map(|record| (record.source_id, record.text, record.value.copied()))
            .collect();
        assert_eq!(
            records,
            vec![
                (3, "", None),
                (0, "aba", Some(0)),
                (1, "aba", None),
                (2, "ababa", Some(7)),
            ]
        );
        assert!(captured.contains_source("aba"));
        assert!(!captured.contains_source("ba"));
        assert_eq!(captured.substring_frequency("aba"), 4);
        assert_eq!(captured.substring_frequency(""), 15);

        assert!(index.remove("aba"));
        let reduced = index.source_snapshot();
        assert_eq!(reduced.producer_id(), captured.producer_id());
        assert_eq!(reduced.revision(), 5);
        assert_eq!(reduced.substring_frequency("aba"), 3);
        assert_eq!(captured.substring_frequency("aba"), 4);
        assert!(index.remove("aba"));
        assert!(!index.source_snapshot().contains_source("aba"));
        assert_eq!(index.source_snapshot().substring_frequency("aba"), 2);
        assert!(index.remove("ababa"));
        let final_source = index.source_snapshot();
        assert!(!final_source.contains_substring("aba"));
        assert_eq!(final_source.substring_frequency("aba"), 0);
        assert!(final_source.contains_substring(""));
        assert_eq!(final_source.substring_frequency(""), 1);
        index.compact();
        assert!(!index.source_snapshot().contains_substring("aba"));
        assert!(index.remove(""));
        let empty = index.source_snapshot();
        assert_eq!(empty.source_count(), 0);
        assert!(!empty.contains_substring(""));
        assert_eq!(empty.substring_frequency(""), 0);
        assert!(!index.remove("missing"));
        assert_eq!(index.source_snapshot().revision(), empty.revision());
        assert_eq!(captured.substring_frequency("aba"), 4);
    }

    #[test]
    fn byte_and_scalar_boundaries_are_distinct() {
        assert_eq!(<u8 as SuffixIndexUnit>::occurrences("é", ""), 3);
        assert_eq!(<char as SuffixIndexUnit>::occurrences("é", ""), 2);
        assert!(<u8 as SuffixIndexUnit>::has_occurrence("", ""));
        assert!(<char as SuffixIndexUnit>::has_occurrence("", ""));
        let byte = SuffixAutomaton::<()>::from_texts(["é🙂é", ""]);
        let scalar = SuffixAutomatonChar::<()>::from_texts(["é🙂é", ""]);
        let byte = byte.source_snapshot();
        let scalar = scalar.source_snapshot();
        assert_eq!(byte.substring_frequency(""), 10);
        assert_eq!(scalar.substring_frequency(""), 5);
        assert_eq!(byte.substring_frequency("é"), 2);
        assert_eq!(scalar.substring_frequency("é"), 2);
        assert_ne!(byte.producer_id(), scalar.producer_id());
        assert_eq!(byte.revision(), 0);
        assert_eq!(scalar.revision(), 0);
    }

    #[test]
    fn captured_root_survives_clear_and_concurrent_writes() {
        let index = SuffixAutomatonChar::<()>::from_texts(["éé", ""]);
        let captured = index.source_snapshot();
        let writer = index.clone();
        std::thread::spawn(move || {
            writer.clear();
            for _ in 0..16 {
                writer.insert("new");
            }
        })
        .join()
        .expect("writer thread");
        assert_eq!(captured.source_count(), 2);
        assert_eq!(captured.substring_frequency("é"), 2);
        assert_eq!(captured.substring_frequency(""), 4);
        let current = index.source_snapshot();
        assert_eq!(current.producer_id(), captured.producer_id());
        assert_eq!(current.revision(), captured.revision() + 17);
        assert_eq!(current.substring_frequency("new"), 16);
        assert_eq!(current.substring_frequency(""), 64);
        assert_ne!(current.source_count(), captured.source_count());
    }

    #[cfg(feature = "serialization")]
    #[test]
    fn deserialization_rebuilds_active_index_with_new_live_identity() {
        use crate::serialization::bincode_compat;

        let byte = SuffixAutomaton::<u32>::new();
        byte.insert_with_value("éé", 9);
        byte.insert("éé");
        byte.remove("éé");
        let before = byte.source_snapshot();
        let wire = bincode_compat::serialize(&byte).expect("serialize suffix index");
        let restored: SuffixAutomaton<u32> =
            bincode_compat::deserialize(&wire).expect("deserialize suffix index");
        let after = restored.source_snapshot();
        assert_ne!(after.producer_id(), before.producer_id());
        assert_eq!(after.revision(), 0);
        assert_eq!(after.source_count(), 1);
        assert_eq!(after.substring_frequency("é"), 2);
        assert_eq!(after.substring_frequency(""), 5);

        let scalar = SuffixAutomatonChar::<u32>::new();
        scalar.insert_with_value("éé", 9);
        let scalar_wire = bincode_compat::serialize(&scalar).expect("serialize char suffix index");
        let restored_scalar: SuffixAutomatonChar<u32> =
            bincode_compat::deserialize(&scalar_wire).expect("deserialize char suffix index");
        let scalar_after = restored_scalar.source_snapshot();
        assert_ne!(
            scalar_after.producer_id(),
            scalar.source_snapshot().producer_id()
        );
        assert_eq!(scalar_after.substring_frequency(""), 3);
        assert_eq!(
            scalar_after
                .source_at(0)
                .and_then(|record| record.value.copied()),
            Some(9)
        );
    }
}
