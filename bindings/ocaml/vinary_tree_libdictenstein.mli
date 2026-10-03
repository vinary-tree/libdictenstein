(** Native, snapshot-capable dictionary backends for OCaml.
    A dictionary handle owns one backend. Close it with {!close}; use
    [Fun.protect] around all owned handles, including algebra results.
    Exported resources are retained independently by consumers. *)

(** Owned dictionary handle. Its backend and unit domain are fixed at creation. *)
type t

(** Typed suffix-source handle. The current OCaml facade reports unsupported
    rather than pretending this is an ordinary dictionary. *)
type suffix_index

(** Lookup distinguishes an absent key from a present key with no value.
    For example [{found = true; value = None}] is a real entry. *)
type lookup = { found : bool; value : int64 option }

(** Set operation over one immutable snapshot of each input. *)
type algebra_operation = Union | Intersection | Difference | Symmetric_difference

(** Value resolution for overlapping keys. [First] and [Last] select an input;
    lattice join/meet compute optional unsigned-64 maximum/minimum. *)
type value_merge = First | Last | Lattice_join | Lattice_meet

(** Domain-preserving entry key. [Bytes] may contain arbitrary octets;
    [Unicode] is UTF-8; [U64] carries unsigned token bits in [int64]. *)
type entry_key = Bytes of bytes | Unicode of string | U64 of int64 array

(** Owned entry copied out of a native snapshot. *)
type entry = { key : entry_key; value : int64 option }

(** Dictionary value representation. *)
type value_domain = Unit | Optional_u64

(** Metadata for a scoped entry traversal. [snapshot_identity] identifies
    the captured revision when the backend provides one. *)
type entries_metadata = {
  unit_domain : Vinary_tree_interop.unit_domain;
  value_domain : value_domain;
  exact_length : int64 option;
  snapshot_identity : (int64 * int64) option;
}

(** Native ABI version, independent of package version. *)
val abi_version : unit -> int

(** Compatible-additions revision within the native ABI version. *)
val api_revision : unit -> int

(** Mutable dynamic directed acyclic word graph. Defaults to Unicode scalars;
    byte and unsigned-64 domains are also supported. *)
val dynamic_dawg : ?domain:Vinary_tree_interop.unit_domain -> unit -> t

(** Construct a read-optimized immutable double-array trie from entries.
    Supported unit domains are byte and Unicode scalar. *)
val double_array_trie :
  ?domain:Vinary_tree_interop.unit_domain -> (string * int64 option) array -> t

(** Mutable substring-indexing SCDAWG in byte or Unicode-scalar domain. *)
val scdawg : ?domain:Vinary_tree_interop.unit_domain -> unit -> t

(** This revision-7-linkable OCaml facade raises [Failure] with
    [UNSUPPORTED (status 6)]; it does not load revision-8 PathMap symbols. *)
val pathmap : ?domain:Vinary_tree_interop.unit_domain -> unit -> t

(** Raises [Failure] with [UNSUPPORTED (status 6)]. Typed suffix-source
    indexing is distinct from {!scdawg} and the ordinary dictionary API. *)
val suffix_index : ?domain:Vinary_tree_interop.unit_domain -> unit -> suffix_index

(** Create a persistent adaptive radix trie at [path]. Use {!checkpoint}
    to persist subsequent mutations before closing. *)
val create_persistent_artrie :
  ?domain:Vinary_tree_interop.unit_domain -> string -> t

(** Reopen a persistent adaptive radix trie at [path]. *)
val open_persistent_artrie :
  ?domain:Vinary_tree_interop.unit_domain -> string -> t

(** Create a persistent Unicode vocabulary with reverse term lookup. *)
val create_persistent_vocabulary : string -> t

(** Reopen a persistent Unicode vocabulary. *)
val open_persistent_vocabulary : string -> t

(** Release the dictionary handle. Previously retained resources and snapshots
    remain valid independently. *)
val close : t -> unit

(** Export a retained-interface resource for consumers such as liblevenshtein.
    The consumer acquires its own retain; no Rust object layout is shared. *)
val resource : t -> Vinary_tree_interop.resource

(** Number of terminal entries visible through this dictionary handle. *)
val length : t -> int

(** Native backend kind identifier. Use with {!capabilities} before invoking
    operations that are not supported by every backend. *)
val kind : t -> int

(** Backend capability bitset from the stable native ABI. *)
val capabilities : t -> int64

(** Insert or replace a text/byte-domain key. [None] stores valueless
    membership; [Some 0L] stores a zero value. Returns whether data changed. *)
val put : t -> string -> int64 option -> bool

(** Insert a batch and return the number of changed entries. *)
val put_many : t -> (string * int64 option) array -> int

(** Remove a text/byte-domain key; returns whether it was present. *)
val remove : t -> string -> bool

(** Test terminal membership in the text/byte domain. *)
val contains : t -> string -> bool

(** Look up membership and optional value without collapsing absent and
    present-valueless entries. *)
val get : t -> string -> lookup

(** Insert or replace a raw unsigned-64-token key. *)
val put_u64 : t -> int64 array -> int64 option -> bool

(** Remove a raw unsigned-64-token key. *)
val remove_u64 : t -> int64 array -> bool

(** Test raw unsigned-64-token membership. *)
val contains_u64 : t -> int64 array -> bool

(** Look up a raw unsigned-64-token key and optional value. *)
val get_u64 : t -> int64 array -> lookup

(** Remove all entries when the backend advertises clear capability. *)
val clear : t -> unit

(** Compact a mutable backend and return its native compaction result. *)
val compact : t -> int

(** Snapshot both inputs and perform one native ordered merge. The independent
    mutable DynamicDAWG result must be closed by the caller. Inputs must share
    the same unit domain. *)
val algebra : ?value_merge:value_merge -> algebra_operation -> t -> t -> t

(** All keys in either input. Overlap defaults to right/last value. *)
val union : ?value_merge:value_merge -> t -> t -> t

(** Keys in both inputs. Overlap defaults to optional-unsigned-64 meet. *)
val intersection : ?value_merge:value_merge -> t -> t -> t

(** Keys present only in the left input. *)
val difference : t -> t -> t

(** Keys present in exactly one input. *)
val symmetric_difference : t -> t -> t

(** Persist pending changes for a persistent backend. *)
val checkpoint : t -> unit

(** Test whether an SCDAWG indexes [substring]. *)
val contains_substring : t -> string -> bool

(** Count SCDAWG terms containing [substring]. *)
val substring_frequency : t -> string -> int

(** Reverse-lookup a persistent vocabulary identifier. *)
val term : t -> int64 -> string option

(** Scope a lazy lexicographic sequence to [action]. Each entry is copied
    before crossing into OCaml; the native cursor closes after [action], even
    on exception or early return. Do not let the sequence escape [action].
    Batch limits bound each native page, not the total number of entries. *)
val with_entries_seq :
  ?max_entries:int -> ?max_units:int -> ?max_values:int -> t ->
  (entries_metadata -> entry Seq.t -> 'a) -> 'a

(** Fold all entries of one immutable captured revision. The fold closes its
    native cursor on success or exception. *)
val fold_entries :
  ?max_entries:int -> ?max_units:int -> ?max_values:int -> t ->
  init:'a -> f:('a -> entry -> 'a) -> 'a
