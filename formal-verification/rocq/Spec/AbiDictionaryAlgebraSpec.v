(** * AbiDictionaryAlgebraSpec: snapshot-based dictionary algebra laws

    This module models the semantic boundary of [ldict_dictionary_algebra].
    A dictionary revision is a partial map whose present payload is itself
    optional: [None] means an absent key, while [Some None] means a present,
    valueless key. Keys and values remain abstract so the same argument
    applies to byte strings, Unicode-scalar strings, and u64-token sequences.

    The publication model makes ownership explicit. Algebra captures both
    source revisions, computes a new result revision, and stores that result
    separately from the subsequently mutable sources. Source mutation
    therefore cannot rewrite an existing result, while result mutation cannot
    rewrite either source.

    Registry obligations proved here:

    - [LDICT-ALG-1] every result lookup follows the selected set operation;
    - [LDICT-ALG-3] keys are queried without rewriting and absence remains
      distinct from a present valueless entry;
    - [LDICT-ALG-4] the published result is independently mutable and immune
      to later source mutations.

    Duplicate-value policies ([LDICT-ALG-2]) are proved in
    [ValuedSetCombinatorSpec].
*)

Section DictionaryAlgebra.

Variable Key Value : Type.
Variable key_eq_dec : forall (left right : Key), {left = right} + {left <> right}.

(** The outer option records membership. The inner option preserves the
    distinction between a valueless entry and a valued entry. *)
Definition Revision := Key -> option (option Value).

Inductive AlgebraOperation :=
  | Union
  | Intersection
  | Difference
  | SymmetricDifference.

Variable merge_values : option Value -> option Value -> option Value.

Definition algebra_at
  (operation : AlgebraOperation)
  (left right : Revision)
  (key : Key) : option (option Value) :=
  match operation, left key, right key with
  | Union, None, None => None
  | Union, Some left_value, None => Some left_value
  | Union, None, Some right_value => Some right_value
  | Union, Some left_value, Some right_value =>
      Some (merge_values left_value right_value)
  | Intersection, Some left_value, Some right_value =>
      Some (merge_values left_value right_value)
  | Intersection, _, _ => None
  | Difference, Some left_value, None => Some left_value
  | Difference, _, _ => None
  | SymmetricDifference, Some left_value, None => Some left_value
  | SymmetricDifference, None, Some right_value => Some right_value
  | SymmetricDifference, _, _ => None
  end.

Definition algebra_revision
  (operation : AlgebraOperation)
  (left right : Revision) : Revision :=
  fun key => algebra_at operation left right key.

Definition revise
  (revision : Revision)
  (key : Key)
  (value : option Value) : Revision :=
  fun candidate =>
    if key_eq_dec key candidate then Some value else revision candidate.

(** The generic key type is passed unchanged to both inputs. Consequently
    algebra cannot normalize, transcode, truncate, or otherwise rewrite a
    byte, Unicode-scalar, or u64-token key. *)
Theorem algebra_never_rewrites_keys : forall operation left right key,
  algebra_revision operation left right key =
  algebra_at operation left right key.
Proof.
  reflexivity.
Qed.

Theorem union_lookup : forall left right key,
  algebra_revision Union left right key =
  match left key, right key with
  | None, None => None
  | Some left_value, None => Some left_value
  | None, Some right_value => Some right_value
  | Some left_value, Some right_value =>
      Some (merge_values left_value right_value)
  end.
Proof.
  reflexivity.
Qed.

Theorem intersection_lookup : forall left right key,
  algebra_revision Intersection left right key =
  match left key, right key with
  | Some left_value, Some right_value =>
      Some (merge_values left_value right_value)
  | _, _ => None
  end.
Proof.
  reflexivity.
Qed.

Theorem difference_lookup : forall left right key,
  algebra_revision Difference left right key =
  match left key, right key with
  | Some left_value, None => Some left_value
  | _, _ => None
  end.
Proof.
  reflexivity.
Qed.

Theorem symmetric_difference_lookup : forall left right key,
  algebra_revision SymmetricDifference left right key =
  match left key, right key with
  | Some left_value, None => Some left_value
  | None, Some right_value => Some right_value
  | _, _ => None
  end.
Proof.
  reflexivity.
Qed.

Theorem absent_and_valueless_are_distinct :
  (@None (option Value)) <> Some None.
Proof.
  discriminate.
Qed.

(** A world separates mutable source heads from the independently published
    algebra result. *)
Record AlgebraWorld := mkAlgebraWorld {
  current_left : Revision;
  current_right : Revision;
  published_result : Revision
}.

Definition publish_algebra
  (operation : AlgebraOperation)
  (left right : Revision) : AlgebraWorld :=
  mkAlgebraWorld left right (algebra_revision operation left right).

Definition revise_left
  (world : AlgebraWorld)
  (key : Key)
  (value : option Value) : AlgebraWorld :=
  mkAlgebraWorld
    (revise (current_left world) key value)
    (current_right world)
    (published_result world).

Definition revise_right
  (world : AlgebraWorld)
  (key : Key)
  (value : option Value) : AlgebraWorld :=
  mkAlgebraWorld
    (current_left world)
    (revise (current_right world) key value)
    (published_result world).

Definition revise_result
  (world : AlgebraWorld)
  (key : Key)
  (value : option Value) : AlgebraWorld :=
  mkAlgebraWorld
    (current_left world)
    (current_right world)
    (revise (published_result world) key value).

Theorem later_left_mutation_preserves_result : forall world key value,
  published_result (revise_left world key value) = published_result world.
Proof.
  reflexivity.
Qed.

Theorem later_right_mutation_preserves_result : forall world key value,
  published_result (revise_right world key value) = published_result world.
Proof.
  reflexivity.
Qed.

Theorem result_mutation_preserves_sources : forall world key value,
  current_left (revise_result world key value) = current_left world /\
  current_right (revise_result world key value) = current_right world.
Proof.
  intros world key value.
  split; reflexivity.
Qed.

Theorem result_mutation_is_visible : forall world key value,
  published_result (revise_result world key value) key = Some value.
Proof.
  intros world key value.
  unfold revise_result, revise.
  simpl.
  destruct (key_eq_dec key key) as [_ | not_equal].
  - reflexivity.
  - exfalso. apply not_equal. reflexivity.
Qed.

End DictionaryAlgebra.
