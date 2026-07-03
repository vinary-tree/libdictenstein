use libdictenstein::bijective::BijectiveMap;
use libdictenstein::dynamic_dawg::{DynamicDawg, DynamicDawgChar};
use libdictenstein::scdawg::{Scdawg, ScdawgChar};
use libdictenstein::suffix_automaton::{SuffixAutomaton, SuffixAutomatonChar};
use libdictenstein::{Dictionary, MutableDictionary};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

const READERS: usize = 6;
const SEED_TERMS: usize = 512;
const NEW_TERMS: usize = 256;
const READ_ROUNDS: usize = 4;

fn byte_terms(prefix: &str, count: usize) -> Vec<String> {
    (0..count)
        .map(|i| format!("{prefix}_term_{i:04x}_suffix"))
        .collect()
}

fn char_terms(prefix: &str, count: usize) -> Vec<String> {
    let samples = ["東京", "cafe", "résumé", "θήτα", "данные", "مفتاح"];
    (0..count)
        .map(|i| format!("{prefix}-{}-{i:04x}", samples[i % samples.len()]))
        .collect()
}

fn assert_seed_visible<D>(dict: Arc<D>, seed: Arc<Vec<String>>, new_terms: Arc<Vec<String>>)
where
    D: Dictionary + MutableDictionary + Send + Sync + 'static,
{
    let start = Arc::new(Barrier::new(READERS + 1));
    let mut handles = Vec::with_capacity(READERS);

    for _ in 0..READERS {
        let dict = Arc::clone(&dict);
        let seed = Arc::clone(&seed);
        let start = Arc::clone(&start);
        handles.push(thread::spawn(move || {
            start.wait();
            for _ in 0..READ_ROUNDS {
                for term in seed.iter() {
                    assert!(dict.contains(term), "seed term disappeared: {term}");
                }
            }
        }));
    }

    start.wait();
    for term in new_terms.iter() {
        dict.insert(term);
    }

    for handle in handles {
        handle.join().expect("reader thread must finish");
    }

    for term in new_terms.iter() {
        assert!(
            dict.contains(term),
            "published term missing after join: {term}"
        );
    }
}

#[test]
fn dynamic_dawg_readers_do_not_block_or_lose_seed_terms() {
    let seed = Arc::new(byte_terms("seed", SEED_TERMS));
    let new_terms = Arc::new(byte_terms("new", NEW_TERMS));
    let dict = Arc::new(DynamicDawg::<()>::from_terms(seed.iter()));

    assert_seed_visible(dict, seed, new_terms);
}

#[test]
fn dynamic_dawg_char_readers_do_not_block_or_lose_seed_terms() {
    let seed = Arc::new(char_terms("seed", SEED_TERMS));
    let new_terms = Arc::new(char_terms("new", NEW_TERMS));
    let dict = Arc::new(DynamicDawgChar::<()>::from_terms(seed.iter()));

    assert_seed_visible(dict, seed, new_terms);
}

#[test]
fn dynamic_dawg_concurrent_first_update_or_insert_has_one_insert_winner() {
    const WRITERS: usize = 8;
    const INCREMENTS: usize = 64;

    let dict = Arc::new(DynamicDawg::<i64>::new());
    let barrier = Arc::new(Barrier::new(WRITERS));
    let insert_winners = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::with_capacity(WRITERS);

    for _ in 0..WRITERS {
        let dict = Arc::clone(&dict);
        let barrier = Arc::clone(&barrier);
        let insert_winners = Arc::clone(&insert_winners);
        handles.push(thread::spawn(move || {
            barrier.wait();
            for _ in 0..INCREMENTS {
                if dict.update_or_insert("counter", 0, |value| {
                    *value += 1;
                }) {
                    insert_winners.fetch_add(1, Ordering::Relaxed);
                }
            }
        }));
    }

    for handle in handles {
        handle.join().expect("update thread must finish");
    }

    assert_eq!(insert_winners.load(Ordering::Relaxed), 1);
    assert_eq!(
        dict.get_value("counter"),
        Some((WRITERS * INCREMENTS - 1) as i64)
    );
}

