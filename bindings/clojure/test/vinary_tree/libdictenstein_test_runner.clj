(ns vinary-tree.libdictenstein-test-runner
  (:require [clojure.test :as test]
            [vinary-tree.libdictenstein-test]))

(let [result (test/run-tests 'vinary-tree.libdictenstein-test)]
  (System/exit (if (test/successful? result) 0 1)))
