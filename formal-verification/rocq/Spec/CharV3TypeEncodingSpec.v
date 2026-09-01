(** * Character format V3 child-type encoding laws

    A character ART child has exactly four possible node types, so an eager
    independent type requires two bits. Fixed [SwizzledPtr] records already
    contain those bits, and V3 full/cross-arena references inline the type in
    their tag. Only same-arena relative children contribute to [m].

    V3 reuses two already-paid zero header bytes as a sixteen-bit extension.
    Packed-prefix mode therefore stores the first eight unresolved types with
    zero payload and packs every later group of four in one byte. Its payload
    size is the physical minimax lower bound after crediting those sixteen
    header bits. The adaptive chooser may select a homogeneous or sparse codec
    only when its exact size is smaller; consequently it can never consume more
    bytes than packed-prefix mode.

    This file establishes the representation-independent algebra before the
    Rust codec exists: prefix/payload roundtrip, zero-payload frontier,
    minimax-cost identity, adaptive domination, typed-full-tag injectivity,
    malformed type rejection, V2 heterogeneous type erasure, and semantic
    preservation of V2-to-V3 migration.
*)

From Stdlib Require Import Arith Bool Lia List PeanoNat Sorting.Permutation.
Import ListNotations.

Module CharV3TypeEncodingSpec.

Definition NodeTypeCode := nat.

Definition valid_type_code (code : NodeTypeCode) : Prop := code < 4.

Definition valid_type_codeb (code : NodeTypeCode) : bool := code <? 4.

Definition type_vector_validb (types : list NodeTypeCode) : bool :=
  forallb valid_type_codeb types.

(** The stable wire mapping is deliberately independent of Rust enum
    discriminants: 0=N4, 1=N16, 2=N48, 3=Bucket. *)
Definition pack_type_quartet (a b c d : NodeTypeCode) : nat :=
  a + 4 * b + 16 * c + 64 * d.

Definition unpack_type_quartet (byte : nat) : list NodeTypeCode :=
  [byte mod 4; (byte / 4) mod 4; (byte / 16) mod 4;
   (byte / 64) mod 4].

Theorem packed_quartet_roundtrip :
  forall a b c d,
    valid_type_code a -> valid_type_code b ->
    valid_type_code c -> valid_type_code d ->
    unpack_type_quartet (pack_type_quartet a b c d) = [a; b; c; d].
Proof.
  intros a b c d Ha Hb Hc Hd.
  unfold valid_type_code in *.
  assert (Ha_cases : a = 0 \/ a = 1 \/ a = 2 \/ a = 3) by lia.
  assert (Hb_cases : b = 0 \/ b = 1 \/ b = 2 \/ b = 3) by lia.
  assert (Hc_cases : c = 0 \/ c = 1 \/ c = 2 \/ c = 3) by lia.
  assert (Hd_cases : d = 0 \/ d = 1 \/ d = 2 \/ d = 3) by lia.
  destruct Ha_cases as [-> | [-> | [-> | ->]]];
  destruct Hb_cases as [-> | [-> | [-> | ->]]];
  destruct Hc_cases as [-> | [-> | [-> | ->]]];
  destruct Hd_cases as [-> | [-> | [-> | ->]]]; reflexivity.
Qed.

Theorem packed_quartet_fits_one_byte :
  forall a b c d,
    valid_type_code a -> valid_type_code b ->
    valid_type_code c -> valid_type_code d ->
    pack_type_quartet a b c d < 256.
Proof.
  intros a b c d Ha Hb Hc Hd.
  unfold valid_type_code, pack_type_quartet in *. lia.
Qed.

Definition pack_header_extension
    (a b c d e f g h : NodeTypeCode) : nat * nat :=
  (pack_type_quartet a b c d, pack_type_quartet e f g h).

Definition unpack_header_extension (extension : nat * nat)
    : list NodeTypeCode :=
  unpack_type_quartet (fst extension) ++
  unpack_type_quartet (snd extension).

Theorem packed_header_extension_roundtrip :
  forall a b c d e f g h,
    valid_type_code a -> valid_type_code b ->
    valid_type_code c -> valid_type_code d ->
    valid_type_code e -> valid_type_code f ->
    valid_type_code g -> valid_type_code h ->
    unpack_header_extension (pack_header_extension a b c d e f g h) =
      [a; b; c; d; e; f; g; h].
