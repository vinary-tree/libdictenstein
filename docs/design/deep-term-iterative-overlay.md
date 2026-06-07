# Deep-term safety: making the lock-free overlay's recursions iterative (flag-1)

## Problem

The lock-free overlay (`OverlayNode<K,V>`) stores its spine **un-path-compressed** — one
node per key unit (one `OverlayNode` per `u8` byte / per `u32` codepoint). This is by
design: a one-node-per-unit spine keeps the CAS-publish path simple (no prefix-split race).
But it means a term of length *L* builds an `Arc`-linked structure *L* levels deep.

Three operations walked that spine **recursively**, so a long term (the stress test uses
500 chars) overflowed the thread stack:

1. **Insert** — `build_value_path_recursive` (already fixed earlier; made iterative).
2. **Checkpoint serialize** — overflowed FIRST (at `dict.checkpoint()`), see Site B.
3. **Drop** — the compiler-generated recursive drop of the `Arc<OverlayNode>` chain, see
   Site A.

`test_stress_large_terms` (`tests/persistent_artrie_stress.rs`) SIGABRT'd with
`fatal runtime error: stack overflow` under the overlay before this fix.

## Site A — iterative `Drop` for the shared `OverlayNode` (commit 289401f)

`src/persistent_artrie_core/overlay/node.rs`. One custom `Drop` on the **shared** node
fixes byte + char + vocab. It flattens the descent onto a heap worklist via the safe
`Arc::try_unwrap` pattern:

- Sole owner of a child `Arc` ⇒ `try_unwrap` yields the node by value; drain ITS children
  onto the worklist; the node drops with an empty store (re-entrant `drop` finds nothing →
  at most one extra frame, never a chain).
- Shared `Arc` ⇒ it just drops (refcount−−); the eventual last owner dismantles it.

No node freed while referenced (no UAF), none twice, none leaked — driven purely by `Arc`
refcounting. **Zero `unsafe`.** The `Drop` must be `impl<K, V>` (E0367: a `Drop` impl
cannot add a `V: Clone` bound the struct lacks), so its helpers are bound-free: a new
`impl<K, V> ChildStore` block adds `empty_inline` (`ChildStore::new` delegates to it),
`take` (`mem::replace` with empty), and `drain_in_mem_into` (moves `Child::InMem` `Arc`s
out — no clone, so refcounts are unchanged: the property the reclaim/leak witnesses depend
on). Validated by `reclaim_tests` (`strong_count == 1` after drop;
`many_supersessions_over_a_deep_path_do_not_accumulate_leaks`; `prop_reclaim_is_lossless`).

## Site B — iterative overlay-checkpoint serialize (commit 78fa6d2)

`src/persistent_artrie/overlay_checkpoint.rs` (byte) and `src/persistent_artrie_char/persist.rs`
(char). **Option (i):** serialize the overlay DIRECTLY, iteratively, never materializing the
deep intermediate owned tree (`overlay_root_to_owned` byte / `overlay_to_inner` char) — which
killed the conversion recursion, the intermediate-tree drop, AND the serializer recursion at
once.

The drivers (`serialize_overlay_subtree_iterative` byte / `serialize_overlay_to_disk_iterative`
char) run an explicit **post-order** work-stack:

- Each frame holds the overlay node, the in-mem children still to descend (queued in
  **reverse** `iter_children()` order so `pop()` yields them **ascending**), and the
  `(edge, disk_ptr)` child slots resolved so far (on-disk children pre-filled).
- Descend into in-mem children (ascending); when a frame's children are all resolved, build
  the owned single node with those child ptrs and serialize it via the **existing
  non-recursive** per-node serializer (`serialize_node_to_disk_with_value` byte /
  `serialize_one_char_node_to_disk` char — the latter factored out of the recursive walk so
  both paths encode identically); bubble the node's disk ptr up to the parent's slot.

### THE load-bearing invariant: byte-identical on-disk image

The arena allocates slots **in serialize order**, so the on-disk `SwizzledPtr`s depend on
the order nodes are serialized. The iterative walk reproduces the recursive DFS's order
exactly — **post-order (children before parent), children visited ascending** — so the arena
slot IDs, every disk pointer, and the whole image are **byte-identical**. The char walk also
maintains the key `path` (push-before-descend / pop-on-finish, symmetric) so the
eviction-registry paths match. The image-equivalence correspondence tests
(`persistent_compaction_correspondence`, `persistent_char_node_layout_correspondence`,
`relative_encoding_correspondence`, `dictionary_node_reopen_traversal_correspondence`,
`root_descriptor_reopen_correspondence`, `persistent_end_to_end_trace_correspondence`, …)
are the guard. Root branches + the root-value (`?`) vs child-value (`.ok()`) asymmetry + the
empty-`""` round-trip (H1/H2) are reproduced. `count_overlay_finals` is also iterative.
**Zero `unsafe` added** (byte 0→0, char 1→1).

Vocab needs no fix: it checkpoints the bounded-depth owned `VocabTrieNode` tree (the overlay
spine is merged into it before checkpoint).

## Verification

`test_stress_large_terms` (500-char terms) PASS both feature directions (was SIGABRT). Full
suite feature-on 2628 / feature-off 2613 (0 failures), reclaim 13/13 both directions,
formal-correspondence gate exit 0 (unsafe inventory matches ledgers).
