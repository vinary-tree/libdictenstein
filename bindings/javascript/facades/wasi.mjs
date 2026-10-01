import { libdictenstein } from "@vinary-tree/javascript-runtime/wasi";
import { collectionNamespace } from "./collections.mjs";
export { UnsupportedBackendError } from "./collections.mjs";
const facade = collectionNamespace(libdictenstein);
export const { runtimeIdentity, dynamicDawg, doubleArrayTrie, scdawg,
  createPersistentARTrie, openPersistentARTrie, pathMap, suffixIndex } = facade;
export default facade;
