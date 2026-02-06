//! Disk location registry for tracking persisted node locations.
//!
//! This module maps node paths to their disk locations (SwizzledPtr) after checkpoint.
//! Only nodes in the registry can be evicted, as they have valid disk representations.

use std::collections::HashMap;

use super::lru_tracker::LruRegistry;
use crate::persistent_artrie::swizzled_ptr::{NodeType, SwizzledPtr};

/// Information about an evictable node.
#[derive(Debug, Clone)]
pub struct EvictableNode {
    /// Path from root to this node (sequence of edge labels).
    pub path: Vec<u8>,
    /// Disk location from last checkpoint.
    pub disk_ptr: SwizzledPtr,
    /// Estimated memory size in bytes.
    pub size_bytes: usize,
    /// Depth in the trie (0 = root children).
    pub depth: usize,
    /// Node type for statistics.
    pub node_type: NodeType,
}

impl EvictableNode {
    /// Create a new evictable node entry.
    pub fn new(
        path: Vec<u8>,
        disk_ptr: SwizzledPtr,
        size_bytes: usize,
        depth: usize,
        node_type: NodeType,
    ) -> Self {
        Self {
            path,
            disk_ptr,
            size_bytes,
            depth,
            node_type,
        }
    }
}

/// Evictable node for char-level tries.
#[derive(Debug, Clone)]
pub struct EvictableCharNode {
    /// Path from root to this node (sequence of char edge labels).
    pub path: Vec<char>,
    /// Disk location from last checkpoint.
    pub disk_ptr: SwizzledPtr,
    /// Estimated memory size in bytes.
    pub size_bytes: usize,
    /// Depth in the trie (0 = root children).
    pub depth: usize,
    /// Node type for statistics.
    pub node_type: NodeType,
}

impl EvictableCharNode {
    /// Create a new evictable char node entry.
    pub fn new(
        path: Vec<char>,
        disk_ptr: SwizzledPtr,
        size_bytes: usize,
        depth: usize,
        node_type: NodeType,
    ) -> Self {
        Self {
            path,
            disk_ptr,
            size_bytes,
            depth,
            node_type,
        }
    }
}

/// Registry mapping node paths to their disk locations.
///
/// Populated during checkpoint and used by the eviction coordinator to
/// determine which nodes can be safely evicted (i.e., have valid disk
/// representations).
///
/// # Lifetime
///
/// The registry is invalidated after any write operation, as nodes may
/// have been modified since the last checkpoint. A new registry is
/// populated during each checkpoint.
///
/// # Memory Overhead
///
/// Each entry uses approximately:
/// - Path length + 8 bytes (Vec overhead)
/// - 8 bytes for the SwizzledPtr
/// - 8 bytes for size_bytes
/// - 8 bytes for depth
/// - 8 bytes for HashMap overhead
///
/// For a trie with 1M nodes and average path length of 10 bytes,
/// this is ~50MB of registry overhead.
pub struct DiskLocationRegistry {
    /// Maps path hash to evictable node info (for byte-level tries).
    locations: HashMap<u64, EvictableNode>,
    /// Maps path hash to evictable char node info (for char-level tries).
    char_locations: HashMap<u64, EvictableCharNode>,
    /// Total size of tracked nodes.
    total_size_bytes: usize,
    /// Number of nodes by type.
    node_type_counts: HashMap<NodeType, usize>,
    /// Whether this registry is valid (not invalidated by writes).
    valid: bool,
}

