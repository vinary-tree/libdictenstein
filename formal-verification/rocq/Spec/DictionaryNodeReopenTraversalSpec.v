(** * DictionaryNodeReopenTraversalSpec: Faulting Traversal Completeness

    Models the correctness property restored by the HEAD fix to
    [PersistentARTrieCharNode::{transition,edges}]: after a persistent char trie is
    checkpointed and reopened from disk, the root is resident but its children are
    *swizzled* (on-disk). A [DictionaryNode] graph walk (the API liblevenshtein's
    transducer drives) must FAULT swizzled children in, so it observes the same
    logical content as a fully-resident walk — i.e. faulting makes the swizzled
    subtree observationally equal to the resident one.

    The checked claims are above the filesystem boundary:

    - the faulting walk's result is INVARIANT under residency (swizzled flags), so it
      equals the snapshot regardless of how much is on disk;
    - reopening (marking every edge on-disk) does not change the faulting walk;
    - the non-faulting walk (the pre-fix `get_child`/`iter_children` that drop
      swizzled children) is a SUBSET of the faulting walk, and is strictly
      incomplete whenever a swizzled child carries a final node;
    - `edges` enumerates all children regardless of residency, whereas the
      non-faulting enumeration drops swizzled children.

    No [Admitted], no [Axiom], no [Parameter]. *)

From Stdlib Require Import Lists.List.
From Stdlib Require Import Arith.PeanoNat.
Import ListNotations.

Section DictionaryNodeReopen.
Context {V : Type}.

