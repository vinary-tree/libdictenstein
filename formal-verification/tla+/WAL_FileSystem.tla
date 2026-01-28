--------------------------- MODULE WAL_FileSystem ------------------------------
(****************************************************************************)
(* WAL_FileSystem: WAL specification refined to use POSIX filesystem model. *)
(*                                                                          *)
(* This module refines WAL.tla by composing it with FileSystem.tla to       *)
(* verify that WAL operations correctly handle filesystem-level race        *)
(* conditions, particularly:                                                *)
(*                                                                          *)
(* 1. TOCTOU Race: exists() followed by open() is not atomic                *)
(* 2. Missing Parent Directories: create() assumes parent dirs exist        *)
(*                                                                          *)
(* This is a REFINEMENT specification - it adds detail to the abstract      *)
(* WAL.tla model by exposing the filesystem syscall sequence.               *)
(****************************************************************************)

EXTENDS Integers, Sequences, FiniteSets, TLC

--------------------------------------------------------------------------------
(* CONFIGURATION CONSTANTS                                                    *)
--------------------------------------------------------------------------------

CONSTANTS
    \* WAL file paths
    WalPaths,
    WalDirPath,
    WalFilePath,

    \* Thread identifiers for WAL operations
    WalThreads,

    \* Maximum LSN for bounded model checking
    WalMaxLsn,

    \* Null path constant
    NullWalPath

--------------------------------------------------------------------------------
(* ASSUMPTIONS                                                                *)
--------------------------------------------------------------------------------

ASSUME WalPaths # {}
ASSUME WalDirPath \in WalPaths
ASSUME WalFilePath \in WalPaths
ASSUME WalThreads # {}
ASSUME WalMaxLsn \in Nat

--------------------------------------------------------------------------------
(* FILE AND DIRECTORY STATES (from FileSystem.tla)                            *)
--------------------------------------------------------------------------------

FileState == {"absent", "empty", "hasData"}
DirState == {"absent", "present"}
FsError == {"Ok", "NotFound", "ParentNotFound", "AlreadyExists"}

--------------------------------------------------------------------------------
(* STATE VARIABLES                                                            *)
--------------------------------------------------------------------------------

VARIABLES
    \* Filesystem state
    files,              \* Path -> FileState
    directories,        \* Path -> DirState
    opResults,          \* Thread -> FsError (last operation result)
    openHandles,        \* Thread -> SUBSET Paths (open file handles)

    \* WAL-specific state
    walLsn,             \* Current log sequence number
    walOpen,            \* Is the WAL file currently open?
    walOwner,           \* Which thread has the WAL open (if any)
    walOpPhase,         \* Current operation phase: "idle", "mkdir", "open", "verify"
    walOpThread,        \* Thread performing current operation
    walOpType,          \* Type of operation: "none", "create", "open_vulnerable", "open_safe"
    walContent          \* Content written to WAL (sequence of values)

--------------------------------------------------------------------------------
(* HELPER OPERATORS                                                           *)
--------------------------------------------------------------------------------

\* Check if file exists
FileExists(path) == files[path] # "absent"

\* Check if directory exists
DirExists(path) == directories[path] # "absent"

\* Parent directory mapping (configured for this model)
ParentDir(path) ==
    IF path = WalFilePath THEN WalDirPath
    ELSE IF path = WalDirPath THEN NullWalPath
    ELSE NullWalPath

\* Check if d is a prefix of p
IsPrefix(d, p) ==
    \/ (d = WalDirPath /\ p = WalFilePath)
    \/ (d = WalDirPath /\ p = WalDirPath)

--------------------------------------------------------------------------------
(* TYPE INVARIANT                                                             *)
--------------------------------------------------------------------------------

