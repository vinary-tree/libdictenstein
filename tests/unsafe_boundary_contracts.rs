use std::sync::{Arc, Barrier};
use std::thread;

use libdictenstein::scdawg::char::{ScdawgChar, ScdawgCharNodeHandle};
use libdictenstein::scdawg::{Scdawg, ScdawgNodeHandle};
use libdictenstein::DictionaryNode;

fn assert_send_sync<T: Send + Sync>() {}

#[test]
fn scdawg_node_handles_are_send_sync() {
    assert_send_sync::<ScdawgNodeHandle<usize>>();
    assert_send_sync::<ScdawgCharNodeHandle<usize>>();
}

#[test]
fn byte_scdawg_handle_supports_concurrent_read_traversal() {
    let scdawg = Scdawg::<usize>::from_terms_with_values([
        ("alpha", 1),
        ("alpine", 2),
        ("alphabet", 3),
        ("beta", 4),
    ]);
    let handle = Arc::new(scdawg.find("alp").expect("substring handle exists"));
    let expected_final = handle.is_final();
    let expected_edges = handle.edge_count();
    let barrier = Arc::new(Barrier::new(8));

    let workers: Vec<_> = (0..8)
        .map(|_| {
            let handle = Arc::clone(&handle);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                for _ in 0..64 {
                    assert_eq!(handle.is_final(), expected_final);
                    assert_eq!(handle.edge_count(), expected_edges);
                    let _ = handle.edges().count();
                }
            })
        })
        .collect();

    for worker in workers {
        worker.join().expect("reader thread completes");
    }
}

#[test]
fn char_scdawg_handle_supports_concurrent_read_traversal() {
    let scdawg = ScdawgChar::<usize>::from_terms_with_values([
        ("cafe", 1),
        ("cafeine", 2),
        ("cafeteria", 3),
        ("cane", 4),
    ]);
    let handle = Arc::new(scdawg.find("caf").expect("substring handle exists"));
    let expected_final = handle.is_final();
    let expected_edges = handle.edge_count();
    let barrier = Arc::new(Barrier::new(8));

    let workers: Vec<_> = (0..8)
        .map(|_| {
            let handle = Arc::clone(&handle);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                for _ in 0..64 {
                    assert_eq!(handle.is_final(), expected_final);
                    assert_eq!(handle.edge_count(), expected_edges);
                    let _ = handle.edges().count();
                }
            })
        })
        .collect();

    for worker in workers {
        worker.join().expect("reader thread completes");
    }
}

#[cfg(feature = "persistent-artrie")]
mod persistent_buffer_contracts {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use libdictenstein::persistent_artrie::{
        BlockStorage, BufferManager, FileHeader, PersistentARTrieError, Result, BLOCK_SIZE,
    };

    #[derive(Clone)]
    struct NonFixedStorage {
        blocks: Arc<Mutex<HashMap<u32, Vec<u8>>>>,
        next_block: Arc<AtomicU32>,
        fixed_batch_writes: Arc<AtomicUsize>,
    }

