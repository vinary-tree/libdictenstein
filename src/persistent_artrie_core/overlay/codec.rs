//! CX (task #43) — path-compressing overlay↔dense codec: the SHARED, K-generic, PURE
//! **no-truncation core**.
//!
//! The owner mandate (2026-06-08) for the path-compressing codec is: KEEP compression, but PROVE it
//! never truncates / loses key data. This module isolates the single load-bearing pure function —
//! [`chain_chunks`] — that splits a single-child chain's edge-unit string into the vertical stack of
//! dense chain-nodes, and carries the **NO-TRUNCATION invariant** as an exhaustively-property-tested
//! contract: the concatenation of every emitted chunk's `prefix ++ [edge]`, top-to-bottom, equals the
//! input EXACTLY — for ANY input length and ANY prefix width.
//!
//! Keeping it pure + variant-agnostic (operates on `&[U]`, no `OverlayNode`, no disk) means the
//! no-truncation property is validated in isolation, complementing the Rocq `chunk_concat_id` theorem
//! (T2 in docs/design/cx-task43-codec-design-2026-06-08.md). The per-variant serialize/load (peel,
//! emit, expand) live in the byte/char `persist.rs` and consume this.
//!
//! DORMANT / REVERSIBLE: nothing in production calls this yet (L2/L3 wire the codec later).

/// One emitted dense **chain-node**: a path-compressed node on a single-child chain. It stores
/// `prefix` (the path units it absorbs, at most `max_prefix` of them) and exactly one outgoing
/// `edge` (the unit selecting its single child — the next chain-node below it, or the terminus).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ChainChunk<'a, U> {
    /// The prefix units this node absorbs (`len <= max_prefix`).
    pub prefix: &'a [U],
    /// The single outgoing edge unit (selects this node's one child).
    pub edge: U,
}