WalFsTypeInvariant ==
    /\ \A p \in WalPaths : files[p] \in FileState
    /\ \A p \in WalPaths : directories[p] \in DirState
    /\ \A t \in WalThreads : opResults[t] \in FsError
    /\ \A t \in WalThreads : openHandles[t] \subseteq WalPaths
    /\ walLsn \in Nat
    /\ walOpen \in BOOLEAN
    /\ walOwner \in WalThreads \cup {NullWalPath}
    /\ walOpPhase \in {"idle", "mkdir", "open", "verify"}
    /\ walOpThread \in WalThreads \cup {NullWalPath}
    /\ walOpType \in {"none", "create", "open_vulnerable", "open_safe"}

--------------------------------------------------------------------------------
(* INITIAL STATE                                                              *)
--------------------------------------------------------------------------------

WalFsInit ==
    /\ files = [p \in WalPaths |-> "absent"]
    /\ directories = [p \in WalPaths |-> "absent"]
    /\ opResults = [t \in WalThreads |-> "Ok"]
    /\ openHandles = [t \in WalThreads |-> {}]
    /\ walLsn = 0
    /\ walOpen = FALSE
    /\ walOwner = NullWalPath
    /\ walOpPhase = "idle"
    /\ walOpThread = NullWalPath
    /\ walOpType = "none"
    /\ walContent = <<>>

--------------------------------------------------------------------------------
(* FILESYSTEM HELPER OPERATIONS                                               *)
--------------------------------------------------------------------------------

\* mkdir_all: Create directory and all parents (idempotent)
MkdirAll(path) ==
    directories' = [p \in WalPaths |->
        IF p = path \/ IsPrefix(p, path) THEN "present" ELSE directories[p]]

--------------------------------------------------------------------------------
(* WAL CREATE OPERATION - SAFE IMPLEMENTATION                                 *)
(* Ensures parent directory exists before creating file.                      *)
--------------------------------------------------------------------------------

\* Start WalCreate: begin mkdir phase
WalCreate_Start(thread) ==
    /\ walOpType = "none"
    /\ ~walOpen
    /\ walOpType' = "create"
    /\ walOpThread' = thread
    /\ walOpPhase' = "mkdir"
    /\ UNCHANGED <<files, directories, opResults, openHandles,
                   walLsn, walOpen, walOwner, walContent>>

\* WalCreate Phase 1: Ensure parent directory exists
WalCreate_MkdirPhase ==
    /\ walOpType = "create"
    /\ walOpPhase = "mkdir"
    /\ MkdirAll(WalDirPath)
    /\ walOpPhase' = "open"
    /\ UNCHANGED <<files, opResults, openHandles,
                   walLsn, walOpen, walOwner, walOpType, walOpThread, walContent>>

\* WalCreate Phase 2: Create the WAL file (success case)
WalCreate_OpenPhase_Success ==
    /\ walOpType = "create"
    /\ walOpPhase = "open"
    /\ DirExists(WalDirPath)         \* Parent exists (from mkdir)
    /\ ~FileExists(WalFilePath)       \* File doesn't exist yet
    /\ files' = [files EXCEPT ![WalFilePath] = "empty"]
    /\ openHandles' = [openHandles EXCEPT ![walOpThread] = @ \cup {WalFilePath}]
    /\ opResults' = [opResults EXCEPT ![walOpThread] = "Ok"]
    /\ walOpen' = TRUE
    /\ walOwner' = walOpThread
    /\ walOpPhase' = "idle"
    /\ walOpType' = "none"
    /\ walOpThread' = NullWalPath
    /\ walContent' = <<>>
    /\ UNCHANGED <<directories, walLsn>>

\* WalCreate Phase 2: File already exists (failure case)
WalCreate_OpenPhase_AlreadyExists ==
    /\ walOpType = "create"
    /\ walOpPhase = "open"
    /\ FileExists(WalFilePath)
    /\ opResults' = [opResults EXCEPT ![walOpThread] = "AlreadyExists"]
    /\ walOpPhase' = "idle"
    /\ walOpType' = "none"
    /\ walOpThread' = NullWalPath
    /\ UNCHANGED <<files, directories, openHandles,
                   walLsn, walOpen, walOwner, walContent>>

