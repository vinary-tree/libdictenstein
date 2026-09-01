//! Disk location registry for tracking persisted node locations.
//!
//! This module maps node paths to their disk locations (SwizzledPtr) after checkpoint.
//! Only nodes in the registry can be evicted, as they have valid disk representations.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use smallvec::SmallVec;

use super::lru_tracker::LruRegistry;
use super::registry_build::{DiskRecordAddress, DurableRegistryRecord};
use super::{AtomicResidencyGeneration, PackedResidencyDelta, PackedResidencyTransition};
use crate::persistent_artrie::core::key_encoding::{ByteKey, CharKey, KeyEncoding, U64Key};
use crate::persistent_artrie::core::overlay::EvictionBinding;
use crate::persistent_artrie::core::swizzled_ptr::{NodeType, SwizzledPtr};
use crate::persistent_artrie::error::PersistentARTrieError;

/// Dense identifier for one path in a checkpoint-local prefix topology.
///
/// The all-ones value is the virtual root path. Every admitted identifier names
/// one registered-node segment in a [`PathTopology`]. Keeping the identifier
/// word-sized avoids an artificial topology-size limit; every arithmetic and
/// allocation step is checked before publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct RegistryPathId(usize);

/// Hash and disk-address buckets are singleton in the common case. Structural
/// aliases and true hash collisions spill fallibly without charging every
/// ordinary registry record for a separate heap allocation.
type RegistryPathBucket = SmallVec<[RegistryPathId; 1]>;

impl RegistryPathId {
    pub(crate) const ROOT: Self = Self(usize::MAX);

    pub(crate) fn index(self) -> Option<usize> {
        (self != Self::ROOT).then_some(self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BuilderSubtreeRange {
    root: RegistryPathId,
    end_exclusive: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct ByteBuilderSubtreeStart {
    generation: RegistryGeneration,
    root: RegistryPathId,
}

#[derive(Clone, Debug)]
pub(crate) struct CharBuilderSubtreeStart {
    generation: RegistryGeneration,
    root: RegistryPathId,
}

/// One exact completed byte subtree in a registry still under construction.
/// Private fields and the generation binding prevent cross-registry forgery.
#[derive(Clone, Debug)]
pub(crate) struct ByteBuilderSubtree {
    generation: RegistryGeneration,
    range: BuilderSubtreeRange,
    root_resident: bool,
}

/// Character-key twin of [`ByteBuilderSubtree`].
#[derive(Clone, Debug)]
pub(crate) struct CharBuilderSubtree {
    generation: RegistryGeneration,
    range: BuilderSubtreeRange,
    root_resident: bool,
}

/// Key-family-safe start token retained by the generic serializer frame.
#[derive(Clone, Debug)]
pub(crate) enum RegistryBuilderSubtreeStart {
    Byte(ByteBuilderSubtreeStart),
    Char(CharBuilderSubtreeStart),
}

/// Key-family-safe completed handle retained by the generic DAG memo.
#[derive(Clone, Debug)]
pub(crate) enum RegistryBuilderSubtree {
    Byte(ByteBuilderSubtree),
    Char(CharBuilderSubtree),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct LocalRegistryGraftStats {
    #[cfg(any(test, feature = "perf-instrumentation"))]
    pub(crate) appended_topology_entries: usize,
    #[cfg(any(test, feature = "perf-instrumentation"))]
    pub(crate) durable_records: usize,
    #[cfg(any(test, feature = "perf-instrumentation"))]
    pub(crate) serialized_bytes: usize,
    #[cfg(any(test, feature = "perf-instrumentation"))]
    pub(crate) overflowed: bool,
}

pub(crate) type RegistryGeneration = EvictionBinding;

#[derive(Debug, Clone, Copy)]
struct PathEntry {
    parent: RegistryPathId,
    segment_start: usize,
    segment_end: usize,
    depth: usize,
    hash: u64,
}

/// Prefix-sharing path topology used by one published eviction registry.
///
/// Each registered node owns only the units since its registered parent. A
/// depth-`n` trie therefore stores exactly `O(n)` path units without a hash-map
/// entry or parent/depth/hash record per uncompressed key unit. Paths are
/// reconstructed iteratively only for compatibility callbacks that require a
/// contiguous slice.
pub(crate) struct PathTopology<U> {
    entries: Vec<PathEntry>,
    units: Vec<U>,
    /// One preorder-exclusive subtree end per entry. Values are authoritative
    /// exactly when `finalized` is true. During finalization each open slot
    /// temporarily stores the preceding open ancestor, forming an intrusive
    /// pushdown stack without auxiliary allocation.
    subtree_ends: Vec<usize>,
    finalized: bool,
}

struct MappedPathReservation<'a, T> {
    parent: RegistryPathId,
    segment: &'a [T],
    suffix: Option<T>,
    root_hash: u64,
    preserve_finalized_root_sibling: bool,
}

impl<U: Copy> PathTopology<U> {
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    fn new() -> Self {
        Self {
            entries: Vec::new(),
            units: Vec::new(),
            subtree_ends: Vec::new(),
            finalized: true,
        }
    }

    fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: Vec::with_capacity(capacity),
            units: Vec::with_capacity(capacity),
            subtree_ends: Vec::with_capacity(capacity),
            finalized: true,
        }
    }

    #[inline]
    fn is_finalized(&self) -> bool {
        self.finalized && self.subtree_ends.len() == self.entries.len()
    }

    fn try_reserve_additional(
        &mut self,
        additional_entries: usize,
        additional_units: usize,
    ) -> std::result::Result<(), RegistryBuildError> {
        if self.subtree_ends.len() != self.entries.len() {
            return Err(RegistryBuildError::TopologyInvariant(
                "path topology subtree-end storage is out of lockstep",
            ));
        }
        self.entries.len().checked_add(additional_entries).ok_or(
            RegistryBuildError::Arithmetic("path topology required entry count"),
        )?;
        self.entries
            .try_reserve(additional_entries)
            .map_err(|_| RegistryBuildError::Allocation("path topology entries"))?;
        self.units
            .try_reserve(additional_units)
            .map_err(|_| RegistryBuildError::Allocation("path topology units"))?;
        self.subtree_ends
            .try_reserve(additional_entries)
            .map_err(|_| RegistryBuildError::Allocation("path topology subtree ends"))?;
        Ok(())
    }

    fn entry(&self, id: RegistryPathId) -> Option<&PathEntry> {
        self.entries.get(id.0)
    }

    pub(crate) fn parent(&self, id: RegistryPathId) -> Option<RegistryPathId> {
        self.entry(id).map(|entry| entry.parent)
    }

    pub(crate) fn segment(&self, id: RegistryPathId) -> Option<&[U]> {
        let entry = self.entry(id)?;
        self.units.get(entry.segment_start..entry.segment_end)
    }

    /// Validate preorder topology and fill the already-reserved end index.
    ///
    /// Unfinalized end slots form an intrusive predecessor stack during this
    /// pass. The method therefore performs no allocation and uses O(1)
    /// auxiliary memory regardless of topology depth.
    fn try_finalize_subtree_ends(&mut self) -> std::result::Result<(), RegistryBuildError> {
        if self.is_finalized() {
            return Ok(());
        }
        self.finalized = false;
        if self.subtree_ends.len() != self.entries.len() {
            return Err(RegistryBuildError::TopologyInvariant(
                "path topology subtree-end storage is out of lockstep",
            ));
        }

        let mut top = RegistryPathId::ROOT;
        for (index, entry) in self.entries.iter().enumerate() {
            while top != entry.parent {
                let completed = top.index().ok_or(RegistryBuildError::TopologyInvariant(
                    "entry parent is outside the active preorder ancestry",
                ))?;
                let preceding = *self.subtree_ends.get(completed).ok_or(
                    RegistryBuildError::TopologyInvariant(
                        "intrusive finalization stack contains an invalid entry",
                    ),
                )?;
                self.subtree_ends[completed] = index;
                top = RegistryPathId(preceding);
            }
            if entry.parent != RegistryPathId::ROOT && entry.parent.0 >= index {
                return Err(RegistryBuildError::TopologyInvariant(
                    "entry parent does not precede its child",
                ));
            }
            self.subtree_ends[index] = top.0;
            top = RegistryPathId(index);
        }
        while top != RegistryPathId::ROOT {
            let completed = top.index().ok_or(RegistryBuildError::TopologyInvariant(
                "intrusive finalization stack contains the virtual root",
            ))?;
            let preceding =
                *self
                    .subtree_ends
                    .get(completed)
                    .ok_or(RegistryBuildError::TopologyInvariant(
                        "intrusive finalization stack contains an invalid entry",
                    ))?;
            self.subtree_ends[completed] = self.entries.len();
            top = RegistryPathId(preceding);
        }
        // Each entry slot is overwritten with its predecessor before it can be
        // read as a stack link. A pop closes an entry at a strictly later
        // preorder index, and the EOF drain closes it at `entries.len()`, so
        // every published end is constructively in `(index, entries.len()]`.
        self.finalized = true;
        Ok(())
    }

    fn try_finalized_subtree_ends(&self) -> std::result::Result<&[usize], RegistryBuildError> {
        if !self.is_finalized() {
            return Err(RegistryBuildError::TopologyInvariant(
                "path topology has not been finalized",
            ));
        }
        Ok(&self.subtree_ends)
    }

    fn subtree_ends(&self) -> Option<&[usize]> {
        self.try_finalized_subtree_ends().ok()
    }

    pub(crate) fn subtree_range(&self, id: RegistryPathId) -> Option<std::ops::Range<usize>> {
        let index = id.index()?;
        let end = *self.subtree_ends()?.get(index)?;
        (end > index && end <= self.entries.len()).then_some(index..end)
    }

    fn try_subtree_range(
        &self,
        id: RegistryPathId,
    ) -> std::result::Result<std::ops::Range<usize>, RegistryBuildError> {
        let index = id.index().ok_or(RegistryBuildError::TopologyInvariant(
            "virtual root has no concrete subtree range",
        ))?;
        let end = *self.try_finalized_subtree_ends()?.get(index).ok_or(
            RegistryBuildError::TopologyInvariant("subtree root identifier is out of range"),
        )?;
        if end <= index || end > self.entries.len() {
            return Err(RegistryBuildError::TopologyInvariant(
                "subtree end index is invalid",
            ));
        }
        Ok(index..end)
    }

    fn path_equals_mapped_slice<T, M>(&self, mut id: RegistryPathId, path: &[T], mut map: M) -> bool
    where
        T: Copy,
        U: Eq,
        M: FnMut(T) -> Option<U>,
    {
        if self.depth(id) != Some(path.len()) {
            return false;
        }
        let mut path_cursor = path.len();
        while id != RegistryPathId::ROOT {
            let Some(entry) = self.entry(id) else {
                return false;
            };
            let Some(segment) = self.units.get(entry.segment_start..entry.segment_end) else {
                return false;
            };
            for &unit in segment.iter().rev() {
                let Some(next_cursor) = path_cursor.checked_sub(1) else {
                    return false;
                };
                let Some(mapped) = map(path[next_cursor]) else {
                    return false;
                };
                if unit != mapped {
                    return false;
                }
                path_cursor = next_cursor;
            }
            id = entry.parent;
        }
        path_cursor == 0
    }

    pub(crate) fn path_equals_slice(&self, id: RegistryPathId, path: &[U]) -> bool
    where
        U: Eq,
    {
        self.path_equals_mapped_slice(id, path, Some)
    }

    fn reverse_unit(
        &self,
        path_id: &mut RegistryPathId,
        remaining_in_segment: &mut Option<usize>,
    ) -> std::result::Result<Option<U>, ()> {
        loop {
            if *path_id == RegistryPathId::ROOT {
                return Ok(None);
            }
            let entry = self.entry(*path_id).ok_or(())?;
            let segment = self
                .units
                .get(entry.segment_start..entry.segment_end)
                .ok_or(())?;
            let remaining = remaining_in_segment.get_or_insert(segment.len());
            if *remaining > 0 {
                *remaining -= 1;
                return segment.get(*remaining).copied().map(Some).ok_or(());
            }
            *path_id = entry.parent;
            *remaining_in_segment = None;
        }
    }

    /// Compare two segmented paths exactly without materializing either path.
    ///
    /// The reverse cursors make the comparison independent of how each
    /// checkpoint split its compressed segments. Native call-stack use is
    /// constant and heap use is zero.
    pub(crate) fn path_equals(
        &self,
        left: RegistryPathId,
        other: &Self,
        right: RegistryPathId,
    ) -> bool
    where
        U: Eq,
    {
        if self.depth(left) != other.depth(right) {
            return false;
        }
        let mut left_id = left;
        let mut right_id = right;
        let mut left_remaining = None;
        let mut right_remaining = None;
        loop {
            let Ok(left_unit) = self.reverse_unit(&mut left_id, &mut left_remaining) else {
                return false;
            };
            let Ok(right_unit) = other.reverse_unit(&mut right_id, &mut right_remaining) else {
                return false;
            };
            match (left_unit, right_unit) {
                (Some(left_unit), Some(right_unit)) if left_unit == right_unit => {}
                (None, None) => return true,
                _ => return false,
            }
        }
    }

    fn depth(&self, id: RegistryPathId) -> Option<usize> {
        if id == RegistryPathId::ROOT {
            Some(0)
        } else {
            self.entry(id).map(|entry| entry.depth)
        }
    }

    fn hash(&self, id: RegistryPathId, root_hash: u64) -> Option<u64> {
        if id == RegistryPathId::ROOT {
            Some(root_hash)
        } else {
            self.entry(id).map(|entry| entry.hash)
        }
    }

    fn try_reserve_mapped_path_with_suffix<T, M, H>(
        &mut self,
        parent: RegistryPathId,
        segment: &[T],
        suffix: Option<T>,
        root_hash: u64,
        map_unit: M,
        extend_hash: H,
    ) -> std::result::Result<RegistryPathId, &'static str>
    where
        T: Copy,
        M: FnMut(T) -> std::result::Result<U, &'static str>,
        H: FnMut(u64, U) -> u64,
    {
        self.try_reserve_mapped_path_with_suffix_mode(
            MappedPathReservation {
                parent,
                segment,
                suffix,
                root_hash,
                preserve_finalized_root_sibling: false,
            },
            map_unit,
            extend_hash,
        )
    }

    fn try_reserve_mapped_path_with_suffix_mode<T, M, H>(
        &mut self,
        reservation: MappedPathReservation<'_, T>,
        mut map_unit: M,
        mut extend_hash: H,
    ) -> std::result::Result<RegistryPathId, &'static str>
    where
        T: Copy,
        M: FnMut(T) -> std::result::Result<U, &'static str>,
        H: FnMut(u64, U) -> u64,
    {
        let MappedPathReservation {
            parent,
            segment,
            suffix,
            root_hash,
            preserve_finalized_root_sibling,
        } = reservation;
        let preserve_finalized = preserve_finalized_root_sibling
            && parent == RegistryPathId::ROOT
            && self.is_finalized();
        let segment_len = segment
            .len()
            .checked_add(usize::from(suffix.is_some()))
            .ok_or("eviction registry path segment length overflow")?;
        // A concrete child must advance semantic path depth.  Resident-budget
        // ordering relies on every resident descendant ranking before an
        // equal-score ancestor; admitting an empty child segment would make
        // their depths equal and let the ancestor's earlier preorder id win.
        // The virtual root remains allowed to own an empty concrete root record.
        if segment_len == 0 && parent != RegistryPathId::ROOT {
            return Err("eviction registry concrete child segment is empty");
        }
        let parent_depth = self
            .depth(parent)
            .ok_or("eviction registry path parent is invalid")?;
        let depth = parent_depth
            .checked_add(segment_len)
            .ok_or("eviction registry path depth overflow")?;
        if self.entries.len() == RegistryPathId::ROOT.0 {
            return Err("eviction registry path identifier overflow");
        }
        let parent_hash = self
            .hash(parent, root_hash)
            .ok_or("eviction registry path parent hash is unavailable")?;
        let segment_start = self.units.len();
        let segment_end = segment_start
            .checked_add(segment_len)
            .ok_or("eviction registry path-unit offset overflow")?;

        self.try_reserve_additional(1, segment_len)
            .map_err(|_| "eviction registry path-topology capacity preparation failed")?;

        let mut hash = parent_hash;
        for source_unit in segment.iter().copied().chain(suffix) {
            let unit = match map_unit(source_unit) {
                Ok(unit) => unit,
                Err(error) => {
                    self.units.truncate(segment_start);
                    return Err(error);
                }
            };
            hash = extend_hash(hash, unit);
            self.units.push(unit);
        }
        let id = RegistryPathId(self.entries.len());
        self.entries.push(PathEntry {
            parent,
            segment_start,
            segment_end,
            depth,
            hash,
        });
        self.subtree_ends
            .push(if preserve_finalized { id.0 + 1 } else { 0 });
        self.finalized = preserve_finalized;
        Ok(id)
    }

    fn try_reserve_independent_mapped_path<T, M, H>(
        &mut self,
        segment: &[T],
        root_hash: u64,
        map_unit: M,
        extend_hash: H,
    ) -> std::result::Result<RegistryPathId, &'static str>
    where
        T: Copy,
        M: FnMut(T) -> std::result::Result<U, &'static str>,
        H: FnMut(u64, U) -> u64,
    {
        self.try_reserve_mapped_path_with_suffix_mode(
            MappedPathReservation {
                parent: RegistryPathId::ROOT,
                segment,
                suffix: None,
                root_hash,
                preserve_finalized_root_sibling: true,
            },
            map_unit,
            extend_hash,
        )
    }

    fn try_reserve_independent_path<H>(
        &mut self,
        segment: &[U],
        root_hash: u64,
        extend_hash: H,
    ) -> std::result::Result<RegistryPathId, &'static str>
    where
        H: FnMut(u64, U) -> u64,
    {
        self.try_reserve_independent_mapped_path(
            segment,
            root_hash,
            Ok::<U, &'static str>,
            extend_hash,
        )
    }

    fn try_reserve_mapped_path<T, M, H>(
        &mut self,
        parent: RegistryPathId,
        segment: &[T],
        root_hash: u64,
        map_unit: M,
        extend_hash: H,
    ) -> std::result::Result<RegistryPathId, &'static str>
    where
        T: Copy,
        M: FnMut(T) -> std::result::Result<U, &'static str>,
        H: FnMut(u64, U) -> u64,
    {
        self.try_reserve_mapped_path_with_suffix(
            parent,
            segment,
            None,
            root_hash,
            map_unit,
            extend_hash,
        )
    }

    fn try_reserve_path<H>(
        &mut self,
        parent: RegistryPathId,
        segment: &[U],
        root_hash: u64,
        extend_hash: H,
    ) -> std::result::Result<RegistryPathId, &'static str>
    where
        H: FnMut(u64, U) -> u64,
    {
        self.try_reserve_mapped_path(
            parent,
            segment,
            root_hash,
            Ok::<U, &'static str>,
            extend_hash,
        )
    }

    /// Append a completed source span beneath a newly-reserved destination root.
    ///
    /// Source and destination belong to this same builder. All source indices
    /// precede the destination, and capacity is reserved before the first append,
    /// so source-unit indexing remains valid even if the units vector moves.
    fn try_graft_builder_subtree<H>(
        &mut self,
        source: BuilderSubtreeRange,
        destination_root: RegistryPathId,
        root_hash: u64,
        mut extend_hash: H,
    ) -> std::result::Result<(), RegistryBuildError>
    where
        H: FnMut(u64, U) -> u64,
    {
        let source_root = source
            .root
            .index()
            .ok_or(RegistryBuildError::TopologyInvariant(
                "builder graft source is the virtual root",
            ))?;
        let destination =
            destination_root
                .index()
                .ok_or(RegistryBuildError::DestinationInvariant(
                    "builder graft destination is the virtual root",
                ))?;
        if source.end_exclusive <= source_root
            || source.end_exclusive > destination
            || source.end_exclusive > self.entries.len()
        {
            return Err(RegistryBuildError::TopologyInvariant(
                "builder graft source range is invalid or overlaps its destination",
            ));
        }
        if destination.checked_add(1) != Some(self.entries.len()) {
            return Err(RegistryBuildError::DestinationInvariant(
                "builder graft destination must be the newest topology entry",
            ));
        }

        let source_len =
            source
                .end_exclusive
                .checked_sub(source_root)
                .ok_or(RegistryBuildError::Arithmetic(
                    "builder graft source length",
                ))?;
        let additional_entries =
            source_len
                .checked_sub(1)
                .ok_or(RegistryBuildError::Arithmetic(
                    "builder graft additional entries",
                ))?;
        let mut additional_units = 0usize;
        for source_index in source_root + 1..source.end_exclusive {
            let entry =
                self.entries
                    .get(source_index)
                    .ok_or(RegistryBuildError::TopologyInvariant(
                        "builder graft source entry is unavailable",
                    ))?;
            let parent = entry
                .parent
                .index()
                .ok_or(RegistryBuildError::TopologyInvariant(
                    "builder graft descendant has the virtual root as parent",
                ))?;
            if parent < source_root || parent >= source_index {
                return Err(RegistryBuildError::TopologyInvariant(
                    "builder graft source is not one contiguous preorder subtree",
                ));
            }
            let segment_len = entry.segment_end.checked_sub(entry.segment_start).ok_or(
                RegistryBuildError::Arithmetic("builder graft source segment length"),
            )?;
            if entry.segment_end > self.units.len() {
                return Err(RegistryBuildError::TopologyInvariant(
                    "builder graft source segment is out of range",
                ));
            }
            additional_units =
                additional_units
                    .checked_add(segment_len)
                    .ok_or(RegistryBuildError::Arithmetic(
                        "builder graft path-unit count",
                    ))?;
        }
        self.try_reserve_additional(additional_entries, additional_units)?;
        self.finalized = false;

        for source_index in source_root + 1..source.end_exclusive {
            let source_entry =
                *self
                    .entries
                    .get(source_index)
                    .ok_or(RegistryBuildError::TopologyInvariant(
                        "builder graft source entry disappeared",
                    ))?;
            let source_parent =
                source_entry
                    .parent
                    .index()
                    .ok_or(RegistryBuildError::TopologyInvariant(
                        "builder graft descendant parent disappeared",
                    ))?;
            let parent_offset =
                source_parent
                    .checked_sub(source_root)
                    .ok_or(RegistryBuildError::Arithmetic(
                        "builder graft parent offset",
                    ))?;
            let target_parent = RegistryPathId(destination.checked_add(parent_offset).ok_or(
                RegistryBuildError::Arithmetic("builder graft target parent"),
            )?);
            let parent_depth =
                self.depth(target_parent)
                    .ok_or(RegistryBuildError::DestinationInvariant(
                        "builder graft target parent depth is unavailable",
                    ))?;
            let mut hash = self.hash(target_parent, root_hash).ok_or(
                RegistryBuildError::DestinationInvariant(
                    "builder graft target parent hash is unavailable",
                ),
            )?;
            let segment_len = source_entry
                .segment_end
                .checked_sub(source_entry.segment_start)
                .ok_or(RegistryBuildError::Arithmetic(
                    "builder graft segment length",
                ))?;
            let depth = parent_depth
                .checked_add(segment_len)
                .ok_or(RegistryBuildError::Arithmetic("builder graft target depth"))?;
            let segment_start = self.units.len();
            let segment_end =
                segment_start
                    .checked_add(segment_len)
                    .ok_or(RegistryBuildError::Arithmetic(
                        "builder graft target segment end",
                    ))?;
            for unit_index in source_entry.segment_start..source_entry.segment_end {
                let unit =
                    *self
                        .units
                        .get(unit_index)
                        .ok_or(RegistryBuildError::TopologyInvariant(
                            "builder graft source unit disappeared",
                        ))?;
                hash = extend_hash(hash, unit);
                self.units.push(unit);
            }
            let source_offset =
                source_index
                    .checked_sub(source_root)
                    .ok_or(RegistryBuildError::Arithmetic(
                        "builder graft source offset",
                    ))?;
            let expected_target =
                destination
                    .checked_add(source_offset)
                    .ok_or(RegistryBuildError::Arithmetic(
                        "builder graft target identifier",
                    ))?;
            if self.entries.len() != expected_target {
                return Err(RegistryBuildError::DestinationInvariant(
                    "builder graft target identifiers are not contiguous",
                ));
            }
            self.entries.push(PathEntry {
                parent: target_parent,
                segment_start,
                segment_end,
                depth,
                hash,
            });
            self.subtree_ends.push(0);
        }
        Ok(())
    }

    /// Materialize one segmented path into reusable storage with a single
    /// iterative leaf-to-root walk.
    ///
    /// The mapper must be pure: units are converted in reverse path order and
    /// the completed buffer is then reversed. This lets callers reuse one exact
    /// buffer instead of allocating both an ancestry list and an output path.
    /// Any malformed topology, conversion failure, or allocation failure clears
    /// `out`, so a previous path can never be mistaken for the current one.
    fn materialize_mapped_into<T, M, const N: usize>(
        &self,
        mut id: RegistryPathId,
        out: &mut SmallVec<[T; N]>,
        mut map: M,
    ) -> Option<()>
    where
        M: FnMut(U) -> Option<T>,
        [T; N]: smallvec::Array<Item = T>,
    {
        out.clear();
        let depth = self.depth(id)?;
        out.try_reserve_exact(depth).ok()?;
        while id != RegistryPathId::ROOT {
            let entry = self.entry(id)?;
            for &unit in self
                .units
                .get(entry.segment_start..entry.segment_end)?
                .iter()
                .rev()
            {
                let Some(mapped) = map(unit) else {
                    out.clear();
                    return None;
                };
                out.push(mapped);
            }
            id = entry.parent;
        }
        out.reverse();
        Some(())
    }

    fn materialize_mapped<T, M>(&self, id: RegistryPathId, map: M) -> Option<Vec<T>>
    where
        M: FnMut(U) -> Option<T>,
    {
        let mut path = SmallVec::<[T; 0]>::new();
        self.materialize_mapped_into(id, &mut path, map)?;
        Some(path.into_vec())
    }

    fn materialize(&self, id: RegistryPathId) -> Option<Vec<U>> {
        self.materialize_mapped(id, Some)
    }

    #[cfg(test)]
    fn stored_units(&self) -> usize {
        self.units.len()
    }

    #[cfg(test)]
    fn stored_entries(&self) -> usize {
        self.entries.len()
    }
}

fn try_prepare_dense_slot<T>(
    slots: &mut Vec<Option<T>>,
    path_id: RegistryPathId,
) -> std::result::Result<(), &'static str> {
    if path_id == RegistryPathId::ROOT {
        return Err("eviction registry virtual root cannot hold a node record");
    }
    let required = path_id
        .0
        .checked_add(1)
        .ok_or("eviction registry dense slot index overflow")?;
    if required > slots.len() {
        slots
            .try_reserve(required - slots.len())
            .map_err(|_| "eviction registry dense record allocation failed")?;
        slots.resize_with(required, || None);
    }
    Ok(())
}

fn try_prepare_hash_bucket<K: Copy + Eq + std::hash::Hash>(
    index: &mut HashMap<K, RegistryPathBucket>,
    key: K,
    path_id: RegistryPathId,
    path_id_known_absent: bool,
) -> std::result::Result<Option<RegistryPathBucket>, &'static str> {
    if let Some(bucket) = index.get_mut(&key) {
        if path_id_known_absent || !bucket.contains(&path_id) {
            bucket
                .try_reserve(1)
                .map_err(|_| "eviction registry hash bucket allocation failed")?;
        }
        Ok(None)
    } else {
        index
            .try_reserve(1)
            .map_err(|_| "eviction registry hash index allocation failed")?;
        Ok(Some(SmallVec::new()))
    }
}

fn remove_hash_id<K: Copy + Eq + std::hash::Hash>(
    index: &mut HashMap<K, RegistryPathBucket>,
    key: K,
    path_id: RegistryPathId,
) {
    let remove_bucket = if let Some(bucket) = index.get_mut(&key) {
        if let Some(position) = bucket.iter().position(|&id| id == path_id) {
            bucket.swap_remove(position);
        }
        bucket.is_empty()
    } else {
        false
    };
    if remove_bucket {
        index.remove(&key);
    }
}

fn registry_disk_address(
    pointer: &SwizzledPtr,
) -> std::result::Result<DiskRecordAddress, &'static str> {
    let location = pointer
        .disk_location()
        .ok_or("eviction registry pointer is null, swizzled, transitional, or malformed")?;
    if location.block_id == 0 {
        return Err("eviction registry arena record uses reserved block zero");
    }
    Ok(DiskRecordAddress {
        block_id: location.block_id,
        slot_id: location.offset,
    })
}

/// Per-node IN-MEMORY residual NOT captured by a registered node's on-disk `size_bytes`
/// (which counts only the serialized bytes): the `Arc` control block, the overlay node's
/// `version`/`serial_disk_ptr` atomics + flags, the inline `ChildStore` arrays, and the
/// prefix `Arc` allocation. Added to each node's on-disk size to estimate its RESIDENT
/// footprint. These are the SINGLE source of truth shared by `select_*_for_eviction`'s
/// resident accumulation AND `*_resident_estimate_bytes` — keep them in lockstep so the
/// budget target and the eviction accumulation are in the same unit.
///
/// ANALYTIC PLACEHOLDERS — to be CALIBRATED by the Phase-8 massif bench (the physical
/// witness; see docs/benchmarks/). Byte (`K::Unit = u8`, MAX_PREFIX_LEN 12) and char
/// (`K::Unit = u32`, MAX_PREFIX_LEN 6) differ in the inline-array width; the estimate is
/// approximate (the 2-tier ChildStore makes the true residual count-dependent), so the
/// budget is a soft target with a massif-calibrated safety margin, never an exact bound.
pub const STRUCT_OVERHEAD_BYTE: usize = 128;
pub const STRUCT_OVERHEAD_CHAR: usize = 160;

/// Information about an evictable node.
#[derive(Debug, Clone)]
pub struct EvictableNode {
    /// Path from root to this node (sequence of edge labels).
    pub path: Vec<u8>,
    /// Disk location from last checkpoint.
    pub disk_ptr: SwizzledPtr,
    /// Estimated memory size in bytes.
    pub size_bytes: usize,
    /// Depth in the trie (0 = root children).
    pub depth: usize,
    /// Node type for statistics.
    pub node_type: NodeType,
}

impl EvictableNode {
    /// Create a new evictable node entry.
    pub fn new(
        path: Vec<u8>,
        disk_ptr: SwizzledPtr,
        size_bytes: usize,
        depth: usize,
        node_type: NodeType,
    ) -> Self {
        Self {
            path,
            disk_ptr,
            size_bytes,
            depth,
            node_type,
        }
    }
}

/// Evictable node for char-level tries.
#[derive(Debug, Clone)]
pub struct EvictableCharNode {
    /// Path from root to this node (sequence of char edge labels).
    pub path: Vec<char>,
    /// Disk location from last checkpoint.
    pub disk_ptr: SwizzledPtr,
    /// Estimated memory size in bytes.
    pub size_bytes: usize,
    /// Depth in the trie (0 = root children).
    pub depth: usize,
    /// Node type for statistics.
    pub node_type: NodeType,
}

impl EvictableCharNode {
    /// Create a new evictable char node entry.
    pub fn new(
        path: Vec<char>,
        disk_ptr: SwizzledPtr,
        size_bytes: usize,
        depth: usize,
        node_type: NodeType,
    ) -> Self {
        Self {
            path,
            disk_ptr,
            size_bytes,
            depth,
            node_type,
        }
    }
}

/// Compact production entry. The path hash is an approximate LRU key only;
/// exact correctness identity is the enclosing [`RegistryPathId`]. Public
/// owned records are materialized explicitly, so checkpoint construction and
/// internal selection retain no absolute path vectors or per-entry caches.
#[derive(Clone)]
struct ByteRegistryEntry {
    path_hash: u64,
    disk_ptr: SwizzledPtr,
    size_bytes: usize,
    depth: usize,
    node_type: NodeType,
}

#[derive(Clone)]
struct CharRegistryEntry {
    path_hash: u64,
    disk_ptr: SwizzledPtr,
    size_bytes: usize,
    depth: usize,
    node_type: NodeType,
}

/// Complete admission record for one compact registry occurrence.
///
/// Keeping these fields together prevents byte/character call sites from
/// accidentally permuting parallel scalar arguments. The value is moved on the
/// stack and introduces no allocation.
struct RegistryNodeAdmission {
    path_id: RegistryPathId,
    hash: u64,
    disk_ptr: SwizzledPtr,
    size_bytes: usize,
    depth: usize,
    node_type: NodeType,
}

/// Immutable structural image retained only while a successor checkpoint is
/// being built.
///
/// Structural eligibility is deliberately independent of the source
/// registry's mutable authority state. Only `Valid` authorizes eviction and
/// fault transitions against one exact published root; invalidating it must not
/// discard immutable path and record metadata that can still be matched
/// exactly against unchanged stamped `Arc` subtrees in a later root.
///
/// Path/record tables are `Arc`-backed. Residency is copied fallibly because it
/// is a point-in-time property of each immutable source occurrence and must not
/// observe later eviction/fault transitions. A successor never retains this
/// source after publication, preventing a generation chain.
pub(crate) struct RegistryStructuralSource {
    _generation: RegistryGeneration,
    byte_paths: Arc<PathTopology<u8>>,
    char_paths: Arc<PathTopology<u32>>,
    locations: Arc<Vec<Option<ByteRegistryEntry>>>,
    char_locations: Arc<Vec<Option<CharRegistryEntry>>>,
    byte_disk_index: Arc<HashMap<DiskRecordAddress, RegistryPathBucket>>,
    char_disk_index: Arc<HashMap<DiskRecordAddress, RegistryPathBucket>>,
    byte_residency_bits: Vec<u64>,
    char_residency_bits: Vec<u64>,
}

/// Immutable byte-family inputs for resident-budget scoring. Capturing clones
/// only immutable `Arc` tables and copies already-reserved residency words while
/// the registry read lock is held; sorting and topology scans happen afterwards.
pub(crate) struct ByteRegistrySelectionSnapshot {
    generation: RegistryGeneration,
    topology: Arc<PathTopology<u8>>,
    locations: Arc<Vec<Option<ByteRegistryEntry>>>,
    residency_bits: Vec<u64>,
}

pub(crate) struct CharRegistrySelectionSnapshot {
    generation: RegistryGeneration,
    topology: Arc<PathTopology<u32>>,
    locations: Arc<Vec<Option<CharRegistryEntry>>>,
    residency_bits: Vec<u64>,
}

