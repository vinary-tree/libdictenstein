# Documentation Overhaul — Scientific Ledger

Tracks the comprehensive documentation overhaul that brings every durable doc into
conformance with pgmcp's documentation guidelines and replaces hand-drawn ASCII
diagrams with rendered, fully-colored figures authored as diagrams-as-code.

- **Plan**: `~/.claude/plans/utilize-the-documentation-guidelines-gentle-lighthouse.md`
- **Strategy**: committed SVG, scripted (`scripts/render-diagrams.sh`) + CI-gated.
- **Started**: 2026-06-19.

## Conventions
- One row per deliverable: phase · item · action · status · verification.
- Status: ☐ pending · ◐ in-progress · ☑ done.
- Diagram IDs (`#N`, `Fn`) reference the diagram inventory in the plan.

---

## Phase 0 — Diagram tooling + CI gate  ☑

| Item | Action | Status | Verification |
|------|--------|--------|--------------|
| `scripts/render-diagrams.sh` | New extension-dispatch renderer (puml/mmd/d2/dot/bytefield/gp); PlantUML version-PI normalized for byte-stable output | ☑ | idempotent (byte-identical on 2nd run); 4 SVGs reproduce with correct dims |
| `docs/diagrams/src/` | `git mv` the 4 existing `.puml` here; sources separated from artifacts | ☑ | `ls docs/diagrams/src/*.puml` → 4 files |
| `docs/diagrams/README.md` | Conventions: renderer-by-extension table, house palette, add-a-diagram steps, determinism note | ☑ | written |
| `docs/diagrams/src/puppeteer-config.json` | Headless-Chromium `--no-sandbox` args for Mermaid (local/ad-hoc only) | ☑ | written |
| `.github/workflows/ci.yml` `diagrams` job | Pinned plantuml 1.2026.5 / d2 0.7.1 / bytefield-svg 1.11.0 → render → `git diff --exit-code` freshness gate | ☑ | YAML parses; 13 jobs incl. `diagrams` |

**Result**: rendering pipeline reproducible and CI-gated. Deterministic renderers
(PlantUML/D2/Graphviz/bytefield-svg/gnuplot) chosen for committed artifacts;
Mermaid available locally but excluded from the gate (Chromium not byte-reproducible).

---

## Phase 1 — Fix docs/algorithms/ rot  ☐
_(32 stale `*_char::` paths · 35 dead `../0X-*` links · mangled benchmark table)_

## Phase 2 — Front door + diagrams  ☐
## Phase 3 — Theory + algorithms refresh + diagrams  ☐
## Phase 4 — Persistence, eviction, formal-verification + diagrams  ☐
## Phase 5 — New conceptual docs  ☐
## Phase 6 — Benchmark plots  ☐
## Phase 7 — Reorganization + top-level index  ☐
