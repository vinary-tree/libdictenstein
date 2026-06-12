#![cfg(feature = "persistent-artrie")]

use std::io::Cursor;
use std::panic;

use libdictenstein::persistent_artrie::char::arena_manager::ArenaSlot;
use libdictenstein::persistent_artrie::char::nodes::{
    CharArtNode, CharBucket, CharCompressedPrefix, CharNode, CharNode16, CharNode4, CharNode48,
};
use libdictenstein::persistent_artrie::char::relative_encoding::{
    SerializationContext, FLAG_RELATIVE_OFFSETS, FLAG_SEQUENTIAL_SIBLINGS,
};
use libdictenstein::persistent_artrie::char::serialization_char::{
    char_node_types, deserialize_char_node_v2, serialize_char_node_v2, DeserializationContext,
    CHAR_FORMAT_VERSION, CHAR_NODE_MAGIC, CHAR_SERIALIZED_HEADER_SIZE,
};
use libdictenstein::persistent_artrie::{NodeType, SwizzledPtr};

fn disk_ptr(slot: ArenaSlot) -> SwizzledPtr {
    SwizzledPtr::on_disk(slot.arena_id + 1, slot.slot_id, NodeType::CharNode4)
}

fn disk_slot(ptr: &SwizzledPtr) -> Option<(u32, u32)> {
    ptr.disk_location()
        .map(|loc| (loc.block_id.saturating_sub(1), loc.offset))
}

fn child_signature(node: &CharNode) -> Vec<(u32, Option<(u32, u32)>)> {
    let mut entries: Vec<_> = node
        .iter_children()
        .map(|(key, child)| (key, disk_slot(child)))
        .collect();
    entries.sort_by_key(|(key, _)| *key);
    entries
}

fn value_signature(node: &CharNode) -> Option<(u32, u32)> {
    match node {
        CharNode::N4(node) => disk_slot(&node.value_ptr),
        CharNode::N16(node) => disk_slot(&node.value_ptr),
        CharNode::N48(node) => disk_slot(&node.value_ptr),
        CharNode::Bucket(node) => disk_slot(&node.value_ptr),
    }
}

fn assert_same_logical_node(expected: &CharNode, actual: &CharNode) {
    assert_eq!(expected.header().node_type, actual.header().node_type);
    assert_eq!(expected.header().num_children, actual.header().num_children);
    assert_eq!(expected.header().prefix_len, actual.header().prefix_len);
    assert_eq!(
        expected.header().flags & 0x3f,
        actual.header().flags & 0x3f,
        "runtime flags must not retain v2 encoding bits"
    );
    assert_eq!(
        expected
            .prefix()
            .as_slice(expected.header().prefix_len as usize),
        actual
            .prefix()
            .as_slice(actual.header().prefix_len as usize)
    );
    assert_eq!(child_signature(expected), child_signature(actual));
    assert_eq!(value_signature(expected), value_signature(actual));
}

fn decode_v2(
    bytes: &[u8],
    parent: ArenaSlot,
) -> libdictenstein::persistent_artrie::Result<CharNode> {
    let mut cursor = Cursor::new(bytes);
    deserialize_char_node_v2(&mut cursor, &DeserializationContext::new(parent))
}

fn serialize_v2(node: &CharNode, ctx: &SerializationContext) -> Vec<u8> {
    let mut bytes = Vec::new();
    serialize_char_node_v2(node, &mut bytes, ctx).expect("serialize v2 char node");
    bytes
}

fn make_node4() -> CharNode {
    let mut node = CharNode4::new();
    node.header.set_final(true);
    node.prefix = CharCompressedPrefix::from_chars(&['l' as u32, 'i' as u32]);
    node.header.prefix_len = 2;
    node.value_ptr = disk_ptr(ArenaSlot::new(3, 9));
    node.add_child('a' as u32, disk_ptr(ArenaSlot::new(0, 10)))
        .expect("add a");
    node.add_child('z' as u32, disk_ptr(ArenaSlot::new(1, 2)))
        .expect("add z");
    CharNode::N4(Box::new(node))
}

