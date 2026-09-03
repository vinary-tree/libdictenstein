(** * Family/profile refinement and consumer-observation laws

    This module is the family-wide refinement layer for variable-width logical
    units.  It deliberately separates three concepts:

    - a logical observation, which is visible to [liblevenshtein], [llattice],
      and other traversing consumers;
    - a storage/profile route, which selects a native kernel, a fixed-width ID
      kernel, or the PathMap byte adapter before traversal begins; and
    - physical state, including encoded staging bytes and node layout, which is
      not a logical observation.

    Arbitrary-width canonical ULEB128 values remain in the vocabulary owner.
    A hot dictionary edge contains either one direct fixed-width unit or one
    fixed-width [SymbolId].  Consequently the vocabulary binding is checked
    once when a snapshot/query view is constructed, never by decoding an
    arbitrary-width value at every node.

    The closed family, profile, and surface inventories below are the
    reviewable applicability matrix for this release.  Open downstream unit
    implementations remain possible in memory; persistent format identities
    are separately certified and never inferred from Rust type names.

    Stable theorem names beginning with [VWENC_] are machine-readable
    invariant identifiers.  They are extracted into the implementation
    conformance ledger and property-test suite after this formal gate closes.
*)

From Coq Require Import Arith Bool Lia List PeanoNat ProofIrrelevance.
Require Import ARTrie.Spec.VariableWidthCodecSpec.
Require Import ARTrie.Spec.VariableWidthInterningSpec.
Import ListNotations.
Import VariableWidthCodecSpec VariableWidthInterning.

Module VariableWidthFamilyRefinementSpec.

(** ** Representation-independent logical observations *)

Record LogicalObservations (Atom Value : Type) : Type := {
  observe_membership : list Atom -> bool;
  observe_terminality : list Atom -> bool;
  observe_mapped_value : list Atom -> option Value;
  observe_ordered_outgoing : list Atom -> list Atom;
  observe_prefix_entries : list Atom -> list (list Atom * option Value);
  observe_full_enumeration : list (list Atom * option Value);
  observe_substring_applicable : bool;
  observe_substring_results : list Atom -> list (list Atom * option Value);
  observe_suffix_applicable : bool;
  observe_suffix_results : list Atom -> list (list Atom * option Value)
}.

(** Equality of observations is deliberately extensional.  It includes the
    order of outgoing labels and enumeration results because deterministic
    iteration is part of the public contract. *)
Record SameLogicalObservations {Atom Value : Type}
    (left right : LogicalObservations Atom Value) : Prop := {
  same_membership :
    forall word, observe_membership Atom Value left word =
                 observe_membership Atom Value right word;
  same_terminality :
    forall word, observe_terminality Atom Value left word =
                 observe_terminality Atom Value right word;
  same_mapped_value :
    forall word, observe_mapped_value Atom Value left word =
                 observe_mapped_value Atom Value right word;
  same_ordered_outgoing :
    forall prefix, observe_ordered_outgoing Atom Value left prefix =
                   observe_ordered_outgoing Atom Value right prefix;
  same_prefix_entries :
    forall prefix, observe_prefix_entries Atom Value left prefix =
                   observe_prefix_entries Atom Value right prefix;
  same_full_enumeration :
    observe_full_enumeration Atom Value left =
    observe_full_enumeration Atom Value right;
  same_substring_applicability :
    observe_substring_applicable Atom Value left =
    observe_substring_applicable Atom Value right;
  same_substring_results :
    forall query,
      observe_substring_applicable Atom Value left = true ->
      observe_substring_results Atom Value left query =
      observe_substring_results Atom Value right query;
  same_suffix_applicability :
    observe_suffix_applicable Atom Value left =
    observe_suffix_applicable Atom Value right;
  same_suffix_results :
    forall query,
      observe_suffix_applicable Atom Value left = true ->
      observe_suffix_results Atom Value left query =
      observe_suffix_results Atom Value right query
}.

Lemma same_logical_observations_reflexive :
  forall (Atom Value : Type) (view : LogicalObservations Atom Value),
    SameLogicalObservations view view.
Proof. intros. constructor; intros; reflexivity. Qed.

Lemma same_logical_observations_symmetric :
  forall (Atom Value : Type) (left right : LogicalObservations Atom Value),
    SameLogicalObservations left right ->
    SameLogicalObservations right left.
Proof.
  intros Atom Value left right Hsame.
  destruct Hsame as
    [Hmembership Hterminal Hvalue Houtgoing Hprefix Henumeration
     Hsubstring_app Hsubstring Hsuffix_app Hsuffix].
  constructor.
  - intros. symmetry. auto.
  - intros. symmetry. auto.
  - intros. symmetry. auto.
  - intros. symmetry. auto.
  - intros. symmetry. auto.
  - symmetry. exact Henumeration.
  - symmetry. exact Hsubstring_app.
  - intros query Hright_app. symmetry. apply Hsubstring.
    rewrite Hsubstring_app. exact Hright_app.
  - symmetry. exact Hsuffix_app.
  - intros query Hright_app. symmetry. apply Hsuffix.
    rewrite Hsuffix_app. exact Hright_app.
Qed.

Lemma same_logical_observations_transitive :
  forall (Atom Value : Type)
         (left middle right : LogicalObservations Atom Value),
    SameLogicalObservations left middle ->
    SameLogicalObservations middle right ->
    SameLogicalObservations left right.
Proof.
  intros Atom Value left middle right Hleft Hright.
  destruct Hleft as
    [Hlm Hlt Hlv Hlo Hlp Hle Hlsa Hls Hlsua Hlsu].
  destruct Hright as
    [Hmm Hmt Hmv Hmo Hmp Hme Hmsa Hms Hmsua Hmsu].
  constructor.
  - intros. eauto using eq_trans.
  - intros. eauto using eq_trans.
  - intros. eauto using eq_trans.
  - intros. eauto using eq_trans.
  - intros. eauto using eq_trans.
  - eauto using eq_trans.
  - eauto using eq_trans.
  - intros query Hleft_app.
    assert (Hmiddle_app :
      observe_substring_applicable Atom Value middle = true).
    { rewrite <- Hlsa. exact Hleft_app. }
    eapply eq_trans.
    + now apply Hls.
    + now apply Hms.
  - eauto using eq_trans.
  - intros query Hleft_app.
    assert (Hmiddle_app :
      observe_suffix_applicable Atom Value middle = true).
    { rewrite <- Hlsua. exact Hleft_app. }
    eapply eq_trans.
    + now apply Hlsu.
    + now apply Hmsu.
Qed.

Theorem VWENC_194_LOGICAL_OBSERVATIONAL_EQUIVALENCE_IS_AN_EQUIVALENCE :
  (forall (Atom Value : Type) (view : LogicalObservations Atom Value),
      SameLogicalObservations view view) /\
  (forall (Atom Value : Type)
          (left right : LogicalObservations Atom Value),
      SameLogicalObservations left right ->
      SameLogicalObservations right left) /\
  (forall (Atom Value : Type)
          (left middle right : LogicalObservations Atom Value),
      SameLogicalObservations left middle ->
      SameLogicalObservations middle right ->
      SameLogicalObservations left right).
Proof.
  split.
  - intros. apply same_logical_observations_reflexive.
  - split.
    + intros. now apply same_logical_observations_symmetric.
    + intros. eapply same_logical_observations_transitive; eassumption.
Qed.

Theorem VWENC_195_MEMBERSHIP_AND_TERMINALITY_ARE_LOGICAL_OBSERVATIONS :
  forall (Atom Value : Type)
         (left right : LogicalObservations Atom Value) word,
    SameLogicalObservations left right ->
    observe_membership Atom Value left word =
      observe_membership Atom Value right word /\
    observe_terminality Atom Value left word =
      observe_terminality Atom Value right word.
Proof.
  intros Atom Value left right word Hsame. split.
  - now apply same_membership.
  - now apply same_terminality.
Qed.

Theorem VWENC_196_MAPPED_VALUE_PRESENCE_AND_IDENTITY_ARE_OBSERVABLE :
  forall (Atom Value : Type)
         (left right : LogicalObservations Atom Value) word,
    SameLogicalObservations left right ->
    observe_mapped_value Atom Value left word =
      observe_mapped_value Atom Value right word.
Proof. intros. now apply same_mapped_value. Qed.

Theorem VWENC_197_ORDERED_LOGICAL_OUTGOING_LABELS_ARE_OBSERVABLE :
  forall (Atom Value : Type)
         (left right : LogicalObservations Atom Value) prefix,
    SameLogicalObservations left right ->
    observe_ordered_outgoing Atom Value left prefix =
      observe_ordered_outgoing Atom Value right prefix.
Proof. intros. now apply same_ordered_outgoing. Qed.

Theorem VWENC_198_PREFIX_ENTRIES_ARE_LOGICAL_OBSERVATIONS :
  forall (Atom Value : Type)
         (left right : LogicalObservations Atom Value) prefix,
    SameLogicalObservations left right ->
    observe_prefix_entries Atom Value left prefix =
      observe_prefix_entries Atom Value right prefix.
Proof. intros. now apply same_prefix_entries. Qed.

Theorem VWENC_199_FULL_ENUMERATION_ORDER_IS_DETERMINISTIC_AND_OBSERVABLE :
  forall (Atom Value : Type)
         (left right : LogicalObservations Atom Value),
    SameLogicalObservations left right ->
    observe_full_enumeration Atom Value left =
      observe_full_enumeration Atom Value right.
Proof. intros. now apply same_full_enumeration. Qed.

Theorem VWENC_200_APPLICABLE_SUBSTRING_RESULTS_ARE_LOGICAL_OBSERVATIONS :
  forall (Atom Value : Type)
         (left right : LogicalObservations Atom Value) query,
    SameLogicalObservations left right ->
    observe_substring_applicable Atom Value left = true ->
    observe_substring_applicable Atom Value left =
      observe_substring_applicable Atom Value right /\
    observe_substring_results Atom Value left query =
      observe_substring_results Atom Value right query.
Proof.
  intros Atom Value left right query Hsame Happlicable. split.
  - now apply same_substring_applicability.
  - now apply same_substring_results.
Qed.

Theorem VWENC_201_APPLICABLE_SUFFIX_RESULTS_ARE_LOGICAL_OBSERVATIONS :
  forall (Atom Value : Type)
         (left right : LogicalObservations Atom Value) query,
    SameLogicalObservations left right ->
    observe_suffix_applicable Atom Value left = true ->
    observe_suffix_applicable Atom Value left =
      observe_suffix_applicable Atom Value right /\
    observe_suffix_results Atom Value left query =
      observe_suffix_results Atom Value right query.
Proof.
  intros Atom Value left right query Hsame Happlicable. split.
  - now apply same_suffix_applicability.
  - now apply same_suffix_results.
Qed.

Record PhysicalImplementation (Atom Value : Type) : Type := {
  implementation_logical_view : LogicalObservations Atom Value;
  implementation_node_layout : list nat;
  implementation_staging_bytes : list PhysicalByte;
  implementation_hash_buckets : list nat;
  implementation_wal_bytes : list PhysicalByte
}.

Definition replace_physical_state {Atom Value : Type}
    (implementation : PhysicalImplementation Atom Value)
    (node_layout hash_buckets : list nat)
    (staging_bytes wal_bytes : list PhysicalByte)
    : PhysicalImplementation Atom Value :=
  {| implementation_logical_view :=
       implementation_logical_view Atom Value implementation;
     implementation_node_layout := node_layout;
     implementation_staging_bytes := staging_bytes;
     implementation_hash_buckets := hash_buckets;
     implementation_wal_bytes := wal_bytes |}.

Theorem VWENC_202_PHYSICAL_LAYOUT_AND_CODEC_STAGING_STATE_ARE_NONOBSERVABLE :
  forall (Atom Value : Type)
         (implementation : PhysicalImplementation Atom Value)
         node_layout hash_buckets staging_bytes wal_bytes,
    SameLogicalObservations
      (implementation_logical_view Atom Value implementation)
      (implementation_logical_view Atom Value
        (replace_physical_state implementation node_layout hash_buckets
          staging_bytes wal_bytes)).
Proof. intros. apply same_logical_observations_reflexive. Qed.

(** ** Closed family/profile/surface applicability matrix *)

Inductive DictionaryFamily : Type :=
| DynamicDawgFamily
| DoubleArrayTrieFamily
| SuffixAutomatonFamily
| ScdawgFamily
| PathMapAdapterFamily
| PersistentARTrieFamily
| PersistentSuffixAutomatonFamily
| PersistentSuffixTreeFamily
| PersistentScdawgFamily
| BijectiveMapFamily
| PersistentVocabARTrieFamily.

Definition all_dictionary_families : list DictionaryFamily :=
  [DynamicDawgFamily; DoubleArrayTrieFamily; SuffixAutomatonFamily;
   ScdawgFamily; PathMapAdapterFamily; PersistentARTrieFamily;
   PersistentSuffixAutomatonFamily; PersistentSuffixTreeFamily;
   PersistentScdawgFamily; BijectiveMapFamily;
   PersistentVocabARTrieFamily].

Inductive DirectUnitDomain : Type :=
| DirectBytesDomain
| DirectUnicodeScalarDomain
| DirectU32Domain
| DirectU64Domain
| DirectF64BitsDomain.

Inductive InternedAtomDomain : Type :=
| CanonicalUlebDomain
| CanonicalUtf8Domain
| OpaqueCanonicalBytesDomain.

Inductive IdCarrier : Type :=
| U32IdCarrier
| U64IdCarrier.

Inductive FamilyProfile : Type :=
| DirectProfile : DirectUnitDomain -> FamilyProfile
| InternedProfile : InternedAtomDomain -> IdCarrier -> FamilyProfile.

