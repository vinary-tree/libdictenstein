# Variable-width logical profiles

Libdictenstein separates a dictionary's topology from the representation of its
logical edge labels. A profile defines the logical atom type, its canonical
encoding, its stable identity, and the boundary at which encoded data becomes
visible to callers.

## Logical atoms and physical bytes

The dictionary core traverses logical atoms. Encoded bytes are an implementation
detail of byte-backed profiles and must not be reported as additional semantic
transitions. A UTF-8 profile therefore represents one Unicode scalar per logical
atom even though its canonical codeword occupies one to four bytes. A ULEB128
profile represents one arbitrary-width unsigned integer per logical atom; its
continuation bytes are never independent symbols.

This distinction is important for Levenshtein, llattice, and language-level
consumers: edit distance, prefix traversal, zippers, and suffix operations must
operate on logical atoms, not codec bytes.

## Stable profile identity

`AtomProfile` supplies a compile-time identity consisting of a canonical name,
version, logical `ProfileKind`, and optional fixed wire width. Persisted
descriptors must retain the name and version and reject unknown, mismatched, or
non-canonical combinations. A missing descriptor is not permission to guess a
codec from the Rust type or from the first key.

The built-in profiles are:

| Profile | Logical atom | Encoding | Width |
| --- | --- | --- | --- |
| `Bytes` | `u8` | raw byte | 1 |
| `UnicodeScalar` | `char` | scalar value | 4 |
| `Utf8` | `char` | canonical UTF-8 codeword | variable |
| `U32` | `u32` | native fixed-width unit | 4 |
| `U64` | `u64` | native fixed-width unit | 8 |
| `F64Bits` | `u64` bit pattern | native fixed-width unit | 8 |
| `Uleb128` | arbitrary-width unsigned integer | canonical ULEB128 | variable |

Canonical variable-width encodings are non-empty, unambiguous, and validated
before insertion or traversal. Malformed, truncated, non-canonical, and
overlong codewords are rejected rather than interpreted as a partial result.

## Topology and naming

`DictionaryFamily` identifies the storage topology (`DynamicDawg`,
`DoubleArrayTrie`, `PathMap`, `SuffixAutomaton`, `Scdawg`, or a persistent
ARTrie family). `BackendProfileDescriptor` identifies the logical profile. The
two axes are intentionally independent; `DictionarySpec<P>` and
`ProfiledDictionaryContainer<P>` bind them when a user-facing typed boundary
is desired.

Legacy names such as `DynamicDawgChar` remain source-compatible. New code can
use the generic core (`DynamicDawgGeneric<U, V>`) and provide a profile witness
with `profile_descriptor::<P>()`. Specialized UTF-8 and ULEB128 wrappers are
appropriate when they improve ergonomics or enforce validation at the boundary.

## Internalization and vocabulary IDs

Interned dictionaries map logical atoms to dense local IDs and store sequences of
those IDs. The mapping, profile descriptor, vocabulary generation, and snapshot
identity travel together. IDs are capsule-local compression handles; they are
not stable public identities and must never be interpreted without the matching
vocabulary metadata.

## Consumer contract

Consumers should request logical traversal APIs or profile-aware zippers. They
must not inspect physical codec nodes as semantic transitions. A serialized
dictionary is readable only when its topology, profile identity, and vocabulary
metadata validate together. This keeps backend substitution observationally
equivalent even when one implementation stores bytes and another stores native
logical units.
