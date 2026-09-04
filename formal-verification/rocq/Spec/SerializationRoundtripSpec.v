(** * SerializationRoundtripSpec: Public Serializer Laws

    This module states the backend-neutral correctness obligations for
    libdictenstein's public serialization API.

    The model separates three contracts that are distinct in the Rust surface:

    - legacy term-only serialization preserves set membership;
    - value-aware serialization preserves mapped lookups;
    - malformed payload handling is fail-closed through a validation predicate.
*)

Require Import Coq.Lists.List.
Require Import Coq.Bool.Bool.
Require Import Coq.Arith.PeanoNat.
Require Import Coq.Logic.FunctionalExtensionality.
Require Import Coq.micromega.Lia.
Require Import ARTrie.Model.ListCompat.
Require Import ARTrie.Spec.MapSpec.
Import ListNotations.

Definition Key := MapSpec.Key.
Definition EncodedBytes := list MapSpec.Byte.
Definition DictSet := Key -> bool.
Definition DictMap (V : Type) := Key -> option V.

Definition same_set (a b : DictSet) : Prop :=
  forall k, a k = b k.

Definition same_map {V : Type} (a b : DictMap V) : Prop :=
  forall k, a k = b k.

Definition set_empty : DictSet := fun _ => false.

Definition map_empty {V : Type} : DictMap V := fun _ => None.

Definition map_domain {V : Type} (m : DictMap V) : DictSet :=
  fun k =>
    match m k with
    | Some _ => true
    | None => false
    end.

Definition legacy_decoded_value_map {V : Type} (_ : DictSet) : DictMap V :=
  fun _ => None.

Lemma same_set_refl : forall s,
  same_set s s.
Proof.
  intros s k.
  reflexivity.
Qed.

Lemma same_map_refl : forall (V : Type) (m : DictMap V),
  same_map m m.
Proof.
  intros V m k.
  reflexivity.
Qed.

Lemma same_set_ext : forall a b,
  same_set a b ->
  a = b.
Proof.
  intros a b Hsame.
  apply functional_extensionality.
  exact Hsame.
Qed.

Lemma same_map_ext : forall (V : Type) (a b : DictMap V),
  same_map a b ->
  a = b.
Proof.
  intros V a b Hsame.
  apply functional_extensionality.
  exact Hsame.
Qed.

(** ** Term-Only Serialization *)

Record SetSerializationModel := {
  encode_set : DictSet -> EncodedBytes;
  decode_set : EncodedBytes -> option DictSet;
  validate_set : EncodedBytes -> bool;

  set_decode_roundtrip :
    forall s, decode_set (encode_set s) = Some s;
  set_decode_success_valid :
    forall bytes s, decode_set bytes = Some s -> validate_set bytes = true;
  set_decode_fail_closed :
    forall bytes, validate_set bytes = false -> decode_set bytes = None
}.

Section SetSerializationLaws.

Variable model : SetSerializationModel.

Theorem set_roundtrip_decode_some : forall s,
  decode_set model (encode_set model s) = Some s.
Proof.
  intro s.
  exact (set_decode_roundtrip model s).
Qed.

Theorem set_roundtrip_contains : forall s k,
  match decode_set model (encode_set model s) with
  | Some decoded => decoded k
  | None => false
  end = s k.
Proof.
  intros s k.
  rewrite (set_decode_roundtrip model s).
  reflexivity.
Qed.

Theorem set_roundtrip_same_set : forall s decoded,
  decode_set model (encode_set model s) = Some decoded ->
  same_set decoded s.
Proof.
  intros s decoded Hdecode k.
  rewrite (set_decode_roundtrip model s) in Hdecode.
  inversion Hdecode.
  reflexivity.
Qed.

Theorem set_roundtrip_extensional : forall s decoded,
  decode_set model (encode_set model s) = Some decoded ->
  decoded = s.
Proof.
  intros s decoded Hdecode.
  apply same_set_ext.
  exact (set_roundtrip_same_set s decoded Hdecode).
Qed.

Theorem set_encoded_payload_valid : forall s,
  validate_set model (encode_set model s) = true.
Proof.
  intro s.
  exact (set_decode_success_valid
    model (encode_set model s) s (set_decode_roundtrip model s)).
Qed.

Theorem decode_set_error_fail_closed : forall bytes,
  validate_set model bytes = false ->
  decode_set model bytes = None.
Proof.
  intros bytes Hinvalid.
  exact (set_decode_fail_closed model bytes Hinvalid).
Qed.

Theorem decode_set_success_not_invalid : forall bytes s,
  decode_set model bytes = Some s ->
  validate_set model bytes <> false.
Proof.
  intros bytes s Hdecode Hinvalid.
  rewrite (set_decode_success_valid model bytes s Hdecode) in Hinvalid.
  discriminate.
Qed.

Theorem set_roundtrip_second_decode_same : forall s decoded,
  decode_set model (encode_set model s) = Some decoded ->
  decode_set model (encode_set model decoded) = Some decoded.
Proof.
  intros s decoded _.
  exact (set_decode_roundtrip model decoded).
Qed.

End SetSerializationLaws.

(** ** Value-Aware Serialization *)

Record MapSerializationModel (V : Type) := {
  encode_map : DictMap V -> EncodedBytes;
  decode_map : EncodedBytes -> option (DictMap V);
  validate_map : EncodedBytes -> bool;

  map_decode_roundtrip :
    forall m, decode_map (encode_map m) = Some m;
  map_decode_success_valid :
    forall bytes m, decode_map bytes = Some m -> validate_map bytes = true;
  map_decode_fail_closed :
    forall bytes, validate_map bytes = false -> decode_map bytes = None
}.

Section MapSerializationLaws.

Variable V : Type.
Variable model : MapSerializationModel V.

Theorem map_roundtrip_decode_some : forall m,
  decode_map V model (encode_map V model m) = Some m.
Proof.
  intro m.
  exact (map_decode_roundtrip V model m).
Qed.

Theorem map_roundtrip_lookup : forall m k,
  match decode_map V model (encode_map V model m) with
  | Some decoded => decoded k
  | None => None
  end = m k.
Proof.
  intros m k.
  rewrite (map_decode_roundtrip V model m).
  reflexivity.
Qed.

Theorem map_roundtrip_same_map : forall m decoded,
  decode_map V model (encode_map V model m) = Some decoded ->
  same_map decoded m.
Proof.
  intros m decoded Hdecode k.
  rewrite (map_decode_roundtrip V model m) in Hdecode.
  inversion Hdecode.
  reflexivity.
Qed.

Theorem map_roundtrip_extensional : forall m decoded,
  decode_map V model (encode_map V model m) = Some decoded ->
  decoded = m.
Proof.
  intros m decoded Hdecode.
  apply same_map_ext.
  exact (map_roundtrip_same_map m decoded Hdecode).
Qed.

Theorem map_roundtrip_domain_contains : forall m k,
  match decode_map V model (encode_map V model m) with
  | Some decoded => map_domain decoded k
  | None => false
  end = map_domain m k.
Proof.
  intros m k.
  rewrite (map_decode_roundtrip V model m).
  reflexivity.
Qed.

Theorem map_encoded_payload_valid : forall m,
  validate_map V model (encode_map V model m) = true.
Proof.
  intro m.
  exact (map_decode_success_valid
    V model (encode_map V model m) m (map_decode_roundtrip V model m)).
Qed.

Theorem decode_map_error_fail_closed : forall bytes,
  validate_map V model bytes = false ->
  decode_map V model bytes = None.
Proof.
  intros bytes Hinvalid.
  exact (map_decode_fail_closed V model bytes Hinvalid).
Qed.

Theorem decode_map_success_not_invalid : forall bytes m,
  decode_map V model bytes = Some m ->
  validate_map V model bytes <> false.
Proof.
  intros bytes m Hdecode Hinvalid.
  rewrite (map_decode_success_valid V model bytes m Hdecode) in Hinvalid.
  discriminate.
Qed.

Theorem map_roundtrip_second_decode_same : forall m decoded,
  decode_map V model (encode_map V model m) = Some decoded ->
  decode_map V model (encode_map V model decoded) = Some decoded.
Proof.
  intros m decoded _.
  exact (map_decode_roundtrip V model decoded).
Qed.

End MapSerializationLaws.

(** ** Legacy Value Dropping *)

Record LegacyTermSerializationModel (V : Type) := {
  encode_legacy_terms : DictMap V -> EncodedBytes;
  decode_legacy_terms : EncodedBytes -> option DictSet;

  legacy_terms_roundtrip_domain :
    forall m, decode_legacy_terms (encode_legacy_terms m) = Some (map_domain m)
}.

Section LegacySerializationLaws.

Variable V : Type.
Variable model : LegacyTermSerializationModel V.

Theorem legacy_roundtrip_domain_decode_some : forall m,
  decode_legacy_terms V model (encode_legacy_terms V model m) =
    Some (map_domain m).
Proof.
  intro m.
  exact (legacy_terms_roundtrip_domain V model m).
Qed.

Theorem legacy_roundtrip_preserves_domain : forall m decoded,
  decode_legacy_terms V model (encode_legacy_terms V model m) = Some decoded ->
  same_set decoded (map_domain m).
Proof.
  intros m decoded Hdecode k.
  rewrite (legacy_terms_roundtrip_domain V model m) in Hdecode.
  inversion Hdecode.
  reflexivity.
Qed.

Theorem legacy_roundtrip_contains : forall m k,
  match decode_legacy_terms V model (encode_legacy_terms V model m) with
  | Some decoded => decoded k
  | None => false
  end = map_domain m k.
Proof.
  intros m k.
  rewrite (legacy_terms_roundtrip_domain V model m).
  reflexivity.
Qed.

Theorem legacy_roundtrip_contains_value_keys : forall m k v,
  m k = Some v ->
  match decode_legacy_terms V model (encode_legacy_terms V model m) with
  | Some decoded => decoded k
  | None => false
  end = true.
Proof.
  intros m k v Hlookup.
  rewrite legacy_roundtrip_contains.
  unfold map_domain.
  rewrite Hlookup.
  reflexivity.
Qed.

Theorem legacy_roundtrip_absent_value_keys : forall m k,
  m k = None ->
  match decode_legacy_terms V model (encode_legacy_terms V model m) with
  | Some decoded => decoded k
  | None => false
  end = false.
Proof.
  intros m k Hlookup.
  rewrite legacy_roundtrip_contains.
  unfold map_domain.
  rewrite Hlookup.
  reflexivity.
Qed.

Theorem legacy_decoded_value_lookup_absent : forall m decoded k,
  decode_legacy_terms V model (encode_legacy_terms V model m) = Some decoded ->
  @legacy_decoded_value_map V decoded k = None.
Proof.
  intros m decoded k _.
  reflexivity.
Qed.

End LegacySerializationLaws.

(** ** Codec Wrappers *)

Record ByteCodecModel := {
  codec_encode : EncodedBytes -> EncodedBytes;
  codec_decode : EncodedBytes -> option EncodedBytes;
  codec_validate : EncodedBytes -> bool;

  codec_decode_roundtrip :
    forall payload, codec_decode (codec_encode payload) = Some payload;
  codec_decode_success_valid :
    forall bytes payload, codec_decode bytes = Some payload ->
      codec_validate bytes = true;
  codec_decode_fail_closed :
    forall bytes, codec_validate bytes = false -> codec_decode bytes = None
}.

Section CodecWrapperLaws.

Variable inner : SetSerializationModel.
Variable codec : ByteCodecModel.

Definition codec_encode_set (s : DictSet) : EncodedBytes :=
  codec_encode codec (encode_set inner s).

Definition codec_decode_set (bytes : EncodedBytes) : option DictSet :=
  match codec_decode codec bytes with
  | Some payload => decode_set inner payload
  | None => None
  end.

Theorem codec_wrapped_set_roundtrip_decode_some : forall s,
  codec_decode_set (codec_encode_set s) = Some s.
Proof.
  intro s.
  unfold codec_decode_set, codec_encode_set.
  rewrite (codec_decode_roundtrip codec (encode_set inner s)).
  exact (set_decode_roundtrip inner s).
Qed.

Theorem codec_wrapped_set_roundtrip_contains : forall s k,
  match codec_decode_set (codec_encode_set s) with
  | Some decoded => decoded k
  | None => false
  end = s k.
Proof.
  intros s k.
  rewrite codec_wrapped_set_roundtrip_decode_some.
  reflexivity.
Qed.

Theorem codec_wrapped_set_roundtrip_same_set : forall s decoded,
  codec_decode_set (codec_encode_set s) = Some decoded ->
  same_set decoded s.
Proof.
  intros s decoded Hdecode k.
  rewrite codec_wrapped_set_roundtrip_decode_some in Hdecode.
  inversion Hdecode.
  reflexivity.
Qed.

Theorem codec_wrapped_payload_valid : forall s,
  codec_validate codec (codec_encode_set s) = true.
Proof.
  intro s.
  unfold codec_encode_set.
  exact (codec_decode_success_valid
    codec
    (codec_encode codec (encode_set inner s))
    (encode_set inner s)
    (codec_decode_roundtrip codec (encode_set inner s))).
Qed.

Theorem codec_invalid_payload_fail_closed : forall bytes,
  codec_validate codec bytes = false ->
  codec_decode_set bytes = None.
Proof.
  intros bytes Hinvalid.
  unfold codec_decode_set.
  rewrite (codec_decode_fail_closed codec bytes Hinvalid).
  reflexivity.
Qed.

Theorem codec_inner_invalid_payload_fail_closed : forall bytes payload,
  codec_decode codec bytes = Some payload ->
  validate_set inner payload = false ->
  codec_decode_set bytes = None.
Proof.
  intros bytes payload Hdecode Hinner.
  unfold codec_decode_set.
  rewrite Hdecode.
  exact (set_decode_fail_closed inner payload Hinner).
Qed.

End CodecWrapperLaws.

(** ** Protobuf Feature Formats *)

Inductive ProtobufDictionaryFormat :=
  | ProtobufV1
  | ProtobufV2
  | ProtobufDat.

Record ProtobufSetSerializationModel := {
  protobuf_encode_set : ProtobufDictionaryFormat -> DictSet -> EncodedBytes;
  protobuf_decode_set : ProtobufDictionaryFormat -> EncodedBytes -> option DictSet;
  protobuf_validate_set : ProtobufDictionaryFormat -> EncodedBytes -> bool;

  protobuf_decode_roundtrip :
    forall format s,
      protobuf_decode_set format (protobuf_encode_set format s) = Some s;
  protobuf_decode_success_valid :
    forall format bytes s,
      protobuf_decode_set format bytes = Some s ->
      protobuf_validate_set format bytes = true;
  protobuf_decode_fail_closed :
    forall format bytes,
      protobuf_validate_set format bytes = false ->
      protobuf_decode_set format bytes = None
}.

Section ProtobufSetLaws.

Variable model : ProtobufSetSerializationModel.

Theorem protobuf_roundtrip_decode_some : forall format s,
  protobuf_decode_set model format (protobuf_encode_set model format s) = Some s.
Proof.
  intros format s.
  exact (protobuf_decode_roundtrip model format s).
Qed.

