(** * Packed residency cells, sparse logical deltas, and rollover

    This specification refines the finite-state helped-residency models to
    the intended 64-bit machine representation.  The low 32 bits carry a
    residency payload and the high 32 bits carry a generation-local ordinal.
    Exact full-cell compare-and-swap prevents delayed-helper ABA; ordinal
    exhaustion is handled by a fresh array identity, never by rejecting a
    consumer operation or constraining trie depth.

    A root revision stores only its sparse word delta plus exact aggregate
    totals.  The logical target is the functional application of that delta to
    its predecessor.  The final lemmas show that unmentioned words are
    preserved and the root's count/byte fields are definitionally the totals
    of the reconstructed logical target.
*)

From Coq Require Import Arith Bool Lia List PeanoNat.
Require Import ARTrie.Model.ListCompat.
Import ListNotations.

Module PackedResidencyRefinementSpec.

Definition payload_base : nat := Nat.pow 2 32.
Definition u32_max : nat := payload_base - 1.
Definition u64_capacity : nat := payload_base * payload_base.

Definition pack_cell (ordinal payload : nat) : nat :=
  ordinal * payload_base + payload.

Definition unpack_payload (cell : nat) : nat := cell mod payload_base.
Definition unpack_ordinal (cell : nat) : nat := cell / payload_base.

Lemma payload_base_positive : 0 < payload_base.
Proof.
  unfold payload_base.
  assert (2 ^ 32 <> 0) by (apply Nat.pow_nonzero; lia).
  lia.
Qed.

Theorem unpack_payload_pack :
  forall ordinal payload,
    payload < payload_base ->
    unpack_payload (pack_cell ordinal payload) = payload.
Proof.
  intros ordinal payload Hpayload.
  unfold unpack_payload, pack_cell.
  rewrite Nat.add_comm.
  rewrite Nat.Div0.mod_add.
  apply Nat.mod_small. exact Hpayload.
Qed.

Theorem unpack_ordinal_pack :
  forall ordinal payload,
    payload < payload_base ->
    unpack_ordinal (pack_cell ordinal payload) = ordinal.
Proof.
  intros ordinal payload Hpayload.
  unfold unpack_ordinal, pack_cell.
  rewrite Nat.div_add_l by (pose proof payload_base_positive; lia).
  rewrite Nat.div_small by exact Hpayload.
  lia.
Qed.

Theorem pack_cell_injective_in_range :
  forall left_ordinal left_payload right_ordinal right_payload,
    left_payload < payload_base ->
    right_payload < payload_base ->
    pack_cell left_ordinal left_payload =
      pack_cell right_ordinal right_payload ->
    left_ordinal = right_ordinal /\ left_payload = right_payload.
Proof.
  intros left_ordinal left_payload right_ordinal right_payload
         Hleft Hright Hequal.
  split.
  - pose proof (f_equal unpack_ordinal Hequal) as Hordinal.
    rewrite unpack_ordinal_pack in Hordinal by exact Hleft.
    rewrite unpack_ordinal_pack in Hordinal by exact Hright.
    exact Hordinal.
  - pose proof (f_equal unpack_payload Hequal) as Hpayload.
    rewrite unpack_payload_pack in Hpayload by exact Hleft.
    rewrite unpack_payload_pack in Hpayload by exact Hright.
    exact Hpayload.
Qed.

(** A sparse publication retags only the words that it changes.  Therefore a
    word used by a successor may legitimately retain the ordinal of the last
    earlier revision that changed that word.  Preparation accepts precisely
    such cells and rejects cells from a revision newer than the captured root.
    The exact packed cell, rather than the root ordinal alone, remains the CAS
    predecessor and consequently preserves delayed-helper ABA exclusion. *)
Definition prepare_successor_cell
    (captured_root_ordinal target_ordinal target_payload observed : nat)
    : option nat :=
  if unpack_ordinal observed <=? captured_root_ordinal
  then Some (pack_cell target_ordinal target_payload)
  else None.

Theorem sparse_preparation_accepts_last_modified_ordinal :
  forall last_modified_ordinal captured_root_ordinal target_ordinal
         observed_payload target_payload,
    last_modified_ordinal <= captured_root_ordinal ->
    observed_payload < payload_base ->
    prepare_successor_cell
      captured_root_ordinal target_ordinal target_payload
      (pack_cell last_modified_ordinal observed_payload) =
    Some (pack_cell target_ordinal target_payload).
