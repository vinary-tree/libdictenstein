# Libdictenstein.jl

High-performance dictionaries and trie-maps for approximate string matching,
with Julia-native collection semantics and snapshot-safe algebra.

`Libdictenstein` owns native DynamicDAWG, DoubleArrayTrie, SCDAWG, and persistent
ARTrie handles. Every dictionary is an `AbstractDict`, so ordinary Julia code
can call `haskey`, `getindex`, `setindex!`, `delete!`, `keys`, `values`, `merge`,
`intersect`, `setdiff`, and `close` without learning a parallel container API.

## Install and load

The release artifact supplies the native library. A source checkout can point
at a locally built library:

```sh
export LIBDICTENSTEIN_LIBRARY="$PWD/target/release/liblibdictenstein.so"
```

```julia
using Libdictenstein

dictionary = DynamicDawg()
try
    dictionary["colour"] = UInt64(17)
    dictionary["color"] = nothing       # present, intentionally valueless
    @assert dictionary["colour"] == 17
    @assert haskey(dictionary, "color")
finally
    close(dictionary)
end
```

Use `do`/`try`–`finally` around long-lived dictionaries. A finalizer protects
abandoned objects, but deterministic `close` keeps native memory pressure
independent of Julia garbage-collection timing.

## Fast algebra

```julia
left = DynamicDawg()
right = DynamicDawg()
try
    left["shared"] = 4
    right["shared"] = 9
    joined = algebra(left, right, ALGEBRA_UNION, VALUE_MERGE_LATTICE_JOIN)
    try
        @assert joined["shared"] == 9
    finally
        close(joined)
    end
finally
    close(left)
    close(right)
end
```

The native engine captures one immutable revision from each input, performs a
linear lexicographic merge, and freezes the sorted result directly into a
minimal mutable DynamicDAWG. No Julia hash table or per-key FFI loop is used.

See the [full guide](docs/src/index.md) for domains, snapshots, persistence,
ownership, algebraic value policies, performance, and security boundaries.