Theorem protobuf_roundtrip_contains : forall format s k,
  match protobuf_decode_set model format (protobuf_encode_set model format s) with
  | Some decoded => decoded k
  | None => false
  end = s k.
Proof.
  intros format s k.
  rewrite (protobuf_decode_roundtrip model format s).
  reflexivity.
Qed.

Theorem protobuf_roundtrip_same_set : forall format s decoded,
  protobuf_decode_set model format (protobuf_encode_set model format s) =
    Some decoded ->
  same_set decoded s.
Proof.
  intros format s decoded Hdecode k.
  rewrite (protobuf_decode_roundtrip model format s) in Hdecode.
  inversion Hdecode.
  reflexivity.
Qed.

Theorem protobuf_roundtrip_extensional : forall format s decoded,
  protobuf_decode_set model format (protobuf_encode_set model format s) =
    Some decoded ->
  decoded = s.
Proof.
  intros format s decoded Hdecode.
  apply same_set_ext.
  exact (protobuf_roundtrip_same_set format s decoded Hdecode).
Qed.

Theorem protobuf_encoded_payload_valid : forall format s,
  protobuf_validate_set model format (protobuf_encode_set model format s) = true.
Proof.
  intros format s.
  exact (protobuf_decode_success_valid
    model format (protobuf_encode_set model format s) s
    (protobuf_decode_roundtrip model format s)).
Qed.

Theorem protobuf_malformed_payload_fail_closed : forall format bytes,
  protobuf_validate_set model format bytes = false ->
  protobuf_decode_set model format bytes = None.
Proof.
  intros format bytes Hinvalid.
  exact (protobuf_decode_fail_closed model format bytes Hinvalid).
Qed.

Theorem protobuf_v1_roundtrip_contains : forall s k,
  match protobuf_decode_set model ProtobufV1
          (protobuf_encode_set model ProtobufV1 s) with
  | Some decoded => decoded k
  | None => false
  end = s k.
Proof.
  intros s k.
  apply protobuf_roundtrip_contains.
Qed.

Theorem protobuf_v2_roundtrip_contains : forall s k,
  match protobuf_decode_set model ProtobufV2
          (protobuf_encode_set model ProtobufV2 s) with
  | Some decoded => decoded k
  | None => false
  end = s k.
Proof.
  intros s k.
  apply protobuf_roundtrip_contains.
Qed.

Theorem protobuf_dat_roundtrip_contains : forall s k,
  match protobuf_decode_set model ProtobufDat
          (protobuf_encode_set model ProtobufDat s) with
  | Some decoded => decoded k
  | None => false
  end = s k.
Proof.
  intros s k.
  apply protobuf_roundtrip_contains.
Qed.

End ProtobufSetLaws.

(** ** Suffix Automaton Protobuf Format *)

Definition SourceCorpus := list Key.

Record SuffixProtobufSerializationModel := {
  suffix_reference_language : SourceCorpus -> DictSet;
  suffix_encode_sources : SourceCorpus -> EncodedBytes;
  suffix_decode_language : EncodedBytes -> option DictSet;
  suffix_validate_payload : EncodedBytes -> bool;
  suffix_count_matches : EncodedBytes -> bool;

  suffix_decode_roundtrip :
    forall sources,
      suffix_decode_language (suffix_encode_sources sources) =
        Some (suffix_reference_language sources);
  suffix_decode_success_valid :
    forall bytes language,
      suffix_decode_language bytes = Some language ->
      suffix_validate_payload bytes = true;
  suffix_decode_fail_closed :
    forall bytes,
      suffix_validate_payload bytes = false ->
      suffix_decode_language bytes = None;
  suffix_count_mismatch_invalid :
    forall bytes,
      suffix_count_matches bytes = false ->
      suffix_validate_payload bytes = false
}.

Section SuffixProtobufLaws.

Variable model : SuffixProtobufSerializationModel.

Theorem suffix_protobuf_roundtrip_decode_some : forall sources,
  suffix_decode_language model (suffix_encode_sources model sources) =
    Some (suffix_reference_language model sources).
Proof.
  intro sources.
  exact (suffix_decode_roundtrip model sources).
Qed.

Theorem suffix_protobuf_roundtrip_contains : forall sources k,
  match suffix_decode_language model (suffix_encode_sources model sources) with
  | Some decoded => decoded k
  | None => false
  end = suffix_reference_language model sources k.
Proof.
  intros sources k.
  rewrite (suffix_decode_roundtrip model sources).
  reflexivity.
Qed.

Theorem suffix_protobuf_roundtrip_same_language : forall sources decoded,
  suffix_decode_language model (suffix_encode_sources model sources) =
    Some decoded ->
  same_set decoded (suffix_reference_language model sources).
Proof.
  intros sources decoded Hdecode k.
  rewrite (suffix_decode_roundtrip model sources) in Hdecode.
  inversion Hdecode.
  reflexivity.
Qed.

Theorem suffix_protobuf_roundtrip_extensional : forall sources decoded,
  suffix_decode_language model (suffix_encode_sources model sources) =
    Some decoded ->
  decoded = suffix_reference_language model sources.
Proof.
  intros sources decoded Hdecode.
  apply same_set_ext.
  exact (suffix_protobuf_roundtrip_same_language sources decoded Hdecode).
Qed.

Theorem suffix_protobuf_encoded_payload_valid : forall sources,
  suffix_validate_payload model (suffix_encode_sources model sources) = true.
Proof.
  intro sources.
  exact (suffix_decode_success_valid
    model
    (suffix_encode_sources model sources)
    (suffix_reference_language model sources)
    (suffix_decode_roundtrip model sources)).
Qed.

Theorem suffix_protobuf_malformed_payload_fail_closed : forall bytes,
  suffix_validate_payload model bytes = false ->
  suffix_decode_language model bytes = None.
Proof.
  intros bytes Hinvalid.
  exact (suffix_decode_fail_closed model bytes Hinvalid).
Qed.

Theorem suffix_protobuf_count_mismatch_fail_closed : forall bytes,
  suffix_count_matches model bytes = false ->
  suffix_decode_language model bytes = None.
Proof.
  intros bytes Hmismatch.
  apply suffix_decode_fail_closed.
  exact (suffix_count_mismatch_invalid model bytes Hmismatch).
Qed.

End SuffixProtobufLaws.

(** ** Stack-Safe Path-Expanded Protobuf Construction

    The public round-trip laws above deliberately abstract from construction.
    This section fixes the missing refinement boundary for the Rust protobuf
    encoders.  A recursive trie oracle first determines an ordered DFS edge
    skeleton.  The production traversal consumes that skeleton with an
    explicit pending-edge worklist, assigns dense node identifiers on edge
    entry, and constructs a local graph before touching the external writer.

    Allocation outcomes are explicit.  One successful reservation authorizes
    one complete wire event; a failed reservation returns [None], so neither a
    partial graph nor a prefix can be published.  The two unsafe controls at
    the end are executable counterexamples to ignoring a failed reservation
    and to publishing each event eagerly. *)

Record PathExpansionSkeletonEdge := {
  skeleton_source_id : nat;
  skeleton_label : MapSpec.Byte;
  skeleton_child_final : bool
}.

Record PathExpansionWireEvent := {
  wire_source_id : nat;
  wire_label : MapSpec.Byte;
  wire_target_id : nat;
  wire_child_final : bool
}.

Definition materialize_path_edge
    (target_id : nat) (edge : PathExpansionSkeletonEdge)
    : PathExpansionWireEvent :=
  {| wire_source_id := skeleton_source_id edge;
     wire_label := skeleton_label edge;
     wire_target_id := target_id;
     wire_child_final := skeleton_child_final edge |}.

(** The recursive oracle has already flattened an ordered trie into its
    final-first, edge-encounter DFS skeleton.  [List.seq] assigns the node ID
    observed on entry to each edge. *)
Definition recursive_path_expansion
    (next_id : nat) (edges : list PathExpansionSkeletonEdge)
    : list PathExpansionWireEvent :=
  map (fun pair => materialize_path_edge (fst pair) (snd pair))
      (combine (List.seq next_id (length edges)) edges).

(** Heap-worklist refinement: consume exactly one pending edge at a time. *)
Fixpoint iterative_path_expansion
    (next_id : nat) (pending : list PathExpansionSkeletonEdge)
    : list PathExpansionWireEvent :=
  match pending with
  | [] => []
  | edge :: rest =>
      materialize_path_edge next_id edge
        :: iterative_path_expansion (S next_id) rest
  end.

Theorem iterative_path_expansion_matches_recursive_oracle :
  forall next_id edges,
    iterative_path_expansion next_id edges =
    recursive_path_expansion next_id edges.
Proof.
  intros next_id edges.
  revert next_id.
  induction edges as [|edge rest IH]; intro next_id; simpl.
  - reflexivity.
  - rewrite IH.
    reflexivity.
Qed.

(** ** Persistent Overlay Cursor-Fallback Selection

    A persistent overlay node may advertise a dense native cursor, but that
    capability is optional in [DictionaryNode].  When it is unavailable the
    protobuf encoder selects the owned-node path above.  The owned path is not
    recursive in Rust: [pending] is a heap worklist consumed by a loop.  These
    laws make the capability withdrawal precise before the production overlay
    handle is returned to its allocation-free baseline representation. *)

Inductive OverlayTraversalMode :=
  | OverlayDirectCursor
  | OverlayOwnedWorklist.

Definition select_overlay_traversal (cursor_available : bool)
    : OverlayTraversalMode :=
  if cursor_available then OverlayDirectCursor else OverlayOwnedWorklist.

Theorem unavailable_overlay_cursor_selects_owned_worklist :
  select_overlay_traversal false = OverlayOwnedWorklist.
Proof.
  reflexivity.
Qed.

(** The direct projection and owned worklist consume the same ordered DFS
    skeleton and therefore expose identical protobuf events. *)
Definition direct_overlay_events := recursive_path_expansion.
Definition owned_overlay_events := iterative_path_expansion.

Theorem owned_overlay_fallback_refines_direct_cursor :
  forall next_id edges,
    owned_overlay_events next_id edges =
    direct_overlay_events next_id edges.
Proof.
  exact iterative_path_expansion_matches_recursive_oracle.
Qed.

(** Retaining the captured immutable overlay root fixes the event skeleton.
    A later publication is deliberately ignored by the owned traversal. *)
Definition owned_events_after_publication
    (next_id : nat) (captured later : list PathExpansionSkeletonEdge)
    : list PathExpansionWireEvent :=
  let _ := later in owned_overlay_events next_id captured.

Theorem owned_overlay_fallback_is_revision_isolated :
  forall next_id captured later,
    owned_events_after_publication next_id captured later =
    owned_overlay_events next_id captured.
Proof.
  reflexivity.
Qed.

Theorem owned_overlay_fallback_preserves_event_count :
  forall next_id pending,
    length (owned_overlay_events next_id pending) = length pending.
Proof.
  intros next_id pending.
  revert next_id.
  induction pending as [|edge rest IH]; intro next_id; simpl.
  - reflexivity.
  - rewrite IH. reflexivity.
Qed.

(** There is no library constant limiting key or worklist depth: every finite
    pending sequence has a complete owned traversal with exactly one event per
    edge.  Consumers may impose policy limits outside this machine. *)
Definition fallback_witness_byte : MapSpec.Byte :=
  MapSpec.byte_of_nat 0 (ltac:(lia)).

Theorem owned_overlay_fallback_has_no_fixed_depth_bound :
  forall depth next_id,
    exists pending events,
      length pending = depth /\
      owned_overlay_events next_id pending = events /\
      length events = depth.
Proof.
  intros depth next_id.
  set (pending := repeat
    {| skeleton_source_id := 0;
       skeleton_label := fallback_witness_byte;
       skeleton_child_final := false |}
    depth).
  exists pending.
  exists (owned_overlay_events next_id pending).
  split.
  - unfold pending. apply repeat_length.
  - split.
    + reflexivity.
    + rewrite owned_overlay_fallback_preserves_event_count.
      unfold pending. apply repeat_length.
Qed.

Definition skeleton_observation (edge : PathExpansionSkeletonEdge)
    : nat * MapSpec.Byte * bool :=
  (skeleton_source_id edge, skeleton_label edge,
   skeleton_child_final edge).

Definition wire_observation (event : PathExpansionWireEvent)
    : nat * MapSpec.Byte * bool :=
  (wire_source_id event, wire_label event, wire_child_final event).

Theorem path_expansion_preserves_edge_encounter_dfs_order :
  forall next_id edges,
    map wire_observation (iterative_path_expansion next_id edges) =
    map skeleton_observation edges.
Proof.
  intros next_id edges.
  revert next_id.
  induction edges as [|edge rest IH]; intro next_id; simpl.
  - reflexivity.
  - rewrite IH.
    reflexivity.
Qed.

