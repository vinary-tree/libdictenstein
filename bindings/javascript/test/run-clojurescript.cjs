"use strict";

const Module = require("node:module");
const { resolve } = require("node:path");
const { collectionNamespace } = require("../facades/collections.cjs");

globalThis.__vinaryTreeTestApiRevision = 7;
globalThis.__vinaryTreeRawBackendCalls = 0;

const facade = collectionNamespace({
  apiRevision: () => globalThis.__vinaryTreeTestApiRevision,
  pathMap() { globalThis.__vinaryTreeRawBackendCalls += 1; },
  suffixIndex() { globalThis.__vinaryTreeRawBackendCalls += 1; },
});
const load = Module._load;
Module._load = function loadTestFacade(request, parent, isMain) {
  if (request === "@vinary-tree/libdictenstein") return facade;
  return load.call(this, request, parent, isMain);
};

try {
  require(resolve(__dirname, "../../../target/libdictenstein-cljs-r8/main.cjs"));
} finally {
  Module._load = load;
}
