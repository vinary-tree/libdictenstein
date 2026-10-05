(** * Overlay Arc ownership accounting

    This module models strong-reference operations, not dictionary language
    traversal.  Distinct token identities preserve multiplicity when several
    child slots or retained handles point at one physical payload.  The ledger
    is an unordered finite collection when [unique_reference_identities] holds.
    The raw functions also accept malformed duplicate-identity lists; machine
    admission and preservation must establish the invariant.  Worklist order
    and native activation stacks belong to the consuming destructor machine.

    An external holder identity is not an executing thread identity: a published
    root, captured cursor, or application handle can own a token.  Work and
    failed-result locations additionally identify a thread and activation.

    Counts are observed at the operation's modeled atomic decision point.  A
    failed uniqueness check retains a token; a later ordinary release is a
    distinct operation.  Other owners may retire between those operations.
    Conversely, into_inner consumes its token even when it returns no payload.

    ExtractPayload and InvokePayloadDrop are deliberately distinct.  The former
    transfers an exclusive payload right; the latter requests synchronous Drop.
    Neither operation consumes the payload's outgoing child-reference tokens.
    The graph machine must track those rights/obligations and drain the children
    before marking PayloadDestroyed.  Zero strong references does not imply
    allocation deallocation: weak references can retain allocation storage.

    The operations abstract the standard library contract, not its compiler or
    memory-order implementation.  Reference: https://doc.rust-lang.org/std/sync/struct.Arc.html
    Acquisition with a fresh identity is modeled explicitly.  Weak upgrade has
    the same strong-ledger effect as acquisition from a live payload; weak-only
    ownership cannot resurrect an extracted or destroyed payload.
*)

From Coq Require Import Lists.List Arith.PeanoNat Bool.Bool Lia.
From Coq Require Import Sorting.Permutation.
Import ListNotations.

Inductive ArcOwner :=
| ExternalHolder (holder : nat)
| ChildSlotOwner (parent : nat)
| WorklistOwner (thread activation : nat)
| FailedResultOwner (thread activation : nat).

Record ArcReference := {
  reference_identity : nat;
  reference_payload : nat;
  reference_owner : ArcOwner
}.

Definition ArcLedger := list ArcReference.

Definition reference_at (reference : ArcReference) (owner : ArcOwner)
  : ArcReference :=
  {| reference_identity := reference_identity reference;
     reference_payload := reference_payload reference;
     reference_owner := owner |}.

Fixpoint strong_count (payload : nat) (ledger : ArcLedger) : nat :=
  match ledger with
  | [] => 0
  | reference :: rest =>
      (if Nat.eqb payload (reference_payload reference) then 1 else 0) +
      strong_count payload rest
  end.

(** Taking a token moves it out of the ledger; it does not itself perform a
    Rust Arc release.  The outcome constructors below decide whether the token
    is retained, consumed into a payload, or released ordinarily. *)
Fixpoint take_reference (identity : nat) (ledger : ArcLedger)
  : option (ArcReference * ArcLedger) :=
  match ledger with
  | [] => None
  | reference :: rest =>
      if Nat.eqb identity (reference_identity reference)
      then Some (reference, rest)
      else match take_reference identity rest with
           | None => None
           | Some (found, remaining) => Some (found, reference :: remaining)
           end
  end.

Definition unique_reference_identities (ledger : ArcLedger) : Prop :=
  NoDup (map reference_identity ledger).

Inductive ArcSignal :=
| InvalidReference
| AcquiredReference (identity : nat)
| ExtractPayload (payload : nat)
| RetainedFailure (identity : nat)
| ReleasedShared (payload : nat)
| InvokePayloadDrop (payload : nat).

Record ArcOutcome := {
  arc_signal : ArcSignal;
  remaining_references : ArcLedger
}.

Definition invalid_reference (ledger : ArcLedger) : ArcOutcome :=
  {| arc_signal := InvalidReference; remaining_references := ledger |}.

