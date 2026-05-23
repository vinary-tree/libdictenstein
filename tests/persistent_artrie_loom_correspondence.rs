//! Bounded schedule checks for the byte lock-free ARTrie publication protocol.
//!
//! These are deliberately small loom models. They mirror the root-slot CAS,
//! cache publication, child handoff, and merge snapshot rules documented in
//! `LockFreeARTrieLinearizability.tla` without changing the production
//! implementation to depend on loom primitives.

#![cfg(feature = "persistent-artrie")]

use loom::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use loom::sync::{Arc, RwLock};
use loom::thread;

#[derive(Debug)]
struct ModelNode {
    contains_key: bool,
}

impl ModelNode {
    fn empty() -> Self {
        Self {
            contains_key: false,
        }
    }

    fn with_key() -> Self {
        Self { contains_key: true }
    }
}

#[derive(Debug)]
struct ModelRootSlot {
    ptr: RwLock<Option<Arc<ModelNode>>>,
}

impl ModelRootSlot {
    fn new(node: Arc<ModelNode>) -> Self {
        Self {
            ptr: RwLock::new(Some(node)),
        }
    }

    fn load(&self) -> Option<Arc<ModelNode>> {
        self.ptr.read().expect("root read lock").clone()
    }

    fn compare_exchange(
        &self,
        expected: &Arc<ModelNode>,
        new: Arc<ModelNode>,
    ) -> Result<Arc<ModelNode>, Arc<ModelNode>> {
        let mut guard = self.ptr.write().expect("root write lock");

        match guard.as_ref() {
            Some(current) if Arc::ptr_eq(current, expected) => {
                let old = Arc::clone(current);
                *guard = Some(new);
                Ok(old)
            }
            Some(current) => Err(Arc::clone(current)),
            None => Err(Arc::new(ModelNode::empty())),
        }
    }
}

fn insert_one_key(root: &ModelRootSlot, cache: &AtomicBool) -> bool {
    if cache.load(Ordering::Acquire) {
        return false;
    }

    loop {
        let current = root.load().expect("model root is initialized");

        if current.contains_key {
            cache.store(true, Ordering::Release);
            return false;
        }

        let new_root = Arc::new(ModelNode::with_key());
        match root.compare_exchange(&current, new_root) {
            Ok(_) => {
                cache.store(true, Ordering::Release);
                return true;
            }
            Err(_) => thread::yield_now(),
        }
    }
}

fn contains_one_key(root: &ModelRootSlot, cache: &AtomicBool) -> bool {
    cache.load(Ordering::Acquire) || root.load().expect("model root is initialized").contains_key
}

fn merge_once(cache: &AtomicBool, persisted: &AtomicBool) {
    if cache.load(Ordering::Acquire) {
        persisted.store(true, Ordering::Release);
    }
}

#[test]
fn atomic_root_compare_exchange_has_single_winner() {
    loom::model(|| {
        let root = Arc::new(ModelRootSlot::new(Arc::new(ModelNode::empty())));
        let expected = root.load().expect("initial root");
        let winners = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for _ in 0..2 {
            let root = Arc::clone(&root);
            let expected = Arc::clone(&expected);
            let winners = Arc::clone(&winners);

            handles.push(thread::spawn(move || {
                let candidate = Arc::new(ModelNode::with_key());
                if root.compare_exchange(&expected, candidate).is_ok() {
                    winners.fetch_add(1, Ordering::SeqCst);
                }
            }));
        }

        for handle in handles {
            handle.join().expect("thread completed");
        }

        assert_eq!(winners.load(Ordering::SeqCst), 1);
        assert!(root.load().expect("final root").contains_key);
    });
}