fn make_node16() -> CharNode {
    let mut node = CharNode16::new();
    node.header.set_final(true);
    node.value_ptr = disk_ptr(ArenaSlot::new(4, 7));
    for idx in 0..5 {
        node.add_child(0x03b1 + idx, disk_ptr(ArenaSlot::new(0, 10 + idx)))
            .expect("add child");
    }
    CharNode::N16(Box::new(node))
}

fn make_node48_sequential(first_slot: u32) -> CharNode {
    let mut node = CharNode48::new();
    for idx in 0..17 {
        node.add_child(0x1000 + idx, disk_ptr(ArenaSlot::new(0, first_slot + idx)))
            .expect("add child");
    }
    CharNode::N48(Box::new(node))
}

fn make_bucket() -> CharNode {
    let mut bucket = CharBucket::new();
    bucket.header.set_final(true);
    bucket.value_ptr = disk_ptr(ArenaSlot::new(5, 12));
    for idx in 0..49 {
        bucket
            .add_child(0x2000 + idx, disk_ptr(ArenaSlot::new(2, 20 + idx)))
            .expect("add bucket child");
    }
    CharNode::Bucket(Box::new(bucket))
}

#[test]
fn valid_v2_layouts_roundtrip_exactly() {
    let relative_parent = ArenaSlot::new(0, 80);
    let node4 = make_node4();
    let node16 = make_node16();
    let bucket = make_bucket();
    let sequential_first = ArenaSlot::new(0, 30);
    let sequential_parent = ArenaSlot::new(0, 80);
    let node48 = make_node48_sequential(sequential_first.slot_id);

    let cases = vec![
        (
            node4,
            SerializationContext::new(relative_parent),
            relative_parent,
        ),
        (
            node16,
            SerializationContext::new(relative_parent),
            relative_parent,
        ),
        (
            bucket,
            SerializationContext::new(ArenaSlot::new(2, 90)),
            ArenaSlot::new(2, 90),
        ),
        (
            node48,
            SerializationContext::sequential(sequential_parent, sequential_first),
            sequential_parent,
        ),
    ];

    for (node, ctx, parent) in cases {
        let bytes = serialize_v2(&node, &ctx);
        let decoded = decode_v2(&bytes, parent).expect("decode valid v2 node");
        assert_same_logical_node(&node, &decoded);
    }
}

#[test]
fn valid_v2_decoder_stops_at_exact_node_boundary() {
    let parent = ArenaSlot::new(0, 80);
    let node = make_node4();
    let node_bytes = serialize_v2(&node, &SerializationContext::new(parent));
    let mut with_suffix = node_bytes.clone();
    with_suffix.extend_from_slice(&[0xa5; 21]);

    let mut cursor = Cursor::new(with_suffix);
    let decoded =
        deserialize_char_node_v2(&mut cursor, &DeserializationContext::new(parent)).unwrap();

    assert_same_logical_node(&node, &decoded);
    assert_eq!(
        cursor.position() as usize,
        node_bytes.len(),
        "vocab-specific suffix bytes must remain outside the char-node layout"
    );
}

#[test]
fn valid_v2_layouts_reject_every_truncated_prefix_without_panicking() {
    let parent = ArenaSlot::new(0, 80);
    let node = make_node16();
    let bytes = serialize_v2(&node, &SerializationContext::new(parent));

    for cut in 0..bytes.len() {
        let truncated = bytes[..cut].to_vec();
        let attempt = panic::catch_unwind(move || decode_v2(&truncated, parent));
        assert!(attempt.is_ok(), "truncated input panicked at cut {cut}");
        assert!(
            attempt.unwrap().is_err(),
            "truncated input decoded successfully at cut {cut}"
        );
    }
}

