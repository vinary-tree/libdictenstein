//! Direct-Rust versus batched-ABI collection traversal.
//!
//! This isolates snapshot capture, owned-key materialization, the reusable
//! visitor path, host materialization, and foreign-boundary batch amortization.
//! Run on an admitted idle host with:
//!
//! ```text
//! cargo bench --features bindings-core --bench collection_traversal_benchmarks
//! ```

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use libdictenstein::bindings::{BindingUnitDomain, DynamicDawgBinding};
use libdictenstein::collection::DictionaryEntries;
use libdictenstein::dynamic_dawg::DynamicDawg;
use std::ffi::c_void;
use std::hint::black_box;
use std::time::Duration;
use vinary_tree_interop::{
    VtDictionaryEntriesCursor, VtDictionaryEntriesInfo, VtDictionaryEntriesVTable,
    VtDictionaryEntryBatchLimits, VtDictionaryEntryBatchView, VtStatus,
    VT_DICTIONARY_ENTRIES_INTERFACE_ID, VT_DICTIONARY_ENTRIES_INTERFACE_VERSION,
};

const SIZES: &[usize] = &[4_096, 65_536];
const ABI_BATCHES: &[usize] = &[64, 256, 1_024];

fn corpus(size: usize) -> Vec<(Vec<u8>, u64)> {
    (0..size)
        .map(|index| {
            (
                format!(
                    "collection/{:04x}/{:08x}/shared-suffix",
                    index & 0x0fff,
                    index
                )
                .into_bytes(),
                index as u64,
            )
        })
        .collect()
}

fn direct_checksum(dictionary: &DynamicDawg<u64>) -> usize {
    dictionary
        .entries()
        .map(|entry| entry.key.len() ^ entry.value.unwrap_or_default() as usize)
        .fold(0, usize::wrapping_add)
}

fn visitor_checksum(dictionary: &DynamicDawg<u64>) -> usize {
    let mut checksum = 0usize;
    dictionary.entries().visit(|key, value| {
        checksum = checksum.wrapping_add(key.len() ^ value.unwrap_or_default() as usize);
    });
    checksum
}

unsafe fn entry_vtable(
    resource: vinary_tree_interop::VtResource,
) -> *const VtDictionaryEntriesVTable {
    let mut interface: *const c_void = std::ptr::null();
    let status = unsafe {
        ((*resource.vtable)
            .query_interface
            .expect("resource query interface"))(
            resource.context,
            &VT_DICTIONARY_ENTRIES_INTERFACE_ID,
            VT_DICTIONARY_ENTRIES_INTERFACE_VERSION,
            &mut interface,
        )
    };
    assert_eq!(VtStatus::from_raw(status), Some(VtStatus::Ok));
    assert!(!interface.is_null());
    interface.cast()
}

fn abi_checksum(dictionary: &DynamicDawgBinding, batch_size: usize) -> usize {
    abi_checksum_limited(dictionary, batch_size, usize::MAX)
}

