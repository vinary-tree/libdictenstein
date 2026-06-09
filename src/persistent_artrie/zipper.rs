//! Persistent ART zipper implementation.
//!
//! This module provides a zipper implementation for PersistentARTrie that uses
//! node-based navigation with lock-per-operation pattern for thread safety.

#[allow(unused_imports)]
use crate::sync_compat::RwLock;
use std::borrow::Cow;
#[allow(unused_imports)]
use std::sync::Arc;

#[allow(unused_imports)]
use super::bucket::StringBucket;
use super::dict_impl::PersistentARTrie;
#[allow(unused_imports)]
use super::transitions::ChildNode;
use super::SharedARTrie;
// F4: the `.read()` compat shim on the collapsed `Arc<PersistentARTrie>` handle.
use crate::persistent_artrie_core::shared_access::SharedTrieAccess;
// **L3.2:** overlay-backed zipper navigation.
use crate::persistent_artrie_core::overlay::flip::LockFreeOverlay;
use crate::value::DictionaryValue;
use crate::zipper::{DictZipper, ValuedDictZipper};

/// Zipper for Persistent ART dictionaries.
///
/// `PersistentARTrieZipper` provides efficient navigation through Persistent ART
/// structures using a path-based approach with thread-safe concurrent access.
///
/// # Design
///
/// The zipper stores:
/// - `inner`: Shared reference to the trie inner structure (Arc<RwLock>)
/// - `path`: Current path from root to focus
/// - `state`: Current navigation state (root, bucket, or ART node)
///
/// Operations use a lock-per-operation pattern, acquiring a read lock only for
/// the duration of each operation to maximize concurrency.
///
/// # Thread Safety
///
/// Each operation acquires a read lock, performs the operation, and releases it.
/// This allows:
/// - Multiple concurrent readers (navigating different zippers)
/// - Exclusive write access for modifications (insert/remove)
///
/// # Examples
///
/// ```text
/// use libdictenstein::DictZipper;
/// use libdictenstein::persistent_artrie::PersistentARTrie;
/// use libdictenstein::persistent_artrie::zipper::PersistentARTrieZipper;
///
/// let mut dict: PersistentARTrie<()> = PersistentARTrie::new();
/// dict.insert("cat");
/// dict.insert("catch");
///
/// let zipper = make_zipper(dict);
///
/// // Navigate through "cat"
/// if let Some(c) = zipper.descend(b'c') {
///     if let Some(a) = c.descend(b'a') {
///         if let Some(t) = a.descend(b't') {
///             if t.is_final() {
///                 println!("Found 'cat'");
///             }
///         }
///     }
/// }
/// ```
#[derive(Clone)]
pub struct PersistentARTrieZipper<V: DictionaryValue = ()> {
    /// Shared reference to trie (thread-safe wrapper)
    trie: SharedARTrie<V>,

    /// Path from root to current position
    path: Vec<u8>,
}

impl<V: DictionaryValue> PersistentARTrieZipper<V> {
    /// Create a new zipper at the root of the Persistent ART.
    ///
    /// # Arguments
    ///
    /// * `dict` - Reference to the PersistentARTrie dictionary
    ///
    /// # Examples
    ///
    /// ```text
    /// use libdictenstein::persistent_artrie::{PersistentARTrie, SharedARTrie};
    /// use libdictenstein::persistent_artrie::zipper::PersistentARTrieZipper;
    /// use std::sync::Arc;
    /// use parking_lot::RwLock;
    ///
    /// let dict: PersistentARTrie<()> = PersistentARTrie::new();
    /// let shared: SharedARTrie<()> = Arc::new(RwLock::new(dict));
    /// let zipper = PersistentARTrieZipper::new(shared);
    /// ```
    /// Create a new zipper from a shared trie reference.
    ///
    /// The zipper provides read-only navigation through the trie.
    /// For thread-safe concurrent access, wrap the trie in `SharedARTrie`
    /// (i.e., `Arc<RwLock<PersistentARTrie<V>>>`).
    ///
    /// **L3.2:** overlay-backed — `has_path`/`is_final_at_path`/`get_children_at_path` navigate
    /// the lock-free overlay (the production rep), so the zipper sees the live dictionary on every
    /// (now universally overlay-routed) trie. (Replaces the M3-DEFER `log::warn!` "navigates an
    /// EMPTY owned tree" stub — overlay traversal is now implemented.)
    pub fn new(trie: SharedARTrie<V>) -> Self {
        PersistentARTrieZipper {
            trie,
            path: Vec::new(),
        }
    }

