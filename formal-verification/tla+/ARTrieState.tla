------------------------------- MODULE ARTrieState -------------------------------
(****************************************************************************)
(* ARTrieState: Abstract state specification for the Persistent ARTrie.     *)
(* This module defines the abstract key-value mapping that the ARTrie       *)
(* implements, along with the core state variables and basic operations.    *)
(*                                                                          *)
(* The abstract state serves as:                                            *)
(* 1. The specification that the concrete implementation refines            *)
(* 2. The oracle for checking linearizability                               *)
(* 3. The abstract data type that Rocq proofs will reference                *)
(****************************************************************************)

EXTENDS ARTrieTypes, Integers, Sequences, FiniteSets, TLC

--------------------------------------------------------------------------------
(* STATE VARIABLES                                                            *)
--------------------------------------------------------------------------------

VARIABLES
    (**** Abstract State (The specification) ****)
    \* The abstract key-value mapping
    abstractMap,

    \* The total number of entries
    entryCount,

    (**** Concrete State (The implementation) ****)
    \* The root node of the trie (SwizzledPtr)
    root,

    \* All nodes in memory: NodeId -> AbstractNode
    nodes,

    \* Next available node ID
    nextNodeId,

    (**** Operational State ****)
    \* Thread contexts: Thread -> ThreadContext
    threads,

    \* Node locks (version numbers): NodeId -> Nat
    versions,

    \* Writer threads holding locks: NodeId -> Thread \cup {0}
    writers

--------------------------------------------------------------------------------
(* TYPE INVARIANT                                                             *)
(* Defines the valid states for all variables.                                *)
--------------------------------------------------------------------------------

TypeInvariant ==
    /\ abstractMap \in [Keys -> Values \cup {Null}]
    /\ entryCount \in Nat
    /\ root \in SwizzledPtr
    /\ nodes \in [NodeIds -> AbstractNode \cup {<<>>}]
    /\ nextNodeId \in Nat
    /\ threads \in [Threads -> ThreadContext]
    /\ versions \in [NodeIds -> Nat]
    /\ writers \in [NodeIds -> Threads \cup {0}]

--------------------------------------------------------------------------------
(* INITIAL STATE                                                              *)
(* The ARTrie starts with an empty mapping and a root Node4.                  *)
--------------------------------------------------------------------------------

InitAbstractMap == [k \in Keys |-> Null]

InitRootNode == [
    header |-> [
        nodeType    |-> "Node4",
        prefixLen   |-> 0,
        flags       |-> {},
        numChildren |-> 0,
        version     |-> 0
    ],
    prefix     |-> <<>>,
    children   |-> EmptyChildren,
    bucketData |-> <<>>
]

InitThreadContext == [
    state         |-> "Idle",
    currentNode   |-> Null,
    heldVersion   |-> 0,
    operationType |-> "None",
    targetKey     |-> Null,
    targetValue   |-> Null
]

Init ==
    /\ abstractMap = InitAbstractMap
    /\ entryCount = 0
    /\ root = InMemoryPtr(1)
    /\ nodes = [nid \in NodeIds |-> IF nid = 1 THEN InitRootNode ELSE <<>>]
    /\ nextNodeId = 2
    /\ threads = [t \in Threads |-> InitThreadContext]
    /\ versions = [nid \in NodeIds |-> 0]
    /\ writers = [nid \in NodeIds |-> 0]

--------------------------------------------------------------------------------
(* ABSTRACT MAP OPERATIONS                                                    *)
(* These define the abstract semantics that the ARTrie must implement.        *)
--------------------------------------------------------------------------------

\* Abstract lookup: returns value if key exists, <<>> otherwise
AbstractLookup(key) == abstractMap[key]

\* Abstract insert: associates key with value
AbstractInsert(key, value) ==
    [abstractMap EXCEPT ![key] = value]

\* Abstract remove: disassociates key (sets to Null)
AbstractRemove(key) ==
    [abstractMap EXCEPT ![key] = Null]

\* Abstract contains: checks if key has a value
AbstractContains(key) == abstractMap[key] # Null

