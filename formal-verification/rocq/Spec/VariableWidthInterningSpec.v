(** * Certified variable-width atom interning and fixed-width ID views

    This functional model is the second formal milestone of the
    variable-width dictionary campaign.  It consumes the certified profile
    and codec laws from [VariableWidthCodecSpec].  In particular:

    - an atom is indexed by one certified persistent profile and carries a
      proof that its complete byte string is one canonical codeword;
    - [SymbolId I] and [TermId T] are distinct nominal types parameterized by
      open, positive-width fixed-width carrier profiles;
    - a single interning state ties the live bijection, historical ownership,
      packed bytes, reverse spans, sparse allocator frontier, dependent
      sequences, and optional term-ID dictionary together;
    - IDs which were published or burned as orphans are never rebound;
    - sequence views are bound to an immutable backing identity and exact
      vocabulary fiber, and index fixed-width IDs without decoding atoms;
    - query-local IDs occupy a distinct, non-serializable namespace;
    - model-to-Rust correspondence records exact paths, symbols, semantic
      relationships, and obligations.  Current conflicts are explicit.

    Stateful object publication, crash recovery, immutable reader retention,
    and repeated generations are modeled in
    [VariableWidthVocabularyInterning.tla] and
    [VariableWidthVocabularyPublication.tla].

    Rocq lists model immutable mathematical observations.  Allocation-free
    borrowing, native slice layout, lifetimes, and hot-loop costs remain
    explicit Rust refinement obligations; this file does not misdescribe a
    Rocq list as a Rust borrow.

    Stable theorem names beginning with [VWENC_] are machine-readable
    invariant identifiers consumed by the conformance ledger.
 *)

From Coq Require Import Lists.List.
From Coq Require Import Arith.Arith.
From Coq Require Import Bool.Bool.
From Coq Require Import micromega.Lia.
From Coq Require Import Logic.ProofIrrelevance.
From Coq Require Import Strings.String.
From Coq Require Import Sorting.Permutation.
Require Import ARTrie.Spec.VariableWidthCodecSpec.
Require Import ARTrie.Model.ListCompat.
Import ListNotations.
Import VariableWidthCodecSpec.

Module VariableWidthInterning.

(** ** Certified atom profiles and canonical atoms *)

Definition descriptor_canonical_codeword
    (descriptor : PersistentProfileDescriptor)
    (bytes : list PhysicalByte) : Prop :=
  match persistent_logical_profile descriptor with
  | PersistedByte =>
      List.length bytes = 1 /\ Forall valid_byte bytes
  | PersistedUnicodeScalar =>
      List.length bytes = 4 /\
      Forall valid_byte bytes /\
      unicode_scalar (decode_fixed_little_endian bytes)
  | PersistedU64 =>
      List.length bytes = 8 /\ Forall valid_byte bytes
  | PersistedF64Bits =>
      List.length bytes = 8 /\ Forall valid_byte bytes
  | PersistedCanonicalUleb => canonical_uleb_codeword bytes
  | PersistedCanonicalUtf8 =>
      exists codepoint, canonical_utf8_codeword codepoint bytes
  end.

Lemma descriptor_canonical_codeword_nonempty :
  forall descriptor bytes,
    descriptor_canonical_codeword descriptor bytes -> bytes <> [].
Proof.
  intros [profile codec layout abi] bytes Hcodeword.
  destruct profile; simpl in Hcodeword.
  - destruct Hcodeword as [Hlength _].
    intros Hequal. subst bytes. simpl in Hlength. discriminate.
  - destruct Hcodeword as [Hlength _].
    intros Hequal. subst bytes. simpl in Hlength. discriminate.
  - destruct Hcodeword as [Hlength _].
    intros Hequal. subst bytes. simpl in Hlength. discriminate.
  - destruct Hcodeword as [Hlength _].
    intros Hequal. subst bytes. simpl in Hlength. discriminate.
  - now apply VWENC_03_ULEB_CODEWORDS_NONEMPTY.
  - destruct Hcodeword as [codepoint [Hscalar Hbytes]].
    subst bytes.
    exact (proj1 (VWENC_12_UTF8_CODEWORDS_NONEMPTY_AND_AT_MOST_FOUR_BYTES
      codepoint Hscalar)).
Qed.

Record CertifiedAtomProfile : Type := mkCertifiedAtomProfile {
  atom_profile_descriptor : PersistentProfileDescriptor;
  atom_profile_certificate :
    certified_persistent_profile atom_profile_descriptor
}.

Definition atom_codeword
    (profile : CertifiedAtomProfile) : list PhysicalByte -> Prop :=
  descriptor_canonical_codeword (atom_profile_descriptor profile).

Lemma atom_codeword_nonempty :
  forall profile bytes, atom_codeword profile bytes -> bytes <> [].
Proof.
  intros profile bytes Hcodeword.
  unfold atom_codeword in Hcodeword.
  now apply descriptor_canonical_codeword_nonempty
    with (descriptor := atom_profile_descriptor profile).
Qed.

Definition canonical_uleb_descriptor : PersistentProfileDescriptor :=
  {| persistent_logical_profile := PersistedCanonicalUleb;
     persistent_codec_identity := ProspectiveCanonicalUlebCodecV1;
     persistent_layout_identity := ProspectiveLogicalUnitLayoutV1;
     persistent_abi_version := 1 |}.

Lemma canonical_uleb_descriptor_certified :
  certified_persistent_profile canonical_uleb_descriptor.
Proof.
  exact VWENC_99_CERTIFICATION_ACCEPTS_VERSIONED_CANONICAL_ULEB_PROFILE.
Qed.

Definition canonical_uleb_profile : CertifiedAtomProfile :=
  {| atom_profile_descriptor := canonical_uleb_descriptor;
     atom_profile_certificate := canonical_uleb_descriptor_certified |}.

Record CanonicalAtom (P : CertifiedAtomProfile) : Type := mkCanonicalAtom {
  canonical_atom_bytes : list PhysicalByte;
  canonical_atom_valid : atom_codeword P canonical_atom_bytes
}.

Definition canonical_atom_identity
    {P : CertifiedAtomProfile} (atom : CanonicalAtom P) :=
  (certified_profile_identity (atom_profile_descriptor P),
   canonical_atom_bytes P atom).

Definition canonical_atom_eq_dec
    (P : CertifiedAtomProfile)
    (left right : CanonicalAtom P) : {left = right} + {left <> right}.
Proof.
  destruct left as [left_bytes left_valid].
  destruct right as [right_bytes right_valid].
  destruct (list_eq_dec Nat.eq_dec left_bytes right_bytes)
    as [Hbytes | Hbytes].
  - subst right_bytes. left.
    assert (left_valid = right_valid) by apply proof_irrelevance.
    now subst right_valid.
  - right. intros Hequal. inversion Hequal. contradiction.
Defined.

Theorem VWENC_101_CANONICAL_ATOM_IDENTITY_IS_CERTIFIED_PROFILE_AND_BYTES :
  forall (P : CertifiedAtomProfile) (left right : CanonicalAtom P),
    canonical_atom_identity left = canonical_atom_identity right ->
    left = right.
Proof.
  intros P [left_bytes left_valid] [right_bytes right_valid] Hequal.
  unfold canonical_atom_identity in Hequal. simpl in Hequal.
  inversion Hequal. subst right_bytes.
  assert (left_valid = right_valid) by apply proof_irrelevance.
  now subst right_valid.
Qed.

Definition canonical_uleb_atom
    (bytes : list PhysicalByte)
    (Hcanonical : canonical_uleb_codeword bytes)
    : CanonicalAtom canonical_uleb_profile.
Proof.
  refine (@mkCanonicalAtom canonical_uleb_profile bytes _).
  change (canonical_uleb_codeword bytes).
  exact Hcanonical.
Defined.

Theorem VWENC_102_ULEB_INTERNALIZATION_REQUIRES_CANONICAL_ARBITRARY_BYTES :
  forall bytes (Hcanonical : canonical_uleb_codeword bytes),
    canonical_atom_bytes
      canonical_uleb_profile
      (canonical_uleb_atom bytes Hcanonical) = bytes /\
    bytes <> [] /\
    certified_profile_identity
      (atom_profile_descriptor canonical_uleb_profile) =
    certified_profile_identity canonical_uleb_descriptor.
Proof.
  intros bytes Hcanonical.
  split; [reflexivity |].
  split.
  - now apply VWENC_03_ULEB_CODEWORDS_NONEMPTY.
  - reflexivity.
Qed.

Lemma one_byte_uleb_is_canonical :
  forall byte, byte < 128 -> canonical_uleb_codeword [byte].
Proof.
  intros byte Hbyte.
  split.
  - constructor. exact Hbyte.
  - unfold canonical_uleb_digits, decode_uleb_payloads.
    simpl.
    repeat split.
    + discriminate.
    + constructor.
      * unfold valid_uleb_digit, uleb_payload.
        apply Nat.mod_upper_bound. lia.
      * constructor.
    + simpl. lia.
Qed.

(** ** Open fixed-width ID carriers and nominal IDs *)

Record FixedWidthCarrierProfile : Type := mkFixedWidthCarrierProfile {
  carrier_format_identity : nat;
  carrier_width_bytes : nat;
  carrier_width_positive : 0 < carrier_width_bytes
}.

Definition carrier_capacity (I : FixedWidthCarrierProfile) : nat :=
  256 ^ carrier_width_bytes I.

Lemma carrier_capacity_positive :
  forall I, 0 < carrier_capacity I.
Proof.
  intros I.
  unfold carrier_capacity.
  assert (256 ^ carrier_width_bytes I <> 0).
  { apply Nat.pow_nonzero. lia. }
  lia.
Qed.

Record SymbolId (I : FixedWidthCarrierProfile) : Type := mkSymbolId {
  symbol_id_value : nat;
  symbol_id_in_range : symbol_id_value < carrier_capacity I
}.

Record TermId (T : FixedWidthCarrierProfile) : Type := mkTermId {
  term_id_value : nat;
  term_id_in_range : term_id_value < carrier_capacity T
}.

Definition symbol_id_eq_dec
    (I : FixedWidthCarrierProfile)
    (left right : SymbolId I) : {left = right} + {left <> right}.
Proof.
  destruct left as [left_value left_range].
  destruct right as [right_value right_range].
  destruct (Nat.eq_dec left_value right_value) as [Hequal | Hdifferent].
  - subst right_value. left.
    assert (left_range = right_range) by apply proof_irrelevance.
    now subst right_range.
  - right. intros Hequal. inversion Hequal. contradiction.
Defined.

Definition term_id_eq_dec
    (T : FixedWidthCarrierProfile)
    (left right : TermId T) : {left = right} + {left <> right}.
Proof.
  destruct left as [left_value left_range].
  destruct right as [right_value right_range].
  destruct (Nat.eq_dec left_value right_value) as [Hequal | Hdifferent].
  - subst right_value. left.
    assert (left_range = right_range) by apply proof_irrelevance.
    now subst right_range.
  - right. intros Hequal. inversion Hequal. contradiction.
Defined.

Definition symbol_id_of_nat
    (I : FixedWidthCarrierProfile) (value : nat) : option (SymbolId I) :=
  match lt_dec value (carrier_capacity I) with
  | left Hfits =>
      Some {| symbol_id_value := value; symbol_id_in_range := Hfits |}
  | right _ => None
  end.

Definition term_id_of_nat
    (T : FixedWidthCarrierProfile) (value : nat) : option (TermId T) :=
  match lt_dec value (carrier_capacity T) with
  | left Hfits =>
      Some {| term_id_value := value; term_id_in_range := Hfits |}
  | right _ => None
  end.

Definition encode_symbol_id
    (I : FixedWidthCarrierProfile) (id : SymbolId I)
    : list PhysicalByte :=
  encode_fixed_little_endian
    (carrier_width_bytes I) (symbol_id_value I id).

Definition encode_term_id
    (T : FixedWidthCarrierProfile) (id : TermId T)
    : list PhysicalByte :=
  encode_fixed_little_endian
    (carrier_width_bytes T) (term_id_value T id).

Definition decode_symbol_id
    (I : FixedWidthCarrierProfile) (bytes : list PhysicalByte)
    : option (SymbolId I) :=
  if Nat.eq_dec (List.length bytes) (carrier_width_bytes I) then
    if all_valid_bytesb bytes then
      symbol_id_of_nat I (decode_fixed_little_endian bytes)
    else None
  else None.

Definition decode_term_id
    (T : FixedWidthCarrierProfile) (bytes : list PhysicalByte)
    : option (TermId T) :=
  if Nat.eq_dec (List.length bytes) (carrier_width_bytes T) then
    if all_valid_bytesb bytes then
      term_id_of_nat T (decode_fixed_little_endian bytes)
    else None
  else None.

Lemma symbol_id_fixed_width_encoding_roundtrips :
  forall (I : FixedWidthCarrierProfile) (id : SymbolId I),
    List.length (encode_symbol_id I id) = carrier_width_bytes I /\
    decode_symbol_id I (encode_symbol_id I id) = Some id.
Proof.
  intros I [value Hrange].
  split.
  - apply fixed_little_endian_length.
  - unfold decode_symbol_id, encode_symbol_id. simpl.
    rewrite fixed_little_endian_length.
    destruct (Nat.eq_dec (carrier_width_bytes I) (carrier_width_bytes I))
      as [_ | Himpossible].
    2: contradiction.
    assert (Hvalid :
      all_valid_bytesb
        (encode_fixed_little_endian (carrier_width_bytes I) value) = true).
    { apply (proj2 (all_valid_bytesb_reflects_validity _)).
      apply fixed_little_endian_bytes_are_valid. }
    rewrite Hvalid.
    unfold symbol_id_of_nat.
    rewrite fixed_little_endian_roundtrip by exact Hrange.
    destruct (lt_dec value (carrier_capacity I)) as [Hfits | Hoverflow].
    + f_equal. f_equal. apply proof_irrelevance.
    + contradiction.
Qed.

Lemma term_id_fixed_width_encoding_roundtrips :
  forall (T : FixedWidthCarrierProfile) (id : TermId T),
    List.length (encode_term_id T id) = carrier_width_bytes T /\
    decode_term_id T (encode_term_id T id) = Some id.
Proof.
  intros T [value Hrange].
  split.
  - apply fixed_little_endian_length.
  - unfold decode_term_id, encode_term_id. simpl.
    rewrite fixed_little_endian_length.
    destruct (Nat.eq_dec (carrier_width_bytes T) (carrier_width_bytes T))
      as [_ | Himpossible].
    2: contradiction.
    assert (Hvalid :
      all_valid_bytesb
        (encode_fixed_little_endian (carrier_width_bytes T) value) = true).
    { apply (proj2 (all_valid_bytesb_reflects_validity _)).
      apply fixed_little_endian_bytes_are_valid. }
    rewrite Hvalid.
    unfold term_id_of_nat.
    rewrite fixed_little_endian_roundtrip by exact Hrange.
    destruct (lt_dec value (carrier_capacity T)) as [Hfits | Hoverflow].
    + f_equal. f_equal. apply proof_irrelevance.
    + contradiction.
Qed.

Theorem VWENC_109_SYMBOL_AND_TERM_ID_FIXED_WIDTH_ENCODINGS_ROUNDTRIP :
  (forall (I : FixedWidthCarrierProfile) (id : SymbolId I),
    List.length (encode_symbol_id I id) = carrier_width_bytes I /\
    decode_symbol_id I (encode_symbol_id I id) = Some id) /\
  (forall (T : FixedWidthCarrierProfile) (id : TermId T),
    List.length (encode_term_id T id) = carrier_width_bytes T /\
    decode_term_id T (encode_term_id T id) = Some id).
Proof.
  split.
  - exact symbol_id_fixed_width_encoding_roundtrips.
  - exact term_id_fixed_width_encoding_roundtrips.
Qed.

Lemma symbol_id_construction_rejects_overflow :
  forall (I : FixedWidthCarrierProfile) value,
    carrier_capacity I <= value ->
    symbol_id_of_nat I value = None.
Proof.
  intros I value Hoverflow.
  unfold symbol_id_of_nat.
  destruct (lt_dec value (carrier_capacity I)); [lia | reflexivity].
Qed.

Lemma term_id_construction_rejects_overflow :
  forall (T : FixedWidthCarrierProfile) value,
    carrier_capacity T <= value ->
    term_id_of_nat T value = None.
Proof.
  intros T value Hoverflow.
  unfold term_id_of_nat.
  destruct (lt_dec value (carrier_capacity T)); [lia | reflexivity].
Qed.

Theorem VWENC_110_SYMBOL_AND_TERM_ID_CONSTRUCTION_REJECTS_OVERFLOW :
  (forall (I : FixedWidthCarrierProfile) value,
    carrier_capacity I <= value ->
    symbol_id_of_nat I value = None) /\
  (forall (T : FixedWidthCarrierProfile) value,
    carrier_capacity T <= value ->
    term_id_of_nat T value = None).
Proof.
  split.
  - exact symbol_id_construction_rejects_overflow.
  - exact term_id_construction_rejects_overflow.
Qed.

Theorem VWENC_111_ID_CARRIER_INTERFACE_REMAINS_OPEN_TO_ANY_POSITIVE_WIDTH :
  forall (I : FixedWidthCarrierProfile),
    0 < carrier_width_bytes I /\
    0 < carrier_capacity I /\
    List.length
      (encode_fixed_little_endian (carrier_width_bytes I) 0) =
      carrier_width_bytes I.
Proof.
  intros I. repeat split.
  - apply carrier_width_positive.
  - apply carrier_capacity_positive.
  - apply fixed_little_endian_length.
Qed.

Definition carrier_from_positive_width
    (format_identity width : nat) (Hwidth : 0 < width)
    : FixedWidthCarrierProfile :=
  {| carrier_format_identity := format_identity;
     carrier_width_bytes := width;
     carrier_width_positive := Hwidth |}.

Theorem VWENC_160_EVERY_POSITIVE_WIDTH_HAS_AN_EXACT_CARRIER_INSTANCE :
  forall format_identity width,
    0 < width ->
    exists carrier : FixedWidthCarrierProfile,
      carrier_format_identity carrier = format_identity /\
      carrier_width_bytes carrier = width.
Proof.
  intros format_identity width Hwidth.
  exists (carrier_from_positive_width format_identity width Hwidth).
  now split.
Qed.

(** ** Vocabulary fibers and exact atom/ID bijections *)

Record VocabularyFiber
    (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile) : Type :=
  mkVocabularyFiber {
    vocabulary_identity : nat;
    vocabulary_generation : nat
  }.

Definition vocabulary_fiber_identity
    {P : CertifiedAtomProfile} {I : FixedWidthCarrierProfile}
    (fiber : VocabularyFiber P I) :=
  (certified_profile_identity (atom_profile_descriptor P),
   (carrier_format_identity I,
    (carrier_width_bytes I,
     (vocabulary_identity P I fiber,
      vocabulary_generation P I fiber)))).