/// Generation-qualified allocation plan for one coherent structural-source
/// capture. The buffers are reserved without holding the coordinator's
/// registry lock, then filled under a later read lock only if this exact
/// generation and its residency dimensions are still current.
pub(crate) struct RegistryStructuralCapturePlan {
    generation: RegistryGeneration,
    byte_residency_words: usize,
    char_residency_words: usize,
}

impl RegistryStructuralCapturePlan {
    pub(crate) fn try_prepare_buffers(
        &self,
        byte_residency_bits: &mut Vec<u64>,
        char_residency_bits: &mut Vec<u64>,
    ) -> std::result::Result<(), RegistryBuildError> {
        byte_residency_bits.clear();
        char_residency_bits.clear();
        if byte_residency_bits.capacity() < self.byte_residency_words {
            byte_residency_bits
                .try_reserve_exact(self.byte_residency_words)
                .map_err(|_| RegistryBuildError::Allocation("byte structural-source residency"))?;
        }
        if char_residency_bits.capacity() < self.char_residency_words {
            char_residency_bits
                .try_reserve_exact(self.char_residency_words)
                .map_err(|_| RegistryBuildError::Allocation("char structural-source residency"))?;
        }
        Ok(())
    }
}

/// Result of the allocation-free, lock-held half of structural-source capture.
pub(crate) enum RegistryStructuralCapture {
    Ready(RegistryStructuralSource),
    Retry {
        byte_residency_bits: Vec<u64>,
        char_residency_bits: Vec<u64>,
    },
}

impl RegistryStructuralSource {
    #[inline]
    fn byte_is_resident(&self, path: RegistryPathId) -> bool {
        residency_snapshot_contains(&self.byte_residency_bits, path.0)
    }

    #[inline]
    fn char_is_resident(&self, path: RegistryPathId) -> bool {
        residency_snapshot_contains(&self.char_residency_bits, path.0)
    }
}

#[inline]
fn residency_snapshot_contains(bits: &[u64], index: usize) -> bool {
    let (word, mask) = ResidencyState::word_and_mask(index);
    bits.get(word)
        .is_some_and(|resident_word| resident_word & mask != 0)
}

impl ByteRegistrySelectionSnapshot {
    pub(crate) fn select_compact(
        &self,
        target_bytes: usize,
        lru_registry: &LruRegistry,
        min_depth: usize,
        max_count: usize,
        overhead: usize,
    ) -> CompactEvictionBatch<u8> {
        match try_select_compact_batch(
            true,
            &self.topology,
            &self.generation,
            self.locations
                .iter()
                .enumerate()
                .filter_map(|(path_id, entry)| {
                    entry.as_ref().and_then(|node| {
                        (node.depth != 0
                            && node.depth >= min_depth
                            && residency_snapshot_contains(&self.residency_bits, path_id))
                        .then(|| CompactEvictionCandidate {
                            path_id: RegistryPathId(path_id),
                            path_hash: node.path_hash,
                            disk_ptr: node.disk_ptr.clone(),
                            size_bytes: node.size_bytes,
                            depth: node.depth,
                            node_type: node.node_type,
                        })
                    })
                }),
            lru_registry,
            CompactSelectionLimits {
                target_bytes,
                max_count,
                overhead,
            },
        ) {
            Ok(batch) => batch,
            Err(_) => empty_compact_batch(
                &self.topology,
                &self.generation,
                CompactEvictionPolicy::DescendantFirst,
            ),
        }
    }

    pub(crate) fn select_resident_budget(
        &self,
        target_bytes: usize,
        lru_registry: &LruRegistry,
        min_depth: usize,
        max_count: usize,
        overhead: usize,
    ) -> CompactEvictionBatch<u8> {
        let is_resident = |path_id| residency_snapshot_contains(&self.residency_bits, path_id);
        let resident_record = |path_id: usize| {
            if !is_resident(path_id) {
                return Ok(None);
            }
            let node = self
                .locations
                .get(path_id)
                .and_then(Option::as_ref)
                .ok_or(CompactSelectionError::TopologyUnavailable)?;
            let weight = node
                .size_bytes
                .checked_add(overhead)
                .ok_or(CompactSelectionError::SizeOverflow)?;
            Ok(Some((weight, node.path_hash)))
        };
        let materialize_candidate = |path_id: RegistryPathId| {
            let node = self
                .locations
                .get(path_id.0)
                .and_then(Option::as_ref)
                .filter(|_| is_resident(path_id.0))
                .ok_or(CompactSelectionError::TopologyUnavailable)?;
            Ok(CompactEvictionCandidate {
                path_id,
                path_hash: node.path_hash,
                disk_ptr: node.disk_ptr.clone(),
                size_bytes: node.size_bytes,
                depth: node.depth,
                node_type: node.node_type,
            })
        };
        match try_select_resident_budget_batch(
            ResidentBudgetSelectionContext {
                valid: true,
                topology: &self.topology,
                generation: &self.generation,
                lru_registry,
                limits: CompactSelectionLimits {
                    target_bytes,
                    max_count,
                    overhead,
                },
            },
            self.locations
                .iter()
                .enumerate()
                .filter_map(|(path_id, entry)| {
                    entry.as_ref().and_then(|node| {
                        (node.depth != 0 && node.depth >= min_depth && is_resident(path_id))
                            .then_some((RegistryPathId(path_id), node.depth))
                    })
                }),
            resident_record,
            materialize_candidate,
        ) {
            Ok(batch) => batch,
            Err(error) => {
                log::error!("byte resident-budget selection failed closed: {error}");
                empty_compact_batch(
                    &self.topology,
                    &self.generation,
                    CompactEvictionPolicy::ResidentBudgetAncestorClosure,
                )
            }
        }
    }
}

impl CharRegistrySelectionSnapshot {
    pub(crate) fn select_compact(
        &self,
        target_bytes: usize,
        lru_registry: &LruRegistry,
        min_depth: usize,
        max_count: usize,
        overhead: usize,
    ) -> CompactEvictionBatch<u32> {
        match try_select_compact_batch(
            true,
            &self.topology,
            &self.generation,
            self.locations
                .iter()
                .enumerate()
                .filter_map(|(path_id, entry)| {
                    entry.as_ref().and_then(|node| {
                        (node.depth != 0
                            && node.depth >= min_depth
                            && residency_snapshot_contains(&self.residency_bits, path_id))
                        .then(|| CompactEvictionCandidate {
                            path_id: RegistryPathId(path_id),
                            path_hash: node.path_hash,
                            disk_ptr: node.disk_ptr.clone(),
                            size_bytes: node.size_bytes,
                            depth: node.depth,
                            node_type: node.node_type,
                        })
                    })
                }),
            lru_registry,
            CompactSelectionLimits {
                target_bytes,
                max_count,
                overhead,
            },
        ) {
            Ok(batch) => batch,
            Err(_) => empty_compact_batch(
                &self.topology,
                &self.generation,
                CompactEvictionPolicy::DescendantFirst,
            ),
        }
    }

    pub(crate) fn select_resident_budget(
        &self,
        target_bytes: usize,
        lru_registry: &LruRegistry,
        min_depth: usize,
        max_count: usize,
        overhead: usize,
    ) -> CompactEvictionBatch<u32> {
        let is_resident = |path_id| residency_snapshot_contains(&self.residency_bits, path_id);
        let resident_record = |path_id: usize| {
            if !is_resident(path_id) {
                return Ok(None);
            }
            let node = self
                .locations
                .get(path_id)
                .and_then(Option::as_ref)
                .ok_or(CompactSelectionError::TopologyUnavailable)?;
            let weight = node
                .size_bytes
                .checked_add(overhead)
                .ok_or(CompactSelectionError::SizeOverflow)?;
            Ok(Some((weight, node.path_hash)))
        };
        let materialize_candidate = |path_id: RegistryPathId| {
            let node = self
                .locations
                .get(path_id.0)
                .and_then(Option::as_ref)
                .filter(|_| is_resident(path_id.0))
                .ok_or(CompactSelectionError::TopologyUnavailable)?;
            Ok(CompactEvictionCandidate {
                path_id,
                path_hash: node.path_hash,
                disk_ptr: node.disk_ptr.clone(),
                size_bytes: node.size_bytes,
                depth: node.depth,
                node_type: node.node_type,
            })
        };
        match try_select_resident_budget_batch(
            ResidentBudgetSelectionContext {
                valid: true,
                topology: &self.topology,
                generation: &self.generation,
                lru_registry,
                limits: CompactSelectionLimits {
                    target_bytes,
                    max_count,
                    overhead,
                },
            },
            self.locations
                .iter()
                .enumerate()
                .filter_map(|(path_id, entry)| {
                    entry.as_ref().and_then(|node| {
                        (node.depth != 0 && node.depth >= min_depth && is_resident(path_id))
                            .then_some((RegistryPathId(path_id), node.depth))
                    })
                }),
            resident_record,
            materialize_candidate,
        ) {
            Ok(batch) => batch,
            Err(error) => {
                log::error!("char resident-budget selection failed closed: {error}");
                empty_compact_batch(
                    &self.topology,
                    &self.generation,
                    CompactEvictionPolicy::ResidentBudgetAncestorClosure,
                )
            }
        }
    }
}

/// Result of attempting to copy one durable subtree from the prior checkpoint
/// registry into the registry under construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RegistryGraftOutcome {
    /// Exact source metadata was copied without materializing absolute paths.
    Grafted {
        topology_entries: usize,
        durable_records: usize,
    },
    /// The source image was absent, ambiguous, or incomplete. The caller must
    /// scan the durable record graph instead; the destination was not changed.
    FallbackRequired,
}

/// A hard failure while constructing an unpublished registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RegistryBuildError {
    Allocation(&'static str),
    Arithmetic(&'static str),
    TopologyInvariant(&'static str),
    DestinationInvariant(&'static str),
    Registration(&'static str),
}

impl std::fmt::Display for RegistryBuildError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Allocation(component) => {
                write!(
                    formatter,
                    "eviction registry allocation failed for {component}"
                )
            }
            Self::Arithmetic(component) => {
                write!(
                    formatter,
                    "eviction registry arithmetic overflow in {component}"
                )
            }
            Self::TopologyInvariant(reason) => {
                write!(
                    formatter,
                    "eviction registry topology invariant failed: {reason}"
                )
            }
            Self::DestinationInvariant(reason) => {
                write!(
                    formatter,
                    "eviction registry destination invariant failed: {reason}"
                )
            }
            Self::Registration(reason) => {
                write!(
                    formatter,
                    "eviction registry record admission failed: {reason}"
                )
            }
        }
    }
}

impl std::error::Error for RegistryBuildError {}

#[derive(Debug, Clone, Copy)]
struct GraftRecordMeta {
    disk_address: DiskRecordAddress,
    disk_raw: u64,
    size_bytes: usize,
    depth: usize,
    node_type: NodeType,
}

#[derive(Debug, Clone)]
struct RegistryGraftPlan {
    source_root: RegistryPathId,
    source_range: std::ops::Range<usize>,
    additional_units: usize,
    durable_records: usize,
}

struct RegistryGraftSource<'a, U, E> {
    topology: &'a PathTopology<U>,
    locations: &'a [Option<E>],
    disk_index: &'a HashMap<DiskRecordAddress, RegistryPathBucket>,
}

struct RegistryGraftDestination<'a, U> {
    topology: &'a PathTopology<U>,
    root: RegistryPathId,
    disk_ptr: &'a SwizzledPtr,
}

fn try_plan_registry_graft<U, E, M, F>(
    source: RegistryGraftSource<'_, U, E>,
    destination: RegistryGraftDestination<'_, U>,
    mut record_meta: M,
    valid_family: F,
) -> std::result::Result<Option<RegistryGraftPlan>, RegistryBuildError>
where
    U: Copy + Eq,
    M: FnMut(&E) -> GraftRecordMeta,
    F: Fn(NodeType) -> bool,
{
    let RegistryGraftSource {
        topology: source_topology,
        locations: source_locations,
        disk_index: source_disk_index,
    } = source;
    let RegistryGraftDestination {
        topology: destination_topology,
        root: destination_root,
        disk_ptr,
    } = destination;
    let Some(destination_index) = destination_root.index() else {
        return Err(RegistryBuildError::DestinationInvariant(
            "virtual root cannot receive a durable subtree",
        ));
    };
    if destination_index.checked_add(1) != Some(destination_topology.len()) {
        return Err(RegistryBuildError::DestinationInvariant(
            "graft root must be the newest topology entry",
        ));
    }
    let Some(requested_location) = disk_ptr.disk_location() else {
        return Ok(None);
    };
    if requested_location.block_id == 0 {
        return Ok(None);
    }

    let disk_address = DiskRecordAddress {
        block_id: requested_location.block_id,
        slot_id: requested_location.offset,
    };
    let Some(bucket) = source_disk_index.get(&disk_address) else {
        return Ok(None);
    };
    let mut source_root = None;
    for &candidate in bucket {
        let Some(entry) = source_locations.get(candidate.0).and_then(Option::as_ref) else {
            return Ok(None);
        };
        let meta = record_meta(entry);
        if meta.disk_address != disk_address
            || !source_topology.path_equals(candidate, destination_topology, destination_root)
        {
            continue;
        }
        if source_root.replace(candidate).is_some() {
            return Ok(None);
        }
    }
    let Some(source_root) = source_root else {
        return Ok(None);
    };
    let source_range = match source_topology.try_subtree_range(source_root) {
        Ok(range) => range,
        Err(RegistryBuildError::TopologyInvariant(_)) => return Ok(None),
        Err(error) => return Err(error),
    };
    if source_range.start != source_root.0 || source_range.is_empty() {
        return Ok(None);
    }

    let mut direct_children = Vec::new();
    direct_children
        .try_reserve_exact(source_range.len())
        .map_err(|_| RegistryBuildError::Allocation("graft child counts"))?;
    direct_children.resize(source_range.len(), 0usize);

    let subtree_ends = source_topology.try_finalized_subtree_ends()?;
    let mut active_disk_records = HashSet::new();
    active_disk_records
        .try_reserve(source_range.len())
        .map_err(|_| RegistryBuildError::Allocation("graft active ancestry"))?;
    let mut active_exits = Vec::new();
    active_exits
        .try_reserve(source_range.len())
        .map_err(|_| RegistryBuildError::Allocation("graft ancestry exits"))?;
    let mut record_ids = Vec::new();
    record_ids
        .try_reserve(source_range.len())
        .map_err(|_| RegistryBuildError::Allocation("graft record identities"))?;
    let mut relevant_disk_records = HashSet::new();
    relevant_disk_records
        .try_reserve(source_range.len())
        .map_err(|_| RegistryBuildError::Allocation("graft disk identities"))?;

    let mut additional_units = 0usize;
    let mut durable_records = 0usize;
    for source_index in source_range.clone() {
        while active_exits
            .last()
            .is_some_and(|(subtree_end, _)| *subtree_end <= source_index)
        {
            let (_, disk_address) =
                active_exits
                    .pop()
                    .ok_or(RegistryBuildError::TopologyInvariant(
                        "graft ancestry exit stack became inconsistent",
                    ))?;
            if !active_disk_records.remove(&disk_address) {
                return Ok(None);
            }
        }
        let source_id = RegistryPathId(source_index);
        let Some(path_entry) = source_topology.entry(source_id) else {
            return Ok(None);
        };
        let Some(segment) = source_topology.segment(source_id) else {
            return Ok(None);
        };
        let parent_depth = if source_id == source_root {
            source_topology.depth(path_entry.parent)
        } else {
            let Some(parent_index) = path_entry.parent.index() else {
                return Ok(None);
            };
            if parent_index < source_range.start || parent_index >= source_index {
                return Ok(None);
            }
            let child_count_index = parent_index - source_range.start;
            direct_children[child_count_index] = direct_children[child_count_index]
                .checked_add(1)
                .ok_or(RegistryBuildError::Arithmetic("graft child counts"))?;
            source_topology.depth(path_entry.parent)
        };
        let Some(expected_depth) = parent_depth.and_then(|depth| depth.checked_add(segment.len()))
        else {
            return Ok(None);
        };
        if expected_depth != path_entry.depth {
            return Ok(None);
        }
        if source_id != source_root {
            additional_units = additional_units
                .checked_add(segment.len())
                .ok_or(RegistryBuildError::Arithmetic("graft path units"))?;
        }

        if let Some(entry) = source_locations.get(source_index).and_then(Option::as_ref) {
            let meta = record_meta(entry);
            let Some(location) = SwizzledPtr::from_raw(meta.disk_raw).disk_location() else {
                return Ok(None);
            };
            if meta.disk_address.block_id == 0
                || location.block_id != meta.disk_address.block_id
                || location.offset != meta.disk_address.slot_id
                || location.node_type != meta.node_type
                || !valid_family(meta.node_type)
                || meta.size_bytes == 0
                || meta.depth != path_entry.depth
            {
                return Ok(None);
            }
            if !active_disk_records.insert(meta.disk_address) {
                // The same durable record on an ancestor and descendant path is
                // a cycle. A later sibling occurrence is accepted after the
                // first occurrence's subtree exit removes it from this set.
                return Ok(None);
            }
            let subtree_end =
                *subtree_ends
                    .get(source_index)
                    .ok_or(RegistryBuildError::TopologyInvariant(
                        "graft record has no finalized subtree end",
                    ))?;
            if subtree_end <= source_index || subtree_end > source_range.end {
                return Ok(None);
            }
            active_exits.push((subtree_end, meta.disk_address));
            record_ids.push((source_id, meta.disk_address));
            relevant_disk_records.insert(meta.disk_address);
            durable_records = durable_records
                .checked_add(1)
                .ok_or(RegistryBuildError::Arithmetic("graft record count"))?;
        }
    }

    let mut indexed_records = HashSet::new();
    indexed_records
        .try_reserve(record_ids.len())
        .map_err(|_| RegistryBuildError::Allocation("graft indexed identities"))?;
    for disk_address in relevant_disk_records {
        let Some(bucket) = source_disk_index.get(&disk_address) else {
            return Ok(None);
        };
        for &path_id in bucket {
            if source_locations
                .get(path_id.0)
                .and_then(Option::as_ref)
                .is_none()
            {
                return Ok(None);
            }
            if !indexed_records.insert((disk_address, path_id)) {
                return Ok(None);
            }
        }
    }
    if record_ids
        .iter()
        .any(|&(path_id, disk_address)| !indexed_records.contains(&(disk_address, path_id)))
    {
        return Ok(None);
    }

    let Some(root_entry) = source_locations.get(source_root.0).and_then(Option::as_ref) else {
        return Ok(None);
    };
    let root_meta = record_meta(root_entry);
    if root_meta.disk_address != disk_address || root_meta.node_type != requested_location.node_type
    {
        return Ok(None);
    }
    for source_index in source_range.clone() {
        if source_locations
            .get(source_index)
            .and_then(Option::as_ref)
            .is_none()
        {
            let child_count = direct_children[source_index - source_range.start];
            if source_index == source_root.0 || child_count < 2 {
                return Ok(None);
            }
        }
    }

    Ok(Some(RegistryGraftPlan {
        source_root,
        source_range,
        additional_units,
        durable_records,
    }))
}

/// Exact internal eviction candidate. No absolute path allocation is retained
/// or produced during selection.
#[derive(Clone)]
pub(crate) struct CompactEvictionCandidate {
    pub(crate) path_id: RegistryPathId,
    pub(crate) path_hash: u64,
    pub(crate) disk_ptr: SwizzledPtr,
    pub(crate) size_bytes: usize,
    pub(crate) depth: usize,
    pub(crate) node_type: NodeType,
}

type CompactCandidateBuffer = SmallVec<[CompactEvictionCandidate; 1]>;

struct ScoredCompactCandidate {
    candidate: CompactEvictionCandidate,
    coldness: u64,
}

type ScoredCompactCandidateBuffer = SmallVec<[ScoredCompactCandidate; 1]>;

struct ResidentRankedAnchor {
    path_id: RegistryPathId,
    depth: usize,
    coldness: u64,
    closure_gain: usize,
}

type ResidentRankedAnchorBuffer = SmallVec<[ResidentRankedAnchor; 1]>;

/// Structural replacement order for one compact batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompactEvictionPolicy {
    /// Preserve the general/manual eviction contract: deepest valid selected
    /// endpoints win and suppress selected ancestors.
    DescendantFirst,
    /// Checkpoint-tail resident-budget contract: a valid selected ancestor
    /// replaces its complete durable subtree and suppresses selected descendants;
    /// a stale ancestor falls through to valid selected descendants.
    ResidentBudgetAncestorClosure,
}

/// Exact selection-side accounting for a compact batch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CompactSelectionReport {
    /// Exact resident weight of the selected subtree union in the captured
    /// authoritative registry.
    pub(crate) planned_bytes: usize,
    /// Resident candidates satisfying the configured minimum depth.
    pub(crate) eligible_candidates: usize,
    /// Eligible anchors whose subtree contributes new resident coverage after
    /// colder-ranked closures are accounted for.
    pub(crate) nonredundant_candidates: usize,
    /// Nonredundant LRU-ranked anchors admitted to this batch.
    pub(crate) selected_priority_count: usize,
    /// The target was not reached because additional nonredundant anchors were
    /// excluded by the configured anchor cap.
    pub(crate) cap_exhausted: bool,
    /// Every eligible subtree closure was admitted without reaching the target;
    /// remaining residency is structurally pinned by the minimum depth/root.
    pub(crate) eligible_exhausted: bool,
}

/// Selected candidates plus the immutable topology generation that gives their
/// IDs meaning. Holding the topology `Arc` keeps those identifiers meaningful if
/// a later checkpoint atomically replaces the coordinator's registry; it does
/// **not** by itself lease the storage address space named by `disk_ptr`.
///
/// The storage lifetime is instead established at the operation boundary:
/// asynchronous callbacks upgrade and retain the trie's `Weak` handle before
/// touching a batch, synchronous callbacks borrow the trie, and in-place
/// compaction joins the eviction worker before releasing or replacing the
/// buffer/arena managers. This avoids a type-erased storage lease on every batch
/// and adds no reference-count traffic to the eviction hot path.
pub(crate) struct CompactEvictionBatch<U> {
    pub(crate) topology: Arc<PathTopology<U>>,
    pub(crate) generation: RegistryGeneration,
    pub(crate) candidates: CompactCandidateBuffer,
    pub(crate) policy: CompactEvictionPolicy,
    pub(crate) report: CompactSelectionReport,
}

impl<U: Copy> CompactEvictionBatch<U> {
    pub(crate) fn materialize_path(&self, path_id: RegistryPathId) -> Option<Vec<U>> {
        self.topology.materialize(path_id)
    }

    /// Materialize an exact path into a caller-owned PDA frame buffer.
    /// `map` must be pure because [`PathTopology::materialize_mapped_into`]
    /// performs conversion leaf-first before reversing the completed buffer.
    pub(crate) fn materialize_path_mapped_into<T, M, const N: usize>(
        &self,
        path_id: RegistryPathId,
        out: &mut SmallVec<[T; N]>,
        map: M,
    ) -> Option<()>
    where
        M: FnMut(U) -> Option<T>,
        [T; N]: smallvec::Array<Item = T>,
    {
        self.topology.materialize_mapped_into(path_id, out, map)
    }
}

impl CompactEvictionBatch<u32> {
    pub(crate) fn materialize_char_path(&self, path_id: RegistryPathId) -> Option<Vec<char>> {
        self.topology.materialize_mapped(path_id, char::from_u32)
    }

    /// Retain candidates whose exact character paths satisfy `predicate`.
    /// One inline buffer is reused across the complete batch; paths deeper than
    /// the inline capacity spill at most as needed and reuse that capacity for
    /// every later candidate.
    #[cfg(feature = "bench-internals")]
    pub(crate) fn retain_char_paths<F>(&mut self, mut predicate: F)
    where
        F: FnMut(&[char]) -> bool,
    {
        let topology = &self.topology;
        let mut path = SmallVec::<[char; 16]>::new();
        let maximum_depth = self
            .candidates
            .iter()
            .map(|candidate| candidate.depth)
            .max()
            .unwrap_or(0);
        if path.try_reserve_exact(maximum_depth).is_err() {
            self.candidates.clear();
            return;
        }
        self.candidates.retain(|candidate| {
            topology
                .materialize_mapped_into(candidate.path_id, &mut path, char::from_u32)
                .is_some_and(|()| predicate(&path))
        });
    }
}

pub(crate) struct PreparedSparseResidency {
    catalog: Arc<PublishedRegistryCatalog>,
    predecessor_ordinal: u32,
    target_ordinal: u32,
    resident_nodes: usize,
    resident_serialized_bytes: usize,
    delta: PackedResidencyDelta,
}

pub(crate) struct PreparedRebasedResidency {
    predecessor_catalog: Arc<PublishedRegistryCatalog>,
    catalog: Arc<PublishedRegistryCatalog>,
    resident_nodes: usize,
    resident_serialized_bytes: usize,
    delta: PackedResidencyDelta,
}

pub(crate) enum PreparedPackedResidency {
    /// Ordinary sparse successor within one retained materialization array.
    Sparse(PreparedSparseResidency),
    /// Total ordinal rollover. The fresh catalog already contains the delta,
    /// and the root CAS publishes its disjoint arrays at settled ordinal zero.
    Rebased(PreparedRebasedResidency),
}

impl PreparedPackedResidency {
    fn sparse(
        catalog: Arc<PublishedRegistryCatalog>,
        predecessor_ordinal: u32,
        target_ordinal: u32,
        resident_nodes: usize,
        resident_serialized_bytes: usize,
        delta: PackedResidencyDelta,
    ) -> Self {
        Self::Sparse(PreparedSparseResidency {
            catalog,
            predecessor_ordinal,
            target_ordinal,
            resident_nodes,
            resident_serialized_bytes,
            delta,
        })
    }

    fn rebased(
        predecessor_catalog: Arc<PublishedRegistryCatalog>,
        catalog: Arc<PublishedRegistryCatalog>,
        resident_nodes: usize,
        resident_serialized_bytes: usize,
        delta: PackedResidencyDelta,
    ) -> Self {
        Self::Rebased(PreparedRebasedResidency {
            predecessor_catalog,
            catalog,
            resident_nodes,
            resident_serialized_bytes,
            delta,
        })
    }

    pub(crate) fn into_root_parts(
        self,
        expected_catalog: &Arc<PublishedRegistryCatalog>,
        expected_ordinal: u32,
    ) -> Option<(
        Arc<PublishedRegistryCatalog>,
        u32,
        u32,
        usize,
        usize,
        PackedResidencyDelta,
    )> {
        match self {
            Self::Sparse(prepared) => {
                if !Arc::ptr_eq(&prepared.catalog, expected_catalog)
                    || prepared.predecessor_ordinal != expected_ordinal
                {
                    return None;
                }
                Some((
                    prepared.catalog,
                    prepared.predecessor_ordinal,
                    prepared.target_ordinal,
                    prepared.resident_nodes,
                    prepared.resident_serialized_bytes,
                    prepared.delta,
                ))
            }
            Self::Rebased(prepared) => {
                if expected_ordinal != u32::MAX
                    || !Arc::ptr_eq(&prepared.predecessor_catalog, expected_catalog)
                    || Arc::ptr_eq(&prepared.catalog, expected_catalog)
                    || !prepared
                        .catalog
                        .binding()
                        .same_publication(expected_catalog.binding())
                    || prepared.catalog.byte_residency().frontier() != 0
                    || prepared.catalog.char_residency().frontier() != 0
                {
                    return None;
                }
                Some((
                    prepared.catalog,
                    0,
                    0,
                    prepared.resident_nodes,
                    prepared.resident_serialized_bytes,
                    prepared.delta,
                ))
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EvictionCommitError {
    CandidateUnavailable,
    TopologyUnavailable,
    Allocation,
    Arithmetic,
    RegistryInvariant,
}

impl std::fmt::Display for EvictionCommitError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::CandidateUnavailable => {
                "eviction candidate has no exact record in the published registry"
            }
            Self::TopologyUnavailable => "eviction registry topology is malformed",
            Self::Allocation => "eviction commit preparation allocation failed",
            Self::Arithmetic => "eviction commit accounting overflowed",
            Self::RegistryInvariant => "eviction registry residency invariant is inconsistent",
        })
    }
}

#[inline]
fn map_residency_prepare_error(
    error: super::atomic_residency::ResidencyPrepareError,
) -> EvictionCommitError {
    match error {
        super::atomic_residency::ResidencyPrepareError::Allocation => {
            EvictionCommitError::Allocation
        }
        _ => EvictionCommitError::CandidateUnavailable,
    }
}

const RESIDENCY_WORD_BITS: usize = u64::BITS as usize;

#[derive(Default)]
struct ResidencyState {
    bits: Vec<u64>,
    word_serialized_bytes: Vec<usize>,
    resident_nodes: usize,
    resident_serialized_bytes: usize,
}

struct PreparedResidencyMark {
    word: usize,
    mask: u64,
    new_word_serialized_bytes: usize,
    new_resident_nodes: usize,
    new_resident_serialized_bytes: usize,
}

struct PreparedResidencyClear {
    word: usize,
    mask: u64,
    new_word_serialized_bytes: usize,
    new_resident_nodes: usize,
    new_resident_serialized_bytes: usize,
}

impl ResidencyState {
    fn word_and_mask(index: usize) -> (usize, u64) {
        let word = index / RESIDENCY_WORD_BITS;
        let bit = index % RESIDENCY_WORD_BITS;
        (word, 1u64 << bit)
    }

    fn ensure_word(&mut self, word: usize) -> std::result::Result<(), &'static str> {
        let required = word
            .checked_add(1)
            .ok_or("eviction residency word-count overflow")?;
        if self.bits.len() >= required {
            return Ok(());
        }
        let additional = required - self.bits.len();
        self.bits
            .try_reserve_exact(additional)
            .map_err(|_| "eviction residency bitset allocation failed")?;
        self.word_serialized_bytes
            .try_reserve_exact(additional)
            .map_err(|_| "eviction residency byte-index allocation failed")?;
        self.bits.resize(required, 0);
        self.word_serialized_bytes.resize(required, 0);
        Ok(())
    }

    fn prepare_mark(
        &mut self,
        index: usize,
        old_serialized_bytes: Option<usize>,
        new_serialized_bytes: usize,
    ) -> std::result::Result<PreparedResidencyMark, &'static str> {
        let (word, mask) = Self::word_and_mask(index);
        self.ensure_word(word)?;
        let was_resident = self.bits[word] & mask != 0;
        let old_bytes = if was_resident {
            old_serialized_bytes
                .ok_or("resident registry entry has no prior serialized byte record")?
        } else {
            0
        };
        let new_word_serialized_bytes = self.word_serialized_bytes[word]
            .checked_sub(old_bytes)
            .and_then(|bytes| bytes.checked_add(new_serialized_bytes))
            .ok_or("eviction residency word-byte accounting overflow or invariant violation")?;
        let new_resident_nodes = if was_resident {
            self.resident_nodes
        } else {
            self.resident_nodes
                .checked_add(1)
                .ok_or("eviction resident-node count overflow")?
        };
        let new_resident_serialized_bytes = self
            .resident_serialized_bytes
            .checked_sub(old_bytes)
            .and_then(|bytes| bytes.checked_add(new_serialized_bytes))
            .ok_or("eviction resident-byte accounting overflow or invariant violation")?;
        Ok(PreparedResidencyMark {
            word,
            mask,
            new_word_serialized_bytes,
            new_resident_nodes,
            new_resident_serialized_bytes,
        })
    }

    fn commit_mark(&mut self, prepared: PreparedResidencyMark) {
        self.bits[prepared.word] |= prepared.mask;
        self.word_serialized_bytes[prepared.word] = prepared.new_word_serialized_bytes;
        self.resident_nodes = prepared.new_resident_nodes;
        self.resident_serialized_bytes = prepared.new_resident_serialized_bytes;
    }

    #[cfg(test)]
    fn try_mark_existing(
        &mut self,
        index: usize,
        serialized_bytes: usize,
    ) -> std::result::Result<bool, &'static str> {
        let was_resident = self.is_resident(index);
        let prepared = self.prepare_mark(index, Some(serialized_bytes), serialized_bytes)?;
        self.commit_mark(prepared);
        Ok(!was_resident)
    }

    #[cfg(test)]
    fn try_clear_existing(
        &mut self,
        index: usize,
        serialized_bytes: usize,
    ) -> std::result::Result<bool, &'static str> {
        let Some(prepared) = self.prepare_clear(index, serialized_bytes)? else {
            return Ok(false);
        };
        self.commit_clear(prepared);
        Ok(true)
    }

    fn prepare_clear(
        &self,
        index: usize,
        serialized_bytes: usize,
    ) -> std::result::Result<Option<PreparedResidencyClear>, &'static str> {
        let (word, mask) = Self::word_and_mask(index);
        let Some(bits) = self.bits.get(word) else {
            return Err("eviction residency path identifier is out of range");
        };
        if *bits & mask == 0 {
            return Ok(None);
        }
        let word_bytes = self
            .word_serialized_bytes
            .get(word)
            .ok_or("eviction residency byte-index is out of range")?;
        let new_word_serialized_bytes = word_bytes
            .checked_sub(serialized_bytes)
            .ok_or("eviction residency word-byte accounting underflow")?;
        let new_resident_nodes = self
            .resident_nodes
            .checked_sub(1)
            .ok_or("eviction resident-node count underflow")?;
        let new_resident_serialized_bytes = self
            .resident_serialized_bytes
            .checked_sub(serialized_bytes)
            .ok_or("eviction resident-byte accounting underflow")?;
        Ok(Some(PreparedResidencyClear {
            word,
            mask,
            new_word_serialized_bytes,
            new_resident_nodes,
            new_resident_serialized_bytes,
        }))
    }

    fn commit_clear(&mut self, prepared: PreparedResidencyClear) {
        self.bits[prepared.word] &= !prepared.mask;
        self.word_serialized_bytes[prepared.word] = prepared.new_word_serialized_bytes;
        self.resident_nodes = prepared.new_resident_nodes;
        self.resident_serialized_bytes = prepared.new_resident_serialized_bytes;
    }

    fn is_resident(&self, index: usize) -> bool {
        let (word, mask) = Self::word_and_mask(index);
        self.bits
            .get(word)
            .is_some_and(|resident_word| resident_word & mask != 0)
    }

    fn resident_nodes(&self) -> usize {
        self.resident_nodes
    }

    fn resident_serialized_bytes(&self) -> usize {
        self.resident_serialized_bytes
    }

    fn clear(&mut self) {
        self.bits.clear();
        self.word_serialized_bytes.clear();
        self.resident_nodes = 0;
        self.resident_serialized_bytes = 0;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompactSelectionError {
    TopologyUnavailable,
    CandidateAllocation,
    SizeOverflow,
}

#[derive(Clone, Copy)]
struct CompactSelectionLimits {
    target_bytes: usize,
    max_count: usize,
    overhead: usize,
}

impl std::fmt::Display for CompactSelectionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::TopologyUnavailable => {
                "eviction registry path topology is malformed or unavailable"
            }
            Self::CandidateAllocation => "eviction candidate allocation failed",
            Self::SizeOverflow => "eviction candidate byte accounting overflowed",
        })
    }
}

