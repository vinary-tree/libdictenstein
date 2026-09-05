//! Stable binding resources owned by libdictenstein.
//!
//! This module is the producer half of the `vt.dictionary.v1` contract.
//! Concrete dictionaries and their CRUD APIs remain in this crate while
//! consumers retain a small project-neutral resource. Capturing a query
//! revision clones an immutable root in O(1). Backends with stable physical
//! node identity preserve graph sharing in a lock-free lazy arena; other
//! backends retain the sequential ABI-local identifier fallback.

mod entries;

use crate::concurrent_slots::HybridOnceBoxSlots;
use crate::double_array_trie::char::DoubleArrayTrieChar;
use crate::double_array_trie::DoubleArrayTrie;
use crate::dynamic_dawg::char::{DynamicDawgChar, DynamicDawgCharNode};
use crate::dynamic_dawg::lockfree::PublishIfEmpty;
use crate::dynamic_dawg::u64::{DynamicDawgU64, DynamicDawgU64Node};
use crate::dynamic_dawg::{DynamicDawg, DynamicDawgNode};
use crate::scdawg::char::ScdawgChar;
use crate::scdawg::Scdawg;
use crate::{
    Dictionary, DictionaryNode, MappedDictionaryNode, SnapshotNodeIdentity,
    SnapshotTraversalCursor, SnapshotTraversalGraph, SnapshotTraversalProjection,
};
use arc_swap::ArcSwapOption;
use llattice::Lattice;
use std::ffi::c_void;
#[cfg(feature = "persistent-artrie")]
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(feature = "perf-instrumentation")]
use std::sync::Mutex;
use std::sync::{Arc, OnceLock};
use vinary_tree_interop::{
    dictionary_flags, VtDictionaryEdge, VtDictionaryGraphEdge, VtDictionaryGraphNode,
    VtDictionaryGraphVTable, VtDictionaryGraphView, VtDictionaryVTable, VtDictionaryVisitVTable,
    VtInterfaceId, VtOptionalU64, VtResource, VtResourceVTable, VtSnapshotIdentity,
    VtSnapshotIdentityVTable, VtStatus, VtUnitDomain, VtValueDomain, VT_ABI_VERSION,
    VT_DICTIONARY_ENTRIES_INTERFACE_ID, VT_DICTIONARY_ENTRIES_INTERFACE_VERSION,
    VT_DICTIONARY_GRAPH_INTERFACE_ID, VT_DICTIONARY_GRAPH_INTERFACE_VERSION,
    VT_DICTIONARY_INTERFACE_ID, VT_DICTIONARY_INTERFACE_VERSION, VT_DICTIONARY_VISIT_INTERFACE_ID,
    VT_DICTIONARY_VISIT_INTERFACE_VERSION, VT_SNAPSHOT_IDENTITY_INTERFACE_ID,
    VT_SNAPSHOT_IDENTITY_INTERFACE_VERSION,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[cfg_attr(
    feature = "persistent-artrie",
    derive(serde::Serialize, serde::Deserialize)
)]
struct BindingValue {
    value: u64,
    has_value: bool,
}

impl crate::DictionaryValue for BindingValue {}

impl BindingValue {
    fn present(value: u64) -> Self {
        Self {
            value,
            has_value: true,
        }
    }

    fn from_option(value: Option<u64>) -> Self {
        Self {
            value: value.unwrap_or_default(),
            has_value: value.is_some(),
        }
    }

    fn into_option(self) -> Option<u64> {
        self.has_value.then_some(self.value)
    }
}

/// Concrete unit domain selected for a binding-owned dictionary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
#[repr(u32)]
pub enum BindingUnitDomain {
    /// Arbitrary byte sequences.
    Byte = 1,
    /// UTF-8 strings traversed as Unicode scalar values.
    UnicodeScalar = 2,
    /// Arbitrary unsigned 64-bit token sequences.
    U64 = 3,
}

/// Canonical profile metadata associated with a binding unit domain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BindingProfileDescriptor {
    /// Built-in logical profile kind.
    pub kind: crate::ProfileKind,
    /// Canonical profile name and version.
    pub identity: crate::VariableWidthProfile,
    /// Fixed width in bytes, or `None` for variable-width profiles.
    pub width_bytes: Option<usize>,
}

impl BindingUnitDomain {
    /// Stable ABI-independent domain identifier.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Byte => "bytes",
            Self::UnicodeScalar => "unicode-scalar",
            Self::U64 => "u64",
        }
    }

    /// Map a logical profile to an ABI domain when the binding supports it.
    ///
    /// Variable-width UTF-8 and ULEB128 profiles intentionally return `None`:
    /// the current ABI has no lossless term variant for those logical atoms.
    pub const fn from_profile_kind(kind: crate::ProfileKind) -> Option<Self> {
        match kind {
            crate::ProfileKind::Bytes => Some(Self::Byte),
            crate::ProfileKind::UnicodeScalar => Some(Self::UnicodeScalar),
            crate::ProfileKind::U64 => Some(Self::U64),
            crate::ProfileKind::Utf8
            | crate::ProfileKind::U32
            | crate::ProfileKind::F64Bits
            | crate::ProfileKind::Uleb128 => None,
        }
    }

    /// Return stable profile metadata without relying on ABI or Rust names.
    pub const fn profile_descriptor(self) -> BindingProfileDescriptor {
        BindingProfileDescriptor::for_kind(match self {
            Self::Byte => crate::ProfileKind::Bytes,
            Self::UnicodeScalar => crate::ProfileKind::UnicodeScalar,
            Self::U64 => crate::ProfileKind::U64,
        })
    }
}

impl BindingProfileDescriptor {
    /// Construct canonical metadata for any built-in logical profile.
    pub const fn for_kind(kind: crate::ProfileKind) -> Self {
        Self {
            kind,
            identity: kind.identity(),
            width_bytes: kind.width_bytes(),
        }
    }

    /// Construct canonical metadata from a compile-time atom profile.
    pub const fn for_profile<P: crate::AtomProfile>() -> Self {
        Self::for_kind(P::KIND)
    }

    /// Return the ABI domain when this profile can be represented losslessly.
    pub const fn binding_domain(self) -> Option<BindingUnitDomain> {
        BindingUnitDomain::from_profile_kind(self.kind)
    }
}

/// One owned term emitted by a binding snapshot traversal.
///
/// The variants preserve arbitrary byte and `u64` keys without coercing them
/// through UTF-8. Unicode keys are validated scalar strings.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum BindingTerm {
    /// Arbitrary bytes.
    Bytes(Vec<u8>),
    /// A Unicode scalar string.
    Unicode(String),
    /// Unsigned 64-bit tokens.
    U64(Vec<u64>),
}

/// One lossless, owned dictionary record from an immutable revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BindingEntry {
    /// Exact key in its native unit domain.
    pub term: BindingTerm,
    /// Mapped value, or `None` for a present term-only record.
    pub value: Option<u64>,
}

/// Snapshot-owning, iterative binding traversal.
///
/// This direct Rust adapter shares the same engine as the batched family ABI
/// but avoids FFI for in-process runtimes such as browser WebAssembly.
pub struct BindingEntries {
    state: entries::EntryCursorState,
    domain: VtUnitDomain,
    remaining: Option<usize>,
    ended: bool,
}

impl BindingEntries {
    fn new(snapshot: Arc<dyn SnapshotOps>) -> Self {
        let domain = snapshot.domain();
        let remaining = snapshot.len();
        Self {
            state: entries::EntryCursorState::new(snapshot),
            domain,
            remaining,
            ended: false,
        }
    }

    fn decode(&self, entry: entries::PendingEntry) -> Result<BindingEntry, VtStatus> {
        let term = match self.domain {
            VtUnitDomain::Byte => BindingTerm::Bytes(
                entry
                    .units
                    .into_iter()
                    .map(|unit| u8::try_from(unit).map_err(|_| VtStatus::ProviderError))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            VtUnitDomain::UnicodeScalar => BindingTerm::Unicode(
                entry
                    .units
                    .into_iter()
                    .map(|unit| {
                        u32::try_from(unit)
                            .ok()
                            .and_then(char::from_u32)
                            .ok_or(VtStatus::ProviderError)
                    })
                    .collect::<Result<String, _>>()?,
            ),
            VtUnitDomain::U64 => BindingTerm::U64(entry.units),
        };
        Ok(BindingEntry {
            term,
            value: entry.value,
        })
    }

    /// Return the immutable term domain captured by this snapshot traversal.
    pub fn domain(&self) -> BindingUnitDomain {
        match self.domain {
            VtUnitDomain::Byte => BindingUnitDomain::Byte,
            VtUnitDomain::UnicodeScalar => BindingUnitDomain::UnicodeScalar,
            VtUnitDomain::U64 => BindingUnitDomain::U64,
        }
    }

    /// Return canonical profile metadata for this immutable traversal.
    #[inline]
    pub fn profile_descriptor(&self) -> BindingProfileDescriptor {
        self.domain().profile_descriptor()
    }
}

impl Iterator for BindingEntries {
    type Item = Result<BindingEntry, VtStatus>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.ended {
            return None;
        }
        match self.state.next_entry() {
            Ok(Some(entry)) => {
                if let Some(remaining) = &mut self.remaining {
                    *remaining = remaining.saturating_sub(1);
                }
                Some(self.decode(entry))
            }
            Ok(None) => {
                self.ended = true;
                self.remaining = Some(0);
                None
            }
            Err(error) => {
                self.ended = true;
                self.remaining = Some(0);
                Some(Err(error))
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.remaining
            .map_or((0, None), |remaining| (remaining, Some(remaining)))
    }
}

impl std::iter::FusedIterator for BindingEntries {}

impl From<BindingUnitDomain> for VtUnitDomain {
    fn from(value: BindingUnitDomain) -> Self {
        match value {
            BindingUnitDomain::Byte => Self::Byte,
            BindingUnitDomain::UnicodeScalar => Self::UnicodeScalar,
            BindingUnitDomain::U64 => Self::U64,
        }
    }
}

/// Error returned by the safe binding-owned dictionary API.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BindingError {
    /// An operation used a term representation incompatible with the dictionary.
    DomainMismatch,
    /// A Unicode dictionary received malformed UTF-8.
    InvalidUtf8,
    /// A requested backend operation is not available for this dictionary kind.
    Unsupported,
    /// A persistent dictionary operation failed.
    Io(String),
}

impl std::fmt::Display for BindingError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DomainMismatch => formatter.write_str("dictionary unit domain mismatch"),
            Self::InvalidUtf8 => formatter.write_str("dictionary term is not valid UTF-8"),
            Self::Unsupported => formatter.write_str("dictionary operation is unsupported"),
            Self::Io(message) => write!(
                formatter,
                "persistent dictionary operation failed: {message}"
            ),
        }
    }
}

impl std::error::Error for BindingError {}

/// Exact-key set operation over two immutable dictionary revisions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum BindingAlgebraOperation {
    /// Keep keys present in either dictionary.
    Union = 1,
    /// Keep keys present in both dictionaries.
    Intersection = 2,
    /// Keep keys present in the left dictionary but not the right dictionary.
    Difference = 3,
    /// Keep keys present in exactly one dictionary.
    SymmetricDifference = 4,
}

/// Conflict policy for a key present in both input dictionaries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum BindingValueMerge {
    /// Preserve the left value.
    First = 1,
    /// Preserve the right value.
    Last = 2,
    /// Use the `Option<u64>` lattice join (optional maximum).
    LatticeJoin = 3,
    /// Use the `Option<u64>` lattice meet (shared optional minimum).
    LatticeMeet = 4,
}

/// Failure while combining two immutable dictionary revisions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BindingAlgebraError {
    /// The two dictionaries use different term domains.
    DomainMismatch,
    /// A dictionary provider rejected snapshot traversal.
    Provider(VtStatus),
}

impl std::fmt::Display for BindingAlgebraError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DomainMismatch => formatter.write_str("dictionary unit domain mismatch"),
            Self::Provider(status) => {
                write!(formatter, "dictionary snapshot provider failed: {status:?}")
            }
        }
    }
}

impl std::error::Error for BindingAlgebraError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SnapshotIdentity {
    producer: u64,
    revision: u64,
}

static NEXT_SNAPSHOT_PRODUCER: AtomicU64 = AtomicU64::new(1);

struct CachedSnapshot {
    revision: u64,
    generation: u64,
    snapshot: OnceLock<Arc<dyn SnapshotOps>>,
}

const MAX_SNAPSHOT_BACKOFF_CREDITS: u64 = 64;
const SNAPSHOT_BACKOFF_SPINS_PER_CREDIT: u64 = 16;
const COLD_INITIALIZER_WAIT_ATTEMPTS: usize = 256;

/// One strong warmed snapshot per shared producer revision.
///
/// This is a seqlock-style protocol over heterogeneous immutable backends.
/// Writers announce themselves before changing backend state and publish a
/// new revision before withdrawing that announcement. Snapshotters capture
/// optimistically, then validate the active-writer count *before* the
/// revision. If the count observes the final writer withdrawal, its Acquire
/// pairs with that AcqRel RMW and the following revision load must observe the
/// preceding revision publication. If it observes zero before a writer enters,
/// the capture can linearize before that writer.
///
/// Snapshotters are obstruction-free rather than wait-free: uninterrupted
/// mutation can invalidate every capture because backend root/count and this
/// memo revision are not one atomic descriptor. The protocol is nevertheless
/// lock-free system-wide. No snapshotter owns an admission gate, a suspended
/// or panicking snapshotter cannot stop writers. A cold publisher installs an
/// empty per-revision generation before construction; ordinary contenders
/// poll its `OnceLock` without entering a blocking initializer. After a
/// bounded stall they may replace that exact generation by CAS and initialize
/// the successor, so normal capture is single-flight while abandoned work is
/// helpably superseded.
struct SnapshotMemo {
    producer: u64,
    revision: AtomicU64,
    active_writers: AtomicU64,
    /// Consumable advisory pressure from invalidated snapshot attempts.
    /// Writers atomically take these credits and perform a bounded pause; no
    /// reader owns a state that a writer must wait to observe or clear.
    snapshot_backoff_credits: AtomicU64,
    cached: ArcSwapOption<CachedSnapshot>,
    #[cfg(feature = "perf-instrumentation")]
    legacy_control: Mutex<()>,
}

struct SnapshotMutation<'a> {
    memo: &'a SnapshotMemo,
    dirty: bool,
}

impl SnapshotMemo {
    fn new() -> Self {
        let producer = NEXT_SNAPSHOT_PRODUCER
            .try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_add(1)
            })
            .expect("snapshot producer identity space exhausted");
        Self {
            producer,
            revision: AtomicU64::new(0),
            active_writers: AtomicU64::new(0),
            snapshot_backoff_credits: AtomicU64::new(0),
            cached: ArcSwapOption::empty(),
            #[cfg(feature = "perf-instrumentation")]
            legacy_control: Mutex::new(()),
        }
    }

    fn begin_mutation(&self) -> SnapshotMutation<'_> {
        // This is bounded cooperative backoff, not admission control. Taking
        // the credits before pausing makes abandoned reader pressure finite:
        // even if its source snapshotter is suspended forever, this writer
        // consumes the entire residual cost and then enters normally.
        let credits = self.snapshot_backoff_credits.swap(0, Ordering::AcqRel);
        let spins = credits
            .min(MAX_SNAPSHOT_BACKOFF_CREDITS)
            .saturating_mul(SNAPSHOT_BACKOFF_SPINS_PER_CREDIT);
        for _ in 0..spins {
            std::hint::spin_loop();
        }
        let mut active = self.active_writers.load(Ordering::Acquire);
        loop {
            let next = active
                .checked_add(1)
                .expect("concurrent snapshot-writer count exhausted");
            match self.active_writers.compare_exchange_weak(
                active,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                // The acquire half keeps every subsequent backend write after
                // this admission announcement; the release half participates
                // in the counter's single modification order.
                Ok(_) => break,
                Err(observed) => active = observed,
            }
        }
        SnapshotMutation {
            memo: self,
            // Conservatively invalidate if a backend mutation unwinds.
            dirty: true,
        }
    }

    fn get_or_create(
        &self,
        mut create: impl FnMut(SnapshotIdentity) -> Arc<dyn SnapshotOps>,
    ) -> Arc<dyn SnapshotOps> {
        #[cfg(feature = "perf-instrumentation")]
        let _legacy_guard = legacy_snapshot_locks_enabled().then(|| {
            self.legacy_control
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        });
        let mut retries = 0usize;
        loop {
            let revision = self.revision.load(Ordering::Acquire);
            if self.active_writers.load(Ordering::Acquire) != 0 {
                self.request_snapshot_window();
                snapshot_retry_pause(&mut retries);
                continue;
            }

            let snapshot = self.snapshot_for_revision(revision, true, &mut create);
            if self.capture_is_current(revision) {
                return snapshot;
            }
            self.request_snapshot_window();
            snapshot_retry_pause(&mut retries);
        }
    }

    #[inline]
    fn capture_is_current(&self, revision: u64) -> bool {
        self.capture_is_current_after(revision, || {})
    }

    #[inline]
    fn capture_is_current_after(&self, revision: u64, after_active_load: impl FnOnce()) -> bool {
        // Do not reverse these loads. If a writer completes between them, the
        // Acquire count load either saw it active or synchronizes with its
        // final AcqRel withdrawal; the later revision load then rejects the
        // capture. A deterministic unit test holds that exact interval open.
        let active_writers = self.active_writers.load(Ordering::Acquire);
        after_active_load();
        let validated_revision = self.revision.load(Ordering::Acquire);
        active_writers == 0 && validated_revision == revision
    }

    /// Memoize a snapshot whose revision comes from the backend's atomically
    /// captured immutable graph descriptor.
    ///
    /// DynamicDAWG does not need the heterogeneous writer handshake above:
    /// root, count, and revision are fields of one retained `GraphVersion`.
    fn get_or_create_at(
        &self,
        revision: u64,
        mut create: impl FnMut(SnapshotIdentity) -> Arc<dyn SnapshotOps>,
    ) -> Arc<dyn SnapshotOps> {
        self.observe_authoritative_revision(revision);
        self.snapshot_for_revision(revision, true, &mut create)
    }

    /// Advance the memo's authoritative revision floor and evict any older
    /// warmed arena without perturbing a snapshot already built for this or a
    /// newer revision.
    ///
    /// DynamicDAWG publishes root, count, and revision in one retained graph
    /// descriptor. Its mutation guard calls this after publication, while a
    /// snapshot capture also calls it before memo lookup. The latter closes
    /// the race where a current capture reaches the memo before its mutator's
    /// guard runs; the monotonic floor prevents an older in-flight capture
    /// from repopulating the cache after invalidation.
    fn observe_authoritative_revision(&self, revision: u64) {
        let previous = self.revision.fetch_max(revision, Ordering::AcqRel);
        let floor = previous.max(revision);
        loop {
            let cached = self.cached.load_full();
            let Some(entry) = cached.as_ref() else {
                return;
            };
            if entry.revision >= floor {
                return;
            }
            let observed = self.cached.compare_and_swap(&cached, None);
            if observed.as_ref().map(Arc::as_ptr) == cached.as_ref().map(Arc::as_ptr) {
                return;
            }
        }
    }

    #[inline]
    fn request_snapshot_window(&self) {
        let _ = self.snapshot_backoff_credits.try_update(
            Ordering::Release,
            Ordering::Relaxed,
            |credits| Some(credits.saturating_add(1).min(MAX_SNAPSHOT_BACKOFF_CREDITS)),
        );
    }

    fn snapshot_for_revision(
        &self,
        revision: u64,
        memo_revision_is_authoritative: bool,
        create: &mut impl FnMut(SnapshotIdentity) -> Arc<dyn SnapshotOps>,
    ) -> Arc<dyn SnapshotOps> {
        loop {
            let cell = self.cached.load_full();
            if let Some(cached) = cell.as_ref().filter(|cached| cached.revision == revision) {
                if let Some(snapshot) = cached.snapshot.get() {
                    return Arc::clone(snapshot);
                }

                // Poll only; never call `get_or_init`, whose initializer lease
                // can block forever when its owner is descheduled. The pointer
                // identity is the generation token. Once another contender
                // replaces it, immediately follow the new generation.
                let mut current_generation = true;
                for attempt in 0..COLD_INITIALIZER_WAIT_ATTEMPTS {
                    if let Some(snapshot) = cached.snapshot.get() {
                        return Arc::clone(snapshot);
                    }
                    let observed = self.cached.load_full();
                    current_generation = observed
                        .as_ref()
                        .is_some_and(|observed| Arc::ptr_eq(observed, cached));
                    if !current_generation {
                        break;
                    }
                    cold_initializer_wait_pause(attempt);
                }
                if !current_generation {
                    continue;
                }
                if let Some(snapshot) = cached.snapshot.get() {
                    return Arc::clone(snapshot);
                }
                if memo_revision_is_authoritative
                    && self.revision.load(Ordering::Acquire) != revision
                {
                    return create(SnapshotIdentity {
                        producer: self.producer,
                        revision,
                    });
                }

                let candidate = Arc::new(CachedSnapshot {
                    revision,
                    generation: cached
                        .generation
                        .checked_add(1)
                        .expect("snapshot initializer generation exhausted"),
                    snapshot: OnceLock::new(),
                });
                let previous = self
                    .cached
                    .compare_and_swap(&cell, Some(Arc::clone(&candidate)));
                if previous.as_ref().map(Arc::as_ptr) == cell.as_ref().map(Arc::as_ptr) {
                    return self.initialize_snapshot_candidate(revision, candidate, create);
                }
                continue;
            }

            // A stale capture must not displace a newer revision's candidate.
            // Its outer validation will reject the result after construction.
            if (memo_revision_is_authoritative && self.revision.load(Ordering::Acquire) != revision)
                || cell
                    .as_ref()
                    .is_some_and(|cached| cached.revision > revision)
            {
                return create(SnapshotIdentity {
                    producer: self.producer,
                    revision,
                });
            }

            let candidate = Arc::new(CachedSnapshot {
                revision,
                generation: 0,
                snapshot: OnceLock::new(),
            });
            let previous = self
                .cached
                .compare_and_swap(&cell, Some(Arc::clone(&candidate)));
            if previous.as_ref().map(Arc::as_ptr) == cell.as_ref().map(Arc::as_ptr) {
                return self.initialize_snapshot_candidate(revision, candidate, create);
            }
        }
    }

    fn initialize_snapshot_candidate(
        &self,
        revision: u64,
        candidate: Arc<CachedSnapshot>,
        create: &mut impl FnMut(SnapshotIdentity) -> Arc<dyn SnapshotOps>,
    ) -> Arc<dyn SnapshotOps> {
        let snapshot = create(SnapshotIdentity {
            producer: self.producer,
            revision,
        });
        // Exactly the thread that CAS-published this generation initializes
        // it. A takeover publishes a distinct Arc and therefore never races
        // this OnceLock. If construction panics, the empty generation remains
        // observable and is superseded after the bounded poll above.
        candidate
            .snapshot
            .set(Arc::clone(&snapshot))
            .unwrap_or_else(|_| unreachable!("one publisher initializes each snapshot generation"));
        snapshot
    }
}

