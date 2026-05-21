# Changelog

All notable changes to libdictenstein are recorded here.

Date format is ISO-8601 (YYYY-MM-DD).

## Unreleased

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
