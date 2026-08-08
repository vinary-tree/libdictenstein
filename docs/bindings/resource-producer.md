# The resource producer — how libdictenstein exports `vt.dictionary.v1`

**Navigation**: [← Bindings corpus](README.md) ·
[C ABI reference](c-abi-reference.md) ·
[FFI boundary analysis](../security/ffi-boundary.md)

This document explains the **producer half** of the family dictionary
contract: how the module behind the `bindings-core` feature
([`src/bindings.rs`](../../src/bindings.rs)) turns four very different
dictionary engines into one uniform, retained, snapshot-capable two-word
resource that independently compiled consumers can walk. The consumer half —
cursors, leases, query semantics — lives in liblevenshtein and is documented
there (see the [family documents](#family-documents)).

The design premise, stated once in the module header and enforced
everywhere: **concrete dictionaries and their CRUD stay in this crate;
consumers receive a small project-neutral resource.** Capturing a query
revision clones an immutable root in $`\mathcal{O}(1)`$; nodes acquire
ABI-local identifiers only when a consumer traverses their incoming edge.

---

## 1. Terms

| Term | Definition |
|---|---|
| binding | A cheaply clonable `Arc`-shared wrapper (`DynamicDawgBinding`, `DoubleArrayTrieBinding`, `ScdawgBinding`, `PersistentARTrieBinding`) exposing one engine's CRUD to `src/ffi.rs` and producing resources. |
| payload | The `ResourcePayload` variant a resource context carries: `Live` (mutable DynamicDAWG), `Secondary` (DAT or SCDAWG), `Persistent` (ARTrie family), or `Snapshot` (a captured revision). |
| revision | One immutable logical value of a dictionary. Mutable backends *publish* successor revisions; they never edit a published one in place. |
| capture | Producing a `Snapshot` payload from any other payload: clone the current revision's root handle, allocate a one-slot arena — no traversal, no copy. |
| arena | The `TraversalSnapshot`'s append-only table mapping ABI-local node ids to engine node handles (plus each node's materialized edge list). |
| ABI-local node id | A `u64` index into one snapshot's arena. Meaningful only for that snapshot, only while it is retained. |
| retain ledger | The `Arc<ResourceContext>` strong count: one owned retain per stored copy of the two words (Collins [[1]](#references)). |

---

## 2. Architecture

<img src="../diagrams/abi-producer-component.svg" alt="Component diagram of the libdictenstein producer stack. At the top, inside a red trust-boundary rectangle, sit the foreign consumers: the liblevenshtein transducer, the duallity WFST constructor, and any C-ABI or facade caller. They call into the green libdictenstein cdylib package: the C ABI layer (35 ldict_* functions with catch_unwind and a thread-local last error, owning LdictDictionary handles) which fans out to the four producer bindings — DynamicDawgBinding over RwLock(DynamicBackend), DoubleArrayTrieBinding and ScdawgBinding over Arc(SecondaryBackend), and PersistentARTrieBinding over Arc(PersistentBackend) — each wrapping its dictionary core. Every binding produces an OwnedDictionaryResource (drawn in the green handle color, born with one retain) which holds an Arc(ResourceContext) whose strong count is the retain ledger. The context creates TraversalSnapshot arenas (amber leased color: lazy, append-only ABI-local ids) via O(1) revision capture, and query_interface selects one of the eleven 'static vtables (RESOURCE_VTABLE plus ten VtDictionaryVTable instances by domain and flags). The OwnedDictionaryResource exports the two borrowed words as a VtResource conforming to the pink vinary-tree-interop family contract at the bottom; consumers call retain, release, query_interface, and the node-walk operations against it across the trust boundary." width="100%"/>

Five layers, from the metal up:

1. **Engines** — the dictionaries themselves
   ([DynamicDAWG](../algorithms/implementations/dynamic-dawg.md),
   [DoubleArrayTrie](../algorithms/implementations/double-array-trie.md),
   [SCDAWG](../theory/scdawg/README.md),
   [persistent ARTrie](../persistence/README.md)). All are internally
   synchronized; readers are lock-free on the in-memory cores.
2. **Backend enums** — `DynamicBackend` (byte/Unicode/u64 DAWG),
   `SecondaryBackend` (DAT + SCDAWG in both text domains), and
   `PersistentBackend` (byte/Unicode/u64/vocabulary ARTrie) erase the
   per-domain generics behind one `snapshot()`/`len()`/`domain()` seam.
3. **Bindings** — the four public structs `src/ffi.rs` dispatches into; each
   is `Clone` (an `Arc` bump) and each has `resource() → OwnedDictionaryResource`.
4. **Resource machinery** — `ResourceContext` (payload + domain + flags),
   `TraversalSnapshot` (the arena), `OwnedDictionaryResource` (the retain
   guard), and eleven `'static` vtables.
5. **The exported words** — a `VtResource { context, vtable }` whose vtable
   pointers live in the producer's read-only data for the process lifetime.

### 2.1 Why the DynamicDAWG binding holds an `RwLock`

`SharedDictionary` wraps its `DynamicBackend` in an `RwLock` even though the
DAWG cores are internally synchronized. The lock is **not** protecting CRUD —
every CRUD path takes the *read* side and relies on the engine's own
synchronization. The single write-side user is
[`ldict_dictionary_clear`](c-abi-reference.md#ldict_dictionary_clear), which
implements "remove everything" by **replacing the whole backend value** with a
fresh empty engine of the same domain: an atomic swap of the published
dictionary, after which readers see the empty revision and pre-existing
snapshots keep the old one. Lock poisoning is neutralized everywhere with
`unwrap_or_else(PoisonError::into_inner)` — a panicking writer elsewhere in
the process cannot brick the resource surface.

---

## 3. Snapshot capture is $`\mathcal{O}(1)`$ — per backend

The interop contract is explicit: *implementations must make `snapshot`
$`\mathcal{O}(1)`$ with structural sharing or an equivalent immutable
revision; copying the whole dictionary or holding a long-lived read lock
violates the interface contract.* Capture in this crate is one code path —

```math
\text{capture}(\mathit{payload}) \;=\;
\text{TraversalSnapshot::new}\bigl(\text{root}(\mathit{payload}),\
\text{len},\ \text{domain},\ \text{suffix}\bigr)
```

— allocating an arena of exactly one entry
(`nodes = [root]`, `edges = [None]`), so its cost is the cost of `root()`
plus one small allocation:

```math
T_{\text{capture}} \;=\; T_{\text{root}} + \Theta(1),
\qquad T_{\text{root}} = \Theta(1) \text{ for every backend below.}
```

What makes `root()` constant-time differs per engine, and each mechanism is
an instance of persistent-data-structure theory (Driscoll et al.
[[2]](#references); Okasaki [[3]](#references)):

| Backend | Revision mechanism | Why `root()` is $`\Theta(1)`$ | What keeps the revision alive |
|---|---|---|---|
| **DynamicDAWG** (`Live`) | Immutable revisions with atomic root publication: every mutation builds new structure and publishes a new root; published structure is never edited. | Load the current root handle — a reference-counted pointer read under the binding's read lock (held only for the call, never across it). | The root handle: reference-counted structural sharing keeps everything reachable from it alive, however far the live dictionary moves on. |
| **DoubleArrayTrie** (`Secondary`) | The whole trie is one frozen revision — read-only after construction, by contract. | Return the root cursor over the shared `Arc`'d base/check arrays. | The arena's node handles share the `Arc` of the trie itself. |
| **SCDAWG** (`Secondary`) | Insert-only graph; a root view over the current node table. | Return the root node view. | The node handles keep the shared structure reachable. |
| **Persistent ARTrie** (`Persistent`) | Copy-on-write revisions over the lock-free overlay (path-copying — the on-disk WAL/checkpoint machinery sits *below* this and is invisible to capture). | Load the current revision root from the overlay. | The revision root pins its copy-on-write structure in memory; eviction and checkpointing never mutate captured paths. |
| **Snapshot** (re-capture) | Already immutable. | `Arc::clone` of the existing `TraversalSnapshot` — the **self-snapshot law**: snapshotting a snapshot yields a resource sharing the same arena. | Itself. |

Two consequences worth internalizing:

- **Capture never blocks writers, and writers never invalidate captures.**
  There is no lock held for the snapshot's lifetime — the pin is ownership,
  not exclusion.
- **`ldict_dictionary_free` does not end a snapshot.** The arena's handles own
  the revision; the handle's death merely releases the handle's own retain
  (see [§ 7](#7-the-retain-ledger--owneddictionaryresource)).

The empirical counterpart: the run-verified example in the
[C ABI reference § 15](c-abi-reference.md#15-a-complete-verified-c-example)
observes a pre-removal revision through a snapshot while the live dictionary
reports the term gone.

---

## 4. Lazy ABI-local node ids — the arena

A dictionary node crossing an ABI must become a plain integer: engine node
handles are Rust types (generic, lifetime-bearing, non-FFI-safe), and leaking
raw pointers would weld consumers to engine internals and make forgery
memory-unsafe. The producer therefore *names* nodes on demand:

```text
TraversalSnapshot<N> {
    arena: Mutex<NodeArena<N>>,   # nodes: Vec<N>, edges: Vec<Option<Vec<(label, id)>>>
    len, domain, suffix,
}

id 0 ↦ root                                   # assigned at capture

ensure_edges(node):                            # on first expansion of `node`
    if node ≥ |nodes|: return InvalidArgument  # bounds check — forged ids die here
    if edges[node] is Some: return             # already materialized — idempotent
    for (label, child) in nodes[node].edges():
        id ← |nodes|                           # append-only: ids never reused
        push nodes, child;  push edges, None
        record (label.to_abi(), id)
    edges[node] ← Some(recorded)
```

`node_transition` and `node_edges` both funnel through `ensure_edges`;
`node_is_final` and `node_value_u64` only index the arena. Every operation
takes the arena mutex for the duration of one call — and **no consumer code
ever runs under that mutex** (the vtable makes no callbacks), so the lock
cannot participate in a cross-binary deadlock.

Properties, each load-bearing:

- **Ids are snapshot-scoped.** An id indexes *this* snapshot's `Vec`. Another
  snapshot — even of the same dictionary at the same instant — has its own
  arena and its own numbering. Using an id against the wrong snapshot is
  either out of range (`InvalidArgument`) or silently names a different node:
  a correctness bug on the consumer's side of the trust boundary, never a
  memory-safety event on the producer's (see
  [ffi-boundary.md](../security/ffi-boundary.md#node-id-forgery)).
- **Ids are append-only and stable.** Once assigned, an id never moves or
  gets reused for the snapshot's lifetime — consumers may cache them freely
  while the snapshot is retained.
- **Assignment is per traversed edge, not per engine node.** A DAWG shares
  suffixes, so the same engine node reached along two paths receives two ids:
  the ABI view is the trie *unfolding* of the DAG. Consumers walk a tree;
  producers store a DAG. (A consumer that wants DAG-sharing back can hash on
  its side; the ABI deliberately does not promise node identity.)
- **Memory grows only with consumer work.** After capture the arena holds one
  entry; each first-time expansion of a node $`v`$ appends exactly
  $`\deg(v)`$ entries. After expanding the set $`E`$ of nodes:

  ```math
  \lvert \mathrm{arena} \rvert \;=\; 1 + \sum_{v \in E} \deg(v)
  ```

  — one arena entry per edge the consumer actually traversed, allocated by
  the consumer's own calls. The producer never pre-materializes anything.
  (Exhaustion economics: the consumer pays at least one ABI call per
  expansion; see the [boundary analysis](../security/ffi-boundary.md#resource-exhaustion).)
- **The id space cannot overflow silently.** Ids are minted with a checked
  `u64::try_from(nodes.len())`; the failure arm answers `LimitExceeded`
  (unreachable on 64-bit hosts, where `usize` fits in `u64` — the check is
  defensive portability, not dead weight). Incoming ids are narrowed with
  `usize::try_from` and bounds-checked before use.

### 4.1 Only snapshots are walkable

`root`, `len`, `node_is_final`, `node_value_u64`, `node_transition`, and
`node_edges` all begin with `context.immutable()` — a payload check that
answers `InvalidArgument` unless the resource **is** a `Snapshot`. The
exported resource of a live dictionary is a *snapshot factory*, not a
walkable graph: the only meaningful traversal-adjacent operation on it is
`snapshot` itself. This is what makes the query-start boundary law
enforceable at the type level of the protocol — there is no API through which
a consumer can accidentally walk a moving revision. The `IMMUTABLE` flag
([§ 6](#6-the-flag-truth-table)) is the discoverable marker of walkability.

---

## 5. The eleven `'static` vtables

`query_interface` never allocates: it validates the 16-byte interface id and
the minimum version, then hands back a pointer to one of eleven immutable
`'static` structs — the base `RESOURCE_VTABLE` (retain/release/query_interface)
plus ten `VtDictionaryVTable` instances covering the reachable
(domain × immutable × suffix) combinations:

| | Byte | UnicodeScalar | U64 |
|---|---|---|---|
| live | `BYTE_LIVE` | `UNICODE_LIVE` | `U64_LIVE` |
| live · suffix | `BYTE_SUFFIX_LIVE` | `UNICODE_SUFFIX_LIVE` | — coalesces to `U64_LIVE`¹ |
| snapshot | `BYTE_SNAPSHOT` | `UNICODE_SNAPSHOT` | `U64_SNAPSHOT` |
| snapshot · suffix | `BYTE_SUFFIX_SNAPSHOT` | `UNICODE_SUFFIX_SNAPSHOT` | — coalesces to `U64_SNAPSHOT`¹ |

¹ No `u64` suffix engine exists (the SCDAWG is text-only); the dispatch match
tolerates the combination defensively rather than leaving an unreachable arm.

Because the vtables are `'static`, a consumer holding a stale vtable pointer
after over-releasing a resource still cannot be led through a dangling
*vtable* — the failure surface is confined to the context word. Every
`VtDictionaryVTable` declares `struct_size`, `interface_version = 1`,
`value_domain = OptionalU64`, and its `unit_domain`/`flags`; all seven
operation slots are populated — a `vt.dictionary.v1` consumer never needs a
null-slot fallback path against this producer.

---

## 6. The flag truth table

Flags come from `ResourceContext::flags()` and are baked into the selected
static vtable. Read directly from the source (and pinned by the unit test
`every_project_owned_resource_is_parallel_and_reentrant`):

| Resource | `PARALLEL_REENTRANT` | `SUFFIX_BASED` | `IMMUTABLE` |
|---|:-:|:-:|:-:|
| DynamicDAWG (live) | ✔ | — | — |
| DoubleArrayTrie (live) | ✔ | — | — |
| SCDAWG (live) | ✔ | ✔ | — |
| Persistent ARTrie / vocabulary (live) | ✔ | — | — |
| Snapshot of any non-suffix source | ✔ | — | ✔ |
| Snapshot of an SCDAWG | ✔ | ✔ | ✔ |

Three facts the table encodes:

1. **`PARALLEL_REENTRANT` is universal — and it is a *claim*, honored by
   construction.** Consumers seeing this bit skip their serializing call
   gates. The producer earns it: engine reads are internally synchronized,
   the DynamicDAWG binding's `RwLock` is held only within a call, and arena
   access serializes on its own mutex without ever calling out. Nothing in
   the producer requires callers to serialize.
2. **`SUFFIX_BASED` marks semantics, not structure.** A suffix-flagged
   resource indexes substrings; a Levenshtein consumer must interpret finality
   as substring-match acceptance. The flag survives capture — a snapshot of an
   SCDAWG stays suffix-based.
3. **`IMMUTABLE` means "this resource *is* a snapshot"** — walkable, and
   self-snapshotting in $`\mathcal{O}(1)`$ by arena sharing. Deliberately, a
   live DoubleArrayTrie resource does **not** carry `IMMUTABLE` even though
   the trie is frozen: `IMMUTABLE` advertises the walkability contract of
   [§ 4.1](#41-only-snapshots-are-walkable), not the engine's mutability. A
   DAT capture is a fresh one-slot arena either way — still $`\Theta(1)`$.

---

## 7. The retain ledger — `OwnedDictionaryResource`

`OwnedDictionaryResource` is the producer's ownership guard around the raw
two words. Its whole API is three moments:

- **`new(payload)`** — wrap the payload in an `Arc<ResourceContext>`,
  `Arc::into_raw` it into the context word, pair it with `&RESOURCE_VTABLE`.
  The `Arc`'s initial strong count of 1 **is** the born-with-one-retain law.
- **`as_raw()`** — copy out the two words, retaining nothing: the
  copy-not-retain law made explicit. `ldict_dictionary_resource` returns
  exactly this borrow.
- **`Drop`** — one `release`. The guard releases precisely the retain it was
  born with, never more.

`retain`/`release` themselves are two lines each:
`Arc::increment_strong_count` / `Arc::decrement_strong_count` on the context
pointer — the entire cross-binary reference-counting protocol reduces to the
`Arc` ledger (Collins [[1]](#references)), with null-context calls tolerated
as no-ops.

One transfer is special: `dictionary_snapshot` constructs the snapshot's
guard, writes its raw words to the consumer's out-parameter, and then
`std::mem::forget`s the guard. The born retain is neither dropped nor
duplicated — its *ownership moves across the ABI* to the consumer, which is
why a returned snapshot "arrives owning one retain" and must be released
exactly once. The full state space:

<img src="../diagrams/owned-resource-lifecycle-state.svg" alt="State diagram of the OwnedDictionaryResource lifecycle. From the initial state, OwnedDictionaryResource::new (handle construction or resource()) enters ProducerOwned with strong count 1 — born with one retain, drawn in the green handle color. as_raw() self-loops on ProducerOwned as a borrow with no retain (copy-not-retain). A foreign vtable retain moves to CoOwned (strong = 1 + n, amber lease color); further retains and releases self-loop there; the last foreign release returns to ProducerOwned. Dropping the guard from CoOwned (ldict_dictionary_free) moves to ForeignOwned, where every remaining retain is foreign-held — the state that lets retained resources outlive the dictionary handle. dictionary_snapshot's std::mem::forget hands the born retain directly from ProducerOwned to ForeignOwned. From ProducerOwned or ForeignOwned, the last release reaches Destroyed: the ResourceContext drops, the payload Arc releases, and the backend is freed with its last co-owner. A red warning note marks everything after zero — release without retain or any call after the last release — as undefined behavior the producer cannot detect, making the ledger invariant (releases equal retains, never exceeding them mid-flight) the consumer's proof obligation." width="90%"/>

The ledger's safety split is sharp and worth stating twice (the
[boundary analysis](../security/ffi-boundary.md) owns the adversarial
version): **leaks are availability bugs; over-releases are undefined
behavior.** The producer can survive any number of forgotten releases (memory
stays reachable) but cannot detect a release-after-zero — `Arc`'s count is a
protocol, not a guard.

---

## 8. How a new backend joins the ABI

The checklist, in dependency order — "done" means every gate below is green:

1. **Engine seam.** Give the engine a $`\Theta(1)`$ `root()` over an immutable
   revision (publish-don't-edit), plus $`\Theta(1)`$ `len()`/`domain()`. If its
   nodes implement `MappedDictionaryNode` with `Unit: AbiUnit`
   (`u8`/`char`/`u64`) and `Value: AbiValue`, the generic `TraversalSnapshot`
   works unchanged; otherwise implement `SnapshotOps` by hand (same laws:
   arena semantics, append-only ids, no callbacks under the lock).
2. **Payload + binding.** Add the backend to an existing payload enum (or a
   new `ResourcePayload` variant + `flags()`/`domain()`/`snapshot()` arms) and
   write the public `*Binding` struct: `Arc`-shared, `Clone`,
   `resource() → OwnedDictionaryResource`.
3. **`LdictBinding` arms** in `src/ffi.rs`: `kind()`, `capabilities()`,
   `len()`, `resource()`, and every CRUD family — choosing `Unsupported`
   (operation family absent) versus `DomainMismatch` (wrong term
   representation) per the
   [semantic rule](c-abi-reference.md#53-what-unsupported-vs-domain_mismatch-mean).
4. **Constants.** New `LDICT_KIND_*` in `src/ffi.rs` **and**
   `include/libdictenstein.h`; new capability bits only for genuinely new
   operation families. New `ldict_*` functions (if any) bump
   `LDICT_API_REVISION` — additive only, per the
   [evolution policy](https://github.com/vinary-tree/liblevenshtein-rust/blob/master/vinary-tree-interop/docs/abi-evolution.md).
5. **Flags honesty.** Claim `PARALLEL_REENTRANT` only if every path is
   internally synchronized; set `SUFFIX_BASED` for substring semantics; extend
   the `dictionary_vtable()` dispatch (new `'static` vtables) if a new
   domain × flag combination becomes reachable.
6. **Model.** `bindings/api.json`: a `backends[]` row (kind, unit domains,
   capabilities), the `kinds.values` entry, a `producers` entry, and any new
   `cFunctions` rows.
7. **Gate.** `python3 scripts/check-bindings.py` green — it cross-checks
   model ↔ `src/ffi.rs` ↔ header ↔ all 13 facades, so missing constants or
   facade drift fail here, in CI job `binding-contract`.
8. **Tests.** Extend the FFI suites: the status matrix (capability-derived
   `Unsupported`/`DomainMismatch` arms for the new kind), the snapshot-law
   test, paging over the new backend's node degrees, and the flag pin in
   `src/bindings.rs`.
9. **Docs.** Kind table + capability matrix + text-acceptance row in the
   [C ABI reference](c-abi-reference.md); a revision-mechanism row in
   [§ 3](#3-snapshot-capture-is-mathcalo1--per-backend); facade constants
   regenerate from the model.

---

## 9. Fault and panic containment, producer side

Two surfaces, two disciplines — documented honestly because they differ:

- **The `ldict_*` surface** runs every fallible body inside
  `boundary()` = `catch_unwind` + thread-local diagnostics. A panic becomes
  `LDICT_STATUS_PANIC` plus a message; it never unwinds into a foreign frame.
- **The vtable surface** (`resource_retain`, `resource_release`,
  `query_interface`, `dictionary_*`) is **panic-free by construction** rather
  than unwind-caught: poisoned locks are recovered with
  `PoisonError::into_inner`, integer narrowing uses checked `try_from` with
  status-mapped failure arms, arena access is bounds-checked (`get`, explicit
  length comparison), out-pointers are null-checked before any write, and no
  path contains `unwrap`/`expect`/indexing-by-faith. Should a panic
  nevertheless occur (e.g. allocator failure while growing the arena), Rust's
  `extern "C"` unwind semantics **abort the process** — a crash with a
  backtrace, never undefined behavior unwinding into a foreign caller. The
  asymmetry is deliberate: the vtable operations sit on hot traversal paths
  where a `catch_unwind` frame per node call is measurable, and their bodies
  are small enough to audit exhaustively for panic-freedom.

What the producer does *not* contain — misuse of the ledger itself
(release-after-zero, forged ids, paging abuse) — is quantified vector by
vector in the [FFI boundary analysis](../security/ffi-boundary.md).

---

## References

DOIs verified resolving 2026-08-08 (`curl -sIL` / Crossref metadata match).

1. G. E. Collins. "A Method for Overlapping and Erasure of Lists."
   *Communications of the ACM* 3(12), 1960 — reference counting: the retain
   ledger's ancestry.
   [DOI:10.1145/367487.367501](https://doi.org/10.1145/367487.367501)
2. J. R. Driscoll, N. Sarnak, D. D. Sleator, R. E. Tarjan. "Making Data
   Structures Persistent." *JCSS* 38(1), 1989 — structural sharing and
   revision persistence, the theory behind $`\mathcal{O}(1)`$ capture.
   [DOI:10.1016/0022-0000(89)90034-2](https://doi.org/10.1016/0022-0000(89)90034-2)
3. C. Okasaki. *Purely Functional Data Structures.* Cambridge University
   Press, 1998 — publish-don't-edit as a design discipline.
   [DOI:10.1017/CBO9780511530104](https://doi.org/10.1017/CBO9780511530104)

Backend-specific theory citations (Aoe's double array; Blumer et al.'s
SCDAWG) are kept with the per-backend sections of the
[C ABI reference](c-abi-reference.md#references) and the
[theory corpus](../theory/).

## Family documents

- [ABI reference — `vinary_tree_interop.h`, annotated](https://github.com/vinary-tree/liblevenshtein-rust/blob/master/vinary-tree-interop/docs/abi-reference.md)
- [ABI evolution policy — the four version counters](https://github.com/vinary-tree/liblevenshtein-rust/blob/master/vinary-tree-interop/docs/abi-evolution.md)
- [Family security model — trust zones and validation duties](https://github.com/vinary-tree/liblevenshtein-rust/blob/master/vinary-tree-interop/docs/security-model.md)
- [liblevenshtein language-binding architecture (the consumer side)](https://github.com/vinary-tree/liblevenshtein-rust/blob/master/docs/language-bindings.md)
