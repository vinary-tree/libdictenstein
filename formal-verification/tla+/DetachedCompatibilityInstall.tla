---------------- MODULE DetachedCompatibilityInstall ----------------
(***************************************************************************)
(* The deprecated infallible compatibility wrapper is total: rejection of   *)
(* malformed input or a retired coordinator neither panics nor publishes the *)
(* rejected candidate. Other successful install actions may independently   *)
(* replace the detached advisory slot. The fallible API uses the same state  *)
(* transition and reports rejection explicitly.                             *)
(***************************************************************************)

EXTENDS TLC

CONSTANTS
    Catalogs,
    NoCatalog,
    UnsafeOverwriteOnReject,
    UnsafePanicOnReject

ASSUME /\ Catalogs # {}
       /\ NoCatalog \notin Catalogs

CatalogValues == Catalogs \cup {NoCatalog}
Outcomes == {"None", "Installed", "Rejected", "Panicked", "Retired"}

VARIABLES
    live,
    catalog,
    lastAccepted,
    outcome,
    badPanic,
    badRejectedMutation

vars ==
    <<live, catalog, lastAccepted, outcome, badPanic, badRejectedMutation>>

TypeInvariant ==
    /\ live \in BOOLEAN
    /\ catalog \in CatalogValues
    /\ lastAccepted \in BOOLEAN
    /\ outcome \in Outcomes
    /\ badPanic \in BOOLEAN
    /\ badRejectedMutation \in BOOLEAN

LegacyWrapperNeverPanics == ~badPanic

RejectedInstallPreservesCatalog == ~badRejectedMutation

Init ==
    /\ live = TRUE
    /\ catalog = NoCatalog
    /\ lastAccepted = FALSE
    /\ outcome = "None"
    /\ badPanic = FALSE
    /\ badRejectedMutation = FALSE

Install(candidate, structurallyValid) ==
    /\ candidate \in Catalogs
    /\ structurallyValid \in BOOLEAN
    /\ LET accepted == live /\ structurallyValid
           successor ==
             IF accepted
             THEN candidate
             ELSE IF UnsafeOverwriteOnReject THEN candidate ELSE catalog
           result ==
             IF accepted
             THEN "Installed"
             ELSE IF UnsafePanicOnReject THEN "Panicked" ELSE "Rejected"
       IN /\ catalog' = successor
          /\ lastAccepted' = accepted
          /\ outcome' = result
          /\ badPanic' = (badPanic \/ result = "Panicked")
          /\ badRejectedMutation' =
               (badRejectedMutation \/ (~accepted /\ successor # catalog))
    /\ UNCHANGED live

Retire ==
    /\ live
    /\ live' = FALSE
    /\ catalog' = NoCatalog
    /\ lastAccepted' = FALSE
    /\ outcome' = "Retired"
    /\ UNCHANGED <<badPanic, badRejectedMutation>>

Next ==
    \/ \E candidate \in Catalogs, structurallyValid \in BOOLEAN :
         Install(candidate, structurallyValid)
    \/ Retire

Spec == Init /\ [][Next]_vars

=============================================================================
