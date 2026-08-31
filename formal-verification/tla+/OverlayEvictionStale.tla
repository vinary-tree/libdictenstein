-------------------------- MODULE OverlayEvictionStale --------------------------
(***************************************************************************)
(* Exact durable-image provenance for overlay eviction and fault-in.        *)
(*                                                                         *)
(* A semantic value version and a durable image identity are deliberately   *)
(* different dimensions. `imageVersion[n][i]` records the immutable value   *)
(* stored by image i; `durableImage[n]` is the registry image currently      *)
(* authorized for n; and `liveStamp[n]` is provenance carried by a live      *)
(* in-memory node. Eviction compares image identities, not merely equal      *)
(* value versions. A path-copy write publishes fresh stamp-zero nodes.       *)
(*                                                                         *)
(* Fault decoding is split into preparation and root CAS. Preparation owns   *)
(* a private candidate containing image, value, stamp, and captured root.    *)
(* A winner publishes that exact candidate. A loser drops it without         *)
(* changing any published state. This represents both the byte and char      *)
(* fault paths and makes the loser-safety obligation explicit.               *)
(*                                                                         *)
(* USE_EVICTION_GUARD = FALSE is the historical stale-eviction negative      *)
(* control. USE_FAULT_STAMP = FALSE is the exact-fault-stamp negative         *)
(* control: a fault winner becomes resident without provenance and therefore *)
(* cannot safely participate in another eviction cycle.                      *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets

CONSTANTS
    Nodes,
    USE_EVICTION_GUARD,
    USE_FAULT_STAMP,
    MaxVer,
    MaxImage,
    MaxRoot

NoImage == 0
ImageIds == 0..MaxImage
FaultPhases == {"Idle", "Prepared"}

VARIABLES
    root,             \* monotone published-root revision
    linkedInMem,      \* nodes reachable as live in-memory boxes
    onDisk,           \* nodes whose published slot is an OnDisk reference
    liveVersion,      \* last published semantic/structural version
    ackedVersion,     \* latest acknowledged semantic/structural version
    durableImage,     \* exact registry image identity for each node
    imageVersion,     \* immutable value version stored by each image identity
    liveStamp,        \* exact durable image named by the live node; 0 = fresh copy
    evictedImage,     \* image identity currently named by an OnDisk slot
    pathCopied,       \* live node was created by path copying since checkpoint
    faultInstalled,   \* live node was last installed by a successful fault CAS
    faultPhase,       \* per-node private fault-candidate lifecycle
    faultImage,       \* private candidate's decoded image identity
    faultVersion,     \* private candidate's decoded value version
    faultStamp,       \* private candidate's proposed live provenance
    faultRoot         \* root revision captured before decoding

Vars ==
    <<root, linkedInMem, onDisk, liveVersion, ackedVersion, durableImage,
      imageVersion, liveStamp, evictedImage, pathCopied, faultInstalled,
      faultPhase, faultImage, faultVersion, faultStamp, faultRoot>>

AllZero == [n \in Nodes |-> 0]
AllFalse == [n \in Nodes |-> FALSE]
EmptyImageTable == [n \in Nodes |-> [i \in ImageIds |-> 0]]

TypeInvariant ==
    /\ root \in Nat
    /\ linkedInMem \subseteq Nodes
    /\ onDisk \subseteq Nodes
    /\ liveVersion \in [Nodes -> 0..MaxVer]
    /\ ackedVersion \in [Nodes -> 0..MaxVer]
    /\ durableImage \in [Nodes -> ImageIds]
    /\ imageVersion \in [Nodes -> [ImageIds -> 0..MaxVer]]
    /\ liveStamp \in [Nodes -> ImageIds]
    /\ evictedImage \in [Nodes -> ImageIds]
    /\ pathCopied \in [Nodes -> BOOLEAN]
    /\ faultInstalled \in [Nodes -> BOOLEAN]
    /\ faultPhase \in [Nodes -> FaultPhases]
    /\ faultImage \in [Nodes -> ImageIds]
    /\ faultVersion \in [Nodes -> 0..MaxVer]
    /\ faultStamp \in [Nodes -> ImageIds]
    /\ faultRoot \in [Nodes -> Nat]

Init ==
    /\ root = 1
    /\ linkedInMem = {}
    /\ onDisk = {}
    /\ liveVersion = AllZero
    /\ ackedVersion = AllZero
    /\ durableImage = AllZero
    /\ imageVersion = EmptyImageTable
    /\ liveStamp = AllZero
    /\ evictedImage = AllZero
    /\ pathCopied = AllFalse
    /\ faultInstalled = AllFalse
    /\ faultPhase = [n \in Nodes |-> "Idle"]
    /\ faultImage = AllZero
    /\ faultVersion = AllZero
    /\ faultStamp = AllZero
    /\ faultRoot = AllZero

(***************************************************************************)
(* PathCopy(copied): a successful semantic/root CAS rebuilds a nonempty      *)
(* spine. Every copied node receives a fresh version and stamp zero. The     *)
(* bounded model lets the environment choose the spine; safety depends only  *)
(* on clearing provenance for every copied node, not on a particular trie    *)
(* shape. A write may race a prepared fault and thereby force its CAS loser.  *)
(***************************************************************************)
PathCopy(copied) ==
    /\ copied \in SUBSET Nodes
    /\ copied # {}
    /\ \A n \in copied : liveVersion[n] < MaxVer
    /\ root < MaxRoot
    /\ root' = root + 1
    /\ linkedInMem' = linkedInMem \cup copied
    /\ onDisk' = onDisk \ copied
    /\ liveVersion' =
         [n \in Nodes |->
            IF n \in copied THEN liveVersion[n] + 1 ELSE liveVersion[n]]
    /\ ackedVersion' =
         [n \in Nodes |->
            IF n \in copied THEN liveVersion[n] + 1 ELSE ackedVersion[n]]
    /\ liveStamp' =
         [n \in Nodes |-> IF n \in copied THEN NoImage ELSE liveStamp[n]]
    /\ pathCopied' =
         [n \in Nodes |-> IF n \in copied THEN TRUE ELSE pathCopied[n]]
    /\ faultInstalled' =
         [n \in Nodes |-> IF n \in copied THEN FALSE ELSE faultInstalled[n]]
    /\ UNCHANGED <<durableImage, imageVersion, evictedImage, faultPhase,
                    faultImage, faultVersion, faultStamp, faultRoot>>

(***************************************************************************)
(* Checkpoint(n) creates a fresh immutable image, records its value version,  *)
(* and gives the still-live node that exact image stamp. Image identity is    *)
(* never inferred from value equality.                                       *)
(***************************************************************************)
Checkpoint(n) ==
    /\ n \in linkedInMem
    /\ durableImage[n] < MaxImage
    /\ durableImage' =
         [durableImage EXCEPT ![n] = durableImage[n] + 1]
    /\ imageVersion' =
         [imageVersion EXCEPT
            ![n][durableImage[n] + 1] = liveVersion[n]]
    /\ liveStamp' =
         [liveStamp EXCEPT ![n] = durableImage[n] + 1]
    /\ pathCopied' = [pathCopied EXCEPT ![n] = FALSE]
    /\ UNCHANGED <<root, linkedInMem, onDisk, liveVersion, ackedVersion,
                    evictedImage, faultInstalled, faultPhase, faultImage,
                    faultVersion, faultStamp, faultRoot>>

(***************************************************************************)
(* Evict(n) publishes OnDisk(durableImage[n]). The exact guard compares the  *)
(* live provenance stamp with that image identity. Destroying the in-memory  *)
(* box destroys its live stamp. The numeric version is retained as the       *)
(* monotone base for a later write while the slot is OnDisk; it is not        *)
(* provenance and cannot authorize eviction.                                 *)
(***************************************************************************)
Evict(n) ==
    /\ n \in linkedInMem
    /\ durableImage[n] > NoImage
    /\ (USE_EVICTION_GUARD => liveStamp[n] = durableImage[n])
    /\ root < MaxRoot
    /\ root' = root + 1
    /\ linkedInMem' = linkedInMem \ {n}
    /\ onDisk' = onDisk \cup {n}
    /\ liveStamp' = [liveStamp EXCEPT ![n] = NoImage]
    /\ evictedImage' = [evictedImage EXCEPT ![n] = durableImage[n]]
    /\ pathCopied' = [pathCopied EXCEPT ![n] = FALSE]
    /\ faultInstalled' = [faultInstalled EXCEPT ![n] = FALSE]
    /\ UNCHANGED <<liveVersion, ackedVersion, durableImage, imageVersion,
                    faultPhase, faultImage, faultVersion, faultStamp,
                    faultRoot>>

(***************************************************************************)
(* FaultPrepare(n) decodes into private storage. Neither reachability nor the *)
(* root changes. The safe decoder stamps the top node with the exact source   *)
(* image for compressed and uncompressed records alike.                      *)
(***************************************************************************)
FaultPrepare(n) ==
    /\ faultPhase[n] = "Idle"
    /\ n \in onDisk
    /\ evictedImage[n] > NoImage
    /\ faultPhase' = [faultPhase EXCEPT ![n] = "Prepared"]
    /\ faultImage' = [faultImage EXCEPT ![n] = evictedImage[n]]
    /\ faultVersion' =
         [faultVersion EXCEPT
            ![n] = imageVersion[n][evictedImage[n]]]
    /\ faultStamp' =
         [faultStamp EXCEPT
            ![n] = IF USE_FAULT_STAMP THEN evictedImage[n] ELSE NoImage]
    /\ faultRoot' = [faultRoot EXCEPT ![n] = root]
    /\ UNCHANGED <<root, linkedInMem, onDisk, liveVersion, ackedVersion,
                    durableImage, imageVersion, liveStamp, evictedImage,
                    pathCopied, faultInstalled>>

(***************************************************************************)
(* FaultCasWin(n) publishes exactly the privately decoded candidate only if  *)
(* the captured root and OnDisk image are still current.                      *)
(***************************************************************************)
FaultCasWin(n) ==
    /\ faultPhase[n] = "Prepared"
    /\ faultRoot[n] = root
    /\ n \in onDisk
    /\ faultImage[n] = evictedImage[n]
    /\ root < MaxRoot
    /\ root' = root + 1
    /\ linkedInMem' = linkedInMem \cup {n}
    /\ onDisk' = onDisk \ {n}
    /\ liveVersion' = [liveVersion EXCEPT ![n] = faultVersion[n]]
    /\ liveStamp' = [liveStamp EXCEPT ![n] = faultStamp[n]]
    /\ pathCopied' = [pathCopied EXCEPT ![n] = FALSE]
    /\ faultInstalled' = [faultInstalled EXCEPT ![n] = TRUE]
    /\ faultPhase' = [faultPhase EXCEPT ![n] = "Idle"]
    /\ faultImage' = [faultImage EXCEPT ![n] = NoImage]
    /\ faultVersion' = [faultVersion EXCEPT ![n] = 0]
    /\ faultStamp' = [faultStamp EXCEPT ![n] = NoImage]
    /\ faultRoot' = [faultRoot EXCEPT ![n] = 0]
    /\ UNCHANGED <<ackedVersion, durableImage, imageVersion, evictedImage>>

(***************************************************************************)
(* FaultCasLose(n) releases only private candidate state. A competing path   *)
(* copy, eviction/fault, or other root publication can make the snapshot      *)
(* stale; none permits the loser to overwrite the published root or slot.     *)
(***************************************************************************)
FaultCasLose(n) ==
    /\ faultPhase[n] = "Prepared"
    /\ \/ faultRoot[n] # root
       \/ n \notin onDisk
       \/ faultImage[n] # evictedImage[n]
    /\ faultPhase' = [faultPhase EXCEPT ![n] = "Idle"]
    /\ faultImage' = [faultImage EXCEPT ![n] = NoImage]
    /\ faultVersion' = [faultVersion EXCEPT ![n] = 0]
    /\ faultStamp' = [faultStamp EXCEPT ![n] = NoImage]
    /\ faultRoot' = [faultRoot EXCEPT ![n] = 0]
    /\ UNCHANGED <<root, linkedInMem, onDisk, liveVersion, ackedVersion,
                    durableImage, imageVersion, liveStamp, evictedImage,
                    pathCopied, faultInstalled>>

Next ==
    \/ \E copied \in SUBSET Nodes : PathCopy(copied)
    \/ \E n \in Nodes : Checkpoint(n)
    \/ \E n \in Nodes : Evict(n)
    \/ \E n \in Nodes : FaultPrepare(n)
    \/ \E n \in Nodes : FaultCasWin(n)
    \/ \E n \in Nodes : FaultCasLose(n)

Spec == Init /\ [][Next]_Vars

RootBound == root <= MaxRoot

(* ------------------------------ Invariants ----------------------------- *)

LinkedAndOnDiskDisjoint == linkedInMem \cap onDisk = {}

DurableImagesAreInitialized ==
    \A n \in Nodes :
        durableImage[n] > NoImage =>
            imageVersion[n][durableImage[n]] > 0

OnDiskSlotsNameInitializedImages ==
    \A n \in onDisk :
        /\ evictedImage[n] > NoImage
        /\ imageVersion[n][evictedImage[n]] > 0

LiveStampIsHonest ==
    \A n \in linkedInMem :
        liveStamp[n] > NoImage =>
            imageVersion[n][liveStamp[n]] = liveVersion[n]

PathCopiesCarryNoStamp ==
    \A n \in Nodes :
        (n \in linkedInMem /\ pathCopied[n]) => liveStamp[n] = NoImage

PrivateFaultCandidateMatchesImage ==
    \A n \in Nodes :
        faultPhase[n] = "Prepared" =>
            /\ faultImage[n] > NoImage
            /\ faultVersion[n] = imageVersion[n][faultImage[n]]

FaultInstalledCarriesExactStamp ==
    \A n \in Nodes :
        faultInstalled[n] =>
            /\ n \in linkedInMem
            /\ liveStamp[n] = durableImage[n]
            /\ liveVersion[n] = imageVersion[n][durableImage[n]]

\* Every acknowledged version remains observable either directly in memory or
\* through the exact image named by its OnDisk slot.
NoStaleEvict ==
    \A n \in Nodes :
        ackedVersion[n] > 0 =>
            \/ (n \in linkedInMem /\ liveVersion[n] = ackedVersion[n])
            \/ (n \in onDisk /\
                  imageVersion[n][evictedImage[n]] = ackedVersion[n])

\* Action-level correspondence obligations make private-candidate ownership
\* and path-copy stamp clearing explicit rather than relying on prose.
FaultLoserPublishesNothing ==
    [][\A n \in Nodes :
        FaultCasLose(n) =>
            /\ root' = root
            /\ linkedInMem' = linkedInMem
            /\ onDisk' = onDisk
            /\ liveVersion' = liveVersion
            /\ liveStamp' = liveStamp
            /\ durableImage' = durableImage
            /\ imageVersion' = imageVersion
            /\ evictedImage' = evictedImage]_Vars

FaultWinnerPublishesPrivateCandidate ==
    [][\A n \in Nodes :
        FaultCasWin(n) =>
            /\ n \in linkedInMem'
            /\ n \notin onDisk'
            /\ liveVersion'[n] = faultVersion[n]
            /\ liveStamp'[n] = faultStamp[n]]_Vars

PathCopyClearsProvenance ==
    [][\A copied \in SUBSET Nodes :
        PathCopy(copied) =>
            \A n \in copied :
                /\ n \in linkedInMem'
                /\ liveStamp'[n] = NoImage
                /\ pathCopied'[n]]_Vars
=============================================================================
