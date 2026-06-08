# Task #43 (CX) — Path-Compressing Overlay↔Dense Codec — converged design (2026-06-08)

> **Status: RED-TEAMED (pass 1) → REFINED; re-red-team PENDING before the serialize/load impl. CX.1
> no-truncation core LANDED + PROVEN (Rust exhaustive test + Coq `Model/PrefixChunking.v`, 0 admits).**
> Red-team #1 verdict: the **chunker math is SOUND** (the owner's truncation fear is NOT realized in
> the chunker — exhaustively verified), and the wire format **already round-trips `prefix_len>0`**
> correctly (fail-closed validation + a passing layout test). It raised **2 blocking design
> refinements** (now folded in below) + several non-blocking gates.
>
> **Refinement (chunk width = `max_prefix + 1`, NOT `W`).** Each dense chain-node packs at most
> `max_prefix` prefix units PLUS one outgoing edge unit, so the optimal lossless chunk width is
> `max_prefix + 1` (= 7 char / 13 byte), giving `ceil(L / (max_prefix+1))` nodes — fewer than the
> Plan's `W` and within the format's `prefix_len ≤ max_prefix` cap (prefix = `chunk[..len-1]`,
> edge = `chunk[len-1]`). Same no-truncation/correctness; strictly better density.
>
> **CX.1 core landed:** `src/persistent_artrie_core/overlay/codec.rs` — `chain_chunks` (the shared,
> K-generic, PURE no-truncation splitter) + an EXHAUSTIVE property test
> (`chain_chunks_never_truncates_exhaustive`: all chain lengths 0..=128 × all widths 0..=14 →
> `concat(prefix ++ [edge]) == input`, prefix ≤ cap, count = `ceil`). GREEN. This is the executable
> twin of the Rocq `chunk_concat_id` (T2). Remaining: peel + emit (serialize), load/expand, density
> gate, byte twin, Rocq proof, vocab.
> **Owner mandate (2026-06-08): KEEP path compression, but PROVE no-truncation.** The owner is
> specifically concerned that `MAX_PREFIX_LEN` could truncate/lose key data. So the **NO-TRUNCATION
> safety property is the #1 proof obligation** — a Rocq theorem, not a code comment. The codec MUST
> chunk a single-child chain longer than `MAX_PREFIX_LEN` across MULTIPLE dense nodes; it must NEVER
> truncate.

