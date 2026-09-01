//! Exact native-cursor paging laws for every DynamicDAWG label domain.

use libdictenstein::dynamic_dawg::{DynamicDawg, DynamicDawgChar, DynamicDawgU64};
use libdictenstein::{Dictionary, DictionaryNode};

fn collect_pages<N>(root: &N, page_capacity: usize) -> (bool, usize, Vec<N::Unit>)
where
    N: DictionaryNode,
{
    assert!(root.supports_efficient_snapshot_cursor_edge_paging());
    let cursor = root
        .snapshot_root_cursor()
        .expect("DynamicDAWG exposes a native root cursor");
    let mut labels = Vec::new();
    let (expected_finality, total) = unsafe {
        root.visit_snapshot_cursor_edge_page(cursor, 0, 0, |_, _| {
            panic!("a zero-capacity page must not visit an edge")
        })
        .expect("advertised cursor pager is available")
    };
    let mut start = 0;
    while start < total {
        let before = labels.len();
        let (is_final, confirmed_total) = unsafe {
            root.visit_snapshot_cursor_edge_page(cursor, start, page_capacity, |label, _| {
                labels.push(label)
            })
            .expect("advertised cursor pager remains available")
        };
        assert_eq!(is_final, expected_finality);
        assert_eq!(confirmed_total, total);
        let visited = labels.len() - before;
        assert!(visited > 0 && visited <= page_capacity);
        start += visited;
    }
    (expected_finality, total, labels)
}

#[test]
fn byte_cursor_pages_are_exact_sorted_and_capacity_bounded() {
    let dictionary: DynamicDawg<()> = (0_u8..=63)
        .map(|suffix| vec![suffix])
        .collect::<Vec<_>>()
        .into_iter()
        .collect();
    let root = dictionary.root();
    for page_capacity in [1, 2, 7, 8, 17, 64] {
        let (is_final, total, labels) = collect_pages(&root, page_capacity);
        assert!(!is_final);
        assert_eq!(total, 64);
        assert_eq!(labels, (0_u8..=63).collect::<Vec<_>>());
    }
}

#[test]
fn unicode_cursor_pages_are_exact_sorted_and_capacity_bounded() {
    let symbols = ['a', 'b', 'c', 'd', 'λ', '界', '🙂'];
    let dictionary: DynamicDawgChar<()> = symbols
        .iter()
        .map(|symbol| symbol.to_string())
        .collect::<Vec<_>>()
        .into_iter()
        .collect();
    let root = dictionary.root();
    for page_capacity in [1, 3, 8] {
        let (is_final, total, labels) = collect_pages(&root, page_capacity);
        assert!(!is_final);
        assert_eq!(total, symbols.len());
        let mut expected = symbols.to_vec();
        expected.sort_unstable();
        assert_eq!(labels, expected);
    }
}

#[test]
fn u64_cursor_pages_are_exact_sorted_and_capacity_bounded() {
    let labels = [0_u64, 1, 2, 7, u32::MAX as u64, u64::MAX - 1, u64::MAX];
    let dictionary: DynamicDawgU64<()> = labels
        .iter()
        .map(|label| vec![*label])
        .collect::<Vec<_>>()
        .into_iter()
        .collect();
    let root = dictionary.root();
    for page_capacity in [1, 2, 8] {
        let (is_final, total, observed) = collect_pages(&root, page_capacity);
        assert!(!is_final);
        assert_eq!(total, labels.len());
        assert_eq!(observed, labels);
    }
}
