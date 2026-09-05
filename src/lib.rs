//! High-performance dictionary data structures — tries, DAWGs, double-array tries, suffix
//! automata, compact suffix graphs, and lock-free durable Adaptive Radix Tries —
//! unified behind one trait API.
//!
//! libdictenstein provides the *container* half of approximate string matching: efficient,
//! traversable collections of terms. The *query* half — a Levenshtein-automaton transducer —
//! lives in the companion crate [`liblevenshtein`](https://github.com/vinary-tree/liblevenshtein-rust),
//! which walks any type implementing [`Dictionary`]. This crate contains no fuzzy-matching code itself.
//!
//! # Architecture
//!
//! Every backend implements a small, layered set of traits. **Read** traits
//! ([`Dictionary`], [`MappedDictionary`], [`BijectiveDictionary`]) handle query and
//! traversal; **mutation** traits ([`MutableDictionary`], [`CompactableDictionary`])
//! add `insert` / `remove` / `compact`; the **persistent** `ARTrie` traits add a
//! lock-free compare-and-swap publish path plus checkpointing. The [`CharUnit`]
//! (edge label) and `KeyEncoding` (persistent key) abstractions let one
//! implementation serve `u8`, `char`, and `u64` alphabets from a single code path.
//!
//! <img src="https://raw.githubusercontent.com/vinary-tree/libdictenstein/master/docs/diagrams/traits.svg" alt="libdictenstein trait layer — read API and associated-type bounds (1 of 2)" width="620"/>
//!
//! <img src="https://raw.githubusercontent.com/vinary-tree/libdictenstein/master/docs/diagrams/traits-2.svg" alt="libdictenstein trait layer — mutation and persistent APIs (2 of 2)" width="560"/>
//!
//! See the [documentation index](https://github.com/vinary-tree/libdictenstein/blob/master/docs/README.md)
//! for theory, per-backend algorithm walkthroughs, persistence architecture, and the
//! formal-verification corpus.
//!
//! # Choosing a Dictionary Backend
//!
//! ## In-memory backends
//!
//! | Backend | Best For | Performance | Memory | Dynamic Updates | Unicode |
//! |---------|----------|-------------|--------|-----------------|---------|
//! | **[DoubleArrayTrie]** | General use (recommended) | ⭐⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ | ✅ Insert-only | Byte-level |
//! | **[DoubleArrayTrieChar]** | Unicode text | ⭐⭐⭐⭐ | ⭐⭐⭐⭐ | ✅ Insert-only | ✅ Character-level |
//! | **[DynamicDawg]** | Insert + Remove | ⭐⭐⭐ | ⭐⭐⭐ | ✅ Thread-safe | Byte-level |
//! | **[DynamicDawgChar]** | Unicode + Insert + Remove | ⭐⭐⭐ | ⭐⭐⭐ | ✅ Thread-safe | ✅ Character-level |
//! | **[DynamicDawgU64]** | Token sequences, time series | ⭐⭐⭐ | ⭐⭐ | ✅ Thread-safe | 64-bit labels |
//! | **[SuffixAutomaton]** | Substring search | ⭐⭐⭐ | ⭐⭐ | ✅ Insert + Remove | Byte-level |
//! | **[SuffixAutomatonChar]** | Unicode substring search | ⭐⭐⭐ | ⭐⭐ | ✅ Insert + Remove | ✅ Character-level |
//! | **[Scdawg]** | Substring search (static, compact) | ⭐⭐⭐⭐ | ⭐⭐⭐⭐ | ✅ Insert-only | Byte-level |
//! | **[ScdawgChar]** | Unicode substring search (static) | ⭐⭐⭐⭐ | ⭐⭐⭐⭐ | ✅ Insert-only | ✅ Character-level |
//! | **`PathMapDictionary`** (feature `pathmap-backend`) | Fast in-memory queries | ⭐⭐⭐⭐ | ⭐⭐⭐ | ✅ Thread-safe | Byte-level |
//! | **`PathMapDictionaryChar`** (feature `pathmap-backend`) | Fast in-memory queries (Unicode) | ⭐⭐⭐⭐ | ⭐⭐⭐ | ✅ Thread-safe | ✅ Character-level |
//!
//! ## Disk-backed backends (feature `persistent-artrie`)
//!
//! | Backend | Best For | Persistence | Concurrency | Unicode |
//! |---------|----------|-------------|-------------|---------|
//! | **[PersistentARTrie]** | Disk-backed key/value, byte keys | mmap + WAL | Lock-free CAS | Byte-level |
//! | **[PersistentARTrieChar]** | Disk-backed key/value, Unicode | mmap + WAL | Lock-free CAS | ✅ Character-level |
//! | **[PersistentARTrieU64]** / **[PersistentARTrieU64Compact]** | Disk-backed sequence key/value, native u64 labels | overlay CX snapshot + WAL | Lock-free CAS | 64-bit labels |
//! | **[PersistentARTrieU64Prefix3Compat]** | Prefix-3 u64 CX compatibility/baseline profile | overlay CX snapshot + WAL | Lock-free CAS | 64-bit labels |
//! | **[PersistentSuffixAutomaton]** | Disk-backed substring search, byte keys | native suffix snapshot + WAL | Snapshot reads, COW writes | Byte-level |
//! | **[PersistentSuffixAutomatonChar]** | Disk-backed Unicode substring search | native suffix snapshot + WAL | Snapshot reads, COW writes | ✅ Character-level |
//! | **[PersistentSuffixTree]** | Disk-backed suffix-tree-compatible substring API, byte keys | native compact suffix-tree snapshot + WAL | Snapshot reads, COW writes | Byte-level |
//! | **[PersistentSuffixTreeChar]** | Disk-backed suffix-tree-compatible Unicode substring API | native compact suffix-tree snapshot + WAL | Snapshot reads, COW writes | ✅ Character-level |
//! | **[PersistentScdawg]** | Disk-backed compact-suffix API, byte keys | native SCDAWG snapshot + WAL | Snapshot reads, COW writes | Byte-level |
//! | **[PersistentScdawgChar]** | Disk-backed compact-suffix API, Unicode | native SCDAWG snapshot + WAL | Snapshot reads, COW writes | ✅ Character-level |
//! | **[PersistentVocabARTrie]** | Vocabulary trie (term ↔ u64 index) | overlay checkpoint + WAL | Lock-free CAS | ✅ Character-level |
//!
//! Use the [`factory::DictionaryFactory`] for a unified construction API across
//! all in-memory backends. See [`bijective::BijectiveDictionary`] for the
//! bidirectional-lookup trait shared by `BijectiveMap` and the vocab tries.
//!
//! [DoubleArrayTrie]: double_array_trie::DoubleArrayTrie
//! [DoubleArrayTrieChar]: double_array_trie::DoubleArrayTrieChar
//! [DynamicDawg]: dynamic_dawg::DynamicDawg
//! [DynamicDawgChar]: dynamic_dawg::DynamicDawgChar
//! [DynamicDawgU64]: dynamic_dawg::DynamicDawgU64
//! [SuffixAutomaton]: suffix_automaton::SuffixAutomaton
//! [SuffixAutomatonChar]: suffix_automaton::SuffixAutomatonChar
//! [Scdawg]: scdawg::Scdawg
//! [ScdawgChar]: scdawg::ScdawgChar
//! [PersistentARTrie]: persistent_artrie::PersistentARTrie
//! [PersistentARTrieChar]: persistent_artrie::char::PersistentARTrieChar
//! [PersistentARTrieU64]: persistent_artrie::PersistentARTrieU64
//! [PersistentARTrieU64Compact]: persistent_artrie::PersistentARTrieU64Compact
//! [PersistentARTrieU64Prefix3Compat]: persistent_artrie::PersistentARTrieU64Prefix3Compat
//! [PersistentSuffixAutomaton]: persistent_artrie::PersistentSuffixAutomaton
//! [PersistentSuffixAutomatonChar]: persistent_artrie::PersistentSuffixAutomatonChar
//! [PersistentSuffixTree]: persistent_artrie::PersistentSuffixTree
//! [PersistentSuffixTreeChar]: persistent_artrie::PersistentSuffixTreeChar
//! [PersistentScdawg]: persistent_artrie::PersistentScdawg
//! [PersistentScdawgChar]: persistent_artrie::PersistentScdawgChar
//! [PersistentVocabARTrie]: persistent_artrie::vocab::PersistentVocabARTrie

// === Shared infrastructure ===
mod causal_perf;
#[doc(hidden)]
pub mod concurrent_slots;
#[doc(hidden)]
pub use causal_perf::{
    causal_construction_stats, reset_causal_construction_stats, CausalConstructionStats,
};
#[cfg(feature = "perf-instrumentation")]
#[doc(hidden)]
pub use causal_perf::{
    persistent_serialization_stats, reset_persistent_serialization_stats,
    PersistentSerializationStats,
};
pub mod bijective;
#[cfg(feature = "bindings-core")]
pub mod bindings;
pub mod bloom_filter;
pub mod char_unit;
pub mod collection;
pub mod factory;
#[cfg(feature = "ffi")]
pub mod ffi;
pub mod iterator;
pub mod node_signature;
mod nonblocking;
pub mod substring;
pub mod sync_compat;
pub mod value;
pub mod zipper;

