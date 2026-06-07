# Overlay-backed `DictionaryNode` traversal (F7 BLOCKER-1)

## Problem

The persistent ART tries have a lock-free **overlay** representation
(`OverlayNode<K, V>`, `src/persistent_artrie_core/overlay/node.rs`). When
`route_overlay()` is `true` the overlay serves all reads and the **owned tree is
empty** (it is cleared at reestablish on reopen / create-flip).

`Dictionary::root()` returns a `DictionaryNode` (`is_final`/`transition`/`edges`/
`value`/`get_value`) that the zipper / Levenshtein transducer / fuzzy search drive.
Before this change `root()` always walked the **owned** tree, so under the overlay
it returned an EMPTY node — zipper / transducer / fuzzy saw an empty dictionary,
while `contains` / `get_value` / `iter_prefix` (which route to the overlay) stayed
correct. Both variants `log::warn!`ed "overlay-backed DictionaryNode traversal is
not yet implemented" (the "F7 BLOCKER-1").

## Goal

Make `root()` under `route_overlay()` return a `DictionaryNode` that navigates the
**overlay** lazily, so the graph walk works on a flipped trie. Additive +
reversible: the owned variant and its behavior are unchanged; the Overlay variant
is returned ONLY under `route_overlay()`.

## Why the overlay is fundamentally simpler (and safer) than the owned tree

The owned `DictionaryNode` holds **raw pointers** into trie-owned arena storage
plus a `pin` (epoch guard + `Arc` keepalive) because owned nodes live in storage
that eviction can reclaim and a reopened trie's children are *swizzled* (on-disk)
and must be faulted in (commit `549b068`).

The overlay is different in two load-bearing ways:

1. **Overlay nodes are `Arc<OverlayNode<K, V>>`** — reference-counted, immutable,
   owned. Holding the `Arc` in the node variant keeps the node (and its in-memory
   subtree) alive regardless of what happens to the trie: no dangling, no raw
   pointer, **no `unsafe`** for in-memory descent. Writers publish NEW root
   versions via a single root CAS; a held `Arc` snapshot is never mutated
   in place (persistent data structure), so a walk sees a consistent snapshot.

2. **The overlay is un-path-compressed** (one node per key unit, no buckets, no
   path-compression prefixes are *consumed* during navigation). Each `transition`
   consumes exactly one unit; `edges()` is one entry per child slot. This is
   simpler than the owned tree's bucket + path-compression traversal.

## On-disk (`Child::OnDisk`) overlay children — when do they occur?

A `Child::OnDisk(SwizzledPtr)` slot in a *live, reader-visible* overlay arises
**only** from overlay **eviction** (`evict_overlay_nodes`), which is
`#[cfg(feature = "bench-internals")]` / test-only, and is then faulted back via
`load_overlay_node_from_disk`.

- **char** HAS overlay eviction (bench/test) + the production read/write fault-in
  (`find_leaf_faulting`, `build_*_path_recursive`) via
  `load_overlay_node_from_disk` (`disk_io.rs`). So char OnDisk overlay children
  ARE reachable under the bench/test eviction driver, and the production point-read
  path already faults them in. The `DictionaryNode` walk MUST do the same (must not
  drop them — that would lose terms from traversal).

- **byte** has **no overlay eviction and no overlay fault-in** at all (there is no
  `evict_overlay_*` nor `load_overlay_node_from_disk` in `src/persistent_artrie/`).
  Byte's overlay point-read (`find_leaf_recursive`) treats an OnDisk child as
  absent. On reopen the overlay is reestablished fully **InMem** (the reestablish
  folds publish via `overlay_publish_*`, all `Child::InMem`). Therefore in byte a
  reader-visible routed overlay is **always fully InMem** and `Child::OnDisk` is
  unreachable on the read path.

  To keep byte and char symmetric and to *never silently drop* an OnDisk child
  (per the owner constraint), byte gains a fault-in primitive
  `load_overlay_node_from_disk` (the byte twin of char's — it reuses the existing
  byte v2 node decoder `serialization::v2::deserialize_node_v2` + `read_node_value`
  and produces an `Arc<OverlayNode<ByteKey, V>>` whose children stay `OnDisk`, i.e.
  single-level / lazy). The byte Overlay node carries an optional faulter so that
  *if* an OnDisk slot is ever encountered it is faulted in rather than dropped.

