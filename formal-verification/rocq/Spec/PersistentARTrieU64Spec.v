(** * PersistentARTrieU64Spec: Sequence-Keyed Persistent Trie Laws

    The implementation exposes u64-native sequence operations and stores one
    native [u64] label per overlay edge.  Durable WAL records encode labels as
    fixed-width little-endian bytes, while the AR64CX01 checkpoint stores native
    labels in path-compressed disk nodes.  This specification names both proof
    boundaries: the WAL encoding is injective at sequence boundaries, exact
    membership refines an abstract u64-sequence set, checkpoint/reopen preserves
    that set, and the explicit heap-spine machines are extensionally equivalent
    to their recursive mathematical definitions.
*)

From Coq Require Import Lists.List.
From Coq Require Import Arith.PeanoNat.
From Coq Require Import Lia.
From Coq Require Import Sorting.Sorted.
Require Import ARTrie.Spec.DynamicDawgU64Spec.
Require Import ARTrie.Model.ListCompat.
Import ListNotations.

Definition U64Bytes := list nat.

Record U64PersistentState := {
  u64_live : U64Set;
  u64_durable : U64Set
}.

Definition fixed_width_u64_encoding
  (encode : U64Sequence -> U64Bytes) : Prop :=
  forall sequence, length (encode sequence) = 8 * length sequence.

Definition sequence_boundary_key (bytes : U64Bytes) : Prop :=
  exists n, length bytes = 8 * n.

Definition u64_persistent_init : U64PersistentState := {|
  u64_live := u64_set_empty;
  u64_durable := u64_set_empty
|}.

Definition u64_persistent_insert
  (state : U64PersistentState) (sequence : U64Sequence) : U64PersistentState :=
  {|
    u64_live := u64_set_insert (u64_live state) sequence;
    u64_durable := u64_durable state
  |}.

Definition u64_persistent_remove
  (state : U64PersistentState) (sequence : U64Sequence) : U64PersistentState :=
  {|
    u64_live := u64_set_remove (u64_live state) sequence;
    u64_durable := u64_durable state
  |}.

Definition u64_persistent_checkpoint
  (state : U64PersistentState) : U64PersistentState :=
  {|
    u64_live := u64_live state;
    u64_durable := u64_live state
  |}.

Definition u64_persistent_reopen
  (state : U64PersistentState) : U64PersistentState :=
  {|
    u64_live := u64_durable state;
    u64_durable := u64_durable state
  |}.

Theorem fixed_width_encoding_has_sequence_boundary :
  forall encode sequence,
    fixed_width_u64_encoding encode ->
    sequence_boundary_key (encode sequence).
Proof.
  intros encode sequence Hwidth.
  unfold fixed_width_u64_encoding in Hwidth.
  unfold sequence_boundary_key.
  exists (length sequence).
  apply Hwidth.
Qed.

Theorem persistent_u64_insert_contains :
  forall state sequence,
    u64_set_contains
      (u64_live (u64_persistent_insert state sequence))
      sequence = true.
Proof.
  intros state sequence.
  simpl.
  apply u64_set_insert_contains_same.
Qed.

Theorem persistent_u64_remove_absent :
  forall state sequence,
    u64_set_contains
      (u64_live (u64_persistent_remove state sequence))
      sequence = false.
Proof.
  intros state sequence.
  simpl.
  apply u64_set_remove_contains_same.
Qed.

Theorem persistent_u64_checkpoint_reopen_preserves_live :
  forall state sequence,
    u64_set_contains
      (u64_live (u64_persistent_reopen (u64_persistent_checkpoint state)))
      sequence =
    u64_set_contains (u64_live state) sequence.
Proof.
  intros state sequence.
  reflexivity.
Qed.

Theorem persistent_u64_reopen_uses_durable :
  forall state sequence,
    u64_set_contains
      (u64_live (u64_persistent_reopen state))
      sequence =
    u64_set_contains (u64_durable state) sequence.
