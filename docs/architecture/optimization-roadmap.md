# Optimization Roadmap

This document records the architecture, algorithm, engineering, and benchmark
rules used to keep libdictenstein fast as the backend family grows. It is a
companion to the public backend selector in the root `README.md`: the selector
answers "which backend should I use?", while this document answers "how should a
backend be improved, measured, and admitted?"

## Terms

| Term | Meaning |
|------|---------|
| Backend | A concrete dictionary implementation such as `DoubleArrayTrie`, `DynamicDawg`, `PathMapDictionary`, `SuffixAutomaton`, `Scdawg`, or a persistent ARTrie variant. |
| Volatile dictionary | An in-memory backend whose state is not durable by itself. |
| Persistent dictionary | A backend with WAL, checkpoint, recovery, and reopen semantics. |
| Snapshot publication | A writer builds a new immutable or effectively immutable view, then publishes it with a single atomic pointer update or compare-and-swap (CAS). |
| Wait-free read | A read operation completes in a bounded number of local steps without waiting for a writer. In this crate the term is used for snapshot reads that do not acquire locks. |
| Non-blocking write | A write may retry after CAS contention, but it does not wait on a blocking mutex or reader-writer lock. |
| Unit | The edge label type. `u8` is byte-level, `char` is Unicode scalar-level, and `u64` is sequence-token-level. |
| $`n`$, $`m`$, $`k`$ | $`n`$ is indexed terms or source length, $`m`$ is query length, and $`k`$ is key length in units. |

## System View

<img src="../diagrams/arch-roadmap-system-view.svg" alt="System view of libdictenstein. A caller or transducer calls the stable Dictionary trait API, which fans out to two backend families. The always-on in-memory family (green) holds DoubleArrayTrie (static, cache-local), DynamicDawg (lock-free prefix graph), PathMapDictionary (amber, feature pathmap-backend; atomic copy-on-write trie snapshot), SuffixAutomaton (substring index), and Scdawg (compact substring graph). The feature-gated persistent family (blue, disk-backed) holds the Persistent ARTrie (mmap + WAL + checkpoint), the persistent suffix family (native snapshot + WAL), and the persistent vocab trie (bijective term/index map). Two CI gate suites admit changes: the Criterion benchmark gates validate both families, and the Loom / TLA+ / Rocq formal gates validate the persistent family and the concurrent in-memory backends (DynamicDawg, PathMapDictionary, SuffixAutomaton, Scdawg) but not the static DoubleArrayTrie." width="70%"/>

The key architectural rule is that public traits stay stable while backend
internals specialize aggressively. Static read-mostly backends optimize layout;
mutable volatile backends optimize snapshot publication; persistent backends
optimize durability, recovery, and bounded unsafe boundaries.

## Backend Selection Matrix

| Workload | Preferred backend | Why | Expected complexity |
|----------|-------------------|-----|---------------------|
| Static byte dictionary with high lookup volume | `DoubleArrayTrie` | Contiguous `BASE`/`CHECK` arrays minimize pointer chasing. | Build depends on placement; lookup $`O(m)`$. |
| Static Unicode dictionary | `DoubleArrayTrieChar` | Same cache-local layout with `char` units. | Lookup $`O(m)`$ over Unicode scalar values. |
| Mutable prefix dictionary | `DynamicDawg` / `DynamicDawgChar` | Lock-free edge/value publication with suffix sharing. | Lookup $`O(m)`$, write $`O(k \times c)`$ where $`c`$ is CAS retry cost. |
| Fast in-memory trie with external `pathmap` feature | `PathMapDictionary` / `PathMapDictionaryChar` | Copy-on-write root snapshot over a mature trie engine. | Lookup $`O(m)`$, write clone cost depends on pathmap structural sharing. |
| Substring lookup over dynamic text | `SuffixAutomaton` / `SuffixAutomatonChar` | Substrings are accepted by graph paths. | Query $`O(m)`$, states up to about $`2n - 1`$ for one source string. |
| Compact substring lookup over batch terms | `Scdawg` / `ScdawgChar` | Compact suffix graph shape plus left-extension support. | Query $`O(m)`$, better space locality after compaction. |
| Durable mutable byte/char keys | `PersistentARTrie` / `PersistentARTrieChar` | ART node adaptation plus WAL/checkpoint recovery. | Lookup/update $`O(k)`$, storage cost depends on checkpoint cadence. |
| Durable sequence keys | `PersistentARTrieU64*` | Native `u64` units avoid byte encoding overhead. | Lookup/update $`O(k)`$ over sequence units. |
| Durable vocabulary bijection | `PersistentVocabARTrie` | Term-to-index and index-to-term recovery are first-class. | Lookup/update $`O(k)`$ plus reverse-map maintenance. |

## Benchmark Gate

Performance changes must be measured before and after implementation.

```text
cargo bench --bench volatile_dictionary_benchmarks
cargo bench --bench volatile_dictionary_benchmarks --features pathmap-backend
cargo bench --bench persistent_artrie_benchmarks --features persistent-artrie
cargo bench --bench persistent_suffix_native_benchmarks --features persistent-artrie
```

