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

## Phase 1 — Fix docs/algorithms/ rot  ☑

| Item | Action | Status | Verification |
|------|--------|--------|--------------|
| Stale `*_char::` module paths (32) | `dynamic_dawg_char::`→`dynamic_dawg::` etc. across README + 3 impl docs (verified vs `prelude`) | ☑ | grep → 0 |
| Dead `../0X-*` liblevenshtein-tree links (35) | levenshtein-automata + contextual-completion → companion-crate GitHub URL; value-storage/serialization → `serialization.md`; zipper-navigation → `zippers.md`; performance → `theory/disk-tries/07`; home nav → `docs/README.md` | ☑ | grep `../0X` → 0 |
| Stale `src/scdawg(_char).rs` refs | → `src/scdawg/{ascii,char}.rs` | ☑ | grep → 0 |
| Pre-existing broken `pathmap-dictionary-char.md` link | → `src/pathmap/char.rs` (no dedicated doc exists) | ☑ | link-check clean |
| Mangled benchmark table | de-dup `DynamicDawg` rows → relabel `DynamicDawgChar`; column alignment; **provenance caveat** pointing to reproducible ledgers | ☑ | visual + table consistent |
| `docs/README.md` (new) | Foundational documentation index (taxonomy table + reading order); resolves the home-nav links. (Phase 7 adds the reading-order diagram post-reorg.) | ☑ | written |
| nav headers (algorithms/README) | liblevenshtein "Back/Next Layer" cruft → libdictenstein index/theory/architecture nav | ☑ | — |

**Result**: only remaining unresolved links in `docs/algorithms/` are forward
references to `serialization.md` (15) and `zippers.md` (5), created in Phase 5.
These md files are not `include_str!` doctests, so grep verification is the gate.

## Phase 2 — Front door + diagrams  ☑

| Item | Action | Status | Verification |
|------|--------|--------|--------------|
| README counts | `67→69` Rocq `.v`, `52→55` TLA⁺ (headline + formal table + module count); props `1,283→1,301`; verified 0 real Admitted/Axiom/Parameter | ☑ | `find … -name '*.v'`=69; `*.tla`=55; props grep=1301 |
| README math | bare `Rᵤ` → backtick-wrapped `` `Rᵤ` `` | ☑ | — |
| README doc-map | "Diagram sources (PlantUML)" → "…(PlantUML · D2 · Graphviz · bytefield · gnuplot)" → `docs/diagrams/README.md` | ☑ | — |
| Diagram #17 durable-write **sequence** (PlantUML) | authored from `durable_write.rs` Order-A header; embedded in README "Order-A protocol" section | ☑ | render clean 1220$\times$933; embed resolves |
| `src/lib.rs` Architecture | new `# Architecture` section + trait diagram embed (raw-GitHub URL, master); intra-doc links use plain code for feature-gated `ARTrie`/`KeyEncoding` | ☑ | `cargo doc --all-features -D warnings` → exit 0 |
| Diagram #1 trait class diagram (PlantUML) | `traits.svg` — read/mutation/persistent families + assoc-type edges | ☑ | render clean 1841$\times$665 |
| Diagram #4 factory dispatch (PlantUML) | `factory-dispatch.svg`; embedded in user-guide In-Memory section | ☑ | render clean; embed resolves |
| Persistent file-lifecycle (PlantUML state) | `persistent-lifecycle.svg`; embedded in user-guide Persistent section | ☑ | render clean 839$\times$882; embed resolves |

**Note**: git remote IS present (`github.com/vinary-tree/libdictenstein`, also in
Cargo.toml) — the memory "local-only (no git remote)" is stale; raw-GitHub `master`
URLs are the correct rustdoc embed mechanism for docs.rs.
## Phase 3 — Theory + algorithms refresh + diagrams  ☑

Executed via 3 parallel cluster subagents (disk-tries, scdawg, algorithms/impl),
each owning its docs end-to-end; parent provided exact source-derived layouts,
rendered + verified all diagrams.

