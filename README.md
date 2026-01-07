# libdictenstein

High-performance dictionary backends for fuzzy string matching with Levenshtein automata.

## Overview

libdictenstein provides multiple dictionary implementations optimized for approximate string matching. It is designed to work seamlessly with [liblevenshtein](https://github.com/universal-automata/liblevenshtein-rust) for fuzzy search, spell checking, and code completion.

## Dictionary Backends

| Backend | Best For | Performance | Memory | Dynamic Updates | Unicode |
|---------|----------|-------------|--------|-----------------|---------|
| **DoubleArrayTrie** | General use (recommended) | 5/5 | 5/5 | Insert-only | Byte-level |
| **DoubleArrayTrieChar** | Unicode text | 4/5 | 4/5 | Insert-only | Character-level |
| **DynamicDawg** | Insert + Remove | 3/5 | 3/5 | Thread-safe | Byte-level |
| **DynamicDawgChar** | Unicode + Insert + Remove | 3/5 | 3/5 | Thread-safe | Character-level |
| **DynamicDawgU64** | Token sequences, time series | 3/5 | 2/5 | Thread-safe | 64-bit labels |
| **SuffixAutomaton** | Substring search | 3/5 | 2/5 | Insert + Remove | Byte-level |
| **SuffixAutomatonChar** | Unicode substring search | 3/5 | 2/5 | Insert + Remove | Character-level |

## Quick Start

```rust
use libdictenstein::prelude::*;
use libdictenstein::double_array_trie::DoubleArrayTrie;

// Create a dictionary
let dict = DoubleArrayTrie::from_terms(vec!["hello", "help", "world"]);

// Check if a term exists
assert!(dict.contains("hello"));

// Traverse the dictionary
let root = dict.root();
if let Some(next) = root.transition(b'h') {
    println!("Found edge 'h'");
}
```

## Features

- **pathmap-backend**: Enable PathMap dictionary backend
- **serialization**: Enable serde serialization support
- **persistent-artrie**: Enable disk-backed Adaptive Radix Trie
- **simd**: Enable SIMD optimizations for edge lookup
- **scdawg-bloom**: Enable bloom filter optimization for SCDAWG
- **scdawg-simd**: Enable SIMD optimization for SCDAWG

## Core Traits

### Dictionary

The main trait for dictionary backends:

```rust
pub trait Dictionary {
    type Node: DictionaryNode;

    fn root(&self) -> Self::Node;
    fn contains(&self, term: &str) -> bool;
    fn len(&self) -> Option<usize>;
}
```

### DictionaryNode

Represents traversable nodes in the dictionary graph:

```rust
pub trait DictionaryNode: Clone + Send + Sync {
    type Unit: CharUnit;

    fn is_final(&self) -> bool;
    fn transition(&self, label: Self::Unit) -> Option<Self>;
    fn edges(&self) -> Box<dyn Iterator<Item = (Self::Unit, Self)> + '_>;
}
```

### MappedDictionary

For dictionaries that associate values with terms:

```rust
pub trait MappedDictionary: Dictionary {
    type Value: DictionaryValue;

    fn get_value(&self, term: &str) -> Option<Self::Value>;
}
```

## Performance

Benchmarks with 10,000 words:

```text
Construction:  DAT: 3.2ms    DAWG: 7.2ms
Exact Match:   DAT: 6.6us    DAWG: 19.8us
Contains:      DAT: 0.22us   DAWG: 6.7us
```

## Migration from liblevenshtein

If you were using `liblevenshtein::dictionary::*`, update your imports:

```rust
// Old
use liblevenshtein::dictionary::{Dictionary, DictionaryNode};
use liblevenshtein::dictionary::double_array_trie::DoubleArrayTrie;

// New
use libdictenstein::{Dictionary, DictionaryNode};
use libdictenstein::double_array_trie::DoubleArrayTrie;

// Or use the prelude
use libdictenstein::prelude::*;
```

## License

Apache-2.0