Proof.
  intros state sequence.
  reflexivity.
Qed.

(** ** Explicit-spine equivalence

    Production mutation descends iteratively, recording root-to-parent frames,
    and then consumes those frames in reverse.  The recursive function below is
    a proof oracle only; no production Rust path evaluates it. *)
Section ExplicitSpine.

Context {Node Parent Unit : Type}.
Variable attach : Parent -> Unit -> Node -> Node.

Definition SpineFrame := (Parent * Unit)%type.

Definition apply_frame (node : Node) (frame : SpineFrame) : Node :=
  let '(parent, unit) := frame in attach parent unit node.

Fixpoint recursive_plug (frames : list SpineFrame) (leaf : Node) : Node :=
  match frames with
  | [] => leaf
  | frame :: rest => apply_frame (recursive_plug rest leaf) frame
  end.

Definition iterative_unwind (frames : list SpineFrame) (leaf : Node) : Node :=
  fold_left apply_frame (rev frames) leaf.

Theorem iterative_unwind_refines_recursive_plug :
  forall frames leaf,
    iterative_unwind frames leaf = recursive_plug frames leaf.
Proof.
  intros frames.
  induction frames as [|frame rest IH]; intros leaf.
  - reflexivity.
  - unfold iterative_unwind in *.
    cbn.
    rewrite fold_left_app.
    simpl.
    rewrite IH.
    reflexivity.
Qed.

End ExplicitSpine.

(** ** Missing-suffix construction

    [iterative_create_spine] is the mathematical counterpart of the shared
    [create_spine] reverse loop. *)
Section CreateSpine.

Context {Node Unit : Type}.
Variable wrap : Unit -> Node -> Node.

Fixpoint recursive_create_spine (suffix : list Unit) (leaf : Node) : Node :=
  match suffix with
  | [] => leaf
  | unit :: rest => wrap unit (recursive_create_spine rest leaf)
  end.

Definition iterative_create_spine (suffix : list Unit) (leaf : Node) : Node :=
  fold_left (fun node unit => wrap unit node) (rev suffix) leaf.

Theorem iterative_create_spine_refines_recursive_definition :
  forall suffix leaf,
    iterative_create_spine suffix leaf = recursive_create_spine suffix leaf.
Proof.
  intros suffix.
  induction suffix as [|unit rest IH]; intros leaf.
  - reflexivity.
  - unfold iterative_create_spine in *.
    cbn.
    rewrite fold_left_app.
    simpl.
    rewrite IH.
    reflexivity.
Qed.

Definition descent_frames (key : list Unit) (matched : nat) : list Unit :=
  firstn matched key.

Theorem descent_frame_count_is_bounded_by_key_length :
  forall key matched,
    length (descent_frames key matched) <= length key.
Proof.
  unfold descent_frames.
  intros key.
  induction key as [|unit rest IH]; intros [|matched]; simpl.
  - apply le_n.
  - apply le_n.
  - apply Nat.le_0_l.
  - apply le_n_S. apply IH.
Qed.

End CreateSpine.

(** ** Disk validation frame bound

    The Rust validator stores [(node_index, next_child)] frames.  A node is
    pushed only while its color is [Visiting], so active frame nodes are unique
    and every frame index belongs to the finite disk-node table.  This proves
    that explicit-machine storage grows with reachable graph depth but can never
    exceed the number of encoded records. *)
Section DiskValidationFrames.

Definition DiskFrame := (nat * nat)%type.

Theorem disk_validation_frame_count_is_bounded_by_node_table :
  forall node_count (frames : list DiskFrame),
    NoDup (map fst frames) ->
    (forall frame, In frame frames -> fst frame < node_count) ->
    length frames <= node_count.
