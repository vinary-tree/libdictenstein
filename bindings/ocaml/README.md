# Vinary Tree libdictenstein for OCaml

C stubs over libdictenstein's stable `ldict_*` C ABI, built with Dune. The opam
package is `vinary-tree-libdictenstein`; the module is
`Vinary_tree_libdictenstein`. It exposes DynamicDAWG CRUD and batch insertion,
immutable DoubleArrayTrie construction, SCDAWG substring search, persistent
ARTrie CRUD/checkpoint/reopen, and persistent vocabulary reverse lookup.

## Native library

The stubs link the shared library `libdictenstein`. Build it and put it on the
loader path:

```sh
cargo build --release --no-default-features --features ffi
export LD_LIBRARY_PATH="$PWD/target/release:$LD_LIBRARY_PATH"
```

The in-repo `include/` directory carries copies of `libdictenstein.h` and
`vinary_tree_interop.h` for the opam build.

## Quickstart

```ocaml
open Vinary_tree_libdictenstein

let () =
  let d = dynamic_dawg () in                       (* Unicode-scalar default *)
  ignore (put_many d [| ("cat", Some 1L); ("cot", Some 2L); ("cut", None) |]);
  assert (length d = 3);
  let hit = get d "cot" in
  assert (hit.found && hit.value = Some 2L);

  let s = scdawg () in
  ignore (put s "cat" None);
  ignore (put s "cot" None);
  assert (contains_substring s "ot");
  assert (substring_frequency s "t" = 2);
  close d;
  close s
```

Pass `~domain:Vinary_tree_interop.Byte` (or `U64`) to a constructor to select a
non-default unit domain.

## Backends and capabilities

| Constructor | Kind | Unit domains | Capabilities |
|-------------|------|--------------|--------------|
| `dynamic_dawg` | 1 | Byte, Unicode_scalar, U64 | read, insert, remove, clear, compact |
| `double_array_trie` | 2 | Byte, Unicode_scalar | read (immutable) |
| `scdawg` | 3 | Byte, Unicode_scalar | read, insert, substring |
| `create/open_persistent_artrie` | 4 | Byte, Unicode_scalar, U64 | read, insert, remove, checkpoint |
| `create/open_persistent_vocabulary` | 5 | Unicode_scalar | read, insert, checkpoint |

`kind` and `capabilities` report the runtime backend id and `LDICT_CAP_*`
bitset.

## Values and domains

Text terms are `string` (UTF-8 for the Unicode-scalar backends, which validate
it). The u64 API (`put_u64`, `contains_u64`, `get_u64`, `remove_u64`) takes
`int64 array`. Values are `int64 option`; because OCaml's native `int64` is
signed, the two's-complement bit pattern is passed through to the u64 slot, so
the full `0 .. UINT64_MAX` range is expressible. `None` and `Some 0L` are
distinct. `get` returns `{ found : bool; value : int64 option }`.

## Error handling

Non-OK statuses raise `Failure` carrying the thread-local
`ldict_last_error_message()`. Backend-unsupported operations surface the
`UNSUPPORTED` status; wrong-domain terms surface `DOMAIN_MISMATCH` (9).

## Retained resource handoff

`resource` returns the shared `Vinary_tree_interop.resource`. An independently
packaged liblevenshtein transducer retains it in constant time and keeps its
query-start revision valid after `close`.
