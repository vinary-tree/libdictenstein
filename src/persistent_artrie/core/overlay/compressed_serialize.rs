//! CX-universal: the ONE path-compressed overlay-checkpoint serializer, generic over
//! `K: KeyEncoding`.
//!
//! The post-order chain-peeling work-stack loop is byte-for-byte the same across
//! the persistent ARTrie variants (byte / char / vocab / u64) because they
//! operate on the unified [`OverlayNode<K, V>`]. Only the leaves differ, and they
//! differ by on-disk format (byte `PART` `Node` tiers, char `ARTC` `CharNode`
//! tiers, vocab sidecars, and native-u64 CX records). So the loop lives once
//! here (the default method
//! [`OverlayCompressedSerialize::serialize_compressed_loop`]) and each variant
//! supplies the format-specific seams: peel is shared ([`peel_chain_generic`]),
//! chunking is shared through the checked `chain_chunk_width`,
//! `chain_chunk_count`, and `chain_chunk_bounds` index laws, and the
//! per-variant trait methods cover node projection, single-node serialization,
//! and eviction durable-stamping.
//!
//! DATA-LOSS-CRITICAL: the edge convention (peel terminus = `num_children()!=1 || is_final ||
//! has_value`, OnDisk sole child ends the chain), the `K::MAX_PREFIX_LEN` chunk
//! width, and the `ends[c] = base+1+Σ_{i<c}(|P_i|+1)` true-depth
//! registry/stamp index are preserved from the proven byte/char/vocab originals
//! and are reused by the native-u64 profile.

/// The result of following a prefix link chain: the units consumed, every node
/// passed through, and the node the chain terminates at.
type PrefixChain<K, V> = (
    Vec<<K as KeyEncoding>::Unit>,
    Vec<Arc<OverlayNode<K, V>>>,
    Arc<OverlayNode<K, V>>,
);

use std::collections::{hash_map::Entry, HashMap};
use std::sync::Arc;

use smallvec::SmallVec;

use crate::persistent_artrie::core::eviction::{
    DiskLocationRegistry, DiskRecordAddress, DurableRecordRef, EvictionCoordinator,
    LocalRegistryGraftStats, PreparedRegistryPublication, RegistryBuilderSubtree,
    RegistryBuilderSubtreeStart, RegistryPathId, RegistryStructuralSource,
};
use crate::persistent_artrie::core::key_encoding::KeyEncoding;
use crate::persistent_artrie::core::overlay::node::OverlayNode;
use crate::persistent_artrie::core::overlay::{DeferredDurableStamp, RootRevision};
use crate::persistent_artrie::error::Result;
use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;
use crate::value::DictionaryValue;

enum RegistryBuildMode<K: KeyEncoding, V: DictionaryValue> {
    Disabled,
    Analysis {
        registry: DiskLocationRegistry,
    },
    Enabled {
        coordinator: Arc<EvictionCoordinator>,
        registry: DiskLocationRegistry,
        structural_source: Option<RegistryStructuralSource>,
        deferred_stamps: Vec<DeferredDurableStamp<K, V>>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CensusState {
    Active,
    Complete,
}

#[derive(Clone)]
pub(crate) struct CompletedSubtree {
    disk_ptr: SwizzledPtr,
    registry_span: Option<RegistryBuilderSubtree>,
}

/// One completed import of a nonresident durable record graph.
///
/// The map key is the type-independent arena address. Keeping node type out of
/// the key is deliberate: a second pointer that claims a different type for the
/// same record must be rejected, not admitted as an unrelated memo entry.
#[derive(Clone)]
struct CompletedOnDiskImport {
    canonical_ptr: SwizzledPtr,
    registry_span: RegistryBuilderSubtree,
}

/// Deterministic build-local evidence for durable-alias complexity tests.
#[cfg(any(test, feature = "perf-instrumentation"))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct OnDiskImportStats {
    lookups: usize,
    source_imports: usize,
    local_grafts: usize,
    local_graft_topology_entries: usize,
    local_graft_durable_records: usize,
    local_graft_serialized_bytes: usize,
    overflowed: bool,
}

#[derive(Clone)]
pub(crate) enum NodeBuildState {
    Unseen,
    Active,
    Complete(CompletedSubtree),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExpectedBuildState {
    Unseen,
    Active,
}

struct CensusEntry {
    incoming_edges: usize,
    census: CensusState,
}

struct CensusFrame<K: KeyEncoding, V: DictionaryValue> {
    node: Arc<OverlayNode<K, V>>,
    next_child: usize,
}

enum CensusStep<K: KeyEncoding, V: DictionaryValue> {
    Child(Option<Arc<OverlayNode<K, V>>>),
    Complete,
}

/// Sealed graph-shape policy for the shared serializer machine.
///
/// Production byte, character, and vocabulary roots use the zero-sized
/// arborescent policy. General internal graphs and native-u64 checkpoints use
/// the checked DAG policy, because native-u64 files legitimately preserve
/// shared acyclic suffixes.
pub(crate) trait GraphPolicy<K: KeyEncoding, V: DictionaryValue>: private::Sealed {
    fn prepare_graph(&mut self, root: &Arc<OverlayNode<K, V>>) -> Result<()>;

    fn is_compression_boundary(&self, node: &Arc<OverlayNode<K, V>>) -> Result<bool>;

    fn node_build_state(&self, node: &Arc<OverlayNode<K, V>>) -> Result<NodeBuildState>;

    fn mark_active(&mut self, node: &Arc<OverlayNode<K, V>>) -> Result<()>;

    fn mark_complete(
        &mut self,
        node: &Arc<OverlayNode<K, V>>,
        expected: ExpectedBuildState,
        disk_ptr: &SwizzledPtr,
        registry_span: Option<RegistryBuilderSubtree>,
    ) -> Result<()>;
}

mod private {
    pub trait Sealed {}
}

/// Zero-sized policy for a root whose unique-parent invariant is established
/// by its production trie owner.
pub(crate) struct ArborescentProduction;

impl private::Sealed for ArborescentProduction {}

/// Checked policy for arbitrary resident DAGs. It retains the existing census,
/// cycle rejection, compression-boundary, and completed-node memo semantics.
pub(crate) struct DagAware {
    nodes: HashMap<usize, NodeBuildState>,
    graph_prepared: bool,
}

impl DagAware {
    fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            graph_prepared: false,
        }
    }
}

impl private::Sealed for DagAware {}

/// Build-owned state for one stack-safe overlay serialization.
///
/// Registry construction, predecessor carry metadata, and deferred stamps are
/// one mode so callers cannot accidentally combine metadata from distinct
/// checkpoint generations or mutate live stamps before durability succeeds.
pub(crate) struct OverlaySerializationBuild<
    K: KeyEncoding,
    V: DictionaryValue,
    P: GraphPolicy<K, V>,
> {
    graph: P,
    on_disk_imports: HashMap<DiskRecordAddress, CompletedOnDiskImport>,
    #[cfg(any(test, feature = "perf-instrumentation"))]
    on_disk_import_stats: OnDiskImportStats,
    mode: RegistryBuildMode<K, V>,
}

impl<K: KeyEncoding, V: DictionaryValue> OverlaySerializationBuild<K, V, ArborescentProduction> {
    pub(crate) fn production_disabled() -> Self {
        Self {
            graph: ArborescentProduction,
            on_disk_imports: HashMap::new(),
            #[cfg(any(test, feature = "perf-instrumentation"))]
            on_disk_import_stats: OnDiskImportStats::default(),
            mode: RegistryBuildMode::Disabled,
        }
    }

    pub(crate) fn production_with_eviction(
        coordinator: Arc<EvictionCoordinator>,
        structural_source: Option<RegistryStructuralSource>,
    ) -> Self {
        Self {
            graph: ArborescentProduction,
            on_disk_imports: HashMap::new(),
            #[cfg(any(test, feature = "perf-instrumentation"))]
            on_disk_import_stats: OnDiskImportStats::default(),
            mode: RegistryBuildMode::Enabled {
                coordinator,
                registry: DiskLocationRegistry::new(),
                structural_source,
                deferred_stamps: Vec::new(),
            },
        }
    }
}

impl<K: KeyEncoding, V: DictionaryValue> OverlaySerializationBuild<K, V, DagAware> {
    pub(crate) fn dag_disabled() -> Self {
        Self {
            graph: DagAware::new(),
            on_disk_imports: HashMap::new(),
            #[cfg(any(test, feature = "perf-instrumentation"))]
            on_disk_import_stats: OnDiskImportStats::default(),
            mode: RegistryBuildMode::Disabled,
        }
    }

    /// Construct a registry-only build for structural/density analysis.
    ///
    /// This mode never reuses prior durable stamps and never produces new live
    /// stamps, because it has no durable checkpoint publication transaction.
    pub(crate) fn analysis(registry: DiskLocationRegistry) -> Self {
        Self {
            graph: DagAware::new(),
            on_disk_imports: HashMap::new(),
            #[cfg(any(test, feature = "perf-instrumentation"))]
            on_disk_import_stats: OnDiskImportStats::default(),
            mode: RegistryBuildMode::Analysis { registry },
        }
    }
}

impl<K: KeyEncoding, V: DictionaryValue, P: GraphPolicy<K, V>> OverlaySerializationBuild<K, V, P> {
    #[inline]
    fn is_enabled(&self) -> bool {
        matches!(&self.mode, RegistryBuildMode::Enabled { .. })
    }

    fn registry_mut(&mut self) -> Option<&mut DiskLocationRegistry> {
        match &mut self.mode {
            RegistryBuildMode::Disabled => None,
            RegistryBuildMode::Analysis { registry } => Some(registry),
            RegistryBuildMode::Enabled { registry, .. } => Some(registry),
        }
    }

    #[inline]
    fn has_registry(&self) -> bool {
        !matches!(&self.mode, RegistryBuildMode::Disabled)
    }

    fn on_disk_import(&self, address: DiskRecordAddress) -> Option<CompletedOnDiskImport> {
        self.on_disk_imports.get(&address).cloned()
    }

    /// Reserve memo capacity before any registry topology is mutated. The
    /// eventual vacant-entry insertion is therefore allocation-free.
    fn try_reserve_on_disk_import(&mut self, address: DiskRecordAddress) -> Result<()> {
        if self.on_disk_imports.contains_key(&address) {
            return Err(
                crate::persistent_artrie::error::PersistentARTrieError::internal(
                    "durable serializer memo entry was reserved twice",
                ),
            );
        }
        let requested_entries = self.on_disk_imports.len().checked_add(1).ok_or_else(|| {
            crate::persistent_artrie::error::PersistentARTrieError::internal(
                "durable serializer memo size overflow",
            )
        })?;
        self.on_disk_imports.try_reserve(1).map_err(|source| {
            crate::persistent_artrie::error::PersistentARTrieError::allocation_failed(
                "durable serializer memo",
                requested_entries,
                source,
            )
        })?;
        Ok(())
    }