Proof.
  intros last_modified_ordinal captured_root_ordinal target_ordinal
         observed_payload target_payload Hnot_newer Hpayload.
  unfold prepare_successor_cell.
  rewrite unpack_ordinal_pack by exact Hpayload.
  assert ((last_modified_ordinal <=? captured_root_ordinal) = true)
    as Hcomparison by (apply Nat.leb_le; exact Hnot_newer).
  rewrite Hcomparison.
  reflexivity.
Qed.

Theorem sparse_preparation_rejects_newer_cell :
  forall newer_ordinal captured_root_ordinal target_ordinal
         observed_payload target_payload,
    captured_root_ordinal < newer_ordinal ->
    observed_payload < payload_base ->
    prepare_successor_cell
      captured_root_ordinal target_ordinal target_payload
      (pack_cell newer_ordinal observed_payload) = None.
Proof.
  intros newer_ordinal captured_root_ordinal target_ordinal
         observed_payload target_payload Hnewer Hpayload.
  unfold prepare_successor_cell.
  rewrite unpack_ordinal_pack by exact Hpayload.
  assert ((newer_ordinal <=? captured_root_ordinal) = false)
    as Hcomparison by (apply Nat.leb_gt; exact Hnewer).
  rewrite Hcomparison.
  reflexivity.
Qed.

Theorem bounded_fields_fit_u64_capacity :
  forall ordinal payload,
    ordinal < payload_base ->
    payload < payload_base ->
    pack_cell ordinal payload < u64_capacity.
Proof.
  intros ordinal payload Hordinal Hpayload.
  unfold pack_cell, u64_capacity.
  nia.
Qed.

Record packed_generation : Type := mkPackedGeneration {
  generation_identity : nat;
  generation_cell : nat
}.

Record retained_helper : Type := mkRetainedHelper {
  helper_generation : nat;
  helper_expected : nat;
  helper_target : nat
}.

Definition apply_retained_helper
    (helper : retained_helper) (generation : packed_generation)
    : packed_generation :=
  if Nat.eq_dec (helper_generation helper) (generation_identity generation)
  then
    if Nat.eq_dec (generation_cell generation) (helper_expected helper)
    then mkPackedGeneration
           (generation_identity generation) (helper_target helper)
    else generation
  else generation.

Theorem retained_helper_cannot_touch_fresh_generation :
  forall helper generation,
    helper_generation helper <> generation_identity generation ->
    apply_retained_helper helper generation = generation.
Proof.
  intros helper generation Hdifferent.
  unfold apply_retained_helper.
  destruct (Nat.eq_dec (helper_generation helper)
                       (generation_identity generation));
    [contradiction | reflexivity].
Qed.

(** At the final generation-local ordinal, the requested operation is
    pre-materialized into a fresh array rather than rejected or wrapped in the
    retained array.  Payload application is pointwise and iterative in the
    implementation; this functional model states its exact result without
    imposing a bound on trie depth or consumer operation count. *)
Definition payload_words := nat -> nat.
Definition payload_delta := list (nat * nat).

Fixpoint apply_payload_delta
    (base : payload_words) (delta : payload_delta) (word : nat) : nat :=
  match delta with
  | [] => base word
  | (changed_word, target_payload) :: rest =>
      if Nat.eq_dec word changed_word
      then target_payload
      else apply_payload_delta base rest word
  end.

Definition fresh_rebased_cell
    (base : payload_words) (delta : payload_delta) (word : nat) : nat :=
  pack_cell 0 (apply_payload_delta base delta word).

Theorem fresh_rebase_tags_every_cell_at_zero :
  forall base delta word,
    apply_payload_delta base delta word < payload_base ->
    unpack_ordinal (fresh_rebased_cell base delta word) = 0.
Proof.
  intros base delta word Hrange.
  unfold fresh_rebased_cell.
  apply unpack_ordinal_pack.
  exact Hrange.
Qed.

Theorem fresh_rebase_payload_is_exact :
  forall base delta word,
    apply_payload_delta base delta word < payload_base ->
    unpack_payload (fresh_rebased_cell base delta word) =
    apply_payload_delta base delta word.
Proof.
  intros base delta word Hrange.
  unfold fresh_rebased_cell.
  apply unpack_payload_pack.
  exact Hrange.
