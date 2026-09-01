# Binding findings ledger

Scrutiny ledger for the libdictenstein language-binding surface: the
`ldict_*` C ABI (`src/ffi.rs`, `include/libdictenstein.h`), the producer
binding layer (`src/bindings.rs`), the 14 governed facades under
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
defects**: the then-35-function ABI had 35/35/35 symbol agreement across the
model, `src/ffi.rs`, and
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

## Finding LDICT-B2 — producer C ABI lacked direct tests at baseline

| Field | Value |
|-------|-------|
| id | LDICT-B2 |
| date | 2026-08-08 |
| component | `src/ffi.rs` (35 `ldict_*` functions at the finding date; 41 now) |
| class | test-coverage |
| severity | high |
| status | **FIXED** — direct ABI, resource, snapshot, and entry-lease suites ship |

**Baseline evidence.** At the finding date, `grep -c '#\[test\]' src/ffi.rs`
→ **0** and `src/bindings.rs` carried only 2 unit tests. The FFI-adjacent suites
(`tests/query_start_snapshot_correspondence.rs`; the release workflow's
`cargo test --features ffi --lib ffi`) exercise the Rust binding layer and the
core dictionaries, not the C ABI entry points themselves.

**Baseline analysis.** The `ldict_*` surface landed in baseline commit `686f102` with
`catch_unwind` boundaries, thread-local error text, out-param nulling on
failure, and capability-gated `Unsupported`/`DomainMismatch` arms — none of
which was pinned by a test. Untested semantics included: null-pointer sweeps for
every function, `LdictOptionalU64.has_value == 2` rejection
(`InvalidArgument`), `InvalidUtf8` paths on the UTF-8-validating backends,
batch-insert edge counts, `ldict_vocab_get_term` size-query/`LimitExceeded`
truncation protocol, `ldict_dictionary_free(NULL)` no-op, and the
kind/capability constants as observed through the ABI. A regression in any of
these could have shipped silently.

**Fix.** Wave W2 added the producer suite:
`tests/ffi_status_matrix.rs`, `tests/ffi_batch_edges.rs`,
`tests/ffi_resource_paging_proptest.rs`,
`tests/ffi_crud_model_correspondence.rs`, `tests/ffi_snapshot_law.rs`,
`tests/ffi_persistent_checkpoint_reopen.rs`,
`tests/ffi_concurrent_snapshot_stress.rs`, and `src/bindings.rs` test
extensions. The collection expansion adds `tests/ffi_entries_batches.rs` for
the versioned lease laws and `bindings/c/tests/entries.c` for the six public
cursor/reducer functions. Rust suites remain `#![cfg(feature = "ffi")]`.

**Verification.** `cargo nextest run --workspace --all-features` exercises the
Rust suites. The C conformance job compiles and runs
`bindings/c/tests/entries.c` against the built `cdylib`; the binding contract
gate independently proves the modeled/header/exported 42-symbol set is exact.

---

## Finding LDICT-B3 — facade symbol coverage is partial and uneven

| Field | Value |
|-------|-------|
| id | LDICT-B3 |
| date | 2026-08-08 |
| component | facade FFI import layers (`bindings/*`) |
| class | facade-coverage |
| severity | low |
| status | **ACCEPTED** — semantic completeness is gated; duplicate raw calling forms may be omitted |

**Evidence.** Current gate coverage matrix, 2026-08-29 (referenced / 42
modeled `ldict_*` symbols):

| Facade | Coverage | Facade | Coverage |
|--------|----------|--------|----------|
| python | 36 / 42 | ocaml | 30 / 42 |
| jvm | 31 / 42 | ruby | 35 / 42 |
| cpp | 35 / 42 | fortran | 30 / 42 |
| dotnet | 30 / 42 | lua | 29 / 42 |
| go | 35 / 42 | swift | 29 / 42 |
| haskell | 30 / 42 | C | 42 / 42 |
| Julia | 22 / 42 | Raku | 34 / 42 |
| | | clojure | mediated (JVM facade) |
| | | javascript | mediated (umbrella runtime) |

