(** * Variable-width codec and logical-transition laws

    This module fixes the representation-independent contract for dictionary
    profiles before a Rust codec is introduced. Variable-width bytes are a
    storage grammar only. A dictionary transition observed by a language
    consumer denotes exactly one logical atom.

    Canonical ULEB128 is modeled as an arbitrary-length, little-endian sequence
    of seven-bit digits. Rocq naturals are unbounded, so no theorem silently
    restricts a value to a Rust primitive. UTF-8 codewords are canonical
    encodings of Unicode scalar values. Direct profiles expose a single fixed
    unit. F64Bits preserves raw IEEE-754 bit identity and orders those bits by
    the monotone key used by Rust's total_cmp rather than numeric equality.

    Stable theorem names beginning with [VWENC_] are machine-readable invariant
    IDs. The
    conformance ledger and property tests consume these exact identifiers.
*)

From Stdlib Require Import Arith Bool Lia List PeanoNat.
Require Import ARTrie.Spec.DynamicDawgMutationSpec.
Require Import ARTrie.Spec.DynamicDawgU64Spec.
Import ListNotations.

Module VariableWidthCodecSpec.

(** ** Canonical arbitrary-width ULEB128 *)

Definition PhysicalByte := nat.
Definition UlebDigit := nat.

Definition valid_byte (byte : PhysicalByte) : Prop := byte < 256.
Definition valid_uleb_digit (digit : UlebDigit) : Prop := digit < 128.

Fixpoint encode_uleb_digits (digits : list UlebDigit) : list PhysicalByte :=
  match digits with
  | [] => []
  | [digit] => [digit]
  | digit :: rest => (128 + digit) :: encode_uleb_digits rest
  end.

Definition uleb_payload (byte : PhysicalByte) : UlebDigit := byte mod 128.
Definition decode_uleb_payloads (bytes : list PhysicalByte) : list UlebDigit :=
  map uleb_payload bytes.

(** Every non-final byte continues; the final byte terminates. *)
Inductive uleb_continuation_shape : list PhysicalByte -> Prop :=
| UlebShapeLast : forall byte,
    byte < 128 ->
    uleb_continuation_shape [byte]
| UlebShapeMore : forall byte rest,
    128 <= byte ->
    byte < 256 ->
    uleb_continuation_shape rest ->
    uleb_continuation_shape (byte :: rest).

(** A multi-byte zero high digit is an overlong spelling. The singleton zero
    remains the canonical spelling of logical zero. *)
Definition canonical_uleb_digits (digits : list UlebDigit) : Prop :=
  digits <> [] /\
  Forall valid_uleb_digit digits /\
  (2 <= length digits -> last digits 0 <> 0).

Definition canonical_uleb_codeword (bytes : list PhysicalByte) : Prop :=
  uleb_continuation_shape bytes /\
  canonical_uleb_digits (decode_uleb_payloads bytes).

Lemma uleb_terminal_payload_identity :
  forall digit, valid_uleb_digit digit -> uleb_payload digit = digit.
Proof.
  intros digit Hdigit.
  unfold valid_uleb_digit, uleb_payload in *.
  apply Nat.mod_small. exact Hdigit.
Qed.

Lemma uleb_continuing_payload_identity :
  forall digit,
    valid_uleb_digit digit -> uleb_payload (128 + digit) = digit.
Proof.
  intros digit Hdigit.
  unfold valid_uleb_digit, uleb_payload in *.
  replace (128 + digit) with (digit + 128) by lia.
  rewrite Nat.Div0.add_mod by lia.
  rewrite Nat.Div0.mod_same by lia.
  rewrite Nat.add_0_r.
  rewrite Nat.Div0.mod_mod by lia.
  apply Nat.mod_small. exact Hdigit.
Qed.

Theorem VWENC_01_ULEB_PAYLOAD_ROUNDTRIP :
  forall digits,
    Forall valid_uleb_digit digits ->
    decode_uleb_payloads (encode_uleb_digits digits) = digits.
Proof.
  induction digits as [| digit rest IH]; intros Hvalid.
  - reflexivity.
  - inversion Hvalid as [| ? ? Hdigit Hrest]; subst.
    destruct rest as [| next tail].
    + change ([uleb_payload digit] = [digit]).
      rewrite uleb_terminal_payload_identity by exact Hdigit.
      reflexivity.
    + change
        (uleb_payload (128 + digit) ::
           decode_uleb_payloads (encode_uleb_digits (next :: tail)) =
         digit :: next :: tail).
      rewrite uleb_continuing_payload_identity by exact Hdigit.
      f_equal. apply IH. exact Hrest.
Qed.

Theorem VWENC_88_ULEB_CANONICAL_DIGIT_ENCODER_IS_INJECTIVE :
  forall left right,
    Forall valid_uleb_digit left ->
    Forall valid_uleb_digit right ->
    encode_uleb_digits left = encode_uleb_digits right ->
    left = right.
Proof.
  intros left right Hleft Hright Hencoded.
  apply (f_equal decode_uleb_payloads) in Hencoded.
  rewrite (VWENC_01_ULEB_PAYLOAD_ROUNDTRIP left Hleft) in Hencoded.
  rewrite (VWENC_01_ULEB_PAYLOAD_ROUNDTRIP right Hright) in Hencoded.
  exact Hencoded.
Qed.

Lemma encode_uleb_has_continuation_shape :
  forall digits,
    digits <> [] ->
    Forall valid_uleb_digit digits ->
    uleb_continuation_shape (encode_uleb_digits digits).
Proof.
  induction digits as [| digit rest IH]; intros Hnonempty Hvalid.
  - contradiction.
  - inversion Hvalid as [| ? ? Hdigit Hrest]; subst.
    unfold valid_uleb_digit in Hdigit.
    destruct rest as [| next tail].
    + simpl. constructor. exact Hdigit.
    + simpl. apply UlebShapeMore; [lia | lia |].
      apply IH; [discriminate | exact Hrest].
Qed.

Theorem VWENC_02_ULEB_CANONICAL_ENCODE :
  forall digits,
    canonical_uleb_digits digits ->
    canonical_uleb_codeword (encode_uleb_digits digits).
Proof.
  intros digits Hcanonical.
  destruct Hcanonical as [Hnonempty [Hvalid Hminimal]].
  split.
  - apply encode_uleb_has_continuation_shape; assumption.
  - unfold canonical_uleb_digits.
    rewrite VWENC_01_ULEB_PAYLOAD_ROUNDTRIP by exact Hvalid.
    repeat split; assumption.
Qed.

Theorem VWENC_03_ULEB_CODEWORDS_NONEMPTY :
  forall bytes, canonical_uleb_codeword bytes -> bytes <> [].
Proof.
  intros bytes [Hshape _] ->. inversion Hshape.
Qed.

Lemma uleb_continuing_byte_payload_identity :
  forall byte,
    128 <= byte -> byte < 256 -> 128 + uleb_payload byte = byte.
Proof.
  intros byte Hlower Hupper.
  assert (exists digit, byte = 128 + digit /\ digit < 128) as
      [digit [-> Hdigit]].
  { exists (byte - 128). split; lia. }
  rewrite uleb_continuing_payload_identity by exact Hdigit.
  reflexivity.
Qed.

Lemma decode_uleb_payloads_nonempty :
  forall bytes,
    uleb_continuation_shape bytes -> decode_uleb_payloads bytes <> [].
Proof.
  intros bytes Hshape.
  inversion Hshape; discriminate.
Qed.

Lemma encode_uleb_cons_with_nonempty_tail :
  forall digit tail,
    tail <> [] ->
    encode_uleb_digits (digit :: tail) =
    (128 + digit) :: encode_uleb_digits tail.
Proof.
  intros digit tail Hnonempty.
  destruct tail; [contradiction | reflexivity].
Qed.

Lemma uleb_shape_reencodes_payloads :
  forall bytes,
    uleb_continuation_shape bytes ->
    encode_uleb_digits (decode_uleb_payloads bytes) = bytes.
Proof.
  intros bytes Hshape.
  induction Hshape as [byte Hterminal | byte rest Hlower Hupper Hrest IH].
  - change ([uleb_payload byte] = [byte]).
    rewrite uleb_terminal_payload_identity by exact Hterminal.
    reflexivity.
  - change
      (encode_uleb_digits
         (uleb_payload byte :: decode_uleb_payloads rest) =
       byte :: rest).
    rewrite encode_uleb_cons_with_nonempty_tail.
    + rewrite uleb_continuing_byte_payload_identity by assumption.
      f_equal. exact IH.
    + apply decode_uleb_payloads_nonempty. exact Hrest.
Qed.

Theorem VWENC_04_ULEB_UNIQUE_DECODING :
  forall left right,
    uleb_continuation_shape left ->
    uleb_continuation_shape right ->
    decode_uleb_payloads left = decode_uleb_payloads right ->
    left = right.
Proof.
  intros left right Hleft Hright Hpayloads.
  rewrite <- (uleb_shape_reencodes_payloads left Hleft).
  rewrite <- (uleb_shape_reencodes_payloads right Hright).
  now rewrite Hpayloads.
Qed.

Lemma uleb_shape_final_byte_terminates :
  forall bytes default,
    uleb_continuation_shape bytes -> last bytes default < 128.
Proof.
  intros bytes default Hshape.
  induction Hshape as [byte Hterminal | byte rest Hlower Hupper Hrest IH].
  - exact Hterminal.
  - destruct Hrest; simpl; apply IH.
Qed.

Definition unterminated_uleb (bytes : list PhysicalByte) : Prop :=
  bytes <> [] /\ 128 <= last bytes 0.

Theorem VWENC_05_ULEB_UNTERMINATED_REJECTED :
  forall bytes,
    canonical_uleb_codeword bytes -> ~ unterminated_uleb bytes.
Proof.
  intros bytes [Hshape _] [_ Hcontinues].
  pose proof (uleb_shape_final_byte_terminates bytes 0 Hshape).
  lia.
Qed.

Definition overlong_uleb (bytes : list PhysicalByte) : Prop :=
  2 <= length bytes /\ last (decode_uleb_payloads bytes) 0 = 0.

Theorem VWENC_06_ULEB_OVERLONG_REJECTED :
  forall bytes,
    canonical_uleb_codeword bytes -> ~ overlong_uleb bytes.
Proof.
  intros bytes [_ [_ [_ Hminimal]]] [Hlength Hzero].
  apply Hminimal.
  - unfold decode_uleb_payloads. rewrite length_map. exact Hlength.
  - exact Hzero.
Qed.

Lemma uleb_shape_tail :
  forall byte rest,
    rest <> [] ->
    uleb_continuation_shape (byte :: rest) ->
    uleb_continuation_shape rest.
Proof.
  intros byte rest Hnonempty Hshape.
  inversion Hshape; subst.
  - contradiction.
  - assumption.
Qed.

Theorem VWENC_07_ULEB_EARLY_TERMINATOR_REJECTED :
  forall prefix terminal suffix,
    suffix <> [] ->
    terminal < 128 ->
    ~ uleb_continuation_shape (prefix ++ terminal :: suffix).
Proof.
  induction prefix as [| byte prefix IH]; intros terminal suffix Hsuffix Hterminal Hshape.
  - inversion Hshape; subst.
    + contradiction.
    + lia.
  - apply (IH terminal suffix Hsuffix Hterminal).
    apply (uleb_shape_tail byte (prefix ++ terminal :: suffix)).
    + destruct prefix; discriminate.
    + exact Hshape.
Qed.

Theorem VWENC_08_ULEB_EACH_BYTE_IS_U8 :
  forall bytes,
    uleb_continuation_shape bytes -> Forall valid_byte bytes.
Proof.
  intros bytes Hshape.
  induction Hshape as [byte Hterminal | byte rest Hlower Hupper Hrest IH].
  - constructor; [unfold valid_byte; lia | constructor].
  - constructor; [exact Hupper | exact IH].
Qed.

Theorem VWENC_09_ULEB_DECODING_IS_INPUT_BOUNDED :
  forall bytes,
    length (decode_uleb_payloads bytes) = length bytes.
Proof.
  intros bytes. apply length_map.
Qed.

Fixpoint byte_sequence_eqb
    (left right : list PhysicalByte) : bool :=
  match left, right with
  | [], [] => true
  | left_byte :: left_rest, right_byte :: right_rest =>
      (left_byte =? right_byte) &&
      byte_sequence_eqb left_rest right_rest
  | _, _ => false
  end.

Lemma byte_sequence_eqb_reflects_equality :
  forall left right,
    byte_sequence_eqb left right = true <-> left = right.
Proof.
  induction left as [| left_byte left_rest IH];
    destruct right as [| right_byte right_rest]; simpl.
  - tauto.
  - split; [discriminate | discriminate].
  - split; [discriminate | discriminate].
  - rewrite andb_true_iff, Nat.eqb_eq, IH.
    split.
    + intros [-> ->]. reflexivity.
    + intros Hequal. inversion Hequal. tauto.
Qed.

