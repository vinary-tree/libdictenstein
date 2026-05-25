-------------------- MODULE ConcurrentVocabLinearizability --------------------
(****************************************************************************)
(* Bounded public-operation linearizability model for ConcurrentVocabARTrie.  *)
(*                                                                          *)
(* Scope: insert, read, fixed two-term batch insert, checkpoint, and          *)
(* crash/reopen after checkpoint/flush publication. The model records start   *)
(* and finish times plus a ghost linearization order, then checks that the    *)
(* completed history has a sequential vocabulary-map explanation respecting   *)
(* real-time order.                                                          *)
(****************************************************************************)

EXTENDS Naturals, FiniteSets, Sequences, TLC

CONSTANTS Writers, Terms, None, MaxIndex, MaxOps, BatchFirst, BatchSecond

VARIABLES nextIndex, visible, checkpointed, walLive,
          nextOp, clock, phase, activeId,
          idKind, idArg, idRet1, idRet2, idRetMap, idStart, idFinish,
          linIds, history, gate

Vars == <<nextIndex, visible, checkpointed, walLive,
          nextOp, clock, phase, activeId,
          idKind, idArg, idRet1, idRet2, idRetMap, idStart, idFinish,
          linIds, history, gate>>

OpKinds == {"Insert", "Read", "Batch", "Checkpoint", "Recover", "None"}
Phases == {"Idle", "Started", "Linearized"}
GateStates == {"Open", "Checkpoint"}
OpIds == 1..MaxOps
Indexes == 0..MaxIndex
IndexOrNone == Indexes \cup {None}
EmptyIndexMap == [k \in Terms |-> None]
EmptyWalMap == [k \in Terms |-> FALSE]
EmptyIdKind == [id \in OpIds |-> "None"]
EmptyIdArg == [id \in OpIds |-> None]
EmptyIdRet == [id \in OpIds |-> None]
EmptyIdRetMap == [id \in OpIds |-> EmptyIndexMap]
EmptyIdClock == [id \in OpIds |-> 0]

RecoveredMap(v, c, wal) ==
    [k \in Terms |->
        IF c[k] # None
        THEN c[k]
        ELSE IF wal[k] THEN v[k] ELSE None]

RecordAt(id) ==
    [ kind |-> idKind[id],
      arg |-> idArg[id],
      ret1 |-> idRet1[id],
      ret2 |-> idRet2[id],
      retMap |-> idRetMap[id] ]

LinRecordSeq ==
    [i \in DOMAIN linIds |-> RecordAt(linIds[i])]

RECURSIVE SeqOK(_, _, _)

SeqOK(seq, v, n) ==
    IF Len(seq) = 0
    THEN TRUE
    ELSE
        LET r == Head(seq) IN
        LET rest == Tail(seq) IN
        CASE r.kind = "Insert" ->
            IF v[r.arg] = None
            THEN /\ r.ret1 = n
                 /\ SeqOK(rest, [v EXCEPT ![r.arg] = n], n + 1)
            ELSE /\ r.ret1 = v[r.arg]
                 /\ SeqOK(rest, v, n)
        [] r.kind = "Read" ->
            /\ r.ret1 = v[r.arg]
            /\ SeqOK(rest, v, n)
        [] r.kind = "Batch" ->
            LET firstIdx == IF v[BatchFirst] = None THEN n ELSE v[BatchFirst] IN
            LET v1 == IF v[BatchFirst] = None
                      THEN [v EXCEPT ![BatchFirst] = n]
                      ELSE v IN
            LET n1 == IF v[BatchFirst] = None THEN n + 1 ELSE n IN
            LET secondIdx == IF v1[BatchSecond] = None THEN n1 ELSE v1[BatchSecond] IN
            LET v2 == IF v1[BatchSecond] = None
                      THEN [v1 EXCEPT ![BatchSecond] = n1]
                      ELSE v1 IN
            LET n2 == IF v1[BatchSecond] = None THEN n1 + 1 ELSE n1 IN
                /\ r.ret1 = firstIdx
                /\ r.ret2 = secondIdx
                /\ SeqOK(rest, v2, n2)
        [] r.kind = "Checkpoint" ->
            SeqOK(rest, v, n)
        [] r.kind = "Recover" ->
            /\ r.retMap = v
            /\ SeqOK(rest, v, n)
        [] OTHER -> FALSE