    /// Create a new zipper from a shared trie reference (alias for `new`).
    pub fn new_from_shared(trie: SharedARTrie<V>) -> Self {
        Self::new(trie)
    }

    /// Get the current path from root.
    ///
    /// Returns the sequence of edge labels from root to current position.
    pub fn current_path(&self) -> &[u8] {
        &self.path
    }
}

impl<V: DictionaryValue> DictZipper for PersistentARTrieZipper<V> {
    type Unit = u8;

    fn is_final(&self) -> bool {
        let guard = self.trie.read();
        self.is_final_at_path(&guard, &self.path)
    }

    fn descend(&self, label: Self::Unit) -> Option<Self> {
        let guard = self.trie.read();

        // Check if the path + label leads to a valid position
        let mut new_path = self.path.clone();
        new_path.push(label);

        if self.has_path(&guard, &new_path) {
            Some(PersistentARTrieZipper {
                trie: self.trie.clone(),
                path: new_path,
            })
        } else {
            None
        }
    }

    fn path(&self) -> Vec<Self::Unit> {
        self.path.clone()
    }

    fn children(&self) -> impl Iterator<Item = (Self::Unit, Self)> {
        // Collect children to avoid holding lock during iteration
        let children: Vec<u8> = {
            let guard = self.trie.read();
            self.get_children_at_path(&guard, &self.path)
        };

        // Create iterator from collected children
        let trie_clone = self.trie.clone();
        let base_path = self.path.clone();
        children.into_iter().map(move |label| {
            let mut new_path = base_path.clone();
            new_path.push(label);
            (
                label,
                PersistentARTrieZipper {
                    trie: trie_clone.clone(),
                    path: new_path,
                },
            )
        })
    }
}

impl<V: DictionaryValue> PersistentARTrieZipper<V> {
    /// Check if a path exists in the trie.
    ///
    /// **L3.2:** the struct zipper navigates the lock-free OVERLAY (the production rep), not the
    /// owned tree — every trie is overlay-routed now (`route_overlay()` universal). `overlay_navigate`
    /// descends unit-by-unit (the live overlay is un-path-compressed) and faults OnDisk prefix
    /// nodes READ-ONLY, so `is_some()` ⇔ the path is a strict-prefix-or-final member — matching the
    /// prior owned `bucket_has_path`'s `suffix.starts_with(path)` semantics.
    fn has_path(&self, inner: &PersistentARTrie<V>, path: &[u8]) -> bool {
        inner.overlay_navigate(path).is_some()
    }

    /// Check if bucket has entries starting with path
    fn bucket_has_path(&self, bucket: &StringBucket, path: &[u8]) -> bool {
        // Empty path always matches (root of bucket)
        if path.is_empty() {
            return true;
        }

        // Check if any entry starts with this path
        for i in 0..bucket.len() {
            if let Some(entry) = bucket.get_entry(i) {
                let suffix = bucket.get_suffix(&entry);
                if suffix.starts_with(path) || suffix == path {
                    return true;
                }
            }
        }
        false
    }

    /// Check if the position is final (a term). **L3.2:** overlay-backed (see [`Self::has_path`]).
    fn is_final_at_path(&self, inner: &PersistentARTrie<V>, path: &[u8]) -> bool {
        inner.overlay_navigate(path).is_some_and(|n| n.is_final())
    }

    /// Get all child edge labels at the current path. **L3.2:** overlay-backed (see
    /// [`Self::has_path`]). Children are returned in the overlay's ascending key order; the
    /// consumers (DictZipper / set-combinators) treat children as a set, so order is unobserved.
    fn get_children_at_path(&self, inner: &PersistentARTrie<V>, path: &[u8]) -> Vec<u8> {
        match inner.overlay_navigate(path) {
            Some(node) => node.iter_children().map(|(k, _)| *k).collect(),
            None => Vec::new(),
        }
    }