/// Split a single-child chain's **edge-unit string** `units` into the vertical stack of
/// [`ChainChunk`]s, ordered **top-first** (stack\[0\] is reached from the chain head's parent;
/// stack\[last\].edge points at the terminus).
///
/// Each chain-node packs at most `max_prefix` prefix units PLUS one outgoing edge unit, so the chunk
/// width is `max_prefix + 1`; the last (bottom) chunk may be shorter. A chunk of `w` units becomes a
/// node with `prefix = chunk[..w-1]` (so `prefix.len() <= max_prefix`) and `edge = chunk[w-1]`.
///
/// # NO-TRUNCATION CONTRACT (task #43 owner mandate)
/// For ALL `units` and ALL `max_prefix`, the concatenation of every returned chunk's
/// `prefix ++ [edge]`, in order, equals `units` exactly — no unit is dropped, duplicated, or
/// reordered. There is **no `min`, no `firstn MAX`, no truncation** anywhere in this function: it is
/// `slice::chunks` (a total partition) plus a last-element split. Returns an empty `Vec` iff `units`
/// is empty (a chain head that is itself the terminus). `max_prefix` may be 0 (degenerate: each node
/// is one bare edge, `prefix = []`).
pub(crate) fn chain_chunks<U: Copy>(units: &[U], max_prefix: usize) -> Vec<ChainChunk<'_, U>> {
    // Each node holds <= max_prefix prefix units + exactly 1 edge unit.
    let width = max_prefix + 1;
    // Preallocate the exact number of chunks (best practice: we know the count up front).
    let mut chunks = Vec::with_capacity(units.len().div_ceil(width));
    for chunk in units.chunks(width) {
        // `slice::chunks` never yields an empty chunk, so `len() >= 1` and the split is total.
        let (prefix, last) = chunk.split_at(chunk.len() - 1);
        chunks.push(ChainChunk {
            prefix,
            edge: last[0],
        });
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reassemble the original unit string from the chunk stack (the inverse of `chain_chunks`'s
    /// split) — the operational witness of the NO-TRUNCATION contract.
    fn reassemble<U: Copy>(chunks: &[ChainChunk<'_, U>]) -> Vec<U> {
        let mut out = Vec::new();
        for ch in chunks {
            out.extend_from_slice(ch.prefix);
            out.push(ch.edge);
        }
        out
    }

    /// **THE no-truncation regression: exhaustive.** For every chain length 0..=128 and every prefix
    /// width 0..=14 (covers char `MAX_PREFIX_LEN=6`, byte `=12`, and the boundaries on either side),
    /// the chunk stack round-trips the input EXACTLY, every prefix respects the width cap, and the
    /// node count is the optimal `ceil(L / (max_prefix+1))`.
    #[test]
    fn chain_chunks_never_truncates_exhaustive() {
        for max_prefix in 0..=14usize {
            let width = max_prefix + 1;
            for len in 0..=128usize {
                // Distinct units so any drop/duplicate/reorder is detectable.
                let units: Vec<u32> = (0..len as u32).map(|i| i.wrapping_mul(2_654_435_761)).collect();
                let chunks = chain_chunks(&units, max_prefix);

                // (1) NO-TRUNCATION: concat(prefix ++ [edge]) == units, exactly.
                assert_eq!(
                    reassemble(&chunks),
                    units,
                    "truncation/loss at len={len} max_prefix={max_prefix}"
                );

                // (2) Every prefix respects the format cap (so CharCompressedPrefix::from_chars and
                //     the prefix_len<=MAX_PREFIX_LEN header validation can never overflow/panic).
                for ch in &chunks {
                    assert!(
                        ch.prefix.len() <= max_prefix,
                        "prefix {} exceeds cap {} at len={len}",
                        ch.prefix.len(),
                        max_prefix
                    );
                }

                // (3) Optimal node count = ceil(L / (max_prefix+1)); 0 for the empty chain.
                let expected = if len == 0 { 0 } else { len.div_ceil(width) };
                assert_eq!(
                    chunks.len(),
                    expected,
                    "node count at len={len} max_prefix={max_prefix}"
                );

                // (4) Total path units covered = sum(prefix.len()+1) = len (cross-check of (1)).
                let covered: usize = chunks.iter().map(|c| c.prefix.len() + 1).sum();
                assert_eq!(covered, len, "covered != len at len={len} max_prefix={max_prefix}");
            }
        }
    }

    /// Boundary spot-checks with concrete expected splits (char `max_prefix=6`, so width 7).
    #[test]
    fn chain_chunks_boundaries_char_width7() {
        let mp = 6; // char MAX_PREFIX_LEN

        // Empty chain → no nodes.
        assert!(chain_chunks::<u32>(&[], mp).is_empty());

        // 1 unit → one bare-edge node (prefix empty).
        let u1: Vec<u32> = vec![10];
        let c1 = chain_chunks(&u1, mp);
        assert_eq!(c1.len(), 1);
        assert_eq!(c1[0].prefix, &[] as &[u32]);
        assert_eq!(c1[0].edge, 10);

        // Exactly width (7) → ONE node: prefix = first 6, edge = 7th.
        let u7: Vec<u32> = (0..7).collect();
        let c7 = chain_chunks(&u7, mp);
        assert_eq!(c7.len(), 1);
        assert_eq!(c7[0].prefix, &[0, 1, 2, 3, 4, 5]);
        assert_eq!(c7[0].edge, 6);

        // width+1 (8) → TWO nodes: [6+edge] then [bare edge].
        let u8v: Vec<u32> = (0..8).collect();
        let c8 = chain_chunks(&u8v, mp);
        assert_eq!(c8.len(), 2);
        assert_eq!(c8[0].prefix, &[0, 1, 2, 3, 4, 5]);
        assert_eq!(c8[0].edge, 6);
        assert_eq!(c8[1].prefix, &[] as &[u32]);
        assert_eq!(c8[1].edge, 7);

        // 3*MAX_PREFIX_LEN+1 = 19 units → ceil(19/7) = 3 nodes; round-trips.
        let u19: Vec<u32> = (0..19).collect();
        let c19 = chain_chunks(&u19, mp);
        assert_eq!(c19.len(), 3);
        assert_eq!(reassemble(&c19), u19);
    }
}
