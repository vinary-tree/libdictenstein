# Feature flags

**Navigation**: [← docs](../README.md) · [Testing strategy](testing-strategy.md) · [Benchmarking methodology](benchmarking-methodology.md)

The definitive reference for libdictenstein's Cargo features: what each gates, what it pulls in, and
its caveats. The list here is reconciled against `[features]` in [`Cargo.toml`](../../Cargo.toml)
(verified with `cargo metadata`). Notation follows [`docs/notation.md`](../notation.md).

## The features

| Feature | Default? | Gates | Pulls in | Notes |
|---------|:---:|-------|----------|-------|
| `parking_lot` | ✅ (via `default`) | `parking_lot::RwLock` for the dynamic backends' internal bookkeeping | `parking_lot` | faster than `std`; the default. Not a reader-visible global lock — see [volatile-concurrency](../design/volatile-concurrency.md). |
| `pathmap-backend` | | the `PathMap*` backends | `pathmap` (`>=0.2.2, <0.4`) | structural-sharing trie; needs AES + SSE2 (its hash) |
| `serialization` | | `serde` + `bincode` + JSON (de)serialization | `serde`, `bincode`, `serde_json` | see [deserialization-safety](../security/deserialization-safety.md) |
| `compression` | | gzip wrapper over the serialized form | `flate2` | decompression-bomb caveat — [deserialization-safety](../security/deserialization-safety.md) |
| `protobuf` | | Protobuf (de)serialization | `prost`, `prost-build`, `protoc-bin-vendored` | richest parse surface; recursion + preallocation edges documented in [deserialization-safety](../security/deserialization-safety.md). Needs no host `protoc` — see below. |
| `persistent-artrie` | | the disk-backed ARTrie, vocabulary, and native-suffix-graph families | `memmap2`, `sysinfo`, `xxhash-rust`, `lru`, `dashmap`, `crossbeam-channel`, `rustix`, **+ `serialization` transitively** | `mmap` + WAL + CX/native snapshots. Enables `serialization` because the persistent modules use `crate::serialization::bincode_compat` unconditionally. |
| `group-commit` | | batched WAL group commit | ⟶ `persistent-artrie`, `crossbeam-channel` | ⚠️ **EXPERIMENTAL** — measured ~1.5–2$`\times`$ throughput *regression* on NVMe vs per-record sync (recorded 2026-01-15). Intended only for slow storage (HDD / remote block stores); do not enable on NVMe without re-benchmarking. See [group-commit.md](../persistence/group-commit.md). |
| `parallel-merge` | | multi-core merge | ⟶ `persistent-artrie`, `rayon` | |
| `io-uring-backend` | | `io_uring` + `O_DIRECT` block storage | ⟶ `persistent-artrie`, `io-uring`, `libc` | Linux kernel $`\ge`$ 5.1 |
| `bench-internals` | | exposes internal APIs to benchmarks | ⟶ `io-uring-backend` | not for application use |

`⟶` marks a feature that transitively enables another (so, e.g., turning on `group-commit`
necessarily turns on `persistent-artrie` and hence `serialization`).

## Common combinations

- **In-memory only** (the default) — no features needed beyond `default`; the volatile backends are
  always available.
- **In-memory + PathMap** — `pathmap-backend`.
- **Durable** — `persistent-artrie` (brings serialization along).
- **Durable, fast async I/O** — `persistent-artrie` + `io-uring-backend` (Linux ≥ 5.1).
- **Save/load without durability** — `serialization` (+ `compression` and/or `protobuf` for
  alternative wire formats).

## `protobuf` and the `protoc` binary

`prost-build` does not vendor a protobuf compiler; it shells out to a `protoc` executable.
Left to itself it searches `PATH`, so `cargo build --features protobuf` would succeed or fail
depending on whether the machine happens to have `protobuf-compiler` installed — including on
CI, where it passed only because the runner image shipped one.

The `protobuf` feature therefore also pulls `protoc-bin-vendored`, which ships prebuilt
binaries, and [`build.rs`](../../build.rs) resolves the compiler in this order:

1. **`PROTOC` environment variable**, if set. This is the escape hatch for targets
   `protoc-bin-vendored` has no binary for, and for anyone who needs a specific compiler
   version. `build.rs` re-runs when it changes.
2. **The vendored binary**, passed to `prost_build::Config::protoc_executable` as an absolute
   path — so `PATH` is not consulted at all.
3. **`PATH`**, only if the vendored crate has no binary for the target. That case emits a
   `cargo:warning` rather than failing, so a host with its own `protoc` still builds.

```bash
cargo build --features protobuf                 # hermetic; uses the vendored protoc
PROTOC=/usr/bin/protoc cargo build --features protobuf   # override with a specific compiler
```

## Not a feature: the `Lattice` / WFST integration

The value-merge `Lattice` trait used by the [zipper set-algebra](../algorithms/zippers.md) is backed
by the [`llattice`](../../Cargo.toml) path dependency, which is **always on** — it is a plain
dependency, not gated by any Cargo feature. An earlier `lling-llang` feature gated this integration;
it was **retired** when the dependency became unconditional (the correspondence harness
[`scripts/verify-formal-correspondence.sh`](../../scripts/verify-formal-correspondence.sh) still
guards for the feature's presence and skips gracefully now that it is absent). If you see
`lling-llang` referenced anywhere as a feature, it is stale.

## MSRV and edition

Minimum supported Rust version is **1.95**; edition **2021**. The `msrv` CI job builds
`--all-features` on a pinned 1.95 toolchain, so a feature that raised the MSRV would fail CI. `docs.rs`
builds with `all-features`.

The floor is set by dependencies, not by this crate's own language use. Under `--all-features` the
binding constraints are `sysinfo` 0.39 (1.95) and `pathmap` 0.2.2 (1.88); with only `serialization`
or `persistent-artrie` it is `bincode` 2.0 / `lru` 0.18 (1.85). Raising a dependency floor is
therefore the usual reason this number moves.
