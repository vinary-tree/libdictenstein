(** * Exact overlay-fault provenance and re-eviction authorization

    A durable provenance stamp records where one exact immutable node image is
    represented on disk.  It is necessary but deliberately insufficient for
    eviction: the live root revision, root/registry generation, registry path,
    disk pointer, authority bit, and residency bit must all match in the same
    commit transaction.

    This model separates the off-lock disk decode from that exact commit.  A
    decoded node carries both the disk image's payload and its pointer stamp.
    The winning root transition installs that node and marks precisely its
    binding resident.  A losing transition is the identity.  Detached loads may
    carry truthful stamps but cannot authorize eviction because they are neither
    installed nor resident.  Every structural path copy clears its stamp, so a
    mutation cannot re-evict to an older durable image.  The winning, unchanged
    fault state is immediately eligible for exact re-eviction.
*)

From Stdlib Require Import Arith Bool Lia List PeanoNat.
Import ListNotations.

Module OverlayFaultProvenanceSpec.

Definition DiskPtr := nat.
Definition Generation := nat.
Definition RootRevision := nat.
Definition Path := list nat.
Definition Payload := nat.
Definition DiskImage := DiskPtr -> Payload.

Record OverlayNode : Type := mkOverlayNode {
  node_payload : Payload;
  node_provenance : option DiskPtr
}.

Definition decode_exact
    (image : DiskImage) (pointer : DiskPtr) : OverlayNode :=
  mkOverlayNode (image pointer) (Some pointer).

Definition checkpoint_exact
    (payload : Payload) (pointer : DiskPtr) : OverlayNode :=
  mkOverlayNode payload (Some pointer).

Definition structural_path_copy
    (node : OverlayNode) (new_payload : Payload) : OverlayNode :=
  mkOverlayNode new_payload None.

Theorem exact_decode_has_truthful_payload_and_stamp :
  forall image pointer,
    node_payload (decode_exact image pointer) = image pointer /\
    node_provenance (decode_exact image pointer) = Some pointer.
Proof.
  intros image pointer. split; reflexivity.
Qed.

Theorem exact_checkpoint_has_requested_stamp :
  forall payload pointer,
    node_payload (checkpoint_exact payload pointer) = payload /\
    node_provenance (checkpoint_exact payload pointer) = Some pointer.
Proof.
  intros payload pointer. split; reflexivity.
Qed.

Theorem every_structural_path_copy_clears_provenance :
  forall node new_payload,
    node_provenance (structural_path_copy node new_payload) = None.
Proof.
  intros node new_payload. reflexivity.
Qed.

Inductive OverlaySlot : Type :=
| SlotOnDisk : DiskPtr -> OverlaySlot
| SlotInMem : OverlayNode -> OverlaySlot.

Record RegistryBinding : Type := mkRegistryBinding {
  binding_path : Path;
  binding_disk : DiskPtr;
  binding_generation : Generation;
  binding_resident : bool
}.

Record FaultState : Type := mkFaultState {
  state_root_revision : RootRevision;
  state_root_generation : Generation;
  state_registry_generation : Generation;
  state_authoritative : bool;
  state_slot : OverlaySlot;
  state_binding : RegistryBinding
}.

Record PreparedFault : Type := mkPreparedFault {
  prepared_expected_root : RootRevision;
  prepared_path : Path;
  prepared_disk : DiskPtr;
  prepared_generation : Generation;
  prepared_node : OverlayNode
}.

Definition prepare_exact_fault
    (image : DiskImage)
    (state : FaultState)
    (path : Path)
    (pointer : DiskPtr) : PreparedFault :=
  mkPreparedFault
    (state_root_revision state)
    path
    pointer
    (state_registry_generation state)
    (decode_exact image pointer).

