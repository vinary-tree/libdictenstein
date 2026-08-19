# Security

Security documentation for libdictenstein: what an adversary can and cannot do to a process that
uses these dictionaries, where the trust boundaries are, and how the library is hardened. Notation
follows [`docs/notation.md`](../notation.md).

libdictenstein is a **library**, not a service — it has no network surface, no privilege boundary,
and no persistent daemon. Its security posture is therefore about **how it behaves on adversarial
input**: malformed serialized data, hostile key sets, and untrusted callers. The three concerns that
matter, and where each is covered:

| Concern | Question | Document |
|---------|----------|----------|
| **Threat model** | Who is the adversary, what do they control, what is in and out of scope? | [threat-model.md](threat-model.md) |
| **Untrusted input** | Can adversarial keys or crafted serialized data cause OOM, panic, or stack overflow? | [untrusted-input.md](untrusted-input.md) |
| **Deserialization** | What are the trust boundaries when loading a dictionary from bytes? | [deserialization-safety.md](deserialization-safety.md) |
| **Memory safety** | Where is the `unsafe`, and what proves it sound? | [unsafe-contracts.md](unsafe-contracts.md) |
| **FFI boundary** | What can a foreign ABI caller cause through the `ldict_*` C ABI and the exported resource vtables, and whose duty is each defense? | [ffi-boundary.md](ffi-boundary.md) |

## The one-paragraph posture

The public **in-memory** dictionary APIs are safe, while three acceleration seams are explicit and
reviewed: SCDAWG handle `Send`/`Sync` assertions, sealed double-array layouts validated before
unchecked reads, and DynamicDAWG's typed `NonNull` cursors over an immutable revision retained by
`Arc`. Dense cursors and native pointer capabilities are separate associated types, and only dense
cursors can enter the integer ABI. Strict-provenance Miri, compile-time type separation,
correspondence tests, and the source-derived unsafe inventory cover these seams. The one
caller-reachable production panic is `BijectiveMap::insert` on a duplicate (with a non-panicking
`try_insert` alternative). The **deserialization** paths are where adversarial *data* is handled:
parsing is fail-closed (length-guarded, returns errors rather than panicking or reading out of
bounds), but a few allocation-sizing sites read a count from untrusted input *before* validating
it, so a crafted input can request a very large allocation — the residual OOM edge documented in
[untrusted-input.md](untrusted-input.md). The **persistent** engine contributes 37 of the 214
grouped inventory patterns; the remaining patterns cover generic cursor contracts, DAT and
DynamicDAWG acceleration, ABI adapters, and test probes. Every pattern resolves to one of 40
reviewed contracts; see [unsafe-contracts.md](unsafe-contracts.md).

## What this is not

- **Not a cryptographic component.** libdictenstein stores and retrieves terms; it does no
  encryption, signing, or access control. Do not rely on it for confidentiality or integrity of
  data at rest — that is the caller's responsibility.
- **Not a sandbox.** Loading a serialized dictionary executes no code from the input, but it does
  allocate memory proportional to (and, at a few sites, requested by) the input; treat a serialized
  blob from an untrusted source the way you would any untrusted allocation request
  ([deserialization-safety.md](deserialization-safety.md)).