    /// Publish only a fully finished registry span into the build-local memo.
    fn memoize_on_disk_import(
        &mut self,
        address: DiskRecordAddress,
        completed: CompletedOnDiskImport,
    ) -> Result<()> {
        match self.on_disk_imports.entry(address) {
            Entry::Vacant(entry) => {
                entry.insert(completed);
                Ok(())
            }
            Entry::Occupied(_) => Err(
                crate::persistent_artrie::error::PersistentARTrieError::internal(
                    "durable serializer memo completed one address twice",
                ),
            ),
        }
    }

    #[cfg(any(test, feature = "perf-instrumentation"))]
    fn record_on_disk_lookup(&mut self) {
        let (next, overflowed) = self.on_disk_import_stats.lookups.overflowing_add(1);
        self.on_disk_import_stats.lookups = if overflowed { usize::MAX } else { next };
        self.on_disk_import_stats.overflowed |= overflowed;
    }

    #[cfg(any(test, feature = "perf-instrumentation"))]
    fn record_source_import(&mut self) {
        let (next, overflowed) = self.on_disk_import_stats.source_imports.overflowing_add(1);
        self.on_disk_import_stats.source_imports = if overflowed { usize::MAX } else { next };
        self.on_disk_import_stats.overflowed |= overflowed;
    }

    #[cfg(any(test, feature = "perf-instrumentation"))]
    fn record_local_graft(&mut self, graft: LocalRegistryGraftStats) {
        let add = |current: usize, increment: usize| {
            let (next, overflowed) = current.overflowing_add(increment);
            (if overflowed { usize::MAX } else { next }, overflowed)
        };
        let (local_grafts, graft_count_overflow) = add(self.on_disk_import_stats.local_grafts, 1);
        let (topology_entries, topology_overflow) = add(
            self.on_disk_import_stats.local_graft_topology_entries,
            graft.appended_topology_entries,
        );
        let (durable_records, record_overflow) = add(
            self.on_disk_import_stats.local_graft_durable_records,
            graft.durable_records,
        );
        let (serialized_bytes, byte_overflow) = add(
            self.on_disk_import_stats.local_graft_serialized_bytes,
            graft.serialized_bytes,
        );
        self.on_disk_import_stats.local_grafts = local_grafts;
        self.on_disk_import_stats.local_graft_topology_entries = topology_entries;
        self.on_disk_import_stats.local_graft_durable_records = durable_records;
        self.on_disk_import_stats.local_graft_serialized_bytes = serialized_bytes;
        self.on_disk_import_stats.overflowed |= graft.overflowed
            || graft_count_overflow
            || topology_overflow
            || record_overflow
            || byte_overflow;
    }

    #[cfg(feature = "perf-instrumentation")]
    fn publish_perf_observation(&self) {
        let as_u64 = |value: usize| u64::try_from(value).unwrap_or(u64::MAX);
        crate::causal_perf::record_persistent_serialization(
            crate::causal_perf::PersistentSerializationStats {
                durable_alias_lookups: as_u64(self.on_disk_import_stats.lookups),
                durable_source_imports: as_u64(self.on_disk_import_stats.source_imports),
                local_registry_grafts: as_u64(self.on_disk_import_stats.local_grafts),
                local_graft_topology_entries: as_u64(
                    self.on_disk_import_stats.local_graft_topology_entries,
                ),
                local_graft_durable_records: as_u64(
                    self.on_disk_import_stats.local_graft_durable_records,
                ),
                local_graft_serialized_bytes: as_u64(
                    self.on_disk_import_stats.local_graft_serialized_bytes,
                ),
                observation_overflows: u64::from(self.on_disk_import_stats.overflowed),
            },
        );
    }

    fn registry_and_structural_source(
        &mut self,
    ) -> Option<(&mut DiskLocationRegistry, Option<&RegistryStructuralSource>)> {
        match &mut self.mode {
            RegistryBuildMode::Disabled => None,
            RegistryBuildMode::Analysis { registry } => Some((registry, None)),
            RegistryBuildMode::Enabled {
                registry,
                structural_source,
                ..
            } => Some((registry, structural_source.as_ref())),
        }
    }

    fn defer_stamp(&mut self, node: &Arc<OverlayNode<K, V>>, raw: u64) -> Result<()> {
        let RegistryBuildMode::Enabled {
            deferred_stamps, ..
        } = &mut self.mode
        else {
            return Ok(());
        };
        let requested_entries = deferred_stamps.len().checked_add(1).ok_or_else(|| {
            crate::persistent_artrie::error::PersistentARTrieError::internal(
                "overlay deferred durable-stamp count overflow",
            )
        })?;
        deferred_stamps.try_reserve(1).map_err(|source| {
            crate::persistent_artrie::error::PersistentARTrieError::allocation_failed(
                "overlay deferred durable-stamp plan",
                requested_entries,
                source,
            )
        })?;
        deferred_stamps.push(DeferredDurableStamp::new(Arc::clone(node), raw));
        Ok(())
    }

    #[inline(always)]
    fn prepare_graph(&mut self, root: &Arc<OverlayNode<K, V>>) -> Result<()> {
        self.graph.prepare_graph(root)
    }

    #[inline(always)]
    fn is_compression_boundary(&self, node: &Arc<OverlayNode<K, V>>) -> Result<bool> {
        self.graph.is_compression_boundary(node)
    }

    #[inline(always)]
    fn node_build_state(&self, node: &Arc<OverlayNode<K, V>>) -> Result<NodeBuildState> {
        self.graph.node_build_state(node)
    }

    #[inline(always)]
    fn mark_active(&mut self, node: &Arc<OverlayNode<K, V>>) -> Result<()> {
        self.graph.mark_active(node)
    }

    #[inline(always)]
    fn mark_complete(
        &mut self,
        node: &Arc<OverlayNode<K, V>>,
        expected: ExpectedBuildState,
        disk_ptr: &SwizzledPtr,
        registry_span: Option<RegistryBuilderSubtree>,
    ) -> Result<()> {
        self.graph
            .mark_complete(node, expected, disk_ptr, registry_span)
    }
}

impl<K: KeyEncoding, V: DictionaryValue> GraphPolicy<K, V> for ArborescentProduction {
    #[inline(always)]
    fn prepare_graph(&mut self, _root: &Arc<OverlayNode<K, V>>) -> Result<()> {
        Ok(())
    }

    #[inline(always)]
    fn is_compression_boundary(&self, _node: &Arc<OverlayNode<K, V>>) -> Result<bool> {
        Ok(false)
    }

    #[inline(always)]
    fn node_build_state(&self, _node: &Arc<OverlayNode<K, V>>) -> Result<NodeBuildState> {
        Ok(NodeBuildState::Unseen)
    }

    #[inline(always)]
    fn mark_active(&mut self, _node: &Arc<OverlayNode<K, V>>) -> Result<()> {
        Ok(())
    }

    #[inline(always)]
    fn mark_complete(
        &mut self,
        _node: &Arc<OverlayNode<K, V>>,
        _expected: ExpectedBuildState,
        _disk_ptr: &SwizzledPtr,
        registry_span: Option<RegistryBuilderSubtree>,
    ) -> Result<()> {
        debug_assert!(
            registry_span.is_none(),
            "an arborescent node cannot produce a shared registry span"
        );
        Ok(())
    }
}

impl DagAware {
    #[inline]
    fn node_id<K: KeyEncoding, V: DictionaryValue>(node: &Arc<OverlayNode<K, V>>) -> usize {
        Arc::as_ptr(node) as usize
    }
}

impl<K: KeyEncoding, V: DictionaryValue> GraphPolicy<K, V> for DagAware {
    /// Count every in-memory graph edge and reject cycles without native-stack
    /// growth. Each distinct node's outgoing edges are visited once; later DAG
    /// occurrences only increment the target's incoming-edge count.
    fn prepare_graph(&mut self, root: &Arc<OverlayNode<K, V>>) -> Result<()> {
        if self.graph_prepared || !self.nodes.is_empty() {
            return Err(
                crate::persistent_artrie::error::PersistentARTrieError::internal(
                    "compressed serializer graph census was initialized more than once",
                ),
            );
        }
        let mut census = HashMap::<usize, CensusEntry>::new();
        census.try_reserve(1).map_err(|source| {
            crate::persistent_artrie::error::PersistentARTrieError::allocation_failed(
                "compressed serializer DAG census",
                1,
                source,
            )
        })?;
        census.insert(
            Self::node_id(root),
            CensusEntry {
                incoming_edges: 0,
                census: CensusState::Active,
            },
        );

        let mut stack: Vec<CensusFrame<K, V>> = Vec::new();
        stack.try_reserve(1).map_err(|source| {
            crate::persistent_artrie::error::PersistentARTrieError::allocation_failed(
                "compressed serializer DAG census stack",
                1,
                source,
            )
        })?;
        stack.push(CensusFrame {
            node: Arc::clone(root),
            next_child: 0,
        });

        while !stack.is_empty() {
            let step = {
                let frame = stack.last_mut().ok_or_else(|| {
                    crate::persistent_artrie::error::PersistentARTrieError::internal(
                        "compressed serializer DAG census lost its active frame",
                    )
                })?;
                if frame.next_child < frame.node.num_children() {
                    let child_index = frame.next_child;
                    frame.next_child = frame.next_child.checked_add(1).ok_or_else(|| {
                        crate::persistent_artrie::error::PersistentARTrieError::internal(
                            "compressed serializer DAG census child cursor overflow",
                        )
                    })?;
                    let (_, child) = frame.node.child_at(child_index).ok_or_else(|| {
                        crate::persistent_artrie::error::PersistentARTrieError::internal(
                            "compressed serializer DAG census child index is inconsistent",
                        )
                    })?;
                    let child = child.as_in_mem().map(Arc::clone);
                    CensusStep::Child(child)
                } else {
                    CensusStep::Complete
                }
            };

            match step {
                CensusStep::Child(None) => {}
                CensusStep::Child(Some(child)) => {
                    let child_id = Self::node_id(&child);
                    if let Some(entry) = census.get_mut(&child_id) {
                        entry.incoming_edges =
                            entry.incoming_edges.checked_add(1).ok_or_else(|| {
                                crate::persistent_artrie::error::PersistentARTrieError::internal(
                                    "compressed serializer DAG incoming-edge count overflow",
                                )
                            })?;
                        if entry.census == CensusState::Active {
                            return Err(
                                crate::persistent_artrie::error::PersistentARTrieError::internal(
                                    "compressed serializer reached an in-memory overlay cycle",
                                ),
                            );
                        }
                    } else {
                        let requested_nodes = census.len().checked_add(1).ok_or_else(|| {
                            crate::persistent_artrie::error::PersistentARTrieError::internal(
                                "compressed serializer DAG node count overflow",
                            )
                        })?;
                        census.try_reserve(1).map_err(|source| {
                            crate::persistent_artrie::error::PersistentARTrieError::allocation_failed(
                                "compressed serializer DAG census",
                                requested_nodes,
                                source,
                            )
                        })?;
                        census.insert(
                            child_id,
                            CensusEntry {
                                incoming_edges: 1,
                                census: CensusState::Active,
                            },
                        );
                        let requested_frames = stack.len().checked_add(1).ok_or_else(|| {
                            crate::persistent_artrie::error::PersistentARTrieError::internal(
                                "compressed serializer DAG census depth overflow",
                            )
                        })?;
                        stack.try_reserve(1).map_err(|source| {
                            crate::persistent_artrie::error::PersistentARTrieError::allocation_failed(
                                "compressed serializer DAG census stack",
                                requested_frames,
                                source,
                            )
                        })?;
                        stack.push(CensusFrame {
                            node: child,
                            next_child: 0,
                        });
                    }
                }
                CensusStep::Complete => {
                    let completed = stack.pop().ok_or_else(|| {
                        crate::persistent_artrie::error::PersistentARTrieError::internal(
                            "compressed serializer DAG census completion underflow",
                        )
                    })?;
                    let entry =
                        census
                            .get_mut(&Self::node_id(&completed.node))
                            .ok_or_else(|| {
                                crate::persistent_artrie::error::PersistentARTrieError::internal(
                                    "compressed serializer DAG census node disappeared",
                                )
                            })?;
                    if entry.census != CensusState::Active {
                        return Err(
                            crate::persistent_artrie::error::PersistentARTrieError::internal(
                                "compressed serializer DAG census completed a non-active node",
                            ),
                        );
                    }
                    entry.census = CensusState::Complete;
                }
            }
        }

        let shared_nodes = census
            .values()
            .filter(|entry| entry.incoming_edges > 1)
            .count();
        self.nodes.try_reserve(shared_nodes).map_err(|source| {
            crate::persistent_artrie::error::PersistentARTrieError::allocation_failed(
                "compressed serializer DAG completion memo",
                shared_nodes,
                source,
            )
        })?;
        for (node_id, entry) in census {
            if entry.incoming_edges > 1 {
                self.nodes.insert(node_id, NodeBuildState::Unseen);
            }
        }
        self.graph_prepared = true;
        Ok(())
    }