Definition exact_fault_precondition
    (image : DiskImage) (state : FaultState) (prepared : PreparedFault) : Prop :=
  state_authoritative state = true /\
  prepared_expected_root prepared = state_root_revision state /\
  prepared_generation prepared = state_registry_generation state /\
  state_root_generation state = state_registry_generation state /\
  binding_generation (state_binding state) = state_registry_generation state /\
  prepared_path prepared = binding_path (state_binding state) /\
  prepared_disk prepared = binding_disk (state_binding state) /\
  binding_resident (state_binding state) = false /\
  state_slot state = SlotOnDisk (prepared_disk prepared) /\
  node_payload (prepared_node prepared) = image (prepared_disk prepared) /\
  node_provenance (prepared_node prepared) = Some (prepared_disk prepared).

Definition resident_binding (binding : RegistryBinding) : RegistryBinding :=
  mkRegistryBinding
    (binding_path binding)
    (binding_disk binding)
    (binding_generation binding)
    true.

Definition fault_winner_state
    (state : FaultState) (prepared : PreparedFault) : FaultState :=
  mkFaultState
    (S (state_root_revision state))
    (state_root_generation state)
    (state_registry_generation state)
    (state_authoritative state)
    (SlotInMem (prepared_node prepared))
    (resident_binding (state_binding state)).

Inductive FaultOutcome : Type :=
| FaultWon
| FaultLost.

Inductive fault_commit (image : DiskImage) :
    FaultState -> PreparedFault -> FaultOutcome -> FaultState -> Prop :=
| CommitFaultWinner : forall state prepared,
    exact_fault_precondition image state prepared ->
    fault_commit image state prepared FaultWon
      (fault_winner_state state prepared)
| CommitFaultLoser : forall state prepared,
    ~ exact_fault_precondition image state prepared ->
    fault_commit image state prepared FaultLost state.

Definition exact_eviction_authorized
    (image : DiskImage)
    (state : FaultState)
    (path : Path)
    (pointer : DiskPtr) : Prop :=
  state_authoritative state = true /\
  state_root_generation state = state_registry_generation state /\
  binding_generation (state_binding state) = state_registry_generation state /\
  binding_path (state_binding state) = path /\
  binding_disk (state_binding state) = pointer /\
  binding_resident (state_binding state) = true /\
  exists node,
    state_slot state = SlotInMem node /\
    node_payload node = image pointer /\
    node_provenance node = Some pointer.

Theorem prepared_exact_fault_decodes_before_publication :
  forall image state path pointer,
    node_payload (prepared_node (prepare_exact_fault image state path pointer)) =
      image pointer /\
    node_provenance
      (prepared_node (prepare_exact_fault image state path pointer)) =
      Some pointer.
Proof.
  intros image state path pointer. split; reflexivity.
Qed.

Theorem exact_fault_winner_is_the_only_publishing_transition :
  forall image state prepared final,
    fault_commit image state prepared FaultWon final ->
    final = fault_winner_state state prepared.
Proof.
  intros image state prepared final Hcommit.
  inversion Hcommit. reflexivity.
Qed.

Theorem exact_fault_winner_installs_decoded_node_and_residence :
  forall image state prepared final,
    fault_commit image state prepared FaultWon final ->
    state_slot final = SlotInMem (prepared_node prepared) /\
    binding_resident (state_binding final) = true /\
    state_root_revision final = S (state_root_revision state) /\
    state_root_generation final = state_root_generation state /\
    state_registry_generation final = state_registry_generation state.
Proof.
  intros image state prepared final Hcommit.
  apply exact_fault_winner_is_the_only_publishing_transition in Hcommit.
  subst final. repeat split; reflexivity.
Qed.

Theorem losing_fault_is_state_identity :
  forall image state prepared final,
    fault_commit image state prepared FaultLost final ->
    final = state.
Proof.
  intros image state prepared final Hcommit.
  inversion Hcommit. reflexivity.
Qed.

Theorem losing_fault_cannot_mark_resident_or_publish_its_decode :
  forall image state prepared final,
    fault_commit image state prepared FaultLost final ->
    state_slot final = state_slot state /\
    state_binding final = state_binding state /\
    state_root_revision final = state_root_revision state.
Proof.
  intros image state prepared final Hcommit.
  apply losing_fault_is_state_identity in Hcommit.
  subst final. repeat split; reflexivity.
