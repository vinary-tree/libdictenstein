# D5 Handoff: Upgrade `bincode` 1.3 → 3.0

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