    fn is_compression_boundary(&self, node: &Arc<OverlayNode<K, V>>) -> Result<bool> {
        Ok(self.nodes.contains_key(&Self::node_id(node)))
    }

    fn node_build_state(&self, node: &Arc<OverlayNode<K, V>>) -> Result<NodeBuildState> {
        Ok(self
            .nodes
            .get(&Self::node_id(node))
            .cloned()
            .unwrap_or(NodeBuildState::Unseen))
    }

    fn mark_active(&mut self, node: &Arc<OverlayNode<K, V>>) -> Result<()> {
        let Some(state) = self.nodes.get_mut(&Self::node_id(node)) else {
            return Ok(());
        };
        match state {
            NodeBuildState::Unseen => {
                *state = NodeBuildState::Active;
                Ok(())
            }
            NodeBuildState::Active => Err(
                crate::persistent_artrie::error::PersistentARTrieError::internal(
                    "compressed serializer activated an already-active DAG node",
                ),
            ),
            NodeBuildState::Complete(_) => Err(
                crate::persistent_artrie::error::PersistentARTrieError::internal(
                    "compressed serializer activated an already-completed DAG node",
                ),
            ),
        }
    }

    fn mark_complete(
        &mut self,
        node: &Arc<OverlayNode<K, V>>,
        expected: ExpectedBuildState,
        disk_ptr: &SwizzledPtr,
        registry_span: Option<RegistryBuilderSubtree>,
    ) -> Result<()> {
        let Some(state) = self.nodes.get_mut(&Self::node_id(node)) else {
            if registry_span.is_some() {
                return Err(
                    crate::persistent_artrie::error::PersistentARTrieError::internal(
                        "non-shared serializer node produced a shared registry span",
                    ),
                );
            }
            return Ok(());
        };
        let state_matches = matches!(
            (&*state, expected),
            (NodeBuildState::Unseen, ExpectedBuildState::Unseen)
                | (NodeBuildState::Active, ExpectedBuildState::Active)
        );
        if !state_matches {
            return Err(
                crate::persistent_artrie::error::PersistentARTrieError::internal(
                    "compressed serializer completed a DAG node from an invalid build state",
                ),
            );
        }
        *state = NodeBuildState::Complete(CompletedSubtree {
            disk_ptr: disk_ptr.clone(),
            registry_span,
        });
        Ok(())
    }
}

impl<K: KeyEncoding, V: DictionaryValue, P: GraphPolicy<K, V>> OverlaySerializationBuild<K, V, P> {
    pub(crate) fn finish(
        self,
        captured_root: &RootRevision<K, V>,
    ) -> Result<Option<PreparedRegistryPublication<K, V>>>
    where
        K: crate::persistent_artrie::core::eviction::RegistryFamily,
    {
        #[cfg(feature = "perf-instrumentation")]
        self.publish_perf_observation();
        match self.mode {
            RegistryBuildMode::Disabled => Ok(None),
            RegistryBuildMode::Analysis { .. } => Err(
                crate::persistent_artrie::error::PersistentARTrieError::internal(
                    "a registry-analysis build cannot become a checkpoint publication",
                ),
            ),
            RegistryBuildMode::Enabled {
                coordinator,
                registry,
                deferred_stamps,
                ..
            } => PreparedRegistryPublication::try_new(
                coordinator,
                captured_root,
                registry,
                deferred_stamps,
            )
            .map(Some)
            .map_err(|error| {
                crate::persistent_artrie::error::PersistentARTrieError::internal(format!(
                    "finalize eviction-registry publication: {error}"
                ))
            }),
        }
    }

    pub(crate) fn into_analysis_registry(self) -> Result<DiskLocationRegistry> {
        #[cfg(feature = "perf-instrumentation")]
        self.publish_perf_observation();
        match self.mode {
            RegistryBuildMode::Analysis { mut registry } => {
                registry.try_finalize_for_publication().map_err(|error| {
                    crate::persistent_artrie::error::PersistentARTrieError::internal(format!(
                        "finalize analysis registry: {error}"
                    ))
                })?;
                Ok(registry)
            }
            RegistryBuildMode::Disabled | RegistryBuildMode::Enabled { .. } => Err(
                crate::persistent_artrie::error::PersistentARTrieError::internal(
                    "only a registry-analysis build yields an analysis registry",
                ),
            ),
        }
    }
}

/// Run a registry-only serialization as an atomic replacement transaction.
///
/// Analysis callers historically moved the caller's registry directly into a
/// build. A returned serialization error therefore exposed a partially built,
/// unfinalized registry. This boundary always builds from a fresh registry and
/// publishes it only after both the operation and mode extraction succeed. On
/// every returned error, the caller observes its exact prior registry.
pub(crate) fn try_analysis_registry_transaction<K, V, T, F>(
    target: &mut DiskLocationRegistry,
    operation: F,
) -> Result<T>
where
    K: KeyEncoding,
    V: DictionaryValue,
    F: FnOnce(&mut OverlaySerializationBuild<K, V, DagAware>) -> Result<T>,
{
    let mut build = OverlaySerializationBuild::analysis(DiskLocationRegistry::new());
    let value = operation(&mut build)?;
    let replacement = build.into_analysis_registry()?;
    *target = replacement;
    Ok(value)
}

fn chain_chunk_width<K: KeyEncoding>() -> Result<usize> {
    K::MAX_PREFIX_LEN.checked_add(1).ok_or_else(|| {
        crate::persistent_artrie::error::PersistentARTrieError::internal(
            "compressed serializer chain-chunk width overflow",
        )
    })
}

fn chain_chunk_count(units_len: usize, width: usize) -> Result<usize> {
    if width == 0 {
        return Err(
            crate::persistent_artrie::error::PersistentARTrieError::internal(
                "compressed serializer chain-chunk width is zero",
            ),
        );
    }
    Ok(units_len.div_ceil(width))
}

fn chain_chunk_bounds(
    units_len: usize,
    width: usize,
    chunk_index: usize,
) -> Result<(usize, usize)> {
    let start = chunk_index.checked_mul(width).ok_or_else(|| {
        crate::persistent_artrie::error::PersistentARTrieError::internal(
            "compressed serializer chain-chunk offset overflow",
        )
    })?;
    if start >= units_len {
        return Err(
            crate::persistent_artrie::error::PersistentARTrieError::internal(
                "compressed serializer chain-chunk index is out of range",
            ),
        );
    }
    let chunk_len = units_len
        .checked_sub(start)
        .ok_or_else(|| {
            crate::persistent_artrie::error::PersistentARTrieError::internal(
                "compressed serializer chain-chunk length underflow",
            )
        })?
        .min(width);
    let end = start.checked_add(chunk_len).ok_or_else(|| {
        crate::persistent_artrie::error::PersistentARTrieError::internal(
            "compressed serializer chain-chunk end overflow",
        )
    })?;
    Ok((start, end))
}

