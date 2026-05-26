(** * PersistentReadTraversalSpec: Public Read Snapshot Laws

    This module states the semantic proof boundary for public iterator,
    prefix-iterator, and zipper-style traversal over persistent tries.

    The checked claim is deliberately above the filesystem boundary:

    - successful traversal returns exactly the visible snapshot restricted to
      the requested prefix;
    - every yielded key/value came from that snapshot;
    - every visible key/value under the prefix is yielded;
    - lazy-load or disk-reference failure is fail-closed and yields no entries;
    - read failures preserve the visible snapshot.
*)

From Stdlib Require Import Arith.PeanoNat.
From Stdlib Require Import Logic.FunctionalExtensionality.

Definition Key := nat.
Definition Prefix := nat.

Definition KeyMap (V : Type) := Key -> option V.

Definition map_empty {V : Type} : KeyMap V := fun _ => None.

Definition same_key_map {V : Type} (left right : KeyMap V) : Prop :=
  forall key, left key = right key.

Definition prefix_matches (prefix key : Key) : Prop :=
  prefix = 0 \/ prefix = key.

Definition prefix_matches_dec
  (prefix key : Key) : {prefix_matches prefix key} + {~ prefix_matches prefix key}.
Proof.
  unfold prefix_matches.
  destruct (Nat.eq_dec prefix 0) as [Hzero | Hnonzero].
  - left. left. exact Hzero.
  - destruct (Nat.eq_dec prefix key) as [Heq | Hneq].
    + left. right. exact Heq.
    + right. intros [Hzero | Heq]; contradiction.
Defined.

Definition snapshot_prefix_filter {V : Type}
  (snapshot : KeyMap V) (prefix : Prefix) : KeyMap V :=
  fun key => if prefix_matches_dec prefix key then snapshot key else None.

Inductive TraversalResult (V : Type) : Type :=
| TraversalOk : KeyMap V -> TraversalResult V
| TraversalErr : TraversalResult V.

Arguments TraversalOk {V} _.
Arguments TraversalErr {V}.

Definition public_result_map {V : Type} (result : TraversalResult V) : KeyMap V :=
  match result with
  | TraversalOk view => view
  | TraversalErr => map_empty
  end.

Definition traversal_exact {V : Type}
  (snapshot : KeyMap V) (prefix : Prefix) (result : TraversalResult V) : Prop :=
  match result with
  | TraversalOk view => same_key_map view (snapshot_prefix_filter snapshot prefix)
  | TraversalErr => True
  end.

Definition read_failure_step {V : Type}
  (snapshot : KeyMap V) : KeyMap V * TraversalResult V :=
  (snapshot, TraversalErr).

Definition read_success_step {V : Type}
  (snapshot : KeyMap V) (prefix : Prefix) : KeyMap V * TraversalResult V :=
  (snapshot, TraversalOk (snapshot_prefix_filter snapshot prefix)).

Theorem prefix_filter_sound :
  forall (V : Type) (snapshot : KeyMap V) prefix key value,
    snapshot_prefix_filter snapshot prefix key = Some value ->
    snapshot key = Some value /\ prefix_matches prefix key.
Proof.
  intros V snapshot prefix key value Hlookup.
  unfold snapshot_prefix_filter in Hlookup.
  destruct (prefix_matches_dec prefix key) as [Hprefix | Hnot].
  - split; assumption.
  - discriminate Hlookup.
Qed.

Theorem prefix_filter_complete :
  forall (V : Type) (snapshot : KeyMap V) prefix key value,
    snapshot key = Some value ->
    prefix_matches prefix key ->
    snapshot_prefix_filter snapshot prefix key = Some value.
Proof.
  intros V snapshot prefix key value Hlookup Hprefix.
  unfold snapshot_prefix_filter.
  destruct (prefix_matches_dec prefix key) as [_ | Hnot].
  - exact Hlookup.
  - contradiction.
Qed.

Theorem prefix_filter_rejects_nonmatching :
  forall (V : Type) (snapshot : KeyMap V) prefix key,
    ~ prefix_matches prefix key ->
    snapshot_prefix_filter snapshot prefix key = None.
Proof.
  intros V snapshot prefix key Hnot.
  unfold snapshot_prefix_filter.
  destruct (prefix_matches_dec prefix key) as [Hprefix | _].
  - contradiction.
  - reflexivity.
Qed.

