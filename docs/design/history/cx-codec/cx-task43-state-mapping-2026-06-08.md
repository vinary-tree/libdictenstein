# Task #43 (CX) — Path-Compressing Overlay↔Dense Codec State Mapping

**Purpose:** Map the EXACT current state needed to design the path-compressing overlay↔dense codec that will enable L2/L3 to delete the owned tree without regressing on-disk size.

**Date:** 2026-06-08  
**Focus:** Char variant (byte/vocab parallels noted)  
**Scope:** Read-only structural mapping; no edits.

---

## 1. THE OVERLAY NODE TYPE — `OverlayNode<K, V>`

**Location:** `/home/dylon/Workspace/f1r3fly.io/libdictenstein/src/persistent_artrie_core/overlay/node.rs:649-687`

### Struct Definition

```rust
pub struct OverlayNode<K: KeyEncoding, V = ()> {
    /// Monotonic version counter (incremented on each modification)
    version: AtomicU64,
    /// Durable on-disk location stamp (SwizzledPtr::to_raw()), 0 = none
    serial_disk_ptr: AtomicU64,
    /// Tiered child storage (Inline for 0-4 children, Heap for 5+)
    store: ChildStore<K, V>,
    /// Node flags (IS_FINAL, IS_DIRTY, IS_LEAF, HAS_VALUE)
    flags: AtomicU8,
    /// Value for final nodes (arbitrary V, immutable at node construction)
    value: Option<V>,
    /// Compressed prefix for path compression
    prefix: Arc<[K::Unit]>,
    /// Length of the valid prefix (may be less than prefix.len())
    prefix_len: u8,
}
```

### Child Storage Enum

**Location:** `/home/dylon/Workspace/f1r3fly.io/libdictenstein/src/persistent_artrie_core/overlay/node.rs:118-123`

```rust
pub enum Child<K: KeyEncoding, V = ()> {
    /// An in-memory child node, owned by Arc (reclaimed via refcount on drop).
    InMem(Arc<OverlayNode<K, V>>),
    /// An on-disk reference to a serialized subtree (a swizzled block location).
    OnDisk(SwizzledPtr),
}
```

### Key Properties

- **NO path compression in current overlay**: The overlay builds via `add_child_growing()` which leaves `prefix_len=0` and `prefix=[]` on every node (by design).
- **One node per unit**: Each key-unit (u8 byte, u32 char) gets its own OverlayNode tier. No collapsed single-child chains.
- **Child storage is tiered**: Inline (0–4 children, no heap) → Heap (5+, Vec-backed). Both store owned `Child` enums.
- **Value is immutable**: Set at node construction via `with_value(V)` or `as_final()`, then never mutated. Fits arbitrary `V` (unlike the u64-only prior overlay).
- **Prefix field exists but unused in the round-trip**: `prefix_len=0` always during overlay→inner conversion (lines 1646–1649 in persist.rs note this).

### Char Type Alias

```rust
// From persistent_artrie_char/mod.rs
pub type PersistentCharNode<V = ()> = OverlayNode<CharKey, V>;
```

---

## 2. THE DENSE ON-DISK NODE FORMAT — `CharTrieNodeInner<V>` + `CharNode` + Serialization

**Location:** `/home/dylon/Workspace/f1r3fly.io/libdictenstein/src/persistent_artrie_char/types.rs:468-474`

```rust
pub struct CharTrieNodeInner<V: DictionaryValue> {
    /// The adaptive radix node structure (N4/N16/N48/Bucket)
    pub node: CharNode,
    /// Optional value associated with this node
    pub value: Option<V>,
}
```

### Serialized Char Node Header

**Location:** `/home/dylon/Workspace/f1r3fly.io/libdictenstein/src/persistent_artrie_char/serialization_char.rs:117-135`

```rust
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SerializedCharNodeHeader {
    pub magic: [u8; 4],          // "ARC\0"
    pub version: u8,              // Format version
    pub node_type: u8,            // 104 (N4), 116 (N16), 148 (N48), 101 (Bucket)
    pub flags: u8,                // is_final, is_dirty, is_leaf
    pub reserved: u8,
    pub num_children: u16,        // Number of children
    pub prefix_len: u8,           // Compressed prefix length (0–6 chars for char ART)
    pub _padding: u8,
    pub data_size: u32,           // Size of type-specific data (children, keys, etc.)
}
// Total: 16 bytes
```

