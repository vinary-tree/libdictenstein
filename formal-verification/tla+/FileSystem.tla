-------------------------------- MODULE FileSystem --------------------------------
(****************************************************************************)
(* FileSystem: POSIX filesystem model for verifying file operation races.   *)
(*                                                                          *)
(* This module models POSIX filesystem operations with their NON-ATOMIC     *)
(* semantics to verify that higher-level abstractions (like WAL) correctly  *)
(* handle race conditions such as TOCTOU (Time-Of-Check-To-Time-Of-Use).    *)
(*                                                                          *)
(* Key aspects modeled:                                                     *)
(* 1. File existence is observable but can change between observations      *)
(* 2. Directory existence affects file creation                             *)
(* 3. Concurrent processes can interleave at syscall boundaries             *)
(* 4. Operations can fail due to filesystem state changes                   *)
(*                                                                          *)
(* NOT modeled (out of scope):                                              *)
(* - File permissions (we assume sufficient permissions)                    *)
(* - Disk space exhaustion                                                  *)
(* - Hard links and symbolic links                                          *)
(* - File locking (advisory/mandatory)                                      *)
(****************************************************************************)

EXTENDS Integers, Sequences, FiniteSets, TLC

--------------------------------------------------------------------------------
(* CONFIGURATION CONSTANTS                                                    *)
--------------------------------------------------------------------------------

CONSTANTS
    \* Set of all possible file paths (abstract strings)
    Paths,

    \* Maximum number of concurrent operations to model
    MaxConcurrentOps,

    \* Set of thread/process identifiers
    FsThreads,

    \* Null path constant (model value)
    NullPath

--------------------------------------------------------------------------------
(* ASSUMPTIONS                                                                *)
--------------------------------------------------------------------------------

ASSUME Paths # {}
ASSUME MaxConcurrentOps \in Nat \ {0}
ASSUME FsThreads # {}
ASSUME NullPath \notin Paths

--------------------------------------------------------------------------------
(* FILE AND DIRECTORY STATES                                                  *)
--------------------------------------------------------------------------------

\* A file can be absent, empty, or contain data
FileState == {"absent", "empty", "hasData"}

\* A directory can be absent or present
DirState == {"absent", "present"}

\* Filesystem error codes
FsError == {"Ok", "NotFound", "ParentNotFound", "AlreadyExists", "IsDirectory", "NotDirectory", "NotEmpty"}

--------------------------------------------------------------------------------
(* STATE VARIABLES                                                            *)
--------------------------------------------------------------------------------

VARIABLES
    \* File states: Path -> FileState
    files,

    \* Directory states: Path -> DirState
    directories,

    \* In-flight operations: Seq of pending operations per thread
    pendingOps,

    \* Completed operation results: Thread -> Result
    opResults,

    \* File open handles: Thread -> Set of open file paths
    openHandles

--------------------------------------------------------------------------------
(* HELPER OPERATORS                                                           *)
--------------------------------------------------------------------------------

\* Extract parent directory from a path
\* In the model, paths are abstract - we use a function to determine parent
\* For model checking, this is configured in the .cfg file
CONSTANT ParentDir(_)

\* Check if one path is a prefix of another (for mkdir -p semantics)
CONSTANT IsPrefix(_, _)

\* Get all ancestor directories of a path
Ancestors(path) == {d \in Paths : IsPrefix(d, path) /\ d # path}

\* Check if a file exists in current state
FileExists(path) ==
    /\ path \in DOMAIN files
    /\ files[path] # "absent"

\* Check if a directory exists in current state
DirExists(path) ==
    /\ path \in DOMAIN directories
    /\ directories[path] # "absent"

--------------------------------------------------------------------------------
(* TYPE INVARIANT                                                             *)
--------------------------------------------------------------------------------

FileSystemTypeInvariant ==
    /\ \A p \in DOMAIN files : files[p] \in FileState
    /\ \A p \in DOMAIN directories : directories[p] \in DirState
    /\ \A t \in DOMAIN openHandles : openHandles[t] \subseteq Paths

--------------------------------------------------------------------------------
(* INITIAL STATE                                                              *)
--------------------------------------------------------------------------------

FileSystemInit ==
    /\ files = [p \in Paths |-> "absent"]
    /\ directories = [p \in Paths |-> "absent"]
    /\ pendingOps = [t \in FsThreads |-> <<>>]
    /\ opResults = [t \in FsThreads |-> "Ok"]
    /\ openHandles = [t \in FsThreads |-> {}]

--------------------------------------------------------------------------------
(* NON-ATOMIC FILE OPERATIONS                                                 *)
(* These model the syscall semantics where operations can fail due to         *)
(* concurrent state changes.                                                  *)
--------------------------------------------------------------------------------

\* stat() - Check if file exists
\* Returns current state but result may be stale by the time it's used
Stat(thread, path) ==
    /\ opResults' = [opResults EXCEPT ![thread] =
        IF FileExists(path) THEN "Ok" ELSE "NotFound"]
    /\ UNCHANGED <<files, directories, pendingOps, openHandles>>

