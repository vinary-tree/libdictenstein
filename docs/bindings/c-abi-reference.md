# The `ldict_*` C ABI — producer reference

**Navigation**: [← Bindings corpus](README.md) ·
[Resource-producer architecture](resource-producer.md) ·
[FFI boundary analysis](../security/ffi-boundary.md) ·
[Findings ledger](FINDINGS_LEDGER.md)

This is the normative reference for the **41-function `ldict_*` C ABI** exported
by the libdictenstein cdylib — the project-owned surface above the family
resource ABI. Every function is documented with its exact header signature, its
preconditions, the **exact** set of statuses it can return (derived from the
function bodies in [`src/ffi.rs`](../../src/ffi.rs), not from convention), its
ownership rules, its thread-safety truth, and its complexity.

Authoritative sources, in precedence order:

1. [`bindings/api.json`](../../bindings/api.json) — the machine-readable model
   of this surface (symbols, enums, kinds, capabilities, marshalling laws),
   enforced against `src/ffi.rs`, `include/libdictenstein.h`, and all 13
   language facades by [`scripts/check-bindings.py`](../../scripts/check-bindings.py)
   (CI job `binding-contract`).
2. [`include/libdictenstein.h`](../../include/libdictenstein.h) — the C header
   whose signatures are quoted verbatim below.
3. [`src/ffi.rs`](../../src/ffi.rs) — the implementation each claim below was
   read from.

