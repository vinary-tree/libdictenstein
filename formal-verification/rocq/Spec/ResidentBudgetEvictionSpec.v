(** * Exact resident-budget closure selection and ancestor-first execution

    The production resident-budget selector operates over a finite path topology
    stored in preorder.  It ranks durable resident anchors, assigns every
    resident record to the earliest-ranked selected ancestor on its root-to-node
    path, and admits the shortest positive-gain rank prefix that reaches the
    requested byte target (subject to an anchor cap).  Its executor then checks
    selected endpoints in structural preorder: an exact ancestor replaces its
    complete durable subtree, while a stale ancestor falls through to exact
    descendants.

    This file proves the unbounded mathematical core of that protocol.  Paths
    are finite lists of edge labels and resident weights are natural numbers.
    The closure partition is constructive: each rank removes its subtree from
    the remaining records before the next rank is evaluated.  Consequently the
    buckets are disjoint, their flattened weight is exactly the selected subtree
    union, and every nonempty bucket has positive gain when record weights are
    positive.  The proofs do not recurse over trie depth; path depth is data.
*)

From Stdlib Require Import Arith Bool Lia List PeanoNat.
Import ListNotations.

Module ResidentBudgetEvictionSpec.

Definition Path := list nat.

Fixpoint path_prefixb (ancestor descendant : Path) : bool :=
  match ancestor, descendant with
  | [], _ => true
  | _, [] => false
  | a :: ancestor', d :: descendant' =>
      Nat.eqb a d && path_prefixb ancestor' descendant'
  end.

Definition path_prefix (ancestor descendant : Path) : Prop :=
  exists suffix, descendant = ancestor ++ suffix.

Lemma path_prefixb_spec :
  forall ancestor descendant,
    path_prefixb ancestor descendant = true <->
    path_prefix ancestor descendant.
Proof.
  induction ancestor as [| a ancestor IH]; intros descendant.
  - split.
    + intros _. exists descendant. reflexivity.
    + intros _. reflexivity.
  - destruct descendant as [| d descendant]; simpl.
    + split; [discriminate | intros [suffix H]; discriminate H].
    + rewrite Bool.andb_true_iff, Nat.eqb_eq, IH.
      split.
      * intros [Heq [suffix Hsuffix]].
        subst d. exists suffix. simpl. f_equal. exact Hsuffix.
      * intros [suffix Hsuffix].
        simpl in Hsuffix. injection Hsuffix as Heq Htail.
        split; [symmetry; exact Heq | exists suffix; exact Htail].
Qed.

Lemma path_prefix_refl : forall path, path_prefix path path.
Proof.
  intros path. exists []. rewrite app_nil_r. reflexivity.
Qed.

Lemma path_prefix_transitive :
  forall first middle last,
    path_prefix first middle ->
    path_prefix middle last ->
    path_prefix first last.
Proof.
  intros first middle last [left Hleft] [right Hright].
  subst middle last. exists (left ++ right). rewrite app_assoc. reflexivity.
Qed.

Record ResidentRecord : Type := mkResidentRecord {
  record_path : Path;
  record_weight : nat
}.

(** These are precisely the two structural properties supplied by the
    production [PathTopology]: ancestors precede descendants, and a subtree is
    one contiguous preorder interval.  [finite_records] is a list, so finiteness
    is intrinsic rather than postulated. *)
Record FinitePreorder : Type := mkFinitePreorder {
  finite_records : list ResidentRecord;
  finite_positive_weights :
    Forall (fun record => 0 < record_weight record) finite_records;
  finite_ancestor_before_descendant :
    forall ancestor_index descendant_index ancestor descendant,
      nth_error finite_records ancestor_index = Some ancestor ->
      nth_error finite_records descendant_index = Some descendant ->
      path_prefix (record_path ancestor) (record_path descendant) ->
      record_path ancestor <> record_path descendant ->
      ancestor_index < descendant_index;
  finite_subtree_contiguous :
    forall ancestor_index middle_index descendant_index
           ancestor middle descendant,
      nth_error finite_records ancestor_index = Some ancestor ->
      nth_error finite_records middle_index = Some middle ->
      nth_error finite_records descendant_index = Some descendant ->
      ancestor_index < middle_index ->
      middle_index < descendant_index ->
      path_prefix (record_path ancestor) (record_path descendant) ->
      path_prefix (record_path ancestor) (record_path middle)
}.

Definition select_subtree
    (anchor : Path) (records : list ResidentRecord) : list ResidentRecord :=
  filter (fun record => path_prefixb anchor (record_path record)) records.

