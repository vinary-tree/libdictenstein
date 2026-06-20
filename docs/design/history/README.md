# Design history — preserved campaign records

The documents under this directory are **preserved, point-in-time
scientific-ledger records** from the development campaigns that built
libdictenstein's persistent ARTrie family: execution plans, red-team
(adversarial-review) findings, formal-verification strategies, implementation
handoffs, deletion inventories, and bug-fix designs.

They are kept for **provenance** — to show how a mechanism was reasoned about,
attacked, converged, and landed — and are intentionally frozen at the state they
described. They are **not** maintained as current references. For the durable
"how it works today" documents, start from the parent
[`docs/design/README.md`](../README.md).

> These files are organized by campaign. Within a campaign the names usually encode
> a version/iteration progression (e.g. `…-d2` → `…-d2.5` → … → `…-d2.8`, or
> `…-v2` → `…-v4`), reflecting successive red-team rounds and revisions.

---

## Campaigns

| Subdirectory | Campaign |
|--------------|----------|
| [`slice3/`](slice3/) | **Slice-3 owned-deletion slicing** — the staged plan to remove the owned-tree deletion path (F5/F7 loader, L0.2 rollback, L1 recovery redirect, the L3.3 levels, and the C2 byte/char deletion inventories). |
| [`durable-commit-seq/`](durable-commit-seq/) | **Durable global commit-sequence redesign (D1 → D2.8)** — the iteratively red-teamed design for a crash-durable global commit sequence, plus the reconciliation-stamp seed step and the Order-A replay-order fix. |
| [`s5-flip/`](s5-flip/) | **S5 production lock-free flip** — the production read/write flip design (v1 → v4), the E1 read-flip, the impl plan, the final flip red-team, and the formal-verification strategy. |
| [`f7-eviction/`](f7-eviction/) | **F7 overlay-eviction productionization** — completion + revalidated execution plans, the v4 production-eviction design, the owned→overlay rotation, and its implementation ledger. |
| [`redteam/`](redteam/) | **Adversarial design reviews** — standalone red-team findings/syntheses for the D1/D2/D2.5 durable-commit-seq designs, the dg-recon reconciliation step, and the commit-rank-and-flip synthesis. |
| [`phase-f-g5/`](phase-f-g5/) | **Phase-F / G5 overlay-node genericization + owned-tree deletion** — the converged plan to delete the owned tree, plus the G4/G5 eviction-on-immutable-checkpoint, reclamation benchmark, and fault-in-on-read designs. |
| [`bug-fixes/`](bug-fixes/) | **Recovery / checkpoint double-apply fixes** — the designs for the reopen double-apply bug, the C2 double-apply fix, the torn-checkpoint fix (#48), and the checkpoint-record LSN watermark-gap fix (#49). |
| [`byte-flip/`](byte-flip/) | **Byte-trie flip** — the byte lock-free-flip design, its reachability audit, and the M4 flip design. |
| [`cx-codec/`](cx-codec/) | **CX (compact snapshot) codec** — the path-compressing overlay↔dense codec design and the accompanying state-mapping. |
| [`counter-u64/`](counter-u64/) | **`u64` counter restoration** — the execution spec and restoration design for full-width `u64` counter values. |
| [`vocab/`](vocab/) | **Vocab-trie sub-campaign** — the vocab overlay-flip design and the V6 owned-deletion blueprint for the term ↔ `u64` bijection trie. |

---

## Root-level records

A few documents do not belong to a multi-document campaign and live directly under
`history/`:

| Document | What it records |
|----------|-----------------|
| [`f0-hack-fixes.md`](f0-hack-fixes.md) | Principled replacements for the four "F0" hacks/gaps (`batch_insert` fan-out, `commit_document` errors, `compare_and_swap`/`get_or_insert` routes) ahead of the lock-free flip. |
| [`rb-overlay-delete.md`](rb-overlay-delete.md) | Design "R-B": the proven overlay `DELETE` for the lock-free char-ARTrie overlay, including the loom/proptest/TLA re-proof that insert ∪ remove stays linearizable once finality is no longer monotone. |
| [`persistence-enhancements-experimental-plan.md`](persistence-enhancements-experimental-plan.md) | The original PersistentARTrie persistence-enhancements experimental plan (group commit, epoch checkpointing, memory-pressure eviction, adaptive buffer pool, per-node logging). |
