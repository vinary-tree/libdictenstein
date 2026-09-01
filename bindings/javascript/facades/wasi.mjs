import { libdictenstein } from "@vinary-tree/javascript-runtime/wasi";
import { collectionNamespace } from "./collections.mjs";
const facade = collectionNamespace(libdictenstein);
export const { runtimeIdentity, dynamicDawg, doubleArrayTrie, scdawg,
  createPersistentARTrie, openPersistentARTrie } = facade;
export default facade;
