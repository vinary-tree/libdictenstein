(* Model/PrefixChunking.v — CX (task #43): the path-compression chunker's NO-TRUNCATION core,
   formalized.

   The Coq twin of [src/persistent_artrie/core/overlay/codec.rs::chain_chunks]. The owner mandate
   (2026-06-08) for the path-compressing overlay<->dense codec is: KEEP compression, but PROVE it
   never truncates / loses key data. This file proves exactly that for the chunking step — the only
   place a fixed [MAX_PREFIX_LEN] could cause loss:

     a single-child chain's edge-unit string [l] is split into width-[w] chunks (w = max_prefix + 1,
     each dense node packing <= max_prefix prefix units + one edge unit); the codec emits, per
     chunk [c], a node carrying (prefix = removelast c, edge = last c).

   THEOREM [chain_chunks_no_truncation]: the concatenation of every emitted chunk's
   [removelast c ++ [last c]], in order, equals [l] EXACTLY — no unit dropped, duplicated, or
   reordered, for ALL chain lengths and ALL widths w >= 1. There is no [min]/[firstn MAX]/truncation
   anywhere; the chunker is a total partition (the Coq [chunks]) plus a last-element split.

   This is the chunk-level (T2) lemma of the red-teamed CX proof plan
   (docs/design/cx-task43-codec-design-2026-06-08.md). It carries the [(prefix, edge)] split with the
   edge as a unit DISTINCT from the prefix — the shape the red-team requires the full tree-level
   [keys_de] model to preserve.

   Self-contained: depends only on the Coq/Rocq stdlib (List/Arith/Lia). No [Axiom], no [admit],
   no [Admitted] — consistent with the ARTrie proof corpus. *)

From Stdlib Require Import List Arith Lia.
Import ListNotations.

Section Chunking.
Context {U : Type}.

(* [chunks_fuel f l w]: partition [l] into consecutive pieces of size [w] (the last possibly
   smaller), using [f] units of structural fuel. [chunks] supplies [length l] fuel, which is always
   enough (each step consumes >= 1 element when w >= 1). *)
Fixpoint chunks_fuel (f : nat) (l : list U) (w : nat) : list (list U) :=
  match f with
  | 0 => []
  | S f' =>
      match l with
      | [] => []
      | _ :: _ => firstn w l :: chunks_fuel f' (skipn w l) w
      end
  end.

Definition chunks (l : list U) (w : nat) : list (list U) := chunks_fuel (length l) l w.

