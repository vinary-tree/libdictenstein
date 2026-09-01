use std::fmt::Write as _;
use std::io::Cursor;

use libdictenstein::persistent_artrie::char::arena_manager::ArenaSlot;
use libdictenstein::persistent_artrie::char::nodes::{
    CharArtNode, CharBucket, CharCompressedPrefix, CharNode, CharNode16, CharNode4, CharNode48,
};
use libdictenstein::persistent_artrie::char::relative_encoding::SerializationContext;
use libdictenstein::persistent_artrie::char::serialization_char::{
    deserialize_char_node_v2, serialize_char_node_v2, DeserializationContext, CHAR_FORMAT_VERSION,
};
use libdictenstein::persistent_artrie::{NodeType, PersistentARTrieError, SwizzledPtr};

const FORMAT_HEADER: &str = "char-node-format-corpus-v1";
const PARENT: ArenaSlot = ArenaSlot {
    arena_id: 1,
    slot_id: 2_000,
};
const SEQUENTIAL_FIRST: ArenaSlot = ArenaSlot {
    arena_id: 1,
    slot_id: 1_000,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeKind {
    N4,
    N16,
    N48,
    Bucket,
}

impl NodeKind {
    const ALL: [Self; 4] = [Self::N4, Self::N16, Self::N48, Self::Bucket];

    fn name(self) -> &'static str {
        match self {
            Self::N4 => "node4",
            Self::N16 => "node16",
            Self::N48 => "node48",
            Self::Bucket => "bucket",
        }
    }

    fn child_count(self) -> usize {
        match self {
            Self::N4 => 4,
            Self::N16 => 12,
            Self::N48 => 24,
            Self::Bucket => 49,
        }
    }

    fn key_base(self) -> u32 {
        match self {
            Self::N4 => 0x0100,
            Self::N16 => 0x1000,
            Self::N48 => 0x2000,
            Self::Bucket => 0x3000,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EncodingMode {
    Fixed,
    Relative,
    Sequential,
}

impl EncodingMode {
    const ALL: [Self; 3] = [Self::Fixed, Self::Relative, Self::Sequential];

    fn name(self) -> &'static str {
        match self {
            Self::Fixed => "fixed",
            Self::Relative => "relative",
            Self::Sequential => "sequential",
        }
    }

    fn parse(name: &str) -> Result<Self, String> {
        match name {
            "fixed" => Ok(Self::Fixed),
            "relative" => Ok(Self::Relative),
            "sequential" => Ok(Self::Sequential),
            _ => Err(format!("unknown corpus encoding mode {name:?}")),
        }
    }
}

#[derive(Debug)]
pub struct Corpus<'a> {
    pub writer: &'a str,
    pub source: &'a str,
    pub records: Vec<CorpusRecord>,
}

#[derive(Debug)]
pub struct CorpusRecord {
    pub name: String,
    pub version: u8,
    pub parent: ArenaSlot,
    pub bytes: Vec<u8>,
}

fn child_node_type(index: usize, kind: NodeKind, mode: EncodingMode) -> NodeType {
    if mode == EncodingMode::Sequential && kind != NodeKind::N4 {
        return NodeType::CharNode16;
    }
    [
        NodeType::CharNode4,
        NodeType::CharNode16,
        NodeType::CharNode48,
        NodeType::CharBucket,
    ][index % 4]
}

fn child_slot(index: usize, mode: EncodingMode) -> ArenaSlot {
    match mode {
        EncodingMode::Fixed => {
            if index.is_multiple_of(2) {
                ArenaSlot::new(PARENT.arena_id, 300 + index as u32)
            } else {
                ArenaSlot::new(3, 700 + index as u32)
            }
        }
        EncodingMode::Relative => {
            if index.is_multiple_of(3) {
                ArenaSlot::new(3, 700 + index as u32)
            } else {
                ArenaSlot::new(PARENT.arena_id, 300 + index as u32)
            }
        }
        EncodingMode::Sequential => ArenaSlot::new(
            SEQUENTIAL_FIRST.arena_id,
            SEQUENTIAL_FIRST.slot_id + index as u32,
        ),
    }
}

fn disk_ptr(slot: ArenaSlot, node_type: NodeType) -> SwizzledPtr {
    SwizzledPtr::on_disk(slot.arena_id + 1, slot.slot_id, node_type)
}

fn configure_node(node: &mut CharNode) {
    node.header_mut().set_final(true);
    node.header_mut().prefix_len = 2;
    *node.prefix_mut() = CharCompressedPrefix::from_chars(&['λ' as u32, 'δ' as u32]);
    let value = disk_ptr(ArenaSlot::new(4, 77), NodeType::CharBucket);
    match node {
        CharNode::N4(node) => node.value_ptr = value,
        CharNode::N16(node) => node.value_ptr = value,
        CharNode::N48(node) => node.value_ptr = value,
        CharNode::Bucket(node) => node.value_ptr = value,
    }
}

fn add_child(node: &mut CharNode, key: u32, child: SwizzledPtr) {
    match node {
        CharNode::N4(node) => node.add_child(key, child),
        CharNode::N16(node) => node.add_child(key, child),
        CharNode::N48(node) => node.add_child(key, child),
        CharNode::Bucket(node) => node.add_child(key, child),
    }
    .expect("corpus child must fit its selected ART representation");
}

fn build_case(kind: NodeKind, mode: EncodingMode) -> (CharNode, SerializationContext) {
    let mut node = match kind {
        NodeKind::N4 => CharNode::N4(Box::new(CharNode4::new())),
        NodeKind::N16 => CharNode::N16(Box::new(CharNode16::new())),
        NodeKind::N48 => CharNode::N48(Box::new(CharNode48::new())),
        NodeKind::Bucket => CharNode::Bucket(Box::new(CharBucket::new())),
    };
    configure_node(&mut node);

    for index in 0..kind.child_count() {
        add_child(
            &mut node,
            kind.key_base() + index as u32,
            disk_ptr(child_slot(index, mode), child_node_type(index, kind, mode)),
        );
    }

    let context = match mode {
        EncodingMode::Fixed => SerializationContext {
            parent_slot: PARENT,
            use_relative: false,
            use_sequential: false,
            first_child_slot: None,
        },
        EncodingMode::Relative => SerializationContext::new(PARENT),
        EncodingMode::Sequential => SerializationContext::sequential(PARENT, SEQUENTIAL_FIRST),
    };
    (node, context)
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut encoded, "{byte:02x}").expect("String writes are infallible");
    }
    encoded
}