\* WalCreate Phase 2: Parent directory was deleted (race condition)
WalCreate_OpenPhase_ParentNotFound ==
    /\ walOpType = "create"
    /\ walOpPhase = "open"
    /\ ~DirExists(WalDirPath)         \* Parent was deleted after mkdir!
    /\ opResults' = [opResults EXCEPT ![walOpThread] = "ParentNotFound"]
    /\ walOpPhase' = "idle"
    /\ walOpType' = "none"
    /\ walOpThread' = NullWalPath
    /\ UNCHANGED <<files, directories, openHandles,
                   walLsn, walOpen, walOwner, walContent>>

--------------------------------------------------------------------------------
(* WAL OPEN OPERATION - VULNERABLE IMPLEMENTATION                             *)
(* This demonstrates the TOCTOU race that was discovered.                     *)
--------------------------------------------------------------------------------

\* Start vulnerable open: check if file exists
WalOpen_Vulnerable_Start(thread) ==
    /\ walOpType = "none"
    /\ ~walOpen
    /\ walOpType' = "open_vulnerable"
    /\ walOpThread' = thread
    /\ walOpPhase' = "verify"
    \* Check if file exists (result can become stale!)
    /\ opResults' = [opResults EXCEPT ![thread] =
        IF FileExists(WalFilePath) THEN "Ok" ELSE "NotFound"]
    /\ UNCHANGED <<files, directories, openHandles,
                   walLsn, walOpen, walOwner, walContent>>

\* Vulnerable open: try to open (can fail due to TOCTOU!)
WalOpen_Vulnerable_Open_Success ==
    /\ walOpType = "open_vulnerable"
    /\ walOpPhase = "verify"
    /\ opResults[walOpThread] = "Ok"  \* Check said file existed
    /\ FileExists(WalFilePath)         \* File STILL exists
    /\ openHandles' = [openHandles EXCEPT ![walOpThread] = @ \cup {WalFilePath}]
    /\ opResults' = [opResults EXCEPT ![walOpThread] = "Ok"]
    /\ walOpen' = TRUE
    /\ walOwner' = walOpThread
    /\ walOpPhase' = "idle"
    /\ walOpType' = "none"
    /\ walOpThread' = NullWalPath
    /\ UNCHANGED <<files, directories, walLsn, walContent>>

\* Vulnerable open: TOCTOU RACE - file was deleted between check and open!
WalOpen_Vulnerable_Open_TOCTOU ==
    /\ walOpType = "open_vulnerable"
    /\ walOpPhase = "verify"
    /\ opResults[walOpThread] = "Ok"  \* Check said file existed
    /\ ~FileExists(WalFilePath)        \* But now it's GONE! TOCTOU RACE!
    /\ opResults' = [opResults EXCEPT ![walOpThread] = "NotFound"]
    /\ walOpPhase' = "idle"
    /\ walOpType' = "none"
    /\ walOpThread' = NullWalPath
    /\ UNCHANGED <<files, directories, openHandles,
                   walLsn, walOpen, walOwner, walContent>>

\* Vulnerable open: file not found in initial check - nothing to open
WalOpen_Vulnerable_Open_NotFound ==
    /\ walOpType = "open_vulnerable"
    /\ walOpPhase = "verify"
    /\ opResults[walOpThread] = "NotFound"  \* Initial check said file didn't exist
    /\ walOpPhase' = "idle"
    /\ walOpType' = "none"
    /\ walOpThread' = NullWalPath
    /\ UNCHANGED <<files, directories, opResults, openHandles,
                   walLsn, walOpen, walOwner, walContent>>

--------------------------------------------------------------------------------
(* WAL OPEN OPERATION - SAFE IMPLEMENTATION                                   *)
(* Handles TOCTOU by falling back to create on NotFound.                      *)
--------------------------------------------------------------------------------

