(ns vinary-tree.libdictenstein
  "Idiomatic immutable-data facade over libdictenstein's Java FFM bindings."
  (:refer-clojure :exclude [contains? get remove])
  (:import
   (io.vinarytree.libdictenstein
    Dictionary Dictionary$Lookup DoubleArrayTrie DynamicDawg
    PersistentARTrie PersistentVocabulary Scdawg UnitDomain)
   (io.vinarytree.interop
    DictionaryEntry DictionaryEntryIterator DictionaryKey DictionarySnapshot
    DictionaryUnitDomain UnsignedLong)
   (java.lang AutoCloseable Long)
   (java.nio.file Path)
   (java.util HashMap Optional OptionalLong)))

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

(defn- unsigned-entry-value [^Optional value]
  (when (.isPresent value)
    (bigint (.toString ^UnsignedLong (.get value)))))

(defn- u64-array [tokens]
  (long-array (map unsigned-long tokens)))

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

(defn put-u64!
  "Insert or update a full-range u64-token term."
  [dictionary tokens value]
  (.put dictionary ^longs (u64-array tokens) (optional value)))

(defn put-all! [dictionary entries]
  (.putAllStrings dictionary (java-entries entries)))

(defn remove! [dictionary term]
  (.remove dictionary ^String term))

(defn remove-u64! [dictionary tokens]
  (.remove dictionary ^longs (u64-array tokens)))

(defn contains-u64? [^Dictionary dictionary tokens]
  (.contains dictionary ^longs (u64-array tokens)))

(defn get-u64
  "Return {:present? boolean :value unsigned-integer-or-nil} for u64 tokens."
  [^Dictionary dictionary tokens]
  (let [^Dictionary$Lookup result (.get dictionary ^longs (u64-array tokens))]
    {:present? (.present result)
     :value (unsigned (.value result))}))

(defn clear! [^DynamicDawg dictionary] (.clear dictionary))
(defn compact! [^DynamicDawg dictionary] (.compact dictionary))
(defn checkpoint! [dictionary] (.checkpoint dictionary))

(defn contains-substring? [^Scdawg dictionary pattern]
  (.containsSubstring dictionary pattern))

(defn frequency [^Scdawg dictionary pattern]
  (.frequency dictionary pattern))

(defn vocabulary-term [^PersistentVocabulary vocabulary index]
  (.orElse (.term vocabulary (unsigned-long index)) nil))

(defn- clojure-key [^DictionaryKey key]
  (let [unit-domain (.domain key)]
    (cond
      (= unit-domain DictionaryUnitDomain/BYTE)
      (mapv #(bit-and (long %) 0xff) (.bytes key))

      (= unit-domain DictionaryUnitDomain/UNICODE_SCALAR)
      (.unicode key)

      (= unit-domain DictionaryUnitDomain/U64)
      (mapv #(bigint (Long/toUnsignedString (long %))) (.u64 key))

      :else
      (throw (IllegalStateException. (str "unknown entry unit domain: " unit-domain))))))

(defn- clojure-entry [^DictionaryEntry entry]
  (let [^DictionaryKey key (.key entry)
        unit-domain (.domain key)]
    {:key (clojure-key key)
     :value (unsigned-entry-value (.value entry))
     :domain (cond
               (= unit-domain DictionaryUnitDomain/BYTE) :bytes
               (= unit-domain DictionaryUnitDomain/UNICODE_SCALAR) :unicode-scalar
               (= unit-domain DictionaryUnitDomain/U64) :u64
               :else
               (throw (IllegalStateException.
                       (str "unknown entry unit domain: " unit-domain))))}))

(defn snapshot
  "Capture one immutable revision as a persistent vector of entry maps.

  The vector is host-owned, lexicographically ordered, and naturally supports
  seq, reduce, transduce, and repeated traversal after the dictionary closes."
  [^Dictionary dictionary]
  (let [^DictionarySnapshot captured (.snapshot dictionary)]
    (mapv clojure-entry (.orderedEntries captured))))

(def entries
  "Alias for snapshot; returns an immutable persistent vector."
  snapshot)

(defn entry-seq
  "Return a seq over one newly captured immutable revision."
  [dictionary]
  (seq (snapshot dictionary)))

(defn entry-eduction
  "Return an eduction over one newly captured immutable revision."
  ([dictionary]
   (eduction identity (snapshot dictionary)))
  ([dictionary xform]
   (eduction xform (snapshot dictionary))))

(defn open-entry-stream
  "Open a closeable, single-pass iterator over one immutable revision."
  ([^Dictionary dictionary]
   (.openEntryStream dictionary))
  ([^Dictionary dictionary batch-size]
   (.openEntryStream dictionary (int batch-size))))

(defn stream-seq
  "Adapt an open EntryStream to a lazy seq of immutable Clojure entry maps.

  Consume the result inside with-open; abandoning a lazy seq does not itself
  close its native cursor."
  [^DictionaryEntryIterator stream]
  (map clojure-entry (iterator-seq stream)))

(defmacro with-entry-stream
  "Bind a stream for body and close it after normal return, reduced traversal,
  or exception. Binding forms are [name dictionary] or
  [name dictionary batch-size]."
  [[binding dictionary & [batch-size]] & body]
  `(with-open [~binding (open-entry-stream ~dictionary ~@(when batch-size [batch-size]))]
     ~@body))

(defn reduce-entries
  "Resource-scoped streaming reduce over one immutable revision."
  ([dictionary reducing-function initial]
   (reduce-entries dictionary 256 reducing-function initial))
  ([dictionary batch-size reducing-function initial]
   (with-entry-stream [stream dictionary batch-size]
     (reduce reducing-function initial (stream-seq stream)))))

(defn transduce-entries
  "Resource-scoped streaming transduction over one immutable revision."
  ([dictionary xform reducing-function initial]
   (transduce-entries dictionary 256 xform reducing-function initial))
  ([dictionary batch-size xform reducing-function initial]
   (with-entry-stream [stream dictionary batch-size]
     (transduce xform reducing-function initial (stream-seq stream)))))

(defn close! [resource] (.close ^AutoCloseable resource))
