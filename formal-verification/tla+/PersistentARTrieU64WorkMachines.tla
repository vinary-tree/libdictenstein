--------------------- MODULE PersistentARTrieU64WorkMachines ---------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC

(*
  Bounded refinement model for the two specialized explicit machines used by
  the native-u64 persistent ARTrie.

  Mutation records the matched parents as an explicit sequence, transforms a
  terminal candidate, unwinds in reverse input order, and publishes only the
  fully reconstructed candidate. Disk reopen first validates an explicit
  adjacency graph with (node, next-child) frames and tri-color state. Only after
  structural validation does it consume postorder and install memoized edges.

  Depth bounds TLC state exploration only. The Rust implementation has no depth
  cap and performs one loop transition per modeled step.

  GraphKind is one of:
    Chain      - a linear acyclic graph;
    Diamond    - two parents share one completed child;
    LateCycle  - a valid sibling completes before a later branch reveals 4 -> 2.

  RejectBackEdge = FALSE is the deliberate negative control. It records a
  visiting edge as already installed, modeling the unsafe implementation that
  creates an Arc cycle rather than returning Corrupted.
*)

CONSTANTS Depth, GraphKind, RejectBackEdge

ASSUME /\ Depth \in Nat \ {0}
       /\ GraphKind \in {"Chain", "Diamond", "LateCycle"}
       /\ RejectBackEdge \in BOOLEAN
       /\ (GraphKind = "Chain" \/ Depth = 4)

Nodes == 0..Depth
Root == 0
Units == 1..Depth
MutationKey == [i \in 1..Depth |-> i]

Children(node) ==
  CASE GraphKind = "Chain" ->
         IF node < Depth THEN <<node + 1>> ELSE <<>>
    [] GraphKind = "Diamond" ->
         CASE node = 0 -> <<1, 2>>
           [] node = 1 -> <<3>>
           [] node = 2 -> <<3>>
           [] node = 3 -> <<4>>
           [] OTHER -> <<>>
    [] OTHER ->
         CASE node = 0 -> <<1, 2>>
           [] node = 1 -> <<3>>
           [] node = 2 -> <<4>>
           [] node = 4 -> <<2>>
           [] OTHER -> <<>>

SeqSet(sequence) == {sequence[i] : i \in 1..Len(sequence)}
FrameNodes(sequence) == {sequence[i].node : i \in 1..Len(sequence)}
GraphEdges == UNION {{node} \X SeqSet(Children(node)) : node \in Nodes}
CyclicGraph == GraphKind = "LateCycle"

VARIABLES
  mutationPhase,
  keyCursor,
  mutationFrames,
  candidate,
  published,
  publishedKey,
  diskPhase,
  diskFrames,
  color,
  preorder,
  postorder,
  constructCursor,
  constructed,
  installedEdges,
  corrupted,
  cycleSeen

vars == <<
  mutationPhase,
  keyCursor,
  mutationFrames,
  candidate,
  published,
  publishedKey,
  diskPhase,
  diskFrames,
  color,
  preorder,
  postorder,
  constructCursor,
  constructed,
  installedEdges,
  corrupted,
  cycleSeen
>>

MutationVars == <<
  mutationPhase,
  keyCursor,
  mutationFrames,
  candidate,
  published,
  publishedKey
>>

DiskVars == <<
  diskPhase,
  diskFrames,
  color,
  preorder,
  postorder,
  constructCursor,
  constructed,
  installedEdges,
  corrupted,
  cycleSeen
>>

Init ==
  /\ mutationPhase = "Descend"
  /\ keyCursor = 0
  /\ mutationFrames = <<>>
  /\ candidate = <<>>
  /\ published = FALSE
  /\ publishedKey = <<>>
  /\ diskPhase = "Validate"
  /\ diskFrames = <<[node |-> Root, next |-> 1]>>
  /\ color = [node \in Nodes |-> IF node = Root THEN "Visiting" ELSE "Unseen"]
  /\ preorder = <<Root>>
  /\ postorder = <<>>
  /\ constructCursor = 1
  /\ constructed = {}
  /\ installedEdges = {}
  /\ corrupted = FALSE
  /\ cycleSeen = FALSE

MutationDescend ==
  /\ mutationPhase = "Descend"
  /\ keyCursor < Len(MutationKey)
  /\ keyCursor' = keyCursor + 1
  /\ mutationFrames' = Append(mutationFrames, MutationKey[keyCursor + 1])
  /\ UNCHANGED <<mutationPhase, candidate, published, publishedKey>>

MutationBeginUnwind ==
  /\ mutationPhase = "Descend"
  /\ keyCursor = Len(MutationKey)
  /\ mutationPhase' = "Unwind"
  /\ candidate' = <<>>
  /\ UNCHANGED <<keyCursor, mutationFrames, published, publishedKey>>