Definition try_unwrap_reference
  (identity thread activation : nat) (ledger : ArcLedger) : ArcOutcome :=
  match take_reference identity ledger with
  | None => invalid_reference ledger
  | Some (reference, remaining) =>
      match strong_count (reference_payload reference) remaining with
      | 0 => {| arc_signal := ExtractPayload (reference_payload reference);
                remaining_references := remaining |}
      | S _ =>
          {| arc_signal := RetainedFailure identity;
             remaining_references :=
               reference_at reference (FailedResultOwner thread activation) :: remaining |}
      end
  end.

Definition into_inner_reference (identity : nat) (ledger : ArcLedger) : ArcOutcome :=
  match take_reference identity ledger with
  | None => invalid_reference ledger
  | Some (reference, remaining) =>
      {| arc_signal :=
           match strong_count (reference_payload reference) remaining with
           | 0 => ExtractPayload (reference_payload reference)
           | S _ => ReleasedShared (reference_payload reference)
           end;
         remaining_references := remaining |}
  end.

Definition release_reference (identity : nat) (ledger : ArcLedger) : ArcOutcome :=
  match take_reference identity ledger with
  | None => invalid_reference ledger
  | Some (reference, remaining) =>
      {| arc_signal :=
           match strong_count (reference_payload reference) remaining with
           | 0 => InvokePayloadDrop (reference_payload reference)
           | S _ => ReleasedShared (reference_payload reference)
           end;
         remaining_references := remaining |}
  end.

(** A clone acquires one fresh token without consuming the source token.  The
    caller/graph machine establishes authority to use that source reference.
    An unknown source or currently present destination identity is invalid.
    Freshness here means absence from the current ledger, not a historical
    never-issued identity or an externally reusable capability. *)
Definition clone_reference
  (source fresh : nat) (owner : ArcOwner) (ledger : ArcLedger) : ArcOutcome :=
  match take_reference fresh ledger, take_reference source ledger with
  | None, Some (reference, _) =>
      {| arc_signal := AcquiredReference fresh;
         remaining_references :=
           {| reference_identity := fresh;
              reference_payload := reference_payload reference;
              reference_owner := owner |} :: ledger |}
  | _, _ => invalid_reference ledger
  end.

Theorem RSDICT_216_strong_count_append :
  forall payload left right,
    strong_count payload (left ++ right) =
    strong_count payload left + strong_count payload right.
Proof.
  intros payload left. induction left as [|reference rest IH]; intros right.
  - reflexivity.
  - simpl. rewrite IH. lia.
Qed.

Theorem RSDICT_217_relocation_preserves_payload_and_identity :
  forall reference owner,
    reference_payload (reference_at reference owner) = reference_payload reference /\
    reference_identity (reference_at reference owner) = reference_identity reference.
Proof. intros. split; reflexivity. Qed.

Theorem RSDICT_218_take_reference_matches_identity :
  forall ledger identity reference remaining,
    take_reference identity ledger = Some (reference, remaining) ->
    reference_identity reference = identity.
Proof.
  induction ledger as [|head rest IH]; intros identity reference remaining Htake.
  - discriminate.
  - simpl in Htake.
    destruct (Nat.eqb identity (reference_identity head)) eqn:Hidentity.
    + inversion Htake; subst. apply Nat.eqb_eq in Hidentity. symmetry. exact Hidentity.
    + destruct (take_reference identity rest) as [[found tail]|] eqn:Hrest;
        try discriminate.
      inversion Htake; subst. eapply IH. exact Hrest.
Qed.

Theorem RSDICT_219_take_reference_removes_exactly_one_token :
  forall ledger identity reference remaining,
    take_reference identity ledger = Some (reference, remaining) ->
    length ledger = S (length remaining).
