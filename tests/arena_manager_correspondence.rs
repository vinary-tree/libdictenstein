//! Arena-manager correspondence checks for the persistent byte and char ARTs.
//!
//! These tests exercise the Rust side of `ArenaReservationSpec.v`: allocated
//! slots read back exact payloads, reserved sibling slots are contiguous,
//! updates preserve slot identity, dirty-slot flushes fail closed, and
//! load/reopen reconstructs the persisted slot directory.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::{
    ArenaManager as ByteArenaManager, ArenaSlot as ByteArenaSlot, BlockStorage, BufferManager,
    FileHeader, FlushConfig as ByteFlushConfig, MmapDiskManager, PersistentARTrieError, BLOCK_SIZE,
};
use libdictenstein::persistent_artrie_char::{
    ArenaManager as CharArenaManager, FlushConfig as CharFlushConfig,
};
use parking_lot::RwLock;
use std::sync::Arc;
use tempfile::TempDir;

struct FailingAllocateStorage;

impl BlockStorage for FailingAllocateStorage {
    fn read_block(
        &self,
        _block_id: u32,
        buffer: &mut [u8; BLOCK_SIZE],
    ) -> Result<(), PersistentARTrieError> {
        buffer.fill(0);
        Ok(())
    }

    fn write_block(
        &self,
        _block_id: u32,
        _buffer: &[u8; BLOCK_SIZE],
    ) -> Result<(), PersistentARTrieError> {
        Ok(())
    }

    fn read_bytes(
        &self,
        _block_id: u32,
        _offset: usize,
        buffer: &mut [u8],
    ) -> Result<(), PersistentARTrieError> {
        buffer.fill(0);
        Ok(())
    }

    fn write_bytes(
        &self,
        _block_id: u32,
        _offset: usize,
        _data: &[u8],
    ) -> Result<(), PersistentARTrieError> {
        Ok(())
    }

    fn allocate_block(&self) -> Result<u32, PersistentARTrieError> {
        Err(PersistentARTrieError::internal(
            "injected allocation failure",
        ))
    }

    fn free_block(&self, _block_id: u32) -> Result<(), PersistentARTrieError> {
        Ok(())
    }

    fn read_header(&self) -> Result<FileHeader, PersistentARTrieError> {
        Ok(FileHeader::new())
    }

    fn write_header(&self, _header: &FileHeader) -> Result<(), PersistentARTrieError> {
        Ok(())
    }

    fn read_header_bytes(&self, buffer: &mut [u8]) -> Result<(), PersistentARTrieError> {
        buffer.fill(0);
        Ok(())
    }

    fn write_header_bytes(&self, _bytes: &[u8]) -> Result<(), PersistentARTrieError> {
        Ok(())
    }

    fn root_ptr(&self) -> Result<u64, PersistentARTrieError> {
        Ok(0)
    }

    fn set_root_ptr(&self, _ptr: u64) -> Result<(), PersistentARTrieError> {
        Ok(())
    }

    fn entry_count(&self) -> Result<u64, PersistentARTrieError> {
        Ok(0)
    }

    fn set_entry_count(&self, _count: u64) -> Result<(), PersistentARTrieError> {
        Ok(())
    }

    fn file_size(&self) -> u64 {
        BLOCK_SIZE as u64
    }

    fn block_count(&self) -> Result<u32, PersistentARTrieError> {
        Ok(1)
    }

    fn path(&self) -> &str {
        "failing://allocate"
    }

    fn sync(&self) -> Result<(), PersistentARTrieError> {
        Ok(())
    }
}

