//! Protobuf serializers for cross-language compatibility.

use crate::{Dictionary, DictionaryNode};
use std::io::{Read, Write};

use super::{DictionaryFromTerms, DictionarySerializer, SerializationError};

#[cfg(feature = "protobuf")]
use std::collections::{HashMap, HashSet};

/// Generated protobuf types
mod proto {
    #![allow(dead_code)]
    include!(concat!(env!("OUT_DIR"), "/libdictenstein.proto.rs"));
}

#[cfg(feature = "protobuf")]
const DAT_TERMS_MAGIC: &[u8] = b"LDT1";

#[cfg(feature = "protobuf")]
fn dictionary_error(message: impl Into<String>) -> SerializationError {
    SerializationError::DictionaryError(message.into())
}

#[cfg(feature = "protobuf")]
fn try_reserve_exact<T>(
    values: &mut Vec<T>,
    additional: usize,
    context: &'static str,
) -> Result<(), SerializationError> {
    if values.capacity() - values.len() < additional {
        #[cfg(test)]
        if allocation_fault::take(context) {
            return Err(vector_allocation_error(
                context,
                injected_vector_allocation_error(),
            ));
        }
        values
            .try_reserve_exact(additional)
            .map_err(|source| vector_allocation_error(context, source))?;
    }
    Ok(())
}

#[cfg(feature = "protobuf")]
#[cold]
fn vector_allocation_error(
    context: &'static str,
    source: std::collections::TryReserveError,
) -> SerializationError {
    SerializationError::Allocation { context, source }
}

#[cfg(feature = "protobuf")]
#[cold]
fn small_vector_allocation_error(
    context: &'static str,
    detail: smallvec::CollectionAllocErr,
) -> SerializationError {
    SerializationError::SmallVectorAllocation { context, detail }
}

#[cfg(feature = "protobuf")]
fn checked_capacity(
    estimate: usize,
    multiplier: usize,
    context: &'static str,
) -> Result<usize, SerializationError> {
    estimate
        .checked_mul(multiplier)
        .ok_or(SerializationError::CapacityOverflow { context })
}

#[cfg(all(feature = "protobuf", test))]
fn injected_vector_allocation_error() -> std::collections::TryReserveError {
    Vec::<u8>::new()
        .try_reserve(usize::MAX)
        .expect_err("usize::MAX reservation must overflow")
}

#[cfg(all(feature = "protobuf", test))]
mod allocation_fault {
    use std::cell::Cell;

    std::thread_local! {
        static FAIL_CONTEXT: Cell<Option<&'static str>> = const { Cell::new(None) };
    }

    pub(super) fn arm(context: &'static str) {
        FAIL_CONTEXT.with(|slot| slot.set(Some(context)));
    }

    pub(super) fn take(context: &'static str) -> bool {
        FAIL_CONTEXT.with(|slot| {
            if slot.get() == Some(context) {
                slot.set(None);
                true
            } else {
                false
            }
        })
    }
}

#[cfg(all(feature = "protobuf", test))]
mod pending_push_observation {
    use std::cell::Cell;

    std::thread_local! {
        static PUSHES: Cell<usize> = const { Cell::new(0) };
        static CURRENT_FRAMES: Cell<usize> = const { Cell::new(0) };
        static PEAK_FRAMES: Cell<usize> = const { Cell::new(0) };
        static FALLBACK_RESERVE_CHECKS: Cell<usize> = const { Cell::new(0) };
        static COUNTED_BATCH_RESERVATIONS: Cell<usize> = const { Cell::new(0) };
    }

    pub(super) fn reset() {
        PUSHES.with(|count| count.set(0));
        CURRENT_FRAMES.with(|count| count.set(0));
        PEAK_FRAMES.with(|count| count.set(0));
        FALLBACK_RESERVE_CHECKS.with(|count| count.set(0));
        COUNTED_BATCH_RESERVATIONS.with(|count| count.set(0));
    }

    pub(super) fn record_push() {
        PUSHES.with(|count| count.set(count.get() + 1));
        CURRENT_FRAMES.with(|current| {
            let next = current.get() + 1;
            current.set(next);
            PEAK_FRAMES.with(|peak| peak.set(peak.get().max(next)));
        });
    }

    pub(super) fn record_pop() {
        CURRENT_FRAMES.with(|count| {
            let current = count.get();
            assert!(current != 0, "pending-frame observation underflow");
            count.set(current - 1);
        });
    }

    pub(super) fn record_fallback_reserve_check() {
        FALLBACK_RESERVE_CHECKS.with(|count| count.set(count.get() + 1));
    }

    pub(super) fn record_counted_batch_reservation() {
        COUNTED_BATCH_RESERVATIONS.with(|count| count.set(count.get() + 1));
    }

    pub(super) fn pushes() -> usize {
        PUSHES.with(Cell::get)
    }

    pub(super) fn current_frames() -> usize {
        CURRENT_FRAMES.with(Cell::get)
    }

    pub(super) fn peak_frames() -> usize {
        PEAK_FRAMES.with(Cell::get)
    }

    pub(super) fn fallback_reserve_checks() -> usize {
        FALLBACK_RESERVE_CHECKS.with(Cell::get)
    }

    pub(super) fn counted_batch_reservations() -> usize {
        COUNTED_BATCH_RESERVATIONS.with(Cell::get)
    }
}

#[cfg(all(feature = "protobuf", test))]
mod cursor_path_observation {
    use std::cell::Cell;

    std::thread_local! {
        static OWNED_NODE_VISITS: Cell<usize> = const { Cell::new(0) };
        static CURSOR_EDGE_OBSERVATIONS: Cell<usize> = const { Cell::new(0) };
        static RANGE_STARTS: Cell<usize> = const { Cell::new(0) };
        static RANGE_STEPS: Cell<usize> = const { Cell::new(0) };
        static INDEXED_EDGE_OBSERVATIONS: Cell<usize> = const { Cell::new(0) };
        static FULL_CURSOR_VISITS: Cell<usize> = const { Cell::new(0) };
    }

    pub(super) fn reset() {
        OWNED_NODE_VISITS.with(|count| count.set(0));
        CURSOR_EDGE_OBSERVATIONS.with(|count| count.set(0));
        RANGE_STARTS.with(|count| count.set(0));
        RANGE_STEPS.with(|count| count.set(0));
        INDEXED_EDGE_OBSERVATIONS.with(|count| count.set(0));
        FULL_CURSOR_VISITS.with(|count| count.set(0));
    }

    pub(super) fn record_owned_node_visit() {
        OWNED_NODE_VISITS.with(|count| count.set(count.get() + 1));
    }

    pub(super) fn record_cursor_edge_observation() {
        CURSOR_EDGE_OBSERVATIONS.with(|count| count.set(count.get() + 1));
    }

    pub(super) fn record_range_start() {
        RANGE_STARTS.with(|count| count.set(count.get() + 1));
        record_cursor_edge_observation();
    }

    pub(super) fn record_range_step() {
        RANGE_STEPS.with(|count| count.set(count.get() + 1));
        record_cursor_edge_observation();
    }

    pub(super) fn record_indexed_edge_observation() {
        INDEXED_EDGE_OBSERVATIONS.with(|count| count.set(count.get() + 1));
        record_cursor_edge_observation();
    }

    pub(super) fn record_full_cursor_visit() {
        FULL_CURSOR_VISITS.with(|count| count.set(count.get() + 1));
    }

    pub(super) fn owned_node_visits() -> usize {
        OWNED_NODE_VISITS.with(Cell::get)
    }

    pub(super) fn cursor_edge_observations() -> usize {
        CURSOR_EDGE_OBSERVATIONS.with(Cell::get)
    }

    pub(super) fn range_starts() -> usize {
        RANGE_STARTS.with(Cell::get)
    }

    pub(super) fn range_steps() -> usize {
        RANGE_STEPS.with(Cell::get)
    }

    pub(super) fn indexed_edge_observations() -> usize {
        INDEXED_EDGE_OBSERVATIONS.with(Cell::get)
    }

    pub(super) fn full_cursor_visits() -> usize {
        FULL_CURSOR_VISITS.with(Cell::get)
    }
}

#[cfg(all(feature = "protobuf", test))]
mod v2_sink_observation {
    use std::cell::Cell;

    std::thread_local! {
        static FINAL_GROWTH_ATTEMPTS: Cell<usize> = const { Cell::new(0) };
        static EDGE_GROWTH_ATTEMPTS: Cell<usize> = const { Cell::new(0) };
        static COMMITS: Cell<usize> = const { Cell::new(0) };
    }

    pub(super) fn reset() {
        FINAL_GROWTH_ATTEMPTS.with(|count| count.set(0));
        EDGE_GROWTH_ATTEMPTS.with(|count| count.set(0));
        COMMITS.with(|count| count.set(0));
    }

    pub(super) fn record_final_growth_attempt() {
        FINAL_GROWTH_ATTEMPTS.with(|count| count.set(count.get() + 1));
    }

    pub(super) fn record_edge_growth_attempt() {
        EDGE_GROWTH_ATTEMPTS.with(|count| count.set(count.get() + 1));
    }

    pub(super) fn record_commit() {
        COMMITS.with(|count| count.set(count.get() + 1));
    }

    pub(super) fn final_growth_attempts() -> usize {
        FINAL_GROWTH_ATTEMPTS.with(Cell::get)
    }

    pub(super) fn edge_growth_attempts() -> usize {
        EDGE_GROWTH_ATTEMPTS.with(Cell::get)
    }

    pub(super) fn commits() -> usize {
        COMMITS.with(Cell::get)
    }
}

#[cfg(all(feature = "protobuf", test))]
mod cursor_path_fault {
    use std::cell::Cell;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(super) enum Action {
        Unavailable,
        ChangeFinality,
        ChangeTotal,
        SuppressCallbacks,
        DuplicateCallbacks,
    }

    #[derive(Clone, Copy)]
    struct ScheduledFault {
        action: Action,
        target_visit: usize,
        visits: usize,
    }

    std::thread_local! {
        static SCHEDULED: Cell<Option<ScheduledFault>> = const { Cell::new(None) };
    }

    pub(super) struct Guard;

    impl Drop for Guard {
        fn drop(&mut self) {
            SCHEDULED.with(|scheduled| scheduled.set(None));
        }
    }

    pub(super) fn arm() -> Guard {
        arm_on_visit(Action::Unavailable, 1)
    }

    pub(super) fn arm_on_visit(action: Action, target_visit: usize) -> Guard {
        assert!(target_visit != 0, "cursor-page visits are one-indexed");
        SCHEDULED.with(|scheduled| {
            scheduled.set(Some(ScheduledFault {
                action,
                target_visit,
                visits: 0,
            }));
        });
        Guard
    }

    pub(super) fn next_action() -> Option<Action> {
        SCHEDULED.with(|scheduled| {
            let mut state = scheduled.get()?;
            state.visits += 1;
            if state.visits == state.target_visit {
                scheduled.set(None);
                Some(state.action)
            } else {
                scheduled.set(Some(state));
                None
            }
        })
    }
}

#[cfg(feature = "protobuf")]
#[inline]
fn try_reserve_one<T>(
    values: &mut Vec<T>,
    context: &'static str,
) -> Result<(), SerializationError> {
    if values.len() == values.capacity() {
        try_reserve_one_slow(values, context)?;
    }
    Ok(())
}

#[cfg(feature = "protobuf")]
#[cold]
#[inline(never)]
fn try_reserve_one_slow<T>(
    values: &mut Vec<T>,
    context: &'static str,
) -> Result<(), SerializationError> {
    #[cfg(test)]
    if allocation_fault::take(context) {
        return Err(vector_allocation_error(
            context,
            injected_vector_allocation_error(),
        ));
    }
    values
        .try_reserve(1)
        .map_err(|source| vector_allocation_error(context, source))
}

#[cfg(feature = "protobuf")]
#[inline]
fn try_reserve_additional<T>(
    values: &mut Vec<T>,
    additional: usize,
    context: &'static str,
) -> Result<(), SerializationError> {
    if values.capacity() - values.len() < additional {
        try_reserve_additional_slow(values, additional, context)?;
    }
    Ok(())
}

#[cfg(feature = "protobuf")]
#[cold]
#[inline(never)]
fn try_reserve_additional_slow<T>(
    values: &mut Vec<T>,
    additional: usize,
    context: &'static str,
) -> Result<(), SerializationError> {
    #[cfg(test)]
    if allocation_fault::take(context) {
        return Err(vector_allocation_error(
            context,
            injected_vector_allocation_error(),
        ));
    }
    values
        .try_reserve(additional)
        .map_err(|source| vector_allocation_error(context, source))
}

