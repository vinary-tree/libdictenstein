(** * ScdawgOccurrenceSpec: SCDAWG Occurrence Construction Laws

    This module states the construction-facing proof boundary for the
    SCDAWG substring index.  The existing [SubstringSearchSpec] proves the
    public exact-candidate API laws.  This specification refines that boundary
    by naming the internal obligations that make those candidates trustworthy:

    - forward traversal finds the state for a nonempty pattern exactly when
      the reference term set contains that pattern;
    - left-extension closure from that state enumerates exactly the term-end
      witnesses for all occurrences; and
    - [contains_substring], [locations], and [freq] agree with the same
      reference occurrence relation.

    The model deliberately proves occurrence exactness, not optimal/minimal
    CDAWG construction or upstream Levenshtein transducer correctness.
*)

Require Import Coq.Lists.List.
Require Import Coq.Arith.PeanoNat.
Require Import Coq.micromega.Lia.
Require Import ARTrie.Spec.SubstringSearchSpec.
Import ListNotations.

Definition State := nat.
Definition TermIndex := nat.
Definition EndPosition := nat.
Definition TermEnd := (TermIndex * EndPosition)%type.

Definition Step := State -> Label -> option State.

Fixpoint walk_from (step : Step) (state : State) (pattern : Pattern)
  : option State :=
  match pattern with
  | [] => Some state
  | label :: rest =>
      match step state label with
      | Some next => walk_from step next rest
      | None => None
      end
  end.

Record ScdawgGraph := {
  scdawg_root : State;
  scdawg_step : Step;
  scdawg_left_edges : State -> list (Label * State);
  scdawg_term_ends : State -> list TermEnd;
  scdawg_term_at_index : TermIndex -> option Term
}.

Definition accepts_at
  (graph : ScdawgGraph) (pattern : Pattern) (state : State) : Prop :=
  walk_from (scdawg_step graph) (scdawg_root graph) pattern = Some state.

Inductive left_reachable (graph : ScdawgGraph) : State -> State -> Prop :=
| left_reachable_refl :
    forall state,
      left_reachable graph state state
| left_reachable_step :
    forall start current label next,
      left_reachable graph start current ->
      In (label, next) (scdawg_left_edges graph current) ->
      left_reachable graph start next.

Definition term_end_occurrence
  (graph : ScdawgGraph) (pattern_len : nat) (state : State)
  (occ : Occurrence) : Prop :=
  exists term_index end_pos term,
    In (term_index, end_pos) (scdawg_term_ends graph state) /\
    scdawg_term_at_index graph term_index = Some term /\
    end_pos + 1 >= pattern_len /\
    occ = {|
      occurrence_term := term;
      occurrence_position := end_pos + 1 - pattern_len;
      occurrence_length := pattern_len
    |}.

Definition left_closure_occurrence
  (graph : ScdawgGraph) (pattern_len : nat) (start : State)
  (occ : Occurrence) : Prop :=
  exists witness_state,
    left_reachable graph start witness_state /\
    term_end_occurrence graph pattern_len witness_state occ.

Section ScdawgOccurrenceLaws.

Variable graph : ScdawgGraph.
Variable terms : list Term.
Variable FindState : Pattern -> option State.
Variable LocationsAt : State -> nat -> list Occurrence.
Variable Contains : Pattern -> bool.
Variable Freq : Pattern -> nat.

Definition Locations (pattern : Pattern) : list Occurrence :=
  match FindState pattern with
  | Some state => LocationsAt state (length pattern)
  | None => []
  end.

Record ScdawgOccurrenceLaws := {
  scdawg_find_state_sound :
    forall pattern state,
      FindState pattern = Some state ->
      accepts_at graph pattern state;

  scdawg_find_state_complete :
    forall pattern state,
      nonempty_pattern pattern ->
      accepts_at graph pattern state ->
      FindState pattern = Some state;

  scdawg_forward_sound :
    forall pattern state,
      nonempty_pattern pattern ->
      accepts_at graph pattern state ->
      reference_contains terms pattern;

  scdawg_forward_complete :
    forall pattern,
      nonempty_pattern pattern ->
      reference_contains terms pattern ->
      exists state, accepts_at graph pattern state;

  scdawg_left_closure_sound :
    forall pattern state occ,
      accepts_at graph pattern state ->
      left_closure_occurrence graph (length pattern) state occ ->
      reference_occurrence terms pattern occ;

  scdawg_left_closure_complete :
    forall pattern state occ,
      nonempty_pattern pattern ->
      accepts_at graph pattern state ->
      reference_occurrence terms pattern occ ->
      left_closure_occurrence graph (length pattern) state occ;

  scdawg_locations_at_sound :
    forall state pattern_len occ,
      In occ (LocationsAt state pattern_len) ->
      left_closure_occurrence graph pattern_len state occ;

  scdawg_locations_at_complete :
    forall state pattern_len occ,
      left_closure_occurrence graph pattern_len state occ ->
      In occ (LocationsAt state pattern_len);

  scdawg_locations_at_no_duplicates :
    forall state pattern_len,
      NoDup (LocationsAt state pattern_len);

  scdawg_contains_find :
    forall pattern,
      Contains pattern = true <-> exists state, FindState pattern = Some state;

  scdawg_freq_present :
    forall pattern state,
      FindState pattern = Some state ->
      Freq pattern = length (LocationsAt state (length pattern));

  scdawg_freq_absent :
    forall pattern,
      FindState pattern = None ->
      Freq pattern = 0
}.

