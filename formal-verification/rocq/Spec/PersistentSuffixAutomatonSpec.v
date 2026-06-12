(** * PersistentSuffixAutomatonSpec: Suffix-Prefix Language Laws

    The persistent suffix automata store every source suffix in the persistent
    ARTrie data namespace, then answer substring membership by asking whether
    the queried needle is a prefix of at least one live source suffix.

    This file proves the core semantic equivalence used by both byte and char
    variants:

    - a needle is a substring of an active source text iff it is a prefix of
      some suffix of that active source text;
    - filtering/removing inactive sources preserves exactly the active
      substring language after compaction.
*)

From Stdlib Require Import Arith.PeanoNat.
From Stdlib Require Import Bool.Bool.
From Stdlib Require Import Lists.List.
From Stdlib Require Import Lia.
Import ListNotations.

Section PersistentSuffixAutomaton.

Variable A : Type.

Definition text := list A.

Record source : Type := {
  source_active : bool;
  source_text : text
}.

Definition prefix_of (needle haystack : text) : Prop :=
  exists rest, haystack = needle ++ rest.

Definition suffix_of (suffix haystack : text) : Prop :=
  exists front, haystack = front ++ suffix.

Definition substring_of (needle haystack : text) : Prop :=
  exists front rest, haystack = front ++ needle ++ rest.

Definition occurrence_position
  (needle haystack : text) (start finish : nat) : Prop :=
  exists front rest,
    haystack = front ++ needle ++ rest /\
    start = length front /\
    finish = start + length needle.

Definition active_language (needle : text) (sources : list source) : Prop :=
  needle = [] \/
  exists src,
    In src sources /\
    source_active src = true /\
    substring_of needle (source_text src).

Definition live_suffix_prefix_language
  (needle : text) (sources : list source) : Prop :=
  needle = [] \/
  exists src suffix,
    In src sources /\
    source_active src = true /\
    suffix_of suffix (source_text src) /\
    prefix_of needle suffix.

Definition valued_language
  (needle : text) (sources : list source) (value_keys : list text) : Prop :=
  active_language needle sources \/
  exists value_key,
    In value_key value_keys /\
    prefix_of needle value_key.

Definition compact_sources (sources : list source) : list source :=
  filter source_active sources.

Theorem substring_iff_prefix_of_suffix :
  forall needle haystack,
    substring_of needle haystack <->
    exists suffix,
      suffix_of suffix haystack /\ prefix_of needle suffix.
Proof.
  intros needle haystack.
  split.
  - intros [front [rest Htext]].
    exists (needle ++ rest).
    split.
    + unfold suffix_of. exists front. exact Htext.
    + unfold prefix_of. exists rest. reflexivity.
  - intros [suffix [[front Hsuffix] [rest Hprefix]]].
    unfold substring_of.
    exists front, rest.
    rewrite Hsuffix.
    rewrite Hprefix.
    rewrite app_assoc.
    reflexivity.
Qed.

Theorem occurrence_position_sound :
  forall needle haystack start finish,
    occurrence_position needle haystack start finish ->
    substring_of needle haystack.
Proof.
  intros needle haystack start finish [front [rest [Htext [_ _]]]].
  unfold substring_of.
  exists front, rest.
  exact Htext.
Qed.

Theorem occurrence_position_complete :
  forall needle haystack,
    substring_of needle haystack ->
    exists start finish,
      occurrence_position needle haystack start finish.
Proof.
  intros needle haystack [front [rest Htext]].
  exists (length front), (length front + length needle).
  unfold occurrence_position.
  exists front, rest.
  repeat split; try assumption.
Qed.

Theorem occurrence_position_finish_sound :
  forall needle haystack start finish,
    occurrence_position needle haystack start finish ->
    finish = start + length needle.
Proof.
  intros needle haystack start finish [_ [_ [_ [_ Hfinish]]]].
  exact Hfinish.
Qed.

Theorem live_suffix_prefix_exact :
  forall needle sources,
    active_language needle sources <->
    live_suffix_prefix_language needle sources.
Proof.
  intros needle sources.
  split.
  - intros [Hempty | [src [Hin [Hactive Hsub]]]].
    + left. exact Hempty.
    + right.
      apply substring_iff_prefix_of_suffix in Hsub.
      destruct Hsub as [suffix [Hsuffix Hprefix]].
      exists src, suffix.
      repeat split; assumption.
  - intros [Hempty | [src [suffix [Hin [Hactive [Hsuffix Hprefix]]]]]].
    + left. exact Hempty.
    + right.
      exists src.
      repeat split; try assumption.
      apply substring_iff_prefix_of_suffix.
      exists suffix.
      split; assumption.
Qed.

Theorem compact_preserves_active_language :
  forall needle sources,
    active_language needle (compact_sources sources) <->
    active_language needle sources.
Proof.
  intros needle sources.
  split.
  - intros [Hempty | [src [Hin [Hactive Hsub]]]].
    + left. exact Hempty.
    + unfold compact_sources in Hin.
      apply filter_In in Hin.
      destruct Hin as [Hin _].
      right.
      exists src.
      repeat split; assumption.
  - intros [Hempty | [src [Hin [Hactive Hsub]]]].
    + left. exact Hempty.
    + right.
      exists src.
      repeat split; try assumption.
      unfold compact_sources.
      apply filter_In.
      split; assumption.
Qed.

Theorem compact_preserves_live_suffix_prefix_language :
  forall needle sources,
    live_suffix_prefix_language needle (compact_sources sources) <->
    live_suffix_prefix_language needle sources.
Proof.
  intros needle sources.
  rewrite <- live_suffix_prefix_exact.
  rewrite <- (live_suffix_prefix_exact needle sources).
  apply compact_preserves_active_language.
Qed.

Theorem compact_preserves_valued_language :
  forall needle sources value_keys,
    valued_language needle (compact_sources sources) value_keys <->
    valued_language needle sources value_keys.
Proof.
  intros needle sources value_keys.
  unfold valued_language.
  split.
  - intros [Hactive | Hvalue].
    + left.
      apply compact_preserves_active_language.
      exact Hactive.
    + right. exact Hvalue.
  - intros [Hactive | Hvalue].
    + left.
      apply compact_preserves_active_language.
      exact Hactive.
    + right. exact Hvalue.
Qed.

Theorem compact_sources_idempotent :
  forall sources,
    compact_sources (compact_sources sources) = compact_sources sources.
Proof.
  induction sources as [| src rest IH].
  - reflexivity.
  - unfold compact_sources in *.
    simpl.
    destruct (source_active src) eqn:Hactive.
    + simpl.
      rewrite Hactive.
      rewrite IH.
      reflexivity.
    + exact IH.
Qed.

End PersistentSuffixAutomaton.
