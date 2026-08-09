type t
type lookup = { found : bool; value : int64 option }

external abi_version : unit -> int = "ocaml_ldict_abi_version"
external api_revision : unit -> int = "ocaml_ldict_api_revision"
external raw_dynamic_dawg : Vinary_tree_interop.unit_domain -> t
  = "ocaml_ldict_dynamic_dawg"
external raw_double_array_trie :
  Vinary_tree_interop.unit_domain -> (string * int64 option) array -> t
  = "ocaml_ldict_double_array_trie"
external raw_scdawg : Vinary_tree_interop.unit_domain -> t = "ocaml_ldict_scdawg"
external raw_create_persistent_artrie :
  Vinary_tree_interop.unit_domain -> string -> t
  = "ocaml_ldict_create_persistent_artrie"
external raw_open_persistent_artrie :
  Vinary_tree_interop.unit_domain -> string -> t
  = "ocaml_ldict_open_persistent_artrie"
external create_persistent_vocabulary : string -> t
  = "ocaml_ldict_create_persistent_vocabulary"
external open_persistent_vocabulary : string -> t
  = "ocaml_ldict_open_persistent_vocabulary"
external close : t -> unit = "ocaml_ldict_close"
external resource : t -> Vinary_tree_interop.resource = "ocaml_ldict_resource"
external length : t -> int = "ocaml_ldict_length"
external kind : t -> int = "ocaml_ldict_kind"
external capabilities : t -> int64 = "ocaml_ldict_capabilities"
external put : t -> string -> int64 option -> bool = "ocaml_ldict_put"
external put_many : t -> (string * int64 option) array -> int = "ocaml_ldict_put_many"
external remove : t -> string -> bool = "ocaml_ldict_remove"
external contains : t -> string -> bool = "ocaml_ldict_contains"
external raw_get : t -> string -> bool * int64 option = "ocaml_ldict_get"
external put_u64 : t -> int64 array -> int64 option -> bool = "ocaml_ldict_put_u64"
external remove_u64 : t -> int64 array -> bool = "ocaml_ldict_remove_u64"
external contains_u64 : t -> int64 array -> bool = "ocaml_ldict_contains_u64"
external raw_get_u64 : t -> int64 array -> bool * int64 option = "ocaml_ldict_get_u64"
external clear : t -> unit = "ocaml_ldict_clear"
external compact : t -> int = "ocaml_ldict_compact"
external checkpoint : t -> unit = "ocaml_ldict_checkpoint"
external contains_substring : t -> string -> bool = "ocaml_ldict_contains_substring"
external substring_frequency : t -> string -> int = "ocaml_ldict_substring_frequency"
external term : t -> int64 -> string option = "ocaml_ldict_term"

let dynamic_dawg ?(domain = Vinary_tree_interop.Unicode_scalar) () =
  raw_dynamic_dawg domain

let double_array_trie ?(domain = Vinary_tree_interop.Unicode_scalar) entries =
  raw_double_array_trie domain entries

let scdawg ?(domain = Vinary_tree_interop.Unicode_scalar) () = raw_scdawg domain

let create_persistent_artrie
    ?(domain = Vinary_tree_interop.Unicode_scalar) path =
  raw_create_persistent_artrie domain path

let open_persistent_artrie
    ?(domain = Vinary_tree_interop.Unicode_scalar) path =
  raw_open_persistent_artrie domain path

let get dictionary text =
  let found, value = raw_get dictionary text in
  { found; value }

let get_u64 dictionary tokens =
  let found, value = raw_get_u64 dictionary tokens in
  { found; value }