\* Start safe open
WalOpen_Safe_Start(thread) ==
    /\ walOpType = "none"
    /\ ~walOpen
    /\ walOpType' = "open_safe"
    /\ walOpThread' = thread
    /\ walOpPhase' = "open"
    /\ opResults' = [opResults EXCEPT ![thread] = "Ok"]  \* Reset result
    /\ UNCHANGED <<files, directories, openHandles,
                   walLsn, walOpen, walOwner, walContent>>

\* Safe open: file exists, open it
WalOpen_Safe_TryOpen_Success ==
    /\ walOpType = "open_safe"
    /\ walOpPhase = "open"
    /\ FileExists(WalFilePath)
    /\ openHandles' = [openHandles EXCEPT ![walOpThread] = @ \cup {WalFilePath}]
    /\ opResults' = [opResults EXCEPT ![walOpThread] = "Ok"]
    /\ walOpen' = TRUE
    /\ walOwner' = walOpThread
    /\ walOpPhase' = "idle"
    /\ walOpType' = "none"
    /\ walOpThread' = NullWalPath
    /\ UNCHANGED <<files, directories, walLsn, walContent>>

\* Safe open: file doesn't exist, transition to mkdir phase
WalOpen_Safe_TryOpen_NotFound ==
    /\ walOpType = "open_safe"
    /\ walOpPhase = "open"
    /\ ~FileExists(WalFilePath)
    /\ walOpPhase' = "mkdir"
    /\ opResults' = [opResults EXCEPT ![walOpThread] = "NotFound"]
    /\ UNCHANGED <<files, directories, openHandles,
                   walLsn, walOpen, walOwner, walOpType, walOpThread, walContent>>

\* Safe open: mkdir phase - create directory
WalOpen_Safe_Mkdir ==
    /\ walOpType = "open_safe"
    /\ walOpPhase = "mkdir"
    /\ MkdirAll(WalDirPath)
    \* Immediately create file too
    /\ files' = [files EXCEPT ![WalFilePath] = "empty"]
    /\ openHandles' = [openHandles EXCEPT ![walOpThread] = @ \cup {WalFilePath}]
    /\ opResults' = [opResults EXCEPT ![walOpThread] = "Ok"]
    /\ walOpen' = TRUE
    /\ walOwner' = walOpThread
    /\ walOpPhase' = "idle"
    /\ walOpType' = "none"
    /\ walOpThread' = NullWalPath
    /\ walContent' = <<>>
    /\ UNCHANGED walLsn

--------------------------------------------------------------------------------
(* WAL DATA OPERATIONS                                                        *)
--------------------------------------------------------------------------------

\* Write a record to the WAL
WalWrite(thread, value) ==
    /\ walOpen
    /\ walOwner = thread
    /\ walLsn < WalMaxLsn
    /\ walLsn' = walLsn + 1
    /\ walContent' = Append(walContent, value)
    /\ files' = [files EXCEPT ![WalFilePath] = "hasData"]
    /\ UNCHANGED <<directories, opResults, openHandles,
                   walOpen, walOwner, walOpPhase, walOpThread, walOpType>>

\* Close the WAL
WalClose(thread) ==
    /\ walOpen
    /\ walOwner = thread
    /\ openHandles' = [openHandles EXCEPT ![thread] = @ \ {WalFilePath}]
    /\ walOpen' = FALSE
    /\ walOwner' = NullWalPath
    /\ opResults' = [opResults EXCEPT ![thread] = "Ok"]
    /\ UNCHANGED <<files, directories, walLsn, walOpPhase, walOpThread, walOpType, walContent>>

--------------------------------------------------------------------------------
(* EXTERNAL INTERFERENCE - MODELS CONCURRENT PROCESS                          *)
--------------------------------------------------------------------------------