### Serialization Format (With Prefix)

**Location:** `/home/dylon/Workspace/f1r3fly.io/libdictenstein/src/persistent_artrie_char/serialization_char.rs:1–50`

```
┌─────────────────────────────────────────────────┐
│ SerializedCharNodeHeader (16 bytes)             │
├─────────────────────────────────────────────────┤
│ CharCompressedPrefix (variable, if prefix_len>0)│
│ - up to 6 u32 chars (24 bytes max)             │
├─────────────────────────────────────────────────┤
│ Type-Specific Data (variable)                   │
│ - CharNode4:    keys[u32; 4] + children[u64; 4]│
│ - CharNode16:   keys[u32; 16] + children[u64; 16]
│ - CharNode48:   bitmap[u8; 256] + children[u64; 48]
│ - CharBucket:   entry_count + (key, child) pairs
├─────────────────────────────────────────────────┤
│ Value Blob (variable, if value.is_some())      │
│ - bincode::<V> serialized bytes                │
└─────────────────────────────────────────────────┘
```

**Key Insight:** The prefix is stored as **raw char bytes** (4-byte u32 per char) within the data section, indexed by `prefix_len`. When deserialized, a multi-char prefix is a SINGLE node with `prefix_len > 0` — it collapses what would be a chain of single-child OverlayNodes into ONE dense node.

---

## 3. PATH-COMPRESSION ALGORITHM IN OWNED TREE

### Byte Variant

**Location:** `/home/dylon/Workspace/f1r3fly.io/libdictenstein/src/persistent_artrie/path_compression.rs`

- Single-child chains are compressed into the parent node's prefix field.
- When a divergent key is inserted below a prefix node, the prefix is split: the common prefix stays in the parent, the remaining prefix moves to a new child node, and the divergent byte gets its own child.

### Char Variant

**Location:** `/home/dylon/Workspace/f1r3fly.io/libdictenstein/src/persistent_artrie_char/serialization_char.rs:1–50` (format definition)

- Same concept as byte, scaled to u32 char keys.
- Prefix storage: `CharCompressedPrefix` (24 bytes = 6 chars max per the `CHAR_MAX_PREFIX_LEN`).
- Serialization reads `node.header().prefix_len` to extract the valid prefix bytes.
- Deserialization reconstructs the prefix array and sets `node.header_mut().set_prefix(...)`.

**NO split/merge functions currently in-tree**: The path-compression only appears in **serialization/deserialization** (reading/writing the on-disk format). The owned tree is built via the in-memory APIs (which do not use prefixes), then serialized with prefixes added by the serializer (if single-child chains are detected).

---

## 4. EXISTING OVERLAY→DENSE SERIALIZE PATH

### Converter: `overlay_to_inner<V>`

**Location:** `/home/dylon/Workspace/f1r3fly.io/libdictenstein/src/persistent_artrie_char/persist.rs:1584–1621`

```rust
fn overlay_to_inner<V>(node: &super::nodes::PersistentCharNode<V>) -> CharTrieNodeInner<V>
where
    V: DictionaryValue,
{
    let mut inner = CharTrieNodeInner::<V>::default();
    inner.node.header_mut().set_final(node.is_final());
    inner.value = node.get_value();
    for (&key, child) in node.iter_children() {
        if let Some(child_arc) = child.as_in_mem() {
            let child_inner = overlay_to_inner::<V>(child_arc);
            let child_ptr = SwizzledPtr::in_memory(Box::into_raw(Box::new(child_inner)));
            if let Some(grown) = inner.node.add_child_growing(key, child_ptr)
                .expect("add in-memory child within capacity")
            {
                inner.node = grown;
            }
        } else if let Some(on_disk) = child.as_on_disk() {
            // On-disk overlay children: reuse verbatim
            if !on_disk.is_null() {
                if let Some(grown) = inner.node.add_child_growing(key, on_disk.clone())
                    .expect("add on-disk child within capacity")
                {
                    inner.node = grown;
                }
            }
        }
    }
    inner
}
```

**CRITICAL:** The converter is RECURSIVE and does NOT path-compress. Each node recursively converts its in-mem children; the resulting `CharTrieNodeInner` tree has the EXACT same structure as the overlay: one node per key-unit, `prefix_len=0` on every node.

