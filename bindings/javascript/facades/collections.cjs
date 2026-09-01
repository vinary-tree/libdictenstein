"use strict";

function cloneKey(key) {
  if (key instanceof Uint8Array) return new Uint8Array(key);
  if (key instanceof BigUint64Array) return new BigUint64Array(key);
  return key;
}
function ownedEntry(entry) { return Object.freeze([cloneKey(entry[0]), entry[1]]); }
const owned = Symbol("owned dictionary snapshot entries");
function disposeCursor(cursor) {
  if (typeof cursor.close === "function") { cursor.close(); return; }
  if (typeof Symbol.dispose === "symbol" && typeof cursor[Symbol.dispose] === "function") {
    cursor[Symbol.dispose]();
    return;
  }
  if (typeof cursor.return === "function") cursor.return();
}
function closeableCursor(cursor, fallbackSize) {
  let closed = false;
  const close = () => { if (!closed) { closed = true; disposeCursor(cursor); } };
  const result = {
    get size() { return cursor.size ?? fallbackSize; },
    get identity() { return cursor.identity ?? null; },
    next() {
      const item = cursor.next();
      if (item.done) close();
      return item.done ? item : { done: false, value: ownedEntry(item.value) };
    },
    nextBatch(maximum) {
      if (typeof cursor.nextBatch !== "function") {
        if (!Number.isSafeInteger(maximum) || maximum <= 0) throw new RangeError("batch size must be a positive safe integer");
        const batch = [];
        while (batch.length < maximum) {
          const item = this.next();
          if (item.done) break;
          batch.push(item.value);
        }
        return batch;
      }
      const batch = cursor.nextBatch(maximum).map(ownedEntry);
      if (batch.length === 0) close();
      return batch;
    },
    reduceBatches(reducer, initial, batchSize = 256) {
      let accumulator = initial;
      for (;;) {
        const batch = this.nextBatch(batchSize);
        if (batch.length === 0) return accumulator;
        accumulator = reducer(accumulator, batch);
      }
    },
    return(value) { close(); return { done: true, value }; },
    close,
    [Symbol.iterator]() { return this; },
  };
  if (typeof Symbol.dispose === "symbol") result[Symbol.dispose] = close;
  return result;
}
class DictionarySnapshot {
  #entries;
  constructor(entries, ownership) { this.#entries = Object.freeze(ownership === owned ? entries : entries.map(ownedEntry)); Object.freeze(this); }
  get size() { return this.#entries.length; }
  entries() { return this.#entries[Symbol.iterator](); }
  *keys() { for (const entry of this.#entries) yield entry[0]; }
  *values() { for (const entry of this.#entries) yield entry[1]; }
  [Symbol.iterator]() { return this.entries(); }
  forEach(callback, thisArg) { for (const [key, value] of this.#entries) callback.call(thisArg, value, key, this); }
  toMap() { return new Map(this.#entries); }
}
function collectionNamespace(namespace) {
  const targets = new WeakMap();
  const unwrap = (dictionary) => targets.get(dictionary) ?? dictionary;
  const wrap = (dictionary) => {
    const materialize = () => {
      const cursor = dictionary.entries();
      try {
        const exact = cursor.size;
        if (exact !== undefined && (!Number.isSafeInteger(exact) || exact < 0)) throw new RangeError("native exact entry count is not materializable");
        const entries = exact === undefined ? [] : new Array(exact);
        let count = 0;
        for (const entry of cursor) {
          if (exact === undefined) entries.push(ownedEntry(entry));
          else if (count < exact) entries[count] = ownedEntry(entry);
          else throw new Error("native exact entry count was not truthful");
          count += 1;
        }
        if (exact !== undefined && count !== exact) throw new Error("native exact entry count was not truthful");
        return new DictionarySnapshot(entries, owned);
      }
      finally { disposeCursor(cursor); }
    };
    let facade;
    facade = new Proxy(dictionary, { get(target, property) {
      switch (property) {
        case "snapshot": return materialize;
        case "streamEntries": return () => closeableCursor(target.entries(), target.size);
        case "entries": return () => materialize().entries();
        case "keys": return () => materialize().keys();
        case "values": return () => materialize().values();
        case "toMap": return () => materialize().toMap();
        case "forEach": return (callback, thisArg) => materialize().forEach((value, key) => callback.call(thisArg, value, key, facade));
        case "algebra": return (right, operation, valueMerge = "last") => wrap(target.algebra(unwrap(right), operation, valueMerge));
        case "union": return (right, valueMerge = "last") => wrap(target.union(unwrap(right), valueMerge));
        case "intersection": return (right, valueMerge = "lattice-meet") => wrap(target.intersection(unwrap(right), valueMerge));
        case "difference": return (right) => wrap(target.difference(unwrap(right)));
        case "symmetricDifference": return (right) => wrap(target.symmetricDifference(unwrap(right)));
        case Symbol.iterator: return () => materialize()[Symbol.iterator]();
        default: { const value = Reflect.get(target, property, target); return typeof value === "function" ? value.bind(target) : value; }
      }
    }});
    targets.set(facade, dictionary);
    return facade;
  };
  const result = {
    runtimeIdentity: namespace.runtimeIdentity,
    dynamicDawg: (...arguments_) => wrap(namespace.dynamicDawg(...arguments_)),
    doubleArrayTrie: (...arguments_) => wrap(namespace.doubleArrayTrie(...arguments_)),
    scdawg: (...arguments_) => wrap(namespace.scdawg(...arguments_)),
  };
  if (typeof namespace.createPersistentARTrie === "function") {
    result.createPersistentARTrie = (...arguments_) => wrap(namespace.createPersistentARTrie(...arguments_));
    result.openPersistentARTrie = (...arguments_) => wrap(namespace.openPersistentARTrie(...arguments_));
  }
  return Object.freeze(result);
}
module.exports = { DictionarySnapshot, collectionNamespace };