Qed.

Theorem payload_delta_preserves_unmentioned_word :
  forall base delta word,
    ~ In word (map fst delta) ->
    apply_payload_delta base delta word = base word.
Proof.
  intros base delta.
  induction delta as [| [changed target] rest IH];
    intros word Hunmentioned.
  - reflexivity.
  - simpl in *.
    destruct (Nat.eq_dec word changed) as [Hequal | Hdifferent].
    + subst word. exfalso. apply Hunmentioned. left. reflexivity.
    + apply IH. intro Hin. apply Hunmentioned. right. exact Hin.
Qed.

Inductive ordinal_advance : Type :=
| SparseOrdinalAdvance (target_ordinal : nat)
| FreshGenerationAdvance.

Definition choose_ordinal_advance (current_ordinal : nat) : ordinal_advance :=
  if current_ordinal <? u32_max
  then SparseOrdinalAdvance (S current_ordinal)
  else FreshGenerationAdvance.

Theorem ordinal_advance_is_total_without_wrap_or_rejection :
  forall current_ordinal,
    current_ordinal <= u32_max ->
    (current_ordinal < u32_max /\
       choose_ordinal_advance current_ordinal =
       SparseOrdinalAdvance (S current_ordinal)) \/
    (current_ordinal = u32_max /\
       choose_ordinal_advance current_ordinal = FreshGenerationAdvance).
Proof.
  intros current_ordinal Hbounded.
  unfold choose_ordinal_advance.
  destruct (current_ordinal <? u32_max) eqn:Hcomparison.
  - left. split.
    + apply Nat.ltb_lt. exact Hcomparison.
    + reflexivity.
  - right. split.
    + apply Nat.ltb_ge in Hcomparison. lia.
    + reflexivity.
Qed.

Record rebase_candidate : Type := mkRebaseCandidate {
  rebase_expected_root : nat;
  rebase_target_root : nat;
  rebase_old_generation : nat;
  rebase_fresh_generation : nat
}.

Definition publish_rebase_candidate
    (observed_root : nat) (candidate : rebase_candidate)
    : option (nat * nat) :=
  if Nat.eq_dec observed_root (rebase_expected_root candidate)
  then Some
         (rebase_target_root candidate,
          rebase_fresh_generation candidate)
  else None.

Theorem exact_root_cas_publishes_rebase_winner :
  forall candidate,
    publish_rebase_candidate (rebase_expected_root candidate) candidate =
    Some
      (rebase_target_root candidate,
       rebase_fresh_generation candidate).
Proof.
  intros candidate.
  unfold publish_rebase_candidate.
  destruct (Nat.eq_dec
    (rebase_expected_root candidate)
    (rebase_expected_root candidate)); [reflexivity | contradiction].
Qed.

Theorem failed_exact_root_cas_cannot_publish_rebase_loser :
  forall observed_root candidate,
    observed_root <> rebase_expected_root candidate ->
    publish_rebase_candidate observed_root candidate = None.
Proof.
  intros observed_root candidate Hdifferent.
  unfold publish_rebase_candidate.
  destruct (Nat.eq_dec observed_root (rebase_expected_root candidate));
    [contradiction | reflexivity].
Qed.

Definition residency_bits := nat -> bool.
Definition serialized_sizes := nat -> nat.
Definition sparse_delta := list (nat * bool).

(** The packed eviction scanner intersects a topology-derived coverage mask
    with one atomically observed residency payload.  The intersection is both
    the exact set of records to enumerate and the exact mask cleared from that
    same predecessor cell.  Modeling the operation pointwise makes the
    refinement independent of the machine word width. *)
Definition covered_resident_bits
    (predecessor coverage : residency_bits) : residency_bits :=
  fun word => andb (predecessor word) (coverage word).

Definition clear_covered_bits
    (predecessor coverage : residency_bits) : residency_bits :=
  fun word => andb (predecessor word) (negb (coverage word)).

Fixpoint apply_sparse_delta
    (base : residency_bits) (delta : sparse_delta) (word : nat) : bool :=
  match delta with
  | [] => base word
  | (changed_word, target) :: rest =>
      if Nat.eq_dec word changed_word
      then target
      else apply_sparse_delta base rest word
  end.