| Item | Action | Status | Verification |
|------|--------|--------|--------------|
| Svgbob wired into pipeline | `.bob` branch in `render-diagrams.sh` (+ headless-`DISPLAY` fix for JVM renderers), CI `cargo install svgbob_cli@0.7.6`, README row | ☑ | all `.bob` render 648$\times$7xx |
| 10 new diagrams | node-header (bytefield, parent) + node-layouts/path-compression/swizzled-ptr/burst-trie (Svgbob) + node-state/swizzled-ptr-states/clone-on-split/dawg-minimization (PlantUML) + scdawg-structure/suffix-links (Graphviz) | ☑ | 19/19 idempotent; 0 render-error markers |
| Inline DOIs | added across the body of **15** theory files from the README's Crossref-verified list (was ~0 inline) | ☑ | `grep doi.org` = 15 files |
| Backtick prose math | `O(·)`, `∣q∣`, `Σ`, `≤ 2·∣T∣−1` etc. wrapped in sentences across disk-tries + scdawg + algorithms (fences left as-is) | ☑ | sampled; balanced fences |
| Thin docs filled | `implementations/scdawg.md` 240→314 (real `SubstringDictionary` API verified), `bijective.md`→285 (corrected data-model), `disk-tries/07-benchmark-results.md` 125→238 (metric defs + intuition + provenance) | ☑ | API symbols verified vs `src/scdawg/`, `src/bijective/` |
| Diagram embeds | byte-layout/path-compression/burst-trie/node-state into disk-tries 02-04; clone-on-split/suffix-links/scdawg-structure into scdawg 02/04; dawg-min + dawg-suffix-sharing into dynamic-dawg.md | ☑ | 0 broken embeds; 0 broken links (excl. Phase-5 forward-refs) |
| Corrections found | inline-prefix cap corrected to crate-real 12 B (byte)/6 `u32` (char); Crochemore 1986 added where load-bearing; DAWG year 1983→1985 | ☑ | vs `nodes/mod.rs` MAX_PREFIX_LEN |
## Phase 4 — Persistence, eviction, formal-verification + diagrams  ☑

Executed via 4 parallel subagents (WAL-format, persistence-arch, eviction, formal)
over disjoint files; parent authored the WAL byte layouts + rendered/verified all.

