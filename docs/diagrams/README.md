# Diagrams — sources, rendering, and conventions

Every figure in the `libdictenstein` documentation is **diagrams-as-code**: a
plain-text source under [`src/`](src/) that renders to a committed `.svg` in this
directory. The committed SVG is what the docs embed (via `<img …>`), so figures
render identically on **GitHub**, **docs.rs**, and any plain Markdown viewer — no
build step is required to *read* the docs, only to *change* a figure.

This convention follows the pgmcp **diagramming catalog**
(`toolbox_list domain=diagramming`): each illustration is authored with the tool
best suited to its shape, then rendered to SVG by a single script.

Mathematical labels inside a diagram follow the same standard as prose math — see the
[notation & terminology register](../notation.md). Because SVG is not Markdown, the delimiter rules
do not apply; instead, typeset math in a **PlantUML** label with its bundled JLaTeXMath
(`<latex>…</latex>` inline, `<math>…</math>` block) rather than pasting a Unicode glyph. Ordinary
flow arrows and cell separators stay Unicode.

---

## Layout

```
docs/diagrams/
├── README.md                  ← this file
├── <name>.svg                 ← committed, rendered artifact (embedded by docs)
└── src/
    ├── <name>.<ext>           ← diagram source (ext selects the renderer)
    └── puppeteer-config.json  ← headless-Chromium args for Mermaid under CI
```

Benchmark **plots** follow the same idea but live next to their data:
`docs/benchmarks/artifacts/<name>.gp` (a self-contained `gnuplot` script that
`set output`s `<name>.svg` in place).

---

## Renderer selection (by file extension)

