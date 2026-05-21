//! DFS iterators over a byte `PersistentARTrie`.
//!
//! Split out of byte `dict_impl.rs` (lines ~5853-6219) as part of the
//! Phase-5 decomposition. The internal `IterState` enum + the two public
//! `TermIterator<V>` / `TermValueIterator<V>` types form a coherent DFS
//! traversal subsystem. The `iter()` / `iter_with_values()` accessor
//! methods on `PersistentARTrie<V, S>` stay in `dict_impl.rs` and call the
//! `pub(super) fn new` constructors here.

use crate::value::DictionaryValue;

use super::dict_impl::TrieRoot;
use super::transitions::ChildNode;

/// Iterator state for DFS traversal of the trie
#[derive(Clone)]
pub(super) enum IterState {
    /// Iterating over a bucket's entries
    Bucket {
        /// Current prefix (path to this bucket)
        prefix: Vec<u8>,
        /// Entries to iterate (suffix, value_bytes)
        entries: Vec<(Vec<u8>, Option<Vec<u8>>)>,
        /// Current index in entries
        index: usize,
    },
    /// Iterating over an ART node's children
    ArtNode {
        /// Current prefix (path to this node)
        prefix: Vec<u8>,
        /// Whether this node is final (represents a term)
        is_final: bool,
        /// Value at this node if final
        value: Option<Vec<u8>>,
        /// Whether we've yielded the final state yet
        yielded_final: bool,
        /// Children to visit (edge byte, child)
        children: Vec<(u8, ChildNode)>,
        /// Current child index
        child_index: usize,
    },
}

/// Iterator over all terms in a PersistentARTrie.
///
/// This iterator performs a depth-first traversal of the trie,
/// yielding terms in lexicographic order.
///
/// # Example
///
/// ```rust,ignore
/// use libdictenstein::persistent_artrie::PersistentARTrie;
///
/// let mut dict = PersistentARTrie::new();
/// dict.insert("apple");
/// dict.insert("banana");
///
/// for term in dict.iter() {
///     println!("{}", String::from_utf8_lossy(&term));
/// }
/// ```
pub struct TermIterator<V: DictionaryValue> {
    /// Stack of iteration states for DFS
    stack: Vec<IterState>,
    /// Marker for value type
    _marker: std::marker::PhantomData<V>,
}

impl<V: DictionaryValue> TermIterator<V> {
    /// Create a new iterator starting from the trie root.
    pub(super) fn new(root: &TrieRoot<V>) -> Self {
        let mut stack = Vec::new();

        match root {
            TrieRoot::Bucket(bucket) => {
                // Collect all bucket entries
                let entries: Vec<(Vec<u8>, Option<Vec<u8>>)> = bucket
                    .iter()
                    .map(|(entry, suffix)| {
                        let value = bucket.get_value(&entry).map(|v| v.to_vec());
                        (suffix.to_vec(), value)
                    })
                    .collect();

                if !entries.is_empty() {
                    stack.push(IterState::Bucket {
                        prefix: Vec::new(),
                        entries,
                        index: 0,
                    });
                }
            }
            TrieRoot::ArtNode {
                is_final,
                value,
                children,
                ..
            } => {
                // Serialize value if present
                let value_bytes = value.as_ref().and_then(|v| crate::serialization::bincode_compat::serialize(v).ok());
                let _ = value; // Silence unused warning

                stack.push(IterState::ArtNode {
                    prefix: Vec::new(),
                    is_final: *is_final,
                    value: value_bytes,
                    yielded_final: false,
                    children: children.clone(),
                    child_index: 0,
                });
            }
        }

        Self {
            stack,
            _marker: std::marker::PhantomData,
        }
    }
}

impl<V: DictionaryValue> Iterator for TermIterator<V> {
    type Item = Vec<u8>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let state = self.stack.last_mut()?;

            match state {
                IterState::Bucket {
                    prefix,
                    entries,
                    index,
                } => {
                    if *index < entries.len() {
                        let (suffix, _value) = &entries[*index];
                        let mut term = prefix.clone();
                        term.extend_from_slice(suffix);
                        *index += 1;
                        return Some(term);
                    } else {
                        // Done with this bucket
                        self.stack.pop();
                    }
                }
                IterState::ArtNode {
                    prefix,
                    is_final,
                    yielded_final,
                    children,
                    child_index,
                    ..
                } => {
                    // First, yield the final state if applicable
                    if *is_final && !*yielded_final {
                        *yielded_final = true;
                        return Some(prefix.clone());
                    }

                    // Then, process children
                    if *child_index < children.len() {
                        let (edge, child) = children[*child_index].clone();
                        *child_index += 1;

                        let mut child_prefix = prefix.clone();
                        child_prefix.push(edge);

                        // Push child state onto stack
                        match child {
                            ChildNode::Bucket(bucket) => {
                                let entries: Vec<(Vec<u8>, Option<Vec<u8>>)> = bucket
                                    .iter()
                                    .map(|(entry, suffix)| {
                                        let value = bucket.get_value(&entry).map(|v| v.to_vec());
                                        (suffix.to_vec(), value)
                                    })
                                    .collect();

                                if !entries.is_empty() {
                                    self.stack.push(IterState::Bucket {
                                        prefix: child_prefix,
                                        entries,
                                        index: 0,
                                    });
                                }
                            }
                            ChildNode::ArtNode {
                                is_final: child_final,
                                value: child_value,
                                children: child_children,
                                ..
                            } => {
                                self.stack.push(IterState::ArtNode {
                                    prefix: child_prefix,
                                    is_final: child_final,
                                    value: child_value,
                                    yielded_final: false,
                                    children: child_children,
                                    child_index: 0,
                                });
                            }
                            ChildNode::DiskRef { .. } => {
                                // Skip disk refs for now - they would need async loading
                                // In a full implementation, we'd resolve them here
                            }
                        }
                    } else {
                        // Done with this ART node
                        self.stack.pop();
                    }
                }
            }
        }
    }
}