// === Zipper combinators ===
pub mod difference_zipper;
pub mod excluding_prefix_zipper;
pub mod intersection_zipper;
pub mod prefix_zipper;
pub mod symmetric_difference_zipper;
pub mod union_zipper;
pub mod value_diff_zipper;

// === Dictionary families ===
// Each family is a directory submodule whose `mod.rs` re-exports the family's
// public types. Within a family: `ascii` = byte/`u8` base, `char` = Unicode
// (`char`), `u64` = `u64`-labeled (dynamic_dawg only), `core` = the unit-generic
// substrate shared by the variants, and `*zipper` = the navigators.
pub mod double_array_trie;
pub mod dynamic_dawg;
pub mod interning;
#[cfg(feature = "pathmap-backend")]
pub mod pathmap;
pub mod profile;
pub mod scdawg;
pub mod suffix_automaton;
pub mod variable_width;

// === Persistent ARTrie modules (feature-gated at module level) ===
// These modules are gated here; internal code does NOT need feature gates.
//
// Layering: `persistent_artrie::core` is the shared substrate; the three
// variants depend on core, never on each other. See
// `persistent_artrie/core/mod.rs` for the invariant.
#[cfg(feature = "persistent-artrie")]
pub mod artrie_trait;
#[cfg(feature = "persistent-artrie")]
pub mod persistent_artrie;

#[cfg(any(feature = "serialization", feature = "protobuf"))]
pub mod serialization;

// Re-export core types at crate root
pub use bijective::{BijectiveDictionary, BijectiveMap, InsertError};
pub use bloom_filter::BloomFilter;
pub use char_unit::CharUnit;
pub use collection::{
    DictionaryEntries, DictionaryEntriesIter, DictionaryEntry, DictionaryKeys,
    DictionaryLanguageEntries, DictionaryLanguageTerms, DictionaryTerms, DictionaryValues,
    ExactSnapshotEntryIterator, SnapshotEntryIterator, SnapshotTermIterator,
    ValuedZipperCollection, ZipperCollection, ZipperEntryIterator, ZipperTermIterator,
};
pub use double_array_trie::{DoubleArrayTrieUleb128, DoubleArrayTrieUtf8};
pub use dynamic_dawg::core::{DawgCore, DawgNode};
pub use dynamic_dawg::{
    DynamicDawgByteProfile, DynamicDawgCharProfile, DynamicDawgGeneric, DynamicDawgProfile,
    DynamicDawgU32, DynamicDawgU64Profile, DynamicDawgUleb128, DynamicDawgUtf8,
};
pub use interning::{
    InternedEntries, InternedId, InternedIdDictionaryView, InternedSequence,
    InternedSequenceDictionary, InternedSequenceDictionaryU64, InternedUlebSequenceDictionary,
    InternedUlebSequenceDictionaryU64, InternedVocabulary, InterningError,
};
pub use iterator::{DictionaryIterator, DictionaryTermIterator};
pub use node_signature::NodeSignature;
#[cfg(feature = "pathmap-backend")]
pub use pathmap::PathMapDictionaryUleb128;
#[cfg(feature = "pathmap-backend")]
pub use pathmap::PathMapDictionaryUtf8;
pub use profile::{
    AtomProfile, AtomSequence, AtomStream, Bytes, F64Bits, ProfileError, ProfileKind, Uleb128Atom,
    UnicodeScalar, Utf8, U32, U64,
};
pub use substring::{
    BidirectionalDictionaryNode, ExtensionResult, SubstringDictionary, SubstringMatch,
};
pub use value::DictionaryValue;
pub use variable_width::{
    validate_uleb128_sequence, Uleb128, Uleb128Codec, Uleb128Error, Uleb128Ref, Uleb128Sequence,
    Uleb128Stream, VariableWidthCodec, VariableWidthProfile, ULEB128_PROFILE,
};
pub use zipper::{DictZipper, ValuedDictZipper, ZipperTraversalNode};

// Re-export persistent ARTrie types (only available with feature)
#[cfg(feature = "persistent-artrie")]
pub use artrie_trait::{ARTrie, EvictableARTrie};
// `ARTrieAtomicOps` is #[deprecated]; re-exported behind an allow so the
// re-export site itself doesn't spam warnings. External callers that name
// the trait still get the deprecation message.
#[cfg(feature = "persistent-artrie")]
#[allow(deprecated)]
pub use artrie_trait::ARTrieAtomicOps;
#[cfg(feature = "persistent-artrie")]
pub use persistent_artrie::char::{
    PersistentARTrieChar, PersistentARTrieCharNode, PersistentARTrieCharZipper,
};
#[cfg(feature = "persistent-artrie")]
pub use persistent_artrie::vocab::{IndexedVocabularyPersistent, PersistentVocabARTrie};
#[cfg(feature = "persistent-artrie")]
pub use persistent_artrie::wal::Lsn;
#[cfg(feature = "persistent-artrie")]
pub use persistent_artrie::{
    PersistentARTrie, PersistentARTrieU64, PersistentARTrieU64Node, PersistentARTrieUleb128,
    PersistentARTrieUtf8, PersistentARTrieZipper, PersistentScdawg, PersistentScdawgChar,
    PersistentScdawgCharNode, PersistentScdawgNode, PersistentScdawgUtf8,
    PersistentSuffixAutomaton, PersistentSuffixAutomatonChar, PersistentSuffixAutomatonCharNode,
    PersistentSuffixAutomatonNode, PersistentSuffixAutomatonUtf8, PersistentSuffixTree,
    PersistentSuffixTreeChar, PersistentSuffixTreeCharNode, PersistentSuffixTreeNode,
    PersistentSuffixTreeUtf8, RecoveryMode, RecoveryReport, WalConfig,
};

/// Synchronization strategy for dictionary operations.
///
/// Different dictionary backends may have different thread-safety guarantees.
/// This trait allows backends to specify their synchronization requirements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncStrategy {
    /// Backend requires external synchronization (e.g., RwLock).
    ///
    /// Use this for backends that use interior mutability without
    /// internal synchronization.
    ExternalSync,

    /// Backend is internally synchronized and safe for concurrent access.
    ///
    /// Use this for backends that use atomic operations, locks, or
    /// lock-free data structures internally.
    InternalSync,

    /// Backend is a persistent/immutable data structure.
    ///
    /// Mutations create new versions with structural sharing.
    /// Reads require no synchronization. Writes can use atomic swaps.
    Persistent,
}

/// One compact immutable edge exposed by a captured traversal graph.
#[derive(Clone, Copy, Debug)]
pub struct SnapshotTraversalEdge<U: CharUnit> {
    label: U,
    target: u32,
}

impl<U: CharUnit> SnapshotTraversalEdge<U> {
    /// Construct an edge from a label and zero-based target index.
    pub fn new(label: U, target: u32) -> Self {
        Self { label, target }
    }

    /// Edge label.
    pub fn label(self) -> U {
        self.label
    }

    /// Target as an opaque, one-based traversal cursor.
    pub fn target_cursor(self) -> SnapshotTraversalCursor {
        SnapshotTraversalCursor::from_index(self.target as usize)
            .expect("snapshot traversal targets are one-based")
    }
}

/// Borrowed outgoing edge range and finality for one captured cursor.
pub struct SnapshotTraversalEdges<'a, U: CharUnit> {
    edges: &'a [SnapshotTraversalEdge<U>],
    is_final: bool,
}

impl<'a, U: CharUnit> SnapshotTraversalEdges<'a, U> {
    /// Construct a borrowed edge range.
    pub fn new(edges: &'a [SnapshotTraversalEdge<U>], is_final: bool) -> Self {
        Self { edges, is_final }
    }

