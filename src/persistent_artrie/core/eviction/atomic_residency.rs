//! Lock-free, generation-local residency materialization.
//!
//! A published root revision is the logical authority. Each affected 32-bit
//! residency payload is paired with the root's generation-local ordinal in one
//! `AtomicU64`, so a delayed helper cannot mistake an inverse successor for its
//! retained predecessor. The separately padded frontier advances only after
//! all exact word transitions have materialized.
//!
//! Residency words deliberately remain contiguous. Padding every word would
//! turn the exact two-bit-per-record representation into roughly sixteen bits
//! per record and damage scan locality. Only the single, globally contended
//! frontier occupies an isolated cache line.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

const PAYLOAD_BITS: u32 = u32::BITS;
const PAYLOAD_MASK: u64 = u32::MAX as u64;
pub(super) const RESIDENCY_PATHS_PER_WORD: usize = u32::BITS as usize;

#[inline(always)]
const fn pack_cell(ordinal: u32, payload: u32) -> u64 {
    ((ordinal as u64) << PAYLOAD_BITS) | payload as u64
}

#[inline(always)]
const fn cell_ordinal(cell: u64) -> u32 {
    (cell >> PAYLOAD_BITS) as u32
}

#[inline(always)]
const fn cell_payload(cell: u64) -> u32 {
    (cell & PAYLOAD_MASK) as u32
}

#[repr(align(64))]
struct ResidencyFrontier(AtomicU32);

/// One exact predecessor-to-target transition for a packed residency word.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PackedResidencyTransition {
    pub(super) word: usize,
    pub(super) expected: u64,
    pub(super) target: u64,
}

/// Sparse root-carried residency change. A point fault remains allocation-free
/// beyond the root allocation it already requires; batch eviction reuses its
/// preallocated word-transition vector.
#[derive(Debug)]
pub(crate) enum PackedResidencyDelta {
    None,
    One(PackedResidencyTransition),
    Many(Vec<PackedResidencyTransition>),
}

impl PackedResidencyDelta {
    #[inline(always)]
    fn transitions(&self) -> &[PackedResidencyTransition] {
        match self {
            Self::None => &[],
            Self::One(transition) => std::slice::from_ref(transition),
            Self::Many(transitions) => transitions,
        }
    }