/// Commits one already-authorized optimized-protobuf event without repeating
/// `Vec` capacity checks.
///
/// # Safety
///
/// The caller must hold the only mutable access to both vectors and must have
/// successfully established all of the following after the most recent
/// operation that could grow either vector:
///
/// - if `is_final`, `final_node_ids` has at least one spare element;
/// - `edge_data` has at least three spare elements;
/// - the vectors are distinct allocations and do not alias;
/// - neither vector's logical length changes until this function returns.
///
/// This function initializes every element before exposing it through
/// `set_len`.  It performs no allocation, callback, formatting, or other
/// fallible or panicking operation between initialization and length commit.
#[cfg(feature = "protobuf")]
#[inline(always)]
unsafe fn commit_v2_event_to_spare(
    final_node_ids: &mut Vec<u64>,
    edge_data: &mut Vec<u64>,
    event: (u64, u8, u64, bool),
) {
    let (source_id, label, target_id, is_final) = event;
    let final_len = final_node_ids.len();
    let edge_len = edge_data.len();
    debug_assert!(!is_final || final_node_ids.capacity() - final_len >= 1);
    debug_assert!(edge_data.capacity() - edge_len >= 3);

    if is_final {
        let final_spare = final_node_ids.spare_capacity_mut();
        // SAFETY: The caller proved that a final event has one spare slot,
        // and this borrow is dropped before either vector length changes.
        let final_slot = unsafe { final_spare.get_unchecked_mut(0) };
        final_slot.write(target_id);
    }
    {
        let edge_spare = edge_data.spare_capacity_mut();
        // SAFETY: The caller proved that three consecutive spare slots exist.
        // `u64` writes cannot panic, and the spare borrow ends before set_len.
        let source_slot = unsafe { edge_spare.get_unchecked_mut(0) };
        source_slot.write(source_id);
        let label_slot = unsafe { edge_spare.get_unchecked_mut(1) };
        label_slot.write(u64::from(label));
        let target_slot = unsafe { edge_spare.get_unchecked_mut(2) };
        target_slot.write(target_id);
    }

    // SAFETY: Every newly exposed slot was initialized above, the new lengths
    // are within the capacities proved by the caller, and no spare reference
    // remains live across either length update.
    if is_final {
        unsafe { final_node_ids.set_len(final_len + 1) };
    }
    unsafe { edge_data.set_len(edge_len + 3) };
}

/// Transactional local sink for one optimized-protobuf wire event.
///
/// Both fallible capacity checks complete before either vector's logical
/// length changes.  Therefore a failure leaves the optional-final vector and
/// the three-word packed-edge vector at their previous logical contents.  The
/// enclosing serializer publishes neither vector until the complete local
/// protobuf has been encoded successfully.
#[cfg(feature = "protobuf")]
struct V2GraphSink<'a> {
    final_node_ids: &'a mut Vec<u64>,
    edge_data: &'a mut Vec<u64>,
}

#[cfg(feature = "protobuf")]
impl<'a> V2GraphSink<'a> {
    #[inline(always)]
    fn new(final_node_ids: &'a mut Vec<u64>, edge_data: &'a mut Vec<u64>) -> Self {
        Self {
            final_node_ids,
            edge_data,
        }
    }

    #[inline(always)]
    fn emit(
        &mut self,
        source_id: u64,
        label: u8,
        target_id: u64,
        is_final: bool,
    ) -> Result<(), SerializationError> {
        if is_final {
            #[cfg(test)]
            if self.final_node_ids.len() == self.final_node_ids.capacity() {
                v2_sink_observation::record_final_growth_attempt();
            }
            try_reserve_one(self.final_node_ids, "protobuf v2 final-node table")?;
        }
        #[cfg(test)]
        if self.edge_data.capacity() - self.edge_data.len() < 3 {
            v2_sink_observation::record_edge_growth_attempt();
        }
        try_reserve_additional(self.edge_data, 3, "protobuf v2 edge table")?;

        // SAFETY: The two successful reservation guards above establish the
        // exact spare capacities required by commit_v2_event_to_spare.  No
        // vector operation occurs between those guards and this call.
        let event = (source_id, label, target_id, is_final);
        unsafe { commit_v2_event_to_spare(self.final_node_ids, self.edge_data, event) };
        #[cfg(test)]
        v2_sink_observation::record_commit();
        Ok(())
    }
}

#[cfg(feature = "protobuf")]
#[inline]
fn try_reserve_smallvec_one<A: smallvec::Array>(
    values: &mut smallvec::SmallVec<A>,
    context: &'static str,
) -> Result<(), SerializationError> {
    if values.len() == values.capacity() {
        #[cfg(test)]
        if allocation_fault::take(context) {
            return Err(small_vector_allocation_error(
                context,
                smallvec::CollectionAllocErr::CapacityOverflow,
            ));
        }
        values
            .try_reserve(1)
            .map_err(|detail| small_vector_allocation_error(context, detail))?;
    }
    Ok(())
}

#[cfg(feature = "protobuf")]
#[inline]
fn try_reserve_smallvec_additional<A: smallvec::Array>(
    values: &mut smallvec::SmallVec<A>,
    additional: usize,
    context: &'static str,
) -> Result<(), SerializationError> {
    if values.capacity() - values.len() < additional {
        #[cfg(test)]
        if allocation_fault::take(context) {
            return Err(small_vector_allocation_error(
                context,
                smallvec::CollectionAllocErr::CapacityOverflow,
            ));
        }
        values
            .try_reserve(additional)
            .map_err(|detail| small_vector_allocation_error(context, detail))?;
    }
    Ok(())
}

#[cfg(feature = "protobuf")]
fn checked_label_u32(label: u32, format: &str) -> Result<u8, SerializationError> {
    u8::try_from(label)
        .map_err(|_| dictionary_error(format!("{format} edge label {label} exceeds u8")))
}

#[cfg(feature = "protobuf")]
fn checked_label_u64(label: u64, format: &str) -> Result<u8, SerializationError> {
    u8::try_from(label)
        .map_err(|_| dictionary_error(format!("{format} edge label {label} exceeds u8")))
}

#[cfg(feature = "protobuf")]
fn validate_term_count(
    expected: u64,
    actual: usize,
    format: &str,
) -> Result<(), SerializationError> {
    let expected = usize::try_from(expected)
        .map_err(|_| dictionary_error(format!("{format} term count does not fit usize")))?;
    if expected == actual {
        Ok(())
    } else {
        Err(dictionary_error(format!(
            "{format} term count mismatch: expected {expected}, decoded {actual}"
        )))
    }
}

#[cfg(feature = "protobuf")]
fn ensure_reachable_acyclic(
    root_id: u64,
    adjacency: &HashMap<u64, Vec<(u8, u64)>>,
) -> Result<(), SerializationError> {
    let mut visiting = HashSet::with_capacity(adjacency.len());
    let mut visited = HashSet::with_capacity(adjacency.len());
    let mut stack = vec![(root_id, 0usize)];
    visiting.insert(root_id);

    while let Some((node_id, next_edge)) = stack.last_mut() {
        let edges = adjacency.get(node_id).map(Vec::as_slice).unwrap_or(&[]);
        if let Some(&(_, target_id)) = edges.get(*next_edge) {
            *next_edge += 1;
            if visited.contains(&target_id) {
                continue;
            }
            if !visiting.insert(target_id) {
                return Err(dictionary_error(format!(
                    "protobuf graph contains a reachable cycle at node {target_id}"
                )));
            }
            stack.push((target_id, 0));
        } else {
            let (completed, _) = stack.pop().expect("the DFS stack is non-empty");
            visiting.remove(&completed);
            visited.insert(completed);
        }
    }
    Ok(())
}

#[cfg(feature = "protobuf")]
fn terms_from_adjacency(
    root_id: u64,
    adjacency: &HashMap<u64, Vec<(u8, u64)>>,
    final_set: &HashSet<u64>,
) -> Result<Vec<String>, SerializationError> {
    ensure_reachable_acyclic(root_id, adjacency)?;

    struct Frame {
        node_id: u64,
        next_edge: usize,
        restore_len: usize,
        entered: bool,
    }

    let mut terms = Vec::with_capacity(final_set.len());
    let mut current_term = Vec::with_capacity(32);
    let mut stack = vec![Frame {
        node_id: root_id,
        next_edge: 0,
        restore_len: 0,
        entered: false,
    }];

    while let Some(frame) = stack.last_mut() {
        if !frame.entered {
            frame.entered = true;
            if final_set.contains(&frame.node_id) {
                let term = String::from_utf8(current_term.clone()).map_err(|_| {
                    dictionary_error("protobuf graph produced a non-UTF-8 dictionary term")
                })?;
                terms.push(term);
            }
        }

        let edges = adjacency
            .get(&frame.node_id)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        if let Some(&(label, target_id)) = edges.get(frame.next_edge) {
            frame.next_edge += 1;
            let restore_len = current_term.len();
            current_term.push(label);
            stack.push(Frame {
                node_id: target_id,
                next_edge: 0,
                restore_len,
                entered: false,
            });
        } else {
            let completed = stack.pop().expect("the traversal stack is non-empty");
            current_term.truncate(completed.restore_len);
        }
    }
    Ok(terms)
}

#[cfg(feature = "protobuf")]
fn insert_deterministic_edge(
    adjacency: &mut HashMap<u64, Vec<(u8, u64)>>,
    source_id: u64,
    label: u8,
    target_id: u64,
    format: &str,
) -> Result<(), SerializationError> {
    let edges = adjacency.entry(source_id).or_default();
    if edges.iter().any(|&(existing, _)| existing == label) {
        return Err(dictionary_error(format!(
            "{format} node {source_id} has duplicate outgoing label {label}"
        )));
    }
    edges.push((label, target_id));
    Ok(())
}

/// Emit the path-expanded trie graph in final-first, edge-encounter DFS order.
///
/// One pending-edge stack owns only sibling continuations that recursive
/// callers would retain on their native stacks. The first child is traversed
/// directly; later siblings are reversed in place so popping reproduces exact
/// DFS order. Unary chains therefore use no pending frames. Node IDs are
/// assigned when an edge is entered, exactly as in the recursive oracle.
#[cfg(feature = "protobuf")]
struct PendingEdge<N> {
    source_id: u64,
    label: u8,
    child: N,
}

/// A revision-local cursor whose invariant lifetime cannot be named outside
/// one retained traversal. The raw backend capability is private so cursors
/// can only originate at the retained root or from an admitted callback.
#[cfg(feature = "protobuf")]
struct BrandedSnapshotCursor<'brand, C: Copy> {
    raw: C,
    _invariant: std::marker::PhantomData<fn(&'brand mut ()) -> &'brand mut ()>,
}

#[cfg(feature = "protobuf")]
impl<C: Copy> Copy for BrandedSnapshotCursor<'_, C> {}

#[cfg(feature = "protobuf")]
impl<C: Copy> Clone for BrandedSnapshotCursor<'_, C> {
    fn clone(&self) -> Self {
        *self
    }
}

/// A retained raw range whose exact owner revision is hidden behind the same
/// invariant brand as every cursor emitted from it.
#[cfg(feature = "protobuf")]
struct BrandedSnapshotEdgeRange<'brand, N: DictionaryNode> {
    raw: crate::SnapshotEdgeRangeToken<N>,
    _invariant: std::marker::PhantomData<fn(&'brand mut ()) -> &'brand mut ()>,
}

#[cfg(feature = "protobuf")]
impl<N: DictionaryNode> Copy for BrandedSnapshotEdgeRange<'_, N> {}

#[cfg(feature = "protobuf")]
impl<N: DictionaryNode> Clone for BrandedSnapshotEdgeRange<'_, N> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}

#[cfg(feature = "protobuf")]
type BrandedRangeStart<'brand, N> = (
    bool,
    usize,
    Option<(
        u8,
        BrandedSnapshotCursor<'brand, <N as DictionaryNode>::SnapshotCursor>,
    )>,
    Option<BrandedSnapshotEdgeRange<'brand, N>>,
);

#[cfg(feature = "protobuf")]
type BrandedRangeStep<'brand, N> = (
    u8,
    BrandedSnapshotCursor<'brand, <N as DictionaryNode>::SnapshotCursor>,
    Option<BrandedSnapshotEdgeRange<'brand, N>>,
);

/// Safe, non-escaping access to one backend's unsafe retained-cursor API.
#[cfg(feature = "protobuf")]
struct RetainedSnapshotTraversal<'owner, 'brand, N: DictionaryNode> {
    owner: &'owner N,
    _invariant: std::marker::PhantomData<fn(&'brand mut ()) -> &'brand mut ()>,
}

#[cfg(feature = "protobuf")]
impl<'owner, 'brand, N> RetainedSnapshotTraversal<'owner, 'brand, N>
where
    N: DictionaryNode<Unit = u8>,
{
    #[inline]
    fn brand(&self, raw: N::SnapshotCursor) -> BrandedSnapshotCursor<'brand, N::SnapshotCursor> {
        BrandedSnapshotCursor {
            raw,
            _invariant: std::marker::PhantomData,
        }
    }