(* [length (firstn w l) <= w], proved without depending on the exact stdlib name. *)
Lemma firstn_width : forall w (l : list U), length (firstn w l) <= w.
Proof.
  induction w as [|w' IH]; intros l; simpl.
  - lia.
  - destruct l as [|x xs]; simpl; [lia | specialize (IH xs); lia].
Qed.

(* Each step removes [length (firstn w l)] elements; with w >= 1 and l non-empty that is >= 1. *)
Lemma firstn_pos_len : forall (l : list U) w,
  w >= 1 -> l <> [] -> length (firstn w l) >= 1.
Proof.
  intros l w Hw Hne.
  destruct l as [|x xs]; [contradiction|].
  destruct w as [|w']; [lia|]. simpl. lia.
Qed.

(* With enough fuel, the partition reassembles [l]: NO unit is lost. *)
Lemma chunks_fuel_concat : forall f l w,
  w >= 1 -> length l <= f -> concat (chunks_fuel f l w) = l.
Proof.
  induction f as [|f' IH]; intros l w Hw Hlen; simpl.
  - (* fuel 0 ==> length l <= 0 ==> l = [] *)
    destruct l as [|x xs]; [reflexivity | simpl in Hlen; lia].
  - destruct l as [|x xs]; [reflexivity|].
    simpl (concat (_ :: _)).
    (* length (skipn w (x::xs)) <= f' : firstn + skipn = whole; firstn >= 1; whole <= S f'. *)
    assert (Hsplit : length (firstn w (x :: xs)) + length (skipn w (x :: xs))
                     = length (x :: xs)).
    { rewrite <- length_app. rewrite firstn_skipn. reflexivity. }
    assert (Hpos : length (firstn w (x :: xs)) >= 1)
      by (apply firstn_pos_len; [assumption | discriminate]).
    rewrite IH.
    + apply firstn_skipn.
    + assumption.
    + lia.
Qed.

(* THE no-loss partition lemma: [concat (chunks l w) = l]. *)
Lemma chunks_concat : forall l w, w >= 1 -> concat (chunks l w) = l.
Proof.
  intros l w Hw. unfold chunks. apply chunks_fuel_concat; [assumption | lia].
Qed.

(* Every emitted chunk is non-empty (so [removelast c ++ [last c] = c] applies). *)
Lemma chunks_fuel_nonempty : forall f l w c,
  w >= 1 -> In c (chunks_fuel f l w) -> c <> [].
Proof.
  induction f as [|f' IH]; intros l w c Hw Hin; simpl in Hin; [contradiction|].
  destruct l as [|x xs]; [contradiction|].
  destruct Hin as [Heq | Hin].
  - subst c. intro Hc.
    assert (length (firstn w (x :: xs)) >= 1) as Hp
      by (apply firstn_pos_len; [assumption | discriminate]).
    rewrite Hc in Hp. simpl in Hp. lia.
  - eapply IH; eassumption.
Qed.

Lemma chunks_nonempty : forall l w c, w >= 1 -> In c (chunks l w) -> c <> [].
Proof. unfold chunks; eauto using chunks_fuel_nonempty. Qed.

(* Every chunk respects the width cap (so the prefix [removelast c] has <= max_prefix = w-1 units,
   and the [prefix_len <= MAX_PREFIX_LEN] header validation can never overflow). *)
Lemma chunks_fuel_width : forall f l w c,
  In c (chunks_fuel f l w) -> length c <= w.
Proof.
  induction f as [|f' IH]; intros l w c Hin; simpl in Hin; [contradiction|].
  destruct l as [|x xs]; [contradiction|].
  destruct Hin as [Heq | Hin].
  - subst c. apply firstn_width.
  - eapply IH; eassumption.
Qed.

Lemma chunks_width : forall l w c, In c (chunks l w) -> length c <= w.
Proof. unfold chunks; eauto using chunks_fuel_width. Qed.

(* Reassembling a list of non-empty chunks via the codec's (prefix=removelast, edge=last) split is
   the identity on the chunk list. *)
Lemma map_reassemble : forall (d : U) (cs : list (list U)),
  (forall c, In c cs -> c <> []) ->
  map (fun c => removelast c ++ [last c d]) cs = cs.
Proof.
  induction cs as [|c cs' IH]; intros Hne; simpl; [reflexivity|].
  rewrite <- app_removelast_last by (apply Hne; left; reflexivity).
  rewrite IH; [reflexivity | intros c' Hin; apply Hne; right; assumption].
Qed.

(* ======================================================================== *)
(* THE HEADLINE: NO-TRUNCATION for the codec's (prefix, edge) emission.      *)
(* ======================================================================== *)

(* The codec turns each non-empty chunk [c] into a dense node carrying
   [prefix = removelast c] and [edge = last c]. Reassembling every node's [prefix ++ [edge]]
   reproduces the original chain [l] EXACTLY — for all lengths and all w >= 1. This is the formal
   guarantee that path compression with a bounded prefix width loses NO key data. *)
Theorem chain_chunks_no_truncation : forall (d : U) (l : list U) (w : nat),
  w >= 1 ->
  concat (map (fun c => removelast c ++ [last c d]) (chunks l w)) = l.
Proof.
  intros d l w Hw.
  rewrite <- (chunks_concat l w Hw) at 2.
  f_equal.
  apply map_reassemble.
  intros c Hin. eapply chunks_nonempty; eassumption.
Qed.

End Chunking.