fn mutate_header(mut bytes: Vec<u8>, edit: impl FnOnce(&mut [u8])) -> Vec<u8> {
    edit(&mut bytes[..CHAR_SERIALIZED_HEADER_SIZE]);
    bytes
}

fn set_u16(header: &mut [u8], offset: usize, value: u16) {
    header[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn set_u32(header: &mut [u8], offset: usize, value: u32) {
    header[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

#[test]
fn corrupt_header_fields_fail_closed() {
    let parent = ArenaSlot::new(0, 80);
    let valid = serialize_v2(&make_node4(), &SerializationContext::new(parent));
    let data_size = u32::from_le_bytes(valid[12..16].try_into().unwrap());

    let corruptions = vec![
        mutate_header(valid.clone(), |h| h[0..4].copy_from_slice(b"BAD\0")),
        mutate_header(valid.clone(), |h| h[4] = CHAR_FORMAT_VERSION + 1),
        mutate_header(valid.clone(), |h| h[5] = 0xff),
        mutate_header(valid.clone(), |h| h[6] = FLAG_SEQUENTIAL_SIBLINGS),
        mutate_header(valid.clone(), |h| h[7] = 1),
        mutate_header(valid.clone(), |h| set_u16(h, 8, 5)),
        mutate_header(valid.clone(), |h| h[10] = 7),
        mutate_header(valid.clone(), |h| h[11] = 1),
        mutate_header(valid.clone(), |h| set_u32(h, 12, data_size - 1)),
    ];

    for corrupted in corruptions {
        assert!(decode_v2(&corrupted, parent).is_err());
    }

    let mut too_large = mutate_header(valid, |h| set_u32(h, 12, data_size + 1));
    too_large.push(0);
    assert!(decode_v2(&too_large, parent).is_err());

    assert_eq!(&CHAR_NODE_MAGIC, b"ARC\0");
    assert_eq!(char_node_types::CHARNODE4, 104);
}

#[test]
fn bucket_header_entry_count_mismatch_fails_closed() {
    let parent = ArenaSlot::new(2, 90);
    let valid = serialize_v2(&make_bucket(), &SerializationContext::new(parent));
    let corrupted = mutate_header(valid, |h| set_u16(h, 8, 48));

    assert!(decode_v2(&corrupted, parent).is_err());
}

#[test]
fn sequential_serialization_rejects_missing_empty_and_noncontiguous_children() {
    let parent = ArenaSlot::new(0, 80);
    let mut sink = Vec::new();
    let node = make_node4();

    let missing_first = SerializationContext {
        parent_slot: parent,
        use_relative: true,
        use_sequential: true,
        first_child_slot: None,
    };
    assert!(serialize_char_node_v2(&node, &mut sink, &missing_first).is_err());

    let empty_node = CharNode::N4(Box::new(CharNode4::new()));
    let empty_seq = SerializationContext::sequential(parent, ArenaSlot::new(0, 1));
    assert!(serialize_char_node_v2(&empty_node, &mut Vec::new(), &empty_seq).is_err());

    let noncontiguous = make_node4();
    let bad_seq = SerializationContext::sequential(parent, ArenaSlot::new(0, 10));
    assert!(serialize_char_node_v2(&noncontiguous, &mut Vec::new(), &bad_seq).is_err());
}

#[test]
fn sequential_flag_without_relative_flag_fails_closed_even_with_valid_payload() {
    let first = ArenaSlot::new(0, 30);
    let parent = ArenaSlot::new(0, 80);
    let node = make_node48_sequential(first.slot_id);
    let valid = serialize_v2(&node, &SerializationContext::sequential(parent, first));
    let corrupted = mutate_header(valid, |h| {
        h[6] = (h[6] | FLAG_SEQUENTIAL_SIBLINGS) & !FLAG_RELATIVE_OFFSETS;
    });

    assert!(decode_v2(&corrupted, parent).is_err());
}
