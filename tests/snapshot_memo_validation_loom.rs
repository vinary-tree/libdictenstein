//! Memory-model correspondence for the heterogeneous `SnapshotMemo` writer
//! handshake in `bindings.rs`.
//!
//! The backend atomic stands for publication of an immutable root. Acquiring
//! that root carries the writer's preceding active-count announcement into the
//! reader. Final validation must then load active writers before revision.

use loom::sync::atomic::{AtomicUsize, Ordering};
use loom::sync::Arc;
use loom::thread;

#[derive(Clone, Copy)]
enum ValidationOrder {
    ActiveThenRevision,
    RevisionThenActive,
}

fn model_validation(order: ValidationOrder) {
    loom::model(move || {
        let active = Arc::new(AtomicUsize::new(0));
        let revision = Arc::new(AtomicUsize::new(0));
        let backend = Arc::new(AtomicUsize::new(0));

        let writer_active = Arc::clone(&active);
        let writer_revision = Arc::clone(&revision);
        let writer_backend = Arc::clone(&backend);
        let writer = thread::spawn(move || {
            writer_active.fetch_add(1, Ordering::AcqRel);
            // Represents the Dynamic/Persistent immutable-root publication.
            writer_backend.store(1, Ordering::Release);
            writer_revision.fetch_add(1, Ordering::AcqRel);
            writer_active.fetch_sub(1, Ordering::AcqRel);
        });

        let reader_active = Arc::clone(&active);
        let reader_revision = Arc::clone(&revision);
        let reader_backend = Arc::clone(&backend);
        let reader = thread::spawn(move || {
            let expected_revision = reader_revision.load(Ordering::Acquire);
            if reader_active.load(Ordering::Acquire) != 0 {
                return;
            }
            let captured_backend_revision = reader_backend.load(Ordering::Acquire);

            let (final_active, final_revision) = match order {
                ValidationOrder::ActiveThenRevision => (
                    reader_active.load(Ordering::Acquire),
                    reader_revision.load(Ordering::Acquire),
                ),
                ValidationOrder::RevisionThenActive => {
                    let revision = reader_revision.load(Ordering::Acquire);
                    let active = reader_active.load(Ordering::Acquire);
                    (active, revision)
                }
            };
            if final_active == 0 && final_revision == expected_revision {
                assert_eq!(
                    captured_backend_revision, expected_revision,
                    "accepted snapshot identity must describe the captured backend root"
                );
            }
        });

        writer.join().unwrap();
        reader.join().unwrap();
    });
}

#[test]
fn active_then_revision_rejects_every_torn_capture() {
    model_validation(ValidationOrder::ActiveThenRevision);
}

#[test]
#[should_panic(expected = "accepted snapshot identity must describe the captured backend root")]
fn revision_then_active_has_a_torn_capture_witness() {
    model_validation(ValidationOrder::RevisionThenActive);
}

#[test]
fn active_then_revision_checks_one_overlapping_two_writer_completion_order() {
    let mut builder = loom::model::Builder::new();
    builder.preemption_bound = Some(2);
    builder.check(|| {
        let active = Arc::new(AtomicUsize::new(0));
        let revision = Arc::new(AtomicUsize::new(0));
        let backend_bits = Arc::new(AtomicUsize::new(0));

        // Collapse two logical writers into one scheduling actor while
        // retaining an admissible overlapping order: both admit, writer one
        // publishes and withdraws 2 -> 1, then writer two publishes and makes
        // the final 1 -> 0 withdrawal. This deliberately checks the AcqRel
        // completion-chain handoff without claiming exhaustive exploration of
        // every three-thread writer permutation.
        let writer_active = Arc::clone(&active);
        let writer_revision = Arc::clone(&revision);
        let writer_backend = Arc::clone(&backend_bits);
        let writers = thread::spawn(move || {
            writer_active.fetch_add(1, Ordering::AcqRel);
            writer_active.fetch_add(1, Ordering::AcqRel);

            writer_backend.fetch_or(1, Ordering::AcqRel);
            writer_revision.fetch_add(1, Ordering::AcqRel);
            writer_active.fetch_sub(1, Ordering::AcqRel);

            writer_backend.fetch_or(2, Ordering::AcqRel);
            writer_revision.fetch_add(1, Ordering::AcqRel);
            // The 1 -> 0 RMW acquires writer one's withdrawal and releases the
            // combined completion chain to the snapshotter.
            writer_active.fetch_sub(1, Ordering::AcqRel);
        });

        let reader_active = Arc::clone(&active);
        let reader_revision = Arc::clone(&revision);
        let reader_backend = Arc::clone(&backend_bits);
        let reader = thread::spawn(move || {
            let expected_revision = reader_revision.load(Ordering::Acquire);
            if reader_active.load(Ordering::Acquire) != 0 {
                return;
            }
            let captured_bits = reader_backend.load(Ordering::Acquire);
            let final_active = reader_active.load(Ordering::Acquire);
            let final_revision = reader_revision.load(Ordering::Acquire);
            if final_active == 0 && final_revision == expected_revision {
                assert_eq!(
                    captured_bits.count_ones() as usize,
                    expected_revision,
                    "accepted identity counts every backend publication it captures"
                );
            }
        });

        writers.join().unwrap();
        reader.join().unwrap();
    });
}
