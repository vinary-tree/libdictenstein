# The persistent dictionary families

**Navigation**: [↑ Persistence architecture](README.md) · [Lock-free overlay](lock-free-overlay.md) · [Storage backends](storage-backends.md) · [Core abstractions](../architecture/abstractions.md)

The `persistent-artrie` feature ships several disk-backed dictionaries. They **share one
durability infrastructure** (a write-ahead log (WAL), checkpoint, buffer/block storage — the
[durable-storage kernel](durable-storage-kernel.md)) but split into **two representation
families** that differ in *what they publish*:

1. the **ARTrie family** — a lock-free copy-on-write *overlay* over a radix trie, folded
   periodically into a dense checkpoint image; and
2. the **suffix-graph family** — immutable *native substring graphs*, rebuilt and
   republished per write.

Both log to the WAL before a write is visible; they differ only in the shape of the data
they make durable.

---

## ARTrie family — overlay + checkpoint

Each write appends a WAL record, *then* publishes a new immutable overlay root; the
successful compare-and-swap (CAS) is both the visibility point and the linearization
point. Readers traverse the current published root and never take a global mutation lock.
Periodically a **checkpoint** folds the published overlay into a dense image and advances
the reclaimable WAL watermark (see [durability-and-recovery.md](durability-and-recovery.md)).

### Profiles

| Profile | Keys | Encoding | File magic | Intended use |
|---------|------|----------|-----------|--------------|
| `PersistentARTrie<V>` | bytes (`u8`) | `ByteKey` | `b"PART"` | arbitrary byte strings; **no** UTF-8 re-decode on reconstruction |
| `PersistentARTrieChar<V>` | Unicode scalars (`u32`) | `CharKey` | `b"ARTC"` | Unicode text (CJK, emoji, accents); WAL stores terms as UTF-8 |
| `PersistentARTrieU64Compact<V>` | native `u64` sequences | `U64Key<4>` | `b"AR64"` | token / time-series data; prefix-4 compact **CX** checkpoint budget (CX = the compact-snapshot codec, magic `AR64CX01`) |
| `PersistentARTrieU64Prefix3Compat<V>` | native `u64` sequences | `U64Key<3>` | `b"AR64"` | prefix-3 compatibility / benchmark baseline |
| `PersistentVocabARTrie` | Unicode terms | `CharKey`, `V = u64` | `b"ARTC"` (vocab header) | durable `term $\leftrightarrow$ u64` bijection |

Here $`V`$ is the caller's value type; membership-only dictionaries use $`V = ()`$, counting
dictionaries use a `u64` counter, and mapped dictionaries use an arbitrary construction-time
`Option<V>`. Exact lookup costs $`O(m)`$ in the term length $`m`$ regardless of profile.

`PersistentVocabARTrie` is specialized: its **forward** lookup ($`\text{term} \to \text{id}`$)
walks the overlay in $`O(k)`$ for a term of $`k`$ units; its **reverse** lookup
($`\text{id} \to \text{term}`$) is $`O(1)`$ through an in-memory `reverse_term_map` that is
**rebuilt from the checkpoint image on reopen** (the reverse direction is not itself logged —
it is a derived index). See [../algorithms/vocab-trie.md](../algorithms/vocab-trie.md).

### One implementation, three alphabets

The four ARTrie profiles are **not** four hand-written tries. A single generic
`OverlayNode<K: KeyEncoding, V>` (`src/persistent_artrie/core/overlay/node.rs`) is
monomorphized once per alphabet; the `KeyEncoding` marker (`ByteKey` / `CharKey` /
`U64Key`) threads the per-alphabet specifics — unit width, public traversal token,
term reconstruction, on-disk magics, path-compression cap — through the associated types
and constants. This is the "three alphabets, one code path" design detailed in
[abstractions.md](../architecture/abstractions.md):

<img src="../diagrams/units-keys.svg" alt="Two abstractions fanning into the shared generic backends. The left grey column is CharUnit (src/char_unit.rs) with its u8/char/u64 impls feeding the generic in-memory DictionaryNode. The right grey column is KeyEncoding (core/key_encoding.rs) with its ByteKey/CharKey/U64Key impls, each labelled with its Unit, Token, and KEY_BYTES, fanning into the single generic persistent overlay OverlayNode<K,V> / AtomicNodePtr<K,V> / OverlayDictionaryNode<K,V> (blue). A dashed edge marks the seam KeyEncoding::Token is itself a CharUnit. Grey = the unit/key traits and their alphabets; green = the in-memory consumers; blue = the persistent overlay consumers." width="100%"/>