Fixpoint resident_count
    (words : list nat) (bits : residency_bits) : nat :=
  match words with
  | [] => 0
  | word :: rest =>
      (if bits word then 1 else 0) + resident_count rest bits
  end.

Fixpoint resident_serialized_bytes
    (words : list nat) (sizes : serialized_sizes)
    (bits : residency_bits) : nat :=
  match words with
  | [] => 0
  | word :: rest =>
      (if bits word then sizes word else 0) +
      resident_serialized_bytes rest sizes bits
  end.

Theorem covered_resident_bits_are_exact :
  forall predecessor coverage word,
    covered_resident_bits predecessor coverage word = true <->
    predecessor word = true /\ coverage word = true.
Proof.
  intros predecessor coverage word.
  unfold covered_resident_bits.
  rewrite andb_true_iff.
  reflexivity.
Qed.

Theorem clear_covered_bits_are_exact :
  forall predecessor coverage word,
    clear_covered_bits predecessor coverage word = true <->
    predecessor word = true /\ coverage word = false.
Proof.
  intros predecessor coverage word.
  unfold clear_covered_bits.
  rewrite andb_true_iff, negb_true_iff.
  reflexivity.
Qed.

Theorem covered_clear_count_partition :
  forall words predecessor coverage,
    resident_count words predecessor =
      resident_count words (clear_covered_bits predecessor coverage) +
      resident_count words (covered_resident_bits predecessor coverage).
Proof.
  induction words as [| word rest IH]; intros predecessor coverage.
  - reflexivity.
  - simpl. unfold clear_covered_bits, covered_resident_bits in *.
    specialize (IH predecessor coverage).
    destruct (predecessor word), (coverage word); simpl in *; lia.
Qed.

Theorem covered_clear_bytes_partition :
  forall words sizes predecessor coverage,
    resident_serialized_bytes words sizes predecessor =
      resident_serialized_bytes words sizes
        (clear_covered_bits predecessor coverage) +
      resident_serialized_bytes words sizes
        (covered_resident_bits predecessor coverage).
Proof.
  induction words as [| word rest IH]; intros sizes predecessor coverage.
  - reflexivity.
  - simpl. unfold clear_covered_bits, covered_resident_bits in *.
    specialize (IH sizes predecessor coverage).
    destruct (predecessor word), (coverage word); simpl in *; lia.
Qed.

(** The implementation receives selected subtree intervals in dense preorder
    order.  It keeps one pending interval and emits that interval only when a
    strictly separated successor begins.  Nested, duplicate, overlapping, and
    adjacent intervals therefore collapse without a materialized range vector.
    The following refinement proves that this constant-space cursor preserves
    exactly the selected interval union. *)
Definition preorder_interval := (nat * nat)%type.

Definition valid_interval (range : preorder_interval) : Prop :=
  fst range < snd range.

Definition interval_contains (range : preorder_interval) (point : nat) : Prop :=
  fst range <= point /\ point < snd range.

Fixpoint interval_list_contains
    (ranges : list preorder_interval) (point : nat) : Prop :=
  match ranges with
  | [] => False
  | range :: rest =>
      interval_contains range point \/ interval_list_contains rest point
  end.

Fixpoint ordered_intervals_from
    (lower_start : nat) (ranges : list preorder_interval) : Prop :=
  match ranges with
  | [] => True
  | range :: rest =>
      lower_start <= fst range /\
      valid_interval range /\
      ordered_intervals_from (fst range) rest
  end.

Fixpoint merge_preorder_intervals_from
    (current : preorder_interval) (rest : list preorder_interval)
    : list preorder_interval :=
  match rest with
  | [] => [current]
  | next :: tail =>
      if fst next <=? snd current
      then merge_preorder_intervals_from
             (fst current, Nat.max (snd current) (snd next)) tail
      else current :: merge_preorder_intervals_from next tail
  end.

Definition merge_preorder_intervals
    (ranges : list preorder_interval) : list preorder_interval :=
  match ranges with
  | [] => []
  | current :: rest => merge_preorder_intervals_from current rest
  end.

Lemma ordered_intervals_lower_bound_can_weaken :
  forall ranges lower stronger,
    lower <= stronger ->
    ordered_intervals_from stronger ranges ->
    ordered_intervals_from lower ranges.
Proof.
  destruct ranges as [| [start finish] rest];
    intros lower stronger Hlower Hordered; simpl in *.
  - exact I.
  - destruct Hordered as [Hstronger [Hvalid Hrest]].
    repeat split; try lia; assumption.
