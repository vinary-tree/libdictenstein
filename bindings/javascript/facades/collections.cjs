"use strict";

function cloneKey(key) {
  if (key instanceof Uint8Array) return new Uint8Array(key);
  if (key instanceof BigUint64Array) return new BigUint64Array(key);
  return key;
}
function ownedEntry(entry) { return Object.freeze([cloneKey(entry[0]), entry[1]]); }
const owned = Symbol("owned dictionary snapshot entries");
function closeableCursor(cursor) {
  let closed = false;
  const close = () => { if (!closed) { closed = true; cursor.close(); } };
  const result = {
    get size() { return cursor.size; },
    get identity() { return cursor.identity; },
    next() {
      const item = cursor.next();
      if (item.done) close();
      return item.done ? item : { done: false, value: ownedEntry(item.value) };
    },
    nextBatch(maximum) {
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
      finally { cursor.close(); }
    };
    let facade;
    facade = new Proxy(dictionary, { get(target, property) {
      switch (property) {
        case "snapshot": return materialize;
        case "streamEntries": return () => closeableCursor(target.entries());
        case "entries": return () => materialize().entries();
        case "keys": return () => materialize().keys();
        case "values": return () => materialize().values();
        case "toMap": return () => materialize().toMap();
        case "forEach": return (callback, thisArg) => materialize().forEach((value, key) => callback.call(thisArg, value, key, facade));
        case Symbol.iterator: return () => materialize()[Symbol.iterator]();
        default: { const value = Reflect.get(target, property, target); return typeof value === "function" ? value.bind(target) : value; }
      }
    }});
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
