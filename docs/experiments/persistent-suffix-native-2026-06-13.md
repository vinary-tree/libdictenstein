# Persistent Suffix Native Graph Benchmarks

Date: 2026-06-13

This ledger records the fixed-sample benchmark run for the native persistent
suffix graph families:

- `PersistentSuffixAutomaton` / `PersistentSuffixAutomatonChar`
- `PersistentSuffixTree` / `PersistentSuffixTreeChar`
- `PersistentScdawg` / `PersistentScdawgChar`

The run compares native graph storage against encoded suffix ARTrie controls.
It was collected after the host load dropped; interval `vmstat` rows captured
around the run showed 87-93% idle CPU and 0% I/O wait.

## Command

```bash
PERSISTENT_SUFFIX_FIXED_SAMPLES=1 cargo bench --features persistent-artrie --bench persistent_suffix_native_benchmarks
```

Fixed-sample mode emits 51 measured samples after 3 warmups for each
`metric x arm` pair. Scratch data uses `target/bench-scratch`, not `/tmp`.

## pgmcp Records

All 36 raw `metric x arm` sample vectors are stored in pgmcp data table
`libdictenstein.persistent_suffix_native_benchmark_sample_sets` under run id
`persistent_suffix_native_fixed_2026_06_13_0638z_63c0fa4d`.

The pre-registered parallel read/write hypotheses were decided by pgmcp using
Welch's t-test with `p < 0.05` and minimum Cohen's `d = 0.5`.

| Metric | Experiment | Hypothesis | Verdict |
|---|---:|---:|---|
| `suffix_byte_parallel_read_write_ns_per_read` | 56 | 56 | accepted |
| `suffix_char_parallel_read_write_ns_per_read` | 57 | 57 | accepted |
| `suffix_tree_byte_parallel_read_write_ns_per_read` | 58 | 58 | accepted |
| `suffix_tree_char_parallel_read_write_ns_per_read` | 59 | 59 | accepted |
| `scdawg_byte_parallel_read_write_ns_per_read` | 60 | 60 | accepted |
| `scdawg_char_parallel_read_write_ns_per_read` | 61 | 61 | accepted |

## Parallel Read/Write Results

Four reader threads performed 2,000 reads each while one writer inserted into
the same persistent index.

| Type | Control mean | Native mean | Reduction | Welch `p` | Cohen's `d` |
|---|---:|---:|---:|---:|---:|
| Suffix automaton byte | 11,751.65 ns/read | 1,498.33 ns/read | 87.25% | 3.21e-60 | -7.37 |
| Suffix automaton char | 20,043.34 ns/read | 2,288.87 ns/read | 88.58% | 2.01e-66 | -10.30 |
| Suffix tree byte | 22,142.33 ns/read | 1,258.74 ns/read | 94.32% | 4.16e-48 | -11.58 |
| Suffix tree char | 35,903.02 ns/read | 1,223.74 ns/read | 96.59% | 9.52e-65 | -25.04 |
| SCDAWG byte | 20,307.08 ns/read | 213.95 ns/read | 98.95% | 3.76e-47 | -11.10 |
| SCDAWG char | 35,220.62 ns/read | 251.65 ns/read | 99.29% | 4.45e-63 | -23.19 |

## Lookup And Location Results

These are fixed-sample means from the same run. They were recorded as raw
sample vectors in pgmcp but were not the pre-registered Welch hypotheses for
this batch.

| Type | Control mean | Native mean | Reduction |
|---|---:|---:|---:|
| Suffix automaton byte match positions | 36,693.71 ns/query | 4,624.77 ns/query | 87.40% |
| Suffix automaton char match positions | 66,199.02 ns/query | 4,696.49 ns/query | 92.91% |
| Suffix tree byte locations | 64,079.53 ns/query | 9,418.33 ns/query | 85.30% |
| Suffix tree char locations | 125,720.63 ns/query | 10,184.02 ns/query | 91.90% |
| SCDAWG byte locations | 63,184.32 ns/query | 93.04 ns/query | 99.85% |
| SCDAWG char locations | 125,604.41 ns/query | 150.20 ns/query | 99.88% |

## Checkpoint Disk Size Results

Checkpoint disk-byte metrics were also stored as raw pgmcp vectors.

| Type | Control mean | Native mean | Reduction |
|---|---:|---:|---:|
| Suffix automaton byte checkpoint | 745,956.8 bytes | 5,381.0 bytes | 99.28% |
| Suffix automaton char checkpoint | 1,366,267.0 bytes | 8,928.2 bytes | 99.35% |
| Suffix tree byte checkpoint | 745,949.2 bytes | 5,381.0 bytes | 99.28% |
| Suffix tree char checkpoint | 1,368,007.2 bytes | 9,027.9 bytes | 99.34% |
| SCDAWG byte checkpoint | 745,956.5 bytes | 5,117.0 bytes | 99.31% |
| SCDAWG char checkpoint | 1,369,283.6 bytes | 8,797.5 bytes | 99.36% |

## Notes

- Native graph treatment samples include occasional high-latency outliers in
  the suffix automaton parallel workloads, but every registered comparison
  still had a large effect and passed the pre-registered Welch criterion.
- pgmcp robustness checks reported Cliff's delta `-1.0` for all six registered
  parallel hypotheses. Several treatment distributions departed from normality;
  the robustness check's Mann-Whitney result agreed with the Welch decision.
