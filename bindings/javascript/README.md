# libdictenstein JavaScript bindings

`@vinary-tree/libdictenstein` owns the JavaScript, TypeScript, and
ClojureScript dictionary facades. It delegates to the single
`@vinary-tree/vinary-tree` runtime so a dictionary passes to
`@vinary-tree/liblevenshtein` without serialization or copying.

Use the default export for native Node, `/wasm` in browsers, and `/wasi` for
Node filesystem-backed persistent ARTrie dictionaries. Query consumers remain
lazy and never require materializing the complete result set.
