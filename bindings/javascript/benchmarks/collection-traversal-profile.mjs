const SCHEMA = "libdictenstein.host-collection-traversal.v1";
const KEY_UNITS = 38;
const U64_MASK = (1n << 64n) - 1n;
const encoder = new TextEncoder();

function positive(value, name, allowZero = false) {
  if (!Number.isSafeInteger(value) || (allowZero ? value < 0 : value <= 0)) {
    throw new TypeError(`${name} must be ${allowZero ? "nonnegative" : "positive"}`);
  }
  return value;
}

export function makeCorpus(size) {
  positive(size, "entries");
  return Array.from({ length: size }, (_, index) => ({
    key: encoder.encode(`collection/${(index & 0x0fff).toString(16).padStart(4, "0")}/${index.toString(16).padStart(8, "0")}/shared-suffix`),
    value: BigInt(index),
  }));
}

function compareBytes(left, right) {
  const count = Math.min(left.length, right.length);
  for (let index = 0; index < count; index += 1) {
    if (left[index] !== right[index]) return left[index] - right[index];
  }
  return left.length - right.length;
}

function checksumEntry([key, value]) {
  if (!(key instanceof Uint8Array)) throw new TypeError("benchmark expected byte-domain entries");
  return BigInt(key.byteLength) ^ (value ?? 0n);
}

export function expectedChecksum(corpus, limit) {
  return corpus.slice().sort((left, right) => compareBytes(left.key, right.key)).slice(0, limit)
    .reduce((total, entry) => (total + (BigInt(entry.key.byteLength) ^ entry.value)) & U64_MASK, 0n);
}

function drain(dictionary, config) {
  const limit = config.arm === "stream-cancel"
    ? Math.min(config.entries, config.earlyCancel)
    : config.entries;
  if (config.arm === "materialized") {
    const snapshot = dictionary.snapshot();
    let checksum = 0n;
    for (const entry of snapshot) checksum = (checksum + checksumEntry(entry)) & U64_MASK;
    return { checksum, count: snapshot.size };
  }

  const stream = dictionary.streamEntries();
  let checksum = 0n;
  let count = 0;
  try {
    while (count < limit) {
      const batch = stream.nextBatch(Math.min(config.batchSize, limit - count));
      if (batch.length === 0) break;
      for (const entry of batch) checksum = (checksum + checksumEntry(entry)) & U64_MASK;
      count += batch.length;
    }
    if (config.arm === "stream" && (count !== limit || stream.nextBatch(config.batchSize).length !== 0)) {
      throw new Error("stream cardinality differs from the generated corpus");
    }
    return { checksum, count };
  } finally {
    stream.close();
  }
}

/** Run one browser, native Node, or Node-WASI facade arm and return its machine row. */
export function runCollectionTraversalProfile(namespace, options = {}) {
  const config = {
    arm: options.arm,
    entries: positive(options.entries ?? 65_536, "entries"),
    passes: positive(options.passes ?? 1, "passes"),
    warmupPasses: positive(options.warmupPasses ?? 1, "warmupPasses", true),
    batchSize: positive(options.batchSize ?? 256, "batchSize"),
    earlyCancel: positive(options.earlyCancel ?? 64, "earlyCancel"),
  };
  if (!["materialized", "stream", "stream-cancel"].includes(config.arm)) {
    throw new TypeError("arm must be materialized, stream, or stream-cancel");
  }
  const corpus = makeCorpus(config.entries);
  const consumed = config.arm === "stream-cancel"
    ? Math.min(config.entries, config.earlyCancel)
    : config.entries;
  const expected = expectedChecksum(corpus, consumed);
  const dictionary = namespace.dynamicDawg("byte");
  try {
    for (const entry of corpus) {
      if (!dictionary.put(entry.key, entry.value)) throw new Error("generated corpus contains a duplicate key");
    }
    for (let pass = 0; pass < config.warmupPasses; pass += 1) {
      const result = drain(dictionary, config);
      if (result.count !== consumed || result.checksum !== expected) throw new Error("warmup checksum or cardinality mismatch");
    }
    const started = performance.now();
    let checksum = 0n;
    for (let pass = 0; pass < config.passes; pass += 1) {
      const result = drain(dictionary, config);
      if (result.count !== consumed || result.checksum !== expected) throw new Error("timed checksum or cardinality mismatch");
      checksum = (checksum + result.checksum) & U64_MASK;
    }
    const elapsedNs = Math.max(1, Math.round((performance.now() - started) * 1_000_000));
    if (checksum !== (expected * BigInt(config.passes) & U64_MASK)) throw new Error("aggregate checksum mismatch");
    if (checksum > BigInt(Number.MAX_SAFE_INTEGER)) throw new RangeError("checksum exceeds the JSON safe-integer range");
    return {
      schema: SCHEMA,
      runtime: options.runtime ?? "javascript",
      arm: config.arm,
      dictionary_entries: config.entries,
      consumed_entries_per_pass: consumed,
      passes: config.passes,
      warmup_passes: config.warmupPasses,
      batch_size: config.arm === "materialized" ? null : config.batchSize,
      early_cancel: config.arm === "stream-cancel" ? config.earlyCancel : null,
      elapsed_ns: elapsedNs,
      checksum: Number(checksum),
    };
  } finally {
    dictionary.close();
  }
}

export const keyUnits = KEY_UNITS;
