# Threat model

**Navigation**: [← Security](README.md)

This document states who the adversary is, what they control, and what is in and out of scope. It is
the frame for [untrusted-input.md](untrusted-input.md),
[deserialization-safety.md](deserialization-safety.md), and
[ffi-boundary.md](ffi-boundary.md). Notation follows [`docs/notation.md`](../notation.md).

## Assets

The process embedding libdictenstein wants to keep:

- **Availability** — the process should not be crashed (panic / abort) or wedged (OOM / stack
  overflow / live-lock) by data it feeds the library.
- **Memory safety** — no out-of-bounds read/write, use-after-free, or data race, regardless of input
  or concurrency.

libdictenstein holds no secrets of its own; **confidentiality and integrity of the stored terms are
the caller's concern** (see [README §What this is not](README.md#what-this-is-not)).

## The adversary and what they control

The adversary is whoever supplies data that reaches the library. Depending on how the embedding
application is wired, they may control one or more of these inputs:

| Input | Reaches | Adversary can… |
|-------|---------|----------------|
| **Term set** — the strings inserted into a dictionary | `from_terms`, `insert`, `insert_with_value`, … | choose adversarial keys (very long, deeply shared, high-codepoint) |
| **Query** — the string passed to a lookup | `contains`, `transition`, substring search | choose adversarial queries (very long, non-matching) |
| **Serialized blob** — bytes loaded into a dictionary | `deserialize` (feature `serialization` / `protobuf` / `compression`) | craft a malformed or hostile encoding |
| **Concurrency** — the schedule of concurrent calls | any `&self` method | race readers against writers |
| **Foreign ABI caller** — an independently compiled binary invoking the exported C surface (features `ffi` / `bindings-core`) | the 42 `ldict_*` entry points (`src/ffi.rs`); the exported `vt.dictionary.v1` resource vtables (`src/bindings.rs`) | pass null or malformed arguments and descriptors; misuse the retain/release ledger; forge snapshot node ids; abuse paging parameters (`start`, `capacity`); supply hostile persistence paths; drive snapshot/arena allocation; race calls against `free` |
| **Foreign vtable / resource** *(inbound, family-level)* | none in this crate — libdictenstein **produces** resources and consumes none | (defended by consumers; see the [family security model](https://github.com/vinary-tree/liblevenshtein-rust/blob/master/vinary-tree-interop/docs/security-model.md)) |

The adversary does **not** control the process's code, its other memory, or the filesystem beyond
what the application hands the library.

## Trust boundaries

<img src="../diagrams/security-trust-boundaries.svg" alt="Trust-boundary flow. Adversary-controlled inputs (red: term set, queries, serialized bytes, concurrent schedule) cross into libdictenstein along three edges: terms and queries into the memory-safe in-memory dictionaries (green), bytes into the fail-closed deserialize path (green), and schedule or bytes into the persistent open / WAL / checkpoint surface (blue). Only the persistent surface reaches the out-of-scope zone (slate: OS, disk, io_uring kernel path, and the liblevenshtein transducer) via syscalls. libdictenstein must fail safely at the boundary it owns." width="70%"/>

The library's job at each inbound boundary is to **fail safely** — return an `Err`/`Option`, or
bound the work — rather than corrupt memory, panic on attacker input, or consume unbounded
resources.

## In scope

- **Denial of service** via memory (OOM), time (super-linear blow-up), stack (deep recursion), or
  live-lock. Analyzed per input in [untrusted-input.md](untrusted-input.md).
- **Panics reachable from adversary-influenced input**, versus operations that return
  `Result`/`Option`. Enumerated in [untrusted-input.md §Panic surface](untrusted-input.md#panic-surface).
- **Memory-safety of `unsafe` code** under any input and any concurrent schedule. Bound to contracts
  in [unsafe-contracts.md](unsafe-contracts.md).
- **Deserialization** of adversarial encodings — parse-safety and allocation-sizing
  ([deserialization-safety.md](deserialization-safety.md)).
- **The exported ABI surface** — what a buggy or hostile foreign caller can cause through the
  `ldict_*` C ABI (host role: total validation, `catch_unwind` containment, thread-local errors) and
  through the exported resource vtables (producer role: ledger misuse, node-id forgery, paging
  abuse, path policy, exhaustion). Analyzed vector-by-vector in [ffi-boundary.md](ffi-boundary.md),
  which instantiates the
  [family security model](https://github.com/vinary-tree/liblevenshtein-rust/blob/master/vinary-tree-interop/docs/security-model.md)
  for this repository.

## Out of scope

- **The Levenshtein transducer.** Fuzzy matching lives in the companion crate
  [liblevenshtein](https://github.com/vinary-tree/liblevenshtein-rust); its query-side
  resource bounds are that crate's concern.
- **The operating system and disk.** The persistent engine trusts the kernel's `mmap` / `io_uring` /
  `fsync` semantics and the integrity of the underlying block device; a hostile *kernel* or a
  bit-flipping disk is not modeled (though torn-write *crash* recovery is — see
  [persistence](../persistence/README.md)).
- **Cross-key transactionality.** There is no multi-term atomic transaction to attack; each operation
  is individually linearizable ([design/volatile-concurrency.md](../design/volatile-concurrency.md)).
- **Side channels.** Timing/cache side channels on term contents are not defended; the structures are
  not constant-time and are not intended for secret-dependent lookups.

## Assurance

The safety claims are not merely asserted:

- **Machine-checked proofs** (Rocq) and **model checking** (TLA⁺/TLC) cover the persistent engine's
  concurrency and crash-recovery safety; see [formal-verification](../../formal-verification/README.md).
- **loom** exhaustively explores the lock-free interleavings; **ThreadSanitizer / AddressSanitizer /
  Miri** run the suite for races and UB; see [engineering/testing-strategy.md](../engineering/testing-strategy.md).
- The **`unsafe` inventory** is CI-gated for drift; see [unsafe-contracts.md](unsafe-contracts.md).
- The **binding contract gate** (`scripts/check-bindings.py`, CI job `binding-contract`) pins the
  exported ABI surface — symbol parity, status/kind/capability values, header identity — against
  the machine-readable model [`bindings/api.json`](../../bindings/api.json); the boundary's
  behavioral claims and their scheduled executable/formal pinning are tracked in
  [ffi-boundary.md §Verification status](ffi-boundary.md#verification-status).
