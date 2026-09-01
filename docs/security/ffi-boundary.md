# FFI boundary — the producer-side trust analysis

**Navigation**: [← Security](README.md) · [Threat model](threat-model.md) ·
[Bindings corpus](../bindings/README.md)

This document instantiates the
[family security model](https://github.com/vinary-tree/liblevenshtein-rust/blob/master/vinary-tree-interop/docs/security-model.md)
for libdictenstein's exported surfaces. The [threat model](threat-model.md)
analyzes the adversary who supplies *data* to a Rust caller; the `ffi` /
`bindings-core` features add a new input class — a **foreign ABI caller**: an
independently compiled binary invoking the 42 `ldict_*` functions
([reference](../bindings/c-abi-reference.md)) or the exported
`vt.dictionary.v1` vtables
([architecture](../bindings/resource-producer.md)). No compiler checks that
caller; every contract is enforced at run time or carried as a documented
proof obligation.

libdictenstein plays two roles at this boundary, with different defensive
postures:

1. **Host of the C ABI** — the `ldict_*` entry points. Posture: *total
   validation + total containment*. Every argument that can be checked is
   checked; every failure is a status; every panic is caught.
2. **Producer of retained resources** — the vtables behind
   [`ldict_dictionary_resource`](../bindings/c-abi-reference.md#ldict_dictionary_resource).
   Posture: *validate what is checkable, pin what is not*. Node ids and
   paging parameters are fully validated; the retain/release ledger and
   buffer honesty are **unverifiable from the producer side** and are pinned
   as consumer proof obligations instead.

## Who the foreign caller is — and is not

The caller runs **in the same process at the same privilege**. A genuinely
hostile caller already owns the address space; no library can defend against
its own process. The boundary analysis therefore targets the realistic
failure classes, in the family model's own framing:

- **buggy consumers** — facade bugs, miscounted retains, stale ids, undersized
  buffers;
- **hostile data flowing through correct consumers** — attacker-chosen terms,
  patterns, counts, and paths arriving via a well-behaved binding;
- **fault amplification** — a small consumer error must not escalate into
  producer-side undefined behavior when a status code can express it.

The assets defended are the threat model's: **availability** (no panic,
abort, wedge, or unbounded allocation from checkable input) and **memory
safety** (no out-of-bounds access, use-after-free, or data race from any
checkable input, under any schedule).

## Role 1 — hosting the C ABI

All 41 entry points share one containment spine
([reference § 3.2](../bindings/c-abi-reference.md#32-the-boundary-contract)):

| Defense | Mechanism | What it kills |
|---|---|---|
| Null totality | Every handle, input buffer (when `len > 0`), and out-parameter is null-checked before use; `ldict_dictionary_free(NULL)` is a no-op. | The classic null-deref crash class — reduced to `LDICT_STATUS_NULL_POINTER`. |
| Argument validation | Unit domains ∈ {1, 2, 3}; `has_value` ∈ {0, 1}; UTF-8 validated wherever the backend·domain requires it; empty persistence paths rejected. | Type-confusion downstream: no unvalidated discriminant ever reaches a `match`. |
| Panic containment | Every fallible body runs under `catch_unwind`; payloads become thread-local diagnostics and `LDICT_STATUS_PANIC`. | Unwinding across the FFI frame — undefined behavior territory — is unreachable through `ldict_*`. |
| Thread-local error channel | One diagnostic slot per thread; cleared on success, written on failure. | Cross-thread error smearing and TOCTOU on the diagnostic. |
| Out-parameter hygiene | Constructors null the out-handle first; other outputs are written only on `OK` (single documented exception: [`ldict_vocab_get_term`](../bindings/c-abi-reference.md#ldict_vocab_get_term)'s size/truncation protocol). | Consumers acting on uninitialized memory after ignoring a status. |

What this role deliberately does **not** do: verify that `data` really has
`len` readable bytes, that a handle pointer is genuinely a live
`LdictDictionary`, or that `free` is not racing another call on the same
handle. Those are C's irreducible preconditions — stated per function in the
reference — because no in-process check can validate a foreign pointer's
provenance.

## Role 2 — producing retained resources

The vtable surface is where unknown callers hold long-lived references into
producer memory. Vector by vector:

### Retain/release misuse

The ledger law (family canon, instantiated
[here](../bindings/resource-producer.md#7-the-retain-ledger--owneddictionaryresource)):
one owned retain per stored copy of the two words; one release per retain;
the count is a bare `Arc` strong count.

| Misuse | Effect | Duty |
|---|---|---|
| Releases **fewer** than retains (leak) | The `ResourceContext`, its payload, and any pinned revision stay allocated — an availability cost, linear in the leak. Memory safety unaffected. | Consumer (leak-discipline tests, family contract C9). |
| Release **without** a matching retain / release after zero / any vtable call after the last release | **Undefined behavior.** The strong count is a protocol, not a guard: the producer cannot distinguish the last legal release from an illegal one. | Consumer proof obligation — pinned by the family model; the producer's own guard (`OwnedDictionaryResource`) releases exactly the retain it was born with, so every producer-side path is balanced by construction. |
| Retain/release with a null context | Tolerated no-op (checked). | — |

The producer-side halves of the ledger — born-with-one-retain, exactly one
release in `Drop`, the `mem::forget` ownership transfer on snapshot export —
are single-assignment code paths in `src/bindings.rs`, auditable in one
sitting and scheduled for correspondence testing under the family plan's
`LDICT-LIFE-*` invariants (see the
[findings ledger](../bindings/FINDINGS_LEDGER.md), LDICT-B2).

### Node-id forgery

Node identifiers are indices into a snapshot-scoped, append-only,
mutex-guarded arena
([architecture § 4](../bindings/resource-producer.md#4-lazy-abi-local-node-ids--the-arena)).
A consumer may present any `u64` — stale, cross-snapshot, fabricated:

| Forged id | Producer behavior |
|---|---|
| ≥ arena length (including ids from a *larger* sibling snapshot) | Bounds check fails → `VT_STATUS_INVALID_ARGUMENT`. No access occurs. |
| < arena length but from another revision | Names a **different, valid node of this revision** — a consumer-side correctness bug. The walk stays memory-safe: every access is a checked chunk/slot lookup. Distinct resources with the same negotiated snapshot identity deliberately share one id namespace. |
| Any id after the snapshot's last release | Use-after-free of the *context* — the retain/release UB class above, not an id-validation issue. |
| Exceeding `usize` on narrow targets | Checked `usize::try_from` → `InvalidArgument`. |

Design note: this containment is exactly why the ABI exports dense indices
instead of node addresses. A pointer-shaped id would make every forgery a
potential memory-safety event; an index-shaped id makes forgery at worst a
wrong answer to the forger.

### Paging-parameter abuse

`node_edges(node, start, out_edges, capacity, out_written, out_total)` takes
two attacker-influenceable magnitudes:

- **`start` beyond the edge count** — the page iterator (`skip(start)`)
  yields nothing: `out_written = 0`, `out_total` correct, status `OK`. No
  arithmetic on `start` can overflow into the buffer (writes are indexed
  `0..written`, never by `start`).
- **Huge `capacity`** — the producer writes at most
  $`\min(\mathrm{capacity},\ \mathrm{total} - \mathrm{start})`$ entries,
  which is bounded by the node's real degree regardless of the claimed
  capacity. A `capacity` **larger than the consumer's actual buffer** is
  therefore dangerous only when the node's remaining degree also exceeds the
  real buffer — and buffer honesty is unverifiable across an ABI: the
  producer's guarantee is *never write more than `capacity` entries*; sizing
  the allocation truthfully is the consumer's precondition, stated on the
  vtable contract.
- **`capacity != 0` with null `out_edges`** — rejected: `NULL_POINTER`.

### Resource exhaustion

Three producer allocations are consumer-drivable; each is metered so that
cost tracks the consumer's own effort — there is no small-request/large-cost
amplifier:

| Vector | Growth law | Bound |
|---|---|---|
| Snapshot flooding (`snapshot` in a loop) | One small `ResourceContext` per retained call; all captures of the same source revision share one memoized `TraversalSnapshot`, pinned revision, and warmed chunked arena. | Linear only in retained handle contexts; graph and traversal-cache memory are constant per source revision. Host still bounds retained-handle counts as it would any allocation API. |
| Arena growth (exhaustive walking) | Expanding node $`v`$ appends $`\deg(v)`$ entries — so after expanding the set $`E`$: $`\lvert \mathrm{arena} \rvert = 1 + \sum_{v \in E} \deg(v)`$, one entry per **edge the consumer traversed**, at ≥ 1 ABI call per expansion. Note the arena unfolds the DAG into its trie view, so full walks of suffix-sharing dictionaries cost the *unfolded* size — the walker pays it call by call. | Proportional to consumer work; `LimitExceeded` guards the id space itself. |
| Diagnostic strings | One thread-local `CString`, overwritten per failure. | $`\Theta(1)`$ per thread. |

Unbounded *dictionary* growth via `insert` is the threat model's existing
term-set row — nothing ABI-specific; the host bounds corpus size.

### Persistence paths

`ldict_persistent_artrie_{create,open}`, `ldict_persistent_vocab_{create,open}`,
and `ldict_dictionary_checkpoint` operate on **verbatim** UTF-8 paths: no
canonicalization, no symlink or `..` traversal defense, no sandbox
([reference § 13](../bindings/c-abi-reference.md#13-persistence-path-caveats)).
A hostile path is a hostile *filesystem instruction* executed with the
process's privileges — creating store directories, WAL files, and checkpoint
manifests wherever it points, or slow-reading special files.

This is a deliberate scope line, identical to the threat model's stance on
the OS: the library cannot know the host's path policy. **Duty: the host.**
Hosts deriving store paths from external input should canonicalize, then
enforce an allowlisted prefix, before the path reaches the ABI. What the
library does guarantee: the path bytes are validated UTF-8, emptiness is
rejected, engine-level failures surface as `IO_ERROR` with diagnostics rather
than panics, and same-path open races are arbitrated to a status, not a
corruption.

### Concurrency schedule

Every exported resource claims `PARALLEL_REENTRANT`, so consumers will call
concurrently without gates — the claim is honored by construction
(internally synchronized engines; per-call locks; **no consumer callbacks
under any producer lock**, which removes the cross-binary deadlock class by
shape). The residual caller duties are C-standard: do not race
`ldict_dictionary_free` against other calls on the same handle, and do not
call anything after the last release. Both are ordering obligations no
in-process producer can enforce.

## The duty split

| Law | Producer discharges by… | Consumer discharges by… | Host discharges by… |
|---|---|---|---|
| Ledger balance | Balanced guard paths by construction (`new`/`Drop`/`forget`-transfer). | One retain per stored copy; one release each; nothing after zero. | — |
| Id validity | Bounds + narrowing checks on every access. | Using ids only against their snapshot, only while retained. | — |
| Paging | `written ≤ capacity`; stable totals; lossless pages. | Truthful `capacity`; buffers sized to claim. | — |
| Panic/fault channel | `catch_unwind` on `ldict_*`; panic-free vtables (abort, never unwind, as the last resort — [architecture § 9](../bindings/resource-producer.md#9-fault-and-panic-containment-producer-side)). | Treating every status as reachable; never interpreting `PANIC` as success. | — |
| Path policy | UTF-8 + non-empty validation; status-mapped engine failures. | — | Canonicalize + allowlist externally influenced paths. |
| Resource ceilings | Cost proportional to calls; no amplification. | — | Rate/size limits on untrusted call sources. |

## Verification status

The containment claims above are code-derived (every claim cites its
function or module) and their executable pinning is scheduled work under the
family plan: the W2 producer FFI suites (status matrix, paging property
tests, snapshot-law, concurrent stress — tracked as
[LDICT-B2](../bindings/FINDINGS_LEDGER.md)) and the W2 formal items
(`AbiProducerSnapshot` TLA⁺ model, traversal-snapshot and paging Rocq specs,
`LDICT-LIFE-*` refcount correspondence). The structural gate
`scripts/check-bindings.py` (CI job `binding-contract`) already pins the
surface itself: symbol parity, enum values, and header identity cannot drift
silently. Language-level guarantees for the safe interior rest on the
RustBelt line of work [[2]](#references); the boundary contracts here are
precisely the obligations that sit *outside* what the type system can carry
across an ABI.

## References

DOIs verified resolving 2026-08-08 (`curl -sIL` / Crossref metadata match).

1. G. E. Collins. "A Method for Overlapping and Erasure of Lists."
   *Communications of the ACM* 3(12), 1960 — the reference-counting ledger.
   [DOI:10.1145/367487.367501](https://doi.org/10.1145/367487.367501)
2. R. Jung, J.-H. Jourdan, R. Krebbers, D. Dreyer. "RustBelt: Securing the
   Foundations of the Rust Programming Language." *PACMPL* 2(POPL), 2018.
   [DOI:10.1145/3158154](https://doi.org/10.1145/3158154)
3. J. R. Driscoll, N. Sarnak, D. D. Sleator, R. E. Tarjan. "Making Data
   Structures Persistent." *JCSS* 38(1), 1989 — why pinned revisions make
   snapshot walking safe under concurrent mutation.
   [DOI:10.1016/0022-0000(89)90034-2](https://doi.org/10.1016/0022-0000(89)90034-2)

## Family documents

- [Family security model — the canonical trust model this document instantiates](https://github.com/vinary-tree/liblevenshtein-rust/blob/master/vinary-tree-interop/docs/security-model.md)
- [ABI reference — `vinary_tree_interop.h`, annotated](https://github.com/vinary-tree/liblevenshtein-rust/blob/master/vinary-tree-interop/docs/abi-reference.md)
- [ABI evolution policy](https://github.com/vinary-tree/liblevenshtein-rust/blob/master/vinary-tree-interop/docs/abi-evolution.md)
