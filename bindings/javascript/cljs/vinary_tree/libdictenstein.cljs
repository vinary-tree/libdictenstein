(ns vinary-tree.libdictenstein
  "ClojureScript facade mirroring the Clojure libdictenstein API."
  (:refer-clojure :exclude [contains? get keys])
  (:require [goog.object :as gobj]
            ["@vinary-tree/libdictenstein" :as native]))

(defn- domain-name [value]
  (case value
    (:bytes :byte) "byte"
    (:unicode :unicode-scalar) "unicode"
    :u64 "u64"
    (name value)))

(defn dynamic-dawg
  ([] (native/dynamicDawg "unicode"))
  ([unit-domain] (native/dynamicDawg (domain-name unit-domain))))

(defn double-array-trie
  ([entries] (double-array-trie entries :unicode))
  ([entries unit-domain]
   (native/doubleArrayTrie
    (clj->js (map (fn [[term value]] {:term (str term) :value value}) entries))
    (domain-name unit-domain))))

(defn scdawg
  ([] (native/scdawg "unicode"))
  ([unit-domain] (native/scdawg (domain-name unit-domain))))

(defn size [dictionary] (.-size dictionary))
(defn- native-key [term]
  (if (or (string? term)
          (instance? js/Uint8Array term)
          (instance? js/BigUint64Array term))
    term
    (str term)))

(defn contains? [dictionary term] (.has dictionary (native-key term)))
(defn get [dictionary term]
  (let [result (.lookup dictionary (native-key term))]
    {:present? (gobj/get result "found")
     :value (gobj/get result "value")}))
(defn put! [dictionary term value] (.put dictionary (native-key term) value))
(defn put-all! [dictionary entries]
  (reduce (fn [count [term value]]
            (+ count (if (put! dictionary term value) 1 0)))
          0
          entries))
(defn remove! [dictionary term] (.remove dictionary (native-key term)))
(defn snapshot
  "Materialize one immutable dictionary revision as a persistent vector."
  [dictionary]
  (mapv (fn [pair] [(aget pair 0) (aget pair 1)])
        (array-seq (js/Array.from dictionary))))
(defn entries [dictionary] (seq (snapshot dictionary)))
(defn keys [dictionary] (map first (snapshot dictionary)))
(defn values [dictionary] (map second (snapshot dictionary)))
(defn reduce-entries
  "Reduce one native snapshot without materializing it; reduced values stop
  early and the native cursor is closed in all exit paths."
  [dictionary reducer initial]
  (let [cursor (.streamEntries dictionary)]
    (try
      (loop [accumulator initial]
        (let [step (.next cursor)]
          (if (.-done step)
            accumulator
            (let [pair (.-value step)
                  next-value (reducer accumulator
                                      (aget pair 0)
                                      (aget pair 1))]
              (if (reduced? next-value)
                @next-value
                (recur next-value))))))
      (finally (.close cursor)))))
(defn with-entry-stream
  "Invoke f with a closeable native entry iterator and always release it."
  [dictionary f]
  (let [cursor (.streamEntries dictionary)]
    (try (f cursor)
         (finally (.close cursor)))))
(defn clear! [dictionary] (.clear dictionary))
(defn compact! [dictionary] (.compact dictionary))
(defn contains-substring? [dictionary pattern] (.containsSubstring dictionary pattern))
(defn frequency [dictionary pattern] (.substringFrequency dictionary pattern))
(defn close! [resource] (.close resource))
