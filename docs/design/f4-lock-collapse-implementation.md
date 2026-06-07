# F4 — The Lock Collapse: Implementation Record

**Status:** IMPLEMENTED (working tree; not committed — owner reviews + commits).
**Scope:** byte (`SharedARTrie`) + char (`SharedCharARTrie`) ONLY. `SharedVocabARTrie`
is OUT OF SCOPE (a distinct struct with its own `RwLock`, unchanged).
**Design source (CONVERGED, 5 red-team rounds):**
`docs/design/phase-f-g5-delete-owned-tree.md` §3.5 + `phase-f-g5-v9-converged-plan.md`
§5 / V10.4 / V11.2 / V11.3 / V11.4.

---

## 1. What changed

`SharedARTrie<V,S>` and `SharedCharARTrie<V,S>` went from `Arc<RwLock<…>>` to a bare
`Arc<…>` — the **outer trie `RwLock` is deleted**. Overlay reads AND writes are now
fully lock-free: the now-`&self` mutators route to lock-free CAS internally, and the
only operations that still need mutual exclusion take dedicated INNER locks under the
hard, acyclic hierarchy

```
    CK  >  merge_lock  >  OR  >  EC
```

| Lock | Field | Role |
|------|-------|------|
| **CK** | `checkpoint_lock: Arc<Mutex<()>>` (pre-existing, F3) | serialize concurrent checkpoints |
| **merge_lock** | `merge_lock: Arc<Mutex<()>>` (NEW, F4-ready, mirrors CK) | serialize merge‖merge |
| **OR** | `root: RwLock<TrieRoot<V>>` / `RwLock<CharTrieRoot<V>>` | the dormant owned path (kill-switch + WAL-replay); checkpoint-capture read |
| **EC** | `eviction_coordinator: Mutex<Option<Arc<…>>>` | eviction-coordinator slot — a **LEAF** (never held across CK/merge_lock/OR or a worker join) |

Backward compatibility is preserved by the **compat shim**
(`src/persistent_artrie_core/shared_access.rs`): an extension trait
`SharedTrieAccess` adds `.read()` / `.write()` to the collapsed `Arc<T>` handles,
each returning a transparent `TrieAccessGuard` that `Deref`s to `&T` — **there is no
lock**; both `read()` and `write()` hand back `&T`. Every historical
`handle.read()/.write()` call site (~270 in-repo + the `liblevenshtein-rust` sibling)
compiles unchanged, dispatching to the now-`&self` methods through the guard.

---

## 2. The complete Tier-1 / Tier-2 field disposition (the F3 obligation)

Every `&mut self` inherent/trait method on both tries was classified. **Tier-2** =
reachable on the shared handle ⇒ method becomes `&self`, field wrapped for interior
mutability. **Tier-1** = pre-share configuration only (never on a `Shared*` API) ⇒
stays `&mut self`, NO wrap.

### Tier-2 fields wrapped (byte = 5, char = 7; + `merge_lock` ⇒ byte=6/char=8 per V11.2)

| Field | byte | char | Wrapper | Why interior-mutable |
|-------|------|------|---------|----------------------|
| `root` | ✓ | ✓ | `RwLock<TrieRoot/CharTrieRoot>` (OR) | `&self` owned mutators + owned-checkpoint capture |
| `eviction_coordinator` | ✓ | ✓ | `Mutex<Option<Arc<…>>>` (EC leaf) | `&self` enable/disable (already on `EvictableARTrie`) |
| `overlay_write_mode` | ✓ | ✓ | `AtomicEnumCell<OverlayWriteMode>` | `&self` `kill_switch_to_owned`; hot `route_overlay()` read |
| `durability_policy` | ✓ | ✓ | `AtomicEnumCell<DurabilityPolicy>` | `&self` `set_durability_policy`; write-path read |
| `dirty_prefixes` | ✓ | — | `Mutex<HashSet<Vec<u8>>>` | `&self` owned-mutator dirty tracking (byte only) |
| `memory_monitor` | — | ✓ | `Mutex<Option<Arc<…>>>` | subsystem family, uniform (char only) |
| `checkpoint_manager` | — | ✓ | `Mutex<Option<Arc<…>>>` | subsystem family, uniform (char only) |
| `group_commit` (cfg) | — | ✓ | `Mutex<Option<Arc<…>>>` | subsystem family, uniform (char only) |
| `merge_lock` | ✓ | ✓ | `Arc<Mutex<()>>` (NEW) | merge‖merge serializer (V11.2) |