Qed.

Lemma overlapping_interval_merge_is_exact :
  forall left_start left_end right_start right_end point,
    left_start < left_end ->
    right_start < right_end ->
    left_start <= right_start ->
    right_start <= left_end ->
    interval_contains
      (left_start, Nat.max left_end right_end) point <->
    interval_contains (left_start, left_end) point \/
    interval_contains (right_start, right_end) point.
Proof.
  intros left_start left_end right_start right_end point
         Hleft Hright Hordered Hoverlap.
  unfold interval_contains; simpl.
  split.
  - intros [Hstart Hmaximum].
    destruct (lt_dec point left_end) as [Hin_left | Hnot_left].
    + left. lia.
    + right. split; [lia |].
      destruct (Nat.le_ge_cases left_end right_end) as [Hends | Hends].
      * rewrite Nat.max_r in Hmaximum by exact Hends.
        exact Hmaximum.
      * rewrite Nat.max_l in Hmaximum by exact Hends.
        lia.
  - intros [[Hstart Hin_left] | [Hin_right_start Hin_right]].
    + split; [exact Hstart |].
      eapply Nat.lt_le_trans; [exact Hin_left | apply Nat.le_max_l].
    + split; [lia |].
      eapply Nat.lt_le_trans; [exact Hin_right | apply Nat.le_max_r].
Qed.

Theorem merge_preorder_intervals_from_preserves_union :
  forall rest current point,
    valid_interval current ->
    ordered_intervals_from (fst current) rest ->
    interval_list_contains
      (merge_preorder_intervals_from current rest) point <->
    interval_contains current point \/ interval_list_contains rest point.
Proof.
  induction rest as [| next tail IH];
    intros current point Hcurrent Hordered.
  - simpl. tauto.
  - simpl in Hordered.
    destruct Hordered as [Hstart [Hnext Htail]].
    simpl.
    destruct (fst next <=? snd current) eqn:Hoverlap.
    + apply Nat.leb_le in Hoverlap.
      assert (Hmerged_valid :
        valid_interval
          (fst current, Nat.max (snd current) (snd next))).
      {
        unfold valid_interval in *; simpl in *.
        eapply Nat.lt_le_trans; [exact Hcurrent | apply Nat.le_max_l].
      }
      assert (Hmerged_ordered :
        ordered_intervals_from (fst current) tail).
      {
        eapply ordered_intervals_lower_bound_can_weaken;
          [exact Hstart | exact Htail].
      }
      rewrite (IH
        (fst current, Nat.max (snd current) (snd next))
        point Hmerged_valid Hmerged_ordered).
      destruct current as [left_start left_end].
      destruct next as [right_start right_end].
      simpl in *.
      rewrite (overlapping_interval_merge_is_exact
        left_start left_end right_start right_end point)
        by assumption.
      tauto.
    + apply Nat.leb_gt in Hoverlap.
      change
        (interval_contains current point \/
           interval_list_contains
             (merge_preorder_intervals_from next tail) point <->
         interval_contains current point \/
           (interval_contains next point \/
            interval_list_contains tail point)).
      rewrite (IH next point Hnext Htail).
      tauto.
Qed.

Theorem merge_preorder_intervals_preserves_union :
  forall ranges point,
    match ranges with
    | [] => True
    | current :: rest =>
        valid_interval current /\
        ordered_intervals_from (fst current) rest
    end ->
    interval_list_contains (merge_preorder_intervals ranges) point <->
    interval_list_contains ranges point.
Proof.
  destruct ranges as [| current rest]; intros point Hordered.
  - simpl. tauto.
  - simpl in Hordered |- *.
    destruct Hordered as [Hcurrent Hrest].
    apply merge_preorder_intervals_from_preserves_union;
      assumption.
Qed.

(** The transition accumulator stores zero and one transitions inline.  From
    the second transition onward it requests geometrically growing heap
    capacity.  Capacity is an allocation policy only: it cannot change the
    accumulated transition sequence.  With an exact-capacity allocator the
    requested heap capacity is always less than twice the live transition
    count, excluding both semantic-size and trie-depth caps. *)
Inductive transition_builder : Type :=
| BuilderEmpty
| BuilderOne (transition : nat)
| BuilderMany (capacity : nat) (transitions : list nat).

