//! Executable correspondence checks for dirty checkpoint publication safety.
//!
//! These tests exercise the Rust side of `PersistentDirtyCheckpointSpec.v`:
//! dirty arena/slot evidence survives failed writes and syncs, slot tracking
//! enabled after pre-existing mutations still covers dirty arenas, and a
//! descriptor published before WAL truncation reopens with the WAL tail.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::{
    ArenaManager as ByteArenaManager, BlockStorage, BufferManager, FileHeader,
    FlushConfig as ByteFlushConfig, PersistentARTrie, PersistentARTrieError, BLOCK_SIZE,
};
use libdictenstein::persistent_artrie_char::{
    ArenaManager as CharArenaManager, FlushConfig as CharFlushConfig, PersistentARTrieChar,
};
use libdictenstein::MappedDictionary;
use parking_lot::RwLock;
use std::io;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use tempfile::tempdir;

#[derive(Clone)]
struct FlakyBlockStorage {
    state: Arc<Mutex<FlakyStorageState>>,
    path: String,
}

struct FlakyStorageState {
    blocks: Vec<Box<[u8; BLOCK_SIZE]>>,
    fail_next_write: bool,
    fail_next_write_bytes: bool,
    fail_next_sync: bool,
}

impl FlakyBlockStorage {
    fn new(path: &str) -> Self {
        let mut header = FileHeader::new();
        header.update_checksum();

        let mut block0 = Box::new([0u8; BLOCK_SIZE]);
        block0[..64].copy_from_slice(&header.to_bytes());

        Self {
            state: Arc::new(Mutex::new(FlakyStorageState {
                blocks: vec![block0],
                fail_next_write: false,
                fail_next_write_bytes: false,
                fail_next_sync: false,
            })),
            path: path.to_string(),
        }
    }

    fn fail_next_write_bytes(&self) {
        self.state.lock().unwrap().fail_next_write_bytes = true;
    }

    fn fail_next_sync(&self) {
        self.state.lock().unwrap().fail_next_sync = true;
    }

    fn injected_error(&self, operation: &str) -> PersistentARTrieError {
        PersistentARTrieError::io_error(
            operation,
            self.path.clone(),
            io::Error::new(io::ErrorKind::Other, "injected storage failure"),
        )
    }

    fn ensure_block<'a>(
        &'a self,
        state: &'a mut FlakyStorageState,
        block_id: u32,
    ) -> Result<&'a mut [u8; BLOCK_SIZE], PersistentARTrieError> {
        state
            .blocks
            .get_mut(block_id as usize)
            .map(Box::as_mut)
            .ok_or_else(|| PersistentARTrieError::InvalidBlockId {
                block_id,
                reason: "block does not exist".to_string(),
            })
    }
}

impl BlockStorage for FlakyBlockStorage {
    fn read_block(
        &self,
        block_id: u32,
        buffer: &mut [u8; BLOCK_SIZE],
    ) -> Result<(), PersistentARTrieError> {
        let state = self.state.lock().unwrap();
        let block = state.blocks.get(block_id as usize).ok_or_else(|| {
            PersistentARTrieError::InvalidBlockId {
                block_id,
                reason: "block does not exist".to_string(),
            }
        })?;
        buffer.copy_from_slice(block.as_ref());
        Ok(())
    }

    fn write_block(
        &self,
        block_id: u32,
        buffer: &[u8; BLOCK_SIZE],
    ) -> Result<(), PersistentARTrieError> {
        let mut state = self.state.lock().unwrap();
        if state.fail_next_write {
            state.fail_next_write = false;
            return Err(self.injected_error("write_block"));
        }
        let block = self.ensure_block(&mut state, block_id)?;
        block.copy_from_slice(buffer);
        Ok(())
    }

    fn read_bytes(
        &self,
        block_id: u32,
        offset: usize,
        buffer: &mut [u8],
    ) -> Result<(), PersistentARTrieError> {
        let state = self.state.lock().unwrap();
        let block = state.blocks.get(block_id as usize).ok_or_else(|| {
            PersistentARTrieError::InvalidBlockId {
                block_id,
                reason: "block does not exist".to_string(),
            }
        })?;
        let end = offset.checked_add(buffer.len()).ok_or_else(|| {
            PersistentARTrieError::InvalidBlockId {
                block_id,
                reason: "read range overflow".to_string(),
            }
        })?;
        if end > BLOCK_SIZE {
            return Err(PersistentARTrieError::InvalidBlockId {
                block_id,
                reason: "read range exceeds block".to_string(),
            });
        }
        buffer.copy_from_slice(&block[offset..end]);
        Ok(())
    }