`AtomicEnumCell<E: U8Enum>` (in `shared_access.rs`) backs the two `Copy`-enum fields
with a single `AtomicU8` — a lock-free `&self` load/store, **cheaper** than the old
`RwLock`-guarded field read. NO `UnsafeCell`, NO new `unsafe`.

### Tier-1 methods (stay `&mut self`, pre-share only — verified on NO `Shared*` trait)

- `enable_lockfree`, `flip_to_overlay`, `reestablish_overlay_*`, `clear_owned`
  (overlay installers; `lockfree_root`/`lockfree_cache` whole-`Option` assigned — no wrap)
- `increment` / `fetch_add` / `increment_via_value_cas` / `try_increment_impl_no_wal`
  (counter path; removed from the `ARTrie` trait at C1; only ever on owned counter
  tries — V11.4 sweep B)
- `merge_from` / `merge_replace` / `merge_from_batched*` (inherent; only on owned
  tries in practice — the `Shared*`-reachable merge driver is `union_with` /
  `merge_from_parallel`, which DO take `merge_lock`)

### Tier-2 methods converted `&mut self` → `&self`

byte+char: `insert`, `insert_with_value`, `remove`, `upsert`, `get_or_insert`,
`remove_prefix`, `remove_prefix_batched`, `sync`, `checkpoint`, `persist_to_disk`,
`mark_clean`, `set_durability_policy`, `set_overlay_write_mode`,
`kill_switch_to_owned`, the owned-mutator core (`insert_impl*`, `remove_impl*`,
`upsert_impl_no_wal`, `try_*_impl_no_wal`, `evict_node_at_path`), `record_dirty_path`,
`clear_dirty_tracking_state`, `propagate_dirty_to_root`; char subsystem
enable/disable/query (`enable/disable_memory_monitor`, `enable/disable_group_commit`,
`enable/disable_epoch_checkpointing`, `force_epoch_checkpoint`, `merge_entries`).
Trait-level: the `ARTrie` / `Dictionary` / `MappedDictionary` / `MutableMappedDictionary`
/ `EvictableARTrie` impls on the `Shared*` aliases (drop the now-vestigial
`self.write()` exclusion; route to the `&self` inherent methods through the shim).

### Reentrancy-safe owned mutators (NO re-lock)

The byte owned mutators recurse (bucket→ART conversion + retry) and the char owned
mutators do raw-pointer walks. parking_lot is non-reentrant, so naively wrapping each
with `self.root.write()` would self-deadlock. Resolution: the `&self` public mutator
acquires the OR guard ONCE and delegates to a private helper taking `root: &mut
TrieRoot` (byte: `insert_into_root` / `remove_from_root` / `convert_root_bucket_to_art`
/ `find_parent_in_root`) or operating under the single held guard (char: the
`try_*_impl_no_wal` raw-pointer walks now anchor exclusivity on the held OR guard
instead of the old `&mut self`).

---

## 3. The owned-read path (NO new `unsafe`)

The owned readers (`navigate_to_prefix*`, `owned_try_get`, `owned_try_contains`,
`DictionaryNode::from_trie`) returned `&self`-lifetime borrows into `self.root`. With
`root` now behind the OR `RwLock`, that borrow would have to outlive a read-guard
temporary. Resolved **without any new `unsafe`**:

