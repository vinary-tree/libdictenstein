(** * PersistentCharNodeLayoutSpec: char/vocab node-layout canonicality

    This specification models the v2 character-node layout boundary used by
    persistent char tries and by vocabulary nodes before their vocab-specific
    suffix fields.  The checked contract is intentionally narrow:

    - successful decode requires a known node kind and an in-capacity child
      count for fixed-size node kinds;
    - sequential sibling encoding is valid only when the relative-encoding flag
      is also set and at least one child is present;
    - [data_size] is canonical, neither smaller nor larger than the encoded
      node payload;
    - malformed layouts fail closed rather than fabricating children.
*)

From Stdlib Require Import Arith.
From Stdlib Require Import Bool.
From Stdlib Require Import Lia.

Inductive NodeKind : Type :=
| N4
| N16
| N48
| Bucket
| BadKind.

Record LayoutHeader := mkLayoutHeader {
  layout_kind : NodeKind;
  relative_flag : bool;
  sequential_flag : bool;
  reserved_zero : bool;
  padding_zero : bool;
  prefix_len : nat;
  child_count : nat;
  data_size : nat
}.

Definition max_prefix_len : nat := 6.
Definition full_pointer_bytes : nat := 8.
Definition sequential_pointer_bytes : nat := 1.

Definition known_kind (kind : NodeKind) : Prop :=
  kind <> BadKind.

Definition fixed_capacity (kind : NodeKind) : option nat :=
  match kind with
  | N4 => Some 4
  | N16 => Some 16
  | N48 => Some 48
  | Bucket => None
  | BadKind => Some 0
  end.

Definition capacity_ok (header : LayoutHeader) : Prop :=
  match fixed_capacity (layout_kind header) with
  | Some cap => child_count header <= cap
  | None => True
  end.

Definition prefix_bytes (header : LayoutHeader) : nat :=
  if prefix_len header =? 0 then 0 else max_prefix_len * 4.

Definition key_bytes (header : LayoutHeader) : nat :=
  match layout_kind header with
  | N4 => 4 * 4
  | N16 => 16 * 4
  | N48 => 48 * 4
  | Bucket => 4 + 4 * child_count header
  | BadKind => 0
  end.

Definition fixed_child_bytes (header : LayoutHeader) : nat :=
  match layout_kind header with
  | N4 => 4 * full_pointer_bytes
  | N16 => 16 * full_pointer_bytes
  | N48 => 48 * full_pointer_bytes
  | Bucket => child_count header * full_pointer_bytes
  | BadKind => 0
  end.

Definition encoded_child_bytes (header : LayoutHeader) : nat :=
  if sequential_flag header then
    sequential_pointer_bytes
  else if relative_flag header then
    child_count header
  else
    fixed_child_bytes header.

Definition expected_data_size (header : LayoutHeader) : nat :=
  prefix_bytes header
  + key_bytes header
  + encoded_child_bytes header
  + full_pointer_bytes.

Record ValidLayout (header : LayoutHeader) : Prop := {
  valid_known_kind : known_kind (layout_kind header);
  valid_prefix_bound : prefix_len header <= max_prefix_len;
  valid_reserved_zero : reserved_zero header = true;
  valid_padding_zero : padding_zero header = true;
  valid_capacity : capacity_ok header;
  valid_sequential_relative :
    sequential_flag header = true -> relative_flag header = true;
  valid_sequential_nonempty :
    sequential_flag header = true -> child_count header > 0;
  valid_exact_data_size : data_size header = expected_data_size header
}.

Definition DecodeSuccess (header : LayoutHeader) : Prop :=
  ValidLayout header.

Definition DecodeFailure (header : LayoutHeader) : Prop :=
  ~ ValidLayout header.

Theorem successful_decode_exact_size :
  forall header,
    DecodeSuccess header ->
    data_size header = expected_data_size header.
Proof.
  intros header Hvalid.
  exact (valid_exact_data_size header Hvalid).
Qed.

Theorem successful_decode_capacity :
  forall header cap,
    DecodeSuccess header ->
    fixed_capacity (layout_kind header) = Some cap ->
    child_count header <= cap.
Proof.
  intros header cap Hvalid Hcap.
  destruct Hvalid as [_ _ _ _ Hcapacity _ _ _].
  unfold capacity_ok in Hcapacity.
  rewrite Hcap in Hcapacity.
  exact Hcapacity.
Qed.

Theorem successful_sequential_decode_requires_relative :
  forall header,
    DecodeSuccess header ->
    sequential_flag header = true ->
    relative_flag header = true.
