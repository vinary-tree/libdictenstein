(** * RootDescriptorReopenSpec: persistent root publication and reopen refinement

    This specification models the fixed root descriptor used by the persistent
    byte and char ART backends.  The checked boundary is intentionally narrow:
    a valid descriptor may publish a checkpointed map, while malformed
    descriptor fields, bad arena counts, or failed root loads must fail closed
    into WAL replay without trusting the checkpoint skip threshold.

    Filesystem, kernel, mmap/io_uring internals, and certified Rust
    compilation are outside this proof boundary.
*)

From Stdlib Require Import List.
From Stdlib Require Import Arith.
From Stdlib Require Import Lia.
From Stdlib Require Import Bool.
Import ListNotations.

(** ** Reference maps and WAL replay *)

Definition Key := nat.
Definition Value := nat.
Definition RefMap := Key -> option Value.

Definition empty_map : RefMap := fun _ => None.

Definition lookup (map : RefMap) (key : Key) : option Value :=
  map key.

Definition map_put (map : RefMap) (key : Key) (value : Value) : RefMap :=
  fun query => if Nat.eq_dec query key then Some value else map query.

Definition map_remove (map : RefMap) (key : Key) : RefMap :=
  fun query => if Nat.eq_dec query key then None else map query.

Theorem map_put_lookup_same :
  forall map key value,
    lookup (map_put map key value) key = Some value.
Proof.
  intros map key value.
  unfold lookup, map_put.
  destruct (Nat.eq_dec key key) as [_ | Hneq].
  - reflexivity.
  - exfalso. apply Hneq. reflexivity.
Qed.

Theorem map_remove_lookup_same :
  forall map key,
    lookup (map_remove map key) key = None.
Proof.
  intros map key.
  unfold lookup, map_remove.
  destruct (Nat.eq_dec key key) as [_ | Hneq].
  - reflexivity.
  - exfalso. apply Hneq. reflexivity.
Qed.

Inductive WalOp : Type :=
| WalPut (lsn : nat) (key : Key) (value : Value)
| WalDelete (lsn : nat) (key : Key)
| WalCheckpoint (lsn : nat) (checkpoint_lsn : nat).

Definition op_lsn (op : WalOp) : nat :=
  match op with
  | WalPut lsn _ _ => lsn
  | WalDelete lsn _ => lsn
  | WalCheckpoint lsn _ => lsn
  end.

Definition apply_wal_op (map : RefMap) (op : WalOp) : RefMap :=
  match op with
  | WalPut _ key value => map_put map key value
  | WalDelete _ key => map_remove map key
  | WalCheckpoint _ _ => map
  end.

Fixpoint replay_after (threshold : option nat) (ops : list WalOp) (map : RefMap)
  : RefMap :=
  match ops with
  | [] => map
  | op :: rest =>
      let skip :=
        match threshold with
        | Some limit => op_lsn op <=? limit
        | None => false
        end in
      if skip then
        replay_after threshold rest map
      else
        replay_after threshold rest (apply_wal_op map op)
  end.

Theorem replay_without_threshold_applies_put :
  forall ops map lsn key value,
    lookup
      (replay_after None (WalPut lsn key value :: ops) map)
      key =
    lookup (replay_after None ops (map_put map key value)) key.
Proof.
  reflexivity.
Qed.

Theorem replay_with_threshold_skips_old_put :
  forall ops map limit lsn key value,
    lsn <= limit ->
    replay_after (Some limit) (WalPut lsn key value :: ops) map =
    replay_after (Some limit) ops map.
Proof.
  intros ops map limit lsn key value Hle.
  simpl.
  assert (Hskip : (lsn <=? limit) = true).
  { apply Nat.leb_le. exact Hle. }
  rewrite Hskip.
  reflexivity.
Qed.

(** ** Root descriptor model *)

Inductive RootKind : Type :=
| EmptyRoot
| BucketRoot
| ArtNodeRoot
| UnknownRoot (tag : nat).

Definition known_root_kind (kind : RootKind) : bool :=
  match kind with
  | EmptyRoot | BucketRoot | ArtNodeRoot => true
  | UnknownRoot _ => false
  end.

Record RootDescriptor := mkRootDescriptor {
  descriptor_kind : RootKind;
  descriptor_final_flag : nat;
  descriptor_term_count : nat;
  descriptor_arena_count : nat;
  descriptor_root_ptr : option nat
}.

Definition valid_final_flag (descriptor : RootDescriptor) : bool :=
  descriptor_final_flag descriptor <=? 1.

Definition valid_empty_payload (descriptor : RootDescriptor) : bool :=
  match descriptor_kind descriptor with
  | EmptyRoot =>
      (descriptor_term_count descriptor =? 0) &&
      match descriptor_root_ptr descriptor with
      | None => true
      | Some _ => false
      end
  | _ => true
  end.

