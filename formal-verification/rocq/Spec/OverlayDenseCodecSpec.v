(* Spec/OverlayDenseCodecSpec.v — CX (task #43): TREE-level reversibility of the path-compressing
   overlay <-> dense codec.

   The Coq twin of the overlay<->dense transform implemented in Rust by
   [src/persistent_artrie_char/persist.rs::serialize_overlay_snapshot_compressed] / [peel_chain]
   (the ENCODE / compress side) and [src/persistent_artrie_char/disk_io.rs::load_overlay_node_from_disk]
   -> [persist.rs::inner_to_overlay] (the DECODE / expand side), with the chunking step
   [src/persistent_artrie_core/overlay/codec.rs::chain_chunks] already proven no-truncating in
   [Model/PrefixChunking.v] (theorem [chunks_concat]).

   The owner mandate (2026-06-08) is "KEEP path compression, but PROVE it is safe (loses no key
   data)". This file discharges that at the TREE level, building on the chunk-level no-truncation core:

   - [embed] is the UNCOMPRESSED (prefix_len=0) image — exactly what the production overlay serializer
     emits today (no producer emits prefix_len>0 yet). [decode (embed t) = t] (T1) therefore certifies
     the LIVE load/fault-in round-trip: [inner_to_overlay] of a prefix_len=0 image is the identity.
   - [compress]/[encode_chain] is the DORMANT path-compression codec: a peeled single-child chain
     [Lp] is split by [chunks] (width = max_prefix+1) into dense chunk nodes, each carrying
     (prefix = removelast c, edge = last c).  [T1_chain] proves expanding the chunk stack reproduces
     the uncompressed chain EXACTLY, for ALL chains and ALL terminus subtrees — the tree-level lift of
     [chain_chunks_no_truncation].  The single rewrite by [chunks_concat] is the formal "no key unit
     is lost across a chunk boundary".
   - [keys]/[keys_de] model the term set with the dense node's per-child edge as a unit DISTINCT from
     the prefix (keys_de lays [path ++ prefix ++ [edge]]); [keys_embed] proves this explicit-edge
     model degenerates to the overlay [keys] (the red-team's BLOCKING-2 off-by-one obligation).
   - [well_chunked] is an OPERATIONAL (bool-decidable) predicate: every dense node's prefix is
     <= max_prefix.  [embed]/[compress] always produce well-chunked trees ([encode_well_chunked],
     [compress_well_chunked]) — the prefix never overflows MAX_PREFIX_LEN (T2_prefix_cap).

   The section parameter [W] = max_prefix is instantiated TWICE at the end (char W=6, byte W=12),
   matching [CHAR_MAX_PREFIX_LEN] / [MAX_PREFIX_LEN] (codec.rs); chunk width = [S W] = W+1.

   SCOPE BOUNDARY (stated, not papered over): this certifies the overlay<->dense TREE transform. The
   on-disk BYTE encoding of [prefix]/[prefix_len] (header layout) is covered by the Rust layout
   correspondence test (persistent_char_node_layout_correspondence.rs), NOT here; the alphabet here is
   [nat], so non-scalar/astral prefix-unit corruption (the fail-closed loader, design #5d) is a Rust
   concern.  The WHOLE-tree composition (a tree with several INTERNAL compressed chains round-tripping
   as one) is exercised EXHAUSTIVELY by the Rust [cx_roundtrip] tests (char + byte); the faithful
   whole-tree [encode] needs a non-structural peel (fuel) and is intentionally out of this file's
   structural scope — the chain-level [T1_chain] + structural [T1] are its two composable halves.

   Self-contained beyond the proven [Model/PrefixChunking.v]. No [Axiom], no [admit], no [Admitted]. *)

From Stdlib Require Import List Arith Lia Bool.
Require Import ARTrie.Model.PrefixChunking.
Import ListNotations.

(* The codec alphabet. [nat] (matching [CharLabel := nat] in the other persistent specs); the generic
   [chunks]/[chunks_concat] of PrefixChunking are instantiated at [U := nat] by unification. *)
Definition Unit := nat.
(* A default unit for [last]/[removelast]; every chunk is nonempty (chunks_nonempty), so the choice is
   irrelevant — it is never the actually-returned [last]. *)
Definition d0 : Unit := 0.

Section Codec.
Context {V : Type}.
(* [W] = max_prefix (char 6 / byte 12); chunk width = [S W] = W+1 (>= 1 definitionally). *)
Context (W : nat).
Definition width : nat := S W.

(* ---------- Trees ---------- *)

(* The UNCOMPRESSED overlay tree: finality, optional value, edge-labeled children. No prefix field —
   the in-memory overlay is always prefix_len=0 (traversal is prefix-UNAWARE). *)
Inductive Tree : Type :=
| Node : bool -> option V -> list (Unit * Tree) -> Tree.

(* The DENSE (on-disk) tree: each node ALSO carries a path-compression [prefix] (the units strictly
   between its incoming edge and its out-edges). *)
Inductive DenseTree : Type :=
| DNode : list Unit -> bool -> option V -> list (Unit * DenseTree) -> DenseTree.

(* Strong induction principle for [Tree] (the auto-generated one ignores the nested child list). The
   standard nested-fixpoint encoding producing a [Forall] over the children. *)
Fixpoint Tree_ind'
  (P : Tree -> Prop)
  (step : forall f v ch, Forall (fun e => P (snd e)) ch -> P (Node f v ch))
  (t : Tree) {struct t} : P t :=
  match t with
  | Node f v ch =>
      step f v ch
        ((fix go (l : list (Unit * Tree)) : Forall (fun e => P (snd e)) l :=
            match l with
            | [] => Forall_nil _
            | e :: tl => Forall_cons e (Tree_ind' P step (snd e)) (go tl)
            end) ch)
  end.

(* ---------- Term-set semantics (with the explicit per-child edge) ---------- *)

Definition here (f : bool) (v : option V) (path : list Unit) : list (list Unit * option V) :=
  if f then [(path, v)] else [].

(* Overlay key-set: each child edge [lbl] contributes [lbl] to the path; no prefix. *)
Fixpoint keys (path : list Unit) (t : Tree) : list (list Unit * option V) :=
  match t with
  | Node f v ch =>
      here f v path
        ++ flat_map (fun e : Unit * Tree =>
                       let '(lbl, sub) := e in keys (path ++ [lbl]) sub) ch
  end.

(* Dense key-set: a node first lays its [prefix] (path ++ prefix), THEN each edge [lbl] contributes
   [lbl] (path ++ prefix ++ [lbl]) — the [prefix ++ [edge] ++ child] shape (BLOCKING-2). *)
Fixpoint keys_de (path : list Unit) (t : DenseTree) : list (list Unit * option V) :=
  match t with
  | DNode prefix f v ch =>
      here f v (path ++ prefix)
        ++ flat_map (fun e : Unit * DenseTree =>
                       let '(lbl, sub) := e in keys_de (path ++ prefix ++ [lbl]) sub) ch
  end.

(* ---------- decode (expand) : DenseTree -> Tree ---------- *)

(* Lay a unit list out as a chain of single-child non-final no-value links terminating in [inner] —
   the [inner_to_overlay] prefix expansion. STRUCTURAL on [prefix]. *)
Fixpoint wrap_chain (prefix : list Unit) (inner : Tree) : Tree :=
  match prefix with
  | [] => inner
  | p :: ps => Node false None [(p, wrap_chain ps inner)]
  end.

(* Expand every dense node's [prefix] into a chain above its (finality, value, decoded children).
   STRUCTURAL on [t] (the recursive call is on [sub], a subterm reached through [map]). *)
Fixpoint decode (t : DenseTree) : Tree :=
  match t with
  | DNode prefix f v ch =>
      wrap_chain prefix
        (Node f v (map (fun e : Unit * DenseTree =>
                          let '(lbl, sub) := e in (lbl, decode sub)) ch))
  end.

(* ---------- embed (uncompressed image) + compress (the dormant codec) ---------- *)

(* The prefix-free embedding Tree -> DenseTree (every prefix []): the production serialize image. *)
Fixpoint embed (t : Tree) : DenseTree :=
  match t with
  | Node f v ch =>
      DNode [] f v (map (fun e : Unit * Tree =>
                           let '(lbl, sub) := e in (lbl, embed sub)) ch)
  end.

Definition encode (t : Tree) : DenseTree := embed t.

(* One dense chunk node for chunk [c]: prefix = removelast c, single out-edge = last c, one child. The
   image of [overlay_inner_single_node_with_prefix(synth, [(last c, child_ptr)], removelast c)]. *)
Definition chunk_node (c : list Unit) (below : DenseTree) : DenseTree :=
  DNode (removelast c) false None [(last c d0, below)].

(* Compress a peeled chain [l] above [inner_dt]: one chunk node per [chunks l width], bottom-up.
   Non-recursive ([fold_right] over the already-total [chunks] list) — no termination obligation. *)
Definition compress (l : list Unit) (inner_dt : DenseTree) : DenseTree :=
  fold_right chunk_node inner_dt (chunks l width).

(* Encode a subtree whose incoming chain from its parent is [Lp]: the faithful image of "peel produced
   chain [Lp] down to terminus [t], then chain_chunks [Lp]". *)
Definition encode_chain (Lp : list Unit) (t : Tree) : DenseTree := compress Lp (embed t).

(* ---------- Supporting lemmas ---------- *)

Lemma wrap_chain_app : forall a b inner,
  wrap_chain (a ++ b) inner = wrap_chain a (wrap_chain b inner).
Proof.
  induction a as [|x xs IH]; intros b inner; simpl.
  - reflexivity.
  - rewrite IH. reflexivity.
Qed.

(* Decoding one chunk node lays its whole chunk [c] out as a chain above the child's decode. *)
Lemma decode_chunk_node : forall c below,
  c <> [] -> decode (chunk_node c below) = wrap_chain c (decode below).
Proof.
  intros c below Hne. unfold chunk_node. simpl.
  (* goal: wrap_chain (removelast c) (Node false None [(last c d0, decode below)])
           = wrap_chain c (decode below) *)
  change (Node false None [(last c d0, decode below)])
    with (wrap_chain [last c d0] (decode below)).
  rewrite <- wrap_chain_app.
  rewrite <- (app_removelast_last d0 Hne).
  reflexivity.
Qed.

(* Decoding the whole chunk stack reassembles [concat cs] above the inner decode. *)
Lemma decode_fold_chunks : forall cs inner_dt,
  (forall c, In c cs -> c <> []) ->
  decode (fold_right chunk_node inner_dt cs) = wrap_chain (concat cs) (decode inner_dt).
Proof.
  induction cs as [|c cs' IH]; intros inner_dt Hne.
  - simpl. reflexivity.
  - cbn [fold_right concat].
    rewrite decode_chunk_node by (apply Hne; left; reflexivity).
    rewrite IH by (intros c' Hc'; apply Hne; right; exact Hc').
    rewrite wrap_chain_app. reflexivity.
Qed.

(* THE chain-level core: compress then decode reproduces the uncompressed chain. The [chunks_concat]
   rewrite is the no-truncation guarantee — no key unit lost across a chunk boundary. *)
Lemma decode_compress : forall l inner_dt,
  decode (compress l inner_dt) = wrap_chain l (decode inner_dt).
Proof.
  intros l inner_dt. unfold compress.
  rewrite decode_fold_chunks
    by (intros c Hc; apply (chunks_nonempty l width c); [ unfold width; lia | exact Hc ]).
  rewrite chunks_concat by (unfold width; lia).
  reflexivity.
Qed.

(* The structural round-trip core: decoding the uncompressed image is the identity. *)
Lemma decode_embed : forall t, decode (embed t) = t.
Proof.
  intros t.
  induction t as [f v ch IH] using Tree_ind'.
  simpl. f_equal.
  induction ch as [| e tl IHch].
  - reflexivity.
  - destruct e as [lbl sub]. simpl.
    inversion IH as [| e0 tl0 Hhead Htail Heq]; subst.
    simpl in Hhead.
    rewrite Hhead.
    f_equal.
    apply IHch. exact Htail.
Qed.

(* The explicit-edge dense model degenerates to the overlay key-set (BLOCKING-2 faithfulness). *)
Lemma keys_embed : forall t path, keys_de path (embed t) = keys path t.
Proof.
  intros t.
  induction t as [f v ch IH] using Tree_ind'; intros path.
  simpl. rewrite app_nil_r. f_equal.
  induction ch as [| e tl IHch].
  - reflexivity.
  - destruct e as [lbl sub]. simpl.
    inversion IH as [| e0 tl0 Hhead Htail Heq]; subst.
    simpl in Hhead.
    rewrite Hhead.
    f_equal.
    apply IHch. exact Htail.
Qed.

(* ---------- T1: decode o encode = id (the headline) ---------- *)

Theorem T1_decode_encode_id : forall t, decode (encode t) = t.
Proof. intros t. unfold encode. apply decode_embed. Qed.

(* The compression-aware chain round-trip: a peeled chain [Lp] down to terminus [t], chunkified then
   expanded, reproduces the uncompressed chain [wrap_chain Lp t] EXACTLY — for ALL [Lp], ALL [t]. The
   tree-level lift of [chain_chunks_no_truncation]. *)
Theorem T1_chain : forall Lp t, decode (encode_chain Lp t) = wrap_chain Lp t.
Proof.
  intros Lp t. unfold encode_chain.
  rewrite decode_compress. rewrite decode_embed. reflexivity.
Qed.

(* User-facing corollaries: the term set is preserved (structural equality gives it for free), and the
   dense-side view of the encoded tree agrees with the overlay-side view. *)
Corollary T1_keys_struct : forall t path, keys path (decode (encode t)) = keys path t.
Proof. intros t path. rewrite T1_decode_encode_id. reflexivity. Qed.

Theorem T1_keys_de : forall t path, keys_de path (encode t) = keys path t.
Proof. intros t path. unfold encode. apply keys_embed. Qed.

(* ---------- T2: no-truncation + width cap (reuse PrefixChunking) ---------- *)

Theorem T2_no_truncation : forall (l : list Unit), concat (chunks l width) = l.
Proof. intros l. apply chunks_concat. unfold width; lia. Qed.

Theorem T2_width_cap : forall (l : list Unit) c, In c (chunks l width) -> length c <= width.
Proof. intros l c Hin. exact (chunks_width l width c Hin). Qed.

Lemma length_removelast_pred : forall (l : list Unit), length (removelast l) = length l - 1.
Proof.
  induction l as [|x xs IH]; simpl.
  - reflexivity.
  - destruct xs as [|y ys]; simpl in *; lia.
Qed.

(* Every dense node's prefix (= removelast of its chunk) is <= W = max_prefix: prefix_len never
   overflows MAX_PREFIX_LEN. *)
Corollary T2_prefix_cap : forall (l : list Unit) c,
  In c (chunks l width) -> length (removelast c) <= W.
Proof.
  intros l c Hin.
  pose proof (chunks_width l width c Hin) as Hw.
  pose proof (length_removelast_pred c) as Hr.
  unfold width in Hw. lia.
Qed.

(* ---------- T3: idempotence (falls out of T1) ---------- *)

Theorem T3_idempotent : forall t, encode (decode (encode t)) = encode t.
Proof. intros t. rewrite T1_decode_encode_id. reflexivity. Qed.

Theorem T3_decode_idem : forall dt, decode (encode (decode dt)) = decode dt.
Proof. intros dt. rewrite T1_decode_encode_id. reflexivity. Qed.

(* ---------- well_chunked: an OPERATIONAL (decidable) compression invariant ---------- *)

Definition node_well_formed (p : list Unit) : bool := Nat.leb (length p) W.

Fixpoint well_chunked (dt : DenseTree) : bool :=
  match dt with
  | DNode p _ _ ch =>
      node_well_formed p
        && forallb (fun e : Unit * DenseTree => let '(_, sub) := e in well_chunked sub) ch
  end.

Lemma embed_well_chunked : forall t, well_chunked (embed t) = true.
Proof.
  intros t.
  induction t as [f v ch IH] using Tree_ind'.
  (* well_chunked (DNode [] ..) = node_well_formed [] && forallb .. ; node_well_formed [] reduces to
     true (Nat.leb 0 W), so [simpl] collapses the [&&] to the [forallb] goal directly. *)
  simpl.
  induction ch as [| e tl IHch].
  - reflexivity.
  - destruct e as [lbl sub]. simpl.
    inversion IH as [| e0 tl0 Hhead Htail Heq]; subst.
    simpl in Hhead.
    rewrite Hhead. simpl.
    apply IHch. exact Htail.
Qed.

Lemma fold_chunks_well_chunked : forall cs inner_dt,
  (forall c, In c cs -> length (removelast c) <= W) ->
  well_chunked inner_dt = true ->
  well_chunked (fold_right chunk_node inner_dt cs) = true.
Proof.
  induction cs as [|c cs' IH]; intros inner_dt Hcap Hin; simpl.
  - exact Hin.
  - unfold chunk_node. simpl.
    apply andb_true_intro. split.
    + unfold node_well_formed. apply Nat.leb_le. apply Hcap. left. reflexivity.
    + rewrite andb_true_r.
      apply IH.
      * intros c' Hc'. apply Hcap. right. exact Hc'.
      * exact Hin.
Qed.

Lemma compress_well_chunked : forall l inner_dt,
  well_chunked inner_dt = true -> well_chunked (compress l inner_dt) = true.
Proof.
  intros l inner_dt Hin. unfold compress.
  apply fold_chunks_well_chunked.
  - intros c Hc. apply (T2_prefix_cap l c Hc).
  - exact Hin.
Qed.

(* The dormant compression codec always produces a well-chunked image (every prefix <= max_prefix). *)
Theorem encode_chain_well_chunked : forall Lp t, well_chunked (encode_chain Lp t) = true.
Proof.
  intros Lp t. unfold encode_chain.
  apply compress_well_chunked. apply embed_well_chunked.
Qed.

End Codec.

(* ---------- Alphabet instantiation (the section [W] pinned at BOTH alphabets) ---------- *)

(* char: CHAR_MAX_PREFIX_LEN = 6 -> chunk width 7. *)
Definition encode_chain_char {V} : list Unit -> @Tree V -> @DenseTree V := @encode_chain V 6.
Definition T1_chain_char {V} := @T1_chain V 6.
Definition T2_no_truncation_char := @T2_no_truncation 6.

(* byte: MAX_PREFIX_LEN = 12 -> chunk width 13. *)
Definition encode_chain_byte {V} : list Unit -> @Tree V -> @DenseTree V := @encode_chain V 12.
Definition T1_chain_byte {V} := @T1_chain V 12.
Definition T2_no_truncation_byte := @T2_no_truncation 12.
