(**
  Feature-gated public API visibility for optional performance observations.

  This specification separates the established causal-construction API from
  the new persistent-serialization instrumentation. The latter is meaningful
  only when [perf-instrumentation] is enabled and therefore must not become an
  unconditional semver-visible root export.

  Rust correspondence:
  - [src/lib.rs] root re-exports;
  - [src/causal_perf.rs] counter definitions;
  - feature-on/off downstream compile checks in the closure gate.
*)

From Coq Require Import Bool.Bool.

Inductive public_symbol : Type :=
| CausalConstructionStats
| CausalConstructionSnapshot
| ResetCausalConstructionStats
| PersistentSerializationStats
| PersistentSerializationSnapshot
| ResetPersistentSerializationStats.

Definition instrumentation_only (symbol : public_symbol) : bool :=
  match symbol with
  | PersistentSerializationStats
  | PersistentSerializationSnapshot
  | ResetPersistentSerializationStats => true
  | _ => false
  end.

Definition root_exported
    (perf_instrumentation : bool)
    (symbol : public_symbol) : bool :=
  negb (instrumentation_only symbol) || perf_instrumentation.

Theorem established_causal_api_is_always_exported :
  forall perf,
    root_exported perf CausalConstructionStats = true /\
    root_exported perf CausalConstructionSnapshot = true /\
    root_exported perf ResetCausalConstructionStats = true.
Proof.
  intros []; repeat split; reflexivity.
Qed.

Theorem persistent_observation_api_is_absent_without_feature :
  root_exported false PersistentSerializationStats = false /\
  root_exported false PersistentSerializationSnapshot = false /\
  root_exported false ResetPersistentSerializationStats = false.
Proof.
  repeat split; reflexivity.
Qed.

Theorem persistent_observation_api_is_present_with_feature :
  root_exported true PersistentSerializationStats = true /\
  root_exported true PersistentSerializationSnapshot = true /\
  root_exported true ResetPersistentSerializationStats = true.
Proof.
  repeat split; reflexivity.
Qed.

Theorem persistent_observation_visibility_is_exact :
  forall perf symbol,
    instrumentation_only symbol = true ->
    root_exported perf symbol = perf.
Proof.
  intros [] [] H; simpl in *; try discriminate; reflexivity.
Qed.