Definition valid_payload_pointer (descriptor : RootDescriptor) : bool :=
  match descriptor_kind descriptor with
  | EmptyRoot => true
  | BucketRoot | ArtNodeRoot =>
      match descriptor_root_ptr descriptor with
      | Some _ => true
      | None => false
      end
  | UnknownRoot _ => false
  end.

Definition descriptor_valid
  (available_arena_blocks : nat)
  (descriptor : RootDescriptor) : bool :=
  known_root_kind (descriptor_kind descriptor) &&
  valid_final_flag descriptor &&
  valid_empty_payload descriptor &&
  valid_payload_pointer descriptor &&
  (descriptor_arena_count descriptor <=? available_arena_blocks).

Theorem valid_descriptor_has_known_kind :
  forall available descriptor,
    descriptor_valid available descriptor = true ->
    known_root_kind (descriptor_kind descriptor) = true.
Proof.
  intros available descriptor Hvalid.
  unfold descriptor_valid in Hvalid.
  repeat rewrite andb_true_iff in Hvalid.
  destruct Hvalid as [[[[Hkind _] _] _] _].
  exact Hkind.
Qed.

Theorem unknown_root_kind_rejected :
  forall available tag final_flag term_count arena_count root_ptr,
    descriptor_valid available
      (mkRootDescriptor
        (UnknownRoot tag) final_flag term_count arena_count root_ptr) = false.
Proof.
  reflexivity.
Qed.

Theorem invalid_final_flag_rejected :
  forall available kind term_count arena_count root_ptr final_flag,
    1 < final_flag ->
    descriptor_valid available
      (mkRootDescriptor kind final_flag term_count arena_count root_ptr) = false.
Proof.
  intros available kind term_count arena_count root_ptr final_flag Hgt.
  unfold descriptor_valid, valid_final_flag.
  simpl.
  assert (Hflag : (final_flag <=? 1) = false).
  { apply Nat.leb_gt. exact Hgt. }
  rewrite Hflag.
  destruct (known_root_kind kind); reflexivity.
Qed.

Theorem excessive_arena_count_rejected :
  forall available kind final_flag term_count arena_count root_ptr,
    available < arena_count ->
    descriptor_valid available
      (mkRootDescriptor kind final_flag term_count arena_count root_ptr) = false.
Proof.
  intros available kind final_flag term_count arena_count root_ptr Hlt.
  unfold descriptor_valid.
  simpl.
  assert (Harena : (arena_count <=? available) = false).
  { apply Nat.leb_gt. exact Hlt. }
  rewrite Harena.
  repeat rewrite andb_false_r.
  reflexivity.
Qed.

Theorem empty_descriptor_requires_empty_payload :
  forall available final_flag term_count arena_count root_ptr,
    descriptor_valid available
      (mkRootDescriptor EmptyRoot final_flag term_count arena_count root_ptr) = true ->
    term_count = 0 /\ root_ptr = None.
Proof.
  intros available final_flag term_count arena_count root_ptr Hvalid.
  unfold descriptor_valid, valid_empty_payload,
    valid_final_flag, valid_payload_pointer in Hvalid.
  destruct term_count as [| count]; destruct root_ptr as [ptr |];
    simpl in Hvalid.
  - destruct (final_flag <=? 1); simpl in Hvalid; discriminate.
  - split; reflexivity.
  - destruct (final_flag <=? 1); simpl in Hvalid; discriminate.
  - destruct (final_flag <=? 1); simpl in Hvalid; discriminate.
Qed.

(** ** Reopen model *)

Inductive RootLoad : Type :=
| RootLoaded (map : RefMap)
| RootLoadFailed.

Definition descriptor_publish_map
  (available_arena_blocks : nat)
  (descriptor : RootDescriptor)
  (checkpoint_map : RefMap)
  (root_load : RootLoad) : option RefMap :=
  if descriptor_valid available_arena_blocks descriptor then
    match root_load with
    | RootLoaded loaded => Some loaded
    | RootLoadFailed => None
    end
  else
    None.

Definition checkpoint_threshold
  (available_arena_blocks : nat)
  (descriptor : RootDescriptor)
  (root_load : RootLoad)
  (checkpoint_lsn : option nat) : option nat :=
  match descriptor_publish_map available_arena_blocks descriptor empty_map root_load with
  | Some _ => checkpoint_lsn
  | None => None
  end.

Definition reopen_model
  (available_arena_blocks : nat)
  (descriptor : RootDescriptor)
  (checkpoint_map : RefMap)
  (root_load : RootLoad)
  (checkpoint_lsn : option nat)
  (ops : list WalOp) : RefMap :=
  match descriptor_publish_map available_arena_blocks descriptor checkpoint_map root_load with
  | Some loaded =>
      replay_after
        (checkpoint_threshold
          available_arena_blocks descriptor root_load checkpoint_lsn)
        ops
        loaded
  | None => replay_after None ops empty_map
  end.