Theorem empty_prefix_returns_snapshot :
  forall (V : Type) (snapshot : KeyMap V),
    snapshot_prefix_filter snapshot 0 = snapshot.
Proof.
  intros V snapshot.
  apply functional_extensionality.
  intros key.
  unfold snapshot_prefix_filter.
  destruct (prefix_matches_dec 0 key) as [_ | Hnot].
  - reflexivity.
  - exfalso. apply Hnot. left. reflexivity.
Qed.

Theorem successful_read_is_exact :
  forall (V : Type) (snapshot : KeyMap V) prefix,
    traversal_exact snapshot prefix (snd (read_success_step snapshot prefix)).
Proof.
  intros V snapshot prefix.
  unfold read_success_step, traversal_exact, same_key_map.
  simpl.
  intros key.
  reflexivity.
Qed.

Theorem successful_read_sound :
  forall (V : Type) (snapshot : KeyMap V) prefix key value,
    public_result_map (snd (read_success_step snapshot prefix)) key = Some value ->
    snapshot key = Some value /\ prefix_matches prefix key.
Proof.
  intros V snapshot prefix key value Hlookup.
  unfold read_success_step in Hlookup.
  simpl in Hlookup.
  apply prefix_filter_sound in Hlookup.
  exact Hlookup.
Qed.

Theorem successful_read_complete :
  forall (V : Type) (snapshot : KeyMap V) prefix key value,
    snapshot key = Some value ->
    prefix_matches prefix key ->
    public_result_map (snd (read_success_step snapshot prefix)) key = Some value.
Proof.
  intros V snapshot prefix key value Hlookup Hprefix.
  unfold read_success_step.
  simpl.
  apply prefix_filter_complete; assumption.
Qed.

Theorem failed_read_is_closed :
  forall (V : Type) (snapshot : KeyMap V),
    public_result_map (snd (read_failure_step snapshot)) = map_empty.
Proof.
  intros V snapshot.
  unfold read_failure_step, public_result_map.
  simpl.
  reflexivity.
Qed.

Theorem failed_read_fabricates_no_entries :
  forall (V : Type) (snapshot : KeyMap V) key value,
    public_result_map (snd (read_failure_step snapshot)) key = Some value ->
    False.
Proof.
  intros V snapshot key value Hlookup.
  unfold read_failure_step, public_result_map, map_empty in Hlookup.
  simpl in Hlookup.
  discriminate Hlookup.
Qed.

Theorem failed_read_preserves_snapshot :
  forall (V : Type) (snapshot : KeyMap V),
    fst (read_failure_step snapshot) = snapshot.
Proof.
  intros V snapshot.
  reflexivity.
Qed.

Theorem successful_read_preserves_snapshot :
  forall (V : Type) (snapshot : KeyMap V) prefix,
    fst (read_success_step snapshot prefix) = snapshot.
Proof.
  intros V snapshot prefix.
  reflexivity.
Qed.

Theorem traversal_ok_no_fabrication :
  forall (V : Type) (snapshot view : KeyMap V) prefix key value,
    same_key_map view (snapshot_prefix_filter snapshot prefix) ->
    public_result_map (TraversalOk view) key = Some value ->
    snapshot key = Some value.
Proof.
  intros V snapshot view prefix key value Hexact Hlookup.
  unfold public_result_map in Hlookup.
  pose proof (Hexact key) as Hview.
  rewrite Hview in Hlookup.
  apply prefix_filter_sound in Hlookup.
  destruct Hlookup as [Hsnapshot _].
  exact Hsnapshot.
Qed.

Theorem traversal_ok_prefix_safe :
  forall (V : Type) (snapshot view : KeyMap V) prefix key value,
    same_key_map view (snapshot_prefix_filter snapshot prefix) ->
    public_result_map (TraversalOk view) key = Some value ->
    prefix_matches prefix key.
Proof.
  intros V snapshot view prefix key value Hexact Hlookup.
  unfold public_result_map in Hlookup.
  pose proof (Hexact key) as Hview.
  rewrite Hview in Hlookup.
  apply prefix_filter_sound in Hlookup.
  destruct Hlookup as [_ Hprefix].
  exact Hprefix.
Qed.

Theorem traversal_error_no_fabrication :
  forall (V : Type) (snapshot : KeyMap V) key value,
    public_result_map TraversalErr key = Some value ->
    snapshot key = Some value.
Proof.
  intros V snapshot key value Hlookup.
  unfold public_result_map, map_empty in Hlookup.
  discriminate Hlookup.
Qed.
