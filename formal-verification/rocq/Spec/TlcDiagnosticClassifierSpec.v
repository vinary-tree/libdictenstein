(** * Exact classification of TLC invariant-violation diagnostics

    TLC reports an invariant violation in one of two forms: after a transition,
    or directly in an initial state.  The verification harness may accept both
    forms, but only when the reported invariant name is exactly the one declared
    by the negative control. *)

From Coq Require Import Bool.Bool Strings.String.

Open Scope string_scope.

Inductive invariant_violation_form : Type :=
| AfterTransition
| InInitialState.

Record invariant_violation_diagnostic : Type := {
  diagnostic_form : invariant_violation_form;
  diagnostic_invariant : string;
}.

Definition accepts_invariant_violation
    (expected : string)
    (diagnostic : invariant_violation_diagnostic) : bool :=
  String.eqb expected diagnostic.(diagnostic_invariant).

Theorem accepts_after_transition_exact :
  forall name,
    accepts_invariant_violation name
      {| diagnostic_form := AfterTransition;
         diagnostic_invariant := name |} = true.
Proof.
  intros name.
  unfold accepts_invariant_violation.
  apply String.eqb_refl.
Qed.

Theorem accepts_initial_state_exact :
  forall name,
    accepts_invariant_violation name
      {| diagnostic_form := InInitialState;
         diagnostic_invariant := name |} = true.
Proof.
  intros name.
  unfold accepts_invariant_violation.
  apply String.eqb_refl.
Qed.

Theorem acceptance_implies_exact_name :
  forall expected diagnostic,
    accepts_invariant_violation expected diagnostic = true ->
    expected = diagnostic.(diagnostic_invariant).
Proof.
  intros expected diagnostic accepted.
  unfold accepts_invariant_violation in accepted.
  now apply String.eqb_eq.
Qed.

Theorem rejects_wrong_name :
  forall expected diagnostic,
    expected <> diagnostic.(diagnostic_invariant) ->
    accepts_invariant_violation expected diagnostic = false.
Proof.
  intros expected diagnostic names_differ.
  unfold accepts_invariant_violation.
  now apply String.eqb_neq.
Qed.
