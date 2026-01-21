-------------------------------- MODULE ARTrieTypes --------------------------------
(****************************************************************************)
(* ARTrieTypes: Type definitions and constants for the Persistent ARTrie    *)
(* specification. This module defines the fundamental types, node kinds,    *)
(* and constants used throughout the TLA+ specification.                    *)
(*                                                                          *)
(* Based on the Rust implementation in libdictenstein:                      *)
(* - Node types: Node4, Node16, Node48, Node256                             *)
(* - Bucket: B-trie leaf storage with binary search                         *)
(* - Path compression: Up to 12 bytes inline prefix                         *)
(* - Optimistic lock coupling: Even=stable, Odd=writing                     *)
(* - Swizzled pointers: Memory/disk reference transparency                  *)
(****************************************************************************)

EXTENDS Integers, Sequences, FiniteSets, TLC

--------------------------------------------------------------------------------
(* MODEL CONFIGURATION CONSTANTS                                              *)
(* These are parameterized for model checking - use small values initially    *)
--------------------------------------------------------------------------------

CONSTANTS
    \* Number of concurrent threads/processes
    NumThreads,

    \* Maximum number of keys in the model
    MaxKeys,

    \* Maximum key length (in bytes, abstracted as sequence length)
    MaxKeyLength,

    \* Maximum LSN (log sequence number) for bounded model checking
    MaxLSN,

    \* Maximum epoch number for bounded model checking
    MaxEpoch,

    \* Maximum transaction ID for bounded model checking
    MaxTxId,

    \* Enable crash modeling (TRUE/FALSE)
    EnableCrash,

    \* Node identifiers (abstract set for model checking)
    NodeIds,

    \* Key values (abstract set representing byte sequences)
    Keys,

    \* Value type (can be any set, e.g., Nat for simplicity)
    Values,

    \* Null value constant (model value to represent absence)
    \* Use a model value like "Null" in TLC configuration
    Null

ASSUME NumThreads \in Nat \ {0}
\* Note: Null must be distinct from all Values - this is ensured by configuration
ASSUME MaxKeys \in Nat \ {0}
ASSUME MaxKeyLength \in Nat \ {0}
ASSUME MaxLSN \in Nat
ASSUME MaxEpoch \in Nat
ASSUME EnableCrash \in BOOLEAN

--------------------------------------------------------------------------------
(* NODE TYPES                                                                 *)
(* The ARTrie uses adaptive radix tree nodes that grow/shrink based on        *)
(* child count. Each node type has different capacity and lookup strategy.    *)
--------------------------------------------------------------------------------

NodeType == {"Node4", "Node16", "Node48", "Node256", "Bucket"}

\* Node type capacity limits (from Rust implementation)
NodeCapacity == [
    Node4   |-> 4,
    Node16  |-> 16,
    Node48  |-> 48,
    Node256 |-> 256,
    Bucket  |-> 256   \* MAX_BUCKET_ENTRIES
]

\* Minimum children for downgrade (shrink threshold)
NodeMinChildren == [
    Node4   |-> 0,    \* Node4 cannot shrink further
    Node16  |-> 4,    \* Shrink to Node4 if <= 4 children
    Node48  |-> 16,   \* Shrink to Node16 if <= 16 children
    Node256 |-> 48,   \* Shrink to Node48 if <= 48 children
    Bucket  |-> 0     \* Buckets don't shrink this way
]

\* Target node type after growth
NodeGrowTarget == [
    Node4   |-> "Node16",
    Node16  |-> "Node48",
    Node48  |-> "Node256",
    Node256 |-> "Node256",  \* Cannot grow further
    Bucket  |-> "Bucket"    \* Buckets split, don't grow to nodes
]

\* Target node type after shrink
NodeShrinkTarget == [
    Node4   |-> "Node4",    \* Cannot shrink
    Node16  |-> "Node4",
    Node48  |-> "Node16",
    Node256 |-> "Node48",
    Bucket  |-> "Bucket"    \* Buckets merge, don't shrink to other types
]

--------------------------------------------------------------------------------
(* PATH COMPRESSION                                                           *)
(* Path compression stores up to MAX_PREFIX_LEN bytes inline in each node     *)
(* to reduce tree height.                                                     *)
--------------------------------------------------------------------------------

MAX_PREFIX_LEN == 12

\* Compressed prefix is a sequence of bytes (abstracted)
CompressedPrefix == Seq(0..255)

\* Prefix match result types
PrefixMatchResult == {"FullMatch", "PartialMatch", "KeyTooShort"}

--------------------------------------------------------------------------------
(* NODE HEADER AND FLAGS                                                      *)
(* Each node has a header with type, prefix length, flags, and version.       *)
--------------------------------------------------------------------------------

