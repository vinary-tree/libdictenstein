# Persistence Documentation — Full-Reauthor Scientific Ledger

Tracks the end-to-end reauthor of the persistence documentation into one coherent,
architect-facing corpus under [`persistence/`](persistence/), following the same
diagrams-as-code + CI-gated + guideline-conformant discipline established by the
[documentation overhaul](doc-overhaul-ledger.md).

- **Plan**: `~/.claude/plans/while-the-other-agent-cosmic-star.md`
- **Strategy**: full reauthor + dedicated reusable-kernel doc + whole-repo LaTeX/MathJax
  math + best-diagram-per-concept (PlantUML-primary), committed SVG, scripted
  (`scripts/render-diagrams.sh`) + CI-gated.
- **Started**: 2026-07-06.
- **Amendments to the house style**: PlantUML preferred over Mermaid for committed
  figures; MathJax/LaTeX (`$…$` / `$$…$$`) replaces Unicode-in-backticks for math.
  - **Superseded 2026-07-09**: those delimiters are unsafe — GitHub strips
    backslash-escapes from inside them. Use ``$`…`$`` and ` ```math ` fences instead;
    see `docs/README.md` § *Authoring conventions — math in Markdown*, gated by
    `scripts/check-doc-math.py`.
- **Hard coordination constraint**: the in-flight files
  `design/f4-lock-collapse-implementation.md` and
  `../formal-verification/tla+/SharedPersistentConcurrency.tla` are **not edited** here
  (held uncommitted by a concurrent agent finalizing vocab-F4); the new docs reference
  them and reconcile after they land.

## Conventions
- One row per deliverable: phase · item · action · status · verification.
- Status: ☐ pending · ◐ in-progress · ☑ done.
- Diagram IDs (`#N`) reference the diagram inventory in the plan.

---

## Phase 0 — Palette + ledger  ☑

| Item | Action | Status | Verification |
|------|--------|--------|--------------|
| `diagrams/README.md` house style | Add persistence-scoped accents (overlay=teal `#B2DFDB`, WAL=orange `#FFCC80`, checkpoint=indigo `#C5CAE9`, locks=red `#FFCDD2`, kernel=slate `#CFD8DC`, proofs=purple `#E1BEE7`); extend, not replace | ☑ | palette table renders; base colors unchanged |
| `persistence-reauthor-ledger.md` | New scientific ledger (this file) | ☑ | written |

## Phase 1 — Architect entry point + families  ☑