The family layer underneath (two-word `VtResource`, retain/release,
`query_interface`, the `vt.dictionary.v1` vtable) is specified once, in the
canonical interop documents — see the [family documents](#family-documents)
footer. This document covers what libdictenstein adds **above** that layer and
how the two connect at [`ldict_dictionary_resource`](#ldict_dictionary_resource).

---

## 1. Terms

| Term | Definition |
|---|---|
| handle | An opaque `LdictDictionary*` returned by a constructor. It owns one concrete dictionary plus one retained resource, and is destroyed by [`ldict_dictionary_free`](#ldict_dictionary_free). |
| backend | The concrete dictionary implementation behind a handle: DynamicDAWG, DoubleArrayTrie, SCDAWG, persistent ARTrie, or persistent vocabulary ARTrie. Reported by [`ldict_dictionary_kind`](#ldict_dictionary_kind). |
| unit domain | The label alphabet a dictionary transitions over: raw bytes (`1`), Unicode scalar values (`2`), or `u64` tokens (`3`). Matches the interop `VtUnitDomain` numbering. |
| capability | One bit in the `uint64_t` bitset reported by [`ldict_dictionary_capabilities`](#ldict_dictionary_capabilities): an operation family the backend supports. |
| out-parameter | A caller-supplied pointer the function writes a result through. Unless a function documents otherwise, out-parameters are written **only on `LDICT_STATUS_OK`**. |
| resource | The two-word `VtResource` (context + vtable) borrowed from a handle: the family-neutral object a consumer retains, negotiates interfaces on, and walks snapshots of. |
| snapshot | An immutable capture of one dictionary revision, obtained through the resource vtable's `snapshot` operation — never through an `ldict_*` function. Capture is $`\mathcal{O}(1)`$ (see [resource-producer.md](resource-producer.md)). |
| boundary | The `catch_unwind` + thread-local-error wrapper every fallible `ldict_*` function runs inside (`boundary()` in `src/ffi.rs`). |

---

## 2. Versioning and the error channel

The project ABI carries two counters, following the family's four-counter
evolution model (see the canonical
[ABI evolution policy](https://github.com/vinary-tree/liblevenshtein-rust/blob/master/vinary-tree-interop/docs/abi-evolution.md)):

| Constant | Value | Meaning | Caller check |
|---|---|---|---|
| `LDICT_ABI_VERSION` | 1 | Breaking-change counter for the `ldict_*` surface: layouts, ownership rules, status meanings. | **Exact equality** — refuse any other value. |
| `LDICT_API_REVISION` | 5 | Additive counter: revision 5 adds the bounded entry cursor/reducer surface over `vt.dict.entry.v1`. | **At least** — a facade built against revision $`n`$ refuses a library reporting less than $`n`$. |

Every fallible function reports failure twice: as an `LdictStatus` return value
(the machine channel) and as a human-readable message retrievable through
[`ldict_last_error_message`](#ldict_last_error_message) (the diagnostic
channel). The message is **thread-local**: concurrent failures on different
threads never overwrite each other.

### `ldict_abi_version`

```c
LDICT_API uint32_t ldict_abi_version(void);
```

Returns `LDICT_ABI_VERSION` (currently `1`).

- **Preconditions**: none.
- **Statuses**: none — the function cannot fail and does not touch the error channel.
- **Ownership**: nothing allocated or transferred.
- **Thread-safety**: safe from any thread, any time.
- **Complexity**: $`\mathcal{O}(1)`$.

### `ldict_api_revision`

```c
LDICT_API uint32_t ldict_api_revision(void);
```

Returns `LDICT_API_REVISION` (currently `5`).

- **Preconditions**: none.
- **Statuses**: none — cannot fail.
- **Ownership**: nothing allocated or transferred.
- **Thread-safety**: safe from any thread, any time.
- **Complexity**: $`\mathcal{O}(1)`$.

### `ldict_last_error_message`

```c
LDICT_API const char* ldict_last_error_message(void);
```

Returns the calling thread's most recent ABI diagnostic as a NUL-terminated
UTF-8 string. The pointer is never null: after a successful call (`OK` or,
prospectively, `END`) it points at an empty string, because the boundary clears
the slot on success. Interior NUL bytes in a diagnostic are escaped to `\0`
before storage, so the C string always carries the full message.

- **Preconditions**: none.
- **Statuses**: none — cannot fail.
- **Ownership**: the string is owned by libdictenstein's thread-local storage. It remains valid **on the same thread** until the next fallible `ldict_*` call from that thread; copy it before calling anything else.
- **Thread-safety**: inherently thread-safe — each thread reads its own slot. Reading another thread's diagnostic is impossible by construction.
- **Complexity**: $`\mathcal{O}(1)`$.

---

## 3. The status enum

```c
typedef enum LdictStatus {
    LDICT_STATUS_OK = 0,
    LDICT_STATUS_END = 1,
    LDICT_STATUS_INVALID_ARGUMENT = 2,
    LDICT_STATUS_INVALID_UTF8 = 3,
    LDICT_STATUS_NULL_POINTER = 4,
    LDICT_STATUS_PANIC = 5,
    LDICT_STATUS_UNSUPPORTED = 6,
    LDICT_STATUS_IO_ERROR = 7,
    LDICT_STATUS_CLOSED = 8,
    LDICT_STATUS_DOMAIN_MISMATCH = 9,
    LDICT_STATUS_LIMIT_EXCEEDED = 10,
    LDICT_STATUS_PROVIDER_ERROR = 11,
    LDICT_STATUS_BATCH_IN_USE = 12
} LdictStatus;
```

| Value | Name | Semantics | Producible today? |
|---:|---|---|---|
| 0 | `OK` | The operation completed; every documented out-parameter was written. | yes — every function |
| 1 | `END` | An entry cursor is exhausted, or an entry reducer requests successful early stop. | yes — entry collection surface |
| 2 | `INVALID_ARGUMENT` | An argument value is outside its contract: an unknown unit domain, an empty persistence path, or `LdictOptionalU64.has_value` outside $`\{0, 1\}`$. | yes |
| 3 | `INVALID_UTF8` | A term, pattern, or path that must be UTF-8 was not (see the [text-acceptance matrix](#54-text-acceptance-per-backend)). | yes |
| 4 | `NULL_POINTER` | A required pointer was null: the handle, a non-empty input buffer, or any out-parameter. | yes |
| 5 | `PANIC` | A Rust panic was caught at the boundary; the panic payload becomes the thread-local diagnostic. Defense-in-depth — no known input produces it. | yes (any boundary-wrapped function) |
| 6 | `UNSUPPORTED` | The backend cannot perform this operation **in any domain** — e.g. removal from an SCDAWG, substring search on a DAWG (see [§ 5.3](#53-what-unsupported-vs-domain_mismatch-mean)). | yes |
| 7 | `IO_ERROR` | A persistent-engine operation failed: create/open, WAL append, checkpoint. The diagnostic carries the engine's message. | yes (persistent backends only) |
| 8 | `CLOSED` | A handle was already closed. | **no** — reserved. `ldict_*` handles have no closed-but-not-freed state; the code exists for family enum-shape parity and future lifecycle surfaces. |
| 9 | `DOMAIN_MISMATCH` | The operation exists, but the caller used the wrong **term representation** for the dictionary's unit domain — e.g. a `*_text` call on a `u64`-domain dictionary. | yes |
| 10 | `LIMIT_EXCEEDED` | A resource bound was exceeded. Today: [`ldict_vocab_get_term`](#ldict_vocab_get_term)'s output buffer is too small (the required size is reported). | yes |
| 11 | `PROVIDER_ERROR` | The negotiated entry provider returned an unknown status, malformed metadata/batch, or an otherwise unclassified failure. | yes — entry collection surface |
| 12 | `BATCH_IN_USE` | The entry cursor already has a live borrowed batch; release its exact generation before `next`, `reduce`, or `free`. | yes — entry collection surface |

### 3.1 This enum is per-project — never cast across projects

`LdictStatus` deliberately shares its **prefix** `0..=8` (`OK` through
`CLOSED`) with liblevenshtein's `LlevStatus`, but the tails diverge — and the
divergence is where careless code corrupts meaning:

| Value | `LdictStatus` (this project) | `LlevStatus` (liblevenshtein) |
|---:|---|---|
| 9 | **`DOMAIN_MISMATCH`** | `LIMIT_EXCEEDED` |
| 10 | **`LIMIT_EXCEEDED`** | `PROVIDER_ERROR` |
| 11 | **`PROVIDER_ERROR`** | `BATCH_IN_USE` |
| 12 | **`BATCH_IN_USE`** | `DOMAIN_MISMATCH` |

The interop layer's `VtStatus` is a **third** numbering again (`NullPointer`
is 3 there, 4 here). This is intentional: each project's status enum is an
independent contract with its own totality proof obligation — libdictenstein's
`BindingError → LdictStatus` mapping is one closed function
(`binding()` in `src/ffi.rs`; formal home: the wave-W2 Rocq status-mapping
spec, IDs `LDICT-STAT-*` in the family plan), and it owes nothing to the
numeric choices of its siblings. The practical rule for every consumer and
facade:

> **Map by name, never by number.** Converting a raw `ldict_*` status integer
> into another project's enum (or vice versa) silently turns a domain
> mismatch into a limit overflow. Each facade must carry a per-project
> mapping table keyed on the constants of the header it was built against.

### 3.2 The boundary contract

Every fallible function's body runs inside `boundary()`:

1. **Panic containment** — the closure runs under `catch_unwind`; a panic
   payload becomes the diagnostic and the return value `PANIC`. No Rust panic
   ever unwinds into a foreign caller through an `ldict_*` function.
2. **Error-channel discipline** — on `OK` (or `END`) the thread-local message
   is cleared; on any failure it is set before the status is returned. The
   diagnostic therefore always describes the **most recent failure** of the
   calling thread, never a stale one.
3. **Out-parameter hygiene** — constructors write `NULL` through
   `*out_dictionary` *before* attempting construction, so a failed constructor
   never leaves an uninitialized handle pointer. All other out-parameters are
   written only on `OK`, with the single documented exception of
   [`ldict_vocab_get_term`](#ldict_vocab_get_term).

---

## 4. Common argument contracts

**Buffers.** Every `(data, len)` pair follows one rule: when `len == 0`,
`data` may be null (the empty term/pattern is valid input); when `len > 0`, a
null `data` is `NULL_POINTER`. Terms may contain embedded NUL bytes — lengths
are explicit everywhere, and the ABI never treats term data as C strings.

**`LdictOptionalU64`.**

```c
typedef struct LdictOptionalU64 {
    uint64_t value;
    uint8_t has_value;
    uint8_t reserved[7];
} LdictOptionalU64;
```

`has_value` must be exactly `0` or `1`; anything else is `INVALID_ARGUMENT`
(checked before any mutation). `reserved` **must be zero** per the model
(`bindings/api.json` pins `mustBeZero`); revision 4 does not yet reject a
nonzero `reserved` on input — the bytes are reserved precisely so a future
revision can assign meaning, so writing anything else forfeits forward
compatibility. On output the producer always writes zeros. When
`has_value == 0`, `value` is ignored on input and written as `0` on output.

**Entry descriptors.** Batched mutation passes contiguous descriptor arrays;
each element embeds a `(data, len)` buffer and an optional value:

```c
typedef struct LdictTextEntry {
    const uint8_t* data;
    size_t len;
    LdictOptionalU64 value;
} LdictTextEntry;

typedef struct LdictU64Entry {
    const uint64_t* data;
    size_t len;
    LdictOptionalU64 value;
} LdictU64Entry;
```

**Insert/remove result booleans.** `out_inserted` is written `1` when the term
was **not previously present** (a value update of an existing term reports
`0`); `out_removed` is `1` when a present term was removed (`0` when it was
absent). Both are idempotence signals, not error signals — updating or
re-removing is `OK`.

**Lookup result pairs.** `ldict_dictionary_get_*` distinguishes three cases
through two outputs: `out_found == 0` — the term is absent;
`out_found == 1` with `has_value == 0` — the term is a member without an
attached value; `out_found == 1` with `has_value == 1` — a member with `value`.

---

## 5. Backends: kinds, capabilities, and the support matrix

### 5.1 Kind identifiers

```c
#define LDICT_KIND_DYNAMIC_DAWG 1u
#define LDICT_KIND_DOUBLE_ARRAY_TRIE 2u
#define LDICT_KIND_SCDAWG 3u
#define LDICT_KIND_PERSISTENT_ARTRIE 4u
#define LDICT_KIND_PERSISTENT_VOCAB_ARTRIE 5u
```

| Kind | Backend | Unit domains | Mutability | Documented in |
|---|---|---|---|---|
| 1 | DynamicDAWG | Byte, UnicodeScalar, U64 | fully mutable | [dynamic DAWG](../algorithms/implementations/dynamic-dawg.md) |
| 2 | DoubleArrayTrie | Byte, UnicodeScalar | read-only after construction | [double-array trie](../algorithms/implementations/double-array-trie.md) |
| 3 | SCDAWG | Byte, UnicodeScalar | insert-only (plus substring queries) | [SCDAWG](../theory/scdawg/README.md) |
| 4 | Persistent ARTrie | Byte, UnicodeScalar, U64 | mutable + durable | [persistence corpus](../persistence/README.md) |
| 5 | Persistent vocabulary ARTrie | UnicodeScalar | insert-only bijection term ↔ index | [vocab trie](../algorithms/vocab-trie.md) |

### 5.2 Capability bits

```c
#define LDICT_CAP_READ (UINT64_C(1) << 0)
#define LDICT_CAP_INSERT (UINT64_C(1) << 1)
#define LDICT_CAP_REMOVE (UINT64_C(1) << 2)
#define LDICT_CAP_CLEAR (UINT64_C(1) << 3)
#define LDICT_CAP_COMPACT (UINT64_C(1) << 4)
#define LDICT_CAP_SUBSTRING (UINT64_C(1) << 5)
#define LDICT_CAP_CHECKPOINT (UINT64_C(1) << 6)
```

The exact bitsets, from the `capabilities()` match in `src/ffi.rs`:

| Backend | READ | INSERT | REMOVE | CLEAR | COMPACT | SUBSTRING | CHECKPOINT | Bitset |
|---|:-:|:-:|:-:|:-:|:-:|:-:|:-:|---:|
| DynamicDAWG | ✔ | ✔ | ✔ | ✔ | ✔ | — | — | `0x1F` |
| DoubleArrayTrie | ✔ | — | — | — | — | — | — | `0x01` |
| SCDAWG | ✔ | ✔ | — | — | — | ✔ | — | `0x23` |
| Persistent ARTrie | ✔ | ✔ | ✔ | — | — | — | ✔ | `0x47` |
| Persistent vocab ARTrie | ✔ | ✔ | — | — | — | — | ✔ | `0x43` |

### 5.3 What `UNSUPPORTED` vs `DOMAIN_MISMATCH` mean

The two "you can't do that" statuses carve the failure space precisely:

- **`UNSUPPORTED`** — the backend lacks the operation *family* entirely; the
  corresponding capability bit is clear. Removing from a DoubleArrayTrie fails
  this way no matter how the term is spelled.
- **`DOMAIN_MISMATCH`** — the operation family exists, but the caller entered
  through the wrong **term representation** for the dictionary's unit domain:
  a `*_text` call against a `u64`-domain dictionary, or a `*_u64` call against
  a byte/Unicode dictionary. The fix is to switch entry points, not backends.

Derived from the per-operation match arms in `src/ffi.rs` and
`src/bindings.rs`, the full matrix (cell = status when the operation cannot
proceed; `OK` = supported):

| Operation | DynDAWG B/U | DynDAWG 64 | DAT B/U | SCDAWG B/U | P-ART B/U | P-ART 64 | P-Vocab |
|---|---|---|---|---|---|---|---|
| `insert_text` (+`_value`, batch) | OK | DOMAIN_MISMATCH | UNSUPPORTED | OK | OK | DOMAIN_MISMATCH | OK¹ |
| `remove_text` | OK | DOMAIN_MISMATCH | UNSUPPORTED | UNSUPPORTED | OK | DOMAIN_MISMATCH | UNSUPPORTED |
| `contains_text` / `get_text` (+`_value`) | OK | DOMAIN_MISMATCH | OK | OK | OK | DOMAIN_MISMATCH | OK¹ |
| `insert_u64` (+`_value`, batch) | DOMAIN_MISMATCH² | OK | DOMAIN_MISMATCH | DOMAIN_MISMATCH | DOMAIN_MISMATCH² | OK | DOMAIN_MISMATCH |
| `remove_u64` | DOMAIN_MISMATCH² | OK | DOMAIN_MISMATCH | DOMAIN_MISMATCH | DOMAIN_MISMATCH² | OK | DOMAIN_MISMATCH |
| `contains_u64` / `get_u64` (+`_value`) | DOMAIN_MISMATCH² | OK | DOMAIN_MISMATCH | DOMAIN_MISMATCH | DOMAIN_MISMATCH² | OK | DOMAIN_MISMATCH |
| `clear` | OK | OK | UNSUPPORTED | UNSUPPORTED | UNSUPPORTED | UNSUPPORTED | UNSUPPORTED |
| `compact` | OK | OK | UNSUPPORTED | UNSUPPORTED | UNSUPPORTED | UNSUPPORTED | UNSUPPORTED |
| `checkpoint` | UNSUPPORTED | UNSUPPORTED | UNSUPPORTED | UNSUPPORTED | OK | OK | OK |
| `scdawg_contains_substring` / `substring_frequency` | UNSUPPORTED | UNSUPPORTED | UNSUPPORTED | OK | UNSUPPORTED | UNSUPPORTED | UNSUPPORTED |
| `vocab_get_term` | UNSUPPORTED | UNSUPPORTED | UNSUPPORTED | UNSUPPORTED | UNSUPPORTED | UNSUPPORTED | OK |

¹ Vocabulary value semantics: on insert, a supplied value is the **index** to
bind (`insert_with_index`); with no value the vocabulary assigns the next
available index itself (nearly dense — a lost insert race may burn an id).
Binding a *conflicting* index — one already assigned to a different term, a
term already bound to a different index, or an index below the store's start
index — is rejected by the engine and surfaces as `IO_ERROR` with a
diagnostic. On lookup, the value returned **is** the term's index
(`out_found == 1` always pairs with `has_value == 1` for vocabulary members).

² A DynamicDAWG or persistent ARTrie constructed for the Byte or UnicodeScalar
domain rejects the `*_u64` entry points with `DOMAIN_MISMATCH`, exactly as the
`u64`-domain instance rejects `*_text`: the *backend* supports both families,
each *instance* speaks one domain.

### 5.4 Text acceptance per backend

"Text" operations take `(const uint8_t*, size_t)` — but which byte sequences a
backend accepts depends on both backend and domain:

| Backend · domain | Accepts | On non-UTF-8 |
|---|---|---|
| DynamicDAWG · Byte | **arbitrary bytes** (embedded NUL, `0xFF`, anything) | accepted as-is |
| DynamicDAWG · UnicodeScalar | valid UTF-8 only | `INVALID_UTF8` |
| DoubleArrayTrie · Byte and UnicodeScalar | valid UTF-8 only — the byte form selects byte-granular *transitions*, not permissive encodings | `INVALID_UTF8` |
| SCDAWG · Byte and UnicodeScalar | valid UTF-8 only (same rationale) | `INVALID_UTF8` |
| Persistent ARTrie · Byte | **arbitrary bytes** | accepted as-is |
| Persistent ARTrie · UnicodeScalar; vocabulary | valid UTF-8 only | `INVALID_UTF8` |

---

## 6. Constructors

All seven constructors share the contract: `out_dictionary` must be non-null;
it is nulled first, then set to a heap-allocated handle only on `OK`. The
returned handle owns the dictionary **and** one already-retained resource, so
[`ldict_dictionary_resource`](#ldict_dictionary_resource) is $`\mathcal{O}(1)`$
and infallible-by-construction later. Handles are destroyed with
[`ldict_dictionary_free`](#ldict_dictionary_free); handles are independent —
constructing, using, and freeing different handles on different threads never
interferes.

### `ldict_dynamic_dawg_new`

```c
LDICT_API LdictStatus ldict_dynamic_dawg_new(
    uint32_t unit_domain,
    LdictDictionary** out_dictionary);
```

Constructs an empty, fully mutable DynamicDAWG for one unit domain
(`1` = byte, `2` = Unicode scalar, `3` = `u64` token).

- **Preconditions**: `out_dictionary` non-null; `unit_domain` ∈ {1, 2, 3}.
- **Statuses**: `OK` · `NULL_POINTER` · `INVALID_ARGUMENT` (unknown domain) · `PANIC`.
- **Ownership**: on `OK` the caller owns the handle.
- **Thread-safety**: safe from any thread.
- **Complexity**: $`\mathcal{O}(1)`$.

### `ldict_double_array_trie_new`

```c
LDICT_API LdictStatus ldict_double_array_trie_new(
    uint32_t unit_domain,
    const LdictTextEntry* entries,
    size_t entry_count,
    LdictDictionary** out_dictionary);
```

Builds an **immutable** double-array trie (Aoe [[4]](#references)) from a
descriptor array in one shot. Domains 1 and 2 only; every term must be valid
UTF-8 (the byte domain selects byte-granular transitions, not permissive
encodings). Terms are copied — the descriptor buffers may be freed on return.

- **Preconditions**: `out_dictionary` non-null; `entries` non-null when `entry_count > 0`; each `entry.data` non-null when `entry.len > 0`; each `entry.value.has_value` ∈ {0, 1}; terms valid UTF-8; `unit_domain` ∈ {1, 2}.
- **Statuses**: `OK` · `NULL_POINTER` · `INVALID_ARGUMENT` (unknown domain; bad `has_value`) · `UNSUPPORTED` (domain 3) · `INVALID_UTF8` · `PANIC`.
- **Ownership**: on `OK` the caller owns the handle; input buffers stay caller-owned.
- **Thread-safety**: safe from any thread.
- **Complexity**: expected $`\mathcal{O}\!\left(\sum_i \lvert t_i \rvert\right)`$ over the total term length (double-array base/check placement is expected near-linear; adversarial label distributions degrade the constant, not the correctness).

### `ldict_scdawg_new`

```c
LDICT_API LdictStatus ldict_scdawg_new(
    uint32_t unit_domain,
    LdictDictionary** out_dictionary);
```

Constructs an empty SCDAWG (symmetric compact DAWG, Blumer et al.
[[5]](#references)) for byte or Unicode-scalar transitions. Insert-only; adds
substring containment/frequency queries and produces suffix-flagged resources.

- **Preconditions**: `out_dictionary` non-null; `unit_domain` ∈ {1, 2}.
- **Statuses**: `OK` · `NULL_POINTER` · `INVALID_ARGUMENT` (unknown domain) · `UNSUPPORTED` (domain 3) · `PANIC`.
- **Ownership**: on `OK` the caller owns the handle.
- **Thread-safety**: safe from any thread.
- **Complexity**: $`\mathcal{O}(1)`$.

### `ldict_persistent_artrie_create`

```c
LDICT_API LdictStatus ldict_persistent_artrie_create(
    uint32_t unit_domain,
    const uint8_t* path_data,
    size_t path_len,
    LdictDictionary** out_dictionary);
```

Creates a new filesystem-backed persistent ARTrie (all three domains) rooted
at the UTF-8 directory path `(path_data, path_len)`. The path is used
**verbatim** — no canonicalization, no sandboxing; see the
[FFI boundary analysis](../security/ffi-boundary.md#persistence-paths) before
passing externally influenced paths. Present only in builds with the
`persistent-artrie` feature (the `ffi` feature always enables it, so the
exported symbol set is invariant across FFI builds).

- **Preconditions**: `out_dictionary` non-null; `path_data` non-null (a zero-length path is rejected as empty, not null); path valid UTF-8 and non-empty; `unit_domain` ∈ {1, 2, 3}; the target usable for creation.
- **Statuses**: `OK` · `NULL_POINTER` · `INVALID_UTF8` (path) · `INVALID_ARGUMENT` (empty path; unknown domain) · `IO_ERROR` (engine rejected creation) · `PANIC`.
- **Ownership**: on `OK` the caller owns the handle; the engine owns the on-disk layout under the path.
- **Thread-safety**: safe from any thread; concurrent create/open on the **same path** is arbitrated by the engine and surfaces as `IO_ERROR` on the loser.
- **Complexity**: $`\mathcal{O}(1)`$ plus file-creation I/O.

### `ldict_persistent_artrie_open`

```c
LDICT_API LdictStatus ldict_persistent_artrie_open(
    uint32_t unit_domain,
    const uint8_t* path_data,
    size_t path_len,
    LdictDictionary** out_dictionary);
```

Opens an existing persistent ARTrie. Recovery runs here: the engine loads the
last checkpoint manifest and replays the committed WAL tail, so every
acknowledged pre-crash write is visible on `OK` (see
[durability & recovery](../persistence/durability-and-recovery.md)).

- **Preconditions**: as for `create`, plus: the path holds a store previously created with the **same unit domain** and value profile.
- **Statuses**: `OK` · `NULL_POINTER` · `INVALID_UTF8` · `INVALID_ARGUMENT` · `IO_ERROR` (missing/corrupt store, WAL replay failure, profile mismatch — the diagnostic distinguishes them) · `PANIC`.
- **Ownership**: on `OK` the caller owns the handle.
- **Thread-safety**: safe from any thread; same-path arbitration as `create`.
- **Complexity**: $`\mathcal{O}(\lvert \mathrm{WAL\ tail} \rvert)`$ replay beyond the checkpoint, then $`\mathcal{O}(1)`$.

### `ldict_persistent_vocab_create`

```c
LDICT_API LdictStatus ldict_persistent_vocab_create(
    const uint8_t* path_data,
    size_t path_len,
    LdictDictionary** out_dictionary);
```

Creates a persistent **bidirectional vocabulary**: a Unicode-scalar term set
where every member carries a stable `u64` index, queryable in both directions
(term → index through `ldict_dictionary_get_text*`; index → term through
[`ldict_vocab_get_term`](#ldict_vocab_get_term)). No `unit_domain` parameter —
the domain is fixed to UnicodeScalar.

- **Preconditions**: `out_dictionary` non-null; path non-null, UTF-8, non-empty, usable.
- **Statuses**: `OK` · `NULL_POINTER` · `INVALID_UTF8` · `INVALID_ARGUMENT` (empty path) · `IO_ERROR` · `PANIC`.
- **Ownership**: on `OK` the caller owns the handle.
- **Thread-safety**: safe from any thread.
- **Complexity**: $`\mathcal{O}(1)`$ plus file-creation I/O.

### `ldict_persistent_vocab_open`

```c
LDICT_API LdictStatus ldict_persistent_vocab_open(
    const uint8_t* path_data,
    size_t path_len,
    LdictDictionary** out_dictionary);
```

Opens an existing persistent vocabulary, with the same recovery semantics as
[`ldict_persistent_artrie_open`](#ldict_persistent_artrie_open).

- **Preconditions / Statuses / Ownership / Thread-safety**: identical to `ldict_persistent_vocab_create`, except the path must hold an existing vocabulary store.
- **Complexity**: $`\mathcal{O}(\lvert \mathrm{WAL\ tail} \rvert)`$ replay, then $`\mathcal{O}(1)`$.

---

## 7. Handle introspection and lifetime

### `ldict_dictionary_kind`

```c
LDICT_API LdictStatus ldict_dictionary_kind(
    const LdictDictionary* dictionary,
    uint32_t* out_kind);
```

Writes the backend identifier ([§ 5.1](#51-kind-identifiers)). A persistent
handle reports `4` or `5` depending on whether it is the vocabulary variant.

- **Preconditions**: both pointers non-null; `dictionary` live.
- **Statuses**: `OK` · `NULL_POINTER` · `PANIC`.
- **Ownership**: none.
- **Thread-safety**: safe concurrently with any other call on the same handle.
- **Complexity**: $`\mathcal{O}(1)`$.

### `ldict_dictionary_capabilities`

```c
LDICT_API LdictStatus ldict_dictionary_capabilities(
    const LdictDictionary* dictionary,
    uint64_t* out_capabilities);
```

Writes the capability bitset ([§ 5.2](#52-capability-bits)). Facades should
gate optional UI/API affordances on these bits rather than on kind — new
backends may join with novel bit combinations.

- **Preconditions**: both pointers non-null; `dictionary` live.
- **Statuses**: `OK` · `NULL_POINTER` · `PANIC`.
- **Ownership**: none.
- **Thread-safety**: safe concurrently with any other call on the same handle.
- **Complexity**: $`\mathcal{O}(1)`$.

### `ldict_dictionary_free`

```c
LDICT_API void ldict_dictionary_free(LdictDictionary* dictionary);
```

Destroys a handle: drops the concrete dictionary binding and releases the
handle's own resource retain. **Null is a safe no-op.** Resources a consumer
has independently retained remain fully usable afterwards — the captured
payload lives until the last retain is released (see the
[lifecycle diagram](resource-producer.md#7-the-retain-ledger--owneddictionaryresource)).
For persistent backends, buffered state not yet checkpointed is still
recovered from the WAL on the next open; call
[`ldict_dictionary_checkpoint`](#ldict_dictionary_checkpoint) first to bound
replay time.

- **Preconditions**: `dictionary` is null or a live handle from this library, and no other call on this handle is in flight or made afterwards (destruction is the one operation the caller must externally order).
- **Statuses**: none — `void`. A panic during destruction would abort (there is no status channel), which is why the drop paths hold no panicking operations.
- **Ownership**: consumes the handle.
- **Thread-safety**: may be called from any thread, but never concurrently with other calls on the **same** handle.
- **Complexity**: $`\Theta(1)`$ while other co-owners remain (retained resources, cloned bindings — the drop is a reference-count decrement); the **last** co-owner pays the backend teardown, $`\mathcal{O}(n)`$ in its structures, with deferred reclamation on the lock-free cores.

### `ldict_dictionary_resource`

```c
/* Borrowed resource; retaining consumers may outlive the dictionary handle. */
LDICT_API LdictStatus ldict_dictionary_resource(
    const LdictDictionary* dictionary,
    VtResource* out_resource);
```

Writes the handle's two-word `vt.dictionary.v1` resource — the bridge to the
family ABI. The words are **borrowed**: they stay valid while the handle is
alive, and copying them confers no ownership. A consumer that stores them must
call the resource vtable's `retain` first (the copy-not-retain law); a
retained resource then outlives even `ldict_dictionary_free`. Repeated calls
return the same context — the retain ledger is shared, not forked. Snapshots
are captured through the resource vtable, never through `ldict_*`; the full
walk protocol is in [§ 14](#14-the-snapshot-then-walk-consumer-loop).

- **Preconditions**: both pointers non-null; `dictionary` live.
- **Statuses**: `OK` · `NULL_POINTER` · `PANIC`.
- **Ownership**: the two words are borrowed; ownership begins only at an explicit `retain`.
- **Thread-safety**: safe concurrently with any other call; the vtable operations behind the resource are themselves `PARALLEL_REENTRANT` (see [resource-producer.md](resource-producer.md#6-the-flag-truth-table)).
- **Complexity**: $`\mathcal{O}(1)`$ — the retained resource was created with the handle.

### `ldict_dictionary_len`

```c
LDICT_API LdictStatus ldict_dictionary_len(
    const LdictDictionary* dictionary,
    size_t* out_len);
```

Writes the number of currently visible terms (the live revision's count — a
concurrent writer may change it immediately after).

- **Preconditions**: both pointers non-null; `dictionary` live.
- **Statuses**: `OK` · `NULL_POINTER` · `PANIC`.
- **Ownership**: none.
- **Thread-safety**: safe concurrently; the value is a linearization-point read.
- **Complexity**: $`\mathcal{O}(1)`$ (maintained counters on every backend).

---

## 8. Maintenance

### `ldict_dictionary_clear`

```c
LDICT_API LdictStatus ldict_dictionary_clear(LdictDictionary* dictionary);
```

Removes every term by **publishing a fresh empty revision** — readers and
existing snapshots keep the old revision; nothing is destroyed in place.
DynamicDAWG only.

- **Preconditions**: `dictionary` non-null and live.
- **Statuses**: `OK` · `NULL_POINTER` · `UNSUPPORTED` (every backend except DynamicDAWG) · `PANIC`.
- **Ownership**: none.
- **Thread-safety**: safe concurrently with readers and snapshot holders; the swap is atomic under the binding's writer lock.
- **Complexity**: $`\mathcal{O}(1)`$ publication; the superseded revision is reclaimed when its last reference (including snapshots) drops.

### `ldict_dictionary_compact`

```c
LDICT_API LdictStatus ldict_dictionary_compact(
    LdictDictionary* dictionary,
    size_t* out_reclaimed);
```

Restores minimal DAWG structure after a mutation workload and writes the
number of reclaimed nodes. DynamicDAWG only.

- **Preconditions**: both pointers non-null; `dictionary` live.
- **Statuses**: `OK` · `NULL_POINTER` · `UNSUPPORTED` (non-DynamicDAWG) · `PANIC`.
- **Ownership**: none.
- **Thread-safety**: safe concurrently; compaction publishes new structure without invalidating captured snapshots.
- **Complexity**: $`\mathcal{O}(n)`$ in the node count of the current revision.

### `ldict_dictionary_checkpoint`

```c
LDICT_API LdictStatus ldict_dictionary_checkpoint(LdictDictionary* dictionary);
```

Atomically persists the current revision and advances the committed WAL
frontier ([persistence caveats, § 13](#13-persistence-path-caveats)).
Persistent backends only.

- **Preconditions**: `dictionary` non-null and live.
- **Statuses**: `OK` · `NULL_POINTER` · `UNSUPPORTED` (in-memory backends) · `IO_ERROR` · `PANIC`.
- **Ownership**: none.
- **Thread-safety**: safe concurrently with CRUD; the engine orders the checkpoint against in-flight writes (see [group commit](../persistence/group-commit.md)).
- **Complexity**: I/O-bound in the work since the previous checkpoint — $`\mathcal{O}(\Delta)`$ where $`\Delta`$ is the dirty set plus WAL frontier advance.

---

## 9. Vocabulary lookup

### `ldict_vocab_get_term`

```c
LDICT_API LdictStatus ldict_vocab_get_term(
    const LdictDictionary* dictionary,
    uint64_t index,
    uint8_t* out_data,
    size_t capacity,
    size_t* out_len,
    uint8_t* out_found);
```

Copies the UTF-8 term bound to `index` in a persistent vocabulary. This is the
one function with a **size-query protocol** and the one documented exception
to write-only-on-`OK`:

1. **Size query** — `out_data == NULL` and `capacity == 0`: on `OK`,
   `out_found` reports existence and `out_len` the full byte count; nothing is
   copied.
2. **Fits** — `capacity >= *out_len`: the term bytes are copied (not
   NUL-terminated; the length is the contract), `out_found = 1`.
3. **Too small** — `capacity < *out_len` with a non-null `out_data`: the first
   `capacity` bytes are copied (a truncated prefix — may split a UTF-8
   sequence), `out_len` still receives the **required** size, `out_found = 1`,
   and the call returns `LIMIT_EXCEEDED`. Retry with a buffer of `*out_len`
   bytes.
4. **Absent index** — `out_found = 0`, `out_len = 0`, status `OK`.

- **Preconditions**: `dictionary`, `out_len`, `out_found` non-null; `out_data` non-null whenever `capacity != 0`; `dictionary` is a persistent **vocabulary** handle.
- **Statuses**: `OK` · `NULL_POINTER` · `UNSUPPORTED` (any non-vocabulary backend, including the plain persistent ARTrie) · `LIMIT_EXCEEDED` · `PANIC`.
- **Ownership**: the caller owns `out_data`; the producer never retains it.
- **Thread-safety**: safe concurrently.
- **Complexity**: $`\mathcal{O}(\lvert \mathrm{term} \rvert)`$.

---

## 10. CRUD

The twelve single-term functions come in two calling conventions per
operation family:

- the **aggregate** forms pass/return `LdictOptionalU64` **by value** —
  natural for C, C++, ctypes with struct support, FFM, and cgo;
- the **scalar `*_value` twins** (added in API revision 4) split the optional
  into `uint64_t value, uint8_t has_value` — added because several dynamic FFI
  runtimes (Ruby Fiddle, Fortran `bind(c)`, some libffi paths) mis-handle
  small-aggregate-by-value calling conventions. Twins are semantically
  identical to their aggregates; each twin below states only its deltas.

### 10.1 Text CRUD

#### `ldict_dictionary_insert_text`

```c
LDICT_API LdictStatus ldict_dictionary_insert_text(
    LdictDictionary* dictionary,
    const uint8_t* data,
    size_t len,
    LdictOptionalU64 value,
    uint8_t* out_inserted);
```

Inserts or updates one text/byte term with an optional `u64` value. Updating
an existing term overwrites its value and reports `out_inserted = 0`.

- **Preconditions**: `dictionary`, `out_inserted` non-null; `data` non-null when `len > 0`; `value.has_value` ∈ {0, 1}; the backend·domain accepts the bytes ([§ 5.4](#54-text-acceptance-per-backend)).
- **Statuses**: `OK` · `NULL_POINTER` · `INVALID_ARGUMENT` (bad `has_value`) · `INVALID_UTF8` (UTF-8-validating backend·domain) · `UNSUPPORTED` (DoubleArrayTrie) · `DOMAIN_MISMATCH` (`u64`-domain instance) · `IO_ERROR` (persistent WAL append) · `PANIC`.
- **Ownership**: term bytes are copied; the buffer stays caller-owned.
- **Thread-safety**: safe concurrently with readers, writers, and snapshot holders — every mutable backend is internally synchronized and publishes revisions atomically.
- **Complexity**: amortized $`\mathcal{O}(\lvert t \rvert)`$ along the term's path (DAWG minimization bookkeeping and ART node splits are amortized constant per unit); persistent inserts add one WAL append.

#### `ldict_dictionary_insert_text_value`

```c
LDICT_API LdictStatus ldict_dictionary_insert_text_value(
    LdictDictionary* dictionary,
    const uint8_t* data,
    size_t len,
    uint64_t value,
    uint8_t has_value,
    uint8_t* out_inserted);
```

Scalar twin of `ldict_dictionary_insert_text` (it delegates directly).

- **Preconditions / Statuses / Ownership / Thread-safety / Complexity**: identical to the aggregate form — including `INVALID_ARGUMENT` when `has_value` is neither 0 nor 1.

#### `ldict_dictionary_remove_text`

```c
LDICT_API LdictStatus ldict_dictionary_remove_text(
    LdictDictionary* dictionary,
    const uint8_t* data,
    size_t len,
    uint8_t* out_removed);
```

Removes one text/byte term; `out_removed = 0` when it was absent (still `OK`).

- **Preconditions**: `dictionary`, `out_removed` non-null; `data` non-null when `len > 0`; backend supports removal in this domain.
- **Statuses**: `OK` · `NULL_POINTER` · `INVALID_UTF8` (Unicode-domain backends) · `UNSUPPORTED` (DoubleArrayTrie, SCDAWG, persistent vocabulary) · `DOMAIN_MISMATCH` (`u64`-domain instance) · `IO_ERROR` (persistent) · `PANIC`.
- **Ownership**: none.
- **Thread-safety**: safe concurrently; snapshots keep removed terms.
- **Complexity**: amortized $`\mathcal{O}(\lvert t \rvert)`$; persistent removals add one WAL append.

#### `ldict_dictionary_contains_text`

```c
LDICT_API LdictStatus ldict_dictionary_contains_text(
    const LdictDictionary* dictionary,
    const uint8_t* data,
    size_t len,
    uint8_t* out_contains);
```

Tests exact membership of one text/byte term against the live revision.

- **Preconditions**: `dictionary`, `out_contains` non-null; `data` non-null when `len > 0`.
- **Statuses**: `OK` · `NULL_POINTER` · `INVALID_UTF8` (UTF-8-validating backend·domain) · `DOMAIN_MISMATCH` (`u64`-domain instance) · `PANIC`. Never `IO_ERROR`: every backend's membership read has an infallible signature (persistent reads resolve through the in-memory overlay and buffer manager without a fallible ABI-visible path).
- **Ownership**: none.
- **Thread-safety**: safe concurrently; lock-free read paths on the in-memory backends.
- **Complexity**: $`\mathcal{O}(\lvert t \rvert)`$ — one transition per unit.

#### `ldict_dictionary_get_text`

```c
LDICT_API LdictStatus ldict_dictionary_get_text(
    const LdictDictionary* dictionary,
    const uint8_t* data,
    size_t len,
    uint8_t* out_found,
    LdictOptionalU64* out_value);
```

Membership plus value in one crossing, preserving the three-way distinction of
[§ 4](#4-common-argument-contracts) (absent / member-without-value /
member-with-value). For vocabularies the value is the term's index.

- **Preconditions**: `dictionary`, `out_found`, `out_value` non-null; `data` non-null when `len > 0`.
- **Statuses**: `OK` · `NULL_POINTER` · `INVALID_UTF8` · `DOMAIN_MISMATCH` · `PANIC` (same reasoning as `contains_text`).
- **Ownership**: none; `out_value.reserved` is written as zeros.
- **Thread-safety**: safe concurrently.
- **Complexity**: $`\mathcal{O}(\lvert t \rvert)`$.

#### `ldict_dictionary_get_text_value`

```c
LDICT_API LdictStatus ldict_dictionary_get_text_value(
    const LdictDictionary* dictionary,
    const uint8_t* data,
    size_t len,
    uint8_t* out_found,
    uint64_t* out_value,
    uint8_t* out_has_value);
```

Scalar twin of `ldict_dictionary_get_text`; it validates its three outputs,
delegates, and unpacks the aggregate on `OK`.

- **Preconditions / Statuses / Ownership / Thread-safety / Complexity**: identical to the aggregate form (all three out-pointers are null-checked before delegation).

### 10.2 u64 CRUD

The `u64` family mirrors the text family exactly, over token sequences
`(const uint64_t*, size_t)`. Tokens are opaque — any value including `0` and
`UINT64_MAX` is a valid unit; there is no encoding to violate, so
`INVALID_UTF8` is impossible here. Only `u64`-domain DynamicDAWGs and
persistent ARTries accept these entry points; **everything else** answers
`DOMAIN_MISMATCH` ([§ 5.3](#53-what-unsupported-vs-domain_mismatch-mean)).

One engine-level asymmetry, read directly from
`PersistentARTrieU64::insert_sequence_with_value` (src/persistent_artrie/u64.rs):
unlike the byte/Unicode persistent profiles — whose mutations return the
engine's error and therefore surface `IO_ERROR` here — the persistent `u64`
profile's mutation API is infallible-by-signature. An engine-level write
failure on that profile is logged through the `log` facade (`log::warn!`) and
reported as `out_inserted = 0` / `out_removed = 0`, **not** as `IO_ERROR`.
Callers needing a hard durability guarantee on the `u64` profile should pair
mutations with [`ldict_dictionary_checkpoint`](#ldict_dictionary_checkpoint),
whose failures do surface as `IO_ERROR`.

#### `ldict_dictionary_insert_u64`

```c
LDICT_API LdictStatus ldict_dictionary_insert_u64(
    LdictDictionary* dictionary,
    const uint64_t* data,
    size_t len,
    LdictOptionalU64 value,
    uint8_t* out_inserted);
```

- **Preconditions**: `dictionary`, `out_inserted` non-null; `data` non-null when `len > 0`; `value.has_value` ∈ {0, 1}; the instance is `u64`-domain.
- **Statuses**: `OK` · `NULL_POINTER` · `INVALID_ARGUMENT` (bad `has_value`) · `DOMAIN_MISMATCH` · `PANIC`.
- **Ownership**: tokens are copied.
- **Thread-safety**: safe concurrently.
- **Complexity**: amortized $`\mathcal{O}(\lvert t \rvert)`$ in the token count.

#### `ldict_dictionary_insert_u64_value`

```c
LDICT_API LdictStatus ldict_dictionary_insert_u64_value(
    LdictDictionary* dictionary,
    const uint64_t* data,
    size_t len,
    uint64_t value,
    uint8_t has_value,
    uint8_t* out_inserted);
```

Scalar twin of `ldict_dictionary_insert_u64` (direct delegation).

- **Preconditions / Statuses / Ownership / Thread-safety / Complexity**: identical to the aggregate form.

#### `ldict_dictionary_remove_u64`

```c
LDICT_API LdictStatus ldict_dictionary_remove_u64(
    LdictDictionary* dictionary,
    const uint64_t* data,
    size_t len,
    uint8_t* out_removed);
```

- **Preconditions**: `dictionary`, `out_removed` non-null; `data` non-null when `len > 0`; `u64`-domain instance.
- **Statuses**: `OK` · `NULL_POINTER` · `DOMAIN_MISMATCH` · `PANIC`.
- **Ownership**: none.
- **Thread-safety**: safe concurrently.
- **Complexity**: amortized $`\mathcal{O}(\lvert t \rvert)`$.

#### `ldict_dictionary_contains_u64`

```c
LDICT_API LdictStatus ldict_dictionary_contains_u64(
    const LdictDictionary* dictionary,
    const uint64_t* data,
    size_t len,
    uint8_t* out_contains);
```

- **Preconditions**: `dictionary`, `out_contains` non-null; `data` non-null when `len > 0`.
- **Statuses**: `OK` · `NULL_POINTER` · `DOMAIN_MISMATCH` · `PANIC`.
- **Ownership**: none.
- **Thread-safety**: safe concurrently.
- **Complexity**: $`\mathcal{O}(\lvert t \rvert)`$.

#### `ldict_dictionary_get_u64`

```c
LDICT_API LdictStatus ldict_dictionary_get_u64(
    const LdictDictionary* dictionary,
    const uint64_t* data,
    size_t len,
    uint8_t* out_found,
    LdictOptionalU64* out_value);
```

- **Preconditions**: `dictionary`, `out_found`, `out_value` non-null; `data` non-null when `len > 0`.
- **Statuses**: `OK` · `NULL_POINTER` · `DOMAIN_MISMATCH` · `PANIC`.
- **Ownership**: none; `out_value.reserved` written as zeros.
- **Thread-safety**: safe concurrently.
- **Complexity**: $`\mathcal{O}(\lvert t \rvert)`$.

#### `ldict_dictionary_get_u64_value`

```c
LDICT_API LdictStatus ldict_dictionary_get_u64_value(
    const LdictDictionary* dictionary,
    const uint64_t* data,
    size_t len,
    uint8_t* out_found,
    uint64_t* out_value,
    uint8_t* out_has_value);
```

Scalar twin of `ldict_dictionary_get_u64`.

- **Preconditions / Statuses / Ownership / Thread-safety / Complexity**: identical to the aggregate form (all three out-pointers null-checked before delegation).

---

## 11. Batch mutation

Both batch functions apply entries **sequentially, fail-fast**:

```math
\text{apply}(e_1), \ \text{apply}(e_2), \ \ldots \ \text{until the first } e_k \text{ that fails}
```

On failure at entry $`e_k`$: entries $`e_1 \ldots e_{k-1}`$ **remain applied**
(there is no rollback — the batch is a marshalling optimization, not a
transaction), `out_inserted` is **not written** (read it only on `OK`), and
the returned status plus diagnostic describe $`e_k`$. Duplicate terms within
one batch are legal; later entries update earlier ones and do not count as new
insertions.

#### `ldict_dictionary_insert_text_batch`

```c
LDICT_API LdictStatus ldict_dictionary_insert_text_batch(
    LdictDictionary* dictionary,
    const LdictTextEntry* entries,
    size_t entry_count,
    size_t* out_inserted);
```

Applies `entry_count` text insertions in one FFI crossing; on `OK`,
`out_inserted` receives the number of **newly inserted** terms (updates
excluded), so `*out_inserted <= entry_count`.

- **Preconditions**: `dictionary`, `out_inserted` non-null; `entries` non-null when `entry_count > 0`; per entry: `data` non-null when `len > 0`, `has_value` ∈ {0, 1}, bytes acceptable per [§ 5.4](#54-text-acceptance-per-backend).
- **Statuses**: `OK` · `NULL_POINTER` · `INVALID_ARGUMENT` · `INVALID_UTF8` · `UNSUPPORTED` (DoubleArrayTrie) · `DOMAIN_MISMATCH` (`u64`-domain instance) · `IO_ERROR` (persistent) · `PANIC` — the per-entry failure statuses are exactly `ldict_dictionary_insert_text`'s.
- **Ownership**: all buffers caller-owned; terms copied as applied.
- **Thread-safety**: safe concurrently; note the batch as a whole is **not atomic** — a concurrent reader may observe a prefix.
- **Complexity**: amortized $`\mathcal{O}\!\left(\sum_i \lvert t_i \rvert\right)`$.

#### `ldict_dictionary_insert_u64_batch`

```c
LDICT_API LdictStatus ldict_dictionary_insert_u64_batch(
    LdictDictionary* dictionary,
    const LdictU64Entry* entries,
    size_t entry_count,
    size_t* out_inserted);
```

The `u64` mirror of the text batch, with the same fail-fast prefix semantics.

- **Preconditions**: as above, over `LdictU64Entry`; the instance is `u64`-domain.
- **Statuses**: `OK` · `NULL_POINTER` · `INVALID_ARGUMENT` (bad `has_value`) · `DOMAIN_MISMATCH` · `PANIC`.
- **Ownership**: all buffers caller-owned.
- **Thread-safety**: safe concurrently; not atomic as a batch.
- **Complexity**: amortized $`\mathcal{O}\!\left(\sum_i \lvert t_i \rvert\right)`$ in tokens.

---

## 12. Substring queries (SCDAWG)

#### `ldict_scdawg_contains_substring`

```c
LDICT_API LdictStatus ldict_scdawg_contains_substring(
    const LdictDictionary* dictionary,
    const uint8_t* data,
    size_t len,
    uint8_t* out_contains);
```

Tests whether the UTF-8 pattern occurs **inside** any indexed term — the
SCDAWG's raison d'être: containment in time proportional to the pattern, not
the corpus (Blumer et al. [[5]](#references)).

- **Preconditions**: `dictionary`, `out_contains` non-null; `data` non-null when `len > 0`; pattern valid UTF-8; SCDAWG handle.
- **Statuses**: `OK` · `NULL_POINTER` · `INVALID_UTF8` (pattern) · `UNSUPPORTED` (every non-SCDAWG backend) · `PANIC`.
- **Ownership**: none.
- **Thread-safety**: safe concurrently, including with inserts.
- **Complexity**: $`\mathcal{O}(\lvert p \rvert)`$ in the pattern length.

#### `ldict_scdawg_substring_frequency`

```c
LDICT_API LdictStatus ldict_scdawg_substring_frequency(
    const LdictDictionary* dictionary,
    const uint8_t* data,
    size_t len,
    size_t* out_frequency);
```

Counts the pattern's occurrences across all indexed terms (an absent pattern
reports `0` with `OK`).

- **Preconditions / Statuses / Ownership / Thread-safety**: identical to `ldict_scdawg_contains_substring`, with `out_frequency` in place of `out_contains`.
- **Complexity**: $`\mathcal{O}(\lvert p \rvert)`$ — the count is read from the located node's occurrence annotation.

---

## 13. Bounded entry collection cursor

Revision 5 exposes the optional `vt.dict.entry.v1` provider through project
status codes and an opaque owned cursor. Applications never copy the provider's
two-word cursor or call its vtable directly.

```c
typedef VtDictionaryEntry LdictEntry;
typedef VtDictionaryEntryBatchLimits LdictEntryBatchLimits;
typedef VtDictionaryEntryBatchView LdictEntryBatch;
typedef VtDictionaryEntriesInfo LdictEntriesInfo;
typedef struct LdictEntryCursor LdictEntryCursor;

typedef LdictStatus (*LdictEntryReducer)(
    void* reducer_context, const LdictEntryBatch* batch);

LDICT_API LdictStatus ldict_dictionary_entries_open(
    const LdictDictionary* dictionary,
    LdictEntryCursor** out_cursor,
    LdictEntriesInfo* out_info);
LDICT_API LdictStatus ldict_entry_cursor_next(
    LdictEntryCursor* cursor,
    const LdictEntryBatchLimits* limits,
    LdictEntryBatch* out_batch);
LDICT_API LdictStatus ldict_entry_cursor_release(
    LdictEntryCursor* cursor, uint64_t generation);
LDICT_API LdictStatus ldict_entry_cursor_reduce(
    LdictEntryCursor* cursor,
    const LdictEntryBatchLimits* limits,
    LdictEntryReducer reducer,
    void* reducer_context,
    size_t* out_count);
LDICT_API LdictStatus ldict_entry_cursor_cancel(LdictEntryCursor* cursor);
LDICT_API LdictStatus ldict_entry_cursor_free(LdictEntryCursor* cursor);
```

`open` captures one immutable revision in O(1), writes its unit/value domains,
lexicographic order, optional exact cardinality, and snapshot identity, and
returns a cursor that may outlive the source dictionary. Failed opens leave
`*out_cursor == NULL` and zero the metadata output.

`next` accepts hard bounds for descriptors, unit-arena elements, and values.
On `OK` it returns one nonempty cursor-owned batch. Descriptor offsets and
lengths count elements, not bytes: `units` points to `uint8_t`, `uint32_t`, or
`uint64_t` according to `info.unit_domain`. A descriptor's `value_len == 0`
means a present valueless key; `value_len == 1` indexes a present `uint64_t`,
including zero and `UINT64_MAX`. No value sentinel is used.

Exactly one batch generation may be leased. Its pointers remain valid until
`release(cursor, batch.generation)` succeeds. Calling `next`, `reduce`, or
`free` while leased returns `BATCH_IN_USE`; a wrong or repeated generation
returns `INVALID_ARGUMENT`. `LIMIT_EXCEEDED` does not advance an oversized
first entry, so the caller may retry with larger bounds. Exhaustion is sticky
and returns `END` with a canonical empty batch.

`reduce` invokes the callback synchronously once per leased batch and always
settles the lease before interpreting callback control flow. `OK` continues,
`END` stops successfully, and another published `LdictStatus` aborts and
propagates. `out_count` counts entries accepted by completed callbacks.
Callbacks must not retain batch pointers or re-enter the same cursor.

`cancel` is idempotent and makes later traversal return `END`; it deliberately
does not invalidate a current lease. `free(NULL)` is a successful no-op.
Otherwise `free` consumes the opaque cursor only on `OK`, so a caller receiving
`BATCH_IN_USE` must release and retry. Independent cursors are reentrant, but
operations and close must not race on the same cursor.

---

## 14. Persistence-path caveats

The persistent constructors, `checkpoint`, and vocabulary lookups sit on the
durable ARTrie engine; four caveats matter at the ABI:

1. **Durability boundary.** Acknowledged mutations (`OK` from an insert or
   remove) are logged before they become visible ("log before publish" —
   [durability & recovery](../persistence/durability-and-recovery.md)), so a
   crash after `OK` loses nothing that was acknowledged under the engine's
   configured commit policy — with the one profile-level exception documented
   in [§ 10.2](#102-u64-crud) (the `u64` profile reports engine write failures
   as `out_inserted = 0`, not `IO_ERROR`). `checkpoint` does not *create*
   durability; it **bounds recovery time** by folding the WAL tail into the
   checkpoint manifest.
2. **Reopen contract.** `open` after a crash replays the committed WAL tail;
   `open` after a clean `checkpoint` + `free` is
   $`\mathcal{O}(1)`$-plus-manifest-load. Reopening with a different unit
   domain than the store was created with is an `IO_ERROR` (profile mismatch),
   not a silent reinterpretation.
3. **One writer universe per path.** The engine arbitrates concurrent opens of
   the same path; the loser sees `IO_ERROR`. Never point two *processes* at
   one store path expecting shared mutation — the ABI exposes no cross-process
   coordination.
4. **Paths are used verbatim.** No canonicalization, no traversal defense, no
   sandbox — `"../../../etc/cron.d"` is a path like any other. Hosts that
   derive store paths from external input own that validation; see the
   [FFI boundary analysis](../security/ffi-boundary.md#persistence-paths).

---

## 15. The snapshot-then-walk consumer loop

The `ldict_*` surface mutates and introspects; **traversal** happens on the
family resource, against an immutable snapshot. The complete protocol, as
literate pseudocode — each step names the law it discharges:

```text
walk_terms(dict):
    # 1. Bridge: borrow the two words. Borrowing is not owning —
    #    the copy-not-retain law says a stored copy needs a retain.
    res ← ldict_dictionary_resource(dict)
    res.vtable.retain(res.context)                      # own the copy

    # 2. Negotiate: name the interface and the minimum version you
    #    were compiled against. Unsupported → walk somewhere else.
    vt ← res.query_interface("vt.dictionary.v1", 1)

    # 3. Pin a revision: O(1), no copy, no lock held afterwards.
    #    Everything the walk reads comes from `snap`, so concurrent
    #    CRUD on `dict` cannot tear the traversal.
    snap ← vt.snapshot(res)                              # born retained
    wt   ← snap.query_interface("vt.dictionary.v1", 1)   # flags ⊇ IMMUTABLE

    # 4. Prefer the optional immutable graph. Validate the complete view
    #    before publication; value_cursor is opaque and is passed back only
    #    to this graph vtable. Unsupported selects the callback loop below.
    gt ← snap.query_interface("vt.dict.graph.v1", 1)
    if gt is supported:
        graph ← gt.graph(snap)
        validate_graph(graph)
        frontier ← [ (graph.root, ε) ]
        while frontier ≠ ∅:
            (node, prefix) ← pop(frontier)
            descriptor ← graph.nodes[node]
            if descriptor.is_final:
                emit(prefix,
                     gt.node_value_u64(snap, descriptor.value_cursor))
            for edge in graph.edges[descriptor.edge_range]:
                push(frontier, (edge.target, prefix · edge.label))
        goto release

    # 5. Callback fallback: node ids are ABI-local to `snap` — never mix
    #    ids across snapshots, never use them after the release below.
    frontier ← [ (wt.root(snap), ε) ]
    while frontier ≠ ∅:
        (node, prefix) ← pop(frontier)
        if wt.node_is_final(snap, node):
            emit(prefix, wt.node_value_u64(snap, node))
        # 6. Page edges: written = min(capacity, total − start);
        #    pages concatenate losslessly; total is stable.
        start ← 0
        repeat:
            (page, written, total) ← wt.node_edges(snap, node, start,
                                                   capacity = 256)
            for (label, child) in page[0 .. written):
                push(frontier, (child, prefix · label))
            start ← start + written
        until start ≥ total

release:
    # 7. Ledger balance: one release per owned retain, any order.
    snap.vtable.release(snap.context)
    res.vtable.release(res.context)
```

The sequence below shows the same protocol against the producer's internals,
with the trust boundary marked (source:
[`snapshot-capture-sequence.puml`](../diagrams/src/snapshot-capture-sequence.puml)):

<img src="../diagrams/snapshot-capture-sequence.svg" alt="Sequence diagram of the snapshot-then-walk protocol. The consumer, on the foreign side of the trust boundary, borrows the two-word resource from the ldict_* C ABI, retains it, negotiates vt.dictionary.v1, and calls snapshot(). The producer's ResourceContext clones the backend revision root in O(1) by structural sharing, wraps it in a TraversalSnapshot, and hands back a snapshot resource born with one retain via mem::forget. A subsequent live mutation publishes a successor revision that the pinned snapshot never sees. A compact-graph-aware consumer negotiates vt.dict.graph.v1 and receives stable flat arrays projected once for the revision; another consumer falls back to root and paged node_edges, whose append-only arena reads and established slots are lock-free. Teardown releases the snapshot, releases the source resource, and frees the dictionary handle." width="100%"/>

## 16. A complete, verified C example

The program below was compile-gated with
`cc -std=c17 -Wall -Wextra -Werror -fsyntax-only -I include` **and** linked
against the `ffi`-feature cdylib and executed; its output is reproduced after
the listing. Note the include path: `libdictenstein.h` includes the interop
header by the name `vinary_tree_interop.h` (overridable via the
`VT_INTEROP_HEADER` macro), and this repository ships a byte-identical mirror
at [`include/vinary_tree_interop.h`](../../include/vinary_tree_interop.h), so
`-I include` alone resolves both headers.

```c
/* snapshot_walk.c — construct a dictionary through the ldict_* C ABI, export
 * its two-word resource, capture an immutable snapshot, and walk the pinned
 * revision while the live dictionary keeps mutating.
 *
 * Compile check (repository root; the interop header ships in include/ as a
 * byte-identical mirror of the canonical sibling header):
 *
 *   cc -std=c17 -Wall -Wextra -Werror -fsyntax-only -I include snapshot_walk.c
 */
#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>

#include "libdictenstein.h" /* includes vinary_tree_interop.h */

static void fail(const char* where, LdictStatus status) {
    fprintf(stderr, "%s failed: status %d: %s\n", where, (int)status,
            ldict_last_error_message());
    exit(EXIT_FAILURE);
}

static void fail_vt(const char* where, VtStatus status) {
    fprintf(stderr, "%s failed: VtStatus %d\n", where, (int)status);
    exit(EXIT_FAILURE);
}

/* Follow one labelled edge of a snapshot, aborting when it is absent. */
static uint64_t must_step(const VtDictionaryVTable* walk, void* context,
                          uint64_t node, uint64_t label) {
    uint64_t child = 0;
    uint8_t found = 0;
    VtStatus status = walk->node_transition(context, node, label, &child, &found);
    if (status != VT_STATUS_OK) fail_vt("node_transition", status);
    if (!found) {
        fprintf(stderr, "transition U+%04" PRIX64 " unexpectedly missing\n", label);
        exit(EXIT_FAILURE);
    }
    return child;
}

int main(void) {
    /* 0. Version handshake: exact ABI match, at-least API revision. */
    if (ldict_abi_version() != LDICT_ABI_VERSION ||
        ldict_api_revision() < LDICT_API_REVISION) {
        fprintf(stderr, "incompatible libdictenstein ABI/API\n");
        return EXIT_FAILURE;
    }

    /* 1. Construct a mutable Unicode-scalar DynamicDAWG. */
    LdictDictionary* dictionary = NULL;
    LdictStatus status =
        ldict_dynamic_dawg_new(VT_UNIT_DOMAIN_UNICODE_SCALAR, &dictionary);
    if (status != LDICT_STATUS_OK) fail("ldict_dynamic_dawg_new", status);

    /* 2. CRUD: one scalar-form insert, then a single-crossing batch. */
    uint8_t inserted = 0;
    status = ldict_dictionary_insert_text_value(
        dictionary, (const uint8_t*)"cat", 3, 41u, 1u, &inserted);
    if (status != LDICT_STATUS_OK) fail("insert \"cat\"", status);

    const LdictTextEntry batch[] = {
        {(const uint8_t*)"car", 3, {0u, 0u, {0}}}, /* member, no value   */
        {(const uint8_t*)"cot", 3, {7u, 1u, {0}}}, /* member, value = 7  */
    };
    size_t batch_inserted = 0;
    status = ldict_dictionary_insert_text_batch(dictionary, batch, 2,
                                                &batch_inserted);
    if (status != LDICT_STATUS_OK) fail("batch insert", status);
    printf("inserted %u + %zu terms\n", inserted, batch_inserted);

    uint8_t found = 0;
    uint64_t value = 0;
    uint8_t has_value = 0;
    status = ldict_dictionary_get_text_value(
        dictionary, (const uint8_t*)"cat", 3, &found, &value, &has_value);
    if (status != LDICT_STATUS_OK) fail("get \"cat\"", status);
    printf("cat: found=%u has_value=%u value=%" PRIu64 "\n", found, has_value,
           value);

    /* 3. Export the two-word resource; retain before storing a copy. */
    VtResource resource = {NULL, NULL};
    status = ldict_dictionary_resource(dictionary, &resource);
    if (status != LDICT_STATUS_OK) fail("ldict_dictionary_resource", status);
    resource.vtable->retain(resource.context); /* stored copy => one retain */

    /* 4. Negotiate vt.dictionary.v1 and capture the current revision. */
    const void* raw_vtable = NULL;
    VtStatus vt_status = resource.vtable->query_interface(
        resource.context, &VT_DICTIONARY_INTERFACE_ID,
        VT_DICTIONARY_INTERFACE_VERSION, &raw_vtable);
    if (vt_status != VT_STATUS_OK) fail_vt("query_interface(source)", vt_status);
    const VtDictionaryVTable* source_vtable =
        (const VtDictionaryVTable*)raw_vtable;

    VtResource snapshot = {NULL, NULL};
    vt_status = source_vtable->snapshot(resource.context, &snapshot);
    if (vt_status != VT_STATUS_OK) fail_vt("snapshot", vt_status);
    /* The snapshot arrives owning one retain; release it during teardown. */

    const void* raw_walk = NULL;
    vt_status = snapshot.vtable->query_interface(
        snapshot.context, &VT_DICTIONARY_INTERFACE_ID,
        VT_DICTIONARY_INTERFACE_VERSION, &raw_walk);
    if (vt_status != VT_STATUS_OK) fail_vt("query_interface(snapshot)",
                                           vt_status);
    const VtDictionaryVTable* walk = (const VtDictionaryVTable*)raw_walk;
    printf("snapshot flags: immutable=%d suffix=%d\n",
           (walk->flags & VT_DICTIONARY_FLAG_IMMUTABLE) != 0,
           (walk->flags & VT_DICTIONARY_FLAG_SUFFIX_BASED) != 0);

    /* 5. Mutate the live dictionary AFTER capture. */
    uint8_t removed = 0;
    status = ldict_dictionary_remove_text(dictionary, (const uint8_t*)"cot", 3,
                                          &removed);
    if (status != LDICT_STATUS_OK) fail("remove \"cot\"", status);

    /* 6. Walk the pinned revision: root, paged edges, point transitions. */
    size_t pinned_len = 0;
    uint8_t len_known = 0;
    vt_status = walk->len(snapshot.context, &pinned_len, &len_known);
    if (vt_status != VT_STATUS_OK) fail_vt("len", vt_status);
    printf("snapshot terms: %zu (known=%u)\n", pinned_len, len_known);

    uint64_t root = 0;
    vt_status = walk->root(snapshot.context, &root);
    if (vt_status != VT_STATUS_OK) fail_vt("root", vt_status);

    VtDictionaryEdge page[VT_RECOMMENDED_EDGE_BATCH];
    size_t start = 0;
    size_t total = 0;
    do {
        size_t written = 0;
        vt_status = walk->node_edges(snapshot.context, root, start, page,
                                     VT_RECOMMENDED_EDGE_BATCH, &written,
                                     &total);
        if (vt_status != VT_STATUS_OK) fail_vt("node_edges", vt_status);
        for (size_t i = 0; i < written; ++i) {
            uint8_t is_final = 0;
            vt_status = walk->node_is_final(snapshot.context, page[i].node,
                                            &is_final);
            if (vt_status != VT_STATUS_OK) fail_vt("node_is_final", vt_status);
            printf("root edge U+%04" PRIX64 " -> node %" PRIu64 " (final=%u)\n",
                   page[i].label, page[i].node, is_final);
        }
        start += written;
    } while (start < total);

    /* The removed term is still present in the pinned revision. */
    uint64_t node = root;
    for (const char* c = "cot"; *c != '\0'; ++c) {
        node = must_step(walk, snapshot.context, node,
                         (uint64_t)(unsigned char)*c);
    }
    uint8_t pinned_final = 0;
    vt_status = walk->node_is_final(snapshot.context, node, &pinned_final);
    if (vt_status != VT_STATUS_OK) fail_vt("node_is_final(\"cot\")", vt_status);
    VtOptionalU64 pinned_value = {0u, 0u, {0}};
    vt_status = walk->node_value_u64(snapshot.context, node, &pinned_value);
    if (vt_status != VT_STATUS_OK) fail_vt("node_value_u64(\"cot\")", vt_status);

    uint8_t live_contains = 0;
    status = ldict_dictionary_contains_text(dictionary, (const uint8_t*)"cot",
                                            3, &live_contains);
    if (status != LDICT_STATUS_OK) fail("contains \"cot\"", status);
    printf("\"cot\": snapshot final=%u value=%" PRIu64
           " (has_value=%u); live contains=%u\n",
           pinned_final, pinned_value.value, pinned_value.has_value,
           live_contains);

    /* 7. Teardown — one release per owned retain; ordering unconstrained. */
    snapshot.vtable->release(snapshot.context);
    resource.vtable->release(resource.context);
    ldict_dictionary_free(dictionary);
    return EXIT_SUCCESS;
}
```

Observed output (debug cdylib, 2026-08-08):

```text
inserted 1 + 2 terms
cat: found=1 has_value=1 value=41
snapshot flags: immutable=1 suffix=0
snapshot terms: 3 (known=1)
root edge U+0063 -> node 1 (final=0)
"cot": snapshot final=1 value=7 (has_value=1); live contains=0
```

The last line **is** the snapshot law, observed through the ABI: the revision
pinned before `remove_text` still contains `cot ↦ 7`, while the live
dictionary no longer does.

---

## References

DOIs verified resolving 2026-08-08 (`curl -sIL` / Crossref metadata match).

1. G. E. Collins. "A Method for Overlapping and Erasure of Lists."
   *Communications of the ACM* 3(12), 1960 — the reference-counting ledger
   behind retain/release.
   [DOI:10.1145/367487.367501](https://doi.org/10.1145/367487.367501)
2. J. R. Driscoll, N. Sarnak, D. D. Sleator, R. E. Tarjan. "Making Data
   Structures Persistent." *JCSS* 38(1), 1989 — structural sharing, the basis
   of the $`\mathcal{O}(1)`$ snapshot-capture contract.
   [DOI:10.1016/0022-0000(89)90034-2](https://doi.org/10.1016/0022-0000(89)90034-2)
3. C. Okasaki. *Purely Functional Data Structures.* Cambridge University
   Press, 1998 — persistence as an API discipline.
   [DOI:10.1017/CBO9780511530104](https://doi.org/10.1017/CBO9780511530104)
4. J. Aoe. "An Efficient Digital Search Algorithm by Using a Double-Array
   Structure." *IEEE TSE* 15(9), 1989.
   [DOI:10.1109/32.31365](https://doi.org/10.1109/32.31365)
5. A. Blumer, J. Blumer, D. Haussler, R. McConnell, A. Ehrenfeucht. "Complete
   Inverted Files for Efficient Text Retrieval and Analysis." *JACM* 34(3),
   1987 — the SCDAWG.
   [DOI:10.1145/28869.28873](https://doi.org/10.1145/28869.28873)

## Family documents

Canonical family-level specifications live with the interop crate in
liblevenshtein-rust (linked absolutely — cross-repo relative paths do not
survive packaging):

- [ABI reference — `vinary_tree_interop.h`, annotated](https://github.com/vinary-tree/liblevenshtein-rust/blob/master/vinary-tree-interop/docs/abi-reference.md)
- [ABI evolution policy — the four version counters](https://github.com/vinary-tree/liblevenshtein-rust/blob/master/vinary-tree-interop/docs/abi-evolution.md)
- [Family security model — trust zones and validation duties](https://github.com/vinary-tree/liblevenshtein-rust/blob/master/vinary-tree-interop/docs/security-model.md)
- [liblevenshtein language-binding architecture (the consumer side)](https://github.com/vinary-tree/liblevenshtein-rust/blob/master/docs/language-bindings.md)