#[test]
fn dynamic_dawg_char_concurrent_first_update_or_insert_has_one_insert_winner() {
    const WRITERS: usize = 8;
    const INCREMENTS: usize = 64;

    let dict = Arc::new(DynamicDawgChar::<i64>::new());
    let barrier = Arc::new(Barrier::new(WRITERS));
    let insert_winners = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::with_capacity(WRITERS);

    for _ in 0..WRITERS {
        let dict = Arc::clone(&dict);
        let barrier = Arc::clone(&barrier);
        let insert_winners = Arc::clone(&insert_winners);
        handles.push(thread::spawn(move || {
            barrier.wait();
            for _ in 0..INCREMENTS {
                if dict.update_or_insert("κλειδί", 0, |value| {
                    *value += 1;
                }) {
                    insert_winners.fetch_add(1, Ordering::Relaxed);
                }
            }
        }));
    }

    for handle in handles {
        handle.join().expect("update thread must finish");
    }

    assert_eq!(insert_winners.load(Ordering::Relaxed), 1);
    assert_eq!(
        dict.get_value("κλειδί"),
        Some((WRITERS * INCREMENTS - 1) as i64)
    );
}

#[cfg(feature = "pathmap-backend")]
#[test]
fn pathmap_readers_do_not_block_or_lose_seed_terms() {
    use libdictenstein::pathmap::PathMapDictionary;

    let seed = Arc::new(byte_terms("seed", SEED_TERMS));
    let new_terms = Arc::new(byte_terms("new", NEW_TERMS));
    let dict = Arc::new(PathMapDictionary::<()>::from_terms(seed.iter()));

    let start = Arc::new(Barrier::new(READERS + 1));
    let mut handles = Vec::with_capacity(READERS);

    for _ in 0..READERS {
        let dict = Arc::clone(&dict);
        let seed = Arc::clone(&seed);
        let start = Arc::clone(&start);
        handles.push(thread::spawn(move || {
            start.wait();
            for _ in 0..READ_ROUNDS {
                for term in seed.iter() {
                    assert!(dict.contains(term), "seed term disappeared: {term}");
                }
            }
        }));
    }

    start.wait();
    for term in new_terms.iter() {
        dict.insert_with_value(term, ());
    }

    for handle in handles {
        handle.join().expect("reader thread must finish");
    }
}

#[test]
fn suffix_automaton_snapshot_readers_survive_concurrent_writes() {
    let seed = Arc::new(byte_terms("seed", SEED_TERMS));
    let new_terms = Arc::new(byte_terms("new", NEW_TERMS));
    let dict = Arc::new(SuffixAutomaton::<()>::from_texts(seed.iter()));
    let start = Arc::new(Barrier::new(READERS + 1));
    let mut handles = Vec::with_capacity(READERS);

    for _ in 0..READERS {
        let dict = Arc::clone(&dict);
        let seed = Arc::clone(&seed);
        let start = Arc::clone(&start);
        handles.push(thread::spawn(move || {
            start.wait();
            for _ in 0..READ_ROUNDS {
                for term in seed.iter() {
                    assert!(dict.contains(term), "seed term disappeared: {term}");
                }
            }
        }));
    }

    start.wait();
    for term in new_terms.iter() {
        dict.insert(term);
    }

    for handle in handles {
        handle.join().expect("reader thread must finish");
    }
}

