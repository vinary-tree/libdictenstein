//! Allocation laws for persistent overlay dictionary-node traversal.
//!
//! This integration test is its own executable, so its counting global
//! allocator cannot instrument unrelated library tests.  Setup and assertions
//! run with measurement disabled; only the named public traversal operation is
//! observed. The laws guard the point/edge hot path against accidental
//! per-handle metadata, cursor state, or wrapper allocation.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::char::{
    PersistentARTrieChar, PersistentARTrieCharNode, SharedCharARTrie,
};
use libdictenstein::persistent_artrie::u64::PersistentARTrieU64Node;
use libdictenstein::persistent_artrie::vocab::VocabTrieNodeRef;
use libdictenstein::persistent_artrie::{PersistentARTrie, PersistentARTrieNode, SharedARTrie};
use libdictenstein::{Dictionary, DictionaryNode};
use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

struct CountingAllocator;

static MEASURING: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() && MEASURING.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() && MEASURING.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        pointer
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let replacement = unsafe { System.realloc(pointer, layout, new_size) };
        if !replacement.is_null() && MEASURING.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        replacement
    }
}

fn measure_allocations<T>(operation: impl FnOnce() -> T) -> (T, usize) {
    assert!(!MEASURING.swap(true, Ordering::SeqCst));
    ALLOCATIONS.store(0, Ordering::Relaxed);
    let result = operation();
    MEASURING.store(false, Ordering::SeqCst);
    (result, ALLOCATIONS.load(Ordering::Relaxed))
}

fn byte_dictionary(labels: &[u8]) -> PersistentARTrie<u64> {
    let dictionary = PersistentARTrie::default();
    for (index, &label) in labels.iter().enumerate() {
        let term = String::from_utf8(vec![label]).expect("test labels are ASCII");
        assert!(dictionary.insert_with_value(&term, index as u64));
    }
    dictionary
}

fn char_dictionary(labels: &[char]) -> PersistentARTrieChar<u64> {
    let dictionary = PersistentARTrieChar::default();
    for (index, &label) in labels.iter().enumerate() {
        assert!(dictionary
            .insert_with_value(&label.to_string(), index as u64)
            .expect("insert Unicode test label"));
    }
    dictionary
}

fn assert_byte_hot_path_allocations() {
    let labels = b"abcdefghijklmnop";
    let dictionary = byte_dictionary(labels);

    let (root, root_allocations) = measure_allocations(|| dictionary.root());
    assert_eq!(
        root_allocations, 0,
        "capturing a resident byte root must not allocate"
    );

    let (child, transition_allocations) =
        measure_allocations(|| root.transition(black_box(b'a')).expect("byte child"));
    assert_eq!(
        transition_allocations, 0,
        "resident byte transition must not allocate"
    );
    black_box(child);

    let (visited, for_each_allocations) = measure_allocations(|| {
        let mut visited = 0usize;
        root.for_each_edge(|label, child| {
            black_box(label);
            black_box(child);
            visited += 1;
        });
        visited
    });
    assert_eq!(visited, labels.len());
    assert_eq!(
        for_each_allocations, 0,
        "borrowed byte edge visits must not allocate"
    );

    let (accepted, filter_map_allocations) = measure_allocations(|| {
        let mut accepted = 0usize;
        root.filter_map_edges(Some, |label, child, projected| {
            black_box((label, child, projected));
            accepted += 1;
        });
        accepted
    });
    assert_eq!(accepted, labels.len());
    assert_eq!(
        filter_map_allocations, 0,
        "accepted byte projections must not allocate"
    );

    let single = byte_dictionary(b"a");
    let single_root = single.root();
    let (_, single_edges_allocations) =
        measure_allocations(|| black_box(single_root.edges().count()));
    let (_, many_edges_allocations) = measure_allocations(|| black_box(root.edges().count()));
    assert_eq!(
        many_edges_allocations, single_edges_allocations,
        "byte edges allocation count must be constant in fanout"
    );
}