    /// Sorted outgoing edges.
    pub fn edges(&self) -> &'a [SnapshotTraversalEdge<U>] {
        self.edges
    }

    /// Whether this node accepts a dictionary term.
    pub fn is_final(&self) -> bool {
        self.is_final
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SnapshotTraversalNode<H = SnapshotTraversalCursor> {
    pub(crate) edge_start: u32,
    pub(crate) edge_len: u32,
    pub(crate) is_final: bool,
    pub(crate) value_handle: H,
}

/// Packed immutable node range: 32-bit edge start, 31-bit edge count, and
/// finality in the high bit. Construction rejects counts that do not fit.
#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
struct SnapshotTraversalRange(u64);

const _: () = assert!(std::mem::size_of::<SnapshotTraversalRange>() == 8);

const SNAPSHOT_RANGE_FINAL_BIT: u64 = 1u64 << 63;
const SNAPSHOT_RANGE_LENGTH_MASK: u64 = u32::MAX as u64 >> 1;

impl SnapshotTraversalRange {
    #[inline]
    fn new(edge_start: u32, edge_len: u32, is_final: bool) -> Option<Self> {
        if u64::from(edge_len) > SNAPSHOT_RANGE_LENGTH_MASK {
            return None;
        }
        let finality = if is_final {
            SNAPSHOT_RANGE_FINAL_BIT
        } else {
            0
        };
        Some(Self(
            u64::from(edge_start) | (u64::from(edge_len) << u32::BITS) | finality,
        ))
    }

    #[inline]
    fn edge_start(self) -> usize {
        self.0 as u32 as usize
    }

    #[inline]
    fn edge_len(self) -> usize {
        ((self.0 >> u32::BITS) & SNAPSHOT_RANGE_LENGTH_MASK) as usize
    }

    #[inline]
    fn is_final(self) -> bool {
        self.0 & SNAPSHOT_RANGE_FINAL_BIT != 0
    }
}

impl<H: Copy> SnapshotTraversalNode<H> {
    /// Construct one node descriptor for a compact immutable graph.
    pub fn new(edge_start: u32, edge_len: u32, is_final: bool, value_handle: H) -> Self {
        Self {
            edge_start,
            edge_len,
            is_final,
            value_handle,
        }
    }

    /// First outgoing edge in the graph's edge array.
    pub fn edge_start(self) -> u32 {
        self.edge_start
    }

    /// Number of outgoing edges.
    pub fn edge_len(self) -> u32 {
        self.edge_len
    }

    /// Whether this node accepts a dictionary term.
    pub fn is_final(self) -> bool {
        self.is_final
    }

    /// Backend-native value handle retained by the graph owner.
    pub fn value_handle(self) -> H {
        self.value_handle
    }
}

/// Shared concrete format for a compact immutable traversal projection.
///
/// Every backend uses the same flat node/edge arrays, so query schedulers can
/// stay monomorphized while the root capture remains backend-neutral.
#[derive(Debug)]
pub struct SnapshotTraversalGraph<U: CharUnit, H = SnapshotTraversalCursor> {
    nodes: Box<[SnapshotTraversalRange]>,
    value_handles: Box<[H]>,
    pub(crate) edges: Box<[SnapshotTraversalEdge<U>]>,
    pub(crate) root: u32,
}

impl<U: CharUnit, H: Copy> SnapshotTraversalGraph<U, H> {
    /// Validate and construct a compact immutable traversal graph.
    ///
    /// Edge ranges may appear in any node order but every range and target
    /// must lie within the supplied arrays. Labels within one node must be
    /// strictly increasing, matching the dictionary edge contract.
    pub fn new(
        nodes: Vec<SnapshotTraversalNode<H>>,
        edges: Vec<SnapshotTraversalEdge<U>>,
        root: u32,
    ) -> Option<Self> {
        if nodes.is_empty() || root as usize >= nodes.len() {
            return None;
        }
        for node in &nodes {
            let start = node.edge_start as usize;
            let end = start.checked_add(node.edge_len as usize)?;
            let range = edges.get(start..end)?;
            let mut previous = None;
            for edge in range {
                if edge.target as usize >= nodes.len()
                    || previous.is_some_and(|label| label >= edge.label)
                {
                    return None;
                }
                previous = Some(edge.label);
            }
        }
        let mut ranges = Vec::with_capacity(nodes.len());
        let mut value_handles = Vec::with_capacity(nodes.len());
        for node in nodes {
            ranges.push(SnapshotTraversalRange::new(
                node.edge_start,
                node.edge_len,
                node.is_final,
            )?);
            value_handles.push(node.value_handle);
        }
        Some(Self {
            nodes: ranges.into_boxed_slice(),
            value_handles: value_handles.into_boxed_slice(),
            edges: edges.into_boxed_slice(),
            root,
        })
    }

    /// Number of immutable graph nodes.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Reconstruct one complete node descriptor.
    pub fn node(&self, index: usize) -> Option<SnapshotTraversalNode<H>> {
        let range = *self.nodes.get(index)?;
        let value_handle = *self.value_handles.get(index)?;
        Some(SnapshotTraversalNode::new(
            range.edge_start() as u32,
            range.edge_len() as u32,
            range.is_final(),
            value_handle,
        ))
    }

    /// All immutable edges.
    pub fn edges(&self) -> &[SnapshotTraversalEdge<U>] {
        &self.edges
    }

    /// Zero-based root node index.
    pub fn root_index(&self) -> u32 {
        self.root
    }

    /// Root cursor of the captured revision.
    #[inline]
    pub fn root_cursor(&self) -> SnapshotTraversalCursor {
        SnapshotTraversalCursor::from_index(self.root as usize)
            .expect("snapshot traversal roots are one-based")
    }

    /// Borrow one cursor's sorted outgoing edge range and finality.
    #[inline]
    pub fn edges_and_finality(
        &self,
        cursor: SnapshotTraversalCursor,
    ) -> SnapshotTraversalEdges<'_, U> {
        let index = cursor.index();
        let node = self.nodes[index];
        let start = node.edge_start();
        let end = start + node.edge_len();
        SnapshotTraversalEdges::new(&self.edges[start..end], node.is_final())
    }

    /// Borrow one internally produced cursor's edge range without repeating
    /// bounds checks already established by graph construction.
    ///
    /// # Safety
    ///
    /// `cursor` must be the root cursor or an edge target produced by this
    /// exact graph. The graph constructor validates every such target and edge
    /// range before publication.
    #[inline]
    pub unsafe fn edges_and_finality_unchecked(
        &self,
        cursor: SnapshotTraversalCursor,
    ) -> SnapshotTraversalEdges<'_, U> {
        let index = cursor.index();
        // SAFETY: upheld by the method contract.
        let node = unsafe { *self.nodes.get_unchecked(index) };
        let start = node.edge_start();
        let len = node.edge_len();
        // SAFETY: every node range was validated by `new`.
        let edges = unsafe { std::slice::from_raw_parts(self.edges.as_ptr().add(start), len) };
        SnapshotTraversalEdges::new(edges, node.is_final())
    }

    /// Backend-native value handle for one dense graph cursor.
    #[inline]
    pub fn value_handle(&self, cursor: SnapshotTraversalCursor) -> H {
        self.value_handles[cursor.index()]
    }
}

/// Compact traversal projection paired with its backend's native value-handle
/// type.
pub type SnapshotTraversalProjection<N> = std::sync::Arc<
    SnapshotTraversalGraph<
        <N as DictionaryNode>::Unit,
        <N as DictionaryNode>::SnapshotGraphValueHandle,
    >,
>;

/// Decomposed traversal capture with native handles dropped before their owner.
pub struct DictionaryTraversalParts<N: DictionaryNode> {
    projection: Option<SnapshotTraversalProjection<N>>,
    root: N,
}

impl<N: DictionaryNode> DictionaryTraversalParts<N> {
    /// Borrow the optional compact projection.
    pub fn projection(&self) -> Option<&SnapshotTraversalProjection<N>> {
        self.projection.as_ref()
    }

    /// Borrow the retained root owner.
    pub fn root(&self) -> &N {
        &self.root
    }

    /// Move out the projection first and its retained root owner last.
    pub fn into_projection_and_root(self) -> (Option<SnapshotTraversalProjection<N>>, N) {
        (self.projection, self.root)
    }
}

/// Root node plus an optional compact traversal projection captured from the
/// same immutable dictionary revision.
pub struct DictionaryTraversalRoot<N: DictionaryNode> {
    snapshot: Option<SnapshotTraversalProjection<N>>,
    node: N,
}

impl<N: DictionaryNode> DictionaryTraversalRoot<N> {
    /// Compatibility root with owned-node traversal only.
    pub fn owned(node: N) -> Self {
        Self {
            node,
            snapshot: None,
        }
    }

    /// Root with a compact captured traversal graph.
    pub fn captured(node: N, snapshot: SnapshotTraversalProjection<N>) -> Self {
        Self {
            node,
            snapshot: Some(snapshot),
        }
    }

    /// Decompose the capture while preserving projection-before-owner drop
    /// order in the returned [`DictionaryTraversalParts`].
    pub fn into_parts(self) -> DictionaryTraversalParts<N> {
        DictionaryTraversalParts {
            projection: self.snapshot,
            root: self.node,
        }
    }
}

/// Core dictionary abstraction for approximate string matching.
///
/// A dictionary represents a collection of terms that can be efficiently
/// traversed character-by-character via graph-like nodes. This trait
/// allows different backend implementations (trie, DAWG, double-array trie,
/// etc.) to be used interchangeably.
pub trait Dictionary {
    /// The node type used for dictionary traversal
    type Node: DictionaryNode;

    /// Get a root node for one immutable dictionary revision.
    ///
    /// The returned node and every descendant reached from it must keep
    /// query-start snapshot semantics: later insertions, removals, value
    /// updates, clears, or compactions on the dictionary cannot change what is
    /// observable through this root. Implementations should use structural
    /// sharing or another O(1) snapshot mechanism rather than copying the
    /// complete dictionary or retaining a read lock for the traversal's
    /// lifetime.
    fn root(&self) -> Self::Node;

    /// Capture the preferred traversal representation for one immutable
    /// dictionary revision.
    ///
    /// The default preserves compatibility through an owned root node.
    /// Backends with a compact immutable node/edge arena can override this to
    /// share that arena once per query and enqueue copyable cursors.
    fn traversal_root(&self) -> DictionaryTraversalRoot<Self::Node> {
        DictionaryTraversalRoot::owned(self.root())
    }

    /// Check if a term exists in the dictionary
    fn contains(&self, term: &str) -> bool {
        let mut node = self.root();
        for unit in <Self::Node as DictionaryNode>::Unit::iter_str(term) {
            match node.transition(unit) {
                Some(next) => node = next,
                None => return false,
            }
        }
        node.is_final()
    }

    /// Get the total number of terms (if available efficiently)
    fn len(&self) -> Option<usize>;

    /// Check if the dictionary is empty
    fn is_empty(&self) -> bool {
        self.len().map(|n| n == 0).unwrap_or(false)
    }