\* open(O_CREAT | O_EXCL) - Create file, fail if exists
\* Preconditions: parent directory must exist, file must not exist
OpenCreateExcl(thread, path) ==
    LET parent == ParentDir(path) IN
    \/ (  \* Success case: parent exists, file doesn't exist
        /\ parent = NullPath \/ DirExists(parent)
        /\ ~FileExists(path)
        /\ files' = [files EXCEPT ![path] = "empty"]
        /\ openHandles' = [openHandles EXCEPT ![thread] = @ \cup {path}]
        /\ opResults' = [opResults EXCEPT ![thread] = "Ok"]
        /\ UNCHANGED <<directories, pendingOps>>)
    \/ (  \* Failure: parent doesn't exist
        /\ parent # NullPath
        /\ ~DirExists(parent)
        /\ opResults' = [opResults EXCEPT ![thread] = "ParentNotFound"]
        /\ UNCHANGED <<files, directories, pendingOps, openHandles>>)
    \/ (  \* Failure: file already exists
        /\ (parent = NullPath \/ DirExists(parent))
        /\ FileExists(path)
        /\ opResults' = [opResults EXCEPT ![thread] = "AlreadyExists"]
        /\ UNCHANGED <<files, directories, pendingOps, openHandles>>)

\* open(O_CREAT) - Create file if not exists, open if exists
\* Precondition: parent directory must exist
OpenCreate(thread, path) ==
    LET parent == ParentDir(path) IN
    \/ (  \* Success case: parent exists
        /\ parent = NullPath \/ DirExists(parent)
        /\ files' = [files EXCEPT ![path] = IF files[path] = "absent" THEN "empty" ELSE files[path]]
        /\ openHandles' = [openHandles EXCEPT ![thread] = @ \cup {path}]
        /\ opResults' = [opResults EXCEPT ![thread] = "Ok"]
        /\ UNCHANGED <<directories, pendingOps>>)
    \/ (  \* Failure: parent doesn't exist
        /\ parent # NullPath
        /\ ~DirExists(parent)
        /\ opResults' = [opResults EXCEPT ![thread] = "ParentNotFound"]
        /\ UNCHANGED <<files, directories, pendingOps, openHandles>>)

\* open() - Open existing file
\* Precondition: file must exist
OpenExisting(thread, path) ==
    \/ (  \* Success case: file exists
        /\ FileExists(path)
        /\ openHandles' = [openHandles EXCEPT ![thread] = @ \cup {path}]
        /\ opResults' = [opResults EXCEPT ![thread] = "Ok"]
        /\ UNCHANGED <<files, directories, pendingOps>>)
    \/ (  \* Failure: file doesn't exist
        /\ ~FileExists(path)
        /\ opResults' = [opResults EXCEPT ![thread] = "NotFound"]
        /\ UNCHANGED <<files, directories, pendingOps, openHandles>>)

\* close() - Close open file handle
Close(thread, path) ==
    /\ path \in openHandles[thread]
    /\ openHandles' = [openHandles EXCEPT ![thread] = @ \ {path}]
    /\ opResults' = [opResults EXCEPT ![thread] = "Ok"]
    /\ UNCHANGED <<files, directories, pendingOps>>

\* unlink() - Delete file
\* Precondition: file must exist and not be a directory
Unlink(thread, path) ==
    \/ (  \* Success case: file exists
        /\ FileExists(path)
        /\ files' = [files EXCEPT ![path] = "absent"]
        /\ opResults' = [opResults EXCEPT ![thread] = "Ok"]
        /\ UNCHANGED <<directories, pendingOps, openHandles>>)
    \/ (  \* Failure: file doesn't exist
        /\ ~FileExists(path)
        /\ opResults' = [opResults EXCEPT ![thread] = "NotFound"]
        /\ UNCHANGED <<files, directories, pendingOps, openHandles>>)

--------------------------------------------------------------------------------
(* DIRECTORY OPERATIONS                                                       *)
--------------------------------------------------------------------------------