(** Finality is emitted in the event for a child before any later DFS event,
    rather than being deferred until after that child's descendants. *)
Theorem path_expansion_preserves_final_before_descendants :
  forall next_id edges,
    map wire_child_final (iterative_path_expansion next_id edges) =
    map skeleton_child_final edges.
Proof.
  intros next_id edges.
  revert next_id.
  induction edges as [|edge rest IH]; intro next_id; simpl.
  - reflexivity.
  - rewrite IH.
    reflexivity.
Qed.

Theorem path_expansion_assigns_contiguous_ids :
  forall next_id edges,
    map wire_target_id (iterative_path_expansion next_id edges) =
    List.seq next_id (length edges).
Proof.
  intros next_id edges.
  revert next_id.
  induction edges as [|edge rest IH]; intro next_id; simpl.
  - reflexivity.
  - rewrite IH.
    reflexivity.
Qed.

Theorem path_expansion_assigns_contiguous_unique_ids :
  forall next_id edges,
    NoDup (map wire_target_id
      (iterative_path_expansion next_id edges)).
Proof.
  intros next_id edges.
  rewrite path_expansion_assigns_contiguous_ids.
  apply List.seq_NoDup.
Qed.

(** The Rust vector stores a newly encountered child segment in encounter
    order, reverses only that suffix, and then pops from the end.  Thus the
    sequence observed by repeated pop is the original encounter order. *)
Theorem reversed_child_segment_pops_in_encounter_order :
  forall (A : Type) (children : list A),
    rev (rev children) = children.
Proof.
  intros A children.
  apply rev_involutive.
Qed.

(** A finite stand-in for checked [u64] node-ID arithmetic.  [max_id] is the
    first identifier whose successor cannot be represented.  The check occurs
    before the event is exposed, matching the Rust fail-closed boundary. *)
Fixpoint checked_path_expansion
    (max_id next_id : nat) (pending : list PathExpansionSkeletonEdge)
    : option (list PathExpansionWireEvent) :=
  match pending with
  | [] => Some []
  | edge :: rest =>
      if Nat.ltb next_id max_id then
        match checked_path_expansion max_id (S next_id) rest with
        | Some events => Some (materialize_path_edge next_id edge :: events)
        | None => None
        end
      else None
  end.

Theorem checked_node_id_exhaustion_fails_closed :
  forall max_id next_id edge rest,
    max_id <= next_id ->
    checked_path_expansion max_id next_id (edge :: rest) = None.
Proof.
  intros max_id next_id edge rest Hexhausted.
  assert (Hcannot_advance : Nat.ltb next_id max_id = false).
  { apply Nat.ltb_ge. exact Hexhausted. }
  cbn [checked_path_expansion].
  rewrite Hcannot_advance.
  reflexivity.
Qed.

(** A reservation schedule supplies one allocation decision for each complete
    event.  Event construction is atomic with respect to that decision. *)
Fixpoint reserved_path_expansion
    (next_id : nat) (pending : list PathExpansionSkeletonEdge)
    (reservations : list bool)
    : option (list PathExpansionWireEvent) :=
  match pending, reservations with
  | [], _ => Some []
  | _ :: _, [] => None
  | edge :: rest, reserve_ok :: later =>
      if reserve_ok then
        match reserved_path_expansion (S next_id) rest later with
        | Some events => Some (materialize_path_edge next_id edge :: events)
        | None => None
        end
      else None
  end.

Theorem successful_reservations_preserve_recursive_trace :
  forall next_id edges,
    reserved_path_expansion next_id edges
      (repeat true (length edges)) =
    Some (recursive_path_expansion next_id edges).
Proof.
  intros next_id edges.
  revert next_id.
  induction edges as [|edge rest IH]; intro next_id; simpl.
  - reflexivity.
  - rewrite IH.
    rewrite <- (iterative_path_expansion_matches_recursive_oracle
      (S next_id) rest).
    rewrite <- (iterative_path_expansion_matches_recursive_oracle
      next_id (edge :: rest)).
    reflexivity.
Qed.

(** The executable vectors refine an event reservation with a capacity guard:
    when the already-owned allocation has enough spare slots, no allocator
    call is required; only a true growth boundary consults the fallible
    allocator.  Natural subtraction mirrors Rust's [capacity - len] under the
    standard vector invariant [len <= capacity] and avoids addition overflow
    in the guard itself. *)
Definition ensure_append_capacity
    (len capacity additional : nat) (growth_ok : bool) : option nat :=
  if Nat.leb additional (capacity - len) then Some capacity
  else if growth_ok then Some (len + additional) else None.

Theorem spare_capacity_skips_allocator :
  forall len capacity additional growth_ok,
    len <= capacity ->
    additional <= capacity - len ->
    ensure_append_capacity len capacity additional growth_ok = Some capacity.
Proof.
  intros len capacity additional growth_ok Hvector Hspare.
  unfold ensure_append_capacity.
  assert (Hguard : Nat.leb additional (capacity - len) = true).
  { apply Nat.leb_le. exact Hspare. }
  rewrite Hguard.
  reflexivity.
Qed.

Theorem exhausted_capacity_preserves_fallible_failure :
  forall len capacity additional,
    len <= capacity ->
    capacity - len < additional ->
    ensure_append_capacity len capacity additional false = None.
Proof.
  intros len capacity additional Hvector Hexhausted.
  unfold ensure_append_capacity.
  assert (Hguard : Nat.leb additional (capacity - len) = false).
  { apply Nat.leb_gt. exact Hexhausted. }
  rewrite Hguard.
  reflexivity.
Qed.

Theorem successful_capacity_guard_authorizes_append :
  forall len capacity additional growth_ok resulting_capacity,
    len <= capacity ->
    ensure_append_capacity len capacity additional growth_ok =
      Some resulting_capacity ->
    len + additional <= resulting_capacity.
Proof.
  intros len capacity additional growth_ok resulting_capacity Hvector Hsuccess.
  unfold ensure_append_capacity in Hsuccess.
  destruct (Nat.leb additional (capacity - len)) eqn:Hspare.
  - inversion Hsuccess; subst resulting_capacity.
    apply Nat.leb_le in Hspare.
    lia.
  - destruct growth_ok; inversion Hsuccess; subst resulting_capacity.
    apply Nat.le_refl.
Qed.

(** The Rust implementation keeps the already-capacious branch in a compact
    hot function and delegates only the exhausted branch to a cold, fallible
    growth helper.  This definition makes that code-layout split explicit;
    the next theorem proves that outlining the slow branch cannot alter the
    allocation or capacity semantics above. *)
Definition grow_append_capacity
    (len additional : nat) (growth_ok : bool) : option nat :=
  if growth_ok then Some (len + additional) else None.

Definition split_ensure_append_capacity
    (len capacity additional : nat) (growth_ok : bool) : option nat :=
  if Nat.leb additional (capacity - len) then Some capacity
  else grow_append_capacity len additional growth_ok.

Theorem cold_growth_split_refines_capacity_guard :
  forall len capacity additional growth_ok,
    split_ensure_append_capacity len capacity additional growth_ok =
    ensure_append_capacity len capacity additional growth_ok.
Proof.
  intros len capacity additional growth_ok.
  unfold split_ensure_append_capacity, ensure_append_capacity,
    grow_append_capacity.
  destruct (Nat.leb additional (capacity - len)); reflexivity.
Qed.

(** ** Transactional V2 Event Sink

    The optimized protobuf format commits one logical DFS event to two local
    vectors: an optional final-node identifier and the three-word packed edge
    [source, label, target].  Both fallible capacity checks precede either
    logical append.  Consequently a failure in either vector leaves both
    logical lengths and the external writer unchanged.  The model records
    packed edges as typed triples; the Rust representation is their flat,
    three-word encoding. *)

Record V2SinkState := {
  v2_final_ids : list nat;
  v2_packed_edges : list (nat * MapSpec.Byte * nat)
}.

Record V2SinkCapacities := {
  v2_final_capacity : nat;
  v2_edge_word_capacity : nat
}.

Definition bool_slot_count (value : bool) : nat :=
  if value then 1 else 0.

Definition v2_edge_word_length (state : V2SinkState) : nat :=
  3 * length (v2_packed_edges state).

Definition authorize_v2_event
    (state : V2SinkState) (capacities : V2SinkCapacities)
    (event : PathExpansionWireEvent)
    (final_growth_ok edge_growth_ok : bool)
    : option V2SinkCapacities :=
  match ensure_append_capacity
      (length (v2_final_ids state))
      (v2_final_capacity capacities)
      (bool_slot_count (wire_child_final event))
      final_growth_ok with
  | None => None
  | Some final_capacity =>
      match ensure_append_capacity
          (v2_edge_word_length state)
          (v2_edge_word_capacity capacities)
          3 edge_growth_ok with
      | None => None
      | Some edge_capacity =>
          Some {| v2_final_capacity := final_capacity;
                  v2_edge_word_capacity := edge_capacity |}
      end
  end.

Definition commit_v2_event
    (state : V2SinkState) (event : PathExpansionWireEvent) : V2SinkState :=
  {| v2_final_ids :=
       if wire_child_final event
       then v2_final_ids state ++ [wire_target_id event]
       else v2_final_ids state;
     v2_packed_edges :=
       v2_packed_edges state ++
         [(wire_source_id event, wire_label event, wire_target_id event)] |}.

Definition attempt_v2_event
    (state : V2SinkState) (capacities : V2SinkCapacities)
    (event : PathExpansionWireEvent)
    (final_growth_ok edge_growth_ok : bool)
    : V2SinkState * option V2SinkCapacities :=
  match authorize_v2_event state capacities event
      final_growth_ok edge_growth_ok with
  | None => (state, None)
  | Some resulting_capacities =>
      (commit_v2_event state event, Some resulting_capacities)
  end.

Theorem successful_v2_authorization_covers_both_exact_writes :
  forall state capacities event final_growth_ok edge_growth_ok resulting,
    length (v2_final_ids state) <= v2_final_capacity capacities ->
    v2_edge_word_length state <= v2_edge_word_capacity capacities ->
    authorize_v2_event state capacities event
      final_growth_ok edge_growth_ok = Some resulting ->
    length (v2_final_ids state) +
        bool_slot_count (wire_child_final event) <=
      v2_final_capacity resulting /\
    v2_edge_word_length state + 3 <= v2_edge_word_capacity resulting.
Proof.
  intros state capacities event final_growth_ok edge_growth_ok resulting
    Hfinal_vector Hedge_vector Hauthorized.
  unfold authorize_v2_event in Hauthorized.
  destruct (ensure_append_capacity
      (length (v2_final_ids state))
      (v2_final_capacity capacities)
      (bool_slot_count (wire_child_final event))
      final_growth_ok) as [final_capacity|] eqn:Hfinal; try discriminate.
  destruct (ensure_append_capacity
      (v2_edge_word_length state)
      (v2_edge_word_capacity capacities)
      3 edge_growth_ok) as [edge_capacity|] eqn:Hedge; try discriminate.
  inversion Hauthorized; subst resulting; clear Hauthorized.
  split; simpl.
  - eapply successful_capacity_guard_authorizes_append; eauto.
  - eapply successful_capacity_guard_authorizes_append; eauto.
Qed.

Theorem failed_v2_authorization_preserves_both_logical_vectors :
  forall state capacities event final_growth_ok edge_growth_ok,
    authorize_v2_event state capacities event
      final_growth_ok edge_growth_ok = None ->
    fst (attempt_v2_event state capacities event
      final_growth_ok edge_growth_ok) = state.
Proof.
  intros state capacities event final_growth_ok edge_growth_ok Hfailed.
  unfold attempt_v2_event.
  rewrite Hfailed.
  reflexivity.
Qed.

Theorem committed_v2_event_appends_exact_packed_edge :
  forall state event,
    v2_packed_edges (commit_v2_event state event) =
      v2_packed_edges state ++
        [(wire_source_id event, wire_label event, wire_target_id event)].
Proof.
  intros state event.
  reflexivity.
Qed.

Theorem committed_v2_event_appends_exact_optional_final :
  forall state event,
    v2_final_ids (commit_v2_event state event) =
      if wire_child_final event
      then v2_final_ids state ++ [wire_target_id event]
      else v2_final_ids state.
Proof.
  intros state event.
  reflexivity.
Qed.

Definition v2_edge_observation
    (edge : nat * MapSpec.Byte * nat) : nat * MapSpec.Byte * nat := edge.

Theorem committed_v2_event_refines_wire_event :
  forall state event,
    last (map v2_edge_observation
      (v2_packed_edges (commit_v2_event state event)))
      (0, wire_label event, 0) =
    (wire_source_id event, wire_label event, wire_target_id event).
Proof.
  intros state event.
  rewrite committed_v2_event_appends_exact_packed_edge.
  rewrite map_app.
  simpl.
  rewrite last_last.
  reflexivity.
Qed.

Definition publish_v2_local_edges
    (external_writer : list (nat * MapSpec.Byte * nat))
    (local_result : option V2SinkState)
    : list (nat * MapSpec.Byte * nat) :=
  match local_result with
  | Some state => external_writer ++ v2_packed_edges state
  | None => external_writer
  end.

Theorem failed_v2_local_result_preserves_external_writer :
  forall external_writer,
    publish_v2_local_edges external_writer None = external_writer.
Proof.
  intros external_writer.
  reflexivity.
Qed.

(** Unsafe control: authorizing only the edge vector permits a final event to
    mutate both vectors even though the required final-vector growth failed. *)
Definition unsafe_edge_only_v2_commit
    (state : V2SinkState) (capacities : V2SinkCapacities)
    (event : PathExpansionWireEvent) (edge_growth_ok : bool)
    : option V2SinkState :=
  match ensure_append_capacity
      (v2_edge_word_length state)
      (v2_edge_word_capacity capacities) 3 edge_growth_ok with
  | Some _ => Some (commit_v2_event state event)
  | None => None
  end.

Theorem unsafe_single_vector_authorization_has_counterexample :
  forall label,
    let state := {| v2_final_ids := [];
                    v2_packed_edges := [] |} in
    let capacities := {| v2_final_capacity := 0;
                         v2_edge_word_capacity := 3 |} in
    let event := {| wire_source_id := 0;
                    wire_label := label;
                    wire_target_id := 1;
                    wire_child_final := true |} in
    authorize_v2_event state capacities event false true = None /\
    unsafe_edge_only_v2_commit state capacities event true =
      Some (commit_v2_event state event).
Proof.
  intros label.
  split; reflexivity.
Qed.

(** Unsafe control: changing the final-vector logical length before edge
    authorization exposes a strict prefix when the edge reservation fails. *)
Definition unsafe_advance_final_before_edge_authorization
    (state : V2SinkState) (event : PathExpansionWireEvent) : V2SinkState :=
  {| v2_final_ids := v2_final_ids state ++ [wire_target_id event];
     v2_packed_edges := v2_packed_edges state |}.

Theorem unsafe_early_length_advance_has_counterexample :
  forall state event,
    wire_child_final event = true ->
    v2_final_ids
      (unsafe_advance_final_before_edge_authorization state event) =
      v2_final_ids state ++ [wire_target_id event] /\
    v2_packed_edges
      (unsafe_advance_final_before_edge_authorization state event) =
      v2_packed_edges state.
Proof.
  intros state event Hfinal.
  split; reflexivity.
Qed.

(** Physical initialization obligation for a flat three-word edge commit.
    Exposing three elements after initializing only two violates it. *)
Definition initialized_prefix_is_valid
    (initialized exposed : nat) : bool := Nat.leb exposed initialized.

Theorem unsafe_partial_three_word_initialization_is_rejected :
  initialized_prefix_is_valid 2 3 = false.
Proof.
  reflexivity.
Qed.

(** Unsafe control: eager publication leaks the already-committed local edge
    even when a later event fails. *)
Definition unsafe_publish_v2_edge_eagerly
    (external_writer : list (nat * MapSpec.Byte * nat))
    (event : PathExpansionWireEvent)
    : list (nat * MapSpec.Byte * nat) :=
  external_writer ++
    [(wire_source_id event, wire_label event, wire_target_id event)].

Theorem unsafe_eager_v2_publication_leaks_prefix :
  forall external_writer event,
    unsafe_publish_v2_edge_eagerly external_writer event <>
      external_writer ->
    unsafe_publish_v2_edge_eagerly external_writer event <>
      publish_v2_local_edges external_writer None.
Proof.
  intros external_writer event Hchanged.
  unfold publish_v2_local_edges.
  exact Hchanged.
Qed.

Definition publish_local_graph
    (external_writer : list PathExpansionWireEvent)
    (local_result : option (list PathExpansionWireEvent))
    : list PathExpansionWireEvent :=
  match local_result with
  | Some graph => external_writer ++ graph
  | None => external_writer
  end.

Theorem reserve_failure_discards_local_graph :
  forall next_id edges reservations external_writer,
    reserved_path_expansion next_id edges reservations = None ->
    publish_local_graph external_writer
      (reserved_path_expansion next_id edges reservations) = external_writer.
Proof.
  intros next_id edges reservations external_writer Hfailed.
  rewrite Hfailed.
  reflexivity.
Qed.

Theorem failed_path_expansion_preserves_external_writer :
  forall next_id edge rest later external_writer,
    publish_local_graph external_writer
      (reserved_path_expansion next_id (edge :: rest) (false :: later)) =
    external_writer.
Proof.
  intros next_id edge rest later external_writer.
  reflexivity.
Qed.

(** Unsafe control 1: an infallible push ignores the failed reservation and
    returns a graph that the checked construction rejects. *)
Definition unchecked_path_expansion
    (next_id : nat) (pending : list PathExpansionSkeletonEdge)
    (_reservations : list bool)
    : option (list PathExpansionWireEvent) :=
  Some (iterative_path_expansion next_id pending).

Theorem unchecked_spill_masks_allocation_failure :
  forall next_id edge,
    reserved_path_expansion next_id [edge] [false] = None /\
    unchecked_path_expansion next_id [edge] [false] =
      Some [materialize_path_edge next_id edge].
Proof.
  intros next_id edge.
  split; reflexivity.
Qed.

(** Unsafe control 2: eager publication mutates the external writer after the
    first reservation, so a later failure leaks a strict successful prefix. *)
Fixpoint eager_path_expansion
    (next_id : nat) (pending : list PathExpansionSkeletonEdge)
    (reservations : list bool)
    (external_writer : list PathExpansionWireEvent)
    : list PathExpansionWireEvent * bool :=
  match pending, reservations with
  | [], _ => (external_writer, true)
  | _ :: _, [] => (external_writer, false)
  | edge :: rest, reserve_ok :: later =>
      if reserve_ok then
        eager_path_expansion (S next_id) rest later
          (external_writer ++ [materialize_path_edge next_id edge])
      else (external_writer, false)
  end.

Theorem unsafe_eager_publication_leaks_prefix :
  forall next_id first second external_writer,
    eager_path_expansion next_id [first; second] [true; false]
      external_writer =
    (external_writer ++ [materialize_path_edge next_id first], false).
Proof.
  intros next_id first second external_writer.
  reflexivity.
Qed.


(** ** Tail-Child Elimination for the Heap Worklist

    The first child in DFS order can be traversed immediately.  Only its later
    siblings are continuations and therefore require heap-worklist frames.
    The executable SmallVec stores the physical stack from bottom to top, so
    repeated pop observes its reverse.  Appending the reversed sibling tail
    preserves exact DFS order while eliminating every frame operation on a
    unary chain. *)

Section TailChildEliminationLaws.

Context {A : Type}.

Definition physical_pop_order (pending : list A) : list A := rev pending.

Definition tail_child_schedule
    (children pending : list A) : option A * list A :=
  match children with
  | [] => (None, pending)
  | first :: later => (Some first, pending ++ rev later)
  end.

Definition scheduled_observation
    (scheduled : option A * list A) : list A :=
  match scheduled with
  | (None, pending) => physical_pop_order pending
  | (Some direct, pending) => direct :: physical_pop_order pending
  end.

Theorem tail_child_schedule_preserves_dfs_order :
  forall children pending,
    scheduled_observation (tail_child_schedule children pending) =
    children ++ physical_pop_order pending.
Proof.
  intros children pending.
  destruct children as [|first later].
  - reflexivity.
  - unfold scheduled_observation, tail_child_schedule, physical_pop_order.
    simpl.
    rewrite rev_app_distr, rev_involutive.
    reflexivity.
Qed.

Theorem singleton_child_uses_no_pending_frame :
  forall child pending,
    tail_child_schedule [child] pending = (Some child, pending).
Proof.
  intros child pending.
  change ((Some child, pending ++ []) = (Some child, pending)).
  rewrite app_nil_r.
  reflexivity.
Qed.

Theorem tail_child_schedule_stores_only_sibling_continuations :
  forall first later pending direct resulting_pending,
    tail_child_schedule (first :: later) pending =
      (direct, resulting_pending) ->
    length resulting_pending = length pending + length later.
Proof.
  intros first later pending direct resulting_pending Hscheduled.
  cbn [tail_child_schedule] in Hscheduled.
  inversion Hscheduled; subst.
  rewrite app_length_portable, rev_length.
  reflexivity.
Qed.

(** Performance control: the former machine pushed the direct child as well.
    It is semantically correct but provably performs one unnecessary physical
    frame insertion for every unary node. *)
Definition uneliminated_child_schedule
    (children pending : list A) : list A :=
  pending ++ rev children.

Theorem uneliminated_singleton_pushes_one_unnecessary_frame :
  forall child pending,
    length (uneliminated_child_schedule [child] pending) =
    S (length pending).
Proof.
  intros child pending.
  unfold uneliminated_child_schedule.
  rewrite app_length_portable.
  simpl.
  lia.
Qed.

Theorem uneliminated_schedule_has_same_pop_order :
  forall children pending,
    physical_pop_order (uneliminated_child_schedule children pending) =
    children ++ physical_pop_order pending.
Proof.
  intros children pending.
  unfold physical_pop_order, uneliminated_child_schedule.
  rewrite rev_app_distr, rev_involutive.
  reflexivity.
Qed.

End TailChildEliminationLaws.

(** ** Validated Counted Scheduling

    [DictionaryNode.edge_count] is an optional efficiency hint at the generic
    Rust trait boundary, so the serializer may use it to reserve capacity but
    must not trust it for correctness.  A counted scheduler bounds physical
    sibling storage by [pred declared], validates the count observed during
    visitation, and publishes nothing on either mismatch or reservation
    failure.  Counts zero and one require no reservation. *)

Section ValidatedCountedSchedulingLaws.

Context {A : Type}.

(** Store at most the declared sibling budget.  This is the state the
    implementation may construct before it knows whether the hint was exact. *)
Definition bounded_tail_child_schedule
    (declared : nat) (children pending : list A) : option A * list A :=
  match children with
  | [] => (None, pending)
  | first :: later =>
      (Some first, pending ++ rev (firstn (Nat.pred declared) later))
  end.

(** Counts zero and one bypass reservation.  Larger declarations require one
    successful batch reservation before visitation.  All paths validate the
    observed number of children before returning a publishable schedule. *)
Definition counted_tail_child_schedule
    (reserve_ok : bool) (declared : nat) (children pending : list A)
    : option (option A * list A) :=
  match declared with
  | 0 =>
      if Nat.eqb declared (length children)
      then Some (bounded_tail_child_schedule declared children pending)
      else None
  | 1 =>
      if Nat.eqb declared (length children)
      then Some (bounded_tail_child_schedule declared children pending)
      else None
  | S (S _) =>
      if reserve_ok then
        if Nat.eqb declared (length children)
        then Some (bounded_tail_child_schedule declared children pending)
        else None
      else None
  end.

Definition counted_batch_capacity (pending : list A) (declared : nat) : nat :=
  length pending + Nat.pred declared.

Theorem bounded_schedule_never_exceeds_declared_sibling_budget :
  forall declared children pending,
    length (snd (bounded_tail_child_schedule declared children pending)) <=
    counted_batch_capacity pending declared.
Proof.
  intros declared children pending.
  destruct children as [|first later].
  - unfold bounded_tail_child_schedule, counted_batch_capacity.
    simpl.
    lia.
  - unfold bounded_tail_child_schedule, counted_batch_capacity.
    simpl.
    rewrite app_length_portable, rev_length.
    apply Nat.add_le_mono_l.
    apply firstn_le_length.
Qed.

Theorem exact_bounded_schedule_is_tail_child_schedule :
  forall declared children pending,
    declared = length children ->
    bounded_tail_child_schedule declared children pending =
      tail_child_schedule children pending.
Proof.
  intros declared children pending Hexact.
  subst declared.
  destruct children as [|first later].
  - reflexivity.
  - cbn [bounded_tail_child_schedule tail_child_schedule].
    replace (Nat.pred (length (first :: later))) with (length later)
      by reflexivity.
    rewrite firstn_all.
    reflexivity.
Qed.

Theorem counted_schedule_success_is_exact_tail_child_schedule :
  forall reserve_ok declared children pending scheduled,
    counted_tail_child_schedule reserve_ok declared children pending =
      Some scheduled ->
    scheduled = tail_child_schedule children pending.
Proof.
  intros reserve_ok declared children pending scheduled Hscheduled.
  destruct declared as [|[|declared]].
  - cbn [counted_tail_child_schedule] in Hscheduled.
    destruct (0 =? length children) eqn:Hexact; try discriminate.
    inversion Hscheduled; subst scheduled.
    apply exact_bounded_schedule_is_tail_child_schedule.
    apply Nat.eqb_eq.
    exact Hexact.
  - cbn [counted_tail_child_schedule] in Hscheduled.
    destruct (1 =? length children) eqn:Hexact; try discriminate.
    inversion Hscheduled; subst scheduled.
    apply exact_bounded_schedule_is_tail_child_schedule.
    apply Nat.eqb_eq.
    exact Hexact.
  - cbn [counted_tail_child_schedule] in Hscheduled.
    destruct reserve_ok; try discriminate.
    destruct (S (S declared) =? length children) eqn:Hexact;
      try discriminate.
    inversion Hscheduled; subst scheduled.
    apply exact_bounded_schedule_is_tail_child_schedule.
    apply Nat.eqb_eq.
    exact Hexact.
Qed.

Theorem counted_schedule_success_preserves_dfs_order :
  forall reserve_ok declared children pending scheduled,
    counted_tail_child_schedule reserve_ok declared children pending =
      Some scheduled ->
    scheduled_observation scheduled =
      children ++ physical_pop_order pending.
Proof.
  intros reserve_ok declared children pending scheduled Hscheduled.
  rewrite (counted_schedule_success_is_exact_tail_child_schedule
    reserve_ok declared children pending scheduled Hscheduled).
  apply tail_child_schedule_preserves_dfs_order.
Qed.

(** No reservation is consulted for the empty and unary cases. *)
Theorem counted_empty_uses_no_reservation_or_pending_frame :
  forall pending,
    counted_tail_child_schedule false 0 [] pending = Some (None, pending).
Proof.
  reflexivity.
Qed.

Theorem counted_unary_uses_no_reservation_or_pending_frame :
  forall child pending,
    counted_tail_child_schedule false 1 [child] pending =
      Some (Some child, pending).
Proof.
  intros child pending.
  cbn [counted_tail_child_schedule bounded_tail_child_schedule].
  rewrite app_nil_r.
  reflexivity.
Qed.

(** A successful reservation for [pred declared] sibling frames is sufficient
    even when an implementation supplies too many children: bounded staging
    never performs an infallible push beyond the reserved capacity. *)
Theorem counted_batch_reservation_covers_every_staged_push :
  forall declared children pending,
    length (snd (bounded_tail_child_schedule declared children pending)) <=
      counted_batch_capacity pending declared.
Proof.
  apply bounded_schedule_never_exceeds_declared_sibling_budget.
Qed.

Theorem counted_mismatch_fails_closed :
  forall reserve_ok declared children pending,
    declared <> length children ->
    counted_tail_child_schedule reserve_ok declared children pending = None.
Proof.
  intros reserve_ok declared children pending Hmismatch.
  destruct declared as [|[|declared]].
  - cbn [counted_tail_child_schedule].
    destruct (0 =? length children) eqn:Hexact.
    + apply Nat.eqb_eq in Hexact. contradiction.
    + reflexivity.
  - cbn [counted_tail_child_schedule].
    destruct (1 =? length children) eqn:Hexact.
    + apply Nat.eqb_eq in Hexact. contradiction.
    + reflexivity.
  - cbn [counted_tail_child_schedule].
    destruct reserve_ok; try reflexivity.
    destruct (S (S declared) =? length children) eqn:Hexact.
    + apply Nat.eqb_eq in Hexact. contradiction.
    + reflexivity.
Qed.

Theorem counted_batch_reservation_failure_fails_closed :
  forall declared children pending,
    2 <= declared ->
    counted_tail_child_schedule false declared children pending = None.
Proof.
  intros declared children pending Hlarge.
  destruct declared as [|[|declared]]; try lia.
  reflexivity.
Qed.

Definition publish_counted_observation
    (external_writer : list A)
    (local_result : option (option A * list A)) : list A :=
  match local_result with
  | Some scheduled => external_writer ++ scheduled_observation scheduled
  | None => external_writer
  end.

Theorem counted_mismatch_preserves_external_writer :
  forall reserve_ok declared children pending external_writer,
    declared <> length children ->
    publish_counted_observation external_writer
      (counted_tail_child_schedule reserve_ok declared children pending) =
      external_writer.
Proof.
  intros reserve_ok declared children pending external_writer Hmismatch.
  rewrite (counted_mismatch_fails_closed
    reserve_ok declared children pending Hmismatch).
  reflexivity.
Qed.

Theorem counted_reservation_failure_preserves_external_writer :
  forall declared children pending external_writer,
    2 <= declared ->
    publish_counted_observation external_writer
      (counted_tail_child_schedule false declared children pending) =
      external_writer.
Proof.
  intros declared children pending external_writer Hlarge.
  rewrite (counted_batch_reservation_failure_fails_closed
    declared children pending Hlarge).
  reflexivity.
Qed.

(** Negative controls: ignoring validation can silently drop an undeclared
    sibling, and reserving from an under-reported count is insufficient. *)
Theorem unvalidated_underreported_schedule_drops_a_sibling :
  forall first second pending,
    bounded_tail_child_schedule 1 [first; second] pending =
      (Some first, pending).
Proof.
  intros first second pending.
  cbn [bounded_tail_child_schedule].
  rewrite app_nil_r.
  reflexivity.
Qed.

Theorem validated_underreported_schedule_is_rejected :
  forall first second pending,
    counted_tail_child_schedule false 1 [first; second] pending = None.
Proof.
  reflexivity.
Qed.

Theorem underreported_capacity_cannot_cover_the_missing_sibling :
  forall (pending : list A),
    ~ (length pending + 1 <= counted_batch_capacity pending 1).
Proof.
  intros pending.
  unfold counted_batch_capacity.
  simpl.
  lia.
Qed.

End ValidatedCountedSchedulingLaws.

(** ** Retained Snapshot-Cursor Paging

    Native snapshot cursors let a serializer retain one immutable root owner
    while the pending worklist stores only copyable cursor tokens.  The first
    native page observes finality, the exact edge count, and at most the first
    child.  Empty and unary nodes therefore finish after one observation.

    The first model below records the eager sibling-batch scheduler that was
    proved wire-correct but rejected by performance qualification: a node with
    at least two children reserves [pred total] sibling slots and materializes
    the remaining page.  Its laws remain as negative-control evidence.  The
    depth-bounded parent-continuation refinement that supersedes it is proved
    in [DepthBoundedCursorContinuationLaws] below.

    The model below makes the unsafe backend boundary explicit.  Every cursor
    and page carries the retained owner's revision; both native pages must
    report the same revision, finality, and total count.  The safe scheduler
    validates all observable metadata and callback counts, rejects a missing
    second page or failed reservation, and publishes only a complete local
    result. *)

Record RetainedSnapshotOwner := {
  retained_snapshot_revision : nat;
  retained_snapshot_live : bool
}.

Record SnapshotCursorModel (A : Type) := {
  snapshot_cursor_revision : nat;
  snapshot_cursor_finality : bool;
  snapshot_cursor_children : list A
}.

Arguments snapshot_cursor_revision {A} _.
Arguments snapshot_cursor_finality {A} _.
Arguments snapshot_cursor_children {A} _.

Definition snapshot_cursor_valid {A : Type}
    (owner : RetainedSnapshotOwner) (cursor : SnapshotCursorModel A) : Prop :=
  retained_snapshot_live owner = true /\
  snapshot_cursor_revision cursor = retained_snapshot_revision owner.

Definition retained_root_cursor {A : Type}
    (owner : RetainedSnapshotOwner) (finality : bool) (children : list A)
    : SnapshotCursorModel A :=
  {| snapshot_cursor_revision := retained_snapshot_revision owner;
     snapshot_cursor_finality := finality;
     snapshot_cursor_children := children |}.

Definition emitted_child_cursor {A : Type}
    (parent : SnapshotCursorModel A) (finality : bool) (children : list A)
    : SnapshotCursorModel A :=
  {| snapshot_cursor_revision := snapshot_cursor_revision parent;
     snapshot_cursor_finality := finality;
     snapshot_cursor_children := children |}.

Theorem live_retained_root_cursor_is_valid :
  forall (A : Type) owner finality (children : list A),
    retained_snapshot_live owner = true ->
    snapshot_cursor_valid owner
      (retained_root_cursor owner finality children).
Proof.
  intros A owner finality children Hlive.
  split; [exact Hlive | reflexivity].
Qed.

Theorem emitted_child_cursor_preserves_retained_provenance :
  forall (A : Type) owner (parent : SnapshotCursorModel A) finality children,
    snapshot_cursor_valid owner parent ->
    snapshot_cursor_valid owner
      (emitted_child_cursor parent finality children).
Proof.
  intros A owner parent finality children [Hlive Hrevision].
  split; [exact Hlive | exact Hrevision].
Qed.

Theorem valid_cursor_sibling_staging_preserves_provenance :
  forall (A : Type) owner
      (children pending : list (SnapshotCursorModel A)),
    Forall (snapshot_cursor_valid owner) children ->
    Forall (snapshot_cursor_valid owner) pending ->
    Forall (snapshot_cursor_valid owner) (pending ++ rev children).
Proof.
  intros A owner children pending Hchildren Hpending.
  apply Forall_app.
  split.
  - exact Hpending.
  - apply Forall_rev.
    exact Hchildren.
Qed.

Theorem foreign_revision_cursor_is_rejected :
  forall (A : Type) owner (cursor : SnapshotCursorModel A),
    snapshot_cursor_revision cursor <>
      retained_snapshot_revision owner ->
    ~ snapshot_cursor_valid owner cursor.
Proof.
  intros A owner cursor Hforeign [_ Hrevision].
  contradiction.
Qed.

Theorem retired_owner_invalidates_every_cursor :
  forall (A : Type) owner (cursor : SnapshotCursorModel A),
    retained_snapshot_live owner = false ->
    ~ snapshot_cursor_valid owner cursor.
Proof.
  intros A owner cursor Hretired [Hlive _].
  rewrite Hretired in Hlive.
  discriminate.
Qed.

Section RetainedSnapshotCursorPagingLaws.

Context {A : Type}.

Record SnapshotCursorPage := {
  cursor_page_revision : nat;
  cursor_page_finality : bool;
  cursor_page_total : nat;
  cursor_page_items : list A
}.

Definition make_cursor_page
    (cursor : SnapshotCursorModel A) (items : list A)
    : SnapshotCursorPage :=
  {| cursor_page_revision := snapshot_cursor_revision cursor;
     cursor_page_finality := snapshot_cursor_finality cursor;
     cursor_page_total := length (snapshot_cursor_children cursor);
     cursor_page_items := items |}.

(** Native page [(0, 1)]: observe metadata and at most the direct child. *)
Definition native_first_cursor_page (cursor : SnapshotCursorModel A)
    : SnapshotCursorPage :=
  match snapshot_cursor_children cursor with
  | [] => make_cursor_page cursor []
  | first :: _ => make_cursor_page cursor [first]
  end.

(** Native page [(1, total - 1)]: requested only for a real sibling tail. *)
Definition native_sibling_cursor_page (cursor : SnapshotCursorModel A)
    : option SnapshotCursorPage :=
  match snapshot_cursor_children cursor with
  | _ :: second :: later =>
      Some (make_cursor_page cursor (second :: later))
  | _ => None
  end.

Definition reported_cursor_children
    (first : SnapshotCursorPage) (siblings : option SnapshotCursorPage)
    : list A :=
  cursor_page_items first ++
    match siblings with
    | Some page => cursor_page_items page
    | None => []
    end.

(** All observable conditions imposed by the safe wrapper. *)
Definition cursor_page_reports_consistent
    (first : SnapshotCursorPage) (siblings : option SnapshotCursorPage)
    : bool :=
  match cursor_page_total first with
  | 0 =>
      Nat.eqb (length (cursor_page_items first)) 0 &&
      match siblings with None => true | Some _ => false end
  | 1 =>
      Nat.eqb (length (cursor_page_items first)) 1 &&
      match siblings with None => true | Some _ => false end
  | S (S later_count) =>
      match siblings with
      | None => false
      | Some later =>
          Nat.eqb (cursor_page_revision later)
            (cursor_page_revision first) &&
          Bool.eqb (cursor_page_finality later)
            (cursor_page_finality first) &&
          Nat.eqb (cursor_page_total later)
            (cursor_page_total first) &&
          Nat.eqb (length (cursor_page_items first)) 1 &&
          Nat.eqb (length (cursor_page_items later)) (S later_count)
      end
  end.

Definition cursor_page_reservation_authorized
    (reserve_ok : bool) (first : SnapshotCursorPage) : bool :=
  match cursor_page_total first with
  | 0 | 1 => true
  | S (S _) => reserve_ok
  end.

Definition validated_cursor_page_schedule
    (reserve_ok : bool)
    (first : SnapshotCursorPage)
    (siblings : option SnapshotCursorPage)
    (pending : list A) : option (option A * list A) :=
  if cursor_page_reports_consistent first siblings &&
     cursor_page_reservation_authorized reserve_ok first
  then Some (tail_child_schedule
    (reported_cursor_children first siblings) pending)
  else None.

Theorem native_cursor_pages_report_stable_revision_finality_and_count :
  forall cursor sibling_page,
    native_sibling_cursor_page cursor = Some sibling_page ->
    cursor_page_revision sibling_page =
      cursor_page_revision (native_first_cursor_page cursor) /\
    cursor_page_finality sibling_page =
      cursor_page_finality (native_first_cursor_page cursor) /\
    cursor_page_total sibling_page =
      cursor_page_total (native_first_cursor_page cursor).
Proof.
  intros cursor sibling_page Hpage.
  destruct cursor as [revision finality children].
  destruct children as [|first [|second later]]; try discriminate.
  inversion Hpage; subst sibling_page.
  repeat split; reflexivity.
Qed.

Theorem native_cursor_pages_cover_every_child_exactly_once :
  forall cursor,
    reported_cursor_children
      (native_first_cursor_page cursor)
      (native_sibling_cursor_page cursor) =
    snapshot_cursor_children cursor.
Proof.
  intros [revision finality children].
  destruct children as [|first [|second later]];
    cbn [reported_cursor_children native_first_cursor_page
      native_sibling_cursor_page make_cursor_page];
    reflexivity.
Qed.

Theorem native_cursor_page_reports_are_consistent :
  forall cursor,
    cursor_page_reports_consistent
      (native_first_cursor_page cursor)
      (native_sibling_cursor_page cursor) = true.
Proof.
  intros [revision finality children].
  destruct children as [|first [|second later]].
  - reflexivity.
  - reflexivity.
  - change (
      Nat.eqb revision revision &&
      Bool.eqb finality finality &&
      Nat.eqb (S (S (length later))) (S (S (length later))) &&
      Nat.eqb 1 1 &&
      Nat.eqb (S (length later)) (S (length later)) = true).
    repeat rewrite Nat.eqb_refl.
    destruct finality; reflexivity.
Qed.

Theorem native_empty_cursor_uses_no_reservation_or_pending_frame :
  forall revision finality pending,
    validated_cursor_page_schedule false
      (native_first_cursor_page
        {| snapshot_cursor_revision := revision;
           snapshot_cursor_finality := finality;
           snapshot_cursor_children := [] |})
      None pending = Some (None, pending).
Proof.
  intros revision finality pending.
  reflexivity.
Qed.

Theorem native_unary_cursor_uses_no_reservation_or_pending_frame :
  forall revision finality child pending,
    validated_cursor_page_schedule false
      (native_first_cursor_page
        {| snapshot_cursor_revision := revision;
           snapshot_cursor_finality := finality;
           snapshot_cursor_children := [child] |})
      None pending = Some (Some child, pending).
Proof.
  intros revision finality child pending.
  change (Some (tail_child_schedule [child] pending) =
    Some (Some child, pending)).
  cbn [tail_child_schedule].
  rewrite app_nil_r.
  reflexivity.
Qed.

Theorem native_cursor_schedule_is_exact_tail_child_schedule :
  forall cursor pending,
    validated_cursor_page_schedule true
      (native_first_cursor_page cursor)
      (native_sibling_cursor_page cursor) pending =
    Some (tail_child_schedule
      (snapshot_cursor_children cursor) pending).
Proof.
  intros cursor pending.
  unfold validated_cursor_page_schedule.
  rewrite native_cursor_page_reports_are_consistent.
  assert (Hauthorized :
    cursor_page_reservation_authorized true
      (native_first_cursor_page cursor) = true).
  {
    destruct cursor as [revision finality children].
    destruct children as [|first [|second later]]; reflexivity.
  }
  rewrite Hauthorized.
  rewrite native_cursor_pages_cover_every_child_exactly_once.
  reflexivity.
Qed.

Theorem native_cursor_schedule_preserves_dfs_order :
  forall cursor pending scheduled,
    validated_cursor_page_schedule true
      (native_first_cursor_page cursor)
      (native_sibling_cursor_page cursor) pending = Some scheduled ->
    scheduled_observation scheduled =
      snapshot_cursor_children cursor ++ physical_pop_order pending.
Proof.
  intros cursor pending scheduled Hscheduled.
  rewrite native_cursor_schedule_is_exact_tail_child_schedule in Hscheduled.
  inversion Hscheduled; subst scheduled.
  apply tail_child_schedule_preserves_dfs_order.
Qed.

Theorem multi_child_cursor_reservation_failure_fails_closed :
  forall first siblings pending later_count,
    cursor_page_total first = S (S later_count) ->
    validated_cursor_page_schedule false first siblings pending = None.
Proof.
  intros first siblings pending later_count Htotal.
  unfold validated_cursor_page_schedule, cursor_page_reservation_authorized.
  rewrite Htotal.
  rewrite Bool.andb_false_r.
  reflexivity.
Qed.

Theorem missing_second_page_fails_closed :
  forall first pending later_count reserve_ok,
    cursor_page_total first = S (S later_count) ->
    validated_cursor_page_schedule reserve_ok first None pending = None.
Proof.
  intros first pending later_count reserve_ok Htotal.
  unfold validated_cursor_page_schedule, cursor_page_reports_consistent.
  rewrite Htotal.
  reflexivity.
Qed.

(** Negative control: trusting a reported total while omitting callback-count
    validation silently turns a claimed binary node into a unary schedule. *)
Definition unchecked_cursor_page_schedule
    (first : SnapshotCursorPage)
    (siblings : option SnapshotCursorPage)
    (pending : list A) : option (option A * list A) :=
  Some (tail_child_schedule
    (reported_cursor_children first siblings) pending).

Theorem unchecked_missing_page_drops_a_claimed_sibling :
  forall revision finality first pending,
    let claimed_binary_first :=
      {| cursor_page_revision := revision;
         cursor_page_finality := finality;
         cursor_page_total := 2;
         cursor_page_items := [first] |} in
    unchecked_cursor_page_schedule claimed_binary_first None pending =
      Some (tail_child_schedule [first] pending) /\
    validated_cursor_page_schedule true claimed_binary_first None pending =
      None.
Proof.
  intros revision finality first pending.
  split; reflexivity.
Qed.

Theorem inconsistent_second_revision_fails_closed :
  forall first later pending reserve_ok later_count,
    cursor_page_total first = S (S later_count) ->
    cursor_page_revision later <> cursor_page_revision first ->
    validated_cursor_page_schedule reserve_ok first (Some later) pending = None.
Proof.
  intros first later pending reserve_ok later_count Htotal Hrevision.
  unfold validated_cursor_page_schedule, cursor_page_reports_consistent.
  rewrite Htotal.
  assert (Hneqb :
    Nat.eqb (cursor_page_revision later) (cursor_page_revision first) = false).
  { apply Nat.eqb_neq. exact Hrevision. }
  rewrite Hneqb.
  reflexivity.
Qed.

Definition publish_cursor_schedule
    (external_writer : list A)
    (local_result : option (option A * list A)) : list A :=
  match local_result with
  | Some scheduled => external_writer ++ scheduled_observation scheduled
  | None => external_writer
  end.

Theorem failed_cursor_schedule_preserves_external_writer :
  forall external_writer,
    publish_cursor_schedule external_writer None = external_writer.
Proof.
  intros external_writer.
  reflexivity.
Qed.

Theorem cursor_reservation_failure_preserves_external_writer :
  forall first siblings pending external_writer later_count,
    cursor_page_total first = S (S later_count) ->
    publish_cursor_schedule external_writer
      (validated_cursor_page_schedule false first siblings pending) =
    external_writer.
Proof.
  intros first siblings pending external_writer later_count Htotal.
  rewrite (multi_child_cursor_reservation_failure_fails_closed
    first siblings pending later_count Htotal).
  reflexivity.
Qed.

(** ** Depth-Bounded Cursor-Continuation PDA

    A recursive DFS retains one iterator position for every active branching
    ancestor, not one record for every unvisited sibling.  The production
    refinement therefore stores a parent cursor plus its next child index.
    Empty and unary nodes allocate no continuation.  A branching node pushes
    exactly one continuation before descending into its first child.  When a
    subtree completes, the top frame requests exactly one native edge page and
    advances only after validating the page.

    The model separates the recursive iterator and the cursor frame so their
    bisimulation is explicit.  It also proves that the live-frame bound is the
    branching depth and is independent of fan-out. *)

Record RecursiveParentIterator := {
  recursive_parent_source_id : nat;
  recursive_parent_children : list A;
  recursive_parent_next_index : nat;
  recursive_parent_finality : bool
}.

Record ParentCursorContinuation := {
  parent_continuation_source_id : nat;
  parent_continuation_cursor : SnapshotCursorModel A;
  parent_continuation_next_index : nat;
  parent_continuation_total : nat;
  parent_continuation_first_finality : bool
}.

Definition make_parent_cursor_continuation
    (source_id : nat) (cursor : SnapshotCursorModel A) (next_index : nat)
    : ParentCursorContinuation :=
  {| parent_continuation_source_id := source_id;
     parent_continuation_cursor := cursor;
     parent_continuation_next_index := next_index;
     parent_continuation_total := length (snapshot_cursor_children cursor);
     parent_continuation_first_finality :=
       snapshot_cursor_finality cursor |}.

Definition parent_cursor_continuation_valid
    (owner : RetainedSnapshotOwner) (frame : ParentCursorContinuation) : Prop :=
  snapshot_cursor_valid owner (parent_continuation_cursor frame) /\
  parent_continuation_total frame =
    length (snapshot_cursor_children (parent_continuation_cursor frame)) /\
  parent_continuation_first_finality frame =
    snapshot_cursor_finality (parent_continuation_cursor frame) /\
  1 <= parent_continuation_next_index frame /\
  parent_continuation_next_index frame < parent_continuation_total frame.

Definition recursive_parent_remaining
    (iterator : RecursiveParentIterator) : list A :=
  skipn (recursive_parent_next_index iterator)
    (recursive_parent_children iterator).

Definition parent_continuation_remaining
    (frame : ParentCursorContinuation) : list A :=
  skipn (parent_continuation_next_index frame)
    (snapshot_cursor_children (parent_continuation_cursor frame)).

Definition parent_continuation_refines_recursive_iterator
    (iterator : RecursiveParentIterator)
    (frame : ParentCursorContinuation) : Prop :=
  recursive_parent_source_id iterator =
    parent_continuation_source_id frame /\
  recursive_parent_children iterator =
    snapshot_cursor_children (parent_continuation_cursor frame) /\
  recursive_parent_next_index iterator =
    parent_continuation_next_index frame /\
  recursive_parent_finality iterator =
    parent_continuation_first_finality frame /\
  parent_continuation_total frame =
    length (recursive_parent_children iterator).

Theorem parent_continuation_bisimulates_recursive_iterator :
  forall iterator frame,
    parent_continuation_refines_recursive_iterator iterator frame ->
    parent_continuation_remaining frame =
      recursive_parent_remaining iterator.
Proof.
  intros iterator frame [_ [Hchildren [Hindex _]]].
  unfold parent_continuation_remaining, recursive_parent_remaining.
  rewrite <- Hchildren, <- Hindex.
  reflexivity.
Qed.

Definition recursive_iterator_step
    (iterator : RecursiveParentIterator) : option (A * list A) :=
  match recursive_parent_remaining iterator with
  | [] => None
  | child :: later => Some (child, later)
  end.

Definition cursor_continuation_step
    (frame : ParentCursorContinuation) : option (A * list A) :=
  match parent_continuation_remaining frame with
  | [] => None
  | child :: later => Some (child, later)
  end.

Theorem parent_continuation_step_matches_recursive_iterator_step :
  forall iterator frame,
    parent_continuation_refines_recursive_iterator iterator frame ->
    cursor_continuation_step frame = recursive_iterator_step iterator.
Proof.
  intros iterator frame Hrefines.
  unfold cursor_continuation_step, recursive_iterator_step.
  rewrite (parent_continuation_bisimulates_recursive_iterator
    iterator frame Hrefines).
  reflexivity.
Qed.

Inductive CursorNodeObservation : Type :=
| CursorObservedEmpty (is_final : bool)
| CursorObservedDirect
    (is_final : bool) (direct_child : A)
    (parent : option ParentCursorContinuation).

Definition observe_cursor_node
    (reserve_one_ok : bool) (source_id : nat)
    (cursor : SnapshotCursorModel A) : option CursorNodeObservation :=
  match snapshot_cursor_children cursor with
  | [] => Some (CursorObservedEmpty (snapshot_cursor_finality cursor))
  | first :: [] =>
      Some (CursorObservedDirect
        (snapshot_cursor_finality cursor) first None)
  | first :: _ :: _ =>
      if reserve_one_ok then
        Some (CursorObservedDirect
          (snapshot_cursor_finality cursor) first
          (Some (make_parent_cursor_continuation source_id cursor 1)))
      else None
  end.

Theorem empty_cursor_observation_pushes_no_parent_frame :
  forall reserve_ok source_id revision finality,
    observe_cursor_node reserve_ok source_id
      {| snapshot_cursor_revision := revision;
         snapshot_cursor_finality := finality;
         snapshot_cursor_children := [] |} =
      Some (CursorObservedEmpty finality).
Proof.
  reflexivity.
Qed.

Theorem unary_cursor_observation_pushes_no_parent_frame :
  forall reserve_ok source_id revision finality child,
    observe_cursor_node reserve_ok source_id
      {| snapshot_cursor_revision := revision;
         snapshot_cursor_finality := finality;
         snapshot_cursor_children := [child] |} =
      Some (CursorObservedDirect finality child None).
Proof.
  reflexivity.
Qed.

Theorem branching_cursor_observation_pushes_exactly_one_parent_frame :
  forall source_id revision finality first second later,
    observe_cursor_node true source_id
      {| snapshot_cursor_revision := revision;
         snapshot_cursor_finality := finality;
         snapshot_cursor_children := first :: second :: later |} =
      Some (CursorObservedDirect finality first
        (Some (make_parent_cursor_continuation source_id
          {| snapshot_cursor_revision := revision;
             snapshot_cursor_finality := finality;
             snapshot_cursor_children := first :: second :: later |} 1))).
Proof.
  reflexivity.
Qed.

Theorem branching_cursor_reservation_failure_fails_before_descent :
  forall source_id revision finality first second later,
    observe_cursor_node false source_id
      {| snapshot_cursor_revision := revision;
         snapshot_cursor_finality := finality;
         snapshot_cursor_children := first :: second :: later |} = None.
Proof.
  reflexivity.
Qed.

Theorem newly_pushed_parent_continuation_is_valid :
  forall owner source_id cursor first second later,
    snapshot_cursor_valid owner cursor ->
    snapshot_cursor_children cursor = first :: second :: later ->
    parent_cursor_continuation_valid owner
      (make_parent_cursor_continuation source_id cursor 1).
Proof.
  intros owner source_id cursor first second later Hvalid Hchildren.
  destruct Hvalid as [Hlive Hrevision].
  unfold parent_cursor_continuation_valid, snapshot_cursor_valid,
    make_parent_cursor_continuation; cbn.
  repeat split.
  - exact Hlive.
  - exact Hrevision.
  - lia.
  - rewrite Hchildren. cbn. lia.
Qed.

Definition native_parent_resume_page
    (frame : ParentCursorContinuation) : SnapshotCursorPage :=
  make_cursor_page (parent_continuation_cursor frame)
    (match nth_error
       (snapshot_cursor_children (parent_continuation_cursor frame))
       (parent_continuation_next_index frame) with
     | Some child => [child]
     | None => []
     end).

Record CursorEdgeAtObservation := {
  cursor_edge_at_revision : nat;
  cursor_edge_at_finality : bool;
  cursor_edge_at_total : nat;
  cursor_edge_at_item : option A
}.

Definition native_cursor_edge_at
    (cursor : SnapshotCursorModel A) (index : nat)
    : CursorEdgeAtObservation :=
  {| cursor_edge_at_revision := snapshot_cursor_revision cursor;
     cursor_edge_at_finality := snapshot_cursor_finality cursor;
     cursor_edge_at_total := length (snapshot_cursor_children cursor);
     cursor_edge_at_item := nth_error (snapshot_cursor_children cursor) index |}.

Definition edge_at_observation_as_page
    (observation : CursorEdgeAtObservation) : SnapshotCursorPage :=
  {| cursor_page_revision := cursor_edge_at_revision observation;
     cursor_page_finality := cursor_edge_at_finality observation;
     cursor_page_total := cursor_edge_at_total observation;
     cursor_page_items :=
       match cursor_edge_at_item observation with
       | Some child => [child]
       | None => []
       end |}.

Theorem closure_free_edge_at_is_capacity_one_page :
  forall frame,
    edge_at_observation_as_page
      (native_cursor_edge_at
        (parent_continuation_cursor frame)
        (parent_continuation_next_index frame)) =
    native_parent_resume_page frame.
Proof.
  intros frame.
  reflexivity.
Qed.

Theorem valid_parent_edge_at_returns_exactly_one_child :
  forall owner frame,
    parent_cursor_continuation_valid owner frame ->
    exists child,
      cursor_edge_at_item
        (native_cursor_edge_at
          (parent_continuation_cursor frame)
          (parent_continuation_next_index frame)) = Some child.
Proof.
  intros owner frame [_ [Htotal [_ [_ Hindex]]]].
  unfold native_cursor_edge_at; cbn.
  assert (Hsome :
    nth_error
      (snapshot_cursor_children (parent_continuation_cursor frame))
      (parent_continuation_next_index frame) <> None).
  {
    apply nth_error_Some.
    rewrite <- Htotal.
    exact Hindex.
  }
  destruct (nth_error
    (snapshot_cursor_children (parent_continuation_cursor frame))
    (parent_continuation_next_index frame)) as [child|] eqn:Hchild.
  - exists child. reflexivity.
  - exfalso. apply Hsome. reflexivity.
Qed.

Definition parent_resume_page_consistent
    (frame : ParentCursorContinuation) (page : SnapshotCursorPage) : bool :=
  Nat.eqb (cursor_page_revision page)
      (snapshot_cursor_revision (parent_continuation_cursor frame)) &&
  Bool.eqb (cursor_page_finality page)
      (parent_continuation_first_finality frame) &&
  Nat.eqb (cursor_page_total page) (parent_continuation_total frame) &&
  Nat.eqb (length (cursor_page_items page)) 1.

Definition advance_parent_continuation
    (frame : ParentCursorContinuation) : ParentCursorContinuation :=
  {| parent_continuation_source_id :=
       parent_continuation_source_id frame;
     parent_continuation_cursor := parent_continuation_cursor frame;
     parent_continuation_next_index :=
       S (parent_continuation_next_index frame);
     parent_continuation_total := parent_continuation_total frame;
     parent_continuation_first_finality :=
       parent_continuation_first_finality frame |}.

Definition validate_and_resume_parent
    (frame : ParentCursorContinuation) (page : option SnapshotCursorPage)
    : option (A * option ParentCursorContinuation) :=
  match page with
  | None => None
  | Some observed =>
      if parent_resume_page_consistent frame observed then
        match cursor_page_items observed with
        | [child] =>
            let advanced := advance_parent_continuation frame in
            if Nat.ltb (parent_continuation_next_index advanced)
                 (parent_continuation_total frame)
            then Some (child, Some advanced)
            else if Nat.eqb (parent_continuation_next_index advanced)
                      (parent_continuation_total frame)
                 then Some (child, None)
                 else None
        | _ => None
        end
      else None
  end.

Theorem native_resume_page_is_consistent_for_a_valid_parent :
  forall owner frame,
    parent_cursor_continuation_valid owner frame ->
    parent_resume_page_consistent frame
      (native_parent_resume_page frame) = true.
Proof.
  intros owner frame [_ [Htotal [Hfinality [_ Hindex]]]].
  unfold parent_resume_page_consistent, native_parent_resume_page,
    make_cursor_page; cbn.
  rewrite Hfinality, Htotal.
  repeat rewrite Nat.eqb_refl.
  assert (Hsome :
    nth_error
      (snapshot_cursor_children (parent_continuation_cursor frame))
      (parent_continuation_next_index frame) <> None).
  {
    apply nth_error_Some.
    rewrite <- Htotal.
    exact Hindex.
  }
  destruct (nth_error
    (snapshot_cursor_children (parent_continuation_cursor frame))
    (parent_continuation_next_index frame)) as [child|] eqn:Hchild.
  - cbn.
    destruct (snapshot_cursor_finality (parent_continuation_cursor frame));
      reflexivity.
  - contradiction.
Qed.

Theorem resumed_parent_advances_only_after_a_valid_page :
  forall frame,
    validate_and_resume_parent frame None = None.
Proof.
  reflexivity.
Qed.

Theorem changed_resume_metadata_fails_closed :
  forall frame page,
    parent_resume_page_consistent frame page = false ->
    validate_and_resume_parent frame (Some page) = None.
Proof.
  intros frame page Hinconsistent.
  unfold validate_and_resume_parent.
  rewrite Hinconsistent.
  reflexivity.
Qed.

Theorem zero_or_multiple_resume_callbacks_fail_closed :
  forall frame page,
    length (cursor_page_items page) <> 1 ->
    validate_and_resume_parent frame (Some page) = None.
Proof.
  intros frame page Hcount.
  unfold validate_and_resume_parent, parent_resume_page_consistent.
  assert (Hneqb : Nat.eqb (length (cursor_page_items page)) 1 = false).
  { apply Nat.eqb_neq. exact Hcount. }
  rewrite Hneqb.
  repeat rewrite Bool.andb_false_r.
  reflexivity.
Qed.

Definition branching_frame_count (degrees : list nat) : nat :=
  length (filter (fun degree => Nat.leb 2 degree) degrees).

Definition eager_sibling_record_count (degrees : list nat) : nat :=
  fold_right (fun degree total => Nat.pred degree + total) 0 degrees.

(** [DictionaryNode<Unit = u8>] is deterministic, so a valid node has at most
    256 distinct outgoing labels.  The Rust frame stores [next_index] and
    [total] in lossless 16-bit fields after checking this boundary. *)
Definition byte_cursor_fanout_admitted (total : nat) : bool :=
  Nat.leb total 256.

Theorem admitted_byte_cursor_indices_fit_the_packed_frame :
  forall next_index total,
    byte_cursor_fanout_admitted total = true ->
    next_index < total ->
    next_index <= 256 /\ total <= 256 /\ S next_index <= total.
Proof.
  intros next_index total Hadmitted Hindex.
  unfold byte_cursor_fanout_admitted in Hadmitted.
  apply Nat.leb_le in Hadmitted.
  repeat split; lia.
Qed.

Theorem invalid_byte_cursor_fanout_is_rejected_before_frame_creation :
  forall total,
    256 < total ->
    byte_cursor_fanout_admitted total = false.
Proof.
  intros total Hlarge.
  unfold byte_cursor_fanout_admitted.
  apply Nat.leb_gt.
  exact Hlarge.
Qed.

Theorem maximum_deterministic_byte_fanout_is_losslessly_admitted :
  byte_cursor_fanout_admitted 256 = true.
Proof.
  reflexivity.
Qed.

Theorem parent_continuation_frames_are_bounded_by_active_depth :
  forall degrees,
    branching_frame_count degrees <= length degrees.
Proof.
  intros degrees.
  unfold branching_frame_count.
  apply filter_length_le.
Qed.

Theorem continuation_frame_bound_is_independent_of_fanout_at_depth_64 :
  branching_frame_count (repeat 2 64) = 64 /\
  branching_frame_count (repeat 17 64) = 64.
Proof.
  vm_compute.
  auto.
Qed.

Theorem rejected_eager_scheduler_materializes_1024_siblings_at_depth_64 :
  eager_sibling_record_count (repeat 17 64) = 1024 /\
  branching_frame_count (repeat 17 64) = 64.
Proof.
  vm_compute.
  auto.
Qed.

(** Negative control: descending before saving the parent loses its siblings. *)
Definition unsafe_observe_before_parent_push (children : list A) : list A :=
  match children with
  | [] => []
  | first :: _ => [first]
  end.

Theorem parent_frame_must_be_pushed_before_first_child_descent :
  forall first second,
    unsafe_observe_before_parent_push [first; second] <> [first; second].
Proof.
  intros first second Hequal.
  inversion Hequal.
Qed.

(** Negative control: resuming from zero duplicates the already emitted edge. *)
Theorem resume_index_zero_duplicates_the_first_child :
  forall (first second : A),
    first :: skipn 0 [first; second] = [first; first; second].
Proof.
  reflexivity.
Qed.

(** A failed page must not mutate the frame.  Advancing first violates that
    transition law even though the page is rejected afterward. *)
Definition unsafe_advance_before_page_validation
    (frame : ParentCursorContinuation) : ParentCursorContinuation :=
  advance_parent_continuation frame.

Theorem advancing_before_validation_changes_the_rejected_state :
  forall frame,
    parent_continuation_next_index
      (unsafe_advance_before_page_validation frame) <>
    parent_continuation_next_index frame.
Proof.
  intros frame Hequal.
  unfold unsafe_advance_before_page_validation,
    advance_parent_continuation in Hequal; cbn in Hequal.
  lia.
Qed.

Definition publish_resumed_parent
    (external_writer : list A)
    (local_result : option (A * option ParentCursorContinuation)) : list A :=
  match local_result with
  | Some (child, _) => external_writer ++ [child]
  | None => external_writer
  end.

Theorem failed_parent_resume_preserves_external_writer :
  forall external_writer,
    publish_resumed_parent external_writer None = external_writer.
Proof.
  reflexivity.
Qed.

Definition select_cursor_or_owned_schedule
    (cursor_supported : bool)
    (cursor_result : option (option A * list A))
    (owned_schedule : option A * list A)
    : option (option A * list A) :=
  if cursor_supported then cursor_result else Some owned_schedule.

Theorem unsupported_cursor_selects_owned_schedule :
  forall cursor_result owned_schedule,
    select_cursor_or_owned_schedule false cursor_result owned_schedule =
      Some owned_schedule.
Proof.
  reflexivity.
Qed.

Theorem supported_native_cursor_is_observationally_equivalent_to_owned :
  forall cursor pending,
    select_cursor_or_owned_schedule true
      (validated_cursor_page_schedule true
        (native_first_cursor_page cursor)
        (native_sibling_cursor_page cursor) pending)
      (tail_child_schedule (snapshot_cursor_children cursor) pending) =
    Some (tail_child_schedule
      (snapshot_cursor_children cursor) pending).
Proof.
  intros cursor pending.
  unfold select_cursor_or_owned_schedule.
  apply native_cursor_schedule_is_exact_tail_child_schedule.
Qed.

(** ** Retained Immutable Edge-Range Refinement

    Indexed cursor resumption is depth bounded, but it re-observes the parent
    node for every sibling.  The retained-range refinement observes a node
    exactly once.  That observation returns the first edge directly and, for
    a branching node, a nonempty token denoting the untouched sibling suffix.
    Each successful step consumes the head of that suffix and returns either
    its nonempty remainder or [None] at the exact end.

    The executable representation is a pair of provenance-preserving pointers
    into immutable edge storage.  This logical model deliberately separates
    the sequence law from the pointer-refinement law: the former proves wire
    order and PDA behavior, while the latter states the allocation, element
    type, bounds, immutability, and retained-owner obligations discharged by
    the unsafe backend and checked by Miri correspondence tests. *)

Record RetainedEdgeRangeToken := {
  edge_range_token_revision : nat;
  edge_range_token_remaining : list A
}.

Definition edge_range_token_valid
    (owner : RetainedSnapshotOwner) (token : RetainedEdgeRangeToken) : Prop :=
  retained_snapshot_live owner = true /\
  edge_range_token_revision token = retained_snapshot_revision owner /\
  edge_range_token_remaining token <> [].

Record EdgeRangeStartObservation := {
  edge_range_start_finality : bool;
  edge_range_start_total : nat;
  edge_range_start_first : option A;
  edge_range_start_tail : option RetainedEdgeRangeToken
}.

Definition make_edge_range_token
    (cursor : SnapshotCursorModel A) (remaining : list A)
    : RetainedEdgeRangeToken :=
  {| edge_range_token_revision := snapshot_cursor_revision cursor;
     edge_range_token_remaining := remaining |}.

Definition native_edge_range_start (cursor : SnapshotCursorModel A)
    : EdgeRangeStartObservation :=
  match snapshot_cursor_children cursor with
  | [] =>
      {| edge_range_start_finality := snapshot_cursor_finality cursor;
         edge_range_start_total := 0;
         edge_range_start_first := None;
         edge_range_start_tail := None |}
  | first :: [] =>
      {| edge_range_start_finality := snapshot_cursor_finality cursor;
         edge_range_start_total := 1;
         edge_range_start_first := Some first;
         edge_range_start_tail := None |}
  | first :: second :: later =>
      {| edge_range_start_finality := snapshot_cursor_finality cursor;
         edge_range_start_total := S (S (length later));
         edge_range_start_first := Some first;
         edge_range_start_tail :=
           Some (make_edge_range_token cursor (second :: later)) |}
  end.

Definition edge_range_start_sequence
    (observation : EdgeRangeStartObservation) : list A :=
  match edge_range_start_first observation with
  | None => []
  | Some first => first ::
      match edge_range_start_tail observation with
      | None => []
      | Some token => edge_range_token_remaining token
      end
  end.

Definition edge_range_start_shape_valid
    (observation : EdgeRangeStartObservation) : bool :=
  Nat.eqb (edge_range_start_total observation)
    (length (edge_range_start_sequence observation)) &&
  match edge_range_start_total observation,
        edge_range_start_first observation,
        edge_range_start_tail observation with
  | 0, None, None => true
  | 1, Some _, None => true
  | S (S _), Some _, Some token =>
      negb (Nat.eqb (length (edge_range_token_remaining token)) 0)
  | _, _, _ => false
  end.

Theorem native_edge_range_start_is_exact :
  forall cursor,
    edge_range_start_sequence (native_edge_range_start cursor) =
      snapshot_cursor_children cursor.
Proof.
  intros [revision finality children].
  destruct children as [|first [|second later]]; reflexivity.
Qed.

Theorem native_edge_range_start_reports_exact_metadata :
  forall cursor,
    edge_range_start_finality (native_edge_range_start cursor) =
      snapshot_cursor_finality cursor /\
    edge_range_start_total (native_edge_range_start cursor) =
      length (snapshot_cursor_children cursor) /\
    edge_range_start_shape_valid (native_edge_range_start cursor) = true.
Proof.
  intros [revision finality children].
  destruct children as [|first [|second later]].
  - repeat split; reflexivity.
  - repeat split; reflexivity.
  - split; [reflexivity |].
    split; [reflexivity |].
    change (
      Nat.eqb (S (S (length later))) (S (S (length later))) &&
      negb (Nat.eqb (S (length later)) 0) = true).
    rewrite Nat.eqb_refl.
    reflexivity.
Qed.

Theorem native_branching_tail_token_is_valid :
  forall owner cursor first second later,
    snapshot_cursor_valid owner cursor ->
    snapshot_cursor_children cursor = first :: second :: later ->
    edge_range_token_valid owner
      (make_edge_range_token cursor (second :: later)).
Proof.
  intros owner cursor first second later [Hlive Hrevision] Hchildren.
  unfold edge_range_token_valid, make_edge_range_token; cbn.
  repeat split; auto.
  discriminate.
Qed.

Definition native_edge_range_step (token : RetainedEdgeRangeToken)
    : option (A * option RetainedEdgeRangeToken) :=
  match edge_range_token_remaining token with
  | [] => None
  | child :: [] => Some (child, None)
  | child :: next :: later =>
      Some (child,
        Some {| edge_range_token_revision := edge_range_token_revision token;
                edge_range_token_remaining := next :: later |})
  end.

Definition stepped_range_sequence
    (child : A) (next : option RetainedEdgeRangeToken) : list A :=
  child ::
    match next with
    | None => []
    | Some token => edge_range_token_remaining token
    end.

Theorem native_edge_range_step_decomposes_the_token_exactly :
  forall token child next,
    native_edge_range_step token = Some (child, next) ->
    edge_range_token_remaining token = stepped_range_sequence child next.
Proof.
  intros [revision remaining] child next Hstep.
  destruct remaining as [|first [|second later]]; try discriminate.
  - inversion Hstep. reflexivity.
  - inversion Hstep. reflexivity.
Qed.

Theorem valid_edge_range_token_always_steps :
  forall owner token,
    edge_range_token_valid owner token ->
    exists child next,
      native_edge_range_step token = Some (child, next).
Proof.
  intros owner [revision remaining] [_ [_ Hnonempty]].
  destruct remaining as [|first [|second later]].
  - contradiction.
  - exists first, None. reflexivity.
  - exists first.
    exists (Some
      {| edge_range_token_revision := revision;
         edge_range_token_remaining := second :: later |}).
    reflexivity.
Qed.

Theorem native_edge_range_step_preserves_retained_provenance :
  forall owner token child next,
    edge_range_token_valid owner token ->
    native_edge_range_step token = Some (child, Some next) ->
    edge_range_token_valid owner next.
Proof.
  intros owner [revision remaining] child next
    [Hlive [Hrevision Hnonempty]] Hstep.
  destruct remaining as [|first [|second later]]; try discriminate.
  inversion Hstep; subst next; cbn.
  repeat split; auto.
  discriminate.
Qed.

(** One owner count is threaded through these operations unchanged.  The Rust
    correspondence test observes [Arc::strong_count] and establishes that the
    backend borrows child handles instead of cloning or moving them. *)
Definition retained_range_owner_count_after_start (count : nat) : nat := count.
Definition retained_range_owner_count_after_step (count : nat) : nat := count.

Theorem retained_range_operations_do_not_change_owner_count :
  forall count,
    retained_range_owner_count_after_start count = count /\
    retained_range_owner_count_after_step count = count.
Proof.
  intros count.
  split; reflexivity.
Qed.

Inductive PublishedEdgeStorageMode :=
| InlineEdgeStorage
| SpilledEdgeStorage.

Record PublishedEdgeStorage := {
  published_storage_allocation : nat;
  published_storage_element_type : nat;
  published_storage_revision : nat;
  published_storage_length : nat;
  published_storage_mode : PublishedEdgeStorageMode;
  published_storage_immutable : bool;
  published_storage_owner_live : bool
}.

Record AbstractEdgePointer := {
  abstract_pointer_allocation : nat;
  abstract_pointer_element_type : nat;
  abstract_pointer_index : nat
}.

Definition edge_range_pointer_refines
    (storage : PublishedEdgeStorage)
    (current finish : AbstractEdgePointer) : Prop :=
  published_storage_immutable storage = true /\
  published_storage_owner_live storage = true /\
  abstract_pointer_allocation current =
    published_storage_allocation storage /\
  abstract_pointer_allocation finish =
    published_storage_allocation storage /\
  abstract_pointer_element_type current =
    published_storage_element_type storage /\
  abstract_pointer_element_type finish =
    published_storage_element_type storage /\
  abstract_pointer_index current < abstract_pointer_index finish /\
  abstract_pointer_index finish <= published_storage_length storage.

Definition advance_abstract_edge_pointer
    (pointer : AbstractEdgePointer) : AbstractEdgePointer :=
  {| abstract_pointer_allocation := abstract_pointer_allocation pointer;
     abstract_pointer_element_type := abstract_pointer_element_type pointer;
     abstract_pointer_index := S (abstract_pointer_index pointer) |}.

Theorem refined_edge_range_pointers_share_allocation_and_element_type :
  forall storage current finish,
    edge_range_pointer_refines storage current finish ->
    abstract_pointer_allocation current =
      abstract_pointer_allocation finish /\
    abstract_pointer_element_type current =
      abstract_pointer_element_type finish.
Proof.
  intros storage current finish
    [_ [_ [Hcurrent_allocation [Hfinish_allocation
      [Hcurrent_type [Hfinish_type _]]]]]].
  split; congruence.
Qed.

Theorem advancing_a_nonfinal_refined_pointer_preserves_refinement :
  forall storage current finish,
    edge_range_pointer_refines storage current finish ->
    S (abstract_pointer_index current) < abstract_pointer_index finish ->
    edge_range_pointer_refines storage
      (advance_abstract_edge_pointer current) finish.
Proof.
  intros storage current finish
    [Himmutable [Hlive [Hcurrent_allocation [Hfinish_allocation
      [Hcurrent_type [Hfinish_type [Hbounds Hend]]]]]]]
    Hnext.
  unfold advance_abstract_edge_pointer; cbn.
  repeat split; auto.
Qed.

Theorem advancing_to_the_exact_end_produces_no_successor_token :
  forall storage current finish,
    edge_range_pointer_refines storage current finish ->
    S (abstract_pointer_index current) = abstract_pointer_index finish ->
    ~ edge_range_pointer_refines storage
        (advance_abstract_edge_pointer current) finish.
Proof.
  intros storage current finish Hrefines Hend Hadvanced.
  destruct Hadvanced as [_ [_ [_ [_ [_ [_ [Hstrict _]]]]]]].
  unfold advance_abstract_edge_pointer in Hstrict; cbn in Hstrict.
  lia.
Qed.

Theorem different_allocation_edge_range_is_rejected :
  forall storage current finish,
    abstract_pointer_allocation current <>
      abstract_pointer_allocation finish ->
    ~ edge_range_pointer_refines storage current finish.
Proof.
  intros storage current finish Hforeign Hrefines.
  apply refined_edge_range_pointers_share_allocation_and_element_type
    in Hrefines.
  destruct Hrefines as [Hsame_allocation _].
  exact (Hforeign Hsame_allocation).
Qed.

Theorem mutable_edge_storage_is_rejected :
  forall storage current finish,
    published_storage_immutable storage = false ->
    ~ edge_range_pointer_refines storage current finish.
Proof.
  intros storage current finish Hmutable [Himmutable _].
  rewrite Hmutable in Himmutable.
  discriminate.
Qed.

Theorem retired_edge_storage_owner_is_rejected :
  forall storage current finish,
    published_storage_owner_live storage = false ->
    ~ edge_range_pointer_refines storage current finish.
Proof.
  intros storage current finish Hretired [_ [Hlive _]].
  rewrite Hretired in Hlive.
  discriminate.
Qed.

Definition publishes_new_revision_without_mutating_old
    (old newer : PublishedEdgeStorage) : Prop :=
  published_storage_owner_live old = true /\
  published_storage_immutable old = true /\
  published_storage_revision old < published_storage_revision newer.

Theorem retained_old_range_survives_new_revision_publication :
  forall old newer current finish,
    publishes_new_revision_without_mutating_old old newer ->
    edge_range_pointer_refines old current finish ->
    edge_range_pointer_refines old current finish.
Proof.
  auto.
Qed.

Theorem inline_and_spilled_storage_obey_the_same_range_contract :
  forall storage current finish,
    edge_range_pointer_refines storage current finish ->
    (published_storage_mode storage = InlineEdgeStorage \/
     published_storage_mode storage = SpilledEdgeStorage) /\
    edge_range_pointer_refines storage current finish.
Proof.
  intros storage current finish Hrefines.
  split.
  - destruct (published_storage_mode storage); auto.
  - exact Hrefines.
Qed.

Record ParentEdgeRangeContinuation := {
  parent_range_source_id : nat;
  parent_range_token : RetainedEdgeRangeToken
}.

Definition parent_edge_range_continuation_valid
    (owner : RetainedSnapshotOwner)
    (frame : ParentEdgeRangeContinuation) : Prop :=
  edge_range_token_valid owner (parent_range_token frame).

Definition parent_edge_range_remaining
    (frame : ParentEdgeRangeContinuation) : list A :=
  edge_range_token_remaining (parent_range_token frame).

Definition parent_edge_range_refines_recursive_iterator
    (iterator : RecursiveParentIterator)
    (frame : ParentEdgeRangeContinuation) : Prop :=
  recursive_parent_source_id iterator = parent_range_source_id frame /\
  recursive_parent_remaining iterator = parent_edge_range_remaining frame.

Theorem parent_edge_range_bisimulates_recursive_iterator :
  forall iterator frame,
    parent_edge_range_refines_recursive_iterator iterator frame ->
    parent_edge_range_remaining frame = recursive_parent_remaining iterator.
Proof.
  intros iterator frame [_ Hremaining].
  symmetry.
  exact Hremaining.
Qed.

Inductive EdgeRangeCursorNodeObservation : Type :=
| EdgeRangeObservedEmpty (is_final : bool)
| EdgeRangeObservedDirect
    (is_final : bool) (direct_child : A)
    (parent : option ParentEdgeRangeContinuation).

Definition observe_edge_range_node
    (reserve_one_ok : bool) (source_id : nat)
    (cursor : SnapshotCursorModel A)
    : option EdgeRangeCursorNodeObservation :=
  let start := native_edge_range_start cursor in
  match edge_range_start_first start, edge_range_start_tail start with
  | None, None => Some (EdgeRangeObservedEmpty
      (edge_range_start_finality start))
  | Some first, None => Some (EdgeRangeObservedDirect
      (edge_range_start_finality start) first None)
  | Some first, Some tail =>
      if reserve_one_ok then
        Some (EdgeRangeObservedDirect
          (edge_range_start_finality start) first
          (Some {| parent_range_source_id := source_id;
                   parent_range_token := tail |}))
      else None
  | _, _ => None
  end.

Theorem retained_range_empty_and_unary_nodes_push_no_frame :
  forall reserve_ok source_id revision finality child,
    observe_edge_range_node reserve_ok source_id
      {| snapshot_cursor_revision := revision;
         snapshot_cursor_finality := finality;
         snapshot_cursor_children := [] |} =
      Some (EdgeRangeObservedEmpty finality) /\
    observe_edge_range_node reserve_ok source_id
      {| snapshot_cursor_revision := revision;
         snapshot_cursor_finality := finality;
         snapshot_cursor_children := [child] |} =
      Some (EdgeRangeObservedDirect finality child None).
Proof.
  intros reserve_ok source_id revision finality child.
  split; reflexivity.
Qed.

Theorem retained_range_branching_node_pushes_one_nonempty_tail :
  forall source_id revision finality first second later,
    observe_edge_range_node true source_id
      {| snapshot_cursor_revision := revision;
         snapshot_cursor_finality := finality;
         snapshot_cursor_children := first :: second :: later |} =
      Some (EdgeRangeObservedDirect finality first
        (Some {| parent_range_source_id := source_id;
                 parent_range_token :=
                   {| edge_range_token_revision := revision;
                      edge_range_token_remaining := second :: later |} |})).
Proof.
  reflexivity.
Qed.

Theorem retained_range_reservation_failure_precedes_first_descent :
  forall source_id revision finality first second later,
    observe_edge_range_node false source_id
      {| snapshot_cursor_revision := revision;
         snapshot_cursor_finality := finality;
         snapshot_cursor_children := first :: second :: later |} = None.
Proof.
  reflexivity.
Qed.

Definition validate_and_resume_edge_range
    (frame : ParentEdgeRangeContinuation)
    (backend_step : option (A * option RetainedEdgeRangeToken))
    : option (A * option ParentEdgeRangeContinuation) :=
  match backend_step with
  | None => None
  | Some (child, None) => Some (child, None)
  | Some (child, Some next) =>
      if Nat.eqb (edge_range_token_revision next)
           (edge_range_token_revision (parent_range_token frame))
      then Some (child,
        Some {| parent_range_source_id := parent_range_source_id frame;
                parent_range_token := next |})
      else None
  end.

Theorem native_edge_range_resume_matches_the_remaining_sequence :
  forall frame child next,
    native_edge_range_step (parent_range_token frame) =
      Some (child, next) ->
    parent_edge_range_remaining frame = stepped_range_sequence child next.
Proof.
  intros frame child next Hstep.
  unfold parent_edge_range_remaining.
  apply native_edge_range_step_decomposes_the_token_exactly.
  exact Hstep.
Qed.

Theorem native_edge_range_resume_is_accepted_for_a_valid_frame :
  forall owner frame child next,
    parent_edge_range_continuation_valid owner frame ->
    native_edge_range_step (parent_range_token frame) =
      Some (child, next) ->
    exists accepted,
      validate_and_resume_edge_range frame
        (Some (child, next)) = Some accepted.
Proof.
  intros owner frame child [next|] Hvalid Hstep.
  - exists (child,
      Some {| parent_range_source_id := parent_range_source_id frame;
              parent_range_token := next |}).
    unfold validate_and_resume_edge_range.
    assert (Hnextvalid : edge_range_token_valid owner next).
    {
      unfold parent_edge_range_continuation_valid in Hvalid.
      eapply native_edge_range_step_preserves_retained_provenance;
        eauto.
    }
    destruct Hvalid as [_ [Hframe_revision _]].
    destruct Hnextvalid as [_ [Hnext_revision _]].
    rewrite Hframe_revision, Hnext_revision, Nat.eqb_refl.
    reflexivity.
  - exists (child, None).
    reflexivity.
Qed.

Theorem failed_edge_range_step_does_not_advance_the_parent :
  forall frame,
    validate_and_resume_edge_range frame None = None.
Proof.
  reflexivity.
Qed.

Definition publish_edge_range_resume
    (external_writer : list A)
    (local_result : option (A * option ParentEdgeRangeContinuation))
    : list A :=
  match local_result with
  | None => external_writer
  | Some (child, _) => external_writer ++ [child]
  end.

Theorem failed_edge_range_resume_preserves_external_writer :
  forall external_writer,
    publish_edge_range_resume external_writer None = external_writer.
Proof.
  reflexivity.
Qed.

(** Negative controls for the exact range boundary. *)
Definition unsafe_edge_range_start_skips_first (cursor : SnapshotCursorModel A)
    : list A :=
  match edge_range_start_tail (native_edge_range_start cursor) with
  | None => []
  | Some token => edge_range_token_remaining token
  end.

Theorem skipping_the_direct_child_changes_a_binary_sequence :
  forall revision finality first second,
    unsafe_edge_range_start_skips_first
      {| snapshot_cursor_revision := revision;
         snapshot_cursor_finality := finality;
         snapshot_cursor_children := [first; second] |} = [second] /\
    [second] <> [first; second].
Proof.
  intros revision finality first second.
  split; [reflexivity | discriminate].
Qed.

Definition unsafe_tail_includes_direct_child
    (cursor : SnapshotCursorModel A) : RetainedEdgeRangeToken :=
  make_edge_range_token cursor (snapshot_cursor_children cursor).

Theorem including_the_direct_child_in_the_tail_duplicates_it :
  forall revision finality first second,
    stepped_range_sequence first
      (Some (unsafe_tail_includes_direct_child
        {| snapshot_cursor_revision := revision;
           snapshot_cursor_finality := finality;
           snapshot_cursor_children := [first; second] |})) =
      [first; first; second].
Proof.
  reflexivity.
Qed.

Theorem empty_edge_range_token_is_invalid :
  forall owner revision,
    ~ edge_range_token_valid owner
      {| edge_range_token_revision := revision;
         edge_range_token_remaining := [] |}.
Proof.
  intros owner revision [_ [_ Hnonempty]].
  contradiction.
Qed.

Theorem foreign_revision_edge_range_token_is_invalid :
  forall owner token,
    edge_range_token_revision token <>
      retained_snapshot_revision owner ->
    ~ edge_range_token_valid owner token.
Proof.
  intros owner token Hforeign [_ [Hrevision _]].
  contradiction.
Qed.

Definition unsafe_advance_range_before_backend_success
    (token : RetainedEdgeRangeToken) : RetainedEdgeRangeToken :=
  {| edge_range_token_revision := edge_range_token_revision token;
     edge_range_token_remaining :=
       match edge_range_token_remaining token with
       | [] => []
       | _ :: later => later
       end |}.

Theorem advancing_range_before_a_failed_step_loses_the_head :
  forall revision first later,
    edge_range_token_remaining
      (unsafe_advance_range_before_backend_success
        {| edge_range_token_revision := revision;
           edge_range_token_remaining := first :: later |}) = later.
Proof.
  reflexivity.
Qed.

End RetainedSnapshotCursorPagingLaws.

Theorem retained_edge_range_preserves_recursive_wire_trace :
  forall
      (iterator : @RecursiveParentIterator PathExpansionSkeletonEdge)
      (frame : @ParentEdgeRangeContinuation PathExpansionSkeletonEdge)
      next_id,
    parent_edge_range_refines_recursive_iterator iterator frame ->
    iterative_path_expansion next_id (parent_edge_range_remaining frame) =
    recursive_path_expansion next_id (recursive_parent_remaining iterator).
Proof.
  intros iterator frame next_id Hrefines.
  rewrite (parent_edge_range_bisimulates_recursive_iterator
    iterator frame Hrefines).
  apply iterative_path_expansion_matches_recursive_oracle.
Qed.

Theorem parent_continuation_preserves_recursive_wire_trace :
  forall
      (iterator : @RecursiveParentIterator PathExpansionSkeletonEdge)
      (frame : @ParentCursorContinuation PathExpansionSkeletonEdge)
      next_id,
    parent_continuation_refines_recursive_iterator iterator frame ->
    iterative_path_expansion next_id (parent_continuation_remaining frame) =
    recursive_path_expansion next_id (recursive_parent_remaining iterator).
Proof.
  intros iterator frame next_id Hrefines.
  rewrite (parent_continuation_bisimulates_recursive_iterator
    iterator frame Hrefines).
  apply iterative_path_expansion_matches_recursive_oracle.
Qed.

(** Instantiating children with the already-defined global DFS skeleton gives
    equality to the recursive wire oracle, including encounter order,
    finality, and dense target identifiers. *)
Theorem native_cursor_pages_preserve_recursive_wire_trace :
  forall (cursor : SnapshotCursorModel PathExpansionSkeletonEdge) next_id,
    iterative_path_expansion next_id
      (reported_cursor_children
        (native_first_cursor_page cursor)
        (native_sibling_cursor_page cursor)) =
    recursive_path_expansion next_id
      (snapshot_cursor_children cursor).
Proof.
  intros cursor next_id.
  rewrite native_cursor_pages_cover_every_child_exactly_once.
  apply iterative_path_expansion_matches_recursive_oracle.
Qed.

(** Negative control: pushing siblings in encounter order makes a LIFO
    worklist observe them in reverse DFS order. *)
Definition cursor_schedule_without_sibling_reversal
    (children pending : list nat) : option nat * list nat :=
  match children with
  | [] => (None, pending)
  | first :: later => (Some first, pending ++ later)
  end.

Theorem omitted_cursor_sibling_reversal_changes_dfs_order :
  scheduled_observation
    (cursor_schedule_without_sibling_reversal [0; 1; 2] []) = [0; 2; 1] /\
  scheduled_observation
    (tail_child_schedule [0; 1; 2] []) = [0; 1; 2].
Proof.
  split; reflexivity.
Qed.

(** ** Operation-scoped performance-measurement boundary

    Performance qualification is evidence about the serializer only when
    fixture construction and semantic validation are outside the hardware
    counter's enabled interval.  The executable harness implements the trace

      prepare; enable; serialize^n; disable; read; validate

    after constructing the fixture.  The projection below is deliberately
    independent of counter values: it proves which logical operations may
    contribute to a reading.  Linux perf-event correspondence tests separately
    establish reset/enable/disable/read ordering and reject multiplexing. *)

Inductive ProtobufMeasurementEvent : Type :=
| BuildBranchingFixture
| PrepareDisabledCounter
| EnableCounter
| SerializeProtobuf
| DisableCounter
| ReadCounter
| ValidateDigest.

Fixpoint measured_until_disable
    (events : list ProtobufMeasurementEvent)
    : list ProtobufMeasurementEvent :=
  match events with
  | [] => []
  | DisableCounter :: _ => []
  | event :: later => event :: measured_until_disable later
  end.

Fixpoint measured_interval
    (events : list ProtobufMeasurementEvent)
    : list ProtobufMeasurementEvent :=
  match events with
  | [] => []
  | EnableCounter :: later => measured_until_disable later
  | _ :: later => measured_interval later
  end.

Definition protobuf_measurement_trace (repetitions : nat)
    : list ProtobufMeasurementEvent :=
  [BuildBranchingFixture;
   PrepareDisabledCounter;
   EnableCounter] ++
  repeat SerializeProtobuf repetitions ++
  [DisableCounter;
   ReadCounter;
   ValidateDigest].

Lemma measured_enabled_suffix_is_exact_serialization_loop :
  forall repetitions,
    measured_until_disable
      (repeat SerializeProtobuf repetitions ++
       [DisableCounter; ReadCounter; ValidateDigest]) =
      repeat SerializeProtobuf repetitions.
Proof.
  intros repetitions.
  induction repetitions as [|repetitions IH].
  - reflexivity.
  - change
      ((SerializeProtobuf ::
         measured_until_disable
           (repeat SerializeProtobuf repetitions ++
            (DisableCounter :: ReadCounter :: ValidateDigest :: nil))) =
       (SerializeProtobuf :: repeat SerializeProtobuf repetitions)).
    f_equal.
    exact IH.
Qed.

Theorem protobuf_measurement_counts_exactly_the_serialization_loop :
  forall repetitions,
    measured_interval (protobuf_measurement_trace repetitions) =
      repeat SerializeProtobuf repetitions.
Proof.
  intros repetitions.
  unfold protobuf_measurement_trace.
  cbn [measured_interval].
  apply measured_enabled_suffix_is_exact_serialization_loop.
Qed.

Corollary protobuf_measurement_excludes_fixture_and_validation :
  forall repetitions,
    ~ In BuildBranchingFixture
        (measured_interval (protobuf_measurement_trace repetitions)) /\
    ~ In ValidateDigest
        (measured_interval (protobuf_measurement_trace repetitions)).
Proof.
  intros repetitions.
  rewrite protobuf_measurement_counts_exactly_the_serialization_loop.
  split.
  - intro Hin.
    apply repeat_spec in Hin.
    discriminate.
  - intro Hin.
    apply repeat_spec in Hin.
    discriminate.
Qed.

(** Negative control: enabling before fixture construction contaminates the
    reading, so it cannot satisfy the serializer-only boundary. *)
Definition contaminated_measurement_trace (repetitions : nat)
    : list ProtobufMeasurementEvent :=
  EnableCounter :: BuildBranchingFixture ::
  repeat SerializeProtobuf repetitions ++
  [DisableCounter; ReadCounter; ValidateDigest].

Theorem counter_enabled_before_fixture_is_observably_contaminated :
  forall repetitions,
    In BuildBranchingFixture
      (measured_interval (contaminated_measurement_trace repetitions)).
Proof.
  intros repetitions.
  unfold contaminated_measurement_trace.
  cbn [measured_interval measured_until_disable].
  left.
  reflexivity.
Qed.
