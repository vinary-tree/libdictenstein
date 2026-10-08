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

For already ordered input, `SortedMinimalDawg` validates nondecreasing keys
and feeds one batch to the native freeze-once minimal-graph builder. The result
is still a mutable `DynamicDawg`, not a separate backend:

```julia
ordered = SortedMinimalDawg(["ant" => 1, "bee" => nothing, "bee" => 2])
try
    @assert ordered["bee"] == 2 # last duplicate value wins
finally
    close(ordered)
end
```

Supply `domain=UNIT_BYTE` for byte-vector keys or `domain=UNIT_U64` for
unsigned token vectors. Out-of-order input raises `ArgumentError` before a
native handle is created. Calling `insert_batch!` once on an empty
`DynamicDawg` with ordered entries reaches the same native fast path; this
constructor adds validation and may allocate more than that direct call. Its
advantage is over building the dictionary by repeated individual insertions.

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

For a result that can be consumed without building a new dictionary, use
`algebra_entries(left, right; operation=ALGEBRA_UNION, page_size=256)`. It owns
both captured revisions, yields Julia-owned `Pair`s in lexicographic order,
and closes automatically at exhaustion. `prefix_entries(dictionary, prefix)`
uses the same bounded native cursor; `fold_entries(f, initial, stream)` sends
pages through the native reducer and closes the stream even if `f` throws.
Call `close(stream)` when stopping iteration early.

`lookup_batch(dictionary, keys)` returns one `(found, value)` tuple per key;
`remove_batch!(dictionary, keys)` returns one Boolean per key. Both preserve
input order and duplicates. A present valueless key is `(true, nothing)`,
distinct from `(false, nothing)` for an absent key. `insert_batch!` remains the
single-call bulk mutation path.

See the [full guide](docs/src/index.md) for domains, snapshots, persistence,
ownership, algebraic value policies, performance, and security boundaries.
The [live development guide](https://vinary-tree.github.io/libdictenstein/dev/)
documents the current source branch; it is not a claim of RC.6 registry publication.

## ABI maintenance

The package does not maintain a second handwritten native ABI. Its constants,
layouts, and typed call wrappers are generated from the repository's
authoritative `bindings/api.json`, checked exactly against the public C header,
and covered by freshness and mutation-based negative controls. Maintainers
should follow the [full Julia binding guide](../README.md#generated-abi-boundary)
before changing a native signature.