\* mkdir() - Create single directory
\* Precondition: parent must exist, directory must not exist
Mkdir(thread, path) ==
    LET parent == ParentDir(path) IN
    \/ (  \* Success case
        /\ parent = NullPath \/ DirExists(parent)
        /\ ~DirExists(path)
        /\ ~FileExists(path)
        /\ directories' = [directories EXCEPT ![path] = "present"]
        /\ opResults' = [opResults EXCEPT ![thread] = "Ok"]
        /\ UNCHANGED <<files, pendingOps, openHandles>>)
    \/ (  \* Failure: parent doesn't exist
        /\ parent # NullPath
        /\ ~DirExists(parent)
        /\ opResults' = [opResults EXCEPT ![thread] = "ParentNotFound"]
        /\ UNCHANGED <<files, directories, pendingOps, openHandles>>)
    \/ (  \* Failure: already exists (as dir or file)
        /\ (DirExists(path) \/ FileExists(path))
        /\ opResults' = [opResults EXCEPT ![thread] = "AlreadyExists"]
        /\ UNCHANGED <<files, directories, pendingOps, openHandles>>)

\* mkdir_all() - Create directory and all parent directories (mkdir -p)
\* This is idempotent - succeeds even if directories already exist
MkdirAll(thread, path) ==
    /\ directories' = [d \in Paths |->
        IF d = path \/ IsPrefix(d, path) THEN "present" ELSE directories[d]]
    /\ opResults' = [opResults EXCEPT ![thread] = "Ok"]
    /\ UNCHANGED <<files, pendingOps, openHandles>>

\* rmdir() - Remove empty directory
Rmdir(thread, path) ==
    \* Note: checking for empty is simplified - we don't track directory contents
    \/ (  \* Success case
        /\ DirExists(path)
        /\ directories' = [directories EXCEPT ![path] = "absent"]
        /\ opResults' = [opResults EXCEPT ![thread] = "Ok"]
        /\ UNCHANGED <<files, pendingOps, openHandles>>)
    \/ (  \* Failure: doesn't exist
        /\ ~DirExists(path)
        /\ opResults' = [opResults EXCEPT ![thread] = "NotFound"]
        /\ UNCHANGED <<files, directories, pendingOps, openHandles>>)

--------------------------------------------------------------------------------
(* COMPOSITE OPERATIONS - SHOWING TOCTOU VULNERABILITY                        *)
--------------------------------------------------------------------------------

\* VULNERABLE: Check exists then open (can fail between check and open)
\* This models the anti-pattern that the plan identified
TOCTOU_Vulnerable_Exists_Open(thread, path) ==
    \* This is a TWO-STEP operation that can be interleaved
    \* Step 1: Record intent to check+open
    /\ pendingOps' = [pendingOps EXCEPT ![thread] = Append(@,
        [type |-> "toctou_check_open", path |-> path, phase |-> "check"])]
    /\ UNCHANGED <<files, directories, opResults, openHandles>>

\* TOCTOU Step 1: Check phase
TOCTOU_Check(thread) ==
    /\ Len(pendingOps[thread]) > 0
    /\ pendingOps[thread][1].type = "toctou_check_open"
    /\ pendingOps[thread][1].phase = "check"
    /\ LET op == pendingOps[thread][1] IN
       IF FileExists(op.path)
       THEN \* File exists, proceed to open phase
           /\ pendingOps' = [pendingOps EXCEPT ![thread][1].phase = "open"]
           /\ UNCHANGED <<files, directories, opResults, openHandles>>
       ELSE \* File doesn't exist, fail immediately
           /\ pendingOps' = [pendingOps EXCEPT ![thread] = Tail(@)]
           /\ opResults' = [opResults EXCEPT ![thread] = "NotFound"]
           /\ UNCHANGED <<files, directories, openHandles>>

\* TOCTOU Step 2: Open phase (can fail if file was deleted!)
TOCTOU_Open(thread) ==
    /\ Len(pendingOps[thread]) > 0
    /\ pendingOps[thread][1].type = "toctou_check_open"
    /\ pendingOps[thread][1].phase = "open"
    /\ LET op == pendingOps[thread][1] IN
       IF FileExists(op.path)
       THEN \* File still exists, success
           /\ openHandles' = [openHandles EXCEPT ![thread] = @ \cup {op.path}]
           /\ pendingOps' = [pendingOps EXCEPT ![thread] = Tail(@)]
           /\ opResults' = [opResults EXCEPT ![thread] = "Ok"]
           /\ UNCHANGED <<files, directories>>
       ELSE \* File was deleted between check and open - TOCTOU RACE!
           /\ pendingOps' = [pendingOps EXCEPT ![thread] = Tail(@)]
           /\ opResults' = [opResults EXCEPT ![thread] = "NotFound"]
           /\ UNCHANGED <<files, directories, openHandles>>

--------------------------------------------------------------------------------
(* SAFE COMPOSITE OPERATIONS                                                  *)
--------------------------------------------------------------------------------