### Serializer: `serialize_char_node_to_disk`

**Location:** `/home/dylon/Workspace/f1r3fly.io/libdictenstein/src/persistent_artrie_char/persist.rs:1066–1130`

- RECURSIVE walk of `CharTrieNodeInner` (NOT the overlay).
- Collects child disk pointers.
- Calls shared NON-recursive [`serialize_one_char_node_to_disk`] (lines 1131–1200) to encode the per-node bytes.
- **Does NOT add path compression** (no prefix collapsing in the write).

**Phase-B Design Note (Line 1647):** "the overlay round-trip path produces empty prefixes."

### Current Checkpoint Flow

**Location:** `/home/dylon/Workspace/f1r3fly.io/libdictenstein/src/persistent_artrie_char/persist.rs:300–400` (capture_snapshot_immutable)

```
Overlay (in-memory, uncompressed)
    ↓ overlay_to_inner (recursive, uncompressed)
CharTrieNodeInner (owned temp, uncompressed)
    ↓ serialize_char_node_to_disk (non-recursive per-node encoder)
Dense On-Disk (CURRENTLY uncompressed; prefix_len=0 on every node)
```

**Consequence:** The current on-disk checkpoint image from the overlay has the SAME node-count and structure as the uncompressed overlay. No space savings from path compression.

---

## 5. EXISTING DENSE→OVERLAY LOAD PATH (F5 — Fault-in loader)

