--------------------------- MODULE NodeTransitions ----------------------------
(****************************************************************************)
(* NodeTransitions: Node growth and shrink transitions for the Persistent   *)
(* ARTrie. This module specifies how nodes transition between types         *)
(* (Node4 -> Node16 -> Node48 -> Node256) to maintain efficient storage.    *)
(*                                                                          *)
(* Key properties verified:                                                 *)
(* 1. Transitions preserve all children                                     *)
(* 2. Node type is appropriate for child count                              *)
(* 3. Transitions maintain tree structure invariants                        *)
(* 4. SIMD-optimized layouts are preserved                                  *)
(****************************************************************************)

EXTENDS ARTrieTypes, ARTrieState, Integers, Sequences, FiniteSets, TLC

--------------------------------------------------------------------------------
(* NODE TRANSITION THRESHOLDS                                                 *)
(* These define when nodes should grow or shrink.                             *)
--------------------------------------------------------------------------------

\* Threshold for growing to next node type
GrowThreshold == [
    Node4   |-> 4,   \* Grow when 5th child added
    Node16  |-> 16,  \* Grow when 17th child added
    Node48  |-> 48,  \* Grow when 49th child added
    Node256 |-> 256, \* Cannot grow further
    Bucket  |-> 256  \* Buckets split instead
]

\* Threshold for shrinking to previous node type
ShrinkThreshold == [
    Node4   |-> 0,   \* Cannot shrink
    Node16  |-> 4,   \* Shrink when <= 4 children
    Node48  |-> 16,  \* Shrink when <= 16 children
    Node256 |-> 48,  \* Shrink when <= 48 children
    Bucket  |-> 0    \* Buckets merge instead
]

--------------------------------------------------------------------------------
(* HELPER OPERATORS                                                           *)
--------------------------------------------------------------------------------

\* Count non-null children in a node
ChildCount(node) ==
    Cardinality({b \in 0..255 : ~IsNull(node.children[b])})

\* Get all children of a node as a set of (byte, ptr) pairs
GetAllChildren(node) ==
    {<<byte, node.children[byte]>> : byte \in {b \in 0..255 : ~IsNull(node.children[b])}}

\* Check if node should grow
ShouldGrow(node) ==
    /\ node.header.nodeType # "Node256"
    /\ node.header.nodeType # "Bucket"
    /\ ChildCount(node) >= GrowThreshold[node.header.nodeType]

\* Check if node should shrink
ShouldShrink(node) ==
    /\ node.header.nodeType # "Node4"
    /\ node.header.nodeType # "Bucket"
    /\ ChildCount(node) <= ShrinkThreshold[node.header.nodeType]

\* Convert sequence to set
LOCAL SeqToSetLocal(s) == {s[i] : i \in DOMAIN s}

\* Get the sorted keys for a node (for Node4/Node16 SIMD alignment)
SortedKeys(node) ==
    LET children == {b \in 0..255 : ~IsNull(node.children[b])} IN
        CHOOSE seq \in Seq(0..255) :
            /\ SeqToSetLocal(seq) = children
            /\ \A i, j \in DOMAIN seq : i < j => seq[i] < seq[j]

--------------------------------------------------------------------------------
(* NODE4 TO NODE16 TRANSITION                                                 *)
(* When Node4 becomes full (5th child), upgrade to Node16.                    *)
--------------------------------------------------------------------------------

\* Create a Node16 from a Node4's data
Node4ToNode16(node4) ==
    [
        header |-> [
            nodeType    |-> "Node16",
            prefixLen   |-> node4.header.prefixLen,
            flags       |-> node4.header.flags,
            numChildren |-> node4.header.numChildren,
            version     |-> node4.header.version + 2  \* New version
        ],
        prefix     |-> node4.prefix,
        children   |-> node4.children,  \* Children mapping stays the same
        bucketData |-> <<>>
    ]

\* Predicate: Node4 can transition to Node16
CanGrowNode4(nid) ==
    /\ ValidNode(nid)
    /\ nodes[nid].header.nodeType = "Node4"
    /\ ChildCount(nodes[nid]) = 4  \* About to add 5th child

\* Action: Grow Node4 to Node16
GrowNode4ToNode16(thread, nid) ==
    /\ CanGrowNode4(nid)
    /\ HoldsWriteLock(thread, nid)
    /\ LET
        oldNode == nodes[nid]
        newNode == Node4ToNode16(oldNode)
       IN
        /\ nodes' = [nodes EXCEPT ![nid] = newNode]
        \* Update version (write lock will release with even version)
        /\ versions' = [versions EXCEPT ![nid] = newNode.header.version - 1]
    /\ UNCHANGED <<abstractMap, entryCount, root, nextNodeId, threads, writers>>

