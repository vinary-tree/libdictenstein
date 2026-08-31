//! `OverlayEvictable<K, V, S>` — the SHARED GENERIC overlay-eviction + read-fault
//! primitives, lifted K-generic over [`OverlayNode<K, V>`] from char's PROVEN
//! implementation and evolved into the generation-bound compact batch design.
//!
//! # Why a trait (trait-first, char is the first/proven impl)
//!
//! The foundation [`OverlayNode<K, V>`] / [`AtomicNodePtr<K, V>`] is ALREADY
//! generic, so per the trait-first rule the shared eviction layer is built as a
//! trait from the start. Compact eviction and exact fault-in are identical between
//! byte and character tries except for three accessors:
//!
//! 1. the `arc-swap` overlay root slot (`lockfree_root: AtomicNodePtr<K, V>`),
//! 2. the [`EpochManager`] (`enter_read` for active-reader accounting),
//! 3. the [`EvictionCoordinator`] that owns the exact topology generation,
//!    residency state, and LRU registry.
//!
//! and ONE capability: loading an `OnDisk` overlay child back into memory, which
//! is routed through the [`OverlayFaulter<K, V>`] super-trait
//! (`fault_overlay_slot`). The LOADERS stay variant-specific (char
//! `buffer_manager` + `load_char_node_from_disk_lazy`; byte `arena_manager` +
//! `deserialize_node_v2`). Selection carries compact path identifiers and an
//! immutable topology generation; byte and character registry adapters remain
//! monomorphized.
//!
//! # Exact overwrite and publication guards
//!
//! The iterative batch replacement checks each freshly reached victim's durable
//! stamp against the selected disk record. It then prepares an allocation-complete
//! exact residency transition and publishes only with a CAS that preserves the
//! same root/registry binding. A semantic writer clears that binding atomically,
//! so a stale batch cannot cross a checkpoint or overwrite boundary.
//!
//! Zero `unsafe`: only `AtomicNodePtr` revision operations, pure node copies,
//! `Arc` clone/drop, and the existing per-variant lazy loader
//! (called through the safe `&self` `fault_overlay_slot` boundary).

use std::cmp::Reverse;
use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};
use smallvec::SmallVec;

use crate::persistent_artrie::core::concurrency::EpochManager;
use crate::persistent_artrie::core::eviction::{
    CompactEvictionBatch, CompactEvictionPolicy, EvictionCoordinator, ExactEvictionOutcome,
    ExactFaultOutcome, PreparedPackedResidency, RegistryFamily,
};
use crate::persistent_artrie::core::key_encoding::KeyEncoding;
use crate::persistent_artrie::core::overlay::atomic_ptr::{
    AtomicNodePtr, PreparedBoundRootTransition, RootRevision,
};
use crate::persistent_artrie::core::overlay::faulter::OverlayFaulter;
use crate::persistent_artrie::core::overlay::node::{Child, ChildReplacements, OverlayNode};
use crate::persistent_artrie::core::swizzled_ptr::SwizzledPtr;
use crate::value::DictionaryValue;

/// Default fault-in retry budget for the shared read-fault default
/// ([`OverlayEvictable::find_leaf_faulting`]) used by
/// `LockFreeOverlay::overlay_value_get`. Equals both variants' per-variant
/// `lockfree_cas::DEFAULT_MAX_FAULTIN_RETRIES` (`16`): after this many loser-safe
/// install-CAS rebases, ONE final read-only walk answers (a still-`OnDisk` slot
/// reads absent — durable; a later read faults it — never spins).
pub(crate) const DEFAULT_MAX_FAULTIN_RETRIES: usize = 16;

struct DecodedFaultCache<K: KeyEncoding, V: DictionaryValue> {
    key_depth: usize,
    raw_pointer: u64,
    node: Arc<OverlayNode<K, V>>,
}

struct BatchPlanNode<U> {
    edge: Option<U>,
    first_child: Option<usize>,
    last_child: Option<usize>,
    next_sibling: Option<usize>,
    selected_candidate: Option<usize>,
}

fn build_batch_plan<U>(batch: &CompactEvictionBatch<U>) -> Option<Vec<BatchPlanNode<U>>>
where
    U: Copy + Eq + std::hash::Hash,
{
    let mut included = FxHashSet::default();
    included.try_reserve(batch.candidates.len()).ok()?;
    let mut topology_ids = Vec::new();
    topology_ids.try_reserve(batch.candidates.len()).ok()?;
    let mut ancestry = Vec::new();
    let mut unit_count = 1usize;
    for candidate in &batch.candidates {
        ancestry.clear();
        let mut path_id = candidate.path_id;
        loop {
            let index = path_id.index()?;
            if index >= batch.topology.len() {
                return None;
            }
            if included.contains(&path_id) {
                break;
            }
            if ancestry.len() == ancestry.capacity() {
                ancestry.try_reserve(1).ok()?;
            }
            ancestry.push(path_id);
            path_id = batch.topology.parent(path_id)?;
            if path_id.index().is_none() {
                break;
            }
        }
        for &path_id in ancestry.iter().rev() {
            if included.len() == included.capacity() {
                included.try_reserve(1).ok()?;
            }
            if !included.insert(path_id) {
                return None;
            }
            if topology_ids.len() == topology_ids.capacity() {
                topology_ids.try_reserve(1).ok()?;
            }
            topology_ids.push(path_id);
            unit_count = unit_count.checked_add(batch.topology.segment(path_id)?.len())?;
        }
    }

    let mut selected_by_id = FxHashMap::default();
    selected_by_id.try_reserve(batch.candidates.len()).ok()?;
    for (candidate_index, candidate) in batch.candidates.iter().enumerate() {
        if selected_by_id
            .insert(candidate.path_id, candidate_index)
            .is_some()
        {
            return None;
        }
    }

    let mut plan = Vec::new();
    plan.try_reserve_exact(unit_count).ok()?;
    plan.push(BatchPlanNode {
        edge: None,
        first_child: None,
        last_child: None,
        next_sibling: None,
        selected_candidate: None,
    });
    let mut endpoint_by_path = FxHashMap::default();
    endpoint_by_path.try_reserve(topology_ids.len()).ok()?;
    let mut edge_index = FxHashMap::default();
    edge_index.try_reserve(unit_count.saturating_sub(1)).ok()?;

    for path_id in topology_ids {
        let parent_path = batch.topology.parent(path_id)?;
        let mut endpoint = if let Some(parent_index) = parent_path.index() {
            *endpoint_by_path.get(&parent_index)?
        } else {
            0
        };
        for &edge in batch.topology.segment(path_id)? {
            if let Some(&existing) = edge_index.get(&(endpoint, edge)) {
                endpoint = existing;
                continue;
            }
            let child_index = plan.len();
            plan.push(BatchPlanNode {
                edge: Some(edge),
                first_child: None,
                last_child: None,
                next_sibling: None,
                selected_candidate: None,
            });
            if let Some(last_child) = plan[endpoint].last_child {
                plan[last_child].next_sibling = Some(child_index);
            } else {
                plan[endpoint].first_child = Some(child_index);
            }
            plan[endpoint].last_child = Some(child_index);
            edge_index.insert((endpoint, edge), child_index);
            endpoint = child_index;
        }
        endpoint_by_path.insert(path_id.index()?, endpoint);
        if let Some(&candidate_index) = selected_by_id.get(&path_id) {
            if plan[endpoint]
                .selected_candidate
                .replace(candidate_index)
                .is_some()
            {
                return None;
            }
        }
    }
    Some(plan)
}

struct BatchWalkFrame<K: KeyEncoding, V> {
    plan_index: usize,
    live: Arc<OverlayNode<K, V>>,
    next_child: Option<usize>,
    replacements: ChildReplacements<K, V>,
    successful_descendants: usize,
}

type SuccessfulCandidateIndices = SmallVec<[usize; 1]>;
#[cfg(test)]
type BatchReplacement<K, V> = (Arc<OverlayNode<K, V>>, SuccessfulCandidateIndices);

/// One frame in the optimized chain-eviction PDA. Path edges, selected
/// endpoints, and live parent snapshots share one reusable buffer.
struct ChainWalkFrame<K: KeyEncoding, V> {
    edge: K::Unit,
    selected_candidate: Option<usize>,
    parent: Option<Arc<OverlayNode<K, V>>>,
}

/// Build the compact PDA for a batch whose selected endpoints all lie on one
/// topology ancestry chain. This covers the common unary/prefix case without
/// the hash maps required by the general union-prefix plan. A branching or
/// aliased batch returns `None` and is handled by the general planner.
fn build_chain_plan<K: KeyEncoding, V: DictionaryValue>(
    batch: &CompactEvictionBatch<K::Unit>,
) -> Option<SmallVec<[ChainWalkFrame<K, V>; 16]>> {
    let deepest_index = batch
        .candidates
        .iter()
        .enumerate()
        .max_by_key(|(_, candidate)| candidate.depth)
        .map(|(index, _)| index)?;
    let deepest = batch.candidates.get(deepest_index)?;
    if deepest.depth == 0 {
        return None;
    }

    let mut frames = SmallVec::<[ChainWalkFrame<K, V>; 16]>::new();
    batch.materialize_path_mapped_into(deepest.path_id, &mut frames, |edge| {
        Some(ChainWalkFrame {
            edge,
            selected_candidate: None,
            parent: None,
        })
    })?;
    if frames.len() != deepest.depth {
        return None;
    }

    let mut selected = SmallVec::<[usize; 16]>::new();
    selected.try_reserve_exact(batch.candidates.len()).ok()?;
    selected.extend(0..batch.candidates.len());
    selected.sort_unstable_by_key(|&index| Reverse(batch.candidates[index].depth));

    let mut cursor = deepest.path_id;
    for candidate_index in selected {
        let candidate = batch.candidates.get(candidate_index)?;
        if candidate.depth == 0 || candidate.depth > frames.len() {
            return None;
        }
        while cursor != candidate.path_id {
            cursor = batch.topology.parent(cursor)?;
            cursor.index()?;
        }
        if frames[candidate.depth - 1]
            .selected_candidate
            .replace(candidate_index)
            .is_some()
        {
            return None;
        }
    }
    Some(frames)
}

