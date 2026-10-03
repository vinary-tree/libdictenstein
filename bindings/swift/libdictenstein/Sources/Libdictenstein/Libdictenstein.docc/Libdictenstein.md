# Libdictenstein

Build mutable, immutable, and persistent dictionaries for exact lookup, ordered traversal, substring search, and dictionary algebra.

## Overview

`Libdictenstein` is the SwiftPM facade for libdictenstein. A ``Dictionary``
owns one native backend. ``DynamicDAWG`` is mutable; ``DoubleArrayTrie`` is
built once; ``SCDAWG`` indexes substrings; ``PersistentARTrie`` can be opened
again from a filesystem path. `close()` releases a handle deterministically,
and `deinit` is a fallback. Native operations that can fail throw
``LibdictensteinError``.

```swift
import Libdictenstein

let left = try DynamicDAWG()
defer { left.close() }
try left.put("cat", value: 7)
try left.put("cot")       // Present, without a mapped value.

let right = try DynamicDAWG()
defer { right.close() }
try right.put("cut", value: 9)

let joined = try left.union(right)
defer { joined.close() }
let hit = try joined.get("cot")
assert(hit.found && hit.value == nil)

let snapshot = try joined.entries()
for entry in snapshot {
    print(entry.key, entry.value as Any)
}
```

``Lookup`` distinguishes absence (`found == false`) from a present valueless
term (`found == true`, `value == nil`); mapped zero is a third state. A
``EntrySnapshot`` copies keys and values out of one immutable captured
revision, so it remains valid after later mutation or `close()`. For a large
dictionary, ``EntryStream`` fetches bounded pages. Close or cancel it when
stopping early; `deinit` alone should not govern native resource lifetime.

Text (`String`), raw bytes (`[UInt8]`), and unsigned-64 token sequences
(`[UInt64]`) have distinct ``DictionaryEntryKey`` cases. Select the matching
`UnitDomain` at construction. ``EntriesInfo`` carries the stream's domain,
optional exact count, and optional producer/revision identity. The
``EntryBatchLimits`` bound page size and copied units/values.

``AlgebraOperation`` supports union, intersection, left difference, and
symmetric difference. The two inputs are captured independently; one native
ordered merge produces a new mutable ``DynamicDAWG``. On duplicate keys,
``ValueMerge`` chooses left, right, optional-value lattice join, or meet.
Union defaults to right-biased values and intersection to lattice meet.

``OptionalBackend`` identifies revision-8 PathMap and typed suffix-index
surfaces that this statically linked facade intentionally does not expose.
`require()` throws an explicit unsupported status; it never substitutes a
different backend. See the
[binding guide](https://github.com/vinary-tree/libdictenstein/blob/master/bindings/swift/README.md)
for native linking, persistence, and domain constraints.

## Topics

### Dictionary implementations and ownership

- ``Dictionary``
- ``DynamicDAWG``
- ``DoubleArrayTrie``
- ``SCDAWG``
- ``PersistentARTrie``
- ``LibdictensteinError``
- ``OptionalBackend``

### Entries and queries

- ``Lookup``
- ``DictionaryEntry``
- ``DictionaryEntryKey``
- ``EntrySnapshot``
- ``EntryStream``
- ``EntriesInfo``
- ``EntryBatchLimits``

### Algebra

- ``AlgebraOperation``
- ``ValueMerge``