Definition vocabulary_fiber_eq_dec
    (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (left right : VocabularyFiber P I)
    : {left = right} + {left <> right}.
Proof.
  destruct left as [left_identity left_generation].
  destruct right as [right_identity right_generation].
  destruct (Nat.eq_dec left_identity right_identity)
    as [Hidentity | Hidentity].
  - subst right_identity.
    destruct (Nat.eq_dec left_generation right_generation)
      as [Hgeneration | Hgeneration].
    + subst right_generation. left. reflexivity.
    + right. intros Hequal. inversion Hequal. contradiction.
  - right. intros Hequal. inversion Hequal. contradiction.
Defined.

Record FiberBoundSymbolId
    (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile) : Type :=
  mkFiberBoundSymbolId {
    bound_symbol_fiber : VocabularyFiber P I;
    bound_symbol_value : SymbolId I
  }.

Definition interpret_symbol_id
    {P : CertifiedAtomProfile} {I : FixedWidthCarrierProfile}
    (expected : VocabularyFiber P I)
    (bound : FiberBoundSymbolId P I) : option (SymbolId I) :=
  if vocabulary_fiber_eq_dec P I expected (bound_symbol_fiber P I bound)
  then Some (bound_symbol_value P I bound)
  else None.

Theorem VWENC_112_CROSS_FIBER_ID_INTERPRETATION_IS_REJECTED :
  forall (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (expected actual : VocabularyFiber P I) (id : SymbolId I),
    expected <> actual ->
    interpret_symbol_id expected
      (mkFiberBoundSymbolId P I actual id) = None.
Proof.
  intros P I expected actual id Hdifferent.
  unfold interpret_symbol_id. simpl.
  destruct (vocabulary_fiber_eq_dec P I expected actual)
    as [Hequal | _].
  - contradiction.
  - reflexivity.
Qed.

Theorem VWENC_113_SAME_FIBER_ID_INTERPRETATION_IS_EXACT :
  forall (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (fiber : VocabularyFiber P I) (id : SymbolId I),
    interpret_symbol_id fiber
      (mkFiberBoundSymbolId P I fiber id) = Some id.
Proof.
  intros P I fiber id.
  unfold interpret_symbol_id. simpl.
  destruct (vocabulary_fiber_eq_dec P I fiber fiber)
    as [_ | Himpossible].
  - reflexivity.
  - contradiction.
Qed.

Fixpoint assoc_lookup {Key Value : Type}
    (key_eq_dec : forall left right : Key, {left = right} + {left <> right})
    (entries : list (Key * Value))
    (query : Key) : option Value :=
  match entries with
  | [] => None
  | (key, value) :: rest =>
      if key_eq_dec query key then Some value
      else assoc_lookup key_eq_dec rest query
  end.

Lemma assoc_lookup_sound :
  forall (Key Value : Type)
    (key_eq_dec : forall left right : Key, {left = right} + {left <> right})
    (entries : list (Key * Value)) query value,
    assoc_lookup key_eq_dec entries query = Some value ->
    In (query, value) entries.
Proof.
  intros Key Value key_eq_dec entries.
  induction entries as [| [key current] rest IH];
    intros query value Hlookup.
  - discriminate.
  - simpl in Hlookup.
    destruct (key_eq_dec query key) as [Hequal | Hdifferent].
    + inversion Hlookup. subst. now left.
    + right. now apply IH.
Qed.

Lemma assoc_lookup_complete_unique :
  forall (Key Value : Type)
    (key_eq_dec : forall left right : Key, {left = right} + {left <> right})
    (entries : list (Key * Value)) query value,
    NoDup (map fst entries) ->
    In (query, value) entries ->
    assoc_lookup key_eq_dec entries query = Some value.
Proof.
  intros Key Value key_eq_dec entries.
  induction entries as [| [key current] rest IH];
    intros query value Hnodup Hin.
  - contradiction.
  - inversion Hnodup as [| head keys Hhead Hrest].
    simpl in Hin.
    destruct Hin as [Hequal | Hin].
    + inversion Hequal. subst query value.
      simpl. destruct (key_eq_dec key key); [reflexivity | contradiction].
    + simpl.
      destruct (key_eq_dec query key) as [Hequal | Hdifferent].
      * subst query. exfalso. apply Hhead.
        apply in_map with (f := fst) in Hin. exact Hin.
      * now apply IH.
Qed.

Lemma assoc_lookup_none_key_absent :
  forall (Key Value : Type)
    (key_eq_dec : forall left right : Key, {left = right} + {left <> right})
    (entries : list (Key * Value)) query,
    assoc_lookup key_eq_dec entries query = None ->
    ~ In query (map fst entries).
Proof.
  intros Key Value key_eq_dec entries.
  induction entries as [| [key value] rest IH]; intros query Hlookup.
  - simpl. tauto.
  - simpl in Hlookup |- *.
    destruct (key_eq_dec query key) as [Hequal | Hdifferent].
    + discriminate.
    + intros [Hequal | Hin].
      * apply Hdifferent. symmetry. exact Hequal.
      * now apply (IH query Hlookup).
Qed.

Definition VocabularyEntry
    (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile) :=
  (CanonicalAtom P * SymbolId I)%type.

Definition reverse_vocabulary_entries
    {P : CertifiedAtomProfile} {I : FixedWidthCarrierProfile}
    (entries : list (VocabularyEntry P I))
    : list (SymbolId I * CanonicalAtom P) :=
  map (fun entry => (snd entry, fst entry)) entries.

Lemma reverse_vocabulary_membership :
  forall (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (entries : list (VocabularyEntry P I)) atom id,
    In (id, atom) (reverse_vocabulary_entries entries) <->
    In (atom, id) entries.
Proof.
  intros P I entries atom id.
  unfold reverse_vocabulary_entries. rewrite in_map_iff.
  split.
  - intros [[entry_atom entry_id] [Hequal Hin]].
    simpl in Hequal. inversion Hequal. subst. exact Hin.
  - intros Hin. exists (atom, id). split; [reflexivity | exact Hin].
Qed.

Lemma reverse_vocabulary_keys_are_ids :
  forall (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (entries : list (VocabularyEntry P I)),
    map fst (reverse_vocabulary_entries entries) = map snd entries.
Proof.
  intros P I entries. unfold reverse_vocabulary_entries.
  rewrite map_map. apply map_ext. intros [atom id]. reflexivity.
Qed.

Definition lookup_atom
    {P : CertifiedAtomProfile} {I : FixedWidthCarrierProfile}
    (entries : list (VocabularyEntry P I))
    (atom : CanonicalAtom P) : option (SymbolId I) :=
  assoc_lookup (canonical_atom_eq_dec P) entries atom.

Definition lookup_symbol
    {P : CertifiedAtomProfile} {I : FixedWidthCarrierProfile}
    (entries : list (VocabularyEntry P I))
    (id : SymbolId I) : option (CanonicalAtom P) :=
  assoc_lookup (symbol_id_eq_dec I)
    (reverse_vocabulary_entries entries) id.

Definition vocabulary_relation_well_formed
    {P : CertifiedAtomProfile} {I : FixedWidthCarrierProfile}
    (entries : list (VocabularyEntry P I)) : Prop :=
  NoDup (map fst entries) /\ NoDup (map snd entries).

Theorem VWENC_103_PUBLISHED_VOCABULARY_IS_AN_EXACT_BIJECTION :
  forall (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (entries : list (VocabularyEntry P I)) atom id,
    vocabulary_relation_well_formed entries ->
    (lookup_atom entries atom = Some id <->
     lookup_symbol entries id = Some atom).
Proof.
  intros P I entries atom id [Hatom_unique Hid_unique].
  split; intros Hlookup.
  - apply assoc_lookup_sound in Hlookup.
    unfold lookup_symbol. apply assoc_lookup_complete_unique.
    + rewrite reverse_vocabulary_keys_are_ids. exact Hid_unique.
    + apply reverse_vocabulary_membership. exact Hlookup.
  - unfold lookup_symbol in Hlookup.
    apply assoc_lookup_sound in Hlookup.
    apply reverse_vocabulary_membership in Hlookup.
    unfold lookup_atom. now apply assoc_lookup_complete_unique.
Qed.

Definition fingerprint_candidate
    {P : CertifiedAtomProfile} (_atom : CanonicalAtom P) : nat := 0.

Definition collision_atom_left : CanonicalAtom canonical_uleb_profile :=
  canonical_uleb_atom [1] (one_byte_uleb_is_canonical 1 ltac:(lia)).

Definition collision_atom_right : CanonicalAtom canonical_uleb_profile :=
  canonical_uleb_atom [2] (one_byte_uleb_is_canonical 2 ltac:(lia)).

Definition u32_carrier : FixedWidthCarrierProfile :=
  {| carrier_format_identity := 32;
     carrier_width_bytes := 4;
     carrier_width_positive := ltac:(lia) |}.

Definition symbol_zero : SymbolId u32_carrier.
Proof.
  refine (@mkSymbolId u32_carrier 0 _).
  apply carrier_capacity_positive.
Defined.

Definition symbol_one : SymbolId u32_carrier.
Proof.
  refine (@mkSymbolId u32_carrier 1 _).
  unfold carrier_capacity, u32_carrier.
  apply Nat.pow_gt_1.
  - lia.
  - discriminate.
Defined.

Definition symbol_two : SymbolId u32_carrier.
Proof.
  refine (@mkSymbolId u32_carrier 2 _).
  change (2 < 256 ^ 4).
  replace 4 with (S 3) by reflexivity.
  rewrite Nat.pow_succ_r by lia.
  set (power := 256 ^ 3).
  assert (Hpower : power <> 0).
  { unfold power. apply Nat.pow_nonzero. lia. }
  nia.
Defined.

Lemma symbol_two_differs_from_symbol_zero :
  symbol_two <> symbol_zero.
Proof.
  intros Hequal.
  apply (f_equal (symbol_id_value u32_carrier)) in Hequal.
  discriminate.
Qed.

Definition term_zero : TermId u32_carrier.
Proof.
  refine (@mkTermId u32_carrier 0 _).
  apply carrier_capacity_positive.
Defined.

Definition witness_vocabulary_fiber :
    VocabularyFiber canonical_uleb_profile u32_carrier :=
  mkVocabularyFiber canonical_uleb_profile u32_carrier 700 1.

Definition collision_vocabulary :
    list (VocabularyEntry canonical_uleb_profile u32_carrier) :=
  [(collision_atom_left, symbol_zero);
   (collision_atom_right, symbol_one)].

Theorem VWENC_104_FINGERPRINT_COLLISION_REQUIRES_FULL_CANONICAL_BYTES :
  fingerprint_candidate collision_atom_left =
    fingerprint_candidate collision_atom_right /\
  collision_atom_left <> collision_atom_right /\
  lookup_atom collision_vocabulary collision_atom_left = Some symbol_zero /\
  lookup_atom collision_vocabulary collision_atom_right = Some symbol_one.
Proof.
  split; [reflexivity |].
  split.
  - intros Hequal.
    apply (f_equal
      (canonical_atom_bytes canonical_uleb_profile)) in Hequal.
    discriminate.
  - split.
    + unfold lookup_atom, collision_vocabulary. simpl.
      destruct (canonical_atom_eq_dec
        canonical_uleb_profile collision_atom_left collision_atom_left);
        [reflexivity | contradiction].
    + unfold lookup_atom, collision_vocabulary. simpl.
      destruct (canonical_atom_eq_dec
        canonical_uleb_profile collision_atom_right collision_atom_left)
        as [Hequal | _].
      * exfalso. apply (f_equal
          (canonical_atom_bytes canonical_uleb_profile)) in Hequal.
        discriminate.
      * destruct (canonical_atom_eq_dec
          canonical_uleb_profile collision_atom_right collision_atom_right);
          [reflexivity | contradiction].
Qed.

Inductive InternLookupDecision
    (I : FixedWidthCarrierProfile) : Type :=
| InternExisting : SymbolId I -> InternLookupDecision I
| InternMissing : InternLookupDecision I.

Definition inspect_interning
    {P : CertifiedAtomProfile} {I : FixedWidthCarrierProfile}
    (entries : list (VocabularyEntry P I))
    (atom : CanonicalAtom P) : InternLookupDecision I :=
  match lookup_atom entries atom with
  | Some id => InternExisting I id
  | None => InternMissing I
  end.

Theorem VWENC_105_EXISTING_ATOM_INTERNING_IS_IDEMPOTENT :
  forall (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (entries : list (VocabularyEntry P I)) atom id,
    lookup_atom entries atom = Some id ->
    inspect_interning entries atom = InternExisting I id.
Proof.
  intros P I entries atom id Hlookup.
  unfold inspect_interning. now rewrite Hlookup.
Qed.

(** ** Packed canonical bytes and non-overwriting reverse spans *)

Record ByteSpan : Type := mkByteSpan {
  span_offset : nat;
  span_length : nat
}.

Definition read_span (storage : list PhysicalByte) (span : ByteSpan)
    : list PhysicalByte :=
  firstn (span_length span) (skipn (span_offset span) storage).

Definition span_in_bounds
    (storage : list PhysicalByte) (span : ByteSpan) : Prop :=
  span_offset span + span_length span <= List.length storage.

Definition SpanEntry (I : FixedWidthCarrierProfile) :=
  (SymbolId I * ByteSpan)%type.

Record PackedAtomStorage (I : FixedWidthCarrierProfile) : Type :=
  mkPackedAtomStorage {
    packed_canonical_bytes : list PhysicalByte;
    packed_reverse_spans : list (SpanEntry I)
  }.

Definition spans_disjoint (left right : ByteSpan) : Prop :=
  span_offset left + span_length left <= span_offset right \/
  span_offset right + span_length right <= span_offset left.

Definition span_contains_offset (span : ByteSpan) (offset : nat) : Prop :=
  span_offset span <= offset < span_offset span + span_length span.

Definition packed_spans_pairwise_disjoint
    {I : FixedWidthCarrierProfile}
    (storage : PackedAtomStorage I) : Prop :=
  forall left_id left_span right_id right_span,
    In (left_id, left_span) (packed_reverse_spans I storage) ->
    In (right_id, right_span) (packed_reverse_spans I storage) ->
    left_id <> right_id ->
    spans_disjoint left_span right_span.

Definition packed_spans_cover_bytes
    {I : FixedWidthCarrierProfile}
    (storage : PackedAtomStorage I) : Prop :=
  forall offset,
    offset < List.length (packed_canonical_bytes I storage) <->
    exists id span,
      In (id, span) (packed_reverse_spans I storage) /\
      span_contains_offset span offset.

Definition lookup_span
    {I : FixedWidthCarrierProfile}
    (storage : PackedAtomStorage I) (id : SymbolId I)
    : option ByteSpan :=
  assoc_lookup (symbol_id_eq_dec I) (packed_reverse_spans I storage) id.

Definition append_packed_atom
    {P : CertifiedAtomProfile} {I : FixedWidthCarrierProfile}
    (storage : PackedAtomStorage I)
    (id : SymbolId I)
    (atom : CanonicalAtom P) : option (PackedAtomStorage I) :=
  match lookup_span storage id with
  | Some _ => None
  | None =>
      let bytes := canonical_atom_bytes P atom in
      let offset := List.length (packed_canonical_bytes I storage) in
      Some
        (mkPackedAtomStorage I
          (packed_canonical_bytes I storage ++ bytes)
          ((id, mkByteSpan offset (List.length bytes)) ::
             packed_reverse_spans I storage))
  end.

Lemma read_appended_suffix_exact :
  forall prefix suffix,
    read_span (prefix ++ suffix)
      (mkByteSpan (List.length prefix) (List.length suffix)) = suffix.
Proof.
  intros prefix suffix.
  unfold read_span. simpl.
  rewrite skipn_app, skipn_all, Nat.sub_diag. simpl.
  apply firstn_all.
Qed.

Lemma read_span_append_preserved :
  forall prefix suffix span,
    span_in_bounds prefix span ->
    read_span (prefix ++ suffix) span = read_span prefix span.
Proof.
  intros prefix suffix [offset count] Hbounds.
  unfold span_in_bounds, read_span in *. simpl in *.
  rewrite skipn_app.
  replace (offset - List.length prefix) with 0 by lia.
  simpl.
  rewrite firstn_app.
  replace (count - List.length (skipn offset prefix)) with 0.
  2: rewrite skipn_length_portable; lia.
  simpl. now rewrite app_nil_r.
Qed.

Theorem VWENC_114_SAFE_PACKED_APPEND_READS_EXACT_CANONICAL_BYTES :
  forall (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (storage updated : PackedAtomStorage I)
    (id : SymbolId I) (atom : CanonicalAtom P),
    append_packed_atom storage id atom = Some updated ->
    exists span,
      lookup_span updated id = Some span /\
      read_span (packed_canonical_bytes I updated) span =
        canonical_atom_bytes P atom /\
      span_length span = List.length (canonical_atom_bytes P atom) /\
      span_in_bounds (packed_canonical_bytes I updated) span.
Proof.
  intros P I storage updated id atom Happend.
  unfold append_packed_atom in Happend.
  destruct (lookup_span storage id) as [occupied |] eqn:Hlookup.
  - discriminate.
  - inversion Happend. subst updated. clear Happend.
    exists
      (mkByteSpan (List.length (packed_canonical_bytes I storage))
        (List.length (canonical_atom_bytes P atom))).
    split.
    + unfold lookup_span. simpl.
      destruct (symbol_id_eq_dec I id id); [reflexivity | contradiction].
    + split.
      * apply read_appended_suffix_exact.
      * split; [reflexivity |].
        unfold span_in_bounds. simpl. rewrite length_app. lia.
Qed.

Theorem VWENC_115_SAFE_PACKED_APPEND_PRESERVES_EXISTING_SPANS :
  forall (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (storage updated : PackedAtomStorage I)
    (new_id existing_id : SymbolId I) (atom : CanonicalAtom P),
    append_packed_atom storage new_id atom = Some updated ->
    existing_id <> new_id ->
    lookup_span updated existing_id = lookup_span storage existing_id.
Proof.
  intros P I storage updated new_id existing_id atom Happend Hdifferent.
  unfold append_packed_atom in Happend.
  destruct (lookup_span storage new_id); [discriminate |].
  inversion Happend. subst updated. clear Happend.
  unfold lookup_span. simpl.
  destruct (symbol_id_eq_dec I existing_id new_id);
    [contradiction | reflexivity].
Qed.

(** ** Term dictionaries and the combined interning state *)

Record TermDictionaryFiber
    (P : CertifiedAtomProfile)
    (I T : FixedWidthCarrierProfile)
    (vocabulary_fiber : VocabularyFiber P I) : Type :=
  mkTermDictionaryFiber {
    term_dictionary_identity : nat;
    term_dictionary_generation : nat
  }.

Definition term_dictionary_fiber_identity
    {P : CertifiedAtomProfile}
    {I T : FixedWidthCarrierProfile}
    {vocabulary_fiber : VocabularyFiber P I}
    (fiber : TermDictionaryFiber P I T vocabulary_fiber) :=
  (vocabulary_fiber_identity vocabulary_fiber,
   (carrier_format_identity T,
    (carrier_width_bytes T,
     (term_dictionary_identity P I T vocabulary_fiber fiber,
      term_dictionary_generation P I T vocabulary_fiber fiber)))).

Definition term_dictionary_fiber_eq_dec
    (P : CertifiedAtomProfile)
    (I T : FixedWidthCarrierProfile)
    (vocabulary_fiber : VocabularyFiber P I)
    (left right : TermDictionaryFiber P I T vocabulary_fiber)
    : {left = right} + {left <> right}.
Proof.
  destruct left as [left_identity left_generation].
  destruct right as [right_identity right_generation].
  destruct (Nat.eq_dec left_identity right_identity)
    as [Hidentity | Hidentity].
  - subst right_identity.
    destruct (Nat.eq_dec left_generation right_generation)
      as [Hgeneration | Hgeneration].
    + subst right_generation. left. reflexivity.
    + right. intros Hequal. inversion Hequal. contradiction.
  - right. intros Hequal. inversion Hequal. contradiction.
Defined.

Record FiberBoundTermId
    (P : CertifiedAtomProfile)
    (I T : FixedWidthCarrierProfile)
    (vocabulary_fiber : VocabularyFiber P I) : Type :=
  mkFiberBoundTermId {
    bound_term_fiber : TermDictionaryFiber P I T vocabulary_fiber;
    bound_term_value : TermId T
  }.

Definition interpret_term_id
    {P : CertifiedAtomProfile}
    {I T : FixedWidthCarrierProfile}
    {vocabulary_fiber : VocabularyFiber P I}
    (expected : TermDictionaryFiber P I T vocabulary_fiber)
    (bound : FiberBoundTermId P I T vocabulary_fiber)
    : option (TermId T) :=
  if term_dictionary_fiber_eq_dec P I T vocabulary_fiber
      expected (bound_term_fiber P I T vocabulary_fiber bound)
  then Some (bound_term_value P I T vocabulary_fiber bound)
  else None.

Theorem VWENC_169_CROSS_TERM_FIBER_ID_INTERPRETATION_IS_REJECTED :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (vocabulary_fiber : VocabularyFiber P I)
    (expected actual : TermDictionaryFiber P I T vocabulary_fiber)
    (id : TermId T),
    expected <> actual ->
    interpret_term_id expected
      (mkFiberBoundTermId P I T vocabulary_fiber actual id) = None.
Proof.
  intros P I T vocabulary_fiber expected actual id Hdifferent.
  unfold interpret_term_id. simpl.
  destruct (term_dictionary_fiber_eq_dec
    P I T vocabulary_fiber expected actual) as [Hequal | _].
  - contradiction.
  - reflexivity.
Qed.

Theorem VWENC_170_SAME_TERM_FIBER_ID_INTERPRETATION_IS_EXACT :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (vocabulary_fiber : VocabularyFiber P I)
    (fiber : TermDictionaryFiber P I T vocabulary_fiber)
    (id : TermId T),
    interpret_term_id fiber
      (mkFiberBoundTermId P I T vocabulary_fiber fiber id) = Some id.
Proof.
  intros P I T vocabulary_fiber fiber id.
  unfold interpret_term_id. simpl.
  destruct (term_dictionary_fiber_eq_dec
    P I T vocabulary_fiber fiber fiber) as [_ | Himpossible].
  - reflexivity.
  - contradiction.
Qed.

Definition symbol_sequence_eq_dec
    (I : FixedWidthCarrierProfile)
    : forall left right : list (SymbolId I),
      {left = right} + {left <> right} :=
  list_eq_dec (symbol_id_eq_dec I).

Definition TermEntry
    (I T : FixedWidthCarrierProfile) :=
  (list (SymbolId I) * TermId T)%type.

Definition reverse_term_entries
    {I T : FixedWidthCarrierProfile}
    (entries : list (TermEntry I T))
    : list (TermId T * list (SymbolId I)) :=
  map (fun entry => (snd entry, fst entry)) entries.

Definition lookup_term_sequence
    {I T : FixedWidthCarrierProfile}
    (entries : list (TermEntry I T))
    (sequence : list (SymbolId I)) : option (TermId T) :=
  assoc_lookup (symbol_sequence_eq_dec I) entries sequence.

Definition lookup_term_id
    {I T : FixedWidthCarrierProfile}
    (entries : list (TermEntry I T))
    (id : TermId T) : option (list (SymbolId I)) :=
  assoc_lookup (term_id_eq_dec T) (reverse_term_entries entries) id.

Lemma reverse_term_membership :
  forall (I T : FixedWidthCarrierProfile)
    (entries : list (TermEntry I T)) sequence id,
    In (id, sequence) (reverse_term_entries entries) <->
    In (sequence, id) entries.
Proof.
  intros I T entries sequence id.
  unfold reverse_term_entries. rewrite in_map_iff.
  split.
  - intros [[entry_sequence entry_id] [Hequal Hin]].
    simpl in Hequal. inversion Hequal. subst. exact Hin.
  - intros Hin. exists (sequence, id). split; [reflexivity | exact Hin].
Qed.

Lemma reverse_term_keys_are_term_ids :
  forall (I T : FixedWidthCarrierProfile)
    (entries : list (TermEntry I T)),
    map fst (reverse_term_entries entries) = map snd entries.
Proof.
  intros I T entries. unfold reverse_term_entries.
  rewrite map_map. apply map_ext. intros [sequence id]. reflexivity.
Qed.

Definition term_relation_well_formed
    {I T : FixedWidthCarrierProfile}
    (entries : list (TermEntry I T)) : Prop :=
  NoDup (map fst entries) /\ NoDup (map snd entries).

Definition packed_entry_exact
    {P : CertifiedAtomProfile} {I : FixedWidthCarrierProfile}
    (storage : PackedAtomStorage I)
    (atom : CanonicalAtom P) (id : SymbolId I) : Prop :=
  exists span,
    lookup_span storage id = Some span /\
    read_span (packed_canonical_bytes I storage) span =
      canonical_atom_bytes P atom /\
    span_length span = List.length (canonical_atom_bytes P atom) /\
    0 < span_length span /\
    span_in_bounds (packed_canonical_bytes I storage) span.

Definition packed_storage_matches_allocations
    {P : CertifiedAtomProfile} {I : FixedWidthCarrierProfile}
    (allocations : list (VocabularyEntry P I))
    (storage : PackedAtomStorage I) : Prop :=
  NoDup (map fst (packed_reverse_spans I storage)) /\
  packed_spans_pairwise_disjoint storage /\
  packed_spans_cover_bytes storage /\
  (forall atom id,
      In (atom, id) allocations ->
      packed_entry_exact storage atom id) /\
  (forall id span,
      In (id, span) (packed_reverse_spans I storage) ->
      exists atom, In (atom, id) allocations).

Lemma packed_reverse_span_in_bounds :
  forall (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (allocations : list (VocabularyEntry P I)) storage id span,
    packed_storage_matches_allocations allocations storage ->
    In (id, span) (packed_reverse_spans I storage) ->
    span_in_bounds (packed_canonical_bytes I storage) span.
Proof.
  intros P I allocations storage id span
    [Hunique [_ [_ [Hexact Hcomplete]]]] Hin.
  destruct (Hcomplete id span Hin) as [atom Hallocation].
  destruct (Hexact atom id Hallocation)
    as [exact_span [Hlookup [_ [_ [_ Hbounds]]]]].
  assert (Hmember_lookup : lookup_span storage id = Some span).
  { unfold lookup_span.
    eapply assoc_lookup_complete_unique; eassumption. }
  rewrite Hmember_lookup in Hlookup. inversion Hlookup.
  exact Hbounds.
Qed.

Lemma lookup_span_none_id_absent :
  forall (I : FixedWidthCarrierProfile)
    (storage : PackedAtomStorage I) id,
    lookup_span storage id = None ->
    ~ In id (map fst (packed_reverse_spans I storage)).
Proof.
  intros I storage id Hnone.
  unfold lookup_span in Hnone.
  now apply assoc_lookup_none_key_absent in Hnone.
Qed.

Lemma packed_storage_matches_allocations_after_append :
  forall (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (history : list (VocabularyEntry P I))
    (storage updated : PackedAtomStorage I)
    (atom : CanonicalAtom P) (id : SymbolId I),
    packed_storage_matches_allocations history storage ->
    ~ In id (map snd history) ->
    append_packed_atom storage id atom = Some updated ->
    packed_storage_matches_allocations ((atom, id) :: history) updated.
Proof.
  intros P I history storage updated atom id Hmatches
    Hid_absent Happend.
  pose proof Hmatches as Hmatches_for_bounds.
  destruct Hmatches as
    [Hspan_unique [Hspans_disjoint
      [Hspans_cover [Hhistory_exact Hspan_complete]]]].
  unfold append_packed_atom in Happend.
  destruct (lookup_span storage id) as [occupied |] eqn:Hid_span.
  - discriminate.
  - inversion Happend. subst updated. clear Happend.
    split.
    + simpl. constructor.
      * now apply lookup_span_none_id_absent.
      * exact Hspan_unique.
    + split.
      * unfold packed_spans_pairwise_disjoint in *.
        intros left_id left_span right_id right_span
          Hleft Hright Hdifferent.
        simpl in Hleft, Hright.
        destruct Hleft as [Hleft_new | Hleft_old];
          destruct Hright as [Hright_new | Hright_old].
        { inversion Hleft_new. inversion Hright_new. subst.
          contradiction. }
        { inversion Hleft_new. subst left_id left_span.
          right.
          pose proof
            (packed_reverse_span_in_bounds
              P I history storage right_id right_span
              Hmatches_for_bounds Hright_old) as Hbounds.
          unfold span_in_bounds in Hbounds. simpl in Hbounds |- *.
          exact Hbounds. }
        { inversion Hright_new. subst right_id right_span.
          left.
          pose proof
            (packed_reverse_span_in_bounds
              P I history storage left_id left_span
              Hmatches_for_bounds Hleft_old) as Hbounds.
          unfold span_in_bounds in Hbounds. simpl in Hbounds |- *.
          exact Hbounds. }
        { now apply Hspans_disjoint with (left_id := left_id)
            (right_id := right_id). }
      * split.
        { unfold packed_spans_cover_bytes in *.
          simpl. intros offset. rewrite length_app. split.
          - intros Hbelow.
            destruct (Nat.lt_ge_cases offset
              (List.length (packed_canonical_bytes I storage)))
              as [Hold | Hnew].
            + destruct (proj1 (Hspans_cover offset) Hold)
                as [existing_id [span [Hin Hcontains]]].
              exists existing_id, span. split; [now right | exact Hcontains].
            + exists id,
                (mkByteSpan
                  (List.length (packed_canonical_bytes I storage))
                  (List.length (canonical_atom_bytes P atom))).
              split; [now left |].
              unfold span_contains_offset. simpl. lia.
          - intros [existing_id [span [Hin Hcontains]]].
            simpl in Hin. destruct Hin as [Hnew | Hold].
            + inversion Hnew. subst existing_id span.
              unfold span_contains_offset in Hcontains. simpl in Hcontains.
              lia.
            + assert (Hbelow_old :
                offset < List.length (packed_canonical_bytes I storage)).
              { apply (proj2 (Hspans_cover offset)).
                exists existing_id, span. now split. }
              lia. }
        { split.
          - intros existing_atom existing_id Hin.
            simpl in Hin. destruct Hin as [Hnew | Hold].
            { inversion Hnew. subst existing_atom existing_id.
              exists
                (mkByteSpan
                  (List.length (packed_canonical_bytes I storage))
                  (List.length (canonical_atom_bytes P atom))).
              split.
              - unfold lookup_span. simpl.
                destruct (symbol_id_eq_dec I id id);
                  [reflexivity | contradiction].
              - split.
                + apply read_appended_suffix_exact.
                + split; [reflexivity |].
                  split.
                  * pose proof
                      (atom_codeword_nonempty P
                        (canonical_atom_bytes P atom)
                        (canonical_atom_valid P atom)) as Hnonempty.
                    destruct (canonical_atom_bytes P atom);
                      simpl; [contradiction | lia].
                  * unfold span_in_bounds. simpl. rewrite length_app. lia. }
            { specialize (Hhistory_exact existing_atom existing_id Hold).
              destruct Hhistory_exact as
                [span [Hlookup [Hread [Hlength [Hpositive Hbounds]]]]].
              assert (Hdifferent : existing_id <> id).
              { intros Hequal. subst existing_id. apply Hid_absent.
                apply in_map with (f := snd) in Hold. exact Hold. }
              exists span. split.
              - unfold lookup_span. simpl.
                destruct (symbol_id_eq_dec I existing_id id);
                  [contradiction | exact Hlookup].
              - split.
                + simpl.
                  rewrite read_span_append_preserved by exact Hbounds.
                  exact Hread.
                + split; [exact Hlength |].
                  split; [exact Hpositive |].
                  unfold span_in_bounds in Hbounds |- *. simpl.
                  rewrite length_app. lia. }
          - intros existing_id span Hin.
            simpl in Hin. destruct Hin as [Hnew | Hold].
            { inversion Hnew. subst existing_id span.
              exists atom. now left. }
            { destruct (Hspan_complete existing_id span Hold)
                as [existing_atom Hhistory].
              exists existing_atom. now right. } }
Qed.

Lemma packed_storage_matches_allocations_permutation :
  forall (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (left right : list (VocabularyEntry P I)) storage,
    Permutation left right ->
    packed_storage_matches_allocations left storage ->
    packed_storage_matches_allocations right storage.
Proof.
  intros P I left right storage Hpermutation
    [Hspan_unique [Hdisjoint [Hcover [Hexact Hcomplete]]]].
  split; [exact Hspan_unique |].
  split; [exact Hdisjoint |].
  split; [exact Hcover |].
  split.
  - intros atom id Hin.
    apply Hexact.
    eapply Permutation_in.
    + exact (Permutation_sym Hpermutation).
    + exact Hin.
  - intros id span Hin.
    destruct (Hcomplete id span Hin) as [atom Hleft].
    exists atom.
    eapply Permutation_in.
    + exact Hpermutation.
    + exact Hleft.
Qed.

Definition live_symbol
    {P : CertifiedAtomProfile} {I : FixedWidthCarrierProfile}
    (live : list (VocabularyEntry P I)) (id : SymbolId I) : Prop :=
  exists atom, In (atom, id) live.

Definition sequence_vocabulary_bound
    {P : CertifiedAtomProfile} {I : FixedWidthCarrierProfile}
    (live : list (VocabularyEntry P I))
    (frontier : nat) (sequence : list (SymbolId I)) : Prop :=
  Forall
    (fun id =>
      symbol_id_value I id < frontier /\ live_symbol live id)
    sequence.

Record InterningState
    (P : CertifiedAtomProfile)
    (I T : FixedWidthCarrierProfile) : Type :=
  mkInterningState {
    state_fiber : VocabularyFiber P I;
    state_term_fiber : TermDictionaryFiber P I T state_fiber;
    state_reserved_entries : list (VocabularyEntry P I);
    state_claimed_entries : list (VocabularyEntry P I);
    state_live_entries : list (VocabularyEntry P I);
    state_ever_entries : list (VocabularyEntry P I);
    state_orphan_entries : list (VocabularyEntry P I);
    state_unmaterialized_orphan_entries : list (VocabularyEntry P I);
    state_packed_storage : PackedAtomStorage I;
    state_allocator_frontier : nat;
    state_sequences : list (list (SymbolId I));
    state_term_dictionary_enabled : bool;
    state_term_entries : list (TermEntry I T)
  }.

Definition lookup_state_term_sequence
    {P : CertifiedAtomProfile}
    {I T : FixedWidthCarrierProfile}
    (state : InterningState P I T)
    (sequence : list (SymbolId I))
    : option
        (FiberBoundTermId
          P I T (state_fiber P I T state)) :=
  if state_term_dictionary_enabled P I T state then
    match lookup_term_sequence
      (state_term_entries P I T state) sequence with
    | Some id =>
        Some
          (mkFiberBoundTermId
            P I T (state_fiber P I T state)
            (state_term_fiber P I T state) id)
    | None => None
    end
  else None.

Theorem VWENC_171_TERM_LOOKUP_RETURNS_EXACT_FIBER_BOUND_ID :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state : InterningState P I T) sequence id,
    state_term_dictionary_enabled P I T state = true ->
    lookup_term_sequence
      (state_term_entries P I T state) sequence = Some id ->
    lookup_state_term_sequence state sequence =
      Some
        (mkFiberBoundTermId
          P I T (state_fiber P I T state)
          (state_term_fiber P I T state) id) /\
    interpret_term_id
      (state_term_fiber P I T state)
      (mkFiberBoundTermId
        P I T (state_fiber P I T state)
        (state_term_fiber P I T state) id) = Some id.
Proof.
  intros P I T state sequence id Henabled Hlookup.
  unfold lookup_state_term_sequence. rewrite Henabled, Hlookup.
  split; [reflexivity |].
  apply VWENC_170_SAME_TERM_FIBER_ID_INTERPRETATION_IS_EXACT.
Qed.

Definition state_allocation_entries
    {P : CertifiedAtomProfile}
    {I T : FixedWidthCarrierProfile}
    (state : InterningState P I T) : list (VocabularyEntry P I) :=
  state_ever_entries P I T state ++
  state_reserved_entries P I T state ++
  state_claimed_entries P I T state ++
  state_orphan_entries P I T state ++
  state_unmaterialized_orphan_entries P I T state.

Definition state_materialized_entries
    {P : CertifiedAtomProfile}
    {I T : FixedWidthCarrierProfile}
    (state : InterningState P I T) : list (VocabularyEntry P I) :=
  state_ever_entries P I T state ++
  state_claimed_entries P I T state ++
  state_orphan_entries P I T state.

Definition state_orphan_ids
    {P : CertifiedAtomProfile}
    {I T : FixedWidthCarrierProfile}
    (state : InterningState P I T) : list (SymbolId I) :=
  map snd
    (state_orphan_entries P I T state ++
     state_unmaterialized_orphan_entries P I T state).

Inductive AllocationStatus :=
| AllocationReserved
| AllocationMaterializedClaimed
| AllocationPublished
| AllocationTombstoned
| AllocationMaterializedOrphaned
| AllocationUnmaterializedOrphaned.

Definition vocabulary_entry_eq_dec
    (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    : forall left right : VocabularyEntry P I,
      {left = right} + {left <> right}.
Proof.
  intros [left_atom left_id] [right_atom right_id].
  destruct (canonical_atom_eq_dec P left_atom right_atom)
    as [Hatom | Hatom].
  - subst right_atom.
    destruct (symbol_id_eq_dec I left_id right_id) as [Hid | Hid].
    + subst right_id. left. reflexivity.
    + right. intros Hequal. inversion Hequal. contradiction.
  - right. intros Hequal. inversion Hequal. contradiction.
Defined.

Definition allocation_status_of
    {P : CertifiedAtomProfile}
    {I T : FixedWidthCarrierProfile}
    (state : InterningState P I T)
    (atom : CanonicalAtom P)
    (id : SymbolId I) : option AllocationStatus :=
  let entry := (atom, id) in
  if in_dec (vocabulary_entry_eq_dec P I) entry
      (state_reserved_entries P I T state)
  then Some AllocationReserved
  else if in_dec (vocabulary_entry_eq_dec P I) entry
      (state_claimed_entries P I T state)
  then Some AllocationMaterializedClaimed
  else if in_dec (vocabulary_entry_eq_dec P I) entry
      (state_orphan_entries P I T state)
  then Some AllocationMaterializedOrphaned
  else if in_dec (vocabulary_entry_eq_dec P I) entry
      (state_unmaterialized_orphan_entries P I T state)
  then Some AllocationUnmaterializedOrphaned
  else if in_dec (vocabulary_entry_eq_dec P I) entry
      (state_ever_entries P I T state)
  then
    if in_dec (vocabulary_entry_eq_dec P I) entry
        (state_live_entries P I T state)
    then Some AllocationPublished
    else Some AllocationTombstoned
  else None.

Definition allocation_has_status
    {P : CertifiedAtomProfile}
    {I T : FixedWidthCarrierProfile}
    (state : InterningState P I T)
    (atom : CanonicalAtom P)
    (id : SymbolId I)
    (status : AllocationStatus) : Prop :=
  allocation_status_of state atom id = Some status.

Definition allocation_status_category
    {P : CertifiedAtomProfile}
    {I T : FixedWidthCarrierProfile}
    (state : InterningState P I T)
    (atom : CanonicalAtom P)
    (id : SymbolId I)
    (status : AllocationStatus) : Prop :=
  match status with
  | AllocationReserved =>
      In (atom, id) (state_reserved_entries P I T state)
  | AllocationMaterializedClaimed =>
      In (atom, id) (state_claimed_entries P I T state)
  | AllocationPublished =>
      In (atom, id) (state_live_entries P I T state) /\
      In (atom, id) (state_ever_entries P I T state)
  | AllocationTombstoned =>
      In (atom, id) (state_ever_entries P I T state) /\
      ~ In (atom, id) (state_live_entries P I T state)
  | AllocationMaterializedOrphaned =>
      In (atom, id) (state_orphan_entries P I T state)
  | AllocationUnmaterializedOrphaned =>
      In (atom, id)
        (state_unmaterialized_orphan_entries P I T state)
  end.

Lemma allocation_status_reserved_from_membership :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state : InterningState P I T) atom id,
    In (atom, id) (state_reserved_entries P I T state) ->
    allocation_has_status state atom id AllocationReserved.
Proof.
  intros P I T state atom id Hin.
  unfold allocation_has_status, allocation_status_of.
  destruct (in_dec (vocabulary_entry_eq_dec P I) (atom, id)
    (state_reserved_entries P I T state)); [reflexivity | contradiction].
Qed.

Lemma allocation_status_materialized_claimed_from_membership :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state : InterningState P I T) atom id,
    ~ In (atom, id) (state_reserved_entries P I T state) ->
    In (atom, id) (state_claimed_entries P I T state) ->
    allocation_has_status state atom id AllocationMaterializedClaimed.
Proof.
  intros P I T state atom id Hreserved Hclaimed.
  unfold allocation_has_status, allocation_status_of.
  destruct (in_dec (vocabulary_entry_eq_dec P I) (atom, id)
    (state_reserved_entries P I T state)); [contradiction |].
  destruct (in_dec (vocabulary_entry_eq_dec P I) (atom, id)
    (state_claimed_entries P I T state)); [reflexivity | contradiction].
Qed.

Lemma allocation_status_materialized_orphan_from_membership :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state : InterningState P I T) atom id,
    ~ In (atom, id) (state_reserved_entries P I T state) ->
    ~ In (atom, id) (state_claimed_entries P I T state) ->
    In (atom, id) (state_orphan_entries P I T state) ->
    allocation_has_status state atom id AllocationMaterializedOrphaned.
Proof.
  intros P I T state atom id Hreserved Hclaimed Horphan.
  unfold allocation_has_status, allocation_status_of.
  destruct (in_dec (vocabulary_entry_eq_dec P I) (atom, id)
    (state_reserved_entries P I T state)); [contradiction |].
  destruct (in_dec (vocabulary_entry_eq_dec P I) (atom, id)
    (state_claimed_entries P I T state)); [contradiction |].
  destruct (in_dec (vocabulary_entry_eq_dec P I) (atom, id)
    (state_orphan_entries P I T state)); [reflexivity | contradiction].
Qed.

Lemma allocation_status_unmaterialized_orphan_from_membership :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state : InterningState P I T) atom id,
    ~ In (atom, id) (state_reserved_entries P I T state) ->
    ~ In (atom, id) (state_claimed_entries P I T state) ->
    ~ In (atom, id) (state_orphan_entries P I T state) ->
    In (atom, id)
      (state_unmaterialized_orphan_entries P I T state) ->
    allocation_has_status state atom id AllocationUnmaterializedOrphaned.
Proof.
  intros P I T state atom id Hreserved Hclaimed Horphan Hunmaterialized.
  unfold allocation_has_status, allocation_status_of.
  destruct (in_dec (vocabulary_entry_eq_dec P I) (atom, id)
    (state_reserved_entries P I T state)); [contradiction |].
  destruct (in_dec (vocabulary_entry_eq_dec P I) (atom, id)
    (state_claimed_entries P I T state)); [contradiction |].
  destruct (in_dec (vocabulary_entry_eq_dec P I) (atom, id)
    (state_orphan_entries P I T state)); [contradiction |].
  destruct (in_dec (vocabulary_entry_eq_dec P I) (atom, id)
    (state_unmaterialized_orphan_entries P I T state));
    [reflexivity | contradiction].
Qed.

Lemma allocation_status_published_from_membership :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state : InterningState P I T) atom id,
    ~ In (atom, id) (state_reserved_entries P I T state) ->
    ~ In (atom, id) (state_claimed_entries P I T state) ->
    ~ In (atom, id) (state_orphan_entries P I T state) ->
    ~ In (atom, id)
      (state_unmaterialized_orphan_entries P I T state) ->
    In (atom, id) (state_ever_entries P I T state) ->
    In (atom, id) (state_live_entries P I T state) ->
    allocation_has_status state atom id AllocationPublished.
Proof.
  intros P I T state atom id
    Hreserved Hclaimed Horphan Hunmaterialized Hever Hlive.
  unfold allocation_has_status, allocation_status_of.
  destruct (in_dec (vocabulary_entry_eq_dec P I) (atom, id)
    (state_reserved_entries P I T state)); [contradiction |].
  destruct (in_dec (vocabulary_entry_eq_dec P I) (atom, id)
    (state_claimed_entries P I T state)); [contradiction |].
  destruct (in_dec (vocabulary_entry_eq_dec P I) (atom, id)
    (state_orphan_entries P I T state)); [contradiction |].
  destruct (in_dec (vocabulary_entry_eq_dec P I) (atom, id)
    (state_unmaterialized_orphan_entries P I T state));
    [contradiction |].
  destruct (in_dec (vocabulary_entry_eq_dec P I) (atom, id)
    (state_ever_entries P I T state)); [| contradiction].
  destruct (in_dec (vocabulary_entry_eq_dec P I) (atom, id)
    (state_live_entries P I T state)); [reflexivity | contradiction].
Qed.

Lemma allocation_status_tombstoned_from_membership :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state : InterningState P I T) atom id,
    ~ In (atom, id) (state_reserved_entries P I T state) ->
    ~ In (atom, id) (state_claimed_entries P I T state) ->
    ~ In (atom, id) (state_orphan_entries P I T state) ->
    ~ In (atom, id)
      (state_unmaterialized_orphan_entries P I T state) ->
    In (atom, id) (state_ever_entries P I T state) ->
    ~ In (atom, id) (state_live_entries P I T state) ->
    allocation_has_status state atom id AllocationTombstoned.
Proof.
  intros P I T state atom id
    Hreserved Hclaimed Horphan Hunmaterialized Hever Hlive.
  unfold allocation_has_status, allocation_status_of.
  destruct (in_dec (vocabulary_entry_eq_dec P I) (atom, id)
    (state_reserved_entries P I T state)); [contradiction |].
  destruct (in_dec (vocabulary_entry_eq_dec P I) (atom, id)
    (state_claimed_entries P I T state)); [contradiction |].
  destruct (in_dec (vocabulary_entry_eq_dec P I) (atom, id)
    (state_orphan_entries P I T state)); [contradiction |].
  destruct (in_dec (vocabulary_entry_eq_dec P I) (atom, id)
    (state_unmaterialized_orphan_entries P I T state));
    [contradiction |].
  destruct (in_dec (vocabulary_entry_eq_dec P I) (atom, id)
    (state_ever_entries P I T state)); [| contradiction].
  destruct (in_dec (vocabulary_entry_eq_dec P I) (atom, id)
    (state_live_entries P I T state)); [contradiction | reflexivity].
Qed.

Theorem VWENC_161_ALLOCATION_STATUS_IS_FUNCTIONALLY_UNIQUE :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state : InterningState P I T) atom id left right,
    allocation_has_status state atom id left ->
    allocation_has_status state atom id right ->
    left = right.
Proof.
  intros P I T state atom id left right Hleft Hright.
  unfold allocation_has_status in *. rewrite Hleft in Hright.
  now inversion Hright.
Qed.

Lemma allocated_entry_has_computed_status :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state : InterningState P I T) atom id,
    In (atom, id) (state_allocation_entries state) ->
    exists status, allocation_has_status state atom id status.
Proof.
  intros P I T state atom id Hallocated.
  unfold allocation_has_status, allocation_status_of.
  destruct (in_dec (vocabulary_entry_eq_dec P I) (atom, id)
    (state_reserved_entries P I T state)) as [Hreserved | Hreserved].
  - now exists AllocationReserved.
  - destruct (in_dec (vocabulary_entry_eq_dec P I) (atom, id)
      (state_claimed_entries P I T state)) as [Hclaimed | Hclaimed].
    + now exists AllocationMaterializedClaimed.
    + destruct (in_dec (vocabulary_entry_eq_dec P I) (atom, id)
        (state_orphan_entries P I T state)) as [Horphan | Horphan].
      * now exists AllocationMaterializedOrphaned.
      * destruct (in_dec (vocabulary_entry_eq_dec P I) (atom, id)
          (state_unmaterialized_orphan_entries P I T state))
          as [Hunmaterialized | Hunmaterialized].
        { now exists AllocationUnmaterializedOrphaned. }
        { destruct (in_dec (vocabulary_entry_eq_dec P I) (atom, id)
            (state_ever_entries P I T state)) as [Hever | Hever].
          - destruct (in_dec (vocabulary_entry_eq_dec P I) (atom, id)
              (state_live_entries P I T state)) as [Hlive | Hlive].
            + now exists AllocationPublished.
            + now exists AllocationTombstoned.
          - exfalso. unfold state_allocation_entries in Hallocated.
            repeat rewrite in_app_iff in Hallocated.
            tauto. }
Qed.

Lemma allocation_status_reports_observable_membership :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state : InterningState P I T) atom id status,
    allocation_has_status state atom id status ->
    match status with
    | AllocationReserved =>
        In (atom, id) (state_reserved_entries P I T state)
    | AllocationMaterializedClaimed =>
        In (atom, id) (state_claimed_entries P I T state)
    | AllocationPublished =>
        In (atom, id) (state_live_entries P I T state)
    | AllocationTombstoned =>
        In (atom, id) (state_ever_entries P I T state) /\
        ~ In (atom, id) (state_live_entries P I T state)
    | AllocationMaterializedOrphaned =>
        In (atom, id) (state_orphan_entries P I T state)
    | AllocationUnmaterializedOrphaned =>
        In (atom, id)
          (state_unmaterialized_orphan_entries P I T state)
    end.
Proof.
  intros P I T state atom id status Hstatus.
  unfold allocation_has_status, allocation_status_of in Hstatus.
  destruct (in_dec (vocabulary_entry_eq_dec P I) (atom, id)
    (state_reserved_entries P I T state)) as [Hreserved | Hreserved].
  - inversion Hstatus. exact Hreserved.
  - destruct (in_dec (vocabulary_entry_eq_dec P I) (atom, id)
      (state_claimed_entries P I T state)) as [Hclaimed | Hclaimed].
    + inversion Hstatus. exact Hclaimed.
    + destruct (in_dec (vocabulary_entry_eq_dec P I) (atom, id)
        (state_orphan_entries P I T state)) as [Horphan | Horphan].
      * inversion Hstatus. exact Horphan.
      * destruct (in_dec (vocabulary_entry_eq_dec P I) (atom, id)
          (state_unmaterialized_orphan_entries P I T state))
          as [Hunmaterialized | Hunmaterialized].
        { inversion Hstatus. exact Hunmaterialized. }
        { destruct (in_dec (vocabulary_entry_eq_dec P I) (atom, id)
            (state_ever_entries P I T state)) as [Hever | Hever].
          - destruct (in_dec (vocabulary_entry_eq_dec P I) (atom, id)
              (state_live_entries P I T state)) as [Hlive | Hlive].
            + inversion Hstatus. exact Hlive.
            + inversion Hstatus. now split.
          - discriminate. }
Qed.

Record InterningStateWellFormed
    {P : CertifiedAtomProfile}
    {I T : FixedWidthCarrierProfile}
    (state : InterningState P I T) : Prop :=
  mkInterningStateWellFormed {
    state_live_bijection :
      vocabulary_relation_well_formed (state_live_entries P I T state);
    state_history_bijection :
      vocabulary_relation_well_formed (state_ever_entries P I T state);
    state_live_is_historical :
      forall atom id,
        In (atom, id) (state_live_entries P I T state) ->
        In (atom, id) (state_ever_entries P I T state);
    state_allocation_ids_unique :
      NoDup (map snd (state_allocation_entries state));
    state_packed_allocations_exact :
      packed_storage_matches_allocations
        (state_materialized_entries state)
        (state_packed_storage P I T state);
    state_allocations_below_sparse_frontier :
      Forall
        (fun entry =>
          symbol_id_value I (snd entry) <
            state_allocator_frontier P I T state)
        (state_allocation_entries state);
    state_frontier_representable :
      state_allocator_frontier P I T state <= carrier_capacity I;
    state_all_sequences_bound :
      Forall
        (sequence_vocabulary_bound
          (state_live_entries P I T state)
          (state_allocator_frontier P I T state))
        (state_sequences P I T state);
    state_term_relation_bijection :
      term_relation_well_formed (state_term_entries P I T state);
    state_term_sequences_bound :
      Forall
        (fun entry =>
          sequence_vocabulary_bound
            (state_live_entries P I T state)
            (state_allocator_frontier P I T state)
            (fst entry))
        (state_term_entries P I T state);
    state_disabled_term_dictionary_is_empty :
      state_term_dictionary_enabled P I T state = false ->
      state_term_entries P I T state = []
  }.

Lemma NoDup_in_separated_segments :
  forall (Element : Type)
    (prefix left middle right suffix : list Element) element,
    NoDup (prefix ++ left ++ middle ++ right ++ suffix) ->
    In element left ->
    In element right ->
    False.
Proof.
  intros Element prefix left middle right suffix element
    Hunique Hleft Hright.
  destruct (in_split element left Hleft)
    as [before [after Hsplit]].
  subst left.
  assert (Hshape :
    prefix ++ (before ++ element :: after) ++ middle ++ right ++ suffix =
      (prefix ++ before) ++
        element :: (after ++ middle ++ right ++ suffix)).
  { repeat rewrite <- app_assoc. reflexivity. }
  rewrite Hshape in Hunique.
  pose proof
    (NoDup_remove_2
      (prefix ++ before)
      (after ++ middle ++ right ++ suffix)
      element Hunique) as Hnot_in_remainder.
  apply Hnot_in_remainder.
  apply in_or_app. right.
  apply in_or_app. right.
  apply in_or_app. right.
  apply in_or_app. left. exact Hright.
Qed.

Inductive AllocationBucket :=
| BucketHistorical
| BucketReserved
| BucketMaterializedClaimed
| BucketMaterializedOrphaned
| BucketUnmaterializedOrphaned.

Definition allocation_bucket_entries
    {P : CertifiedAtomProfile}
    {I T : FixedWidthCarrierProfile}
    (state : InterningState P I T)
    (bucket : AllocationBucket) : list (VocabularyEntry P I) :=
  match bucket with
  | BucketHistorical => state_ever_entries P I T state
  | BucketReserved => state_reserved_entries P I T state
  | BucketMaterializedClaimed => state_claimed_entries P I T state
  | BucketMaterializedOrphaned => state_orphan_entries P I T state
  | BucketUnmaterializedOrphaned =>
      state_unmaterialized_orphan_entries P I T state
  end.

Inductive AllocationBucketPrecedes :
    AllocationBucket -> AllocationBucket -> Prop :=
| HistoricalBeforeReserved :
    AllocationBucketPrecedes BucketHistorical BucketReserved
| HistoricalBeforeClaimed :
    AllocationBucketPrecedes BucketHistorical BucketMaterializedClaimed
| HistoricalBeforeMaterializedOrphan :
    AllocationBucketPrecedes BucketHistorical BucketMaterializedOrphaned
| HistoricalBeforeUnmaterializedOrphan :
    AllocationBucketPrecedes BucketHistorical BucketUnmaterializedOrphaned
| ReservedBeforeClaimed :
    AllocationBucketPrecedes BucketReserved BucketMaterializedClaimed
| ReservedBeforeMaterializedOrphan :
    AllocationBucketPrecedes BucketReserved BucketMaterializedOrphaned
| ReservedBeforeUnmaterializedOrphan :
    AllocationBucketPrecedes BucketReserved BucketUnmaterializedOrphaned
| ClaimedBeforeMaterializedOrphan :
    AllocationBucketPrecedes
      BucketMaterializedClaimed BucketMaterializedOrphaned
| ClaimedBeforeUnmaterializedOrphan :
    AllocationBucketPrecedes
      BucketMaterializedClaimed BucketUnmaterializedOrphaned
| MaterializedOrphanBeforeUnmaterializedOrphan :
    AllocationBucketPrecedes
      BucketMaterializedOrphaned BucketUnmaterializedOrphaned.

Lemma allocation_bucket_precedence_makes_ids_disjoint :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state : InterningState P I T) left right left_atom right_atom id,
    InterningStateWellFormed state ->
    AllocationBucketPrecedes left right ->
    In (left_atom, id) (allocation_bucket_entries state left) ->
    In (right_atom, id) (allocation_bucket_entries state right) ->
    False.
Proof.
  intros P I T state left right left_atom right_atom id
    Hwell Hprecedes Hleft Hright.
  destruct Hwell as [_ _ _ Hunique].
  unfold state_allocation_entries in Hunique.
  repeat rewrite map_app in Hunique.
  repeat rewrite <- app_assoc in Hunique.
  assert (Hleft_id :
    In id (map snd (allocation_bucket_entries state left))).
  { now apply in_map with (f := snd) in Hleft. }
  assert (Hright_id :
    In id (map snd (allocation_bucket_entries state right))).
  { now apply in_map with (f := snd) in Hright. }
  destruct Hprecedes; simpl in Hleft_id, Hright_id.
  - eapply NoDup_in_separated_segments
      with
        (prefix := [])
        (left := map snd (state_ever_entries P I T state))
        (middle := [])
        (right := map snd (state_reserved_entries P I T state))
        (suffix :=
          map snd (state_claimed_entries P I T state) ++
          map snd (state_orphan_entries P I T state) ++
          map snd (state_unmaterialized_orphan_entries P I T state))
        (element := id); simpl; repeat rewrite <- app_assoc; simpl;
        try rewrite app_nil_r; eauto.
  - eapply NoDup_in_separated_segments
      with
        (prefix := [])
        (left := map snd (state_ever_entries P I T state))
        (middle := map snd (state_reserved_entries P I T state))
        (right := map snd (state_claimed_entries P I T state))
        (suffix :=
          map snd (state_orphan_entries P I T state) ++
          map snd (state_unmaterialized_orphan_entries P I T state))
        (element := id); simpl; repeat rewrite <- app_assoc; simpl;
        try rewrite app_nil_r; eauto.
  - eapply NoDup_in_separated_segments
      with
        (prefix := [])
        (left := map snd (state_ever_entries P I T state))
        (middle :=
          map snd (state_reserved_entries P I T state) ++
          map snd (state_claimed_entries P I T state))
        (right := map snd (state_orphan_entries P I T state))
        (suffix :=
          map snd (state_unmaterialized_orphan_entries P I T state))
        (element := id); simpl; repeat rewrite <- app_assoc; simpl;
        try rewrite app_nil_r; eauto.
  - eapply NoDup_in_separated_segments
      with
        (prefix := [])
        (left := map snd (state_ever_entries P I T state))
        (middle :=
          map snd (state_reserved_entries P I T state) ++
          map snd (state_claimed_entries P I T state) ++
          map snd (state_orphan_entries P I T state))
        (right :=
          map snd (state_unmaterialized_orphan_entries P I T state))
        (suffix := [])
        (element := id); simpl; repeat rewrite <- app_assoc; simpl;
        try rewrite app_nil_r; eauto.
  - eapply NoDup_in_separated_segments
      with
        (prefix := map snd (state_ever_entries P I T state))
        (left := map snd (state_reserved_entries P I T state))
        (middle := [])
        (right := map snd (state_claimed_entries P I T state))
        (suffix :=
          map snd (state_orphan_entries P I T state) ++
          map snd (state_unmaterialized_orphan_entries P I T state))
        (element := id); simpl; repeat rewrite <- app_assoc; simpl;
        try rewrite app_nil_r; eauto.
  - eapply NoDup_in_separated_segments
      with
        (prefix := map snd (state_ever_entries P I T state))
        (left := map snd (state_reserved_entries P I T state))
        (middle := map snd (state_claimed_entries P I T state))
        (right := map snd (state_orphan_entries P I T state))
        (suffix :=
          map snd (state_unmaterialized_orphan_entries P I T state))
        (element := id); simpl; repeat rewrite <- app_assoc; simpl;
        try rewrite app_nil_r; eauto.
  - eapply NoDup_in_separated_segments
      with
        (prefix := map snd (state_ever_entries P I T state))
        (left := map snd (state_reserved_entries P I T state))
        (middle :=
          map snd (state_claimed_entries P I T state) ++
          map snd (state_orphan_entries P I T state))
        (right :=
          map snd (state_unmaterialized_orphan_entries P I T state))
        (suffix := [])
        (element := id); simpl; repeat rewrite <- app_assoc; simpl;
        try rewrite app_nil_r; eauto.
  - eapply NoDup_in_separated_segments
      with
        (prefix :=
          map snd (state_ever_entries P I T state) ++
          map snd (state_reserved_entries P I T state))
        (left := map snd (state_claimed_entries P I T state))
        (middle := [])
        (right := map snd (state_orphan_entries P I T state))
        (suffix :=
          map snd (state_unmaterialized_orphan_entries P I T state))
        (element := id); simpl; repeat rewrite <- app_assoc; simpl;
        try rewrite app_nil_r; eauto.
  - eapply NoDup_in_separated_segments
      with
        (prefix :=
          map snd (state_ever_entries P I T state) ++
          map snd (state_reserved_entries P I T state))
        (left := map snd (state_claimed_entries P I T state))
        (middle := map snd (state_orphan_entries P I T state))
        (right :=
          map snd (state_unmaterialized_orphan_entries P I T state))
        (suffix := [])
        (element := id); simpl; repeat rewrite <- app_assoc; simpl;
        try rewrite app_nil_r; eauto.
  - eapply NoDup_in_separated_segments
      with
        (prefix :=
          map snd (state_ever_entries P I T state) ++
          map snd (state_reserved_entries P I T state) ++
          map snd (state_claimed_entries P I T state))
        (left := map snd (state_orphan_entries P I T state))
        (middle := [])
        (right :=
          map snd (state_unmaterialized_orphan_entries P I T state))
        (suffix := [])
        (element := id); simpl; repeat rewrite <- app_assoc; simpl;
        try rewrite app_nil_r; eauto.
Qed.

Theorem VWENC_163_ALLOCATION_STATUS_REPORTS_ITS_EXACT_STATE_CATEGORY :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state : InterningState P I T) atom id status,
    InterningStateWellFormed state ->
    (allocation_has_status state atom id status <->
     allocation_status_category state atom id status).
Proof.
  intros P I T state atom id status Hwell.
  split.
  - intros Hstatus.
    pose proof (allocation_status_reports_observable_membership
      P I T state atom id status Hstatus) as Hmembership.
    destruct status; simpl in *; try exact Hmembership.
    split; [exact Hmembership |].
    now apply (state_live_is_historical state Hwell).
  - intros Hcategory.
    assert (Hdisjoint := allocation_bucket_precedence_makes_ids_disjoint
      P I T state).
    destruct status; simpl in Hcategory.
    + now apply allocation_status_reserved_from_membership.
    + apply allocation_status_materialized_claimed_from_membership.
      * intros Hreserved.
        eapply Hdisjoint with
          (left := BucketReserved)
          (right := BucketMaterializedClaimed); eauto using ReservedBeforeClaimed.
      * exact Hcategory.
    + destruct Hcategory as [Hlive Hever].
      apply allocation_status_published_from_membership; try assumption.
      * intros Hreserved.
        eapply Hdisjoint with
          (left := BucketHistorical) (right := BucketReserved);
          eauto using HistoricalBeforeReserved.
      * intros Hclaimed.
        eapply Hdisjoint with
          (left := BucketHistorical) (right := BucketMaterializedClaimed);
          eauto using HistoricalBeforeClaimed.
      * intros Horphan.
        eapply Hdisjoint with
          (left := BucketHistorical) (right := BucketMaterializedOrphaned);
          eauto using HistoricalBeforeMaterializedOrphan.
      * intros Hunmaterialized.
        eapply Hdisjoint with
          (left := BucketHistorical)
          (right := BucketUnmaterializedOrphaned);
          eauto using HistoricalBeforeUnmaterializedOrphan.
    + destruct Hcategory as [Hever Hnot_live].
      apply allocation_status_tombstoned_from_membership; try assumption.
      * intros Hreserved.
        eapply Hdisjoint with
          (left := BucketHistorical) (right := BucketReserved);
          eauto using HistoricalBeforeReserved.
      * intros Hclaimed.
        eapply Hdisjoint with
          (left := BucketHistorical) (right := BucketMaterializedClaimed);
          eauto using HistoricalBeforeClaimed.
      * intros Horphan.
        eapply Hdisjoint with
          (left := BucketHistorical) (right := BucketMaterializedOrphaned);
          eauto using HistoricalBeforeMaterializedOrphan.
      * intros Hunmaterialized.
        eapply Hdisjoint with
          (left := BucketHistorical)
          (right := BucketUnmaterializedOrphaned);
          eauto using HistoricalBeforeUnmaterializedOrphan.
    + apply allocation_status_materialized_orphan_from_membership.
      * intros Hreserved.
        eapply Hdisjoint with
          (left := BucketReserved) (right := BucketMaterializedOrphaned);
          eauto using ReservedBeforeMaterializedOrphan.
      * intros Hclaimed.
        eapply Hdisjoint with
          (left := BucketMaterializedClaimed)
          (right := BucketMaterializedOrphaned);
          eauto using ClaimedBeforeMaterializedOrphan.
      * exact Hcategory.
    + apply allocation_status_unmaterialized_orphan_from_membership.
      * intros Hreserved.
        eapply Hdisjoint with
          (left := BucketReserved)
          (right := BucketUnmaterializedOrphaned);
          eauto using ReservedBeforeUnmaterializedOrphan.
      * intros Hclaimed.
        eapply Hdisjoint with
          (left := BucketMaterializedClaimed)
          (right := BucketUnmaterializedOrphaned);
          eauto using ClaimedBeforeUnmaterializedOrphan.
      * intros Horphan.
        eapply Hdisjoint with
          (left := BucketMaterializedOrphaned)
          (right := BucketUnmaterializedOrphaned);
          eauto using MaterializedOrphanBeforeUnmaterializedOrphan.
      * exact Hcategory.
Qed.

Theorem VWENC_162_EVERY_ALLOCATED_ENTRY_HAS_ONE_AUTHORITATIVE_STATUS :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state : InterningState P I T) atom id,
    InterningStateWellFormed state ->
    In (atom, id) (state_allocation_entries state) ->
    exists! status, allocation_status_category state atom id status.
