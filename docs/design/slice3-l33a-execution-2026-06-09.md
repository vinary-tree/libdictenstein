# L3.3a execution ledger (2026-06-09)

**Stage:** L3.3a — the first keystone deletion (the "L2.2-absorbed-into-L3.3" surface).
Per `slice3-level3-converged-plan-2026-06-08.md` §LEVEL 2 (L2.2 forced to L3.3) + §LEVEL 3.

**Goal of L3.3a:** delete the kill-switch / `OverlayWriteMode` / owned-checkpoint-arm surface so
the lock-free overlay is the SOLE production representation, and retire the owned-white-box test
corpus that exercised the deleted machinery. The owned *readers / converters / root field / holder
types / loaders* remain (deleted at L3.3b/L3.3c).

## Production deletions (src)

- **`route_overlay()` body** (`persistent_artrie_core/overlay/flip.rs`): `overlay_write_mode().uses_overlay()
  && lockfree_root().is_some()` → **`self.lockfree_root().is_some()`**. Since `kill_switch_to_owned`
  (the only writer of `OwnedTree` mode) is gone, an installed overlay ALWAYS implies overlay routing.
- **`OverlayWriteMode` enum** (`persistent_artrie_core/overlay/write_mode.rs`): **deleted** (file removed +
  `mod write_mode;` removed). The `U8Enum`/`AtomicEnumCell` infra stays (durability still uses it).
- **`kill_switch_to_owned`** (flip.rs default + byte/char inherent delegators) + **`wal_stamp_owned_regime`**
  (trait decl + byte/char impls): **deleted**. `flip_to_overlay`/`install_prebuilt_overlay_root` dropped
  their `set_overlay_write_mode(LockFreeOverlay)` (now redundant — overlay-install ⇒ routed).
- **`overlay_write_mode()` / `set_overlay_write_mode()`** seams (trait decls + byte/char impls + byte/char
  inherent) + the **`overlay_write_mode` field** (byte `dict_impl.rs`, char `mod.rs`) + **all ~13 ctor
  inits** (byte/char mmap_ctor + io_uring_ctor) + the **`::new()` mode-set** (byte/char mmap_ctor):
  **deleted**.
- **Shared checkpoint owned arm** (`persistent_artrie_core/overlay/checkpoint.rs`): `checkpoint_route_split`
  collapsed to overlay-only (capture immutable overlay → publish retaining; `debug_assert!(route_overlay())`
  documents the invariant). The RES-4 total-loss footgun is now STRUCTURALLY gone (no owned arm to
  mis-select). Trait decls `capture_owned_snapshot` / `publish_owned_and_reclaim` **deleted**.
- **Byte owned-arm checkpoint** (`persistent_artrie/overlay_checkpoint.rs`): `capture_owned_snapshot`,
  `publish_owned_and_reclaim` (inherent + trait impl), and **`serialize_root`** (its sole caller)
  **deleted**. `serialize_root_value_bytes` KEPT (the live iterative overlay serializer uses it).
- **`compact()`** (`persistent_artrie/compaction_impl.rs`): the owned-staging else-arm
  (`kill_switch_to_owned` + `insert_impl_no_wal` loop + owned `checkpoint()`) **deleted**; the CX
  path-compressing serialize is now **unconditional**. `compaction_snapshot` value-read → overlay
  unconditional. The in-place reopen no longer references the Owned→Overlay converter.

## Test corpus retirement

- **In-crate `#[cfg(test)]` (src):** retired the kill-switch/OverlayWriteMode white-box tests
  (`overlay_write_mode.rs` byte+char, `lockfree_cas.rs` byte+char, `overlay_correspondence_tests.rs`
  gutted to the overlay-only deep-key test, `overlay_routing_tests.rs` gutted to the overlay-only tests,
  `dict_impl.rs` byte+char — doc-tx tests CONVERTED to overlay (C2), `test_version_tracking` deleted
  (owned write-lock MVCC), recovery test CONVERTED, `f5_loader.rs` byte+char format-3 differential tests
  deleted, `mmap_ctor.rs`/`persist.rs`/`mod.rs` char owned-conversion/reestablish tests deleted, char
  mod.rs DictionaryNode-traversal tests CONVERTED to overlay).
- **Integration tests (`tests/`):** 8 files **deleted** (pure owned machinery: `*_owned_to_overlay_*`,
  `*_lazy_mutation_*`, `*_lockfree_merge_*` [owned drain], `*_char_ebr_*` [owned-walk], `*_char_eviction_proptest`
  [owned-rep], `*_bulk_mutation_*`, `*_e1_readflip_*`, `*_overlay_traversal_*` [covered by the converted
  `dictionary_node_reopen_traversal` + char-mod traversal tests]). Per-test deletes across ~13 broad suites
  (the specific owned tests: owned drains, negative-decrement, owned per-record recovery, owned dirty-flag,
  owned archive, value-level CAS, `DurabilityPolicy::None`, i64 doc-tx). `dictionary_node_reopen_traversal`
  CONVERTED (the DictionaryNode walk reads the overlay post-L3.2). `l32_new_overlay` gutted to the still-valid
  `::new()`-overlay-routed assertions.
- **Formal gate** (`scripts/verify-formal-correspondence.sh`): removed the 5 dead test-target hooks for the
  deleted owned-machinery suites (with a note).

## Verification

- `cargo test --no-run --features persistent-artrie` + `cargo test --all-features --no-run`: **0 errors**.
- `cargo build --no-default-features`: **0 errors**.
- Full suite `cargo nextest run --features persistent-artrie`: **2644/2645 pass**; the 1 "timeout" is the
  pre-existing `persistent_lockfree_durable_loom` watermark loom test, CPU-starved under the 16-thread
  parallel run — **passes in 2.4s in isolation** (not a regression).
- `scripts/verify-unsafe-boundary-inventory.sh`: **exit 0** (ZERO unsafe delta — only deletions of safe code).
- `scripts/verify-formal-correspondence.sh`: re-run after the hook prune (see commit gate).
- `cargo fmt`: clean.

## Residual (expected mid-campaign — deleted at L3.3b/L3.3c)

Dead-code warnings remain for the owned readers/mutators/holders that L3.3b/c remove:
`insert_impl_no_wal`, `serialize_node_to_disk`, the byte zipper helpers (`bucket_has_path` etc.),
`next_lsn_at_capture` (byte CheckpointSnapshot field, now set-but-unread). `cargo build` does not
`-D warnings`; clippy `-D warnings` is pre-existing-red (600+ issues in untouched zipper/DAT/vocab files).

## L3.3c precondition recorded

Building a legacy **format-3** (owned `StringBucket`-suffix) on-disk file in-process is no longer
possible (kill-switch deleted). L3.3c's BLOCKER-#4 graceful-fallback test (an unparseable image →
in-loader `Err`→empty+WAL, NOT a corrupt read) must therefore use a **checked-in format-3 binary
fixture** rather than build one at runtime.
