# libdictenstein .NET API — 4.0.0-rc.6

The `Libdictenstein` NuGet package provides high-performance mutable and
immutable dictionaries, trie-maps, persistent indexes, and exact key-set
algebra through the `VinaryTree.Libdictenstein` namespace. It exports a
versioned `VinaryTree.Interop.IDictionaryResource` that a consumer such as
liblevenshtein can retain without copying every term.

## Construct, search, and combine dictionaries

```csharp
using VinaryTree.Libdictenstein;

using var left = new DynamicDawg();
left.Put("cat", 7);
left.Put("cot");                 // Present with no mapped value.

using var right = new DynamicDawg();
right.Put("cut", 9);

using var union = left.Union(right);
Lookup found = union.Get("cot");
Console.WriteLine(found.Found && found.Value is null);

var snapshot = union.Snapshot();
foreach (var entry in snapshot)
    Console.WriteLine(entry.Key);
```

`Union` performs one native ordered merge over immutable input revisions and
returns an independent mutable `DynamicDawg`. `Intersection`, `Difference`,
and `SymmetricDifference` implement the other exact key-set operations;
`ValueMerge` chooses how mapped values on duplicate keys combine. The
snapshot's keys and values are host-owned and survive later mutations and
closure of all three dictionaries. For large datasets, `StreamEntries`
provides bounded, closeable native-backed traversal without materializing
every entry.

`Lookup.Found` distinguishes an absent key from a present key whose
`Value` is `null`. Value zero is a third, distinct state. The Unicode text,
raw-byte, and unsigned-64 token domains remain separate; pass a compatible
`UnitDomain` at construction and use the matching overloads.

## Choose a backend and manage ownership

`DynamicDawg` supports insertion, removal, clear, and compaction.
`DoubleArrayTrie` is immutable after one batched construction. `Scdawg`
supports substring membership and frequency. `PersistentArtrie` and
`PersistentVocabulary` provide filesystem-backed durability with explicit
`Checkpoint`. Check `Kind` and `Capabilities` at runtime when a backend is
selected dynamically. Revision-8 PathMap and typed suffix-index native
backends are not mediated by this .NET facade: `OptionalBackends.Require`
throws status 6, rather than silently substituting another backend.

Use C# `using` or F# `use` for every native owner, including an algebra
result and a native entry stream. The
[compile-checked F# guide](https://github.com/vinary-tree/libdictenstein/blob/v4.0.0-rc.6-release.1/bindings/dotnet/fsharp.md)
shows the same public assembly from F#. For installation, native loading,
persistence, and status handling, see the
[.NET binding guide](https://github.com/vinary-tree/libdictenstein/blob/v4.0.0-rc.6-release.1/bindings/dotnet/README.md).