/// Peel a maximal **single-child non-final no-value** chain starting at `start`, returning
/// `(chain_units, live_spine, terminus)`. `chain_units` is the edge unit-string of the peeled links;
/// EMPTY iff `start` is itself the terminus. `live_spine[j]` is the live chain-link reached by
/// `chain_units[j-1]` (`live_spine[0]` = the chain head); `live_spine.len() == chain_units.len()` and
/// the terminus is NOT included (returned separately). The terminus is the first node that is NOT a
/// prefix-link — final, valued, `!= 1` child, OR whose sole child is `OnDisk` (the serializer NEVER
/// faults disk: an OnDisk sole child ends the chain, its `SwizzledPtr` passing through verbatim).
/// ITERATIVE (walks the uncompressed spine). The generic twin of the three identical originals
/// (char `peel_chain`, byte `peel_chain_byte`, vocab's reuse).
pub(crate) fn peel_chain_generic<K: KeyEncoding, V: DictionaryValue, P: GraphPolicy<K, V>>(
    start: Arc<OverlayNode<K, V>>,
    stop_before_durable_child: bool,
    build: &OverlaySerializationBuild<K, V, P>,
) -> Result<PrefixChain<K, V>> {
    let mut units: Vec<K::Unit> = Vec::new();
    let mut live: Vec<Arc<OverlayNode<K, V>>> = Vec::new();
    let mut cur = start;
    loop {
        if build.is_compression_boundary(&cur)? {
            return Ok((units, live, cur));
        }
        // A prefix-link: exactly one child, not final, no value.
        if cur.num_children() != 1 || cur.is_final() || cur.has_value() {
            return Ok((units, live, cur));
        }
        // Its sole child — continue ONLY while it is InMem (never fault disk during serialize).
        let sole = {
            let mut it = cur.iter_children();
            let (&edge, child) = it.next().ok_or_else(|| {
                crate::persistent_artrie::error::PersistentARTrieError::internal(
                    "compressed serializer observed an inconsistent one-child node",
                )
            })?;
            child.as_in_mem().map(|arc| (edge, Arc::clone(arc)))
        };
        match sole {
            Some((edge, child_arc)) => {
                if stop_before_durable_child && child_arc.durable_stamp() != 0 {
                    return Ok((units, live, cur));
                }
                let requested_units = units.len().checked_add(1).ok_or_else(|| {
                    crate::persistent_artrie::error::PersistentARTrieError::internal(
                        "compressed serializer peeled-chain length overflow",
                    )
                })?;
                units.try_reserve(1).map_err(|source| {
                    crate::persistent_artrie::error::PersistentARTrieError::allocation_failed(
                        "compressed serializer peeled-chain units",
                        requested_units,
                        source,
                    )
                })?;
                live.try_reserve(1).map_err(|source| {
                    crate::persistent_artrie::error::PersistentARTrieError::allocation_failed(
                        "compressed serializer peeled-chain nodes",
                        requested_units,
                        source,
                    )
                })?;
                live.push(Arc::clone(&cur));
                units.push(edge);
                cur = child_arc;
            }
            // Sole child is OnDisk => `cur` is the terminus (its OnDisk child passes through).
            None => return Ok((units, live, cur)),
        }
    }
}

enum PendingChildSource<K: KeyEncoding, V: DictionaryValue> {
    InMem(Arc<OverlayNode<K, V>>),
    OnDisk(SwizzledPtr),
    Processed,
}

/// One child in canonical edge order. In-memory and durable children share the
/// same stream so registry topology is emitted in exact preorder.
struct PendingChild<K: KeyEncoding, V: DictionaryValue> {
    key: K::Unit,
    ptr: Option<SwizzledPtr>,
    source: PendingChildSource<K, V>,
}

/// A work-stack frame: one peeled-chain terminus mid-descent (the root has an empty chain), held by
/// OWNED `Arc`, plus the peeled chain prefix collapsed into chunks ABOVE it.
struct Frame<K: KeyEncoding, V: DictionaryValue> {
    node: Arc<OverlayNode<K, V>>,
    parent_slot: Option<usize>,
    chain_prefix: Vec<K::Unit>,
    live_spine: Vec<Arc<OverlayNode<K, V>>>,
    base_depth: usize,
    pushed_units: usize,
    /// Compact eviction-topology entries for the emitted nodes represented by
    /// this frame, ordered top-to-bottom: compressed chunks followed by the
    /// terminus. Empty when eviction registration is disabled.
    registry_entries: SmallVec<[RegistryPathId; 1]>,
    registry_start: Option<RegistryBuilderSubtreeStart>,
    next_child: usize,
    slots: Vec<PendingChild<K, V>>,
}

struct FrameRegistryState {
    entries: SmallVec<[RegistryPathId; 1]>,
    subtree_start: Option<RegistryBuilderSubtreeStart>,
}

fn make_frame<K: KeyEncoding, V: DictionaryValue>(
    node: Arc<OverlayNode<K, V>>,
    parent_slot: Option<usize>,
    chain_prefix: Vec<K::Unit>,
    live_spine: Vec<Arc<OverlayNode<K, V>>>,
    base_depth: usize,
    pushed_units: usize,
    registry_state: FrameRegistryState,
) -> Result<Frame<K, V>> {
    let n = node.num_children();
    let mut slots: Vec<PendingChild<K, V>> = Vec::new();
    slots.try_reserve_exact(n).map_err(|source| {
        crate::persistent_artrie::error::PersistentARTrieError::allocation_failed(
            "compressed serializer child slots",
            n,
            source,
        )
    })?;
    for (&key, child) in node.iter_children() {
        if let Some(child_arc) = child.as_in_mem() {
            // Retain the resident Arc even when it has a durable stamp. Exact
            // carry metadata may be unavailable or stale; in that case this
            // subtree must be serialized from memory rather than scanned as if
            // all of its descendants were nonresident.
            slots.push(PendingChild {
                key,
                ptr: None,
                source: PendingChildSource::InMem(Arc::clone(child_arc)),
            });
        } else if let Some(on_disk) = child.as_on_disk() {
            if !on_disk.is_null() {
                slots.push(PendingChild {
                    key,
                    ptr: Some(on_disk.clone()),
                    source: PendingChildSource::OnDisk(on_disk.clone()),
                });
            }
        }
    }
    Ok(Frame {
        node,
        parent_slot,
        chain_prefix,
        live_spine,
        base_depth,
        pushed_units,
        registry_entries: registry_state.entries,
        registry_start: registry_state.subtree_start,
        next_child: 0,
        slots,
    })
}

/// The single generic path-compressed overlay serializer. Implementors supply the format-specific
/// seams; the shared post-order loop lives in the default [`Self::serialize_compressed_loop`].
pub(crate) trait OverlayCompressedSerialize<K: KeyEncoding, V: DictionaryValue> {
    /// The variant's projected single-node value carrier handed to [`Self::serialize_projected_node`].
    /// char/vocab: `CharTrieNodeInner<V>`; byte: a `{node, value}` struct.
    type Projected;

    /// Project `node` into a single-node carrier (finality + value + the already-resolved on-disk
    /// child ptrs), NO prefix. char: `overlay_inner_single_node`; byte: build `Node` + value blob.
    fn project_node(
        node: &OverlayNode<K, V>,
        child_disk_ptrs: &[(K::Unit, SwizzledPtr)],
    ) -> Result<Self::Projected>;

    /// As [`Self::project_node`] but stamps a path-compression `prefix` (a synthetic non-final
    /// no-value chunk carrier). `prefix.len() <= K::MAX_PREFIX_LEN`.
    fn project_chunk(
        synth: &OverlayNode<K, V>,
        child_disk_ptrs: &[(K::Unit, SwizzledPtr)],
        prefix: &[K::Unit],
    ) -> Result<Self::Projected>;

    /// Serialize ONE projected node to a fresh arena slot, returning its disk ptr. Registers at
    /// `path` (full expanded depth) IFF `registry.is_some()`. Eviction-OFF variants (vocab) ignore
    /// `path`/`registry`.
    fn serialize_projected_node(
        &self,
        projected: &Self::Projected,
        child_disk_ptrs: &[(K::Unit, SwizzledPtr)],
        path: &[K::Unit],
        registry_path: RegistryPathId,
        registry: Option<&mut DiskLocationRegistry>,
    ) -> Result<SwizzledPtr>;

    /// Reserve one emitted node's segment in the checkpoint-local compact
    /// eviction topology. `segment` contains only the units since the emitted
    /// parent node, never an absolute root path.
    fn reserve_registry_path(
        _registry: &mut DiskLocationRegistry,
        _parent: RegistryPathId,
        _segment: &[K::Unit],
    ) -> Result<RegistryPathId> {
        Err(
            crate::persistent_artrie::error::PersistentARTrieError::internal(
                "reserve_registry_path has no implementation for this key encoding",
            ),
        )
    }

    /// Begin one exact LIFO builder subtree at its newest topology entry.
    fn begin_registry_subtree(
        _registry: &mut DiskLocationRegistry,
        _root: RegistryPathId,
    ) -> Result<RegistryBuilderSubtreeStart> {
        Err(
            crate::persistent_artrie::error::PersistentARTrieError::internal(
                "begin_registry_subtree has no implementation for this key encoding",
            ),
        )
    }

    /// Preflight the only immediately-known fallible growth needed by
    /// [`Self::begin_registry_subtree`]. Durable imports call this before
    /// reserving their destination root.
    fn prepare_registry_subtree_start(_registry: &mut DiskLocationRegistry) -> Result<()> {
        Err(
            crate::persistent_artrie::error::PersistentARTrieError::internal(
                "prepare_registry_subtree_start has no implementation for this key encoding",
            ),
        )
    }

    /// Cancel a just-begun empty subtree after durable carry lookup falls back.
    fn cancel_registry_subtree(
        _registry: &mut DiskLocationRegistry,
        _start: RegistryBuilderSubtreeStart,
    ) -> Result<()> {
        Err(
            crate::persistent_artrie::error::PersistentARTrieError::internal(
                "cancel_registry_subtree has no implementation for this key encoding",
            ),
        )
    }

    /// Finish the current LIFO subtree and mint its registry-bound exact handle.
    fn finish_registry_subtree(
        _registry: &mut DiskLocationRegistry,
        _start: RegistryBuilderSubtreeStart,
    ) -> Result<RegistryBuilderSubtree> {
        Err(
            crate::persistent_artrie::error::PersistentARTrieError::internal(
                "finish_registry_subtree has no implementation for this key encoding",
            ),
        )
    }

    /// Copy an exact completed local subtree at a second occurrence, preserving
    /// the explicitly requested resident or nonresident root state.
    fn graft_registry_subtree(
        _registry: &mut DiskLocationRegistry,
        _source: &RegistryBuilderSubtree,
        _destination: RegistryPathId,
        _expected_root: &SwizzledPtr,
        _expected_root_resident: bool,
    ) -> Result<LocalRegistryGraftStats> {
        Err(
            crate::persistent_artrie::error::PersistentARTrieError::internal(
                "graft_registry_subtree has no implementation for this key encoding",
            ),
        )
    }

    /// A fresh synthetic non-final no-value node for chunk carriers (`OverlayNode::<K, V>::new()`).
    fn new_synth_node() -> OverlayNode<K, V>;

    /// Register an ALREADY-serialized, durable-CLEAN node that dirty-skip is REUSING this
    /// checkpoint: record its existing `ptr` + on-disk size in the (freshly rebuilt) eviction
    /// `registry` WITHOUT re-appending a fresh arena slot.
    ///
    /// Called ONLY when eviction is ON (`registry.is_some()`) and the live node's
    /// `durable_stamp()` is nonzero — i.e. the node's bytes already sit at `ptr` from a prior
    /// checkpoint and (by the M-2a stamp invariant) its whole subtree is byte-identical. Retaining
    /// FULL registration here (size = the EXACT on-disk slot length, so it equals what a fresh
    /// serialize would have recorded) keeps the resident-budget census faithful while the
    /// growth-causing arena `allocate` is elided.
    ///
    /// Default: unreachable — a variant that durable-stamps its nodes AND threads an eviction
    /// registry (char, byte) MUST override this. Eviction-OFF variants (u64, vocab) never stamp
    /// and always pass `None`, so the dirty-skip branch never reaches this for them.
    fn try_reuse_durable_subtree(
        &self,
        _ptr: &SwizzledPtr,
        _path: &[K::Unit],
        _registry_path: RegistryPathId,
        _registry: &mut DiskLocationRegistry,
        _structural_source: Option<&RegistryStructuralSource>,
        _root_resident: bool,
    ) -> Result<bool> {
        Err(
            crate::persistent_artrie::error::PersistentARTrieError::internal(
                "try_reuse_durable_subtree has no default impl; a variant that durable-stamps \
                 its nodes and threads an eviction registry must override it",
            ),
        )
    }

