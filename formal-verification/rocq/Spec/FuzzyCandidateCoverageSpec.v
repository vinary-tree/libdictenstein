(** * FuzzyCandidateCoverageSpec: WallBreaker Candidate Coverage

    This module proves the library-local fuzzy-search bridge:

    If a query is split into [budget + 1] nonempty contiguous pieces, and an
    edit witness damages at most [budget] pieces, then at least one query piece
    is untouched.  If untouched pieces survive as exact substrings of a term,
    the existing substring-candidate specification can surface that term as a
    candidate.

    The proof deliberately stops at candidate coverage.  It does not prove the
    upstream Levenshtein automaton/transducer distance algorithm.
*)

Require Import Coq.Lists.List.
Require Import Coq.Arith.PeanoNat.
Require Import Coq.micromega.Lia.
Require Import ARTrie.Spec.SubstringSearchSpec.
Import ListNotations.

Record QueryPiece := {
  piece_index : nat;
  piece_start : nat;
  piece_pattern : Pattern
}.

Definition piece_end (piece : QueryPiece) : nat :=
  piece_start piece + length (piece_pattern piece).

Definition piece_untouched (damaged_piece_indices : list nat) (piece : QueryPiece) : Prop :=
  ~ In (piece_index piece) damaged_piece_indices.

Definition piece_occurs_in_query (query : Pattern) (piece : QueryPiece) : Prop :=
  nonempty_pattern (piece_pattern piece) /\
  occurs_at (piece_pattern piece) query (piece_start piece).

Definition piece_occurs_in_term (term : Term) (piece : QueryPiece) : Prop :=
  exists position, occurs_at (piece_pattern piece) term position.

Definition partition_patterns (pieces : list QueryPiece) : list Pattern :=
  map piece_pattern pieces.

Definition partition_units (pieces : list QueryPiece) : Pattern :=
  concat (partition_patterns pieces).

Record QueryPartition (query : Pattern) (pieces : list QueryPiece) := {
  partition_concatenates :
    partition_units pieces = query;
  partition_nonempty :
    Forall (fun piece => nonempty_pattern (piece_pattern piece)) pieces;
  partition_piece_occurs :
    forall piece, In piece pieces -> occurs_at (piece_pattern piece) query (piece_start piece);
  partition_index_cover :
    forall index, index < length pieces ->
      exists piece, In piece pieces /\ piece_index piece = index
}.

Record PieceDamageWitness
  (term : Term) (pieces : list QueryPiece) (damaged_piece_indices : list nat) (budget : nat) := {
  damaged_piece_count_within_budget :
    length damaged_piece_indices <= budget;
  undamaged_piece_survives :
    forall piece,
      In piece pieces ->
      piece_untouched damaged_piece_indices piece ->
      piece_occurs_in_term term piece
}.

Lemma all_or_missing_prefix : forall n damaged,
  (forall index, index < n -> In index damaged) \/
  exists index, index < n /\ ~ In index damaged.
Proof.
  induction n as [| n IH]; intros damaged.
  - left.
    intros index Hlt.
    lia.
  - destruct (IH damaged) as [Hall | [index [Hlt Hmissing]]].
    + destruct (in_dec Nat.eq_dec n damaged) as [Hin | Hmissing].
      * left.
        intros index Hlt.
        assert (index < n \/ index = n) as [Hbefore | Heq] by lia.
        -- exact (Hall index Hbefore).
        -- subst. exact Hin.
      * right.
        exists n.
        split.
        -- lia.
        -- exact Hmissing.
    + right.
      exists index.
      split.
      * lia.
      * exact Hmissing.
Qed.

Theorem pigeonhole_untouched_piece_index : forall budget damaged,
  length damaged <= budget ->
  exists index, index < S budget /\ ~ In index damaged.
Proof.
  intros budget damaged Hbudget.
  destruct (all_or_missing_prefix (S budget) damaged)
    as [Hall | Hmissing].
  - exfalso.
    assert (Hincl : incl (seq 0 (S budget)) damaged).
    {
      intros index Hin_seq.
      apply in_seq in Hin_seq.
      destruct Hin_seq as [_ Hlt].
      apply Hall.
      exact Hlt.
    }
    pose proof (@NoDup_incl_length nat (seq 0 (S budget)) damaged
      (seq_NoDup (S budget) 0) Hincl) as Hseq_length.
    rewrite length_seq in Hseq_length.
    lia.
  - exact Hmissing.
Qed.

Theorem budget_plus_one_partition_has_untouched_piece :
  forall query pieces budget damaged,
    QueryPartition query pieces ->
    length pieces = S budget ->
    length damaged <= budget ->
    exists piece,
      In piece pieces /\
      piece_untouched damaged piece.
Proof.
  intros query pieces budget damaged Hpartition Hpiece_count Hbudget.
  destruct (pigeonhole_untouched_piece_index budget damaged Hbudget)
    as [index [Hindex_bound Hmissing]].
  destruct (partition_index_cover query pieces Hpartition index)
    as [piece [Hin Hpiece_index]].
  - rewrite Hpiece_count.
    exact Hindex_bound.
  - exists piece.
    split.
    + exact Hin.
    + unfold piece_untouched.
      rewrite Hpiece_index.
      exact Hmissing.
Qed.

Lemma query_partition_piece_nonempty :
  forall query pieces piece,
    QueryPartition query pieces ->
    In piece pieces ->
    nonempty_pattern (piece_pattern piece).