    #[inline]
    fn brand_range(
        &self,
        raw: crate::SnapshotEdgeRangeToken<N>,
    ) -> BrandedSnapshotEdgeRange<'brand, N> {
        BrandedSnapshotEdgeRange {
            raw,
            _invariant: std::marker::PhantomData,
        }
    }

    #[inline]
    fn range_start(
        &self,
        cursor: BrandedSnapshotCursor<'brand, N::SnapshotCursor>,
    ) -> Option<BrandedRangeStart<'brand, N>> {
        #[cfg(test)]
        {
            cursor_path_observation::record_range_start();
            let action = cursor_path_fault::next_action();
            if matches!(
                action,
                Some(cursor_path_fault::Action::Unavailable)
                    | Some(cursor_path_fault::Action::DuplicateCallbacks)
            ) {
                return None;
            }
            // SAFETY: the cursor and every returned capability remain inside
            // this HRTB brand while the exact producing owner is borrowed.
            let start = unsafe { self.owner.snapshot_cursor_edge_range_start(cursor.raw) }?;
            let mut is_final = start.is_final();
            let mut total = start.total_edge_count();
            let (mut first, remaining) = start.into_first_and_remaining();
            match action {
                Some(cursor_path_fault::Action::ChangeFinality) => is_final = !is_final,
                Some(cursor_path_fault::Action::ChangeTotal) => total = 257,
                Some(cursor_path_fault::Action::SuppressCallbacks) => first = None,
                _ => {}
            }
            Some((
                is_final,
                total,
                first.map(|(label, child)| (label, self.brand(child))),
                remaining.map(|token| self.brand_range(token)),
            ))
        }

        #[cfg(not(test))]
        {
            // SAFETY: identical retained-owner and HRTB branding argument to
            // the test-enabled path above.
            let start = unsafe { self.owner.snapshot_cursor_edge_range_start(cursor.raw) }?;
            let is_final = start.is_final();
            let total = start.total_edge_count();
            let (first, remaining) = start.into_first_and_remaining();
            Some((
                is_final,
                total,
                first.map(|(label, child)| (label, self.brand(child))),
                remaining.map(|token| self.brand_range(token)),
            ))
        }
    }

    #[inline]
    fn range_step(
        &self,
        token: BrandedSnapshotEdgeRange<'brand, N>,
    ) -> Option<BrandedRangeStep<'brand, N>> {
        #[cfg(test)]
        {
            cursor_path_observation::record_range_step();
            if cursor_path_fault::next_action().is_some() {
                return None;
            }
        }
        // SAFETY: the HRTB brand proves that the token cannot escape or be
        // mixed with another receiver/revision, and the owner remains borrowed.
        let (label, child, remaining) =
            unsafe { self.owner.snapshot_cursor_edge_range_step(token.raw) }?;
        Some((
            label,
            self.brand(child),
            remaining.map(|next| self.brand_range(next)),
        ))
    }

    #[inline]
    fn edge_at(
        &self,
        cursor: BrandedSnapshotCursor<'brand, N::SnapshotCursor>,
        index: usize,
    ) -> Option<
        crate::SnapshotCursorEdgeObservation<u8, BrandedSnapshotCursor<'brand, N::SnapshotCursor>>,
    > {
        #[cfg(test)]
        {
            cursor_path_observation::record_indexed_edge_observation();
            let action = cursor_path_fault::next_action();
            if matches!(
                action,
                Some(cursor_path_fault::Action::Unavailable)
                    | Some(cursor_path_fault::Action::DuplicateCallbacks)
            ) {
                return None;
            }
            // SAFETY: `cursor` is branded inside
            // `with_retained_snapshot_traversal`; the exact producing owner is
            // borrowed for the complete invariant brand.
            let observation = unsafe { self.owner.snapshot_cursor_edge_at(cursor.raw, index) }?;
            let mut is_final = observation.is_final();
            let mut total = observation.total_edge_count();
            let mut edge = observation
                .into_edge()
                .map(|(label, child)| (label, self.brand(child)));
            match action {
                Some(cursor_path_fault::Action::ChangeFinality) => is_final = !is_final,
                Some(cursor_path_fault::Action::ChangeTotal) => total = 257,
                Some(cursor_path_fault::Action::SuppressCallbacks) => edge = None,
                _ => {}
            }
            Some(crate::SnapshotCursorEdgeObservation::new(
                is_final, total, edge,
            ))
        }

        #[cfg(not(test))]
        {
            // SAFETY: identical retained-owner and branding argument to the
            // test-enabled path above.
            let observation = unsafe { self.owner.snapshot_cursor_edge_at(cursor.raw, index) }?;
            Some(crate::SnapshotCursorEdgeObservation::new(
                observation.is_final(),
                observation.total_edge_count(),
                observation
                    .into_edge()
                    .map(|(label, child)| (label, self.brand(child))),
            ))
        }
    }

    #[inline]
    fn visit_all<F>(
        &self,
        cursor: BrandedSnapshotCursor<'brand, N::SnapshotCursor>,
        mut visitor: F,
    ) -> Option<bool>
    where
        F: FnMut(u8, BrandedSnapshotCursor<'brand, N::SnapshotCursor>),
    {
        #[cfg(test)]
        cursor_path_observation::record_full_cursor_visit();
        // SAFETY: identical retained-owner and branding argument to
        // `visit_page`; rejected projections cannot emit a cursor.
        unsafe {
            self.owner.filter_map_snapshot_cursor_edges_and_finality(
                cursor.raw,
                |_| Some(()),
                |label, child, ()| visitor(label, self.brand(child)),
            )
        }
    }
}

/// Introduce a fresh invariant cursor brand and consume every branded cursor
/// before the retained owner may be released.
#[cfg(feature = "protobuf")]
#[inline]
fn with_retained_snapshot_traversal<'owner, N, R, F>(
    owner: &'owner N,
    root: N::SnapshotCursor,
    operation: F,
) -> R
where
    N: DictionaryNode<Unit = u8>,
    F: for<'brand> FnOnce(
        RetainedSnapshotTraversal<'owner, 'brand, N>,
        BrandedSnapshotCursor<'brand, N::SnapshotCursor>,
    ) -> R,
{
    operation(
        RetainedSnapshotTraversal {
            owner,
            _invariant: std::marker::PhantomData,
        },
        BrandedSnapshotCursor {
            raw: root,
            _invariant: std::marker::PhantomData,
        },
    )
}

#[cfg(feature = "protobuf")]
struct PendingCursorEdge<'brand, C: Copy> {
    source_id: u64,
    label: u8,
    child: BrandedSnapshotCursor<'brand, C>,
}

#[cfg(feature = "protobuf")]
type CursorChildSchedule<'brand, C> =
    Result<(bool, Option<PendingCursorEdge<'brand, C>>), SerializationError>;

/// One paused recursive-parent iterator in the efficient cursor scheduler.
///
/// The private invariant is `1 <= next_index < total`.  The frame is pushed
/// before first-child descent and updated only after a resumed capacity-one
/// page has passed metadata and callback-cardinality validation.
#[cfg(feature = "protobuf")]
struct ParentCursorContinuation<'brand, C: Copy> {
    source_id: u64,
    parent: BrandedSnapshotCursor<'brand, C>,
    next_index: u16,
    total: u16,
    first_finality: bool,
}

#[cfg(feature = "protobuf")]
impl<C: Copy> Copy for ParentCursorContinuation<'_, C> {}

#[cfg(feature = "protobuf")]
impl<C: Copy> Clone for ParentCursorContinuation<'_, C> {
    fn clone(&self) -> Self {
        *self
    }
}

/// One paused recursive-parent iterator backed by an immutable edge suffix.
///
/// The range is nonempty by construction. A successful step either replaces
/// it with the exact nonempty suffix or removes the frame at the one-past end.
#[cfg(feature = "protobuf")]
struct ParentRangeContinuation<'brand, N: DictionaryNode> {
    source_id: u64,
    remaining: BrandedSnapshotEdgeRange<'brand, N>,
}

#[cfg(feature = "protobuf")]
impl<N: DictionaryNode> Copy for ParentRangeContinuation<'_, N> {}

#[cfg(feature = "protobuf")]
impl<N: DictionaryNode> Clone for ParentRangeContinuation<'_, N> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}

#[cfg(feature = "protobuf")]
#[cold]
#[inline(never)]
fn snapshot_cursor_page_error(detail: &'static str) -> SerializationError {
    dictionary_error(format!(
        "protobuf snapshot cursor page contract failed: {detail}"
    ))
}

#[cfg(feature = "protobuf")]
#[inline]
fn try_push_pending_edge<N>(
    pending: &mut smallvec::SmallVec<[PendingEdge<N>; 16]>,
    edge: PendingEdge<N>,
) -> Result<(), SerializationError> {
    #[cfg(test)]
    {
        pending_push_observation::record_push();
        pending_push_observation::record_fallback_reserve_check();
    }
    try_reserve_smallvec_one(pending, "protobuf pending-edge worklist")?;
    pending.push(edge);
    Ok(())
}

#[cfg(feature = "protobuf")]
#[cold]
#[inline(never)]
fn edge_count_mismatch(declared: usize, observed: Option<usize>) -> SerializationError {
    match observed {
        Some(observed) => dictionary_error(format!(
            "protobuf node edge_count mismatch: declared {declared}, observed {observed}"
        )),
        None => dictionary_error(format!(
            "protobuf node edge_count mismatch: declared {declared}, observed more than {declared}"
        )),
    }
}

#[cfg(feature = "protobuf")]
#[inline(always)]
fn append_counted_empty_children<N>(node: &N) -> Result<Option<PendingEdge<N>>, SerializationError>
where
    N: DictionaryNode<Unit = u8>,
{
    let mut saw_child = false;
    node.for_each_edge(|_, _| saw_child = true);
    if saw_child {
        Err(edge_count_mismatch(0, None))
    } else {
        Ok(None)
    }
}

#[cfg(feature = "protobuf")]
#[inline(always)]
fn append_counted_unary_child<N>(
    node: &N,
    source_id: u64,
) -> Result<Option<PendingEdge<N>>, SerializationError>
where
    N: DictionaryNode<Unit = u8>,
{
    let mut direct_child = None;
    let mut saw_extra_child = false;
    node.for_each_edge(|label, child| {
        if direct_child.is_none() {
            direct_child = Some(PendingEdge {
                source_id,
                label,
                child,
            });
        } else {
            saw_extra_child = true;
        }
    });
    if saw_extra_child {
        Err(edge_count_mismatch(1, None))
    } else if direct_child.is_none() {
        Err(edge_count_mismatch(1, Some(0)))
    } else {
        Ok(direct_child)
    }
}

#[cfg(feature = "protobuf")]
#[inline(never)]
fn append_counted_multiple_children<N>(
    node: &N,
    source_id: u64,
    pending: &mut smallvec::SmallVec<[PendingEdge<N>; 16]>,
    declared: usize,
) -> Result<Option<PendingEdge<N>>, SerializationError>
where
    N: DictionaryNode<Unit = u8>,
{
    debug_assert!(declared >= 2);
    let first_sibling = pending.len();
    let sibling_budget = declared - 1;
    #[cfg(test)]
    pending_push_observation::record_counted_batch_reservation();
    try_reserve_smallvec_additional(pending, sibling_budget, "protobuf pending-edge worklist")?;

    let mut direct_child = None;
    let mut observed = 0usize;
    let mut saw_extra_child = false;
    node.for_each_edge(|label, child| {
        if observed == declared {
            saw_extra_child = true;
            return;
        }

        let edge = PendingEdge {
            source_id,
            label,
            child,
        };
        if observed == 0 {
            direct_child = Some(edge);
        } else {
            #[cfg(test)]
            pending_push_observation::record_push();
            pending.push(edge);
        }
        observed += 1;
    });

    if saw_extra_child || observed != declared {
        pending.truncate(first_sibling);
        return Err(edge_count_mismatch(
            declared,
            (!saw_extra_child).then_some(observed),
        ));
    }
    pending[first_sibling..].reverse();
    Ok(direct_child)
}

#[cfg(feature = "protobuf")]
#[inline(never)]
fn append_uncounted_children<N>(
    node: &N,
    source_id: u64,
    pending: &mut smallvec::SmallVec<[PendingEdge<N>; 16]>,
) -> Result<Option<PendingEdge<N>>, SerializationError>
where
    N: DictionaryNode<Unit = u8>,
{
    let first_sibling = pending.len();
    let mut direct_child = None;
    let mut push_error = None;
    node.for_each_edge(|label, child| {
        if push_error.is_none() {
            let edge = PendingEdge {
                source_id,
                label,
                child,
            };
            if direct_child.is_none() {
                direct_child = Some(edge);
            } else {
                push_error = try_push_pending_edge(pending, edge).err();
            }
        }
    });
    if let Some(error) = push_error {
        pending.truncate(first_sibling);
        return Err(error);
    }
    pending[first_sibling..].reverse();
    Ok(direct_child)
}

#[cfg(feature = "protobuf")]
#[inline(always)]
fn append_pending_children<N>(
    node: &N,
    source_id: u64,
    pending: &mut smallvec::SmallVec<[PendingEdge<N>; 16]>,
) -> Result<Option<PendingEdge<N>>, SerializationError>
where
    N: DictionaryNode<Unit = u8>,
{
    #[cfg(test)]
    cursor_path_observation::record_owned_node_visit();
    match node.edge_count() {
        Some(0) => append_counted_empty_children(node),
        Some(1) => append_counted_unary_child(node, source_id),
        Some(declared) => append_counted_multiple_children(node, source_id, pending, declared),
        None => append_uncounted_children(node, source_id, pending),
    }
}

#[cfg(feature = "protobuf")]
#[cold]
#[inline(never)]
fn snapshot_cursor_range_error(detail: &'static str) -> SerializationError {
    dictionary_error(format!(
        "protobuf retained edge-range contract failed: {detail}"
    ))
}