fn empty_compact_batch<U: Copy>(
    topology: &Arc<PathTopology<U>>,
    generation: &RegistryGeneration,
    policy: CompactEvictionPolicy,
) -> CompactEvictionBatch<U> {
    CompactEvictionBatch {
        topology: Arc::clone(topology),
        generation: generation.clone(),
        candidates: SmallVec::new(),
        policy,
        report: CompactSelectionReport::default(),
    }
}

fn try_select_compact_batch<U, I>(
    valid: bool,
    topology: &Arc<PathTopology<U>>,
    generation: &RegistryGeneration,
    candidate_iter: I,
    lru_registry: &LruRegistry,
    limits: CompactSelectionLimits,
) -> std::result::Result<CompactEvictionBatch<U>, CompactSelectionError>
where
    U: Copy,
    I: Iterator<Item = CompactEvictionCandidate>,
{
    let CompactSelectionLimits {
        target_bytes,
        max_count,
        overhead,
    } = limits;
    if !valid || target_bytes == 0 || max_count == 0 {
        return Ok(empty_compact_batch(
            topology,
            generation,
            CompactEvictionPolicy::DescendantFirst,
        ));
    }
    if topology.subtree_ends().is_none() {
        return Err(CompactSelectionError::TopologyUnavailable);
    }

    let mut scored = ScoredCompactCandidateBuffer::new();
    if let Some(upper_bound) = candidate_iter.size_hint().1 {
        scored
            .try_reserve_exact(upper_bound)
            .map_err(|_| CompactSelectionError::CandidateAllocation)?;
    }
    let selection_time = lru_registry.selection_time_us();
    for candidate in candidate_iter {
        let coldness = lru_registry.coldness_score_hash_at(candidate.path_hash, selection_time);
        scored.push(ScoredCompactCandidate {
            candidate,
            coldness,
        });
    }
    retain_coldest_prefix(&mut scored, max_count);

    let mut candidates = CompactCandidateBuffer::new();
    candidates
        .try_reserve_exact(scored.len())
        .map_err(|_| CompactSelectionError::CandidateAllocation)?;
    let mut total_bytes = 0usize;
    for scored_candidate in scored {
        let candidate = scored_candidate.candidate;
        let candidate_bytes = candidate
            .size_bytes
            .checked_add(overhead)
            .ok_or(CompactSelectionError::SizeOverflow)?;
        total_bytes = total_bytes
            .checked_add(candidate_bytes)
            .ok_or(CompactSelectionError::SizeOverflow)?;
        candidates.push(candidate);
        if total_bytes >= target_bytes {
            break;
        }
    }
    Ok(CompactEvictionBatch {
        topology: Arc::clone(topology),
        generation: generation.clone(),
        candidates,
        policy: CompactEvictionPolicy::DescendantFirst,
        report: CompactSelectionReport::default(),
    })
}

/// Select a minimal cold-priority prefix whose exact laminar subtree closure
/// reaches a resident-byte target.
///
/// `resident_record(index)` returns `(resident_weight, path_hash)` for a durable
/// record that is currently resident, or `None` for a structural/nonresident
/// topology entry. The topology is preorder, so one reverse pass computes each
/// possible anchor's warmest resident descendant and one forward pass propagates
/// the earliest admitted ancestor rank. Both passes use explicit dense vectors;
/// native-stack use is constant at arbitrary trie depth.
struct ResidentBudgetSelectionContext<'a, U> {
    valid: bool,
    topology: &'a Arc<PathTopology<U>>,
    generation: &'a RegistryGeneration,
    lru_registry: &'a LruRegistry,
    limits: CompactSelectionLimits,
}

fn try_select_resident_budget_batch<U, I, R, M>(
    context: ResidentBudgetSelectionContext<'_, U>,
    candidate_iter: I,
    mut resident_record: R,
    mut materialize_candidate: M,
) -> std::result::Result<CompactEvictionBatch<U>, CompactSelectionError>
where
    U: Copy,
    I: Iterator<Item = (RegistryPathId, usize)>,
    R: FnMut(usize) -> std::result::Result<Option<(usize, u64)>, CompactSelectionError>,
    M: FnMut(
        RegistryPathId,
    ) -> std::result::Result<CompactEvictionCandidate, CompactSelectionError>,
{
    let ResidentBudgetSelectionContext {
        valid,
        topology,
        generation,
        lru_registry,
        limits,
    } = context;
    let CompactSelectionLimits {
        target_bytes,
        max_count,
        overhead: _,
    } = limits;
    if !valid || target_bytes == 0 {
        return Ok(empty_compact_batch(
            topology,
            generation,
            CompactEvictionPolicy::ResidentBudgetAncestorClosure,
        ));
    }
    if topology.subtree_ends().is_none() {
        return Err(CompactSelectionError::TopologyUnavailable);
    }

    // A subtree can be evicted only as a unit, so rank an anchor by its
    // warmest resident descendant rather than by the anchor's local access
    // history. This prevents a cold/untracked prefix from bypassing a recently
    // touched descendant. The selection instant is frozen across the pass.
    let topology_len = topology.len();
    let mut subtree_coldness = Vec::new();
    subtree_coldness
        .try_reserve_exact(topology_len)
        .map_err(|_| CompactSelectionError::CandidateAllocation)?;
    subtree_coldness.resize(topology_len, u64::MAX);
    let selection_time = lru_registry.selection_time_us();
    for (index, score) in subtree_coldness.iter_mut().enumerate() {
        if let Some((_, path_hash)) = resident_record(index)? {
            *score = lru_registry.coldness_score_hash_at(path_hash, selection_time);
        }
    }
    for index in (0..topology_len).rev() {
        let parent = topology
            .parent(RegistryPathId(index))
            .ok_or(CompactSelectionError::TopologyUnavailable)?;
        if let Some(parent_index) = parent.index() {
            if parent_index >= index {
                return Err(CompactSelectionError::TopologyUnavailable);
            }
            subtree_coldness[parent_index] =
                subtree_coldness[parent_index].min(subtree_coldness[index]);
        }
    }

    let mut scored = ResidentRankedAnchorBuffer::new();
    if let Some(upper_bound) = candidate_iter.size_hint().1 {
        scored
            .try_reserve_exact(upper_bound)
            .map_err(|_| CompactSelectionError::CandidateAllocation)?;
    }
    for (path_id, depth) in candidate_iter {
        let path_index = path_id
            .index()
            .filter(|&index| index < topology_len)
            .ok_or(CompactSelectionError::TopologyUnavailable)?;
        scored.push(ResidentRankedAnchor {
            path_id,
            depth,
            coldness: subtree_coldness[path_index],
            closure_gain: 0,
        });
    }
    drop(subtree_coldness);
    scored.sort_unstable_by_key(|anchor| {
        (
            Reverse(anchor.coldness),
            Reverse(anchor.depth),
            anchor.path_id.0,
        )
    });

    // Map every eligible direct anchor to its cold rank, then overwrite each
    // topology slot with the minimum rank on its root-to-node path. A resident
    // record contributes exactly once, to the earliest-ranked selected ancestor
    // that structurally covers it.
    let mut effective_rank = Vec::new();
    effective_rank
        .try_reserve_exact(topology_len)
        .map_err(|_| CompactSelectionError::CandidateAllocation)?;
    effective_rank.resize(topology_len, usize::MAX);
    for (rank, scored_candidate) in scored.iter().enumerate() {
        let path_index = scored_candidate
            .path_id
            .index()
            .filter(|&index| index < topology_len)
            .ok_or(CompactSelectionError::TopologyUnavailable)?;
        if effective_rank[path_index] != usize::MAX {
            return Err(CompactSelectionError::TopologyUnavailable);
        }
        effective_rank[path_index] = rank;
    }
    for index in 0..topology_len {
        let parent = topology
            .parent(RegistryPathId(index))
            .ok_or(CompactSelectionError::TopologyUnavailable)?;
        let inherited = if let Some(parent_index) = parent.index() {
            if parent_index >= index {
                return Err(CompactSelectionError::TopologyUnavailable);
            }
            effective_rank[parent_index]
        } else {
            usize::MAX
        };
        let rank = effective_rank[index].min(inherited);
        effective_rank[index] = rank;
        if rank == usize::MAX {
            continue;
        }
        if let Some((resident_weight, _)) = resident_record(index)? {
            scored[rank].closure_gain = scored[rank]
                .closure_gain
                .checked_add(resident_weight)
                .ok_or(CompactSelectionError::SizeOverflow)?;
        }
    }

    let eligible_candidates = scored.len();
    let nonredundant_candidates = scored
        .iter()
        .filter(|candidate| candidate.closure_gain != 0)
        .count();
    let mut candidates = CompactCandidateBuffer::new();
    candidates
        .try_reserve_exact(max_count.min(nonredundant_candidates))
        .map_err(|_| CompactSelectionError::CandidateAllocation)?;
    let mut planned_bytes = 0usize;
    for scored_candidate in scored {
        if scored_candidate.closure_gain == 0 || candidates.len() == max_count {
            continue;
        }
        planned_bytes = planned_bytes
            .checked_add(scored_candidate.closure_gain)
            .ok_or(CompactSelectionError::SizeOverflow)?;
        candidates.push(materialize_candidate(scored_candidate.path_id)?);
        if planned_bytes >= target_bytes {
            break;
        }
    }
    let selected_priority_count = candidates.len();
    let target_reached = planned_bytes >= target_bytes;
    let cap_exhausted = !target_reached && selected_priority_count < nonredundant_candidates;
    let eligible_exhausted = !target_reached && selected_priority_count == nonredundant_candidates;

    Ok(CompactEvictionBatch {
        topology: Arc::clone(topology),
        generation: generation.clone(),
        candidates,
        policy: CompactEvictionPolicy::ResidentBudgetAncestorClosure,
        report: CompactSelectionReport {
            planned_bytes,
            eligible_candidates,
            nonredundant_candidates,
            selected_priority_count,
            cap_exhausted,
            eligible_exhausted,
        },
    })
}

#[derive(Clone, Copy)]
struct RegistryRecordMeta {
    disk_address: DiskRecordAddress,
    serialized_bytes: usize,
}

/// Registry mapping node paths to their disk locations.
///
/// Populated during checkpoint and used by the eviction coordinator to
/// determine which nodes can be safely evicted (i.e., have valid disk
/// representations).
///
/// # Lifetime
///
/// The registry is invalidated after any write operation, as nodes may
/// have been modified since the last checkpoint. A new registry is
/// populated during each checkpoint.
///
/// # Memory Overhead
///
/// Each entry uses approximately:
/// - Path length + 8 bytes (Vec overhead)
/// - 8 bytes for the SwizzledPtr
/// - 8 bytes for size_bytes
/// - 8 bytes for depth
/// - 8 bytes for HashMap overhead
///
/// For a trie with 1M nodes and average path length of 10 bytes,
/// this is ~50MB of registry overhead.
pub struct DiskLocationRegistry {
    generation: RegistryGeneration,
    /// Byte records indexed directly by dense compact path identifier.
    /// Arc-backed so a checkpoint can retain an immutable O(1) source image
    /// while constructing its successor without holding the coordinator lock.
    locations: Arc<Vec<Option<ByteRegistryEntry>>>,
    /// Char records indexed directly by dense compact path identifier.
    char_locations: Arc<Vec<Option<CharRegistryEntry>>>,
    /// Compatibility lookup from approximate byte LRU hash to every exact ID
    /// sharing that hash. Collisions never overwrite durable records.
    byte_hash_index: Arc<HashMap<u64, RegistryPathBucket>>,
    /// Character twin of `byte_hash_index`.
    char_hash_index: Arc<HashMap<u64, RegistryPathBucket>>,
    /// Exact raw disk-record identity to every byte path occurrence. A durable
    /// record may be shared by several structural aliases, so the value is a
    /// bucket and exact segmented path comparison resolves the occurrence.
    byte_disk_index: Arc<HashMap<DiskRecordAddress, RegistryPathBucket>>,
    /// Character-key twin of `byte_disk_index`.
    char_disk_index: Arc<HashMap<DiskRecordAddress, RegistryPathBucket>>,
    byte_len: usize,
    char_len: usize,
    /// Prefix-sharing topology for byte paths.
    byte_paths: Arc<PathTopology<u8>>,
    /// Prefix-sharing topology for char paths.
    char_paths: Arc<PathTopology<u32>>,
    /// LIFO byte subtree starts currently owned by serializer frames.
    byte_builder_stack: Vec<RegistryPathId>,
    /// LIFO character subtree starts currently owned by serializer frames.
    char_builder_stack: Vec<RegistryPathId>,
    /// Exact current-root residency for byte durable records.
    byte_residency: ResidencyState,
    /// Exact current-root residency for char durable records.
    char_residency: ResidencyState,
    /// Total size of tracked nodes.
    total_size_bytes: usize,
    /// Number of nodes by type.
    node_type_counts: HashMap<NodeType, usize>,
    /// Whether this registry can currently authorize residency transitions.
    authority: RegistryAuthority,
}

/// Immutable structural catalog plus lock-free residency materializations for
/// one checkpoint generation. Root revisions retain this `Arc` directly, so
/// exact authority and metadata lifetime do not depend on a coordinator slot.
pub(crate) struct PublishedRegistryCatalog {
    generation: RegistryGeneration,
    locations: Arc<Vec<Option<ByteRegistryEntry>>>,
    char_locations: Arc<Vec<Option<CharRegistryEntry>>>,
    byte_disk_index: Arc<HashMap<DiskRecordAddress, RegistryPathBucket>>,
    char_disk_index: Arc<HashMap<DiskRecordAddress, RegistryPathBucket>>,
    byte_paths: Arc<PathTopology<u8>>,
    char_paths: Arc<PathTopology<u32>>,
    byte_residency: AtomicResidencyGeneration,
    char_residency: AtomicResidencyGeneration,
}

/// Fallible sparse-transition accumulator with an inline singleton fast path.
/// A one-word eviction allocates no transition vector. Multiword evictions
/// begin with exactly two slots and grow geometrically with the number of
/// affected resident words, never with the size of a sparse covered subtree.
struct PackedTransitionBuilder {
    one: Option<PackedResidencyTransition>,
    many: Vec<PackedResidencyTransition>,
}

impl PackedTransitionBuilder {
    #[inline(always)]
    fn new() -> Self {
        Self {
            one: None,
            many: Vec::new(),
        }
    }

    fn try_push(
        &mut self,
        transition: PackedResidencyTransition,
    ) -> std::result::Result<(), EvictionCommitError> {
        if self.many.is_empty() {
            if let Some(first) = self.one.take() {
                self.many
                    .try_reserve_exact(2)
                    .map_err(|_| EvictionCommitError::Allocation)?;
                self.many.push(first);
                self.many.push(transition);
            } else {
                self.one = Some(transition);
            }
        } else {
            if self.many.len() == self.many.capacity() {
                self.many
                    .try_reserve_exact(self.many.len())
                    .map_err(|_| EvictionCommitError::Allocation)?;
            }
            self.many.push(transition);
        }
        Ok(())
    }

    fn finish(self) -> std::result::Result<PackedResidencyDelta, EvictionCommitError> {
        if self.many.is_empty() {
            self.one
                .map(PackedResidencyDelta::One)
                .ok_or(EvictionCommitError::CandidateUnavailable)
        } else {
            debug_assert!(self.one.is_none());
            Ok(PackedResidencyDelta::Many(self.many))
        }
    }
}

#[inline(always)]
fn topology_coverage_mask(range: &std::ops::Range<usize>, word: usize) -> u32 {
    let word_start = word * super::atomic_residency::RESIDENCY_PATHS_PER_WORD;
    let start_bit = range.start.saturating_sub(word_start);
    let end_bit = range
        .end
        .saturating_sub(word_start)
        .min(super::atomic_residency::RESIDENCY_PATHS_PER_WORD);
    let lower = u32::MAX << start_bit;
    let upper = if end_bit == super::atomic_residency::RESIDENCY_PATHS_PER_WORD {
        u32::MAX
    } else {
        (1u32 << end_bit) - 1
    };
    lower & upper
}

/// Visit the union of selected preorder-subtree ranges without materializing a
/// range vector. `successful` is sorted once by dense preorder identifier;
/// ancestor, duplicate, and adjacent ranges then collapse in one streaming
/// pass with constant auxiliary space.
fn try_for_each_merged_eviction_range<U, F>(
    topology: &PathTopology<U>,
    batch: &CompactEvictionBatch<U>,
    successful: &[usize],
    mut visit: F,
) -> std::result::Result<(), EvictionCommitError>
where
    U: Copy,
    F: FnMut(std::ops::Range<usize>) -> std::result::Result<(), EvictionCommitError>,
{
    let mut pending: Option<std::ops::Range<usize>> = None;
    for &candidate_index in successful.iter() {
        let candidate = batch
            .candidates
            .get(candidate_index)
            .ok_or(EvictionCommitError::CandidateUnavailable)?;
        let next = topology
            .subtree_range(candidate.path_id)
            .ok_or(EvictionCommitError::TopologyUnavailable)?;
        match pending.as_mut() {
            Some(current) if current.end >= next.start => {
                current.end = current.end.max(next.end);
            }
            Some(_) => {
                visit(pending.take().expect("pending range exists"))?;
                pending = Some(next);
            }
            None => pending = Some(next),
        }
    }
    if let Some(range) = pending {
        visit(range)?;
    }
    Ok(())
}

struct PreparedPackedEvictionDelta {
    resident_nodes: usize,
    resident_serialized_bytes: usize,
    delta: PackedResidencyDelta,
}

fn try_prepare_packed_eviction_delta<U, M>(
    topology: &PathTopology<U>,
    stable: &super::atomic_residency::StableResidencyTransitionView<'_>,
    resident_nodes: usize,
    resident_serialized_bytes: usize,
    batch: &CompactEvictionBatch<U>,
    successful: &[usize],
    record_meta: &mut M,
) -> std::result::Result<PreparedPackedEvictionDelta, EvictionCommitError>
where
    U: Copy,
    M: FnMut(RegistryPathId) -> Option<RegistryRecordMeta>,
{
    let mut transitions = PackedTransitionBuilder::new();
    let mut evicted_nodes = 0usize;
    let mut evicted_serialized_bytes = 0usize;

    let mut prepare_word =
        |word: usize, coverage_mask: u32| -> std::result::Result<(), EvictionCommitError> {
            let Some((transition, mut resident_mask)) = stable
                .prepare_clear_covered(word, coverage_mask)
                .map_err(|_| EvictionCommitError::CandidateUnavailable)?
            else {
                return Ok(());
            };
            transitions.try_push(transition)?;
            while resident_mask != 0 {
                let bit = resident_mask.trailing_zeros() as usize;
                let path_index = word
                    .checked_mul(super::atomic_residency::RESIDENCY_PATHS_PER_WORD)
                    .and_then(|base| base.checked_add(bit))
                    .ok_or(EvictionCommitError::Arithmetic)?;
                let path_id = RegistryPathId(path_index);
                let meta = record_meta(path_id).ok_or(EvictionCommitError::RegistryInvariant)?;
                evicted_nodes = evicted_nodes
                    .checked_add(1)
                    .ok_or(EvictionCommitError::Arithmetic)?;
                evicted_serialized_bytes = evicted_serialized_bytes
                    .checked_add(meta.serialized_bytes)
                    .ok_or(EvictionCommitError::Arithmetic)?;
                resident_mask &= resident_mask - 1;
            }
            Ok(())
        };

    let mut pending_word = None;
    let mut pending_mask = 0u32;
    try_for_each_merged_eviction_range(topology, batch, successful, |range| {
        let first_word = range.start / super::atomic_residency::RESIDENCY_PATHS_PER_WORD;
        let last_word = (range.end - 1) / super::atomic_residency::RESIDENCY_PATHS_PER_WORD;
        for word in first_word..=last_word {
            let coverage_mask = topology_coverage_mask(&range, word);
            if pending_word == Some(word) {
                pending_mask |= coverage_mask;
            } else {
                if let Some(prior_word) = pending_word {
                    prepare_word(prior_word, pending_mask)?;
                }
                pending_word = Some(word);
                pending_mask = coverage_mask;
            }
        }
        Ok(())
    })?;
    if let Some(word) = pending_word {
        prepare_word(word, pending_mask)?;
    }
    if evicted_nodes == 0 {
        return Err(EvictionCommitError::CandidateUnavailable);
    }
    Ok(PreparedPackedEvictionDelta {
        resident_nodes: resident_nodes
            .checked_sub(evicted_nodes)
            .ok_or(EvictionCommitError::RegistryInvariant)?,
        resident_serialized_bytes: resident_serialized_bytes
            .checked_sub(evicted_serialized_bytes)
            .ok_or(EvictionCommitError::RegistryInvariant)?,
        delta: transitions.finish()?,
    })
}

#[cold]
#[inline(never)]
#[expect(
    clippy::too_many_arguments,
    reason = "explicit monomorphized transition inputs preserve borrow separation on the cold rollover path"
)]
fn try_prepare_packed_eviction_rollover<U, M, R>(
    catalog: &Arc<PublishedRegistryCatalog>,
    topology: &PathTopology<U>,
    residency: &AtomicResidencyGeneration,
    resident_nodes: usize,
    resident_serialized_bytes: usize,
    batch: &CompactEvictionBatch<U>,
    successful: &[usize],
    mut record_meta: M,
    rebase_catalog: R,
) -> std::result::Result<PreparedPackedResidency, EvictionCommitError>
where
    U: Copy,
    M: FnMut(RegistryPathId) -> Option<RegistryRecordMeta>,
    R: FnOnce(
        &PackedResidencyDelta,
    ) -> std::result::Result<Arc<PublishedRegistryCatalog>, EvictionCommitError>,
{
    let stable = residency
        .try_stable(u32::MAX)
        .and_then(|stable| stable.try_fresh_generation())
        .map_err(|_| EvictionCommitError::CandidateUnavailable)?;
    let prepared = try_prepare_packed_eviction_delta(
        topology,
        &stable,
        resident_nodes,
        resident_serialized_bytes,
        batch,
        successful,
        &mut record_meta,
    )?;
    let successor_catalog = rebase_catalog(&prepared.delta)?;
    Ok(PreparedPackedResidency::rebased(
        Arc::clone(catalog),
        successor_catalog,
        prepared.resident_nodes,
        prepared.resident_serialized_bytes,
        prepared.delta,
    ))
}

#[expect(
    clippy::too_many_arguments,
    reason = "explicit monomorphized transition inputs avoid a second request aggregate on exact-root preparation"
)]
fn try_prepare_packed_eviction<U, M, R>(
    catalog: &Arc<PublishedRegistryCatalog>,
    topology: &Arc<PathTopology<U>>,
    residency: &AtomicResidencyGeneration,
    predecessor_ordinal: u32,
    resident_nodes: usize,
    resident_serialized_bytes: usize,
    batch: &CompactEvictionBatch<U>,
    successful: &mut [usize],
    mut record_meta: M,
    rebase_catalog: R,
) -> std::result::Result<PreparedPackedResidency, EvictionCommitError>
where
    U: Copy,
    M: FnMut(RegistryPathId) -> Option<RegistryRecordMeta>,
    R: FnOnce(
        &PackedResidencyDelta,
    ) -> std::result::Result<Arc<PublishedRegistryCatalog>, EvictionCommitError>,
{
    if successful.is_empty()
        || !batch.generation.same_publication(catalog.binding())
        || !Arc::ptr_eq(&batch.topology, topology)
    {
        return Err(EvictionCommitError::CandidateUnavailable);
    }

    for &candidate_index in successful.iter() {
        let candidate = batch
            .candidates
            .get(candidate_index)
            .ok_or(EvictionCommitError::CandidateUnavailable)?;
        let meta =
            record_meta(candidate.path_id).ok_or(EvictionCommitError::CandidateUnavailable)?;
        let candidate_address = DiskRecordAddress::from_pointer(&candidate.disk_ptr)
            .map_err(|_| EvictionCommitError::CandidateUnavailable)?;
        if meta.disk_address != candidate_address {
            return Err(EvictionCommitError::CandidateUnavailable);
        }
    }

    // Compact batches already own their successful-index buffer. Sorting that
    // buffer in place avoids allocating a second vector of subtree ranges.
    // Registry path identifiers are dense preorder positions, so their numeric
    // order is exactly the order required by the streaming union cursor.
    successful.sort_unstable_by_key(|&candidate_index| batch.candidates[candidate_index].path_id.0);

    if predecessor_ordinal == u32::MAX {
        return try_prepare_packed_eviction_rollover(
            catalog,
            topology,
            residency,
            resident_nodes,
            resident_serialized_bytes,
            batch,
            successful,
            record_meta,
            rebase_catalog,
        );
    }

    let target_ordinal = predecessor_ordinal + 1;
    let stable = residency
        .try_stable(predecessor_ordinal)
        .and_then(|stable| stable.try_successor(target_ordinal))
        .map_err(|_| EvictionCommitError::CandidateUnavailable)?;
    let prepared = try_prepare_packed_eviction_delta(
        topology,
        &stable,
        resident_nodes,
        resident_serialized_bytes,
        batch,
        successful,
        &mut record_meta,
    )?;
    Ok(PreparedPackedResidency::sparse(
        Arc::clone(catalog),
        predecessor_ordinal,
        target_ordinal,
        prepared.resident_nodes,
        prepared.resident_serialized_bytes,
        prepared.delta,
    ))
}

impl PublishedRegistryCatalog {
    #[cfg(test)]
    pub(crate) fn empty_for_binding(binding: RegistryGeneration) -> Self {
        Self {
            generation: binding,
            locations: Arc::new(Vec::new()),
            char_locations: Arc::new(Vec::new()),
            byte_disk_index: Arc::new(HashMap::new()),
            char_disk_index: Arc::new(HashMap::new()),
            byte_paths: Arc::new(PathTopology::new()),
            char_paths: Arc::new(PathTopology::new()),
            byte_residency: AtomicResidencyGeneration::try_from_builder_words(&[], 0, 0)
                .expect("empty byte residency generation"),
            char_residency: AtomicResidencyGeneration::try_from_builder_words(&[], 0, 0)
                .expect("empty char residency generation"),
        }
    }

    pub(crate) fn try_from_builder(
        registry: &DiskLocationRegistry,
    ) -> std::result::Result<Self, RegistryBuildError> {
        debug_assert!(registry.topologies_are_finalized());
        let byte_residency = AtomicResidencyGeneration::try_from_builder_words(
            &registry.byte_residency.bits,
            registry.locations.len(),
            0,
        )
        .map_err(|_| RegistryBuildError::Allocation("byte packed residency cells"))?;
        let char_residency = AtomicResidencyGeneration::try_from_builder_words(
            &registry.char_residency.bits,
            registry.char_locations.len(),
            0,
        )
        .map_err(|_| RegistryBuildError::Allocation("char packed residency cells"))?;
        Ok(Self {
            generation: registry.generation.clone(),
            locations: Arc::clone(&registry.locations),
            char_locations: Arc::clone(&registry.char_locations),
            byte_disk_index: Arc::clone(&registry.byte_disk_index),
            char_disk_index: Arc::clone(&registry.char_disk_index),
            byte_paths: Arc::clone(&registry.byte_paths),
            char_paths: Arc::clone(&registry.char_paths),
            byte_residency,
            char_residency,
        })
    }

    #[cfg(test)]
    pub(crate) fn try_from_builder_at_ordinals(
        registry: &DiskLocationRegistry,
        byte_ordinal: u32,
        char_ordinal: u32,
    ) -> std::result::Result<Self, RegistryBuildError> {
        let mut catalog = Self::try_from_builder(registry)?;
        catalog.byte_residency = AtomicResidencyGeneration::try_from_builder_words(
            &registry.byte_residency.bits,
            registry.locations.len(),
            byte_ordinal,
        )
        .map_err(|_| RegistryBuildError::Allocation("test byte packed residency cells"))?;
        catalog.char_residency = AtomicResidencyGeneration::try_from_builder_words(
            &registry.char_residency.bits,
            registry.char_locations.len(),
            char_ordinal,
        )
        .map_err(|_| RegistryBuildError::Allocation("test char packed residency cells"))?;
        Ok(catalog)
    }

    #[inline]
    pub(crate) fn binding(&self) -> &RegistryGeneration {
        &self.generation
    }

    #[inline]
    pub(crate) fn byte_residency(&self) -> &AtomicResidencyGeneration {
        &self.byte_residency
    }

    #[inline]
    pub(crate) fn char_residency(&self) -> &AtomicResidencyGeneration {
        &self.char_residency
    }

    fn with_rebased_residencies(
        &self,
        byte_residency: AtomicResidencyGeneration,
        char_residency: AtomicResidencyGeneration,
    ) -> Self {
        Self {
            generation: self.generation.clone(),
            locations: Arc::clone(&self.locations),
            char_locations: Arc::clone(&self.char_locations),
            byte_disk_index: Arc::clone(&self.byte_disk_index),
            char_disk_index: Arc::clone(&self.char_disk_index),
            byte_paths: Arc::clone(&self.byte_paths),
            char_paths: Arc::clone(&self.char_paths),
            byte_residency,
            char_residency,
        }
    }

    #[cold]
    #[inline(never)]
    fn try_rebased_byte(
        &self,
        delta: &PackedResidencyDelta,
    ) -> std::result::Result<Arc<Self>, EvictionCommitError> {
        let byte_residency = self
            .byte_residency
            .try_rebased(u32::MAX, delta)
            .map_err(map_residency_prepare_error)?;
        let char_ordinal = self.char_residency.frontier();
        let char_residency = self
            .char_residency
            .try_rebased(char_ordinal, &PackedResidencyDelta::None)
            .map_err(map_residency_prepare_error)?;
        Ok(Arc::new(
            self.with_rebased_residencies(byte_residency, char_residency),
        ))
    }

    #[cold]
    #[inline(never)]
    fn try_rebased_char(
        &self,
        delta: &PackedResidencyDelta,
    ) -> std::result::Result<Arc<Self>, EvictionCommitError> {
        let char_residency = self
            .char_residency
            .try_rebased(u32::MAX, delta)
            .map_err(map_residency_prepare_error)?;
        let byte_ordinal = self.byte_residency.frontier();
        let byte_residency = self
            .byte_residency
            .try_rebased(byte_ordinal, &PackedResidencyDelta::None)
            .map_err(map_residency_prepare_error)?;
        Ok(Arc::new(
            self.with_rebased_residencies(byte_residency, char_residency),
        ))
    }

    pub(crate) fn try_byte_selection_snapshot(
        &self,
        ordinal: u32,
    ) -> std::result::Result<ByteRegistrySelectionSnapshot, RegistryBuildError> {
        let mut residency_bits = Vec::new();
        self.byte_residency
            .try_snapshot_payloads(ordinal, &mut residency_bits)
            .map_err(|error| match error {
                super::atomic_residency::ResidencyPrepareError::Allocation => {
                    RegistryBuildError::Allocation("byte packed selection snapshot")
                }
                _ => RegistryBuildError::DestinationInvariant(
                    "byte packed selection snapshot changed during capture",
                ),
            })?;
        Ok(ByteRegistrySelectionSnapshot {
            generation: self.generation.clone(),
            topology: Arc::clone(&self.byte_paths),
            locations: Arc::clone(&self.locations),
            residency_bits,
        })
    }

    pub(crate) fn try_char_selection_snapshot(
        &self,
        ordinal: u32,
    ) -> std::result::Result<CharRegistrySelectionSnapshot, RegistryBuildError> {
        let mut residency_bits = Vec::new();
        self.char_residency
            .try_snapshot_payloads(ordinal, &mut residency_bits)
            .map_err(|error| match error {
                super::atomic_residency::ResidencyPrepareError::Allocation => {
                    RegistryBuildError::Allocation("char packed selection snapshot")
                }
                _ => RegistryBuildError::DestinationInvariant(
                    "char packed selection snapshot changed during capture",
                ),
            })?;
        Ok(CharRegistrySelectionSnapshot {
            generation: self.generation.clone(),
            topology: Arc::clone(&self.char_paths),
            locations: Arc::clone(&self.char_locations),
            residency_bits,
        })
    }

    #[inline]
    fn byte_record_meta(&self, path_id: RegistryPathId) -> Option<RegistryRecordMeta> {
        let entry = self.locations.get(path_id.0)?.as_ref()?;
        Some(RegistryRecordMeta {
            disk_address: DiskRecordAddress::from_pointer(&entry.disk_ptr).ok()?,
            serialized_bytes: entry.size_bytes,
        })
    }

    #[inline]
    fn char_record_meta(&self, path_id: RegistryPathId) -> Option<RegistryRecordMeta> {
        let entry = self.char_locations.get(path_id.0)?.as_ref()?;
        Some(RegistryRecordMeta {
            disk_address: DiskRecordAddress::from_pointer(&entry.disk_ptr).ok()?,
            serialized_bytes: entry.size_bytes,
        })
    }

    pub(crate) fn prepare_byte_eviction_packed(
        self: &Arc<Self>,
        predecessor_ordinal: u32,
        resident_nodes: usize,
        resident_serialized_bytes: usize,
        batch: &CompactEvictionBatch<u8>,
        successful: &mut [usize],
    ) -> Option<PreparedPackedResidency> {
        try_prepare_packed_eviction(
            self,
            &self.byte_paths,
            &self.byte_residency,
            predecessor_ordinal,
            resident_nodes,
            resident_serialized_bytes,
            batch,
            successful,
            |path_id| self.byte_record_meta(path_id),
            |delta| self.try_rebased_byte(delta),
        )
        .ok()
    }

    pub(crate) fn prepare_char_eviction_packed(
        self: &Arc<Self>,
        predecessor_ordinal: u32,
        resident_nodes: usize,
        resident_serialized_bytes: usize,
        batch: &CompactEvictionBatch<u32>,
        successful: &mut [usize],
    ) -> Option<PreparedPackedResidency> {
        try_prepare_packed_eviction(
            self,
            &self.char_paths,
            &self.char_residency,
            predecessor_ordinal,
            resident_nodes,
            resident_serialized_bytes,
            batch,
            successful,
            |path_id| self.char_record_meta(path_id),
            |delta| self.try_rebased_char(delta),
        )
        .ok()
    }

