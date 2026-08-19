//! Generic `DATCoreShared<U, V>` storage struct shared by the byte and
//! char DAT variants.
//!
//! Mirrors the conditional serde bound pattern used by
//! [`crate::dynamic_dawg::core::DawgNode`] so the same struct works under both
//! the `serialization`-only and `persistent-artrie` configurations.

use std::ops::Deref;
use std::sync::Arc;

#[cfg(any(test, debug_assertions, feature = "serialization"))]
use std::collections::VecDeque;
#[cfg(any(test, debug_assertions, feature = "serialization"))]
use std::fmt;

#[cfg(any(feature = "perf-instrumentation", feature = "benchmark-controls"))]
use std::sync::OnceLock;

use crate::value::DictionaryValue;
use crate::{CharUnit, SnapshotTraversalCursor};

/// Unit types whose DAT offset semantics are owned by this crate and therefore
/// may participate in the construction-certified traversal fast path.
///
/// `CharUnit` is intentionally downstream-implementable. Keeping this proof
/// trait private prevents an external implementation with stateful or
/// otherwise surprising `to_dat_offset` behavior from invalidating the unsafe
/// invariants of [`ValidatedDATCoreShared`].
pub(crate) trait CertifiedDatUnit: CharUnit {
    fn certified_dat_offset(self) -> usize;
    fn try_from_certified_dat_offset(offset: usize) -> Option<Self>;
}

impl CertifiedDatUnit for u8 {
    #[inline(always)]
    fn certified_dat_offset(self) -> usize {
        self as usize
    }

    #[inline(always)]
    fn try_from_certified_dat_offset(offset: usize) -> Option<Self> {
        u8::try_from(offset).ok()
    }
}

impl CertifiedDatUnit for char {
    #[inline(always)]
    fn certified_dat_offset(self) -> usize {
        self as usize
    }

    #[inline(always)]
    fn try_from_certified_dat_offset(offset: usize) -> Option<Self> {
        u32::try_from(offset).ok().and_then(char::from_u32)
    }
}

/// Shared storage for Double-Array Trie states.
///
/// Holds the four parallel arrays (BASE, CHECK, IS_FINAL, edges) plus
/// optional terminal values. All fields are `Arc<Vec<…>>` so clone is
/// cheap (no deep copy) and multiple readers can navigate the trie
/// concurrently.
///
/// # Type parameters
///
/// - `U`: edge label type (`u8` for byte-keyed DAT, `char` for
///   Unicode-keyed DAT). Must implement [`CharUnit`].
/// - `V`: value type associated with terminal states. Must implement
///   [`DictionaryValue`].
///
/// # Serialization
///
/// Custom serde plumbing routes through
/// `crate::serialization::serde_helpers` so the on-disk format
/// matches the previous byte-for-byte layout used by both
/// `DoubleArrayTrie<V>` and `DoubleArrayTrieChar<V>`.
#[cfg_attr(
    feature = "serialization",
    derive(serde::Serialize, serde::Deserialize)
)]
#[cfg_attr(
    all(feature = "serialization", not(feature = "persistent-artrie")),
    serde(bound(
        serialize = "U: serde::Serialize, V: serde::Serialize",
        deserialize = "U: serde::Deserialize<'de>, V: serde::Deserialize<'de>",
    ))
)]
#[cfg_attr(
    all(feature = "serialization", feature = "persistent-artrie"),
    serde(bound(
        serialize = "U: serde::Serialize, V: serde::Serialize",
        deserialize = "U: serde::de::DeserializeOwned, V: serde::de::DeserializeOwned",
    ))
)]
#[derive(Clone, Debug)]
pub struct DATCoreShared<U: CharUnit, V: DictionaryValue = ()> {
    /// BASE array: offset for computing next state.
    ///
    /// Transition from state `s` on label `u` lands at
    /// `base[s] + (u as i32)`. The CHECK array verifies the parent.
    #[cfg_attr(
        feature = "serialization",
        serde(
            serialize_with = "crate::serialization::serde_helpers::serialize_arc_vec",
            deserialize_with = "crate::serialization::serde_helpers::deserialize_arc_vec"
        )
    )]
    pub base: Arc<Vec<i32>>,

    /// CHECK array: parent state verification.
    ///
    /// A computed child state `c = base[parent] + u` is only valid
    /// when `check[c] == parent`.
    #[cfg_attr(
        feature = "serialization",
        serde(
            serialize_with = "crate::serialization::serde_helpers::serialize_arc_vec",
            deserialize_with = "crate::serialization::serde_helpers::deserialize_arc_vec"
        )
    )]
    pub check: Arc<Vec<i32>>,

    /// Final-state markers (terminal flag per state).
    #[cfg_attr(
        feature = "serialization",
        serde(
            serialize_with = "crate::serialization::serde_helpers::serialize_arc_vec",
            deserialize_with = "crate::serialization::serde_helpers::deserialize_arc_vec"
        )
    )]
    pub is_final: Arc<Vec<bool>>,

    /// Edge lists per state: the actual outgoing edge labels at each
    /// state. Avoids scanning all 256 (byte) or 1.1M (char) possible
    /// labels during iteration.
    #[cfg_attr(
        feature = "serialization",
        serde(
            serialize_with = "crate::serialization::serde_helpers::serialize_arc_vec_vec",
            deserialize_with = "crate::serialization::serde_helpers::deserialize_arc_vec_vec"
        )
    )]
    pub edges: Arc<Vec<Vec<U>>>,

    /// Values associated with final states.
    ///
    /// Indexed by state number; only final states may hold `Some(v)`.
    #[cfg_attr(
        feature = "serialization",
        serde(
            serialize_with = "crate::serialization::serde_helpers::serialize_arc_vec",
            deserialize_with = "crate::serialization::serde_helpers::deserialize_arc_vec"
        )
    )]
    pub values: Arc<Vec<Option<V>>>,
}

