------------------ MODULE VariableWidthFamilyRefinement ------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC

(***************************************************************************)
(* Temporal refinement gate for dictionary profiles and consumer views.    *)
(*                                                                         *)
(* Three initial witnesses are explored: a direct profile, an interned      *)
(* profile bound to the matching vocabulary snapshot, and an interned       *)
(* profile presented with a mismatched snapshot. Direct traversal needs no  *)
(* vocabulary. Matching interned traversal binds before any logical         *)
(* projection. A mismatch terminates as Rejected without observations.      *)
(*                                                                         *)
(* The finite witness bounds TLC exploration only; it is not a workload or  *)
(* resource limit in libdictenstein. Each Boolean constant weakens one       *)
(* protocol decision. The verification harness checks the named failure and *)
(* independently proves that every non-target invariant remains true.       *)
(***************************************************************************)

CONSTANTS ExposeCodecBytes,
          SplitUtf8Scalar,
          AllowInteriorSuffixStart,
          DivergeSpecializedKernel,
          InferFormatIdentityFromTypeName,
          AcceptMismatchedVocabularyFiber

ASSUME /\ ExposeCodecBytes \in BOOLEAN
       /\ SplitUtf8Scalar \in BOOLEAN
       /\ AllowInteriorSuffixStart \in BOOLEAN
       /\ DivergeSpecializedKernel \in BOOLEAN
       /\ InferFormatIdentityFromTypeName \in BOOLEAN
       /\ AcceptMismatchedVocabularyFiber \in BOOLEAN

Modes == {"Direct", "InternedMatching", "InternedMismatched"}

Phases == {
  "Ready", "Bound", "Rejected", "FormatChecked", "Projected",
  "SuffixChecked", "KernelChecked", "Done"
}

BindingStatuses == {"Unchecked", "NotRequired", "Accepted", "Rejected"}

PhysicalUtf8 == <<195, 169>>
CodewordBoundaries(selectedMode) ==
  IF selectedMode = "Direct" THEN {0, 2} ELSE {0, 4}

UnicodeScalarLabel == [kind |-> "UnicodeScalar", value |-> 233]
SymbolIdLabel == [kind |-> "SymbolIdU32", value |-> 7]
PhysicalLabel(byte) == [kind |-> "PhysicalByte", value |-> byte]
LeadHalfLabel == [kind |-> "Utf8LeadHalf", value |-> 0]
ContinuationHalfLabel == [kind |-> "Utf8ContinuationHalf", value |-> 0]

LogicalLabelUniverse ==
  {UnicodeScalarLabel, SymbolIdLabel, LeadHalfLabel, ContinuationHalfLabel} \cup
  {PhysicalLabel(byte) : byte \in 0..255}

LogicalUnitLabel(selectedMode) ==
  IF selectedMode = "Direct" THEN UnicodeScalarLabel ELSE SymbolIdLabel

LogicalUnitName(selectedMode) ==
  IF selectedMode = "Direct" THEN "U+00E9" ELSE "SymbolIdU32(7)"

BaselineObservation(selectedMode) ==
  [membership |-> TRUE,
   terminal |-> TRUE,
   mappedValue |-> 7,
   outgoing |-> <<LogicalUnitLabel(selectedMode)>>,
   prefixEntries |-> <<LogicalUnitName(selectedMode)>>,
   enumeration |-> <<LogicalUnitName(selectedMode)>>,
   substring |-> <<LogicalUnitName(selectedMode)>>,
   suffix |-> <<LogicalUnitName(selectedMode)>>]

DivergedObservation(selectedMode) ==
  [BaselineObservation(selectedMode) EXCEPT !.mappedValue = 8]

CanonicalFormat(selectedMode) ==
  IF selectedMode = "Direct"
  THEN [backend |-> "DynamicDawgFamily",
        profile |-> "DirectUnicodeScalarDomain",
        codec |-> "FamilyExistingCharU32Codec",
        layout |-> "GenericLogicalLayout",
        version |-> 1]
  ELSE [backend |-> "DynamicDawgFamily",
        profile |-> "InternedCanonicalUlebDomainU32",
        codec |-> "FamilyFixedIdCarrierCodecU32",
        layout |-> "ProspectiveInternedIdLayoutU32",
        version |-> 1]

TypeNameDerivedFormat(selectedMode) ==
  [backend |-> "RustTypeNameHash",
   profile |-> IF selectedMode = "Direct"
               THEN "DynamicDawgChar"
               ELSE "DynamicDawgInternedU32",
   codec |-> "Implicit",
   layout |-> "Implicit",
   version |-> 1]

