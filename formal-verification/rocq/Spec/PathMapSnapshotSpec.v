(** * PathMapSnapshotSpec: PathMap TrieRef Snapshot/Ref Laws

    This module states the proof boundary for the zero-plumbing PathMap
    snapshot/ref adapters in [src/pathmap/snapshot.rs] and the shared TrieRef
    node layer in [src/pathmap/core.rs].

    The checked claim is intentionally public and semantic:

    - owned snapshots read the map captured at construction time;
    - borrowed refs read the map state represented by the borrow;
    - subtrie views scope every query by prefix concatenation;
    - optional length metadata affects only [len]/[is_empty], never lookup;
    - character snapshots are byte-backed views through a UTF-8-like encoding.

    The model does not prove upstream PathMap internals, UnsafeCell memory
    safety, cache locality, or native PathMap serialization.
*)

From Stdlib Require Import Lists.List.
From Stdlib Require Import Bool.Bool.
From Stdlib Require Import Arith.PeanoNat.
From Stdlib Require Import Logic.FunctionalExtensionality.
From ARTrie.Spec Require Import PathMapFactorySpec.
Import ListNotations.

Record PathMapView (V : Type) := {
  view_map : PathMap V;
  view_focus : PathKey;
  view_len : option nat;
}.

Definition snapshot_from_map {V : Type} (map : PathMap V) : PathMapView V :=
  {|
    view_map := map;
    view_focus := [];
    view_len := None;
  |}.

Definition snapshot_from_map_with_len {V : Type}
  (map : PathMap V) (len : nat) : PathMapView V :=
  {|
    view_map := map;
    view_focus := [];
    view_len := Some len;
  |}.

Definition snapshot_from_map_ref {V : Type} (map : PathMap V) :
  PathMapView V :=
  snapshot_from_map map.

Definition ref_from_map {V : Type} (map : PathMap V) : PathMapView V :=
  snapshot_from_map map.

Definition snapshot_from_trie_ref {V : Type}
  (map : PathMap V) (focus : PathKey) : PathMapView V :=
  {|
    view_map := map;
    view_focus := focus;
    view_len := None;
  |}.

Definition snapshot_with_len {V : Type}
  (view : PathMapView V) (len : nat) : PathMapView V :=
  {|
    view_map := view_map V view;
    view_focus := view_focus V view;
    view_len := Some len;
  |}.

Definition snapshot_lookup {V : Type}
  (view : PathMapView V) (suffix : PathKey) : option V :=
  pathmap_lookup (view_map V view) (view_focus V view ++ suffix).

Definition snapshot_contains {V : Type}
  (view : PathMapView V) (suffix : PathKey) : bool :=
  match snapshot_lookup view suffix with
  | Some _ => true
  | None => false
  end.

Definition snapshot_is_empty {V : Type} (view : PathMapView V) : bool :=
  match view_len V view with
  | Some 0 => true
  | Some _ => false
  | None => false
  end.

Theorem snapshot_from_map_lookup :
  forall (V : Type) (map : PathMap V) key,
    snapshot_lookup (snapshot_from_map map) key =
    pathmap_lookup map key.
Proof.
  reflexivity.
Qed.

Theorem snapshot_from_map_ref_lookup :
  forall (V : Type) (map : PathMap V) key,
    snapshot_lookup (snapshot_from_map_ref map) key =
    pathmap_lookup map key.
Proof.
  reflexivity.
Qed.

Theorem borrowed_ref_lookup_current :
  forall (V : Type) (map : PathMap V) key,
    snapshot_lookup (ref_from_map map) key =
    pathmap_lookup map key.
Proof.
  reflexivity.
Qed.

Theorem owned_snapshot_keeps_captured_value_after_source_insert :
  forall (V : Type) (source : PathMap V) key old_value new_value,
    pathmap_lookup source key = Some old_value ->
    snapshot_lookup (snapshot_from_map source) key = Some old_value /\
    pathmap_lookup (pathmap_insert source key new_value) key = Some new_value.
Proof.
  intros V source key old_value new_value Hlookup.
  split.
  - exact Hlookup.
  - apply pathmap_lookup_insert_same.
Qed.

Theorem snapshot_from_trie_ref_lookup :
  forall (V : Type) (map : PathMap V) focus suffix,
    snapshot_lookup (snapshot_from_trie_ref map focus) suffix =
    pathmap_lookup map (focus ++ suffix).
Proof.
  reflexivity.
Qed.

Theorem snapshot_from_trie_ref_empty_suffix :
  forall (V : Type) (map : PathMap V) focus,
    snapshot_lookup (snapshot_from_trie_ref map focus) [] =
    pathmap_lookup map focus.
Proof.
  intros V map focus.
  unfold snapshot_lookup, snapshot_from_trie_ref, pathmap_lookup.
  simpl.
  rewrite app_nil_r.
  reflexivity.
Qed.

Theorem snapshot_contains_lookup_some :
  forall (V : Type) (view : PathMapView V) key value,
    snapshot_lookup view key = Some value ->
    snapshot_contains view key = true.
Proof.
  intros V view key value Hlookup.
  unfold snapshot_contains.
  rewrite Hlookup.
  reflexivity.
Qed.

Theorem snapshot_contains_lookup_none :
  forall (V : Type) (view : PathMapView V) key,
    snapshot_lookup view key = None ->
    snapshot_contains view key = false.
