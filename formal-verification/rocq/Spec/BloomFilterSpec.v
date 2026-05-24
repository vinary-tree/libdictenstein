(** * BloomFilterSpec: No-False-Negative Lookup Rejection

    This module states the semantic proof boundary for libdictenstein's public
    [BloomFilter] API.  The correctness claim is one-sided: once a byte string
    has been inserted, [might_contain] must not reject it.  False positives are
    allowed and are intentionally outside the safety property.

    The Rust implementation uses [FxHasher] and stores bits in [u64] chunks.
    This model abstracts over hash quality and chunk layout, retaining only the
    obligations needed for safe dictionary lookup rejection.
*)

From Stdlib Require Import Lists.List.
From Stdlib Require Import Bool.Bool.
From Stdlib Require Import Arith.PeanoNat.
From Stdlib Require Import micromega.Lia.
Require Import ARTrie.Spec.MapSpec.
Import ListNotations.

Definition BloomBytes := MapSpec.Key.
Definition BitIndex := nat.
Definition HashSeed := nat.
Definition Bitset := BitIndex -> bool.
Definition HashPositions := BloomBytes -> HashSeed -> BitIndex.

Definition min_bit_count : nat := 64.
Definition default_hash_count : nat := 3.

Definition normalize_bit_count (requested : nat) : nat :=
  Nat.max min_bit_count requested.

Definition normalize_hash_count (requested : nat) : nat :=
  Nat.max 1 requested.

Record BloomParams := {
  bloom_bit_count : nat;
  bloom_hash_count : nat;
  bloom_bit_count_min : min_bit_count <= bloom_bit_count;
  bloom_hash_count_nonzero : 1 <= bloom_hash_count
}.

Definition bloom_with_params (requested_bits requested_hashes : nat)
  : BloomParams.
Proof.
  refine {|
    bloom_bit_count := normalize_bit_count requested_bits;
    bloom_hash_count := normalize_hash_count requested_hashes
  |}.
  - unfold normalize_bit_count.
    apply Nat.le_max_l.
  - unfold normalize_hash_count.
    apply Nat.le_max_l.
Defined.

Definition bloom_new_params (expected_elements : nat) : BloomParams :=
  bloom_with_params (expected_elements * 10) default_hash_count.

Definition empty_bits : Bitset := fun _ => false.

Definition set_bit (bits : Bitset) (index : BitIndex) : Bitset :=
  fun query => if Nat.eq_dec index query then true else bits query.

Definition bit_is_set (bits : Bitset) (index : BitIndex) : bool :=
  bits index.

Fixpoint set_hash_positions
  (fuel : nat)
  (hash : HashPositions)
  (bytes : BloomBytes)
  (seed : HashSeed)
  (bits : Bitset)
  : Bitset :=
  match fuel with
  | 0 => bits
  | S rest =>
      set_hash_positions rest hash bytes (S seed)
        (set_bit bits (hash bytes seed))
  end.

Fixpoint all_hash_positions_set
  (fuel : nat)
  (hash : HashPositions)
  (bytes : BloomBytes)
  (seed : HashSeed)
  (bits : Bitset)
  : bool :=
  match fuel with
  | 0 => true
  | S rest =>
      bit_is_set bits (hash bytes seed) &&
      all_hash_positions_set rest hash bytes (S seed) bits
  end.

Definition bloom_insert
  (params : BloomParams)
  (hash : HashPositions)
  (bytes : BloomBytes)
  (bits : Bitset)
  : Bitset :=
  set_hash_positions (bloom_hash_count params) hash bytes 0 bits.

Definition bloom_might_contain
  (params : BloomParams)
  (hash : HashPositions)
  (bytes : BloomBytes)
  (bits : Bitset)
  : bool :=
  all_hash_positions_set (bloom_hash_count params) hash bytes 0 bits.

Definition bloom_clear (_ : Bitset) : Bitset := empty_bits.

Lemma set_bit_marks_index : forall bits index,
  bit_is_set (set_bit bits index) index = true.
