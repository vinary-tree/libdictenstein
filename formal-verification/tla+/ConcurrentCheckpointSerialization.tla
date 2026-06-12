-------------------- MODULE ConcurrentCheckpointSerialization --------------------
(***************************************************************************)
(* NF-3 (Phase F / F3): serializing concurrent checkpoints with the          *)
(* `checkpoint_lock`.                                                          *)
(*                                                                            *)
(* The non-blocking overlay checkpoint reads the atomic root WITHOUT a write   *)
(* lock — char's overlay arm holds only `self.read()`                         *)
(* (`persistent_artrie/char/mod.rs` checkpoint trait impl). So two concurrent  *)
(* `checkpoint()` calls on a shared handle run TOGETHER. Each publishes a      *)
(* block-0 descriptor whose fields (root_ptr, arena_count, entry_count) plus   *)
(* the sequentially-allocated arena slots MUST all come from the SAME captured *)
(* snapshot — one checkpoint "generation". If two checkpoints interleave their *)
(* descriptor writes, the persisted descriptor mixes fields from different     *)
(* generations: a TORN descriptor → terms lost / corrupt on reopen (the NF-3   *)
(* data-loss the red-team surfaced — reachable in production today for an       *)
(* eligible-`V` overlay-flipped trie).                                         *)
(*                                                                            *)
(* The fix (F3): a dedicated `checkpoint_lock: Mutex<()>` serializes the       *)
(* descriptor-writing critical section. Readers/writers NEVER touch it, so the *)
(* overlay stays lock-free; only two CHECKPOINTS cannot interleave.            *)
(*                                                                            *)
(* USE_LOCK = TRUE  → the checkpoint_lock design: `NoTornDescriptor` and       *)
(*   `MutualExclusion` HOLD (no error).                                        *)
(* USE_LOCK = FALSE → the bug (no lock): `NoTornDescriptor` is VIOLATED — TLC  *)
(*   produces the torn-descriptor trace (the negative control fires, proving   *)
(*   the model genuinely exhibits the race the lock must prevent).             *)
(*                                                                            *)
(* The 3-field descriptor is the minimal faithful model of "a set of values    *)
(* that must be published atomically together"; the arena-slot allocation race *)
(* (two checkpoints claiming overlapping ranges) is the SAME interleaved-write *)
(* pattern, captured by the same invariant.                                    *)
(***************************************************************************)

EXTENDS Naturals, FiniteSets, TLC

CONSTANTS Checkpointers, USE_LOCK

\* The descriptor fields that must be published atomically together (root_ptr,
\* arena_count, entry_count). A checkpoint writes them one at a time.
Fields == 1..3

VARIABLES
    phase,       \* checkpointer -> "Idle" | "Writing" | "Done"
    step,        \* checkpointer -> next descriptor field to write (1..4; 4 = all done)
    lockHolder,  \* the checkpointer holding checkpoint_lock, or 0 (none held)
    desc         \* field -> the generation (= checkpointer id) that last wrote it
                 \*          (0 = initial / never written)

Vars == <<phase, step, lockHolder, desc>>

\* Each checkpointer's "generation" is its own id (each checkpoint captures a
\* distinct snapshot; modeled as the checkpointer id, distinct per process).

Init ==
    /\ phase = [c \in Checkpointers |-> "Idle"]
    /\ step = [c \in Checkpointers |-> 1]
    /\ lockHolder = 0
    /\ desc = [f \in Fields |-> 0]

\* Begin a checkpoint's descriptor write. Under USE_LOCK the `checkpoint_lock`
\* must be free (mutual exclusion); without it, begin freely (the bug).
Begin(c) ==
    /\ phase[c] = "Idle"
    /\ (USE_LOCK => lockHolder = 0)
    /\ lockHolder' = IF USE_LOCK THEN c ELSE lockHolder
    /\ phase' = [phase EXCEPT ![c] = "Writing"]
    /\ step' = [step EXCEPT ![c] = 1]
    /\ UNCHANGED desc

\* Write the next descriptor field, tagging it with c's generation. Without the
\* lock, another checkpointer's WriteField can interleave between these steps.
WriteField(c) ==
    /\ phase[c] = "Writing"
    /\ step[c] \in Fields
    /\ desc' = [desc EXCEPT ![step[c]] = c]
    /\ step' = [step EXCEPT ![c] = step[c] + 1]
    /\ UNCHANGED <<phase, lockHolder>>

\* All fields written: publish complete, release the checkpoint_lock.
Finish(c) ==
    /\ phase[c] = "Writing"
    /\ step[c] = 4
    /\ phase' = [phase EXCEPT ![c] = "Done"]
    /\ lockHolder' = IF USE_LOCK /\ lockHolder = c THEN 0 ELSE lockHolder
    /\ UNCHANGED <<step, desc>>

Next == \E c \in Checkpointers : Begin(c) \/ WriteField(c) \/ Finish(c)

Spec == Init /\ [][Next]_Vars

\* No checkpoint is mid-write.
Quiescent == \A c \in Checkpointers : phase[c] # "Writing"

\* SAFETY (the headline): whenever no checkpoint is mid-write, the persisted
\* descriptor's fields ALL come from the same generation — no torn descriptor.
\* (The all-0 initial state is trivially consistent.)
NoTornDescriptor ==
    Quiescent => (\A i, j \in Fields : desc[i] = desc[j])

\* The checkpoint_lock's mutual exclusion (USE_LOCK only): at most one checkpoint
\* is writing the descriptor at a time.
MutualExclusion ==
    USE_LOCK => Cardinality({c \in Checkpointers : phase[c] = "Writing"}) <= 1

\* Type-correctness.
TypeOK ==
    /\ phase \in [Checkpointers -> {"Idle", "Writing", "Done"}]
    /\ step \in [Checkpointers -> 1..4]
    /\ lockHolder \in (Checkpointers \cup {0})
    /\ desc \in [Fields -> (Checkpointers \cup {0})]
=============================================================================
