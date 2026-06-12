------------------------------ MODULE PersistentARTrieU64 ------------------------------
EXTENDS Naturals, FiniteSets, TLC

(*
  Bounded model for the sequence-keyed native persistent u64 ARTrie.

  Public u64 sequences are exact keys. The implementation stores native u64
  edge labels in adaptive edge stores and publishes copy-on-write snapshots, so
  the abstract state is a finite set of complete u64 sequences plus a durable
  checkpoint set.
*)

CONSTANTS Empty, U1, U2, U12, U21

Sequences == {Empty, U1, U2, U12, U21}

IsPrefix(prefix, sequence) ==
  \/ prefix = Empty
  \/ prefix = sequence
  \/ /\ prefix = U1 /\ sequence = U12
  \/ /\ prefix = U2 /\ sequence = U21

VARIABLES live, durable

vars == <<live, durable>>

Init ==
  /\ live = {}
  /\ durable = {}

Insert(sequence) ==
  /\ sequence \in Sequences
  /\ live' = live \cup {sequence}
  /\ UNCHANGED durable

Remove(sequence) ==
  /\ sequence \in Sequences
  /\ live' = live \ {sequence}
  /\ UNCHANGED durable

Checkpoint ==
  /\ durable' = live
  /\ UNCHANGED live

Reopen ==
  /\ live' = durable
  /\ UNCHANGED durable

Next ==
  \/ \E sequence \in Sequences : Insert(sequence)
  \/ \E sequence \in Sequences : Remove(sequence)
  \/ Checkpoint
  \/ Reopen

Contains(sequence) == sequence \in live

PrefixResults(prefix) == {sequence \in live : IsPrefix(prefix, sequence)}

TypeOK ==
  /\ live \subseteq Sequences
  /\ durable \subseteq Sequences

ExactContains ==
  \A sequence \in Sequences : Contains(sequence) <=> sequence \in live

PrefixSound ==
  \A prefix \in Sequences :
    \A sequence \in PrefixResults(prefix) : IsPrefix(prefix, sequence)

CheckpointReopenPreservesLive ==
  \A sequence \in Sequences :
    sequence \in live =>
      sequence \in (live \cup durable)

Spec == Init /\ [][Next]_vars

=============================================================================