\* Node flags (bit flags in implementation, explicit set here)
NodeFlag == {"IS_FINAL", "IS_DIRTY", "IS_LEAF"}

\* A node header record
NodeHeader == [
    nodeType   : NodeType,
    prefixLen  : 0..MAX_PREFIX_LEN,
    flags      : SUBSET NodeFlag,
    numChildren: 0..256,
    version    : Nat
]

--------------------------------------------------------------------------------
(* VERSION NUMBERS FOR OPTIMISTIC LOCKING                                     *)
(* Even versions = stable state, Odd versions = write in progress             *)
--------------------------------------------------------------------------------

\* Check if version indicates stable (even)
IsStable(v) == v % 2 = 0

\* Check if version indicates writing (odd)
IsWriting(v) == v % 2 = 1

\* Increment version for begin write (even -> odd)
BeginWriteVersion(v) == IF IsStable(v) THEN v + 1 ELSE v

\* Increment version for end write (odd -> even)
EndWriteVersion(v) == IF IsWriting(v) THEN v + 1 ELSE v

--------------------------------------------------------------------------------
(* SWIZZLED POINTERS                                                          *)
(* A swizzled pointer can reference either memory (swizzled) or disk          *)
(* (unswizzled). The MSB indicates the state.                                 *)
--------------------------------------------------------------------------------

SwizzleState == {"InMemory", "OnDisk", "Null"}

\* A swizzled pointer record
SwizzledPtr == [
    state     : SwizzleState,
    \* For InMemory: points to a NodeId
    memPtr    : NodeIds \cup {<<>>},
    \* For OnDisk: block_id, offset, node_type
    blockId   : Nat,
    offset    : Nat,
    ptrNodeType: NodeType \cup {"None"}
]

\* Null pointer constant
NullPtr == [state |-> "Null", memPtr |-> <<>>, blockId |-> 0, offset |-> 0, ptrNodeType |-> "None"]

\* Create an in-memory pointer
InMemoryPtr(nodeId) == [
    state |-> "InMemory",
    memPtr |-> nodeId,
    blockId |-> 0,
    offset |-> 0,
    ptrNodeType |-> "None"
]

\* Create an on-disk pointer
OnDiskPtr(bid, off, nt) == [
    state |-> "OnDisk",
    memPtr |-> <<>>,
    blockId |-> bid,
    offset |-> off,
    ptrNodeType |-> nt
]

--------------------------------------------------------------------------------
(* WAL RECORD TYPES                                                           *)
(* The write-ahead log supports various record types for durability.          *)
--------------------------------------------------------------------------------

WalRecordType == {
    "Insert",
    "Remove",
    "Checkpoint",
    "BeginTx",
    "CommitTx",
    "AbortTx",
    "Increment",
    "Upsert",
    "CompareAndSwap",
    "BatchInsert"
}

\* A WAL record (set definition - cannot be enumerated by TLC due to Int)
WalRecord == [
    lsn      : Nat,
    recType  : WalRecordType,
    txId     : Nat \cup {0},  \* 0 for non-transactional
    key      : Keys \cup {Null},
    value    : Values \cup {Null},
    \* For CAS operations
    expected : Values \cup {Null},
    \* For increment operations
    delta    : Int,
    \* For checkpoint
    checkpointLsn: Nat
]

\* Predicate-based type check for WAL record (TLC-compatible)
IsWalRecord(r) ==
    /\ DOMAIN r = {"lsn", "recType", "txId", "key", "value", "expected", "delta", "checkpointLsn"}
    /\ r.lsn \in Nat
    /\ r.recType \in WalRecordType
    /\ r.txId \in Nat
    /\ r.key \in Keys \cup {Null}
    /\ r.value \in Values \cup {Null}
    \* expected can be a value, Null, or <<>> (empty sequence for non-CAS operations)
    /\ (r.expected = <<>> \/ r.expected \in Values \cup {Null})
    /\ r.delta \in Int
    /\ r.checkpointLsn \in Nat

--------------------------------------------------------------------------------
(* TRANSACTION STATES                                                         *)
--------------------------------------------------------------------------------

TxState == {"InProgress", "Committed", "Aborted"}

\* Transaction record (set definition - cannot be enumerated by TLC)
Transaction == [
    txId      : Nat,
    state     : TxState,
    beginLsn  : Nat,
    commitLsn : Nat \cup {0},  \* 0 if not yet committed
    operations: Seq(WalRecord)
]