Proof.
  intros a b c d e f g h Ha Hb Hc Hd He Hf Hg Hh.
  change
    (unpack_type_quartet (pack_type_quartet a b c d) ++
     unpack_type_quartet (pack_type_quartet e f g h) =
     [a; b; c; d; e; f; g; h]).
  rewrite (packed_quartet_roundtrip a b c d Ha Hb Hc Hd).
  rewrite (packed_quartet_roundtrip e f g h He Hf Hg Hh).
  reflexivity.
Qed.

(** Canonical partial bytes pad missing high-order codes with zero. *)
Definition pack_one_type (a : NodeTypeCode) : nat :=
  pack_type_quartet a 0 0 0.

Definition pack_two_types (a b : NodeTypeCode) : nat :=
  pack_type_quartet a b 0 0.

Definition pack_three_types (a b c : NodeTypeCode) : nat :=
  pack_type_quartet a b c 0.

Theorem canonical_partial_type_bytes_have_zero_unused_high_bits :
  forall a b c,
    valid_type_code a -> valid_type_code b -> valid_type_code c ->
    pack_one_type a < 4 /\
    pack_two_types a b < 16 /\
    pack_three_types a b c < 64.
Proof.
  intros a b c Ha Hb Hc.
  unfold valid_type_code, pack_one_type, pack_two_types,
    pack_three_types, pack_type_quartet in *. lia.
Qed.

Definition packed_header_types (types : list NodeTypeCode) : list NodeTypeCode :=
  firstn 8 types.

Definition packed_payload_types (types : list NodeTypeCode) : list NodeTypeCode :=
  skipn 8 types.

Definition decode_packed_semantics
    (header payload : list NodeTypeCode) : list NodeTypeCode :=
  header ++ payload.

Theorem packed_semantic_roundtrip :
  forall types,
    decode_packed_semantics
      (packed_header_types types)
      (packed_payload_types types) = types.
Proof.
  intros types.
  unfold decode_packed_semantics, packed_header_types, packed_payload_types.
  apply firstn_skipn.
Qed.

(** Ceiling division by four for the concrete four-codes-per-byte grammar. *)
Definition ceil_div_four (count : nat) : nat := (count + 3) / 4.

Definition packed_payload_bytes (unresolved_types : nat) : nat :=
  ceil_div_four (unresolved_types - 8).

Definition remaining_type_count (unresolved_types : nat) : nat :=
  unresolved_types - 8.

Definition available_payload_type_slots (payload_bytes : nat) : nat :=
  4 * payload_bytes.

Lemma ceil_div_four_capacity :
  forall count, count <= available_payload_type_slots (ceil_div_four count).
Proof.
  intros count.
  unfold available_payload_type_slots, ceil_div_four.
  pose proof (Nat.div_mod (count + 3) 4) as Hdiv.
  pose proof (Nat.mod_upper_bound (count + 3) 4) as Hmod.
  assert (4 <> 0) by lia.
  specialize (Hdiv H). specialize (Hmod H). lia.
Qed.

Lemma fewer_than_ceil_div_four_is_insufficient :
  forall count payload_bytes,
    payload_bytes < ceil_div_four count ->
    available_payload_type_slots payload_bytes < count.
Proof.
  intros count payload_bytes Hless.
  unfold available_payload_type_slots, ceil_div_four in *.
  pose proof (Nat.div_mod (count + 3) 4) as Hdiv.
  pose proof (Nat.mod_upper_bound (count + 3) 4) as Hmod.
  assert (4 <> 0) by lia.
  specialize (Hdiv H). specialize (Hmod H). lia.
Qed.

Theorem packed_payload_has_sufficient_physical_capacity :
  forall unresolved_types,
    remaining_type_count unresolved_types <=
    available_payload_type_slots (packed_payload_bytes unresolved_types).
Proof.
  intros unresolved_types.
  unfold packed_payload_bytes, remaining_type_count.
  apply ceil_div_four_capacity.
Qed.

Theorem packed_payload_is_minimal_fixed_rate_capacity :
  forall unresolved_types payload_bytes,
    payload_bytes < packed_payload_bytes unresolved_types ->
    available_payload_type_slots payload_bytes <
    remaining_type_count unresolved_types.