fn abi_checksum_limited(
    dictionary: &DynamicDawgBinding,
    batch_size: usize,
    maximum_entries: usize,
) -> usize {
    let resource = dictionary.resource();
    let raw = resource.as_raw();
    let vtable = unsafe { entry_vtable(raw) };
    let mut cursor = VtDictionaryEntriesCursor::NULL;
    let mut metadata = VtDictionaryEntriesInfo::default();
    let open = unsafe {
        ((*vtable).open.expect("entry cursor open"))(raw.context, &mut cursor, &mut metadata)
    };
    assert_eq!(VtStatus::from_raw(open), Some(VtStatus::Ok));

    let limits = VtDictionaryEntryBatchLimits {
        max_entries: batch_size,
        max_units: usize::MAX,
        max_values: batch_size,
        reserved: 0,
    };
    let mut checksum = 0usize;
    let mut processed = 0usize;
    let mut ended = false;
    loop {
        let mut batch = VtDictionaryEntryBatchView::default();
        let status = unsafe {
            ((*vtable).next_batch.expect("entry cursor next"))(&mut cursor, &limits, &mut batch)
        };
        match VtStatus::from_raw(status) {
            Some(VtStatus::End) => {
                ended = true;
                break;
            }
            Some(VtStatus::Ok) => {
                let entries = if batch.entry_count == 0 {
                    &[]
                } else {
                    unsafe { std::slice::from_raw_parts(batch.entries, batch.entry_count) }
                };
                for entry in entries
                    .iter()
                    .take(maximum_entries.saturating_sub(processed))
                {
                    let value = if entry.value_len == 0 {
                        0
                    } else {
                        unsafe { *batch.values.add(entry.value_offset) as usize }
                    };
                    checksum = checksum.wrapping_add(entry.unit_len ^ value);
                    processed += 1;
                }
                let release = unsafe {
                    ((*vtable).release_batch.expect("entry cursor release"))(
                        &mut cursor,
                        batch.generation,
                    )
                };
                assert_eq!(VtStatus::from_raw(release), Some(VtStatus::Ok));
                if processed >= maximum_entries {
                    break;
                }
            }
            status => panic!("unexpected entry cursor status {status:?}"),
        }
    }
    if !ended {
        let cancel = unsafe { ((*vtable).cancel.expect("entry cursor cancel"))(&mut cursor) };
        assert_eq!(VtStatus::from_raw(cancel), Some(VtStatus::Ok));
    }
    let close = unsafe { ((*vtable).close.expect("entry cursor close"))(&mut cursor) };
    assert_eq!(VtStatus::from_raw(close), Some(VtStatus::Ok));
    checksum
}

fn bench_collection_traversal(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("collection_traversal");
    for &size in SIZES {
        let entries = corpus(size);
        let direct: DynamicDawg<u64> = entries.iter().cloned().collect();
        let binding = DynamicDawgBinding::new(BindingUnitDomain::Byte);
        binding
            .insert_text_batch(
                entries
                    .iter()
                    .map(|(key, value)| (key.as_slice(), Some(*value))),
            )
            .expect("benchmark corpus is valid");
        let expected = direct_checksum(&direct);
        assert_eq!(visitor_checksum(&direct), expected);
        assert_eq!(abi_checksum(&binding, 256), expected);

        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(
            BenchmarkId::new("direct-owned-entries", size),
            &size,
            |bencher, _| bencher.iter(|| black_box(direct_checksum(black_box(&direct)))),
        );
        group.throughput(Throughput::Elements(64));
        group.bench_with_input(
            BenchmarkId::new("direct-early-cancel-64", size),
            &size,
            |bencher, _| {
                bencher.iter(|| {
                    let checksum = black_box(&direct)
                        .entries()
                        .take(64)
                        .map(|entry| entry.key.len() ^ entry.value.unwrap_or_default() as usize)
                        .fold(0, usize::wrapping_add);
                    black_box(checksum)
                })
            },
        );
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(
            BenchmarkId::new("direct-borrowed-visitor", size),
            &size,
            |bencher, _| bencher.iter(|| black_box(visitor_checksum(black_box(&direct)))),
        );
        group.bench_with_input(
            BenchmarkId::new("direct-materialize-vec", size),
            &size,
            |bencher, _| {
                bencher.iter(|| {
                    let entries: Vec<_> = black_box(&direct).entries().collect();
                    black_box(entries)
                })
            },
        );
        for &batch_size in ABI_BATCHES {
            group.bench_with_input(
                BenchmarkId::new(format!("abi-batch-{batch_size}"), size),
                &size,
                |bencher, _| {
                    bencher.iter(|| black_box(abi_checksum(black_box(&binding), batch_size)))
                },
            );
        }
        group.throughput(Throughput::Elements(64));
        group.bench_with_input(
            BenchmarkId::new("abi-early-cancel-64", size),
            &size,
            |bencher, _| {
                bencher.iter(|| black_box(abi_checksum_limited(black_box(&binding), 64, 64)))
            },
        );
        group.throughput(Throughput::Elements(size as u64));
    }
    group.finish();
}

criterion_group! {
    name = collection_benches;
    config = Criterion::default()
        .sample_size(30)
        .measurement_time(Duration::from_secs(3));
    targets = bench_collection_traversal
}
criterion_main!(collection_benches);
