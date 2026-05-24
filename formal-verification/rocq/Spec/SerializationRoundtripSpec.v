(** * SerializationRoundtripSpec: Public Serializer Laws

    This module states the backend-neutral correctness obligations for
    libdictenstein's public serialization API.

    The model separates three contracts that are distinct in the Rust surface:

    - legacy term-only serialization preserves set membership;
    - value-aware serialization preserves mapped lookups;
    - malformed payload handling is fail-closed through a validation predicate.
*)

Require Import Stdlib.Lists.List.
Require Import Stdlib.Bool.Bool.
Require Import Stdlib.Logic.FunctionalExtensionality.
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
