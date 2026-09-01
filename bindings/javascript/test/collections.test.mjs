import assert from "node:assert/strict";
import test from "node:test";

import { collectionNamespace } from "../facades/collections.mjs";
import { runCollectionTraversalProfile } from "../benchmarks/collection-traversal-profile.mjs";

function compare(left, right) {
  const count = Math.min(left.length, right.length);
  for (let index = 0; index < count; index += 1) {
    if (left[index] !== right[index]) return left[index] - right[index];
  }
  return left.length - right.length;
}

function fakeNamespace(counters = { opened: 0, closed: 0 }) {
  return {
    runtimeIdentity: { provider: "fake" },
    dynamicDawg() {
      const values = [];
      return {
        put(key, value = null) {
          values.push([new Uint8Array(key), value]);
          return true;
        },
        entries() {
          counters.opened += 1;
          const ordered = values.slice().sort((left, right) => compare(left[0], right[0]));
          let index = 0;
          let closed = false;
          return {
            size: ordered.length,
            identity: { producer: 1n, revision: 1n },
            next() {
              if (closed || index === ordered.length) return { done: true, value: undefined };
              return { done: false, value: ordered[index++] };
            },
            nextBatch(maximum) {
              if (closed) return [];
              const result = ordered.slice(index, index + maximum);
              index += result.length;
              return result;
            },
            close() { if (!closed) { closed = true; counters.closed += 1; } },
            [Symbol.iterator]() { return this; },
          };
        },
        close() {},
      };
    },
    doubleArrayTrie() { return this.dynamicDawg(); },
    scdawg() { return this.dynamicDawg(); },
  };
}

test("ordinary protocols materialize and close; explicit streams cancel", () => {
  const counters = { opened: 0, closed: 0 };
  const dictionary = collectionNamespace(fakeNamespace(counters)).dynamicDawg("byte");
  dictionary.put(new Uint8Array([2]), 2n);
  dictionary.put(new Uint8Array([1]), null);

  assert.deepEqual(Array.from(dictionary, ([key, value]) => [key[0], value]), [[1, null], [2, 2n]]);
  assert.equal(counters.opened, 1);
  assert.equal(counters.closed, 1);
  const snapshot = dictionary.snapshot();
  assert.equal(snapshot.size, 2);
  assert.equal(snapshot.toMap().size, 2);

  const stream = dictionary.streamEntries();
  assert.equal(stream.next().done, false);
  assert.deepEqual(stream.return(), { done: true, value: undefined });
  assert.equal(counters.opened, 3);
  assert.equal(counters.closed, 3);
});

test("algebra operands are unwrapped and results retain collection protocols", () => {
  const raw = (entries) => ({
    size: entries.length,
    entries() {
      let index = 0;
      return {
        size: entries.length,
        next: () => index < entries.length
          ? { done: false, value: entries[index++] }
          : { done: true, value: undefined },
        close() {},
        [Symbol.iterator]() { return this; },
      };
    },
    close() {},
  });
  const firstRaw = raw([["cat", 1n]]);
  const secondRaw = raw([["dog", 2n]]);
  const unionRaw = raw([["cat", 1n], ["dog", 2n]]);
  firstRaw.union = (right, policy) => {
    assert.equal(right, secondRaw);
    assert.equal(policy, "last");
    return unionRaw;
  };
  const namespace = collectionNamespace({
    runtimeIdentity: Object.freeze({}),
    dynamicDawg: () => firstRaw,
    doubleArrayTrie: () => secondRaw,
    scdawg: () => raw([]),
  });
  const first = namespace.dynamicDawg();
  const second = namespace.doubleArrayTrie([]);
  const union = first.union(second);
  assert.deepEqual([...union], [["cat", 1n], ["dog", 2n]]);
  assert.equal(union.snapshot().size, 2);
});

for (const arm of ["materialized", "stream", "stream-cancel"]) {
  test(`benchmark ${arm} emits the common schema`, () => {
    const row = runCollectionTraversalProfile(collectionNamespace(fakeNamespace()), {
      runtime: "javascript-test",
      arm,
      entries: 16,
      passes: 2,
      warmupPasses: 1,
      batchSize: 4,
      earlyCancel: 3,
    });
    assert.equal(row.schema, "libdictenstein.host-collection-traversal.v1");
    assert.equal(row.arm, arm);
    assert.equal(row.consumed_entries_per_pass, arm === "stream-cancel" ? 3 : 16);
    assert.equal(row.checksum > 0, true);
  });
}
