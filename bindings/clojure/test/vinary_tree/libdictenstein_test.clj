(ns vinary-tree.libdictenstein-test
  (:require [clojure.test :refer [deftest is]]
            [vinary-tree.libdictenstein :as dictionary])
  (:import (io.vinarytree.liblevenshtein Transducer)))

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