Definition all_family_profiles : list FamilyProfile :=
  [DirectProfile DirectBytesDomain;
   DirectProfile DirectUnicodeScalarDomain;
   DirectProfile DirectU32Domain;
   DirectProfile DirectU64Domain;
   DirectProfile DirectF64BitsDomain;
   InternedProfile CanonicalUlebDomain U32IdCarrier;
   InternedProfile CanonicalUlebDomain U64IdCarrier;
   InternedProfile CanonicalUtf8Domain U32IdCarrier;
   InternedProfile CanonicalUtf8Domain U64IdCarrier;
   InternedProfile OpaqueCanonicalBytesDomain U32IdCarrier;
   InternedProfile OpaqueCanonicalBytesDomain U64IdCarrier].

(** The logical unit type is a function of the profile.  An implementation
    cannot independently choose an unrelated [Atom] type. *)
Definition direct_codec_profile
    (domain : DirectUnitDomain) : VariableWidthCodecSpec.DirectProfile :=
  match domain with
  | DirectBytesDomain => DirectBytes
  | DirectUnicodeScalarDomain => DirectUnicodeScalar
  | DirectU32Domain => DirectU32
  | DirectU64Domain => DirectU64
  | DirectF64BitsDomain => DirectF64Bits
  end.

Definition u32_id_profile : FixedWidthCarrierProfile :=
  {| carrier_format_identity := 32;
     carrier_width_bytes := 4;
     carrier_width_positive := ltac:(lia) |}.

Definition u64_id_profile : FixedWidthCarrierProfile :=
  {| carrier_format_identity := 64;
     carrier_width_bytes := 8;
     carrier_width_positive := ltac:(lia) |}.

Definition id_carrier_profile
    (carrier : IdCarrier) : FixedWidthCarrierProfile :=
  match carrier with
  | U32IdCarrier => u32_id_profile
  | U64IdCarrier => u64_id_profile
  end.

Definition DirectUnit (domain : DirectUnitDomain) : Type :=
  { unit : nat | direct_profile_valid (direct_codec_profile domain) unit }.

Definition ProfileUnit (profile : FamilyProfile) : Type :=
  match profile with
  | DirectProfile domain => DirectUnit domain
  | InternedProfile _ carrier => SymbolId (id_carrier_profile carrier)
  end.

Inductive ExplicitLayoutContract : Type :=
| GenericLogicalLayout
| PathMapNativeByteLayout
| PathMapUtf8BoundaryLayout
| PathMapFixedWidthBoundaryLayout
| PathMapInternedIdLayout : IdCarrier -> ExplicitLayoutContract
| PersistentU64CompactLayout
| PersistentU64Prefix3CompatibilityLayout
| EncodedU64ByteCompatibilityLayout
| ProspectiveInternedIdLayout : IdCarrier -> ExplicitLayoutContract.

Inductive ProfileRoute : Type :=
| GenericNativeKernel
| RetainedSpecializedKernel
| InternedFixedIdKernel : IdCarrier -> ProfileRoute
| EncodedU64ByteAdapterKernel
| PathMapNativeByteRoute
| PathMapUtf8BoundaryAdapterRoute
| PathMapFixedWidthBoundaryAdapterRoute
| PathMapInternedIdAdapterRoute : IdCarrier -> ProfileRoute
| BijectiveTermValueKernel
| VocabularyOwnerRoute : IdCarrier -> ProfileRoute.

(** Existing behavior and prospective work are deliberately distinct.  There
    is no "unknown" or implicit-support constructor. *)
Inductive ProfileCell : Type :=
| ExistingProfileCell : ProfileRoute -> ExplicitLayoutContract -> ProfileCell
| ProspectiveProfileCell : ProfileRoute -> ExplicitLayoutContract -> ProfileCell.

Definition family_profile_cell
    (family : DictionaryFamily) (profile : FamilyProfile) : ProfileCell :=
  match family, profile with
  | DynamicDawgFamily, DirectProfile DirectBytesDomain
  | DynamicDawgFamily, DirectProfile DirectUnicodeScalarDomain
  | DynamicDawgFamily, DirectProfile DirectU64Domain
  | DoubleArrayTrieFamily, DirectProfile DirectBytesDomain
  | DoubleArrayTrieFamily, DirectProfile DirectUnicodeScalarDomain
  | SuffixAutomatonFamily, DirectProfile DirectBytesDomain
  | SuffixAutomatonFamily, DirectProfile DirectUnicodeScalarDomain
  | ScdawgFamily, DirectProfile DirectBytesDomain
  | ScdawgFamily, DirectProfile DirectUnicodeScalarDomain
  | PersistentSuffixAutomatonFamily, DirectProfile DirectBytesDomain
  | PersistentSuffixAutomatonFamily, DirectProfile DirectUnicodeScalarDomain
  | PersistentSuffixTreeFamily, DirectProfile DirectBytesDomain
  | PersistentSuffixTreeFamily, DirectProfile DirectUnicodeScalarDomain
  | PersistentScdawgFamily, DirectProfile DirectBytesDomain
  | PersistentScdawgFamily, DirectProfile DirectUnicodeScalarDomain =>
      ExistingProfileCell RetainedSpecializedKernel GenericLogicalLayout
  | PersistentARTrieFamily, DirectProfile DirectBytesDomain
  | PersistentARTrieFamily, DirectProfile DirectUnicodeScalarDomain =>
      ExistingProfileCell RetainedSpecializedKernel GenericLogicalLayout
  | PersistentARTrieFamily, DirectProfile DirectU64Domain =>
      ExistingProfileCell RetainedSpecializedKernel PersistentU64CompactLayout
  | PathMapAdapterFamily, DirectProfile DirectBytesDomain =>
      ExistingProfileCell PathMapNativeByteRoute PathMapNativeByteLayout
  | PathMapAdapterFamily, DirectProfile DirectUnicodeScalarDomain =>
      ExistingProfileCell PathMapUtf8BoundaryAdapterRoute
        PathMapUtf8BoundaryLayout
  | BijectiveMapFamily, DirectProfile DirectUnicodeScalarDomain =>
      ExistingProfileCell BijectiveTermValueKernel GenericLogicalLayout
  | PersistentVocabARTrieFamily,
      DirectProfile DirectUnicodeScalarDomain =>
      ExistingProfileCell (VocabularyOwnerRoute U64IdCarrier)
        GenericLogicalLayout
  | PathMapAdapterFamily, DirectProfile _ =>
      ProspectiveProfileCell PathMapFixedWidthBoundaryAdapterRoute
        PathMapFixedWidthBoundaryLayout
  | PathMapAdapterFamily, InternedProfile _ carrier =>
      ProspectiveProfileCell (PathMapInternedIdAdapterRoute carrier)
        (PathMapInternedIdLayout carrier)
  | BijectiveMapFamily, InternedProfile _ carrier
  | PersistentVocabARTrieFamily, InternedProfile _ carrier =>
      ProspectiveProfileCell (VocabularyOwnerRoute carrier)
        (ProspectiveInternedIdLayout carrier)
  | BijectiveMapFamily, DirectProfile _
  | PersistentVocabARTrieFamily, DirectProfile _ =>
      ProspectiveProfileCell BijectiveTermValueKernel GenericLogicalLayout
  | _, DirectProfile _ =>
      ProspectiveProfileCell GenericNativeKernel GenericLogicalLayout
  | _, InternedProfile _ carrier =>
      ProspectiveProfileCell (InternedFixedIdKernel carrier)
        (ProspectiveInternedIdLayout carrier)
  end.

Definition profile_cell_route (cell : ProfileCell) : ProfileRoute :=
  match cell with
  | ExistingProfileCell route _ | ProspectiveProfileCell route _ => route
  end.

Definition profile_cell_layout
    (cell : ProfileCell) : ExplicitLayoutContract :=
  match cell with
  | ExistingProfileCell _ layout | ProspectiveProfileCell _ layout => layout
  end.

Definition family_profile_route
    (family : DictionaryFamily) (profile : FamilyProfile) : ProfileRoute :=
  profile_cell_route (family_profile_cell family profile).

Inductive ConsumerSurfaceClass : Type :=
| DictionarySurface
| DictionaryNodeSurfaceClass
| ZipperSurfaceClass
| SnapshotCursorSurfaceClass
| FactorySurface
| CollectionSurface
| SerializationReopenSurface
| SnapshotSurface
| SetCombinatorSurface
| ValueCombinatorSurface
| PrefixSurface
| SubstringSurface
| SuffixSurface
| ReverseLookupSurface.

Definition all_consumer_surfaces : list ConsumerSurfaceClass :=
  [DictionarySurface; DictionaryNodeSurfaceClass; ZipperSurfaceClass;
   SnapshotCursorSurfaceClass; FactorySurface; CollectionSurface;
   SerializationReopenSurface; SnapshotSurface; SetCombinatorSurface;
   ValueCombinatorSurface; PrefixSurface; SubstringSurface; SuffixSurface;
   ReverseLookupSurface].

Inductive SurfaceRoute : Type :=
| CommonDictionaryRoute
| SuffixIndexRoute
| VocabularyReverseLookupRoute.

Inductive SurfaceInapplicability : Type :=
| ExactTermFamilyHasNoSubstringIndex
| TermIndexHasNoVocabularyReverseLookup
| VocabularyOwnerHasNoSuffixIndex
| PersistentConstructionRequiresExplicitStoreConfiguration.

Inductive SurfaceCell : Type :=
| ExistingSurface : SurfaceRoute -> SurfaceCell
| ProspectiveSurface : SurfaceRoute -> SurfaceCell
| SurfaceStructurallyInapplicable : SurfaceInapplicability -> SurfaceCell.

Definition persistent_family (family : DictionaryFamily) : bool :=
  match family with
  | PersistentARTrieFamily | PersistentSuffixAutomatonFamily
  | PersistentSuffixTreeFamily | PersistentScdawgFamily
  | PersistentVocabARTrieFamily => true
  | _ => false
  end.

Definition family_surface_cell
    (family : DictionaryFamily) (surface : ConsumerSurfaceClass)
    : SurfaceCell :=
  match surface with
  | ReverseLookupSurface =>
      match family with
      | BijectiveMapFamily | PersistentVocabARTrieFamily =>
          ExistingSurface VocabularyReverseLookupRoute
      | _ => SurfaceStructurallyInapplicable
               TermIndexHasNoVocabularyReverseLookup
      end
  | SubstringSurface | SuffixSurface =>
      match family with
      | SuffixAutomatonFamily | ScdawgFamily
      | PersistentSuffixAutomatonFamily | PersistentSuffixTreeFamily
      | PersistentScdawgFamily => ExistingSurface SuffixIndexRoute
      | BijectiveMapFamily | PersistentVocabARTrieFamily =>
          SurfaceStructurallyInapplicable VocabularyOwnerHasNoSuffixIndex
      | _ => SurfaceStructurallyInapplicable
               ExactTermFamilyHasNoSubstringIndex
      end
  | FactorySurface =>
      match family with
      | PersistentARTrieFamily | PersistentSuffixAutomatonFamily
      | PersistentSuffixTreeFamily | PersistentScdawgFamily
      | PersistentVocabARTrieFamily =>
          SurfaceStructurallyInapplicable
            PersistentConstructionRequiresExplicitStoreConfiguration
      | BijectiveMapFamily => ProspectiveSurface CommonDictionaryRoute
      | _ => ExistingSurface CommonDictionaryRoute
      end
  | SerializationReopenSurface =>
      if persistent_family family then ExistingSurface CommonDictionaryRoute
      else ProspectiveSurface CommonDictionaryRoute
  | DictionarySurface | DictionaryNodeSurfaceClass
  | SnapshotCursorSurfaceClass | SnapshotSurface | PrefixSurface =>
      ExistingSurface CommonDictionaryRoute
  | CollectionSurface => ExistingSurface CommonDictionaryRoute
  | ZipperSurfaceClass | SetCombinatorSurface | ValueCombinatorSurface =>
      match family with
      | DynamicDawgFamily | DoubleArrayTrieFamily | SuffixAutomatonFamily
      | PathMapAdapterFamily | PersistentARTrieFamily =>
          ExistingSurface CommonDictionaryRoute
      | _ => ProspectiveSurface CommonDictionaryRoute
      end
  end.

Inductive CapabilityInapplicability : Type :=
| SurfaceCapabilityReason : SurfaceInapplicability ->
    CapabilityInapplicability.

Inductive CapabilityCell : Type :=
| ExistingCapability : ProfileRoute -> ExplicitLayoutContract ->
    SurfaceRoute -> CapabilityCell
| ProspectiveCapability : ProfileRoute -> ExplicitLayoutContract ->
    SurfaceRoute -> CapabilityCell
| CapabilityStructurallyInapplicable :
    CapabilityInapplicability -> CapabilityCell.

Definition family_profile_surface_cell
    (family : DictionaryFamily) (profile : FamilyProfile)
    (surface : ConsumerSurfaceClass) : CapabilityCell :=
  match family_profile_cell family profile,
        family_surface_cell family surface with
  | ExistingProfileCell route layout, ExistingSurface surface_route =>
      ExistingCapability route layout surface_route
  | ExistingProfileCell route layout, ProspectiveSurface surface_route
  | ProspectiveProfileCell route layout, ExistingSurface surface_route
  | ProspectiveProfileCell route layout, ProspectiveSurface surface_route =>
      ProspectiveCapability route layout surface_route
  | _, SurfaceStructurallyInapplicable reason =>
      CapabilityStructurallyInapplicable (SurfaceCapabilityReason reason)
  end.

Theorem VWENC_203_DICTIONARY_FAMILY_INVENTORY_IS_EXHAUSTIVE :
  length all_dictionary_families = 11 /\
  forall family, In family all_dictionary_families.