Definition builder_contents (builder : transition_builder) : list nat :=
  match builder with
  | BuilderEmpty => []
  | BuilderOne transition => [transition]
  | BuilderMany _ transitions => transitions
  end.

Definition append_transition
    (builder : transition_builder) (transition : nat)
    : transition_builder :=
  match builder with
  | BuilderEmpty => BuilderOne transition
  | BuilderOne first => BuilderMany 2 [first; transition]
  | BuilderMany capacity transitions =>
      let required := S (length transitions) in
      let target_capacity :=
        if required <=? capacity then capacity else 2 * capacity in
      BuilderMany target_capacity (transitions ++ [transition])
  end.

Definition builder_capacity_invariant (builder : transition_builder) : Prop :=
  match builder with
  | BuilderEmpty => True
  | BuilderOne _ => True
  | BuilderMany capacity transitions =>
      2 <= length transitions /\
      length transitions <= capacity /\
      capacity < 2 * length transitions
  end.

Theorem append_transition_preserves_contents :
  forall builder transition,
    builder_contents (append_transition builder transition) =
    builder_contents builder ++ [transition].
Proof.
  destruct builder as [| first | capacity transitions];
    intros transition; simpl.
  - reflexivity.
  - reflexivity.
  - destruct (S (length transitions) <=? capacity); reflexivity.
Qed.

Theorem append_transition_preserves_capacity_invariant :
  forall builder transition,
    builder_capacity_invariant builder ->
    builder_capacity_invariant (append_transition builder transition).
Proof.
  destruct builder as [| first | capacity transitions];
    intros transition Hinvariant.
  - simpl. exact I.
  - simpl. lia.
  - unfold builder_capacity_invariant in Hinvariant.
    destruct Hinvariant as [Htwo [Hfits Hlinear]].
    destruct capacity as [| capacity].
    { lia. }
    change
      (builder_capacity_invariant
        (BuilderMany
          (if length transitions <=? capacity
           then S capacity
           else 2 * S capacity)
          (transitions ++ [transition]))).
    destruct (length transitions <=? capacity) eqn:Hcapacity;
      unfold builder_capacity_invariant;
      rewrite app_length_portable; simpl.
    + apply Nat.leb_le in Hcapacity.
      repeat split; lia.
    + apply Nat.leb_gt in Hcapacity.
      assert (Hequal : S capacity = length transitions) by lia.
      repeat split; lia.
Qed.

Fixpoint append_all_transitions
    (builder : transition_builder) (transitions : list nat)
    : transition_builder :=
  match transitions with
  | [] => builder
  | transition :: rest =>
      append_all_transitions (append_transition builder transition) rest
  end.

Theorem append_all_transitions_preserves_contents :
  forall transitions builder,
    builder_contents (append_all_transitions builder transitions) =
    builder_contents builder ++ transitions.
Proof.
  induction transitions as [| transition rest IH]; intros builder.
  - simpl. rewrite app_nil_r. reflexivity.
  - simpl. rewrite IH, append_transition_preserves_contents.
    rewrite <- app_assoc.
    reflexivity.
Qed.

Theorem geometric_builder_requested_capacity_is_strictly_linear :
  forall capacity transitions,
    builder_capacity_invariant (BuilderMany capacity transitions) ->
    capacity < 2 * length transitions.
Proof.
  intros capacity transitions [_ [_ Hlinear]].
  exact Hlinear.
Qed.

Record sparse_root_revision : Type := mkSparseRootRevision {
  sparse_root_bits : residency_bits;
  sparse_root_resident_count : nat;
  sparse_root_resident_bytes : nat
}.

Definition publish_sparse_revision
    (words : list nat) (sizes : serialized_sizes)
    (predecessor : residency_bits) (delta : sparse_delta)
    : sparse_root_revision :=
  let target := apply_sparse_delta predecessor delta in
  mkSparseRootRevision
    target
    (resident_count words target)
    (resident_serialized_bytes words sizes target).

Theorem sparse_delta_preserves_unmentioned_word :
  forall predecessor delta word,
    ~ In word (map fst delta) ->
    apply_sparse_delta predecessor delta word = predecessor word.