    /// Get children from bucket at given prefix
    fn get_bucket_children(&self, bucket: &StringBucket, prefix: &[u8]) -> Vec<u8> {
        let mut seen = [false; 256];
        let mut children = Vec::new();

        for i in 0..bucket.len() {
            if let Some(entry) = bucket.get_entry(i) {
                let suffix = bucket.get_suffix(&entry);
                if suffix.starts_with(prefix) && suffix.len() > prefix.len() {
                    let next_byte = suffix[prefix.len()];
                    if !seen[next_byte as usize] {
                        seen[next_byte as usize] = true;
                        children.push(next_byte);
                    }
                }
            }
        }

        children
    }

    /// Resolve a DiskRef child to its actual node content.
    ///
    /// Returns `Cow::Borrowed` for already-loaded nodes (no allocation),
    /// and `Cow::Owned` for newly resolved disk refs.
    ///
    /// # Arguments
    ///
    /// * `inner` - The trie inner structure containing the buffer manager
    /// * `child` - The child node to potentially resolve
    ///
    /// # Returns
    ///
    /// `Some(Cow::Borrowed(child))` for in-memory nodes,
    /// `Some(Cow::Owned(resolved))` for successfully resolved disk refs,
    /// `None` for failed disk ref resolution.
    fn resolve_child<'a>(
        inner: &PersistentARTrie<V>,
        child: &'a ChildNode,
    ) -> Option<Cow<'a, ChildNode>> {
        match child {
            ChildNode::DiskRef { ptr } => {
                if let Some(disk_location) = ptr.disk_location() {
                    match inner.resolve_disk_ref(&disk_location) {
                        Ok(resolved) => Some(Cow::Owned(resolved)),
                        Err(e) => {
                            log::warn!(
                                "Failed to resolve disk ref at block {}, offset {}: {}",
                                disk_location.block_id,
                                disk_location.offset,
                                e
                            );
                            None
                        }
                    }
                } else {
                    None
                }
            }
            _ => Some(Cow::Borrowed(child)),
        }
    }

    /// Resolve a DiskRef child to its actual node content (non-persistent variant).
    ///
    /// Without the persistent-artrie feature, DiskRef nodes cannot be resolved
    /// and return None.

    /// Check if a path exists within a child node, resolving DiskRefs as needed.
    fn has_path_in_child(
        &self,
        inner: &PersistentARTrie<V>,
        child: &ChildNode,
        remaining: &[u8],
    ) -> bool {
        let resolved = match Self::resolve_child(inner, child) {
            Some(cow) => cow,
            None => return false,
        };

        match &*resolved {
            ChildNode::Bucket(bucket) => self.bucket_has_path(bucket, remaining),
            ChildNode::ArtNode { children, .. } => {
                if remaining.is_empty() {
                    true
                } else {
                    let next_byte = remaining[0];
                    let next_remaining = &remaining[1..];
                    for (b, nc) in children {
                        if *b == next_byte {
                            return self.has_path_in_child(inner, nc, next_remaining);
                        }
                    }
                    false
                }
            }
            ChildNode::DiskRef { .. } => {
                // Should not reach here after resolution, but handle gracefully
                false
            }
        }
    }

    /// Check if a path leads to a final state within a child node, resolving DiskRefs as needed.
    fn is_final_in_child(
        &self,
        inner: &PersistentARTrie<V>,
        child: &ChildNode,
        remaining: &[u8],
    ) -> bool {
        let resolved = match Self::resolve_child(inner, child) {
            Some(cow) => cow,
            None => return false,
        };

        match &*resolved {
            ChildNode::Bucket(bucket) => bucket.contains(remaining),
            ChildNode::ArtNode {
                is_final, children, ..
            } => {
                if remaining.is_empty() {
                    *is_final
                } else {
                    let next_byte = remaining[0];
                    let next_remaining = &remaining[1..];
                    for (b, nc) in children {
                        if *b == next_byte {
                            return self.is_final_in_child(inner, nc, next_remaining);
                        }
                    }
                    false
                }
            }
            ChildNode::DiskRef { .. } => {
                // Should not reach here after resolution, but handle gracefully
                false
            }
        }
    }

    /// Get children at a path within a child node, resolving DiskRefs as needed.
    fn get_children_in_child(
        &self,
        inner: &PersistentARTrie<V>,
        child: &ChildNode,
        path: &[u8],
    ) -> Vec<u8> {
        let resolved = match Self::resolve_child(inner, child) {
            Some(cow) => cow,
            None => return Vec::new(),
        };

        match &*resolved {
            ChildNode::Bucket(bucket) => self.get_bucket_children(bucket, path),
            ChildNode::ArtNode { children, .. } => {
                if path.is_empty() {
                    // At this node, return its direct children
                    children.iter().map(|(b, _)| *b).collect()
                } else {
                    let next_byte = path[0];
                    let next_path = &path[1..];
                    for (b, nc) in children {
                        if *b == next_byte {
                            return self.get_children_in_child(inner, nc, next_path);
                        }
                    }
                    Vec::new()
                }
            }
            ChildNode::DiskRef { .. } => {
                // Should not reach here after resolution, but handle gracefully
                Vec::new()
            }
        }
    }
}