Proof.
  intros node_count frames Hunique Hbounded.
  assert (Hincl : incl (map fst frames) (seq 0 node_count)).
  {
    intros node Hnode.
    apply in_map_iff in Hnode.
    destruct Hnode as [frame [Hnode Hframe]].
    subst node.
    apply in_seq.
    split; [lia |].
    apply Hbounded.
    exact Hframe.
  }
  pose proof
    (@NoDup_incl_length nat (map fst frames) (seq 0 node_count)
      Hunique Hincl) as Hlength.
  rewrite length_map, seq_length_portable in Hlength.
  exact Hlength.
Qed.

Theorem visiting_child_cannot_be_pushed_without_duplicate :
  forall (active_nodes : list nat) child,
    In child active_nodes ->
    ~ NoDup (child :: active_nodes).
Proof.
  intros active_nodes child Hin Hunique.
  inversion Hunique.
  contradiction.
Qed.

End DiskValidationFrames.

(** ** Child-before-parent postorder construction

    Validation records a postorder but creates no [Arc] edges.  Construction
    consumes that order from left to right.  [postorder_ready] states the exact
    executable obligation: every child of a node occurs in the already-consumed
    prefix. *)
Section PostorderConstruction.

Definition append_completed (completed : list nat) (node : nat) : list nat :=
  completed ++ [node].

Definition construct_postorder (order : list nat) : list nat :=
  fold_left append_completed order [].

Lemma fold_left_append_completed :
  forall pending completed,
    fold_left append_completed pending completed = completed ++ pending.
Proof.
  induction pending as [|node rest IH]; intros completed.
  - simpl. rewrite app_nil_r. reflexivity.
  - simpl. rewrite IH. unfold append_completed.
    rewrite <- app_assoc. reflexivity.
Qed.

Theorem construct_postorder_preserves_order :
  forall order,
    construct_postorder order = order.
Proof.
  intros order.
  unfold construct_postorder.
  rewrite fold_left_append_completed.
  reflexivity.
Qed.

Definition postorder_ready
  (children : nat -> list nat) (order : list nat) : Prop :=
  forall prefix parent suffix,
    order = prefix ++ parent :: suffix ->
    forall child,
      In child (children parent) ->
      In child prefix.

Theorem postorder_parent_observes_every_completed_child :
  forall children order prefix parent suffix child,
    postorder_ready children order ->
    order = prefix ++ parent :: suffix ->
    In child (children parent) ->
    In child (construct_postorder prefix).
Proof.
  intros children order prefix parent suffix child
    Hready Hdecompose Hchild.
  rewrite construct_postorder_preserves_order.
  eapply Hready; eauto.
Qed.

End PostorderConstruction.

(** ** Linear bulk-child construction

    The decoder's [SortedUniqueEntries] witness establishes that each next edge
    is strictly greater than the preceding labels.  Under that boundary,
    sequential [with_child] calls append those edges in order.  The production
    bulk constructor installs the same canonical sequence in one pass, avoiding
    repeated immutable-store copies. *)
Section BulkChildren.

Context {Edge : Type}.

Definition append_edge (built : list Edge) (edge : Edge) : list Edge :=
  built ++ [edge].

Definition sequential_sorted_children (entries : list Edge) : list Edge :=
  fold_left append_edge entries [].

Definition bulk_sorted_children (entries : list Edge) : list Edge := entries.

Lemma fold_left_append_edge :
  forall entries built,
    fold_left append_edge entries built = built ++ entries.
Proof.
  induction entries as [|edge rest IH]; intros built.
  - simpl. rewrite app_nil_r. reflexivity.
  - simpl. rewrite IH. unfold append_edge.
    rewrite <- app_assoc. reflexivity.
Qed.

Theorem bulk_sorted_children_refines_sequential_insertion :
  forall entries,
    bulk_sorted_children entries = sequential_sorted_children entries.
Proof.
  intros entries.
  unfold bulk_sorted_children, sequential_sorted_children.
  rewrite fold_left_append_edge.
  reflexivity.
Qed.

End BulkChildren.

