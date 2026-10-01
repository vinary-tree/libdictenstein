(ns vinary-tree.libdictenstein-test
  (:require [clojure.test :refer [deftest is]]
            [vinary-tree.libdictenstein :as dictionary])
  (:import (io.vinarytree.libdictenstein NativeException)
           (io.vinarytree.liblevenshtein Transducer)))

(deftest revision-eight-backends-are-explicitly-unsupported
  (let [expected-reason (if (< (dictionary/api-revision) 8)
                          #"requires native API revision 8"
                          #"not yet mediated")]
    (doseq [constructor [dictionary/path-map dictionary/suffix-index]]
      (let [error (try (constructor)
                       nil
                       (catch NativeException failure failure))]
        (is (instance? NativeException error))
        (is (= 6 (.status ^NativeException error)))
        (is (re-find expected-reason (.getMessage ^NativeException error)))))))

(deftest dynamic-crud-and-query-snapshot
  (let [dawg (dictionary/dynamic-dawg)]
    (try
      (is (= 3 (dictionary/put-all! dawg {"cat" 1 "cot" 2 "cut" nil})))
      (is (= {:present? true :value nil} (dictionary/get dawg "cut")))
      (with-open [automaton (Transducer. dawg)
                  cursor (.query automaton "cat" 2)]
        (let [first (.next cursor)]
          (is (dictionary/remove! dawg "cot"))
          (is (dictionary/put! dawg "cit" 4))
          (let [frozen (cons first (iterator-seq cursor))]
            (is (= 3 (count frozen))))))
      (finally (dictionary/close! dawg)))))

(deftest dat-and-scdawg
  (with-open [dat (dictionary/double-array-trie {"café" 7 "caff" nil})]
    (is (dictionary/contains? dat "café"))
    (is (= {:present? true :value nil} (dictionary/get dat "caff"))))
  (with-open [suffixes (dictionary/scdawg)]
    (dictionary/put-all! suffixes {"cat" 1 "cot" 2})
    (is (dictionary/contains-substring? suffixes "ot"))
    (is (= 2 (dictionary/frequency suffixes "t")))))

(deftest native-algebra-is-idiomatic-and-value-aware
  (with-open [left (dictionary/dynamic-dawg)
              right (dictionary/dynamic-dawg)]
    (dictionary/put-all! left {"a" 1 "shared" 7 "valueless" nil})
    (dictionary/put-all! right {"b" 2 "shared" 11 "valueless" 5})
    (with-open [joined (dictionary/union left right {:value-merge :lattice-join})
                common (dictionary/intersection left right)
                only-left (dictionary/difference left right)
                exclusive (dictionary/symmetric-difference left right)]
      (is (= 4 (dictionary/size joined)))
      (is (= {:present? true :value 11N} (dictionary/get joined "shared")))
      (is (= {:present? true :value 5N} (dictionary/get joined "valueless")))
      (is (= 2 (dictionary/size common)))
      (is (= {:present? true :value 7N} (dictionary/get common "shared")))
      (is (= {:present? true :value nil} (dictionary/get common "valueless")))
      (is (dictionary/contains? only-left "a"))
      (is (= 2 (dictionary/size exclusive)))
      (is (dictionary/contains? exclusive "b"))
      (dictionary/put! left "later" 99)
      (is (not (dictionary/contains? joined "later")))
      (is (dictionary/put! joined "mutable-result" 23)))))
