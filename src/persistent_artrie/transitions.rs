//! Bucket ↔ ART Node Transitions
//!
//! This module handles the transitions between bucket leaf nodes and ART internal nodes.
//! These transitions occur when:
//!
//! 1. **Bucket → ART**: A bucket becomes full and needs to be converted to an ART node
//!    with child buckets (one per first-byte of entries)
//!
//! 2. **ART → Bucket**: An ART node's children all become small enough to be merged
//!    back into a single bucket
//!
//! # Architecture
//!
//! ```text
//! Before (single bucket):
//! ┌─────────────────────────────────────────┐
//! │ Bucket: ["apple", "apricot", "banana",  │
//! │          "berry", "cherry"]             │
//! └─────────────────────────────────────────┘
//!
//! After (ART node with child buckets):
//! ┌─────────────┐
//! │  Node4      │
//! │ a→ b→ c→    │
//! └──┬──┬──┬────┘
//!    │  │  │
//!    │  │  └─► Bucket: ["herry"]
//!    │  │
//!    │  └────► Bucket: ["anana", "erry"]
//!    │
//!    └───────► Bucket: ["pple", "pricot"]
//! ```

/// A borrowed view of an `ArtNode`'s fields: header, terminal flag,
/// path-compressed prefix, and `(key unit, child)` edges.
type ArtNodeView<'a> = (
    &'a Node,
    bool,
    &'a Option<Vec<u8>>,
    &'a Vec<(u8, ChildNode)>,
);

/// The mutable counterpart of [`ArtNodeView`].
type ArtNodeViewMut<'a> = (
    &'a mut Node,
    &'a mut bool,
    &'a mut Option<Vec<u8>>,
    &'a mut Vec<(u8, ChildNode)>,
);

use std::fmt;

use smallvec::SmallVec;

use super::bucket::StringBucket;
use super::nodes::Node;
use super::swizzled_ptr::SwizzledPtr;

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default)]
struct ChildNodeDropProbeState {
    enabled: bool,
    invocations: usize,
    current_depth: usize,
    maximum_depth: usize,
}

#[cfg(test)]
std::thread_local! {
    static CHILD_NODE_DROP_PROBE: std::cell::Cell<ChildNodeDropProbeState> =
        const { std::cell::Cell::new(ChildNodeDropProbeState {
            enabled: false,
            invocations: 0,
            current_depth: 0,
            maximum_depth: 0,
        }) };
}

#[cfg(test)]
struct ChildNodeDropProbeGuard {
    enabled: bool,
}

#[cfg(test)]
impl ChildNodeDropProbeGuard {
    fn enter() -> Self {
        let enabled = CHILD_NODE_DROP_PROBE.with(|probe| {
            let mut state = probe.get();
            if state.enabled {
                state.invocations = state
                    .invocations
                    .checked_add(1)
                    .expect("test Drop invocation counter must not overflow");
                state.current_depth = state
                    .current_depth
                    .checked_add(1)
                    .expect("test Drop depth counter must not overflow");
                state.maximum_depth = state.maximum_depth.max(state.current_depth);
                probe.set(state);
            }
            state.enabled
        });
        Self { enabled }
    }
}

#[cfg(test)]
impl Drop for ChildNodeDropProbeGuard {
    fn drop(&mut self) {
        if self.enabled {
            CHILD_NODE_DROP_PROBE.with(|probe| {
                let mut state = probe.get();
                state.current_depth = state
                    .current_depth
                    .checked_sub(1)
                    .expect("test Drop depth must be positive while leaving");
                probe.set(state);
            });
        }
    }
}

#[cfg(test)]
fn start_child_node_drop_probe() {
    CHILD_NODE_DROP_PROBE.with(|probe| {
        assert!(
            !probe.get().enabled,
            "ChildNode Drop probe is already active"
        );
        probe.set(ChildNodeDropProbeState {
            enabled: true,
            ..ChildNodeDropProbeState::default()
        });
    });
}

#[cfg(test)]
fn finish_child_node_drop_probe() -> ChildNodeDropProbeState {
    CHILD_NODE_DROP_PROBE.with(|probe| {
        let mut state = probe.get();
        assert!(state.enabled, "ChildNode Drop probe is not active");
        assert_eq!(
            state.current_depth, 0,
            "a ChildNode Drop call is still active"
        );
        state.enabled = false;
        probe.set(state);
        state
    })
}

// L3.3c: removed — the owned bucket↔ART transition surface
// (BUCKET_TO_ART_THRESHOLD / ART_TO_BUCKET_THRESHOLD, BucketToArtResult /
// ArtToBucketResult / TransitionError, should_convert_bucket_to_art /
// bucket_to_art_node / should_merge_art_to_bucket / art_node_to_bucket). These built
// the deleted owned trie's bucket→ART promotions / ART→bucket merges. The lock-free
// overlay is un-path-compressed (no buckets, no promotions). The `ChildNode` enum +
// its decode/overlay helper methods are KEPT below (the disk-decode path + the
// `serialize_*`/`resolve_disk_ref` surface still use them).

