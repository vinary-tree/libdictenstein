# Group commit (experimental)

**Navigation**: [↑ Persistence architecture](README.md) · [Durability & recovery](durability-and-recovery.md) · [WAL format](wal-format.md)

## Status

**EXPERIMENTAL.** The `group-commit` feature gates the `GroupCommitCoordinator`
WAL-batching path (`DurabilityPolicy::GroupCommit`). Per the benchmark below it produces a
measurable throughput **regression on local NVMe**, so it is disabled by default — keep it
off on fast storage. It is retained for slow-storage scenarios (HDDs, network block stores)
and for future revision under a different coordinator design.

## What group commit is

Under `DurabilityPolicy::Immediate` (the default), every acknowledged write `fsync`s its own
WAL record before returning — one `fsync` per write. **Group commit** instead lets many
concurrent writers *share* one `fsync`: a coordinator collects their records into a batch,
issues a single `fsync`, and then acknowledges all of them. When $`k`$ writers commit together
the amortized cost is one `fsync` per $`k`$ writes rather than $`k`$, which is a large win *when
`fsync` is expensive relative to the coordination overhead*.

The coordinator (`GroupCommitCoordinator`) offers `low_latency`, `high_throughput`, and
`nvme_optimized` config presets and reports a `batching_efficiency` statistic. It preserves
the Order-A contract: an acknowledged write is still durable before it is visible — group
commit only changes *when* the shared `fsync` happens, never the "acknowledged $`\implies`$
durable" invariant.

## Why per-record sync wins on NVMe

`fsync` on a local NVMe queue completes in low microseconds. The coordinator adds, per record:

- a cross-thread `crossbeam-channel` hop (~hundreds of ns at the producer, plus coordinator wake-up latency);
- coordinator-side bookkeeping (LSN assignment, batch-close condition, ack fan-out);
- an extra `Arc<Mutex<…>>` acquire/release for the in-flight batch.

Let $`t_{\text{sync}}`$ be the `fsync` time and $`t_{\text{coord}}`$ the added per-record
coordination cost. Group commit helps only when $`t_{\text{coord}} < t_{\text{sync}}`$ (the
saved sync time must exceed the overhead it costs). On NVMe $`t_{\text{sync}}`$ is already a
few microseconds, so $`t_{\text{coord}} \ge t_{\text{sync}}`$ and the "batched" path is slower
in absolute terms.

## Where it still wins (or is expected to)

- Spinning disks and remote block storage, where $`t_{\text{sync}}`$ rounds to milliseconds.
- Cloud volumes with bursty IOPS quotas and large `fsync` tail latency.

These workloads are not in the CI matrix (the benches need a real disk backend and stable
timing), so the feature stays experimental — opt in by hand *after measuring on your storage*.

## How to reproduce

```bash
cargo bench --bench group_commit_benchmarks --features group-commit
```

Benchmark file: `benches/group_commit_benchmarks.rs` — single- and multi-thread WAL
throughput / latency with and without batching.

## Correctness

Even though it is experimental for *performance*, the group-commit path's *correctness* is
modelled: `DurabilityFrontier.tla` proves the synced-LSN frontier advances with **no early
acknowledgement** (a writer is never acked before its record is in a completed `fsync`), and
`AsyncWalGroupCommit.tla` proves ordered group-commit FIFO and returned-LSN correspondence.
See [formal-verification-map.md](formal-verification-map.md).

## Source pointers

- Coordinator: `src/persistent_artrie/core/group_commit.rs` (`GroupCommitCoordinator`, `GroupCommitConfig`, `GroupCommitStats`).
- Async writer path: `src/persistent_artrie/core/wal/async_writer.rs` (group-commit-aware).
- Wiring: `#[cfg(feature = "group-commit")]` sites across `src/persistent_artrie/` (byte), `src/persistent_artrie/char/`, and `src/persistent_artrie/core/`.
- Policy: `DurabilityPolicy::GroupCommit` in [`durability-and-recovery.md`](durability-and-recovery.md#durability-policies).

## History

An earlier `Cargo.toml` comment read "REJECTED: causes regression on NVMe". That was meant to
flag the NVMe slowdown, not to declare the feature dead: the code ships and is maintained; the
flag merely gates compilation. The comment now reads "EXPERIMENTAL" to state the status honestly.
