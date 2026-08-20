//! Allocation census for native dictionary collection traversal.
//!
//! This is deliberately separate from Criterion timing: allocator
//! instrumentation changes the hot path and therefore explains allocation
//! behavior rather than supplying latency results.
//!
//! ```text
//! cargo run --release --example collection_allocation_profile
//! ```

use libdictenstein::collection::DictionaryEntries;
use libdictenstein::dynamic_dawg::DynamicDawg;
use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct MeasuringAllocator;

static MEASURING: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static DEALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static ALLOCATED_BYTES: AtomicUsize = AtomicUsize::new(0);
static CURRENT_BYTES: AtomicUsize = AtomicUsize::new(0);
static PEAK_BYTES: AtomicUsize = AtomicUsize::new(0);

#[global_allocator]
static ALLOCATOR: MeasuringAllocator = MeasuringAllocator;

fn update_peak(current: usize) {
    let mut observed = PEAK_BYTES.load(Ordering::Relaxed);
    while current > observed {
        match PEAK_BYTES.compare_exchange_weak(
            observed,
            current,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return,
            Err(actual) => observed = actual,
        }
    }
}

unsafe impl GlobalAlloc for MeasuringAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() && MEASURING.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            ALLOCATED_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
            let current = CURRENT_BYTES
                .fetch_add(layout.size(), Ordering::Relaxed)
                .saturating_add(layout.size());
            update_peak(current);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        if MEASURING.load(Ordering::Relaxed) {
            DEALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            let _ = CURRENT_BYTES.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                Some(current.saturating_sub(layout.size()))
            });
        }
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, old: Layout, new_size: usize) -> *mut u8 {
        let replacement = unsafe { System.realloc(pointer, old, new_size) };
        if !replacement.is_null() && MEASURING.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            DEALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            ALLOCATED_BYTES.fetch_add(new_size, Ordering::Relaxed);
            let current = CURRENT_BYTES
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                    Some(current.saturating_sub(old.size()).saturating_add(new_size))
                })
                .unwrap_or_default()
                .saturating_sub(old.size())
                .saturating_add(new_size);
            update_peak(current);
        }
        replacement
    }
}

#[derive(Clone, Copy)]
struct Census {
    allocations: usize,
    deallocations: usize,
    allocated_bytes: usize,
    peak_bytes: usize,
}

fn measure(operation: &str, entries: usize, body: impl FnOnce()) -> Census {
    ALLOCATIONS.store(0, Ordering::Relaxed);
    DEALLOCATIONS.store(0, Ordering::Relaxed);
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    CURRENT_BYTES.store(0, Ordering::Relaxed);
    PEAK_BYTES.store(0, Ordering::Relaxed);
    MEASURING.store(true, Ordering::SeqCst);
    body();
    MEASURING.store(false, Ordering::SeqCst);

    let census = Census {
        allocations: ALLOCATIONS.load(Ordering::Relaxed),
        deallocations: DEALLOCATIONS.load(Ordering::Relaxed),
        allocated_bytes: ALLOCATED_BYTES.load(Ordering::Relaxed),
        peak_bytes: PEAK_BYTES.load(Ordering::Relaxed),
    };
    println!(
        "{operation},{entries},{},{},{},{}",
        census.allocations, census.deallocations, census.allocated_bytes, census.peak_bytes
    );
    census
}

fn corpus(size: usize) -> DynamicDawg<u64> {
    (0..size)
        .map(|index| {
            (
                format!("allocation/{index:08x}/shared-suffix").into_bytes(),
                index as u64,
            )
        })
        .collect()
}

fn main() {
    println!("operation,entries,allocations,deallocations,allocated_bytes,peak_bytes");
    for size in [4_096, 65_536] {
        let dictionary = corpus(size);

        let owned = measure("owned_entries", size, || {
            let checksum = dictionary
                .entries()
                .map(|entry| entry.key.len() ^ entry.value.unwrap_or_default() as usize)
                .fold(0, usize::wrapping_add);
            black_box(checksum);
        });
        let visitor = measure("borrowed_visitor", size, || {
            let mut checksum = 0usize;
            dictionary.entries().visit(|key, value| {
                checksum = checksum.wrapping_add(key.len() ^ value.unwrap_or_default() as usize);
            });
            black_box(checksum);
        });
        let materialized = measure("materialized_vec", size, || {
            let entries: Vec<_> = dictionary.entries().collect();
            black_box(&entries);
            drop(entries);
        });

        assert!(
            visitor.allocations < owned.allocations,
            "the reusable visitor must allocate less often than owned entry iteration"
        );
        assert!(
            visitor.allocated_bytes < owned.allocated_bytes,
            "the reusable visitor must allocate fewer bytes than owned entry iteration"
        );
        assert!(
            materialized.peak_bytes >= owned.peak_bytes,
            "materializing the complete snapshot must retain at least the owned iterator's peak"
        );
    }
}
