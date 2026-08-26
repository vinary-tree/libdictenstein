"use strict";
const { libdictenstein } = require("@vinary-tree/javascript-runtime/wasi");
const { collectionNamespace } = require("./collections.cjs");
const facade = collectionNamespace(libdictenstein);
module.exports = { ...facade, default: facade };