NoFormat ==
  [backend |-> "None", profile |-> "None", codec |-> "None",
   layout |-> "None", version |-> 0]

VocabularyFiber(generation) ==
  [identity |-> "Vocabulary-A",
   generation |-> generation,
   atomProfile |-> "CanonicalULEB",
   codec |-> "CanonicalULEB-v1",
   layout |-> "LogicalUnit-v1",
   abiVersion |-> 1,
   carrierFormat |-> 32,
   carrierWidth |-> 4]

NoFiber ==
  [identity |-> "None",
   generation |-> 0,
   atomProfile |-> "None",
   codec |-> "None",
   layout |-> "None",
   abiVersion |-> 0,
   carrierFormat |-> 0,
   carrierWidth |-> 0]

VARIABLES mode, phase, bindingStatus, logicalLabels, suffixStart,
          genericObservation, specializedObservation, explicitFormat,
          readFormat, expectedFiber, actualFiber

vars == <<mode, phase, bindingStatus, logicalLabels, suffixStart,
          genericObservation, specializedObservation, explicitFormat,
          readFormat, expectedFiber, actualFiber>>

Init ==
  /\ mode \in Modes
  /\ phase = "Ready"
  /\ bindingStatus = "Unchecked"
  /\ logicalLabels = <<>>
  /\ suffixStart = 0
  /\ genericObservation = BaselineObservation(mode)
  /\ specializedObservation = BaselineObservation(mode)
  /\ explicitFormat = CanonicalFormat(mode)
  /\ readFormat = NoFormat
  /\ expectedFiber = IF mode = "Direct" THEN NoFiber ELSE VocabularyFiber(1)
  /\ actualFiber =
       IF mode = "Direct" THEN NoFiber
       ELSE IF mode = "InternedMatching" THEN VocabularyFiber(1)
       ELSE VocabularyFiber(2)

BindProfile ==
  /\ phase = "Ready"
  /\ phase' =
       IF mode = "InternedMismatched" /\
          ~AcceptMismatchedVocabularyFiber
       THEN "Rejected"
       ELSE "Bound"
  /\ bindingStatus' =
       IF mode = "Direct" THEN "NotRequired"
       ELSE IF expectedFiber = actualFiber THEN "Accepted"
       ELSE IF AcceptMismatchedVocabularyFiber THEN "Accepted"
       ELSE "Rejected"
  /\ UNCHANGED <<mode, logicalLabels, suffixStart, genericObservation,
                  specializedObservation, explicitFormat, readFormat,
                  expectedFiber, actualFiber>>

LoadExplicitFormat ==
  /\ phase = "Bound"
  /\ phase' = "FormatChecked"
  /\ readFormat' =
       IF InferFormatIdentityFromTypeName
       THEN TypeNameDerivedFormat(mode)
       ELSE explicitFormat
  /\ UNCHANGED <<mode, bindingStatus, logicalLabels, suffixStart,
                  genericObservation, specializedObservation, explicitFormat,
                  expectedFiber, actualFiber>>

ProjectLogicalScalar ==
  /\ phase = "FormatChecked"
  /\ phase' = "Projected"
  /\ logicalLabels' =
       IF mode = "Direct"
       THEN IF ExposeCodecBytes
            THEN <<PhysicalLabel(PhysicalUtf8[1]),
                   PhysicalLabel(PhysicalUtf8[2])>>
            ELSE IF SplitUtf8Scalar
                 THEN <<LeadHalfLabel, ContinuationHalfLabel>>
                 ELSE <<UnicodeScalarLabel>>
       ELSE <<SymbolIdLabel>>
  /\ UNCHANGED <<mode, bindingStatus, suffixStart, genericObservation,
                  specializedObservation, explicitFormat, readFormat,
                  expectedFiber, actualFiber>>

SelectSuffixBoundary ==
  /\ phase = "Projected"
  /\ phase' = "SuffixChecked"
  /\ suffixStart' = IF AllowInteriorSuffixStart THEN 1 ELSE 0
  /\ UNCHANGED <<mode, bindingStatus, logicalLabels, genericObservation,
                  specializedObservation, explicitFormat, readFormat,
                  expectedFiber, actualFiber>>

