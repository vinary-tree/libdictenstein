# API revision 8: PathMap dictionaries and typed suffix-source indexes

**Navigation**: [bindings corpus](README.md) · [C ABI reference](c-abi-reference.md) ·
[resource producer](resource-producer.md) · [Julia package](../../bindings/julia/README.md)

Revision 8 makes two different native abstractions available to foreign
languages. `PathMap` is an exact-key dictionary: it implements the existing
`LdictDictionary` CRUD and `vt.dictionary.v1` snapshot contract. A **suffix
source index** is a collection of insertion *records* and substring
occurrences: duplicate source texts are distinct, and an active substring is
not necessarily an inserted key. It therefore has separate
`LdictSuffixIndex` and `LdictSuffixSnapshot` handles and is **not** a
`LdictDictionary`, `AbstractDict`, or `vt.dictionary.v1` resource.

The stable ABI major remains `1`; the additive `LDICT_API_REVISION` is `8`.
Clients written against revision 7 may keep using their existing symbols.
Optional clients that load a revision-8 symbol dynamically must check
`ldict_api_revision() >= 8` *before resolving that symbol*. This matters for
eagerly bound FFI loaders. A binary directly linked to a revision-8 symbol,
such as the C example below, requires a revision-8 library at process load;
its runtime check cannot make a missing symbol loadable.
The [canonical model](../../bindings/api.json),
[header](../../include/libdictenstein.h), and generated
[Julia](../../bindings/generated/julia-abi-capabilities.tsv) and
[Raku](../../bindings/generated/raku-abi-capabilities.tsv) inventories are
the exact signature/layout sources; this guide explains their semantics.

## PathMap: exact keys, not UTF-8 coercion

```c
LdictStatus ldict_pathmap_new(uint32_t unit_domain,
                              LdictDictionary** out_dictionary);
```

Kind `LDICT_KIND_PATHMAP` is `6`. The supported domains are `BYTE` (`1`) and
`UNICODE_SCALAR` (`2`); `U64` (`3`) returns `UNSUPPORTED`. The byte form
accepts **all octets**, including NUL and invalid UTF-8. The scalar form
validates UTF-8 and traverses Unicode scalar labels. Optional values use the
same absent-versus-present-zero `LdictOptionalU64` semantics as other
`vt.dictionary.v1` producers. The exact capability bitset is `0x0F`
(`READ | INSERT | REMOVE | CLEAR`); `compact`, `checkpoint`, and the SCDAWG
substring operations are unsupported on this dictionary backend.

The constructor is always exported from an `ffi` build. Without the separate
`pathmap-backend` feature it returns `UNSUPPORTED` and sets a non-null
`*out_dictionary` to `NULL`; it never silently falls back to another engine.
The official native release/conformance builds explicitly enable
`ffi,pathmap-backend`. `ffi` alone does **not** imply PathMap. An unknown
domain returns `INVALID_ARGUMENT`; null output returns `NULL_POINTER`;
allocation panic is contained as `PANIC`. Every successful handle is freed by
`ldict_dictionary_free`.

PathMap's batch call validates and copies the complete descriptor array
before one native batch publication. An invalid entry publishes none of the
batch and initializes `out_inserted` to zero. Ordinary single-key mutation
has the same snapshot isolation as other dictionary backends: a captured
resource revision retains its original keys and values after later mutation,
clear, or handle free. A consumer borrows a two-word resource from the handle,
retains it before storing it, and releases each retain exactly once. See the
[family resource lifecycle](resource-producer.md).

## Suffix-index model

A suffix index stores active source records $`(s_i, v_i)`$, where $`s_i`$ is
valid UTF-8 text and $`v_i \in \{\varnothing\} \cup \mathrm{UInt64}`$.
Inserting the same $`s_i`$ twice creates two records, even if their values
differ. Removing a text removes its **oldest active** matching record once.
`contains_source` means exact active record membership; `contains_substring`
means an occurrence in any active source. Graph edges left by a removal never
make a removed source count as active.

