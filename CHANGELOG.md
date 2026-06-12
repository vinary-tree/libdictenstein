# Changelog

All notable changes to libdictenstein are recorded here.

Date format is ISO-8601 (YYYY-MM-DD).

## Unreleased

### Changed

- **PathMap dictionary nodes rebuilt on `TrieRef` (lock-free, `𝒪(1)`-from-focus).**
  `PathMapNode` / `PathMapNodeChar` are now type aliases of the new
  `TrieRefNode` / `TrieRefNodeChar` (`pathmap::core`) over a sealed `TrieRefLike`
  handle. `PathMapDictionary{,Char}::root()` takes an `𝒪(1)` copy-on-write
  snapshot and queries run **lock-free** over it (snapshot isolation), replacing
  the former lock-per-operation, root-replay node (`𝒪(n²)` byte-steps + `n` lock
  round-trips to walk a term of length `n`). `PathMapZipper` is likewise reworked
  onto `TrieRefZipper`. Fields were private, so there is no downstream breakage.
- **All dictionary families reorganized into directory submodules** —
  `pathmap`, `dynamic_dawg`, `double_array_trie`, `suffix_automaton`, `scdawg`,
  and `persistent_artrie` (with `char/`, `core/`, `vocab/`) — each as
  `family/{mod,ascii,char,…}.rs`, with `mod.rs` re-exporting the family's public
  types. The crate-root re-exports and prelude are preserved; **no
  compatibility shims** remain for the old flat module paths.

### Added

- **Zero-plumbing, MORK-facing dictionaries** (`pathmap::snapshot`):
  `PathMapSnapshot` / `PathMapRef` (and `…Char` variants) wrap a **borrowed** or
  `𝒪(1)`-snapshotted `PathMap` so a caller that already holds one (e.g. MORK's
  `Space.btm`) can fuzzy-query it with no copy and no lock. Constructors:
  `from_map`, `from_map_ref`, `from_trie_ref`, `from_read_zipper`. Plus
  `PathMapDictionary{,Char}::snapshot()` and a borrowed `PathMapZipperRef<'a>`.

### Dependencies

- `pathmap` requirement widened to `>=0.2.2, <0.4` (publishable — resolves to
  0.2.2 on crates.io; accepts a local 0.3 via `[patch.crates-io]`). Verified to
  compile against PathMap 0.3.0 (0 API errors).

### Build infrastructure

- **`.cargo/config.toml`**: scoped `target-cpu=native` down to
  `target-feature=+aes,+sse2` (the minimum gxhash requires). Native
  builds remain available via `RUSTFLAGS="-C target-cpu=native"`. The
  previous unconditional setting silently produced binaries that emitted
  illegal-instruction signals on slightly older x86_64 hardware.
- **`.github/workflows/ci.yml`** replaces `coverage.yml` with a
  full-coverage CI matrix:
  - 11-config feature matrix (default + no-default + persistent-artrie +
    pathmap-backend + io-uring-backend + parallel-merge + serialization +
    protobuf + lling-llang + all-features + macOS default).
  - Clippy with `-D warnings`.
  - Doc with `RUSTDOCFLAGS=-D warnings` (broken intra-doc links fail CI).
  - rustfmt `--check`.
  - MSRV at 1.70 (matching `rust-version` in Cargo.toml).
  - Nightly coverage with branch tracking (stable degrades branch coverage).
  - Sanitizer matrix (ASan, TSan).
  - Rocq proofs (`make` in `formal-verification/rocq/`).
- **`cargo fmt`** applied across the workspace; ~200 files normalized.
  CI now enforces drift via `cargo fmt --check`.
- **lru** upgraded from `0.12` → `0.18`. API unchanged at our call site
  (`LruCache::new(NonZeroUsize)`); 7 reverse_cache tests still pass.

### Tier C (architecture / dedup)

- **`src/serialization/serde_helpers.rs`**: extracted shared
  `serialize_arc_vec` / `deserialize_arc_vec` / `serialize_arc_vec_vec` /
  `deserialize_arc_vec_vec` (previously byte-for-byte duplicated across
  `double_array_trie.rs` and `double_array_trie_char.rs`). Both DAT files
  now `use` them.
- **`src/sync_compat.rs`**: std-fallback `RwLock` wrapper now has
  `try_read` / `try_write` matching parking_lot's `Option<Guard>` shape.
  Single canonical type per build; the audit's "two RwLock types" issue
  resolved.
- **`src/union_zipper/`**: 1632-LOC `union_zipper.rs` split into 4 modules
  (`merge_strategies.rs`, `lattice.rs`, `semiring_lattice.rs`, plus
  `mod.rs` for the zipper + iterator + extension traits + tests). All
  external `use libdictenstein::union_zipper::{FirstWins, LatticeJoin,
  …}` call-sites continue to work via `pub use` re-exports.

### Tier B (API parity)