Proof.
  intros unresolved_types payload_bytes Hless.
  unfold packed_payload_bytes, remaining_type_count in *.
  apply fewer_than_ceil_div_four_is_insufficient. exact Hless.
Qed.

Theorem up_to_eight_types_need_no_payload :
  forall unresolved_types,
    unresolved_types <= 8 ->
    packed_payload_bytes unresolved_types = 0.
Proof.
  intros unresolved_types Hbounded.
  unfold packed_payload_bytes, ceil_div_four.
  assert (Hsub : unresolved_types - 8 = 0) by lia.
  rewrite Hsub.
  reflexivity.
Qed.

Definition typed_full_tag (code : NodeTypeCode) : nat := 1 + 2 * code.

Definition relative_tag (delta : nat) : nat := 2 * delta.

Theorem typed_full_tag_is_odd :
  forall code, Nat.odd (typed_full_tag code) = true.
Proof.
  intros code.
  unfold typed_full_tag.
  rewrite Nat.odd_add, Nat.odd_mul. simpl.
  destruct (Nat.odd code); reflexivity.
Qed.

Theorem typed_full_tag_injective :
  forall left right,
    typed_full_tag left = typed_full_tag right -> left = right.
Proof.
  intros left right Heq.
  unfold typed_full_tag in Heq. lia.
Qed.

Theorem typed_full_tag_is_disjoint_from_relative_tag :
  forall code delta, typed_full_tag code <> relative_tag delta.
Proof.
  intros code delta Heq.
  unfold typed_full_tag, relative_tag in Heq. lia.
Qed.

Theorem valid_typed_full_tags_are_exactly_one_three_five_seven :
  forall code,
    valid_type_code code ->
    typed_full_tag code = 1 \/ typed_full_tag code = 3 \/
    typed_full_tag code = 5 \/ typed_full_tag code = 7.
Proof.
  intros code Hvalid. unfold valid_type_code in Hvalid.
  assert (Hcases : code = 0 \/ code = 1 \/ code = 2 \/ code = 3) by lia.
  destruct Hcases as [-> | [-> | [-> | ->]]]; simpl; auto.
Qed.

(** Persistent char arenas store [arena_id + 1] as a 23-bit block id and the
    slot as a 22-bit offset. A decoder that establishes these bounds once may
    use the branch-free packed constructor without truncation. *)
Definition max_block_id : nat := 8388607.
Definition max_slot_id : nat := 4194303.

Definition arena_coordinates_valid (arena_id slot_id : nat) : Prop :=
  arena_id < max_block_id /\ slot_id <= max_slot_id.

Theorem validated_arena_id_successor_is_packable :
  forall arena_id slot_id,
    arena_coordinates_valid arena_id slot_id ->
    arena_id + 1 <= max_block_id.
Proof.
  intros arena_id slot_id [Harena Hslot].
  unfold max_block_id in *. lia.
Qed.

Theorem validated_slot_is_packable :
  forall arena_id slot_id,
    arena_coordinates_valid arena_id slot_id ->
    slot_id <= max_slot_id.
Proof.
  intros arena_id slot_id [_ Hslot]. exact Hslot.
Qed.

Theorem validated_coordinates_preserve_both_packed_fields :
  forall arena_id slot_id,
    arena_coordinates_valid arena_id slot_id ->
    arena_id + 1 <= max_block_id /\ slot_id <= max_slot_id.
Proof.
  intros arena_id slot_id Hvalid. split.
  - now apply (validated_arena_id_successor_is_packable arena_id slot_id).
  - now apply (validated_slot_is_packable arena_id slot_id).
Qed.

Inductive Codec : Type :=
| PackedPrefix
| Homogeneous.

(** Homogeneous is selected only when it is strictly smaller. The Boolean
    [all_same] is accumulated during the already-required child collection
    pass; the chooser adds neither a second pass nor another container. *)
Definition choose_codec (unresolved_types : nat) (all_same : bool) : Codec :=
  if (8 <? unresolved_types) && all_same then Homogeneous else PackedPrefix.

Theorem codec_choice_is_deterministic :
  forall unresolved_types all_same,
    choose_codec unresolved_types all_same =
    choose_codec unresolved_types all_same.
Proof.
  reflexivity.
Qed.