## Why this exists
L2 (#42) and L3 (#44) delete the OWNED in-memory tree. To do that without regressing on-disk size, the
overlay needs its own overlay↔dense serialize/load that realizes ART path compression. This task builds
+ PROVES that codec, DORMANT/REVERSIBLE (new functions, NOT wired into production capture/reopen — L2/L3
wire them later). State map: `docs/design/cx-task43-state-mapping-2026-06-08.md`.

## Ground truth (cite-anchored, verified)
- Overlay is UNCOMPRESSED (one node per unit, `prefix_len=0` always); traversal is **prefix-UNAWARE**
  (`OverlayNode::match_prefix`/`prefix_matches`, node.rs:994/1009, have **0 non-test callers**). ⇒ the
  in-memory overlay must STAY uncompressed; the codec compresses only ON DISK and EXPANDS on load.
- `MAX_PREFIX_LEN` = **12 byte / 6 char** units (`key_encoding.rs:43,79`, `KeyEncoding` trait const, from
  G4/#16). The dense format already reserves `prefix_len` (`SerializedCharNodeHeader`, validated
  `≤ CHAR_MAX_PREFIX_LEN` at serialization_char.rs:261) and `deserialize_char_node_v2` already READS it
  (:1403) — so **no on-disk format change, no version bump**; CX just exercises `prefix_len>0`.
- **Two existing bugs the codec must bypass (and incidentally fixes):** `load_char_node_from_disk_lazy`
  (disk_io.rs:357-378) **silently DROPS the prefix**; `inner_to_overlay` (persist.rs:1663) uses the
  **TRUNCATING** `OverlayNode::with_prefix` (node.rs:717, `.min(MAX_PREFIX_LEN)`). The byte
  `path_compression.rs::extend_prefix` (:231) **also truncates** — but it has **0 non-test callers**
  (test-only). The codec must use NONE of these for >W chains.

## Architecture
- **Serialize (overlay→dense, compressing):** reuse the EXISTING iterative post-order driver +
  `serialize_one_char_node_to_disk` core VERBATIM. The only new write-side ingredient is
  `overlay_inner_single_node_with_prefix` (= `overlay_inner_single_node` + stamps `header.prefix_len` +
  `*prefix_mut()`). A maximal single-child non-final no-value **chain** is peeled and its accumulated
  unit-string `Lp` is **chunked from the bottom** at width `W`, emitting a vertical stack of
  `ceil(|Lp|/W)` dense nodes (each carrying a chunk's prefix + one outgoing edge).
- **Load (dense→overlay, expanding):** read the prefix off the `CharNode` DIRECTLY (not the
  prefix-dropping lazy helper); for a node with `prefix_len=p`, **expand** into `p` single-child
  `prefix_len=0` non-final no-value intermediates + the real node — byte-for-byte the structure the
  overlay WRITE path builds, so prefix-unaware traversal is unaffected. Lazy: grandchildren stay
  `Child::OnDisk`.

### Collapsibility + the chunk-boundary nail (off-by-one)
`is_prefix_link(n) ≡ n.num_children()==1 ∧ ¬n.is_final() ∧ ¬n.has_value()`. Peel down through links,
accumulating each link's **edge unit**, until a terminus (final / valued / ≠1 child / **OnDisk sole
child** — peel only continues while the sole child is `InMem`, so serialize NEVER faults disk). The unit
INTO the terminus and each chunk's OUTGOING edge are **edge-labels, NOT prefix units**; only `chunk_len−1`
units are prefix: `(pfx, out_edge) = (chunk[..len-1], chunk[len-1])`. Bottom remainder width =
`if L mod W == 0 then W else L mod W` (never an empty bottom chunk).

Worked example (char, `W=6`, `L=13`): `Lp` splits `[6 | 6 | 1]` → emit bottom-up `node_C(prefix=[],edge=u13→t)`,
`node_B(prefix=[u7..u11],edge=u12→C)`, `node_A(prefix=[u1..u5],edge=u6→B)`; reload expands to
`5+1+5+1+0+1 = 13` units — nothing lost.

## The proof (Rocq, `formal-verification/rocq/Spec/OverlayDenseCodecSpec.v`, NO admits/axioms)
Model overlay (`Ov`) + dense (`De`) as inductive trees over a unit alphabet; logical map `keys_ov`/`keys_de`
via the existing `starts_with`/`skipn`/`CharMap`/`same_char_map` (PersistentPrefixSpec.v). `encode`/`decode`
as total `Fixpoint`s.
- **T2 (NO-TRUNCATION / totality — proved FIRST, the heart):**
  `chunk_concat_id : concat (chunk_from_bottom l W) = l` (no unit dropped);
  `chunk_count : length (chunk_from_bottom l W) = div_up (length l) W`;
  `chunk_width_bound : In c (chunk_from_bottom l W) → length c ≤ W` (so `from_chars` never panics);
  `encode_preserves_chain`. We deliberately do NOT model a truncating `extend`, so the proof cannot
  certify a truncating impl.
- **T1 (round-trip / logical-map identity):** `decode_encode_id_map` via `encode_sound`/`decode_sound`,
  using `strip_prefix_concat` (telescopes the chunk stack to `strip_prefix Lp`, depends on T2's
  `chunk_concat_id`) + reused `split_preserves_bytes`/`firstn_nth_skipn` (Model/PathCompression.v).
- **T3 (idempotence / density normal form):** `encode_decode_id` over the `well_chunked` normal form
  (operational instance of `maximally_compressed`) — the formal core of the density gate.
- Instantiate `Spec/SerializationRoundtripSpec.v`'s record-of-laws (discharge `map_decode_roundtrip`
  FROM T1) to inherit the 8 derived serializer-law theorems.

