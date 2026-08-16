-------------------------- MODULE AbiProducerSnapshot --------------------------
(***************************************************************************)
(* Producer half of the vt.dictionary.v1 capture protocol (family FV       *)
(* obligation #10; wave W2).                                               *)
(*                                                                         *)
(* Models src/bindings.rs: a mutable store holds an atomically published   *)
(* current revision and its exact cardinality as one atomic pair; writers *)
(* publish path-copied successors (insert /                               *)
(* remove / clear), and compact / checkpoint publish content-preserving    *)
(* successors; `snapshot()` captures the revision visible at call time in  *)
(* O(1) by pinning it, and every read through a captured resource is       *)
(* served from the pinned revision no matter what publishes follow.        *)
(*                                                                         *)
(* USE_IMMUTABLE_CAPTURE is the design choice this model exists to         *)
(* justify: TRUE (AbiProducerSnapshot.cfg, must pass) pins the captured    *)
(* revision; FALSE (AbiProducerSnapshot_Unsafe.cfg, must FAIL              *)
(* CapturedRevisionImmutable) aliases the live head — the rejected design  *)
(* where a snapshot is a pointer into the mutable store, so any            *)
(* post-capture publish rewrites what an ABI consumer already captured.    *)
(*                                                                         *)
(* Invariants (registry: formal-verification/ABI_INVARIANTS.tsv):          *)
(*   LDICT-SNAP-1  CapturedRevisionImmutable — reads through a capture     *)
(*                 equal the contents at capture time, always.             *)
(*   LDICT-SNAP-2  FreshCaptureSeesHead — a new capture observes exactly   *)
(*                 the currently published contents.                        *)
(*   LDICT-SNAP-3  ContentPreservingPublishes — compact / checkpoint       *)
(*                 never change the published term set (action property).  *)
(*   LDICT-SNAP-6  PublishedCountCoherent — the live root and exact count  *)
(*                 always describe the same revision.                     *)
(*   LDICT-SNAP-7  CapturedCountCoherent — a snapshot pins both the root   *)
(*                 and count from one publication.                        *)
(*   PublishesMonotone — the published version strictly increases          *)
(*                 (action property; supports the atomic-publication       *)
(*                 reading of the Rust Arc swap).                          *)
(*                                                                         *)
(* The O(1) cost of capture is not a TLA+ statement: it is pinned by the   *)
(* zero-callback metrics assertion in the Rust correspondence tests and    *)
(* by the wave-W8 snapshot-cost benchmark (flat curve over dictionary      *)
(* size).                                                                  *)
(*                                                                         *)
(* Companion Rust tests: tests/ffi_snapshot_law.rs and                     *)
(* tests/query_start_snapshot_correspondence.rs (consumer-visible law),    *)
(* liblevenshtein-rust/tests/abi_resource_lifecycle_correspondence.rs      *)
(* (lifetime half, owned by the interop lifecycle model).                  *)
(***************************************************************************)
EXTENDS Integers, FiniteSets, Sequences, TLC

CONSTANTS
  Terms,                 \* small universe of terms, e.g. {t1, t2, t3}
  MaxPublishes,          \* bound on publish steps (mutations + maintenance)
  MaxCaptures,           \* bound on concurrently tracked captures
  USE_IMMUTABLE_CAPTURE  \* TRUE = pin the captured revision (the design)

ASSUME MaxPublishes \in Nat
ASSUME MaxCaptures \in Nat \ {0}
ASSUME USE_IMMUTABLE_CAPTURE \in BOOLEAN

VARIABLES
  version,      \* currently published revision number
  head,         \* currently published contents: the set of stored terms
  headCount,    \* exact count atomically published with head
  history,      \* function: published version -> its contents
  historyCounts,\* function: published version -> its exact count
  captures,     \* function: capture id -> the version it pinned
  expected,     \* function: capture id -> contents at capture time (oracle)
  capturedCounts, \* function: capture id -> count read from pinned revision
  expectedCounts, \* function: capture id -> cardinality oracle at capture
  publishesLeft \* remaining publish budget

vars == <<version, head, headCount, history, historyCounts, captures,
          expected, capturedCounts, expectedCounts, publishesLeft>>

CaptureIds == DOMAIN captures

Init ==
  /\ version = 0
  /\ head = {}
  /\ headCount = 0
  /\ history = (0 :> {})
  /\ historyCounts = (0 :> 0)
  /\ captures = <<>>
  /\ expected = <<>>
  /\ capturedCounts = <<>>
  /\ expectedCounts = <<>>
  /\ publishesLeft = MaxPublishes

(***************************************************************************)
(* Publishing. Every publish creates version+1 and extends history; the    *)
(* old revisions are never edited in place — that is the path-copy design. *)
(***************************************************************************)
Publish(newContents) ==
  /\ publishesLeft > 0
  /\ version' = version + 1
  /\ head' = newContents
  /\ headCount' = Cardinality(newContents)
  /\ history' = history @@ (version + 1 :> newContents)
  /\ historyCounts' = historyCounts @@
                         (version + 1 :> Cardinality(newContents))
  /\ publishesLeft' = publishesLeft - 1
  /\ UNCHANGED <<captures, expected, capturedCounts, expectedCounts>>

Insert(term) ==
  /\ term \notin head
  /\ Publish(head \cup {term})

Remove(term) ==
  /\ term \in head
  /\ Publish(head \ {term})

Clear ==
  /\ head # {}
  /\ Publish({})

\* Compact and checkpoint publish a NEW revision with IDENTICAL contents
\* (in-memory compaction re-lays storage; checkpoint persists) — the model
\* keeps them distinct actions so the action property names both.
Compact == Publish(head)

Checkpoint == Publish(head)

(***************************************************************************)
(* Capturing. The safe design pins the current version; the unsafe design  *)
(* records a live alias (modeled as pinning the special marker -1, whose   *)
(* reads always follow the head).                                          *)
(***************************************************************************)
LiveAlias == -1

Capture ==
  /\ Len(captures) < MaxCaptures
  /\ captures' = Append(captures, IF USE_IMMUTABLE_CAPTURE THEN version ELSE LiveAlias)
  /\ expected' = Append(expected, head)
  /\ capturedCounts' = Append(capturedCounts,
       IF USE_IMMUTABLE_CAPTURE THEN historyCounts[version] ELSE headCount)
  /\ expectedCounts' = Append(expectedCounts, Cardinality(head))
  /\ UNCHANGED <<version, head, headCount, history, historyCounts,
                  publishesLeft>>

\* What a read through capture i returns under the current state.
ReadBack(i) ==
  IF captures[i] = LiveAlias THEN head ELSE history[captures[i]]

ReadCount(i) ==
  IF captures[i] = LiveAlias THEN headCount ELSE capturedCounts[i]

Next ==
  \/ \E term \in Terms : Insert(term)
  \/ \E term \in Terms : Remove(term)
  \/ Clear
  \/ Compact
  \/ Checkpoint
  \/ Capture

Spec == Init /\ [][Next]_vars

(***************************************************************************)
(* Invariants                                                              *)
(***************************************************************************)
TypeOK ==
  /\ version \in 0..MaxPublishes
  /\ head \subseteq Terms
  /\ headCount \in 0..Cardinality(Terms)
  /\ DOMAIN history = 0..version
  /\ DOMAIN historyCounts = 0..version
  /\ \A v \in DOMAIN history : history[v] \subseteq Terms
  /\ \A v \in DOMAIN historyCounts : historyCounts[v] \in 0..Cardinality(Terms)
  /\ Len(captures) = Len(expected)
  /\ Len(captures) = Len(capturedCounts)
  /\ Len(captures) = Len(expectedCounts)
  /\ Len(captures) <= MaxCaptures
  /\ \A i \in 1..Len(captures) : captures[i] \in (0..version) \cup {LiveAlias}
  /\ publishesLeft \in 0..MaxPublishes

\* LDICT-SNAP-1: every capture reads back exactly what was published when
\* it was taken, under every subsequent interleaving of publishes.
CapturedRevisionImmutable ==
  \A i \in 1..Len(captures) : ReadBack(i) = expected[i]

\* LDICT-SNAP-2: the newest capture (taken this instant) equals the head —
\* stated over all captures whose pinned version IS the current version.
FreshCaptureSeesHead ==
  \A i \in 1..Len(captures) : captures[i] = version => ReadBack(i) = head

\* LDICT-SNAP-6: root and count are one atomic published value.
PublishedCountCoherent ==
  /\ headCount = Cardinality(head)
  /\ \A v \in DOMAIN history : historyCounts[v] = Cardinality(history[v])

\* LDICT-SNAP-7: the count captured beside a root is exact for that root and
\* remains exact after any later publication.
CapturedCountCoherent ==
  \A i \in 1..Len(captures) :
    /\ ReadCount(i) = expectedCounts[i]
    /\ ReadCount(i) = Cardinality(ReadBack(i))

\* LDICT-SNAP-3 (action property): compact and checkpoint preserve contents.
ContentPreservingPublishes ==
  [][ (Compact \/ Checkpoint) => head' = head ]_vars

\* Action property: publication is monotone (the atomic root swap).
PublishesMonotone ==
  [][ version' > version \/ UNCHANGED version ]_vars

=============================================================================
