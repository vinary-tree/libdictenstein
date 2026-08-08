# Binding findings ledger

Scrutiny ledger for the libdictenstein language-binding surface: the
`ldict_*` C ABI (`src/ffi.rs`, `include/libdictenstein.h`), the producer
binding layer (`src/bindings.rs`), the 13 language facades under
[`bindings/`](../../bindings/), and their packaging/release metadata.

Every entry follows the uniform family schema:

```text
Finding LDICT-B<N> | date | component | class | severity | evidence | analysis | fix (commit or ledger-only) | verification | status
```

Machine context: the binding model is [`bindings/api.json`](../../bindings/api.json)
and the enforcing gate is [`scripts/check-bindings.py`](../../scripts/check-bindings.py)
(CI job `binding-contract`). Version/pin inconsistencies are recorded here and
never "fixed" by release actions — releases are fully out of scope for this
effort (family plan decision #4).

**Gate bring-up note (2026-08-08).** The first end-to-end run of
`check-bindings.py` against baseline commit `686f102` found **zero parity
defects**: 35/35/35 symbol agreement across model, `src/ffi.rs`, and
`include/libdictenstein.h`; all `LdictStatus`/kind/capability/unit-domain
constants aligned; all 12 registry coordinates and every facade version at
0.2.1; identity guard clean; `include/vinary_tree_interop.h` byte-identical to
the canonical sibling header; OCaml header copies identical to the in-repo
originals. Consequently this ledger seeds with no fixed-defect entries — the
open findings below are pins, coverage gaps, and scheduled work.

---

## Finding LDICT-B1 — every liblevenshtein-rust pin targets the unreleased v0.10.0

| Field | Value |
|-------|-------|
| id | LDICT-B1 |
| date | 2026-08-08 |
| component | `.github/workflows/release-bindings.yml` + facade dependency metadata |
| class | version-pin |
| severity | medium |
| status | **OPEN** (ledger-only; blocked on the liblevenshtein-rust v0.10.0 release) |

**Evidence.**
`release-bindings.yml` clones the sibling at a pinned tag in five jobs
(lines 28, 85, 196, 236, 310):

```text
git clone --depth 1 --branch v0.10.0 https://github.com/vinary-tree/liblevenshtein-rust.git
```

but `git -C ../liblevenshtein-rust tag` lists `v0.2.0` … `v0.9.0`, `v0.9.1` —
**no `v0.10.0` exists**. The same future version is pinned throughout the
facade metadata: `bindings/go/go.mod`
(`…/liblevenshtein-rust/bindings/go v0.10.0`), root `Package.swift`
(`from: "0.10.0"`), `bindings/jvm/build.gradle.kts`
(`testImplementation("io.vinarytree:liblevenshtein:0.10.0")`),
`bindings/clojure/project.clj` (`:test` profile
`[io.vinarytree/liblevenshtein "0.10.0"]`), and
`bindings/javascript/package.json`
(`"@vinary-tree/vinary-tree": "0.10.0"` umbrella runtime). By contrast the
llattice pin is sound: `release-bindings.yml` clones `llattice` at `v0.1.0`
and `git -C ../llattice tag` confirms that tag exists.

**Analysis.** The binding stack pre-pins the sibling's *next* release, matching
liblevenshtein-rust's own in-tree `bindings/api.json`
`packageVersion: 0.10.0`. All references are mutually coherent — the gate's
`sibling-pins` check enforces that they stay so — but none of the pinned
artifacts (git tag, crates.io release, npm umbrella, Maven artifact, Go module
version) exist yet, so `release-bindings.yml` cannot succeed until
liblevenshtein-rust v0.10.0 is tagged and published.

**Fix.** Ledger-only, per family plan decision #4 (releases fully out of
scope; version/pin inconsistencies are recorded, never acted on here).

**Verification.** `python3 scripts/check-bindings.py` (`sibling-pins` check)
passes: every liblevenshtein-rust reference pins 0.10.0 and every llattice
reference pins 0.1.0. Tag existence is to be re-verified at release time.

---

## Finding LDICT-B2 — producer C ABI has zero direct tests

