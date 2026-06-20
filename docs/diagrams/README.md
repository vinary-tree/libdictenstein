# Diagrams — sources, rendering, and conventions

Every figure in the `libdictenstein` documentation is **diagrams-as-code**: a
plain-text source under [`src/`](src/) that renders to a committed `.svg` in this
directory. The committed SVG is what the docs embed (via `<img …>`), so figures
render identically on **GitHub**, **docs.rs**, and any plain Markdown viewer — no
build step is required to *read* the docs, only to *change* a figure.

This convention follows the pgmcp **diagramming catalog**
(`toolbox_list domain=diagramming`): each illustration is authored with the tool
best suited to its shape, then rendered to SVG by a single script.

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
| `.mmd` | [Mermaid](https://mermaid.js.org/) (`mmdc`) | `mermaid-cli` | **flowchart**, **state**, class, sequence, ER — GitHub/rustdoc-native shapes |
| `.d2` | [D2](https://d2lang.com/) | `d2` | modern **layered architecture** diagrams, clean orthogonal routing |
| `.dot` / `.gv` | [Graphviz](https://graphviz.org/) (`dot`) | `graphviz` | large **node-edge graphs** (dependency graphs, dense automata) |
| `.bytefield` | [bytefield-svg](https://bytefield-svg.deepsymmetry.org/) | `bytefield-svg` | **byte / record / bitfield** layouts (WAL records, node headers) |
| `.bob` | [Svgbob](https://github.com/ivanceras/svgbob) | `svgbob` | **ASCII-art schematics** (node-storage layouts, before/after trees, bit fields) |
| `.gp` | [gnuplot](http://www.gnuplot.info/) | `gnuplot` | **benchmark plots** (bars, lines, threshold bands) |

`.bytefield` sources are written in bytefield-svg's Clojure-flavored DSL; `.gp`
scripts emit `set terminal svg` themselves.

> **Determinism note.** Committed artifacts use the **deterministic** renderers
> (PlantUML, D2, Graphviz, bytefield-svg, gnuplot) so the CI byte-diff stays
> stable across machines. PlantUML covers state / sequence / activity / class /
> component / C4; D2 covers flowcharts and layered architecture. Mermaid (`.mmd`)
> is supported by the script for local/ad-hoc use, but its headless-Chromium
> rendering is **not** guaranteed byte-reproducible across environments — prefer
> PlantUML or D2 for anything committed.

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

Conventions:
- White background (`#FFFFFF`), `DejaVu Sans` font, dark slate arrows (`#37474F`).
- Put a **color key** in a comment at the top of each source and, where the format
  supports it, a rendered legend.
- Name flows end-to-end; never leave a dangling edge or an undefined symbol.

---

## Adding a diagram

1. Pick the tool from the table above that best fits the illustration's shape.
2. Create `src/<name>.<ext>`; open with a comment block stating the **color key**
   and what the figure shows.
3. `scripts/render-diagrams.sh docs/diagrams/src/<name>.<ext>`.
4. Embed in the target doc: `<img src="…/docs/diagrams/<name>.svg" alt="…" width="…"/>`.
5. Commit the source **and** the rendered `.svg` together.

---

## Tool availability

All renderers are installed on the development host (`plantuml`, `mmdc`, `d2`,
`dot`, `gnuplot` on `PATH`; `bytefield-svg` in the workspace `node_modules/.bin`).
The Kroki HTTP gateway is **not** required — each CLI is driven directly. The CI
`diagrams` job installs pinned versions of each tool before re-rendering.