    fn write_bytes(
        &self,
        block_id: u32,
        offset: usize,
        data: &[u8],
    ) -> Result<(), PersistentARTrieError> {
        let mut state = self.state.lock().unwrap();
        if state.fail_next_write_bytes {
            state.fail_next_write_bytes = false;
            return Err(self.injected_error("write_bytes"));
        }
        let end = offset.checked_add(data.len()).ok_or_else(|| {
            PersistentARTrieError::InvalidBlockId {
                block_id,
                reason: "write range overflow".to_string(),
            }
        })?;
        if end > BLOCK_SIZE {
            return Err(PersistentARTrieError::InvalidBlockId {
                block_id,
                reason: "write range exceeds block".to_string(),
            });
        }
        let block = self.ensure_block(&mut state, block_id)?;
        block[offset..end].copy_from_slice(data);
        Ok(())
    }

    fn allocate_block(&self) -> Result<u32, PersistentARTrieError> {
        let mut state = self.state.lock().unwrap();
        let block_id = state.blocks.len() as u32;
        state.blocks.push(Box::new([0u8; BLOCK_SIZE]));
        Ok(block_id)
    }

    fn free_block(&self, _block_id: u32) -> Result<(), PersistentARTrieError> {
        Ok(())
    }

    fn read_header(&self) -> Result<FileHeader, PersistentARTrieError> {
        let state = self.state.lock().unwrap();
        let mut header_bytes = [0u8; 64];
        header_bytes.copy_from_slice(&state.blocks[0][..64]);
        Ok(FileHeader::from_bytes(&header_bytes))
    }

    fn write_header(&self, header: &FileHeader) -> Result<(), PersistentARTrieError> {
        let mut state = self.state.lock().unwrap();
        state.blocks[0][..64].copy_from_slice(&header.to_bytes());
        Ok(())
    }

    fn read_header_bytes(&self, buffer: &mut [u8]) -> Result<(), PersistentARTrieError> {
        self.read_bytes(0, 0, buffer)
    }

    fn write_header_bytes(&self, bytes: &[u8]) -> Result<(), PersistentARTrieError> {
        self.write_bytes(0, 0, bytes)
    }

    fn root_ptr(&self) -> Result<u64, PersistentARTrieError> {
        Ok(self.read_header()?.root_ptr.load(Ordering::Acquire))
    }

    fn set_root_ptr(&self, ptr: u64) -> Result<(), PersistentARTrieError> {
        let mut header = self.read_header()?;
        header.root_ptr.store(ptr, Ordering::Release);
        header.update_checksum();
        self.write_header(&header)
    }

    fn entry_count(&self) -> Result<u64, PersistentARTrieError> {
        Ok(self.read_header()?.entry_count.load(Ordering::Acquire))
    }

    fn set_entry_count(&self, count: u64) -> Result<(), PersistentARTrieError> {
        let mut header = self.read_header()?;
        header.entry_count.store(count, Ordering::Release);
        header.update_checksum();
        self.write_header(&header)
    }

    fn file_size(&self) -> u64 {
        (self.state.lock().unwrap().blocks.len() * BLOCK_SIZE) as u64
    }

    fn block_count(&self) -> Result<u32, PersistentARTrieError> {
        Ok(self.state.lock().unwrap().blocks.len() as u32)
    }

    fn path(&self) -> &str {
        &self.path
    }

    fn sync(&self) -> Result<(), PersistentARTrieError> {
        let mut state = self.state.lock().unwrap();
        if state.fail_next_sync {
            state.fail_next_sync = false;
            return Err(self.injected_error("sync"));
        }
        Ok(())
    }
}

