"use strict";
const { libdictenstein } = require("@vinary-tree/javascript-runtime");
const { collectionNamespace, UnsupportedBackendError } = require("./collections.cjs");
const facade = collectionNamespace(libdictenstein);
module.exports = { ...facade, UnsupportedBackendError, default: facade };