Proof.
  intros bits index.
  unfold bit_is_set, set_bit.
  destruct (Nat.eq_dec index index) as [_ | Hneq].
  - reflexivity.
  - exfalso. apply Hneq. reflexivity.
Qed.

Lemma set_bit_preserves_set : forall bits set_index query,
  bit_is_set bits query = true ->
  bit_is_set (set_bit bits set_index) query = true.
Proof.
  intros bits set_index query Hset.
  unfold bit_is_set, set_bit.
  destruct (Nat.eq_dec set_index query) as [_ | _].
  - reflexivity.
  - exact Hset.
Qed.

Lemma set_hash_positions_preserves_set :
  forall fuel hash bytes seed bits query,
    bit_is_set bits query = true ->
    bit_is_set
      (set_hash_positions fuel hash bytes seed bits)
      query = true.
Proof.
  induction fuel as [| rest IH]; intros hash bytes seed bits query Hset.
  - exact Hset.
  - simpl.
    apply IH.
    apply set_bit_preserves_set.
    exact Hset.
Qed.

Lemma set_hash_positions_marks_all :
  forall fuel hash bytes seed bits,
    all_hash_positions_set fuel hash bytes seed
      (set_hash_positions fuel hash bytes seed bits) = true.
Proof.
  induction fuel as [| rest IH]; intros hash bytes seed bits.
  - reflexivity.
  - simpl.
    rewrite (set_hash_positions_preserves_set
      rest hash bytes (S seed)
      (set_bit bits (hash bytes seed))
      (hash bytes seed)).
    + rewrite IH.
      reflexivity.
    + apply set_bit_marks_index.
Qed.

Theorem bloom_insert_no_false_negative : forall params hash bytes bits,
  bloom_might_contain params hash bytes
    (bloom_insert params hash bytes bits) = true.
Proof.
  intros params hash bytes bits.
  unfold bloom_might_contain, bloom_insert.
  apply set_hash_positions_marks_all.
Qed.