    /// Get the synchronization strategy for this dictionary backend.
    ///
    /// This allows wrappers to optimize synchronization based on
    /// the backend's thread-safety guarantees.
    ///
    /// Default: `ExternalSync` (conservative, always safe)
    fn sync_strategy(&self) -> SyncStrategy {
        SyncStrategy::ExternalSync
    }

    /// Check if this dictionary uses suffix-based matching (substring search).
    ///
    /// Suffix-based dictionaries (like SuffixAutomaton) match substrings anywhere
    /// in the indexed text, whereas prefix-based dictionaries match complete words
    /// from the beginning.
    ///
    /// This affects how the Levenshtein automaton computes match distances:
    /// - Prefix-based: penalizes unmatched query suffix
    /// - Suffix-based: allows partial query matches without penalty
    ///
    /// Default: `false` (prefix-based matching)
    fn is_suffix_based(&self) -> bool {
        false
    }
}

/// One closure-free indexed observation of an immutable cursor node.
///
/// The observation reports node finality and total fanout alongside the edge
/// at the requested index. A valid retained cursor has an edge exactly when
/// `index < total_edge_count`. Backends return this value without cloning an
/// owning child handle.
#[derive(Clone, Copy)]
pub struct SnapshotCursorEdgeObservation<U, C> {
    is_final: bool,
    total_edge_count: usize,
    edge: Option<(U, C)>,
}

impl<U, C> SnapshotCursorEdgeObservation<U, C> {
    /// Construct one complete indexed cursor observation.
    #[inline]
    pub const fn new(is_final: bool, total_edge_count: usize, edge: Option<(U, C)>) -> Self {
        Self {
            is_final,
            total_edge_count,
            edge,
        }
    }

    /// Whether the observed node accepts the path reaching it.
    #[inline]
    pub const fn is_final(&self) -> bool {
        self.is_final
    }

    /// Exact number of deterministic outgoing edges on the observed node.
    #[inline]
    pub const fn total_edge_count(&self) -> usize {
        self.total_edge_count
    }

    /// Consume the observation and return the indexed label and child cursor.
    #[inline]
    pub fn into_edge(self) -> Option<(U, C)> {
        self.edge
    }
}

/// Opaque nonempty suffix of one immutable node's outgoing edge storage.
///
/// The two pointers retain their original allocation provenance. The backend
/// type parameter is invariant, preventing a token created for one node
/// implementation from being passed to another implementation in safe code.
/// The token owns no allocation and changes no child ownership count; its
/// producing snapshot root must remain retained until the token is consumed.
#[repr(C)]
pub struct SnapshotEdgeRangeToken<N: ?Sized> {
    current: std::ptr::NonNull<()>,
    end: std::ptr::NonNull<()>,
    _backend: std::marker::PhantomData<fn(&mut N) -> &mut N>,
}

impl<N: ?Sized> Copy for SnapshotEdgeRangeToken<N> {}

impl<N: ?Sized> Clone for SnapshotEdgeRangeToken<N> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}

impl<N: ?Sized> std::fmt::Debug for SnapshotEdgeRangeToken<N> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SnapshotEdgeRangeToken(..)")
    }
}

impl<N: ?Sized> SnapshotEdgeRangeToken<N> {
    /// Construct a nonempty retained edge range without destroying pointer
    /// provenance.
    ///
    /// # Safety
    ///
    /// `current` must point to an initialized edge in immutable storage owned
    /// transitively by the retained snapshot root. `end` must be the one-past
    /// pointer for the same allocation and exact edge element type, with
    /// `current < end`. The storage must not move, mutate, or be reclaimed
    /// while this token or any token derived from it can be consumed. `N` must
    /// be the exact [`DictionaryNode`] backend that will interpret the token.
    #[inline]
    pub const unsafe fn from_raw_parts(
        current: std::ptr::NonNull<()>,
        end: std::ptr::NonNull<()>,
    ) -> Self {
        Self {
            current,
            end,
            _backend: std::marker::PhantomData,
        }
    }

    /// Consume the opaque token and recover its provenance-bearing pointers.
    ///
    /// Dereferencing, advancing, or otherwise interpreting either pointer is
    /// unsafe and remains subject to [`from_raw_parts`](Self::from_raw_parts)'s
    /// complete contract.
    #[inline]
    pub const fn into_raw_parts(self) -> (std::ptr::NonNull<()>, std::ptr::NonNull<()>) {
        (self.current, self.end)
    }
}

/// Fused observation that begins one retained immutable edge range.
///
/// Empty nodes return neither a first edge nor a remaining token. Unary nodes
/// return only the first edge. Branching nodes return the first edge and a
/// nonempty token for every untouched sibling. The total therefore agrees
/// exactly with the shape `(None, None)`, `(Some, None)`, or `(Some, Some)`.
pub struct SnapshotEdgeRangeStart<U, C, N: ?Sized> {
    is_final: bool,
    total_edge_count: usize,
    first: Option<(U, C)>,
    remaining: Option<SnapshotEdgeRangeToken<N>>,
}

impl<U, C, N: ?Sized> SnapshotEdgeRangeStart<U, C, N> {
    /// Construct one complete range-start observation.
    ///
    /// Implementations of the unsafe range-start trait method must uphold the
    /// documented shape, count, provenance, and retained-owner contract.
    #[inline]
    pub const fn new(
        is_final: bool,
        total_edge_count: usize,
        first: Option<(U, C)>,
        remaining: Option<SnapshotEdgeRangeToken<N>>,
    ) -> Self {
        Self {
            is_final,
            total_edge_count,
            first,
            remaining,
        }
    }

    /// Whether the observed node accepts the path reaching it.
    #[inline]
    pub const fn is_final(&self) -> bool {
        self.is_final
    }

    /// Exact deterministic fanout of the observed node.
    #[inline]
    pub const fn total_edge_count(&self) -> usize {
        self.total_edge_count
    }

    /// Consume the observation into its direct edge and untouched suffix.
    #[inline]
    pub fn into_first_and_remaining(self) -> (Option<(U, C)>, Option<SnapshotEdgeRangeToken<N>>) {
        (self.first, self.remaining)
    }
}

/// One exact step of a retained immutable edge range.
///
/// The optional token is present precisely when another edge remains in the
/// same retained allocation. Naming the observation keeps backend trait
/// signatures readable without changing its tuple representation.
pub type SnapshotEdgeRangeStep<U, C, N> = (U, C, Option<SnapshotEdgeRangeToken<N>>);

/// Traversable dictionary node.
///
/// Nodes form a graph structure representing the dictionary, where edges
/// are labeled with character units (bytes or Unicode characters) and final
/// nodes mark valid terms.
///
/// # Determinism invariant
///
/// A node has at most one outgoing edge for any label. [`transition`](Self::transition),
/// [`edges`](Self::edges), and the visitation methods must describe that same
/// unique mapping. Consequently, a label sequence identifies at most one path
/// from a dictionary root, even when an acyclic graph shares suffix nodes.
/// Query algorithms rely on this invariant and do not retain a redundant set
/// of already-emitted terms.
///
/// # Type Parameters
///
/// The node is generic over [`CharUnit`], which can be:
/// - [`u8`] for byte-level matching (faster, ASCII-optimized)
/// - [`char`] for character-level matching (correct Unicode semantics)
pub trait DictionaryNode: Clone + Send + Sync {
    /// The character unit type for edge labels.
    ///
    /// Use `u8` for byte-level (existing behavior, fastest).
    /// Use `char` for character-level (proper Unicode support).
    type Unit: CharUnit;

    /// Revision-local cursor used by this backend's zero-copy traversal seam.
    ///
    /// Dense immutable arenas use [`DenseSnapshotCursor`]. Backends whose
    /// fastest cursor is a native capability may use a distinct opaque type,
    /// which prevents that capability from being confused with a dense graph
    /// index or exported through an integer ABI.
    type SnapshotCursor: Copy + Send + Sync + 'static;

    /// Backend-native value handle retained by compact snapshot graphs.
    ///
    /// This is deliberately separate from [`SnapshotCursor`](Self::SnapshotCursor):
    /// a native traversal cursor can be a consumer-local pointer capability,
    /// while a graph captured through an ABI retains an unrelated provider
    /// handle for value callbacks.
    type SnapshotGraphValueHandle: Copy + Send + Sync + 'static;

    /// Whether terminal visibility depends on the complete root-relative key.
    ///
    /// Most dictionaries expose every final node and therefore keep the
    /// default `false`. Semantic decorators such as time-to-live views return
    /// `true`, allowing query engines to reconstruct a candidate key only at
    /// accepting terminals instead of paying that cost for every traversal.
    #[inline]
    fn requires_final_units(&self) -> bool {
        false
    }

    /// Decide whether a structurally final node is visible for `units`.
    ///
    /// This method is invoked on the retained traversal owner, with the exact
    /// root-relative unit sequence of an otherwise accepted terminal. A
    /// decorator that returns `true` from [`requires_final_units`](Self::requires_final_units)
    /// must reproduce its dictionary-level visibility semantics here.
    #[inline]
    fn accepts_final_units(&self, units: &[Self::Unit]) -> bool {
        let _ = units;
        true
    }