    /// Enumerate exact path indices cleared by this sparse delta without
    /// allocation. Transition words are unique, so every path is yielded at
    /// most once.
    pub(crate) fn for_each_cleared_path(&self, mut visit: impl FnMut(usize)) {
        for transition in self.transitions() {
            let mut cleared = cell_payload(transition.expected) & !cell_payload(transition.target);
            while cleared != 0 {
                let bit = cleared.trailing_zeros() as usize;
                visit(transition.word * RESIDENCY_PATHS_PER_WORD + bit);
                cleared &= cleared - 1;
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResidencyHelpOutcome {
    Complete,
    Stale,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum ResidencyPrepareError {
    PathOutOfRange,
    FrontierMismatch,
    OrdinalExhausted,
    Allocation,
}

/// One retained materialization array. Generation rollover allocates a fresh
/// instance before ordinal reuse, so old helpers can mutate only the old array
/// retained by their `Arc`.
pub(crate) struct AtomicResidencyGeneration {
    words: Box<[AtomicU64]>,
    frontier: ResidencyFrontier,
}

/// One frontier-qualified view of a retained residency generation.
///
/// Qualification performs the single frontier acquire required before an
/// exact preparation. Individual word loads still reject a tag newer than the
/// captured root because another publisher may advance after qualification;
/// the eventual exact-root CAS then provides the publication-time fence.
pub(super) struct StableResidencyView<'a> {
    generation: &'a AtomicResidencyGeneration,
    ordinal: u32,
}

/// One already-validated exact successor of a stable residency view.
///
/// Validating `target == predecessor + 1` once keeps the multiword eviction
/// scan free of a redundant ordinal branch for every covered word.
pub(super) struct StableResidencyTransitionView<'a> {
    stable: StableResidencyView<'a>,
    target_ordinal: u32,
}

impl AtomicResidencyGeneration {
    /// Convert the legacy 64-payload-bit builder representation into exact
    /// 32-payload/32-ordinal cells. This runs once on the cold checkpoint path.
    pub(super) fn try_from_builder_words(
        builder_words: &[u64],
        path_count: usize,
        initial_ordinal: u32,
    ) -> Result<Self, ResidencyPrepareError> {
        let word_count = path_count.div_ceil(RESIDENCY_PATHS_PER_WORD);
        if word_count > builder_words.len().saturating_mul(2) {
            return Err(ResidencyPrepareError::PathOutOfRange);
        }
        let mut words = Vec::new();
        words
            .try_reserve_exact(word_count)
            .map_err(|_| ResidencyPrepareError::Allocation)?;
        for cell_index in 0..word_count {
            let builder_word = builder_words[cell_index / 2];
            let payload = if cell_index & 1 == 0 {
                builder_word as u32
            } else {
                (builder_word >> u32::BITS) as u32
            };
            words.push(AtomicU64::new(pack_cell(initial_ordinal, payload)));
        }
        Ok(Self {
            words: words.into_boxed_slice(),
            frontier: ResidencyFrontier(AtomicU32::new(initial_ordinal)),
        })
    }

    #[inline(always)]
    pub(crate) fn frontier(&self) -> u32 {
        self.frontier.0.load(Ordering::Acquire)
    }

    #[inline(always)]
    pub(super) fn next_ordinal(current: u32) -> Result<u32, ResidencyPrepareError> {
        current
            .checked_add(1)
            .ok_or(ResidencyPrepareError::OrdinalExhausted)
    }

    #[inline(always)]
    pub(super) fn try_stable(
        &self,
        expected_ordinal: u32,
    ) -> Result<StableResidencyView<'_>, ResidencyPrepareError> {
        if self.frontier() != expected_ordinal {
            return Err(ResidencyPrepareError::FrontierMismatch);
        }
        Ok(StableResidencyView {
            generation: self,
            ordinal: expected_ordinal,
        })
    }

    #[inline(always)]
    fn path_word_and_mask(path_index: usize) -> (usize, u32) {
        let word = path_index / RESIDENCY_PATHS_PER_WORD;
        let bit = path_index % RESIDENCY_PATHS_PER_WORD;
        (word, 1u32 << bit)
    }

    /// Materialize a root-published sparse delta. Duplicate helpers are
    /// harmless; any word that is neither the exact predecessor nor the exact
    /// target proves that this descriptor is stale and is never overwritten.
    pub(crate) fn help(
        &self,
        predecessor_ordinal: u32,
        target_ordinal: u32,
        delta: &PackedResidencyDelta,
    ) -> ResidencyHelpOutcome {
        let frontier = self.frontier();
        if frontier == target_ordinal {
            return ResidencyHelpOutcome::Complete;
        }
        if frontier != predecessor_ordinal {
            return ResidencyHelpOutcome::Stale;
        }
        for transition in delta.transitions() {
            let Some(word) = self.words.get(transition.word) else {
                return ResidencyHelpOutcome::Stale;
            };
            match word.compare_exchange(
                transition.expected,
                transition.target,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {}
                Err(observed) if observed == transition.target => {}
                Err(_) => return ResidencyHelpOutcome::Stale,
            }
        }
        match self.frontier.0.compare_exchange(
            predecessor_ordinal,
            target_ordinal,
            Ordering::Release,
            Ordering::Acquire,
        ) {
            Ok(_) => ResidencyHelpOutcome::Complete,
            Err(observed) if observed == target_ordinal => ResidencyHelpOutcome::Complete,
            Err(_) => ResidencyHelpOutcome::Stale,
        }
    }

    pub(super) fn try_snapshot_payloads(
        &self,
        expected_ordinal: u32,
        destination: &mut Vec<u64>,
    ) -> Result<(), ResidencyPrepareError> {
        let stable = self.try_stable(expected_ordinal)?;
        let legacy_word_count = self.words.len().div_ceil(2);
        destination.clear();
        if destination.capacity() < legacy_word_count {
            destination
                .try_reserve_exact(legacy_word_count)
                .map_err(|_| ResidencyPrepareError::Allocation)?;
        }
        let mut pairs = self.words.chunks_exact(2);
        for pair in &mut pairs {
            let low = stable.validate_cell(pair[0].load(Ordering::Acquire))?;
            let high = stable.validate_cell(pair[1].load(Ordering::Acquire))?;
            destination.push(cell_payload(low) as u64 | ((cell_payload(high) as u64) << u32::BITS));
        }
        if let [low] = pairs.remainder() {
            destination
                .push(cell_payload(stable.validate_cell(low.load(Ordering::Acquire))?) as u64);
        }
        if self.frontier() != expected_ordinal {
            return Err(ResidencyPrepareError::FrontierMismatch);
        }
        Ok(())
    }

    /// Build a disjoint, zero-tagged generation from one stable predecessor.
    ///
    /// Construction is iterative and uses only the destination array. The
    /// sparse delta is already sorted and unique by word; matching it while the
    /// predecessor is copied avoids a second full snapshot allocation. Nothing
    /// becomes reachable until the enclosing root CAS publishes the returned
    /// generation.
    #[cold]
    #[inline(never)]
    pub(super) fn try_rebased(
        &self,
        expected_ordinal: u32,
        delta: &PackedResidencyDelta,
    ) -> Result<Self, ResidencyPrepareError> {
        let stable = self.try_stable(expected_ordinal)?;
        let transitions = delta.transitions();
        let mut transition_index = 0usize;
        let mut words = Vec::new();
        words
            .try_reserve_exact(self.words.len())
            .map_err(|_| ResidencyPrepareError::Allocation)?;

        for (word_index, word) in self.words.iter().enumerate() {
            let observed = stable.validate_cell(word.load(Ordering::Acquire))?;
            let payload = match transitions.get(transition_index) {
                Some(transition) if transition.word == word_index => {
                    if transition.expected != observed || cell_ordinal(transition.target) != 0 {
                        return Err(ResidencyPrepareError::FrontierMismatch);
                    }
                    transition_index += 1;
                    cell_payload(transition.target)
                }
                Some(transition) if transition.word < word_index => {
                    return Err(ResidencyPrepareError::FrontierMismatch);
                }
                _ => cell_payload(observed),
            };
            words.push(AtomicU64::new(pack_cell(0, payload)));
        }
        if transition_index != transitions.len() || self.frontier() != expected_ordinal {
            return Err(ResidencyPrepareError::FrontierMismatch);
        }
        Ok(Self {
            words: words.into_boxed_slice(),
            frontier: ResidencyFrontier(AtomicU32::new(0)),
        })
    }

    #[cfg(test)]
    fn packed_word(&self, word: usize) -> u64 {
        self.words[word].load(Ordering::Acquire)
    }
}

impl<'a> StableResidencyView<'a> {
    #[inline(always)]
    fn validate_cell(&self, observed: u64) -> Result<u64, ResidencyPrepareError> {
        if cell_ordinal(observed) > self.ordinal {
            return Err(ResidencyPrepareError::FrontierMismatch);
        }
        Ok(observed)
    }

    #[inline(always)]
    fn load_predecessor_word(&self, word: usize) -> Result<u64, ResidencyPrepareError> {
        let observed = self
            .generation
            .words
            .get(word)
            .ok_or(ResidencyPrepareError::PathOutOfRange)?
            .load(Ordering::Acquire);
        self.validate_cell(observed)
    }

    #[inline(always)]
    pub(super) fn try_successor(
        self,
        target_ordinal: u32,
    ) -> Result<StableResidencyTransitionView<'a>, ResidencyPrepareError> {
        if AtomicResidencyGeneration::next_ordinal(self.ordinal)? != target_ordinal {
            return Err(ResidencyPrepareError::FrontierMismatch);
        }
        Ok(StableResidencyTransitionView {
            stable: self,
            target_ordinal,
        })
    }

    #[cold]
    #[inline(never)]
    pub(super) fn try_fresh_generation(
        self,
    ) -> Result<StableResidencyTransitionView<'a>, ResidencyPrepareError> {
        if self.ordinal != u32::MAX {
            return Err(ResidencyPrepareError::FrontierMismatch);
        }
        Ok(StableResidencyTransitionView {
            stable: self,
            target_ordinal: 0,
        })
    }
}

