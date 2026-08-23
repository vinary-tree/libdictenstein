(** * AbiStatusMappingSpec: status-code tables of the producer ABI

    Family FV obligation #13 (wave W2). libdictenstein speaks TWO status
    vocabularies across its ABI surfaces:

    - the project ABI (`ldict_*`, src/ffi.rs `LdictStatus`, 11 values), and
    - the interop resource callbacks (`vinary-tree-interop` `VtStatus`,
      9 values) emitted by its vt.dictionary.v1 vtables.

    This spec pins both tables and their divergence:

    - [LDICT-STAT-1] each table's encoder is injective and its decoder is a
      two-sided inverse on the declared u32 range (no collisions, no gaps,
      nothing decodable outside the range);
    - [LDICT-STAT-2] the two tables deliberately DIVERGE above code 7:
      LdictStatus carries Closed=8, DomainMismatch=9, LimitExceeded=10 while
      interop VtStatus carries Closed=6, LimitExceeded=7, ProviderError=8 —
      per-project enums are not interchangeable, and every cross-surface
      translation must go through named constructors, never through raw
      integer reuse.

    The per-function RETURNABLE status sets are executable knowledge, pinned
    by tests/ffi_status_matrix.rs; the untrusted-discriminant hardening at
    consumer boundaries is llev ledger LLEV-B6 (wave W3).
*)

From Coq Require Import Lists.List.
From Coq Require Import Arith.Arith.
From Coq Require Import micromega.Lia.
Import ListNotations.

(** ** The project-ABI table (src/ffi.rs / include/libdictenstein.h) *)

Inductive LdictStatus : Type :=
  | LdictOk
  | LdictEnd
  | LdictInvalidArgument
  | LdictInvalidUtf8
  | LdictNullPointer
  | LdictPanic
  | LdictUnsupported
  | LdictIoError
  | LdictClosed
  | LdictDomainMismatch
  | LdictLimitExceeded.

Definition ldict_encode (status : LdictStatus) : nat :=
  match status with
  | LdictOk => 0
  | LdictEnd => 1
  | LdictInvalidArgument => 2
  | LdictInvalidUtf8 => 3
  | LdictNullPointer => 4
  | LdictPanic => 5
  | LdictUnsupported => 6
  | LdictIoError => 7
  | LdictClosed => 8
  | LdictDomainMismatch => 9
  | LdictLimitExceeded => 10
  end.

Definition ldict_decode (code : nat) : option LdictStatus :=
  match code with
  | 0 => Some LdictOk
  | 1 => Some LdictEnd
  | 2 => Some LdictInvalidArgument
  | 3 => Some LdictInvalidUtf8
  | 4 => Some LdictNullPointer
  | 5 => Some LdictPanic
  | 6 => Some LdictUnsupported
  | 7 => Some LdictIoError
  | 8 => Some LdictClosed
  | 9 => Some LdictDomainMismatch
  | 10 => Some LdictLimitExceeded
  | _ => None
  end.

(** ** The interop callback table (vinary-tree-interop VtStatus) *)

Inductive VtAbiStatus : Type :=
  | VtOk
  | VtEnd
  | VtInvalidArgument
  | VtNullPointer
  | VtUnsupported
  | VtIoError
  | VtClosed
  | VtLimitExceeded
  | VtProviderError.

Definition vt_encode (status : VtAbiStatus) : nat :=
  match status with
  | VtOk => 0
  | VtEnd => 1
  | VtInvalidArgument => 2
  | VtNullPointer => 3
  | VtUnsupported => 4
  | VtIoError => 5
  | VtClosed => 6
  | VtLimitExceeded => 7
  | VtProviderError => 8
  end.

Definition vt_decode (code : nat) : option VtAbiStatus :=
  match code with
  | 0 => Some VtOk
  | 1 => Some VtEnd
  | 2 => Some VtInvalidArgument
  | 3 => Some VtNullPointer
  | 4 => Some VtUnsupported
  | 5 => Some VtIoError
  | 6 => Some VtClosed
  | 7 => Some VtLimitExceeded
  | 8 => Some VtProviderError
  | _ => None
  end.

