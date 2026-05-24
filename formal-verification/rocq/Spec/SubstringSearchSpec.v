(** * SubstringSearchSpec: Exact Candidate Laws

    This module states the backend-neutral law surface for substring candidate
    generation.  It is the proof boundary that libdictenstein's substring
    indexes must satisfy before an upstream edit-distance transducer consumes
    the candidates for fuzzy search.
*)

Require Import Coq.Lists.List.
Require Import Coq.Arith.PeanoNat.
Require Import Coq.micromega.Lia.
Require Import ARTrie.Spec.MapSpec.
Import ListNotations.

Definition Label := MapSpec.Byte.
Definition Term := MapSpec.Key.
Definition Pattern := Term.

Record Occurrence := {
  occurrence_term : Term;
  occurrence_position : nat;
  occurrence_length : nat
}.

Definition nonempty_pattern (pattern : Pattern) : Prop :=
  pattern <> [].

Definition occurs_at (pattern term : Term) (position : nat) : Prop :=
  exists prefix suffix,
    term = prefix ++ pattern ++ suffix /\
    position = length prefix.

Definition reference_occurrence
  (terms : list Term) (pattern : Pattern) (occ : Occurrence) : Prop :=
  In (occurrence_term occ) terms /\
  occurrence_length occ = length pattern /\
  occurs_at pattern (occurrence_term occ) (occurrence_position occ).

Definition reference_contains (terms : list Term) (pattern : Pattern) : Prop :=
  exists occ, reference_occurrence terms pattern occ.

Definition exact_results
  (terms : list Term) (pattern : Pattern) (results : list Occurrence) : Prop :=
  NoDup results /\
  forall occ, In occ results <-> reference_occurrence terms pattern occ.

Definition limited_results
  (Find : Pattern -> list Occurrence) (pattern : Pattern) (limit : nat) :=
  firstn limit (Find pattern).

Lemma firstn_in : forall (A : Type) (limit : nat) (xs : list A) (x : A),
  In x (firstn limit xs) ->
  In x xs.
Proof.
  intros A limit.
  induction limit as [| limit IH]; intros xs x Hin.
  - simpl in Hin. contradiction.
  - destruct xs as [| head tail].
    + simpl in Hin. contradiction.
    + simpl in Hin. simpl.
      destruct Hin as [Heq | Hin].
      * left. exact Heq.
      * right. apply IH. exact Hin.
Qed.

Lemma firstn_preserves_NoDup : forall (A : Type) (limit : nat) (xs : list A),
  NoDup xs ->
  NoDup (firstn limit xs).
Proof.
  intros A limit.
  induction limit as [| limit IH]; intros xs Hnodup.
  - simpl. constructor.
  - destruct xs as [| head tail].
    + simpl. constructor.
    + simpl.
      inversion Hnodup as [| ? ? Hnotin Htail_nodup]; subst.
      constructor.
      * intros Hin.
        apply Hnotin.
        apply firstn_in with (limit := limit).
        exact Hin.
      * apply IH.
        exact Htail_nodup.
Qed.

Section SubstringSearchLaws.

Variable terms : list Term.
Variable Contains : Pattern -> bool.
Variable Find : Pattern -> list Occurrence.

Record SubstringSearchLaws := {
  contains_sound :
    forall pattern,
      nonempty_pattern pattern ->
      Contains pattern = true ->
      reference_contains terms pattern;
  contains_complete :
    forall pattern,
      nonempty_pattern pattern ->
      reference_contains terms pattern ->
      Contains pattern = true;
  find_sound :
    forall pattern occ,
      In occ (Find pattern) ->
      reference_occurrence terms pattern occ;
  find_complete :
    forall pattern occ,
      nonempty_pattern pattern ->
      reference_occurrence terms pattern occ ->
      In occ (Find pattern);
  find_no_duplicates :
    forall pattern,
      NoDup (Find pattern)
}.

Variable laws : SubstringSearchLaws.

Theorem contains_iff_reference_nonempty : forall pattern,
  nonempty_pattern pattern ->
  Contains pattern = true <-> reference_contains terms pattern.
Proof.
  intros pattern Hnonempty.
  split.
  - intros Hcontains.
    exact (contains_sound laws pattern Hnonempty Hcontains).
  - intros Hreference.
    exact (contains_complete laws pattern Hnonempty Hreference).
Qed.

Theorem find_sound_reference : forall pattern occ,
  In occ (Find pattern) ->
  reference_occurrence terms pattern occ.
Proof.
  intros pattern occ Hin.
  exact (find_sound laws pattern occ Hin).
Qed.

Theorem find_complete_reference_nonempty : forall pattern occ,
  nonempty_pattern pattern ->
  reference_occurrence terms pattern occ ->
  In occ (Find pattern).
Proof.
  intros pattern occ Hnonempty Hreference.
  exact (find_complete laws pattern occ Hnonempty Hreference).
Qed.

Theorem find_iff_reference_nonempty : forall pattern occ,
  nonempty_pattern pattern ->
  In occ (Find pattern) <-> reference_occurrence terms pattern occ.
Proof.
  intros pattern occ Hnonempty.
  split.
  - apply find_sound_reference.
  - apply find_complete_reference_nonempty.
    exact Hnonempty.
Qed.

Theorem find_results_exact_nonempty : forall pattern,
  nonempty_pattern pattern ->
  exact_results terms pattern (Find pattern).
Proof.
  intros pattern Hnonempty.
  split.
  - exact (find_no_duplicates laws pattern).
  - intros occ.
    exact (find_iff_reference_nonempty pattern occ Hnonempty).
