//! Immutable adaptive edge storage shared by the persistent ARTrie variants.
//!
//! The store is optimized for copy-on-write publication: writers clone the current
//! immutable edge set, modify the clone, and publish it atomically at the caller's
//! existing synchronization boundary. Readers only borrow from the published store.

#![cfg_attr(part_legacy_edge_store, allow(dead_code))]

use std::fmt;
use std::hash::Hash;

use rustc_hash::FxHashMap;
use smallvec::SmallVec;

const TINY_LIMIT: usize = 4;
const SMALL_LIMIT: usize = 16;
const SORTED_LIMIT: usize = 64;
const BYTE_INDEXED_LIMIT: usize = 48;
const BYTE_INDEX48_SENTINEL: u8 = u8::MAX;
const BYTE_DENSE_SENTINEL: u16 = u16::MAX;

/// An edge vector whose labels have been proven strictly ascending and unique.
///
/// The field is private so code outside this module cannot manufacture the
/// witness without validation. Mapping only the child values preserves the
/// label proof and lets persistent-image decoders validate raw indices once,
/// then construct immutable child stores without a second branch per edge.
#[derive(Debug)]
pub(crate) struct SortedUniqueEntries<L, C> {
    entries: Vec<(L, C)>,
}

impl<L: AdaptiveLabel, C> SortedUniqueEntries<L, C> {
    /// Validate an externally assembled edge vector. The error is the index of
    /// the second edge in the first non-increasing pair.
    pub(crate) fn try_new(entries: Vec<(L, C)>) -> Result<Self, usize> {
        if let Some(index) = entries.windows(2).position(|pair| pair[0].0 >= pair[1].0) {
            return Err(index + 1);
        }
        Ok(Self { entries })
    }

    /// Map child payloads while retaining the validated label sequence.
    pub(crate) fn try_map<D, E, F, A>(
        &self,
        mut map: F,
        allocation_error: A,
    ) -> Result<SortedUniqueEntries<L, D>, E>
    where
        F: FnMut(&C) -> Result<D, E>,
        A: FnOnce(std::collections::TryReserveError) -> E,
    {
        let mut mapped = Vec::new();
        mapped
            .try_reserve_exact(self.entries.len())
            .map_err(allocation_error)?;
        for (label, child) in &self.entries {
            mapped.push((*label, map(child)?));
        }
        Ok(SortedUniqueEntries { entries: mapped })
    }

    #[inline]
    pub(crate) fn as_slice(&self) -> &[(L, C)] {
        &self.entries
    }

    #[inline]
    fn trusted(entries: Vec<(L, C)>) -> Self {
        debug_assert!(entries.windows(2).all(|pair| pair[0].0 < pair[1].0));
        Self { entries }
    }
}

/// Label types supported by `AdaptiveEdgeStore`.
pub trait AdaptiveLabel: Copy + Eq + Ord + Hash + Send + Sync + 'static {
    /// Return the byte value for byte-specialized ART tiers.
    #[inline]
    fn as_byte(self) -> Option<u8> {
        None
    }

    #[inline]
    fn legacy_inline_limit() -> usize {
        TINY_LIMIT
    }

    #[inline]
    fn legacy_sorted_limit() -> usize {
        usize::MAX
    }

    #[inline]
    fn adaptive_sorted_limit() -> usize {
        SORTED_LIMIT
    }
}

impl AdaptiveLabel for u8 {
    #[inline]
    fn as_byte(self) -> Option<u8> {
        Some(self)
    }
}

impl AdaptiveLabel for u32 {}
impl AdaptiveLabel for u64 {
    #[inline]
    fn legacy_inline_limit() -> usize {
        SMALL_LIMIT
    }

    #[inline]
    fn legacy_sorted_limit() -> usize {
        128
    }

    #[inline]
    fn adaptive_sorted_limit() -> usize {
        128
    }
}