For a nonempty pattern, `substring_frequency` sums **overlapping**
occurrences across every active record. For an empty pattern, each source of
length $`n`$ transition units contributes $`n+1`$ boundaries. Byte mode
measures UTF-8 *bytes*; scalar mode measures Unicode scalar values. For the
records `"é🙂é"` and `""`, the empty-pattern frequency is `10` in byte
mode and `5` in scalar mode. Both modes still require valid UTF-8 input;
byte-transition mode is **not** arbitrary-byte source ingestion. `U64`
sources and persistence are not exposed by this API.

The active-record page is ordered lexicographically by source text, then by
source ID for equal texts. A source ID is stable within a captured revision
and across ordinary insert/remove/compaction, but `clear` may reuse it.
Producer and revision are process-local. The two suffix and dictionary
producer allocators can issue the same number: a cache key must include the
**resource family** as well as producer, revision, and source ID. Never feed a
suffix pair into project-neutral `VtSnapshotIdentity` or merge caches solely
because their numeric identities match.

## Typed C operations and failure boundaries

These functions operate only on typed suffix handles. `OK` means success;
`END` means a page offset is at or beyond the captured record count.
All fallible calls catch a native panic and return `PANIC` with a thread-local
diagnostic. A null data pointer is valid when length is zero; otherwise it
returns `NULL_POINTER`. Malformed UTF-8 returns `INVALID_UTF8`. Scalar outputs
that are non-null are initialized to zero before fallible validation;
constructors initialize handle outputs to `NULL`.

| Operation | Meaning | Additional statuses and ownership |
|---|---|---|
| `ldict_suffix_index_new(domain, out)` | New byte/scalar index | `INVALID_ARGUMENT` unknown domain; `UNSUPPORTED` for U64; caller owns `out` on `OK` |
| `ldict_suffix_index_insert_text(index, data, len, value)` | Append one record | `INVALID_ARGUMENT` for malformed optional value; input copied before return |
| `ldict_suffix_index_remove_text(index, data, len, out_removed)` | Remove oldest active exact text once | `out_removed` is `0` or `1` on `OK`; zeroed on other statuses |
| `ldict_suffix_index_clear(index)` | Publish an empty active set | Preserves producer, advances revision when state changes; old snapshots remain valid |
| `ldict_suffix_index_compact(index)` | Compact graph if needed | Returns only `LdictStatus`, **not** reclaimed-node count |
| `ldict_suffix_index_snapshot(index, out)` | Retain one active-source revision | $`\mathcal{O}(1)`$ root capture; snapshot may outlive index close/free |
| `ldict_suffix_snapshot_identity(snapshot, out_producer, out_revision)` | Type-scoped identity | Both outputs start at zero; not interchangeable with dictionary identity |
| `ldict_suffix_snapshot_source_count(snapshot, out_count)` | Count active records, including duplicates | `LIMIT_EXCEEDED` only if count cannot fit `uint64_t` |
| `ldict_suffix_snapshot_contains_source(snapshot, data, len, out_contains)` | Exact active source membership | `out_contains` is `uint64_t` zero/one |
| `ldict_suffix_snapshot_contains_substring(snapshot, data, len, out_contains)` | Active substring membership | `out_contains` is `uint64_t` zero/one |
| `ldict_suffix_snapshot_substring_frequency(snapshot, data, len, out_frequency)` | Sum overlapping occurrences | Empty pattern counts unit boundaries; `LIMIT_EXCEEDED` if result cannot fit `uint64_t` |
| `ldict_suffix_snapshot_source_page(snapshot, offset, records, capacity, out_written, out_total)` | Copy bounded descriptors, **borrow** text | `END` at/beyond total; zero capacity with null records queries total; outputs/descriptors initialized before validation |
| `ldict_suffix_{index,snapshot}_close(handle)` | Release state, keep allocated opaque pointer | First call `OK`; later operations and close return `CLOSED` |
| `ldict_suffix_{index,snapshot}_free(handle)` | Deallocate opaque pointer | `void`; null is harmless; **any subsequent use of the freed pointer is undefined behavior** |