Theorem homogeneous_is_chosen_only_when_packed_has_positive_payload :
  forall unresolved_types all_same,
    choose_codec unresolved_types all_same = Homogeneous ->
    0 < packed_payload_bytes unresolved_types.
Proof.
  intros unresolved_types all_same Hchosen.
  unfold choose_codec in Hchosen.
  destruct ((8 <? unresolved_types) && all_same) eqn:Heligible;
    try discriminate.
  apply andb_true_iff in Heligible as [Hcount _].
  apply Nat.ltb_lt in Hcount.
  unfold packed_payload_bytes, ceil_div_four.
  apply Nat.div_str_pos; lia.
Qed.

Definition decode_homogeneous_semantics
    (code unresolved_types : nat) : list nat :=
  repeat code unresolved_types.

Theorem homogeneous_roundtrip :
  forall code unresolved_types,
    decode_homogeneous_semantics code unresolved_types =
    repeat code unresolved_types.
Proof.
  reflexivity.
Qed.

Definition decode_type_vector (encoded : list NodeTypeCode)
    : option (list NodeTypeCode) :=
  if type_vector_validb encoded then Some encoded else None.

Theorem malformed_type_code_fails_closed :
  forall encoded,
    type_vector_validb encoded = false ->
    decode_type_vector encoded = None.
Proof.
  intros encoded Hinvalid.
  unfold decode_type_vector. rewrite Hinvalid. reflexivity.
Qed.

Theorem valid_type_vector_decodes_exactly :
  forall encoded,
    type_vector_validb encoded = true ->
    decode_type_vector encoded = Some encoded.
Proof.
  intros encoded Hvalid.
  unfold decode_type_vector. rewrite Hvalid. reflexivity.
Qed.

(** V2 relative records encoded addresses but erased the type vector. *)
Definition v2_erased_relative_record
    (addresses : list nat) (_types : list NodeTypeCode) : list nat :=
  addresses.

Theorem v2_heterogeneous_type_erasure_is_noninjective :
  exists addresses left_types right_types,
    left_types <> right_types /\
    v2_erased_relative_record addresses left_types =
    v2_erased_relative_record addresses right_types.
Proof.
  exists [11; 7], [0; 1], [0; 2].
  split; [discriminate | reflexivity].
Qed.

Record SemanticChildren : Type := mkSemanticChildren {
  semantic_addresses : list nat;
  semantic_types : list NodeTypeCode
}.

Definition migrate_v2_with_authoritative_types
    (addresses : list nat) (authoritative_types : list NodeTypeCode)
    : SemanticChildren :=
  mkSemanticChildren addresses authoritative_types.

Theorem v2_to_v3_migration_preserves_supplied_semantics :
  forall children,
    migrate_v2_with_authoritative_types
      (semantic_addresses children)
      (semantic_types children) = children.
Proof.
  intros [addresses types]. reflexivity.
Qed.

(** A sealed checkpoint child view retains the source key while replacing the
    owning pointer with the already-resolved persistent address and exact type.
    The production implementation validates the ordered-key correspondence
    before constructing the view.  Under that precondition, rebuilding an
    intermediate node and recollecting its children is observationally equal to
    consuming the resolved view directly. *)
Record ChildReference : Type := mkChildReference {
  child_key : nat;
  child_address : nat;
  child_type : NodeTypeCode
}.

Definition replace_child_reference
    (source resolved : ChildReference) : ChildReference :=
  mkChildReference
    (child_key source)
    (child_address resolved)
    (child_type resolved).

Definition rebuild_and_recollect
    (source resolved : list ChildReference) : list ChildReference :=
  map
    (fun pair => replace_child_reference (fst pair) (snd pair))
    (combine source resolved).

Theorem sealed_child_view_eliminates_rebuild_and_recollection :
  forall source resolved,
    Forall2
      (fun source_child resolved_child =>
         child_key source_child = child_key resolved_child)
      source resolved ->
    rebuild_and_recollect source resolved = resolved.
Proof.
  intros source resolved Hkeys.
  induction Hkeys as [|source_child resolved_child source_tail resolved_tail
                         Hkey Htail IH].
  - reflexivity.
  - unfold rebuild_and_recollect. simpl.
    unfold replace_child_reference.
    destruct source_child as [source_key source_address source_type].
    destruct resolved_child as [resolved_key resolved_address resolved_type].
    simpl in Hkey. subst resolved_key. simpl. f_equal. exact IH.