#[inline]
fn cold_initializer_wait_pause(attempt: usize) {
    if attempt < 32 {
        std::hint::spin_loop();
    } else {
        std::thread::yield_now();
    }
}

#[inline]
fn snapshot_retry_pause(retries: &mut usize) {
    *retries = retries.saturating_add(1);
    if *retries <= 8 {
        std::hint::spin_loop();
    } else {
        std::thread::yield_now();
    }
}

#[cfg(feature = "perf-instrumentation")]
fn legacy_snapshot_locks_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var_os("LIBLEVENSHTEIN_CAUSAL_USE_LEGACY_SNAPSHOT_LOCKS").is_some()
    })
}

impl SnapshotMutation<'_> {
    fn finish(mut self, dirty: bool) {
        self.dirty = dirty;
    }
}

impl Drop for SnapshotMutation<'_> {
    fn drop(&mut self) {
        // Backend publication is sequenced before this guard is finished.
        // For a dirty mutation, the AcqRel revision RMW publishes those writes
        // before the final AcqRel withdrawal from `active_writers`. A reader
        // loads the active count with Acquire and then the revision with
        // Acquire. It therefore cannot accept across this exit: it observes
        // either a still-active writer or the changed revision. Cache eviction
        // precedes both markers, so a matching post-exit revision cannot return
        // the invalidated candidate.
        let revision_exhausted = if self.dirty {
            self.memo.cached.store(None);
            self.memo
                .revision
                .try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                    current.checked_add(1)
                })
                .is_err()
        } else {
            false
        };
        // Acquire prior RMWs as well as releasing this writer's revision. If
        // this is the 1 -> 0 withdrawal, a reader that acquires it observes a
        // completion chain covering every earlier concurrent writer.
        let previous = self.memo.active_writers.fetch_sub(1, Ordering::AcqRel);
        debug_assert_ne!(previous, 0);
        // Restore the writer-progress state before surfacing the practically
        // unreachable identity-exhaustion failure from this Drop path.
        assert!(
            !revision_exhausted,
            "snapshot revision identity space exhausted"
        );
    }
}

struct SnapshotSource<B> {
    backend: B,
    snapshots: SnapshotMemo,
}

impl<B> SnapshotSource<B> {
    fn new(backend: B) -> Self {
        Self {
            backend,
            snapshots: SnapshotMemo::new(),
        }
    }
}

impl<B> std::ops::Deref for SnapshotSource<B> {
    type Target = B;

    fn deref(&self) -> &Self::Target {
        &self.backend
    }
}

enum DynamicBackend {
    Byte(DynamicDawg<BindingValue>),
    Unicode(DynamicDawgChar<BindingValue>),
    U64(DynamicDawgU64<BindingValue>),
}

/// One retained DynamicDAWG graph generation. Root, count, and revision are
/// captured by one ArcSwap load in the unit-generic core.
enum DynamicSnapshotCapture {
    Byte {
        root: DynamicDawgNode<BindingValue>,
        term_count: usize,
        revision: u64,
    },
    Unicode {
        root: DynamicDawgCharNode<BindingValue>,
        term_count: usize,
        revision: u64,
    },
    U64 {
        root: DynamicDawgU64Node<BindingValue>,
        term_count: usize,
        revision: u64,
    },
}

impl DynamicSnapshotCapture {
    fn revision(&self) -> u64 {
        match self {
            Self::Byte { revision, .. }
            | Self::Unicode { revision, .. }
            | Self::U64 { revision, .. } => *revision,
        }
    }

    fn snapshot(&self, identity: SnapshotIdentity) -> Arc<dyn SnapshotOps> {
        match self {
            Self::Byte {
                root, term_count, ..
            } => exact_traversal_snapshot(
                root.clone(),
                *term_count,
                VtUnitDomain::Byte,
                false,
                identity,
            ),
            Self::Unicode {
                root, term_count, ..
            } => exact_traversal_snapshot(
                root.clone(),
                *term_count,
                VtUnitDomain::UnicodeScalar,
                false,
                identity,
            ),
            Self::U64 {
                root, term_count, ..
            } => exact_traversal_snapshot(
                root.clone(),
                *term_count,
                VtUnitDomain::U64,
                false,
                identity,
            ),
        }
    }
}

impl DynamicBackend {
    fn new(domain: BindingUnitDomain) -> Self {
        match domain {
            BindingUnitDomain::Byte => Self::Byte(DynamicDawg::new()),
            BindingUnitDomain::UnicodeScalar => Self::Unicode(DynamicDawgChar::new()),
            BindingUnitDomain::U64 => Self::U64(DynamicDawgU64::new()),
        }
    }

    fn domain(&self) -> BindingUnitDomain {
        match self {
            Self::Byte(_) => BindingUnitDomain::Byte,
            Self::Unicode(_) => BindingUnitDomain::UnicodeScalar,
            Self::U64(_) => BindingUnitDomain::U64,
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::Byte(dictionary) => dictionary.term_count(),
            Self::Unicode(dictionary) => dictionary.term_count(),
            Self::U64(dictionary) => dictionary.term_count(),
        }
    }

    fn capture_snapshot(&self) -> DynamicSnapshotCapture {
        match self {
            Self::Byte(dictionary) => {
                let (root, term_count, revision) = dictionary.root_with_term_count_revision();
                DynamicSnapshotCapture::Byte {
                    root,
                    term_count,
                    revision,
                }
            }
            Self::Unicode(dictionary) => {
                let (root, term_count, revision) = dictionary.root_with_term_count_revision();
                DynamicSnapshotCapture::Unicode {
                    root,
                    term_count,
                    revision,
                }
            }
            Self::U64(dictionary) => {
                let (root, term_count, revision) = dictionary.root_with_term_count_revision();
                DynamicSnapshotCapture::U64 {
                    root,
                    term_count,
                    revision,
                }
            }
        }
    }

    #[cfg(test)]
    fn snapshot(&self, identity: SnapshotIdentity) -> Arc<dyn SnapshotOps> {
        self.capture_snapshot().snapshot(identity)
    }

    fn clear(&self) -> bool {
        match self {
            Self::Byte(dictionary) => dictionary.clear_graph(),
            Self::Unicode(dictionary) => dictionary.clear_graph(),
            Self::U64(dictionary) => dictionary.clear_graph(),
        }
    }
}

struct SharedDictionary {
    /// The selected domain never changes. Each contained DynamicDAWG already
    /// owns an immutable graph-generation ArcSwap, so an outer lock would only
    /// serialize otherwise lock-free reads and writers.
    backend: DynamicBackend,
    snapshots: SnapshotMemo,
}

impl SharedDictionary {
    /// Couple every public DynamicDAWG mutation to the revision-backed
    /// snapshot memo without adding writer admission or a lock. Capturing the
    /// latest graph descriptor also makes out-of-order guard drops converge on
    /// the newest published revision.
    fn snapshot_revision_guard(&self) -> DynamicSnapshotRevisionGuard<'_> {
        DynamicSnapshotRevisionGuard { dictionary: self }
    }
}

struct DynamicSnapshotRevisionGuard<'a> {
    dictionary: &'a SharedDictionary,
}

impl Drop for DynamicSnapshotRevisionGuard<'_> {
    fn drop(&mut self) {
        let revision = self.dictionary.backend.capture_snapshot().revision();
        self.dictionary
            .snapshots
            .observe_authoritative_revision(revision);
    }
}

#[cfg(feature = "persistent-artrie")]
enum PersistentBackend {
    Byte(crate::persistent_artrie::PersistentARTrie<BindingValue>),
    Unicode(crate::persistent_artrie::char::PersistentARTrieChar<BindingValue>),
    U64(crate::persistent_artrie::u64::PersistentARTrieU64<BindingValue>),
    Vocab(crate::persistent_artrie::vocab::PersistentVocabARTrie),
}

#[cfg(feature = "persistent-artrie")]
impl PersistentBackend {
    fn domain(&self) -> BindingUnitDomain {
        match self {
            Self::Byte(_) => BindingUnitDomain::Byte,
            Self::Unicode(_) | Self::Vocab(_) => BindingUnitDomain::UnicodeScalar,
            Self::U64(_) => BindingUnitDomain::U64,
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::Byte(dictionary) => dictionary.overlay_len(),
            Self::Unicode(dictionary) => dictionary.len(),
            Self::U64(dictionary) => dictionary.term_count(),
            Self::Vocab(dictionary) => dictionary.len(),
        }
    }

    fn snapshot(&self, identity: SnapshotIdentity) -> Arc<dyn SnapshotOps> {
        match self {
            Self::Byte(dictionary) => {
                let (root, term_count) = dictionary.root_with_term_count();
                exact_traversal_snapshot(root, term_count, VtUnitDomain::Byte, false, identity)
            }
            Self::Unicode(dictionary) => {
                let (root, term_count) = dictionary.root_with_term_count();
                exact_traversal_snapshot(
                    root,
                    term_count,
                    VtUnitDomain::UnicodeScalar,
                    false,
                    identity,
                )
            }
            Self::U64(dictionary) => {
                let (root, term_count) = dictionary.root_with_term_count();
                exact_traversal_snapshot(root, term_count, VtUnitDomain::U64, false, identity)
            }
            Self::Vocab(dictionary) => {
                let (root, term_count) = dictionary.root_with_term_count();
                exact_traversal_snapshot(
                    root,
                    term_count,
                    VtUnitDomain::UnicodeScalar,
                    false,
                    identity,
                )
            }
        }
    }
}

/// Disk-backed persistent ARTrie family exposed to foreign-language bindings.
#[cfg(feature = "persistent-artrie")]
#[derive(Clone)]
pub struct PersistentARTrieBinding {
    shared: Arc<SnapshotSource<PersistentBackend>>,
}

#[cfg(feature = "persistent-artrie")]
impl PersistentARTrieBinding {
    /// Create a new byte, Unicode-scalar, or native-u64 persistent trie.
    pub fn create(path: impl AsRef<Path>, domain: BindingUnitDomain) -> Result<Self, BindingError> {
        let shared = match domain {
            BindingUnitDomain::Byte => Arc::new(SnapshotSource::new(PersistentBackend::Byte(
                crate::persistent_artrie::PersistentARTrie::create(path).map_err(io_error)?,
            ))),
            BindingUnitDomain::UnicodeScalar => {
                Arc::new(SnapshotSource::new(PersistentBackend::Unicode(
                    crate::persistent_artrie::char::PersistentARTrieChar::create(path)
                        .map_err(io_error)?,
                )))
            }
            BindingUnitDomain::U64 => Arc::new(SnapshotSource::new(PersistentBackend::U64(
                crate::persistent_artrie::u64::PersistentARTrieU64::create(path)
                    .map_err(io_error)?,
            ))),
        };
        Ok(Self { shared })
    }

    /// Open an existing byte, Unicode-scalar, or native-u64 persistent trie.
    pub fn open(path: impl AsRef<Path>, domain: BindingUnitDomain) -> Result<Self, BindingError> {
        let shared = match domain {
            BindingUnitDomain::Byte => Arc::new(SnapshotSource::new(PersistentBackend::Byte(
                crate::persistent_artrie::PersistentARTrie::open(path).map_err(io_error)?,
            ))),
            BindingUnitDomain::UnicodeScalar => {
                Arc::new(SnapshotSource::new(PersistentBackend::Unicode(
                    crate::persistent_artrie::char::PersistentARTrieChar::open(path)
                        .map_err(io_error)?,
                )))
            }
            BindingUnitDomain::U64 => Arc::new(SnapshotSource::new(PersistentBackend::U64(
                crate::persistent_artrie::u64::PersistentARTrieU64::open(path).map_err(io_error)?,
            ))),
        };
        Ok(Self { shared })
    }

    /// Create a persistent bidirectional term/index vocabulary.
    pub fn create_vocab(path: impl AsRef<Path>) -> Result<Self, BindingError> {
        Ok(Self {
            shared: Arc::new(SnapshotSource::new(PersistentBackend::Vocab(
                crate::persistent_artrie::vocab::PersistentVocabARTrie::create(path)
                    .map_err(io_error)?,
            ))),
        })
    }

    /// Open an existing persistent bidirectional term/index vocabulary.
    pub fn open_vocab(path: impl AsRef<Path>) -> Result<Self, BindingError> {
        Ok(Self {
            shared: Arc::new(SnapshotSource::new(PersistentBackend::Vocab(
                crate::persistent_artrie::vocab::PersistentVocabARTrie::open(path)
                    .map_err(io_error)?,
            ))),
        })
    }

    /// Unit domain used by dictionary transitions.
    pub fn domain(&self) -> BindingUnitDomain {
        self.shared.domain()
    }

    /// Number of visible terms in the current revision.
    pub fn len(&self) -> usize {
        self.shared.len()
    }

    /// Whether the current revision has no visible terms.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether this is the bidirectional vocabulary ARTrie variant.
    pub fn is_vocab(&self) -> bool {
        matches!(&self.shared.backend, PersistentBackend::Vocab(_))
    }

    /// Insert/update a byte or Unicode term and optional u64 metadata.
    pub fn insert_text(&self, term: &[u8], value: Option<u64>) -> Result<bool, BindingError> {
        let mutation = self.shared.snapshots.begin_mutation();
        let result = match &self.shared.backend {
            PersistentBackend::Byte(dictionary) => dictionary
                .upsert_bytes(term, BindingValue::from_option(value))
                .map_err(io_error),
            PersistentBackend::Unicode(dictionary) => {
                let term = std::str::from_utf8(term).map_err(|_| BindingError::InvalidUtf8)?;
                dictionary
                    .insert_with_value(term, BindingValue::from_option(value))
                    .map_err(io_error)
            }
            PersistentBackend::Vocab(dictionary) => {
                let term = std::str::from_utf8(term).map_err(|_| BindingError::InvalidUtf8)?;
                if let Some(index) = value {
                    dictionary.insert_with_index(term, index).map_err(io_error)
                } else {
                    let existed = dictionary.contains(term);
                    dictionary.insert(term).map_err(io_error)?;
                    Ok(!existed)
                }
            }
            PersistentBackend::U64(_) => Err(BindingError::DomainMismatch),
        };
        // An I/O error may be reported after durable or overlay state changed.
        // Keep RAII's conservative dirty=true on Err; only a proven Ok(false)
        // is known not to have published a new visible term.
        mutation.finish(!matches!(result, Ok(false)));
        result
    }

    /// Remove a byte or Unicode term where the selected variant supports removal.
    pub fn remove_text(&self, term: &[u8]) -> Result<bool, BindingError> {
        let mutation = self.shared.snapshots.begin_mutation();
        let result = match &self.shared.backend {
            PersistentBackend::Byte(dictionary) => {
                dictionary.remove_cas_durable(term).map_err(io_error)
            }
            PersistentBackend::Unicode(dictionary) => {
                let term = std::str::from_utf8(term).map_err(|_| BindingError::InvalidUtf8)?;
                dictionary.remove(term).map_err(io_error)
            }
            PersistentBackend::Vocab(_) => Err(BindingError::Unsupported),
            PersistentBackend::U64(_) => Err(BindingError::DomainMismatch),
        };
        mutation.finish(!matches!(result, Ok(false)));
        result
    }

    /// Test byte or Unicode exact membership.
    pub fn contains_text(&self, term: &[u8]) -> Result<bool, BindingError> {
        match &self.shared.backend {
            PersistentBackend::Byte(dictionary) => Ok(dictionary.contains_bytes(term)),
            PersistentBackend::Unicode(dictionary) => {
                let term = std::str::from_utf8(term).map_err(|_| BindingError::InvalidUtf8)?;
                Ok(dictionary.contains(term))
            }
            PersistentBackend::Vocab(dictionary) => {
                let term = std::str::from_utf8(term).map_err(|_| BindingError::InvalidUtf8)?;
                Ok(dictionary.contains(term))
            }
            PersistentBackend::U64(_) => Err(BindingError::DomainMismatch),
        }
    }

    /// Read byte or Unicode metadata, preserving absent versus valueless terms.
    pub fn value_text(&self, term: &[u8]) -> Result<Option<Option<u64>>, BindingError> {
        match &self.shared.backend {
            PersistentBackend::Byte(dictionary) => Ok(dictionary
                .get_value_bytes(term)
                .map(BindingValue::into_option)),
            PersistentBackend::Unicode(dictionary) => {
                let term = std::str::from_utf8(term).map_err(|_| BindingError::InvalidUtf8)?;
                Ok(dictionary.get_value(term).map(BindingValue::into_option))
            }
            PersistentBackend::Vocab(dictionary) => {
                let term = std::str::from_utf8(term).map_err(|_| BindingError::InvalidUtf8)?;
                Ok(dictionary.get_index(term).map(Some))
            }
            PersistentBackend::U64(_) => Err(BindingError::DomainMismatch),
        }
    }

    /// Insert/update a native-u64 term.
    ///
    /// Engine write failures propagate as I/O errors exactly like the byte
    /// and Unicode profiles; the infallible `insert_sequence_with_value`
    /// wrapper (which logs and returns `false`) is deliberately not used
    /// here — the ABI must report `IO_ERROR`, never a silent no-op `OK`.
    pub fn insert_u64(&self, term: &[u64], value: Option<u64>) -> Result<bool, BindingError> {
        let mutation = self.shared.snapshots.begin_mutation();
        let result = match &self.shared.backend {
            PersistentBackend::U64(dictionary) => dictionary
                .try_insert_sequence_with_value(term, BindingValue::from_option(value))
                .map_err(io_error),
            _ => Err(BindingError::DomainMismatch),
        };
        mutation.finish(!matches!(result, Ok(false)));
        result
    }

    /// Remove a native-u64 term.
    ///
    /// Engine write failures propagate as I/O errors (see `insert_u64`).
    pub fn remove_u64(&self, term: &[u64]) -> Result<bool, BindingError> {
        let mutation = self.shared.snapshots.begin_mutation();
        let result = match &self.shared.backend {
            PersistentBackend::U64(dictionary) => {
                dictionary.try_remove_sequence(term).map_err(io_error)
            }
            _ => Err(BindingError::DomainMismatch),
        };
        mutation.finish(!matches!(result, Ok(false)));
        result
    }

    /// Test native-u64 exact membership.
    pub fn contains_u64(&self, term: &[u64]) -> Result<bool, BindingError> {
        match &self.shared.backend {
            PersistentBackend::U64(dictionary) => Ok(dictionary.contains_sequence(term)),
            _ => Err(BindingError::DomainMismatch),
        }
    }

    /// Read native-u64 metadata, preserving absent versus valueless terms.
    pub fn value_u64(&self, term: &[u64]) -> Result<Option<Option<u64>>, BindingError> {
        match &self.shared.backend {
            PersistentBackend::U64(dictionary) => Ok(dictionary
                .get_sequence_value(term)
                .map(BindingValue::into_option)),
            _ => Err(BindingError::DomainMismatch),
        }
    }

    /// Atomically checkpoint the current revision to disk.
    pub fn checkpoint(&self) -> Result<(), BindingError> {
        match &self.shared.backend {
            PersistentBackend::Byte(dictionary) => dictionary.checkpoint().map_err(io_error),
            PersistentBackend::Unicode(dictionary) => dictionary.checkpoint().map_err(io_error),
            PersistentBackend::U64(dictionary) => dictionary.checkpoint().map_err(io_error),
            PersistentBackend::Vocab(dictionary) => dictionary.checkpoint().map_err(io_error),
        }
    }

    /// Look up a vocabulary term by its stable index.
    pub fn vocab_term(&self, index: u64) -> Result<Option<String>, BindingError> {
        match &self.shared.backend {
            PersistentBackend::Vocab(dictionary) => Ok(dictionary.get_term(index)),
            _ => Err(BindingError::Unsupported),
        }
    }

    /// Borrow a retained interoperable dictionary resource.
    pub fn resource(&self) -> OwnedDictionaryResource {
        OwnedDictionaryResource::new(ResourcePayload::Persistent(Arc::clone(&self.shared)))
    }
}

#[cfg(feature = "persistent-artrie")]
fn io_error(error: impl std::fmt::Display) -> BindingError {
    BindingError::Io(error.to_string())
}

