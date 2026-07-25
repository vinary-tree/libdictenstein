# D5 Handoff: Upgrade `bincode` 1.3 → 3.0

> **SUPERSEDED (2026-07-25) — do not follow this document.**
>
> Three of its premises are now false:
>
> 1. **The 3.0 target no longer exists as usable software.** bincode was abandoned after a
>    doxxing/harassment incident and the repository was archived on 2025-08-15. Version 3.0.0
>    (published 2025-12-16) is a *tombstone*: per the crate's own docs.rs notice it "contains only
>    this README, as well as a lib.rs containing only a compiler error, to inform potential users of
>    the maintenance status." docs.rs failed to build it. Depending on it breaks the build by design.
> 2. **The serde-adapter attribution below is wrong.** The `bincode::serde` adapter module described
>    as a 3.0 feature is in fact a **2.0** feature.
> 3. **The migration was completed as 1.3 → 2.0, not 1.3 → 3.0**, via the
>    `serialization::bincode_compat` shim pinned to `bincode::config::legacy()` (fixint
>    little-endian) so the wire format stayed byte-identical to 1.x. See
>    `docs/algorithms/serialization.md` for the contract that shim upholds.
>
> `Cargo.toml` now pins `bincode = ">=2.0, <3"` to make walking into the tombstone impossible.
> The live plan is migration to the maintained `bincode-next` fork, gated behind a byte-pinning
> and golden-fixture safety net.
>
> Retained unedited below for historical context.

## Goal

Migrate the `serialization` feature off `bincode` 1.3.x onto a
current-major release.

## Current state

- `Cargo.toml` pins `bincode = { version = "1.3", optional = true }`.
- Current latest on crates.io: **bincode 3.0.0**.
- `cargo search bincode --limit 1` confirmed at session close.

## Why bincode 2.x/3.0 isn't a drop-in upgrade

bincode 1.x uses serde-compatible APIs (`bincode::serialize_into(&mut
writer, &value)`, `bincode::deserialize_from(&mut reader)`). bincode
2.0 dropped serde support out of the box and introduced its own
`Encode` / `Decode` derive macros. bincode 3.0 reintroduced serde via
a `bincode::serde` adapter module but kept the new APIs.

For 3.0 the call-sites change from:

```rust,no_run
bincode::serialize_into(&mut writer, &terms)?;
let terms: Vec<String> = bincode::deserialize_from(&mut reader)?;
```

to:

```rust,no_run
let config = bincode::config::standard();
bincode::serde::encode_into_std_write(&terms, &mut writer, config)?;
let (terms, _len): (Vec<String>, usize) =
    bincode::serde::decode_from_std_read(&mut reader, config)?;
```

## Affected files

- `src/serialization/bincode_impl.rs` — primary site
- `src/serialization/protobuf_impl.rs` — uses bincode for SuffixAutomaton
  source-text encoding
- `src/serialization/compression_impl.rs` — gzip wrapper around bincode
- Every persistent-ARTrie on-disk format that uses bincode for arena
  records / WAL entries

## Step-by-step plan

(Each step a commit.)

### Step 1: Bump Cargo.toml

```toml
bincode = { version = "3.0", features = ["serde"], optional = true }
```

### Step 2: Migrate the serializer impls

For each `BincodeSerializer::*` method, replace the bincode 1.x call
with the bincode 3.x equivalent. Use `bincode::config::standard()` as
the default config (matches bincode 1.x defaults).

### Step 3: Migrate the persistent-ARTrie on-disk encode/decode sites

`grep -rn 'bincode::' src/persistent_artrie src/persistent_artrie_char
src/persistent_artrie_core src/persistent_vocab_artrie`.

Each `serialize_into` / `deserialize_from` call needs migration.

### Step 4: Bump the on-disk format-version constant

bincode 3.0's wire format is **not** byte-compatible with bincode 1.x.
Every persistent dictionary written with the old version becomes
unreadable. Bump the format-version constant in
`src/persistent_artrie_core/disk_manager.rs` and add a migration helper
that reads the old version with a vendored bincode 1.x decoder, then
re-writes with bincode 3.0.

### Step 5: Tests

- `cargo test --all-features --no-fail-fast` — must remain at 2288+.
- New tests in `tests/bincode_migration.rs` that verify a file written
  with the old version can be migrated.

## Expected effort

- 2-4 days, assuming the migration helper for the on-disk format
  works cleanly. If older on-disk indexes have unusual edge cases
  (sparse arenas, partial writes), the helper may need iteration.

## Risks

- **Breaking on-disk format**: a hard requirement of this migration.
  Mitigate via the read-old/write-new helper in Step 4 plus a CI test
  that exercises round-trip from a fixture file.
- **lib stability**: bincode 3.0 was released recently; verify it's
  not a pre-release before merging.
- **Performance**: bincode 3.0's serde adapter has slightly different
  performance characteristics than 1.x's direct API. Benchmark before
  merging.
