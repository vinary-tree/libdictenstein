(** * Byte-DAWG ownership and native-frame laws

    This module specializes the generic Arc-token semantics of
    [OverlayArcOwnershipSpec] to the byte-DAWG edge-drain mechanism in
    [src/dynamic_dawg/lockfree.rs]. An edge slot owns one distinct strong
    token; equal target payloads do not merge those tokens. A public node
    handle is an additional external token. The graph here is finite but
    unbounded: no fixed key depth, node count, or fanout enters a theorem.

    The native-frame model counts only library-owned [LockFreeDawgNode::drop]
    activations. The current source calls [Arc::try_unwrap] and may then drop
    its failed [Err] token; that operation can enter a child destructor on
    the same stack. The safe candidate consumes each edge token with
    [Arc::into_inner], whose shared result does not invoke payload [Drop].
    External final releases begin on otherwise idle threads. Arbitrary
    reentrancy from user-defined mapped-value destructors is not claimed.

    Reference: Rust standard-library [Arc::into_inner] contract and
    concurrent linked-list Drop example in alloc/src/sync.rs, stable 1.98.1,
    lines 1131-1230. A later source-correspondence test must establish that
    the production implementation follows these modeled operations.
*)

From Coq Require Import Lists.List Arith.PeanoNat Lia.
Require Import ARTrie.Spec.OverlayArcOwnershipSpec.
Import ListNotations.

Record DawgEdgeSlot := {
  dawg_edge_token : nat;
  dawg_edge_target : nat
}.

Record DawgPayload := {
  dawg_payload_id : nat;
  dawg_payload_edges : list DawgEdgeSlot
}.

Definition dawg_edge_reference (parent : nat) (edge : DawgEdgeSlot)
  : ArcReference :=
  {| reference_identity := dawg_edge_token edge;
     reference_payload := dawg_edge_target edge;
     reference_owner := ChildSlotOwner parent |}.

Fixpoint dawg_graph_references (graph : list DawgPayload) : ArcLedger :=
  match graph with
  | [] => []
  | node :: rest =>
      map (dawg_edge_reference (dawg_payload_id node))
          (dawg_payload_edges node) ++ dawg_graph_references rest
  end.