enum SecondaryBackend {
    DoubleArrayByte(DoubleArrayTrie<BindingValue>),
    DoubleArrayUnicode(DoubleArrayTrieChar<BindingValue>),
    ScdawgByte(Scdawg<BindingValue>),
    ScdawgUnicode(ScdawgChar<BindingValue>),
}

/// Same-binary causal control for direct immutable-DAT cursor snapshots.
#[inline]
fn dat_cursor_resource_snapshots_enabled() -> bool {
    #[cfg(any(feature = "perf-instrumentation", feature = "benchmark-controls"))]
    {
        static ENABLED: OnceLock<bool> = OnceLock::new();
        *ENABLED.get_or_init(|| {
            std::env::var_os("LIBDICTENSTEIN_CAUSAL_DISABLE_DAT_CURSOR_SNAPSHOTS").is_none()
        })
    }
    #[cfg(not(any(feature = "perf-instrumentation", feature = "benchmark-controls")))]
    {
        true
    }
}

impl SecondaryBackend {
    fn domain(&self) -> BindingUnitDomain {
        match self {
            Self::DoubleArrayByte(_) | Self::ScdawgByte(_) => BindingUnitDomain::Byte,
            Self::DoubleArrayUnicode(_) | Self::ScdawgUnicode(_) => {
                BindingUnitDomain::UnicodeScalar
            }
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::DoubleArrayByte(dictionary) => dictionary.len().unwrap_or_default(),
            Self::DoubleArrayUnicode(dictionary) => dictionary.len().unwrap_or_default(),
            Self::ScdawgByte(dictionary) => dictionary.term_count(),
            Self::ScdawgUnicode(dictionary) => dictionary.term_count(),
        }
    }

    fn suffix(&self) -> bool {
        matches!(self, Self::ScdawgByte(_) | Self::ScdawgUnicode(_))
    }

    fn snapshot(&self, identity: SnapshotIdentity) -> Arc<dyn SnapshotOps> {
        match self {
            // DoubleArrayTrie backends are immutable after construction, so
            // separate root()/len() reads cannot tear (no writer exists).
            Self::DoubleArrayByte(dictionary) => {
                let root = dictionary.root();
                if dat_cursor_resource_snapshots_enabled() {
                    Arc::new(
                        CursorTraversalSnapshot::new(
                            root,
                            dictionary.len(),
                            VtUnitDomain::Byte,
                            false,
                            identity,
                        )
                        .expect("byte DAT roots provide validated mapped cursors"),
                    )
                } else {
                    Arc::new(TraversalSnapshot::new(
                        root,
                        dictionary.len(),
                        VtUnitDomain::Byte,
                        false,
                        identity,
                    ))
                }
            }
            Self::DoubleArrayUnicode(dictionary) => {
                let root = dictionary.root();
                if dat_cursor_resource_snapshots_enabled() {
                    Arc::new(
                        CursorTraversalSnapshot::new(
                            root,
                            dictionary.len(),
                            VtUnitDomain::UnicodeScalar,
                            false,
                            identity,
                        )
                        .expect("Unicode DAT roots provide validated mapped cursors"),
                    )
                } else {
                    Arc::new(TraversalSnapshot::new(
                        root,
                        dictionary.len(),
                        VtUnitDomain::UnicodeScalar,
                        false,
                        identity,
                    ))
                }
            }
            // SCDAWGs are mutable: pair the root with the count from ONE
            // published revision (finding LDICT-B4).
            Self::ScdawgByte(dictionary) => {
                let (root, term_count, entries) = dictionary.root_with_term_count_and_entries();
                Arc::new(
                    TraversalSnapshot::new(
                        root,
                        Some(term_count),
                        VtUnitDomain::Byte,
                        true,
                        identity,
                    )
                    .with_entry_factory(move || {
                        entries.clone().map(|(term, value)| {
                            (
                                term.into_bytes().into_iter().map(u64::from).collect(),
                                value.and_then(BindingValue::into_option),
                            )
                        })
                    }),
                )
            }
            Self::ScdawgUnicode(dictionary) => {
                let (root, term_count, entries) = dictionary.root_with_term_count_and_entries();
                Arc::new(
                    TraversalSnapshot::new(
                        root,
                        Some(term_count),
                        VtUnitDomain::UnicodeScalar,
                        true,
                        identity,
                    )
                    .with_entry_factory(move || {
                        entries.clone().map(|(term, value)| {
                            (
                                term.chars().map(|unit| u64::from(unit as u32)).collect(),
                                value.and_then(BindingValue::into_option),
                            )
                        })
                    }),
                )
            }
        }
    }
}

/// Immutable, cache-local DoubleArrayTrie exposed to foreign-language bindings.
#[derive(Clone)]
pub struct DoubleArrayTrieBinding {
    shared: Arc<SnapshotSource<SecondaryBackend>>,
}

impl DoubleArrayTrieBinding {
    /// Build a byte-transition DAT from UTF-8 terms and optional metadata.
    pub fn from_byte_terms<I, S>(entries: I) -> Self
    where
        I: IntoIterator<Item = (S, Option<u64>)>,
        S: AsRef<str>,
    {
        let dictionary = DoubleArrayTrie::from_terms_with_values(
            entries
                .into_iter()
                .map(|(term, value)| (term, BindingValue::from_option(value))),
        );
        Self {
            shared: Arc::new(SnapshotSource::new(SecondaryBackend::DoubleArrayByte(
                dictionary,
            ))),
        }
    }

    /// Build a Unicode-scalar DAT from terms and optional metadata.
    pub fn from_unicode_terms<I, S>(entries: I) -> Self
    where
        I: IntoIterator<Item = (S, Option<u64>)>,
        S: AsRef<str>,
    {
        let dictionary = DoubleArrayTrieChar::from_terms_with_values(
            entries
                .into_iter()
                .map(|(term, value)| (term, BindingValue::from_option(value))),
        );
        Self {
            shared: Arc::new(SnapshotSource::new(SecondaryBackend::DoubleArrayUnicode(
                dictionary,
            ))),
        }
    }

    /// Unit domain used by transitions.
    pub fn domain(&self) -> BindingUnitDomain {
        self.shared.domain()
    }

    /// Number of exact dictionary terms.
    pub fn len(&self) -> usize {
        self.shared.len()
    }

    /// Whether there are no exact dictionary terms.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Test exact membership.
    pub fn contains(&self, term: &str) -> bool {
        match &self.shared.backend {
            SecondaryBackend::DoubleArrayByte(dictionary) => dictionary.contains(term),
            SecondaryBackend::DoubleArrayUnicode(dictionary) => dictionary.contains(term),
            _ => unreachable!("DoubleArrayTrieBinding contains only DAT backends"),
        }
    }

    /// Read optional metadata while preserving absent versus valueless terms.
    pub fn value(&self, term: &str) -> Option<Option<u64>> {
        match &self.shared.backend {
            SecondaryBackend::DoubleArrayByte(dictionary) => {
                dictionary.get_value(term).map(BindingValue::into_option)
            }
            SecondaryBackend::DoubleArrayUnicode(dictionary) => {
                dictionary.get_value(term).map(BindingValue::into_option)
            }
            _ => unreachable!("DoubleArrayTrieBinding contains only DAT backends"),
        }
    }

    /// Borrow a retained interoperable dictionary resource.
    pub fn resource(&self) -> OwnedDictionaryResource {
        OwnedDictionaryResource::new(ResourcePayload::Secondary(Arc::clone(&self.shared)))
    }
}

/// Mutable SCDAWG binding with exact-term and substring operations.
#[derive(Clone)]
pub struct ScdawgBinding {
    shared: Arc<SnapshotSource<SecondaryBackend>>,
}

impl ScdawgBinding {
    /// Construct an empty byte-transition SCDAWG.
    pub fn new_byte() -> Self {
        Self {
            shared: Arc::new(SnapshotSource::new(SecondaryBackend::ScdawgByte(
                Scdawg::new(),
            ))),
        }
    }

    /// Construct an empty Unicode-scalar SCDAWG.
    pub fn new_unicode() -> Self {
        Self {
            shared: Arc::new(SnapshotSource::new(SecondaryBackend::ScdawgUnicode(
                ScdawgChar::new(),
            ))),
        }
    }

    /// Unit domain used by transitions.
    pub fn domain(&self) -> BindingUnitDomain {
        self.shared.domain()
    }

    /// Number of indexed exact terms.
    pub fn len(&self) -> usize {
        self.shared.len()
    }

    /// Whether no exact terms are indexed.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Insert or update a term and optional metadata.
    pub fn insert(&self, term: &str, value: Option<u64>) -> bool {
        let mutation = self.shared.snapshots.begin_mutation();
        let value = BindingValue::from_option(value);
        let inserted = match &self.shared.backend {
            SecondaryBackend::ScdawgByte(dictionary) => dictionary.insert_with_value(term, value),
            SecondaryBackend::ScdawgUnicode(dictionary) => {
                dictionary.insert_with_value(term, value)
            }
            _ => unreachable!("ScdawgBinding contains only SCDAWG backends"),
        };
        mutation.finish(true);
        inserted
    }

    /// Test exact-term membership.
    pub fn contains(&self, term: &str) -> bool {
        match &self.shared.backend {
            SecondaryBackend::ScdawgByte(dictionary) => dictionary.contains(term),
            SecondaryBackend::ScdawgUnicode(dictionary) => dictionary.contains(term),
            _ => unreachable!("ScdawgBinding contains only SCDAWG backends"),
        }
    }

    /// Test whether a pattern occurs as a substring of an indexed term.
    pub fn contains_substring(&self, pattern: &str) -> bool {
        match &self.shared.backend {
            SecondaryBackend::ScdawgByte(dictionary) => dictionary.contains_substring(pattern),
            SecondaryBackend::ScdawgUnicode(dictionary) => dictionary.contains_substring(pattern),
            _ => unreachable!("ScdawgBinding contains only SCDAWG backends"),
        }
    }

    /// Count occurrences of a substring across indexed terms.
    pub fn frequency(&self, pattern: &str) -> usize {
        match &self.shared.backend {
            SecondaryBackend::ScdawgByte(dictionary) => dictionary.freq(pattern),
            SecondaryBackend::ScdawgUnicode(dictionary) => dictionary.freq(pattern),
            _ => unreachable!("ScdawgBinding contains only SCDAWG backends"),
        }
    }

    /// Read optional metadata while preserving absent versus valueless terms.
    pub fn value(&self, term: &str) -> Option<Option<u64>> {
        match &self.shared.backend {
            SecondaryBackend::ScdawgByte(dictionary) => {
                dictionary.get_value(term).map(BindingValue::into_option)
            }
            SecondaryBackend::ScdawgUnicode(dictionary) => {
                dictionary.get_value(term).map(BindingValue::into_option)
            }
            _ => unreachable!("ScdawgBinding contains only SCDAWG backends"),
        }
    }

    /// Borrow a retained interoperable dictionary resource.
    pub fn resource(&self) -> OwnedDictionaryResource {
        OwnedDictionaryResource::new(ResourcePayload::Secondary(Arc::clone(&self.shared)))
    }
}

/// Mutable DynamicDAWG exposed to foreign-language bindings.
///
/// Clones share the same atomically published revisions. A resource borrowed
/// from this object remains usable after the object is dropped whenever a
/// consumer has retained it through the shared ABI.
#[derive(Clone)]
pub struct DynamicDawgBinding {
    shared: Arc<SharedDictionary>,
}

impl std::fmt::Debug for DynamicDawgBinding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DynamicDawgBinding")
            .field("domain", &self.domain())
            .field("len", &self.len())
            .finish()
    }
}

impl DynamicDawgBinding {
    /// Construct an empty DynamicDAWG for one term domain.
    pub fn new(domain: BindingUnitDomain) -> Self {
        Self {
            shared: Arc::new(SharedDictionary {
                backend: DynamicBackend::new(domain),
                snapshots: SnapshotMemo::new(),
            }),
        }
    }

    /// Construct a DynamicDAWG directly from an already sorted, duplicate-free
    /// binding snapshot.
    ///
    /// This crate-internal path lets family ABI combinators feed their linear
    /// merge output directly into the minimal freeze-once builder without a
    /// second sort, key clone, or sequence of incremental publications.
    pub(crate) fn from_sorted_binding_entries(
        domain: BindingUnitDomain,
        entries: Vec<BindingEntry>,
    ) -> Self {
        debug_assert!(entries.windows(2).all(|pair| pair[0].term < pair[1].term));

        let backend = match domain {
            BindingUnitDomain::Byte => DynamicBackend::Byte(DynamicDawg::from_sorted_byte_entries(
                entries.into_iter().map(|entry| {
                    let BindingTerm::Bytes(term) = entry.term else {
                        unreachable!("byte algebra result contains a non-byte key")
                    };
                    (term, entry.value.map(BindingValue::present))
                }),
            )),
            BindingUnitDomain::UnicodeScalar => DynamicBackend::Unicode(
                DynamicDawgChar::from_sorted_optional_entries(entries.into_iter().map(|entry| {
                    let BindingTerm::Unicode(term) = entry.term else {
                        unreachable!("Unicode algebra result contains a non-Unicode key")
                    };
                    (term, entry.value.map(BindingValue::present))
                })),
            ),
            BindingUnitDomain::U64 => DynamicBackend::U64(
                DynamicDawgU64::from_sorted_sequence_entries(entries.into_iter().map(|entry| {
                    let BindingTerm::U64(term) = entry.term else {
                        unreachable!("u64 algebra result contains a non-u64 key")
                    };
                    (term, entry.value.map(BindingValue::present))
                })),
            ),
        };

        Self {
            shared: Arc::new(SharedDictionary {
                backend,
                snapshots: SnapshotMemo::new(),
            }),
        }
    }

    /// Return the dictionary's immutable unit domain.
    pub fn domain(&self) -> BindingUnitDomain {
        self.shared.backend.domain()
    }

    /// Return the number of visible terms.
    pub fn len(&self) -> usize {
        self.shared.backend.len()
    }

    /// Return whether the dictionary is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Insert or update a UTF-8/byte term and optional value.
    pub fn insert_text(&self, term: &[u8], value: Option<u64>) -> Result<bool, BindingError> {
        let _snapshot_revision = self.shared.snapshot_revision_guard();
        let result = match &self.shared.backend {
            DynamicBackend::Byte(dictionary) => {
                Ok(dictionary
                    .insert_bytes_with_optional_value(term, value.map(BindingValue::present)))
            }
            DynamicBackend::Unicode(dictionary) => {
                let term = std::str::from_utf8(term).map_err(|_| BindingError::InvalidUtf8)?;
                Ok(dictionary.insert_with_optional_value(term, value.map(BindingValue::present)))
            }
            DynamicBackend::U64(_) => Err(BindingError::DomainMismatch),
        };
        result
    }

    /// Insert/update a complete text batch, selecting the freeze-once builder
    /// when the dictionary is empty.
    ///
    /// Ordered input is detected in linear time and skips sorting. Unordered
    /// input is stably sorted so duplicate entries retain last-value-wins
    /// semantics. The frozen candidate is built privately and published only
    /// if the inner graph generation is still empty. A competing insert makes
    /// this operation merge through ordinary path-copy CAS publication.
    pub fn insert_text_batch<'a, I>(&self, entries: I) -> Result<usize, BindingError>
    where
        I: IntoIterator<Item = (&'a [u8], Option<u64>)>,
    {
        let _snapshot_revision = self.shared.snapshot_revision_guard();
        let entries: Vec<_> = entries.into_iter().collect();
        match &self.shared.backend {
            DynamicBackend::Byte(dictionary) => {
                let mut owned: Vec<_> = entries
                    .iter()
                    .map(|(term, value)| (term.to_vec(), value.map(BindingValue::present)))
                    .collect();
                if owned.is_empty() {
                    return Ok(0);
                }
                if dictionary.term_count() == 0 {
                    if !owned.windows(2).all(|pair| pair[0].0 <= pair[1].0) {
                        crate::causal_perf::record_batch_sort_calls(1);
                        crate::causal_perf::record_batch_sort_terms(owned.len() as u64);
                        crate::causal_perf::record_batch_sort_units(
                            owned.iter().map(|(term, _)| term.len()).sum::<usize>() as u64,
                        );
                        owned.sort_by(|left, right| left.0.cmp(&right.0));
                    }
                    let frozen = DynamicDawg::from_sorted_byte_entries(owned.clone());
                    if let PublishIfEmpty::Published(len) = dictionary.try_publish_if_empty(&frozen)
                    {
                        return Ok(len);
                    }
                }
                Ok(owned
                    .into_iter()
                    .map(|(term, value)| dictionary.insert_bytes_with_optional_value(&term, value))
                    .map(usize::from)
                    .sum())
            }
            DynamicBackend::Unicode(dictionary) => {
                // Validate the complete batch before any graph publication.
                let mut owned = Vec::with_capacity(entries.len());
                for (term, value) in &entries {
                    let term = std::str::from_utf8(term)
                        .map_err(|_| BindingError::InvalidUtf8)?
                        .to_owned();
                    owned.push((term, value.map(BindingValue::present)));
                }
                if owned.is_empty() {
                    return Ok(0);
                }
                if dictionary.term_count() == 0 {
                    if !owned.windows(2).all(|pair| pair[0].0 <= pair[1].0) {
                        crate::causal_perf::record_batch_sort_calls(1);
                        crate::causal_perf::record_batch_sort_terms(owned.len() as u64);
                        crate::causal_perf::record_batch_sort_units(
                            owned
                                .iter()
                                .map(|(term, _)| term.chars().count())
                                .sum::<usize>() as u64,
                        );
                        owned.sort_by(|left, right| left.0.cmp(&right.0));
                    }
                    let frozen = DynamicDawgChar::from_sorted_optional_entries(owned.clone());
                    if let PublishIfEmpty::Published(len) = dictionary.try_publish_if_empty(&frozen)
                    {
                        return Ok(len);
                    }
                }
                Ok(owned
                    .into_iter()
                    .map(|(term, value)| dictionary.insert_with_optional_value(&term, value))
                    .map(usize::from)
                    .sum())
            }
            DynamicBackend::U64(_) => Err(BindingError::DomainMismatch),
        }
    }

    /// Remove a UTF-8/byte term.
    pub fn remove_text(&self, term: &[u8]) -> Result<bool, BindingError> {
        let _snapshot_revision = self.shared.snapshot_revision_guard();
        let result = match &self.shared.backend {
            DynamicBackend::Byte(dictionary) => Ok(dictionary.remove_bytes(term)),
            DynamicBackend::Unicode(dictionary) => {
                let term = std::str::from_utf8(term).map_err(|_| BindingError::InvalidUtf8)?;
                Ok(dictionary.remove(term))
            }
            DynamicBackend::U64(_) => Err(BindingError::DomainMismatch),
        };
        result
    }

    /// Test membership for a UTF-8/byte term.
    pub fn contains_text(&self, term: &[u8]) -> Result<bool, BindingError> {
        match &self.shared.backend {
            DynamicBackend::Byte(dictionary) => Ok(dictionary.contains_bytes(term)),
            DynamicBackend::Unicode(dictionary) => {
                let term = std::str::from_utf8(term).map_err(|_| BindingError::InvalidUtf8)?;
                Ok(dictionary.contains(term))
            }
            DynamicBackend::U64(_) => Err(BindingError::DomainMismatch),
        }
    }

    /// Read the optional value for a UTF-8/byte term.
    pub fn value_text(&self, term: &[u8]) -> Result<Option<Option<u64>>, BindingError> {
        match &self.shared.backend {
            DynamicBackend::Byte(dictionary) => Ok(dictionary
                .get_bytes_optional_value(term)
                .map(|value| value.and_then(BindingValue::into_option))),
            DynamicBackend::Unicode(dictionary) => {
                let term = std::str::from_utf8(term).map_err(|_| BindingError::InvalidUtf8)?;
                Ok(dictionary
                    .get_optional_value(term)
                    .map(|value| value.and_then(BindingValue::into_option)))
            }
            DynamicBackend::U64(_) => Err(BindingError::DomainMismatch),
        }
    }

    /// Insert or update a u64-token term and optional value.
    pub fn insert_u64(&self, term: &[u64], value: Option<u64>) -> Result<bool, BindingError> {
        let _snapshot_revision = self.shared.snapshot_revision_guard();
        match &self.shared.backend {
            DynamicBackend::U64(dictionary) => Ok(dictionary
                .insert_sequence_with_optional_value(term, value.map(BindingValue::present))),
            _ => Err(BindingError::DomainMismatch),
        }
    }

    /// Insert/update a complete u64 batch with the same empty-dictionary
    /// freeze-once fast path as [`insert_text_batch`](Self::insert_text_batch).
    pub fn insert_u64_batch<'a, I>(&self, entries: I) -> Result<usize, BindingError>
    where
        I: IntoIterator<Item = (&'a [u64], Option<u64>)>,
    {
        let _snapshot_revision = self.shared.snapshot_revision_guard();
        let DynamicBackend::U64(dictionary) = &self.shared.backend else {
            return Err(BindingError::DomainMismatch);
        };
        let mut owned: Vec<_> = entries
            .into_iter()
            .map(|(term, value)| (term.to_vec(), value.map(BindingValue::present)))
            .collect();
        if owned.is_empty() {
            return Ok(0);
        }
        if dictionary.term_count() == 0 {
            if !owned.windows(2).all(|pair| pair[0].0 <= pair[1].0) {
                crate::causal_perf::record_batch_sort_calls(1);
                crate::causal_perf::record_batch_sort_terms(owned.len() as u64);
                crate::causal_perf::record_batch_sort_units(
                    owned.iter().map(|(term, _)| term.len()).sum::<usize>() as u64,
                );
                owned.sort_by(|left, right| left.0.cmp(&right.0));
            }
            let frozen = DynamicDawgU64::from_sorted_sequence_entries(owned.clone());
            if let PublishIfEmpty::Published(len) = dictionary.try_publish_if_empty(&frozen) {
                return Ok(len);
            }
        }
        Ok(owned
            .into_iter()
            .map(|(term, value)| dictionary.insert_sequence_with_optional_value(&term, value))
            .map(usize::from)
            .sum())
    }

    /// Remove a u64-token term.
    pub fn remove_u64(&self, term: &[u64]) -> Result<bool, BindingError> {
        let _snapshot_revision = self.shared.snapshot_revision_guard();
        match &self.shared.backend {
            DynamicBackend::U64(dictionary) => Ok(dictionary.remove_sequence(term)),
            _ => Err(BindingError::DomainMismatch),
        }
    }

    /// Test membership for a u64-token term.
    pub fn contains_u64(&self, term: &[u64]) -> Result<bool, BindingError> {
        match &self.shared.backend {
            DynamicBackend::U64(dictionary) => Ok(dictionary.contains_sequence(term)),
            _ => Err(BindingError::DomainMismatch),
        }
    }

    /// Read the optional value for a u64-token term.
    pub fn value_u64(&self, term: &[u64]) -> Result<Option<Option<u64>>, BindingError> {
        match &self.shared.backend {
            DynamicBackend::U64(dictionary) => Ok(dictionary
                .get_sequence_optional_value(term)
                .map(|value| value.and_then(BindingValue::into_option))),
            _ => Err(BindingError::DomainMismatch),
        }
    }

    /// Remove every term by publishing one empty inner graph generation.
    pub fn clear(&self) {
        let _snapshot_revision = self.shared.snapshot_revision_guard();
        self.shared.backend.clear();
    }

    /// Restore compact DynamicDAWG structure and return reclaimed nodes.
    pub fn compact(&self) -> usize {
        let _snapshot_revision = self.shared.snapshot_revision_guard();
        match &self.shared.backend {
            DynamicBackend::Byte(dictionary) => dictionary.compact(),
            DynamicBackend::Unicode(dictionary) => dictionary.compact(),
            DynamicBackend::U64(dictionary) => dictionary.compact(),
        }
    }

    /// Borrow a two-word resource. The returned resource owns one retain.
    pub fn resource(&self) -> OwnedDictionaryResource {
        OwnedDictionaryResource::new(ResourcePayload::Live(Arc::clone(&self.shared)))
    }
}

trait AbiUnit: Copy + Send + Sync + 'static {
    fn to_abi(self) -> u64;
    fn from_abi(value: u64) -> Option<Self>;
}

trait AbiValue: crate::DictionaryValue {
    fn into_abi_value(self) -> Option<u64>;
}

impl AbiValue for BindingValue {
    fn into_abi_value(self) -> Option<u64> {
        self.into_option()
    }
}

impl AbiValue for u64 {
    fn into_abi_value(self) -> Option<u64> {
        Some(self)
    }
}

impl AbiUnit for u8 {
    fn to_abi(self) -> u64 {
        u64::from(self)
    }

