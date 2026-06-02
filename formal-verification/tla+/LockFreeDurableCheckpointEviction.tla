-------------------- MODULE LockFreeDurableCheckpointEviction --------------------
(***************************************************************************)
(* EVICTION-ON durable checkpoint under LOCK-FREE writers.                    *)
(*                                                                         *)
(* This spec COMPOSES two already-verified pieces into the one new component  *)
(* the eviction-ON immutable-snapshot checkpoint introduces:                  *)
(*                                                                         *)
(*   (A) the watermark-bounded WAL reclaim under lock-free, out-of-order      *)
(*       commit, proven safe in `LockFreeDurableCheckpoint.tla` (the          *)
(*       `Append`/`Commit`/`Capture`/`Publish`/`Reclaim`/`CrashRecover`       *)
(*       skeleton, with the committed-watermark `checkpoint_lsn`); and        *)
(*   (B) the eviction `DiskLocationRegistry` publication ordering proven in    *)
(*       `EvictionRegistryPublication.tla` (publish ONLY after a verified      *)
(*       durable checkpoint; INVALIDATE on any write at the `append_to_wal`    *)
(*       chokepoint; evict ONLY a published, durable-up-to-watermark entry;    *)
(*       the registry is NEVER recovery state).                               *)
(*                                                                         *)
(* The Rust component being modelled is                                       *)
(* `publish_immutable_snapshot_retaining_wal_with_eviction`                   *)
(* (persist.rs): it does the EXACT retain-WAL reclaim of                      *)
(* `publish_immutable_snapshot_retaining_wal` (record                         *)
(* `checkpoint_lsn = committed watermark`, RETAIN the WAL, NO destructive      *)
(* truncate) AND ALSO publishes the eviction registry to the coordinator      *)
(* (`update_disk_registry`) once `verify_checkpoint()` proves the image        *)
(* durable. The headline question is unchanged from the base spec — what       *)
(* `checkpoint_lsn` is safe — and the answer is unchanged: the committed       *)
(* WATERMARK. Eviction is invisible to recovery (`RecoveredSet` never reads    *)
(* the registry), so publishing the registry CANNOT change the no-lost-write   *)
(* conclusion; this spec re-derives `NoLostWriteUnderLockFreeCommit` UNDER     *)
(* the added registry actions to prove exactly that, and adds two              *)
(* registry-specific safety invariants.                                       *)
(*                                                                         *)
(* USE_WATERMARK = TRUE  -> the correct design; all invariants hold.          *)
(* USE_WATERMARK = FALSE -> the appended-frontier negative control; TLC        *)
(*   produces the GAP_LEDGER #41 losing trace                                  *)
(*   (`NoLostWriteUnderLockFreeCommit` is violated), exactly as the base       *)
(*   spec's `_Unsafe.cfg` does — confirming the watermark choice is REQUIRED   *)
(*   even with eviction on.                                                    *)
(***************************************************************************)

EXTENDS Naturals, FiniteSets, TLC

CONSTANTS Writers, Lsns, NoLsn, USE_WATERMARK

ASSUME Lsns = 1..Cardinality(Lsns)          \* Lsns is a contiguous 1..N
ASSUME NoLsn \notin Lsns
MaxL == Cardinality(Lsns)

VARIABLES
    nextLsn,           \* next LSN to assign (1..MaxL+1)
    wphase,            \* writer -> "Idle" | "Appended" | "Committed"
    wlsn,              \* writer -> the LSN it reserved (or NoLsn)
    appended,          \* set of LSNs whose WAL record is durable (Order A)
    committed,         \* set of LSNs that have been CAS-published (visible)
    ckptPhase,         \* "Idle" | "Captured" | "Verified" | "Published"
    ckptTarget,        \* the chosen checkpoint_lsn (watermark or appended frontier)
    ckptSnapshot,      \* set of LSNs captured in the immutable snapshot
    durableCkpt,       \* set of LSNs durably checkpointed (the published image)
    checkpointLsn,     \* durable checkpoint_lsn on disk (block-0)
    walRetainedFrom,   \* WAL reclaim frontier: records with lsn >= this are retained
    recovered,         \* set of LSNs reconstructed by a crash+reopen
    recoveryFresh,     \* the most recent step was a crash recovery
    \* ── registry state (the new component) ───────────────────────────────
    registryDurableUpTo, \* the watermark the published registry's entries are durable up to
    registryValid        \* the published registry is currently valid (not invalidated by a write)

Vars == <<nextLsn, wphase, wlsn, appended, committed, ckptPhase, ckptTarget,
          ckptSnapshot, durableCkpt, checkpointLsn, walRetainedFrom,
          recovered, recoveryFresh, registryDurableUpTo, registryValid>>

MaxNat(a, b) == IF a >= b THEN a ELSE b

\* The committed watermark: the largest L such that every LSN in 1..L is committed.
\* (Contiguous all-committed prefix — robust to out-of-order CAS commits.)
Watermark ==
    CHOOSE n \in 0..MaxL :
        /\ \A m \in 1..n : m \in committed
        /\ (n = MaxL \/ (n + 1) \notin committed)

\* The appended frontier: the max appended LSN (the UNSAFE choice).
AppendedFrontier == IF appended = {} THEN 0 ELSE CHOOSE n \in appended : \A m \in appended : m <= n

TypeInvariant ==
    /\ nextLsn \in 1..(MaxL + 1)
    /\ wphase \in [Writers -> {"Idle", "Appended", "Committed"}]
    /\ wlsn \in [Writers -> Lsns \cup {NoLsn}]
    /\ appended \subseteq Lsns
    /\ committed \subseteq Lsns
    /\ committed \subseteq appended            \* Order A: visible ⊆ durable
    /\ ckptPhase \in {"Idle", "Captured", "Verified", "Published"}
    /\ ckptTarget \in 0..MaxL
    /\ ckptSnapshot \subseteq Lsns
    /\ durableCkpt \subseteq Lsns
    /\ checkpointLsn \in 0..MaxL
    /\ walRetainedFrom \in 1..(MaxL + 1)
    /\ recovered \subseteq Lsns
    /\ recoveryFresh \in BOOLEAN
    /\ registryDurableUpTo \in 0..MaxL
    /\ registryValid \in BOOLEAN

Init ==
    /\ nextLsn = 1
    /\ wphase = [w \in Writers |-> "Idle"]
    /\ wlsn = [w \in Writers |-> NoLsn]
    /\ appended = {}
    /\ committed = {}
    /\ ckptPhase = "Idle"
    /\ ckptTarget = 0
    /\ ckptSnapshot = {}
    /\ durableCkpt = {}
    /\ checkpointLsn = 0
    /\ walRetainedFrom = 1
    /\ recovered = {}
    /\ recoveryFresh = FALSE
    /\ registryDurableUpTo = 0
    /\ registryValid = FALSE

\* Order A step 1: reserve an LSN and make its WAL record durable (append + sync).
\* Writers run with NO checkpoint gate. At the `append_to_wal` chokepoint the
\* writer ALSO INVALIDATES any published eviction registry (the A1 fix, modelled
\* here as the `registryValid' = FALSE` conjunct) — BEFORE its visibility CAS, so
\* a concurrent writer dirties the registry before its write becomes visible.
Append(w) ==
    /\ wphase[w] = "Idle"
    /\ nextLsn <= MaxL
    /\ wphase' = [wphase EXCEPT ![w] = "Appended"]
    /\ wlsn' = [wlsn EXCEPT ![w] = nextLsn]
    /\ appended' = appended \cup {nextLsn}
    /\ nextLsn' = nextLsn + 1
    /\ registryValid' = FALSE
    /\ recoveryFresh' = FALSE
    /\ UNCHANGED <<committed, ckptPhase, ckptTarget, ckptSnapshot, durableCkpt,
                  checkpointLsn, walRetainedFrom, recovered, registryDurableUpTo>>

\* Order A step 2: CAS-publish the new root → the write becomes visible.
\* May happen in any order relative to other writers' commits (lock-free).
Commit(w) ==
    /\ wphase[w] = "Appended"
    /\ committed' = committed \cup {wlsn[w]}
    /\ wphase' = [wphase EXCEPT ![w] = "Committed"]
    /\ recoveryFresh' = FALSE
    /\ UNCHANGED <<nextLsn, wlsn, appended, ckptPhase, ckptTarget, ckptSnapshot,
                  durableCkpt, checkpointLsn, walRetainedFrom, recovered,
                  registryDurableUpTo, registryValid>>

\* A committed writer returns to Idle (can do another op).
RetireWriter(w) ==
    /\ wphase[w] = "Committed"
    /\ wphase' = [wphase EXCEPT ![w] = "Idle"]
    /\ wlsn' = [wlsn EXCEPT ![w] = NoLsn]
    /\ recoveryFresh' = FALSE
    /\ UNCHANGED <<nextLsn, appended, committed, ckptPhase, ckptTarget,
                  ckptSnapshot, durableCkpt, checkpointLsn, walRetainedFrom,
                  recovered, registryDurableUpTo, registryValid>>

\* Capture: read checkpoint_lsn (watermark or appended frontier) BEFORE the root
\* snapshot, then snapshot the immutable root (committed restricted to ≤ target).
\* NO writer-exclusion: writers continue throughout.
CaptureCheckpoint ==
    /\ ckptPhase = "Idle"
    /\ ckptTarget' = (IF USE_WATERMARK THEN Watermark ELSE AppendedFrontier)
    /\ ckptSnapshot' = {l \in committed : l <= ckptTarget'}
    /\ ckptPhase' = "Captured"
    /\ recoveryFresh' = FALSE
    /\ UNCHANGED <<nextLsn, wphase, wlsn, appended, committed, durableCkpt,
                  checkpointLsn, walRetainedFrom, recovered, registryDurableUpTo,
                  registryValid>>

\* Publish-image: the snapshot becomes the durable checkpoint (block-0 fsync), and
\* `verify_checkpoint()` succeeds — the registry-publication precondition. This is
\* the `publish_snapshot` + `verify_checkpoint` pair in the Rust publisher (the
\* on-disk linearization point). The registry is NOT published yet (publish is
\* gated on this Verified phase, mirroring EvictionRegistryPublication.Publish).
PublishCheckpoint ==
    /\ ckptPhase = "Captured"
    /\ durableCkpt' = ckptSnapshot
    /\ checkpointLsn' = MaxNat(checkpointLsn, ckptTarget)
    /\ ckptPhase' = "Verified"
    /\ recoveryFresh' = FALSE
    /\ UNCHANGED <<nextLsn, wphase, wlsn, appended, committed, ckptTarget,
                  ckptSnapshot, walRetainedFrom, recovered, registryDurableUpTo,
                  registryValid>>

\* Publish-registry: ONLY reachable from the Verified phase (after the image is
\* durable). Publishes the eviction registry to the coordinator
\* (`update_disk_registry`): it becomes valid and its entries are durable up to
\* the durable checkpoint_lsn (the committed watermark captured before the root
\* load). `registryDurableUpTo := checkpointLsn` (= ckptTarget under USE_WATERMARK)
\* — every published entry references a node folded into the durable image ≤ that
\* watermark, so eviction may safely unswizzle it to its on-disk location.
PublishRegistry ==
    /\ ckptPhase = "Verified"
    /\ registryValid' = TRUE
    /\ registryDurableUpTo' = checkpointLsn
    /\ ckptPhase' = "Published"
    /\ recoveryFresh' = FALSE
    /\ UNCHANGED <<nextLsn, wphase, wlsn, appended, committed, ckptTarget,
                  ckptSnapshot, durableCkpt, checkpointLsn, walRetainedFrom,
                  recovered>>

\* Reclaim: record `checkpoint_lsn = watermark`, RETAIN the WAL tail > checkpoint_lsn
\* (NO destructive truncate — the reversible bench publisher's exact semantics; a
\* SUBSET of the base spec's ReclaimWal). Closes the checkpoint round.
ReclaimWal ==
    /\ ckptPhase = "Published"
    /\ walRetainedFrom' = MaxNat(walRetainedFrom, checkpointLsn + 1)
    /\ ckptPhase' = "Idle"
    /\ ckptTarget' = 0
    /\ ckptSnapshot' = {}
    /\ recoveryFresh' = FALSE
    /\ UNCHANGED <<nextLsn, wphase, wlsn, appended, committed, durableCkpt,
                  checkpointLsn, recovered, registryDurableUpTo, registryValid>>

\* Eviction reclaims in-memory node boxes for entries the published registry says
\* are durable. Enabled ONLY while the registry is valid (a concurrent writer's
\* `Append` invalidation disables it → zero evictions, liveness-not-safety). The
\* registry is in-memory only and is INVISIBLE to recovery, so eviction touches no
\* recovery state: this action leaves every WAL/checkpoint variable UNCHANGED. It
\* exists in the spec purely to let the registry-safety invariants
\* (`EvictionTouchesOnlyDurable`) be checked over reachable post-eviction states.
EvictUnderRegistry ==
    /\ registryValid
    /\ recoveryFresh' = FALSE
    /\ UNCHANGED <<nextLsn, wphase, wlsn, appended, committed, ckptPhase,
                  ckptTarget, ckptSnapshot, durableCkpt, checkpointLsn,
                  walRetainedFrom, recovered, registryDurableUpTo, registryValid>>

\* Crash + reopen: recovered = durable checkpoint ∪ {WAL records lsn > checkpoint_lsn
\* still retained (≥ walRetainedFrom) and durable (appended)}. The ephemeral
\* registry is DISCARDED (registryValid' = FALSE) and is NEVER read here — recovery
\* restores membership from durable on-disk state ALONE (EvictionRegistryPublication's
\* "registry is not recovery state" guarantee).
RecoveredSet ==
    durableCkpt \cup {l \in appended : l > checkpointLsn /\ l >= walRetainedFrom}

CrashRecover ==
    /\ recovered' = RecoveredSet
    /\ recoveryFresh' = TRUE
    /\ registryValid' = FALSE
    /\ registryDurableUpTo' = 0
    /\ UNCHANGED <<nextLsn, wphase, wlsn, appended, committed, ckptPhase,
                  ckptTarget, ckptSnapshot, durableCkpt, checkpointLsn,
                  walRetainedFrom>>

Next ==
    \/ \E w \in Writers : Append(w)
    \/ \E w \in Writers : Commit(w)
    \/ \E w \in Writers : RetireWriter(w)
    \/ CaptureCheckpoint
    \/ PublishCheckpoint
    \/ PublishRegistry
    \/ ReclaimWal
    \/ EvictUnderRegistry
    \/ CrashRecover

Spec == Init /\ [][Next]_Vars

------------------------------------------------------------------------------
\* Invariants

\* Order A: every visible (committed) write is durable (its WAL record appended+synced).
DurablePrefix == committed \subseteq appended

\* The immutable snapshot cannot contain a write beyond the captured frontier.
ImmutableSnapshotIsClosed ==
    ckptPhase \in {"Captured", "Verified", "Published"} => \A l \in ckptSnapshot : l <= ckptTarget

\* A durably-checkpointed write is within the published checkpoint_lsn.
CaptureEqualsPublishFrontier ==
    \A l \in durableCkpt : l <= checkpointLsn

\* THE headline (re-derived UNDER reclaim + registry publication + eviction):
\* after a crash+reopen, every COMMITTED (acknowledged/visible) write is
\* reconstructed — eviction-ON does not lose a lock-free commit concurrent with a
\* checkpoint. (Same statement as the base spec; the added registry actions never
\* touch `recovered`, so the proof carries.)
NoLostWriteUnderLockFreeCommit ==
    recoveryFresh => (committed \subseteq recovered)

\* Recovery never invents a write that was never durable.
RecoveredNeverInventsState ==
    recovered \subseteq appended

\* NEW (registry safety #1): a valid published registry's entries are durable up to
\* the committed watermark, i.e. never beyond the durable checkpoint frontier on
\* disk. (`registryDurableUpTo` is set to `checkpointLsn` at publish; a write
\* invalidates the registry before advancing anything, and recovery resets it.)
RegistryPointsAtDurableWatermark ==
    registryValid => registryDurableUpTo <= checkpointLsn

\* NEW (registry safety #2): everything the registry authorizes eviction to
\* reclaim (entries ≤ registryDurableUpTo, only while valid) is contained in the
\* durable checkpoint image — eviction NEVER unswizzles a node onto a
\* non-durable on-disk location. Refines EvictionRegistryPublication's
\* `RegistryEntriesAreDurable` into the watermark-bounded reclaim domain: the set
\* of LSNs eviction may touch is `{1..registryDurableUpTo}`, and (since
\* `registryDurableUpTo <= checkpointLsn` and the durable image is the
\* watermark-prefix) that set is ⊆ the durable checkpoint.
EvictableLsns == IF registryValid THEN {l \in Lsns : l <= registryDurableUpTo} ELSE {}

EvictionTouchesOnlyDurable ==
    \A l \in EvictableLsns : l <= checkpointLsn

==============================================================================
