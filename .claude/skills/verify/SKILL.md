---
name: verify
description: Verify a change to libdictenstein by exercising it and observing real behavior. Routes by what changed — diagrams-as-code (render + hygiene + freshness gate), Rust source (cargo build/test under the right feature set + clippy + fmt), or formal artifacts (the Rocq/TLA+ correspondence harness). Use before committing a nontrivial change.
---

# Verify a libdictenstein change

Observe **real behavior**, don't infer it from the diff. First see what changed, then run
the tightest loop for that surface. Mirror the CI jobs in `.github/workflows/ci.yml` so a
green local run predicts a green CI run.

## Step 0 — what changed

```bash
git status --porcelain
git diff --stat            # (or: git show --stat HEAD for the last commit)
```

Route by the paths that changed (a change may hit more than one surface — run each loop):

| Changed paths | Surface | Loop |
|---|---|---|
| `docs/diagrams/src/*.{puml,mmd,d2,dot,gv,bob}`, `docs/benchmarks/artifacts/*.gp` | diagrams-as-code | §A |
| `src/**`, `benches/**`, `tests/**`, `Cargo.toml` | Rust code | §B |
| `docs/formal/**`, `*.v`, `*.tla`, `UNSAFE_*.tsv` | formal / unsafe boundary | §C |
| any `*.md` (including `docs/**` prose) | docs | §D |

---

## §A — Diagrams-as-code (the render + gate loop)

A committed `docs/diagrams/<name>.svg` MUST be regenerable from its
`docs/diagrams/src/<name>.<ext>` source; CI re-renders everything and fails on a stale SVG.
So the verification IS the render + gate loop — there is no other runtime.

```bash
# 1. Render the touched sources (or all of them):
scripts/render-diagrams.sh docs/diagrams/src/<name>.<ext> …     # or: scripts/render-diagrams.sh

# 2. Hygiene gate (root viewBox · no PlantUML deprecation banner · width warnings):
scripts/render-diagrams.sh --check          # expect "hygiene: OK"; no NEW '⚠ width' line for your figure

# 3. Freshness gate (what CI runs): a full re-render must leave only your intended SVGs changed:
scripts/render-diagrams.sh
git diff --name-only -- docs/diagrams        # every changed .svg must have a matching changed src/*.<ext>

# 4. Idempotence / byte-stability — a 2nd render must change nothing:
s1=$(find docs/diagrams -name '*.svg' | sort | xargs sha1sum | sha1sum)
scripts/render-diagrams.sh
s2=$(find docs/diagrams -name '*.svg' | sort | xargs sha1sum | sha1sum)
[ "$s1" = "$s2" ] && echo "byte-stable" || echo "NON-idempotent — investigate"

# 5. Width budget (R3, ≤ ~700 px) for each figure you touched:
grep -oE 'viewBox="0 0 [0-9.]+ ' docs/diagrams/<name>.svg   # 3rd number is the width
```

**Observe the figure, not just the exit code.** For a byte/struct/bit **layout** (a Creole
byte-table in a `note`), confirm the cells render **contiguously** — successive field-cell
rects satisfy `y_{n+1} == y_n + height_n` (no gaps) — and each cell is fully color-filled:

```bash
grep -oE '<rect[^>]*fill="#[0-9A-F]{6}"[^>]*/?>' docs/diagrams/<name>.svg \
  | grep -oE '(height|y)="[0-9.]+"' | paste - -      # y should advance by exactly one height per row
```

Gotcha when authoring a byte-table cell: a literal `|` is the Creole column separator —
rephrase with `·`/`/` or the row splits (see `docs/diagrams/README.md`).

---

## §B — Rust code (build + test under the right feature set)

The crate is feature-gated; CI builds/tests a matrix. Verify the feature(s) your change
touches (not just `default`). Tee output to a file so an intermittent failure stays
inspectable.

```bash
# Pick the feature set for what you changed (mirror ci.yml's matrix):
FEATURES="--all-features"                                   # broad default
# FEATURES="--no-default-features --features persistent-artrie"   # disk-backed ARTrie work
# FEATURES="--no-default-features --features pathmap-backend"
# FEATURES="--no-default-features"                          # in-memory only

cargo build  $FEATURES --verbose            2>&1 | tee /tmp/verify-build.log
cargo test   $FEATURES --no-fail-fast       2>&1 | tee /tmp/verify-test.log
cargo clippy --all-features --all-targets -- -D warnings   2>&1 | tee /tmp/verify-clippy.log
cargo fmt --all -- --check
```

For a targeted change, run the **narrowest test that exercises it** first
(`cargo test $FEATURES <module_or_test_name>`), then the fuller suite. A doc example counts
as behavior: `cargo test --doc $FEATURES`. Read the tail of the tee'd log and confirm the
relevant tests actually ran and passed — don't declare success from a build alone.

---

## §C — Formal / unsafe boundary

Only when you touched proofs, TLA+ models, or the `unsafe` inventory. These are heavy;
prefer the specific harness for what changed and cap resources per the repo's
`RESOURCE_LIMITING.md`.

- **Rocq proofs:** build under `systemd-run --user --scope -p MemoryMax=32G -p CPUQuota=1800% make -j1`.
- **Correspondence / Miri / io_uring harnesses:** run the same script the CI `Formal *`
  jobs invoke (see `.github/workflows/ci.yml` steps "Run … correspondence harness").
- **Unsafe change:** reconcile `UNSAFE_INVENTORY.tsv` + `UNSAFE_CONTRACTS.tsv`
  (set-equality, no orphan tags) or the formal-correspondence gate fails.

---

## §D — Docs (the GitHub-math gate)

GitHub runs CommonMark backslash-escape processing *inside* math spans before MathJax
parses them, so `$…$` and `$$…$$` corrupt `\_` `\{` `\}` `\;` `\,` `\#` — loudly (a
`'_' allowed only in math mode` parse error) or, worse, silently: `\max\{\,L\,\}`
renders as `\max{,L,}`. Only ``$`…`$`` and ` ```math ` fences survive verbatim.

```bash
python3 scripts/check-doc-math.py --selftest   # tokenizer termination + all 4 rules fire
python3 scripts/check-doc-math.py              # expect "doc-math: OK"; this is what CI runs
```

The gate also rejects an ASCII letter abutting an opening ``$` `` (GitHub renders no math
there) and two-or-more literal `\$` on one line (they can pair into a spurious math span).
See `docs/README.md` § *Authoring conventions — math in Markdown*.

**Observe the render, not just the exit code.** For a doc whose math you changed, confirm
GitHub itself agrees — the escapes must survive into the rendered span:

```bash
jq -Rs '{text: ., mode: "gfm"}' docs/persistence/durability-and-recovery.md \
  | gh api -X POST /markdown --input - \
  | grep -o '<math-renderer[^>]*>[^<]*' | head
```

Every `\_` `\{` `\;` you authored must still be present in that output, and no
`\text{…}` may contain a bare `_`.

---

## Report

State what you expected, what you ran, and what you observed (command + the decisive lines
of output). If a gate failed, say so with the output; never claim success you didn't observe.
For diagrams, a green run is: render exit 0 · `--check` OK · freshness diff = only your
intended SVGs · byte-stable · every touched figure at most 700 px and visibly contiguous.
