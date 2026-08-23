/// <reference lib="esnext.disposable" />

import type { RuntimeIdentity, UnitDomain } from "@vinary-tree/interop";

export type DictionaryValue = bigint | null;
export type DictionaryKey = string | Uint8Array | BigUint64Array;
export type DictionaryEntry = readonly [DictionaryKey, DictionaryValue];
export interface Lookup { readonly found: boolean; readonly value: DictionaryValue; }
export interface DictionaryEntryCursor extends IterableIterator<DictionaryEntry> {
  readonly size: number;
  readonly identity: Readonly<{ producer: bigint; revision: bigint }> | null;
  nextBatch(maximum: number): DictionaryEntry[];
  reduceBatches<A>(
    reducer: (accumulator: A, batch: readonly DictionaryEntry[]) => A,
    initial: A,
    batchSize?: number,
  ): A;
  close(): void;
  return(value?: unknown): IteratorResult<DictionaryEntry>;
  [Symbol.dispose](): void;
}
export interface DictionarySnapshot extends Iterable<DictionaryEntry> {
  readonly size: number;
  entries(): IterableIterator<DictionaryEntry>;
  keys(): IterableIterator<DictionaryKey>;
  values(): IterableIterator<DictionaryValue>;
  forEach(callback: (value: DictionaryValue, key: DictionaryKey, snapshot: DictionarySnapshot) => void, thisArg?: unknown): void;
  toMap(): Map<DictionaryKey, DictionaryValue>;
}
export interface Dictionary extends Iterable<DictionaryEntry> {
  readonly interfaceId: "vt.dictionary.v1";
  readonly runtimeIdentity: RuntimeIdentity;
  readonly unitDomain: UnitDomain;
  readonly valueDomain: "optional-u64";
  readonly size: number;
  put(term: DictionaryKey, value?: DictionaryValue): boolean;
  putU64(term: BigUint64Array, value?: DictionaryValue): boolean;
  set(term: DictionaryKey, value?: DictionaryValue): this;
  remove(term: DictionaryKey): boolean;
  removeU64(term: BigUint64Array): boolean;
  delete(term: DictionaryKey): boolean;
  has(term: DictionaryKey): boolean;
  hasU64(term: BigUint64Array): boolean;
  lookup(term: DictionaryKey): Lookup;
  lookupU64(term: BigUint64Array): Lookup;
  get(term: DictionaryKey): DictionaryValue | undefined;
  getU64(term: BigUint64Array): DictionaryValue | undefined;
  snapshot(): DictionarySnapshot;
  streamEntries(): DictionaryEntryCursor;
  entries(): IterableIterator<DictionaryEntry>;
  keys(): IterableIterator<DictionaryKey>;
  values(): IterableIterator<DictionaryValue>;
  forEach(callback: (value: DictionaryValue, key: DictionaryKey, dictionary: Dictionary) => void, thisArg?: unknown): void;
  toMap(): Map<DictionaryKey, DictionaryValue>;
  clear(): void;
  compact(): number;
  containsSubstring(term: string): boolean;
  substringFrequency(term: string): number;
  close(): void;
  [Symbol.dispose](): void;
}
export interface LibdictensteinNamespace {
  readonly runtimeIdentity: RuntimeIdentity;
  dynamicDawg(unitDomain?: UnitDomain): Dictionary;
  doubleArrayTrie(entries: readonly { term: string; value?: DictionaryValue }[], unitDomain?: "byte" | "unicode"): Dictionary;
  scdawg(unitDomain?: "byte" | "unicode"): Dictionary;
}
export const runtimeIdentity: RuntimeIdentity;
export function dynamicDawg(unitDomain?: UnitDomain): Dictionary;
export function doubleArrayTrie(entries: readonly { term: string; value?: DictionaryValue }[], unitDomain?: "byte" | "unicode"): Dictionary;
export function scdawg(unitDomain?: "byte" | "unicode"): Dictionary;
declare const libdictenstein: LibdictensteinNamespace;
export default libdictenstein;