impl DiskLocationRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self {
            locations: HashMap::new(),
            char_locations: HashMap::new(),
            total_size_bytes: 0,
            node_type_counts: HashMap::new(),
            valid: true,
        }
    }

    /// Create a registry with pre-allocated capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            locations: HashMap::with_capacity(capacity),
            char_locations: HashMap::with_capacity(capacity),
            total_size_bytes: 0,
            node_type_counts: HashMap::new(),
            valid: true,
        }
    }

    /// Register a byte-level node's disk location.
    ///
    /// Called during checkpoint serialization to record where each node
    /// was written to disk.
    pub fn register(
        &mut self,
        path: Vec<u8>,
        disk_ptr: SwizzledPtr,
        size_bytes: usize,
        depth: usize,
        node_type: NodeType,
    ) {
        let hash = LruRegistry::path_hash(&path);
        let node = EvictableNode::new(path, disk_ptr, size_bytes, depth, node_type);

        // Update totals if this is a new entry
        if let Some(old) = self.locations.insert(hash, node) {
            self.total_size_bytes = self.total_size_bytes.saturating_sub(old.size_bytes);
            *self.node_type_counts.entry(old.node_type).or_insert(0) -= 1;
        }

        self.total_size_bytes += size_bytes;
        *self.node_type_counts.entry(node_type).or_insert(0) += 1;
    }

    /// Register a char-level node's disk location.
    pub fn register_char(
        &mut self,
        path: Vec<char>,
        disk_ptr: SwizzledPtr,
        size_bytes: usize,
        depth: usize,
        node_type: NodeType,
    ) {
        use super::lru_tracker::hash_char_path;
        let hash = hash_char_path(&path);
        let node = EvictableCharNode::new(path, disk_ptr, size_bytes, depth, node_type);

        if let Some(old) = self.char_locations.insert(hash, node) {
            self.total_size_bytes = self.total_size_bytes.saturating_sub(old.size_bytes);
            *self.node_type_counts.entry(old.node_type).or_insert(0) -= 1;
        }

        self.total_size_bytes += size_bytes;
        *self.node_type_counts.entry(node_type).or_insert(0) += 1;
    }

    /// Get a byte-level node's disk location by path hash.
    pub fn get(&self, path_hash: u64) -> Option<&EvictableNode> {
        self.locations.get(&path_hash)
    }

    /// Get a char-level node's disk location by path hash.
    pub fn get_char(&self, path_hash: u64) -> Option<&EvictableCharNode> {
        self.char_locations.get(&path_hash)
    }

    /// Remove a byte-level node from the registry (after eviction).
    pub fn remove(&mut self, path_hash: u64) -> Option<EvictableNode> {
        if let Some(node) = self.locations.remove(&path_hash) {
            self.total_size_bytes = self.total_size_bytes.saturating_sub(node.size_bytes);
            *self.node_type_counts.entry(node.node_type).or_insert(0) -= 1;
            Some(node)
        } else {
            None
        }
    }

    /// Remove a char-level node from the registry (after eviction).
    pub fn remove_char(&mut self, path_hash: u64) -> Option<EvictableCharNode> {
        if let Some(node) = self.char_locations.remove(&path_hash) {
            self.total_size_bytes = self.total_size_bytes.saturating_sub(node.size_bytes);
            *self.node_type_counts.entry(node.node_type).or_insert(0) -= 1;
            Some(node)
        } else {
            None
        }
    }

    /// Check if a path hash is registered.
    pub fn contains(&self, path_hash: u64) -> bool {
        self.locations.contains_key(&path_hash) || self.char_locations.contains_key(&path_hash)
    }

    /// Get the number of registered byte-level nodes.
    pub fn len(&self) -> usize {
        self.locations.len()
    }

    /// Get the number of registered char-level nodes.
    pub fn char_len(&self) -> usize {
        self.char_locations.len()
    }

    /// Get the total number of registered nodes.
    pub fn total_len(&self) -> usize {
        self.locations.len() + self.char_locations.len()
    }

    /// Check if the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.locations.is_empty() && self.char_locations.is_empty()
    }

    /// Get the total size of tracked nodes.
    pub fn total_size_bytes(&self) -> usize {
        self.total_size_bytes
    }

    /// Get the count of nodes by type.
    pub fn count_by_type(&self, node_type: NodeType) -> usize {
        *self.node_type_counts.get(&node_type).unwrap_or(&0)
    }

    /// Check if this registry is still valid.
    ///
    /// Returns `false` if any write operations have occurred since
    /// the registry was populated.
    pub fn is_valid(&self) -> bool {
        self.valid
    }

    /// Invalidate this registry.
    ///
    /// Called when a write operation occurs, making the registry
    /// unsuitable for eviction decisions.
    pub fn invalidate(&mut self) {
        self.valid = false;
    }

    /// Clear all entries and reset to valid state.
    pub fn clear(&mut self) {
        self.locations.clear();
        self.char_locations.clear();
        self.total_size_bytes = 0;
        self.node_type_counts.clear();
        self.valid = true;
    }

    /// Get an iterator over all byte-level path hashes.
    pub fn path_hashes(&self) -> impl Iterator<Item = u64> + '_ {
        self.locations.keys().copied()
    }

    /// Get an iterator over all char-level path hashes.
    pub fn char_path_hashes(&self) -> impl Iterator<Item = u64> + '_ {
        self.char_locations.keys().copied()
    }

    /// Get candidates for eviction, filtered by minimum depth.
    ///
    /// Returns path hashes of nodes at or below `min_depth`.
    pub fn eviction_candidates(&self, min_depth: usize) -> Vec<u64> {
        self.locations
            .iter()
            .filter(|(_, node)| node.depth >= min_depth)
            .map(|(hash, _)| *hash)
            .collect()
    }

    /// Get char candidates for eviction, filtered by minimum depth.
    pub fn char_eviction_candidates(&self, min_depth: usize) -> Vec<u64> {
        self.char_locations
            .iter()
            .filter(|(_, node)| node.depth >= min_depth)
            .map(|(hash, _)| *hash)
            .collect()
    }

    /// Select nodes for eviction up to a target size.
    ///
    /// Uses the LRU registry to prioritize cold nodes. Returns a list
    /// of (path_hash, EvictableNode) pairs.
    ///
    /// # Arguments
    ///
    /// * `target_bytes` - Target amount of memory to free
    /// * `lru_registry` - LRU registry for coldness scoring
    /// * `min_depth` - Minimum depth to evict
    /// * `max_count` - Maximum number of nodes to return
    pub fn select_for_eviction(
        &self,
        target_bytes: usize,
        lru_registry: &LruRegistry,
        min_depth: usize,
        max_count: usize,
    ) -> Vec<(u64, EvictableNode)> {
        if !self.valid || self.locations.is_empty() {
            return Vec::new();
        }

        // Collect candidates with coldness scores
        let mut candidates: Vec<_> = self.locations
            .iter()
            .filter(|(_, node)| node.depth >= min_depth)
            .map(|(hash, node)| {
                let coldness = lru_registry.coldness_score_hash(*hash);
                (*hash, node.clone(), coldness)
            })
            .collect();

        // Sort by coldness (coldest first)
        candidates.sort_unstable_by_key(|(_, _, coldness)| std::cmp::Reverse(*coldness));

        // Select until target bytes reached or max count
        let mut result = Vec::with_capacity(max_count.min(candidates.len()));
        let mut total_bytes = 0;

        for (hash, node, _) in candidates {
            if result.len() >= max_count {
                break;
            }
            total_bytes += node.size_bytes;
            result.push((hash, node));

            if total_bytes >= target_bytes {
                break;
            }
        }

        result
    }

    /// Select char nodes for eviction up to a target size.
    pub fn select_char_for_eviction(
        &self,
        target_bytes: usize,
        lru_registry: &LruRegistry,
        min_depth: usize,
        max_count: usize,
    ) -> Vec<(u64, EvictableCharNode)> {
        if !self.valid || self.char_locations.is_empty() {
            return Vec::new();
        }

        let mut candidates: Vec<_> = self.char_locations
            .iter()
            .filter(|(_, node)| node.depth >= min_depth)
            .map(|(hash, node)| {
                let coldness = lru_registry.coldness_score_hash(*hash);
                (*hash, node.clone(), coldness)
            })
            .collect();

        candidates.sort_unstable_by_key(|(_, _, coldness)| std::cmp::Reverse(*coldness));

        let mut result = Vec::with_capacity(max_count.min(candidates.len()));
        let mut total_bytes = 0;

        for (hash, node, _) in candidates {
            if result.len() >= max_count {
                break;
            }
            total_bytes += node.size_bytes;
            result.push((hash, node));

            if total_bytes >= target_bytes {
                break;
            }
        }

        result
    }
}