/// Immutable adaptive edge storage.
pub(crate) enum AdaptiveEdgeStore<L: AdaptiveLabel, C> {
    /// 0-4 edges, no heap allocation.
    Tiny(SmallVec<[(L, C); TINY_LIMIT]>),
    /// 5-16 edges, inline `SmallVec` storage.
    Small(SmallVec<[(L, C); SMALL_LIMIT]>),
    /// Sparse sorted storage for medium fanout.
    Sorted(Vec<(L, C)>),
    /// Sparse indexed storage for high-fanout non-byte labels.
    SparseIndexed {
        positions: FxHashMap<L, usize>,
        entries: Vec<(L, C)>,
    },
    /// Byte ART Node48-style index for 17-48 byte labels.
    ByteIndexed48 {
        index: Box<[u8; 256]>,
        entries: Vec<(L, C)>,
    },
    /// Byte ART Node256-style direct index for 49+ byte labels.
    ByteDense256 {
        index: Box<[u16; 256]>,
        entries: Vec<(L, C)>,
    },
}

impl<L: AdaptiveLabel, C> Default for AdaptiveEdgeStore<L, C> {
    fn default() -> Self {
        Self::new()
    }
}

impl<L: AdaptiveLabel, C> AdaptiveEdgeStore<L, C> {
    #[inline]
    pub(crate) fn new() -> Self {
        Self::Tiny(SmallVec::new())
    }

    #[inline]
    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Tiny(entries) => entries.len(),
            Self::Small(entries) => entries.len(),
            Self::Sorted(entries) => entries.len(),
            Self::SparseIndexed { entries, .. } => entries.len(),
            Self::ByteIndexed48 { entries, .. } => entries.len(),
            Self::ByteDense256 { entries, .. } => entries.len(),
        }
    }

    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline]
    pub(crate) fn find(&self, label: L) -> Option<&C> {
        match self {
            Self::Tiny(entries) => find_linear(entries, label),
            Self::Small(entries) => find_binary(entries, label),
            Self::Sorted(entries) => find_binary(entries, label),
            Self::SparseIndexed { positions, entries } => positions
                .get(&label)
                .and_then(|&idx| entries.get(idx))
                .map(|(_, child)| child),
            Self::ByteIndexed48 { index, entries } => {
                let byte = label.as_byte()? as usize;
                let idx = index[byte];
                if idx == BYTE_INDEX48_SENTINEL {
                    None
                } else {
                    entries.get(idx as usize).map(|(_, child)| child)
                }
            }
            Self::ByteDense256 { index, entries } => {
                let byte = label.as_byte()? as usize;
                let idx = index[byte];
                if idx == BYTE_DENSE_SENTINEL {
                    None
                } else {
                    entries.get(idx as usize).map(|(_, child)| child)
                }
            }
        }
    }

    #[inline]
    pub(crate) fn contains_key(&self, label: L) -> bool {
        self.find(label).is_some()
    }

    #[inline]
    pub(crate) fn entry_at(&self, index: usize) -> Option<(&L, &C)> {
        self.entries()
            .get(index)
            .map(|(label, child)| (label, child))
    }

    #[inline]
    pub(crate) fn iter(&self) -> AdaptiveEdgeIter<'_, L, C> {
        AdaptiveEdgeIter {
            inner: self.entries().iter(),
        }
    }

    #[inline]
    pub(crate) fn take(&mut self) -> Self {
        std::mem::take(self)
    }

    pub(crate) fn into_entries(self) -> Vec<(L, C)> {
        match self {
            Self::Tiny(entries) => entries.into_iter().collect(),
            Self::Small(entries) => entries.into_iter().collect(),
            Self::Sorted(entries) => entries,
            Self::SparseIndexed { entries, .. } => entries,
            Self::ByteIndexed48 { entries, .. } => entries,
            Self::ByteDense256 { entries, .. } => entries,
        }
    }

    pub(crate) fn memory_usage(&self) -> usize {
        let edge_bytes = std::mem::size_of::<(L, C)>();
        match self {
            Self::Tiny(_) => 0,
            Self::Small(_) => 0,
            Self::Sorted(entries) => entries.capacity() * edge_bytes,
            Self::SparseIndexed { positions, entries } => {
                entries.capacity() * edge_bytes
                    + positions.capacity()
                        * (std::mem::size_of::<L>() + std::mem::size_of::<usize>())
            }
            Self::ByteIndexed48 { entries, .. } => {
                entries.capacity() * edge_bytes + 256 * std::mem::size_of::<u8>()
            }
            Self::ByteDense256 { entries, .. } => {
                entries.capacity() * edge_bytes + 256 * std::mem::size_of::<u16>()
            }
        }
    }

    #[inline]
    fn entries(&self) -> &[(L, C)] {
        match self {
            Self::Tiny(entries) => entries.as_slice(),
            Self::Small(entries) => entries.as_slice(),
            Self::Sorted(entries) => entries.as_slice(),
            Self::SparseIndexed { entries, .. } => entries.as_slice(),
            Self::ByteIndexed48 { entries, .. } => entries.as_slice(),
            Self::ByteDense256 { entries, .. } => entries.as_slice(),
        }
    }

    fn from_sorted_entries_with<E, F>(
        sorted: SortedUniqueEntries<L, C>,
        build_sparse: F,
    ) -> Result<Self, E>
    where
        F: FnOnce(&[(L, C)]) -> Result<FxHashMap<L, usize>, E>,
    {
        let entries = sorted.entries;
        let len = entries.len();
        #[cfg(part_legacy_edge_store)]
        {
            if len <= TINY_LIMIT {
                return Ok(Self::Tiny(entries.into_iter().collect()));
            }
            if len <= L::legacy_inline_limit() {
                return Ok(Self::Small(entries.into_iter().collect()));
            }
            if len <= L::legacy_sorted_limit() {
                return Ok(Self::Sorted(entries));
            }
            let positions = build_sparse(&entries)?;
            return Ok(Self::SparseIndexed { positions, entries });
        }
        #[cfg(not(part_legacy_edge_store))]
        {
            if len <= TINY_LIMIT {
                return Ok(Self::Tiny(entries.into_iter().collect()));
            }
            if len <= SMALL_LIMIT {
                return Ok(Self::Small(entries.into_iter().collect()));
            }
            if entries
                .first()
                .and_then(|(label, _)| label.as_byte())
                .is_some()
            {
                if len <= BYTE_INDEXED_LIMIT {
                    return Ok(Self::ByteIndexed48 {
                        index: build_byte_index48(&entries),
                        entries,
                    });
                }
                return Ok(Self::ByteDense256 {
                    index: build_byte_dense_index(&entries),
                    entries,
                });
            }
            if len <= L::adaptive_sorted_limit() {
                return Ok(Self::Sorted(entries));
            }
            let positions = build_sparse(&entries)?;
            Ok(Self::SparseIndexed { positions, entries })
        }
    }

    /// Build the optimal tier from a validated edge vector.
    pub(crate) fn from_sorted_entries(sorted: SortedUniqueEntries<L, C>) -> Self {
        Self::from_sorted_entries_with(sorted, |entries| {
            Ok::<_, std::convert::Infallible>(build_sparse_index(entries))
        })
        .unwrap_or_else(|never| match never {})
    }

    /// Build the optimal tier while reporting an input-sized sparse-index
    /// allocation failure to an untrusted-image decoder.
    pub(crate) fn try_from_sorted_entries(
        sorted: SortedUniqueEntries<L, C>,
    ) -> Result<Self, std::collections::TryReserveError> {
        Self::from_sorted_entries_with(sorted, try_build_sparse_index)
    }
}