/// A structural error found while turning untrusted double-array data into an
/// immutable dictionary revision.
///
/// The raw [`DATCoreShared`] representation remains public and wire-compatible,
/// but query cursors never receive unchecked access to it. Only this
/// crate-private type-state can expose the trusted traversal operations below.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg(any(test, debug_assertions, feature = "serialization"))]
pub(crate) enum DatValidationError {
    ParallelArrayLengths {
        base: usize,
        check: usize,
        is_final: usize,
        edges: usize,
        values: usize,
    },
    RootOutOfBounds {
        root: usize,
        len: usize,
    },
    InvalidRootParent {
        root: usize,
        check: i32,
    },
    StateExceedsCheckRepresentation {
        state: usize,
    },
    UnsortedOrDuplicateEdges {
        state: usize,
    },
    NegativeBaseWithEdges {
        state: usize,
    },
    TransitionOverflow {
        state: usize,
        offset: usize,
    },
    TransitionOutOfBounds {
        state: usize,
        target: usize,
        len: usize,
    },
    WrongTransitionParent {
        state: usize,
        target: usize,
        actual: i32,
    },
    RepeatedOrCyclicTarget {
        state: usize,
        target: usize,
    },
    NonCanonicalUnreachableState {
        state: usize,
    },
    ValueOnNonFinalState {
        state: usize,
    },
    #[cfg(feature = "serialization")]
    TermCountMismatch {
        declared: usize,
        reachable_finals: usize,
    },
    #[cfg(feature = "serialization")]
    InvalidFreeListEntry {
        state: usize,
    },
    #[cfg(feature = "serialization")]
    DuplicateFreeListEntry {
        state: usize,
    },
    #[cfg(feature = "serialization")]
    InvalidRebuildThreshold,
}

#[cfg(any(test, debug_assertions, feature = "serialization"))]
impl fmt::Display for DatValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ParallelArrayLengths {
                base,
                check,
                is_final,
                edges,
                values,
            } => write!(
                formatter,
                "DAT parallel arrays have different lengths: BASE={base}, CHECK={check}, finality={is_final}, edges={edges}, values={values}"
            ),
            Self::RootOutOfBounds { root, len } => {
                write!(formatter, "DAT root state {root} is outside array length {len}")
            }
            Self::InvalidRootParent { root, check } => write!(
                formatter,
                "DAT root state {root} has invalid CHECK parent {check}"
            ),
            Self::StateExceedsCheckRepresentation { state } => write!(
                formatter,
                "DAT state {state} cannot be represented by the i32 CHECK array"
            ),
            Self::UnsortedOrDuplicateEdges { state } => write!(
                formatter,
                "DAT state {state} has unsorted or duplicate edge labels"
            ),
            Self::NegativeBaseWithEdges { state } => {
                write!(formatter, "DAT state {state} has edges but a negative BASE")
            }
            Self::TransitionOverflow { state, offset } => write!(
                formatter,
                "DAT transition from state {state} over offset {offset} overflows usize"
            ),
            Self::TransitionOutOfBounds { state, target, len } => write!(
                formatter,
                "DAT transition from state {state} targets {target}, outside array length {len}"
            ),
            Self::WrongTransitionParent {
                state,
                target,
                actual,
            } => write!(
                formatter,
                "DAT transition from state {state} targets {target}, whose CHECK parent is {actual}"
            ),
            Self::RepeatedOrCyclicTarget { state, target } => write!(
                formatter,
                "DAT transition from state {state} repeats or cycles to target {target}"
            ),
            Self::NonCanonicalUnreachableState { state } => write!(
                formatter,
                "DAT state {state} is unreachable but is not a canonical empty slot"
            ),
            Self::ValueOnNonFinalState { state } => write!(
                formatter,
                "DAT state {state} stores a value without being final"
            ),
            #[cfg(feature = "serialization")]
            Self::TermCountMismatch {
                declared,
                reachable_finals,
            } => write!(
                formatter,
                "DAT declares {declared} terms but has {reachable_finals} reachable final states"
            ),
            #[cfg(feature = "serialization")]
            Self::InvalidFreeListEntry { state } => {
                write!(formatter, "DAT free-list entry {state} is not a reusable hole")
            }
            #[cfg(feature = "serialization")]
            Self::DuplicateFreeListEntry { state } => {
                write!(formatter, "DAT free-list entry {state} occurs more than once")
            }
            #[cfg(feature = "serialization")]
            Self::InvalidRebuildThreshold => write!(
                formatter,
                "DAT rebuild threshold must be finite and between zero and one"
            ),
        }
    }
}