## Back-compat (one loader, no version bump)
Reads (i) current `prefix_len=0` images, (ii) legacy owned `prefix_len>0` images (FIXES the existing
prefix-drop bug for them too), (iii) new compressed images. `CHAR_FORMAT_VERSION` stays 2.

## Test matrix (in-crate `cx_compressed_codec_correspondence`, `target/test-tmp` scratch — NOT tmpfs)
empty trie · single term · branching (N4→N16→N48→Bucket + astral-plane char) ·
**`cx_no_truncation_deep_chain_3W_plus_1`** (term length `3·MAX_PREFIX_LEN+1`; assert membership exact +
chunk count = `ceil((len-1)/W)` + re-serialize byte-identical) · `cx_chain_exact_multiple_of_W`
(off-by-one) · arbitrary V incl. empty-string-value root · OnDisk child in a collapsible chain (assert no
fault) · **`cx_density_eq_owned_serialize_root`** (CX image ≤ owned image) · back-compat (i)+(ii).

## Phasing (each compiles + suite green; dormant/reversible throughout)
- **CX.1** char serialize-with-chunking (`is_prefix_link`, `overlay_inner_single_node_with_prefix`,
  `peel_chain`, `chunk_from_bottom`, `serialize_overlay_snapshot_compressed`) + pure unit tests of the
  chunker (no disk).
- **CX.2** char load-with-expand (`expand`, `load_overlay_root_compressed`) + round-trip + the
  no-truncation test.
- **CX.3** byte twin (W=12, `CompressedPrefix::from_bytes`).
- **CX.4** density gate + back-compat tests.
- **CX.5** Rocq proof (T1/T2/T3) wired into `scripts/verify-formal-correspondence.sh`.
- **CX.6** vocab parallel only if it has a distinct overlay→dense path (else inherits char).
- **Hygiene (owner concern):** `#[cfg(test)]`-gate or `#[deprecated]` + `debug_assert!` the truncating
  byte `extend_prefix` (and char analogue) so a future caller that would truncate trips in tests; document
  the prohibition in the CX module header. (0 non-test callers today — confirmed.)

## Red-team seeds (truncation-adjacent)
chunk-boundary off-by-one (edge vs prefix unit); chain length an exact multiple of W (remainder→W not 0);
value/finality at a boundary (impossible — only on terminus); 1-unit remainder; char units > 0xFFFF;
OnDisk child mid-chain (peel stops, no fault); `from_chars` panic (T2 `chunk_width_bound` forbids);
the truncating `extend_prefix`/`with_prefix` being wired later (gate/deprecate); arena placement/density
(bottom-first emission matches post-order alloc); deep-chain Drop depth (iterative Drop already flattens).

## Red-team #1 result + convergence refinements (2026-06-08)
**Verdict: chunker SOUND (no truncation, exhaustively verified); BLOCKED on 2 design refinements,
now closed below. Re-red-team to confirm before the serialize/load impl.** Confirmed sound: the
`(prefix=chunk[..len-1], edge=chunk[len-1])` split for all `L ∈ {0,1,W-1,W,W+1,2W,2W+1,3W+1,100}`;
`from_chars` cannot panic (prefix ≤ W-1); OnDisk-mid-chain (peel stops at the InMem boundary, no
fault); density (CX is *never worse* than uncompressed — `ceil(L/W) ≤ L`, and a `w=1` chunk writes
NO prefix block, byte-identical to uncompressed); back-compat (no existing test depends on the
prefix-drop bug; the wire format already round-trips `prefix_len>0` per
`tests/persistent_char_node_layout_correspondence.rs:46-65` + fail-closed `prefix_len ≤ MAX` validate).