fn build_chain_replacement_into<K: KeyEncoding, V: DictionaryValue>(
    old_root: &Arc<OverlayNode<K, V>>,
    frames: &mut [ChainWalkFrame<K, V>],
    batch: &CompactEvictionBatch<K::Unit>,
    successful: &mut SuccessfulCandidateIndices,
) -> Option<Arc<OverlayNode<K, V>>> {
    successful.clear();
    let mut current = Arc::clone(old_root);
    let mut reached = 0usize;
    for frame in frames.iter_mut() {
        let Some(child) = current
            .find_child(frame.edge)
            .and_then(Child::as_in_mem)
            .cloned()
        else {
            break;
        };
        frame.parent = Some(current);
        current = child;
        reached += 1;
    }

    for frame_index in (0..reached).rev() {
        let frame = frames.get_mut(frame_index)?;
        let parent = frame.parent.take()?;
        let replacement = if successful.is_empty() {
            let mut replacement = None;
            if let Some(candidate_index) = frame.selected_candidate {
                let candidate = batch.candidates.get(candidate_index)?;
                if candidate.disk_ptr.disk_location().is_some()
                    && current.durable_stamp() == candidate.disk_ptr.to_raw()
                {
                    replacement = Some(Child::OnDisk(candidate.disk_ptr.clone()));
                    successful.push(candidate_index);
                }
            }
            replacement
        } else {
            Some(Child::InMem(current))
        };
        if let Some(replacement) = replacement {
            let mut replacements = SmallVec::<[(K::Unit, Child<K, V>); 1]>::new();
            replacements.push((frame.edge, replacement));
            current = Arc::new(parent.try_with_child_replacements(replacements, 1).ok()?);
        } else {
            current = parent;
        }
    }

    if successful.is_empty() {
        None
    } else {
        Some(current)
    }
}

fn build_batch_replacement_into<K: KeyEncoding, V: DictionaryValue>(
    old_root: &Arc<OverlayNode<K, V>>,
    plan: &[BatchPlanNode<K::Unit>],
    batch: &CompactEvictionBatch<K::Unit>,
    successful: &mut SuccessfulCandidateIndices,
) -> Option<Arc<OverlayNode<K, V>>> {
    successful.clear();
    let mut frames = Vec::new();
    let maximum_selected_depth = batch
        .candidates
        .iter()
        .map(|candidate| candidate.depth)
        .max()
        .unwrap_or(0);
    let initial_frame_capacity = maximum_selected_depth
        .checked_add(1)?
        .min(plan.len())
        .max(1);
    frames.try_reserve_exact(initial_frame_capacity).ok()?;
    frames.push(BatchWalkFrame {
        plan_index: 0,
        live: Arc::clone(old_root),
        next_child: plan.first()?.first_child,
        replacements: SmallVec::new(),
        successful_descendants: 0,
    });
    loop {
        let next_child = {
            let frame = frames.last_mut()?;
            let next = frame.next_child;
            if let Some(index) = next {
                frame.next_child = plan.get(index)?.next_sibling;
            }
            next
        };
        if let Some(child_plan_index) = next_child {
            let edge = plan.get(child_plan_index)?.edge?;
            let child_live = frames
                .last()?
                .live
                .find_child(edge)
                .and_then(Child::as_in_mem)
                .cloned();
            if let Some(child_live) = child_live {
                if frames.len() == frames.capacity() {
                    frames.try_reserve(1).ok()?;
                }
                frames.push(BatchWalkFrame {
                    plan_index: child_plan_index,
                    live: child_live,
                    next_child: plan.get(child_plan_index)?.first_child,
                    replacements: SmallVec::new(),
                    successful_descendants: 0,
                });
            }
            continue;
        }

        let frame = frames.pop()?;
        let had_descendant_replacements = !frame.replacements.is_empty();
        let mut live = frame.live;
        if had_descendant_replacements {
            let mut replacements = frame.replacements;
            replacements.sort_unstable_by_key(|(edge, _)| *edge);
            if replacements.windows(2).any(|pair| pair[0].0 == pair[1].0) {
                return None;
            }
            live = Arc::new(
                live.try_with_child_replacements(replacements, frame.successful_descendants)
                    .ok()?,
            );
        }

        let mut replacement = None;
        let mut changed = had_descendant_replacements;
        let mut successful_here = 0usize;
        if frame.plan_index != 0 && frame.successful_descendants == 0 {
            if let Some(candidate_index) = plan.get(frame.plan_index)?.selected_candidate {
                let candidate = batch.candidates.get(candidate_index)?;
                if candidate.disk_ptr.disk_location().is_some()
                    && live.durable_stamp() == candidate.disk_ptr.to_raw()
                {
                    replacement = Some(Child::OnDisk(candidate.disk_ptr.clone()));
                    changed = true;
                    successful_here = 1;
                    successful.push(candidate_index);
                }
            }
        }
        let subtree_successes = frame.successful_descendants.checked_add(successful_here)?;

        if let Some(parent) = frames.last_mut() {
            if changed {
                let edge = plan.get(frame.plan_index)?.edge?;
                parent.replacements.try_reserve(1).ok()?;
                parent
                    .replacements
                    .push((edge, replacement.unwrap_or(Child::InMem(live))));
            }
            parent.successful_descendants = parent
                .successful_descendants
                .checked_add(subtree_successes)?;
        } else {
            if frame.plan_index != 0 {
                return None;
            }
            return (!successful.is_empty()).then_some(live);
        }
    }
}

/// Resident-budget chain executor. Selected endpoints are tested shallow-to-deep;
/// the first exact stamped ancestor replaces its complete subtree. A stale
/// ancestor is not authority and therefore falls through to deeper selected
/// descendants without recursion or repeated root walks.
fn build_chain_ancestor_replacement_into<K: KeyEncoding, V: DictionaryValue>(
    old_root: &Arc<OverlayNode<K, V>>,
    frames: &mut [ChainWalkFrame<K, V>],
    batch: &CompactEvictionBatch<K::Unit>,
    successful: &mut SuccessfulCandidateIndices,
) -> Option<Arc<OverlayNode<K, V>>> {
    successful.clear();
    let mut current = Arc::clone(old_root);
    for frame_index in 0..frames.len() {
        let frame = frames.get_mut(frame_index)?;
        let child = current
            .find_child(frame.edge)
            .and_then(Child::as_in_mem)
            .cloned()?;
        frame.parent = Some(current);

        if let Some(candidate_index) = frame.selected_candidate {
            let candidate = batch.candidates.get(candidate_index)?;
            if candidate.disk_ptr.disk_location().is_some()
                && child.durable_stamp() == candidate.disk_ptr.to_raw()
            {
                let parent = frame.parent.take()?;
                let mut replacements = SmallVec::<[(K::Unit, Child<K, V>); 1]>::new();
                replacements.push((frame.edge, Child::OnDisk(candidate.disk_ptr.clone())));
                let mut replacement_root =
                    Arc::new(parent.try_with_child_replacements(replacements, 1).ok()?);
                for ancestor_index in (0..frame_index).rev() {
                    let ancestor_frame = frames.get_mut(ancestor_index)?;
                    let ancestor = ancestor_frame.parent.take()?;
                    let mut replacements = SmallVec::<[(K::Unit, Child<K, V>); 1]>::new();
                    replacements.push((ancestor_frame.edge, Child::InMem(replacement_root)));
                    replacement_root =
                        Arc::new(ancestor.try_with_child_replacements(replacements, 1).ok()?);
                }
                successful.push(candidate_index);
                return Some(replacement_root);
            }
        }
        current = child;
    }

    // No exact endpoint was found. Release every captured parent before a
    // caller retry so this reusable PDA never retains a stale root revision.
    for frame in frames {
        frame.parent = None;
    }
    None
}