    fn from_abi(value: u64) -> Option<Self> {
        u8::try_from(value).ok()
    }
}

impl AbiUnit for char {
    fn to_abi(self) -> u64 {
        u64::from(u32::from(self))
    }

    fn from_abi(value: u64) -> Option<Self> {
        u32::try_from(value).ok().and_then(char::from_u32)
    }
}

impl AbiUnit for u64 {
    fn to_abi(self) -> u64 {
        self
    }

    fn from_abi(value: u64) -> Option<Self> {
        Some(value)
    }
}

const TRAVERSAL_CHUNK_SIZE: usize = 256;
const TRAVERSAL_DENSE_CHUNKS: usize = 64;
const TRAVERSAL_SPARSE_SHARDS: usize = 64;

struct ArenaNode<N> {
    node: N,
    edges: OnceLock<Result<Vec<(u64, u64)>, VtStatus>>,
}

impl<N> ArenaNode<N> {
    fn new(node: N) -> Self {
        Self {
            node,
            edges: OnceLock::new(),
        }
    }
}

/// Append-only, chunked lazy node arena.
///
/// Readers load an immutable ArcSwap directory and an atomic slot without a
/// lock. Writers geometrically grow and CAS-publish the directory. Each node's
/// outgoing edges are expanded exactly once by its local `OnceLock`.
struct NodeArena<N> {
    slots: HybridOnceBoxSlots<
        ArenaNode<N>,
        TRAVERSAL_CHUNK_SIZE,
        TRAVERSAL_DENSE_CHUNKS,
        TRAVERSAL_SPARSE_SHARDS,
    >,
    next_id: AtomicU64,
    published: AtomicU64,
    stable_identity: bool,
}

impl<N> NodeArena<N> {
    fn new(root: N, root_identity: Option<SnapshotNodeIdentity>) -> Self {
        let slots = HybridOnceBoxSlots::new();
        let (_, installed) = slots.install_if_absent_with_status(0, ArenaNode::new(root));
        debug_assert!(installed);
        Self {
            slots,
            next_id: AtomicU64::new(1),
            published: AtomicU64::new(1),
            stable_identity: root_identity.is_some(),
        }
    }

    fn slot(&self, node: u64) -> Result<&ArenaNode<N>, VtStatus> {
        self.slots.get(node).ok_or(VtStatus::InvalidArgument)
    }

    fn reserve(&self, count: usize) -> Result<u64, VtStatus> {
        let count = u64::try_from(count).map_err(|_| VtStatus::LimitExceeded)?;
        if count == 0 {
            return Ok(self.next_id.load(Ordering::Acquire));
        }
        let mut start = self.next_id.load(Ordering::Acquire);
        loop {
            let end = start.checked_add(count).ok_or(VtStatus::LimitExceeded)?;
            match self.next_id.compare_exchange_weak(
                start,
                end,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(start),
                Err(observed) => start = observed,
            }
        }
    }

    fn install(&self, node: u64, value: N) -> Result<(), VtStatus> {
        if node >= self.next_id.load(Ordering::Acquire) {
            return Err(VtStatus::LimitExceeded);
        }
        let (_, installed) = self
            .slots
            .install_if_absent_with_status(node, ArenaNode::new(value));
        if installed {
            self.published.fetch_add(1, Ordering::Relaxed);
            Ok(())
        } else {
            Err(VtStatus::InvalidArgument)
        }
    }
}

impl<N: DictionaryNode> NodeArena<N> {
    fn install_stable(&self, value: N) -> Result<(u64, bool), VtStatus> {
        if !self.stable_identity {
            return Err(VtStatus::InvalidArgument);
        }
        let node = value
            .snapshot_node_identity()
            .ok_or(VtStatus::InvalidArgument)?
            .get();
        let (_, installed) = self
            .slots
            .install_if_absent_with_status(node, ArenaNode::new(value));
        if installed {
            self.published.fetch_add(1, Ordering::Relaxed);
        }
        Ok((node, installed))
    }
}

impl<N> Drop for NodeArena<N> {
    fn drop(&mut self) {
        #[cfg(feature = "perf-instrumentation")]
        let started = std::time::Instant::now();
        let nodes = self.published.load(Ordering::Relaxed);
        // Reclaim every published chunk on the releasing thread. Swapping an
        // empty directory makes the destruction point explicit and measurable;
        // no background reclaimer or unbounded deferred queue is involved.
        self.slots.clear();
        crate::causal_perf::record_resource_nodes_reclaimed(nodes);
        #[cfg(feature = "perf-instrumentation")]
        {
            let nanos = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
            crate::causal_perf::record_resource_reclaim_nanos(nanos);
            crate::causal_perf::record_resource_reclaim_max_nanos(nanos);
        }
    }
}

struct TraversalSnapshot<N: DictionaryNode> {
    native_graph: OnceLock<Option<SnapshotTraversalProjection<N>>>,
    abi_graph: OnceLock<AbiTraversalGraph>,
    len: Option<usize>,
    domain: VtUnitDomain,
    suffix: bool,
    identity: SnapshotIdentity,
    entry_factory: Option<SnapshotEntryFactory>,
    // Keep the immutable owner last so native provenance handles are dropped
    // before the revision that makes them valid.
    arena: NodeArena<N>,
}

/// Immutable resource snapshot backed by the dictionary's own compact cursor.
///
/// The retained root owns the immutable revision. ABI node identifiers are the
/// backend's validated one-based cursor words, so visits copy edge descriptors
/// directly into the caller's page and never populate a second node arena.
struct CursorTraversalSnapshot<N: DictionaryNode<SnapshotCursor = SnapshotTraversalCursor>> {
    root: N,
    root_cursor: SnapshotTraversalCursor,
    len: Option<usize>,
    domain: VtUnitDomain,
    suffix: bool,
    identity: SnapshotIdentity,
}

struct AbiTraversalGraph {
    nodes: Box<[VtDictionaryGraphNode]>,
    edges: Box<[VtDictionaryGraphEdge]>,
    root: u64,
}

impl AbiTraversalGraph {
    fn from_native<U, H>(graph: &SnapshotTraversalGraph<U, H>) -> Self
    where
        U: AbiUnit + crate::CharUnit,
        H: Copy,
    {
        crate::causal_perf::record_resource_graph_projections(1);
        let nodes = (0..graph.node_count())
            .map(|index| {
                let node = graph
                    .node(index)
                    .expect("native graph node index is in bounds");
                VtDictionaryGraphNode {
                    edge_start: u64::from(node.edge_start()),
                    edge_len: u64::from(node.edge_len()),
                    // Keep backend pointer cursors behind the producer trust
                    // boundary. The ABI token is the checked one-based dense
                    // graph index and is translated only by `graph_value`.
                    value_cursor: u64::try_from(index + 1)
                        .expect("snapshot graph node index fits u64"),
                    is_final: u8::from(node.is_final()),
                    reserved: [0; 7],
                }
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let edges = graph
            .edges()
            .iter()
            .map(|edge| VtDictionaryGraphEdge {
                label: edge.label().to_abi(),
                target: u64::try_from(edge.target_cursor().get() - 1)
                    .expect("snapshot graph target fits u64"),
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            nodes,
            edges,
            root: u64::from(graph.root_index()),
        }
    }

    fn view(&self) -> VtDictionaryGraphView {
        VtDictionaryGraphView {
            nodes: self.nodes.as_ptr(),
            node_count: self.nodes.len(),
            edges: self.edges.as_ptr(),
            edge_count: self.edges.len(),
            root: self.root,
            reserved: 0,
        }
    }
}

impl<N: DictionaryNode> TraversalSnapshot<N> {
    fn new(
        root: N,
        len: Option<usize>,
        domain: VtUnitDomain,
        suffix: bool,
        identity: SnapshotIdentity,
    ) -> Self {
        crate::causal_perf::record_resource_snapshots_created(1);
        crate::causal_perf::record_resource_nodes_materialized(1);
        let root_identity = root.snapshot_node_identity();
        // Causal profiling needs a same-binary control for the physical-node
        // identity treatment.  Keep the switch out of ordinary builds and
        // evaluate it only once, while capturing the immutable snapshot.
        #[cfg(feature = "perf-instrumentation")]
        let root_identity = if std::env::var("LIBDICTENSTEIN_CAUSAL_DISABLE_STABLE_NODE_IDENTITY")
            .is_ok_and(|value| value == "1")
        {
            None
        } else {
            root_identity
        };
        Self {
            native_graph: OnceLock::new(),
            abi_graph: OnceLock::new(),
            len,
            domain,
            suffix,
            identity,
            entry_factory: None,
            arena: NodeArena::new(root, root_identity),
        }
    }

    fn with_entry_factory<F, I>(mut self, factory: F) -> Self
    where
        F: Fn() -> I + Send + Sync + 'static,
        I: Iterator<Item = (Vec<u64>, Option<u64>)> + Send + 'static,
    {
        self.entry_factory = Some(Arc::new(move || Box::new(factory())));
        self
    }
}

/// Build one exact-dictionary snapshot with a sequential entry stream backed
/// by the same generic native/graph/owned traversal selector as the pure Rust
/// collection API.
///
/// The random-access graph ABI retains its lazy, validated node arena. Entry
/// cursors do not accept caller-supplied node identifiers, so they can safely
/// keep the captured root and use backend-native cursors directly. This avoids
/// populating and probing a second arena during a full collection scan while
/// preserving one implementation across byte, Unicode-scalar, and `u64`
/// dictionaries.
fn exact_traversal_snapshot<N>(
    root: N,
    len: usize,
    domain: VtUnitDomain,
    suffix: bool,
    identity: SnapshotIdentity,
) -> Arc<dyn SnapshotOps>
where
    N: MappedDictionaryNode + 'static,
    N::Unit: AbiUnit,
    N::Value: AbiValue,
{
    let entry_root = root.clone();
    Arc::new(
        TraversalSnapshot::new(root, Some(len), domain, suffix, identity).with_entry_factory(
            move || {
                crate::collection::ExactSnapshotEntryIterator::from_node(entry_root.clone(), len)
                    .map(|entry| {
                        (
                            entry.key.into_iter().map(AbiUnit::to_abi).collect(),
                            entry.value.and_then(AbiValue::into_abi_value),
                        )
                    })
            },
        ),
    )
}

type SnapshotEntryStream = Box<dyn Iterator<Item = (Vec<u64>, Option<u64>)> + Send>;
type SnapshotEntryFactory = Arc<dyn Fn() -> SnapshotEntryStream + Send + Sync>;

trait SnapshotOps: Send + Sync {
    fn root(&self) -> u64;
    fn domain(&self) -> VtUnitDomain;
    fn suffix(&self) -> bool;
    fn len(&self) -> Option<usize>;
    fn identity(&self) -> SnapshotIdentity;
    fn entries(&self) -> Option<SnapshotEntryStream> {
        None
    }
    fn graph(&self) -> Option<VtDictionaryGraphView>;
    fn graph_value(&self, value_cursor: u64) -> Result<Option<u64>, VtStatus>;
    fn is_final(&self, node: u64) -> Result<bool, VtStatus>;
    fn value(&self, node: u64) -> Result<Option<u64>, VtStatus>;
    fn transition(&self, node: u64, label: u64) -> Result<Option<u64>, VtStatus>;
    fn copy_edges(
        &self,
        node: u64,
        start: usize,
        output: &mut [VtDictionaryEdge],
    ) -> Result<(usize, usize), VtStatus>;
    fn copy_node(
        &self,
        node: u64,
        start: usize,
        output: &mut [VtDictionaryEdge],
    ) -> Result<(bool, usize, usize), VtStatus> {
        let is_final = self.is_final(node)?;
        let (written, total) = self.copy_edges(node, start, output)?;
        Ok((is_final, written, total))
    }
}

impl<N> CursorTraversalSnapshot<N>
where
    N: MappedDictionaryNode<SnapshotCursor = SnapshotTraversalCursor> + 'static,
    N::Unit: AbiUnit,
    N::Value: AbiValue,
{
    fn new(
        root: N,
        len: Option<usize>,
        domain: VtUnitDomain,
        suffix: bool,
        identity: SnapshotIdentity,
    ) -> Option<Self> {
        let root_cursor = root.snapshot_root_cursor()?;
        if !root.contains_snapshot_cursor(root_cursor) || !root.supports_snapshot_cursor_values() {
            return None;
        }
        crate::causal_perf::record_resource_snapshots_created(1);
        Some(Self {
            root,
            root_cursor,
            len,
            domain,
            suffix,
            identity,
        })
    }

    #[inline]
    fn abi_cursor(cursor: SnapshotTraversalCursor) -> Result<u64, VtStatus> {
        u64::try_from(cursor.get()).map_err(|_| VtStatus::LimitExceeded)
    }

    #[inline]
    fn native_cursor(&self, node: u64) -> Result<SnapshotTraversalCursor, VtStatus> {
        let cursor = usize::try_from(node)
            .ok()
            .and_then(SnapshotTraversalCursor::new)
            .ok_or(VtStatus::InvalidArgument)?;
        self.root
            .contains_snapshot_cursor(cursor)
            .then_some(cursor)
            .ok_or(VtStatus::InvalidArgument)
    }

    fn copy_cursor_node(
        &self,
        node: u64,
        start: usize,
        output: &mut [VtDictionaryEdge],
    ) -> Result<(bool, usize, usize), VtStatus> {
        let cursor = self.native_cursor(node)?;
        let mut written = 0usize;
        // SAFETY: `native_cursor` validated this cursor against the retained
        // root revision. Every child is emitted by that same revision.
        let (is_final, total) = unsafe {
            self.root.visit_snapshot_cursor_edge_page(
                cursor,
                start,
                output.len(),
                |label, child| {
                    output[written] = VtDictionaryEdge {
                        label: label.to_abi(),
                        node: Self::abi_cursor(child)
                            .expect("a native cursor already fits the platform usize"),
                    };
                    written += 1;
                },
            )
        }
        .ok_or(VtStatus::Unsupported)?;
        crate::causal_perf::record_resource_native_edges_enumerated(written as u64);
        Ok((is_final, written, total))
    }
}

impl<N> SnapshotOps for CursorTraversalSnapshot<N>
where
    N: MappedDictionaryNode<SnapshotCursor = SnapshotTraversalCursor> + 'static,
    N::Unit: AbiUnit,
    N::Value: AbiValue,
{
    fn root(&self) -> u64 {
        Self::abi_cursor(self.root_cursor).expect("the root cursor fits the ABI")
    }

    fn domain(&self) -> VtUnitDomain {
        self.domain
    }

    fn suffix(&self) -> bool {
        self.suffix
    }

    fn len(&self) -> Option<usize> {
        self.len
    }

    fn identity(&self) -> SnapshotIdentity {
        self.identity
    }

    fn graph(&self) -> Option<VtDictionaryGraphView> {
        None
    }

    fn graph_value(&self, _value_cursor: u64) -> Result<Option<u64>, VtStatus> {
        Err(VtStatus::Unsupported)
    }

    fn is_final(&self, node: u64) -> Result<bool, VtStatus> {
        crate::causal_perf::record_resource_is_final_calls(1);
        let cursor = self.native_cursor(node)?;
        // SAFETY: `native_cursor` validated this cursor against `self.root`.
        unsafe { self.root.snapshot_cursor_is_final(cursor) }.ok_or(VtStatus::Unsupported)
    }

    fn value(&self, node: u64) -> Result<Option<u64>, VtStatus> {
        crate::causal_perf::record_resource_value_calls(1);
        let cursor = self.native_cursor(node)?;
        // SAFETY: `native_cursor` validated this cursor against `self.root`.
        let value =
            unsafe { self.root.snapshot_cursor_value(cursor) }.ok_or(VtStatus::Unsupported)?;
        Ok(value.and_then(AbiValue::into_abi_value))
    }

    fn transition(&self, node: u64, label: u64) -> Result<Option<u64>, VtStatus> {
        let cursor = self.native_cursor(node)?;
        let Some(label) = N::Unit::from_abi(label) else {
            return Ok(None);
        };
        // SAFETY: `native_cursor` validated this cursor against `self.root`.
        let result = unsafe { self.root.snapshot_cursor_transition(cursor, label) }
            .ok_or(VtStatus::Unsupported)?;
        result.map(Self::abi_cursor).transpose()
    }

    fn copy_edges(
        &self,
        node: u64,
        start: usize,
        output: &mut [VtDictionaryEdge],
    ) -> Result<(usize, usize), VtStatus> {
        crate::causal_perf::record_resource_edges_calls(1);
        let (_, written, total) = self.copy_cursor_node(node, start, output)?;
        Ok((written, total))
    }

    fn copy_node(
        &self,
        node: u64,
        start: usize,
        output: &mut [VtDictionaryEdge],
    ) -> Result<(bool, usize, usize), VtStatus> {
        crate::causal_perf::record_resource_is_final_calls(1);
        crate::causal_perf::record_resource_edges_calls(1);
        self.copy_cursor_node(node, start, output)
    }
}

impl<N> TraversalSnapshot<N>
where
    N: MappedDictionaryNode + 'static,
    N::Unit: AbiUnit,
    N::Value: AbiValue,
{
    fn edges<'a>(&self, slot: &'a ArenaNode<N>) -> Result<&'a [(u64, u64)], VtStatus> {
        let cached = slot.edges.get_or_init(|| {
            let children = crate::collect_node_edges(&slot.node);
            crate::causal_perf::record_resource_native_edges_enumerated(children.len() as u64);
            let child_count = children.len();
            let mut descriptors = Vec::with_capacity(child_count);
            let mut materialized = 0_u64;
            if self.arena.stable_identity {
                for (label, child) in children {
                    let (child_id, installed) = self.arena.install_stable(child)?;
                    materialized += u64::from(installed);
                    descriptors.push((label.to_abi(), child_id));
                }
            } else {
                let first_child = self.arena.reserve(child_count)?;
                for (offset, (label, child)) in children.into_iter().enumerate() {
                    let child_id = first_child
                        .checked_add(u64::try_from(offset).map_err(|_| VtStatus::LimitExceeded)?)
                        .ok_or(VtStatus::LimitExceeded)?;
                    self.arena.install(child_id, child)?;
                    materialized += 1;
                    descriptors.push((label.to_abi(), child_id));
                }
            }
            crate::causal_perf::record_resource_nodes_materialized(materialized);
            Ok(descriptors)
        });
        match cached {
            Ok(edges) => Ok(edges),
            Err(status) => Err(*status),
        }
    }
}

impl<N> SnapshotOps for TraversalSnapshot<N>
where
    N: MappedDictionaryNode + 'static,
    N::Unit: AbiUnit,
    N::Value: AbiValue,
{
    fn root(&self) -> u64 {
        0
    }

    fn domain(&self) -> VtUnitDomain {
        self.domain
    }

    fn suffix(&self) -> bool {
        self.suffix
    }

    fn len(&self) -> Option<usize> {
        self.len
    }

    fn identity(&self) -> SnapshotIdentity {
        self.identity
    }

    fn entries(&self) -> Option<SnapshotEntryStream> {
        self.entry_factory.as_ref().map(|factory| factory())
    }

    fn graph(&self) -> Option<VtDictionaryGraphView> {
        let root = &self.arena.slot(0).ok()?.node;
        if !root.supports_snapshot_graph_values() {
            return None;
        }
        let native = self
            .native_graph
            .get_or_init(|| root.snapshot_traversal_graph())
            .as_deref()?;
        Some(
            self.abi_graph
                .get_or_init(|| AbiTraversalGraph::from_native::<N::Unit, _>(native))
                .view(),
        )
    }

    fn graph_value(&self, value_cursor: u64) -> Result<Option<u64>, VtStatus> {
        crate::causal_perf::record_resource_value_calls(1);
        crate::causal_perf::record_resource_graph_value_calls(1);
        let root = &self.arena.slot(0)?.node;
        let graph = self
            .native_graph
            .get_or_init(|| root.snapshot_traversal_graph())
            .as_deref()
            .ok_or(VtStatus::Unsupported)?;
        let dense_cursor = usize::try_from(value_cursor)
            .ok()
            .and_then(SnapshotTraversalCursor::new)
            .ok_or(VtStatus::InvalidArgument)?;
        if dense_cursor.get() > graph.node_count() {
            return Err(VtStatus::InvalidArgument);
        }
        // SAFETY: the dense cursor was range-checked against the exact compact
        // graph captured with this retained root revision. Backend-native
        // handles remain inside the generic graph and never cross the ABI.
        let value = unsafe { root.snapshot_graph_cursor_value(graph, dense_cursor) }
            .ok_or(VtStatus::InvalidArgument)?;
        Ok(value.and_then(AbiValue::into_abi_value))
    }

    fn is_final(&self, node: u64) -> Result<bool, VtStatus> {
        crate::causal_perf::record_resource_is_final_calls(1);
        let slot = self.arena.slot(node)?;
        Ok(slot.node.is_final())
    }

    fn value(&self, node: u64) -> Result<Option<u64>, VtStatus> {
        crate::causal_perf::record_resource_value_calls(1);
        let slot = self.arena.slot(node)?;
        Ok(slot.node.value().and_then(AbiValue::into_abi_value))
    }

    fn transition(&self, node: u64, label: u64) -> Result<Option<u64>, VtStatus> {
        let slot = self.arena.slot(node)?;
        let cache_miss = slot.edges.get().is_none();
        let edges = self.edges(slot)?;
        if cache_miss {
            crate::causal_perf::record_resource_edge_cache_misses(1);
        }
        Ok(edges
            .iter()
            .find_map(|edge| (edge.0 == label).then_some(edge.1)))
    }

    fn copy_edges(
        &self,
        node: u64,
        start: usize,
        output: &mut [VtDictionaryEdge],
    ) -> Result<(usize, usize), VtStatus> {
        crate::causal_perf::record_resource_edges_calls(1);
        let slot = self.arena.slot(node)?;
        let cache_miss = slot.edges.get().is_none();
        let descriptors = self.edges(slot)?;
        if cache_miss {
            crate::causal_perf::record_resource_edge_cache_misses(1);
        }
        let total = descriptors.len();
        let page = descriptors.iter().skip(start).zip(output.iter_mut());
        let mut written = 0usize;
        for ((label, child), slot) in page {
            *slot = VtDictionaryEdge {
                label: *label,
                node: *child,
            };
            written += 1;
        }
        Ok((written, total))
    }

    fn copy_node(
        &self,
        node: u64,
        start: usize,
        output: &mut [VtDictionaryEdge],
    ) -> Result<(bool, usize, usize), VtStatus> {
        crate::causal_perf::record_resource_is_final_calls(1);
        crate::causal_perf::record_resource_edges_calls(1);
        let slot = self.arena.slot(node)?;
        let cache_miss = slot.edges.get().is_none();
        let is_final = slot.node.is_final();
        let descriptors = self.edges(slot)?;
        if cache_miss {
            crate::causal_perf::record_resource_edge_cache_misses(1);
        }
        let total = descriptors.len();
        let page = descriptors.iter().skip(start).zip(output.iter_mut());
        let mut written = 0usize;
        for ((label, child), slot) in page {
            *slot = VtDictionaryEdge {
                label: *label,
                node: *child,
            };
            written += 1;
        }
        Ok((is_final, written, total))
    }
}

enum ResourcePayload {
    Live(Arc<SharedDictionary>),
    Secondary(Arc<SnapshotSource<SecondaryBackend>>),
    #[cfg(feature = "persistent-artrie")]
    Persistent(Arc<SnapshotSource<PersistentBackend>>),
    Snapshot(Arc<dyn SnapshotOps>),
}

struct ResourceContext {
    payload: ResourcePayload,
}

impl ResourceContext {
    fn domain(&self) -> VtUnitDomain {
        match &self.payload {
            ResourcePayload::Live(dictionary) => dictionary.backend.domain().into(),
            ResourcePayload::Secondary(dictionary) => dictionary.backend.domain().into(),
            #[cfg(feature = "persistent-artrie")]
            ResourcePayload::Persistent(dictionary) => dictionary.backend.domain().into(),
            ResourcePayload::Snapshot(snapshot) => snapshot.domain(),
        }
    }