(** A character-trie node: finality, an optional value, and labeled children. Each
    child carries a [swizzled] flag: [true] means the child's slot is on-disk. *)
Inductive Trie : Type :=
| TNode : bool -> option V -> list (nat * bool * Trie) -> Trie.

(** The derived induction principle ignores the nested list, so we build a strong
    one that supplies a [Forall] hypothesis over the children. *)
Fixpoint Trie_ind' (P : Trie -> Prop)
  (step : forall f v ch, Forall (fun e => P (snd e)) ch -> P (TNode f v ch))
  (t : Trie) {struct t} : P t :=
  match t with
  | TNode f v ch =>
      step f v ch
        ((fix go (l : list (nat * bool * Trie)) : Forall (fun e => P (snd e)) l :=
            match l with
            | [] => Forall_nil _
            | e :: tl => Forall_cons e (Trie_ind' P step (snd e)) (go tl)
            end) ch)
  end.

Definition here_entries (is_final : bool) (val : option V) (path : list nat)
  : list (list nat * V) :=
  if is_final then match val with
                   | Some v => [(rev path, v)]
                   | None => []
                   end
  else [].

(** The logical content: every [(path, value)] at a final node, reached by FAULTING
    into every child regardless of its swizzled flag. This is the snapshot. *)
Fixpoint faulting_collect (path : list nat) (t : Trie) : list (list nat * V) :=
  match t with
  | TNode is_final val ch =>
      here_entries is_final val path
        ++ flat_map (fun e : nat * bool * Trie =>
                       let '(lbl, _sw, sub) := e in faulting_collect (lbl :: path) sub)
                    ch
  end.

(** The buggy NON-faulting walk: a swizzled child is dropped (contributes nothing),
    modeling the pre-fix `get_child`/`iter_children` that filter through `as_ptr`. *)
Fixpoint nonfaulting_collect (path : list nat) (t : Trie) : list (list nat * V) :=
  match t with
  | TNode is_final val ch =>
      here_entries is_final val path
        ++ flat_map (fun e : nat * bool * Trie =>
                       let '(lbl, sw, sub) := e in
                       if sw then [] else nonfaulting_collect (lbl :: path) sub)
                    ch
  end.

(** Erase residency: set every edge resident (swizzled := false). *)
Fixpoint erase (t : Trie) : Trie :=
  match t with
  | TNode f v ch =>
      TNode f v (map (fun e : nat * bool * Trie =>
                        let '(lbl, _sw, sub) := e in (lbl, false, erase sub)) ch)
  end.

(** Reopen models the worst case: every edge becomes on-disk (swizzled := true). *)
Fixpoint reopen (t : Trie) : Trie :=
  match t with
  | TNode f v ch =>
      TNode f v (map (fun e : nat * bool * Trie =>
                        let '(lbl, _sw, sub) := e in (lbl, true, reopen sub)) ch)
  end.

(** ** Faulting traversal is residency-invariant *)

(** The faulting walk gives the same result whether or not children are swizzled —
    it depends only on the swizzle-erased structure. This is the heart of "faulting
    makes the swizzled subtree observationally equal to the resident one." *)
Lemma faulting_collect_erase :
  forall (t : Trie) (path : list nat),
    faulting_collect path t = faulting_collect path (erase t).
Proof.
  intros t.
  induction t as [f v ch IH] using Trie_ind'.
  intros path. simpl.
  f_equal.
  (* The `here` entries are identical; reduce to the children list. *)
  induction ch as [| e tl IHch].
  - reflexivity.
  - destruct e as [[lbl sw] sub]. simpl.
    inversion IH as [| e0 tl0 Hhead Htail Heq]; subst.
    simpl in Hhead.
    rewrite (Hhead (lbl :: path)).
    f_equal.
    apply IHch. exact Htail.
Qed.

(** Two tries with the same erased structure yield the same faulting walk. *)
Lemma faulting_collect_swizzle_invariant :
  forall (t1 t2 : Trie) (path : list nat),
    erase t1 = erase t2 ->
    faulting_collect path t1 = faulting_collect path t2.
Proof.
  intros t1 t2 path Heq.
  rewrite (faulting_collect_erase t1 path).
  rewrite (faulting_collect_erase t2 path).
  rewrite Heq. reflexivity.
Qed.

(** Reopening only flips swizzled flags, so it has the same erased structure. *)
Lemma erase_reopen :
  forall (t : Trie), erase (reopen t) = erase t.
Proof.
  intros t.
  induction t as [f v ch IH] using Trie_ind'.
  simpl. f_equal.
  induction ch as [| e tl IHch].
  - reflexivity.
  - destruct e as [[lbl sw] sub]. simpl.
    inversion IH as [| e0 tl0 Hhead Htail Heq]; subst.
    simpl in Hhead.
    rewrite Hhead.
    f_equal.
    apply IHch. exact Htail.
Qed.

(** MAIN: a reopened trie's faulting walk equals the original's. After a
    checkpoint/reopen, the [DictionaryNode] walk reaches exactly the pre-checkpoint
    snapshot — the regression the HEAD fix repairs. *)
Theorem faulting_walk_equals_snapshot_after_reopen :
  forall (t : Trie) (path : list nat),
    faulting_collect path (reopen t) = faulting_collect path t.
Proof.
  intros t path.
  apply faulting_collect_swizzle_invariant.
  apply erase_reopen.
Qed.

(** ** The non-faulting walk is incomplete *)

(** Every entry the non-faulting walk finds, the faulting walk also finds. *)
Lemma nonfaulting_subset_faulting :
  forall (t : Trie) (path : list nat) (e : list nat * V),
    In e (nonfaulting_collect path t) ->
    In e (faulting_collect path t).
Proof.
  intros t.
  induction t as [f v ch IH] using Trie_ind'.
  intros path e Hin. simpl in *.
  apply in_app_or in Hin. apply in_or_app.
  destruct Hin as [Hhere | Hrec].
  - left. exact Hhere.
  - right.
    apply in_flat_map in Hrec.
    destruct Hrec as [c [Hc Hin_c]].
    apply in_flat_map.
    exists c. split; [exact Hc |].
    destruct c as [[lbl sw] sub]. simpl in *.
    destruct sw.
    + (* swizzled child: non-faulting contributes nothing, contradiction *)
      simpl in Hin_c. contradiction.
    + (* resident child: recurse via IH *)
      rewrite Forall_forall in IH.
      apply (IH (lbl, false, sub) Hc (lbl :: path) e).
      exact Hin_c.
Qed.

(** There is a trie where the faulting walk finds a key the non-faulting walk drops
    (a swizzled child carrying a final node) — so the non-faulting walk is strictly
    incomplete. This is the concrete bug. *)
Theorem nonfaulting_walk_incomplete :
  forall (v0 : V),
  exists (t : Trie) (e : list nat * V),
    In e (faulting_collect [] t) /\ ~ In e (nonfaulting_collect [] t).
Proof.
  intros v0.
  exists (TNode false None [(0, true, TNode true (Some v0) [])]).
  exists ([0], v0).
  split.
  - simpl. left. reflexivity.
  - simpl. intros [].
Qed.

(** ** `edges` enumerates all children regardless of residency *)

Definition child_label (e : nat * bool * Trie) : nat := fst (fst e).
Definition child_swizzled (e : nat * bool * Trie) : bool := snd (fst e).

(** The labels `edges` yields (faulting): ALL children. *)
Definition faulting_edges (t : Trie) : list nat :=
  match t with TNode _ _ ch => map child_label ch end.

(** The labels the non-faulting enumeration yields: resident children only. *)
Definition nonfaulting_edges (t : Trie) : list nat :=
  match t with
  | TNode _ _ ch => map child_label (filter (fun e => negb (child_swizzled e)) ch)
  end.

(** Faulting `edges` enumerates the same labels regardless of residency. *)
Theorem edges_enumerate_all_children :
  forall (t : Trie), faulting_edges t = faulting_edges (erase t).
Proof.
  intros [f v ch]. simpl.
  induction ch as [| e tl IHch].
  - reflexivity.
  - destruct e as [[lbl sw] sub]. simpl. f_equal. exact IHch.
Qed.

(** The non-faulting enumeration drops swizzled children: there is a trie whose
    faulting edges contain a label its non-faulting edges omit. *)
Theorem nonfaulting_edges_drop_swizzled :
  forall (v0 : V),
  exists (t : Trie),
    In 0 (faulting_edges t) /\ ~ In 0 (nonfaulting_edges t).
Proof.
  intros v0.
  exists (TNode false None [(0, true, TNode true (Some v0) [])]).
  split.
  - simpl. left. reflexivity.
  - simpl. intros [].
Qed.

End DictionaryNodeReopen.
