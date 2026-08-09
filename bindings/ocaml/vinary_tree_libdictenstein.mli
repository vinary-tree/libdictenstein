type t
type lookup = { found : bool; value : int64 option }

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