Proof.
  intros query pieces piece Hpartition Hin.
  pose proof (partition_nonempty query pieces Hpartition) as Hnonempty.
  rewrite Forall_forall in Hnonempty.
  exact (Hnonempty piece Hin).
Qed.

Lemma concat_piece_patterns_nonempty_length_bound :
  forall pieces,
    Forall (fun piece => nonempty_pattern (piece_pattern piece)) pieces ->
    length pieces <= length (partition_units pieces).
Proof.
  induction pieces as [| piece rest IH]; intros Hnonempty.
  - unfold partition_units, partition_patterns.
    simpl.
    lia.
  - inversion Hnonempty as [| ? ? Hpiece_nonempty Hrest_nonempty]; subst.
    unfold partition_units, partition_patterns.
    simpl.
    destruct (piece_pattern piece) as [| unit pattern_tail] eqn:Hpattern.
    + unfold nonempty_pattern in Hpiece_nonempty.
      contradiction.
    + simpl.
      pose proof (IH Hrest_nonempty) as Hrest_bound.
      unfold partition_units, partition_patterns in Hrest_bound.
      rewrite length_app.
      simpl.
      lia.
Qed.

Theorem short_query_has_no_full_nonempty_budget_partition :
  forall query pieces budget,
    QueryPartition query pieces ->
    length pieces = S budget ->
    length query < S budget ->
    False.
Proof.
  intros query pieces budget Hpartition Hpiece_count Hshort.
  pose proof (concat_piece_patterns_nonempty_length_bound
    pieces (partition_nonempty query pieces Hpartition)) as Hbound.
  rewrite (partition_concatenates query pieces Hpartition) in Hbound.
  rewrite Hpiece_count in Hbound.
  lia.
Qed.

Theorem untouched_piece_survives_as_exact_substring :
  forall term pieces damaged budget piece,
    PieceDamageWitness term pieces damaged budget ->
    In piece pieces ->
    piece_untouched damaged piece ->
    piece_occurs_in_term term piece.
Proof.
  intros term pieces damaged budget piece Hwitness Hin Huntouched.
  exact (undamaged_piece_survives
    term pieces damaged budget Hwitness piece Hin Huntouched).
Qed.

Theorem fuzzy_candidate_coverage :
  forall query term pieces damaged budget,
    QueryPartition query pieces ->
    length pieces = S budget ->
    PieceDamageWitness term pieces damaged budget ->
    exists piece position,
      In piece pieces /\
      nonempty_pattern (piece_pattern piece) /\
      piece_untouched damaged piece /\
      occurs_at (piece_pattern piece) term position.
Proof.
  intros query term pieces damaged budget Hpartition Hpiece_count Hwitness.
  destruct (budget_plus_one_partition_has_untouched_piece
    query pieces budget damaged Hpartition Hpiece_count
    (damaged_piece_count_within_budget
      term pieces damaged budget Hwitness))
    as [piece [Hin Hpiece_untouched]].
  destruct (undamaged_piece_survives
    term pieces damaged budget Hwitness piece Hin Hpiece_untouched)
    as [position Hoccurs].
  exists piece, position.
  repeat split.
  - exact Hin.
  - exact (query_partition_piece_nonempty query pieces piece Hpartition Hin).
  - exact Hpiece_untouched.
  - exact Hoccurs.
Qed.

Theorem fuzzy_candidate_reference_occurrence :
  forall terms query term pieces damaged budget,
    In term terms ->
    QueryPartition query pieces ->
    length pieces = S budget ->
    PieceDamageWitness term pieces damaged budget ->
    exists piece occurrence,
      In piece pieces /\
      nonempty_pattern (piece_pattern piece) /\
      piece_untouched damaged piece /\
      reference_occurrence terms (piece_pattern piece) occurrence.
Proof.
  intros terms query term pieces damaged budget Hterm Hpartition Hpiece_count Hwitness.
  destruct (fuzzy_candidate_coverage
    query term pieces damaged budget Hpartition Hpiece_count Hwitness)
    as [piece [position [Hin [Hnonempty [Huntouched Hoccurs]]]]].
  exists piece, {|
    occurrence_term := term;
    occurrence_position := position;
    occurrence_length := length (piece_pattern piece)
  |}.
  split.
  - exact Hin.
  - split.
    + exact Hnonempty.
    + split.
      * exact Huntouched.
      * unfold reference_occurrence.
        simpl.
        split.
        -- exact Hterm.
        -- split.
           ++ reflexivity.
           ++ exact Hoccurs.
Qed.

Theorem fuzzy_candidate_reference_contains :
  forall terms query term pieces damaged budget,
    In term terms ->
    QueryPartition query pieces ->
    length pieces = S budget ->
    PieceDamageWitness term pieces damaged budget ->
    exists piece,
      In piece pieces /\
      nonempty_pattern (piece_pattern piece) /\
      piece_untouched damaged piece /\
      reference_contains terms (piece_pattern piece).
Proof.
  intros terms query term pieces damaged budget Hterm Hpartition Hpiece_count Hwitness.
  destruct (fuzzy_candidate_reference_occurrence
    terms query term pieces damaged budget Hterm Hpartition Hpiece_count Hwitness)
    as [piece [occurrence [Hin [Hnonempty [Huntouched Hoccurrence]]]]].
  exists piece.
  repeat split.
  - exact Hin.
  - exact Hnonempty.
  - exact Huntouched.
  - exists occurrence.
    exact Hoccurrence.
Qed.