(** Node identifiers are opaque; [rank] is a mathematical witness of the
    physical graph's acyclicity, not a runtime field or a depth limit. *)
Definition dawg_graph_admissible (rank : nat -> nat)
  (graph : list DawgPayload) : Prop :=
  NoDup (map dawg_payload_id graph) /\
  unique_reference_identities (dawg_graph_references graph) /\
  forall parent edge,
    In parent graph -> In edge (dawg_payload_edges parent) ->
    exists child,
      In child graph /\
      dawg_payload_id child = dawg_edge_target edge /\
      rank (dawg_payload_id child) < rank (dawg_payload_id parent).

Definition dawg_graph_step (graph : list DawgPayload)
  (source target : nat) : Prop :=
  exists parent edge,
    In parent graph /\
    dawg_payload_id parent = source /\
    In edge (dawg_payload_edges parent) /\
    dawg_edge_target edge = target.

Inductive dawg_graph_path (graph : list DawgPayload) : nat -> nat -> Prop :=
| DawgDirect : forall source target,
    dawg_graph_step graph source target ->
    dawg_graph_path graph source target
| DawgPrepend : forall source middle target,
    dawg_graph_step graph source middle ->
    dawg_graph_path graph middle target ->
    dawg_graph_path graph source target.

Theorem RSDICT_DAWG_000_admitted_edges_decrease_rank :
  forall rank graph source target,
    dawg_graph_admissible rank graph ->
    dawg_graph_step graph source target ->
    rank target < rank source.
Proof.
  intros rank graph source target [_ [_ Hedges]]
    [parent [edge [Hparent [Hsource [Hedge Htarget]]]]].
  destruct (Hedges parent edge Hparent Hedge) as
    [child [_ [Hchild Hrank]]].
  rewrite <- Hsource, <- Htarget, <- Hchild. exact Hrank.
Qed.

(** Every nonempty path strictly decreases the witness rank, so a valid
    physical DAWG cannot contain a cycle of any finite length. *)
Theorem RSDICT_DAWG_008_admitted_paths_are_acyclic :
  forall rank graph source target,
    dawg_graph_admissible rank graph ->
    dawg_graph_path graph source target ->
    rank target < rank source.
Proof.
  intros rank graph source target Hadmitted Hpath.
  induction Hpath as [a b Hstep | a b c Hstep Htail IH].
  - eapply RSDICT_DAWG_000_admitted_edges_decrease_rank; eauto.
  - pose proof (RSDICT_DAWG_000_admitted_edges_decrease_rank
      rank graph a b Hadmitted Hstep) as Hdecrease.
    lia.
Qed.

Corollary RSDICT_DAWG_009_no_admitted_payload_cycle :
  forall rank graph payload,
    dawg_graph_admissible rank graph ->
    ~ dawg_graph_path graph payload payload.
Proof.
  intros rank graph payload Hadmitted Hcycle.
  pose proof (RSDICT_DAWG_008_admitted_paths_are_acyclic
    rank graph payload payload Hadmitted Hcycle).
  lia.
Qed.

Fixpoint dawg_graph_incoming_count (payload : nat)
  (graph : list DawgPayload) : nat :=
  match graph with
  | [] => 0
  | node :: rest =>
      strong_count payload
        (map (dawg_edge_reference (dawg_payload_id node))
             (dawg_payload_edges node)) +
      dawg_graph_incoming_count payload rest
  end.

(** Every physical edge slot is counted, including two slots targeting one
    shared DAWG node. No tree assumption is hidden in the accounting. *)
Theorem RSDICT_DAWG_001_all_edge_tokens_counted :
  forall graph payload,
    strong_count payload (dawg_graph_references graph) =
    dawg_graph_incoming_count payload graph.
Proof.
  induction graph as [|node rest IH]; intros payload.
  - reflexivity.
  - simpl. rewrite RSDICT_216_strong_count_append. rewrite IH.
    reflexivity.
Qed.

Definition dawg_retained_handle (payload identity : nat) : ArcReference :=
  {| reference_identity := identity;
     reference_payload := payload;
     reference_owner := ExternalHolder identity |}.

(** A retained public handle adds one strong token regardless of the number
    of incoming edge slots or DAG sharing. *)
Theorem RSDICT_DAWG_002_retained_handle_adds_one_token :
  forall graph payload identity,
    strong_count payload
      (dawg_graph_references graph ++
       [dawg_retained_handle payload identity]) =
    S (strong_count payload (dawg_graph_references graph)).
Proof.
  intros graph payload identity.
  rewrite RSDICT_216_strong_count_append.
  simpl. rewrite Nat.eqb_refl. lia.
Qed.

(** Publication and reader capture are independent owners of the same root.
    Their token identities must be distinct and fresh in an admitted ledger;
    the arithmetic counts both owners even though their payload is equal. *)
Theorem RSDICT_DAWG_010_published_and_captured_roots_count_separately :
  forall graph payload published captured,
    published <> captured ->
    strong_count payload
      (dawg_graph_references graph ++
       [dawg_retained_handle payload published;
        dawg_retained_handle payload captured]) =
    S (S (strong_count payload (dawg_graph_references graph))).
Proof.
  intros graph payload published captured _.
  rewrite RSDICT_216_strong_count_append.
  simpl. rewrite !Nat.eqb_refl. lia.
Qed.

(** Aggregate counts erase node identities but retain each operational phase.
    A count is mathematical proof state, not a runtime counter or resource
    limit. The ten constructors below are the non-stuttering TLA action cases;
    source correspondence must establish that every concrete successful
    release/drain/return maps to one of them. *)
Record DawgProgressCounts := {
  dawg_external_tokens : nat;
  dawg_undrained_edges : nat;
  dawg_pending_edges : nat;
  dawg_failed_edges : nat;
  dawg_live_payloads : nat;
  dawg_dropping_payloads : nat;
  dawg_extracted_payloads : nat;
  dawg_draining_payloads : nat
}.

Definition dawg_counts h e p f l d x r : DawgProgressCounts :=
  {| dawg_external_tokens := h;
     dawg_undrained_edges := e;
     dawg_pending_edges := p;
     dawg_failed_edges := f;
     dawg_live_payloads := l;
     dawg_dropping_payloads := d;
     dawg_extracted_payloads := x;
     dawg_draining_payloads := r |}.

Definition dawg_progress_potential (state : DawgProgressCounts) : nat :=
  4 * dawg_external_tokens state +
  4 * dawg_undrained_edges state +
  3 * dawg_pending_edges state +
  2 * dawg_failed_edges state +
  4 * dawg_live_payloads state +
  3 * dawg_dropping_payloads state +
  3 * dawg_extracted_payloads state +
  2 * dawg_draining_payloads state.

Inductive DawgProgressStep : DawgProgressCounts -> DawgProgressCounts -> Prop :=
| DawgReleaseShared : forall h e p f l d x r,
    DawgProgressStep (dawg_counts (S h) e p f l d x r)
                     (dawg_counts h e p f l d x r)
| DawgReleaseFinal : forall h e p f l d x r k,
    DawgProgressStep (dawg_counts (S h) (e + k) p f (S l) d x r)
                     (dawg_counts h e (p + k) f l (S d) x r)
| DawgConsumeShared : forall h e p f l d x r,
    DawgProgressStep (dawg_counts h e (S p) f l d x r)
                     (dawg_counts h e p f l d x r)
| DawgConsumeFailure : forall h e p f l d x r,
    DawgProgressStep (dawg_counts h e (S p) f l d x r)
                     (dawg_counts h e p (S f) l d x r)
| DawgConsumeFinal : forall h e p f l d x r,
    DawgProgressStep (dawg_counts h e (S p) f (S l) d x r)
                     (dawg_counts h e p f l d (S x) r)
| DawgDiscardShared : forall h e p f l d x r,
    DawgProgressStep (dawg_counts h e p (S f) l d x r)
                     (dawg_counts h e p f l d x r)
| DawgDiscardFinal : forall h e p f l d x r k,
    DawgProgressStep (dawg_counts h (e + k) p (S f) (S l) d x r)
                     (dawg_counts h e (p + k) f l (S d) x r)
| DawgBeginExtracted : forall h e p f l d x r k,
    DawgProgressStep (dawg_counts h (e + k) p f l d (S x) r)
                     (dawg_counts h e (p + k) f l d x (S r))
| DawgFinishExtracted : forall h e p f l d x r,
    DawgProgressStep (dawg_counts h e p f l d x (S r))
                     (dawg_counts h e p f l d x r)
| DawgFinishDrop : forall h e p f l d x r,
    DawgProgressStep (dawg_counts h e p f l (S d) x r)
                     (dawg_counts h e p f l d x r).

(** No fixed depth enters this strict-decrease theorem. A larger admitted
    graph increases the initial potential but never changes the law. *)
Theorem RSDICT_DAWG_011_each_progress_step_strictly_decreases_potential :
  forall before after,
    DawgProgressStep before after ->
    dawg_progress_potential after < dawg_progress_potential before.
Proof.
  intros before after Hstep.
  destruct Hstep; unfold dawg_progress_potential, dawg_counts; simpl; lia.
Qed.

Inductive DawgProgressTrace : DawgProgressCounts -> DawgProgressCounts -> nat -> Prop :=
| DawgProgressTraceRefl : forall state,
    DawgProgressTrace state state 0
| DawgProgressTraceNext : forall start middle finish length,
    DawgProgressTrace start middle length ->
    DawgProgressStep middle finish ->
    DawgProgressTrace start finish (S length).

(** Every finite prefix has a length bounded by its own initial potential;
    hence no infinite sequence of non-stuttering reclamation actions exists.
    This is not an enabledness or application-holder-release theorem. *)
Theorem RSDICT_DAWG_012_progress_trace_has_finite_step_budget :
  forall start finish length,
    DawgProgressTrace start finish length ->
    length + dawg_progress_potential finish <= dawg_progress_potential start.
Proof.
  intros start finish length Htrace.
  induction Htrace as [state | start middle finish length Hprefix IH Hstep].
  - lia.
  - pose proof (RSDICT_DAWG_011_each_progress_step_strictly_decreases_potential
      middle finish Hstep) as Hdecrease.
    lia.
Qed.

(** One payload's strong-count lifecycle is independent of the size of the
    graph containing it. Failed try_unwrap retains its token and therefore is
    a stuttering lifecycle event until its later ordinary release; the generic
    Arc ledger proves that ownership fact. A finish action here presupposes
    that the TLA edge/worklist machine has drained the payload's children. *)
Inductive DawgPayloadLife :=
| DawgLive
| DawgDropping
| DawgExtracted
| DawgDraining
| DawgDestroyed.

Record DawgLifeState := {
  dawg_life : DawgPayloadLife;
  dawg_strong : nat;
  dawg_destroy_count : nat
}.

Definition dawg_life_state life strong destroyed : DawgLifeState :=
  {| dawg_life := life;
     dawg_strong := strong;
     dawg_destroy_count := destroyed |}.

Inductive DawgLifeStep : DawgLifeState -> DawgLifeState -> Prop :=
| DawgLifeAcquire : forall n,
    DawgLifeStep (dawg_life_state DawgLive (S n) 0)
                 (dawg_life_state DawgLive (S (S n)) 0)
| DawgLifeReleaseShared : forall n,
    DawgLifeStep (dawg_life_state DawgLive (S (S n)) 0)
                 (dawg_life_state DawgLive (S n) 0)
| DawgLifeReleaseLast :
    DawgLifeStep (dawg_life_state DawgLive 1 0)
                 (dawg_life_state DawgDropping 0 0)
| DawgLifeInnerShared : forall n,
    DawgLifeStep (dawg_life_state DawgLive (S (S n)) 0)
                 (dawg_life_state DawgLive (S n) 0)
| DawgLifeInnerLast :
    DawgLifeStep (dawg_life_state DawgLive 1 0)
                 (dawg_life_state DawgExtracted 0 0)
| DawgLifeBeginDrain :
    DawgLifeStep (dawg_life_state DawgExtracted 0 0)
                 (dawg_life_state DawgDraining 0 0)
| DawgLifeFinishOrdinaryDrop :
    DawgLifeStep (dawg_life_state DawgDropping 0 0)
                 (dawg_life_state DawgDestroyed 0 1)
| DawgLifeFinishExtractedDrop :
    DawgLifeStep (dawg_life_state DawgDraining 0 0)
                 (dawg_life_state DawgDestroyed 0 1).

Definition DawgLifeInvariant (state : DawgLifeState) : Prop :=
  match dawg_life state with
  | DawgLive => dawg_strong state > 0 /\ dawg_destroy_count state = 0
  | DawgDestroyed => dawg_strong state = 0 /\ dawg_destroy_count state = 1
  | DawgDropping | DawgExtracted | DawgDraining =>
      dawg_strong state = 0 /\ dawg_destroy_count state = 0
  end.

Theorem RSDICT_DAWG_013_lifecycle_step_preserves_strong_and_destroy_state :
  forall before after,
    DawgLifeInvariant before ->
    DawgLifeStep before after ->
    DawgLifeInvariant after.
Proof.
  intros before after _ Hstep.
  destruct Hstep; unfold DawgLifeInvariant, dawg_life_state; simpl; lia.
Qed.

Inductive DawgLifeTrace : DawgLifeState -> DawgLifeState -> Prop :=
| DawgLifeTraceRefl : forall state,
    DawgLifeTrace state state
| DawgLifeTraceNext : forall start middle finish,
    DawgLifeTrace start middle ->
    DawgLifeStep middle finish ->
    DawgLifeTrace start finish.

Theorem RSDICT_DAWG_014_unbounded_lifecycle_has_no_premature_or_double_destroy :
  forall initial_strong state,
    DawgLifeTrace (dawg_life_state DawgLive (S initial_strong) 0) state ->
    dawg_destroy_count state <= 1 /\
    (dawg_strong state > 0 -> dawg_life state = DawgLive) /\
    (dawg_life state = DawgDestroyed -> dawg_strong state = 0).
Proof.
  intros initial_strong state Htrace.
  assert (Hgeneral : forall start finish,
    DawgLifeTrace start finish ->
    DawgLifeInvariant start -> DawgLifeInvariant finish).
  { intros start finish Hsteps.
    induction Hsteps as [current | first middle last Hprefix IH Hstep];
      intro Hvalid.
    - exact Hvalid.
    - eapply RSDICT_DAWG_013_lifecycle_step_preserves_strong_and_destroy_state.
      + apply IH. exact Hvalid.
      + exact Hstep. }
  pose proof (Hgeneral _ _ Htrace) as Hvalid.
  assert (Hinitial : DawgLifeInvariant
    (dawg_life_state DawgLive (S initial_strong) 0)).
  { unfold DawgLifeInvariant, dawg_life_state. simpl. lia. }
  specialize (Hvalid Hinitial).
  destruct state as [life strong destroyed].
  destruct life; cbn [DawgLifeInvariant dawg_life dawg_strong dawg_destroy_count] in *;
    destruct Hvalid as [Hstrong Hcount]; repeat split; intros;
    try congruence; lia.
Qed.

(** IntoInner has only invalid, shared-release, or exclusive-extraction
    results. In particular, it cannot itself invoke a child payload Drop
    while a parent native Drop frame is active. *)
Theorem RSDICT_DAWG_003_into_inner_never_invokes_payload_drop :
  forall ledger identity payload,
    arc_signal (into_inner_reference identity ledger) <>
      InvokePayloadDrop payload.
Proof.
  intros ledger identity payload.
  unfold into_inner_reference.
  destruct (take_reference identity ledger) as [[reference remaining]|];
    [destruct (strong_count (reference_payload reference) remaining)|];
    discriminate.
Qed.

Theorem RSDICT_DAWG_004_retained_owner_forces_shared_release :
  forall ledger identity reference remaining,
    take_reference identity ledger = Some (reference, remaining) ->
    strong_count (reference_payload reference) remaining > 0 ->
    arc_signal (into_inner_reference identity ledger) =
      ReleasedShared (reference_payload reference).
Proof.
  intros ledger identity reference remaining Htake Hretained.
  unfold into_inner_reference. rewrite Htake.
  destruct (strong_count (reference_payload reference) remaining);
    [lia|reflexivity].
Qed.

(** The first component is the number of persistent library-owned native
    Drop frames on one executing thread. The second is the peak, including
    the transient empty Drop of a heap-worklist payload after its edges are
    taken. A final external handle release can enter only from an idle
    thread; the edge operation above cannot synchronously enter Drop. *)
Inductive SafeNativeStep : (nat * nat) -> (nat * nat) -> Prop :=
| SafeEnterRoot : forall peak,
    SafeNativeStep (0, peak) (1, Nat.max peak 1)
| SafeConsumeEdge : forall peak,
    SafeNativeStep (1, peak) (1, peak)
| SafeDropEmptyLocal : forall peak,
    SafeNativeStep (1, peak) (1, Nat.max peak 2)
| SafeReturnRoot : forall peak,
    SafeNativeStep (1, peak) (0, peak).

Definition SafeNativeBound (state : nat * nat) : Prop :=
  fst state <= 1 /\ snd state <= 2.

Theorem RSDICT_DAWG_005_one_safe_step_preserves_native_bound :
  forall before after,
    SafeNativeBound before ->
    SafeNativeStep before after ->
    SafeNativeBound after.
Proof.
  intros before after Hbound Hstep.
  destruct Hstep; unfold SafeNativeBound in *; simpl in *;
    destruct Hbound as [Hframes Hpeak]; split; try lia;
    apply Nat.max_lub; lia.
Qed.

Inductive SafeNativeTrace : (nat * nat) -> (nat * nat) -> Prop :=
| SafeNativeTraceRefl : forall state,
    SafeNativeTrace state state
| SafeNativeTraceNext : forall start middle finish,
    SafeNativeTrace start middle ->
    SafeNativeStep middle finish ->
    SafeNativeTrace start finish.

Local Lemma safe_trace_preserves_native_bound :
  forall start finish,
    SafeNativeTrace start finish ->
    SafeNativeBound start ->
    SafeNativeBound finish.
Proof.
  intros start finish Htrace.
  induction Htrace as [state | start middle finish Hprefix IH Hstep];
    intro Hbound.
  - exact Hbound.
  - eapply RSDICT_DAWG_005_one_safe_step_preserves_native_bound.
    + apply IH. exact Hbound.
    + exact Hstep.
Qed.

(** This induction is over an arbitrarily long execution; the bound is not
    inferred from TLC's finite model depth. *)
Theorem RSDICT_DAWG_006_unbounded_safe_trace_native_bound :
  forall state,
    SafeNativeTrace (0, 0) state ->
    SafeNativeBound state.
Proof.
  intros state Htrace.
  eapply safe_trace_preserves_native_bound.
  - exact Htrace.
  - unfold SafeNativeBound. simpl. lia.
Qed.

(** The generic Arc ledger and the native-frame machine meet at every
    source-level edge attempt, regardless of graph size or token sharing. *)
Theorem RSDICT_DAWG_007_safe_edge_source_correspondence_boundary :
  forall ledger identity payload peak,
    arc_signal (into_inner_reference identity ledger) <>
      InvokePayloadDrop payload /\
    SafeNativeStep (1, peak) (1, peak).
Proof.
  intros ledger identity payload peak. split.
  - apply RSDICT_DAWG_003_into_inner_never_invokes_payload_drop.
  - constructor.
Qed.