Every non-null handle must be a valid handle of the corresponding suffix type;
all caller-provided buffers must be valid for the declared lengths. Each
operation on an allocated-but-closed handle returns `CLOSED` (except `free`).
`close` and operations may race in the sense that an operation which already
loaded an owned state may finish; callers must **not** race close/free with
use of a borrowed page pointer. A page's descriptor array belongs to the
caller, but `record.data` points into the retained immutable snapshot and is
valid only until that snapshot is closed/freed. Copy text when host values
must outlive it. Index mutation and independent snapshot reads are safe to
run concurrently; source pages do not invoke one FFI call per record.

Mutation uses the native immutable-root publication path; snapshot capture is
constant-time in source length, while a page costs time proportional to the
number of descriptors copied (text bytes are not copied). Exact source lookup
uses the sorted active index; substring frequency currently scans active
source text and should not be advertised as a constant-time suffix-graph
operation. Host facades may add their own copied-text cost. The C boundary
copies input before return, catches panics, and never treats arbitrary pointers
or lengths as trusted; as with every C API, invalid *non-null* pointers and
concurrent use-after-free remain caller violations, not recoverable statuses.

## Minimal C and Julia usage

The directly linked C example requires a revision-8 library at process load.
It uses a captured revision after closing its producer. It must
copy borrowed page text before closing the snapshot if it needs longer-lived
host data.

```c
#include "libdictenstein.h"
#include <assert.h>
#include <stddef.h>
#include <stdint.h>

int main(void) {
    assert(ldict_abi_version() == 1 && ldict_api_revision() >= 8);
    LdictSuffixIndex* index = NULL;
    assert(ldict_suffix_index_new(VT_UNIT_DOMAIN_BYTE, &index) == LDICT_STATUS_OK);
    LdictOptionalU64 zero = {0, 1, {0}};
    assert(ldict_suffix_index_insert_text(index, (const uint8_t*)"aba", 3, zero)
           == LDICT_STATUS_OK);
    LdictSuffixSnapshot* view = NULL;
    assert(ldict_suffix_index_snapshot(index, &view) == LDICT_STATUS_OK);
    assert(ldict_suffix_index_close(index) == LDICT_STATUS_OK);
    ldict_suffix_index_free(index);
    uint64_t frequency = 0;
    assert(ldict_suffix_snapshot_substring_frequency(
        view, (const uint8_t*)"a", 1, &frequency) == LDICT_STATUS_OK);
    assert(frequency == 2);
    assert(ldict_suffix_snapshot_close(view) == LDICT_STATUS_OK);
    ldict_suffix_snapshot_free(view);
    return 0;
}
```

The Julia facade preserves the distinction between a standard dictionary and
a source index. `source_records` copies text in bounded native pages before
returning host-owned `String` values.

```julia
using Libdictenstein

dictionary = PathMap(UNIT_BYTE)
dictionary[UInt8[0x00, 0xff]] = UInt64(0)
@assert dictionary[UInt8[0x00, 0xff]] == 0
close(dictionary)

index = SuffixIndex(UNIT_UNICODE_SCALAR)
insert_source!(index, "é🙂é", 7)
view = source_snapshot(index)
try
    @assert contains_source(view, "é🙂é")
    @assert substring_frequency(view, "é") == 2
    @assert only(source_records(view)).value == 7
finally
    close(view)
    close(index)
end
```

## Packaging and foreign-language contract

The release cdylib and language-package native build enable
`--features ffi,pathmap-backend`; feature-off builds remain a tested negative
control. Julia exposes both backends with a revision gate before resolving
new symbols. C sees the header directly. Raku exposes PathMap as an idiomatic
`Dictionary` with the same pre-call revision gate; its generated NativeCall
layer declares typed suffix symbols, but its `suffix-index` facade rejects
them explicitly until record and snapshot lifetimes are qualified. Other
foreign facades must either add a native PathMap dictionary and typed suffix
index with the above lifetimes, or fail explicitly rather than silently
selecting an older backend. The exact per-language gate status is tracked by
the [capability matrix](revision8-capability-matrix.md); the
[binding model](../../bindings/api.json) instead governs symbols, constants,
and layouts. **A generated symbol declaration is not
evidence of an idiomatic high-level facade**. No RC.6 artifact is published by
this source change.
