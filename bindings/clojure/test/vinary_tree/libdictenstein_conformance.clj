(ns vinary-tree.libdictenstein-conformance
  "Uniform facade conformance suite for the Clojure binding.

  Instantiates the family C1-C10 contract for Clojure against a live
  libdictenstein shared library (mediated through the JVM facade). Unlike
  libdictenstein-test this suite needs only libdictenstein and the canonical
  fixture, never a liblevenshtein transducer, so it pins the *producer* ABI in
  isolation.

    C1  identity/version           (abi/api via facade; kind/caps via Java object)
    C2  lifecycle/ownership        (idempotent close! + free-order independence)
    C3  error-mapping matrix       (IO_ERROR + message; others N/A, see below)
    C4  canonical fixture replay   (cross-language oracle)
    C5  CRUD/value/batch/substring (+ capability-derived assertions)
    C6  text domains / values      (é/🦀/combining + embedded NUL; u64/invalid N/A)
    C7  batch edges                (0/1/255/256/257/large)
    C8  property vs oracle         (CRUD script + substring naive)
    C9  leak discipline            (>=10k cycles, RSS bounded)
    C10 concurrency                (parallel snapshot/mutate)

  The Clojure facade is deliberately String/text-oriented, so a few contract
  points are marked N/A:
    - INVALID_UTF8 (C3) and invalid-UTF-8 byte terms (C6): unrepresentable as a
      Clojure String, which is the only term type the facade accepts.
    - DOMAIN_MISMATCH (C3) and the u64 token domain (C6): the facade exposes no
      u64-token surface."
  (:require [clojure.test :refer [deftest is]]
            [clojure.data.json :as json]
            [vinary-tree.libdictenstein :as d])
  (:import (io.vinarytree.libdictenstein Dictionary NativeException)))

(def fixture
  (json/read-str
   (slurp (first (filter #(.exists (java.io.File. ^String %))
                         ["../canonical_fixture.json"
                          "bindings/canonical_fixture.json"
                          "../../bindings/canonical_fixture.json"])))))

;; Capability bits (LDICT_CAP_*).
(def ^:const cap-read (bit-shift-left 1 0))
(def ^:const cap-insert (bit-shift-left 1 1))
(def ^:const cap-remove (bit-shift-left 1 2))
(def ^:const cap-clear (bit-shift-left 1 3))
(def ^:const cap-compact (bit-shift-left 1 4))
(def ^:const cap-substring (bit-shift-left 1 5))
(def ^:const cap-checkpoint (bit-shift-left 1 6))

(defn- entries [] (into {} (map (fn [e] [(e "term") (e "value")]) (fixture "entries"))))

(defn- value= [expected actual]
  (if (nil? expected) (nil? actual) (and (some? actual) (== expected actual))))

;; --------------------------------------------------------------------------
;; C1 identity/version
;; --------------------------------------------------------------------------

(deftest c1-identity-constants
  (is (= 1 (d/abi-version)))
  (is (= 4 (d/api-revision))))

(deftest c1-kind-and-capabilities
  (with-open [dawg (d/dynamic-dawg)]
    (is (= 1 (.kind ^Dictionary dawg)))
    (let [caps (.capabilities ^Dictionary dawg)]
      (is (pos? (bit-and caps cap-insert)))
      (is (pos? (bit-and caps cap-remove)))
      (is (pos? (bit-and caps cap-clear)))
      (is (pos? (bit-and caps cap-compact)))
      (is (zero? (bit-and caps cap-substring)))
      (is (zero? (bit-and caps cap-checkpoint)))))
  (with-open [dat (d/double-array-trie {"x" nil})]
    (is (= 2 (.kind ^Dictionary dat)))
    (is (pos? (bit-and (.capabilities ^Dictionary dat) cap-read))))
  (with-open [scdawg (d/scdawg)]
    (is (= 3 (.kind ^Dictionary scdawg)))
    (is (pos? (bit-and (.capabilities ^Dictionary scdawg) cap-substring)))))

;; --------------------------------------------------------------------------
;; C2 lifecycle/ownership
;; --------------------------------------------------------------------------