    /// Return an opaque identity for this physical node within one immutable
    /// dictionary revision.
    ///
    /// The compatibility default disables identity-based sharing. A backend
    /// may return `Some` only when equal identities mean the same physical
    /// node and distinct physical nodes always have distinct identities for
    /// as long as the captured root remains alive. Implementations must be
    /// consistent when enabled: if the captured root returns `Some`, every
    /// reachable node must return a stable identity. A root returning `None`
    /// selects the sequential fallback and descendant identities are ignored.
    /// Snapshot resource arenas use this optional seam to preserve DAWG suffix
    /// sharing instead of publishing a fresh ABI node for every incoming edge.
    #[inline]
    fn snapshot_node_identity(&self) -> Option<SnapshotNodeIdentity> {
        None
    }

    /// Capture this node as the owner of a revision-local traversal cursor.
    ///
    /// The compatibility default disables cursor traversal. Backends may
    /// return a cursor when retaining `self` keeps every node reachable from
    /// that cursor alive and immutable. Query schedulers can then retain this
    /// owner once and enqueue copyable cursors instead of cloning an owned
    /// child handle for every accepted edge.
    #[inline]
    fn snapshot_root_cursor(&self) -> Option<Self::SnapshotCursor> {
        None
    }

    /// Whether obtaining the first snapshot cursor constructs a dense
    /// projection of the complete reachable dictionary revision.
    ///
    /// Query engines use this capability to avoid paying an O(dictionary)
    /// capture cost for every independent lookup. Resource producers that
    /// amortize one immutable projection across many consumers may still call
    /// [`snapshot_root_cursor`](Self::snapshot_root_cursor) deliberately.
    #[inline]
    fn snapshot_cursor_requires_full_projection(&self) -> bool {
        false
    }

    /// Validate an externally supplied cursor against this retained revision.
    ///
    /// Returning `true` certifies that `cursor` may safely be passed to this
    /// node's unsafe snapshot-cursor methods. The conservative default rejects
    /// every cursor: pointer-backed and otherwise provenance-sensitive
    /// backends cannot validate an arbitrary machine word without retaining a
    /// separate membership index. Compact index-backed backends can override
    /// this in O(1), which allows resource ABIs to expose their native cursor
    /// directly without constructing a second traversal arena.
    #[inline]
    fn contains_snapshot_cursor(&self, cursor: Self::SnapshotCursor) -> bool {
        let _ = cursor;
        false
    }

    /// Whether every valid snapshot cursor can be materialized back into one
    /// owned node handle through [`snapshot_cursor_node`](Self::snapshot_cursor_node).
    #[inline]
    fn supports_snapshot_cursor_nodes(&self) -> bool {
        false
    }

    /// Whether a descendant cursor uniquely determines its root-relative key.
    ///
    /// The conservative default is `false`: directed acyclic word graphs may
    /// reach one physical node through multiple keys, and provider-defined
    /// cursors need not encode ancestry. Immutable trie backends may return
    /// `true` when retaining this node fixes both the cursor revision and the
    /// traversal root used by [`snapshot_cursor_key_units`](Self::snapshot_cursor_key_units).
    #[inline]
    fn supports_snapshot_cursor_key_units(&self) -> bool {
        false
    }

    /// Reconstruct the exact edge-label sequence from this captured node to a
    /// provenance-valid descendant cursor.
    ///
    /// Returning a unit vector rather than text preserves byte, Unicode-scalar,
    /// and arbitrary-token semantics. Implementations must return an empty
    /// vector for this node's root cursor and must not include labels above the
    /// captured node when it represents a dictionary subtree.
    ///
    /// # Safety
    ///
    /// `cursor` must obey the retained-revision and ancestry contract of
    /// [`filter_map_snapshot_cursor_edges_and_finality`](Self::filter_map_snapshot_cursor_edges_and_finality).
    #[inline]
    unsafe fn snapshot_cursor_key_units(
        &self,
        cursor: Self::SnapshotCursor,
    ) -> Option<Vec<Self::Unit>> {
        let _ = cursor;
        None
    }

    /// Materialize one owned node handle from a retained snapshot cursor.
    ///
    /// # Safety
    ///
    /// `cursor` must obey the retained-revision and ancestry contract of
    /// [`filter_map_snapshot_cursor_edges_and_finality`](Self::filter_map_snapshot_cursor_edges_and_finality).
    #[inline]
    unsafe fn snapshot_cursor_node(&self, cursor: Self::SnapshotCursor) -> Option<Self>
    where
        Self: Sized,
    {
        let _ = cursor;
        None
    }

    /// Project the outgoing edges of a captured traversal cursor.
    ///
    /// Returns `None` when this backend does not support captured cursor
    /// traversal. Supported backends return `Some(finality)` and invoke
    /// `project` exactly once per edge, creating a child cursor only for an
    /// accepted projection.
    ///
    /// # Safety
    ///
    /// `cursor` must have been returned by [`snapshot_root_cursor`](Self::snapshot_root_cursor)
    /// on this node or supplied to `visitor` by an earlier invocation on the
    /// same retained node. The retained node must outlive the call and every
    /// queued cursor. A cursor must never be mixed with another captured
    /// dictionary revision.
    #[inline]
    unsafe fn filter_map_snapshot_cursor_edges_and_finality<T, P, F>(
        &self,
        cursor: Self::SnapshotCursor,
        _project: P,
        _visitor: F,
    ) -> Option<bool>
    where
        Self: Sized,
        P: FnMut(Self::Unit) -> Option<T>,
        F: FnMut(Self::Unit, Self::SnapshotCursor, T),
    {
        let _ = cursor;
        None
    }

    /// Read finality through a validated snapshot cursor.
    ///
    /// Backends with directly indexed final-state storage should override this
    /// default to avoid enumerating outgoing edges.
    ///
    /// # Safety
    ///
    /// `cursor` must satisfy this node's retained-revision cursor contract.
    #[inline]
    unsafe fn snapshot_cursor_is_final(&self, cursor: Self::SnapshotCursor) -> Option<bool>
    where
        Self: Sized,
    {
        // SAFETY: the caller supplies the cursor contract unchanged.
        unsafe {
            self.filter_map_snapshot_cursor_edges_and_finality(
                cursor,
                |_| None::<()>,
                |_, _, _| unreachable!("a rejected projection cannot be visited"),
            )
        }
    }

    /// Follow one label through a validated snapshot cursor.
    ///
    /// The outer `Option` reports cursor-traversal support; the inner option
    /// reports whether the edge exists. Index-backed dictionaries should
    /// override this default with their direct transition operation.
    ///
    /// # Safety
    ///
    /// `cursor` must satisfy this node's retained-revision cursor contract.
    #[inline]
    unsafe fn snapshot_cursor_transition(
        &self,
        cursor: Self::SnapshotCursor,
        wanted: Self::Unit,
    ) -> Option<Option<Self::SnapshotCursor>>
    where
        Self: Sized,
    {
        let mut child = None;
        // SAFETY: the caller supplies the cursor contract unchanged.
        unsafe {
            self.filter_map_snapshot_cursor_edges_and_finality(
                cursor,
                |label| (label == wanted).then_some(()),
                |_, cursor, ()| child = Some(cursor),
            )?
        };
        Some(child)
    }

    /// Whether [`visit_snapshot_cursor_edge_page`](Self::visit_snapshot_cursor_edge_page)
    /// slices retained edge storage without re-enumerating the complete node.
    ///
    /// The compatibility implementation below scans every edge to answer one
    /// page request, so schedulers must not assume pagination is efficient
    /// unless a backend explicitly advertises it here.
    #[inline]
    fn supports_efficient_snapshot_cursor_edge_paging(&self) -> bool {
        false
    }

    /// Visit one page of edges through a validated snapshot cursor.
    ///
    /// The returned pair is `(is_final, total_edge_count)`. The default keeps
    /// compatibility by enumerating the complete cursor node; compact
    /// backends should override it when they can slice native edge storage and
    /// obtain the total in O(1).
    ///
    /// # Safety
    ///
    /// `cursor` must satisfy this node's retained-revision cursor contract.
    #[inline]
    unsafe fn visit_snapshot_cursor_edge_page<F>(
        &self,
        cursor: Self::SnapshotCursor,
        start: usize,
        capacity: usize,
        mut visitor: F,
    ) -> Option<(bool, usize)>
    where
        Self: Sized,
        F: FnMut(Self::Unit, Self::SnapshotCursor),
    {
        let end = start.saturating_add(capacity);
        let mut total = 0usize;
        // SAFETY: the caller supplies the cursor contract unchanged.
        let is_final = unsafe {
            self.filter_map_snapshot_cursor_edges_and_finality(
                cursor,
                |_| Some(()),
                |label, child, ()| {
                    if total >= start && total < end {
                        visitor(label, child);
                    }
                    total += 1;
                },
            )?
        };
        Some((is_final, total))
    }