    #[cold]
    #[inline(never)]
    fn prepare_byte_fault_rollover(
        self: &Arc<Self>,
        path_id: RegistryPathId,
        meta: RegistryRecordMeta,
        resident_nodes: usize,
        resident_serialized_bytes: usize,
    ) -> Option<PreparedPackedResidency> {
        let transition = self
            .byte_residency
            .try_stable(u32::MAX)
            .and_then(|stable| stable.try_fresh_generation())
            .ok()?
            .prepare_mark(path_id.0)
            .ok()??;
        let resident_nodes = resident_nodes.checked_add(1)?;
        let resident_serialized_bytes =
            resident_serialized_bytes.checked_add(meta.serialized_bytes)?;
        let delta = PackedResidencyDelta::One(transition);
        Some(PreparedPackedResidency::rebased(
            Arc::clone(self),
            self.try_rebased_byte(&delta).ok()?,
            resident_nodes,
            resident_serialized_bytes,
            delta,
        ))
    }

    #[cold]
    #[inline(never)]
    fn prepare_char_fault_rollover(
        self: &Arc<Self>,
        path_id: RegistryPathId,
        meta: RegistryRecordMeta,
        resident_nodes: usize,
        resident_serialized_bytes: usize,
    ) -> Option<PreparedPackedResidency> {
        let transition = self
            .char_residency
            .try_stable(u32::MAX)
            .and_then(|stable| stable.try_fresh_generation())
            .ok()?
            .prepare_mark(path_id.0)
            .ok()??;
        let resident_nodes = resident_nodes.checked_add(1)?;
        let resident_serialized_bytes =
            resident_serialized_bytes.checked_add(meta.serialized_bytes)?;
        let delta = PackedResidencyDelta::One(transition);
        Some(PreparedPackedResidency::rebased(
            Arc::clone(self),
            self.try_rebased_char(&delta).ok()?,
            resident_nodes,
            resident_serialized_bytes,
            delta,
        ))
    }

    pub(crate) fn prepare_byte_fault_packed(
        self: &Arc<Self>,
        predecessor_ordinal: u32,
        resident_nodes: usize,
        resident_serialized_bytes: usize,
        path: &[u8],
        disk_ptr: &SwizzledPtr,
    ) -> Option<PreparedPackedResidency> {
        let disk_address = DiskRecordAddress::from_pointer(disk_ptr).ok()?;
        let bucket = self.byte_disk_index.get(&disk_address)?;
        let mut resolved = None;
        for &path_id in bucket {
            let meta = self.byte_record_meta(path_id)?;
            if meta.disk_address != disk_address
                || !self.byte_paths.path_equals_slice(path_id, path)
            {
                continue;
            }
            if resolved.replace((path_id, meta)).is_some() {
                return None;
            }
        }
        let (path_id, meta) = resolved?;
        if predecessor_ordinal == u32::MAX {
            return self.prepare_byte_fault_rollover(
                path_id,
                meta,
                resident_nodes,
                resident_serialized_bytes,
            );
        }
        let target_ordinal = predecessor_ordinal + 1;
        let stable = self
            .byte_residency
            .try_stable(predecessor_ordinal)
            .and_then(|stable| stable.try_successor(target_ordinal))
            .ok()?;
        let transition = stable.prepare_mark(path_id.0).ok()??;
        let resident_nodes = resident_nodes.checked_add(1)?;
        let resident_serialized_bytes =
            resident_serialized_bytes.checked_add(meta.serialized_bytes)?;
        let delta = PackedResidencyDelta::One(transition);
        Some(PreparedPackedResidency::sparse(
            Arc::clone(self),
            predecessor_ordinal,
            target_ordinal,
            resident_nodes,
            resident_serialized_bytes,
            delta,
        ))
    }

    pub(crate) fn prepare_char_fault_packed(
        self: &Arc<Self>,
        predecessor_ordinal: u32,
        resident_nodes: usize,
        resident_serialized_bytes: usize,
        path: &[u32],
        disk_ptr: &SwizzledPtr,
    ) -> Option<PreparedPackedResidency> {
        let disk_address = DiskRecordAddress::from_pointer(disk_ptr).ok()?;
        let bucket = self.char_disk_index.get(&disk_address)?;
        let mut resolved = None;
        for &path_id in bucket {
            let meta = self.char_record_meta(path_id)?;
            if meta.disk_address != disk_address
                || !self.char_paths.path_equals_slice(path_id, path)
            {
                continue;
            }
            if resolved.replace((path_id, meta)).is_some() {
                return None;
            }
        }
        let (path_id, meta) = resolved?;
        if predecessor_ordinal == u32::MAX {
            return self.prepare_char_fault_rollover(
                path_id,
                meta,
                resident_nodes,
                resident_serialized_bytes,
            );
        }
        let target_ordinal = predecessor_ordinal + 1;
        let stable = self
            .char_residency
            .try_stable(predecessor_ordinal)
            .and_then(|stable| stable.try_successor(target_ordinal))
            .ok()?;
        let transition = stable.prepare_mark(path_id.0).ok()??;
        let resident_nodes = resident_nodes.checked_add(1)?;
        let resident_serialized_bytes =
            resident_serialized_bytes.checked_add(meta.serialized_bytes)?;
        let delta = PackedResidencyDelta::One(transition);
        Some(PreparedPackedResidency::sparse(
            Arc::clone(self),
            predecessor_ordinal,
            target_ordinal,
            resident_nodes,
            resident_serialized_bytes,
            delta,
        ))
    }
}

/// Compile-time selection of the catalog family used by one root encoding.
/// Byte/char monomorphizations contain no runtime family branch. Native-u64
/// persistence currently has no eviction registry and therefore observes the
/// empty byte family.
pub(crate) trait RegistryFamily: KeyEncoding {
    fn builder_resident_totals(registry: &DiskLocationRegistry) -> (usize, usize);

    fn residency(catalog: &PublishedRegistryCatalog) -> &AtomicResidencyGeneration;

    fn path_hash(catalog: &PublishedRegistryCatalog, path_index: usize) -> Option<u64>;
}

impl RegistryFamily for ByteKey {
    #[inline(always)]
    fn builder_resident_totals(registry: &DiskLocationRegistry) -> (usize, usize) {
        (
            registry.byte_resident_len(),
            registry.byte_resident_serialized_bytes(),
        )
    }

    #[inline(always)]
    fn residency(catalog: &PublishedRegistryCatalog) -> &AtomicResidencyGeneration {
        catalog.byte_residency()
    }

    #[inline(always)]
    fn path_hash(catalog: &PublishedRegistryCatalog, path_index: usize) -> Option<u64> {
        catalog
            .locations
            .get(path_index)?
            .as_ref()
            .map(|entry| entry.path_hash)
    }
}

impl RegistryFamily for CharKey {
    #[inline(always)]
    fn builder_resident_totals(registry: &DiskLocationRegistry) -> (usize, usize) {
        (
            registry.char_resident_len(),
            registry.char_resident_serialized_bytes(),
        )
    }

    #[inline(always)]
    fn residency(catalog: &PublishedRegistryCatalog) -> &AtomicResidencyGeneration {
        catalog.char_residency()
    }

    #[inline(always)]
    fn path_hash(catalog: &PublishedRegistryCatalog, path_index: usize) -> Option<u64> {
        catalog
            .char_locations
            .get(path_index)?
            .as_ref()
            .map(|entry| entry.path_hash)
    }
}

impl<const PREFIX: usize> RegistryFamily for U64Key<PREFIX> {
    #[inline(always)]
    fn builder_resident_totals(registry: &DiskLocationRegistry) -> (usize, usize) {
        (
            registry.byte_resident_len(),
            registry.byte_resident_serialized_bytes(),
        )
    }

    #[inline(always)]
    fn residency(catalog: &PublishedRegistryCatalog) -> &AtomicResidencyGeneration {
        catalog.byte_residency()
    }

    #[inline(always)]
    fn path_hash(catalog: &PublishedRegistryCatalog, path_index: usize) -> Option<u64> {
        catalog
            .locations
            .get(path_index)?
            .as_ref()
            .map(|entry| entry.path_hash)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RegistryAuthority {
    /// Structurally complete registry data that has not been atomically bound
    /// to an overlay root revision. Detached registries may serve the public
    /// compatibility inspection/selection APIs, but can never authorize an
    /// eviction or fault residency transition.
    Detached,
    /// Installed and root-bound, but durable node stamps are still being
    /// activated. Selection is disabled and fault preparation waits at the
    /// coordinator lifecycle boundary.
    Publishing,
    /// Fully published for the exact bound root revision.
    Valid,
    /// Invalidated by a semantic mutation or failed residency invariant.
    Invalid,
}

impl DiskLocationRegistry {
    /// Opaque identity that must be bound to the exact overlay root revision
    /// before this registry can authorize eviction or fault transitions.
    pub(crate) fn binding(&self) -> RegistryGeneration {
        self.generation.clone()
    }

    /// Complete every fallible topology-finalization step before publication.
    pub(crate) fn try_finalize_for_publication(
        &mut self,
    ) -> std::result::Result<(), RegistryBuildError> {
        if !self.byte_builder_stack.is_empty() || !self.char_builder_stack.is_empty() {
            return Err(RegistryBuildError::TopologyInvariant(
                "registry publication reached an unfinished builder subtree",
            ));
        }
        if !self.byte_paths.is_finalized() {
            Arc::get_mut(&mut self.byte_paths)
                .ok_or(RegistryBuildError::DestinationInvariant(
                    "unfinalized byte topology is shared",
                ))?
                .try_finalize_subtree_ends()?;
        } else {
            self.byte_paths.try_finalized_subtree_ends()?;
        }
        if !self.char_paths.is_finalized() {
            Arc::get_mut(&mut self.char_paths)
                .ok_or(RegistryBuildError::DestinationInvariant(
                    "unfinalized char topology is shared",
                ))?
                .try_finalize_subtree_ends()?;
        } else {
            self.char_paths.try_finalized_subtree_ends()?;
        }
        Ok(())
    }

    /// Whether both immutable topology indexes are ready for publication.
    ///
    /// This is an allocation-free assertion seam for the already-prepared
    /// coordinator critical section; it does not validate or mutate either
    /// topology.
    pub(crate) fn topologies_are_finalized(&self) -> bool {
        self.byte_paths.is_finalized() && self.char_paths.is_finalized()
    }

    /// Create a new empty registry.
    pub fn new() -> Self {
        Self {
            generation: RegistryGeneration::new(),
            locations: Arc::new(Vec::new()),
            char_locations: Arc::new(Vec::new()),
            byte_hash_index: Arc::new(HashMap::new()),
            char_hash_index: Arc::new(HashMap::new()),
            byte_disk_index: Arc::new(HashMap::new()),
            char_disk_index: Arc::new(HashMap::new()),
            byte_len: 0,
            char_len: 0,
            byte_paths: Arc::new(PathTopology::new()),
            char_paths: Arc::new(PathTopology::new()),
            byte_builder_stack: Vec::new(),
            char_builder_stack: Vec::new(),
            byte_residency: ResidencyState::default(),
            char_residency: ResidencyState::default(),
            total_size_bytes: 0,
            node_type_counts: HashMap::new(),
            authority: RegistryAuthority::Detached,
        }
    }

    /// Create a registry with pre-allocated capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            generation: RegistryGeneration::new(),
            locations: Arc::new(Vec::with_capacity(capacity)),
            char_locations: Arc::new(Vec::with_capacity(capacity)),
            byte_hash_index: Arc::new(HashMap::with_capacity(capacity)),
            char_hash_index: Arc::new(HashMap::with_capacity(capacity)),
            byte_disk_index: Arc::new(HashMap::with_capacity(capacity)),
            char_disk_index: Arc::new(HashMap::with_capacity(capacity)),
            byte_len: 0,
            char_len: 0,
            byte_paths: Arc::new(PathTopology::with_capacity(capacity)),
            char_paths: Arc::new(PathTopology::with_capacity(capacity)),
            byte_builder_stack: Vec::new(),
            char_builder_stack: Vec::new(),
            byte_residency: ResidencyState::default(),
            char_residency: ResidencyState::default(),
            total_size_bytes: 0,
            node_type_counts: HashMap::new(),
            authority: RegistryAuthority::Detached,
        }
    }

    /// Describe the buffers required for one structural-source capture.
    ///
    /// The returned generation token makes the later copy fail closed when a
    /// registry replacement races the allocation performed between read-lock
    /// acquisitions.
    pub(crate) fn structural_source_capture_plan(
        &self,
    ) -> std::result::Result<RegistryStructuralCapturePlan, RegistryBuildError> {
        self.byte_paths.try_finalized_subtree_ends()?;
        self.char_paths.try_finalized_subtree_ends()?;
        Ok(RegistryStructuralCapturePlan {
            generation: self.generation.clone(),
            byte_residency_words: self.byte_residency.bits.len(),
            char_residency_words: self.char_residency.bits.len(),
        })
    }

    /// Fill already-reserved residency buffers and capture immutable tables.
    ///
    /// This method performs no allocation. A generation or dimension change
    /// returns ownership of both buffers so the caller can retry iteratively
    /// after releasing the coordinator read lock.
    pub(crate) fn try_capture_structural_source(
        &self,
        plan: &RegistryStructuralCapturePlan,
        mut byte_residency_bits: Vec<u64>,
        mut char_residency_bits: Vec<u64>,
    ) -> std::result::Result<RegistryStructuralCapture, RegistryBuildError> {
        self.byte_paths.try_finalized_subtree_ends()?;
        self.char_paths.try_finalized_subtree_ends()?;
        if !self.generation.same_publication(&plan.generation)
            || self.byte_residency.bits.len() != plan.byte_residency_words
            || self.char_residency.bits.len() != plan.char_residency_words
            || byte_residency_bits.capacity() < plan.byte_residency_words
            || char_residency_bits.capacity() < plan.char_residency_words
        {
            return Ok(RegistryStructuralCapture::Retry {
                byte_residency_bits,
                char_residency_bits,
            });
        }
        debug_assert!(byte_residency_bits.is_empty());
        debug_assert!(char_residency_bits.is_empty());
        byte_residency_bits.extend_from_slice(&self.byte_residency.bits);
        char_residency_bits.extend_from_slice(&self.char_residency.bits);
        Ok(RegistryStructuralCapture::Ready(RegistryStructuralSource {
            _generation: self.generation.clone(),
            byte_paths: Arc::clone(&self.byte_paths),
            char_paths: Arc::clone(&self.char_paths),
            locations: Arc::clone(&self.locations),
            char_locations: Arc::clone(&self.char_locations),
            byte_disk_index: Arc::clone(&self.byte_disk_index),
            char_disk_index: Arc::clone(&self.char_disk_index),
            byte_residency_bits,
            char_residency_bits,
        }))
    }

    /// Direct capture convenience for tests that own the registry exclusively.
    #[cfg(test)]
    pub(crate) fn structural_source(
        &self,
    ) -> std::result::Result<Option<RegistryStructuralSource>, RegistryBuildError> {
        let plan = self.structural_source_capture_plan()?;
        let mut byte_residency_bits = Vec::new();
        let mut char_residency_bits = Vec::new();
        plan.try_prepare_buffers(&mut byte_residency_bits, &mut char_residency_bits)?;
        match self.try_capture_structural_source(&plan, byte_residency_bits, char_residency_bits)? {
            RegistryStructuralCapture::Ready(source) => Ok(Some(source)),
            RegistryStructuralCapture::Retry { .. } => {
                Err(RegistryBuildError::DestinationInvariant(
                    "exclusively borrowed structural source changed during capture",
                ))
            }
        }
    }

    /// Copy an exact byte subtree from a prior immutable registry image.
    ///
    /// The destination root must already represent the pass-through child's
    /// incoming edge and must be the newest topology entry. Descendants retain
    /// only their source-local segments, so this operation is linear in the
    /// subtree and never materializes an absolute key path.
    pub(crate) fn try_graft_byte_subtree(
        &mut self,
        source: &RegistryStructuralSource,
        destination_root: RegistryPathId,
        disk_ptr: &SwizzledPtr,
        root_resident: bool,
    ) -> std::result::Result<RegistryGraftOutcome, RegistryBuildError> {
        if self
            .locations
            .get(destination_root.0)
            .and_then(Option::as_ref)
            .is_some()
        {
            return Err(RegistryBuildError::DestinationInvariant(
                "byte graft root is already registered",
            ));
        }
        let Some(plan) = try_plan_registry_graft(
            RegistryGraftSource {
                topology: &source.byte_paths,
                locations: &source.locations,
                disk_index: &source.byte_disk_index,
            },
            RegistryGraftDestination {
                topology: &self.byte_paths,
                root: destination_root,
                disk_ptr,
            },
            |entry| GraftRecordMeta {
                disk_address: entry.disk_ptr.disk_location().map_or(
                    DiskRecordAddress {
                        block_id: 0,
                        slot_id: 0,
                    },
                    |location| DiskRecordAddress {
                        block_id: location.block_id,
                        slot_id: location.offset,
                    },
                ),
                disk_raw: entry.disk_ptr.to_raw(),
                size_bytes: entry.size_bytes,
                depth: entry.depth,
                node_type: entry.node_type,
            },
            |node_type| node_type.is_byte_level(),
        )?
        else {
            return Ok(RegistryGraftOutcome::FallbackRequired);
        };
        if source.byte_is_resident(plan.source_root) != root_resident {
            return Ok(RegistryGraftOutcome::FallbackRequired);
        }

        let additional_entries = plan
            .source_range
            .len()
            .checked_sub(1)
            .ok_or(RegistryBuildError::Arithmetic("byte graft entry count"))?;
        let final_index = destination_root
            .0
            .checked_add(additional_entries)
            .ok_or(RegistryBuildError::Arithmetic("byte graft destination IDs"))?;
        Arc::get_mut(&mut self.byte_paths)
            .ok_or(RegistryBuildError::DestinationInvariant(
                "byte builder topology is shared",
            ))?
            .try_reserve_additional(additional_entries, plan.additional_units)?;
        let locations = Arc::get_mut(&mut self.locations).ok_or(
            RegistryBuildError::DestinationInvariant("byte builder locations are shared"),
        )?;
        let required_locations = final_index
            .checked_add(1)
            .ok_or(RegistryBuildError::Arithmetic("byte graft location count"))?;
        if required_locations > locations.len() {
            locations
                .try_reserve(required_locations - locations.len())
                .map_err(|_| RegistryBuildError::Allocation("byte graft locations"))?;
        }
        Arc::get_mut(&mut self.byte_hash_index)
            .ok_or(RegistryBuildError::DestinationInvariant(
                "byte builder hash index is shared",
            ))?
            .try_reserve(plan.durable_records)
            .map_err(|_| RegistryBuildError::Allocation("byte graft hash index"))?;
        Arc::get_mut(&mut self.byte_disk_index)
            .ok_or(RegistryBuildError::DestinationInvariant(
                "byte builder disk index is shared",
            ))?
            .try_reserve(plan.durable_records)
            .map_err(|_| RegistryBuildError::Allocation("byte graft disk index"))?;
        self.node_type_counts
            .try_reserve(5)
            .map_err(|_| RegistryBuildError::Allocation("byte graft node-type counts"))?;
        let (last_word, _) = ResidencyState::word_and_mask(final_index);
        self.byte_residency
            .ensure_word(last_word)
            .map_err(RegistryBuildError::Registration)?;

        for source_index in plan.source_range.clone() {
            let source_id = RegistryPathId(source_index);
            let offset = source_index
                .checked_sub(plan.source_root.0)
                .ok_or(RegistryBuildError::Arithmetic("byte graft source offset"))?;
            let target_id = if source_id == plan.source_root {
                destination_root
            } else {
                let source_parent = source.byte_paths.parent(source_id).ok_or(
                    RegistryBuildError::TopologyInvariant("byte graft descendant has no parent"),
                )?;
                let parent_offset = source_parent
                    .0
                    .checked_sub(plan.source_root.0)
                    .ok_or(RegistryBuildError::Arithmetic("byte graft parent offset"))?;
                let target_parent = RegistryPathId(
                    destination_root
                        .0
                        .checked_add(parent_offset)
                        .ok_or(RegistryBuildError::Arithmetic("byte graft parent ID"))?,
                );
                let segment = source.byte_paths.segment(source_id).ok_or(
                    RegistryBuildError::TopologyInvariant("byte graft segment is unavailable"),
                )?;
                let admitted = self
                    .try_reserve_byte_path(target_parent, segment)
                    .map_err(RegistryBuildError::Registration)?;
                let expected = destination_root
                    .0
                    .checked_add(offset)
                    .ok_or(RegistryBuildError::Arithmetic("byte graft target ID"))?;
                if admitted.0 != expected {
                    return Err(RegistryBuildError::DestinationInvariant(
                        "byte graft IDs are not contiguous",
                    ));
                }
                admitted
            };

            if let Some(entry) = source.locations.get(source_index).and_then(Option::as_ref) {
                self.register_byte_path_with_residency(
                    target_id,
                    entry.disk_ptr.clone(),
                    entry.size_bytes,
                    entry.depth,
                    entry.node_type,
                    source.byte_is_resident(source_id),
                )
                .map_err(RegistryBuildError::Registration)?;
            }
        }

        Ok(RegistryGraftOutcome::Grafted {
            topology_entries: plan.source_range.len(),
            durable_records: plan.durable_records,
        })
    }

    /// Character-key twin of [`Self::try_graft_byte_subtree`].
    pub(crate) fn try_graft_char_subtree(
        &mut self,
        source: &RegistryStructuralSource,
        destination_root: RegistryPathId,
        disk_ptr: &SwizzledPtr,
        root_resident: bool,
    ) -> std::result::Result<RegistryGraftOutcome, RegistryBuildError> {
        if self
            .char_locations
            .get(destination_root.0)
            .and_then(Option::as_ref)
            .is_some()
        {
            return Err(RegistryBuildError::DestinationInvariant(
                "char graft root is already registered",
            ));
        }
        let Some(plan) = try_plan_registry_graft(
            RegistryGraftSource {
                topology: &source.char_paths,
                locations: &source.char_locations,
                disk_index: &source.char_disk_index,
            },
            RegistryGraftDestination {
                topology: &self.char_paths,
                root: destination_root,
                disk_ptr,
            },
            |entry| GraftRecordMeta {
                disk_address: entry.disk_ptr.disk_location().map_or(
                    DiskRecordAddress {
                        block_id: 0,
                        slot_id: 0,
                    },
                    |location| DiskRecordAddress {
                        block_id: location.block_id,
                        slot_id: location.offset,
                    },
                ),
                disk_raw: entry.disk_ptr.to_raw(),
                size_bytes: entry.size_bytes,
                depth: entry.depth,
                node_type: entry.node_type,
            },
            |node_type| node_type.is_char_level(),
        )?
        else {
            return Ok(RegistryGraftOutcome::FallbackRequired);
        };
        if source.char_is_resident(plan.source_root) != root_resident {
            return Ok(RegistryGraftOutcome::FallbackRequired);
        }

        let additional_entries = plan
            .source_range
            .len()
            .checked_sub(1)
            .ok_or(RegistryBuildError::Arithmetic("char graft entry count"))?;
        let final_index = destination_root
            .0
            .checked_add(additional_entries)
            .ok_or(RegistryBuildError::Arithmetic("char graft destination IDs"))?;
        Arc::get_mut(&mut self.char_paths)
            .ok_or(RegistryBuildError::DestinationInvariant(
                "char builder topology is shared",
            ))?
            .try_reserve_additional(additional_entries, plan.additional_units)?;
        let locations = Arc::get_mut(&mut self.char_locations).ok_or(
            RegistryBuildError::DestinationInvariant("char builder locations are shared"),
        )?;
        let required_locations = final_index
            .checked_add(1)
            .ok_or(RegistryBuildError::Arithmetic("char graft location count"))?;
        if required_locations > locations.len() {
            locations
                .try_reserve(required_locations - locations.len())
                .map_err(|_| RegistryBuildError::Allocation("char graft locations"))?;
        }
        Arc::get_mut(&mut self.char_hash_index)
            .ok_or(RegistryBuildError::DestinationInvariant(
                "char builder hash index is shared",
            ))?
            .try_reserve(plan.durable_records)
            .map_err(|_| RegistryBuildError::Allocation("char graft hash index"))?;
        Arc::get_mut(&mut self.char_disk_index)
            .ok_or(RegistryBuildError::DestinationInvariant(
                "char builder disk index is shared",
            ))?
            .try_reserve(plan.durable_records)
            .map_err(|_| RegistryBuildError::Allocation("char graft disk index"))?;
        self.node_type_counts
            .try_reserve(4)
            .map_err(|_| RegistryBuildError::Allocation("char graft node-type counts"))?;
        let (last_word, _) = ResidencyState::word_and_mask(final_index);
        self.char_residency
            .ensure_word(last_word)
            .map_err(RegistryBuildError::Registration)?;

        for source_index in plan.source_range.clone() {
            let source_id = RegistryPathId(source_index);
            let offset = source_index
                .checked_sub(plan.source_root.0)
                .ok_or(RegistryBuildError::Arithmetic("char graft source offset"))?;
            let target_id = if source_id == plan.source_root {
                destination_root
            } else {
                let source_parent = source.char_paths.parent(source_id).ok_or(
                    RegistryBuildError::TopologyInvariant("char graft descendant has no parent"),
                )?;
                let parent_offset = source_parent
                    .0
                    .checked_sub(plan.source_root.0)
                    .ok_or(RegistryBuildError::Arithmetic("char graft parent offset"))?;
                let target_parent = RegistryPathId(
                    destination_root
                        .0
                        .checked_add(parent_offset)
                        .ok_or(RegistryBuildError::Arithmetic("char graft parent ID"))?,
                );
                let segment = source.char_paths.segment(source_id).ok_or(
                    RegistryBuildError::TopologyInvariant("char graft segment is unavailable"),
                )?;
                let admitted = self
                    .try_reserve_char_units(target_parent, segment)
                    .map_err(RegistryBuildError::Registration)?;
                let expected = destination_root
                    .0
                    .checked_add(offset)
                    .ok_or(RegistryBuildError::Arithmetic("char graft target ID"))?;
                if admitted.0 != expected {
                    return Err(RegistryBuildError::DestinationInvariant(
                        "char graft IDs are not contiguous",
                    ));
                }
                admitted
            };

            if let Some(entry) = source
                .char_locations
                .get(source_index)
                .and_then(Option::as_ref)
            {
                self.register_char_path_with_residency(
                    target_id,
                    entry.disk_ptr.clone(),
                    entry.size_bytes,
                    entry.depth,
                    entry.node_type,
                    source.char_is_resident(source_id),
                )
                .map_err(RegistryBuildError::Registration)?;
            }
        }

        Ok(RegistryGraftOutcome::Grafted {
            topology_entries: plan.source_range.len(),
            durable_records: plan.durable_records,
        })
    }

    /// Preflight the fallible stack growth needed to begin a byte builder
    /// subtree. Callers that must append the destination root first use this
    /// seam so `begin` cannot allocate after topology mutation.
    pub(crate) fn try_prepare_byte_builder_subtree_start(
        &mut self,
    ) -> std::result::Result<(), RegistryBuildError> {
        self.byte_builder_stack
            .try_reserve(1)
            .map_err(|_| RegistryBuildError::Allocation("byte builder-subtree stack"))
    }

    /// Begin an exact byte builder subtree while its root is the newest entry.
    pub(crate) fn try_begin_byte_builder_subtree(
        &mut self,
        root: RegistryPathId,
    ) -> std::result::Result<ByteBuilderSubtreeStart, RegistryBuildError> {
        let root_index = root.index().ok_or(RegistryBuildError::TopologyInvariant(
            "virtual root cannot begin a byte builder subtree",
        ))?;
        if root_index.checked_add(1) != Some(self.byte_paths.len()) {
            return Err(RegistryBuildError::DestinationInvariant(
                "byte builder subtree must begin at the newest topology entry",
            ));
        }
        let requested = self
            .byte_builder_stack
            .len()
            .checked_add(1)
            .ok_or(RegistryBuildError::Arithmetic("byte builder-subtree depth"))?;
        self.try_prepare_byte_builder_subtree_start()?;
        self.byte_builder_stack.push(root);
        debug_assert_eq!(self.byte_builder_stack.len(), requested);
        Ok(ByteBuilderSubtreeStart {
            generation: self.generation.clone(),
            root,
        })
    }

    /// Finish the most recently begun byte subtree in O(1) time.
    pub(crate) fn try_finish_byte_builder_subtree(
        &mut self,
        start: ByteBuilderSubtreeStart,
    ) -> std::result::Result<ByteBuilderSubtree, RegistryBuildError> {
        if !self.generation.same_publication(&start.generation) {
            return Err(RegistryBuildError::TopologyInvariant(
                "byte builder-subtree start belongs to another registry",
            ));
        }
        if self.byte_builder_stack.last().copied() != Some(start.root) {
            return Err(RegistryBuildError::TopologyInvariant(
                "byte builder subtrees did not finish in LIFO order",
            ));
        }
        if self
            .locations
            .get(start.root.0)
            .and_then(Option::as_ref)
            .is_none()
        {
            return Err(RegistryBuildError::TopologyInvariant(
                "completed byte builder-subtree root has no durable record",
            ));
        }
        let root_resident = self.byte_residency.is_resident(start.root.0);
        let end_exclusive = self.byte_paths.len();
        if end_exclusive <= start.root.0 {
            return Err(RegistryBuildError::TopologyInvariant(
                "completed byte builder-subtree range is empty",
            ));
        }
        let popped = self
            .byte_builder_stack
            .pop()
            .ok_or(RegistryBuildError::TopologyInvariant(
                "byte builder-subtree stack underflow",
            ))?;
        debug_assert_eq!(popped, start.root);
        Ok(ByteBuilderSubtree {
            generation: start.generation,
            range: BuilderSubtreeRange {
                root: start.root,
                end_exclusive,
            },
            root_resident,
        })
    }

    /// Cancel a byte subtree start after a reuse lookup reports fallback and
    /// before any descendant topology or root record was admitted.
    pub(crate) fn try_cancel_byte_builder_subtree(
        &mut self,
        start: ByteBuilderSubtreeStart,
    ) -> std::result::Result<(), RegistryBuildError> {
        if !self.generation.same_publication(&start.generation) {
            return Err(RegistryBuildError::TopologyInvariant(
                "cancelled byte builder-subtree start belongs to another registry",
            ));
        }
        if self.byte_builder_stack.last().copied() != Some(start.root)
            || start.root.0.checked_add(1) != Some(self.byte_paths.len())
            || self
                .locations
                .get(start.root.0)
                .and_then(Option::as_ref)
                .is_some()
        {
            return Err(RegistryBuildError::TopologyInvariant(
                "byte builder-subtree fallback mutated or outlived its empty destination",
            ));
        }
        let popped = self
            .byte_builder_stack
            .pop()
            .ok_or(RegistryBuildError::TopologyInvariant(
                "byte builder-subtree cancel underflow",
            ))?;
        debug_assert_eq!(popped, start.root);
        Ok(())
    }

    /// Reproduce a completed byte builder subtree at a second DAG occurrence.
    pub(crate) fn try_graft_byte_builder_subtree(
        &mut self,
        source: &ByteBuilderSubtree,
        destination_root: RegistryPathId,
        expected_root: &SwizzledPtr,
        expected_root_resident: bool,
    ) -> std::result::Result<LocalRegistryGraftStats, RegistryBuildError> {
        if !self.generation.same_publication(&source.generation) {
            return Err(RegistryBuildError::TopologyInvariant(
                "byte builder graft source belongs to another registry",
            ));
        }
        if source.root_resident != expected_root_resident {
            return Err(RegistryBuildError::TopologyInvariant(
                "byte builder graft source-root residency does not match its occurrence",
            ));
        }
        if self
            .locations
            .get(destination_root.0)
            .and_then(Option::as_ref)
            .is_some()
        {
            return Err(RegistryBuildError::DestinationInvariant(
                "byte builder graft destination is already registered",
            ));
        }
        let source_root = self
            .locations
            .get(source.range.root.0)
            .and_then(Option::as_ref)
            .ok_or(RegistryBuildError::TopologyInvariant(
                "byte builder graft source root has no durable record",
            ))?;
        if source_root.disk_ptr.to_raw() != expected_root.to_raw() {
            return Err(RegistryBuildError::TopologyInvariant(
                "byte builder graft source pointer does not match the completed node",
            ));
        }

        Arc::get_mut(&mut self.byte_paths)
            .ok_or(RegistryBuildError::DestinationInvariant(
                "byte builder topology is shared",
            ))?
            .try_graft_builder_subtree(
                source.range,
                destination_root,
                super::lru_tracker::PATH_HASH_OFFSET,
                super::lru_tracker::extend_byte_path_hash,
            )?;

        #[cfg(any(test, feature = "perf-instrumentation"))]
        let mut graft_stats = LocalRegistryGraftStats::default();
        for source_index in source.range.root.0..source.range.end_exclusive {
            let offset = source_index.checked_sub(source.range.root.0).ok_or(
                RegistryBuildError::Arithmetic("byte builder graft record offset"),
            )?;
            let target_id = RegistryPathId(destination_root.0.checked_add(offset).ok_or(
                RegistryBuildError::Arithmetic("byte builder graft record target"),
            )?);
            let source_record = self
                .locations
                .get(source_index)
                .and_then(Option::as_ref)
                .map(|entry| {
                    (
                        entry.disk_ptr.clone(),
                        entry.size_bytes,
                        entry.node_type,
                        self.byte_residency.is_resident(source_index),
                    )
                });
            let Some((disk_ptr, size_bytes, node_type, resident)) = source_record else {
                continue;
            };
            let depth = self.byte_paths.depth(target_id).ok_or(
                RegistryBuildError::DestinationInvariant(
                    "byte builder graft target depth is unavailable",
                ),
            )?;
            self.register_byte_path_with_residency(
                target_id, disk_ptr, size_bytes, depth, node_type, resident,
            )
            .map_err(RegistryBuildError::Registration)?;
            #[cfg(any(test, feature = "perf-instrumentation"))]
            {
                let (durable_records, record_overflow) =
                    graft_stats.durable_records.overflowing_add(1);
                graft_stats.durable_records = if record_overflow {
                    usize::MAX
                } else {
                    durable_records
                };
                let (serialized_bytes, byte_overflow) =
                    graft_stats.serialized_bytes.overflowing_add(size_bytes);
                graft_stats.serialized_bytes = if byte_overflow {
                    usize::MAX
                } else {
                    serialized_bytes
                };
                graft_stats.overflowed |= record_overflow || byte_overflow;
            }
        }
        #[cfg(any(test, feature = "perf-instrumentation"))]
        {
            graft_stats.appended_topology_entries = source
                .range
                .end_exclusive
                .saturating_sub(source.range.root.0)
                .saturating_sub(1);
            Ok(graft_stats)
        }
        #[cfg(not(any(test, feature = "perf-instrumentation")))]
        Ok(LocalRegistryGraftStats::default())
    }