Proof.
  split; [reflexivity |].
  intros family. destruct family; simpl; tauto.
Qed.

Theorem VWENC_204_FAMILY_PROFILE_MATRIX_IS_TOTAL_AND_FUNCTIONAL :
  length all_family_profiles = 11 /\
  (forall profile, In profile all_family_profiles) /\
  forall family profile,
    (exists route layout,
        family_profile_cell family profile =
          ExistingProfileCell route layout) \/
    (exists route layout,
        family_profile_cell family profile =
          ProspectiveProfileCell route layout).
Proof.
  split; [reflexivity |]. split.
  - intros [direct | domain carrier].
    + destruct direct; simpl; tauto.
    + destruct domain, carrier; simpl; tauto.
  - intros family profile.
    destruct (family_profile_cell family profile) as [route layout|route layout]
      eqn:Hcell.
    + left. now exists route, layout.
    + right. now exists route, layout.
Qed.

Theorem VWENC_205_FAMILY_SURFACE_MATRIX_IS_TOTAL_AND_FUNCTIONAL :
  length all_consumer_surfaces = 14 /\
  (forall surface, In surface all_consumer_surfaces) /\
  forall family surface,
    (exists route,
        family_surface_cell family surface = ExistingSurface route) \/
    (exists route,
        family_surface_cell family surface = ProspectiveSurface route) \/
    (exists reason,
        family_surface_cell family surface =
          SurfaceStructurallyInapplicable reason).
Proof.
  split; [reflexivity |]. split.
  - intros surface. destruct surface; simpl; tauto.
  - intros family surface.
    destruct (family_surface_cell family surface) as [route|route|reason]
      eqn:Hcell.
    + left. now exists route.
    + right. left. now exists route.
    + right. right. now exists reason.
Qed.

Theorem VWENC_206_FAMILY_PROFILE_SURFACE_MATRIX_IS_TOTAL :
  forall family profile surface,
    (exists route layout surface_route,
        family_profile_surface_cell family profile surface =
          ExistingCapability route layout surface_route) \/
    (exists route layout surface_route,
        family_profile_surface_cell family profile surface =
          ProspectiveCapability route layout surface_route) \/
    (exists reason,
        family_profile_surface_cell family profile surface =
          CapabilityStructurallyInapplicable reason).
Proof.
  intros family profile surface.
  destruct (family_profile_surface_cell family profile surface) as
    [route layout surface_route|route layout surface_route|reason] eqn:Hcell.
  - left. now exists route, layout, surface_route.
  - right. left. now exists route, layout, surface_route.
  - right. right. now exists reason.
Qed.

Theorem VWENC_207_EVERY_INAPPLICABLE_CELL_HAS_AN_EXPLICIT_STRUCTURAL_REASON :
  forall family profile surface,
    (exists reason,
        family_profile_surface_cell family profile surface =
          CapabilityStructurallyInapplicable
            (SurfaceCapabilityReason reason)) ->
    exists reason,
      family_surface_cell family surface =
        SurfaceStructurallyInapplicable reason.
Proof.
  intros family profile surface [reason Hcell].
  unfold family_profile_surface_cell in Hcell.
  destruct (family_profile_cell family profile);
    destruct (family_surface_cell family surface) eqn:Hsurface;
    inversion Hcell; subst; eauto.
Qed.

Definition pathmap_adapter_route (route : ProfileRoute) : Prop :=
  match route with
  | PathMapNativeByteRoute
  | PathMapUtf8BoundaryAdapterRoute
  | PathMapFixedWidthBoundaryAdapterRoute
  | PathMapInternedIdAdapterRoute _ => True
  | _ => False
  end.

Theorem VWENC_208_PATHMAP_REMAINS_AN_EXTERNAL_BYTE_KEYED_ADAPTER :
  forall profile,
    pathmap_adapter_route
      (profile_cell_route
        (family_profile_cell PathMapAdapterFamily profile)).
Proof.
  intros [direct | domain carrier].
  - destruct direct; exact I.
  - destruct domain, carrier; exact I.
Qed.

Definition family_profile_logical_domain
    (profile : FamilyProfile) : option InternedAtomDomain :=
  match profile with
  | DirectProfile _ => None
  | InternedProfile domain _ => Some domain
  end.

Theorem VWENC_209_PATHMAP_CANONICAL_ULEB_USES_ONLY_FIXED_WIDTH_INTERNED_IDS :
  forall profile,
    family_profile_logical_domain profile = Some CanonicalUlebDomain ->
    exists carrier,
      profile = InternedProfile CanonicalUlebDomain carrier /\
      family_profile_cell PathMapAdapterFamily profile =
        ProspectiveProfileCell (PathMapInternedIdAdapterRoute carrier)
          (PathMapInternedIdLayout carrier) /\
      carrier_width_bytes (id_carrier_profile carrier) =
        match carrier with U32IdCarrier => 4 | U64IdCarrier => 8 end.
Proof.
  intros [direct | domain carrier] Hdomain.
  - discriminate.
  - destruct domain; inversion Hdomain; subst.
    exists carrier. repeat split; destruct carrier; reflexivity.
Qed.

(** ** Naming, persistent identity, and specialization *)

Inductive FamilyTypeSpelling : Type :=
| CanonicalFamilySpelling : DictionaryFamily -> FamilyProfile ->
    FamilyTypeSpelling
| LegacyOneParameterSpelling : DictionaryFamily -> FamilyTypeSpelling.

Definition legacy_default_profile (family : DictionaryFamily) : FamilyProfile :=
  match family with
  | BijectiveMapFamily | PersistentVocabARTrieFamily =>
      DirectProfile DirectUnicodeScalarDomain
  | _ => DirectProfile DirectBytesDomain
  end.

Definition legacy_family_defaults_to_bytes (family : DictionaryFamily) : bool :=
  match family with
  | BijectiveMapFamily | PersistentVocabARTrieFamily => false
  | _ => true
  end.

Definition normalize_family_spelling
    (spelling : FamilyTypeSpelling) : DictionaryFamily * FamilyProfile :=
  match spelling with
  | CanonicalFamilySpelling family profile => (family, profile)
  | LegacyOneParameterSpelling family =>
      (family, legacy_default_profile family)
  end.

Theorem VWENC_210_LEGACY_ONE_PARAMETER_FAMILY_SPELLING_DEFAULTS_TO_BYTES :
  (forall family,
    legacy_family_defaults_to_bytes family = true ->
      normalize_family_spelling (LegacyOneParameterSpelling family) =
      normalize_family_spelling
        (CanonicalFamilySpelling family
          (DirectProfile DirectBytesDomain))) /\
  normalize_family_spelling (LegacyOneParameterSpelling BijectiveMapFamily) =
    (BijectiveMapFamily, DirectProfile DirectUnicodeScalarDomain) /\
  normalize_family_spelling
      (LegacyOneParameterSpelling PersistentVocabARTrieFamily) =
    (PersistentVocabARTrieFamily, DirectProfile DirectUnicodeScalarDomain).
Proof.
  split.
  - intros family Hbyte. destruct family; simpl in *; try reflexivity;
      discriminate.
  - split; reflexivity.
Qed.

Inductive GenericParameterSlot : Type :=
| MappedValueParameterSlot
| LogicalProfileParameterSlot
| RedundantWidthParameterSlot.

Definition canonical_family_parameter_order : list GenericParameterSlot :=
  [MappedValueParameterSlot; LogicalProfileParameterSlot].

Theorem VWENC_211_MAPPED_VALUE_REMAINS_FIRST_AND_WIDTH_IS_NOT_A_PARAMETER :
  hd_error canonical_family_parameter_order =
    Some MappedValueParameterSlot /\
  nth_error canonical_family_parameter_order 1 =
    Some LogicalProfileParameterSlot /\
  ~ In RedundantWidthParameterSlot canonical_family_parameter_order.
Proof.
  split; [reflexivity |].
  split; [reflexivity |].
  simpl. intuition discriminate.
Qed.

Inductive EdgeUnitKind : Type :=
| ByteEdgeUnit
| UnicodeScalarEdgeUnit
| U32EdgeUnit
| U64EdgeUnit
| F64BitsEdgeUnit
| SymbolIdEdgeUnit : IdCarrier -> EdgeUnitKind.

Definition id_carrier_width (carrier : IdCarrier) : nat :=
  match carrier with U32IdCarrier => 4 | U64IdCarrier => 8 end.

Definition profile_edge_contract
    (profile : FamilyProfile) : EdgeUnitKind * nat :=
  match profile with
  | DirectProfile DirectBytesDomain => (ByteEdgeUnit, 1)
  | DirectProfile DirectUnicodeScalarDomain => (UnicodeScalarEdgeUnit, 4)
  | DirectProfile DirectU32Domain => (U32EdgeUnit, 4)
  | DirectProfile DirectU64Domain => (U64EdgeUnit, 8)
  | DirectProfile DirectF64BitsDomain => (F64BitsEdgeUnit, 8)
  | InternedProfile _ carrier =>
      (SymbolIdEdgeUnit carrier, id_carrier_width carrier)
  end.

Theorem VWENC_212_PROFILE_ALONE_OWNS_EDGE_UNIT_AND_WIDTH_METADATA :
  forall profile,
    exists! contract,
      contract = profile_edge_contract profile /\
      0 < snd contract.
Proof.
  intros profile.
  exists (profile_edge_contract profile). split.
  - split; [reflexivity |].
    destruct profile as [direct | domain carrier].
    + destruct direct; simpl; lia.
    + destruct carrier; simpl; lia.
  - intros contract [Hcontract _]. symmetry. exact Hcontract.
Qed.

Inductive FamilyCodecIdentity : Type :=
| FamilyExistingByteCodec
| FamilyExistingCharU32Codec
| FamilyExistingNativeU64Codec
| FamilyEncodedU64LittleEndianBytePathCodec
| FamilyProspectiveU32CodecV1
| FamilyProspectiveF64BitsCodecV1
| FamilyFixedIdCarrierCodec : IdCarrier -> FamilyCodecIdentity.

Definition codec_matches_profile
    (profile : FamilyProfile) (codec : FamilyCodecIdentity) : Prop :=
  match profile, codec with
  | DirectProfile DirectBytesDomain, FamilyExistingByteCodec
  | DirectProfile DirectUnicodeScalarDomain, FamilyExistingCharU32Codec
  | DirectProfile DirectU64Domain, FamilyExistingNativeU64Codec
  | DirectProfile DirectU64Domain,
      FamilyEncodedU64LittleEndianBytePathCodec
  | DirectProfile DirectU32Domain, FamilyProspectiveU32CodecV1
  | DirectProfile DirectF64BitsDomain, FamilyProspectiveF64BitsCodecV1 => True
  | InternedProfile _ expected, FamilyFixedIdCarrierCodec actual =>
      expected = actual
  | _, _ => False
  end.

Definition layout_matches_codec
    (codec : FamilyCodecIdentity) (layout : ExplicitLayoutContract) : Prop :=
  match codec, layout with
  | FamilyExistingByteCodec, GenericLogicalLayout
  | FamilyExistingByteCodec, PathMapNativeByteLayout
  | FamilyExistingCharU32Codec, GenericLogicalLayout
  | FamilyExistingCharU32Codec, PathMapUtf8BoundaryLayout
  | FamilyExistingNativeU64Codec, GenericLogicalLayout
  | FamilyExistingNativeU64Codec, PersistentU64CompactLayout
  | FamilyExistingNativeU64Codec,
      PersistentU64Prefix3CompatibilityLayout
  | FamilyEncodedU64LittleEndianBytePathCodec,
      EncodedU64ByteCompatibilityLayout
  | FamilyProspectiveU32CodecV1, GenericLogicalLayout
  | FamilyProspectiveU32CodecV1, PathMapFixedWidthBoundaryLayout
  | FamilyProspectiveF64BitsCodecV1, GenericLogicalLayout
  | FamilyProspectiveF64BitsCodecV1, PathMapFixedWidthBoundaryLayout => True
  | FamilyFixedIdCarrierCodec expected, ProspectiveInternedIdLayout actual
  | FamilyFixedIdCarrierCodec expected, PathMapInternedIdLayout actual =>
      expected = actual
  | _, _ => False
  end.

Definition backend_matches_layout
    (family : DictionaryFamily) (layout : ExplicitLayoutContract) : Prop :=
  match layout with
  | PathMapNativeByteLayout | PathMapUtf8BoundaryLayout
  | PathMapFixedWidthBoundaryLayout | PathMapInternedIdLayout _ =>
      family = PathMapAdapterFamily
  | PersistentU64CompactLayout
  | PersistentU64Prefix3CompatibilityLayout
  | EncodedU64ByteCompatibilityLayout =>
      family = PersistentARTrieFamily
  | GenericLogicalLayout | ProspectiveInternedIdLayout _ => True
  end.

(** Persistent/ABI certification is intentionally stricter than the abstract
    codec/layout compatibility relations above.  A format may be certified
    only for an existing family/profile cell and its exact declared layout.
    The two historical PersistentARTrie U64 byte layouts are explicit,
    reviewed compatibility exceptions; prospective cells cannot mint a
    persistent identity. *)
Definition family_profile_layout_is_certifiable
    (family : DictionaryFamily) (profile : FamilyProfile)
    (codec : FamilyCodecIdentity) (layout : ExplicitLayoutContract) : Prop :=
  (exists route,
      family_profile_cell family profile = ExistingProfileCell route layout) \/
  (family = PersistentARTrieFamily /\
   profile = DirectProfile DirectU64Domain /\
   codec = FamilyExistingNativeU64Codec /\
   layout = PersistentU64Prefix3CompatibilityLayout) \/
  (family = PersistentARTrieFamily /\
   profile = DirectProfile DirectU64Domain /\
   codec = FamilyEncodedU64LittleEndianBytePathCodec /\
   layout = EncodedU64ByteCompatibilityLayout).