| Ext | Tool | pgmcp catalog slug | Best for |
|-----|------|--------------------|----------|
| `.puml` | [PlantUML](https://plantuml.com/) | `plantuml` | UML, **sequence**, activity, component, C4, layered stacks |
| `.mmd` | [Mermaid](https://mermaid.js.org/) (`mmdc`) | `mermaid-cli` | ⚠️ **local/ad-hoc only — do not commit** (see below); flowchart, state, class, sequence, ER |
| `.d2` | [D2](https://d2lang.com/) | `d2` | modern **layered architecture** diagrams, clean orthogonal routing |
| `.dot` / `.gv` | [Graphviz](https://graphviz.org/) (`dot`) | `graphviz` | large **node-edge graphs** (dependency graphs, dense automata) |
| `.bytefield` | [bytefield-svg](https://bytefield-svg.deepsymmetry.org/) | `bytefield-svg` | *deprecated — renders too small; use PlantUML for byte/record layouts* |
| `.bob` | [Svgbob](https://github.com/ivanceras/svgbob) | `svgbob` | *deprecated — no root viewBox (fails R1); use PlantUML for ASCII-art schematics* |
| `.gp` | [gnuplot](http://www.gnuplot.info/) | `gnuplot` | **benchmark plots** (bars, lines, threshold bands) |

The dispatch script still has branches for `.bytefield`, `.bob`, and `.mmd`, but **no committed
source uses them** — they are kept only so an old source would still render. The `.gp` scripts emit
`set terminal svg` themselves.

> **Determinism note.** Committed artifacts use only the **deterministic, CI-installed** renderers
> — **PlantUML, D2, Graphviz, gnuplot** — so the CI byte-diff stays stable across machines. PlantUML
> covers state / sequence / activity / class / component / C4 and typesets math via bundled
> JLaTeXMath (`<latex>`); D2 covers flowcharts and layered architecture.
>
> **Do not commit a Mermaid (`.mmd`) source.** CI does **not** install `mmdc`, and
> `render-diagrams.sh` exits non-zero on a missing renderer, so a committed `.mmd` would fail the
> `diagrams` job. Mermaid rendering is also headless-Chromium and not byte-reproducible. Use it only
> for throwaway local sketches; author anything committed in PlantUML or D2.

---

## Rendering

```bash
# Render everything (the exact command CI runs):
scripts/render-diagrams.sh

# Render only specific sources:
scripts/render-diagrams.sh docs/diagrams/src/selector.puml

# On a box missing some renderers, skip them instead of failing:
ALLOW_MISSING_RENDERERS=1 scripts/render-diagrams.sh
```

The script is **idempotent** and **version-stable**: PlantUML's volatile
`<?plantuml $version$?>` processing-instruction is stripped after rendering so the
committed SVG is byte-identical across tool versions and machines. This is what
keeps the CI freshness gate (`git diff --exit-code -- docs/diagrams`) reliable.

After editing any source, **re-render and commit the updated `.svg` alongside it**.
CI re-renders and fails if a committed SVG is stale.

---

## House style (apply to every diagram)

Per the documentation guidelines, diagrams must be **fully colored with intuitive
colorization per concept**, complete end-to-end, and use the best actors per
component. The established palette (mirror it in new figures):

| Color | Hex | Concept |
|-------|-----|---------|
| 🟩 green | `#C8E6C9` | in-memory backends (always-on) |
| 🟨 amber | `#FFF59D` | feature `pathmap-backend` |
| 🟦 blue | `#BBDEFB` | feature `persistent-artrie` (disk-backed) |
| ⬜ neutral | `#F1F1F1` / `#37474F` | infrastructure / arrows / borders |

### Persistence-scoped accents

The persistent-ARTrie architecture docs under [`../persistence/`](../persistence/)
**extend — never replace** — the base palette above with accents that assign one
intuitive color to each durable-storage concept, so the same idea reads the same
way across every figure in the persistence corpus:

| Color | Hex | Concept |
|-------|-----|---------|
| 🟩 green | `#C8E6C9` | in-memory / client-facing / the live lock-free representation |
| 🟦 blue | `#BBDEFB` | persistent / disk-backed layers (block storage, disk managers, arena, image) |
| 🟦 teal | `#B2DFDB` | the lock-free **overlay engine** — the heart (`AtomicNodePtr`, `OverlayNode`, CAS) |
| 🟧 orange | `#FFCC80` | **WAL / durability log** (append + fsync, LSNs, commit-rank) |
| 🟪 indigo | `#C5CAE9` | **checkpoint / dense image** (fold, publish, `image_checkpoint_lsn`) |
| 🟥 red | `#FFCDD2` | **locks / serialization points** (the `CK > merge_lock > OR > EC` hierarchy) |
| ⬛ slate | `#CFD8DC` | the reusable **durable-storage kernel** substrate |
| 🟪 purple | `#E1BEE7` | **proofs / formal verification** (TLA⁺ models, Rocq specs) |
| ⬜ neutral | `#F1F1F1` / `#37474F` | infrastructure / arrows / borders |

Conventions:
- White background (`#FFFFFF`), `DejaVu Sans` font, dark slate arrows (`#37474F`).
- Put a **color key** in a comment at the top of each source and, where the format
  supports it, a rendered legend. Prefer a **swatch legend** — a table whose first
  column is a filled `|<#hex>|` cell and second column names the concept — over a prose
  list that spells out color names (`green identity · amber flags`), which is not
  reader-friendly and drifts from the actual fills.
- Name flows end-to-end; never leave a dangling edge or an undefined symbol.

---

## Rendering hygiene (so figures scale correctly in the viewer)

Docs are read in a viewer (`vinary-viewer`, `figures.cljs`) that sizes each embedded
`<img>` SVG by **font-matching**: it scales the whole figure so its dominant text size
equals the surrounding prose. That imposes rules on every rendered SVG;
`scripts/render-diagrams.sh --check` enforces the hard ones (also wired into the CI
`diagrams` job).

| # | Rule | Why |
|---|------|-----|
| **R1** | A **root `viewBox="0 0 W H"`** (4-number form) on the top-level `<svg>` | The viewer reads the *first* `viewBox`; with none on the root it picks up a nested `<marker>` arrowhead's tiny box and renders the figure at ~8 px. The render script auto-injects one if a renderer omits it. |
| **R2** | **One dominant font-size = the body text** | The viewer scales by the plurality `font-size`; if titles/labels out-number body text the whole figure mis-scales. Author body text at $`\approx`$14 px and keep it the most common size. |
| **R3** | **viewBox width $`\le`$ ~700 px** (`W ≲ 42 × dominant-font`) | Font-match only engages when the figure fits the text column; a wider canvas is capped-to-column and its text renders *smaller* than prose. Levers: vertical/top-down layout, concise labels, `skinparam wrapWidth`, split a dense figure in two, flip Graphviz `rankdir=LR→TB`, or stack side-by-side D2 groups with a hidden edge (`a -> b: "" { style.opacity: 0.0 }`). |
| **R4** | If a renderer emits no viewBox, the root must carry `width`/`height` in **plain-digit px** | The viewer's fallback path needs px (not `pt`/`%`/`em`). |

Additional hard rules the gate checks:

- **No baked PlantUML deprecation banner.** The leading-color activity form
  `#RRGGBB:label;` is deprecated in the installed PlantUML — it prints a warning *into*
  the SVG **and** silently drops the color. Use the trailing stereotype
  `:label;<<#RRGGBB>>` (see `src/selector.puml`).
- **No `svgbob` / `bytefield-svg`.** Both are retired: svgbob emits no root viewBox (R1)
  and bytefield renders too small. Byte / bit / struct layouts are compact PlantUML
  **byte tables** — a Creole table hosted in a floating `note` (with a `title` above and
  a swatch `legend` below): one row per field, an offset (or `field : type`) column plus
  a description column, every cell filled with its house color via `|<#hex>|`. Because
  table rows share borders, the fields render **contiguously** (no gaps) — the correct
  depiction of adjacent bytes, unlike a stack of separate `rectangle`s which the layout
  engine staggers (see `src/file-header.puml`, `src/wal-record.puml`).

Three more PlantUML gotchas (render wrong rather than fail the gate):

- **`skinparam` blocks must be multi-line** — a single-line `skinparam rectangle { A B }`
  is a syntax error.
- **A literal `|` is the Creole column separator** — inside a byte-table (or legend) cell
  it opens a new column and breaks the row. Rephrase with `·` or `/` (e.g. wal-header's
  `magic = 'PARTWAL'+NUL · 'PARTWALO'`). Single `_underscores_` are literal; only
  `__double__` underlines.
- **A literal `\"` renders as a visible backslash-quote.** Use single quotes `'x'` or
  typographic `"x"` inside labels.

---

## Adding a diagram

1. Pick the tool from the table above that best fits the illustration's shape.
2. Create `src/<name>.<ext>`; open with a comment block stating the **color key**
   and what the figure shows.
3. `scripts/render-diagrams.sh docs/diagrams/src/<name>.<ext>`.
4. Check hygiene: `scripts/render-diagrams.sh --check` (root viewBox, no deprecation
   banner) and confirm the rendered `viewBox` width is $`\le`$ ~700 px per R3 — narrow it if not.
5. Embed in the target doc: `<img src="…/docs/diagrams/<name>.svg" alt="…" width="…"/>`.
6. Commit the source **and** the rendered `.svg` together.

---

## Tool availability

All renderers may be installed on the development host (`plantuml`, `mmdc`, `d2`,
`dot`, `gnuplot` on `PATH`; `bytefield-svg` in the workspace `node_modules/.bin`).
The Kroki HTTP gateway is **not** required — each CLI is driven directly. The CI
`diagrams` job installs pinned versions of PlantUML 1.2026.5, D2 0.7.1, Graphviz, gnuplot,
svgbob 0.7.6, and bytefield-svg 1.11.0 before re-rendering (the last two installed but unused). It
deliberately does **not** install `mmdc`, which is why a committed `.mmd` source (see the
determinism note above) would fail the job.