TypeInvariant ==
    /\ None \notin Terms
    /\ None \notin Indexes
    /\ BatchFirst \in Terms
    /\ BatchSecond \in Terms
    /\ BatchFirst # BatchSecond
    /\ nextIndex \in 0..(MaxIndex + 1)
    /\ visible \in [Terms -> IndexOrNone]
    /\ checkpointed \in [Terms -> IndexOrNone]
    /\ walLive \in [Terms -> BOOLEAN]
    /\ nextOp \in 1..(MaxOps + 1)
    /\ clock \in 1..((2 * MaxOps) + 1)
    /\ phase \in [Writers -> Phases]
    /\ activeId \in [Writers -> OpIds \cup {None}]
    /\ idKind \in [OpIds -> OpKinds]
    /\ idArg \in [OpIds -> Terms \cup {None}]
    /\ idRet1 \in [OpIds -> IndexOrNone]
    /\ idRet2 \in [OpIds -> IndexOrNone]
    /\ idRetMap \in [OpIds -> [Terms -> IndexOrNone]]
    /\ idStart \in [OpIds -> 0..(2 * MaxOps)]
    /\ idFinish \in [OpIds -> 0..(2 * MaxOps)]
    /\ linIds \in Seq(OpIds)
    /\ history \in Seq(OpIds)
    /\ gate \in GateStates

Init ==
    /\ nextIndex = 0
    /\ visible = EmptyIndexMap
    /\ checkpointed = EmptyIndexMap
    /\ walLive = EmptyWalMap
    /\ nextOp = 1
    /\ clock = 1
    /\ phase = [w \in Writers |-> "Idle"]
    /\ activeId = [w \in Writers |-> None]
    /\ idKind = EmptyIdKind
    /\ idArg = EmptyIdArg
    /\ idRet1 = EmptyIdRet
    /\ idRet2 = EmptyIdRet
    /\ idRetMap = EmptyIdRetMap
    /\ idStart = EmptyIdClock
    /\ idFinish = EmptyIdClock
    /\ linIds = <<>>
    /\ history = <<>>
    /\ gate = "Open"

StartOp(w, kind, arg) ==
    /\ w \in Writers
    /\ phase[w] = "Idle"
    /\ gate = "Open"
    /\ nextOp <= MaxOps
    /\ clock <= 2 * MaxOps
    /\ phase' = [phase EXCEPT ![w] = "Started"]
    /\ activeId' = [activeId EXCEPT ![w] = nextOp]
    /\ idKind' = [idKind EXCEPT ![nextOp] = kind]
    /\ idArg' = [idArg EXCEPT ![nextOp] = arg]
    /\ idStart' = [idStart EXCEPT ![nextOp] = clock]
    /\ nextOp' = nextOp + 1
    /\ clock' = clock + 1
    /\ UNCHANGED <<nextIndex, visible, checkpointed, walLive,
                  idRet1, idRet2, idRetMap, idFinish, linIds, history, gate>>

StartCheckpoint(w) ==
    /\ w \in Writers
    /\ phase[w] = "Idle"
    /\ gate = "Open"
    /\ nextOp <= MaxOps
    /\ clock <= 2 * MaxOps
    /\ \A other \in Writers : phase[other] = "Idle"
    /\ phase' = [phase EXCEPT ![w] = "Started"]
    /\ activeId' = [activeId EXCEPT ![w] = nextOp]
    /\ idKind' = [idKind EXCEPT ![nextOp] = "Checkpoint"]
    /\ idArg' = [idArg EXCEPT ![nextOp] = None]
    /\ idStart' = [idStart EXCEPT ![nextOp] = clock]
    /\ nextOp' = nextOp + 1
    /\ clock' = clock + 1
    /\ gate' = "Checkpoint"
    /\ UNCHANGED <<nextIndex, visible, checkpointed, walLive,
                  idRet1, idRet2, idRetMap, idFinish, linIds, history>>

StartRecover(w) ==
    /\ w \in Writers
    /\ phase[w] = "Idle"
    /\ gate = "Open"
    /\ nextOp <= MaxOps
    /\ clock <= 2 * MaxOps
    /\ \A other \in Writers : phase[other] = "Idle"
    /\ StartOp(w, "Recover", None)