Record CertifiedFamilyFormat : Type := {
  certified_format_family : DictionaryFamily;
  certified_format_profile : FamilyProfile;
  certified_format_codec : FamilyCodecIdentity;
  certified_format_layout : ExplicitLayoutContract;
  certified_format_abi_version : nat;
  certified_format_profile_codec_coherent :
    codec_matches_profile certified_format_profile certified_format_codec;
  certified_format_codec_layout_coherent :
    layout_matches_codec certified_format_codec certified_format_layout;
  certified_format_backend_layout_coherent :
    backend_matches_layout certified_format_family certified_format_layout;
  certified_format_family_profile_coherent :
    family_profile_layout_is_certifiable
      certified_format_family certified_format_profile
      certified_format_codec certified_format_layout;
  certified_format_version_positive : 0 < certified_format_abi_version
}.

Definition CertifiedProfileIdentity : Type :=
  DictionaryFamily *
    (FamilyProfile *
      (FamilyCodecIdentity * (ExplicitLayoutContract * nat))).

Definition certified_family_format_identity
    (descriptor : CertifiedFamilyFormat) : CertifiedProfileIdentity :=
  (certified_format_family descriptor,
   (certified_format_profile descriptor,
    (certified_format_codec descriptor,
     (certified_format_layout descriptor,
      certified_format_abi_version descriptor)))).

Inductive ProfileReference : Type :=
| OpenInMemoryProfileReference
| CertifiedPersistentProfileReference : CertifiedFamilyFormat ->
    ProfileReference.

Definition persistent_identity_of
    (reference : ProfileReference) : option CertifiedProfileIdentity :=
  match reference with
  | OpenInMemoryProfileReference => None
  | CertifiedPersistentProfileReference descriptor =>
      Some (certified_family_format_identity descriptor)
  end.

Theorem VWENC_213_OPEN_IN_MEMORY_UNITS_CANNOT_MINT_PERSISTENT_IDENTITIES :
  persistent_identity_of OpenInMemoryProfileReference = None /\
  forall descriptor,
    persistent_identity_of
      (CertifiedPersistentProfileReference descriptor) =
      Some (certified_family_format_identity descriptor) /\
    0 < certified_format_abi_version descriptor.
Proof.
  split; [reflexivity |].
  intros descriptor. split; [reflexivity |].
  exact (certified_format_version_positive descriptor).
Qed.

(** Rust spelling is diagnostic text only.  All semantic fields, including
    codec and layout, come from the certified descriptor. *)
Definition format_identity_with_rust_name
    (_rust_type_name : list nat) (descriptor : CertifiedFamilyFormat)
    : CertifiedProfileIdentity :=
  certified_family_format_identity descriptor.

Lemma certified_family_format_identity_injective :
  forall left right,
    certified_family_format_identity left =
      certified_family_format_identity right ->
    left = right.
Proof.
  intros
    [lf lp lc ll lv lpc lcl lbl lfp lvp]
    [rf rp rc rl rv rpc rcl rbl rfp rvp] Hequal.
  unfold certified_family_format_identity in Hequal. simpl in Hequal.
  inversion Hequal. subst.
  f_equal; apply proof_irrelevance.
Qed.

Theorem VWENC_214_FORMAT_IDENTITY_IS_INDEPENDENT_OF_RUST_TYPE_NAMES :
  (forall left_name right_name descriptor,
    format_identity_with_rust_name left_name descriptor =
    format_identity_with_rust_name right_name descriptor) /\
  (forall left right,
    certified_family_format_identity left =
      certified_family_format_identity right ->
    left = right).
Proof.
  split; [reflexivity |].
  exact certified_family_format_identity_injective.
Qed.

Inductive KernelKind : Type :=
| GenericLogicalKernel
| SpecializedLogicalKernel
| FixedIdLogicalKernel : IdCarrier -> KernelKind
| EncodedU64AdapterLogicalKernel
| PathMapAdapterKernel
| BijectiveLogicalKernel
| VocabularyOwnerKernel.

Definition kernel_for_profile_route (route : ProfileRoute) : KernelKind :=
  match route with
  | GenericNativeKernel => GenericLogicalKernel
  | RetainedSpecializedKernel => SpecializedLogicalKernel
  | InternedFixedIdKernel carrier => FixedIdLogicalKernel carrier
  | EncodedU64ByteAdapterKernel => EncodedU64AdapterLogicalKernel
  | PathMapNativeByteRoute | PathMapUtf8BoundaryAdapterRoute
  | PathMapFixedWidthBoundaryAdapterRoute
  | PathMapInternedIdAdapterRoute _ => PathMapAdapterKernel
  | BijectiveTermValueKernel => BijectiveLogicalKernel
  | VocabularyOwnerRoute _ => VocabularyOwnerKernel
  end.

Definition selected_kernel
    (family : DictionaryFamily) (profile : FamilyProfile) : KernelKind :=
  kernel_for_profile_route (family_profile_route family profile).

(** A nominal interned profile is usable only together with the certified atom
    profile, expected and actual vocabulary fibers, their equality proof, and
    the exact immutable vocabulary snapshot.  This is a type-level prerequisite
    of every family snapshot, rather than an optional hot-path side channel. *)
Definition atom_profile_matches_interned_domain
    (domain : InternedAtomDomain) (profile : CertifiedAtomProfile) : Prop :=
  match domain with
  | CanonicalUlebDomain =>
      persistent_logical_profile (atom_profile_descriptor profile) =
        PersistedCanonicalUleb
  | CanonicalUtf8Domain =>
      persistent_logical_profile (atom_profile_descriptor profile) =
        PersistedCanonicalUtf8
  | OpaqueCanonicalBytesDomain => True
  end.

Record InternedConsumerContext
    (domain : InternedAtomDomain) (carrier : IdCarrier) : Type := {
  interned_context_atom_profile : CertifiedAtomProfile;
  interned_context_atom_profile_exact :
    atom_profile_matches_interned_domain
      domain interned_context_atom_profile;
  interned_context_expected_fiber :
    VocabularyFiber interned_context_atom_profile
      (id_carrier_profile carrier);
  interned_context_actual_fiber :
    VocabularyFiber interned_context_atom_profile
      (id_carrier_profile carrier);
  interned_context_fiber_exact :
    interned_context_expected_fiber = interned_context_actual_fiber;
  interned_context_snapshot :
    VocabularySnapshot interned_context_atom_profile
      (id_carrier_profile carrier) interned_context_actual_fiber
}.

Definition FamilyConsumerContext (profile : FamilyProfile) : Type :=
  match profile with
  | DirectProfile _ => unit
  | InternedProfile domain carrier => InternedConsumerContext domain carrier
  end.

(** Runtime payload is one fixed-width ID plus an erased proof of membership in
    the exact bound snapshot.  It does not duplicate the arbitrary-width atom. *)