/// Iterator over all terms with their values in a PersistentARTrie.
///
/// This iterator performs a depth-first traversal of the trie,
/// yielding (term, value) pairs in lexicographic order.
pub struct TermValueIterator<V: DictionaryValue> {
    /// Stack of iteration states for DFS
    stack: Vec<IterState>,
    /// Marker for value type
    _marker: std::marker::PhantomData<V>,
}

impl<V: DictionaryValue> TermValueIterator<V> {
    /// Create a new iterator starting from the trie root.
    pub(super) fn new(root: &TrieRoot<V>) -> Self {
        let mut stack = Vec::new();

        match root {
            TrieRoot::Bucket(bucket) => {
                let entries: Vec<(Vec<u8>, Option<Vec<u8>>)> = bucket
                    .iter()
                    .map(|(entry, suffix)| {
                        let value = bucket.get_value(&entry).map(|v| v.to_vec());
                        (suffix.to_vec(), value)
                    })
                    .collect();

                if !entries.is_empty() {
                    stack.push(IterState::Bucket {
                        prefix: Vec::new(),
                        entries,
                        index: 0,
                    });
                }
            }
            TrieRoot::ArtNode {
                is_final,
                value,
                children,
                ..
            } => {
                let value_bytes = value.as_ref().and_then(|v| crate::serialization::bincode_compat::serialize(v).ok());
                let _ = value;

                stack.push(IterState::ArtNode {
                    prefix: Vec::new(),
                    is_final: *is_final,
                    value: value_bytes,
                    yielded_final: false,
                    children: children.clone(),
                    child_index: 0,
                });
            }
        }

        Self {
            stack,
            _marker: std::marker::PhantomData,
        }
    }
}

impl<V: DictionaryValue> Iterator for TermValueIterator<V> {
    type Item = (Vec<u8>, Option<V>);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let state = self.stack.last_mut()?;

            match state {
                IterState::Bucket {
                    prefix,
                    entries,
                    index,
                } => {
                    if *index < entries.len() {
                        let (suffix, value_bytes) = &entries[*index];
                        let mut term = prefix.clone();
                        term.extend_from_slice(suffix);

                        // Deserialize value if present
                        let value: Option<V> = value_bytes
                            .as_ref()
                            .and_then(|bytes| crate::serialization::bincode_compat::deserialize(bytes).ok());
                        let _ = value_bytes;

                        *index += 1;
                        return Some((term, value));
                    } else {
                        self.stack.pop();
                    }
                }
                IterState::ArtNode {
                    prefix,
                    is_final,
                    value: value_bytes,
                    yielded_final,
                    children,
                    child_index,
                } => {
                    if *is_final && !*yielded_final {
                        *yielded_final = true;

                        let value: Option<V> = value_bytes
                            .as_ref()
                            .and_then(|bytes| crate::serialization::bincode_compat::deserialize(bytes).ok());
                        let _ = value_bytes;

                        return Some((prefix.clone(), value));
                    }

                    if *child_index < children.len() {
                        let (edge, child) = children[*child_index].clone();
                        *child_index += 1;

                        let mut child_prefix = prefix.clone();
                        child_prefix.push(edge);

                        match child {
                            ChildNode::Bucket(bucket) => {
                                let entries: Vec<(Vec<u8>, Option<Vec<u8>>)> = bucket
                                    .iter()
                                    .map(|(entry, suffix)| {
                                        let value = bucket.get_value(&entry).map(|v| v.to_vec());
                                        (suffix.to_vec(), value)
                                    })
                                    .collect();

                                if !entries.is_empty() {
                                    self.stack.push(IterState::Bucket {
                                        prefix: child_prefix,
                                        entries,
                                        index: 0,
                                    });
                                }
                            }
                            ChildNode::ArtNode {
                                is_final: child_final,
                                value: child_value,
                                children: child_children,
                                ..
                            } => {
                                self.stack.push(IterState::ArtNode {
                                    prefix: child_prefix,
                                    is_final: child_final,
                                    value: child_value,
                                    yielded_final: false,
                                    children: child_children,
                                    child_index: 0,
                                });
                            }
                            ChildNode::DiskRef { .. } => {
                                // Skip disk refs for now
                            }
                        }
                    } else {
                        self.stack.pop();
                    }
                }
            }
        }
    }
}