- `owned_root_guard()` returns a `parking_lot::MappedRwLockReadGuard<CharTrieNodeInner>`
  (the guard travels with the borrow).
- `navigate_to_prefix_from<'s,'g>(&'s self, root: &'g …, …) where 's: 'g` — the deeper
  `get_child_lazy` walk returns `&self`-tied refs that COERCE to the guard lifetime
  `'g` (covariance: `'s: 'g`). The caller holds the guard across the collect.
- `get` / `try_get` / `owned_get` / `owned_try_get` now return **owned `Option<V>`**
  (clone). Every caller already `.cloned()`/`.copied()`/reads the value, so this is
  net-zero churn and the lock-correct shape (a `&V` borrow can't outlive the OR guard).

The unsafe-boundary inventory (`verify-unsafe-boundary-inventory.sh`) is **byte-
identical** to the committed ledgers — **0 new `unsafe`**.

---

## 4. Drop-before-join sites (the deadlock discipline, V11.3)

EC (and the subsystem-family mutexes) are LEAVES: NEVER held across a worker
`.join()`. Every join site uses the statement-temporary
`let x = self.field.lock().take(); /* guard dropped */ x.shutdown()/drop(x);`:

1-2. `disable_eviction` (byte `shared_trait_impl.rs`, char `mod.rs`)
3-4. `close()` (byte `dict_impl.rs`, char `mod.rs` — runs on every `Drop`)
5. `disable_group_commit` (char `observability.rs`)
6. `disable_memory_monitor` (char `observability.rs` — its callback can re-enter the trie)
7. `disable_epoch_checkpointing` (char `epoch_checkpointing.rs`)
8. `enable_*` re-arm (take-old-then-drop-guard-then-drop-old, V11.3 #9)
9. the eviction callback (`evict_char_nodes` / byte `enable_eviction`) + `force_eviction`:
   clone the coordinator out under a BRIEF EC lock, release EC, THEN take OR (order OR>EC).

The owned-arm checkpoint takes CK (via the `Shared*` wrapper) then OR-read for capture
(`capture_snapshot` / `capture_owned_snapshot`). The old write→read `downgrade` is
DELETED with the outer lock it used (NF-2). The char C2 assert is `!route_overlay()`
(RES-4), matching the trait.

---

## 5. Verification results (all green)

| # | Step | Result |
|---|------|--------|
| 1 | `cargo build --features "persistent-artrie parallel-merge"` / `persistent-artrie` | clean (0 errors) |
| 2 | **F4 deadlock-freedom loom** (`persistent_lockfree_f4_lock_hierarchy_loom`) | 3 models, 0 failed — NO deadlock, NO lost write |
| 2 | `LockFreeDurableCheckpoint.tla` (TLC) | No error, 2810 states (no-lost-write / no-writer-exclusion) |
| 3 | `ConcurrentCheckpointSerialization.tla` (TLC) | No error, 21 states; negative control `_Unsafe` correctly VIOLATES `NoTornDescriptor` |
| 4 | full suite `--features "persistent-artrie parallel-merge"` | 2644 passed, 3 skipped, 0 failed |
| 5 | full suite `--features persistent-artrie` | 2639 passed, 3 skipped, 0 failed |
| 6 | concurrent soak (`persistent_f4_lock_collapse_soak`, ~22s under `timeout 90`) | completed (no hang), no lost write, survives reopen |
| 7 | `scripts/verify-formal-correspondence.sh` | exit 0 (SANY + correspondence + unsafe inventory match) |
| 8 | cross-repo `liblevenshtein-rust` build (read-only) | 0 errors — shim preserves the API; sibling unmodified |

**0 new `unsafe`** (inventory matches ledger). NO stubs/TODOs. The durability proof
(`checkpoint_lsn = committed watermark`, Acquire-watermark-before-root-load) is
PRESERVED — the collapse deletes a lock the proof never relied on.