Definition reject_subtree
    (anchor : Path) (records : list ResidentRecord) : list ResidentRecord :=
  filter (fun record => negb (path_prefixb anchor (record_path record))) records.

Fixpoint closure_partition
    (ranked_anchors : list Path) (remaining : list ResidentRecord)
    : list (list ResidentRecord) :=
  match ranked_anchors with
  | [] => []
  | anchor :: rest =>
      select_subtree anchor remaining ::
      closure_partition rest (reject_subtree anchor remaining)
  end.

Fixpoint unassigned_records
    (ranked_anchors : list Path) (remaining : list ResidentRecord)
    : list ResidentRecord :=
  match ranked_anchors with
  | [] => remaining
  | anchor :: rest =>
      unassigned_records rest (reject_subtree anchor remaining)
  end.

Fixpoint records_weight (records : list ResidentRecord) : nat :=
  match records with
  | [] => 0
  | record :: rest => record_weight record + records_weight rest
  end.

Definition closure_gains
    (ranked_anchors : list Path) (records : list ResidentRecord) : list nat :=
  map records_weight (closure_partition ranked_anchors records).

Definition planned_weight
    (ranked_anchors : list Path) (records : list ResidentRecord) : nat :=
  records_weight (concat (closure_partition ranked_anchors records)).

Fixpoint nat_sum (values : list nat) : nat :=
  match values with
  | [] => 0
  | value :: rest => value + nat_sum rest
  end.

Lemma records_weight_app :
  forall left right,
    records_weight (left ++ right) =
    records_weight left + records_weight right.
Proof.
  induction left as [| record rest IH]; intros right; simpl.
  - reflexivity.
  - rewrite IH. lia.
Qed.

Lemma filter_weight_partition :
  forall anchor records,
    records_weight (select_subtree anchor records) +
    records_weight (reject_subtree anchor records) =
    records_weight records.
Proof.
  intros anchor records.
  induction records as [| record rest IH]; simpl.
  - reflexivity.
  - destruct (path_prefixb anchor (record_path record)) eqn:Hprefix;
      simpl in *; lia.
Qed.

Lemma concat_partition_weight :
  forall ranked_anchors records,
    records_weight (concat (closure_partition ranked_anchors records)) =
    nat_sum (closure_gains ranked_anchors records).
Proof.
  induction ranked_anchors as [| anchor rest IH]; intros records; simpl.
  - reflexivity.
  - rewrite records_weight_app, IH. reflexivity.
Qed.

Theorem closure_partition_exact :
  forall ranked_anchors records,
    planned_weight ranked_anchors records +
    records_weight (unassigned_records ranked_anchors records) =
    records_weight records.
Proof.
  induction ranked_anchors as [| anchor rest IH]; intros records; simpl.
  - lia.
  - unfold planned_weight in *; simpl.
    rewrite records_weight_app.
    specialize (IH (reject_subtree anchor records)).
    pose proof (filter_weight_partition anchor records) as Hsplit.
    lia.
Qed.

Corollary finite_preorder_closure_partition_exact :
  forall topology ranked_anchors,
    planned_weight ranked_anchors (finite_records topology) +
    records_weight
      (unassigned_records ranked_anchors (finite_records topology)) =
    records_weight (finite_records topology).
Proof.
  intros topology ranked_anchors.
  apply closure_partition_exact.
Qed.

Corollary planned_weight_is_sum_of_exact_closure_gains :
  forall ranked_anchors records,
    planned_weight ranked_anchors records =
    nat_sum (closure_gains ranked_anchors records).
Proof.
  intros ranked_anchors records.
  unfold planned_weight. apply concat_partition_weight.
Qed.

Definition covered_by (ranked_anchors : list Path) (record : ResidentRecord) : Prop :=
  exists anchor,
    In anchor ranked_anchors /\
    path_prefix anchor (record_path record).

Lemma select_subtree_spec :
  forall anchor records record,
    In record (select_subtree anchor records) <->
    In record records /\ path_prefix anchor (record_path record).
Proof.
  intros anchor records record.
  unfold select_subtree.
  rewrite filter_In, path_prefixb_spec.
  reflexivity.
Qed.

Lemma reject_subtree_spec :
  forall anchor records record,
    In record (reject_subtree anchor records) <->
    In record records /\ ~ path_prefix anchor (record_path record).