Qed.

Theorem find_empty_when_no_reference : forall pattern,
  nonempty_pattern pattern ->
  (forall occ, ~ reference_occurrence terms pattern occ) ->
  Find pattern = [].
Proof.
  intros pattern Hnonempty Hnone.
  destruct (Find pattern) as [| occ rest] eqn:Hfind.
  - reflexivity.
  - exfalso.
    apply (Hnone occ).
    apply (find_sound laws pattern occ).
    rewrite Hfind.
    simpl.
    left.
    reflexivity.
Qed.

Theorem find_nonempty_when_reference : forall pattern occ,
  nonempty_pattern pattern ->
  reference_occurrence terms pattern occ ->
  Find pattern <> [].
Proof.
  intros pattern occ Hnonempty Hreference Hempty.
  pose proof (find_complete laws pattern occ Hnonempty Hreference) as Hin.
  rewrite Hempty in Hin.
  contradiction.
Qed.

Theorem result_occurrence_in_terms : forall pattern occ,
  In occ (Find pattern) ->
  In (occurrence_term occ) terms.
Proof.
  intros pattern occ Hin.
  destruct (find_sound laws pattern occ Hin) as [Hin_terms _].
  exact Hin_terms.
Qed.

Theorem result_length_matches_pattern : forall pattern occ,
  In occ (Find pattern) ->
  occurrence_length occ = length pattern.
Proof.
  intros pattern occ Hin.
  destruct (find_sound laws pattern occ Hin) as [_ [Hlen _]].
  exact Hlen.
Qed.

Theorem result_position_in_bounds : forall pattern occ,
  In occ (Find pattern) ->
  occurrence_position occ + occurrence_length occ <= length (occurrence_term occ).
Proof.
  intros pattern occ Hin.
  destruct (find_sound laws pattern occ Hin) as [_ [Hlen Hoccurs]].
  destruct Hoccurs as [prefix [suffix [Hterm Hpos]]].
  rewrite Hterm, Hpos, Hlen.
  repeat rewrite app_length.
  simpl.
  lia.
Qed.

Theorem result_has_witness_split : forall pattern occ,
  In occ (Find pattern) ->
  exists prefix suffix,
    occurrence_term occ = prefix ++ pattern ++ suffix /\
    occurrence_position occ = length prefix.
Proof.
  intros pattern occ Hin.
  destruct (find_sound laws pattern occ Hin) as [_ [_ Hoccurs]].
  exact Hoccurs.
Qed.

Theorem limited_sound : forall pattern limit occ,
  In occ (limited_results Find pattern limit) ->
  In occ (Find pattern).
Proof.
  intros pattern limit occ Hin.
  unfold limited_results in Hin.
  apply firstn_in with (limit := limit).
  exact Hin.
Qed.

Theorem limited_reference_sound : forall pattern limit occ,
  In occ (limited_results Find pattern limit) ->
  reference_occurrence terms pattern occ.
Proof.
  intros pattern limit occ Hin.
  apply find_sound_reference.
  apply limited_sound with (limit := limit).
  exact Hin.
Qed.

Theorem limited_length_bound : forall pattern limit,
  length (limited_results Find pattern limit) <= limit.
Proof.
  intros pattern limit.
  unfold limited_results.
  rewrite firstn_length.
  lia.
Qed.

Theorem limited_length_source_bound : forall pattern limit,
  length (limited_results Find pattern limit) <= length (Find pattern).
Proof.
  intros pattern limit.
  unfold limited_results.
  rewrite firstn_length.
  lia.
Qed.

Theorem limited_zero_empty : forall pattern,
  limited_results Find pattern 0 = [].
Proof.
  intros pattern.
  reflexivity.
Qed.

Theorem limited_all_when_large : forall pattern limit,
  length (Find pattern) <= limit ->
  limited_results Find pattern limit = Find pattern.
Proof.
  intros pattern limit Hle.
  unfold limited_results.
  apply firstn_all2.
  exact Hle.
Qed.

Theorem limited_no_duplicates : forall pattern limit,
  NoDup (limited_results Find pattern limit).
Proof.
  intros pattern limit.
  unfold limited_results.
  apply firstn_preserves_NoDup.
  exact (find_no_duplicates laws pattern).
Qed.

End SubstringSearchLaws.

Theorem reference_occurrence_position_in_bounds : forall terms pattern occ,
  reference_occurrence terms pattern occ ->
  occurrence_position occ + occurrence_length occ <= length (occurrence_term occ).
Proof.
  intros terms pattern occ [_ [Hlen Hoccurs]].
  destruct Hoccurs as [prefix [suffix [Hterm Hpos]]].
  rewrite Hterm, Hpos, Hlen.
  repeat rewrite app_length.
  simpl.
  lia.
Qed.

Theorem reference_contains_from_occurrence : forall terms pattern occ,
  reference_occurrence terms pattern occ ->
  reference_contains terms pattern.
Proof.
  intros terms pattern occ Hocc.
  exists occ.
  exact Hocc.
Qed.

Theorem empty_pattern_start_occurrence : forall terms term,
  In term terms ->
  reference_occurrence terms [] {|
    occurrence_term := term;
    occurrence_position := 0;
    occurrence_length := 0
  |}.
Proof.
  intros terms term Hin.
  split.
  - exact Hin.
  - split.
    + reflexivity.
    + exists [], term.
      split.
      * reflexivity.
      * reflexivity.
Qed.
