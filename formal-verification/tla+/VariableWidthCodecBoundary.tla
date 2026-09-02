---------------------- MODULE VariableWidthCodecBoundary ----------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC

(*
  Incremental logical-unit decoder for variable-width dictionary profiles.
  Every ReadPhysicalByte action consumes one actual input byte, updates an
  explicit codeword buffer, validates the real ULEB/UTF-8 grammar, and emits
  only when that codeword is complete. Adjacent valid atoms exercise buffer
  reset and boundary preservation. TLC's finite scenarios bound model checking
  only; they are not workload limits in the library contract.

  The unsafe constants weaken the actual decoder rule they name:

    AcceptOverlongUleb       removes ULEB minimality at termination;
    AcceptUnterminatedUleb   emits a buffered ULEB at end of input;
    AcceptUtf8Continuation   treats a continuation byte as a width-one scalar;
    ExposePhysicalCodecBytes appends each consumed codec byte to the public
                             logical output instead of the decoded atom.

  Pure ULEB ordering and F64 raw-bit identity are proved, with their mutants,
  in Rocq. They are deliberately absent from this temporal boundary model.
*)

CONSTANTS AcceptOverlongUleb,
          AcceptUnterminatedUleb,
          AcceptUtf8Continuation,
          ExposePhysicalCodecBytes

ASSUME /\ AcceptOverlongUleb \in BOOLEAN
       /\ AcceptUnterminatedUleb \in BOOLEAN
       /\ AcceptUtf8Continuation \in BOOLEAN
       /\ ExposePhysicalCodecBytes \in BOOLEAN

Scenarios == {
  "UlebAdjacent",
  "UlebOverlong",
  "UlebUnterminated",
  "Utf8Adjacent",
  "Utf8Continuation",
  "DirectByte"
}

InvalidScenarios == {
  "UlebOverlong", "UlebUnterminated", "Utf8Continuation"
}

Profile(scenario) ==
  CASE scenario \in {"UlebAdjacent", "UlebOverlong", "UlebUnterminated"}
         -> "ULEB"
    [] scenario \in {"Utf8Adjacent", "Utf8Continuation"} -> "UTF8"
    [] OTHER -> "DirectByte"

InputBytes(scenario) ==
  CASE scenario = "UlebAdjacent"     -> <<129, 1, 2>>
    [] scenario = "UlebOverlong"      -> <<129, 0>>
    [] scenario = "UlebUnterminated"  -> <<129>>
    [] scenario = "Utf8Adjacent"      -> <<195, 169, 65>>
    [] scenario = "Utf8Continuation"  -> <<169>>
    [] OTHER                            -> <<169, 1>>

ExpectedOutput(scenario) ==
  CASE scenario = "UlebAdjacent" ->
         <<[kind |-> "ULEB", bytes |-> <<129, 1>>, scalar |-> 0],
           [kind |-> "ULEB", bytes |-> <<2>>, scalar |-> 0]>>
    [] scenario = "Utf8Adjacent"  ->
         <<[kind |-> "UnicodeScalar", bytes |-> <<>>, scalar |-> 233],
           [kind |-> "UnicodeScalar", bytes |-> <<>>, scalar |-> 65]>>
    [] scenario = "DirectByte"    ->
         <<[kind |-> "Byte", bytes |-> <<>>, scalar |-> 169],
           [kind |-> "Byte", bytes |-> <<>>, scalar |-> 1]>>
    [] OTHER                        -> <<>>

IsVariableWidth(scenario) == Profile(scenario) \in {"ULEB", "UTF8"}
IsContinuation(byte) == 128 <= byte /\ byte < 192
LastElement(sequence) == sequence[Len(sequence)]

UlebPrefixShape(bytes) ==
  /\ Len(bytes) > 0
  /\ \A index \in 1..(Len(bytes) - 1):
       128 <= bytes[index] /\ bytes[index] < 256

UlebTerminated(bytes) == LastElement(bytes) < 128
UlebOverlong(bytes) == Len(bytes) > 1 /\ (LastElement(bytes) % 128) = 0

UlebStatus(bytes) ==
  IF ~UlebPrefixShape(bytes) \/ LastElement(bytes) >= 256
  THEN "RejectInvalidUleb"
  ELSE IF ~UlebTerminated(bytes)
       THEN "NeedMore"
       ELSE IF UlebOverlong(bytes) /\ ~AcceptOverlongUleb
            THEN "RejectOverlongUleb"
            ELSE "Emit"

Utf8ExpectedWidth(first) ==
  IF first < 128 THEN 1
  ELSE IF 194 <= first /\ first < 224 THEN 2
  ELSE IF 224 <= first /\ first < 240 THEN 3
  ELSE IF 240 <= first /\ first < 245 THEN 4
  ELSE IF AcceptUtf8Continuation /\ IsContinuation(first) THEN 1
  ELSE 0

Utf8ContinuationPrefix(bytes) ==
  \A index \in 2..Len(bytes): IsContinuation(bytes[index])

