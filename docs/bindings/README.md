# Bindings — the producer contract corpus

**Navigation**: [← Documentation index](../README.md)

libdictenstein is the **producer** half of the family's dictionary ABI: it
owns the concrete dictionaries and their CRUD, exports a 35-function `ldict_*`
C surface, and hands consumers a two-word, retained `vt.dictionary.v1`
resource whose snapshots they walk. The **consumer** half (cursor model, lease
protocol, language-facade query APIs) lives in
[liblevenshtein](https://github.com/vinary-tree/liblevenshtein-rust) — this
corpus documents everything on the producing side of that boundary.

## What lives where

| Artifact | Path | What it is |
|---|---|---|
| **C ABI reference** | [`c-abi-reference.md`](c-abi-reference.md) | The normative reference for all 35 `ldict_*` functions: exact signatures, preconditions, exact status sets, ownership, thread-safety, complexity; the status/kind/capability tables; the per-backend support matrix; persistence caveats; a compile-and-run-verified C example. |
| **Resource-producer architecture** | [`resource-producer.md`](resource-producer.md) | How the producer side works: the four backend bindings, `OwnedDictionaryResource` and the retain ledger, per-backend $`\mathcal{O}(1)`$ snapshot capture, lazy ABI-local node ids, the flag truth table, and the new-backend checklist. |
| **FFI boundary analysis** | [`../security/ffi-boundary.md`](../security/ffi-boundary.md) | The producer-side trust analysis: what a misbehaving foreign caller can and cannot cause, and whose duty each defense is. Extends the [threat model](../security/threat-model.md). |
| **Findings ledger** | [`FINDINGS_LEDGER.md`](FINDINGS_LEDGER.md) | The scientific ledger of binding-scrutiny findings: defects, pins, coverage gaps, and version-pin inconsistencies (`LDICT-B<N>` schema). |
| **Machine-readable model** | [`../../bindings/api.json`](../../bindings/api.json) | The source of truth for the binding surface: symbols, enums, kinds, capabilities, marshalling and snapshot laws, facade layout, registry coordinates. |
| **Contract gate** | [`../../scripts/check-bindings.py`](../../scripts/check-bindings.py) | Enforces the model against `src/ffi.rs`, `include/libdictenstein.h`, the interop-header mirror, and all 13 language facades. CI job `binding-contract`. |
| **Diagrams** | [`../diagrams/`](../diagrams/) | `abi-producer-component` (layer map), `snapshot-capture-sequence` (the walk protocol), `owned-resource-lifecycle-state` (the retain ledger); sources under [`../diagrams/src/`](../diagrams/src/). |
| **Language facades** | [`../../bindings/`](../../bindings/) | The 13 per-language packages over the `ldict_*` surface. Per-language usage guides are scheduled for the family plan's uniform language sweep (wave W7). |

## Reading order

1. **Orient** — the component diagram in
   [`resource-producer.md § 2`](resource-producer.md#2-architecture) shows
   the whole producer stack on one page.
2. **Call it** — [`c-abi-reference.md`](c-abi-reference.md), front to back:
   versioning → status discipline → backend matrix → the function groups →
   the verified example.
3. **Understand what you were handed** — the rest of
   [`resource-producer.md`](resource-producer.md): snapshots, node-id leasing,
   flags, and the refcount ledger.
4. **Trust it** — [`../security/ffi-boundary.md`](../security/ffi-boundary.md)
   for the adversarial reading, then the family canon below for the laws this
   repo instantiates.
5. **Audit it** — [`FINDINGS_LEDGER.md`](FINDINGS_LEDGER.md) plus a local run
   of `python3 scripts/check-bindings.py`.

## Family documents

Canonical family-level specifications live with the interop crate in
liblevenshtein-rust (linked absolutely — cross-repo relative paths do not
survive packaging):

- [ABI reference — `vinary_tree_interop.h`, annotated](https://github.com/vinary-tree/liblevenshtein-rust/blob/master/vinary-tree-interop/docs/abi-reference.md)
- [ABI evolution policy — the four version counters](https://github.com/vinary-tree/liblevenshtein-rust/blob/master/vinary-tree-interop/docs/abi-evolution.md)
- [Family security model — trust zones and validation duties](https://github.com/vinary-tree/liblevenshtein-rust/blob/master/vinary-tree-interop/docs/security-model.md)
- [liblevenshtein language-binding architecture (the consumer side)](https://github.com/vinary-tree/liblevenshtein-rust/blob/master/docs/language-bindings.md)