/// Resident-budget branching executor. A valid selected child is replaced on
/// entry and its plan subtree is pruned; a stale selected child is traversed so
/// previously ranked descendants remain exact fallbacks.
fn build_batch_ancestor_replacement_into<K: KeyEncoding, V: DictionaryValue>(
    old_root: &Arc<OverlayNode<K, V>>,
    plan: &[BatchPlanNode<K::Unit>],
    batch: &CompactEvictionBatch<K::Unit>,
    successful: &mut SuccessfulCandidateIndices,
) -> Option<Arc<OverlayNode<K, V>>> {
    successful.clear();
    let mut frames = Vec::new();
    let maximum_selected_depth = batch
        .candidates
        .iter()
        .map(|candidate| candidate.depth)
        .max()
        .unwrap_or(0);
    let initial_frame_capacity = maximum_selected_depth
        .checked_add(1)?
        .min(plan.len())
        .max(1);
    frames.try_reserve_exact(initial_frame_capacity).ok()?;
    frames.push(BatchWalkFrame {
        plan_index: 0,
        live: Arc::clone(old_root),
        next_child: plan.first()?.first_child,
        replacements: SmallVec::new(),
        successful_descendants: 0,
    });

    loop {
        let next_child = {
            let frame = frames.last_mut()?;
            let next = frame.next_child;
            if let Some(index) = next {
                frame.next_child = plan.get(index)?.next_sibling;
            }
            next
        };
        if let Some(child_plan_index) = next_child {
            let child_plan = plan.get(child_plan_index)?;
            let edge = child_plan.edge?;
            let child_live = frames
                .last()?
                .live
                .find_child(edge)
                .and_then(Child::as_in_mem)
                .cloned();
            let Some(child_live) = child_live else {
                continue;
            };

            if let Some(candidate_index) = child_plan.selected_candidate {
                let candidate = batch.candidates.get(candidate_index)?;
                if candidate.disk_ptr.disk_location().is_some()
                    && child_live.durable_stamp() == candidate.disk_ptr.to_raw()
                {
                    let parent = frames.last_mut()?;
                    parent.replacements.try_reserve(1).ok()?;
                    parent
                        .replacements
                        .push((edge, Child::OnDisk(candidate.disk_ptr.clone())));
                    parent.successful_descendants = parent.successful_descendants.checked_add(1)?;
                    successful.push(candidate_index);
                    continue;
                }
            }

            if frames.len() == frames.capacity() {
                frames.try_reserve(1).ok()?;
            }
            frames.push(BatchWalkFrame {
                plan_index: child_plan_index,
                live: child_live,
                next_child: child_plan.first_child,
                replacements: SmallVec::new(),
                successful_descendants: 0,
            });
            continue;
        }

        let frame = frames.pop()?;
        let changed = !frame.replacements.is_empty();
        let mut live = frame.live;
        if changed {
            let mut replacements = frame.replacements;
            replacements.sort_unstable_by_key(|(edge, _)| *edge);
            if replacements.windows(2).any(|pair| pair[0].0 == pair[1].0) {
                return None;
            }
            live = Arc::new(
                live.try_with_child_replacements(replacements, frame.successful_descendants)
                    .ok()?,
            );
        }

        if let Some(parent) = frames.last_mut() {
            if changed {
                let edge = plan.get(frame.plan_index)?.edge?;
                parent.replacements.try_reserve(1).ok()?;
                parent.replacements.push((edge, Child::InMem(live)));
            }
            parent.successful_descendants = parent
                .successful_descendants
                .checked_add(frame.successful_descendants)?;
        } else {
            if frame.plan_index != 0 {
                return None;
            }
            return (!successful.is_empty()).then_some(live);
        }
    }
}

#[cfg(test)]
fn build_chain_replacement<K: KeyEncoding, V: DictionaryValue>(
    old_root: &Arc<OverlayNode<K, V>>,
    frames: &mut [ChainWalkFrame<K, V>],
    batch: &CompactEvictionBatch<K::Unit>,
) -> Option<BatchReplacement<K, V>> {
    let mut successful = SuccessfulCandidateIndices::new();
    successful.try_reserve(batch.candidates.len()).ok()?;
    let root = build_chain_replacement_into(old_root, frames, batch, &mut successful)?;
    Some((root, successful))
}

#[cfg(test)]
fn build_batch_replacement<K: KeyEncoding, V: DictionaryValue>(
    old_root: &Arc<OverlayNode<K, V>>,
    plan: &[BatchPlanNode<K::Unit>],
    batch: &CompactEvictionBatch<K::Unit>,
) -> Option<BatchReplacement<K, V>> {
    let mut successful = SuccessfulCandidateIndices::new();
    successful.try_reserve(batch.candidates.len()).ok()?;
    let root = build_batch_replacement_into(old_root, plan, batch, &mut successful)?;
    Some((root, successful))
}

