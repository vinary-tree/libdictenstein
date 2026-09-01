(ns vinary-tree.libdictenstein-collection-profile
  "Public-package collection traversal driver for the Clojure facade."
  (:require [vinary-tree.libdictenstein :as d])
  (:import (java.lang Integer Long System)))

(def ^:private default-entries 65536)
(def ^:private default-batch-size 256)
(def ^:private default-early-cancel 64)
(def ^:private u64-modulus 18446744073709551616N)
(def ^:private arms #{"materialized" "stream" "stream-cancel" "reduce"})

(defn- parse-integer [value option allow-zero?]
  (let [parsed (try
                 (Long/parseLong value)
                 (catch NumberFormatException _
                   (throw (IllegalArgumentException.
                           (str option " must be an integer")))))]
    (when (if allow-zero? (neg? parsed) (not (pos? parsed)))
      (throw (IllegalArgumentException.
              (str option (if allow-zero? " must be nonnegative" " must be positive")))))
    parsed))

(defn- parse-arguments [arguments]
  (when (odd? (count arguments))
    (throw (IllegalArgumentException. "every option requires a value")))
  (let [config
        (reduce
         (fn [result [option value]]
           (case option
             "--arm" (assoc result :arm value)
             "--entries" (assoc result :entries (parse-integer value option false))
             "--passes" (assoc result :passes (parse-integer value option false))
             "--warmup-passes" (assoc result :warmup-passes (parse-integer value option true))
             "--batch-size" (assoc result :batch-size (parse-integer value option false))
             "--early-cancel" (assoc result :early-cancel (parse-integer value option false))
             (throw (IllegalArgumentException. (str "unknown argument: " option)))))
         {:arm nil
          :entries default-entries
          :passes 1
          :warmup-passes 1
          :batch-size default-batch-size
          :early-cancel default-early-cancel}
         (partition 2 arguments))]
    (when-not (arms (:arm config))
      (throw (IllegalArgumentException.
              "--arm must be materialized, stream, stream-cancel, or reduce")))
    (when (or (> (:entries config) Integer/MAX_VALUE)
              (> (:batch-size config) Integer/MAX_VALUE))
      (throw (IllegalArgumentException. "--entries or --batch-size exceeds the JVM collection range")))
    config))

(defn- make-corpus [size]
  (mapv (fn [index]
          [(format "collection/%04x/%08x/shared-suffix"
                   (bit-and index 0x0fff) index)
           index])
        (range size)))

(defn- wrap-add [left right]
  (mod (+ left right) u64-modulus))

(defn- expected-checksum [corpus limit]
  (reduce (fn [checksum [key value]]
            (wrap-add checksum (bit-xor (count key) value)))
          0N
          (take limit (sort-by first corpus))))

(defn- entry-checksum [entry]
  (bit-xor (long (count (:key entry))) (long (or (:value entry) 0))))

(defn- drain-materialized [dictionary]
  (let [snapshot (d/snapshot dictionary)]
    {:checksum (reduce #(wrap-add %1 (entry-checksum %2)) 0N snapshot)
     :count (count snapshot)}))

(defn- drain-stream [dictionary batch-size limit]
  (d/with-entry-stream [stream dictionary batch-size]
    (loop [remaining (seq (d/stream-seq stream))
           checksum 0N
           consumed 0]
      (if (or (nil? remaining) (= consumed limit))
        {:checksum checksum :count consumed}
        (let [entry (first remaining)]
          (recur (next remaining)
                 (wrap-add checksum (entry-checksum entry))
                 (inc consumed)))))))

(defn- drain-reduce [dictionary batch-size]
  (d/reduce-entries
   dictionary batch-size
   (fn [{:keys [checksum count]} entry]
     {:checksum (wrap-add checksum (entry-checksum entry))
      :count (inc count)})
   {:checksum 0N :count 0}))

(defn- drain [dictionary config]
  (case (:arm config)
    "materialized" (drain-materialized dictionary)
    "stream" (drain-stream dictionary (:batch-size config) (:entries config))
    "stream-cancel" (drain-stream dictionary (:batch-size config)
                                  (min (:entries config) (:early-cancel config)))
    "reduce" (drain-reduce dictionary (:batch-size config))))

(defn- json-result [config consumed elapsed-ns checksum]
  (str "{\"schema\":\"libdictenstein.host-collection-traversal.v1\","
       "\"runtime\":\"clojure\",\"arm\":\"" (:arm config) "\","
       "\"dictionary_entries\":" (:entries config) ","
       "\"consumed_entries_per_pass\":" consumed ","
       "\"passes\":" (:passes config) ","
       "\"warmup_passes\":" (:warmup-passes config) ","
       "\"batch_size\":"
       (if (= "materialized" (:arm config)) "null" (:batch-size config)) ","
       "\"early_cancel\":"
       (if (= "stream-cancel" (:arm config)) (:early-cancel config) "null") ","
       "\"elapsed_ns\":" elapsed-ns ",\"checksum\":" checksum "}"))

(defn -main [& arguments]
  (try
    (let [config (parse-arguments arguments)
          corpus (make-corpus (:entries config))
          dictionary (d/dynamic-dawg :bytes)]
      (try
        (when-not (= (:entries config) (d/put-all! dictionary (into {} corpus)))
          (throw (IllegalStateException. "generated corpus did not insert completely")))
        (let [consumed (if (= "stream-cancel" (:arm config))
                         (min (:entries config) (:early-cancel config))
                         (:entries config))
              expected (expected-checksum corpus consumed)]
          (dotimes [_ (:warmup-passes config)]
            (let [result (drain dictionary config)]
              (when-not (= {:checksum expected :count consumed} result)
                (throw (IllegalStateException.
                        "warmup checksum or cardinality mismatch")))))
          (let [started (System/nanoTime)
                checksum
                (loop [pass 0 total 0N]
                  (if (= pass (:passes config))
                    total
                    (let [result (drain dictionary config)]
                      (when-not (= {:checksum expected :count consumed} result)
                        (throw (IllegalStateException.
                                "timed checksum or cardinality mismatch")))
                      (recur (inc pass) (wrap-add total (:checksum result))))))
                elapsed (max 1 (- (System/nanoTime) started))
                aggregate (mod (* expected (:passes config)) u64-modulus)]
            (when-not (= aggregate checksum)
              (throw (IllegalStateException. "aggregate checksum mismatch")))
            (println (json-result config consumed elapsed checksum))))
        (finally
          (d/close! dictionary))))
    (catch Throwable error
      (binding [*out* *err*] (println (.getMessage error)))
      (System/exit 2))))