Proof.
  intros predecessor delta.
  induction delta as [| [changed target] rest IH]; intros word Hunmentioned.
  - reflexivity.
  - simpl in *.
    destruct (Nat.eq_dec word changed) as [Hequal | Hdifferent].
    + subst word. exfalso. apply Hunmentioned. left. reflexivity.
    + apply IH. intro Hin. apply Hunmentioned. right. exact Hin.
Qed.

Theorem sparse_root_count_is_exact :
  forall words sizes predecessor delta,
    sparse_root_resident_count
      (publish_sparse_revision words sizes predecessor delta) =
    resident_count words (apply_sparse_delta predecessor delta).
Proof.
  reflexivity.
Qed.

Theorem sparse_root_bytes_are_exact :
  forall words sizes predecessor delta,
    sparse_root_resident_bytes
      (publish_sparse_revision words sizes predecessor delta) =
    resident_serialized_bytes
      words sizes (apply_sparse_delta predecessor delta).
Proof.
  reflexivity.
Qed.

(** A prepared rollover value is produced only by applying its requested delta
    to the stable predecessor.  The Rust representation mirrors this smart
    constructor with private variant payloads; root publication consumes the
    opaque prepared value rather than accepting caller-supplied target cells. *)
Record sealed_rebase_preparation : Type := mkSealedRebasePreparation {
  sealed_predecessor_payload : payload_words;
  sealed_requested_delta : payload_delta;
  sealed_target_payload : payload_words
}.

Definition prepare_sealed_rebase
    (predecessor : payload_words) (delta : payload_delta)
    : sealed_rebase_preparation :=
  mkSealedRebasePreparation
    predecessor delta (apply_payload_delta predecessor delta).

Theorem sealed_rebase_target_is_exact :
  forall predecessor delta word,
    sealed_target_payload (prepare_sealed_rebase predecessor delta) word =
    apply_payload_delta predecessor delta word.
Proof.
  reflexivity.
Qed.

(** Published catalogs previously retained metadata that no exact selection,
    preparation, helping, or publication operation observed.  Erasing such
    fields is observationally exact and reduces both retained space and
    rollover cloning. *)
Record full_catalog_model : Type := mkFullCatalogModel {
  catalog_observed_authority : nat;
  catalog_observed_structure : list nat;
  catalog_observed_residency : list nat;
  catalog_unobserved_metadata : list nat
}.

Record operational_catalog_model : Type := mkOperationalCatalogModel {
  operational_authority : nat;
  operational_structure : list nat;
  operational_residency : list nat
}.

Definition erase_unobserved_catalog_metadata
    (catalog : full_catalog_model) : operational_catalog_model :=
  mkOperationalCatalogModel
    (catalog_observed_authority catalog)
    (catalog_observed_structure catalog)
    (catalog_observed_residency catalog).

Definition observe_full_catalog
    (catalog : full_catalog_model) : nat * list nat * list nat :=
  (catalog_observed_authority catalog,
   catalog_observed_structure catalog,
   catalog_observed_residency catalog).

Definition observe_operational_catalog
    (catalog : operational_catalog_model) : nat * list nat * list nat :=
  (operational_authority catalog,
   operational_structure catalog,
   operational_residency catalog).

Theorem unobserved_catalog_metadata_erasure_is_exact :
  forall catalog,
    observe_operational_catalog
      (erase_unobserved_catalog_metadata catalog) =
    observe_full_catalog catalog.
Proof.
  intros catalog.
  destruct catalog.
  reflexivity.
Qed.

(** Rollover dispatch has one exhaustion guard.  Every bounded non-maximum
    ordinal follows the straight-line sparse preparation; the outlined fresh
    builder is unreachable on that ordinary path. *)
Inductive preparation_path : Type :=
| SparsePreparationPath
| FreshPreparationPath.

Definition choose_preparation_path (current_ordinal : nat) : preparation_path :=
  if Nat.eq_dec current_ordinal u32_max
  then FreshPreparationPath
  else SparsePreparationPath.

Theorem nonmaximum_ordinal_uses_only_sparse_preparation :
  forall current_ordinal,
    current_ordinal < u32_max ->
    choose_preparation_path current_ordinal = SparsePreparationPath.
Proof.
  intros current_ordinal Hless.
  unfold choose_preparation_path.
  destruct (Nat.eq_dec current_ordinal u32_max) as [Hequal | Hdifferent].
  - subst current_ordinal. lia.
  - reflexivity.
Qed.

End PackedResidencyRefinementSpec.
