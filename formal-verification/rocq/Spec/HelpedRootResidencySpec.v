(** * Root-linearized, revision-fenced residency materialization

    The persistent ARTrie root is the sole logical authority for both trie
    topology and residency.  A fault or eviction first prepares a descriptor,
    then publishes the logical successor with the existing exact-root CAS.
    The descriptor's mutable materialization is helped afterwards.

    A residency cell contains payload bits and the exact revision that last
    wrote them.  Helpers use a compare-and-swap from the descriptor's retained
    predecessor cell to its target cell.  The revision tag is essential: plain
    OR/AND updates are individually idempotent but are not safe after an inverse
    successor.  A delayed old helper must miss once any newer helper changed the
    tag.  The lemmas below establish CAS hit/miss behavior, duplicate-helper
    idempotence, inverse-successor exclusion, exact frontier qualification,
    semantic detachment, scan revalidation, and the retirement-fence property.

    The proof is independent of a numeric machine representation.  Production
    may use a proven lock-free wide atomic or a generation-local packed word;
    in either case, equality denotes the complete tagged cell and revision
    identities are not reused while an old descriptor remains reachable.
*)

From Stdlib Require Import Arith Bool Lia List PeanoNat ZArith.
Import ListNotations.

Module HelpedRootResidencySpec.

Record tagged_cell : Type := mkTaggedCell {
  cell_bits : nat;
  cell_revision : nat
}.

Definition tagged_cell_eq_dec :
  forall left right : tagged_cell, {left = right} + {left <> right}.
Proof.
  decide equality; apply Nat.eq_dec.
Defined.

(** One exact predecessor-cell CAS.  A miss returns the observed cell without
    mutation, matching the production compare-exchange contract. *)
Definition tagged_cas
    (expected target observed : tagged_cell) : tagged_cell :=
  if tagged_cell_eq_dec observed expected then target else observed.

Lemma tagged_cas_hit :
  forall expected target,
    tagged_cas expected target expected = target.
Proof.
  intros expected target.
  unfold tagged_cas.
  destruct (tagged_cell_eq_dec expected expected) as [_ | Hneq].
  - reflexivity.
  - contradiction.
Qed.

Lemma tagged_cas_miss :
  forall expected target observed,
    observed <> expected ->
    tagged_cas expected target observed = observed.
Proof.
  intros expected target observed Hneq.
  unfold tagged_cas.
  destruct (tagged_cell_eq_dec observed expected) as [Heq | _].
  - contradiction.
  - reflexivity.
Qed.

Theorem duplicate_helper_idempotent :
  forall expected target observed,
    tagged_cas expected target (tagged_cas expected target observed) =
    tagged_cas expected target observed.
Proof.
  intros expected target observed.
  destruct (tagged_cell_eq_dec observed expected) as [Heq | Hneq].
  - subst observed. rewrite tagged_cas_hit.
    destruct (tagged_cell_eq_dec target expected) as [Heq | Hneq].
    + subst target. rewrite tagged_cas_hit. reflexivity.
    + rewrite tagged_cas_miss by exact Hneq. reflexivity.
  - pose proof (tagged_cas_miss expected target observed Hneq) as Hmiss.
    rewrite Hmiss. exact Hmiss.
Qed.

(** This is the decisive delayed-helper theorem.  [old_target] may be the
    inverse of [successor] at the payload level; the proof needs only the full
    tagged successor to differ from the old expected predecessor. *)
Theorem delayed_helper_cannot_overwrite_successor :
  forall old_expected old_target successor,
    successor <> old_expected ->
    tagged_cas old_expected old_target successor = successor.
Proof.
  apply tagged_cas_miss.
Qed.

Corollary newer_revision_rejects_delayed_helper :
  forall old_bits old_revision old_target_bits old_target_revision
         successor_bits successor_revision,
    successor_revision <> old_revision ->
    tagged_cas
      (mkTaggedCell old_bits old_revision)
      (mkTaggedCell old_target_bits old_target_revision)
      (mkTaggedCell successor_bits successor_revision) =
    mkTaggedCell successor_bits successor_revision.