impl<L: AdaptiveLabel, C: Clone> AdaptiveEdgeStore<L, C> {
    pub(crate) fn with_edge(&self, label: L, child: C) -> Self {
        let entries = self.entries();
        match entries.binary_search_by_key(&label, |(edge, _)| *edge) {
            Ok(index) => {
                let mut next = Vec::with_capacity(entries.len());
                next.extend_from_slice(&entries[..index]);
                next.push((label, child));
                next.extend_from_slice(&entries[index + 1..]);
                Self::from_sorted_entries(SortedUniqueEntries::trusted(next))
            }
            Err(index) => {
                let mut next = Vec::with_capacity(entries.len() + 1);
                next.extend_from_slice(&entries[..index]);
                next.push((label, child));
                next.extend_from_slice(&entries[index..]);
                Self::from_sorted_entries(SortedUniqueEntries::trusted(next))
            }
        }
    }

    pub(crate) fn without_edge(&self, label: L) -> Option<Self> {
        let entries = self.entries();
        let index = entries
            .binary_search_by_key(&label, |(edge, _)| *edge)
            .ok()?;
        let mut next = Vec::with_capacity(entries.len().saturating_sub(1));
        next.extend_from_slice(&entries[..index]);
        next.extend_from_slice(&entries[index + 1..]);
        Some(Self::from_sorted_entries(SortedUniqueEntries::trusted(
            next,
        )))
    }
}

impl<L: AdaptiveLabel, C: Clone> Clone for AdaptiveEdgeStore<L, C> {
    fn clone(&self) -> Self {
        Self::from_sorted_entries(SortedUniqueEntries::trusted(self.entries().to_vec()))
    }
}