impl StableResidencyTransitionView<'_> {
    #[inline]
    pub(super) fn prepare_mark(
        &self,
        path_index: usize,
    ) -> Result<Option<PackedResidencyTransition>, ResidencyPrepareError> {
        let (word, mask) = AtomicResidencyGeneration::path_word_and_mask(path_index);
        let expected = self.stable.load_predecessor_word(word)?;
        let payload = cell_payload(expected);
        if payload & mask != 0 {
            return Ok(None);
        }
        Ok(Some(PackedResidencyTransition {
            word,
            expected,
            target: pack_cell(self.target_ordinal, payload | mask),
        }))
    }

    /// Intersect one topology-derived coverage mask with one exact atomic
    /// predecessor load. The returned mask is precisely the resident records
    /// removed by the transition, so callers can enumerate and total them
    /// without a second atomic read.
    #[inline]
    pub(super) fn prepare_clear_covered(
        &self,
        word: usize,
        coverage_mask: u32,
    ) -> Result<Option<(PackedResidencyTransition, u32)>, ResidencyPrepareError> {
        if coverage_mask == 0 {
            return Err(ResidencyPrepareError::FrontierMismatch);
        }
        let expected = self.stable.load_predecessor_word(word)?;
        let payload = cell_payload(expected);
        let resident_mask = payload & coverage_mask;
        if resident_mask == 0 {
            return Ok(None);
        }
        Ok(Some((
            PackedResidencyTransition {
                word,
                expected,
                target: pack_cell(self.target_ordinal, payload & !resident_mask),
            },
            resident_mask,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_cell_roundtrips_all_boundary_fields() {
        for ordinal in [0, 1, u32::MAX - 1, u32::MAX] {
            for payload in [0, 1, u32::MAX - 1, u32::MAX] {
                let packed = pack_cell(ordinal, payload);
                assert_eq!(cell_ordinal(packed), ordinal);
                assert_eq!(cell_payload(packed), payload);
            }
        }
    }

    #[test]
    fn point_mark_is_one_word_cas_plus_one_frontier_cas() {
        let generation = AtomicResidencyGeneration::try_from_builder_words(&[0], 64, 0)
            .expect("allocate residency generation");
        let transition = generation
            .try_stable(0)
            .expect("stable initial generation")
            .try_successor(1)
            .expect("exact successor")
            .prepare_mark(63)
            .expect("prepare point mark")
            .expect("path begins nonresident");
        assert_eq!(transition.word, 1);
        assert_eq!(
            generation.help(0, 1, &PackedResidencyDelta::One(transition)),
            ResidencyHelpOutcome::Complete
        );
        assert_eq!(cell_payload(generation.packed_word(1)), 1u32 << 31);
        assert_eq!(generation.frontier(), 1);
    }

    #[test]
    fn delayed_helper_cannot_resurrect_after_inverse_successor() {
        let generation = AtomicResidencyGeneration::try_from_builder_words(&[0], 64, 0)
            .expect("allocate residency generation");
        let delayed = generation
            .try_stable(0)
            .expect("stable initial generation")
            .try_successor(1)
            .expect("exact successor")
            .prepare_mark(0)
            .expect("prepare first mark")
            .expect("first mark changes residency");
        assert_eq!(
            generation.help(0, 1, &PackedResidencyDelta::One(delayed)),
            ResidencyHelpOutcome::Complete
        );
        let inverse = generation
            .try_stable(1)
            .expect("stable first successor")
            .try_successor(2)
            .expect("exact inverse successor")
            .prepare_clear_covered(0, 1)
            .expect("prepare inverse clear")
            .expect("inverse clear changes residency")
            .0;
        assert_eq!(
            generation.help(1, 2, &PackedResidencyDelta::One(inverse)),
            ResidencyHelpOutcome::Complete
        );
        assert_eq!(
            generation.help(0, 1, &PackedResidencyDelta::One(delayed)),
            ResidencyHelpOutcome::Stale
        );
        assert_eq!(cell_payload(generation.packed_word(0)), 0);
        assert_eq!(cell_ordinal(generation.packed_word(0)), 2);
    }

    #[test]
    fn payload_snapshot_roundtrips_builder_layout_without_depth_limit() {
        let builder_words = vec![u64::MAX, 0x0123_4567_89ab_cdef, 1];
        let generation = AtomicResidencyGeneration::try_from_builder_words(
            &builder_words,
            builder_words.len() * u64::BITS as usize,
            17,
        )
        .expect("allocate residency generation");
        let mut snapshot = Vec::new();
        generation
            .try_snapshot_payloads(17, &mut snapshot)
            .expect("capture coherent snapshot");
        assert_eq!(snapshot, builder_words);
    }

    #[test]
    fn sparse_successor_accepts_an_untouched_word_with_an_older_tag() {
        let generation = AtomicResidencyGeneration::try_from_builder_words(&[0], 64, 0)
            .expect("allocate residency generation");
        let first = generation
            .try_stable(0)
            .expect("stable initial generation")
            .try_successor(1)
            .expect("exact successor")
            .prepare_mark(0)
            .expect("prepare first word")
            .expect("first word starts clear");
        assert_eq!(
            generation.help(0, 1, &PackedResidencyDelta::One(first)),
            ResidencyHelpOutcome::Complete
        );
        assert_eq!(cell_ordinal(generation.packed_word(1)), 0);

        let second = generation
            .try_stable(1)
            .expect("stable first successor")
            .try_successor(2)
            .expect("exact sparse successor")
            .prepare_mark(32)
            .expect("untouched word remains a valid exact predecessor")
            .expect("second word starts clear");
        assert_eq!(
            generation.help(1, 2, &PackedResidencyDelta::One(second)),
            ResidencyHelpOutcome::Complete
        );

        let mut snapshot = Vec::new();
        generation
            .try_snapshot_payloads(2, &mut snapshot)
            .expect("sparse tags admit a coherent snapshot");
        assert_eq!(snapshot, vec![1 | (1u64 << 32)]);
    }

    #[test]
    fn ordinal_exhaustion_requests_rollover_instead_of_wrapping() {
        assert_eq!(
            AtomicResidencyGeneration::next_ordinal(u32::MAX),
            Err(ResidencyPrepareError::OrdinalExhausted)
        );
    }

    #[test]
    fn fresh_generation_applies_multiword_delta_and_preserves_odd_tail() {
        let predecessor = AtomicResidencyGeneration::try_from_builder_words(
            &[u32::MAX as u64 | (1u64 << u32::BITS)],
            RESIDENCY_PATHS_PER_WORD + 1,
            u32::MAX,
        )
        .expect("allocate maximum-ordinal predecessor");
        let stable = predecessor
            .try_stable(u32::MAX)
            .expect("stable maximum-ordinal predecessor")
            .try_fresh_generation()
            .expect("prepare fresh generation");
        let first = stable
            .prepare_clear_covered(0, 0b11)
            .expect("prepare first word")
            .expect("first word contains resident paths")
            .0;
        let tail = stable
            .prepare_clear_covered(1, 1)
            .expect("prepare odd tail word")
            .expect("tail path is resident")
            .0;
        let delta = PackedResidencyDelta::Many(vec![first, tail]);

        let fresh = predecessor
            .try_rebased(u32::MAX, &delta)
            .expect("materialize fresh generation");
        assert_eq!(fresh.frontier(), 0);
        assert_eq!(cell_ordinal(fresh.packed_word(0)), 0);
        assert_eq!(cell_payload(fresh.packed_word(0)), !0b11u32);
        assert_eq!(cell_ordinal(fresh.packed_word(1)), 0);
        assert_eq!(cell_payload(fresh.packed_word(1)), 0);

        let successor = fresh
            .try_stable(0)
            .expect("fresh generation is immediately stable")
            .try_successor(1)
            .expect("fresh generation admits an ordinary successor")
            .prepare_mark(RESIDENCY_PATHS_PER_WORD)
            .expect("prepare tail mark")
            .expect("tail was cleared by rollover");
        assert_eq!(
            fresh.help(0, 1, &PackedResidencyDelta::One(successor)),
            ResidencyHelpOutcome::Complete
        );
        assert_eq!(fresh.frontier(), 1);
        assert_eq!(cell_payload(fresh.packed_word(1)), 1);
    }

    #[test]
    fn delayed_old_helper_cannot_address_a_fresh_generation() {
        let predecessor = AtomicResidencyGeneration::try_from_builder_words(
            &[0],
            RESIDENCY_PATHS_PER_WORD,
            u32::MAX - 1,
        )
        .expect("allocate predecessor");
        let delayed = predecessor
            .try_stable(u32::MAX - 1)
            .expect("stable predecessor")
            .try_successor(u32::MAX)
            .expect("last sparse successor")
            .prepare_mark(0)
            .expect("prepare delayed mark")
            .expect("path starts clear");
        let delayed_delta = PackedResidencyDelta::One(delayed);
        assert_eq!(
            predecessor.help(u32::MAX - 1, u32::MAX, &delayed_delta),
            ResidencyHelpOutcome::Complete
        );

        let rollover = predecessor
            .try_stable(u32::MAX)
            .expect("stable maximum ordinal")
            .try_fresh_generation()
            .expect("prepare rollover")
            .prepare_clear_covered(0, 1)
            .expect("prepare inverse clear")
            .expect("path is resident")
            .0;
        let fresh = predecessor
            .try_rebased(u32::MAX, &PackedResidencyDelta::One(rollover))
            .expect("materialize fresh generation");
        assert_eq!(cell_payload(fresh.packed_word(0)), 0);

        assert_eq!(
            predecessor.help(u32::MAX - 1, u32::MAX, &delayed_delta),
            ResidencyHelpOutcome::Complete
        );
        assert_eq!(
            cell_payload(fresh.packed_word(0)),
            0,
            "a helper retaining the old array cannot mutate the fresh array"
        );
    }
}