/// The SHARED GENERIC overlay-eviction + read-fault capability — a subtrait of
/// [`OverlayFaulter<K, V>`] (the per-variant `OnDisk`-child loader).
///
/// `K`/`V` are the key encoding + value; `S` is the block-storage parameter the
/// variant's loader needs (it never appears in this trait's signatures — it is
/// carried so the impl can name the variant's `<V, S>` type). The three accessors
/// expose the per-attempt primitives' only variant-specific state; the two default
/// methods are the lifted primitives.
pub(crate) trait OverlayEvictable<K: KeyEncoding, V: DictionaryValue, S>:
    OverlayFaulter<K, V>
{
    /// The `arc-swap` overlay root slot (`lockfree_root`), or `None` when the
    /// lock-free overlay is not enabled.
    /// Production checkpoint and resident-budget eviction drivers use this
    /// through the shared batch primitive. The read-fault default takes its
    /// root slot as a parameter.
    fn overlay_root_slot(&self) -> Option<&AtomicNodePtr<K, V>>;

    /// The trie's epoch manager (pinned `enter_read` for reader accounting parity;
    /// the overlay needs no EBR for correctness — reclamation is by `Arc` refcount).
    fn overlay_epoch_manager(&self) -> &EpochManager;

    /// Clone out the installed eviction coordinator (the LRU registry lives here;
    /// the variant-specific batch driver uses it to `remove_hash` an evicted path).
    /// `None` when eviction is not enabled.
    fn overlay_eviction_coordinator(&self) -> Option<Arc<EvictionCoordinator>>;

    /// Resolve the successful topology endpoints against the currently
    /// published registry and preallocate the exact subtree-residency commit.
    fn prepare_overlay_eviction_commit(
        &self,
        coordinator: &EvictionCoordinator,
        root_revision: &RootRevision<K, V>,
        batch: &CompactEvictionBatch<K::Unit>,
        successful: &mut [usize],
    ) -> Option<PreparedPackedResidency>;

    /// Revalidate authority, publish the preallocated root transition, and
    /// commit residency under one coordinator lifecycle transaction.
    fn commit_overlay_eviction(
        &self,
        coordinator: &EvictionCoordinator,
        root: &AtomicNodePtr<K, V>,
        root_transition: PreparedBoundRootTransition<K, V>,
    ) -> ExactEvictionOutcome;

    /// Capture an exact fault anchor while the registry generation that names it
    /// is still available. This call holds no registry lock across disk I/O.
    fn prepare_overlay_fault_commit(
        &self,
        coordinator: &EvictionCoordinator,
        root_revision: &RootRevision<K, V>,
        path: &[K::Unit],
        disk_ptr: &SwizzledPtr,
    ) -> Option<PreparedPackedResidency>;

    /// Revalidate authority, publish the preallocated fault replacement, and
    /// mark the exact durable record resident in one lifecycle transaction.
    fn commit_overlay_fault(
        &self,
        coordinator: &EvictionCoordinator,
        root: &AtomicNodePtr<K, V>,
        root_transition: PreparedBoundRootTransition<K, V>,
    ) -> ExactFaultOutcome;

    /// Record a fault-in install-CAS attempt (won OR lost) in the variant's
    /// contention monitor. Default no-op; char overrides it to bump its
    /// `cas_retries` counter EXACTLY as its pre-lift `find_leaf_faulting` did
    /// (preserving the observable `cas_retry_count()`). Byte's pre-lift hot paths
    /// did not bump on fault-in (byte had no fault-in), so the byte impl keeps the
    /// default no-op — no behavioral delta on either side.
    #[inline]
    fn note_faultin_cas(&self) {}

    /// Evict a compact set of exact topology endpoints in one stack-safe
    /// union-prefix traversal and one root CAS per attempt.
    ///
    /// Manual and memory-pressure batches use `DescendantFirst`, preserving the
    /// legacy leaf-first stamp semantics. Checkpoint-tail resident-budget batches
    /// use `ResidentBudgetAncestorClosure`: the first exact selected ancestor
    /// replaces its complete durable subtree, while a stale ancestor falls through
    /// to previously ranked exact descendants. Policy dispatch occurs once per
    /// retry; neither executor recurses or repeats root walks per candidate.
    fn evict_overlay_batch(
        &self,
        batch: CompactEvictionBatch<K::Unit>,
        max_rebase_retries: usize,
    ) -> (usize, usize)
    where
        K: RegistryFamily,
    {
        if batch.candidates.is_empty() {
            return (0, 0);
        }
        let mut chain_plan = build_chain_plan::<K, V>(&batch);
        let plan = if chain_plan.is_none() {
            let Some(plan) = build_batch_plan(&batch) else {
                return (0, 0);
            };
            Some(plan)
        } else {
            None
        };
        let Some(root_slot) = self.overlay_root_slot() else {
            return (0, 0);
        };
        // Everything after a winning root CAS must be allocation-complete and
        // structurally infallible. Clone the coordinator before the loop; a
        // missing coordinator means there is no exact registry transition, so
        // reclamation must fail closed before publication.
        let Some(coordinator) = self.overlay_eviction_coordinator() else {
            return (0, 0);
        };
        let mut successful = SuccessfulCandidateIndices::new();
        if successful.try_reserve(batch.candidates.len()).is_err() {
            return (0, 0);
        }

        for _attempt in 0..=max_rebase_retries {
            let _epoch = self.overlay_epoch_manager().enter_read();
            let Some(old_revision) = root_slot.load_revision() else {
                return (0, 0);
            };
            let old_root = old_revision.node();
            let new_root = match (batch.policy, chain_plan.as_deref_mut(), plan.as_deref()) {
                (CompactEvictionPolicy::DescendantFirst, Some(frames), _) => {
                    build_chain_replacement_into(old_root, frames, &batch, &mut successful)
                }
                (CompactEvictionPolicy::DescendantFirst, None, Some(plan)) => {
                    build_batch_replacement_into(old_root, plan, &batch, &mut successful)
                }
                (CompactEvictionPolicy::ResidentBudgetAncestorClosure, Some(frames), _) => {
                    build_chain_ancestor_replacement_into(old_root, frames, &batch, &mut successful)
                }
                (CompactEvictionPolicy::ResidentBudgetAncestorClosure, None, Some(plan)) => {
                    build_batch_ancestor_replacement_into(old_root, plan, &batch, &mut successful)
                }
                _ => None,
            };
            let Some(new_root) = new_root else {
                return (0, 0);
            };
            if successful.is_empty() {
                return (0, 0);
            }

            let Some(packed) = self.prepare_overlay_eviction_commit(
                coordinator.as_ref(),
                &old_revision,
                &batch,
                &mut successful,
            ) else {
                return (0, 0);
            };
            let Some(root_transition) =
                AtomicNodePtr::prepare_exact_root_transition(&old_revision, new_root, packed)
            else {
                return (0, 0);
            };
            match self.commit_overlay_eviction(coordinator.as_ref(), root_slot, root_transition) {
                ExactEvictionOutcome::Committed(nodes, bytes) => return (nodes, bytes),
                ExactEvictionOutcome::RootAdvanced => continue,
                ExactEvictionOutcome::AuthorityLost => return (0, 0),
            }
        }
        (0, 0)
    }

    /// Find the leaf node for `key` in the overlay, FAULTING any `OnDisk` (evicted)
    /// child back in along the way. The K-generic LIFT of char's proven
    /// `find_leaf_faulting` (char `lockfree_cas.rs`); behavior-identical.
    ///
    /// Per attempt (bounded by `max_faultin_retries`): pin the epoch, `load()` the
    /// root, walk `key` top-down; `None` edge ⇒ absent (`Ok(None)`); `InMem` ⇒
    /// descend; **`OnDisk` ⇒ fault** (`fault_overlay_slot`, rebuild the spine
    /// bottom-up splicing `Child::InMem(loaded)`, then loser-safe install-CAS), then
    /// rebase to a fresh root load. A retry-local `(depth, disk pointer, decoded
    /// node)` cache prevents checkpoint/retirement races from repeating disk I/O.
    /// If the bounded publication retry budget is exhausted, the last captured
    /// immutable snapshot is completed transiently and returned without reporting
    /// a false absence.
    ///
    /// **Idempotent / loser-safe:** two faulters each load their own `Arc`; exactly
    /// one install CAS wins, the loser drops + re-reads the now-`InMem` child.
    ///
    /// Maintenance coupling: this is the inverse direction of compact eviction;
    /// eviction swaps `InMem` to `OnDisk`, while fault-in swaps `OnDisk` to
    /// `InMem` under the same exact generation binding.
    ///
    /// 🚫 NEVER call this from a read-BEFORE-WAL-append hot-insert present-hoist: a
    /// faulting read before the WAL append, racing a checkpoint/eviction that holds
    /// the buffer/arena lock, is a lock-ordering inversion (char's documented
    /// "75-minute hang"). Use the NON-faulting in-memory walk for any such hoist.
    fn find_leaf_faulting(
        &self,
        root_slot: &AtomicNodePtr<K, V>,
        key: &[K::Unit],
        max_faultin_retries: usize,
    ) -> crate::persistent_artrie::core::error::Result<Option<Arc<OverlayNode<K, V>>>>
    where
        K: RegistryFamily,
    {
        // One read-only walk of `root` (no faulting): used for the empty-key leaf
        // and the post-exhaustion liveness fallback. A still-OnDisk slot reads
        // absent (durable; a later call retries) — never spins.
        fn walk_no_fault<K: KeyEncoding, V: DictionaryValue>(
            root: &Arc<OverlayNode<K, V>>,
            key: &[K::Unit],
        ) -> Option<Arc<OverlayNode<K, V>>> {
            let mut current = Arc::clone(root);
            for &edge in key {
                let child = current.find_child(edge)?;
                let child_arc = child.as_in_mem()?;
                let next = Arc::clone(child_arc);
                current = next;
            }
            if current.is_final() {
                Some(current)
            } else {
                None
            }
        }

        // Complete a point read against an already-decoded immutable subtree.
        // This is the bounded retry fallback: it performs no root publication or
        // registry mutation and is therefore linearizable at the captured root
        // revision. The walk is iterative and faults each remaining durable edge
        // at most once.
        let walk_faulting_snapshot = |mut current: Arc<OverlayNode<K, V>>,
                                      suffix: &[K::Unit]|
         -> crate::persistent_artrie::core::error::Result<_> {
            for &edge in suffix {
                let Some(child) = current.find_child(edge) else {
                    return Ok(None);
                };
                let next = match child {
                    Child::InMem(child) => Arc::clone(child),
                    Child::OnDisk(ptr) if !ptr.is_null() => self.try_fault_overlay_slot(ptr)?,
                    Child::OnDisk(_) => return Ok(None),
                };
                current = next;
            }
            Ok(current.is_final().then_some(current))
        };

        // Reused only when a fresh root walk encounters the same durable
        // occurrence at the same key depth. Durable record identity is immutable,
        // so retaining the decoded Arc is safe across root-revision rebases.
        let mut decoded_cache: Option<DecodedFaultCache<K, V>> = None;
        let mut fallback_snapshot: Option<(Arc<OverlayNode<K, V>>, usize)> = None;

        // +1 so we always get at least one fresh-root liveness walk even when
        // `max_faultin_retries == 0`.
        'retry: for _attempt in 0..=max_faultin_retries {
            let _epoch = self.overlay_epoch_manager().enter_read();

            let old_revision = match root_slot.load_revision() {
                Some(revision) => revision,
                None => return Ok(None), // empty overlay
            };
            let old_root = Arc::clone(old_revision.node());
            // Refresh on every attempt. Disable atomically removes the old
            // coordinator and root binding; re-enable installs a distinct one.
            let coordinator = self.overlay_eviction_coordinator();

            // Walk top-down, collecting (node, edge) for a possible rebuild, until
            // we either reach the leaf (all InMem ⇒ answer directly), hit a missing
            // edge (absent), or hit an OnDisk edge (fault + CAS + rebase).
            let mut spine = super::OverlayPathSpine::<K, V>::new();
            let mut current = &old_root;
            let mut faulted = false;

            let mut idx = 0usize;
            while idx < key.len() {
                let edge = key[idx];
                let child = match current.find_child(edge) {
                    Some(c) => c,
                    None => return Ok(None), // genuinely absent on this snapshot
                };
                match child {
                    Child::InMem(child_arc) => {
                        super::try_push_overlay_path_spine(
                            &mut spine,
                            super::OverlayPathFrame {
                                node: super::OverlayNodeHandle::Borrowed(current),
                                unit: edge,
                            },
                            key.len(),
                        )
                        .map_err(|source| {
                            crate::persistent_artrie::error::PersistentARTrieError::allocation_failed(
                                "overlay fault-in path",
                                key.len(),
                                source,
                            )
                        })?;
                        current = child_arc;
                        idx += 1;
                    }
                    Child::OnDisk(ptr) if !ptr.is_null() => {
                        // Resolve the exact registry occurrence before disk I/O.
                        // This preparation owns every generation/path handle it
                        // needs and releases the registry read lock immediately.
                        let prepared_fault = coordinator.as_ref().and_then(|coordinator| {
                            self.prepare_overlay_fault_commit(
                                coordinator.as_ref(),
                                &old_revision,
                                &key[..=idx],
                                ptr,
                            )
                        });
                        if old_revision.eviction_binding().is_some() && prepared_fault.is_none() {
                            // Retirement publishes an unbound metadata revision
                            // before withdrawing its cold registry slot. If that
                            // publication already won, rebase before doing any disk
                            // I/O. Only an unchanged, still-bound snapshot proceeds
                            // to the transient single-decode fallback below.
                            let latest = root_slot.load_revision();
                            let same_revision = match latest.as_ref() {
                                Some(latest) => old_revision.same_revision(latest),
                                None => false,
                            };
                            if !same_revision {
                                continue 'retry;
                            }
                        }
                        // FAULT: load the OnDisk child back into memory (the
                        // per-variant loader, via the `OverlayFaulter` seam), then
                        // rebuild the spine bottom-up splicing it InMem at THIS edge.
                        // Exact loader failures propagate to the caller. Durable
                        // mutation must never acknowledge I/O/decode failure as
                        // proven absence; best-effort read APIs may explicitly map
                        // the returned error to a resident-only fallback.
                        let raw = ptr.to_raw();
                        let loaded = match decoded_cache.as_ref() {
                            Some(cached)
                                if cached.key_depth == idx && cached.raw_pointer == raw =>
                            {
                                Arc::clone(&cached.node)
                            }
                            _ => {
                                let node = self.try_fault_overlay_slot(ptr)?;
                                decoded_cache = Some(DecodedFaultCache {
                                    key_depth: idx,
                                    raw_pointer: raw,
                                    node: Arc::clone(&node),
                                });
                                node
                            }
                        };
                        fallback_snapshot = Some((Arc::clone(&loaded), idx + 1));

                        // The deepest rebuilt node is `current` with its `edge` child
                        // replaced by InMem(loaded); each shallower ancestor in
                        // `spine` is re-linked InMem around the rebuilt child.
                        let mut new_child =
                            Arc::new(current.with_child(edge, Child::InMem(loaded)));
                        for frame in spine.iter().rev() {
                            new_child = Arc::new(
                                frame
                                    .node
                                    .node()
                                    .with_child(frame.unit, Child::InMem(new_child)),
                            );
                        }

                        // Loser-safe install CAS against the snapshot root. Whether
                        // we won (published) or lost (a racer advanced the root,
                        // possibly already faulting this node), rebase. Record the
                        // attempt in the variant's contention monitor (char's
                        // pre-lift `find_leaf_faulting` bumped `cas_retries` on both
                        // the win and the loss arm).
                        let attempted_cas = if let Some(packed) = prepared_fault {
                            let Some(root_transition) =
                                AtomicNodePtr::prepare_exact_root_transition(
                                    &old_revision,
                                    new_child,
                                    packed,
                                )
                            else {
                                faulted = true;
                                break;
                            };
                            let Some(coordinator) = coordinator.as_ref() else {
                                faulted = true;
                                break;
                            };
                            match self.commit_overlay_fault(
                                coordinator.as_ref(),
                                root_slot,
                                root_transition,
                            ) {
                                ExactFaultOutcome::Committed | ExactFaultOutcome::RootAdvanced => {
                                    true
                                }
                                ExactFaultOutcome::AuthorityLost => false,
                            }
                        } else if old_revision.eviction_binding().is_some() {
                            // The captured revision was bound, but retirement or
                            // replacement removed exact registry authority before
                            // preparation. Never clear a still-authoritative binding
                            // through a semantic CAS; retain the decoded node and
                            // rebase. The fallback below can answer from this exact
                            // immutable snapshot if bounded retries are exhausted.
                            false
                        } else {
                            // Faulting without an exact registry transition remains
                            // structurally safe, but atomically clears any binding so
                            // stale eviction candidates cannot use under-counted
                            // residency metadata.
                            let _ = root_slot.compare_exchange_revision_counted(
                                &old_revision,
                                new_child,
                                0,
                            );
                            true
                        };
                        if attempted_cas {
                            self.note_faultin_cas();
                        }
                        faulted = true;
                        break;
                    }
                    // Null filler (never yielded as a real child) ⇒ absent.
                    Child::OnDisk(_) => return Ok(None),
                }
            }

            if faulted {
                // Re-walk from a freshly-published root on the next attempt.
                continue;
            }

            // Reached the terminal depth with an all-InMem spine: answer directly.
            return Ok(if current.is_final() {
                Some(Arc::clone(current))
            } else {
                None
            });
        }

        // Retry budget exhausted. First use the freshest fully-resident root when
        // possible. If it still contains an OnDisk edge, complete the last exact
        // immutable snapshot transiently; this preserves point-read correctness
        // without an unbounded CAS loop or repeated decode of the contested edge.
        let final_root = match root_slot.load() {
            Some(r) => r,
            None => return Ok(None),
        };
        if let Some(found) = walk_no_fault(&final_root, key) {
            return Ok(Some(found));
        }
        if let Some((decoded, suffix_start)) = fallback_snapshot {
            let _epoch = self.overlay_epoch_manager().enter_read();
            return walk_faulting_snapshot(decoded, &key[suffix_start..]);
        }
        Ok(None)
    }
}