Proof.
  intros anchor records record.
  unfold reject_subtree.
  rewrite filter_In, Bool.negb_true_iff.
  split.
  - intros [Hin Hfalse]. split; [exact Hin |].
    intro Hprefix. apply path_prefixb_spec in Hprefix.
    rewrite Hprefix in Hfalse. discriminate.
  - intros [Hin Hnot]. split; [exact Hin |].
    destruct (path_prefixb anchor (record_path record)) eqn:Hprefix.
    + apply path_prefixb_spec in Hprefix. contradiction.
    + reflexivity.
Qed.

Lemma concat_partition_sound :
  forall ranked_anchors records record,
    In record (concat (closure_partition ranked_anchors records)) ->
    In record records /\ covered_by ranked_anchors record.
Proof.
  induction ranked_anchors as [| anchor rest IH]; intros records record Hin; simpl in Hin.
  - contradiction.
  - apply in_app_iff in Hin. destruct Hin as [Hselected | Hlater].
    + apply select_subtree_spec in Hselected.
      destruct Hselected as [Hin Hprefix]. split; [exact Hin |].
      exists anchor. split; [left; reflexivity | exact Hprefix].
    + specialize (IH (reject_subtree anchor records) record Hlater).
      destruct IH as [Hremaining Hcovered].
      apply reject_subtree_spec in Hremaining.
      destruct Hremaining as [Hin _]. split; [exact Hin |].
      destruct Hcovered as [later [HlaterIn Hprefix]].
      exists later. split; [right; exact HlaterIn | exact Hprefix].
Qed.

Lemma concat_partition_complete :
  forall ranked_anchors records record,
    In record records ->
    covered_by ranked_anchors record ->
    In record (concat (closure_partition ranked_anchors records)).
Proof.
  induction ranked_anchors as [| anchor rest IH]; intros records record Hin Hcovered.
  - destruct Hcovered as [candidate [HinCandidate _]]. contradiction.
  - simpl. apply in_app_iff.
    destruct (path_prefixb anchor (record_path record)) eqn:Hprefix.
    + left. apply select_subtree_spec. split; [exact Hin |].
      apply path_prefixb_spec. exact Hprefix.
    + right. apply IH.
      * apply reject_subtree_spec. split; [exact Hin |].
        intro Hancestor. apply path_prefixb_spec in Hancestor.
        rewrite Hancestor in Hprefix. discriminate.
      * destruct Hcovered as [candidate [[Heq | Hrest] Hcandidate]].
        -- subst candidate. exfalso.
           apply path_prefixb_spec in Hcandidate.
           rewrite Hcandidate in Hprefix. discriminate.
        -- exists candidate. split; assumption.
Qed.

Theorem closure_partition_is_exact_selected_union :
  forall ranked_anchors records record,
    In record (concat (closure_partition ranked_anchors records)) <->
    In record records /\ covered_by ranked_anchors record.
Proof.
  intros ranked_anchors records record. split.
  - apply concat_partition_sound.
  - intros [Hin Hcovered].
    apply concat_partition_complete; assumption.
Qed.

Fixpoint owner_rank (ranked_anchors : list Path) (path : Path) : option nat :=
  match ranked_anchors with
  | [] => None
  | anchor :: rest =>
      if path_prefixb anchor path then Some 0
      else option_map S (owner_rank rest path)
  end.

Lemma owner_rank_sound :
  forall ranked_anchors path rank,
    owner_rank ranked_anchors path = Some rank ->
    exists anchor,
      nth_error ranked_anchors rank = Some anchor /\
      path_prefix anchor path.
Proof.
  induction ranked_anchors as [| anchor rest IH]; intros path rank Howner; simpl in Howner.
  - discriminate.
  - destruct (path_prefixb anchor path) eqn:Hprefix.
    + injection Howner as Hrank. subst rank.
      exists anchor. split; [reflexivity |].
      apply path_prefixb_spec. exact Hprefix.
    + destruct (owner_rank rest path) as [later |] eqn:Hlater; simpl in Howner.
      * injection Howner as Hrank. subst rank.
        specialize (IH path later Hlater).
        destruct IH as [candidate [Hnth Hcandidate]].
        exists candidate. split; [simpl; exact Hnth | exact Hcandidate].
      * discriminate.
Qed.

Lemma owner_rank_complete :
  forall ranked_anchors path,
    (exists anchor, In anchor ranked_anchors /\ path_prefix anchor path) ->
    exists rank, owner_rank ranked_anchors path = Some rank.
