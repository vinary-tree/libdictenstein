--------------------------- MODULE HotStuffConsensus ---------------------------
(****************************************************************************)
(* Bounded HotStuff/PBFT-style safety model.                                *)
(*                                                                          *)
(* Scope: replicas vote for finite logs; Byzantine replicas may equivocate. *)
(* Honest replicas only cast votes compatible with their prior votes.       *)
(* A log may commit after a quorum of votes.                                *)
(****************************************************************************)

EXTENDS Naturals, FiniteSets, Sequences, TLC

CONSTANTS Replicas, Byzantine, Commands, MaxLogLen, QuorumSize

VARIABLES votes, committed

Vars == <<votes, committed>>

Honest == Replicas \ Byzantine

LogSet == UNION {[1..len -> Commands] : len \in 0..MaxLogLen}

VoteSet == [replica : Replicas, log : LogSet]

Prefix(left, right) ==
    /\ Len(left) <= Len(right)
    /\ SubSeq(right, 1, Len(left)) = left

Compatible(left, right) ==
    \/ Prefix(left, right)
    \/ Prefix(right, left)

QuorumFor(log) ==
    {replica \in Replicas :
        \E vote \in votes :
          /\ vote.replica = replica
          /\ vote.log = log}

TypeInvariant ==
    /\ Byzantine \subseteq Replicas
    /\ QuorumSize \in Nat
    /\ votes \in SUBSET VoteSet
    /\ MaxLogLen \in Nat
    /\ committed \in SUBSET LogSet

Init ==
    /\ votes = {}
    /\ committed = {}

HonestSafeVote(replica, log) ==
    \/ replica \in Byzantine
    \/ \A prior \in votes :
        prior.replica = replica => Compatible(prior.log, log)

Vote(v) ==
    /\ v \in VoteSet
    /\ v \notin votes
    /\ HonestSafeVote(v.replica, v.log)
    /\ votes' = votes \cup {v}
    /\ UNCHANGED committed

Commit(log) ==
    /\ log \in LogSet
    /\ Cardinality(QuorumFor(log)) >= QuorumSize
    /\ committed' = committed \cup {log}
    /\ UNCHANGED votes

Next ==
    \/ \E v \in VoteSet : Vote(v)
    \/ \E log \in LogSet : Commit(log)

HonestVoteLock ==
    \A replica \in Honest :
      \A left \in votes :
        \A right \in votes :
          /\ left.replica = replica
          /\ right.replica = replica
          => Compatible(left.log, right.log)

CommittedCompatible ==
    \A left \in committed :
      \A right \in committed :
        Compatible(left, right)

CommittedQuorumsIntersectHonestly ==
    \A left \in committed :
      \A right \in committed :
        Cardinality(QuorumFor(left) \cap QuorumFor(right) \cap Honest) >= 1

Spec == Init /\ [][Next]_Vars

=============================================================================