Fixpoint uleb_continuation_shapeb
    (bytes : list PhysicalByte) : bool :=
  match bytes with
  | [] => false
  | [byte] => byte <? 128
  | byte :: rest =>
      (128 <=? byte) && (byte <? 256) &&
      uleb_continuation_shapeb rest
  end.

Lemma uleb_continuation_shapeb_reflects_shape :
  forall bytes,
    uleb_continuation_shapeb bytes = true <->
    uleb_continuation_shape bytes.
Proof.
  induction bytes as [| byte rest IH].
  - simpl. split; [discriminate | intros Hshape; inversion Hshape].
  - destruct rest as [| next tail].
    + simpl. rewrite Nat.ltb_lt.
      split.
      * intros Hterminal. constructor. exact Hterminal.
      * intros Hshape.
        exact (uleb_shape_final_byte_terminates [byte] 0 Hshape).
    + change
        (((128 <=? byte) && (byte <? 256) &&
          uleb_continuation_shapeb (next :: tail)) = true <->
         uleb_continuation_shape (byte :: next :: tail)).
      rewrite !andb_true_iff, Nat.leb_le, Nat.ltb_lt, IH.
      split.
      * intros [[Hlower Hupper] Htail].
        now apply UlebShapeMore.
      * intros Hshape. inversion Hshape; subst. tauto.
Qed.

Definition canonical_uleb_minimalb (digits : list UlebDigit) : bool :=
  (length digits <? 2) || negb (last digits 0 =? 0).

Lemma canonical_uleb_minimalb_reflects_minimality :
  forall digits,
    canonical_uleb_minimalb digits = true <->
    (2 <= length digits -> last digits 0 <> 0).
Proof.
  intros digits.
  unfold canonical_uleb_minimalb.
  rewrite orb_true_iff, negb_true_iff, Nat.ltb_lt, Nat.eqb_neq.
  split.
  - intros [Hshort | Hlast] Hmultiple; [lia | exact Hlast].
  - intros Hminimal.
    destruct (Nat.lt_ge_cases (length digits) 2) as [Hshort | Hmultiple].
    + now left.
    + right. apply Hminimal. exact Hmultiple.
Qed.

Lemma uleb_payloads_are_digits :
  forall bytes,
    Forall valid_uleb_digit (decode_uleb_payloads bytes).
Proof.
  induction bytes as [| byte rest IH].
  - constructor.
  - constructor.
    + unfold valid_uleb_digit, uleb_payload.
      apply Nat.mod_upper_bound. lia.
    + exact IH.
Qed.

Definition canonical_uleb_codewordb
    (bytes : list PhysicalByte) : bool :=
  uleb_continuation_shapeb bytes &&
  canonical_uleb_minimalb (decode_uleb_payloads bytes).

Theorem VWENC_33_ULEB_CANONICAL_RECOGNIZER_IS_EXACT :
  forall bytes,
    canonical_uleb_codewordb bytes = true <->
    canonical_uleb_codeword bytes.
Proof.
  intros bytes.
  unfold canonical_uleb_codewordb, canonical_uleb_codeword.
  rewrite andb_true_iff,
    uleb_continuation_shapeb_reflects_shape,
    canonical_uleb_minimalb_reflects_minimality.
  split.
  - intros [Hshape Hminimal]. split; [exact Hshape |].
    unfold canonical_uleb_digits.
    repeat split.
    + apply decode_uleb_payloads_nonempty. exact Hshape.
    + apply uleb_payloads_are_digits.
    + exact Hminimal.
  - intros [Hshape [_ [_ Hminimal]]]. tauto.
Qed.

Definition decode_canonical_uleb
    (bytes : list PhysicalByte) : option (list UlebDigit) :=
  if canonical_uleb_codewordb bytes
  then Some (decode_uleb_payloads bytes)
  else None.

Theorem VWENC_34_ULEB_DECODER_ACCEPTS_EXACTLY_CANONICAL_CODEWORDS :
  forall bytes,
    canonical_uleb_codeword bytes <->
    decode_canonical_uleb bytes = Some (decode_uleb_payloads bytes).
Proof.
  intros bytes.
  unfold decode_canonical_uleb.
  destruct (canonical_uleb_codewordb bytes) eqn:Hcanonical.
  - rewrite VWENC_33_ULEB_CANONICAL_RECOGNIZER_IS_EXACT in Hcanonical.
    tauto.
  - split.
    + intros Hcodeword.
      apply VWENC_33_ULEB_CANONICAL_RECOGNIZER_IS_EXACT in Hcodeword.
      rewrite Hcodeword in Hcanonical. discriminate.
    + discriminate.
Qed.

Theorem VWENC_89_ULEB_DECODER_ROUNDTRIPS_CANONICAL_ENCODER :
  forall digits,
    canonical_uleb_digits digits ->
    decode_canonical_uleb (encode_uleb_digits digits) = Some digits.
Proof.
  intros digits Hcanonical.
  assert (canonical_uleb_codeword (encode_uleb_digits digits))
    as Hcodeword.
  { now apply VWENC_02_ULEB_CANONICAL_ENCODE. }
  apply VWENC_34_ULEB_DECODER_ACCEPTS_EXACTLY_CANONICAL_CODEWORDS
    in Hcodeword.
  rewrite Hcodeword.
  destruct Hcanonical as [Hnonempty [Hvalid Hminimal]].
  now rewrite VWENC_01_ULEB_PAYLOAD_ROUNDTRIP.
Qed.

Theorem VWENC_35_ULEB_NONCANONICAL_AND_MALFORMED_INPUT_IS_REJECTED :
  forall bytes,
    ~ canonical_uleb_codeword bytes ->
    decode_canonical_uleb bytes = None.
Proof.
  intros bytes Hnoncanonical.
  unfold decode_canonical_uleb.
  destruct (canonical_uleb_codewordb bytes) eqn:Hcanonical.
  - apply VWENC_33_ULEB_CANONICAL_RECOGNIZER_IS_EXACT in Hcanonical.
    contradiction.
  - reflexivity.
Qed.

Theorem VWENC_36_ULEB_ENCODER_HAS_NO_BUILTIN_WIDTH_LIMIT :
  forall digits,
    length (encode_uleb_digits digits) = length digits.
Proof.
  induction digits as [| digit rest IH].
  - reflexivity.
  - destruct rest as [| next tail].
    + reflexivity.
    + simpl. f_equal. exact IH.
Qed.

Fixpoint uleb_value (digits : list UlebDigit) : nat :=
  match digits with
  | [] => 0
  | digit :: rest => digit + 128 * uleb_value rest
  end.

(** Compare equal-width canonical codewords from their most-significant
    payloads toward their least-significant payloads.  The recursion visits
    the physical bytes themselves and never materializes a bounded integer or
    a BigUint.  The unequal-list cases make the function total; the public
    comparator below selects them only after an explicit width comparison. *)
Fixpoint compare_equal_width_uleb_bytes
    (left right : list PhysicalByte) : comparison :=
  match left, right with
  | [], [] => Eq
  | [], _ => Lt
  | _, [] => Gt
  | left_byte :: left_rest, right_byte :: right_rest =>
      match compare_equal_width_uleb_bytes left_rest right_rest with
      | Eq => Nat.compare
                (uleb_payload left_byte) (uleb_payload right_byte)
      | Lt => Lt
      | Gt => Gt
      end
  end.

Definition compare_uleb_codewords_structural
    (left right : list PhysicalByte) : comparison :=
  match Nat.compare (length left) (length right) with
  | Eq => compare_equal_width_uleb_bytes left right
  | Lt => Lt
  | Gt => Gt
  end.

(** Reverse-index machine used as the production correspondence. A Rust
    implementation is a [while remaining != 0] loop over two borrowed slices:
    decrement [remaining], mask the two indexed bytes, and return at the first
    difference. It owns one index and one comparison only: O(n) time, O(1)
    auxiliary state, no allocation, and no call-stack growth. The tail-recursive
    Rocq evaluator below is a mathematical iterator; it is not an instruction
    to extract recursive Rust. *)
Fixpoint compare_equal_width_uleb_reverse_index
    (remaining : nat)
    (left right : list PhysicalByte) : comparison :=
  match remaining with
  | 0 => Eq
  | S index =>
      match Nat.compare
        (uleb_payload (nth index left 0))
        (uleb_payload (nth index right 0)) with
      | Eq => compare_equal_width_uleb_reverse_index index left right
      | Lt => Lt
      | Gt => Gt
      end
  end.

Lemma compare_reverse_index_cons :
  forall remaining left_byte left_rest right_byte right_rest,
    compare_equal_width_uleb_reverse_index
      (S remaining) (left_byte :: left_rest) (right_byte :: right_rest) =
    match compare_equal_width_uleb_reverse_index
            remaining left_rest right_rest with
    | Eq => Nat.compare
              (uleb_payload left_byte) (uleb_payload right_byte)
    | Lt => Lt
    | Gt => Gt
    end.
Proof.
  induction remaining as [| remaining IH];
    intros left_byte left_rest right_byte right_rest.
  - simpl.
    destruct
      (Nat.compare (uleb_payload left_byte) (uleb_payload right_byte));
      reflexivity.
  - change
      (match Nat.compare
         (uleb_payload (nth remaining left_rest 0))
         (uleb_payload (nth remaining right_rest 0)) with
       | Eq =>
           compare_equal_width_uleb_reverse_index
             (S remaining) (left_byte :: left_rest)
             (right_byte :: right_rest)
       | Lt => Lt
       | Gt => Gt
       end =
       match
         (match Nat.compare
            (uleb_payload (nth remaining left_rest 0))
            (uleb_payload (nth remaining right_rest 0)) with
          | Eq =>
              compare_equal_width_uleb_reverse_index
                remaining left_rest right_rest
          | Lt => Lt
          | Gt => Gt
          end)
       with
       | Eq => Nat.compare
                 (uleb_payload left_byte) (uleb_payload right_byte)
       | Lt => Lt
       | Gt => Gt
       end).
    destruct
      (Nat.compare
        (uleb_payload (nth remaining left_rest 0))
        (uleb_payload (nth remaining right_rest 0))) eqn:Hhighest;
      [apply IH | reflexivity | reflexivity].
Qed.

Lemma compare_reverse_index_agrees_with_structural :
  forall left right,
    length left = length right ->
    compare_equal_width_uleb_reverse_index (length left) left right =
      compare_equal_width_uleb_bytes left right.
Proof.
  induction left as [| left_byte left_rest IH];
    destruct right as [| right_byte right_rest];
    intros Hlength; try discriminate; [reflexivity |].
  simpl in Hlength. injection Hlength as Hrest_length.
  change
    (compare_equal_width_uleb_reverse_index
       (S (length left_rest)) (left_byte :: left_rest)
       (right_byte :: right_rest) =
     match compare_equal_width_uleb_bytes left_rest right_rest with
     | Eq => Nat.compare
               (uleb_payload left_byte) (uleb_payload right_byte)
     | Lt => Lt
     | Gt => Gt
     end).
  rewrite compare_reverse_index_cons.
  rewrite (IH right_rest Hrest_length).
  reflexivity.
Qed.

Definition compare_uleb_codewords
    (left right : list PhysicalByte) : comparison :=
  match Nat.compare (length left) (length right) with
  | Eq => compare_equal_width_uleb_reverse_index (length left) left right
  | Lt => Lt
  | Gt => Gt
  end.

Theorem VWENC_95_REVERSE_INDEX_ULEB_COMPARATOR_REFINES_STRUCTURAL_SPEC :
  forall left right,
    compare_uleb_codewords left right =
      compare_uleb_codewords_structural left right.
Proof.
  intros left right.
  unfold compare_uleb_codewords, compare_uleb_codewords_structural.
  destruct (Nat.compare (length left) (length right))
    eqn:Hlength; try reflexivity.
  apply Nat.compare_eq_iff in Hlength.
  now apply compare_reverse_index_agrees_with_structural.
Qed.

Record ReverseIndexMachineState := {
  reverse_index_remaining : nat;
  reverse_index_outcome : option comparison;
}.

Definition reverse_index_machine_step
    (left right : list PhysicalByte)
    (state : ReverseIndexMachineState) : ReverseIndexMachineState :=
  match state.(reverse_index_outcome), state.(reverse_index_remaining) with
  | Some outcome, _ => state
  | None, 0 =>
      {| reverse_index_remaining := 0;
         reverse_index_outcome := Some Eq |}
  | None, S index =>
      match Nat.compare
        (uleb_payload (nth index left 0))
        (uleb_payload (nth index right 0)) with
      | Eq =>
          {| reverse_index_remaining := index;
             reverse_index_outcome := None |}
      | outcome =>
          {| reverse_index_remaining := index;
             reverse_index_outcome := Some outcome |}
      end
  end.

Theorem VWENC_96_REVERSE_INDEX_MACHINE_PENDING_STEP_STRICTLY_DESCENDS :
  forall left right remaining next,
    reverse_index_machine_step left right
      {| reverse_index_remaining := S remaining;
         reverse_index_outcome := None |} = next ->
    reverse_index_remaining next = remaining.