    /// Shared post-order serializer over one cohesive build context.
    fn serialize_compressed_loop<P: GraphPolicy<K, V>>(
        &self,
        root: &Arc<OverlayNode<K, V>>,
        build: &mut OverlaySerializationBuild<K, V, P>,
    ) -> Result<SwizzledPtr> {
        // The full key path of the CURRENT node (edge + chain pushed before descending).
        let mut path: Vec<K::Unit> = Vec::new();
        let mut root_registry_entries = SmallVec::<[RegistryPathId; 1]>::new();
        if let Some(registry) = build.registry_mut() {
            root_registry_entries
                .try_reserve_exact(1)
                .map_err(|source| {
                    crate::persistent_artrie::error::PersistentARTrieError::allocation_failed(
                        "compressed serializer root registry entry",
                        1,
                        source,
                    )
                })?;
            root_registry_entries.push(Self::reserve_registry_path(
                registry,
                RegistryPathId::ROOT,
                &[],
            )?);
        }
        let reuse_durable = build.is_enabled();
        if reuse_durable {
            let stamp = root.durable_stamp();
            if stamp != 0 {
                let pointer = SwizzledPtr::from_raw(stamp);
                let registry_path = root_registry_entries
                    .last()
                    .copied()
                    .unwrap_or(RegistryPathId::ROOT);
                if let Some((registry, structural_source)) = build.registry_and_structural_source()
                {
                    if self.try_reuse_durable_subtree(
                        &pointer,
                        &path,
                        registry_path,
                        registry,
                        structural_source,
                        true,
                    )? {
                        return Ok(pointer);
                    }
                }
            }
        }
        build.prepare_graph(root)?;
        build.mark_active(root)?;
        // The root is never peeled (it is always the on-disk entry node); its children are.
        let root_frame = make_frame(
            Arc::clone(root),
            None,
            Vec::new(),
            Vec::new(),
            0,
            0,
            FrameRegistryState {
                entries: root_registry_entries,
                subtree_start: None,
            },
        )?;
        let mut stack: Vec<Frame<K, V>> = Vec::new();
        stack.try_reserve(1).map_err(|source| {
            crate::persistent_artrie::error::PersistentARTrieError::allocation_failed(
                "compressed serializer frame stack",
                1,
                source,
            )
        })?;
        stack.push(root_frame);
        let mut completed: Option<(usize, SwizzledPtr)> = None;

        loop {
            let next_child = {
                let frame = stack.last_mut().ok_or_else(|| {
                    crate::persistent_artrie::error::PersistentARTrieError::internal(
                        "compressed serializer work stack became empty before completion",
                    )
                })?;

                if let Some((slot_index, ptr)) = completed.take() {
                    let slot = frame.slots.get_mut(slot_index).ok_or_else(|| {
                        crate::persistent_artrie::error::PersistentARTrieError::internal(
                            "compressed serializer completed child slot is out of range",
                        )
                    })?;
                    if slot.ptr.is_some() {
                        return Err(
                            crate::persistent_artrie::error::PersistentARTrieError::internal(
                                "compressed serializer completed child slot is already filled",
                            ),
                        );
                    }
                    slot.ptr = Some(ptr);
                }

                if frame.next_child < frame.slots.len() {
                    let index = frame.next_child;
                    frame.next_child = frame.next_child.checked_add(1).ok_or_else(|| {
                        crate::persistent_artrie::error::PersistentARTrieError::internal(
                            "compressed serializer child cursor overflow",
                        )
                    })?;
                    let slot = &mut frame.slots[index];
                    let source = std::mem::replace(&mut slot.source, PendingChildSource::Processed);
                    let parent_registry_path = frame
                        .registry_entries
                        .last()
                        .copied()
                        .unwrap_or(RegistryPathId::ROOT);
                    Some((index, slot.key, source, parent_registry_path))
                } else {
                    None
                }
            };

            if let Some((slot_index, edge, source, parent_registry_path)) = next_child {
                let mut pre_reserved_top = None;
                let child_arc = match source {
                    PendingChildSource::OnDisk(pointer) => {
                        let root_ref = DurableRecordRef::from_typed_pointer(&pointer)?;
                        let claimed_type = root_ref.expected_type.ok_or_else(|| {
                            crate::persistent_artrie::error::PersistentARTrieError::internal(
                                "typed durable child lost its node-type claim",
                            )
                        })?;
                        let canonical_candidate =
                            root_ref.address.canonical_pointer(claimed_type)?;
                        #[cfg(any(test, feature = "perf-instrumentation"))]
                        build.record_on_disk_lookup();
                        let resolved_pointer = if build.has_registry() {
                            if let Some(completed_import) = build.on_disk_import(root_ref.address) {
                                let completed_location = completed_import
                                    .canonical_ptr
                                    .disk_location()
                                    .ok_or_else(|| {
                                        crate::persistent_artrie::error::PersistentARTrieError::internal(
                                            "durable serializer memo contains a non-durable pointer",
                                        )
                                    })?;
                                if completed_location.node_type != claimed_type {
                                    return Err(
                                        crate::persistent_artrie::error::PersistentARTrieError::corrupted(
                                            "two durable child occurrences claim different node types for one arena record",
                                        ),
                                    );
                                }
                                if completed_location.block_id != root_ref.address.block_id
                                    || completed_location.offset != root_ref.address.slot_id
                                {
                                    return Err(
                                        crate::persistent_artrie::error::PersistentARTrieError::internal(
                                            "durable serializer memo address does not match its key",
                                        ),
                                    );
                                }
                                let _graft_observation = {
                                    let registry = build.registry_mut().ok_or_else(|| {
                                        crate::persistent_artrie::error::PersistentARTrieError::internal(
                                            "durable serializer memo exists without a registry",
                                        )
                                    })?;
                                    let destination = Self::reserve_registry_path(
                                        registry,
                                        parent_registry_path,
                                        &[edge],
                                    )?;
                                    Self::graft_registry_subtree(
                                        registry,
                                        &completed_import.registry_span,
                                        destination,
                                        &completed_import.canonical_ptr,
                                        false,
                                    )?
                                };
                                #[cfg(any(test, feature = "perf-instrumentation"))]
                                build.record_local_graft(_graft_observation);
                                completed_import.canonical_ptr
                            } else {
                                // Every known fallible capacity is prepared before
                                // the destination root or builder stack is mutated.
                                build.try_reserve_on_disk_import(root_ref.address)?;
                                let requested_path = path.len().checked_add(1).ok_or_else(|| {
                                    crate::persistent_artrie::error::PersistentARTrieError::internal(
                                        "compressed serializer durable-child path length overflow",
                                    )
                                })?;
                                path.try_reserve(1).map_err(|source| {
                                    crate::persistent_artrie::error::PersistentARTrieError::allocation_failed(
                                        "compressed serializer durable-child path",
                                        requested_path,
                                        source,
                                    )
                                })?;
                                path.push(edge);
                                let registry_span = {
                                    let (registry, structural_source) = build
                                        .registry_and_structural_source()
                                        .ok_or_else(|| {
                                            crate::persistent_artrie::error::PersistentARTrieError::internal(
                                                "durable serializer import lost its registry",
                                            )
                                        })?;
                                    Self::prepare_registry_subtree_start(registry)?;
                                    let registry_path = Self::reserve_registry_path(
                                        registry,
                                        parent_registry_path,
                                        &[edge],
                                    )?;
                                    let registry_start =
                                        Self::begin_registry_subtree(registry, registry_path)?;
                                    let import_result = self.try_reuse_durable_subtree(
                                        &canonical_candidate,
                                        &path,
                                        registry_path,
                                        registry,
                                        structural_source,
                                        false,
                                    );
                                    path.pop();
                                    let imported = import_result?;
                                    if !imported {
                                        Self::cancel_registry_subtree(registry, registry_start)?;
                                        return Err(
                                            crate::persistent_artrie::error::PersistentARTrieError::internal(
                                                "an on-disk subtree could not be imported into the registry",
                                            ),
                                        );
                                    }
                                    Self::finish_registry_subtree(registry, registry_start)?
                                };
                                build.memoize_on_disk_import(
                                    root_ref.address,
                                    CompletedOnDiskImport {
                                        canonical_ptr: canonical_candidate.clone(),
                                        registry_span,
                                    },
                                )?;
                                #[cfg(any(test, feature = "perf-instrumentation"))]
                                build.record_source_import();
                                canonical_candidate
                            }
                        } else {
                            canonical_candidate
                        };
                        let slot = stack
                            .last_mut()
                            .and_then(|frame| frame.slots.get_mut(slot_index))
                            .ok_or_else(|| {
                                crate::persistent_artrie::error::PersistentARTrieError::internal(
                                    "compressed serializer durable-child slot is unavailable",
                                )
                            })?;
                        slot.ptr = Some(resolved_pointer);
                        continue;
                    }
                    PendingChildSource::InMem(child_arc) => {
                        match build.node_build_state(&child_arc)? {
                            NodeBuildState::Complete(completed_subtree) => {
                                if let Some(registry) = build.registry_mut() {
                                    let registry_span = completed_subtree
                                        .registry_span
                                        .as_ref()
                                        .ok_or_else(|| {
                                            crate::persistent_artrie::error::PersistentARTrieError::internal(
                                                "completed DAG node has no local registry span",
                                            )
                                        })?;
                                    let destination = Self::reserve_registry_path(
                                        registry,
                                        parent_registry_path,
                                        &[edge],
                                    )?;
                                    Self::graft_registry_subtree(
                                        registry,
                                        registry_span,
                                        destination,
                                        &completed_subtree.disk_ptr,
                                        true,
                                    )?;
                                }
                                let slot = stack
                                    .last_mut()
                                    .and_then(|frame| frame.slots.get_mut(slot_index))
                                    .ok_or_else(|| {
                                        crate::persistent_artrie::error::PersistentARTrieError::internal(
                                            "compressed serializer DAG-alias child slot is unavailable",
                                        )
                                    })?;
                                slot.ptr = Some(completed_subtree.disk_ptr);
                                continue;
                            }
                            NodeBuildState::Active => {
                                return Err(
                                    crate::persistent_artrie::error::PersistentARTrieError::internal(
                                        "compressed serializer reached an active in-memory DAG node",
                                    ),
                                );
                            }
                            NodeBuildState::Unseen => {}
                        }
                        let capture_registry_span = build.is_compression_boundary(&child_arc)?;
                        let stamp = if reuse_durable {
                            child_arc.durable_stamp()
                        } else {
                            0
                        };
                        if stamp != 0 {
                            let pointer = SwizzledPtr::from_raw(stamp);
                            let reused_completion = if let Some((registry, structural_source)) =
                                build.registry_and_structural_source()
                            {
                                let requested_path = path.len().checked_add(1).ok_or_else(|| {
                                    crate::persistent_artrie::error::PersistentARTrieError::internal(
                                        "compressed serializer resident-child path length overflow",
                                    )
                                })?;
                                path.try_reserve(1).map_err(|source| {
                                    crate::persistent_artrie::error::PersistentARTrieError::allocation_failed(
                                        "compressed serializer resident-child path",
                                        requested_path,
                                        source,
                                    )
                                })?;
                                path.push(edge);
                                let registry_path = Self::reserve_registry_path(
                                    registry,
                                    parent_registry_path,
                                    &[edge],
                                )?;
                                let registry_start = capture_registry_span
                                    .then(|| Self::begin_registry_subtree(registry, registry_path))
                                    .transpose()?;
                                let reused = self.try_reuse_durable_subtree(
                                    &pointer,
                                    &path,
                                    registry_path,
                                    registry,
                                    structural_source,
                                    true,
                                )?;
                                path.pop();
                                if reused {
                                    let registry_span = registry_start
                                        .map(|start| Self::finish_registry_subtree(registry, start))
                                        .transpose()?;
                                    Some(registry_span)
                                } else {
                                    if let Some(start) = registry_start {
                                        Self::cancel_registry_subtree(registry, start)?;
                                    }
                                    pre_reserved_top = Some(registry_path);
                                    None
                                }
                            } else {
                                None
                            };
                            if let Some(registry_span) = reused_completion {
                                build.mark_complete(
                                    &child_arc,
                                    ExpectedBuildState::Unseen,
                                    &pointer,
                                    registry_span,
                                )?;
                                let slot = stack
                                    .last_mut()
                                    .and_then(|frame| frame.slots.get_mut(slot_index))
                                    .ok_or_else(|| {
                                        crate::persistent_artrie::error::PersistentARTrieError::internal(
                                            "compressed serializer reused child slot is unavailable",
                                        )
                                    })?;
                                slot.ptr = Some(pointer);
                                continue;
                            }
                        }
                        child_arc
                    }
                    PendingChildSource::Processed => {
                        return Err(
                            crate::persistent_artrie::error::PersistentARTrieError::internal(
                                "compressed serializer processed one child source twice",
                            ),
                        )
                    }
                };
                let (chain_prefix, live_spine, terminus) =
                    peel_chain_generic::<K, V, P>(child_arc, reuse_durable, build)?;
                build.mark_active(&terminus)?;
                let base_depth = path.len();
                let pushed = chain_prefix.len().checked_add(1).ok_or_else(|| {
                    crate::persistent_artrie::error::PersistentARTrieError::internal(
                        "compressed serializer descended path length overflow",
                    )
                })?;
                let requested_path = path.len().checked_add(pushed).ok_or_else(|| {
                    crate::persistent_artrie::error::PersistentARTrieError::internal(
                        "compressed serializer descended path capacity overflow",
                    )
                })?;
                path.try_reserve(pushed).map_err(|source| {
                    crate::persistent_artrie::error::PersistentARTrieError::allocation_failed(
                        "compressed serializer descended path",
                        requested_path,
                        source,
                    )
                })?;
                path.push(edge);
                path.extend_from_slice(&chain_prefix);
                let mut registry_entries = SmallVec::<[RegistryPathId; 1]>::new();
                let mut registry_start = None;
                let capture_registry_span = build.is_compression_boundary(&terminus)?;
                if let Some(reg) = build.registry_mut() {
                    let chunk_width = chain_chunk_width::<K>()?;
                    let chunk_count = chain_chunk_count(chain_prefix.len(), chunk_width)?;
                    let requested_entries = chunk_count.checked_add(1).ok_or_else(|| {
                        crate::persistent_artrie::error::PersistentARTrieError::internal(
                            "compressed serializer registry-entry count overflow",
                        )
                    })?;
                    registry_entries.try_reserve_exact(requested_entries).map_err(|source| {
                        crate::persistent_artrie::error::PersistentARTrieError::allocation_failed(
                            "compressed serializer registry entries",
                            requested_entries,
                            source,
                        )
                    })?;
                    let mut parent = parent_registry_path;
                    let mut segment_start = base_depth;
                    let mut chunk_endpoint = base_depth.checked_add(1).ok_or_else(|| {
                        crate::persistent_artrie::error::PersistentARTrieError::internal(
                            "compressed serializer first chunk endpoint overflow",
                        )
                    })?;
                    for chunk_index in 0..chunk_count {
                        let entry = if chunk_index == 0 {
                            match pre_reserved_top.take() {
                                Some(entry) => entry,
                                None => Self::reserve_registry_path(
                                    reg,
                                    parent,
                                    &path[segment_start..chunk_endpoint],
                                )?,
                            }
                        } else {
                            Self::reserve_registry_path(
                                reg,
                                parent,
                                &path[segment_start..chunk_endpoint],
                            )?
                        };
                        registry_entries.push(entry);
                        parent = entry;
                        segment_start = chunk_endpoint;
                        let (_, chunk_end) =
                            chain_chunk_bounds(chain_prefix.len(), chunk_width, chunk_index)?;
                        chunk_endpoint = base_depth
                            .checked_add(1)
                            .and_then(|depth| depth.checked_add(chunk_end))
                            .ok_or_else(|| {
                                crate::persistent_artrie::error::PersistentARTrieError::internal(
                                    "compressed serializer chunk endpoint overflow",
                                )
                            })?;
                    }
                    let terminus = match pre_reserved_top.take() {
                        Some(entry) => entry,
                        None => Self::reserve_registry_path(reg, parent, &path[segment_start..])?,
                    };
                    registry_entries.push(terminus);
                    if capture_registry_span {
                        registry_start = Some(Self::begin_registry_subtree(reg, terminus)?);
                    }
                }
                let child_frame = make_frame(
                    terminus,
                    Some(slot_index),
                    chain_prefix,
                    live_spine,
                    base_depth,
                    pushed,
                    FrameRegistryState {
                        entries: registry_entries,
                        subtree_start: registry_start,
                    },
                )?;
                let requested_frames = stack.len().checked_add(1).ok_or_else(|| {
                    crate::persistent_artrie::error::PersistentARTrieError::internal(
                        "compressed serializer frame count overflow",
                    )
                })?;
                stack.try_reserve(1).map_err(|source| {
                    crate::persistent_artrie::error::PersistentARTrieError::allocation_failed(
                        "compressed serializer frame stack",
                        requested_frames,
                        source,
                    )
                })?;
                stack.push(child_frame);
                continue;
            }

            // All children resolved → serialize THIS terminus, then collapse its peeled chain.
            let frame = stack.pop().ok_or_else(|| {
                crate::persistent_artrie::error::PersistentARTrieError::internal(
                    "compressed serializer has no frame to finalize",
                )
            })?;
            let child_count = frame.slots.len();
            let mut child_disk_ptrs: Vec<(K::Unit, SwizzledPtr)> = Vec::new();
            child_disk_ptrs
                .try_reserve_exact(child_count)
                .map_err(|source| {
                    crate::persistent_artrie::error::PersistentARTrieError::allocation_failed(
                        "compressed serializer resolved child pointers",
                        child_count,
                        source,
                    )
                })?;
            for slot in frame.slots {
                if !matches!(slot.source, PendingChildSource::Processed) {
                    return Err(
                        crate::persistent_artrie::error::PersistentARTrieError::internal(
                            "compressed serializer finalized a child source before processing it",
                        ),
                    );
                }
                let ptr = slot.ptr.ok_or_else(|| {
                    crate::persistent_artrie::error::PersistentARTrieError::internal(
                        "compressed serializer finalized an unresolved child pointer",
                    )
                })?;
                child_disk_ptrs.push((slot.key, ptr));
            }

            // (1) The terminus node — NO prefix. Durable-clean subtree roots
            // were converted to opaque durable children before descent, so every
            // frame reaching finalization is genuinely new and is serialized once.
            let terminus_registry_path = frame
                .registry_entries
                .last()
                .copied()
                .unwrap_or(RegistryPathId::ROOT);
            let projected = Self::project_node(frame.node.as_ref(), &child_disk_ptrs)?;
            let terminus_ptr = self.serialize_projected_node(
                &projected,
                &child_disk_ptrs,
                &path,
                terminus_registry_path,
                build.registry_mut(),
            )?;
            if reuse_durable {
                build.defer_stamp(&frame.node, terminus_ptr.to_raw())?;
            }
            let registry_span = match frame.registry_start {
                Some(start) => {
                    let registry = build.registry_mut().ok_or_else(|| {
                        crate::persistent_artrie::error::PersistentARTrieError::internal(
                            "compressed serializer frame start has no registry",
                        )
                    })?;
                    Some(Self::finish_registry_subtree(registry, start)?)
                }
                None => None,
            };
            build.mark_complete(
                &frame.node,
                ExpectedBuildState::Active,
                &terminus_ptr,
                registry_span,
            )?;

            // (2) Collapse the peeled chain into a chunk stack ABOVE the terminus (bottom-up). Each
            // chunk carries <= K::MAX_PREFIX_LEN inter-edge units as its prefix + one out-edge. Empty
            // chain ⇒ the terminus is the top. #6: each chunk registers at its TRUE expanded depth
            // `ends[c] = base+1+Σ_{i<c}(|P_i|+1)` and #6-stamps its LIVE top-of-span node.
            let top_ptr = if frame.chain_prefix.is_empty() {
                terminus_ptr
            } else {
                let chunk_width = chain_chunk_width::<K>()?;
                let chunk_count = chain_chunk_count(frame.chain_prefix.len(), chunk_width)?;
                let chain_head = frame.base_depth.checked_add(1).ok_or_else(|| {
                    crate::persistent_artrie::error::PersistentARTrieError::internal(
                        "compressed serializer chain-head depth overflow",
                    )
                })?;
                let synth = Self::new_synth_node();
                let mut child_ptr = terminus_ptr;
                for chunk_index in (0..chunk_count).rev() {
                    let (chunk_start, chunk_end) =
                        chain_chunk_bounds(frame.chain_prefix.len(), chunk_width, chunk_index)?;
                    let edge_index = chunk_end.checked_sub(1).ok_or_else(|| {
                        crate::persistent_artrie::error::PersistentARTrieError::internal(
                            "compressed serializer emitted an empty chain chunk",
                        )
                    })?;
                    let prefix =
                        frame
                            .chain_prefix
                            .get(chunk_start..edge_index)
                            .ok_or_else(|| {
                                crate::persistent_artrie::error::PersistentARTrieError::internal(
                                    "compressed serializer chain-chunk prefix is out of range",
                                )
                            })?;
                    let edge = *frame.chain_prefix.get(edge_index).ok_or_else(|| {
                        crate::persistent_artrie::error::PersistentARTrieError::internal(
                            "compressed serializer chain-chunk edge is out of range",
                        )
                    })?;
                    // idx = ends[c] - base - 1 = Σ_{i<c}(|P_i|+1) = this chunk's top-of-span live node.
                    let top_live = frame.live_spine.get(chunk_start).ok_or_else(|| {
                        crate::persistent_artrie::error::PersistentARTrieError::internal(
                            "compressed serializer live-spine index is out of range",
                        )
                    })?;
                    let chunk_path_end = chain_head.checked_add(chunk_start).ok_or_else(|| {
                        crate::persistent_artrie::error::PersistentARTrieError::internal(
                            "compressed serializer chunk-path endpoint overflow",
                        )
                    })?;
                    let chunk_path = path.get(..chunk_path_end).ok_or_else(|| {
                        crate::persistent_artrie::error::PersistentARTrieError::internal(
                            "compressed serializer chunk path is out of range",
                        )
                    })?;
                    let chunk_registry_path = if frame.registry_entries.is_empty() {
                        RegistryPathId::ROOT
                    } else {
                        frame
                            .registry_entries
                            .get(chunk_index)
                            .copied()
                            .ok_or_else(|| {
                                crate::persistent_artrie::error::PersistentARTrieError::internal(
                                    "compressed serializer chunk registry path is unavailable",
                                )
                            })?
                    };
                    let child_slots = [(edge, child_ptr.clone())];
                    let chunk_proj = Self::project_chunk(&synth, &child_slots, prefix)?;
                    let next_ptr = self.serialize_projected_node(
                        &chunk_proj,
                        &child_slots,
                        chunk_path,
                        chunk_registry_path,
                        build.registry_mut(),
                    )?;
                    if reuse_durable {
                        build.defer_stamp(top_live, next_ptr.to_raw())?;
                    }
                    child_ptr = next_ptr;
                }
                child_ptr
            };

            // Symmetric pop of THIS frame's pushed `[edge] ++ chain_prefix` segment.
            let expected_path_len = frame
                .base_depth
                .checked_add(frame.pushed_units)
                .ok_or_else(|| {
                    crate::persistent_artrie::error::PersistentARTrieError::internal(
                        "compressed serializer frame path length overflow",
                    )
                })?;
            if path.len() != expected_path_len {
                return Err(
                    crate::persistent_artrie::error::PersistentARTrieError::internal(
                        "compressed serializer frame path is unbalanced",
                    ),
                );
            }
            path.truncate(frame.base_depth);

            match frame.parent_slot {
                Some(slot_index) => completed = Some((slot_index, top_ptr)),
                None => return Ok(top_ptr),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use super::*;
    use crate::persistent_artrie::core::concurrency::EpochManager;
    use crate::persistent_artrie::core::eviction::EvictionConfig;
    use crate::persistent_artrie::core::key_encoding::ByteKey;
    use crate::persistent_artrie::core::overlay::node::Child;
    use crate::persistent_artrie::core::swizzled_ptr::NodeType;

    #[derive(Default)]
    struct RecordingSerializer {
        writes: AtomicUsize,
        durable_import_reads: AtomicUsize,
        children_by_write: Mutex<Vec<Vec<u64>>>,
    }

    impl RecordingSerializer {
        fn recorded_children(&self) -> Vec<Vec<u64>> {
            self.children_by_write
                .lock()
                .expect("recording serializer lock poisoned")
                .clone()
        }
    }

    impl OverlayCompressedSerialize<ByteKey, ()> for RecordingSerializer {
        type Projected = ();

        fn project_node(
            _node: &OverlayNode<ByteKey, ()>,
            _child_disk_ptrs: &[(u8, SwizzledPtr)],
        ) -> Result<Self::Projected> {
            Ok(())
        }

        fn project_chunk(
            _synth: &OverlayNode<ByteKey, ()>,
            _child_disk_ptrs: &[(u8, SwizzledPtr)],
            _prefix: &[u8],
        ) -> Result<Self::Projected> {
            Ok(())
        }

        fn serialize_projected_node(
            &self,
            _projected: &Self::Projected,
            child_disk_ptrs: &[(u8, SwizzledPtr)],
            _path: &[u8],
            _registry_path: RegistryPathId,
            _registry: Option<&mut DiskLocationRegistry>,
        ) -> Result<SwizzledPtr> {
            let write_index = self.writes.fetch_add(1, Ordering::Relaxed);
            let mut recorded = self
                .children_by_write
                .lock()
                .expect("recording serializer lock poisoned");
            recorded.push(
                child_disk_ptrs
                    .iter()
                    .map(|(_, pointer)| pointer.to_raw())
                    .collect(),
            );
            let offset = u32::try_from(write_index.checked_add(1).expect("test write overflow"))
                .expect("test write offset exceeds u32");
            Ok(SwizzledPtr::on_disk(1, offset, NodeType::Node4))
        }

        fn reserve_registry_path(
            registry: &mut DiskLocationRegistry,
            parent: RegistryPathId,
            segment: &[u8],
        ) -> Result<RegistryPathId> {
            registry
                .try_reserve_byte_path(parent, segment)
                .map_err(crate::persistent_artrie::error::PersistentARTrieError::internal)
        }

        fn prepare_registry_subtree_start(registry: &mut DiskLocationRegistry) -> Result<()> {
            registry
                .try_prepare_byte_builder_subtree_start()
                .map_err(|error| {
                    crate::persistent_artrie::error::PersistentARTrieError::internal(
                        error.to_string(),
                    )
                })
        }

        fn begin_registry_subtree(
            registry: &mut DiskLocationRegistry,
            root: RegistryPathId,
        ) -> Result<RegistryBuilderSubtreeStart> {
            registry
                .try_begin_byte_builder_subtree(root)
                .map(RegistryBuilderSubtreeStart::Byte)
                .map_err(|error| {
                    crate::persistent_artrie::error::PersistentARTrieError::internal(
                        error.to_string(),
                    )
                })
        }

        fn cancel_registry_subtree(
            registry: &mut DiskLocationRegistry,
            start: RegistryBuilderSubtreeStart,
        ) -> Result<()> {
            let RegistryBuilderSubtreeStart::Byte(start) = start else {
                return Err(
                    crate::persistent_artrie::error::PersistentARTrieError::internal(
                        "byte recording serializer received a character builder start",
                    ),
                );
            };
            registry
                .try_cancel_byte_builder_subtree(start)
                .map_err(|error| {
                    crate::persistent_artrie::error::PersistentARTrieError::internal(
                        error.to_string(),
                    )
                })
        }

        fn finish_registry_subtree(
            registry: &mut DiskLocationRegistry,
            start: RegistryBuilderSubtreeStart,
        ) -> Result<RegistryBuilderSubtree> {
            let RegistryBuilderSubtreeStart::Byte(start) = start else {
                return Err(
                    crate::persistent_artrie::error::PersistentARTrieError::internal(
                        "byte recording serializer received a character builder start",
                    ),
                );
            };
            registry
                .try_finish_byte_builder_subtree(start)
                .map(RegistryBuilderSubtree::Byte)
                .map_err(|error| {
                    crate::persistent_artrie::error::PersistentARTrieError::internal(
                        error.to_string(),
                    )
                })
        }

        fn graft_registry_subtree(
            registry: &mut DiskLocationRegistry,
            source: &RegistryBuilderSubtree,
            destination: RegistryPathId,
            expected_root: &SwizzledPtr,
            expected_root_resident: bool,
        ) -> Result<LocalRegistryGraftStats> {
            let RegistryBuilderSubtree::Byte(source) = source else {
                return Err(
                    crate::persistent_artrie::error::PersistentARTrieError::internal(
                        "byte recording serializer received a character builder span",
                    ),
                );
            };
            registry
                .try_graft_byte_builder_subtree(
                    source,
                    destination,
                    expected_root,
                    expected_root_resident,
                )
                .map_err(|error| {
                    crate::persistent_artrie::error::PersistentARTrieError::internal(
                        error.to_string(),
                    )
                })
        }

        fn try_reuse_durable_subtree(
            &self,
            ptr: &SwizzledPtr,
            path: &[u8],
            registry_path: RegistryPathId,
            registry: &mut DiskLocationRegistry,
            _structural_source: Option<&RegistryStructuralSource>,
            root_resident: bool,
        ) -> Result<bool> {
            if root_resident {
                return Ok(false);
            }
            self.durable_import_reads.fetch_add(1, Ordering::Relaxed);
            let location = ptr.disk_location().ok_or_else(|| {
                crate::persistent_artrie::error::PersistentARTrieError::corrupted(
                    "recording serializer received a malformed durable pointer",
                )
            })?;
            registry
                .register_nonresident_byte_path(
                    registry_path,
                    ptr.clone(),
                    13,
                    path.len(),
                    location.node_type,
                )
                .map_err(crate::persistent_artrie::error::PersistentARTrieError::internal)?;
            Ok(true)
        }

        fn new_synth_node() -> OverlayNode<ByteKey, ()> {
            OverlayNode::new()
        }
    }

    #[test]
    fn arborescent_policy_is_zero_sized_and_drop_free() {
        assert_eq!(std::mem::size_of::<ArborescentProduction>(), 0);
        assert!(!std::mem::needs_drop::<ArborescentProduction>());
    }

    #[test]
    fn arborescent_and_dag_policies_emit_identical_unique_tree() {
        let leaf_a = Arc::new(OverlayNode::<ByteKey, ()>::new());
        let leaf_b = Arc::new(OverlayNode::<ByteKey, ()>::new());
        let branch =
            Arc::new(OverlayNode::<ByteKey, ()>::new().with_child(b'c', Child::InMem(leaf_b)));
        let root = Arc::new(
            OverlayNode::<ByteKey, ()>::new()
                .with_child(b'a', Child::InMem(leaf_a))
                .with_child(b'b', Child::InMem(branch)),
        );

        let tree_serializer = RecordingSerializer::default();
        let mut tree_build = OverlaySerializationBuild::production_disabled();
        let tree_root = tree_serializer
            .serialize_compressed_loop(&root, &mut tree_build)
            .expect("serialize certified tree");

        let dag_serializer = RecordingSerializer::default();
        let mut dag_build = OverlaySerializationBuild::dag_disabled();
        let dag_root = dag_serializer
            .serialize_compressed_loop(&root, &mut dag_build)
            .expect("serialize checked tree");

        assert_eq!(tree_root.to_raw(), dag_root.to_raw());
        assert_eq!(
            tree_serializer.writes.load(Ordering::Relaxed),
            dag_serializer.writes.load(Ordering::Relaxed)
        );
        assert_eq!(
            tree_serializer.recorded_children(),
            dag_serializer.recorded_children()
        );
    }

    #[test]
    fn disabled_and_analysis_stamp_paths_do_not_clone_nodes() {
        let node = Arc::new(OverlayNode::<ByteKey, ()>::new());
        let initial = Arc::strong_count(&node);

        let mut disabled = OverlaySerializationBuild::production_disabled();
        disabled.defer_stamp(&node, 1).expect("disabled stamp path");
        assert_eq!(Arc::strong_count(&node), initial);

        let mut analysis = OverlaySerializationBuild::analysis(DiskLocationRegistry::new());
        analysis.defer_stamp(&node, 2).expect("analysis stamp path");
        assert_eq!(Arc::strong_count(&node), initial);
    }

    #[test]
    fn enabled_serializer_defers_each_emitted_stamp_once_with_exact_raw_pointer() {
        let node = Arc::new(OverlayNode::<ByteKey, ()>::new());
        let initial = Arc::strong_count(&node);
        let coordinator =
            EvictionCoordinator::new(EvictionConfig::default(), Arc::new(EpochManager::new()));
        let serializer = RecordingSerializer::default();
        let mut build = OverlaySerializationBuild::production_with_eviction(coordinator, None);

        let pointer = serializer
            .serialize_compressed_loop(&node, &mut build)
            .expect("serialize enabled root");

        let RegistryBuildMode::Enabled {
            deferred_stamps, ..
        } = &build.mode
        else {
            panic!("enabled build changed mode");
        };
        assert_eq!(deferred_stamps.len(), 1);
        assert_eq!(Arc::strong_count(&node), initial + 1);
        deferred_stamps[0].apply();
        assert_eq!(node.durable_stamp(), pointer.to_raw());
    }

    #[test]
    fn shared_sibling_arc_is_serialized_once_and_reused_twice() {
        let shared = Arc::new(OverlayNode::<ByteKey, ()>::new());
        let root = Arc::new(
            OverlayNode::<ByteKey, ()>::new()
                .with_child(b'a', Child::InMem(Arc::clone(&shared)))
                .with_child(b'b', Child::InMem(shared)),
        );
        let serializer = RecordingSerializer::default();
        let mut build = OverlaySerializationBuild::dag_disabled();

        serializer
            .serialize_compressed_loop(&root, &mut build)
            .expect("serialize shared sibling DAG");

        assert_eq!(serializer.writes.load(Ordering::Relaxed), 2);
        let writes = serializer.recorded_children();
        assert!(writes[0].is_empty());
        assert_eq!(writes[1].len(), 2);
        assert_eq!(writes[1][0], writes[1][1]);
    }

    #[test]
    fn shared_node_inside_unary_chain_is_a_compression_boundary() {
        let shared = Arc::new(OverlayNode::<ByteKey, ()>::new());
        let chain_head = Arc::new(
            OverlayNode::<ByteKey, ()>::new().with_child(b'x', Child::InMem(Arc::clone(&shared))),
        );
        let root = Arc::new(
            OverlayNode::<ByteKey, ()>::new()
                .with_child(b'a', Child::InMem(chain_head))
                .with_child(b'b', Child::InMem(shared)),
        );
        let serializer = RecordingSerializer::default();
        let mut build = OverlaySerializationBuild::dag_disabled();

        serializer
            .serialize_compressed_loop(&root, &mut build)
            .expect("serialize shared node beneath unary chain");

        assert_eq!(serializer.writes.load(Ordering::Relaxed), 3);
        let writes = serializer.recorded_children();
        assert!(writes[0].is_empty());
        assert_eq!(
            writes[1],
            vec![SwizzledPtr::on_disk(1, 1, NodeType::Node4).to_raw()]
        );
        assert_eq!(writes[2].len(), 2);
        assert_eq!(
            writes[2][0],
            SwizzledPtr::on_disk(1, 2, NodeType::Node4).to_raw()
        );
        assert_eq!(
            writes[2][1],
            SwizzledPtr::on_disk(1, 1, NodeType::Node4).to_raw()
        );
    }

    #[test]
    fn repeated_on_disk_siblings_import_once_and_graft_nonresident() {
        const ALIASES: usize = 8;
        let durable = SwizzledPtr::on_disk(9, 7, NodeType::Node4);
        let mut root = OverlayNode::<ByteKey, ()>::new();
        for edge in b'a'..=b'h' {
            root = root.with_child(edge, Child::OnDisk(durable.clone()));
        }
        let root = Arc::new(root);
        let serializer = RecordingSerializer::default();
        let mut registry = DiskLocationRegistry::new();
        let mut observed_stats = None;

        try_analysis_registry_transaction::<ByteKey, (), (), _>(&mut registry, |build| {
            serializer.serialize_compressed_loop(&root, build)?;
            observed_stats = Some(build.on_disk_import_stats);
            Ok(())
        })
        .expect("serialize repeated durable aliases");

        let stats = observed_stats.expect("durable import stats");
        assert_eq!(stats.lookups, ALIASES);
        assert_eq!(stats.source_imports, 1);
        assert_eq!(stats.local_grafts, ALIASES - 1);
        assert_eq!(stats.local_graft_topology_entries, 0);
        assert_eq!(stats.local_graft_durable_records, ALIASES - 1);
        assert_eq!(stats.local_graft_serialized_bytes, 13 * (ALIASES - 1));
        assert_eq!(serializer.durable_import_reads.load(Ordering::Relaxed), 1);
        assert_eq!(serializer.writes.load(Ordering::Relaxed), 1);
        assert_eq!(registry.len(), ALIASES);
        assert_eq!(registry.byte_resident_len(), 0);
        assert_eq!(registry.total_size_bytes(), 13 * ALIASES);
        let writes = serializer.recorded_children();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0], vec![durable.to_raw(); ALIASES]);
    }

    #[test]
    fn same_durable_address_with_conflicting_type_fails_transactionally() {
        let node4 = SwizzledPtr::on_disk(10, 11, NodeType::Node4);
        let node16 = SwizzledPtr::on_disk(10, 11, NodeType::Node16);
        let root = Arc::new(
            OverlayNode::<ByteKey, ()>::new()
                .with_child(b'a', Child::OnDisk(node4))
                .with_child(b'b', Child::OnDisk(node16)),
        );
        let serializer = RecordingSerializer::default();
        let mut registry = DiskLocationRegistry::new();
        register_test_record(&mut registry, b"old", 1, 7);

        let error =
            try_analysis_registry_transaction::<ByteKey, (), (), _>(&mut registry, |build| {
                serializer.serialize_compressed_loop(&root, build)?;
                Ok(())
            })
            .expect_err("conflicting durable node types must fail");

        assert!(error
            .to_string()
            .contains("different node types for one arena record"));
        assert_eq!(serializer.durable_import_reads.load(Ordering::Relaxed), 1);
        assert_eq!(serializer.writes.load(Ordering::Relaxed), 0);
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.total_size_bytes(), 7);
    }

