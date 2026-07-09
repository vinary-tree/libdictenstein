# Native `u64` keys and the CX compact snapshot

**Navigation**: [↑ Dictionary layer](README.md) · [Crate README → persistent ARTrie](../../README.md#persistent-artrie--lock-free--durable) · [Order-A writes →](../../README.md#durable-writes-the-order-a-protocol) · [Persistence architecture →](../persistence/README.md)

> **Scope.** This document describes the **native `u64` sequence/time-series
> profile** of the persistent ARTrie — `PersistentARTrieU64` and its two
> ready-made aliases `PersistentARTrieU64Compact` (the default) and
> `PersistentARTrieU64Prefix3Compat` — and the **CX compact snapshot format**
> through which it checkpoints. It explains *why* native 64-bit edge labels beat
> byte-expanding the key, *what* the `U64Key<PREFIX>` encoding is, and *how* the
> two profiles (prefix-4 disk-compact vs. prefix-3 compatibility) differ. The
> general persistent-ARTrie machinery (lock-free overlay, WAL, Order-A writes,
> recovery) is shared with the byte/char families and documented under
> [`../persistence/`](../persistence/README.md); this page is the
> `u64`-specific layer.

---

## 1. Purpose — sequence and time-series keys, kept whole

The byte (`PersistentARTrie`) and Unicode (`…Char`) ARTries key on **text**: a
term is a sequence of `u8` or `char` units. But many durable indexes are keyed on
sequences whose natural unit is a **64-bit word**, not a character:

- **Token sequences** — n-grams of vocabulary ids, instruction streams, opcode
  traces, event-type sequences.
- **Time series** — quantized samples, where each `f64` reading is mapped to its
  bit pattern (`f64::to_bits`) and the series becomes a `&[u64]` key.
- **Composite keys** — any tuple of 64-bit fields (timestamps, hashes, ids)
  concatenated into one sequence.

`PersistentARTrieU64` is the durable Adaptive Radix Trie for exactly these: a
crash-safe, lock-free, write-ahead-logged $`&[u64] \to V`$ map where **one trie edge
carries one whole `u64`**.

---

## 2. Intuition — why native `u64` labels beat byte expansion

You *could* store a `u64` sequence in a byte ARTrie by serializing each word into
8 bytes. That "byte-expansion" is tempting but quietly expensive, and the native
profile exists to avoid it.

Consider a key of `n` sixty-four-bit words.

| Aspect | Byte-expanded into a `u8` ARTrie | Native `u64` ARTrie |
|--------|----------------------------------|---------------------|
| **Edges traversed per lookup** | up to $`8 \cdot n`$ (one per byte) | `n` (one per word) |
| **Interior nodes on the spine** | up to $`8 \cdot n`$ node hops | `n` node hops |
| **Label comparison** | byte-at-a-time descent | one 64-bit equality test per hop |
| **Fan-out alphabet** | 256 (byte values) | the set of distinct next-words |
| **Wasted structure** | 7 "filler" nodes per word for non-branching bytes | none — words are atomic |

Byte expansion turns every key into an $`8\times`$-longer path and forces the trie to
spend interior nodes resolving *within* a word that never actually branches there.
Native `u64` labels keep each transition **atomic**: the descent length is the
number of *words*, the per-hop test is a single machine-word compare, and the
adaptive node only grows when there is genuine **word-level** branching. This is
the same reasoning the Adaptive Radix Tree paper
([Leis et al. 2013](https://doi.org/10.1109/ICDE.2013.6544812)) gives for using
native-width labels and SIMD/indexed lookup once fan-out outgrows inline storage —
here applied at 64-bit granularity.

<img src="../diagrams/native-u64-hops.svg" alt="Comparison of storing a three-word u64 key: a byte-expanded u8 ARTrie needs up to 24 node hops with 21 filler nodes (8 byte-nodes per word), while a native u64 ARTrie needs just 3 hops and 0 filler nodes because each 64-bit word is one atomic edge." width="70%"/>

---

## 3. The `U64Key<PREFIX>` encoding

The `u64` variant is one instantiation of the crate's generic key-encoding seam.
The `KeyEncoding` trait (`src/persistent_artrie/core/key_encoding.rs`) lets the
shared overlay node, adaptive edge store, dictionary-node handles, and the CX
checkpoint serializer all be generic over the **unit width** of a variant. Three
marker types implement it:

| Marker | `Unit` | Used by |
|--------|--------|---------|
| `ByteKey` | `u8` | `PersistentARTrie` (text bytes) |
| `CharKey` | `u32` (`char`) | `PersistentARTrieChar` (Unicode) |
| `U64Key<const PREFIX = 4>` | `u64` | `PersistentARTrieU64` (native sequences) |

`U64Key` carries a **const generic `PREFIX`** — the *CX path-compression prefix
budget* (see §4). The same overlay-node shape is reused for all three unit widths;
only the unit type and a handful of associated constants differ. The native `u64`
keys therefore inherit the *entire* lock-free overlay + Order-A durability stack
for free, while keeping their native 64-bit labels.

```text
type U64Node<V, const PREFIX: usize> = OverlayNode<U64Key<PREFIX>, V>;

pub struct PersistentARTrieU64<V = (), S = MmapDiskManager, const PREFIX: usize = 4> {
    root: AtomicNodePtr<U64Key<PREFIX>, V>,   // lock-free overlay root (CAS-published)
    term_count: AtomicUsize,
    committed_watermark: CommittedWatermark,  // Order-A durable frontier
    commit_seq: AtomicU64,                     // CommitRank generation counter
    checkpoint_lock: Arc<Mutex<()>>,           // serializes checkpoints, not reads
    /* … wal_writer, path, … */
}
```

Each interior transition stores its child as a `(u64, u64)` pair — `(label,
child)` — i.e. **one native `u64` edge per transition**, exactly as the intuition
in §2 requires.

---

## 4. The CX compact snapshot — checkpointing through a compressor

A **checkpoint** folds the live overlay into a dense on-disk image so that reopen
is `O(image) + O(WAL tail)` rather than `O(history)`. For the `u64` variant that
image is the **CX compact snapshot** (magic `AR64CX01`, `SNAPSHOT_VERSION = 1`):

- It is a **dense, path-compressed** serialization. Long non-branching runs of
  `u64` labels are collapsed into a single node carrying a **prefix** of up to
  `PREFIX` words, instead of one node per word. The `PREFIX` budget is the maximum
  number of `u64` labels a compressed node may absorb before a new node is forced.
- It is keyed by the **committed watermark** so the image is a coherent,
  fully-durable cut of the trie.
- It is **intentionally not** the old native `bincode` snapshot/WAL format. The
  CX compact format replaced it; the historical bincode controls live **only in
  git history** and are not a supported on-disk format. (Hence
  `PersistentARTrieU64Compact::open` reads CX images, not bincode.)

<img src="../diagrams/cx-compact-node.svg" alt="Conceptual on-disk field layout of a CX compact snapshot node: is_final (leaf flag), a path-compressed prefix of up to PREFIX u64 labels, an optional value, and a children array of (label, child) u64 edge pairs." width="70%"/>

The `PREFIX` budget is the *only* knob that distinguishes the two shipped
profiles. The systems-level description of the `u64` on-disk image — the `AR64CX01`
snapshot, the `b"AR64"` file magic, and how the CX format sits beside the byte/char
formats and the two block backends — is in
[`../persistence/storage-backends.md#u64-profile-formats`](../persistence/storage-backends.md#u64-profile-formats).

### 4.1 The two profiles

| Alias | `PREFIX` | Role |
|-------|----------|------|
| **`PersistentARTrieU64Compact`** | `4` (`U64_CX_PREFIX_COMPACT`) | **Default.** One `u64` edge per transition; the wider prefix budget was measured to **reduce checkpoint bytes** while preserving lookup performance. Use this for new indexes. |
| **`PersistentARTrieU64Prefix3Compat`** | `3` (`U64_CX_PREFIX_COMPAT`) | **Compatibility / baseline.** Opens or benchmarks **prefix-3 CX images** explicitly, and serves as the baseline the prefix-4 budget is compared against. |

Both are thin type aliases over the same `PersistentARTrieU64<V, S, PREFIX>`
generic; they differ *only* in the const `PREFIX`. A wider budget (4) packs more
labels per compressed node, shrinking the image; the prefix-3 alias exists so that
images written under the older budget remain openable and so benchmarks can hold
the budget fixed when comparing.

> **Profile = on-disk compatibility.** Because `PREFIX` shapes how many labels a
> CX node absorbs, a CX image is read back with the alias whose budget it was
> written with. Choose `…Compact` (prefix-4) for everything new; reach for
> `…Prefix3Compat` only to open legacy prefix-3 images or to reproduce prefix-3
> benchmark baselines.

---

## 5. Durable writes — the same Order-A protocol, native labels

The `u64` profile follows the crate-wide **Order-A "log before publish"** rule
([crate README](../../README.md#durable-writes-the-order-a-protocol)), with shared
WAL records and lock-free root publication:

```text
insert_sequence_with_value(seq, value):
    generation := commit_seq.fetch_add(1) + 1        # CommitRank generation
    loop:
        root      := self.root.load()                # snapshot the overlay
        new_root  := build_insert_path(root, seq, value)   # copy-on-write spine
        if self.root.compare_exchange(root, new_root).is_ok():   # CAS publish (linearize)
            term_count.fetch_add(1) on first insert
            break
    data_lsn := WAL.append_and_sync(data record)     # durable
    WAL.append(CommitRank { data_lsn, generation })   # durable rank
    committed_watermark.mark_committed(data_lsn …)    # advance the durable frontier
```

- **Copy-on-write spine + CAS publish.** `build_insert_path` clones only the nodes
  along the affected spine and the root is swapped with `compare_exchange`; losers
  retry on the newer root. Reads traverse the immutable overlay with no lock.
- **CommitRank generations.** A durable global `commit_seq` stamps each write with
  a generation; on recovery `reconcile_lww` replays survivors in
  `(generation, lsn)` order so concurrent out-of-order commits resolve to the
  last-writer-wins result. This is the identical machinery the byte/char families
  use — the `u64` variant simply rides on it.
- **`checkpoint_lock`.** Held only to serialize *checkpoints* against each other;
  it does **not** gate reads, which stay lock-free against the overlay.

Recovery, then, is the standard redo-only path: load the CX image at the committed
watermark, replay the durable WAL tail past it in `(generation, lsn)` order, drop
un-acknowledged/orphan records, rebuild the overlay, resume. See
[`../persistence/README.md`](../persistence/README.md) and
the recovery flow under [`../persistence/wal-format.md`](../persistence/wal-format.md).

---

## 6. Usage

The durable native-`u64` quick start (mirrors the crate README, compile-checked
there as `rust,no_run`): create a prefix-4 CX file, insert a 3-word series keyed by
the `f64` bit patterns of a time-series sample, checkpoint, reopen.

```rust,no_run
use libdictenstein::persistent_artrie::PersistentARTrieU64Compact;

// Prefix-4 CX profile: the default compact native-u64 representation.
let series = PersistentARTrieU64Compact::<u64>::create("series.ar64")?;
series.insert_sequence_with_value(
    &[0x1000_0000_0000_002a, 0x3000_0000_0000_0100, f64::to_bits(42.5)],
    f64::to_bits(42.5),
);
series.checkpoint()?;   // fold the overlay into a dense CX image

let reopened = PersistentARTrieU64Compact::<u64>::open("series.ar64")?;
assert!(reopened.contains_sequence(
    &[0x1000_0000_0000_002a, 0x3000_0000_0000_0100, f64::to_bits(42.5)]
));
# Ok::<(), Box<dyn std::error::Error>>(())
```

Membership without values uses `insert_sequence` / `contains_sequence`:

```rust,no_run
use libdictenstein::persistent_artrie::PersistentARTrieU64Compact;

let ngrams = PersistentARTrieU64Compact::<()>::create("ngrams.ar64")?;
ngrams.insert_sequence(&[10, 42, 7]);    // a token-id 3-gram
ngrams.checkpoint()?;
assert!(ngrams.contains_sequence(&[10, 42, 7]));
assert!(!ngrams.contains_sequence(&[10, 42, 8]));
# Ok::<(), Box<dyn std::error::Error>>(())
```

Opening a **legacy prefix-3** CX image uses the compatibility alias so the budget
matches what the file was written with:

```text
use libdictenstein::persistent_artrie::PersistentARTrieU64Prefix3Compat;

// Only for prefix-3 images / prefix-3 benchmark baselines.
let legacy = PersistentARTrieU64Prefix3Compat::<u64>::open("legacy_p3.ar64")?;
```

---

## 7. Properties at a glance

| Property | Native `u64` profile | Mechanism |
|----------|----------------------|-----------|
| **Lookup hops** | `O(words)`, not `O(bytes)` | one native `u64` edge per transition |
| **Per-hop test** | single 64-bit compare | atomic word labels, no byte descent |
| **Reads lock-free** | wait-free per traversal | immutable overlay, CAS-published root |
| **Writes durable & linearizable** | Order-A log-before-publish | copy-on-write spine + `compare_exchange` + WAL `CommitRank` |
| **Bounded reopen** | `O(CX image) + O(WAL tail)` | CX compact checkpoint at the committed watermark |
| **Compact images** | prefix-budgeted path compression | `U64Key<PREFIX>`; default `PREFIX = 4` |
| **Format discipline** | CX only (not legacy bincode) | `AR64CX01` snapshot; bincode controls live in git history |

---

## 8. Relationship to the rest of the crate

- The **shared** persistent-ARTrie substrate — lock-free overlay, WAL framing,
  Order-A writes, checkpoint/recovery, `mmap`/`io_uring` storage:
  [`../persistence/README.md`](../persistence/README.md) and
  [`../persistence/wal-format.md`](../persistence/wal-format.md).
- The **on-disk** side of this profile — the `u64` CX image format and the
  `BlockStorage` backends it is written through:
  [`../persistence/storage-backends.md#u64-profile-formats`](../persistence/storage-backends.md#u64-profile-formats).
- The byte/char ARTrie family this generalizes (same overlay, `ByteKey`/`CharKey`
  instead of `U64Key`): [crate README → persistent variants](../../README.md#persistent-artrie--lock-free--durable).
- The sibling durable substring and vocabulary backends:
  [`persistent-suffix-graphs.md`](persistent-suffix-graphs.md) and
  [`vocab-trie.md`](vocab-trie.md).

---

## References

1. Leis, V., Kemper, A., & Neumann, T. (2013). *The Adaptive Radix Tree: ARTful
   Indexing for Main-Memory Databases.* IEEE ICDE.
   [10.1109/ICDE.2013.6544812](https://doi.org/10.1109/ICDE.2013.6544812) — the
   case for native-width labels and adaptive nodes that the `u64` profile applies
   at 64-bit granularity.
2. Driscoll, J. R., Sarnak, N., Sleator, D. D., & Tarjan, R. E. (1989). *Making
   Data Structures Persistent.* Journal of Computer and System Sciences 38(1).
   [10.1016/0022-0000(89)90034-2](https://doi.org/10.1016/0022-0000(89)90034-2) —
   the immutable-versioned overlay the CAS publish builds on.
3. Mohan, C., Haderle, D., Lindsay, B., Pirahesh, H., & Schwarz, P. (1992). *ARIES:
   A Transaction Recovery Method …Using Write-Ahead Logging.* ACM TODS 17(1).
   [10.1145/128765.128770](https://doi.org/10.1145/128765.128770) — the
   log-before-publish / redo-on-recovery foundation of the Order-A protocol.

---

**Navigation**: [↑ Dictionary layer](README.md) · [Crate README → persistent ARTrie](../../README.md#persistent-artrie--lock-free--durable) · [Order-A writes →](../../README.md#durable-writes-the-order-a-protocol) · [Persistence architecture →](../persistence/README.md)