> **⚠️ CORRECTION (re-red-team #2, 2026-06-08): §5's "CURRENT ASSUMPTION" below + the DELTA-table row
> "load_char_node_from_disk_lazy() reads prefix_len correctly" are FALSE.** Verified: the lazy fault
> loader (`disk_io.rs:357-378`) reads `is_final`/`value`/children but **NEVER reads
> `char_node.prefix()`/`header.prefix_len`** — the prefix is DROPPED. The byte twin
> (`overlay_fault.rs:99`) explicitly builds `OverlayNode::new()` with the comment "prefix is always
> empty for the overlay." So the EXISTING fault-in path is prefix-lossy, and the CX loader CANNOT reuse
> it for compressed images — it must EXPAND `prefix_len>0` at the single-node fault granularity (see the
> codec-design doc's "re-red-team #2" / Finding 4A). The codec-design doc (its §"Ground truth" + Load
> section) is authoritative.

### Lazy Deserializer: `load_char_node_from_disk_lazy`

**Location:** `/home/dylon/Workspace/f1r3fly.io/libdictenstein/src/persistent_artrie_char/disk_io.rs:296–412`

```rust
pub(super) fn load_char_node_from_disk_lazy(&self, bm: &Arc<RwLock<BufferManager<S>>>,
    disk_ptr: &SwizzledPtr) -> Result<CharTrieNodeInner<V>>
{
    // Read from arena, deserialize into CharTrieNodeInner
    let inner = /* deserialize_char_node_v2(...) */;
    // Children stay as SwizzledPtr (disk or in-mem), NOT recursively loaded
    Ok(inner)
}
```

**CURRENT ASSUMPTION:** The deserializer reads `prefix_len` from the header and reconstructs the prefix bytes **AS-IS** from the serialized data. If a node on disk has `prefix_len=3`, the loader sets the CharNode's prefix to those 3 chars.

### Fault-in Converter: `load_overlay_node_from_disk`

**Location:** `/home/dylon/Workspace/f1r3fly.io/libdictenstein/src/persistent_artrie_char/disk_io.rs:383–412`

```rust
pub(super) fn load_overlay_node_from_disk(&self, disk_ptr: &SwizzledPtr)
    -> Result<Arc<super::nodes::PersistentCharNode<V>>>
{
    let inner = self.load_char_node_from_disk_lazy(bm, disk_ptr)?;
    Ok(Arc::new(super::persist::inner_to_overlay::<V>(&inner)))
}
```

### Converter: `inner_to_overlay<V>`

**Location:** `/home/dylon/Workspace/f1r3fly.io/libdictenstein/src/persistent_artrie_char/persist.rs:1655–1686`

```rust
pub(super) fn inner_to_overlay<V>(inner: &CharTrieNodeInner<V>)
    -> super::nodes::PersistentCharNode<V>
where V: DictionaryValue,
{
    let prefix_len = inner.node.header().prefix_len as usize;
    let mut node = if prefix_len > 0 {
        super::nodes::PersistentCharNode::<V>::with_prefix(
            inner.node.prefix().as_slice(prefix_len)
        )
    } else {
        super::nodes::PersistentCharNode::<V>::new()
    };
    
    if inner.is_final() { node = node.as_final(); }
    if let Some(v) = inner.value.clone() { node = node.with_value(v); }
    
    for (key, ptr) in inner.node.iter_children() {
        if !ptr.is_null() {
            node = node.with_child(key, super::nodes::persistent_node::Child::OnDisk(ptr.clone()));
        }
    }
    node
}
```

**CRITICAL:** If the loaded inner node has `prefix_len > 0`, the converter DOES build an OverlayNode with a non-empty prefix via `with_prefix()`. **But the current serializer never produces `prefix_len > 0`**, so the overlay always gets `prefix_len=0`.

**Design Intent (Lines 1646–1649):** "the overlay representation that `overlay_to_inner` serializes never path-compresses (it builds via `add_child_growing`, which leaves the prefix empty), so on the round-trip the prefix is empty; we still propagate any non-empty prefix faithfully so the builder is a total inverse."

---

## 6. L2/L3 CALL SITES THAT NEED THE CODEC

### L2.1: Compaction (`compact()` — byte only)

**Location:** `/home/dylon/Workspace/f1r3fly.io/libdictenstein/src/persistent_artrie/compaction_impl.rs:110–370`

Current flow:
```
1. Capture overlay snapshot
2. Kill-switch to owned tree (OverlayWriteMode::KillSwitched)
3. Build staging trie (insert_impl_no_wal loop)
4. Checkpoint staging (serialize_root)
5. Atomic file rename
```

**L2 design (docs/design/slice3-level3-converged-plan-2026-06-08.md:142–146):**
```
1. Capture overlay snapshot (unchanged)
2. serialize_overlay_snapshot_compressed (CX.1) → temp file
3. Atomic file rename (unchanged)
```

**Direct dependency:** compaction will call the new `serialize_overlay_snapshot_compressed()` codec function to emit a path-compressed dense image.

### L3.1: Reopen-Scratch (`load_root_immutable_seam`)

**Location:** `/home/dylon/Workspace/f1r3fly.io/libdictenstein/src/persistent_artrie_core/overlay/flip.rs:1557` (mentioned in the plan)

Current byte flow:
```
load_root_from_disk() → TrieRoot enum (owned)
    ↓
build_overlay_root_from_owned() → OverlayNode (uncompressed)
```

Current char flow:
```
load_char_node_from_disk_lazy() → CharTrieNodeInner (owned temp)
    ↓
inner_to_overlay() → PersistentCharNode (uncompressed)
```

**L3 design:**
```
load_overlay_root_compressed() → PersistentCharNode directly
(no TrieRoot scratch, no build_overlay_root_from_owned)
```

**Direct dependency:** reopen will call `load_overlay_root_compressed()` to load the path-compressed dense image and reconstruct an overlay WITHOUT materializing the owned tree.

---

## 7. EXISTING CODEC SCAFFOLD

### None currently in code. But:

- The `overlay_to_inner` / `inner_to_overlay` converters are the STRUCTURAL inverses needed.
- The serialization format infrastructure (`SerializedCharNodeHeader`, `serialize_one_char_node_to_disk`, `load_char_node_from_disk_lazy`) is in place.
- The relative encoding module (`relative_encoding.rs`) handles variable-width child pointers (not yet path-compressed prefixes).

**New code needed (CX.1–CX.3):**
- `serialize_overlay_snapshot_compressed()` (byte)
- `load_overlay_root_compressed()` (byte)
- `serialize_char_snapshot_compressed()` (char)
- `load_overlay_char_root_compressed()` (char)
- Plus vocab parallels if needed.

---

## 8. FORMAL VERIFICATION STRUCTURE & PRECEDENTS

### Existing Verification Files

**Location:** `/home/dylon/Workspace/f1r3fly.io/libdictenstein/formal-verification/`

**Rocq Proofs (.v):**
- `WorkerLifecycle.v` — single file covering unsafe boundary.
- No standalone `MapRefinement.v` or `SerializationCorrespondence.v` yet.

**TLA+ Models (.tla):**
- `ConcurrentCheckpointSerialization.tla` (285 LOC, TLC passed) — checkpoint publication invariants.
- `ConcurrentCheckpointPublication.tla` (285 LOC, TLC passed) — concurrent checkpoint queue.
- `PersistentEndToEndTrace.tla` (121 LOC, TLC passed) — operation trace refinement.
- **No OverlayDenseCodecRoundTrip.tla yet** (mentioned in CX plan as "optional").

### Correspondence Test Harness

**Location:** `/home/dylon/Workspace/f1r3fly.io/libdictenstein/tests/persistent_artrie_formal_correspondence.rs` (and scripts/verify-formal-correspondence.sh)

- Tests like `immutable_checkpoint_correspondence` (persist.rs:1712–1890) prove overlay checkpoint ≡ owned checkpoint on reopen.
- Tests like `overlay_faultin_load_roundtrip` (persist.rs:1891+) prove `load(serialize(overlay_to_inner(n))) ≡ n`.

### Proof Obligations for CX Codec

From plan (line 188–191):

**CX correspondence proof structure:**
1. **`load(serialize(overlay))` round-trip:** Prove the new `load_overlay_root_compressed()` is the inverse of `serialize_overlay_snapshot_compressed()`.
   - Input: overlay root with arbitrary `V`, valued/empty-string/membership terms, deep key-path.
   - Output: reloaded overlay ≡ input overlay (finality, value, child-set identical).

2. **Back-compat load:** Prove the new loader can read legacy owned-tree checkpoints (uncompressed or with path compression from the old `serialize_root`).
   - Input: on-disk image produced by the current `serialize_root()` or new `serialize_overlay_snapshot_compressed()`.
   - Output: reloaded overlay ≡ the snapshot that produced it.

3. **Byte-identity / density:** Prove the new serializer produces the SAME bytes as the old `serialize_root()` when both compress the same uncompressed tree.
   - **Risk:** If owned `serialize_root` adds path compression but current overlay→owned→serialize does not, the new codec MUST collapse the single-child chains to match owned's density.

### Recommended Proof Approach

1. **TLA+ (optional):** New `OverlayDenseCodecRoundTrip.tla` model covering:
   - Serialize an overlay with multi-unit prefixes → Dense image.
   - Deserialize dense image → Overlay.
   - Verify finality/value/children match source.

2. **Rocq (recommended):** Build on existing correspondence structure:
   - New lemma: `overlay_compressed_roundtrip_correct : ∀ overlay, load_compressed(serialize_compressed(overlay)) = overlay`.
   - New lemma: `overlay_compressed_vs_owned : serialize_compressed(overlay) =_bytes serialize_root(owned) when they encode the same terms`.

3. **Correspondence tests (mandatory):**
   - `test_overlay_compressed_roundtrip()` — overlay → serialize → load → overlay identity.
   - `test_overlay_vs_legacy_owned()` — new loader can read old owned checkpoints.
   - `test_compressed_density_matches_owned()` — byte count (or on-disk size) equals old serializer's.

---

## CRITICAL DESIGN CONSTRAINTS

### Constraint 1: No Path Compression in Current Overlay

The overlay stores one `OverlayNode` per key-unit. **The codec MUST synthesize path-compressed prefixes during serialization** by detecting single-child chains and collapsing them. This is the **core transformation** of CX.

### Constraint 2: Single-Node Deserialization

The `inner_to_overlay()` converter (1655–1686 in persist.rs) is already capable of handling a node with `prefix_len > 0`. **The codec's deserializer MUST expand a multi-char prefix into a CHAIN of OverlayNodes** (one per char) so the resulting overlay has the correct structure for further inserts/deletes.

**Example:**
- Disk: `CharNode { prefix="abc", prefix_len=3, children={…} }`
- Load: 
  - Node 'a' → Node 'b' → Node 'c' → (original children)
  - All intermediate nodes have `prefix_len=0, value=None, is_final=false`.

### Constraint 3: Back-Compat with Owned Checkpoints

The new loader MUST be able to read:
1. Current overlay checkpoints (all `prefix_len=0`).
2. Old owned checkpoints (may have `prefix_len > 0` from `serialize_root` if it path-compresses).
3. New compressed overlays (may have mixed `prefix_len` in different nodes).

### Constraint 4: Byte-Identity / Space Equivalence

**Red-team requirement (plan line 138):** prove the new serializer produces byte-identical or size-equivalent output to the existing `serialize_root()` on the same term set. This is the **density gate** — proof that deleting the owned tree doesn't regress on-disk footprint.

---

## SUMMARY TABLE: Key Code Locations

| Component | File | Lines | Role |
|-----------|------|-------|------|
| **OverlayNode struct** | overlay/node.rs | 649–687 | In-mem uncompressed node type |
| **Child enum** | overlay/node.rs | 118–123 | InMem(Arc) \| OnDisk(SwizzledPtr) |
| **CharTrieNodeInner** | persistent_artrie_char/types.rs | 468–474 | Owned temp/intermediate node |
| **SerializedHeader** | persistent_artrie_char/serialization_char.rs | 117–135 | 16-byte disk header w/ prefix_len |
| **overlay_to_inner** | persistent_artrie_char/persist.rs | 1584–1621 | Recursive uncompressed overlay→owned |
| **serialize_char_node** | persistent_artrie_char/persist.rs | 1066–1130 | Per-node recursive serializer (no compression) |
| **serialize_one_char_node** | persistent_artrie_char/persist.rs | 1131–1200 | Per-node non-recursive encoder (shared core) |
| **inner_to_overlay** | persistent_artrie_char/persist.rs | 1655–1686 | Single-node owned→overlay converter (supports prefix!) |
| **load_char_node_lazy** | persistent_artrie_char/disk_io.rs | 296–412 | Deserializer (reads prefix_len correctly) |
| **load_overlay_node_from_disk** | persistent_artrie_char/disk_io.rs | 383–412 | Fault-in: load→inner→overlay |
| **Compaction entry** | persistent_artrie/compaction_impl.rs | 110–370 | L2.1 call site (currently owns state, kill_switch) |
| **Reopen seam** | persistent_artrie_core/overlay/flip.rs | 1557 | L3.1 call site (root loader) |
| **Correspondence tests** | persistent_artrie_char/persist.rs | 1712–2000+ | Existing round-trip + back-compat gate |
| **Path compression (byte)** | persistent_artrie/path_compression.rs | — | Single-child chain collapse (algo, not impl) |
| **Relative encoding** | persistent_artrie_char/relative_encoding.rs | 1–250 | Child pointer varint encoding (not for prefixes yet) |

---

## DELTA: Current Overlay→Dense vs. Needed Codec

| Aspect | Current (Uncompressed) | Needed (CX Codec) |
|--------|------------------------|------------------|
| Overlay→Dense | `overlay_to_inner()` recursive, then `serialize_char_node_to_disk()` | Detect single-child chains; emit collapsed prefix in one node |
| Dense→Overlay | `load_char_node_from_disk_lazy()`, then `inner_to_overlay()` | **Expand** multi-char prefix into chain; reconstruct original overlay structure |
| Prefix in produced image | `prefix_len=0` on every node | `prefix_len ∈ [0, K::MAX_PREFIX_LEN]` per node (multi-unit compression) |
| Node count on disk | Same as overlay (1 per unit) | Fewer (single-child chains collapsed) |
| Space savings | None | Matches owned `serialize_root()` density |
| UNSAFE | 0 (overlay is Arc-safe) | 0 (build via `with_child`, no raw pointers) |

---

## NEXT STEPS FOR TASK #43

1. **CX.1 (Byte Serializer):** Implement `serialize_overlay_snapshot_compressed()`.
   - Walk overlay root iteratively (existing iterative traverse pattern).
   - On entering a node, check if single child: if so, extend prefix; else emit node with accumulated prefix.
   - Use `serialize_one_char_node_to_disk()` core to write per-node bytes.

2. **CX.2 (Byte Loader):** Implement `load_overlay_root_compressed()`.
   - Load root node (via `load_char_node_from_disk_lazy()`).
   - If `prefix_len > 0`, expand into chain of overlay nodes.
   - Recursively fault-in children as needed.

3. **CX.3 (Char Twins):** Repeat for char variant.

4. **Proof:** Correspondence test + optional TLA+; back-compat + byte-identity gates.

5. **Red-Team:** Every node size, every V type, empty string, deep terms, owned-vs-new density match.

---

**END OF MAPPING**
