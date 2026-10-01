(ns vinary-tree.libdictenstein-rev8-test
  (:require [cljs.test :refer-macros [deftest is run-tests]]
            [vinary-tree.libdictenstein :as dictionary]))

(deftest revision-eight-backends-are-explicitly-unsupported
  (doseq [revision [7 8 nil]]
    (set! (.-__vinaryTreeTestApiRevision js/globalThis) revision)
    (let [expected-reason (if (and (some? revision) (< revision 8))
                            #"requires native API revision 8"
                            #"not yet mediated")]
      (doseq [[constructor backend] [[dictionary/path-map "PathMap"]
                                     [dictionary/suffix-index "suffix index"]]]
        (let [error (try (constructor)
                         nil
                         (catch :default failure failure))]
          (is (= 6 (.-status error)))
          (is (= backend (.-backend error)))
          (is (= 8 (.-requiredRevision error)))
          (is (= revision (.-apiRevision error)))
          (is (re-find expected-reason (.-message error)))))))
  (is (zero? (.-__vinaryTreeRawBackendCalls js/globalThis))))

(let [result (run-tests 'vinary-tree.libdictenstein-rev8-test)]
  (set! (.-exitCode js/process) (if (zero? (+ (:fail result) (:error result))) 0 1)))
