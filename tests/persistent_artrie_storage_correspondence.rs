//! Storage-boundary correspondence checks for the persistent ARTrie backends.
//!
//! These tests exercise the Rust side of the `MmapBlockStorage.tla` model:
//! allocation publishes unique block IDs, byte ranges stay inside the declared
//! block, and sync refreshes the header checksum after allocation metadata
//! changes.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::{MmapDiskManager, BLOCK_SIZE};
use std::collections::BTreeSet;
use std::sync::{Arc, Barrier};
use std::thread;
use tempfile::TempDir;

#[test]
fn mmap_concurrent_allocations_return_unique_accessible_blocks() {
    let temp_dir = TempDir::new().expect("temp dir");
    let path = temp_dir.path().join("concurrent_alloc.part");
    let storage = Arc::new(MmapDiskManager::create(&path).expect("create mmap storage"));
    let barrier = Arc::new(Barrier::new(9));
    let mut handles = Vec::new();

    for worker in 0..8u64 {
        let storage = Arc::clone(&storage);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            let mut allocated = Vec::new();

            for step in 0..4u64 {
                let block_id = storage.allocate_block().expect("allocate block");
                let marker = ((worker << 32) | step).to_le_bytes();
                storage
                    .write_bytes(block_id, 0, &marker)
                    .expect("write marker into allocated block");
                allocated.push((block_id, marker));
            }

            allocated
        }));
    }

    barrier.wait();

    let mut allocated = Vec::new();
    for handle in handles {
        allocated.extend(handle.join().expect("allocator thread completed"));
    }

    let unique: BTreeSet<_> = allocated.iter().map(|(block_id, _)| *block_id).collect();
    assert_eq!(
        unique.len(),
        allocated.len(),
        "allocated duplicate block IDs"
    );
    assert!(unique.iter().all(|block_id| *block_id > 0));

    let block_count = storage.block_count().expect("block count");
    assert_eq!(usize::try_from(block_count).unwrap(), allocated.len() + 1);
    assert!(storage.file_size() >= u64::from(block_count) * BLOCK_SIZE as u64);

    for (block_id, marker) in allocated {
        let mut recovered = [0u8; 8];
        storage
            .read_bytes(block_id, 0, &mut recovered)
            .expect("read marker from allocated block");
        assert_eq!(recovered, marker, "marker for block {block_id}");
    }
}

#[test]
fn mmap_sub_block_io_rejects_ranges_that_cross_block_boundaries() {
    let temp_dir = TempDir::new().expect("temp dir");
    let path = temp_dir.path().join("range_bounds.part");
    let storage = MmapDiskManager::create(&path).expect("create mmap storage");
    let second_block = storage.allocate_block().expect("allocate second block");

    storage
        .write_bytes(second_block, 0, b"keep")
        .expect("seed second block");

    assert!(
        storage
            .write_bytes(0, BLOCK_SIZE - 1, &[0xaa, 0xbb])
            .is_err(),
        "write crossed from block 0 into block 1"
    );

    let mut rejected_read = [0u8; 2];
    assert!(
        storage
            .read_bytes(0, BLOCK_SIZE - 1, &mut rejected_read)
            .is_err(),
        "read crossed from block 0 into block 1"
    );

    assert!(
        storage
            .write_bytes(second_block, BLOCK_SIZE, &[0xcc])
            .is_err(),
        "write at one-past-end offset was accepted"
    );

    let mut second_prefix = [0u8; 4];
    storage
        .read_bytes(second_block, 0, &mut second_prefix)
        .expect("read second block prefix");
    assert_eq!(
        &second_prefix, b"keep",
        "rejected cross-block write mutated the following block"
    );
}

#[test]
fn mmap_sync_refreshes_allocation_header_checksum_for_reopen() {
    let temp_dir = TempDir::new().expect("temp dir");
    let path = temp_dir.path().join("sync_reopen.part");
    let block_id;

    {
        let storage = MmapDiskManager::create(&path).expect("create mmap storage");
        block_id = storage.allocate_block().expect("allocate block");
        storage
            .write_bytes(block_id, 32, b"durable")
            .expect("write durable marker");
        storage.sync().expect("sync storage");
    }

    let reopened = MmapDiskManager::open(&path).expect("reopen mmap storage");
    assert!(reopened.block_count().expect("block count") > block_id);

    let mut recovered = [0u8; 7];
    reopened
        .read_bytes(block_id, 32, &mut recovered)
        .expect("read durable marker after reopen");
    assert_eq!(&recovered, b"durable");
}

#[test]
fn mmap_raw_ptr_rejects_offsets_outside_declared_block() {
    let temp_dir = TempDir::new().expect("temp dir");
    let path = temp_dir.path().join("raw_ptr_bounds.part");
    let storage = MmapDiskManager::create(&path).expect("create mmap storage");

    assert!(unsafe { storage.raw_ptr(0, 0) }.is_ok());
    assert!(unsafe { storage.raw_ptr(0, BLOCK_SIZE) }.is_err());
    assert!(unsafe { storage.raw_ptr(1, 0) }.is_err());
}

#[test]
fn swizzled_disk_pointer_raw_roundtrip_is_miri_friendly() {
    use libdictenstein::persistent_artrie::{NodeType, SwizzledPtr};

    let ptr = SwizzledPtr::on_disk(17, 4096, NodeType::Node16);
    let restored = SwizzledPtr::from_raw(ptr.to_raw());
    let loc = restored.disk_location().expect("disk location");

    assert!(restored.is_on_disk());
    assert!(!restored.is_swizzled());
    assert_eq!(loc.block_id, 17);
    assert_eq!(loc.offset, 4096);
    assert_eq!(loc.node_type, NodeType::Node16);

    let null = SwizzledPtr::from_raw(SwizzledPtr::null().to_raw());
    assert!(null.is_null());
    assert!(null.disk_location().is_none());
}

#[cfg(feature = "io-uring-backend")]
#[test]
fn io_uring_block_storage_rejects_out_of_block_ranges_when_available() {
    use libdictenstein::persistent_artrie::{BlockStorage, IoUringDiskManager};

    let temp_dir = TempDir::new().expect("temp dir");
    let path = temp_dir.path().join("io_uring_bounds.part");
    let storage = match IoUringDiskManager::create(&path) {
        Ok(storage) => storage,
        Err(error) => {
            eprintln!("skipping io_uring storage correspondence test: {error}");
            return;
        }
    };

    let second_block = storage.allocate_block().expect("allocate second block");
    storage
        .write_bytes(second_block, 0, b"keep")
        .expect("seed second block");

    assert!(storage.write_bytes(0, BLOCK_SIZE - 1, &[1, 2]).is_err());
    assert!(storage.write_bytes(second_block, BLOCK_SIZE, &[3]).is_err());

    let mut recovered = [0u8; 4];
    storage
        .read_bytes(second_block, 0, &mut recovered)
        .expect("read second block");
    assert_eq!(&recovered, b"keep");
}