/// Private immutable proof that the raw parallel arrays form one valid DAT.
///
/// `ROOT` preserves the historical layouts without a virtual dispatch or a
/// runtime root field: byte DATs use root 1, while Unicode DATs use root 0.
/// The wrapper intentionally implements only immutable [`Deref`]; no mutable
/// escape can invalidate the proof after construction.
#[derive(Clone, Debug)]
pub(crate) struct ValidatedDATCoreShared<
    U: CertifiedDatUnit,
    V: DictionaryValue = (),
    const ROOT: usize = 0,
> {
    raw: DATCoreShared<U, V>,
    #[cfg(feature = "serialization")]
    reachable_final_count: usize,
}

impl<U: CertifiedDatUnit, V: DictionaryValue, const ROOT: usize> Deref
    for ValidatedDATCoreShared<U, V, ROOT>
{
    type Target = DATCoreShared<U, V>;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.raw
    }
}

#[cfg(feature = "serialization")]
impl<U, V, const ROOT: usize> serde::Serialize for ValidatedDATCoreShared<U, V, ROOT>
where
    U: CertifiedDatUnit + serde::Serialize,
    V: DictionaryValue + serde::Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.raw.serialize(serializer)
    }
}

// =============================================================================
// Algorithmic methods generic over U: CharUnit
// =============================================================================
//
// Methods that operate on the BASE/CHECK arrays in unit-agnostic ways. The
// byte and char DAT variants used to each carry their own copy of these
// (under different unit-type bounds); now both delegate to these.

impl<U: CharUnit, V: DictionaryValue> DATCoreShared<U, V> {
    /// Prove the complete reachable structure rooted at `root_state`.
    ///
    /// Validation is deliberately linear in the stored array and edge counts.
    /// Hostile deserialization pays this once; trusted builders assert it in
    /// debug/CI builds and seal their jointly-produced arrays without adding a
    /// second production construction pass.
    #[cfg(any(test, debug_assertions, feature = "serialization"))]
    fn validate_layout(&self, root_state: usize) -> Result<usize, DatValidationError>
    where
        U: CertifiedDatUnit,
    {
        let len = self.base.len();
        if self.check.len() != len
            || self.is_final.len() != len
            || self.edges.len() != len
            || self.values.len() != len
        {
            return Err(DatValidationError::ParallelArrayLengths {
                base: len,
                check: self.check.len(),
                is_final: self.is_final.len(),
                edges: self.edges.len(),
                values: self.values.len(),
            });
        }
        if root_state >= len {
            return Err(DatValidationError::RootOutOfBounds {
                root: root_state,
                len,
            });
        }

        let root_parent = self.check[root_state];
        let root_as_i32 = i32::try_from(root_state).map_err(|_| {
            DatValidationError::StateExceedsCheckRepresentation { state: root_state }
        })?;
        if root_parent >= 0 && root_parent != root_as_i32 {
            return Err(DatValidationError::InvalidRootParent {
                root: root_state,
                check: root_parent,
            });
        }

        let mut reachable = vec![false; len];
        let mut queue = VecDeque::new();
        reachable[root_state] = true;
        queue.push_back(root_state);
        let mut reachable_final_count = 0usize;

        while let Some(state) = queue.pop_front() {
            let parent = i32::try_from(state)
                .map_err(|_| DatValidationError::StateExceedsCheckRepresentation { state })?;
            if self.is_final[state] {
                reachable_final_count += 1;
            } else if self.values[state].is_some() {
                return Err(DatValidationError::ValueOnNonFinalState { state });
            }

            let labels = &self.edges[state];
            if labels.windows(2).any(|pair| pair[0] >= pair[1]) {
                return Err(DatValidationError::UnsortedOrDuplicateEdges { state });
            }
            if labels.is_empty() {
                continue;
            }

            let base = self.base[state];
            if base < 0 {
                return Err(DatValidationError::NegativeBaseWithEdges { state });
            }
            let base = base as usize;

            for &label in labels {
                let offset = label.certified_dat_offset();
                let target = base
                    .checked_add(offset)
                    .ok_or(DatValidationError::TransitionOverflow { state, offset })?;
                if target >= len {
                    return Err(DatValidationError::TransitionOutOfBounds { state, target, len });
                }
                if self.check[target] != parent {
                    return Err(DatValidationError::WrongTransitionParent {
                        state,
                        target,
                        actual: self.check[target],
                    });
                }
                if reachable[target] {
                    return Err(DatValidationError::RepeatedOrCyclicTarget { state, target });
                }
                reachable[target] = true;
                queue.push_back(target);
            }
        }

        for (state, is_reachable) in reachable.iter().copied().enumerate() {
            if is_reachable {
                if !self.is_final[state] && self.values[state].is_some() {
                    return Err(DatValidationError::ValueOnNonFinalState { state });
                }
                continue;
            }

            if self.base[state] != -1
                || self.check[state] != -1
                || self.is_final[state]
                || !self.edges[state].is_empty()
                || self.values[state].is_some()
            {
                return Err(DatValidationError::NonCanonicalUnreachableState { state });
            }
        }

        Ok(reachable_final_count)
    }