- **`DictionaryFactory`** expanded from 4 backends → 11
  (`DoubleArrayTrie{,Char}`, `DynamicDawg{,Char,U64}`,
  `SuffixAutomaton{,Char}`, `Scdawg{,Char}`,
  `PathMapDictionary{,Char}`). Persistent-ARTrie family excluded
  (needs file paths). +1 test added for Unicode backends.
- **Value-preserving serializers (`*_with_values_char`)** for char-Unit
  backends. Combined with the byte path (added in A3), bincode/json/
  plaintext now round-trip `(String, V)` pairs for both `Unit = u8` and
  `Unit = char` backends. u64 (`DynamicDawgU64`) still has no
  `*_with_values` path because u64 doesn't trivially round-trip through
  `String`; needs format design.
- **`Scdawg<V>::get_value` + `MappedDictionary` impls** added for parity
  with `ScdawgChar`. Both Scdawg variants now usable with the
  value-preserving serializers and `MappedDictionary` callers.
- **`docs/dynamic_dawg/suffix_cache_bug.md`** documents why the
  `find_or_create_suffix` cache in `dynamic_dawg{,_char}.rs` is
  `#[allow(dead_code)]` (the dynamic-insertion path violates the cache's
  endpoint-uniqueness invariant). Includes design candidates for
  re-enabling.
- **`DoubleArrayTrie{,Char}::free_list`** and `rebuild_threshold` fields
  documented as RESERVED-FOR-FUTURE; kept in the on-disk format for
  back-compat.
- **`MutableDictionary` docstring** rewritten to call out the
  complementarity with `MutableMappedDictionary` (set-like vs value-
  aware; both needed; neither subsumes the other).
