(* End-to-end persistent public trace specification.

   This model composes the public logical map, checkpoint image, WAL tail,
   compaction rewrite, crash/reopen replay, and vocabulary bijection laws used
   by the executable correspondence tests. It intentionally stays at the
   public API level: storage allocation, syscall outcomes, and byte layouts are
   covered by the lower-level specs. *)

From Stdlib Require Import List Arith Lia.
Import ListNotations.

Definition Key := nat.
Definition Value := nat.
Definition Index := nat.
Definition Term := nat.

Definition RefMap := Key -> option Value.

Definition empty_map : RefMap := fun _ => None.

Definition lookup (m : RefMap) (k : Key) : option Value := m k.

Definition map_put (m : RefMap) (k : Key) (v : Value) : RefMap :=
  fun q => if Nat.eqb q k then Some v else m q.

Definition map_remove (m : RefMap) (k : Key) : RefMap :=
  fun q => if Nat.eqb q k then None else m q.

Lemma map_put_lookup_same :
  forall m k v,
    lookup (map_put m k v) k = Some v.
Proof.
  intros. unfold lookup, map_put. rewrite Nat.eqb_refl. reflexivity.
Qed.

Lemma map_put_lookup_other :
  forall m k q v,
    q <> k ->
    lookup (map_put m k v) q = lookup m q.
Proof.
  intros m k q v Hneq.
  unfold lookup, map_put.
  destruct (Nat.eqb q k) eqn:Heq.
  - apply Nat.eqb_eq in Heq. subst. contradiction.
  - reflexivity.
Qed.

Lemma map_remove_lookup_same :
  forall m k,
    lookup (map_remove m k) k = None.
Proof.
  intros. unfold lookup, map_remove. rewrite Nat.eqb_refl. reflexivity.
Qed.

Lemma map_remove_lookup_other :
  forall m k q,
    q <> k ->
    lookup (map_remove m k) q = lookup m q.
Proof.
  intros m k q Hneq.
  unfold lookup, map_remove.
  destruct (Nat.eqb q k) eqn:Heq.
  - apply Nat.eqb_eq in Heq. subst. contradiction.
  - reflexivity.
Qed.

Inductive TraceOp :=
| TracePut : Key -> Value -> TraceOp
| TraceDelete : Key -> TraceOp
| TraceSync : TraceOp
| TraceCheckpoint : TraceOp
| TraceCompactRewrite : TraceOp
| TraceCrashReopen : TraceOp.

Definition apply_mutation (op : TraceOp) (m : RefMap) : RefMap :=
  match op with
  | TracePut k v => map_put m k v
  | TraceDelete k => map_remove m k
  | TraceSync => m
  | TraceCheckpoint => m
  | TraceCompactRewrite => m
  | TraceCrashReopen => m
  end.

Fixpoint replay_ops (ops : list TraceOp) (m : RefMap) : RefMap :=
  match ops with
  | [] => m
  | op :: rest => replay_ops rest (apply_mutation op m)
  end.

Lemma replay_ops_app_lookup :
  forall xs ys m k,
    lookup (replay_ops (xs ++ ys) m) k =
    lookup (replay_ops ys (replay_ops xs m)) k.
Proof.
  induction xs as [|op rest IH]; intros ys m k; simpl.
  - reflexivity.
  - apply IH.
Qed.

Record PersistentTraceState := {
  live_map : RefMap;
  checkpoint_map : RefMap;
  wal_tail : list TraceOp
}.

Definition append_public_mutation
  (s : PersistentTraceState)
  (op : TraceOp)
  : PersistentTraceState :=
  {|
    live_map := apply_mutation op (live_map s);
    checkpoint_map := checkpoint_map s;
    wal_tail := wal_tail s ++ [op]
  |}.

Definition publish_checkpoint (s : PersistentTraceState) : PersistentTraceState :=
  {|
    live_map := live_map s;
    checkpoint_map := live_map s;
    wal_tail := []
  |}.

Definition compact_rewrite (s : PersistentTraceState) : PersistentTraceState :=
  {|
    live_map := live_map s;
    checkpoint_map := live_map s;
    wal_tail := []
  |}.

Definition crash_reopen (s : PersistentTraceState) : PersistentTraceState :=
  {|
    live_map := replay_ops (wal_tail s) (checkpoint_map s);
    checkpoint_map := checkpoint_map s;
    wal_tail := wal_tail s
  |}.

Theorem append_public_mutation_live_lookup :
  forall s op k,
    lookup (live_map (append_public_mutation s op)) k =
    lookup (apply_mutation op (live_map s)) k.
Proof.
  reflexivity.
Qed.

Theorem checkpoint_preserves_live_lookup :
  forall s k,
    lookup (live_map (publish_checkpoint s)) k = lookup (live_map s) k.
Proof.
  reflexivity.
Qed.

Theorem checkpoint_clears_wal_tail :
  forall s,
    wal_tail (publish_checkpoint s) = [].
Proof.
  reflexivity.
Qed.

Theorem compact_rewrite_preserves_live_lookup :
  forall s k,
    lookup (live_map (compact_rewrite s)) k = lookup (live_map s) k.