Proof.
  intros P I T state atom id Hwell Hallocated.
  destruct (allocated_entry_has_computed_status
    P I T state atom id Hallocated) as [status Hstatus].
  exists status.
  split.
  - now apply (proj1
      (VWENC_163_ALLOCATION_STATUS_REPORTS_ITS_EXACT_STATE_CATEGORY
        P I T state atom id status Hwell)).
  - intros other Hother.
    eapply VWENC_161_ALLOCATION_STATUS_IS_FUNCTIONALLY_UNIQUE.
    + exact Hstatus.
    + now apply (proj2
        (VWENC_163_ALLOCATION_STATUS_REPORTS_ITS_EXACT_STATE_CATEGORY
          P I T state atom id other Hwell)).
Qed.

Lemma NoDup_map_members_with_same_image_are_equal :
  forall (Element Image : Type) (project : Element -> Image)
    (values : list Element) left right,
    NoDup (map project values) ->
    In left values ->
    In right values ->
    project left = project right ->
    left = right.
Proof.
  intros Element Image project values.
  induction values as [| head tail IH]; intros left right
    Hunique Hleft Hright Himage.
  - contradiction.
  - inversion Hunique as [| projected projected_tail
      Hhead_absent Htail_unique]; subst.
    simpl in Hleft, Hright.
    destruct Hleft as [Hleft | Hleft];
      destruct Hright as [Hright | Hright].
    + now subst left; subst right.
    + subst left. exfalso. apply Hhead_absent.
      apply in_map_iff. exists right. split; [symmetry | assumption].
      exact Himage.
    + subst right. exfalso. apply Hhead_absent.
      apply in_map_iff. exists left. now split.
    + exact (IH left right Htail_unique Hleft Hright Himage).
Qed.

Lemma allocation_status_category_entry_is_allocated :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state : InterningState P I T) atom id status,
    allocation_status_category state atom id status ->
    In (atom, id) (state_allocation_entries state).
Proof.
  intros P I T state atom id status Hcategory.
  unfold state_allocation_entries.
  repeat rewrite in_app_iff.
  destruct status; simpl in Hcategory; tauto.
Qed.

Theorem VWENC_188_ALLOCATION_STATUS_CATEGORIES_ARE_PAIRWISE_DISJOINT_BY_ID :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state : InterningState P I T)
    left_atom right_atom id left_status right_status,
    InterningStateWellFormed state ->
    allocation_status_category state left_atom id left_status ->
    allocation_status_category state right_atom id right_status ->
    left_atom = right_atom /\ left_status = right_status.
Proof.
  intros P I T state left_atom right_atom id left_status right_status
    Hwell Hleft_category Hright_category.
  assert (Hleft_allocated :
    In (left_atom, id) (state_allocation_entries state)).
  { now apply allocation_status_category_entry_is_allocated
      with (status := left_status). }
  assert (Hright_allocated :
    In (right_atom, id) (state_allocation_entries state)).
  { now apply allocation_status_category_entry_is_allocated
      with (status := right_status). }
  assert (Hentry : (left_atom, id) = (right_atom, id)).
  { eapply NoDup_map_members_with_same_image_are_equal
      with (project := snd)
           (values := state_allocation_entries state).
    - exact (state_allocation_ids_unique state Hwell).
    - exact Hleft_allocated.
    - exact Hright_allocated.
    - reflexivity. }
  inversion Hentry. subst right_atom.
  split; [reflexivity |].
  eapply VWENC_161_ALLOCATION_STATUS_IS_FUNCTIONALLY_UNIQUE.
  - now apply (proj2
      (VWENC_163_ALLOCATION_STATUS_REPORTS_ITS_EXACT_STATE_CATEGORY
        P I T state left_atom id left_status Hwell)).
  - now apply (proj2
      (VWENC_163_ALLOCATION_STATUS_REPORTS_ITS_EXACT_STATE_CATEGORY
        P I T state left_atom id right_status Hwell)).
Qed.

Definition empty_packed_atom_storage
    (I : FixedWidthCarrierProfile) : PackedAtomStorage I :=
  mkPackedAtomStorage I [] [].

Definition empty_interning_state
    (P : CertifiedAtomProfile)
    (I T : FixedWidthCarrierProfile)
    (fiber : VocabularyFiber P I)
    (term_identity term_generation : nat) : InterningState P I T :=
  {| state_fiber := fiber;
     state_term_fiber :=
       mkTermDictionaryFiber P I T fiber term_identity term_generation;
     state_reserved_entries := [];
     state_claimed_entries := [];
     state_live_entries := [];
     state_ever_entries := [];
     state_orphan_entries := [];
     state_unmaterialized_orphan_entries := [];
     state_packed_storage := empty_packed_atom_storage I;
     state_allocator_frontier := 0;
     state_sequences := [];
     state_term_dictionary_enabled := false;
     state_term_entries := [] |}.

Theorem VWENC_157_EMPTY_INTERNING_STATE_IS_WELL_FORMED :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (fiber : VocabularyFiber P I) term_identity term_generation,
    InterningStateWellFormed
      (empty_interning_state
        P I T fiber term_identity term_generation).
Proof.
  intros P I T fiber term_identity term_generation. constructor; simpl.
  - split; constructor.
  - split; constructor.
  - intros atom id Hin. contradiction.
  - constructor.
  - split.
    + constructor.
    + split.
      * unfold packed_spans_pairwise_disjoint. simpl.
        intros left_id left_span right_id right_span Hleft.
        contradiction.
      * split.
        { unfold packed_spans_cover_bytes. simpl. intros offset. split.
          - lia.
          - intros [id [span [Hin _]]]. contradiction. }
        { split.
          - intros atom id Hin. contradiction.
          - intros id span Hin. contradiction. }
  - constructor.
  - pose proof (carrier_capacity_positive I). lia.
  - constructor.
  - split; constructor.
  - constructor.
  - intros _. reflexivity.
Qed.

Theorem VWENC_158_WELL_FORMED_PACKED_SPANS_ARE_DISJOINT_AND_COVER_EXACTLY :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state : InterningState P I T),
    InterningStateWellFormed state ->
    packed_spans_pairwise_disjoint
      (state_packed_storage P I T state) /\
    packed_spans_cover_bytes
      (state_packed_storage P I T state).
Proof.
  intros P I T state Hwell.
  destruct Hwell as [_ _ _ _ Hpacked].
  destruct Hpacked as [_ [Hdisjoint [Hcover _]]].
  now split.
Qed.

Definition publish_fresh_atom
    {P : CertifiedAtomProfile}
    {I T : FixedWidthCarrierProfile}
    (state : InterningState P I T)
    (atom : CanonicalAtom P)
    (id : SymbolId I) : option (InterningState P I T) :=
  match lookup_atom (state_ever_entries P I T state) atom with
  | Some _ => None
  | None =>
      match lookup_symbol (state_ever_entries P I T state) id with
      | Some _ => None
      | None =>
          if Nat.leb
              (state_allocator_frontier P I T state)
              (symbol_id_value I id)
          then
            match append_packed_atom
              (state_packed_storage P I T state) id atom with
            | None => None
            | Some packed =>
                Some
                  {| state_fiber := state_fiber P I T state;
                     state_term_fiber := state_term_fiber P I T state;
                     state_reserved_entries :=
                       state_reserved_entries P I T state;
                     state_claimed_entries :=
                       state_claimed_entries P I T state;
                     state_live_entries :=
                       (atom, id) :: state_live_entries P I T state;
                     state_ever_entries :=
                       (atom, id) :: state_ever_entries P I T state;
                     state_orphan_entries :=
                       state_orphan_entries P I T state;
                     state_unmaterialized_orphan_entries :=
                       state_unmaterialized_orphan_entries P I T state;
                     state_packed_storage := packed;
                     state_allocator_frontier :=
                       S (symbol_id_value I id);
                     state_sequences := state_sequences P I T state;
                     state_term_dictionary_enabled :=
                       state_term_dictionary_enabled P I T state;
                     state_term_entries := state_term_entries P I T state |}
            end
          else None
      end
  end.

Definition claim_atom_allocation
    {P : CertifiedAtomProfile}
    {I T : FixedWidthCarrierProfile}
    (state : InterningState P I T)
    (atom : CanonicalAtom P)
    (id : SymbolId I) : option (InterningState P I T) :=
  match lookup_atom (state_ever_entries P I T state) atom with
  | Some _ => None
  | None =>
      if Nat.leb
          (state_allocator_frontier P I T state)
          (symbol_id_value I id)
      then
        Some
          {| state_fiber := state_fiber P I T state;
             state_term_fiber := state_term_fiber P I T state;
             state_reserved_entries :=
               (atom, id) :: state_reserved_entries P I T state;
             state_claimed_entries := state_claimed_entries P I T state;
             state_live_entries := state_live_entries P I T state;
             state_ever_entries := state_ever_entries P I T state;
             state_orphan_entries := state_orphan_entries P I T state;
             state_unmaterialized_orphan_entries :=
               state_unmaterialized_orphan_entries P I T state;
             state_packed_storage := state_packed_storage P I T state;
             state_allocator_frontier := S (symbol_id_value I id);
             state_sequences := state_sequences P I T state;
             state_term_dictionary_enabled :=
               state_term_dictionary_enabled P I T state;
             state_term_entries := state_term_entries P I T state |}
      else None
  end.

Theorem VWENC_164_ALLOCATED_IDS_CANNOT_BE_RESERVED_OR_FRESHLY_PUBLISHED_AGAIN :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state : InterningState P I T) existing_atom new_atom id,
    InterningStateWellFormed state ->
    In (existing_atom, id) (state_allocation_entries state) ->
    claim_atom_allocation state new_atom id = None /\
    publish_fresh_atom state new_atom id = None.
Proof.
  intros P I T state existing_atom new_atom id Hwell Hallocated.
  destruct Hwell as [_ _ _ _ _ Hbelow].
  apply Forall_forall with (x := (existing_atom, id)) in Hbelow;
    [| exact Hallocated].
  simpl in Hbelow. split.
  - unfold claim_atom_allocation.
    destruct (lookup_atom (state_ever_entries P I T state) new_atom);
      [reflexivity |].
    destruct (Nat.leb
      (state_allocator_frontier P I T state)
      (symbol_id_value I id)) eqn:Hfrontier; [| reflexivity].
    apply Nat.leb_le in Hfrontier. lia.
  - unfold publish_fresh_atom.
    destruct (lookup_atom (state_ever_entries P I T state) new_atom);
      [reflexivity |].
    destruct (lookup_symbol (state_ever_entries P I T state) id);
      [reflexivity |].
    destruct (Nat.leb
      (state_allocator_frontier P I T state)
      (symbol_id_value I id)) eqn:Hfrontier; [| reflexivity].
    apply Nat.leb_le in Hfrontier. lia.
Qed.

Theorem VWENC_106_FRESH_PUBLICATION_UPDATES_LIVE_HISTORY_AND_PACKED_BYTES :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state updated : InterningState P I T)
    (atom : CanonicalAtom P) (id : SymbolId I),
    publish_fresh_atom state atom id = Some updated ->
    In (atom, id) (state_live_entries P I T updated) /\
    In (atom, id) (state_ever_entries P I T updated) /\
    packed_entry_exact
      (state_packed_storage P I T updated) atom id.
Proof.
  intros P I T state updated atom id Hpublish.
  unfold publish_fresh_atom in Hpublish.
  destruct (lookup_atom (state_ever_entries P I T state) atom);
    [discriminate |].
  destruct (lookup_symbol (state_ever_entries P I T state) id);
    [discriminate |].
  destruct (Nat.leb
    (state_allocator_frontier P I T state)
    (symbol_id_value I id)); [| discriminate].
  destruct (append_packed_atom
    (state_packed_storage P I T state) id atom)
    as [packed |] eqn:Happend; [| discriminate].
  inversion Hpublish. subst updated. clear Hpublish.
  split; [now left |].
  split; [now left |].
  destruct (VWENC_114_SAFE_PACKED_APPEND_READS_EXACT_CANONICAL_BYTES
    P I (state_packed_storage P I T state) packed id atom Happend)
    as [span [Hlookup [Hread [Hlength Hbounds]]]].
  exists span. split; [exact Hlookup |].
  split; [exact Hread |].
  split; [exact Hlength |].
  split.
  - pose proof
      (atom_codeword_nonempty P
        (canonical_atom_bytes P atom)
        (canonical_atom_valid P atom)) as Hnonempty.
    rewrite Hlength.
    destruct (canonical_atom_bytes P atom); simpl; [contradiction | lia].
  - exact Hbounds.
Qed.

Theorem VWENC_107_EVER_PUBLISHED_ID_CANNOT_BE_REBOUND_AFTER_TOMBSTONE :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state : InterningState P I T)
    (new_atom previous_atom : CanonicalAtom P) (id : SymbolId I),
    lookup_symbol (state_ever_entries P I T state) id =
      Some previous_atom ->
    publish_fresh_atom state new_atom id = None.
Proof.
  intros P I T state new_atom previous_atom id Howned.
  unfold publish_fresh_atom.
  destruct (lookup_atom (state_ever_entries P I T state) new_atom);
    [reflexivity |].
  now rewrite Howned.
Qed.

Lemma referenced_ids_are_live_and_below_frontier :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state : InterningState P I T) sequence id,
    InterningStateWellFormed state ->
    In sequence (state_sequences P I T state) ->
    In id sequence ->
    symbol_id_value I id < state_allocator_frontier P I T state /\
    live_symbol (state_live_entries P I T state) id.
Proof.
  intros P I T state sequence id Hwell Hsequence Hid.
  destruct Hwell as [_ _ _ _ _ _ _ Hsequences].
  apply Forall_forall with (x := sequence) in Hsequences;
    [| exact Hsequence].
  now apply Forall_forall with (x := id) in Hsequences.
Qed.

