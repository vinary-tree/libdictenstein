# Compact Binary Serialization

[← Algorithms](README.md) · [Documentation index](../README.md)

**Status**: implemented
**Updated**: 2026-08-01

Libdictenstein persists dictionaries in two binary formats: bincode for efficient
Rust-native storage and Protocol Buffers for schema-based interchange. Gzip may wrap either
format. JSON, TOML, newline-delimited text, and native-path text are intentionally absent from
the persistence API because production dictionaries can be very large.

Text word lists remain useful as *construction input*. They are not a serialized dictionary:
loading one must parse all terms and rebuild a backend.

<img src="../diagrams/serializer-selector.svg" alt="Choose bincode for Rust-native binary persistence or Protocol Buffers for portable binary interchange; gzip may wrap either stream." width="70%"/>

## Feature matrix

| Capability | Feature | Dependencies added | Contract |
|---|---|---|---|
| Bincode | `serialization` | Serde, `bincode-next` | Fixed-int little-endian legacy layout |
| Protocol Buffers | `protobuf` | `prost`, schema compiler | Numbered binary schema |
| Gzip wrapper | `compression` | `flate2` | Compression around a supported binary serializer |

`serialization` does not enable `serde_json` or any other text-format dependency. The crate
uses the maintained `bincode-next` fork under the dependency name `bincode`; the original
crate is unmaintained. The `bincode_compat` module pins the legacy fixed-int,
little-endian layout with byte-level tests.

## Terms-only bincode

`DictionarySerializer` is the common terms-only interface:

```rust
pub trait DictionarySerializer {
    fn serialize<D, W>(dictionary: &D, writer: W) -> Result<(), SerializationError>;
    fn deserialize<D, R>(reader: R) -> Result<D, SerializationError>;
}
```

The concrete bounds require a byte-oriented `Dictionary` for serialization and a
`DictionaryFromTerms` backend for reconstruction.

```rust
use libdictenstein::double_array_trie::DoubleArrayTrie;
use libdictenstein::serialization::{BincodeSerializer, DictionarySerializer};

let dictionary = DoubleArrayTrie::from_terms(vec!["alpha", "alpine", "beta"]);
let mut bytes = Vec::new();
BincodeSerializer::serialize(&dictionary, &mut bytes)?;

let restored: DoubleArrayTrie = BincodeSerializer::deserialize(&bytes[..])?;
assert!(restored.contains("alpine"));
# Ok::<(), libdictenstein::serialization::SerializationError>(())
```

This wire value is a binary `Vec<String>` extracted by iterative trie traversal. Decoding
reconstructs the selected backend, so the guarantee is preservation of its accepted term set,
not preservation of internal node identities or allocation layout.

The decoder requires exact input consumption. A complete bincode value followed by any byte
is rejected as `BincodeError::TrailingBytes` on both slice and reader APIs.

## Value-preserving bincode

The terms-only format has no slot for mapped values. For a `MappedDictionary<Value = V>`, call
the explicit value-preserving methods:

```rust
use libdictenstein::double_array_trie::DoubleArrayTrie;
use libdictenstein::serialization::BincodeSerializer;
use libdictenstein::MappedDictionary;

let dictionary: DoubleArrayTrie<u32> =
    DoubleArrayTrie::from_terms_with_values([("alpha", 10), ("beta", 20)]);

let mut bytes = Vec::new();
BincodeSerializer::serialize_with_values(&dictionary, &mut bytes)?;
let restored: DoubleArrayTrie<u32> =
    BincodeSerializer::deserialize_with_values(&bytes[..])?;
assert_eq!(restored.get_value("beta"), Some(20));
# Ok::<(), libdictenstein::serialization::SerializationError>(())
```

The value-preserving wire value is `Vec<(String, V)>` and is intentionally distinct from the
terms-only `Vec<String>`. Calling `serialize` on a mapped dictionary preserves its domain but
drops values on reconstruction; use `serialize_with_values` whenever values are part of the
application contract.

For `Unit = char` backends, use `serialize_with_values_char`; decoding uses the same
`deserialize_with_values` method because both value-aware variants have the same wire shape.

## Direct Serde types

Some backends derive `Serialize` and `Deserialize` and can use the compatibility shim directly:

```rust
use libdictenstein::double_array_trie::char::DoubleArrayTrieChar;
use libdictenstein::serialization::bincode_compat;

let dictionary = DoubleArrayTrieChar::from_terms(vec!["café", "日本語"]);
let bytes = bincode_compat::serialize(&dictionary)?;
let restored: DoubleArrayTrieChar = bincode_compat::deserialize(&bytes)?;
assert!(restored.contains("日本語"));
# Ok::<(), Box<dyn std::error::Error>>(())
```

This path preserves the Serde representation of that type. It is not available for every
backend, and it should not be confused with the backend-independent term-set contract of
`BincodeSerializer`.

## Protocol Buffers

