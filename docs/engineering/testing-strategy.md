# Testing strategy

**Navigation**: [← docs](../README.md) · [Benchmarking methodology](benchmarking-methodology.md) · [Feature flags](feature-flags.md)

How libdictenstein is tested, layer by layer, and why each layer exists. The through-line is
**correspondence**: wherever a component has a formal specification (a Rocq theorem or a TLA⁺ model),
a Rust test pins the implementation to it, so the proofs are not decorative. Notation follows
[`docs/notation.md`](../notation.md).

## The layers

| Layer | Roughly | Where | Catches |
|-------|--------:|-------|---------|
| Unit tests | **~1,865** `#[test]` in `src/` | inline `mod tests` | per-function correctness, invariants |
| Integration tests | **~969** `#[test]` across ~103 files | [`tests/`](../../tests/) | cross-module behavior, trait laws, recovery |
| Property tests | 105 `proptest!` sites | `tests/proptest_*.rs`, in-module | edge cases over generated inputs |
| Concurrency (loom) | 8 files | `tests/*_loom*.rs`, `*_correspondence.rs` | lost writes / torn reads under all interleavings |
| Doctests | every public API + README examples | rustdoc | that the documented usage compiles and runs |
| Formal correspondence | ~50+ targets | `scripts/verify-formal-correspondence.sh` | that the code matches the Rocq / TLA⁺ specs |
| Sanitizers | ASan · MSan · TSan · Miri | `scripts/run-sanitizers.sh` | data races, UB, memory errors |

## Correspondence testing — the spine

The dominant integration idiom is the **correspondence test**: a Rust test whose assertions mirror a
machine-checked specification, so a regression in the code is caught as a divergence from the proof.
Examples across the tree: `dictionary_law_correspondence.rs` (trait algebra),
`persistent_artrie_formal_correspondence.rs` and `persistent_wal_atomicity_correspondence.rs`
(durability), `recovery_replay_completeness_correspondence.rs` (crash recovery),
`serialization_correspondence.rs` / `protobuf_compression_correspondence.rs` (round-trips), and
`unsafe_boundary_contracts.rs` (the `unsafe` contracts of [the security cluster](../security/unsafe-contracts.md)).
There are on the order of sixty such files. This is why the [security threat model](../security/threat-model.md)
can treat correctness as *checked* rather than asserted.

## Property-based testing

`proptest 1.4` (dev-dependency) drives ~105 sites, configured by
[`proptest.toml`](../../proptest.toml): **256 cases** per property, **`fork = true`** (each case runs
in its own process, so a segfault or OOM in one case cannot poison the run), `max_shrink_iters =
10000`, a 30 s per-case timeout, and a persisted `proptest-regressions` corpus so a once-failing
input is replayed forever. Shared input strategies live in `tests/common/strategies.rs`. Dedicated
files cover the trait surface (`proptest_core_dictionaries.rs`, `proptest_trait_macros.rs`,
`proptest_zipper_operations.rs`, `proptest_serialization.rs`, `proptest_bijective.rs`).

## Concurrency: loom + stress

Lock-free code is checked two ways. **[loom](https://docs.rs/loom) 0.7** exhaustively explores the
legal thread interleavings of the CAS paths — `tests/dynamic_dawg_u64_correspondence.rs` and
`tests/bloom_filter_correspondence.rs` cover the volatile side; the persistent side adds
`persistent_lockfree_overlay_loom.rs`, `persistent_lockfree_durable_loom.rs`, the F4 lock-hierarchy
looms, and `persistent_worker_lifecycle_loom.rs`. Complementing the exhaustive-but-small loom runs,
`tests/volatile_lockfree_concurrency.rs` and the persistent **soak** tests
(`persistent_f4_lock_collapse_soak.rs`, `vocab_shared_lockfree_soak.rs`) drive real threads at scale.
The design these check is documented in [design/volatile-concurrency.md](../design/volatile-concurrency.md)
and [persistence/concurrency-model.md](../persistence/concurrency-model.md).

## Sanitizers

[`scripts/run-sanitizers.sh`](../../scripts/run-sanitizers.sh) runs the suite under four nightly
sanitizers — **AddressSanitizer**, **MemorySanitizer** (with origin tracking), **ThreadSanitizer**,
and **Miri** (a curated subset, since the full suite is too slow under Miri) — each via
`cargo +nightly test --all-features`. Logs land in `docs/sanitizers/<tool>-results-<date>.log`
(git-ignored by default; the directory and its [README](../sanitizers/README.md) are tracked, and a
snapshot can be committed with `git add -f`). TSan carries the most signal for the lock-free code.

## The formal-correspondence harness

[`scripts/verify-formal-correspondence.sh`](../../scripts/verify-formal-correspondence.sh) is the
umbrella that ties code to proofs. It runs: the `unsafe`-inventory set-equality gate (see
[security/unsafe-contracts.md](../security/unsafe-contracts.md)); ~50+ `cargo test` correspondence
targets across feature sets; the Rocq build (`make -C formal-verification/rocq`); a `tla2sany` SANY
syntax check of every `.tla` module; and — opt-in — `RUN_MIRI=1` (~10 Miri targets),
`RUN_IO_URING=1`, and `RUN_TLC=1` bounded model checking *with negative controls* (`_Unsafe.cfg`
models that MUST fail, proving the checker would catch a real violation).

## Continuous integration

[`.github/workflows/ci.yml`](../../.github/workflows/ci.yml) runs **13 jobs**: a 10-row
`build-matrix` (every feature combination + macOS), `clippy` (`-D warnings`), `doc`
(`RUSTDOCFLAGS=-D warnings`, so a broken intra-doc link fails CI), `fmt`, `msrv` (Rust **1.95**),
`coverage` (`llvm-cov` → Codecov), `sanitizers` (address + thread on nightly), `rocq`,
`formal-correspondence` (the PR gate hosting the `unsafe` gate), `formal-miri`, `formal-io-uring`,
`diagrams` (pinned renderers + the SVG freshness gate + the [doc-math gate](../notation.md#8-what-the-gate-enforces)),
and a cron-only `formal-tlc`. The three orthogonal quality gates — **`unsafe` inventory**,
**doc-math**, and **diagram freshness** — fail the PR independently of the build/clippy/fmt jobs.

## Gaps

- **No fuzzing harness.** There is no `cargo-fuzz` / libFuzzer / AFL target; the natural first targets
  are the protobuf importer and the WAL/arena loaders (see [security/deserialization-safety.md](../security/deserialization-safety.md)).
- **MSan / full Miri are not in the default CI matrix** — they run via `run-sanitizers.sh` and the
  `formal-miri` job respectively, not on every PR, because of runtime cost.