Proof.
  intros left right remaining next Hstep.
  unfold reverse_index_machine_step in Hstep. simpl in Hstep.
  destruct
    (Nat.compare
      (uleb_payload (nth remaining left 0))
      (uleb_payload (nth remaining right 0)));
    inversion Hstep; reflexivity.
Qed.

Lemma compare_equal_width_uleb_bytes_agrees_with_value :
  forall left right,
    length left = length right ->
    compare_equal_width_uleb_bytes left right =
    Nat.compare
      (uleb_value (decode_uleb_payloads left))
      (uleb_value (decode_uleb_payloads right)).
Proof.
  induction left as [| left_byte left_rest IH];
    destruct right as [| right_byte right_rest];
    intros Hlength; try discriminate; [reflexivity |].
  simpl in Hlength. injection Hlength as Hrest_length.
  change
    (match compare_equal_width_uleb_bytes left_rest right_rest with
     | Eq => Nat.compare
                 (uleb_payload left_byte) (uleb_payload right_byte)
     | Lt => Lt
     | Gt => Gt
     end =
     Nat.compare
       (uleb_payload left_byte +
        128 * uleb_value (decode_uleb_payloads left_rest))
       (uleb_payload right_byte +
        128 * uleb_value (decode_uleb_payloads right_rest))).
  rewrite (IH right_rest Hrest_length).
  pose proof (Nat.mod_upper_bound left_byte 128 ltac:(lia)) as Hleft_digit.
  pose proof (Nat.mod_upper_bound right_byte 128 ltac:(lia)) as Hright_digit.
  unfold uleb_payload in *.
  destruct
    (Nat.compare
       (uleb_value (decode_uleb_payloads left_rest))
       (uleb_value (decode_uleb_payloads right_rest)))
    eqn:Htail.
  - apply Nat.compare_eq_iff in Htail.
    destruct
    (Nat.compare (left_byte mod 128) (right_byte mod 128))
      eqn:Hlow.
    + apply Nat.compare_eq_iff in Hlow.
      symmetry. apply Nat.compare_eq_iff. lia.
    + apply Nat.compare_lt_iff in Hlow.
      symmetry. apply Nat.compare_lt_iff. lia.
    + apply Nat.compare_gt_iff in Hlow.
      symmetry. apply Nat.compare_gt_iff. lia.
  - apply Nat.compare_lt_iff in Htail.
    symmetry. apply Nat.compare_lt_iff. nia.
  - apply Nat.compare_gt_iff in Htail.
    symmetry. apply Nat.compare_gt_iff. nia.
Qed.

Lemma uleb_value_below_width :
  forall digits,
    Forall valid_uleb_digit digits ->
    uleb_value digits < 128 ^ length digits.
Proof.
  induction digits as [| digit rest IH]; intros Hvalid.
  - simpl. lia.
  - inversion Hvalid as [| ? ? Hdigit Hrest]; subst.
    specialize (IH Hrest).
    unfold valid_uleb_digit in Hdigit.
    simpl. nia.
Qed.

Lemma uleb_value_reaches_highest_place :
  forall digits,
    digits <> [] ->
    Forall valid_uleb_digit digits ->
    last digits 0 <> 0 ->
    128 ^ (length digits - 1) <= uleb_value digits.
Proof.
  induction digits as [| digit rest IH];
    intros Hnonempty Hvalid Hhighest; [contradiction |].
  inversion Hvalid as [| ? ? Hdigit Hrest]; subst.
  destruct rest as [| next tail].
  - simpl in *. unfold valid_uleb_digit in Hdigit. lia.
  - specialize
      (IH ltac:(discriminate) Hrest ltac:(simpl in Hhighest; exact Hhighest)).
    cbn [length] in IH.
    replace (S (length tail) - 1) with (length tail) in IH by lia.
    change (128 ^ length tail <= uleb_value (next :: tail)) in IH.
    change
      (128 * 128 ^ length tail <=
       digit + 128 * uleb_value (next :: tail)).
    nia.
Qed.

Lemma radix_128_power_monotone :
  forall lower upper,
    lower <= upper -> 128 ^ lower <= 128 ^ upper.
Proof.
  intros lower upper Hle. revert lower Hle.
  induction upper as [| upper IH]; intros lower Hle.
  - assert (lower = 0) by lia. subst. reflexivity.
  - destruct (Nat.eq_dec lower (S upper)) as [-> | Hneq];
      [reflexivity |].
    assert (lower <= upper) as Hlower by lia.
    specialize (IH lower Hlower).
    change (128 ^ lower <= 128 * 128 ^ upper).
    eapply Nat.le_trans; [exact IH |].
    set (power := 128 ^ upper).
    change (power <= 128 * power).
    lia.
Qed.

Lemma canonical_uleb_shorter_width_has_smaller_value :
  forall left right,
    canonical_uleb_digits left ->
    canonical_uleb_digits right ->
    length left < length right ->
    uleb_value left < uleb_value right.
Proof.
  intros left right
    [Hleft_nonempty [Hleft_valid Hleft_minimal]]
    [Hright_nonempty [Hright_valid Hright_minimal]]
    Hwidth.
  pose proof (uleb_value_below_width left Hleft_valid) as Hleft_upper.
  assert (1 <= length left) as Hleft_positive.
  { destruct left; [contradiction | simpl; lia]. }
  assert (2 <= length right) as Hright_multiple by lia.
  pose proof (Hright_minimal Hright_multiple) as Hright_highest.
  pose proof
    (uleb_value_reaches_highest_place
       right Hright_nonempty Hright_valid Hright_highest)
    as Hright_lower.
  pose proof
    (radix_128_power_monotone
       (length left) (length right - 1) ltac:(lia))
    as Hpowers.
  lia.
Qed.

Theorem VWENC_10_ULEB_ORDER_IS_LOGICAL_NUMERIC_ORDER :
  forall left right,
    canonical_uleb_codeword left ->
    canonical_uleb_codeword right ->
    compare_uleb_codewords left right =
    Nat.compare
      (uleb_value (decode_uleb_payloads left))
      (uleb_value (decode_uleb_payloads right)).
Proof.
  intros left right [Hleft_shape Hleft_digits]
    [Hright_shape Hright_digits].
  unfold compare_uleb_codewords.
  destruct (Nat.compare (length left) (length right)) eqn:Hwidth.
  - apply Nat.compare_eq_iff in Hwidth.
    rewrite compare_reverse_index_agrees_with_structural by exact Hwidth.
    now apply compare_equal_width_uleb_bytes_agrees_with_value.
  - apply Nat.compare_lt_iff in Hwidth.
    symmetry. apply Nat.compare_lt_iff.
    apply canonical_uleb_shorter_width_has_smaller_value.
    + exact Hleft_digits.
    + exact Hright_digits.
    + unfold decode_uleb_payloads. now rewrite !length_map.
  - apply Nat.compare_gt_iff in Hwidth.
    symmetry. apply Nat.compare_gt_iff.
    apply canonical_uleb_shorter_width_has_smaller_value.
    + exact Hright_digits.
    + exact Hleft_digits.
    + unfold decode_uleb_payloads. now rewrite !length_map.
Qed.

Lemma compare_equal_width_uleb_bytes_eq_payloads :
  forall left right,
    length left = length right ->
    compare_equal_width_uleb_bytes left right = Eq ->
    decode_uleb_payloads left = decode_uleb_payloads right.
Proof.
  induction left as [| left_byte left_rest IH];
    destruct right as [| right_byte right_rest];
    intros Hlength Hcompare; try discriminate; [reflexivity |].
  simpl in Hlength. injection Hlength as Hrest_length.
  simpl in Hcompare.
  destruct
    (compare_equal_width_uleb_bytes left_rest right_rest)
    eqn:Hrest_compare; try discriminate.
  apply Nat.compare_eq_iff in Hcompare.
  unfold decode_uleb_payloads. simpl.
  f_equal.
  - exact Hcompare.
  - apply IH; assumption.
Qed.

Theorem VWENC_57_ULEB_COMPARATOR_EQUAL_IFF_CANONICAL_BYTES_EQUAL :
  forall left right,
    canonical_uleb_codeword left ->
    canonical_uleb_codeword right ->
    (compare_uleb_codewords left right = Eq <-> left = right).
Proof.
  intros left right [Hleft_shape Hleft_digits]
    [Hright_shape Hright_digits].
  split.
  - intros Hcompare.
    unfold compare_uleb_codewords in Hcompare.
    destruct (Nat.compare (length left) (length right))
      eqn:Hlength; try discriminate.
    apply Nat.compare_eq_iff in Hlength.
    rewrite compare_reverse_index_agrees_with_structural in Hcompare
      by exact Hlength.
    apply VWENC_04_ULEB_UNIQUE_DECODING; [exact Hleft_shape | exact Hright_shape |].
    now apply compare_equal_width_uleb_bytes_eq_payloads.
  - intros ->.
    unfold compare_uleb_codewords.
    rewrite Nat.compare_refl.
    rewrite compare_reverse_index_agrees_with_structural by reflexivity.
    rewrite compare_equal_width_uleb_bytes_agrees_with_value by reflexivity.
    apply Nat.compare_refl.
Qed.

Theorem VWENC_58_ULEB_CANONICAL_SEMANTIC_VALUE_IS_INJECTIVE :
  forall left right,
    canonical_uleb_codeword left ->
    canonical_uleb_codeword right ->
    uleb_value (decode_uleb_payloads left) =
      uleb_value (decode_uleb_payloads right) ->
    left = right.
Proof.
  intros left right Hleft Hright Hvalue.
  apply (proj1
    (VWENC_57_ULEB_COMPARATOR_EQUAL_IFF_CANONICAL_BYTES_EQUAL
      left right Hleft Hright)).
  rewrite (VWENC_10_ULEB_ORDER_IS_LOGICAL_NUMERIC_ORDER
    left right Hleft Hright), Hvalue.
  apply Nat.compare_refl.
Qed.

Definition uleb_byte_identity
    (bytes : list PhysicalByte) : list PhysicalByte := bytes.

Definition uleb_hash_material
    (bytes : list PhysicalByte) : list PhysicalByte := bytes.

Definition uleb_biguint_view (bytes : list PhysicalByte) : nat :=
  uleb_value (decode_uleb_payloads bytes).

Definition decode_uleb_bounded
    (exclusive_bound : nat) (bytes : list PhysicalByte) : option nat :=
  match decode_canonical_uleb bytes with
  | None => None
  | Some digits =>
      let value := uleb_value digits in
      if value <? exclusive_bound then Some value else None
  end.

Theorem VWENC_37_ULEB_EQUALITY_IS_CANONICAL_BYTE_EQUALITY :
  forall left right,
    canonical_uleb_codeword left ->
    canonical_uleb_codeword right ->
    byte_sequence_eqb left right = true <->
    uleb_byte_identity left = uleb_byte_identity right.
Proof.
  intros left right Hleft Hright.
  unfold uleb_byte_identity.
  apply byte_sequence_eqb_reflects_equality.
Qed.

(** [uleb_hash_material] is collision-free input material, not the output of a
    finite hash function. Actual hash outputs may collide; consumers rely on
    equality checks after hash-table bucket selection. *)
Theorem VWENC_38_ULEB_HASH_MATERIAL_IS_INJECTIVE :
  forall left right,
    canonical_uleb_codeword left ->
    canonical_uleb_codeword right ->
    uleb_hash_material left = uleb_hash_material right -> left = right.
Proof. intros left right Hleft Hright Hequal. exact Hequal. Qed.

Theorem VWENC_90_FINITE_HASH_OUTPUT_REQUIRES_ONLY_EQUALITY_CONGRUENCE :
  forall (finite_hash : list PhysicalByte -> nat) left right,
    left = right -> finite_hash left = finite_hash right.
Proof. intros finite_hash left right ->. reflexivity. Qed.

Theorem VWENC_39_ULEB_BIGUINT_VIEW_AGREES_WITH_NUMERIC_ORDER :
  forall left right,
    canonical_uleb_codeword left ->
    canonical_uleb_codeword right ->
    compare_uleb_codewords left right =
    Nat.compare (uleb_biguint_view left) (uleb_biguint_view right).
Proof.
  intros left right Hleft Hright.
  unfold uleb_biguint_view.
  now apply VWENC_10_ULEB_ORDER_IS_LOGICAL_NUMERIC_ORDER.
Qed.

Theorem VWENC_40_ULEB_BOUNDED_ADAPTER_AGREES_WHEN_REPRESENTABLE :
  forall exclusive_bound bytes,
    canonical_uleb_codeword bytes ->
    uleb_biguint_view bytes < exclusive_bound ->
    decode_uleb_bounded exclusive_bound bytes =
    Some (uleb_biguint_view bytes).
Proof.
  intros exclusive_bound bytes Hcanonical Hbounded.
  unfold decode_uleb_bounded, uleb_biguint_view.
  apply VWENC_34_ULEB_DECODER_ACCEPTS_EXACTLY_CANONICAL_CODEWORDS
    in Hcanonical.
  rewrite Hcanonical.
  unfold uleb_biguint_view in Hbounded.
  apply Nat.ltb_lt in Hbounded.
  now rewrite Hbounded.