MutationUnwind ==
  /\ mutationPhase = "Unwind"
  /\ Len(mutationFrames) > 0
  /\ LET unit == mutationFrames[Len(mutationFrames)]
     IN /\ mutationFrames' = SubSeq(mutationFrames, 1, Len(mutationFrames) - 1)
        /\ candidate' = <<unit>> \o candidate
  /\ UNCHANGED <<mutationPhase, keyCursor, published, publishedKey>>

MutationReadyToPublish ==
  /\ mutationPhase = "Unwind"
  /\ mutationFrames = <<>>
  /\ mutationPhase' = "CAS"
  /\ UNCHANGED <<keyCursor, mutationFrames, candidate, published, publishedKey>>

MutationPublish ==
  /\ mutationPhase = "CAS"
  /\ mutationPhase' = "Done"
  /\ published' = TRUE
  /\ publishedKey' = candidate
  /\ UNCHANGED <<keyCursor, mutationFrames, candidate>>

MutationStep ==
  \/ MutationDescend
  \/ MutationBeginUnwind
  \/ MutationUnwind
  \/ MutationReadyToPublish
  \/ MutationPublish

ValidateUnseenChild ==
  /\ diskPhase = "Validate"
  /\ Len(diskFrames) > 0
  /\ LET frame == diskFrames[Len(diskFrames)]
         children == Children(frame.node)
     IN /\ frame.next <= Len(children)
        /\ LET child == children[frame.next]
               advanced == [diskFrames EXCEPT ![Len(diskFrames)].next = @ + 1]
           IN /\ color[child] = "Unseen"
              /\ diskFrames' = Append(advanced, [node |-> child, next |-> 1])
              /\ color' = [color EXCEPT ![child] = "Visiting"]
              /\ preorder' = Append(preorder, child)
  /\ UNCHANGED <<diskPhase, postorder, constructCursor, constructed,
                  installedEdges, corrupted, cycleSeen>>

ValidateDoneChild ==
  /\ diskPhase = "Validate"
  /\ Len(diskFrames) > 0
  /\ LET frame == diskFrames[Len(diskFrames)]
         children == Children(frame.node)
     IN /\ frame.next <= Len(children)
        /\ color[children[frame.next]] = "Done"
        /\ diskFrames' = [diskFrames EXCEPT ![Len(diskFrames)].next = @ + 1]
  /\ UNCHANGED <<diskPhase, color, preorder, postorder, constructCursor,
                  constructed, installedEdges, corrupted, cycleSeen>>

RejectVisitingChild ==
  /\ diskPhase = "Validate"
  /\ Len(diskFrames) > 0
  /\ RejectBackEdge
  /\ LET frame == diskFrames[Len(diskFrames)]
         children == Children(frame.node)
     IN /\ frame.next <= Len(children)
        /\ color[children[frame.next]] = "Visiting"
  /\ diskPhase' = "Corrupted"
  /\ corrupted' = TRUE
  /\ cycleSeen' = TRUE
  /\ UNCHANGED <<diskFrames, color, preorder, postorder, constructCursor,
                  constructed, installedEdges>>

UnsafeInstallVisitingChild ==
  /\ diskPhase = "Validate"
  /\ Len(diskFrames) > 0
  /\ ~RejectBackEdge
  /\ LET frame == diskFrames[Len(diskFrames)]
         children == Children(frame.node)
     IN /\ frame.next <= Len(children)
        /\ LET child == children[frame.next]
           IN /\ color[child] = "Visiting"
              /\ diskFrames' = [diskFrames EXCEPT ![Len(diskFrames)].next = @ + 1]
              /\ installedEdges' = installedEdges \cup {<<frame.node, child>>}
  /\ cycleSeen' = TRUE
  /\ UNCHANGED <<diskPhase, color, preorder, postorder, constructCursor,
                  constructed, corrupted>>

ValidateFinishNode ==
  /\ diskPhase = "Validate"
  /\ Len(diskFrames) > 0
  /\ LET frame == diskFrames[Len(diskFrames)]
     IN /\ frame.next > Len(Children(frame.node))
        /\ diskFrames' = SubSeq(diskFrames, 1, Len(diskFrames) - 1)
        /\ color' = [color EXCEPT ![frame.node] = "Done"]
        /\ postorder' = Append(postorder, frame.node)
  /\ UNCHANGED <<diskPhase, preorder, constructCursor, constructed,
                  installedEdges, corrupted, cycleSeen>>

ValidationComplete ==
  /\ diskPhase = "Validate"
  /\ diskFrames = <<>>
  /\ diskPhase' = "Construct"
  /\ constructCursor' = 1
  /\ UNCHANGED <<diskFrames, color, preorder, postorder, constructed,
                  installedEdges, corrupted, cycleSeen>>

ConstructNode ==
  /\ diskPhase = "Construct"
  /\ constructCursor <= Len(postorder)
  /\ LET node == postorder[constructCursor]
     IN /\ constructed' = constructed \cup {node}
        /\ installedEdges' = installedEdges \cup ({node} \X SeqSet(Children(node)))
  /\ constructCursor' = constructCursor + 1
  /\ UNCHANGED <<diskPhase, diskFrames, color, preorder, postorder,
                  corrupted, cycleSeen>>