#[cfg(test)]
mod fault_race_tests {
    use super::OverlayEvictable;
    use crate::persistent_artrie::core::concurrency::EpochManager;
    use crate::persistent_artrie::core::error::CollectionAllocationError;
    use crate::persistent_artrie::core::eviction::{
        CompactEvictionBatch, DiskLocationRegistry, EvictionConfig, EvictionCoordinator,
        ExactEvictionOutcome, ExactFaultOutcome, PreparedPackedResidency,
        PreparedRegistryPublication, RegistryPathId, RegistryPublicationOutcome, RetirementOutcome,
    };
    use crate::persistent_artrie::core::key_encoding::ByteKey;
    use crate::persistent_artrie::core::overlay::node::Child;
    use crate::persistent_artrie::core::overlay::{
        overlay_spine_failpoint, AtomicNodePtr, OverlayFaulter, OverlayNode,
        PreparedBoundRootTransition, RootRevision,
    };
    use crate::persistent_artrie::core::swizzled_ptr::{NodeType, SwizzledPtr};
    use crate::persistent_artrie::error::PersistentARTrieError;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    struct RetirementFaultHarness {
        root: AtomicNodePtr<ByteKey, u64>,
        epoch: Arc<EpochManager>,
        coordinator: Mutex<Option<Arc<EvictionCoordinator>>>,
        decoded: Arc<OverlayNode<ByteKey, u64>>,
        loads: AtomicUsize,
    }

    impl OverlayFaulter<ByteKey, u64> for RetirementFaultHarness {
        fn try_fault_overlay_slot(
            &self,
            _slot: &SwizzledPtr,
        ) -> crate::persistent_artrie::core::error::Result<Arc<OverlayNode<ByteKey, u64>>> {
            self.loads.fetch_add(1, Ordering::Relaxed);
            let coordinator = self
                .coordinator
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some(coordinator) = coordinator {
                assert_eq!(
                    coordinator.retire_from_trie_with_root(&self.root),
                    RetirementOutcome::ExactBindingDetached
                );
            }
            Ok(Arc::clone(&self.decoded))
        }
    }

    impl OverlayEvictable<ByteKey, u64, ()> for RetirementFaultHarness {
        fn overlay_root_slot(&self) -> Option<&AtomicNodePtr<ByteKey, u64>> {
            Some(&self.root)
        }

        fn overlay_epoch_manager(&self) -> &EpochManager {
            &self.epoch
        }

        fn overlay_eviction_coordinator(&self) -> Option<Arc<EvictionCoordinator>> {
            self.coordinator
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
                .map(Arc::clone)
        }

        fn prepare_overlay_eviction_commit(
            &self,
            coordinator: &EvictionCoordinator,
            root_revision: &RootRevision<ByteKey, u64>,
            batch: &CompactEvictionBatch<u8>,
            successful: &mut [usize],
        ) -> Option<PreparedPackedResidency> {
            coordinator.prepare_byte_eviction_commit(root_revision, batch, successful)
        }

        fn commit_overlay_eviction(
            &self,
            coordinator: &EvictionCoordinator,
            root: &AtomicNodePtr<ByteKey, u64>,
            root_transition: PreparedBoundRootTransition<ByteKey, u64>,
        ) -> ExactEvictionOutcome {
            coordinator.commit_byte_eviction_transaction(root, root_transition)
        }

        fn prepare_overlay_fault_commit(
            &self,
            coordinator: &EvictionCoordinator,
            root_revision: &RootRevision<ByteKey, u64>,
            path: &[u8],
            disk_ptr: &SwizzledPtr,
        ) -> Option<PreparedPackedResidency> {
            coordinator.prepare_byte_fault_commit(root_revision, path, disk_ptr)
        }

        fn commit_overlay_fault(
            &self,
            coordinator: &EvictionCoordinator,
            root: &AtomicNodePtr<ByteKey, u64>,
            root_transition: PreparedBoundRootTransition<ByteKey, u64>,
        ) -> ExactFaultOutcome {
            coordinator.commit_byte_fault_transaction(root, root_transition)
        }
    }

    #[test]
    fn authority_loss_rebases_with_one_disk_decode_and_no_false_absence() {
        let epoch = Arc::new(EpochManager::new());
        let coordinator = EvictionCoordinator::new(EvictionConfig::default(), Arc::clone(&epoch));
        let disk_ptr = SwizzledPtr::on_disk(7, 41, NodeType::Node4);
        let root_node = Arc::new(
            OverlayNode::<ByteKey, u64>::new().with_child(b'x', Child::OnDisk(disk_ptr.clone())),
        );
        let root = AtomicNodePtr::new_with_term_count(root_node, 1);
        let captured = root.load_revision().expect("captured test root");
        let mut registry = DiskLocationRegistry::new();
        let path = registry
            .try_reserve_byte_path(RegistryPathId::ROOT, b"x")
            .expect("reserve fault path");
        registry
            .register_nonresident_byte_path(path, disk_ptr, 29, 1, NodeType::Node4)
            .expect("register nonresident fault target");
        let publication = PreparedRegistryPublication::try_new(
            Arc::clone(&coordinator),
            &captured,
            registry,
            Vec::new(),
        )
        .expect("prepare bound nonresident registry");
        assert_eq!(
            publication.publish(&coordinator, &root),
            RegistryPublicationOutcome::Published
        );

        let harness = RetirementFaultHarness {
            root,
            epoch,
            coordinator: Mutex::new(Some(coordinator)),
            decoded: Arc::new(OverlayNode::new().as_final()),
            loads: AtomicUsize::new(0),
        };

        let found = harness
            .find_leaf_faulting(&harness.root, b"x", 4)
            .expect("fault read succeeds")
            .expect("retirement race must not report absence");
        assert!(found.is_final());
        assert_eq!(harness.loads.load(Ordering::Relaxed), 1);
        let published = harness.root.load_revision().expect("faulted root");
        assert!(published.eviction_binding().is_none());
        assert!(published
            .node()
            .find_child(b'x')
            .and_then(Child::as_in_mem)
            .is_some());
    }

    fn resident_prefix_with_disk_tail(
        prefix_len: usize,
        disk_ptr: &SwizzledPtr,
    ) -> Arc<OverlayNode<ByteKey, u64>> {
        let mut node =
            Arc::new(OverlayNode::new().with_child(b'z', Child::OnDisk(disk_ptr.clone())));
        for _ in 0..prefix_len {
            node = Arc::new(OverlayNode::new().with_child(b'x', Child::InMem(node)));
        }
        node
    }

    fn resident_chain(depth: usize) -> Arc<OverlayNode<ByteKey, u64>> {
        let mut node = Arc::new(OverlayNode::new().as_final());
        for _ in 0..depth {
            node = Arc::new(OverlayNode::new().with_child(b'x', Child::InMem(node)));
        }
        node
    }