(deftest c2-double-close-is-idempotent
  (let [dawg (d/dynamic-dawg)]
    (d/put! dawg "a" nil)
    (d/close! dawg)
    (d/close! dawg)))                     ; no double free, no crash

(deftest c2-free-order-independence
  (let [dawgs (mapv (fn [_] (d/dynamic-dawg)) (range 4))]
    (doseq [i (range 4)] (d/put! (nth dawgs i) (str "term" i) i))
    (doseq [i [2 0 3 1]] (d/close! (nth dawgs i)))))

;; --------------------------------------------------------------------------
;; C3 error-mapping matrix + thread-local message
;;
;; Reachable through the idiomatic (String-oriented) facade: IO_ERROR (7).
;; N/A: INVALID_UTF8 (3) and DOMAIN_MISMATCH (9) — see the ns docstring;
;; NULL_POINTER (4) is guarded by an IllegalStateException before the ABI;
;; UNSUPPORTED (6) is capability-derived (C5); LIMIT_EXCEEDED (10) is auto-sized
;; away by vocabulary-term.
;; --------------------------------------------------------------------------

(deftest c3-io-error-on-missing-persistent
  (let [path (str (System/getProperty "java.io.tmpdir")
                  "/ldict-clj-missing-" (System/nanoTime) ".part")]
    (try
      (d/open-persistent-artrie path)
      (is false "expected NativeException")
      (catch NativeException e
        (is (= 7 (.status e)))
        (is (seq (.getMessage e)))))))

;; --------------------------------------------------------------------------
;; C4 canonical fixture replay (cross-language oracle)
;; --------------------------------------------------------------------------

(defn- assert-fixture-reads [dictionary]
  (is (= (fixture "size") (d/size dictionary)))
  (doseq [item (fixture "contains")]
    (is (= (item "expected") (d/contains? dictionary (item "term"))) (item "term")))
  (doseq [item (fixture "get")]
    (let [result (d/get dictionary (item "term"))]
      (is (= (item "found") (:present? result)) (item "term"))
      (is (value= (item "value") (:value result)) (item "term")))))

(deftest c4-dynamic-dawg-matches-oracle
  (with-open [dawg (d/dynamic-dawg)]
    (is (= (fixture "size") (d/put-all! dawg (entries))))
    (assert-fixture-reads dawg)))

(deftest c4-double-array-trie-matches-oracle
  (with-open [dat (d/double-array-trie (entries))]
    (assert-fixture-reads dat)))

(deftest c4-persistent-artrie-matches-oracle
  (let [path (str (System/getProperty "java.io.tmpdir")
                  "/ldict-clj-c4-" (System/nanoTime) ".part")]
    (with-open [art (d/create-persistent-artrie path)]
      (is (= (fixture "size") (d/put-all! art (entries))))
      (assert-fixture-reads art))))

(deftest c4-scdawg-matches-substring-oracle
  (with-open [scdawg (d/scdawg)]
    (d/put-all! scdawg (entries))
    (doseq [item (fixture "substring_frequency")]
      (is (= (item "expected") (d/frequency scdawg (item "pattern"))) (item "pattern")))
    (doseq [item (fixture "substring_contains")]
      (is (= (item "expected") (d/contains-substring? scdawg (item "pattern"))) (item "pattern")))))

;; --------------------------------------------------------------------------
;; C5 CRUD + value + batch + substring; capability-derived assertions
;; --------------------------------------------------------------------------

(deftest c5-crud-round-trip
  (with-open [dawg (d/dynamic-dawg)]
    (is (d/put! dawg "cat" 1))
    (is (not (d/put! dawg "cat" 1)))       ; idempotent
    (is (value= 1 (:value (d/get dawg "cat"))))
    (is (d/remove! dawg "cat"))
    (is (not (d/remove! dawg "cat")))
    (is (not (d/contains? dawg "cat")))))