#[test]
fn byte_dirty_slot_write_failure_preserves_evidence_and_retry_persists_update() {
    let storage = FlakyBlockStorage::new("flaky://byte-write");
    let buffer_manager = Arc::new(RwLock::new(BufferManager::new(storage.clone(), 4)));
    let config = ByteFlushConfig::with_slot_tracking().with_threshold(1.0);
    let mut manager =
        ByteArenaManager::with_buffer_manager_and_config(Arc::clone(&buffer_manager), config);

    let slot0 = manager.allocate(b"aaaa").expect("allocate slot0");
    let slot1 = manager.allocate(b"bbbb").expect("allocate slot1");
    manager
        .flush_dirty_slots()
        .expect("initial dirty-slot flush");

    manager
        .update(slot0, b"CCCC")
        .expect("same-size byte update");
    let before = manager.dirty_tracker_stats().expect("dirty tracker");
    assert_eq!(before.dirty_slots, 1);

    storage.fail_next_write_bytes();
    assert!(
        manager.flush_dirty_slots().is_err(),
        "failed slot write must abort the flush"
    );

    let after = manager.dirty_tracker_stats().expect("dirty tracker");
    assert_eq!(after.dirty_arenas, before.dirty_arenas);
    assert_eq!(after.dirty_slots, before.dirty_slots);
    assert_eq!(manager.read(slot0).unwrap(), b"CCCC");

    let stats = manager.flush_dirty_slots().expect("retry dirty-slot flush");
    assert_eq!(stats.partial_writes, 1);
    assert_eq!(stats.slots_written, 1);

    let arena_blocks = manager.arena_block_ids();
    drop(manager);
    drop(buffer_manager);

    let reopened_buffer_manager = Arc::new(RwLock::new(BufferManager::new(storage, 4)));
    let mut loaded = ByteArenaManager::with_buffer_manager(reopened_buffer_manager);
    loaded.clear_for_loading();
    for (expected_arena_id, block_id) in arena_blocks {
        let loaded_arena_id = loaded.load_arena(block_id).expect("load byte arena");
        assert_eq!(loaded_arena_id, expected_arena_id);
    }
    loaded.set_active_arena(loaded.arena_count().saturating_sub(1));

    assert_eq!(loaded.read(slot0).unwrap(), b"CCCC");
    assert_eq!(loaded.read(slot1).unwrap(), b"bbbb");
}

#[test]
fn char_dirty_slot_sync_failure_preserves_evidence_until_retry_syncs() {
    let storage = FlakyBlockStorage::new("flaky://char-sync");
    let buffer_manager = Arc::new(RwLock::new(BufferManager::new(storage.clone(), 4)));
    let config = CharFlushConfig::with_slot_tracking().with_threshold(1.0);
    let mut manager =
        CharArenaManager::with_buffer_manager_and_config(Arc::clone(&buffer_manager), config);

    let slot0 = manager.allocate(b"rose").expect("allocate slot0");
    let slot1 = manager.allocate(b"lily").expect("allocate slot1");
    manager
        .flush_dirty_slots()
        .expect("initial char arena flush");

    manager
        .update(slot0, b"iris")
        .expect("same-size char update");
    let before = manager.dirty_tracker_stats().expect("dirty tracker");
    assert_eq!(before.dirty_slots, 1);

    storage.fail_next_sync();
    assert!(
        manager.flush_dirty_slots().is_err(),
        "failed storage sync must abort checkpoint completion"
    );

    let after_failure = manager.dirty_tracker_stats().expect("dirty tracker");
    assert_eq!(after_failure.dirty_arenas, before.dirty_arenas);
    assert_eq!(after_failure.dirty_slots, before.dirty_slots);

    manager
        .flush_dirty_slots()
        .expect("retry must complete sync and clear dirty tracker");
    let after_retry = manager.dirty_tracker_stats().expect("dirty tracker");
    assert_eq!(after_retry.dirty_arenas, 0);
    assert_eq!(after_retry.dirty_slots, 0);

    let arena_blocks = manager.arena_block_ids();
    drop(manager);
    drop(buffer_manager);

    let reopened_buffer_manager = Arc::new(RwLock::new(BufferManager::new(storage, 4)));
    let mut loaded = CharArenaManager::with_buffer_manager(reopened_buffer_manager);
    loaded.clear_for_loading();
    for (expected_arena_id, block_id) in arena_blocks {
        let loaded_arena_id = loaded.load_arena(block_id).expect("load char arena");
        assert_eq!(loaded_arena_id, expected_arena_id);
    }
    loaded.set_active_arena(loaded.arena_count().saturating_sub(1));

    assert_eq!(loaded.read(slot0).unwrap(), b"iris");
    assert_eq!(loaded.read(slot1).unwrap(), b"lily");
}

