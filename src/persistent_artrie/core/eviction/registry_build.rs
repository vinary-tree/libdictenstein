//! Stack-safe reconstruction primitives for checkpoint eviction registries.
//!
//! Persistent storage identity is the arena record address, not the complete
//! encoded [`SwizzledPtr`]. This distinction is essential for the existing
//! character-node v2 format: relative child references encode an arena address
//! but do not encode the child's node type. The referenced record header is the
//! authoritative source of that type.

use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::persistent_artrie::swizzled_ptr::{NodeType, SwizzledPtr};
use std::collections::HashSet;

/// Type-independent identity of one persistent arena record.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct DiskRecordAddress {
    pub(crate) block_id: u32,
    pub(crate) slot_id: u32,
}

impl DiskRecordAddress {
    /// Extract an arena record address from a non-null on-disk pointer.
    pub(crate) fn from_pointer(pointer: &SwizzledPtr) -> Result<Self> {
        let location = pointer.disk_location().ok_or_else(|| {
            PersistentARTrieError::corrupted(
                "durable record pointer is null, in memory, transitional, or malformed",
            )
        })?;
        if location.block_id == 0 {
            return Err(PersistentARTrieError::corrupted(
                "durable arena record uses reserved block zero",
            ));
        }
        Ok(Self {
            block_id: location.block_id,
            slot_id: location.offset,
        })
    }

    /// Construct the canonical typed pointer after a record header has supplied
    /// the authoritative node type.
    pub(crate) fn canonical_pointer(self, node_type: NodeType) -> Result<SwizzledPtr> {
        SwizzledPtr::try_on_disk(self.block_id, self.slot_id, node_type)
            .map_err(PersistentARTrieError::from)
    }
}

/// A durable edge whose address is known before the child record is read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DurableRecordRef {
    pub(crate) address: DiskRecordAddress,
    /// `None` means the parent format omitted the child type; the child header
    /// must determine it. `Some` is checked against that header.
    pub(crate) expected_type: Option<NodeType>,
}

impl DurableRecordRef {
    /// Build a typed reference from a byte or fixed-width character pointer.
    pub(crate) fn from_typed_pointer(pointer: &SwizzledPtr) -> Result<Self> {
        let location = pointer.disk_location().ok_or_else(|| {
            PersistentARTrieError::corrupted(
                "typed durable child pointer is null, in memory, transitional, or malformed",
            )
        })?;
        Ok(Self {
            address: DiskRecordAddress::from_pointer(pointer)?,
            expected_type: Some(location.node_type),
        })
    }

    /// Build a reference for a format that stores only the child address.
    pub(crate) const fn untyped(address: DiskRecordAddress) -> Self {
        Self {
            address,
            expected_type: None,
        }
    }
}

/// Owned metadata for one durable record, excluding its application value.
pub(crate) struct DurableRegistryRecord<U> {
    pub(crate) canonical_ptr: SwizzledPtr,
    pub(crate) address: DiskRecordAddress,
    pub(crate) node_type: NodeType,
    pub(crate) serialized_bytes: usize,
    pub(crate) prefix: Vec<U>,
    pub(crate) children: Vec<(U, DurableRecordRef)>,
}

/// Exact work performed by one durable-subtree registry scan.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct DurableRegistryScanStats {
    pub(crate) topology_entries: usize,
    pub(crate) durable_records: usize,
    pub(crate) serialized_bytes: usize,
}

/// One mutation requested from a durable-registry scan sink.
///
/// A compressed record's prefix has no durable record of its own. The scanner
/// therefore reserves a locationless shared prefix only for actual fanout. A
/// unary record reserves `prefix ++ edge` as one segment, avoiding a locationless
/// unary node that canonical graft validation would reject. The two borrowed
/// pieces are never concatenated into a temporary vector.
pub(crate) enum DurableRegistryScanEvent<'a, U> {
    ReservePath {
        prefix: &'a [U],
        edge: Option<U>,
    },
    RegisterRecord {
        resident: bool,
        record: &'a DurableRegistryRecord<U>,
    },
}

/// One active durable record. Only the next child index is retained; siblings
/// are never expanded into separate pending heap objects.
struct ScanFrame<U, P> {
    address: DiskRecordAddress,
    child_parent: P,
    child_prefix: Vec<U>,
    children: Vec<(U, DurableRecordRef)>,
    next_child: usize,
}