    /// Character-key twin of
    /// [`Self::try_prepare_byte_builder_subtree_start`].
    pub(crate) fn try_prepare_char_builder_subtree_start(
        &mut self,
    ) -> std::result::Result<(), RegistryBuildError> {
        self.char_builder_stack
            .try_reserve(1)
            .map_err(|_| RegistryBuildError::Allocation("char builder-subtree stack"))
    }

    /// Begin an exact character builder subtree while its root is newest.
    pub(crate) fn try_begin_char_builder_subtree(
        &mut self,
        root: RegistryPathId,
    ) -> std::result::Result<CharBuilderSubtreeStart, RegistryBuildError> {
        let root_index = root.index().ok_or(RegistryBuildError::TopologyInvariant(
            "virtual root cannot begin a char builder subtree",
        ))?;
        if root_index.checked_add(1) != Some(self.char_paths.len()) {
            return Err(RegistryBuildError::DestinationInvariant(
                "char builder subtree must begin at the newest topology entry",
            ));
        }
        let requested = self
            .char_builder_stack
            .len()
            .checked_add(1)
            .ok_or(RegistryBuildError::Arithmetic("char builder-subtree depth"))?;
        self.try_prepare_char_builder_subtree_start()?;
        self.char_builder_stack.push(root);
        debug_assert_eq!(self.char_builder_stack.len(), requested);
        Ok(CharBuilderSubtreeStart {
            generation: self.generation.clone(),
            root,
        })
    }

    /// Finish the most recently begun character subtree in O(1) time.
    pub(crate) fn try_finish_char_builder_subtree(
        &mut self,
        start: CharBuilderSubtreeStart,
    ) -> std::result::Result<CharBuilderSubtree, RegistryBuildError> {
        if !self.generation.same_publication(&start.generation) {
            return Err(RegistryBuildError::TopologyInvariant(
                "char builder-subtree start belongs to another registry",
            ));
        }
        if self.char_builder_stack.last().copied() != Some(start.root) {
            return Err(RegistryBuildError::TopologyInvariant(
                "char builder subtrees did not finish in LIFO order",
            ));
        }
        if self
            .char_locations
            .get(start.root.0)
            .and_then(Option::as_ref)
            .is_none()
        {
            return Err(RegistryBuildError::TopologyInvariant(
                "completed char builder-subtree root has no durable record",
            ));
        }
        let root_resident = self.char_residency.is_resident(start.root.0);
        let end_exclusive = self.char_paths.len();
        if end_exclusive <= start.root.0 {
            return Err(RegistryBuildError::TopologyInvariant(
                "completed char builder-subtree range is empty",
            ));
        }
        let popped = self
            .char_builder_stack
            .pop()
            .ok_or(RegistryBuildError::TopologyInvariant(
                "char builder-subtree stack underflow",
            ))?;
        debug_assert_eq!(popped, start.root);
        Ok(CharBuilderSubtree {
            generation: start.generation,
            range: BuilderSubtreeRange {
                root: start.root,
                end_exclusive,
            },
            root_resident,
        })
    }

    /// Character-key twin of [`Self::try_cancel_byte_builder_subtree`].
    pub(crate) fn try_cancel_char_builder_subtree(
        &mut self,
        start: CharBuilderSubtreeStart,
    ) -> std::result::Result<(), RegistryBuildError> {
        if !self.generation.same_publication(&start.generation) {
            return Err(RegistryBuildError::TopologyInvariant(
                "cancelled char builder-subtree start belongs to another registry",
            ));
        }
        if self.char_builder_stack.last().copied() != Some(start.root)
            || start.root.0.checked_add(1) != Some(self.char_paths.len())
            || self
                .char_locations
                .get(start.root.0)
                .and_then(Option::as_ref)
                .is_some()
        {
            return Err(RegistryBuildError::TopologyInvariant(
                "char builder-subtree fallback mutated or outlived its empty destination",
            ));
        }
        let popped = self
            .char_builder_stack
            .pop()
            .ok_or(RegistryBuildError::TopologyInvariant(
                "char builder-subtree cancel underflow",
            ))?;
        debug_assert_eq!(popped, start.root);
        Ok(())
    }

    /// Character-key twin of [`Self::try_graft_byte_builder_subtree`].
    pub(crate) fn try_graft_char_builder_subtree(
        &mut self,
        source: &CharBuilderSubtree,
        destination_root: RegistryPathId,
        expected_root: &SwizzledPtr,
        expected_root_resident: bool,
    ) -> std::result::Result<LocalRegistryGraftStats, RegistryBuildError> {
        if !self.generation.same_publication(&source.generation) {
            return Err(RegistryBuildError::TopologyInvariant(
                "char builder graft source belongs to another registry",
            ));
        }
        if source.root_resident != expected_root_resident {
            return Err(RegistryBuildError::TopologyInvariant(
                "char builder graft source-root residency does not match its occurrence",
            ));
        }
        if self
            .char_locations
            .get(destination_root.0)
            .and_then(Option::as_ref)
            .is_some()
        {
            return Err(RegistryBuildError::DestinationInvariant(
                "char builder graft destination is already registered",
            ));
        }
        let source_root = self
            .char_locations
            .get(source.range.root.0)
            .and_then(Option::as_ref)
            .ok_or(RegistryBuildError::TopologyInvariant(
                "char builder graft source root has no durable record",
            ))?;
        if source_root.disk_ptr.to_raw() != expected_root.to_raw() {
            return Err(RegistryBuildError::TopologyInvariant(
                "char builder graft source pointer does not match the completed node",
            ));
        }

        Arc::get_mut(&mut self.char_paths)
            .ok_or(RegistryBuildError::DestinationInvariant(
                "char builder topology is shared",
            ))?
            .try_graft_builder_subtree(
                source.range,
                destination_root,
                super::lru_tracker::PATH_HASH_OFFSET,
                super::lru_tracker::extend_char_unit_hash,
            )?;

        #[cfg(any(test, feature = "perf-instrumentation"))]
        let mut graft_stats = LocalRegistryGraftStats::default();
        for source_index in source.range.root.0..source.range.end_exclusive {
            let offset = source_index.checked_sub(source.range.root.0).ok_or(
                RegistryBuildError::Arithmetic("char builder graft record offset"),
            )?;
            let target_id = RegistryPathId(destination_root.0.checked_add(offset).ok_or(
                RegistryBuildError::Arithmetic("char builder graft record target"),
            )?);
            let source_record = self
                .char_locations
                .get(source_index)
                .and_then(Option::as_ref)
                .map(|entry| {
                    (
                        entry.disk_ptr.clone(),
                        entry.size_bytes,
                        entry.node_type,
                        self.char_residency.is_resident(source_index),
                    )
                });
            let Some((disk_ptr, size_bytes, node_type, resident)) = source_record else {
                continue;
            };
            let depth = self.char_paths.depth(target_id).ok_or(
                RegistryBuildError::DestinationInvariant(
                    "char builder graft target depth is unavailable",
                ),
            )?;
            self.register_char_path_with_residency(
                target_id, disk_ptr, size_bytes, depth, node_type, resident,
            )
            .map_err(RegistryBuildError::Registration)?;
            #[cfg(any(test, feature = "perf-instrumentation"))]
            {
                let (durable_records, record_overflow) =
                    graft_stats.durable_records.overflowing_add(1);
                graft_stats.durable_records = if record_overflow {
                    usize::MAX
                } else {
                    durable_records
                };
                let (serialized_bytes, byte_overflow) =
                    graft_stats.serialized_bytes.overflowing_add(size_bytes);
                graft_stats.serialized_bytes = if byte_overflow {
                    usize::MAX
                } else {
                    serialized_bytes
                };
                graft_stats.overflowed |= record_overflow || byte_overflow;
            }
        }
        #[cfg(any(test, feature = "perf-instrumentation"))]
        {
            graft_stats.appended_topology_entries = source
                .range
                .end_exclusive
                .saturating_sub(source.range.root.0)
                .saturating_sub(1);
            Ok(graft_stats)
        }
        #[cfg(not(any(test, feature = "perf-instrumentation")))]
        Ok(LocalRegistryGraftStats::default())
    }

    /// Register a byte-level node's disk location.
    ///
    /// Called during checkpoint serialization to record where each node
    /// was written to disk.
    pub fn register(
        &mut self,
        path: Vec<u8>,
        disk_ptr: SwizzledPtr,
        size_bytes: usize,
        depth: usize,
        node_type: NodeType,
    ) {
        let hash = LruRegistry::path_hash(&path);
        let existing = self.byte_hash_index.get(&hash).and_then(|bucket| {
            bucket
                .iter()
                .copied()
                .find(|&path_id| self.byte_paths.path_equals_slice(path_id, &path))
        });
        let path_id = existing.unwrap_or_else(|| {
            self.try_reserve_independent_byte_path(&path)
                .expect("eviction registry byte path admission failed")
        });
        self.insert_byte_node(RegistryNodeAdmission {
            path_id,
            hash,
            disk_ptr,
            size_bytes,
            depth,
            node_type,
        })
        .expect("eviction registry byte record admission failed");
    }

    /// Admit one complete public-registry path as an independent root sibling.
    /// Existing finalized subtree ends remain correct, so this path stays
    /// selection-ready in O(1) time without lazy publication work.
    fn try_reserve_independent_byte_path(
        &mut self,
        path: &[u8],
    ) -> std::result::Result<RegistryPathId, &'static str> {
        Arc::get_mut(&mut self.byte_paths)
            .ok_or("eviction registry byte topology is shared")?
            .try_reserve_independent_path(
                path,
                super::lru_tracker::PATH_HASH_OFFSET,
                super::lru_tracker::extend_byte_path_hash,
            )
    }

    /// Reserve one byte segment beneath its registered parent.
    pub(crate) fn try_reserve_byte_path(
        &mut self,
        parent: RegistryPathId,
        segment: &[u8],
    ) -> std::result::Result<RegistryPathId, &'static str> {
        Arc::get_mut(&mut self.byte_paths)
            .ok_or("eviction registry byte builder topology is shared")?
            .try_reserve_path(
                parent,
                segment,
                super::lru_tracker::PATH_HASH_OFFSET,
                super::lru_tracker::extend_byte_path_hash,
            )
    }

    /// Reserve `prefix` followed by an optional edge as one canonical topology
    /// segment without materializing their concatenation.
    pub(crate) fn try_reserve_byte_path_parts(
        &mut self,
        parent: RegistryPathId,
        prefix: &[u8],
        edge: Option<u8>,
    ) -> std::result::Result<RegistryPathId, &'static str> {
        Arc::get_mut(&mut self.byte_paths)
            .ok_or("eviction registry byte builder topology is shared")?
            .try_reserve_mapped_path_with_suffix(
                parent,
                prefix,
                edge,
                super::lru_tracker::PATH_HASH_OFFSET,
                Ok::<u8, &'static str>,
                super::lru_tracker::extend_byte_path_hash,
            )
    }

    /// Register a byte node at an already-interned path.
    pub(crate) fn register_byte_path(
        &mut self,
        path_id: RegistryPathId,
        disk_ptr: SwizzledPtr,
        size_bytes: usize,
        depth: usize,
        node_type: NodeType,
    ) -> std::result::Result<(), &'static str> {
        self.register_byte_path_with_residency(
            path_id, disk_ptr, size_bytes, depth, node_type, true,
        )
    }

    /// Register a durable byte record that is represented by an `OnDisk` edge in
    /// the newly captured overlay root. Its structural metadata remains complete,
    /// but its current-root residency bit begins clear.
    #[cfg(test)]
    pub(crate) fn register_nonresident_byte_path(
        &mut self,
        path_id: RegistryPathId,
        disk_ptr: SwizzledPtr,
        size_bytes: usize,
        depth: usize,
        node_type: NodeType,
    ) -> std::result::Result<(), &'static str> {
        self.register_byte_path_with_residency(
            path_id, disk_ptr, size_bytes, depth, node_type, false,
        )
    }

    /// Atomically reserve (for descendants) and admit one record produced by
    /// the metadata-only durable scanner. Every such occurrence begins
    /// nonresident because the captured overlay reaches it through an `OnDisk`
    /// edge.
    pub(crate) fn apply_byte_scan_record(
        &mut self,
        path_or_parent: RegistryPathId,
        segment: Option<&[u8]>,
        resident: bool,
        record: &DurableRegistryRecord<u8>,
    ) -> crate::persistent_artrie::error::Result<RegistryPathId> {
        let path_id = match segment {
            Some(segment) => self
                .try_reserve_byte_path(path_or_parent, segment)
                .map_err(PersistentARTrieError::internal)?,
            None => path_or_parent,
        };
        if self
            .locations
            .get(path_id.0)
            .and_then(Option::as_ref)
            .is_some()
        {
            return Err(PersistentARTrieError::corrupted(
                "durable byte scanner attempted to register an occupied path",
            ));
        }
        let depth = self.byte_paths.depth(path_id).ok_or_else(|| {
            PersistentARTrieError::internal("durable byte scanner produced an invalid path")
        })?;
        self.register_byte_path_with_residency(
            path_id,
            record.canonical_ptr.clone(),
            record.serialized_bytes,
            depth,
            record.node_type,
            resident,
        )
        .map_err(PersistentARTrieError::internal)?;
        Ok(path_id)
    }

    fn register_byte_path_with_residency(
        &mut self,
        path_id: RegistryPathId,
        disk_ptr: SwizzledPtr,
        size_bytes: usize,
        depth: usize,
        node_type: NodeType,
        resident: bool,
    ) -> std::result::Result<(), &'static str> {
        let actual_depth = self
            .byte_paths
            .depth(path_id)
            .ok_or("eviction registry byte path identifier is invalid")?;
        if actual_depth != depth {
            return Err("eviction registry byte path depth mismatch");
        }
        let hash = self
            .byte_paths
            .hash(path_id, super::lru_tracker::PATH_HASH_OFFSET)
            .ok_or("eviction registry byte path hash is unavailable")?;
        self.insert_byte_node_with_residency(
            RegistryNodeAdmission {
                path_id,
                hash,
                disk_ptr,
                size_bytes,
                depth,
                node_type,
            },
            resident,
        )?;
        Ok(())
    }

    fn insert_byte_node(
        &mut self,
        admission: RegistryNodeAdmission,
    ) -> std::result::Result<(), &'static str> {
        self.insert_byte_node_with_residency(admission, true)
    }

    fn insert_byte_node_with_residency(
        &mut self,
        admission: RegistryNodeAdmission,
        resident: bool,
    ) -> std::result::Result<(), &'static str> {
        let RegistryNodeAdmission {
            path_id,
            hash,
            disk_ptr,
            size_bytes,
            depth,
            node_type,
        } = admission;
        let location = disk_ptr
            .disk_location()
            .ok_or("eviction registry byte record pointer is null or swizzled")?;
        if location.block_id == 0 {
            return Err("eviction registry byte record uses reserved block zero");
        }
        if location.node_type != node_type || !node_type.is_byte_level() {
            return Err("eviction registry byte record node type does not match its pointer");
        }
        if size_bytes == 0 {
            return Err("eviction registry byte record has zero serialized bytes");
        }
        let node = ByteRegistryEntry {
            path_hash: hash,
            disk_ptr,
            size_bytes,
            depth,
            node_type,
        };
        let disk_address = registry_disk_address(&node.disk_ptr)?;

        let locations = Arc::get_mut(&mut self.locations)
            .ok_or("eviction registry byte builder locations are shared")?;
        let hash_index = Arc::get_mut(&mut self.byte_hash_index)
            .ok_or("eviction registry byte builder hash index is shared")?;
        let disk_index = Arc::get_mut(&mut self.byte_disk_index)
            .ok_or("eviction registry byte builder disk index is shared")?;
        try_prepare_dense_slot(locations, path_id)?;
        let path_id_known_absent = locations.get(path_id.0).is_some_and(Option::is_none);
        let replacing_same_hash = locations
            .get(path_id.0)
            .and_then(Option::as_ref)
            .is_some_and(|old| old.path_hash == hash);
        let new_hash_bucket = if replacing_same_hash {
            None
        } else {
            try_prepare_hash_bucket(hash_index, hash, path_id, path_id_known_absent)?
        };
        let old_disk_address = match locations.get(path_id.0).and_then(Option::as_ref) {
            Some(old) => Some(registry_disk_address(&old.disk_ptr)?),
            None => None,
        };
        let replacing_same_disk = old_disk_address == Some(disk_address);
        let new_disk_bucket = if replacing_same_disk {
            None
        } else {
            try_prepare_hash_bucket(disk_index, disk_address, path_id, path_id_known_absent)?
        };
        if !self.node_type_counts.contains_key(&node_type) {
            self.node_type_counts
                .try_reserve(1)
                .map_err(|_| "eviction registry node-type counter allocation failed")?;
        }
        let slot = locations
            .get(path_id.0)
            .ok_or("eviction registry prepared byte slot is unavailable")?;
        let new_len = if slot.is_some() {
            self.byte_len
        } else {
            self.byte_len
                .checked_add(1)
                .ok_or("eviction registry byte length overflow")?
        };
        let previous_size = slot.as_ref().map_or(0, |old| old.size_bytes);
        if let Some(old) = slot.as_ref() {
            let indexed = hash_index
                .get(&old.path_hash)
                .is_some_and(|bucket| bucket.contains(&path_id));
            let disk_indexed = disk_index
                .get(&registry_disk_address(&old.disk_ptr)?)
                .is_some_and(|bucket| bucket.contains(&path_id));
            let counted = self
                .node_type_counts
                .get(&old.node_type)
                .is_some_and(|&count| count > 0);
            if !indexed || !disk_indexed || !counted {
                return Err("eviction registry byte replacement invariant violation");
            }
        }
        let new_total = self
            .total_size_bytes
            .checked_sub(previous_size)
            .and_then(|base| base.checked_add(size_bytes))
            .ok_or("eviction registry total size overflow or invariant violation")?;
        let old_serialized_bytes = slot.as_ref().map(|old| old.size_bytes);
        let residency_mark = if resident {
            Some(
                self.byte_residency
                    .prepare_mark(path_id.0, old_serialized_bytes, size_bytes)?,
            )
        } else {
            let (word, _) = ResidencyState::word_and_mask(path_id.0);
            self.byte_residency.ensure_word(word)?;
            None
        };
        let residency_clear = if resident {
            None
        } else if let Some(old_bytes) = old_serialized_bytes {
            self.byte_residency.prepare_clear(path_id.0, old_bytes)?
        } else {
            None
        };

        if let Some(bucket) = new_hash_bucket {
            hash_index.insert(hash, bucket);
        }
        if let Some(bucket) = new_disk_bucket {
            disk_index.insert(disk_address, bucket);
        }
        let slot = locations
            .get_mut(path_id.0)
            .expect("prepared byte slot remains available");
        if let Some(old) = slot.take() {
            *self
                .node_type_counts
                .get_mut(&old.node_type)
                .expect("preflighted byte node-type counter remains present") -= 1;
            if !replacing_same_hash {
                remove_hash_id(hash_index, old.path_hash, path_id);
            }
            if !replacing_same_disk {
                let old_address =
                    old_disk_address.expect("preflighted byte disk address remains available");
                remove_hash_id(disk_index, old_address, path_id);
            }
        }
        if !replacing_same_hash {
            hash_index
                .get_mut(&hash)
                .expect("prepared byte hash bucket remains available")
                .push(path_id);
        }
        if !replacing_same_disk {
            disk_index
                .get_mut(&disk_address)
                .expect("prepared byte disk bucket remains available")
                .push(path_id);
        }
        *slot = Some(node);
        self.byte_len = new_len;
        self.total_size_bytes = new_total;
        *self.node_type_counts.entry(node_type).or_insert(0) += 1;
        if let Some(residency_mark) = residency_mark {
            self.byte_residency.commit_mark(residency_mark);
        }
        if let Some(residency_clear) = residency_clear {
            self.byte_residency.commit_clear(residency_clear);
        }
        Ok(())
    }

    /// Register a char-level node's disk location.
    pub fn register_char(
        &mut self,
        path: Vec<char>,
        disk_ptr: SwizzledPtr,
        size_bytes: usize,
        depth: usize,
        node_type: NodeType,
    ) {
        let hash = super::lru_tracker::hash_char_path(&path);
        let existing = self.char_hash_index.get(&hash).and_then(|bucket| {
            bucket.iter().copied().find(|&path_id| {
                self.char_paths
                    .path_equals_mapped_slice(path_id, &path, |unit| Some(u32::from(unit)))
            })
        });
        let path_id = existing.unwrap_or_else(|| {
            self.try_reserve_independent_char_path(&path)
                .expect("eviction registry char path admission failed")
        });
        self.insert_char_node(RegistryNodeAdmission {
            path_id,
            hash,
            disk_ptr,
            size_bytes,
            depth,
            node_type,
        })
        .expect("eviction registry char record admission failed");
    }

    /// Character-key twin of [`Self::try_reserve_independent_byte_path`].
    fn try_reserve_independent_char_path(
        &mut self,
        path: &[char],
    ) -> std::result::Result<RegistryPathId, &'static str> {
        Arc::get_mut(&mut self.char_paths)
            .ok_or("eviction registry char topology is shared")?
            .try_reserve_independent_mapped_path(
                path,
                super::lru_tracker::PATH_HASH_OFFSET,
                |unit| Ok(u32::from(unit)),
                super::lru_tracker::extend_char_unit_hash,
            )
    }

    /// Reserve one char segment beneath its registered parent.
    #[cfg(test)]
    pub(crate) fn try_reserve_char_path(
        &mut self,
        parent: RegistryPathId,
        segment: &[char],
    ) -> std::result::Result<RegistryPathId, &'static str> {
        Arc::get_mut(&mut self.char_paths)
            .ok_or("eviction registry char builder topology is shared")?
            .try_reserve_mapped_path(
                parent,
                segment,
                super::lru_tracker::PATH_HASH_OFFSET,
                |unit| Ok(u32::from(unit)),
                super::lru_tracker::extend_char_unit_hash,
            )
    }

    /// Reserve a char segment supplied in the overlay's canonical `u32`
    /// representation without allocating a temporary `Vec<char>`.
    pub(crate) fn try_reserve_char_units(
        &mut self,
        parent: RegistryPathId,
        segment: &[u32],
    ) -> std::result::Result<RegistryPathId, &'static str> {
        Arc::get_mut(&mut self.char_paths)
            .ok_or("eviction registry char builder topology is shared")?
            .try_reserve_mapped_path(
                parent,
                segment,
                super::lru_tracker::PATH_HASH_OFFSET,
                |unit| {
                    char::from_u32(unit)
                        .map(|_| unit)
                        .ok_or("char overlay path contains a non-Unicode-scalar unit")
                },
                super::lru_tracker::extend_char_unit_hash,
            )
    }

    /// Character-key twin of [`Self::try_reserve_byte_path_parts`].
    pub(crate) fn try_reserve_char_units_parts(
        &mut self,
        parent: RegistryPathId,
        prefix: &[u32],
        edge: Option<u32>,
    ) -> std::result::Result<RegistryPathId, &'static str> {
        Arc::get_mut(&mut self.char_paths)
            .ok_or("eviction registry char builder topology is shared")?
            .try_reserve_mapped_path_with_suffix(
                parent,
                prefix,
                edge,
                super::lru_tracker::PATH_HASH_OFFSET,
                |unit| {
                    char::from_u32(unit)
                        .map(|_| unit)
                        .ok_or("char overlay path contains a non-Unicode-scalar unit")
                },
                super::lru_tracker::extend_char_unit_hash,
            )
    }

    /// Register a char node at an already-interned path.
    pub(crate) fn register_char_path(
        &mut self,
        path_id: RegistryPathId,
        disk_ptr: SwizzledPtr,
        size_bytes: usize,
        depth: usize,
        node_type: NodeType,
    ) -> std::result::Result<(), &'static str> {
        self.register_char_path_with_residency(
            path_id, disk_ptr, size_bytes, depth, node_type, true,
        )
    }

    /// Character-key twin of [`Self::register_nonresident_byte_path`].
    #[cfg(test)]
    pub(crate) fn register_nonresident_char_path(
        &mut self,
        path_id: RegistryPathId,
        disk_ptr: SwizzledPtr,
        size_bytes: usize,
        depth: usize,
        node_type: NodeType,
    ) -> std::result::Result<(), &'static str> {
        self.register_char_path_with_residency(
            path_id, disk_ptr, size_bytes, depth, node_type, false,
        )
    }

    /// Character-key twin of [`Self::apply_nonresident_byte_scan_record`].
    pub(crate) fn apply_char_scan_record(
        &mut self,
        path_or_parent: RegistryPathId,
        segment: Option<&[u32]>,
        resident: bool,
        record: &DurableRegistryRecord<u32>,
    ) -> crate::persistent_artrie::error::Result<RegistryPathId> {
        let path_id = match segment {
            Some(segment) => self
                .try_reserve_char_units(path_or_parent, segment)
                .map_err(|message| {
                    if message == "char overlay path contains a non-Unicode-scalar unit" {
                        PersistentARTrieError::corrupted(message)
                    } else {
                        PersistentARTrieError::internal(message)
                    }
                })?,
            None => path_or_parent,
        };
        if self
            .char_locations
            .get(path_id.0)
            .and_then(Option::as_ref)
            .is_some()
        {
            return Err(PersistentARTrieError::corrupted(
                "durable char scanner attempted to register an occupied path",
            ));
        }
        let depth = self.char_paths.depth(path_id).ok_or_else(|| {
            PersistentARTrieError::internal("durable char scanner produced an invalid path")
        })?;
        self.register_char_path_with_residency(
            path_id,
            record.canonical_ptr.clone(),
            record.serialized_bytes,
            depth,
            record.node_type,
            resident,
        )
        .map_err(PersistentARTrieError::internal)?;
        Ok(path_id)
    }

    fn register_char_path_with_residency(
        &mut self,
        path_id: RegistryPathId,
        disk_ptr: SwizzledPtr,
        size_bytes: usize,
        depth: usize,
        node_type: NodeType,
        resident: bool,
    ) -> std::result::Result<(), &'static str> {
        let actual_depth = self
            .char_paths
            .depth(path_id)
            .ok_or("eviction registry char path identifier is invalid")?;
        if actual_depth != depth {
            return Err("eviction registry char path depth mismatch");
        }
        let hash = self
            .char_paths
            .hash(path_id, super::lru_tracker::PATH_HASH_OFFSET)
            .ok_or("eviction registry char path hash is unavailable")?;
        self.insert_char_node_with_residency(
            RegistryNodeAdmission {
                path_id,
                hash,
                disk_ptr,
                size_bytes,
                depth,
                node_type,
            },
            resident,
        )?;
        Ok(())
    }

    fn insert_char_node(
        &mut self,
        admission: RegistryNodeAdmission,
    ) -> std::result::Result<(), &'static str> {
        self.insert_char_node_with_residency(admission, true)
    }

    fn insert_char_node_with_residency(
        &mut self,
        admission: RegistryNodeAdmission,
        resident: bool,
    ) -> std::result::Result<(), &'static str> {
        let RegistryNodeAdmission {
            path_id,
            hash,
            disk_ptr,
            size_bytes,
            depth,
            node_type,
        } = admission;
        let location = disk_ptr
            .disk_location()
            .ok_or("eviction registry char record pointer is null or swizzled")?;
        if location.block_id == 0 {
            return Err("eviction registry char record uses reserved block zero");
        }
        if location.node_type != node_type || !node_type.is_char_level() {
            return Err("eviction registry char record node type does not match its pointer");
        }
        if size_bytes == 0 {
            return Err("eviction registry char record has zero serialized bytes");
        }
        let node = CharRegistryEntry {
            path_hash: hash,
            disk_ptr,
            size_bytes,
            depth,
            node_type,
        };
        let disk_address = registry_disk_address(&node.disk_ptr)?;

        let locations = Arc::get_mut(&mut self.char_locations)
            .ok_or("eviction registry char builder locations are shared")?;
        let hash_index = Arc::get_mut(&mut self.char_hash_index)
            .ok_or("eviction registry char builder hash index is shared")?;
        let disk_index = Arc::get_mut(&mut self.char_disk_index)
            .ok_or("eviction registry char builder disk index is shared")?;
        try_prepare_dense_slot(locations, path_id)?;
        let path_id_known_absent = locations.get(path_id.0).is_some_and(Option::is_none);
        let replacing_same_hash = locations
            .get(path_id.0)
            .and_then(Option::as_ref)
            .is_some_and(|old| old.path_hash == hash);
        let new_hash_bucket = if replacing_same_hash {
            None
        } else {
            try_prepare_hash_bucket(hash_index, hash, path_id, path_id_known_absent)?
        };
        let old_disk_address = match locations.get(path_id.0).and_then(Option::as_ref) {
            Some(old) => Some(registry_disk_address(&old.disk_ptr)?),
            None => None,
        };
        let replacing_same_disk = old_disk_address == Some(disk_address);
        let new_disk_bucket = if replacing_same_disk {
            None
        } else {
            try_prepare_hash_bucket(disk_index, disk_address, path_id, path_id_known_absent)?
        };
        if !self.node_type_counts.contains_key(&node_type) {
            self.node_type_counts
                .try_reserve(1)
                .map_err(|_| "eviction registry node-type counter allocation failed")?;
        }
        let slot = locations
            .get(path_id.0)
            .ok_or("eviction registry prepared char slot is unavailable")?;
        let new_len = if slot.is_some() {
            self.char_len
        } else {
            self.char_len
                .checked_add(1)
                .ok_or("eviction registry char length overflow")?
        };
        let previous_size = slot.as_ref().map_or(0, |old| old.size_bytes);
        if let Some(old) = slot.as_ref() {
            let indexed = hash_index
                .get(&old.path_hash)
                .is_some_and(|bucket| bucket.contains(&path_id));
            let disk_indexed = disk_index
                .get(&registry_disk_address(&old.disk_ptr)?)
                .is_some_and(|bucket| bucket.contains(&path_id));
            let counted = self
                .node_type_counts
                .get(&old.node_type)
                .is_some_and(|&count| count > 0);
            if !indexed || !disk_indexed || !counted {
                return Err("eviction registry char replacement invariant violation");
            }
        }
        let new_total = self
            .total_size_bytes
            .checked_sub(previous_size)
            .and_then(|base| base.checked_add(size_bytes))
            .ok_or("eviction registry total size overflow or invariant violation")?;
        let old_serialized_bytes = slot.as_ref().map(|old| old.size_bytes);
        let residency_mark = if resident {
            Some(
                self.char_residency
                    .prepare_mark(path_id.0, old_serialized_bytes, size_bytes)?,
            )
        } else {
            let (word, _) = ResidencyState::word_and_mask(path_id.0);
            self.char_residency.ensure_word(word)?;
            None
        };
        let residency_clear = if resident {
            None
        } else if let Some(old_bytes) = old_serialized_bytes {
            self.char_residency.prepare_clear(path_id.0, old_bytes)?
        } else {
            None
        };

        if let Some(bucket) = new_hash_bucket {
            hash_index.insert(hash, bucket);
        }
        if let Some(bucket) = new_disk_bucket {
            disk_index.insert(disk_address, bucket);
        }
        let slot = locations
            .get_mut(path_id.0)
            .expect("prepared char slot remains available");
        if let Some(old) = slot.take() {
            *self
                .node_type_counts
                .get_mut(&old.node_type)
                .expect("preflighted char node-type counter remains present") -= 1;
            if !replacing_same_hash {
                remove_hash_id(hash_index, old.path_hash, path_id);
            }
            if !replacing_same_disk {
                let old_address =
                    old_disk_address.expect("preflighted char disk address remains available");
                remove_hash_id(disk_index, old_address, path_id);
            }
        }
        if !replacing_same_hash {
            hash_index
                .get_mut(&hash)
                .expect("prepared char hash bucket remains available")
                .push(path_id);
        }
        if !replacing_same_disk {
            disk_index
                .get_mut(&disk_address)
                .expect("prepared char disk bucket remains available")
                .push(path_id);
        }
        *slot = Some(node);
        self.char_len = new_len;
        self.total_size_bytes = new_total;
        *self.node_type_counts.entry(node_type).or_insert(0) += 1;
        if let Some(residency_mark) = residency_mark {
            self.char_residency.commit_mark(residency_mark);
        }
        if let Some(residency_clear) = residency_clear {
            self.char_residency.commit_clear(residency_clear);
        }
        Ok(())
    }

    fn materialize_byte_entry(
        &self,
        path_id: RegistryPathId,
        entry: &ByteRegistryEntry,
    ) -> Option<EvictableNode> {
        Some(EvictableNode::new(
            self.byte_paths.materialize(path_id)?,
            entry.disk_ptr.clone(),
            entry.size_bytes,
            entry.depth,
            entry.node_type,
        ))
    }

    fn materialize_char_entry(
        &self,
        path_id: RegistryPathId,
        entry: &CharRegistryEntry,
    ) -> Option<EvictableCharNode> {
        Some(EvictableCharNode::new(
            self.char_paths
                .materialize_mapped(path_id, char::from_u32)?,
            entry.disk_ptr.clone(),
            entry.size_bytes,
            entry.depth,
            entry.node_type,
        ))
    }

    /// Materialize an owned byte-level node record by path hash.
    ///
    /// Hash collisions retain insertion order; this returns the last still-live
    /// occurrence, matching [`Self::remove`]. The returned path and durable
    /// pointer are detached from the registry and remain valid after the
    /// registry is mutated or dropped.
    ///
    /// This explicit ownership boundary avoids retaining an allocated absolute
    /// path or synchronization cell in every compact registry entry. A
    /// successful lookup therefore performs one `O(depth)` path materialization
    /// and returns `None` without mutating the registry if materialization fails.
    pub fn get_owned(&self, path_hash: u64) -> Option<EvictableNode> {
        let path_id = *self.byte_hash_index.get(&path_hash)?.last()?;
        let entry = self.locations.get(path_id.0)?.as_ref()?;
        self.materialize_byte_entry(path_id, entry)
    }

    /// Materialize an owned char-level node record by path hash.
    ///
    /// This is the character-key twin of [`Self::get_owned`]. UTF-32 topology
    /// units must all decode as Unicode scalar values; a malformed unit returns
    /// `None` and leaves registry structure, residency, accounting, and
    /// authority unchanged.
    pub fn get_char_owned(&self, path_hash: u64) -> Option<EvictableCharNode> {
        let path_id = *self.char_hash_index.get(&path_hash)?.last()?;
        let entry = self.char_locations.get(path_id.0)?.as_ref()?;
        self.materialize_char_entry(path_id, entry)
    }

    /// Remove and return the last byte-level occurrence for a path hash.
    ///
    /// The owned path is materialized before the first registry mutation. If
    /// materialization fails, the operation returns `None` atomically. On
    /// success, the stored durable pointer is moved into the result rather than
    /// cloned.
    pub fn remove(&mut self, path_hash: u64) -> Option<EvictableNode> {
        let path_id = *self.byte_hash_index.get(&path_hash)?.last()?;
        let node = self.locations.get(path_id.0)?.as_ref()?;
        let node_type = node.node_type;
        let disk_address = registry_disk_address(&node.disk_ptr).ok()?;
        let new_len = self.byte_len.checked_sub(1)?;
        let new_total = self.total_size_bytes.checked_sub(node.size_bytes)?;
        let new_type_count = self.node_type_counts.get(&node_type)?.checked_sub(1)?;
        if !self
            .byte_disk_index
            .get(&disk_address)
            .is_some_and(|bucket| bucket.contains(&path_id))
        {
            return None;
        }
        let residency_clear = self
            .byte_residency
            .prepare_clear(path_id.0, node.size_bytes)
            .ok()?;
        let path = self.byte_paths.materialize(path_id)?;

        // Every fallible accounting and invariant check is complete before the
        // first mutation. The commit below performs no allocation and cannot
        // return a partially removed record.
        let locations = Arc::get_mut(&mut self.locations)?;
        let hash_index = Arc::get_mut(&mut self.byte_hash_index)?;
        let disk_index = Arc::get_mut(&mut self.byte_disk_index)?;
        let node = locations.get_mut(path_id.0)?.take()?;
        remove_hash_id(hash_index, path_hash, path_id);
        remove_hash_id(disk_index, disk_address, path_id);
        self.byte_len = new_len;
        self.total_size_bytes = new_total;
        *self
            .node_type_counts
            .get_mut(&node_type)
            .expect("preflighted byte node-type counter remains present") = new_type_count;
        if let Some(prepared) = residency_clear {
            self.byte_residency.commit_clear(prepared);
        }
        Some(EvictableNode::new(
            path,
            node.disk_ptr,
            node.size_bytes,
            node.depth,
            node.node_type,
        ))
    }

    /// Remove and return the last char-level occurrence for a path hash.
    ///
    /// This has the same materialize-before-mutation transaction boundary as
    /// [`Self::remove`], including fail-atomic handling of malformed Unicode
    /// scalar units.
    pub fn remove_char(&mut self, path_hash: u64) -> Option<EvictableCharNode> {
        let path_id = *self.char_hash_index.get(&path_hash)?.last()?;
        let node = self.char_locations.get(path_id.0)?.as_ref()?;
        let node_type = node.node_type;
        let disk_address = registry_disk_address(&node.disk_ptr).ok()?;
        let new_len = self.char_len.checked_sub(1)?;
        let new_total = self.total_size_bytes.checked_sub(node.size_bytes)?;
        let new_type_count = self.node_type_counts.get(&node_type)?.checked_sub(1)?;
        if !self
            .char_disk_index
            .get(&disk_address)
            .is_some_and(|bucket| bucket.contains(&path_id))
        {
            return None;
        }
        let residency_clear = self
            .char_residency
            .prepare_clear(path_id.0, node.size_bytes)
            .ok()?;
        let path = self
            .char_paths
            .materialize_mapped(path_id, char::from_u32)?;

        // Mirror the byte transaction: commit begins only after every
        // potentially failing check has succeeded.
        let locations = Arc::get_mut(&mut self.char_locations)?;
        let hash_index = Arc::get_mut(&mut self.char_hash_index)?;
        let disk_index = Arc::get_mut(&mut self.char_disk_index)?;
        let node = locations.get_mut(path_id.0)?.take()?;
        remove_hash_id(hash_index, path_hash, path_id);
        remove_hash_id(disk_index, disk_address, path_id);
        self.char_len = new_len;
        self.total_size_bytes = new_total;
        *self
            .node_type_counts
            .get_mut(&node_type)
            .expect("preflighted char node-type counter remains present") = new_type_count;
        if let Some(prepared) = residency_clear {
            self.char_residency.commit_clear(prepared);
        }
        Some(EvictableCharNode::new(
            path,
            node.disk_ptr,
            node.size_bytes,
            node.depth,
            node.node_type,
        ))
    }

    /// Check if a path hash is registered.
    pub fn contains(&self, path_hash: u64) -> bool {
        self.byte_hash_index.contains_key(&path_hash)
            || self.char_hash_index.contains_key(&path_hash)
    }

    /// Get the number of registered byte-level nodes.
    pub fn len(&self) -> usize {
        self.byte_len
    }

    /// Get the number of registered char-level nodes.
    pub fn char_len(&self) -> usize {
        self.char_len
    }

    /// Get the total number of registered nodes.
    pub fn total_len(&self) -> usize {
        self.byte_len.saturating_add(self.char_len)
    }

    /// Check if the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.byte_len == 0 && self.char_len == 0
    }

    /// Get the total size of tracked nodes.
    pub fn total_size_bytes(&self) -> usize {
        self.total_size_bytes
    }

    /// Get the count of nodes by type.
    pub fn count_by_type(&self, node_type: NodeType) -> usize {
        *self.node_type_counts.get(&node_type).unwrap_or(&0)
    }

    /// Check whether this registry remains structurally valid for the public
    /// compatibility inspection and materialized-candidate APIs.
    ///
    /// This preserves the historical public contract: newly constructed,
    /// cleared, and directly installed registries are valid until explicitly
    /// invalidated. Structural validity does not imply generation-qualified
    /// eviction authority; only exact checkpoint publication creates that
    /// internal authority.
    pub fn is_valid(&self) -> bool {
        self.is_compatibility_selectable()
    }

    #[inline]
    pub(super) fn is_authoritative(&self) -> bool {
        self.authority == RegistryAuthority::Valid
    }

    /// Whether this structurally complete registry can be consumed by the
    /// legacy inspection and materialized-candidate APIs. Publishing and
    /// invalid registries are excluded because their view is transitional or
    /// semantically stale; detached registries are safe because these APIs do
    /// not themselves authorize a root transition.
    #[inline]
    fn is_compatibility_selectable(&self) -> bool {
        matches!(
            self.authority,
            RegistryAuthority::Detached | RegistryAuthority::Valid
        )
    }

    /// Whether an unbound registry may enter the exact checkpoint publication
    /// transaction. This is deliberately narrower than structural usability.
    #[inline]
    pub(crate) fn is_publication_candidate(&self) -> bool {
        self.authority == RegistryAuthority::Detached
    }

    /// Remove any prior authority before installing a registry through the
    /// source-compatible direct-update API. Only prepared publication may make
    /// a registry authoritative because it also binds the exact root revision.
    #[inline]
    pub(crate) fn detach_for_direct_install(&mut self) {
        self.authority = RegistryAuthority::Detached;
    }

    /// Temporarily withhold eviction authority while a prepared checkpoint's
    /// durable stamps are installed. The coordinator lifecycle mutex ensures
    /// that replacement and invalidation cannot cross this transition.
    pub(crate) fn begin_prepared_publication(&mut self) {
        debug_assert_eq!(self.authority, RegistryAuthority::Detached);
        self.authority = RegistryAuthority::Publishing;
    }

    /// Make a successfully root-bound prepared registry authoritative after
    /// every corresponding node stamp has been installed.
    pub(crate) fn try_finish_prepared_publication(
        &mut self,
        expected: &RegistryGeneration,
    ) -> bool {
        if self.authority == RegistryAuthority::Publishing
            && self.generation.same_publication(expected)
        {
            self.authority = RegistryAuthority::Valid;
            true
        } else {
            self.authority = RegistryAuthority::Invalid;
            false
        }
    }

    /// Withdraw this cold-lifecycle registry candidate.
    ///
    /// Publication rollback and coordinator retirement use this mutable builder
    /// state while holding their lifecycle boundary. Semantic writers never call
    /// this method; their existing root CAS publishes an unbound successor.
    pub fn invalidate(&mut self) {
        self.authority = RegistryAuthority::Invalid;
    }

    /// Clear all entries and reset to detached structural state.
    pub fn clear(&mut self) {
        self.locations = Arc::new(Vec::new());
        self.char_locations = Arc::new(Vec::new());
        self.byte_hash_index = Arc::new(HashMap::new());
        self.char_hash_index = Arc::new(HashMap::new());
        self.byte_disk_index = Arc::new(HashMap::new());
        self.char_disk_index = Arc::new(HashMap::new());
        self.byte_len = 0;
        self.char_len = 0;
        self.byte_paths = Arc::new(PathTopology::new());
        self.char_paths = Arc::new(PathTopology::new());
        self.byte_builder_stack.clear();
        self.char_builder_stack.clear();
        self.byte_residency.clear();
        self.char_residency.clear();
        self.generation = RegistryGeneration::new();
        self.total_size_bytes = 0;
        self.node_type_counts.clear();
        self.authority = RegistryAuthority::Detached;
    }

    /// Get an iterator over all byte-level path hashes.
    pub fn path_hashes(&self) -> impl Iterator<Item = u64> + '_ {
        self.locations
            .iter()
            .filter_map(|entry| entry.as_ref().map(|node| node.path_hash))
    }

    /// Get an iterator over all char-level path hashes.
    pub fn char_path_hashes(&self) -> impl Iterator<Item = u64> + '_ {
        self.char_locations
            .iter()
            .filter_map(|entry| entry.as_ref().map(|node| node.path_hash))
    }

    /// Get candidates for eviction, filtered by minimum depth.
    ///
    /// Returns path hashes of nodes at or below `min_depth`.
    pub fn eviction_candidates(&self, min_depth: usize) -> Vec<u64> {
        if !self.is_compatibility_selectable() {
            return Vec::new();
        }
        self.locations
            .iter()
            .enumerate()
            .filter_map(|(path_id, entry)| {
                entry
                    .as_ref()
                    .filter(|node| {
                        node.depth >= min_depth && self.byte_residency.is_resident(path_id)
                    })
                    .map(|node| node.path_hash)
            })
            .collect()
    }

    /// Get char candidates for eviction, filtered by minimum depth.
    pub fn char_eviction_candidates(&self, min_depth: usize) -> Vec<u64> {
        if !self.is_compatibility_selectable() {
            return Vec::new();
        }
        self.char_locations
            .iter()
            .enumerate()
            .filter_map(|(path_id, entry)| {
                entry
                    .as_ref()
                    .filter(|node| {
                        node.depth >= min_depth && self.char_residency.is_resident(path_id)
                    })
                    .map(|node| node.path_hash)
            })
            .collect()
    }

    pub(crate) fn select_compact_for_eviction(
        &self,
        target_bytes: usize,
        lru_registry: &LruRegistry,
        min_depth: usize,
        max_count: usize,
        overhead: usize,
    ) -> CompactEvictionBatch<u8> {
        self.select_compact_for_eviction_with(
            self.is_authoritative(),
            target_bytes,
            lru_registry,
            min_depth,
            max_count,
            overhead,
        )
    }

    pub(crate) fn select_compact_for_compatibility(
        &self,
        target_bytes: usize,
        lru_registry: &LruRegistry,
        min_depth: usize,
        max_count: usize,
        overhead: usize,
    ) -> CompactEvictionBatch<u8> {
        self.select_compact_for_eviction_with(
            self.is_compatibility_selectable(),
            target_bytes,
            lru_registry,
            min_depth,
            max_count,
            overhead,
        )
    }

    fn select_compact_for_eviction_with(
        &self,
        selectable: bool,
        target_bytes: usize,
        lru_registry: &LruRegistry,
        min_depth: usize,
        max_count: usize,
        overhead: usize,
    ) -> CompactEvictionBatch<u8> {
        match try_select_compact_batch(
            selectable,
            &self.byte_paths,
            &self.generation,
            self.locations
                .iter()
                .enumerate()
                .filter_map(|(path_id, entry)| {
                    entry.as_ref().and_then(|node| {
                        (node.depth != 0
                            && node.depth >= min_depth
                            && self.byte_residency.is_resident(path_id))
                        .then(|| CompactEvictionCandidate {
                            path_id: RegistryPathId(path_id),
                            path_hash: node.path_hash,
                            disk_ptr: node.disk_ptr.clone(),
                            size_bytes: node.size_bytes,
                            depth: node.depth,
                            node_type: node.node_type,
                        })
                    })
                }),
            lru_registry,
            CompactSelectionLimits {
                target_bytes,
                max_count,
                overhead,
            },
        ) {
            Ok(batch) => batch,
            Err(error) => {
                log::error!("byte eviction selection failed closed: {error}");
                empty_compact_batch(
                    &self.byte_paths,
                    &self.generation,
                    CompactEvictionPolicy::DescendantFirst,
                )
            }
        }
    }

    pub(crate) fn select_compact_char_for_eviction(
        &self,
        target_bytes: usize,
        lru_registry: &LruRegistry,
        min_depth: usize,
        max_count: usize,
        overhead: usize,
    ) -> CompactEvictionBatch<u32> {
        self.select_compact_char_for_eviction_with(
            self.is_authoritative(),
            target_bytes,
            lru_registry,
            min_depth,
            max_count,
            overhead,
        )
    }

    pub(crate) fn select_compact_char_for_compatibility(
        &self,
        target_bytes: usize,
        lru_registry: &LruRegistry,
        min_depth: usize,
        max_count: usize,
        overhead: usize,
    ) -> CompactEvictionBatch<u32> {
        self.select_compact_char_for_eviction_with(
            self.is_compatibility_selectable(),
            target_bytes,
            lru_registry,
            min_depth,
            max_count,
            overhead,
        )
    }

    fn select_compact_char_for_eviction_with(
        &self,
        selectable: bool,
        target_bytes: usize,
        lru_registry: &LruRegistry,
        min_depth: usize,
        max_count: usize,
        overhead: usize,
    ) -> CompactEvictionBatch<u32> {
        match try_select_compact_batch(
            selectable,
            &self.char_paths,
            &self.generation,
            self.char_locations
                .iter()
                .enumerate()
                .filter_map(|(path_id, entry)| {
                    entry.as_ref().and_then(|node| {
                        (node.depth != 0
                            && node.depth >= min_depth
                            && self.char_residency.is_resident(path_id))
                        .then(|| CompactEvictionCandidate {
                            path_id: RegistryPathId(path_id),
                            path_hash: node.path_hash,
                            disk_ptr: node.disk_ptr.clone(),
                            size_bytes: node.size_bytes,
                            depth: node.depth,
                            node_type: node.node_type,
                        })
                    })
                }),
            lru_registry,
            CompactSelectionLimits {
                target_bytes,
                max_count,
                overhead,
            },
        ) {
            Ok(batch) => batch,
            Err(error) => {
                log::error!("char eviction selection failed closed: {error}");
                empty_compact_batch(
                    &self.char_paths,
                    &self.generation,
                    CompactEvictionPolicy::DescendantFirst,
                )
            }
        }
    }

    /// Select nodes for eviction up to a target size.
    ///
    /// Uses the LRU registry to prioritize cold nodes. Returns a list
    /// of (path_hash, EvictableNode) pairs.
    ///
    /// # Arguments
    ///
    /// * `target_bytes` - Target amount of memory to free
    /// * `lru_registry` - LRU registry for coldness scoring
    /// * `min_depth` - Minimum depth to evict
    /// * `max_count` - Maximum number of nodes to return
    /// * `overhead` - per-node residual added to each node's on-disk `size_bytes`
    ///   when accumulating toward `target_bytes`. Pass `0` for an ON-DISK-unit target
    ///   (the async/public-batch path); pass `STRUCT_OVERHEAD_BYTE` for a RESIDENT-unit
    ///   target (the checkpoint-tail budget path) so the accumulation matches a
    ///   resident-bytes `target_bytes` computed via [`Self::byte_resident_estimate_bytes`].
    pub fn select_for_eviction(
        &self,
        target_bytes: usize,
        lru_registry: &LruRegistry,
        min_depth: usize,
        max_count: usize,
        overhead: usize,
    ) -> Vec<(u64, EvictableNode)> {
        let batch = self.select_compact_for_compatibility(
            target_bytes,
            lru_registry,
            min_depth,
            max_count,
            overhead,
        );
        batch
            .candidates
            .iter()
            .filter_map(|candidate| {
                batch.materialize_path(candidate.path_id).map(|path| {
                    (
                        candidate.path_hash,
                        EvictableNode::new(
                            path,
                            candidate.disk_ptr.clone(),
                            candidate.size_bytes,
                            candidate.depth,
                            candidate.node_type,
                        ),
                    )
                })
            })
            .collect()
    }

    /// Select char nodes for eviction up to a target size. `overhead` matches
    /// [`Self::select_for_eviction`]: `0` for an on-disk-unit target, `STRUCT_OVERHEAD_CHAR`
    /// for a resident-unit target (the checkpoint-tail budget path).
    pub fn select_char_for_eviction(
        &self,
        target_bytes: usize,
        lru_registry: &LruRegistry,
        min_depth: usize,
        max_count: usize,
        overhead: usize,
    ) -> Vec<(u64, EvictableCharNode)> {
        let batch = self.select_compact_char_for_compatibility(
            target_bytes,
            lru_registry,
            min_depth,
            max_count,
            overhead,
        );
        batch
            .candidates
            .iter()
            .filter_map(|candidate| {
                batch
                    .topology
                    .materialize_mapped(candidate.path_id, char::from_u32)
                    .map(|path| {
                        (
                            candidate.path_hash,
                            EvictableCharNode::new(
                                path,
                                candidate.disk_ptr.clone(),
                                candidate.size_bytes,
                                candidate.depth,
                                candidate.node_type,
                            ),
                        )
                    })
            })
            .collect()
    }

    /// Exact number of byte durable-record occurrences represented by resident
    /// nodes in the currently published overlay root.
    pub fn byte_resident_len(&self) -> usize {
        self.byte_residency.resident_nodes()
    }

    /// Exact serialized-equivalent byte total for resident byte records.
    ///
    /// This is a logical, allocator-independent measure. It intentionally does
    /// not claim to measure `Arc`, allocator, child-store, or retained-snapshot
    /// memory.
    pub fn byte_resident_serialized_bytes(&self) -> usize {
        self.byte_residency.resident_serialized_bytes()
    }

    /// Exact number of char durable-record occurrences represented by resident
    /// nodes in the currently published overlay root.
    pub fn char_resident_len(&self) -> usize {
        self.char_residency.resident_nodes()
    }

    /// Exact serialized-equivalent byte total for resident char records.
    pub fn char_resident_serialized_bytes(&self) -> usize {
        self.char_residency.resident_serialized_bytes()
    }

    fn resident_soft_estimate(
        residency: &ResidencyState,
        per_node_overhead: usize,
    ) -> Option<usize> {
        residency
            .resident_nodes()
            .checked_mul(per_node_overhead)
            .and_then(|overhead| residency.resident_serialized_bytes().checked_add(overhead))
    }

    /// Resident-heap soft estimate over resident BYTE records only.
    ///
    /// The exact serialized component and exact logical resident count come
    /// from [`Self::byte_resident_serialized_bytes`] and
    /// [`Self::byte_resident_len`]. `STRUCT_OVERHEAD_BYTE` remains an explicitly
    /// approximate physical residual until allocator instrumentation provides a
    /// measured footprint. If the estimate is not representable in `usize`, the
    /// result fails conservatively to `usize::MAX` so a memory budget can never
    /// mistake overflow for low residency.
    pub fn byte_resident_estimate_bytes(&self) -> usize {
        Self::resident_soft_estimate(&self.byte_residency, STRUCT_OVERHEAD_BYTE).unwrap_or_else(
            || {
                log::error!("byte resident soft estimate is not representable in usize");
                usize::MAX
            },
        )
    }

    /// Resident-heap soft estimate over resident CHAR records only. See
    /// [`Self::byte_resident_estimate_bytes`] for its exact and approximate
    /// components and overflow semantics.
    pub fn char_resident_estimate_bytes(&self) -> usize {
        Self::resident_soft_estimate(&self.char_residency, STRUCT_OVERHEAD_CHAR).unwrap_or_else(
            || {
                log::error!("char resident soft estimate is not representable in usize");
                usize::MAX
            },
        )
    }
}