Record SnapshotBoundSymbolId
    (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (fiber : VocabularyFiber P I)
    (snapshot : VocabularySnapshot P I fiber) : Type := {
  snapshot_bound_symbol_id : SymbolId I;
  snapshot_bound_live :
    live_symbol (vocabulary_snapshot_live_entries P I fiber snapshot)
      snapshot_bound_symbol_id
}.

Definition BoundProfileUnit
    (profile : FamilyProfile) (context : FamilyConsumerContext profile) : Type.
Proof.
  destruct profile as [domain | domain carrier].
  - exact (DirectUnit domain).
  - exact
      (SnapshotBoundSymbolId
        (interned_context_atom_profile domain carrier context)
        (id_carrier_profile carrier)
        (interned_context_actual_fiber domain carrier context)
        (interned_context_snapshot domain carrier context)).
Defined.

Definition snapshot_route_allowed
    (family : DictionaryFamily) (profile : FamilyProfile)
    (kernel : KernelKind) (layout : ExplicitLayoutContract) : Prop :=
  (kernel = GenericLogicalKernel /\ layout = GenericLogicalLayout) \/
  (kernel = selected_kernel family profile /\
   layout = profile_cell_layout (family_profile_cell family profile)) \/
  (family = PersistentARTrieFamily /\
   profile = DirectProfile DirectU64Domain /\
   kernel = EncodedU64AdapterLogicalKernel /\
   layout = EncodedU64ByteCompatibilityLayout).

Record FamilySnapshot
    (family : DictionaryFamily) (profile : FamilyProfile)
    (context : FamilyConsumerContext profile) (Value : Type) : Type := {
  snapshot_revision : nat;
  snapshot_kernel : KernelKind;
  snapshot_layout : ExplicitLayoutContract;
  snapshot_observations : LogicalObservations
    (BoundProfileUnit profile context) Value;
  snapshot_route_certificate :
    snapshot_route_allowed family profile snapshot_kernel snapshot_layout
}.

(** Downstream [CharUnit]-like implementations remain open in memory.  They
    share the same family observation contract, but their profile reference is
    definitionally non-persistent and therefore cannot be mistaken for a
    certified ABI identity. *)
Record OpenFamilySnapshot
    (family : DictionaryFamily) (Unit Value : Type)
    (profile : OpenUnitProfile Unit) : Type := {
  open_snapshot_revision : nat;
  open_snapshot_observations : LogicalObservations Unit Value
}.

Definition open_family_snapshot_profile_reference
    {family Unit Value} {profile : OpenUnitProfile Unit}
    (_snapshot : OpenFamilySnapshot family Unit Value profile)
    : ProfileReference := OpenInMemoryProfileReference.

Lemma open_family_snapshots_cannot_mint_persistent_identity :
  forall family Unit Value (profile : OpenUnitProfile Unit)
         (snapshot : OpenFamilySnapshot family Unit Value profile),
    persistent_identity_of
      (open_family_snapshot_profile_reference snapshot) = None.
Proof. reflexivity. Qed.

Record OpenFamilySpecializationRefinement
    (family : DictionaryFamily) (Unit Value : Type)
    (profile : OpenUnitProfile Unit)
    (generic specialized : OpenFamilySnapshot family Unit Value profile)
    : Prop := {
  open_specialization_same_revision :
    open_snapshot_revision _ _ _ _ generic =
      open_snapshot_revision _ _ _ _ specialized;
  open_specialization_same_observations :
    SameLogicalObservations
      (open_snapshot_observations _ _ _ _ generic)
      (open_snapshot_observations _ _ _ _ specialized)
}.

Record SpecializationRefinement
    (family : DictionaryFamily) (profile : FamilyProfile)
    (context : FamilyConsumerContext profile) (Value : Type)
    (generic specialized : FamilySnapshot family profile context Value) : Prop := {
  specialization_generic_kernel :
    snapshot_kernel family profile context Value generic = GenericLogicalKernel;
  specialization_selected_kernel :
    snapshot_kernel family profile context Value specialized =
      selected_kernel family profile;
  specialization_same_revision :
    snapshot_revision family profile context Value generic =
      snapshot_revision family profile context Value specialized;
  specialization_same_observations :
    SameLogicalObservations
      (snapshot_observations family profile context Value generic)
      (snapshot_observations family profile context Value specialized)
}.

Theorem VWENC_215_SPECIALIZATION_REFINES_THE_GENERIC_LOGICAL_VIEW :
  forall family profile (context : FamilyConsumerContext profile) (Value : Type)
         (generic specialized : FamilySnapshot family profile context Value),
    SpecializationRefinement family profile context Value generic specialized ->
    snapshot_revision family profile context Value generic =
      snapshot_revision family profile context Value specialized /\
    snapshot_kernel family profile context Value generic = GenericLogicalKernel /\
    snapshot_kernel family profile context Value specialized =
      selected_kernel family profile /\
    SameLogicalObservations
      (snapshot_observations family profile context Value generic)
      (snapshot_observations family profile context Value specialized).
Proof.
  intros family profile context Value generic specialized Hrefinement.
  split.
  - exact (specialization_same_revision _ _ _ _ _ _ Hrefinement).
  - split.
    + exact (specialization_generic_kernel _ _ _ _ _ _ Hrefinement).
    + split.
      * exact (specialization_selected_kernel _ _ _ _ _ _ Hrefinement).
      * exact (specialization_same_observations _ _ _ _ _ _ Hrefinement).
Qed.

Theorem VWENC_216_EVERY_RETAINED_SPECIALIZED_KERNEL_PRESERVES_ALL_OBSERVATIONS :
  forall family profile (context : FamilyConsumerContext profile) (Value : Type)
         (generic specialized : FamilySnapshot family profile context Value),
    family_profile_route family profile = RetainedSpecializedKernel ->
    SpecializationRefinement family profile context Value generic specialized ->
    snapshot_kernel family profile context Value specialized =
      SpecializedLogicalKernel /\
    SameLogicalObservations
      (snapshot_observations family profile context Value generic)
      (snapshot_observations family profile context Value specialized).
Proof.
  intros family profile context Value generic specialized Hroute Hrefinement.
  split.
  - rewrite (specialization_selected_kernel _ _ _ _ _ _ Hrefinement).
    unfold selected_kernel. now rewrite Hroute.
  - exact (specialization_same_observations _ _ _ _ _ _ Hrefinement).
Qed.

(** A traversal batch is homogeneous in [Unit].  Kernel selection happens when
    this record is built; its encoder cannot inspect a profile tag per edge. *)
Record MonomorphicFixedWidthKernel (Unit : Type) : Type := {
  monomorphic_width : nat;
  monomorphic_width_positive : 0 < monomorphic_width;
  monomorphic_encode : Unit -> list PhysicalByte;
  monomorphic_encode_exact :
    forall unit, length (monomorphic_encode unit) = monomorphic_width;
  monomorphic_variable_decode_request : Unit -> option (list PhysicalByte);
  monomorphic_has_no_variable_decode :
    forall unit, monomorphic_variable_decode_request unit = None
}.

Definition run_bound_kernel {Unit : Type}
    (kernel : MonomorphicFixedWidthKernel Unit) (units : list Unit)
    : list (list PhysicalByte) :=
  map (monomorphic_encode Unit kernel) units.

Lemma bound_kernel_widths_are_constant :
  forall (Unit : Type) (kernel : MonomorphicFixedWidthKernel Unit) units,
    map (@length PhysicalByte) (run_bound_kernel kernel units) =
      repeat (monomorphic_width Unit kernel) (length units).
Proof.
  intros Unit kernel units. induction units as [|unit rest IH]; simpl.
  - reflexivity.
  - rewrite (monomorphic_encode_exact Unit kernel unit), IH. reflexivity.
Qed.

Theorem VWENC_217_KERNEL_SELECTION_IS_BOUND_ONCE_NOT_BRANCHING_PER_EDGE :
  forall (Unit : Type) (kernel : MonomorphicFixedWidthKernel Unit) units,
    run_bound_kernel kernel units =
      map (monomorphic_encode Unit kernel) units /\
    map (@length PhysicalByte) (run_bound_kernel kernel units) =
      repeat (monomorphic_width Unit kernel) (length units).
Proof.
  intros. split; [reflexivity |].
  apply bound_kernel_widths_are_constant.
Qed.

(** ** Backward-compatible aliases and independent projections *)

Inductive LegacyAlias : Type :=
| LegacyDynamicDawg | LegacyDoubleArrayTrie | LegacySuffixAutomaton
| LegacyScdawg | LegacyPathMapDictionary | LegacyPersistentARTrie
| LegacyPersistentSuffixAutomaton | LegacyPersistentSuffixTree
| LegacyPersistentScdawg
| LegacyDynamicDawgChar | LegacyDoubleArrayTrieChar
| LegacySuffixAutomatonChar | LegacyScdawgChar
| LegacyPathMapDictionaryChar | LegacyPersistentARTrieChar
| LegacyPersistentSuffixAutomatonChar | LegacyPersistentSuffixTreeChar
| LegacyPersistentScdawgChar
| LegacyDynamicDawgU64 | LegacyPersistentARTrieU64
| LegacyPersistentARTrieU64Compact
| LegacyPersistentARTrieU64Prefix3Compat
| LegacyEncodedPersistentARTrieU64.

Definition all_legacy_aliases : list LegacyAlias :=
  [LegacyDynamicDawg; LegacyDoubleArrayTrie; LegacySuffixAutomaton;
   LegacyScdawg; LegacyPathMapDictionary; LegacyPersistentARTrie;
   LegacyPersistentSuffixAutomaton; LegacyPersistentSuffixTree;
   LegacyPersistentScdawg; LegacyDynamicDawgChar;
   LegacyDoubleArrayTrieChar; LegacySuffixAutomatonChar;
   LegacyScdawgChar; LegacyPathMapDictionaryChar;
   LegacyPersistentARTrieChar; LegacyPersistentSuffixAutomatonChar;
   LegacyPersistentSuffixTreeChar; LegacyPersistentScdawgChar;
   LegacyDynamicDawgU64; LegacyPersistentARTrieU64;
   LegacyPersistentARTrieU64Compact;
   LegacyPersistentARTrieU64Prefix3Compat;
   LegacyEncodedPersistentARTrieU64].

Definition legacy_alias_family (alias : LegacyAlias) : DictionaryFamily :=
  match alias with
  | LegacyDynamicDawg | LegacyDynamicDawgChar | LegacyDynamicDawgU64 =>
      DynamicDawgFamily
  | LegacyDoubleArrayTrie | LegacyDoubleArrayTrieChar =>
      DoubleArrayTrieFamily
  | LegacySuffixAutomaton | LegacySuffixAutomatonChar =>
      SuffixAutomatonFamily
  | LegacyScdawg | LegacyScdawgChar => ScdawgFamily
  | LegacyPathMapDictionary | LegacyPathMapDictionaryChar =>
      PathMapAdapterFamily
  | LegacyPersistentARTrie | LegacyPersistentARTrieChar
  | LegacyPersistentARTrieU64 | LegacyPersistentARTrieU64Compact
  | LegacyPersistentARTrieU64Prefix3Compat
  | LegacyEncodedPersistentARTrieU64 => PersistentARTrieFamily
  | LegacyPersistentSuffixAutomaton
  | LegacyPersistentSuffixAutomatonChar => PersistentSuffixAutomatonFamily
  | LegacyPersistentSuffixTree | LegacyPersistentSuffixTreeChar =>
      PersistentSuffixTreeFamily
  | LegacyPersistentScdawg | LegacyPersistentScdawgChar =>
      PersistentScdawgFamily
  end.

Inductive LegacyAliasClass : Type :=
| LegacyByteClass | LegacyCharClass | LegacyU64Class.

Definition legacy_alias_class (alias : LegacyAlias) : LegacyAliasClass :=
  match alias with
  | LegacyDynamicDawgChar | LegacyDoubleArrayTrieChar
  | LegacySuffixAutomatonChar | LegacyScdawgChar
  | LegacyPathMapDictionaryChar | LegacyPersistentARTrieChar
  | LegacyPersistentSuffixAutomatonChar | LegacyPersistentSuffixTreeChar
  | LegacyPersistentScdawgChar => LegacyCharClass
  | LegacyDynamicDawgU64 | LegacyPersistentARTrieU64
  | LegacyPersistentARTrieU64Compact
  | LegacyPersistentARTrieU64Prefix3Compat
  | LegacyEncodedPersistentARTrieU64 => LegacyU64Class
  | _ => LegacyByteClass
  end.

Definition legacy_alias_profile (alias : LegacyAlias) : FamilyProfile :=
  match legacy_alias_class alias with
  | LegacyByteClass => DirectProfile DirectBytesDomain
  | LegacyCharClass => DirectProfile DirectUnicodeScalarDomain
  | LegacyU64Class => DirectProfile DirectU64Domain
  end.

Definition legacy_alias_layout
    (alias : LegacyAlias) : ExplicitLayoutContract :=
  match alias with
  | LegacyPathMapDictionary => PathMapNativeByteLayout
  | LegacyPathMapDictionaryChar => PathMapUtf8BoundaryLayout
  | LegacyPersistentARTrieU64 | LegacyPersistentARTrieU64Compact =>
      PersistentU64CompactLayout
  | LegacyPersistentARTrieU64Prefix3Compat =>
      PersistentU64Prefix3CompatibilityLayout
  | LegacyEncodedPersistentARTrieU64 =>
      EncodedU64ByteCompatibilityLayout
  | _ => GenericLogicalLayout
  end.

Definition legacy_alias_route (alias : LegacyAlias) : ProfileRoute :=
  match alias with
  | LegacyEncodedPersistentARTrieU64 => EncodedU64ByteAdapterKernel
  | _ => family_profile_route
           (legacy_alias_family alias) (legacy_alias_profile alias)
  end.

Definition legacy_alias_codec (alias : LegacyAlias) : FamilyCodecIdentity :=
  match legacy_alias_class alias with
  | LegacyByteClass => FamilyExistingByteCodec
  | LegacyCharClass => FamilyExistingCharU32Codec
  | LegacyU64Class =>
      match alias with
      | LegacyEncodedPersistentARTrieU64 =>
          FamilyEncodedU64LittleEndianBytePathCodec
      | _ => FamilyExistingNativeU64Codec
      end
  end.

Inductive VocabularyAlias : Type :=
| PersistentVocabARTrieName
| SharedVocabARTrieName
| IndexedVocabularyPersistentName
| SharedVocabTrieName
| DiskBackedVocabTrieInnerName.

Definition all_vocabulary_aliases : list VocabularyAlias :=
  [PersistentVocabARTrieName; SharedVocabARTrieName;
   IndexedVocabularyPersistentName; SharedVocabTrieName;
   DiskBackedVocabTrieInnerName].

Inductive PersistentHandleAlias : Type :=
| SharedARTrieName
| SharedCharARTrieName
| SharedCharTrieName.

Definition all_persistent_handle_aliases : list PersistentHandleAlias :=
  [SharedARTrieName; SharedCharARTrieName; SharedCharTrieName].

Inductive PublicCompatibilitySpelling : Type :=
| LegacyPublicSpelling : LegacyAlias -> PublicCompatibilitySpelling
| VocabularyPublicSpelling : VocabularyAlias -> PublicCompatibilitySpelling
| PersistentHandlePublicSpelling : PersistentHandleAlias ->
    PublicCompatibilitySpelling.

Definition public_spelling_family
    (spelling : PublicCompatibilitySpelling) : DictionaryFamily :=
  match spelling with
  | LegacyPublicSpelling alias => legacy_alias_family alias
  | VocabularyPublicSpelling _ => PersistentVocabARTrieFamily
  | PersistentHandlePublicSpelling _ => PersistentARTrieFamily
  end.

Definition public_spelling_profile
    (spelling : PublicCompatibilitySpelling) : FamilyProfile :=
  match spelling with
  | LegacyPublicSpelling alias => legacy_alias_profile alias
  | VocabularyPublicSpelling _ => DirectProfile DirectUnicodeScalarDomain
  | PersistentHandlePublicSpelling SharedARTrieName =>
      DirectProfile DirectBytesDomain
  | PersistentHandlePublicSpelling SharedCharARTrieName
  | PersistentHandlePublicSpelling SharedCharTrieName =>
      DirectProfile DirectUnicodeScalarDomain
  end.

Definition public_spelling_layout
    (spelling : PublicCompatibilitySpelling) : ExplicitLayoutContract :=
  match spelling with
  | LegacyPublicSpelling alias => legacy_alias_layout alias
  | _ => GenericLogicalLayout
  end.

Definition public_spelling_route
    (spelling : PublicCompatibilitySpelling) : ProfileRoute :=
  match spelling with
  | LegacyPublicSpelling alias => legacy_alias_route alias
  | VocabularyPublicSpelling _ => VocabularyOwnerRoute U64IdCarrier
  | PersistentHandlePublicSpelling _ =>
      family_profile_route
        (public_spelling_family spelling) (public_spelling_profile spelling)
  end.

Definition public_spelling_codec
    (spelling : PublicCompatibilitySpelling) : FamilyCodecIdentity :=
  match spelling with
  | LegacyPublicSpelling alias => legacy_alias_codec alias
  | VocabularyPublicSpelling _ => FamilyExistingCharU32Codec
  | PersistentHandlePublicSpelling SharedARTrieName =>
      FamilyExistingByteCodec
  | PersistentHandlePublicSpelling SharedCharARTrieName
  | PersistentHandlePublicSpelling SharedCharTrieName =>
      FamilyExistingCharU32Codec
  end.

Definition public_spelling_is_persistent
    (spelling : PublicCompatibilitySpelling) : bool :=
  persistent_family (public_spelling_family spelling).

Definition expected_public_format_identity
    (spelling : PublicCompatibilitySpelling)
    : option CertifiedProfileIdentity :=
  if public_spelling_is_persistent spelling then
    Some
      (public_spelling_family spelling,
       (public_spelling_profile spelling,
        (public_spelling_codec spelling,
         (public_spelling_layout spelling, 1))))
  else None.

Definition public_spelling_context
    (spelling : PublicCompatibilitySpelling)
    : FamilyConsumerContext (public_spelling_profile spelling).
Proof.
  destruct spelling as [alias | vocabulary | handle].
  - destruct alias; exact tt.
  - exact tt.
  - destruct handle; exact tt.
Defined.

Record PublicFacadeState
    (spelling : PublicCompatibilitySpelling) (Value : Type) : Type := {
  facade_snapshot :
    FamilySnapshot (public_spelling_family spelling)
      (public_spelling_profile spelling) (public_spelling_context spelling) Value;
  facade_profile_reference : ProfileReference;
  facade_serialized_image : option (list PhysicalByte)
}.

Record PublicFacadeCompatibility
    (spelling : PublicCompatibilitySpelling) (Value : Type)
    (legacy canonical : PublicFacadeState spelling Value) : Prop := {
  facade_legacy_layout_exact :
    snapshot_layout _ _ _ _ (facade_snapshot spelling Value legacy) =
      public_spelling_layout spelling;
  facade_canonical_layout_exact :
    snapshot_layout _ _ _ _ (facade_snapshot spelling Value canonical) =
      public_spelling_layout spelling;
  facade_legacy_kernel_exact :
    snapshot_kernel _ _ _ _ (facade_snapshot spelling Value legacy) =
      kernel_for_profile_route (public_spelling_route spelling);
  facade_canonical_kernel_exact :
    snapshot_kernel _ _ _ _ (facade_snapshot spelling Value canonical) =
      kernel_for_profile_route (public_spelling_route spelling);
  facade_same_revision :
    snapshot_revision _ _ _ _ (facade_snapshot spelling Value legacy) =
      snapshot_revision _ _ _ _ (facade_snapshot spelling Value canonical);
  facade_same_observations :
    SameLogicalObservations
      (snapshot_observations _ _ _ _ (facade_snapshot spelling Value legacy))
      (snapshot_observations _ _ _ _ (facade_snapshot spelling Value canonical));
  facade_legacy_format_exact :
    persistent_identity_of (facade_profile_reference spelling Value legacy) =
      expected_public_format_identity spelling;
  facade_canonical_format_exact :
    persistent_identity_of
      (facade_profile_reference spelling Value canonical) =
      expected_public_format_identity spelling;
  facade_serialization_exact :
    facade_serialized_image spelling Value legacy =
      facade_serialized_image spelling Value canonical
}.

Theorem VWENC_218_LEGACY_ALIAS_INVENTORIES_PRESERVE_CANONICAL_TARGETS :
  length all_legacy_aliases = 23 /\
  (forall alias, In alias all_legacy_aliases) /\
  length all_vocabulary_aliases = 5 /\
  (forall alias, In alias all_vocabulary_aliases) /\
  length all_persistent_handle_aliases = 3 /\
  (forall alias, In alias all_persistent_handle_aliases) /\
  forall spelling (Value : Type)
         (legacy canonical : PublicFacadeState spelling Value),
    PublicFacadeCompatibility spelling Value legacy canonical ->
    SameLogicalObservations
      (snapshot_observations _ _ _ _ (facade_snapshot spelling Value legacy))
      (snapshot_observations _ _ _ _ (facade_snapshot spelling Value canonical)) /\
    facade_serialized_image spelling Value legacy =
      facade_serialized_image spelling Value canonical /\
    persistent_identity_of (facade_profile_reference spelling Value legacy) =
      expected_public_format_identity spelling /\
    persistent_identity_of
      (facade_profile_reference spelling Value canonical) =
      expected_public_format_identity spelling.
Proof.
  split; [reflexivity |]. split.
  - intros alias. destruct alias; simpl; tauto.
  - split; [reflexivity |]. split.
    + intros alias. destruct alias; simpl; tauto.
    + split; [reflexivity |]. split.
      * intros alias. destruct alias; simpl; tauto.
      * intros spelling Value legacy canonical Hcompatibility.
        split.
        -- exact (facade_same_observations _ _ _ _ Hcompatibility).
        -- split.
           ++ exact (facade_serialization_exact _ _ _ _ Hcompatibility).
           ++ split.
              ** exact (facade_legacy_format_exact _ _ _ _ Hcompatibility).
              ** exact (facade_canonical_format_exact _ _ _ _ Hcompatibility).
Qed.

Theorem VWENC_219_EVERY_CHAR_ALIAS_TARGETS_UNICODE_SCALAR_UNITS :
  forall alias,
    legacy_alias_class alias = LegacyCharClass ->
    legacy_alias_profile alias = DirectProfile DirectUnicodeScalarDomain /\
    ProfileUnit (legacy_alias_profile alias) =
      DirectUnit DirectUnicodeScalarDomain.
Proof.
  intros alias Hclass. unfold legacy_alias_profile. rewrite Hclass.
  split; reflexivity.
Qed.

Definition direct_unit_value {domain : DirectUnitDomain}
    (unit : DirectUnit domain) : nat := proj1_sig unit.

Definition encoded_u64_unit_bytes
    (unit : DirectUnit DirectU64Domain) : list PhysicalByte :=
  encode_fixed_little_endian 8 (direct_unit_value unit).

Fixpoint legacy_encoded_u64_sequence
    (units : list (DirectUnit DirectU64Domain)) : list PhysicalByte :=
  match units with
  | [] => []
  | unit :: rest => encoded_u64_unit_bytes unit ++
                    legacy_encoded_u64_sequence rest
  end.

Definition canonical_encoded_u64_sequence
    (units : list (DirectUnit DirectU64Domain)) : list PhysicalByte :=
  concat (map encoded_u64_unit_bytes units).

Definition encoded_u64_logical_edges
    (units : list (DirectUnit DirectU64Domain)) :=
  map (fun unit => [unit]) units.

Lemma legacy_and_canonical_encoded_u64_sequences_are_equal :
  forall units,
    legacy_encoded_u64_sequence units = canonical_encoded_u64_sequence units.
Proof.
  induction units as [|unit rest IH]; simpl; [reflexivity |].
  now rewrite IH.
Qed.

Lemma encoded_u64_sequence_has_exact_physical_width :
  forall units,
    length (canonical_encoded_u64_sequence units) = 8 * length units.
Proof.
  induction units as [|unit rest IH]; [reflexivity |].
  change
    (length (encoded_u64_unit_bytes unit ++
       canonical_encoded_u64_sequence rest) = 8 * S (length rest)).
  rewrite length_app, IH.
  assert (Hunit : length (encoded_u64_unit_bytes unit) = 8).
  { unfold encoded_u64_unit_bytes. apply fixed_little_endian_length. }
  rewrite Hunit. lia.
Qed.

Definition u64_alias_layout_and_route_are_exact (alias : LegacyAlias) : Prop :=
  match alias with
  | LegacyDynamicDawgU64 =>
      legacy_alias_layout alias = GenericLogicalLayout /\
      legacy_alias_route alias = RetainedSpecializedKernel
  | LegacyPersistentARTrieU64 | LegacyPersistentARTrieU64Compact =>
      legacy_alias_layout alias = PersistentU64CompactLayout /\
      legacy_alias_route alias = RetainedSpecializedKernel
  | LegacyPersistentARTrieU64Prefix3Compat =>
      legacy_alias_layout alias =
        PersistentU64Prefix3CompatibilityLayout /\
      legacy_alias_route alias = RetainedSpecializedKernel
  | LegacyEncodedPersistentARTrieU64 =>
      legacy_alias_layout alias = EncodedU64ByteCompatibilityLayout /\
      legacy_alias_route alias = EncodedU64ByteAdapterKernel
  | _ => True
  end.

Theorem VWENC_220_EVERY_U64_ALIAS_PRESERVES_PROFILE_AND_EXPLICIT_LAYOUT :
  (forall alias,
    legacy_alias_class alias = LegacyU64Class ->
    legacy_alias_profile alias = DirectProfile DirectU64Domain /\
    u64_alias_layout_and_route_are_exact alias) /\
  (forall units,
    legacy_encoded_u64_sequence units =
      canonical_encoded_u64_sequence units /\
    length (canonical_encoded_u64_sequence units) = 8 * length units /\
    length (encoded_u64_logical_edges units) = length units).
Proof.
  split.
  - intros alias Hclass. split.
    + unfold legacy_alias_profile. now rewrite Hclass.
    + destruct alias; simpl in *; try discriminate; repeat split; reflexivity.
  - intros units. repeat split.
    + apply legacy_and_canonical_encoded_u64_sequences_are_equal.
    + apply encoded_u64_sequence_has_exact_physical_width.
    + apply length_map.
Qed.

Record DynamicToDatConversion
    (profile : FamilyProfile) (context : FamilyConsumerContext profile)
    (Value : Type) : Type := {
  conversion_dynamic_source :
    FamilySnapshot DynamicDawgFamily profile context Value;
  conversion_dat_target :
    FamilySnapshot DoubleArrayTrieFamily profile context Value;
  conversion_same_revision :
    snapshot_revision _ _ _ _ conversion_dynamic_source =
      snapshot_revision _ _ _ _ conversion_dat_target;
  conversion_same_observations :
    SameLogicalObservations
      (snapshot_observations _ _ _ _ conversion_dynamic_source)
      (snapshot_observations _ _ _ _ conversion_dat_target)
}.

Theorem VWENC_221_DYNAMIC_TO_FROZEN_CONVERSION_PRESERVES_LOGICAL_OBSERVATIONS :
  forall profile (context : FamilyConsumerContext profile) (Value : Type)
         (conversion : DynamicToDatConversion profile context Value),
    snapshot_revision _ _ _ _ (conversion_dynamic_source _ _ _ conversion) =
      snapshot_revision _ _ _ _ (conversion_dat_target _ _ _ conversion) /\
    SameLogicalObservations
      (snapshot_observations _ _ _ _
        (conversion_dynamic_source _ _ _ conversion))
      (snapshot_observations _ _ _ _
        (conversion_dat_target _ _ _ conversion)).
Proof.
  intros profile context Value conversion. split.
  - exact (conversion_same_revision _ _ _ conversion).
  - exact (conversion_same_observations _ _ _ conversion).
Qed.

Record TraversalProjectionBundle
    (family : DictionaryFamily) (profile : FamilyProfile)
    (context : FamilyConsumerContext profile) (Value : Type)
    : Type := {
  projection_revision : nat;
  projection_reference_view :
    LogicalObservations (BoundProfileUnit profile context) Value;
  projection_node_view :
    LogicalObservations (BoundProfileUnit profile context) Value;
  projection_zipper_view :
    LogicalObservations (BoundProfileUnit profile context) Value;
  projection_cursor_view :
    LogicalObservations (BoundProfileUnit profile context) Value;
  projection_node_revision : nat;
  projection_zipper_revision : nat;
  projection_cursor_revision : nat;
  projection_node_revision_exact : projection_node_revision = projection_revision;
  projection_zipper_revision_exact : projection_zipper_revision = projection_revision;
  projection_cursor_revision_exact : projection_cursor_revision = projection_revision;
  projection_node_refines_reference :
    SameLogicalObservations projection_node_view projection_reference_view;
  projection_zipper_refines_reference :
    SameLogicalObservations projection_zipper_view projection_reference_view;
  projection_cursor_refines_reference :
    SameLogicalObservations projection_cursor_view projection_reference_view
}.

Theorem VWENC_222_NODE_ZIPPER_AND_CURSOR_SHARE_ONE_REVISION_BOUND_VIEW :
  forall family profile (context : FamilyConsumerContext profile) (Value : Type)
         (bundle : TraversalProjectionBundle family profile context Value),
    projection_node_revision _ _ _ _ bundle =
      projection_zipper_revision _ _ _ _ bundle /\
    projection_zipper_revision _ _ _ _ bundle =
      projection_cursor_revision _ _ _ _ bundle /\
    SameLogicalObservations
      (projection_node_view _ _ _ _ bundle)
      (projection_zipper_view _ _ _ _ bundle) /\
    SameLogicalObservations
      (projection_zipper_view _ _ _ _ bundle)
      (projection_cursor_view _ _ _ _ bundle).
Proof.
  intros family profile context Value bundle. split.
  - rewrite (projection_node_revision_exact _ _ _ _ bundle),
      (projection_zipper_revision_exact _ _ _ _ bundle). reflexivity.
  - split.
    + rewrite (projection_zipper_revision_exact _ _ _ _ bundle),
        (projection_cursor_revision_exact _ _ _ _ bundle). reflexivity.
    + split.
      * eapply same_logical_observations_transitive with
            (middle := projection_reference_view
              family profile context Value bundle).
        -- exact (projection_node_refines_reference _ _ _ _ bundle).
        -- apply same_logical_observations_symmetric.
           exact (projection_zipper_refines_reference _ _ _ _ bundle).
      * eapply same_logical_observations_transitive with
            (middle := projection_reference_view
              family profile context Value bundle).
        -- exact (projection_zipper_refines_reference _ _ _ _ bundle).
        -- apply same_logical_observations_symmetric.
           exact (projection_cursor_refines_reference _ _ _ _ bundle).
Qed.

Definition surface_is_available
    (family : DictionaryFamily) (surface : ConsumerSurfaceClass) : Prop :=
  (exists route, family_surface_cell family surface = ExistingSurface route) \/
  (exists route, family_surface_cell family surface = ProspectiveSurface route).

Record LifecycleRefinement
    (family : DictionaryFamily) (profile : FamilyProfile)
    (context : FamilyConsumerContext profile) (Value : Type)
    (surface : ConsumerSurfaceClass)
    : Type := {
  lifecycle_surface_available : surface_is_available family surface;
  lifecycle_source : FamilySnapshot family profile context Value;
  lifecycle_product : FamilySnapshot family profile context Value;
  lifecycle_product_refines :
    SameLogicalObservations
      (snapshot_observations _ _ _ _ lifecycle_source)
      (snapshot_observations _ _ _ _ lifecycle_product);
  lifecycle_revision_exact :
    snapshot_revision _ _ _ _ lifecycle_source =
      snapshot_revision _ _ _ _ lifecycle_product
}.

Theorem VWENC_223_FACTORY_COLLECTION_AND_SERIALIZATION_PRESERVE_PROFILE_VIEW :
  forall family profile (context : FamilyConsumerContext profile)
         (Value : Type) surface
         (lifecycle : LifecycleRefinement
           family profile context Value surface),
    In surface
      [FactorySurface; CollectionSurface; SerializationReopenSurface] ->
    surface_is_available family surface /\
    SameLogicalObservations
      (snapshot_observations _ _ _ _
        (lifecycle_source _ _ _ _ _ lifecycle))
      (snapshot_observations _ _ _ _
        (lifecycle_product _ _ _ _ _ lifecycle)) /\
    snapshot_revision _ _ _ _
      (lifecycle_source _ _ _ _ _ lifecycle) =
      snapshot_revision _ _ _ _
        (lifecycle_product _ _ _ _ _ lifecycle).
Proof.
  intros family profile context Value surface lifecycle _.
  split.
  - exact (lifecycle_surface_available _ _ _ _ _ lifecycle).
  - split.
    + exact (lifecycle_product_refines _ _ _ _ _ lifecycle).
    + exact (lifecycle_revision_exact _ _ _ _ _ lifecycle).
Qed.

Record ExtensionalSetCombinator (Atom Value : Type) : Type := {
  combine_set_views :
    LogicalObservations Atom Value -> LogicalObservations Atom Value ->
    LogicalObservations Atom Value;
  combine_set_views_extensional :
    forall left left_refined right right_refined,
      SameLogicalObservations left left_refined ->
      SameLogicalObservations right right_refined ->
      SameLogicalObservations
        (combine_set_views left right)
        (combine_set_views left_refined right_refined)
}.

Theorem VWENC_224_SET_COMBINATORS_COMMUTE_WITH_PROFILE_REFINEMENT :
  forall (Atom Value : Type) (combine : ExtensionalSetCombinator Atom Value)
         left left_refined right right_refined,
    SameLogicalObservations left left_refined ->
    SameLogicalObservations right right_refined ->
    SameLogicalObservations
      (combine_set_views Atom Value combine left right)
      (combine_set_views Atom Value combine left_refined right_refined).
Proof. intros. now apply combine_set_views_extensional. Qed.

Record ExtensionalValueCombinator (Atom Value : Type) : Type := {
  combine_value_views :
    LogicalObservations Atom Value -> LogicalObservations Atom Value ->
    LogicalObservations Atom Value;
  combine_value_views_extensional :
    forall left left_refined right right_refined,
      SameLogicalObservations left left_refined ->
      SameLogicalObservations right right_refined ->
      SameLogicalObservations
        (combine_value_views left right)
        (combine_value_views left_refined right_refined)
}.

Theorem VWENC_225_VALUE_COMBINATORS_COMMUTE_WITH_PROFILE_REFINEMENT :
  forall (Atom Value : Type)
         (combine : ExtensionalValueCombinator Atom Value)
         left left_refined right right_refined,
    SameLogicalObservations left left_refined ->
    SameLogicalObservations right right_refined ->
    SameLogicalObservations
      (combine_value_views Atom Value combine left right)
      (combine_value_views Atom Value combine left_refined right_refined).
Proof. intros. now apply combine_value_views_extensional. Qed.

(** ** Encoded adapters and logical suffix boundaries *)

Inductive PathMapTraceEvent : Type :=
| PathMapPhysicalByteVisited : PhysicalByte -> PathMapTraceEvent
| PathMapLogicalAtomEmitted : LogicalAtom -> PathMapTraceEvent.

Definition pathmap_physical_trace
    (stored : StoredLogicalUnit) (atom : LogicalAtom)
    : list PathMapTraceEvent :=
  map PathMapPhysicalByteVisited (physical_codeword_of stored) ++
    [PathMapLogicalAtomEmitted atom].

Fixpoint pathmap_logical_projection
    (trace : list PathMapTraceEvent) : list LogicalAtom :=
  match trace with
  | [] => []
  | PathMapPhysicalByteVisited _ :: rest => pathmap_logical_projection rest
  | PathMapLogicalAtomEmitted atom :: rest =>
      atom :: pathmap_logical_projection rest
  end.

Lemma pathmap_logical_projection_app :
  forall left right,
    pathmap_logical_projection (left ++ right) =
      pathmap_logical_projection left ++ pathmap_logical_projection right.
Proof.
  induction left as [|event rest IH]; intros right; simpl; [reflexivity |].
  destruct event; simpl; now rewrite IH.
Qed.

Lemma pathmap_physical_prefix_projects_to_no_logical_atoms :
  forall bytes,
    pathmap_logical_projection (map PathMapPhysicalByteVisited bytes) = [].
Proof.
  induction bytes; simpl; auto.
Qed.

Lemma pathmap_trace_projects_exactly_one_atom :
  forall stored atom,
    pathmap_logical_projection (pathmap_physical_trace stored atom) = [atom].
Proof.
  intros stored atom. unfold pathmap_physical_trace.
  rewrite pathmap_logical_projection_app,
    pathmap_physical_prefix_projects_to_no_logical_atoms.
  reflexivity.
Qed.

Definition pathmap_node_projection := pathmap_logical_projection.
Definition pathmap_zipper_projection := pathmap_logical_projection.
Definition pathmap_snapshot_projection := pathmap_logical_projection.

Theorem VWENC_226_ENCODED_ADAPTER_STAGING_BYTES_ARE_HIDDEN_FROM_CONSUMERS :
  forall surface stored atom,
    representation_admits EncodedBytePathAdapter stored ->
    decode_stored_logical_unit stored = Some atom ->
    consumer_observation surface
      {| transition_representation := EncodedBytePathAdapter;
         transition_unit := stored |} = [atom] /\
    length
      (consumer_observation surface
        {| transition_representation := EncodedBytePathAdapter;
           transition_unit := stored |}) = 1 /\
    pathmap_node_projection (pathmap_physical_trace stored atom) = [atom] /\
    pathmap_zipper_projection (pathmap_physical_trace stored atom) = [atom] /\
    pathmap_snapshot_projection (pathmap_physical_trace stored atom) = [atom].
Proof.
  intros surface stored atom Hadmits Hdecode.
  assert (Hvalid :
    valid_stored_transition
      {| transition_representation := EncodedBytePathAdapter;
         transition_unit := stored |}).
  { split; [exact Hadmits |]. exists atom. exact Hdecode. }
  repeat split.
  - apply VWENC_16_CODEC_BYTES_ARE_NOT_LOGICAL_TRANSITIONS.
    change
      ((if representation_admitsb EncodedBytePathAdapter stored
        then decode_stored_logical_unit stored else None) = Some atom).
    apply representation_admitsb_reflects_admission in Hadmits.
    now rewrite Hadmits, Hdecode.
  - now apply VWENC_17_ONE_LOGICAL_ATOM_PER_CONSUMER_TRANSITION.
  - apply pathmap_trace_projects_exactly_one_atom.
  - apply pathmap_trace_projects_exactly_one_atom.
  - apply pathmap_trace_projects_exactly_one_atom.
Qed.

Theorem VWENC_227_PATHMAP_UTF8_GROUPING_EMITS_ONE_UNICODE_SCALAR :
  forall surface bytes codepoint,
    canonical_utf8_codeword codepoint bytes ->
    family_profile_cell PathMapAdapterFamily
      (DirectProfile DirectUnicodeScalarDomain) =
      ExistingProfileCell PathMapUtf8BoundaryAdapterRoute
        PathMapUtf8BoundaryLayout /\
    consumer_observation surface
      {| transition_representation := EncodedBytePathAdapter;
         transition_unit := StoredUtf8 bytes |} = [UnicodeAtom codepoint] /\
    pathmap_node_projection
      (pathmap_physical_trace (StoredUtf8 bytes) (UnicodeAtom codepoint)) =
      [UnicodeAtom codepoint] /\
    pathmap_zipper_projection
      (pathmap_physical_trace (StoredUtf8 bytes) (UnicodeAtom codepoint)) =
      [UnicodeAtom codepoint] /\
    pathmap_snapshot_projection
      (pathmap_physical_trace (StoredUtf8 bytes) (UnicodeAtom codepoint)) =
      [UnicodeAtom codepoint].
Proof.
  intros surface bytes codepoint Hcanonical.
  split; [reflexivity |]. split.
  - apply VWENC_16_CODEC_BYTES_ARE_NOT_LOGICAL_TRANSITIONS.
    now apply VWENC_66_UTF8_LOGICAL_IDENTITY_IS_UNICODE_SCALAR.
  - split; [apply pathmap_trace_projects_exactly_one_atom |].
    split; apply pathmap_trace_projects_exactly_one_atom.
Qed.

Theorem VWENC_228_CANONICAL_ULEB_CODEWORD_EMITS_ONE_OPAQUE_LOGICAL_ATOM :
  forall carrier surface bytes,
    canonical_uleb_codeword bytes ->
    family_profile_cell PathMapAdapterFamily
      (InternedProfile CanonicalUlebDomain carrier) =
      ProspectiveProfileCell (PathMapInternedIdAdapterRoute carrier)
        (PathMapInternedIdLayout carrier) /\
    consumer_observation surface
      {| transition_representation := OpaqueCodewordEdge;
         transition_unit := StoredUleb bytes |} = [UlebAtom bytes].
Proof.
  intros carrier surface bytes Hcanonical. split; [reflexivity |].
  apply VWENC_16_CODEC_BYTES_ARE_NOT_LOGICAL_TRANSITIONS.
  now apply VWENC_65_ULEB_LOGICAL_IDENTITY_IS_CANONICAL_BYTES.
Qed.

Fixpoint codeword_boundary_offsets_from
    (start : nat) (codewords : list (list PhysicalByte)) : list nat :=
  match codewords with
  | [] => [start]
  | codeword :: rest =>
      start ::
      codeword_boundary_offsets_from (start + length codeword) rest
  end.

Definition codeword_boundary_offsets
    (codewords : list (list PhysicalByte)) : list nat :=
  codeword_boundary_offsets_from 0 codewords.

Lemma codeword_boundary_offsets_from_are_exact :
  forall codewords start offset,
    In offset (codeword_boundary_offsets_from start codewords) <->
    exists prefix suffix,
      codewords = prefix ++ suffix /\
      offset = start + length (concat prefix).
Proof.
  induction codewords as [| codeword rest IH]; intros start offset.
  - simpl. split.
    + intros [Hequal | Himpossible]; [subst | contradiction].
      exists [], []. simpl. split; [reflexivity | lia].
    + intros [prefix [suffix [Hequal Hoffset]]].
      destruct prefix as [| first prefix];
        [simpl in Hoffset; left; lia | discriminate].
  - simpl. split.
    + intros [Hstart | Hlater].
      * subst. exists [], (codeword :: rest). simpl.
        split; [reflexivity | lia].
      * apply IH in Hlater.
        destruct Hlater as [prefix [suffix [Hrest Hoffset]]].
        exists (codeword :: prefix), suffix. split.
        -- simpl. now rewrite Hrest.
        -- simpl. rewrite length_app. lia.
    + intros [prefix [suffix [Hequal Hoffset]]].
      destruct prefix as [| first prefix].
      * simpl in Hoffset. left. lia.
      * simpl in Hequal. inversion Hequal; subst first.
        right. rewrite <- H1. apply IH.
        exists prefix, suffix. split; [assumption |].
        simpl in Hoffset. rewrite length_app in Hoffset. lia.
Qed.

Theorem VWENC_229_CODEWORD_BOUNDARY_OFFSETS_ARE_EXACTLY_LOGICAL_SPLITS :
  forall codewords offset,
    In offset (codeword_boundary_offsets codewords) <->
    exists prefix suffix,
      codewords = prefix ++ suffix /\
      offset = length (concat prefix).
Proof.
  intros codewords offset.
  unfold codeword_boundary_offsets.
  rewrite codeword_boundary_offsets_from_are_exact.
  split.
  - intros [prefix [suffix [Hequal Hoffset]]].
    exists prefix, suffix. split; [exact Hequal | lia].
  - intros [prefix [suffix [Hequal Hoffset]]].
    exists prefix, suffix. split; [exact Hequal | lia].
Qed.

Definition physical_suffix_at
    (bytes : list PhysicalByte) (offset : nat)
    (suffix : list PhysicalByte) : Prop :=
  exists prefix,
    bytes = prefix ++ suffix /\
    length prefix = offset.

Theorem VWENC_230_RAW_UTF8_SUFFIX_CAN_START_INSIDE_ONE_SCALAR_CODEWORD :
  canonical_utf8_codeword 169 [194; 169] /\
  physical_suffix_at [194; 169] 1 [169] /\
  ~ In 1 (codeword_boundary_offsets [[194; 169]]).
Proof.
  split.
  - split.
    + unfold unicode_scalar. split.
      * unfold unicode_limit, utf8_three_byte_limit.
        change (169 < 17 * (256 * (256 * 1))). nia.
      * unfold surrogate_start, surrogate_end.
        change (169 < 216 * 256 \/ 224 * 256 <= 169).
        left. nia.
    + unfold encode_utf8_scalar, utf8_one_byte_limit,
        utf8_two_byte_limit.
      rewrite (proj2 (Nat.ltb_ge 169 128)) by lia.
      rewrite (proj2 (Nat.ltb_lt 169 (8 * 256))) by lia.
      reflexivity.
  - split.
    + exists [194]. split; reflexivity.
    + simpl. lia.
Qed.

Theorem VWENC_231_RAW_ULEB_SUFFIX_CAN_START_INSIDE_ONE_CODEWORD :
  canonical_uleb_codeword [128; 1] /\
  physical_suffix_at [128; 1] 1 [1] /\
  ~ In 1 (codeword_boundary_offsets [[128; 1]]).
Proof.
  split.
  - split.
    + apply UlebShapeMore; [lia | lia |].
      apply UlebShapeLast. lia.
    + change (canonical_uleb_digits [0; 1]).
      unfold canonical_uleb_digits. split; [discriminate |].
      split.
      * constructor; [unfold valid_uleb_digit; lia |].
        constructor; [unfold valid_uleb_digit; lia | constructor].
      * intros. simpl. discriminate.
  - split.
    + exists [128]. split; reflexivity.
    + simpl. lia.
Qed.

Definition logical_codeword_suffix
    (codewords suffix : list (list PhysicalByte)) : Prop :=
  exists prefix, codewords = prefix ++ suffix.

Inductive SuffixSemanticDomain : Type :=
| SuffixNotApplicable
| RawByteSuffixSemantics
| LogicalAtomSuffixSemantics.

Definition suffix_family (family : DictionaryFamily) : bool :=
  match family with
  | SuffixAutomatonFamily | ScdawgFamily
  | PersistentSuffixAutomatonFamily | PersistentSuffixTreeFamily
  | PersistentScdawgFamily => true
  | _ => false
  end.

Definition suffix_semantic_domain
    (family : DictionaryFamily) (profile : FamilyProfile)
    (layout : ExplicitLayoutContract) : SuffixSemanticDomain :=
  if suffix_family family then
    match profile, layout with
    | DirectProfile DirectBytesDomain, _ => RawByteSuffixSemantics
    | _, EncodedU64ByteCompatibilityLayout => RawByteSuffixSemantics
    | _, PathMapNativeByteLayout => RawByteSuffixSemantics
    | _, _ => LogicalAtomSuffixSemantics
    end
  else SuffixNotApplicable.

Definition suffix_start_admissible
    (domain : SuffixSemanticDomain)
    (codewords : list (list PhysicalByte)) (offset : nat) : Prop :=
  match domain with
  | SuffixNotApplicable => False
  | RawByteSuffixSemantics => offset <= length (concat codewords)
  | LogicalAtomSuffixSemantics =>
      In offset (codeword_boundary_offsets codewords)
  end.

Theorem VWENC_232_LOGICAL_SUFFIXES_BEGIN_ONLY_AT_CODEWORD_BOUNDARIES :
  (forall codewords suffix,
    logical_codeword_suffix codewords suffix ->
    exists offset,
      In offset (codeword_boundary_offsets codewords) /\
      physical_suffix_at (concat codewords) offset (concat suffix)) /\
  (forall family profile layout codewords offset,
    suffix_semantic_domain family profile layout =
      LogicalAtomSuffixSemantics ->
    suffix_start_admissible
      (suffix_semantic_domain family profile layout) codewords offset ->
    In offset (codeword_boundary_offsets codewords)).
Proof.
  split.
  - intros codewords suffix [prefix Hequal].
    exists (length (concat prefix)). split.
    + apply (proj2
        (VWENC_229_CODEWORD_BOUNDARY_OFFSETS_ARE_EXACTLY_LOGICAL_SPLITS
          codewords (length (concat prefix)))).
      exists prefix, suffix. now split.
    + unfold physical_suffix_at. exists (concat prefix). split.
      * rewrite Hequal. apply concat_app.
      * reflexivity.
  - intros family profile layout codewords offset Hdomain Hadmissible.
    rewrite Hdomain in Hadmissible. exact Hadmissible.
Qed.

Theorem VWENC_233_RAW_BYTE_SUFFIX_INDEXES_CLAIM_ONLY_BYTE_SEMANTICS :
  forall family layout,
    suffix_family family = true ->
    suffix_semantic_domain family
      (DirectProfile DirectBytesDomain) layout = RawByteSuffixSemantics /\
    suffix_semantic_domain family
      (DirectProfile DirectBytesDomain) layout <>
        LogicalAtomSuffixSemantics /\
    family_surface_cell family SuffixSurface =
      ExistingSurface SuffixIndexRoute.
Proof.
  intros family layout Hsuffix.
  unfold suffix_semantic_domain. rewrite Hsuffix. split; [reflexivity |].
  split; [discriminate |].
  destruct family; simpl in Hsuffix |- *; try discriminate; reflexivity.
Qed.

Definition serialized_direct_codewords
    (profile : VariableWidthCodecSpec.DirectProfile) (units : list nat)
    : list (list PhysicalByte) :=
  map (fun unit => snd (serialize_direct_unit profile unit)) units.

Theorem VWENC_234_DIRECT_UNITS_PRESERVE_ONE_CODEWORD_PER_LOGICAL_EDGE :
  forall profile units,
    length (serialized_direct_codewords profile units) = length units /\
    Forall
      (fun bytes => length bytes = direct_byte_width profile)
      (serialized_direct_codewords profile units).
Proof.
  intros profile units. split.
  - apply length_map.
  - induction units as [| unit rest IH]; simpl; constructor.
    + apply VWENC_49_DIRECT_SERIALIZATION_HAS_EXACT_FIXED_WIDTH.
    + exact IH.
Qed.

Definition serialized_symbol_id_codewords (I : FixedWidthCarrierProfile)
    (ids : list (SymbolId I)) : list (list PhysicalByte) :=
  map (encode_symbol_id I) ids.

Theorem VWENC_235_INTERNED_IDS_PRESERVE_ONE_FIXED_CODEWORD_PER_LOGICAL_EDGE :
  forall I ids,
    length (serialized_symbol_id_codewords I ids) = length ids /\
    Forall
      (fun bytes => length bytes = carrier_width_bytes I)
      (serialized_symbol_id_codewords I ids).
Proof.
  intros I ids. split.
  - apply length_map.
  - induction ids as [| id rest IH]; simpl; constructor.
    + exact (proj1 (symbol_id_fixed_width_encoding_roundtrips I id)).
    + exact IH.
Qed.

(** ** One-time vocabulary binding and fixed-width hot traversal *)

Record BoundConsumerFiber
    (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (expected actual : VocabularyFiber P I) : Type := {
  consumer_fiber_binding_certificate : expected = actual
}.

Definition bind_consumer_fiber
    (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (expected actual : VocabularyFiber P I)
    : option (BoundConsumerFiber P I expected actual).
Proof.
  destruct (vocabulary_fiber_eq_dec P I expected actual) as [Hequal | Hdifferent].
  - exact (Some {| consumer_fiber_binding_certificate := Hequal |}).
  - exact None.
Defined.

Theorem VWENC_236_CONSUMER_VOCABULARY_BINDING_IS_VALIDATED_ONCE :
  forall P I (expected actual : VocabularyFiber P I),
    bind_consumer_fiber P I expected actual <> None <->
    expected = actual.
Proof.
  intros P I expected actual.
  unfold bind_consumer_fiber.
  destruct (vocabulary_fiber_eq_dec P I expected actual) as
    [Hequal | Hdifferent].
  - split; [intros; exact Hequal | intros; discriminate].
  - split.
    + intros Hpresent. exfalso. apply Hpresent. reflexivity.
    + intros Hequal. contradiction.
Qed.

Theorem VWENC_237_MISMATCHED_VOCABULARY_FIBERS_ARE_REJECTED_BEFORE_TRAVERSAL :
  forall P I (expected actual : VocabularyFiber P I),
    expected <> actual ->
    bind_consumer_fiber P I expected actual = None.
Proof.
  intros P I expected actual Hdifferent.
  unfold bind_consumer_fiber.
  destruct (vocabulary_fiber_eq_dec P I expected actual);
    [contradiction | reflexivity].
Qed.

(** Direct and interned hot views remain separate.  The interned unit type was
    defined above the family snapshot so every consumer surface—not only this
    optimized view—must use the same snapshot-bound representation. *)

Definition direct_hot_kernel (domain : DirectUnitDomain)
    : MonomorphicFixedWidthKernel (DirectUnit domain).
Proof.
  refine
    {| monomorphic_width := direct_byte_width (direct_codec_profile domain);
       monomorphic_width_positive := _;
       monomorphic_encode := fun unit =>
         snd (serialize_direct_unit (direct_codec_profile domain)
           (direct_unit_value unit));
       monomorphic_encode_exact := _;
       monomorphic_variable_decode_request := fun _ => None;
       monomorphic_has_no_variable_decode := _ |}.
  - destruct domain; simpl; lia.
  - intros [unit Hvalid].
    apply VWENC_49_DIRECT_SERIALIZATION_HAS_EXACT_FIXED_WIDTH.
  - reflexivity.
Defined.

Definition interned_hot_kernel
    (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (fiber : VocabularyFiber P I)
    (snapshot : VocabularySnapshot P I fiber)
    : MonomorphicFixedWidthKernel
        (SnapshotBoundSymbolId P I fiber snapshot).
Proof.
  refine
    {| monomorphic_width := carrier_width_bytes I;
       monomorphic_width_positive := carrier_width_positive I;
       monomorphic_encode := fun bound =>
         encode_symbol_id I
           (snapshot_bound_symbol_id P I fiber snapshot bound);
       monomorphic_encode_exact := _;
       monomorphic_variable_decode_request := fun _ => None;
       monomorphic_has_no_variable_decode := _ |}.
  - intros bound.
    exact (proj1 (symbol_id_fixed_width_encoding_roundtrips I
      (snapshot_bound_symbol_id P I fiber snapshot bound))).
  - reflexivity.
Defined.

Record DirectHotTraversalView (domain : DirectUnitDomain) : Type := {
  direct_hot_view_units : list (DirectUnit domain)
}.

Definition run_direct_hot_view (domain : DirectUnitDomain)
    (view : DirectHotTraversalView domain) : list (list PhysicalByte) :=
  run_bound_kernel (direct_hot_kernel domain)
    (direct_hot_view_units domain view).

Record BoundHotTraversalView
    (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (expected actual : VocabularyFiber P I)
    (snapshot : VocabularySnapshot P I actual) : Type := {
  hot_view_binding : BoundConsumerFiber P I expected actual;
  hot_view_units : list (SnapshotBoundSymbolId P I actual snapshot)
}.

Definition construct_bound_hot_traversal_view
    (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (expected actual : VocabularyFiber P I)
    (snapshot : VocabularySnapshot P I actual)
    (units : list (SnapshotBoundSymbolId P I actual snapshot))
    : option (BoundHotTraversalView P I expected actual snapshot) :=
  match bind_consumer_fiber P I expected actual with
  | Some binding =>
      Some {| hot_view_binding := binding; hot_view_units := units |}
  | None => None
  end.

Definition run_interned_hot_view
    (P : CertifiedAtomProfile) (I : FixedWidthCarrierProfile)
    (expected actual : VocabularyFiber P I)
    (snapshot : VocabularySnapshot P I actual)
    (view : BoundHotTraversalView P I expected actual snapshot)
    : list (list PhysicalByte) :=
  run_bound_kernel (interned_hot_kernel P I actual snapshot)
    (hot_view_units P I expected actual snapshot view).

Theorem VWENC_238_EVERY_HOT_TRANSITION_HAS_AN_EXACT_FIXED_WIDTH_ENCODING :
  forall (Unit : Type) (kernel : MonomorphicFixedWidthKernel Unit) unit,
    length (monomorphic_encode Unit kernel unit) =
      monomorphic_width Unit kernel /\
    0 < monomorphic_width Unit kernel.
Proof.
  intros Unit kernel unit. split.
  - apply monomorphic_encode_exact.
  - apply monomorphic_width_positive.
Qed.

Theorem VWENC_239_ARBITRARY_WIDTH_BIGUINT_BYTES_STAY_OUTSIDE_HOT_TRAVERSAL :
  forall (Unit : Type) (kernel : MonomorphicFixedWidthKernel Unit) unit,
    monomorphic_variable_decode_request Unit kernel unit = None.
Proof. intros. apply monomorphic_has_no_variable_decode. Qed.

Inductive SemanticOwnership : Type :=
| LibdictensteinStorageSemantics
| LlatticeAlgebraSemantics.

Definition join_meet_semantics_owner
    (_profile : FamilyProfile) : SemanticOwnership :=
  LlatticeAlgebraSemantics.

Theorem VWENC_240_DICTIONARY_PROFILES_DO_NOT_OWN_LLATTICE_ALGEBRA :
  forall profile,
    join_meet_semantics_owner profile = LlatticeAlgebraSemantics /\
    join_meet_semantics_owner profile <>
      LibdictensteinStorageSemantics.
Proof. intros. split; discriminate || reflexivity. Qed.

Theorem VWENC_247_HOT_TRAVERSAL_VIEW_EXISTS_IFF_FIBER_BINDING_SUCCEEDS :
  forall P I (expected actual : VocabularyFiber P I)
         (snapshot : VocabularySnapshot P I actual) units,
    construct_bound_hot_traversal_view
      P I expected actual snapshot units <> None <->
    expected = actual.
Proof.
  intros P I expected actual snapshot units.
  unfold construct_bound_hot_traversal_view.
  destruct (bind_consumer_fiber P I expected actual) as
    [binding |] eqn:Hbinding.
  - split; [intros | intros; discriminate].
    apply (proj1 (VWENC_236_CONSUMER_VOCABULARY_BINDING_IS_VALIDATED_ONCE
      P I expected actual)).
    rewrite Hbinding. discriminate.
  - split.
    + intros Hpresent. exfalso. apply Hpresent. reflexivity.
    + intros Hequal.
      apply (proj2 (VWENC_236_CONSUMER_VOCABULARY_BINDING_IS_VALIDATED_ONCE
        P I expected actual)) in Hequal.
      rewrite Hbinding in Hequal. contradiction.
Qed.

Theorem VWENC_248_MISMATCHED_FIBER_CANNOT_CONSTRUCT_A_HOT_TRAVERSAL_VIEW :
  forall P I (expected actual : VocabularyFiber P I)
         (snapshot : VocabularySnapshot P I actual) units,
    expected <> actual ->
    construct_bound_hot_traversal_view
      P I expected actual snapshot units = None.
Proof.
  intros P I expected actual snapshot units Hdifferent.
  unfold construct_bound_hot_traversal_view.
  rewrite (VWENC_237_MISMATCHED_VOCABULARY_FIBERS_ARE_REJECTED_BEFORE_TRAVERSAL
    P I expected actual Hdifferent).
  reflexivity.
Qed.

Theorem VWENC_249_BOUND_HOT_VIEWS_CONTAIN_ONLY_EXACT_FIXED_WIDTH_UNITS :
  (forall P I (expected actual : VocabularyFiber P I)
          (snapshot : VocabularySnapshot P I actual)
          (view : BoundHotTraversalView P I expected actual snapshot),
    map (@length PhysicalByte)
      (run_interned_hot_view P I expected actual snapshot view) =
      repeat (carrier_width_bytes I)
        (length (hot_view_units P I expected actual snapshot view)) /\
    Forall
      (fun bound =>
        exists atom,
          In (atom,
              snapshot_bound_symbol_id P I actual snapshot bound)
            (vocabulary_snapshot_live_entries P I actual snapshot))
      (hot_view_units P I expected actual snapshot view)) /\
  (forall domain (view : DirectHotTraversalView domain),
    map (@length PhysicalByte) (run_direct_hot_view domain view) =
      repeat (direct_byte_width (direct_codec_profile domain))
        (length (direct_hot_view_units domain view))).
Proof.
  split.
  - intros P I expected actual snapshot view. split.
    + unfold run_interned_hot_view.
      apply bound_kernel_widths_are_constant.
    + induction (hot_view_units P I expected actual snapshot view)
        as [|bound rest IH]; constructor.
      * exact (snapshot_bound_live P I actual snapshot bound).
      * exact IH.
  - intros domain view. unfold run_direct_hot_view.
    apply bound_kernel_widths_are_constant.
Qed.

End VariableWidthFamilyRefinementSpec.
