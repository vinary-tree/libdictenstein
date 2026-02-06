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

use super::bucket::{BucketError, StringBucket};
use super::nodes::{ArtNode, Node, Node4};
use super::swizzled_ptr::SwizzledPtr;

/// Threshold for converting a bucket to an ART node
/// (when bucket has this many unique first-bytes)
pub const BUCKET_TO_ART_THRESHOLD: usize = 4;

/// Threshold for merging ART children back to a bucket
/// (when total entries across all children is below this)
pub const ART_TO_BUCKET_THRESHOLD: usize = 32;

/// Result of a bucket-to-ART transition
#[derive(Debug)]
pub struct BucketToArtResult {
    /// The new ART node
    pub node: Node,
    /// Child nodes keyed by their edge byte (can be buckets or nested ART nodes)
    pub children: Vec<(u8, ChildNode)>,
    /// Whether this node is final (had an empty suffix in the bucket)
    pub is_final: bool,
    /// Value associated with the final state (if any)
    pub final_value: Option<Vec<u8>>,
}

/// Result of an ART-to-bucket transition
#[derive(Debug)]
pub struct ArtToBucketResult {
    /// The merged bucket
    pub bucket: StringBucket,
}

/// Error during transition operations
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransitionError {
    /// Bucket doesn't meet criteria for conversion
    BucketNotReady(String),
    /// ART node doesn't meet criteria for merging
    ArtNotReady(String),
    /// Bucket operation failed
    BucketError(BucketError),
    /// Resulting bucket would be too large
    MergedBucketTooLarge,
}

impl std::fmt::Display for TransitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransitionError::BucketNotReady(msg) => {
                write!(f, "bucket not ready for conversion: {}", msg)
            }
            TransitionError::ArtNotReady(msg) => {
                write!(f, "ART node not ready for merging: {}", msg)
            }
            TransitionError::BucketError(e) => write!(f, "bucket error: {}", e),
            TransitionError::MergedBucketTooLarge => {
                write!(f, "merged bucket would exceed size limit")
            }
        }
    }
}

impl std::error::Error for TransitionError {}

impl From<BucketError> for TransitionError {
    fn from(e: BucketError) -> Self {
        TransitionError::BucketError(e)
    }
}

/// Check if a bucket should be converted to an ART node
///
/// A bucket should be converted when:
/// 1. It's full or near the split threshold
/// 2. It has entries with multiple distinct first-bytes
pub fn should_convert_bucket_to_art(bucket: &StringBucket) -> bool {
    if !bucket.header().should_split() {
        return false;
    }

    // Count distinct first bytes
    let result = bucket.split_by_first_byte();
    result.buckets.len() >= BUCKET_TO_ART_THRESHOLD
}