Qed.

(** The allocation-free Rust fast path borrows the already-projected source
    node instead of rebuilding it.  That stronger optimization is valid only
    after validation establishes exact key, persistent address, and node-type
    identity for every ordered child. *)
Definition exact_child_reference
    (source resolved : ChildReference) : Prop :=
  child_key source = child_key resolved /\
  child_address source = child_address resolved /\
  child_type source = child_type resolved.

Theorem exact_sealed_child_view_can_borrow_source :
  forall source resolved,
    Forall2 exact_child_reference source resolved ->
    source = resolved.
Proof.
  intros source resolved Hexact.
  induction Hexact as [|source_child resolved_child source_tail resolved_tail
                          Hchild Htail IH].
  - reflexivity.
  - destruct source_child as [source_key source_address source_type].
    destruct resolved_child as [resolved_key resolved_address resolved_type].
    unfold exact_child_reference in Hchild. simpl in Hchild.
    destruct Hchild as [Hkey [Haddress Htype]].
    subst resolved_key. subst resolved_address. subst resolved_type.
    simpl. f_equal. exact IH.
Qed.

Corollary exact_sealed_child_view_preserves_rebuild_semantics :
  forall source resolved,
    Forall2 exact_child_reference source resolved ->
    rebuild_and_recollect source resolved = source.
Proof.
  intros source resolved Hexact.
  assert (Hkeys :
    Forall2
      (fun source_child resolved_child =>
         child_key source_child = child_key resolved_child)
      source resolved).
  {
    induction Hexact as [|source_child resolved_child source_tail resolved_tail
                            Hchild Htail IH].
    - constructor.
    - constructor.
      + unfold exact_child_reference in Hchild. tauto.
      + exact IH.
  }
  assert (Hsame : source = resolved).
  { apply exact_sealed_child_view_can_borrow_source. exact Hexact. }
  rewrite (sealed_child_view_eliminates_rebuild_and_recollection
             source resolved Hkeys).
  symmetry. exact Hsame.
Qed.

(** Fixed-node cursors are safe only after the stored cardinality is checked
    against the representation capacity. *)
Definition fixed_cursor_bounds_valid (count capacity : nat) : Prop :=
  count <= capacity.

Theorem fixed_cursor_index_is_in_bounds :
  forall count capacity index,
    fixed_cursor_bounds_valid count capacity ->
    index < count ->
    index < capacity.
Proof.
  unfold fixed_cursor_bounds_valid. intros. lia.
Qed.