Utf8CanonicalComplete(bytes) ==
  LET width == Len(bytes)
      first == bytes[1]
  IN CASE width = 1 ->
            first < 128 \/ (AcceptUtf8Continuation /\ IsContinuation(first))
       [] width = 2 -> 194 <= first /\ first < 224
       [] width = 3 ->
            /\ 224 <= first /\ first < 240
            /\ IF first = 224 THEN bytes[2] >= 160 ELSE TRUE
            /\ IF first = 237 THEN bytes[2] < 160 ELSE TRUE
       [] width = 4 ->
            /\ 240 <= first /\ first < 245
            /\ IF first = 240 THEN bytes[2] >= 144 ELSE TRUE
            /\ IF first = 244 THEN bytes[2] < 144 ELSE TRUE
       [] OTHER -> FALSE

Utf8Value(bytes) ==
  CASE Len(bytes) = 1 -> bytes[1]
    [] Len(bytes) = 2 -> (bytes[1] % 32) * 64 + (bytes[2] % 64)
    [] Len(bytes) = 3 ->
         (bytes[1] % 16) * 4096 +
         (bytes[2] % 64) * 64 +
         (bytes[3] % 64)
    [] Len(bytes) = 4 ->
         (bytes[1] % 8) * 262144 +
         (bytes[2] % 64) * 4096 +
         (bytes[3] % 64) * 64 +
         (bytes[4] % 64)
    [] OTHER -> 0

Utf8Status(bytes) ==
  LET width == Utf8ExpectedWidth(bytes[1])
  IN IF width = 0 \/ Len(bytes) > width \/ ~Utf8ContinuationPrefix(bytes)
     THEN "RejectInvalidUtf8"
     ELSE IF Len(bytes) < width
          THEN "NeedMore"
          ELSE IF Utf8CanonicalComplete(bytes)
               THEN "Emit"
               ELSE "RejectInvalidUtf8"

CodewordStatus(profile, bytes) ==
  CASE profile = "ULEB" -> UlebStatus(bytes)
    [] profile = "UTF8" -> Utf8Status(bytes)
    [] OTHER -> "Emit"

DecodedAtom(profile, bytes) ==
  CASE profile = "ULEB" ->
         [kind |-> "ULEB", bytes |-> bytes, scalar |-> 0]
    [] profile = "UTF8" ->
         [kind |-> "UnicodeScalar", bytes |-> <<>>,
          scalar |-> Utf8Value(bytes)]
    [] OTHER ->
         [kind |-> "Byte", bytes |-> <<>>,
          scalar |-> LastElement(bytes)]

RejectStatuses == {
  "RejectInvalidUleb", "RejectOverlongUleb", "RejectInvalidUtf8"
}

RejectError(status) ==
  CASE status = "RejectOverlongUleb" -> "NonCanonicalUleb"
    [] status = "RejectInvalidUleb"  -> "InvalidUleb"
    [] OTHER                         -> "InvalidUtf8"

VARIABLES scenario, phase, cursor, codewordBuffer, logicalOutput,
          completedAtoms, decoderError

vars == <<scenario, phase, cursor, codewordBuffer, logicalOutput,
          completedAtoms, decoderError>>

Init ==
  /\ scenario \in Scenarios
  /\ phase = "Reading"
  /\ cursor = 1
  /\ codewordBuffer = <<>>
  /\ logicalOutput = <<>>
  /\ completedAtoms = 0
  /\ decoderError = "None"

ReadPhysicalByte ==
  /\ phase = "Reading"
  /\ cursor <= Len(InputBytes(scenario))
  /\ LET byte == InputBytes(scenario)[cursor]
         nextBuffer == Append(codewordBuffer, byte)
         status == CodewordStatus(Profile(scenario), nextBuffer)
         exposedOutput ==
           IF ExposePhysicalCodecBytes /\ IsVariableWidth(scenario)
           THEN Append(logicalOutput, byte)
           ELSE logicalOutput
     IN /\ cursor' = cursor + 1
        /\ CASE status = "NeedMore" ->
                  /\ phase' = "Reading"
                  /\ codewordBuffer' = nextBuffer
                  /\ logicalOutput' = exposedOutput
                  /\ completedAtoms' = completedAtoms
                  /\ decoderError' = "None"
             [] status = "Emit" ->
                  /\ phase' = "Reading"
                  /\ codewordBuffer' = <<>>
                  /\ logicalOutput' =
                       IF ExposePhysicalCodecBytes /\ IsVariableWidth(scenario)
                       THEN exposedOutput
                       ELSE Append(logicalOutput,
                                   DecodedAtom(Profile(scenario), nextBuffer))
                  /\ completedAtoms' = completedAtoms + 1
                  /\ decoderError' = "None"
             [] status \in RejectStatuses ->
                  /\ phase' = "Rejected"
                  /\ codewordBuffer' = nextBuffer
                  /\ logicalOutput' = exposedOutput
                  /\ completedAtoms' = completedAtoms
                  /\ decoderError' = RejectError(status)
  /\ UNCHANGED scenario