#[test]
fn late_slot_tracking_flushes_preexisting_dirty_byte_and_char_arenas() {
    let byte_storage = FlakyBlockStorage::new("flaky://late-byte");
    let byte_buffer_manager = Arc::new(RwLock::new(BufferManager::new(byte_storage.clone(), 4)));
    let mut byte_manager = ByteArenaManager::with_buffer_manager(Arc::clone(&byte_buffer_manager));
    let byte_slot = byte_manager
        .allocate(b"byte")
        .expect("allocate byte before tracking");
    assert!(byte_manager.dirty_tracker_stats().is_none());

    byte_manager.enable_slot_tracking();
    assert_eq!(
        byte_manager.dirty_tracker_stats().unwrap().dirty_arenas,
        1,
        "late slot tracking must remember the already dirty byte arena"
    );
    byte_manager
        .flush_dirty_slots()
        .expect("flush late-tracked byte arena");
    let byte_blocks = byte_manager.arena_block_ids();
    drop(byte_manager);
    drop(byte_buffer_manager);

    let byte_reopen_buffer = Arc::new(RwLock::new(BufferManager::new(byte_storage, 4)));
    let mut loaded_byte = ByteArenaManager::with_buffer_manager(byte_reopen_buffer);
    loaded_byte.clear_for_loading();
    for (_, block_id) in byte_blocks {
        loaded_byte.load_arena(block_id).expect("load byte arena");
    }
    loaded_byte.set_active_arena(loaded_byte.arena_count().saturating_sub(1));
    assert_eq!(loaded_byte.read(byte_slot).unwrap(), b"byte");

    let char_storage = FlakyBlockStorage::new("flaky://late-char");
    let char_buffer_manager = Arc::new(RwLock::new(BufferManager::new(char_storage.clone(), 4)));
    let mut char_manager = CharArenaManager::with_buffer_manager(Arc::clone(&char_buffer_manager));
    let char_slot = char_manager
        .allocate(b"char")
        .expect("allocate char before tracking");
    assert!(char_manager.dirty_tracker_stats().is_none());

    char_manager.enable_slot_tracking();
    assert_eq!(
        char_manager.dirty_tracker_stats().unwrap().dirty_arenas,
        1,
        "late slot tracking must remember the already dirty char arena"
    );
    char_manager
        .flush_dirty_slots()
        .expect("flush late-tracked char arena");
    let char_blocks = char_manager.arena_block_ids();
    drop(char_manager);
    drop(char_buffer_manager);

    let char_reopen_buffer = Arc::new(RwLock::new(BufferManager::new(char_storage, 4)));
    let mut loaded_char = CharArenaManager::with_buffer_manager(char_reopen_buffer);
    loaded_char.clear_for_loading();
    for (_, block_id) in char_blocks {
        loaded_char.load_arena(block_id).expect("load char arena");
    }
    loaded_char.set_active_arena(loaded_char.arena_count().saturating_sub(1));
    assert_eq!(loaded_char.read(char_slot).unwrap(), b"char");
}

#[test]
fn byte_descriptor_publication_before_wal_truncation_reopens_with_wal_tail() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("byte_descriptor_before_truncate.part");

    {
        let mut trie = PersistentARTrie::<i32>::create(&path).expect("create byte trie");
        assert!(trie.insert_with_value("descriptor", 10));
        trie.persist_to_disk().expect("publish descriptor only");

        assert!(trie.insert_with_value("wal-tail", 20));
        trie.sync().expect("sync WAL tail");
    }

    let reopened = PersistentARTrie::<i32>::open(&path).expect("reopen byte trie");
    assert_eq!(reopened.get_value("descriptor"), Some(10));
    assert_eq!(reopened.get_value("wal-tail"), Some(20));
}

#[test]
fn char_descriptor_publication_before_wal_truncation_reopens_with_wal_tail() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("char_descriptor_before_truncate.part");

    {
        let mut trie = PersistentARTrieChar::<i32>::create(&path).expect("create char trie");
        trie.insert_with_value("descriptor", 10)
            .expect("insert descriptor term");
        trie.persist_to_disk().expect("publish descriptor only");

        trie.insert_with_value("wal-tail", 20)
            .expect("insert WAL tail term");
        trie.sync().expect("sync WAL tail");
    }

    let reopened = PersistentARTrieChar::<i32>::open(&path).expect("reopen char trie");
    // F2-migrate: Bucket A — `get()` returns None under the overlay; read via `get_value`.
    assert_eq!(reopened.get_value("descriptor"), Some(10));
    assert_eq!(reopened.get_value("wal-tail"), Some(20));
}