    impl NonFixedStorage {
        fn new() -> Self {
            let mut blocks = HashMap::new();
            blocks.insert(0, vec![0; BLOCK_SIZE]);

            Self {
                blocks: Arc::new(Mutex::new(blocks)),
                next_block: Arc::new(AtomicU32::new(1)),
                fixed_batch_writes: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn ensure_block(&self, block_id: u32) {
            self.blocks
                .lock()
                .expect("blocks lock")
                .entry(block_id)
                .or_insert_with(|| vec![0; BLOCK_SIZE]);
        }
    }

    impl BlockStorage for NonFixedStorage {
        fn read_block(&self, block_id: u32, buffer: &mut [u8; BLOCK_SIZE]) -> Result<()> {
            let blocks = self.blocks.lock().expect("blocks lock");
            let block = blocks.get(&block_id).ok_or_else(|| {
                PersistentARTrieError::internal(format!("missing block {block_id}"))
            })?;
            buffer.copy_from_slice(block);
            Ok(())
        }

        fn write_block(&self, block_id: u32, buffer: &[u8; BLOCK_SIZE]) -> Result<()> {
            self.ensure_block(block_id);
            self.blocks
                .lock()
                .expect("blocks lock")
                .insert(block_id, buffer.to_vec());
            Ok(())
        }

        fn read_bytes(&self, block_id: u32, offset: usize, buffer: &mut [u8]) -> Result<()> {
            if offset + buffer.len() > BLOCK_SIZE {
                return Err(PersistentARTrieError::internal("read range outside block"));
            }

            let blocks = self.blocks.lock().expect("blocks lock");
            let block = blocks.get(&block_id).ok_or_else(|| {
                PersistentARTrieError::internal(format!("missing block {block_id}"))
            })?;
            buffer.copy_from_slice(&block[offset..offset + buffer.len()]);
            Ok(())
        }

        fn write_bytes(&self, block_id: u32, offset: usize, data: &[u8]) -> Result<()> {
            if offset + data.len() > BLOCK_SIZE {
                return Err(PersistentARTrieError::internal("write range outside block"));
            }

            self.ensure_block(block_id);
            let mut blocks = self.blocks.lock().expect("blocks lock");
            let block = blocks.get_mut(&block_id).expect("ensured block exists");
            block[offset..offset + data.len()].copy_from_slice(data);
            Ok(())
        }

        fn allocate_block(&self) -> Result<u32> {
            let block_id = self.next_block.fetch_add(1, Ordering::AcqRel);
            self.ensure_block(block_id);
            Ok(block_id)
        }

        fn free_block(&self, block_id: u32) -> Result<()> {
            self.blocks.lock().expect("blocks lock").remove(&block_id);
            Ok(())
        }

        fn read_header(&self) -> Result<FileHeader> {
            Ok(FileHeader::new())
        }

        fn write_header(&self, _header: &FileHeader) -> Result<()> {
            Ok(())
        }

        fn read_header_bytes(&self, buffer: &mut [u8]) -> Result<()> {
            self.read_bytes(0, 0, buffer)
        }

        fn write_header_bytes(&self, bytes: &[u8]) -> Result<()> {
            self.write_bytes(0, 0, bytes)
        }

        fn root_ptr(&self) -> Result<u64> {
            Ok(0)
        }

        fn set_root_ptr(&self, _ptr: u64) -> Result<()> {
            Ok(())
        }

        fn entry_count(&self) -> Result<u64> {
            Ok(0)
        }

        fn set_entry_count(&self, _count: u64) -> Result<()> {
            Ok(())
        }

        fn file_size(&self) -> u64 {
            u64::from(self.next_block.load(Ordering::Acquire)) * BLOCK_SIZE as u64
        }

        fn block_count(&self) -> Result<u32> {
            Ok(self.next_block.load(Ordering::Acquire))
        }

        fn path(&self) -> &str {
            "non-fixed-contract-storage"
        }

        fn sync(&self) -> Result<()> {
            Ok(())
        }

        fn write_blocks_batch_fixed(
            &self,
            _requests: &[(u32, &[u8; BLOCK_SIZE], u16)],
        ) -> Result<()> {
            self.fixed_batch_writes.fetch_add(1, Ordering::AcqRel);
            Err(PersistentARTrieError::internal(
                "fixed-buffer path used without backend support",
            ))
        }
    }

    #[test]
    fn buffer_manager_requires_backend_fixed_buffer_capability_after_registration() {
        let storage = NonFixedStorage::new();
        let fixed_batch_writes = Arc::clone(&storage.fixed_batch_writes);
        let manager = BufferManager::new(storage, 1);

        {
            let mut page = manager.new_page().expect("allocate page");
            page.data_mut()[..8].copy_from_slice(b"nofixed!");
        }

        manager
            .flush_all()
            .expect("non-fixed backend must use normal batch write path");
        assert_eq!(
            fixed_batch_writes.load(Ordering::Acquire),
            0,
            "BufferManager used fixed-buffer writes even though the backend did not support them"
        );
    }
}
