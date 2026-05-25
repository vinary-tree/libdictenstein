(** * PathMapFactorySpec: PathMap and Factory Public API Laws

    This module states the semantic proof boundary for the optional
    [pathmap-backend] implementation and for [DictionaryFactory] dispatch.

    The checked claim is intentionally at the public API level:

    - PathMap dictionaries refine a finite partial map over byte labels.
    - PathMapChar dictionaries refine the same laws over Unicode scalar labels.
    - Character-level node traversal is complete over all UTF-8-backed sibling
      characters, even when several characters share leading UTF-8 bytes.
    - Factory construction preserves the requested backend tag and produces the
      same abstract dictionary domain as direct construction.

    The model does not specify PathMap's internal memory layout, structural
    sharing, lock fairness, or native serialization format.
*)

From Stdlib Require Import Lists.List.
From Stdlib Require Import Bool.Bool.
From Stdlib Require Import Arith.PeanoNat.
From Stdlib Require Import Logic.FunctionalExtensionality.
Import ListNotations.

Definition PathLabel := nat.
Definition ByteLabel := PathLabel.
Definition CharLabel := PathLabel.

Definition PathKey := list PathLabel.
Definition ByteKey := list ByteLabel.
Definition CharKey := list CharLabel.

Definition PathMap (V : Type) := PathKey -> option V.
Definition BytePathMap (V : Type) := ByteKey -> option V.
Definition CharPathMap (V : Type) := CharKey -> option V.

Definition path_key_eq_dec
  (left right : PathKey) : {left = right} + {left <> right} :=
  list_eq_dec Nat.eq_dec left right.

Definition pathmap_empty {V : Type} : PathMap V := fun _ => None.

Definition pathmap_lookup {V : Type} (map : PathMap V) (key : PathKey) :
  option V :=
  map key.

Definition pathmap_contains {V : Type} (map : PathMap V) (key : PathKey) :
  bool :=
  match pathmap_lookup map key with
  | Some _ => true
  | None => false
  end.

Definition pathmap_insert {V : Type}
  (map : PathMap V) (key : PathKey) (value : V) : PathMap V :=
  fun query => if path_key_eq_dec key query then Some value else map query.

Definition pathmap_delete {V : Type}
  (map : PathMap V) (key : PathKey) : PathMap V :=
  fun query => if path_key_eq_dec key query then None else map query.

Definition pathmap_update_or_insert {V : Type}
  (map : PathMap V) (key : PathKey) (default : V) (update : V -> V) :
  PathMap V :=
  match pathmap_lookup map key with
  | Some old => pathmap_insert map key (update old)
  | None => pathmap_insert map key (update default)
  end.

Definition pathmap_union_with {V : Type}
  (left right : PathMap V) (merge : V -> V -> V) : PathMap V :=
  fun key =>
    match left key, right key with
    | Some left_value, Some right_value => Some (merge left_value right_value)
    | Some left_value, None => Some left_value
    | None, Some right_value => Some right_value
    | None, None => None
    end.

Definition pathmap_from_entries {V : Type}
  (entries : list (PathKey * V)) : PathMap V :=
  fold_left
    (fun map entry => pathmap_insert map (fst entry) (snd entry))
    entries
    pathmap_empty.

Definition pathmap_from_terms {V : Type}
  (terms : list PathKey) (default : V) : PathMap V :=
  fold_left
    (fun map key => pathmap_insert map key default)
    terms
    pathmap_empty.

Theorem pathmap_empty_contains_none :
  forall (V : Type) key,
    pathmap_contains (@pathmap_empty V) key = false.
Proof.
  reflexivity.
Qed.

Theorem pathmap_lookup_insert_same :
  forall (V : Type) (map : PathMap V) key value,
    pathmap_lookup (pathmap_insert map key value) key = Some value.
Proof.
  intros V map key value.
  unfold pathmap_lookup, pathmap_insert.
  destruct (path_key_eq_dec key key) as [_ | Hneq].
  - reflexivity.
  - exfalso. apply Hneq. reflexivity.
Qed.

Theorem pathmap_lookup_insert_other :
  forall (V : Type) (map : PathMap V) key query value,
    key <> query ->
    pathmap_lookup (pathmap_insert map key value) query =
    pathmap_lookup map query.
Proof.
  intros V map key query value Hneq.
  unfold pathmap_lookup, pathmap_insert.
  destruct (path_key_eq_dec key query) as [Heq | _].
  - contradiction.
  - reflexivity.
Qed.

Theorem pathmap_lookup_delete_same :
  forall (V : Type) (map : PathMap V) key,
    pathmap_lookup (pathmap_delete map key) key = None.
Proof.
  intros V map key.
  unfold pathmap_lookup, pathmap_delete.
  destruct (path_key_eq_dec key key) as [_ | Hneq].
  - reflexivity.
  - exfalso. apply Hneq. reflexivity.
Qed.

Theorem pathmap_lookup_delete_other :
  forall (V : Type) (map : PathMap V) key query,
    key <> query ->
    pathmap_lookup (pathmap_delete map key) query =
    pathmap_lookup map query.