Lemma all_hash_positions_set_preserved :
  forall fuel hash probe seed bits bits',
    (forall query,
      bit_is_set bits query = true ->
      bit_is_set bits' query = true) ->
    all_hash_positions_set fuel hash probe seed bits = true ->
    all_hash_positions_set fuel hash probe seed bits' = true.
Proof.
  induction fuel as [| rest IH]; intros hash probe seed bits bits' Hpreserve Hall.
  - reflexivity.
  - simpl in Hall.
    apply andb_true_iff in Hall as [Hhead Htail].
    simpl.
    apply andb_true_iff.
    split.
    + apply Hpreserve.
      exact Hhead.
    + apply IH with (bits := bits).
      * exact Hpreserve.
      * exact Htail.
Qed.

Theorem bloom_insert_preserves_prior_membership :
  forall params hash inserted probe bits,
    bloom_might_contain params hash probe bits = true ->
    bloom_might_contain params hash probe
      (bloom_insert params hash inserted bits) = true.
Proof.
  intros params hash inserted probe bits Hmember.
  unfold bloom_might_contain, bloom_insert in *.
  apply all_hash_positions_set_preserved with (bits := bits).
  - intros query Hset.
    apply set_hash_positions_preserves_set.
    exact Hset.
  - exact Hmember.
Qed.

Theorem bloom_duplicate_insert_no_false_negative :
  forall params hash bytes bits,
    bloom_might_contain params hash bytes
      (bloom_insert params hash bytes
        (bloom_insert params hash bytes bits)) = true.
Proof.
  intros params hash bytes bits.
  apply bloom_insert_no_false_negative.
Qed.

Theorem bloom_clear_rejects_all :
  forall params hash bytes bits,
    bloom_might_contain params hash bytes (bloom_clear bits) = false.
Proof.
  intros [bit_count hash_count Hbits Hhashes] hash bytes bits.
  unfold bloom_might_contain, bloom_clear.
  simpl.
  destruct hash_count as [| rest].
  - lia.
  - reflexivity.
Qed.

Definition no_false_negatives
  (params : BloomParams)
  (hash : HashPositions)
  (inserted : list BloomBytes)
  (bits : Bitset)
  : Prop :=
  forall bytes,
    In bytes inserted ->
    bloom_might_contain params hash bytes bits = true.

Theorem no_false_negatives_after_insert :
  forall params hash inserted bytes bits,
    no_false_negatives params hash inserted bits ->
    no_false_negatives params hash (bytes :: inserted)
      (bloom_insert params hash bytes bits).
Proof.
  intros params hash inserted bytes bits Hsafe query Hin.
  simpl in Hin.
  destruct Hin as [Heq | Hprior].
  - subst.
    apply bloom_insert_no_false_negative.
  - apply bloom_insert_preserves_prior_membership.
    apply Hsafe.
    exact Hprior.
Qed.

Lemma bloom_insert_trace_preserves_membership :
  forall terms params hash probe bits,
    bloom_might_contain params hash probe bits = true ->
    bloom_might_contain params hash probe
      (fold_left
        (fun acc term => bloom_insert params hash term acc)
        terms
        bits) = true.
Proof.
  induction terms as [| head rest IH]; intros params hash probe bits Hmember.
  - exact Hmember.
  - simpl.
    apply IH.
    apply bloom_insert_preserves_prior_membership.
    exact Hmember.
Qed.

Theorem no_false_negatives_after_insert_trace :
  forall terms params hash bits,
    no_false_negatives params hash terms
      (fold_left
        (fun acc term => bloom_insert params hash term acc)
        terms
        bits).
Proof.
  induction terms as [| head rest IH]; intros params hash bits query Hin.
  - contradiction.
  - simpl.
    simpl in Hin.
    destruct Hin as [Heq | Hrest].
    + subst.
      apply bloom_insert_trace_preserves_membership.
      apply bloom_insert_no_false_negative.
    + apply IH.
      exact Hrest.
Qed.

Theorem false_positives_do_not_violate_no_false_negatives :
  forall params hash inserted bits probe,
    no_false_negatives params hash inserted bits ->
    ~ In probe inserted ->
    bloom_might_contain params hash probe bits = true ->
    no_false_negatives params hash inserted bits.
Proof.
  intros params hash inserted bits probe Hsafe _ _.
  exact Hsafe.
Qed.

Definition StringPayload := BloomBytes.

Definition string_to_bytes (payload : StringPayload) : BloomBytes := payload.

Definition bloom_insert_string
  (params : BloomParams)
  (hash : HashPositions)
  (payload : StringPayload)
  (bits : Bitset)
  : Bitset :=
  bloom_insert params hash (string_to_bytes payload) bits.

Definition bloom_might_contain_string
  (params : BloomParams)
  (hash : HashPositions)
  (payload : StringPayload)
  (bits : Bitset)
  : bool :=
  bloom_might_contain params hash (string_to_bytes payload) bits.

Theorem string_insert_refines_bytes : forall params hash payload bits,
  bloom_insert_string params hash payload bits =
  bloom_insert params hash (string_to_bytes payload) bits.
Proof.
  reflexivity.
Qed.

Theorem string_might_contain_refines_bytes : forall params hash payload bits,
  bloom_might_contain_string params hash payload bits =
  bloom_might_contain params hash (string_to_bytes payload) bits.
Proof.
  reflexivity.
Qed.

Theorem bloom_new_params_nonvacuous : forall expected_elements,
  min_bit_count <= bloom_bit_count (bloom_new_params expected_elements) /\
  1 <= bloom_hash_count (bloom_new_params expected_elements).
Proof.
  intro expected_elements.
  split.
  - apply bloom_bit_count_min.
  - apply bloom_hash_count_nonzero.
Qed.

Theorem bloom_with_params_nonvacuous :
  forall requested_bits requested_hashes,
    min_bit_count <=
      bloom_bit_count (bloom_with_params requested_bits requested_hashes) /\
    1 <= bloom_hash_count
      (bloom_with_params requested_bits requested_hashes).
Proof.
  intros requested_bits requested_hashes.
  split.
  - apply bloom_bit_count_min.
  - apply bloom_hash_count_nonzero.
Qed.