Qed.

Theorem VWENC_41_ULEB_BOUNDED_ADAPTER_REJECTS_REPRESENTATION_OVERFLOW :
  forall exclusive_bound bytes,
    canonical_uleb_codeword bytes ->
    exclusive_bound <= uleb_biguint_view bytes ->
    decode_uleb_bounded exclusive_bound bytes = None.
Proof.
  intros exclusive_bound bytes Hcanonical Hoverflow.
  unfold decode_uleb_bounded, uleb_biguint_view.
  apply VWENC_34_ULEB_DECODER_ACCEPTS_EXACTLY_CANONICAL_CODEWORDS
    in Hcanonical.
  rewrite Hcanonical.
  unfold uleb_biguint_view in Hoverflow.
  destruct
    (uleb_value (decode_uleb_payloads bytes) <? exclusive_bound)
    eqn:Hrepresentable.
  - rewrite Nat.ltb_lt in Hrepresentable. lia.
  - reflexivity.
Qed.

(** ** Unicode scalar and canonical UTF-8 profile *)

(** Factor Unicode boundaries instead of expanding large decimal Peano
    numerals.  [unicode_limit] is U+110000, [surrogate_start] is U+D800,
    and [surrogate_end] is the exclusive U+E000 boundary. *)
Definition utf8_one_byte_limit : nat := 128.
Definition utf8_two_byte_limit : nat := 8 * 256.
Definition utf8_three_byte_limit : nat := 256 ^ 2.
Definition unicode_limit : nat := 17 * utf8_three_byte_limit.
Definition surrogate_start : nat := 216 * 256.
Definition surrogate_end : nat := 224 * 256.

Definition unicode_scalar (codepoint : nat) : Prop :=
  codepoint < unicode_limit /\
  (codepoint < surrogate_start \/ surrogate_end <= codepoint).

Definition unicode_scalarb (codepoint : nat) : bool :=
  (codepoint <? unicode_limit) &&
  ((codepoint <? surrogate_start) || (surrogate_end <=? codepoint)).

Definition utf8_width (codepoint : nat) : nat :=
  if codepoint <? utf8_one_byte_limit then 1
  else if codepoint <? utf8_two_byte_limit then 2
  else if codepoint <? utf8_three_byte_limit then 3
  else 4.

Definition encode_utf8_scalar (codepoint : nat) : list PhysicalByte :=
  if codepoint <? utf8_one_byte_limit then
    [codepoint]
  else if codepoint <? utf8_two_byte_limit then
    [codepoint / 64 + 192;
     codepoint mod 64 + 128]
  else if codepoint <? utf8_three_byte_limit then
    [(codepoint / 64) / 64 + 224;
     (codepoint / 64) mod 64 + 128;
     codepoint mod 64 + 128]
  else
    [((codepoint / 64) / 64) / 64 + 240;
     ((codepoint / 64) / 64) mod 64 + 128;
     (codepoint / 64) mod 64 + 128;
     codepoint mod 64 + 128].

Definition canonical_utf8_codeword
    (codepoint : nat) (bytes : list PhysicalByte) : Prop :=
  unicode_scalar codepoint /\ bytes = encode_utf8_scalar codepoint.

Definition decode_utf8_value
    (bytes : list PhysicalByte) : option nat :=
  match bytes with
  | [first] => Some first
  | [first; second] =>
      Some ((first - 192) * 64 + (second - 128))
  | [first; second; third] =>
      Some
        (((first - 224) * 64 + (second - 128)) * 64 +
         (third - 128))
  | [first; second; third; fourth] =>
      Some
        (((((first - 240) * 64 + (second - 128)) * 64 +
           (third - 128)) * 64) +
         (fourth - 128))
  | _ => None
  end.

Definition decode_utf8_scalar
    (bytes : list PhysicalByte) : option nat :=
  match decode_utf8_value bytes with
  | None => None
  | Some codepoint =>
      if unicode_scalarb codepoint &&
         byte_sequence_eqb bytes (encode_utf8_scalar codepoint)
      then Some codepoint
      else None
  end.

Lemma radix64_reconstruct :
  forall value,
    (value / 64) * 64 + value mod 64 = value.
Proof.
  intros value.
  pose proof (Nat.div_mod value 64) as Hdivision.
  specialize (Hdivision ltac:(lia)). nia.
Qed.

Lemma decode_utf8_value_encode_roundtrip :
  forall codepoint,
    decode_utf8_value (encode_utf8_scalar codepoint) = Some codepoint.
Proof.
  intros codepoint.
  unfold encode_utf8_scalar, decode_utf8_value.
  destruct (codepoint <? utf8_one_byte_limit);
  [| destruct (codepoint <? utf8_two_byte_limit);
     [| destruct (codepoint <? utf8_three_byte_limit)]].
  - reflexivity.
  - cbn [decode_utf8_value].
    repeat rewrite Nat.add_sub.
    f_equal. apply radix64_reconstruct.
  - cbn [decode_utf8_value].
    repeat rewrite Nat.add_sub.
    f_equal.
    pose proof (radix64_reconstruct codepoint) as Hlow.
    pose proof (radix64_reconstruct (codepoint / 64)) as Hmiddle.
    nia.
  - cbn [decode_utf8_value].
    repeat rewrite Nat.add_sub.
    f_equal.
    pose proof (radix64_reconstruct codepoint) as Hlow.
    pose proof (radix64_reconstruct (codepoint / 64)) as Hmiddle.
    pose proof
      (radix64_reconstruct ((codepoint / 64) / 64)) as Hhigh.
    nia.
Qed.

Theorem VWENC_11_UTF8_SCALAR_BOOLEAN_REFLECTION :
  forall codepoint,
    unicode_scalarb codepoint = true <-> unicode_scalar codepoint.
Proof.
  intros codepoint.
  unfold unicode_scalarb, unicode_scalar.
  rewrite andb_true_iff, orb_true_iff.
  rewrite !Nat.leb_le, !Nat.ltb_lt.
  tauto.
Qed.

Theorem VWENC_12_UTF8_CODEWORDS_NONEMPTY_AND_AT_MOST_FOUR_BYTES :
  forall codepoint,
    unicode_scalar codepoint ->
    encode_utf8_scalar codepoint <> [] /\
    1 <= length (encode_utf8_scalar codepoint) <= 4.
Proof.
  intros codepoint Hscalar.
  unfold encode_utf8_scalar.
  destruct (codepoint <? utf8_one_byte_limit);
  [| destruct (codepoint <? utf8_two_byte_limit);
     [| destruct (codepoint <? utf8_three_byte_limit)]].
  all: simpl.
  all: split.
  all: try discriminate.
  all: lia.
Qed.

Theorem VWENC_13_UTF8_WIDTH_MATCHES_CANONICAL_CODEWORD :
  forall codepoint,
    unicode_scalar codepoint ->
    length (encode_utf8_scalar codepoint) = utf8_width codepoint.
Proof.
  intros codepoint Hscalar.
  unfold encode_utf8_scalar, utf8_width.
  destruct (codepoint <? utf8_one_byte_limit);
  [| destruct (codepoint <? utf8_two_byte_limit);
     [| destruct (codepoint <? utf8_three_byte_limit)]];
  reflexivity.
Qed.

Theorem VWENC_14_UTF8_REJECTS_NONSCALARS :
  forall codepoint,
    ~ unicode_scalar codepoint ->
    forall bytes, ~ canonical_utf8_codeword codepoint bytes.
Proof.
  intros codepoint Hinvalid bytes [Hscalar _]. contradiction.
Qed.

Theorem VWENC_42_UTF8_CANONICAL_DECODE_ROUNDTRIP :
  forall codepoint,
    unicode_scalar codepoint ->
    decode_utf8_scalar (encode_utf8_scalar codepoint) = Some codepoint.
Proof.
  intros codepoint Hscalar.
  unfold decode_utf8_scalar.
  rewrite decode_utf8_value_encode_roundtrip.
  assert (Hscalarb : unicode_scalarb codepoint = true).
  { apply VWENC_11_UTF8_SCALAR_BOOLEAN_REFLECTION. exact Hscalar. }
  assert
    (Hequal :
       byte_sequence_eqb
         (encode_utf8_scalar codepoint)
         (encode_utf8_scalar codepoint) = true).
  { apply byte_sequence_eqb_reflects_equality. reflexivity. }
  now rewrite Hscalarb, Hequal.
Qed.

Theorem VWENC_43_UTF8_DECODER_ACCEPTANCE_IS_CANONICAL :
  forall bytes codepoint,
    decode_utf8_scalar bytes = Some codepoint ->
    canonical_utf8_codeword codepoint bytes.
Proof.
  intros bytes codepoint Hdecode.
  unfold decode_utf8_scalar in Hdecode.
  destruct (decode_utf8_value bytes) as [candidate |] eqn:Hcandidate;
    [| discriminate].
  destruct
    (unicode_scalarb candidate &&
     byte_sequence_eqb bytes (encode_utf8_scalar candidate))
    eqn:Haccepted; [| discriminate].
  inversion Hdecode; subst candidate.
  apply andb_true_iff in Haccepted.
  destruct Haccepted as [Hscalar Hequal].
  apply VWENC_11_UTF8_SCALAR_BOOLEAN_REFLECTION in Hscalar.
  apply byte_sequence_eqb_reflects_equality in Hequal.
  split; assumption.
Qed.

Theorem VWENC_44_UTF8_DECODER_ACCEPTS_CANONICAL_CODEWORDS :
  forall bytes codepoint,
    canonical_utf8_codeword codepoint bytes ->
    decode_utf8_scalar bytes = Some codepoint.
Proof.
  intros bytes codepoint [Hscalar ->].
  apply VWENC_42_UTF8_CANONICAL_DECODE_ROUNDTRIP.
  exact Hscalar.
Qed.

Theorem VWENC_45_UTF8_CANONICAL_ENCODING_IS_INJECTIVE :
  forall left right,
    unicode_scalar left ->
    unicode_scalar right ->
    encode_utf8_scalar left = encode_utf8_scalar right ->
    left = right.
Proof.
  intros left right Hleft Hright Hencoded.
  pose proof (VWENC_42_UTF8_CANONICAL_DECODE_ROUNDTRIP left Hleft)
    as Hdecode_left.
  pose proof (VWENC_42_UTF8_CANONICAL_DECODE_ROUNDTRIP right Hright)
    as Hdecode_right.
  rewrite Hencoded in Hdecode_left.
  rewrite Hdecode_right in Hdecode_left.
  inversion Hdecode_left. reflexivity.
Qed.

Theorem VWENC_46_UTF8_MALFORMED_OR_NONCANONICAL_INPUT_IS_REJECTED :
  forall bytes,
    (forall codepoint, ~ canonical_utf8_codeword codepoint bytes) ->
    decode_utf8_scalar bytes = None.
Proof.
  intros bytes Hnoncanonical.
  destruct (decode_utf8_scalar bytes) as [codepoint |] eqn:Hdecode.
  - exfalso. apply (Hnoncanonical codepoint).
    now apply VWENC_43_UTF8_DECODER_ACCEPTANCE_IS_CANONICAL.
  - reflexivity.
Qed.

Theorem VWENC_47_UTF8_REJECTS_CONTINUATION_OVERLONG_TRUNCATED_AND_SURROGATE :
  decode_utf8_scalar [169] = None /\
  decode_utf8_scalar [192; 128] = None /\
  decode_utf8_scalar [195] = None /\
  decode_utf8_scalar [237; 160; 128] = None.
Proof. repeat split; reflexivity. Qed.

(** ** Direct fixed-unit and logical-observation laws *)

(** Keep machine-width bounds symbolic.  Expanding 64-bit decimal literals into
    Peano naturals is both semantically unnecessary and prohibitively expensive
    for the proof checker.  These factored definitions preserve the exact
    values while proofs reason about their algebraic relationships. *)
Definition two_to_32 : nat := 256 ^ 4.
Definition two_to_63 : nat := 128 * 256 ^ 7.
Definition two_to_64 : nat := 256 ^ 8.

Lemma two_to_63_positive : 0 < two_to_63.
Proof.
  unfold two_to_63.
  assert (256 ^ 7 <> 0).
  { apply Nat.pow_nonzero. lia. }
  nia.
Qed.

Lemma two_to_64_is_double_two_to_63 :
  two_to_64 = 2 * two_to_63.
Proof.
  unfold two_to_64, two_to_63.
  replace 8 with (S 7) by reflexivity.
  rewrite Nat.pow_succ_r by lia.
  set (power := 256 ^ 7).
  change (256 * power = 2 * (128 * power)).
  lia.
Qed.

(** Subsequent proofs use the checked positivity/doubling interface above.
    Keeping the factored Peano definitions opaque prevents the kernel from
    expanding machine-width bounds while closing unrelated theorems. *)
Global Opaque two_to_32 two_to_63 two_to_64.