#[cfg(feature = "protobuf")]
#[inline(always)]
fn append_range_cursor_children<'owner, 'brand, N>(
    traversal: &RetainedSnapshotTraversal<'owner, 'brand, N>,
    cursor: BrandedSnapshotCursor<'brand, N::SnapshotCursor>,
    source_id: u64,
    pending: &mut Vec<ParentRangeContinuation<'brand, N>>,
) -> CursorChildSchedule<'brand, N::SnapshotCursor>
where
    N: DictionaryNode<Unit = u8>,
{
    let Some((is_final, total, first, remaining)) = traversal.range_start(cursor) else {
        return Err(snapshot_cursor_range_error("start observation unavailable"));
    };
    if total > usize::from(u8::MAX) + 1 {
        return Err(snapshot_cursor_range_error(
            "edge count exceeds deterministic byte fanout",
        ));
    }
    let shape_is_valid = match total {
        0 => first.is_none() && remaining.is_none(),
        1 => first.is_some() && remaining.is_none(),
        _ => first.is_some() && remaining.is_some(),
    };
    if !shape_is_valid {
        return Err(snapshot_cursor_range_error(
            "first edge or remaining suffix disagrees with total",
        ));
    }

    if let Some(remaining) = remaining {
        try_reserve_one(pending, "protobuf pending-edge worklist")?;
        #[cfg(test)]
        {
            pending_push_observation::record_counted_batch_reservation();
            pending_push_observation::record_push();
        }
        pending.push(ParentRangeContinuation {
            source_id,
            remaining,
        });
    }
    let direct_child = first.map(|(label, child)| PendingCursorEdge {
        source_id,
        label,
        child,
    });
    Ok((is_final, direct_child))
}

#[cfg(feature = "protobuf")]
#[inline(always)]
fn resume_range_cursor_parent<'owner, 'brand, N>(
    traversal: &RetainedSnapshotTraversal<'owner, 'brand, N>,
    pending: &mut Vec<ParentRangeContinuation<'brand, N>>,
) -> Result<Option<PendingCursorEdge<'brand, N::SnapshotCursor>>, SerializationError>
where
    N: DictionaryNode<Unit = u8>,
{
    let Some(frame) = pending.last().copied() else {
        return Ok(None);
    };
    let Some((label, child, remaining)) = traversal.range_step(frame.remaining) else {
        return Err(snapshot_cursor_range_error(
            "resumed sibling step unavailable",
        ));
    };

    // Commit the continuation transition only after the backend step succeeds.
    if let Some(remaining) = remaining {
        pending
            .last_mut()
            .expect("successful range step retains its parent frame")
            .remaining = remaining;
    } else {
        let removed = pending.pop();
        debug_assert!(removed.is_some());
        #[cfg(test)]
        pending_push_observation::record_pop();
    }
    Ok(Some(PendingCursorEdge {
        source_id: frame.source_id,
        label,
        child,
    }))
}

#[cfg(feature = "protobuf")]
fn visit_range_cursor_path_expanded_graph<'owner, 'brand, N, F>(
    traversal: &RetainedSnapshotTraversal<'owner, 'brand, N>,
    root_cursor: BrandedSnapshotCursor<'brand, N::SnapshotCursor>,
    mut visit_edge: F,
) -> Result<(), SerializationError>
where
    N: DictionaryNode<Unit = u8>,
    F: FnMut(u64, u8, u64, bool) -> Result<(), SerializationError>,
{
    let mut pending: Vec<ParentRangeContinuation<'brand, N>> = Vec::new();
    let (_, mut direct_child) =
        append_range_cursor_children(traversal, root_cursor, 0, &mut pending)?;
    let mut next_id = 1u64;

    loop {
        while let Some(PendingCursorEdge {
            source_id,
            label,
            child,
        }) = direct_child
        {
            let child_id = next_id;
            next_id = next_id
                .checked_add(1)
                .ok_or_else(|| dictionary_error("protobuf path-expanded node ID overflow"))?;
            let (child_finality, next_direct_child) =
                append_range_cursor_children(traversal, child, child_id, &mut pending)?;
            visit_edge(source_id, label, child_id, child_finality)?;
            direct_child = next_direct_child;
        }
        direct_child = resume_range_cursor_parent(traversal, &mut pending)?;
        if direct_child.is_none() {
            break;
        }
    }
    Ok(())
}

#[cfg(feature = "protobuf")]
#[inline(always)]
fn append_paged_cursor_children<'owner, 'brand, N>(
    traversal: &RetainedSnapshotTraversal<'owner, 'brand, N>,
    cursor: BrandedSnapshotCursor<'brand, N::SnapshotCursor>,
    source_id: u64,
    pending: &mut Vec<ParentCursorContinuation<'brand, N::SnapshotCursor>>,
) -> CursorChildSchedule<'brand, N::SnapshotCursor>
where
    N: DictionaryNode<Unit = u8>,
{
    let observation = traversal
        .edge_at(cursor, 0)
        .ok_or_else(|| snapshot_cursor_page_error("first indexed observation unavailable"))?;
    let is_final = observation.is_final();
    let total = observation.total_edge_count();
    let direct_child = observation
        .into_edge()
        .map(|(label, child)| PendingCursorEdge {
            source_id,
            label,
            child,
        });
    if direct_child.is_some() != (total != 0) {
        return Err(snapshot_cursor_page_error(
            "first indexed edge disagrees with total",
        ));
    }

    if total >= 2 {
        if total > usize::from(u8::MAX) + 1 {
            return Err(snapshot_cursor_page_error(
                "edge count exceeds deterministic byte fanout",
            ));
        }
        try_reserve_one(pending, "protobuf pending-edge worklist")?;
        #[cfg(test)]
        {
            pending_push_observation::record_counted_batch_reservation();
            pending_push_observation::record_push();
        }
        pending.push(ParentCursorContinuation {
            source_id,
            parent: cursor,
            next_index: 1,
            total: total as u16,
            first_finality: is_final,
        });
    }
    Ok((is_final, direct_child))
}

#[cfg(feature = "protobuf")]
#[inline(never)]
fn resume_paged_cursor_parent<'owner, 'brand, N>(
    traversal: &RetainedSnapshotTraversal<'owner, 'brand, N>,
    pending: &mut Vec<ParentCursorContinuation<'brand, N::SnapshotCursor>>,
) -> Result<Option<PendingCursorEdge<'brand, N::SnapshotCursor>>, SerializationError>
where
    N: DictionaryNode<Unit = u8>,
{
    let Some(frame) = pending.last().copied() else {
        return Ok(None);
    };
    debug_assert!(frame.next_index != 0);
    debug_assert!(frame.next_index < frame.total);

    let next_index = usize::from(frame.next_index);
    let total = usize::from(frame.total);
    let Some(observation) = traversal.edge_at(frame.parent, next_index) else {
        return Err(snapshot_cursor_page_error(
            "resumed indexed observation unavailable",
        ));
    };
    let metadata_is_valid =
        observation.is_final() == frame.first_finality && observation.total_edge_count() == total;
    let resumed_child = observation
        .into_edge()
        .map(|(label, child)| PendingCursorEdge {
            source_id: frame.source_id,
            label,
            child,
        });
    if !metadata_is_valid || resumed_child.is_none() {
        return Err(snapshot_cursor_page_error(
            "resumed indexed observation changed metadata or omitted its edge",
        ));
    }

    // The private frame invariant proves this addition cannot overflow:
    // `next_index < total <= 256`.  State changes only after the page
    // has passed every observable validation above.
    let advanced_index = frame.next_index + 1;
    if advanced_index == frame.total {
        let removed = pending.pop();
        debug_assert!(removed.is_some());
        #[cfg(test)]
        pending_push_observation::record_pop();
    } else {
        debug_assert!(advanced_index < frame.total);
        pending
            .last_mut()
            .expect("validated parent continuation must remain present")
            .next_index = advanced_index;
    }
    Ok(resumed_child)
}

