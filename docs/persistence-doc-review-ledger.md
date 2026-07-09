# Persistence Documentation Review — Ledger

An auditable record of the end-to-end correctness/precision/consistency/pedagogy review of
the persistence documentation corpus (persistence + disk-trie theory + persistence-adjacent
algorithms + `abstractions.md`) and its diagrams. Every issue is logged with a `file:line`
anchor, a code citation where it is a correctness fact, and a status (`OPEN` → `FIXED` /
`FLAGGED` / `VERIFIED-OK`).

## Governing conventions (this pass)

All updates align with the **pgmcp documentation guidelines** (23 guidelines / 7 categories:
placement, coverage, pedagogy, diagrams, math-notation, citations, algorithms), **with two
owner amendments that override where they conflict**:

1. **Prefer PlantUML over Mermaid** for any new/replaced diagram.
2. **Math notation uses LaTeX, not unicode literals** — this **overrides** the pgmcp
   "Mathematical notation" guideline ("use unicode for math"):
   - **Prose:** MathJax. **Delimiters (superseded 2026-07-09):** `$…$` / `$$…$$` are
     unsafe — GitHub strips backslash-escapes from inside them, corrupting `\_` `\{` `\;`.
     Use ``$`…`$`` inline and ` ```math ` fences for display; gated by
     `scripts/check-doc-math.py`. See `docs/README.md`.
   - **Diagram labels:** PlantUML LaTeX `<latex>…</latex>` / `<math>…</math>` (JLaTeXMath,
     bundled in the installed `plantuml.jar`). **Verified:** renders headless + **byte-stable**
     across renders (identical `data:image` sha1). **Caveat:** it embeds a *raster* image
     (no font-match participation; cross-JLaTeXMath-version byte-diff risk), so apply only
     where genuine mathematical notation warrants it — verify byte-stability + ≤700 px + font
     fit per figure; fall back to backticked/plain text if the hygiene gate breaks.
   - **Left as unicode (not math):** `·` separators, `①②`, box-drawing, `⋮` elisions, em/en
     dashes, `→/↑/↓` process-flow, `<img alt>` text, and math inside fenced code/pseudocode.

## Canonical ground-truth facts (verified against `src/` — apply consistently everywhere)

| # | Fact | Code evidence | Doc impact |
|---|------|---------------|-----------|
| G1 | **Lock hierarchy is `CK > merge_lock > EC`** (3 rungs). The `OR`/`owned_root: RwLock<TrieRoot>` rung is **deleted** (L3.3c); reads/writes are lock-free. | `dict_impl.rs:297-301` ("the 'OR' lock of the **former** … hierarchy is DELETED"; no `owned_root` field); `checkpoint_lock:410`, `merge_lock:424`, `eviction_coordinator:350` | Fix `CK > merge_lock > OR > EC` → `CK > merge_lock > EC` in `concurrency-model.md`, `README.md`, `eviction.md`. *(Stale code comments `dict_impl.rs:345-346,416-417` still say OR — FLAGGED, not edited.)* |
| G2 | **Two distinct block-id limits, both correct.** File/BlockStorage capacity = **24-bit** (`MAX_BLOCK_COUNT = 1<<24` = 16 M blocks = **4 TB**); swizzled-pointer *reference reach* = **23-bit** (`BLOCK_ID_BITS = 23`, `MAX_BLOCK_ID` = 8 M blocks = **2 TB**). | `disk_manager.rs:62-63`; `swizzled_ptr.rs` (`BLOCK_ID_BITS=23`, `MAX_BLOCK_ID=(1<<23)-1`) | `durable-storage-kernel.md:55` + `storage-backends.md` say "24-bit/4 TB" (correct as *capacity*) but must **distinguish** it from the 23-bit swizzled reach so it doesn't read as contradicting `swizzled-ptr.svg` (23-bit). **NOT a 24→23 correction.** |
| G3 | **In-memory `NodeHeader` = 16 B**: `node_type@0, prefix_len@1, flags@2, _pad@3, num_children:u16@4-5, _pad2@6-7, version:u64@8-15`. | `nodes/mod.rs:92-106` | `node-header.svg` is **correct** for this. *(The ASCII doc-comment `nodes/mod.rs:20-25` is itself stale — FLAGGED.)* |
| G4 | **On-disk `SerializedNodeHeader` = 16 B**, *different* layout: `magic "ART\0"@0-3, version:u8@4, node_type@5, flags@6, reserved/encoding_flags@7-8, num_children:u16@9-10, prefix_len@11, _pad@12, data_size:u32@13-16`. `NODE_MAGIC = b"ART\0"`. | `serialization.rs:14-29`, `NODE_MAGIC` | `storage-backends.md:87-89` mislabels the *in-memory* header (G3) as "serialized." Fix: describe the correct header for the on-disk-format section, and add a distinct on-disk figure (new PlantUML byte-table). |
| G5 | **CX** = codename for the **compact snapshot codec** (magic `AR64CX01`, `SNAPSHOT_VERSION=1`): a path-compressing overlay→dense on-disk image. **Never expanded as an acronym** anywhere. | `u64.rs SNAPSHOT_MAGIC = b"AR64CX01"`; `docs/design/history/cx-codec/` | Define **descriptively** on first use (do not invent an expansion) in `README.md`, `durability-and-recovery.md`, `storage-backends.md`, `families.md`, `native-u64-and-cx.md`, architecture README. |
| G6 | **Empty-string ("") keys/values are first-class** (empty-term root publishers). | `core/overlay/flip.rs:587+`; `docs/design/empty-string-value-support.md` | **Doc gap** — add coverage (families/lock-free-overlay or wal-format value-domain notes). |

## Worklist — findings by file (status tracked as executed)

### Correctness / staleness
- **[OPEN] G1 lock hierarchy** — `concurrency-model.md:47,52`, `README.md:107`, `eviction.md:663`.
- **[OPEN] G4 NodeHeader/SerializedNodeHeader** — `storage-backends.md:87-89` (+ new on-disk figure).
- **[OPEN] G2 block-id precision** — `durable-storage-kernel.md:55`, `storage-backends.md`.
- **[OPEN] eviction.md stale source anchor** — says `EvictableARTrie` at `src/artrie_trait.rs:513-584`; actual `artrie_trait.rs:624`.
- **[OPEN] `Frame` → `FrameMetadata`** naming — `eviction.md`, `storage-backends.md`, `README.md` (+ buffer-page diagrams) refer to a "Frame"; the per-page struct is `FrameMetadata` (`buffer_manager.rs:61`). Precision (fields are correct).
- **[OPEN] `FORMAT_VERSION` overload** — `storage-backends.md` "version = FORMAT_VERSION = 2" is the block-0 header `u32` (`disk_manager.rs:75`); distinct from `serialization.rs` `FORMAT_VERSION:u8=1`/`_V2:u8=2`. Disambiguate.
- **[OPEN] concurrency-model.md soft-stale status** — "vocab-F4 extension is landing in the working tree at time of writing" reads stale (vocab overlay-flip campaign is COMPLETE + gated).
- **[OPEN] G6 empty-string coverage gap.**
- **[TODO-VERIFY] formal-verification-map.md proposition tally** — file counts 69 `.v` / 55 `.tla` / 65 `.cfg` / 8 `_Unsafe.cfg` **verified exact**; the "1,301 (992 Theorem + 301 Lemma + 8 Corollary)" breakdown **not yet re-derived** — recount from `.v` before trusting.
- **[TODO-VERIFY] eviction.md embedded struct literals** — diff `EvictionCoordinator`/`LruRegistry`/`DiskLocationRegistry`/`EvictionStats`/`AccessTracker` snippets field-for-field vs source.

### Formality / conventions
- **[OPEN] MathJax stragglers** — `formal-verification-map.md:1,119` raw `↔` → `$\leftrightarrow$`; `wal-format.md:119,124,149,175,306` `∈ ≤ − ‖` inside inline-`code` → `$…$` for consistency with siblings.
- **[OPEN] Undefined acronyms** — **CX** (G5), `SWMR`, `ADT`, `SANY`, `ARIES`, `PART`/`PARTWAL`, and internal campaign tags (`RES-4`, `M-2a`, `A2`, `G4`, `L3.3`, `#47/#48/#49`, `DG0`) used as if defined — expand on first use / add glossary rows / gloss-or-drop tags.