ConstructionComplete ==
  /\ diskPhase = "Construct"
  /\ constructCursor > Len(postorder)
  /\ diskPhase' = "Done"
  /\ UNCHANGED <<diskFrames, color, preorder, postorder, constructCursor,
                  constructed, installedEdges, corrupted, cycleSeen>>

DiskStep ==
  \/ ValidateUnseenChild
  \/ ValidateDoneChild
  \/ RejectVisitingChild
  \/ UnsafeInstallVisitingChild
  \/ ValidateFinishNode
  \/ ValidationComplete
  \/ ConstructNode
  \/ ConstructionComplete

MutationAction == MutationStep /\ UNCHANGED DiskVars
DiskAction == DiskStep /\ UNCHANGED MutationVars
Terminal ==
  /\ mutationPhase = "Done"
  /\ diskPhase \in {"Done", "Corrupted"}
TerminalStutter == Terminal /\ UNCHANGED vars

Next == MutationAction \/ DiskAction \/ TerminalStutter

TypeOK ==
  /\ mutationPhase \in {"Descend", "Unwind", "CAS", "Done"}
  /\ keyCursor \in 0..Depth
  /\ mutationFrames \in Seq(Units)
  /\ candidate \in Seq(Units)
  /\ published \in BOOLEAN
  /\ publishedKey \in Seq(Units)
  /\ diskPhase \in {"Validate", "Construct", "Done", "Corrupted"}
  /\ diskFrames \in Seq([node : Nodes, next : 1..(Depth + 2)])
  /\ color \in [Nodes -> {"Unseen", "Visiting", "Done"}]
  /\ preorder \in Seq(Nodes)
  /\ postorder \in Seq(Nodes)
  /\ constructCursor \in 1..(Depth + 2)
  /\ constructed \subseteq Nodes
  /\ installedEdges \subseteq GraphEdges
  /\ corrupted \in BOOLEAN
  /\ cycleSeen \in BOOLEAN

MutationFrameBound == Len(mutationFrames) <= keyCursor
MutationPartition == Len(mutationFrames) + Len(candidate) = keyCursor
MutationOrder == mutationFrames \o candidate = SubSeq(MutationKey, 1, keyCursor)
CandidateBeforePublication == published => mutationFrames = <<>>
PublishedCandidateIsExact == published => publishedKey = MutationKey
PublicationIsTerminal == published => mutationPhase = "Done"

FrameCursorBound ==
  \A i \in 1..Len(diskFrames) :
    /\ diskFrames[i].node \in Nodes
    /\ diskFrames[i].next \in 1..(Len(Children(diskFrames[i].node)) + 1)
FrameNodesUnique == Len(diskFrames) = Cardinality(FrameNodes(diskFrames))
VisitingMatchesFrames ==
  {node \in Nodes : color[node] = "Visiting"} = FrameNodes(diskFrames)
PostorderMatchesDone ==
  SeqSet(postorder) = {node \in Nodes : color[node] = "Done"}
PostorderHasNoDuplicates == Len(postorder) = Cardinality(SeqSet(postorder))
PreorderHasNoDuplicates == Len(preorder) = Cardinality(SeqSet(preorder))
NoConstructionDuringValidation ==
  diskPhase = "Validate" => constructed = {} /\ installedEdges = {}
CompletedChildrenPrecedeParent ==
  \A edge \in installedEdges : edge[1] \in constructed /\ edge[2] \in constructed
NoVisitingEdgeInstalled ==
  \A edge \in installedEdges : color[edge[2]] # "Visiting"
CorruptionIsTerminal == corrupted => diskPhase = "Corrupted"
CorruptionPrecedesArcSplice == corrupted => constructed = {} /\ installedEdges = {}
AcyclicCompletionMaterializesEveryNode ==
  (~CyclicGraph /\ diskPhase = "Done") =>
    constructed = Nodes /\ installedEdges = GraphEdges
DiamondAliasUsesOneMemoIdentity ==
  (GraphKind = "Diamond" /\ diskPhase = "Done") =>
    /\ 3 \in constructed
    /\ <<1, 3>> \in installedEdges
    /\ <<2, 3>> \in installedEdges
NoCyclicSnapshotAccepted == ~(CyclicGraph /\ diskPhase = "Done")

MutationEventuallyPublishes == <>published
AcyclicReopenEventuallyCompletes == ~CyclicGraph => <>(diskPhase = "Done")
CyclicReopenEventuallyRejects ==
  (CyclicGraph /\ RejectBackEdge) => <>(diskPhase = "Corrupted")

Spec ==
  /\ Init
  /\ [][Next]_vars
  /\ WF_vars(MutationAction)
  /\ WF_vars(DiskAction)

=============================================================================
