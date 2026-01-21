(** * NodeTypes: ARTrie Node Type Definitions

    This module defines the node types used in the Adaptive Radix Trie:
    - Node4: 1-4 children, linear search
    - Node16: 5-16 children, SIMD-optimized search
    - Node48: 17-48 children, index array lookup
    - Node256: 49-256 children, direct array access

    Each node type has:
    - A header with type, prefix length, flags, and version
    - Path compression prefix (up to 12 bytes)
    - Child pointers (organization varies by type)
*)

Require Import Coq.Lists.List.
Require Import Coq.Arith.Arith.
Require Import Coq.Bool.Bool.
Require Import Coq.micromega.Lia.
Require Import ARTrie.Model.Key.
Import ListNotations.

(** ** Node Type Enumeration *)

Inductive NodeType :=
  | TNode4
  | TNode16
  | TNode48
  | TNode256
  | TBucket.

Definition node_type_eq_dec (t1 t2 : NodeType) : {t1 = t2} + {t1 <> t2}.
Proof.
  decide equality.
Defined.

(** ** Node Capacity Constants *)

Definition MAX_PREFIX_LEN : nat := 12.
Definition NODE4_CAPACITY : nat := 4.
Definition NODE16_CAPACITY : nat := 16.
Definition NODE48_CAPACITY : nat := 48.
Definition NODE256_CAPACITY : nat := 256.
Definition MAX_BUCKET_ENTRIES : nat := 256.

(** Get capacity for a node type *)
Definition node_capacity (t : NodeType) : nat :=
  match t with
  | TNode4 => NODE4_CAPACITY
  | TNode16 => NODE16_CAPACITY
  | TNode48 => NODE48_CAPACITY
  | TNode256 => NODE256_CAPACITY
  | TBucket => MAX_BUCKET_ENTRIES
  end.