Proof.
  induction ledger as [|head rest IH]; intros identity reference remaining Htake.
  - discriminate.
  - simpl in Htake.
    destruct (Nat.eqb identity (reference_identity head)).
    + inversion Htake; subst. reflexivity.
    + destruct (take_reference identity rest) as [[found tail]|] eqn:Hrest;
        try discriminate.
      inversion Htake; subst. simpl. f_equal. eapply IH. exact Hrest.
Qed.

Theorem RSDICT_220_take_reference_accounts_for_every_payload :
  forall ledger identity reference remaining payload,
    take_reference identity ledger = Some (reference, remaining) ->
    strong_count payload ledger =
    (if Nat.eqb payload (reference_payload reference) then 1 else 0) +
    strong_count payload remaining.
Proof.
  induction ledger as [|head rest IH]; intros identity reference remaining payload Htake.
  - discriminate.
  - simpl in Htake.
    destruct (Nat.eqb identity (reference_identity head)).
    + inversion Htake; subst. reflexivity.
    + destruct (take_reference identity rest) as [[found tail]|] eqn:Hrest;
        try discriminate.
      inversion Htake; subst. simpl.
      specialize (IH identity reference tail payload Hrest). lia.
Qed.

Theorem RSDICT_221_failed_unwrap_preserves_all_strong_counts :
  forall ledger identity thread activation,
    arc_signal (try_unwrap_reference identity thread activation ledger) =
      RetainedFailure identity ->
    forall payload,
      strong_count payload
        (remaining_references (try_unwrap_reference identity thread activation ledger)) =
      strong_count payload ledger.
Proof.
  intros ledger identity thread activation Hsignal payload.
  unfold try_unwrap_reference in *.
  destruct (take_reference identity ledger) as [[reference remaining]|] eqn:Htake;
    try discriminate.
  destruct (strong_count (reference_payload reference) remaining); try discriminate.
  simpl. symmetry. eapply RSDICT_220_take_reference_accounts_for_every_payload.
  exact Htake.
Qed.

Theorem RSDICT_222_inner_consumption_removes_one_token :
  forall ledger identity reference remaining,
    take_reference identity ledger = Some (reference, remaining) ->
    remaining_references (into_inner_reference identity ledger) = remaining /\
    length ledger = S (length remaining).
Proof.
  intros ledger identity reference remaining Htake. split.
  - unfold into_inner_reference. rewrite Htake. reflexivity.
  - eapply RSDICT_219_take_reference_removes_exactly_one_token. exact Htake.
Qed.

Theorem RSDICT_223_extraction_is_not_an_ordinary_last_release :
  forall ledger identity payload,
    arc_signal (into_inner_reference identity ledger) = ExtractPayload payload ->
    arc_signal (release_reference identity ledger) = InvokePayloadDrop payload.
Proof.
  intros ledger identity payload Hsignal.
  unfold into_inner_reference in Hsignal. unfold release_reference.
  destruct (take_reference identity ledger) as [[reference remaining]|]; try discriminate.
  destruct (strong_count (reference_payload reference) remaining); simpl in *;
    congruence.
Qed.

Theorem RSDICT_224_unknown_reference_is_not_success :
  forall ledger identity thread activation,
    take_reference identity ledger = None ->
    try_unwrap_reference identity thread activation ledger = invalid_reference ledger /\
    into_inner_reference identity ledger = invalid_reference ledger /\
    release_reference identity ledger = invalid_reference ledger.
Proof.
  intros ledger identity thread activation Htake.
  unfold try_unwrap_reference, into_inner_reference, release_reference.
  rewrite Htake. repeat split; reflexivity.
Qed.

Theorem RSDICT_225_fresh_clone_adds_one_matching_payload_token :
  forall ledger source fresh owner reference remaining,
    take_reference fresh ledger = None ->
    take_reference source ledger = Some (reference, remaining) ->
    forall payload,
      strong_count payload
        (remaining_references (clone_reference source fresh owner ledger)) =
      (if Nat.eqb payload (reference_payload reference) then 1 else 0) +
      strong_count payload ledger.
