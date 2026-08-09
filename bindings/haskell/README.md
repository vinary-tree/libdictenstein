# Vinary Tree libdictenstein for Haskell

A `foreign import ccall` facade over libdictenstein's stable `ldict_*` C ABI,
with a thin `cbits/libdictenstein_hs.c` shim for the aggregate-return calls. The
Hackage package is `vinary-tree-libdictenstein`; the module is
`VinaryTree.Libdictenstein`. It exposes DynamicDAWG CRUD and batch insertion,
immutable DoubleArrayTrie construction, SCDAWG substring search, persistent
ARTrie CRUD/checkpoint/reopen, and persistent vocabulary reverse lookup.

## Native library

The facade links the shared library `libdictenstein`. Build it and expose it to
the linker and loader:

```sh
cargo build --release --no-default-features --features ffi
export LIBRARY_PATH="$PWD/target/release:$LIBRARY_PATH"
export LD_LIBRARY_PATH="$PWD/target/release:$LD_LIBRARY_PATH"
cabal build --extra-lib-dirs="$PWD/target/release"
```

## Quickstart

```haskell
{-# LANGUAGE OverloadedStrings #-}

import VinaryTree.Libdictenstein

main :: IO ()
main = do
  d <- dynamicDawg UnicodeScalar
  _ <- putText d "cat" (Just 1)
  _ <- putText d "cot" (Just 2)
  _ <- putText d "cut" Nothing          -- valueless term
  n <- dictionaryLength d
  print n                                -- 3
  hit <- getText d "cot"
  print (found hit, mappedValue hit)     -- (True,Just 2)

  s <- scdawg UnicodeScalar
  _ <- putText s "cat" Nothing
  _ <- putText s "cot" Nothing
  present <- containsSubstring s "ot"    -- ByteString pattern
  freq <- substringFrequency s "t"
  print (present, freq)                  -- (True,2)

  close d
  close s
```

## Backends and capabilities

| Constructor | Kind | Unit domains | Capabilities |
|-------------|------|--------------|--------------|
| `dynamicDawg` | 1 | Byte, UnicodeScalar, U64 | read, insert, remove, clear, compact |
| `doubleArrayTrie` | 2 | Byte, UnicodeScalar | read (immutable) |
| `scdawg` | 3 | Byte, UnicodeScalar | read, insert, substring |
| `createPersistentARTrie` / `openPersistentARTrie` | 4 | Byte, UnicodeScalar, U64 | read, insert, remove, checkpoint |
| `createPersistentVocabulary` / `openPersistentVocabulary` | 5 | UnicodeScalar | read, insert, checkpoint |

`dictionaryKind` and `capabilities` report the runtime backend id and
`LDICT_CAP_*` bitset.

## Values and domains

Both `Text` (`putText`, `getText`, `containsText`, `removeText`) and
`ByteString` (`putBytes`, `getBytes`, `containsBytes`, `removeBytes`,
`putManyBytes`) APIs are provided; the Unicode-scalar backends validate UTF-8.
The u64 API (`putU64`, `containsU64`, `getU64`, `removeU64`) takes `[Word64]`.
Values are `Maybe Word64` over the full `0 .. maxBound` range; `Nothing` and
`Just 0` are distinct. `Lookup` carries `found :: Bool` and
`mappedValue :: Maybe Word64`.

## Error handling

Non-OK statuses throw an `IOError` carrying the thread-local
`ldict_last_error_message()`. Backend-unsupported operations surface the
`UNSUPPORTED` status; wrong-domain terms surface `DOMAIN_MISMATCH` (9).

## Retained resource handoff

`resource` yields the shared retained handle, so a liblevenshtein transducer
retains the dictionary in constant time and keeps its query-start revision valid
after `close`. A `ForeignPtr` finalizer frees any handle a caller forgets.