impl Default for DiskLocationRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_disk_ptr(block_id: u32, offset: u32) -> SwizzledPtr {
        SwizzledPtr::on_disk(block_id, offset, NodeType::Node16)
    }

    #[test]
    fn test_registry_basic() {
        let mut registry = DiskLocationRegistry::new();
        assert!(registry.is_empty());
        assert!(registry.is_valid());

        registry.register(
            b"test".to_vec(),
            make_disk_ptr(1, 100),
            256,
            1,
            NodeType::Node16,
        );

        assert_eq!(registry.len(), 1);
        assert_eq!(registry.total_size_bytes(), 256);
        assert_eq!(registry.count_by_type(NodeType::Node16), 1);

        let hash = LruRegistry::path_hash(b"test");
        let node = registry.get(hash).expect("node should exist");
        assert_eq!(node.path, b"test".to_vec());
        assert_eq!(node.size_bytes, 256);
        assert_eq!(node.depth, 1);
    }

    #[test]
    fn test_registry_remove() {
        let mut registry = DiskLocationRegistry::new();

        registry.register(
            b"node1".to_vec(),
            make_disk_ptr(1, 100),
            256,
            1,
            NodeType::Node4,
        );
        registry.register(
            b"node2".to_vec(),
            make_disk_ptr(1, 200),
            512,
            2,
            NodeType::Node16,
        );

        assert_eq!(registry.len(), 2);
        assert_eq!(registry.total_size_bytes(), 768);

        let hash1 = LruRegistry::path_hash(b"node1");
        let removed = registry.remove(hash1);
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().size_bytes, 256);

        assert_eq!(registry.len(), 1);
        assert_eq!(registry.total_size_bytes(), 512);
        assert_eq!(registry.count_by_type(NodeType::Node4), 0);
        assert_eq!(registry.count_by_type(NodeType::Node16), 1);
    }

    #[test]
    fn test_registry_invalidate() {
        let mut registry = DiskLocationRegistry::new();
        registry.register(
            b"test".to_vec(),
            make_disk_ptr(1, 100),
            256,
            1,
            NodeType::Node16,
        );

        assert!(registry.is_valid());

        registry.invalidate();
        assert!(!registry.is_valid());

        registry.clear();
        assert!(registry.is_valid());
        assert!(registry.is_empty());
    }

    #[test]
    fn test_eviction_candidates() {
        let mut registry = DiskLocationRegistry::new();

        // Add nodes at different depths
        for depth in 0..5 {
            let path = format!("depth{}", depth);
            registry.register(
                path.into_bytes(),
                make_disk_ptr(1, depth as u32 * 100),
                256,
                depth,
                NodeType::Node16,
            );
        }

        assert_eq!(registry.len(), 5);

        // Min depth 0 should include all
        let candidates = registry.eviction_candidates(0);
        assert_eq!(candidates.len(), 5);

        // Min depth 2 should exclude depths 0 and 1
        let candidates = registry.eviction_candidates(2);
        assert_eq!(candidates.len(), 3);

        // Min depth 5 should include none
        let candidates = registry.eviction_candidates(5);
        assert_eq!(candidates.len(), 0);
    }

    #[test]
    fn test_select_for_eviction() {
        let mut registry = DiskLocationRegistry::new();
        let lru = LruRegistry::new();

        // Add nodes with different sizes
        for i in 0..10 {
            let path = format!("node{}", i);
            registry.register(
                path.clone().into_bytes(),
                make_disk_ptr(1, i * 100),
                100 * (i as usize + 1), // Sizes: 100, 200, 300, ...
                1,
                NodeType::Node16,
            );

            // Touch in LRU to create different access patterns
            // Earlier nodes are touched less (colder)
            for _ in 0..i {
                lru.touch(path.as_bytes());
            }
        }

        // Select nodes to free 500 bytes
        let selected = registry.select_for_eviction(500, &lru, 1, 5);

        // Should select coldest nodes first
        assert!(!selected.is_empty());

        let total_bytes: usize = selected.iter().map(|(_, n)| n.size_bytes).sum();
        assert!(total_bytes >= 500 || selected.len() >= 5);
    }

    #[test]
    fn test_char_registry() {
        let mut registry = DiskLocationRegistry::new();

        registry.register_char(
            vec!['日', '本', '語'],
            make_disk_ptr(1, 100),
            512,
            1,
            NodeType::CharNode16,
        );

        assert_eq!(registry.char_len(), 1);
        assert_eq!(registry.total_size_bytes(), 512);
        assert_eq!(registry.count_by_type(NodeType::CharNode16), 1);

        use super::super::lru_tracker::hash_char_path;
        let hash = hash_char_path(&['日', '本', '語']);
        let node = registry.get_char(hash).expect("char node should exist");
        assert_eq!(node.path, vec!['日', '本', '語']);
    }
}
