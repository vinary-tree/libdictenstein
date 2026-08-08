# Design records

This directory holds the **durable design and mechanism references** for
libdictenstein — the documents you read to understand *how a subsystem works and
why it is shaped that way*, and that are kept current as the code evolves.

> **Architecture-level synthesis.** For the disk-backed persistence engine as a
> whole — how the lock-free overlay, WAL, checkpoint, recovery, concurrency, and
> eviction layers compose into one story — start at
> **[`../persistence/README.md`](../persistence/README.md)**. That corpus is the
> *narrative* map; the overlay / durability / checkpoint records below are the
> *mechanism-detail* pages it links down into. Read narrative → detail to go deep,
> or detail → narrative (via each record's **Synthesized in** header) to zoom out.

The day-to-day **development-campaign ledger** — point-in-time execution plans,
red-team logs, handoffs, and bug-fix designs produced while building these
mechanisms — lives separately under [`history/`](history/README.md). Those are
preserved for provenance, not maintained as references.

---

## Durable references

The lock-free **overlay** is the heart of the persistent ARTrie family: an
immutable, path-copied, CAS-published representation shared by the byte, char, and
vocab tries. The first cluster of documents describes that overlay and its
lock-free machinery; the second describes the **durability / checkpoint / recovery**
control flow layered on top; the third covers the non-ARTrie dictionary families
(DAWG, suffix index).

| Document | What it covers |
|----------|----------------|
| [`lockfree-cas-artrie.md`](lockfree-cas-artrie.md) | The foundational lock-free concurrent-insert mechanism for `PersistentARTrie`/`PersistentARTrieChar`: persistent (immutable) nodes + Compare-And-Swap publish instead of an `RwLock`, including the current `arc_swap`-based atomic-root reality. |
| [`overlay-backed-dictionary-node.md`](overlay-backed-dictionary-node.md) | How `Dictionary::root()` exposes the lock-free overlay as a `DictionaryNode` so the zipper / Levenshtein transducer / fuzzy search traverse the overlay (not the now-empty owned tree) when `route_overlay()` is true. |
| [`g4-unify-overlay-node.md`](g4-unify-overlay-node.md) | The G4 design unifying the byte and char overlay nodes into a single generic `OverlayNode<U, V>` over the `KeyEncoding`/`CharUnit` traits, eliminating the duplicated node implementation. |
| [`arbitrary-v-overlay-genericization.md`](arbitrary-v-overlay-genericization.md) | Roadmap for lifting the overlay's value field from a `u64`-only `AtomicU64` to an arbitrary construction-time `Option<V>`, making the lock-free architecture the default for all `V` (not just `() `/`u64`-counter). |
| [`deep-term-iterative-overlay.md`](deep-term-iterative-overlay.md) | Why the un-path-compressed overlay spine recurses one node per key unit, and how insert / checkpoint-serialize / drop were made iterative to survive very long (500-char) terms without stack overflow. |
| [`empty-string-value-support.md`](empty-string-value-support.md) | The implemented + gated design making the empty term `""` a full first-class, value-carrying key (membership / counter / arbitrary-`V`) across the byte, char, and vocab tries. |
| [`overlay-flip-genericization.md`](overlay-flip-genericization.md) | Extracting the char lock-free-overlay "flip" into a shared generic layer over `K: KeyEncoding` so the byte trie reuses it, and the correctness argument for why vocab is excluded (its overlay value is an allocator-assigned index). |
| [`lockfree-flip-irreversible.md`](lockfree-flip-irreversible.md) | The irreversible, owner-gated, data-loss-critical "lock-free flip" design (Phase E2/E1/checkpoint/eviction/recovery + Phase F) that makes the overlay the production default. |
| [`f4-lock-collapse-implementation.md`](f4-lock-collapse-implementation.md) | The "Lock Collapse" implementation record: deleting the outer trie `RwLock` on `SharedARTrie`/`SharedCharARTrie`/`SharedVocabARTrie` so overlay reads *and* writes are fully lock-free, with mutators routing to lock-free CAS internally. |
| [`os-level-locking.md`](os-level-locking.md) | The **Tier-1** exclusive-owner OS advisory lock (`flock` on a `.wlock` sidecar at the `DiskManager` open chokepoints) that makes a second process opening the same file fail cleanly with `FileLocked` instead of silently corrupting it — closing the multi-process footgun and forming the `LOCK_EX` half of SWMR. |
| [`swmr-multiprocess.md`](swmr-multiprocess.md) | The **Tier-2** single-writer / multi-reader-**process** (SWMR) design: reader processes open read-only and serve lock-free snapshots of the last durable checkpoint, refreshed via an atomically-renamed image inode + a background `checkpoint_lsn` poll — preserving the intra-process lock-free invariant. |
| [`overlay-durable-architecture.md`](overlay-durable-architecture.md) | The shared lock-free **durable**-overlay architecture (Template-Method-driven): one copy of the data-loss-critical durable-write + checkpoint + watermark + recovery control flow, shared across byte, char, and future variants. |
| [`non-blocking-checkpoint.md`](non-blocking-checkpoint.md) | The non-blocking checkpoint for the persistent char ARTrie via an `RwLock` write→read downgrade, so a checkpoint no longer starves concurrent readers; correct + formally verified, with measured results. |
| [`dynamic-dawg.md`](dynamic-dawg.md) | Design rationale for the mutable, minimized `DynamicDawg` family: immutable revisions, copy-on-write of shared paths, root CAS publication, and the shared unit-generic core. |
| [`suffix-automaton.md`](suffix-automaton.md) | Design rationale for the **volatile** `SuffixAutomaton`: online construction, arena representation, whole-graph-snapshot concurrency, and the two deliberate trait asymmetries. |
| [`persistent-suffix-index.md`](persistent-suffix-index.md) | Overview of the **persistent** suffix-index family — suffix automata, suffix-tree-compatible API, and SCDAWGs — in byte and Unicode forms, with WAL/checkpoint/CAS durability. |
| [`volatile-concurrency.md`](volatile-concurrency.md) | The concurrency model shared by the in-memory backends: wait-free reads, lock-free writes, the path-copy and whole-graph root-publication strategies, their invariants, and correspondence/stress/sanitizer testing. |

---

## Historical campaign records

The execution plans, red-team findings, formal-verification strategies, handoffs,
and bug-fix designs that produced the mechanisms above are preserved under
[`history/`](history/README.md), grouped by development campaign. They are
point-in-time scientific-ledger records kept for provenance and are **not**
maintained as current references — start from the durable references above
instead.
