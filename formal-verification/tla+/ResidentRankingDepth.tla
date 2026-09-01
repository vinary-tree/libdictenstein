------------------------- MODULE ResidentRankingDepth -------------------------
(*****************************************************************************)
(* The cap/local-weight optimization requires every concrete descendant to   *)
(* rank before its resident ancestor. With equal warmest scores, production  *)
(* orders greater depth first, then ascending preorder id. A zero-length      *)
(* concrete child segment gives equal depth; the ancestor's smaller preorder *)
(* id then wins a cap-one prefix and covers two resident records. Rejecting   *)
(* empty non-root segments restores the strict-depth premise and exact cap.   *)
(*****************************************************************************)

EXTENDS Naturals, FiniteSets, TLC

CONSTANT UnsafeAllowEmptyConcreteSegment

Nodes == {"ancestor", "descendant"}
Cap == 1

Depth(node) ==
    CASE node = "ancestor" -> 1
      [] node = "descendant" ->
           IF UnsafeAllowEmptyConcreteSegment THEN 1 ELSE 2

WarmestScore(node) == 10
PreorderId(node) == IF node = "ancestor" THEN 0 ELSE 1

Before(left, right) ==
    \/ WarmestScore(left) > WarmestScore(right)
    \/ /\ WarmestScore(left) = WarmestScore(right)
       /\ Depth(left) > Depth(right)
    \/ /\ WarmestScore(left) = WarmestScore(right)
       /\ Depth(left) = Depth(right)
       /\ PreorderId(left) < PreorderId(right)

First == CHOOSE node \in Nodes : \A other \in Nodes : ~Before(other, node)
Selected == {First}

Subtree(node) ==
    IF node = "ancestor" THEN Nodes ELSE {"descendant"}

Covered == UNION {Subtree(node) : node \in Selected}

VARIABLE dummy

ConcreteChildStrictDepth ==
    dummy = 0 => Depth("ancestor") < Depth("descendant")

CoveredResidentCountWithinCap ==
    dummy = 0 => Cardinality(Covered) <= Cap

Init == dummy = 0
Next == UNCHANGED dummy
Spec == Init /\ [][Next]_<<dummy>>

=============================================================================
