# DAWG minimization

**Navigation**: [← Volatile automata](README.md) · [Double-array tries →](02-double-array-tries.md)

A **DAWG** (Directed Acyclic Word Graph) is the minimal deterministic automaton that recognizes a
finite set of terms. It is a trie with a further collapse: where a trie shares common *prefixes*, a
DAWG additionally shares common *suffixes*, so the `"-tion"` ending of a million English words is
stored once. This document develops the equivalence that makes that collapse minimal, Daciuk's
incremental construction, and how libdictenstein realizes it with a single 64-bit signature per node
([`src/node_signature.rs`](../../../src/node_signature.rs)). Notation follows
[`docs/notation.md`](../../notation.md).

## From trie to DAWG

<img src="../../diagrams/dawg-suffix-sharing.svg" alt="Trie versus DAWG suffix sharing: in the trie, words that end alike each keep their own tail path to a distinct final node; in the DAWG those identical tails converge onto shared nodes and a single final node, so the common suffix is stored once." width="620"/>

A trie over a term set `T` has one node per distinct prefix, so its size is
$`O(\sum_{t \in T} \lvert t\rvert)`$ in the worst case. Many of those nodes are redundant: two nodes
that spell out exactly the same set of continuations to a final state are interchangeable. Merging
every such pair yields the **minimal** automaton for `T` — the DAWG.

## The right-language equivalence

Define the **right language** of a node `u` as the set of strings spelled on paths from `u` to any
final node:

```math
R(u) = \{\, w \in \Sigma^{*} : \text{some path } u \xrightarrow{w} f \text{ ends at a final node } f \,\}
```

Two nodes are **equivalent** exactly when their right languages coincide:

```math
u \equiv v \iff R(u) = R(v)
```