#[test]
fn duplicate_insert_linearizes_at_single_root_publish() {
    loom::model(|| {
        let root = Arc::new(ModelRootSlot::new(Arc::new(ModelNode::empty())));
        let cache = Arc::new(AtomicBool::new(false));
        let successful_inserts = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for _ in 0..2 {
            let root = Arc::clone(&root);
            let cache = Arc::clone(&cache);
            let successful_inserts = Arc::clone(&successful_inserts);

            handles.push(thread::spawn(move || {
                if insert_one_key(&root, &cache) {
                    successful_inserts.fetch_add(1, Ordering::SeqCst);
                }
            }));
        }

        for handle in handles {
            handle.join().expect("thread completed");
        }

        assert_eq!(successful_inserts.load(Ordering::SeqCst), 1);
        assert!(cache.load(Ordering::Acquire));
        assert!(contains_one_key(&root, &cache));
    });
}

#[test]
fn contains_after_insert_join_observes_published_key() {
    loom::model(|| {
        let root = Arc::new(ModelRootSlot::new(Arc::new(ModelNode::empty())));
        let cache = Arc::new(AtomicBool::new(false));
        let observed_visible = Arc::new(AtomicBool::new(false));

        let inserter = {
            let root = Arc::clone(&root);
            let cache = Arc::clone(&cache);
            thread::spawn(move || {
                insert_one_key(&root, &cache);
            })
        };

        let observer = {
            let root = Arc::clone(&root);
            let cache = Arc::clone(&cache);
            let observed_visible = Arc::clone(&observed_visible);
            thread::spawn(move || {
                if contains_one_key(&root, &cache) {
                    observed_visible.store(true, Ordering::Release);
                }
            })
        };

        inserter.join().expect("insert completed");
        observer.join().expect("observer completed");

        assert!(contains_one_key(&root, &cache));
        assert!(
            observed_visible.load(Ordering::Acquire) || contains_one_key(&root, &cache),
            "a concurrent contains may linearize before or after the insert"
        );
    });
}

#[test]
fn merge_snapshot_is_prefix_of_cache_publication() {
    loom::model(|| {
        let root = Arc::new(ModelRootSlot::new(Arc::new(ModelNode::empty())));
        let cache = Arc::new(AtomicBool::new(false));
        let persisted = Arc::new(AtomicBool::new(false));

        let inserter = {
            let root = Arc::clone(&root);
            let cache = Arc::clone(&cache);
            thread::spawn(move || {
                insert_one_key(&root, &cache);
            })
        };

        let merger = {
            let cache = Arc::clone(&cache);
            let persisted = Arc::clone(&persisted);
            thread::spawn(move || {
                merge_once(&cache, &persisted);
            })
        };

        inserter.join().expect("insert completed");
        merger.join().expect("merge completed");

        assert!(
            !persisted.load(Ordering::Acquire) || cache.load(Ordering::Acquire),
            "merge cannot persist a key before cache publication"
        );

        merge_once(&cache, &persisted);
        assert!(persisted.load(Ordering::Acquire));
        assert!(contains_one_key(&root, &cache));
    });
}

#[derive(Debug)]
struct DropTracked {
    drops: Arc<AtomicUsize>,
}

impl Drop for DropTracked {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Debug)]
struct ChildSlot {
    child: RwLock<Option<Arc<DropTracked>>>,
}

impl ChildSlot {
    fn new(child: Arc<DropTracked>) -> Self {
        Self {
            child: RwLock::new(Some(child)),
        }
    }

    fn load_child(&self) -> Option<Arc<DropTracked>> {
        self.child.read().expect("child read lock").clone()
    }
}

#[test]
fn child_pointer_handoff_keeps_arc_alive_for_readers() {
    loom::model(|| {
        let drops = Arc::new(AtomicUsize::new(0));
        let child = Arc::new(DropTracked {
            drops: Arc::clone(&drops),
        });
        let slot = Arc::new(ChildSlot::new(child));

        let mut handles = Vec::new();
        for _ in 0..2 {
            let slot = Arc::clone(&slot);
            handles.push(thread::spawn(move || {
                let child = slot.load_child().expect("child present");
                thread::yield_now();
                drop(child);
            }));
        }

        for handle in handles {
            handle.join().expect("reader completed");
        }

        assert_eq!(drops.load(Ordering::SeqCst), 0);
        drop(slot);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    });
}
