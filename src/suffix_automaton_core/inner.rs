//! Generic `SuffixAutomatonInner<U, V>` shared by byte and char variants.
//!
//! The on-line `extend(unit: U)` algorithm + the empty-root constructor
//! `new()` are byte-for-byte identical between the byte (`Unit = u8`) and
//! char (`Unit = char`) variants — they only differ in the edge-label type.
//! This generic module hosts both so the variants can share a single
//! implementation via `pub(crate) type` aliases (C3 algorithmic dedup).

use std::collections::HashMap;

use super::node::SuffixNode;
use crate::value::DictionaryValue;
use crate::CharUnit;

/// Internal state of the suffix automaton.
///
/// This is wrapped in `Arc<RwLock<...>>` to provide thread-safe concurrent
/// access with dynamic mutation support.
#[derive(Debug)]
#[cfg_attr(feature = "serialization", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    all(feature = "serialization", not(feature = "persistent-artrie")),
    serde(bound(
        serialize = "U: serde::Serialize, V: serde::Serialize",
        deserialize = "U: serde::Deserialize<'de>, V: serde::Deserialize<'de>",
    ))
)]
#[cfg_attr(
    all(feature = "serialization", feature = "persistent-artrie"),
    serde(bound(
        serialize = "U: serde::Serialize, V: serde::Serialize",
        deserialize = "U: serde::de::DeserializeOwned, V: serde::de::DeserializeOwned",
    ))
)]
pub struct SuffixAutomatonInner<U: CharUnit, V: DictionaryValue = ()> {
    /// Node storage (index-based graph). State 0 is always the root.
    pub nodes: Vec<SuffixNode<U, V>>,
    /// Current state during online construction (the last state added).
    pub last_state: usize,
    /// Total number of indexed strings.
    pub string_count: usize,
    /// Original source texts for serialization.
    pub source_texts: Vec<String>,
    /// Position metadata: maps state IDs to (string_id, end_position).
    pub positions: HashMap<usize, Vec<(usize, usize)>>,
    /// Flag indicating compaction is recommended.
    pub needs_compaction: bool,
}

impl<U: CharUnit, V: DictionaryValue> SuffixAutomatonInner<U, V> {
    /// Create an empty suffix automaton with a single root state.
    pub fn new() -> Self {
        Self {
            nodes: vec![SuffixNode::root()],
            last_state: 0,
            string_count: 0,
            source_texts: Vec::new(),
            positions: HashMap::new(),
            needs_compaction: false,
        }
    }

    /// Extend the automaton with one unit (on-line construction).
    ///
    /// Implements the Blumer et al. (1985) construction algorithm.
    /// Complexity: O(1) amortized per unit; adds 1 state plus possibly 1
    /// clone when an equivalence class needs to be split.
    pub fn extend(&mut self, unit: U) {
        let cur = self.nodes.len();
        let mut new_node = SuffixNode::new(self.nodes[self.last_state].max_length + 1);
        new_node.is_final = true;
        self.nodes.push(new_node);

        // Walk suffix links backward, adding transitions to the new state.
        let mut p = Some(self.last_state);
        while let Some(p_idx) = p {
            if self.nodes[p_idx].find_edge(unit).is_some() {
                break;
            }
            self.nodes[p_idx].add_edge(unit, cur);
            p = self.nodes[p_idx].suffix_link;
        }

        if let Some(p_idx) = p {
            // Invariant: the suffix-link walk exits only when
            // `find_edge(unit).is_some()`, so the edge exists by construction.
            let q = self.nodes[p_idx]
                .find_edge(unit)
                .expect("suffix-link walk exited with a known edge for unit at p_idx");

            if self.nodes[p_idx].max_length + 1 == self.nodes[q].max_length {
                // Continuous transition — no split needed.
                self.nodes[cur].suffix_link = Some(q);
            } else {
                // Split equivalence class by cloning state q.
                let clone = self.nodes.len();
                let mut cloned_node = self.nodes[q].clone();
                cloned_node.max_length = self.nodes[p_idx].max_length + 1;
                cloned_node.is_final = true;
                self.nodes.push(cloned_node);

                self.nodes[cur].suffix_link = Some(clone);
                self.nodes[q].suffix_link = Some(clone);

                // Redirect transitions from states along the suffix-link path.
                let mut p2 = Some(p_idx);
                while let Some(p2_idx) = p2 {
                    if let Some(target) = self.nodes[p2_idx].find_edge(unit) {
                        if target == q {
                            self.nodes[p2_idx].update_edge(unit, clone);
                            p2 = self.nodes[p2_idx].suffix_link;
                        } else {
                            break;
                        }
                    } else {
                        break;
                    }
                }
            }
        } else {
            // Reached root without finding the transition — simple case.
            self.nodes[cur].suffix_link = Some(0);
        }

        self.last_state = cur;
    }
}

impl<U: CharUnit, V: DictionaryValue> Default for SuffixAutomatonInner<U, V> {
    fn default() -> Self {
        Self::new()
    }
}