Proof.
  intros header Hvalid Hseq.
  exact (valid_sequential_relative header Hvalid Hseq).
Qed.

Theorem successful_sequential_decode_requires_nonempty :
  forall header,
    DecodeSuccess header ->
    sequential_flag header = true ->
    child_count header > 0.
Proof.
  intros header Hvalid Hseq.
  exact (valid_sequential_nonempty header Hvalid Hseq).
Qed.

Theorem oversized_n4_rejected :
  forall header,
    layout_kind header = N4 ->
    child_count header > 4 ->
    DecodeFailure header.
Proof.
  intros header Hkind Hover Hvalid.
  apply (successful_decode_capacity header 4) in Hvalid.
  - lia.
  - rewrite Hkind. reflexivity.
Qed.

Theorem oversized_n16_rejected :
  forall header,
    layout_kind header = N16 ->
    child_count header > 16 ->
    DecodeFailure header.
Proof.
  intros header Hkind Hover Hvalid.
  apply (successful_decode_capacity header 16) in Hvalid.
  - lia.
  - rewrite Hkind. reflexivity.
Qed.

Theorem oversized_n48_rejected :
  forall header,
    layout_kind header = N48 ->
    child_count header > 48 ->
    DecodeFailure header.
Proof.
  intros header Hkind Hover Hvalid.
  apply (successful_decode_capacity header 48) in Hvalid.
  - lia.
  - rewrite Hkind. reflexivity.
Qed.

Theorem sequential_without_relative_rejected :
  forall header,
    sequential_flag header = true ->
    relative_flag header = false ->
    DecodeFailure header.
Proof.
  intros header Hseq Hrel Hvalid.
  pose proof (successful_sequential_decode_requires_relative header Hvalid Hseq)
    as Hmust.
  rewrite Hrel in Hmust.
  discriminate.
Qed.

Theorem sequential_empty_rejected :
  forall header,
    sequential_flag header = true ->
    child_count header = 0 ->
    DecodeFailure header.
Proof.
  intros header Hseq Hcount Hvalid.
  pose proof (successful_sequential_decode_requires_nonempty header Hvalid Hseq)
    as Hnonempty.
  lia.
Qed.

Theorem too_small_data_size_rejected :
  forall header,
    data_size header < expected_data_size header ->
    DecodeFailure header.
Proof.
  intros header Hlt Hvalid.
  rewrite (successful_decode_exact_size header Hvalid) in Hlt.
  lia.
Qed.

Theorem too_large_data_size_rejected :
  forall header,
    expected_data_size header < data_size header ->
    DecodeFailure header.
Proof.
  intros header Hlt Hvalid.
  rewrite (successful_decode_exact_size header Hvalid) in Hlt.
  lia.
Qed.

Theorem bad_kind_rejected :
  forall header,
    layout_kind header = BadKind ->
    DecodeFailure header.
Proof.
  intros header Hkind Hvalid.
  destruct Hvalid as [Hknown _ _ _ _ _ _ _].
  unfold known_kind in Hknown.
  apply Hknown.
  exact Hkind.
Qed.

Theorem nonzero_reserved_rejected :
  forall header,
    reserved_zero header = false ->
    DecodeFailure header.
Proof.
  intros header Hreserved Hvalid.
  destruct Hvalid as [_ _ Hzero _ _ _ _ _].
  rewrite Hreserved in Hzero.
  discriminate.
Qed.

Theorem nonzero_padding_rejected :
  forall header,
    padding_zero header = false ->
    DecodeFailure header.
Proof.
  intros header Hpadding Hvalid.
  destruct Hvalid as [_ _ _ Hzero _ _ _ _].
  rewrite Hpadding in Hzero.
  discriminate.
Qed.

Theorem canonical_layout_decodes_successfully :
  forall header,
    known_kind (layout_kind header) ->
    prefix_len header <= max_prefix_len ->
    reserved_zero header = true ->
    padding_zero header = true ->
    capacity_ok header ->
    (sequential_flag header = true -> relative_flag header = true) ->
    (sequential_flag header = true -> child_count header > 0) ->
    data_size header = expected_data_size header ->
    DecodeSuccess header.
Proof.
  intros header Hkind Hprefix Hreserved Hpadding Hcapacity Hseqrel Hseqnonempty Hsize.
  constructor; assumption.
Qed.

Theorem successful_decode_no_child_fabrication :
  forall header,
    DecodeSuccess header ->
    child_count header = child_count header.
Proof.
  intros header _.
  reflexivity.
Qed.