(** Get minimum children for shrinking *)
Definition node_min_children (t : NodeType) : nat :=
  match t with
  | TNode4 => 0      (* Cannot shrink *)
  | TNode16 => 4     (* Shrink to Node4 *)
  | TNode48 => 16    (* Shrink to Node16 *)
  | TNode256 => 48   (* Shrink to Node48 *)
  | TBucket => 0     (* Buckets don't shrink this way *)
  end.

(** ** Node Flags *)

Inductive NodeFlag :=
  | FlagFinal   (* Node represents a valid dictionary entry *)
  | FlagDirty   (* Node modified since last write-back *)
  | FlagLeaf.   (* Node points to a bucket *)

Definition NodeFlags := list NodeFlag.

Definition has_flag (flags : NodeFlags) (f : NodeFlag) : bool :=
  existsb (fun f' =>
    match f, f' with
    | FlagFinal, FlagFinal => true
    | FlagDirty, FlagDirty => true
    | FlagLeaf, FlagLeaf => true
    | _, _ => false
    end) flags.

Definition set_flag (flags : NodeFlags) (f : NodeFlag) : NodeFlags :=
  if has_flag flags f then flags else f :: flags.

Definition clear_flag (flags : NodeFlags) (f : NodeFlag) : NodeFlags :=
  filter (fun f' =>
    match f, f' with
    | FlagFinal, FlagFinal => false
    | FlagDirty, FlagDirty => false
    | FlagLeaf, FlagLeaf => false
    | _, _ => true
    end) flags.

(** ** Compressed Prefix *)

(** Prefix stored inline in node (up to 12 bytes) *)
Record CompressedPrefix := mkPrefix {
  prefix_bytes : list Byte;
  prefix_len : nat;
  prefix_len_valid : prefix_len = length prefix_bytes;
  prefix_len_bound : prefix_len <= MAX_PREFIX_LEN
}.

Definition empty_prefix : CompressedPrefix.
Proof.
  refine (mkPrefix [] 0 _ _).
  - reflexivity.
  - unfold MAX_PREFIX_LEN. lia.
Defined.

(** Create prefix from key segment *)
Program Definition make_prefix (k : Key) (H : length k <= MAX_PREFIX_LEN)
  : CompressedPrefix :=
  mkPrefix k (length k) eq_refl H.

(** ** Version Numbers *)

(** Version for optimistic locking: even = stable, odd = writing *)
Definition Version := nat.

Definition is_stable (v : Version) : bool := Nat.even v.
Definition is_writing (v : Version) : bool := Nat.odd v.

Definition begin_write_version (v : Version) : Version :=
  if is_stable v then S v else v.

Definition end_write_version (v : Version) : Version :=
  if is_writing v then S v else v.

(** ** Node Header *)

Record NodeHeader := mkHeader {
  header_type : NodeType;
  header_prefix_len : nat;
  header_flags : NodeFlags;
  header_num_children : nat;
  header_version : Version
}.

Definition empty_header (t : NodeType) : NodeHeader :=
  mkHeader t 0 [] 0 0.

(** ** Child Pointer Types *)

(** Abstract pointer type (will be refined in concrete modules) *)
Inductive ChildPtr :=
  | NullPtr
  | NodePtr (nid : nat)    (* Node identifier *)
  | BucketPtr (bid : nat). (* Bucket identifier *)

Definition is_null (p : ChildPtr) : bool :=
  match p with
  | NullPtr => true
  | _ => false
  end.

(** Child array: maps byte to child pointer *)
Definition ChildArray := Byte -> ChildPtr.

Definition empty_children : ChildArray := fun _ => NullPtr.

Definition set_child (children : ChildArray) (b : Byte) (p : ChildPtr)
  : ChildArray :=
  fun b' => if byte_eqb b b' then p else children b'.

Definition get_child (children : ChildArray) (b : Byte) : ChildPtr :=
  children b.

(** ** Node4 *)

(** Node4 stores keys and children in sorted arrays *)
Record Node4Data := mkNode4 {
  n4_keys : list Byte;
  n4_children : list ChildPtr;
  n4_keys_len : length n4_keys <= NODE4_CAPACITY;
  n4_children_len : length n4_children = length n4_keys;
  n4_keys_sorted : forall i j,
    i < j -> j < length n4_keys ->
    byte_val (nth i n4_keys (make_byte 0 ltac:(lia))) <
    byte_val (nth j n4_keys (make_byte 0 ltac:(lia)))
}.

(** Find child in Node4 *)
Fixpoint node4_find_child (keys : list Byte) (children : list ChildPtr)
  (b : Byte) : ChildPtr :=
  match keys, children with
  | [], _ => NullPtr
  | _, [] => NullPtr
  | k :: ks, c :: cs =>
      if byte_eqb k b then c
      else node4_find_child ks cs b
  end.

(** ** Node16 *)

(** Node16 uses SIMD-aligned keys array *)
Record Node16Data := mkNode16 {
  n16_keys : list Byte;
  n16_children : list ChildPtr;
  n16_keys_len : length n16_keys <= NODE16_CAPACITY;
  n16_children_len : length n16_children = length n16_keys;
  n16_keys_sorted : forall i j,
    i < j -> j < length n16_keys ->
    byte_val (nth i n16_keys (make_byte 0 ltac:(lia))) <
    byte_val (nth j n16_keys (make_byte 0 ltac:(lia)))
}.

(** Find child in Node16 (models SIMD search) *)
Definition node16_find_child (data : Node16Data) (b : Byte) : ChildPtr :=
  node4_find_child (n16_keys data) (n16_children data) b.

(** ** Node48 *)

(** Node48 uses an index array for O(1) lookup *)
Record Node48Data := mkNode48 {
  n48_index : Byte -> option nat;  (* Byte -> slot index *)
  n48_children : list ChildPtr;
  n48_children_len : length n48_children <= NODE48_CAPACITY;
  n48_index_valid : forall b idx,
    n48_index b = Some idx -> idx < length n48_children
}.

(** Find child in Node48 *)
Definition node48_find_child (data : Node48Data) (b : Byte) : ChildPtr :=
  match n48_index data b with
  | None => NullPtr
  | Some idx => nth idx (n48_children data) NullPtr
  end.

(** ** Node256 *)

(** Node256 uses direct indexing *)
Record Node256Data := mkNode256 {
  n256_children : ChildArray
}.

(** Find child in Node256 *)
Definition node256_find_child (data : Node256Data) (b : Byte) : ChildPtr :=
  n256_children data b.

(** ** Unified Node Type *)

Inductive NodeData :=
  | DataNode4 (data : Node4Data)
  | DataNode16 (data : Node16Data)
  | DataNode48 (data : Node48Data)
  | DataNode256 (data : Node256Data)
  | DataBucket. (* Bucket data handled separately *)

Record Node := mkNode {
  node_header : NodeHeader;
  node_prefix : CompressedPrefix;
  node_data : NodeData
}.

(** Get node type from node *)
Definition get_node_type (n : Node) : NodeType :=
  header_type (node_header n).

(** Get child count from node *)
Definition get_child_count (n : Node) : nat :=
  header_num_children (node_header n).

(** Find child in any node type *)
Definition find_child (n : Node) (b : Byte) : ChildPtr :=
  match node_data n with
  | DataNode4 data => node4_find_child (n4_keys data) (n4_children data) b
  | DataNode16 data => node16_find_child data b
  | DataNode48 data => node48_find_child data b
  | DataNode256 data => node256_find_child data b
  | DataBucket => NullPtr
  end.

(** ** Node Well-formedness *)

Definition wf_node4 (data : Node4Data) : Prop :=
  length (n4_keys data) = length (n4_children data) /\
  length (n4_keys data) <= NODE4_CAPACITY.

Definition wf_node16 (data : Node16Data) : Prop :=
  length (n16_keys data) = length (n16_children data) /\
  length (n16_keys data) <= NODE16_CAPACITY.

Definition wf_node48 (data : Node48Data) : Prop :=
  length (n48_children data) <= NODE48_CAPACITY.

Definition wf_node256 (data : Node256Data) : Prop :=
  True. (* Node256 is always well-formed *)

Definition wf_node_data (data : NodeData) : Prop :=
  match data with
  | DataNode4 d => wf_node4 d
  | DataNode16 d => wf_node16 d
  | DataNode48 d => wf_node48 d
  | DataNode256 d => wf_node256 d
  | DataBucket => True
  end.

Definition wf_node (n : Node) : Prop :=
  wf_node_data (node_data n) /\
  header_num_children (node_header n) <= node_capacity (get_node_type n).

(** ** Node Type Predicates *)

Definition should_grow (n : Node) : bool :=
  let t := get_node_type n in
  let count := get_child_count n in
  match t with
  | TNode4 => Nat.leb NODE4_CAPACITY count
  | TNode16 => Nat.leb NODE16_CAPACITY count
  | TNode48 => Nat.leb NODE48_CAPACITY count
  | TNode256 => false
  | TBucket => false
  end.

Definition should_shrink (n : Node) : bool :=
  let t := get_node_type n in
  let count := get_child_count n in
  match t with
  | TNode4 => false
  | TNode16 => Nat.leb count 4
  | TNode48 => Nat.leb count 16
  | TNode256 => Nat.leb count 48
  | TBucket => false
  end.

(** Target type after growth *)
Definition grow_target (t : NodeType) : NodeType :=
  match t with
  | TNode4 => TNode16
  | TNode16 => TNode48
  | TNode48 => TNode256
  | TNode256 => TNode256
  | TBucket => TBucket
  end.

(** Target type after shrink *)
Definition shrink_target (t : NodeType) : NodeType :=
  match t with
  | TNode4 => TNode4
  | TNode16 => TNode4
  | TNode48 => TNode16
  | TNode256 => TNode48
  | TBucket => TBucket
  end.