CompareSpecializedKernel ==
  /\ phase = "SuffixChecked"
  /\ phase' = "KernelChecked"
  /\ specializedObservation' =
       IF DivergeSpecializedKernel
       THEN DivergedObservation(mode)
       ELSE genericObservation
  /\ UNCHANGED <<mode, bindingStatus, logicalLabels, suffixStart,
                  genericObservation, explicitFormat, readFormat,
                  expectedFiber, actualFiber>>

Finish ==
  /\ phase = "KernelChecked"
  /\ phase' = "Done"
  /\ UNCHANGED <<mode, bindingStatus, logicalLabels, suffixStart,
                  genericObservation, specializedObservation, explicitFormat,
                  readFormat, expectedFiber, actualFiber>>

Advance ==
  BindProfile \/ LoadExplicitFormat \/ ProjectLogicalScalar \/
  SelectSuffixBoundary \/ CompareSpecializedKernel \/ Finish

TerminalStutter ==
  /\ phase \in {"Done", "Rejected"}
  /\ UNCHANGED vars

Next == Advance \/ TerminalStutter
Spec == Init /\ [][Next]_vars /\ WF_vars(Advance)

TypeOK ==
  /\ mode \in Modes
  /\ phase \in Phases
  /\ bindingStatus \in BindingStatuses
  /\ logicalLabels \in Seq(LogicalLabelUniverse)
  /\ suffixStart \in 0..4
  /\ genericObservation = BaselineObservation(mode)
  /\ specializedObservation \in
       {BaselineObservation(mode), DivergedObservation(mode)}
  /\ explicitFormat = CanonicalFormat(mode)
  /\ readFormat \in
       {NoFormat, CanonicalFormat(mode), TypeNameDerivedFormat(mode)}
  /\ expectedFiber = IF mode = "Direct" THEN NoFiber ELSE VocabularyFiber(1)
  /\ actualFiber =
       IF mode = "Direct" THEN NoFiber
       ELSE IF mode = "InternedMatching" THEN VocabularyFiber(1)
       ELSE VocabularyFiber(2)

ProjectionCompleted ==
  phase \in {"Projected", "SuffixChecked", "KernelChecked", "Done"}

TraversalStarted == ProjectionCompleted

SuffixSelectionCompleted ==
  phase \in {"SuffixChecked", "KernelChecked", "Done"}

KernelComparisonCompleted == phase \in {"KernelChecked", "Done"}
FormatLoadCompleted ==
  phase \in {"FormatChecked", "Projected", "SuffixChecked",
             "KernelChecked", "Done"}

VWENC_241_CODEC_BYTES_NEVER_APPEAR_AS_LOGICAL_LABELS ==
  ~ProjectionCompleted \/
  \A index \in 1..Len(logicalLabels):
    logicalLabels[index].kind # "PhysicalByte"

VWENC_242_UTF8_SCALAR_IS_NEVER_SPLIT_ACROSS_LOGICAL_TRANSITIONS ==
  ~ProjectionCompleted \/
  \A index \in 1..Len(logicalLabels):
    logicalLabels[index].kind \notin {"Utf8LeadHalf", "Utf8ContinuationHalf"}

VWENC_243_SUFFIX_MATCHES_NEVER_BEGIN_INSIDE_A_LOGICAL_CODEWORD ==
  ~SuffixSelectionCompleted \/ suffixStart \in CodewordBoundaries(mode)

VWENC_244_SPECIALIZED_KERNEL_PRESERVES_THE_COMPLETE_OBSERVATION ==
  ~KernelComparisonCompleted \/
  specializedObservation = genericObservation

VWENC_245_FORMAT_IDENTITY_COMES_ONLY_FROM_EXPLICIT_PROFILE_METADATA ==
  ~FormatLoadCompleted \/ readFormat = explicitFormat

VWENC_246_MISMATCHED_VOCABULARY_FIBER_IS_REJECTED_BEFORE_TRAVERSAL ==
  /\ (bindingStatus = "Accepted" => expectedFiber = actualFiber)
  /\ (TraversalStarted =>
        mode = "Direct" \/
        (bindingStatus = "Accepted" /\ expectedFiber = actualFiber))
  /\ (mode = "InternedMismatched" =>
        /\ phase \in {"Ready", "Rejected"}
        /\ bindingStatus \in {"Unchecked", "Rejected"}
        /\ ~TraversalStarted
        /\ logicalLabels = <<>>)

FamilyRefinementEventuallyTerminates ==
  <>(phase \in {"Done", "Rejected"})

=============================================================================