Proof.
  intros ledger source fresh owner reference remaining Hfresh Hsource payload.
  unfold clone_reference. rewrite Hfresh, Hsource. reflexivity.
Qed.

(** One executable interleaving.  It is a local reference-accounting witness,
    not yet a proof of graph reachability, destructor nesting, or a depth bound. *)
Definition two_owner_ledger (payload : nat) : ArcLedger :=
  [{| reference_identity := 1; reference_payload := payload;
      reference_owner := WorklistOwner 0 0 |};
   {| reference_identity := 2; reference_payload := payload;
      reference_owner := ExternalHolder 1 |}].

Theorem RSDICT_226_two_owner_ledger_counts_both_references :
  forall payload, strong_count payload (two_owner_ledger payload) = 2.
Proof.
  intros. cbn [two_owner_ledger strong_count reference_payload].
  rewrite !Nat.eqb_refl. reflexivity.
Qed.

Theorem RSDICT_227_failed_unwrap_then_external_release_then_discard_invokes_drop :
  forall payload,
    let failed := try_unwrap_reference 1 0 0 (two_owner_ledger payload) in
    let retired := release_reference 2 (remaining_references failed) in
    let discarded := release_reference 1 (remaining_references retired) in
    arc_signal failed = RetainedFailure 1 /\
    arc_signal retired = ReleasedShared payload /\
    arc_signal discarded = InvokePayloadDrop payload /\
    remaining_references discarded = [].
Proof.
  intros payload.
  unfold two_owner_ledger.
  repeat progress (cbn; rewrite ?Nat.eqb_refl).
  repeat split; reflexivity.
Qed.

Theorem RSDICT_228_inner_consumption_does_not_leave_a_discardable_failure :
  forall payload,
    let consumed := into_inner_reference 1 (two_owner_ledger payload) in
    let retired := release_reference 2 (remaining_references consumed) in
    let discarded := release_reference 1 (remaining_references retired) in
    arc_signal consumed = ReleasedShared payload /\
    arc_signal retired = InvokePayloadDrop payload /\
    arc_signal discarded = InvalidReference /\
    remaining_references discarded = [].
Proof.
  intros payload.
  unfold two_owner_ledger.
  repeat progress (cbn; rewrite ?Nat.eqb_refl).
  repeat split; reflexivity.
Qed.

Theorem RSDICT_229_take_reference_preserves_the_other_tokens :
  forall ledger identity reference remaining,
    take_reference identity ledger = Some (reference, remaining) ->
    Permutation ledger (reference :: remaining).
Proof.
  induction ledger as [|head rest IH]; intros identity reference remaining Htake.
  - discriminate.
  - simpl in Htake. destruct (Nat.eqb identity (reference_identity head)).
    + inversion Htake; subst. apply Permutation_refl.
    + destruct (take_reference identity rest) as [[found tail]|] eqn:Hrest;
        try discriminate.
      inversion Htake; subst.
      eapply Permutation_trans.
      * apply perm_skip. eapply IH. exact Hrest.
      * apply perm_swap.
Qed.

Theorem RSDICT_230_take_reference_preserves_unique_identities :
  forall ledger identity reference remaining,
    unique_reference_identities ledger ->
    take_reference identity ledger = Some (reference, remaining) ->
    unique_reference_identities remaining /\
    ~ In (reference_identity reference) (map reference_identity remaining).
Proof.
  intros ledger identity reference remaining Hunique Htake.
  unfold unique_reference_identities in *.
  assert (Hpermutation : Permutation (map reference_identity ledger)
    (reference_identity reference :: map reference_identity remaining)).
  { change (Permutation (map reference_identity ledger)
      (map reference_identity (reference :: remaining))).
    apply Permutation_map.
    eapply RSDICT_229_take_reference_preserves_the_other_tokens. exact Htake. }
  pose proof (Permutation_NoDup Hpermutation Hunique) as Hremaining.
  inversion Hremaining. tauto.