## Loading-context design (zero new `unsafe`)

The owner constraint is **no `unsafe` added; keep the existing `unsafe` block
counts byte-for-byte identical** (a strict set-equality gate over
`UNSAFE_INVENTORY.tsv` greps every `unsafe`/`unsafe impl`/`unsafe fn` line in
`src`). So the Overlay path must add ZERO `unsafe` lines.

The faulting context is therefore a **safe owned trait object**, NOT a raw pointer
+ pin (which is how the owned variant does it):

```text
trait OverlayFaulter<V>: Send + Sync {
    fn fault_overlay_slot(&self, slot: &SwizzledPtr) -> Option<Arc<OverlayNode<K, V>>>;
}
```

- char: `impl OverlayFaulter<V> for PersistentARTrieChar<V, S>` delegating to the
  existing `load_overlay_node_from_disk(slot).ok()`.
- byte: `impl OverlayFaulter<V> for PersistentARTrie<V, S>` delegating to the new
  byte `load_overlay_node_from_disk(slot).ok()`.

The Overlay node variant holds:

```text
Overlay {
    node:    Arc<OverlayNode<K, V>>,                 // owned snapshot — keeps subtree alive
    faulter: Option<Arc<dyn OverlayFaulter<V>>>,     // None ⇒ resident-only (OnDisk = absent)
}
```

`faulter` is an **owned `Arc<dyn ...>`**: cloning the node clones the `Arc`
(cheap), and the faulter (the trie) stays alive for the whole walk through this
owned handle — no pin, no epoch, no raw pointer, no `unsafe`. Because the faulter
holds the trie behind an owned `Arc`, the trie's allocation (and its
buffer/arena managers, behind their own `Arc`s) is alive whenever a fault-in is
attempted.

### Where the faulter comes from

A faulter requires an *owned* handle to the trie (to call
`load_overlay_node_from_disk`, which needs `&self` + the buffer/arena managers).

- The `Shared*ARTrie` paths (`Arc<RwLock<trie>>`) can build an
  `Arc<dyn OverlayFaulter<V>>` (the `Shared` wrapper *is* such an `Arc`). These are
  the only paths where eviction (hence OnDisk overlay children) is possible.
- The inherent `root(&self)` paths cannot capture an owned trie `Arc`, but on an
  owned (non-`Shared`) trie eviction is impossible, so the overlay is fully InMem
  and no faulter is needed (`faulter = None`).

`None` faulter ⇒ an encountered non-null OnDisk slot maps to "no transition" / is
skipped in `edges()` — but this is **unreachable** on these paths (proven by the
correspondence test, which checks the overlay walk equals `iter_prefix("")` /
the owned twin EXACTLY). It is the same conservative degrade the production
point-read uses when fault-in is unavailable (liveness-only, never UB, never a
fabricated term).

## Method semantics (both variants)

For the Overlay variant the `DictionaryNode` / `MappedDictionaryNode` methods map
directly onto the overlay node API:

- `is_final()` → `node.is_final()`.
- `value()` (MappedDictionaryNode) → `node.get_value()`. For `V = ()` membership
  finals carry no stored value (`get_value()` is `None`); the existing
  `MappedDictionaryNode` contract returns `None` there, matching the owned node
  (whose `value` is also `None` for membership).
- `transition(unit)` → `node.find_child(unit)`:
  - `Child::InMem(arc)` ⇒ `Some(Overlay { node: arc.clone(), faulter })`.
  - `Child::OnDisk(ptr)` (non-null) ⇒ fault via `faulter` ⇒
    `Some(Overlay { node: loaded, faulter })`; if no faulter / load fails ⇒ `None`.
  - null / absent ⇒ `None`.
  - char converts the `char` label to `u32` (overlay keys are `u32`); byte uses the
    `u8` directly.
