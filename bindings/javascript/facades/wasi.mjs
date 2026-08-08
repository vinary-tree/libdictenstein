import { libdictenstein } from "@vinary-tree/vinary-tree/wasi";
export const runtimeIdentity = libdictenstein.runtimeIdentity;
export const dynamicDawg = libdictenstein.dynamicDawg.bind(libdictenstein);
export const doubleArrayTrie = libdictenstein.doubleArrayTrie.bind(libdictenstein);
export const scdawg = libdictenstein.scdawg.bind(libdictenstein);
export const createPersistentARTrie = libdictenstein.createPersistentARTrie.bind(libdictenstein);
export const openPersistentARTrie = libdictenstein.openPersistentARTrie.bind(libdictenstein);
export default libdictenstein;
