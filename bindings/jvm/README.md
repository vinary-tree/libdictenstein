# Vinary Tree libdictenstein for the JVM

A Java Foreign Function &amp; Memory (Panama) facade over libdictenstein's stable
`ldict_*` C ABI. The Maven coordinate is `io.vinarytree:libdictenstein`; the
package is `io.vinarytree.libdictenstein`. It exposes DynamicDAWG CRUD and batch
insertion, immutable DoubleArrayTrie construction, SCDAWG substring search,
persistent ARTrie CRUD/checkpoint/reopen, and persistent vocabulary reverse
lookup. The Clojure facade (`io.vinarytree/libdictenstein-clojure`) is a thin
layer over these classes.

## Requirements and native library

The facade uses `java.lang.foreign`, so run on a recent JDK with native access
enabled (`--enable-native-access=ALL-UNNAMED`). The published jar bundles the
native library under `META-INF/native/<platform>/`; `NativeLibraryLoader`
extracts and loads it, falling back to `System.loadLibrary("libdictenstein")`.
For a source checkout, build the library and put it on `java.library.path`:

```sh
cargo build --release --no-default-features --features ffi
# then run with: -Djava.library.path=target/release --enable-native-access=ALL-UNNAMED
```

## Quickstart

```java
import io.vinarytree.libdictenstein.*;
import java.util.Map;
import java.util.OptionalLong;

try (var dictionary = new DynamicDawg()) {          // Unicode-scalar by default
    dictionary.putAllStrings(Map.of(
            "cat", OptionalLong.of(1),
            "cot", OptionalLong.of(2),
            "cut", OptionalLong.empty()));           // valueless term
    Dictionary.Lookup hit = dictionary.get("cot");
    assert hit.present() && hit.value().equals(OptionalLong.of(2));
}

try (var suffixes = new Scdawg()) {
    suffixes.put("cat", OptionalLong.empty());
    suffixes.put("cot", OptionalLong.empty());
    assert suffixes.containsSubstring("ot");
    assert suffixes.frequency("t") == 2;
}
```

`Dictionary` implements `AutoCloseable`; `close()` (via try-with-resources)
frees the handle once, and a `Cleaner` reclaims any handle a caller forgets.

## Backends and capabilities

| Class | Kind | Unit domains | Capabilities |
|-------|------|--------------|--------------|
| `DynamicDawg` | 1 | byte, unicode-scalar, u64 | read, insert, remove, clear, compact |
| `DoubleArrayTrie` | 2 | byte, unicode-scalar | read (immutable) |
| `Scdawg` | 3 | byte, unicode-scalar | read, insert, substring |
| `PersistentARTrie` | 4 | byte, unicode-scalar, u64 | read, insert, remove, checkpoint |
| `PersistentVocabulary` | 5 | unicode-scalar | read, insert, checkpoint |

`kind()` and `capabilities()` report the runtime backend id and `LDICT_CAP_*`
bitset; `domain()` returns the `UnitDomain` fixed at construction.

## Text domains and values

`String` terms are UTF-8 encoded; `byte[]` terms are passed verbatim (byte
domain); `long[]` terms drive the u64 domain. The Unicode-scalar backends
validate UTF-8. Values are `OptionalLong` interpreted as unsigned across the
whole 64-bit range; `OptionalLong.empty()` and `OptionalLong.of(0)` are
distinct. `Lookup` is a record of `present()` and `value()`. Batch inserts are
`putAllStrings` / `putAllBytes` / `putAllU64`.

## Error handling

Non-OK statuses throw `NativeException`, whose `status()` is the numeric
`LdictStatus` and whose message is the thread-local
`ldict_last_error_message()`. Backend-unsupported operations surface as
`UNSUPPORTED`; wrong-domain terms surface as `DOMAIN_MISMATCH` (9).

## Retained resource handoff

`resourceSegment()` exposes the shared `VtResource`, so a liblevenshtein
transducer retains the dictionary in constant time and keeps its query-start
revision valid after `close`.

## Testing

```sh
./gradlew test
```

from `bindings/jvm`, with the native library discoverable. See
[`BackendIntegrationTest.java`](src/test/java/io/vinarytree/libdictenstein/BackendIntegrationTest.java).
