#[path = "../tests/support/char_node_format_cases.rs"]
mod char_node_format_cases;

use std::fs;

fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("emit") => {
            let writer = args.next().expect("emit requires a writer name");
            let source = args.next().expect("emit requires a source identity");
            assert!(args.next().is_none(), "emit received unexpected arguments");
            print!("{}", char_node_format_cases::emit_corpus(&writer, &source));
        }
        Some("verify") => {
            let path = args.next().expect("verify requires a corpus path");
            let reader_max = args
                .next()
                .expect("verify requires a reader maximum version")
                .parse::<u8>()
                .expect("reader maximum version must be a u8");
            assert!(
                args.next().is_none(),
                "verify received unexpected arguments"
            );
            let corpus = fs::read_to_string(path).expect("read corpus");
            char_node_format_cases::verify_corpus(&corpus, reader_max)
                .expect("corpus compatibility verification");
        }
        _ => panic!("usage: char_node_format_probe emit WRITER SOURCE | verify PATH READER_MAX"),
    }
}
