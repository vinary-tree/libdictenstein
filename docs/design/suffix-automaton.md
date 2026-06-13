# Persistent Suffix Index Design

This document describes the current suffix-index family. It is no longer a
proposal for adding suffix automata; byte and Unicode suffix automata, suffix
trees, SCDAWGs, and their persistent counterparts are implemented.

## Implemented Families

| Family | In-memory | Persistent |
|--------|-----------|------------|
| Suffix automaton | `SuffixAutomaton`, `SuffixAutomatonChar` | `PersistentSuffixAutomaton`, `PersistentSuffixAutomatonChar` |
| Suffix-tree-compatible API | internal/native compact graph | `PersistentSuffixTree`, `PersistentSuffixTreeChar` |
| Symmetric compact DAWG | `Scdawg`, `ScdawgChar` | `PersistentScdawg`, `PersistentScdawgChar` |

All variants are selected by unit type:

- byte variants use `u8` units
- `Char` variants use Unicode scalar values

## Persistent Representation

Persistent suffix indexes are native suffix graphs, not ARTrie-encoded suffix
key spaces.

```text
operation WAL
    -> active source/term records
    -> rebuild native graph revision
    -> publish immutable snapshot
    -> checkpoint native graph image
```

The persistent suffix automaton stores suffix automaton graph nodes. The
persistent suffix tree stores a path-compressed suffix-tree-compatible graph.
The persistent SCDAWG stores a compact SCDAWG graph with substring location and
left/right extension support.

## Concurrency

Reads traverse immutable snapshots and do not take the writer lock. Writes are
serialized because each mutation rebuilds and publishes a graph revision. This
is intentionally different from the persistent ARTrie overlay family, whose
write publication is lock-free CAS.

## API Shape

The persistent suffix graph variants expose:

- `new`, `create`, `open`, `open_with_recovery`
- `insert`, `insert_with_value`, `remove`, `clear`, `compact`
- `checkpoint`, `close`
- `Dictionary`, `MappedDictionary`, `MutableDictionary`, and
  `MutableMappedDictionary`
- `SubstringDictionary` for suffix-tree and SCDAWG variants

Suffix-tree and SCDAWG variants also expose `find`, `freq_at`, `locations`, and
`locations_at` helpers over graph handles.

## Choosing A Persistent Suffix Index

Use `PersistentSuffixAutomaton` when you want the suffix automaton model and
general substring acceptance over durable text.

Use `PersistentSuffixTree` when callers need suffix-tree-style handles,
frequency at a matched node, or location lookup from a path-compressed graph.

Use `PersistentScdawg` when compact SCDAWG structure and bidirectional
substring metadata are the better fit.

Choose `Char` variants when Unicode scalar semantics matter. Choose byte
variants for byte protocols, ASCII-heavy data, or smallest edge labels.

## Example

```rust,no_run
use libdictenstein::persistent_artrie::PersistentScdawgChar;

# fn main() -> Result<(), Box<dyn std::error::Error>> {
let index = PersistentScdawgChar::<u64>::create("docs.pscdawg")?;
index.insert_with_value("the quick brown fox", 7);
assert!(index.contains_substring("quick"));
assert_eq!(index.locations("brown"), vec![("the quick brown fox".to_string(), 10)]);
index.checkpoint()?;

let reopened = PersistentScdawgChar::<u64>::open("docs.pscdawg")?;
assert!(reopened.contains_substring("quick"));
# Ok(())
# }
```

## Verification And Benchmarks

Persistent suffix graph tests cover reopen, recovery, mutation parity, substring
locations, and byte/char variants. Benchmarks live in
`benches/persistent_suffix_native_benchmarks.rs`; fixed-sample mode prints raw
samples suitable for pgmcp/Welch analysis.

Do not use this document to claim lock-free suffix graph writes. The read path is
snapshot/non-blocking; the write path is serialized around graph rebuild and
copy-on-write publication.