    /// Observe one indexed outgoing edge without a callback.
    ///
    /// The outer `Option` reports cursor-capability availability. A successful
    /// observation returns stable node finality, exact total fanout, and
    /// `Some(edge)` exactly when `index < total_edge_count`. The compatibility
    /// implementation refines a capacity-one page and rejects a backend that
    /// invokes the page callback more than once.
    ///
    /// Native immutable backends should override this method with direct
    /// indexing. That removes callback and page-range machinery from explicit
    /// continuation schedulers while retaining the same observable contract.
    ///
    /// # Safety
    ///
    /// `cursor` must satisfy this node's retained-revision cursor contract.
    /// Every returned child cursor belongs to that same retained revision and
    /// may be used only while the producing owner remains alive.
    #[inline]
    unsafe fn snapshot_cursor_edge_at(
        &self,
        cursor: Self::SnapshotCursor,
        index: usize,
    ) -> Option<SnapshotCursorEdgeObservation<Self::Unit, Self::SnapshotCursor>>
    where
        Self: Sized,
    {
        let mut edge = None;
        let mut saw_excess_callback = false;
        // SAFETY: the caller supplies the cursor contract unchanged.
        let (is_final, total_edge_count) = unsafe {
            self.visit_snapshot_cursor_edge_page(cursor, index, 1, |label, child| {
                if edge.is_none() {
                    edge = Some((label, child));
                } else {
                    saw_excess_callback = true;
                }
            })?
        };
        if saw_excess_callback {
            return None;
        }
        Some(SnapshotCursorEdgeObservation::new(
            is_final,
            total_edge_count,
            edge,
        ))
    }

    /// Whether this backend implements native retained edge ranges.
    ///
    /// A backend returning `true` commits callers to the range contract for
    /// every valid cursor in the retained revision. Returning `false` selects
    /// the indexed-page or owned-node compatibility traversal instead.
    #[inline]
    fn supports_efficient_snapshot_cursor_edge_ranges(&self) -> bool {
        false
    }

    /// Begin a closure-free traversal of one immutable node's edge storage.
    ///
    /// A successful observation dereferences the node once, reports stable
    /// finality and exact fanout, returns the first edge directly, and returns
    /// a nonempty token only for the untouched sibling suffix. Backends that
    /// do not advertise native range support may retain this `None` default.
    ///
    /// # Safety
    ///
    /// `cursor` must belong to the exact immutable revision retained by
    /// `self`. Every returned child cursor and range token belongs to that
    /// revision. The first/token shape must agree exactly with the reported
    /// total: neither for zero, first only for one, and both for two or more.
    /// A returned token must satisfy [`SnapshotEdgeRangeToken::from_raw_parts`]
    /// and must remain valid until fully consumed while `self` stays alive.
    #[inline]
    unsafe fn snapshot_cursor_edge_range_start(
        &self,
        cursor: Self::SnapshotCursor,
    ) -> Option<SnapshotEdgeRangeStart<Self::Unit, Self::SnapshotCursor, Self>>
    where
        Self: Sized,
    {
        let _ = cursor;
        None
    }

    /// Consume the first edge of a nonempty retained sibling range.
    ///
    /// Success returns exactly one label/child and `Some(next)` precisely when
    /// another edge remains. The backend must borrow any owning child handle;
    /// stepping must not clone, move, retain, or release it.
    ///
    /// # Safety
    ///
    /// `token` must have been produced by this exact backend, receiver, and
    /// retained revision, or by an earlier successful step from that token.
    /// The producing owner must remain alive. The implementation may read the
    /// current edge and advance by exactly one element only; it must never
    /// dereference the one-past pointer. On failure it must not consume any
    /// externally observable traversal or writer state.
    #[inline]
    unsafe fn snapshot_cursor_edge_range_step(
        &self,
        token: SnapshotEdgeRangeToken<Self>,
    ) -> Option<SnapshotEdgeRangeStep<Self::Unit, Self::SnapshotCursor, Self>>
    where
        Self: Sized,
    {
        let _ = token;
        None
    }

    /// Check if this node marks the end of a valid term
    fn is_final(&self) -> bool;

    /// Transition to a child node via the given character unit
    ///
    /// Returns `None` if no such transition exists
    fn transition(&self, label: Self::Unit) -> Option<Self>;

    /// Iterate over all outgoing edges as (unit, child_node) pairs
    fn edges(&self) -> Box<dyn Iterator<Item = (Self::Unit, Self)> + '_>;

    /// Visit every outgoing edge without prescribing an iterator representation.
    ///
    /// The default preserves compatibility for dictionary implementations that
    /// expose only [`edges`](Self::edges). Backends with borrowed edge storage
    /// can override this monomorphized seam to avoid allocating an intermediate
    /// collection or boxed iterator.
    #[inline]
    fn for_each_edge<F>(&self, mut visitor: F)
    where
        Self: Sized,
        F: FnMut(Self::Unit, Self),
    {
        for (label, child) in self.edges() {
            visitor(label, child);
        }
    }

    /// Read finality and visit outgoing edges as one logical node operation.
    ///
    /// The compatibility default composes the existing methods. Backends that
    /// cross a synchronization or foreign-function boundary can override this
    /// seam to amortize that boundary without changing query algorithms.
    #[inline]
    fn visit_edges_and_finality<F>(&self, visitor: F) -> bool
    where
        Self: Sized,
        F: FnMut(Self::Unit, Self),
    {
        let is_final = self.is_final();
        self.for_each_edge(visitor);
        is_final
    }

    /// Project edge labels before constructing owned child handles.
    ///
    /// `project` is called exactly once for every label. `visitor` is called
    /// exactly once for each label whose projection returns `Some`, and only
    /// those accepted edges need an owned child node. The default preserves
    /// compatibility through [`visit_edges_and_finality`](Self::visit_edges_and_finality).
    /// Backends with borrowed edge storage should override this method so a
    /// rejected edge never clones, allocates, faults, or reference-counts its
    /// child handle.
    #[inline]
    fn filter_map_edges<T, P, F>(&self, mut project: P, mut visitor: F)
    where
        Self: Sized,
        P: FnMut(Self::Unit) -> Option<T>,
        F: FnMut(Self::Unit, Self, T),
    {
        self.for_each_edge(|label, child| {
            if let Some(projected) = project(label) {
                visitor(label, child, projected);
            }
        });
    }

    /// Read finality and project labels before constructing accepted children.
    ///
    /// Backends that can observe finality and borrowed edges in one operation
    /// may override this fused form. The compatibility default composes
    /// [`is_final`](Self::is_final) and [`filter_map_edges`](Self::filter_map_edges).
    #[inline]
    fn filter_map_edges_and_finality<T, P, F>(&self, project: P, visitor: F) -> bool
    where
        Self: Sized,
        P: FnMut(Self::Unit) -> Option<T>,
        F: FnMut(Self::Unit, Self, T),
    {
        let is_final = self.is_final();
        self.filter_map_edges(project, visitor);
        is_final
    }

    /// Whether [`visit_edge_page_and_finality`](Self::visit_edge_page_and_finality)
    /// can address one bounded edge window without first materializing or
    /// re-enumerating the complete node.
    ///
    /// The compatibility implementation below performs a complete visit for
    /// every requested page. Consumers must therefore retain their eager
    /// fallback unless a node explicitly advertises efficient paging here.
    #[inline]
    fn supports_efficient_edge_paging(&self) -> bool {
        false
    }

    /// Visit one bounded page of owned child nodes and return
    /// `(is_final, total_edge_count)`.
    ///
    /// Edges must retain the same order as [`for_each_edge`](Self::for_each_edge).
    /// `capacity == 0` is a metadata-only request and must not construct a
    /// child node. Implementations that advertise efficient paging must keep
    /// work and temporary storage proportional to `capacity`, not total
    /// out-degree.
    ///
    /// The default is exact but deliberately not advertised as efficient: it
    /// visits the complete node and forwards only the requested window.
    #[inline]
    fn visit_edge_page_and_finality<F>(
        &self,
        start: usize,
        capacity: usize,
        mut visitor: F,
    ) -> (bool, usize)
    where
        Self: Sized,
        F: FnMut(Self::Unit, Self),
    {
        let end = start.saturating_add(capacity);
        let mut total = 0usize;
        let is_final = self.visit_edges_and_finality(|label, child| {
            if total >= start && total < end {
                visitor(label, child);
            }
            total = total
                .checked_add(1)
                .expect("a dictionary node's out-degree fits in usize");
        });
        (is_final, total)
    }

    /// Visit one bounded page after finality has already been captured.
    ///
    /// The return value is the stable total edge count for this retained node
    /// revision. The default delegates to
    /// [`visit_edge_page_and_finality`](Self::visit_edge_page_and_finality).
    /// Providers whose finality and edge callbacks are independent should
    /// override this seam so page refills do not re-read finality.
    #[inline]
    fn visit_edge_page<F>(&self, start: usize, capacity: usize, visitor: F) -> usize
    where
        Self: Sized,
        F: FnMut(Self::Unit, Self),
    {
        self.visit_edge_page_and_finality(start, capacity, visitor)
            .1
    }

    /// Check if a specific edge exists
    fn has_edge(&self, label: Self::Unit) -> bool {
        self.transition(label).is_some()
    }

    /// Get the number of outgoing edges (if efficiently available)
    fn edge_count(&self) -> Option<usize> {
        None
    }
}

/// Opaque, non-zero identity of a physical node within one captured revision.
///
/// This is not a persistent identifier and must never be dereferenced or
/// compared across independently captured dictionary revisions.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SnapshotNodeIdentity(std::num::NonZeroU64);