(** ** LDICT-STAT-1: both tables are exact bijections onto their ranges *)

Theorem ldict_decode_encode :
  forall status, ldict_decode (ldict_encode status) = Some status.
Proof.
  destruct status; reflexivity.
Qed.

Theorem ldict_encode_decode :
  forall code status,
    ldict_decode code = Some status -> ldict_encode status = code.
Proof.
  intros code status Hdecode.
  do 11 (destruct code as [| code]; [injection Hdecode as <-; reflexivity |]).
  discriminate.
Qed.

Theorem ldict_encode_injective :
  forall left right,
    ldict_encode left = ldict_encode right -> left = right.
Proof.
  intros left right Heq.
  pose proof (ldict_decode_encode left) as Hleft.
  pose proof (ldict_decode_encode right) as Hright.
  rewrite Heq in Hleft. rewrite Hleft in Hright.
  injection Hright as <-. reflexivity.
Qed.

Theorem ldict_range_complete :
  forall code, code <= 10 <-> exists status, ldict_decode code = Some status.
Proof.
  intros code. split.
  - intros Hle.
    do 11 (destruct code as [| code]; [eexists; reflexivity |]).
    lia.
  - intros [status Hdecode].
    do 11 (destruct code as [| code]; [lia |]).
    discriminate.
Qed.

Theorem vt_decode_encode :
  forall status, vt_decode (vt_encode status) = Some status.
Proof.
  destruct status; reflexivity.
Qed.

Theorem vt_encode_decode :
  forall code status,
    vt_decode code = Some status -> vt_encode status = code.
Proof.
  intros code status Hdecode.
  do 9 (destruct code as [| code]; [injection Hdecode as <-; reflexivity |]).
  discriminate.
Qed.

Theorem vt_encode_injective :
  forall left right,
    vt_encode left = vt_encode right -> left = right.
Proof.
  intros left right Heq.
  pose proof (vt_decode_encode left) as Hleft.
  pose proof (vt_decode_encode right) as Hright.
  rewrite Heq in Hleft. rewrite Hleft in Hright.
  injection Hright as <-. reflexivity.
Qed.

Theorem vt_range_complete :
  forall code, code <= 8 <-> exists status, vt_decode code = Some status.
Proof.
  intros code. split.
  - intros Hle.
    do 9 (destruct code as [| code]; [eexists; reflexivity |]).
    lia.
  - intros [status Hdecode].
    do 9 (destruct code as [| code]; [lia |]).
    discriminate.
Qed.

(** ** LDICT-STAT-2: the tables diverge above code 7 by design *)

(** The shared prefix agrees except at 3/4 (InvalidUtf8 is project-local, so
    NullPointer shifts): stated positively for the codes that DO coincide. *)
Theorem shared_prefix_agreement :
  ldict_encode LdictOk = vt_encode VtOk /\
  ldict_encode LdictEnd = vt_encode VtEnd /\
  ldict_encode LdictInvalidArgument = vt_encode VtInvalidArgument /\
  ldict_encode LdictUnsupported <> vt_encode VtUnsupported /\
  ldict_encode LdictNullPointer <> vt_encode VtNullPointer.
Proof.
  repeat split; discriminate.
Qed.

(** Same NAME, different CODE: reusing a raw integer across the two
    vocabularies misroutes real statuses — the theorem the "never cast raw
    integers across surfaces" rule rests on. *)
Theorem same_name_different_code :
  ldict_encode LdictClosed <> vt_encode VtClosed /\
  ldict_encode LdictLimitExceeded <> vt_encode VtLimitExceeded /\
  ldict_encode LdictIoError <> vt_encode VtIoError.
Proof.
  repeat split; discriminate.
Qed.

(** A concrete collision witness: ldict's 8 is Closed, interop's 8 is
    ProviderError — decoding one table's byte with the other's decoder
    yields a DIFFERENT, valid-looking status. *)
Theorem raw_reuse_misroutes :
  ldict_decode (vt_encode VtProviderError) = Some LdictClosed /\
  vt_decode (ldict_encode LdictClosed) = Some VtProviderError.
Proof.
  split; reflexivity.
Qed.
