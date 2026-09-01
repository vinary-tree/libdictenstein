//! Bounded schedule checks for exact-root eviction authority.
//!
//! Semantic writers and exact fault/eviction commits use the root compare-and-
//! swap as their linearization point; neither enters the registry lifecycle
//! lock. The lock is confined to cold checkpoint publication, detached-catalog
//! replacement, and coordinator retirement. Detached compatibility callbacks
//! retain immutable advisory snapshots and may overlap every exact-root action.
//! TLA+ covers the larger state graph and Rocq proves unbounded transition
//! preservation.

#![cfg(feature = "persistent-artrie")]

use loom::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use loom::sync::{mpsc, Arc, Mutex};
use loom::thread;

const ROOT_INITIAL_UNBOUND: usize = 0;
const ROOT_BOUND_GENERATION_ONE: usize = 1;
const ROOT_EXACT_SUCCESSOR: usize = 2;
const ROOT_SEMANTIC_UNBOUND: usize = 3;
const ROOT_RETIREMENT_UNBOUND: usize = 4;

const REGISTRY_EMPTY: usize = 0;
const REGISTRY_PUBLISHING: usize = 1;
const REGISTRY_VALID_GENERATION_ONE: usize = 2;
const REGISTRY_INVALID: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Authority {
    Invalid,
    Valid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ExactBatch {
    generation: usize,
    expected_root: usize,
}

#[derive(Debug)]
struct DetachedCatalog {
    generation: usize,
}

#[derive(Debug)]
struct PublicationModel {
    lifecycle: Mutex<()>,
    root: AtomicUsize,
    registry: AtomicUsize,
    retired: AtomicBool,
    detached: Mutex<Option<Arc<DetachedCatalog>>>,
    semantic_commits: AtomicUsize,
    exact_commits: AtomicUsize,
}

impl PublicationModel {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            lifecycle: Mutex::new(()),
            root: AtomicUsize::new(ROOT_INITIAL_UNBOUND),
            registry: AtomicUsize::new(REGISTRY_EMPTY),
            retired: AtomicBool::new(false),
            detached: Mutex::new(None),
            semantic_commits: AtomicUsize::new(0),
            exact_commits: AtomicUsize::new(0),
        })
    }

    fn captured_root(&self) -> usize {
        self.root.load(Ordering::SeqCst)
    }

    fn publish_checkpoint(&self, expected_root: usize) -> bool {
        let _lifecycle = self.lifecycle.lock().expect("checkpoint lifecycle lock");
        if self.retired.load(Ordering::SeqCst) {
            return false;
        }
        self.registry.store(REGISTRY_PUBLISHING, Ordering::SeqCst);
        if self
            .root
            .compare_exchange(
                expected_root,
                ROOT_BOUND_GENERATION_ONE,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_err()
        {
            self.registry.store(REGISTRY_INVALID, Ordering::SeqCst);
            return false;
        }
        thread::yield_now();
        if self.root.load(Ordering::SeqCst) == ROOT_BOUND_GENERATION_ONE {
            self.registry
                .store(REGISTRY_VALID_GENERATION_ONE, Ordering::SeqCst);
            true
        } else {
            self.registry.store(REGISTRY_INVALID, Ordering::SeqCst);
            false
        }
    }

    fn select_exact_batch(&self) -> Option<ExactBatch> {
        let expected_root = self.root.load(Ordering::SeqCst);
        if !Self::is_generation_one_root(expected_root)
            || self.registry.load(Ordering::SeqCst) != REGISTRY_VALID_GENERATION_ONE
        {
            return None;
        }
        Some(ExactBatch {
            generation: 1,
            expected_root,
        })
    }

    fn commit_exact_batch(&self, batch: ExactBatch) -> bool {
        assert_eq!(batch.generation, 1);
        if self
            .root
            .compare_exchange(
                batch.expected_root,
                ROOT_EXACT_SUCCESSOR,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_err()
        {
            return false;
        }
        self.exact_commits.fetch_add(1, Ordering::SeqCst);
        true
    }

    fn publish_semantic_successor(&self) {
        let mut observed = self.root.load(Ordering::SeqCst);
        loop {
            match self.root.compare_exchange(
                observed,
                ROOT_SEMANTIC_UNBOUND,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => {
                    self.semantic_commits.fetch_add(1, Ordering::SeqCst);
                    return;
                }
                Err(actual) => observed = actual,
            }
        }
    }

    fn retire(&self) {
        let _lifecycle = self.lifecycle.lock().expect("retirement lifecycle lock");
        self.retired.store(true, Ordering::SeqCst);
        let mut observed = self.root.load(Ordering::SeqCst);
        while observed != ROOT_RETIREMENT_UNBOUND {
            match self.root.compare_exchange(
                observed,
                ROOT_RETIREMENT_UNBOUND,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => break,
                Err(actual) => observed = actual,
            }
        }
        self.registry.store(REGISTRY_INVALID, Ordering::SeqCst);
        self.detached
            .lock()
            .expect("retirement detached slot lock")
            .take();
    }

    fn install_detached(&self, generation: usize) -> bool {
        let _lifecycle = self.lifecycle.lock().expect("install lifecycle lock");
        if self.retired.load(Ordering::SeqCst) {
            return false;
        }
        *self.detached.lock().expect("detached slot lock") =
            Some(Arc::new(DetachedCatalog { generation }));
        true
    }

    fn load_detached(&self) -> Option<Arc<DetachedCatalog>> {
        self.detached.lock().expect("detached slot lock").clone()
    }

    fn exact_authorized(&self) -> bool {
        !self.retired.load(Ordering::SeqCst)
            && Self::is_generation_one_root(self.root.load(Ordering::SeqCst))
            && self.registry.load(Ordering::SeqCst) == REGISTRY_VALID_GENERATION_ONE
    }

    fn is_generation_one_root(root: usize) -> bool {
        matches!(root, ROOT_BOUND_GENERATION_ONE | ROOT_EXACT_SUCCESSOR)
    }
    fn assert_retired_root_is_unbound(&self) {
        if self.retired.load(Ordering::SeqCst) {
            assert!(
                !Self::is_generation_one_root(self.root.load(Ordering::SeqCst)),
                "retired coordinator retained an exact root binding"
            );
        }
    }

    fn unsafe_retire_without_root_fence(&self) {
        let _lifecycle = self.lifecycle.lock().expect("unsafe retirement lock");
        self.retired.store(true, Ordering::SeqCst);
        self.registry.store(REGISTRY_INVALID, Ordering::SeqCst);
    }

    fn unsafe_commit_exact_without_root_cas(&self) {
        self.root.store(ROOT_EXACT_SUCCESSOR, Ordering::SeqCst);
        self.exact_commits.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn checkpoint_and_semantic_writer_are_ordered_only_by_exact_root_cas() {
    loom::model(|| {
        let model = PublicationModel::new();
        let captured_root = model.captured_root();

        let checkpoint_model = Arc::clone(&model);
        let checkpoint = thread::spawn(move || checkpoint_model.publish_checkpoint(captured_root));
        let semantic_model = Arc::clone(&model);
        let semantic = thread::spawn(move || semantic_model.publish_semantic_successor());

        let _checkpoint_won_first = checkpoint.join().expect("checkpoint joins");
        semantic.join().expect("semantic writer joins");
        assert_eq!(model.root.load(Ordering::SeqCst), ROOT_SEMANTIC_UNBOUND);
        assert!(!model.exact_authorized());
        assert_eq!(model.semantic_commits.load(Ordering::SeqCst), 1);
    });
}

#[test]
fn checkpoint_and_retirement_never_leave_an_exact_binding() {
    loom::model(|| {
        let model = PublicationModel::new();
        let captured_root = model.captured_root();
        let checkpoint_model = Arc::clone(&model);
        let checkpoint = thread::spawn(move || checkpoint_model.publish_checkpoint(captured_root));
        let retirement_model = Arc::clone(&model);
        let retirement = thread::spawn(move || retirement_model.retire());

        let _published_before_retirement = checkpoint.join().expect("checkpoint joins");
        retirement.join().expect("retirement joins");
        model.assert_retired_root_is_unbound();
        assert!(!model.exact_authorized());
    });
}

#[test]
fn selected_exact_transition_racing_semantic_successor_fails_closed() {
    loom::model(|| {
        let model = PublicationModel::new();
        assert!(model.publish_checkpoint(ROOT_INITIAL_UNBOUND));
        let batch = model.select_exact_batch().expect("exact batch");

        let exact_model = Arc::clone(&model);
        let exact = thread::spawn(move || exact_model.commit_exact_batch(batch));
        let semantic_model = Arc::clone(&model);
        let semantic = thread::spawn(move || semantic_model.publish_semantic_successor());

        let _exact_won_first = exact.join().expect("exact commit joins");
        semantic.join().expect("semantic writer joins");
        assert_eq!(model.root.load(Ordering::SeqCst), ROOT_SEMANTIC_UNBOUND);
        assert!(!model.exact_authorized());
        assert!(model.exact_commits.load(Ordering::SeqCst) <= 1);
    });
}

#[test]
fn selected_exact_transition_racing_retirement_cannot_rebind_the_root() {
    loom::model(|| {
        let model = PublicationModel::new();
        assert!(model.publish_checkpoint(ROOT_INITIAL_UNBOUND));
        let batch = model.select_exact_batch().expect("exact batch");

        let exact_model = Arc::clone(&model);
        let exact = thread::spawn(move || exact_model.commit_exact_batch(batch));
        let retirement_model = Arc::clone(&model);
        let retirement = thread::spawn(move || retirement_model.retire());

        let _committed_before_fence = exact.join().expect("exact commit joins");
        retirement.join().expect("retirement joins");
        model.assert_retired_root_is_unbound();
        assert!(!model.exact_authorized());
    });
}

#[test]
fn detached_callback_snapshot_overlaps_replacement_and_semantic_publication() {
    loom::model(|| {
        let model = PublicationModel::new();
        assert!(model.install_detached(1));
        let (captured_tx, captured_rx) = mpsc::channel();
        let (replaced_tx, replaced_rx) = mpsc::channel();

        let callback_model = Arc::clone(&model);
        let callback = thread::spawn(move || {
            let snapshot = callback_model
                .load_detached()
                .expect("detached callback snapshot");
            captured_tx.send(()).expect("announce detached capture");
            replaced_rx.recv().expect("observe replacement");
            assert_eq!(snapshot.generation, 1);
        });

        let replacement_model = Arc::clone(&model);
        let replacement = thread::spawn(move || {
            captured_rx.recv().expect("observe detached capture");
            assert!(replacement_model.install_detached(2));
            replacement_model.publish_semantic_successor();
            replaced_tx.send(()).expect("announce replacement");
        });

        callback.join().expect("detached callback joins");
        replacement.join().expect("replacement joins");
        assert_eq!(
            model
                .load_detached()
                .expect("replacement catalog")
                .generation,
            2
        );
        assert!(!model.exact_authorized());
    });
}

#[test]
fn retirement_clears_the_slot_but_not_an_existing_detached_snapshot() {
    loom::model(|| {
        let model = PublicationModel::new();
        assert!(model.install_detached(7));
        let snapshot = model.load_detached().expect("detached snapshot");
        model.retire();
        assert!(model.load_detached().is_none());
        assert_eq!(snapshot.generation, 7);
        assert!(!model.install_detached(8));
    });
}

#[test]
#[should_panic(expected = "retired coordinator retained an exact root binding")]
fn unsafe_retirement_without_an_unbound_root_fence_is_detected() {
    loom::model(|| {
        let model = PublicationModel::new();
        assert!(model.publish_checkpoint(ROOT_INITIAL_UNBOUND));
        model.unsafe_retire_without_root_fence();
        model.assert_retired_root_is_unbound();
    });
}

#[test]
#[should_panic(expected = "unsafe exact commit rebound a semantic successor")]
fn unsafe_exact_commit_without_root_cas_rebinds_a_semantic_successor() {
    loom::model(|| {
        let model = PublicationModel::new();
        assert!(model.publish_checkpoint(ROOT_INITIAL_UNBOUND));
        let _batch = model.select_exact_batch().expect("exact batch");
        model.publish_semantic_successor();
        model.unsafe_commit_exact_without_root_cas();
        assert!(
            !PublicationModel::is_generation_one_root(model.root.load(Ordering::SeqCst)),
            "unsafe exact commit rebound a semantic successor"
        );
    });
}

#[derive(Clone, Copy)]
struct ResidentCapturePlan {
    generation: usize,
    residency_words: usize,
}

#[derive(Clone, Copy)]
struct ResidentSelectionSnapshot {
    generation: usize,
    root_revision: usize,
    copied_resident: bool,
}

enum ResidentCaptureOutcome {
    Ready(ResidentSelectionSnapshot),
    Retry,
    Unavailable,
}

struct ResidentCaptureState {
    authority: Authority,
    generation: usize,
    root_generation: Option<usize>,
    root_revision: usize,
    residency_words: usize,
    resident: bool,
    cleared_generation: Option<usize>,
}

struct ResidentCaptureModel {
    state: Mutex<ResidentCaptureState>,
}

impl ResidentCaptureModel {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(ResidentCaptureState {
                authority: Authority::Valid,
                generation: 1,
                root_generation: Some(1),
                root_revision: 1,
                residency_words: 1,
                resident: true,
                cleared_generation: None,
            }),
        })
    }

    fn capture_plan(&self) -> Option<ResidentCapturePlan> {
        let state = self.state.lock().expect("resident capture plan lock");
        (state.authority == Authority::Valid).then_some(ResidentCapturePlan {
            generation: state.generation,
            residency_words: state.residency_words,
        })
    }

    fn capture(&self, plan: ResidentCapturePlan) -> ResidentCaptureOutcome {
        let state = self.state.lock().expect("resident snapshot lock");
        if state.authority != Authority::Valid {
            return ResidentCaptureOutcome::Unavailable;
        }
        if state.generation != plan.generation || state.residency_words != plan.residency_words {
            return ResidentCaptureOutcome::Retry;
        }
        assert_eq!(state.root_generation, Some(state.generation));
        ResidentCaptureOutcome::Ready(ResidentSelectionSnapshot {
            generation: state.generation,
            root_revision: state.root_revision,
            copied_resident: state.resident,
        })
    }

    fn commit(&self, snapshot: ResidentSelectionSnapshot) -> bool {
        let mut state = self.state.lock().expect("resident commit lock");
        if !snapshot.copied_resident
            || state.authority != Authority::Valid
            || state.generation != snapshot.generation
            || state.root_generation != Some(snapshot.generation)
            || state.root_revision != snapshot.root_revision
            || !state.resident
        {
            return false;
        }
        state.resident = false;
        state.cleared_generation = Some(snapshot.generation);
        state.root_revision += 1;
        true
    }

    fn invalidate(&self) {
        let mut state = self.state.lock().expect("resident invalidate lock");
        state.authority = Authority::Invalid;
        state.root_generation = None;
        state.root_revision += 1;
    }

    fn publish_successor(&self) {
        let mut state = self.state.lock().expect("resident successor lock");
        state.generation += 1;
        state.root_generation = Some(state.generation);
        state.root_revision += 1;
        state.residency_words += 1;
        state.resident = true;
        state.cleared_generation = None;
        state.authority = Authority::Valid;
    }

    fn unsafe_commit_without_revalidation(&self, snapshot: ResidentSelectionSnapshot) {
        if !snapshot.copied_resident {
            return;
        }
        let mut state = self.state.lock().expect("unsafe resident commit lock");
        state.resident = false;
        state.cleared_generation = Some(snapshot.generation);
        state.root_revision += 1;
    }

    fn assert_snapshot_safety(&self) {
        let state = self.state.lock().expect("resident safety lock");
        if !state.resident {
            assert_eq!(
                state.cleared_generation,
                Some(state.generation),
                "stale resident selection cleared a successor generation"
            );
        }
    }
}