#[test]
fn byte_reserved_allocations_are_contiguous_and_exact() {
    let mut manager = ByteArenaManager::<MmapDiskManager>::with_arena_size(4096);
    let mut reserved = manager.reserve_slots(3).expect("reserve slots");
    let first = reserved.first_child_slot();

    let slot0 = manager
        .allocate_reserved(&mut reserved, b"child-0")
        .expect("allocate child 0");
    let slot1 = manager
        .allocate_reserved(&mut reserved, b"child-1")
        .expect("allocate child 1");
    let slot2 = manager
        .allocate_reserved(&mut reserved, b"child-2")
        .expect("allocate child 2");

    assert_eq!(slot0, first);
    assert_eq!(slot1.arena_id, first.arena_id);
    assert_eq!(slot2.arena_id, first.arena_id);
    assert_eq!(slot1.slot_id, first.slot_id + 1);
    assert_eq!(slot2.slot_id, first.slot_id + 2);
    assert!(reserved.is_complete());
    assert!(manager.is_reservation_complete(&reserved));
    assert_eq!(manager.read(slot0).unwrap(), b"child-0");
    assert_eq!(manager.read(slot1).unwrap(), b"child-1");
    assert_eq!(manager.read(slot2).unwrap(), b"child-2");
    assert!(manager
        .allocate_reserved(&mut reserved, b"child-3")
        .is_err());
    assert!(manager.reserve_slots(0).is_err());
}

#[test]
fn byte_reserved_allocation_rejects_interleaved_slot_use() {
    let mut manager = ByteArenaManager::<MmapDiskManager>::with_arena_size(4096);
    let mut reserved = manager.reserve_slots(2).expect("reserve slots");

    let outside = manager.allocate(b"outside").expect("outside allocation");
    assert_eq!(outside.slot_id, reserved.first_slot);
    assert!(manager
        .allocate_reserved(&mut reserved, b"reserved")
        .is_err());
}

#[test]
fn byte_next_slot_is_defensive_after_loading_clear() {
    let mut manager = ByteArenaManager::<MmapDiskManager>::with_arena_size(4096);
    manager.allocate(b"before-clear").unwrap();
    assert!(manager.is_valid());

    manager.clear_for_loading();
    assert!(!manager.is_valid());
    assert_eq!(manager.next_slot(), ByteArenaSlot::new(0, 0));

    manager.ensure_valid();
    assert!(manager.is_valid());
    assert_eq!(manager.next_slot(), ByteArenaSlot::new(0, 0));
}

#[test]
fn byte_partial_update_survives_flush_reload() {
    let temp_dir = TempDir::new().expect("temp dir");
    let path = temp_dir.path().join("byte_arena.part");
    let slot0;
    let slot1;
    let arena_blocks;

    {
        let storage = MmapDiskManager::create(&path).expect("create storage");
        let buffer_manager = Arc::new(RwLock::new(BufferManager::new(storage, 8)));
        let config = ByteFlushConfig::with_slot_tracking().with_threshold(1.0);
        let mut manager =
            ByteArenaManager::with_buffer_manager_and_config(Arc::clone(&buffer_manager), config);

        slot0 = manager.allocate(b"aaaa").expect("allocate slot0");
        slot1 = manager.allocate(b"bbbb").expect("allocate slot1");
        manager
            .flush_dirty_slots()
            .expect("initial arena flush should succeed");

        manager
            .update(slot0, b"CCCC")
            .expect("same-size update should succeed");
        assert_eq!(manager.read(slot0).unwrap(), b"CCCC");
        assert_eq!(manager.read(slot1).unwrap(), b"bbbb");
        assert_eq!(
            manager.dirty_tracker_stats().unwrap().dirty_slots,
            1,
            "update must mark exactly the changed slot dirty"
        );

        let stats = manager
            .flush_dirty_slots()
            .expect("partial dirty-slot flush should succeed");
        assert_eq!(stats.partial_writes, 1);
        assert_eq!(stats.slots_written, 1);
        arena_blocks = manager.arena_block_ids();
        buffer_manager
            .write()
            .storage()
            .sync()
            .expect("sync storage");
    }

    let storage = MmapDiskManager::open(&path).expect("open storage");
    let buffer_manager = Arc::new(RwLock::new(BufferManager::new(storage, 8)));
    let mut loaded = ByteArenaManager::with_buffer_manager(buffer_manager);
    loaded.clear_for_loading();
    for (expected_arena_id, block_id) in arena_blocks {
        let loaded_arena_id = loaded.load_arena(block_id).expect("load arena");
        assert_eq!(loaded_arena_id, expected_arena_id);
    }
    loaded.set_active_arena(loaded.arena_count().saturating_sub(1));

    assert_eq!(loaded.read(slot0).unwrap(), b"CCCC");
    assert_eq!(loaded.read(slot1).unwrap(), b"bbbb");
}