- **`ARTrieAtomicOps`** trait body commented out per CLAUDE.md (no impl
  sites; signatures conflicted with `ARTrie`'s own methods). Empty
  `#[deprecated]` stub kept for back-compat.
- **`EvictableARTrie::{enable,disable,force}_eviction`** changed from
  `&mut self` to `&self` (3 impl sites updated). The `&mut self` bound
  was performative — impls already mutate through interior write guards.
- **`ARTrie::durability_policy`** return type points at
  `crate::persistent_artrie::core::durability::DurabilityPolicy`
  (canonical home); the byte-side `pub use` re-export is retained.
- **`ARTrie::iter_prefix_units`** sibling method added that preserves
  `Self::Unit` typing (the old `iter_prefix` returns `String`, lossy for
  non-byte impls). Default fallback round-trips through `iter_prefix`.

### Tier A (correctness — silent bugs fixed)

- **`BijectiveDictionary::get_term`** now returns `Option<Cow<'_, str>>`
  (was `Option<&str>`). Drops the unsafe pointer dereference in
  `BijectiveMap`. `PersistentVocabARTrie` / `SharedVocabARTrie` impls now
  return reconstructed terms instead of unconditional `None` (which
  silently violated the documented bijection invariant). New
  `tests/bijective_trait_invariant.rs` covers all 3 impls.
- **`MappedDictionary::get_value`** broken default removed — was `let _ =
  self.contains(term); None`. Now required; all 15 in-tree impls already
  provided one.
- **Value-preserving serializers** for `MappedDictionary` impls:
  `DictionaryFromTermsWithValues` trait + `extract_terms_with_values`
  helper + `serialize_with_values` / `deserialize_with_values` methods on
  bincode/json/plaintext. The legacy `serialize`/`deserialize` path
  silently dropped values (shipped `Vec<String>` over the wire). New
  `tests/serialization_value_roundtrip.rs` covers DynamicDawg/DAT byte
  variants, char variants, and a regression-guard for the legacy
  drop-values behavior.
- **`SharedVocabARTrie` no-op stubs** now emit `log::warn!` on every call
  with non-default arguments, naming the discarded argument and
  recommending an alternative. Behavior unchanged for back-compat. New
  `tests/vocab_trait_honesty.rs` pins every documented sentinel return.
- **`FilterableValue::Atom`** associated type added; `Vec<T>` / `HashSet<T>`
  / `SmallVec<A>` get per-element semantics
  (`self.iter().any(predicate)` / `self.iter().all(predicate)`) instead
  of the previous "test the whole collection" behavior that made
  `matches_any` and `matches_all` indistinguishable.
- **`extract_terms` iterative rewrite** prevents stack overflow on long
  single-child chains. Bug found during implementation: the depth field
  in the new explicit-stack frame must be captured BEFORE pushing the
  descent byte (using AFTER caused the post-backtrack `current_term` to
  retain the wrong prefix). New 50k-deep DynamicDawg test in
  `serialization/mod.rs`.
- **5 `.unwrap()` → `.expect("invariant: …")`** in production code
  (`suffix_automaton{,_char}.rs`, `dynamic_dawg_char.rs`) with forensic
  messages tied to the actual invariant.
- **`transducer` doc-rot** fixed: 6 `rust,ignore` doc-tests in
  `suffix_automaton{,_char}.rs` rewritten to use the real API +
  `libdictenstein::prelude::*`, with a pointer to where the transducer
  actually lives (downstream in `liblevenshtein`). All 6 now run.
- Pre-existing orphan `/// ```` doc fence at `prefetch_api.rs:14` fixed
  (broke `cargo test --doc` whenever the file was visited).

### Phase 0 hygiene

- **`formal-verification/VERIFICATION_RESULTS.md` and `README.md`**
  refreshed to reflect 15 .v files / 232 propositions / 0 Admitted /
  0 Axiom across the Rocq tree (commits `b7630ad` + `efe1943`).
- **Dead Cargo features removed**: `simd`, `scdawg-bloom`, `scdawg-simd`
  (all had zero `#[cfg(feature = …)]` references in code).
- **`group-commit`** feature relabeled EXPERIMENTAL with cross-reference
  to `docs/persistence/group_commit_regression.md`. Behavior unchanged.
- **Sanitizer logs** relocated from repo root to `docs/sanitizers/`
  with date-stamped filenames + `scripts/run-sanitizers.sh` regen script.
- **`build.rs`** emits `cargo:rerun-if-changed=proto/libdictenstein.proto`
  (under `#[cfg(feature = "protobuf")]`).
- **`.gitignore`** for the `formal-verification/rocq/**/.*.aux` files.

### Documentation

- **`src/lib.rs` backend table** refreshed: 11 in-memory + 3 disk-backed
  backends, all linked.
- **README.md Quick Start** explains prelude usage; mirrors the lib.rs
  table; pointer to `DictionaryFactory`.
- **`docs/persistence/mmap-architecture.md`** refreshed to reference the
  post-Phase-6 file layout (`persistent_artrie/core/{disk_manager,
  buffer_manager, swizzled_ptr, block_storage, io_uring_disk_manager,
  wal, durability}.rs`).
- New **`docs/algorithms/implementations/scdawg.md`** and
  **`docs/algorithms/implementations/bijective.md`**.
- **25 of 148 `rust,ignore`** doc-tests promoted to compile-checked
  `rust,no_run` (the conversion script wraps `?`-using bodies in a
  hidden `fn main() -> Result<…>`; the remaining 123 use API patterns
  that need context-specific per-block rewrites and stay as
  `rust,ignore`).

### Test growth

Pre-plan: 2006 passing. Post-plan: **2288 passing** (+282).

### Removed
- **Cargo features**: dropped three unused feature flags that the codebase
  never referenced (`simd`, `scdawg-bloom`, `scdawg-simd`). No-op for any
  downstream consumer that wasn't getting any SIMD/bloom-filter behavior from
  them anyway.

### Changed
- **Cargo feature `group-commit`**: relabeled from "REJECTED: causes regression
  on NVMe" to "EXPERIMENTAL" with explicit benchmark cross-reference. The
  feature itself is unchanged; the description is now honest about its status.
  See [docs/persistence/group_commit_regression.md](docs/persistence/group_commit_regression.md).
- **`README.md` Features section**: now lists all 11 real features
  (was: 6, with 3 referring to dropped flags).
- **`build.rs`**: emits `cargo:rerun-if-changed=proto/libdictenstein.proto`
  under `#[cfg(feature = "protobuf")]`, so cargo correctly rebuilds the
  generated protobuf code when the schema changes.
- **`formal-verification/VERIFICATION_RESULTS.md` and
  `formal-verification/README.md`**: refreshed to reflect the current state —
  15 .v files, 232 propositions, 0 `Admitted` / 0 `Axiom` / 0 `Parameter`.
  The "Admitted Theorems", "Proven Theorems" and "Future Work" sections now
  match the actual proof tree (commits `b7630ad` and `efe1943`).
- **Sanitizer-result logs**: relocated from repo root to `docs/sanitizers/`,
  with date-stamped filenames and a `scripts/run-sanitizers.sh` regen script.
- **`.gitignore`**: added `formal-verification/rocq/**/.*.aux` to silence the
  cosmetic dot-prefix `.aux` files that Rocq leaves behind.

### Documentation
- Added [docs/persistence/group_commit_regression.md](docs/persistence/group_commit_regression.md)
  explaining why `group-commit` regresses on NVMe and where it's still
  expected to help.
- Added [docs/sanitizers/README.md](docs/sanitizers/README.md) explaining
  the snapshot archive layout and how to regenerate.

### Plan
- Tracking the broader crate-wide tech-debt repair plan at
  `/home/dylon/.claude/plans/rust-backtrace-1-rust-log-debug-cargo-n-purrfect-lemon.md`
  (7 phases: Hygiene → Tier A correctness → Tier B API parity → Tier C
  architecture/dedup → CI/build infra → Documentation → Verification).