#[test]
fn resident_selection_capture_and_commit_fail_closed_across_generation_and_authority_change() {
    let mut builder = loom::model::Builder::new();
    builder.preemption_bound = Some(3);
    builder.check(|| {
        let model = ResidentCaptureModel::new();
        let selector_model = Arc::clone(&model);
        let selector = thread::spawn(move || {
            let Some(plan) = selector_model.capture_plan() else {
                return;
            };
            thread::yield_now();
            if let ResidentCaptureOutcome::Ready(snapshot) = selector_model.capture(plan) {
                thread::yield_now();
                let _ = selector_model.commit(snapshot);
            }
        });
        let publisher_model = Arc::clone(&model);
        let publisher = thread::spawn(move || {
            thread::yield_now();
            publisher_model.invalidate();
            thread::yield_now();
            publisher_model.publish_successor();
        });

        selector.join().expect("resident selector joins");
        publisher.join().expect("resident publisher joins");
        model.assert_snapshot_safety();
    });
}

#[test]
#[should_panic(expected = "stale resident selection cleared a successor generation")]
fn unsafe_resident_commit_without_generation_revalidation_clears_successor() {
    loom::model(|| {
        let model = ResidentCaptureModel::new();
        let plan = model.capture_plan().expect("unsafe capture plan");
        let snapshot = match model.capture(plan) {
            ResidentCaptureOutcome::Ready(snapshot) => snapshot,
            ResidentCaptureOutcome::Retry => panic!("stable unsafe capture retried"),
            ResidentCaptureOutcome::Unavailable => panic!("stable unsafe capture unavailable"),
        };
        model.invalidate();
        model.publish_successor();
        model.unsafe_commit_without_revalidation(snapshot);
        model.assert_snapshot_safety();
    });
}