Lemma vocabulary_insert_preserves_well_formedness :
  forall (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (entries : list (VocabularyEntry P I)) atom id,
    vocabulary_relation_well_formed entries ->
    lookup_atom entries atom = None ->
    lookup_symbol entries id = None ->
    vocabulary_relation_well_formed ((atom, id) :: entries).
Proof.
  intros P I entries atom id [Hatom_unique Hid_unique] Hatom Hid.
  split; simpl; constructor.
  - unfold lookup_atom in Hatom.
    now apply assoc_lookup_none_key_absent in Hatom.
  - exact Hatom_unique.
  - unfold lookup_symbol in Hid.
    apply assoc_lookup_none_key_absent in Hid.
    rewrite reverse_vocabulary_keys_are_ids in Hid. exact Hid.
  - exact Hid_unique.
Qed.

Lemma sequence_bound_monotone :
  forall (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (old_live new_live : list (VocabularyEntry P I))
    old_frontier new_frontier sequence,
    (forall atom id, In (atom, id) old_live ->
      In (atom, id) new_live) ->
    old_frontier <= new_frontier ->
    sequence_vocabulary_bound old_live old_frontier sequence ->
    sequence_vocabulary_bound new_live new_frontier sequence.
Proof.
  intros P I old_live new_live old_frontier new_frontier sequence
    Hinclude Hfrontier Hbound.
  apply Forall_forall. intros id Hin.
  apply Forall_forall with (x := id) in Hbound; [| exact Hin].
  destruct Hbound as [Hbelow [atom Hlive]].
  split; [lia |].
  exists atom. now apply Hinclude.
Qed.

Lemma NoDup_app_disjoint_right :
  forall (Element : Type) (left right : list Element) element,
    NoDup (left ++ right) ->
    In element left ->
    ~ In element right.
Proof.
  intros Element left right element Hunique Hin_left Hin_right.
  destruct (in_split element left Hin_left)
    as [prefix [suffix Hleft]].
  subst left.
  assert (Hshape :
    (prefix ++ element :: suffix) ++ right =
    prefix ++ element :: (suffix ++ right)).
  { now rewrite <- app_assoc. }
  rewrite Hshape in Hunique.
  pose proof (NoDup_remove_2 prefix (suffix ++ right) element Hunique)
    as Hnot_in_remainder.
  apply Hnot_in_remainder.
  apply in_or_app. right.
  apply in_or_app. right. exact Hin_right.
Qed.

Lemma allocated_id_has_exact_span :
  forall (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (allocations : list (VocabularyEntry P I)) storage id,
    packed_storage_matches_allocations allocations storage ->
    In id (map snd allocations) ->
    exists atom span,
      In (atom, id) allocations /\
      lookup_span storage id = Some span.
Proof.
  intros P I allocations storage id
    [_ [_ [_ [Hexact _]]]] Hin.
  apply in_map_iff in Hin.
  destruct Hin as [[atom allocated_id] [Hequal Hin]].
  simpl in Hequal. subst allocated_id.
  destruct (Hexact atom id Hin)
    as [span [Hlookup _]].
  exists atom, span. now split.
Qed.

Lemma permutation_move_middle_entry_right :
  forall (Element : Type)
    (before prefix suffix after : list Element) (element : Element),
    Permutation
      (before ++ prefix ++ element :: suffix ++ after)
      (before ++ prefix ++ suffix ++ element :: after).
Proof.
  intros Element before prefix suffix after element.
  apply Permutation_app_head.
  apply Permutation_app_head.
  apply Permutation_middle.
Qed.

Lemma permutation_extract_after_three_prefixes :
  forall (Element : Type)
    (first second third fourth fifth : list Element) (element : Element),
    Permutation
      (first ++ second ++ third ++ element :: fourth ++ fifth)
      (element :: first ++ second ++ third ++ fourth ++ fifth).
Proof.
  intros Element first second third fourth fifth element.
  apply Permutation_sym.
  eapply Permutation_trans.
  - apply Permutation_middle.
  - apply Permutation_app_head.
    eapply Permutation_trans.
    + apply Permutation_middle.
    + apply Permutation_app_head.
      apply Permutation_middle.
Qed.

Lemma permutation_move_after_three_prefixes :
  forall (Element : Type)
    (first second third fourth fifth : list Element) (element : Element),
    Permutation
      (first ++ second ++ third ++ element :: fourth ++ fifth)
      (first ++ second ++ third ++ fourth ++ element :: fifth).
Proof.
  intros Element first second third fourth fifth element.
  apply Permutation_app_head.
  apply Permutation_app_head.
  apply Permutation_app_head.
  apply Permutation_middle.
Qed.

Lemma claim_atom_allocation_preserves_combined_state_well_formedness :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state claimed : InterningState P I T)
    (atom : CanonicalAtom P) (id : SymbolId I),
    InterningStateWellFormed state ->
    claim_atom_allocation state atom id = Some claimed ->
    InterningStateWellFormed claimed.
Proof.
  intros P I T state claimed atom id Hwell Hclaim.
  destruct Hwell as
    [Hlive_bijection Hhistory_bijection Hlive_history
     Hallocation_unique Hpacked Hallocations_below Hfrontier_capacity
     Hsequences Hterm_bijection Hterm_bound Hdisabled].
  unfold claim_atom_allocation in Hclaim.
  destruct (lookup_atom (state_ever_entries P I T state) atom);
    [discriminate |].
  destruct (Nat.leb
    (state_allocator_frontier P I T state)
    (symbol_id_value I id)) eqn:Hfrontier; [| discriminate].
  apply Nat.leb_le in Hfrontier.
  inversion Hclaim. subst claimed. clear Hclaim.
  assert (Hid_absent :
    ~ In id (map snd (state_allocation_entries state))).
  { intros Hin.
    apply in_map_iff in Hin.
    destruct Hin as [[allocated_atom allocated_id] [Hequal Hin]].
    simpl in Hequal. subst allocated_id.
    apply Forall_forall with
      (x := (allocated_atom, id)) in Hallocations_below;
      [simpl in Hallocations_below; lia | exact Hin]. }
  assert (Hallocation_permutation :
    Permutation
      ((atom, id) :: state_allocation_entries state)
      (state_allocation_entries
        {| state_fiber := state_fiber P I T state;
           state_term_fiber := state_term_fiber P I T state;
           state_reserved_entries :=
             (atom, id) :: state_reserved_entries P I T state;
           state_claimed_entries := state_claimed_entries P I T state;
           state_live_entries := state_live_entries P I T state;
           state_ever_entries := state_ever_entries P I T state;
           state_orphan_entries := state_orphan_entries P I T state;
           state_unmaterialized_orphan_entries :=
             state_unmaterialized_orphan_entries P I T state;
           state_packed_storage := state_packed_storage P I T state;
           state_allocator_frontier := S (symbol_id_value I id);
           state_sequences := state_sequences P I T state;
           state_term_dictionary_enabled :=
             state_term_dictionary_enabled P I T state;
           state_term_entries := state_term_entries P I T state |})).
  { unfold state_allocation_entries. simpl.
    apply Permutation_middle. }
  constructor; simpl.
  - exact Hlive_bijection.
  - exact Hhistory_bijection.
  - exact Hlive_history.
  - apply (Permutation_NoDup (Permutation_map snd Hallocation_permutation)).
    constructor; assumption.
  - exact Hpacked.
  - apply Forall_forall. intros entry Hin.
    assert (Hordered :
      In entry ((atom, id) :: state_allocation_entries state)).
    { eapply Permutation_in.
      - exact (Permutation_sym Hallocation_permutation).
      - exact Hin. }
    simpl in Hordered. destruct Hordered as [Hnew | Hold].
    + inversion Hnew. simpl. lia.
    + apply Forall_forall with (x := entry) in Hallocations_below;
        [| exact Hold].
      simpl in *. lia.
  - pose proof (symbol_id_in_range I id). lia.
  - apply Forall_forall. intros sequence Hin.
    apply Forall_forall with (x := sequence) in Hsequences;
      [| exact Hin].
    eapply sequence_bound_monotone; [| | exact Hsequences].
    + intros live_atom live_id Hlive. exact Hlive.
    + lia.
  - exact Hterm_bijection.
  - apply Forall_forall. intros entry Hin.
    apply Forall_forall with (x := entry) in Hterm_bound;
      [| exact Hin].
    eapply sequence_bound_monotone; [| | exact Hterm_bound].
    + intros live_atom live_id Hlive. exact Hlive.
    + lia.
  - exact Hdisabled.
Qed.

Definition materialize_reserved_allocation
    {P : CertifiedAtomProfile}
    {I T : FixedWidthCarrierProfile}
    (state : InterningState P I T)
    (atom : CanonicalAtom P)
    (id : SymbolId I)
    (updated : InterningState P I T) : Prop :=
  exists prefix suffix packed,
    state_reserved_entries P I T state =
      prefix ++ (atom, id) :: suffix /\
    append_packed_atom
      (state_packed_storage P I T state) id atom = Some packed /\
    updated =
      {| state_fiber := state_fiber P I T state;
         state_term_fiber := state_term_fiber P I T state;
         state_reserved_entries := prefix ++ suffix;
         state_claimed_entries :=
           (atom, id) :: state_claimed_entries P I T state;
         state_live_entries := state_live_entries P I T state;
         state_ever_entries := state_ever_entries P I T state;
         state_orphan_entries := state_orphan_entries P I T state;
         state_unmaterialized_orphan_entries :=
           state_unmaterialized_orphan_entries P I T state;
         state_packed_storage := packed;
         state_allocator_frontier := state_allocator_frontier P I T state;
         state_sequences := state_sequences P I T state;
         state_term_dictionary_enabled :=
           state_term_dictionary_enabled P I T state;
         state_term_entries := state_term_entries P I T state |}.

Lemma materialize_reserved_allocation_preserves_combined_state_well_formedness :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state updated : InterningState P I T) atom id,
    InterningStateWellFormed state ->
    materialize_reserved_allocation state atom id updated ->
    InterningStateWellFormed updated.
Proof.
  intros P I T state updated atom id Hwell Hmaterialize.
  destruct Hwell as
    [Hlive_bijection Hhistory_bijection Hlive_history
     Hallocation_unique Hpacked Hallocations_below Hfrontier_capacity
     Hsequences Hterm_bijection Hterm_bound Hdisabled].
  destruct Hmaterialize as
    [prefix [suffix [packed [Hreserved [Happend Hupdated]]]]].
  subst updated.
  assert (Hspan_none :
    lookup_span (state_packed_storage P I T state) id = None).
  { unfold append_packed_atom in Happend.
    destruct (lookup_span (state_packed_storage P I T state) id);
      [discriminate | reflexivity]. }
  assert (Hid_materialized_absent :
    ~ In id (map snd (state_materialized_entries state))).
  { intros Hin.
    destruct (allocated_id_has_exact_span
      P I (state_materialized_entries state)
      (state_packed_storage P I T state) id Hpacked Hin)
      as [allocated_atom [span [_ Hlookup]]].
    rewrite Hspan_none in Hlookup. discriminate. }
  assert (Hallocation_permutation :
    Permutation
      (state_allocation_entries state)
      (state_allocation_entries
        {| state_fiber := state_fiber P I T state;
           state_term_fiber := state_term_fiber P I T state;
           state_reserved_entries := prefix ++ suffix;
           state_claimed_entries :=
             (atom, id) :: state_claimed_entries P I T state;
           state_live_entries := state_live_entries P I T state;
           state_ever_entries := state_ever_entries P I T state;
           state_orphan_entries := state_orphan_entries P I T state;
           state_unmaterialized_orphan_entries :=
             state_unmaterialized_orphan_entries P I T state;
           state_packed_storage := packed;
           state_allocator_frontier := state_allocator_frontier P I T state;
           state_sequences := state_sequences P I T state;
           state_term_dictionary_enabled :=
             state_term_dictionary_enabled P I T state;
           state_term_entries := state_term_entries P I T state |})).
  { unfold state_allocation_entries. simpl. rewrite Hreserved.
    repeat rewrite <- app_assoc.
    apply permutation_move_middle_entry_right. }
  assert (Hmaterialized_permutation :
    Permutation
      ((atom, id) :: state_materialized_entries state)
      (state_materialized_entries
        {| state_fiber := state_fiber P I T state;
           state_term_fiber := state_term_fiber P I T state;
           state_reserved_entries := prefix ++ suffix;
           state_claimed_entries :=
             (atom, id) :: state_claimed_entries P I T state;
           state_live_entries := state_live_entries P I T state;
           state_ever_entries := state_ever_entries P I T state;
           state_orphan_entries := state_orphan_entries P I T state;
           state_unmaterialized_orphan_entries :=
             state_unmaterialized_orphan_entries P I T state;
           state_packed_storage := packed;
           state_allocator_frontier := state_allocator_frontier P I T state;
           state_sequences := state_sequences P I T state;
           state_term_dictionary_enabled :=
             state_term_dictionary_enabled P I T state;
           state_term_entries := state_term_entries P I T state |})).
  { unfold state_materialized_entries. simpl.
    apply Permutation_middle. }
  constructor; simpl.
  - exact Hlive_bijection.
  - exact Hhistory_bijection.
  - exact Hlive_history.
  - apply (Permutation_NoDup (Permutation_map snd Hallocation_permutation)).
    exact Hallocation_unique.
  - eapply packed_storage_matches_allocations_permutation.
    + exact Hmaterialized_permutation.
    + eapply packed_storage_matches_allocations_after_append;
        eassumption.
  - apply Forall_forall. intros entry Hin.
    apply Forall_forall with (x := entry) in Hallocations_below.
    + exact Hallocations_below.
    + eapply Permutation_in.
      * exact (Permutation_sym Hallocation_permutation).
      * exact Hin.
  - exact Hfrontier_capacity.
  - exact Hsequences.
  - exact Hterm_bijection.
  - exact Hterm_bound.
  - exact Hdisabled.
Qed.

Definition orphan_reserved_allocation
    {P : CertifiedAtomProfile}
    {I T : FixedWidthCarrierProfile}
    (state : InterningState P I T)
    (atom : CanonicalAtom P)
    (id : SymbolId I)
    (updated : InterningState P I T) : Prop :=
  exists prefix suffix,
    state_reserved_entries P I T state =
      prefix ++ (atom, id) :: suffix /\
    updated =
      {| state_fiber := state_fiber P I T state;
         state_term_fiber := state_term_fiber P I T state;
         state_reserved_entries := prefix ++ suffix;
         state_claimed_entries := state_claimed_entries P I T state;
         state_live_entries := state_live_entries P I T state;
         state_ever_entries := state_ever_entries P I T state;
         state_orphan_entries := state_orphan_entries P I T state;
         state_unmaterialized_orphan_entries :=
           (atom, id) ::
             state_unmaterialized_orphan_entries P I T state;
         state_packed_storage := state_packed_storage P I T state;
         state_allocator_frontier := state_allocator_frontier P I T state;
         state_sequences := state_sequences P I T state;
         state_term_dictionary_enabled :=
           state_term_dictionary_enabled P I T state;
         state_term_entries := state_term_entries P I T state |}.

Lemma orphan_reserved_allocation_preserves_combined_state_well_formedness :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state updated : InterningState P I T) atom id,
    InterningStateWellFormed state ->
    orphan_reserved_allocation state atom id updated ->
    InterningStateWellFormed updated.
Proof.
  intros P I T state updated atom id Hwell Horphan.
  destruct Hwell as
    [Hlive_bijection Hhistory_bijection Hlive_history
     Hallocation_unique Hpacked Hallocations_below Hfrontier_capacity
     Hsequences Hterm_bijection Hterm_bound Hdisabled].
  destruct Horphan as [prefix [suffix [Hreserved Hupdated]]].
  subst updated.
  assert (Hallocation_permutation :
    Permutation
      (state_allocation_entries state)
      (state_allocation_entries
        {| state_fiber := state_fiber P I T state;
           state_term_fiber := state_term_fiber P I T state;
           state_reserved_entries := prefix ++ suffix;
           state_claimed_entries := state_claimed_entries P I T state;
           state_live_entries := state_live_entries P I T state;
           state_ever_entries := state_ever_entries P I T state;
           state_orphan_entries := state_orphan_entries P I T state;
           state_unmaterialized_orphan_entries :=
             (atom, id) ::
               state_unmaterialized_orphan_entries P I T state;
           state_packed_storage := state_packed_storage P I T state;
           state_allocator_frontier := state_allocator_frontier P I T state;
           state_sequences := state_sequences P I T state;
           state_term_dictionary_enabled :=
             state_term_dictionary_enabled P I T state;
           state_term_entries := state_term_entries P I T state |})).
  { unfold state_allocation_entries. simpl. rewrite Hreserved.
    repeat rewrite <- app_assoc.
    simpl.
    apply Permutation_app_head.
    apply Permutation_app_head.
    repeat rewrite app_assoc.
    apply Permutation_middle. }
  constructor; simpl.
  - exact Hlive_bijection.
  - exact Hhistory_bijection.
  - exact Hlive_history.
  - apply (Permutation_NoDup (Permutation_map snd Hallocation_permutation)).
    exact Hallocation_unique.
  - exact Hpacked.
  - apply Forall_forall. intros entry Hin.
    apply Forall_forall with (x := entry) in Hallocations_below.
    + exact Hallocations_below.
    + eapply Permutation_in.
      * exact (Permutation_sym Hallocation_permutation).
      * exact Hin.
  - exact Hfrontier_capacity.
  - exact Hsequences.
  - exact Hterm_bijection.
  - exact Hterm_bound.
  - exact Hdisabled.
Qed.

Definition publish_claimed_allocation
    {P : CertifiedAtomProfile}
    {I T : FixedWidthCarrierProfile}
    (state : InterningState P I T)
    (atom : CanonicalAtom P)
    (id : SymbolId I)
    (updated : InterningState P I T) : Prop :=
  exists prefix suffix,
    state_claimed_entries P I T state =
      prefix ++ (atom, id) :: suffix /\
    lookup_atom (state_ever_entries P I T state) atom = None /\
    lookup_symbol (state_ever_entries P I T state) id = None /\
    lookup_atom (state_live_entries P I T state) atom = None /\
    lookup_symbol (state_live_entries P I T state) id = None /\
    updated =
      {| state_fiber := state_fiber P I T state;
         state_term_fiber := state_term_fiber P I T state;
         state_reserved_entries := state_reserved_entries P I T state;
         state_claimed_entries := prefix ++ suffix;
         state_live_entries :=
           (atom, id) :: state_live_entries P I T state;
         state_ever_entries :=
           (atom, id) :: state_ever_entries P I T state;
         state_orphan_entries := state_orphan_entries P I T state;
         state_unmaterialized_orphan_entries :=
           state_unmaterialized_orphan_entries P I T state;
         state_packed_storage := state_packed_storage P I T state;
         state_allocator_frontier := state_allocator_frontier P I T state;
         state_sequences := state_sequences P I T state;
         state_term_dictionary_enabled :=
           state_term_dictionary_enabled P I T state;
         state_term_entries := state_term_entries P I T state |}.

Lemma publish_claimed_allocation_preserves_combined_state_well_formedness :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state updated : InterningState P I T) atom id,
    InterningStateWellFormed state ->
    publish_claimed_allocation state atom id updated ->
    InterningStateWellFormed updated.
Proof.
  intros P I T state updated atom id Hwell Hpublish.
  destruct Hwell as
    [Hlive_bijection Hhistory_bijection Hlive_history
     Hallocation_unique Hpacked Hallocations_below Hfrontier_capacity
     Hsequences Hterm_bijection Hterm_bound Hdisabled].
  destruct Hpublish as
    [prefix [suffix
      [Hclaimed [Hatom_history [Hid_history
        [Hatom_live [Hid_live Hupdated]]]]]]].
  subst updated.
  assert (Hallocation_permutation :
    Permutation
      (state_allocation_entries state)
      (state_allocation_entries
        {| state_fiber := state_fiber P I T state;
           state_term_fiber := state_term_fiber P I T state;
           state_reserved_entries := state_reserved_entries P I T state;
           state_claimed_entries := prefix ++ suffix;
           state_live_entries :=
             (atom, id) :: state_live_entries P I T state;
           state_ever_entries :=
             (atom, id) :: state_ever_entries P I T state;
           state_orphan_entries := state_orphan_entries P I T state;
           state_unmaterialized_orphan_entries :=
             state_unmaterialized_orphan_entries P I T state;
           state_packed_storage := state_packed_storage P I T state;
           state_allocator_frontier := state_allocator_frontier P I T state;
           state_sequences := state_sequences P I T state;
           state_term_dictionary_enabled :=
             state_term_dictionary_enabled P I T state;
           state_term_entries := state_term_entries P I T state |})).
  { unfold state_allocation_entries. simpl. rewrite Hclaimed.
    repeat rewrite <- app_assoc.
    apply permutation_extract_after_three_prefixes. }
  assert (Hmaterialized_permutation :
    Permutation
      (state_materialized_entries state)
      (state_materialized_entries
        {| state_fiber := state_fiber P I T state;
           state_term_fiber := state_term_fiber P I T state;
           state_reserved_entries := state_reserved_entries P I T state;
           state_claimed_entries := prefix ++ suffix;
           state_live_entries :=
             (atom, id) :: state_live_entries P I T state;
           state_ever_entries :=
             (atom, id) :: state_ever_entries P I T state;
           state_orphan_entries := state_orphan_entries P I T state;
           state_unmaterialized_orphan_entries :=
             state_unmaterialized_orphan_entries P I T state;
           state_packed_storage := state_packed_storage P I T state;
           state_allocator_frontier := state_allocator_frontier P I T state;
           state_sequences := state_sequences P I T state;
           state_term_dictionary_enabled :=
             state_term_dictionary_enabled P I T state;
           state_term_entries := state_term_entries P I T state |})).
  { unfold state_materialized_entries. simpl. rewrite Hclaimed.
    repeat rewrite <- app_assoc.
    exact
      (permutation_extract_after_three_prefixes
        (VocabularyEntry P I)
        (state_ever_entries P I T state)
        prefix [] suffix
        (state_orphan_entries P I T state)
        (atom, id)). }
  constructor; simpl.
  - now apply vocabulary_insert_preserves_well_formedness.
  - now apply vocabulary_insert_preserves_well_formedness.
  - intros live_atom live_id Hin.
    simpl in Hin. destruct Hin as [Hnew | Hold].
    + inversion Hnew. now left.
    + right. now apply Hlive_history.
  - apply (Permutation_NoDup (Permutation_map snd Hallocation_permutation)).
    exact Hallocation_unique.
  - eapply packed_storage_matches_allocations_permutation.
    + exact Hmaterialized_permutation.
    + exact Hpacked.
  - apply Forall_forall. intros entry Hin.
    apply Forall_forall with (x := entry) in Hallocations_below.
    + exact Hallocations_below.
    + eapply Permutation_in.
      * exact (Permutation_sym Hallocation_permutation).
      * exact Hin.
  - exact Hfrontier_capacity.
  - apply Forall_forall. intros sequence Hin.
    apply Forall_forall with (x := sequence) in Hsequences;
      [| exact Hin].
    eapply (sequence_bound_monotone P I
      (state_live_entries P I T state)
      ((atom, id) :: state_live_entries P I T state)
      (state_allocator_frontier P I T state)
      (state_allocator_frontier P I T state)
      sequence).
    + intros live_atom live_id Hlive. now right.
    + apply le_n.
    + exact Hsequences.
  - exact Hterm_bijection.
  - apply Forall_forall. intros entry Hin.
    apply Forall_forall with (x := entry) in Hterm_bound;
      [| exact Hin].
    eapply (sequence_bound_monotone P I
      (state_live_entries P I T state)
      ((atom, id) :: state_live_entries P I T state)
      (state_allocator_frontier P I T state)
      (state_allocator_frontier P I T state)
      (fst entry)).
    + intros live_atom live_id Hlive. now right.
    + apply le_n.
    + exact Hterm_bound.
  - exact Hdisabled.
Qed.

Definition orphan_claimed_allocation
    {P : CertifiedAtomProfile}
    {I T : FixedWidthCarrierProfile}
    (state : InterningState P I T)
    (atom : CanonicalAtom P)
    (id : SymbolId I)
    (updated : InterningState P I T) : Prop :=
  exists prefix suffix,
    state_claimed_entries P I T state =
      prefix ++ (atom, id) :: suffix /\
    updated =
      {| state_fiber := state_fiber P I T state;
         state_term_fiber := state_term_fiber P I T state;
         state_reserved_entries := state_reserved_entries P I T state;
         state_claimed_entries := prefix ++ suffix;
         state_live_entries := state_live_entries P I T state;
         state_ever_entries := state_ever_entries P I T state;
         state_orphan_entries :=
           (atom, id) :: state_orphan_entries P I T state;
         state_unmaterialized_orphan_entries :=
           state_unmaterialized_orphan_entries P I T state;
         state_packed_storage := state_packed_storage P I T state;
         state_allocator_frontier := state_allocator_frontier P I T state;
         state_sequences := state_sequences P I T state;
         state_term_dictionary_enabled :=
           state_term_dictionary_enabled P I T state;
         state_term_entries := state_term_entries P I T state |}.

Lemma orphan_claimed_allocation_preserves_combined_state_well_formedness :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state updated : InterningState P I T) atom id,
    InterningStateWellFormed state ->
    orphan_claimed_allocation state atom id updated ->
    InterningStateWellFormed updated.
Proof.
  intros P I T state updated atom id Hwell Horphan.
  destruct Hwell as
    [Hlive_bijection Hhistory_bijection Hlive_history
     Hallocation_unique Hpacked Hallocations_below Hfrontier_capacity
     Hsequences Hterm_bijection Hterm_bound Hdisabled].
  destruct Horphan as
    [prefix [suffix [Hclaimed Hupdated]]].
  subst updated.
  assert (Hallocation_permutation :
    Permutation
      (state_allocation_entries state)
      (state_allocation_entries
        {| state_fiber := state_fiber P I T state;
           state_term_fiber := state_term_fiber P I T state;
           state_reserved_entries := state_reserved_entries P I T state;
           state_claimed_entries := prefix ++ suffix;
           state_live_entries := state_live_entries P I T state;
           state_ever_entries := state_ever_entries P I T state;
           state_orphan_entries :=
             (atom, id) :: state_orphan_entries P I T state;
           state_unmaterialized_orphan_entries :=
             state_unmaterialized_orphan_entries P I T state;
           state_packed_storage := state_packed_storage P I T state;
           state_allocator_frontier := state_allocator_frontier P I T state;
           state_sequences := state_sequences P I T state;
           state_term_dictionary_enabled :=
             state_term_dictionary_enabled P I T state;
           state_term_entries := state_term_entries P I T state |})).
  { unfold state_allocation_entries. simpl. rewrite Hclaimed.
    repeat rewrite <- app_assoc.
    apply permutation_move_after_three_prefixes. }
  assert (Hmaterialized_permutation :
    Permutation
      (state_materialized_entries state)
      (state_materialized_entries
        {| state_fiber := state_fiber P I T state;
           state_term_fiber := state_term_fiber P I T state;
           state_reserved_entries := state_reserved_entries P I T state;
           state_claimed_entries := prefix ++ suffix;
           state_live_entries := state_live_entries P I T state;
           state_ever_entries := state_ever_entries P I T state;
           state_orphan_entries :=
             (atom, id) :: state_orphan_entries P I T state;
           state_unmaterialized_orphan_entries :=
             state_unmaterialized_orphan_entries P I T state;
           state_packed_storage := state_packed_storage P I T state;
           state_allocator_frontier := state_allocator_frontier P I T state;
           state_sequences := state_sequences P I T state;
           state_term_dictionary_enabled :=
             state_term_dictionary_enabled P I T state;
           state_term_entries := state_term_entries P I T state |})).
  { unfold state_materialized_entries. simpl. rewrite Hclaimed.
    repeat rewrite <- app_assoc.
    exact
      (permutation_move_after_three_prefixes
        (VocabularyEntry P I)
        (state_ever_entries P I T state)
        prefix [] suffix
        (state_orphan_entries P I T state)
        (atom, id)). }
  constructor; simpl.
  - exact Hlive_bijection.
  - exact Hhistory_bijection.
  - exact Hlive_history.
  - apply (Permutation_NoDup (Permutation_map snd Hallocation_permutation)).
    exact Hallocation_unique.
  - eapply packed_storage_matches_allocations_permutation.
    + exact Hmaterialized_permutation.
    + exact Hpacked.
  - apply Forall_forall. intros entry Hin.
    apply Forall_forall with (x := entry) in Hallocations_below.
    + exact Hallocations_below.
    + eapply Permutation_in.
      * exact (Permutation_sym Hallocation_permutation).
      * exact Hin.
  - exact Hfrontier_capacity.
  - exact Hsequences.
  - exact Hterm_bijection.
  - exact Hterm_bound.
  - exact Hdisabled.
Qed.

Lemma different_id_survives_middle_entry_removal :
  forall (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (prefix suffix : list (VocabularyEntry P I))
    atom id removed_atom removed_id,
    In (atom, id) (prefix ++ (removed_atom, removed_id) :: suffix) ->
    id <> removed_id ->
    In (atom, id) (prefix ++ suffix).
Proof.
  intros P I prefix suffix atom id removed_atom removed_id Hin Hdifferent.
  apply in_app_or in Hin. apply in_or_app.
  destruct Hin as [Hprefix | Htail].
  - now left.
  - simpl in Htail. destruct Htail as [Hremoved | Hsuffix].
    + exfalso. apply Hdifferent.
      apply (f_equal snd) in Hremoved. now symmetry.
    + now right.
Qed.

Lemma different_id_middle_entry_membership_iff :
  forall (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (prefix suffix : list (VocabularyEntry P I))
    atom id removed_atom removed_id,
    id <> removed_id ->
    (In (atom, id) (prefix ++ (removed_atom, removed_id) :: suffix) <->
     In (atom, id) (prefix ++ suffix)).
Proof.
  intros P I prefix suffix atom id removed_atom removed_id Hdifferent.
  split.
  - intros Hin.
    exact (different_id_survives_middle_entry_removal
      P I prefix suffix atom id removed_atom removed_id Hin Hdifferent).
  - intros Hin.
    apply in_app_or in Hin. apply in_or_app.
    destruct Hin as [Hprefix | Hsuffix].
    + now left.
    + right. simpl. now right.
Qed.

Lemma different_id_cons_entry_membership_iff :
  forall (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (entries : list (VocabularyEntry P I))
    atom id inserted_atom inserted_id,
    id <> inserted_id ->
    (In (atom, id) ((inserted_atom, inserted_id) :: entries) <->
     In (atom, id) entries).
Proof.
  intros P I entries atom id inserted_atom inserted_id Hdifferent.
  simpl. split.
  - intros [Hequal | Hin].
    + exfalso. apply Hdifferent.
      apply (f_equal snd) in Hequal. now symmetry.
    + exact Hin.
  - now right.
Qed.

Definition tombstone_published_allocation
    {P : CertifiedAtomProfile}
    {I T : FixedWidthCarrierProfile}
    (state : InterningState P I T)
    (atom : CanonicalAtom P)
    (id : SymbolId I)
    (updated : InterningState P I T) : Prop :=
  exists prefix suffix,
    state_live_entries P I T state =
      prefix ++ (atom, id) :: suffix /\
    Forall (fun sequence => ~ In id sequence)
      (state_sequences P I T state) /\
    Forall (fun entry => ~ In id (fst entry))
      (state_term_entries P I T state) /\
    updated =
      {| state_fiber := state_fiber P I T state;
         state_term_fiber := state_term_fiber P I T state;
         state_reserved_entries := state_reserved_entries P I T state;
         state_claimed_entries := state_claimed_entries P I T state;
         state_live_entries := prefix ++ suffix;
         state_ever_entries := state_ever_entries P I T state;
         state_orphan_entries := state_orphan_entries P I T state;
         state_unmaterialized_orphan_entries :=
           state_unmaterialized_orphan_entries P I T state;
         state_packed_storage := state_packed_storage P I T state;
         state_allocator_frontier := state_allocator_frontier P I T state;
         state_sequences := state_sequences P I T state;
         state_term_dictionary_enabled :=
           state_term_dictionary_enabled P I T state;
         state_term_entries := state_term_entries P I T state |}.

Lemma tombstone_published_allocation_preserves_combined_state_well_formedness :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state updated : InterningState P I T) atom id,
    InterningStateWellFormed state ->
    tombstone_published_allocation state atom id updated ->
    InterningStateWellFormed updated.
Proof.
  intros P I T state updated atom id Hwell Htombstone.
  destruct Hwell as
    [[Hlive_atoms Hlive_ids] Hhistory_bijection Hlive_history
     Hallocation_unique Hpacked Hallocations_below Hfrontier_capacity
     Hsequences Hterm_bijection Hterm_bound Hdisabled].
  destruct Htombstone as
    [prefix [suffix
      [Hlive [Hsequence_excludes [Hterm_excludes Hupdated]]]]].
  subst updated.
  constructor; simpl.
  - split.
    + rewrite Hlive in Hlive_atoms.
      rewrite map_app in Hlive_atoms. simpl in Hlive_atoms.
      rewrite map_app.
      apply NoDup_remove_1 with (a := atom). exact Hlive_atoms.
    + rewrite Hlive in Hlive_ids.
      rewrite map_app in Hlive_ids. simpl in Hlive_ids.
      rewrite map_app.
      apply NoDup_remove_1 with (a := id). exact Hlive_ids.
  - exact Hhistory_bijection.
  - intros live_atom live_id Hin.
    apply Hlive_history. rewrite Hlive.
    apply in_or_app. apply in_app_or in Hin.
    destruct Hin as [Hprefix | Hsuffix].
    + now left.
    + right. simpl. now right.
  - exact Hallocation_unique.
  - exact Hpacked.
  - exact Hallocations_below.
  - exact Hfrontier_capacity.
  - apply Forall_forall. intros sequence Hin_sequence.
    apply Forall_forall with (x := sequence) in Hsequences;
      [| exact Hin_sequence].
    apply Forall_forall with (x := sequence) in Hsequence_excludes;
      [| exact Hin_sequence].
    apply Forall_forall. intros sequence_id Hin_id.
    apply Forall_forall with (x := sequence_id) in Hsequences;
      [| exact Hin_id].
    destruct Hsequences as [Hbelow [live_atom Hlive_entry]].
    split; [exact Hbelow |]. exists live_atom.
    eapply different_id_survives_middle_entry_removal.
    + rewrite Hlive in Hlive_entry. exact Hlive_entry.
    + intros Hequal. subst sequence_id. contradiction.
  - exact Hterm_bijection.
  - apply Forall_forall. intros entry Hin_entry.
    apply Forall_forall with (x := entry) in Hterm_bound;
      [| exact Hin_entry].
    apply Forall_forall with (x := entry) in Hterm_excludes;
      [| exact Hin_entry].
    apply Forall_forall. intros term_id Hin_id.
    apply Forall_forall with (x := term_id) in Hterm_bound;
      [| exact Hin_id].
    destruct Hterm_bound as [Hbelow [live_atom Hlive_entry]].
    split; [exact Hbelow |]. exists live_atom.
    eapply different_id_survives_middle_entry_removal.
    + rewrite Hlive in Hlive_entry. exact Hlive_entry.
    + intros Hequal. subst term_id. contradiction.
  - exact Hdisabled.
Qed.

Definition add_dependent_sequence
    {P : CertifiedAtomProfile}
    {I T : FixedWidthCarrierProfile}
    (state : InterningState P I T)
    (sequence : list (SymbolId I))
    (updated : InterningState P I T) : Prop :=
  sequence_vocabulary_bound
    (state_live_entries P I T state)
    (state_allocator_frontier P I T state)
    sequence /\
  updated =
    {| state_fiber := state_fiber P I T state;
       state_term_fiber := state_term_fiber P I T state;
       state_reserved_entries := state_reserved_entries P I T state;
       state_claimed_entries := state_claimed_entries P I T state;
       state_live_entries := state_live_entries P I T state;
       state_ever_entries := state_ever_entries P I T state;
       state_orphan_entries := state_orphan_entries P I T state;
       state_unmaterialized_orphan_entries :=
         state_unmaterialized_orphan_entries P I T state;
       state_packed_storage := state_packed_storage P I T state;
       state_allocator_frontier := state_allocator_frontier P I T state;
       state_sequences := sequence :: state_sequences P I T state;
       state_term_dictionary_enabled :=
         state_term_dictionary_enabled P I T state;
       state_term_entries := state_term_entries P I T state |}.

Lemma add_dependent_sequence_preserves_combined_state_well_formedness :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state updated : InterningState P I T) sequence,
    InterningStateWellFormed state ->
    add_dependent_sequence state sequence updated ->
    InterningStateWellFormed updated.
Proof.
  intros P I T state updated sequence Hwell [Hbound Hupdated].
  subst updated. destruct Hwell.
  constructor; simpl; try assumption.
  now constructor.
Qed.

Definition enable_term_dictionary
    {P : CertifiedAtomProfile}
    {I T : FixedWidthCarrierProfile}
    (state updated : InterningState P I T) : Prop :=
  updated =
    {| state_fiber := state_fiber P I T state;
       state_term_fiber := state_term_fiber P I T state;
       state_reserved_entries := state_reserved_entries P I T state;
       state_claimed_entries := state_claimed_entries P I T state;
       state_live_entries := state_live_entries P I T state;
       state_ever_entries := state_ever_entries P I T state;
       state_orphan_entries := state_orphan_entries P I T state;
       state_unmaterialized_orphan_entries :=
         state_unmaterialized_orphan_entries P I T state;
       state_packed_storage := state_packed_storage P I T state;
       state_allocator_frontier := state_allocator_frontier P I T state;
       state_sequences := state_sequences P I T state;
       state_term_dictionary_enabled := true;
       state_term_entries := state_term_entries P I T state |}.

Lemma enable_term_dictionary_preserves_combined_state_well_formedness :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state updated : InterningState P I T),
    InterningStateWellFormed state ->
    enable_term_dictionary state updated ->
    InterningStateWellFormed updated.