(deftest c5-compact-preserves-terms
  (with-open [dawg (d/dynamic-dawg)]
    (d/put-all! dawg (into {} (map (fn [i] [(str "t" i) i]) (range 50))))
    (doseq [i (range 0 50 2)] (is (d/remove! dawg (str "t" i))))
    (d/compact! dawg)
    (is (= 25 (d/size dawg)))
    (is (value= 1 (:value (d/get dawg "t1"))))
    (is (not (d/contains? dawg "t0")))))

(deftest c5-substring-updates-with-inserts
  (with-open [scdawg (d/scdawg)]
    (d/put! scdawg "cat" 1)
    (d/put! scdawg "cot" 2)
    (is (= 2 (d/frequency scdawg "t")))
    (is (d/put! scdawg "cut" nil))
    (is (= 3 (d/frequency scdawg "t")))))

(deftest c5-capability-derived-rejects
  (with-open [dat (d/double-array-trie {"x" nil})]
    (let [caps (.capabilities ^Dictionary dat)]
      (is (zero? (bit-and caps (bit-or cap-insert cap-remove cap-clear cap-compact)))))))

;; --------------------------------------------------------------------------
;; C6 text domains and values (String-oriented facade)
;; --------------------------------------------------------------------------

(deftest c6-precomposed-and-multibyte
  (with-open [dawg (d/dynamic-dawg)]
    (is (d/put! dawg "café" 7))       ; precomposed U+00E9
    (is (d/put! dawg "🦀" 255))  ; 🦀, 4-byte scalar
    (is (d/contains? dawg "café"))
    (is (value= 255 (:value (d/get dawg "🦀"))))))

(deftest c6-combining-sequence-is-distinct-from-precomposed
  (with-open [dawg (d/dynamic-dawg)]
    (is (d/put! dawg "café" 1))       ; precomposed U+00E9
    (is (d/put! dawg "café" 2))      ; cafe + U+0301 combining acute
    (is (= 2 (d/size dawg)))
    (is (value= 1 (:value (d/get dawg "café"))))
    (is (value= 2 (:value (d/get dawg "café"))))))

(deftest c6-byte-domain-accepts-embedded-nul
  (let [embedded-nul (str "a" (char 0) "b")]  ; embedded NUL survives UTF-8 encoding
    (with-open [dawg (d/dynamic-dawg :bytes)]
      (is (d/put! dawg embedded-nul 1))
      (is (d/contains? dawg embedded-nul))
      (is (value= 1 (:value (d/get dawg embedded-nul)))))))

;; --------------------------------------------------------------------------
;; C7 batch/paging edges
;; --------------------------------------------------------------------------

(deftest c7-batch-sizes
  (doseq [size [0 1 255 256 257 1000]]
    (with-open [dawg (d/dynamic-dawg)]
      (is (= size (d/put-all! dawg (into {} (map (fn [i] [(str "t" i) i]) (range size))))))
      (is (= size (d/size dawg)))
      (when (pos? size)
        (is (value= 0 (:value (d/get dawg "t0"))))
        (is (value= (dec size) (:value (d/get dawg (str "t" (dec size))))))))))

;; --------------------------------------------------------------------------
;; C8 property-based testing vs an in-language oracle
;; --------------------------------------------------------------------------