/// Represents a child pointer that can be either a bucket or an ART node.
///
/// Its generated lifecycle operations would recurse through `children` and can
/// exhaust the native stack for adversarially deep decoded images. The manual
/// [`Clone`], [`fmt::Debug`], and [`Drop`] implementations below are explicit
/// depth-bounded machines. Because this type implements [`Drop`], callers must
/// borrow variant fields or use the accessors rather than move a field directly
/// out of an owned `ChildNode` pattern.
pub enum ChildNode {
    /// A bucket leaf node
    Bucket(StringBucket),
    /// An ART internal node with its own children
    ArtNode {
        /// The node itself
        node: Node,
        /// Whether this node represents a final state
        is_final: bool,
        /// Value if this is a final state with a value
        value: Option<Vec<u8>>,
        /// Child nodes (for nested ART)
        children: Vec<(u8, ChildNode)>,
    },
    /// A disk-backed reference (not yet loaded)
    ///
    /// This variant is used for lazy loading. When accessed, the SwizzledPtr
    /// is resolved by loading the node from disk via the BufferManager.
    DiskRef {
        /// The swizzled pointer containing disk location
        ptr: SwizzledPtr,
    },
}

/// Suspended state for the iterative [`Clone`] implementation of [`ChildNode`].
///
/// Keeping one frame per ancestor avoids both native-stack recursion and the
/// node-count-sized worklist that a flat post-order traversal would require.
struct ChildNodeCloneFrame<'a> {
    node: Node,
    is_final: bool,
    value: Option<Vec<u8>>,
    source_children: &'a [(u8, ChildNode)],
    next_child: usize,
    cloned_children: Vec<(u8, ChildNode)>,
}

/// Suspended child-list state for the iterative [`fmt::Debug`] implementation.
struct ChildNodeDebugFrame<'a> {
    children: &'a [(u8, ChildNode)],
    next_child: usize,
    depth: usize,
}

/// Re-indents continuation lines emitted by a bounded non-`ChildNode` value.
///
/// `Node`, `StringBucket`, `SwizzledPtr`, booleans, and byte vectors may safely
/// use their standard pretty `Debug` implementations: none owns another
/// `ChildNode`. This adapter composes their output into the explicit-depth
/// pretty-printer without buffering it in a `String`.
struct DebugContinuationWriter<'formatter, 'buffer> {
    formatter: &'formatter mut fmt::Formatter<'buffer>,
    indentation: usize,
    at_line_start: bool,
}

impl fmt::Write for DebugContinuationWriter<'_, '_> {
    fn write_str(&mut self, mut value: &str) -> fmt::Result {
        while let Some(newline) = value.find('\n') {
            let (line, remainder) = value.split_at(newline + 1);
            if self.at_line_start {
                write_debug_indentation(self.formatter, self.indentation)?;
            }
            self.formatter.write_str(line)?;
            self.at_line_start = true;
            value = remainder;
        }

        if !value.is_empty() {
            if self.at_line_start {
                write_debug_indentation(self.formatter, self.indentation)?;
            }
            self.formatter.write_str(value)?;
            self.at_line_start = false;
        }
        Ok(())
    }
}

fn write_debug_indentation(formatter: &mut fmt::Formatter<'_>, indentation: usize) -> fmt::Result {
    for _ in 0..indentation {
        formatter.write_str("    ")?;
    }
    Ok(())
}

fn checked_debug_depth(depth: usize, additional: usize) -> Result<usize, fmt::Error> {
    depth.checked_add(additional).ok_or(fmt::Error)
}

fn write_nested_pretty_debug<T: fmt::Debug + ?Sized>(
    formatter: &mut fmt::Formatter<'_>,
    value: &T,
    continuation_indentation: usize,
) -> fmt::Result {
    let mut writer = DebugContinuationWriter {
        formatter,
        indentation: continuation_indentation,
        at_line_start: false,
    };
    fmt::write(&mut writer, format_args!("{value:#?}"))
}

