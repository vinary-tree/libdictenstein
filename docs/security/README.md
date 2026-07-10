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

## The one-paragraph posture

The **in-memory** dictionaries are memory-safe by construction: they store nodes in flat `Vec`
arenas addressed by integer index — no raw pointers, no manual `Drop`, so no recursive-drop or
use-after-free surface — and the whole volatile tree carries only **4** `unsafe` sites (all
`Send`/`Sync` assertions in the SCDAWG node handle). The one caller-reachable production panic is
`BijectiveMap::insert` on a duplicate (with a non-panicking `try_insert` alternative). The
**deserialization** paths are where adversarial *data* is handled: parsing is fail-closed
(length-guarded, returns errors rather than panicking or reading out of bounds), but a few
allocation-sizing sites read a count from untrusted input *before* validating it, so a crafted input
can request a very large allocation — the residual OOM edge documented in
[untrusted-input.md](untrusted-input.md). The **persistent** engine concentrates the `unsafe` (37 of
43 crate-wide sites), each bound to a machine-checked contract; see
[unsafe-contracts.md](unsafe-contracts.md) and the CI-gated inventory it links.

## What this is not

- **Not a cryptographic component.** libdictenstein stores and retrieves terms; it does no
  encryption, signing, or access control. Do not rely on it for confidentiality or integrity of
  data at rest — that is the caller's responsibility.
- **Not a sandbox.** Loading a serialized dictionary executes no code from the input, but it does
  allocate memory proportional to (and, at a few sites, requested by) the input; treat a serialized
  blob from an untrusted source the way you would any untrusted allocation request
  ([deserialization-safety.md](deserialization-safety.md)).