#[cfg(feature = "protobuf")]
#[inline(never)]
fn append_uncounted_cursor_children<'owner, 'brand, N>(
    traversal: &RetainedSnapshotTraversal<'owner, 'brand, N>,
    cursor: BrandedSnapshotCursor<'brand, N::SnapshotCursor>,
    source_id: u64,
    pending: &mut smallvec::SmallVec<[PendingCursorEdge<'brand, N::SnapshotCursor>; 16]>,
) -> CursorChildSchedule<'brand, N::SnapshotCursor>
where
    N: DictionaryNode<Unit = u8>,
{
    let first_sibling = pending.len();
    let mut direct_child = None;
    let mut push_error = None;
    let finality = traversal.visit_all(cursor, |label, child| {
        if push_error.is_none() {
            let edge = PendingCursorEdge {
                source_id,
                label,
                child,
            };
            if direct_child.is_none() {
                direct_child = Some(edge);
            } else if let Err(error) =
                try_reserve_smallvec_one(pending, "protobuf pending-edge worklist")
            {
                push_error = Some(error);
            } else {
                pending.push(edge);
            }
        }
    });
    if let Some(error) = push_error {
        pending.truncate(first_sibling);
        return Err(error);
    }
    let Some(finality) = finality else {
        pending.truncate(first_sibling);
        return Err(snapshot_cursor_page_error(
            "cursor traversal became unavailable after root selection",
        ));
    };
    pending[first_sibling..].reverse();
    Ok((finality, direct_child))
}

#[cfg(feature = "protobuf")]
fn visit_paged_cursor_path_expanded_graph<'owner, 'brand, N, F>(
    traversal: &RetainedSnapshotTraversal<'owner, 'brand, N>,
    root_cursor: BrandedSnapshotCursor<'brand, N::SnapshotCursor>,
    mut visit_edge: F,
) -> Result<(), SerializationError>
where
    N: DictionaryNode<Unit = u8>,
    F: FnMut(u64, u8, u64, bool) -> Result<(), SerializationError>,
{
    let mut pending: Vec<ParentCursorContinuation<'brand, N::SnapshotCursor>> = Vec::new();
    let (_, mut direct_child) =
        append_paged_cursor_children(traversal, root_cursor, 0, &mut pending)?;
    let mut next_id = 1u64;

    loop {
        while let Some(PendingCursorEdge {
            source_id,
            label,
            child,
        }) = direct_child
        {
            let child_id = next_id;
            next_id = next_id
                .checked_add(1)
                .ok_or_else(|| dictionary_error("protobuf path-expanded node ID overflow"))?;
            let (child_finality, next_direct_child) =
                append_paged_cursor_children(traversal, child, child_id, &mut pending)?;
            visit_edge(source_id, label, child_id, child_finality)?;
            direct_child = next_direct_child;
        }
        direct_child = resume_paged_cursor_parent(traversal, &mut pending)?;
        if direct_child.is_none() {
            break;
        }
    }
    Ok(())
}

#[cfg(feature = "protobuf")]
fn visit_uncounted_cursor_path_expanded_graph<'owner, 'brand, N, F>(
    traversal: &RetainedSnapshotTraversal<'owner, 'brand, N>,
    root_cursor: BrandedSnapshotCursor<'brand, N::SnapshotCursor>,
    mut visit_edge: F,
) -> Result<(), SerializationError>
where
    N: DictionaryNode<Unit = u8>,
    F: FnMut(u64, u8, u64, bool) -> Result<(), SerializationError>,
{
    let mut pending: smallvec::SmallVec<[PendingCursorEdge<'brand, N::SnapshotCursor>; 16]> =
        smallvec::SmallVec::new();
    let (_, mut direct_child) =
        append_uncounted_cursor_children(traversal, root_cursor, 0, &mut pending)?;
    let mut next_id = 1u64;

    loop {
        while let Some(PendingCursorEdge {
            source_id,
            label,
            child,
        }) = direct_child
        {
            let child_id = next_id;
            next_id = next_id
                .checked_add(1)
                .ok_or_else(|| dictionary_error("protobuf path-expanded node ID overflow"))?;
            let (child_finality, next_direct_child) =
                append_uncounted_cursor_children(traversal, child, child_id, &mut pending)?;
            visit_edge(source_id, label, child_id, child_finality)?;
            direct_child = next_direct_child;
        }
        direct_child = pending.pop();
        if direct_child.is_none() {
            break;
        }
    }
    Ok(())
}

#[cfg(feature = "protobuf")]
fn visit_owned_path_expanded_graph<N, F>(
    root: &N,
    mut visit_edge: F,
) -> Result<(), SerializationError>
where
    N: DictionaryNode<Unit = u8>,
    F: FnMut(u64, u8, u64, bool) -> Result<(), SerializationError>,
{
    let mut pending: smallvec::SmallVec<[PendingEdge<N>; 16]> = smallvec::SmallVec::new();
    let mut direct_child = append_pending_children(root, 0, &mut pending)?;
    let mut next_id = 1u64;

    loop {
        while let Some(PendingEdge {
            source_id,
            label,
            child,
        }) = direct_child
        {
            let child_id = next_id;
            next_id = next_id
                .checked_add(1)
                .ok_or_else(|| dictionary_error("protobuf path-expanded node ID overflow"))?;
            visit_edge(source_id, label, child_id, child.is_final())?;
            direct_child = append_pending_children(&child, child_id, &mut pending)?;
        }
        direct_child = match pending.pop() {
            Some(edge) => Some(edge),
            None => break,
        };
    }
    Ok(())
}

#[cfg(feature = "protobuf")]
fn visit_path_expanded_graph<N, F>(root: &N, mut visit_edge: F) -> Result<(), SerializationError>
where
    N: DictionaryNode<Unit = u8>,
    F: FnMut(u64, u8, u64, bool) -> Result<(), SerializationError>,
{
    if let Some(root_cursor) = root.snapshot_root_cursor() {
        return with_retained_snapshot_traversal(root, root_cursor, |traversal, branded_root| {
            if root.supports_efficient_snapshot_cursor_edge_ranges() {
                visit_range_cursor_path_expanded_graph(&traversal, branded_root, &mut visit_edge)
            } else if root.supports_efficient_snapshot_cursor_edge_paging() {
                visit_paged_cursor_path_expanded_graph(&traversal, branded_root, &mut visit_edge)
            } else {
                visit_uncounted_cursor_path_expanded_graph(
                    &traversal,
                    branded_root,
                    &mut visit_edge,
                )
            }
        });
    }
    visit_owned_path_expanded_graph(root, visit_edge)
}

#[cfg(test)]
mod pending_spill_fault {
    pub(super) fn arm() {
        super::allocation_fault::arm("protobuf pending-edge worklist");
    }
}

#[cfg(feature = "protobuf")]
fn encode_dat_terms(terms: &[String]) -> Result<Vec<u8>, SerializationError> {
    let encoded_len =
        DAT_TERMS_MAGIC.len() + terms.iter().map(|term| 4 + term.len()).sum::<usize>();
    let mut encoded = Vec::with_capacity(encoded_len);
    encoded.extend_from_slice(DAT_TERMS_MAGIC);
    for term in terms {
        let term_bytes = term.as_bytes();
        let len = u32::try_from(term_bytes.len())
            .map_err(|_| dictionary_error("DAT protobuf term exceeds u32 length"))?;
        encoded.extend_from_slice(&len.to_le_bytes());
        encoded.extend_from_slice(term_bytes);
    }
    Ok(encoded)
}

#[cfg(feature = "protobuf")]
fn decode_dat_terms(edge_data: &[u8], term_count: u64) -> Result<Vec<String>, SerializationError> {
    let term_capacity = usize::try_from(term_count)
        .map_err(|_| dictionary_error("DAT protobuf term_count does not fit usize"))?;
    if !edge_data.starts_with(DAT_TERMS_MAGIC) {
        return Err(dictionary_error(
            "DAT protobuf term payload is not the length-delimited binary format",
        ));
    }

    let mut offset = DAT_TERMS_MAGIC.len();
    // Every encoded term consumes at least its four-byte length field. Bound
    // the initial allocation by bytes actually present, not by an attacker-
    // controlled count that may be orders of magnitude larger than the input.
    let encoded_term_ceiling = edge_data.len().saturating_sub(DAT_TERMS_MAGIC.len()) / 4;
    let mut terms = Vec::with_capacity(term_capacity.min(encoded_term_ceiling));
    while offset < edge_data.len() {
        let Some(length_bytes) = edge_data.get(offset..offset + 4) else {
            return Err(dictionary_error("DAT protobuf term length is truncated"));
        };
        let len = u32::from_le_bytes([
            length_bytes[0],
            length_bytes[1],
            length_bytes[2],
            length_bytes[3],
        ]) as usize;
        offset += 4;

        let Some(term_bytes) = edge_data.get(offset..offset + len) else {
            return Err(dictionary_error("DAT protobuf term payload is truncated"));
        };
        offset += len;

        let term = String::from_utf8(term_bytes.to_vec())
            .map_err(|_| dictionary_error("DAT protobuf term is not valid UTF-8"))?;
        terms.push(term);
    }

    validate_term_count(term_count, terms.len(), "DAT protobuf")?;
    Ok(terms)
}

#[cfg(feature = "protobuf")]
/// Protobuf serializer for cross-language compatibility.
///
/// This serializer uses Protocol Buffers to serialize the dictionary
/// as a graph structure (nodes + edges), which is:
/// - More space-efficient than storing all terms as strings
/// - Compatible with all liblevenshtein implementations (Java, C++, Rust)
/// - Preserves the DAWG/trie structure directly without rebuilding
///
/// # Format
///
/// The dictionary is serialized as:
/// - List of node IDs
/// - List of final (terminal) node IDs
/// - List of edges (source_id, label, target_id)
/// - Root node ID
/// - Dictionary size (term count)
///
/// This format is defined in `proto/liblevenshtein.proto` and is shared
/// across all liblevenshtein implementations.
pub struct ProtobufSerializer;

#[cfg(feature = "protobuf")]
impl ProtobufSerializer {
    /// Extract graph structure from dictionary.
    ///
    /// Performs DFS traversal to collect all nodes and edges.
    ///
    /// NOTE: Since the Dictionary trait doesn't provide node identity,
    /// we serialize as a trie structure where each unique path creates
    /// new nodes. For true DAWG serialization with node sharing, we'd
    /// need dictionary implementations to expose node IDs.
    fn extract_graph<D>(dict: &D) -> Result<proto::Dictionary, SerializationError>
    where
        D: Dictionary,
        D::Node: DictionaryNode<Unit = u8>,
    {
        // Pre-allocate vectors with estimated capacity
        let est_size = dict.len().unwrap_or(100);
        let node_capacity = checked_capacity(est_size, 2, "protobuf v1 node table")?;
        let edge_capacity = checked_capacity(est_size, 3, "protobuf v1 edge table")?;
        let mut node_ids = Vec::new();
        let mut final_node_ids = Vec::new();
        let mut edges = Vec::new();
        try_reserve_exact(&mut node_ids, node_capacity, "protobuf v1 node table")?;
        try_reserve_exact(
            &mut final_node_ids,
            est_size,
            "protobuf v1 final-node table",
        )?;
        try_reserve_exact(&mut edges, edge_capacity, "protobuf v1 edge table")?;

        // Root node
        try_reserve_one(&mut node_ids, "protobuf v1 node table")?;
        node_ids.push(0);
        let root = dict.root();
        if root.is_final() {
            try_reserve_one(&mut final_node_ids, "protobuf v1 final-node table")?;
            final_node_ids.push(0);
        }
        visit_path_expanded_graph(&root, |source_id, label, target_id, is_final| {
            try_reserve_one(&mut node_ids, "protobuf v1 node table")?;
            if is_final {
                try_reserve_one(&mut final_node_ids, "protobuf v1 final-node table")?;
            }
            try_reserve_one(&mut edges, "protobuf v1 edge table")?;

            node_ids.push(target_id);
            if is_final {
                final_node_ids.push(target_id);
            }
            edges.push(proto::dictionary::Edge {
                source_id,
                label: u32::from(label),
                target_id,
            });
            Ok(())
        })?;

        let size = u64::try_from(dict.len().unwrap_or(0))
            .map_err(|_| dictionary_error("protobuf v1 term count does not fit u64"))?;

        Ok(proto::Dictionary {
            node_id: node_ids,
            final_node_id: final_node_ids,
            edge: edges,
            root_id: 0,
            size,
        })
    }
}

#[cfg(feature = "protobuf")]
impl DictionarySerializer for ProtobufSerializer {
    fn serialize<D, W>(dict: &D, mut writer: W) -> Result<(), SerializationError>
    where
        D: Dictionary,
        D::Node: DictionaryNode<Unit = u8>,
        W: Write,
    {
        use prost::Message;

        let proto_dict = Self::extract_graph(dict)?;
        let encoded_len = proto_dict.encoded_len();
        let mut buf = Vec::new();
        try_reserve_exact(&mut buf, encoded_len, "protobuf output buffer")?;
        proto_dict
            .encode(&mut buf)
            .map_err(|e| SerializationError::Io(std::io::Error::other(e)))?;
        writer.write_all(&buf)?;
        Ok(())
    }

    fn deserialize<D, R>(mut reader: R) -> Result<D, SerializationError>
    where
        D: DictionaryFromTerms,
        R: Read,
    {
        use prost::Message;

        // Read all bytes
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf)?;

        // Decode protobuf
        let proto_dict = proto::Dictionary::decode(&buf[..])?;

        // Reconstruct dictionary from graph
        // Build adjacency list with pre-allocated capacity
        let est_nodes = proto_dict.node_id.len();
        let mut adjacency: HashMap<u64, Vec<(u8, u64)>> = HashMap::with_capacity(est_nodes);
        let node_ids: HashSet<u64> = proto_dict.node_id.iter().copied().collect();
        if !node_ids.contains(&proto_dict.root_id) {
            return Err(dictionary_error(format!(
                "protobuf v1 root node {} is not declared",
                proto_dict.root_id
            )));
        }

        for edge in &proto_dict.edge {
            if !node_ids.contains(&edge.source_id) {
                return Err(dictionary_error(format!(
                    "protobuf v1 edge source {} is not declared",
                    edge.source_id
                )));
            }
            if !node_ids.contains(&edge.target_id) {
                return Err(dictionary_error(format!(
                    "protobuf v1 edge target {} is not declared",
                    edge.target_id
                )));
            }
            let label = checked_label_u32(edge.label, "protobuf v1")?;
            insert_deterministic_edge(
                &mut adjacency,
                edge.source_id,
                label,
                edge.target_id,
                "protobuf v1",
            )?;
        }

        // Pre-allocate HashSet with known size
        let mut final_set: HashSet<u64> = HashSet::with_capacity(proto_dict.final_node_id.len());
        final_set.extend(proto_dict.final_node_id.iter().copied());
        for final_id in &final_set {
            if !node_ids.contains(final_id) {
                return Err(dictionary_error(format!(
                    "protobuf v1 final node {final_id} is not declared"
                )));
            }
        }

        let terms = terms_from_adjacency(proto_dict.root_id, &adjacency, &final_set)?;
        validate_term_count(proto_dict.size, terms.len(), "protobuf v1")?;

        Ok(D::from_terms(terms))
    }
}

#[cfg(feature = "protobuf")]
/// Optimized protobuf serializer using DictionaryV2 format.
///
/// This serializer uses an optimized protobuf format that is 40-60% smaller
/// than the standard ProtobufSerializer by:
/// - Removing redundant node_id field (IDs are sequential)
/// - Using packed edge format (flat array instead of messages)
/// - Delta-encoding final node IDs for better compression
///
/// **Note**: This format is NOT compatible with older liblevenshtein
/// implementations. Use `ProtobufSerializer` for cross-language compatibility.
///
/// # Example
///
/// ```text
/// use liblevenshtein::prelude::*;
///
/// let dict = PathMapDictionary::from_terms(vec!["test", "testing"]);
///
/// // Serialize with optimized format (smaller size)
/// let mut buf = Vec::new();
/// OptimizedProtobufSerializer::serialize(&dict, &mut buf)?;
///
/// // Deserialize
/// let loaded: PathMapDictionary =
///     OptimizedProtobufSerializer::deserialize(&buf[..])?;
/// ```
pub struct OptimizedProtobufSerializer;

#[cfg(feature = "protobuf")]
impl OptimizedProtobufSerializer {
    /// Extract graph structure in optimized format.
    fn extract_graph_v2<D>(dict: &D) -> Result<proto::DictionaryV2, SerializationError>
    where
        D: Dictionary,
        D::Node: DictionaryNode<Unit = u8>,
    {
        // Pre-allocate vectors with estimated capacity
        let est_size = dict.len().unwrap_or(100);
        let edge_capacity = checked_capacity(est_size, 9, "protobuf v2 edge table")?;
        let mut final_node_ids = Vec::new();
        let mut edge_data = Vec::new();
        try_reserve_exact(
            &mut final_node_ids,
            est_size,
            "protobuf v2 final-node table",
        )?;
        try_reserve_exact(&mut edge_data, edge_capacity, "protobuf v2 edge table")?;

        // Root node
        let root = dict.root();
        if root.is_final() {
            try_reserve_one(&mut final_node_ids, "protobuf v2 final-node table")?;
            final_node_ids.push(0);
        }
        {
            let mut sink = V2GraphSink::new(&mut final_node_ids, &mut edge_data);
            visit_path_expanded_graph(&root, |source_id, label, target_id, is_final| {
                sink.emit(source_id, label, target_id, is_final)
            })?;
        }

        // Convert final node IDs to deltas
        let final_node_delta = if final_node_ids.is_empty() {
            Vec::new()
        } else {
            let mut deltas = Vec::new();
            try_reserve_exact(
                &mut deltas,
                final_node_ids.len(),
                "protobuf v2 final-node delta table",
            )?;
            deltas.push(final_node_ids[0]); // First value is absolute

            for i in 1..final_node_ids.len() {
                // Delta = current - previous
                let delta = final_node_ids[i]
                    .checked_sub(final_node_ids[i - 1])
                    .ok_or_else(|| dictionary_error("protobuf v2 final-node order regressed"))?;
                deltas.push(delta);
            }
            deltas
        };

        let edge_count = edge_data.len() / 3;
        let size = u64::try_from(dict.len().unwrap_or(0))
            .map_err(|_| dictionary_error("protobuf v2 term count does not fit u64"))?;
        let edge_count = u64::try_from(edge_count)
            .map_err(|_| dictionary_error("protobuf v2 edge count does not fit u64"))?;

        Ok(proto::DictionaryV2 {
            final_node_delta,
            edge_data,
            root_id: 0,
            size,
            edge_count,
        })
    }
}

#[cfg(feature = "protobuf")]
impl DictionarySerializer for OptimizedProtobufSerializer {
    fn serialize<D, W>(dict: &D, mut writer: W) -> Result<(), SerializationError>
    where
        D: Dictionary,
        D::Node: DictionaryNode<Unit = u8>,
        W: Write,
    {
        use prost::Message;

        let proto_dict = Self::extract_graph_v2(dict)?;
        let encoded_len = proto_dict.encoded_len();
        let mut buf = Vec::new();
        try_reserve_exact(&mut buf, encoded_len, "protobuf output buffer")?;
        proto_dict
            .encode(&mut buf)
            .map_err(|e| SerializationError::Io(std::io::Error::other(e)))?;
        writer.write_all(&buf)?;
        Ok(())
    }

    fn deserialize<D, R>(mut reader: R) -> Result<D, SerializationError>
    where
        D: DictionaryFromTerms,
        R: Read,
    {
        use prost::Message;

        // Read all bytes
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf)?;

        // Decode protobuf
        let proto_dict = proto::DictionaryV2::decode(&buf[..])?;

        // Validate edge_data length
        if proto_dict.edge_data.len() % 3 != 0 {
            return Err(SerializationError::DictionaryError(format!(
                "Invalid edge_data length: {} (must be multiple of 3)",
                proto_dict.edge_data.len()
            )));
        }
        let num_edges = proto_dict.edge_data.len() / 3;
        let declared_edges = usize::try_from(proto_dict.edge_count)
            .map_err(|_| dictionary_error("protobuf v2 edge_count does not fit usize"))?;
        if declared_edges != num_edges {
            return Err(dictionary_error(format!(
                "protobuf v2 edge_count mismatch: expected {declared_edges}, decoded {num_edges}"
            )));
        }

        // Reconstruct final node IDs from deltas with pre-allocation
        let mut final_node_ids = Vec::with_capacity(proto_dict.final_node_delta.len());
        if !proto_dict.final_node_delta.is_empty() {
            let mut cumsum = 0u64;
            for &delta in &proto_dict.final_node_delta {
                cumsum = cumsum
                    .checked_add(delta)
                    .ok_or_else(|| dictionary_error("protobuf v2 final-node delta overflow"))?;
                final_node_ids.push(cumsum);
            }
        }

        // Build adjacency list from packed edge data with pre-allocation
        let est_nodes = (num_edges as f64 * 0.6) as usize; // Estimate nodes from edges
        let mut adjacency: HashMap<u64, Vec<(u8, u64)>> = HashMap::with_capacity(est_nodes);
        let (edges, remainder) = proto_dict.edge_data.as_chunks::<3>();
        debug_assert!(remainder.is_empty(), "validated edge_data triplets");
        for chunk in edges {
            let source_id = chunk[0];
            let label = checked_label_u64(chunk[1], "protobuf v2")?;
            let target_id = chunk[2];

            insert_deterministic_edge(&mut adjacency, source_id, label, target_id, "protobuf v2")?;
        }

        // Pre-allocate HashSet with known size
        let mut final_set: HashSet<u64> = HashSet::with_capacity(final_node_ids.len());
        final_set.extend(final_node_ids.iter().copied());

        let terms = terms_from_adjacency(proto_dict.root_id, &adjacency, &final_set)?;
        validate_term_count(proto_dict.size, terms.len(), "protobuf v2")?;

        Ok(D::from_terms(terms))
    }
}

