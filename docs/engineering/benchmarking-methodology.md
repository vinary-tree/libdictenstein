# Benchmarking methodology

**Navigation**: [← docs](../README.md) · [Testing strategy](testing-strategy.md) · [Feature flags](feature-flags.md)

How performance is measured in libdictenstein, so that a number in a ledger means the same thing
each time it is recorded. The discipline is deliberately **scientific-method**: hypothesize, measure
under controlled conditions, record raw output, analyze, and only then conclude. Notation follows
[`docs/notation.md`](../notation.md).

## Harnesses

There are **22** benchmark harnesses under [`benches/`](../../benches/), every one built on
[Criterion](https://docs.rs/criterion) (`harness = false`, so Criterion provides `main`). They are
feature-gated to match what they exercise — most require `persistent-artrie`; others require
`group-commit`, `parallel-merge`, `io-uring-backend`, `bench-internals`, or
`serialization`+`pathmap-backend`. A bench must be declared as a `[[bench]]` entry in
[`Cargo.toml`](../../Cargo.toml) before `cargo bench` can run it.

For latency distributions (not just means), several persistent harnesses use a custom fixed-iteration
loop — `FIXED_WARMUPS = 3` warmup rounds then a fixed sample count — recording nanosecond latencies
into an [HdrHistogram](https://docs.rs/hdrhistogram) so that p50 / p99 / p999 tails are reported, not
averaged away (`persistent_artrie_benchmarks.rs`, `…_char`, `…_vocab`, `…_u64_native`,
`…_suffix_native`).

## Build configuration

- **`[profile.bench]` `inherits = "release"`** — so benchmarks compile at `opt-level = 3` with LTO
  and one codegen unit, matching a release deployment rather than a debug build.
- **[`.cargo/config.toml`](../../.cargo/config.toml)** sets `-C target-feature=+aes,+sse2` for
  *portability* (PathMap's `gxhash` requires AES + SSE2). For a *local* benchmark on hardware you
  control, it documents opting into `RUSTFLAGS="-C target-cpu=native"` to let the compiler use the
  full instruction set — do this only when the numbers are for your machine, not for a portable
  claim.

## Controlled measurement

Absolute times vary with CPU, allocator, and corpus, so **relative** behavior is what the ledgers
compare, and measurements are taken under controlled conditions. The bench headers document the
intended invocation; the house practice is:

- **Pin to specific cores** so the scheduler cannot migrate the process mid-measurement. The bench
  headers prescribe `taskset` — e.g. `io_backend_benchmarks.rs` documents
  `taskset -c 0-3 cargo bench …`, and `concurrent_read_vs_flush_benchmarks.rs` uses `taskset -c 0-15`
  for its multi-threaded workload.
- **Fix the CPU frequency** at (or near) maximum where the platform allows, so turbo/idle transitions
  do not add variance.
- **Profile with `perf`** alongside timing when a bottleneck is being investigated — e.g.
  `taskset -c 0-3 perf stat -e page-faults,LLC-load-misses,… cargo bench …` for cache/fault behavior,
  or `perf record --call-graph lbr` for low-overhead call-graph attribution. Generate and analyze the
  report rather than eyeballing wall-clock alone.
- **Tee the raw output to a file** and analyze it once, rather than re-running the benchmark to read
  off different numbers — the ledgers cite the captured output.

## Recording results — the scientific ledger

Benchmark results are not scattered in commit messages; they live in dated **ledgers** so a result
is reproducible and its provenance is clear:

- [`docs/benchmarks/`](../benchmarks/) — benchmarking ledgers and the rendered plots (with their
  `.dat`/`.gp` sources) under `docs/benchmarks/artifacts/`.
- [`docs/experiments/`](../experiments/) — per-optimization experiment ledgers (persistence
  enhancements, loading optimization, lock-free flip, native-u64 watermark studies).
- [`docs/io_uring_migration/benchmark_results.md`](../io_uring_migration/benchmark_results.md) — the
  mmap-vs-`io_uring` characterization.

Each ledger follows the same shape: the **hypothesis** under test, the **exact commands and
conditions** (features, core pinning, corpus), the **raw numbers** (or a pointer to the captured
output), the **analysis**, and the **conclusion** — confirmed or refuted. A plot committed under
`docs/benchmarks/artifacts/` cites the ledger and table its `.dat` was extracted from, so no figure
is a free-floating claim.

## A caution on the numbers

The [root README's Performance section](../../README.md#performance) gives *indicative* figures for
one corpus on one machine to convey **relative** backend behavior (a static double-array trie wins
read-heavy in-memory lookups; a DAWG pays a constant factor for runtime mutation). Treat those as
orientation, not a guarantee; run `cargo bench` on your platform for numbers that apply to it. The
one caveat that is *not* platform-relative is the [`group-commit` regression](feature-flags.md) —
~1.5–2$`\times`$ slower on NVMe by design — which is a property of the algorithm, not the hardware.