#[test]
fn char_partial_update_survives_flush_reload_with_checksums() {
    let temp_dir = TempDir::new().expect("temp dir");
    let path = temp_dir.path().join("char_arena.part");
    let slot0;
    let slot1;
    let arena_blocks;

    {
        let storage = MmapDiskManager::create(&path).expect("create storage");
        let buffer_manager = Arc::new(RwLock::new(BufferManager::new(storage, 8)));
        let config = CharFlushConfig::with_slot_tracking().with_threshold(1.0);
        let mut manager =
            CharArenaManager::with_buffer_manager_and_config(Arc::clone(&buffer_manager), config);

        slot0 = manager.allocate(b"rose").expect("allocate slot0");
        slot1 = manager.allocate(b"lily").expect("allocate slot1");
        manager
            .flush_dirty_slots()
            .expect("initial arena flush should succeed");

        manager
            .update(slot0, b"iris")
            .expect("same-size update should succeed");
        assert_eq!(manager.read(slot0).unwrap(), b"iris");
        assert_eq!(manager.read(slot1).unwrap(), b"lily");
        assert_eq!(
            manager.dirty_tracker_stats().unwrap().dirty_slots,
            1,
            "update must mark exactly the changed slot dirty"
        );

        let stats = manager
            .flush_dirty_slots()
            .expect("partial dirty-slot flush should succeed");
        assert_eq!(stats.partial_writes, 1);
        assert_eq!(stats.slots_written, 1);
        arena_blocks = manager.arena_block_ids();
        buffer_manager
            .write()
            .storage()
            .sync()
            .expect("sync storage");
    }

    let storage = MmapDiskManager::open(&path).expect("open storage");
    let buffer_manager = Arc::new(RwLock::new(BufferManager::new(storage, 8)));
    let mut loaded = CharArenaManager::with_buffer_manager(buffer_manager);
    loaded.clear_for_loading();
    for (expected_arena_id, block_id) in arena_blocks {
        let loaded_arena_id = loaded.load_arena(block_id).expect("load arena");
        assert_eq!(loaded_arena_id, expected_arena_id);
    }
    loaded.set_active_arena(loaded.arena_count().saturating_sub(1));

    assert_eq!(loaded.read(slot0).unwrap(), b"iris");
    assert_eq!(loaded.read(slot1).unwrap(), b"lily");
}

#[test]
fn dirty_slot_flush_failure_preserves_dirty_tracker_state() {
    let storage = FailingAllocateStorage;
    let buffer_manager = Arc::new(RwLock::new(BufferManager::new(storage, 2)));
    let config = ByteFlushConfig::with_slot_tracking();
    let mut manager =
        ByteArenaManager::with_buffer_manager_and_config(Arc::clone(&buffer_manager), config);

    let slot = manager.allocate(b"dirty").expect("allocate dirty slot");
    let before = manager
        .dirty_tracker_stats()
        .expect("dirty tracker enabled");

    assert!(manager.flush_dirty_slots().is_err());

    let after = manager
        .dirty_tracker_stats()
        .expect("dirty tracker still enabled");
    assert_eq!(after.dirty_arenas, before.dirty_arenas);
    assert_eq!(after.dirty_slots, before.dirty_slots);
    assert_eq!(manager.read(slot).unwrap(), b"dirty");
}
