# Diagram-Rendering Overhaul — Ledger

A scientific ledger of the diagram-rendering-quality fix: making every figure in `docs/`
render crisply and prose-matched in the docs viewer (`vinary-viewer`, whose `figures.cljs`
font-matches each embedded `<img>` SVG to the surrounding prose).

## Problem (five defects, agent-verified)

| # | Defect | Root cause |
|---|--------|-----------|
| 1 | 6 PlantUML activity diagrams carried a baked "deprecated" banner **and rendered un-colored** | The leading-color activity form `#RRGGBB:label;` is deprecated in the installed PlantUML — it prints a warning *into* the SVG and silently drops the fill |
| 2 | 6 `svgbob` SVGs rendered as a tiny/blank square | No root `viewBox`; the viewer matched a nested `<marker>` arrowhead's ~8 px box instead |
| 3 | 2 `bytefield` SVGs rendered too small | 322 px canvas, 11 px dominant font |
| 4 | ~49 ASCII-art diagrams were fixed-width monospace, not graphical | Never converted to diagrams-as-code |
| 5 | Wide canvases render sub-prose text | Font-match only engages when `viewBox` width ≲ 42×dominant-font (~700 px); wider figures are capped-to-column and shrink |

## The viewer's contract (`vinary-viewer/src/vinary/renderer/figures.cljs`)

- **R1** root `viewBox="0 0 W H"` (read first; nested-marker viewBox ≠ root).
- **R2** one dominant font-size = body text (scales by the plurality `font-size`).
- **R3** `W ≤ ~700 px` (`W ≲ 42×dominant-font`) or text renders smaller than prose.
- **R4** no-viewBox fallback needs `width`/`height` in plain-digit px.

These are now documented in `docs/diagrams/README.md` and enforced (the hard ones) by
`scripts/render-diagrams.sh --check`, wired into the CI `diagrams` job.

---

## Phases

### P1 — PlantUML deprecation fix — ☑ DONE
Transformed every leading-color activity action `#RRGGBB:label;` → `:label;<<#RRGGBB>>`
(the `selector.puml` form) across the 6 affected sources (`cas-walk`, `recovery-flow`,
`dawg-minimization`, `rank-regime-replay`, `verification-ci-flow`, `vocab-recovery`) —
51 lines, greedy-to-last-`;` to preserve labels with internal `;`; special-cased the
`backward :#FFCDD2:…;` line. **Result:** 0 baked "deprecated" text; action-node colors
restored (e.g. `cas-walk` 19 accent fills, `recovery-flow` 21 — previously legend-only).

### P7 — Render-pipeline hygiene safeguard — ☑ DONE
Hardened `scripts/render-diagrams.sh`: (a) `ensure_root_viewbox` auto-injects
`viewBox="0 0 W H"` (from width/height) when a renderer omits it on the root — idempotent
+ byte-stable; (b) a `--check` gate fails on a baked deprecation banner or a missing root
viewBox and warns on `viewBox` width > 700 px. Fixed a line-based bug (Graphviz `<svg>`
tags span lines) by joining lines before the tag scan. Wired `--check` into CI.
**Result:** `--check` → 0 hard failures crate-wide.

### P2/P3 — Retire svgbob + bytefield → compact PlantUML byte-fields — ☑ DONE
Deleted the 6 `.bob` + 2 `.bytefield` sources; re-authored all 8 as PlantUML with a new
**byte-field template** (a vertical stack of colored `rectangle`s, one per field/tier,
offsets on the left) or before/after `package`s. All carry a root viewBox, dominant font
14, and fit the budget:

| diagram | kind | width |
|---|---|---|
| `file-header` | byte-field (64 B) | 496 |
| `node-header` | byte-field (16 B) | 614 |
| `wal-header` | byte-field (64 B) | 514 |
| `wal-record` | byte-field + type list | 564 |
| `node-layouts` | N4/16/48/256 tiers | 610 |
| `path-compression` | before/after | 613 |
| `burst-trie` | before/after burst | 640 |
| `swizzled-ptr` | 64-bit word + states | 676 |

