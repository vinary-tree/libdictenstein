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

## Julia-hosted providers

```@docs
AbstractHostDictionarySnapshot
HostDictionaryGraph
host_dictionary
provider_capture
provider_unit_domain
provider_value_domain
provider_root
provider_isfinal
provider_value
provider_length
provider_edge_page
provider_transition
provider_visit
provider_identity
provider_graph
provider_graph_value
provider_entries
```

## Collections, snapshots, and maintenance

```@docs
insert_batch!
lookup_batch
remove_batch!
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
AlgebraEntries
algebra_entries
prefix_entries
next_page!
fold_entries
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
