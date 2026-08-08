import type { RuntimeIdentity, UnitDomain } from "@vinary-tree/interop";

export type DictionaryValue = bigint | null;
export interface Lookup { readonly found: boolean; readonly value: DictionaryValue; }
export interface Dictionary {
  readonly interfaceId: "vt.dictionary.v1";
  readonly runtimeIdentity: RuntimeIdentity;
  readonly unitDomain: UnitDomain;
  readonly valueDomain: "optional-u64";
  readonly size: number;
  put(term: string, value?: DictionaryValue): boolean;
  putU64(term: BigUint64Array, value?: DictionaryValue): boolean;
  remove(term: string): boolean;
  removeU64(term: BigUint64Array): boolean;
  has(term: string): boolean;
  hasU64(term: BigUint64Array): boolean;
  get(term: string): Lookup;
  getU64(term: BigUint64Array): Lookup;
  clear(): void;
  compact(): number;
  containsSubstring(term: string): boolean;
  substringFrequency(term: string): number;
  close(): void;
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
