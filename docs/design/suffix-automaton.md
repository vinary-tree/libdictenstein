# Design: the volatile suffix automaton

Design rationale for the **in-memory** `SuffixAutomaton` / `SuffixAutomatonChar` backends — why they
are shaped the way they are. The *theory* (endpos classes, suffix links, the online construction and
its bounds) is in [`theory/scdawg/02-suffix-automaton.md`](../theory/scdawg/02-suffix-automaton.md);
the *usage and API* are in
[`algorithms/implementations/suffix-automaton.md`](../algorithms/implementations/suffix-automaton.md);
the *persistent* suffix family is [`persistent-suffix-index.md`](persistent-suffix-index.md). This
document is the "why". Notation follows [`docs/notation.md`](../notation.md).

## What it is for

A suffix automaton indexes **every substring** of the texts it is built from, so it answers "does
pattern `P` occur anywhere inside an indexed text?" in $`O(\lvert P\rvert)`$ — the *substring* (infix)
query that the whole-term backends ([double-array trie](../algorithms/implementations/double-array-trie.md),
[DAWG](../algorithms/implementations/dynamic-dawg.md)) cannot answer. That single requirement drives
every design choice below.

## Design choices and their rationale

### Build from *texts*, not a term set

The public constructor is `from_texts`, not `from_terms`. This is deliberate: the automaton indexes
the substrings of each supplied *text*, so the input is a set of documents to be searched, not a set
of words to be matched exactly. It is the API-level signal that this backend answers a different
question than the whole-term backends.

### Online (left-to-right) construction

Construction is **online** — `extend(c)` appends one unit at a time in amortized $`O(1)`$, so a text
of length `n` is indexed in $`O(n)`$ and the automaton is queryable after every prefix. The
alternative (batch construction from the full text) would forfeit incremental indexing for no
asymptotic gain. The one non-trivial step is **clone-on-split**: when appending a unit would merge
two distinct endpos classes, the offending state is cloned so each class keeps its own state. Capping
this at one clone per unit is exactly what holds the automaton to its provable bounds — at most
$`2\lvert T\rvert - 1`$ states and $`3\lvert T\rvert - 4`$ transitions
([theory](../theory/scdawg/02-suffix-automaton.md)). This linear size is *by construction*, not a
best case, which is why the [security analysis](../security/untrusted-input.md) treats state growth
as expected rather than an adversarial blow-up.

### Arena of indexed nodes, not pointers

The core (`SuffixAutomatonInner<U, V>`,
[`src/suffix_automaton/core/inner.rs`](../../src/suffix_automaton/core/inner.rs)) stores states in a
flat `Vec<SuffixNode>` addressed by integer index; edges and suffix links are `usize` indices, not
`Box`/`Arc` pointers. This is the same arena discipline the DAWG and SCDAWG cores use, and it buys the
same two properties: non-recursive drop (no stack overflow tearing down a huge automaton) and
compact, cache-friendly traversal ([architecture §2](../architecture/in-memory-dictionaries.md#2-monomorphized-cores)).

### Whole-graph snapshot concurrency (not per-node CAS)

Unlike the DAWG family, the suffix automaton uses the **whole-graph snapshot / copy-on-write**
concurrency strategy: `LockFreeSuffixAutomaton` wraps `Arc<ArcSwap<SuffixAutomatonInner<U,V>>>`, and a
writer clones the inner automaton, applies `extend`, and CAS-publishes the whole new revision
([architecture §3b](../architecture/in-memory-dictionaries.md#3b-whole-graph-snapshot-copy-on-write--suffix-automaton-scdawg-pathmap)).
The rationale is edit locality: a `clone-on-split` rewires suffix links across the graph, so an edit
is *not* confined to one root-to-node path the way a DAWG insert is. Per-node CAS would have to
coordinate a graph-wide rewrite atomically; a single root-pointer CAS after building the new revision
is simpler and correct, and gives readers a stable, internally consistent snapshot.

### Two trait asymmetries, on purpose

Two API decisions surprise trait-driven callers and are therefore stated explicitly (and surfaced in
the [implementation index](../algorithms/implementations/README.md#asymmetries-worth-knowing-all-verified-against-src)):

- **It does not implement `SubstringDictionary`.** Substring capability is advertised by
  `Dictionary::is_suffix_based() == true`, and substring queries are served through node/zipper
  traversal (which is what the companion Levenshtein transducer consumes). The `SubstringDictionary`
  trait — with its `find_exact_substring` returning positional matches — is implemented by
  [`Scdawg`](../algorithms/implementations/scdawg.md) instead. The two substring families thus answer
  "am I a substring index?" through different signals; this is intentional, reflecting that the
  suffix automaton is the *online-updatable* index while the SCDAWG is the *static, positional* one.
- **Removal is not via `MutableDictionary`.** There is an inherent `remove(&self, text)`, but it is an
  $`O(n)`$ rebuild (removing a text can change the endpos structure globally), so it is not offered
  through the `MutableDictionary` trait, whose `remove` implies a cheap targeted deletion. Presenting
  a rebuild behind that trait would misrepresent its cost.

## When to choose it over the SCDAWG

Both index substrings; the design split is *update model*:

| Need | Backend |
|------|---------|
| index that grows online, one unit at a time | `SuffixAutomaton` |
| static index, most compact, with **bidirectional** (left + right) extension and positional matches | [`Scdawg`](../algorithms/implementations/scdawg.md) |

## Related

- [theory/scdawg/02-suffix-automaton.md](../theory/scdawg/02-suffix-automaton.md) — the endpos theory,
  suffix links, and construction bounds.
- [algorithms/implementations/suffix-automaton.md](../algorithms/implementations/suffix-automaton.md) — API and usage.
- [architecture/in-memory-dictionaries.md](../architecture/in-memory-dictionaries.md) — the shared
  arena + concurrency design.
- [persistent-suffix-index.md](persistent-suffix-index.md) — the durable suffix family.