    /// Encode a backend state as the copyable cursor used by snapshot
    /// traversals. DAT state zero remains representable because public cursors
    /// are one-based.
    #[inline]
    pub(crate) fn traversal_cursor(state: usize) -> Option<SnapshotTraversalCursor> {
        SnapshotTraversalCursor::new(state.checked_add(1)?)
    }

    /// Materialize one state index only when it belongs to every parallel DAT
    /// array required by node traversal.
    #[inline]
    pub(crate) fn traversal_state(&self, cursor: SnapshotTraversalCursor) -> Option<usize> {
        let state = cursor.get() - 1;
        (state < self.base.len() && state < self.is_final.len() && state < self.edges.len())
            .then_some(state)
    }

    /// Check that an opaque cursor names an occupied state in this exact DAT.
    ///
    /// Static DAT builders leave `check[state] == -1` in unused collision
    /// slots. The root is the sole occupied state without a parent, so its
    /// backend-specific index is admitted explicitly. Including the value
    /// array in this boundary check also certifies every operation required by
    /// a mapped FFI snapshot before it enters the unsafe cursor API.
    #[inline]
    pub(crate) fn contains_traversal_cursor(
        &self,
        cursor: SnapshotTraversalCursor,
        root_state: usize,
    ) -> bool {
        let Some(state) = self.traversal_state(cursor) else {
            return false;
        };
        self.values.get(state).is_some()
            && (state == root_state || self.check.get(state).is_some_and(|parent| *parent >= 0))
    }

    /// Read finality directly from an already validated immutable DAT cursor.
    #[inline]
    pub(crate) fn traversal_cursor_is_final(
        &self,
        cursor: SnapshotTraversalCursor,
    ) -> Option<bool> {
        let state = self.traversal_state(cursor)?;
        self.is_final.get(state).copied()
    }

    /// Follow one DAT transition without materializing a node handle.
    #[inline]
    pub(crate) fn traversal_cursor_transition(
        &self,
        cursor: SnapshotTraversalCursor,
        label: U,
    ) -> Option<Option<SnapshotTraversalCursor>> {
        let state = self.traversal_state(cursor)?;
        let base = self.base[state];
        if base < 0 {
            return Some(None);
        }
        let Some(next) = (base as usize).checked_add(label.to_dat_offset()) else {
            return Some(None);
        };
        Some(
            (next < self.check.len() && self.check[next] == state as i32)
                .then(|| Self::traversal_cursor(next))
                .flatten(),
        )
    }

    /// Visit a page from the DAT's native edge slice without rescanning edges
    /// before `start` or after the page. The immutable builder maintains the
    /// invariant that every stored label names a CHECK-validated child.
    #[inline]
    pub(crate) fn visit_traversal_cursor_edge_page<F>(
        &self,
        cursor: SnapshotTraversalCursor,
        start: usize,
        capacity: usize,
        mut visitor: F,
    ) -> Option<(bool, usize)>
    where
        F: FnMut(U, SnapshotTraversalCursor),
    {
        let state = self.traversal_state(cursor)?;
        let is_final = self.is_final[state];
        let labels = &self.edges[state];
        let total = labels.len();
        let base = self.base[state];
        if base < 0 || start >= total || capacity == 0 {
            return Some((is_final, total));
        }

        for &label in labels.iter().skip(start).take(capacity) {
            let Some(next) = (base as usize).checked_add(label.to_dat_offset()) else {
                continue;
            };
            if next < self.check.len() && self.check[next] == state as i32 {
                let child = Self::traversal_cursor(next)
                    .expect("a DAT array index always fits its one-based cursor");
                visitor(label, child);
            }
        }
        Some((is_final, total))
    }

    /// Project one captured DAT state directly from its immutable parallel
    /// arrays. This is shared by the byte and Unicode DAT node adapters so
    /// accepted edges enqueue only a word-sized cursor instead of cloning an
    /// `Arc`-owned node handle.
    #[inline]
    pub(crate) fn filter_map_traversal_cursor<T, P, F>(
        &self,
        cursor: SnapshotTraversalCursor,
        mut project: P,
        mut visitor: F,
    ) -> Option<bool>
    where
        P: FnMut(U) -> Option<T>,
        F: FnMut(U, SnapshotTraversalCursor, T),
    {
        let state = self.traversal_state(cursor)?;
        let is_final = self.is_final[state];
        let base = self.base[state];
        if base < 0 {
            return Some(is_final);
        }

        for &label in &self.edges[state] {
            let Some(projected) = project(label) else {
                continue;
            };
            let Some(next) = (base as usize).checked_add(label.to_dat_offset()) else {
                continue;
            };
            if next < self.check.len() && self.check[next] == state as i32 {
                let child = Self::traversal_cursor(next)
                    .expect("a DAT array index always fits its one-based cursor");
                visitor(label, child, projected);
            }
        }
        Some(is_final)
    }

