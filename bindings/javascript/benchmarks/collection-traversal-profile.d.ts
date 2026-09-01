import type { LibdictensteinNamespace } from "../index.js";

export type CollectionTraversalArm = "materialized" | "stream" | "stream-cancel";
export interface CollectionTraversalOptions {
  arm: CollectionTraversalArm;
  runtime?: string;
  entries?: number;
  passes?: number;
  warmupPasses?: number;
  batchSize?: number;
  earlyCancel?: number;
}
export interface CollectionTraversalResult {
  schema: "libdictenstein.host-collection-traversal.v1";
  runtime: string;
  arm: CollectionTraversalArm;
  dictionary_entries: number;
  consumed_entries_per_pass: number;
  passes: number;
  warmup_passes: number;
  batch_size: number | null;
  early_cancel: number | null;
  elapsed_ns: number;
  checksum: number;
}
export interface CorpusEntry { key: Uint8Array; value: bigint; }
export function makeCorpus(size: number): CorpusEntry[];
export function expectedChecksum(corpus: CorpusEntry[], limit: number): bigint;
export function runCollectionTraversalProfile(
  namespace: LibdictensteinNamespace,
  options: CollectionTraversalOptions,
): CollectionTraversalResult;
export const keyUnits: 38;
