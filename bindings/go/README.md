# Vinary Tree libdictenstein for Go

A cgo facade over libdictenstein's stable `ldict_*` C ABI. The module is
`github.com/vinary-tree/libdictenstein/bindings/go`. It exposes DynamicDAWG
CRUD and batch insertion, immutable DoubleArrayTrie construction, SCDAWG
substring search, persistent ARTrie CRUD/checkpoint/reopen, and persistent
vocabulary reverse lookup, and lends each dictionary's retained resource to an
independent liblevenshtein transducer without serialization.

## Building the native library

cgo links `libdictenstein` (file name `liblibdictenstein.so`):

```sh
cargo build --release --no-default-features --features ffi
```

The `#cgo` directives in `libdictenstein.go` already add the in-repo header
roots. Point the linker and loader at the built library:

```sh
export CGO_LDFLAGS="-L$PWD/target/release"
export LD_LIBRARY_PATH="$PWD/target/release:$LD_LIBRARY_PATH"
```

The workspace file [`go.work`](../../go.work) wires this module to the sibling
interop and liblevenshtein Go modules for local development.

## Quickstart

```go
package main

import (
	"fmt"

	ld "github.com/vinary-tree/libdictenstein/bindings/go"
)

// A mapped u64 value is a *uint64, so nil and 0 stay distinct.
func value(v uint64) *uint64 { return &v }

func main() {
	dictionary, err := ld.NewDynamicDawg(ld.UnicodeScalarDomain)
	if err != nil {
		panic(err)
	}
	defer dictionary.Close()

	_, _ = dictionary.PutAll([]ld.Entry{
		{Term: "cat", Value: value(1)},
		{Term: "cot", Value: value(2)},
		{Term: "cut"}, // valueless
	})

	hit, _ := dictionary.Get("cot")
	fmt.Println(hit.Found, *hit.Value) // true 2

	suffixes, _ := ld.NewScdawg(ld.UnicodeScalarDomain)
	defer suffixes.Close()
	_, _ = suffixes.Put("cat", nil)
	_, _ = suffixes.Put("cot", nil)
	present, _ := suffixes.ContainsSubstring("ot")
	frequency, _ := suffixes.SubstringFrequency("t")
	fmt.Println(present, frequency) // true 2
}
```

`Close` is idempotent and a finalizer frees any handle a caller forgets, so a
double `Close` is safe.

## Backends and capabilities

| Constructor | Kind constant | Unit domains | Capabilities |
|-------------|---------------|--------------|--------------|
| `NewDynamicDawg` | `DynamicDawgKind` | byte, unicode-scalar, u64 | read, insert, remove, clear, compact |
| `NewDoubleArrayTrie` | `DoubleArrayTrieKind` | byte, unicode-scalar | read (immutable) |
| `NewScdawg` | `ScdawgKind` | byte, unicode-scalar | read, insert, substring |
| `CreatePersistentArtrie` / `OpenPersistentArtrie` | `PersistentArtrieKind` | byte, unicode-scalar, u64 | read, insert, remove, checkpoint |
| `CreatePersistentVocabulary` / `OpenPersistentVocabulary` | `PersistentVocabularyKind` | unicode-scalar | read, insert, checkpoint |

`Capabilities()` returns the `Can*` bitset; `Kind()` returns the backend
constant.

## Text domains and values

`string` terms are passed as UTF-8 bytes. The Unicode-scalar backends validate
UTF-8 and return an error with status `INVALID_UTF8` on invalid input; the byte
domain accepts arbitrary bytes. The u64 API (`PutU64`, `ContainsU64`, `GetU64`,
`RemoveU64`) takes `[]uint64` terms. Values are `*uint64` over the full
`0 .. math.MaxUint64` range; a `nil` value and a value of `0` are distinct.
`Lookup` reports `Found` and an optional `Value`.

## Error handling

Every fallible call returns an `error`. A native failure is an `*Error` whose
message is the thread-local `ldict_last_error_message()` and whose `Status`
field is the numeric `LdictStatus`. Operations a backend does not support return
`UNSUPPORTED`; a term submitted to the wrong unit domain returns
`DOMAIN_MISMATCH` (9).

## Retained resource handoff

`WithResource(func(context, vtable uintptr) error)` borrows the shared resource
for one synchronous retaining call. A liblevenshtein transducer built over the
dictionary retains it in constant time; a query it started keeps the immutable
revision visible at its start even after `Close`.

## Testing

```sh
go test ./bindings/go/...
```

with `CGO_LDFLAGS` and `LD_LIBRARY_PATH` set as above. See
[`libdictenstein_test.go`](libdictenstein_test.go).
