use std::io::Cursor;

use libdictenstein::persistent_artrie::arena_manager::ArenaSlot as ByteArenaSlot;
use libdictenstein::persistent_artrie::error::PersistentARTrieError;
use libdictenstein::persistent_artrie::relative_encoding as byte_rel;
use libdictenstein::persistent_artrie_char::arena_manager::ArenaSlot as CharArenaSlot;
use libdictenstein::persistent_artrie_char::relative_encoding as char_rel;
use libdictenstein::persistent_artrie_char::serialization_char::{
    char_node_types, deserialize_char_node_v2, DeserializationContext, SerializedCharNodeHeader,
    CHAR_FORMAT_VERSION, CHAR_NODE_MAGIC,
};

#[test]
fn byte_relative_encoding_falls_back_to_full_for_forward_same_arena_child() {
    let parent = ByteArenaSlot::new(3, 10);
    let child = ByteArenaSlot::new(3, 20);
    let mut buf = Vec::new();

    let written = byte_rel::encode_child_pointer(parent, child, &mut buf);

    assert_eq!(written, byte_rel::CROSS_ARENA_SIZE);
    assert_eq!(
        byte_rel::encoded_size(parent, child),
        byte_rel::CROSS_ARENA_SIZE
    );
    assert!(matches!(
        byte_rel::try_encode_child_pointer(parent, child, &mut Vec::new()),
        Err(byte_rel::RelativeEncodingError::InvalidRelativeDirection { .. })
    ));

    let (decoded, consumed) = byte_rel::try_decode_child_pointer(&buf, parent).unwrap();
    assert_eq!(decoded, child);
    assert_eq!(consumed, byte_rel::CROSS_ARENA_SIZE);
}

#[test]
fn char_relative_encoding_falls_back_to_full_for_forward_same_arena_child() {
    let parent = CharArenaSlot::new(7, 10);
    let child = CharArenaSlot::new(7, 20);
    let mut buf = Vec::new();

    let written = char_rel::encode_child_pointer(parent, child, &mut buf);

    assert_eq!(written, char_rel::CROSS_ARENA_SIZE);
    assert_eq!(
        char_rel::encoded_size(parent, child),
        char_rel::CROSS_ARENA_SIZE
    );
    assert!(matches!(
        char_rel::try_encode_child_pointer(parent, child, &mut Vec::new()),
        Err(char_rel::RelativeEncodingError::InvalidRelativeDirection { .. })
    ));

    let (decoded, consumed) = char_rel::try_decode_child_pointer(&buf, parent).unwrap();
    assert_eq!(decoded, child);
    assert_eq!(consumed, char_rel::CROSS_ARENA_SIZE);
}

#[test]
fn checked_decode_rejects_empty_truncated_odd_and_underflow_inputs() {
    let parent = CharArenaSlot::new(0, 10);

    assert!(matches!(
        char_rel::try_decode_child_pointer(&[], parent),
        Err(char_rel::RelativeEncodingError::EmptyInput)
    ));
    assert!(matches!(
        char_rel::try_decode_child_pointer(&[char_rel::FLAG_CROSS_ARENA, 0, 0], parent),
        Err(char_rel::RelativeEncodingError::TruncatedFullPointer { .. })
    ));
    assert!(matches!(
        char_rel::try_decode_child_pointer(&[251, 1], parent),
        Err(char_rel::RelativeEncodingError::TruncatedVarint { .. })
    ));
    assert!(matches!(
        char_rel::try_decode_child_pointer(&[3], parent),
        Err(char_rel::RelativeEncodingError::OddRelativeTag { .. })
    ));
    assert!(matches!(
        char_rel::try_decode_child_pointer(&[22], parent),
        Err(char_rel::RelativeEncodingError::RelativeUnderflow { .. })
    ));
}

#[test]
fn checked_children_decode_rejects_truncated_concatenated_stream() {
    let parent = ByteArenaSlot::new(1, 100);
    let children = [ByteArenaSlot::new(1, 90), ByteArenaSlot::new(2, 1)];
    let mut buf = Vec::new();
    byte_rel::encode_children(parent, &children, &mut buf);
    buf.pop();

    assert!(matches!(
        byte_rel::try_decode_children(&buf, parent, children.len()),
        Err(byte_rel::RelativeEncodingError::TruncatedFullPointer { .. })
    ));
}

#[test]
fn checked_sequential_decode_rejects_slot_overflow() {
    let parent = CharArenaSlot::new(0, 0);
    let first_child = CharArenaSlot::new(3, u32::MAX);
    let mut buf = Vec::new();
    char_rel::encode_full(first_child, &mut buf);

    assert!(matches!(
        char_rel::try_decode_sequential_siblings(&buf, parent, 2),
        Err(char_rel::RelativeEncodingError::SequentialOverflow { .. })
    ));

    let (empty, consumed) = char_rel::try_decode_sequential_siblings(&[], parent, 0).unwrap();
    assert!(empty.is_empty());
    assert_eq!(consumed, 0);
}

#[test]
fn char_v2_deserialization_returns_corruption_for_truncated_relative_children() {
    let header = SerializedCharNodeHeader {
        magic: CHAR_NODE_MAGIC,
        version: CHAR_FORMAT_VERSION,
        node_type: char_node_types::CHARNODE4,
        flags: char_rel::FLAG_RELATIVE_OFFSETS,
        reserved: 0,
        num_children: 1,
        prefix_len: 0,
        _padding: 0,
        data_size: 19,
    };

    let mut bytes = Vec::new();
    bytes.extend_from_slice(&header.to_bytes());
    bytes.extend_from_slice(&[0u8; 16]);
    bytes.extend_from_slice(&[char_rel::FLAG_CROSS_ARENA, 0, 0]);

    let mut reader = Cursor::new(bytes);
    let ctx = DeserializationContext::new(CharArenaSlot::new(0, 10));
    let result = deserialize_char_node_v2(&mut reader, &ctx);

    assert!(matches!(
        result,
        Err(PersistentARTrieError::CorruptedFile { .. })
    ));
}
