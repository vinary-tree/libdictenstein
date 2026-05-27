(** Persistent char-trie eviction DiskLocationRegistry model (HEAD commit
    f10c43e, feature G6).

    A checkpoint serializes the trie bottom-up, assigning each node a fresh
    on-disk pointer, and registers an [Entry] (path, ptr, depth) per node
    (serialize_char_node_to_disk + register_char). This specification models
    that serialize-and-register pass over a finite char tree and proves the
    invariants the Rust implementation and the companion TLA+ spec
    (EvictionRegistryPublication.tla) rely on:

    - every registered entry's depth equals its path length;
    - the root's entry carries exactly the pointer the serializer returned;
    - every registered pointer is one the serializer actually assigned (no
      fabricated pointers): it never exceeds the root pointer;
    - the pointer a node receives, and the resulting counter, do not depend on
      the path or whether the registry is built -- the registry is a pure
      side-effect on serialization, so recovery (which reads only the
      serialized bytes / on-disk root) is unaffected;
    - publication is the identity when verified and empty otherwise, and the
      recovered on-disk root never depends on the registry.

    The char tree is encoded as a mutual inductive Tree/Forest (the textbook
    rose-tree encoding) so that the bottom-up serializer is structurally
    recursive and proofs use the generated mutual induction principle.

    Filesystem/kernel ordering below a successful sync and certified Rust
    compilation are outside this proof boundary. No Admitted, Axiom, or
    Hypothesis is used. *)

From Stdlib Require Import List.
From Stdlib Require Import Arith.
From Stdlib Require Import Lia.
From Stdlib Require Import Bool.
Import ListNotations.

(* Char codepoints and on-disk pointers are modeled as natural numbers. *)
Definition Char := nat.
Definition Ptr := nat.