FinalizeInput ==
  /\ phase = "Reading"
  /\ cursor > Len(InputBytes(scenario))
  /\ IF codewordBuffer = <<>>
     THEN /\ phase' = "Done"
          /\ UNCHANGED <<cursor, codewordBuffer, logicalOutput,
                         completedAtoms, decoderError>>
     ELSE IF Profile(scenario) = "ULEB" /\ AcceptUnterminatedUleb
          THEN /\ phase' = "Done"
               /\ codewordBuffer' = <<>>
               /\ logicalOutput' =
                    IF ExposePhysicalCodecBytes
                    THEN logicalOutput
                    ELSE Append(logicalOutput,
                           [kind |-> "ULEB", bytes |-> codewordBuffer,
                            scalar |-> 0])
               /\ completedAtoms' = completedAtoms + 1
               /\ decoderError' = "None"
               /\ UNCHANGED cursor
          ELSE /\ phase' = "Rejected"
               /\ decoderError' =
                    IF Profile(scenario) = "ULEB"
                    THEN "UnterminatedUleb"
                    ELSE "TruncatedUtf8"
               /\ UNCHANGED <<cursor, codewordBuffer, logicalOutput,
                              completedAtoms>>
  /\ UNCHANGED scenario

DecodeStep == ReadPhysicalByte \/ FinalizeInput

TerminalStutter ==
  /\ phase \in {"Done", "Rejected"}
  /\ UNCHANGED vars

Next == DecodeStep \/ TerminalStutter
Spec == Init /\ [][Next]_vars /\ WF_vars(DecodeStep)

IsByteSequence(sequence) ==
  /\ DOMAIN sequence = 1..Len(sequence)
  /\ \A index \in DOMAIN sequence: sequence[index] \in 0..255

IsLogicalAtom(atom) ==
  /\ atom.kind \in {"ULEB", "UnicodeScalar", "Byte"}
  /\ IsByteSequence(atom.bytes)
  /\ atom.scalar \in Nat
  /\ atom.kind = "ULEB" => atom.bytes # <<>>
  /\ atom.kind # "ULEB" => atom.bytes = <<>>

TypeOK ==
  /\ scenario \in Scenarios
  /\ phase \in {"Reading", "Done", "Rejected"}
  /\ cursor \in 1..(Len(InputBytes(scenario)) + 1)
  /\ codewordBuffer \in Seq(0..255)
  /\ DOMAIN logicalOutput = 1..Len(logicalOutput)
  /\ \A index \in DOMAIN logicalOutput: IsLogicalAtom(logicalOutput[index])
  /\ completedAtoms \in 0..(Len(InputBytes(scenario)) + 1)
  /\ decoderError \in {
       "None", "NonCanonicalUleb", "InvalidUleb", "UnterminatedUleb",
       "InvalidUtf8", "TruncatedUtf8"
     }

VWENC_22_NO_LOGICAL_TRANSITION_BEFORE_COMPLETE_CODEWORD ==
  ~ExposePhysicalCodecBytes =>
    logicalOutput = SubSeq(ExpectedOutput(scenario), 1, completedAtoms)

VWENC_23_SUCCESS_EMITS_EXACT_LOGICAL_STREAM ==
  phase = "Done" => logicalOutput = ExpectedOutput(scenario)

VWENC_24_CODEC_BYTES_NEVER_BECOME_LOGICAL_TRANSITIONS ==
  phase = "Done" /\ IsVariableWidth(scenario) =>
    logicalOutput = ExpectedOutput(scenario)

VWENC_25_DIRECT_BYTE_SEMANTICS_IS_EXPLICIT ==
  phase = "Done" /\ scenario = "DirectByte" =>
    logicalOutput = ExpectedOutput("DirectByte")

VWENC_26_OVERLONG_ULEB_IS_REJECTED ==
  scenario = "UlebOverlong" => phase # "Done"

VWENC_27_UNTERMINATED_ULEB_IS_REJECTED ==
  scenario = "UlebUnterminated" => phase # "Done"

VWENC_28_UTF8_CONTINUATION_IS_REJECTED ==
  scenario = "Utf8Continuation" => phase # "Done"

VWENC_29_REJECTION_IS_EXPLICIT_AND_HAS_NO_LOGICAL_OUTPUT ==
  phase = "Rejected" /\ scenario \in InvalidScenarios =>
    /\ decoderError # "None"
    /\ logicalOutput = <<>>

VWENC_32_CURSOR_AND_BUFFER_ARE_BOUNDED_BY_CONSUMED_INPUT ==
  /\ cursor <= Len(InputBytes(scenario)) + 1
  /\ Len(codewordBuffer) <= cursor - 1
  /\ completedAtoms <= cursor - 1

VWENC_80_ADJACENT_CODEWORDS_PRESERVE_EVERY_BOUNDARY ==
  phase = "Done" /\ scenario \in {"UlebAdjacent", "Utf8Adjacent"} =>
    /\ completedAtoms = 2
    /\ logicalOutput = ExpectedOutput(scenario)

VWENC_81_INCOMPLETE_BUFFER_NEVER_INCREMENTS_COMPLETED_ATOMS ==
  phase = "Reading" /\ codewordBuffer # <<>> =>
    completedAtoms < cursor

VWENC_82_DECODER_EVENTUALLY_TERMINATES ==
  <>(phase \in {"Done", "Rejected"})

=============================================================================