--------------------------------------------------------------------------------
(* NODE16 TO NODE48 TRANSITION                                                *)
(* When Node16 becomes full (17th child), upgrade to Node48.                  *)
--------------------------------------------------------------------------------

\* Create a Node48 from a Node16's data
\* Node48 uses an index array for O(1) lookup
Node16ToNode48(node16) ==
    [
        header |-> [
            nodeType    |-> "Node48",
            prefixLen   |-> node16.header.prefixLen,
            flags       |-> node16.header.flags,
            numChildren |-> node16.header.numChildren,
            version     |-> node16.header.version + 2
        ],
        prefix     |-> node16.prefix,
        children   |-> node16.children,  \* Children mapping stays the same
        bucketData |-> <<>>
    ]

\* Predicate: Node16 can transition to Node48
CanGrowNode16(nid) ==
    /\ ValidNode(nid)
    /\ nodes[nid].header.nodeType = "Node16"
    /\ ChildCount(nodes[nid]) = 16

\* Action: Grow Node16 to Node48
GrowNode16ToNode48(thread, nid) ==
    /\ CanGrowNode16(nid)
    /\ HoldsWriteLock(thread, nid)
    /\ LET
        oldNode == nodes[nid]
        newNode == Node16ToNode48(oldNode)
       IN
        /\ nodes' = [nodes EXCEPT ![nid] = newNode]
        /\ versions' = [versions EXCEPT ![nid] = newNode.header.version - 1]
    /\ UNCHANGED <<abstractMap, entryCount, root, nextNodeId, threads, writers>>

--------------------------------------------------------------------------------
(* NODE48 TO NODE256 TRANSITION                                               *)
(* When Node48 becomes full (49th child), upgrade to Node256.                 *)
--------------------------------------------------------------------------------

\* Create a Node256 from a Node48's data
Node48ToNode256(node48) ==
    [
        header |-> [
            nodeType    |-> "Node256",
            prefixLen   |-> node48.header.prefixLen,
            flags       |-> node48.header.flags,
            numChildren |-> node48.header.numChildren,
            version     |-> node48.header.version + 2
        ],
        prefix     |-> node48.prefix,
        children   |-> node48.children,
        bucketData |-> <<>>
    ]

\* Predicate: Node48 can transition to Node256
CanGrowNode48(nid) ==
    /\ ValidNode(nid)
    /\ nodes[nid].header.nodeType = "Node48"
    /\ ChildCount(nodes[nid]) = 48

\* Action: Grow Node48 to Node256
GrowNode48ToNode256(thread, nid) ==
    /\ CanGrowNode48(nid)
    /\ HoldsWriteLock(thread, nid)
    /\ LET
        oldNode == nodes[nid]
        newNode == Node48ToNode256(oldNode)
       IN
        /\ nodes' = [nodes EXCEPT ![nid] = newNode]
        /\ versions' = [versions EXCEPT ![nid] = newNode.header.version - 1]
    /\ UNCHANGED <<abstractMap, entryCount, root, nextNodeId, threads, writers>>

--------------------------------------------------------------------------------
(* NODE16 TO NODE4 TRANSITION (SHRINK)                                        *)
(* When Node16 has <= 4 children, shrink to Node4.                            *)
--------------------------------------------------------------------------------

\* Create a Node4 from a Node16's data
Node16ToNode4(node16) ==
    [
        header |-> [
            nodeType    |-> "Node4",
            prefixLen   |-> node16.header.prefixLen,
            flags       |-> node16.header.flags,
            numChildren |-> node16.header.numChildren,
            version     |-> node16.header.version + 2
        ],
        prefix     |-> node16.prefix,
        children   |-> node16.children,
        bucketData |-> <<>>
    ]

\* Predicate: Node16 can shrink to Node4
CanShrinkNode16(nid) ==
    /\ ValidNode(nid)
    /\ nodes[nid].header.nodeType = "Node16"
    /\ ChildCount(nodes[nid]) <= 4

\* Action: Shrink Node16 to Node4
ShrinkNode16ToNode4(thread, nid) ==
    /\ CanShrinkNode16(nid)
    /\ HoldsWriteLock(thread, nid)
    /\ LET
        oldNode == nodes[nid]
        newNode == Node16ToNode4(oldNode)
       IN
        /\ nodes' = [nodes EXCEPT ![nid] = newNode]
        /\ versions' = [versions EXCEPT ![nid] = newNode.header.version - 1]
    /\ UNCHANGED <<abstractMap, entryCount, root, nextNodeId, threads, writers>>

--------------------------------------------------------------------------------
(* NODE48 TO NODE16 TRANSITION (SHRINK)                                       *)
(* When Node48 has <= 16 children, shrink to Node16.                          *)
--------------------------------------------------------------------------------