    fn flags(&self) -> u64 {
        dictionary_flags::PARALLEL_REENTRANT
            | match &self.payload {
                ResourcePayload::Live(_) => 0,
                ResourcePayload::Secondary(dictionary) => {
                    if dictionary.backend.suffix() {
                        dictionary_flags::SUFFIX_BASED
                    } else {
                        0
                    }
                }
                #[cfg(feature = "persistent-artrie")]
                ResourcePayload::Persistent(_) => 0,
                ResourcePayload::Snapshot(snapshot) => {
                    dictionary_flags::IMMUTABLE
                        | if snapshot.suffix() {
                            dictionary_flags::SUFFIX_BASED
                        } else {
                            0
                        }
                }
            }
    }

    fn snapshot(&self) -> Arc<dyn SnapshotOps> {
        match &self.payload {
            ResourcePayload::Live(dictionary) => {
                let capture = dictionary.backend.capture_snapshot();
                dictionary
                    .snapshots
                    .get_or_create_at(capture.revision(), |identity| capture.snapshot(identity))
            }
            ResourcePayload::Secondary(dictionary) => dictionary
                .snapshots
                .get_or_create(|identity| dictionary.backend.snapshot(identity)),
            #[cfg(feature = "persistent-artrie")]
            ResourcePayload::Persistent(dictionary) => dictionary
                .snapshots
                .get_or_create(|identity| dictionary.backend.snapshot(identity)),
            ResourcePayload::Snapshot(snapshot) => Arc::clone(snapshot),
        }
    }

    fn immutable(&self) -> Result<&dyn SnapshotOps, VtStatus> {
        match &self.payload {
            ResourcePayload::Snapshot(snapshot) => Ok(snapshot.as_ref()),
            ResourcePayload::Live(_) | ResourcePayload::Secondary(_) => {
                Err(VtStatus::InvalidArgument)
            }
            #[cfg(feature = "persistent-artrie")]
            ResourcePayload::Persistent(_) => Err(VtStatus::InvalidArgument),
        }
    }
}

/// Owned retain of a `vt.dictionary.v1` resource.
pub struct OwnedDictionaryResource {
    raw: VtResource,
}

impl OwnedDictionaryResource {
    fn new(payload: ResourcePayload) -> Self {
        let context = Arc::new(ResourceContext { payload });
        Self {
            raw: VtResource {
                context: Arc::into_raw(context).cast_mut().cast(),
                vtable: &RESOURCE_VTABLE,
            },
        }
    }

    /// Borrow the two-word ABI value.
    pub fn as_raw(&self) -> VtResource {
        self.raw
    }

