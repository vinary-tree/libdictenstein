# Notation & terminology register

This is the single source of truth for **how mathematics and key terms are written** across the
libdictenstein documentation. It exists so that every document — theory, algorithms, architecture,
persistence, security — spells the same idea the same way, and so that a mechanical gate can
enforce it.

Two authorities, cleanly divided:

- [`docs/README.md` § Authoring conventions](README.md#authoring-conventions--math-in-markdown)
  owns **delimiters** — the GitHub-Flavored-Markdown rules that decide *which characters* wrap a
  math span so MathJax receives it intact.
- **This file** owns **symbols and terminology** — *what LaTeX* goes inside a span, and *which
  word* names each concept.

Each links to the other; neither restates the other. Both are enforced by
[`scripts/check-doc-math.py`](../scripts/check-doc-math.py), which runs in the `diagrams` CI job.

---

## 1. The delimiter rule that decides everything: length is `\lvert … \rvert`

A length or cardinality bar is written `\lvert x \rvert` (and a norm `\lVert x \rVert`) — **never**
a bare `|`, and **never** `\mid`. This one rule is the reason a whole prior migration had to be
undone, so it is worth stating why.

**Table-safety is necessary but not sufficient.** In a Markdown table a literal `|` closes the
current cell, so `|q|` inside a table row silently splits the row. Both `\mid` and `\lvert…\rvert`
avoid the literal pipe, so both survive a table cell — which is exactly why the migration reached
for `\mid`.

**Spacing is the deciding axis.** In TeX every symbol has a *class* that fixes the space around it:

- `\mid` is a **relation** (`\mathrel`, class 3). It is set with a medium space on *both* sides and
  does not grow or nest, so a length written with it — $`\lvert q\rvert`$ done wrong — comes out
  with the bars floating in relation-space, the very look of the corrupted corpus.
- `\lvert` is an **opening** delimiter (`\mathopen`, class 4) and `\rvert` a **closing** one
  (`\mathclose`, class 5). They take no surrounding space, nest correctly, and grow under
  `\left…\right`, so the same length renders as the tight, upright $`\lvert q\rvert`$.

`\lvert…\rvert` is therefore the **unique** form that is both table-safe *and* correctly spaced. It
is mandatory. `\mid` is forbidden as a delimiter, and — because the only legitimate relational use
in this corpus is set-builder "such that", which we spell with a colon (§3) — `\mid` is forbidden
outright, which makes the gate a simple substring check.

```text
Write this:   $`\lvert q \rvert`$      $`\lVert v \rVert`$      $`\lvert \Sigma \rvert`$
Not this:     $`\mid q\mid `$          $`|q|`$ (breaks tables)
```

---

## 2. Big-O and asymptotic notation are MathJax, not code spans

Asymptotic bounds are mathematics, so they are inline math with an **upright roman** `O`, never an
inert code span. `contains` answers a lookup in $`O(\lvert q\rvert)`$; a double-array trie build is
$`O(N \log N)`$; a cached hit is $`\Theta(1)`$.

```text
Write this:   $`O(1)`$    $`O(\lvert q\rvert)`$    $`\Theta(n)`$    $`O(N \log N)`$
Not this:     `O(1)`      `O(|q|)`                 (inert code spans — no math is typeset)
```

A single-backtick code span is reserved for a big-O expression that is *literally source code* (it
is not, anywhere in these docs). Identifiers such as `O_DIRECT`, `Option<V>` and the like remain
code spans and are correctly left alone by the gate — it only flags a code span whose whole content
is a big-O bound or a lone Greek letter.

---

## 3. Sets, relations, and the atoms of a formula

| Concept | Write it as | Renders |
|---|---|---|
| Set-builder "such that" | `\{\, x : P(x) \,\}` | $`\{\, x : P(x) \,\}`$ |
| Set braces (literal) | `\{ \dots \}` | $`\{ \dots \}`$ |
| Subtraction / negation | ASCII `-` | $`2n - 1`$ |
| Multiplication | `\cdot` | $`2 \cdot \lvert T\rvert`$ |
| Reduction / scaling factor | `\times` | $`2\times`$ to $`4\times`$ fewer I/Os |
| Mean / overline | `\bar{s}` | $`O(\lvert k\rvert / \bar{s})`$ |
| Maps-to / transition | `\to` | $`\delta : Q \times \Sigma \to Q`$ |
| Implies / iff | `\Rightarrow`, `\Leftrightarrow` | $`a \Rightarrow b`$ |
| Membership / subset | `\in`, `\subseteq` | $`x \in \Sigma^{*}`$ |
| Empty string / set | `\varepsilon`, `\emptyset` | $`\varepsilon`$, $`\emptyset`$ |

Two hazards the gate catches, both from the earlier migration:

- **No Unicode inside a math body.** A pasted glyph renders as an un-typeset literal, or silently
  vanishes. Every one has an ASCII LaTeX command; use it:

```text
    −  (U+2212 minus)          → -            ≤ → \le        ≥ → \ge
    ×  (multiplication)        → \times       ∈ → \in        ∪ → \cup
    Σ  (Greek sigma)           → \Sigma       ⊆ → \subseteq  → → \to
    s̄  (combining macron)      → \bar{s}      … → \dots      ⇒ → \Rightarrow
```

- **No bare braces for a set.** `{x}` in a math body is *invisible grouping* to MathJax — the
  braces never appear. Write `\{ x \}`; add thin spaces for a set-builder, `\{\, x : P \,\}`.

```text
Set-builder — write this:   $`\{\, t : s = h \cdot t \in V \,\}`$
           — not this:      $`{t \mid s = h·t ∈ V}`$   (\mid relation · bare braces · unicode)
```

---

## 4. Multi-letter identifiers inside math

A word inside a math body is a run of italic single-letter variables unless you say otherwise.
`$`value \to term`$` renders as the *product* `v·a·l·u·e → t·e·r·m`. Wrap any multi-letter name in
`\text{…}`.

```text
Write this:   $`\text{value} \to \text{term}`$      $`\text{acknowledged} \Rightarrow \text{durable}`$
Not this:     $`value \to term`$                    $`acknowledged \implies durable`$
```

This rule cannot be mechanically enforced — a gate cannot distinguish an intended product `xy` from
the identifier `xy` without a full LaTeX parser — so it is a review-time obligation. The standard
LaTeX operators (`\log`, `\max`, `\min`, `\det`, …) are already upright and need no `\text{}`.

---

## 5. Symbol register

Every symbol used mathematically across the docs, its LaTeX, and its meaning. Defined here once so
individual documents may use it without re-deriving it (they should still gloss a symbol on first
use in prose).

| Symbol | LaTeX (inside a math span) | Meaning |
|---|---|---|
| $`\Sigma`$ | `\Sigma` | the **alphabet** — the set of units an edge may carry |
| $`\lvert \Sigma \rvert`$ | `\lvert \Sigma \rvert` | alphabet cardinality (256 for bytes; all scalar values for `char`) |
| $`\Sigma^{*}`$ | `\Sigma^{*}` | the set of all finite strings over $`\Sigma`$ |
| $`\varepsilon`$ | `\varepsilon` | the empty string |
| $`\lvert x \rvert`$ | `\lvert x \rvert` | length of string/sequence `x`, in units |
| $`q`$ | `q` | a **query** string presented to a lookup |
| $`N`$ | `N` | number of **terms** stored in the dictionary |
| $`n`$ | `n` | total indexed size in units (context-stated per document) |
| $`m`$ | `m` | length of one key/term under discussion (context-stated) |
| $`O(\cdot), \Theta(\cdot), \Omega(\cdot)`$ | roman `O`; `\Theta`; `\Omega` (each with a `(\cdot)` argument) | asymptotic upper / tight / lower bound |
| $`\delta`$ | `\delta` | a transition function, $`\delta : Q \times \Sigma \to Q`$ |
| $`\bar{s}`$ | `\bar{s}` | mean of `s` (e.g. mean compressed edge span) |

`n` and `m` are overloaded across the literature; **state their meaning in each document** before
use. Everything else above is global.

---

## 6. Terminology register

One canonical term per concept. The rejected synonyms are not wrong English — they are banned only
to keep the corpus uniform and greppable.

| Canonical | Rejected synonyms | Definition |
|---|---|---|
| **unit** | symbol, character (as the atom), letter | The atomic element of a key: a `u8` byte, a `char`, or a `u64` token. Modeled by the [`CharUnit`](architecture/abstractions.md) trait. |
| **alphabet** ($`\Sigma`$) | symbol set | The set of units an edge may carry. |
| **term** | word, string, key (as a stored entry) | A string stored in the dictionary. |
| **query** | needle, lookup string | The input string of a lookup, $`q`$. |
| **pattern** | — (scoped) | The searched-for string in **substring-search** theory only, where the literature (Blumer, Crochemore) fixes the word. Elsewhere use *query*. |
| **fanout** | arity, branching factor | A node's child count. ("Arity" is reserved — it names *budget arity* in the persistence coordinator, a different concept.) |
| **edge label** | edge symbol | The string on a **path-compressed** edge. When an edge carries exactly one unit, say *unit*; reserve *edge label* for compressed multi-unit edges. |
| **final** (node/state) | accepting, terminal | A node that completes a stored term. |
| **overlay** | delta layer, write buffer | The in-memory lock-free layer above a persistent dense image. |

> **Architecture note.** The edge-label abstraction for the in-memory dictionaries is
> [`CharUnit`](architecture/abstractions.md) (`u8` / `char` / `u64`). The *persistent* key model is a separate
> abstraction, `KeyEncoding` (`ByteKey` / `CharKey` / `U64Key`); it is not a synonym for `CharUnit`
> and the two are documented distinctly.

---

## 7. Citations

Every citation resolves to a real work, and to its DOI wherever one exists (linked as
`https://doi.org/…`). A work with no DOI (a technical report, a book, a blog post) is cited in full
without one. The crate-wide bibliography lives in the root [`README.md` § References](../README.md#references);
per-topic reference lists (e.g. [`theory/scdawg/07-references.md`](theory/scdawg/07-references.md))
extend it.

---

## 8. What the gate enforces

[`scripts/check-doc-math.py`](../scripts/check-doc-math.py) fails CI on:

| Rule | Rejects | This register § |
|---|---|---|
| `inline` / `display` | bare `$…$` / `$$…$$` delimiters | [README](README.md#authoring-conventions--math-in-markdown) |
| `letter-before` | an ASCII letter abutting an opening `` $` `` | [README](README.md#authoring-conventions--math-in-markdown) |
| `literal-dollar` | two or more `\$` on one line | [README](README.md#authoring-conventions--math-in-markdown) |
| `mid-delimiter` | `\mid` anywhere | §1, §3 |
| `unicode-in-math` | a non-ASCII glyph inside a math body | §3 |
| `inert-code-math` | big-O / bare-Greek as a code span | §2 |

The last three run on every non-archival file. The frozen historical ledgers under
`docs/design/history/` and the `*ledger*.md` / `*handoff*.md` records are exempt from them — they
are a scientific record, not living documentation. Run the gate before committing docs:

```bash
python3 scripts/check-doc-math.py --selftest   # prove the rules fire
python3 scripts/check-doc-math.py               # scan every tracked *.md
```