Qed.

(** Unbound fallback installs are separate from exact registry commits. They
    may publish a truthfully decoded node after retirement has removed the root
    generation, but they never update registry residency or create eviction
    authority. *)
Record UnboundFaultState : Type := mkUnboundFaultState {
  unbound_root_revision : RootRevision;
  unbound_slot : OverlaySlot;
  unbound_binding : RegistryBinding
}.

Definition unbound_fault_precondition
    (image : DiskImage)
    (state : UnboundFaultState)
    (prepared : PreparedFault) : Prop :=
  prepared_expected_root prepared = unbound_root_revision state /\
  unbound_slot state = SlotOnDisk (prepared_disk prepared) /\
  node_payload (prepared_node prepared) = image (prepared_disk prepared) /\
  node_provenance (prepared_node prepared) = Some (prepared_disk prepared).

Definition unbound_fault_winner_state
    (state : UnboundFaultState)
    (prepared : PreparedFault) : UnboundFaultState :=
  mkUnboundFaultState
    (S (unbound_root_revision state))
    (SlotInMem (prepared_node prepared))
    (unbound_binding state).

Inductive unbound_fault_commit (image : DiskImage) :
    UnboundFaultState -> PreparedFault -> FaultOutcome -> UnboundFaultState -> Prop :=
| CommitUnboundFaultWinner : forall state prepared,
    unbound_fault_precondition image state prepared ->
    unbound_fault_commit image state prepared FaultWon
      (unbound_fault_winner_state state prepared)
| CommitUnboundFaultLoser : forall state prepared,
    ~ unbound_fault_precondition image state prepared ->
    unbound_fault_commit image state prepared FaultLost state.

Theorem unbound_fault_winner_installs_exact_decode_and_advances_revision :
  forall image state prepared final,
    unbound_fault_commit image state prepared FaultWon final ->
    unbound_root_revision final = S (unbound_root_revision state) /\
    unbound_slot final = SlotInMem (prepared_node prepared) /\
    node_payload (prepared_node prepared) = image (prepared_disk prepared) /\
    node_provenance (prepared_node prepared) = Some (prepared_disk prepared).
Proof.
  intros image state prepared final Hcommit.
  inversion Hcommit; subst. unfold unbound_fault_precondition in H.
  destruct H as [_ [_ [Hpayload Hstamp]]].
  repeat split; assumption || reflexivity.
Qed.

Theorem unbound_fault_winner_preserves_registry_residency_metadata :
  forall state prepared,
    unbound_binding (unbound_fault_winner_state state prepared) =
      unbound_binding state /\
    binding_resident
      (unbound_binding (unbound_fault_winner_state state prepared)) =
      binding_resident (unbound_binding state).
Proof.
  intros state prepared. split; reflexivity.
Qed.

Definition unbound_as_non_authoritative_state
    (state : UnboundFaultState) (generation : Generation) : FaultState :=
  mkFaultState
    (unbound_root_revision state)
    generation
    generation
    false
    (unbound_slot state)
    (unbound_binding state).

Theorem unbound_fault_winner_cannot_authorize_re_eviction :
  forall image state prepared path pointer generation,
    ~ exact_eviction_authorized
        image
        (unbound_as_non_authoritative_state
          (unbound_fault_winner_state state prepared) generation)
        path
        pointer.
Proof.
  intros image state prepared path pointer generation Hauthorized.
  destruct Hauthorized as [Hauthority _]. discriminate.
Qed.

Theorem winning_fault_is_exactly_re_evictable :
  forall image state prepared,
    exact_fault_precondition image state prepared ->
    exact_eviction_authorized
      image
      (fault_winner_state state prepared)
      (prepared_path prepared)
      (prepared_disk prepared).
