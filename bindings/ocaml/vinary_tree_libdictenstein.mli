type t
type lookup = { found : bool; value : int64 option }
type entry_key = Bytes of bytes | Unicode of string | U64 of int64 array
type entry = { key : entry_key; value : int64 option }
type value_domain = Unit | Optional_u64
type entries_metadata = {
  unit_domain : Vinary_tree_interop.unit_domain;
  value_domain : value_domain;
  exact_length : int64 option;
  snapshot_identity : (int64 * int64) option;
}

(** Native ABI version (LDICT_ABI_VERSION); always 1 for this family. *)
val abi_version : unit -> int

(** Compatible-additions revision within the ABI version (LDICT_API_REVISION). *)
val api_revision : unit -> int

val dynamic_dawg : ?domain:Vinary_tree_interop.unit_domain -> unit -> t
val double_array_trie :
  ?domain:Vinary_tree_interop.unit_domain -> (string * int64 option) array -> t
val scdawg : ?domain:Vinary_tree_interop.unit_domain -> unit -> t
val create_persistent_artrie :
  ?domain:Vinary_tree_interop.unit_domain -> string -> t
val open_persistent_artrie :
  ?domain:Vinary_tree_interop.unit_domain -> string -> t
val create_persistent_vocabulary : string -> t
val open_persistent_vocabulary : string -> t
val close : t -> unit
val resource : t -> Vinary_tree_interop.resource
val length : t -> int
val kind : t -> int
val capabilities : t -> int64
val put : t -> string -> int64 option -> bool
val put_many : t -> (string * int64 option) array -> int
val remove : t -> string -> bool
val contains : t -> string -> bool
val get : t -> string -> lookup
val put_u64 : t -> int64 array -> int64 option -> bool
val remove_u64 : t -> int64 array -> bool
val contains_u64 : t -> int64 array -> bool
val get_u64 : t -> int64 array -> lookup
val clear : t -> unit
val compact : t -> int
val checkpoint : t -> unit
val contains_substring : t -> string -> bool
val substring_frequency : t -> string -> int
val term : t -> int64 -> string option

(** Scope a lazy lexicographic sequence to [action]. Every native batch is
    copied and released before a sequence node is exposed. The cursor closes
    after [action], on early return, and when [action] raises; a captured
    sequence therefore cannot keep a native lease alive. *)
val with_entries_seq :
  ?max_entries:int -> ?max_units:int -> ?max_values:int -> t ->
  (entries_metadata -> entry Seq.t -> 'a) -> 'a

(** Resource-scoped fold over one immutable dictionary revision. *)
val fold_entries :
  ?max_entries:int -> ?max_units:int -> ?max_values:int -> t ->
  init:'a -> f:('a -> entry -> 'a) -> 'a
