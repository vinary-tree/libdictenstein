---------------------- MODULE BackgroundWorkerLifecycle ----------------------
(***************************************************************************)
(* Shutdown protocol of a `PersistentARTrieChar` background daemon worker  *)
(* (wal-sync / artrie-eviction / artrie-memory-monitor) AFTER the          *)
(* Weak-handle and self-join fixes in libdictenstein.                      *)
(*                                                                         *)
(* The bug being prevented: each worker used to capture a *strong* `Arc`    *)
(* to the manager whose `Drop` was the only thing that set its stop flag.   *)
(* The strong-count therefore never reached zero, `Drop` never ran, the     *)
(* flag was never set, and the worker looped forever — one leaked OS thread *)
(* per trie instance.                                                       *)
(*                                                                         *)
(* The fix: the worker holds a `Weak` and POLLS a shutdown flag each        *)
(* iteration, never blocking on a resource the owner holds while the owner  *)
(* waits to join it. The owner (`close()` / `Drop` /                        *)
(* `EvictionCoordinator::shutdown` / `SegmentSyncManager::stop`) sets the   *)
(* flag, then joins. The related `disable_eviction` deadlock fix is modeled *)
(* by the owner NOT holding the trie lock across the join — here, the        *)
(* worker's exit step is ALWAYS enabled once the flag is set, i.e. it never  *)
(* depends on the owner releasing anything.                                 *)
(*                                                                         *)
(* Verified:                                                               *)
(* A second bug being prevented: if the last strong reference is the worker  *)
(* thread's per-iteration Weak::upgrade(), Drop runs on the worker itself.   *)
(* POSIX correctly rejects joining the current thread with EDEADLK. The      *)
(* fixed shutdown path detects that case and detaches the handle instead of  *)
(* calling join().                                                           *)
(*                                                                         *)
(* Verified:                                                               *)
(*   - TypeOK       (safety)  — state stays well-typed.                     *)
(*   - NoOrphan     (safety)  — owner-thread join never reports "joined"    *)
(*                              while the worker is still running.          *)
(*   - NoSelfJoin   (safety)  — worker-thread teardown never takes the       *)
(*                              owner-thread join transition.               *)
(*   - Termination  (liveness)— teardown always completes; no thread leak,  *)
(*                              no hung join/self-join deadlock.            *)
(***************************************************************************)

VARIABLES
    flag,      \* shutdown requested by the owner (AtomicBool)
    worker,    \* "running" | "exited"
    owner,     \* "active" | "closing" | "joined" | "detached"
    joiner     \* "none" | "owner_thread" | "worker_thread"

vars == <<flag, worker, owner, joiner>>

Init ==
    /\ flag = FALSE
    /\ worker = "running"
    /\ owner = "active"
    /\ joiner = "none"

(* Owner teardown, step 1: request shutdown. Models close()/Drop calling     *)
(* coordinator.shutdown() / sync_manager.stop() / monitor.shutdown(). The     *)
(* owner does NOT hold the trie lock here (disable_eviction takes the manager *)
(* out under a short guard, then releases it before joining), so this never   *)
(* blocks the worker.                                                         *)
RequestShutdown ==
    /\ owner = "active"
    /\ flag' = TRUE
    /\ owner' = "closing"
    /\ joiner' = "owner_thread"
    /\ UNCHANGED worker

(* Worker-side teardown: the last strong Arc can be the worker's upgraded Weak.
 * Drop then runs on the worker thread. The correct implementation sets the
 * shutdown flag but must NOT join the current thread. *)
WorkerInitiatesShutdown ==
    /\ owner = "active"
    /\ worker = "running"
    /\ flag' = TRUE
    /\ owner' = "closing"
    /\ joiner' = "worker_thread"
    /\ UNCHANGED worker

(* Worker poll iteration: it sees the flag (or fails to upgrade its Weak) and *)
(* exits. This step is ALWAYS enabled while running once the flag is set — it  *)
(* requires no resource the owner holds. This is the essence of the           *)
(* Weak + 100ms-poll design that replaced the strong-Arc + condvar wait.      *)
WorkerExit ==
    /\ worker = "running"
    /\ flag = TRUE
    /\ worker' = "exited"
    /\ UNCHANGED <<flag, owner, joiner>>

(* Owner-thread teardown, step 2: join completes once the worker has exited. *)
Join ==
    /\ owner = "closing"
    /\ joiner = "owner_thread"
    /\ worker = "exited"
    /\ owner' = "joined"
    /\ UNCHANGED <<flag, worker, joiner>>

(* Worker-thread teardown: the stored JoinHandle names the current thread, so
 * shutdown drops/detaches the handle instead of joining it. The worker can
 * then return from Drop and exit normally. *)
DetachSelfHandle ==
    /\ owner = "closing"
    /\ joiner = "worker_thread"
    /\ owner' = "detached"
    /\ UNCHANGED <<flag, worker, joiner>>

Next ==
    \/ RequestShutdown
    \/ WorkerInitiatesShutdown
    \/ WorkerExit
    \/ Join
    \/ DetachSelfHandle

Spec ==
    /\ Init
    /\ [][Next]_vars
    /\ WF_vars(RequestShutdown)
    /\ WF_vars(WorkerInitiatesShutdown)
    /\ WF_vars(WorkerExit)
    /\ WF_vars(Join)
    /\ WF_vars(DetachSelfHandle)

----------------------------------------------------------------------------

TypeOK ==
    /\ flag \in BOOLEAN
    /\ worker \in {"running", "exited"}
    /\ owner \in {"active", "closing", "joined", "detached"}
    /\ joiner \in {"none", "owner_thread", "worker_thread"}

(* The owner never reports the worker joined while it is still running. *)
NoOrphan == (owner = "joined") => (worker = "exited")

(* Worker-side teardown must detach the current-thread handle; it must never
 * take the owner-thread join transition. *)
NoSelfJoin == (joiner = "worker_thread") => (owner # "joined")

(* Teardown always finishes: no orphaned thread, no join that hangs forever. *)
Termination ==
    (owner = "active") ~> ((owner = "joined" \/ owner = "detached") /\ worker = "exited")

=============================================================================