    #[test]
    fn fault_spine_reservation_failure_preserves_root_and_registry_authority() {
        const PREFIX_LEN: usize = 17;

        let epoch = Arc::new(EpochManager::new());
        let coordinator = EvictionCoordinator::new(EvictionConfig::default(), Arc::clone(&epoch));
        let disk_ptr = SwizzledPtr::on_disk(7, 43, NodeType::Node4);
        let mut key = vec![b'x'; PREFIX_LEN];
        key.push(b'z');
        let root = AtomicNodePtr::new_with_term_count(
            resident_prefix_with_disk_tail(PREFIX_LEN, &disk_ptr),
            1,
        );
        let captured = root.load_revision().expect("captured test root");
        let mut registry = DiskLocationRegistry::new();
        let path = registry
            .try_reserve_byte_path(RegistryPathId::ROOT, &key)
            .expect("reserve fault path");
        registry
            .register_nonresident_byte_path(path, disk_ptr.clone(), 31, key.len(), NodeType::Node4)
            .expect("register nonresident fault target");
        let publication = PreparedRegistryPublication::try_new(
            Arc::clone(&coordinator),
            &captured,
            registry,
            Vec::new(),
        )
        .expect("prepare bound nonresident registry");
        assert_eq!(
            publication.publish(&coordinator, &root),
            RegistryPublicationOutcome::Published
        );

        let harness = RetirementFaultHarness {
            root,
            epoch,
            coordinator: Mutex::new(Some(Arc::clone(&coordinator))),
            decoded: Arc::new(OverlayNode::new().as_final()),
            loads: AtomicUsize::new(0),
        };
        let before = harness.root.load_revision().expect("bound root");
        let _failpoint = overlay_spine_failpoint::fail_next_spill();
        let error = harness
            .find_leaf_faulting(&harness.root, &key, 4)
            .expect_err("the first inline-spine spill must fail");

        assert!(matches!(
            error,
            PersistentARTrieError::AllocationFailed {
                operation,
                requested_entries,
                source: CollectionAllocationError::CapacityOverflow,
            } if operation == "overlay fault-in path" && requested_entries == key.len()
        ));
        assert_eq!(harness.loads.load(Ordering::Relaxed), 0);
        let after = harness.root.load_revision().expect("unchanged bound root");
        assert!(before.same_revision(&after));
        assert!(before
            .eviction_binding()
            .zip(after.eviction_binding())
            .is_some_and(|(before, after)| before.same_publication(after)));
        assert!(coordinator
            .prepare_byte_fault_commit(&after, &key, &disk_ptr)
            .is_some());
        let retained = harness
            .coordinator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(retained
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &coordinator)));
    }

    #[test]
    fn resident_fault_walk_is_stack_safe_across_inline_and_extreme_depths() {
        for depth in [16, 17, 64, 100_000] {
            let epoch = Arc::new(EpochManager::new());
            let harness = RetirementFaultHarness {
                root: AtomicNodePtr::new_with_term_count(resident_chain(depth), 1),
                epoch,
                coordinator: Mutex::new(None),
                decoded: Arc::new(OverlayNode::new().as_final()),
                loads: AtomicUsize::new(0),
            };
            let key = vec![b'x'; depth];
            let found = harness
                .find_leaf_faulting(&harness.root, &key, 0)
                .expect("resident read succeeds")
                .expect("resident final node exists");
            assert!(found.is_final());
            assert_eq!(harness.loads.load(Ordering::Relaxed), 0);
        }
    }
}

#[cfg(test)]
mod batch_tests {
    use super::{
        build_batch_ancestor_replacement_into, build_batch_plan, build_batch_replacement,
        build_chain_ancestor_replacement_into, build_chain_plan, build_chain_replacement,
        SuccessfulCandidateIndices,
    };
    use crate::persistent_artrie::core::eviction::{
        CompactEvictionBatch, CompactEvictionPolicy, DiskLocationRegistry, RegistryPathId,
    };
    use crate::persistent_artrie::core::key_encoding::ByteKey;
    use crate::persistent_artrie::core::overlay::node::{Child, OverlayNode};
    use crate::persistent_artrie::core::swizzled_ptr::{NodeType, SwizzledPtr};
    use crate::persistent_artrie::eviction::lru_tracker::LruRegistry;
    use std::sync::Arc;

    fn disk(offset: u32) -> SwizzledPtr {
        SwizzledPtr::on_disk(1, offset, NodeType::Node4)
    }

    fn stamped_leaf(ptr: &SwizzledPtr) -> Arc<OverlayNode<ByteKey, u64>> {
        let leaf = Arc::new(OverlayNode::new());
        leaf.set_durable_stamp(ptr.to_raw());
        leaf
    }

    fn selected_batch(entries: &[(&[u8], SwizzledPtr)]) -> CompactEvictionBatch<u8> {
        let mut registry = DiskLocationRegistry::with_capacity(entries.len());
        for (path, ptr) in entries {
            registry.register(path.to_vec(), ptr.clone(), 1, path.len(), NodeType::Node4);
        }
        registry
            .try_finalize_for_publication()
            .expect("finalize selected-batch registry");
        let batch = registry.select_compact_for_compatibility(
            usize::MAX,
            &LruRegistry::new(),
            0,
            usize::MAX,
            0,
        );
        assert_eq!(batch.candidates.len(), entries.len());
        batch
    }

    fn selected_chain_batch(entries: &[(u8, SwizzledPtr)]) -> CompactEvictionBatch<u8> {
        let mut registry = DiskLocationRegistry::with_capacity(entries.len());
        let mut path_id = RegistryPathId::ROOT;
        for (depth, (edge, pointer)) in entries.iter().enumerate() {
            path_id = registry
                .try_reserve_byte_path(path_id, &[*edge])
                .expect("reserve chain endpoint");
            registry
                .register_byte_path(path_id, pointer.clone(), 1, depth + 1, NodeType::Node4)
                .expect("register chain endpoint");
        }
        registry
            .try_finalize_for_publication()
            .expect("finalize chain registry");
        let batch = registry.select_compact_for_compatibility(
            usize::MAX,
            &LruRegistry::new(),
            0,
            usize::MAX,
            0,
        );
        assert_eq!(batch.candidates.len(), entries.len());
        batch
    }

    fn successful_raws(batch: &CompactEvictionBatch<u8>, successful: &[usize]) -> Vec<u64> {
        successful
            .iter()
            .map(|&index| batch.candidates[index].disk_ptr.to_raw())
            .collect()
    }

    #[test]
    fn detached_registry_uses_only_the_compatibility_selector() {
        let ptr = disk(5);
        let mut registry = DiskLocationRegistry::with_capacity(1);
        registry.register(b"a".to_vec(), ptr, 1, 1, NodeType::Node4);
        registry
            .try_finalize_for_publication()
            .expect("finalize detached registry topology");

        let authoritative =
            registry.select_compact_for_eviction(usize::MAX, &LruRegistry::new(), 0, usize::MAX, 0);
        let compatibility = registry.select_compact_for_compatibility(
            usize::MAX,
            &LruRegistry::new(),
            0,
            usize::MAX,
            0,
        );

        assert!(authoritative.candidates.is_empty());
        assert_eq!(compatibility.candidates.len(), 1);
    }

    #[test]
    fn batch_rebuilds_shared_root_once_for_siblings() {
        let a_ptr = disk(10);
        let b_ptr = disk(20);
        let a = stamped_leaf(&a_ptr);
        let b = stamped_leaf(&b_ptr);
        let root = Arc::new(
            OverlayNode::<ByteKey, u64>::new()
                .with_child(b'a', Child::InMem(a))
                .with_child(b'b', Child::InMem(b)),
        );
        let old_version = root.version();
        let batch = selected_batch(&[(b"a", a_ptr.clone()), (b"b", b_ptr.clone())]);
        let plan = build_batch_plan(&batch).expect("valid sibling plan");
        let (rebuilt, successful) =
            build_batch_replacement(&root, &plan, &batch).expect("sibling rebuild");

        assert_eq!(successful.len(), 2);
        assert_eq!(rebuilt.version(), old_version + 2);
        for (edge, expected) in [(b'a', a_ptr), (b'b', b_ptr)] {
            match rebuilt.find_child(edge).expect("retained child slot") {
                Child::OnDisk(actual) => assert_eq!(actual.to_raw(), expected.to_raw()),
                Child::InMem(_) => panic!("selected sibling remained resident"),
            }
        }
    }

    #[test]
    fn successful_descendant_suppresses_selected_ancestor() {
        let ancestor_ptr = disk(30);
        let descendant_ptr = disk(40);
        let descendant = stamped_leaf(&descendant_ptr);
        let ancestor =
            Arc::new(OverlayNode::<ByteKey, u64>::new().with_child(b'b', Child::InMem(descendant)));
        ancestor.set_durable_stamp(ancestor_ptr.to_raw());
        let root = Arc::new(
            OverlayNode::<ByteKey, u64>::new()
                .with_child(b'a', Child::InMem(Arc::clone(&ancestor))),
        );
        let batch = selected_batch(&[
            (b"a", ancestor_ptr.clone()),
            (b"ab", descendant_ptr.clone()),
        ]);
        let plan = build_batch_plan(&batch).expect("valid nested plan");
        let (rebuilt, successful) =
            build_batch_replacement(&root, &plan, &batch).expect("nested rebuild");

        assert_eq!(
            successful_raws(&batch, &successful),
            vec![descendant_ptr.to_raw()]
        );
        let Child::InMem(rebuilt_ancestor) = rebuilt.find_child(b'a').expect("ancestor slot")
        else {
            panic!("ancestor was incorrectly evicted after descendant publication");
        };
        assert_eq!(rebuilt_ancestor.durable_stamp(), 0);
        match rebuilt_ancestor.find_child(b'b').expect("descendant slot") {
            Child::OnDisk(actual) => assert_eq!(actual.to_raw(), descendant_ptr.to_raw()),
            Child::InMem(_) => panic!("valid descendant remained resident"),
        }
    }

