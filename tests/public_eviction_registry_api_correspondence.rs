#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::{
    DiskLocationRegistry, EvictableCharNode, EvictableNode, EvictionCoordinator, LruRegistry,
    NodeType, SwizzledPtr,
};

#[test]
#[expect(
    deprecated,
    reason = "this compatibility test pins the deprecated source-level method signature"
)]
fn public_eviction_node_shapes_and_owned_registry_records_are_explicit() {
    let _legacy_update_signature: fn(&EvictionCoordinator, DiskLocationRegistry) =
        EvictionCoordinator::update_disk_registry;
    let _owned_byte_lookup_signature: fn(&DiskLocationRegistry, u64) -> Option<EvictableNode> =
        DiskLocationRegistry::get_owned;
    let _owned_char_lookup_signature: fn(&DiskLocationRegistry, u64) -> Option<EvictableCharNode> =
        DiskLocationRegistry::get_char_owned;
    let byte_ptr = SwizzledPtr::on_disk(1, 16, NodeType::Node4);
    let byte = EvictableNode::new(
        b"byte/path".to_vec(),
        byte_ptr.clone(),
        64,
        3,
        NodeType::Node4,
    );
    assert_eq!(byte.path, b"byte/path");
    assert_eq!(byte.disk_ptr.to_raw(), byte_ptr.to_raw());
    assert_eq!(byte.size_bytes, 64);
    assert_eq!(byte.depth, 3);
    assert_eq!(byte.node_type, NodeType::Node4);

    let char_ptr = SwizzledPtr::on_disk(2, 32, NodeType::CharNode4);
    let char_node = EvictableCharNode::new(
        vec!['λ', '樹'],
        char_ptr.clone(),
        96,
        2,
        NodeType::CharNode4,
    );
    assert_eq!(char_node.path, vec!['λ', '樹']);
    assert_eq!(char_node.disk_ptr.to_raw(), char_ptr.to_raw());

    let mut registry = DiskLocationRegistry::new();
    registry.register(
        b"registered".to_vec(),
        byte_ptr.clone(),
        80,
        1,
        NodeType::Node4,
    );
    registry.register(
        b"registered/sibling".to_vec(),
        SwizzledPtr::on_disk(3, 48, NodeType::Node16),
        88,
        7,
        NodeType::Node16,
    );
    registry.register_char(
        vec!['登', '録'],
        char_ptr.clone(),
        112,
        1,
        NodeType::CharNode4,
    );
    registry.register_char(
        vec!['登', '録', '二'],
        SwizzledPtr::on_disk(4, 64, NodeType::CharNode16),
        120,
        9,
        NodeType::CharNode16,
    );

    let byte_hash = LruRegistry::path_hash(b"registered");
    let mut registered_byte = registry
        .get_owned(byte_hash)
        .expect("registered byte entry");
    assert_eq!(registered_byte.path, b"registered");
    assert_eq!(registered_byte.depth, 1);
    registered_byte.path.clear();
    let registered_byte_again = registry
        .get_owned(byte_hash)
        .expect("independent registered byte entry");
    assert_eq!(registered_byte_again.path, b"registered");

    let char_hash =
        libdictenstein::persistent_artrie::eviction::lru_tracker::hash_char_path(&['登', '録']);
    let mut registered_char = registry
        .get_char_owned(char_hash)
        .expect("registered char entry");
    assert_eq!(registered_char.path, vec!['登', '録']);
    assert_eq!(registered_char.depth, 1);
    registered_char.path.clear();
    let registered_char_again = registry
        .get_char_owned(char_hash)
        .expect("independent registered char entry");
    assert_eq!(registered_char_again.path, vec!['登', '録']);

    let selected_bytes =
        registry.select_for_eviction(usize::MAX, &LruRegistry::new(), 0, usize::MAX, 0);
    assert_eq!(selected_bytes.len(), 2);
    assert!(selected_bytes.iter().any(|(_, node)| node.depth == 1));
    assert!(selected_bytes.iter().any(|(_, node)| node.depth == 7));

    let selected_chars =
        registry.select_char_for_eviction(usize::MAX, &LruRegistry::new(), 0, usize::MAX, 0);
    assert_eq!(selected_chars.len(), 2);
    assert!(selected_chars.iter().any(|(_, node)| node.depth == 1));
    assert!(selected_chars.iter().any(|(_, node)| node.depth == 9));

    let removed_byte = registry.remove(byte_hash).expect("remove byte occurrence");
    assert_eq!(removed_byte.path, b"registered");
    let removed_char = registry
        .remove_char(char_hash)
        .expect("remove char occurrence");
    assert_eq!(removed_char.path, vec!['登', '録']);
    assert_eq!(
        registry
            .select_for_eviction(usize::MAX, &LruRegistry::new(), 0, usize::MAX, 0)
            .len(),
        1
    );
    assert_eq!(
        registry
            .select_char_for_eviction(usize::MAX, &LruRegistry::new(), 0, usize::MAX, 0)
            .len(),
        1
    );

    registry.register(b"registered".to_vec(), byte_ptr, 80, 1, NodeType::Node4);
    registry.register_char(vec!['登', '録'], char_ptr, 112, 1, NodeType::CharNode4);
    assert_eq!(
        registry
            .select_for_eviction(usize::MAX, &LruRegistry::new(), 0, usize::MAX, 0)
            .len(),
        2
    );
    assert_eq!(
        registry
            .select_char_for_eviction(usize::MAX, &LruRegistry::new(), 0, usize::MAX, 0)
            .len(),
        2
    );

    let retained_byte = registry
        .get_owned(byte_hash)
        .expect("owned byte survives registry lifetime");
    let retained_char = registry
        .get_char_owned(char_hash)
        .expect("owned char survives registry lifetime");
    drop(registry);
    assert_eq!(retained_byte.path, b"registered");
    assert_eq!(retained_char.path, vec!['登', '録']);
}