At the first measurement, Fortran lacked a `bind(c)` import for
`ldict_last_error_message`; the dated addendum below records its closure.
Several facades
skip either the aggregate-by-value forms (e.g. `ldict_dictionary_insert_text`
with a struct argument) or their scalar `…_value` twins, depending on which
calling convention their FFI runtime handles; the scalar twins exist precisely
for the dynamic runtimes (ruby/haskell/fortran use them, ctypes/FFM/cgo use
the aggregate forms).

**Analysis.** Raw-symbol equality is neither necessary nor desirable for every
facade. Aggregate-by-value functions and their scalar twins are alternative
calling conventions; importing both adds no host capability. Clojure and the
JavaScript family intentionally consume governed facades rather than importing
C symbols. The gate therefore enforces *referenced-symbol validity* while the
language conformance suites enforce constructor, CRUD, tri-state value,
snapshot, collection, cancellation, and deterministic-lifetime semantics.

**Fix.** The 14 governed guides and their conformance suites now instantiate
the uniform semantic contract. `scripts/check-bindings.py` intentionally keeps
referenced-symbol validation instead of demanding unused duplicate calling
forms. Any future import omission that removes a host capability must be
ledgered as a semantic gap, not hidden by a raw symbol count.

**Verification.** `python3 scripts/check-bindings.py` reports the matrix above
and fails any unknown import, model/header/export mismatch, package-coordinate
drift, or mediated facade that starts importing unmanaged raw symbols.

**Addendum (2026-08-09, W7 uniform sweep).** The specifically-flagged Fortran
gap is closed: `bindings/fortran/src/vinary_tree_libdictenstein.f90` now binds
the whole ABI meta group — `ldict_abi_version`, `ldict_api_revision`, and
`ldict_last_error_message` — surfaced as the public module functions
`abi_version()`, `api_revision()`, and `last_error_message()`. The last of
these marshals the borrowed, NUL-terminated thread-local C string into an owned
Fortran string (length bounded by a `strlen` interop binding, so no read runs
past the terminator), returning an empty string when no message is set.
Fortran gate coverage rose 27 → 30 / 35 then-modeled functions; the four aggregate-by-value CRUD forms
(`insert_text`, `insert_u64`, `get_text`, `get_u64`) and `insert_u64_batch`
remain intentionally uncovered — the scalar `…_value` twins and the text batch
are the calling conventions the Fortran `bind(c)` layer uses. Regression:
`bindings/fortran/test/conformance.f90` (`test_error_message` asserts a
non-empty `last_error_message()` after an INVALID_UTF8 insert).

---

## Finding LDICT-B4 — torn snapshot capture: root and len read from different revisions

| Field | Value |
|-------|-------|
| id | LDICT-B4 |
| date | 2026-08-08 |
| component | `src/bindings.rs` snapshot capture (`DynamicBackend::snapshot`, `SecondaryBackend::snapshot`, `PersistentBackend::snapshot`) |
| class | concurrency / snapshot-coherence |
| severity | medium |
| status | **FIXED for every family** — persistent overlays atomically publish `(root, term_count)` and persistent snapshots again report exact known lengths (`out_known = 1`) |

**Evidence.** Every `snapshot()` arm captured the traversal root and the
advertised term count with two independent atomic loads
(`dictionary.root()` then `dictionary.len()`). For the lock-free DynamicDAWG
the two calls are two separate `version.load()`s
(`src/dynamic_dawg/lockfree.rs`), so a writer CAS between them pairs
revision `$`N`$`'s root with revision `$`N\pm k`$`'s count. Reproduction
(release mode, one writer cycling insert/remove over 64 single-token u64
terms, one capturer comparing each snapshot's walked FINAL-node count with
its `dictionary_len`):

```text
concurrent captures: 2163 / 100000 torn   (~2.2%)
quiescent  captures:    0 /  10000 torn   (control: rules out counter drift)
```