    #[test]
    fn stale_descendant_allows_independently_durable_ancestor() {
        let ancestor_ptr = disk(50);
        let stale_descendant_ptr = disk(60);
        let stale_descendant = Arc::new(OverlayNode::<ByteKey, u64>::new());
        let ancestor = Arc::new(
            OverlayNode::<ByteKey, u64>::new().with_child(b'b', Child::InMem(stale_descendant)),
        );
        ancestor.set_durable_stamp(ancestor_ptr.to_raw());
        let root =
            Arc::new(OverlayNode::<ByteKey, u64>::new().with_child(b'a', Child::InMem(ancestor)));
        let batch = selected_batch(&[(b"a", ancestor_ptr.clone()), (b"ab", stale_descendant_ptr)]);
        let plan = build_batch_plan(&batch).expect("valid nested plan");
        let (rebuilt, successful) =
            build_batch_replacement(&root, &plan, &batch).expect("nested rebuild");

        assert_eq!(
            successful_raws(&batch, &successful),
            vec![ancestor_ptr.to_raw()]
        );
        match rebuilt.find_child(b'a').expect("ancestor slot") {
            Child::OnDisk(actual) => assert_eq!(actual.to_raw(), ancestor_ptr.to_raw()),
            Child::InMem(_) => panic!("durable ancestor remained resident"),
        }
    }

    #[test]
    fn chain_pda_matches_general_leaf_first_and_stale_fallback() {
        fn state(root: &OverlayNode<ByteKey, u64>) -> (u64, u64) {
            match root.find_child(b'a').expect("ancestor slot") {
                Child::OnDisk(pointer) => (pointer.to_raw(), 0),
                Child::InMem(ancestor) => match ancestor.find_child(b'b').expect("descendant slot")
                {
                    Child::OnDisk(pointer) => (0, pointer.to_raw()),
                    Child::InMem(_) => (0, 0),
                },
            }
        }

        for descendant_is_durable in [false, true] {
            let ancestor_ptr = disk(61);
            let descendant_ptr = disk(62);
            let descendant = if descendant_is_durable {
                stamped_leaf(&descendant_ptr)
            } else {
                Arc::new(OverlayNode::<ByteKey, u64>::new())
            };
            let ancestor = Arc::new(
                OverlayNode::<ByteKey, u64>::new().with_child(b'b', Child::InMem(descendant)),
            );
            ancestor.set_durable_stamp(ancestor_ptr.to_raw());
            let root = Arc::new(
                OverlayNode::<ByteKey, u64>::new().with_child(b'a', Child::InMem(ancestor)),
            );
            let batch = selected_chain_batch(&[
                (b'a', ancestor_ptr.clone()),
                (b'b', descendant_ptr.clone()),
            ]);

            let general_plan = build_batch_plan(&batch).expect("general chain plan");
            let (general_root, general_successful) =
                build_batch_replacement(&root, &general_plan, &batch)
                    .expect("general chain rebuild");
            let mut chain_plan =
                build_chain_plan::<ByteKey, u64>(&batch).expect("specialized chain plan");
            let (chain_root, chain_successful) =
                build_chain_replacement(&root, &mut chain_plan, &batch)
                    .expect("specialized chain rebuild");

            assert_eq!(
                successful_raws(&batch, &chain_successful),
                successful_raws(&batch, &general_successful)
            );
            assert_eq!(state(&chain_root), state(&general_root));
            let expected = if descendant_is_durable {
                (0, descendant_ptr.to_raw())
            } else {
                (ancestor_ptr.to_raw(), 0)
            };
            assert_eq!(state(&chain_root), expected);
        }
    }

    #[test]
    fn resident_chain_policy_prefers_exact_ancestor_and_falls_back_when_stale() {
        fn state(root: &OverlayNode<ByteKey, u64>) -> (u64, u64) {
            match root.find_child(b'a').expect("ancestor slot") {
                Child::OnDisk(pointer) => (pointer.to_raw(), 0),
                Child::InMem(ancestor) => match ancestor.find_child(b'b').expect("descendant slot")
                {
                    Child::OnDisk(pointer) => (0, pointer.to_raw()),
                    Child::InMem(_) => (0, 0),
                },
            }
        }

        for ancestor_is_exact in [true, false] {
            let ancestor_ptr = disk(63);
            let descendant_ptr = disk(64);
            let descendant = stamped_leaf(&descendant_ptr);
            let ancestor = Arc::new(
                OverlayNode::<ByteKey, u64>::new().with_child(b'b', Child::InMem(descendant)),
            );
            if ancestor_is_exact {
                ancestor.set_durable_stamp(ancestor_ptr.to_raw());
            }
            let root = Arc::new(
                OverlayNode::<ByteKey, u64>::new()
                    .with_child(b'a', Child::InMem(Arc::clone(&ancestor))),
            );
            let batch = selected_chain_batch(&[
                (b'a', ancestor_ptr.clone()),
                (b'b', descendant_ptr.clone()),
            ]);
            let mut plan = build_chain_plan::<ByteKey, u64>(&batch).expect("resident chain plan");
            let mut successful = SuccessfulCandidateIndices::new();
            successful
                .try_reserve(batch.candidates.len())
                .expect("resident success buffer");
            let rebuilt =
                build_chain_ancestor_replacement_into(&root, &mut plan, &batch, &mut successful)
                    .expect("resident chain replacement");

            let expected = if ancestor_is_exact {
                (ancestor_ptr.to_raw(), 0)
            } else {
                (0, descendant_ptr.to_raw())
            };
            assert_eq!(state(&rebuilt), expected);
            assert_eq!(successful.len(), 1);
            assert_eq!(
                batch.candidates[successful[0]].disk_ptr.to_raw(),
                if ancestor_is_exact {
                    ancestor_ptr.to_raw()
                } else {
                    descendant_ptr.to_raw()
                }
            );
        }
    }

    #[test]
    fn resident_branching_policy_prunes_valid_ancestor_subtrees() {
        let ancestor_ptr = disk(65);
        let descendant_ptr = disk(66);
        let sibling_ptr = disk(67);
        let descendant = stamped_leaf(&descendant_ptr);
        let ancestor =
            Arc::new(OverlayNode::<ByteKey, u64>::new().with_child(b'b', Child::InMem(descendant)));
        ancestor.set_durable_stamp(ancestor_ptr.to_raw());
        let sibling = stamped_leaf(&sibling_ptr);
        let root = Arc::new(
            OverlayNode::<ByteKey, u64>::new()
                .with_child(b'a', Child::InMem(ancestor))
                .with_child(b'x', Child::InMem(sibling)),
        );
        let batch = selected_batch(&[
            (b"a", ancestor_ptr.clone()),
            (b"ab", descendant_ptr),
            (b"x", sibling_ptr.clone()),
        ]);
        let plan = build_batch_plan(&batch).expect("resident branching plan");
        let mut successful = SuccessfulCandidateIndices::new();
        successful
            .try_reserve(batch.candidates.len())
            .expect("resident branching success buffer");
        let rebuilt = build_batch_ancestor_replacement_into(&root, &plan, &batch, &mut successful)
            .expect("resident branching replacement");

        assert_eq!(successful.len(), 2);
        match rebuilt.find_child(b'a').expect("ancestor slot") {
            Child::OnDisk(pointer) => assert_eq!(pointer.to_raw(), ancestor_ptr.to_raw()),
            Child::InMem(_) => panic!("valid selected ancestor remained resident"),
        }
        match rebuilt.find_child(b'x').expect("sibling slot") {
            Child::OnDisk(pointer) => assert_eq!(pointer.to_raw(), sibling_ptr.to_raw()),
            Child::InMem(_) => panic!("valid selected sibling remained resident"),
        }
    }

    #[test]
    fn missing_and_already_on_disk_branches_do_not_block_valid_sibling() {
        let valid_ptr = disk(70);
        let already_disk_ptr = disk(80);
        let replacement_ptr = disk(90);
        let missing_ptr = disk(100);
        let valid = stamped_leaf(&valid_ptr);
        let root = Arc::new(
            OverlayNode::<ByteKey, u64>::new()
                .with_child(b'a', Child::InMem(valid))
                .with_child(b'b', Child::OnDisk(already_disk_ptr.clone())),
        );
        let batch = selected_batch(&[
            (b"a", valid_ptr.clone()),
            (b"b", replacement_ptr),
            (b"c", missing_ptr),
        ]);
        let plan = build_batch_plan(&batch).expect("valid pruned plan");
        let (rebuilt, successful) =
            build_batch_replacement(&root, &plan, &batch).expect("pruned rebuild");

        assert_eq!(
            successful_raws(&batch, &successful),
            vec![valid_ptr.to_raw()]
        );
        match rebuilt.find_child(b'b').expect("existing on-disk sibling") {
            Child::OnDisk(actual) => assert_eq!(actual.to_raw(), already_disk_ptr.to_raw()),
            Child::InMem(_) => panic!("existing on-disk sibling was faulted"),
        }
    }