(deftest c8-crud-script-matches-map-oracle
  (let [rng (java.util.Random. 0xC0FFEE)
        keys (mapv #(str "k" %) (range 40))
        oracle (atom {})]
    (with-open [dawg (d/dynamic-dawg)]
      (dotimes [_ 3000]
        (let [key (nth keys (.nextInt rng (count keys)))
              present (contains? @oracle key)
              op (.nextDouble rng)]
          (cond
            (< op 0.5) (let [value (if (.nextBoolean rng) nil (long (.nextInt rng Integer/MAX_VALUE)))]
                         (is (= (not present) (d/put! dawg key value)))
                         (swap! oracle assoc key value))
            (< op 0.75) (do (is (= present (d/remove! dawg key)))
                            (swap! oracle dissoc key))
            (< op 0.95) (do (is (= present (d/contains? dawg key)))
                            (when present
                              (is (value= (@oracle key) (:value (d/get dawg key))))))
            :else (d/compact! dawg))
          (is (= (count @oracle) (d/size dawg))))))))

(deftest c8-substring-matches-naive-oracle
  (let [rng (java.util.Random. 0x5CDA)
        alphabet "abcx"
        gen (fn [max-len]
              (apply str (repeatedly (inc (.nextInt rng max-len))
                                     #(nth alphabet (.nextInt rng (count alphabet))))))
        terms (loop [acc #{}] (if (< (count acc) 60) (recur (conj acc (gen 6))) (vec acc)))
        naive (fn [pattern]
                (reduce + (map (fn [term]
                                 (count (filter #(= pattern (subs term % (+ % (count pattern))))
                                                (range (inc (- (count term) (count pattern)))))))
                               terms)))]
    (with-open [scdawg (d/scdawg)]
      (d/put-all! scdawg (into {} (map (fn [t] [t nil]) terms)))
      (dotimes [_ 200]
        (let [pattern (gen 3)
              expected (naive pattern)]
          (is (= expected (d/frequency scdawg pattern)) pattern)
          (is (= (pos? expected) (d/contains-substring? scdawg pattern)) pattern))))))

;; --------------------------------------------------------------------------
;; C9 leak discipline
;; --------------------------------------------------------------------------

(defn- rss-kib []
  ;; Read /proc/self/status via NIO: FileInputStream.available() throws EINVAL
  ;; on procfs, so slurp is unusable here.
  (try
    (or (some (fn [^String line]
                (when (.startsWith line "VmRSS:")
                  (Long/parseLong (re-find #"\d+" line))))
              (java.nio.file.Files/readAllLines
               (java.nio.file.Path/of "/proc/self/status" (make-array String 0))))
        0)
    (catch Exception _ 0)))

(deftest c9-create-use-free-cycles-do-not-leak
  (let [cycles 12000
        batch {"cat" 1 "cot" 2 "cut" nil}]
    (dotimes [_ 2000]
      (let [dawg (d/dynamic-dawg)] (d/put! dawg "cat" 1) (d/close! dawg)))
    (System/gc)
    (let [before (rss-kib)]
      (dotimes [_ cycles]
        (let [dawg (d/dynamic-dawg)]
          (d/put-all! dawg batch)
          (is (d/contains? dawg "cot"))
          (d/close! dawg)))
      (System/gc)
      (let [after (rss-kib)]
        (when (and (pos? before) (> after before))
          (is (< (- after before) (* 96 1024))
              (str "RSS grew " (- after before) " KiB over " cycles " cycles")))))))

;; --------------------------------------------------------------------------
;; C10 concurrency
;; --------------------------------------------------------------------------

(deftest c10-independent-dictionaries-per-thread
  (let [errors (atom [])
        workers (mapv (fn [seed]
                        (future
                          (try
                            (with-open [dawg (d/dynamic-dawg)]
                              (dotimes [i 2000] (d/put! dawg (str "t" seed "_" i) i))
                              (when-not (= 2000 (d/size dawg)) (throw (ex-info "len" {})))
                              (when-not (value= 1500 (:value (d/get dawg (str "t" seed "_1500"))))
                                (throw (ex-info "get" {}))))
                            (catch Throwable t (swap! errors conj t)))))
                      (range 8))]
    (doseq [w workers] @w)
    (is (empty? @errors))))

(deftest c10-concurrent-readers-during-writer
  (let [errors (atom [])]
    (with-open [dawg (d/dynamic-dawg)]
      (d/put-all! dawg (into {} (map (fn [i] [(str "seed" i) i]) (range 500))))
      (let [stop (atom false)
            readers (mapv (fn [_]
                            (future
                              (try
                                (while (not @stop)
                                  (when-not (d/contains? dawg "seed0") (throw (ex-info "lost seed0" {})))
                                  (d/get dawg "seed250"))
                                (catch Throwable t (swap! errors conj t)))))
                          (range 4))]
        (doseq [i (range 500 3000)] (d/put! dawg (str "w" i) i))
        (reset! stop true)
        (doseq [r readers] @r)
        (is (empty? @errors))
        (is (value= 2999 (:value (d/get dawg "w2999"))))))))