#[cfg(feature = "protobuf")]
/// Suffix automaton-optimized protobuf serializer.
///
/// This serializer is specifically optimized for `SuffixAutomaton` by storing
/// the original source texts rather than the graph structure. Since suffix
/// automata can be efficiently rebuilt from source texts in linear time,
/// this approach is both simpler and more space-efficient than serializing
/// the full automaton structure.
///
/// **Benefits**:
/// - Much smaller than serializing full graph (nodes, edges, suffix links)
/// - Simple and reliable reconstruction via online algorithm
/// - Preserves source text metadata
/// - Fast deserialization (O(n) construction)
///
/// **Note**: Only works with `SuffixAutomaton`, not other dictionary backends.
pub struct SuffixAutomatonProtobufSerializer;

#[cfg(feature = "protobuf")]
impl SuffixAutomatonProtobufSerializer {
    /// Serialize SuffixAutomaton to optimized protobuf format.
    ///
    /// Extracts source texts and rebuilds on deserialization.
    pub fn serialize_suffix_automaton<W>(
        dict: &crate::suffix_automaton::SuffixAutomaton,
        mut writer: W,
    ) -> Result<(), SerializationError>
    where
        W: Write,
    {
        use prost::Message;

        // Extract source texts from the automaton
        let source_texts = dict.source_texts();
        let string_count = dict.string_count();

        let proto_suffix = proto::SuffixAutomaton {
            source_texts,
            string_count: string_count as u64,
        };

        let mut buf = Vec::with_capacity(proto_suffix.encoded_len());
        proto_suffix
            .encode(&mut buf)
            .map_err(|e| SerializationError::Io(std::io::Error::other(e)))?;
        writer.write_all(&buf)?;
        Ok(())
    }

    /// Deserialize SuffixAutomaton from optimized protobuf format.
    pub fn deserialize_suffix_automaton<R>(
        mut reader: R,
    ) -> Result<crate::suffix_automaton::SuffixAutomaton, SerializationError>
    where
        R: Read,
    {
        use prost::Message;

        let mut buf = Vec::new();
        reader.read_to_end(&mut buf)?;

        let proto_suffix = proto::SuffixAutomaton::decode(&buf[..])?;

        // Validate string count
        if proto_suffix.source_texts.len() != proto_suffix.string_count as usize {
            return Err(SerializationError::DictionaryError(format!(
                "String count mismatch: expected {}, got {}",
                proto_suffix.string_count,
                proto_suffix.source_texts.len()
            )));
        }

        // Rebuild suffix automaton from source texts
        Ok(crate::suffix_automaton::SuffixAutomaton::from_texts(
            proto_suffix.source_texts,
        ))
    }
}

#[cfg(feature = "protobuf")]
/// DAT-optimized protobuf serializer.
///
/// This serializer is specifically optimized for `DoubleArrayTrie` and directly
/// serializes the internal BASE/CHECK/IS_FINAL arrays without graph traversal.
///
/// **Benefits**:
/// - Direct array serialization (no graph traversal)
/// - Fastest serialization/deserialization for DAT
/// - Smallest binary format for DAT structures
/// - Preserves all DAT optimizations
///
/// **Note**: Only works with `DoubleArrayTrie`, not other dictionary backends.
pub struct DatProtobufSerializer;

#[cfg(feature = "protobuf")]
impl DatProtobufSerializer {
    /// Serialize DoubleArrayTrie to optimized protobuf format.
    ///
    /// Directly extracts terms and rebuilds on deserialization.
    /// This is simpler and more reliable than trying to serialize internal state.
    pub fn serialize_dat<W>(
        dict: &crate::double_array_trie::DoubleArrayTrie,
        mut writer: W,
    ) -> Result<(), SerializationError>
    where
        W: Write,
    {
        use prost::Message;

        // Extract all terms from the dictionary
        let terms = super::extract_terms(dict);

        // Create a marker protobuf message indicating this is a DAT serialization
        // We'll use the term count as a simple serialization
        let proto_dat = proto::DoubleArrayTrie {
            base: Vec::new(), // Placeholder - we serialize via terms
            check: Vec::new(),
            is_final: Vec::new(),
            edge_data: encode_dat_terms(&terms)?,
            free_list: Vec::new(),
            term_count: terms.len() as u64,
            rebuild_threshold: 0.2,
        };

        let mut buf = Vec::with_capacity(proto_dat.encoded_len());
        proto_dat
            .encode(&mut buf)
            .map_err(|e| SerializationError::Io(std::io::Error::other(e)))?;
        writer.write_all(&buf)?;
        Ok(())
    }

    /// Deserialize DoubleArrayTrie from optimized protobuf format.
    pub fn deserialize_dat<R>(
        mut reader: R,
    ) -> Result<crate::double_array_trie::DoubleArrayTrie, SerializationError>
    where
        R: Read,
    {
        use prost::Message;

        let mut buf = Vec::new();
        reader.read_to_end(&mut buf)?;

        let proto_dat = proto::DoubleArrayTrie::decode(&buf[..])?;

        let terms = decode_dat_terms(&proto_dat.edge_data, proto_dat.term_count)?;

        // Rebuild DAT from terms
        Ok(crate::double_array_trie::DoubleArrayTrie::from_terms(terms))
    }
}

#[cfg(all(test, feature = "protobuf"))]
mod binary_dat_payload_tests {
    use super::*;
    use std::sync::Arc;

    #[derive(Clone, Copy)]
    struct EmptyNode;

    impl DictionaryNode for EmptyNode {
        type Unit = u8;
        type SnapshotCursor = usize;
        type SnapshotGraphValueHandle = usize;

        fn is_final(&self) -> bool {
            false
        }

        fn transition(&self, _label: u8) -> Option<Self> {
            None
        }