Inductive DirectProfile :=
| DirectBytes
| DirectUnicodeScalar
| DirectU32
| DirectU64
| DirectF64Bits.

Definition direct_profile_tag (profile : DirectProfile) : nat :=
  match profile with
  | DirectBytes => 1
  | DirectUnicodeScalar => 2
  | DirectU32 => 3
  | DirectU64 => 4
  | DirectF64Bits => 5
  end.

Definition direct_byte_width (profile : DirectProfile) : nat :=
  match profile with
  | DirectBytes => 1
  | DirectUnicodeScalar => 4
  | DirectU32 => 4
  | DirectU64 => 8
  | DirectF64Bits => 8
  end.

Definition direct_profile_valid
    (profile : DirectProfile) (unit : nat) : Prop :=
  match profile with
  | DirectBytes => unit < 256
  | DirectUnicodeScalar => unicode_scalar unit
  | DirectU32 => unit < 256 ^ 4
  | DirectU64 => unit < 256 ^ 8
  | DirectF64Bits => unit < 256 ^ 8
  end.

Definition direct_profile_validb
    (profile : DirectProfile) (unit : nat) : bool :=
  match profile with
  | DirectBytes => unit <? 256
  | DirectUnicodeScalar => unicode_scalarb unit
  | DirectU32 => unit <? 256 ^ 4
  | DirectU64 => unit <? 256 ^ 8
  | DirectF64Bits => unit <? 256 ^ 8
  end.

Lemma direct_profile_validb_reflects_validity :
  forall profile unit,
    direct_profile_validb profile unit = true <->
    direct_profile_valid profile unit.
Proof.
  intros profile unit.
  destruct profile;
    cbn [direct_profile_validb direct_profile_valid].
  - apply Nat.ltb_lt.
  - apply VWENC_11_UTF8_SCALAR_BOOLEAN_REFLECTION.
  - apply Nat.ltb_lt.
  - apply Nat.ltb_lt.
  - apply Nat.ltb_lt.
Qed.

Fixpoint encode_fixed_little_endian
    (byte_count value : nat) : list PhysicalByte :=
  match byte_count with
  | 0 => []
  | S rest => value mod 256 ::
              encode_fixed_little_endian rest (value / 256)
  end.

Fixpoint decode_fixed_little_endian
    (bytes : list PhysicalByte) : nat :=
  match bytes with
  | [] => 0
  | byte :: rest => byte + decode_fixed_little_endian rest * 256
  end.

Definition serialize_direct_unit
    (profile : DirectProfile) (unit : nat)
    : nat * list PhysicalByte :=
  (direct_profile_tag profile,
   encode_fixed_little_endian (direct_byte_width profile) unit).

Fixpoint all_valid_bytesb (bytes : list PhysicalByte) : bool :=
  match bytes with
  | [] => true
  | byte :: rest => (byte <? 256) && all_valid_bytesb rest
  end.

Lemma all_valid_bytesb_reflects_validity :
  forall bytes,
    all_valid_bytesb bytes = true <-> Forall valid_byte bytes.
Proof.
  induction bytes as [| byte rest IH]; simpl.
  - split; constructor.
  - rewrite andb_true_iff, Nat.ltb_lt, IH.
    unfold valid_byte.
    split.
    + intros [Hbyte Hrest]. constructor; assumption.
    + intros Hvalid. inversion Hvalid; subst. tauto.
Qed.

(** This checked record is the prospective canonical direct-profile codec.
    It is not a claim about the byte layout of any existing serde/bincode or
    persistent-ARTrie image.  Migration of a persistent backend may select
    this record only under a new, explicit format identity. *)
Definition decode_direct_unit
    (expected_profile : DirectProfile)
    (serialized : nat * list PhysicalByte) : option nat :=
  let '(profile_tag, bytes) := serialized in
  if profile_tag =? direct_profile_tag expected_profile then
    if length bytes =? direct_byte_width expected_profile then
      if all_valid_bytesb bytes then
        let unit := decode_fixed_little_endian bytes in
        if direct_profile_validb expected_profile unit
        then Some unit
        else None
      else None
    else None
  else None.

Definition direct_codeword (unit : nat) : list nat := [unit].

Lemma fixed_little_endian_length :
  forall byte_count value,
    length (encode_fixed_little_endian byte_count value) = byte_count.
Proof.
  induction byte_count; intros value; simpl; [reflexivity |].
  now rewrite IHbyte_count.
Qed.

Lemma fixed_little_endian_bytes_are_valid :
  forall byte_count value,
    Forall valid_byte (encode_fixed_little_endian byte_count value).
Proof.
  induction byte_count as [| byte_count IH]; intros value.
  - change (Forall valid_byte []). constructor.
  - change
      (Forall valid_byte
        (value mod 256 ::
         encode_fixed_little_endian byte_count (value / 256))).
    constructor.
    + unfold valid_byte. apply Nat.mod_upper_bound. lia.
    + apply IH.
Qed.

Lemma fixed_little_endian_roundtrip :
  forall byte_count value,
    value < 256 ^ byte_count ->
    decode_fixed_little_endian
      (encode_fixed_little_endian byte_count value) = value.
Proof.
  induction byte_count as [| byte_count IH]; intros value Hbounded.
  - simpl in *. lia.
  - cbn [encode_fixed_little_endian decode_fixed_little_endian].
    rewrite IH.
    + pose proof (Nat.div_mod value 256 ltac:(lia)) as Hdivision.
      nia.
    + pose proof (Nat.div_mod value 256 ltac:(lia)) as Hdivision.
      pose proof (Nat.mod_upper_bound value 256 ltac:(lia)) as Hremainder.
      change (value < 256 * 256 ^ byte_count) in Hbounded.
      nia.
Qed.

Lemma direct_profile_value_fits_serialization :
  forall profile unit,
    direct_profile_valid profile unit ->
    unit < 256 ^ direct_byte_width profile.
Proof.
  intros profile unit Hvalid.
  destruct profile; cbn [direct_profile_valid direct_byte_width] in Hvalid |- *.
  - exact Hvalid.
  - destruct Hvalid as [Hupper _].
    unfold unicode_limit, utf8_three_byte_limit in Hupper.
    assert (Hbase : 17 < 256 ^ 2).
    { change (17 < 256 * (256 * 1)). rewrite Nat.mul_1_r. lia. }
    replace 4 with (2 + 2) by lia.
    rewrite Nat.pow_add_r.
    assert (0 < 256 ^ 2).
    { apply Nat.neq_0_lt_0. apply Nat.pow_nonzero. lia. }
    assert
      (Hunicode_fits :
         17 * 256 ^ 2 < 256 ^ 2 * 256 ^ 2) by nia.
    eapply Nat.lt_trans; [exact Hupper | exact Hunicode_fits].
  - exact Hvalid.
  - exact Hvalid.
  - exact Hvalid.
Qed.

Theorem VWENC_48_DIRECT_PROFILE_TAGS_ARE_INJECTIVE :
  forall left right,
    direct_profile_tag left = direct_profile_tag right -> left = right.
Proof.
  intros left right Hequal.
  destruct left, right; simpl in Hequal; try reflexivity; discriminate.
Qed.

Theorem VWENC_49_DIRECT_SERIALIZATION_HAS_EXACT_FIXED_WIDTH :
  forall profile unit,
    length (snd (serialize_direct_unit profile unit)) =
    direct_byte_width profile.
Proof.
  intros profile unit.
  unfold serialize_direct_unit. simpl.
  apply fixed_little_endian_length.
Qed.

Theorem VWENC_50_DIRECT_SERIALIZATION_ROUNDTRIPS_VALID_UNITS :
  forall profile unit,
    direct_profile_valid profile unit ->
    decode_fixed_little_endian
      (snd (serialize_direct_unit profile unit)) = unit.
Proof.
  intros profile unit Hvalid.
  unfold serialize_direct_unit. simpl.
  apply fixed_little_endian_roundtrip.
  now apply direct_profile_value_fits_serialization.
Qed.

Theorem VWENC_59_CHECKED_DIRECT_DECODER_ACCEPTS_CANONICAL_RECORD :
  forall profile unit,
    direct_profile_valid profile unit ->
    decode_direct_unit profile (serialize_direct_unit profile unit) =
      Some unit.
Proof.
  intros profile unit Hvalid.
  unfold decode_direct_unit, serialize_direct_unit.
  rewrite Nat.eqb_refl, fixed_little_endian_length, Nat.eqb_refl.
  pose proof
    (fixed_little_endian_bytes_are_valid
       (direct_byte_width profile) unit) as Hbytes.
  apply all_valid_bytesb_reflects_validity in Hbytes.
  rewrite Hbytes.
  rewrite fixed_little_endian_roundtrip.
  - apply direct_profile_validb_reflects_validity in Hvalid.
    now rewrite Hvalid.
  - now apply direct_profile_value_fits_serialization.
Qed.

Theorem VWENC_60_CHECKED_DIRECT_DECODER_REJECTS_WRONG_PROFILE_TAG :
  forall expected_profile supplied_tag bytes,
    supplied_tag <> direct_profile_tag expected_profile ->
    decode_direct_unit expected_profile (supplied_tag, bytes) = None.
Proof.
  intros expected_profile supplied_tag bytes Hwrong.
  unfold decode_direct_unit.
  apply Nat.eqb_neq in Hwrong. now rewrite Hwrong.
Qed.

Theorem VWENC_61_CHECKED_DIRECT_DECODER_REJECTS_WRONG_WIDTH :
  forall profile bytes,
    length bytes <> direct_byte_width profile ->
    decode_direct_unit profile (direct_profile_tag profile, bytes) = None.
Proof.
  intros profile bytes Hwrong.
  unfold decode_direct_unit. rewrite Nat.eqb_refl.
  apply Nat.eqb_neq in Hwrong. now rewrite Hwrong.
Qed.

Theorem VWENC_62_CHECKED_DIRECT_DECODER_REJECTS_NONBYTE_PAYLOAD :
  forall profile bytes,
    length bytes = direct_byte_width profile ->
    ~ Forall valid_byte bytes ->
    decode_direct_unit profile (direct_profile_tag profile, bytes) = None.
Proof.
  intros profile bytes Hwidth Hinvalid.
  unfold decode_direct_unit. rewrite Nat.eqb_refl.
  apply Nat.eqb_eq in Hwidth. rewrite Hwidth.
  destruct (all_valid_bytesb bytes) eqn:Hbytes; [| reflexivity].
  apply all_valid_bytesb_reflects_validity in Hbytes. contradiction.
Qed.

Theorem VWENC_63_CHECKED_DIRECT_DECODER_SUCCESS_IS_EXACT :
  forall profile supplied_tag bytes unit,
    decode_direct_unit profile (supplied_tag, bytes) = Some unit ->
    supplied_tag = direct_profile_tag profile /\
    length bytes = direct_byte_width profile /\
    Forall valid_byte bytes /\
    direct_profile_valid profile unit /\
    decode_fixed_little_endian bytes = unit.
Proof.
  intros profile supplied_tag bytes unit Hdecode.
  unfold decode_direct_unit in Hdecode.
  destruct (supplied_tag =? direct_profile_tag profile)
    eqn:Htag; [| discriminate].
  destruct (length bytes =? direct_byte_width profile)
    eqn:Hwidth; [| discriminate].
  destruct (all_valid_bytesb bytes) eqn:Hbytes; [| discriminate].
  destruct
    (direct_profile_validb profile (decode_fixed_little_endian bytes))
    eqn:Hvalid; [| discriminate].
  inversion Hdecode; subst.
  repeat split.
  - now apply Nat.eqb_eq.
  - now apply Nat.eqb_eq.
  - now apply all_valid_bytesb_reflects_validity.
  - now apply direct_profile_validb_reflects_validity.
Qed.

Theorem VWENC_64_CHECKED_DIRECT_DECODER_REJECTS_INVALID_LOGICAL_UNIT :
  forall profile bytes,
    length bytes = direct_byte_width profile ->
    Forall valid_byte bytes ->
    ~ direct_profile_valid profile (decode_fixed_little_endian bytes) ->
    decode_direct_unit profile (direct_profile_tag profile, bytes) = None.
Proof.
  intros profile bytes Hwidth Hbytes Hinvalid.
  unfold decode_direct_unit. rewrite Nat.eqb_refl.
  apply Nat.eqb_eq in Hwidth. rewrite Hwidth.
  apply all_valid_bytesb_reflects_validity in Hbytes. rewrite Hbytes.
  destruct
    (direct_profile_validb profile (decode_fixed_little_endian bytes))
    eqn:Hvalid; [| reflexivity].
  apply direct_profile_validb_reflects_validity in Hvalid.
  contradiction.
Qed.

Theorem VWENC_51_UNICODE_SCALAR_DIRECT_STORAGE_IS_NOT_UTF8_STORAGE :
  forall codepoint,
    unicode_scalar codepoint ->
    direct_codeword codepoint = [codepoint] /\
    decode_utf8_scalar (encode_utf8_scalar codepoint) = Some codepoint.