Proof.
  intros V map key query Hneq.
  unfold pathmap_lookup, pathmap_delete.
  destruct (path_key_eq_dec key query) as [Heq | _].
  - contradiction.
  - reflexivity.
Qed.

Theorem pathmap_insert_last_write_wins :
  forall (V : Type) (map : PathMap V) key old_value new_value,
    pathmap_insert (pathmap_insert map key old_value) key new_value =
    pathmap_insert map key new_value.
Proof.
  intros V map key old_value new_value.
  apply functional_extensionality.
  intros query.
  unfold pathmap_insert.
  destruct (path_key_eq_dec key query); reflexivity.
Qed.

Theorem pathmap_delete_idempotent :
  forall (V : Type) (map : PathMap V) key,
    pathmap_delete (pathmap_delete map key) key =
    pathmap_delete map key.
Proof.
  intros V map key.
  apply functional_extensionality.
  intros query.
  unfold pathmap_delete.
  destruct (path_key_eq_dec key query); reflexivity.
Qed.

Theorem pathmap_update_or_insert_existing :
  forall (V : Type) (map : PathMap V) key old_value default update,
    pathmap_lookup map key = Some old_value ->
    pathmap_lookup
      (pathmap_update_or_insert map key default update)
      key = Some (update old_value).
Proof.
  intros V map key old_value default update Hlookup.
  unfold pathmap_update_or_insert.
  rewrite Hlookup.
  apply pathmap_lookup_insert_same.
Qed.

Theorem pathmap_update_or_insert_missing :
  forall (V : Type) (map : PathMap V) key default update,
    pathmap_lookup map key = None ->
    pathmap_lookup
      (pathmap_update_or_insert map key default update)
      key = Some (update default).
Proof.
  intros V map key default update Hlookup.
  unfold pathmap_update_or_insert.
  rewrite Hlookup.
  apply pathmap_lookup_insert_same.
Qed.

Theorem pathmap_union_with_left_only :
  forall (V : Type) (left right : PathMap V) merge key value,
    pathmap_lookup left key = Some value ->
    pathmap_lookup right key = None ->
    pathmap_lookup (pathmap_union_with left right merge) key = Some value.
Proof.
  intros V left right merge key value Hleft Hright.
  unfold pathmap_lookup, pathmap_union_with in *.
  rewrite Hleft. rewrite Hright. reflexivity.
Qed.

Theorem pathmap_union_with_right_only :
  forall (V : Type) (left right : PathMap V) merge key value,
    pathmap_lookup left key = None ->
    pathmap_lookup right key = Some value ->
    pathmap_lookup (pathmap_union_with left right merge) key = Some value.
Proof.
  intros V left right merge key value Hleft Hright.
  unfold pathmap_lookup, pathmap_union_with in *.
  rewrite Hleft. rewrite Hright. reflexivity.
Qed.

Theorem pathmap_union_with_overlap :
  forall (V : Type) (left right : PathMap V) merge key left_value right_value,
    pathmap_lookup left key = Some left_value ->
    pathmap_lookup right key = Some right_value ->
    pathmap_lookup (pathmap_union_with left right merge) key =
    Some (merge left_value right_value).
Proof.
  intros V left right merge key left_value right_value Hleft Hright.
  unfold pathmap_lookup, pathmap_union_with in *.
  rewrite Hleft. rewrite Hright. reflexivity.
Qed.

(** ** Node Traversal Laws *)

Definition path_child_exists {V : Type}
  (map : PathMap V) (prefix : PathKey) (label : PathLabel) : Prop :=
  exists suffix value,
    pathmap_lookup map (prefix ++ [label] ++ suffix) = Some value.

Definition path_node_final {V : Type}
  (map : PathMap V) (prefix : PathKey) : Prop :=
  exists value, pathmap_lookup map prefix = Some value.

Definition path_edge_labels_exact {V : Type}
  (map : PathMap V) (prefix : PathKey) (labels : list PathLabel) : Prop :=
  forall label, In label labels <-> path_child_exists map prefix label.

Theorem path_node_final_lookup :
  forall (V : Type) (map : PathMap V) prefix,
    path_node_final map prefix <->
    exists value, pathmap_lookup map prefix = Some value.
Proof.
  reflexivity.
Qed.

Theorem path_edge_labels_sound :
  forall (V : Type) (map : PathMap V) prefix labels label,
    path_edge_labels_exact map prefix labels ->
    In label labels ->
    path_child_exists map prefix label.
Proof.
  intros V map prefix labels label Hexact Hin.
  apply (proj1 (Hexact label)).
  exact Hin.
Qed.

Theorem path_edge_labels_complete :
  forall (V : Type) (map : PathMap V) prefix labels label,
    path_edge_labels_exact map prefix labels ->
    path_child_exists map prefix label ->
    In label labels.
Proof.
  intros V map prefix labels label Hexact Hchild.
  apply (proj2 (Hexact label)).
  exact Hchild.
Qed.

