import { libdictenstein } from "@vinary-tree/vinary-tree/wasi";
import { collectionNamespace } from "./collections.mjs";
const facade = collectionNamespace(libdictenstein);
export const { runtimeIdentity, dynamicDawg, doubleArrayTrie, scdawg,
  createPersistentARTrie, openPersistentARTrie } = facade;
export default facade;