- `edges()` → iterate `node.iter_children()`, mapping each non-null child (InMem
  direct, OnDisk faulted) to `(unit, Overlay { child, faulter })`. The collected
  `Vec` is preallocated to `node.num_children()`. char filters keys through
  `char::from_u32` (an unmappable scalar — impossible for real data — is skipped).
- `edge_count()` → `Some(node.num_children())`.

## Thread-safety argument

- In-memory descent holds owned `Arc<OverlayNode>` snapshots. The overlay node is
  immutable after publication; concurrent writers CAS a fresh root, never mutating
  a published node. A held snapshot is therefore a stable, consistent view (the
  same property `overlay_navigate` / the lock-free point reads rely on). No locks,
  no pin, no `unsafe`.
- OnDisk fault-in calls `load_overlay_node_from_disk` through the owned
  `Arc<dyn OverlayFaulter>` (the trie). That method takes `&self` and reads the
  arena via the buffer manager; the trie is alive through the owned `Arc`. The
  fault produces a fresh owned `Arc` and writes nothing to disk / advances no
  watermark (it is the same fault-in primitive the production read path uses).
- This is NOT the non-blocking read engine described in `flip.rs` ("DO NOT add
  disk fault-in") — that warning is about the lock-free read engine's *internal*
  DFS walks (`overlay_collect_*`) on the insert/len/iter hot path, where a faulting
  read racing a checkpoint that holds the buffer-manager lock deadlocked the soak.
  The `DictionaryNode` walk is an *external* read driven by the transducer, exactly
  analogous to the owned `DictionaryNode` faulting walk (`549b068`) and to the
  production point-read fault-in (`find_leaf_faulting`), both of which already
  fault on demand. char's overlay OnDisk path is bench/test-only; byte's is
  unreachable.

## Verification

1. New correspondence test
   `tests/persistent_overlay_traversal_correspondence.rs`: for several tries
   (Unicode for char, varied fan-out, multi-level, final empty string, term-only +
   valued entries) build the trie feature-on (overlay-routed) and a
   kill-switched-to-owned twin with the same data; DFS via `root()` + `edges()` +
   `transition()` + `is_final()` + `value()`, collecting `(term, is_final, value)`
   for ALL reachable terms, and assert the overlay traversal yields EXACTLY the
   same set as the owned twin AND as `iter_prefix("")`. Gold-standard proof that
   overlay traversal ≡ owned traversal.
2. Existing transducer / fuzzy / zipper / reopen-traversal tests.
3. Full suite feature-ON
   (`--features "persistent-artrie overlay-arbitrary-v parallel-merge"`).
4. Full suite feature-OFF (`--features persistent-artrie`).
5. `scripts/verify-formal-correspondence.sh` (incl. the strict unsafe-inventory
   set-equality gate — 0 unsafe added).

## Files changed

- `src/persistent_artrie_core/overlay/faulter.rs` (NEW): the safe `OverlayFaulter`
  trait (generic over `K`, `V`).
- `src/persistent_artrie_core/overlay/mod.rs`: register + re-export the trait.
- `src/persistent_artrie/node_impl.rs`: add the byte `Overlay` `NodeInner` variant
  + its method arms.
- `src/persistent_artrie/dictionary_traits.rs`: byte `root()` returns the Overlay
  node under `route_overlay()` (warn removed).
- `src/persistent_artrie/overlay_fault.rs` (NEW): byte `load_overlay_node_from_disk`
  fault-in primitive + `impl OverlayFaulter for PersistentARTrie`.
- `src/persistent_artrie/mod.rs`: register the new byte module.
- `src/persistent_artrie_char/mod.rs`: add the char `Overlay` arm to
  `PersistentARTrieCharNode` (new safe `overlay` + `overlay_faulter` fields) + its
  method arms; `root()` (inherent + `Shared`) returns the Overlay node under
  `route_overlay()`.
- `src/persistent_artrie_char/overlay_fault.rs` (NEW): `impl OverlayFaulter for
  PersistentARTrieChar` (delegates to the existing `load_overlay_node_from_disk`) +
  the `SharedOverlayFaulter` newtype for the `Arc<RwLock<..>>` form.
- `src/persistent_artrie_char/overlay_dictionary_node_faulting_tests.rs` (NEW,
  `#[cfg(test)]`): in-crate proof the overlay `DictionaryNode` faults evicted OnDisk
  children in (in-crate because the overlay-eviction driver is `pub(crate)`).

## Results (verification run — all green)

- **New correspondence test** `tests/persistent_overlay_traversal_correspondence.rs`
  (11 tests): PASS feature-on AND feature-off. Proves the overlay walk term/value
  set == `iter()`/`iter_with_values()` (the `iter_prefix("")` oracle) for byte +
  char across membership, valued, Unicode, varied fan-out, multi-level, empty-string
  final, and empty-dictionary; == owned-twin walk for char (see the byte asymmetry
  below); `edge_count()` == `edges().len()`.
- **In-crate OnDisk-fault test**
  `persistent_artrie_char::overlay_dictionary_node_faulting_tests` (1 test): PASS
  feature-on AND feature-off. After real cold overlay eviction to `Child::OnDisk`,
  the faulting overlay `DictionaryNode` walk recovers EVERY term (cold faulted in +
  live resident); the no-faulter walk degrades to the resident subset (no fabricated
  term, no panic, every live term present); the faulting walk recovers strictly more
  than the no-faulter walk (real OnDisk work); `transition`-descent of a cold term
  faults its spine in.
- **Existing traversal/zipper/reopen/EBR tests** (19): PASS feature-on
  (`dictionary_node_reopen_traversal_correspondence`, `persistent_char_ebr_*`,
  `zipper_language_correspondence` incl. the persistent zipper case,
  `root_descriptor_reopen_*`).
- **Full suite feature-ON** (`--features "persistent-artrie overlay-arbitrary-v
  parallel-merge"`): 2639 passed, 0 failed, 3 skipped. **Full lib subset incl. the
  new in-crate test:** 1767 passed.
- **Full suite feature-OFF** (`--features persistent-artrie`): 2624 passed, 0
  failed, 3 skipped.
- **Formal gate** `scripts/verify-formal-correspondence.sh`: exit 0 (unsafe-inventory
  set-equality gate matches; Rocq builds 0 Admitted / 0 Axiom; TLA specs parse; all
  in-gate Rust correspondence tests pass).
- **Unsafe added: ZERO.** No actual `unsafe` construct exists in any new/changed file
  (the word "unsafe" appears only in comments); the char node retains exactly its 11
  inventoried unsafe lines (owned arm, byte-for-byte unchanged). The strict
  `verify-unsafe-boundary-inventory.sh` set-equality gate passes.

### Pre-existing finding surfaced (NOT introduced here): byte owned-walk gap

The test surfaced that the **byte OWNED `DictionaryNode` walk is pre-existingly
incomplete** (orthogonal to this overlay work; the owned-arm code is unchanged): the
byte trie stores deeper terms in buckets, and the byte node's bucket/children
traversal does not fully expand bucket suffixes through the trait surface, so an owned
byte walk of `{a, ab, abc, b, cat, cats}` yields only `{a, b, cat}` (and its `value()`
returns `None` for owned nodes — the value codec is unavailable at that layer). The
CHAR owned walk is complete (bucketless trie + the `549b068` faulter). The new OVERLAY
byte walk is complete and correct — it is in fact a strict superset of the deficient
owned byte walk. Because of this, the byte owned-twin comparison in the test is `⊇`
(term-set superset) with the authoritative equivalence asserted against
`iter()`/`iter_with_values()`; the char owned-twin comparison is `==`. Fixing the
byte owned `DictionaryNode` walk is a separate follow-up, out of scope for F7
BLOCKER-1 (which is about the OVERLAY walk).