fn assert_char_hot_path_allocations() {
    let labels = [
        'a', 'β', 'γ', 'δ', 'λ', 'π', 'σ', 'φ', '雪', '樹', '火', '水', '🦀', '🧠', '🛠', '🚀',
    ];
    let dictionary = char_dictionary(&labels);

    let (root, root_allocations) = measure_allocations(|| dictionary.root());
    assert_eq!(
        root_allocations, 0,
        "capturing a resident char root must not allocate"
    );

    let (child, transition_allocations) =
        measure_allocations(|| root.transition(black_box('雪')).expect("char child"));
    assert_eq!(
        transition_allocations, 0,
        "resident char transition must not allocate"
    );
    black_box(child);

    let (visited, for_each_allocations) = measure_allocations(|| {
        let mut visited = 0usize;
        root.for_each_edge(|label, child| {
            black_box(label);
            black_box(child);
            visited += 1;
        });
        visited
    });
    assert_eq!(visited, labels.len());
    assert_eq!(
        for_each_allocations, 0,
        "borrowed char edge visits must not allocate"
    );

    let (accepted, filter_map_allocations) = measure_allocations(|| {
        let mut accepted = 0usize;
        root.filter_map_edges(Some, |label, child, projected| {
            black_box((label, child, projected));
            accepted += 1;
        });
        accepted
    });
    assert_eq!(accepted, labels.len());
    assert_eq!(
        filter_map_allocations, 0,
        "accepted char projections must not allocate"
    );

    let single = char_dictionary(&['雪']);
    let single_root = single.root();
    let (_, single_edges_allocations) =
        measure_allocations(|| black_box(single_root.edges().count()));
    let (_, many_edges_allocations) = measure_allocations(|| black_box(root.edges().count()));
    assert_eq!(
        many_edges_allocations, single_edges_allocations,
        "char edges allocation count must be constant in fanout"
    );
}

fn assert_eviction_capable_root_and_descendant_allocations() {
    let byte: SharedARTrie<u64> = Arc::new(byte_dictionary(b"abc"));
    let (byte_root, byte_root_allocations) = measure_allocations(|| Dictionary::root(&byte));
    assert_eq!(
        byte_root_allocations, 0,
        "an eviction-capable byte root must reuse its existing Arc faulter"
    );
    let (byte_child, byte_transition_allocations) =
        measure_allocations(|| byte_root.transition(black_box(b'a')).expect("byte child"));
    assert_eq!(
        byte_transition_allocations, 0,
        "an eviction-capable byte descendant must clone its faulter without allocation"
    );
    black_box(byte_child);

    let unicode: SharedCharARTrie<u64> = Arc::new(char_dictionary(&['a', 'β', '雪']));
    let (unicode_root, unicode_root_allocations) =
        measure_allocations(|| Dictionary::root(&unicode));
    assert_eq!(
        unicode_root_allocations, 1,
        "an eviction-capable char root allocates only its established concrete faulter"
    );
    let (unicode_child, unicode_transition_allocations) = measure_allocations(|| {
        unicode_root
            .transition(black_box('雪'))
            .expect("Unicode child")
    });
    assert_eq!(
        unicode_transition_allocations, 0,
        "an eviction-capable char descendant must clone its faulter without allocation"
    );
    black_box(unicode_child);
}

#[test]
fn persistent_overlay_legacy_traversal_has_no_per_node_snapshot_memo_allocation() {
    // The original three-word layout is one overlay Arc plus the data and
    // vtable words of the optional erased faulter. Ordinary node handles carry
    // no cursor memo or side metadata.
    assert_eq!(
        std::mem::size_of::<PersistentARTrieNode<u64>>(),
        3 * std::mem::size_of::<usize>()
    );
    assert_eq!(
        std::mem::size_of::<PersistentARTrieCharNode<u64>>(),
        3 * std::mem::size_of::<usize>()
    );
    assert_eq!(
        std::mem::size_of::<PersistentARTrieU64Node<u64>>(),
        3 * std::mem::size_of::<usize>()
    );
    assert_eq!(
        std::mem::size_of::<VocabTrieNodeRef>(),
        3 * std::mem::size_of::<usize>()
    );

    assert_byte_hot_path_allocations();
    assert_char_hot_path_allocations();
    assert_eviction_capable_root_and_descendant_allocations();
}