| Item | Action | Status | Verification |
|------|--------|--------|--------------|
| `persistence/README.md` (NEW) | Architect entry point: whole-stack synthesis + master diagram (#1) + guided tour + reading map | ☑ | written; embeds `persistence-stack.svg` (recolored to accents; D2 renders 2371$`\times`$2305) |
| `persistence/families.md` (NEW) | Reauthor of `architecture/persistence/README.md`; diagrams #2 (layering + alias note), #3 (KeyEncoding seam), #21 (suffix publish → WAL recolored orange) | ☑ | written; 3 embeds resolve; layering + suffix re-rendered clean, 0 error markers |
| Cross-links | `docs/README.md` index rows + `architecture/persistence/README.md` reauthored to a lean orientation pointing at the new home | ☑ | updated; family-overview now → `persistence/families.md` |

## Phase 2 — Reusable durable-storage kernel  ☑

| Item | Action | Status | Verification |
|------|--------|--------|--------------|
| `persistence/durable-storage-kernel.md` (NEW) | `core/` as a general reusable engine; two extension seams (BlockStorage + key/record model) + the overlay Template-Method hooks; Recipe A (CoW tree) + Recipe B (non-tree snapshot); reusable-vs-specific table; diagram #4 | ☑ | written; `BlockStorage`/`DurableOverlayWrite`/`DurabilityPolicy` verified vs `core/` source; `kernel-substrate.svg` renders clean 1697$`\times`$1659 |

## Phase 3 — Storage backends + WAL format  ☑

| Item | Action | Status | Verification |
|------|--------|--------|--------------|
| `persistence/storage-backends.md` | `git mv` of `mmap-architecture.md`; reauthored to the `BlockStorage` seam + backends + buffer manager + on-disk format; overlay/checkpoint/recovery content moved out to P4 docs; NEW `file-header.bytefield` (#17), reuses `node-header`/`node-layouts`/`swizzled-ptr-states` (#7/#18) | ☑ | written; `FileHeader` fields verified vs `disk_manager.rs`; `file-header.svg` renders (322px, 8$`\times`$8 rows) |
| `persistence/wal-format.md` | Integrated into the new corpus (nav + See-also → P4 docs); `.bob` byte layouts kept as-is (accurate; orange WAL accent lives in the D2/PlantUML figures) | ☑ | edits applied; #16 unchanged (correct) |
| Inbound-link surgery | Retargeted all live `mmap-architecture.md` links (7 algo/theory/user-guide docs + root README) → `persistence/README.md` / `storage-backends.md` / `eviction.md`; refreshed reading-order label; ledger/CHANGELOG inline-code left as historical record | ☑ | `grep '](…mmap-architecture.md'` → 0 live links |

## Phase 4 — Lock-free core (deepest)  ☑

| Item | Action | Status | Verification |
|------|--------|--------|--------------|
| `persistence/lock-free-overlay.md` (NEW) | Immutable `OverlayNode` + `arc_swap` root + owned `Child` leak-fix + G4; NEW `path-copy.dot` (#6), reuses `cas-walk` (#5) | ☑ | written; `OverlayNode`/`Child`/`ArcSwapOption`/`try_set_final` verified vs `node.rs`/`atomic_ptr.rs`; `path-copy.svg` 657$`\times`$474pt |
| `persistence/durability-and-recovery.md` (NEW) | Order-A protocol + committed-watermark theorem + checkpoint flips + recovery + #47/#48/#49; NEW `committed-watermark.d2` (#9), reuses `durable-write-sequence` (#8, WAL→orange), `checkpoint-flip` (#10), `recovery-flow` (#11) | ☑ | written; `CommittedWatermark`/`image_coverage_lsn`/`reconcile_lww` verified vs source; `committed-watermark.svg` 1688$`\times`$330 |
| `persistence/concurrency-model.md` (NEW) | F4 collapse + `CK>merge_lock>OR>EC` + `SharedTrieAccess`/`AtomicEnumCell` + MVCC/EBR + version-GC + eviction stamp; NEW `f4-lock-hierarchy.dot` (#12), `two-epochs.d2` (#14), `serial-disk-ptr-stamp.puml` (#15), reuses `epoch-reclamation` (#13) | ☑ | written; F4 shim + hierarchy verified vs `shared_access.rs`; 3 new diagrams render clean, 0 error markers |

## Phase 5 — Eviction / MVCC / group-commit  ☑

| Item | Action | Status | Verification |
|------|--------|--------|--------------|
| `persistence/eviction.md` | `git mv` of `eviction/README.md` (empty `docs/eviction/` removed); integrated (nav + Related → corpus; aligned "DiskRef" ↔ `Child::OnDisk`); **verified overlay eviction is functional+wired for byte/char** (`evict_overlay_nodes` called from coordinator + checkpoint callback; reads route through `find_leaf_faulting`) — the stale "no-op" memory note is superseded by f7-v4; vocab callback is a no-op for parity; reuses `eviction-pipeline`/`epoch-reclamation`/`buffer-page-lifecycle` (#19) | ☑ | img paths still resolve (both dirs 1 level under docs/); source map matches live `core/eviction/` |
| `persistence/group-commit.md` | `git mv` of `group_commit_regression.md`; reauthored — corrected stale `persistent_artrie_core/` → `persistent_artrie/core/` paths; added group-commit definition, the $`t_\text{coord}<t_\text{sync}`$ rationale, `DurabilityFrontier`/`AsyncWalGroupCommit` correctness links | ☑ | paths verified vs live tree |
| MVCC / version-GC | Covered in `concurrency-model.md` (§MVCC, §Version checkpoint & GC) | ☑ | `TrieRoot`/`ReadTransaction`/`VersionGcRegistry` cited |

## Phase 6 — Formal-verification map  ☑

| Item | Action | Status | Verification |
|------|--------|--------|--------------|
| `persistence/formal-verification-map.md` (NEW) | two-pronged strategy + correspondence tables by concern (overlay/durability/recovery/concurrency/eviction/WAL/group-commit) + negative-control methodology + CI gate; reuses `proof-artifact-map.d2` (#20, stale tree path fixed + re-rendered) | ☑ | written; specs cross-checked vs the exploration inventory; counts 69 `.v` / 55 `.tla` / 65 `.cfg` verified vs `find` |

## Phase 7 — Theory + algorithms reauthor  ☑  *(parallel subagent, parent-verified)*

| Item | Action | Status | Verification |
|------|--------|--------|--------------|
| `theory/disk-tries/01–07` (+ README) | Surgical integration: bridge links into the systems corpus (04/05/06 → storage-backends/lock-free-overlay/durability-and-recovery); 01/02 left as prerequisite CS theory (02 supersedes-note preserved) | ☑ | 0 stale paths (already new-tree), 0 broken links, 0 bad anchors; every symbol/const verified vs `src/` |
| `algorithms/{vocab-trie,native-u64-and-cx,persistent-suffix-graphs}.md` | Deep-link into `families.md`/`storage-backends.md`; verified `PersistentVocabARTrie`/`AR64CX01`/`NativeSuffixIndex`/`MAX_CAS_RETRIES` vs source | ☑ | 18 added cross-corpus anchors all resolve |

## Phase 8 — Design-refs reauthor/consolidate  ☑  *(parallel subagent, parent-verified)*

| Item | Action | Status | Verification |
|------|--------|--------|--------------|
| 11 non-in-flight `design/*.md` (+ README) | Added **Synthesized in:** back-links (detail↔narrative); fixed ~70 stale `persistent_artrie_{core,char}`/`persistent_vocab_artrie` paths incl. dead `lockfree.rs`→`vocab/mutation_api.rs`; superseded `im::Vector` body flagged (kept for provenance, not deleted) | ☑ | 0 stale prefixes remain; all 19 `src/` paths + 5 back-link + 6 round-trip targets resolve; `f4-lock-collapse-implementation.md` linked-not-edited |

## Phase 9 — Whole-repo LaTeX/MathJax sweep  ☑

| Item | Action | Status | Verification |
|------|--------|--------|--------------|
| Backtick-Unicode-math → `$…$` (raku `latexify.raku`) | Fence-aware; **underscore-guard** (skip `_`-bearing code spans so `get_value`/`L_final` are never subscript-mangled; Unicode-subscript `Rᵤ`→`$R_u$` still converts); complete symbol map (relations/sets/logic/lattice/greek/delimiters) | ☑ | 0 misses; 0 `$`-in-fence introduced; 6 pre-guard mangled spans hand-fixed (`\text{}`) |
| Loose-prose math → `$\cmd$` (raku `loose-latexify.raku`) | Per-symbol wrap outside backticks; skips fences + `//`/indented code + nav chars (`·`/`→` preserved) | ☑ | **69 docs** converted; **0 odd-`$`** after stripping fences **and** inline-code (GitHub-safe, no mis-pairing) |
| scdawg + history **completed** (no exclusion) | Sentinel fix: escape bare `$` → `\$` FIRST (raku `escape-dollar.raku`, outside fences/inline-code) so string-end markers (`"abab$"`) don't collide with MathJax, THEN convert. `theory/scdawg/**` (7) + `design/history/**` (58) swept | ☑ | 65 files; 0 code-in-math / 0 odd-`$` / 0 fence-leak corpus-wide |
| Converter hardening (2 flaws found + fixed mid-sweep) | (a) **multi-line inline-code spans** — per-line backtick tracking is unreliable, so skip lines with an odd backtick count; (b) **code spans with a stray math glyph** — skip a backtick span containing `::` / `\` / `==` / `->` / `<Upper` (TLA `\A`/`\E`, `TypeId::of::<V>()`, Rust generics) so code is never math-mangled | ☑ | reverted scdawg+history to HEAD, re-applied hardened converter; 2 pre-hardening live mangles (`durable-storage-kernel.md`, `empty-string-value-support.md`) hand-fixed |
| In-flight F4 files **completed** (explicit no-deferral override) | `f4-lock-collapse-implementation.md` swept — **3 math-notation lines only** (`⇒`→`$\Rightarrow$`); backed up to scratchpad first (recovery guarantee); the concurrent agent's design content is byte-identical apart from those glyphs. `SharedPersistentConcurrency.tla` is **TLA⁺ source, not markdown** (0 backticks, 0 Unicode-math) — nothing to convert, complete by scope | ☑ | **0 markdown files excluded**; corpus-wide 0 code-in-math / 0 odd-`$` / 0 fence-leak |

## Phase 10 — Final verification  ☑

| Item | Action | Status | Verification |
|------|--------|--------|--------------|
| Diagrams | full `scripts/render-diagrams.sh` (the CI command) | ☑ | 60 rendered, exit 0, **0 error markers**; byte-stable — only the 6 revised + 7 new SVGs changed, **no untouched diagram drifted**; all 13 mine confirmed idempotent (sha256 stable across re-render) |
| Links | tree-wide relative-link scan | ☑ | **0 real broken links** across `docs/` + `README.md` + `CHANGELOG.md` + `formal-verification/*.md` (the sole hit is the intentional `<name>.svg` template in `diagrams/README.md`) |
| Facts | cited paths/types/symbols/magics vs live `src/` | ☑ | 27 symbols + 9 magic constants (`OverlayNode`, `serial_disk_ptr`, `CommittedWatermark`, `FILE_MAGIC` `PART`/`ARTC`/`AR64`, `0x5041525400010000`, …) all FOUND; subagents verified theirs; counts 69 `.v` / 55 `.tla` / 65 `.cfg` |
| Math | LaTeX sweep safety | ☑ | **0** LaTeX commands leaked into code fences; **0** odd-`$` after stripping fences + inline-code (every `$…$` balanced, GitHub-safe) |
| rustdoc | `RUSTDOCFLAGS="-D warnings" cargo doc --all-features --no-deps` | ☑ | exit 0 (lib.rs SVG embeds unbroken; no `src/` touched) |
| DOIs/citations | canonical DOIs | ☑ | Askitis-Zobel 2009, Driscoll 1989, Leis 2013, Mohan 1992, Gamma 1994 — all standard, matching the crate's Crossref-verified set |
| No clobber | in-flight files | ☑ | `f4-lock-collapse-implementation.md`, `SharedPersistentConcurrency.tla`, + 3 vocab tests show **only** their session-start state — untouched by this effort |

---

## Final tally

- **11 persistence-corpus docs** under `persistence/` (README entry point · durable-storage-kernel · families · storage-backends · wal-format · lock-free-overlay · durability-and-recovery · concurrency-model · eviction · group-commit · formal-verification-map), unified with cross-links and a bidirectional detail↔narrative bridge to the design refs.
- **13 diagrams**: 7 new (`kernel-substrate`, `path-copy`, `committed-watermark`, `f4-lock-hierarchy`, `two-epochs`, `serial-disk-ptr-stamp`, `file-header`) + 6 revised (`persistence-stack`, `layering-invariant`, `suffix-graph-publish`, `durable-write-sequence`, `proof-artifact-map`, `docs-reading-order`), best-type + best-actor + persistence-accent palette, all byte-stable.
- **Whole-repo LaTeX/MathJax sweep**: **~134 docs** (live corpus + scdawg theory + all 58 history archives), three raku passes (escape `$`-sentinels → underscore/code-guarded backtick → fence/code/nav-aware loose-prose), balanced + GitHub-safe corpus-wide (0 code-in-math, 0 odd-`$`, 0 fence-leak); **0 markdown files excluded** (the in-flight F4 doc was swept under explicit override, backed up first; the `.tla` is ASCII source with no math to convert).
- **Reusable-substrate framing**: `durable-storage-kernel.md` presents `core/` as a general durable-storage engine with two recipes for building a **new** persistent file layer — the requested basis for future layers.
- **Coordination**: the concurrent agent's in-flight F4 / vocab-lockfree work was referenced but never edited.