Proof.
  intros image state prepared Hexact.
  destruct Hexact as
    [Hauthority
      [Hroot
        [HpreparedGeneration
          [HrootGeneration
            [HbindingGeneration
              [Hpath
                [Hdisk
                  [Hnonresident
                    [Hslot [Hpayload Hstamp]]]]]]]]]].
  unfold exact_eviction_authorized, fault_winner_state, resident_binding.
  simpl. repeat split.
  - exact Hauthority.
  - exact HrootGeneration.
  - exact HbindingGeneration.
  - symmetry. exact Hpath.
  - symmetry. exact Hdisk.
  - exists (prepared_node prepared).
    split; [reflexivity |]. split; assumption.
Qed.

Theorem one_fault_winner_excludes_the_same_prepared_competitor :
  forall image state prepared,
    exact_fault_precondition image state prepared ->
    ~ exact_fault_precondition
        image (fault_winner_state state prepared) prepared.
Proof.
  intros image state prepared Hbefore Hafter.
  destruct Hbefore as [_ [Hexpected _]].
  destruct Hafter as [_ [HexpectedAfter _]].
  simpl in HexpectedAfter.
  rewrite Hexpected in HexpectedAfter. lia.
Qed.

Corollary same_prepared_competitor_is_a_state_identity_loser :
  forall image state prepared,
    exact_fault_precondition image state prepared ->
    fault_commit
      image
      (fault_winner_state state prepared)
      prepared
      FaultLost
      (fault_winner_state state prepared).
Proof.
  intros image state prepared Hexact.
  apply CommitFaultLoser.
  apply one_fault_winner_excludes_the_same_prepared_competitor.
  exact Hexact.
Qed.

Definition install_structural_copy
    (state : FaultState) (node : OverlayNode) : FaultState :=
  mkFaultState
    (S (state_root_revision state))
    (state_root_generation state)
    (state_registry_generation state)
    (state_authoritative state)
    (SlotInMem node)
    (state_binding state).

Theorem structural_path_copy_cannot_re_evict_to_old_image :
  forall image state original new_payload path pointer,
    ~ exact_eviction_authorized
        image
        (install_structural_copy
          state (structural_path_copy original new_payload))
        path
        pointer.
Proof.
  intros image state original new_payload path pointer Hauthorized.
  destruct Hauthorized as
    [_ [_ [_ [_ [_ [_ [node [Hslot [_ Hstamp]]]]]]]]].
  simpl in Hslot. injection Hslot as Heq. subst node.
  simpl in Hstamp. discriminate.
Qed.

Definition detached_load
    (image : DiskImage)
    (pointer : DiskPtr)
    (state : FaultState) : OverlayNode * FaultState :=
  (decode_exact image pointer, state).

Theorem detached_load_has_truthful_provenance_but_no_state_effect :
  forall image pointer state,
    node_payload (fst (detached_load image pointer state)) = image pointer /\
    node_provenance (fst (detached_load image pointer state)) = Some pointer /\
    snd (detached_load image pointer state) = state.
Proof.
  intros image pointer state. repeat split; reflexivity.
Qed.

Theorem detached_nonresident_decode_is_not_eviction_authority :
  forall image state path pointer,
    state_slot state = SlotOnDisk pointer ->
    binding_resident (state_binding state) = false ->
    ~ exact_eviction_authorized image state path pointer.
Proof.
  intros image state path pointer Hslot Hnonresident Hauthorized.
  destruct Hauthorized as
    [_ [_ [_ [_ [_ [Hresident [node [Hinmem _]]]]]]]].
  rewrite Hnonresident in Hresident. discriminate.
Qed.

Theorem provenance_stamp_alone_is_not_authority :
  forall image state node path pointer,
    node_provenance node = Some pointer ->
    state_authoritative state = false ->
    ~ exact_eviction_authorized image state path pointer.
Proof.
  intros image state node path pointer Hstamp Hinvalid Hauthorized.
  destruct Hauthorized as [Hauthority _].
  rewrite Hinvalid in Hauthority. discriminate.
Qed.

Theorem generation_mismatch_rejects_re_eviction :
  forall image state path pointer,
    state_root_generation state <> state_registry_generation state ->
    ~ exact_eviction_authorized image state path pointer.