impl<V: DictionaryValue> ValuedDictZipper for PersistentARTrieZipper<V> {
    type Value = V;

    fn value(&self) -> Option<Self::Value> {
        // Value retrieval is not implemented because internal storage uses Vec<u8>
        // while the trait requires V. To implement this, DictionaryValue would need
        // serialization bounds (e.g., serde) to convert between V and Vec<u8>.
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper function to create a zipper from a dict for tests.
    fn make_zipper<V: DictionaryValue>(dict: PersistentARTrie<V>) -> PersistentARTrieZipper<V> {
        let shared: SharedARTrie<V> = Arc::new(dict);
        PersistentARTrieZipper::new(shared)
    }

    #[test]
    fn test_root_zipper_not_final() {
        let dict: PersistentARTrie<()> = PersistentARTrie::new();
        dict.insert("test");

        let zipper = make_zipper(dict);
        assert!(!zipper.is_final());
    }

    #[test]
    fn test_descend_single_term() {
        let dict: PersistentARTrie<()> = PersistentARTrie::new();
        dict.insert("cat");

        let zipper = make_zipper(dict);

        let c = zipper.descend(b'c').expect("should have 'c'");
        let a = c.descend(b'a').expect("should have 'a'");
        let t = a.descend(b't').expect("should have 't'");

        assert!(t.is_final());
    }

    #[test]
    fn test_descend_nonexistent() {
        let dict: PersistentARTrie<()> = PersistentARTrie::new();
        dict.insert("cat");

        let zipper = make_zipper(dict);

        assert!(zipper.descend(b'x').is_none());
    }

    #[test]
    fn test_children() {
        let dict: PersistentARTrie<()> = PersistentARTrie::new();
        dict.insert("cat");
        dict.insert("car");
        dict.insert("can");

        let zipper = make_zipper(dict);

        // Root should have 'c' as child
        let children: Vec<_> = zipper.children().collect();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].0, b'c');
    }

    #[test]
    fn test_path() {
        let dict: PersistentARTrie<()> = PersistentARTrie::new();
        dict.insert("cat");

        let zipper = make_zipper(dict);
        assert!(zipper.path().is_empty());

        let c = zipper.descend(b'c').unwrap();
        assert_eq!(c.path(), vec![b'c']);

        let a = c.descend(b'a').unwrap();
        assert_eq!(a.path(), vec![b'c', b'a']);
    }

    #[test]
    fn test_empty_string() {
        let dict: PersistentARTrie<()> = PersistentARTrie::new();
        dict.insert("");
        dict.insert("test");

        let zipper = make_zipper(dict);
        assert!(zipper.is_final()); // Empty string makes root final
    }

    #[test]
    fn test_clone() {
        let dict: PersistentARTrie<()> = PersistentARTrie::new();
        dict.insert("test");

        let zipper1 = make_zipper(dict);
        let zipper2 = zipper1.clone();

        assert_eq!(zipper1.path(), zipper2.path());
        assert_eq!(zipper1.is_final(), zipper2.is_final());
    }

    #[test]
    fn test_resolve_child_returns_borrowed_for_bucket() {
        use std::borrow::Cow;

        let dict: PersistentARTrie<()> = PersistentARTrie::new();
        dict.insert("test");

        let bucket = super::StringBucket::new();
        let child = ChildNode::Bucket(bucket);

        let resolved = PersistentARTrieZipper::<()>::resolve_child(&dict, &child);
        assert!(resolved.is_some());

        // Should return borrowed (no clone needed for in-memory nodes)
        if let Some(cow) = resolved {
            assert!(matches!(cow, Cow::Borrowed(_)));
        }
    }

    #[test]
    fn test_resolve_child_returns_borrowed_for_art_node() {
        use super::super::nodes::{Node, Node4};
        use std::borrow::Cow;

        let dict: PersistentARTrie<()> = PersistentARTrie::new();
        dict.insert("test");

        let node = Node::N4(Box::new(Node4::new()));
        let child = ChildNode::art_node(node, false, None);

        let resolved = PersistentARTrieZipper::<()>::resolve_child(&dict, &child);
        assert!(resolved.is_some());

        // Should return borrowed (no clone needed for in-memory nodes)
        if let Some(cow) = resolved {
            assert!(matches!(cow, Cow::Borrowed(_)));
        }
    }

    #[test]
    fn test_resolve_child_returns_none_for_disk_ref_without_manager() {
        use super::super::swizzled_ptr::SwizzledPtr;

        let dict: PersistentARTrie<()> = PersistentARTrie::new();
        dict.insert("test");

        let ptr = SwizzledPtr::null();
        let child = ChildNode::disk_ref(ptr);

        // Without a buffer manager, DiskRef resolution should return None
        let resolved = PersistentARTrieZipper::<()>::resolve_child(&dict, &child);
        assert!(resolved.is_none());
    }

    #[test]
    fn test_deep_navigation_with_many_terms() {
        // Test that navigation works correctly through deeply nested ART structures
        let dict: PersistentARTrie<()> = PersistentARTrie::new();

        // Insert terms that will create nested ART nodes
        for i in 0..100 {
            let term = format!("prefix_{:03}_suffix", i);
            dict.insert(&term);
        }

        let zipper = make_zipper(dict);

        // Navigate through "prefix_050_suffix"
        let mut current = zipper;
        for byte in b"prefix_050_suffix" {
            current = current.descend(*byte).expect("should be able to descend");
        }
        assert!(current.is_final());
    }

    #[test]
    fn test_children_with_many_terms() {
        // Test that children() correctly returns all child edges
        let dict: PersistentARTrie<()> = PersistentARTrie::new();

        // Insert terms with different first bytes
        dict.insert("apple");
        dict.insert("banana");
        dict.insert("cherry");
        dict.insert("date");
        dict.insert("elderberry");

        let zipper = make_zipper(dict);
        let children: Vec<u8> = zipper.children().map(|(b, _)| b).collect();

        assert_eq!(children.len(), 5);
        assert!(children.contains(&b'a'));
        assert!(children.contains(&b'b'));
        assert!(children.contains(&b'c'));
        assert!(children.contains(&b'd'));
        assert!(children.contains(&b'e'));
    }

    #[test]
    fn test_recursive_has_path_through_nested_art() {
        // Ensure has_path works through multiple levels of ART nodes
        let dict: PersistentARTrie<()> = PersistentARTrie::new();

        // Create terms that will generate nested ART structure
        for prefix in &["aa", "ab", "ac", "ad"] {
            for suffix in &["1", "2", "3", "4"] {
                let term = format!("{}{}", prefix, suffix);
                dict.insert(&term);
            }
        }

        let zipper = make_zipper(dict);

        // Test descending through paths that exist
        let a = zipper.descend(b'a').expect("should have 'a'");
        let ab = a.descend(b'b').expect("should have 'b'");
        let ab3 = ab.descend(b'3').expect("should have '3'");
        assert!(ab3.is_final());

        // Test descending through paths that don't exist
        let aa = a.descend(b'a').expect("should have 'a'");
        assert!(aa.descend(b'x').is_none());
    }
}
