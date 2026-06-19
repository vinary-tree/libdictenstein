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
| ⚙️ **Algorithms** | [`algorithms/`](algorithms/) | The dictionary-layer trait API and a per-backend deep-dive for each implementation ([`implementations/`](algorithms/implementations/)). |
| 🏗️ **Architecture** | [`architecture/`](architecture/) | Cross-cutting system design, starting with the [persistence family overview](architecture/persistence/README.md). |
| 💾 **Persistence** | [`persistence/`](persistence/) | The on-disk storage design ([mmap architecture](persistence/mmap-architecture.md), group-commit trade-offs). |
| ♻️ **Eviction** | [`eviction/`](eviction/) | The memory-pressure eviction subsystem for the persistent ARTrie. |
| 🔌 **Integration** | [`integration/`](integration/) | Backend integrations (e.g. [PathMap](integration/pathmap/README.md)). |
| 📊 **Benchmarks** | [`benchmarks/`](benchmarks/) | Scientific-method benchmarking ledgers and artifacts. |
| 🧪 **Experiments** | [`experiments/`](experiments/) | Per-optimization experiment ledgers (persistence enhancements, loading, lock-free flip). |
| 📐 **Design** | [`design/`](design/) | Architecture/mechanism design records and the historical campaign ledger. |
| ✅ **Formal verification** | [`../formal-verification/`](../formal-verification/) | Rocq theorems + TLA⁺ models + the CI-gated `unsafe` contract inventory. |
| 🎨 **Diagrams** | [`diagrams/`](diagrams/) | Diagrams-as-code sources and rendered SVGs; see [`diagrams/README.md`](diagrams/README.md) for the rendering pipeline. |

---

## Suggested reading order

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
