(** * AbiPagingProducerSpec: the producer's edge-paging refinement

    Family FV obligation #12 (wave W2). The producer's `node_edges`
    implementation is literally "skip [start], take [capacity]" over a
    node's memoized edge list, reporting the full count as [out_total]
    (src/bindings.rs `dictionary_edges`). This spec proves that shape
    satisfies the interop paging laws, restated here as theorem premises per
    the family's assumption-import discipline (the consumer-side acceptance
    predicate lives in liblevenshtein's `ConsumerAcceptance.v`, wave W3 —
    cited, not duplicated):

    - [LDICT-PAGE-1] every page fits its capacity, lies inside the edge
      list, and the reported total never varies with the paging position;
    - [LDICT-PAGE-2] pages are lossless: walking page starts
      0, capacity, 2*capacity, ... and concatenating the pages reproduces
      the memoized edge list exactly, and the page at offset k*capacity is
      precisely what a consumer's k-th call receives.

    Executable mirror: tests/ffi_resource_paging_proptest.rs (capacity 0/1/
    exact/overshoot, out_total stability, lossless concatenation against the
    real C ABI).
*)

From Stdlib Require Import List.
From Stdlib Require Import Arith.
From Stdlib Require Import Lia.
Import ListNotations.

Section Paging.

Variable Edge : Type.

(** The producer's page: skip [start], take [capacity]. *)
Definition page (edges : list Edge) (start capacity : nat) : list Edge :=
  firstn capacity (skipn start edges).

(** The total the producer reports, independent of paging position. *)
Definition total (edges : list Edge) : nat := length edges.

(** ** LDICT-PAGE-1: per-call bounds *)

Theorem page_written_bounded :
  forall edges start capacity,
    length (page edges start capacity) <= capacity.
Proof.
  intros. unfold page. apply firstn_le_length.
Qed.

Theorem page_within_remaining :
  forall edges start capacity,
    length (page edges start capacity) <= total edges - start.
Proof.
  intros. unfold page, total.
  rewrite length_firstn, length_skipn. lia.
Qed.

(** A full-capacity page exists exactly while pages remain. *)
Theorem page_exact_while_remaining :
  forall edges start capacity,
    start + capacity <= total edges ->
    length (page edges start capacity) = capacity.
Proof.
  intros edges start capacity Hle. unfold page, total in *.
  rewrite length_firstn, length_skipn. lia.
Qed.

(** Past-the-end starts yield the empty page (the End condition). *)
Theorem page_after_end_empty :
  forall edges start capacity,
    total edges <= start ->
    page edges start capacity = [].
Proof.
  intros edges start capacity Hle. unfold page, total in *.
  rewrite skipn_all2 by lia.
  apply firstn_nil.
Qed.

(** One paged call as the consumer sees it: the page plus the reported
    total. *)
Definition paged_call (edges : list Edge) (start capacity : nat)
  : list Edge * nat :=
  (page edges start capacity, total edges).

(** The reported total is identical across every paging position and
    capacity — the out_total-stability half of the paging law. *)
Theorem total_stable :
  forall edges start_a capacity_a start_b capacity_b,
    snd (paged_call edges start_a capacity_a)
    = snd (paged_call edges start_b capacity_b).
Proof.
  reflexivity.
Qed.

(** ** LDICT-PAGE-2: lossless decomposition *)

(** The consumer's walk: pages at starts 0, c, 2c, ..., driven by fuel
    (any fuel >= length edges suffices; each step consumes >= 1 edge). *)
Fixpoint pages_fuel (fuel capacity : nat) (edges : list Edge)
  : list (list Edge) :=
  match fuel with
  | 0 => []
  | S fuel' =>
      match edges with
      | [] => []
      | _ => firstn capacity edges :: pages_fuel fuel' capacity (skipn capacity edges)
      end
  end.

Definition pages (capacity : nat) (edges : list Edge) : list (list Edge) :=
  pages_fuel (length edges) capacity edges.

Lemma firstn_skipn_edge :
  forall (edges : list Edge) capacity,
    firstn capacity edges ++ skipn capacity edges = edges.
Proof.
  intros. apply firstn_skipn.
Qed.

Lemma pages_fuel_concat :
  forall fuel capacity edges,
    1 <= capacity ->
    length edges <= fuel ->
    concat (pages_fuel fuel capacity edges) = edges.
Proof.
  induction fuel as [| fuel IH]; simpl; intros capacity edges Hcap Hlen.
  - destruct edges; [reflexivity | simpl in Hlen; lia].
  - destruct edges as [| head rest]; [reflexivity |].
    simpl in Hlen |- *.
    rewrite IH.
    + apply firstn_skipn.
    + exact Hcap.
    + rewrite length_skipn.
      destruct capacity as [| c]; [lia | simpl; lia].
Qed.

(** Concatenating the pages reproduces the edge list exactly. *)
Theorem pages_concat_exact :
  forall capacity edges,
    1 <= capacity ->
    concat (pages capacity edges) = edges.
Proof.
  intros. unfold pages. apply pages_fuel_concat; auto.
Qed.

(** Every page in the decomposition respects the capacity bound (true for
    every fuel and edge list — no side conditions). *)
Lemma pages_fuel_all_bounded :
  forall fuel capacity edges,
    Forall (fun p : list Edge => length p <= capacity)
           (pages_fuel fuel capacity edges).
Proof.
  induction fuel as [| fuel IH]; simpl; intros capacity edges.
  - constructor.
  - destruct edges as [| head rest]; [constructor |].
    constructor.
    + apply firstn_le_length.
    + apply IH.
Qed.

Theorem pages_all_bounded :
  forall capacity edges,
    Forall (fun p => length p <= capacity) (pages capacity edges).
Proof.
  intros. unfold pages. apply pages_fuel_all_bounded.
Qed.

(** The k-th page of the decomposition is exactly the producer's answer to
    the k-th paged call (start = k * capacity). *)
Theorem nth_page_is_paged_call :
  forall capacity edges k,
    1 <= capacity ->
    k * capacity < length edges ->
    nth_error (pages capacity edges) k = Some (page edges (k * capacity) capacity).
Proof.
  intros capacity edges k Hcap.
  revert edges k.
  assert (General :
    forall fuel edges k,
      length edges <= fuel ->
      k * capacity < length edges ->
      nth_error (pages_fuel fuel capacity edges) k
      = Some (page edges (k * capacity) capacity)).
  { induction fuel as [| fuel IH]; simpl; intros edges k Hfuel Hin.
    - lia.
    - destruct edges as [| head rest]; [simpl in Hin; lia |].
      simpl in Hfuel, Hin.
      destruct k as [| k'].
      + simpl. unfold page. simpl. reflexivity.
      + simpl.
        rewrite IH.
        * unfold page.
          rewrite skipn_skipn.
          replace (S k' * capacity)
            with (k' * capacity + capacity) by lia.
          rewrite Nat.add_comm.
          reflexivity.
        * rewrite length_skipn.
          destruct capacity as [| c]; [lia | simpl; lia].
        * rewrite length_skipn.
          assert (Hstep : S k' * capacity = capacity + k' * capacity) by lia.
          destruct capacity as [| c]; [lia | simpl in Hstep |- *; lia]. }
  intros edges k Hin.
  unfold pages. apply General; auto.
Qed.

End Paging.
