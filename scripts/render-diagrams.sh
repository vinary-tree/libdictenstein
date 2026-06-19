#!/usr/bin/env bash
# ============================================================================
# render-diagrams.sh — render all diagrams-as-code sources to committed SVGs.
#
# Diagram SOURCES live in  docs/diagrams/src/<name>.<ext>  and render to
# committed artifacts at    docs/diagrams/<name>.svg.
# Benchmark PLOT sources are self-contained gnuplot scripts at
#                           docs/benchmarks/artifacts/<name>.gp
# that `set output` themselves (rendered in place).
#
# The file extension selects the renderer (pgmcp diagramming catalog):
#   .puml       → PlantUML        (UML / sequence / activity / C4)
#   .mmd        → Mermaid (mmdc)  (flowchart / state / class / sequence / ER)
#   .d2         → D2              (modern layered architecture diagrams)
#   .dot|.gv    → Graphviz dot    (large node-edge graphs)
#   .bytefield  → bytefield-svg   (byte / record / bitfield layouts)
#   .gp         → gnuplot         (benchmark plots; under benchmarks/artifacts)
#
# This script is the single source of truth for diagram rendering and is the
# exact command the CI `diagrams` freshness job runs before
#   git diff --exit-code -- docs/diagrams docs/benchmarks/artifacts
# so a stale committed SVG fails CI.
#
# Usage:
#   scripts/render-diagrams.sh            # render everything (strict)
#   scripts/render-diagrams.sh path…      # render only the named source files
#   ALLOW_MISSING_RENDERERS=1 scripts/render-diagrams.sh   # skip absent tools
#
# Tool locations may be overridden via env: PLANTUML, MMDC, D2, DOT,
# BYTEFIELD_SVG, GNUPLOT. Node-based tools (bytefield-svg) are also probed in
# the workspace node_modules/.bin and via `npx`.
# ============================================================================
set -euo pipefail

# --- locate the repository root ---------------------------------------------
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

SRC_DIR="docs/diagrams/src"
OUT_DIR="docs/diagrams"
BENCH_DIR="docs/benchmarks/artifacts"

ALLOW_MISSING="${ALLOW_MISSING_RENDERERS:-0}"
rendered=0
skipped=0

# --- tool resolution --------------------------------------------------------
# Resolve a CLI: honor an env override, else search PATH, else (for node tools)
# the workspace node_modules/.bin, else `npx <pkg>`. Echoes a runnable command
# or empty string if unavailable.
node_bin_dirs=(
  "$REPO_ROOT/node_modules/.bin"
  "$REPO_ROOT/../node_modules/.bin"
)

resolve_node_tool() {  # $1=env-var-name $2=binary $3=npx-package
  local override="${!1:-}"
  if [[ -n "$override" ]]; then echo "$override"; return; fi
  if command -v "$2" >/dev/null 2>&1; then echo "$2"; return; fi
  local d
  for d in "${node_bin_dirs[@]}"; do
    if [[ -x "$d/$2" ]]; then echo "$d/$2"; return; fi
  done
  if command -v npx >/dev/null 2>&1; then echo "npx --no-install $3"; return; fi
  echo ""
}

PLANTUML_CMD="${PLANTUML:-plantuml}"
MMDC_CMD="${MMDC:-mmdc}"
D2_CMD="${D2:-d2}"
DOT_CMD="${DOT:-dot}"
GNUPLOT_CMD="${GNUPLOT:-gnuplot}"
BYTEFIELD_CMD="$(resolve_node_tool BYTEFIELD_SVG bytefield-svg bytefield-svg)"

# Optional puppeteer config so mmdc runs headless-Chromium under CI sandboxes.
MMDC_PUPPETEER="$SRC_DIR/puppeteer-config.json"

have() { command -v "${1%% *}" >/dev/null 2>&1; }

missing_tool() {  # $1=tool label
  if [[ "$ALLOW_MISSING" == "1" ]]; then
    echo "  ⚠ skipping — renderer '$1' not found (ALLOW_MISSING_RENDERERS=1)"
    skipped=$((skipped + 1))
    return 0
  fi
  echo "  ✗ renderer '$1' not found. Install it or set ALLOW_MISSING_RENDERERS=1." >&2
  exit 1
}

# --- per-extension renderers ------------------------------------------------
render_one() {  # $1=source path
  local src="$1" base ext out
  base="$(basename "$src")"
  ext="${base##*.}"
  out="$OUT_DIR/${base%.*}.svg"

  case "$ext" in
    puml)
      have "$PLANTUML_CMD" || { missing_tool plantuml; return; }
      echo "  plantuml  $src → $out"
      "$PLANTUML_CMD" -tsvg -nometadata -o "$REPO_ROOT/$OUT_DIR" "$src"
      # Strip the volatile `<?plantuml $version$?>` processing instruction so the
      # committed SVG is byte-identical across PlantUML versions / machines
      # (keeps the CI freshness diff stable).
      sed -i 's#<?plantuml[^>]*>##g' "$out"
      ;;
    mmd)
      have "$MMDC_CMD" || { missing_tool mmdc; return; }
      echo "  mermaid   $src → $out"
      local pflag=()
      [[ -f "$MMDC_PUPPETEER" ]] && pflag=(-p "$MMDC_PUPPETEER")
      "$MMDC_CMD" -i "$src" -o "$out" -b transparent -t neutral "${pflag[@]}"
      ;;
    d2)
      have "$D2_CMD" || { missing_tool d2; return; }
      echo "  d2        $src → $out"
      D2_LAYOUT="${D2_LAYOUT:-dagre}" "$D2_CMD" --pad 16 "$src" "$out"
      ;;
    dot|gv)
      have "$DOT_CMD" || { missing_tool dot; return; }
      echo "  graphviz  $src → $out"
      "$DOT_CMD" -Tsvg "$src" -o "$out"
      ;;
    bytefield)
      [[ -n "$BYTEFIELD_CMD" ]] || { missing_tool bytefield-svg; return; }
      echo "  bytefield $src → $out"
      # shellcheck disable=SC2086
      $BYTEFIELD_CMD -s "$src" -o "$out"
      ;;
    *)
      echo "  ? no renderer for .$ext ($src) — skipping" >&2
      return
      ;;
  esac
  rendered=$((rendered + 1))
}

render_plot() {  # $1=gnuplot script (self-contained: sets terminal+output)
  local gp="$1"
  have "$GNUPLOT_CMD" || { missing_tool gnuplot; return; }
  echo "  gnuplot   $gp"
  ( cd "$(dirname "$gp")" && "$GNUPLOT_CMD" "$(basename "$gp")" )
  rendered=$((rendered + 1))
}

# --- main -------------------------------------------------------------------
echo "render-diagrams: repo=$REPO_ROOT"

if [[ $# -gt 0 ]]; then
  for src in "$@"; do
    case "$src" in
      *.gp) render_plot "$src" ;;
      *)    render_one "$src" ;;
    esac
  done
else
  # All diagram sources.
  if [[ -d "$SRC_DIR" ]]; then
    shopt -s nullglob
    for src in "$SRC_DIR"/*.{puml,mmd,d2,dot,gv,bytefield}; do
      render_one "$src"
    done
    shopt -u nullglob
  fi
  # All benchmark plot scripts.
  if [[ -d "$BENCH_DIR" ]]; then
    shopt -s nullglob
    for gp in "$BENCH_DIR"/*.gp; do
      render_plot "$gp"
    done
    shopt -u nullglob
  fi
fi

echo "render-diagrams: done (rendered=$rendered, skipped=$skipped)"