#[test]
fn suffix_automaton_char_snapshot_readers_survive_concurrent_writes() {
    let seed = Arc::new(char_terms("seed", SEED_TERMS));
    let new_terms = Arc::new(char_terms("new", NEW_TERMS));
    let dict = Arc::new(SuffixAutomatonChar::<()>::from_texts(seed.iter()));
    let start = Arc::new(Barrier::new(READERS + 1));
    let mut handles = Vec::with_capacity(READERS);

    for _ in 0..READERS {
        let dict = Arc::clone(&dict);
        let seed = Arc::clone(&seed);
        let start = Arc::clone(&start);
        handles.push(thread::spawn(move || {
            start.wait();
            for _ in 0..READ_ROUNDS {
                for term in seed.iter() {
                    assert!(dict.contains(term), "seed term disappeared: {term}");
                }
            }
        }));
    }

    start.wait();
    for term in new_terms.iter() {
        dict.insert(term);
    }

    for handle in handles {
        handle.join().expect("reader thread must finish");
    }
}

#[test]
fn scdawg_snapshot_readers_survive_concurrent_writes() {
    let seed = Arc::new(byte_terms("seed", SEED_TERMS));
    let new_terms = Arc::new(byte_terms("new", NEW_TERMS));
    let dict = Arc::new(Scdawg::<()>::from_terms(seed.iter()));
    let start = Arc::new(Barrier::new(READERS + 1));
    let mut handles = Vec::with_capacity(READERS);

    for _ in 0..READERS {
        let dict = Arc::clone(&dict);
        let seed = Arc::clone(&seed);
        let start = Arc::clone(&start);
        handles.push(thread::spawn(move || {
            start.wait();
            for _ in 0..READ_ROUNDS {
                for term in seed.iter() {
                    assert!(dict.contains(term), "seed term disappeared: {term}");
                }
            }
        }));
    }

    start.wait();
    for term in new_terms.iter() {
        dict.insert(term);
    }

    for handle in handles {
        handle.join().expect("reader thread must finish");
    }
}

#[test]
fn scdawg_char_snapshot_readers_survive_concurrent_writes() {
    let seed = Arc::new(char_terms("seed", SEED_TERMS));
    let new_terms = Arc::new(char_terms("new", NEW_TERMS));
    let dict = Arc::new(ScdawgChar::<()>::from_terms(seed.iter()));
    let start = Arc::new(Barrier::new(READERS + 1));
    let mut handles = Vec::with_capacity(READERS);

    for _ in 0..READERS {
        let dict = Arc::clone(&dict);
        let seed = Arc::clone(&seed);
        let start = Arc::clone(&start);
        handles.push(thread::spawn(move || {
            start.wait();
            for _ in 0..READ_ROUNDS {
                for term in seed.iter() {
                    assert!(dict.contains(term), "seed term disappeared: {term}");
                }
            }
        }));
    }

    start.wait();
    for term in new_terms.iter() {
        dict.insert(term);
    }

    for handle in handles {
        handle.join().expect("reader thread must finish");
    }
}

#[test]
fn bijective_reverse_lookup_snapshot_survives_concurrent_inserts() {
    let seed = Arc::new(byte_terms("seed", SEED_TERMS));
    let new_terms = Arc::new(byte_terms("new", NEW_TERMS));
    let map = Arc::new(BijectiveMap::<usize>::new());

    for (index, term) in seed.iter().enumerate() {
        map.insert(term, index);
    }

    let start = Arc::new(Barrier::new(READERS + 1));
    let mut handles = Vec::with_capacity(READERS);

    for _ in 0..READERS {
        let map = Arc::clone(&map);
        let seed = Arc::clone(&seed);
        let start = Arc::clone(&start);
        handles.push(thread::spawn(move || {
            start.wait();
            for _ in 0..READ_ROUNDS {
                for (index, term) in seed.iter().enumerate() {
                    assert_eq!(map.get_term(&index), Some(term.clone()));
                }
            }
        }));
    }

    start.wait();
    for (offset, term) in new_terms.iter().enumerate() {
        map.insert(term, SEED_TERMS + offset);
    }

    for handle in handles {
        handle.join().expect("reader thread must finish");
    }
}
