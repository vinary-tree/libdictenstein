//! Stack-safe helpers shared by byte- and character-overlay tests.

use std::sync::Arc;

use smallvec::SmallVec;

use crate::persistent_artrie::core::key_encoding::KeyEncoding;

use super::node::{Child, OverlayNode};

type InsertSpine<K, V> = SmallVec<[(Arc<OverlayNode<K, V>>, <K as KeyEncoding>::Unit); 32]>;
type VisitContinuation<K, V> = SmallVec<
    [(
        Arc<OverlayNode<K, V>>,
        usize,
        Option<<K as KeyEncoding>::Unit>,
    ); 32],
>;
type ResolvedChildren<K, V> = SmallVec<[(<K as KeyEncoding>::Unit, Arc<OverlayNode<K, V>>); 16]>;

/// Insert one path by immutable path copying without consuming native stack per edge.
pub(crate) fn insert_path<K, V>(
    root: Arc<OverlayNode<K, V>>,
    units: &[K::Unit],
) -> Arc<OverlayNode<K, V>>
where
    K: KeyEncoding,
    V: Clone,
{
    let mut current = root;
    let mut spine = InsertSpine::<K, V>::new();
    let mut missing_tail_start = None;

    for (offset, &edge) in units.iter().enumerate() {
        spine.push((current.clone(), edge));
        match current.find_child(edge).and_then(Child::as_in_mem) {
            Some(child) => current = child.clone(),
            None => {
                missing_tail_start = Some(offset + 1);
                break;
            }
        }
    }

    let mut replacement = if let Some(tail_start) = missing_tail_start {
        let mut tail = Arc::new(OverlayNode::<K, V>::new().as_final());
        for &edge in units[tail_start..].iter().rev() {
            tail = Arc::new(OverlayNode::<K, V>::new().with_child(edge, Child::InMem(tail)));
        }
        tail
    } else {
        Arc::new(current.as_final())
    };

    while let Some((parent, edge)) = spine.pop() {
        replacement = Arc::new(parent.with_child(edge, Child::InMem(replacement)));
    }
    replacement
}

/// Visit an overlay in deterministic depth-first order with one heap-backed continuation stack.
///
/// Every immediate child is resolved before descent, matching the former recursive fixture
/// semantics. `path` is restored to its entry length before this function returns.
pub(crate) fn visit_paths<K, V, Resolve, Visit>(
    root: &Arc<OverlayNode<K, V>>,
    path: &mut Vec<K::Unit>,
    mut resolve: Resolve,
    mut visit: Visit,
) where
    K: KeyEncoding,
    V: Clone,
    Resolve: FnMut(&Child<K, V>) -> Arc<OverlayNode<K, V>>,
    Visit: FnMut(&[K::Unit], &Arc<OverlayNode<K, V>>),
{
    let initial_path_len = path.len();
    let mut pending = VisitContinuation::<K, V>::new();
    pending.push((root.clone(), initial_path_len, None));

    while let Some((node, parent_path_len, incoming)) = pending.pop() {
        path.truncate(parent_path_len);
        if let Some(edge) = incoming {
            path.push(edge);
        }

        visit(path, &node);

        let child_path_len = path.len();
        let mut children: ResolvedChildren<K, V> = node
            .iter_children()
            .map(|(&edge, child)| (edge, resolve(child)))
            .collect();
        while let Some((edge, child)) = children.pop() {
            pending.push((child, child_path_len, Some(edge)));
        }
    }

    path.truncate(initial_path_len);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistent_artrie::core::key_encoding::ByteKey;

    const DEEP_PATH: usize = 100_000;

    #[test]
    fn immutable_path_copy_spills_to_the_heap_without_native_stack_growth() {
        let units = vec![0_u8; DEEP_PATH];
        let root = insert_path(
            Arc::new(OverlayNode::<ByteKey, ()>::new()),
            units.as_slice(),
        );

        let mut current = root;
        for _ in 0..DEEP_PATH {
            current = current
                .find_child(0)
                .and_then(Child::as_in_mem)
                .expect("the inserted spine is complete")
                .clone();
        }
        assert!(current.is_final());
    }

    #[test]
    fn ordered_visit_spills_one_pending_sibling_per_level() {
        let shared_final = Arc::new(OverlayNode::<ByteKey, ()>::new().as_final());
        let mut root = shared_final.clone();
        for _ in 0..DEEP_PATH {
            root = Arc::new(
                OverlayNode::<ByteKey, ()>::new()
                    .with_child(0, Child::InMem(root))
                    .with_child(1, Child::InMem(shared_final.clone())),
            );
        }

        let mut final_count = 0;
        let mut deepest_path = 0;
        visit_paths(
            &root,
            &mut Vec::new(),
            |child| {
                child
                    .as_in_mem()
                    .expect("the fixture is entirely resident")
                    .clone()
            },
            |path, node| {
                deepest_path = deepest_path.max(path.len());
                final_count += usize::from(node.is_final());
            },
        );

        assert_eq!(deepest_path, DEEP_PATH);
        assert_eq!(final_count, DEEP_PATH + 1);
    }
}
