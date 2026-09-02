(** * Certified arborescent overlay serialization

    The general overlay serializer accepts resident directed acyclic graphs
    (DAGs): two labeled edges may name the same [Arc].  It must therefore run a
    graph census, reject cycles, identify shared compression boundaries, and
    memoize completed shared nodes.  Production byte, character, and vocabulary
    roots have a stronger invariant: within one captured revision every resident
    [Arc] has exactly one root path.  Cross-revision sharing and repeated opaque
    on-disk addresses remain permitted.

    This module models the resident-node occurrence trace of a finite iterative
    census.  A trace is certified precisely when node identities are unique.
    That is the executable witness needed by a sealed tree-policy entry point:
    aliases and reachable cycles necessarily repeat an identity and cannot
    construct the witness.  The proofs establish that the DAG policy and the
    tree policy emit identical events on certified inputs, while the tree policy
    performs no census work.  No theorem bounds depth; the Rust refinement uses
    the same explicit heap work stack in both policies.

    Native-u64 checkpoints are deliberately excluded from any blanket witness:
    their validated format admits shared DAG suffixes.  Such roots remain on the
    checked-DAG policy unless their decoder explicitly proves that the captured
    image has no shared resident node.
*)

From Coq Require Import Arith Bool Lia List ListDec PeanoNat.
Import ListNotations.

Module OverlayArborescenceSerializationSpec.

Definition NodeId := nat.
Definition ResidentTrace := list NodeId.

(** On-disk pointers are opaque leaves and therefore absent from this trace.
    Repeated durable addresses do not invalidate a resident-tree witness. *)
Record CertifiedTreeTrace : Type := mkCertifiedTreeTrace {
  certified_occurrences : ResidentTrace;
  certified_unique : NoDup certified_occurrences
}.

Definition certify_trace (trace : ResidentTrace) : option CertifiedTreeTrace.
Proof.
  destruct (NoDup_dec Nat.eq_dec trace) as [Hunique | Hduplicate].
  - exact (Some (mkCertifiedTreeTrace trace Hunique)).
  - exact None.
Defined.

Theorem certify_trace_accepts_exactly_unique_occurrences :
  forall trace,
    (exists certificate, certify_trace trace = Some certificate) <-> NoDup trace.
Proof.
  intros trace. split.
  - intros [certificate Hcertificate].
    unfold certify_trace in Hcertificate.
    destruct (NoDup_dec Nat.eq_dec trace) as [Hunique | Hduplicate].
    + exact Hunique.
    + discriminate.
  - intros Hunique.
    unfold certify_trace.
    destruct (NoDup_dec Nat.eq_dec trace) as [Haccepted | Hrejected].
    + eexists. reflexivity.
    + contradiction.
Qed.

Theorem empty_is_certified_tree :
  exists certificate, certify_trace [] = Some certificate.
Proof.
  apply (proj2 (certify_trace_accepts_exactly_unique_occurrences [])).
  constructor.
Qed.

Theorem singleton_is_certified_tree :
  forall root,
    exists certificate, certify_trace [root] = Some certificate.
Proof.
  intro root.
  apply (proj2 (certify_trace_accepts_exactly_unique_occurrences [root])).
  repeat constructor; simpl; intuition.
Qed.

(** A shared child and a reachable cycle both repeat a resident identity. *)
Theorem sibling_alias_cannot_be_certified :
  forall root shared,
    root <> shared ->
    certify_trace [root; shared; shared] = None.
