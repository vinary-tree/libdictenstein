(** * PersistentScdawgSpec: Persistent SCDAWG Refinement Boundary

    The persistent SCDAWG variants reuse the persistent suffix-index storage
    architecture.  Their public SCDAWG contract is therefore a refinement of
    the live-suffix-prefix language: substring membership, frequency, and
    locations are read from active source suffixes, while exact dictionary
    membership is restricted to active source texts.
*)

From Coq Require Import Lists.List.
From Coq Require Import Arith.PeanoNat.
Require Import ARTrie.Spec.PersistentSuffixAutomatonSpec.
Import ListNotations.

Section PersistentScdawg.

Variable A : Type.

Definition pscdawg_text := list A.
Definition pscdawg_source := source A.

Definition persistent_scdawg_substring
  (needle : pscdawg_text) (sources : list pscdawg_source) : Prop :=
  live_suffix_prefix_language A needle sources.

Definition persistent_scdawg_exact
  (term : pscdawg_text) (sources : list pscdawg_source) : Prop :=
  exists src,
    In src sources /\
    source_active A src = true /\
    source_text A src = term.

Definition persistent_scdawg_compact
  (sources : list pscdawg_source) : list pscdawg_source :=
  compact_sources A sources.

Theorem persistent_scdawg_substring_refines_active_language :
  forall needle sources,
    persistent_scdawg_substring needle sources <->
    active_language A needle sources.
Proof.
  intros needle sources.
  unfold persistent_scdawg_substring.
  symmetry.
  apply live_suffix_prefix_exact.
Qed.

Theorem persistent_scdawg_exact_implies_substring :
  forall term sources,
    persistent_scdawg_exact term sources ->
    persistent_scdawg_substring term sources.
Proof.
  intros term sources [src [Hin [Hactive Htext]]].
  unfold persistent_scdawg_substring.
  apply live_suffix_prefix_exact.
  right.
  exists src.
  repeat split; try assumption.
  unfold substring_of.
  exists [], [].
  simpl.
  rewrite app_nil_r.
  exact Htext.
Qed.

Theorem persistent_scdawg_compact_preserves_substrings :
  forall needle sources,
    persistent_scdawg_substring needle (persistent_scdawg_compact sources) <->
    persistent_scdawg_substring needle sources.
Proof.
  intros needle sources.
  unfold persistent_scdawg_substring, persistent_scdawg_compact.
  apply compact_preserves_live_suffix_prefix_language.
Qed.

Theorem persistent_scdawg_compact_preserves_exact :
  forall term sources,
    persistent_scdawg_exact term (persistent_scdawg_compact sources) <->
    persistent_scdawg_exact term sources.
Proof.
  intros term sources.
  unfold persistent_scdawg_exact, persistent_scdawg_compact, compact_sources.
  split.
  - intros [src [Hin [Hactive Htext]]].
    apply filter_In in Hin.
    destruct Hin as [Hin _].
    exists src.
    repeat split; assumption.
  - intros [src [Hin [Hactive Htext]]].
    exists src.
    repeat split; try assumption.
    apply filter_In.
    split; assumption.
Qed.

End PersistentScdawg.
