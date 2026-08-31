# Character-node V2/V3 wire-format compatibility

**Navigation**: [↑ Persistence architecture](README.md) · [Families](families.md) · [Durability & recovery](durability-and-recovery.md) · [Formal-verification map](formal-verification-map.md)

This document defines the compatibility contract for persistent character ART
node records. It covers the four node representations (`CharNode4`,
`CharNode16`, `CharNode48`, and `CharBucket`) in fixed, relative, and sequential
encoding modes.

## Writer and reader contract

| Producer | Fixed | Relative | Sequential |
|----------|-------|----------|------------|
| Baseline writer at `6a1b267a60fe9c445a0c8c7c8136e6dd40aedbf5` | V2 | V2 | V2 |
| Current writer | V2 | V3 | V3 |

| Consumer | Baseline V2 records | Current fixed V2 | Current compact V3 |
|----------|---------------------|------------------|--------------------|
| Baseline reader | Accepts | Accepts | Rejects before payload interpretation with `UnsupportedVersion { max_supported: 2, found: 3 }` |
| Current reader | Accepts | Accepts | Accepts |

Keeping fixed-width output at V2 preserves exact old-reader compatibility.
Relative and sequential records use V3 because their compact child references
must preserve each child's exact character-node type. A V3 file therefore
requires a V3-capable reader; this is a deliberate version boundary, not a
silent reinterpretation of V2 bytes.

## Common 16-byte header

All integer fields are little-endian.

| Offset | Width | Field | Contract |
|--------|-------|-------|----------|
| 0 | 4 | magic | `ARC\0` |
| 4 | 1 | version | `2` or `3` |
| 5 | 1 | node type | `104` (`Node4`), `116` (`Node16`), `148` (`Node48`), or `101` (`Bucket`) |
| 6 | 1 | flags | Runtime and encoding flags described below |
| 7 | 1 | V2 reserved / V3 type extension low byte | Must be zero in V2 |
| 8 | 2 | child count | Must fit the selected representation |
| 10 | 1 | compressed-prefix length | At most `CHAR_MAX_PREFIX_LEN` |
| 11 | 1 | V2 padding / V3 type extension high byte | Must be zero in V2 |
| 12 | 4 | payload size | Exact type-specific payload length |

Runtime flags occupy bits `0x01` (`IS_FINAL`), `0x02` (`IS_DIRTY`), and
`0x04` (`IS_LEAF`). Encoding flags are `0x80` (relative offsets), `0x40`
(sequential siblings), and, in V3 only, `0x20` (homogeneous unresolved child
types). Every other flag bit is invalid. Sequential and homogeneous modes both
require relative encoding, sequential mode requires at least one child, and V3
always requires relative encoding.

## V3 child locations and types

Each child type has a two-bit code:

| Code | Type |
|------|------|
| `00` | `CharNode4` |
| `01` | `CharNode16` |
| `10` | `CharNode48` |
| `11` | `CharBucket` |

Same-arena children use the established parent-relative location codec. A
cross-arena reference uses nine bytes: a one-byte odd tag (`1`, `3`, `5`, or
`7`) that carries the type code, followed by the arena and slot identifiers.
The sequential layout stores only the first location and derives the remaining
contiguous slots.

Types not already present in cross-arena tags are packed in encounter order.
The first eight two-bit codes use the two header extension bytes, which were
already paid padding in V2. Remaining codes use four children per payload byte.
When all unresolved children have one type and more than eight codes would be
needed, `0x20` stores that one code in the header and emits no type payload.
Unused header and payload bits must be zero; the reader rejects noncanonical
encodings.

The worst additional storage over the type-erasing relative V2 layout is
`ceil(max(unresolved_children - 8, 0) / 4)` bytes per heterogeneous record.
Cross-arena type tags and the homogeneous representation add no bytes. Fixed
records remain byte-for-byte V2. The 12-case executable corpus totals 3,393
bytes for the baseline writer and 3,401 bytes for the current writer: eight
bytes of total overhead while preserving exact types across all compact cases.

## Fail-closed decoding

The reader rejects bad magic, versions newer than V3, unknown node types or
flags, nonzero V2 reserved bytes, invalid flag combinations, impossible child
counts, oversized prefixes, noncanonical sizes, truncated location/type data,
relative underflow, persistent-address overflow, and nonzero unused type bits.
Validation precedes construction of persistent child pointers. A malformed
record cannot be treated as another version or node representation.

## Durable publication and migration

A checkpoint writes the complete candidate arena and validates its checksum
before atomically publishing the root descriptor. Crash recovery opens only a
committed descriptor; it never treats an uncommitted V3 arena as current. V2
roots remain readable by the current implementation. Rewriting a V2 file emits
V2 fixed records and V3 compact records under the normal checkpoint publication
protocol, so migration is explicit through a newly committed root.

The publication invariant is modeled by `CharV3ArenaPublication.tla` and its
unsafe controls. `CharV3TypeEncodingSpec.v` proves the complete writer × reader
× mode × node-kind compatibility matrix. Crash/reopen behavior is exercised by
`tests/char_v3_crash_reopen_correspondence.rs`.

Serialization, deserialization, and fault traversal use explicit worklists.
The library imposes no traversal-depth ceiling and does not tune thread stack
size; consumers may enforce their own resource policy. The ordinary-stack
100,000-character regression covers production compressed serialization,
source destruction, reload, fault traversal, and exact terminal value recovery.

## Executable compatibility corpus

The immutable corpus contains every combination of four node kinds and three
encoding modes, including same-arena and cross-arena locations, heterogeneous
types, and homogeneous packed types:

| Artifact | SHA-256 | Bytes |
|----------|---------|-------|
| `tests/fixtures/char-node-format/baseline-v2-6a1b267.txt` | `344376ee2753b70cdb4825c09fb5a5980c27a315fdfee77f5127d52585bc6e49` | 3,393 |
| `tests/fixtures/char-node-format/current-writer.txt` | `5bf013663732a2c3eee4c91e16b93642a2fa3446134034f925332f42c361b8c4` | 3,401 |

`scripts/verify-char-node-format-compatibility.sh` archives the exact baseline
commit under `target/`, regenerates both corpora with their actual writers,
compares every byte and digest, and exercises both reader directions. It binds
`vinary-tree-interop` to exact `=4.0.0-rc.2` at commit
`6694ad4fcb5ce498f69b77cb14ce1ea7a2f20033`. Scratch source and build artifacts
are removed on success or failure; the checked-in fixtures and manifest are the
durable evidence.
