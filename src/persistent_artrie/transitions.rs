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

use super::bucket::StringBucket;
use super::nodes::Node;
#[cfg(test)]
use super::nodes::Node4;
use super::swizzled_ptr::SwizzledPtr;

// L3.3c: removed — the owned bucket↔ART transition surface
// (BUCKET_TO_ART_THRESHOLD / ART_TO_BUCKET_THRESHOLD, BucketToArtResult /
// ArtToBucketResult / TransitionError, should_convert_bucket_to_art /
// bucket_to_art_node / should_merge_art_to_bucket / art_node_to_bucket). These built
// the deleted owned trie's bucket→ART promotions / ART→bucket merges. The lock-free
// overlay is un-path-compressed (no buckets, no promotions). The `ChildNode` enum +
// its decode/overlay helper methods are KEPT below (the disk-decode path + the
// `serialize_*`/`resolve_disk_ref` surface still use them).

/// Represents a child pointer that can be either a bucket or an ART node
#[derive(Debug, Clone)]
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
    pub fn as_art_node(&self) -> Option<(&Node, bool, &Option<Vec<u8>>, &Vec<(u8, ChildNode)>)> {
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
    pub fn as_art_node_mut(
        &mut self,
    ) -> Option<(
        &mut Node,
        &mut bool,
        &mut Option<Vec<u8>>,
        &mut Vec<(u8, ChildNode)>,
    )> {
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
    use super::*;

    // L3.3c: removed — the bucket↔ART transition tests (test_should_convert_*,
    // test_bucket_to_art_*, test_art_to_bucket_*, test_roundtrip_bucket_art_bucket,
    // test_should_merge_art_to_bucket) exercised the deleted owned transition functions.

    #[test]
    fn test_child_node_enum() {
        let bucket = StringBucket::new();
        let child = ChildNode::bucket(bucket);
        assert!(child.is_bucket());
        assert!(child.as_bucket().is_some());

        let node = Node::N4(Box::new(Node4::new()));
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
        let node = Node::N4(Box::new(Node4::new()));
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
        let node = Node::N4(Box::new(Node4::new()));
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
}