    /// Traverse one immutable revision without crossing the C ABI.
    ///
    /// The returned iterator owns the snapshot and remains coherent if the
    /// live producer changes or this resource is dropped.
    pub fn entries(&self) -> BindingEntries {
        let context = unsafe { &*self.raw.context.cast::<ResourceContext>() };
        BindingEntries::new(context.snapshot())
    }
}

#[inline]
fn merge_binding_values(
    left: Option<u64>,
    right: Option<u64>,
    policy: BindingValueMerge,
) -> Option<u64> {
    match policy {
        BindingValueMerge::First => left,
        BindingValueMerge::Last => right,
        BindingValueMerge::LatticeJoin => left.join(&right),
        BindingValueMerge::LatticeMeet => left.meet(&right),
    }
}

/// Materialize an algebraic combination of two immutable dictionary revisions.
///
/// Each input is captured once through its snapshot-owning lexicographic entry
/// iterator. A single linear merge emits sorted, duplicate-free records
/// directly into the DynamicDAWG freeze-once builder. The returned dictionary
/// is mutable and independent of later changes to either input.
///
/// # Errors
///
/// Returns [`BindingAlgebraError::DomainMismatch`] when the captured term
/// domains differ, or propagates a provider status emitted while traversing
/// either snapshot.
pub fn dictionary_algebra(
    left: &OwnedDictionaryResource,
    right: &OwnedDictionaryResource,
    operation: BindingAlgebraOperation,
    value_merge: BindingValueMerge,
) -> Result<DynamicDawgBinding, BindingAlgebraError> {
    let mut left_entries = left.entries();
    let mut right_entries = right.entries();
    let domain = left_entries.domain();
    if domain != right_entries.domain() {
        return Err(BindingAlgebraError::DomainMismatch);
    }

    let left_len = left_entries.size_hint().1.unwrap_or(0);
    let right_len = right_entries.size_hint().1.unwrap_or(0);
    let capacity = match operation {
        BindingAlgebraOperation::Union | BindingAlgebraOperation::SymmetricDifference => {
            left_len.saturating_add(right_len)
        }
        BindingAlgebraOperation::Intersection => left_len.min(right_len),
        BindingAlgebraOperation::Difference => left_len,
    };
    let mut result = Vec::with_capacity(capacity);
    let mut left_entry = left_entries
        .next()
        .transpose()
        .map_err(BindingAlgebraError::Provider)?;
    let mut right_entry = right_entries
        .next()
        .transpose()
        .map_err(BindingAlgebraError::Provider)?;

    loop {
        match (left_entry.as_ref(), right_entry.as_ref()) {
            (Some(left_current), Some(right_current)) => {
                match left_current.term.cmp(&right_current.term) {
                    std::cmp::Ordering::Less => {
                        if matches!(
                            operation,
                            BindingAlgebraOperation::Union
                                | BindingAlgebraOperation::Difference
                                | BindingAlgebraOperation::SymmetricDifference
                        ) {
                            result.push(left_entry.take().expect("left entry is present"));
                        }
                        left_entry = left_entries
                            .next()
                            .transpose()
                            .map_err(BindingAlgebraError::Provider)?;
                    }
                    std::cmp::Ordering::Greater => {
                        if matches!(
                            operation,
                            BindingAlgebraOperation::Union
                                | BindingAlgebraOperation::SymmetricDifference
                        ) {
                            result.push(right_entry.take().expect("right entry is present"));
                        }
                        right_entry = right_entries
                            .next()
                            .transpose()
                            .map_err(BindingAlgebraError::Provider)?;
                    }
                    std::cmp::Ordering::Equal => {
                        if matches!(
                            operation,
                            BindingAlgebraOperation::Union | BindingAlgebraOperation::Intersection
                        ) {
                            let left_current = left_entry.take().expect("left entry is present");
                            result.push(BindingEntry {
                                term: left_current.term,
                                value: merge_binding_values(
                                    left_current.value,
                                    right_current.value,
                                    value_merge,
                                ),
                            });
                        }
                        left_entry = left_entries
                            .next()
                            .transpose()
                            .map_err(BindingAlgebraError::Provider)?;
                        right_entry = right_entries
                            .next()
                            .transpose()
                            .map_err(BindingAlgebraError::Provider)?;
                    }
                }
            }
            (Some(_), None) => {
                if matches!(
                    operation,
                    BindingAlgebraOperation::Union
                        | BindingAlgebraOperation::Difference
                        | BindingAlgebraOperation::SymmetricDifference
                ) {
                    result.push(left_entry.take().expect("left entry is present"));
                }
                left_entry = left_entries
                    .next()
                    .transpose()
                    .map_err(BindingAlgebraError::Provider)?;
            }
            (None, Some(_)) => {
                if matches!(
                    operation,
                    BindingAlgebraOperation::Union | BindingAlgebraOperation::SymmetricDifference
                ) {
                    result.push(right_entry.take().expect("right entry is present"));
                }
                right_entry = right_entries
                    .next()
                    .transpose()
                    .map_err(BindingAlgebraError::Provider)?;
            }
            (None, None) => break,
        }
    }

    Ok(DynamicDawgBinding::from_sorted_binding_entries(
        domain, result,
    ))
}

impl Drop for OwnedDictionaryResource {
    fn drop(&mut self) {
        unsafe { resource_release(self.raw.context) };
    }
}

unsafe extern "C" fn resource_retain(context: *mut c_void) {
    if !context.is_null() {
        Arc::increment_strong_count(context.cast::<ResourceContext>());
    }
}

unsafe extern "C" fn resource_release(context: *mut c_void) {
    if !context.is_null() {
        Arc::decrement_strong_count(context.cast::<ResourceContext>());
    }
}

unsafe extern "C" fn query_interface(
    context: *mut c_void,
    interface_id: *const VtInterfaceId,
    minimum_version: u32,
    out_vtable: *mut *const c_void,
) -> u32 {
    query_interface_status(context, interface_id, minimum_version, out_vtable).to_raw()
}

unsafe fn query_interface_status(
    context: *mut c_void,
    interface_id: *const VtInterfaceId,
    minimum_version: u32,
    out_vtable: *mut *const c_void,
) -> VtStatus {
    if context.is_null() || interface_id.is_null() || out_vtable.is_null() {
        return VtStatus::NullPointer;
    }
    let context = &*context.cast::<ResourceContext>();
    if (*interface_id).bytes == VT_DICTIONARY_INTERFACE_ID.bytes
        && minimum_version <= VT_DICTIONARY_INTERFACE_VERSION
    {
        out_vtable.write(dictionary_vtable(context.domain(), context.flags()).cast());
        VtStatus::Ok
    } else if (*interface_id).bytes == VT_DICTIONARY_VISIT_INTERFACE_ID.bytes
        && minimum_version <= VT_DICTIONARY_VISIT_INTERFACE_VERSION
    {
        out_vtable.write((&DICTIONARY_VISIT_VTABLE as *const VtDictionaryVisitVTable).cast());
        VtStatus::Ok
    } else if (*interface_id).bytes == VT_DICTIONARY_ENTRIES_INTERFACE_ID.bytes
        && minimum_version <= VT_DICTIONARY_ENTRIES_INTERFACE_VERSION
    {
        out_vtable.write(
            (&entries::DICTIONARY_ENTRIES_VTABLE
                as *const vinary_tree_interop::VtDictionaryEntriesVTable)
                .cast(),
        );
        VtStatus::Ok
    } else if (*interface_id).bytes == VT_DICTIONARY_GRAPH_INTERFACE_ID.bytes
        && minimum_version <= VT_DICTIONARY_GRAPH_INTERFACE_VERSION
        && context
            .immutable()
            .is_ok_and(|snapshot| snapshot.graph().is_some())
    {
        out_vtable.write((&DICTIONARY_GRAPH_VTABLE as *const VtDictionaryGraphVTable).cast());
        VtStatus::Ok
    } else if (*interface_id).bytes == VT_SNAPSHOT_IDENTITY_INTERFACE_ID.bytes
        && minimum_version <= VT_SNAPSHOT_IDENTITY_INTERFACE_VERSION
        && matches!(context.payload, ResourcePayload::Snapshot(_))
    {
        out_vtable.write((&SNAPSHOT_IDENTITY_VTABLE as *const VtSnapshotIdentityVTable).cast());
        VtStatus::Ok
    } else {
        VtStatus::Unsupported
    }
}

unsafe extern "C" fn dictionary_snapshot(
    context: *mut c_void,
    out_snapshot: *mut VtResource,
) -> u32 {
    dictionary_snapshot_status(context, out_snapshot).to_raw()
}

unsafe extern "C" fn dictionary_snapshot_identity(
    context: *mut c_void,
    out_identity: *mut VtSnapshotIdentity,
) -> u32 {
    if context.is_null() || out_identity.is_null() {
        return VtStatus::NullPointer.to_raw();
    }
    let context = &*context.cast::<ResourceContext>();
    let Ok(snapshot) = context.immutable() else {
        return VtStatus::InvalidArgument.to_raw();
    };
    let identity = snapshot.identity();
    out_identity.write(VtSnapshotIdentity {
        producer: identity.producer,
        revision: identity.revision,
    });
    VtStatus::Ok.to_raw()
}

unsafe extern "C" fn dictionary_graph(
    context: *mut c_void,
    out_graph: *mut VtDictionaryGraphView,
) -> u32 {
    if context.is_null() || out_graph.is_null() {
        return VtStatus::NullPointer.to_raw();
    }
    let context = &*context.cast::<ResourceContext>();
    crate::causal_perf::record_resource_graph_calls(1);
    let graph = match context
        .immutable()
        .and_then(|snapshot| snapshot.graph().ok_or(VtStatus::Unsupported))
    {
        Ok(graph) => graph,
        Err(status) => return status.to_raw(),
    };
    out_graph.write(graph);
    VtStatus::Ok.to_raw()
}

unsafe extern "C" fn dictionary_graph_value(
    context: *mut c_void,
    value_cursor: u64,
    out_value: *mut VtOptionalU64,
) -> u32 {
    if context.is_null() || out_value.is_null() {
        return VtStatus::NullPointer.to_raw();
    }
    let context = &*context.cast::<ResourceContext>();
    match context
        .immutable()
        .and_then(|snapshot| snapshot.graph_value(value_cursor))
    {
        Ok(value) => {
            out_value.write(VtOptionalU64 {
                value: value.unwrap_or_default(),
                has_value: u8::from(value.is_some()),
                reserved: [0; 7],
            });
            VtStatus::Ok.to_raw()
        }
        Err(status) => status.to_raw(),
    }
}

unsafe fn dictionary_snapshot_status(
    context: *mut c_void,
    out_snapshot: *mut VtResource,
) -> VtStatus {
    if context.is_null() || out_snapshot.is_null() {
        return VtStatus::NullPointer;
    }
    let context = &*context.cast::<ResourceContext>();
    let owned = OwnedDictionaryResource::new(ResourcePayload::Snapshot(context.snapshot()));
    out_snapshot.write(owned.raw);
    std::mem::forget(owned);
    VtStatus::Ok
}

unsafe extern "C" fn dictionary_root(context: *mut c_void, out_node: *mut u64) -> u32 {
    dictionary_root_status(context, out_node).to_raw()
}

unsafe fn dictionary_root_status(context: *mut c_void, out_node: *mut u64) -> VtStatus {
    if context.is_null() || out_node.is_null() {
        return VtStatus::NullPointer;
    }
    let context = &*context.cast::<ResourceContext>();
    match context.immutable() {
        Ok(snapshot) => {
            out_node.write(snapshot.root());
            VtStatus::Ok
        }
        Err(status) => status,
    }
}

unsafe extern "C" fn dictionary_len(
    context: *mut c_void,
    out_len: *mut usize,
    out_known: *mut u8,
) -> u32 {
    dictionary_len_status(context, out_len, out_known).to_raw()
}

unsafe fn dictionary_len_status(
    context: *mut c_void,
    out_len: *mut usize,
    out_known: *mut u8,
) -> VtStatus {
    if context.is_null() || out_len.is_null() || out_known.is_null() {
        return VtStatus::NullPointer;
    }
    let context = &*context.cast::<ResourceContext>();
    let Ok(snapshot) = context.immutable() else {
        return VtStatus::InvalidArgument;
    };
    match snapshot.len() {
        Some(len) => {
            out_len.write(len);
            out_known.write(1);
        }
        None => {
            out_len.write(0);
            out_known.write(0);
        }
    }
    VtStatus::Ok
}

unsafe extern "C" fn dictionary_is_final(
    context: *mut c_void,
    node: u64,
    out_is_final: *mut u8,
) -> u32 {
    dictionary_is_final_status(context, node, out_is_final).to_raw()
}

unsafe fn dictionary_is_final_status(
    context: *mut c_void,
    node: u64,
    out_is_final: *mut u8,
) -> VtStatus {
    if context.is_null() || out_is_final.is_null() {
        return VtStatus::NullPointer;
    }
    let context = &*context.cast::<ResourceContext>();
    match context
        .immutable()
        .and_then(|snapshot| snapshot.is_final(node))
    {
        Ok(value) => {
            out_is_final.write(u8::from(value));
            VtStatus::Ok
        }
        Err(status) => status,
    }
}

unsafe extern "C" fn dictionary_value(
    context: *mut c_void,
    node: u64,
    out_value: *mut VtOptionalU64,
) -> u32 {
    dictionary_value_status(context, node, out_value).to_raw()
}

unsafe fn dictionary_value_status(
    context: *mut c_void,
    node: u64,
    out_value: *mut VtOptionalU64,
) -> VtStatus {
    if context.is_null() || out_value.is_null() {
        return VtStatus::NullPointer;
    }
    let context = &*context.cast::<ResourceContext>();
    match context
        .immutable()
        .and_then(|snapshot| snapshot.value(node))
    {
        Ok(value) => {
            out_value.write(VtOptionalU64 {
                value: value.unwrap_or_default(),
                has_value: u8::from(value.is_some()),
                reserved: [0; 7],
            });
            VtStatus::Ok
        }
        Err(status) => status,
    }
}

unsafe extern "C" fn dictionary_transition(
    context: *mut c_void,
    node: u64,
    label: u64,
    out_child: *mut u64,
    out_found: *mut u8,
) -> u32 {
    dictionary_transition_status(context, node, label, out_child, out_found).to_raw()
}

unsafe fn dictionary_transition_status(
    context: *mut c_void,
    node: u64,
    label: u64,
    out_child: *mut u64,
    out_found: *mut u8,
) -> VtStatus {
    if context.is_null() || out_child.is_null() || out_found.is_null() {
        return VtStatus::NullPointer;
    }
    let context = &*context.cast::<ResourceContext>();
    match context
        .immutable()
        .and_then(|snapshot| snapshot.transition(node, label))
    {
        Ok(child) => {
            out_child.write(child.unwrap_or_default());
            out_found.write(u8::from(child.is_some()));
            VtStatus::Ok
        }
        Err(status) => status,
    }
}

unsafe extern "C" fn dictionary_edges(
    context: *mut c_void,
    node: u64,
    start: usize,
    out_edges: *mut VtDictionaryEdge,
    capacity: usize,
    out_written: *mut usize,
    out_total: *mut usize,
) -> u32 {
    dictionary_edges_status(
        context,
        node,
        start,
        out_edges,
        capacity,
        out_written,
        out_total,
    )
    .to_raw()
}

unsafe fn dictionary_edges_status(
    context: *mut c_void,
    node: u64,
    start: usize,
    out_edges: *mut VtDictionaryEdge,
    capacity: usize,
    out_written: *mut usize,
    out_total: *mut usize,
) -> VtStatus {
    if context.is_null()
        || out_written.is_null()
        || out_total.is_null()
        || (capacity != 0 && out_edges.is_null())
    {
        return VtStatus::NullPointer;
    }
    let context = &*context.cast::<ResourceContext>();
    if capacity > isize::MAX as usize / std::mem::size_of::<VtDictionaryEdge>() {
        return VtStatus::LimitExceeded;
    }
    let output = if capacity == 0 {
        &mut []
    } else {
        std::slice::from_raw_parts_mut(out_edges, capacity)
    };
    let (written, total) = match context
        .immutable()
        .and_then(|snapshot| snapshot.copy_edges(node, start, output))
    {
        Ok(page) => page,
        Err(status) => return status,
    };
    out_total.write(total);
    out_written.write(written);
    VtStatus::Ok
}

unsafe extern "C" fn dictionary_visit(
    context: *mut c_void,
    node: u64,
    start: usize,
    out_is_final: *mut u8,
    out_edges: *mut VtDictionaryEdge,
    capacity: usize,
    out_written: *mut usize,
    out_total: *mut usize,
) -> u32 {
    dictionary_visit_status(
        context,
        node,
        start,
        out_is_final,
        out_edges,
        capacity,
        out_written,
        out_total,
    )
    .to_raw()
}

// Keep the status-returning helper isomorphic to the public C callback. The
// eight fields are fixed by `VtDictionaryVisitVTable`, not an internal API
// design choice, and grouping them would obscure the boundary validation.
#[allow(clippy::too_many_arguments)]
unsafe fn dictionary_visit_status(
    context: *mut c_void,
    node: u64,
    start: usize,
    out_is_final: *mut u8,
    out_edges: *mut VtDictionaryEdge,
    capacity: usize,
    out_written: *mut usize,
    out_total: *mut usize,
) -> VtStatus {
    if context.is_null()
        || out_is_final.is_null()
        || out_written.is_null()
        || out_total.is_null()
        || (capacity != 0 && out_edges.is_null())
    {
        return VtStatus::NullPointer;
    }
    if capacity > isize::MAX as usize / std::mem::size_of::<VtDictionaryEdge>() {
        return VtStatus::LimitExceeded;
    }
    let output = if capacity == 0 {
        &mut []
    } else {
        std::slice::from_raw_parts_mut(out_edges, capacity)
    };
    let context = &*context.cast::<ResourceContext>();
    let (is_final, written, total) = match context
        .immutable()
        .and_then(|snapshot| snapshot.copy_node(node, start, output))
    {
        Ok(page) => page,
        Err(status) => return status,
    };
    out_is_final.write(u8::from(is_final));
    out_written.write(written);
    out_total.write(total);
    VtStatus::Ok
}

static RESOURCE_VTABLE: VtResourceVTable = VtResourceVTable {
    struct_size: std::mem::size_of::<VtResourceVTable>(),
    abi_version: VT_ABI_VERSION,
    reserved: 0,
    retain: Some(resource_retain),
    release: Some(resource_release),
    query_interface: Some(query_interface),
};

static DICTIONARY_VISIT_VTABLE: VtDictionaryVisitVTable = VtDictionaryVisitVTable {
    struct_size: std::mem::size_of::<VtDictionaryVisitVTable>(),
    interface_version: VT_DICTIONARY_VISIT_INTERFACE_VERSION,
    reserved: 0,
    node_visit: Some(dictionary_visit),
};

static DICTIONARY_GRAPH_VTABLE: VtDictionaryGraphVTable = VtDictionaryGraphVTable {
    struct_size: std::mem::size_of::<VtDictionaryGraphVTable>(),
    interface_version: VT_DICTIONARY_GRAPH_INTERFACE_VERSION,
    reserved: 0,
    graph: Some(dictionary_graph),
    node_value_u64: Some(dictionary_graph_value),
};

static SNAPSHOT_IDENTITY_VTABLE: VtSnapshotIdentityVTable = VtSnapshotIdentityVTable {
    struct_size: std::mem::size_of::<VtSnapshotIdentityVTable>(),
    interface_version: VT_SNAPSHOT_IDENTITY_INTERFACE_VERSION,
    reserved: 0,
    identity: Some(dictionary_snapshot_identity),
};

macro_rules! dictionary_vtable {
    ($name:ident, $domain:expr, $flags:expr) => {
        static $name: VtDictionaryVTable = VtDictionaryVTable {
            struct_size: std::mem::size_of::<VtDictionaryVTable>(),
            interface_version: VT_DICTIONARY_INTERFACE_VERSION,
            unit_domain: $domain,
            value_domain: VtValueDomain::OptionalU64,
            flags: $flags,
            snapshot: Some(dictionary_snapshot),
            root: Some(dictionary_root),
            len: Some(dictionary_len),
            node_is_final: Some(dictionary_is_final),
            node_value_u64: Some(dictionary_value),
            node_transition: Some(dictionary_transition),
            node_edges: Some(dictionary_edges),
        };
    };
}

dictionary_vtable!(
    BYTE_LIVE,
    VtUnitDomain::Byte,
    dictionary_flags::PARALLEL_REENTRANT
);
dictionary_vtable!(
    UNICODE_LIVE,
    VtUnitDomain::UnicodeScalar,
    dictionary_flags::PARALLEL_REENTRANT
);
dictionary_vtable!(
    U64_LIVE,
    VtUnitDomain::U64,
    dictionary_flags::PARALLEL_REENTRANT
);
dictionary_vtable!(
    BYTE_SUFFIX_LIVE,
    VtUnitDomain::Byte,
    dictionary_flags::PARALLEL_REENTRANT | dictionary_flags::SUFFIX_BASED
);
dictionary_vtable!(
    UNICODE_SUFFIX_LIVE,
    VtUnitDomain::UnicodeScalar,
    dictionary_flags::PARALLEL_REENTRANT | dictionary_flags::SUFFIX_BASED
);
dictionary_vtable!(
    BYTE_SNAPSHOT,
    VtUnitDomain::Byte,
    dictionary_flags::PARALLEL_REENTRANT | dictionary_flags::IMMUTABLE
);
dictionary_vtable!(
    UNICODE_SNAPSHOT,
    VtUnitDomain::UnicodeScalar,
    dictionary_flags::PARALLEL_REENTRANT | dictionary_flags::IMMUTABLE
);
dictionary_vtable!(
    U64_SNAPSHOT,
    VtUnitDomain::U64,
    dictionary_flags::PARALLEL_REENTRANT | dictionary_flags::IMMUTABLE
);
dictionary_vtable!(
    BYTE_SUFFIX_SNAPSHOT,
    VtUnitDomain::Byte,
    dictionary_flags::PARALLEL_REENTRANT
        | dictionary_flags::IMMUTABLE
        | dictionary_flags::SUFFIX_BASED
);
dictionary_vtable!(
    UNICODE_SUFFIX_SNAPSHOT,
    VtUnitDomain::UnicodeScalar,
    dictionary_flags::PARALLEL_REENTRANT
        | dictionary_flags::IMMUTABLE
        | dictionary_flags::SUFFIX_BASED
);

fn dictionary_vtable(domain: VtUnitDomain, flags: u64) -> *const VtDictionaryVTable {
    let immutable = flags & dictionary_flags::IMMUTABLE != 0;
    let suffix = flags & dictionary_flags::SUFFIX_BASED != 0;
    match (domain, immutable, suffix) {
        (VtUnitDomain::Byte, false, false) => &BYTE_LIVE,
        (VtUnitDomain::UnicodeScalar, false, false) => &UNICODE_LIVE,
        (VtUnitDomain::U64, false, _) => &U64_LIVE,
        (VtUnitDomain::Byte, false, true) => &BYTE_SUFFIX_LIVE,
        (VtUnitDomain::UnicodeScalar, false, true) => &UNICODE_SUFFIX_LIVE,
        (VtUnitDomain::Byte, true, false) => &BYTE_SNAPSHOT,
        (VtUnitDomain::UnicodeScalar, true, false) => &UNICODE_SNAPSHOT,
        (VtUnitDomain::U64, true, false) => &U64_SNAPSHOT,
        (VtUnitDomain::Byte, true, true) => &BYTE_SUFFIX_SNAPSHOT,
        (VtUnitDomain::UnicodeScalar, true, true) => &UNICODE_SUFFIX_SNAPSHOT,
        (VtUnitDomain::U64, true, true) => &U64_SNAPSHOT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    fn snapshot_edges(snapshot: &dyn SnapshotOps, node: u64) -> Result<Vec<(u64, u64)>, VtStatus> {
        let (_, total) = snapshot.copy_edges(node, 0, &mut [])?;
        let mut edges = vec![VtDictionaryEdge::default(); total];
        let (written, confirmed_total) = snapshot.copy_edges(node, 0, &mut edges)?;
        assert_eq!(confirmed_total, total);
        assert_eq!(written, total);
        Ok(edges
            .into_iter()
            .map(|edge| (edge.label, edge.node))
            .collect())
    }

    fn captured_resource(snapshot: Arc<dyn SnapshotOps>) -> OwnedDictionaryResource {
        OwnedDictionaryResource::new(ResourcePayload::Snapshot(snapshot))
    }

    fn algebra_terms(dictionary: &DynamicDawgBinding) -> Vec<(BindingTerm, Option<u64>)> {
        dictionary
            .resource()
            .entries()
            .map(|entry| {
                let entry = entry.expect("binding-owned snapshot traversal must succeed");
                (entry.term, entry.value)
            })
            .collect()
    }

    #[test]
    fn binding_profile_metadata_is_canonical_and_fail_closed() {
        assert_eq!(BindingUnitDomain::Byte.as_str(), "bytes");
        assert_eq!(BindingUnitDomain::UnicodeScalar.as_str(), "unicode-scalar");
        assert_eq!(BindingUnitDomain::U64.as_str(), "u64");
        assert_eq!(
            BindingProfileDescriptor::for_profile::<crate::Utf8>().kind,
            crate::ProfileKind::Utf8
        );
        assert_eq!(
            BindingProfileDescriptor::for_profile::<crate::Utf8>().binding_domain(),
            None
        );
        assert_eq!(
            BindingProfileDescriptor::for_profile::<crate::U64>().binding_domain(),
            Some(BindingUnitDomain::U64)
        );
        assert_eq!(
            BindingUnitDomain::from_profile_kind(crate::ProfileKind::Uleb128),
            None
        );
    }

    #[test]
    fn dictionary_algebra_is_snapshot_consistent_and_domain_safe() {
        let left = DynamicDawgBinding::new(BindingUnitDomain::UnicodeScalar);
        left.insert_text(b"cat", Some(3)).unwrap();
        left.insert_text(b"dog", None).unwrap();
        let right = DynamicDawgBinding::new(BindingUnitDomain::UnicodeScalar);
        right.insert_text(b"cat", Some(7)).unwrap();
        right.insert_text(b"eel", Some(11)).unwrap();

        let union = dictionary_algebra(
            &left.resource(),
            &right.resource(),
            BindingAlgebraOperation::Union,
            BindingValueMerge::LatticeJoin,
        )
        .unwrap();
        assert_eq!(
            algebra_terms(&union),
            vec![
                (BindingTerm::Unicode("cat".into()), Some(7)),
                (BindingTerm::Unicode("dog".into()), None),
                (BindingTerm::Unicode("eel".into()), Some(11)),
            ]
        );
        assert_eq!(
            algebra_terms(
                &dictionary_algebra(
                    &left.resource(),
                    &right.resource(),
                    BindingAlgebraOperation::Intersection,
                    BindingValueMerge::First,
                )
                .unwrap()
            ),
            vec![(BindingTerm::Unicode("cat".into()), Some(3))]
        );
        assert_eq!(
            algebra_terms(
                &dictionary_algebra(
                    &left.resource(),
                    &right.resource(),
                    BindingAlgebraOperation::Difference,
                    BindingValueMerge::First,
                )
                .unwrap()
            ),
            vec![(BindingTerm::Unicode("dog".into()), None)]
        );
        assert_eq!(
            algebra_terms(
                &dictionary_algebra(
                    &left.resource(),
                    &right.resource(),
                    BindingAlgebraOperation::SymmetricDifference,
                    BindingValueMerge::First,
                )
                .unwrap()
            ),
            vec![
                (BindingTerm::Unicode("dog".into()), None),
                (BindingTerm::Unicode("eel".into()), Some(11)),
            ]
        );

        left.insert_text(b"fox", Some(13)).unwrap();
        assert_eq!(algebra_terms(&union).len(), 3);

        let bytes = DynamicDawgBinding::new(BindingUnitDomain::Byte);
        assert_eq!(
            dictionary_algebra(
                &left.resource(),
                &bytes.resource(),
                BindingAlgebraOperation::Union,
                BindingValueMerge::First,
            )
            .unwrap_err(),
            BindingAlgebraError::DomainMismatch
        );
    }

    #[test]
    fn waiting_snapshot_does_not_close_writer_admission() {
        let memo = Arc::new(SnapshotMemo::new());
        let backend = Arc::new(DynamicBackend::new(BindingUnitDomain::U64));
        let admitted = memo.begin_mutation();
        let (snapshot_tx, snapshot_rx) = mpsc::channel();
        let (writer_tx, writer_rx) = mpsc::channel();

        std::thread::scope(|scope| {
            let snapshot_memo = Arc::clone(&memo);
            let snapshot_backend = Arc::clone(&backend);
            scope.spawn(move || {
                let snapshot =
                    snapshot_memo.get_or_create(|identity| snapshot_backend.snapshot(identity));
                snapshot_tx
                    .send(snapshot.identity())
                    .expect("snapshot receiver remains live");
            });

            let waiting_writer_memo = Arc::clone(&memo);
            scope.spawn(move || {
                let mutation = waiting_writer_memo.begin_mutation();
                writer_tx.send(()).expect("writer receiver remains live");
                mutation.finish(false);
            });
            writer_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("a waiting snapshot never closes writer admission");
            assert!(snapshot_rx.try_recv().is_err());

            admitted.finish(false);
            let identity = snapshot_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("snapshot completes once writers become quiescent");
            assert_eq!(identity.producer, memo.producer);
        });
        assert_eq!(memo.active_writers.load(Ordering::Acquire), 0);
    }

    #[test]
    fn abandoned_snapshot_pressure_is_bounded_and_consumed_by_one_writer() {
        let memo = SnapshotMemo::new();
        for _ in 0..(MAX_SNAPSHOT_BACKOFF_CREDITS * 4) {
            memo.request_snapshot_window();
        }
        assert_eq!(
            memo.snapshot_backoff_credits.load(Ordering::Acquire),
            MAX_SNAPSHOT_BACKOFF_CREDITS
        );

        let mutation = memo.begin_mutation();
        assert_eq!(memo.snapshot_backoff_credits.load(Ordering::Acquire), 0);
        assert_eq!(memo.active_writers.load(Ordering::Acquire), 1);
        mutation.finish(false);
        assert_eq!(memo.active_writers.load(Ordering::Acquire), 0);

        // No reader is required to withdraw an ownership token. Later writers
        // enter normally after the one bounded residual pause was consumed.
        memo.begin_mutation().finish(false);
        assert_eq!(memo.snapshot_backoff_credits.load(Ordering::Acquire), 0);
    }

    #[test]
    fn active_first_validation_rejects_writer_completing_between_final_loads() {
        let memo = Arc::new(SnapshotMemo::new());
        let expected_revision = memo.revision.load(Ordering::Acquire);
        let mutation = memo.begin_mutation();
        let (active_loaded_tx, active_loaded_rx) = mpsc::channel();
        let (writer_finished_tx, writer_finished_rx) = mpsc::channel();

        std::thread::scope(|scope| {
            let validating_memo = Arc::clone(&memo);
            let validator = scope.spawn(move || {
                validating_memo.capture_is_current_after(expected_revision, || {
                    active_loaded_tx
                        .send(())
                        .expect("validation observer remains live");
                    writer_finished_rx
                        .recv()
                        .expect("writer completion signal remains live");
                })
            });

            active_loaded_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("validator loads the active count while the writer is present");
            mutation.finish(true);
            writer_finished_tx.send(()).expect("validator remains live");
            assert!(!validator.join().expect("validator does not panic"));
        });
        assert_eq!(memo.active_writers.load(Ordering::Acquire), 0);
        assert_eq!(memo.revision.load(Ordering::Acquire), 1);
    }

    #[test]
    fn concurrent_cold_snapshotters_return_one_revision_with_bounded_takeover() {
        const SNAPSHOTTERS: usize = 16;
        let memo = Arc::new(SnapshotMemo::new());
        let backend = Arc::new(DynamicBackend::new(BindingUnitDomain::U64));
        let barrier = Arc::new(std::sync::Barrier::new(SNAPSHOTTERS));
        let constructions = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let snapshots = std::thread::scope(|scope| {
            let mut threads = Vec::with_capacity(SNAPSHOTTERS);
            for _ in 0..SNAPSHOTTERS {
                let memo = Arc::clone(&memo);
                let backend = Arc::clone(&backend);
                let barrier = Arc::clone(&barrier);
                let constructions = Arc::clone(&constructions);
                threads.push(scope.spawn(move || {
                    barrier.wait();
                    memo.get_or_create(|identity| {
                        constructions.fetch_add(1, Ordering::Relaxed);
                        backend.snapshot(identity)
                    })
                }));
            }
            threads
                .into_iter()
                .map(|thread| thread.join().expect("cold snapshotter does not panic"))
                .collect::<Vec<_>>()
        });

        let construction_count = constructions.load(Ordering::Relaxed);
        assert!(
            (1..=SNAPSHOTTERS).contains(&construction_count),
            "each contender constructs at most one same-revision generation"
        );

        let expected_identity = snapshots[0].identity();
        let expected_root = snapshots[0].root();
        let expected_len = snapshots[0].len();
        let expected_edges = snapshot_edges(snapshots[0].as_ref(), expected_root)
            .expect("reference snapshot root remains traversable");
        for snapshot in &snapshots {
            assert_eq!(snapshot.identity(), expected_identity);
            assert_eq!(snapshot.root(), expected_root);
            assert_eq!(snapshot.len(), expected_len);
            assert_eq!(
                snapshot_edges(snapshot.as_ref(), snapshot.root())
                    .expect("same-revision snapshot root remains traversable"),
                expected_edges
            );
        }

        let warmed = memo.get_or_create(|identity| backend.snapshot(identity));
        assert_eq!(warmed.identity(), expected_identity);
        assert!(
            snapshots
                .iter()
                .any(|snapshot| Arc::ptr_eq(snapshot, &warmed)),
            "the warmed generation is one of the successfully returned generations"
        );
    }

    #[test]
    fn stalled_cold_snapshot_initializer_cannot_convoy_other_snapshotters() {
        let memo = Arc::new(SnapshotMemo::new());
        let backend = Arc::new(DynamicBackend::new(BindingUnitDomain::Byte));
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();

        std::thread::scope(|scope| {
            let stalled_memo = Arc::clone(&memo);
            let stalled_backend = Arc::clone(&backend);
            let stalled = scope.spawn(move || {
                let mut first_call = true;
                stalled_memo.get_or_create(|identity| {
                    if first_call {
                        first_call = false;
                        entered_tx
                            .send(())
                            .expect("initializer observer remains live");
                        release_rx.recv().expect("initializer release remains live");
                    }
                    stalled_backend.snapshot(identity)
                })
            });

            entered_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("first snapshotter reaches its cold initializer");
            let winner = memo.get_or_create(|identity| backend.snapshot(identity));
            assert_eq!(winner.identity().revision, 0);
            let mutation = memo.begin_mutation();
            assert_eq!(memo.active_writers.load(Ordering::Acquire), 1);
            mutation.finish(true);
            assert_eq!(memo.revision.load(Ordering::Acquire), 1);
            release_tx
                .send(())
                .expect("stalled initializer remains live");
            let stalled_result = stalled.join().expect("stalled snapshotter does not panic");
            assert_eq!(stalled_result.identity().revision, 1);
            assert!(!Arc::ptr_eq(&winner, &stalled_result));
            let warmed = memo.get_or_create(|identity| backend.snapshot(identity));
            assert!(Arc::ptr_eq(&stalled_result, &warmed));
        });
    }

    #[test]
    fn snapshot_and_mutation_panics_leave_progress_state_recoverable() {
        let memo = SnapshotMemo::new();
        let snapshot_panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            memo.get_or_create(|_| panic!("injected snapshot construction panic"));
        }));
        assert!(snapshot_panic.is_err());
        assert_eq!(memo.active_writers.load(Ordering::Acquire), 0);
        let abandoned = memo
            .cached
            .load_full()
            .expect("panicking initializer leaves a replaceable generation");
        assert!(abandoned.snapshot.get().is_none());

