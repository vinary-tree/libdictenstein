use crate::value::DictionaryValue;
use std::collections::VecDeque;

/// A label that can be placed in a double-array trie's integer address space.
pub(crate) trait DoubleArrayUnit: Copy + Ord {
    fn code(self) -> usize;
}

impl DoubleArrayUnit for u8 {
    #[inline]
    fn code(self) -> usize {
        usize::from(self)
    }
}

impl DoubleArrayUnit for char {
    #[inline]
    fn code(self) -> usize {
        self as usize
    }
}

struct TrieNode<U, V> {
    edges: Vec<(U, usize)>,
    is_final: bool,
    value: Option<V>,
}

impl<U, V> TrieNode<U, V> {
    fn new() -> Self {
        Self {
            edges: Vec::new(),
            is_final: false,
            value: None,
        }
    }
}

/// Components produced by the static double-array builder.
pub(crate) struct StaticDATComponents<U, V> {
    pub(crate) base: Vec<i32>,
    pub(crate) check: Vec<i32>,
    pub(crate) is_final: Vec<bool>,
    pub(crate) edges: Vec<Vec<U>>,
    pub(crate) values: Vec<Option<V>>,
    pub(crate) term_count: usize,
}

/// Generic two-phase builder for immutable byte- and character-keyed DATs.
///
/// Terms are first accumulated in a compact ordinary trie. Once every sibling
/// set is known, `build` assigns each set one collision-free BASE value. This
/// avoids the repeated child relocation performed by the incremental builder.
pub(crate) struct StaticDATBuilder<U, V> {
    nodes: Vec<TrieNode<U, V>>,
    term_count: usize,
}

impl<U, V> StaticDATBuilder<U, V>
where
    U: DoubleArrayUnit,
    V: DictionaryValue,
{
    pub(crate) fn new() -> Self {
        Self {
            nodes: vec![TrieNode::new()],
            term_count: 0,
        }
    }

    /// Inserts one term. Existing terms are updated in place, so the last
    /// supplied value wins regardless of input order.
    pub(crate) fn insert<I>(&mut self, term: I, value: Option<V>) -> bool
    where
        I: IntoIterator<Item = U>,
    {
        let mut state = 0usize;
        for label in term {
            let child = match self.nodes[state]
                .edges
                .binary_search_by_key(&label, |&(edge, _)| edge)
            {
                Ok(index) => self.nodes[state].edges[index].1,
                Err(index) => {
                    let child = self.nodes.len();
                    self.nodes.push(TrieNode::new());
                    self.nodes[state].edges.insert(index, (label, child));
                    child
                }
            };
            state = child;
        }

        let inserted = !self.nodes[state].is_final;
        if inserted {
            self.nodes[state].is_final = true;
            self.term_count += 1;
        }
        if value.is_some() || inserted {
            self.nodes[state].value = value;
        }
        inserted
    }

    /// Places the accumulated trie into BASE/CHECK arrays.
    ///
    /// `root_state` captures the two historical layouts: byte DATs reserve
    /// state 0 and root at 1, while character DATs root at state 0.
    pub(crate) fn build(mut self, root_state: usize) -> StaticDATComponents<U, V> {
        let initial_len = root_state + 1;
        let mut base = vec![-1; initial_len];
        let mut check = vec![-1; initial_len];
        let mut is_final = vec![false; initial_len];
        let mut edges = (0..initial_len).map(|_| Vec::new()).collect::<Vec<_>>();
        let mut values = vec![None; initial_len];
        let mut occupied = vec![true; initial_len];

        let mut queue = VecDeque::new();
        queue.push_back((0usize, root_state));
        let mut next_check_pos = root_state + 1;

        while let Some((trie_state, dat_state)) = queue.pop_front() {
            is_final[dat_state] = self.nodes[trie_state].is_final;
            values[dat_state] = self.nodes[trie_state].value.take();

            if self.nodes[trie_state].edges.is_empty() {
                continue;
            }

            let first_code = self.nodes[trie_state].edges[0].0.code();
            let last_code = self.nodes[trie_state]
                .edges
                .last()
                .expect("non-empty sibling set")
                .0
                .code();
            let max_base = (i32::MAX as usize)
                .checked_sub(last_code)
                .expect("double-array label exceeds the representable address space");

            let candidate_base = loop {
                let first_slot = next_check_pos.max(first_code + 1);
                next_check_pos = first_slot;
                while next_check_pos < occupied.len() && occupied[next_check_pos] {
                    next_check_pos += 1;
                }

                let candidate = next_check_pos - first_code;
                assert!(
                    candidate <= max_base,
                    "double-array trie exhausted every representable collision-free base"
                );

                if self.nodes[trie_state].edges.iter().all(|&(label, _)| {
                    let slot = candidate + label.code();
                    slot >= occupied.len() || !occupied[slot]
                }) {
                    break candidate;
                }
                next_check_pos += 1;
            };

            base[dat_state] =
                i32::try_from(candidate_base).expect("candidate BASE is bounded by i32::MAX");
            let parent = i32::try_from(dat_state)
                .expect("double-array state index exceeds the CHECK representation");
            edges[dat_state].reserve(self.nodes[trie_state].edges.len());

            for &(label, child_trie_state) in &self.nodes[trie_state].edges {
                let child_dat_state = candidate_base + label.code();
                ensure_len(
                    child_dat_state + 1,
                    &mut base,
                    &mut check,
                    &mut is_final,
                    &mut edges,
                    &mut values,
                    &mut occupied,
                );
                debug_assert!(!occupied[child_dat_state]);
                occupied[child_dat_state] = true;
                check[child_dat_state] = parent;
                edges[dat_state].push(label);
                queue.push_back((child_trie_state, child_dat_state));
            }
        }

        StaticDATComponents {
            base,
            check,
            is_final,
            edges,
            values,
            term_count: self.term_count,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn ensure_len<U, V>(
    len: usize,
    base: &mut Vec<i32>,
    check: &mut Vec<i32>,
    is_final: &mut Vec<bool>,
    edges: &mut Vec<Vec<U>>,
    values: &mut Vec<Option<V>>,
    occupied: &mut Vec<bool>,
) {
    if len <= base.len() {
        return;
    }
    base.resize(len, -1);
    check.resize(len, -1);
    is_final.resize(len, false);
    edges.resize_with(len, Vec::new);
    values.resize_with(len, || None);
    occupied.resize(len, false);
}

#[cfg(test)]
mod tests {
    use super::StaticDATBuilder;

    #[test]
    fn duplicate_updates_value_and_count_once() {
        let mut builder = StaticDATBuilder::<u8, u32>::new();
        assert!(builder.insert(b"cat".iter().copied(), Some(1)));
        assert!(!builder.insert(b"cat".iter().copied(), Some(2)));
        let built = builder.build(1);
        assert_eq!(built.term_count, 1);

        let c = built.base[1] as usize + b'c' as usize;
        let a = built.base[c] as usize + b'a' as usize;
        let t = built.base[a] as usize + b't' as usize;
        assert!(built.is_final[t]);
        assert_eq!(built.values[t], Some(2));
    }

    #[test]
    fn supports_empty_and_unsorted_terms() {
        let mut builder = StaticDATBuilder::<char, ()>::new();
        assert!(builder.insert("z".chars(), None));
        assert!(builder.insert("".chars(), None));
        assert!(builder.insert("a".chars(), None));
        let built = builder.build(0);
        assert_eq!(built.term_count, 3);
        assert!(built.is_final[0]);
        assert_eq!(built.edges[0], vec!['a', 'z']);
    }
}
