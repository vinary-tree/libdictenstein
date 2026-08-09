# Vinary Tree libdictenstein for .NET

A `LibraryImport` (source-generated P/Invoke) facade over libdictenstein's
stable `ldict_*` C ABI. The NuGet package is `VinaryTree.Libdictenstein`; the
namespace is `VinaryTree.Libdictenstein`. It exposes DynamicDAWG CRUD and batch
insertion, immutable DoubleArrayTrie construction, SCDAWG substring search,
persistent ARTrie CRUD/checkpoint/reopen, and persistent vocabulary reverse
lookup.

## Native library

The interop layer imports the shared library named `libdictenstein` (resolved to
`liblibdictenstein.so` / `.dylib` / `libdictenstein.dll`). The published package
ships the native asset under `runtimes/<rid>/native`. For a source checkout,
build it and put it on the loader search path:

```sh
cargo build --release --no-default-features --features ffi
export LD_LIBRARY_PATH="$PWD/target/release:$LD_LIBRARY_PATH"
```

## Quickstart

```csharp
using VinaryTree.Libdictenstein;

using var dictionary = new DynamicDawg();          // Unicode-scalar by default
dictionary.PutAll(new Dictionary<string, ulong?>
{
    ["cat"] = 1,
    ["cot"] = 2,
    ["cut"] = null,                                 // valueless term
});

Lookup hit = dictionary.Get("cot");
Console.WriteLine($"{hit.Found} {hit.Value}");      // True 2

using var suffixes = new Scdawg();
suffixes.Put("cat");
suffixes.Put("cot");
Console.WriteLine(suffixes.ContainsSubstring("ot"));       // True
Console.WriteLine(suffixes.SubstringFrequency("t"));        // 2
```

`Dictionary` implements `IDisposable`; `using` (or `Dispose`) frees the native
handle exactly once.

## Backends and capabilities

| Type | Kind | Unit domains | Capabilities |
|------|------|--------------|--------------|
| `DynamicDawg` | `BackendKind.DynamicDawg` | byte, unicode-scalar, u64 | read, insert, remove, clear, compact |
| `DoubleArrayTrie` | `BackendKind.DoubleArrayTrie` | byte, unicode-scalar | read (immutable) |
| `Scdawg` | `BackendKind.Scdawg` | byte, unicode-scalar | read, insert, substring |
| `PersistentArtrie` | `BackendKind.PersistentArtrie` | byte, unicode-scalar, u64 | read, insert, remove, checkpoint |
| `PersistentVocabulary` | `BackendKind.PersistentVocabulary` | unicode-scalar | read, insert, checkpoint |

`Kind` and `Capabilities` report the runtime backend id and `LDICT_CAP_*`
bitset.

## Text domains and values

`string` terms are UTF-8 encoded; the Unicode-scalar backends validate UTF-8.
The u64 API takes `ReadOnlySpan<ulong>` terms. `Lookup` is a
`readonly record struct (bool Found, ulong? Value)`; a `null` value and a
mapped value of `0` are distinct across the whole `0 .. ulong.MaxValue` range.

## Error handling

Non-OK statuses throw `LibdictensteinException`, whose `StatusCode` is the
numeric `LdictStatus` and whose message is the thread-local
`ldict_last_error_message()`. Backend-unsupported operations surface as
`UNSUPPORTED`; wrong-domain terms surface as `DOMAIN_MISMATCH` (9).

## Retained resource handoff

`WithResource(...)` borrows the shared `VtResource` for one synchronous
retaining call. A liblevenshtein transducer retains the dictionary in constant
time and keeps its query-start revision alive after `Dispose`.

## Testing

```sh
dotnet test bindings/dotnet/tests/VinaryTree.Libdictenstein.Tests
```

with the native library on `LD_LIBRARY_PATH`. See
[`tests/VinaryTree.Libdictenstein.Tests/Program.cs`](tests/VinaryTree.Libdictenstein.Tests/Program.cs).