impl SnapshotNodeIdentity {
    /// Construct an identity from a backend-proven non-zero token.
    pub const fn new(value: u64) -> Option<Self> {
        match std::num::NonZeroU64::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    /// Construct an identity from a zero-based immutable node index.
    pub fn from_index(index: usize) -> Option<Self> {
        let value = u64::try_from(index).ok()?.checked_add(1)?;
        Self::new(value)
    }

    /// Return the opaque non-zero token.
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// One-based cursor into a validated dense immutable arena.
///
/// The representation contains only an integer index. It cannot carry a native
/// pointer and therefore cannot lose, forge, or expose pointer provenance.
/// Backends with provenance-bearing cursors use a different associated cursor
/// type through [`DictionaryNode::SnapshotCursor`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct DenseSnapshotCursor(std::num::NonZeroUsize);

impl DenseSnapshotCursor {
    /// Construct a cursor from its public one-based representation.
    #[inline]
    pub const fn try_from_one_based(value: usize) -> Option<Self> {
        match std::num::NonZeroUsize::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    /// Construct a cursor from a zero-based immutable-arena index.
    #[inline]
    pub fn from_index(index: usize) -> Option<Self> {
        Self::try_from_one_based(index.checked_add(1)?)
    }

    /// Return the zero-based immutable-arena index.
    #[inline]
    pub const fn index(self) -> usize {
        self.0.get() - 1
    }

    /// Return the public one-based representation.
    #[inline]
    pub const fn one_based(self) -> usize {
        self.0.get()
    }

    /// Compatibility constructor for the one-based representation.
    #[inline]
    pub const fn new(value: usize) -> Option<Self> {
        Self::try_from_one_based(value)
    }

    /// Compatibility accessor for the one-based representation.
    #[inline]
    pub const fn get(self) -> usize {
        self.one_based()
    }
}

/// Backward-compatible name for the dense snapshot cursor.
pub type SnapshotTraversalCursor = DenseSnapshotCursor;

#[cfg(test)]
mod snapshot_traversal_cursor_tests {
    use super::{DenseSnapshotCursor, SnapshotTraversalCursor};

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn dense_cursor_is_one_word_and_round_trips_both_index_forms() {
        assert_eq!(
            std::mem::size_of::<SnapshotTraversalCursor>(),
            std::mem::size_of::<usize>()
        );
        assert_send_sync::<SnapshotTraversalCursor>();

        let dense = SnapshotTraversalCursor::new(17).expect("small dense cursor");
        assert_eq!(dense.get(), 17);
        assert_eq!(dense.index(), 16);
        assert_eq!(DenseSnapshotCursor::from_index(16), Some(dense));
        assert!(SnapshotTraversalCursor::new(0).is_none());
        assert!(DenseSnapshotCursor::from_index(usize::MAX).is_none());
    }
}

/// Collect owned child handles through the monomorphized visitation seam.
///
/// Some stack-based serializers and foreign-resource arenas must retain child
/// nodes after the parent borrow ends. They still require one owned `Vec`, but
/// this helper avoids layering the compatibility `Box<dyn Iterator>` and any
/// backend-specific intermediate collection underneath it.
#[inline]
#[cfg(any(
    feature = "bindings-core",
    feature = "protobuf",
    feature = "serialization",
    feature = "persistent-artrie"
))]
pub(crate) fn collect_node_edges<N: DictionaryNode>(node: &N) -> Vec<(N::Unit, N)> {
    let mut edges = Vec::with_capacity(node.edge_count().unwrap_or(0));
    node.for_each_edge(|label, child| edges.push((label, child)));
    edges
}

/// Extension trait for dictionaries that map terms to values.
///
/// This trait enables "fuzzy maps" - dictionaries that associate arbitrary values
/// with terms, allowing efficient filtered queries based on those values. This is
/// particularly useful for contextual code completion where terms are mapped to
/// scope IDs, categories, or other metadata.
pub trait MappedDictionary: Dictionary {
    /// The type of values associated with dictionary terms
    type Value: DictionaryValue;

    /// Get the value associated with a term.
    ///
    /// Returns `None` if the term doesn't exist in the dictionary.
    ///
    /// This is a required method. The previous default returned `None` for
    /// every term while pretending to be a real implementation, which silently
    /// broke any user expecting `MappedDictionary` semantics. Every backend in
    /// this crate now provides an explicit override.
    fn get_value(&self, term: &str) -> Option<Self::Value>;

    /// Check if a term exists and its value matches a predicate
    ///
    /// This is more efficient than `get_value` + predicate test, as it can
    /// short-circuit early if the term doesn't exist.
    fn contains_with_value<F>(&self, term: &str, predicate: F) -> bool
    where
        F: Fn(&Self::Value) -> bool,
    {
        self.get_value(term).is_some_and(|v| predicate(&v))
    }
}

/// Extension trait for dictionary nodes that provide access to values.
///
/// This trait allows nodes to expose values during graph traversal, enabling
/// efficient filtering at query time without materializing all results first.
pub trait MappedDictionaryNode: DictionaryNode {
    /// The type of values associated with terms at this node
    type Value: DictionaryValue;

    /// Get the value at this node if it's a final node
    ///
    /// Returns `None` if this is not a final node, or if no value is associated.
    fn value(&self) -> Option<Self::Value>;

    /// Get the value when the caller has already established finality.
    ///
    /// The default preserves existing semantics. Boundary-backed nodes can
    /// override it to avoid repeating an expensive finality callback.
    fn value_at_final(&self) -> Option<Self::Value> {
        self.value()
    }

    /// Resolve a value after finality and root-relative key visibility have
    /// been established.
    ///
    /// Semantic decorators can use `units` to reproduce dictionary-level
    /// value behavior without disabling compact cursor traversal. The default
    /// is exactly the legacy node-local value operation.
    #[inline]
    fn value_at_final_with_units(&self, units: &[Self::Unit]) -> Option<Self::Value> {
        let _ = units;
        self.value_at_final()
    }

    /// Whether this retained node supports value reads through every snapshot
    /// cursor reachable from [`DictionaryNode::snapshot_root_cursor`].
    ///
    /// This capability is separate from [`snapshot_cursor_value`](Self::snapshot_cursor_value)
    /// so query construction never clones an empty-term value merely to detect
    /// backend support.
    #[inline]
    fn supports_snapshot_cursor_values(&self) -> bool {
        false
    }

    /// Whether this retained node can resolve the backend value cursors stored
    /// in a compact [`SnapshotTraversalGraph`].
    #[inline]
    fn supports_snapshot_graph_values(&self) -> bool {
        false
    }

    /// Build the compact traversal projection for this retained revision.
    ///
    /// Resource producers call this lazily after the O(1) snapshot boundary,
    /// so graph projection never extends a backend publication/read lock.
    /// Backends that return `Some` must keep every embedded value cursor valid
    /// through this retained node owner.
    #[inline]
    fn snapshot_traversal_graph(
        &self,
    ) -> Option<std::sync::Arc<SnapshotTraversalGraph<Self::Unit, Self::SnapshotGraphValueHandle>>>
    {
        None
    }

    /// Read the value at a captured revision-local traversal cursor.
    ///
    /// The outer `Option` reports whether this backend supports value access
    /// through snapshot cursors. The inner `Option` is the dictionary value,
    /// which may be absent even at a final node for value-optional backends.
    ///
    /// # Safety
    ///
    /// `cursor` must obey the same retained-revision and ancestry contract as
    /// [`DictionaryNode::filter_map_snapshot_cursor_edges_and_finality`].
    #[inline]
    unsafe fn snapshot_cursor_value(
        &self,
        cursor: Self::SnapshotCursor,
    ) -> Option<Option<Self::Value>> {
        let _ = cursor;
        None
    }

    /// Read a value at a captured cursor with the accepted root-relative key.
    ///
    /// # Safety
    ///
    /// `cursor` obeys the contract of
    /// [`snapshot_cursor_value`](Self::snapshot_cursor_value), and `units`
    /// names that cursor from the retained root.
    #[inline]
    unsafe fn snapshot_cursor_value_with_units(
        &self,
        cursor: Self::SnapshotCursor,
        units: &[Self::Unit],
    ) -> Option<Option<Self::Value>> {
        let _ = units;
        // SAFETY: forwarded under the identical cursor contract.
        unsafe { self.snapshot_cursor_value(cursor) }
    }

    /// Read the value at a dense compact-graph cursor.
    ///
    /// The graph translates its dense cursor to the backend-native value
    /// cursor captured from the same immutable revision. The outer `Option`
    /// reports capability support and the inner `Option` is the node value.
    ///
    /// # Safety
    ///
    /// `graph` must have been captured with this exact retained owner and
    /// `cursor` must belong to that graph.
    #[inline]
    unsafe fn snapshot_graph_cursor_value(
        &self,
        graph: &SnapshotTraversalGraph<Self::Unit, Self::SnapshotGraphValueHandle>,
        cursor: SnapshotTraversalCursor,
    ) -> Option<Option<Self::Value>> {
        let _ = (graph, cursor);
        None
    }

    /// Read a compact-graph cursor value with the accepted root-relative key.
    ///
    /// # Safety
    ///
    /// `graph` and `cursor` obey the contract of
    /// [`snapshot_graph_cursor_value`](Self::snapshot_graph_cursor_value), and
    /// `units` names that cursor from the graph root.
    #[inline]
    unsafe fn snapshot_graph_cursor_value_with_units(
        &self,
        graph: &SnapshotTraversalGraph<Self::Unit, Self::SnapshotGraphValueHandle>,
        cursor: SnapshotTraversalCursor,
        units: &[Self::Unit],
    ) -> Option<Option<Self::Value>> {
        let _ = units;
        // SAFETY: forwarded under the identical graph/cursor contract.
        unsafe { self.snapshot_graph_cursor_value(graph, cursor) }
    }
}

