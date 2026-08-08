# libdictenstein Clojure bindings

The `io.vinarytree/libdictenstein-clojure` artifact is an idiomatic Clojure
facade over the Java 22+ FFM package. Dictionaries implement the shared
`DictionaryResource` contract, so they pass directly to liblevenshtein without
serialization. Bulk mutation uses one native crossing and lookup preserves the
three states absent, present-without-value, and present-with-u64-value.

Publish to Clojars with `lein deploy clojars`; credentials are read from
`CLOJARS_USERNAME` and `CLOJARS_PASSWORD`.