(* A finite char trie. A [node] carries an optional value (mirroring the
   [value : Option<V>] field of CharTrieNodeInner) and a [Forest] of
   (edge-char, child) entries (the adaptive radix node's children). *)
Inductive Tree : Type :=
| node : option nat -> Forest -> Tree
with Forest : Type :=
| fnil  : Forest
| fcons : Char -> Tree -> Forest -> Forest.

Scheme Tree_mut := Induction for Tree Sort Prop
  with Forest_mut := Induction for Forest Sort Prop.
Combined Scheme Tree_Forest_mut from Tree_mut, Forest_mut.

(* A registry entry mirrors EvictableCharNode { path, disk_ptr, depth }. *)
Record Entry := mkEntry { e_path : list Char; e_ptr : Ptr; e_depth : nat }.

Definition Registry := list Entry.

(* ------------------------------------------------------------------ *)
(* Serialize-and-register.                                              *)
(*                                                                      *)
(* [serialize t path next reg] serializes [t] (rooted at char-path      *)
(* [path]) using [next] as the next fresh pointer, accumulating into     *)
(* [reg]. It returns (this node's pointer, the next fresh pointer, the   *)
(* extended registry). Children are serialized first (left to right),    *)
(* then this node is assigned [next'] and registered -- the bottom-up    *)
(* order of serialize_char_node_to_disk, whose register_char call runs   *)
(* after the recursive child calls.                                      *)
(* ------------------------------------------------------------------ *)
Fixpoint serialize (t : Tree) (path : list Char) (next : Ptr) (reg : Registry)
  {struct t} : Ptr * Ptr * Registry :=
  match t with
  | node _ children =>
      let '(next', reg') := serialize_forest children path next reg in
      (next', S next', mkEntry path next' (length path) :: reg')
  end
with serialize_forest (f : Forest) (path : list Char) (next : Ptr) (reg : Registry)
  {struct f} : Ptr * Registry :=
  match f with
  | fnil => (next, reg)
  | fcons c child rest =>
      let '(_, next1, reg1) := serialize child (path ++ [c]) next reg in
      serialize_forest rest path next1 reg1
  end.

(* The public entry point: serialize the whole tree from the empty path. *)
Definition serialize_and_register (t : Tree) : Ptr * Registry :=
  let '(p, _, reg) := serialize t [] 0 [] in (p, reg).

(* Projections of the serialize triple. *)
Definition s_ptr  (r : Ptr * Ptr * Registry) : Ptr      := fst (fst r).
Definition s_next (r : Ptr * Ptr * Registry) : Ptr      := snd (fst r).
Definition s_reg  (r : Ptr * Ptr * Registry) : Registry := snd r.

(* Reduction equations (so proofs never fight [simpl] on the mutual fix). *)
Lemma serialize_node_eq :
  forall v children path next reg,
    serialize (node v children) path next reg =
      (let '(next', reg') := serialize_forest children path next reg in
         (next', S next', mkEntry path next' (length path) :: reg')).
Proof. reflexivity. Qed.

Lemma sf_nil_eq :
  forall path next reg, serialize_forest fnil path next reg = (next, reg).
Proof. reflexivity. Qed.

Lemma sf_cons_eq :
  forall c child rest path next reg,
    serialize_forest (fcons c child rest) path next reg =
      (let '(_, next1, reg1) := serialize child (path ++ [c]) next reg in
         serialize_forest rest path next1 reg1).
Proof. reflexivity. Qed.

(* ============================ Monotonicity =========================== *)

Lemma serialize_monotone_mut :
  (forall t path next reg, next <= s_next (serialize t path next reg)) /\
  (forall f path next reg, next <= fst (serialize_forest f path next reg)).
Proof.
  apply Tree_Forest_mut.
  - intros v children IHf path next reg.
    rewrite serialize_node_eq.
    pose proof (IHf path next reg) as Hf.
    destruct (serialize_forest children path next reg) as [scn scr] eqn:Hd.
    simpl in Hf. unfold s_next. simpl. lia.
  - intros path next reg. rewrite sf_nil_eq. simpl. lia.
  - intros c child IHchild rest IHrest path next reg.
    rewrite sf_cons_eq.
    destruct (serialize child (path ++ [c]) next reg) as [[cp cnext] creg] eqn:Hc.
    pose proof (IHchild (path ++ [c]) next reg) as Hch.
    unfold s_next in Hch. rewrite Hc in Hch. simpl in Hch.
    pose proof (IHrest path cnext creg) as Hr.
    lia.
Qed.

Definition serialize_next_monotone := proj1 serialize_monotone_mut.
Definition serialize_forest_next_monotone := proj2 serialize_monotone_mut.

(* Every Tree is a [node], so the next counter is one past the node pointer. *)
Lemma serialize_next_is_S_ptr :
  forall t path next reg,
    s_next (serialize t path next reg) = S (s_ptr (serialize t path next reg)).
Proof.
  intros [v children] path next reg.
  rewrite serialize_node_eq.
  destruct (serialize_forest children path next reg) as [scn scr] eqn:Hd.
  unfold s_next, s_ptr. simpl. reflexivity.
Qed.

(* ================= Theorem (ii): depth = path length ================ *)

Definition entry_ok (e : Entry) : Prop := e_depth e = length (e_path e).

Lemma serialize_depth_mut :
  (forall t path next reg,
     Forall entry_ok reg -> Forall entry_ok (s_reg (serialize t path next reg))) /\
  (forall f path next reg,
     Forall entry_ok reg -> Forall entry_ok (snd (serialize_forest f path next reg))).
Proof.
  apply Tree_Forest_mut.
  - intros v children IHf path next reg Hreg.
    rewrite serialize_node_eq.
    pose proof (IHf path next reg Hreg) as Hf.
    destruct (serialize_forest children path next reg) as [scn scr] eqn:Hd.
    simpl in Hf. unfold s_reg. simpl. apply Forall_cons.
    + unfold entry_ok. simpl. reflexivity.
    + exact Hf.
  - intros path next reg Hreg. rewrite sf_nil_eq. simpl. exact Hreg.
  - intros c child IHchild rest IHrest path next reg Hreg.
    rewrite sf_cons_eq.
    destruct (serialize child (path ++ [c]) next reg) as [[cp cnext] creg] eqn:Hc.
    assert (Hcreg : Forall entry_ok creg).
    { pose proof (IHchild (path ++ [c]) next reg Hreg) as Ht.
      unfold s_reg in Ht. rewrite Hc in Ht. simpl in Ht. exact Ht. }
    apply (IHrest path cnext creg Hcreg).
Qed.

Theorem entry_depth_is_path_length :
  forall t e, In e (snd (serialize_and_register t)) -> e_depth e = length (e_path e).
Proof.
  intros t e Hin.
  unfold serialize_and_register in Hin.
  destruct (serialize t [] 0 []) as [[p nx] reg] eqn:Hs.
  simpl in Hin.
  pose proof (proj1 serialize_depth_mut t [] 0 [] (Forall_nil entry_ok)) as Hall.
  unfold s_reg in Hall. rewrite Hs in Hall. simpl in Hall.
  rewrite Forall_forall in Hall. apply Hall. exact Hin.
Qed.

(* ============ Theorem (i-a): root entry carries the root ptr ========= *)

Theorem root_entry_is_root_ptr :
  forall t p reg,
    serialize_and_register t = (p, reg) ->
    exists e, In e reg /\ e_path e = [] /\ e_ptr e = p /\ e_depth e = 0.
Proof.
  intros t p reg Hsr.
  unfold serialize_and_register in Hsr.
  destruct t as [v children].
  rewrite serialize_node_eq in Hsr.
  destruct (serialize_forest children [] 0 []) as [scn scr] eqn:Hd.
  simpl in Hsr. inversion Hsr. subst p reg.
  exists (mkEntry [] scn 0). simpl.
  split; [ left; reflexivity | ].
  split; [ reflexivity | ].
  split; reflexivity.
Qed.

(* ===== Theorem (i-b): every registered ptr is freshly assigned ======= *)

Lemma serialize_fresh_mut :
  (forall t path next reg e,
     In e (s_reg (serialize t path next reg)) ->
     In e reg \/ (next <= e_ptr e /\ e_ptr e < s_next (serialize t path next reg))) /\
  (forall f path next reg e,
     In e (snd (serialize_forest f path next reg)) ->
     In e reg \/ (next <= e_ptr e /\ e_ptr e < fst (serialize_forest f path next reg))).
Proof.
  apply Tree_Forest_mut.
  - intros v children IHf path next reg e Hin.
    rewrite serialize_node_eq in Hin |- *.
    destruct (serialize_forest children path next reg) as [scn scr] eqn:Hd.
    unfold s_reg in Hin. simpl in Hin. unfold s_next. simpl.
    pose proof (serialize_forest_next_monotone children path next reg) as Hmono.
    rewrite Hd in Hmono. simpl in Hmono.    (* next <= scn *)
    destruct Hin as [Hhead | Htail].
    + subst e. simpl. right. split; lia.
    + pose proof (IHf path next reg e) as Hf.
      rewrite Hd in Hf. simpl in Hf.
      destruct (Hf Htail) as [Hin_reg | [Hlo Hhi]].
      * left. exact Hin_reg.
      * right. split; lia.
  - intros path next reg e Hin. rewrite sf_nil_eq in Hin |- *. simpl in Hin. simpl. left. exact Hin.
  - intros c child IHchild rest IHrest path next reg e Hin.
    rewrite sf_cons_eq in Hin |- *.
    destruct (serialize child (path ++ [c]) next reg) as [[cp cnext] creg] eqn:Hc.
    pose proof (serialize_next_monotone child (path ++ [c]) next reg) as Hcm.
    unfold s_next in Hcm. rewrite Hc in Hcm. simpl in Hcm.    (* next <= cnext *)
    pose proof (serialize_forest_next_monotone rest path cnext creg) as Hrm. (* cnext <= fst rest *)
    pose proof (IHchild (path ++ [c]) next reg e) as Hchildfact.
    unfold s_reg, s_next in Hchildfact. rewrite Hc in Hchildfact. simpl in Hchildfact.
    destruct (IHrest path cnext creg e Hin) as [Hin_creg | [Hlo Hhi]].
    + destruct (Hchildfact Hin_creg) as [Hin_reg | [Hclo Hchi]].
      * left. exact Hin_reg.
      * right. split; lia.
    + right. split; lia.
Qed.

Theorem registry_ptrs_are_fresh :
  forall t p reg e,
    serialize_and_register t = (p, reg) ->
    In e reg ->
    e_ptr e <= p.
Proof.
  intros t p reg e Hsr Hin.
  unfold serialize_and_register in Hsr.
  destruct (serialize t [] 0 []) as [[p0 nx] reg0] eqn:Hs.
  inversion Hsr. subst p0 reg0. clear Hsr.
  (* Hs : serialize t [] 0 [] = (p, nx, reg). *)
  pose proof (proj1 serialize_fresh_mut t [] 0 [] e) as Hf.
  rewrite Hs in Hf. unfold s_reg, s_next in Hf. simpl in Hf.
  (* Hf : In e reg -> In e [] \/ (0 <= e_ptr e /\ e_ptr e < nx) *)
  destruct (Hf Hin) as [Hbot | [_ Hhi]].
  - inversion Hbot.
  - pose proof (serialize_next_is_S_ptr t [] 0 []) as Hsn.
    rewrite Hs in Hsn. unfold s_next, s_ptr in Hsn. simpl in Hsn.
    (* Hsn : nx = S p ; Hhi : e_ptr e < nx *)
    lia.
Qed.

(* ===== Theorem (iv): the registry is a pure side-effect ============== *)

Lemma serialize_pc_mut :
  (forall t p1 p2 next r1 r2,
     s_ptr  (serialize t p1 next r1) = s_ptr  (serialize t p2 next r2) /\
     s_next (serialize t p1 next r1) = s_next (serialize t p2 next r2)) /\
  (forall f p1 p2 next r1 r2,
     fst (serialize_forest f p1 next r1) = fst (serialize_forest f p2 next r2)).
Proof.
  apply Tree_Forest_mut.
  - intros v children IHf p1 p2 next r1 r2.
    rewrite !serialize_node_eq.
    pose proof (IHf p1 p2 next r1 r2) as Hf.
    destruct (serialize_forest children p1 next r1) as [scn1 scr1] eqn:Hd1.
    destruct (serialize_forest children p2 next r2) as [scn2 scr2] eqn:Hd2.
    simpl in Hf. subst scn2. unfold s_ptr, s_next. simpl. split; reflexivity.
  - intros p1 p2 next r1 r2. rewrite !sf_nil_eq. reflexivity.
  - intros c child IHchild rest IHrest p1 p2 next r1 r2.
    rewrite !sf_cons_eq.
    destruct (serialize child (p1 ++ [c]) next r1) as [[cp1 cn1] cr1] eqn:Hc1.
    destruct (serialize child (p2 ++ [c]) next r2) as [[cp2 cn2] cr2] eqn:Hc2.
    pose proof (IHchild (p1 ++ [c]) (p2 ++ [c]) next r1 r2) as [_ Hcn].
    unfold s_next in Hcn. rewrite Hc1, Hc2 in Hcn. simpl in Hcn. subst cn2.
    apply (IHrest p1 p2 cn1 cr1 cr2).
Qed.

(* Building the registry does not change the on-disk pointer a node receives:
   the pointer depends only on the tree and the incoming counter, never on the
   path or the (accumulating) registry. Hence recovery, which reads the
   serialized on-disk root, is unaffected by whether the registry was built. *)
Theorem registry_is_side_effect_free_on_disk_root :
  forall t path1 path2 next reg1 reg2,
    s_ptr (serialize t path1 next reg1) = s_ptr (serialize t path2 next reg2).
Proof.
  intros t path1 path2 next reg1 reg2.
  apply (proj1 serialize_pc_mut t path1 path2 next reg1 reg2).
Qed.

(* ===== Theorem (iii): publication ordering / recovery independence ==== *)

Record Publication := mkPub {
  pub_verified : bool;
  pub_registry : Registry;
  pub_disk_root : Ptr
}.

(* The coordinator's published registry: identity when verified, empty
   otherwise (checkpoint() calls update_disk_registry only after
   verify_checkpoint() succeeds). *)
Definition published (p : Publication) : Registry :=
  if pub_verified p then pub_registry p else [].

(* Recovery restores the durable on-disk root; it never reads the registry. *)
Definition recovered_root (p : Publication) : Ptr := pub_disk_root p.

Theorem publish_empty_unless_verified :
  forall p, pub_verified p = false -> published p = [].
Proof. intros p H. unfold published. rewrite H. reflexivity. Qed.

Theorem publish_is_registry_when_verified :
  forall p, pub_verified p = true -> published p = pub_registry p.
Proof. intros p H. unfold published. rewrite H. reflexivity. Qed.

(* The recovered root is independent of the published registry. *)
Theorem recovery_independent_of_registry :
  forall v r1 r2 d,
    recovered_root (mkPub v r1 d) = recovered_root (mkPub v r2 d).
Proof. intros. reflexivity. Qed.
