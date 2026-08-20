----------------------- MODULE PersistentARTrieU64Iteration -----------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC

(*
  Bounded refinement model of Rust's native-u64 lazy iterator.  The machine has
  exactly one mutable label path.  Each explicit frame stores only a node, its
  next-child cursor, the length at which that node owns the shared path, and an
  emitted bit.  Pop truncates the one path to the parent's recorded length.

  Edges carry labels independently of node identities.  The Diamond graph shares
  accepting nodes below two differently labeled incoming paths, so correct trie
  enumeration must be path-sensitive.  UseGlobalVisited is solely a negative
  control: enabling reachability-style identity suppression loses the second
  logical path.  Safe runs keep visited empty, matching Rust.

  StartMode = "RightPrefix" captures the prefix-local constructor.  A later
  publication changes publishedRevision but not capturedRevision or the captured
  graph.  AllowCancel models dropping a partially consumed iterator by clearing
  its explicit frames and path.  InjectOnDisk models an invalid native-u64 root;
  construction fails closed before enumeration because public native-u64 reopen
  must fully materialize every reachable child.

  Depth bounds TLC exploration only.  Rust has no depth cap and grows explicit
  SmallVec/Vec state rather than the native call stack.
*)

CONSTANTS Depth, GraphKind, UseGlobalVisited, StartMode, AllowCancel, InjectOnDisk

ASSUME /\ Depth \in Nat \ {0}
       /\ GraphKind \in {"Chain", "Diamond"}
       /\ UseGlobalVisited \in BOOLEAN
       /\ StartMode \in {"Root", "RightPrefix"}
       /\ AllowCancel \in BOOLEAN
       /\ InjectOnDisk \in BOOLEAN
       /\ (GraphKind = "Chain" => StartMode = "Root")
       /\ (GraphKind = "Diamond" => Depth = 4)
       /\ (InjectOnDisk => GraphKind = "Diamond")

Nodes == 0..Depth
Root == 0

Edge(label, target, resident) ==
  [label |-> label, target |-> target, resident |-> resident]

Children(node) ==
  CASE GraphKind = "Chain" ->
         IF node < Depth
         THEN <<Edge((node + 1) * 10, node + 1, TRUE)>>
         ELSE <<>>
    [] OTHER ->
         CASE node = 0 -> <<Edge(10, 1, TRUE), Edge(20, 2, TRUE)>>
           [] node = 1 -> <<Edge(30, 3, TRUE)>>
           [] node = 2 -> <<Edge(30, 3, ~InjectOnDisk)>>
           [] node = 3 -> <<Edge(40, 4, TRUE)>>
           [] OTHER -> <<>>

IsFinal(node) ==
  IF GraphKind = "Chain"
  THEN node = Depth
  ELSE node \in {3, 4}

NodeValue(node) == node * 100

StartNode == IF StartMode = "RightPrefix" THEN 2 ELSE Root
StartPath == IF StartMode = "RightPrefix" THEN <<20>> ELSE <<>>

ChainPath == [i \in 1..Depth |-> i * 10]
Output(outputPath, value) == [path |-> outputPath, value |-> value]

ExpectedOutput ==
  IF GraphKind = "Chain"
  THEN <<Output(ChainPath, NodeValue(Depth))>>
  ELSE IF StartMode = "RightPrefix"
       THEN <<Output(<<20, 30>>, 300), Output(<<20, 30, 40>>, 400)>>
       ELSE <<Output(<<10, 30>>, 300), Output(<<10, 30, 40>>, 400),
              Output(<<20, 30>>, 300), Output(<<20, 30, 40>>, 400)>>

SeqSet(sequence) == {sequence[i] : i \in 1..Len(sequence)}

ResidentTopology ==
  \A node \in Nodes :
    \A i \in 1..Len(Children(node)) : Children(node)[i].resident

TerminalPhases == {"Done", "Cancelled", "Error"}

VARIABLES phase, frames, path, emitted, visited,
          capturedRevision, publishedRevision

vars == <<phase, frames, path, emitted, visited,
          capturedRevision, publishedRevision>>