Proof.
  intros codepoint Hscalar. split; [reflexivity |].
  now apply VWENC_42_UTF8_CANONICAL_DECODE_ROUNDTRIP.
Qed.

Theorem VWENC_15_DIRECT_PROFILE_IS_ONE_UNIT_PER_TRANSITION :
  forall unit, length (direct_codeword unit) = 1.
Proof. reflexivity. Qed.

(** A variable-width ULEB atom retains its canonical bytes as its identity.
    No built-in integer is required at the consumer boundary. UTF-8 instead
    denotes a Unicode scalar, so its public logical identity is the decoded
    scalar value. Direct atoms are already one native edge unit. *)
Inductive LogicalAtom :=
| DirectAtom : DirectProfile -> nat -> LogicalAtom
| UlebAtom : list PhysicalByte -> LogicalAtom
| UnicodeAtom : nat -> LogicalAtom.

Definition direct_logical_atom
    (profile : DirectProfile) (unit : nat) : LogicalAtom :=
  match profile with
  | DirectUnicodeScalar => UnicodeAtom unit
  | _ => DirectAtom profile unit
  end.

(** Native direct labels match the existing [CharUnit]-generic DAWG cores.
    An opaque codeword is one edge label carrying canonical bytes. A byte-path
    adapter may use several third-party physical edges internally, but it has
    the same logical projection and must not expose its intermediate nodes to
    [DictionaryNode], zipper, or cursor consumers. *)
Inductive StorageRepresentation :=
| NativeDirectEdge
| OpaqueCodewordEdge
| EncodedBytePathAdapter.

Inductive StoredLogicalUnit :=
| StoredDirect : DirectProfile -> nat -> StoredLogicalUnit
| StoredUleb : list PhysicalByte -> StoredLogicalUnit
| StoredUtf8 : list PhysicalByte -> StoredLogicalUnit.

Definition representation_admits
    (representation : StorageRepresentation)
    (stored : StoredLogicalUnit) : Prop :=
  match representation, stored with
  | NativeDirectEdge, StoredDirect _ _ => True
  | OpaqueCodewordEdge, StoredUleb _ => True
  | OpaqueCodewordEdge, StoredUtf8 _ => True
  | EncodedBytePathAdapter, StoredUleb _ => True
  | EncodedBytePathAdapter, StoredUtf8 _ => True
  | _, _ => False
  end.

Definition representation_admitsb
    (representation : StorageRepresentation)
    (stored : StoredLogicalUnit) : bool :=
  match representation, stored with
  | NativeDirectEdge, StoredDirect _ _ => true
  | OpaqueCodewordEdge, StoredUleb _ => true
  | OpaqueCodewordEdge, StoredUtf8 _ => true
  | EncodedBytePathAdapter, StoredUleb _ => true
  | EncodedBytePathAdapter, StoredUtf8 _ => true
  | _, _ => false
  end.

Lemma representation_admitsb_reflects_admission :
  forall representation stored,
    representation_admitsb representation stored = true <->
    representation_admits representation stored.
Proof.
  intros representation stored.
  destruct representation, stored;
    cbn [representation_admitsb representation_admits]; easy.
Qed.

Definition decode_stored_logical_unit
    (stored : StoredLogicalUnit) : option LogicalAtom :=
  match stored with
  | StoredDirect profile unit =>
      if direct_profile_validb profile unit
      then Some (direct_logical_atom profile unit)
      else None
  | StoredUleb bytes =>
      match decode_canonical_uleb bytes with
      | Some _ => Some (UlebAtom bytes)
      | None => None
      end
  | StoredUtf8 bytes =>
      match decode_utf8_scalar bytes with
      | Some codepoint => Some (UnicodeAtom codepoint)
      | None => None
      end
  end.

Definition physical_codeword_of
    (stored : StoredLogicalUnit) : list PhysicalByte :=
  match stored with
  | StoredDirect profile unit => snd (serialize_direct_unit profile unit)
  | StoredUleb bytes => bytes
  | StoredUtf8 bytes => bytes
  end.

Record StoredTransition := {
  transition_representation : StorageRepresentation;
  transition_unit : StoredLogicalUnit;
}.

Definition valid_stored_transition
    (transition : StoredTransition) : Prop :=
  representation_admits
    transition.(transition_representation) transition.(transition_unit) /\
  exists atom,
    decode_stored_logical_unit transition.(transition_unit) = Some atom.

Definition logical_transition
    (transition : StoredTransition) : option LogicalAtom :=
  if representation_admitsb
       transition.(transition_representation) transition.(transition_unit)
  then decode_stored_logical_unit transition.(transition_unit)
  else None.

Inductive ConsumerSurface :=
| DictionaryNodeSurface
| ZipperSurface
| SnapshotCursorSurface.

Definition consumer_observation
    (_surface : ConsumerSurface)
    (transition : StoredTransition) : list LogicalAtom :=
  match logical_transition transition with
  | Some atom => [atom]
  | None => []
  end.

(** A concrete API surface discharges this refinement obligation in the
    family-wide proof phase. The common target below deliberately abstracts
    over how a node, zipper, or cursor obtains the transition. *)
Record ConsumerSurfaceImplementation := {
  implementation_surface : ConsumerSurface;
  implementation_observation : StoredTransition -> list LogicalAtom;
  implementation_refines_logical_target :
    forall transition,
      implementation_observation transition =
        consumer_observation implementation_surface transition;
}.

Theorem VWENC_16_CODEC_BYTES_ARE_NOT_LOGICAL_TRANSITIONS :
  forall surface transition atom,
    logical_transition transition = Some atom ->
    consumer_observation surface transition = [atom].
Proof.
  intros surface transition atom Hlogical.
  unfold consumer_observation. now rewrite Hlogical.
Qed.

Theorem VWENC_17_ONE_LOGICAL_ATOM_PER_CONSUMER_TRANSITION :
  forall surface transition,
    valid_stored_transition transition ->
    length (consumer_observation surface transition) = 1.
Proof.
  intros surface transition [Hadmitted [atom Hdecode]].
  unfold consumer_observation, logical_transition.
  apply representation_admitsb_reflects_admission in Hadmitted.
  now rewrite Hadmitted, Hdecode.
Qed.

Theorem VWENC_65_ULEB_LOGICAL_IDENTITY_IS_CANONICAL_BYTES :
  forall representation bytes,
    representation_admits representation (StoredUleb bytes) ->
    canonical_uleb_codeword bytes ->
    logical_transition
      {| transition_representation := representation;
         transition_unit := StoredUleb bytes |} =
      Some (UlebAtom bytes).
Proof.
  intros representation bytes Hadmitted Hcanonical.
  unfold logical_transition. simpl.
  apply representation_admitsb_reflects_admission in Hadmitted.
  rewrite Hadmitted.
  apply VWENC_34_ULEB_DECODER_ACCEPTS_EXACTLY_CANONICAL_CODEWORDS
    in Hcanonical.
  now rewrite Hcanonical.
Qed.

Theorem VWENC_66_UTF8_LOGICAL_IDENTITY_IS_UNICODE_SCALAR :
  forall representation bytes codepoint,
    representation_admits representation (StoredUtf8 bytes) ->
    canonical_utf8_codeword codepoint bytes ->
    logical_transition
      {| transition_representation := representation;
         transition_unit := StoredUtf8 bytes |} =
      Some (UnicodeAtom codepoint).
Proof.
  intros representation bytes codepoint Hadmitted Hcanonical.
  unfold logical_transition. simpl.
  apply representation_admitsb_reflects_admission in Hadmitted.
  rewrite Hadmitted.
  apply VWENC_44_UTF8_DECODER_ACCEPTS_CANONICAL_CODEWORDS in Hcanonical.
  now rewrite Hcanonical.
Qed.

Theorem VWENC_67_OPAQUE_AND_BYTE_PATH_ADAPTERS_HAVE_SAME_LOGICAL_VIEW :
  forall stored,
    representation_admits OpaqueCodewordEdge stored ->
    representation_admits EncodedBytePathAdapter stored ->
    logical_transition
      {| transition_representation := OpaqueCodewordEdge;
         transition_unit := stored |} =
    logical_transition
      {| transition_representation := EncodedBytePathAdapter;
         transition_unit := stored |}.
Proof.
  intros stored Hopaque Hadapter.
  apply representation_admitsb_reflects_admission in Hopaque.
  apply representation_admitsb_reflects_admission in Hadapter.
  change
    ((if representation_admitsb OpaqueCodewordEdge stored
      then decode_stored_logical_unit stored else None) =
     (if representation_admitsb EncodedBytePathAdapter stored
      then decode_stored_logical_unit stored else None)).
  now rewrite Hopaque, Hadapter.
Qed.

Theorem VWENC_68_DICTIONARY_NODE_ZIPPER_CURSOR_SHARE_COMMON_TARGET_DEFINITION :
  forall transition,
    consumer_observation DictionaryNodeSurface transition =
      consumer_observation ZipperSurface transition /\
    consumer_observation ZipperSurface transition =
      consumer_observation SnapshotCursorSurface transition.
Proof. intros transition. split; reflexivity. Qed.

Theorem VWENC_97_SURFACE_REFINEMENT_OBLIGATIONS_IMPLY_LOGICAL_AGREEMENT :
  forall left right transition,
    implementation_observation left transition =
      implementation_observation right transition.
Proof.
  intros [left_surface left_observe Hleft]
    [right_surface right_observe Hright] transition.
  simpl.
  rewrite (Hleft transition), (Hright transition).
  destruct left_surface, right_surface; reflexivity.
Qed.

Theorem VWENC_69_MULTIBYTE_STORAGE_STILL_EMITS_ONE_LOGICAL_TRANSITION :
  forall surface transition,
    valid_stored_transition transition ->
    2 <= length (physical_codeword_of transition.(transition_unit)) ->
    length (consumer_observation surface transition) = 1.
Proof.
  intros surface transition Hvalid Hmultibyte.
  now apply VWENC_17_ONE_LOGICAL_ATOM_PER_CONSUMER_TRANSITION.
Qed.

(** Exact correspondence target for the baseline generic cores at revision
    [6e8bb1d]: [CharUnit] supplies [u8], [char], and [u64] edge labels;
    [DawgCore<U,V>] and [LockFreeDawg<U,V>] both store one [U] per edge. *)
Inductive BaselineCharUnitKind :=
| BaselineU8
| BaselineChar
| BaselineU64.

Definition baseline_profile (kind : BaselineCharUnitKind) : DirectProfile :=
  match kind with
  | BaselineU8 => DirectBytes
  | BaselineChar => DirectUnicodeScalar
  | BaselineU64 => DirectU64
  end.

Inductive BaselineDawgCoreKind :=
| IndexedDawgCore
| LockFreeDawgCore.

Definition baseline_transition
    (kind : BaselineCharUnitKind) (unit : nat) : StoredTransition :=
  {| transition_representation := NativeDirectEdge;
     transition_unit := StoredDirect (baseline_profile kind) unit |}.

Definition baseline_core_observations
    (_core : BaselineDawgCoreKind)
    (kind : BaselineCharUnitKind)
    (units : list nat) : list (list LogicalAtom) :=
  map
    (fun unit =>
       consumer_observation DictionaryNodeSurface
         (baseline_transition kind unit))
    units.

Theorem VWENC_70_BASELINE_CHARUNIT_EDGE_IS_ONE_LOGICAL_ATOM :
  forall kind unit,
    direct_profile_valid (baseline_profile kind) unit ->
    logical_transition (baseline_transition kind unit) =
      Some (direct_logical_atom (baseline_profile kind) unit).
Proof.
  intros kind unit Hvalid.
  unfold logical_transition, baseline_transition. simpl.
  apply direct_profile_validb_reflects_validity in Hvalid.
  now rewrite Hvalid.
Qed.

Theorem VWENC_71_INDEXED_AND_LOCKFREE_SHARE_REQUIRED_TARGET_DEFINITION :
  forall kind units,
    baseline_core_observations IndexedDawgCore kind units =
      baseline_core_observations LockFreeDawgCore kind units.
Proof. reflexivity. Qed.

(** Existing persistent profiles are closed over the already implemented
    [ByteKey], [CharKey], and [U64Key] units. Variable-width profiles and new
    semantic interpretations require an explicit format/profile identity and
    are not silently asserted to match an existing persistent image. *)
Inductive ExistingPersistentUnitKind :=
| PersistentByteKey
| PersistentCharKey
| PersistentU64Key.

Definition persistent_baseline_kind
    (kind : ExistingPersistentUnitKind) : BaselineCharUnitKind :=
  match kind with
  | PersistentByteKey => BaselineU8
  | PersistentCharKey => BaselineChar
  | PersistentU64Key => BaselineU64
  end.

Theorem VWENC_72_EXISTING_PERSISTENT_UNITS_MAP_TO_BASELINE_CHARUNITS :
  forall kind unit,
    direct_profile_valid
      (baseline_profile (persistent_baseline_kind kind)) unit ->
    logical_transition
      (baseline_transition (persistent_baseline_kind kind) unit) =
    Some
      (direct_logical_atom
        (baseline_profile (persistent_baseline_kind kind)) unit).
