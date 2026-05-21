# Sanitizer Results Archive

Snapshots of `cargo test` under AddressSanitizer (ASan), MemorySanitizer
(MSan), ThreadSanitizer (TSan), and Miri.

Logs are date-stamped in their filenames. Re-run via:

```bash
scripts/run-sanitizers.sh
```

(See the script for the exact `cargo +nightly` invocations and required
environment variables.)

## Files

| File | Tool | Captured | Notes |
|---|---|---|---|
| `asan-results-2026-01-15.log` | AddressSanitizer | 2026-01-15 | Initial run |
| `asan-results-2026-01-19.log` | AddressSanitizer | 2026-01-19 | After Phase 1 ARTrie fixes |
| `msan-results-2026-01-15.log` | MemorySanitizer | 2026-01-15 | |
| `tsan-results-2026-01-15.log` | ThreadSanitizer | 2026-01-15 | Large file — has the most signal |
| `miri-results-2026-01-15.log` | Miri | 2026-01-15 | UB checker |

## How these are tracked

Filenames are matched by `*_results.log` in `.gitignore`, so the logs
themselves are not committed. The directory and `README.md` are tracked so
contributors know where to put fresh snapshots.

To intentionally commit a snapshot (e.g., a clean run after fixing a TSan
race), use `git add -f docs/sanitizers/<name>.log`.