/// Convert a bucket to an ART node with child buckets or nested ART nodes.
///
/// This splits the bucket by first byte and creates an ART node (Node4 initially)
/// with edges to child buckets. If a child bucket would overflow (more than
/// MAX_BUCKET_ENTRIES share the same first byte), the overflow entries are
/// recursively inserted into the child, converting it to an ART node if needed.
pub fn bucket_to_art_node(bucket: &StringBucket) -> Result<BucketToArtResult, TransitionError> {
    let split_result = bucket.split_by_first_byte();

    if split_result.buckets.is_empty() && split_result.finals.is_empty() && split_result.overflow.is_empty() {
        return Err(TransitionError::BucketNotReady("bucket is empty".to_string()));
    }

    // Create a new Node4 (will grow as needed when children are added)
    let mut node = Node4::new();
    let has_values = bucket.header().has_values();

    // Determine if this node is final
    let is_final = !split_result.finals.is_empty();
    let final_value = if is_final {
        split_result.finals.first().and_then(|(_, v)| v.clone())
    } else {
        None
    };

    node.header.set_final(is_final);

    // Collect children - start with buckets
    let mut children: Vec<(u8, ChildNode)> = Vec::new();

    // Collect children first as buckets
    for (byte, child_bucket) in split_result.buckets {
        children.push((byte, ChildNode::Bucket(child_bucket)));
    }

    // Handle overflow entries by inserting them into the appropriate child
    // This may cause child buckets to convert to ART nodes recursively
    for (first_byte, remaining, value) in split_result.overflow {
        // Find the child for this first byte
        let child_idx = children.iter().position(|(b, _)| *b == first_byte);

        if let Some(idx) = child_idx {
            // Insert into existing child (may trigger bucket-to-ART conversion)
            let inserted = if let Some(ref v) = value {
                children[idx].1.insert_with_value(&remaining, Some(v.as_slice()))
            } else {
                children[idx].1.insert_key(&remaining)
            };
            // We don't need to check `inserted` since overflow entries are
            // guaranteed to be new (they weren't already in the bucket)
            let _ = inserted;
        } else {
            // This shouldn't happen - overflow entries should have a corresponding bucket
            // But handle it gracefully by creating a new bucket
            let mut new_bucket = if has_values {
                StringBucket::with_values()
            } else {
                StringBucket::new()
            };
            if let Some(ref v) = value {
                let _ = new_bucket.insert(&remaining, v);
            } else {
                let _ = new_bucket.insert_key(&remaining);
            }
            children.push((first_byte, ChildNode::Bucket(new_bucket)));
        }
    }

    // Now build the appropriate node type based on child count
    let result_node = if children.len() <= 4 {
        // Node4 can hold all children
        for (byte, _) in &children {
            let ptr = SwizzledPtr::null();
            let _ = node.add_child(*byte, ptr);
        }
        Node::N4(Box::new(node))
    } else if children.len() <= 16 {
        // Need Node16
        let mut node16 = node.grow();
        for (byte, _) in &children {
            let ptr = SwizzledPtr::null();
            let _ = node16.add_child(*byte, ptr);
        }
        Node::N16(Box::new(node16))
    } else if children.len() <= 48 {
        // Need Node48
        let node16 = node.grow();
        let mut node48 = node16.grow();
        for (byte, _) in &children {
            let ptr = SwizzledPtr::null();
            let _ = node48.add_child(*byte, ptr);
        }
        Node::N48(Box::new(node48))
    } else {
        // Need Node256
        let node16 = node.grow();
        let node48 = node16.grow();
        let mut node256 = node48.grow();
        for (byte, _) in &children {
            let ptr = SwizzledPtr::null();
            let _ = node256.add_child(*byte, ptr);
        }
        Node::N256(Box::new(node256))
    };

    Ok(BucketToArtResult {
        node: result_node,
        children,
        is_final,
        final_value,
    })
}

/// Check if an ART node's children should be merged back to a bucket
///
/// This checks if the total size of all child buckets is small enough
/// to fit in a single bucket.
pub fn should_merge_art_to_bucket(children: &[(u8, &StringBucket)]) -> bool {
    let total_entries: usize = children.iter().map(|(_, b)| b.len()).sum();
    total_entries <= ART_TO_BUCKET_THRESHOLD
}