fn scored_candidate_order_key(
    scored: &ScoredCompactCandidate,
) -> (Reverse<u64>, Reverse<usize>, usize) {
    (
        Reverse(scored.coldness),
        Reverse(scored.candidate.depth),
        scored.candidate.path_id.0,
    )
}

fn sort_coldest(candidates: &mut ScoredCompactCandidateBuffer) {
    candidates.sort_unstable_by_key(scored_candidate_order_key);
}

fn retain_coldest_prefix(candidates: &mut ScoredCompactCandidateBuffer, max_count: usize) {
    let limit = max_count.min(candidates.len());
    if limit == 0 {
        candidates.clear();
        return;
    }

    if limit < candidates.len() / 4 {
        candidates.select_nth_unstable_by_key(limit - 1, scored_candidate_order_key);
        candidates.truncate(limit);
    }

    sort_coldest(candidates);
    candidates.truncate(limit);
}

impl Default for DiskLocationRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::{prop_assert, prop_assert_eq};

    #[allow(dead_code)]
    struct CompactRegistryEntryLayout {
        path_hash: u64,
        disk_ptr: SwizzledPtr,
        size_bytes: usize,
        depth: usize,
        node_type: NodeType,
    }

    #[test]
    fn compact_registry_entries_do_not_reserve_materialized_compatibility_storage() {
        let compact_size = std::mem::size_of::<CompactRegistryEntryLayout>();
        assert_eq!(std::mem::size_of::<ByteRegistryEntry>(), compact_size);
        assert_eq!(std::mem::size_of::<CharRegistryEntry>(), compact_size);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn compact_registry_dense_slots_pin_the_64_bit_memory_budget() {
        assert_eq!(std::mem::align_of::<ByteRegistryEntry>(), 8);
        assert_eq!(std::mem::align_of::<CharRegistryEntry>(), 8);
        assert_eq!(std::mem::size_of::<ByteRegistryEntry>(), 48);
        assert_eq!(std::mem::size_of::<CharRegistryEntry>(), 48);
        // The entry retains a niche, so the dense table's optional slot adds no
        // discriminant storage beyond the compact record itself.
        assert_eq!(std::mem::size_of::<Option<ByteRegistryEntry>>(), 48);
        assert_eq!(std::mem::size_of::<Option<CharRegistryEntry>>(), 48);
    }

    fn make_disk_ptr(block_id: u32, offset: u32) -> SwizzledPtr {
        SwizzledPtr::on_disk(block_id, offset, NodeType::Node16)
    }

    fn make_typed_disk_ptr(block_id: u32, offset: u32, node_type: NodeType) -> SwizzledPtr {
        SwizzledPtr::on_disk(block_id, offset, node_type)
    }

    fn test_admission(
        path_id: RegistryPathId,
        hash: u64,
        disk_ptr: SwizzledPtr,
        size_bytes: usize,
        depth: usize,
        node_type: NodeType,
    ) -> RegistryNodeAdmission {
        RegistryNodeAdmission {
            path_id,
            hash,
            disk_ptr,
            size_bytes,
            depth,
            node_type,
        }
    }

    fn make_authoritative(registry: &mut DiskLocationRegistry) {
        registry
            .try_finalize_for_publication()
            .expect("finalize registry before test publication");
        assert!(registry.is_publication_candidate());
        let binding = registry.binding();
        registry.begin_prepared_publication();
        assert!(registry.try_finish_prepared_publication(&binding));
        assert!(registry.is_valid());
    }

    #[test]
    fn streamed_eviction_ranges_collapse_ancestors_duplicates_and_adjacency() {
        let mut topology = PathTopology::<u8>::new();
        let a = topology
            .try_reserve_path(RegistryPathId::ROOT, b"a", 0, |hash, unit| {
                hash ^ u64::from(unit)
            })
            .expect("reserve a");
        let a_child = topology
            .try_reserve_path(a, b"x", 0, |hash, unit| hash ^ u64::from(unit))
            .expect("reserve a child");
        let b = topology
            .try_reserve_path(RegistryPathId::ROOT, b"b", 0, |hash, unit| {
                hash ^ u64::from(unit)
            })
            .expect("reserve b");
        let b_child = topology
            .try_reserve_path(b, b"y", 0, |hash, unit| hash ^ u64::from(unit))
            .expect("reserve b child");
        let c = topology
            .try_reserve_path(RegistryPathId::ROOT, b"c", 0, |hash, unit| {
                hash ^ u64::from(unit)
            })
            .expect("reserve c");
        while topology.len() <= 32 {
            let unit = u8::try_from(topology.len()).expect("test topology fits in u8");
            topology
                .try_reserve_path(RegistryPathId::ROOT, &[unit], 0, |hash, unit| {
                    hash ^ u64::from(unit)
                })
                .expect("reserve boundary sibling");
        }
        topology
            .try_finalize_subtree_ends()
            .expect("finalize test topology");
        let topology = Arc::new(topology);

        let selected_paths = [a, a_child, b, c, RegistryPathId(30), RegistryPathId(32)];
        let mut candidates = CompactCandidateBuffer::new();
        for (offset, path_id) in selected_paths.into_iter().enumerate() {
            candidates.push(CompactEvictionCandidate {
                path_id,
                path_hash: offset as u64,
                disk_ptr: make_disk_ptr(1, offset as u32),
                size_bytes: 1,
                depth: 1,
                node_type: NodeType::Node4,
            });
        }
        let batch = CompactEvictionBatch {
            topology: Arc::clone(&topology),
            generation: RegistryGeneration::new(),
            candidates,
            policy: CompactEvictionPolicy::DescendantFirst,
            report: CompactSelectionReport::default(),
        };

        // Deliberately unsorted, including a duplicate successful endpoint and
        // an ancestor/descendant pair. This is the exact buffer shape accepted
        // by packed preparation before its in-place preorder sort.
        let mut successful = [5usize, 3, 1, 0, 0, 4];
        successful.sort_unstable_by_key(|&index| batch.candidates[index].path_id.0);
        let mut ranges = Vec::new();
        try_for_each_merged_eviction_range(&topology, &batch, &successful, |range| {
            ranges.push(range);
            Ok(())
        })
        .expect("stream disjoint and overlapping ranges");
        assert_eq!(ranges, vec![0..2, 4..5, 30..31, 32..33]);

        let mut adjacent = [3usize, 2, 0];
        adjacent.sort_unstable_by_key(|&index| batch.candidates[index].path_id.0);
        ranges.clear();
        try_for_each_merged_eviction_range(&topology, &batch, &adjacent, |range| {
            ranges.push(range);
            Ok(())
        })
        .expect("stream adjacent ranges");
        assert_eq!(ranges, vec![0..5]);

        assert_eq!(topology_coverage_mask(&(30..33), 0), 0xc000_0000);
        assert_eq!(topology_coverage_mask(&(30..33), 1), 0x0000_0001);
        assert_eq!(topology.subtree_range(b_child), Some(3..4));
    }

    #[test]
    fn packed_transition_storage_scales_with_affected_words_not_covered_words() {
        let transition = |word| PackedResidencyTransition {
            word,
            expected: word as u64,
            target: word as u64 + 1,
        };

        let mut sparse = PackedTransitionBuilder::new();
        sparse
            .try_push(transition(0))
            .expect("store inline transition");
        sparse
            .try_push(transition(32_767))
            .expect("store far second transition");
        assert_eq!(sparse.many.len(), 2);
        assert!(
            sparse.many.capacity() < 64,
            "two affected words must not reserve by the 32,768-word coverage span"
        );

        let mut dense = PackedTransitionBuilder::new();
        for word in 0..65 {
            dense
                .try_push(transition(word))
                .expect("grow transition storage geometrically");
        }
        assert_eq!(dense.many.len(), 65);
        assert!(
            dense.many.capacity() < 130,
            "geometric retained capacity must remain below twice live transitions"
        );
    }

    fn byte_selection_snapshot(registry: &DiskLocationRegistry) -> ByteRegistrySelectionSnapshot {
        PublishedRegistryCatalog::try_from_builder(registry)
            .expect("build byte selection catalog")
            .try_byte_selection_snapshot(0)
            .expect("capture byte selection")
    }

    fn char_selection_snapshot(registry: &DiskLocationRegistry) -> CharRegistrySelectionSnapshot {
        PublishedRegistryCatalog::try_from_builder(registry)
            .expect("build char selection catalog")
            .try_char_selection_snapshot(0)
            .expect("capture char selection")
    }
    #[test]
    fn empty_path_topology_is_immediately_finalized() {
        let topology = PathTopology::<u8>::new();

        assert!(topology.is_finalized());
        assert_eq!(topology.try_finalized_subtree_ends(), Ok(&[][..]));
        assert_eq!(topology.subtree_ends(), Some(&[][..]));
    }

    #[test]
    fn public_full_path_registration_remains_directly_selection_ready() {
        let mut registry = DiskLocationRegistry::new();
        registry.register(
            b"byte".to_vec(),
            make_typed_disk_ptr(1, 8, NodeType::Node4),
            11,
            4,
            NodeType::Node4,
        );
        registry.register_char(
            vec!['計'],
            make_typed_disk_ptr(2, 8, NodeType::CharNode4),
            13,
            1,
            NodeType::CharNode4,
        );

        assert!(registry.topologies_are_finalized());
        assert_eq!(
            registry
                .select_for_eviction(usize::MAX, &LruRegistry::new(), 0, usize::MAX, 0)
                .len(),
            1
        );
        assert_eq!(
            registry
                .select_char_for_eviction(usize::MAX, &LruRegistry::new(), 0, usize::MAX, 0)
                .len(),
            1
        );
    }

    #[test]
    fn path_topology_finalization_is_iterative_and_ranges_are_immutable() {
        const DEEP_ENTRIES: usize = 4_096;
        const WIDE_ENTRIES: usize = 2_048;

        let mut topology = PathTopology::with_capacity(DEEP_ENTRIES + WIDE_ENTRIES);
        let mut parent = RegistryPathId::ROOT;
        for _ in 0..DEEP_ENTRIES {
            parent = topology
                .try_reserve_path(
                    parent,
                    b"d",
                    super::super::lru_tracker::PATH_HASH_OFFSET,
                    super::super::lru_tracker::extend_byte_path_hash,
                )
                .expect("deep topology entry");
        }
        let first_wide = topology.len();
        for index in 0..WIDE_ENTRIES {
            let unit = [u8::try_from(index & 0xff).expect("masked byte")];
            topology
                .try_reserve_path(
                    RegistryPathId::ROOT,
                    &unit,
                    super::super::lru_tracker::PATH_HASH_OFFSET,
                    super::super::lru_tracker::extend_byte_path_hash,
                )
                .expect("wide topology entry");
        }

        assert!(!topology.is_finalized());
        assert!(topology.subtree_ends().is_none());
        topology
            .try_finalize_subtree_ends()
            .expect("iterative topology finalization");
        assert!(topology.is_finalized());
        assert_eq!(
            topology.subtree_range(RegistryPathId(0)),
            Some(0..DEEP_ENTRIES)
        );
        assert_eq!(
            topology.subtree_range(RegistryPathId(DEEP_ENTRIES - 1)),
            Some(DEEP_ENTRIES - 1..DEEP_ENTRIES)
        );
        assert_eq!(
            topology.subtree_range(RegistryPathId(first_wide)),
            Some(first_wide..first_wide + 1)
        );

        let previous_len = topology.len();
        topology
            .try_reserve_path(
                RegistryPathId::ROOT,
                b"z",
                super::super::lru_tracker::PATH_HASH_OFFSET,
                super::super::lru_tracker::extend_byte_path_hash,
            )
            .expect("extend finalized topology");
        assert!(!topology.is_finalized());
        topology
            .try_finalize_subtree_ends()
            .expect("refinalize extended topology");
        assert_eq!(
            topology.subtree_range(RegistryPathId(previous_len)),
            Some(previous_len..previous_len + 1)
        );
    }

    #[test]
    fn reusable_materialization_preserves_segments_reuses_capacity_and_clears_on_failure() {
        let mut topology = PathTopology::<u32>::new();
        let first = topology
            .try_reserve_path(
                RegistryPathId::ROOT,
                &[u32::from('λ'), u32::from('日')],
                0,
                |hash, unit| hash.wrapping_mul(31).wrapping_add(u64::from(unit)),
            )
            .expect("reserve first Unicode segment");
        let deepest = topology
            .try_reserve_path(
                first,
                &[u32::from('本'), u32::from('語')],
                0,
                |hash, unit| hash.wrapping_mul(31).wrapping_add(u64::from(unit)),
            )
            .expect("reserve second Unicode segment");
        let invalid = topology
            .try_reserve_path(RegistryPathId::ROOT, &[0xD800], 0, |hash, unit| {
                hash.wrapping_mul(31).wrapping_add(u64::from(unit))
            })
            .expect("reserve invalid scalar fixture");

        let mut scratch = SmallVec::<[char; 2]>::new();
        topology
            .materialize_mapped_into(deepest, &mut scratch, char::from_u32)
            .expect("materialize segmented Unicode path");
        assert_eq!(scratch.as_slice(), &['λ', '日', '本', '語']);
        let spilled_capacity = scratch.capacity();

        topology
            .materialize_mapped_into(first, &mut scratch, char::from_u32)
            .expect("reuse scratch for shallower path");
        assert_eq!(scratch.as_slice(), &['λ', '日']);
        assert_eq!(scratch.capacity(), spilled_capacity);

        assert!(topology
            .materialize_mapped_into(invalid, &mut scratch, char::from_u32)
            .is_none());
        assert!(scratch.is_empty());
    }

    #[test]
    fn compact_selection_excludes_the_non_evictable_root_endpoint() {
        let mut registry = DiskLocationRegistry::new();
        registry.register_char(
            Vec::new(),
            make_typed_disk_ptr(1, 190, NodeType::CharNode4),
            16,
            0,
            NodeType::CharNode4,
        );
        registry.register_char(
            vec!['x'],
            make_typed_disk_ptr(1, 191, NodeType::CharNode4),
            16,
            1,
            NodeType::CharNode4,
        );
        make_authoritative(&mut registry);

        let batch = registry.select_compact_char_for_eviction(
            usize::MAX,
            &LruRegistry::new(),
            0,
            usize::MAX,
            0,
        );
        assert_eq!(batch.candidates.len(), 1);
        assert_eq!(batch.candidates[0].depth, 1);
        assert_eq!(
            batch.materialize_char_path(batch.candidates[0].path_id),
            Some(vec!['x'])
        );
    }

    #[test]
    fn resident_budget_selector_uses_exact_chain_closure_and_reports_caps() {
        let mut registry = DiskLocationRegistry::with_capacity(3);
        let a = registry
            .try_reserve_byte_path(RegistryPathId::ROOT, b"a")
            .expect("reserve a");
        let ab = registry.try_reserve_byte_path(a, b"b").expect("reserve ab");
        let abc = registry
            .try_reserve_byte_path(ab, b"c")
            .expect("reserve abc");
        for (path, depth, offset) in [(a, 1, 10), (ab, 2, 20), (abc, 3, 30)] {
            registry
                .register_byte_path(
                    path,
                    make_typed_disk_ptr(1, offset, NodeType::Node4),
                    100,
                    depth,
                    NodeType::Node4,
                )
                .expect("register chain record");
        }
        make_authoritative(&mut registry);
        let lru = LruRegistry::new();

        let snapshot = byte_selection_snapshot(&registry);
        let batch = snapshot.select_resident_budget(200, &lru, 0, usize::MAX, 0);
        assert_eq!(
            batch.policy,
            CompactEvictionPolicy::ResidentBudgetAncestorClosure
        );
        assert_eq!(batch.report.planned_bytes, 200);
        assert_eq!(batch.report.eligible_candidates, 3);
        assert_eq!(batch.report.nonredundant_candidates, 3);
        assert_eq!(batch.report.selected_priority_count, 2);
        assert!(!batch.report.cap_exhausted);
        assert!(!batch.report.eligible_exhausted);
        let selected_paths: Vec<Vec<u8>> = batch
            .candidates
            .iter()
            .map(|candidate| {
                batch
                    .materialize_path(candidate.path_id)
                    .expect("selected chain path")
            })
            .collect();
        assert_eq!(selected_paths, [b"abc".to_vec(), b"ab".to_vec()]);

        let capped = byte_selection_snapshot(&registry).select_resident_budget(200, &lru, 0, 1, 0);
        assert_eq!(capped.report.planned_bytes, 100);
        assert_eq!(capped.report.selected_priority_count, 1);
        assert!(capped.report.cap_exhausted);
        assert!(!capped.report.eligible_exhausted);

        let pinned = byte_selection_snapshot(&registry).select_resident_budget(
            usize::MAX,
            &lru,
            2,
            usize::MAX,
            0,
        );
        assert_eq!(pinned.report.planned_bytes, 200);
        assert_eq!(pinned.report.eligible_candidates, 2);
        assert!(!pinned.report.cap_exhausted);
        assert!(pinned.report.eligible_exhausted);
    }

    #[test]
    fn resident_budget_subtree_priority_protects_a_hot_descendant() {
        let mut registry = DiskLocationRegistry::with_capacity(3);
        let a = registry
            .try_reserve_byte_path(RegistryPathId::ROOT, b"a")
            .expect("reserve a");
        let ab = registry.try_reserve_byte_path(a, b"b").expect("reserve ab");
        let x = registry
            .try_reserve_byte_path(RegistryPathId::ROOT, b"x")
            .expect("reserve x");
        for (path, depth, offset) in [(a, 1, 40), (ab, 2, 50), (x, 1, 60)] {
            registry
                .register_byte_path(
                    path,
                    make_typed_disk_ptr(1, offset, NodeType::Node4),
                    100,
                    depth,
                    NodeType::Node4,
                )
                .expect("register priority record");
        }
        make_authoritative(&mut registry);
        let lru = LruRegistry::new();
        lru.touch_hash(LruRegistry::path_hash(b"ab"));

        let batch = byte_selection_snapshot(&registry).select_resident_budget(100, &lru, 0, 1, 0);
        assert_eq!(batch.candidates.len(), 1);
        assert_eq!(
            batch.materialize_path(batch.candidates[0].path_id),
            Some(b"x".to_vec()),
            "the untracked sibling must rank colder than a subtree containing a hot descendant"
        );
    }

    #[test]
    fn concrete_child_segment_must_strictly_advance_topology_depth() {
        let mut topology = PathTopology::<u8>::new();
        let concrete_root = topology
            .try_reserve_path(
                RegistryPathId::ROOT,
                &[],
                super::super::lru_tracker::PATH_HASH_OFFSET,
                super::super::lru_tracker::extend_byte_path_hash,
            )
            .expect("the virtual root may own an empty concrete root record");
        let entries_before = topology.entries.len();
        let units_before = topology.units.len();
        let error = topology
            .try_reserve_path(
                concrete_root,
                &[],
                super::super::lru_tracker::PATH_HASH_OFFSET,
                super::super::lru_tracker::extend_byte_path_hash,
            )
            .expect_err("a concrete child must not reuse its parent's depth");

        assert_eq!(error, "eviction registry concrete child segment is empty");
        assert_eq!(topology.entries.len(), entries_before);
        assert_eq!(topology.units.len(), units_before);
        assert_eq!(topology.depth(concrete_root), Some(0));
    }

    #[test]
    fn resident_budget_byte_and_char_selectors_have_matching_closure_semantics() {
        let mut registry = DiskLocationRegistry::with_capacity(4);
        let byte_a = registry
            .try_reserve_byte_path(RegistryPathId::ROOT, b"a")
            .expect("reserve byte a");
        let byte_ab = registry
            .try_reserve_byte_path(byte_a, b"b")
            .expect("reserve byte ab");
        let char_a = registry
            .try_reserve_char_units(RegistryPathId::ROOT, &[u32::from('a')])
            .expect("reserve char a");
        let char_ab = registry
            .try_reserve_char_units(char_a, &[u32::from('b')])
            .expect("reserve char ab");
        registry
            .register_byte_path(
                byte_a,
                make_typed_disk_ptr(1, 70, NodeType::Node4),
                90,
                1,
                NodeType::Node4,
            )
            .expect("register byte a");
        registry
            .register_byte_path(
                byte_ab,
                make_typed_disk_ptr(1, 80, NodeType::Node4),
                110,
                2,
                NodeType::Node4,
            )
            .expect("register byte ab");
        registry
            .register_char_path(
                char_a,
                make_typed_disk_ptr(2, 70, NodeType::CharNode4),
                90,
                1,
                NodeType::CharNode4,
            )
            .expect("register char a");
        registry
            .register_char_path(
                char_ab,
                make_typed_disk_ptr(2, 80, NodeType::CharNode4),
                110,
                2,
                NodeType::CharNode4,
            )
            .expect("register char ab");
        make_authoritative(&mut registry);
        let lru = LruRegistry::new();
        let byte =
            byte_selection_snapshot(&registry).select_resident_budget(150, &lru, 0, usize::MAX, 0);
        let chars =
            char_selection_snapshot(&registry).select_resident_budget(150, &lru, 0, usize::MAX, 0);

        assert_eq!(byte.report, chars.report);
        assert_eq!(byte.report.planned_bytes, 200);
        assert_eq!(byte.candidates.len(), 2);
        assert_eq!(chars.candidates.len(), 2);
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(128))]

        #[test]
        fn resident_closure_selector_matches_reference_subtree_union(
            nodes in proptest::collection::vec(
                (
                    0u8..16,
                    1usize..128,
                    proptest::bool::ANY,
                    proptest::bool::ANY,
                ),
                1..17,
            ),
            min_depth in 0usize..17,
            target in 1usize..4096,
            cap in 0usize..17,
        ) {
            let mut topology = PathTopology::<u8>::new();
            let mut path_by_depth = Vec::new();
            let mut depths = Vec::new();
            depths
                .try_reserve_exact(nodes.len())
                .expect("reserve property depths");
            for (index, (depth_seed, _, _, _)) in nodes.iter().enumerate() {
                let depth = if index == 0 {
                    1
                } else {
                    1 + usize::from(*depth_seed) % (depths[index - 1] + 1)
                };
                let parent = if depth == 1 {
                    RegistryPathId::ROOT
                } else {
                    path_by_depth[depth - 2]
                };
                let edge = [u8::try_from(index).expect("bounded property edge")];
                let path_id = topology
                    .try_reserve_path(
                        parent,
                        &edge,
                        super::super::lru_tracker::PATH_HASH_OFFSET,
                        super::super::lru_tracker::extend_byte_path_hash,
                    )
                    .expect("reserve generated preorder node");
                path_by_depth.truncate(depth - 1);
                path_by_depth.push(path_id);
                depths.push(depth);
            }
            topology
                .try_finalize_subtree_ends()
                .expect("finalize property topology");
            let topology = Arc::new(topology);
            let resident: Vec<bool> = nodes.iter().map(|(_, _, resident, _)| *resident).collect();
            let weights: Vec<usize> = nodes.iter().map(|(_, weight, _, _)| *weight).collect();
            let anchors: Vec<(RegistryPathId, usize)> = (0..topology.len())
                .filter_map(|index| {
                    let id = RegistryPathId(index);
                    let depth = topology.depth(id)?;
                    (resident[index] && depth != 0 && depth >= min_depth)
                        .then_some((id, depth))
                })
                .collect();
            let lru = LruRegistry::new();
            for (index, (_, _, is_resident, is_hot)) in nodes.iter().enumerate() {
                if *is_resident && *is_hot {
                    let hash = topology
                        .hash(
                            RegistryPathId(index),
                            super::super::lru_tracker::PATH_HASH_OFFSET,
                        )
                        .expect("generated hot path hash");
                    lru.touch_hash(hash);
                }
            }
            let resident_record = |index: usize| {
                if !resident[index] {
                    return Ok(None);
                }
                let id = RegistryPathId(index);
                let hash = topology
                    .hash(id, super::super::lru_tracker::PATH_HASH_OFFSET)
                    .ok_or(CompactSelectionError::TopologyUnavailable)?;
                Ok(Some((weights[index], hash)))
            };
            let materialize = |id: RegistryPathId| {
                let index = id.index().ok_or(CompactSelectionError::TopologyUnavailable)?;
                Ok(CompactEvictionCandidate {
                    path_id: id,
                    path_hash: topology
                        .hash(id, super::super::lru_tracker::PATH_HASH_OFFSET)
                        .ok_or(CompactSelectionError::TopologyUnavailable)?,
                    disk_ptr: make_typed_disk_ptr(
                        1,
                        u32::try_from((index + 1) * 16)
                            .map_err(|_| CompactSelectionError::SizeOverflow)?,
                        NodeType::Node4,
                    ),
                    size_bytes: weights[index],
                    depth: topology
                        .depth(id)
                        .ok_or(CompactSelectionError::TopologyUnavailable)?,
                    node_type: NodeType::Node4,
                })
            };
            let generation = RegistryGeneration::new();
            let batch = try_select_resident_budget_batch(
                ResidentBudgetSelectionContext {
                    valid: true,
                    topology: &topology,
                    generation: &generation,
                    lru_registry: &lru,
                    limits: CompactSelectionLimits {
                        target_bytes: target,
                        max_count: cap,
                        overhead: 0,
                    },
                },
                anchors.clone().into_iter(),
                resident_record,
                materialize,
            )
            .expect("property resident selection");

            let union_weight = |selected: &[CompactEvictionCandidate]| {
                let mut covered = vec![false; nodes.len()];
                for candidate in selected {
                    for index in topology
                        .subtree_range(candidate.path_id)
                        .expect("selected subtree range")
                    {
                        covered[index] = true;
                    }
                }
                covered
                    .iter()
                    .enumerate()
                    .filter(|(index, covered)| **covered && resident[*index])
                    .map(|(index, _)| weights[index])
                    .sum::<usize>()
            };
            let exact_union = union_weight(&batch.candidates);
            prop_assert_eq!(batch.report.planned_bytes, exact_union);
            prop_assert_eq!(batch.report.eligible_candidates, anchors.len());
            prop_assert_eq!(batch.report.nonredundant_candidates, anchors.len());
            prop_assert_eq!(batch.report.selected_priority_count, batch.candidates.len());
            prop_assert!(batch.candidates.len() <= cap);

            let mut selected = vec![false; nodes.len()];
            for candidate in &batch.candidates {
                selected[candidate.path_id.0] = true;
            }
            let mut covered = vec![false; nodes.len()];
            for candidate in &batch.candidates {
                for index in topology
                    .subtree_range(candidate.path_id)
                    .expect("selected property subtree")
                {
                    covered[index] = true;
                }
            }
            for index in 0..nodes.len() {
                if resident[index] && covered[index] {
                    prop_assert!(
                        selected[index],
                        "a selected ancestor covered an unselected resident descendant"
                    );
                }
            }
            prop_assert_eq!(
                batch.report.selected_priority_count,
                covered
                    .iter()
                    .enumerate()
                    .filter(|(index, is_covered)| **is_covered && resident[*index])
                    .count()
            );

            if exact_union >= target {
                prop_assert!(!batch.report.cap_exhausted);
                prop_assert!(!batch.report.eligible_exhausted);
                if !batch.candidates.is_empty() {
                    prop_assert!(union_weight(
                        &batch.candidates[..batch.candidates.len() - 1]
                    ) < target);
                }
            } else if batch.candidates.len() < anchors.len() {
                prop_assert!(batch.report.cap_exhausted);
                prop_assert!(!batch.report.eligible_exhausted);
            } else {
                prop_assert!(!batch.report.cap_exhausted);
                prop_assert!(batch.report.eligible_exhausted);
            }

            for (earlier_index, earlier) in batch.candidates.iter().enumerate() {
                for later in batch.candidates.iter().skip(earlier_index + 1) {
                    let earlier_range = topology
                        .subtree_range(earlier.path_id)
                        .expect("earlier range");
                    prop_assert!(
                        !earlier_range.contains(&later.path_id.0),
                        "an ancestor ranked before its resident descendant"
                    );
                }
            }
        }
    }

    #[test]
    fn resident_closure_selector_is_stack_safe_at_one_hundred_thousand_depth() {
        const DEPTH: usize = 100_000;

        let mut topology = PathTopology::<u8>::new();
        let mut cursor = RegistryPathId::ROOT;
        let mut anchors = Vec::new();
        anchors
            .try_reserve_exact(DEPTH)
            .expect("reserve deep resident anchors");
        for depth in 1..=DEPTH {
            cursor = topology
                .try_reserve_path(
                    cursor,
                    b"x",
                    super::super::lru_tracker::PATH_HASH_OFFSET,
                    super::super::lru_tracker::extend_byte_path_hash,
                )
                .expect("reserve deep resident topology");
            anchors.push((cursor, depth));
        }
        topology
            .try_finalize_subtree_ends()
            .expect("finalize deep resident topology");
        let topology = Arc::new(topology);
        let resident_record = |index: usize| {
            let path_id = RegistryPathId(index);
            let path_hash = topology
                .hash(path_id, super::super::lru_tracker::PATH_HASH_OFFSET)
                .ok_or(CompactSelectionError::TopologyUnavailable)?;
            Ok(Some((1, path_hash)))
        };
        let materialize = |path_id: RegistryPathId| {
            let index = path_id
                .index()
                .ok_or(CompactSelectionError::TopologyUnavailable)?;
            Ok(CompactEvictionCandidate {
                path_id,
                path_hash: topology
                    .hash(path_id, super::super::lru_tracker::PATH_HASH_OFFSET)
                    .ok_or(CompactSelectionError::TopologyUnavailable)?,
                disk_ptr: make_typed_disk_ptr(
                    1,
                    u32::try_from(index + 1).map_err(|_| CompactSelectionError::SizeOverflow)?,
                    NodeType::Node4,
                ),
                size_bytes: 1,
                depth: index + 1,
                node_type: NodeType::Node4,
            })
        };

        let generation = RegistryGeneration::new();
        let lru_registry = LruRegistry::new();
        let batch = try_select_resident_budget_batch(
            ResidentBudgetSelectionContext {
                valid: true,
                topology: &topology,
                generation: &generation,
                lru_registry: &lru_registry,
                limits: CompactSelectionLimits {
                    target_bytes: DEPTH,
                    max_count: DEPTH,
                    overhead: 0,
                },
            },
            anchors.into_iter(),
            resident_record,
            materialize,
        )
        .expect("select deep resident closure");

        assert_eq!(batch.report.planned_bytes, DEPTH);
        assert_eq!(batch.report.eligible_candidates, DEPTH);
        assert_eq!(batch.report.nonredundant_candidates, DEPTH);
        assert_eq!(batch.report.selected_priority_count, DEPTH);
        assert_eq!(batch.candidates.len(), DEPTH);
        assert_eq!(
            batch.candidates.first().map(|candidate| candidate.depth),
            Some(DEPTH)
        );
        assert_eq!(
            batch.candidates.last().map(|candidate| candidate.depth),
            Some(1)
        );
    }

    #[test]
    fn publication_rejects_shared_unfinalized_topology_without_copy_on_write() {
        let mut registry = DiskLocationRegistry::new();
        registry
            .try_reserve_byte_path(RegistryPathId::ROOT, b"shared")
            .expect("unfinalized byte path");
        let shared_identity = Arc::clone(&registry.byte_paths);

        assert!(matches!(
            registry.try_finalize_for_publication(),
            Err(RegistryBuildError::DestinationInvariant(
                "unfinalized byte topology is shared"
            ))
        ));
        assert!(Arc::ptr_eq(&shared_identity, &registry.byte_paths));
        assert!(!registry.byte_paths.is_finalized());
    }

    #[test]
    fn publication_accepts_shared_topology_after_unique_finalization() {
        let mut registry = DiskLocationRegistry::new();
        registry
            .try_reserve_char_units(RegistryPathId::ROOT, &[u32::from('計')])
            .expect("unfinalized char path");
        registry
            .try_finalize_for_publication()
            .expect("unique finalization");
        let shared_identity = Arc::clone(&registry.char_paths);

        registry
            .try_finalize_for_publication()
            .expect("shared finalized topology is publication-ready");
        assert!(Arc::ptr_eq(&shared_identity, &registry.char_paths));
        assert_eq!(
            registry.char_paths.subtree_range(RegistryPathId(0)),
            Some(0..1)
        );
    }

    #[test]
    fn malformed_preorder_fails_finalization_and_leaves_ranges_unavailable() {
        let mut topology = PathTopology::with_capacity(2);
        topology
            .try_reserve_path(
                RegistryPathId::ROOT,
                b"a",
                super::super::lru_tracker::PATH_HASH_OFFSET,
                super::super::lru_tracker::extend_byte_path_hash,
            )
            .expect("root entry");
        topology
            .try_reserve_path(
                RegistryPathId(0),
                b"b",
                super::super::lru_tracker::PATH_HASH_OFFSET,
                super::super::lru_tracker::extend_byte_path_hash,
            )
            .expect("child entry");
        topology.entries[1].parent = RegistryPathId(9);

        assert!(matches!(
            topology.try_finalize_subtree_ends(),
            Err(RegistryBuildError::TopologyInvariant(
                "entry parent is outside the active preorder ancestry"
            ))
        ));
        assert!(!topology.is_finalized());
        assert!(topology.subtree_range(RegistryPathId(0)).is_none());
    }

    #[test]
    fn compact_selection_rejects_unfinalized_topology() {
        let mut topology = PathTopology::new();
        let path_id = topology
            .try_reserve_path(
                RegistryPathId::ROOT,
                b"x",
                super::super::lru_tracker::PATH_HASH_OFFSET,
                super::super::lru_tracker::extend_byte_path_hash,
            )
            .expect("unfinalized path");
        let topology = Arc::new(topology);
        let candidate = CompactEvictionCandidate {
            path_id,
            path_hash: topology
                .hash(path_id, super::super::lru_tracker::PATH_HASH_OFFSET)
                .expect("path hash"),
            disk_ptr: make_typed_disk_ptr(1, 8, NodeType::Node4),
            size_bytes: 1,
            depth: 1,
            node_type: NodeType::Node4,
        };

        assert!(matches!(
            try_select_compact_batch(
                true,
                &topology,
                &RegistryGeneration::new(),
                std::iter::once(candidate),
                &LruRegistry::new(),
                CompactSelectionLimits {
                    target_bytes: usize::MAX,
                    max_count: 1,
                    overhead: 0,
                },
            ),
            Err(CompactSelectionError::TopologyUnavailable)
        ));
    }

    #[test]
    fn structural_source_retains_finalized_topologies_after_registry_clear() {
        let mut registry = DiskLocationRegistry::new();
        let byte_id = registry
            .try_reserve_byte_path(RegistryPathId::ROOT, b"byte")
            .expect("byte path");
        registry
            .register_byte_path(
                byte_id,
                make_typed_disk_ptr(1, 8, NodeType::Node4),
                11,
                4,
                NodeType::Node4,
            )
            .expect("byte record");
        let char_id = registry
            .try_reserve_char_units(RegistryPathId::ROOT, &[u32::from('計')])
            .expect("char path");
        registry
            .register_char_path(
                char_id,
                make_typed_disk_ptr(2, 8, NodeType::CharNode4),
                13,
                1,
                NodeType::CharNode4,
            )
            .expect("char record");
        registry
            .try_finalize_for_publication()
            .expect("finalize source registry");
        let source = registry
            .structural_source()
            .expect("capture source")
            .expect("structural source");

        registry.clear();

        assert!(registry.topologies_are_finalized());
        assert_eq!(source.byte_paths.subtree_range(byte_id), Some(0..1));
        assert_eq!(source.char_paths.subtree_range(char_id), Some(0..1));
        assert_eq!(source.locations.len(), 1);
        assert_eq!(source.char_locations.len(), 1);
    }

    #[test]
    fn exact_byte_graft_preserves_topology_across_different_segmentations() {
        let root_ptr = make_typed_disk_ptr(1, 10, NodeType::Node4);
        let child_ptr = make_typed_disk_ptr(1, 20, NodeType::Node16);
        let leaf_ptr = make_typed_disk_ptr(1, 30, NodeType::Node48);
        let mut source = DiskLocationRegistry::new();
        let source_ab = source
            .try_reserve_byte_path(RegistryPathId::ROOT, b"ab")
            .expect("source ab");
        let source_abc = source
            .try_reserve_byte_path(source_ab, b"c")
            .expect("source abc");
        let source_abcde = source
            .try_reserve_byte_path(source_abc, b"de")
            .expect("source abcde");
        source
            .register_byte_path(source_ab, root_ptr.clone(), 11, 2, NodeType::Node4)
            .expect("source root record");
        source
            .register_byte_path(source_abc, child_ptr.clone(), 13, 3, NodeType::Node16)
            .expect("source child record");
        source
            .register_byte_path(source_abcde, leaf_ptr.clone(), 17, 5, NodeType::Node48)
            .expect("source leaf record");
        source
            .try_finalize_for_publication()
            .expect("finalize source topology");
        let carry = source
            .structural_source()
            .expect("capture source")
            .expect("valid source");

        let mut target = DiskLocationRegistry::new();
        let target_a = target
            .try_reserve_byte_path(RegistryPathId::ROOT, b"a")
            .expect("target a");
        let target_ab = target
            .try_reserve_byte_path(target_a, b"b")
            .expect("target ab");
        let outcome = target
            .try_graft_byte_subtree(&carry, target_ab, &root_ptr, true)
            .expect("graft succeeds");

        assert_eq!(
            outcome,
            RegistryGraftOutcome::Grafted {
                topology_entries: 3,
                durable_records: 3,
            }
        );
        assert_eq!(target.byte_paths.len(), 4);
        assert_eq!(target.len(), 3);
        assert_eq!(target.byte_resident_len(), 3);
        assert_eq!(target.byte_resident_serialized_bytes(), 41);
        assert!(target.byte_paths.path_equals_slice(target_ab, b"ab"));
        assert!(target
            .byte_paths
            .path_equals_slice(RegistryPathId(2), b"abc"));
        assert!(target
            .byte_paths
            .path_equals_slice(RegistryPathId(3), b"abcde"));
        assert_eq!(
            target.locations[RegistryPathId(3).0]
                .as_ref()
                .expect("grafted leaf")
                .disk_ptr
                .to_raw(),
            leaf_ptr.to_raw()
        );
    }

    #[test]
    fn invalidation_disables_eviction_but_preserves_exact_structural_reuse() {
        let root_ptr = make_typed_disk_ptr(41, 10, NodeType::Node4);
        let child_ptr = make_typed_disk_ptr(41, 20, NodeType::Node16);
        let mut source = DiskLocationRegistry::new();
        let source_root = source
            .try_reserve_byte_path(RegistryPathId::ROOT, b"root")
            .expect("source root");
        let source_child = source
            .try_reserve_byte_path(source_root, b"/child")
            .expect("source child");
        source
            .register_byte_path(source_root, root_ptr.clone(), 17, 4, NodeType::Node4)
            .expect("source root record");
        source
            .register_byte_path(source_child, child_ptr, 23, 10, NodeType::Node16)
            .expect("source child record");

        source
            .try_finalize_for_publication()
            .expect("finalize source topology");

        source.invalidate();
        assert!(!source.is_valid());
        assert!(source
            .select_compact_for_eviction(usize::MAX, &LruRegistry::new(), 0, usize::MAX, 0,)
            .candidates
            .is_empty());

        let structural_source = source
            .structural_source()
            .expect("structural-source capture")
            .expect("structural source remains available");
        let mut target = DiskLocationRegistry::new();
        let target_root = target
            .try_reserve_byte_path(RegistryPathId::ROOT, b"root")
            .expect("target root");
        assert_eq!(
            target
                .try_graft_byte_subtree(&structural_source, target_root, &root_ptr, true)
                .expect("exact structural graft"),
            RegistryGraftOutcome::Grafted {
                topology_entries: 2,
                durable_records: 2,
            }
        );
        assert_eq!(target.byte_resident_len(), 2);
        assert_eq!(target.byte_resident_serialized_bytes(), 40);
    }

    #[test]
    fn byte_graft_accepts_sibling_alias_but_rejects_ancestor_cycle_atomically() {
        let root_ptr = make_typed_disk_ptr(2, 10, NodeType::Node4);
        let alias_ptr = make_typed_disk_ptr(2, 20, NodeType::Node16);
        let mut aliases = DiskLocationRegistry::new();
        let root = aliases
            .try_reserve_byte_path(RegistryPathId::ROOT, b"r")
            .expect("alias root");
        let left = aliases
            .try_reserve_byte_path(root, b"l")
            .expect("alias left");
        let right = aliases
            .try_reserve_byte_path(root, b"r")
            .expect("alias right");
        aliases
            .register_byte_path(root, root_ptr.clone(), 7, 1, NodeType::Node4)
            .expect("alias root record");
        for id in [left, right] {
            aliases
                .register_byte_path(id, alias_ptr.clone(), 9, 2, NodeType::Node16)
                .expect("sibling alias record");
        }
        aliases
            .try_finalize_for_publication()
            .expect("finalize alias topology");
        let alias_source = aliases
            .structural_source()
            .expect("alias source capture")
            .expect("valid alias source");
        let mut alias_target = DiskLocationRegistry::new();
        let alias_target_root = alias_target
            .try_reserve_byte_path(RegistryPathId::ROOT, b"r")
            .expect("alias target root");
        assert!(matches!(
            alias_target
                .try_graft_byte_subtree(&alias_source, alias_target_root, &root_ptr, true)
                .expect("sibling alias graft"),
            RegistryGraftOutcome::Grafted {
                topology_entries: 3,
                durable_records: 3,
            }
        ));

        let mut cyclic = DiskLocationRegistry::new();
        let cycle_root = cyclic
            .try_reserve_byte_path(RegistryPathId::ROOT, b"c")
            .expect("cycle root");
        let cycle_child = cyclic
            .try_reserve_byte_path(cycle_root, b"x")
            .expect("cycle child");
        cyclic
            .register_byte_path(cycle_root, root_ptr.clone(), 7, 1, NodeType::Node4)
            .expect("cycle root record");
        cyclic
            .register_byte_path(cycle_child, root_ptr.clone(), 7, 2, NodeType::Node4)
            .expect("cycle child record");
        cyclic
            .try_finalize_for_publication()
            .expect("finalize cyclic topology");
        let cyclic_source = cyclic
            .structural_source()
            .expect("cycle source capture")
            .expect("valid structural source");
        let mut cycle_target = DiskLocationRegistry::new();
        let cycle_target_root = cycle_target
            .try_reserve_byte_path(RegistryPathId::ROOT, b"c")
            .expect("cycle target root");
        let before_entries = cycle_target.byte_paths.len();
        assert_eq!(
            cycle_target
                .try_graft_byte_subtree(&cyclic_source, cycle_target_root, &root_ptr, true)
                .expect("cycle selects fallback"),
            RegistryGraftOutcome::FallbackRequired
        );
        assert_eq!(cycle_target.byte_paths.len(), before_entries);
        assert_eq!(cycle_target.len(), 0);
        assert_eq!(cycle_target.byte_resident_len(), 0);
    }

    #[test]
    fn exact_char_graft_preserves_residency_and_segmentation() {
        let root_ptr = make_typed_disk_ptr(3, 10, NodeType::CharNode4);
        let leaf_ptr = make_typed_disk_ptr(3, 20, NodeType::CharNode16);
        let mut source = DiskLocationRegistry::new();
        let source_root = source
            .try_reserve_char_units(RegistryPathId::ROOT, &[u32::from('λ'), u32::from('計')])
            .expect("char source root");
        let source_leaf = source
            .try_reserve_char_units(source_root, &[u32::from('算')])
            .expect("char source leaf");
        source
            .register_char_path(source_root, root_ptr.clone(), 19, 2, NodeType::CharNode4)
            .expect("char source root record");
        source
            .register_char_path(source_leaf, leaf_ptr, 23, 3, NodeType::CharNode16)
            .expect("char source leaf record");
        source
            .try_finalize_for_publication()
            .expect("finalize char source topology");
        let carry = source
            .structural_source()
            .expect("char source capture")
            .expect("valid char source");

        let mut target = DiskLocationRegistry::new();
        let lambda = target
            .try_reserve_char_units(RegistryPathId::ROOT, &[u32::from('λ')])
            .expect("target lambda");
        let target_root = target
            .try_reserve_char_units(lambda, &[u32::from('計')])
            .expect("target char root");
        assert_eq!(
            target
                .try_graft_char_subtree(&carry, target_root, &root_ptr, true)
                .expect("char graft"),
            RegistryGraftOutcome::Grafted {
                topology_entries: 2,
                durable_records: 2,
            }
        );
        assert_eq!(target.char_len(), 2);
        assert_eq!(target.char_resident_len(), 2);
        assert_eq!(target.char_resident_serialized_bytes(), 42);
        assert!(target.char_paths.path_equals_slice(
            RegistryPathId(2),
            &[u32::from('λ'), u32::from('計'), u32::from('算')]
        ));
    }

    #[test]
    fn local_byte_builder_graft_preserves_locationless_prefixes_and_mixed_residency() {
        let root_ptr = make_typed_disk_ptr(31, 10, NodeType::Node4);
        let left_ptr = make_typed_disk_ptr(31, 20, NodeType::Node16);
        let right_ptr = make_typed_disk_ptr(31, 30, NodeType::Node48);
        let mut registry = DiskLocationRegistry::new();

        let source_root = registry
            .try_reserve_byte_path(RegistryPathId::ROOT, b"a")
            .expect("source root");
        let start = registry
            .try_begin_byte_builder_subtree(source_root)
            .expect("begin source subtree");
        registry
            .register_byte_path(source_root, root_ptr.clone(), 11, 1, NodeType::Node4)
            .expect("source root record");
        let locationless_prefix = registry
            .try_reserve_byte_path(source_root, b"bc")
            .expect("locationless prefix");
        let left = registry
            .try_reserve_byte_path(locationless_prefix, b"d")
            .expect("left leaf");
        registry
            .register_nonresident_byte_path(left, left_ptr.clone(), 13, 4, NodeType::Node16)
            .expect("nonresident left record");
        let right = registry
            .try_reserve_byte_path(locationless_prefix, b"e")
            .expect("right leaf");
        registry
            .register_byte_path(right, right_ptr.clone(), 17, 4, NodeType::Node48)
            .expect("resident right record");
        let source = registry
            .try_finish_byte_builder_subtree(start)
            .expect("finish source subtree");

        let destination = registry
            .try_reserve_byte_path(RegistryPathId::ROOT, b"z")
            .expect("destination root");
        let stats = registry
            .try_graft_byte_builder_subtree(&source, destination, &root_ptr, true)
            .expect("local byte graft");

        assert_eq!(
            stats,
            LocalRegistryGraftStats {
                appended_topology_entries: 3,
                durable_records: 3,
                serialized_bytes: 41,
                overflowed: false,
            }
        );
        assert_eq!(registry.byte_paths.len(), 8);
        assert!(registry.byte_paths.path_equals_slice(destination, b"z"));
        assert!(registry
            .byte_paths
            .path_equals_slice(RegistryPathId(5), b"zbc"));
        assert!(registry
            .byte_paths
            .path_equals_slice(RegistryPathId(6), b"zbcd"));
        assert!(registry
            .byte_paths
            .path_equals_slice(RegistryPathId(7), b"zbce"));
        assert!(registry.byte_residency.is_resident(destination.0));
        assert!(!registry.byte_residency.is_resident(6));
        assert!(registry.byte_residency.is_resident(7));
        assert_eq!(registry.len(), 6);
        assert_eq!(registry.byte_resident_len(), 4);
        assert_eq!(registry.byte_resident_serialized_bytes(), 56);
        registry
            .try_finalize_for_publication()
            .expect("completed builder has no open lifecycle");
    }

    #[test]
    fn local_char_builder_graft_recomputes_destination_depth_and_rejects_wrong_residency() {
        let root_ptr = make_typed_disk_ptr(32, 10, NodeType::CharNode4);
        let leaf_ptr = make_typed_disk_ptr(32, 20, NodeType::CharNode16);
        let mut registry = DiskLocationRegistry::new();

        let source_root = registry
            .try_reserve_char_units(RegistryPathId::ROOT, &[u32::from('λ')])
            .expect("char source root");
        let start = registry
            .try_begin_char_builder_subtree(source_root)
            .expect("begin char source subtree");
        registry
            .register_char_path(source_root, root_ptr.clone(), 19, 1, NodeType::CharNode4)
            .expect("char source root record");
        let locationless_prefix = registry
            .try_reserve_char_units(source_root, &[u32::from('計')])
            .expect("char locationless prefix");
        let leaf = registry
            .try_reserve_char_units(locationless_prefix, &[u32::from('算')])
            .expect("char leaf");
        registry
            .register_nonresident_char_path(leaf, leaf_ptr, 23, 3, NodeType::CharNode16)
            .expect("char nonresident leaf");
        let source = registry
            .try_finish_char_builder_subtree(start)
            .expect("finish char source subtree");

        let destination_parent = registry
            .try_reserve_char_units(RegistryPathId::ROOT, &[u32::from('文')])
            .expect("char destination parent");
        let destination = registry
            .try_reserve_char_units(destination_parent, &[u32::from('章')])
            .expect("char destination root");
        assert!(matches!(
            registry.try_graft_char_builder_subtree(&source, destination, &root_ptr, false),
            Err(RegistryBuildError::TopologyInvariant(
                "char builder graft source-root residency does not match its occurrence"
            ))
        ));
        assert_eq!(registry.char_paths.len(), 5);
        let stats = registry
            .try_graft_char_builder_subtree(&source, destination, &root_ptr, true)
            .expect("local char graft");

        assert_eq!(
            stats,
            LocalRegistryGraftStats {
                appended_topology_entries: 2,
                durable_records: 2,
                serialized_bytes: 42,
                overflowed: false,
            }
        );
        assert!(registry.char_paths.path_equals_slice(
            RegistryPathId(5),
            &[u32::from('文'), u32::from('章'), u32::from('計')]
        ));
        assert!(registry.char_paths.path_equals_slice(
            RegistryPathId(6),
            &[
                u32::from('文'),
                u32::from('章'),
                u32::from('計'),
                u32::from('算')
            ]
        ));
        assert_eq!(
            registry
                .char_locations
                .get(destination.0)
                .and_then(Option::as_ref)
                .map(|entry| entry.depth),
            Some(2)
        );
        assert_eq!(
            registry
                .char_locations
                .get(6)
                .and_then(Option::as_ref)
                .map(|entry| entry.depth),
            Some(4)
        );
        assert!(registry.char_residency.is_resident(destination.0));
        assert!(!registry.char_residency.is_resident(6));
    }

    #[test]
    fn builder_subtree_tokens_enforce_generation_lifo_and_empty_cancel() {
        let root_ptr = make_typed_disk_ptr(33, 10, NodeType::Node4);
        let child_ptr = make_typed_disk_ptr(33, 20, NodeType::Node16);
        let mut registry = DiskLocationRegistry::new();
        let root = registry
            .try_reserve_byte_path(RegistryPathId::ROOT, b"r")
            .expect("outer root");
        let outer = registry
            .try_begin_byte_builder_subtree(root)
            .expect("begin outer");
        let child = registry
            .try_reserve_byte_path(root, b"c")
            .expect("inner root");
        let inner = registry
            .try_begin_byte_builder_subtree(child)
            .expect("begin inner");
        registry
            .register_byte_path(child, child_ptr, 13, 2, NodeType::Node16)
            .expect("inner record");

        assert!(matches!(
            registry.try_finish_byte_builder_subtree(outer.clone()),
            Err(RegistryBuildError::TopologyInvariant(
                "byte builder subtrees did not finish in LIFO order"
            ))
        ));
        registry
            .try_finish_byte_builder_subtree(inner)
            .expect("finish inner");
        registry
            .register_byte_path(root, root_ptr.clone(), 11, 1, NodeType::Node4)
            .expect("outer record");
        let source = registry
            .try_finish_byte_builder_subtree(outer)
            .expect("finish outer");

        let empty = registry
            .try_reserve_byte_path(RegistryPathId::ROOT, b"e")
            .expect("empty cancellation root");
        let empty_start = registry
            .try_begin_byte_builder_subtree(empty)
            .expect("begin cancellable subtree");
        registry
            .try_cancel_byte_builder_subtree(empty_start)
            .expect("cancel untouched subtree");

        let mut other = DiskLocationRegistry::new();
        let destination = other
            .try_reserve_byte_path(RegistryPathId::ROOT, b"x")
            .expect("foreign destination");
        assert!(matches!(
            other.try_graft_byte_builder_subtree(&source, destination, &root_ptr, true),
            Err(RegistryBuildError::TopologyInvariant(
                "byte builder graft source belongs to another registry"
            ))
        ));
        registry
            .try_finalize_for_publication()
            .expect("all builder tokens closed");
    }

    #[test]
    fn test_registry_basic() {
        let mut registry = DiskLocationRegistry::new();
        assert!(registry.is_empty());
        assert!(registry.is_valid());
        assert!(!registry.is_authoritative());
        assert!(registry.is_publication_candidate());

        registry.register(
            b"test".to_vec(),
            make_disk_ptr(1, 100),
            256,
            1,
            NodeType::Node16,
        );

        assert_eq!(registry.len(), 1);
        assert_eq!(registry.total_size_bytes(), 256);
        assert_eq!(registry.count_by_type(NodeType::Node16), 1);

        let hash = LruRegistry::path_hash(b"test");
        let node = registry.get_owned(hash).expect("node should exist");
        assert_eq!(node.path, b"test".to_vec());
        assert_eq!(node.size_bytes, 256);
        assert_eq!(node.depth, 1);
    }

    #[test]
    fn residency_tracks_exact_byte_and_char_totals_across_replacement_and_removal() {
        let mut registry = DiskLocationRegistry::new();

        registry.register(
            b"resident-byte".to_vec(),
            make_typed_disk_ptr(1, 10, NodeType::Node4),
            101,
            1,
            NodeType::Node4,
        );
        registry.register_char(
            vec!['常', '駐'],
            make_typed_disk_ptr(2, 10, NodeType::CharNode4),
            203,
            2,
            NodeType::CharNode4,
        );

        assert_eq!(registry.byte_resident_len(), 1);
        assert_eq!(registry.byte_resident_serialized_bytes(), 101);
        assert_eq!(registry.char_resident_len(), 1);
        assert_eq!(registry.char_resident_serialized_bytes(), 203);
        assert_eq!(
            registry.byte_resident_estimate_bytes(),
            101 + STRUCT_OVERHEAD_BYTE
        );
        assert_eq!(
            registry.char_resident_estimate_bytes(),
            203 + STRUCT_OVERHEAD_CHAR
        );

        registry.register(
            b"resident-byte".to_vec(),
            make_disk_ptr(1, 20),
            307,
            1,
            NodeType::Node16,
        );
        registry.register_char(
            vec!['常', '駐'],
            make_typed_disk_ptr(2, 20, NodeType::CharNode16),
            409,
            2,
            NodeType::CharNode16,
        );

        assert_eq!(registry.byte_resident_len(), 1);
        assert_eq!(registry.byte_resident_serialized_bytes(), 307);
        assert_eq!(registry.char_resident_len(), 1);
        assert_eq!(registry.char_resident_serialized_bytes(), 409);

        assert!(registry
            .remove(LruRegistry::path_hash(b"resident-byte"))
            .is_some());
        assert!(registry
            .remove_char(super::super::lru_tracker::hash_char_path(&['常', '駐']))
            .is_some());
        assert_eq!(registry.byte_resident_len(), 0);
        assert_eq!(registry.byte_resident_serialized_bytes(), 0);
        assert_eq!(registry.char_resident_len(), 0);
        assert_eq!(registry.char_resident_serialized_bytes(), 0);
        assert_eq!(registry.byte_resident_estimate_bytes(), 0);
        assert_eq!(registry.char_resident_estimate_bytes(), 0);
    }

    #[test]
    fn residency_bitset_is_exact_across_machine_word_boundaries() {
        let mut residency = ResidencyState::default();
        let mut expected_bytes = 0usize;
        for index in 0..=RESIDENCY_WORD_BITS {
            let bytes = index + 1;
            assert!(residency
                .try_mark_existing(index, bytes)
                .expect("mark resident bit"));
            expected_bytes += bytes;
        }

        assert_eq!(residency.bits.len(), 2);
        assert_eq!(residency.resident_nodes(), RESIDENCY_WORD_BITS + 1);
        assert_eq!(residency.resident_serialized_bytes(), expected_bytes);
        assert_eq!(
            residency.word_serialized_bytes[0],
            (1..=RESIDENCY_WORD_BITS).sum::<usize>()
        );
        assert_eq!(residency.word_serialized_bytes[1], RESIDENCY_WORD_BITS + 1);

        for index in [0, RESIDENCY_WORD_BITS - 1, RESIDENCY_WORD_BITS] {
            let bytes = index + 1;
            assert!(residency
                .try_clear_existing(index, bytes)
                .expect("clear resident bit"));
            expected_bytes -= bytes;
            assert!(!residency.is_resident(index));
        }

        assert_eq!(residency.resident_nodes(), RESIDENCY_WORD_BITS - 2);
        assert_eq!(residency.resident_serialized_bytes(), expected_bytes);
        assert_eq!(
            residency.word_serialized_bytes[0],
            (2..RESIDENCY_WORD_BITS).sum::<usize>()
        );
        assert_eq!(residency.word_serialized_bytes[1], 0);
        assert!(!residency
            .try_clear_existing(RESIDENCY_WORD_BITS, RESIDENCY_WORD_BITS + 1)
            .expect("clearing an already-clear in-range bit is a no-op"));
    }

    #[test]
    fn selection_excludes_nonresident_records_without_discarding_metadata() {
        let mut registry = DiskLocationRegistry::new();
        let byte_id = registry
            .try_reserve_byte_path(RegistryPathId::ROOT, b"byte")
            .expect("reserve byte path");
        registry
            .register_byte_path(
                byte_id,
                make_typed_disk_ptr(1, 1, NodeType::Node4),
                41,
                4,
                NodeType::Node4,
            )
            .expect("register byte record");
        let char_id = registry
            .try_reserve_char_path(RegistryPathId::ROOT, &['字'])
            .expect("reserve char path");
        registry
            .register_char_path(
                char_id,
                make_typed_disk_ptr(2, 1, NodeType::CharNode4),
                43,
                1,
                NodeType::CharNode4,
            )
            .expect("register char record");

        assert!(registry
            .byte_residency
            .try_clear_existing(byte_id.0, 41)
            .expect("clear byte residency"));
        assert!(registry
            .char_residency
            .try_clear_existing(char_id.0, 43)
            .expect("clear char residency"));
        registry
            .try_finalize_for_publication()
            .expect("finalize nonresident registry");
        make_authoritative(&mut registry);

        assert_eq!(registry.len(), 1, "byte metadata remains registered");
        assert_eq!(registry.char_len(), 1, "char metadata remains registered");
        assert!(registry.eviction_candidates(0).is_empty());
        assert!(registry.char_eviction_candidates(0).is_empty());
        assert!(registry
            .select_compact_for_eviction(usize::MAX, &LruRegistry::new(), 0, usize::MAX, 0)
            .candidates
            .is_empty());
        assert!(registry
            .select_compact_char_for_eviction(usize::MAX, &LruRegistry::new(), 0, usize::MAX, 0,)
            .candidates
            .is_empty());
    }
    #[test]
    fn resident_soft_estimate_detects_unrepresentable_totals() {
        let residency = ResidencyState {
            resident_nodes: usize::MAX,
            ..ResidencyState::default()
        };
        assert_eq!(
            DiskLocationRegistry::resident_soft_estimate(&residency, 2),
            None
        );
    }

    #[test]
    fn duplicate_public_byte_and_char_paths_replace_exact_records() {
        let mut registry = DiskLocationRegistry::new();
        let byte_path = b"duplicate".to_vec();
        let byte_hash = LruRegistry::path_hash(&byte_path);
        registry.register(
            byte_path.clone(),
            make_typed_disk_ptr(1, 10, NodeType::Node4),
            100,
            7,
            NodeType::Node4,
        );
        registry.register(byte_path, make_disk_ptr(1, 20), 220, 9, NodeType::Node16);
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.byte_paths.stored_entries(), 1);
        let byte = registry
            .get_owned(byte_hash)
            .expect("replacement byte record");
        assert_eq!(byte.disk_ptr.to_raw(), make_disk_ptr(1, 20).to_raw());
        assert_eq!(byte.size_bytes, 220);
        assert_eq!(byte.depth, 9);

        let char_path = vec!['重', '複'];
        let char_hash = super::super::lru_tracker::hash_char_path(&char_path);
        registry.register_char(
            char_path.clone(),
            make_typed_disk_ptr(2, 10, NodeType::CharNode4),
            130,
            3,
            NodeType::CharNode4,
        );
        registry.register_char(
            char_path,
            make_typed_disk_ptr(2, 20, NodeType::CharNode16),
            260,
            5,
            NodeType::CharNode16,
        );
        assert_eq!(registry.char_len(), 1);
        assert_eq!(registry.char_paths.stored_entries(), 1);
        let chars = registry
            .get_char_owned(char_hash)
            .expect("replacement char record");
        assert_eq!(
            chars.disk_ptr.to_raw(),
            make_typed_disk_ptr(2, 20, NodeType::CharNode16).to_raw()
        );
        assert_eq!(chars.size_bytes, 260);
        assert_eq!(chars.depth, 5);
        assert_eq!(registry.total_size_bytes(), 480);
    }

    #[test]
    fn same_dense_id_and_hash_replacement_is_transactional() {
        let mut registry = DiskLocationRegistry::new();
        let byte_id = registry
            .try_reserve_byte_path(RegistryPathId::ROOT, b"same")
            .expect("reserve byte path");
        registry
            .register_byte_path(
                byte_id,
                make_typed_disk_ptr(1, 1, NodeType::Node4),
                10,
                4,
                NodeType::Node4,
            )
            .expect("initial byte record");
        registry
            .register_byte_path(byte_id, make_disk_ptr(1, 2), 20, 4, NodeType::Node16)
            .expect("replace byte record");

        let char_id = registry
            .try_reserve_char_units(RegistryPathId::ROOT, &[0x03BB])
            .expect("reserve char path");
        registry
            .register_char_path(
                char_id,
                make_typed_disk_ptr(2, 1, NodeType::CharNode4),
                30,
                1,
                NodeType::CharNode4,
            )
            .expect("initial char record");
        registry
            .register_char_path(
                char_id,
                make_typed_disk_ptr(2, 2, NodeType::CharNode16),
                40,
                1,
                NodeType::CharNode16,
            )
            .expect("replace char record");

        assert_eq!(registry.len(), 1);
        assert_eq!(registry.char_len(), 1);
        assert_eq!(registry.total_size_bytes(), 60);
        assert_eq!(
            registry
                .get_owned(LruRegistry::path_hash(b"same"))
                .expect("byte replacement")
                .disk_ptr
                .to_raw(),
            make_disk_ptr(1, 2).to_raw()
        );
        assert_eq!(
            registry
                .get_char_owned(super::super::lru_tracker::hash_char_path(&['λ']))
                .expect("char replacement")
                .disk_ptr
                .to_raw(),
            make_typed_disk_ptr(2, 2, NodeType::CharNode16).to_raw()
        );
    }

    #[test]
    fn test_registry_remove() {
        let mut registry = DiskLocationRegistry::new();

        registry.register(
            b"node1".to_vec(),
            make_typed_disk_ptr(1, 100, NodeType::Node4),
            256,
            1,
            NodeType::Node4,
        );
        registry.register(
            b"node2".to_vec(),
            make_disk_ptr(1, 200),
            512,
            2,
            NodeType::Node16,
        );

        assert_eq!(registry.len(), 2);
        assert_eq!(registry.total_size_bytes(), 768);

        let hash1 = LruRegistry::path_hash(b"node1");
        let removed = registry.remove(hash1);
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().size_bytes, 256);

        assert_eq!(registry.len(), 1);
        assert_eq!(registry.total_size_bytes(), 512);
        assert_eq!(registry.count_by_type(NodeType::Node4), 0);
        assert_eq!(registry.count_by_type(NodeType::Node16), 1);
    }

    #[test]
    fn test_registry_invalidate() {
        let mut registry = DiskLocationRegistry::new();
        registry.register(
            b"test".to_vec(),
            make_disk_ptr(1, 100),
            256,
            1,
            NodeType::Node16,
        );

        assert!(registry.is_valid());
        assert!(!registry.is_authoritative());
        assert!(registry.is_publication_candidate());

        registry.invalidate();
        assert!(!registry.is_valid());

        registry.clear();
        assert!(registry.is_valid());
        assert!(!registry.is_authoritative());
        assert!(registry.is_publication_candidate());
        assert!(registry.is_empty());
    }
    #[test]
    fn invalidated_or_mismatched_publication_cannot_be_reactivated() {
        let mut registry = DiskLocationRegistry::new();
        let binding = registry.binding();
        registry.begin_prepared_publication();
        registry.invalidate();
        assert!(!registry.try_finish_prepared_publication(&binding));
        assert!(!registry.is_valid());

        registry.clear();
        let wrong = RegistryGeneration::new();
        registry.begin_prepared_publication();
        assert!(!registry.try_finish_prepared_publication(&wrong));
        assert!(!registry.is_valid());

        registry.clear();
        assert!(registry.is_valid());
        assert!(!registry.is_authoritative());
        assert!(registry.is_publication_candidate());
    }

    #[test]
    fn test_eviction_candidates() {
        let mut registry = DiskLocationRegistry::new();

        // Add nodes at different depths
        for depth in 0..5 {
            let path = format!("depth{}", depth);
            registry.register(
                path.into_bytes(),
                make_disk_ptr(1, depth as u32 * 100),
                256,
                depth,
                NodeType::Node16,
            );
        }

        assert_eq!(registry.len(), 5);

        // Min depth 0 should include all
        let candidates = registry.eviction_candidates(0);
        assert_eq!(candidates.len(), 5);

        // Min depth 2 should exclude depths 0 and 1
        let candidates = registry.eviction_candidates(2);
        assert_eq!(candidates.len(), 3);

        // Min depth 5 should include none
        let candidates = registry.eviction_candidates(5);
        assert_eq!(candidates.len(), 0);
    }

    #[test]
    fn test_select_for_eviction() {
        let mut registry = DiskLocationRegistry::new();
        let lru = LruRegistry::new();

        // Add nodes with different sizes
        for i in 0..10 {
            let path = format!("node{}", i);
            registry.register(
                path.clone().into_bytes(),
                make_disk_ptr(1, i * 100),
                100 * (i as usize + 1), // Sizes: 100, 200, 300, ...
                1,
                NodeType::Node16,
            );

            // Touch in LRU to create different access patterns
            // Earlier nodes are touched less (colder)
            for _ in 0..i {
                lru.touch(path.as_bytes());
            }
        }
        registry
            .try_finalize_for_publication()
            .expect("finalize eviction-selection registry");

        // Select nodes to free 500 bytes
        let selected = registry.select_for_eviction(500, &lru, 1, 5, 0);

        // Should select coldest nodes first
        assert!(!selected.is_empty());

        let total_bytes: usize = selected.iter().map(|(_, n)| n.size_bytes).sum();
        assert!(total_bytes >= 500 || selected.len() >= 5);
    }

    #[test]
    fn select_for_eviction_respects_small_cap_without_full_result() {
        let mut registry = DiskLocationRegistry::new();
        let lru = LruRegistry::new();

        for i in 0..32 {
            let path = format!("node{i:02}");
            registry.register(
                path.clone().into_bytes(),
                make_disk_ptr(1, i * 16),
                100,
                1,
                NodeType::Node16,
            );
            for _ in 0..i {
                lru.touch(path.as_bytes());
            }
        }
        registry
            .try_finalize_for_publication()
            .expect("finalize capped byte-selection registry");

        let selected = registry.select_for_eviction(10_000, &lru, 1, 3, 0);
        assert_eq!(selected.len(), 3);

        let selected_paths: Vec<_> = selected
            .into_iter()
            .map(|(_, node)| String::from_utf8(node.path).expect("test path utf8"))
            .collect();

        for path in selected_paths {
            assert_ne!(path, "node31", "hottest node must not be selected");
        }
    }

    #[test]
    fn select_char_for_eviction_respects_small_cap_without_full_result() {
        let mut registry = DiskLocationRegistry::new();
        let lru = LruRegistry::new();

        for i in 0..32 {
            let path = vec!['節', char::from_u32('a' as u32 + i).expect("ascii char")];
            registry.register_char(
                path.clone(),
                make_typed_disk_ptr(1, i * 16, NodeType::CharNode16),
                100,
                1,
                NodeType::CharNode16,
            );
            for _ in 0..i {
                lru.touch_hash(super::super::lru_tracker::hash_char_path(&path));
            }
        }
        registry
            .try_finalize_for_publication()
            .expect("finalize capped char-selection registry");

        let selected = registry.select_char_for_eviction(10_000, &lru, 1, 3, 0);
        assert_eq!(selected.len(), 3);

        let hottest = vec!['節', char::from_u32('a' as u32 + 31).expect("ascii char")];
        for (_, node) in selected {
            assert_ne!(node.path, hottest, "hottest char node must not be selected");
        }
    }

    #[test]
    fn every_selector_enforces_an_ordinary_ratio_cap() {
        let mut registry = DiskLocationRegistry::new();
        let lru = LruRegistry::new();
        for i in 0..10u32 {
            registry.register(
                vec![b'a' + i as u8],
                make_typed_disk_ptr(1, i * 16, NodeType::Node4),
                100,
                1,
                NodeType::Node4,
            );
            registry.register_char(
                vec![char::from_u32(0x03B1 + i).expect("valid Greek scalar")],
                make_typed_disk_ptr(2, i * 16, NodeType::CharNode4),
                100,
                1,
                NodeType::CharNode4,
            );
        }
        registry
            .try_finalize_for_publication()
            .expect("finalize ratio-cap registry");
        make_authoritative(&mut registry);

        assert_eq!(
            registry
                .select_for_eviction(usize::MAX, &lru, 0, 4, 0)
                .len(),
            4
        );
        assert_eq!(
            registry
                .select_char_for_eviction(usize::MAX, &lru, 0, 4, 0)
                .len(),
            4
        );
        assert_eq!(
            registry
                .select_compact_for_eviction(usize::MAX, &lru, 0, 4, 0)
                .candidates
                .len(),
            4
        );
        assert_eq!(
            registry
                .select_compact_char_for_eviction(usize::MAX, &lru, 0, 4, 0)
                .candidates
                .len(),
            4
        );
    }

    #[test]
    fn zero_byte_target_is_a_no_op_for_every_compact_selector() {
        let mut registry = DiskLocationRegistry::new();
        registry.register(
            b"byte".to_vec(),
            make_typed_disk_ptr(1, 16, NodeType::Node4),
            64,
            1,
            NodeType::Node4,
        );
        registry.register_char(
            vec!['字'],
            make_typed_disk_ptr(2, 16, NodeType::CharNode4),
            80,
            1,
            NodeType::CharNode4,
        );
        registry
            .try_finalize_for_publication()
            .expect("finalize zero-target registry");
        make_authoritative(&mut registry);
        let lru = LruRegistry::new();

        assert!(registry
            .select_compact_for_eviction(0, &lru, 0, usize::MAX, 0)
            .candidates
            .is_empty());
        assert!(registry
            .select_compact_char_for_eviction(0, &lru, 0, usize::MAX, 0)
            .candidates
            .is_empty());
    }

    #[test]
    fn compact_selection_reports_size_overflow_instead_of_saturating() {
        let mut topology = PathTopology::new();
        let path_id = topology
            .try_reserve_path(
                RegistryPathId::ROOT,
                b"x",
                super::super::lru_tracker::PATH_HASH_OFFSET,
                super::super::lru_tracker::extend_byte_path_hash,
            )
            .expect("one-entry topology");
        topology
            .try_finalize_subtree_ends()
            .expect("finalize one-entry topology");
        let topology = Arc::new(topology);
        let generation = RegistryGeneration::new();
        let candidate = CompactEvictionCandidate {
            path_id,
            path_hash: topology
                .hash(path_id, super::super::lru_tracker::PATH_HASH_OFFSET)
                .expect("path hash"),
            disk_ptr: make_typed_disk_ptr(1, 32, NodeType::Node4),
            size_bytes: usize::MAX,
            depth: 1,
            node_type: NodeType::Node4,
        };

        let error = try_select_compact_batch(
            true,
            &topology,
            &generation,
            std::iter::once(candidate),
            &LruRegistry::new(),
            CompactSelectionLimits {
                target_bytes: usize::MAX,
                max_count: 1,
                overhead: 1,
            },
        )
        .err()
        .expect("size overflow must fail");
        assert_eq!(error, CompactSelectionError::SizeOverflow);
    }

    #[test]
    fn test_char_registry() {
        let mut registry = DiskLocationRegistry::new();

        registry.register_char(
            vec!['日', '本', '語'],
            make_typed_disk_ptr(1, 100, NodeType::CharNode16),
            512,
            1,
            NodeType::CharNode16,
        );

        assert_eq!(registry.char_len(), 1);
        assert_eq!(registry.total_size_bytes(), 512);
        assert_eq!(registry.count_by_type(NodeType::CharNode16), 1);

        use super::super::lru_tracker::hash_char_path;
        let hash = hash_char_path(&['日', '本', '語']);
        let node = registry
            .get_char_owned(hash)
            .expect("char node should exist");
        assert_eq!(node.path, vec!['日', '本', '語']);
    }

    #[test]
    fn deep_prefixes_share_linear_byte_and_char_path_storage() {
        const DEPTH: usize = 100_000;
        const STRIDE: usize = 13;

        let mut registry = DiskLocationRegistry::new();
        let mut byte_cursor = RegistryPathId::ROOT;
        let mut char_cursor = RegistryPathId::ROOT;

        let byte_units = [b'x'; STRIDE];
        let char_units = ['λ'; STRIDE];
        let mut depth = 0usize;
        while depth < DEPTH {
            let width = STRIDE.min(DEPTH - depth);
            byte_cursor = registry
                .try_reserve_byte_path(byte_cursor, &byte_units[..width])
                .expect("reserve byte segment");
            char_cursor = registry
                .try_reserve_char_path(char_cursor, &char_units[..width])
                .expect("reserve char segment");
            depth += width;
            registry
                .register_byte_path(
                    byte_cursor,
                    make_typed_disk_ptr(1, depth as u32, NodeType::Node4),
                    32,
                    depth,
                    NodeType::Node4,
                )
                .expect("register byte prefix");
            registry
                .register_char_path(
                    char_cursor,
                    SwizzledPtr::on_disk(2, depth as u32, NodeType::CharNode4),
                    48,
                    depth,
                    NodeType::CharNode4,
                )
                .expect("register char prefix");
        }

        assert_eq!(registry.byte_paths.stored_units(), DEPTH);
        assert_eq!(registry.char_paths.stored_units(), DEPTH);
        assert_eq!(registry.byte_paths.stored_entries(), DEPTH.div_ceil(STRIDE));
        assert_eq!(registry.char_paths.stored_entries(), DEPTH.div_ceil(STRIDE));
        assert_eq!(
            registry
                .byte_paths
                .materialize(byte_cursor)
                .map(|path| path.len()),
            Some(DEPTH)
        );
        assert_eq!(
            registry
                .char_paths
                .materialize(char_cursor)
                .map(|path| path.len()),
            Some(DEPTH)
        );
    }

    #[test]
    fn hash_collisions_retain_every_exact_byte_and_char_record() {
        const COLLIDING_HASH: u64 = 0x5a5a_a5a5_dead_beef;
        let mut registry = DiskLocationRegistry::new();

        let byte_a = registry
            .try_reserve_byte_path(RegistryPathId::ROOT, b"alpha")
            .expect("reserve byte alpha");
        let byte_b = registry
            .try_reserve_byte_path(RegistryPathId::ROOT, b"beta")
            .expect("reserve byte beta");
        registry
            .insert_byte_node(test_admission(
                byte_a,
                COLLIDING_HASH,
                make_typed_disk_ptr(4, 10, NodeType::Node4),
                10,
                5,
                NodeType::Node4,
            ))
            .expect("register byte alpha");
        registry
            .insert_byte_node(test_admission(
                byte_b,
                COLLIDING_HASH,
                make_typed_disk_ptr(4, 20, NodeType::Node4),
                20,
                4,
                NodeType::Node4,
            ))
            .expect("register byte beta");

        let char_a = registry
            .try_reserve_char_path(RegistryPathId::ROOT, &['α'])
            .expect("reserve char alpha");
        let char_b = registry
            .try_reserve_char_path(RegistryPathId::ROOT, &['β'])
            .expect("reserve char beta");
        registry
            .insert_char_node(test_admission(
                char_a,
                COLLIDING_HASH,
                SwizzledPtr::on_disk(5, 10, NodeType::CharNode4),
                30,
                1,
                NodeType::CharNode4,
            ))
            .expect("register char alpha");
        registry
            .insert_char_node(test_admission(
                char_b,
                COLLIDING_HASH,
                SwizzledPtr::on_disk(5, 20, NodeType::CharNode4),
                40,
                1,
                NodeType::CharNode4,
            ))
            .expect("register char beta");

        assert_eq!(registry.len(), 2);
        assert_eq!(registry.char_len(), 2);
        assert_eq!(registry.byte_hash_index[&COLLIDING_HASH].len(), 2);
        assert_eq!(registry.char_hash_index[&COLLIDING_HASH].len(), 2);

        let byte_lookup = registry
            .get_owned(COLLIDING_HASH)
            .expect("last byte collision occurrence");
        assert_eq!(byte_lookup.path, b"beta");
        assert_eq!(
            byte_lookup.disk_ptr.to_raw(),
            make_typed_disk_ptr(4, 20, NodeType::Node4).to_raw()
        );
        let byte_beta = registry
            .remove(COLLIDING_HASH)
            .expect("remove last byte collision occurrence");
        assert_eq!(byte_beta.path, b"beta");
        assert_eq!(
            registry
                .get_owned(COLLIDING_HASH)
                .expect("preceding byte collision occurrence")
                .path,
            b"alpha"
        );
        assert!(registry.contains(COLLIDING_HASH));
        let byte_alpha = registry
            .remove(COLLIDING_HASH)
            .expect("remove preceding byte collision occurrence");
        assert_eq!(byte_alpha.path, b"alpha");
        assert_eq!(registry.len(), 0);
        assert!(registry.contains(COLLIDING_HASH));

        let char_lookup = registry
            .get_char_owned(COLLIDING_HASH)
            .expect("last char collision occurrence");
        assert_eq!(char_lookup.path, vec!['β']);
        assert_eq!(
            char_lookup.disk_ptr.to_raw(),
            SwizzledPtr::on_disk(5, 20, NodeType::CharNode4).to_raw()
        );
        let char_beta = registry
            .remove_char(COLLIDING_HASH)
            .expect("remove last char collision occurrence");
        assert_eq!(char_beta.path, vec!['β']);
        assert_eq!(
            registry
                .get_char_owned(COLLIDING_HASH)
                .expect("preceding char collision occurrence")
                .path,
            vec!['α']
        );
        assert!(registry.contains(COLLIDING_HASH));
        let char_alpha = registry
            .remove_char(COLLIDING_HASH)
            .expect("remove preceding char collision occurrence");
        assert_eq!(char_alpha.path, vec!['α']);
        assert!(!registry.contains(COLLIDING_HASH));
    }

    #[test]
    fn failed_char_materialization_leaves_lookup_and_remove_state_unchanged() {
        let mut registry = DiskLocationRegistry::new();
        let invalid_id = Arc::get_mut(&mut registry.char_paths)
            .expect("unshared test topology")
            .try_reserve_path(
                RegistryPathId::ROOT,
                &[0xD800],
                super::super::lru_tracker::PATH_HASH_OFFSET,
                super::super::lru_tracker::extend_char_unit_hash,
            )
            .expect("reserve invalid scalar fixture through the internal seam");
        let invalid_hash = registry
            .char_paths
            .hash(invalid_id, super::super::lru_tracker::PATH_HASH_OFFSET)
            .expect("invalid fixture hash");
        let disk_ptr = SwizzledPtr::on_disk(7, 70, NodeType::CharNode4);
        registry
            .insert_char_node(test_admission(
                invalid_id,
                invalid_hash,
                disk_ptr.clone(),
                37,
                1,
                NodeType::CharNode4,
            ))
            .expect("register invalid materialization fixture");

        let before_bucket = registry.char_hash_index[&invalid_hash].clone();
        let before_disk_bucket = registry.char_disk_index
            [&registry_disk_address(&disk_ptr).expect("durable fixture")]
            .clone();
        let before_authority = registry.authority;
        let before_binding = registry.binding();
        let before_raw = registry.char_locations[invalid_id.0]
            .as_ref()
            .expect("fixture entry")
            .disk_ptr
            .to_raw();

        assert!(registry.get_char_owned(invalid_hash).is_none());
        assert!(registry.remove_char(invalid_hash).is_none());

        assert_eq!(registry.char_len(), 1);
        assert_eq!(registry.total_size_bytes(), 37);
        assert_eq!(registry.count_by_type(NodeType::CharNode4), 1);
        assert_eq!(registry.char_resident_len(), 1);
        assert_eq!(registry.char_resident_serialized_bytes(), 37);
        assert_eq!(registry.char_hash_index[&invalid_hash], before_bucket);
        assert_eq!(
            registry.char_disk_index[&registry_disk_address(&disk_ptr).expect("durable fixture")],
            before_disk_bucket
        );
        assert_eq!(registry.authority, before_authority);
        assert!(registry.binding().same_publication(&before_binding));
        assert_eq!(
            registry.char_locations[invalid_id.0]
                .as_ref()
                .expect("fixture remains present")
                .disk_ptr
                .to_raw(),
            before_raw
        );
    }

    #[test]
    fn owned_remove_matches_prestate_lookup_for_byte_and_char() {
        let mut registry = DiskLocationRegistry::new();
        let byte_path = b"owned-byte".to_vec();
        let byte_hash = LruRegistry::path_hash(&byte_path);
        registry.register(
            byte_path,
            make_typed_disk_ptr(8, 80, NodeType::Node16),
            83,
            10,
            NodeType::Node16,
        );
        let char_path = vec!['所', '有'];
        let char_hash = super::super::lru_tracker::hash_char_path(&char_path);
        registry.register_char(
            char_path,
            SwizzledPtr::on_disk(9, 90, NodeType::CharNode16),
            97,
            2,
            NodeType::CharNode16,
        );

        let byte_before = registry
            .get_owned(byte_hash)
            .expect("pre-remove byte lookup");
        let byte_removed = registry.remove(byte_hash).expect("owned byte removal");
        assert_eq!(byte_removed.path, byte_before.path);
        assert_eq!(
            byte_removed.disk_ptr.to_raw(),
            byte_before.disk_ptr.to_raw()
        );
        assert_eq!(byte_removed.size_bytes, byte_before.size_bytes);
        assert_eq!(byte_removed.depth, byte_before.depth);
        assert_eq!(byte_removed.node_type, byte_before.node_type);

        let char_before = registry
            .get_char_owned(char_hash)
            .expect("pre-remove char lookup");
        let char_removed = registry.remove_char(char_hash).expect("owned char removal");
        assert_eq!(char_removed.path, char_before.path);
        assert_eq!(
            char_removed.disk_ptr.to_raw(),
            char_before.disk_ptr.to_raw()
        );
        assert_eq!(char_removed.size_bytes, char_before.size_bytes);
        assert_eq!(char_removed.depth, char_before.depth);
        assert_eq!(char_removed.node_type, char_before.node_type);
    }
}