For a focused optimization, save a Criterion baseline and compare:

```text
cargo bench --bench volatile_dictionary_benchmarks -- --save-baseline before
cargo bench --bench volatile_dictionary_benchmarks -- --baseline before
```

The minimum acceptance rule is:

```text
accept(change) =
  correctness_tests_pass
  ∧ no_new_blocking_lock_in_volatile_dictionary
  ∧ p50_regression ≤ 3%
  ∧ p95_regression ≤ 5%
  ∧ memory_regression_explained_or_improved
```

Small regressions can be accepted only when the change buys a documented
semantic guarantee, such as stronger recovery, stricter snapshot isolation, or a
smaller unsafe boundary.

## Optimization Algorithm

The workflow is deliberately mechanical.

```text
Algorithm optimize_backend(backend, workload):
    define the user-visible invariant for backend
    define the primary metric for workload
    record a Criterion baseline
    identify the hottest operation by benchmark group
    inspect only the code on that hot path
    choose the smallest representation or publication change that targets it
    add a regression test for the invariant
    rerun the focused benchmark and correctness tests
    accept the change only if the benchmark gate and invariant test pass
```

This process prevents speculative rewrites. It also keeps representation choices
local: for example, a byte trie node can use a dense 256-way index while a
Unicode node stays sorted or sparse-indexed.

## Current Architectural Decisions

| Decision | Consequence |
|----------|-------------|
| Volatile mutable dictionaries use internal synchronization. | Public callers do not need an outer `RwLock` for `DynamicDawg`, `PathMapDictionary`, `SuffixAutomaton`, `Scdawg`, or `BijectiveMap`. |
| Reader handles and zippers carry stable snapshots. | Compaction and mutation cannot invalidate traversal state. |
| Writers use CAS publication or per-node atomic edge/value replacement. | Contended writers may retry, but readers do not block behind writers. |
| Static backends keep compact array layouts. | Lookup and traversal stay cache-local for read-heavy dictionaries. |
| Persistent backends keep WAL/checkpoint logic separate from volatile backends. | Durability concerns do not leak into the in-memory trait layer. |

## Time and Space Targets

| Area | Target |
|------|--------|
| Prefix lookup | $`O(m)`$ time, no heap allocation on successful read paths. |
| Prefix insert | $`O(k)`$ expected time, bounded CAS retry under contention. |
| Exact traversal | $`O(\text{nodes} + \text{edges})`$ time, iterative traversal for deep structures. |
| Substring lookup | $`O(m)`$ time in suffix backends. |
| Compaction | Rebuild from visible terms with exact-capacity collections where the bound is known. |
| Persistent recovery | Replay only the durable WAL suffix after the last valid checkpoint. |
| Edge storage | Tiny inline arrays for low fanout, sorted vectors for medium fanout, indexed/dense storage for high byte fanout. |

## Experimental Track

The following ideas are eligible only after the benchmark gate has enough
coverage to make their trade-offs visible.

| Idea | Benefit | Risk |
|------|---------|------|
| SIMD edge search for medium fanout byte nodes | Lower transition latency before dense indexing becomes worthwhile. | Architecture-specific code and more unsafe review. |
| Hot/cold dictionary layering | Keep a compact static base with a small mutable lock-free delta. | More complex iteration, deletion, and value merge semantics. |
| Epoch reclamation for graph snapshots | Lower clone and memory-retention cost under writer contention. | Harder reclamation proofs than `Arc` snapshots. |
| Learned backend selector | Choose backend from observed key distribution and workload ratios. | Selector drift if trained on unrepresentative corpora. |
| LOUDS/FST static snapshots | Very compact read-only dictionaries. | Reduced mutation support and more conversion code. |
| Adaptive checkpoint cadence | Improve persistent write throughput and recovery bound together. | Needs workload-aware control logic and failure-injection tests. |

## References

- Leis, Kemper, and Neumann, "The Adaptive Radix Tree: ARTful Indexing for Main-Memory Databases", ICDE 2013, DOI: <https://doi.org/10.1109/ICDE.2013.6544812>.
- Daciuk, Mihov, Watson, and Watson, "Incremental Construction of Minimal Acyclic Finite-State Automata", Computational Linguistics 2000, DOI: <https://doi.org/10.1162/089120100561601>.
- Blumer, Blumer, Haussler, Ehrenfeucht, Chen, and Seiferas, "The Smallest Automaton Recognizing the Subwords of a Text", Theoretical Computer Science 1985, DOI: <https://doi.org/10.1016/0304-3975(85)90157-4>.
- Blumer, Blumer, Ehrenfeucht, Haussler, and McConnell, "Complete Inverted Files for Efficient Text Retrieval and Analysis", Journal of the ACM 1987, DOI: <https://doi.org/10.1145/32204.32208>.
- Michael, "Hazard Pointers: Safe Memory Reclamation for Lock-Free Objects", IEEE Transactions on Parallel and Distributed Systems 2004, DOI: <https://doi.org/10.1109/TPDS.2004.8>.
