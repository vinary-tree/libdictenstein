//! Deterministic single-arm driver for paired collection-traversal experiments.
//!
//! The driver constructs equivalent direct-Rust and resource-backed
//! dictionaries before starting its clock. One invocation measures one arm so
//! the repository's topology admission runner can alternate arm order without
//! retaining diagnostic branches in production code.

use libdictenstein::bindings::{BindingUnitDomain, DynamicDawgBinding};
use libdictenstein::collection::DictionaryEntries;
use libdictenstein::dynamic_dawg::DynamicDawg;
use std::env;
use std::ffi::c_void;
use std::hint::black_box;
use std::time::Instant;
use vinary_tree_interop::{
    VtDictionaryEntriesCursor, VtDictionaryEntriesInfo, VtDictionaryEntriesVTable,
    VtDictionaryEntryBatchLimits, VtDictionaryEntryBatchView, VtStatus,
    VT_DICTIONARY_ENTRIES_INTERFACE_ID, VT_DICTIONARY_ENTRIES_INTERFACE_VERSION,
};

const USAGE: &str = "usage: collection_traversal_profile --arm ARM [--entries N] [--passes N]\n\
arms: direct-owned, direct-visitor, direct-materialized, abi-64, abi-256, abi-1024, direct-cancel-64, abi-cancel-64";

#[derive(Clone, Copy, Debug)]
enum Arm {
    Owned,
    Visitor,
    Materialized,
    Abi(usize),
    DirectCancel,
    AbiCancel,
}

impl Arm {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "direct-owned" => Ok(Self::Owned),
            "direct-visitor" => Ok(Self::Visitor),
            "direct-materialized" => Ok(Self::Materialized),
            "abi-64" => Ok(Self::Abi(64)),
            "abi-256" => Ok(Self::Abi(256)),
            "abi-1024" => Ok(Self::Abi(1_024)),
            "direct-cancel-64" => Ok(Self::DirectCancel),
            "abi-cancel-64" => Ok(Self::AbiCancel),
            _ => Err(format!("unknown arm: {value}")),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Owned => "direct-owned",
            Self::Visitor => "direct-visitor",
            Self::Materialized => "direct-materialized",
            Self::Abi(64) => "abi-64",
            Self::Abi(256) => "abi-256",
            Self::Abi(1_024) => "abi-1024",
            Self::Abi(_) => unreachable!("only supported batch sizes are constructed"),
            Self::DirectCancel => "direct-cancel-64",
            Self::AbiCancel => "abi-cancel-64",
        }
    }

    fn consumed_entries(self, dictionary_entries: usize) -> usize {
        match self {
            Self::DirectCancel | Self::AbiCancel => dictionary_entries.min(64),
            _ => dictionary_entries,
        }
    }
}

fn parse_positive(value: Option<String>, option: &str) -> Result<usize, String> {
    let raw = value.ok_or_else(|| format!("missing value for {option}"))?;
    let parsed = raw
        .parse::<usize>()
        .map_err(|_| format!("{option} must be a positive integer"))?;
    if parsed == 0 {
        return Err(format!("{option} must be a positive integer"));
    }
    Ok(parsed)
}

fn arguments() -> Result<(Arm, usize, usize), String> {
    let mut arm = None;
    let mut entries = 65_536;
    let mut passes = 1;
    let mut args = env::args().skip(1);
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--arm" => {
                arm = Some(Arm::parse(
                    &args
                        .next()
                        .ok_or_else(|| "missing value for --arm".to_owned())?,
                )?);
            }
            "--entries" => entries = parse_positive(args.next(), "--entries")?,
            "--passes" => passes = parse_positive(args.next(), "--passes")?,
            "--help" | "-h" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            _ => return Err(format!("unknown argument: {argument}")),
        }
    }
    Ok((
        arm.ok_or_else(|| "--arm is required".to_owned())?,
        entries,
        passes,
    ))
}

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

fn direct_owned(dictionary: &DynamicDawg<u64>, limit: usize) -> usize {
    dictionary
        .entries()
        .take(limit)
        .map(|entry| entry.key.len() ^ entry.value.unwrap_or_default() as usize)
        .fold(0, usize::wrapping_add)
}

fn direct_visitor(dictionary: &DynamicDawg<u64>) -> usize {
    let mut checksum = 0usize;
    dictionary.entries().visit(|key, value| {
        checksum = checksum.wrapping_add(key.len() ^ value.unwrap_or_default() as usize);
    });
    checksum
}

fn direct_materialized(dictionary: &DynamicDawg<u64>) -> usize {
    let entries: Vec<_> = dictionary.entries().collect();
    entries
        .iter()
        .map(|entry| entry.key.len() ^ entry.value.unwrap_or_default() as usize)
        .fold(0, usize::wrapping_add)
}