/// Merge ART node children back into a single bucket
///
/// This collects all entries from child buckets and creates a single
/// bucket with the edge byte prepended to each suffix.
pub fn art_node_to_bucket(
    children: &[(u8, &StringBucket)],
    is_final: bool,
    final_value: Option<&[u8]>,
) -> Result<ArtToBucketResult, TransitionError> {
    let has_values = children.iter().any(|(_, b)| b.header().has_values())
        || final_value.is_some();

    let mut bucket = if has_values {
        StringBucket::with_values()
    } else {
        StringBucket::new()
    };

    // Add final entry if this node was final
    if is_final {
        if let Some(value) = final_value {
            bucket.insert(b"", value)?;
        } else {
            bucket.insert_key(b"")?;
        }
    }

    // Collect entries from all children
    for (edge_byte, child) in children {
        for i in 0..child.len() {
            let entry = child.get_entry(i).expect("valid index");
            let suffix = child.get_suffix(&entry);
            let value = child.get_value(&entry);

            // Prepend edge byte to suffix
            let mut full_suffix = vec![*edge_byte];
            full_suffix.extend_from_slice(suffix);

            if let Some(v) = value {
                bucket.insert(&full_suffix, v)?;
            } else {
                bucket.insert_key(&full_suffix)?;
            }
        }
    }

    Ok(ArtToBucketResult { bucket })
}

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
            ChildNode::ArtNode { node, .. } => {
                node.header().needs_persistence()
            }
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
    ) -> Option<(&mut Node, &mut bool, &mut Option<Vec<u8>>, &mut Vec<(u8, ChildNode)>)> {
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

    /// Recursively insert a key into this child node.
    ///
    /// This handles all child types:
    /// - `Bucket`: directly insert into the bucket, converting to ART node if full
    /// - `ArtNode`: recursively descend through nested ART structure
    /// - `DiskRef`: **NOT SUPPORTED** - returns `false` without loading
    ///
    /// # DiskRef Limitation
    ///
    /// This method cannot resolve `DiskRef` nodes because it lacks access to the
    /// `BufferManager` required for disk I/O. For operations on potentially disk-backed
    /// tries, use `PersistentARTrie::insert()` which properly resolves `DiskRef`
    /// nodes before mutation via `resolve_child_for_mutation()`.
    ///
    /// # Returns
    ///
    /// * `true` if the key was newly inserted
    /// * `false` if it already existed, insertion failed, or the node is a `DiskRef`
    pub fn insert_key(&mut self, remaining: &[u8]) -> bool {
        // Handle bucket case with potential overflow conversion
        if let ChildNode::Bucket(bucket) = self {
            match bucket.insert_key(remaining) {
                Ok(inserted) => return inserted,
                Err(BucketError::BucketFull) => {
                    // Bucket is full, convert to ART node
                    if let Ok(result) = bucket_to_art_node(bucket) {
                        // bucket_to_art_node now returns ChildNode directly
                        *self = ChildNode::ArtNode {
                            node: result.node,
                            is_final: result.is_final,
                            value: result.final_value,
                            children: result.children,
                        };
                        // Retry insert with the new ART node (recursive call)
                        return self.insert_key(remaining);
                    }
                    return false;
                }
                Err(_) => return false,
            }
        }

        match self {
            ChildNode::Bucket(_) => unreachable!("handled above"),
            ChildNode::ArtNode {
                is_final,
                value: _,
                children,
                ..
            } => {
                if remaining.is_empty() {
                    // Insert at this node (make it final)
                    if *is_final {
                        false // Already exists
                    } else {
                        *is_final = true;
                        true
                    }
                } else {
                    let first = remaining[0];
                    let rest = &remaining[1..];

                    // Find child with matching byte
                    for (b, child) in children.iter_mut() {
                        if *b == first {
                            return child.insert_key(rest);
                        }
                    }

                    // No matching child, create new bucket
                    let mut new_bucket = StringBucket::with_values();
                    let _ = new_bucket.insert_key(rest);
                    children.push((first, ChildNode::Bucket(new_bucket)));
                    true
                }
            }
            ChildNode::DiskRef { .. } => {
                // Cannot insert into disk ref without loading first
                false
            }
        }
    }

    /// Recursively insert a key with an optional value into this child node.
    ///
    /// This handles all child types:
    /// - `Bucket`: directly insert into the bucket, converting to ART node if full
    /// - `ArtNode`: recursively descend through nested ART structure
    /// - `DiskRef`: **NOT SUPPORTED** - returns `false` without loading
    ///
    /// # DiskRef Limitation
    ///
    /// This method cannot resolve `DiskRef` nodes because it lacks access to the
    /// `BufferManager` required for disk I/O. For operations on potentially disk-backed
    /// tries, use `PersistentARTrie::insert()` which properly resolves `DiskRef`
    /// nodes before mutation via `resolve_child_for_mutation()`.
    ///
    /// # Returns
    ///
    /// * `true` if the key was newly inserted
    /// * `false` if it already existed, insertion failed, or the node is a `DiskRef`
    pub fn insert_with_value(&mut self, remaining: &[u8], value: Option<&[u8]>) -> bool {
        // Handle bucket case with potential overflow conversion
        if let ChildNode::Bucket(bucket) = self {
            let insert_result = if let Some(val) = value {
                bucket.insert(remaining, val)
            } else {
                bucket.insert_key(remaining)
            };
            match insert_result {
                Ok(inserted) => return inserted,
                Err(BucketError::BucketFull) => {
                    // Bucket is full, convert to ART node
                    if let Ok(result) = bucket_to_art_node(bucket) {
                        // bucket_to_art_node now returns ChildNode directly
                        *self = ChildNode::ArtNode {
                            node: result.node,
                            is_final: result.is_final,
                            value: result.final_value,
                            children: result.children,
                        };
                        // Retry insert with the new ART node (recursive call)
                        return self.insert_with_value(remaining, value);
                    }
                    return false;
                }
                Err(_) => return false,
            }
        }

        match self {
            ChildNode::Bucket(_) => unreachable!("handled above"),
            ChildNode::ArtNode {
                is_final,
                value: node_value,
                children,
                ..
            } => {
                if remaining.is_empty() {
                    // Insert at this node (make it final)
                    if *is_final {
                        false // Already exists
                    } else {
                        *is_final = true;
                        *node_value = value.map(|v| v.to_vec());
                        true
                    }
                } else {
                    let first = remaining[0];
                    let rest = &remaining[1..];

                    // Find child with matching byte
                    for (b, child) in children.iter_mut() {
                        if *b == first {
                            return child.insert_with_value(rest, value);
                        }
                    }

                    // No matching child, create new bucket
                    let mut new_bucket = StringBucket::with_values();
                    if let Some(val) = value {
                        let _ = new_bucket.insert(rest, val);
                    } else {
                        let _ = new_bucket.insert_key(rest);
                    }
                    children.push((first, ChildNode::Bucket(new_bucket)));
                    true
                }
            }
            ChildNode::DiskRef { .. } => {
                // Cannot insert into disk ref without loading first
                false
            }
        }
    }

    /// Recursively remove a key from this child node.
    ///
    /// This handles all child types:
    /// - `Bucket`: directly remove from the bucket
    /// - `ArtNode`: recursively descend through nested ART structure
    /// - `DiskRef`: **NOT SUPPORTED** - returns `false` without loading
    ///
    /// # DiskRef Limitation
    ///
    /// This method cannot resolve `DiskRef` nodes because it lacks access to the
    /// `BufferManager` required for disk I/O. For operations on potentially disk-backed
    /// tries, use `PersistentARTrie::remove()` which properly resolves `DiskRef`
    /// nodes before mutation via `resolve_child_for_mutation()`.
    ///
    /// # Returns
    ///
    /// * `true` if the key was removed
    /// * `false` if it didn't exist, removal failed, or the node is a `DiskRef`
    pub fn remove_key(&mut self, remaining: &[u8]) -> bool {
        match self {
            ChildNode::Bucket(bucket) => {
                bucket.remove(remaining).is_some()
            }
            ChildNode::ArtNode {
                is_final,
                value,
                children,
                ..
            } => {
                if remaining.is_empty() {
                    // Remove at this node
                    if *is_final {
                        *is_final = false;
                        *value = None;
                        true
                    } else {
                        false // Didn't exist
                    }
                } else {
                    let first = remaining[0];
                    let rest = &remaining[1..];

                    // Find child with matching byte
                    for (b, child) in children.iter_mut() {
                        if *b == first {
                            return child.remove_key(rest);
                        }
                    }
                    false // Child not found
                }
            }
            ChildNode::DiskRef { .. } => {
                // Cannot remove from disk ref without loading first
                false
            }
        }
    }

    /// Check if this child node contains a key.
    ///
    /// This handles all child types:
    /// - `Bucket`: directly check in the bucket
    /// - `ArtNode`: recursively descend through nested ART structure
    /// - `DiskRef`: **NOT SUPPORTED** - returns `false` without loading
    ///
    /// # DiskRef Limitation
    ///
    /// This method cannot resolve `DiskRef` nodes because it lacks access to the
    /// `BufferManager` required for disk I/O. For operations on potentially disk-backed
    /// tries, use `PersistentARTrie::contains()` which properly resolves `DiskRef`
    /// nodes via `contains_in_child_with_depth()`.
    ///
    /// # Returns
    ///
    /// * `true` if the key exists in the trie
    /// * `false` if it doesn't exist, or the node is a `DiskRef`
    pub fn contains_key(&self, remaining: &[u8]) -> bool {
        match self {
            ChildNode::Bucket(bucket) => {
                bucket.contains(remaining)
            }
            ChildNode::ArtNode {
                is_final,
                children,
                ..
            } => {
                if remaining.is_empty() {
                    *is_final
                } else {
                    let first = remaining[0];
                    let rest = &remaining[1..];

                    // Find child with matching byte
                    for (b, child) in children.iter() {
                        if *b == first {
                            return child.contains_key(rest);
                        }
                    }
                    false // Child not found
                }
            }
            ChildNode::DiskRef { .. } => {
                // Cannot check disk ref without loading
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_convert_empty_bucket() {
        let bucket = StringBucket::new();
        assert!(!should_convert_bucket_to_art(&bucket));
    }

    #[test]
    fn test_should_convert_small_bucket() {
        let mut bucket = StringBucket::new();
        bucket.insert_key(b"apple").unwrap();
        bucket.insert_key(b"banana").unwrap();
        assert!(!should_convert_bucket_to_art(&bucket));
    }

    #[test]
    fn test_bucket_to_art_basic() {
        let mut bucket = StringBucket::new();

        // Insert entries with different first bytes
        bucket.insert_key(b"apple").unwrap();
        bucket.insert_key(b"banana").unwrap();
        bucket.insert_key(b"cherry").unwrap();

        let result = bucket_to_art_node(&bucket).unwrap();

        // Should have 3 children
        assert_eq!(result.children.len(), 3);

        // Should not be final
        assert!(!result.is_final);

        // Check children - now ChildNode instead of StringBucket
        let a_child = result.children.iter().find(|(b, _)| *b == b'a');
        assert!(a_child.is_some());
        let (_, a_child_node) = a_child.unwrap();
        assert!(a_child_node.contains_key(b"pple"));

        let b_child = result.children.iter().find(|(b, _)| *b == b'b');
        assert!(b_child.is_some());
        let (_, b_child_node) = b_child.unwrap();
        assert!(b_child_node.contains_key(b"anana"));
    }

    #[test]
    fn test_bucket_to_art_with_final() {
        let mut bucket = StringBucket::new();

        bucket.insert_key(b"").unwrap(); // Final marker
        bucket.insert_key(b"apple").unwrap();
        bucket.insert_key(b"banana").unwrap();

        let result = bucket_to_art_node(&bucket).unwrap();

        // Should be final
        assert!(result.is_final);
        assert!(result.final_value.is_none());
    }

    #[test]
    fn test_bucket_to_art_with_value() {
        let mut bucket = StringBucket::with_values();

        bucket.insert(b"", b"root_value").unwrap();
        bucket.insert(b"apple", b"apple_value").unwrap();

        let result = bucket_to_art_node(&bucket).unwrap();

        assert!(result.is_final);
        assert_eq!(result.final_value, Some(b"root_value".to_vec()));
    }

    #[test]
    fn test_art_to_bucket_basic() {
        // Create child buckets
        let mut a_bucket = StringBucket::new();
        a_bucket.insert_key(b"pple").unwrap();
        a_bucket.insert_key(b"pricot").unwrap();

        let mut b_bucket = StringBucket::new();
        b_bucket.insert_key(b"anana").unwrap();

        let children: Vec<(u8, &StringBucket)> = vec![(b'a', &a_bucket), (b'b', &b_bucket)];

        let result = art_node_to_bucket(&children, false, None).unwrap();

        // Should have all entries with edge bytes prepended
        assert_eq!(result.bucket.len(), 3);
        assert!(result.bucket.contains(b"apple"));
        assert!(result.bucket.contains(b"apricot"));
        assert!(result.bucket.contains(b"banana"));
    }

    #[test]
    fn test_art_to_bucket_with_final() {
        let mut a_bucket = StringBucket::new();
        a_bucket.insert_key(b"pple").unwrap();

        let children: Vec<(u8, &StringBucket)> = vec![(b'a', &a_bucket)];

        let result = art_node_to_bucket(&children, true, None).unwrap();

        // Should include the empty suffix for final state
        assert!(result.bucket.contains(b""));
        assert!(result.bucket.contains(b"apple"));
    }

    #[test]
    fn test_roundtrip_bucket_art_bucket() {
        let mut original = StringBucket::new();

        original.insert_key(b"apple").unwrap();
        original.insert_key(b"apricot").unwrap();
        original.insert_key(b"banana").unwrap();
        original.insert_key(b"berry").unwrap();
        original.insert_key(b"cherry").unwrap();

        // Collect original entries
        let original_entries: Vec<_> = original.iter().map(|(_, s)| s.to_vec()).collect();

        // Convert to ART
        let art_result = bucket_to_art_node(&original).unwrap();

        // Convert back to bucket - extract StringBucket from ChildNode
        let children: Vec<(u8, &StringBucket)> = art_result
            .children
            .iter()
            .filter_map(|(b, child)| {
                child.as_bucket().map(|bucket| (*b, bucket))
            })
            .collect();

        let bucket_result =
            art_node_to_bucket(&children, art_result.is_final, art_result.final_value.as_deref())
                .unwrap();

        // Should have same entries
        let restored_entries: Vec<_> = bucket_result.bucket.iter().map(|(_, s)| s.to_vec()).collect();
        assert_eq!(original_entries, restored_entries);
    }

    #[test]
    fn test_should_merge_art_to_bucket() {
        let mut small_bucket = StringBucket::new();
        small_bucket.insert_key(b"test").unwrap();

        let children: Vec<(u8, &StringBucket)> = vec![(b'a', &small_bucket)];
        assert!(should_merge_art_to_bucket(&children));

        // With many entries, should not merge
        let mut large_bucket = StringBucket::new();
        for i in 0..50 {
            let key = format!("{:03}", i);
            large_bucket.insert_key(key.as_bytes()).unwrap();
        }

        let children: Vec<(u8, &StringBucket)> = vec![(b'a', &large_bucket)];
        assert!(!should_merge_art_to_bucket(&children));
    }

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

    #[test]
    fn test_child_node_insert_key_bucket() {
        let bucket = StringBucket::new();
        let mut child = ChildNode::bucket(bucket);

        // Insert into bucket child
        assert!(child.insert_key(b"apple"));
        assert!(child.insert_key(b"banana"));

        // Duplicate returns false
        assert!(!child.insert_key(b"apple"));

        // Verify keys exist
        assert!(child.contains_key(b"apple"));
        assert!(child.contains_key(b"banana"));
        assert!(!child.contains_key(b"cherry"));
    }

    #[test]
    fn test_child_node_insert_key_art() {
        let node = Node::N4(Box::new(Node4::new()));
        let mut child = ChildNode::art_node_with_children(node, false, None, Vec::new());

        // Insert creates new bucket children
        assert!(child.insert_key(b"apple"));
        assert!(child.insert_key(b"apricot"));

        // Verify keys exist
        assert!(child.contains_key(b"apple"));
        assert!(child.contains_key(b"apricot"));
        assert!(!child.contains_key(b"banana"));

        // Insert at empty path (marks node as final)
        assert!(child.insert_key(b""));
        assert!(!child.insert_key(b"")); // Already final
    }

    #[test]
    fn test_child_node_remove_key_bucket() {
        let mut bucket = StringBucket::new();
        bucket.insert_key(b"apple").unwrap();
        bucket.insert_key(b"banana").unwrap();

        let mut child = ChildNode::bucket(bucket);

        // Remove existing key
        assert!(child.remove_key(b"apple"));
        assert!(!child.contains_key(b"apple"));

        // Remove non-existing key
        assert!(!child.remove_key(b"apple"));

        // Verify other key still exists
        assert!(child.contains_key(b"banana"));
    }

    #[test]
    fn test_child_node_remove_key_art() {
        let node = Node::N4(Box::new(Node4::new()));
        let mut child = ChildNode::art_node_with_children(node, true, None, Vec::new());

        // Insert some keys
        assert!(child.insert_key(b"apple"));

        // Remove from final state
        assert!(child.remove_key(b""));

        // Remove existing key
        assert!(child.remove_key(b"apple"));
        assert!(!child.contains_key(b"apple"));

        // Remove non-existing key
        assert!(!child.remove_key(b"apple"));
    }

    #[test]
    fn test_child_node_nested_art_operations() {
        // Create a nested ART structure: root ART -> child ART -> bucket
        let inner_node = Node::N4(Box::new(Node4::new()));
        let inner_bucket = StringBucket::new();
        let inner_child = ChildNode::art_node_with_children(
            inner_node,
            false,
            None,
            vec![(b'p', ChildNode::Bucket(inner_bucket))],
        );

        let outer_node = Node::N4(Box::new(Node4::new()));
        let mut outer_child = ChildNode::art_node_with_children(
            outer_node,
            false,
            None,
            vec![(b'a', inner_child)],
        );

        // Insert through nested structure
        assert!(outer_child.insert_key(b"apple")); // a -> p -> ple

        // Verify the key exists
        assert!(outer_child.contains_key(b"apple"));
        assert!(!outer_child.contains_key(b"apricot"));

        // Remove through nested structure
        assert!(outer_child.remove_key(b"apple"));
        assert!(!outer_child.contains_key(b"apple"));
    }

    #[test]
    fn test_child_node_disk_ref_operations() {
        let ptr = SwizzledPtr::null();
        let mut child = ChildNode::disk_ref(ptr);

        // All operations on disk ref return false (not supported without loading)
        assert!(!child.insert_key(b"test"));
        assert!(!child.remove_key(b"test"));
        assert!(!child.contains_key(b"test"));
    }

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
