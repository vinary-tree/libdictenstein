(** * WorkerLifecycle.v — background-daemon shutdown protocol

    Machine-checked companion to [tla+/BackgroundWorkerLifecycle.tla] and the
    Rust thread-leak fix (workers hold a [Weak]; [close]/[Drop]/[shutdown]/[stop]
    set a flag then join). This models the owner/worker teardown handshake as a
    small labelled transition system and proves:

      - [no_orphan_invariant] : a *safety* invariant — the owner never reports the
        worker "joined" while the worker is still running (no orphaned thread);
      - [init_reaches_joined] : a *liveness* witness — from the initial state the
        protocol can always run to completion (stop requested, worker exited,
        join finished), so teardown does not get stuck;
      - [request_shutdown_idempotent] : calling shutdown twice equals calling it
        once (mirrors the idempotent [Option::take] teardown in the Rust code);
      - [request_shutdown_is_step] : the functional [request_shutdown] agrees with
        the relational [step].

    Fully constructive: no [Admitted], no [Axiom], no external dependencies. *)

(** Worker thread status. *)
Inductive worker_state : Set := Running | Exited.

(** Owner teardown status: [Active] (normal), [Closing] (stop requested, join
    pending), [Joined] (worker reclaimed). *)
Inductive owner_state : Set := Active | Closing | Joined.

(** Protocol state: the shutdown [flag] (an [AtomicBool] in the code), the
    worker status, and the owner status. *)
Record state : Set := mk_state {
  flag   : bool;
  worker : worker_state;
  owner  : owner_state;
}.

(** Initial state: running worker, active owner, flag clear. *)
Definition init : state := mk_state false Running Active.

(** One step of the protocol. *)
Inductive step : state -> state -> Prop :=
(** Owner requests shutdown: sets the flag and moves to [Closing]. Enabled only
    while [Active] (flag still clear) — models [close]/[shutdown] being called
    without holding any resource the worker needs. *)
| StepRequestShutdown : forall w,
    step (mk_state false w Active) (mk_state true w Closing)
(** Worker observes the flag (or fails to upgrade its [Weak]) and exits. Enabled
    once the flag is set — it depends on no resource the owner holds, which is
    the essence of the [Weak] + poll design that replaced the strong-[Arc] +
    condvar wait. *)
| StepWorkerExit : forall o,
    step (mk_state true Running o) (mk_state true Exited o)
(** Join completes once the worker has exited. *)
| StepJoin : forall f,
    step (mk_state f Exited Closing) (mk_state f Exited Joined).

(** States reachable from [init] via [step]. *)
Inductive reachable : state -> Prop :=
| reach_init : reachable init
| reach_step : forall s s', reachable s -> step s s' -> reachable s'.

(** Safety: the owner is never [Joined] while the worker is still [Running]. *)
Definition no_orphan (s : state) : Prop :=
  owner s = Joined -> worker s = Exited.

Theorem no_orphan_invariant : forall s, reachable s -> no_orphan s.
Proof.
  intros s H. induction H as [| s s' Hreach IH Hstep].
  - (* init: owner = Active, so the implication is vacuous. *)
    unfold no_orphan, init. simpl. intro Hc. discriminate Hc.
  - (* preserved by every step (the target state already satisfies it). *)
    destruct Hstep; unfold no_orphan in *; simpl in *.
    + intro Hc. discriminate Hc.   (* RequestShutdown: owner = Closing *)
    + intro Hc. reflexivity.        (* WorkerExit: worker = Exited *)
    + intro Hc. reflexivity.        (* Join: worker = Exited *)
Qed.

(** Reflexive–transitive closure of [step]. *)
Inductive steps : state -> state -> Prop :=
| steps_refl : forall s, steps s s
| steps_step : forall s s' s'', step s s' -> steps s' s'' -> steps s s''.

(** Liveness witness: teardown always *can* complete — there is a run from
    [init] to a [Joined] state. *)
Theorem init_reaches_joined :
  exists s, steps init s /\ owner s = Joined.
Proof.
  exists (mk_state true Exited Joined). split.
  - eapply steps_step. apply StepRequestShutdown.
    eapply steps_step. apply StepWorkerExit.
    eapply steps_step. apply StepJoin.
    apply steps_refl.
  - reflexivity.
Qed.

(** Functional model of "request shutdown": idempotent because it only fires
    while [Active] (mirrors the Rust teardown taking its [JoinHandle] via
    [Option::take], so a second call is a no-op). *)
Definition request_shutdown (s : state) : state :=
  match owner s with
  | Active => mk_state true (worker s) Closing
  | _      => s
  end.

Theorem request_shutdown_idempotent : forall s,
  request_shutdown (request_shutdown s) = request_shutdown s.
Proof.
  intros [f w o]. destruct o; simpl; reflexivity.
Qed.

(** The functional [request_shutdown] agrees with the relational [step] from any
    [Active] state. *)
Theorem request_shutdown_is_step : forall w,
  step (mk_state false w Active) (request_shutdown (mk_state false w Active)).
Proof.
  intro w. simpl. apply StepRequestShutdown.
Qed.