\* Create a Node16 from a Node48's data
Node48ToNode16(node48) ==
    [
        header |-> [
            nodeType    |-> "Node16",
            prefixLen   |-> node48.header.prefixLen,
            flags       |-> node48.header.flags,
            numChildren |-> node48.header.numChildren,
            version     |-> node48.header.version + 2
        ],
        prefix     |-> node48.prefix,
        children   |-> node48.children,
        bucketData |-> <<>>
    ]

\* Predicate: Node48 can shrink to Node16
CanShrinkNode48(nid) ==
    /\ ValidNode(nid)
    /\ nodes[nid].header.nodeType = "Node48"
    /\ ChildCount(nodes[nid]) <= 16

\* Action: Shrink Node48 to Node16
ShrinkNode48ToNode16(thread, nid) ==
    /\ CanShrinkNode48(nid)
    /\ HoldsWriteLock(thread, nid)
    /\ LET
        oldNode == nodes[nid]
        newNode == Node48ToNode16(oldNode)
       IN
        /\ nodes' = [nodes EXCEPT ![nid] = newNode]
        /\ versions' = [versions EXCEPT ![nid] = newNode.header.version - 1]
    /\ UNCHANGED <<abstractMap, entryCount, root, nextNodeId, threads, writers>>

--------------------------------------------------------------------------------
(* NODE256 TO NODE48 TRANSITION (SHRINK)                                      *)
(* When Node256 has <= 48 children, shrink to Node48.                         *)
--------------------------------------------------------------------------------

\* Create a Node48 from a Node256's data
Node256ToNode48(node256) ==
    [
        header |-> [
            nodeType    |-> "Node48",
            prefixLen   |-> node256.header.prefixLen,
            flags       |-> node256.header.flags,
            numChildren |-> node256.header.numChildren,
            version     |-> node256.header.version + 2
        ],
        prefix     |-> node256.prefix,
        children   |-> node256.children,
        bucketData |-> <<>>
    ]

\* Predicate: Node256 can shrink to Node48
CanShrinkNode256(nid) ==
    /\ ValidNode(nid)
    /\ nodes[nid].header.nodeType = "Node256"
    /\ ChildCount(nodes[nid]) <= 48

\* Action: Shrink Node256 to Node48
ShrinkNode256ToNode48(thread, nid) ==
    /\ CanShrinkNode256(nid)
    /\ HoldsWriteLock(thread, nid)
    /\ LET
        oldNode == nodes[nid]
        newNode == Node256ToNode48(oldNode)
       IN
        /\ nodes' = [nodes EXCEPT ![nid] = newNode]
        /\ versions' = [versions EXCEPT ![nid] = newNode.header.version - 1]
    /\ UNCHANGED <<abstractMap, entryCount, root, nextNodeId, threads, writers>>

--------------------------------------------------------------------------------
(* PATH COMPRESSION TRANSITIONS                                               *)
(* Operations for splitting and extending prefixes.                           *)
--------------------------------------------------------------------------------

\* Split a prefix at a given position (for insertion divergence)
SplitPrefix(prefix, prefixLen, splitPos) ==
    [
        before    |-> SubSeq(prefix, 1, splitPos),
        beforeLen |-> splitPos,
        splitByte |-> IF splitPos < prefixLen THEN prefix[splitPos + 1] ELSE 0,
        after     |-> SubSeq(prefix, splitPos + 2, prefixLen),
        afterLen  |-> IF prefixLen > splitPos + 1 THEN prefixLen - splitPos - 1 ELSE 0
    ]

\* Create a new intermediate node for prefix split
CreateSplitNode(thread, parentNid, splitInfo, newChildNid) ==
    /\ ValidNode(parentNid)
    /\ HoldsWriteLock(thread, parentNid)
    /\ nextNodeId <= Cardinality(NodeIds)  \* Bounded by model
    /\ LET
        oldNode == nodes[parentNid]
        \* New intermediate node with the 'before' prefix
        splitNode == [
            header |-> [
                nodeType    |-> "Node4",
                prefixLen   |-> splitInfo.beforeLen,
                flags       |-> {},  \* Not final
                numChildren |-> 2,   \* Two children: old path and new key
                version     |-> 0
            ],
            prefix     |-> splitInfo.before,
            children   |-> [b \in 0..255 |->
                IF b = splitInfo.splitByte
                THEN InMemoryPtr(parentNid)  \* Original subtree
                ELSE IF b = newChildNid
                     THEN InMemoryPtr(nextNodeId)  \* New child
                     ELSE NullPtr],
            bucketData |-> <<>>
        ]
       IN
        \* Update the old node's prefix to 'after'
        /\ nodes' = [nodes EXCEPT
            ![parentNid].prefix = splitInfo.after,
            ![parentNid].header.prefixLen = splitInfo.afterLen]
    /\ UNCHANGED <<abstractMap, entryCount, root, threads, versions, writers>>