This is the Myhill–Nerode relation restricted to the acyclic term automaton. It is an equivalence
relation (reflexive, symmetric, transitive), and the quotient automaton — one state per equivalence
class — is the unique minimal DFA recognizing `T` (Blumer et al. 1985,
[10.1016/0304-3975(85)90157-4](https://doi.org/10.1016/0304-3975(85)90157-4)). Minimizing a DAWG is
therefore exactly **merging all nodes with equal right language**.

**Why the quotient is minimal.** If two states of *any* deterministic automaton for `T` had the same
right language they could be merged without changing the accepted set; so a minimal automaton has no
two such states, i.e. its states are precisely the $`\equiv`$-classes. Determinism plus a distinct
right language per state also makes it unique up to isomorphism.

## Comparing right languages is expensive — so hash them

Testing $`R(u) = R(v)`$ directly means comparing two potentially large string sets. The classic
optimization computes a **signature** per node bottom-up and compares signatures instead. Because the
automaton is acyclic, a node's right language is determined by its `is_final` flag and the multiset of
`(label, R(child))` over its out-edges — so a recursive signature captures $`\equiv`$ exactly:

```math
\text{sig}(u) = h\Big(\text{is\_final}(u),\ \mathrm{sort}\big[(\ell, \text{sig}(c)) : u \xrightarrow{\ell} c\big]\Big)
```

Sorting the edge list makes the signature independent of edge insertion order, so two nodes with the
same children (in any internal order) hash equal. libdictenstein computes `sig` as a single **64-bit
[FxHash](https://docs.rs/rustc-hash)** ([`src/node_signature.rs`](../../../src/node_signature.rs)):
`NodeSignature::compute(is_final, sorted[(label, child_hash)])`. Two nodes are candidates for merging
when their `u64` signatures are equal — an $`O(1)`$ comparison instead of a recursive set comparison.

**Guarding the hash.** A 64-bit hash can collide, so equal signatures trigger a **structural equality
re-check** before a merge is committed (documented in `node_signature.rs`): the two nodes are merged
only if they truly have identical `(is_final, sorted edges)`. This defeats the birthday-paradox
collision while keeping the common case a single integer compare. The payoff over a naive recursive
`Box<Signature>` comparison is large — one allocation-free `u64` instead of thousands of allocations
for a graph of a few thousand nodes.

<img src="../../diagrams/dawg-minimization.svg" alt="DAWG minimization by signature: leaf and near-leaf nodes are assigned signatures bottom-up; nodes sharing a signature (and confirmed structurally equal) are merged into one canonical node, and parent edges are redirected to the survivor, shrinking the trie into the minimal DAWG." width="620"/>

## Incremental construction (Daciuk's MADFA)

Building the trie in full and minimizing afterward costs $`O(n)`$ extra space for the un-minimized
trie. Daciuk et al. (2000, [10.1162/089120100561601](https://doi.org/10.1162/089120100561601)) give
an **incremental** algorithm — *Minimal Acyclic Deterministic Finite Automaton* construction — that
keeps the automaton minimal at every step, so it never materializes the full trie. Presented in
literate form:

```text
Register  ← ∅                        // map: signature → canonical node
insert(word):
    // 1. Follow the longest prefix of `word` already in the automaton.
    (last_common, suffix) ← walk as far as existing edges allow
    // 2. Before extending, minimize the part that can no longer change:
    //    any node on the active path whose subtree is now final gets
    //    replaced by its canonical representative from Register (or added
    //    to Register if new). This is the clone-on-shared-path step.
    make_path_unique(last_common)     // copy-on-write shared nodes first
    // 3. Append the fresh suffix as new nodes.
    append(suffix)
    // 4. Register the newly frozen nodes, deepest first.
    for node in new_nodes reversed:
        if Register has v with sig(v) == sig(node) and structurally_equal:
            redirect parent edge to v ; discard node    // merge
        else:
            Register.insert(sig(node), node)
```

Inserting terms in **sorted order** makes step 1 especially cheap (each insert diverges from the
previous only at its last differing character), which is why the batch constructors
(`from_terms`) sort first. The crate exposes both incremental minimization
(`minimize_incremental`, invoked as the DAWG mutates) and a full rebuild (`compact()`); see
[dynamic-dawg.md](../../algorithms/implementations/dynamic-dawg.md).

## Complexity

Let `n` be the total number of units across all terms and `N` the number of terms.

| Quantity | Cost |
|----------|------|
| Signature of one node | $`O(d \log d)`$ for out-degree `d` (the edge sort) |
| Register lookup / insert | $`O(1)`$ expected (hash map keyed by the `u64` signature) |
| Incremental `insert` | $`O(\lvert t\rvert \log \lvert\Sigma\rvert)`$ amortized for a term `t` |
| Full `compact()` | $`O(n)`$ |
| Space | the minimal DAWG — no two nodes with equal right language |

Membership after minimization is still $`O(\lvert q\rvert)`$ for a query `q`: minimization changes
the graph's *size*, never the per-query walk length.

## In this crate

- The signature substrate is [`src/node_signature.rs`](../../../src/node_signature.rs); the
  minimization driver and copy-on-write path handling are in
  [`src/dynamic_dawg/core.rs`](../../../src/dynamic_dawg/core.rs).
- The `u64` sequence DAWG ([dynamic-dawg-u64.md](../../algorithms/implementations/dynamic-dawg-u64.md))
  reuses the same signature theory over a `u64` alphabet.
- Suffix *sharing* is the DAWG; suffix *automata* (indexing every substring, not just whole terms)
  are the separate [`theory/scdawg/`](../scdawg/) series.

## References

- Blumer, A., et al. (1985). *The Smallest Automaton Recognizing the Subwords of a Text.* Theoretical
  Computer Science 40. [10.1016/0304-3975(85)90157-4](https://doi.org/10.1016/0304-3975(85)90157-4)
- Daciuk, J., Mihov, S., Watson, B. W., Watson, R. E. (2000). *Incremental Construction of Minimal
  Acyclic Finite-State Automata.* Computational Linguistics 26(1).
  [10.1162/089120100561601](https://doi.org/10.1162/089120100561601)
- Fredkin, E. (1960). *Trie Memory.* Communications of the ACM 3(9).
  [10.1145/367390.367400](https://doi.org/10.1145/367390.367400)
