---------------------- MODULE BackgroundWorkerLifecycle ----------------------
(***************************************************************************)
(* Shutdown protocol of a `PersistentARTrieChar` background daemon worker  *)
(* (wal-sync / artrie-eviction / artrie-memory-monitor) AFTER the          *)
(* Weak-handle fix in libdictenstein.                                      *)
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
(*   - TypeOK       (safety)  — state stays well-typed.                     *)
(*   - NoOrphan     (safety)  — the owner never reports "joined" while the  *)
(*                              worker is still running (no orphan thread).  *)
(*   - Termination  (liveness)— teardown always completes; no thread leak,  *)
(*                              no hung join.                               *)
(***************************************************************************)

VARIABLES
    flag,      \* shutdown requested by the owner (AtomicBool)
    worker,    \* "running" | "exited"
    owner      \* "active" | "closing" | "joined"

vars == <<flag, worker, owner>>

Init ==
    /\ flag = FALSE
    /\ worker = "running"
    /\ owner = "active"

(* Owner teardown, step 1: request shutdown. Models close()/Drop calling     *)
(* coordinator.shutdown() / sync_manager.stop() / monitor.shutdown(). The     *)
(* owner does NOT hold the trie lock here (disable_eviction takes the manager *)
(* out under a short guard, then releases it before joining), so this never   *)
(* blocks the worker.                                                         *)
RequestShutdown ==
    /\ owner = "active"
    /\ flag' = TRUE
    /\ owner' = "closing"
    /\ UNCHANGED worker

(* Worker poll iteration: it sees the flag (or fails to upgrade its Weak) and *)
(* exits. This step is ALWAYS enabled while running once the flag is set — it  *)
(* requires no resource the owner holds. This is the essence of the           *)
(* Weak + 100ms-poll design that replaced the strong-Arc + condvar wait.      *)
WorkerExit ==
    /\ worker = "running"
    /\ flag = TRUE
    /\ worker' = "exited"
    /\ UNCHANGED <<flag, owner>>

(* Owner teardown, step 2: join completes once the worker has exited.        *)
Join ==
    /\ owner = "closing"
    /\ worker = "exited"
    /\ owner' = "joined"
    /\ UNCHANGED <<flag, worker>>

Next ==
    \/ RequestShutdown
    \/ WorkerExit
    \/ Join

Spec ==
    /\ Init
    /\ [][Next]_vars
    /\ WF_vars(RequestShutdown)
    /\ WF_vars(WorkerExit)
    /\ WF_vars(Join)

----------------------------------------------------------------------------

TypeOK ==
    /\ flag \in BOOLEAN
    /\ worker \in {"running", "exited"}
    /\ owner \in {"active", "closing", "joined"}

(* The owner never reports the worker joined while it is still running. *)
NoOrphan == (owner = "joined") => (worker = "exited")

(* Teardown always finishes: no orphaned thread, no join that hangs forever. *)
Termination == (owner = "active") ~> (owner = "joined")

=============================================================================