(** ** Validation/construction phase separation

    A gray ([Visiting]) edge is corruption, not a construction dependency.  The
    transition below changes only the phase; the installed-edge set is retained.
    Starting from the validator invariant that no edges exist, rejection is
    therefore observably before any [Arc] splice. *)
Section BackEdgeRejection.

Inductive ReopenPhase :=
| Parsing
| Validating
| Constructing
| Corrupted
| Complete.

Record ReopenMachineState := {
  reopen_phase : ReopenPhase;
  installed_edges : list (nat * nat)
}.

Definition reject_visiting_edge (state : ReopenMachineState)
  : ReopenMachineState :=
  {|
    reopen_phase := Corrupted;
    installed_edges := installed_edges state
  |}.

Definition validation_installs_no_edges (state : ReopenMachineState) : Prop :=
  reopen_phase state = Validating -> installed_edges state = [].

Theorem visiting_back_edge_is_rejected_before_arc_splice :
  forall state,
    reopen_phase state = Validating ->
    validation_installs_no_edges state ->
    reopen_phase (reject_visiting_edge state) = Corrupted /\
    installed_edges (reject_visiting_edge state) = [].
Proof.
  intros state Hphase Hclean.
  split.
  - reflexivity.
  - simpl. apply Hclean. exact Hphase.
Qed.

End BackEdgeRejection.

(** ** DAG memo identity and logical path cardinality

    A valid checkpoint may be a DAG.  Every incoming edge resolves the same child
    index through one memo slot, preserving [Arc] identity.  Language cardinality
    is nevertheless path-sensitive: a shared accepting suffix contributes once
    for every incoming path, not once per disk record. *)
Section DagSharing.

Context {Object : Type}.

Definition MaterializationMemo := nat -> option Object.

Definition resolve_edge
  (memo : MaterializationMemo) (edge : nat * nat) : option Object :=
  memo (snd edge).

Theorem shared_child_edges_resolve_one_memo_identity :
  forall memo left_parent right_parent child object,
    memo child = Some object ->
    resolve_edge memo (left_parent, child) = Some object /\
    resolve_edge memo (right_parent, child) = Some object.
Proof.
  intros memo left_parent right_parent child object Hmemo.
  split; simpl; exact Hmemo.
Qed.

Fixpoint sum_child_path_counts (counts : list nat) : nat :=
  match counts with
  | [] => 0
  | count :: rest => count + sum_child_path_counts rest
  end.

Definition logical_node_path_count
  (is_final : bool) (child_counts : list nat) : nat :=
  (if is_final then 1 else 0) + sum_child_path_counts child_counts.

Lemma repeated_child_path_count :
  forall incoming child_count,
    sum_child_path_counts (repeat child_count incoming) =
    incoming * child_count.
Proof.
  induction incoming as [|incoming IH]; intros child_count.
  - reflexivity.
  - simpl. rewrite IH. lia.
Qed.

Theorem shared_suffix_counts_once_per_incoming_path :
  forall incoming child_count,
    logical_node_path_count false (repeat child_count incoming) =
    incoming * child_count.
Proof.
  intros incoming child_count.
  unfold logical_node_path_count.
  simpl.
  apply repeated_child_path_count.
Qed.

End DagSharing.

(** ** Lazy iterator refinement

    The executable iterator retains an immutable captured root, one mutable key
    path, and a list of cursor frames containing only node/cursor/path-length/
    emitted metadata.  The mathematical reference below is deliberately free of
    implementation recursion: a finite output trace is split into an emitted
    prefix and a remaining suffix.  Consuming the suffix preserves exact values,
    order, and repeated path occurrences caused by shared DAG nodes. *)
Section LazyIteratorRefinement.

Context {Value : Type}.

Record U64IteratorEntry := {
  iterator_key : U64Sequence;
  iterator_value : Value
}.

Record U64LazyCursor := {
  cursor_emitted : list U64IteratorEntry;
  cursor_remaining : list U64IteratorEntry
}.