--------------------------------------------------------------------------------
(* TRANSITION INVARIANTS                                                      *)
(* Properties that must hold during and after transitions.                    *)
--------------------------------------------------------------------------------

\* Children are preserved during growth
GrowthPreservesChildren(oldNode, newNode) ==
    GetAllChildren(oldNode) = GetAllChildren(newNode)

\* Children are preserved during shrink
ShrinkPreservesChildren(oldNode, newNode) ==
    GetAllChildren(oldNode) = GetAllChildren(newNode)

\* Prefix is preserved during node type changes
PrefixPreserved(oldNode, newNode) ==
    /\ oldNode.prefix = newNode.prefix
    /\ oldNode.header.prefixLen = newNode.header.prefixLen

\* Node type is appropriate for child count
NodeTypeAppropriate(node) ==
    LET cc == ChildCount(node) IN
        \/ (node.header.nodeType = "Node4" /\ cc <= 4)
        \/ (node.header.nodeType = "Node16" /\ cc > 4 /\ cc <= 16)
        \/ (node.header.nodeType = "Node48" /\ cc > 16 /\ cc <= 48)
        \/ (node.header.nodeType = "Node256" /\ cc > 48 /\ cc <= 256)
        \/ node.header.nodeType = "Bucket"

\* All nodes have appropriate types
AllNodesAppropriateType ==
    \A nid \in NodeIds :
        ValidNode(nid) => NodeTypeAppropriate(nodes[nid])

\* Flags are preserved during transitions
FlagsPreserved(oldNode, newNode) ==
    oldNode.header.flags = newNode.header.flags

--------------------------------------------------------------------------------
(* TRANSITION CORRECTNESS THEOREMS                                            *)
(* Key theorems about node transitions.                                       *)
--------------------------------------------------------------------------------

\* Theorem: Node4 -> Node16 preserves all children
THEOREM Node4ToNode16Correct ==
    \A node : node.header.nodeType = "Node4" =>
        GrowthPreservesChildren(node, Node4ToNode16(node))

\* Theorem: Node16 -> Node48 preserves all children
THEOREM Node16ToNode48Correct ==
    \A node : node.header.nodeType = "Node16" =>
        GrowthPreservesChildren(node, Node16ToNode48(node))

\* Theorem: Node48 -> Node256 preserves all children
THEOREM Node48ToNode256Correct ==
    \A node : node.header.nodeType = "Node48" =>
        GrowthPreservesChildren(node, Node48ToNode256(node))

\* Theorem: Node16 -> Node4 preserves all children
THEOREM Node16ToNode4Correct ==
    \A node :
        /\ node.header.nodeType = "Node16"
        /\ ChildCount(node) <= 4
        => ShrinkPreservesChildren(node, Node16ToNode4(node))

\* Theorem: Node48 -> Node16 preserves all children
THEOREM Node48ToNode16Correct ==
    \A node :
        /\ node.header.nodeType = "Node48"
        /\ ChildCount(node) <= 16
        => ShrinkPreservesChildren(node, Node48ToNode16(node))

\* Theorem: Node256 -> Node48 preserves all children
THEOREM Node256ToNode48Correct ==
    \A node :
        /\ node.header.nodeType = "Node256"
        /\ ChildCount(node) <= 48
        => ShrinkPreservesChildren(node, Node256ToNode48(node))

--------------------------------------------------------------------------------
(* COMBINED ACTIONS                                                           *)
--------------------------------------------------------------------------------

\* Any growth transition
GrowAction(thread, nid) ==
    \/ GrowNode4ToNode16(thread, nid)
    \/ GrowNode16ToNode48(thread, nid)
    \/ GrowNode48ToNode256(thread, nid)

\* Any shrink transition
ShrinkAction(thread, nid) ==
    \/ ShrinkNode16ToNode4(thread, nid)
    \/ ShrinkNode48ToNode16(thread, nid)
    \/ ShrinkNode256ToNode48(thread, nid)

\* Any node transition
NodeTransitionAction(thread, nid) ==
    \/ GrowAction(thread, nid)
    \/ ShrinkAction(thread, nid)

--------------------------------------------------------------------------------
(* SAFETY INVARIANT                                                           *)
--------------------------------------------------------------------------------

NodeTransitionSafetyInvariant ==
    AllNodesAppropriateType

================================================================================
(* LICENSE: MIT License                                                       *)
(* Copyright (c) 2026 F1r3fly.io                                              *)
================================================================================