        let backend = DynamicBackend::new(BindingUnitDomain::UnicodeScalar);
        let recovered_same_revision = memo.get_or_create(|identity| backend.snapshot(identity));
        assert_eq!(recovered_same_revision.identity().revision, 0);

        let mutation_panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _mutation = memo.begin_mutation();
            panic!("injected backend mutation panic");
        }));
        assert!(mutation_panic.is_err());
        assert_eq!(memo.active_writers.load(Ordering::Acquire), 0);
        assert_eq!(memo.revision.load(Ordering::Acquire), 1);
        assert!(memo.cached.load_full().is_none());

        let recovered = memo.get_or_create(|identity| backend.snapshot(identity));
        assert_eq!(recovered.identity().revision, 1);
    }

    #[test]
    fn byte_unicode_and_u64_producers_share_revision_memo_semantics() {
        for domain in [
            BindingUnitDomain::Byte,
            BindingUnitDomain::UnicodeScalar,
            BindingUnitDomain::U64,
        ] {
            let dictionary = DynamicDawgBinding::new(domain);
            match domain {
                BindingUnitDomain::Byte => {
                    dictionary.insert_text(b"alpha", Some(1)).unwrap();
                }
                BindingUnitDomain::UnicodeScalar => {
                    dictionary.insert_text("άλφα".as_bytes(), Some(1)).unwrap();
                }
                BindingUnitDomain::U64 => {
                    dictionary.insert_u64(&[1, 2, 3], Some(1)).unwrap();
                }
            }

            let resource = dictionary.resource();
            let live = unsafe { &*resource.raw.context.cast::<ResourceContext>() };
            let first = live.snapshot();
            let warmed = live.snapshot();
            assert!(Arc::ptr_eq(&first, &warmed));
            assert_eq!(first.len(), Some(1));

            match domain {
                BindingUnitDomain::Byte => {
                    dictionary.insert_text(b"beta", Some(2)).unwrap();
                }
                BindingUnitDomain::UnicodeScalar => {
                    dictionary.insert_text("βήτα".as_bytes(), Some(2)).unwrap();
                }
                BindingUnitDomain::U64 => {
                    dictionary.insert_u64(&[4, 5, 6], Some(2)).unwrap();
                }
            }

            let fresh = live.snapshot();
            assert_eq!(fresh.identity().producer, first.identity().producer);
            assert_eq!(fresh.identity().revision, first.identity().revision + 1);
            assert_eq!(first.len(), Some(1));
            assert_eq!(fresh.len(), Some(2));

            dictionary.compact();
            let compacted = live.snapshot();
            assert_eq!(compacted.identity().revision, fresh.identity().revision + 1);
            assert_eq!(compacted.len(), Some(2));
            assert_eq!(fresh.len(), Some(2), "pre-compact snapshot is retained");

            dictionary.clear();
            let cleared = live.snapshot();
            assert_eq!(
                cleared.identity().revision,
                compacted.identity().revision + 1
            );
            assert_eq!(cleared.len(), Some(0));
            assert_eq!(compacted.len(), Some(2), "pre-clear snapshot is retained");
            assert!(Arc::ptr_eq(&cleared, &live.snapshot()));
        }
    }

    #[test]
    fn byte_dat_snapshot_uses_validated_native_cursor_tokens_end_to_end() {
        let dictionary = DoubleArrayTrieBinding::from_byte_terms([
            ("", Some(10)),
            ("car", Some(20)),
            ("cat", None),
        ]);
        let resource = dictionary.resource();
        let live = unsafe { &*resource.raw.context.cast::<ResourceContext>() };
        let snapshot = live.snapshot();
        assert_eq!(snapshot.root(), 2, "byte DAT state one is cursor token two");
        assert_eq!(snapshot.len(), Some(3));
        assert!(snapshot.graph().is_none());
        assert!(snapshot.is_final(snapshot.root()).unwrap());
        assert_eq!(snapshot.value(snapshot.root()).unwrap(), Some(10));

        let c = snapshot
            .transition(snapshot.root(), u64::from(b'c'))
            .unwrap()
            .expect("c edge");
        let a = snapshot
            .transition(c, u64::from(b'a'))
            .unwrap()
            .expect("a edge");
        let t = snapshot
            .transition(a, u64::from(b't'))
            .unwrap()
            .expect("t edge");
        assert!(snapshot.is_final(t).unwrap());
        assert_eq!(
            snapshot.value(t).unwrap(),
            None,
            "terminal may be valueless"
        );
        assert_eq!(snapshot.transition(a, u64::from(b'z')).unwrap(), None);
        assert_eq!(snapshot.transition(a, 256).unwrap(), None);

        let mut page = [VtDictionaryEdge::default(); 1];
        let (is_final, written, total) = snapshot
            .copy_node(a, 0, &mut page)
            .expect("first one-edge page");
        assert!(!is_final);
        assert_eq!((written, total), (1, 2));
        let first = page[0];
        let (_, written, confirmed_total) = snapshot
            .copy_node(a, 1, &mut page)
            .expect("second one-edge page");
        assert_eq!((written, confirmed_total), (1, 2));
        assert_ne!(page[0].label, first.label);
        assert_eq!(snapshot.copy_node(a, 2, &mut page).unwrap().1, 0);
        assert_eq!(snapshot.copy_node(a, 3, &mut page).unwrap().1, 0);

        for invalid in [0, 1, u64::MAX] {
            let sentinel = VtDictionaryEdge {
                label: 0xfeed,
                node: 0xbeef,
            };
            let mut untouched = [sentinel];
            assert_eq!(
                snapshot.copy_node(invalid, 0, &mut untouched),
                Err(VtStatus::InvalidArgument)
            );
            assert_eq!(untouched[0].label, sentinel.label);
            assert_eq!(untouched[0].node, sentinel.node);
        }

        let captured = captured_resource(Arc::clone(&snapshot));
        let mut abi_root = 0;
        assert_eq!(
            unsafe { dictionary_root_status(captured.raw.context, &mut abi_root) },
            VtStatus::Ok
        );
        assert_eq!(abi_root, snapshot.root());
        let captured_context = unsafe { &*captured.raw.context.cast::<ResourceContext>() };
        let nested = captured_context.snapshot();
        assert_eq!(nested.root(), snapshot.root());
        assert_eq!(nested.identity(), snapshot.identity());
        drop(resource);
        assert!(
            nested.is_final(t).unwrap(),
            "retained snapshot owns DAT arrays"
        );
    }

    #[test]
    fn unicode_dat_cursor_snapshot_pages_sparse_high_degree_roots() {
        let entries =
            std::iter::once((String::new(), Some(99))).chain((0..300_u32).map(|offset| {
                let scalar = char::from_u32(0x1_000 + offset).expect("test scalar");
                (scalar.to_string(), Some(u64::from(offset)))
            }));
        let dictionary = DoubleArrayTrieBinding::from_unicode_terms(entries);
        let resource = dictionary.resource();
        let live = unsafe { &*resource.raw.context.cast::<ResourceContext>() };
        let snapshot = live.snapshot();
        assert_eq!(
            snapshot.root(),
            1,
            "Unicode DAT state zero is cursor token one"
        );
        assert!(snapshot.is_final(1).unwrap());
        assert_eq!(snapshot.value(1).unwrap(), Some(99));

        let mut page = [VtDictionaryEdge::default(); 32];
        assert_eq!(snapshot.copy_node(1, 0, &mut []).unwrap(), (true, 0, 300));
        let (is_final, written, total) = snapshot.copy_node(1, 256, &mut page).unwrap();
        assert!(is_final);
        assert_eq!((written, total), (32, 300));
        let (_, tail_written, tail_total) = snapshot.copy_node(1, 288, &mut page).unwrap();
        assert_eq!((tail_written, tail_total), (12, 300));
        assert!(page[..tail_written]
            .windows(2)
            .all(|pair| pair[0].label < pair[1].label));

        let wanted = u64::from(0x1_000_u32 + 299);
        let child = snapshot
            .transition(1, wanted)
            .unwrap()
            .expect("high Unicode transition");
        assert!(snapshot.is_final(child).unwrap());
        assert_eq!(snapshot.value(child).unwrap(), Some(299));
        assert_eq!(snapshot.transition(1, 0x11_0000).unwrap(), None);
        assert_eq!(snapshot.transition(1, 0xd800).unwrap(), None);
        assert_eq!(snapshot.is_final(0), Err(VtStatus::InvalidArgument));
        assert_eq!(snapshot.is_final(2), Err(VtStatus::InvalidArgument));
        assert_eq!(snapshot.is_final(u64::MAX), Err(VtStatus::InvalidArgument));
    }

    #[test]
    fn resource_snapshot_keeps_the_query_start_revision() {
        let dictionary = DynamicDawgBinding::new(BindingUnitDomain::UnicodeScalar);
        dictionary.insert_text(b"cat", Some(1)).unwrap();
        dictionary.insert_text(b"cot", Some(2)).unwrap();
        let resource = dictionary.resource();

        let live = unsafe { &*resource.raw.context.cast::<ResourceContext>() };
        let snapshot = live.snapshot();
        dictionary.remove_text(b"cot").unwrap();
        dictionary.insert_text(b"cut", Some(3)).unwrap();

        assert_eq!(snapshot.len(), Some(2));
        assert_eq!(dictionary.len(), 2);
        let root_edges = snapshot_edges(&*snapshot, 0).unwrap();
        assert_eq!(root_edges.len(), 1);
    }

    #[test]
    fn empty_dynamic_batches_use_minimal_builders_in_every_unit_domain() {
        let bytes = DynamicDawgBinding::new(BindingUnitDomain::Byte);
        assert_eq!(
            bytes
                .insert_text_batch([
                    (b"cb".as_slice(), None),
                    (b"ab".as_slice(), Some(1)),
                    (b"ab".as_slice(), None),
                ])
                .unwrap(),
            2
        );
        assert_eq!(bytes.value_text(b"ab").unwrap(), Some(None));
        assert_eq!(bytes.value_text(b"cb").unwrap(), Some(None));

        let unicode = DynamicDawgBinding::new(BindingUnitDomain::UnicodeScalar);
        assert_eq!(
            unicode
                .insert_text_batch([("γβ".as_bytes(), None), ("αβ".as_bytes(), Some(7)),])
                .unwrap(),
            2
        );
        assert_eq!(unicode.value_text("αβ".as_bytes()).unwrap(), Some(Some(7)));

        let tokens = DynamicDawgBinding::new(BindingUnitDomain::U64);
        assert_eq!(
            tokens
                .insert_u64_batch([
                    ([2_u64, 9].as_slice(), None),
                    ([1_u64, 9].as_slice(), Some(11)),
                ])
                .unwrap(),
            2
        );
        assert_eq!(tokens.value_u64(&[1, 9]).unwrap(), Some(Some(11)));

        let DynamicBackend::Byte(byte_dawg) = &bytes.shared.backend else {
            panic!("byte binding changed domains");
        };
        assert_eq!(byte_dawg.node_count(), 3);

        let DynamicBackend::Unicode(unicode_dawg) = &unicode.shared.backend else {
            panic!("Unicode binding changed domains");
        };
        // The two suffix nodes cannot merge because one final carries a value
        // and the other is valueless: finality plus value is part of the
        // minimized-state signature.
        assert_eq!(unicode_dawg.node_count(), 5);

        let DynamicBackend::U64(token_dawg) = &tokens.shared.backend else {
            panic!("u64 binding changed domains");
        };
        assert_eq!(token_dawg.node_count(), 5);
    }

    #[test]
    fn dynamic_batch_replacement_preserves_snapshots_and_incremental_updates() {
        let dictionary = DynamicDawgBinding::new(BindingUnitDomain::UnicodeScalar);
        let resource = dictionary.resource();
        let live = unsafe { &*resource.raw.context.cast::<ResourceContext>() };
        let empty_snapshot = live.snapshot();

        assert_eq!(
            dictionary
                .insert_text_batch([(b"cat".as_slice(), Some(5)), (b"cot".as_slice(), None),])
                .unwrap(),
            2
        );
        assert_eq!(empty_snapshot.len(), Some(0));
        assert_eq!(dictionary.len(), 2);

        // A later batch falls back to ordinary update semantics: one new term,
        // and an existing valued term becomes explicitly valueless.
        assert_eq!(
            dictionary
                .insert_text_batch([(b"cat".as_slice(), None), (b"cut".as_slice(), Some(9)),])
                .unwrap(),
            1
        );
        assert_eq!(dictionary.value_text(b"cat").unwrap(), Some(None));
        assert_eq!(dictionary.value_text(b"cut").unwrap(), Some(Some(9)));
    }

    #[test]
    fn invalid_unicode_empty_batch_does_not_publish_a_partial_dictionary() {
        let dictionary = DynamicDawgBinding::new(BindingUnitDomain::UnicodeScalar);
        let resource = dictionary.resource();
        let live = unsafe { &*resource.raw.context.cast::<ResourceContext>() };
        let before = live.snapshot();
        let invalid = [0xff_u8];
        assert_eq!(
            dictionary
                .insert_text_batch([(b"valid".as_slice(), None), (invalid.as_slice(), None),]),
            Err(BindingError::InvalidUtf8)
        );
        assert!(dictionary.is_empty());
        let after = live.snapshot();
        assert!(Arc::ptr_eq(&before, &after));
        assert_eq!(after.identity().revision, 0);

        assert_eq!(
            dictionary.insert_u64(&[1], None),
            Err(BindingError::DomainMismatch)
        );
        assert!(Arc::ptr_eq(&after, &live.snapshot()));
    }

    #[test]
    fn concurrent_empty_batch_and_insert_preserve_union_in_every_unit_domain() {
        let bytes = Arc::new(DynamicDawgBinding::new(BindingUnitDomain::Byte));
        let byte_barrier = Arc::new(std::sync::Barrier::new(3));
        std::thread::scope(|scope| {
            let dictionary = Arc::clone(&bytes);
            let barrier = Arc::clone(&byte_barrier);
            let batch = scope.spawn(move || {
                barrier.wait();
                dictionary
                    .insert_text_batch([
                        (b"batch-a".as_slice(), Some(1)),
                        (b"batch-b".as_slice(), None),
                    ])
                    .unwrap()
            });
            let dictionary = Arc::clone(&bytes);
            let barrier = Arc::clone(&byte_barrier);
            let insert = scope.spawn(move || {
                barrier.wait();
                dictionary.insert_text(b"single", Some(3)).unwrap()
            });
            byte_barrier.wait();
            batch.join().unwrap();
            insert.join().unwrap();
        });
        assert_eq!(bytes.len(), 3);
        assert_eq!(bytes.value_text(b"batch-a").unwrap(), Some(Some(1)));
        assert_eq!(bytes.value_text(b"batch-b").unwrap(), Some(None));
        assert_eq!(bytes.value_text(b"single").unwrap(), Some(Some(3)));

        let unicode = Arc::new(DynamicDawgBinding::new(BindingUnitDomain::UnicodeScalar));
        let unicode_barrier = Arc::new(std::sync::Barrier::new(3));
        std::thread::scope(|scope| {
            let dictionary = Arc::clone(&unicode);
            let barrier = Arc::clone(&unicode_barrier);
            let batch = scope.spawn(move || {
                barrier.wait();
                dictionary
                    .insert_text_batch([("άλφα".as_bytes(), Some(1)), ("βήτα".as_bytes(), None)])
                    .unwrap()
            });
            let dictionary = Arc::clone(&unicode);
            let barrier = Arc::clone(&unicode_barrier);
            let insert = scope.spawn(move || {
                barrier.wait();
                dictionary.insert_text("γάμμα".as_bytes(), Some(3)).unwrap()
            });
            unicode_barrier.wait();
            batch.join().unwrap();
            insert.join().unwrap();
        });
        assert_eq!(unicode.len(), 3);
        assert_eq!(
            unicode.value_text("άλφα".as_bytes()).unwrap(),
            Some(Some(1))
        );
        assert_eq!(unicode.value_text("βήτα".as_bytes()).unwrap(), Some(None));
        assert_eq!(
            unicode.value_text("γάμμα".as_bytes()).unwrap(),
            Some(Some(3))
        );

        let tokens = Arc::new(DynamicDawgBinding::new(BindingUnitDomain::U64));
        let token_barrier = Arc::new(std::sync::Barrier::new(3));
        std::thread::scope(|scope| {
            let dictionary = Arc::clone(&tokens);
            let barrier = Arc::clone(&token_barrier);
            let batch = scope.spawn(move || {
                barrier.wait();
                dictionary
                    .insert_u64_batch([
                        ([1_u64, 2].as_slice(), Some(1)),
                        ([3_u64, 4].as_slice(), None),
                    ])
                    .unwrap()
            });
            let dictionary = Arc::clone(&tokens);
            let barrier = Arc::clone(&token_barrier);
            let insert = scope.spawn(move || {
                barrier.wait();
                dictionary.insert_u64(&[5, 6], Some(3)).unwrap()
            });
            token_barrier.wait();
            batch.join().unwrap();
            insert.join().unwrap();
        });
        assert_eq!(tokens.len(), 3);
        assert_eq!(tokens.value_u64(&[1, 2]).unwrap(), Some(Some(1)));
        assert_eq!(tokens.value_u64(&[3, 4]).unwrap(), Some(None));
        assert_eq!(tokens.value_u64(&[5, 6]).unwrap(), Some(Some(3)));
    }

    #[test]
    fn every_project_owned_resource_is_parallel_and_reentrant() {
        let dynamic = DynamicDawgBinding::new(BindingUnitDomain::UnicodeScalar);
        let dynamic_resource = dynamic.resource();
        let live = unsafe { &*dynamic_resource.raw.context.cast::<ResourceContext>() };
        assert_ne!(live.flags() & dictionary_flags::PARALLEL_REENTRANT, 0);

        for &(domain, flags) in &[
            (VtUnitDomain::Byte, dictionary_flags::PARALLEL_REENTRANT),
            (
                VtUnitDomain::UnicodeScalar,
                dictionary_flags::PARALLEL_REENTRANT,
            ),
            (VtUnitDomain::U64, dictionary_flags::PARALLEL_REENTRANT),
            (
                VtUnitDomain::UnicodeScalar,
                dictionary_flags::PARALLEL_REENTRANT
                    | dictionary_flags::IMMUTABLE
                    | dictionary_flags::SUFFIX_BASED,
            ),
        ] {
            let vtable = unsafe { &*dictionary_vtable(domain, flags) };
            assert_ne!(
                vtable.flags & dictionary_flags::PARALLEL_REENTRANT,
                0,
                "project-owned vtable did not advertise parallel/reentrant access"
            );
        }
    }

    // -----------------------------------------------------------------
    // W2 producer-invariant extensions. The tests below reach the internal
    // `Arc<ResourceContext>` that realizes the ABI reference counter, which
    // no consumer can do across the boundary — this is the numeric half of
    // the LDICT-LIFE-1 balance law (the behavioural half lives in
    // tests/ffi_concurrent_snapshot_stress.rs).
    // -----------------------------------------------------------------

    /// Observe the realized reference count of a resource context.
    unsafe fn context_strong_count(raw: VtResource) -> usize {
        let context = raw.context.cast::<ResourceContext>();
        Arc::increment_strong_count(context);
        let probe = Arc::from_raw(context);
        let count = Arc::strong_count(&probe) - 1;
        drop(probe);
        count
    }

    /// A `Weak` observer that outlives every ABI retain of the context.
    unsafe fn context_weak_observer(raw: VtResource) -> std::sync::Weak<ResourceContext> {
        let context = raw.context.cast::<ResourceContext>();
        Arc::increment_strong_count(context);
        let probe = Arc::from_raw(context);
        let observer = Arc::downgrade(&probe);
        drop(probe);
        observer
    }

    /// INVARIANT-HOOK: LDICT-LIFE-1 (numeric): each `resource()` borrow is
    /// an independent context owning exactly one retain; copying the two
    /// words is not a retain; vtable retain/release move the counter by
    /// exactly one; dropping the owner destroys the context exactly once.
    #[test]
    fn owned_resources_balance_their_retains_exactly() {
        let dictionary = DynamicDawgBinding::new(BindingUnitDomain::UnicodeScalar);
        dictionary
            .insert_text(b"cat", Some(1))
            .expect("insert must succeed");

        let first = dictionary.resource();
        let second = dictionary.resource();
        assert!(
            !std::ptr::eq(first.raw.context, second.raw.context),
            "each borrow mints an independent context"
        );
        assert_eq!(unsafe { context_strong_count(first.as_raw()) }, 1);
        assert_eq!(unsafe { context_strong_count(second.as_raw()) }, 1);

        // Copying the two words never moves the counter.
        let copied = first.as_raw();
        let copied_again = copied;
        assert_eq!(unsafe { context_strong_count(copied_again) }, 1);

        // Vtable retain/release move it by exactly one.
        unsafe { resource_retain(copied.context) };
        assert_eq!(unsafe { context_strong_count(copied) }, 2);
        unsafe { resource_release(copied.context) };
        assert_eq!(unsafe { context_strong_count(copied) }, 1);

        // retain/release of NULL are documented no-ops.
        unsafe { resource_retain(std::ptr::null_mut()) };
        unsafe { resource_release(std::ptr::null_mut()) };

        // Dropping the owner is the final release: the context dies once.
        let observer = unsafe { context_weak_observer(first.as_raw()) };
        drop(first);
        assert_eq!(observer.strong_count(), 0, "drop released the last retain");
        assert!(
            observer.upgrade().is_none(),
            "context destroyed exactly once"
        );
        // The sibling context is untouched.
        assert_eq!(unsafe { context_strong_count(second.as_raw()) }, 1);
    }

    /// INVARIANT-HOOK: LDICT-LIFE-1 + LDICT-SNAP-3: `dictionary_snapshot`
    /// transfers exactly one owned retain to the caller (the `mem::forget`
    /// in the producer is the ownership handoff, not a leak), for captures
    /// of live resources and of snapshots alike.
    #[test]
    fn dictionary_snapshot_transfers_exactly_one_owned_retain() {
        let dictionary = DynamicDawgBinding::new(BindingUnitDomain::UnicodeScalar);
        dictionary
            .insert_text(b"cot", Some(2))
            .expect("insert must succeed");
        let resource = dictionary.resource();

        let mut captured = VtResource::NULL;
        let status = unsafe { dictionary_snapshot(resource.raw.context, &mut captured) };
        assert_eq!(status, VtStatus::Ok.to_raw());
        assert!(!captured.context.is_null());
        assert_eq!(unsafe { context_strong_count(captured) }, 1);
        // The source context's count is unchanged by the capture.
        assert_eq!(unsafe { context_strong_count(resource.as_raw()) }, 1);

        // Snapshot-of-snapshot: a NEW context (shared arena), one retain.
        let mut nested = VtResource::NULL;
        let status = unsafe { dictionary_snapshot(captured.context, &mut nested) };
        assert_eq!(status, VtStatus::Ok.to_raw());
        assert!(
            !std::ptr::eq(nested.context, captured.context),
            "snapshot-of-snapshot mints a fresh context"
        );
        assert_eq!(unsafe { context_strong_count(nested) }, 1);

        // Releasing the transferred retains destroys each context once.
        let captured_observer = unsafe { context_weak_observer(captured) };
        let nested_observer = unsafe { context_weak_observer(nested) };
        unsafe { resource_release(nested.context) };
        assert_eq!(nested_observer.strong_count(), 0);
        unsafe { resource_release(captured.context) };
        assert_eq!(captured_observer.strong_count(), 0);
    }

    #[test]
    fn dynamic_snapshot_graph_is_stable_and_live_resources_do_not_advertise_it() {
        let dictionary = DynamicDawgBinding::new(BindingUnitDomain::UnicodeScalar);
        dictionary.insert_text(b"a", Some(7)).unwrap();
        dictionary.insert_text("é".as_bytes(), None).unwrap();
        let resource = dictionary.resource();

        let query = unsafe { (*resource.raw.vtable).query_interface.unwrap() };
        let mut live_vtable = std::ptr::without_provenance::<c_void>(1);
        assert_eq!(
            unsafe {
                query(
                    resource.raw.context,
                    &VT_DICTIONARY_GRAPH_INTERFACE_ID,
                    VT_DICTIONARY_GRAPH_INTERFACE_VERSION,
                    &mut live_vtable,
                )
            },
            VtStatus::Unsupported.to_raw()
        );
        assert_eq!(
            live_vtable,
            std::ptr::without_provenance::<c_void>(1),
            "failed negotiation must not modify the output slot"
        );

        let mut captured = VtResource::NULL;
        assert_eq!(
            unsafe { dictionary_snapshot(resource.raw.context, &mut captured) },
            VtStatus::Ok.to_raw()
        );
        let snapshot_query = unsafe { (*captured.vtable).query_interface.unwrap() };
        let mut graph_vtable = std::ptr::null();
        assert_eq!(
            unsafe {
                snapshot_query(
                    captured.context,
                    &VT_DICTIONARY_GRAPH_INTERFACE_ID,
                    VT_DICTIONARY_GRAPH_INTERFACE_VERSION,
                    &mut graph_vtable,
                )
            },
            VtStatus::Ok.to_raw()
        );
        assert_eq!(
            graph_vtable,
            (&DICTIONARY_GRAPH_VTABLE as *const VtDictionaryGraphVTable).cast()
        );
        let graph_vtable = unsafe { &*graph_vtable.cast::<VtDictionaryGraphVTable>() };
        let graph = graph_vtable.graph.unwrap();
        let mut first = VtDictionaryGraphView::default();
        assert_eq!(
            unsafe { graph(captured.context, &mut first) },
            VtStatus::Ok.to_raw()
        );
        assert!(first.node_count >= 3);
        assert!(first.edge_count >= 2);
        assert!(!first.nodes.is_null());
        assert!(!first.edges.is_null());

        // Source mutation publishes another revision but cannot alter any
        // pointer, count, root, label, or value cursor in this retained view.
        dictionary.insert_text(b"z", Some(9)).unwrap();
        let mut second = VtDictionaryGraphView::default();
        assert_eq!(
            unsafe { graph(captured.context, &mut second) },
            VtStatus::Ok.to_raw()
        );
        assert_eq!(first.nodes, second.nodes);
        assert_eq!(first.node_count, second.node_count);
        assert_eq!(first.edges, second.edges);
        assert_eq!(first.edge_count, second.edge_count);
        assert_eq!(first.root, second.root);

        let nodes = unsafe { std::slice::from_raw_parts(first.nodes, first.node_count) };
        let value = graph_vtable.node_value_u64.unwrap();
        let mut output = VtOptionalU64::default();
        let mut observed_seven = false;
        for node in nodes.iter().filter(|node| node.is_final == 1) {
            assert_ne!(node.value_cursor, 0);
            assert_eq!(
                unsafe { value(captured.context, node.value_cursor, &mut output) },
                VtStatus::Ok.to_raw()
            );
            assert!(output.has_value <= 1);
            assert_eq!(output.reserved, [0; 7]);
            observed_seven |= output.has_value == 1 && output.value == 7;
        }
        assert!(observed_seven, "graph value cursors preserve mapped values");
        assert_eq!(
            unsafe { value(captured.context, 0, &mut output) },
            VtStatus::InvalidArgument.to_raw()
        );
        assert_eq!(
            unsafe { value(captured.context, u64::MAX, &mut output) },
            VtStatus::InvalidArgument.to_raw()
        );

        unsafe { resource_release(captured.context) };
    }

    /// ABI-local node identifiers are stable within one snapshot (repeated
    /// enumeration and transition agree) and independent across snapshots
    /// (a later snapshot's ids neither move nor validate an earlier one's
    /// id space).
    ///
    /// INVARIANT-HOOK: LDICT-ARENA-1..5 — the executable mirror of the arena
    /// laws proved in
    /// formal-verification/rocq/Spec/AbiTraversalSnapshotSpec.v.
    #[test]
    fn node_ids_are_stable_within_and_independent_across_snapshots() {
        let dictionary = DynamicDawgBinding::new(BindingUnitDomain::UnicodeScalar);
        dictionary
            .insert_text(b"ab", Some(1))
            .expect("insert must succeed");
        dictionary
            .insert_text(b"ac", Some(2))
            .expect("insert must succeed");
        let resource = dictionary.resource();
        let live = unsafe { &*resource.raw.context.cast::<ResourceContext>() };

        let first_snapshot = live.snapshot();
        let root_edges = snapshot_edges(&*first_snapshot, 0).expect("root edges");
        assert_eq!(root_edges.len(), 1, "both terms share the 'a' prefix");
        // Stability: re-enumeration returns identical (label, id) pairs.
        assert_eq!(
            snapshot_edges(&*first_snapshot, 0).expect("root edges again"),
            root_edges
        );
        // Agreement: transition resolves every listed edge to the same id.
        for (label, child) in &root_edges {
            assert_eq!(
                first_snapshot.transition(0, *label).expect("transition"),
                Some(*child)
            );
        }
        // Materialize one level deeper; ids stay stable there too.
        let a_node = root_edges[0].1;
        let deeper = snapshot_edges(&*first_snapshot, a_node).expect("deeper edges");
        assert_eq!(deeper.len(), 2, "'b' and 'c' leaves");
        assert_eq!(
            snapshot_edges(&*first_snapshot, a_node).expect("deeper again"),
            deeper
        );

        // Mutate, then capture a second snapshot: its id space is its own.
        dictionary
            .insert_text(b"zz", Some(3))
            .expect("insert must succeed");
        let second_snapshot = live.snapshot();
        let second_root = snapshot_edges(&*second_snapshot, 0).expect("second root edges");
        assert_eq!(second_root.len(), 2, "'a' and 'z' branches");
        // The first snapshot is untouched by the second's materialization.
        assert_eq!(
            snapshot_edges(&*first_snapshot, 0).expect("still stable"),
            root_edges
        );
        // An id far beyond the first snapshot's arena is invalid THERE,
        // regardless of what any other snapshot materialized.
        assert_eq!(
            first_snapshot.is_final(10_000),
            Err(VtStatus::InvalidArgument)
        );
        assert_eq!(first_snapshot.value(10_000), Err(VtStatus::InvalidArgument));
        assert_eq!(
            snapshot_edges(&*first_snapshot, 10_000),
            Err(VtStatus::InvalidArgument)
        );
    }

    /// INVARIANT-HOOK: LDICT-ARENA-1..6 — ids and memoized edges remain
    /// stable when one expansion populates many hybrid-directory pages,
    /// including parallel readers of lock-free write-once slots.
    #[test]
    fn hybrid_arena_is_stable_across_many_pages_under_concurrency() {
        let dictionary = DynamicDawgBinding::new(BindingUnitDomain::UnicodeScalar);
        for offset in 0..600_u32 {
            let first = char::from_u32(0x1_000 + offset).expect("test scalar");
            dictionary
                .insert_text(first.to_string().as_bytes(), Some(u64::from(offset)))
                .expect("insert must succeed");
        }
        let resource = dictionary.resource();
        let live = unsafe { &*resource.raw.context.cast::<ResourceContext>() };
        let snapshot = live.snapshot();
        let expected = snapshot_edges(&*snapshot, 0).expect("root edges");
        assert_eq!(expected.len(), 600);
        let unique: std::collections::HashSet<_> = expected.iter().map(|edge| edge.1).collect();
        assert_eq!(unique.len(), expected.len());
        assert!(unique.iter().all(|&node| node != 0));

        let threads: Vec<_> = (0..8)
            .map(|_| {
                let snapshot = Arc::clone(&snapshot);
                let expected = expected.clone();
                std::thread::spawn(move || {
                    for _ in 0..32 {
                        assert_eq!(snapshot_edges(&*snapshot, 0).expect("root edges"), expected);
                        for (_, child) in &expected {
                            assert!(snapshot.is_final(*child).expect("child finality"));
                        }
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().expect("arena reader must not panic");
        }
    }

    #[test]
    fn failed_arena_reservation_does_not_consume_identifiers_or_create_holes() {
        let arena = NodeArena::new(0_u8, None);
        assert_eq!(arena.next_id.load(Ordering::Acquire), 1);
        assert_eq!(
            arena.reserve(usize::MAX),
            Err(VtStatus::LimitExceeded),
            "overflow must fail before directory growth or ID publication"
        );
        assert_eq!(
            arena.next_id.load(Ordering::Acquire),
            1,
            "a failed reservation must leave the committed ID frontier unchanged"
        );
        assert!(arena.slot(0).is_ok());
        assert!(matches!(arena.slot(1), Err(VtStatus::InvalidArgument)));
        assert_eq!(arena.reserve(1), Ok(1));
        arena.install(1, 1_u8).expect("first post-failure slot");
        assert_eq!(arena.slot(1).expect("installed slot").node, 1);
    }

    #[test]
    fn stable_identity_preserves_shared_suffix_nodes_across_incoming_edges() {
        let dictionary = DynamicDawgBinding::new(BindingUnitDomain::Byte);
        dictionary
            .insert_text_batch([(b"ab".as_slice(), None), (b"cb".as_slice(), None)])
            .expect("minimal batch construction must succeed");
        let resource = dictionary.resource();
        let live = unsafe { &*resource.raw.context.cast::<ResourceContext>() };
        let snapshot = live.snapshot();

        let root = snapshot_edges(&*snapshot, 0).expect("root edges");
        assert_eq!(root.len(), 2);
        let a = root
            .iter()
            .find_map(|(label, node)| (*label == u64::from(b'a')).then_some(*node))
            .expect("a branch");
        let c = root
            .iter()
            .find_map(|(label, node)| (*label == u64::from(b'c')).then_some(*node))
            .expect("c branch");
        let a_leaf = snapshot_edges(&*snapshot, a).expect("a edges")[0].1;
        let c_leaf = snapshot_edges(&*snapshot, c).expect("c edges")[0].1;

        assert_eq!(
            a_leaf, c_leaf,
            "one physical minimized suffix node must have one ABI identity"
        );
        assert!(snapshot.is_final(a_leaf).expect("shared leaf finality"));
    }

    #[test]
    fn dynamic_mutation_falls_back_to_sequential_ids_until_compaction() {
        fn branch_leaves(snapshot: &dyn SnapshotOps) -> (u64, u64) {
            let root = snapshot_edges(snapshot, 0).expect("root edges");
            let branch = |wanted| {
                root.iter()
                    .find_map(|(label, node)| (*label == u64::from(wanted)).then_some(*node))
                    .expect("requested root branch")
            };
            let a_leaf = snapshot_edges(snapshot, branch(b'a')).expect("a edges")[0].1;
            let c_leaf = snapshot_edges(snapshot, branch(b'c')).expect("c edges")[0].1;
            (a_leaf, c_leaf)
        }

        let dictionary = DynamicDawgBinding::new(BindingUnitDomain::Byte);
        dictionary
            .insert_text_batch([(b"ab".as_slice(), None), (b"cb".as_slice(), None)])
            .expect("minimal batch construction must succeed");
        let resource = dictionary.resource();
        let live = unsafe { &*resource.raw.context.cast::<ResourceContext>() };

        let minimal = live.snapshot();
        let (a_leaf, c_leaf) = branch_leaves(&*minimal);
        assert_eq!(a_leaf, c_leaf, "minimal revision shares its suffix node");
        drop(minimal);

        dictionary
            .insert_text(b"db", None)
            .expect("path-copy mutation must succeed");
        let mutated = live.snapshot();
        let (a_leaf, c_leaf) = branch_leaves(&*mutated);
        assert_ne!(
            a_leaf, c_leaf,
            "mixed path-copy revisions must use the safe sequential fallback"
        );
        drop(mutated);

        dictionary.compact();
        let compacted = live.snapshot();
        let (a_leaf, c_leaf) = branch_leaves(&*compacted);
        assert_eq!(
            a_leaf, c_leaf,
            "compaction restores dense physical identity and suffix sharing"
        );
    }

    /// INVARIANT-HOOK: LDICT-ARENA-4 — invalidating the producer memo drops its
    /// strong retain of the old revision, and releasing the last snapshot
    /// synchronously reclaims every materialized arena node on that thread.
    #[cfg(feature = "perf-instrumentation")]
    #[test]
    fn releasing_the_last_snapshot_synchronously_reclaims_its_arena() {
        let dictionary = DynamicDawgBinding::new(BindingUnitDomain::UnicodeScalar);
        dictionary
            .insert_text(b"cat", Some(1))
            .expect("insert must succeed");
        dictionary
            .insert_text(b"cot", Some(2))
            .expect("insert must succeed");
        let resource = dictionary.resource();
        let live = unsafe { &*resource.raw.context.cast::<ResourceContext>() };
        let snapshot = live.snapshot();
        let root_edges = snapshot_edges(&*snapshot, 0).expect("root edges");
        for (_, child) in root_edges {
            let _ = snapshot_edges(&*snapshot, child).expect("child edges");
        }

        // Mutation invalidates the memo's strong retain of this revision. The
        // local `snapshot` is consequently the arena's last owner.
        dictionary
            .insert_text(b"cut", Some(3))
            .expect("insert must succeed");
        let before = crate::causal_perf::causal_construction_stats();
        drop(snapshot);
        let after = crate::causal_perf::causal_construction_stats();

        assert!(
            after.resource_nodes_reclaimed > before.resource_nodes_reclaimed,
            "dropping the final snapshot owner must reclaim materialized nodes"
        );
        assert!(
            after.resource_reclaim_nanos >= before.resource_reclaim_nanos,
            "reclamation time is a monotonic cumulative counter"
        );
        assert!(
            after.resource_reclaim_max_nanos >= before.resource_reclaim_max_nanos,
            "maximum reclamation latency is monotonic"
        );
    }

    /// The emitted base vtable and all ten dictionary vtables pin their
    /// struct sizes, versions, reserved fields, domains, flag sets, and
    /// fully populated operation slots.
    #[test]
    fn emitted_vtables_pin_sizes_versions_domains_and_flags() {
        assert_eq!(
            RESOURCE_VTABLE.struct_size,
            std::mem::size_of::<VtResourceVTable>()
        );
        assert_eq!(RESOURCE_VTABLE.abi_version, VT_ABI_VERSION);
        assert_eq!(RESOURCE_VTABLE.reserved, 0, "reserved must be zero");
        assert!(RESOURCE_VTABLE.retain.is_some());
        assert!(RESOURCE_VTABLE.release.is_some());
        assert!(RESOURCE_VTABLE.query_interface.is_some());

        assert_eq!(
            DICTIONARY_GRAPH_VTABLE.struct_size,
            std::mem::size_of::<VtDictionaryGraphVTable>()
        );
        assert_eq!(
            DICTIONARY_GRAPH_VTABLE.interface_version,
            VT_DICTIONARY_GRAPH_INTERFACE_VERSION
        );
        assert_eq!(DICTIONARY_GRAPH_VTABLE.reserved, 0);
        assert!(DICTIONARY_GRAPH_VTABLE.graph.is_some());
        assert!(DICTIONARY_GRAPH_VTABLE.node_value_u64.is_some());

        const PR: u64 = dictionary_flags::PARALLEL_REENTRANT;
        const SUF: u64 = dictionary_flags::SUFFIX_BASED;
        const IMM: u64 = dictionary_flags::IMMUTABLE;
        // (requested domain, immutable, suffix) -> exact emitted flag set.
        // The U64 rows pin the aliasing in `dictionary_vtable`: no suffix
        // U64 vtable exists, so the suffix bit is dropped for that domain.
        let expectations = [
            (VtUnitDomain::Byte, false, false, PR),
            (VtUnitDomain::UnicodeScalar, false, false, PR),
            (VtUnitDomain::U64, false, false, PR),
            (VtUnitDomain::Byte, false, true, PR | SUF),
            (VtUnitDomain::UnicodeScalar, false, true, PR | SUF),
            (VtUnitDomain::U64, false, true, PR),
            (VtUnitDomain::Byte, true, false, PR | IMM),
            (VtUnitDomain::UnicodeScalar, true, false, PR | IMM),
            (VtUnitDomain::U64, true, false, PR | IMM),
            (VtUnitDomain::Byte, true, true, PR | IMM | SUF),
            (VtUnitDomain::UnicodeScalar, true, true, PR | IMM | SUF),
            (VtUnitDomain::U64, true, true, PR | IMM),
        ];
        for (domain, immutable, suffix, expected_flags) in expectations {
            let mut requested = PR;
            if immutable {
                requested |= IMM;
            }
            if suffix {
                requested |= SUF;
            }
            let vtable = unsafe { &*dictionary_vtable(domain, requested) };
            assert_eq!(
                vtable.struct_size,
                std::mem::size_of::<VtDictionaryVTable>(),
                "{domain:?}/{immutable}/{suffix}: struct_size"
            );
            assert_eq!(
                vtable.interface_version, VT_DICTIONARY_INTERFACE_VERSION,
                "{domain:?}/{immutable}/{suffix}: interface_version"
            );
            assert_eq!(
                vtable.unit_domain, domain,
                "{domain:?}/{immutable}/{suffix}: unit_domain"
            );
            assert_eq!(
                vtable.value_domain,
                VtValueDomain::OptionalU64,
                "{domain:?}/{immutable}/{suffix}: value_domain"
            );
            assert_eq!(
                vtable.flags, expected_flags,
                "{domain:?}/{immutable}/{suffix}: exact flag set"
            );
            assert!(vtable.snapshot.is_some(), "snapshot slot");
            assert!(vtable.root.is_some(), "root slot");
            assert!(vtable.len.is_some(), "len slot");
            assert!(vtable.node_is_final.is_some(), "node_is_final slot");
            assert!(vtable.node_value_u64.is_some(), "node_value_u64 slot");
            assert!(vtable.node_transition.is_some(), "node_transition slot");
            assert!(vtable.node_edges.is_some(), "node_edges slot");
        }

        // The U64 aliasing is pointer-level: suffix requests reuse the
        // non-suffix statics rather than minting lookalike tables.
        assert!(std::ptr::eq(
            dictionary_vtable(VtUnitDomain::U64, PR | SUF),
            dictionary_vtable(VtUnitDomain::U64, PR)
        ));
        assert!(std::ptr::eq(
            dictionary_vtable(VtUnitDomain::U64, PR | IMM | SUF),
            dictionary_vtable(VtUnitDomain::U64, PR | IMM)
        ));
    }
}