Proof.
  intros image state path pointer Hmismatch Hauthorized.
  destruct Hauthorized as [_ [Hequal _]]. contradiction.
Qed.

Theorem path_mismatch_rejects_re_eviction :
  forall image state path pointer,
    binding_path (state_binding state) <> path ->
    ~ exact_eviction_authorized image state path pointer.
Proof.
  intros image state path pointer Hmismatch Hauthorized.
  destruct Hauthorized as [_ [_ [_ [Hequal _]]]]. contradiction.
Qed.

Theorem disk_pointer_mismatch_rejects_re_eviction :
  forall image state path pointer,
    binding_disk (state_binding state) <> pointer ->
    ~ exact_eviction_authorized image state path pointer.
Proof.
  intros image state path pointer Hmismatch Hauthorized.
  destruct Hauthorized as [_ [_ [_ [_ [Hequal _]]]]]. contradiction.
Qed.

Definition advance_root_without_fault (state : FaultState) : FaultState :=
  mkFaultState
    (S (state_root_revision state))
    (state_root_generation state)
    (state_registry_generation state)
    (state_authoritative state)
    (state_slot state)
    (state_binding state).

Theorem root_advance_makes_prepared_fault_lose_exactness :
  forall image state prepared,
    exact_fault_precondition image state prepared ->
    ~ exact_fault_precondition image (advance_root_without_fault state) prepared.
Proof.
  intros image state prepared Hbefore Hafter.
  destruct Hbefore as [_ [Hexpected _]].
  destruct Hafter as [_ [HexpectedAfter _]].
  simpl in HexpectedAfter.
  rewrite Hexpected in HexpectedAfter. lia.
Qed.

Corollary root_advanced_prepared_fault_is_a_state_identity_loser :
  forall image state prepared,
    exact_fault_precondition image state prepared ->
    fault_commit
      image
      (advance_root_without_fault state)
      prepared
      FaultLost
      (advance_root_without_fault state).
Proof.
  intros image state prepared Hexact.
  apply CommitFaultLoser.
  apply root_advance_makes_prepared_fault_lose_exactness.
  exact Hexact.
Qed.

Theorem captured_exact_fault_is_ready_when_binding_is_exact :
  forall image state path pointer,
    state_authoritative state = true ->
    state_root_generation state = state_registry_generation state ->
    binding_generation (state_binding state) = state_registry_generation state ->
    binding_path (state_binding state) = path ->
    binding_disk (state_binding state) = pointer ->
    binding_resident (state_binding state) = false ->
    state_slot state = SlotOnDisk pointer ->
    exact_fault_precondition
      image state (prepare_exact_fault image state path pointer).
Proof.
  intros image state path pointer
         Hauthority HrootGeneration HbindingGeneration
         Hpath Hdisk Hresident Hslot.
  unfold exact_fault_precondition, prepare_exact_fault. simpl.
  repeat split.
  - exact Hauthority.
  - exact HrootGeneration.
  - exact HbindingGeneration.
  - symmetry. exact Hpath.
  - symmetry. exact Hdisk.
  - exact Hresident.
  - exact Hslot.
Qed.

Corollary captured_fault_wins_then_is_re_evictable :
  forall image state path pointer,
    state_authoritative state = true ->
    state_root_generation state = state_registry_generation state ->
    binding_generation (state_binding state) = state_registry_generation state ->
    binding_path (state_binding state) = path ->
    binding_disk (state_binding state) = pointer ->
    binding_resident (state_binding state) = false ->
    state_slot state = SlotOnDisk pointer ->
    exact_eviction_authorized
      image
      (fault_winner_state
        state (prepare_exact_fault image state path pointer))
      path
      pointer.
Proof.
  intros image state path pointer
         Hauthority HrootGeneration HbindingGeneration
         Hpath Hdisk Hresident Hslot.
  apply winning_fault_is_exactly_re_evictable.
  apply captured_exact_fault_is_ready_when_binding_is_exact; assumption.
Qed.

End OverlayFaultProvenanceSpec.