Proof.
  intros P I T state updated Hwell Hupdated.
  unfold enable_term_dictionary in Hupdated.
  subst updated. destruct Hwell.
  constructor; simpl; try assumption.
  discriminate.
Qed.

Definition disable_empty_term_dictionary
    {P : CertifiedAtomProfile}
    {I T : FixedWidthCarrierProfile}
    (state updated : InterningState P I T) : Prop :=
  state_term_entries P I T state = [] /\
  updated =
    {| state_fiber := state_fiber P I T state;
       state_term_fiber := state_term_fiber P I T state;
       state_reserved_entries := state_reserved_entries P I T state;
       state_claimed_entries := state_claimed_entries P I T state;
       state_live_entries := state_live_entries P I T state;
       state_ever_entries := state_ever_entries P I T state;
       state_orphan_entries := state_orphan_entries P I T state;
       state_unmaterialized_orphan_entries :=
         state_unmaterialized_orphan_entries P I T state;
       state_packed_storage := state_packed_storage P I T state;
       state_allocator_frontier := state_allocator_frontier P I T state;
       state_sequences := state_sequences P I T state;
       state_term_dictionary_enabled := false;
       state_term_entries := state_term_entries P I T state |}.

Lemma disable_empty_term_dictionary_preserves_combined_state_well_formedness :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state updated : InterningState P I T),
    InterningStateWellFormed state ->
    disable_empty_term_dictionary state updated ->
    InterningStateWellFormed updated.
Proof.
  intros P I T state updated Hwell [Hempty Hupdated].
  subst updated. destruct Hwell.
  constructor; simpl; try assumption.
  intros _. exact Hempty.
Qed.

Lemma term_insert_preserves_well_formedness :
  forall (I T : FixedWidthCarrierProfile)
    (entries : list (TermEntry I T)) sequence term_id,
    term_relation_well_formed entries ->
    lookup_term_sequence entries sequence = None ->
    lookup_term_id entries term_id = None ->
    term_relation_well_formed ((sequence, term_id) :: entries).
Proof.
  intros I T entries sequence term_id
    [Hsequence_unique Hid_unique] Hsequence Hid.
  split; simpl; constructor.
  - unfold lookup_term_sequence in Hsequence.
    now apply assoc_lookup_none_key_absent in Hsequence.
  - exact Hsequence_unique.
  - unfold lookup_term_id in Hid.
    apply assoc_lookup_none_key_absent in Hid.
    rewrite reverse_term_keys_are_term_ids in Hid. exact Hid.
  - exact Hid_unique.
Qed.

Definition insert_term_dictionary_entry
    {P : CertifiedAtomProfile}
    {I T : FixedWidthCarrierProfile}
    (state : InterningState P I T)
    (sequence : list (SymbolId I))
    (term_id : TermId T)
    (updated : InterningState P I T) : Prop :=
  state_term_dictionary_enabled P I T state = true /\
  sequence_vocabulary_bound
    (state_live_entries P I T state)
    (state_allocator_frontier P I T state)
    sequence /\
  lookup_term_sequence (state_term_entries P I T state) sequence = None /\
  lookup_term_id (state_term_entries P I T state) term_id = None /\
  updated =
    {| state_fiber := state_fiber P I T state;
       state_term_fiber := state_term_fiber P I T state;
       state_reserved_entries := state_reserved_entries P I T state;
       state_claimed_entries := state_claimed_entries P I T state;
       state_live_entries := state_live_entries P I T state;
       state_ever_entries := state_ever_entries P I T state;
       state_orphan_entries := state_orphan_entries P I T state;
       state_unmaterialized_orphan_entries :=
         state_unmaterialized_orphan_entries P I T state;
       state_packed_storage := state_packed_storage P I T state;
       state_allocator_frontier := state_allocator_frontier P I T state;
       state_sequences := state_sequences P I T state;
       state_term_dictionary_enabled := true;
       state_term_entries :=
         (sequence, term_id) :: state_term_entries P I T state |}.

Lemma insert_term_dictionary_entry_preserves_combined_state_well_formedness :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state updated : InterningState P I T) sequence term_id,
    InterningStateWellFormed state ->
    insert_term_dictionary_entry state sequence term_id updated ->
    InterningStateWellFormed updated.
Proof.
  intros P I T state updated sequence term_id Hwell
    [Henabled [Hbound [Hsequence [Hid Hupdated]]]].
  subst updated.
  destruct Hwell as
    [Hlive_bijection Hhistory_bijection Hlive_history
     Hallocation_unique Hpacked Hallocations_below Hfrontier_capacity
     Hsequences Hterm_bijection Hterm_bound Hdisabled].
  constructor; simpl; try assumption.
  - now apply term_insert_preserves_well_formedness.
  - now constructor.
  - discriminate.
Qed.

Lemma fresh_publication_preserves_combined_state_well_formedness :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state updated : InterningState P I T)
    (atom : CanonicalAtom P) (id : SymbolId I),
    InterningStateWellFormed state ->
    publish_fresh_atom state atom id = Some updated ->
    InterningStateWellFormed updated.
Proof.
  intros P I T state updated atom id Hwell Hpublish.
  destruct Hwell as
    [Hlive_bijection Hhistory_bijection Hlive_history
     Halloc_unique Hpacked Halloc_below Hfrontier_capacity
     Hsequences Hterm_bijection Hterm_bound Hdisabled].
  unfold publish_fresh_atom in Hpublish.
  destruct (lookup_atom (state_ever_entries P I T state) atom)
    as [existing_atom_id |] eqn:Hatom; [discriminate |].
  destruct (lookup_symbol (state_ever_entries P I T state) id)
    as [existing_atom |] eqn:Hid; [discriminate |].
  destruct (Nat.leb
    (state_allocator_frontier P I T state)
    (symbol_id_value I id)) eqn:Hfrontier; [| discriminate].
  apply Nat.leb_le in Hfrontier.
  destruct (append_packed_atom
    (state_packed_storage P I T state) id atom)
    as [packed |] eqn:Happend; [| discriminate].
  inversion Hpublish. subst updated. clear Hpublish.
  assert (Hid_history_absent :
    ~ In id (map snd (state_ever_entries P I T state))).
  { unfold lookup_symbol in Hid.
    apply assoc_lookup_none_key_absent in Hid.
    rewrite reverse_vocabulary_keys_are_ids in Hid. exact Hid. }
  assert (Hatom_history_absent :
    ~ In atom (map fst (state_ever_entries P I T state))).
  { unfold lookup_atom in Hatom.
    now apply assoc_lookup_none_key_absent in Hatom. }
  assert (Hid_allocation_absent :
    ~ In id (map snd (state_allocation_entries state))).
  { intros Hin.
    apply in_map_iff in Hin.
    destruct Hin as [[allocated_atom allocated_id] [Hequal Hin]].
    simpl in Hequal. subst allocated_id.
    apply Forall_forall with
      (x := (allocated_atom, id)) in Halloc_below; [| exact Hin].
    simpl in Halloc_below. lia. }
  assert (Hspan_none :
    lookup_span (state_packed_storage P I T state) id = None).
  { unfold append_packed_atom in Happend.
    destruct (lookup_span (state_packed_storage P I T state) id);
      [discriminate | reflexivity]. }
  assert (Hid_materialized_absent :
    ~ In id (map snd (state_materialized_entries state))).
  { intros Hin.
    destruct (allocated_id_has_exact_span
      P I (state_materialized_entries state)
      (state_packed_storage P I T state) id Hpacked Hin)
      as [allocated_atom [span [_ Hlookup]]].
    rewrite Hspan_none in Hlookup. discriminate. }
  assert (Hatom_live_none :
    lookup_atom (state_live_entries P I T state) atom = None).
  { destruct (lookup_atom (state_live_entries P I T state) atom)
      as [live_id |] eqn:Hlive_lookup; [| reflexivity].
    exfalso. apply Hatom_history_absent.
    unfold lookup_atom in Hlive_lookup.
    apply assoc_lookup_sound in Hlive_lookup.
    apply in_map_iff.
    exists (atom, live_id). split; [reflexivity |].
    now apply Hlive_history. }
  assert (Hid_live_none :
    lookup_symbol (state_live_entries P I T state) id = None).
  { destruct (lookup_symbol (state_live_entries P I T state) id)
      as [live_atom |] eqn:Hlive_lookup; [| reflexivity].
    exfalso. apply Hid_history_absent.
    unfold lookup_symbol in Hlive_lookup.
    apply assoc_lookup_sound in Hlive_lookup.
    apply reverse_vocabulary_membership in Hlive_lookup.
    apply in_map_iff.
    exists (live_atom, id). split; [reflexivity |].
    now apply Hlive_history. }
  constructor; simpl.
  - apply vocabulary_insert_preserves_well_formedness.
    + exact Hlive_bijection.
    + exact Hatom_live_none.
    + exact Hid_live_none.
  - now apply vocabulary_insert_preserves_well_formedness.
  - intros live_atom live_id Hin.
    simpl in Hin. destruct Hin as [Hnew | Hold].
    + inversion Hnew. now left.
    + right. now apply Hlive_history.
  - constructor.
    + exact Hid_allocation_absent.
    + exact Halloc_unique.
  - eapply (packed_storage_matches_allocations_after_append
      P I
      (state_materialized_entries state)
      (state_packed_storage P I T state)
      packed atom id).
    + exact Hpacked.
    + exact Hid_materialized_absent.
    + exact Happend.
  - constructor.
    + simpl. lia.
    + apply Forall_forall. intros entry Hin.
      apply Forall_forall with (x := entry) in Halloc_below;
        [| exact Hin].
      lia.
  - pose proof (symbol_id_in_range I id). lia.
  - apply Forall_forall. intros sequence Hin.
    apply Forall_forall with (x := sequence) in Hsequences;
      [| exact Hin].
    eapply sequence_bound_monotone; [| | exact Hsequences].
    + intros live_atom live_id Hlive. now right.
    + lia.
  - exact Hterm_bijection.
  - apply Forall_forall. intros entry Hin.
    apply Forall_forall with (x := entry) in Hterm_bound;
      [| exact Hin].
    eapply sequence_bound_monotone; [| | exact Hterm_bound].
    + intros live_atom live_id Hlive. now right.
    + lia.
  - exact Hdisabled.
Qed.

Inductive InterningTransition
    {P : CertifiedAtomProfile}
    {I T : FixedWidthCarrierProfile}
    : InterningState P I T -> InterningState P I T -> Prop :=
| TransitionFreshPublication :
    forall state updated atom id,
      publish_fresh_atom state atom id = Some updated ->
      InterningTransition state updated
| TransitionClaimAllocation :
    forall state updated atom id,
      claim_atom_allocation state atom id = Some updated ->
      InterningTransition state updated
| TransitionMaterializeReservation :
    forall state updated atom id,
      materialize_reserved_allocation state atom id updated ->
      InterningTransition state updated
| TransitionPublishClaim :
    forall state updated atom id,
      publish_claimed_allocation state atom id updated ->
      InterningTransition state updated
| TransitionOrphanClaim :
    forall state updated atom id,
      orphan_claimed_allocation state atom id updated ->
      InterningTransition state updated
| TransitionOrphanReservation :
    forall state updated atom id,
      orphan_reserved_allocation state atom id updated ->
      InterningTransition state updated
| TransitionTombstonePublished :
    forall state updated atom id,
      tombstone_published_allocation state atom id updated ->
      InterningTransition state updated
| TransitionAddDependentSequence :
    forall state updated sequence,
      add_dependent_sequence state sequence updated ->
      InterningTransition state updated
| TransitionEnableTermDictionary :
    forall state updated,
      enable_term_dictionary state updated ->
      InterningTransition state updated
| TransitionDisableEmptyTermDictionary :
    forall state updated,
      disable_empty_term_dictionary state updated ->
      InterningTransition state updated
| TransitionInsertTermEntry :
    forall state updated sequence term_id,
      insert_term_dictionary_entry state sequence term_id updated ->
      InterningTransition state updated.

Theorem VWENC_132_EVERY_INTERNING_TRANSITION_PRESERVES_COMBINED_STATE_WELL_FORMEDNESS :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state updated : InterningState P I T),
    InterningStateWellFormed state ->
    InterningTransition state updated ->
    InterningStateWellFormed updated.
Proof.
  intros P I T state updated Hwell Htransition.
  inversion Htransition; subst;
    eauto using
      fresh_publication_preserves_combined_state_well_formedness,
      claim_atom_allocation_preserves_combined_state_well_formedness,
      materialize_reserved_allocation_preserves_combined_state_well_formedness,
      publish_claimed_allocation_preserves_combined_state_well_formedness,
      orphan_claimed_allocation_preserves_combined_state_well_formedness,
      orphan_reserved_allocation_preserves_combined_state_well_formedness,
      tombstone_published_allocation_preserves_combined_state_well_formedness,
      add_dependent_sequence_preserves_combined_state_well_formedness,
      enable_term_dictionary_preserves_combined_state_well_formedness,
      disable_empty_term_dictionary_preserves_combined_state_well_formedness,
      insert_term_dictionary_entry_preserves_combined_state_well_formedness.
Qed.

(** ** Exact allocation transition algebra *)

Inductive AllocationPhase (P : CertifiedAtomProfile) : Type :=
| PhaseUnallocated
| PhaseAllocated : CanonicalAtom P -> AllocationStatus -> AllocationPhase P.

Definition allocation_phase_matches
    {P : CertifiedAtomProfile}
    {I T : FixedWidthCarrierProfile}
    (state : InterningState P I T)
    (id : SymbolId I)
    (phase : AllocationPhase P) : Prop :=
  match phase with
  | PhaseUnallocated _ =>
      forall atom status,
        ~ allocation_status_category state atom id status
  | PhaseAllocated _ atom status =>
      allocation_status_category state atom id status
  end.

Inductive LegalAllocationEdge (P : CertifiedAtomProfile)
    : AllocationPhase P -> AllocationPhase P -> Prop :=
| EdgeFreshToReserved :
    forall atom,
      LegalAllocationEdge P
        (PhaseUnallocated P)
        (PhaseAllocated P atom AllocationReserved)
| EdgeFreshToPublished :
    forall atom,
      LegalAllocationEdge P
        (PhaseUnallocated P)
        (PhaseAllocated P atom AllocationPublished)
| EdgeReservedToMaterializedClaimed :
    forall atom,
      LegalAllocationEdge P
        (PhaseAllocated P atom AllocationReserved)
        (PhaseAllocated P atom AllocationMaterializedClaimed)
| EdgeReservedToUnmaterializedOrphan :
    forall atom,
      LegalAllocationEdge P
        (PhaseAllocated P atom AllocationReserved)
        (PhaseAllocated P atom AllocationUnmaterializedOrphaned)
| EdgeMaterializedClaimedToPublished :
    forall atom,
      LegalAllocationEdge P
        (PhaseAllocated P atom AllocationMaterializedClaimed)
        (PhaseAllocated P atom AllocationPublished)
| EdgeMaterializedClaimedToMaterializedOrphan :
    forall atom,
      LegalAllocationEdge P
        (PhaseAllocated P atom AllocationMaterializedClaimed)
        (PhaseAllocated P atom AllocationMaterializedOrphaned)
| EdgePublishedToTombstoned :
    forall atom,
      LegalAllocationEdge P
        (PhaseAllocated P atom AllocationPublished)
        (PhaseAllocated P atom AllocationTombstoned).

Inductive AllocationDelta
    {P : CertifiedAtomProfile}
    {I T : FixedWidthCarrierProfile}
    (state updated : InterningState P I T) : Prop :=
| AllocationDeltaNone :
    (forall atom id status,
      allocation_status_category state atom id status <->
      allocation_status_category updated atom id status) ->
    AllocationDelta state updated
| AllocationDeltaOne :
    forall id before after,
      LegalAllocationEdge P before after ->
      allocation_phase_matches state id before ->
      allocation_phase_matches updated id after ->
      (forall other_atom other_id status,
        other_id <> id ->
        (allocation_status_category state other_atom other_id status <->
         allocation_status_category updated other_atom other_id status)) ->
      AllocationDelta state updated.

Lemma allocation_category_preserved_by_exact_components :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state updated : InterningState P I T) atom id,
    (In (atom, id) (state_reserved_entries P I T state) <->
     In (atom, id) (state_reserved_entries P I T updated)) ->
    (In (atom, id) (state_claimed_entries P I T state) <->
     In (atom, id) (state_claimed_entries P I T updated)) ->
    (In (atom, id) (state_live_entries P I T state) <->
     In (atom, id) (state_live_entries P I T updated)) ->
    (In (atom, id) (state_ever_entries P I T state) <->
     In (atom, id) (state_ever_entries P I T updated)) ->
    (In (atom, id) (state_orphan_entries P I T state) <->
     In (atom, id) (state_orphan_entries P I T updated)) ->
    (In (atom, id)
       (state_unmaterialized_orphan_entries P I T state) <->
     In (atom, id)
       (state_unmaterialized_orphan_entries P I T updated)) ->
    forall status,
      allocation_status_category state atom id status <->
      allocation_status_category updated atom id status.
Proof.
  intros P I T state updated atom id
    Hreserved Hclaimed Hlive Hever Horphan Hunmaterialized status.
  destruct status; simpl in *; tauto.
Qed.

Lemma allocation_above_frontier_is_unallocated :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state : InterningState P I T) id,
    InterningStateWellFormed state ->
    state_allocator_frontier P I T state <= symbol_id_value I id ->
    allocation_phase_matches state id (PhaseUnallocated P).
Proof.
  intros P I T state id Hwell Habove atom status Hcategory.
  apply allocation_status_category_entry_is_allocated in Hcategory.
  pose proof (state_allocations_below_sparse_frontier state Hwell)
    as Hbelow.
  apply Forall_forall with (x := (atom, id)) in Hbelow;
    [simpl in Hbelow; lia | exact Hcategory].
Qed.

Theorem VWENC_191_TERMINAL_ALLOCATION_PHASES_HAVE_NO_LEGAL_OUTBOUND_EDGE :
  forall (P : CertifiedAtomProfile) atom after,
    (~ LegalAllocationEdge P
       (PhaseAllocated P atom AllocationTombstoned) after) /\
    (~ LegalAllocationEdge P
       (PhaseAllocated P atom AllocationMaterializedOrphaned) after) /\
    (~ LegalAllocationEdge P
       (PhaseAllocated P atom AllocationUnmaterializedOrphaned) after).
Proof.
  intros P atom after. repeat split; intros Hedge; inversion Hedge.
Qed.

Lemma fresh_publication_has_exact_allocation_delta :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state updated : InterningState P I T) atom id,
    InterningStateWellFormed state ->
    publish_fresh_atom state atom id = Some updated ->
    AllocationDelta state updated.
Proof.
  intros P I T state updated atom id Hwell Hpublish.
  unfold publish_fresh_atom in Hpublish.
  destruct (lookup_atom (state_ever_entries P I T state) atom);
    [discriminate |].
  destruct (lookup_symbol (state_ever_entries P I T state) id);
    [discriminate |].
  destruct (Nat.leb
    (state_allocator_frontier P I T state)
    (symbol_id_value I id)) eqn:Hfrontier; [| discriminate].
  destruct (append_packed_atom
    (state_packed_storage P I T state) id atom)
    as [packed |] eqn:Happend; [| discriminate].
  inversion Hpublish. subst updated. clear Hpublish.
  eapply AllocationDeltaOne
    with
      (id := id)
      (before := PhaseUnallocated P)
      (after := PhaseAllocated P atom AllocationPublished).
  - apply EdgeFreshToPublished.
  - apply allocation_above_frontier_is_unallocated; [exact Hwell |].
    now apply Nat.leb_le.
  - simpl. split; now left.
  - intros other_atom other_id status Hother.
    eapply allocation_category_preserved_by_exact_components;
      simpl; try tauto.
    + symmetry. now apply different_id_cons_entry_membership_iff.
    + symmetry. now apply different_id_cons_entry_membership_iff.
Qed.

Lemma claim_allocation_has_exact_allocation_delta :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state updated : InterningState P I T) atom id,
    InterningStateWellFormed state ->
    claim_atom_allocation state atom id = Some updated ->
    AllocationDelta state updated.
Proof.
  intros P I T state updated atom id Hwell Hclaim.
  unfold claim_atom_allocation in Hclaim.
  destruct (lookup_atom (state_ever_entries P I T state) atom);
    [discriminate |].
  destruct (Nat.leb
    (state_allocator_frontier P I T state)
    (symbol_id_value I id)) eqn:Hfrontier; [| discriminate].
  inversion Hclaim. subst updated. clear Hclaim.
  eapply AllocationDeltaOne
    with
      (id := id)
      (before := PhaseUnallocated P)
      (after := PhaseAllocated P atom AllocationReserved).
  - apply EdgeFreshToReserved.
  - apply allocation_above_frontier_is_unallocated; [exact Hwell |].
    now apply Nat.leb_le.
  - simpl. now left.
  - intros other_atom other_id status Hother.
    eapply allocation_category_preserved_by_exact_components;
      simpl; try tauto.
    symmetry. now apply different_id_cons_entry_membership_iff.
Qed.

Lemma materialize_reservation_has_exact_allocation_delta :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state updated : InterningState P I T) atom id,
    InterningStateWellFormed state ->
    materialize_reserved_allocation state atom id updated ->
    AllocationDelta state updated.
Proof.
  intros P I T state updated atom id Hwell Hmaterialize.
  destruct Hmaterialize as
    [prefix [suffix [packed [Hreserved [Happend Hupdated]]]]].
  subst updated.
  eapply AllocationDeltaOne
    with
      (id := id)
      (before := PhaseAllocated P atom AllocationReserved)
      (after := PhaseAllocated P atom AllocationMaterializedClaimed).
  - apply EdgeReservedToMaterializedClaimed.
  - simpl. rewrite Hreserved. apply in_or_app. right. now left.
  - simpl. now left.
  - intros other_atom other_id status Hother.
    eapply allocation_category_preserved_by_exact_components; simpl; try tauto.
    + rewrite Hreserved.
      now apply different_id_middle_entry_membership_iff.
    + symmetry. now apply different_id_cons_entry_membership_iff.
Qed.

Lemma orphan_reservation_has_exact_allocation_delta :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state updated : InterningState P I T) atom id,
    InterningStateWellFormed state ->
    orphan_reserved_allocation state atom id updated ->
    AllocationDelta state updated.
Proof.
  intros P I T state updated atom id Hwell Horphan.
  destruct Horphan as [prefix [suffix [Hreserved Hupdated]]].
  subst updated.
  eapply AllocationDeltaOne
    with
      (id := id)
      (before := PhaseAllocated P atom AllocationReserved)
      (after :=
        PhaseAllocated P atom AllocationUnmaterializedOrphaned).
  - apply EdgeReservedToUnmaterializedOrphan.
  - simpl. rewrite Hreserved. apply in_or_app. right. now left.
  - simpl. now left.
  - intros other_atom other_id status Hother.
    eapply allocation_category_preserved_by_exact_components; simpl; try tauto.
    + rewrite Hreserved.
      now apply different_id_middle_entry_membership_iff.
    + symmetry. now apply different_id_cons_entry_membership_iff.
Qed.

Lemma publish_claim_has_exact_allocation_delta :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state updated : InterningState P I T) atom id,
    InterningStateWellFormed state ->
    publish_claimed_allocation state atom id updated ->
    AllocationDelta state updated.
Proof.
  intros P I T state updated atom id Hwell Hpublish.
  destruct Hpublish as
    [prefix [suffix
      [Hclaimed [_ [_ [_ [_ Hupdated]]]]]]].
  subst updated.
  eapply AllocationDeltaOne
    with
      (id := id)
      (before := PhaseAllocated P atom AllocationMaterializedClaimed)
      (after := PhaseAllocated P atom AllocationPublished).
  - apply EdgeMaterializedClaimedToPublished.
  - simpl. rewrite Hclaimed. apply in_or_app. right. now left.
  - simpl. split; now left.
  - intros other_atom other_id status Hother.
    eapply allocation_category_preserved_by_exact_components; simpl; try tauto.
    + rewrite Hclaimed.
      now apply different_id_middle_entry_membership_iff.
    + symmetry. now apply different_id_cons_entry_membership_iff.
    + symmetry. now apply different_id_cons_entry_membership_iff.
Qed.

Lemma orphan_claim_has_exact_allocation_delta :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state updated : InterningState P I T) atom id,
    InterningStateWellFormed state ->
    orphan_claimed_allocation state atom id updated ->
    AllocationDelta state updated.
Proof.
  intros P I T state updated atom id Hwell Horphan.
  destruct Horphan as [prefix [suffix [Hclaimed Hupdated]]].
  subst updated.
  eapply AllocationDeltaOne
    with
      (id := id)
      (before := PhaseAllocated P atom AllocationMaterializedClaimed)
      (after := PhaseAllocated P atom AllocationMaterializedOrphaned).
  - apply EdgeMaterializedClaimedToMaterializedOrphan.
  - simpl. rewrite Hclaimed. apply in_or_app. right. now left.
  - simpl. now left.
  - intros other_atom other_id status Hother.
    eapply allocation_category_preserved_by_exact_components; simpl; try tauto.
    + rewrite Hclaimed.
      now apply different_id_middle_entry_membership_iff.
    + symmetry. now apply different_id_cons_entry_membership_iff.
Qed.

Lemma tombstone_publication_has_exact_allocation_delta :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state updated : InterningState P I T) atom id,
    InterningStateWellFormed state ->
    tombstone_published_allocation state atom id updated ->
    AllocationDelta state updated.
Proof.
  intros P I T state updated atom id Hwell Htombstone.
  destruct Htombstone as
    [prefix [suffix
      [Hlive [_ [_ Hupdated]]]]].
  assert (Hever : In (atom, id) (state_ever_entries P I T state)).
  { apply (state_live_is_historical state Hwell).
    rewrite Hlive. apply in_or_app. right. now left. }
  assert (Hnot_remaining : ~ In (atom, id) (prefix ++ suffix)).
  { pose proof (state_live_bijection state Hwell) as [_ Hlive_ids].
    rewrite Hlive in Hlive_ids.
    rewrite map_app in Hlive_ids. simpl in Hlive_ids.
    intros Hin.
    assert (Hin_id : In id (map snd prefix ++ map snd suffix)).
    { rewrite <- map_app. now apply in_map with (f := snd) in Hin. }
    eapply (NoDup_remove_2
      (map snd prefix) (map snd suffix) id Hlive_ids).
    exact Hin_id. }
  subst updated.
  eapply AllocationDeltaOne
    with
      (id := id)
      (before := PhaseAllocated P atom AllocationPublished)
      (after := PhaseAllocated P atom AllocationTombstoned).
  - apply EdgePublishedToTombstoned.
  - simpl. split; [| exact Hever].
    rewrite Hlive. apply in_or_app. right. now left.
  - simpl. now split; [exact Hever | exact Hnot_remaining].
  - intros other_atom other_id status Hother.
    eapply allocation_category_preserved_by_exact_components; simpl; try tauto.
    rewrite Hlive.
    now apply different_id_middle_entry_membership_iff.
Qed.

Lemma add_sequence_has_no_allocation_delta :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state updated : InterningState P I T) sequence,
    add_dependent_sequence state sequence updated ->
    AllocationDelta state updated.
Proof.
  intros P I T state updated sequence [_ Hupdated].
  subst updated. apply AllocationDeltaNone.
  intros atom id status. destruct status; reflexivity.
Qed.

Lemma enable_term_dictionary_has_no_allocation_delta :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state updated : InterningState P I T),
    enable_term_dictionary state updated ->
    AllocationDelta state updated.
Proof.
  intros P I T state updated Hupdated.
  unfold enable_term_dictionary in Hupdated. subst updated.
  apply AllocationDeltaNone.
  intros atom id status. destruct status; reflexivity.
Qed.

Lemma disable_term_dictionary_has_no_allocation_delta :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state updated : InterningState P I T),
    disable_empty_term_dictionary state updated ->
    AllocationDelta state updated.
Proof.
  intros P I T state updated [_ Hupdated]. subst updated.
  apply AllocationDeltaNone.
  intros atom id status. destruct status; reflexivity.
Qed.

Lemma insert_term_entry_has_no_allocation_delta :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state updated : InterningState P I T) sequence term_id,
    insert_term_dictionary_entry state sequence term_id updated ->
    AllocationDelta state updated.
Proof.
  intros P I T state updated sequence term_id
    [_ [_ [_ [_ Hupdated]]]].
  subst updated. apply AllocationDeltaNone.
  intros atom id status. destruct status; reflexivity.
Qed.

Theorem VWENC_189_EVERY_INTERNING_TRANSITION_HAS_ONE_EXACT_LEGAL_ALLOCATION_DELTA :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state updated : InterningState P I T),
    InterningStateWellFormed state ->
    InterningTransition state updated ->
    AllocationDelta state updated.
Proof.
  intros P I T state updated Hwell Htransition.
  inversion Htransition; subst;
    eauto using
      fresh_publication_has_exact_allocation_delta,
      claim_allocation_has_exact_allocation_delta,
      materialize_reservation_has_exact_allocation_delta,
      publish_claim_has_exact_allocation_delta,
      orphan_claim_has_exact_allocation_delta,
      orphan_reservation_has_exact_allocation_delta,
      tombstone_publication_has_exact_allocation_delta,
      add_sequence_has_no_allocation_delta,
      enable_term_dictionary_has_no_allocation_delta,
      disable_term_dictionary_has_no_allocation_delta,
      insert_term_entry_has_no_allocation_delta.
Qed.

Theorem VWENC_190_INTERNING_TRANSITIONS_PRESERVE_EVERY_UNAFFECTED_ID_STATUS :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state updated : InterningState P I T),
    InterningStateWellFormed state ->
    InterningTransition state updated ->
    (forall atom id status,
      allocation_status_category state atom id status <->
      allocation_status_category updated atom id status) \/
    exists changed_id,
      forall atom id status,
        id <> changed_id ->
        (allocation_status_category state atom id status <->
         allocation_status_category updated atom id status).
Proof.
  intros P I T state updated Hwell Htransition.
  pose proof
    (VWENC_189_EVERY_INTERNING_TRANSITION_HAS_ONE_EXACT_LEGAL_ALLOCATION_DELTA
      P I T state updated Hwell Htransition) as Hdelta.
  inversion Hdelta as [Hnone | id before after Hedge Hbefore Hafter Hothers];
    subst.
  - now left.
  - right. exists id. exact Hothers.
Qed.

Inductive InterningReachable
    (P : CertifiedAtomProfile)
    (I T : FixedWidthCarrierProfile)
    (fiber : VocabularyFiber P I)
    (term_identity term_generation : nat)
    : InterningState P I T -> Prop :=
| ReachableInitial :
    InterningReachable P I T fiber term_identity term_generation
      (empty_interning_state
        P I T fiber term_identity term_generation)
| ReachableStep :
    forall state updated,
      InterningReachable
        P I T fiber term_identity term_generation state ->
      InterningTransition state updated ->
      InterningReachable
        P I T fiber term_identity term_generation updated.

Theorem VWENC_159_EVERY_REACHABLE_INTERNING_STATE_IS_WELL_FORMED :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (fiber : VocabularyFiber P I) term_identity term_generation state,
    InterningReachable
      P I T fiber term_identity term_generation state ->
    InterningStateWellFormed state.
Proof.
  intros P I T fiber term_identity term_generation state Hreachable.
  induction Hreachable.
  - apply VWENC_157_EMPTY_INTERNING_STATE_IS_WELL_FORMED.
  - eapply VWENC_132_EVERY_INTERNING_TRANSITION_PRESERVES_COMBINED_STATE_WELL_FORMEDNESS;
      eassumption.