The **child storage** inside each node is a shared `AdaptiveEdgeStore` that adapts to label
width: byte keys use ART-style dense `Node4/16/48/256` tiers for high fan-out (the Adaptive
Radix Tree of Leis et al. 2013), while char
and `u64` keys retain native labels and use inline, sorted, or sparse-indexed storage as
fan-out grows (see [storage-backends.md](storage-backends.md#adaptive-edge-storage)).

---

## Suffix-graph family — native graph snapshots

The persistent suffix-graph types provide durable **substring** APIs *without* encoding
suffixes as ARTrie keys — they persist native substring graphs instead of a trie overlay:

- `PersistentSuffixAutomaton` / `PersistentSuffixAutomatonChar`
- `PersistentSuffixTree` / `PersistentSuffixTreeChar`
- `PersistentScdawg` / `PersistentScdawgChar`

Reads are snapshot-based and non-blocking with respect to graph mutation. A write appends
a **prepared** operation segment, rebuilds a candidate graph revision off to the side,
publishes it with a pointer-identity CAS, and appends a **commit** segment before
acknowledging the caller. Recovery ignores prepared records that lack a commit marker (and
also accepts historical monolithic WAL files). Mapped `update_or_insert` takes a
retry-safe `Fn(&mut V)` updater, so a CAS conflict recomputes against the newest snapshot
without a writer lock.

<img src="../diagrams/suffix-graph-publish.svg" alt="A sequence diagram of a persistent suffix-graph write. A concurrent Reader loads the current Arc snapshot from the ArcSwap root and queries the old immutable graph lock-free. The Writer first appends a durable Prepare record to the operation-segment WAL (orange, fsync) and gets an Lsn; then clones the current snapshot and applies the op to build a private candidate (green); then in a loop compare-and-swaps the ArcSwap root from old to new using Arc::ptr_eq, retrying on a lost race up to 64 times; then appends a durable Commit record and acknowledges — the write is now durable and visible. The old snapshot is reclaimed once the last reader Arc drops. A closing note explains crash semantics: on reopen the image is loaded and the WAL tail replayed, applying only Prepares whose Commit is durable, in op_id order." width="100%"/>

See [../algorithms/persistent-suffix-graphs.md](../algorithms/persistent-suffix-graphs.md)
for the full substring-API treatment and the Inenaga-family theory.

---

## The module layering invariant

The implementation enforces the family split with a **layering invariant** that is both
documented and grep-verified: the shared foundation (`core/` and `nodes/`) never `use`s any
variant, and the byte variant never `use`s char/vocab — so every allowed `use` edge points
*down* onto the foundation, and there is no upward edge from `core` into a variant.

<img src="../diagrams/layering-invariant.svg" alt="A bottom-to-top module dependency graph. At the bottom rank, the grey shared foundation: core/ (KeyEncoding, overlay, wal, buffer/disk manager, arena) and nodes/ (Node4/16/48/256, CharBucket, adaptive edge tiers). Above it, three variants: the byte variant persistent_artrie (green, PersistentARTrie, ByteKey/u8), char/ (blue, PersistentARTrieChar, CharKey/u32), and vocab/ (amber, PersistentVocabARTrie, builds on char). Solid dark-slate 'use' edges point down: byte→core, byte→nodes, char→core, char→nodes, vocab→core, and vocab→char ('builds on char'). Dashed red crossed-out edges mark the forbidden directions the grep asserts are empty: core never uses a variant, and byte never uses char/vocab. A legend notes that char/vocab reach some core facilities via byte's pub use re-export aliases, but the true dependency is still on core." width="92%"/>

The current source tree (reorganized so the variants are subdirectories of one parent) is:

```text
src/persistent_artrie/
├── core/          unit-agnostic substrate (the durable-storage kernel)
│   └── overlay/   the lock-free overlay engine (the heart)
├── nodes/         the classic adaptive ART node zoo (Node4/16/48/256, CharBucket)
├── char/          the char (u32) variant
├── vocab/         the vocab (term↔u64) variant, built on char
└── *.rs           the byte (u8) variant lives at the top level
```

**A documentation trap worth naming.** `char/` and `vocab/` reach several core facilities
through the byte variant's back-compat re-export *aliases*
(`crate::persistent_artrie::{wal, eviction, block_storage, disk_manager}` are
`pub use crate::persistent_artrie::core::…`), rather than naming `core` directly in every
`use`. The *true* dependency is still on `core`; only the namespace path sometimes routes
through the parent module. The layering invariant holds because that path resolves to
`core`, never to variant-specific code.

---

## Durability model — where the two families diverge

Both families distinguish **visibility** from **durability**: an acknowledged write is
never visible before it is durable. They differ in what a checkpoint publishes and how
recovery reconstructs state:

| | ARTrie family | Suffix-graph family |
|---|---------------|---------------------|
| Live representation | immutable copy-on-write **overlay** root | immutable **native graph** snapshot |
| Visibility point | winning **root-CAS** | winning **pointer-identity CAS** |
| WAL granularity | per-operation records (`Insert`, `Increment`, …) | prepared/commit **operation segments** |
| Checkpoint publishes | dense **CX image** of the overlay | dense image of the graph |
| Recovery | image + WAL tail, reconciled to commit order | image + WAL tail, dropping prepared-without-commit |
| Under writer churn | checkpoint always folds | checkpoint may skip image publication and rely on retained WAL replay |

> **Do not** describe the current `u64` ARTrie as using the old native bincode
> snapshot/WAL path — that implementation was removed from source; benchmark controls for
> it come from git history/worktrees.

## Related documentation

- [Persistence architecture (entry point)](README.md)
- [Lock-free overlay](lock-free-overlay.md) · [Durability & recovery](durability-and-recovery.md)
- [Core abstractions — `CharUnit` + `KeyEncoding`](../architecture/abstractions.md)
- [Persistent ARTrie design (theory)](../theory/disk-tries/06-persistent-artrie-design.md)
- [Vocabulary trie](../algorithms/vocab-trie.md) · [Native u64 + CX](../algorithms/native-u64-and-cx.md) · [Persistent suffix graphs](../algorithms/persistent-suffix-graphs.md)

## References

- V. Leis, A. Kemper, T. Neumann. *The Adaptive Radix Tree.* ICDE 2013.
  [DOI:10.1109/ICDE.2013.6544812](https://doi.org/10.1109/ICDE.2013.6544812)