Proof.
  intros old_bits old_revision old_target_bits old_target_revision
         successor_bits successor_revision Hrevision.
  apply tagged_cas_miss.
  intro Heq. injection Heq as _ HeqRevision. contradiction.
Qed.

Record residency_descriptor : Type := mkResidencyDescriptor {
  descriptor_predecessor : nat;
  descriptor_revision : nat;
  descriptor_expected : tagged_cell;
  descriptor_target : tagged_cell
}.

Definition help_descriptor
    (descriptor : residency_descriptor) (observed : tagged_cell) : tagged_cell :=
  tagged_cas
    (descriptor_expected descriptor)
    (descriptor_target descriptor)
    observed.

(** Numeric predecessor/target revisions do not identify a winner: two
    conflicting candidates prepared from the same root share both numbers.
    Materialization is therefore qualified by the immutable candidate-root
    identity produced by the exact root CAS. *)
Definition winner_qualified_help
    (published_candidate candidate : nat)
    (descriptor : residency_descriptor) (observed : tagged_cell)
    : tagged_cell :=
  if Nat.eq_dec candidate published_candidate
  then help_descriptor descriptor observed
  else observed.

Theorem losing_candidate_cannot_materialize :
  forall published_candidate losing_candidate descriptor observed,
    losing_candidate <> published_candidate ->
    winner_qualified_help
      published_candidate losing_candidate descriptor observed = observed.
Proof.
  intros published_candidate losing_candidate descriptor observed Hloser.
  unfold winner_qualified_help.
  destruct (Nat.eq_dec losing_candidate published_candidate) as [Hequal | _].
  - contradiction.
  - reflexivity.
Qed.

Theorem published_candidate_retains_exact_cas_semantics :
  forall published_candidate descriptor observed,
    winner_qualified_help
      published_candidate published_candidate descriptor observed =
    help_descriptor descriptor observed.
Proof.
  intros published_candidate descriptor observed.
  unfold winner_qualified_help.
  destruct (Nat.eq_dec published_candidate published_candidate) as [_ | Hneq].
  - reflexivity.
  - contradiction.
Qed.

Definition descriptor_materialized
    (descriptor : residency_descriptor) (observed : tagged_cell) : Prop :=
  observed = descriptor_target descriptor.

Definition frontier_advance_allowed
    (descriptor : residency_descriptor) (observed : tagged_cell) : Prop :=
  descriptor_materialized descriptor observed.

Theorem helped_descriptor_allows_frontier :
  forall descriptor,
    frontier_advance_allowed descriptor
      (help_descriptor descriptor (descriptor_expected descriptor)).
Proof.
  intros descriptor.
  unfold frontier_advance_allowed, descriptor_materialized, help_descriptor.
  rewrite tagged_cas_hit. reflexivity.
Qed.

Theorem frontier_requires_exact_target :
  forall descriptor observed,
    frontier_advance_allowed descriptor observed ->
    observed = descriptor_target descriptor.
Proof.
  intros descriptor observed Hallowed.
  exact Hallowed.
Qed.

Record root_state : Type := mkRootState {
  root_revision : nat;
  root_generation : option nat;
  root_owner : option nat;
  root_resident_count : nat
}.

Definition exact_root_revision
    (root : root_state) (expected_revision : nat) : Prop :=
  root_revision root = expected_revision.

Definition publish_residency_successor
    (root : root_state) (revision resident_count : nat) : root_state :=
  mkRootState revision (root_generation root) (root_owner root) resident_count.

Definition publish_semantic_successor
    (root : root_state) (revision resident_count : nat) : root_state :=
  mkRootState revision None None resident_count.