Qed.

Definition WitnessInterningState : Type :=
  InterningState canonical_uleb_profile u32_carrier u32_carrier.

Definition witness_initial_state : WitnessInterningState :=
  empty_interning_state
    canonical_uleb_profile u32_carrier u32_carrier
    witness_vocabulary_fiber 900 1.

Definition witness_term_fiber :
    TermDictionaryFiber
      canonical_uleb_profile u32_carrier u32_carrier
      witness_vocabulary_fiber :=
  mkTermDictionaryFiber
    canonical_uleb_profile u32_carrier u32_carrier
    witness_vocabulary_fiber 900 1.

Definition witness_packed_zero : PackedAtomStorage u32_carrier :=
  mkPackedAtomStorage u32_carrier
    [1]
    [(symbol_zero, mkByteSpan 0 1)].

Definition witness_reserved_zero : WitnessInterningState :=
  {| state_fiber := witness_vocabulary_fiber;
     state_term_fiber := witness_term_fiber;
     state_reserved_entries := [(collision_atom_left, symbol_zero)];
     state_claimed_entries := [];
     state_live_entries := [];
     state_ever_entries := [];
     state_orphan_entries := [];
     state_unmaterialized_orphan_entries := [];
     state_packed_storage := empty_packed_atom_storage u32_carrier;
     state_allocator_frontier := 1;
     state_sequences := [];
     state_term_dictionary_enabled := false;
     state_term_entries := [] |}.

Definition witness_materialized_zero : WitnessInterningState :=
  {| state_fiber := witness_vocabulary_fiber;
     state_term_fiber := witness_term_fiber;
     state_reserved_entries := [];
     state_claimed_entries := [(collision_atom_left, symbol_zero)];
     state_live_entries := [];
     state_ever_entries := [];
     state_orphan_entries := [];
     state_unmaterialized_orphan_entries := [];
     state_packed_storage := witness_packed_zero;
     state_allocator_frontier := 1;
     state_sequences := [];
     state_term_dictionary_enabled := false;
     state_term_entries := [] |}.

Definition witness_live_zero : WitnessInterningState :=
  {| state_fiber := witness_vocabulary_fiber;
     state_term_fiber := witness_term_fiber;
     state_reserved_entries := [];
     state_claimed_entries := [];
     state_live_entries := [(collision_atom_left, symbol_zero)];
     state_ever_entries := [(collision_atom_left, symbol_zero)];
     state_orphan_entries := [];
     state_unmaterialized_orphan_entries := [];
     state_packed_storage := witness_packed_zero;
     state_allocator_frontier := 1;
     state_sequences := [];
     state_term_dictionary_enabled := false;
     state_term_entries := [] |}.

Definition witness_tombstoned_zero : WitnessInterningState :=
  {| state_fiber := witness_vocabulary_fiber;
     state_term_fiber := witness_term_fiber;
     state_reserved_entries := [];
     state_claimed_entries := [];
     state_live_entries := [];
     state_ever_entries := [(collision_atom_left, symbol_zero)];
     state_orphan_entries := [];
     state_unmaterialized_orphan_entries := [];
     state_packed_storage := witness_packed_zero;
     state_allocator_frontier := 1;
     state_sequences := [];
     state_term_dictionary_enabled := false;
     state_term_entries := [] |}.

Definition witness_materialized_orphan_zero : WitnessInterningState :=
  {| state_fiber := witness_vocabulary_fiber;
     state_term_fiber := witness_term_fiber;
     state_reserved_entries := [];
     state_claimed_entries := [];
     state_live_entries := [];
     state_ever_entries := [];
     state_orphan_entries := [(collision_atom_left, symbol_zero)];
     state_unmaterialized_orphan_entries := [];
     state_packed_storage := witness_packed_zero;
     state_allocator_frontier := 1;
     state_sequences := [];
     state_term_dictionary_enabled := false;
     state_term_entries := [] |}.

Definition witness_reserved_two_after_orphan : WitnessInterningState :=
  {| state_fiber := witness_vocabulary_fiber;
     state_term_fiber := witness_term_fiber;
     state_reserved_entries := [(collision_atom_right, symbol_two)];
     state_claimed_entries := [];
     state_live_entries := [];
     state_ever_entries := [];
     state_orphan_entries := [(collision_atom_left, symbol_zero)];
     state_unmaterialized_orphan_entries := [];
     state_packed_storage := witness_packed_zero;
     state_allocator_frontier := 3;
     state_sequences := [];
     state_term_dictionary_enabled := false;
     state_term_entries := [] |}.

Definition witness_sparse_orphan_state : WitnessInterningState :=
  {| state_fiber := witness_vocabulary_fiber;
     state_term_fiber := witness_term_fiber;
     state_reserved_entries := [];
     state_claimed_entries := [];
     state_live_entries := [];
     state_ever_entries := [];
     state_orphan_entries := [(collision_atom_left, symbol_zero)];
     state_unmaterialized_orphan_entries :=
       [(collision_atom_right, symbol_two)];
     state_packed_storage := witness_packed_zero;
     state_allocator_frontier := 3;
     state_sequences := [];
     state_term_dictionary_enabled := false;
     state_term_entries := [] |}.

Definition witness_live_sequence_zero : WitnessInterningState :=
  {| state_fiber := witness_vocabulary_fiber;
     state_term_fiber := witness_term_fiber;
     state_reserved_entries := [];
     state_claimed_entries := [];
     state_live_entries := [(collision_atom_left, symbol_zero)];
     state_ever_entries := [(collision_atom_left, symbol_zero)];
     state_orphan_entries := [];
     state_unmaterialized_orphan_entries := [];
     state_packed_storage := witness_packed_zero;
     state_allocator_frontier := 1;
     state_sequences := [[symbol_zero]];
     state_term_dictionary_enabled := false;
     state_term_entries := [] |}.

Definition witness_live_term_enabled : WitnessInterningState :=
  {| state_fiber := witness_vocabulary_fiber;
     state_term_fiber := witness_term_fiber;
     state_reserved_entries := [];
     state_claimed_entries := [];
     state_live_entries := [(collision_atom_left, symbol_zero)];
     state_ever_entries := [(collision_atom_left, symbol_zero)];
     state_orphan_entries := [];
     state_unmaterialized_orphan_entries := [];
     state_packed_storage := witness_packed_zero;
     state_allocator_frontier := 1;
     state_sequences := [];
     state_term_dictionary_enabled := true;
     state_term_entries := [] |}.

Definition witness_live_term_entry : WitnessInterningState :=
  {| state_fiber := witness_vocabulary_fiber;
     state_term_fiber := witness_term_fiber;
     state_reserved_entries := [];
     state_claimed_entries := [];
     state_live_entries := [(collision_atom_left, symbol_zero)];
     state_ever_entries := [(collision_atom_left, symbol_zero)];
     state_orphan_entries := [];
     state_unmaterialized_orphan_entries := [];
     state_packed_storage := witness_packed_zero;
     state_allocator_frontier := 1;
     state_sequences := [];
     state_term_dictionary_enabled := true;
     state_term_entries := [([symbol_zero], term_zero)] |}.

Lemma witness_reserve_zero :
  claim_atom_allocation
    witness_initial_state collision_atom_left symbol_zero =
  Some witness_reserved_zero.
Proof. reflexivity. Qed.

Lemma witness_materialize_zero :
  materialize_reserved_allocation
    witness_reserved_zero collision_atom_left symbol_zero
    witness_materialized_zero.
Proof.
  unfold materialize_reserved_allocation.
  exists [], [], witness_packed_zero. repeat split; reflexivity.
Qed.

Lemma witness_publish_zero :
  publish_claimed_allocation
    witness_materialized_zero collision_atom_left symbol_zero
    witness_live_zero.
Proof.
  unfold publish_claimed_allocation.
  exists [], []. repeat split; reflexivity.
Qed.

Lemma witness_orphan_materialized_zero :
  orphan_claimed_allocation
    witness_materialized_zero collision_atom_left symbol_zero
    witness_materialized_orphan_zero.
Proof.
  unfold orphan_claimed_allocation.
  exists [], []. now split.
Qed.

Lemma witness_reserve_two_after_orphan :
  claim_atom_allocation
    witness_materialized_orphan_zero collision_atom_right symbol_two =
  Some witness_reserved_two_after_orphan.
Proof. reflexivity. Qed.

Lemma witness_orphan_unmaterialized_two :
  orphan_reserved_allocation
    witness_reserved_two_after_orphan collision_atom_right symbol_two
    witness_sparse_orphan_state.
Proof.
  unfold orphan_reserved_allocation.
  exists [], []. now split.
Qed.

Lemma witness_fresh_publish_zero :
  publish_fresh_atom
    witness_initial_state collision_atom_left symbol_zero =
  Some witness_live_zero.
Proof. reflexivity. Qed.

Lemma witness_tombstone_zero :
  tombstone_published_allocation
    witness_live_zero collision_atom_left symbol_zero
    witness_tombstoned_zero.
Proof.
  unfold tombstone_published_allocation.
  exists [], []. repeat split; constructor.
Qed.

Lemma witness_add_live_sequence :
  add_dependent_sequence
    witness_live_zero [symbol_zero] witness_live_sequence_zero.
Proof.
  unfold add_dependent_sequence. split.
  - constructor.
    + split; [simpl; lia |].
      exists collision_atom_left. now left.
    + constructor.
  - reflexivity.
Qed.

Lemma witness_enable_term_dictionary :
  enable_term_dictionary witness_live_zero witness_live_term_enabled.
Proof. reflexivity. Qed.

Lemma witness_disable_empty_term_dictionary :
  disable_empty_term_dictionary witness_live_term_enabled witness_live_zero.
Proof. now split. Qed.

Lemma witness_insert_term_entry :
  insert_term_dictionary_entry
    witness_live_term_enabled [symbol_zero] term_zero
    witness_live_term_entry.
Proof.
  unfold insert_term_dictionary_entry. repeat split.
  - constructor.
    + split; [simpl; lia |].
      exists collision_atom_left. now left.
    + constructor.
Qed.

Lemma witness_sparse_state_is_reachable :
  InterningReachable
    canonical_uleb_profile u32_carrier u32_carrier
    witness_vocabulary_fiber 900 1 witness_sparse_orphan_state.
Proof.
  eapply ReachableStep.
  - eapply ReachableStep.
    + eapply ReachableStep.
      * eapply ReachableStep.
        { eapply ReachableStep.
          - apply ReachableInitial.
          - eapply TransitionClaimAllocation
              with (atom := collision_atom_left) (id := symbol_zero).
            exact witness_reserve_zero. }
        { eapply TransitionMaterializeReservation
            with (atom := collision_atom_left) (id := symbol_zero).
          exact witness_materialize_zero. }
      * eapply TransitionOrphanClaim
          with (atom := collision_atom_left) (id := symbol_zero).
        exact witness_orphan_materialized_zero.
    + eapply TransitionClaimAllocation
        with (atom := collision_atom_right) (id := symbol_two).
      exact witness_reserve_two_after_orphan.
  - eapply TransitionOrphanReservation
      with (atom := collision_atom_right) (id := symbol_two).
    exact witness_orphan_unmaterialized_two.
Qed.

Theorem VWENC_166_EVERY_TRANSITION_FAMILY_HAS_A_CONCRETE_WITNESS :
  InterningTransition witness_initial_state witness_live_zero /\
  InterningTransition witness_initial_state witness_reserved_zero /\
  InterningTransition witness_reserved_zero witness_materialized_zero /\
  InterningTransition witness_materialized_zero witness_live_zero /\
  InterningTransition witness_materialized_zero
    witness_materialized_orphan_zero /\
  InterningTransition witness_reserved_two_after_orphan
    witness_sparse_orphan_state /\
  InterningTransition witness_live_zero witness_tombstoned_zero /\
  InterningTransition witness_live_zero witness_live_sequence_zero /\
  InterningTransition witness_live_zero witness_live_term_enabled /\
  InterningTransition witness_live_term_enabled witness_live_zero /\
  InterningTransition witness_live_term_enabled witness_live_term_entry.
Proof.
  repeat split.
  - eapply TransitionFreshPublication
      with (atom := collision_atom_left) (id := symbol_zero).
    exact witness_fresh_publish_zero.
  - eapply TransitionClaimAllocation
      with (atom := collision_atom_left) (id := symbol_zero).
    exact witness_reserve_zero.
  - eapply TransitionMaterializeReservation
      with (atom := collision_atom_left) (id := symbol_zero).
    exact witness_materialize_zero.
  - eapply TransitionPublishClaim
      with (atom := collision_atom_left) (id := symbol_zero).
    exact witness_publish_zero.
  - eapply TransitionOrphanClaim
      with (atom := collision_atom_left) (id := symbol_zero).
    exact witness_orphan_materialized_zero.
  - eapply TransitionOrphanReservation
      with (atom := collision_atom_right) (id := symbol_two).
    exact witness_orphan_unmaterialized_two.
  - eapply TransitionTombstonePublished
      with (atom := collision_atom_left) (id := symbol_zero).
    exact witness_tombstone_zero.
  - eapply TransitionAddDependentSequence with (sequence := [symbol_zero]).
    exact witness_add_live_sequence.
  - apply TransitionEnableTermDictionary. exact witness_enable_term_dictionary.
  - apply TransitionDisableEmptyTermDictionary.
    exact witness_disable_empty_term_dictionary.
  - eapply TransitionInsertTermEntry
      with (sequence := [symbol_zero]) (term_id := term_zero).
    exact witness_insert_term_entry.
Qed.

Theorem VWENC_167_EVERY_ALLOCATION_STATUS_HAS_A_CONCRETE_WITNESS :
  allocation_has_status witness_reserved_zero
    collision_atom_left symbol_zero AllocationReserved /\
  allocation_has_status witness_materialized_zero
    collision_atom_left symbol_zero AllocationMaterializedClaimed /\
  allocation_has_status witness_live_zero
    collision_atom_left symbol_zero AllocationPublished /\
  allocation_has_status witness_tombstoned_zero
    collision_atom_left symbol_zero AllocationTombstoned /\
  allocation_has_status witness_materialized_orphan_zero
    collision_atom_left symbol_zero AllocationMaterializedOrphaned /\
  allocation_has_status witness_sparse_orphan_state
    collision_atom_right symbol_two AllocationUnmaterializedOrphaned.
Proof.
  repeat split.
  - apply allocation_status_reserved_from_membership.
    simpl. now left.
  - apply allocation_status_materialized_claimed_from_membership.
    + simpl. tauto.
    + simpl. now left.
  - apply allocation_status_published_from_membership.
    + simpl. tauto.
    + simpl. tauto.
    + simpl. tauto.
    + simpl. tauto.
    + simpl. now left.
    + simpl. now left.
  - apply allocation_status_tombstoned_from_membership.
    + simpl. tauto.
    + simpl. tauto.
    + simpl. tauto.
    + simpl. tauto.
    + simpl. now left.
    + simpl. tauto.
  - apply allocation_status_materialized_orphan_from_membership.
    + simpl. tauto.
    + simpl. tauto.
    + simpl. now left.
  - apply allocation_status_unmaterialized_orphan_from_membership.
    + simpl. tauto.
    + simpl. tauto.
    + simpl. intros [Hequal | []].
      apply (f_equal snd) in Hequal. simpl in Hequal.
      now apply symbol_two_differs_from_symbol_zero.
    + simpl. now left.
Qed.

Theorem VWENC_108_SPARSE_FRONTIER_HAS_A_GAP_AND_BOTH_ORPHAN_CLASSES :
  exists state : WitnessInterningState,
    InterningStateWellFormed state /\
    symbol_id_value u32_carrier symbol_one <
      state_allocator_frontier
        canonical_uleb_profile u32_carrier u32_carrier state /\
    ~ In symbol_one (map snd (state_allocation_entries state)) /\
    allocation_has_status state
      collision_atom_left symbol_zero AllocationMaterializedOrphaned /\
    allocation_has_status state
      collision_atom_right symbol_two AllocationUnmaterializedOrphaned /\
    state_live_entries
      canonical_uleb_profile u32_carrier u32_carrier state = [] /\
    state_sequences
      canonical_uleb_profile u32_carrier u32_carrier state = [] /\
    state_term_entries
      canonical_uleb_profile u32_carrier u32_carrier state = [].
Proof.
  exists witness_sparse_orphan_state.
  split.
  - eapply VWENC_159_EVERY_REACHABLE_INTERNING_STATE_IS_WELL_FORMED.
    exact witness_sparse_state_is_reachable.
  - split.
    + unfold witness_sparse_orphan_state, symbol_one.
      simpl. lia.
    + split.
      * simpl. intros [Hequal | [Hequal | []]].
        { apply (f_equal (symbol_id_value u32_carrier)) in Hequal.
          discriminate. }
        { apply (f_equal (symbol_id_value u32_carrier)) in Hequal.
          discriminate. }
      * split.
        { apply allocation_status_materialized_orphan_from_membership.
          - simpl. tauto.
          - simpl. tauto.
          - simpl. now left. }
        { split.
          - apply allocation_status_unmaterialized_orphan_from_membership.
            + simpl. tauto.
            + simpl. tauto.
            + simpl. intros [Hequal | []].
              apply (f_equal snd) in Hequal. simpl in Hequal.
              now apply symbol_two_differs_from_symbol_zero.
            + simpl. now left.
          - split; [reflexivity |].
            split; reflexivity. }
Qed.

Theorem VWENC_130_FRESH_INSERT_PRESERVES_EXISTING_ATOM_LOOKUPS :
  forall (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (entries : list (VocabularyEntry P I))
    new_atom existing_atom new_id existing_id,
    existing_atom <> new_atom ->
    lookup_atom entries existing_atom = Some existing_id ->
    lookup_atom ((new_atom, new_id) :: entries) existing_atom =
      Some existing_id.
Proof.
  intros P I entries new_atom existing_atom new_id existing_id
    Hdifferent Hlookup.
  unfold lookup_atom. simpl.
  destruct (canonical_atom_eq_dec P existing_atom new_atom);
    [contradiction | exact Hlookup].
Qed.

Theorem VWENC_131_FRESH_INSERT_PRESERVES_EXISTING_REVERSE_LOOKUPS :
  forall (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (entries : list (VocabularyEntry P I))
    new_atom existing_atom new_id existing_id,
    existing_id <> new_id ->
    lookup_symbol entries existing_id = Some existing_atom ->
    lookup_symbol ((new_atom, new_id) :: entries) existing_id =
      Some existing_atom.
Proof.
  intros P I entries new_atom existing_atom new_id existing_id
    Hdifferent Hlookup.
  unfold lookup_symbol, reverse_vocabulary_entries in *. simpl.
  destruct (symbol_id_eq_dec I existing_id new_id);
    [contradiction | exact Hlookup].
Qed.

Theorem VWENC_133_EVERY_LIVE_ID_HAS_EXACT_NONEMPTY_BOUNDED_CANONICAL_SPAN :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state : InterningState P I T) atom id,
    InterningStateWellFormed state ->
    In (atom, id) (state_live_entries P I T state) ->
    packed_entry_exact (state_packed_storage P I T state) atom id.
Proof.
  intros P I T state atom id Hwell Hlive.
  destruct Hwell as
    [_ _ Hlive_history _ Hpacked].
  destruct Hpacked as [_ [_ [_ [Hexact _]]]].
  apply Hexact.
  unfold state_materialized_entries.
  apply in_or_app. left.
  now apply Hlive_history.
Qed.

(** ** Certified vocabulary snapshots and sequence descriptors *)

Record VocabularySnapshot
    (P : CertifiedAtomProfile)
    (I : FixedWidthCarrierProfile)
    (fiber : VocabularyFiber P I) : Type :=
  mkVocabularySnapshot {
    vocabulary_snapshot_live_entries : list (VocabularyEntry P I);
    vocabulary_snapshot_available_frontier : nat;
    vocabulary_snapshot_packed_storage : PackedAtomStorage I;
    vocabulary_snapshot_live_bijection :
      vocabulary_relation_well_formed vocabulary_snapshot_live_entries;
    vocabulary_snapshot_frontier_representable :
      vocabulary_snapshot_available_frontier <= carrier_capacity I;
    vocabulary_snapshot_live_ids_below_frontier :
      Forall
        (fun entry =>
          symbol_id_value I (snd entry) <
            vocabulary_snapshot_available_frontier)
        vocabulary_snapshot_live_entries;
    vocabulary_snapshot_live_metadata_exact :
      forall atom id,
        In (atom, id) vocabulary_snapshot_live_entries ->
        packed_entry_exact vocabulary_snapshot_packed_storage atom id
  }.

Definition capture_vocabulary_snapshot
    {P : CertifiedAtomProfile}
    {I T : FixedWidthCarrierProfile}
    (state : InterningState P I T)
    (Hwell : InterningStateWellFormed state)
    : VocabularySnapshot P I (state_fiber P I T state).
Proof.
  pose proof Hwell as Hwhole.
  destruct Hwell as
    [Hlive_bijection _ Hlive_history _ _ Halloc_below Hfrontier].
  refine (mkVocabularySnapshot
    P I (state_fiber P I T state)
    (state_live_entries P I T state)
    (state_allocator_frontier P I T state)
    (state_packed_storage P I T state)
    Hlive_bijection Hfrontier _ _).
  - apply Forall_forall. intros entry Hlive.
    apply Forall_forall with (x := entry) in Halloc_below.
    + exact Halloc_below.
    + unfold state_allocation_entries.
      apply in_or_app. left.
      destruct entry as [atom id].
      now apply Hlive_history.
  - intros atom id Hlive.
    now apply VWENC_133_EVERY_LIVE_ID_HAS_EXACT_NONEMPTY_BOUNDED_CANONICAL_SPAN.
Defined.

Theorem VWENC_181_CAPTURED_VOCABULARY_SNAPSHOT_IS_ONE_EXACT_STATE_FIBER :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state : InterningState P I T)
    (Hwell : InterningStateWellFormed state),
    vocabulary_fiber_identity (state_fiber P I T state) =
      vocabulary_fiber_identity (state_fiber P I T state) /\
    vocabulary_snapshot_live_entries
      P I (state_fiber P I T state)
      (capture_vocabulary_snapshot state Hwell) =
        state_live_entries P I T state /\
    vocabulary_snapshot_available_frontier
      P I (state_fiber P I T state)
      (capture_vocabulary_snapshot state Hwell) =
        state_allocator_frontier P I T state /\
    vocabulary_snapshot_packed_storage
      P I (state_fiber P I T state)
      (capture_vocabulary_snapshot state Hwell) =
        state_packed_storage P I T state.
Proof.
  intros P I T state Hwell.
  destruct Hwell. repeat split.
Qed.

Record SequenceDescriptor
    (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile) : Type :=
  mkSequenceDescriptor {
    descriptor_fiber : VocabularyFiber P I;
    descriptor_required_frontier : nat;
    descriptor_ids : list (SymbolId I)
  }.

Definition descriptor_accepts_snapshot
    {P : CertifiedAtomProfile} {I : FixedWidthCarrierProfile}
    (expected_fiber : VocabularyFiber P I)
    (live : list (VocabularyEntry P I))
    (available_frontier : nat)
    (descriptor : SequenceDescriptor P I) : Prop :=
  descriptor_fiber P I descriptor = expected_fiber /\
  descriptor_required_frontier P I descriptor <= available_frontier /\
  Forall
    (fun id =>
      symbol_id_value I id <
        descriptor_required_frontier P I descriptor /\
      live_symbol live id)
    (descriptor_ids P I descriptor).

Definition descriptor_accepts_vocabulary_snapshot
    {P : CertifiedAtomProfile} {I : FixedWidthCarrierProfile}
    {fiber : VocabularyFiber P I}
    (snapshot : VocabularySnapshot P I fiber)
    (descriptor : SequenceDescriptor P I) : Prop :=
  descriptor_accepts_snapshot
    fiber
    (vocabulary_snapshot_live_entries P I fiber snapshot)
    (vocabulary_snapshot_available_frontier P I fiber snapshot)
    descriptor.

Definition encode_symbol_sequence
    (I : FixedWidthCarrierProfile) (ids : list (SymbolId I))
    : list PhysicalByte :=
  flat_map (encode_symbol_id I) ids.

(** ** Fiber-bound fixed-width ID sequence views *)

Record IdSequenceBacking
    (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile) : Type :=
  mkIdSequenceBacking {
    backing_identity : nat;
    backing_fiber : VocabularyFiber P I;
    backing_snapshot : VocabularySnapshot P I backing_fiber;
    backing_descriptor : SequenceDescriptor P I;
    backing_descriptor_accepted :
      descriptor_accepts_vocabulary_snapshot
        backing_snapshot backing_descriptor;
    backing_bytes : list PhysicalByte;
    backing_bytes_encode_exact_descriptor :
      backing_bytes =
        encode_symbol_sequence I (descriptor_ids P I backing_descriptor);
    backing_bytes_are_valid : Forall valid_byte backing_bytes;
    backing_bytes_have_exact_descriptor_length :
      List.length backing_bytes =
        List.length (descriptor_ids P I backing_descriptor) *
          carrier_width_bytes I
  }.

Definition valid_id_sequence_backing
    {P : CertifiedAtomProfile} {I : FixedWidthCarrierProfile}
    (backing : IdSequenceBacking P I) : Prop :=
  Forall valid_byte (backing_bytes P I backing) /\
  List.length (backing_bytes P I backing) =
    List.length
      (descriptor_ids P I (backing_descriptor P I backing)) *
      carrier_width_bytes I /\
  descriptor_accepts_vocabulary_snapshot
    (backing_snapshot P I backing)
    (backing_descriptor P I backing) /\
  backing_bytes P I backing =
    encode_symbol_sequence I
      (descriptor_ids P I (backing_descriptor P I backing)).