impl<L: AdaptiveLabel, C> fmt::Debug for AdaptiveEdgeStore<L, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let tier = match self {
            Self::Tiny(_) => "Tiny",
            Self::Small(_) => "Small",
            Self::Sorted(_) => "Sorted",
            Self::SparseIndexed { .. } => "SparseIndexed",
            Self::ByteIndexed48 { .. } => "ByteIndexed48",
            Self::ByteDense256 { .. } => "ByteDense256",
        };
        f.debug_struct("AdaptiveEdgeStore")
            .field("tier", &tier)
            .field("len", &self.len())
            .finish()
    }
}

pub(crate) struct AdaptiveEdgeIter<'a, L: AdaptiveLabel, C> {
    inner: std::slice::Iter<'a, (L, C)>,
}

impl<'a, L: AdaptiveLabel, C> Iterator for AdaptiveEdgeIter<'a, L, C> {
    type Item = (&'a L, &'a C);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|(label, child)| (label, child))
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<L: AdaptiveLabel, C> ExactSizeIterator for AdaptiveEdgeIter<'_, L, C> {}

fn find_linear<L: AdaptiveLabel, C>(entries: &[(L, C)], label: L) -> Option<&C> {
    for (edge, child) in entries {
        if *edge == label {
            return Some(child);
        }
        if *edge > label {
            return None;
        }
    }
    None
}

fn find_binary<L: AdaptiveLabel, C>(entries: &[(L, C)], label: L) -> Option<&C> {
    entries
        .binary_search_by_key(&label, |(edge, _)| *edge)
        .ok()
        .map(|index| &entries[index].1)
}

fn build_sparse_index<L: AdaptiveLabel, C>(entries: &[(L, C)]) -> FxHashMap<L, usize> {
    let mut positions = FxHashMap::default();
    positions.reserve(entries.len());
    for (idx, (label, _)) in entries.iter().enumerate() {
        positions.insert(*label, idx);
    }
    positions
}

fn try_build_sparse_index<L: AdaptiveLabel, C>(
    entries: &[(L, C)],
) -> Result<FxHashMap<L, usize>, std::collections::TryReserveError> {
    let mut positions = FxHashMap::default();
    positions.try_reserve(entries.len())?;
    for (idx, (label, _)) in entries.iter().enumerate() {
        positions.insert(*label, idx);
    }
    Ok(positions)
}

fn build_byte_index48<L: AdaptiveLabel, C>(entries: &[(L, C)]) -> Box<[u8; 256]> {
    debug_assert!(entries.len() <= BYTE_INDEXED_LIMIT);
    let mut index = Box::new([BYTE_INDEX48_SENTINEL; 256]);
    for (idx, (label, _)) in entries.iter().enumerate() {
        debug_assert!(idx < BYTE_INDEX48_SENTINEL as usize);
        if let Some(byte) = label.as_byte() {
            index[byte as usize] = idx as u8;
        }
    }
    index
}