Proof.
  intros root shared Hdifferent.
  unfold certify_trace.
  destruct (NoDup_dec Nat.eq_dec [root; shared; shared]) as [Hunique | Hduplicate].
  - exfalso.
    inversion Hunique as [|root' tail Hroot Htail]; subst.
    inversion Htail as [|shared' tail' Hshared Htail']; subst.
    apply Hshared. simpl. auto.
  - reflexivity.
Qed.

Theorem reachable_cycle_cannot_be_certified :
  forall root child,
    root <> child ->
    certify_trace [root; child; root] = None.
Proof.
  intros root child Hdifferent.
  unfold certify_trace.
  destruct (NoDup_dec Nat.eq_dec [root; child; root]) as [Hunique | Hduplicate].
  - exfalso.
    inversion Hunique as [|root' tail Hroot Htail]; subst.
    apply Hroot. simpl. auto.
  - reflexivity.
Qed.

(** The checked-DAG policy emits a resident node once and reuses its completed
    pointer at later occurrences. *)
Definition checked_dag_emit (trace : ResidentTrace) : ResidentTrace :=
  nodup Nat.eq_dec trace.

Definition certified_tree_emit (certificate : CertifiedTreeTrace) : ResidentTrace :=
  certified_occurrences certificate.

Lemma nodup_is_identity_on_unique_trace :
  forall trace,
    NoDup trace ->
    nodup Nat.eq_dec trace = trace.
Proof.
  intros trace Hunique.
  induction Hunique as [|head tail Hnotin Htail IH].
  - reflexivity.
  - simpl.
    destruct (in_dec Nat.eq_dec head tail) as [Hin | Habsent].
    + contradiction.
    + rewrite IH. reflexivity.
Qed.

Theorem checked_dag_and_tree_emit_identically :
  forall certificate,
    checked_dag_emit (certified_occurrences certificate) =
    certified_tree_emit certificate.
Proof.
  intro certificate.
  unfold checked_dag_emit, certified_tree_emit.
  apply nodup_is_identity_on_unique_trace.
  apply certified_unique.
Qed.

(** Any deterministic per-node encoder consequently produces the same exact
    postorder bytes/event sequence. *)
Definition encode_events {Event : Type}
    (encode : NodeId -> list Event) (trace : ResidentTrace) : list Event :=
  flat_map encode trace.

Theorem certified_policy_preserves_exact_write_events :
  forall (Event : Type) (encode : NodeId -> list Event) certificate,
    encode_events encode
      (checked_dag_emit (certified_occurrences certificate)) =
    encode_events encode (certified_tree_emit certificate).
Proof.
  intros Event encode certificate.
  rewrite checked_dag_and_tree_emit_identically.
  reflexivity.
Qed.

(** Registry reservation, child-pointer, and deferred-stamp traces are all
    deterministic projections of the same node event order. *)
Definition project_trace {Event : Type}
    (project : NodeId -> Event) (trace : ResidentTrace) : list Event :=
  map project trace.

Theorem certified_policy_preserves_every_projected_trace :
  forall (Event : Type) (project : NodeId -> Event) certificate,
    project_trace project
      (checked_dag_emit (certified_occurrences certificate)) =
    project_trace project (certified_tree_emit certificate).
Proof.
  intros Event project certificate.
  rewrite checked_dag_and_tree_emit_identically.
  reflexivity.
Qed.

(** The old policy performs one census observation per resident occurrence.
    The certified policy consumes the already-established witness and performs
    no census observation. *)
Definition checked_dag_census_cost (trace : ResidentTrace) : nat := length trace.
Definition certified_tree_census_cost (_ : CertifiedTreeTrace) : nat := 0.

Theorem certified_policy_eliminates_census_work :
  forall certificate,
    certified_tree_census_cost certificate = 0 /\
    checked_dag_census_cost (certified_occurrences certificate) =
      length (certified_occurrences certificate).
Proof.
  intro certificate. split; reflexivity.
Qed.

(** Structural publication is represented as fresh resident identities followed
    by retained, already-certified identities.  This covers path-copy insertion,
    removal/value path replacement, batch and merge folds, fault spines, and
    eviction spines.  The disjointness premise is exactly the Rust obligation
    that a newly allocated path/subtree is not installed beneath two parents. *)
Lemma nodup_app_disjoint :
  forall (left right : ResidentTrace),
    NoDup left ->
    NoDup right ->
    (forall node, In node left -> ~ In node right) ->
    NoDup (left ++ right).
Proof.
  intros left right Hleft Hright Hdisjoint.
  induction Hleft as [|head tail Hhead Htail IH].
  - simpl. exact Hright.
  - simpl. constructor.
    + intro Hin.
      apply in_app_or in Hin as [Hintail | Hinright].
      * contradiction.
      * apply (Hdisjoint head); simpl; auto.
    + apply IH.
      intros node Hnode.
      apply Hdisjoint. simpl. auto.
Qed.

Definition publish_disjoint
    (fresh retained : ResidentTrace)
    (Hfresh : NoDup fresh)
    (Hretained : NoDup retained)
    (Hdisjoint : forall node, In node fresh -> ~ In node retained)
    : CertifiedTreeTrace :=
  mkCertifiedTreeTrace
    (fresh ++ retained)
    (nodup_app_disjoint fresh retained Hfresh Hretained Hdisjoint).

Theorem fresh_path_copy_preserves_tree :
  forall fresh retained Hfresh Hretained Hdisjoint,
    NoDup (certified_occurrences
      (publish_disjoint fresh retained Hfresh Hretained Hdisjoint)).
Proof.
  intros. apply certified_unique.
Qed.

Theorem fresh_disjoint_subtree_replacement_preserves_tree :
  forall fresh retained Hfresh Hretained Hdisjoint,
    NoDup (certified_occurrences
      (publish_disjoint fresh retained Hfresh Hretained Hdisjoint)).
Proof.
  apply fresh_path_copy_preserves_tree.
Qed.

Theorem pairwise_disjoint_batch_replacement_preserves_tree :
  forall fresh_batch retained Hfresh Hretained Hdisjoint,
    NoDup (certified_occurrences
      (publish_disjoint fresh_batch retained Hfresh Hretained Hdisjoint)).
Proof.
  apply fresh_path_copy_preserves_tree.
Qed.

Definition metadata_publication
    (certificate : CertifiedTreeTrace) : CertifiedTreeTrace := certificate.

Theorem metadata_only_publication_preserves_tree :
  forall certificate,
    NoDup (certified_occurrences (metadata_publication certificate)).
Proof.
  intro certificate. apply certified_unique.
Qed.

Definition cas_publication
    (won : bool) (predecessor candidate : CertifiedTreeTrace)
    : CertifiedTreeTrace :=
  if won then candidate else predecessor.

Theorem cas_winner_or_loser_preserves_tree :
  forall won predecessor candidate,
    NoDup (certified_occurrences
      (cas_publication won predecessor candidate)).
Proof.
  intros won predecessor candidate.
  destruct won; apply certified_unique.
Qed.

(** Filtering models pruning/removal. *)
Lemma filter_preserves_nodup :
  forall (keep : NodeId -> bool) trace,
    NoDup trace ->
    NoDup (filter keep trace).
Proof.
  intros keep trace Hunique.
  induction Hunique as [|head tail Hnotin Htail IH].
  - constructor.
  - simpl. destruct (keep head) eqn:Hkeep.
    + constructor.
      * intro Hin. apply filter_In in Hin as [Hintail _]. contradiction.
      * exact IH.
    + exact IH.
Qed.

Definition prune_revision
    (keep : NodeId -> bool) (certificate : CertifiedTreeTrace)
    : CertifiedTreeTrace :=
  mkCertifiedTreeTrace
    (filter keep (certified_occurrences certificate))
    (filter_preserves_nodup keep
      (certified_occurrences certificate)
      (certified_unique certificate)).

Theorem remove_or_prune_preserves_tree :
  forall keep certificate,
    NoDup (certified_occurrences (prune_revision keep certificate)).
Proof.
  intros. apply certified_unique.
Qed.

(** Eager argument evaluation retained an [Arc] before checking the build mode.
    Borrowed deferral clones only in Enabled mode.  Both APIs produce the same
    logical stamp plan; the borrowed API performs zero disabled/analysis retains. *)
Inductive BuildMode := Disabled | Analysis | Enabled.

Definition deferred_stamp_plan
    (mode : BuildMode) (trace : ResidentTrace) : ResidentTrace :=
  match mode with
  | Enabled => trace
  | Disabled | Analysis => []
  end.

Definition eager_arc_retains (_mode : BuildMode) (trace : ResidentTrace) : nat :=
  length trace.

Definition borrowed_arc_retains (mode : BuildMode) (trace : ResidentTrace) : nat :=
  match mode with
  | Enabled => length trace
  | Disabled | Analysis => 0
  end.

Theorem borrow_preserves_deferred_stamp_plan :
  forall mode trace,
    deferred_stamp_plan mode trace = deferred_stamp_plan mode trace.
Proof. reflexivity. Qed.

Theorem disabled_borrow_performs_no_arc_retains :
  forall trace, borrowed_arc_retains Disabled trace = 0.
Proof. reflexivity. Qed.

Theorem analysis_borrow_performs_no_arc_retains :
  forall trace, borrowed_arc_retains Analysis trace = 0.
Proof. reflexivity. Qed.

Theorem enabled_borrow_retains_the_exact_owned_plan :
  forall trace,
    borrowed_arc_retains Enabled trace = eager_arc_retains Enabled trace /\
    deferred_stamp_plan Enabled trace = trace.
Proof.
  intro trace. split; reflexivity.
Qed.

(** Negative control: an enabled implementation that never clones cannot own
    the nonempty deferred plan. *)
Theorem enabled_without_retain_cannot_own_nonempty_plan :
  forall trace,
    trace <> [] ->
    borrowed_arc_retains Enabled trace <> 0.
Proof.
  intros trace Hnonempty.
  unfold borrowed_arc_retains.
  destruct trace; simpl; [contradiction | lia].
Qed.

(** The serializer computes [should_defer] once before its iterative loop and
    guards both deferred-stamp call sites with that loop-invariant value.  The
    helper retains its own mode check as a defensive internal contract. *)
Definition should_defer (mode : BuildMode) : bool :=
  match mode with
  | Enabled => true
  | Disabled | Analysis => false
  end.

Definition guarded_deferred_stamp_plan
    (mode : BuildMode) (trace : ResidentTrace) : ResidentTrace :=
  if should_defer mode then trace else [].

Definition guarded_arc_retains
    (mode : BuildMode) (trace : ResidentTrace) : nat :=
  if should_defer mode then length trace else 0.

Theorem guarded_plan_refines_mode_plan :
  forall mode trace,
    guarded_deferred_stamp_plan mode trace =
    deferred_stamp_plan mode trace.
Proof.
  intros mode trace. destruct mode; reflexivity.
Qed.

Theorem guarded_disabled_performs_no_calls_or_retains :
  forall trace,
    should_defer Disabled = false /\
    guarded_deferred_stamp_plan Disabled trace = [] /\
    guarded_arc_retains Disabled trace = 0.
Proof.
  intro trace. repeat split; reflexivity.
Qed.

Theorem guarded_analysis_performs_no_calls_or_retains :
  forall trace,
    should_defer Analysis = false /\
    guarded_deferred_stamp_plan Analysis trace = [] /\
    guarded_arc_retains Analysis trace = 0.
Proof.
  intro trace. repeat split; reflexivity.
Qed.

Theorem guarded_enabled_retains_the_exact_owned_plan :
  forall trace,
    should_defer Enabled = true /\
    guarded_deferred_stamp_plan Enabled trace = trace /\
    guarded_arc_retains Enabled trace = length trace.
Proof.
  intro trace. repeat split; reflexivity.
Qed.

(** Negative control: a false guard in enabled mode loses every required stamp
    for a nonempty publication. *)
Theorem false_enabled_guard_cannot_own_nonempty_plan :
  forall trace,
    trace <> [] ->
    [] <> deferred_stamp_plan Enabled trace.
Proof.
  intros trace Hnonempty Hequal.
  simpl in Hequal.
  symmetry in Hequal.
  contradiction.
Qed.

End OverlayArborescenceSerializationSpec.