Variable laws : ScdawgOccurrenceLaws.

Theorem scdawg_locations_at_sound_reference :
  forall pattern state occ,
    FindState pattern = Some state ->
    In occ (LocationsAt state (length pattern)) ->
    reference_occurrence terms pattern occ.
Proof.
  intros pattern state occ Hfind Hin.
  apply (scdawg_left_closure_sound laws pattern state occ).
  - apply (scdawg_find_state_sound laws pattern state).
    exact Hfind.
  - apply (scdawg_locations_at_sound laws state (length pattern) occ).
    exact Hin.
Qed.

Theorem scdawg_locations_at_complete_reference :
  forall pattern state occ,
    nonempty_pattern pattern ->
    FindState pattern = Some state ->
    reference_occurrence terms pattern occ ->
    In occ (LocationsAt state (length pattern)).
Proof.
  intros pattern state occ Hnonempty Hfind Hreference.
  apply (scdawg_locations_at_complete laws state (length pattern) occ).
  apply (scdawg_left_closure_complete laws pattern state occ).
  - exact Hnonempty.
  - apply (scdawg_find_state_sound laws pattern state).
    exact Hfind.
  - exact Hreference.
Qed.

Theorem scdawg_locations_sound_reference :
  forall pattern occ,
    In occ (Locations pattern) ->
    reference_occurrence terms pattern occ.
Proof.
  intros pattern occ Hin.
  unfold Locations in Hin.
  destruct (FindState pattern) as [state |] eqn:Hfind.
  - apply scdawg_locations_at_sound_reference with (state := state).
    + exact Hfind.
    + exact Hin.
  - contradiction.
Qed.

Theorem scdawg_locations_complete_reference_nonempty :
  forall pattern occ,
    nonempty_pattern pattern ->
    reference_occurrence terms pattern occ ->
    In occ (Locations pattern).
Proof.
  intros pattern occ Hnonempty Hreference.
  unfold Locations.
  destruct (scdawg_forward_complete laws pattern Hnonempty
    (reference_contains_from_occurrence terms pattern occ Hreference))
    as [state Haccepts].
  pose proof
    (scdawg_find_state_complete laws pattern state Hnonempty Haccepts)
    as Hfind.
  rewrite Hfind.
  apply (scdawg_locations_at_complete laws state (length pattern) occ).
  apply (scdawg_left_closure_complete laws pattern state occ).
  - exact Hnonempty.
  - exact Haccepts.
  - exact Hreference.
Qed.

Theorem scdawg_locations_no_duplicates :
  forall pattern,
    NoDup (Locations pattern).
Proof.
  intros pattern.
  unfold Locations.
  destruct (FindState pattern) as [state |].
  - apply (scdawg_locations_at_no_duplicates laws state (length pattern)).
  - constructor.
Qed.

Theorem scdawg_locations_exact_nonempty :
  forall pattern,
    nonempty_pattern pattern ->
    exact_results terms pattern (Locations pattern).
Proof.
  intros pattern Hnonempty.
  split.
  - apply scdawg_locations_no_duplicates.
  - intros occ.
    split.
    + apply scdawg_locations_sound_reference.
    + apply scdawg_locations_complete_reference_nonempty.
      exact Hnonempty.
Qed.

Theorem scdawg_contains_iff_reference_nonempty :
  forall pattern,
    nonempty_pattern pattern ->
    Contains pattern = true <-> reference_contains terms pattern.