    #[test]
    fn candidate_order_does_not_change_leaf_first_batch_semantics() {
        let ancestor_ptr = disk(101);
        let descendant_ptr = disk(102);
        let sibling_ptr = disk(103);
        let descendant = stamped_leaf(&descendant_ptr);
        let ancestor =
            Arc::new(OverlayNode::<ByteKey, u64>::new().with_child(b'b', Child::InMem(descendant)));
        ancestor.set_durable_stamp(ancestor_ptr.to_raw());
        let sibling = stamped_leaf(&sibling_ptr);
        let root = Arc::new(
            OverlayNode::<ByteKey, u64>::new()
                .with_child(b'a', Child::InMem(ancestor))
                .with_child(b'c', Child::InMem(sibling)),
        );

        for permutation in [
            [0usize, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ] {
            let mut batch = selected_batch(&[
                (b"a", ancestor_ptr.clone()),
                (b"ab", descendant_ptr.clone()),
                (b"c", sibling_ptr.clone()),
            ]);
            let mut candidates = Vec::with_capacity(batch.candidates.len());
            for index in permutation {
                candidates.push(batch.candidates[index].clone());
            }
            batch.candidates = candidates.into_iter().collect();

            let plan = build_batch_plan(&batch).expect("permuted plan");
            let (rebuilt, successful) =
                build_batch_replacement(&root, &plan, &batch).expect("permuted rebuild");
            let mut raws = successful_raws(&batch, &successful);
            raws.sort_unstable();
            assert_eq!(raws, vec![descendant_ptr.to_raw(), sibling_ptr.to_raw()]);

            let Child::InMem(rebuilt_ancestor) = rebuilt.find_child(b'a').expect("ancestor slot")
            else {
                panic!("successful descendant must suppress its selected ancestor");
            };
            assert_eq!(rebuilt_ancestor.durable_stamp(), 0);
            match rebuilt_ancestor.find_child(b'b').expect("descendant slot") {
                Child::OnDisk(actual) => {
                    assert_eq!(actual.to_raw(), descendant_ptr.to_raw());
                }
                Child::InMem(_) => panic!("descendant remained resident"),
            }
            match rebuilt.find_child(b'c').expect("sibling slot") {
                Child::OnDisk(actual) => assert_eq!(actual.to_raw(), sibling_ptr.to_raw()),
                Child::InMem(_) => panic!("sibling remained resident"),
            }
        }
    }

    #[test]
    fn batch_rebuild_branching_spine_is_stack_safe() {
        const DEPTH: usize = 4_096;
        let terminal_ptr = disk(200);
        let mut registry = DiskLocationRegistry::with_capacity(DEPTH * 2);
        let mut continuation_path = RegistryPathId::ROOT;
        let mut side_ptrs = Vec::with_capacity(DEPTH);
        for level in 0..DEPTH {
            let side_ptr = disk(u32::try_from(level).expect("bounded level") + 1_000);
            let side_path = registry
                .try_reserve_byte_path(continuation_path, b"y")
                .expect("admit side path");
            registry
                .register_byte_path(side_path, side_ptr.clone(), 1, level + 1, NodeType::Node4)
                .expect("register side endpoint");
            side_ptrs.push(side_ptr);
            continuation_path = registry
                .try_reserve_byte_path(continuation_path, b"x")
                .expect("admit continuation path");
        }
        registry
            .register_byte_path(
                continuation_path,
                terminal_ptr.clone(),
                1,
                DEPTH,
                NodeType::Node4,
            )
            .expect("register terminal endpoint");
        registry
            .try_finalize_for_publication()
            .expect("finalize branching topology");
        let batch = registry.select_compact_for_compatibility(
            usize::MAX,
            &LruRegistry::new(),
            0,
            usize::MAX,
            0,
        );
        assert_eq!(batch.candidates.len(), DEPTH + 1);

        let terminal = stamped_leaf(&terminal_ptr);
        let mut subtree = terminal;
        for side_ptr in side_ptrs.iter().rev() {
            subtree = Arc::new(
                OverlayNode::<ByteKey, u64>::new()
                    .with_child(b'x', Child::InMem(subtree))
                    .with_child(b'y', Child::InMem(stamped_leaf(side_ptr))),
            );
        }

        let plan = build_batch_plan(&batch).expect("branching plan");
        assert!(build_chain_plan::<ByteKey, u64>(&batch).is_none());
        assert_eq!(plan.len(), DEPTH * 2 + 1);
        let (rebuilt, successful) =
            build_batch_replacement(&subtree, &plan, &batch).expect("branching rebuild");
        assert_eq!(successful.len(), DEPTH + 1);

        let mut current = rebuilt;
        for (level, side_ptr) in side_ptrs.iter().enumerate() {
            match current.find_child(b'y').expect("side slot") {
                Child::OnDisk(actual) => assert_eq!(actual.to_raw(), side_ptr.to_raw()),
                Child::InMem(_) => panic!("side endpoint remained resident"),
            }
            match current.find_child(b'x').expect("continuation slot") {
                Child::InMem(child) if level + 1 < DEPTH => current = Arc::clone(child),
                Child::OnDisk(actual) if level + 1 == DEPTH => {
                    assert_eq!(actual.to_raw(), terminal_ptr.to_raw());
                }
                Child::InMem(_) => panic!("terminal endpoint remained resident"),
                Child::OnDisk(_) => panic!("continuation ancestor was evicted"),
            }
        }
    }

    #[test]
    fn chain_plan_and_rebuild_are_stack_safe_at_one_hundred_thousand_depth() {
        const DEPTH: usize = 100_000;
        let ptr = disk(110);
        let mut registry = DiskLocationRegistry::with_capacity(DEPTH);
        let mut path_id = RegistryPathId::ROOT;
        for _ in 0..DEPTH {
            path_id = registry
                .try_reserve_byte_path(path_id, b"x")
                .expect("admit deep path segment");
        }
        registry
            .register_byte_path(path_id, ptr.clone(), 1, DEPTH, NodeType::Node4)
            .expect("register deep endpoint");
        registry
            .try_finalize_for_publication()
            .expect("finalize deep registry");
        let batch = registry.select_compact_for_compatibility(
            usize::MAX,
            &LruRegistry::new(),
            0,
            usize::MAX,
            0,
        );
        assert_eq!(batch.candidates.len(), 1);

        let victim = stamped_leaf(&ptr);
        let mut subtree = victim;
        for _ in 1..DEPTH {
            subtree = Arc::new(
                OverlayNode::<ByteKey, u64>::new().with_child(b'x', Child::InMem(subtree)),
            );
        }
        let root =
            Arc::new(OverlayNode::<ByteKey, u64>::new().with_child(b'x', Child::InMem(subtree)));

        let mut plan = build_chain_plan::<ByteKey, u64>(&batch).expect("deep chain plan");
        assert_eq!(plan.len(), DEPTH);
        let (rebuilt, successful) =
            build_chain_replacement(&root, &mut plan, &batch).expect("deep chain rebuild");
        assert_eq!(successful.len(), 1);

        let mut current = Arc::clone(&rebuilt);
        for depth in 0..DEPTH {
            match current.find_child(b'x').expect("deep child slot") {
                Child::InMem(child) if depth + 1 < DEPTH => current = Arc::clone(child),
                Child::OnDisk(actual) if depth + 1 == DEPTH => {
                    assert_eq!(actual.to_raw(), ptr.to_raw());
                }
                Child::InMem(_) => panic!("deep endpoint remained resident"),
                Child::OnDisk(_) => panic!("deep ancestor was evicted"),
            }
        }
    }

    #[test]
    fn resident_chain_stale_fallback_is_stack_safe_at_one_hundred_thousand_depth() {
        const DEPTH: usize = 100_000;
        const ANCHOR_STRIDE: usize = 25_000;

        let pointers = [disk(111), disk(112), disk(113), disk(114)];
        let mut registry = DiskLocationRegistry::with_capacity(DEPTH);
        let mut path_id = RegistryPathId::ROOT;
        for depth in 1..=DEPTH {
            path_id = registry
                .try_reserve_byte_path(path_id, b"x")
                .expect("admit deep resident path segment");
            if depth % ANCHOR_STRIDE == 0 {
                let pointer = pointers[depth / ANCHOR_STRIDE - 1].clone();
                registry
                    .register_byte_path(path_id, pointer, 1, depth, NodeType::Node4)
                    .expect("register deep resident anchor");
            }
        }
        registry
            .try_finalize_for_publication()
            .expect("finalize deep resident registry");
        let mut batch = registry.select_compact_for_compatibility(
            usize::MAX,
            &LruRegistry::new(),
            0,
            usize::MAX,
            0,
        );
        assert_eq!(batch.candidates.len(), pointers.len());
        batch.policy = CompactEvictionPolicy::ResidentBudgetAncestorClosure;

        // Only the deepest selected node has exact provenance. The ancestor-
        // first PDA must reject three stale anchors, reach the terminal anchor,
        // and rebuild the complete 100,000-level prefix without native-stack
        // recursion.
        let mut subtree = stamped_leaf(pointers.last().expect("terminal pointer"));
        for _ in 1..DEPTH {
            subtree = Arc::new(
                OverlayNode::<ByteKey, u64>::new().with_child(b'x', Child::InMem(subtree)),
            );
        }
        let root =
            Arc::new(OverlayNode::<ByteKey, u64>::new().with_child(b'x', Child::InMem(subtree)));

        let mut plan = build_chain_plan::<ByteKey, u64>(&batch).expect("deep resident chain plan");
        assert_eq!(plan.len(), DEPTH);
        let mut successful = SuccessfulCandidateIndices::new();
        successful
            .try_reserve(batch.candidates.len())
            .expect("reserve deep resident success buffer");
        let rebuilt =
            build_chain_ancestor_replacement_into(&root, &mut plan, &batch, &mut successful)
                .expect("deep resident stale fallback rebuild");
        assert_eq!(successful.len(), 1);
        assert_eq!(
            batch.candidates[successful[0]].disk_ptr.to_raw(),
            pointers.last().expect("terminal pointer").to_raw()
        );

        let mut current = Arc::clone(&rebuilt);
        for depth in 0..DEPTH {
            match current.find_child(b'x').expect("deep resident child slot") {
                Child::InMem(child) if depth + 1 < DEPTH => current = Arc::clone(child),
                Child::OnDisk(actual) if depth + 1 == DEPTH => {
                    assert_eq!(actual.to_raw(), pointers.last().unwrap().to_raw());
                }
                Child::InMem(_) => panic!("deep resident endpoint remained resident"),
                Child::OnDisk(_) => panic!("stale resident ancestor was evicted"),
            }
        }
    }
}