\* Predicate-based type check for Transaction (TLC-compatible)
IsTransaction(t) ==
    /\ DOMAIN t = {"txId", "state", "beginLsn", "commitLsn", "operations"}
    /\ t.txId \in Nat
    /\ t.state \in TxState
    /\ t.beginLsn \in Nat
    /\ t.commitLsn \in Nat
    /\ \A i \in DOMAIN t.operations : IsWalRecord(t.operations[i])

--------------------------------------------------------------------------------
(* EPOCH STATES                                                               *)
(* Epochs transition: Active -> Sealing -> Durable -> Archived                *)
--------------------------------------------------------------------------------

EpochState == {"Active", "Sealing", "Durable", "Archived"}

\* Epoch metadata record
EpochMetadata == [
    epochId       : Nat,
    state         : EpochState,
    operationCount: Nat,
    firstLsn      : Nat,
    lastLsn       : Nat
]

--------------------------------------------------------------------------------
(* THREAD/PROCESS STATES                                                      *)
(* Model the state of each concurrent thread accessing the ARTrie.            *)
--------------------------------------------------------------------------------

ThreadState == {
    "Idle",
    "Reading",
    "Writing",
    "Validating",
    "Waiting",
    "Crashed"
}

\* Thread context record
ThreadContext == [
    state        : ThreadState,
    currentNode  : NodeIds \cup {Null},
    heldVersion  : Nat,
    operationType: {"None", "Lookup", "Insert", "Remove"},
    targetKey    : Keys \cup {Null},
    targetValue  : Values \cup {Null}
]

--------------------------------------------------------------------------------
(* BUCKET STRUCTURE                                                           *)
(* Buckets are B-trie leaf nodes with sorted entries and binary search.       *)
--------------------------------------------------------------------------------

BUCKET_PAGE_SIZE == 8192
MAX_BUCKET_ENTRIES == 256
BUCKET_HEADER_SIZE == 32
BUCKET_ENTRY_SIZE == 8

\* A bucket entry (suffix + value)
BucketEntry == [
    suffix : Seq(0..255),
    value  : Values \cup {Null}
]

\* Bucket record
Bucket == [
    entryCount: 0..MAX_BUCKET_ENTRIES,
    entries   : Seq(BucketEntry),
    dataSize  : 0..BUCKET_PAGE_SIZE,
    freeSpace : 0..BUCKET_PAGE_SIZE
]

--------------------------------------------------------------------------------
(* ABSTRACT NODE STRUCTURE                                                    *)
(* Unified representation of all node types for specification purposes.       *)
--------------------------------------------------------------------------------

\* Abstract node combining all node type information
AbstractNode == [
    header     : NodeHeader,
    prefix     : CompressedPrefix,
    \* Children mapping: byte -> SwizzledPtr
    children   : [0..255 -> SwizzledPtr],
    \* For buckets: the bucket data
    bucketData : Bucket \cup {<<>>}
]

--------------------------------------------------------------------------------
(* RESULT TYPES                                                               *)
(* Standard result types for operations.                                      *)
--------------------------------------------------------------------------------

OperationResult == {"Success", "NotFound", "Conflict", "Full", "InvalidVersion", "CrashError"}

LookupResult == [
    status : OperationResult,
    value  : Values \cup {Null}
]

InsertResult == [
    status : OperationResult,
    \* For upsert: previous value if existed
    prevValue : Values \cup {Null}
]

RemoveResult == [
    status : OperationResult,
    removedValue : Values \cup {Null}
]

--------------------------------------------------------------------------------
(* HELPER OPERATORS                                                           *)
--------------------------------------------------------------------------------

\* Check if a node needs to grow
NeedsGrow(node) ==
    node.header.numChildren >= NodeCapacity[node.header.nodeType]

\* Check if a node can shrink
CanShrink(node) ==
    /\ node.header.nodeType # "Node4"
    /\ node.header.numChildren <= NodeMinChildren[node.header.nodeType]

\* Get the set of valid thread IDs
Threads == 1..NumThreads

\* Create empty children mapping
EmptyChildren == [b \in 0..255 |-> NullPtr]

\* Create empty bucket
EmptyBucket == [
    entryCount |-> 0,
    entries    |-> <<>>,
    dataSize   |-> BUCKET_HEADER_SIZE,
    freeSpace  |-> BUCKET_PAGE_SIZE - BUCKET_HEADER_SIZE
]

\* Check if a pointer is null
IsNull(ptr) == ptr.state = "Null"

\* Check if a pointer is in memory
IsInMemory(ptr) == ptr.state = "InMemory"

\* Check if a pointer is on disk
IsOnDisk(ptr) == ptr.state = "OnDisk"

================================================================================
(* LICENSE: MIT License                                                       *)
(* Copyright (c) 2026 F1r3fly.io                                              *)
================================================================================