LinearizeInsert(w) ==
    /\ phase[w] = "Started"
    /\ activeId[w] \in OpIds
    /\ idKind[activeId[w]] = "Insert"
    /\ idArg[activeId[w]] \in Terms
    /\ LET id == activeId[w] IN
       LET k == idArg[id] IN
       IF visible[k] = None
       THEN /\ nextIndex <= MaxIndex
            /\ idRet1' = [idRet1 EXCEPT ![id] = nextIndex]
            /\ visible' = [visible EXCEPT ![k] = nextIndex]
            /\ walLive' = [walLive EXCEPT ![k] = TRUE]
            /\ nextIndex' = nextIndex + 1
       ELSE /\ idRet1' = [idRet1 EXCEPT ![id] = visible[k]]
            /\ visible' = visible
            /\ walLive' = walLive
            /\ nextIndex' = nextIndex
    /\ idRet2' = idRet2
    /\ idRetMap' = idRetMap
    /\ phase' = [phase EXCEPT ![w] = "Linearized"]
    /\ linIds' = Append(linIds, activeId[w])
    /\ UNCHANGED <<checkpointed, nextOp, clock, activeId, idKind, idArg,
                  idStart, idFinish, history, gate>>

LinearizeRead(w) ==
    /\ phase[w] = "Started"
    /\ activeId[w] \in OpIds
    /\ idKind[activeId[w]] = "Read"
    /\ idArg[activeId[w]] \in Terms
    /\ idRet1' = [idRet1 EXCEPT ![activeId[w]] = visible[idArg[activeId[w]]]]
    /\ phase' = [phase EXCEPT ![w] = "Linearized"]
    /\ linIds' = Append(linIds, activeId[w])
    /\ UNCHANGED <<nextIndex, visible, checkpointed, walLive,
                  nextOp, clock, activeId, idKind, idArg, idRet2, idRetMap,
                  idStart, idFinish, history, gate>>

LinearizeBatch(w) ==
    /\ phase[w] = "Started"
    /\ activeId[w] \in OpIds
    /\ idKind[activeId[w]] = "Batch"
    /\ LET id == activeId[w] IN
       LET firstIdx == IF visible[BatchFirst] = None THEN nextIndex ELSE visible[BatchFirst] IN
       LET v1 == IF visible[BatchFirst] = None
                 THEN [visible EXCEPT ![BatchFirst] = nextIndex]
                 ELSE visible IN
       LET w1 == IF visible[BatchFirst] = None
                 THEN [walLive EXCEPT ![BatchFirst] = TRUE]
                 ELSE walLive IN
       LET n1 == IF visible[BatchFirst] = None THEN nextIndex + 1 ELSE nextIndex IN
       LET secondIdx == IF v1[BatchSecond] = None THEN n1 ELSE v1[BatchSecond] IN
       LET v2 == IF v1[BatchSecond] = None
                 THEN [v1 EXCEPT ![BatchSecond] = n1]
                 ELSE v1 IN
       LET w2 == IF v1[BatchSecond] = None
                 THEN [w1 EXCEPT ![BatchSecond] = TRUE]
                 ELSE w1 IN
       LET n2 == IF v1[BatchSecond] = None THEN n1 + 1 ELSE n1 IN
            /\ n2 <= MaxIndex + 1
            /\ idRet1' = [idRet1 EXCEPT ![id] = firstIdx]
            /\ idRet2' = [idRet2 EXCEPT ![id] = secondIdx]
            /\ visible' = v2
            /\ walLive' = w2
            /\ nextIndex' = n2
    /\ idRetMap' = idRetMap
    /\ phase' = [phase EXCEPT ![w] = "Linearized"]
    /\ linIds' = Append(linIds, activeId[w])
    /\ UNCHANGED <<checkpointed, nextOp, clock, activeId, idKind, idArg,
                  idStart, idFinish, history, gate>>