fn hex_decode(encoded: &str) -> Result<Vec<u8>, String> {
    if !encoded.len().is_multiple_of(2) {
        return Err("corpus hexadecimal payload has odd length".to_string());
    }
    encoded
        .as_bytes()
        .chunks_exact(2)
        .enumerate()
        .map(|(index, pair)| {
            let text = std::str::from_utf8(pair)
                .map_err(|error| format!("non-UTF-8 hex pair {index}: {error}"))?;
            u8::from_str_radix(text, 16)
                .map_err(|error| format!("invalid hex pair {text:?} at {index}: {error}"))
        })
        .collect()
}

pub fn emit_corpus(writer: &str, source: &str) -> String {
    let mut output = format!("{FORMAT_HEADER}\nwriter={writer}\nsource={source}\n");
    for kind in NodeKind::ALL {
        for mode in EncodingMode::ALL {
            let (node, context) = build_case(kind, mode);
            let mut bytes = Vec::new();
            serialize_char_node_v2(&node, &mut bytes, &context)
                .expect("qualifying corpus serialization must succeed");
            writeln!(
                &mut output,
                "{}.{}\t{}\t{}\t{}\t{}\t{}\t-",
                kind.name(),
                mode.name(),
                bytes[4],
                PARENT.arena_id,
                PARENT.slot_id,
                bytes.len(),
                hex_encode(&bytes)
            )
            .expect("String writes are infallible");
        }
    }
    output
}

pub fn parse_corpus(input: &str) -> Result<Corpus<'_>, String> {
    let mut lines = input.lines();
    if lines.next() != Some(FORMAT_HEADER) {
        return Err("unknown character-node corpus format".to_string());
    }
    let writer = lines
        .next()
        .and_then(|line| line.strip_prefix("writer="))
        .ok_or_else(|| "corpus writer header is missing".to_string())?;
    let source = lines
        .next()
        .and_then(|line| line.strip_prefix("source="))
        .ok_or_else(|| "corpus source header is missing".to_string())?;
    let mut records = Vec::new();
    for (line_index, line) in lines.enumerate() {
        if line.is_empty() {
            continue;
        }
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() != 7 {
            return Err(format!(
                "corpus record {} has {} fields instead of 7",
                line_index + 4,
                fields.len()
            ));
        }
        let version = fields[1]
            .parse::<u8>()
            .map_err(|error| format!("invalid version in {}: {error}", fields[0]))?;
        let arena_id = fields[2]
            .parse::<u32>()
            .map_err(|error| format!("invalid parent arena in {}: {error}", fields[0]))?;
        let slot_id = fields[3]
            .parse::<u32>()
            .map_err(|error| format!("invalid parent slot in {}: {error}", fields[0]))?;
        let declared_len = fields[4]
            .parse::<usize>()
            .map_err(|error| format!("invalid byte length in {}: {error}", fields[0]))?;
        let bytes = hex_decode(fields[5])?;
        if fields[6] != "-" {
            return Err(format!(
                "record {} has a noncanonical trailing field",
                fields[0]
            ));
        }
        if bytes.len() != declared_len {
            return Err(format!(
                "record {} declares {declared_len} bytes but contains {}",
                fields[0],
                bytes.len()
            ));
        }
        if bytes.get(4).copied() != Some(version) {
            return Err(format!(
                "record {} version column disagrees with its wire header",
                fields[0]
            ));
        }
        records.push(CorpusRecord {
            name: fields[0].to_string(),
            version,
            parent: ArenaSlot::new(arena_id, slot_id),
            bytes,
        });
    }
    if records.len() != NodeKind::ALL.len() * EncodingMode::ALL.len() {
        return Err(format!(
            "corpus contains {} records instead of 12",
            records.len()
        ));
    }
    Ok(Corpus {
        writer,
        source,
        records,
    })
}