### Citations
- **[OPEN] `families.md`** cites "Leis et al. 2013" with **no References section** — add one.
- **[OPEN] `wal-format.md`** lowercase `[doi:…]` (L62,295,382) → uppercase `[DOI:…]` for consistency.
- **[OPEN] Book/thesis refs lack DOI/URL** — Gamma 1994 (`durable-storage-kernel`), Gray & Reuter 1993 (`durability-and-recovery`), Herlihy & Shavit 2008 + Fraser 2004 (`concurrency-model`; Fraser has UCAM-CL-TR-579). Add ISBN/handle/URL where no DOI exists.
- **[OPEN] `eviction.md`** "SQLite-style" / CLOCK / EBR design claims uncited — add a References section.

*The four DOI-linked papers (Leis, Askitis-Zobel, Driscoll et al., Mohan/ARIES) are correct and resolve — VERIFIED-OK.*

## Fix log
*(Appended per file as edits land — dimension · file:line · before→after · evidence.)*

### Diagrams (task #11) — in progress
- **[DONE] `f4-lock-hierarchy.dot`** — removed the `OR`/`owned_root` rung (comment, title label, node, `ML→OR→EC` edges → `ML→EC`, vocab-subset text); now renders **3 rungs** (CK, merge_lock, EC), width 491 ≤700, OR absent from SVG. (`.dot`/Graphviz can't do PlantUML LaTeX — its few glyphs `‖`/`⇒` replaced with plain words "vs"/"so".)
- **[DONE] NEW `serialized-node-header.puml`** — on-disk 16-byte `SerializedNodeHeader` byte-table (magic/version:u8/node_type/flags/encoding_flags/num_children:u16/prefix_len/_pad/data_size:u32), contiguous (gap 16.2968==height), width 633 ≤700, 4 house colors. Replaces the mislabeled `node-header.svg` embed in storage-backends.md (§G4).
- **[POLICY] diagram math→LaTeX** — prose math is MathJax (done in the .md files). For diagram labels, PlantUML `<latex>`/`<math>` renders a *raster* `data:image` (verified byte-stable, but no font-match, cross-JLaTeXMath-version risk, non-crisp). In-scope diagrams carry mostly **lone standard glyphs** (`×` product/multiply, `∈`, `≤`, superscripts) that are already correct math typography in the crisp vector labels. Converting genuine multi-symbol *formulae* in labels to `<math>` where it renders cleanly + passes the hygiene gate; keeping lone glyphs as unicode where a raster embed would degrade the figure. Final per-figure decisions after the subagents' DIAGRAM-ISSUES reports; surfaced to owner.

### `concurrency-model.md` — DONE (prose); diagram queued
- **[FIXED] G1** — hierarchy `CK > merge_lock > OR > EC` → **`CK > merge_lock > EC`** (formula, the four bullets → three, alt-text "four red rungs"→"three", vocab subset "no merge_lock and no owned root"→"no merge_lock"); intro "dormant owned-path fallback"→"byte/char merges"; added a sentence that the former `owned_root` rung was removed once the overlay became the sole production structure. Evidence: `dict_impl.rs:297-301` (OR deleted), `checkpoint_lock:410`/`merge_lock:424`/`eviction_coordinator:350`.
- **[FIXED] soft-stale status** — "vocab-F4 … is landing in the working tree at time of writing" → "byte, char, *and* vocab collapse are committed and gated."
- **[FIXED] citation** — Fraser 2004 thesis now links UCAM-CL-TR-579; Herlihy-Shavit left as a book (no DOI).
- **[QUEUED → task #11] diagram** `f4-lock-hierarchy.puml` — remove the OR rung (render 3 rungs: CK, merge_lock, EC) to match the corrected prose.
- **[VERIFIED-OK]** SWMR is glossed inline ("single-writer / multi-reader-process"); terms table complete; MVCC/EBR defined.

### `README.md` (persistence) — DONE
- **[FIXED] G1** — `CK > merge\_lock > OR > EC` → `CK > merge\_lock > EC` (invariant line 107 + `persistence-stack-2.svg` alt-text "CK > merge > OR > EC" → "CK > merge > EC").
- **[FIXED] G5 CX** — added a **CX image** row to the terms-of-art table (compact snapshot, magic `AR64CX01`, codename not acronym), defined before its first use (§Checkpoint).

### `eviction.md` — DONE (prose); embedded-struct spot-check pending
- **[FIXED] stale anchor** — `EvictableARTrie` `src/artrie_trait.rs:513-584` → **`:624`** (verified `rg 'pub trait EvictableARTrie' → 624`).
- **[FIXED] Frame → FrameMetadata** — §Buffer-Pool "A buffer-pool `Frame`" → "a buffer-pool frame's per-frame state (`FrameMetadata`)"; Source-Files row likewise (buffer_manager.rs has `FrameMetadata` @61, no `Frame` type). *(Diagrams say "buffer-pool frame" as a concept — correct, no change.)*
- **[FIXED] citations** — added a References section (Fraser 2004 EBR / UCAM-CL-TR-579; SQLite dynamic-memory URL for the "SQLite-style" claim; CLOCK/LRU noted as classical, described mechanically).
- **[VERIFIED-OK] G1** — line 663 says only "`EC` is the eviction-coordinator leaf" (no stale OR chain).
- **[TODO-VERIFY]** embedded struct literals (`EvictionCoordinator`/`LruRegistry`/`DiskLocationRegistry`/`EvictionStats`) — diff vs source in the consistency pass.

### `storage-backends.md` — DONE (prose); new diagram queued
- **[FIXED] G2** — added swizzled-reach clarification after "block IDs are 24-bit … 4 TB": swizzled child pointers encode `block_id` in 23 bits → first $`2^{23}`$=8 M blocks=2 TB. (24-bit/4 TB kept — it is the correct *file capacity*.)
- **[FIXED] G4 + FORMAT_VERSION** — §"Data blocks" rewritten: "Each serialized node opens with a 16-byte `NodeHeader` (… `u64` version)" → the on-disk **`SerializedNodeHeader`** (magic `b"ART\0"`, `u8` version, node_type, flags, encoding_flags, `u16` num_children, prefix_len, `u32` data_size); notes the in-memory `u64` optimistic-lock version is NOT persisted, and the serialized `u8` `FORMAT_VERSION` ≠ the block-0 `u32` `FORMAT_VERSION`. Evidence: `serialization.rs:86-150` (`SerializedNodeHeader`, `NODE_MAGIC=b"ART\0"`, `SERIALIZED_HEADER_SIZE=16`, `FORMAT_VERSION:u8=1`/`_V2:u8=2`).
- **[FIXED] G5 CX** — added "(the compact snapshot format, magic `AR64CX01`)" gloss on first use.
- **[QUEUED → task #11] NEW diagram** `serialized-node-header.puml/.svg` — on-disk 16-byte `SerializedNodeHeader` byte-table (replaces the mislabeled `node-header.svg` embed here; `node-header.svg` stays the *in-memory* header in `03-adaptive-radix-tree.md`).
- **[VERIFIED-OK]** FileHeader byte table (magic/version/…/image_checkpoint_lsn @56-63), BLOCK_SIZE, the two backends, adaptive-edge ladder — match code. Frame used as a concept ("frames"), not a type — OK. Benchmark numbers cite the 2026-06-13 experiment ledger (TODO-VERIFY vs that ledger, not clearly wrong).

### `durable-storage-kernel.md` — DONE
- **[FIXED] G2** — same swizzled-reach (23-bit/8 M/2 TB) clarification added after the 24-bit/4 TB file-capacity line. Gamma 1994 left as a book (no DOI).

### Remaining Tier-1 (`wal-format`, `families`, `formal-verification-map`, `durability-and-recovery`, `lock-free-overlay`, `group-commit`, `architecture/persistence/README`) — delegated to a subagent (formality-weighted), owner-verified after.

### Tier 2 — disk-trie theory (subagents; owner-verified) — DONE
**Theory 01-04** (applied): `03:214,238` "first **8** bytes compared pessimistically" → "**all 12**" (CORRECTNESS — `node.rs:644-653` `match_prefix` compares all stored units, `MAX_PREFIX_LEN=12`); `03:91` "three ways" listed 4 → 3; `01:204` broken `$log_4_0_0(10^9)$` → `$\log_{400}(10^9)$`; `01:205` `$log_2$`→`$\log_2$`; `[doi:]`→`[DOI:]` (×10); **added web-verified DOIs** (Heinz Burst Tries, Aggarwal-Vitter, Bayer-McCreight, HOT, Masstree, Alvarez, Prefix B-trees).
**Theory 05-07** (applied): `04:86` "**52 bits** (Intel 5-level paging)" → "**57 bits**" (CORRECTNESS — LA57 is 57 *virtual* bits; 52 is physical); `04:552` **FABRICATED co-authors** on the SMART/Luo OSDI'23 citation → corrected to the real dblp authors; `05:683` Graefe TODS year **2012→2010** + DOI; `07:89,132,157` in-math `$~1.5$`→`$\approx 1.5$`; `07:116` CX gloss (G5); `[doi:]`→`[DOI:]`.

### Cross-cutting correctness items surfaced by theory review (owner to apply in the coordinated diagram+prose pass)
- **[OPEN] A — ART node body sizes omit the 12-byte `CompressedPrefix`.** Diagrams/prose say Node4 ~48 / Node16 ~160 / Node48 ~656 / Node256 ~2080 B; the crate's own layout comments give **~64 / ~168 / ~668 / ~2076 B** (`nodes/node4.rs:18` "Total: ~64 bytes" = 16-B NodeHeader + 12-B CompressedPrefix + keys[4] + children[32]). Fix `03-adaptive-radix-tree.md` sizes/space-table + the `art-node{4,16,48,256}-fields.svg` + `node-layouts.svg` titles coherently.
- **[OPEN] B — checksum is FNV-1a, not CRC-64.** `FileHeader.compute_checksum` (`disk_manager.rs:146-160`) is **FNV-1a (64-bit)** ("Simple FNV-1a hash" inline `:148`; `0xcbf29ce484222325` basis) though the field doc-comment `:146` mislabels it "CRC-64". Docs+diagrams (`file-header.svg`, `part-block-header.svg`, `storage-backends.md:79`) inherit "CRC-64". → Update docs/diagrams to **FNV-1a (64-bit)**; the mislabeled code comment is FLAGGED for the owner (not edited — docs task). *(Also: `part-block-header.svg` "block_type 0/1/2" describes a generic-technique illustration, not a crate `BlockType` enum — flagged.)*

### NEEDS-OWNER-REVIEW (from theory subagents)
- `07-benchmark-results.md:41,47` — env snapshot dated **2024-12-27** but lists kernel 6.18 / rustc 1.87 (both ~1 yr later); date↔toolchain mismatch (true date unknown — not invented).
- `04-persistent-art.md:554` Graefe FnT monograph year 2010 vs 2011 (DOI added; year left).
- `disk-tries/README.md:67-69` Levenshtein complexity `$O(n\cdot m\cdot d^2)$` uses `n` undefined in the "Where" clause.
- Stale `.rs` comments (not edited): `disk_manager.rs:146` "CRC-64"; `serialization.rs:1820` "MAX_PREFIX_LEN is 8"; both contradict the code.

### Tier 3 — algorithms + abstractions (subagent; owner-verified) — DONE
- `persistent-suffix-graphs.md` (8 edits): the doc used the **suffix automaton's** magic/version/WAL-ext/index-type as *universal* → corrected to **per-family** (`PSUFU8N1/PSTREEB1/PSCDAWG8`, v3/v2/v2, `suffixwal/streewal/scdawgwal`, `NativeSuffix{,Tree,Scdawg}Index`) — `suffix_automaton.rs:40-42`, `suffix_tree.rs:40-42`, `scdawg.rs:38-40`.
- `zippers.md`: **stale** "`CharUnit` does not require `Ord`, sorts via `Debug` formatting" → "`+ Ord`, sorts with `sort_unstable`" (`char_unit.rs:36`, `union_zipper/mod.rs:361-363`).
- `abstractions.md`: `ChildStore<K,V>` type params; adaptive tiers `Tiny/Small/Sorted/SparseIndexed/ByteIndexed48/ByteDense256` (`adaptive_edge_store.rs:72-93`).
- `README.md`: wrong import `dynamic_dawg_u64` → `dynamic_dawg`; CX gloss.
- VERIFIED-OK: `native-u64-and-cx` (CX defined), `serialization` (in-memory `DictionarySerializer` — NOT the on-disk G4 header), `vocab-trie` (empty-string G6 covered).

### Remaining Tier 1 (subagent; owner-verified) — DONE
- `wal-format.md` (9): `[doi:]`→`[DOI:]`; `∈ ≤ ‖` → `$\in$`/`$\le$`/`$\Vert$`; `−`→ASCII `-` in code.
- `families.md` (4): WAL expanded, CX gloss, Leis attribution, **added References** (Leis DOI).
- `formal-verification-map.md` (5): `↔`→`$\leftrightarrow$` (body); **G1 at :65** `> OR > EC`→`> EC` (extra hit, not in ledger list); ADT/SANY glossed. **Proposition tally RE-DERIVED + CONFIRMED EXACT: 992 Theorem + 301 Lemma + 8 Corollary = 1,301; 0 Admitted/Axiom/Parameter.**
- `durability-and-recovery.md`, `architecture/persistence/README.md`: CX glosses. `lock-free-overlay.md`: `ChildStore<K,V>` precision. `group-commit.md`: verified clean (0 edits).

### Cross-cutting fixes (owner) — DONE
- **A (node sizes):** `art-node{4,16,48,256}-fields.puml` redesigned to the crate layout (16-B `NodeHeader` + 12-B `CompressedPrefix` + keys/index + `[SwizzledPtr;N]`), titles ~64/168/668/2076 B; `03-adaptive-radix-tree.md` summary table + 4 figure alt-texts + space-analysis + bytes-per-pointer note recomputed. Re-rendered (widths 544–562, contiguous, byte-stable).
- **B (checksum):** all "CRC-64" → "64-bit …" in `file-header.puml`/`part-block-header.puml`/`storage-backends.md`/`04-persistent-art.md`; storage-backends notes it is **FNV-1a** (`disk_manager.rs:146` code comment mislabels it CRC-64 — FLAGGED). Corpus now has **0** "CRC-64".
- **Diagrams:** `f4-lock-hierarchy.dot` OR-rung removed (3 rungs); NEW `serialized-node-header.puml`; `suffix-graph-publish.puml` comment made family-neutral; `formal-verification-map.md` H1 made heading-safe (MathJax-in-heading unreliable on GitHub). MathJax set-notation fixes (`\{\text{…}\}`) in `eviction.md`/`zippers.md`; eviction.md enum-in-math (`$Low \Rightarrow$` → `` `Low` $\Rightarrow$ ``).

### Verification — GREEN
- **Diagrams:** full render 125 exit 0; **byte-stable**; hygiene `--check` OK; every touched figure ≤700 px, node byte-tables contiguous (gap==height).
- **Consistency (corpus-wide grep):** 0 stale `> OR > EC`; 0 `CRC-64`; 0 stale node sizes (~48/160/656/2080); 0 lowercase `[doi:`; block_id 24-bit-cap vs 23-bit-reach distinct in both files; hierarchy `CK > merge_lock > EC` present; no bare-identifier-in-`$…$` misuse.
- **Citations:** 4 core DOIs resolve; ~12 real DOIs added (all web-verified); 1 **fabricated** citation fixed; 1 wrong year fixed.

## Follow-through — the flagged items, now RESOLVED
1. **Stale code doc-comments — FIXED** (comment-only; documentation content contradicting its own code):
   `disk_manager.rs:146` "CRC-64"→"64-bit FNV-1a"; `nodes/mod.rs:20-25` NodeHeader ASCII field order → matches the struct; `serialization.rs:1820` "MAX_PREFIX_LEN is 8"→"is 12"; `dict_impl.rs:345-346,416-417` `> OR > EC`→`> EC`. *(Rust-analyzer "unlinked-file" notices only; no logic touched.)*
2. **`07-benchmark-results.md` env anachronism — RESOLVED:** kept `2024-12-27` (it is an embedded identifier — internal anchor + 3 prose refs + the `disktrie-durable-throughput` chart) and added a transparent note that the recorded kernel/rustc postdate it (so it reads as an identifier, not a capture date). No date invented.
3. **`disk-tries/README.md` Levenshtein `$O(n\cdot m\cdot d^2)$` — RESOLVED:** `n` defined as query (input) length in the Where clause.
4. **Baked diagram render defects — FIXED** (owner-reported, then swept comprehensively):
   - `ParticipantPadding` deprecation banner: removed the deprecated `skinparam ParticipantPadding` from `epoch-reclamation.puml` + `suffix-graph-publish.puml` (widths unchanged 696/679).
   - `serial-disk-ptr-stamp.puml`: `<#hex>**SAFE**`/`**REFUSED**` (the `<#hex>` shorthand renders *literally* in sequence-message text) → `<color:#2E7D32>`/`<color:#C62828>` (now green/red, not literal).
   - `tier1-lock-acquire.puml`: deprecated *leading-color* activity form `#hex:label;` → trailing `:label;<<#hex>>` (banner gone, colors preserved).
   - `committed-watermark.svg` "ERROR": FALSE POSITIVE (redaction-hook noise; it's a D2 diagram).
   - **Hardened the hygiene gate** (`render-diagrams.sh:113`) to catch `Please use …instead` AND any `end of the line` deprecation banner (self-tested: fires). **Corpus-wide scan: 0 baked warnings / literal tags anywhere.**

## Remaining (owner's standing rule)
- **Commit:** both this review AND the prior diagram-contiguity work are uncommitted on branch `docs/diagram-contiguity-swatch-legends` — **not committed per your standing rule "commit only when the user asks"** (which overrides the no-deferral goal). Ready to commit (combined or split doc-review vs diagram-contiguity) whenever you say.
