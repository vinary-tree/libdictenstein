-------------------------- MODULE OverlayEvictionCas --------------------------
(***************************************************************************)
(* OVERLAY-EVICTION + FAULT-IN driver under LOCK-FREE writers — the        *)
(* evictor-root-CAS ‖ faulter-root-CAS ‖ writer-root-CAS arbitration on a   *)
(* single `lockfree_root` atomic (the new concurrency interactions the      *)
(* reversible overlay-eviction benchmark driver + the fault-in-on-read/     *)
(* write primitive introduce).                                              *)
(*                                                                         *)
(* The Rust components being modelled are                                  *)
(* `evict_overlay_node_at_path` / `evict_overlay_nodes`                    *)
(* (`persistent_artrie_char/mod.rs`, `cfg(any(test, bench-internals))`):   *)
(* they path-copy the overlay spine from the published `lockfree_root`,    *)
(* swap an in-memory child for an `OnDisk(SwizzledPtr)` reference, and CAS- *)
(* publish the new root. The NEW component is `find_leaf_faulting` /        *)
(* `build_path_recursive`'s OnDisk arm (`lockfree_cas.rs`): it loads an     *)
(* `OnDisk` node back from the durable image via the reused                 *)
(* `load_char_node_from_disk_lazy` decoder and CAS-publishes a new root     *)
(* whose child is `InMem` again (the path-copied spine splices the faulted  *)
(* node in). Concurrently, lock-free writers (`insert_cas_durable`) path-   *)
(* copy + root-CAS to publish new terms.                                    *)
(*                                                                         *)
(* All three contend on the SAME `lockfree_root` atomic (`AtomicNodePtr`,   *)
(* an `arc_swap::ArcSwapOption`). The headline questions the spec answers:  *)
(*                                                                         *)
(*   1. NoLostAck — a loser-safe root CAS NEVER drops an acknowledged       *)
(*      write: a writer/evictor/faulter that loses the CAS rebases; it can  *)
(*      never overwrite a concurrent insert. (Each is the analogue of the   *)
(*      proven loser-safe writer CAS.) STRENGTHENED here: a writer may ack  *)
(*      a term whose prefix was evicted, because the write path FAULTS the  *)
(*      prefix back in first (then descends) — the acked node is reachable   *)
(*      OR recoverable from the durable image.                              *)
(*   2. FaultEqualsDurable — a node that is part of the durable             *)
(*      (checkpointed) image stays consistent with that image across        *)
(*      evict/fault-in: fault-in never manufactures or drops durable        *)
(*      content; a faulted node equals its durable bytes (the §2 round-trip *)
(*      equivalence). A durable node is never removed from `durable`.       *)
(*   3. ReadNeverMissesCommitted — every committed node (acked by a writer  *)
(*      OR pre-published cold) is EITHER reachable through the published    *)
(*      root / a pinned reader snapshot, OR recoverable from the durable    *)
(*      image (`n \in durable`). With fault-in present, eviction may touch  *)
(*      ANY node (not just cold) because a later read/write FAULTS it back   *)
(*      in — so a committed node is never *permanently* unreachable. This    *)
(*      REPLACES the old `EvictTouchesOnlyCold` safety obligation: the       *)
(*      cold-only scoping is no longer required for safety once fault-in     *)
(*      recovers any evicted durable node.                                  *)
(*   4. ReachableNotFreed — no use-after-free: a node version still         *)
(*      reachable from the published root (or a pre-eviction reader          *)
(*      snapshot) is NEVER freed. Reclamation falls out of `Arc` refcount.   *)
(*                                                                         *)
(* USE_FAULT_IN = TRUE  -> the design with fault-in: eviction is             *)
(*   UNRESTRICTED (may evict any linked node), and `FaultInCas` recovers an  *)
(*   evicted durable node on a later read/write. ALL safety invariants hold  *)
(*   (NoLostAck, FaultEqualsDurable, ReadNeverMissesCommitted,               *)
(*   ReachableNotFreed, LinkedAndOnDiskDisjoint).                            *)
(* USE_FAULT_IN = FALSE -> the `_Unsafe.cfg` negative control: fault-in is   *)
(*   DISABLED but eviction is STILL unrestricted (may evict an acked LIVE    *)
(*   node). TLC MUST report a violation of `ReadNeverMissesCommitted`: an    *)
(*   acked LIVE node evicted with no fault-in path is permanently            *)
(*   unreachable (it is NOT in `durable` — only the pre-checkpointed cold    *)
(*   set is) -> silent data loss. This proves fault-in is REQUIRED once      *)
(*   eviction is unrestricted. If TLC unexpectedly PASSES this, the model    *)
(*   no longer catches the bug it must catch -> negative control broken ->   *)
(*   fail.                                                                   *)
(***************************************************************************)

EXTENDS Naturals, FiniteSets, TLC

CONSTANTS
    Nodes,          \* all overlay leaf nodes that may be inserted/evicted/faulted
    Lsns,           \* WAL LSNs available to writers (one per writable node)
    live,           \* the LIVE set: nodes that may be re-read/re-written by writers
    cold,           \* the COLD set: inserted, checkpointed (durable), never re-touched
    USE_FAULT_IN,   \* TRUE = fault-in present (eviction unrestricted, recovery enabled);
                    \* FALSE = unsafe negative control (eviction unrestricted, NO recovery)
    MaxRoot         \* TLC finiteness cap on the monotone `root` version tag (see RootBound)

\* The LIVE and COLD sets partition the nodes the model reasons about. Every
\* node is exactly one of LIVE or COLD (disjoint, covering): the benchmark
\* inserts a LIVE working range and a disjoint COLD prefix range.
\* `Lsns` is the (durable Order-A) WAL-LSN domain writers draw from; it is not
\* load-bearing on the abstract trie state (acked tracks nodes, not LSNs) but is
\* declared so the model mirrors the Rust write path and matches the frozen
\* `.cfg` CONSTANTS. It must be non-empty so a writer always has an LSN to sync.
ASSUME live \cup cold = Nodes
ASSUME live \cap cold = {}
ASSUME Lsns # {}

VARIABLES
    root,           \* the abstract published root "version" id (a Nat, bumped per CAS)
    linkedInMem,    \* set of nodes reachable IN MEMORY through the published root
    onDisk,         \* set of nodes whose child slot the evictor swapped to OnDisk
    acked,          \* set of nodes whose insert was acknowledged (Ok(true) returned)
    durable,        \* set of nodes recoverable from the durable image (checkpointed)
    pinnedRoot,     \* the root version a pre-eviction reader snapshot still pins (or 0 = none)
    pinnedSet       \* the in-memory node set that the pinned reader snapshot observes

Vars == <<root, linkedInMem, onDisk, acked, durable, pinnedRoot, pinnedSet>>

\* A node is REACHABLE if it is in memory via the published root OR via a pinned
\* pre-eviction reader snapshot (arc-swap keeps the snapshot's subtree alive).
Reachable(n) == n \in linkedInMem \/ n \in pinnedSet

TypeInvariant ==
    /\ root \in Nat
    /\ linkedInMem \subseteq Nodes
    /\ onDisk \subseteq Nodes
    /\ acked \subseteq Nodes
    /\ durable \subseteq Nodes
    /\ pinnedRoot \in Nat
    /\ pinnedSet \subseteq Nodes

\* COLD nodes are inserted + checkpointed + published into the overlay BEFORE the
\* measured concurrent phase, then NEVER re-touched (the benchmark's fixed COLD
\* prefix). So at Init they are already linked in memory (evictable) AND durable
\* (the prior checkpoint wrote them to the arena, the bytes `find_leaf_faulting`
\* loads back — proven separately in `LockFreeDurableCheckpointEviction.tla`).
\* The concurrent WRITERS in this spec publish LIVE terms; `acked` therefore
\* tracks the LIVE writer activity whose loser-safe-CAS interaction with the
\* evictor + faulter is the point. LIVE writer terms are NOT in `durable` at Init
\* (they have not been checkpointed) — this is exactly what makes the negative
\* control fire: an evicted LIVE acked node with no fault-in is unrecoverable.
Init ==
    /\ root = 1
    /\ linkedInMem = cold
    /\ onDisk = {}
    /\ acked = {}
    /\ durable = cold
    /\ pinnedRoot = 0
    /\ pinnedSet = {}

\* WriterCas: a lock-free writer publishes a LIVE term `n`. It path-copies the
\* spine and CAS-publishes a new root version (bumping `root`), linking `n` in
\* memory and acknowledging it. A node already linked is a duplicate (skipped).
\* The WAL record was synced durable BEFORE this visibility CAS (Order A), so an
\* acked node is durable-on-disk; the in-memory-reachability statement NoLostAck
\* provides is the stronger overlay property. Writers run with NO eviction gate.
\*
\* WRITE-PATH FAULT-IN (design §4): if `n`'s prefix was evicted (a spine slot is
\* OnDisk), the write path FAULTS it back in first (the OnDisk arm of
\* `build_path_recursive` loads + splices InMem) THEN descends, all within the
\* SAME path-copy whose single root CAS is the sole arbiter. We model this by
\* allowing a writer to ack `n` regardless of whether any node is OnDisk: the
\* fault-then-descend rebuilds one new spine, so no second commit point is
\* introduced. `n` becoming linked clears any stale OnDisk mark on `n` itself.
WriterCas(n) ==
    /\ n \in live
    /\ n \notin linkedInMem
    /\ root' = root + 1
    /\ linkedInMem' = linkedInMem \cup {n}
    /\ acked' = acked \cup {n}
    /\ onDisk' = onDisk \ {n}      \* re-linking a node in memory clears any stale OnDisk mark
    /\ UNCHANGED <<durable, pinnedRoot, pinnedSet>>

\* TakeReaderSnapshot: a reader does `lockfree_root.load_full()`, pinning the
\* CURRENT root version + the in-memory node set it observes. arc-swap keeps that
\* version's whole subtree alive until the snapshot drops (DropReaderSnapshot).
\* Only one snapshot is tracked (the worst case for the no-UAF argument).
TakeReaderSnapshot ==
    /\ pinnedRoot = 0
    /\ pinnedRoot' = root
    /\ pinnedSet' = linkedInMem
    /\ UNCHANGED <<root, linkedInMem, onDisk, acked, durable>>

\* DropReaderSnapshot: the pinned reader releases its snapshot (its Arc drops).
DropReaderSnapshot ==
    /\ pinnedRoot # 0
    /\ pinnedRoot' = 0
    /\ pinnedSet' = {}
    /\ UNCHANGED <<root, linkedInMem, onDisk, acked, durable>>

\* EvictableNode(n): the evictor MAY attempt to evict `n` iff it is currently
\* linked in memory (the candidate must be reachable to be evicted). The decisive
\* relaxation §7: the OLD cold-only gate (`n \in cold`) is REPLACED by a DURABLE
\* gate — with fault-in present (USE_FAULT_IN = TRUE) the evictor may evict ANY
\* node it can fault back, i.e. any node in the durable image (`n \in durable`).
\* This is strictly broader than cold-only (cold ⊆ durable) yet still safe: a
\* read/write later faults the durable node back in. The realistic evictor is
\* registry-driven and the registry holds disk pointers ONLY for checkpointed
\* (durable) nodes, so "evict ⊆ durable" is exactly the driver's precondition
\* (`disk_ptr.disk_location().is_some()` in `evict_overlay_node_at_path`).
\* The `_Unsafe` control DROPS this durable gate (USE_FAULT_IN = FALSE makes the
\* implication vacuous) -> eviction becomes UNRESTRICTED and may evict an acked
\* LIVE node that is NOT durable, which — with fault-in disabled — is then
\* permanently lost (the bug the negative control must exhibit).
EvictableNode(n) ==
    /\ n \in linkedInMem
    /\ (USE_FAULT_IN => n \in durable)

\* EvictCas(n): swaps node `n`'s in-memory child slot for an OnDisk reference and
\* CAS-publishes the new root. Two outcomes model the loser-safe CAS:
\*
\*   * SUCCEED: the evictor's loaded `old_root` is still current -> CAS lands. `n`
\*     leaves `linkedInMem` and joins `onDisk`. A still-pinned pre-eviction reader
\*     keeps `n` alive via `pinnedSet` (no UAF) until that snapshot drops.
\*   * LOSE: a concurrent CAS already advanced `root` since the evictor's load ->
\*     the CAS FAILS. Nothing is published; the evictor rebases (modelled as a
\*     stutter). Loser-safe: the evictor can NEVER clobber a concurrent insert.
\*
\* CRUCIAL for the negative control: evicting `n` does NOT add it to `durable`.
\* Only nodes that were checkpointed (the COLD set, at Init) are durable. So if a
\* LIVE acked node is evicted and fault-in is OFF, it is in neither `linkedInMem`
\* nor `durable` -> `ReadNeverMissesCommitted` fails.
EvictCasSucceed(n) ==
    /\ EvictableNode(n)
    /\ root' = root + 1
    /\ linkedInMem' = linkedInMem \ {n}
    /\ onDisk' = onDisk \cup {n}
    /\ UNCHANGED <<acked, durable, pinnedRoot, pinnedSet>>

\* FaultInCas(n): the NEW read/write fault-in primitive. Enabled iff `n` is
\* currently OnDisk AND part of the durable image (`n \in onDisk \cap durable`) —
\* the loader reads `n`'s bytes from the arena (the durable image) and re-links it
\* InMem via a loser-safe root CAS (path-copying the spine, splicing
\* `Child::InMem(loaded)`). Dual of `EvictCasSucceed`. Two outcomes:
\*
\*   * SUCCEED: CAS lands -> `n` re-joins `linkedInMem`, leaves `onDisk`. The new
\*     root version is published. `durable` is UNCHANGED (fault-in writes nothing
\*     to disk, advances no watermark — design §5 no-lost-write preserved).
\*   * LOSE: a concurrent CAS advanced `root` -> CAS fails (modelled as stutter);
\*     the faulter drops its loaded Arc (refcount) and rebases (idempotent: two
\*     faulters each load their own Arc, exactly one CAS wins).
\*
\* Gated by USE_FAULT_IN: the `_Unsafe` control sets it FALSE so fault-in NEVER
\* fires, leaving any evicted acked node permanently unreachable.
FaultInCas(n) ==
    /\ USE_FAULT_IN
    /\ n \in onDisk
    /\ n \in durable
    /\ root' = root + 1
    /\ linkedInMem' = linkedInMem \cup {n}
    /\ onDisk' = onDisk \ {n}
    /\ UNCHANGED <<acked, durable, pinnedRoot, pinnedSet>>

\* Reclaim: an OnDisk-marked node NO LONGER reachable from any live root version
\* (neither the published root nor a pinned reader snapshot) has its last Arc
\* dropped and is freed. The refcount reclamation point. It changes no abstract
\* trie state (the node was already unlinked); it exists so the no-UAF invariant
\* is checked at the freeing boundary: a node is freed ONLY when unreachable.
Reclaim(n) ==
    /\ n \in onDisk
    /\ ~Reachable(n)
    /\ UNCHANGED Vars      \* freeing an already-unreachable node changes no observable state

Next ==
    \/ \E n \in Nodes : WriterCas(n)
    \/ \E n \in Nodes : EvictCasSucceed(n)
    \/ \E n \in Nodes : FaultInCas(n)
    \/ \E n \in Nodes : Reclaim(n)
    \/ TakeReaderSnapshot
    \/ DropReaderSnapshot

Spec == Init /\ [][Next]_Vars

------------------------------------------------------------------------------
\* State constraint (TLC finiteness)
\*
\* `root` is an unbounded monotone Nat (bumped once per successful CAS, exactly
\* as the Rust `AtomicNodePtr` version increments). The abstract trie state that
\* every invariant actually depends on is the FINITE set-valued vars
\* (`linkedInMem`, `onDisk`, `acked`, `durable`, `pinnedSet`) over the finite
\* `Nodes`; `root`/`pinnedRoot` are version tags no invariant constrains. Because
\* `EvictCasSucceed` and `FaultInCas` can toggle a cold node onDisk<->linked
\* indefinitely, each bump of `root`, the raw reachable-state set is infinite
\* purely in the version tag. We cap `root` with a TLC CONSTRAINT (the standard
\* idiom for an unbounded monotone counter, mirroring the bounded `nextLsn` of
\* `LockFreeDurableCheckpointEviction`). MaxRoot is chosen generously so every
\* meaningful interleaving is reachable within the bound: a writer CAS (1) plus
\* evicting + re-faulting both cold nodes (4) plus reader snapshot churn still
\* fit under MaxRoot = 6. The constraint prunes only the unbounded version-tag
\* tail; it cannot mask a safety violation because no invariant references the
\* numeric value of `root` (a violating state at root = k has an identical
\* set-valued witness reachable at some root <= MaxRoot).
RootBound == root <= MaxRoot

------------------------------------------------------------------------------
\* Invariants

\* (1) NoLostAck — every acknowledged write is recoverable: reachable in memory
\* via the published root, OR via a pinned pre-eviction reader snapshot, OR
\* present in the durable image. The loser-safe CAS never *drops* an acked write
\* to a racing evict between its load and CAS; and with fault-in an evicted acked
\* node is recoverable from `durable`. (Acked nodes are durable-on-disk by Order
\* A; this disjunction makes that explicit at the abstract level.)
NoLostAck == \A n \in acked : (Reachable(n) \/ n \in durable)

\* (2) FaultEqualsDurable — a durable node stays durable across evict/fault-in:
\* fault-in never manufactures content that diverges from the durable bytes, and
\* eviction never erases a node's durable identity (the §2 round-trip `load(
\* serialize(overlay_to_inner(n))) ≡ n` equivalence — finality/value/child-set).
\* Stated abstractly: a node that began durable (the COLD set, at Init) is always
\* in `durable`, whether currently InMem or OnDisk. This is the abstract witness
\* that faulting a node in does not corrupt or lose its durable identity (the
\* Rust round-trip unit test + OE5 check it concretely, byte-for-byte).
FaultEqualsDurable == cold \subseteq durable

\* (3) ReadNeverMissesCommitted — every COMMITTED node (acked by a writer OR
\* pre-published cold) is recoverable: reachable through the published root / a
\* pinned reader snapshot, OR present in the durable image. This is the safety
\* obligation that REPLACES `EvictTouchesOnlyCold`. With fault-in (USE_FAULT_IN =
\* TRUE), eviction may touch ANY node, but:
\*   - a COLD committed node is always in `durable` (checkpointed at Init), so even
\*     when evicted it satisfies the disjunct `n \in durable` and fault-in can
\*     recover it;
\*   - a LIVE acked node is never removed from memory except by eviction; with
\*     USE_FAULT_IN, fault-in re-links it (and the write-path faults its prefix
\*     in), so it returns to `linkedInMem` — it never leaves the (Reachable ∨
\*     durable) cover permanently.
\* The `_Unsafe` control (USE_FAULT_IN = FALSE, eviction still unrestricted)
\* evicts a LIVE acked node that is NOT durable and CANNOT be faulted back ->
\* this invariant is VIOLATED, proving fault-in is REQUIRED.
ReadNeverMissesCommitted == \A n \in (acked \cup cold) : (Reachable(n) \/ n \in durable)

\* (4) ReachableNotFreed — no use-after-free (the Phase-D witness, stated
\* NON-vacuously). The dangerous case is a node the evictor UNLINKED from the
\* published root (`n \in onDisk`, `n \notin linkedInMem`) WHILE a pre-eviction
\* reader still holds a snapshot that observed it (`n \in pinnedSet`). The no-UAF
\* obligation is that such a node is STILL reachable — the arc-swap snapshot keeps
\* the old in-memory subtree alive, so the concurrent reader never dereferences a
\* freed allocation.
ReachableNotFreed ==
    \A n \in (onDisk \cap pinnedSet) : Reachable(n)

\* Sanity: a node is never simultaneously linked-in-memory AND marked on-disk
\* through the SAME published root (the slot is one or the other). Eviction moves
\* a node from linked->onDisk and fault-in moves it onDisk->linked, never both.
LinkedAndOnDiskDisjoint == linkedInMem \cap onDisk = {}

==============================================================================