InitialFrame == [
  node |-> StartNode,
  next |-> 1,
  entered |-> FALSE,
  pathLen |-> Len(StartPath)
]

Init ==
  /\ phase = IF ResidentTopology THEN "Walk" ELSE "Error"
  /\ frames = IF ResidentTopology THEN <<InitialFrame>> ELSE <<>>
  /\ path = IF ResidentTopology THEN StartPath ELSE <<>>
  /\ emitted = <<>>
  /\ visited = IF ResidentTopology /\ UseGlobalVisited THEN {StartNode} ELSE {}
  /\ capturedRevision = 0
  /\ publishedRevision = 0

EnterFinal ==
  /\ phase = "Walk"
  /\ Len(frames) > 0
  /\ LET frame == frames[Len(frames)]
     IN /\ ~frame.entered
        /\ IsFinal(frame.node)
        /\ frames' = [frames EXCEPT ![Len(frames)].entered = TRUE]
        /\ emitted' = Append(emitted, Output(path, NodeValue(frame.node)))
  /\ UNCHANGED <<phase, path, visited, capturedRevision, publishedRevision>>

EnterNonFinal ==
  /\ phase = "Walk"
  /\ Len(frames) > 0
  /\ LET frame == frames[Len(frames)]
     IN /\ ~frame.entered
        /\ ~IsFinal(frame.node)
        /\ frames' = [frames EXCEPT ![Len(frames)].entered = TRUE]
  /\ UNCHANGED <<phase, path, emitted, visited,
                  capturedRevision, publishedRevision>>

VisitNextChild ==
  /\ phase = "Walk"
  /\ Len(frames) > 0
  /\ LET frame == frames[Len(frames)]
         children == Children(frame.node)
     IN /\ frame.entered
        /\ frame.next <= Len(children)
        /\ LET edge == children[frame.next]
               advanced == [frames EXCEPT ![Len(frames)].next = @ + 1]
           IN IF ~edge.resident
              THEN /\ phase' = "Error"
                   /\ frames' = <<>>
                   /\ path' = <<>>
                   /\ UNCHANGED <<emitted, visited,
                                   capturedRevision, publishedRevision>>
              ELSE IF UseGlobalVisited /\ edge.target \in visited
                   THEN /\ frames' = advanced
                        /\ visited' = visited
                        /\ UNCHANGED <<phase, path, emitted,
                                        capturedRevision, publishedRevision>>
                   ELSE /\ frames' = Append(advanced, [
                                node |-> edge.target,
                                next |-> 1,
                                entered |-> FALSE,
                                pathLen |-> Len(path) + 1
                              ])
                        /\ path' = Append(path, edge.label)
                        /\ visited' = IF UseGlobalVisited
                                      THEN visited \cup {edge.target}
                                      ELSE visited
                        /\ UNCHANGED <<phase, emitted,
                                        capturedRevision, publishedRevision>>

FinishNode ==
  /\ phase = "Walk"
  /\ Len(frames) > 0
  /\ LET frame == frames[Len(frames)]
         remaining == SubSeq(frames, 1, Len(frames) - 1)
         parentPathLen == IF Len(remaining) = 0
                          THEN 0
                          ELSE remaining[Len(remaining)].pathLen
     IN /\ frame.entered
        /\ frame.next > Len(Children(frame.node))
        /\ frames' = remaining
        /\ path' = SubSeq(path, 1, parentPathLen)
  /\ UNCHANGED <<phase, emitted, visited,
                  capturedRevision, publishedRevision>>

Cancel ==
  /\ phase = "Walk"
  /\ AllowCancel
  /\ phase' = "Cancelled"
  /\ frames' = <<>>
  /\ path' = <<>>
  /\ UNCHANGED <<emitted, visited, capturedRevision, publishedRevision>>

Complete ==
  /\ phase = "Walk"
  /\ frames = <<>>
  /\ phase' = "Done"
  /\ UNCHANGED <<frames, path, emitted, visited,
                  capturedRevision, publishedRevision>>

PublishMutation ==
  /\ publishedRevision = 0
  /\ publishedRevision' = 1
  /\ UNCHANGED <<phase, frames, path, emitted, visited, capturedRevision>>