Proof.
  intros V view key Hlookup.
  unfold snapshot_contains.
  rewrite Hlookup.
  reflexivity.
Qed.

Theorem snapshot_with_len_reports_exact :
  forall (V : Type) (view : PathMapView V) len,
    view_len V (snapshot_with_len view len) = Some len.
Proof.
  reflexivity.
Qed.

Theorem snapshot_with_len_preserves_lookup :
  forall (V : Type) (view : PathMapView V) len key,
    snapshot_lookup (snapshot_with_len view len) key =
    snapshot_lookup view key.
Proof.
  reflexivity.
Qed.

Theorem snapshot_unknown_len_is_not_reported_empty :
  forall (V : Type) (map : PathMap V),
    snapshot_is_empty (snapshot_from_map map) = false.
Proof.
  reflexivity.
Qed.

Theorem snapshot_zero_len_is_reported_empty :
  forall (V : Type) (map : PathMap V),
    snapshot_is_empty (snapshot_from_map_with_len map 0) = true.
Proof.
  reflexivity.
Qed.

Theorem snapshot_positive_len_is_not_reported_empty :
  forall (V : Type) (map : PathMap V) len,
    len <> 0 ->
    snapshot_is_empty (snapshot_from_map_with_len map len) = false.
Proof.
  intros V map len Hnonzero.
  destruct len as [| n].
  - contradiction.
  - reflexivity.
Qed.

Definition snapshot_child_exists {V : Type}
  (view : PathMapView V) (prefix : PathKey) (label : PathLabel) : Prop :=
  exists suffix value,
    snapshot_lookup view (prefix ++ [label] ++ suffix) = Some value.

Lemma scoped_child_key_assoc :
  forall (focus prefix suffix : PathKey) (label : PathLabel),
    focus ++ (prefix ++ label :: suffix) =
    (focus ++ prefix) ++ label :: suffix.
Proof.
  intros focus prefix suffix label.
  rewrite app_assoc.
  reflexivity.
Qed.

Theorem snapshot_child_exists_scoped :
  forall (V : Type) (map : PathMap V) focus prefix label,
    snapshot_child_exists (snapshot_from_trie_ref map focus) prefix label <->
    path_child_exists map (focus ++ prefix) label.
Proof.
  intros V map focus prefix label.
  split.
  - intros [suffix [value Hlookup]].
    exists suffix, value.
    unfold snapshot_lookup, snapshot_from_trie_ref in Hlookup.
    simpl in Hlookup.
    unfold pathmap_lookup in *.
    simpl.
    rewrite <- scoped_child_key_assoc.
    exact Hlookup.
  - intros [suffix [value Hlookup]].
    exists suffix, value.
    unfold snapshot_lookup, snapshot_from_trie_ref.
    simpl.
    unfold pathmap_lookup in *.
    rewrite scoped_child_key_assoc.
    exact Hlookup.
Qed.

Section CharSnapshot.

Variable encode_char : CharLabel -> ByteKey.

Definition encode_chars (key : CharKey) : ByteKey :=
  flat_map encode_char key.

Record CharPathMapView (V : Type) := {
  char_view_byte_map : BytePathMap V;
  char_view_focus : ByteKey;
  char_view_len : option nat;
}.

Definition char_snapshot_from_storage {V : Type}
  (byte_map : BytePathMap V) : CharPathMapView V :=
  {|
    char_view_byte_map := byte_map;
    char_view_focus := [];
    char_view_len := None;
  |}.

Definition char_snapshot_from_trie_ref {V : Type}
  (byte_map : BytePathMap V) (focus : ByteKey) : CharPathMapView V :=
  {|
    char_view_byte_map := byte_map;
    char_view_focus := focus;
    char_view_len := None;
  |}.

Definition char_snapshot_lookup {V : Type}
  (view : CharPathMapView V) (chars : CharKey) : option V :=
  pathmap_lookup
    (char_view_byte_map V view)
    (char_view_focus V view ++ encode_chars chars).

Definition char_snapshot_storage_refines {V : Type}
  (char_map : CharPathMap V) (byte_map : BytePathMap V) : Prop :=
  forall chars, byte_map (encode_chars chars) = char_map chars.

Theorem char_snapshot_lookup_refines_storage :
  forall (V : Type) (char_map : CharPathMap V) byte_map chars,
    char_snapshot_storage_refines char_map byte_map ->
    char_snapshot_lookup (char_snapshot_from_storage byte_map) chars =
    char_map chars.
Proof.
  intros V char_map byte_map chars Hrefines.
  unfold char_snapshot_lookup, char_snapshot_from_storage.
  simpl.
  apply Hrefines.
Qed.

Theorem char_snapshot_subtrie_lookup :
  forall (V : Type) (byte_map : BytePathMap V) focus chars,
    char_snapshot_lookup (char_snapshot_from_trie_ref byte_map focus) chars =
    pathmap_lookup byte_map (focus ++ encode_chars chars).
Proof.
  reflexivity.
Qed.

Theorem char_snapshot_empty_focus_refines_byte_root :
  forall (V : Type) (byte_map : BytePathMap V) chars,
    char_snapshot_lookup (char_snapshot_from_storage byte_map) chars =
    pathmap_lookup byte_map (encode_chars chars).
Proof.
  reflexivity.
Qed.

End CharSnapshot.
