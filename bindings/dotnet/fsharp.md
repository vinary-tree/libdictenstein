# Use libdictenstein from F#

F# consumes the `Libdictenstein` NuGet package's public .NET assembly. The
same `VinaryTree.Libdictenstein` types are used from C#; there is no separate
F# wrapper package. Native dictionary handles follow F#'s lexical `use`
ownership convention.

```fsharp
module VinaryTree.Libdictenstein.FSharpUsage.Search

open VinaryTree.Libdictenstein

let containsEither () : bool =
    use left = new DynamicDawg()
    left.Put("cat") |> ignore
    use right = new DynamicDawg()
    right.Put("cot") |> ignore
    use combined = left.Union(right)
    combined.Contains("cat") && combined.Contains("cot")
```

`Union` creates a new mutable dictionary from two captured input revisions.
`use` closes all three handles on both normal and exceptional exits; a
materialized `Snapshot()` remains host-owned afterward. For a large
dictionary, use `StreamEntries` and dispose the stream when stopping early.
`Get` returns a `Lookup` whose `Found` field distinguishes absence from a
present key with no mapped value.

The [versioned .NET API reference](../../docs/api/dotnet/index.md) covers
backends, domains, algebra, and ownership. The
[.NET guide](README.md) explains native loading and persistence. The
[compiled source](tests/VinaryTree.Libdictenstein.FSharpUsage/Search.fs)
is checked against both .NET 8 and .NET 10 by CI.
