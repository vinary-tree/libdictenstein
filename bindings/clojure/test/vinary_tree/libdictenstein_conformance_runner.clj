(ns vinary-tree.libdictenstein-conformance-runner
  "Runs the uniform C1-C10 conformance suite and exits non-zero on failure.

  Invoke from bindings/clojure with the shared library on the loader path, e.g.:
    clojure -J-Djava.library.path=../../target/release -M:conformance"
  (:require [clojure.test :as test]
            [vinary-tree.libdictenstein-conformance]))

(let [result (test/run-tests 'vinary-tree.libdictenstein-conformance)]
  (System/exit (if (test/successful? result) 0 1)))
