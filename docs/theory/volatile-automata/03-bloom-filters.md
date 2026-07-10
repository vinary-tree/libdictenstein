# Bloom filters

**Navigation**: [← Double-array tries](02-double-array-tries.md) · [Bit-parallel child scan →](04-bit-parallel-child-scan.md)

A **Bloom filter** is a probabilistic set membership test: it answers "is `x` in the set?" with
either *definitely no* or *probably yes*, using far less space than storing the set. This document
develops the false-positive arithmetic, states libdictenstein's chosen parameters
([`src/bloom_filter.rs`](../../../src/bloom_filter.rs)), and — importantly — gives an honest account
of the filter's limited role on the current read path. Notation follows [`docs/notation.md`](../../notation.md).

## The structure

A Bloom filter is a bit array of `m` bits, initially zero, with `k` independent hash functions
$`h_1, \dots, h_k`$ each mapping an element to a bit position in $`\{0, \dots, m-1\}`$
(Bloom 1970, [10.1145/362686.362692](https://doi.org/10.1145/362686.362692)).

- **Insert `x`**: set the bits $`h_1(x), \dots, h_k(x)`$ to 1.
- **Query `x`**: if *any* of $`h_1(x), \dots, h_k(x)`$ is 0, `x` is **definitely not** in the set; if
  *all* are 1, `x` is **probably** in the set.

There are no false negatives — a real member set all its bits — but there can be **false positives**:
another element's bits may collectively cover `x`'s bit positions.

## False-positive probability

After inserting `n` elements into `m` bits with `k` hash functions, the probability a given bit is
still 0 is $`(1 - 1/m)^{kn} \approx e^{-kn/m}`$, so the false-positive rate — all `k` of a
non-member's bits happening to be set — is approximately:

```math
p \approx \left(1 - e^{-kn/m}\right)^{k}
```

For a fixed ratio $`m/n`$ of bits per element, `p` is minimized by choosing

```math
k^{*} = \frac{m}{n}\ln 2,
```

which gives the well-known rule of thumb: about $`m/n \approx 10`$ bits per element with the optimal
`k` yields roughly a 1% false-positive rate.

## libdictenstein's parameters

[`src/bloom_filter.rs`](../../../src/bloom_filter.rs) fixes:

- **`k = 3`** hash functions — three `FxHash` evaluations with distinct seeds.
- **$`m/n \approx 10`$** bits per element (~1.2 bytes/element).
- Bit storage as a `Vec<u64>` (64-bit words), so a query short-circuits on the first zero word/bit.

With `k = 3` and $`m/n = 10`$ the formula gives $`p \approx (1 - e^{-0.3})^3 \approx 0.017`$ — on the
order of the ~1% the module targets. (The theoretical optimum at $`m/n = 10`$ is $`k \approx 7`$; the
crate trades a little accuracy for fewer hash evaluations per query, which matters when the filter is
consulted on a hot path.)

## Honest note: the filter is *off* the DAWG read path

A Bloom pre-filter is attractive for a dictionary because a *negative* lookup — a query that is not a
stored term — could be rejected in $`O(1)`$ without walking the trie. libdictenstein once used it that
way, and the API still *accepts* a bloom capacity: `DynamicDawg::with_config(threshold,
bloom_capacity)`. **But the live lock-free read path does not consult a Bloom filter.** The current
`contains` performs an **exact wait-free traversal** ([architecture](../../architecture/in-memory-dictionaries.md)),
and `with_config`'s `bloom_filter_capacity` argument is **vestigial — accepted for API compatibility
and ignored on the read path** (see
[dynamic-dawg.md](../../algorithms/implementations/dynamic-dawg.md) for the exact behavior). The
`BloomFilter` type remains in the crate, is covered by
[`tests/bloom_filter_correspondence.rs`](../../../tests/bloom_filter_correspondence.rs) (including
under loom), and is documented here for its theory and because the capacity knob still exists — not
because it accelerates lookups today.

## When a Bloom pre-filter *does* pay

The technique is genuinely valuable when negative lookups dominate and the underlying test is
expensive (a disk seek, a network hop, a large-fanout scan). For an in-memory trie whose exact
membership test is already a cache-friendly $`O(\lvert q\rvert)`$ walk, the constant-factor win from
a pre-filter is small and can be *negative* once the filter's own hashing and the cost of maintaining
it under mutation are counted — which is why the exact path won here. The right mental model: a Bloom
filter trades space and a small false-positive rate for a cheap *reject*, and it earns its keep only
when the thing it lets you skip is much more expensive than the filter itself.

## References

- Bloom, B. H. (1970). *Space/time trade-offs in hash coding with allowable errors.* Communications
  of the ACM 13(7). [10.1145/362686.362692](https://doi.org/10.1145/362686.362692)