**BLOCKING-1 — pin the edge↔prefix convention (the off-by-one's real home).** The chunker has no
off-by-one, but the *parent→chunk edge identification* was unspecified — the one place a reasonable
impl could double-count the incoming edge (lengthening every multi-chunk key by one unit per
boundary). FIX (now normative):
- `expand(incoming_edge g, stored_prefix [p1..pk], out_edge e)` ⟶ the unit path `g · p1 · … · pk · e`
  (k+2 edges). The chunk's STORED prefix is the units **strictly between** the incoming edge and the
  out-edge — the serializer must NOT also place `g` in the prefix. (Worked example reconciled:
  `node_A` is reached from its parent `P` by edge `u1`; `node_A.prefix = [u2..u6]` (the inter-edge
  units); `node_A.out_edge = u7`. The chunk fed to `chain_chunks` for the head is the unit string
  starting at the head's outgoing edge; the head's own incoming edge is the parent's child-key.)
- **Differential test (mandatory, NEW):** `assert_overlay_structural_eq(load_overlay_root_compressed(img),
  build_overlay_root_from_terms(terms))` — node-by-node, edge-key-by-edge-key, against the PROVEN
  term-level builder (`overlay/f5_build.rs:90`). The byte-identical re-serialize gate is necessary but
  NOT sufficient (a consistent off-by-one survives it); the structural diff against the term-builder
  catches convention drift directly.

**BLOCKING-2 — the Rocq tree-level model must be FAITHFUL (do NOT reuse the truncating
`PathCompression.v`).** The new `Spec/OverlayDenseCodecSpec.v` must:
- be parameterized over `W : nat` with `0 < W` and instantiated TWICE (char 6, byte 12) — NOT reuse
  `Model/PathCompression.v`'s `extend_prefix` (it truncates) or `CompressedPrefix` (bakes
  `prefix_len ≤ 12`, byte-only). It MAY (and does) reuse `Model/PrefixChunking.v` (the proven,
  width-parametric, non-truncating chunk core).
- model the dense node with an **explicit per-child edge unit distinct from the prefix**:
  `keys_de (Node prefix children) := flat_map (fun '(e, sub) => map (fun k => prefix ++ [e] ++ k)
  (keys_de sub)) children` — the `++ [e] ++` is load-bearing (it is the edge↔prefix split of
  BLOCKING-1, formalized). Folding the edge into the prefix would be internally consistent but
  UNFAITHFUL to the Rust and would mask the off-by-one.
- supply the two bridge lemmas the sketch omitted: `build_keys_id : keys_ov (build_tree_from_map m) = m`;
  and EITHER a wire-format round-trip lemma OR an explicit statement that the Rocq claim is scoped to
  the **tree level** with wire round-trip kept as the Rust layout-correspondence test (state the
  boundary; do not paper over it).
- define `well_chunked` OPERATIONALLY (all-but-last chunk width = W, last ∈ [1,W], each node
  `is_prefix_link`-collapsed) — NOT via the non-decidable `maximally_compressed`. Termination of
  `encode`/`decode`: explicit measure = chain length (−W per chunk, −1 per expand intermediate).

**Non-blocking gates (fold into impl/tests):**
- (#4) `debug_assert!` in `expand`: every synthesized intermediate has `prefix_len==0 ∧ !is_final ∧
  value.is_none() ∧ num_children==1`; the differential test re-checks this structurally.
- (#6) **eviction registry path length:** when emitting a chunk with `prefix=[p1..pk]`, extend the
  serializer's registry `path` by ALL `k+1` units (prefix + out-edge), NOT just the edge, so
  `path.len()` equals the node's true logical depth — else `durable_stamp`/parent-integrity +
  relative-encoding sibling assumptions desync. Add an evict-then-refault test over a compressed chunk
  node asserting the registry path length.
- (#7) rename the density test `cx_density_eq_owned` → `cx_density_le_owned` (the gate is `≤`, and
  must be re-run if owned compression is ever enabled).
- (#5d) make NUL (`\0`) and astral (`U+10FFFF`) prefix units EXPLICIT mandatory Rust tests (the Rocq
  alphabet is `nat`, giving zero coverage of `char::from_u32`/`unit_from_le_bytes`); decide + document
  whether a non-scalar prefix unit on load is a corruption ERROR or a `U+FFFD` substitution (the
  existing `units_to_term` silently substitutes — CX should choose deliberately).
- (#1) invariant to assert in the serializer: `sum(chunk_widths) == path_len(head→terminus inclusive)`.