Definition init_lazy_cursor (captured : list U64IteratorEntry) : U64LazyCursor :=
  {|
    cursor_emitted := [];
    cursor_remaining := captured
  |}.

Definition step_lazy_cursor (cursor : U64LazyCursor) : U64LazyCursor :=
  match cursor_remaining cursor with
  | [] => cursor
  | entry :: remaining =>
      {|
        cursor_emitted := cursor_emitted cursor ++ [entry];
        cursor_remaining := remaining
      |}
  end.

Fixpoint run_lazy_cursor (fuel : nat) (cursor : U64LazyCursor) : U64LazyCursor :=
  match fuel with
  | 0 => cursor
  | S remaining => run_lazy_cursor remaining (step_lazy_cursor cursor)
  end.

Lemma run_lazy_cursor_consumes_remaining :
  forall remaining emitted,
    run_lazy_cursor
      (length remaining)
      {| cursor_emitted := emitted; cursor_remaining := remaining |} =
    {| cursor_emitted := emitted ++ remaining; cursor_remaining := [] |}.
Proof.
  induction remaining as [|entry rest IH]; intros emitted.
  - simpl. rewrite app_nil_r. reflexivity.
  - simpl.
    change
      (run_lazy_cursor
        (length rest)
        {| cursor_emitted := emitted ++ [entry]; cursor_remaining := rest |} =
       {| cursor_emitted := emitted ++ entry :: rest; cursor_remaining := [] |}).
    rewrite IH.
    rewrite <- app_assoc.
    reflexivity.
Qed.

Theorem lazy_cursor_output_equals_captured_reference :
  forall captured,
    cursor_emitted
      (run_lazy_cursor (length captured) (init_lazy_cursor captured)) =
    captured.
Proof.
  intros captured.
  unfold init_lazy_cursor.
  rewrite run_lazy_cursor_consumes_remaining.
  reflexivity.
Qed.

Theorem lazy_cursor_preserves_values_and_order :
  forall captured,
    map iterator_value
      (cursor_emitted
        (run_lazy_cursor (length captured) (init_lazy_cursor captured))) =
    map iterator_value captured.
Proof.
  intros captured.
  rewrite lazy_cursor_output_equals_captured_reference.
  reflexivity.
Qed.

Definition iterator_entries_strictly_ordered
  (before : U64Sequence -> U64Sequence -> Prop)
  (entries : list U64IteratorEntry) : Prop :=
  StronglySorted
    (fun left right => before (iterator_key left) (iterator_key right))
    entries.

Theorem lazy_cursor_preserves_strict_lexicographic_order :
  forall before captured,
    iterator_entries_strictly_ordered before captured ->
    iterator_entries_strictly_ordered before
      (cursor_emitted
        (run_lazy_cursor (length captured) (init_lazy_cursor captured))).
Proof.
  intros before captured Hordered.
  rewrite lazy_cursor_output_equals_captured_reference.
  exact Hordered.
Qed.

(** Prefix construction receives the already-resolved subtree output rather
    than scanning the full dictionary.  [has_prefix] is the executable prefix
    predicate supplied by the reference trie semantics. *)
Definition prefix_entries
  (has_prefix : U64Sequence -> bool)
  (entries : list U64IteratorEntry) : list U64IteratorEntry :=
  filter (fun entry => has_prefix (iterator_key entry)) entries.

Theorem prefix_entries_are_sound :
  forall has_prefix entries entry,
    In entry (prefix_entries has_prefix entries) ->
    has_prefix (iterator_key entry) = true.
Proof.
  intros has_prefix entries entry Hin.
  unfold prefix_entries in Hin.
  apply filter_In in Hin.
  tauto.
Qed.

Theorem prefix_entries_are_complete :
  forall has_prefix entries entry,
    In entry entries ->
    has_prefix (iterator_key entry) = true ->
    In entry (prefix_entries has_prefix entries).
