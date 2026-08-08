# User guide

Task-oriented documentation for **using** libdictenstein — how to pick a backend, build and query
it, associate values, traverse it edge-by-edge, and persist it. For the *why* and the internals, follow
the links into [theory](../theory/), [algorithms](../algorithms/), and [architecture](../architecture/).

## Where to start

1. **[Getting started](getting-started.md)** — add the crate, build your first dictionary, run a
   query, walk the automaton. Five minutes.
2. **[Choosing a backend](backends.md)** — the selection summary and feature-flag reference. Which
   of the 20+ backend variants fits your workload.
3. **[In-memory dictionaries](in-memory-dictionaries.md)** — a guided tour of the volatile
   (RAM-resident) backends: sets vs. maps, exact vs. substring search, insert-only vs. mutable, and
   the `u8` / `char` / `u64` alphabets.
4. **[Cookbook](cookbook.md)** — copy-paste recipes for common tasks: counting, prefix completion,
   substring search, bidirectional maps, set algebra, serialization, and durable persistence.

## The one-paragraph model

A **dictionary** is a traversable set of **terms** (strings), optionally mapping each term to a
**value** `V`. Every backend implements a small [trait API](../algorithms/README.md) — `contains`,
`root`, and node-by-node `transition` — so you can swap implementations without touching call sites.
With the default value type `()` a dictionary is a **set** (membership only); with any other `V` it
is a **map**. Lookup costs $`O(\lvert q\rvert)`$ in the query length `q`, independent of how many
terms are stored — the defining property of the trie-shaped indexes this crate provides.

> libdictenstein is the *container* half of approximate string matching. The *query* half — a
> Levenshtein-automaton transducer that walks any of these dictionaries to find terms within an edit
> distance — lives in the companion crate
> [liblevenshtein](https://github.com/vinary-tree/liblevenshtein-rust). This crate contains
> no fuzzy-matching code itself.

Notation in these guides follows [`docs/notation.md`](../notation.md).
