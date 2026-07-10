# Persistence — architecture-tier orientation

This is the **architecture-tier** orientation to the `persistent-artrie` feature: a
one-screen glance at how the durable dictionary engine is shaped. The **full
systems-architecture corpus** — subsystem by subsystem, with all diagrams, algorithms,
and the theory/proof links — lives at **[`../../persistence/`](../../persistence/README.md)**;
start there for depth.

## Architecture at a glance

The `persistent-artrie` family shares one lock-free **durable overlay stack** — a client
API over an atomic-root overlay, gated by a log-before-publish WAL, checkpointed into a
dense **CX** (compact-snapshot) image, and persisted by a pluggable block backend:

<img src="../../diagrams/artrie-layering.svg" alt="Persistent ARTrie durability stack: Client API → lock-free overlay (atomic root, immutable adaptive edge stores) → durability (append+fsync WAL, then publish via CAS, checkpoint lock) → checkpoint storage (mmap default or io_uring+O_DIRECT, CX/dense images, retained WAL replay). Acknowledged implies durable; linearization point = the winning CAS." width="100%"/>

```text
Persistent ARTrie byte/char/u64/vocab      (the ARTrie family — overlay + checkpoint)
    Dictionary API
      → immutable overlay nodes
      → WAL-before-publish durability
      → CX/dense checkpoint image

Persistent suffix automaton/tree/SCDAWG    (the suffix-graph family — native snapshots)
    Dictionary + SubstringDictionary APIs
      → immutable native graph snapshots
      → prepared/commit operation-segment WAL
      → CAS copy-on-write graph rebuild/publish
```

The engine splits into **two representation families** — the **ARTrie family** (a
lock-free copy-on-write overlay folded into a dense checkpoint) and the **suffix-graph
family** (immutable native substring graphs republished per write) — over one shared
durability infrastructure. Both log before a write is visible; they differ in *what* they
publish.

## Where to read next

| To understand… | Read |
|----------------|------|
| the whole stack, top to bottom | [Persistence architecture entry point](../../persistence/README.md) |
| the family split, profiles & module layering | [families.md](../../persistence/families.md) |
| `core/` as a reusable engine for a **new** persistent file layer | [durable-storage-kernel.md](../../persistence/durable-storage-kernel.md) |
| the lock-free overlay (immutable nodes, arc-swap root, CAS) | [lock-free-overlay.md](../../persistence/lock-free-overlay.md) |
| durability, checkpoint flips & crash recovery | [durability-and-recovery.md](../../persistence/durability-and-recovery.md) |
| the concurrency model & lock hierarchy | [concurrency-model.md](../../persistence/concurrency-model.md) |
| storage backends & the on-disk format | [storage-backends.md](../../persistence/storage-backends.md) · [wal-format.md](../../persistence/wal-format.md) |

## Related

- [Core abstractions — `CharUnit` + `KeyEncoding`](../abstractions.md)
- [Disk-trie theory](../../theory/disk-tries/) — the ART and persistent-ART foundations.
- [Root README — persistent section](../../../README.md#persistent-artrie--lock-free--durable)