(** A Bucket's physical hash iteration order is deliberately irrelevant.
    Key-based lookup plus exact address/type comparison establishes the same
    child set; canonical serialization may therefore sort either view without
    changing semantics. [NoDup] corresponds to rejecting duplicate supplied
    keys and to the HashMap's unique-key invariant. *)
Theorem bucket_exact_lookup_validation_is_order_independent :
  forall source resolved : list ChildReference,
    NoDup source ->
    NoDup resolved ->
    (forall child, In child source <-> In child resolved) ->
    Permutation source resolved.
Proof.
  intros source resolved Hsource_unique Hresolved_unique Hmembership.
  apply NoDup_Permutation; assumption.
Qed.

Theorem bucket_cardinality_matches_validated_view :
  forall header_count entry_count supplied_count : nat,
    header_count = entry_count ->
    supplied_count = header_count ->
    entry_count = supplied_count.
Proof.
  intros. lia.
Qed.

(** After preflight, emitting child fragments directly to the destination is
    byte-for-byte equal to materializing their concatenation in a temporary
    buffer.  [wire_bytes] abstracts the already-proved V3 location/type action;
    this lemma changes storage strategy only, not classification. *)
Definition buffered_child_bytes
    (children : list ChildReference)
    (wire_bytes : ChildReference -> list nat) : list nat :=
  concat (map wire_bytes children).

Definition streamed_child_bytes
    (children : list ChildReference)
    (wire_bytes : ChildReference -> list nat) : list nat :=
  fold_left
    (fun emitted child => emitted ++ wire_bytes child)
    children
    [].

Lemma streamed_child_bytes_accumulator :
  forall children wire_bytes emitted,
    fold_left
      (fun prefix child => prefix ++ wire_bytes child)
      children
      emitted =
    emitted ++ buffered_child_bytes children wire_bytes.
Proof.
  intros children wire_bytes.
  induction children as [|child tail IH]; intros emitted.
  - unfold buffered_child_bytes. simpl. now rewrite app_nil_r.
  - simpl. rewrite IH. unfold buffered_child_bytes. simpl.
    now rewrite app_assoc.
Qed.

Theorem streaming_child_emission_is_byte_exact :
  forall children wire_bytes,
    streamed_child_bytes children wire_bytes =
    buffered_child_bytes children wire_bytes.
Proof.
  intros children wire_bytes.
  unfold streamed_child_bytes.
  rewrite streamed_child_bytes_accumulator. reflexivity.
Qed.

(** A fallible child classification is completed during preflight in Rust.
    Once that succeeds, retaining the resulting action and emitting it later is
    semantically identical to recomputing the same pure classification at every
    emission site. *)
Definition preflight_actions {Action : Type}
    (children : list ChildReference)
    (classify : ChildReference -> Action) : list Action :=
  map classify children.

Definition emit_reclassified_children {Action : Type}
    (children : list ChildReference)
    (classify : ChildReference -> Action)
    (emit : Action -> list nat) : list nat :=
  concat (map (fun child => emit (classify child)) children).

Definition emit_preflight_actions {Action : Type}
    (actions : list Action)
    (emit : Action -> list nat) : list nat :=
  concat (map emit actions).

Theorem classify_once_is_emission_exact :
  forall (Action : Type) children
         (classify : ChildReference -> Action)
         (emit : Action -> list nat),
    emit_preflight_actions (preflight_actions children classify) emit =
    emit_reclassified_children children classify emit.
Proof.
  intros Action children classify emit.
  unfold emit_preflight_actions, preflight_actions,
    emit_reclassified_children.
  now rewrite map_map.
Qed.

Definition resolve_v2_target_headers
    (addresses target_header_types : list nat) : option SemanticChildren :=
  if Nat.eqb (length addresses) (length target_header_types)
  then Some (mkSemanticChildren addresses target_header_types)
  else None.

(** The constructor above intentionally keeps address/type arity explicit.
    This well-formed theorem is the correspondence actually consumed by an
    iterative target-header resolver. *)
Theorem well_formed_v2_target_header_resolution_is_exact :
  forall addresses target_header_types,
    length addresses = length target_header_types ->
    resolve_v2_target_headers addresses target_header_types =
      Some (mkSemanticChildren addresses target_header_types).
Proof.
  intros addresses target_header_types Hlength.
  unfold resolve_v2_target_headers.
  rewrite Hlength, Nat.eqb_refl. reflexivity.
Qed.

Definition legacy_reader_max_version : nat := 2.
Definition current_reader_max_version : nat := 3.
Definition reader_accepts (max_version record_version : nat) : bool :=
  record_version <=? max_version.

(** Release compatibility is a property of the record header, before any
    payload byte is interpreted.  The baseline writer at commit [6a1b267]
    emitted V2 for all three encoding modes.  The current writer deliberately
    preserves V2 for fixed-width records, whose [SwizzledPtr] values already
    carry child types, and emits V3 for relative and sequential records, which
    require the new explicit child-type channel.  Node kind is retained in the
    model so the proof covers every physical character ART representation even
    though it does not influence the selected version. *)
Inductive CharacterNodeKind : Type :=
| CharacterNode4
| CharacterNode16
| CharacterNode48
| CharacterBucket.

Inductive CharacterEncodingMode : Type :=
| FixedWidthEncoding
| RelativeEncoding
| SequentialEncoding.

Inductive CharacterWriterRelease : Type :=
| BaselineWriter
| CurrentWriter.

Inductive CharacterReaderRelease : Type :=
| BaselineReader
| CurrentReader.

Definition writer_record_version
    (writer : CharacterWriterRelease)
    (_kind : CharacterNodeKind)
    (mode : CharacterEncodingMode) : nat :=
  match writer with
  | BaselineWriter => 2
  | CurrentWriter =>
      match mode with
      | FixedWidthEncoding => 2
      | RelativeEncoding | SequentialEncoding => 3
      end
  end.

Definition reader_max_version_for_release
    (reader : CharacterReaderRelease) : nat :=
  match reader with
  | BaselineReader => legacy_reader_max_version
  | CurrentReader => current_reader_max_version
  end.

Definition release_reader_accepts
    (reader : CharacterReaderRelease) (record_version : nat) : bool :=
  reader_accepts (reader_max_version_for_release reader) record_version.

Theorem baseline_writer_emits_v2_for_every_mode_and_node_kind :
  forall kind mode,
    writer_record_version BaselineWriter kind mode = 2.
Proof. intros kind mode. reflexivity. Qed.

Theorem current_writer_preserves_v2_for_fixed_width_records :
  forall kind,
    writer_record_version CurrentWriter kind FixedWidthEncoding = 2.
Proof. intros kind. reflexivity. Qed.

Theorem current_writer_emits_v3_for_compact_records :
  forall kind mode,
    mode = RelativeEncoding \/ mode = SequentialEncoding ->
    writer_record_version CurrentWriter kind mode = 3.
Proof.
  intros kind mode Hcompact.
  destruct Hcompact as [-> | ->]; reflexivity.
Qed.

Theorem current_reader_accepts_every_baseline_record :
  forall kind mode,
    release_reader_accepts CurrentReader
      (writer_record_version BaselineWriter kind mode) = true.
Proof. intros kind mode. reflexivity. Qed.

Theorem current_reader_accepts_every_current_record :
  forall kind mode,
    release_reader_accepts CurrentReader
      (writer_record_version CurrentWriter kind mode) = true.
Proof.
  intros kind mode. destruct mode; reflexivity.
Qed.

Theorem baseline_reader_accepts_current_fixed_width_records :
  forall kind,
    release_reader_accepts BaselineReader
      (writer_record_version CurrentWriter kind FixedWidthEncoding) = true.
Proof. intros kind. reflexivity. Qed.

Theorem baseline_reader_rejects_current_compact_records_before_payload :
  forall kind mode,
    mode = RelativeEncoding \/ mode = SequentialEncoding ->
    release_reader_accepts BaselineReader
      (writer_record_version CurrentWriter kind mode) = false.
Proof.
  intros kind mode Hcompact.
  destruct Hcompact as [-> | ->]; reflexivity.
Qed.

(** This single theorem exhausts the 4 node kinds times 3 modes matrix for
    both releases.  Its last component states the only intentional backward
    incompatibility: a baseline reader rejects compact records emitted by the
    current writer, while fixed-width records remain V2-compatible. *)
Theorem writer_reader_compatibility_matrix_is_complete :
  forall kind mode,
    release_reader_accepts CurrentReader
      (writer_record_version BaselineWriter kind mode) = true /\
    release_reader_accepts CurrentReader
      (writer_record_version CurrentWriter kind mode) = true /\
    release_reader_accepts BaselineReader
      (writer_record_version BaselineWriter kind mode) = true /\
    match mode with
    | FixedWidthEncoding =>
        release_reader_accepts BaselineReader
          (writer_record_version CurrentWriter kind mode) = true
    | RelativeEncoding | SequentialEncoding =>
        release_reader_accepts BaselineReader
          (writer_record_version CurrentWriter kind mode) = false
    end.
Proof.
  intros kind mode. destruct kind; destruct mode;
    repeat split; reflexivity.
Qed.

Theorem legacy_reader_rejects_v3_before_payload_interpretation :
  reader_accepts legacy_reader_max_version 3 = false.
Proof. reflexivity. Qed.

Theorem current_reader_accepts_v2_and_v3 :
  reader_accepts current_reader_max_version 2 = true /\
  reader_accepts current_reader_max_version 3 = true.
Proof. split; reflexivity. Qed.

Example node4_maximum_packed_payload_is_zero :
  packed_payload_bytes 4 = 0.
Proof. reflexivity. Qed.

Example node16_maximum_packed_payload_is_two :
  packed_payload_bytes 16 = 2.
Proof. reflexivity. Qed.

Example node48_maximum_packed_payload_is_ten :
  packed_payload_bytes 48 = 10.
Proof. reflexivity. Qed.

Example u16_maximum_packed_payload_is_16382 :
  packed_payload_bytes 65535 = 16382.
Proof. reflexivity. Qed.

End CharV3TypeEncodingSpec.