\* SAFE: Open with create fallback (handles TOCTOU)
\* This models the corrected pattern
OpenOrCreate_Safe(thread, path) ==
    LET parent == ParentDir(path) IN
    \* First ensure parent exists
    /\ (parent = NullPath \/ DirExists(parent))
    \* Then open with O_CREAT (atomic at syscall level)
    /\ files' = [files EXCEPT ![path] =
        IF files[path] = "absent" THEN "empty" ELSE files[path]]
    /\ openHandles' = [openHandles EXCEPT ![thread] = @ \cup {path}]
    /\ opResults' = [opResults EXCEPT ![thread] = "Ok"]
    /\ UNCHANGED <<directories, pendingOps>>

\* SAFE: Create with parent directory creation
\* First mkdir_all for parent, then create file
CreateWithParents_Safe(thread, path) ==
    LET parent == ParentDir(path) IN
    /\ directories' = [d \in Paths |->
        IF d = parent \/ IsPrefix(d, parent) THEN "present" ELSE directories[d]]
    /\ files' = [files EXCEPT ![path] =
        IF files[path] = "absent" THEN "empty" ELSE files[path]]
    /\ openHandles' = [openHandles EXCEPT ![thread] = @ \cup {path}]
    /\ opResults' = [opResults EXCEPT ![thread] = "Ok"]
    /\ UNCHANGED pendingOps

--------------------------------------------------------------------------------
(* CONCURRENT INTERLEAVING - ANOTHER THREAD CAN MODIFY FILES                  *)
--------------------------------------------------------------------------------

\* External deletion - models another process deleting a file
ExternalDelete(path) ==
    /\ FileExists(path)
    /\ files' = [files EXCEPT ![path] = "absent"]
    /\ UNCHANGED <<directories, pendingOps, opResults, openHandles>>

\* External directory removal
ExternalRmdir(path) ==
    /\ DirExists(path)
    /\ directories' = [directories EXCEPT ![path] = "absent"]
    /\ UNCHANGED <<files, pendingOps, opResults, openHandles>>

--------------------------------------------------------------------------------
(* SAFETY INVARIANTS                                                          *)
--------------------------------------------------------------------------------

\* A file can only exist if its parent directory exists (or it's in root)
ParentDirInvariant ==
    \A path \in Paths :
        LET parent == ParentDir(path) IN
        FileExists(path) => (parent = NullPath \/ DirExists(parent))

\* Open handles are only for existing files
\* Note: In POSIX, open handles keep file alive even if unlinked
\* We simplify here for the model
OpenHandleConsistency ==
    \A t \in FsThreads :
        \A path \in openHandles[t] : FileExists(path)

--------------------------------------------------------------------------------
(* SAFETY PROPERTY: TOCTOU Race Detection                                     *)
--------------------------------------------------------------------------------

\* Track if a TOCTOU race occurred (file deleted between check and open)
TOCTOURaceOccurred ==
    \E t \in FsThreads :
        /\ Len(pendingOps[t]) > 0
        /\ pendingOps[t][1].type = "toctou_check_open"
        /\ pendingOps[t][1].phase = "open"
        /\ ~FileExists(pendingOps[t][1].path)

\* Invariant: No TOCTOU race in checked operation
\* This SHOULD be violated to demonstrate the bug!
NoTOCTOURace == ~TOCTOURaceOccurred

--------------------------------------------------------------------------------
(* NEXT STATE RELATION                                                        *)
--------------------------------------------------------------------------------

FileSystemNext ==
    \/ \E t \in FsThreads, p \in Paths :
        \/ Stat(t, p)
        \/ OpenCreateExcl(t, p)
        \/ OpenCreate(t, p)
        \/ OpenExisting(t, p)
        \/ Close(t, p)
        \/ Unlink(t, p)
        \/ Mkdir(t, p)
        \/ MkdirAll(t, p)
        \/ Rmdir(t, p)
        \/ OpenOrCreate_Safe(t, p)
        \/ CreateWithParents_Safe(t, p)
    \/ \E t \in FsThreads :
        \/ TOCTOU_Check(t)
        \/ TOCTOU_Open(t)
    \/ \E p \in Paths :
        \/ ExternalDelete(p)
        \/ ExternalRmdir(p)

--------------------------------------------------------------------------------
(* SPECIFICATION                                                              *)
--------------------------------------------------------------------------------

FileSystemSpec ==
    /\ FileSystemInit
    /\ [][FileSystemNext]_<<files, directories, pendingOps, opResults, openHandles>>

--------------------------------------------------------------------------------
(* VARIABLE EXPORTS                                                           *)
--------------------------------------------------------------------------------

fsVars == <<files, directories, pendingOps, opResults, openHandles>>

================================================================================
(* LICENSE: MIT License                                                       *)
(* Copyright (c) 2026 F1r3fly.io                                              *)
================================================================================