| Item | Action | Status | Verification |
|------|--------|--------|--------------|
| WAL byte layouts | `wal-header.bob` (64 B header) + `wal-record.bob` (17 B frame + 15 type codes), Svgbob (bytefield can't span rows) — exact offsets from `wal/{header,codec}.rs` | ☑ | render 656$\times$320 / 672$\times$400 |
| `docs/persistence/wal-format.md` (NEW) | full on-disk codec doc: header/record figures, 15-type table, dual-magic+version tripwire, RankRegime drop-rule, Order-A, recovery, CAS-walk; Mohan DOI | ☑ | 376 lines; embeds resolve |
| WAL/write/recovery diagrams | `wal-segment-lifecycle`, `rank-regime-replay`, `recovery-flow`, `cas-walk` (PlantUML) | ☑ | render clean |
| Persistence architecture | `mmap-architecture.md` + `architecture/persistence/README.md` prose; `persistence-stack.d2`, `mmap-vs-iouring.d2`, `checkpoint-flip.puml`, `layering-invariant.dot`; Driscoll+Mohan DOIs | ☑ | embeds resolve; io_uring tables untouched |
| Eviction | `eviction/README.md` glossary + what/how/why; `buffer-page-lifecycle`, `epoch-reclamation` (sequence), `eviction-pipeline` (PlantUML); corrected stale `core/eviction/` paths + Pressure-vs-Urgency conflation | ☑ | renders clean |
| Formal-verification | reconciled counts to 69 `.v` / 1,301 props / 55 TLA⁺ / 65 `.cfg` / 43+31 unsafe (0 Admitted/Axiom/Parameter) across 4 files; restructured VERIFICATION_RESULTS change-log into bulleted history (snapshot-vs-live totals labeled); F1–F5 diagrams | ☑ | no contradicting stale counts; snapshot clearly framed |
| D2 layout fix | added per-file `# d2-layout: elk` directive support (concentric trust-zones need elk, not dagre) | ☑ | unsafe-trust-zones renders 2689$\times$1357 |

**Result**: 37 diagrams total, all idempotent, 0 render-error markers, 0 broken embeds/links.
## Phase 5 — New conceptual docs  ☑

Executed via 3 parallel subagents over disjoint new docs; each verified APIs
against source and corrected the parent's stale path pointers.

| New doc | Content | Diagrams | Lines |
|---------|---------|----------|------:|
| `algorithms/zippers.md` | lazy set-algebra (7 combinators) + lattice/semilattice value-merge; **resolves the Phase-1 forward-ref** | zipper-composition (D2/elk), zipper-lattice (PlantUML Hasse), zipper-cursor (state) | 549 |
| `algorithms/serialization.md` | bincode/JSON/plaintext/protobuf/compression; terms-only vs value-preserving `*_with_values`; bincode-1→2 byte-compat; **resolves the Phase-1 forward-ref** | — | 345 |
| `architecture/abstractions.md` | `CharUnit{u8,char,u64}` + `KeyEncoding{ByteKey,CharKey,U64Key}`, one-code-path-three-alphabets | units-keys (D2/elk) | 250 |
| `algorithms/persistent-suffix-graphs.md` | snapshot + op-segment WAL + CAS-rebuild-publish for the 3 persistent substring families; Inenaga DOI | suffix-graph-publish (sequence) | 364 |
| `algorithms/native-u64-and-cx.md` | native-u64 profile + CX prefix-3/4 compact snapshot (`AR64CX01`) | — | 299 |
| `algorithms/vocab-trie.md` | durable `term↔u64` bijection; forward overlay (durable) vs reverse map (rebuilt-on-recovery) | vocab-recovery (PlantUML) | 274 |

Subagent corrections (verified vs source): `semiring_lattice.rs` does not exist
(Lattice trait is in the sibling `llattice` crate — documented accurately, not
invented); vocab lives in `src/persistent_artrie/vocab/` (not `persistent_vocab_artrie/`).

**Also fixed**: 8 pre-existing cross-project dead links in
`integration/pathmap/README.md` (MORK/MeTTaIL) → plain-text project references.

**Milestone**: ✅ **ZERO broken relative links across the entire `docs/` +
`formal-verification/` tree**. 43 diagrams idempotent.
## Phase 6 — Benchmark plots  ☑

Subagent extracted ONLY recorded numbers (no re-runs, no fabrication); every `.dat`
header cites its source ledger + table.

| Chart | Source ledger | Type |
|-------|---------------|------|
| persistence-construction-throughput | persistence-enhancements Exp 0 | line, throughput-vs-size $\times$3 backends |
| pernode-recovery-speedup | persistence-enhancements Exp 5 | log-y clustered bars |
| iouring-vs-mmap-latency | io_uring_migration Phase 3 | clustered bars (p50/p99) |
| iouring-batch-read | io_uring_migration Phase 3 | bars |
| loading-strategy-comparison | loading-optimization summary | log-y bars (accept/reject) |
| loading-open-time-before-after | loading-optimization Exp 1 | log-y clustered bars |
| lockfree-flip-throughput | lockfree-flip-benchmark | clustered bars (%-gain) |
| disktrie-durable-throughput | disk-tries/07 snapshot | line $\times$2 series |
| u64-native-vs-byte-latency | persistent-u64 …2026-06-13 | clustered bars |

- gnuplot pipeline: added `<desc>Produced by GNUPLOT …</desc>` version-stripping for
  byte-stable output (versionleak=0 across all 9).
- **Script robustness fix**: hardened the d2 `d2-layout:` grep with `|| true` — a d2
  source without the directive returned exit 1 and aborted the whole render under
  `set -o pipefail`. Full render now completes end-to-end (exit 0, 52 artifacts).

**Result**: 52 committed artifacts (43 diagrams + 9 plots), ALL idempotent; embedded
into 5 ledgers; 0 broken chart embeds. `docs/benchmarks/artifacts/` (was empty) now
holds the `.dat` + `.gp` sources + rendered SVGs.
## Phase 7 — Reorganization + top-level index  ☑

| Item | Action | Status | Verification |
|------|--------|--------|--------------|
| `docs/design/` reorg | 13 durable design refs kept at top; **61 historical logs `git mv`'d** into `docs/design/history/` (11 campaign subdirs: slice3, durable-commit-seq, s5-flip, f7-eviction, redteam, phase-f-g5, bug-fixes, byte-flip, cx-codec, counter-u64, vocab + 3 root singletons) | ☑ | all renames (history preserved); 13/61 counts exact |
| `docs/design/README.md` + `history/README.md` | durable-ref index table + preserved-campaign-ledger index | ☑ | created |
| Inbound links | verified: ZERO markdown-link references to moved files existed (only inline-code mentions) — no fixes needed | ☑ | whole-tree link check = 0 |
| `docs/README.md` enrich | 7 new conceptual docs wired into the map; reading-order diagram embedded | ☑ | links resolve |
| `docs/algorithms/README.md` | "Related Documentation" links the 5 new sibling docs + abstractions + wal-format | ☑ | no `*_char::`/`../0X` reintroduced |
| `docs-reading-order.dot` (Graphviz) | colored documentation reading-path graph (4 tracks) | ☑ | renders 3679$\times$676 |

**Result**: 53 committed artifacts (44 diagrams + 9 plots), all idempotent; ZERO
broken links across `docs/` + `formal-verification/` + `README.md` + `CHANGELOG.md`.

---

## Final tally

- **53 rendered, committed, idempotent diagram artifacts** (44 diagrams-as-code +
  9 benchmark plots) — was 4 hand-PlantUML'd; **22 ASCII-art files** upgraded.
- **Tooling**: `scripts/render-diagrams.sh` (PlantUML/Mermaid/D2/Graphviz/bytefield/
  Svgbob/gnuplot, version-normalized for byte-stable output) + CI `diagrams`
  freshness gate (pinned tool versions).
- **8 new conceptual docs** (zippers, serialization, abstractions, wal-format,
  persistent-suffix-graphs, native-u64-and-cx, vocab-trie, docs/README index).
- **Guideline conformance** across theory/algorithms/persistence/eviction/formal:
  inline DOIs (Crossref-verified), backtick math, terms-defined-before-use,
  literate pseudocode, thin docs filled.
- **Correctness fixes found**: 32 stale `*_char::` paths + 35 dead links; formal
  counts reconciled (69 `.v` / 1,301 props / 55 TLA⁺ / 65 `.cfg` / 43+31 unsafe);
  inline-prefix cap (12 B / 6 `u32`); mangled benchmark table.
- **ZERO broken links** tree-wide; `cargo doc --all-features -D warnings` clean.