Proof.
  induction ranked_anchors as [| anchor rest IH]; intros path Hcovered.
  - destruct Hcovered as [candidate [Hin _]]. contradiction.
  - simpl. destruct (path_prefixb anchor path) eqn:Hprefix.
    + exists 0. reflexivity.
    + destruct Hcovered as [candidate [[Heq | Hin] Hcandidate]].
      * subst candidate. apply path_prefixb_spec in Hcandidate.
        rewrite Hcandidate in Hprefix. discriminate.
      * specialize (IH path). destruct IH as [rank Hrank].
        { exists candidate. split; assumption. }
        rewrite Hrank. exists (S rank). reflexivity.
Qed.

Theorem every_covered_record_has_one_rank :
  forall ranked_anchors record,
    covered_by ranked_anchors record ->
    exists rank,
      owner_rank ranked_anchors (record_path record) = Some rank /\
      forall other,
        owner_rank ranked_anchors (record_path record) = Some other ->
        other = rank.
Proof.
  intros ranked_anchors record Hcovered.
  apply owner_rank_complete in Hcovered.
  destruct Hcovered as [rank Hrank]. exists rank. split; [exact Hrank |].
  intros other Hother. rewrite Hrank in Hother. congruence.
Qed.

Theorem selected_union_is_downward_closed :
  forall ranked_anchors ancestor descendant,
    (exists anchor,
       In anchor ranked_anchors /\ path_prefix anchor ancestor) ->
    path_prefix ancestor descendant ->
    exists anchor,
      In anchor ranked_anchors /\ path_prefix anchor descendant.
Proof.
  intros ranked_anchors ancestor descendant
         [anchor [Hin Hcovers]] Hdescendant.
  exists anchor. split; [exact Hin |].
  eapply path_prefix_transitive; eassumption.
Qed.

Lemma records_weight_positive_nonempty :
  forall records,
    Forall (fun record => 0 < record_weight record) records ->
    records <> [] ->
    0 < records_weight records.
Proof.
  intros records Hpositive Hnonempty.
  destruct records as [| record rest]; [contradiction |].
  inversion Hpositive; subst. simpl. lia.
Qed.

Lemma select_subtree_preserves_positive :
  forall anchor records,
    Forall (fun record => 0 < record_weight record) records ->
    Forall
      (fun record => 0 < record_weight record)
      (select_subtree anchor records).
Proof.
  intros anchor records Hpositive.
  apply Forall_forall. intros record Hin.
  apply select_subtree_spec in Hin. destruct Hin as [Hin _].
  apply Forall_forall with (x := record) in Hpositive; assumption.
Qed.

Theorem every_nonempty_closure_bucket_has_positive_gain :
  forall ranked_anchors records bucket,
    Forall (fun record => 0 < record_weight record) records ->
    In bucket (closure_partition ranked_anchors records) ->
    bucket <> [] ->
    0 < records_weight bucket.
Proof.
  induction ranked_anchors as [| anchor rest IH]; intros records bucket Hpositive Hin Hnonempty.
  - simpl in Hin. contradiction.
  - simpl in Hin. destruct Hin as [Heq | Hin].
    + subst bucket. apply records_weight_positive_nonempty.
      * apply select_subtree_preserves_positive. exact Hpositive.
      * exact Hnonempty.
    + apply IH with (records := reject_subtree anchor records); try assumption.
      apply Forall_forall. intros record Hrecord.
      apply reject_subtree_spec in Hrecord. destruct Hrecord as [Hrecord _].
      apply Forall_forall with (x := record) in Hpositive; assumption.
Qed.

Theorem finite_preorder_positive_closure_contract :
  forall topology ranked_anchors,
    planned_weight ranked_anchors (finite_records topology) +
    records_weight
      (unassigned_records ranked_anchors (finite_records topology)) =
    records_weight (finite_records topology) /\
    forall bucket,
      In bucket
        (closure_partition ranked_anchors (finite_records topology)) ->
      bucket <> [] ->
      0 < records_weight bucket.
Proof.
  intros topology ranked_anchors. split.
  - apply finite_preorder_closure_partition_exact.
  - intros bucket Hin Hnonempty.
    eapply every_nonempty_closure_bucket_has_positive_gain.
    + apply finite_positive_weights.
    + exact Hin.
    + exact Hnonempty.
Qed.

(** Production removes zero-gain ranks before applying the configured anchor
    cap.  Such ranks are exactly closures already covered by an earlier-ranked
    ancestor/descendant anchor and therefore consume neither a candidate slot
    nor planned bytes. *)
Definition nonredundant_gains (gains : list nat) : list nat :=
  filter (fun gain => negb (Nat.eqb gain 0)) gains.

Lemma nonredundant_gains_are_positive :
  forall gains,
    Forall (fun gain => 0 < gain) (nonredundant_gains gains).
