(** * AbiTraversalSnapshotSpec: ABI-local node-id arena laws

    Family FV obligation #11 (wave W2). Models the traversal-snapshot arena in
    src/bindings.rs: a captured vt.dictionary.v1 snapshot assigns ABI-local
    node identifiers lazily — the first time a provider node is exposed across
    the ABI it is appended to a mutex-guarded arena, and its arena index IS
    the identifier handed to consumers; expanded edge lists are memoized once
    per entry. The mutex serializes all arena access, so the model is a
    sequential state machine and every law is proved for all traces by
    structural induction (no interleaving content — the concurrency argument
    is exactly the mutex, recorded in UNSAFE_CONTRACTS.tsv).

    Laws proved (registry: formal-verification/ABI_INVARIANTS.tsv):

    - [LDICT-ARENA-1] assignment is stable: an already-assigned provider node
      keeps its index and the arena is unchanged;
    - [LDICT-ARENA-2] assignment is append-only: existing entries are never
      edited or moved by later assignments;
    - [LDICT-ARENA-3] identifiers are unambiguous: two assigned indices map
      to the same provider node only if they are equal (injectivity), and
      lookup is a function of the node;
    - [LDICT-ARENA-4] memoized edge lists are write-once: once filled they
      survive every later assignment or memoization attempt unchanged;
    - [LDICT-ARENA-5] well-formedness is preserved: no duplicate provider
      nodes, and every memoized edge child references an in-bounds index.

    The consumer-visible revision-immutability half lives in
    formal-verification/tla+/AbiProducerSnapshot.tla (obligation #10); the
    executable mirrors are tests/ffi_snapshot_law.rs and the node-id tests in
    src/bindings.rs.
*)

From Stdlib Require Import List.
From Stdlib Require Import Arith.
From Stdlib Require Import Lia.
From Stdlib Require Import Bool.
Import ListNotations.

(** ** Arena model *)

(** Provider-side node identity (opaque to consumers). *)
Definition RustNode := nat.

(** One memoized edge: a label and the child's ABI-local index. *)
Record ArenaEdge := mkArenaEdge {
  edge_label : nat;
  edge_child : nat
}.

(** One arena entry: the provider node it exposes and, once expanded, its
    memoized edge list. *)
Record ArenaEntry := mkArenaEntry {
  entry_node : RustNode;
  entry_edges : option (list ArenaEdge)
}.

(** The arena: entries in assignment order; the index is the ABI-local id. *)
Definition Arena := list ArenaEntry.

(** Find the index already assigned to a provider node, if any. *)
Fixpoint find_index (arena : Arena) (node : RustNode) : option nat :=
  match arena with
  | [] => None
  | entry :: rest =>
      if Nat.eq_dec (entry_node entry) node
      then Some 0
      else option_map S (find_index rest node)
  end.

(** Lazy assignment: reuse the existing index or append a fresh entry. *)
Definition assign (arena : Arena) (node : RustNode) : Arena * nat :=
  match find_index arena node with
  | Some index => (arena, index)
  | None => (arena ++ [mkArenaEntry node None], length arena)
  end.

(** Write-once memoization of an entry's edge list. *)
Fixpoint memoize (arena : Arena) (index : nat) (edges : list ArenaEdge)
  : Arena :=
  match arena, index with
  | [], _ => []
  | entry :: rest, 0 =>
      match entry_edges entry with
      | Some _ => entry :: rest        (* already memoized: write-once no-op *)
      | None => mkArenaEntry (entry_node entry) (Some edges) :: rest
      end
  | entry :: rest, S index' => entry :: memoize rest index' edges
  end.

(** ** Well-formedness *)

Definition nodes_of (arena : Arena) : list RustNode :=
  map entry_node arena.

Definition edge_children_bounded (arena : Arena) : Prop :=
  forall entry edges edge,
    In entry arena ->
    entry_edges entry = Some edges ->
    In edge edges ->
    edge_child edge < length arena.

Definition WellFormed (arena : Arena) : Prop :=
  NoDup (nodes_of arena) /\ edge_children_bounded arena.

(** ** Basic find/assign facts *)

Lemma find_index_bounds :
  forall arena node index,
    find_index arena node = Some index -> index < length arena.
Proof.
  induction arena as [| entry rest IH]; simpl; intros node index Hfind.
  - discriminate.
  - destruct (Nat.eq_dec (entry_node entry) node) as [_ | Hne].
    + injection Hfind as <-. lia.
    + destruct (find_index rest node) as [found |] eqn:Hrest;
        simpl in Hfind; [| discriminate].
      injection Hfind as <-.
      specialize (IH _ _ Hrest). lia.
Qed.

Lemma find_index_correct :
  forall arena node index,
    find_index arena node = Some index ->
    exists entry,
      nth_error arena index = Some entry /\ entry_node entry = node.
Proof.
  induction arena as [| entry rest IH]; simpl; intros node index Hfind.
  - discriminate.
  - destruct (Nat.eq_dec (entry_node entry) node) as [Heq | Hne].
    + injection Hfind as <-. exists entry. auto.
    + destruct (find_index rest node) as [found |] eqn:Hrest;
        simpl in Hfind; [| discriminate].
      injection Hfind as <-.
      destruct (IH _ _ Hrest) as [witness [Hnth Hnode]].
      exists witness. auto.
Qed.

Lemma find_index_none_not_in :
  forall arena node,
    find_index arena node = None -> ~ In node (nodes_of arena).
Proof.
  induction arena as [| entry rest IH]; simpl; intros node Hfind Hin.
  - exact Hin.
  - destruct (Nat.eq_dec (entry_node entry) node) as [Heq | Hne].
    + discriminate.
    + destruct (find_index rest node) eqn:Hrest; simpl in Hfind.
      * discriminate.
      * destruct Hin as [Heq | Hin]; [exact (Hne Heq) |].
        exact (IH _ Hrest Hin).
Qed.

Lemma find_index_in :
  forall arena node,
    In node (nodes_of arena) -> exists index, find_index arena node = Some index.
Proof.
  intros arena node Hin.
  destruct (find_index arena node) as [index |] eqn:Hfind.
  - eauto.
  - exfalso. exact (find_index_none_not_in _ _ Hfind Hin).
Qed.

Lemma find_index_append_fresh :
  forall arena node,
    find_index arena node = None ->
    find_index (arena ++ [mkArenaEntry node None]) node = Some (length arena).
Proof.
  induction arena as [| entry rest IH]; simpl; intros node Hfind.
  - destruct (Nat.eq_dec node node) as [_ | Hne]; [reflexivity | contradiction].
  - destruct (Nat.eq_dec (entry_node entry) node) as [Heq | Hne].
    + discriminate.
    + destruct (find_index rest node) eqn:Hrest; simpl in Hfind;
        [discriminate |].
      rewrite (IH _ Hrest). reflexivity.
Qed.

(** ** LDICT-ARENA-1: assignment stability *)

Theorem assign_stable :
  forall arena node index,
    find_index arena node = Some index ->
    assign arena node = (arena, index).
Proof.
  intros arena node index Hfind.
  unfold assign. rewrite Hfind. reflexivity.
Qed.

(** Assigning twice in a row yields the same index and no further growth. *)
Theorem assign_idempotent :
  forall arena node arena' index,
    assign arena node = (arena', index) ->
    assign arena' node = (arena', index).
Proof.
  intros arena node arena' index Hassign.
  unfold assign in *.
  destruct (find_index arena node) as [found |] eqn:Hfind.
  - injection Hassign as <- <-. rewrite Hfind. reflexivity.
  - injection Hassign as <- <-.
    rewrite (find_index_append_fresh _ _ Hfind). reflexivity.
Qed.

(** ** LDICT-ARENA-2: assignment is append-only *)

Theorem assign_append_only :
  forall arena node arena' index i,
    assign arena node = (arena', index) ->
    i < length arena ->
    nth_error arena' i = nth_error arena i.
Proof.
  intros arena node arena' index i Hassign Hlt.
  unfold assign in Hassign.
  destruct (find_index arena node) as [found |] eqn:Hfind.
  - injection Hassign as <- <-. reflexivity.
  - injection Hassign as <- <-.
    apply nth_error_app1. exact Hlt.
Qed.

Theorem assign_grows_by_at_most_one :
  forall arena node arena' index,
    assign arena node = (arena', index) ->
    length arena' = length arena \/ length arena' = S (length arena).
Proof.
  intros arena node arena' index Hassign.
  unfold assign in Hassign.
  destruct (find_index arena node) as [found |] eqn:Hfind;
    injection Hassign as <- <-.
  - left. reflexivity.
  - right. rewrite length_app. simpl. lia.
Qed.

(** The returned index is always in bounds of the resulting arena. *)
Theorem assign_index_in_bounds :
  forall arena node arena' index,
    assign arena node = (arena', index) ->
    index < length arena'.
Proof.
  intros arena node arena' index Hassign.
  unfold assign in Hassign.
  destruct (find_index arena node) as [found |] eqn:Hfind;
    injection Hassign as <- <-.
  - eapply find_index_bounds; eauto.
  - rewrite length_app. simpl. lia.
Qed.

(** The returned index exposes exactly the requested provider node. *)
Theorem assign_exposes_node :
  forall arena node arena' index entry,
    assign arena node = (arena', index) ->
    nth_error arena' index = Some entry ->
    entry_node entry = node.
Proof.
  intros arena node arena' index entry Hassign Hnth.
  unfold assign in Hassign.
  destruct (find_index arena node) as [found |] eqn:Hfind;
    injection Hassign as <- <-.
  - destruct (find_index_correct _ _ _ Hfind) as [witness [Hw Hn]].
    rewrite Hw in Hnth. injection Hnth as <-. exact Hn.
  - rewrite nth_error_app2 in Hnth by lia.
    rewrite Nat.sub_diag in Hnth. simpl in Hnth.
    injection Hnth as <-. reflexivity.
Qed.

(** ** LDICT-ARENA-3: identifiers are unambiguous *)

Lemma nodes_of_app :
  forall left right, nodes_of (left ++ right) = nodes_of left ++ nodes_of right.
Proof.
  intros. unfold nodes_of. apply map_app.
Qed.

(** Under NoDup, the same provider node never lives at two indices. *)
Theorem arena_ids_unambiguous :
  forall arena i j entry_i entry_j,
    NoDup (nodes_of arena) ->
    nth_error arena i = Some entry_i ->
    nth_error arena j = Some entry_j ->
    entry_node entry_i = entry_node entry_j ->
    i = j.
Proof.
  intros arena i j entry_i entry_j Hnodup Hi Hj Heq.
  assert (Hmi : nth_error (nodes_of arena) i = Some (entry_node entry_i)).
  { unfold nodes_of. rewrite nth_error_map, Hi. reflexivity. }
  assert (Hmj : nth_error (nodes_of arena) j = Some (entry_node entry_j)).
  { unfold nodes_of. rewrite nth_error_map, Hj. reflexivity. }
  rewrite Heq in Hmi.
  eapply NoDup_nth_error; eauto.
  - apply nth_error_Some. now rewrite Hmi.
  - congruence.
Qed.

(** Assignment preserves node uniqueness. *)
Lemma NoDup_snoc :
  forall (values : list RustNode) (value : RustNode),
    NoDup values -> ~ In value values -> NoDup (values ++ [value]).
Proof.
  induction values as [| head rest IH]; simpl; intros value Hnodup Hnotin.
  - constructor; [tauto | constructor].
  - inversion Hnodup as [| ? ? Hhead Hrest]; subst.
    constructor.
    + rewrite in_app_iff. simpl.
      intros [Hin | [Heq | []]].
      * exact (Hhead Hin).
      * apply Hnotin. left. symmetry. exact Heq.
    + apply IH; tauto.
Qed.

Theorem assign_preserves_nodup :
  forall arena node arena' index,
    NoDup (nodes_of arena) ->
    assign arena node = (arena', index) ->
    NoDup (nodes_of arena').
Proof.
  intros arena node arena' index Hnodup Hassign.
  unfold assign in Hassign.
  destruct (find_index arena node) as [found |] eqn:Hfind;
    injection Hassign as <- <-.
  - exact Hnodup.
  - rewrite nodes_of_app. simpl.
    apply NoDup_snoc; [exact Hnodup |].
    exact (find_index_none_not_in _ _ Hfind).
Qed.

(** ** LDICT-ARENA-4: memoized edges are write-once *)

Theorem memoize_write_once :
  forall arena index old_edges new_edges i entry,
    nth_error arena index = Some entry ->
    entry_edges entry = Some old_edges ->
    nth_error (memoize arena index new_edges) i = nth_error arena i.
Proof.
  induction arena as [| head rest IH]; simpl; intros index old new i entry Hnth Hedges.
  - destruct index; discriminate.
  - destruct index as [| index'].
    + injection Hnth as <-. rewrite Hedges. reflexivity.
    + destruct i as [| i']; simpl.
      * reflexivity.
      * eapply IH; eauto.
Qed.

(** Memoization never touches other entries. *)
Theorem memoize_local :
  forall arena index edges i,
    i <> index ->
    nth_error (memoize arena index edges) i = nth_error arena i.
Proof.
  induction arena as [| head rest IH]; simpl; intros index edges i Hne.
  - reflexivity.
  - destruct index as [| index'].
    + destruct (entry_edges head) eqn:Hh.
      * reflexivity.
      * destruct i as [| i']; [contradiction | reflexivity].
    + destruct i as [| i']; simpl.
      * reflexivity.
      * apply IH. lia.
Qed.

(** Memoization preserves the exposed provider node at every index. *)
Theorem memoize_preserves_nodes :
  forall arena index edges,
    nodes_of (memoize arena index edges) = nodes_of arena.
Proof.
  induction arena as [| head rest IH]; simpl; intros index edges.
  - reflexivity.
  - destruct index as [| index'].
    + destruct (entry_edges head); reflexivity.
    + simpl. rewrite IH. reflexivity.
Qed.

Theorem memoize_preserves_length :
  forall arena index edges,
    length (memoize arena index edges) = length arena.
Proof.
  induction arena as [| head rest IH]; simpl; intros index edges.
  - reflexivity.
  - destruct index as [| index'].
    + destruct (entry_edges head); reflexivity.
    + simpl. rewrite IH. reflexivity.
Qed.

(** ** LDICT-ARENA-5: well-formedness preservation *)

(** Appending an entry that carries no memoized edges preserves the child
    bound (this is exactly the entry [assign] appends). *)
Lemma edge_children_bounded_extend_fresh :
  forall arena node,
    edge_children_bounded arena ->
    edge_children_bounded (arena ++ [mkArenaEntry node None]).
Proof.
  intros arena node Hbounded entry edges edge Hin Hedges Hedge.
  rewrite length_app. simpl.
  apply in_app_or in Hin as [Hin | [Heq | []]].
  - specialize (Hbounded _ _ _ Hin Hedges Hedge). lia.
  - subst entry. discriminate.
Qed.

Theorem assign_preserves_wellformed :
  forall arena node arena' index,
    WellFormed arena ->
    assign arena node = (arena', index) ->
    WellFormed arena'.
Proof.
  intros arena node arena' index [Hnodup Hbounded] Hassign.
  split.
  - eapply assign_preserves_nodup; eauto.
  - unfold assign in Hassign.
    destruct (find_index arena node) as [found |] eqn:Hfind;
      injection Hassign as <- <-.
    + exact Hbounded.
    + apply edge_children_bounded_extend_fresh. exact Hbounded.
Qed.

(** Every entry of a memoized arena is either an untouched original or the
    one updated entry, which keeps its node and gains exactly [edges]. *)
Lemma memoize_in_characterization :
  forall arena index edges entry,
    In entry (memoize arena index edges) ->
    In entry arena \/
    (exists original,
        nth_error arena index = Some original /\
        entry_edges original = None /\
        entry = mkArenaEntry (entry_node original) (Some edges)).
Proof.
  induction arena as [| head rest IH]; simpl; intros index edges entry Hin.
  - contradiction.
  - destruct index as [| index'].
    + destruct (entry_edges head) eqn:Hhead.
      * left. exact Hin.
      * destruct Hin as [Heq | Hin].
        -- right. exists head. subst entry. auto.
        -- left. right. exact Hin.
    + destruct Hin as [Heq | Hin].
      * left. left. exact Heq.
      * destruct (IH _ _ _ Hin) as [Hold | [original [Hnth [Hnone Heq]]]].
        -- left. right. exact Hold.
        -- right. exists original. auto.
Qed.

Theorem memoize_preserves_wellformed :
  forall arena index edges,
    WellFormed arena ->
    (forall edge, In edge edges -> edge_child edge < length arena) ->
    WellFormed (memoize arena index edges).
Proof.
  intros arena index edges [Hnodup Hbounded] Hchildren.
  split.
  - rewrite memoize_preserves_nodes. exact Hnodup.
  - intros entry stored edge Hin Hstored Hedge.
    rewrite memoize_preserves_length.
    destruct (memoize_in_characterization _ _ _ _ Hin)
      as [Hold | [original [Hnth [Hnone Heq]]]].
    + exact (Hbounded _ _ _ Hold Hstored Hedge).
    + subst entry. simpl in Hstored.
      injection Hstored as <-.
      exact (Hchildren _ Hedge).
Qed.
