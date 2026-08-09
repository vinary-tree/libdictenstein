(ns vinary-tree.libdictenstein
  "Idiomatic immutable-data facade over libdictenstein's Java FFM bindings."
  (:refer-clojure :exclude [contains? get remove])
  (:import
   (io.vinarytree.libdictenstein
    Dictionary Dictionary$Lookup DoubleArrayTrie DynamicDawg
    PersistentARTrie PersistentVocabulary Scdawg UnitDomain)
   (java.lang AutoCloseable Long)
   (java.nio.file Path)
   (java.util HashMap OptionalLong)))

(def ^:private domains
  {:bytes UnitDomain/BYTE
   :unicode UnitDomain/UNICODE_SCALAR
   :unicode-scalar UnitDomain/UNICODE_SCALAR
   :u64 UnitDomain/U64})

(defn- domain [value]
  (or (domains value)
      (throw (IllegalArgumentException. (str "unknown unit domain: " value)))))

(defn- unsigned-long [value]
  (when (some? value)
    (let [number (bigint value)]
      (when (or (neg? number) (>= number 18446744073709551616N))
        (throw (IllegalArgumentException. "dictionary value is outside u64")))
      (Long/parseUnsignedLong (str number)))))

(defn- optional [value]
  (if (some? value)
    (OptionalLong/of (unsigned-long value))
    (OptionalLong/empty)))

(defn- unsigned [^OptionalLong value]
  (when (.isPresent value)
    (bigint (Long/toUnsignedString (.getAsLong value)))))

(defn- java-entries [entries]
  (let [output (HashMap.)]
    (doseq [[term value] entries]
      (.put output (str term) (optional value)))
    output))

(defn abi-version
  "Native ABI version (LDICT_ABI_VERSION); always 1 for this family."
  []
  (Dictionary/abiVersion))

(defn api-revision
  "Compatible-additions revision within the ABI version (LDICT_API_REVISION)."
  []
  (Dictionary/apiRevision))

(defn dynamic-dawg
  "Construct an empty full-CRUD DynamicDAWG."
  ([] (DynamicDawg.))
  ([unit-domain] (DynamicDawg. (domain unit-domain))))

(defn double-array-trie
  "Build an immutable DAT from a map/sequence of [term value] pairs."
  ([entries] (DoubleArrayTrie. (java-entries entries)))
  ([entries unit-domain]
   (DoubleArrayTrie. (java-entries entries) (domain unit-domain))))

(defn scdawg
  "Construct an empty substring-indexing SCDAWG."
  ([] (Scdawg.))
  ([unit-domain] (Scdawg. (domain unit-domain))))

(defn create-persistent-artrie
  ([path] (PersistentARTrie/create (Path/of (str path) (make-array String 0))))
  ([path unit-domain]
   (PersistentARTrie/create
    (Path/of (str path) (make-array String 0))
    (domain unit-domain))))

(defn open-persistent-artrie
  ([path] (PersistentARTrie/open (Path/of (str path) (make-array String 0))))
  ([path unit-domain]
   (PersistentARTrie/open
    (Path/of (str path) (make-array String 0))
    (domain unit-domain))))

(defn create-persistent-vocabulary [path]
  (PersistentVocabulary/create (Path/of (str path) (make-array String 0))))

(defn open-persistent-vocabulary [path]
  (PersistentVocabulary/open (Path/of (str path) (make-array String 0))))

(defn size [^Dictionary dictionary] (.size dictionary))

(defn contains? [^Dictionary dictionary term]
  (.contains dictionary ^String term))

(defn get
  "Return {:present? boolean :value unsigned-integer-or-nil}."
  [^Dictionary dictionary term]
  (let [^Dictionary$Lookup result (.get dictionary ^String term)]
    {:present? (.present result)
     :value (unsigned (.value result))}))

(defn put! [dictionary term value]
  (.put dictionary ^String term (optional value)))

(defn put-all! [dictionary entries]
  (.putAllStrings dictionary (java-entries entries)))

(defn remove! [dictionary term]
  (.remove dictionary ^String term))

(defn clear! [^DynamicDawg dictionary] (.clear dictionary))
(defn compact! [^DynamicDawg dictionary] (.compact dictionary))
(defn checkpoint! [dictionary] (.checkpoint dictionary))

(defn contains-substring? [^Scdawg dictionary pattern]
  (.containsSubstring dictionary pattern))

(defn frequency [^Scdawg dictionary pattern]
  (.frequency dictionary pattern))

(defn vocabulary-term [^PersistentVocabulary vocabulary index]
  (.orElse (.term vocabulary (unsigned-long index)) nil))

(defn close! [resource] (.close ^AutoCloseable resource))