IteratorStep ==
  \/ EnterFinal
  \/ EnterNonFinal
  \/ VisitNextChild
  \/ FinishNode
  \/ Cancel
  \/ Complete

TerminalStutter ==
  /\ phase \in TerminalPhases
  /\ UNCHANGED vars

Next == IteratorStep \/ PublishMutation \/ TerminalStutter

FrameType == [
  node : Nodes,
  next : 1..(Depth + 2),
  entered : BOOLEAN,
  pathLen : 0..Depth
]

OutputType == [path : Seq(Nat), value : Nat]

TypeOK ==
  /\ phase \in {"Walk", "Done", "Cancelled", "Error"}
  /\ frames \in Seq(FrameType)
  /\ path \in Seq(Nat)
  /\ emitted \in Seq(OutputType)
  /\ visited \subseteq Nodes
  /\ capturedRevision \in {0, 1}
  /\ publishedRevision \in {0, 1}

FrameCursorBound ==
  \A i \in 1..Len(frames) :
    frames[i].next \in 1..(Len(Children(frames[i].node)) + 1)

SinglePathMatchesTopFrame ==
  /\ Len(frames) = 0 => Len(path) = 0
  /\ Len(frames) > 0 => Len(path) = frames[Len(frames)].pathLen

BottomFrameMatchesPrefixCapture ==
  Len(frames) > 0 =>
    /\ frames[1].node = StartNode
    /\ frames[1].pathLen = Len(StartPath)
    /\ SubSeq(path, 1, Len(StartPath)) = StartPath

AdjacentFramesFollowLabeledEdge ==
  \A i \in 1..(Len(frames) - 1) :
    /\ frames[i + 1].pathLen = frames[i].pathLen + 1
    /\ \E edge \in SeqSet(Children(frames[i].node)) :
         /\ edge.resident
         /\ edge.target = frames[i + 1].node
         /\ edge.label = path[frames[i + 1].pathLen]

ExplicitStateIsDepthBounded ==
  /\ Len(frames) <= Depth + 1
  /\ Len(path) <= Depth

SafeModeHasNoVisitedStorage == ~UseGlobalVisited => visited = {}

EmissionIsExpectedPrefix ==
  /\ Len(emitted) <= Len(ExpectedOutput)
  /\ emitted = SubSeq(ExpectedOutput, 1, Len(emitted))

EmittedPaths == [i \in 1..Len(emitted) |-> emitted[i].path]
EmittedPathsAreUnique == Len(emitted) = Cardinality(SeqSet(EmittedPaths))

CompletionIsExact == phase = "Done" => emitted = ExpectedOutput

PrefixInitializationIsLocal ==
  StartMode = "RightPrefix" =>
    \A i \in 1..Len(emitted) :
      /\ Len(emitted[i].path) > 0
      /\ emitted[i].path[1] = 20

CapturedSnapshotIsImmutable == capturedRevision = 0

PublicationDoesNotAffectCapturedOutput ==
  publishedRevision = 1 => EmissionIsExpectedPrefix

TerminalReleasesTraversalState ==
  phase \in TerminalPhases => frames = <<>> /\ path = <<>>

OnDiskTopologyFailsClosed ==
  ~ResidentTopology => phase = "Error" /\ emitted = <<>>

DiamondAliasIsEmittedPerIncomingLabelPath ==
  (GraphKind = "Diamond" /\ StartMode = "Root" /\ phase = "Done") =>
    /\ <<10, 30>> \in SeqSet(EmittedPaths)
    /\ <<20, 30>> \in SeqSet(EmittedPaths)
    /\ <<10, 30, 40>> \in SeqSet(EmittedPaths)
    /\ <<20, 30, 40>> \in SeqSet(EmittedPaths)

LabelsAreIndependentOfNodeIdentity ==
  GraphKind = "Diamond" => Children(0)[1].label # Children(0)[1].target

IteratorEventuallyTerminates == <>(phase \in TerminalPhases)

Spec ==
  /\ Init
  /\ [][Next]_vars
  /\ WF_vars(IteratorStep)

=============================================================================
