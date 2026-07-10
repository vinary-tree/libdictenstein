# `unsafe` code and its contracts

**Navigation**: [← Security](README.md) · [Threat model](threat-model.md)

This document explains **where** the crate uses `unsafe`, **why** each site is sound, and **how**
that soundness is kept honest over time. It is a map into the authoritative artifacts — it does
**not** reproduce them, because they are CI-gated for drift and any copy here would rot. Notation
follows [`docs/notation.md`](../notation.md).

## The authoritative artifacts

| Artifact | What it is |
|----------|------------|
| [`formal-verification/UNSAFE_INVENTORY.tsv`](../../formal-verification/UNSAFE_INVENTORY.tsv) | Every `unsafe` site in the tree: **43 rows**, columns `path · kind · pattern · count · contract`, where `kind ∈ {unsafe_block, unsafe_fn, unsafe_impl}`. |
| [`formal-verification/UNSAFE_CONTRACTS.tsv`](../../formal-verification/UNSAFE_CONTRACTS.tsv) | The safety contracts each site is bound to: **31 contracts**, columns `contract · scope · obligation · coverage · status · evidence`, where `coverage ∈ {rocq, tla, loom, miri, correspondence, compile-time, unit, trusted-boundary}` and `status ∈ {covered, miri-wired, trusted-boundary}`. |
| [`formal-verification/UNSAFE_BOUNDARY.md`](../../formal-verification/UNSAFE_BOUNDARY.md) | The prose boundary map and the safety-contract matrix that ties sites to obligations. |
| [`scripts/verify-unsafe-boundary-inventory.sh`](../../scripts/verify-unsafe-boundary-inventory.sh) | The CI gate that keeps all of the above true. |

## Where the `unsafe` is

`unsafe` is **concentrated in the persistent engine**; the in-memory dictionaries are almost entirely
safe Rust. By the inventory's `path` column:

| Region | `unsafe` rows | What kind |
|--------|:---:|-----------|
| **Volatile** (`src/scdawg/{ascii,char}.rs`) | **4** | all `unsafe impl Send`/`Sync` thread-safety assertions for the SCDAWG node handle — no raw pointers, no memory-layout unsafety |
| **Shared** (`src/substring.rs`) | **2** | `unsafe impl Send`/`Sync` on a **test-mock** node |
| **Persistent** (`src/persistent_artrie/**`) | **37** | swizzled pointers, atomic node CAS, optimistic-lock cells, raw child pointers, `io_uring` fixed buffers, and `Send`/`Sync` impls |

So **6 of 43** `unsafe` rows are outside the persistent tree, and none of those six involves raw
pointers or manual memory layout — they are all `Send`/`Sync` promises. This is the concrete basis
for the [threat model](threat-model.md)'s claim that the in-memory dictionaries are memory-safe by
construction (see also [architecture §5](../architecture/in-memory-dictionaries.md#5-where-the-unsafe-is-there-is-almost-none)).

## How each site is proven sound

Every inventory row names a **contract**, and every contract names a **coverage class** — the kind
of evidence that discharges its proof obligation:

- **rocq** — a machine-checked Rocq theorem (functional correctness / refinement).
- **tla** — a TLA⁺ model checked by TLC (concurrency / crash-recovery safety), e.g. `PointerOwnership`,
  `BufferPageLease`, the `LockFree*` and `IoUring*` models.
- **loom** — exhaustive interleaving of the lock-free CAS paths.
- **miri** — run under Miri for undefined-behavior / strict-provenance checking.
- **correspondence** — a Rust test asserting the implementation matches the verified spec.
- **compile-time / unit** — the obligation is discharged by the type system or a unit test.
- **trusted-boundary** — an explicitly acknowledged assumption at an external boundary (e.g. the
  kernel's `io_uring` contract), stated rather than proven.

The volatile SCDAWG `Send`/`Sync` impls are in the `compile-time` / `correspondence` classes: the
handle contains no thread-hostile state, and `tests/unsafe_boundary_contracts.rs` exercises it under
concurrent reads.

## How drift is prevented

[`scripts/verify-unsafe-boundary-inventory.sh`](../../scripts/verify-unsafe-boundary-inventory.sh) is
a CI gate (a step in the `formal-*` jobs — see [engineering/testing-strategy.md](../engineering/testing-strategy.md))
that makes the inventory a **living** contract rather than stale documentation. It:

1. Rebuilds the live inventory by scanning `src/**/*.rs` for `unsafe impl` / `unsafe fn` / `unsafe {`.
2. Enforces **set-equality** against `UNSAFE_INVENTORY.tsv` (a `diff -u` fails on any drift) — so
   adding or removing an `unsafe` site without updating the inventory breaks CI.
3. Requires every inventory tag to resolve to a contract, and forbids orphan contracts.
4. Rejects any `src/persistent_*` contract whose status is not `covered` or `miri-wired` — persistent
   unsafe may not be merely asserted.

The practical consequence for a contributor: **you cannot add `unsafe` without also recording its
contract and coverage**, and you cannot let the record and the code diverge. That is what lets the
[threat model](threat-model.md) treat memory safety as a checked property rather than a hope.

## Related

- [architecture/in-memory-dictionaries.md](../architecture/in-memory-dictionaries.md) — why the
  volatile backends need almost no `unsafe`.
- [formal-verification/](../../formal-verification/README.md) — the proof corpus the contracts cite.
- [engineering/testing-strategy.md](../engineering/testing-strategy.md) — where the gate runs in CI.
