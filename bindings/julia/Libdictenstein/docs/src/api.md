# API reference

## Dictionaries and constructors

```@docs
Dictionary
DynamicDawg
SortedMinimalDawg
PathMap
DoubleArrayTrie
Scdawg
SuffixIndex
SuffixSnapshot
PersistentARTrie
PersistentVocabulary
```

## Collections, snapshots, and maintenance

```@docs
insert_batch!
compact!
checkpoint!
snapshot
source_snapshot
source_identity
source_page
source_records
kind
capabilities
close!
```

## Algebra and specialized operations

```@docs
algebra
intersection
difference
symmetric_difference
contains_substring
substring_frequency
contains_source
insert_source!
remove_source!
vocabulary_term
```

## Errors and compatibility

```@docs
NativeError
abi_version
api_revision
```
