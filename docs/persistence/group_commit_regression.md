# Group-commit regression on NVMe

## Status

**EXPERIMENTAL**. The `group-commit` feature gates the `GroupCommitCoordinator`
WAL-batching path. Per the benchmark below, it produces a measurable throughput
**regression** on local NVMe — keep the feature disabled on fast storage. It is
retained for slower-storage scenarios (HDDs, network block stores) and for
future revision under a different coordinator design.

## How to reproduce

```bash
cargo bench --bench group_commit_benchmarks --features group-commit
```

Benchmark file: `benches/group_commit_benchmarks.rs`. Measures single-thread and
multi-thread WAL throughput / latency with and without batching.

## Why per-record sync wins on NVMe

`fsync` on a local NVMe queue completes in low microseconds. The
`GroupCommitCoordinator` adds:

- A cross-thread `crossbeam-channel` hop (~hundreds of ns at the producer, plus
  wake-up latency on the coordinator).
- Coordinator-side bookkeeping per record (LSN assignment, batch close
  condition, ack fan-out).
- An additional `Arc<Mutex<…>>` acquire/release for the in-flight batch.

The aggregate per-record cost exceeds the saved `fsync` time once `fsync`
itself is fast, so the "batched" path is slower in absolute terms.

## Where it still wins (or expected to)

- Spinning disks and remote block storage, where `fsync` rounds to milliseconds.
- Cloud volumes with bursty IOPS quotas and large `fsync` tail latency.

These workloads aren't part of the CI matrix (the benches require a real disk
backend and stable timing), so the feature stays "experimental" — opt in by
hand after measuring.

## Source pointers

- Coordinator: `src/persistent_artrie_core/group_commit.rs`
- Wiring: 8 `#[cfg(feature = "group-commit")]` sites across
  `persistent_artrie/`, `persistent_artrie_char/`, `persistent_artrie_core/`.
- Async writer path: `src/persistent_artrie_core/wal/async_writer.rs`
  (group-commit-aware).

## What "REJECTED" used to mean in the prior Cargo.toml comment

The earlier comment "REJECTED: causes regression on NVMe" was meant to flag the
NVMe slowdown, not to declare the feature dead. The actual code is shipped and
maintained; the feature flag merely gates compilation. The new Cargo.toml
comment uses "EXPERIMENTAL" to be more honest about the status.