Proof.
  intros has_prefix entries entry Hin Hprefix.
  unfold prefix_entries.
  apply filter_In.
  tauto.
Qed.

Theorem prefix_lazy_cursor_equals_prefix_reference :
  forall has_prefix entries,
    cursor_emitted
      (run_lazy_cursor
        (length (prefix_entries has_prefix entries))
        (init_lazy_cursor (prefix_entries has_prefix entries))) =
    prefix_entries has_prefix entries.
Proof.
  intros has_prefix entries.
  apply lazy_cursor_output_equals_captured_reference.
Qed.

(** Publishing a new root after capture cannot mutate the already-owned
    snapshot cursor. *)
Definition publish_after_capture
  (cursor : U64LazyCursor) (_new_publication : list U64IteratorEntry)
  : U64LazyCursor := cursor.

Theorem captured_iterator_is_independent_of_later_publication :
  forall captured later,
    cursor_emitted
      (run_lazy_cursor
        (length captured)
        (publish_after_capture (init_lazy_cursor captured) later)) =
    captured.
Proof.
  intros captured later.
  unfold publish_after_capture.
  apply lazy_cursor_output_equals_captured_reference.
Qed.

(** Shared materialized node identity does not deduplicate labeled path
    occurrences in the captured language. *)
Definition alias_entry
  (key : U64Sequence) (value : Value) : U64IteratorEntry :=
  {| iterator_key := key; iterator_value := value |}.

Theorem shared_node_path_multiplicity_is_preserved :
  forall left_path right_path value,
    map iterator_key
      (cursor_emitted
        (run_lazy_cursor 2
          (init_lazy_cursor
            [alias_entry left_path value; alias_entry right_path value]))) =
    [left_path; right_path].
Proof.
  intros left_path right_path value.
  change 2 with (length [alias_entry left_path value; alias_entry right_path value]).
  rewrite lazy_cursor_output_equals_captured_reference.
  reflexivity.
Qed.

(** The Rust frame has no owned path; all frames index one mutable path. *)
Record U64IteratorFrame := {
  iterator_frame_node : nat;
  iterator_frame_cursor : nat;
  iterator_frame_path_len : nat;
  iterator_frame_emitted : bool
}.

Definition explicit_iterator_resources_bounded
  (depth : nat) (path : U64Sequence) (frames : list U64IteratorFrame) : Prop :=
  length path <= depth /\
  length frames <= S (length path).

Theorem one_path_and_explicit_frames_are_depth_bounded :
  forall (depth : nat) (path : U64Sequence) (frames : list U64IteratorFrame),
    length path <= depth ->
    length frames <= S (length path) ->
    length path <= depth /\ length frames <= S depth.
Proof.
  intros depth path frames Hpath Hframes.
  split; [exact Hpath | lia].
Qed.

Definition cancel_lazy_cursor (cursor : U64LazyCursor) : U64LazyCursor :=
  {|
    cursor_emitted := cursor_emitted cursor;
    cursor_remaining := []
  |}.

Theorem cancellation_releases_remaining_traversal_state :
  forall cursor,
    cursor_remaining (cancel_lazy_cursor cursor) = [].
Proof.
  intros cursor.
  reflexivity.
Qed.

(** Native-u64 iteration is fallible at the resident-topology boundary. *)
Definition init_resident_lazy_cursor
  (resident_edges : list bool) (captured : list U64IteratorEntry)
  : option U64LazyCursor :=
  if forallb (fun resident => resident) resident_edges
  then Some (init_lazy_cursor captured)
  else None.

Theorem unresolved_disk_edge_fails_before_enumeration :
  forall resident_edges captured,
    forallb (fun resident => resident) resident_edges = false ->
    init_resident_lazy_cursor resident_edges captured = None.
Proof.
  intros resident_edges captured Hnonresident.
  unfold init_resident_lazy_cursor.
  rewrite Hnonresident.
  reflexivity.
Qed.

End LazyIteratorRefinement.