fn fmt_child_node_compact(root: &ChildNode, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    let mut frames: SmallVec<[ChildNodeDebugFrame<'_>; 16]> = SmallVec::new();
    let mut current = root;

    loop {
        match current {
            ChildNode::Bucket(bucket) => write!(formatter, "Bucket({bucket:?})")?,
            ChildNode::DiskRef { ptr } => {
                write!(formatter, "DiskRef {{ ptr: {ptr:?} }}")?;
            }
            ChildNode::ArtNode {
                node,
                is_final,
                value,
                children,
            } => {
                write!(
                    formatter,
                    "ArtNode {{ node: {node:?}, is_final: {is_final:?}, value: {value:?}, children: ["
                )?;

                if children.is_empty() {
                    formatter.write_str("] }")?;
                } else {
                    write!(formatter, "({:?}, ", children[0].0)?;
                    frames.push(ChildNodeDebugFrame {
                        children,
                        next_child: 1,
                        depth: 0,
                    });
                    current = &children[0].1;
                    continue;
                }
            }
        }

        loop {
            let Some(frame) = frames.last_mut() else {
                return Ok(());
            };

            formatter.write_str(")")?;
            if frame.next_child < frame.children.len() {
                let next_child = frame.next_child;
                frame.next_child += 1;
                write!(formatter, ", ({:?}, ", frame.children[next_child].0)?;
                current = &frame.children[next_child].1;
                break;
            }

            formatter.write_str("] }")?;
            frames.pop();
        }
    }
}

fn fmt_child_node_pretty(root: &ChildNode, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    let mut frames: SmallVec<[ChildNodeDebugFrame<'_>; 16]> = SmallVec::new();
    let mut current = root;
    let mut current_depth = 0_usize;

    loop {
        match current {
            ChildNode::Bucket(bucket) => {
                let field_depth = checked_debug_depth(current_depth, 1)?;
                formatter.write_str("Bucket(\n")?;
                write_debug_indentation(formatter, field_depth)?;
                write_nested_pretty_debug(formatter, bucket, field_depth)?;
                formatter.write_str(",\n")?;
                write_debug_indentation(formatter, current_depth)?;
                formatter.write_str(")")?;
            }
            ChildNode::DiskRef { ptr } => {
                let field_depth = checked_debug_depth(current_depth, 1)?;
                formatter.write_str("DiskRef {\n")?;
                write_debug_indentation(formatter, field_depth)?;
                formatter.write_str("ptr: ")?;
                write_nested_pretty_debug(formatter, ptr, field_depth)?;
                formatter.write_str(",\n")?;
                write_debug_indentation(formatter, current_depth)?;
                formatter.write_str("}")?;
            }
            ChildNode::ArtNode {
                node,
                is_final,
                value,
                children,
            } => {
                let field_depth = checked_debug_depth(current_depth, 1)?;
                formatter.write_str("ArtNode {\n")?;

                write_debug_indentation(formatter, field_depth)?;
                formatter.write_str("node: ")?;
                write_nested_pretty_debug(formatter, node, field_depth)?;
                formatter.write_str(",\n")?;

                write_debug_indentation(formatter, field_depth)?;
                formatter.write_str("is_final: ")?;
                write_nested_pretty_debug(formatter, is_final, field_depth)?;
                formatter.write_str(",\n")?;

                write_debug_indentation(formatter, field_depth)?;
                formatter.write_str("value: ")?;
                write_nested_pretty_debug(formatter, value, field_depth)?;
                formatter.write_str(",\n")?;

                write_debug_indentation(formatter, field_depth)?;
                if children.is_empty() {
                    formatter.write_str("children: [],\n")?;
                    write_debug_indentation(formatter, current_depth)?;
                    formatter.write_str("}")?;
                } else {
                    formatter.write_str("children: [\n")?;
                    let tuple_depth = checked_debug_depth(current_depth, 2)?;
                    let child_depth = checked_debug_depth(current_depth, 3)?;
                    write_debug_indentation(formatter, tuple_depth)?;
                    formatter.write_str("(\n")?;
                    write_debug_indentation(formatter, child_depth)?;
                    write_nested_pretty_debug(formatter, &children[0].0, child_depth)?;
                    formatter.write_str(",\n")?;
                    write_debug_indentation(formatter, child_depth)?;
                    frames.push(ChildNodeDebugFrame {
                        children,
                        next_child: 1,
                        depth: current_depth,
                    });
                    current = &children[0].1;
                    current_depth = child_depth;
                    continue;
                }
            }
        }

        loop {
            let Some(frame) = frames.last_mut() else {
                return Ok(());
            };
            let tuple_depth = checked_debug_depth(frame.depth, 2)?;
            formatter.write_str(",\n")?;
            write_debug_indentation(formatter, tuple_depth)?;
            formatter.write_str("),\n")?;

            if frame.next_child < frame.children.len() {
                let child_depth = checked_debug_depth(frame.depth, 3)?;
                let next_child = frame.next_child;
                frame.next_child += 1;
                write_debug_indentation(formatter, tuple_depth)?;
                formatter.write_str("(\n")?;
                write_debug_indentation(formatter, child_depth)?;
                write_nested_pretty_debug(formatter, &frame.children[next_child].0, child_depth)?;
                formatter.write_str(",\n")?;
                write_debug_indentation(formatter, child_depth)?;
                current = &frame.children[next_child].1;
                current_depth = child_depth;
                break;
            }

            let parent_depth = frame.depth;
            let field_depth = checked_debug_depth(parent_depth, 1)?;
            write_debug_indentation(formatter, field_depth)?;
            formatter.write_str("],\n")?;
            write_debug_indentation(formatter, parent_depth)?;
            formatter.write_str("}")?;
            frames.pop();
        }
    }
}

impl Clone for ChildNode {
    fn clone(&self) -> Self {
        let mut frames: SmallVec<[ChildNodeCloneFrame<'_>; 16]> = SmallVec::new();
        let mut current = self;

        loop {
            let mut completed = match current {
                ChildNode::Bucket(bucket) => ChildNode::Bucket(bucket.clone()),
                ChildNode::DiskRef { ptr } => ChildNode::DiskRef { ptr: ptr.clone() },
                ChildNode::ArtNode {
                    node,
                    is_final,
                    value,
                    children,
                } if children.is_empty() => ChildNode::ArtNode {
                    node: node.clone(),
                    is_final: *is_final,
                    value: value.clone(),
                    children: Vec::new(),
                },
                ChildNode::ArtNode {
                    node,
                    is_final,
                    value,
                    children,
                } => {
                    // Clone fields in declaration order, matching the snapshots taken by
                    // derived Clone for the atomics contained in `Node`/`SwizzledPtr`.
                    frames.push(ChildNodeCloneFrame {
                        node: node.clone(),
                        is_final: *is_final,
                        value: value.clone(),
                        source_children: children,
                        next_child: 1,
                        cloned_children: Vec::with_capacity(children.len()),
                    });
                    current = &children[0].1;
                    continue;
                }
            };

            loop {
                let Some(frame) = frames.last_mut() else {
                    return completed;
                };

                let completed_index = frame.next_child - 1;
                let label = frame.source_children[completed_index].0;
                frame.cloned_children.push((label, completed));

                if frame.next_child < frame.source_children.len() {
                    let next_child = frame.next_child;
                    frame.next_child += 1;
                    current = &frame.source_children[next_child].1;
                    break;
                }

                let frame = frames
                    .pop()
                    .expect("a frame observed through last_mut must remain present");
                completed = ChildNode::ArtNode {
                    node: frame.node,
                    is_final: frame.is_final,
                    value: frame.value,
                    children: frame.cloned_children,
                };
            }
        }
    }
}

impl fmt::Debug for ChildNode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if formatter.alternate() {
            fmt_child_node_pretty(self, formatter)
        } else {
            fmt_child_node_compact(self, formatter)
        }
    }
}

impl Drop for ChildNode {
    fn drop(&mut self) {
        #[cfg(test)]
        let _probe_guard = ChildNodeDropProbeGuard::enter();

        let ChildNode::ArtNode { children, .. } = self else {
            return;
        };
        if children.is_empty() {
            return;
        }

        // Each iterator owns the original child-vector allocation. A chain therefore
        // needs only `current` and allocates no auxiliary storage. We suspend an iterator
        // only when an ancestor still has an unvisited sibling.
        let mut current = std::mem::take(children).into_iter();
        let mut suspended: SmallVec<[std::vec::IntoIter<(u8, ChildNode)>; 16]> = SmallVec::new();

        loop {
            while let Some((_, mut child)) = current.next() {
                let ChildNode::ArtNode { children, .. } = &mut child else {
                    continue;
                };
                if children.is_empty() {
                    continue;
                }

                let descendants = std::mem::take(children).into_iter();
                if current.len() != 0 {
                    suspended.push(current);
                }
                current = descendants;
                // `child` now has an empty child vector, so its re-entrant Drop takes
                // the constant-depth fast path before the next loop iteration.
            }

            let Some(parent) = suspended.pop() else {
                return;
            };
            current = parent;
        }
    }
}

impl ChildNode {
    /// Create a new bucket child
    pub fn bucket(b: StringBucket) -> Self {
        ChildNode::Bucket(b)
    }

    /// Create a new ART node child
    pub fn art_node(node: Node, is_final: bool, value: Option<Vec<u8>>) -> Self {
        ChildNode::ArtNode {
            node,
            is_final,
            value,
            children: Vec::new(),
        }
    }

    /// Create a new ART node child with children
    pub fn art_node_with_children(
        node: Node,
        is_final: bool,
        value: Option<Vec<u8>>,
        children: Vec<(u8, ChildNode)>,
    ) -> Self {
        ChildNode::ArtNode {
            node,
            is_final,
            value,
            children,
        }
    }

    /// Create a new disk reference child
    pub fn disk_ref(ptr: SwizzledPtr) -> Self {
        ChildNode::DiskRef { ptr }
    }

    /// Check if this is a bucket
    pub fn is_bucket(&self) -> bool {
        matches!(self, ChildNode::Bucket(_))
    }

    /// Check if this is a disk reference (not yet loaded)
    pub fn is_disk_ref(&self) -> bool {
        matches!(self, ChildNode::DiskRef { .. })
    }

    /// Get the SwizzledPtr if this is a disk reference
    pub fn as_disk_ref(&self) -> Option<&SwizzledPtr> {
        match self {
            ChildNode::DiskRef { ptr } => Some(ptr),
            _ => None,
        }
    }

    /// Check if this child node or any of its descendants need persistence
    ///
    /// Returns true if:
    /// - This is a Bucket (buckets are always serialized in full)
    /// - This is an ArtNode with IS_DIRTY or HAS_DIRTY_DESCENDANTS flag set
    /// - This is a DiskRef (already on disk, returns false)
    ///
    /// This is used by `persist_to_disk()` to skip clean subtrees entirely.
    #[inline]
    pub fn needs_persistence(&self) -> bool {
        match self {
            ChildNode::Bucket(_) => {
                // Buckets don't have per-node dirty flags; they're always
                // serialized if encountered during persistence traversal.
                // The parent ART node's dirty flags determine whether we
                // traverse into this bucket.
                true
            }
            ChildNode::ArtNode { node, .. } => node.header().needs_persistence(),
            ChildNode::DiskRef { .. } => {
                // Already on disk and clean - no persistence needed
                false
            }
        }
    }

    /// Mark this child node as having dirty descendants
    ///
    /// For ArtNode, sets the HAS_DIRTY_DESCENDANTS flag on the node header.
    /// For Bucket and DiskRef, this is a no-op (buckets don't track dirty
    /// descendants, and DiskRef should be resolved before mutation).
    #[inline]
    pub fn mark_has_dirty_descendants(&mut self) {
        if let ChildNode::ArtNode { node, .. } = self {
            node.header_mut().set_has_dirty_descendants(true);
        }
    }

    /// Clear dirty flags on this child node
    ///
    /// For ArtNode, clears both IS_DIRTY and HAS_DIRTY_DESCENDANTS flags.
    /// For Bucket and DiskRef, this is a no-op.
    #[inline]
    pub fn clear_dirty_flags(&mut self) {
        if let ChildNode::ArtNode { node, .. } = self {
            node.header_mut().clear_dirty_flags();
        }
    }

    /// Mark this child node itself as dirty
    ///
    /// For ArtNode, sets the IS_DIRTY flag on the node header.
    /// For Bucket and DiskRef, this is a no-op.
    #[inline]
    pub fn mark_dirty(&mut self) {
        if let ChildNode::ArtNode { node, .. } = self {
            node.header_mut().set_dirty(true);
        }
    }

    /// Get as bucket reference
    pub fn as_bucket(&self) -> Option<&StringBucket> {
        match self {
            ChildNode::Bucket(b) => Some(b),
            _ => None,
        }
    }

    /// Get as mutable bucket reference
    pub fn as_bucket_mut(&mut self) -> Option<&mut StringBucket> {
        match self {
            ChildNode::Bucket(b) => Some(b),
            _ => None,
        }
    }

    /// Get as ART node reference
    pub fn as_art_node(&self) -> Option<ArtNodeView<'_>> {
        match self {
            ChildNode::ArtNode {
                node,
                is_final,
                value,
                children,
            } => Some((node, *is_final, value, children)),
            _ => None,
        }
    }

    /// Get as mutable ART node reference
    pub fn as_art_node_mut(&mut self) -> Option<ArtNodeViewMut<'_>> {
        match self {
            ChildNode::ArtNode {
                node,
                is_final,
                value,
                children,
            } => Some((node, is_final, value, children)),
            _ => None,
        }
    }

    // L3.3c: removed — the owned recursive write methods `ChildNode::insert_key`,
    // `insert_with_value`, `remove_key`, `contains_key` mutated/queried the deleted owned
    // trie's in-memory `ChildNode` subtree (bucket→ART promotion on overflow, recursive
    // descent). The lock-free overlay is the sole representation; the `ChildNode` decode +
    // dirty-flag helpers above are KEPT for the disk-decode / serialize paths.
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use proptest::prelude::*;

    use super::*;

    const DEEP_LIFECYCLE_DEPTH: usize = 100_000;

    fn node4() -> Node {
        Node::N4(Box::default())
    }

    fn child_node_chain(depth: usize) -> ChildNode {
        let mut child = ChildNode::disk_ref(SwizzledPtr::null());
        for index in (0..depth).rev() {
            child = ChildNode::art_node_with_children(
                node4(),
                index % 2 == 0,
                (index % 3 == 0).then(|| vec![(index & 0xff) as u8]),
                vec![((index & 0xff) as u8, child)],
            );
        }
        child
    }

    fn assert_child_node_chain(root: &ChildNode, depth: usize) {
        let mut current = root;
        for index in 0..depth {
            let ChildNode::ArtNode {
                is_final,
                value,
                children,
                ..
            } = current
            else {
                panic!("expected ART node at depth {index}");
            };
            assert_eq!(*is_final, index % 2 == 0);
            assert_eq!(
                value.as_deref(),
                (index % 3 == 0).then_some(&[(index & 0xff) as u8][..])
            );
            assert_eq!(children.len(), 1);
            assert_eq!(children[0].0, (index & 0xff) as u8);
            current = &children[0].1;
        }
        assert!(matches!(current, ChildNode::DiskRef { ptr } if ptr.is_null()));
    }

    fn recursive_reference_debug(child: &ChildNode) -> String {
        enum DebugTask<'a> {
            Node(&'a ChildNode),
            EdgePrefix { label: u8, separator: bool },
            Text(&'static str),
        }

        let mut output = String::new();
        let mut tasks = vec![DebugTask::Node(child)];

        while let Some(task) = tasks.pop() {
            match task {
                DebugTask::Node(ChildNode::Bucket(bucket)) => {
                    write!(output, "Bucket({bucket:?})").expect("writing to String cannot fail");
                }
                DebugTask::Node(ChildNode::DiskRef { ptr }) => {
                    write!(output, "DiskRef {{ ptr: {ptr:?} }}")
                        .expect("writing to String cannot fail");
                }
                DebugTask::Node(ChildNode::ArtNode {
                    node,
                    is_final,
                    value,
                    children,
                }) => {
                    write!(
                        output,
                        "ArtNode {{ node: {node:?}, is_final: {is_final:?}, value: {value:?}, children: ["
                    )
                    .expect("writing to String cannot fail");
                    tasks.push(DebugTask::Text("] }"));
                    for (index, (label, child)) in children.iter().enumerate().rev() {
                        tasks.push(DebugTask::Text(")"));
                        tasks.push(DebugTask::Node(child));
                        tasks.push(DebugTask::EdgePrefix {
                            label: *label,
                            separator: index != 0,
                        });
                    }
                }
                DebugTask::EdgePrefix { label, separator } => {
                    if separator {
                        output.push_str(", ");
                    }
                    write!(output, "({label:?}, ").expect("writing to String cannot fail");
                }
                DebugTask::Text(text) => output.push_str(text),
            }
        }

        output
    }

    #[derive(Debug)]
    enum DerivedDebugChildNode<'a> {
        Bucket(&'a StringBucket),
        ArtNode {
            node: &'a Node,
            is_final: bool,
            value: &'a Option<Vec<u8>>,
            children: Vec<(u8, DerivedDebugChildNode<'a>)>,
        },
        DiskRef {
            ptr: &'a SwizzledPtr,
        },
    }

    /// Build the deliberately shallow derived-`Debug` oracle with an explicit postorder machine.
    ///
    /// The generated `Debug` and `Drop` implementations remain bounded by `child_node_specs`'s
    /// depth-six construction limit; this builder itself consumes no native stack per tree level.
    fn derived_debug_reference(child: &ChildNode) -> DerivedDebugChildNode<'_> {
        enum BuildTask<'a> {
            Visit(&'a ChildNode),
            AssembleArt {
                node: &'a Node,
                is_final: bool,
                value: &'a Option<Vec<u8>>,
                labels: Vec<u8>,
            },
        }

        let mut tasks = vec![BuildTask::Visit(child)];
        let mut completed = Vec::new();

        while let Some(task) = tasks.pop() {
            match task {
                BuildTask::Visit(ChildNode::Bucket(bucket)) => {
                    completed.push(DerivedDebugChildNode::Bucket(bucket));
                }
                BuildTask::Visit(ChildNode::DiskRef { ptr }) => {
                    completed.push(DerivedDebugChildNode::DiskRef { ptr });
                }
                BuildTask::Visit(ChildNode::ArtNode {
                    node,
                    is_final,
                    value,
                    children,
                }) => {
                    let labels = children.iter().map(|(label, _)| *label).collect();
                    tasks.push(BuildTask::AssembleArt {
                        node,
                        is_final: *is_final,
                        value,
                        labels,
                    });
                    for (_, child) in children.iter().rev() {
                        tasks.push(BuildTask::Visit(child));
                    }
                }
                BuildTask::AssembleArt {
                    node,
                    is_final,
                    value,
                    labels,
                } => {
                    let children = completed.split_off(completed.len() - labels.len());
                    completed.push(DerivedDebugChildNode::ArtNode {
                        node,
                        is_final,
                        value,
                        children: labels.into_iter().zip(children).collect(),
                    });
                }
            }
        }

        debug_assert_eq!(completed.len(), 1);
        completed.pop().expect("one derived oracle root")
    }

    fn observe_derived_debug_fields(child: &DerivedDebugChildNode<'_>) {
        match child {
            DerivedDebugChildNode::Bucket(bucket) => {
                std::hint::black_box(bucket);
            }
            DerivedDebugChildNode::ArtNode {
                node,
                is_final,
                value,
                children,
            } => {
                std::hint::black_box(node);
                std::hint::black_box(is_final);
                std::hint::black_box(value);
                std::hint::black_box(children);
            }
            DerivedDebugChildNode::DiskRef { ptr } => {
                std::hint::black_box(ptr);
            }
        }
    }

    #[derive(Default)]
    struct DiscardingWriter {
        bytes: usize,
        calls: usize,
    }

    impl fmt::Write for DiscardingWriter {
        fn write_str(&mut self, value: &str) -> fmt::Result {
            self.bytes += value.len();
            self.calls += 1;
            Ok(())
        }
    }

    struct FailingWriter {
        call: usize,
        fail_at: usize,
    }

    #[derive(Clone, Debug)]
    enum ChildNodeSpec {
        Bucket {
            values: bool,
        },
        DiskRef,
        ArtNode {
            node_kind: u8,
            is_final: bool,
            value: Option<Vec<u8>>,
            children: Vec<(u8, ChildNodeSpec)>,
        },
    }

    fn child_node_specs() -> impl Strategy<Value = ChildNodeSpec> {
        let leaves = prop_oneof![
            any::<bool>().prop_map(|values| ChildNodeSpec::Bucket { values }),
            Just(ChildNodeSpec::DiskRef),
        ];

        leaves.prop_recursive(6, 48, 4, |inner| {
            (
                0_u8..4,
                any::<bool>(),
                prop::option::of(prop::collection::vec(any::<u8>(), 0..8)),
                prop::collection::vec((any::<u8>(), inner), 0..5),
            )
                .prop_map(|(node_kind, is_final, value, children)| {
                    ChildNodeSpec::ArtNode {
                        node_kind,
                        is_final,
                        value,
                        children,
                    }
                })
        })
    }

    fn materialize_spec(spec: ChildNodeSpec) -> ChildNode {
        enum MaterializeTask {
            Visit(ChildNodeSpec),
            AssembleArt {
                node_kind: u8,
                is_final: bool,
                value: Option<Vec<u8>>,
                labels: Vec<u8>,
            },
        }

        let mut tasks = vec![MaterializeTask::Visit(spec)];
        let mut completed = Vec::new();

        while let Some(task) = tasks.pop() {
            match task {
                MaterializeTask::Visit(ChildNodeSpec::Bucket { values: false }) => {
                    completed.push(ChildNode::bucket(StringBucket::new()));
                }
                MaterializeTask::Visit(ChildNodeSpec::Bucket { values: true }) => {
                    completed.push(ChildNode::bucket(StringBucket::with_values()));
                }
                MaterializeTask::Visit(ChildNodeSpec::DiskRef) => {
                    completed.push(ChildNode::disk_ref(SwizzledPtr::null()));
                }
                MaterializeTask::Visit(ChildNodeSpec::ArtNode {
                    node_kind,
                    is_final,
                    value,
                    children,
                }) => {
                    let labels = children.iter().map(|(label, _)| *label).collect();
                    tasks.push(MaterializeTask::AssembleArt {
                        node_kind,
                        is_final,
                        value,
                        labels,
                    });
                    for (_, child) in children.into_iter().rev() {
                        tasks.push(MaterializeTask::Visit(child));
                    }
                }
                MaterializeTask::AssembleArt {
                    node_kind,
                    is_final,
                    value,
                    labels,
                } => {
                    let node = match node_kind {
                        0 => Node::N4(Box::default()),
                        1 => Node::N16(Box::default()),
                        2 => Node::N48(Box::default()),
                        _ => Node::N256(Box::default()),
                    };
                    let children = completed.split_off(completed.len() - labels.len());
                    completed.push(ChildNode::art_node_with_children(
                        node,
                        is_final,
                        value,
                        labels.into_iter().zip(children).collect(),
                    ));
                }
            }
        }

        debug_assert_eq!(completed.len(), 1);
        completed
            .pop()
            .expect("one materialized specification root")
    }

    fn assert_child_nodes_equal(left: &ChildNode, right: &ChildNode) {
        let mut pending: SmallVec<[(&ChildNode, &ChildNode); 16]> = SmallVec::new();
        pending.push((left, right));

        while let Some((left, right)) = pending.pop() {
            match (left, right) {
                (ChildNode::Bucket(left), ChildNode::Bucket(right)) => {
                    assert_eq!(left.as_bytes(), right.as_bytes());
                }
                (ChildNode::DiskRef { ptr: left }, ChildNode::DiskRef { ptr: right }) => {
                    assert_eq!(left.to_raw(), right.to_raw());
                    assert_eq!(left.is_swizzled(), right.is_swizzled());
                }
                (
                    ChildNode::ArtNode {
                        node: left_node,
                        is_final: left_final,
                        value: left_value,
                        children: left_children,
                    },
                    ChildNode::ArtNode {
                        node: right_node,
                        is_final: right_final,
                        value: right_value,
                        children: right_children,
                    },
                ) => {
                    assert_eq!(format!("{left_node:?}"), format!("{right_node:?}"));
                    assert_eq!(left_final, right_final);
                    assert_eq!(left_value, right_value);
                    assert_eq!(left_children.len(), right_children.len());
                    for ((left_label, left), (right_label, right)) in
                        left_children.iter().zip(right_children).rev()
                    {
                        assert_eq!(left_label, right_label);
                        pending.push((left, right));
                    }
                }
                _ => panic!("clone changed a ChildNode variant"),
            }
        }
    }

    impl fmt::Write for FailingWriter {
        fn write_str(&mut self, _value: &str) -> fmt::Result {
            if self.call == self.fail_at {
                return Err(fmt::Error);
            }
            self.call += 1;
            Ok(())
        }
    }

    // L3.3c: removed — the bucket↔ART transition tests (test_should_convert_*,
    // test_bucket_to_art_*, test_art_to_bucket_*, test_roundtrip_bucket_art_bucket,
    // test_should_merge_art_to_bucket) exercised the deleted owned transition functions.

    #[test]
    fn test_child_node_enum() {
        let bucket = StringBucket::new();
        let child = ChildNode::bucket(bucket);
        assert!(child.is_bucket());
        assert!(child.as_bucket().is_some());

        let node = Node::N4(Box::default());
        let child = ChildNode::art_node(node, false, None);
        assert!(!child.is_bucket());
        assert!(child.as_bucket().is_none());
    }

    // L3.3c: removed — the ChildNode owned-write tests (test_child_node_insert_key_*,
    // test_child_node_remove_key_*, test_child_node_nested_art_operations,
    // test_child_node_disk_ref_operations) exercised the deleted owned recursive
    // insert_key / insert_with_value / remove_key / contains_key methods.

    #[test]
    fn test_child_node_needs_persistence_bucket() {
        let bucket = StringBucket::new();
        let child = ChildNode::bucket(bucket);

        // Buckets always report needs_persistence as true (no per-entry dirty tracking)
        assert!(child.needs_persistence());
    }

    #[test]
    fn test_child_node_needs_persistence_art_node() {
        let node = Node::N4(Box::default());
        let mut child = ChildNode::art_node_with_children(node, false, None, Vec::new());

        // Fresh ART node has no dirty flags
        assert!(!child.needs_persistence());

        // Mark as dirty
        child.mark_dirty();
        assert!(child.needs_persistence());

        // Clear dirty flags
        child.clear_dirty_flags();
        assert!(!child.needs_persistence());

        // Mark as having dirty descendants
        child.mark_has_dirty_descendants();
        assert!(child.needs_persistence());

        // Clear dirty flags
        child.clear_dirty_flags();
        assert!(!child.needs_persistence());
    }

    #[test]
    fn test_child_node_needs_persistence_disk_ref() {
        let ptr = SwizzledPtr::null();
        let child = ChildNode::disk_ref(ptr);

        // DiskRef is already on disk, doesn't need persistence
        assert!(!child.needs_persistence());
    }

    #[test]
    fn test_child_node_dirty_flag_methods() {
        let node = Node::N4(Box::default());
        let mut child = ChildNode::art_node_with_children(node, false, None, Vec::new());

        // Test mark_dirty
        child.mark_dirty();
        if let ChildNode::ArtNode { node, .. } = &child {
            assert!(node.header().is_dirty());
        }

        // Test mark_has_dirty_descendants
        child.clear_dirty_flags();
        child.mark_has_dirty_descendants();
        if let ChildNode::ArtNode { node, .. } = &child {
            assert!(node.header().has_dirty_descendants());
            assert!(!node.header().is_dirty());
        }

        // Test clear_dirty_flags clears both
        child.mark_dirty();
        child.clear_dirty_flags();
        if let ChildNode::ArtNode { node, .. } = &child {
            assert!(!node.header().is_dirty());
            assert!(!node.header().has_dirty_descendants());
        }
    }

    #[test]
    fn test_child_node_dirty_methods_on_bucket() {
        let bucket = StringBucket::new();
        let mut child = ChildNode::bucket(bucket);

        // These should be no-ops for buckets (no panic)
        child.mark_dirty();
        child.mark_has_dirty_descendants();
        child.clear_dirty_flags();

        // Bucket should still be a bucket
        assert!(child.is_bucket());
    }

    #[test]
    fn test_child_node_dirty_methods_on_disk_ref() {
        let ptr = SwizzledPtr::null();
        let mut child = ChildNode::disk_ref(ptr);

        // These should be no-ops for disk refs (no panic)
        child.mark_dirty();
        child.mark_has_dirty_descendants();
        child.clear_dirty_flags();

        // DiskRef should still be a DiskRef
        assert!(child.is_disk_ref());
    }

    #[test]
    fn child_node_compact_debug_matches_the_derived_shape() {
        let child = ChildNode::art_node_with_children(
            Node::N16(Box::default()),
            true,
            Some(vec![0, 1, 255]),
            vec![
                (200, ChildNode::bucket(StringBucket::with_values())),
                (
                    7,
                    ChildNode::art_node_with_children(
                        Node::N48(Box::default()),
                        false,
                        Some(Vec::new()),
                        vec![
                            (9, ChildNode::disk_ref(SwizzledPtr::null())),
                            (
                                9,
                                ChildNode::art_node(Node::N256(Box::default()), true, None),
                            ),
                        ],
                    ),
                ),
            ],
        );

        let derived = derived_debug_reference(&child);
        observe_derived_debug_fields(&derived);
        assert_eq!(format!("{child:?}"), format!("{derived:?}"));
        assert_eq!(format!("{child:?}"), recursive_reference_debug(&child));
        assert_eq!(format!("{child:#?}"), format!("{derived:#?}"));
    }

    #[test]
    fn child_node_debug_propagates_every_shallow_writer_failure() {
        let child = ChildNode::art_node_with_children(
            node4(),
            true,
            Some(vec![1, 2, 3]),
            vec![
                (3, ChildNode::disk_ref(SwizzledPtr::null())),
                (1, ChildNode::bucket(StringBucket::new())),
            ],
        );
        let mut probe = DiscardingWriter::default();
        write!(&mut probe, "{child:?}").expect("probe writer cannot fail");
        assert!(probe.calls > 0);

        for fail_at in 0..probe.calls {
            let mut writer = FailingWriter { call: 0, fail_at };
            assert!(write!(&mut writer, "{child:?}").is_err());
        }

        let mut pretty_probe = DiscardingWriter::default();
        write!(&mut pretty_probe, "{child:#?}").expect("probe writer cannot fail");
        assert!(pretty_probe.calls > probe.calls);
        for fail_at in 0..pretty_probe.calls {
            let mut writer = FailingWriter { call: 0, fail_at };
            assert!(write!(&mut writer, "{child:#?}").is_err());
        }
    }

    #[test]
    fn child_node_clone_is_independent() {
        let original = ChildNode::art_node_with_children(
            node4(),
            true,
            Some(vec![1, 2]),
            vec![(4, ChildNode::bucket(StringBucket::new()))],
        );
        let mut cloned = original.clone();

        let ChildNode::ArtNode {
            is_final,
            value,
            children,
            ..
        } = &mut cloned
        else {
            unreachable!("constructed an ART node");
        };
        *is_final = false;
        value.as_mut().expect("value is present").push(3);
        children.push((5, ChildNode::disk_ref(SwizzledPtr::null())));

        let ChildNode::ArtNode {
            is_final,
            value,
            children,
            ..
        } = &original
        else {
            unreachable!("constructed an ART node");
        };
        assert!(*is_final);
        assert_eq!(value.as_deref(), Some([1, 2].as_slice()));
        assert_eq!(children.len(), 1);
    }

    #[test]
    fn child_node_clone_debug_and_drop_are_stack_safe_at_one_hundred_thousand_depth() {
        let original = child_node_chain(DEEP_LIFECYCLE_DEPTH);
        let cloned = original.clone();
        assert_child_node_chain(&original, DEEP_LIFECYCLE_DEPTH);
        assert_child_node_chain(&cloned, DEEP_LIFECYCLE_DEPTH);

        let mut writer = DiscardingWriter::default();
        write!(&mut writer, "{original:?}").expect("discarding writer cannot fail");
        assert!(writer.bytes > DEEP_LIFECYCLE_DEPTH);

        // Ordinary scope destruction exercises the manual drop machine for both trees.
        drop(cloned);
        drop(original);
    }

    #[test]
    fn child_node_drop_is_stack_safe_with_one_hundred_thousand_branching_ancestors() {
        let mut child = ChildNode::disk_ref(SwizzledPtr::null());
        for _ in 0..DEEP_LIFECYCLE_DEPTH {
            child = ChildNode::art_node_with_children(
                node4(),
                false,
                None,
                vec![(0, child), (1, ChildNode::disk_ref(SwizzledPtr::null()))],
            );
        }

        // Every ancestor has a pending sibling, forcing the continuation stack to spill.
        drop(child);
    }

    proptest! {
        #[test]
        fn child_node_iterative_clone_and_debug_match_bounded_reference(spec in child_node_specs()) {
            let original = materialize_spec(spec);
            let cloned = original.clone();

            assert_child_nodes_equal(&original, &cloned);
            let original_derived = derived_debug_reference(&original);
            let cloned_derived = derived_debug_reference(&cloned);
            prop_assert_eq!(format!("{original:?}"), format!("{original_derived:?}"));
            prop_assert_eq!(format!("{cloned:?}"), format!("{cloned_derived:?}"));
            prop_assert_eq!(format!("{original:#?}"), format!("{original_derived:#?}"));
            prop_assert_eq!(format!("{cloned:#?}"), format!("{cloned_derived:#?}"));
        }
    }

    #[test]
    fn child_node_drop_reclaims_each_node_once_and_bounds_native_depth() {
        const DEPTH: usize = 4_096;
        let mut child = ChildNode::disk_ref(SwizzledPtr::null());
        for _ in 0..DEPTH {
            child = ChildNode::art_node_with_children(
                node4(),
                false,
                None,
                vec![(0, child), (1, ChildNode::disk_ref(SwizzledPtr::null()))],
            );
        }

        start_child_node_drop_probe();
        drop(child);
        let observed = finish_child_node_drop_probe();
        let expected_nodes = DEPTH
            .checked_mul(2)
            .and_then(|nodes| nodes.checked_add(1))
            .expect("bounded test node count must fit usize");
        assert_eq!(observed.invocations, expected_nodes);
        assert!(
            observed.maximum_depth <= 2,
            "native ChildNode::drop nesting reached {}",
            observed.maximum_depth
        );
    }
}
