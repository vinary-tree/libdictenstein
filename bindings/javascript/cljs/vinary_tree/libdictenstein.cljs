(ns vinary-tree.libdictenstein
  "ClojureScript facade mirroring the Clojure libdictenstein API."
  (:refer-clojure :exclude [contains? get])
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
(defn contains? [dictionary term] (.has dictionary (str term)))
(defn get [dictionary term]
  (let [result (.get dictionary (str term))]
    {:present? (gobj/get result "found")
     :value (gobj/get result "value")}))
(defn put! [dictionary term value] (.put dictionary (str term) value))
(defn put-all! [dictionary entries]
  (reduce (fn [count [term value]]
            (+ count (if (put! dictionary term value) 1 0)))
          0
          entries))
(defn remove! [dictionary term] (.remove dictionary (str term)))
(defn clear! [dictionary] (.clear dictionary))
(defn compact! [dictionary] (.compact dictionary))
(defn contains-substring? [dictionary pattern] (.containsSubstring dictionary pattern))
(defn frequency [dictionary pattern] (.substringFrequency dictionary pattern))
(defn close! [resource] (.close resource))