Qed.

Theorem RSDICT_231_unknown_identity_is_absent_from_ledger :
  forall ledger identity,
    take_reference identity ledger = None ->
    ~ In identity (map reference_identity ledger).
Proof.
  induction ledger as [|head rest IH]; intros identity Htake.
  - simpl. tauto.
  - simpl in Htake.
    destruct (Nat.eqb identity (reference_identity head)) eqn:Hidentity;
      try discriminate.
    apply Nat.eqb_neq in Hidentity.
    destruct (take_reference identity rest) as [[found remaining]|] eqn:Hrest;
      try discriminate.
    simpl. specialize (IH identity Hrest). intuition congruence.
Qed.

Theorem RSDICT_232_clone_preserves_unique_identities :
  forall ledger source fresh owner,
    unique_reference_identities ledger ->
    unique_reference_identities
      (remaining_references (clone_reference source fresh owner ledger)).
Proof.
  intros ledger source fresh owner Hunique. unfold clone_reference.
  destruct (take_reference fresh ledger) as [[existing tail]|] eqn:Hfresh;
    [exact Hunique|].
  destruct (take_reference source ledger) as [[reference remaining]|] eqn:Hsource;
    [|exact Hunique].
  unfold unique_reference_identities in *. simpl. constructor.
  - eapply RSDICT_231_unknown_identity_is_absent_from_ledger. exact Hfresh.
  - exact Hunique.
Qed.

Theorem RSDICT_233_unwrap_preserves_unique_identities :
  forall ledger identity thread activation,
    unique_reference_identities ledger ->
    unique_reference_identities
      (remaining_references (try_unwrap_reference identity thread activation ledger)).
Proof.
  intros ledger identity thread activation Hunique. unfold try_unwrap_reference.
  destruct (take_reference identity ledger) as [[reference remaining]|] eqn:Htake;
    [|exact Hunique].
  destruct (RSDICT_230_take_reference_preserves_unique_identities
    ledger identity reference remaining Hunique Htake) as [Hremaining Habsent].
  destruct (strong_count (reference_payload reference) remaining).
  - exact Hremaining.
  - unfold unique_reference_identities in *. simpl. constructor; assumption.
Qed.

Theorem RSDICT_234_inner_consumption_preserves_unique_identities :
  forall ledger identity,
    unique_reference_identities ledger ->
    unique_reference_identities
      (remaining_references (into_inner_reference identity ledger)).
Proof.
  intros ledger identity Hunique. unfold into_inner_reference.
  destruct (take_reference identity ledger) as [[reference remaining]|] eqn:Htake;
    [|exact Hunique].
  simpl. eapply RSDICT_230_take_reference_preserves_unique_identities; eauto.
Qed.

Theorem RSDICT_235_ordinary_release_preserves_unique_identities :
  forall ledger identity,
    unique_reference_identities ledger ->
    unique_reference_identities
      (remaining_references (release_reference identity ledger)).
Proof.
  intros ledger identity Hunique. unfold release_reference.
  destruct (take_reference identity ledger) as [[reference remaining]|] eqn:Htake;
    [|exact Hunique].
  simpl. eapply RSDICT_230_take_reference_preserves_unique_identities; eauto.
Qed.

(** This excludes a retained-failure outcome for this operation.  Other pending
    failures already in the ledger can remain there. *)
Theorem RSDICT_236_inner_consumption_never_retains_a_failed_result :
  forall ledger identity pending,
    arc_signal (into_inner_reference identity ledger) <> RetainedFailure pending.
Proof.
  intros ledger identity pending. unfold into_inner_reference.
  destruct (take_reference identity ledger) as [[reference remaining]|];
    [|discriminate].
  destruct (strong_count (reference_payload reference) remaining); discriminate.
Qed.