Proof.
  intros kind unit Hvalid.
  now apply VWENC_70_BASELINE_CHARUNIT_EDGE_IS_ONE_LOGICAL_ATOM.
Qed.

Theorem VWENC_83_DYNAMIC_DAWG_CHAR_AND_UTF8_ADAPTER_OBSERVE_SAME_SCALAR :
  forall representation codepoint bytes,
    representation_admits representation (StoredUtf8 bytes) ->
    canonical_utf8_codeword codepoint bytes ->
    logical_transition (baseline_transition BaselineChar codepoint) =
    logical_transition
      {| transition_representation := representation;
         transition_unit := StoredUtf8 bytes |}.
Proof.
  intros representation codepoint bytes Hadmitted Hcanonical.
  assert (unicode_scalar codepoint) as Hscalar.
  { now destruct Hcanonical. }
  rewrite
    (VWENC_70_BASELINE_CHARUNIT_EDGE_IS_ONE_LOGICAL_ATOM
      BaselineChar codepoint Hscalar).
  rewrite
    (VWENC_66_UTF8_LOGICAL_IDENTITY_IS_UNICODE_SCALAR
      representation bytes codepoint Hadmitted Hcanonical).
  reflexivity.
Qed.

(** Correspondence with the existing Rocq graph models: [DawgTerm] is the
    byte-label language of [DynamicDawgMutationSpec], while [U64Sequence] is
    the native-label language of [DynamicDawgU64Spec]. These projections bind
    the new logical-unit laws to the established mutation/zipper corpus rather
    than defining a disconnected graph model. *)
Definition existing_byte_term_observations
    (term : DawgTerm) : list (list LogicalAtom) :=
  map
    (fun label =>
       consumer_observation DictionaryNodeSurface
         (baseline_transition BaselineU8 (MapSpec.byte_to_nat label)))
    term.

Theorem VWENC_91_EXISTING_DYNAMIC_DAWG_BYTE_LABEL_IS_DIRECT_BYTE_ATOM :
  forall label : DawgLabel,
    logical_transition
      (baseline_transition BaselineU8 (MapSpec.byte_to_nat label)) =
    Some (DirectAtom DirectBytes (MapSpec.byte_to_nat label)).
Proof.
  intros [label Hbyte].
  apply VWENC_70_BASELINE_CHARUNIT_EDGE_IS_ONE_LOGICAL_ATOM.
  exact Hbyte.
Qed.

Theorem VWENC_92_EXISTING_DYNAMIC_DAWG_TERM_PRESERVES_EDGE_COUNT :
  forall term : DawgTerm,
    length (existing_byte_term_observations term) = length term.
Proof. intros term. apply length_map. Qed.

Definition existing_u64_sequence_observations
    (sequence : U64Sequence) : list (list LogicalAtom) :=
  map
    (fun label =>
       consumer_observation DictionaryNodeSurface
         (baseline_transition BaselineU64 label))
    sequence.

Theorem VWENC_93_EXISTING_U64_SEQUENCE_LABELS_ARE_DIRECT_U64_ATOMS :
  forall sequence : U64Sequence,
    Forall (fun label => direct_profile_valid DirectU64 label) sequence ->
    existing_u64_sequence_observations sequence =
      map (fun label => [DirectAtom DirectU64 label]) sequence.
Proof.
  induction sequence as [| label rest IH]; intros Hvalid; [reflexivity |].
  inversion Hvalid as [| ? ? Hlabel Hrest]; subst.
  assert
    (logical_transition (baseline_transition BaselineU64 label) =
       Some (DirectAtom DirectU64 label)) as Hlogical.
  { exact
      (VWENC_70_BASELINE_CHARUNIT_EDGE_IS_ONE_LOGICAL_ATOM
        BaselineU64 label Hlabel). }
  pose proof
    (VWENC_16_CODEC_BYTES_ARE_NOT_LOGICAL_TRANSITIONS
      DictionaryNodeSurface (baseline_transition BaselineU64 label)
      (DirectAtom DirectU64 label) Hlogical) as Hobservation.
  change
    (consumer_observation DictionaryNodeSurface
       (baseline_transition BaselineU64 label) ::
       existing_u64_sequence_observations rest =
     [DirectAtom DirectU64 label] ::
       map (fun unit => [DirectAtom DirectU64 unit]) rest).
  rewrite Hobservation, (IH Hrest). reflexivity.
Qed.

Theorem VWENC_94_EXISTING_U64_SEQUENCE_PRESERVES_EDGE_COUNT :
  forall sequence : U64Sequence,
    length (existing_u64_sequence_observations sequence) = length sequence.
Proof. intros sequence. apply length_map. Qed.

(** Open in-memory unit law carrier. This record does not enumerate the unit
    type and therefore preserves downstream implementation of [CharUnit].
    A consumer supplies ordinary equality, ordering, and hash-input laws; the
    generic core then stores exactly one [U] per edge. Persistent identities
    remain closed and separately certified below. *)
Record OpenUnitProfile (U : Type) := {
  open_unit_eqb : U -> U -> bool;
  open_unit_compare : U -> U -> comparison;
  open_unit_hash_material : U -> list nat;
  open_unit_eqb_exact :
    forall left right, open_unit_eqb left right = true <-> left = right;
  open_unit_compare_equal_exact :
    forall left right, open_unit_compare left right = Eq <-> left = right;
  open_unit_compare_dual :
    forall left right,
      (open_unit_compare left right = Lt <->
         open_unit_compare right left = Gt) /\
      (open_unit_compare left right = Gt <->
         open_unit_compare right left = Lt);
  open_unit_compare_lt_transitive :
    forall left middle right,
      open_unit_compare left middle = Lt ->
      open_unit_compare middle right = Lt ->
      open_unit_compare left right = Lt;
  open_unit_hash_congruent :
    forall left right,
      left = right ->
      open_unit_hash_material left = open_unit_hash_material right;
}.

Inductive OpenConsumerSurface :=
| OpenDictionaryNodeSurface
| OpenZipperSurface
| OpenSnapshotCursorSurface.

Definition open_consumer_observation {U : Type}
    (_profile : OpenUnitProfile U)
    (_surface : OpenConsumerSurface)
    (unit : U) : list U := [unit].

Theorem VWENC_84_OPEN_CHARUNIT_PROFILE_REMAINS_ONE_UNIT_PER_EDGE :
  forall (U : Type) (profile : OpenUnitProfile U) surface unit,
    open_consumer_observation profile surface unit = [unit] /\
    length (open_consumer_observation profile surface unit) = 1.
Proof. intros. split; reflexivity. Qed.

Theorem VWENC_100_OPEN_UNIT_COMPARATOR_IS_TOTAL_ON_DISTINCT_UNITS :
  forall (U : Type) (profile : OpenUnitProfile U) left right,
    left <> right ->
    open_unit_compare U profile left right = Lt \/
    open_unit_compare U profile left right = Gt.
Proof.
  intros U profile left right Hdistinct.
  destruct (open_unit_compare U profile left right) eqn:Hcompare.
  - exfalso. apply Hdistinct.
    now apply
      (proj1 (open_unit_compare_equal_exact U profile left right)).
  - now left.
  - now right.
Qed.

Theorem VWENC_85_OPEN_SURFACES_SHARE_REQUIRED_TARGET_DEFINITION :
  forall (U : Type) (profile : OpenUnitProfile U) unit,
    open_consumer_observation profile OpenDictionaryNodeSurface unit =
      open_consumer_observation profile OpenZipperSurface unit /\
    open_consumer_observation profile OpenZipperSurface unit =
      open_consumer_observation profile OpenSnapshotCursorSurface unit.
Proof. intros. split; reflexivity. Qed.

(** Closed identities for persistence. Existing layouts and prospective codecs
    are distinct constructors; equality can never silently reinterpret an old
    image as a new UTF-8, ULEB, or semantic-F64 profile. *)
Inductive PersistentLogicalProfile :=
| PersistedByte
| PersistedUnicodeScalar
| PersistedU64
| PersistedF64Bits
| PersistedCanonicalUleb
| PersistedCanonicalUtf8.

Inductive PersistentCodecIdentity :=
| ExistingByteCodec
| ExistingCharU32Codec
| ExistingU64Codec
| ProspectiveF64BitsCodecV1
| ProspectiveCanonicalUlebCodecV1
| ProspectiveCanonicalUtf8CodecV1.

Inductive PersistentLayoutIdentity :=
| ExistingByteLayout
| ExistingCharLayout
| ExistingU64Layout
| ProspectiveLogicalUnitLayoutV1.

Record PersistentProfileDescriptor := {
  persistent_logical_profile : PersistentLogicalProfile;
  persistent_codec_identity : PersistentCodecIdentity;
  persistent_layout_identity : PersistentLayoutIdentity;
  persistent_abi_version : nat;
}.

Definition certified_persistent_profile
    (descriptor : PersistentProfileDescriptor) : Prop :=
  match descriptor.(persistent_logical_profile),
        descriptor.(persistent_codec_identity),
        descriptor.(persistent_layout_identity) with
  | PersistedByte, ExistingByteCodec, ExistingByteLayout =>
      0 < descriptor.(persistent_abi_version)
  | PersistedUnicodeScalar, ExistingCharU32Codec, ExistingCharLayout =>
      0 < descriptor.(persistent_abi_version)
  | PersistedU64, ExistingU64Codec, ExistingU64Layout =>
      0 < descriptor.(persistent_abi_version)
  | PersistedF64Bits, ProspectiveF64BitsCodecV1,
      ProspectiveLogicalUnitLayoutV1 =>
      0 < descriptor.(persistent_abi_version)
  | PersistedCanonicalUleb, ProspectiveCanonicalUlebCodecV1,
      ProspectiveLogicalUnitLayoutV1 =>
      0 < descriptor.(persistent_abi_version)
  | PersistedCanonicalUtf8, ProspectiveCanonicalUtf8CodecV1,
      ProspectiveLogicalUnitLayoutV1 =>
      0 < descriptor.(persistent_abi_version)
  | _, _, _ => False
  end.

Definition certified_profile_identity
    (profile : PersistentProfileDescriptor)
    : PersistentLogicalProfile *
      (PersistentCodecIdentity * (PersistentLayoutIdentity * nat)) :=
  (profile.(persistent_logical_profile),
   (profile.(persistent_codec_identity),
    (profile.(persistent_layout_identity), profile.(persistent_abi_version)))).

Theorem VWENC_86_CERTIFIED_PERSISTENT_PROFILE_IDENTITY_IS_INJECTIVE :
  forall left right,
    certified_profile_identity left = certified_profile_identity right ->
    left = right.
Proof.
  intros [left_profile left_codec left_layout left_abi]
    [right_profile right_codec right_layout right_abi] Hequal.
  unfold certified_profile_identity in Hequal. simpl in Hequal.
  now inversion Hequal.
Qed.

Definition profile_bound_payload
    (profile : PersistentProfileDescriptor)
    (payload : list PhysicalByte) :=
  (certified_profile_identity profile, payload).

Theorem VWENC_87_PROFILE_AND_PAYLOAD_IDENTITY_IS_JOINTLY_INJECTIVE :
  forall left_profile left_payload right_profile right_payload,
    profile_bound_payload left_profile left_payload =
      profile_bound_payload right_profile right_payload ->
    left_profile = right_profile /\ left_payload = right_payload.
Proof.
  intros
    [left_profile left_codec left_layout left_abi] left_payload
    [right_profile right_codec right_layout right_abi] right_payload Hequal.
  unfold profile_bound_payload, certified_profile_identity in Hequal.
  simpl in Hequal. inversion Hequal. split; reflexivity.
Qed.

Theorem VWENC_98_CERTIFICATION_REJECTS_INCOHERENT_PROFILE_CODEC_LAYOUT :
  ~ certified_persistent_profile
      {| persistent_logical_profile := PersistedCanonicalUleb;
         persistent_codec_identity := ExistingCharU32Codec;
         persistent_layout_identity := ExistingByteLayout;
         persistent_abi_version := 1 |}.
Proof. simpl. tauto. Qed.

Theorem VWENC_99_CERTIFICATION_ACCEPTS_VERSIONED_CANONICAL_ULEB_PROFILE :
  certified_persistent_profile
    {| persistent_logical_profile := PersistedCanonicalUleb;
       persistent_codec_identity := ProspectiveCanonicalUlebCodecV1;
       persistent_layout_identity := ProspectiveLogicalUnitLayoutV1;
       persistent_abi_version := 1 |}.
Proof.
  unfold certified_persistent_profile. simpl.
  exact (Nat.lt_0_succ 0).
Qed.

(** ** F64Bits raw identity and total_cmp-compatible ordering *)

Definition valid_f64_bits (bits : nat) : Prop := bits < two_to_64.