fn scan_allocation_failed(
    component: &'static str,
    requested: usize,
    error: std::collections::TryReserveError,
) -> PersistentARTrieError {
    PersistentARTrieError::allocation_failed(component, requested, error)
}

/// Reconstruct one durable subtree with constant native-stack use.
///
/// The record graph is traversed in deterministic edge preorder by an explicit
/// active-frame machine. `active_ancestry` deliberately tracks only the current
/// DFS ancestry: seeing the same address below itself is a cycle and fails,
/// while seeing it later under a sibling is valid DAG aliasing and produces a
/// second path occurrence. No global visited set is used because storage
/// identity and registry path identity are different domains.
///
/// `root_path` already names the incoming edge to the durable root. A compressed
/// record reserves its prefix once beneath that node and reserves each child as
/// a one-unit edge beneath the shared prefix. Thus a width-`w` record retains
/// `O(w + |prefix|)` topology storage rather than `O(w * |prefix|)` storage.
pub(crate) fn scan_durable_registry_subtree<U, P, ReadRecord, ApplyRecord>(
    root: DurableRecordRef,
    root_path: P,
    root_resident: bool,
    mut read_record: ReadRecord,
    mut apply_record: ApplyRecord,
) -> Result<DurableRegistryScanStats>
where
    U: Copy + Ord,
    P: Copy,
    ReadRecord: FnMut(DurableRecordRef) -> Result<DurableRegistryRecord<U>>,
    ApplyRecord: FnMut(P, DurableRegistryScanEvent<'_, U>) -> Result<P>,
{
    let mut stack: Vec<ScanFrame<U, P>> = Vec::new();
    stack.try_reserve(1).map_err(|error| {
        scan_allocation_failed("durable registry scan active-frame stack", 1, error)
    })?;

    let mut active_ancestry = HashSet::new();
    active_ancestry.try_reserve(1).map_err(|error| {
        scan_allocation_failed("durable registry scan active ancestry", 1, error)
    })?;
    let mut stats = DurableRegistryScanStats::default();
    let mut pending = Some((root, root_path, root_resident));

    loop {
        if let Some((reference, path, resident)) = pending.take() {
            if reference.address.block_id == 0 {
                return Err(PersistentARTrieError::corrupted(
                    "durable registry scan reached reserved block zero",
                ));
            }
            if active_ancestry.contains(&reference.address) {
                return Err(PersistentARTrieError::corrupted(format!(
                    "durable registry record cycle reaches block {} slot {} below itself",
                    reference.address.block_id, reference.address.slot_id
                )));
            }

            let record = read_record(reference)?;
            if record.address != reference.address {
                return Err(PersistentARTrieError::corrupted(
                    "durable registry reader returned a different record address",
                ));
            }
            if record.serialized_bytes == 0 {
                return Err(PersistentARTrieError::corrupted(
                    "durable registry record has zero serialized length",
                ));
            }
            if let Some(expected_type) = reference.expected_type {
                if expected_type != record.node_type {
                    return Err(PersistentARTrieError::NodeTypeMismatch {
                        expected: format!("{expected_type:?}"),
                        found: format!("{:?}", record.node_type),
                    });
                }
            }
            let canonical_address = DiskRecordAddress::from_pointer(&record.canonical_ptr)?;
            let canonical_type = record
                .canonical_ptr
                .disk_location()
                .ok_or_else(|| {
                    PersistentARTrieError::corrupted(
                        "durable registry reader returned a non-disk canonical pointer",
                    )
                })?
                .node_type;
            if canonical_address != record.address || canonical_type != record.node_type {
                return Err(PersistentARTrieError::corrupted(
                    "durable registry reader returned inconsistent canonical identity",
                ));
            }
            if record
                .children
                .windows(2)
                .any(|pair| pair[0].0 >= pair[1].0)
            {
                return Err(PersistentARTrieError::corrupted(
                    "durable registry record child edges are not strictly increasing",
                ));
            }

            let registered_path = apply_record(
                path,
                DurableRegistryScanEvent::RegisterRecord {
                    resident,
                    record: &record,
                },
            )?;
            stats.durable_records = stats.durable_records.checked_add(1).ok_or_else(|| {
                PersistentARTrieError::corrupted("durable registry scan record count overflow")
            })?;
            stats.serialized_bytes = stats
                .serialized_bytes
                .checked_add(record.serialized_bytes)
                .ok_or_else(|| {
                    PersistentARTrieError::corrupted(
                        "durable registry scan serialized-byte count overflow",
                    )
                })?;

            let requested_ancestors = active_ancestry.len().checked_add(1).ok_or_else(|| {
                PersistentARTrieError::corrupted(
                    "durable registry scan active-ancestry count overflow",
                )
            })?;
            active_ancestry.try_reserve(1).map_err(|error| {
                scan_allocation_failed(
                    "durable registry scan active ancestry",
                    requested_ancestors,
                    error,
                )
            })?;
            if !active_ancestry.insert(record.address) {
                return Err(PersistentARTrieError::corrupted(
                    "durable registry record became cyclic during scan admission",
                ));
            }

            if record.children.is_empty() {
                if !active_ancestry.remove(&record.address) {
                    return Err(PersistentARTrieError::corrupted(
                        "durable registry leaf did not match its active ancestor",
                    ));
                }
                continue;
            }

            let (child_parent, child_prefix) =
                if !record.prefix.is_empty() && record.children.len() > 1 {
                    stats.topology_entries =
                        stats.topology_entries.checked_add(1).ok_or_else(|| {
                            PersistentARTrieError::corrupted(
                                "durable registry scan topology-entry count overflow",
                            )
                        })?;
                    let child_parent = apply_record(
                        registered_path,
                        DurableRegistryScanEvent::ReservePath {
                            prefix: &record.prefix,
                            edge: None,
                        },
                    )?;
                    (child_parent, Vec::new())
                } else {
                    (registered_path, record.prefix)
                };
            let requested_frames = stack.len().checked_add(1).ok_or_else(|| {
                PersistentARTrieError::corrupted(
                    "durable registry scan active-frame count overflow",
                )
            })?;
            stack.try_reserve(1).map_err(|error| {
                scan_allocation_failed(
                    "durable registry scan active-frame stack",
                    requested_frames,
                    error,
                )
            })?;
            stack.push(ScanFrame {
                address: record.address,
                child_parent,
                child_prefix,
                children: record.children,
                next_child: 0,
            });
            continue;
        }

        let Some(frame) = stack.last_mut() else {
            break;
        };
        if let Some(&(edge, child)) = frame.children.get(frame.next_child) {
            frame.next_child = frame.next_child.checked_add(1).ok_or_else(|| {
                PersistentARTrieError::corrupted("durable registry scan child cursor overflow")
            })?;
            if child.address.block_id == 0 {
                return Err(PersistentARTrieError::corrupted(
                    "durable registry child references reserved block zero",
                ));
            }
            stats.topology_entries = stats.topology_entries.checked_add(1).ok_or_else(|| {
                PersistentARTrieError::corrupted(
                    "durable registry scan topology-entry count overflow",
                )
            })?;
            let child_path = apply_record(
                frame.child_parent,
                DurableRegistryScanEvent::ReservePath {
                    prefix: &frame.child_prefix,
                    edge: Some(edge),
                },
            )?;
            pending = Some((child, child_path, false));
        } else {
            let completed = stack.pop().ok_or_else(|| {
                PersistentARTrieError::corrupted(
                    "durable registry scanner lost its non-empty active stack",
                )
            })?;
            if !active_ancestry.remove(&completed.address) {
                return Err(PersistentARTrieError::corrupted(
                    "durable registry scan frame did not match its active ancestor",
                ));
            }
        }
    }

    if !active_ancestry.is_empty() {
        return Err(PersistentARTrieError::corrupted(
            "durable registry scan ended with active ancestors",
        ));
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(slot_id: u32) -> DiskRecordAddress {
        DiskRecordAddress {
            block_id: 1,
            slot_id,
        }
    }

    fn untyped(slot_id: u32) -> DurableRecordRef {
        DurableRecordRef::untyped(address(slot_id))
    }

    fn record(
        slot_id: u32,
        prefix: &[u8],
        children: Vec<(u8, DurableRecordRef)>,
    ) -> DurableRegistryRecord<u8> {
        let address = address(slot_id);
        DurableRegistryRecord {
            canonical_ptr: address
                .canonical_pointer(NodeType::Node4)
                .expect("test record pointer"),
            address,
            node_type: NodeType::Node4,
            serialized_bytes: 10,
            prefix: prefix.to_vec(),
            children,
        }
    }

    #[test]
    fn iterative_scan_expands_compressed_segments_and_accepts_sibling_aliases() {
        let root = DurableRecordRef {
            address: address(1),
            expected_type: Some(NodeType::Node4),
        };
        let mut paths = vec![b"r".to_vec()];
        let mut occurrences = Vec::new();
        let stats = scan_durable_registry_subtree(
            root,
            0usize,
            true,
            |reference| match reference.address.slot_id {
                1 => Ok(record(
                    1,
                    b"p",
                    vec![(b'a', untyped(2)), (b'b', untyped(2))],
                )),
                2 => Ok(record(2, b"", Vec::new())),
                slot => Err(PersistentARTrieError::corrupted(format!(
                    "unexpected test slot {slot}"
                ))),
            },
            |path_or_parent, event| match event {
                DurableRegistryScanEvent::ReservePath { prefix, edge } => {
                    let mut path = paths[path_or_parent].clone();
                    path.extend_from_slice(prefix);
                    path.extend(edge);
                    paths.push(path);
                    Ok(paths.len() - 1)
                }
                DurableRegistryScanEvent::RegisterRecord { resident, record } => {
                    occurrences.push((paths[path_or_parent].clone(), record.address));
                    assert_eq!(resident, path_or_parent == 0);
                    Ok(path_or_parent)
                }
            },
        )
        .expect("scan sibling-alias graph");

        assert_eq!(stats.topology_entries, 3);
        assert_eq!(stats.durable_records, 3);
        assert_eq!(stats.serialized_bytes, 30);
        assert_eq!(
            occurrences,
            vec![
                (b"r".to_vec(), address(1)),
                (b"rpa".to_vec(), address(2)),
                (b"rpb".to_vec(), address(2)),
            ]
        );
    }

    #[test]
    fn unary_compressed_prefix_and_edge_form_one_topology_segment() {
        let root = DurableRecordRef {
            address: address(1),
            expected_type: Some(NodeType::Node4),
        };
        let mut paths = vec![b"r".to_vec()];
        let mut reservations = Vec::new();
        let mut occurrences = Vec::new();

        let stats = scan_durable_registry_subtree(
            root,
            0usize,
            true,
            |reference| match reference.address.slot_id {
                1 => Ok(record(1, b"pq", vec![(b'x', untyped(2))])),
                2 => Ok(record(2, b"", Vec::new())),
                slot => Err(PersistentARTrieError::corrupted(format!(
                    "unexpected test slot {slot}"
                ))),
            },
            |path_or_parent, event| match event {
                DurableRegistryScanEvent::ReservePath { prefix, edge } => {
                    reservations.push((prefix.to_vec(), edge));
                    let mut path = paths[path_or_parent].clone();
                    path.extend_from_slice(prefix);
                    path.extend(edge);
                    paths.push(path);
                    Ok(paths.len() - 1)
                }
                DurableRegistryScanEvent::RegisterRecord { record, .. } => {
                    occurrences.push((paths[path_or_parent].clone(), record.address));
                    Ok(path_or_parent)
                }
            },
        )
        .expect("scan unary compressed record");

        assert_eq!(stats.topology_entries, 1);
        assert_eq!(stats.durable_records, 2);
        assert_eq!(reservations, vec![(b"pq".to_vec(), Some(b'x'))]);
        assert_eq!(paths, vec![b"r".to_vec(), b"rpqx".to_vec()]);
        assert_eq!(
            occurrences,
            vec![(b"r".to_vec(), address(1)), (b"rpqx".to_vec(), address(2)),]
        );
    }

    #[test]
    fn iterative_scan_rejects_an_address_repeated_on_active_ancestry() {
        let root = DurableRecordRef {
            address: address(1),
            expected_type: Some(NodeType::Node4),
        };
        let mut next_path = 1usize;
        let result = scan_durable_registry_subtree(
            root,
            0usize,
            false,
            |reference| match reference.address.slot_id {
                1 => Ok(record(1, b"", vec![(b'a', untyped(2))])),
                2 => Ok(record(2, b"", vec![(b'b', untyped(1))])),
                slot => Err(PersistentARTrieError::corrupted(format!(
                    "unexpected test slot {slot}"
                ))),
            },
            |path_or_parent, event| match event {
                DurableRegistryScanEvent::ReservePath { .. } => {
                    let result = next_path;
                    next_path += 1;
                    Ok(result)
                }
                DurableRegistryScanEvent::RegisterRecord { .. } => Ok(path_or_parent),
            },
        );

        assert!(matches!(
            result,
            Err(PersistentARTrieError::CorruptedFile { .. })
        ));
    }
}