Proof.
  intros pattern Hnonempty.
  destruct (scdawg_contains_find laws pattern) as [Hto_find Hfrom_find].
  split.
  - intros Hcontains.
    destruct (Hto_find Hcontains) as [state Hfind].
    apply (scdawg_forward_sound laws pattern state).
    + exact Hnonempty.
    + apply (scdawg_find_state_sound laws pattern state).
      exact Hfind.
  - intros Hreference.
    apply Hfrom_find.
    destruct (scdawg_forward_complete laws pattern Hnonempty Hreference)
      as [state Haccepts].
    exists state.
    apply (scdawg_find_state_complete laws pattern state).
    + exact Hnonempty.
    + exact Haccepts.
Qed.

Theorem scdawg_find_state_none_no_reference :
  forall pattern,
    nonempty_pattern pattern ->
    FindState pattern = None ->
    forall occ, ~ reference_occurrence terms pattern occ.
Proof.
  intros pattern Hnonempty Hnone occ Hreference.
  destruct (scdawg_forward_complete laws pattern Hnonempty
    (reference_contains_from_occurrence terms pattern occ Hreference))
    as [state Haccepts].
  pose proof
    (scdawg_find_state_complete laws pattern state Hnonempty Haccepts)
    as Hsome.
  rewrite Hnone in Hsome.
  discriminate.
Qed.

Theorem scdawg_contains_false_no_reference :
  forall pattern,
    nonempty_pattern pattern ->
    Contains pattern = false ->
    forall occ, ~ reference_occurrence terms pattern occ.
Proof.
  intros pattern Hnonempty Hcontains_false occ Hreference.
  pose proof
    (scdawg_contains_iff_reference_nonempty pattern Hnonempty)
    as [_ Hcomplete].
  pose proof
    (Hcomplete
      (reference_contains_from_occurrence terms pattern occ Hreference))
    as Hcontains_true.
  rewrite Hcontains_false in Hcontains_true.
  discriminate.
Qed.

Theorem scdawg_freq_matches_locations :
  forall pattern,
    Freq pattern = length (Locations pattern).
Proof.
  intros pattern.
  unfold Locations.
  destruct (FindState pattern) as [state |] eqn:Hfind.
  - apply (scdawg_freq_present laws pattern state).
    exact Hfind.
  - rewrite (scdawg_freq_absent laws pattern Hfind).
    reflexivity.
Qed.

Theorem scdawg_freq_zero_when_no_reference :
  forall pattern,
    nonempty_pattern pattern ->
    (forall occ, ~ reference_occurrence terms pattern occ) ->
    Freq pattern = 0.
Proof.
  intros pattern Hnonempty Hnone.
  destruct (FindState pattern) as [state |] eqn:Hfind.
  - exfalso.
    pose proof
      (scdawg_forward_sound laws pattern state Hnonempty
        (scdawg_find_state_sound laws pattern state Hfind))
      as [occ Hreference].
    exact (Hnone occ Hreference).
  - apply (scdawg_freq_absent laws pattern).
    exact Hfind.
Qed.

Theorem scdawg_location_in_terms :
  forall pattern occ,
    In occ (Locations pattern) ->
    In (occurrence_term occ) terms.
Proof.
  intros pattern occ Hin.
  destruct (scdawg_locations_sound_reference pattern occ Hin) as [Hin_terms _].
  exact Hin_terms.
Qed.

Theorem scdawg_location_position_in_bounds :
  forall pattern occ,
    In occ (Locations pattern) ->
    occurrence_position occ + occurrence_length occ <= length (occurrence_term occ).
Proof.
  intros pattern occ Hin.
  apply reference_occurrence_position_in_bounds with
    (terms := terms) (pattern := pattern).
  apply scdawg_locations_sound_reference.
  exact Hin.
Qed.

Theorem scdawg_refines_substring_laws :
  SubstringSearchLaws terms Contains Locations.
Proof.
  constructor.
  - intros pattern Hnonempty Hcontains.
    apply scdawg_contains_iff_reference_nonempty.
    + exact Hnonempty.
    + exact Hcontains.
  - intros pattern Hnonempty Hreference.
    apply scdawg_contains_iff_reference_nonempty.
    + exact Hnonempty.
    + exact Hreference.
  - intros pattern occ Hin.
    apply scdawg_locations_sound_reference.
    exact Hin.
  - intros pattern occ Hnonempty Hreference.
    apply scdawg_locations_complete_reference_nonempty.
    + exact Hnonempty.
    + exact Hreference.
  - intros pattern.
    apply scdawg_locations_no_duplicates.
Qed.

End ScdawgOccurrenceLaws.