\* Another process deletes the WAL file (enables TOCTOU race)
ExternalWalDelete ==
    /\ FileExists(WalFilePath)
    /\ ~walOpen  \* Can only delete if not held open
    /\ files' = [files EXCEPT ![WalFilePath] = "absent"]
    /\ UNCHANGED <<directories, opResults, openHandles,
                   walLsn, walOpen, walOwner, walOpPhase, walOpThread, walOpType, walContent>>

\* Another process deletes the WAL directory
ExternalWalDirDelete ==
    /\ DirExists(WalDirPath)
    /\ ~FileExists(WalFilePath)  \* Directory must be empty
    /\ directories' = [directories EXCEPT ![WalDirPath] = "absent"]
    /\ UNCHANGED <<files, opResults, openHandles,
                   walLsn, walOpen, walOwner, walOpPhase, walOpThread, walOpType, walContent>>

--------------------------------------------------------------------------------
(* SAFETY INVARIANTS                                                          *)
--------------------------------------------------------------------------------

\* Parent directory always exists for WAL file when WAL is open
WalParentDirInvariant ==
    walOpen => DirExists(WalDirPath)

\* When WAL is open, the file exists
WalFileExistsWhenOpen ==
    walOpen => FileExists(WalFilePath)

\* TOCTOU race detection: vulnerable open can fail after check passed
\* This invariant is EXPECTED TO BE VIOLATED to demonstrate the bug!
VulnerableOpenNoRace ==
    (walOpType = "open_vulnerable" /\ walOpPhase = "verify" /\
     opResults[walOpThread] = "Ok") =>
        FileExists(WalFilePath)

\* Safe implementation never fails with ParentNotFound during mkdir
SafeOpenNoParentError ==
    \A t \in WalThreads :
        ~(walOpType = "open_safe" /\ walOpThread = t /\
          opResults[t] = "ParentNotFound")

--------------------------------------------------------------------------------
(* NEXT STATE RELATION                                                        *)
--------------------------------------------------------------------------------

WalFsNext ==
    \* WAL create operations (safe)
    \/ \E t \in WalThreads : WalCreate_Start(t)
    \/ WalCreate_MkdirPhase
    \/ WalCreate_OpenPhase_Success
    \/ WalCreate_OpenPhase_AlreadyExists
    \/ WalCreate_OpenPhase_ParentNotFound
    \* WAL open operations (vulnerable - for demonstrating bug)
    \/ \E t \in WalThreads : WalOpen_Vulnerable_Start(t)
    \/ WalOpen_Vulnerable_Open_Success
    \/ WalOpen_Vulnerable_Open_TOCTOU
    \/ WalOpen_Vulnerable_Open_NotFound
    \* WAL open operations (safe)
    \/ \E t \in WalThreads : WalOpen_Safe_Start(t)
    \/ WalOpen_Safe_TryOpen_Success
    \/ WalOpen_Safe_TryOpen_NotFound
    \/ WalOpen_Safe_Mkdir
    \* WAL data operations
    \/ \E t \in WalThreads, v \in 1..3 : WalWrite(t, v)
    \/ \E t \in WalThreads : WalClose(t)
    \* External interference (for race detection)
    \/ ExternalWalDelete
    \/ ExternalWalDirDelete

--------------------------------------------------------------------------------
(* SPECIFICATION                                                              *)
--------------------------------------------------------------------------------

vars == <<files, directories, opResults, openHandles,
          walLsn, walOpen, walOwner, walOpPhase, walOpThread, walOpType, walContent>>

WalFsSpec ==
    /\ WalFsInit
    /\ [][WalFsNext]_vars

--------------------------------------------------------------------------------
(* LIVENESS PROPERTIES                                                        *)
--------------------------------------------------------------------------------

\* Safe open eventually succeeds (assuming no continuous interference)
SafeOpenEventualSuccess ==
    \A t \in WalThreads :
        (walOpThread = t /\ walOpType = "open_safe") ~>
            (walOpen /\ walOwner = t)

================================================================================
(* LICENSE: MIT License                                                       *)
(* Copyright (c) 2026 F1r3fly.io                                              *)
================================================================================
