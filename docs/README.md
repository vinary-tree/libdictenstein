# libdictenstein documentation

This is the documentation index for **libdictenstein** — a toolbox of
high-performance dictionary data structures (tries, DAWGs, double-array tries,
suffix automata, SCDAWGs, and a lock-free **durable** persistent ARTrie family),
unified behind one trait API and backed by machine-checked proofs.

New here? Start with the root [`README.md`](../README.md) for the crate overview and
the [backend selector](../README.md#backend-selector), then come back here for depth.

---

## Documentation map

The docs are organized along a single axis: **theory** (paper-grounded math and
data-structure foundations) → **algorithms** (per-backend implementation and usage)
→ **architecture / persistence** (cross-cutting systems) → **formal verification**
(the proofs that pin the concurrency and durability claims), with **benchmarks**,
**experiments**, and **design** records alongside.

| Area | Path | What's there |
|------|------|--------------|
| 🧮 **Theory** | [`theory/`](theory/) | Data-structure foundations: [disk-tries](theory/disk-tries/) (trie → B-trie → ART → persistent ART → buffer management) and [SCDAWG](theory/scdawg/) (suffix automaton → CDAWG → symmetric compact DAWG). Paper-grounded, with proofs. |
| ⚙️ **Algorithms** | [`algorithms/`](algorithms/) | The dictionary-layer trait API and a per-backend deep-dive for each implementation ([`implementations/`](algorithms/implementations/)). Conceptual guides: [zippers](algorithms/zippers.md) (lazy set-algebra), [serialization](algorithms/serialization.md) (bincode/JSON/plaintext/protobuf + value-preserving), [persistent suffix graphs](algorithms/persistent-suffix-graphs.md) (durable substring indexes), [native `u64` + CX](algorithms/native-u64-and-cx.md) (`u64`-sequence profile + compact snapshot), and the [vocab trie](algorithms/vocab-trie.md) (term ↔ `u64` bijection). |
| 🏗️ **Architecture** | [`architecture/`](architecture/) | Cross-cutting system design: the core [abstractions](architecture/abstractions.md) (`CharUnit` + `KeyEncoding` — one code path, three alphabets), the [optimization roadmap](architecture/optimization-roadmap.md), and the [persistence family overview](architecture/persistence/README.md). |
| 💾 **Persistence** | [`persistence/`](persistence/) | The on-disk storage design: [mmap architecture](persistence/mmap-architecture.md), the [WAL on-disk format](persistence/wal-format.md), and group-commit trade-offs. |
| ♻️ **Eviction** | [`eviction/`](eviction/) | The memory-pressure eviction subsystem for the persistent ARTrie. |
| 🔌 **Integration** | [`integration/`](integration/) | Backend integrations (e.g. [PathMap](integration/pathmap/README.md)). |
| 📊 **Benchmarks** | [`benchmarks/`](benchmarks/) | Scientific-method benchmarking ledgers and artifacts. |
| 🧪 **Experiments** | [`experiments/`](experiments/) | Per-optimization experiment ledgers (persistence enhancements, loading, lock-free flip). |
| 📐 **Design** | [`design/`](design/) | Architecture/mechanism design records and the historical campaign ledger. |
| ✅ **Formal verification** | [`../formal-verification/`](../formal-verification/) | Rocq theorems + TLA⁺ models + the CI-gated `unsafe` contract inventory. |
| 🎨 **Diagrams** | [`diagrams/`](diagrams/) | Diagrams-as-code sources and rendered SVGs; see [`diagrams/README.md`](diagrams/README.md) for the rendering pipeline. |

---

## Suggested reading order

The map below renders the reading paths as a graph: follow an edge from its tail
to its head. The green spine is the on-ramp (crate `README` → trait layer →
per-backend guide); from there you branch into the grey **theory** track, the
blue **persistence / systems** track, and finally the amber **formal
verification** track.

<img src="diagrams/docs-reading-order.svg" alt="Documentation reading-order map: a left-to-right directed graph of the docs grouped into four colored tracks. Green getting-started spine runs crate README → backend selector → user-guide/backends → algorithms/README (trait layer) → algorithms/implementations (per-backend), which branches to zippers and serialization. Teal 'go deeper' edges cross from the trait layer and per-backend guides into the grey theory track (architecture/abstractions, theory/disk-tries, theory/scdawg) and the blue persistence track (architecture/persistence + mmap-architecture), which fans out to native-u64-and-cx, vocab-trie, persistent-suffix-graphs, the WAL format reference, and eviction. The persistence track finally points into the amber formal-verification track (Rocq theorems, TLA+ models, unsafe-contract inventory)." width="100%"/>

1. **Pick a backend** — root [`README.md`](../README.md#backend-selector) selector +
   [`user-guide/backends.md`](user-guide/backends.md).
2. **Understand the trait layer** — [`algorithms/README.md`](algorithms/README.md).
3. **Go deep on your backend** — the matching file under
   [`algorithms/implementations/`](algorithms/implementations/), with its theory
   back-link into [`theory/`](theory/).
4. **If you need durability** — [`theory/disk-tries/`](theory/disk-tries/) for the
   foundations, then [`persistence/`](persistence/) and
   [`architecture/persistence/`](architecture/persistence/README.md) for the system.
5. **If you need to trust it** — [`../formal-verification/`](../formal-verification/).

> The query half of approximate string matching — a Levenshtein-automaton
> transducer that walks any of these dictionaries — lives in the companion crate
> **[liblevenshtein](https://github.com/universal-automata/liblevenshtein-rust)**.
> libdictenstein contains no fuzzy-matching code itself.