        fn edges(&self) -> Box<dyn Iterator<Item = (u8, Self)> + '_> {
            Box::new(std::iter::empty())
        }
    }

    #[derive(Clone)]
    struct CountHintNode {
        labels: Arc<[u8]>,
        declared_edge_count: Option<usize>,
        is_root: bool,
    }

    impl CountHintNode {
        fn root(labels: Vec<u8>, declared_edge_count: Option<usize>) -> Self {
            Self {
                labels: Arc::from(labels),
                declared_edge_count,
                is_root: true,
            }
        }

        fn leaf(&self) -> Self {
            Self {
                labels: Arc::clone(&self.labels),
                declared_edge_count: Some(0),
                is_root: false,
            }
        }
    }

    impl DictionaryNode for CountHintNode {
        type Unit = u8;
        type SnapshotCursor = usize;
        type SnapshotGraphValueHandle = usize;

        fn is_final(&self) -> bool {
            !self.is_root
        }

        fn transition(&self, label: u8) -> Option<Self> {
            (self.is_root && self.labels.contains(&label)).then(|| self.leaf())
        }

        fn edges(&self) -> Box<dyn Iterator<Item = (u8, Self)> + '_> {
            if !self.is_root {
                return Box::new(std::iter::empty());
            }
            let labels = Arc::clone(&self.labels);
            let child = self.leaf();
            Box::new((0..labels.len()).map(move |index| (labels[index], child.clone())))
        }

        fn for_each_edge<F>(&self, mut visitor: F)
        where
            Self: Sized,
            F: FnMut(u8, Self),
        {
            if self.is_root {
                let child = self.leaf();
                for &label in self.labels.iter() {
                    visitor(label, child.clone());
                }
            }
        }

        fn edge_count(&self) -> Option<usize> {
            if self.is_root {
                self.declared_edge_count
            } else {
                Some(0)
            }
        }
    }

    struct CountHintDictionary {
        root: CountHintNode,
    }

    impl CountHintDictionary {
        fn new(labels: Vec<u8>, declared_edge_count: Option<usize>) -> Self {
            Self {
                root: CountHintNode::root(labels, declared_edge_count),
            }
        }
    }

    impl Dictionary for CountHintDictionary {
        type Node = CountHintNode;

        fn root(&self) -> Self::Node {
            self.root.clone()
        }

        fn len(&self) -> Option<usize> {
            Some(self.root.labels.len())
        }
    }

    struct ReportedLengthDictionary(usize);

    impl Dictionary for ReportedLengthDictionary {
        type Node = EmptyNode;

        fn root(&self) -> Self::Node {
            EmptyNode
        }

        fn len(&self) -> Option<usize> {
            Some(self.0)
        }
    }

    struct MisreportedLengthDictionary(crate::dynamic_dawg::DynamicDawg<()>);

    impl MisreportedLengthDictionary {
        fn one_term() -> Self {
            Self(crate::dynamic_dawg::DynamicDawg::from_terms(vec![
                "a".to_string()
            ]))
        }
    }

    impl Dictionary for MisreportedLengthDictionary {
        type Node = <crate::dynamic_dawg::DynamicDawg<()> as Dictionary>::Node;

        fn root(&self) -> Self::Node {
            self.0.root()
        }

        fn len(&self) -> Option<usize> {
            Some(0)
        }
    }

    #[derive(Clone)]
    struct OwnedOnlyDynamicDawgNode(crate::dynamic_dawg::DynamicDawgNode<()>);

    impl DictionaryNode for OwnedOnlyDynamicDawgNode {
        type Unit = u8;
        type SnapshotCursor =
            <crate::dynamic_dawg::DynamicDawgNode<()> as DictionaryNode>::SnapshotCursor;
        type SnapshotGraphValueHandle =
            <crate::dynamic_dawg::DynamicDawgNode<()> as DictionaryNode>::SnapshotGraphValueHandle;

        fn is_final(&self) -> bool {
            self.0.is_final()
        }

        fn transition(&self, label: u8) -> Option<Self> {
            self.0.transition(label).map(Self)
        }

        fn edges(&self) -> Box<dyn Iterator<Item = (u8, Self)> + '_> {
            Box::new(self.0.edges().map(|(label, child)| (label, Self(child))))
        }

        fn for_each_edge<F>(&self, mut visitor: F)
        where
            F: FnMut(u8, Self),
        {
            self.0
                .for_each_edge(|label, child| visitor(label, Self(child)));
        }

        fn edge_count(&self) -> Option<usize> {
            self.0.edge_count()
        }
    }

    struct OwnedOnlyDynamicDawg(crate::dynamic_dawg::DynamicDawg<()>);

    impl Dictionary for OwnedOnlyDynamicDawg {
        type Node = OwnedOnlyDynamicDawgNode;

        fn root(&self) -> Self::Node {
            OwnedOnlyDynamicDawgNode(self.0.root())
        }

        fn len(&self) -> Option<usize> {
            self.0.len()
        }
    }

    #[test]
    fn dat_payload_round_trips_only_the_length_delimited_binary_form() {
        let terms = vec!["alpha".to_string(), "café".to_string()];
        let encoded = encode_dat_terms(&terms).unwrap();
        assert_eq!(decode_dat_terms(&encoded, 2).unwrap(), terms);

        let text_payload = b"alpha\ncaf\xc3\xa9\n";
        assert!(matches!(
            decode_dat_terms(text_payload, 2),
            Err(SerializationError::DictionaryError(message))
                if message.contains("length-delimited binary format")
        ));

        // The declared count is hostile, but the four-byte payload proves no
        // term body exists. Decoding must fail without reserving usize::MAX.
        assert!(decode_dat_terms(DAT_TERMS_MAGIC, u64::MAX).is_err());
    }

    #[test]
    fn graph_validation_and_enumeration_are_iterative_on_deep_inputs() {
        const DEPTH: u64 = 50_000;
        let mut adjacency = HashMap::with_capacity(DEPTH as usize);
        for node in 0..DEPTH {
            adjacency.insert(node, vec![(b'a', node + 1)]);
        }
        let finals = HashSet::from([DEPTH]);
        let terms = terms_from_adjacency(0, &adjacency, &finals).unwrap();
        assert_eq!(terms.len(), 1);
        assert_eq!(terms[0].len(), DEPTH as usize);
    }

    #[test]
    fn duplicate_outgoing_labels_are_rejected() {
        let mut adjacency = HashMap::new();
        insert_deterministic_edge(&mut adjacency, 0, b'a', 1, "test").unwrap();
        assert!(insert_deterministic_edge(&mut adjacency, 0, b'a', 2, "test").is_err());
    }

    #[test]
    fn both_protobuf_encoders_round_trip_a_hundred_thousand_level_trie() {
        use crate::dynamic_dawg::DynamicDawg;

        const DEPTH: usize = 100_000;
        let term = "x".repeat(DEPTH);
        let dictionary: DynamicDawg<()> = DynamicDawg::from_terms(vec![term.clone()]);

        let mut v1 = Vec::new();
        ProtobufSerializer::serialize(&dictionary, &mut v1).expect("serialize protobuf v1");
        let decoded_v1: DynamicDawg<()> =
            ProtobufSerializer::deserialize(v1.as_slice()).expect("deserialize protobuf v1");
        assert!(decoded_v1.contains(&term));

        let mut v2 = Vec::new();
        OptimizedProtobufSerializer::serialize(&dictionary, &mut v2)
            .expect("serialize protobuf v2");
        let decoded_v2: DynamicDawg<()> = OptimizedProtobufSerializer::deserialize(v2.as_slice())
            .expect("deserialize protobuf v2");
        assert!(decoded_v2.contains(&term));
    }

    #[test]
    fn protobuf_v1_capacity_overflow_is_typed_and_does_not_touch_writer() {
        let dictionary = ReportedLengthDictionary(usize::MAX);
        let mut writer = Vec::new();
        let result = ProtobufSerializer::serialize(&dictionary, &mut writer);
        assert!(result.is_err(), "capacity overflow must be a typed error");
        assert!(
            writer.is_empty(),
            "failed extraction must not publish bytes"
        );
    }

    #[test]
    fn protobuf_v2_capacity_overflow_is_typed_and_does_not_touch_writer() {
        let dictionary = ReportedLengthDictionary(usize::MAX);
        let mut writer = Vec::new();
        let result = OptimizedProtobufSerializer::serialize(&dictionary, &mut writer);
        assert!(result.is_err(), "capacity overflow must be a typed error");
        assert!(
            writer.is_empty(),
            "failed extraction must not publish bytes"
        );
    }

    #[test]
    fn first_pending_edge_spill_failure_is_typed_and_does_not_touch_writer() {
        use crate::dynamic_dawg::DynamicDawg;

        let dictionary: DynamicDawg<()> = DynamicDawg::from_terms(branching_spine_terms(17, b'q'));
        let mut writer = Vec::new();
        pending_spill_fault::arm();
        let result = ProtobufSerializer::serialize(&dictionary, &mut writer);
        assert!(result.is_err(), "pending-edge spill failure must be typed");
        assert!(
            writer.is_empty(),
            "failed extraction must not publish bytes"
        );
    }

    fn schedule_labels(
        labels: Vec<u8>,
        declared_edge_count: Option<usize>,
    ) -> Result<Vec<u8>, SerializationError> {
        let edge_count = labels.len();
        let root = CountHintNode::root(labels, declared_edge_count);
        let mut pending: smallvec::SmallVec<[PendingEdge<CountHintNode>; 16]> =
            smallvec::SmallVec::new();
        let direct = append_pending_children(&root, 0, &mut pending)?;
        let mut observed = Vec::with_capacity(edge_count);
        if let Some(edge) = direct {
            observed.push(edge.label);
        }
        while let Some(edge) = pending.pop() {
            observed.push(edge.label);
        }
        Ok(observed)
    }

    fn flat_schedule(
        edge_count: usize,
        declared_edge_count: Option<usize>,
    ) -> Result<Vec<u8>, SerializationError> {
        let labels: Vec<u8> = (0..edge_count).map(|label| label as u8).collect();
        schedule_labels(labels, declared_edge_count)
    }

    #[test]
    fn exact_edge_counts_use_one_bounded_batch_and_preserve_order() {
        for edge_count in [0usize, 1, 2, 16, 17, 255] {
            pending_push_observation::reset();
            let observed = flat_schedule(edge_count, Some(edge_count)).unwrap();
            let expected: Vec<u8> = (0..edge_count).map(|label| label as u8).collect();
            assert_eq!(observed, expected, "edge count {edge_count}");
            assert_eq!(
                pending_push_observation::pushes(),
                edge_count.saturating_sub(1),
                "edge count {edge_count}"
            );
            assert_eq!(
                pending_push_observation::fallback_reserve_checks(),
                0,
                "exact counted paths must not use the per-sibling fallback"
            );
            assert_eq!(
                pending_push_observation::counted_batch_reservations(),
                usize::from(edge_count >= 2),
                "multi-child counted paths reserve exactly once"
            );
        }
    }

    #[test]
    fn unknown_edge_count_retains_the_fallible_per_sibling_fallback() {
        const EDGE_COUNT: usize = 18;
        pending_push_observation::reset();
        let observed = flat_schedule(EDGE_COUNT, None).unwrap();
        let expected: Vec<u8> = (0..EDGE_COUNT).map(|label| label as u8).collect();
        assert_eq!(observed, expected);
        assert_eq!(pending_push_observation::pushes(), EDGE_COUNT - 1);
        assert_eq!(
            pending_push_observation::fallback_reserve_checks(),
            EDGE_COUNT - 1
        );
        assert_eq!(pending_push_observation::counted_batch_reservations(), 0);
    }

    fn assert_edge_count_mismatch(result: Result<Vec<u8>, SerializationError>) {
        assert!(matches!(
            result,
            Err(SerializationError::DictionaryError(message))
                if message.contains("edge_count mismatch")
        ));
    }

    #[test]
    fn underreported_and_overreported_edge_counts_fail_closed() {
        assert_edge_count_mismatch(flat_schedule(2, Some(1)));
        assert_edge_count_mismatch(flat_schedule(2, Some(3)));
    }

    fn assert_count_mismatch_preserves_writer(dictionary: &CountHintDictionary) {
        let mut v1 = b"sentinel".to_vec();
        let v1_result = ProtobufSerializer::serialize(dictionary, &mut v1);
        assert!(matches!(
            v1_result,
            Err(SerializationError::DictionaryError(ref message))
                if message.contains("edge_count mismatch")
        ));
        assert_eq!(v1, b"sentinel");

        let mut v2 = b"sentinel".to_vec();
        let v2_result = OptimizedProtobufSerializer::serialize(dictionary, &mut v2);
        assert!(matches!(
            v2_result,
            Err(SerializationError::DictionaryError(ref message))
                if message.contains("edge_count mismatch")
        ));
        assert_eq!(v2, b"sentinel");
    }

    #[test]
    fn count_mismatch_preserves_both_external_writers() {
        assert_count_mismatch_preserves_writer(&CountHintDictionary::new(
            vec![b'a', b'b'],
            Some(1),
        ));
        assert_count_mismatch_preserves_writer(&CountHintDictionary::new(
            vec![b'a', b'b'],
            Some(3),
        ));
    }

    #[test]
    fn exact_and_unknown_count_paths_emit_identical_protobufs() {
        let exact = CountHintDictionary::new(vec![b'a', b'b', b'c'], Some(3));
        let unknown = CountHintDictionary::new(vec![b'a', b'b', b'c'], None);

        let mut exact_v1 = Vec::new();
        let mut unknown_v1 = Vec::new();
        ProtobufSerializer::serialize(&exact, &mut exact_v1).unwrap();
        ProtobufSerializer::serialize(&unknown, &mut unknown_v1).unwrap();
        assert_eq!(exact_v1, unknown_v1);

        let mut exact_v2 = Vec::new();
        let mut unknown_v2 = Vec::new();
        OptimizedProtobufSerializer::serialize(&exact, &mut exact_v2).unwrap();
        OptimizedProtobufSerializer::serialize(&unknown, &mut unknown_v2).unwrap();
        assert_eq!(exact_v2, unknown_v2);
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(256))]

        #[test]
        fn counted_and_unknown_schedules_match_the_encounter_order_oracle(
            labels in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..257),
        ) {
            let counted = schedule_labels(labels.clone(), Some(labels.len())).unwrap();
            let unknown = schedule_labels(labels.clone(), None).unwrap();
            proptest::prop_assert_eq!(&counted, &labels);
            proptest::prop_assert_eq!(&unknown, &labels);
            proptest::prop_assert_eq!(counted, unknown);
        }
    }

    fn assert_injected_allocation_failure(
        result: Result<(), SerializationError>,
        writer: &[u8],
        expected_context: &'static str,
    ) {
        match result {
            Err(SerializationError::Allocation { context, source: _ }) => {
                assert_eq!(context, expected_context)
            }
            other => panic!("expected typed allocation failure, got {other:?}"),
        }
        assert_eq!(
            writer, b"sentinel",
            "allocation failure must not publish even a partial protobuf"
        );
    }

    #[test]
    fn v1_each_growth_failure_is_typed_and_preserves_the_writer() {
        for context in [
            "protobuf v1 node table",
            "protobuf v1 final-node table",
            "protobuf v1 edge table",
            "protobuf output buffer",
        ] {
            let dictionary = MisreportedLengthDictionary::one_term();
            let mut writer = b"sentinel".to_vec();
            allocation_fault::arm(context);
            let result = ProtobufSerializer::serialize(&dictionary, &mut writer);
            assert_injected_allocation_failure(result, &writer, context);
        }
    }

    #[test]
    fn v2_each_growth_failure_is_typed_and_preserves_the_writer() {
        for context in [
            "protobuf v2 final-node table",
            "protobuf v2 edge table",
            "protobuf v2 final-node delta table",
            "protobuf output buffer",
        ] {
            let dictionary = MisreportedLengthDictionary::one_term();
            let mut writer = b"sentinel".to_vec();
            allocation_fault::arm(context);
            let result = OptimizedProtobufSerializer::serialize(&dictionary, &mut writer);
            assert_injected_allocation_failure(result, &writer, context);
        }
    }

    #[test]
    fn v2_sink_emits_exact_events_across_spare_capacity_boundaries() {
        for (final_capacity, edge_capacity, is_final) in [
            (0, 0, false),
            (0, 0, true),
            (1, 2, true),
            (1, 3, true),
            (1, 4, false),
        ] {
            let mut final_node_ids = Vec::with_capacity(final_capacity);
            let mut edge_data = Vec::with_capacity(edge_capacity);
            {
                let mut sink = V2GraphSink::new(&mut final_node_ids, &mut edge_data);
                sink.emit(7, b'x', 11, is_final).unwrap();
            }

            assert_eq!(
                final_node_ids,
                if is_final { vec![11] } else { Vec::new() },
                "final-node output differed for requested capacities ({final_capacity}, {edge_capacity})"
            );
            assert_eq!(edge_data, vec![7, u64::from(b'x'), 11]);
        }
    }

    #[test]
    fn v2_sink_second_growth_failure_preserves_both_logical_lengths() {
        v2_sink_observation::reset();
        let mut final_node_ids = Vec::new();
        let mut edge_data = Vec::new();
        allocation_fault::arm("protobuf v2 edge table");

        let result = {
            let mut sink = V2GraphSink::new(&mut final_node_ids, &mut edge_data);
            sink.emit(7, b'x', 11, true)
        };

        assert!(matches!(
            result,
            Err(SerializationError::Allocation {
                context: "protobuf v2 edge table",
                source: _
            })
        ));
        assert!(final_node_ids.is_empty());
        assert!(edge_data.is_empty());
        assert_eq!(v2_sink_observation::final_growth_attempts(), 1);
        assert_eq!(v2_sink_observation::edge_growth_attempts(), 1);
        assert_eq!(v2_sink_observation::commits(), 0);
    }

    #[test]
    fn v2_sink_consults_allocator_only_at_true_exhaustion() {
        v2_sink_observation::reset();
        let mut final_node_ids = Vec::with_capacity(1);
        let mut edge_data = Vec::with_capacity(3);
        allocation_fault::arm("protobuf v2 edge table");

        let exhausted = {
            let mut sink = V2GraphSink::new(&mut final_node_ids, &mut edge_data);
            sink.emit(1, b'a', 2, false).unwrap();
            sink.emit(2, b'b', 3, false)
        };

        assert!(matches!(
            exhausted,
            Err(SerializationError::Allocation {
                context: "protobuf v2 edge table",
                source: _
            })
        ));
        assert!(final_node_ids.is_empty());
        assert_eq!(edge_data, vec![1, u64::from(b'a'), 2]);
        assert_eq!(v2_sink_observation::final_growth_attempts(), 0);
        assert_eq!(v2_sink_observation::edge_growth_attempts(), 1);
        assert_eq!(v2_sink_observation::commits(), 1);
    }

    #[test]
    fn v2_sink_records_exactly_one_atomic_commit_per_successful_event() {
        v2_sink_observation::reset();
        let mut final_node_ids = Vec::with_capacity(2);
        let mut edge_data = Vec::with_capacity(6);
        {
            let mut sink = V2GraphSink::new(&mut final_node_ids, &mut edge_data);
            sink.emit(0, b'a', 1, true).unwrap();
            sink.emit(1, b'b', 2, false).unwrap();
        }

        assert_eq!(final_node_ids, vec![1]);
        assert_eq!(
            edge_data,
            vec![0, u64::from(b'a'), 1, 1, u64::from(b'b'), 2]
        );
        assert_eq!(v2_sink_observation::final_growth_attempts(), 0);
        assert_eq!(v2_sink_observation::edge_growth_attempts(), 0);
        assert_eq!(v2_sink_observation::commits(), 2);
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(256))]

        #[test]
        fn v2_sink_matches_the_event_sequence_oracle(
            events in proptest::collection::vec(
                (
                    proptest::prelude::any::<u64>(),
                    proptest::prelude::any::<u8>(),
                    proptest::prelude::any::<u64>(),
                    proptest::prelude::any::<bool>(),
                ),
                0..257,
            ),
        ) {
            let mut expected_finals = Vec::new();
            let mut expected_edges = Vec::new();
            for &(source_id, label, target_id, is_final) in &events {
                if is_final {
                    expected_finals.push(target_id);
                }
                expected_edges.extend_from_slice(&[
                    source_id,
                    u64::from(label),
                    target_id,
                ]);
            }

            let mut final_node_ids = Vec::new();
            let mut edge_data = Vec::new();
            {
                let mut sink = V2GraphSink::new(&mut final_node_ids, &mut edge_data);
                for (source_id, label, target_id, is_final) in events {
                    sink.emit(source_id, label, target_id, is_final).unwrap();
                }
            }

            proptest::prop_assert_eq!(final_node_ids, expected_finals);
            proptest::prop_assert_eq!(edge_data, expected_edges);
        }
    }

    #[test]
    fn unary_paths_use_no_pending_sibling_frames() {
        use crate::dynamic_dawg::DynamicDawg;

        const DEPTH: usize = 1_000;
        let dictionary: DynamicDawg<()> = DynamicDawg::from_terms(vec!["x".repeat(DEPTH)]);

        pending_push_observation::reset();
        cursor_path_observation::reset();
        let mut v1 = Vec::new();
        ProtobufSerializer::serialize(&dictionary, &mut v1).unwrap();
        assert_eq!(pending_push_observation::pushes(), 0);
        assert_eq!(
            cursor_path_observation::cursor_edge_observations(),
            DEPTH + 1
        );

        pending_push_observation::reset();
        cursor_path_observation::reset();
        let mut v2 = Vec::new();
        OptimizedProtobufSerializer::serialize(&dictionary, &mut v2).unwrap();
        assert_eq!(pending_push_observation::pushes(), 0);
        assert_eq!(
            cursor_path_observation::cursor_edge_observations(),
            DEPTH + 1
        );
    }

    fn branching_spine_terms(depth: usize, last_sibling: u8) -> Vec<String> {
        assert!(last_sibling >= b'b');
        let siblings_per_level = usize::from(last_sibling - b'b') + 1;
        let mut terms = Vec::with_capacity(
            depth
                .checked_mul(siblings_per_level)
                .and_then(|count| count.checked_add(1))
                .expect("test fixture capacity"),
        );
        let mut prefix = String::with_capacity(depth + 1);
        for _ in 0..depth {
            for label in b'b'..=last_sibling {
                let mut sibling = prefix.clone();
                sibling.push(char::from(label));
                terms.push(sibling);
            }
            prefix.push('a');
        }
        terms.push(prefix);
        terms
    }

    fn observe_paged_cursor_frames(depth: usize, last_sibling: u8) -> (usize, usize, usize) {
        use crate::dynamic_dawg::DynamicDawg;

        let dictionary: DynamicDawg<()> =
            DynamicDawg::from_terms(branching_spine_terms(depth, last_sibling));
        pending_push_observation::reset();
        let mut bytes = Vec::new();
        ProtobufSerializer::serialize(&dictionary, &mut bytes).expect("serialize fixture");
        (
            pending_push_observation::pushes(),
            pending_push_observation::peak_frames(),
            pending_push_observation::current_frames(),
        )
    }

    #[test]
    fn paged_cursor_worklist_is_bounded_by_branching_depth_not_fanout() {
        const DEPTH: usize = 64;

        let binary = observe_paged_cursor_frames(DEPTH, b'b');
        let seventeen_way = observe_paged_cursor_frames(DEPTH, b'q');

        assert_eq!(binary, (DEPTH, DEPTH, 0));
        assert_eq!(seventeen_way, (DEPTH, DEPTH, 0));
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn parent_cursor_continuation_is_three_machine_words() {
        type Cursor = crate::dynamic_dawg::DynamicDawgSnapshotCursor<u8, ()>;
        type Frame = ParentCursorContinuation<'static, Cursor>;

        assert_eq!(std::mem::size_of::<Frame>(), 3 * std::mem::size_of::<u64>());
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn parent_range_continuation_is_three_machine_words() {
        type Node = crate::dynamic_dawg::DynamicDawgNode<()>;
        type Frame = ParentRangeContinuation<'static, Node>;

        assert_eq!(std::mem::size_of::<Frame>(), 3 * std::mem::size_of::<u64>());
        assert_eq!(std::mem::align_of::<Frame>(), std::mem::align_of::<u64>());
    }

    #[test]
    fn dynamic_dawg_advertises_exact_native_snapshot_cursor_paging() {
        use crate::dynamic_dawg::DynamicDawg;

        let dictionary: DynamicDawg<()> =
            DynamicDawg::from_terms(vec!["a".to_string(), "b".to_string(), "c".to_string()]);
        let root = dictionary.root();
        assert!(
            root.supports_efficient_snapshot_cursor_edge_paging(),
            "DynamicDawg has immutable indexed edge storage and must expose native paging"
        );
        assert!(
            root.supports_efficient_snapshot_cursor_edge_ranges(),
            "DynamicDawg must expose its immutable edge suffix directly"
        );

        let cursor = root.snapshot_root_cursor().expect("DynamicDawg cursor");
        let mut first = Vec::new();
        // SAFETY: `cursor` came from `root`, which remains retained throughout
        // this call and while the copied child cursor is observed.
        let first_metadata = unsafe {
            root.visit_snapshot_cursor_edge_page(cursor, 0, 1, |label, child| {
                first.push((label, child));
            })
        }
        .expect("native first page");
        assert_eq!(first_metadata, (false, 3));
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].0, b'a');

        let mut siblings = Vec::new();
        // SAFETY: same retained owner and cursor provenance as above.
        let sibling_metadata = unsafe {
            root.visit_snapshot_cursor_edge_page(cursor, 1, 2, |label, child| {
                siblings.push((label, child));
            })
        }
        .expect("native sibling page");
        assert_eq!(sibling_metadata, first_metadata);
        assert_eq!(
            siblings.iter().map(|(label, _)| *label).collect::<Vec<_>>(),
            vec![b'b', b'c']
        );
    }

    #[test]
    fn dynamic_dawg_protobuf_serialization_uses_only_retained_cursors() {
        use crate::dynamic_dawg::DynamicDawg;

        let dictionary: DynamicDawg<()> = DynamicDawg::from_terms(vec![
            "alpha".to_string(),
            "alpine".to_string(),
            "beta".to_string(),
        ]);

        cursor_path_observation::reset();
        let mut writer = Vec::new();
        ProtobufSerializer::serialize(&dictionary, &mut writer).unwrap();

        assert!(cursor_path_observation::cursor_edge_observations() > 0);
        assert!(cursor_path_observation::range_starts() > 0);
        assert!(cursor_path_observation::range_steps() > 0);
        assert_eq!(cursor_path_observation::indexed_edge_observations(), 0);
        assert_eq!(cursor_path_observation::full_cursor_visits(), 0);
        assert_eq!(
            cursor_path_observation::owned_node_visits(),
            0,
            "a retained-cursor traversal must not clone descendant node handles"
        );
    }

    #[test]
    fn retained_cursor_and_owned_fallback_emit_identical_v1_and_v2_bytes() {
        use crate::dynamic_dawg::DynamicDawg;

        let terms = vec![
            "".to_string(),
            "alpha".to_string(),
            "alpine".to_string(),
            "beta".to_string(),
            "betamax".to_string(),
            "z".repeat(257),
        ];
        let cursor_dictionary: DynamicDawg<()> = DynamicDawg::from_terms(terms.clone());
        let owned_dictionary = OwnedOnlyDynamicDawg(DynamicDawg::from_terms(terms));

        let mut cursor_v1 = Vec::new();
        let mut owned_v1 = Vec::new();
        ProtobufSerializer::serialize(&cursor_dictionary, &mut cursor_v1).unwrap();
        ProtobufSerializer::serialize(&owned_dictionary, &mut owned_v1).unwrap();
        assert_eq!(cursor_v1, owned_v1);

        let mut cursor_v2 = Vec::new();
        let mut owned_v2 = Vec::new();
        OptimizedProtobufSerializer::serialize(&cursor_dictionary, &mut cursor_v2).unwrap();
        OptimizedProtobufSerializer::serialize(&owned_dictionary, &mut owned_v2).unwrap();
        assert_eq!(cursor_v2, owned_v2);
    }

    #[test]
    fn cursor_range_start_contract_failure_is_typed_and_preserves_writer() {
        use crate::dynamic_dawg::DynamicDawg;

        let dictionary: DynamicDawg<()> =
            DynamicDawg::from_terms(vec!["alpha".to_string(), "beta".to_string()]);
        let mut writer = b"sentinel".to_vec();
        let _fault = cursor_path_fault::arm();

        let result = ProtobufSerializer::serialize(&dictionary, &mut writer);
        assert!(matches!(
            result,
            Err(SerializationError::DictionaryError(ref message))
                if message.contains("retained edge-range")
        ));
        assert_eq!(writer, b"sentinel");
    }

    #[test]
    fn impossible_deterministic_byte_fanout_is_typed_and_preserves_both_writers() {
        use crate::dynamic_dawg::DynamicDawg;

        let dictionary: DynamicDawg<()> =
            DynamicDawg::from_terms(vec!["a".to_string(), "b".to_string()]);
        for optimized in [false, true] {
            let mut writer = b"sentinel".to_vec();
            let _fault = cursor_path_fault::arm_on_visit(cursor_path_fault::Action::ChangeTotal, 1);
            let result = if optimized {
                OptimizedProtobufSerializer::serialize(&dictionary, &mut writer)
            } else {
                ProtobufSerializer::serialize(&dictionary, &mut writer)
            };
            assert!(matches!(
                result,
                Err(SerializationError::DictionaryError(ref message))
                    if message.contains("deterministic byte fanout")
            ));
            assert_eq!(writer, b"sentinel");
        }
    }

    fn assert_resumed_range_fault_fails_closed(action: cursor_path_fault::Action) {
        use crate::dynamic_dawg::DynamicDawg;

        let dictionary: DynamicDawg<()> =
            DynamicDawg::from_terms(vec!["a".to_string(), "b".to_string()]);
        for optimized in [false, true] {
            let mut writer = b"sentinel".to_vec();
            // Root range start is visit 1, the first leaf start is visit 2,
            // and stepping the root's sibling range is visit 3.
            let _fault = cursor_path_fault::arm_on_visit(action, 3);
            let result = if optimized {
                OptimizedProtobufSerializer::serialize(&dictionary, &mut writer)
            } else {
                ProtobufSerializer::serialize(&dictionary, &mut writer)
            };
            assert!(matches!(
                result,
                Err(SerializationError::DictionaryError(ref message))
                    if message.contains("resumed sibling step")
            ));
            assert_eq!(
                writer, b"sentinel",
                "a rejected range step must not publish partial bytes"
            );
        }
    }

    #[test]
    fn unavailable_resumed_range_is_typed_and_preserves_both_writers() {
        assert_resumed_range_fault_fails_closed(cursor_path_fault::Action::Unavailable);
    }

    #[test]
    fn invalid_resumed_range_finality_signal_preserves_both_writers() {
        assert_resumed_range_fault_fails_closed(cursor_path_fault::Action::ChangeFinality);
    }

    #[test]
    fn invalid_resumed_range_total_signal_preserves_both_writers() {
        assert_resumed_range_fault_fails_closed(cursor_path_fault::Action::ChangeTotal);
    }

    #[test]
    fn missing_resumed_range_edge_is_typed_and_preserves_both_writers() {
        assert_resumed_range_fault_fails_closed(cursor_path_fault::Action::SuppressCallbacks);
    }

    #[test]
    fn invalid_duplicate_range_signal_is_typed_and_preserves_both_writers() {
        assert_resumed_range_fault_fails_closed(cursor_path_fault::Action::DuplicateCallbacks);
    }
}