    fn register_test_record(
        registry: &mut DiskLocationRegistry,
        path: &[u8],
        slot: u32,
        bytes: usize,
    ) {
        registry.register(
            path.to_vec(),
            SwizzledPtr::on_disk(1, slot, NodeType::Node4),
            bytes,
            path.len(),
            NodeType::Node4,
        );
    }

    #[test]
    fn analysis_registry_transaction_restores_prior_registry_on_error() {
        let mut registry = DiskLocationRegistry::new();
        register_test_record(&mut registry, b"old", 1, 7);

        let result =
            try_analysis_registry_transaction::<ByteKey, (), (), _>(&mut registry, |build| {
                let fresh = build.registry_mut().expect("analysis registry");
                assert!(fresh.is_empty());
                register_test_record(fresh, b"partial", 2, 99);
                Err(
                    crate::persistent_artrie::error::PersistentARTrieError::internal(
                        "injected analysis failure",
                    ),
                )
            });

        assert!(result.is_err());
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.total_size_bytes(), 7);
    }

    #[test]
    fn analysis_registry_transaction_preserves_prior_registry_on_unwind() {
        let mut registry = DiskLocationRegistry::new();
        register_test_record(&mut registry, b"old", 1, 7);
        let prior_binding = registry.binding();

        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ =
                try_analysis_registry_transaction::<ByteKey, (), (), _>(&mut registry, |build| {
                    let fresh = build.registry_mut().expect("analysis registry");
                    register_test_record(fresh, b"partial", 2, 99);
                    panic!("injected analysis unwind");
                });
        }));

        assert!(unwind.is_err());
        assert!(registry.binding().same_publication(&prior_binding));
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.total_size_bytes(), 7);
    }

    #[test]
    fn analysis_registry_transaction_replaces_prior_registry_on_success() {
        let mut registry = DiskLocationRegistry::new();
        register_test_record(&mut registry, b"old", 1, 7);

        let result =
            try_analysis_registry_transaction::<ByteKey, (), u8, _>(&mut registry, |build| {
                let fresh = build.registry_mut().expect("analysis registry");
                assert!(fresh.is_empty());
                register_test_record(fresh, b"new", 2, 11);
                Ok(42)
            })
            .expect("analysis transaction should commit");

        assert_eq!(result, 42);
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.total_size_bytes(), 11);
    }

    #[test]
    fn analysis_registry_transaction_rejects_unfinished_builder_span() {
        let mut registry = DiskLocationRegistry::new();
        register_test_record(&mut registry, b"old", 1, 7);

        let result =
            try_analysis_registry_transaction::<ByteKey, (), (), _>(&mut registry, |build| {
                let fresh = build.registry_mut().expect("analysis registry");
                let root = fresh
                    .try_reserve_byte_path(RegistryPathId::ROOT, b"unfinished")
                    .map_err(crate::persistent_artrie::error::PersistentARTrieError::internal)?;
                fresh
                    .try_begin_byte_builder_subtree(root)
                    .map_err(|error| {
                        crate::persistent_artrie::error::PersistentARTrieError::internal(
                            error.to_string(),
                        )
                    })?;
                Ok(())
            });

        assert!(result.is_err());
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.total_size_bytes(), 7);
    }
}