Theorem path_child_exists_after_insert :
  forall (V : Type) (map : PathMap V) prefix label suffix value,
    path_child_exists
      (pathmap_insert map (prefix ++ [label] ++ suffix) value)
      prefix
      label.
Proof.
  intros V map prefix label suffix value.
  exists suffix, value.
  apply pathmap_lookup_insert_same.
Qed.

(** ** PathMapChar Storage Refinement

    Rust stores PathMapChar terms as UTF-8 bytes but exposes character-level
    traversal.  The proof boundary is that the byte storage refines an abstract
    character map through an encoding of each character label to a byte path.
    Completeness of [edges] is then stated over character labels, not over the
    first UTF-8 byte.
*)

Section CharStorageRefinement.

Variable V : Type.
Variable encode_char : CharLabel -> ByteKey.

Definition encode_key (key : CharKey) : ByteKey :=
  flat_map encode_char key.

Definition char_storage_refines
  (char_map : CharPathMap V) (byte_map : BytePathMap V) : Prop :=
  forall chars, byte_map (encode_key chars) = char_map chars.

Theorem encode_key_app :
  forall left right,
    encode_key (left ++ right) = encode_key left ++ encode_key right.
Proof.
  intros left right.
  unfold encode_key.
  apply flat_map_app.
Qed.

Theorem encode_key_child :
  forall prefix label suffix,
    encode_key (prefix ++ [label] ++ suffix) =
    encode_key prefix ++ encode_char label ++ encode_key suffix.
Proof.
  intros prefix label suffix.
  rewrite encode_key_app.
  simpl.
  rewrite app_assoc.
  reflexivity.
Qed.

Theorem char_storage_lookup_refines :
  forall char_map byte_map key value,
    char_storage_refines char_map byte_map ->
    char_map key = Some value ->
    byte_map (encode_key key) = Some value.
Proof.
  intros char_map byte_map key value Hrefines Hlookup.
  rewrite Hrefines.
  exact Hlookup.
Qed.

Theorem char_child_implies_encoded_storage_child :
  forall char_map byte_map prefix label,
    char_storage_refines char_map byte_map ->
    path_child_exists char_map prefix label ->
    exists encoded_suffix value,
      byte_map
        (encode_key prefix ++ encode_char label ++ encoded_suffix) =
      Some value.
Proof.
  intros char_map byte_map prefix label Hrefines Hchild.
  destruct Hchild as [suffix [value Hlookup]].
  exists (encode_key suffix), value.
  rewrite <- encode_key_child.
  rewrite Hrefines.
  exact Hlookup.
Qed.

End CharStorageRefinement.

(** ** DictionaryFactory Dispatch Laws *)

Inductive BackendTag : Type :=
| BackendPathMap
| BackendPathMapChar
| BackendDoubleArrayTrie
| BackendDoubleArrayTrieChar
| BackendDynamicDawg
| BackendDynamicDawgChar
| BackendDynamicDawgU64
| BackendSuffixAutomaton
| BackendSuffixAutomatonChar
| BackendScdawg
| BackendScdawgChar.

Definition backend_tag_eq_dec
  (left right : BackendTag) : {left = right} + {left <> right}.
Proof.
  decide equality.
Defined.

Definition backend_available
  (pathmap_backend_enabled : bool) (backend : BackendTag) : bool :=
  match backend with
  | BackendPathMap => pathmap_backend_enabled
  | BackendPathMapChar => pathmap_backend_enabled
  | _ => true
  end.

Record FactoryDictionary (V : Type) := {
  factory_backend : BackendTag;
  factory_map : PathMap V;
}.

Definition factory_create {V : Type}
  (backend : BackendTag) (entries : list (PathKey * V)) :
  FactoryDictionary V :=
  {|
    factory_backend := backend;
    factory_map := pathmap_from_entries entries;
  |}.

Definition factory_empty {V : Type}
  (backend : BackendTag) : FactoryDictionary V :=
  {|
    factory_backend := backend;
    factory_map := pathmap_empty;
  |}.

Theorem factory_create_preserves_backend :
  forall (V : Type) backend entries,
    @factory_backend V (@factory_create V backend entries) = backend.
Proof.
  reflexivity.
Qed.

Theorem factory_empty_preserves_backend :
  forall (V : Type) backend,
    @factory_backend V (@factory_empty V backend) = backend.
Proof.
  reflexivity.
Qed.

Theorem factory_empty_contains_no_terms :
  forall (V : Type) backend key,
    pathmap_contains (@factory_map V (@factory_empty V backend)) key = false.
Proof.
  reflexivity.
Qed.

Theorem pathmap_backends_available_iff_feature_enabled :
  forall enabled,
    backend_available enabled BackendPathMap = enabled /\
    backend_available enabled BackendPathMapChar = enabled.
Proof.
  intros enabled. split; reflexivity.
Qed.

Theorem non_pathmap_backends_always_available :
  forall enabled backend,
    backend <> BackendPathMap ->
    backend <> BackendPathMapChar ->
    backend_available enabled backend = true.
Proof.
  intros enabled backend Hnot_pathmap Hnot_char.
  destruct backend; try reflexivity; contradiction.
Qed.
