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
| 🧮 **Theory** | [`theory/`](theory/) | Data-structure foundations: [disk-tries](theory/disk-tries/) (trie → B-trie → ART → persistent ART → buffer management), [SCDAWG](theory/scdawg/) (suffix automaton → CDAWG → symmetric compact DAWG), and [volatile automata](theory/volatile-automata/) (DAWG minimization, double-array tries, Bloom filters, bit-parallel child scan). Paper-grounded, with proofs. |
| ⚙️ **Algorithms** | [`algorithms/`](algorithms/) | The dictionary-layer trait API and a per-backend deep-dive for each implementation ([`implementations/`](algorithms/implementations/)). Conceptual guides: [zippers](algorithms/zippers.md) (lazy set-algebra), [serialization](algorithms/serialization.md) (bincode/protobuf binary persistence + value-preserving bincode), [persistent suffix graphs](algorithms/persistent-suffix-graphs.md) (durable substring indexes), [native `u64` + CX](algorithms/native-u64-and-cx.md) (`u64`-sequence profile + compact snapshot), and the [vocab trie](algorithms/vocab-trie.md) (term ↔ `u64` bijection). |
| 🏗️ **Architecture** | [`architecture/`](architecture/) | Cross-cutting system design: the core [abstractions](architecture/abstractions.md) (`CharUnit` + `KeyEncoding` — one code path, three alphabets), the [in-memory dictionary architecture](architecture/in-memory-dictionaries.md) (monomorphized cores + the two lock-free strategies), the [optimization roadmap](architecture/optimization-roadmap.md), and the [persistence family overview](persistence/families.md). |
| 📖 **User guide** | [`user-guide/`](user-guide/README.md) | Task-oriented usage: [getting started](user-guide/getting-started.md), [choosing a backend](user-guide/backends.md), the [in-memory tour](user-guide/in-memory-dictionaries.md), and the [cookbook](user-guide/cookbook.md). |
| 💾 **Persistence** | [`persistence/`](persistence/README.md) | The durable, lock-free ARTrie engine end-to-end. Start at the [**architecture entry point**](persistence/README.md), then descend: the reusable [durable-storage kernel](persistence/durable-storage-kernel.md), the [families](persistence/families.md), the [lock-free overlay](persistence/lock-free-overlay.md), [durability & recovery](persistence/durability-and-recovery.md), the [concurrency model](persistence/concurrency-model.md), [storage backends](persistence/storage-backends.md) + [WAL format](persistence/wal-format.md), [eviction](persistence/eviction.md), [group commit](persistence/group-commit.md), and the [proof map](persistence/formal-verification-map.md). |
| ♻️ **Eviction** | [`persistence/eviction.md`](persistence/eviction.md) | The memory-pressure eviction subsystem for the persistent ARTrie (part of the persistence corpus). |
| 🔌 **Integration** | [`integration/`](integration/) | Backend integrations (e.g. [PathMap](integration/pathmap/README.md)). |
| 🔗 **Bindings / C ABI** | [`bindings/`](bindings/README.md) | The producer contract corpus: the [`ldict_*` C-ABI reference](bindings/c-abi-reference.md) (all 41 functions, status/kind/capability tables, per-backend matrix, verified C example), the [resource-producer architecture](bindings/resource-producer.md) (`vt.dictionary.v1` production, O(1) snapshot capture, node-id leasing, retain ledger), the [native collection quick reference](bindings/README.md#native-collection-quick-reference), the [FFI boundary analysis](security/ffi-boundary.md), and the [findings ledger](bindings/FINDINGS_LEDGER.md). The machine-readable ABI model is [`../bindings/api.json`](../bindings/api.json), enforced by [`../scripts/check-bindings.py`](../scripts/check-bindings.py) (CI job `binding-contract`). |
| 📊 **Benchmarks** | [`benchmarks/`](benchmarks/) | Scientific-method benchmarking ledgers and artifacts, including the [collection traversal and language-binding protocol](benchmarks/collection-traversal-and-bindings.md). |
| 🧪 **Experiments** | [`experiments/`](experiments/) | Per-optimization experiment ledgers (persistence enhancements, loading, lock-free flip). |
| 📐 **Design** | [`design/`](design/) | Architecture/mechanism design records and the historical campaign ledger. |
| ✅ **Formal verification** | [`../formal-verification/`](../formal-verification/) | Rocq theorems + TLA⁺ models + the CI-gated `unsafe` contract inventory. |
| 🔒 **Security** | [`security/`](security/README.md) | Threat model, untrusted-input / DoS analysis, deserialization safety, and the `unsafe`-contract map. |
| 🛠️ **Engineering** | [`engineering/`](engineering/testing-strategy.md) | Testing strategy, benchmarking methodology, and the feature-flag reference. |
| 🎨 **Diagrams** | [`diagrams/`](diagrams/) | Diagrams-as-code sources and rendered SVGs; see [`diagrams/README.md`](diagrams/README.md) for the rendering pipeline. |

---

## Suggested reading order

The map below renders the reading paths as a graph: follow an edge from its tail
to its head. The green spine is the on-ramp (crate `README` → trait layer →
per-backend guide); from there you branch into the grey **theory** track, the
blue **persistence / systems** track, and finally the amber **formal
verification** track. It is split across two figures — part 1 covers the
getting-started and theory tracks, part 2 the persistence and formal tracks; the
three "go deeper" edges that cross between them are drawn as a `→ see part 2`
note in part 1 and re-attached from a `← from part 1` note in part 2.

<img src="diagrams/docs-reading-order.svg" alt="Documentation reading-order map, part 1 of 2 (getting-started and theory tracks): a top-to-bottom directed graph. The green getting-started spine runs crate README → backend selector → user-guide/backends → algorithms/README (trait layer) → algorithms/implementations (per-backend), which branches to zippers and serialization. Teal 'go deeper' edges cross from the trait layer and per-backend guides into the grey theory track (architecture/abstractions, theory/disk-tries, theory/scdawg). The three edges that continue into the persistence and formal tracks terminate at a 'see part 2' note." width="100%"/>
<img src="diagrams/docs-reading-order-2.svg" alt="Documentation reading-order map, part 2 of 2 (persistence and formal tracks): a top-to-bottom directed graph continuing from part 1. A 'from part 1' note re-attaches the three cross-track edges into the blue persistence track (architecture/persistence + storage-backends), which fans out to native-u64-and-cx, vocab-trie, persistent-suffix-graphs, the WAL format reference, and eviction. The persistence track finally points into the amber formal-verification track (Rocq theorems, TLA+ models, unsafe-contract inventory)." width="70%"/>

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
> **[liblevenshtein](https://github.com/vinary-tree/liblevenshtein-rust)**.
> libdictenstein contains no fuzzy-matching code itself.

---

## Authoring conventions — math in Markdown

Prose math is MathJax; diagram labels use PlantUML-LaTeX (`<latex>` / `<math>`).
**Which delimiters you use is not cosmetic — GitHub corrupts the obvious ones.**

> This section is the authority on **delimiters** — the characters that wrap a math span. For
> **which LaTeX goes inside** a span and **which word names each concept**, see the
> [notation & terminology register](notation.md). In particular, a length bar is `\lvert x \rvert`
> (never `\mid`), and big-O is inline math, never a code span.

Before handing a math span to MathJax, GitHub's Markdown pass runs CommonMark
backslash-escape processing *over the span's interior*. Inside `$…$` and `$$…$$`
that rewrites `\_`→`_`, `\{`→`{`, `\}`→`}`, `\;`→`;`, `\,`→`,`, `\#`→`#`. The damage
comes in two flavours, and the silent one is the dangerous one:

- **Loud** — a bare `_` or `#` reaches MathJax, which aborts with
  `'_' allowed only in math mode`.
- **Silent** — `\max\{\,L\,\}` renders as `\max{,L,}`: the set braces disappear and
  literal commas replace thin-spaces. No error, wrong mathematics.

Only two forms survive GitHub's escape pass verbatim. Use them:

| | Write this | Not this |
|---|---|---|
| **Inline** | ``$`\text{checkpoint\_lsn}`$`` | `$\text{checkpoint\_lsn}$` |
| **Display** | a ` ```math ` fenced block | `$$ … $$` |
| **Literal `$`** | inline code — `` `$₁` `` | `\$₁` |

Two further rules, both established by rendering probes against GitHub's own GFM
endpoint (`gh api -X POST /markdown`):

1. **Never let an ASCII letter abut the opening `` $` ``.** GitHub declines to open
   inline math there, so ``InMem$`\Rightarrow`$descend`` renders as literal text.
   Write ``InMem $`\Rightarrow`$ descend``. (A digit or `)` before the `$` is fine.)
2. **Keep literal dollars in inline code.** Two or more `\$` on one line can pair
   into a spurious math span — and whether they do is not predictable from the
   source: `{\$, b\$}` renders as text while `\$₁, \$₂` renders as math.

`scripts/check-doc-math.py` enforces all four rules over every tracked `*.md`, and
runs in the `diagrams` CI job. Run it before committing docs:

```bash
python3 scripts/check-doc-math.py          # every tracked *.md
python3 scripts/check-doc-math.py FILE…    # just these
```
