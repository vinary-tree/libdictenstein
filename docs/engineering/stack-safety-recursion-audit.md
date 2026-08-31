# Stack-safety recursion audit

This audit optimizes for **recall**: missing a feasible recursion or recursive-lifecycle hazard is the primary failure. False positives are retained until source review proves that the alleged edge cannot execute.

The machine-readable evidence manifest is [stack-safety-recursion-evidence-manifest.json](stack-safety-recursion-evidence-manifest.json). It is a static export of pgmcp/libcpg findings plus source review, not an analysis engine or database. It binds each suppression to the current source SHA-256, exact call-site witnesses, evidence channel, disposition, and proof. Any source change reopens that row.

## Evidence union

The audit universe is the union of classic direct and mutual recursion, safe typed SCCs under every configured profile, recursive ownership/drop glue, stack-risk findings, generated operation paths, and manually identified coverage gaps. Typed absence never filters classic candidates.

## Current accounting

- Classic production units: 140/140 source-reviewed and dispositioned.
- Classic production call sites: 150/150 inspected.
- Historical typed candidates retained: 38; all have `compiler_resolved=false` and remain independently recorded.
- Recursive ownership components retained: 8, including four manually reviewed production roots hidden by safe-snapshot wrapper uncertainty.
- Production recursive ownership repairs: 5. `ChildNode`, `CharTrieNodeInner`, `OverlayNode`, `OverlayBuilderNode`, and `LockFreeDawgNode` use operation-specific shallow or iterative lifecycle machines with 100,000-depth gates where topology depth applies.
- Nonproduction-only classic additions: 1; tracked separately.
- Unresolved production candidates: 0. The current exact-RC2 all-features test target compiles, fourteen named 100,000-depth production gates pass, two ignored extreme character-lifecycle gates pass, and two overlay-worklist gates pass without stack tuning.

## Disposition semantics

`false_positive_proven` requires an impossible source edge, such as a different receiver/type or a trait body forwarding to a concrete inherent method. `implicit_lifecycle_refactor` records recursive generated or lifecycle behavior that required an explicit machine. `test_oracle_bounded` is reserved for test-only structures whose depth/size bound is enforced in their construction.

## Analyzer limitations and campaign closure

The current pgmcp safe snapshots are activated and then immediately reported stale, even without a Rust source mutation. This is tracked as `pgmcp-semantic-snapshot-immediate-staleness-after-refresh` (database ID 6073). The ledger therefore retains the last queryable typed snapshot and treats current typed delivery as an explicit coverage gap; it does not interpret zero returned findings as evidence of absence.

Safe parser mode also cannot prove compiler dispatch, macro expansion, every cfg/feature enablement, or ownership semantics for every custom wrapper. These tool limitations remain open and visible, but they do not leave an unresolved libdictenstein production candidate: the campaign closes them with the non-filtering classic ledger, current source-SHA audit, manual ownership review, exact-RC2 all-features compilation, bounded reference parity, and current 100,000-depth execution and destruction gates.

The current evidence is stored under `target/campaign-logs/stack-safety-phase7/`: `recursion-ledger-source-hash-audit.tsv`, `all-features-lib-compile.log`, `production-100k-current.log`, `production-char-extreme-current.log`, and `production-overlay-worklists-current.log`. Tests redirected temporary files to the disk-backed `target/campaign-logs/tmp` directory rather than `/tmp`.

Systemic follow-up is routed to pgmcp work items rather than implemented here: classic receiver/type identity noise is tracked by `pgmcp-libcpg-classic-recursion-same-name-receiver-false-edges`; safe Rust resolver noise by `pgmcp-safe-rust-resolver-invents-recursion-across-distinct-receivers-and-generic-impls-3013f1`; recursive ownership by `libdictenstein-stack-safety-p2-lifecycle`.
