type t
type lookup = { found : bool; value : int64 option }
type algebra_operation = Union | Intersection | Difference | Symmetric_difference
type value_merge = First | Last | Lattice_join | Lattice_meet
type entry_key = Bytes of bytes | Unicode of string | U64 of int64 array
type entry = { key : entry_key; value : int64 option }
type value_domain = Unit | Optional_u64
type entries_metadata = {
  unit_domain : Vinary_tree_interop.unit_domain;
  value_domain : value_domain;
  exact_length : int64 option;
  snapshot_identity : (int64 * int64) option;
}
type entry_cursor

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
external raw_algebra : t -> t -> int -> int -> t = "ocaml_ldict_algebra"
external checkpoint : t -> unit = "ocaml_ldict_checkpoint"
external contains_substring : t -> string -> bool = "ocaml_ldict_contains_substring"
external substring_frequency : t -> string -> int = "ocaml_ldict_substring_frequency"
external term : t -> int64 -> string option = "ocaml_ldict_term"
external raw_entries_open : t -> int -> int -> int -> entry_cursor
  = "ocaml_ldict_entries_open"
external raw_entries_metadata : entry_cursor -> entries_metadata
  = "ocaml_ldict_entries_metadata"
external raw_entries_next : entry_cursor -> entry option
  = "ocaml_ldict_entries_next"
external raw_entries_close : entry_cursor -> unit = "ocaml_ldict_entries_close"

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

let operation_code = function
  | Union -> 1
  | Intersection -> 2
  | Difference -> 3
  | Symmetric_difference -> 4

let value_merge_code = function
  | First -> 1
  | Last -> 2
  | Lattice_join -> 3
  | Lattice_meet -> 4

let algebra ?(value_merge = Last) operation left right =
  raw_algebra left right (operation_code operation) (value_merge_code value_merge)

let union ?(value_merge = Last) left right =
  algebra ~value_merge Union left right

let intersection ?(value_merge = Lattice_meet) left right =
  algebra ~value_merge Intersection left right

let difference left right = algebra ~value_merge:First Difference left right

let symmetric_difference left right =
  algebra ~value_merge:First Symmetric_difference left right

let open_entries ?(max_entries = 256) ?(max_units = 65536)
    ?(max_values = 256) dictionary =
  if max_entries <= 0 || max_units < 0 || max_values < 0 then
    invalid_arg "invalid dictionary entry batch limits";
  raw_entries_open dictionary max_entries max_units max_values

let with_entries_seq ?max_entries ?max_units ?max_values dictionary action =
  let cursor = open_entries ?max_entries ?max_units ?max_values dictionary in
  let rec sequence () =
    match raw_entries_next cursor with
    | None -> Seq.Nil
    | Some entry -> Seq.Cons (entry, sequence)
  in
  Fun.protect
    ~finally:(fun () -> raw_entries_close cursor)
    (fun () -> action (raw_entries_metadata cursor) sequence)

let fold_entries ?max_entries ?max_units ?max_values dictionary ~init ~f =
  with_entries_seq ?max_entries ?max_units ?max_values dictionary
    (fun _ entries -> Seq.fold_left f init entries)