    /// Clone an optional mapped value from a captured DAT cursor.
    #[inline]
    pub(crate) fn traversal_cursor_value(
        &self,
        cursor: SnapshotTraversalCursor,
    ) -> Option<Option<V>> {
        let state = self.traversal_state(cursor)?;
        Some(self.values.get(state)?.clone())
    }

    /// Walk the trie starting at `root_state` and return whether `term`
    /// reaches a final state.
    ///
    /// The byte DAT uses `root_state = 1` (state 0 is a sentinel); the
    /// char DAT uses `root_state = 0`. Pass whichever convention your
    /// builder uses. Generic over the unit type via
    /// [`CharUnit::iter_str`] + [`CharUnit::to_dat_offset`].
    pub fn contains_term_from(&self, term: &str, root_state: usize) -> bool {
        let mut state: usize = root_state;
        for unit in U::iter_str(term) {
            if state >= self.base.len() {
                return false;
            }
            let base = self.base[state];
            if base < 0 {
                return false;
            }
            let next = (base as usize).wrapping_add(unit.to_dat_offset());
            if next >= self.check.len() || self.check[next] != state as i32 {
                return false;
            }
            state = next;
        }
        state < self.is_final.len() && self.is_final[state]
    }

    /// Walk the trie from `root_state` and return the value at the final
    /// state if `term` is present; `None` otherwise.
    pub fn term_value_from(&self, term: &str, root_state: usize) -> Option<V>
    where
        V: Clone,
    {
        let mut state: usize = root_state;
        for unit in U::iter_str(term) {
            if state >= self.base.len() {
                return None;
            }
            let base = self.base[state];
            if base < 0 {
                return None;
            }
            let next = (base as usize).wrapping_add(unit.to_dat_offset());
            if next >= self.check.len() || self.check[next] != state as i32 {
                return None;
            }
            state = next;
        }
        if state < self.is_final.len() && self.is_final[state] {
            self.values.get(state).and_then(|v| v.clone())
        } else {
            None
        }
    }

    /// `contains_term_from` with byte-DAT's `root_state = 1` convention.
    #[inline]
    pub fn contains_term(&self, term: &str) -> bool {
        self.contains_term_from(term, 1)
    }

    /// `term_value_from` with byte-DAT's `root_state = 1` convention.
    #[inline]
    pub fn term_value(&self, term: &str) -> Option<V>
    where
        V: Clone,
    {
        self.term_value_from(term, 1)
    }
}

#[cfg(any(feature = "perf-instrumentation", feature = "benchmark-controls"))]
#[inline]
fn use_checked_dat_cursor_edges() -> bool {
    static USE_CHECKED: OnceLock<bool> = OnceLock::new();
    *USE_CHECKED.get_or_init(|| {
        std::env::var_os("LIBDICTENSTEIN_CAUSAL_USE_CHECKED_DAT_CURSOR_EDGES").is_some()
    })
}

#[cfg(not(any(feature = "perf-instrumentation", feature = "benchmark-controls")))]
#[inline(always)]
fn use_checked_dat_cursor_edges() -> bool {
    false
}

