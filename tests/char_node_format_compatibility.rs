#![cfg(feature = "persistent-artrie")]

#[path = "support/char_node_format_cases.rs"]
mod char_node_format_cases;

const BASELINE_CORPUS: &str = include_str!("fixtures/char-node-format/baseline-v2-6a1b267.txt");
const CURRENT_CORPUS: &str = include_str!("fixtures/char-node-format/current-writer.txt");

#[test]
fn current_writer_matches_the_immutable_corpus_byte_for_byte() {
    let parsed =
        char_node_format_cases::parse_corpus(CURRENT_CORPUS).expect("parse current corpus");
    assert_eq!(
        char_node_format_cases::emit_corpus("current", parsed.source),
        CURRENT_CORPUS
    );
}

#[test]
fn current_reader_accepts_every_baseline_and_current_record() {
    char_node_format_cases::verify_corpus(BASELINE_CORPUS, 3).expect("read baseline corpus");
    char_node_format_cases::verify_corpus(CURRENT_CORPUS, 3).expect("read current corpus");
}

#[test]
fn release_matrix_contains_every_kind_mode_and_expected_wire_version() {
    let baseline =
        char_node_format_cases::parse_corpus(BASELINE_CORPUS).expect("parse baseline corpus");
    let current =
        char_node_format_cases::parse_corpus(CURRENT_CORPUS).expect("parse current corpus");
    assert_eq!(baseline.writer, "baseline");
    assert_eq!(current.writer, "current");
    assert!(baseline.records.iter().all(|record| record.version == 2));
    for record in &current.records {
        let expected = if record.name.ends_with(".fixed") {
            2
        } else {
            3
        };
        assert_eq!(record.version, expected, "{}", record.name);
    }
}