Definition publish_retirement_fence
    (root : root_state) (revision : nat) : root_state :=
  mkRootState revision None None (root_resident_count root).

Definition root_cas
    (expected_revision : nat) (target observed : root_state) : root_state :=
  if Nat.eq_dec (root_revision observed) expected_revision
  then target
  else observed.

Lemma root_cas_hit :
  forall observed target,
    root_cas (root_revision observed) target observed = target.
Proof.
  intros observed target.
  unfold root_cas.
  destruct (Nat.eq_dec (root_revision observed) (root_revision observed));
    [reflexivity | contradiction].
Qed.

Lemma root_cas_miss :
  forall expected target observed,
    root_revision observed <> expected ->
    root_cas expected target observed = observed.
Proof.
  intros expected target observed Hneq.
  unfold root_cas.
  destruct (Nat.eq_dec (root_revision observed) expected);
    [contradiction | reflexivity].
Qed.

Theorem semantic_publication_clears_eviction_authority :
  forall root revision resident_count,
    root_generation (publish_semantic_successor root revision resident_count) = None /\
    root_owner (publish_semantic_successor root revision resident_count) = None.
Proof.
  intros. split; reflexivity.
Qed.

(** Retirement always advances the root, even when it was already unbound.
    Therefore a publisher prepared against the pre-fence exact revision cannot
    succeed after the fence. *)
Theorem retirement_fence_rejects_paused_publisher :
  forall root fence_revision candidate,
    fence_revision <> root_revision root ->
    root_cas (root_revision root) candidate
      (publish_retirement_fence root fence_revision) =
    publish_retirement_fence root fence_revision.
Proof.
  intros root fence_revision candidate Hfresh.
  apply root_cas_miss. simpl. exact Hfresh.
Qed.

Definition checked_count_delta
    (count : nat) (delta : Z) : option nat :=
  match delta with
  | Z0 => Some count
  | Zpos positive_delta => Some (count + Pos.to_nat positive_delta)
  | Zneg positive_delta =>
      let amount := Pos.to_nat positive_delta in
      if amount <=? count then Some (count - amount) else None
  end.

Theorem rejected_count_underflow_preserves_count :
  forall count amount,
    count < Pos.to_nat amount ->
    checked_count_delta count (Zneg amount) = None.
Proof.
  intros count amount Hlt.
  cbn.
  destruct (Pos.to_nat amount <=? count) eqn:Hcomparison.
  - apply Nat.leb_le in Hcomparison. lia.
  - reflexivity.
Qed.

Theorem accepted_negative_count_is_exact :
  forall count amount,
    Pos.to_nat amount <= count ->
    checked_count_delta count (Zneg amount) =
      Some (count - Pos.to_nat amount).
Proof.
  intros count amount Hle.
  cbn.
  destruct (Pos.to_nat amount <=? count) eqn:Hcomparison.
  - reflexivity.
  - apply Nat.leb_gt in Hcomparison. lia.
Qed.

Record scan_capture : Type := mkScanCapture {
  scan_root_revision : nat;
  scan_generation : nat;
  scan_frontier_revision : nat;
  scan_resident_count : nat
}.

Definition scan_accepts
    (capture : scan_capture)
    (current_root : root_state)
    (current_frontier : nat) : Prop :=
  root_revision current_root = scan_root_revision capture /\
  root_generation current_root = Some (scan_generation capture) /\
  current_frontier = scan_frontier_revision capture /\
  current_frontier = root_revision current_root.

Theorem accepted_scan_is_exact_root_and_frontier :
  forall capture current_root current_frontier,
    scan_accepts capture current_root current_frontier ->
    root_revision current_root = scan_root_revision capture /\
    root_generation current_root = Some (scan_generation capture) /\
    current_frontier = root_revision current_root.
Proof.
  intros capture current_root current_frontier
         [Hroot [Hgeneration [_ Hfrontier]]].
  repeat split; assumption.
Qed.

End HelpedRootResidencySpec.