LinearizeCheckpoint(w) ==
    /\ phase[w] = "Started"
    /\ activeId[w] \in OpIds
    /\ idKind[activeId[w]] = "Checkpoint"
    /\ gate = "Checkpoint"
    /\ checkpointed' = visible
    /\ walLive' = EmptyWalMap
    /\ idRet1' = [idRet1 EXCEPT ![activeId[w]] = Cardinality({k \in Terms : visible[k] # None})]
    /\ phase' = [phase EXCEPT ![w] = "Linearized"]
    /\ linIds' = Append(linIds, activeId[w])
    /\ UNCHANGED <<nextIndex, visible, nextOp, clock, activeId, idKind, idArg,
                  idRet2, idRetMap, idStart, idFinish, history, gate>>

LinearizeRecover(w) ==
    /\ phase[w] = "Started"
    /\ activeId[w] \in OpIds
    /\ idKind[activeId[w]] = "Recover"
    /\ idRetMap' = [idRetMap EXCEPT ![activeId[w]] = RecoveredMap(visible, checkpointed, walLive)]
    /\ phase' = [phase EXCEPT ![w] = "Linearized"]
    /\ linIds' = Append(linIds, activeId[w])
    /\ UNCHANGED <<nextIndex, visible, checkpointed, walLive,
                  nextOp, clock, activeId, idKind, idArg, idRet1, idRet2,
                  idStart, idFinish, history, gate>>

FinishOp(w) ==
    /\ w \in Writers
    /\ phase[w] = "Linearized"
    /\ activeId[w] \in OpIds
    /\ clock <= 2 * MaxOps
    /\ idFinish' = [idFinish EXCEPT ![activeId[w]] = clock]
    /\ history' = Append(history, activeId[w])
    /\ clock' = clock + 1
    /\ gate' = IF idKind[activeId[w]] = "Checkpoint" THEN "Open" ELSE gate
    /\ phase' = [phase EXCEPT ![w] = "Idle"]
    /\ activeId' = [activeId EXCEPT ![w] = None]
    /\ UNCHANGED <<nextIndex, visible, checkpointed, walLive,
                  nextOp, idKind, idArg, idRet1, idRet2, idRetMap,
                  idStart, linIds>>

Next ==
    \/ \E w \in Writers, k \in Terms : StartOp(w, "Insert", k)
    \/ \E w \in Writers, k \in Terms : StartOp(w, "Read", k)
    \/ \E w \in Writers : StartOp(w, "Batch", None)
    \/ \E w \in Writers : StartCheckpoint(w)
    \/ \E w \in Writers : StartRecover(w)
    \/ \E w \in Writers : LinearizeInsert(w)
    \/ \E w \in Writers : LinearizeRead(w)
    \/ \E w \in Writers : LinearizeBatch(w)
    \/ \E w \in Writers : LinearizeCheckpoint(w)
    \/ \E w \in Writers : LinearizeRecover(w)
    \/ \E w \in Writers : FinishOp(w)

CompletedIds == {history[i] : i \in DOMAIN history}
LinearizedIds == {linIds[i] : i \in DOMAIN linIds}

NoDuplicateLinearizations ==
    Cardinality(LinearizedIds) = Len(linIds)

NoDuplicateHistoryResponses ==
    Cardinality(CompletedIds) = Len(history)

CompletedOpsWereLinearized ==
    CompletedIds \subseteq LinearizedIds

BeforeInLinearization(a, b) ==
    \E i \in DOMAIN linIds, j \in DOMAIN linIds :
        /\ linIds[i] = a
        /\ linIds[j] = b
        /\ i < j

RealTimeOrderRespected ==
    \A a \in CompletedIds, b \in CompletedIds :
        idFinish[a] < idStart[b] => BeforeInLinearization(a, b)

LinearizationExplainsHistory ==
    SeqOK(LinRecordSeq, EmptyIndexMap, 0)

UniqueVisibleIndexes ==
    \A a \in Terms, b \in Terms :
        /\ visible[a] # None
        /\ visible[b] # None
        /\ visible[a] = visible[b]
        => a = b

RecoverableAfterCheckpointOrWal ==
    RecoveredMap(visible, checkpointed, walLive) = visible

NoStartWhileCheckpointGateHeld ==
    gate = "Checkpoint" =>
        \A w \in Writers :
            phase[w] # "Started" \/ idKind[activeId[w]] = "Checkpoint"

Spec == Init /\ [][Next]_Vars

=============================================================================
