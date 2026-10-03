# libdictenstein for .NET

High-performance dictionaries and trie-maps for approximate string matching.
`Libdictenstein` is the native-backed C# and F# package; both languages use
the same `VinaryTree.Libdictenstein` assembly, and no separate F# wrapper is
required. The package supports .NET 8 and .NET 10 and carries platform
native libraries for its declared runtime identifiers.

```csharp
using VinaryTree.Libdictenstein;

using var left = new DynamicDawg();
left.Put("cat", 7);
left.Put("cot");                         // Present without a mapped value.

using var right = new DynamicDawg();
right.Put("cut", 9);
using var combined = left.Union(right);

Lookup result = combined.Get("cot");
Console.WriteLine(result.Found && result.Value is null);
foreach (var entry in combined.Snapshot())
    Console.WriteLine(entry.Key);
```

The `Lookup` record distinguishes absence, valueless membership, and value
zero. `Snapshot()` owns its copied entries and remains valid after dictionary
mutation or disposal; `StreamEntries()` instead traverses bounded native
batches and must be disposed if stopped early. Use C# `using` or F# `use` for
each native owner, including a union result. Union, intersection, left
difference, and symmetric difference capture immutable input revisions and
return an independent mutable `DynamicDawg`.

Select `UnitDomain.UnicodeScalar`, `.Byte`, or `.U64` when constructing a
backend; text, raw bytes, and unsigned-64 token sequences are distinct.
`DynamicDawg` supports mutable exact terms, `DoubleArrayTrie` is static,
`Scdawg` supports substring queries, and persistent ARTrie/vocabulary
backends can checkpoint and reopen. Revision-8 PathMap and typed suffix-index
facades are deliberately unsupported here; `OptionalBackends.Require` fails
explicitly instead of silently changing the backend.

The [complete C# API and native-loading guide](https://github.com/vinary-tree/libdictenstein/blob/v4.0.0-rc.6-release.1/bindings/dotnet/README.md),
[compiled F# example](https://github.com/vinary-tree/libdictenstein/blob/v4.0.0-rc.6-release.1/bindings/dotnet/fsharp.md),
and [versioned .NET API reference](https://vinary-tree.github.io/libdictenstein/4.0.0-rc.6/dotnet/)
cover ownership, statuses, capabilities, and common operations. The versioned
reference is released only after its exact-byte public readback passes; it
must not be treated as live merely because this source candidate exists.