fn parse_case_name(name: &str) -> Result<(NodeKind, EncodingMode), String> {
    let (kind, mode) = name
        .split_once('.')
        .ok_or_else(|| format!("invalid corpus case name {name:?}"))?;
    let kind = match kind {
        "node4" => NodeKind::N4,
        "node16" => NodeKind::N16,
        "node48" => NodeKind::N48,
        "bucket" => NodeKind::Bucket,
        _ => return Err(format!("unknown corpus node kind {kind:?}")),
    };
    Ok((kind, EncodingMode::parse(mode)?))
}

fn child_signature(node: &CharNode) -> Vec<(u32, u32, u32, NodeType)> {
    let mut children: Vec<_> = node
        .iter_children()
        .map(|(key, child)| {
            let location = child
                .disk_location()
                .expect("corpus children must remain on-disk references");
            (key, location.block_id, location.offset, location.node_type)
        })
        .collect();
    children.sort_by_key(|entry| entry.0);
    children
}

fn assert_decoded_semantics(
    writer: &str,
    kind: NodeKind,
    mode: EncodingMode,
    expected: &CharNode,
    actual: &CharNode,
) -> Result<(), String> {
    if expected.header().node_type != actual.header().node_type
        || expected.header().num_children != actual.header().num_children
        || expected.header().prefix_len != actual.header().prefix_len
        || expected
            .prefix()
            .as_slice(expected.header().prefix_len as usize)
            != actual
                .prefix()
                .as_slice(actual.header().prefix_len as usize)
    {
        return Err(format!(
            "{}.{} changed structural node semantics",
            kind.name(),
            mode.name()
        ));
    }

    let expected_children = child_signature(expected);
    let actual_children = child_signature(actual);
    let legacy_type_erasure = writer == "baseline" && mode != EncodingMode::Fixed;
    for (expected_child, actual_child) in expected_children.iter().zip(&actual_children) {
        if expected_child.0 != actual_child.0
            || expected_child.1 != actual_child.1
            || expected_child.2 != actual_child.2
        {
            return Err(format!(
                "{}.{} changed a child key or location",
                kind.name(),
                mode.name()
            ));
        }
        let expected_type = if legacy_type_erasure {
            NodeType::CharNode4
        } else {
            expected_child.3
        };
        if actual_child.3 != expected_type {
            return Err(format!(
                "{}.{} child {} has type {:?}, expected {:?}",
                kind.name(),
                mode.name(),
                actual_child.0,
                actual_child.3,
                expected_type
            ));
        }
    }
    if expected_children.len() != actual_children.len() {
        return Err(format!(
            "{}.{} changed child cardinality",
            kind.name(),
            mode.name()
        ));
    }
    Ok(())
}

pub fn verify_corpus(input: &str, expected_reader_max: u8) -> Result<(), String> {
    if CHAR_FORMAT_VERSION != expected_reader_max {
        return Err(format!(
            "probe compiled with reader max {}, caller expected {expected_reader_max}",
            CHAR_FORMAT_VERSION
        ));
    }
    let corpus = parse_corpus(input)?;
    if corpus.source.is_empty() {
        return Err("corpus source identity is empty".to_string());
    }
    for record in &corpus.records {
        let (kind, mode) = parse_case_name(&record.name)?;
        let (expected, _) = build_case(kind, mode);
        let decoded = deserialize_char_node_v2(
            &mut Cursor::new(&record.bytes),
            &DeserializationContext::new(record.parent),
        );
        if record.version > expected_reader_max {
            match decoded {
                Err(PersistentARTrieError::UnsupportedVersion {
                    max_supported,
                    found,
                }) if max_supported == u32::from(expected_reader_max)
                    && found == u32::from(record.version) => {}
                other => {
                    return Err(format!(
                        "{} returned {other:?}; expected UnsupportedVersion({}, {})",
                        record.name, expected_reader_max, record.version
                    ))
                }
            }
        } else {
            let decoded =
                decoded.map_err(|error| format!("{} failed to decode: {error}", record.name))?;
            assert_decoded_semantics(corpus.writer, kind, mode, &expected, &decoded)?;
        }
    }
    Ok(())
}