| Field | Value |
|-------|-------|
| id | LDICT-B2 |
| date | 2026-08-08 |
| component | `src/ffi.rs` (35 `ldict_*` extern "C" functions) |
| class | test-coverage |
| severity | high |
| status | **OPEN** (accepted; scheduled for wave W2) |

**Evidence.** `grep -c '#\[test\]' src/ffi.rs` → **0**. `src/bindings.rs`
carries only 2 unit tests. The FFI-adjacent suites
(`tests/query_start_snapshot_correspondence.rs`; the release workflow's
`cargo test --features ffi --lib ffi`) exercise the Rust binding layer and the
core dictionaries, not the C ABI entry points themselves.

**Analysis.** The `ldict_*` surface landed in baseline commit `686f102` with
`catch_unwind` boundaries, thread-local error text, out-param nulling on
failure, and capability-gated `Unsupported`/`DomainMismatch` arms — none of
which is pinned by a test. Untested semantics include: null-pointer sweeps for
every function, `LdictOptionalU64.has_value == 2` rejection
(`InvalidArgument`), `InvalidUtf8` paths on the UTF-8-validating backends,
batch-insert edge counts, `ldict_vocab_get_term` size-query/`LimitExceeded`
truncation protocol, `ldict_dictionary_free(NULL)` no-op, and the
kind/capability constants as observed through the ABI. A regression in any of
these would ship silently today.

**Fix.** Scheduled — wave W2 (plan Phase T1) adds the producer suite:
`tests/ffi_status_matrix.rs`, `tests/ffi_batch_edges.rs`,
`tests/ffi_resource_paging_proptest.rs`,
`tests/ffi_crud_model_correspondence.rs`, `tests/ffi_snapshot_law.rs`,
`tests/ffi_persistent_checkpoint_reopen.rs`,
`tests/ffi_concurrent_snapshot_stress.rs`, plus `src/bindings.rs` test
extensions — all `#![cfg(feature = "ffi")]`.

**Verification.** Pending W2 (suite green under
`cargo test --no-default-features --features ffi`).

---

## Finding LDICT-B3 — facade symbol coverage is partial and uneven

| Field | Value |
|-------|-------|
| id | LDICT-B3 |
| date | 2026-08-08 |
| component | facade FFI import layers (`bindings/*`) |
| class | facade-coverage |
| severity | low |
| status | **OPEN** (accepted; completeness enforcement scheduled for W2/W7) |

**Evidence.** Gate coverage matrix, first run 2026-08-08 (referenced /
modeled `ldict_*` symbols):

| Facade | Coverage | Facade | Coverage |
|--------|----------|--------|----------|
| python | 30 / 35 | ocaml | 28 / 35 |
| jvm | 29 / 35 | ruby | 28 / 35 |
| cpp | 28 / 35 | fortran | 27 / 35 |
| dotnet | 28 / 35 | lua | 27 / 35 |
| go | 28 / 35 | swift | 20 / 35 |
| haskell | 28 / 35 | clojure | mediated (JVM facade) |
| | | javascript | mediated (umbrella runtime) |

Notably, `bindings/fortran/src/vinary_tree_libdictenstein.f90` declares no
`bind(c)` import for `ldict_last_error_message`, so Fortran callers see only
numeric status codes and never the thread-local error text. Several facades
skip either the aggregate-by-value forms (e.g. `ldict_dictionary_insert_text`
with a struct argument) or their scalar `…_value` twins, depending on which
calling convention their FFI runtime handles; the scalar twins exist precisely
for the dynamic runtimes (ruby/haskell/fortran use them, ctypes/FFM/cgo use
the aggregate forms).

**Analysis.** This is by design at W0: the gate enforces *referenced-symbol
validity* (every symbol a facade names must exist in the model — asymmetry,
coordinate, and version mismatches fail) but not *completeness*. The uniform
completeness matrix (35/35 per facade, or an explicit ledgered waiver per
symbol) belongs to the language sweep, where each facade also gains the
C1–C10 contract suite.

**Fix.** Scheduled — wave W2 (13 language suites, per-language READMEs) and
wave W7 (uniform contract instantiation; completeness enforcement switched on
in `scripts/check-bindings.py` last, per the family plan's scrutiny protocol).

**Verification.** Pending W7 (gate flips from referenced-symbol mode to
completeness mode and stays green).