Theorem VWENC_182_EVERY_ID_SEQUENCE_BACKING_IS_CERTIFIED_BY_ONE_EXACT_SNAPSHOT :
  forall (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (backing : IdSequenceBacking P I),
    valid_id_sequence_backing backing.
Proof.
  intros P I backing.
  split; [exact (backing_bytes_are_valid P I backing) |].
  split; [exact (backing_bytes_have_exact_descriptor_length P I backing) |].
  split.
  - exact (backing_descriptor_accepted P I backing).
  - exact (backing_bytes_encode_exact_descriptor P I backing).
Qed.

Record IdSequenceView
    (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile) : Type :=
  mkIdSequenceView {
    view_backing : IdSequenceBacking P I;
    view_start : nat;
    view_count : nat
  }.

Definition valid_id_sequence_view
    {P : CertifiedAtomProfile} {I : FixedWidthCarrierProfile}
    (view : IdSequenceView P I) : Prop :=
  valid_id_sequence_backing (view_backing P I view) /\
  view_start P I view + view_count P I view <=
    List.length
      (descriptor_ids P I
        (backing_descriptor P I (view_backing P I view))).

Definition id_sequence_view_byte_offset
    {P : CertifiedAtomProfile} {I : FixedWidthCarrierProfile}
    (view : IdSequenceView P I) (index : nat) : nat :=
  (view_start P I view + index) * carrier_width_bytes I.

Definition id_sequence_view_byte_window
    {P : CertifiedAtomProfile} {I : FixedWidthCarrierProfile}
    (view : IdSequenceView P I) (index : nat)
    : list PhysicalByte :=
  firstn
    (carrier_width_bytes I)
    (skipn
      (id_sequence_view_byte_offset view index)
      (backing_bytes P I (view_backing P I view))).

Definition id_sequence_view_bytes
    {P : CertifiedAtomProfile} {I : FixedWidthCarrierProfile}
    (view : IdSequenceView P I) (index : nat)
    : option (list PhysicalByte) :=
  if index <? view_count P I view
  then Some (id_sequence_view_byte_window view index)
  else None.

Definition id_sequence_view_index
    {P : CertifiedAtomProfile} {I : FixedWidthCarrierProfile}
    (view : IdSequenceView P I) (index : nat)
    : option (FiberBoundSymbolId P I) :=
  match id_sequence_view_bytes view index with
  | Some bytes =>
      match decode_symbol_id I bytes with
      | Some id =>
          Some
            (mkFiberBoundSymbolId P I
              (backing_fiber P I (view_backing P I view)) id)
      | None => None
      end
  | None => None
  end.

Definition id_sequence_subview
    {P : CertifiedAtomProfile} {I : FixedWidthCarrierProfile}
    (view : IdSequenceView P I) (offset count : nat)
    : option (IdSequenceView P I) :=
  if offset + count <=? view_count P I view
  then Some
    (mkIdSequenceView P I
      (view_backing P I view)
      (view_start P I view + offset)
      count)
  else None.

Lemma Forall_firstn_preserved :
  forall (A : Type) (predicate : A -> Prop) count values,
    Forall predicate values ->
    Forall predicate (firstn count values).
Proof.
  intros A predicate count.
  induction count as [| count IH]; intros values Hforall.
  - simpl. constructor.
  - destruct values as [| value rest].
    + simpl. constructor.
    + inversion Hforall; subst. simpl. constructor; [assumption |].
      now apply IH.
Qed.

Lemma Forall_skipn_preserved :
  forall (A : Type) (predicate : A -> Prop) count values,
    Forall predicate values ->
    Forall predicate (skipn count values).
Proof.
  intros A predicate count.
  induction count as [| count IH]; intros values Hforall.
  - exact Hforall.
  - destruct values as [| value rest].
    + simpl. constructor.
    + inversion Hforall; subst. simpl. now apply IH.
Qed.

Lemma decode_fixed_little_endian_bounded_by_width :
  forall bytes,
    Forall valid_byte bytes ->
    decode_fixed_little_endian bytes < 256 ^ List.length bytes.
Proof.
  induction bytes as [| byte rest IH]; intros Hvalid.
  - simpl. lia.
  - inversion Hvalid as [| current tail Hbyte Hrest]; subst.
    specialize (IH Hrest).
    cbn [decode_fixed_little_endian List.length].
    rewrite Nat.pow_succ_r by lia.
    unfold valid_byte in Hbyte.
    nia.
Qed.

Lemma decode_symbol_id_accepts_every_exact_width_byte_window :
  forall (I : FixedWidthCarrierProfile) bytes,
    List.length bytes = carrier_width_bytes I ->
    Forall valid_byte bytes ->
    exists id, decode_symbol_id I bytes = Some id.
Proof.
  intros I bytes Hlength Hvalid.
  assert (Hbounded :
    decode_fixed_little_endian bytes < carrier_capacity I).
  { unfold carrier_capacity. rewrite <- Hlength.
    now apply decode_fixed_little_endian_bounded_by_width. }
  unfold decode_symbol_id.
  rewrite Hlength.
  destruct (Nat.eq_dec (carrier_width_bytes I) (carrier_width_bytes I))
    as [_ | Himpossible]; [| contradiction].
  assert (Hvalidb : all_valid_bytesb bytes = true).
  { now apply (proj2 (all_valid_bytesb_reflects_validity bytes)). }
  rewrite Hvalidb. unfold symbol_id_of_nat.
  destruct (lt_dec (decode_fixed_little_endian bytes)
    (carrier_capacity I)) as [Hfits | Hoverflow].
  - eexists. reflexivity.
  - contradiction.
Qed.

Lemma firstn_exact_left_append :
  forall (A : Type) (left right : list A),
    firstn (List.length left) (left ++ right) = left.
Proof.
  intros A left.
  induction left as [| value tail IH]; intros right.
  - reflexivity.
  - simpl. now rewrite IH.
Qed.

Lemma skipn_exact_left_append :
  forall (A : Type) (left right : list A) count,
    skipn (List.length left + count) (left ++ right) =
      skipn count right.
Proof.
  intros A left.
  induction left as [| value tail IH]; intros right count.
  - reflexivity.
  - simpl. now rewrite IH.
Qed.

Lemma encoded_symbol_sequence_window_at :
  forall (I : FixedWidthCarrierProfile) ids index id,
    nth_error ids index = Some id ->
    firstn
      (carrier_width_bytes I)
      (skipn
        (index * carrier_width_bytes I)
        (encode_symbol_sequence I ids)) =
      encode_symbol_id I id.
Proof.
  intros I ids.
  induction ids as [| head tail IH]; intros index id Hnth.
  - destruct index; discriminate.
  - destruct index as [| index].
    + simpl in Hnth. inversion Hnth. subst id.
      change
        (firstn (carrier_width_bytes I)
          (encode_symbol_id I head ++ encode_symbol_sequence I tail) =
         encode_symbol_id I head).
      rewrite <- (proj1 (symbol_id_fixed_width_encoding_roundtrips I head)).
      apply firstn_exact_left_append.
    + simpl in Hnth.
      change
        (firstn (carrier_width_bytes I)
          (skipn (S index * carrier_width_bytes I)
            (encode_symbol_id I head ++ encode_symbol_sequence I tail)) =
         encode_symbol_id I id).
      assert (Hhead_length :
        List.length (encode_symbol_id I head) = carrier_width_bytes I).
      { apply symbol_id_fixed_width_encoding_roundtrips. }
      replace (S index * carrier_width_bytes I) with
        (List.length (encode_symbol_id I head) +
          index * carrier_width_bytes I) by
        (rewrite Hhead_length; lia).
      rewrite skipn_exact_left_append.
      now apply IH.
Qed.

Lemma valid_id_sequence_view_window_is_descriptor_encoding :
  forall (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (view : IdSequenceView P I) index,
    valid_id_sequence_view view ->
    index < view_count P I view ->
    exists id,
      nth_error
        (descriptor_ids P I
          (backing_descriptor P I (view_backing P I view)))
        (view_start P I view + index) = Some id /\
      id_sequence_view_byte_window view index = encode_symbol_id I id.
Proof.
  intros P I view index [Hbacking Hrange] Hindex.
  destruct Hbacking as [_ [_ [_ Hbytes_exact]]].
  assert (Hposition :
    view_start P I view + index <
      List.length
        (descriptor_ids P I
          (backing_descriptor P I (view_backing P I view)))) by lia.
  destruct (nth_error
    (descriptor_ids P I
      (backing_descriptor P I (view_backing P I view)))
    (view_start P I view + index)) as [id |] eqn:Hnth.
  - exists id. split; [reflexivity |].
    unfold id_sequence_view_byte_window,
      id_sequence_view_byte_offset.
    rewrite Hbytes_exact.
    now apply encoded_symbol_sequence_window_at.
  - apply nth_error_None in Hnth. lia.
Qed.

Lemma valid_id_sequence_view_has_exact_byte_window :
  forall (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (view : IdSequenceView P I) index,
    valid_id_sequence_view view ->
    index < view_count P I view ->
    List.length (id_sequence_view_byte_window view index) =
      carrier_width_bytes I /\
    Forall valid_byte (id_sequence_view_byte_window view index).
Proof.
  intros P I view index
    [[Hbytes [Hbacking_length [_ _]]] Hrange] Hindex.
  pose proof (carrier_width_positive I) as Hwidth.
  assert (Hwindow_end :
    id_sequence_view_byte_offset view index + carrier_width_bytes I <=
      List.length (backing_bytes P I (view_backing P I view))).
  { unfold id_sequence_view_byte_offset. nia. }
  unfold id_sequence_view_byte_window.
  split.
  - rewrite length_firstn, length_skipn.
    rewrite Nat.min_l; lia.
  - apply Forall_firstn_preserved.
    now apply Forall_skipn_preserved.
Qed.

Theorem VWENC_116_VALID_ID_VIEW_INDEXES_BOUND_BACKING_DIRECTLY :
  forall (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (view : IdSequenceView P I) index,
    valid_id_sequence_view view ->
    index < view_count P I view ->
    exists bytes id,
      nth_error
        (descriptor_ids P I
          (backing_descriptor P I (view_backing P I view)))
        (view_start P I view + index) = Some id /\
      id_sequence_view_bytes view index = Some bytes /\
      bytes = id_sequence_view_byte_window view index /\
      bytes = encode_symbol_id I id /\
      List.length bytes = carrier_width_bytes I /\
      Forall valid_byte bytes /\
      decode_symbol_id I bytes = Some id /\
      live_symbol
        (vocabulary_snapshot_live_entries
          P I
          (backing_fiber P I (view_backing P I view))
          (backing_snapshot P I (view_backing P I view))) id /\
      id_sequence_view_index view index =
        Some
          (mkFiberBoundSymbolId P I
            (backing_fiber P I (view_backing P I view)) id).
Proof.
  intros P I view index Hvalid Hindex.
  destruct (valid_id_sequence_view_window_is_descriptor_encoding
    P I view index Hvalid Hindex) as [id [Hnth Hencoded]].
  destruct (valid_id_sequence_view_has_exact_byte_window
    P I view index Hvalid Hindex) as [Hlength Hbytes].
  assert (Hdecode :
    decode_symbol_id I (id_sequence_view_byte_window view index) = Some id).
  { rewrite Hencoded.
    apply symbol_id_fixed_width_encoding_roundtrips. }
  assert (Hlive :
    live_symbol
      (vocabulary_snapshot_live_entries
        P I
        (backing_fiber P I (view_backing P I view))
        (backing_snapshot P I (view_backing P I view))) id).
  { destruct Hvalid as [[_ [_ [Haccepted _]]] _].
    unfold descriptor_accepts_vocabulary_snapshot,
      descriptor_accepts_snapshot in Haccepted.
    destruct Haccepted as [_ [_ Hids]].
    apply Forall_forall with (x := id) in Hids.
    - exact (proj2 Hids).
    - now apply nth_error_In in Hnth. }
  exists (id_sequence_view_byte_window view index), id.
  split; [exact Hnth |]. split.
  - unfold id_sequence_view_bytes.
    apply Nat.ltb_lt in Hindex. now rewrite Hindex.
  - split; [reflexivity |].
    split; [exact Hencoded |].
    split; [exact Hlength |].
    split; [exact Hbytes |].
    split; [exact Hdecode |].
    split; [exact Hlive |].
    unfold id_sequence_view_index, id_sequence_view_bytes.
    apply Nat.ltb_lt in Hindex. now rewrite Hindex, Hdecode.
Qed.

Theorem VWENC_117_SUBVIEW_PRESERVES_BACKING_FIBER_AND_VALID_RANGE :
  forall (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (view subview : IdSequenceView P I) offset count,
    valid_id_sequence_view view ->
    id_sequence_subview view offset count = Some subview ->
    view_backing P I subview = view_backing P I view /\
    backing_fiber P I (view_backing P I subview) =
      backing_fiber P I (view_backing P I view) /\
    valid_id_sequence_view subview.
Proof.
  intros P I view subview offset count Hvalid Hsubview.
  unfold id_sequence_subview in Hsubview.
  destruct (offset + count <=? view_count P I view)
    eqn:Hrange; [| discriminate].
  apply Nat.leb_le in Hrange.
  inversion Hsubview. subst subview. clear Hsubview.
  split; [reflexivity |].
  split; [reflexivity |].
  destruct Hvalid as [Hbacking Hvalid].
  split; [exact Hbacking |].
  simpl in *. pose proof (carrier_width_positive I). nia.
Qed.

Theorem VWENC_134_ID_SEQUENCE_VIEW_REJECTS_OUT_OF_RANGE_INDEX :
  forall (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (view : IdSequenceView P I) index,
    view_count P I view <= index ->
    id_sequence_view_index view index = None.
Proof.
  intros P I view index Hrange.
  unfold id_sequence_view_index, id_sequence_view_bytes.
  destruct (index <? view_count P I view) eqn:Hless.
  - apply Nat.ltb_lt in Hless. lia.
  - reflexivity.
Qed.

Theorem VWENC_135_ID_VIEW_ELEMENTS_HAVE_EXACT_CARRIER_STRIDE :
  forall (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (view : IdSequenceView P I) index bound_id,
    valid_id_sequence_view view ->
    id_sequence_view_index view index = Some bound_id ->
    index < view_count P I view /\
    exists bytes id,
      bound_id =
        mkFiberBoundSymbolId P I
          (backing_fiber P I (view_backing P I view)) id /\
      nth_error
        (descriptor_ids P I
          (backing_descriptor P I (view_backing P I view)))
        (view_start P I view + index) = Some id /\
      id_sequence_view_bytes view index = Some bytes /\
      bytes = id_sequence_view_byte_window view index /\
      bytes = encode_symbol_id I id /\
      live_symbol
        (vocabulary_snapshot_live_entries
          P I
          (backing_fiber P I (view_backing P I view))
          (backing_snapshot P I (view_backing P I view))) id /\
      id_sequence_view_byte_offset view index =
        (view_start P I view + index) * carrier_width_bytes I /\
      List.length bytes = carrier_width_bytes I /\
      Forall valid_byte bytes /\
      decode_symbol_id I bytes = Some id /\
      List.length (encode_symbol_id I id) = carrier_width_bytes I /\
      decode_symbol_id I (encode_symbol_id I id) = Some id.
Proof.
  intros P I view index bound_id Hvalid Hindex.
  assert (Hwithin : index < view_count P I view).
  { destruct (index <? view_count P I view) eqn:Hless.
    - now apply Nat.ltb_lt.
    - exfalso. apply Nat.ltb_ge in Hless.
      now rewrite (VWENC_134_ID_SEQUENCE_VIEW_REJECTS_OUT_OF_RANGE_INDEX
        P I view index Hless) in Hindex. }
  destruct (VWENC_116_VALID_ID_VIEW_INDEXES_BOUND_BACKING_DIRECTLY
    P I view index Hvalid Hwithin)
    as [bytes [id
      [Hnth [Hwindow [Hexact [Hencoded [Hlength [Hbytes
        [Hdecode [Hlive Hbound]]]]]]]]]].
  rewrite Hindex in Hbound. inversion Hbound. subst bound_id.
  split; [exact Hwithin |]. exists bytes, id.
  split; [reflexivity |].
  split; [exact Hnth |].
  split; [exact Hwindow |].
  split; [exact Hexact |].
  split; [exact Hencoded |].
  split; [exact Hlive |].
  split; [reflexivity |].
  split; [exact Hlength |].
  split; [exact Hbytes |].
  split; [exact Hdecode |].
  apply symbol_id_fixed_width_encoding_roundtrips.
Qed.

Theorem VWENC_187_ID_VIEW_RESULT_REJECTS_EVERY_DIFFERENT_FIBER :
  forall (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (view : IdSequenceView P I) index bound_id expected,
    valid_id_sequence_view view ->
    id_sequence_view_index view index = Some bound_id ->
    expected <> backing_fiber P I (view_backing P I view) ->
    interpret_symbol_id expected bound_id = None.
Proof.
  intros P I view index bound_id expected Hvalid Hindex Hdifferent.
  destruct (VWENC_135_ID_VIEW_ELEMENTS_HAVE_EXACT_CARRIER_STRIDE
    P I view index bound_id Hvalid Hindex)
    as [_ [bytes [id [Hbound _]]]].
  subst bound_id.
  now apply VWENC_112_CROSS_FIBER_ID_INTERPRETATION_IS_REJECTED.
Qed.

(** ** Two-level term IDs and exact vocabulary binding *)

Fixpoint interpret_bound_symbol_sequence
    {P : CertifiedAtomProfile}
    {I : FixedWidthCarrierProfile}
    (expected : VocabularyFiber P I)
    (sequence : list (FiberBoundSymbolId P I))
    : option (list (SymbolId I)) :=
  match sequence with
  | [] => Some []
  | bound :: tail =>
      match interpret_symbol_id expected bound,
            interpret_bound_symbol_sequence expected tail with
      | Some id, Some ids => Some (id :: ids)
      | _, _ => None
      end
  end.

Definition resolve_atom_then_term
    {P : CertifiedAtomProfile}
    {I T : FixedWidthCarrierProfile}
    (state : InterningState P I T)
    (atom : CanonicalAtom P)
    (tail : list (FiberBoundSymbolId P I))
    : option
        (FiberBoundTermId P I T (state_fiber P I T state)) :=
  match lookup_atom (state_live_entries P I T state) atom,
        interpret_bound_symbol_sequence (state_fiber P I T state) tail with
  | Some id, Some ids => lookup_state_term_sequence state (id :: ids)
  | _, _ => None
  end.

Theorem VWENC_118_ATOM_ID_AND_TERM_ID_LOOKUP_LAYERS_ARE_EXPLICIT :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state : InterningState P I T)
    atom id tail interpreted_tail term_id,
    lookup_atom (state_live_entries P I T state) atom = Some id ->
    interpret_bound_symbol_sequence
      (state_fiber P I T state) tail = Some interpreted_tail ->
    state_term_dictionary_enabled P I T state = true ->
    lookup_term_sequence
      (state_term_entries P I T state)
      (id :: interpreted_tail) = Some term_id ->
    resolve_atom_then_term state atom tail =
      Some
        (mkFiberBoundTermId
          P I T (state_fiber P I T state)
          (state_term_fiber P I T state) term_id).
Proof.
  intros P I T state atom id tail interpreted_tail term_id
    Hatom Htail Henabled Hterm.
  unfold resolve_atom_then_term.
  rewrite Hatom, Htail.
  unfold lookup_state_term_sequence. now rewrite Henabled, Hterm.
Qed.

Theorem VWENC_183_TWO_LEVEL_RESOLUTION_REJECTS_A_FOREIGN_FIBER_TAIL :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state : InterningState P I T)
    (actual : VocabularyFiber P I)
    (atom : CanonicalAtom P) (id : SymbolId I) tail,
    state_fiber P I T state <> actual ->
    interpret_bound_symbol_sequence (state_fiber P I T state)
      (mkFiberBoundSymbolId P I actual id :: tail) = None /\
    resolve_atom_then_term state atom
      (mkFiberBoundSymbolId P I actual id :: tail) = None.
Proof.
  intros P I T state actual atom id tail Hdifferent.
  assert (Hreject :
    interpret_bound_symbol_sequence (state_fiber P I T state)
      (mkFiberBoundSymbolId P I actual id :: tail) = None).
  { simpl. now rewrite
    (VWENC_112_CROSS_FIBER_ID_INTERPRETATION_IS_REJECTED
      P I (state_fiber P I T state) actual id Hdifferent). }
  split; [exact Hreject |].
  unfold resolve_atom_then_term. rewrite Hreject.
  now destruct (lookup_atom (state_live_entries P I T state) atom).
Qed.

Theorem VWENC_119_OPTIONAL_TERM_DICTIONARY_SEQUENCES_USE_LIVE_VOCABULARY_IDS :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state : InterningState P I T) sequence term_id,
    InterningStateWellFormed state ->
    In (sequence, term_id) (state_term_entries P I T state) ->
    sequence_vocabulary_bound
      (state_live_entries P I T state)
      (state_allocator_frontier P I T state)
      sequence.
Proof.
  intros P I T state sequence term_id Hwell Hin.
  destruct Hwell as
    [_ _ _ _ _ _ _ _ _ Hterm_bound _].
  apply Forall_forall with (x := (sequence, term_id))
    in Hterm_bound; [exact Hterm_bound | exact Hin].
Qed.

Lemma orphan_id_has_no_live_binding :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state : InterningState P I T) id,
    InterningStateWellFormed state ->
    In id (state_orphan_ids state) ->
    ~ live_symbol (state_live_entries P I T state) id.
Proof.
  intros P I T state id Hwell Horphan.
  destruct Hwell as
    [_ _ Hlive_history Hallocation_unique].
  intros [atom Hlive].
  assert (Hever : In id (map snd (state_ever_entries P I T state))).
  { apply in_map_iff. exists (atom, id). split; [reflexivity |].
    now apply Hlive_history. }
  unfold state_allocation_entries in Hallocation_unique.
  rewrite !map_app in Hallocation_unique.
  eapply NoDup_app_disjoint_right;
    [exact Hallocation_unique | exact Hever |].
  unfold state_orphan_ids in Horphan.
  rewrite map_app in Horphan.
  apply in_or_app. right.
  apply in_or_app. right.
  exact Horphan.
Qed.

Theorem VWENC_120_ORPHAN_IDS_HAVE_NO_LIVE_OR_SEQUENCE_BINDING :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state : InterningState P I T) id sequence,
    InterningStateWellFormed state ->
    In id (state_orphan_ids state) ->
    In sequence (state_sequences P I T state) ->
    ~ live_symbol (state_live_entries P I T state) id /\
    ~ In id sequence.
Proof.
  intros P I T state id sequence Hwell Horphan Hsequence.
  pose proof (orphan_id_has_no_live_binding
    P I T state id Hwell Horphan) as Hnot_live.
  destruct Hwell as [_ _ _ _ _ _ _ Hsequences].
  split; [exact Hnot_live |].
  intros Hin.
  apply Forall_forall with (x := sequence) in Hsequences;
    [| exact Hsequence].
  apply Forall_forall with (x := id) in Hsequences; [| exact Hin].
  destruct Hsequences as [_ Hlive]. contradiction.
Qed.

Theorem VWENC_165_ORPHAN_IDS_HAVE_NO_TERM_SEQUENCE_BINDING :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state : InterningState P I T) id sequence term_id,
    InterningStateWellFormed state ->
    In id (state_orphan_ids state) ->
    In (sequence, term_id) (state_term_entries P I T state) ->
    ~ In id sequence.
Proof.
  intros P I T state id sequence term_id Hwell Horphan Hterm Hin.
  pose proof (orphan_id_has_no_live_binding
    P I T state id Hwell Horphan) as Hnot_live.
  pose proof Hwell as Hwell_for_terms.
  destruct Hwell_for_terms as [_ _ _ _ _ _ _ _ _ Hterm_bound].
  apply Forall_forall with (x := (sequence, term_id)) in Hterm_bound;
    [| exact Hterm].
  apply Forall_forall with (x := id) in Hterm_bound; [| exact Hin].
  destruct Hterm_bound as [_ Hlive]. contradiction.
Qed.

Inductive AnyLocalId
    (I T : FixedWidthCarrierProfile) : Type :=
| AnySymbolId : SymbolId I -> AnyLocalId I T
| AnyTermId : TermId T -> AnyLocalId I T.

Theorem VWENC_136_SYMBOL_AND_TERM_IDS_ARE_NOMINALLY_DISJOINT :
  forall (I T : FixedWidthCarrierProfile)
    (symbol : SymbolId I) (term : TermId T),
    AnySymbolId I T symbol <> AnyTermId I T term.
Proof. discriminate. Qed.

Theorem VWENC_137_TERM_ID_DICTIONARY_IS_A_SECOND_EXACT_BIJECTION :
  forall (I T : FixedWidthCarrierProfile)
    (entries : list (TermEntry I T)) sequence term_id,
    term_relation_well_formed entries ->
    (lookup_term_sequence entries sequence = Some term_id <->
     lookup_term_id entries term_id = Some sequence).
Proof.
  intros I T entries sequence term_id [Hsequence_unique Hterm_unique].
  split; intros Hlookup.
  - unfold lookup_term_sequence in Hlookup.
    apply assoc_lookup_sound in Hlookup.
    unfold lookup_term_id. apply assoc_lookup_complete_unique.
    + rewrite reverse_term_keys_are_term_ids. exact Hterm_unique.
    + apply reverse_term_membership. exact Hlookup.
  - unfold lookup_term_id in Hlookup.
    apply assoc_lookup_sound in Hlookup.
    apply reverse_term_membership in Hlookup.
    unfold lookup_term_sequence.
    now apply assoc_lookup_complete_unique.
Qed.

(** ** Query-local overlay: fiber-bound, namespaced, and non-serializable *)

Record QueryOverlayNamespace
    (P : CertifiedAtomProfile)
    (I : FixedWidthCarrierProfile)
    (fiber : VocabularyFiber P I) : Type :=
  mkQueryOverlayNamespace {
    query_overlay_namespace_identity : nat
  }.

Definition query_overlay_namespace_full_identity
    {P : CertifiedAtomProfile}
    {I : FixedWidthCarrierProfile}
    {fiber : VocabularyFiber P I}
    (namespace : QueryOverlayNamespace P I fiber) :=
  (vocabulary_fiber_identity fiber,
   query_overlay_namespace_identity P I fiber namespace).

Definition query_overlay_namespace_eq_dec
    (P : CertifiedAtomProfile)
    (I : FixedWidthCarrierProfile)
    (fiber : VocabularyFiber P I)
    (left right : QueryOverlayNamespace P I fiber)
    : {left = right} + {left <> right}.
Proof.
  destruct left as [left_identity].
  destruct right as [right_identity].
  destruct (Nat.eq_dec left_identity right_identity)
    as [Hequal | Hdifferent].
  - subst right_identity. left. reflexivity.
  - right. intros Hequal. inversion Hequal. contradiction.
Defined.

Record QueryLocalId
    (P : CertifiedAtomProfile)
    (I : FixedWidthCarrierProfile)
    (fiber : VocabularyFiber P I) : Type :=
  mkQueryLocalId {
    query_local_namespace : QueryOverlayNamespace P I fiber;
    query_local_id_value : nat
  }.

Definition interpret_query_local_id
    {P : CertifiedAtomProfile}
    {I : FixedWidthCarrierProfile}
    {fiber : VocabularyFiber P I}
    (expected : QueryOverlayNamespace P I fiber)
    (id : QueryLocalId P I fiber) : option nat :=
  if query_overlay_namespace_eq_dec
      P I fiber expected (query_local_namespace P I fiber id)
  then Some (query_local_id_value P I fiber id)
  else None.

Theorem VWENC_172_CROSS_OVERLAY_QUERY_LOCAL_ID_IS_REJECTED :
  forall (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (fiber : VocabularyFiber P I)
    (expected actual : QueryOverlayNamespace P I fiber) value,
    expected <> actual ->
    interpret_query_local_id expected
      (mkQueryLocalId P I fiber actual value) = None.
Proof.
  intros P I fiber expected actual value Hdifferent.
  unfold interpret_query_local_id. simpl.
  destruct (query_overlay_namespace_eq_dec
    P I fiber expected actual) as [Hequal | _].
  - contradiction.
  - reflexivity.
Qed.

Definition QueryOverlayEntry
    (P : CertifiedAtomProfile)
    (I : FixedWidthCarrierProfile)
    (fiber : VocabularyFiber P I) :=
  (CanonicalAtom P * QueryLocalId P I fiber)%type.

Record QueryOverlay
    (P : CertifiedAtomProfile)
    (I : FixedWidthCarrierProfile)
    (fiber : VocabularyFiber P I) : Type :=
  mkQueryOverlay {
    query_overlay_namespace : QueryOverlayNamespace P I fiber;
    query_overlay_entries : list (QueryOverlayEntry P I fiber);
    query_overlay_next : nat
  }.

Definition PackedQueryOverlay
    (P : CertifiedAtomProfile)
    (I : FixedWidthCarrierProfile) : Type :=
  { fiber : VocabularyFiber P I & QueryOverlay P I fiber }.

Definition transport_query_overlay
    {P : CertifiedAtomProfile}
    {I : FixedWidthCarrierProfile}
    {actual expected : VocabularyFiber P I}
    (Hequal : actual = expected)
    (overlay : QueryOverlay P I actual)
    : QueryOverlay P I expected :=
  eq_rect
    actual (fun fiber => QueryOverlay P I fiber)
    overlay expected Hequal.

Definition align_query_overlay
    {P : CertifiedAtomProfile}
    {I : FixedWidthCarrierProfile}
    (expected : VocabularyFiber P I)
    (packed : PackedQueryOverlay P I)
    : option (QueryOverlay P I expected).
Proof.
  destruct packed as [actual overlay].
  destruct (vocabulary_fiber_eq_dec P I actual expected)
    as [Hequal | Hdifferent].
  - exact (Some (transport_query_overlay Hequal overlay)).
  - exact None.
Defined.

Theorem VWENC_186_QUERY_OVERLAY_FROM_ANOTHER_VOCABULARY_FIBER_IS_REJECTED :
  forall (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (expected actual : VocabularyFiber P I)
    (overlay : QueryOverlay P I actual),
    expected <> actual ->
    align_query_overlay expected
      (existT (fun fiber => QueryOverlay P I fiber) actual overlay) = None.
Proof.
  intros P I expected actual overlay Hdifferent.
  unfold align_query_overlay.
  destruct (vocabulary_fiber_eq_dec P I actual expected)
    as [Hequal | _].
  - exfalso. apply Hdifferent. symmetry. exact Hequal.
  - reflexivity.
Qed.

Definition query_overlay_id_values
    {P : CertifiedAtomProfile}
    {I : FixedWidthCarrierProfile}
    {fiber : VocabularyFiber P I}
    (overlay : QueryOverlay P I fiber) : list nat :=
  map
    (fun entry =>
      query_local_id_value P I fiber (snd entry))
    (query_overlay_entries P I fiber overlay).

Definition query_overlay_well_formed
    {P : CertifiedAtomProfile}
    {I : FixedWidthCarrierProfile}
    {fiber : VocabularyFiber P I}
    (overlay : QueryOverlay P I fiber) : Prop :=
  NoDup (map fst (query_overlay_entries P I fiber overlay)) /\
  NoDup (query_overlay_id_values overlay) /\
  Forall
    (fun entry =>
      query_local_namespace P I fiber (snd entry) =
        query_overlay_namespace P I fiber overlay /\
      query_local_id_value P I fiber (snd entry) <
        query_overlay_next P I fiber overlay)
    (query_overlay_entries P I fiber overlay).

Definition lookup_query_local
    {P : CertifiedAtomProfile}
    {I : FixedWidthCarrierProfile}
    {fiber : VocabularyFiber P I}
    (overlay : QueryOverlay P I fiber)
    (atom : CanonicalAtom P) : option (QueryLocalId P I fiber) :=
  assoc_lookup (canonical_atom_eq_dec P)
    (query_overlay_entries P I fiber overlay) atom.

Inductive QueryAtomResolution
    (P : CertifiedAtomProfile)
    (I : FixedWidthCarrierProfile)
    (fiber : VocabularyFiber P I) : Type :=
| QueryDurableSymbol :
    FiberBoundSymbolId P I -> QueryAtomResolution P I fiber
| QueryLocalSymbol : QueryLocalId P I fiber -> QueryAtomResolution P I fiber.

Record QueryResolutionResult
    (P : CertifiedAtomProfile)
    (I : FixedWidthCarrierProfile)
    (fiber : VocabularyFiber P I) : Type :=
  mkQueryResolutionResult {
    query_resolution : QueryAtomResolution P I fiber;
    query_overlay_after : QueryOverlay P I fiber
  }.

Definition resolve_query_atom
    {P : CertifiedAtomProfile}
    {I : FixedWidthCarrierProfile}
    {fiber : VocabularyFiber P I}
    (snapshot : VocabularySnapshot P I fiber)
    (overlay : QueryOverlay P I fiber)
    (atom : CanonicalAtom P) : QueryResolutionResult P I fiber :=
  match lookup_atom
      (vocabulary_snapshot_live_entries P I fiber snapshot) atom with
  | Some id =>
      mkQueryResolutionResult P I fiber
        (QueryDurableSymbol P I fiber
          (mkFiberBoundSymbolId P I fiber id)) overlay
  | None =>
      match lookup_query_local overlay atom with
      | Some id =>
          mkQueryResolutionResult P I fiber
            (QueryLocalSymbol P I fiber id) overlay
      | None =>
          let id :=
            mkQueryLocalId P I fiber
              (query_overlay_namespace P I fiber overlay)
              (query_overlay_next P I fiber overlay) in
          let updated :=
            mkQueryOverlay P I fiber
              (query_overlay_namespace P I fiber overlay)
              ((atom, id) :: query_overlay_entries P I fiber overlay)
              (S (query_overlay_next P I fiber overlay)) in
          mkQueryResolutionResult P I fiber
            (QueryLocalSymbol P I fiber id) updated
      end
  end.

Lemma query_overlay_fresh_insert_preserves_well_formedness :
  forall (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (fiber : VocabularyFiber P I)
    (overlay : QueryOverlay P I fiber) atom,
    query_overlay_well_formed overlay ->
    lookup_query_local overlay atom = None ->
    let fresh :=
      mkQueryLocalId P I fiber
        (query_overlay_namespace P I fiber overlay)
        (query_overlay_next P I fiber overlay) in
    query_overlay_well_formed
      (mkQueryOverlay P I fiber
        (query_overlay_namespace P I fiber overlay)
        ((atom, fresh) :: query_overlay_entries P I fiber overlay)
        (S (query_overlay_next P I fiber overlay))) /\
    ~ In (query_overlay_next P I fiber overlay)
        (query_overlay_id_values overlay).
Proof.
  intros P I fiber overlay atom
    [Hatom_unique [Hid_unique Hbelow]] Hlookup.
  simpl.
  assert (Hatom_absent :
    ~ In atom (map fst (query_overlay_entries P I fiber overlay))).
  { unfold lookup_query_local in Hlookup.
    now apply assoc_lookup_none_key_absent in Hlookup. }
  assert (Hid_absent :
    ~ In (query_overlay_next P I fiber overlay)
        (query_overlay_id_values overlay)).
  { intros Hin.
    unfold query_overlay_id_values in Hin.
    apply in_map_iff in Hin.
    destruct Hin as [[existing_atom existing_id] [Hequal Hin]].
    simpl in Hequal.
    apply Forall_forall with
      (x := (existing_atom, existing_id)) in Hbelow; [| exact Hin].
    destruct Hbelow as [_ Hlt]. simpl in Hlt. lia. }
  split.
  - split.
    + simpl. constructor; assumption.
    + split.
      * unfold query_overlay_id_values. simpl.
        constructor; assumption.
      * simpl. constructor.
        { split; [reflexivity |].
          apply Nat.lt_succ_diag_r. }
        { apply Forall_forall. intros entry Hin.
          apply Forall_forall with (x := entry) in Hbelow; [| exact Hin].
          destruct Hbelow as [Hnamespace Hlt].
          now split; [exact Hnamespace | lia]. }
  - exact Hid_absent.
Qed.

Theorem VWENC_121_UNKNOWN_QUERY_ATOM_RECEIVES_STABLE_QUERY_LOCAL_ID :
  forall (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (fiber : VocabularyFiber P I)
    (snapshot : VocabularySnapshot P I fiber)
    (overlay : QueryOverlay P I fiber) atom,
    query_overlay_well_formed overlay ->
    lookup_atom
      (vocabulary_snapshot_live_entries P I fiber snapshot) atom = None ->
    lookup_query_local overlay atom = None ->
    let fresh :=
      mkQueryLocalId P I fiber
        (query_overlay_namespace P I fiber overlay)
        (query_overlay_next P I fiber overlay) in
    let result := resolve_query_atom snapshot overlay atom in
    query_resolution P I fiber result =
      QueryLocalSymbol P I fiber fresh /\
    lookup_query_local (query_overlay_after P I fiber result) atom =
      Some fresh /\
    query_overlay_well_formed
      (query_overlay_after P I fiber result) /\
    interpret_query_local_id
      (query_overlay_namespace P I fiber overlay) fresh =
      Some (query_overlay_next P I fiber overlay) /\
    ~ In (query_overlay_next P I fiber overlay)
        (query_overlay_id_values overlay).
Proof.
  intros P I fiber snapshot overlay atom Hwell Hdurable Hoverlay.
  unfold resolve_query_atom. rewrite Hdurable, Hoverlay. simpl.
  split; [reflexivity |]. split.
  - unfold lookup_query_local. simpl.
    destruct (canonical_atom_eq_dec P atom atom);
      [reflexivity | contradiction].
  - split.
    + apply (proj1
        (query_overlay_fresh_insert_preserves_well_formedness
          P I fiber overlay atom Hwell Hoverlay)).
    + split.
      * unfold interpret_query_local_id. simpl.
        destruct (query_overlay_namespace_eq_dec P I fiber
          (query_overlay_namespace P I fiber overlay)
          (query_overlay_namespace P I fiber overlay));
          [reflexivity | contradiction].
      * apply (proj2
          (query_overlay_fresh_insert_preserves_well_formedness
            P I fiber overlay atom Hwell Hoverlay)).
Qed.

Theorem VWENC_122_REPEATED_QUERY_REUSES_OVERLAY_WITHOUT_DURABLE_MUTATION :
  forall (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (fiber : VocabularyFiber P I)
    (snapshot : VocabularySnapshot P I fiber)
    (overlay : QueryOverlay P I fiber) atom first,
    query_overlay_well_formed overlay ->
    lookup_atom
      (vocabulary_snapshot_live_entries P I fiber snapshot) atom = None ->
    resolve_query_atom snapshot overlay atom = first ->
    resolve_query_atom snapshot
      (query_overlay_after P I fiber first) atom =
      mkQueryResolutionResult P I fiber
        (query_resolution P I fiber first)
        (query_overlay_after P I fiber first) /\
    snapshot = snapshot /\
    query_overlay_well_formed
      (query_overlay_after P I fiber first).
Proof.
  intros P I fiber snapshot overlay atom first Hwell Hdurable Hfirst.
  unfold resolve_query_atom in Hfirst.
  rewrite Hdurable in Hfirst.
  destruct (lookup_query_local overlay atom)
    as [existing |] eqn:Hoverlay.
  - inversion Hfirst. subst first.
    split.
    + change
        ((match lookup_atom
            (vocabulary_snapshot_live_entries P I fiber snapshot) atom with
          | Some durable_id =>
              mkQueryResolutionResult P I fiber
                (QueryDurableSymbol P I fiber
                  (mkFiberBoundSymbolId P I fiber durable_id)) overlay
          | None =>
              match lookup_query_local overlay atom with
              | Some local_id =>
                  mkQueryResolutionResult P I fiber
                    (QueryLocalSymbol P I fiber local_id) overlay
              | None =>
                  let local_id :=
                    mkQueryLocalId P I fiber
                      (query_overlay_namespace P I fiber overlay)
                      (query_overlay_next P I fiber overlay) in
                  mkQueryResolutionResult P I fiber
                    (QueryLocalSymbol P I fiber local_id)
                    (mkQueryOverlay P I fiber
                      (query_overlay_namespace P I fiber overlay)
                      ((atom, local_id) ::
                        query_overlay_entries P I fiber overlay)
                      (S (query_overlay_next P I fiber overlay)))
              end
          end) =
          mkQueryResolutionResult P I fiber
            (QueryLocalSymbol P I fiber existing) overlay).
      now rewrite Hdurable, Hoverlay.
    + now split.
  - inversion Hfirst. subst first. simpl.
    unfold resolve_query_atom. rewrite Hdurable. simpl.
    unfold lookup_query_local. simpl.
    destruct (canonical_atom_eq_dec P atom atom);
      [| contradiction].
    split; [reflexivity |]. split; [reflexivity |].
    apply (proj1
      (query_overlay_fresh_insert_preserves_well_formedness
        P I fiber overlay atom Hwell Hoverlay)).
Qed.

Definition serialize_query_resolution
    {P : CertifiedAtomProfile}
    {I : FixedWidthCarrierProfile}
    {fiber : VocabularyFiber P I}
    (resolution : QueryAtomResolution P I fiber)
    : option (FiberBoundSymbolId P I) :=
  match resolution with
  | QueryDurableSymbol _ _ _ id => Some id
  | QueryLocalSymbol _ _ _ _ => None
  end.

Theorem VWENC_184_DURABLE_QUERY_RESOLUTION_BINDS_THE_EXACT_SNAPSHOT_FIBER :
  forall (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (fiber : VocabularyFiber P I)
    (snapshot : VocabularySnapshot P I fiber)
    (overlay : QueryOverlay P I fiber) atom id,
    lookup_atom
      (vocabulary_snapshot_live_entries P I fiber snapshot) atom = Some id ->
    query_resolution P I fiber
      (resolve_query_atom snapshot overlay atom) =
        QueryDurableSymbol P I fiber
          (mkFiberBoundSymbolId P I fiber id) /\
    serialize_query_resolution
      (query_resolution P I fiber
        (resolve_query_atom snapshot overlay atom)) =
        Some (mkFiberBoundSymbolId P I fiber id).
Proof.
  intros P I fiber snapshot overlay atom id Hlookup.
  unfold resolve_query_atom. rewrite Hlookup. now split.
Qed.

Theorem VWENC_185_SERIALIZED_DURABLE_QUERY_ID_RETAINS_ITS_FIBER :
  forall (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (fiber actual : VocabularyFiber P I) id,
    serialize_query_resolution
      (QueryDurableSymbol P I fiber
        (mkFiberBoundSymbolId P I actual id)) =
      Some (mkFiberBoundSymbolId P I actual id) /\
    (fiber <> actual ->
      interpret_symbol_id fiber
        (mkFiberBoundSymbolId P I actual id) = None).
Proof.
  intros P I fiber actual id.
  split; [reflexivity |].
  apply VWENC_112_CROSS_FIBER_ID_INTERPRETATION_IS_REJECTED.
Qed.

Theorem VWENC_139_QUERY_LOCAL_IDS_CANNOT_ENTER_DURABLE_ID_SEQUENCES :
  forall (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (fiber : VocabularyFiber P I) local_id,
    serialize_query_resolution
      (QueryLocalSymbol P I fiber local_id) = None.
Proof. reflexivity. Qed.
(** ** Exact dependent sequence descriptors *)

Theorem VWENC_123_SEQUENCE_DESCRIPTOR_REQUIRES_EXACT_VOCABULARY_FIBER :
  forall (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (fiber : VocabularyFiber P I)
    (snapshot : VocabularySnapshot P I fiber) descriptor,
    descriptor_accepts_vocabulary_snapshot snapshot descriptor ->
    descriptor_fiber P I descriptor = fiber.
Proof. intros P I fiber snapshot descriptor [Hexact _]. exact Hexact. Qed.

Theorem VWENC_124_DESCRIPTOR_VALIDATES_EACH_LIVE_ID_NOT_DENSE_FRONTIER :
  forall (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (fiber : VocabularyFiber P I)
    (snapshot : VocabularySnapshot P I fiber) descriptor id,
    descriptor_accepts_vocabulary_snapshot snapshot descriptor ->
    In id (descriptor_ids P I descriptor) ->
    symbol_id_value I id <
      descriptor_required_frontier P I descriptor /\
    live_symbol
      (vocabulary_snapshot_live_entries P I fiber snapshot) id.
Proof.
  intros P I fiber snapshot descriptor id
    [_ [_ Hids]] Hin.
  now apply Forall_forall with (x := id) in Hids.
Qed.

(** ** Immutable exact-state snapshot observations *)

Definition observed_vocabulary_entry
    {P : CertifiedAtomProfile} {I : FixedWidthCarrierProfile}
    (entry : VocabularyEntry P I) :=
  (canonical_atom_identity (fst entry),
   symbol_id_value I (snd entry)).

Definition observed_symbol_sequence
    {I : FixedWidthCarrierProfile} (sequence : list (SymbolId I)) :=
  map (symbol_id_value I) sequence.

Definition observed_term_entry
    {I T : FixedWidthCarrierProfile} (entry : TermEntry I T) :=
  (observed_symbol_sequence (fst entry),
   term_id_value T (snd entry)).

Definition observed_reverse_span
    {I : FixedWidthCarrierProfile}
    (entry : SymbolId I * ByteSpan) :=
  (symbol_id_value I (fst entry),
   (span_offset (snd entry), span_length (snd entry))).

Definition observed_allocation_classes
    {P : CertifiedAtomProfile}
    {I T : FixedWidthCarrierProfile}
    (state : InterningState P I T) :=
  (map observed_vocabulary_entry
     (state_reserved_entries P I T state),
   (map observed_vocabulary_entry
      (state_claimed_entries P I T state),
    (map observed_vocabulary_entry
       (state_live_entries P I T state),
     (map observed_vocabulary_entry
        (state_ever_entries P I T state),
      (map observed_vocabulary_entry
         (state_orphan_entries P I T state),
       map observed_vocabulary_entry
         (state_unmaterialized_orphan_entries P I T state)))))).

Definition observed_packed_storage
    {P : CertifiedAtomProfile}
    {I T : FixedWidthCarrierProfile}
    (state : InterningState P I T) :=
  (packed_canonical_bytes I (state_packed_storage P I T state),
   map observed_reverse_span
     (packed_reverse_spans I (state_packed_storage P I T state))).

Definition observed_dependent_state
    {P : CertifiedAtomProfile}
    {I T : FixedWidthCarrierProfile}
    (state : InterningState P I T) :=
  (state_allocator_frontier P I T state,
   (map observed_symbol_sequence (state_sequences P I T state),
    (state_term_dictionary_enabled P I T state,
     map observed_term_entry (state_term_entries P I T state)))).

Definition interning_state_observation
    {P : CertifiedAtomProfile}
    {I T : FixedWidthCarrierProfile}
    (state : InterningState P I T) :=
  (vocabulary_fiber_identity (state_fiber P I T state),
   (term_dictionary_fiber_identity (state_term_fiber P I T state),
    (observed_allocation_classes state,
     (observed_packed_storage state,
      observed_dependent_state state)))).

Record InternedDictionarySnapshot
    (P : CertifiedAtomProfile)
    (I T : FixedWidthCarrierProfile) : Type :=
  mkInternedDictionarySnapshot {
    snapshot_exact_state : InterningState P I T
  }.

Definition capture_snapshot
    {P : CertifiedAtomProfile}
    {I T : FixedWidthCarrierProfile}
    (state : InterningState P I T)
    : InternedDictionarySnapshot P I T :=
  mkInternedDictionarySnapshot P I T state.

Definition snapshot_observation
    {P : CertifiedAtomProfile}
    {I T : FixedWidthCarrierProfile}
    (snapshot : InternedDictionarySnapshot P I T) :=
  interning_state_observation
    (snapshot_exact_state P I T snapshot).

Record SnapshotSession
    (P : CertifiedAtomProfile)
    (I T : FixedWidthCarrierProfile) : Type :=
  mkSnapshotSession {
    session_captured : InternedDictionarySnapshot P I T;
    session_current : InterningState P I T
  }.

Definition begin_snapshot_session
    {P : CertifiedAtomProfile}
    {I T : FixedWidthCarrierProfile}
    (state : InterningState P I T) : SnapshotSession P I T :=
  mkSnapshotSession P I T (capture_snapshot state) state.

Inductive SnapshotSessionTransition
    {P : CertifiedAtomProfile}
    {I T : FixedWidthCarrierProfile}
    : SnapshotSession P I T -> SnapshotSession P I T -> Prop :=
| AdvanceSnapshotSession :
    forall captured current later,
      InterningTransition current later ->
      SnapshotSessionTransition
        (mkSnapshotSession P I T captured current)
        (mkSnapshotSession P I T captured later).

Theorem VWENC_125_CAPTURED_SNAPSHOT_OBSERVATIONS_SURVIVE_LATER_PUBLICATION :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (before later : SnapshotSession P I T),
    SnapshotSessionTransition before later ->
    session_captured P I T later = session_captured P I T before /\
    snapshot_observation (session_captured P I T later) =
      snapshot_observation (session_captured P I T before).
Proof.
  intros P I T before later Htransition.
  inversion Htransition. now split.
Qed.

Theorem VWENC_173_CAPTURED_SNAPSHOT_IS_THE_EXACT_INITIAL_STATE :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (state : InterningState P I T),
    snapshot_exact_state P I T
      (session_captured P I T (begin_snapshot_session state)) = state /\
    snapshot_observation
      (session_captured P I T (begin_snapshot_session state)) =
      interning_state_observation state.
Proof. intros. now split. Qed.

Inductive SnapshotSessionReachable
    {P : CertifiedAtomProfile}
    {I T : FixedWidthCarrierProfile}
    (initial : SnapshotSession P I T)
    : SnapshotSession P I T -> Prop :=
| SnapshotSessionReachableInitial :
    SnapshotSessionReachable initial initial
| SnapshotSessionReachableStep :
    forall current later,
      SnapshotSessionReachable initial current ->
      SnapshotSessionTransition current later ->
      SnapshotSessionReachable initial later.

Theorem VWENC_174_EXACT_CAPTURE_SURVIVES_ARBITRARY_LATER_TRANSITIONS :
  forall (P : CertifiedAtomProfile) (I T : FixedWidthCarrierProfile)
    (initial later : SnapshotSession P I T),
    SnapshotSessionReachable initial later ->
    session_captured P I T later = session_captured P I T initial /\
    snapshot_observation (session_captured P I T later) =
      snapshot_observation (session_captured P I T initial).
Proof.
  intros P I T initial later Hreachable.
  induction Hreachable as
    [| current later Hcurrent IH Htransition].
  - now split.
  - destruct (VWENC_125_CAPTURED_SNAPSHOT_OBSERVATIONS_SURVIVE_LATER_PUBLICATION
      P I T current later Htransition) as [Hcaptured Hobservation].
    split.
    + now rewrite Hcaptured.
    + now rewrite Hobservation.
Qed.
(** ** Exact, machine-readable model-to-Rust correspondence *)

Inductive CorrespondenceRelationship : Type :=
| Refines
| CommonSubstrateOnly
| Conflicts
| Prospective.

Inductive InterningFormalPoint : Type :=
| PointCertifiedAtomProfile
| PointSymbolIdCarrierCodec
| PointTermIdCarrierCodec
| PointForwardAtomToId
| PointReverseIdToAtom
| PointForwardReverseBijection
| PointAllocationStatus
| PointClaimAllocation
| PointOrphanAllocation
| PointTombstoneNoReuse
| PointPackedCanonicalStorage
| PointSparseAllocatorFrontierStorage
| PointSparseAllocatorFrontierAccess
| PointVocabularyFiberHeader
| PointIdSequenceView
| PointSequenceDescriptorLiveMembership
| PointOptionalTermDictionary
| PointCoordinatedSequenceOwner
| PointQueryLocalOverlay
| PointImmutableSnapshot
| PointBeginGeneration
| PointDurabilizeLiveId
| PointSealDurableVocabulary
| PointPublishVocabularyEligibility
| PointStageDependentSequence
| PointPublishSequenceVisibility
| PointDurabilizeDependentSequence
| PointWriteVocabularyObject
| PointSyncVocabularyObject
| PointWriteSequenceObject
| PointSyncSequenceObject
| PointAtomicCheckpointHeadPublication
| PointCaptureReader
| PointSaveReaderContinuation
| PointResumeReaderContinuation
| PointLoseVocabularyArtifact
| PointLoseSequenceArtifact
| PointCorruptVocabularyArtifact
| PointCorruptSequenceArtifact
| PointCrashTransition
| PointStrictPairRecovery
| PointCommitSequenceSubstrate
| PointCommittedWatermarkSubstrate
| PointDurableOverlayInsertionSubstrate
| PointCheckpointLockSubstrate
| PointHeaderCheckpointPublicationSubstrate.

Inductive ImplementationObligation : Type :=
| ObligationAddCertifiedProfileSurface
| ObligationAddSymbolIdCarrierCodec
| ObligationAddTermIdCarrierCodec
| ObligationGeneralizeForwardVocabulary
| ObligationGeneralizeReverseVocabulary
| ObligationCoordinateBijectionVisibility
| ObligationAddAllocationLedger
| ObligationReuseSparseAllocationClaim
| ObligationRetainOrphanedIds
| ObligationAddNoReuseTombstones
| ObligationAddPackedStorage
| ObligationPreserveSparseFrontierStorage
| ObligationExposeSparseFrontier
| ObligationAddProfileGenerationHeader
| ObligationAddBorrowedFiberBoundView
| ObligationAddSequenceDescriptorValidation
| ObligationAddOptionalTermDictionary
| ObligationAddCoordinatedOwner
| ObligationAddEphemeralOverlay
| ObligationAuditSnapshotRefinement
| ObligationAddGenerationStaging
| ObligationAddDurablePackedPublication
| ObligationAddVocabularySeal
| ObligationAddVocabularyEligibilityPublication
| ObligationAddSequenceStaging
| ObligationAddSequenceVisibilityPublication
| ObligationAddSequenceDurabilityPublication
| ObligationReuseVocabularyObjectWrite
| ObligationReuseVocabularyObjectSync
| ObligationReuseSequenceObjectWrite
| ObligationReuseSequenceObjectSync
| ObligationAddAtomicCheckpointHead
| ObligationAddReaderCapture
| ObligationAddContinuationCapture
| ObligationAddContinuationResume
| ObligationAddVocabularyLossRecoveryCase
| ObligationAddSequenceLossRecoveryCase
| ObligationAddVocabularyCorruptionRecoveryCase
| ObligationAddSequenceCorruptionRecoveryCase
| ObligationAddCrashStateTransition
| ObligationAddExactOldNewRecovery
| ObligationReuseCommitSequence
| ObligationReuseCommittedWatermark
| ObligationReuseDurableOverlayInsertion
| ObligationReuseCheckpointLock
| ObligationExtendHeaderCheckpointPublication.

Record CorrespondenceRow : Type := mkCorrespondenceRow {
  correspondence_formal_point : InterningFormalPoint;
  correspondence_source_path : string;
  correspondence_rust_symbol : string;
  correspondence_relationship : CorrespondenceRelationship;
  correspondence_obligation : ImplementationObligation
}.

Definition declared_correspondence_row
    (point : InterningFormalPoint) : CorrespondenceRow :=
  match point with
  | PointCertifiedAtomProfile =>
      mkCorrespondenceRow point
        ("src/profile/mod.rs")%string
        ("DictionaryProfile")%string
        Prospective ObligationAddCertifiedProfileSurface
  | PointSymbolIdCarrierCodec =>
      mkCorrespondenceRow point
        ("src/profile/interned/id.rs")%string
        ("SymbolId<I>::{try_from_nat,encode,decode}")%string
        Prospective ObligationAddSymbolIdCarrierCodec
  | PointTermIdCarrierCodec =>
      mkCorrespondenceRow point
        ("src/profile/interned/id.rs")%string
        ("TermId<I>::{try_from_nat,encode,decode}")%string
        Prospective ObligationAddTermIdCarrierCodec
  | PointForwardAtomToId =>
      mkCorrespondenceRow point
        ("src/persistent_artrie/vocab/mutation_api.rs")%string
        ("PersistentVocabARTrie::insert")%string
        CommonSubstrateOnly ObligationGeneralizeForwardVocabulary
  | PointReverseIdToAtom =>
      mkCorrespondenceRow point
        ("src/persistent_artrie/vocab/query_api.rs")%string
        ("PersistentVocabARTrie::get_term")%string
        CommonSubstrateOnly ObligationGeneralizeReverseVocabulary
  | PointForwardReverseBijection =>
      mkCorrespondenceRow point
        ("src/persistent_artrie/vocab/dict_impl.rs")%string
        ("PersistentVocabARTrie::reverse_term_map")%string
        Conflicts ObligationCoordinateBijectionVisibility
  | PointAllocationStatus =>
      mkCorrespondenceRow point
        ("src/profile/interned/allocation.rs")%string
        ("AllocationStatus")%string
        Prospective ObligationAddAllocationLedger
  | PointClaimAllocation =>
      mkCorrespondenceRow point
        ("src/persistent_artrie/vocab/mutation_api.rs")%string
        ("PersistentVocabARTrie::insert_overlay")%string
        CommonSubstrateOnly ObligationReuseSparseAllocationClaim
  | PointOrphanAllocation =>
      mkCorrespondenceRow point
        ("src/persistent_artrie/vocab/mutation_api.rs")%string
        ("PersistentVocabARTrie::insert_overlay")%string
        CommonSubstrateOnly ObligationRetainOrphanedIds
  | PointTombstoneNoReuse =>
      mkCorrespondenceRow point
        ("src/profile/interned/allocation.rs")%string
        ("AllocationLedger::tombstone")%string
        Prospective ObligationAddNoReuseTombstones
  | PointPackedCanonicalStorage =>
      mkCorrespondenceRow point
        ("src/profile/interned/storage.rs")%string
        ("PackedAtomStorage")%string
        Prospective ObligationAddPackedStorage
  | PointSparseAllocatorFrontierStorage =>
      mkCorrespondenceRow point
        ("src/persistent_artrie/vocab/dict_impl.rs")%string
        ("PersistentVocabARTrie::next_index")%string
        CommonSubstrateOnly ObligationPreserveSparseFrontierStorage
  | PointSparseAllocatorFrontierAccess =>
      mkCorrespondenceRow point
        ("src/persistent_artrie/vocab/query_api.rs")%string
        ("PersistentVocabARTrie::next_index")%string
        CommonSubstrateOnly ObligationExposeSparseFrontier
  | PointVocabularyFiberHeader =>
      mkCorrespondenceRow point
        ("src/persistent_artrie/vocab/types.rs")%string
        ("VocabTrieFileHeader")%string
        Conflicts ObligationAddProfileGenerationHeader
  | PointIdSequenceView =>
      mkCorrespondenceRow point
        ("src/profile/interned/view.rs")%string
        ("IdSequenceView")%string
        Prospective ObligationAddBorrowedFiberBoundView
  | PointSequenceDescriptorLiveMembership =>
      mkCorrespondenceRow point
        ("src/profile/interned/descriptor.rs")%string
        ("SequenceDescriptor::validate_live_ids")%string
        Prospective ObligationAddSequenceDescriptorValidation
  | PointOptionalTermDictionary =>
      mkCorrespondenceRow point
        ("src/profile/interned/term_dictionary.rs")%string
        ("TermSequenceDictionary<I,T,D>")%string
        Prospective ObligationAddOptionalTermDictionary
  | PointCoordinatedSequenceOwner =>
      mkCorrespondenceRow point
        ("src/profile/interned/coordinator.rs")%string
        ("InternedSequenceDictionary<C,I,D>")%string
        Prospective ObligationAddCoordinatedOwner
  | PointQueryLocalOverlay =>
      mkCorrespondenceRow point
        ("src/profile/interned/query.rs")%string
        ("QueryOverlay<I>")%string
        Prospective ObligationAddEphemeralOverlay
  | PointImmutableSnapshot =>
      mkCorrespondenceRow point
        ("src/persistent_artrie/vocab/mutation_api.rs")%string
        ("PersistentVocabARTrie::snapshot")%string
        CommonSubstrateOnly ObligationAuditSnapshotRefinement
  | PointBeginGeneration =>
      mkCorrespondenceRow point
        ("src/profile/interned/coordinator.rs")%string
        ("InternedSequenceDictionary::begin_generation")%string
        Prospective ObligationAddGenerationStaging
  | PointDurabilizeLiveId =>
      mkCorrespondenceRow point
        ("src/profile/interned/persistence.rs")%string
        ("InternedVocabularyWriter::durabilize_live_id")%string
        Prospective ObligationAddDurablePackedPublication
  | PointSealDurableVocabulary =>
      mkCorrespondenceRow point
        ("src/profile/interned/persistence.rs")%string
        ("InternedVocabularyWriter::seal_vocabulary")%string
        Prospective ObligationAddVocabularySeal
  | PointPublishVocabularyEligibility =>
      mkCorrespondenceRow point
        ("src/profile/interned/persistence.rs")%string
        ("InternedVocabularyWriter::publish_frontier")%string
        Prospective ObligationAddVocabularyEligibilityPublication
  | PointStageDependentSequence =>
      mkCorrespondenceRow point
        ("src/profile/interned/coordinator.rs")%string
        ("InternedSequenceDictionary::stage_sequence")%string
        Prospective ObligationAddSequenceStaging
  | PointPublishSequenceVisibility =>
      mkCorrespondenceRow point
        ("src/profile/interned/coordinator.rs")%string
        ("InternedSequenceDictionary::publish_sequence")%string
        Prospective ObligationAddSequenceVisibilityPublication
  | PointDurabilizeDependentSequence =>
      mkCorrespondenceRow point
        ("src/profile/interned/persistence.rs")%string
        ("InternedSequenceWriter::durabilize_sequence")%string
        Prospective ObligationAddSequenceDurabilityPublication
  | PointWriteVocabularyObject =>
      mkCorrespondenceRow point
        ("src/persistent_artrie/vocab/persistence_api.rs")%string
        ("PersistentVocabARTrie::checkpoint_overlay")%string
        CommonSubstrateOnly ObligationReuseVocabularyObjectWrite
  | PointSyncVocabularyObject =>
      mkCorrespondenceRow point
        ("src/persistent_artrie/vocab/persistence_api.rs")%string
        ("PersistentVocabARTrie::checkpoint_overlay")%string
        CommonSubstrateOnly ObligationReuseVocabularyObjectSync
  | PointWriteSequenceObject =>
      mkCorrespondenceRow point
        ("src/persistent_artrie/u64.rs")%string
        ("write_snapshot_file")%string
        CommonSubstrateOnly ObligationReuseSequenceObjectWrite
  | PointSyncSequenceObject =>
      mkCorrespondenceRow point
        ("src/persistent_artrie/u64.rs")%string
        ("write_snapshot_file")%string
        CommonSubstrateOnly ObligationReuseSequenceObjectSync
  | PointAtomicCheckpointHeadPublication =>
      mkCorrespondenceRow point
        ("src/profile/interned/persistence.rs")%string
        ("InternedSequenceDictionary::publish_checkpoint_head")%string
        Prospective ObligationAddAtomicCheckpointHead
  | PointCaptureReader =>
      mkCorrespondenceRow point
        ("src/profile/interned/snapshot.rs")%string
        ("InternedSequenceDictionary::snapshot")%string
        Prospective ObligationAddReaderCapture
  | PointSaveReaderContinuation =>
      mkCorrespondenceRow point
        ("src/profile/interned/continuation.rs")%string
        ("InternedReadCursor::continuation")%string
        Prospective ObligationAddContinuationCapture
  | PointResumeReaderContinuation =>
      mkCorrespondenceRow point
        ("src/profile/interned/continuation.rs")%string
        ("InternedSequenceDictionary::resume")%string
        Prospective ObligationAddContinuationResume
  | PointLoseVocabularyArtifact =>
      mkCorrespondenceRow point
        ("src/profile/interned/recovery.rs")%string
        ("InternedRecoveryError::MissingVocabulary")%string
        Prospective ObligationAddVocabularyLossRecoveryCase
  | PointLoseSequenceArtifact =>
      mkCorrespondenceRow point
        ("src/profile/interned/recovery.rs")%string
        ("InternedRecoveryError::MissingSequence")%string
        Prospective ObligationAddSequenceLossRecoveryCase
  | PointCorruptVocabularyArtifact =>
      mkCorrespondenceRow point
        ("src/profile/interned/recovery.rs")%string
        ("InternedRecoveryError::CorruptVocabulary")%string
        Prospective ObligationAddVocabularyCorruptionRecoveryCase
  | PointCorruptSequenceArtifact =>
      mkCorrespondenceRow point
        ("src/profile/interned/recovery.rs")%string
        ("InternedRecoveryError::CorruptSequence")%string
        Prospective ObligationAddSequenceCorruptionRecoveryCase
  | PointCrashTransition =>
      mkCorrespondenceRow point
        ("src/profile/interned/recovery.rs")%string
        ("InternedRecoveryState")%string
        Prospective ObligationAddCrashStateTransition
  | PointStrictPairRecovery =>
      mkCorrespondenceRow point
        ("src/profile/interned/recovery.rs")%string
        ("InternedSequenceDictionary::recover")%string
        Prospective ObligationAddExactOldNewRecovery
  | PointCommitSequenceSubstrate =>
      mkCorrespondenceRow point
        ("src/persistent_artrie/vocab/dict_impl.rs")%string
        ("PersistentVocabARTrie::commit_seq")%string
        CommonSubstrateOnly ObligationReuseCommitSequence
  | PointCommittedWatermarkSubstrate =>
      mkCorrespondenceRow point
        ("src/persistent_artrie/vocab/dict_impl.rs")%string
        ("PersistentVocabARTrie::committed_watermark")%string
        CommonSubstrateOnly ObligationReuseCommittedWatermark
  | PointDurableOverlayInsertionSubstrate =>
      mkCorrespondenceRow point
        ("src/persistent_artrie/core/overlay/durable_write.rs")%string
        ("DurableOverlayWrite::insert_cas_with_value_durable_default")%string
        CommonSubstrateOnly ObligationReuseDurableOverlayInsertion
  | PointCheckpointLockSubstrate =>
      mkCorrespondenceRow point
        ("src/persistent_artrie/vocab/dict_impl.rs")%string
        ("PersistentVocabARTrie::checkpoint_lock")%string
        CommonSubstrateOnly ObligationReuseCheckpointLock
  | PointHeaderCheckpointPublicationSubstrate =>
      mkCorrespondenceRow point
        ("src/persistent_artrie/vocab/persistence_api.rs")%string
        ("PersistentVocabARTrie::checkpoint_overlay")%string
        CommonSubstrateOnly ObligationExtendHeaderCheckpointPublication
  end.

Definition complete_interning_formal_points : list InterningFormalPoint :=
  [PointCertifiedAtomProfile;
   PointSymbolIdCarrierCodec;
   PointTermIdCarrierCodec;
   PointForwardAtomToId;
   PointReverseIdToAtom;
   PointForwardReverseBijection;
   PointAllocationStatus;
   PointClaimAllocation;
   PointOrphanAllocation;
   PointTombstoneNoReuse;
   PointPackedCanonicalStorage;
   PointSparseAllocatorFrontierStorage;
   PointSparseAllocatorFrontierAccess;
   PointVocabularyFiberHeader;
   PointIdSequenceView;
   PointSequenceDescriptorLiveMembership;
   PointOptionalTermDictionary;
   PointCoordinatedSequenceOwner;
   PointQueryLocalOverlay;
   PointImmutableSnapshot;
   PointBeginGeneration;
   PointDurabilizeLiveId;
   PointSealDurableVocabulary;
   PointPublishVocabularyEligibility;
   PointStageDependentSequence;
   PointPublishSequenceVisibility;
   PointDurabilizeDependentSequence;
   PointWriteVocabularyObject;
   PointSyncVocabularyObject;
   PointWriteSequenceObject;
   PointSyncSequenceObject;
   PointAtomicCheckpointHeadPublication;
   PointCaptureReader;
   PointSaveReaderContinuation;
   PointResumeReaderContinuation;
   PointLoseVocabularyArtifact;
   PointLoseSequenceArtifact;
   PointCorruptVocabularyArtifact;
   PointCorruptSequenceArtifact;
   PointCrashTransition;
   PointStrictPairRecovery;
   PointCommitSequenceSubstrate;
   PointCommittedWatermarkSubstrate;
   PointDurableOverlayInsertionSubstrate;
   PointCheckpointLockSubstrate;
   PointHeaderCheckpointPublicationSubstrate].

Definition interning_correspondence : list CorrespondenceRow :=
  map declared_correspondence_row complete_interning_formal_points.

Lemma declared_correspondence_row_has_requested_point :
  forall point,
    correspondence_formal_point (declared_correspondence_row point) = point.
Proof.
  intros point. destruct point; reflexivity.
Qed.

Theorem VWENC_126_CORRESPONDENCE_SCHEMA_IS_TOTAL_NODUP_AND_EXPLICIT :
  map correspondence_formal_point interning_correspondence =
    complete_interning_formal_points /\
  NoDup complete_interning_formal_points /\
  (forall point, In point complete_interning_formal_points) /\
  (forall row,
      In row interning_correspondence ->
      row = declared_correspondence_row
        (correspondence_formal_point row)).
Proof.
  split.
  - reflexivity.
  - split.
    + repeat
        (apply NoDup_cons; [simpl; intuition discriminate |]).
      constructor.
    + split.
      * intros point. destruct point; simpl; tauto.
      * intros row Hin.
        unfold interning_correspondence in Hin.
        apply in_map_iff in Hin.
        destruct Hin as [point [Hrow Hin]].
        subst row.
        now rewrite declared_correspondence_row_has_requested_point.
Qed.

(** ** Representation-independent equality and native-ID observations *)

Definition canonical_atom_equalb
    {P : CertifiedAtomProfile}
    (left right : CanonicalAtom P) : bool :=
  if canonical_atom_eq_dec P left right then true else false.

Theorem VWENC_127_CANONICAL_ATOM_EQUALITY_IS_EXACT :
  forall (P : CertifiedAtomProfile) (left right : CanonicalAtom P),
    canonical_atom_equalb left right = true <-> left = right.
Proof.
  intros P left right. unfold canonical_atom_equalb.
  destruct (canonical_atom_eq_dec P left right)
    as [Hequal | Hdifferent].
  - split; [intros _; exact Hequal | intros _; reflexivity].
  - split; [discriminate | contradiction].
Qed.

Theorem VWENC_128_EVERY_CANONICAL_ATOM_CODEWORD_IS_NONEMPTY :
  forall (P : CertifiedAtomProfile) (atom : CanonicalAtom P),
    canonical_atom_bytes P atom <> [].
Proof.
  intros P atom.
  apply (atom_codeword_nonempty P).
  exact (canonical_atom_valid P atom).
Qed.

Theorem VWENC_129_FINGERPRINTS_ARE_CANDIDATES_NOT_ATOM_IDENTITY :
  (forall (P : CertifiedAtomProfile) (left right : CanonicalAtom P),
      left = right ->
      fingerprint_candidate left = fingerprint_candidate right) /\
  exists left right : CanonicalAtom canonical_uleb_profile,
    fingerprint_candidate left = fingerprint_candidate right /\
    left <> right.
Proof.
  split.
  - intros P left right Hequal. now subst right.
  - exists collision_atom_left, collision_atom_right.
    split; [reflexivity |].
    intros Hequal.
    apply (f_equal
      (canonical_atom_bytes canonical_uleb_profile)) in Hequal.
    discriminate.
Qed.

Definition native_id_view_observation
    {P : CertifiedAtomProfile} {I : FixedWidthCarrierProfile}
    (view : IdSequenceView P I) (index : nat) :=
  match id_sequence_view_index view index with
  | Some bound_id =>
      Some
        (backing_identity P I (view_backing P I view), bound_id)
  | None => None
  end.

Theorem VWENC_138_NATIVE_ID_VIEW_PRESERVES_BACKING_AND_FIBER_BINDING :
  forall (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (view : IdSequenceView P I) index bound_id,
    id_sequence_view_index view index = Some bound_id ->
    native_id_view_observation view index =
      Some
        (backing_identity P I (view_backing P I view), bound_id).
Proof.
  intros P I view index bound_id Hindex.
  unfold native_id_view_observation. now rewrite Hindex.
Qed.

Theorem VWENC_140_NATIVE_ID_OBSERVATION_ROUNDTRIPS_WITHOUT_ATOM_DECODING :
  forall (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (view : IdSequenceView P I) index bound_id,
    valid_id_sequence_view view ->
    id_sequence_view_index view index = Some bound_id ->
    native_id_view_observation view index =
      Some
        (backing_identity P I (view_backing P I view), bound_id) /\
    exists bytes id,
      bound_id =
        mkFiberBoundSymbolId P I
          (backing_fiber P I (view_backing P I view)) id /\
      nth_error
        (descriptor_ids P I
          (backing_descriptor P I (view_backing P I view)))
        (view_start P I view + index) = Some id /\
      index < view_count P I view /\
      id_sequence_view_bytes view index = Some bytes /\
      bytes = id_sequence_view_byte_window view index /\
      bytes = encode_symbol_id I id /\
      live_symbol
        (vocabulary_snapshot_live_entries
          P I
          (backing_fiber P I (view_backing P I view))
          (backing_snapshot P I (view_backing P I view))) id /\
      id_sequence_view_byte_offset view index =
        (view_start P I view + index) * carrier_width_bytes I /\
      List.length bytes = carrier_width_bytes I /\
      Forall valid_byte bytes /\
      decode_symbol_id I bytes = Some id /\
      List.length (encode_symbol_id I id) = carrier_width_bytes I /\
      decode_symbol_id I (encode_symbol_id I id) = Some id.
Proof.
  intros P I view index bound_id Hvalid Hindex.
  split.
  - now apply VWENC_138_NATIVE_ID_VIEW_PRESERVES_BACKING_AND_FIBER_BINDING.
  - destruct (VWENC_135_ID_VIEW_ELEMENTS_HAVE_EXACT_CARRIER_STRIDE
      P I view index bound_id Hvalid Hindex)
      as [Hwithin Hexists].
    destruct Hexists as [bytes [id Hproperties]].
    destruct Hproperties as
      [Hbound [Hnth [Hwindow [Hexact [Hencoded [Hlive
        [Hoffset [Hlength [Hbytes [Hdecode
          [Hencoded_length Hroundtrip]]]]]]]]]]].
    exists bytes, id.
    split; [exact Hbound |].
    split; [exact Hnth |].
    split; [exact Hwithin |].
    split; [exact Hwindow |].
    split; [exact Hexact |].
    split; [exact Hencoded |].
    split; [exact Hlive |].
    split; [exact Hoffset |].
    split; [exact Hlength |].
    split; [exact Hbytes |].
    split; [exact Hdecode |].
    now split.
Qed.

End VariableWidthInterning.