Theorem valid_loaded_root_roundtrips_checkpoint_map :
  forall available descriptor checkpoint_map checkpoint_lsn,
    descriptor_valid available descriptor = true ->
    reopen_model available descriptor checkpoint_map
      (RootLoaded checkpoint_map) checkpoint_lsn [] =
    checkpoint_map.
Proof.
  intros available descriptor checkpoint_map checkpoint_lsn Hvalid.
  unfold reopen_model, descriptor_publish_map.
  rewrite Hvalid.
  reflexivity.
Qed.

Theorem invalid_descriptor_ignores_checkpoint_map :
  forall available descriptor checkpoint_map checkpoint_lsn ops,
    descriptor_valid available descriptor = false ->
    reopen_model available descriptor checkpoint_map
      (RootLoaded checkpoint_map) checkpoint_lsn ops =
    replay_after None ops empty_map.
Proof.
  intros available descriptor checkpoint_map checkpoint_lsn ops Hinvalid.
  unfold reopen_model, descriptor_publish_map.
  rewrite Hinvalid.
  reflexivity.
Qed.

Theorem failed_root_load_replays_wal_from_zero :
  forall available descriptor checkpoint_map checkpoint_lsn ops,
    reopen_model available descriptor checkpoint_map
      RootLoadFailed checkpoint_lsn ops =
    replay_after None ops empty_map.
Proof.
  intros available descriptor checkpoint_map checkpoint_lsn ops.
  unfold reopen_model, descriptor_publish_map.
  destruct (descriptor_valid available descriptor); reflexivity.
Qed.

Theorem checkpoint_threshold_trusted_only_after_loaded_root :
  forall available descriptor checkpoint_lsn,
    checkpoint_threshold available descriptor RootLoadFailed checkpoint_lsn = None.
Proof.
  intros available descriptor checkpoint_lsn.
  unfold checkpoint_threshold, descriptor_publish_map.
  destruct (descriptor_valid available descriptor); reflexivity.
Qed.

Theorem malformed_descriptor_with_checkpoint_does_not_skip_wal :
  forall available descriptor checkpoint_map checkpoint_lsn ops key value lsn,
    descriptor_valid available descriptor = false ->
    lookup
      (reopen_model available descriptor checkpoint_map
        (RootLoaded checkpoint_map) checkpoint_lsn
        (WalPut lsn key value :: ops))
      key =
    lookup
      (replay_after None ops (map_put empty_map key value))
      key.
Proof.
  intros available descriptor checkpoint_map checkpoint_lsn ops key value lsn Hinvalid.
  rewrite invalid_descriptor_ignores_checkpoint_map by exact Hinvalid.
  reflexivity.
Qed.

(** ** Lazy-read fail-closed model *)

Inductive LazyLookup : Type :=
| LazyFound (value : Value)
| LazyMissing
| LazyReadError.

Definition try_contains_model (lookup_result : LazyLookup) : option bool :=
  match lookup_result with
  | LazyFound _ => Some true
  | LazyMissing => Some false
  | LazyReadError => None
  end.

Definition public_contains_model (lookup_result : LazyLookup) : bool :=
  match try_contains_model lookup_result with
  | Some result => result
  | None => false
  end.

Definition try_get_model (lookup_result : LazyLookup) : option (option Value) :=
  match lookup_result with
  | LazyFound value => Some (Some value)
  | LazyMissing => Some None
  | LazyReadError => None
  end.

Definition public_get_model (lookup_result : LazyLookup) : option Value :=
  match try_get_model lookup_result with
  | Some result => result
  | None => None
  end.

Theorem try_contains_reports_lazy_error :
  try_contains_model LazyReadError = None.
Proof.
  reflexivity.
Qed.

Theorem public_contains_fails_closed_on_lazy_error :
  public_contains_model LazyReadError = false.
Proof.
  reflexivity.
Qed.

Theorem try_get_reports_lazy_error :
  try_get_model LazyReadError = None.
Proof.
  reflexivity.
Qed.

Theorem public_get_fails_closed_on_lazy_error :
  public_get_model LazyReadError = None.
Proof.
  reflexivity.
Qed.

(** ** Byte and char backends share the abstract reopen boundary *)

Inductive PersistentBackend : Type :=
| ByteBackend
| CharBackend.

Definition backend_reopen_model
  (_backend : PersistentBackend)
  (available_arena_blocks : nat)
  (descriptor : RootDescriptor)
  (checkpoint_map : RefMap)
  (root_load : RootLoad)
  (checkpoint_lsn : option nat)
  (ops : list WalOp) : RefMap :=
  reopen_model
    available_arena_blocks descriptor checkpoint_map root_load checkpoint_lsn ops.

Theorem byte_char_reopen_use_same_model :
  forall available descriptor checkpoint_map root_load checkpoint_lsn ops,
    backend_reopen_model ByteBackend
      available descriptor checkpoint_map root_load checkpoint_lsn ops =
    backend_reopen_model CharBackend
      available descriptor checkpoint_map root_load checkpoint_lsn ops.
Proof.
  reflexivity.
Qed.