The same two-load shape exists in the SCDAWG arms (two `inner.load()`s on
the ArcSwap'd core; insert-only, same tear mechanics) and in the persistent
arms, where the byte-trie reproduction under insert/remove churn showed
`30 / 30000` torn captures with a clean `0 / 2000` quiescent control.

**Analysis.** A `vt.dictionary.v1` snapshot is contractually ONE immutable
revision; a torn `(root, len)` pair makes `dictionary_len` on the captured
snapshot report a neighbouring revision's count (`out_known == 1`), which
consumers may use for preallocation or completeness checks. The walked
structure itself is never corrupted — the root `Arc` is a coherent revision
— only the advertised length lies. Note the correct oracle for detection is
the walked FINAL-node count: after removals the DAWG legitimately keeps
non-final ghost edges until compaction, so root degree may exceed `len`
without any defect.

**Fix.** Commit-fixed for the in-memory families: new coherent accessors
that read both fields from ONE published revision —
`LockFreeDawg::root_arc_with_term_count` (single `version.load()`) surfaced
as `DynamicDawg::root_with_term_count`,
`DynamicDawgChar::root_with_term_count`,
`DynamicDawgU64::root_with_term_count`, and
`Scdawg::root_with_term_count` / `ScdawgChar::root_with_term_count`
(single `inner.load()`); `src/bindings.rs` snapshot arms now use them.
DoubleArrayTrie arms are immutable (no writer exists) and provably cannot
tear. The persistent arms remain on two overlay loads because a sound fix
needs a coherent `(root, count)` publication in the overlay flip itself:
`overlay_len()` walks a FRESH root load, `PersistentARTrieU64` has both a
counter-based and a walk-based `term_count`, and the vocabulary's
`entry_count` increments AFTER the root CAS — re-read/retry protocols at
the binding layer are provably unsound against post-flip counter updates.
That design belongs to the persistent core. The capture protocol is now
modelled in `formal-verification/tla+/AbiProducerSnapshot.tla` (obligation
#10), whose `Capture` action is ATOMIC — it appends the whole published
version in one step. The in-memory fix realizes exactly that action; the
persistent family must be brought to the same atomic-capture realization
(a coherent `(root, count)` publication in the overlay flip).

**Verification.** Post-fix reproduction run: `0 / 100000` torn concurrent
captures, `0 / 10000` quiescent (same probe). Permanent regression:
`tests/ffi_concurrent_snapshot_stress.rs::snapshot_len_is_never_torn_from_its_root_under_write_churn`
(12,000 captures under insert/remove churn, asserts walked == len on every
capture; INVARIANT-HOOK LDICT-SNAP-1) under
`cargo test --no-default-features --features ffi`.

**Addendum (same day, W2 reconciliation).** The persistent arms no longer
assemble a `(root, count)` pair at all: `PersistentBackend::snapshot` pins
the root only and passes `None` as the captured count, which the interop
`len` callback surfaces as `out_known = 0` ("not cheaply available") — the
contract's honest affordance for exactly this situation. The tear is now
unrepresentable for every family while capture stays $`O(1)`$; the coherent
overlay-flip publication that would restore `out_known = 1` for the
persistent family remains open persistent-core work (binding-side retries
were shown unsound above). Full `--features ffi` suite green after the
change.

**Resolution addendum (2026-08-15).** The open persistent-core enhancement is
now implemented generically in `AtomicNodePtr`: its `ArcSwapOption` publishes
one immutable record containing both the overlay root and exact term count.
Every membership-changing CAS supplies a checked `+1` or `-1`; value-only and
structural-maintenance CASes preserve the count. Prebuilt byte, Unicode, u64,
and vocabulary roots seed the record with one iterative final-node walk, after
which `len()` and snapshot capture are constant-time single-publication reads.
The producer's four persistent snapshot arms now return `out_known = 1`.
`AbiProducerSnapshot.tla` pins live and captured cardinality coherence as
LDICT-SNAP-6/7; the persistent checkpoint/reopen suite asserts exact known
lengths for all four families, and the concurrent churn test remains the
executable no-tear oracle.


## Finding LDICT-B5 — persistent u64 writes swallowed engine errors into `OK`

| Field | Value |
|-------|-------|
| id | LDICT-B5 |
| date | 2026-08-08 |
| component | `src/bindings.rs` `PersistentARTrieBinding::{insert_u64, remove_u64}` |
| class | correctness (error propagation) |
| severity | high |
| status | **FIXED** |

**Evidence.** Surfaced by the W2 C-ABI documentation pass (per-function
status audit): the u64 arms called the infallible wrappers
`PersistentARTrieU64::{insert_sequence_with_value, remove_sequence}`
(`src/persistent_artrie/u64.rs`), which `log::warn!` and return `false` on
engine failure — so a failed durable write surfaced across the ABI as
`LDICT_STATUS_OK` with `out_inserted = 0`, indistinguishable from a clean
idempotent no-op, while the byte and Unicode profiles propagate the same
failures as `IO_ERROR` via `map_err(io_error)`.

**Fix.** The arms now call `try_insert_sequence_with_value` /
`try_remove_sequence` and map engine errors with the same `io_error`
adapter the sibling profiles use; doc comments on both methods pin the
rule (the ABI must report `IO_ERROR`, never a silent no-op `OK`).

**Verification.** Type-level: the swallowing wrappers are no longer
reachable from any ABI path (the `Result` now flows end-to-end);
`tests/ffi_persistent_checkpoint_reopen.rs` (happy path) and
`tests/ffi_crud_model_correspondence.rs` green after the change; the
engine-side failure injection itself is exercised by the persistent
crash-recovery suites that own the WAL fault model.

## Finding LDICT-B6 — `LdictOptionalU64.reserved` accepted nonzero bytes

| Field | Value |
|-------|-------|
| id | LDICT-B6 |
| date | 2026-08-08 |
| component | `src/ffi.rs` `LdictOptionalU64::decode` |
| class | correctness (ABI validation) |
| severity | medium |
| status | **FIXED** |

**Evidence.** `bindings/api.json` pins the struct's `reserved` bytes as
`mustBeZero`, and the interop family law (VT-ABI-5, llev pre-registered
finding F2) requires consumers and producers to enforce reserved-zero so
the bytes stay available for compatible evolution — but revision 4's
decoder checked only `has_value`, silently accepting garbage reserved
bytes a future revision would reinterpret.

**Fix.** `decode` rejects nonzero reserved bytes with
`INVALID_ARGUMENT` ("reserved bytes must be zero") before the `has_value`
check.

**Verification.** `tests/ffi_status_matrix.rs::reserved_bytes_must_be_zero`
— dirty reserved bytes rejected on both the insert path and the
constructor-entry path with the exact message, and an all-zero control
insert still succeeds (23/23 in the matrix suite).


## Finding LDICT-B7 — pre-existing flaky persistent soak (routed, not a binding defect)

| Field | Value |
|-------|-------|
| id | LDICT-B7 |
| date | 2026-08-08 |
| component | `tests/persistent_f4_lock_collapse_soak.rs` via the `debug_assert!` at `src/persistent_artrie/char/persist.rs:399` |
| class | concurrency (pre-existing; observed under load) |
| severity | low (debug-assert only; single occurrence) |
| status | RECORDED — routed to the persistent-core owners |

**Evidence.** During the wave-W2 full-suite runs the soak failed once via
the eviction-registry `debug_assert!` on the eviction-disabled soak
publisher, then passed 3/3 immediate retries and every subsequent full
run. Both the assert and the test predate this wave by two months; no
wave-W2 change touches that code path (run log preserved in the wave's
scratchpad as `final-ffi-run.log`).

**Analysis.** Possibly a latent race in the S5 checkpoint route-split's
interaction with the eviction registry under load — persistent-core
territory with its own TLA/loom program; not reachable from the binding
layer this program owns.

**Fix.** Ledger-routed: persistent-core owners; reproduction guidance =
full-suite parallel load, debug profile.
