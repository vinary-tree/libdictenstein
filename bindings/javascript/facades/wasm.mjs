import { libdictenstein } from "@vinary-tree/javascript-runtime/wasm";
import { collectionNamespace } from "./collections.mjs";
const facade = collectionNamespace(libdictenstein);
export const { runtimeIdentity, dynamicDawg, doubleArrayTrie, scdawg } = facade;
export default facade;
