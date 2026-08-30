# High-performance dictionaries that feel native in Julia

`Libdictenstein` binds the shared, versioned `libdictenstein` C application
binary interface (ABI). It combines four properties that are usually split
across unrelated containers:

1. compact exact dictionaries and trie-maps;
2. snapshot-consistent traversal under concurrent mutation;
3. byte, Unicode-scalar, and unsigned 64-bit token domains; and
4. native set algebra whose output remains a mutable dictionary.

An **owned handle** is a native pointer for which Julia owes exactly one
`ldict_dictionary_free`. A **snapshot** is an immutable retained revision that
can outlive its source handle. A **valueless entry** is present in the key set
but maps to `nothing`; it is different from an absent key, which makes
`getindex` throw `KeyError`.

## Choose a backend

| Constructor | Best fit | Mutation | Special capability |
|---|---|---:|---|
| `DynamicDawg` | general exact dictionary | yes | minimal graph, all key domains |
| `DoubleArrayTrie` | read-mostly text lexicon | no | dense array traversal |
| `Scdawg` | exact terms plus factor search | yes | substring membership/frequency |
| `PersistentARTrie` | durable large dictionary | yes | checkpoint and reopen |
| `PersistentVocabulary` | durable term/index vocabulary | append | reverse index lookup |

## Collection semantics

```julia
julia> d = DynamicDawg();

julia> d["alpha"] = UInt64(1); d["beta"] = nothing;

julia> haskey(d, "beta") && d["beta"] === nothing
true

julia> sort!(collect(keys(d)))
2-element Vector{String}:
 "alpha"
 "beta"

julia> close(d)
```

Iteration opens a native immutable entry cursor. Batches are bounded, copied
into Julia-owned keys, and released before iteration advances. Consequently,
the iterator observes one coherent revision even while writers publish later
revisions.

## Algebra and value semantics

For input key sets `A` and `B`, the four operations produce:

```math
A \cup B,\qquad A \cap B,\qquad A \setminus B,\qquad
(A \setminus B) \cup (B \setminus A).
```

Keys present in both inputs use one value policy. `FIRST` and `LAST` choose an
operand. `LATTICE_JOIN` treats `nothing` as the bottom optional value and takes
the numeric maximum when both values exist. `LATTICE_MEET` returns a value only
when both exist and then takes their numeric minimum.

The native algorithm is deliberately literate:

```text
ALGORITHM MergeSnapshots(left, right, operation, value_policy)
  CAPTURE one immutable, lexicographically ordered cursor from each input
  WHILE either cursor has an entry
    COMPARE the two current keys
    EMIT the lower key exactly when the selected set operation retains it
    WHEN keys are equal
      EMIT at most one key and combine values with value_policy
    ADVANCE only the cursor or cursors consumed by that decision
  FREEZE the emitted sorted stream once into a minimal DynamicDAWG
  RETURN the independently owned mutable result
```

This takes linear merge time `O(|A| + |B|)` plus the linear minimal-graph
builder. It uses `O(|result|)` owned result storage and does not construct a
host-language `Dict`.

## Ownership and concurrency

```julia
dictionary = DynamicDawg()
try
    dictionary["before"] = 1
    frozen = snapshot(dictionary)
    try
        dictionary["after"] = 2
        @assert haskey(Dict(frozen), "before")
        @assert !haskey(Dict(frozen), "after")
    finally
        close(frozen)
    end
finally
    close(dictionary)
end
```

The producer retains immutable graph revisions with atomic publication. Read
operations and snapshots are safe on Julia tasks and threads; a callback never
enters Julia from an unowned native thread. Closing a handle concurrently with
another operation on that same handle is a caller error, as it is for Julia IO
objects.

## Performance and security

- Prefer `insert_batch!` to amortize the FFI boundary and activate the
  freeze-once sorted builder on an empty DynamicDAWG.
- Keep byte keys as `Vector{UInt8}` and token keys as `Vector{UInt64}`; implicit
  string coercion would change their domains.
- Native errors are copied immediately into `NativeError`, because the C
  diagnostic buffer is thread-local and replaced by the next ABI call.
- Paths are passed as UTF-8 bytes to the persistent constructors. Apply the
  same filesystem authorization and sandboxing policy as native Julia code.
- Artifact builds must pin the ABI version and require an API revision of at
  least `6`; a later additive revision remains compatible.

The finite-map algebra follows the library's `llattice` optional-value laws.
For the underlying ordered-automaton construction, see Daciuk et al.,
“Incremental Construction of Minimal Acyclic Finite-State Automata,”
[doi:10.1162/089120100561601](https://doi.org/10.1162/089120100561601).