unsafe fn entries_vtable(
    resource: vinary_tree_interop::VtResource,
) -> *const VtDictionaryEntriesVTable {
    let mut interface: *const c_void = std::ptr::null();
    let raw_status = unsafe {
        ((*resource.vtable)
            .query_interface
            .expect("resource has query_interface"))(
            resource.context,
            &VT_DICTIONARY_ENTRIES_INTERFACE_ID,
            VT_DICTIONARY_ENTRIES_INTERFACE_VERSION,
            &mut interface,
        )
    };
    assert_eq!(VtStatus::from_raw(raw_status), Some(VtStatus::Ok));
    assert!(!interface.is_null());
    interface.cast()
}

fn abi_traversal(
    dictionary: &DynamicDawgBinding,
    batch_size: usize,
    maximum_entries: usize,
) -> (usize, usize) {
    let resource = dictionary.resource();
    let raw = resource.as_raw();
    let vtable = unsafe { entries_vtable(raw) };
    let mut calls = 1usize; // query_interface
    let mut cursor = VtDictionaryEntriesCursor::NULL;
    let mut metadata = VtDictionaryEntriesInfo::default();
    let open =
        unsafe { ((*vtable).open.expect("entries open"))(raw.context, &mut cursor, &mut metadata) };
    calls += 1;
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
        let next = unsafe {
            ((*vtable).next_batch.expect("entries next"))(&mut cursor, &limits, &mut batch)
        };
        calls += 1;
        match VtStatus::from_raw(next) {
            Some(VtStatus::End) => {
                ended = true;
                break;
            }
            Some(VtStatus::Ok) => {
                let descriptors = if batch.entry_count == 0 {
                    &[]
                } else {
                    unsafe { std::slice::from_raw_parts(batch.entries, batch.entry_count) }
                };
                for entry in descriptors
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
                    ((*vtable).release_batch.expect("entries release"))(
                        &mut cursor,
                        batch.generation,
                    )
                };
                calls += 1;
                assert_eq!(VtStatus::from_raw(release), Some(VtStatus::Ok));
                if processed >= maximum_entries {
                    break;
                }
            }
            status => panic!("unexpected entries status: {status:?}"),
        }
    }
    if !ended {
        let cancel = unsafe { ((*vtable).cancel.expect("entries cancel"))(&mut cursor) };
        calls += 1;
        assert_eq!(VtStatus::from_raw(cancel), Some(VtStatus::Ok));
    }
    let close = unsafe { ((*vtable).close.expect("entries close"))(&mut cursor) };
    calls += 1;
    assert_eq!(VtStatus::from_raw(close), Some(VtStatus::Ok));
    (checksum, calls)
}

fn main() {
    let (arm, entry_count, passes) = arguments().unwrap_or_else(|error| {
        eprintln!("{error}\n{USAGE}");
        std::process::exit(2);
    });
    let entries = corpus(entry_count);
    let direct: DynamicDawg<u64> = entries.iter().cloned().collect();
    let binding = DynamicDawgBinding::new(BindingUnitDomain::Byte);
    binding
        .insert_text_batch(
            entries
                .iter()
                .map(|(key, value)| (key.as_slice(), Some(*value))),
        )
        .expect("generated corpus is valid");

    let expected_full = direct_owned(&direct, usize::MAX);
    assert_eq!(direct_visitor(&direct), expected_full);
    assert_eq!(abi_traversal(&binding, 256, usize::MAX).0, expected_full);
    let expected_early = direct_owned(&direct, 64);
    assert_eq!(abi_traversal(&binding, 64, 64).0, expected_early);

    let started = Instant::now();
    let mut checksum = 0usize;
    let mut boundary_calls = 0usize;
    for _ in 0..passes {
        let (pass_checksum, pass_calls) = match arm {
            Arm::Owned => (direct_owned(black_box(&direct), usize::MAX), 0),
            Arm::Visitor => (direct_visitor(black_box(&direct)), 0),
            Arm::Materialized => (direct_materialized(black_box(&direct)), 0),
            Arm::Abi(batch_size) => abi_traversal(black_box(&binding), batch_size, usize::MAX),
            Arm::DirectCancel => (direct_owned(black_box(&direct), 64), 0),
            Arm::AbiCancel => abi_traversal(black_box(&binding), 64, 64),
        };
        checksum = checksum.wrapping_add(black_box(pass_checksum));
        boundary_calls = boundary_calls.saturating_add(pass_calls);
    }
    let elapsed_ns = started.elapsed().as_nanos();
    let expected = match arm {
        Arm::DirectCancel | Arm::AbiCancel => expected_early,
        _ => expected_full,
    };
    assert_eq!(checksum, expected.wrapping_mul(passes));

    println!(
        "{{\"schema\":\"libdictenstein.collection-traversal.v1\",\"arm\":\"{}\",\"dictionary_entries\":{},\"consumed_entries_per_pass\":{},\"passes\":{},\"elapsed_ns\":{},\"checksum\":{},\"boundary_calls\":{}}}",
        arm.name(),
        entry_count,
        arm.consumed_entries(entry_count),
        passes,
        elapsed_ns,
        checksum,
        boundary_calls
    );
}