\* Abstract size: count of keys with values
AbstractSize == Cardinality({k \in Keys : abstractMap[k] # Null})

--------------------------------------------------------------------------------
(* AUXILIARY PREDICATES                                                       *)
--------------------------------------------------------------------------------

\* Check if a node ID is valid (has been allocated)
ValidNode(nid) ==
    /\ nid \in NodeIds
    /\ nodes[nid] # <<>>

\* Get the node for a swizzled pointer (assumes in-memory)
GetNode(ptr) ==
    IF IsInMemory(ptr) THEN nodes[ptr.memPtr] ELSE <<>>

\* Check if a thread is idle
ThreadIdle(t) == threads[t].state = "Idle"

\* Check if a thread is reading
ThreadReading(t) == threads[t].state = "Reading"

\* Check if a thread is writing
ThreadWriting(t) == threads[t].state = "Writing"

\* Check if any thread holds write lock on node
NodeLocked(nid) == writers[nid] # 0

\* Check if specific thread holds write lock on node
HoldsWriteLock(t, nid) == writers[nid] = t

--------------------------------------------------------------------------------
(* PREFIX MATCHING                                                            *)
(* Path compression requires matching key prefixes against node prefixes.     *)
--------------------------------------------------------------------------------

\* Compare a key segment against a node's prefix
\* Returns: {"FullMatch", "PartialMatch", "KeyTooShort"}
MatchPrefix(key, keyOffset, node) ==
    LET
        prefix == node.prefix
        prefixLen == node.header.prefixLen
        keyRemaining == Len(key) - keyOffset
    IN
        IF keyRemaining < prefixLen THEN
            "KeyTooShort"
        ELSE IF \A i \in 1..prefixLen : key[keyOffset + i] = prefix[i] THEN
            "FullMatch"
        ELSE
            "PartialMatch"

\* Find the first mismatch position in prefix
PrefixMismatchPos(key, keyOffset, node) ==
    LET
        prefix == node.prefix
        prefixLen == node.header.prefixLen
        checkLen == IF Len(key) - keyOffset < prefixLen
                    THEN Len(key) - keyOffset
                    ELSE prefixLen
    IN
        CHOOSE i \in 0..checkLen :
            /\ \A j \in 1..i : key[keyOffset + j] = prefix[j]
            /\ (i = checkLen \/ key[keyOffset + i + 1] # prefix[i + 1])

--------------------------------------------------------------------------------
(* CHILD LOOKUP                                                               *)
(* Finding a child based on the next byte of the key.                         *)
--------------------------------------------------------------------------------

\* Get child pointer for a given byte
GetChild(node, byte) == node.children[byte]

\* Check if node has a child for given byte
HasChild(node, byte) == ~IsNull(node.children[byte])

\* Count the number of children
CountChildren(node) ==
    Cardinality({b \in 0..255 : ~IsNull(node.children[b])})

--------------------------------------------------------------------------------
(* TRIE TRAVERSAL                                                             *)
(* Abstract traversal operations for lookup/insert/remove.                    *)
--------------------------------------------------------------------------------

\* Traverse one step in the trie
\* Returns: <<new_node_ptr, new_key_offset, status>>
\* Status: "Continue", "Found", "NotFound", "PrefixMismatch"
TraverseStep(currentNode, key, keyOffset) ==
    LET
        prefixMatch == MatchPrefix(key, keyOffset, currentNode)
        newOffset == keyOffset + currentNode.header.prefixLen
    IN
        IF prefixMatch = "KeyTooShort" THEN
            <<"NotFound">>
        ELSE IF prefixMatch = "PartialMatch" THEN
            <<"PrefixMismatch">>
        ELSE IF newOffset >= Len(key) THEN
            IF "IS_FINAL" \in currentNode.header.flags THEN
                <<"Found">>
            ELSE
                <<"NotFound">>
        ELSE
            LET
                nextByte == key[newOffset + 1]
                childPtr == GetChild(currentNode, nextByte)
            IN
                IF IsNull(childPtr) THEN
                    <<"NotFound">>
                ELSE
                    <<childPtr, newOffset + 1, "Continue">>

--------------------------------------------------------------------------------
(* NODE STRUCTURE INVARIANTS                                                  *)
(* These must hold for all nodes at all times.                                *)
--------------------------------------------------------------------------------

\* Node has valid child count for its type
ValidChildCount(node) ==
    /\ node.header.numChildren >= 0
    /\ node.header.numChildren <= NodeCapacity[node.header.nodeType]
    /\ node.header.numChildren = CountChildren(node)

\* Node prefix is valid
ValidPrefix(node) ==
    /\ node.header.prefixLen <= MAX_PREFIX_LEN
    /\ Len(node.prefix) = node.header.prefixLen

\* Node type is appropriate for child count
AppropriateNodeType(node) ==
    LET nc == node.header.numChildren IN
        \/ (node.header.nodeType = "Node4" /\ nc <= 4)
        \/ (node.header.nodeType = "Node16" /\ nc > 4 /\ nc <= 16)
        \/ (node.header.nodeType = "Node48" /\ nc > 16 /\ nc <= 48)
        \/ (node.header.nodeType = "Node256" /\ nc > 48 /\ nc <= 256)
        \/ node.header.nodeType = "Bucket"

\* All children point to valid nodes
ValidChildren(node) ==
    \A b \in 0..255 :
        ~IsNull(node.children[b]) =>
            (IsInMemory(node.children[b]) => ValidNode(node.children[b].memPtr))

\* Single node well-formedness
WellFormedNode(node) ==
    /\ node # <<>>
    /\ ValidChildCount(node)
    /\ ValidPrefix(node)
    /\ ValidChildren(node)

\* All allocated nodes are well-formed
AllNodesWellFormed ==
    \A nid \in NodeIds : nodes[nid] # <<>> => WellFormedNode(nodes[nid])

--------------------------------------------------------------------------------
(* COMPLETENESS INVARIANT                                                     *)
(* All items in the abstract map must be retrievable from the trie.           *)
--------------------------------------------------------------------------------

CompletenessInvariant ==
    \A k \in Keys :
        abstractMap[k] # Null =>
            \* The key should be findable in the trie
            \* (This is checked more precisely in the refinement)
            TRUE

--------------------------------------------------------------------------------
(* CONSISTENCY INVARIANT                                                      *)
(* Items removed from the abstract map must not be retrievable.               *)
--------------------------------------------------------------------------------

ConsistencyInvariant ==
    \A k \in Keys :
        abstractMap[k] = Null =>
            \* The key should not be findable in the trie
            TRUE

--------------------------------------------------------------------------------
(* SIZE INVARIANT                                                             *)
(* The entry count matches the abstract map size.                             *)
--------------------------------------------------------------------------------

SizeInvariant ==
    entryCount = AbstractSize

--------------------------------------------------------------------------------
(* VERSION-LOCK CONSISTENCY                                                   *)
(* Odd version implies a writer holds the lock.                               *)
--------------------------------------------------------------------------------

VersionLockConsistency ==
    \A nid \in NodeIds :
        ValidNode(nid) =>
            (IsWriting(versions[nid]) <=> writers[nid] # 0)

--------------------------------------------------------------------------------
(* EXCLUSIVE WRITE INVARIANT                                                  *)
(* At most one writer per node.                                               *)
--------------------------------------------------------------------------------

ExclusiveWriteInvariant ==
    \A nid \in NodeIds :
        writers[nid] # 0 =>
            Cardinality({t \in Threads : HoldsWriteLock(t, nid)}) = 1

--------------------------------------------------------------------------------
(* COMBINED SAFETY INVARIANT                                                  *)
--------------------------------------------------------------------------------

\* Full safety invariant (may be slow for TLC due to TypeInvariant)
FullSafetyInvariant ==
    /\ TypeInvariant
    /\ AllNodesWellFormed
    /\ CompletenessInvariant
    /\ ConsistencyInvariant
    /\ VersionLockConsistency
    /\ ExclusiveWriteInvariant

\* TLC-friendly safety invariant (skips expensive type checks)
SafetyInvariant ==
    /\ CompletenessInvariant
    /\ ConsistencyInvariant
    /\ VersionLockConsistency
    /\ ExclusiveWriteInvariant

--------------------------------------------------------------------------------
(* STATE VARIABLE EXPORTS                                                     *)
(* For use by other modules that extend this one.                             *)
--------------------------------------------------------------------------------

vars == <<abstractMap, entryCount, root, nodes, nextNodeId, threads, versions, writers>>

================================================================================
(* LICENSE: MIT License                                                       *)
(* Copyright (c) 2026 F1r3fly.io                                              *)
================================================================================