impl<U: CertifiedDatUnit, V: DictionaryValue, const ROOT: usize>
    ValidatedDATCoreShared<U, V, ROOT>
{
    /// Seal raw arrays emitted jointly by a trusted in-crate DAT builder.
    ///
    /// # Safety
    ///
    /// `raw` must satisfy [`DATCoreShared::validate_layout`] for `ROOT`, and
    /// `declared_term_count` must equal its reachable final-state count. The
    /// full proof runs in debug and CI builds; release construction deliberately
    /// avoids an otherwise redundant O(states + edges) pass.
    #[inline]
    pub(crate) unsafe fn from_builder_parts_unchecked(
        raw: DATCoreShared<U, V>,
        declared_term_count: usize,
    ) -> Self {
        let _ = declared_term_count;
        #[cfg(debug_assertions)]
        {
            let reachable_final_count = raw
                .validate_layout(ROOT)
                .expect("trusted DAT builder emitted an invalid layout");
            assert_eq!(
                declared_term_count, reachable_final_count,
                "trusted DAT builder emitted an incorrect term count"
            );
        }
        Self {
            raw,
            #[cfg(feature = "serialization")]
            reachable_final_count: declared_term_count,
        }
    }

    /// Validate and seal an untrusted serialized representation.
    #[cfg(feature = "serialization")]
    pub(crate) fn try_from_untrusted(
        raw: DATCoreShared<U, V>,
        declared_term_count: usize,
    ) -> Result<Self, DatValidationError> {
        let reachable_final_count = raw.validate_layout(ROOT)?;
        if declared_term_count != reachable_final_count {
            return Err(DatValidationError::TermCountMismatch {
                declared: declared_term_count,
                reachable_finals: reachable_final_count,
            });
        }
        Ok(Self {
            raw,
            reachable_final_count,
        })
    }

    /// Validate serialized free-list entries against the sealed layout.
    #[cfg(feature = "serialization")]
    pub(crate) fn validate_free_list(&self, free_list: &[usize]) -> Result<(), DatValidationError> {
        let mut listed = vec![false; self.raw.base.len()];
        for &state in free_list {
            if state <= ROOT
                || state >= self.raw.base.len()
                || self.raw.base[state] != -1
                || self.raw.check[state] != -1
                || self.raw.is_final[state]
                || !self.raw.edges[state].is_empty()
                || self.raw.values[state].is_some()
            {
                return Err(DatValidationError::InvalidFreeListEntry { state });
            }
            if std::mem::replace(&mut listed[state], true) {
                return Err(DatValidationError::DuplicateFreeListEntry { state });
            }
        }
        Ok(())
    }

    /// Reachable final-state count certified by the immutable layout proof.
    #[cfg(feature = "serialization")]
    #[inline]
    pub(crate) fn reachable_final_count(&self) -> usize {
        self.reachable_final_count
    }

    #[inline]
    pub(crate) fn traversal_cursor(state: usize) -> Option<SnapshotTraversalCursor> {
        DATCoreShared::<U, V>::traversal_cursor(state)
    }

    /// Recover a state from a provenance-valid cursor.
    ///
    /// # Safety
    ///
    /// The cursor must come from this exact retained revision's root or from a
    /// prior trusted cursor visitor.
    #[inline(always)]
    unsafe fn trusted_state(&self, cursor: SnapshotTraversalCursor) -> usize {
        let state = cursor.get() - 1;
        debug_assert!(self.raw.contains_traversal_cursor(cursor, ROOT));
        state
    }

    /// Materialize a state index from a provenance-valid cursor.
    ///
    /// # Safety
    ///
    /// The caller must uphold [`Self::trusted_state`]'s cursor contract.
    #[inline(always)]
    pub(crate) unsafe fn traversal_state(&self, cursor: SnapshotTraversalCursor) -> Option<usize> {
        if use_checked_dat_cursor_edges() {
            return self.raw.traversal_state(cursor);
        }
        // SAFETY: delegated from this method's contract.
        Some(unsafe { self.trusted_state(cursor) })
    }

    /// Reconstruct one provenance-valid cursor's key relative to a captured
    /// ancestor state by walking the construction-certified CHECK parents.
    ///
    /// # Safety
    ///
    /// `cursor` must be this retained revision's `relative_root_state` cursor
    /// or one of its descendants. The caller must retain this sealed layout
    /// for the complete walk.
    #[inline]
    pub(crate) unsafe fn traversal_cursor_key_units(
        &self,
        cursor: SnapshotTraversalCursor,
        relative_root_state: usize,
    ) -> Option<Vec<U>> {
        let mut state = unsafe { self.trusted_state(cursor) };
        debug_assert!(relative_root_state < self.raw.base.len());
        let mut units = Vec::new();
        let mut remaining = self.raw.base.len();

        while state != relative_root_state {
            if remaining == 0 {
                return None;
            }
            remaining -= 1;

            let parent = usize::try_from(*self.raw.check.get(state)?).ok()?;
            let base = usize::try_from(*self.raw.base.get(parent)?).ok()?;
            let offset = state.checked_sub(base)?;
            let unit = U::try_from_certified_dat_offset(offset)?;
            debug_assert!(self.raw.edges[parent].binary_search(&unit).is_ok());
            units.push(unit);
            state = parent;
        }

        units.reverse();
        Some(units)
    }

    /// Read finality without repeating parallel-array bounds checks.
    ///
    /// # Safety
    ///
    /// The caller must provide a provenance-valid cursor for this revision.
    #[inline(always)]
    pub(crate) unsafe fn traversal_cursor_is_final(
        &self,
        cursor: SnapshotTraversalCursor,
    ) -> Option<bool> {
        if use_checked_dat_cursor_edges() {
            return self.raw.traversal_cursor_is_final(cursor);
        }
        // SAFETY: a certified cursor is in every equal-length parallel array.
        let state = unsafe { self.trusted_state(cursor) };
        Some(unsafe { *self.raw.is_final.get_unchecked(state) })
    }

    /// Follow an arbitrary label from a provenance-valid cursor.
    ///
    /// CHECK remains mandatory here: unlike labels read from `edges[state]`,
    /// the caller-supplied label was not certified by layout validation.
    ///
    /// # Safety
    ///
    /// The caller must provide a provenance-valid cursor for this revision.
    #[inline(always)]
    pub(crate) unsafe fn traversal_cursor_transition(
        &self,
        cursor: SnapshotTraversalCursor,
        label: U,
    ) -> Option<Option<SnapshotTraversalCursor>> {
        if use_checked_dat_cursor_edges() {
            return self.raw.traversal_cursor_transition(cursor, label);
        }
        // SAFETY: a certified cursor indexes BASE.
        let state = unsafe { self.trusted_state(cursor) };
        let base = unsafe { *self.raw.base.get_unchecked(state) };
        if base < 0 {
            return Some(None);
        }
        let Some(next) = (base as usize).checked_add(label.certified_dat_offset()) else {
            return Some(None);
        };
        Some(
            self.raw
                .check
                .get(next)
                .is_some_and(|parent| *parent == state as i32)
                .then(|| DATCoreShared::<U, V>::traversal_cursor(next))
                .flatten(),
        )
    }

    /// Visit a native edge page using targets proved at layout-sealing time.
    ///
    /// # Safety
    ///
    /// The caller must provide a provenance-valid cursor for this revision.
    #[inline(always)]
    pub(crate) unsafe fn visit_traversal_cursor_edge_page<F>(
        &self,
        cursor: SnapshotTraversalCursor,
        start: usize,
        capacity: usize,
        mut visitor: F,
    ) -> Option<(bool, usize)>
    where
        F: FnMut(U, SnapshotTraversalCursor),
    {
        if use_checked_dat_cursor_edges() {
            return self
                .raw
                .visit_traversal_cursor_edge_page(cursor, start, capacity, visitor);
        }

        // SAFETY: a certified cursor indexes every parallel array.
        let state = unsafe { self.trusted_state(cursor) };
        let is_final = unsafe { *self.raw.is_final.get_unchecked(state) };
        let labels = unsafe { self.raw.edges.get_unchecked(state) };
        let total = labels.len();
        if start >= total || capacity == 0 {
            return Some((is_final, total));
        }
        let base = unsafe { *self.raw.base.get_unchecked(state) };
        debug_assert!(base >= 0);
        let base = base as usize;

        for &label in labels.iter().skip(start).take(capacity) {
            let next = base + label.certified_dat_offset();
            debug_assert!(next < self.raw.check.len());
            debug_assert_eq!(self.raw.check[next], state as i32);
            // SAFETY: validation proved `next < len`, hence `next + 1` is a
            // representable non-zero cursor token.
            let child = unsafe { SnapshotTraversalCursor::new(next + 1).unwrap_unchecked() };
            visitor(label, child);
        }
        Some((is_final, total))
    }

    /// Project native edges using targets proved at layout-sealing time.
    ///
    /// # Safety
    ///
    /// The caller must provide a provenance-valid cursor for this revision.
    #[inline(always)]
    pub(crate) unsafe fn filter_map_traversal_cursor<T, P, F>(
        &self,
        cursor: SnapshotTraversalCursor,
        mut project: P,
        mut visitor: F,
    ) -> Option<bool>
    where
        P: FnMut(U) -> Option<T>,
        F: FnMut(U, SnapshotTraversalCursor, T),
    {
        if use_checked_dat_cursor_edges() {
            return self
                .raw
                .filter_map_traversal_cursor(cursor, project, visitor);
        }

        // SAFETY: a certified cursor indexes every parallel array.
        let state = unsafe { self.trusted_state(cursor) };
        let is_final = unsafe { *self.raw.is_final.get_unchecked(state) };
        let labels = unsafe { self.raw.edges.get_unchecked(state) };
        if labels.is_empty() {
            return Some(is_final);
        }
        let base = unsafe { *self.raw.base.get_unchecked(state) };
        debug_assert!(base >= 0);
        let base = base as usize;

        for &label in labels {
            let Some(projected) = project(label) else {
                continue;
            };
            let next = base + label.certified_dat_offset();
            debug_assert!(next < self.raw.check.len());
            debug_assert_eq!(self.raw.check[next], state as i32);
            // SAFETY: layout validation proved the edge target and cursor token.
            let child = unsafe { SnapshotTraversalCursor::new(next + 1).unwrap_unchecked() };
            visitor(label, child, projected);
        }
        Some(is_final)
    }

    /// Clone a mapped value without repeating parallel-array bounds checks.
    ///
    /// # Safety
    ///
    /// The caller must provide a provenance-valid cursor for this revision.
    #[inline(always)]
    pub(crate) unsafe fn traversal_cursor_value(
        &self,
        cursor: SnapshotTraversalCursor,
    ) -> Option<Option<V>> {
        if use_checked_dat_cursor_edges() {
            return self.raw.traversal_cursor_value(cursor);
        }
        // SAFETY: a certified cursor indexes VALUES.
        let state = unsafe { self.trusted_state(cursor) };
        Some(unsafe { self.raw.values.get_unchecked(state) }.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_byte_layout() -> DATCoreShared<u8, u32> {
        DATCoreShared {
            base: Arc::new(vec![-1, 2, -1]),
            check: Arc::new(vec![-1, -1, 1]),
            is_final: Arc::new(vec![false, false, true]),
            edges: Arc::new(vec![vec![], vec![0], vec![]]),
            values: Arc::new(vec![None, None, Some(7)]),
        }
    }

    fn valid_char_layout() -> DATCoreShared<char, ()> {
        DATCoreShared {
            base: Arc::new(vec![1, -1]),
            check: Arc::new(vec![-1, 0]),
            is_final: Arc::new(vec![false, true]),
            edges: Arc::new(vec![vec!['\0'], vec![]]),
            values: Arc::new(vec![None, None]),
        }
    }

    #[test]
    fn dat_core_shared_default_byte() {
        // Smoke test: constructs the generic shared struct over u8.
        let shared: DATCoreShared<u8, ()> = DATCoreShared {
            base: Arc::new(vec![0]),
            check: Arc::new(vec![0]),
            is_final: Arc::new(vec![false]),
            edges: Arc::new(vec![vec![]]),
            values: Arc::new(vec![None]),
        };
        assert_eq!(shared.base.len(), 1);
        assert_eq!(shared.edges.len(), 1);
    }

    #[test]
    fn dat_core_shared_default_char() {
        // Smoke test over char (Unicode-keyed).
        let shared: DATCoreShared<char, u32> = DATCoreShared {
            base: Arc::new(vec![0]),
            check: Arc::new(vec![0]),
            is_final: Arc::new(vec![false]),
            edges: Arc::new(vec![vec![]]),
            values: Arc::new(vec![None]),
        };
        assert_eq!(shared.edges[0].len(), 0);
        assert!(shared.values[0].is_none());
    }

    #[test]
    fn complete_layout_validation_accepts_byte_and_unicode_roots() {
        assert_eq!(valid_byte_layout().validate_layout(1), Ok(1));
        assert_eq!(valid_char_layout().validate_layout(0), Ok(1));

        let historical_empty_char = DATCoreShared::<char, ()> {
            base: Arc::new(vec![0]),
            check: Arc::new(vec![0]),
            is_final: Arc::new(vec![false]),
            edges: Arc::new(vec![vec![]]),
            values: Arc::new(vec![None]),
        };
        assert_eq!(historical_empty_char.validate_layout(0), Ok(0));
    }

    #[test]
    fn layout_validation_rejects_parallel_array_and_root_corruption() {
        let mut truncated = valid_byte_layout();
        Arc::make_mut(&mut truncated.values).pop();
        assert!(matches!(
            truncated.validate_layout(1),
            Err(DatValidationError::ParallelArrayLengths { .. })
        ));

        assert!(matches!(
            valid_char_layout().validate_layout(2),
            Err(DatValidationError::RootOutOfBounds { .. })
        ));

        let mut wrong_root_parent = valid_char_layout();
        Arc::make_mut(&mut wrong_root_parent.check)[0] = 1;
        assert!(matches!(
            wrong_root_parent.validate_layout(0),
            Err(DatValidationError::InvalidRootParent { .. })
        ));
    }

    #[test]
    fn layout_validation_rejects_edge_corruption_and_cycles() {
        let mut duplicate = valid_byte_layout();
        Arc::make_mut(&mut duplicate.edges)[1] = vec![0, 0];
        assert!(matches!(
            duplicate.validate_layout(1),
            Err(DatValidationError::UnsortedOrDuplicateEdges { .. })
        ));

        let mut negative_base = valid_byte_layout();
        Arc::make_mut(&mut negative_base.base)[1] = -1;
        assert!(matches!(
            negative_base.validate_layout(1),
            Err(DatValidationError::NegativeBaseWithEdges { .. })
        ));

        let mut wrong_parent = valid_byte_layout();
        Arc::make_mut(&mut wrong_parent.check)[2] = 0;
        assert!(matches!(
            wrong_parent.validate_layout(1),
            Err(DatValidationError::WrongTransitionParent { .. })
        ));

        let mut cycle = valid_byte_layout();
        Arc::make_mut(&mut cycle.base)[1] = 1;
        Arc::make_mut(&mut cycle.check)[1] = 1;
        assert!(matches!(
            cycle.validate_layout(1),
            Err(DatValidationError::RepeatedOrCyclicTarget { .. })
        ));
    }

    #[test]
    fn layout_validation_rejects_orphans_and_nonfinal_values() {
        let mut orphan = valid_byte_layout();
        Arc::make_mut(&mut orphan.edges)[1].clear();
        Arc::make_mut(&mut orphan.base)[1] = -1;
        assert!(matches!(
            orphan.validate_layout(1),
            Err(DatValidationError::NonCanonicalUnreachableState { state: 2 })
        ));

        let mut nonfinal_value = valid_byte_layout();
        Arc::make_mut(&mut nonfinal_value.is_final)[2] = false;
        assert!(matches!(
            nonfinal_value.validate_layout(1),
            Err(DatValidationError::ValueOnNonFinalState { state: 2 })
        ));
    }

    #[test]
    fn trusted_edge_projection_matches_checked_projection() {
        let raw = valid_byte_layout();
        let checked = raw.clone();
        // SAFETY: `valid_byte_layout` is proved immediately above and declares
        // exactly one reachable final state.
        let sealed =
            unsafe { ValidatedDATCoreShared::<u8, u32, 1>::from_builder_parts_unchecked(raw, 1) };
        let root = SnapshotTraversalCursor::new(2).unwrap();
        let mut checked_edges = Vec::new();
        let checked_final = checked
            .filter_map_traversal_cursor(
                root,
                |label| Some(label.wrapping_add(1)),
                |label, child, projected| checked_edges.push((label, child, projected)),
            )
            .unwrap();
        let mut trusted_edges = Vec::new();
        // SAFETY: root is the sealed layout's root cursor.
        let trusted_final = unsafe {
            sealed.filter_map_traversal_cursor(
                root,
                |label| Some(label.wrapping_add(1)),
                |label, child, projected| trusted_edges.push((label, child, projected)),
            )
        }
        .unwrap();
        assert_eq!(trusted_final, checked_final);
        assert_eq!(trusted_edges, checked_edges);
    }
}