Proof.
  reflexivity.
Qed.

Theorem compact_rewrite_clears_wal_tail :
  forall s,
    wal_tail (compact_rewrite s) = [].
Proof.
  reflexivity.
Qed.

Theorem crash_reopen_lookup_is_checkpoint_plus_tail :
  forall s k,
    lookup (live_map (crash_reopen s)) k =
    lookup (replay_ops (wal_tail s) (checkpoint_map s)) k.
Proof.
  reflexivity.
Qed.

Theorem put_after_checkpoint_survives_crash_reopen :
  forall s k v q,
    let after_checkpoint := publish_checkpoint s in
    let after_put := append_public_mutation after_checkpoint (TracePut k v) in
    lookup (live_map (crash_reopen after_put)) q =
    lookup (map_put (live_map s) k v) q.
Proof.
  reflexivity.
Qed.

Theorem delete_after_checkpoint_survives_crash_reopen :
  forall s k q,
    let after_checkpoint := publish_checkpoint s in
    let after_delete := append_public_mutation after_checkpoint (TraceDelete k) in
    lookup (live_map (crash_reopen after_delete)) q =
    lookup (map_remove (live_map s) k) q.
Proof.
  reflexivity.
Qed.

Theorem compact_then_crash_reopen_preserves_lookup :
  forall s k,
    lookup (live_map (crash_reopen (compact_rewrite s))) k =
    lookup (live_map s) k.
Proof.
  reflexivity.
Qed.

Definition TermMap := Term -> option Index.
Definition IndexMap := Index -> option Term.

Record VocabState := {
  next_index : Index;
  term_to_index : TermMap;
  index_to_term : IndexMap
}.

Definition empty_vocab : VocabState :=
  {|
    next_index := 0;
    term_to_index := fun _ => None;
    index_to_term := fun _ => None
  |}.

Definition index_map_put
  (m : IndexMap)
  (idx : Index)
  (term : Term)
  : IndexMap :=
  fun q => if Nat.eqb q idx then Some term else m q.

Definition vocab_insert (term : Term) (s : VocabState) : VocabState * Index :=
  match term_to_index s term with
  | Some idx => (s, idx)
  | None =>
      ({|
         next_index := S (next_index s);
         term_to_index := map_put (term_to_index s) term (next_index s);
         index_to_term := index_map_put (index_to_term s) (next_index s) term
       |},
       next_index s)
  end.

Theorem vocab_insert_duplicate_returns_existing_index :
  forall s term idx,
    term_to_index s term = Some idx ->
    snd (vocab_insert term s) = idx.
Proof.
  intros s term idx Hlookup.
  unfold vocab_insert. rewrite Hlookup. reflexivity.
Qed.

Theorem vocab_insert_duplicate_preserves_forward_lookup :
  forall s term idx q,
    term_to_index s term = Some idx ->
    term_to_index (fst (vocab_insert term s)) q = term_to_index s q.
Proof.
  intros s term idx q Hlookup.
  unfold vocab_insert. rewrite Hlookup. reflexivity.
Qed.

Theorem vocab_insert_duplicate_preserves_next_index :
  forall s term idx,
    term_to_index s term = Some idx ->
    next_index (fst (vocab_insert term s)) = next_index s.
Proof.
  intros s term idx Hlookup.
  unfold vocab_insert. rewrite Hlookup. reflexivity.
Qed.

Theorem vocab_insert_fresh_forward_lookup :
  forall s term,
    term_to_index s term = None ->
    term_to_index (fst (vocab_insert term s)) term = Some (next_index s).
Proof.
  intros s term Hfresh.
  unfold vocab_insert. rewrite Hfresh. simpl.
  apply map_put_lookup_same.
Qed.

Theorem vocab_insert_fresh_reverse_lookup :
  forall s term,
    term_to_index s term = None ->
    index_to_term (fst (vocab_insert term s)) (next_index s) = Some term.
Proof.
  intros s term Hfresh.
  unfold vocab_insert. rewrite Hfresh. simpl.
  unfold index_map_put. rewrite Nat.eqb_refl. reflexivity.
Qed.

Theorem vocab_insert_fresh_returns_next_index :
  forall s term,
    term_to_index s term = None ->
    snd (vocab_insert term s) = next_index s.
Proof.
  intros s term Hfresh.
  unfold vocab_insert. rewrite Hfresh. reflexivity.
Qed.

Theorem vocab_insert_fresh_advances_next_index :
  forall s term,
    term_to_index s term = None ->
    next_index (fst (vocab_insert term s)) = S (next_index s).
Proof.
  intros s term Hfresh.
  unfold vocab_insert. rewrite Hfresh. reflexivity.
Qed.

Theorem vocab_insert_fresh_preserves_other_forward_lookup :
  forall s term q,
    term_to_index s term = None ->
    q <> term ->
    term_to_index (fst (vocab_insert term s)) q = term_to_index s q.
Proof.
  intros s term q Hfresh Hneq.
  unfold vocab_insert. rewrite Hfresh. simpl.
  apply map_put_lookup_other. exact Hneq.
Qed.