fn build_byte_dense_index<L: AdaptiveLabel, C>(entries: &[(L, C)]) -> Box<[u16; 256]> {
    let mut index = Box::new([BYTE_DENSE_SENTINEL; 256]);
    for (idx, (label, _)) in entries.iter().enumerate() {
        if let Some(byte) = label.as_byte() {
            index[byte as usize] = idx as u16;
        }
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_tiers_preserve_lookup_and_order() {
        let mut store = AdaptiveEdgeStore::<u8, u16>::new();
        for key in (0u8..80).rev() {
            store = store.with_edge(key, key as u16 + 1);
        }

        assert_eq!(store.len(), 80);
        for key in 0u8..80 {
            assert_eq!(store.find(key), Some(&(key as u16 + 1)));
        }
        assert_eq!(store.find(200), None);

        let keys: Vec<_> = store.iter().map(|(&key, _)| key).collect();
        assert_eq!(keys, (0u8..80).collect::<Vec<_>>());
    }

    #[test]
    fn byte_indexed48_uses_one_byte_positions() {
        let mut store = AdaptiveEdgeStore::<u8, u16>::new();
        for key in 0u8..=BYTE_INDEXED_LIMIT as u8 {
            store = store.with_edge(key, key as u16);
        }

        match &store {
            AdaptiveEdgeStore::ByteDense256 { .. } => {}
            other => panic!("expected dense tier after 49 edges, got {other:?}"),
        }

        let mut store = AdaptiveEdgeStore::<u8, u16>::new();
        for key in 0u8..BYTE_INDEXED_LIMIT as u8 {
            store = store.with_edge(key, key as u16);
        }

        match &store {
            AdaptiveEdgeStore::ByteIndexed48 { .. } => {}
            other => panic!("expected indexed48 tier, got {other:?}"),
        }
        assert!(store.memory_usage() < 512 + BYTE_INDEXED_LIMIT * std::mem::size_of::<(u8, u16)>());
    }

    #[test]
    fn sparse_tiers_preserve_lookup_and_order() {
        let mut store = AdaptiveEdgeStore::<u32, u16>::new();
        let mut expected = Vec::new();
        for idx in (0u32..90).rev() {
            let key = idx * 17 + 3;
            expected.push(key);
            store = store.with_edge(key, idx as u16);
        }
        expected.sort_unstable();

        assert_eq!(store.len(), expected.len());
        for key in &expected {
            assert!(store.find(*key).is_some());
        }
        assert_eq!(store.find(4), None);

        let keys: Vec<_> = store.iter().map(|(&key, _)| key).collect();
        assert_eq!(keys, expected);
    }

    #[test]
    fn replacement_and_removal_rebuild_correct_tier() {
        let mut store = AdaptiveEdgeStore::<u64, u64>::new();
        for key in 0..70 {
            store = store.with_edge(key, key);
        }
        store = store.with_edge(42, 999);
        assert_eq!(store.find(42), Some(&999));

        for key in 16..70 {
            store = store.without_edge(key).expect("key should exist");
        }
        assert_eq!(store.len(), 16);
        assert_eq!(store.find(42), None);
        let keys: Vec<_> = store.iter().map(|(&key, _)| key).collect();
        assert_eq!(keys, (0..16).collect::<Vec<_>>());
    }

    #[test]
    fn replacement_preserves_sorted_order_without_len_growth() {
        let mut store = AdaptiveEdgeStore::<u8, u16>::new();
        for key in *b"dacb" {
            store = store.with_edge(key, key as u16);
        }

        store = store.with_edge(b'c', 999);

        assert_eq!(store.len(), 4);
        assert_eq!(store.find(b'c'), Some(&999));
        let keys: Vec<_> = store.iter().map(|(&key, _)| key).collect();
        assert_eq!(keys, vec![b'a', b'b', b'c', b'd']);
    }

    #[test]
    fn sorted_unique_witness_maps_children_without_revalidating_labels() {
        let witness = SortedUniqueEntries::try_new(vec![(3u64, 7u32), (5, 11), (8, 13)])
            .expect("labels are strictly ascending and unique");
        let mapped = witness
            .try_map(
                |child| Ok::<_, &'static str>(u64::from(*child) * 2),
                |_| "allocation failed",
            )
            .expect("bounded mapping succeeds");

        assert_eq!(mapped.as_slice(), &[(3, 14), (5, 22), (8, 26)]);
    }

    #[test]
    fn sorted_unique_witness_stops_at_the_first_child_mapping_error() {
        let witness = SortedUniqueEntries::try_new(vec![(3u64, 7u32), (5, 11), (8, 13)])
            .expect("labels are strictly ascending and unique");
        let mut visited = Vec::new();
        let error = witness
            .try_map(
                |child| {
                    visited.push(*child);
                    if *child == 11 {
                        Err("sentinel child")
                    } else {
                        Ok(u64::from(*child))
                    }
                },
                |_| "allocation failed",
            )
            .expect_err("the mapper error must be preserved");

        assert_eq!(error, "sentinel child");
        assert_eq!(visited, vec![7, 11]);
    }

    #[test]
    fn fallible_sparse_construction_preserves_the_builder_error() {
        let entries = (0u64..=128).map(|label| (label, label)).collect();
        let witness = SortedUniqueEntries::try_new(entries)
            .expect("generated labels are strictly ascending and unique");

        let error = AdaptiveEdgeStore::<u64, u64>::from_sorted_entries_with(witness, |_| {
            Err::<FxHashMap<u64, usize>, _>("sentinel sparse allocation")
        })
        .expect_err("129 u64 edges require the sparse-index builder");

        assert_eq!(error, "sentinel sparse allocation");
    }
}