/// Trait for dictionaries supporting set-like term insertion and removal.
///
/// This trait extends [`Dictionary`] with mutation capabilities. It is the
/// set-like interface — `insert(&str)` adds a term, `remove(&str)` removes
/// one, no values involved. For dictionaries that carry values along with
/// terms, see [`MutableMappedDictionary`].
///
/// # Overlap with `MutableMappedDictionary`
///
/// `MutableMappedDictionary` covers most write-with-value operations
/// (`insert_with_value`, `update_or_insert`, `union_with`, …) but
/// **deliberately omits** `remove` and the value-free `insert`. The two
/// traits are complementary, not redundant:
///
/// - Dictionaries that are set-like only (or have `Value = ()`): impl
///   [`MutableDictionary`].
/// - Dictionaries that carry meaningful values: impl
///   [`MutableMappedDictionary`] for value-aware writes and (if removal is
///   supported) [`MutableDictionary`] for set-like removal.
///
/// Several backends in this crate (`DynamicDawg`, `DynamicDawgChar`,
/// `DynamicDawgU64`) implement both.
///
/// # Default Implementations
///
/// The trait provides default implementations for batch operations
/// (`extend`, `remove_many`) built on top of the required `insert` and
/// `remove` methods.
pub trait MutableDictionary: Dictionary {
    /// Insert a term into the dictionary.
    ///
    /// Returns `true` if the term was newly inserted, `false` if it already existed.
    fn insert(&self, term: &str) -> bool;

    /// Remove a term from the dictionary.
    ///
    /// Returns `true` if the term was present and removed, `false` otherwise.
    fn remove(&self, term: &str) -> bool;

    /// Batch insert multiple terms.
    ///
    /// Returns the number of new terms added (not counting duplicates).
    ///
    /// The default implementation calls `insert` for each term. Implementations
    /// may override this for better performance.
    fn extend<I, S>(&self, terms: I) -> usize
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        terms
            .into_iter()
            .filter(|term| self.insert(term.as_ref()))
            .count()
    }

    /// Batch remove multiple terms.
    ///
    /// Returns the number of terms removed.
    ///
    /// The default implementation calls `remove` for each term. Implementations
    /// may override this for better performance.
    fn remove_many<I, S>(&self, terms: I) -> usize
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        terms
            .into_iter()
            .filter(|term| self.remove(term.as_ref()))
            .count()
    }
}

/// Trait for dictionaries supporting compaction and minimization.
///
/// Dictionaries that support dynamic modifications (insertions and deletions)
/// may accumulate internal fragmentation or redundant structure over time.
/// This trait provides methods to restore optimal structure.
///
/// # Compaction vs Minimization
///
/// - **`compact()`**: Full rebuild - extracts all terms, sorts them, and reconstructs
///   the structure from scratch. Achieves perfect minimality but is O(n log n + m).
///
/// - **`minimize()`**: Incremental optimization - merges equivalent nodes without
///   full rebuild. Faster for localized changes but may not achieve perfect minimality.
pub trait CompactableDictionary: MutableDictionary {
    /// Check if compaction would be beneficial.
    ///
    /// Returns `true` if deletions have occurred or the structure has degraded
    /// significantly from optimal.
    fn needs_compaction(&self) -> bool;

    /// Compact the dictionary to restore optimal structure.
    ///
    /// This performs a full rebuild, extracting all terms, sorting them for
    /// optimal prefix sharing, and reconstructing the dictionary.
    ///
    /// Returns the number of nodes/elements removed or optimized away.
    fn compact(&self) -> usize;

    /// Minimize the dictionary using incremental optimization.
    ///
    /// Unlike `compact()`, this method:
    /// - Makes no assumptions about insertion order
    /// - Only examines affected nodes and their neighbors
    /// - Preserves existing structure where possible
    /// - Is faster than `compact()` for localized updates
    ///
    /// Returns the number of nodes merged.
    ///
    /// The default implementation delegates to `compact()`. Dictionaries
    /// with more efficient incremental algorithms should override this.
    fn minimize(&self) -> usize {
        self.compact()
    }
}

/// Extension trait for dictionaries that support inserting values.
///
/// This trait enables mutation of mapped dictionaries, allowing terms to be
/// added or updated with associated values.
pub trait MutableMappedDictionary: MappedDictionary {
    /// Insert or update a term with an associated value.
    ///
    /// # Arguments
    ///
    /// * `term` - The term to insert
    /// * `value` - The value to associate with the term
    ///
    /// # Returns
    ///
    /// `true` if this is a new term, `false` if updating an existing term's value.
    fn insert_with_value(&self, term: &str, value: Self::Value) -> bool;

    /// Union this dictionary with another, applying a merge function for conflicting values.
    ///
    /// Iterates through all terms in `other` and:
    /// - Inserts new terms directly
    /// - For existing terms, merges values using `merge_fn`
    ///
    /// # Arguments
    ///
    /// * `other` - The dictionary to union with
    /// * `merge_fn` - Function to merge values when term exists in both dictionaries.
    ///   Takes `(existing_value, other_value)` and returns the merged value.
    ///
    /// # Returns
    ///
    /// Number of terms processed from `other`
    fn union_with<F>(&self, other: &Self, merge_fn: F) -> usize
    where
        F: Fn(&Self::Value, &Self::Value) -> Self::Value,
        Self::Value: Clone;

    /// Union with another dictionary, keeping the right (other's) value on conflicts.
    ///
    /// Convenience method equivalent to `union_with(other, |_, right| right.clone())`.
    fn union_replace(&self, other: &Self) -> usize
    where
        Self::Value: Clone,
    {
        self.union_with(other, |_, right| right.clone())
    }

    /// Update an existing term's value in place, or insert a new term with a default value.
    ///
    /// This method is useful when you want to incrementally modify a value (e.g., adding
    /// elements to a `HashSet` or `Vec`) without replacing it entirely.
    ///
    /// # Arguments
    ///
    /// * `term` - The term to update or insert
    /// * `default_value` - The value to use if the term doesn't exist
    /// * `update_fn` - Retry-safe function to apply to the existing value if the
    ///   term exists. Implementations with lock-free publication may invoke this
    ///   function more than once after CAS conflicts.
    ///
    /// # Returns
    ///
    /// `true` if this was a new term (inserted with default), `false` if an existing term was updated.
    fn update_or_insert<F>(&self, term: &str, default_value: Self::Value, update_fn: F) -> bool
    where
        F: Fn(&mut Self::Value);
}

/// Prelude module for convenient imports.
pub mod prelude {
    pub use crate::factory::{Uleb128Backend, Uleb128DictionaryContainer};
    pub use crate::{
        BijectiveDictionary, BijectiveMap, CharUnit, CompactableDictionary, DictZipper, Dictionary,
        DictionaryEntries, DictionaryEntriesIter, DictionaryEntry, DictionaryKeys,
        DictionaryLanguageEntries, DictionaryLanguageTerms, DictionaryNode, DictionaryTerms,
        DictionaryValue, DictionaryValues, ExactSnapshotEntryIterator, InsertError,
        InternedEntries, InternedId, InternedIdDictionaryView, InternedSequence,
        InternedSequenceDictionary, InternedSequenceDictionaryU64, InternedUlebSequenceDictionary,
        InternedUlebSequenceDictionaryU64, InternedVocabulary, MappedDictionary,
        MappedDictionaryNode, MutableDictionary, MutableMappedDictionary, SnapshotEntryIterator,
        SnapshotTermIterator, SyncStrategy, Uleb128Atom, Utf8, ValuedDictZipper,
        ValuedZipperCollection, ZipperCollection, ZipperEntryIterator, ZipperTermIterator,
    };

    // Re-export common dictionary types
    pub use crate::double_array_trie::{
        DoubleArrayTrie, DoubleArrayTrieChar, DoubleArrayTrieUleb128, DoubleArrayTrieUtf8,
    };
    pub use crate::dynamic_dawg::{
        DynamicDawg, DynamicDawgChar, DynamicDawgU64, DynamicDawgUleb128, DynamicDawgUtf8,
    };
    #[cfg(feature = "pathmap-backend")]
    pub use crate::pathmap::{PathMapDictionaryUleb128, PathMapDictionaryUtf8};
    pub use crate::scdawg::{Scdawg, ScdawgChar, ScdawgUtf8};
    pub use crate::suffix_automaton::{SuffixAutomaton, SuffixAutomatonChar, SuffixAutomatonUtf8};

    #[cfg(feature = "persistent-artrie")]
    pub use crate::persistent_artrie::{
        PersistentARTrieU64, PersistentARTrieUleb128, PersistentARTrieUtf8, PersistentScdawg,
        PersistentScdawgChar, PersistentScdawgUtf8, PersistentSuffixAutomaton,
        PersistentSuffixAutomatonChar, PersistentSuffixAutomatonUtf8, PersistentSuffixTree,
        PersistentSuffixTreeChar, PersistentSuffixTreeUtf8,
    };
}
