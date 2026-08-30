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
ABI-local identifiers only when a callback-fallback consumer traverses their
incoming edge. DynamicDAWG snapshots may instead publish one compact immutable
graph for the whole revision on first request.

---

## 1. Terms

| Term | Definition |
|---|---|
| binding | A cheaply clonable `Arc`-shared wrapper (`DynamicDawgBinding`, `DoubleArrayTrieBinding`, `ScdawgBinding`, `PersistentARTrieBinding`) exposing one engine's CRUD to `src/ffi.rs` and producing resources. |
| payload | The `ResourcePayload` variant a resource context carries: `Live` (mutable DynamicDAWG), `Secondary` (DAT or SCDAWG), `Persistent` (ARTrie family), or `Snapshot` (a captured revision). |
| revision | One immutable logical value of a dictionary. Mutable backends *publish* successor revisions; they never edit a published one in place. |
| capture | Producing a `Snapshot` payload from any other payload: reuse the source revision's memoized snapshot or clone its current root handle and allocate a one-slot fallback arena plus empty graph publication cells — no traversal, no dictionary copy. |
| identity | The optional `(producer, revision)` token exposed only by immutable snapshots. Equal tokens mean equal pinned revisions and permit consumers to share derived caches. |
| compact graph | The optional `vt.dict.graph.v1` flat node/edge projection. DynamicDAWG snapshots construct it lazily once per revision; consumers traverse it without per-node callbacks. |
| arena | The `TraversalSnapshot`'s append-only, 256-slot-chunked table mapping ABI-local node ids to engine node handles (plus each node's write-once materialized edge list). |
| ABI-local node id | A `u64` index into one snapshot's arena. Meaningful only for that snapshot, only while it is retained. |
| retain ledger | The `Arc<ResourceContext>` strong count: one owned retain per stored copy of the two words (Collins [[1]](#references)). |

---

## 2. Architecture

<img src="../diagrams/abi-producer-component.svg" alt="Component diagram of the libdictenstein producer stack. At the top, inside a red trust-boundary rectangle, sit the foreign consumers: the liblevenshtein transducer, the duallity WFST constructor, and any C-ABI or facade caller. They call into the green libdictenstein cdylib package: the C ABI layer (42 ldict_* functions with catch_unwind and a thread-local last error, including bounded entry snapshot cursors, native dictionary algebra, and reduction, owning LdictDictionary handles) which fans out to the four producer bindings — DynamicDawgBinding over a fixed-domain DynamicBackend with inner GraphVersion CAS, DoubleArrayTrieBinding and ScdawgBinding over Arc(SecondaryBackend), and PersistentARTrieBinding over Arc(PersistentBackend) — each wrapping its dictionary core. Every binding produces an OwnedDictionaryResource (drawn in the green handle color, born with one retain) which holds an Arc(ResourceContext) whose strong count is the retain ledger. The context creates TraversalSnapshot values with a lazy compact-graph publication path and a fallback append-only ABI-local-id arena via O(1) revision capture. query_interface selects the static base, dictionary, visit, compact-graph, and snapshot-identity vtables. The OwnedDictionaryResource exports the two borrowed words as a VtResource conforming to the pink vinary-tree-interop family contract at the bottom; consumers call retain, release, query_interface, and either compact-graph or node-walk operations against it across the trust boundary." width="100%"/>

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
   `SnapshotMemo` (one warmed snapshot per source revision plus lock-free
   seqlock-style capture validation),
   `TraversalSnapshot` (revision root, graph once-cells, and chunked fallback
   arena), `OwnedDictionaryResource` (the retain guard), and the `'static`
   base/dictionary/visit/graph/identity vtables.
5. **The exported words** — a `VtResource { context, vtable }` whose vtable
   pointers live in the producer's read-only data for the process lifetime.

### 2.1 Why the DynamicDAWG binding needs no outer lock

`SharedDictionary` stores a fixed-domain `DynamicBackend` directly. The byte,
Unicode-scalar, and u64 engines share a unit-generic immutable `GraphVersion`
core: CRUD, compaction, clear, and empty frozen-batch installation all publish
with the same retained-`Arc` CAS. A retained expected generation prevents
pointer ABA. Reads load one generation and never retry; old roots remain alive
for snapshots.

The empty-dictionary batch fast path sorts and builds a minimal candidate
privately, then installs the entire candidate with one CAS only if the current
generation is still empty. If another insertion wins first, the batch falls
back to the baseline per-term CAS loop, preserving the union and duplicate
last-value-wins semantics. Therefore whole-batch atomicity is guaranteed only
for a successful empty fast-path publication; fallback batches retain the
baseline per-term visibility contract. `clear` similarly publishes an empty
generation instead of replacing the backend object.

---

## 3. Snapshot capture is $`\mathcal{O}(1)`$ — per backend

The interop contract is explicit: *implementations must make `snapshot`
$`\mathcal{O}(1)`$ with structural sharing or an equivalent immutable
revision; copying the whole dictionary or holding a long-lived read lock
violates the interface contract.* Capture in this crate is one code path —

```math
\text{capture}(\mathit{payload}) \;=\;
\operatorname{memoize}_{\text{revision}}\!\left(
\text{TraversalSnapshot::new}\bigl(\text{root}(\mathit{payload}),\
\text{len},\ \text{domain},\ \text{suffix}\bigr)\right)
```

On a cold revision this allocates an arena with exactly one initialized root
slot. Repeated captures of the same revision clone the memoized snapshot
`Arc`, preserving its warmed arena; every returned `VtResource` still owns a
fresh `ResourceContext` and therefore has an independent retain ledger. The
cost is the root load plus constant-size synchronization/allocation:

```math
T_{\text{capture}} \;=\; T_{\text{root}} + \Theta(1),
\qquad T_{\text{root}} = \Theta(1) \text{ for every backend below.}
```

What makes `root()` constant-time differs per engine, and each mechanism is
an instance of persistent-data-structure theory (Driscoll et al.
[[2]](#references); Okasaki [[3]](#references)):

| Backend | Revision mechanism | Why `root()` is $`\Theta(1)`$ | What keeps the revision alive |
|---|---|---|---|
| **DynamicDAWG** (`Live`) | Immutable revisions with atomic root publication: every mutation builds new structure and publishes a new root; published structure is never edited. | One acquire load captures root, term count, and graph revision from the same `GraphVersion`. | The root handle: reference-counted structural sharing keeps everything reachable from it alive, however far the live dictionary moves on. |
| **DoubleArrayTrie** (`Secondary`) | The whole trie is one frozen revision — read-only after construction, by contract. | Return the root cursor over the shared `Arc`'d base/check arrays. | The arena's node handles share the `Arc` of the trie itself. |
| **SCDAWG** (`Secondary`) | Insert-only graph; a root view over the current node table. | Return the root node view. | The node handles keep the shared structure reachable. |
| **Persistent ARTrie** (`Persistent`) | Copy-on-write revisions over the lock-free overlay (path-copying — the on-disk WAL/checkpoint machinery sits *below* this and is invisible to capture). | Load the current revision root from the overlay. | The revision root pins its copy-on-write structure in memory; eviction and checkpointing never mutate captured paths. |
| **Snapshot** (re-capture) | Already immutable. | `Arc::clone` of the existing `TraversalSnapshot` — the **self-snapshot law**: snapshotting a snapshot yields a resource sharing the same arena. | Itself. |

DynamicDAWG snapshot identity comes directly from the same atomically captured
`GraphVersion` as its root and term count, so it requires no separate writer
handshake. `SnapshotMemo` uses its seqlock-style handshake for heterogeneous
secondary and persistent producers whose backend descriptor and memo revision
are separate. A writer increments `active_writers` before touching backend state,
publishes a new memo revision after every possibly dirty mutation, and then
decrements the active count through an RAII guard. A snapshotter reads the memo
revision, requires a zero active-writer count, captures the backend's immutable
root/count pair, then validates the active count followed by the revision. It
returns only when the revision is unchanged and no writer is active. Acquire /
release ordering makes a writer visible either as active or as a completed
revision advance. AcqRel writer withdrawals form a completion chain, so the
writer performing the final 1→0 decrement carries every preceding concurrent
writer's revision publication to the reader. A writer that begins after final
validation linearizes after the capture. Loom checks the positive
active-then-revision order, one explicitly bounded overlapping two-writer
completion-chain schedule, and a negative revision-then-active witness that
accepts a torn root/identity pair. The two-writer check is correspondence for
the `2→1→0` AcqRel handoff, not exhaustive three-thread schedule exploration.

There is deliberately no writer-admission bit and no quiescence mutex. A cold
snapshotter CAS-publishes an empty per-revision generation and initializes it;
normal contenders poll its `OnceLock`, so the usual path constructs exactly
one provider snapshot. A boundedly stalled or panicking initializer can be
superseded by a successor generation, whose CAS winner initializes instead.
Consequently abandoned work cannot convoy another snapshotter, and a suspended
snapshotter cannot stop writers. Mutation unwind conservatively invalidates the
memo, advances the revision, and withdraws the writer announcement before the
panic propagates. Fallible persistent mutations also remain dirty on `Err`,
because an I/O error may follow partial overlay or durable publication; only a
proven `Ok(false)` suppresses revision invalidation.

Every invalidated attempt contributes one bounded backoff credit. The next
writer atomically consumes the accumulated credits and performs a bounded CPU
pause before entering; it never waits for a reader or for a state to drain.
This widens practical zero-writer windows under churn without turning the hint
into a lock. If a snapshotter is permanently suspended or panics, at most one
finite residual pause remains, after which later mutations pay nothing.

The exact progress claim is **lock-free system-wide and obstruction-free for an
individual snapshotter**, not wait-free or starvation-free for every reader.
The heterogeneous backends publish their root/count descriptor separately from
the producer memo revision, so no finite number of reader steps can guarantee
success against an adversary that completes a mutation between every capture
and validation. A wait-free reader would require a common atomic descriptor
containing both the backend revision owner and producer identity, or a
helpable multi-version publication protocol implemented by every backend.

Three consequences worth internalizing:

- **Capture never holds exclusion.** Snapshot construction and memo publication
  do not block writers; the pin is ownership, not exclusion.
- **`ldict_dictionary_free` does not end a snapshot.** The arena's handles own
  the revision; the handle's death merely releases the handle's own retain
  (see [§ 7](#7-the-retain-ledger--owneddictionaryresource)).
- **Successful mutations advance the source revision and evict its memo.**
  Writer admission, active-writer accounting, revision publication, and memo
  validation use acquire/release atomics. A concurrent capture validates the
  state on both sides and therefore linearizes wholly before or after the
  revision change; a new root can never be mislabeled with an old identity.

The deterministic regressions prove that a waiting snapshot never closes writer
admission, a stalled cold initializer blocks neither another snapshotter nor a
writer, mutation/snapshot panics leave progress state recoverable, and byte,
Unicode-scalar, and `u64` producers share identical revision-memo semantics.
The concurrent FFI test additionally performs 12,000 coherent root/count
captures under insert/remove churn and checks every walked final-node count
against the captured count. Continuous churn is a safety stress test, not a
per-thread starvation-freedom claim.

The empirical counterpart: the run-verified example in the
[C ABI reference § 15](c-abi-reference.md#15-a-complete-verified-c-example)
observes a pre-removal revision through a snapshot while the live dictionary
reports the term gone.

---

## 4. Compact graph fast path and lazy arena fallback

An immutable DynamicDAWG snapshot may negotiate `vt.dict.graph.v1`. The live
resource never advertises it, and DAT, SCDAWG, and persistent ARTrie snapshots
currently answer `Unsupported`; those resources continue through the universal
callback path below.

`TraversalSnapshot` contains two revision-local publication cells:

```text
native_graph: OnceLock<Option<SnapshotTraversalProjection<Node>>>
abi_graph:    OnceLock<AbiTraversalGraph>
arena:        NodeArena<Node>                           # owner, declared last
```

`SnapshotTraversalProjection<Node>` is an `Arc<SnapshotTraversalGraph<Unit,
Node::SnapshotGraphValueHandle>>`. The graph therefore retains the backend's
native value-handle type instead of erasing it into a universal integer.
DynamicDAWG projections use opaque, strict-provenance `NonNull` capabilities;
the graph builder deduplicates nodes with typed `NonNull` keys and never
round-trips an address through an integer. Dense DAT and ABI projections use
`DenseSnapshotCursor`, a nonzero one-based array position. These are distinct
associated cursor types: the C ABI implementation is available only when the
node's direct cursor is the dense representation, so a provenance-bearing
DynamicDAWG pointer cannot cross the ABI by construction.

Every aggregate that couples a projection with its retained owner declares the
projection first. Rust drops fields in declaration order, so native graph
handles are destroyed before the revision owner that makes them valid.
`DictionaryTraversalRoot::into_parts` preserves the same rule in
`DictionaryTraversalParts`, and moving the values out yields `(projection,
owner)` in that order.

The first graph request projects the retained immutable DAWG root into dense
node descriptors and one sorted flat edge array. Freeze-built revisions use
their dense snapshot ids; path-copied revisions use pointer identity only while
the retained root keeps every node alive. The second once-cell converts the
native labels and targets into the family ABI layout. This
$`\Theta(\lvert V\rvert + \lvert E\rvert)`$ work occurs after the
$`\mathcal{O}(1)`$ snapshot callback and outside the live backend's read lock
and snapshot-memo lock. Every later graph request for that revision is
$`\mathcal{O}(1)`$ and returns the same stable pointers and counts.

ABI `value_cursor` tokens are deliberately one-based dense graph indices—not
backend pointers. `node_value_u64` checks nonzero conversion and the graph's
node bound, translates the index through the retained native graph, and only
then reads backend state. Thus arbitrary or cross-revision cursor forgery is a
reported `InvalidArgument`, never a pointer dereference.

### 4.1 Lazy ABI-local node ids — the fallback arena

A dictionary node crossing an ABI must become a plain integer: engine node
handles are Rust types (generic, lifetime-bearing, non-FFI-safe), and leaking
raw pointers would weld consumers to engine internals and make forgery
memory-unsafe. The producer therefore *names* nodes on demand:

```text
TraversalSnapshot<N> {
    arena: NodeArena<N> {
        chunks: ArcSwap<Vec<Arc<Chunk<256>>>>,
        next_id: AtomicU64,
        growth_lock: Mutex<()>,
    },
    len, domain, suffix, identity,
}

id 0 ↦ root                                   # assigned at capture

ensure_edges(node):                            # on first expansion of `node`
    slot ← chunks[node / 256][node mod 256]     # lock-free directory read
    if slot is absent: return InvalidArgument  # forged ids die here
    slot.edges.get_or_init(||:                  # exactly one enumerator
        children ← slot.node.edges()
        ids ← next_id.fetch_add(|children|)     # one disjoint contiguous range
        grow directory if needed               # only this rare path locks
        install children in write-once slots
        return zip(labels, ids))
```

`node_transition` and `node_edges` both funnel through `ensure_edges`;
`node_is_final` and `node_value_u64` only load a directory and slot. Steady
reads and already-materialized expansions take no global mutex; only adding
directory chunks takes `growth_lock`. No vtable operation calls consumer code,
so even the growth critical section cannot participate in a cross-binary
deadlock.

Properties, each load-bearing:

- **Ids are revision-snapshot-scoped.** An id indexes *this* snapshot arena.
  Resources captured from the same source revision intentionally share that
  arena and numbering; a different revision has its own arena. Using an id
  against a different revision is
  either out of range (`InvalidArgument`) or silently names a different node:
  a correctness bug on the consumer's side of the trust boundary, never a
  memory-safety event on the producer's (see
  [ffi-boundary.md](../security/ffi-boundary.md#node-id-forgery)).
- **Ids are append-only and stable.** Atomic range reservation gives each
  expansion a disjoint interval. Once installed, an id never moves or
  gets reused for the snapshot's lifetime — consumers may cache them freely
  while the snapshot is retained.
- **Assignment is per traversed edge, not per engine node.** A DAWG shares
  suffixes, so the same engine node reached along two paths receives two ids:
  the ABI view is the trie *unfolding* of the DAG. Consumers walk a tree;
  producers store a DAG. (A consumer that wants DAG-sharing back can hash on
  its side; the ABI deliberately does not promise node identity.)
- **Fallback memory grows only with consumer work.** A cold revision begins with one
  entry; each first-time expansion of a node $`v`$ appends exactly
  $`\deg(v)`$ entries. After expanding the set $`E`$ of nodes:

  ```math
  \lvert \mathrm{arena} \rvert \;=\; 1 + \sum_{v \in E} \deg(v)
  ```

  — one arena entry per edge the fallback consumer actually traversed,
  allocated by the consumer's own calls. Compact-graph consumers instead
  deliberately materialize one bounded projection of the immutable revision,
  once, when they negotiate the optional interface.
  (Exhaustion economics: the consumer pays at least one ABI call per
  expansion; see the [boundary analysis](../security/ffi-boundary.md#resource-exhaustion).)
- **Reclamation is synchronous and bounded by the warmed revision.** When the
  source advances and the last retained old snapshot drops, its chunk
  directory and nodes are released on that releasing thread. There is no
  background reclaimer or unbounded deferred queue; instrumentation reports
  reclaimed nodes and release latency explicitly.
- **The id space cannot overflow silently.** Ids are minted with a checked
  `u64::try_from(nodes.len())`; the failure arm answers `LimitExceeded`
  (unreachable on 64-bit hosts, where `usize` fits in `u64` — the check is
  defensive portability, not dead weight). Incoming ids are narrowed with
  `usize::try_from` and bounds-checked before use.

### 4.2 Only snapshots are walkable

`root`, `len`, `node_is_final`, `node_value_u64`, `node_transition`, and
`node_edges` all begin with `context.immutable()` — a payload check that
answers `InvalidArgument` unless the resource **is** a `Snapshot`. The
exported resource of a live dictionary is a *snapshot factory*, not a
walkable graph: the only meaningful traversal-adjacent operation on it is
`snapshot` itself. This is what makes the query-start boundary law
enforceable at the type level of the protocol — there is no API through which
a consumer can accidentally walk a moving revision. The `IMMUTABLE` flag
([§ 6](#6-the-flag-truth-table)) is the discoverable marker of walkability.

### 4.3 Count coherence: `len` is exact for every family

A captured snapshot answers `len` only when the pair `(root, count)` is
coherent — both drawn from one published revision:

- **In-memory families** (DynamicDAWG ×3 domains, SCDAWG ×2) capture both
  fields through single-revision accessors (`root_with_term_count`, one
  `version.load()`/`inner.load()`), so their snapshots answer
  `out_known = 1` with the count of exactly the captured revision.
- **The persistent family** now publishes an immutable
  `{ root: Arc<OverlayNode>, term_count }` record through the same `ArcSwap`
  compare-and-swap. Insert/remove finality changes carry `+1`/`-1`; structural
  maintenance preserves the count; imported roots seed it with one iterative
  final-node walk. Snapshot capture loads the pair once, so byte, Unicode,
  u64, and vocabulary snapshots all answer `out_known = 1` exactly. This
  closes [LDICT-B4](FINDINGS_LEDGER.md#finding-ldict-b4--torn-snapshot-capture-root-and-len-read-from-different-revisions).

---

## 5. The fourteen `'static` vtables

`query_interface` never allocates: it validates the 16-byte interface id and
the minimum version, then hands back a pointer to one of fourteen immutable
`'static` structs: the base `RESOURCE_VTABLE`
(retain/release/query_interface), one dictionary-visit vtable, one compact
graph vtable, one snapshot-identity vtable, plus ten `VtDictionaryVTable`
instances covering the reachable (domain × immutable × suffix) combinations:

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
   DynamicDAWG reads are wait-free generation loads while writes use inner
   graph CAS, and arena access uses lock-free chunk publication without calling out. Nothing in
   the producer requires callers to serialize.
2. **`SUFFIX_BASED` marks semantics, not structure.** A suffix-flagged
   resource indexes substrings; a Levenshtein consumer must interpret finality
   as substring-match acceptance. The flag survives capture — a snapshot of an
   SCDAWG stays suffix-based.
3. **`IMMUTABLE` means "this resource *is* a snapshot"** — walkable, and
   self-snapshotting in $`\mathcal{O}(1)`$ by arena sharing. Deliberately, a
   live DoubleArrayTrie resource does **not** carry `IMMUTABLE` even though
   the trie is frozen: `IMMUTABLE` advertises the walkability contract of
   [§ 4.2](#42-only-snapshots-are-walkable), not the engine's mutability. A
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
   [evolution policy](https://github.com/vinary-tree/vinary-tree-interop/blob/master/docs/abi-evolution.md).
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

- [ABI reference — `vinary_tree_interop.h`, annotated](https://github.com/vinary-tree/vinary-tree-interop/blob/master/docs/abi-reference.md)
- [ABI evolution policy — the four version counters](https://github.com/vinary-tree/vinary-tree-interop/blob/master/docs/abi-evolution.md)
- [Family security model — trust zones and validation duties](https://github.com/vinary-tree/vinary-tree-interop/blob/master/docs/security-model.md)
- [liblevenshtein language-binding architecture (the consumer side)](https://github.com/vinary-tree/liblevenshtein-rust/blob/master/docs/language-bindings.md)