### P4 — Convert live-corpus ASCII diagrams → PlantUML/Graphviz — ☑ DONE
49 ASCII diagrams converted across 3 parallel workstreams, each rendered, width-verified
(≤ 700 px for PlantUML), and embedded via `<img … alt=… width="70%">`:
- **disk-tries** (20): `btrie-*`, `art-*`, `part-*`, `buffer-manager-stack` — all ≤ 637.
- **scdawg + lock-free-CAS** (13): `scdawg-*` (Graphviz), `lockfree-*` (PlantUML) — all ≤ 700 (Graphviz sized separately, see P6).
- **algorithms + integration** (16): `algorithms-backend-family`, `*-selector`, `pathmap-*`, `dat-*`, `cx-compact-node`, `eviction-checkpoint-flow` — all ≤ 678.

**Bug found + fixed (crate-wide):** this PlantUML renders a literal `\"` as a visible
backslash-quote. Fixed in the byte-field templates (`file-header`, `wal-header`,
`path-compression`) and the lock-free sources (`lockfree-path-copy-cat`,
`lockfree-insert-recursive`) by switching to single/typographic quotes; the two P4 ASCII
workstreams fixed their own. Data-tables and file/dir trees were left for P5 / kept as text.

### P5 — Box-drawing data-tables → native Markdown tables — ☑ DONE (core)
Converted every `┌┬┐` data table (a robust box-drawing→Markdown parser, verified per file
to touch only genuine tables and keep code-fence parity): the 2 io_uring benchmark tables
and the 6 memory-overhead tables (`double-array-trie[-char]`, `dynamic-dawg[-char]`,
`pathmap-dictionary`, `suffix-automaton`). **Result:** 0 `┌` box-drawing blocks remain in
any live doc; file/dir trees left as ```text per the scope decision.

### P6 — Narrow the inherently-wide diagrams — ☑ DONE
Per-renderer levers (proven on flagship diagrams by hand, then applied at scale via 4
parallel workstreams): **D2** — shorten labels, stack side-by-side groups with a hidden
`opacity:0` edge; **Graphviz** — flip `rankdir=LR→TB`, tighten `nodesep`/`ranksep`;
**PlantUML** — shorten labels, `skinparam wrapWidth` (does NOT wrap `title` or state/edge
labels — those need manual `\n`), `maxMessageSize` for sequences, relocate `note right`→
`bottom`, split a too-rich figure in two.

**Result: the > 700 px count fell 38 → 0 — every diagram now fits the ≤ 700 px budget.**
The Graphviz `LR→TB` flip did the heavy lifting (`suffix-links` 1235→431, `scdawg-structure`
1320→554, `f4-lock-hierarchy` 1147→536); most PlantUML diagrams reached ≤ 700 by
label/`wrapWidth` trimming. The final 10 that could not shrink further without deleting
content were **split into ≤ 700 companions** or **redesigned** — never gutted:

| diagram | before | resolution (all parts ≤ 700) |
|---|---:|---|
| `docs-reading-order` | 3670 | **split** → 682 + `…-2` 467 (tracks 1–2 / 3–4; all 17 nodes, 20 edges) |
| `proof-artifact-map` | 2812 | **split** → 570 + `…-2` 584 (TLA⁺ prong / Rocq prong) |
| `persistence-stack` | 2371 | **split** → 497 + `…-2` 578 (layers + flows / checkpoint + cross-cutting) |
| `selector` | 2064 | **split** → 574 + `…-2` 684 (substring / prefix-word; all 9 leaves) |
| `dawg-minimization` | 2003 | **redesign** → 577 (branch columns → vertical cards; all 4 cases + collision guard) |
| `scdawg-extension-graphs` | 1988 | **split** → 609 + `…-2` 617 (right / left extension) |
| `rank-regime-replay` | 1904 | **redesign** → 446 (branch columns → vertical cards; all 3 cases) |
| `committed-watermark` | 1688 | **redesign** → 624 (8-cell row wrapped 2×4; all 8 LSNs) |
| `buffer-page-lifecycle` | 1150 | **split** → 518 + `…-2` 589 (top cycle / Resident substates) |
| `scdawg-citation-lineage` | 854 | **restructure** → 537 (Author-year labels; all 9 nodes, 6 DOIs) |

**Seven diagrams were split** (the four above + the earlier `architecture`, `traits`,
`kernel-substrate`), three were **redesigned** to a vertical layout, and one **restructured** —
all content-preserving (leaf/branch/node inventories verified against the committed originals).
Each part-2 companion is embedded next to its part-1 (`README.md`, `src/lib.rs` rustdoc,
`docs/persistence/*`, `docs/theory/scdawg/*`, `docs/README.md`). Hard-failure count stayed 0.

### P8 — Regenerate, verify, finalize — ☑ DONE
- **Full render:** 125 diagrams, exit 0, 0 errors/exceptions.
- **Idempotent / byte-stable:** a 2nd identical render changed **0** SVGs → the CI freshness
  gate (`git diff --exit-code -- docs/diagrams`) passes.
- **Hygiene `--check`:** **0 hard failures AND 0 width warnings** — every SVG has a root
  viewBox, no deprecation banner, and width ≤ 700 px.
- **Embeds:** **128** local `<img>` embeds resolve, **0 broken** (incl. all 7 split
  companions); the `src/lib.rs` `traits-2.svg` raw-URL target is present locally.
- **`\"` literal-render bug:** 0 remaining across all diagram sources.
- **Docs:** `docs/diagrams/README.md` hygiene section added; this ledger complete.

---

### P9 — Byte-layout contiguity + swatch legends — ☑ DONE

**Defect.** The P2/P3 byte-field template (a vertical stack of `rectangle`s joined by
`-[hidden]down-`) fixed *size* but rendered **non-contiguously**: PlantUML's Graphviz
layout content-sizes each box, centers it, and inserts ~60 px vertical gaps between
36 px boxes — so the cells neither align nor touch, misrepresenting the contiguous
bytes they describe. Two follow-ons: `buffer-page-lifecycle-2` was *monotonically
green* (all four Resident substates + the composite `#C8E6C9`), and every legend named
colors in prose (`green identity · amber flags`) rather than showing swatches.

**Fix — contiguous Creole byte tables.** Re-authored all **16** byte/bit/struct layouts
as a Creole table hosted in a floating `note` (a `title` above, a swatch `legend`
below): one row per field, an offset (or `field : type`) column + a description column,
every cell filled via `|<#hex>|`. Table rows share borders → **0 gaps, aligned
columns** by construction. `node-layouts` (four *distinct* node tiers, not one
structure) became a per-tier comparison table (tier · fan-out · layout), not a fused
byte strip. Heights collapse as the gaps vanish and widths hold or shrink:

| diagram | before (W×H) | after (W×H) |
|---------|-------------:|------------:|
| `file-header` | 496 × 1030 | 425 × 421 |
| `node-header` | 614 × 724 | 563 × 323 |
| `wal-header` | 514 × 644 | 498 × 323 |
| `wal-record` | 564 × 678 | 483 × 404 |

**Recolor.** `buffer-page-lifecycle-2` now encodes clean/dirty × pinned/unpinned as a
warm→cool gradient — green `#C8E6C9` (clean·unpinned) · teal `#B2DFDB` (clean·pinned) ·
red `#FFCDD2` (dirty·pinned, flush-blocked) · orange `#FFCC80` (dirty·unpinned,
must-flush) — over a neutral `#F1F1F1` composite frame; the red `#C62828` `mark_dirty`
arrow is unchanged.

**Swatch legends.** All **13** prose color-key legends converted to two-column swatch
tables (`|<#hex>|` cell + concept), and the four redrawn diagrams that lacked a legend
gained one.

**New gotcha (recorded in README):** a literal `|` is the Creole column separator —
`wal-header`'s `magic … | 'PARTWALO'` and `rank_regime (Owned = 0 | Overlay = 1)` were
rephrased with `·` so the table rows don't split.

**Verify:** full render exit 0; `git diff` touched only the intended SVGs; a 2nd render
was byte-identical (freshness gate green); `--check` = 0 hard failures; every touched
SVG's viewBox width ≤ 700 px; cell geometry confirms exact contiguity
(`y_{n+1} = y_n + height_n`).

---

## Status snapshot (final)

- **Hard hygiene (R1 + no-deprecation): met crate-wide** — `--check` = 0 failures; render byte-stable.
- **svgbob + bytefield: eliminated** (0 remaining); byte layouts are contiguous PlantUML byte tables (P9).
- **ASCII diagrams: converted** (49 anchored + 3 long-tail); **data-tables → Markdown** (0 `┌` left).
- **Width (R3): 38 → 0 over-budget** — every diagram ≤ 700 px (the last 10 resolved by content-preserving split/redesign/restructure).
- **3 diagrams split**; part-2 companions embedded in `README.md` / `src/lib.rs` / `durable-storage-kernel.md`.
- **Guardrail:** `render-diagrams.sh --check` (root viewBox + no deprecation) wired into the CI `diagrams` job.