The `protobuf` feature exposes four binary serializers:

| API | Representation | Use |
|---|---|---|
| `ProtobufSerializer` | Declared nodes, finals, and edges | Portable V1 interchange |
| `OptimizedProtobufSerializer` | Packed edge triples, delta finals | Compact V2 Rust interchange |
| `DatProtobufSerializer` | `LDT1` length-delimited term payload | DAT-specific reconstruction |
| `SuffixAutomatonProtobufSerializer` | Original indexed texts | Suffix-automaton reconstruction |

```rust
use libdictenstein::double_array_trie::DoubleArrayTrie;
use libdictenstein::serialization::{DictionarySerializer, ProtobufSerializer};

let dictionary = DoubleArrayTrie::from_terms(vec!["alpha", "alpine", "beta"]);
let mut bytes = Vec::new();
ProtobufSerializer::serialize(&dictionary, &mut bytes)?;
let restored: DoubleArrayTrie = ProtobufSerializer::deserialize(&bytes[..])?;
assert!(restored.contains("alpha"));
# Ok::<(), libdictenstein::serialization::SerializationError>(())
```

The general decoder validates declared roots/endpoints/finals, byte-sized edge labels,
reachable acyclicity, UTF-8 paths, and the declared term count. V2 additionally validates the
packed triple count and checked terminal-ID deltas.

The DAT `edge_data` grammar is binary and self-identifying:

```text
payload := "LDT1" term*
term    := byte_length:u32_le utf8_bytes[byte_length]
```

The decoder rejects missing magic, truncated lengths or bodies, invalid UTF-8, inconsistent
term counts, and newline-delimited compatibility data. No plaintext fallback exists.

Protocol Buffers is a compact encoding, not compression. Its broad runtime support makes it
the default cross-language choice, but compatibility still requires sharing the exact schema
and validation fixtures.

## Gzip wrapper

`GzipSerializer<S>` decorates a supported binary serializer:

```rust
use libdictenstein::double_array_trie::DoubleArrayTrie;
use libdictenstein::serialization::{
    BincodeSerializer, DictionarySerializer, GzipSerializer,
};

let dictionary = DoubleArrayTrie::from_terms(vec!["alpha", "alpine", "beta"]);
let mut bytes = Vec::new();
GzipSerializer::<BincodeSerializer>::serialize(&dictionary, &mut bytes)?;
let restored: DoubleArrayTrie =
    GzipSerializer::<BincodeSerializer>::deserialize(&bytes[..])?;
assert!(restored.contains("beta"));
# Ok::<(), libdictenstein::serialization::SerializationError>(())
```

`GzipSerializer<ProtobufSerializer>` is available with both `compression` and `protobuf`.
Bincode and protobuf are compact encodings but are not themselves compressed; gzip can exploit
repeated prefixes, labels, and field patterns. The benefit is corpus-dependent and costs CPU,
latency, and whole-stream decompression. Benchmark representative artifacts before enabling it.

## Correctness contract

For a terms-only dictionary $`D`$, serialization promises language preservation:

```math
L(\operatorname{decode}(\operatorname{encode}(D))) = L(D).
```

For a mapped dictionary serialized through the value-aware bincode API, it additionally
promises lookup preservation:

```math
\forall t \in L(D),\quad
\operatorname{value}_{\operatorname{roundtrip}(D)}(t)
= \operatorname{value}_{D}(t).
```

These invariants are checked across byte and Unicode backends by example tests and generated
term/value maps. Bincode has byte-layout pins and trailing-data tests. Protobuf tests cover V1,
V2, specialized payloads, corruption, truncation, graph validation, compression composition,
and correspondence with bincode term sets.

## Security and resource boundaries

All persistence decoders operate on untrusted structured bytes and may allocate according to
encoded collection lengths. The high-level protobuf serializers currently read the provided
stream to completion before message decoding. Therefore:

- impose a compressed-input limit before gzip;
- impose a decompressed-output limit while inflating;
- bound process memory and elapsed work for untrusted payloads;
- authenticate artifacts when provenance matters;
- reject an unexpected format or marker instead of guessing another schema;
- atomically replace durable files so interruption cannot expose a partial payload.

Structural validation prevents malformed graphs from becoming dictionaries, but it is not an
authentication mechanism. Bincode's exact-consumption check prevents an otherwise valid prefix
from hiding appended data.

## Format selection

Use bincode for Rust applications that control library-version compatibility. Use Protocol
Buffers when another language must read the artifact or when an explicit schema is required.
Wrap either in gzip only when measured storage or transfer savings justify decompression cost.

Do not use a text format for persisted production dictionaries.

## References

- [Serde data model](https://serde.rs/data-model.html)
- [Protocol Buffers proto3 guide](https://protobuf.dev/programming-guides/proto3/)
- [Protocol Buffers encoding](https://protobuf.dev/programming-guides/encoding/)