Proof.
  intros gains. apply Forall_forall. intros gain Hin.
  unfold nonredundant_gains in Hin.
  apply filter_In in Hin. destruct Hin as [_ Hnonzero].
  apply Bool.negb_true_iff in Hnonzero.
  apply Nat.eqb_neq in Hnonzero. lia.
Qed.

(** Select the shortest gain prefix that reaches [target], unless the finite
    anchor cap or the finite gain list is exhausted first.  The zero target is
    handled before inspecting an anchor, matching the production fast path. *)
Fixpoint choose_prefix_count (target cap : nat) (gains : list nat) : nat :=
  match target with
  | 0 => 0
  | S _ =>
      match cap, gains with
      | 0, _ => 0
      | _, [] => 0
      | S cap', gain :: rest =>
          if target <=? gain then 1
          else S (choose_prefix_count (target - gain) cap' rest)
      end
  end.

Definition chosen_gains (target cap : nat) (gains : list nat) : list nat :=
  firstn (choose_prefix_count target cap gains) gains.

Lemma choose_prefix_count_within_cap :
  forall target cap gains,
    choose_prefix_count target cap gains <= cap.
Proof.
  intros target cap gains. revert target gains.
  induction cap as [| cap IH]; intros target gains.
  - unfold choose_prefix_count. destruct target; reflexivity.
  - destruct target as [| target'].
    + unfold choose_prefix_count. apply Nat.le_0_l.
    + destruct gains as [| gain rest].
      * unfold choose_prefix_count. apply Nat.le_0_l.
      * unfold choose_prefix_count at 1.
        destruct (S target' <=? gain); [lia |].
        apply le_n_S. apply IH.
Qed.

Lemma choose_prefix_count_within_available :
  forall target cap gains,
    choose_prefix_count target cap gains <= length gains.
Proof.
  intros target cap gains. revert target gains.
  induction cap as [| cap IH]; intros target gains.
  - unfold choose_prefix_count. destruct target; apply Nat.le_0_l.
  - destruct target as [| target'].
    + unfold choose_prefix_count. apply Nat.le_0_l.
    + destruct gains as [| gain rest].
      * unfold choose_prefix_count. apply Nat.le_refl.
      * unfold choose_prefix_count at 1.
        destruct (S target' <=? gain); [simpl; lia |].
        simpl. apply le_n_S. apply IH.
Qed.

(** A structurally simpler induction principle for the remaining prefix proofs. *)
Lemma choose_prefix_count_unfold_positive :
  forall target cap gain rest,
    0 < target ->
    choose_prefix_count target (S cap) (gain :: rest) =
      if target <=? gain then 1
      else S (choose_prefix_count (target - gain) cap rest).
Proof.
  intros [| target] cap gain rest Hpositive; [lia | reflexivity].
Qed.

Lemma choose_prefix_reaches_if_bounded_prefix_reaches :
  forall gains cap target,
    target <= nat_sum (firstn cap gains) ->
    target <= nat_sum (chosen_gains target cap gains).
Proof.
  intros gains cap target. revert gains target.
  induction cap as [| cap IH]; intros gains target Hreachable.
  - simpl in Hreachable. assert (target = 0) by lia. subst target.
    unfold chosen_gains, choose_prefix_count. simpl. lia.
  - destruct target as [| target'];
      [unfold chosen_gains, choose_prefix_count; simpl; lia |].
    destruct gains as [| gain rest]; [simpl in Hreachable; lia |].
    unfold chosen_gains. rewrite choose_prefix_count_unfold_positive by lia.
    simpl in Hreachable.
    destruct (S target' <=? gain) eqn:Hfirst.
    + apply Nat.leb_le in Hfirst. simpl. lia.
    + apply Nat.leb_gt in Hfirst. simpl.
      assert (Htail : S target' - gain <= nat_sum (firstn cap rest)) by lia.
      specialize (IH rest (S target' - gain) Htail).
      unfold chosen_gains in IH.
      transitivity (gain + (S target' - gain)); [lia |].
      apply Nat.add_le_mono_l. exact IH.
Qed.

Lemma every_shorter_prefix_misses_target :
  forall gains cap target shorter,
    shorter < choose_prefix_count target cap gains ->
    nat_sum (firstn shorter gains) < target.
Proof.
  intros gains cap target shorter. revert gains target shorter.
  induction cap as [| cap IH]; intros gains target shorter Hshort.
  - unfold choose_prefix_count in Hshort. destruct target; lia.
  - destruct target as [| target'];
      [unfold choose_prefix_count in Hshort; lia |].
    destruct gains as [| gain rest];
      [unfold choose_prefix_count in Hshort; lia |].
    rewrite choose_prefix_count_unfold_positive in Hshort by lia.
    destruct (S target' <=? gain) eqn:Hfirst.
    + apply Nat.leb_le in Hfirst.
      assert (shorter = 0) by lia. subst shorter. simpl. lia.
    + apply Nat.leb_gt in Hfirst.
      destruct shorter as [| shorter']; [simpl; lia |].
      simpl.
      assert (Hrecursive :
        shorter' < choose_prefix_count (S target' - gain) cap rest) by lia.
      specialize (IH rest (S target' - gain) shorter' Hrecursive).
      lia.
Qed.

Lemma stopping_below_target_exhausts_cap_or_gains :
  forall gains cap target,
    nat_sum (chosen_gains target cap gains) < target ->
    choose_prefix_count target cap gains = Nat.min cap (length gains).
Proof.
  intros gains cap target. revert gains target.
  induction cap as [| cap IH]; intros gains target Hmiss.
  - unfold choose_prefix_count. rewrite Nat.min_0_l.
    destruct target; reflexivity.
  - destruct target as [| target'];
      [unfold chosen_gains, choose_prefix_count in Hmiss; simpl in Hmiss; lia |].
    destruct gains as [| gain rest].
    + unfold choose_prefix_count. simpl. reflexivity.
    + unfold chosen_gains in Hmiss.
      rewrite choose_prefix_count_unfold_positive in Hmiss |- by lia.
      destruct (S target' <=? gain) eqn:Hfirst.
      * apply Nat.leb_le in Hfirst. simpl in Hmiss. lia.
      * assert (Hless : gain < S target') by
          (apply Nat.leb_gt; exact Hfirst).
        simpl in Hmiss.
        fold (chosen_gains (S target' - gain) cap rest) in Hmiss.
        assert (Htail :
          nat_sum (chosen_gains (S target' - gain) cap rest) <
          S target' - gain).
        { apply (proj2 (Nat.add_lt_mono_l
                          (nat_sum (chosen_gains (S target' - gain) cap rest))
                          (S target' - gain)
                          gain)).
          replace (gain + (S target' - gain)) with (S target').
          - exact Hmiss.
          - rewrite Nat.add_comm. symmetry. apply Nat.sub_add. lia. }
        specialize (IH rest (S target' - gain) Htail).
        rewrite choose_prefix_count_unfold_positive by lia.
        rewrite Hfirst. rewrite IH. simpl.
        symmetry. apply Nat.succ_min_distr.
Qed.

Theorem selected_prefix_is_minimal_and_cap_bounded :
  forall gains cap target,
    choose_prefix_count target cap gains <= cap /\
    choose_prefix_count target cap gains <= length gains /\
    (nat_sum (chosen_gains target cap gains) >= target ->
       forall shorter,
         shorter < choose_prefix_count target cap gains ->
         nat_sum (firstn shorter gains) < target) /\
    (nat_sum (chosen_gains target cap gains) < target ->
       choose_prefix_count target cap gains = Nat.min cap (length gains)).
Proof.
  intros gains cap target. repeat split.
  - apply choose_prefix_count_within_cap.
  - apply choose_prefix_count_within_available.
  - intros _ shorter Hshort.
    eapply every_shorter_prefix_misses_target; eassumption.
  - apply stopping_below_target_exhausts_cap_or_gains.
Qed.

Corollary resident_selection_skips_redundant_ranks_and_is_minimal :
  forall ranked_anchors records cap target,
    let gains := nonredundant_gains (closure_gains ranked_anchors records) in
    choose_prefix_count target cap gains <= cap /\
    choose_prefix_count target cap gains <= length gains /\
    Forall (fun gain => 0 < gain) gains /\
    (nat_sum (chosen_gains target cap gains) >= target ->
       forall shorter,
         shorter < choose_prefix_count target cap gains ->
         nat_sum (firstn shorter gains) < target) /\
    (nat_sum (chosen_gains target cap gains) < target ->
       choose_prefix_count target cap gains = Nat.min cap (length gains)).
Proof.
  intros ranked_anchors records cap target gains.
  pose proof (selected_prefix_is_minimal_and_cap_bounded gains cap target)
    as Hselection.
  destruct Hselection as [Hcap [Havailable [Hminimal Hexhausted]]].
  repeat split; try assumption.
  apply nonredundant_gains_are_positive.
Qed.

(** ** Strict topology depth and downward-closed priority prefixes

    A concrete child segment must contain at least one path unit.  The virtual
    root may still own one empty concrete root record, but no concrete entry may
    have an empty child segment.  This is the exact structural premise needed
    by the production score order: a descendant subtree's warmest score is
    never colder than its ancestor's score, and equal scores break by greater
    depth first.  Hence every eligible resident descendant precedes its
    ancestor, and every selected priority prefix is downward closed. *)

Record ConcreteChildDepth : Type := mkConcreteChildDepth {
  concrete_parent_depth : nat;
  concrete_segment_length : nat;
  concrete_child_depth : nat;
  concrete_segment_nonempty : 0 < concrete_segment_length;
  concrete_depth_equation :
    concrete_child_depth = concrete_parent_depth + concrete_segment_length
}.

Theorem nonempty_concrete_segment_strictly_increases_depth :
  forall edge,
    concrete_parent_depth edge < concrete_child_depth edge.
Proof.
  intros edge.
  rewrite (concrete_depth_equation edge).
  pose proof (concrete_segment_nonempty edge). lia.
Qed.

Record RankedResident : Type := mkRankedResident {
  ranked_path : Path;
  ranked_warmest_score : nat;
  ranked_depth : nat;
  ranked_position : nat
}.

Definition descendant_priority_before
    (descendant ancestor : RankedResident) : Prop :=
  ranked_warmest_score ancestor < ranked_warmest_score descendant \/
  (ranked_warmest_score ancestor = ranked_warmest_score descendant /\
   ranked_depth ancestor < ranked_depth descendant).

Theorem resident_descendant_priority_precedes_ancestor :
  forall descendant ancestor,
    ranked_warmest_score ancestor <= ranked_warmest_score descendant ->
    ranked_depth ancestor < ranked_depth descendant ->
    descendant_priority_before descendant ancestor.
Proof.
  intros descendant ancestor Hscore Hdepth.
  unfold descendant_priority_before.
  destruct (Nat.eq_dec
              (ranked_warmest_score ancestor)
              (ranked_warmest_score descendant)) as [Heq | Hneq].
  - right. split; assumption.
  - left. lia.
Qed.

Definition selected_in_prefix (cutoff : nat) (resident : RankedResident) : Prop :=
  ranked_position resident < cutoff.

Theorem selected_ancestor_implies_selected_resident_descendant :
  forall cutoff ancestor descendant,
    descendant_priority_before descendant ancestor ->
    (descendant_priority_before descendant ancestor ->
       ranked_position descendant < ranked_position ancestor) ->
    selected_in_prefix cutoff ancestor ->
    selected_in_prefix cutoff descendant.
Proof.
  intros cutoff ancestor descendant Hbefore Horder Hselected.
  unfold selected_in_prefix in *.
  specialize (Horder Hbefore). lia.
Qed.

Definition resident_covered_by_selected
    (selected : RankedResident -> Prop) (resident : RankedResident) : Prop :=
  exists ancestor,
    selected ancestor /\ path_prefix (ranked_path ancestor) (ranked_path resident).

Theorem downward_closed_prefix_has_no_unselected_covered_resident :
  forall selected resident,
    (forall ancestor descendant,
       selected ancestor ->
       path_prefix (ranked_path ancestor) (ranked_path descendant) ->
       selected descendant) ->
    resident_covered_by_selected selected resident ->
    selected resident.
Proof.
  intros selected resident Hdownward [ancestor [Hselected Hprefix]].
  eapply Hdownward; eassumption.
Qed.

Record Endpoint : Type := mkEndpoint {
  endpoint_path : Path;
  endpoint_exact : bool
}.

Fixpoint execute_ancestor_frontier_fuel
    (fuel : nat) (endpoints : list Endpoint) : list Endpoint :=
  match fuel, endpoints with
  | 0, _ => []
  | _, [] => []
  | S fuel', endpoint :: rest =>
      if endpoint_exact endpoint then
        endpoint :: execute_ancestor_frontier_fuel fuel'
          (filter
             (fun later =>
                negb (path_prefixb (endpoint_path endpoint) (endpoint_path later)))
             rest)
      else execute_ancestor_frontier_fuel fuel' rest
  end.

Definition execute_ancestor_frontier (endpoints : list Endpoint) : list Endpoint :=
  execute_ancestor_frontier_fuel (length endpoints) endpoints.

Lemma execute_ancestor_frontier_fuel_subset :
  forall fuel endpoints endpoint,
    In endpoint (execute_ancestor_frontier_fuel fuel endpoints) ->
    In endpoint endpoints.
Proof.
  induction fuel as [| fuel IH]; intros endpoints endpoint Hin.
  - simpl in Hin. contradiction.
  - destruct endpoints as [| head rest]; simpl in Hin.
    + contradiction.
    + destruct (endpoint_exact head) eqn:Hexact.
      * destruct Hin as [Heq | Hin]; [left; exact Heq | right].
        apply IH in Hin. apply filter_In in Hin. tauto.
      * right. apply IH. exact Hin.
Qed.

Lemma execute_ancestor_frontier_subset :
  forall endpoints endpoint,
    In endpoint (execute_ancestor_frontier endpoints) ->
    In endpoint endpoints.
Proof.
  intros endpoints endpoint Hin.
  unfold execute_ancestor_frontier in Hin.
  eapply execute_ancestor_frontier_fuel_subset; eassumption.
Qed.

Theorem stale_ancestor_falls_through :
  forall ancestor rest,
    endpoint_exact ancestor = false ->
    execute_ancestor_frontier (ancestor :: rest) =
    execute_ancestor_frontier rest.
Proof.
  intros ancestor rest Hstale.
  unfold execute_ancestor_frontier at 1. simpl. rewrite Hstale.
  reflexivity.
Qed.

Theorem exact_ancestor_suppresses_selected_descendant :
  forall ancestor rest descendant,
    endpoint_exact ancestor = true ->
    In descendant rest ->
    descendant <> ancestor ->
    path_prefix (endpoint_path ancestor) (endpoint_path descendant) ->
    ~ In descendant (execute_ancestor_frontier (ancestor :: rest)).
Proof.
  intros ancestor rest descendant Hexact Hin Hneq Hprefix Hexecuted.
  unfold execute_ancestor_frontier at 1 in Hexecuted.
  simpl in Hexecuted. rewrite Hexact in Hexecuted.
  destruct Hexecuted as [Heq | Htail]; [apply Hneq; symmetry; exact Heq |].
  apply execute_ancestor_frontier_fuel_subset in Htail.
  apply filter_In in Htail. destruct Htail as [_ HnotPrefix].
  apply path_prefixb_spec in Hprefix. rewrite Hprefix in HnotPrefix.
  discriminate.
Qed.

Theorem stale_ancestor_preserves_exact_descendant_fallback :
  forall ancestor descendant,
    endpoint_exact ancestor = false ->
    endpoint_exact descendant = true ->
    execute_ancestor_frontier [ancestor; descendant] = [descendant].
Proof.
  intros ancestor descendant Hstale Hexact.
  unfold execute_ancestor_frontier. simpl. rewrite Hstale, Hexact.
  reflexivity.
Qed.

Record BudgetCommit : Type := mkBudgetCommit {
  snapshot_generation : nat;
  live_generation : nat;
  snapshot_root_revision : nat;
  live_root_revision : nat;
  selected_endpoints_exact : bool;
  resident_bytes_before : nat;
  resident_budget : nat;
  selected_closure_bytes : nat
}.

Definition quiescent_exactb (commit : BudgetCommit) : bool :=
  Nat.eqb (snapshot_generation commit) (live_generation commit) &&
  Nat.eqb (snapshot_root_revision commit) (live_root_revision commit) &&
  selected_endpoints_exact commit.

Definition committed_reclaimed_bytes (commit : BudgetCommit) : nat :=
  if quiescent_exactb commit
  then selected_closure_bytes commit
  else 0.

Theorem quiescent_exact_commit_reclaims_planned_closure :
  forall commit,
    quiescent_exactb commit = true ->
    committed_reclaimed_bytes commit = selected_closure_bytes commit.
Proof.
  intros commit Hquiescent.
  unfold committed_reclaimed_bytes. rewrite Hquiescent. reflexivity.
Qed.

Theorem authority_or_root_change_fails_closed :
  forall commit,
    quiescent_exactb commit = false ->
    committed_reclaimed_bytes commit = 0.
Proof.
  intros commit Hchanged.
  unfold committed_reclaimed_bytes. rewrite Hchanged. reflexivity.
Qed.

Theorem quiescent_one_pass_converges_to_budget :
  forall commit,
    quiescent_exactb commit = true ->
    selected_closure_bytes commit <= resident_bytes_before commit ->
    resident_bytes_before commit <=
      resident_budget commit + selected_closure_bytes commit ->
    resident_bytes_before commit - committed_reclaimed_bytes commit <=
      resident_budget commit.
Proof.
  intros commit Hquiescent Hbounded Htarget.
  rewrite (quiescent_exact_commit_reclaims_planned_closure commit Hquiescent).
  lia.
Qed.

End ResidentBudgetEvictionSpec.