(** Positive encodings occupy the upper half in increasing bit order. Signed
    encodings occupy the lower half in reversed bit order. This is the
    sortable-key form of Rust's [f64::total_cmp] transformation. *)
Definition split_rank (half whole bits : nat) : nat :=
  if bits <? half
  then half + bits
  else (whole - 1) - bits.

Definition f64_total_rank (bits : nat) : nat :=
  split_rank two_to_63 two_to_64 bits.

Definition compare_f64_bits (left right : nat) : comparison :=
  Nat.compare (f64_total_rank left) (f64_total_rank right).

Definition f64_bits_identity (bits : nat) : nat := bits.

Theorem VWENC_18_F64BITS_RAW_IDENTITY_IS_INJECTIVE :
  forall (left right : nat),
    f64_bits_identity left = f64_bits_identity right -> left = right.
Proof. intros left right Hequal. exact Hequal. Qed.

Theorem VWENC_19_F64BITS_SIGNED_ZEROES_ARE_DISTINCT :
  0 <> two_to_63 /\
  f64_total_rank two_to_63 < f64_total_rank 0.
Proof.
  pose proof two_to_63_positive as Hhalf_positive.
  pose proof two_to_64_is_double_two_to_63 as Hdouble.
  split; [lia |].
  unfold f64_total_rank, split_rank.
  assert ((two_to_63 <? two_to_63) = false) as Hhalf.
  { apply Nat.ltb_ge. lia. }
  assert ((0 <? two_to_63) = true) as Hzero.
  { apply Nat.ltb_lt. exact Hhalf_positive. }
  rewrite Hhalf, Hzero. lia.
Qed.

Theorem VWENC_20_F64BITS_TOTAL_ORDER_IS_RANK_ORDER :
  forall left right,
    compare_f64_bits left right =
    Nat.compare (f64_total_rank left) (f64_total_rank right).
Proof. reflexivity. Qed.

Lemma split_rank_injective :
  forall half whole left right,
    0 < half ->
    whole = 2 * half ->
    left < whole ->
    right < whole ->
    split_rank half whole left = split_rank half whole right ->
    left = right.
Proof.
  intros half whole left right Hhalf Hwhole Hleft Hright Hrank.
  unfold split_rank in Hrank.
  destruct (left <? half) eqn:Hleftsign;
  destruct (right <? half) eqn:Hrightsign;
  rewrite ?Nat.ltb_lt, ?Nat.ltb_ge in *; lia.
Qed.

Theorem VWENC_21_F64BITS_TOTAL_RANK_INJECTIVE :
  forall left right,
    valid_f64_bits left ->
    valid_f64_bits right ->
    f64_total_rank left = f64_total_rank right ->
    left = right.
Proof.
  intros left right Hleft Hright Hrank.
  unfold valid_f64_bits in Hleft, Hright.
  unfold f64_total_rank in Hrank.
  eapply split_rank_injective.
  - exact two_to_63_positive.
  - exact two_to_64_is_double_two_to_63.
  - exact Hleft.
  - exact Hright.
  - exact Hrank.
Qed.

Definition f64_hash_material (bits : nat) : nat := bits.

Theorem VWENC_52_F64BITS_ALL_DISTINCT_PATTERNS_REMAIN_DISTINCT :
  forall left right,
    valid_f64_bits left ->
    valid_f64_bits right ->
    left <> right ->
    f64_bits_identity left <> f64_bits_identity right /\
    f64_hash_material left <> f64_hash_material right /\
    compare_f64_bits left right <> Eq.
Proof.
  intros left right Hleft Hright Hdistinct.
  repeat split; try exact Hdistinct.
  intros Hequal.
  unfold compare_f64_bits in Hequal.
  apply Nat.compare_eq_iff in Hequal.
  apply Hdistinct.
  now apply VWENC_21_F64BITS_TOTAL_RANK_INJECTIVE.
Qed.

Theorem VWENC_73_F64BITS_COMPARATOR_EQUAL_IFF_RAW_BITS_EQUAL :
  forall left right,
    valid_f64_bits left ->
    valid_f64_bits right ->
    (compare_f64_bits left right = Eq <-> left = right).
Proof.
  intros left right Hleft Hright. split.
  - unfold compare_f64_bits. rewrite Nat.compare_eq_iff.
    now apply VWENC_21_F64BITS_TOTAL_RANK_INJECTIVE.
  - intros ->. unfold compare_f64_bits. apply Nat.compare_refl.
Qed.

Theorem VWENC_74_F64BITS_COMPARATOR_IS_TOTAL :
  forall left right,
    compare_f64_bits left right = Lt \/
    compare_f64_bits left right = Eq \/
    compare_f64_bits left right = Gt.
Proof.
  intros left right.
  destruct (compare_f64_bits left right); tauto.
Qed.

Theorem VWENC_75_F64BITS_COMPARATOR_IS_ANTISYMMETRIC :
  forall left right,
    (compare_f64_bits left right = Lt <->
       compare_f64_bits right left = Gt) /\
    (compare_f64_bits left right = Gt <->
       compare_f64_bits right left = Lt).
Proof.
  intros left right. unfold compare_f64_bits.
  repeat split; intro Hcompare.
  - apply Nat.compare_lt_iff in Hcompare.
    apply Nat.compare_gt_iff. exact Hcompare.
  - apply Nat.compare_gt_iff in Hcompare.
    apply Nat.compare_lt_iff. exact Hcompare.
  - apply Nat.compare_gt_iff in Hcompare.
    apply Nat.compare_lt_iff. exact Hcompare.
  - apply Nat.compare_lt_iff in Hcompare.
    apply Nat.compare_gt_iff. exact Hcompare.
Qed.

Theorem VWENC_76_F64BITS_COMPARATOR_LT_IS_TRANSITIVE :
  forall left middle right,
    compare_f64_bits left middle = Lt ->
    compare_f64_bits middle right = Lt ->
    compare_f64_bits left right = Lt.
Proof.
  intros left middle right Hleft Hright.
  unfold compare_f64_bits in *.
  apply Nat.compare_lt_iff in Hleft.
  apply Nat.compare_lt_iff in Hright.
  apply Nat.compare_lt_iff. lia.
Qed.

(** Equivalent rank obtained by interpreting the high bit as the IEEE sign,
    reversing the lower-half order for negative encodings, and then shifting
    the signed key into naturals. This is the arithmetic form of Rust's
    [f64::total_cmp] signed-key transform. *)
Definition rust_total_cmp_shifted_rank
    (half bits : nat) : nat :=
  if bits <? half
  then half + bits
  else (half - 1) - (bits - half).

Theorem VWENC_77_F64BITS_RANK_MATCHES_RUST_SIGNED_KEY_TRANSFORM :
  forall bits,
    valid_f64_bits bits ->
    f64_total_rank bits = rust_total_cmp_shifted_rank two_to_63 bits.
Proof.
  intros bits Hvalid.
  unfold valid_f64_bits in Hvalid.
  unfold f64_total_rank, split_rank, rust_total_cmp_shifted_rank.
  destruct (bits <? two_to_63) eqn:Hsign; [reflexivity |].
  apply Nat.ltb_ge in Hsign.
  pose proof two_to_63_positive.
  pose proof two_to_64_is_double_two_to_63.
  lia.
Qed.

(** Negative control: a numeric-equality representation that aliases signed
    zero violates the raw-bit identity law. *)
Definition f64_zero_alias_mutant (bits : nat) : nat :=
  if bits =? two_to_63 then 0 else bits.

Definition preserves_distinct_valid_f64_bits
    (identity : nat -> nat) : Prop :=
  forall left right,
    valid_f64_bits left ->
    valid_f64_bits right ->
    left <> right ->
    identity left <> identity right.

Lemma f64_zero_alias_mutant_aliases_signed_zero :
  0 <> two_to_63 /\
  f64_zero_alias_mutant 0 = f64_zero_alias_mutant two_to_63.
Proof.
  pose proof two_to_63_positive as Hpositive.
  split; [lia |].
  unfold f64_zero_alias_mutant.
  rewrite Nat.eqb_refl.
  assert ((0 =? two_to_63) = false) as Hdistinct.
  { apply Nat.eqb_neq. lia. }
  now rewrite Hdistinct.
Qed.

Theorem VWENC_78_NEGATIVE_CONTROL_NUMERIC_F64_IDENTITY_VIOLATES_RAW_BITS :
  ~ preserves_distinct_valid_f64_bits f64_zero_alias_mutant.
Proof.
  intros Hpreserves.
  destruct f64_zero_alias_mutant_aliases_signed_zero
    as [Hdistinct Halias].
  assert (valid_f64_bits 0) as Hzero.
  { unfold valid_f64_bits.
    pose proof two_to_63_positive.
    pose proof two_to_64_is_double_two_to_63. lia. }
  assert (valid_f64_bits two_to_63) as Hnegative_zero.
  { unfold valid_f64_bits.
    pose proof two_to_63_positive.
    pose proof two_to_64_is_double_two_to_63. lia. }
  specialize
    (Hpreserves 0 two_to_63 Hzero Hnegative_zero Hdistinct).
  exact (Hpreserves Halias).
Qed.

(** Negative control: comparing only the first encoded ULEB byte reverses the
    adjacent numeric values 255 and 256. *)
Definition compare_first_uleb_byte
    (left right : list PhysicalByte) : comparison :=
  match left, right with
  | left_byte :: _, right_byte :: _ => Nat.compare left_byte right_byte
  | [], [] => Eq
  | [], _ => Lt
  | _, [] => Gt
  end.

Theorem VWENC_79_NEGATIVE_CONTROL_ENCODED_BYTE_ORDER_REVERSES_255_AND_256 :
  canonical_uleb_codeword [255; 1] /\
  canonical_uleb_codeword [128; 2] /\
  compare_uleb_codewords [255; 1] [128; 2] = Lt /\
  compare_first_uleb_byte [255; 1] [128; 2] = Gt.
Proof.
  assert (canonical_uleb_codeword [255; 1]) as H255.
  { split.
    - apply UlebShapeMore; [lia | lia |].
      apply UlebShapeLast. lia.
    - unfold canonical_uleb_digits, decode_uleb_payloads,
        uleb_payload, valid_uleb_digit.
      simpl. repeat split.
      + discriminate.
      + constructor; [lia | constructor; [lia | constructor]].
      + lia. }
  assert (canonical_uleb_codeword [128; 2]) as H256.
  { split.
    - apply UlebShapeMore; [lia | lia |].
      apply UlebShapeLast. lia.
    - unfold canonical_uleb_digits, decode_uleb_payloads,
        uleb_payload, valid_uleb_digit.
      simpl. repeat split.
      + discriminate.
      + constructor; [lia | constructor; [lia | constructor]].
      + lia. }
  split; [exact H255 |].
  split; [exact H256 |].
  split; reflexivity.
Qed.

Definition direct_identity
    (profile : DirectProfile) (unit : nat) : nat * nat :=
  (direct_profile_tag profile, unit).

Definition direct_hash_material
    (profile : DirectProfile) (unit : nat) : nat * nat :=
  direct_identity profile unit.

Definition direct_order_key
    (profile : DirectProfile) (unit : nat) : nat :=
  match profile with
  | DirectF64Bits => f64_total_rank unit
  | _ => unit
  end.

Definition compare_direct_units
    (profile : DirectProfile) (left right : nat) : comparison :=
  Nat.compare (direct_order_key profile left) (direct_order_key profile right).

Theorem VWENC_53_DIRECT_IDENTITY_AND_HASH_ARE_PROFILE_SCOPED_AND_INJECTIVE :
  forall left_profile left_unit right_profile right_unit,
    direct_hash_material left_profile left_unit =
      direct_hash_material right_profile right_unit ->
    left_profile = right_profile /\ left_unit = right_unit.
Proof.
  intros left_profile left_unit right_profile right_unit Hequal.
  unfold direct_hash_material, direct_identity in Hequal.
  inversion Hequal as [[Htag Hunit]].
  split.
  - now apply VWENC_48_DIRECT_PROFILE_TAGS_ARE_INJECTIVE.
  - reflexivity.
Qed.

Theorem VWENC_54_UNSIGNED_DIRECT_ORDER_IS_LOGICAL_VALUE_ORDER :
  forall profile left right,
    profile <> DirectF64Bits ->
    compare_direct_units profile left right = Nat.compare left right.
Proof.
  intros profile left right Hnotf64.
  destruct profile; [reflexivity | reflexivity | reflexivity | reflexivity |].
  contradiction.
Qed.

Theorem VWENC_55_F64BITS_DIRECT_ORDER_IS_TOTAL_CMP_ORDER :
  forall left right,
    compare_direct_units DirectF64Bits left right =
    compare_f64_bits left right.
Proof. reflexivity. Qed.

Theorem VWENC_56_DIRECT_PROFILE_WIDTHS_ARE_EXPLICIT :
  direct_byte_width DirectBytes = 1 /\
  direct_byte_width DirectUnicodeScalar = 4 /\
  direct_byte_width DirectU32 = 4 /\
  direct_byte_width DirectU64 = 8 /\
  direct_byte_width DirectF64Bits = 8.
Proof. repeat split; reflexivity. Qed.

End VariableWidthCodecSpec.
