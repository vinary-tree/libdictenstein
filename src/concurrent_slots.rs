//! Append-only, non-blocking storage for immutable snapshot objects.
//!
//! A slot is published at most once and is never removed before the directory
//! itself is dropped. That lifetime rule eliminates ABA from slot publication
//! and lets both dense producer arenas and sparse foreign-ID caches share the
//! same atomic slot primitive.

use crate::nonblocking::CasBackoff;
use arc_swap::{ArcSwap, ArcSwapOption};
use std::marker::PhantomData;
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::Arc;

/// An immutable value that is atomically published at most once.
///
/// Reads are a single acquire load and do not modify reference counts. Losing
/// publishers reclaim their unpublished candidate immediately. The winning
/// allocation remains stable until exclusive `Drop`, so returned references
/// cannot observe removal or ABA.
///
/// The ownership markers deliberately make this type `Send` only for `T:
/// Send`, and `Sync` only for `T: Send + Sync`: publication may transfer a
/// value to the dropping thread, while `get` also shares it by reference.
///
/// ```compile_fail
/// use libdictenstein::concurrent_slots::AtomicOnceBox;
/// use std::sync::MutexGuard;
/// fn assert_sync<T: Sync>() {}
/// // MutexGuard is Sync but not Send, so publishing it into a shared cell
/// // would permit an invalid cross-thread ownership transfer.
/// assert_sync::<AtomicOnceBox<MutexGuard<'static, ()>>>();
/// ```
pub struct AtomicOnceBox<T> {
    pointer: AtomicPtr<T>,
    ownership: PhantomData<Box<T>>,
    thread_transfer: PhantomData<std::sync::Mutex<T>>,
}

/// A single-value non-blocking mailbox with owned take semantics.
///
/// Publishers race to fill an empty mailbox; a taker atomically removes and
/// owns the value. This is intended for rare control-plane events whose empty
/// fast path must not acquire a mutex (for example, callback fault delivery).
/// Its ownership marker requires `T: Send` before the mailbox can be sent or
/// shared because `take` may move the payload to a different thread.
///
/// ```compile_fail
/// use libdictenstein::concurrent_slots::AtomicTakeBox;
/// use std::sync::MutexGuard;
/// fn assert_sync<T: Sync>() {}
/// assert_sync::<AtomicTakeBox<MutexGuard<'static, ()>>>();
/// ```
pub struct AtomicTakeBox<T> {
    pointer: AtomicPtr<T>,
    thread_transfer: PhantomData<std::sync::Mutex<T>>,
}

impl<T> AtomicTakeBox<T> {
    /// Construct an empty mailbox.
    pub const fn new() -> Self {
        Self {
            pointer: AtomicPtr::new(ptr::null_mut()),
            thread_transfer: PhantomData,
        }
    }

    /// Publish `value` only when the mailbox is empty.
    ///
    /// Returns `true` for the published value. A losing value is reclaimed on
    /// the caller's thread.
    #[inline]
    pub fn publish_if_empty(&self, value: T) -> bool {
        let candidate = Box::into_raw(Box::new(value));
        match self.pointer.compare_exchange(
            ptr::null_mut(),
            candidate,
            Ordering::Release,
            Ordering::Acquire,
        ) {
            Ok(_) => true,
            Err(_) => {
                // SAFETY: a failed CAS never published this candidate.
                unsafe { drop(Box::from_raw(candidate)) };
                false
            }
        }
    }

    /// Atomically remove and return the current value.
    #[inline]
    pub fn take(&self) -> Option<T> {
        let mut pointer = self.pointer.load(Ordering::Acquire);
        loop {
            if pointer.is_null() {
                return None;
            }
            match self.pointer.compare_exchange_weak(
                pointer,
                ptr::null_mut(),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    // SAFETY: the successful CAS transfers the published
                    // Box's sole ownership to this caller.
                    return Some(*unsafe { Box::from_raw(pointer) });
                }
                Err(observed) => pointer = observed,
            }
        }
    }
}

impl<T> Default for AtomicTakeBox<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Drop for AtomicTakeBox<T> {
    fn drop(&mut self) {
        let pointer = *self.pointer.get_mut();
        if !pointer.is_null() {
            // SAFETY: exclusive drop owns the remaining published Box.
            unsafe { drop(Box::from_raw(pointer)) };
        }
    }
}

impl<T> AtomicOnceBox<T> {
    /// Construct an empty publication cell.
    pub const fn new() -> Self {
        Self {
            pointer: AtomicPtr::new(ptr::null_mut()),
            ownership: PhantomData,
            thread_transfer: PhantomData,
        }
    }

    /// Load the published immutable value, if any.
    #[inline]
    pub fn get(&self) -> Option<&T> {
        // SAFETY: successful publication transfers one Box into `pointer`.
        // The pointer is never changed or freed until exclusive `Drop`, so a
        // reference borrowed through `&self` remains valid for that borrow.
        unsafe { self.pointer.load(Ordering::Acquire).as_ref() }
    }

    /// Publish `value` if empty and return the canonical value.
    #[inline]
    pub fn publish_if_absent(&self, value: T) -> &T {
        let candidate = Box::into_raw(Box::new(value));
        let canonical = match self.pointer.compare_exchange(
            ptr::null_mut(),
            candidate,
            Ordering::Release,
            Ordering::Acquire,
        ) {
            Ok(_) => candidate,
            Err(existing) => {
                // SAFETY: this CAS candidate was never published, and this
                // thread still has its unique ownership.
                unsafe { drop(Box::from_raw(candidate)) };
                existing
            }
        };
        // SAFETY: `canonical` is the stable winner described by `get`.
        unsafe { &*canonical }
    }
}

impl<T> Default for AtomicOnceBox<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Drop for AtomicOnceBox<T> {
    fn drop(&mut self) {
        let pointer = *self.pointer.get_mut();
        if !pointer.is_null() {
            // SAFETY: exclusive drop owns the sole Box transferred by the
            // successful publisher; the pointer was never replaced.
            unsafe { drop(Box::from_raw(pointer)) };
        }
    }
}

/// Common interface implemented by append-only slot directories.
pub trait ArcSlotDirectory<T> {
    /// Load a published slot.
    fn get(&self, index: u64) -> Option<Arc<T>>;

    /// Publish `value` if the slot is empty and return the canonical winner.
    fn install_if_absent(&self, index: u64, value: Arc<T>) -> Arc<T>;
}

/// A dense slot directory cannot represent or allocate the requested index.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DenseSlotCapacityError;

impl std::fmt::Display for DenseSlotCapacityError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("dense slot directory capacity exhausted")
    }
}

impl std::error::Error for DenseSlotCapacityError {}

/// A fixed group of independently published immutable values.
pub struct SlotChunk<T, const N: usize> {
    slots: [ArcSwapOption<T>; N],
}

impl<T, const N: usize> SlotChunk<T, N> {
    fn new() -> Self {
        assert!(N != 0, "slot chunks must contain at least one slot");
        Self {
            slots: std::array::from_fn(|_| ArcSwapOption::empty()),
        }
    }

    #[inline]
    fn get(&self, offset: usize) -> Option<Arc<T>> {
        self.slots[offset].load_full()
    }

    #[inline]
    fn install_if_absent(&self, offset: usize, value: Arc<T>) -> Arc<T> {
        let previous =
            self.slots[offset].compare_and_swap(&None::<Arc<T>>, Some(Arc::clone(&value)));
        previous.as_ref().cloned().unwrap_or(value)
    }
}

/// Dense append-only directory for monotonically assigned identifiers.
///
/// Reads require one atomic directory load followed by one atomic slot load.
/// Growth copies only the directory of chunk `Arc`s and publishes it with CAS;
/// the geometrically grown directory has less than 2x capacity slack.
pub struct DenseArcSlots<T, const N: usize> {
    chunks: ArcSwap<Vec<Arc<SlotChunk<T, N>>>>,
}

impl<T, const N: usize> DenseArcSlots<T, N> {
    /// Construct a directory with one allocated chunk.
    pub fn new() -> Self {
        Self {
            chunks: ArcSwap::from_pointee(vec![Arc::new(SlotChunk::new())]),
        }
    }

    /// Ensure `index` has an addressable slot.
    pub fn ensure(&self, index: u64) -> Result<(), DenseSlotCapacityError> {
        let index = usize::try_from(index).map_err(|_| DenseSlotCapacityError)?;
        let required = index
            .checked_div(N)
            .and_then(|chunk| chunk.checked_add(1))
            .ok_or(DenseSlotCapacityError)?;
        let mut backoff = CasBackoff::new();
        loop {
            let current = self.chunks.load_full();
            if current.len() >= required {
                return Ok(());
            }
            let doubled = current.len().checked_mul(2).ok_or(DenseSlotCapacityError)?;
            let new_len = required.max(doubled);
            let mut grown = Vec::new();
            grown
                .try_reserve_exact(new_len)
                .map_err(|_| DenseSlotCapacityError)?;
            grown.extend(current.iter().cloned());
            grown.extend((current.len()..new_len).map(|_| Arc::new(SlotChunk::new())));
            let previous = self.chunks.compare_and_swap(&current, Arc::new(grown));
            if Arc::ptr_eq(&previous, &current) {
                return Ok(());
            }
            backoff.snooze();
        }
    }

    /// Number of addressable slots, including growth slack.
    pub fn capacity(&self) -> usize {
        self.chunks.load().len().saturating_mul(N)
    }

    /// Remove the directory's ownership of all chunks.
    ///
    /// This is intended for deterministic teardown instrumentation. Callers
    /// must already have exclusive lifecycle ownership; append-only lookup and
    /// publication are otherwise defined only before teardown begins.
    #[cfg(feature = "bindings-core")]
    pub(crate) fn clear(&mut self) {
        self.chunks.store(Arc::new(Vec::new()));
    }
}

impl<T, const N: usize> Default for DenseArcSlots<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const N: usize> ArcSlotDirectory<T> for DenseArcSlots<T, N> {
    #[inline]
    fn get(&self, index: u64) -> Option<Arc<T>> {
        let index = usize::try_from(index).ok()?;
        let chunks = self.chunks.load();
        chunks.get(index / N)?.get(index % N)
    }

    #[inline]
    fn install_if_absent(&self, index: u64, value: Arc<T>) -> Arc<T> {
        self.ensure(index)
            .expect("dense append-only slot index exceeds address space");
        let index = usize::try_from(index).expect("validated dense slot index");
        let chunks = self.chunks.load();
        chunks[index / N].install_if_absent(index % N, value)
    }
}

struct SparseChunk<T, const N: usize> {
    id: u64,
    slots: SlotChunk<T, N>,
}

impl<T, const N: usize> SparseChunk<T, N> {
    fn new(id: u64) -> Self {
        Self {
            id,
            slots: SlotChunk::new(),
        }
    }
}

/// Sparse append-only directory for untrusted or non-contiguous identifiers.
///
/// Chunk IDs are distributed over `S` immutable sorted shard directories.
/// A hostile high ID therefore allocates one chunk, never a vector proportional
/// to the ID. Chunk creation is lock-free CAS; populated-slot reads do not CAS.
pub struct SparseArcSlots<T, const N: usize, const S: usize> {
    shards: [ArcSwap<Vec<Arc<SparseChunk<T, N>>>>; S],
}

struct SparseOnceChunk<T, const N: usize> {
    id: u64,
    next: AtomicPtr<SparseOnceChunk<T, N>>,
    slots: [AtomicOnceBox<T>; N],
}

struct OnceSlotChunk<T, const N: usize> {
    slots: [AtomicOnceBox<T>; N],
}

impl<T, const N: usize> OnceSlotChunk<T, N> {
    fn new() -> Self {
        Self {
            slots: std::array::from_fn(|_| AtomicOnceBox::new()),
        }
    }
}

impl<T, const N: usize> SparseOnceChunk<T, N> {
    fn new(id: u64) -> Self {
        Self {
            id,
            next: AtomicPtr::new(ptr::null_mut()),
            slots: std::array::from_fn(|_| AtomicOnceBox::new()),
        }
    }
}

/// Sparse append-only slots with borrowed, refcount-free reads.
///
/// Each shard is a CAS-published intrusive list of immutable pages. Pages and
/// populated slots are never removed, so reads need only acquire-load pointers
/// and may return references tied to the directory borrow. A hostile high ID
/// allocates one fixed-size page rather than a proportional vector.
pub struct SparseOnceBoxSlots<T, const N: usize, const S: usize> {
    heads: [AtomicPtr<SparseOnceChunk<T, N>>; S],
    ownership: PhantomData<Box<SparseOnceChunk<T, N>>>,
}

impl<T, const N: usize, const S: usize> SparseOnceBoxSlots<T, N, S> {
    /// Construct an empty sparse directory.
    pub fn new() -> Self {
        assert!(N != 0, "slot chunks must contain at least one slot");
        assert!(
            S != 0,
            "sparse slot directories must contain at least one shard"
        );
        Self {
            heads: std::array::from_fn(|_| AtomicPtr::new(ptr::null_mut())),
            ownership: PhantomData,
        }
    }

    #[inline]
    fn coordinates(index: u64) -> (u64, usize, usize) {
        let chunk_id = index / N as u64;
        let offset = (index % N as u64) as usize;
        let shard = (chunk_id % S as u64) as usize;
        (chunk_id, offset, shard)
    }

    #[inline]
    fn find_chunk(&self, chunk_id: u64, shard: usize) -> *mut SparseOnceChunk<T, N> {
        let mut current = self.heads[shard].load(Ordering::Acquire);
        while !current.is_null() {
            // SAFETY: pages are never removed before exclusive directory
            // drop. Acquire-loading a published page also publishes its ID,
            // next pointer, and slots.
            let chunk = unsafe { &*current };
            if chunk.id == chunk_id {
                return current;
            }
            current = chunk.next.load(Ordering::Relaxed);
        }
        ptr::null_mut()
    }

    fn find_or_create_chunk(&self, chunk_id: u64, shard: usize) -> *mut SparseOnceChunk<T, N> {
        let existing = self.find_chunk(chunk_id, shard);
        if !existing.is_null() {
            return existing;
        }

        let candidate = Box::into_raw(Box::new(SparseOnceChunk::new(chunk_id)));
        let mut backoff = CasBackoff::new();
        loop {
            let head = self.heads[shard].load(Ordering::Acquire);
            let mut current = head;
            while !current.is_null() {
                // SAFETY: same append-only page invariant as `find_chunk`.
                let chunk = unsafe { &*current };
                if chunk.id == chunk_id {
                    // SAFETY: `candidate` has not been published.
                    unsafe { drop(Box::from_raw(candidate)) };
                    return current;
                }
                current = chunk.next.load(Ordering::Relaxed);
            }

            // SAFETY: the candidate remains uniquely owned until its CAS
            // succeeds, so its publication link may be updated after a loss.
            unsafe { (*candidate).next.store(head, Ordering::Relaxed) };
            if self.heads[shard]
                .compare_exchange(head, candidate, Ordering::Release, Ordering::Acquire)
                .is_ok()
            {
                return candidate;
            }
            backoff.snooze();
        }
    }

    /// Load one published value without incrementing a reference count.
    #[inline]
    pub fn get(&self, index: u64) -> Option<&T> {
        let (chunk_id, offset, shard) = Self::coordinates(index);
        let chunk = self.find_chunk(chunk_id, shard);
        if chunk.is_null() {
            return None;
        }
        // SAFETY: the append-only page remains alive for the `&self` borrow.
        unsafe { (*chunk).slots[offset].get() }
    }

    /// Publish one value if absent and return the canonical value.
    #[inline]
    pub fn install_if_absent(&self, index: u64, value: T) -> &T {
        let (chunk_id, offset, shard) = Self::coordinates(index);
        let chunk = self.find_or_create_chunk(chunk_id, shard);
        // SAFETY: the returned page is published in this append-only directory
        // and remains alive for the `&self` borrow.
        unsafe { (*chunk).slots[offset].publish_if_absent(value) }
    }

    /// Number of allocated pages across all shards.
    pub fn allocated_chunks(&self) -> usize {
        self.heads
            .iter()
            .map(|head| {
                let mut count = 0usize;
                let mut current = head.load(Ordering::Acquire);
                while !current.is_null() {
                    count = count.saturating_add(1);
                    // SAFETY: pages are append-only during shared access.
                    current = unsafe { (*current).next.load(Ordering::Relaxed) };
                }
                count
            })
            .sum()
    }
}

impl<T, const N: usize, const S: usize> Default for SparseOnceBoxSlots<T, N, S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const N: usize, const S: usize> Drop for SparseOnceBoxSlots<T, N, S> {
    fn drop(&mut self) {
        for head in &mut self.heads {
            let mut current = *head.get_mut();
            while !current.is_null() {
                // SAFETY: exclusive directory drop owns every Box linked from
                // its heads, and no page is linked more than once.
                let mut chunk = unsafe { Box::from_raw(current) };
                current = *chunk.next.get_mut();
                drop(chunk);
            }
        }
    }
}

/// Lock-free once-box slots with an O(1) dense prefix and sparse overflow.
///
/// Ordinary monotone node IDs use one fixed directory lookup, one lazy page
/// lookup, and one slot lookup. Only the small directory of `D` page cells is
/// allocated eagerly; each `N`-slot page is allocated on first publication.
/// IDs beyond the dense prefix delegate to [`SparseOnceBoxSlots`], preserving
/// bounded memory for hostile or provider-chosen `u64` identifiers.
pub struct HybridOnceBoxSlots<T, const N: usize, const D: usize, const S: usize> {
    dense: [AtomicOnceBox<OnceSlotChunk<T, N>>; D],
    overflow: SparseOnceBoxSlots<T, N, S>,
}

impl<T, const N: usize, const D: usize, const S: usize> HybridOnceBoxSlots<T, N, D, S> {
    /// Construct an empty hybrid directory.
    pub fn new() -> Self {
        assert!(N != 0, "slot chunks must contain at least one slot");
        assert!(D != 0, "hybrid directories must contain dense pages");
        Self {
            dense: std::array::from_fn(|_| AtomicOnceBox::new()),
            overflow: SparseOnceBoxSlots::new(),
        }
    }

    #[inline]
    fn dense_coordinates(index: u64) -> Option<(usize, usize)> {
        let dense_limit = (D as u64).checked_mul(N as u64)?;
        if index >= dense_limit {
            return None;
        }
        let index = usize::try_from(index).ok()?;
        Some((index / N, index % N))
    }

    /// Load one published value without incrementing a reference count.
    #[inline]
    pub fn get(&self, index: u64) -> Option<&T> {
        if let Some((page, offset)) = Self::dense_coordinates(index) {
            return self.dense[page].get()?.slots[offset].get();
        }
        self.overflow.get(index)
    }

    /// Publish one value if absent and return the canonical value.
    #[inline]
    pub fn install_if_absent(&self, index: u64, value: T) -> &T {
        if let Some((page, offset)) = Self::dense_coordinates(index) {
            let page = self.dense[page].publish_if_absent(OnceSlotChunk::new());
            return page.slots[offset].publish_if_absent(value);
        }
        self.overflow.install_if_absent(index, value)
    }

    /// Number of lazily allocated dense pages.
    pub fn allocated_dense_chunks(&self) -> usize {
        self.dense
            .iter()
            .filter(|page| page.get().is_some())
            .count()
    }

    /// Number of sparse overflow pages.
    pub fn allocated_overflow_chunks(&self) -> usize {
        self.overflow.allocated_chunks()
    }
}

impl<T, const N: usize, const D: usize, const S: usize> Default for HybridOnceBoxSlots<T, N, D, S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const N: usize, const S: usize> SparseArcSlots<T, N, S> {
    /// Construct an empty sparse directory.
    pub fn new() -> Self {
        assert!(N != 0, "slot chunks must contain at least one slot");
        assert!(
            S != 0,
            "sparse slot directories must contain at least one shard"
        );
        Self {
            shards: std::array::from_fn(|_| ArcSwap::from_pointee(Vec::new())),
        }
    }

    #[inline]
    fn coordinates(index: u64) -> (u64, usize, usize) {
        let chunk_id = index / N as u64;
        let offset = (index % N as u64) as usize;
        let shard = (chunk_id % S as u64) as usize;
        (chunk_id, offset, shard)
    }

    fn find_or_create_chunk(&self, chunk_id: u64, shard: usize) -> Arc<SparseChunk<T, N>> {
        let mut backoff = CasBackoff::new();
        loop {
            let current = self.shards[shard].load_full();
            match current.binary_search_by_key(&chunk_id, |chunk| chunk.id) {
                Ok(position) => return Arc::clone(&current[position]),
                Err(position) => {
                    let candidate = Arc::new(SparseChunk::new(chunk_id));
                    let mut grown = Vec::with_capacity(current.len().saturating_add(1));
                    grown.extend(current[..position].iter().cloned());
                    grown.push(Arc::clone(&candidate));
                    grown.extend(current[position..].iter().cloned());
                    let previous = self.shards[shard].compare_and_swap(&current, Arc::new(grown));
                    if Arc::ptr_eq(&previous, &current) {
                        return candidate;
                    }
                    backoff.snooze();
                }
            }
        }
    }

    /// Number of allocated chunks across all shards.
    pub fn allocated_chunks(&self) -> usize {
        self.shards.iter().map(|shard| shard.load().len()).sum()
    }
}

impl<T, const N: usize, const S: usize> Default for SparseArcSlots<T, N, S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const N: usize, const S: usize> ArcSlotDirectory<T> for SparseArcSlots<T, N, S> {
    #[inline]
    fn get(&self, index: u64) -> Option<Arc<T>> {
        let (chunk_id, offset, shard) = Self::coordinates(index);
        let chunks = self.shards[shard].load();
        let position = chunks
            .binary_search_by_key(&chunk_id, |chunk| chunk.id)
            .ok()?;
        chunks[position].slots.get(offset)
    }

    #[inline]
    fn install_if_absent(&self, index: u64, value: Arc<T>) -> Arc<T> {
        let (chunk_id, offset, shard) = Self::coordinates(index);
        self.find_or_create_chunk(chunk_id, shard)
            .slots
            .install_if_absent(offset, value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Barrier;
    use std::thread;

    #[allow(dead_code)]
    fn append_only_cells_have_required_positive_auto_traits() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AtomicOnceBox<String>>();
        assert_send_sync::<AtomicTakeBox<String>>();
        assert_send_sync::<SparseOnceBoxSlots<String, 8, 4>>();
        assert_send_sync::<HybridOnceBoxSlots<String, 8, 4, 4>>();
    }

    #[test]
    fn dense_growth_preserves_every_published_slot() {
        let slots = Arc::new(DenseArcSlots::<u64, 4>::new());
        let threads = (0..8)
            .map(|thread_id| {
                let slots = Arc::clone(&slots);
                thread::spawn(move || {
                    for offset in 0..64u64 {
                        let index = thread_id * 64 + offset;
                        slots.install_if_absent(index, Arc::new(index));
                    }
                })
            })
            .collect::<Vec<_>>();
        for thread in threads {
            thread.join().expect("dense writer");
        }
        for index in 0..512u64 {
            assert_eq!(*slots.get(index).expect("published dense slot"), index);
        }
        assert!(slots.capacity() < 1024);
    }

    #[test]
    fn dense_growth_rejects_unrepresentable_capacity_without_mutation() {
        let slots = DenseArcSlots::<u64, 4>::new();
        let original_capacity = slots.capacity();
        assert_eq!(slots.ensure(u64::MAX), Err(DenseSlotCapacityError));
        assert_eq!(slots.capacity(), original_capacity);
        assert!(slots.get(u64::MAX).is_none());
    }

    #[test]
    fn sparse_high_ids_allocate_only_their_chunks() {
        let slots = SparseArcSlots::<u64, 8, 4>::new();
        slots.install_if_absent(u64::MAX, Arc::new(9));
        slots.install_if_absent(1, Arc::new(3));
        assert_eq!(*slots.get(u64::MAX).expect("high slot"), 9);
        assert_eq!(*slots.get(1).expect("low slot"), 3);
        assert_eq!(slots.allocated_chunks(), 2);
    }

    #[test]
    fn concurrent_same_slot_returns_one_canonical_arc() {
        let slots = Arc::new(SparseArcSlots::<usize, 8, 4>::new());
        let barrier = Arc::new(Barrier::new(8));
        let threads = (0..8)
            .map(|value| {
                let slots = Arc::clone(&slots);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    slots.install_if_absent(42, Arc::new(value))
                })
            })
            .collect::<Vec<_>>();
        let winners = threads
            .into_iter()
            .map(|thread| thread.join().expect("sparse writer"))
            .collect::<Vec<_>>();
        assert!(winners
            .windows(2)
            .all(|pair| Arc::ptr_eq(&pair[0], &pair[1])));
    }

    #[test]
    fn sparse_once_slots_are_canonical_and_bounded_for_high_ids() {
        let slots = Arc::new(SparseOnceBoxSlots::<usize, 8, 4>::new());
        let barrier = Arc::new(Barrier::new(8));
        let threads = (0..8)
            .map(|value| {
                let slots = Arc::clone(&slots);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    slots.install_if_absent(u64::MAX, value) as *const usize as usize
                })
            })
            .collect::<Vec<_>>();
        let winners = threads
            .into_iter()
            .map(|thread| thread.join().expect("sparse once-box writer"))
            .collect::<Vec<_>>();
        assert!(winners.windows(2).all(|pair| pair[0] == pair[1]));
        assert!(slots.get(u64::MAX).is_some_and(|winner| *winner < 8));
        assert_eq!(slots.allocated_chunks(), 1);

        slots.install_if_absent(1, 9);
        assert_eq!(slots.get(1), Some(&9));
        assert_eq!(slots.allocated_chunks(), 2);
    }

    #[test]
    fn hybrid_slots_use_dense_pages_and_bounded_sparse_overflow() {
        let slots = HybridOnceBoxSlots::<usize, 8, 4, 4>::new();
        assert_eq!(slots.install_if_absent(0, 1), &1);
        assert_eq!(slots.install_if_absent(31, 2), &2);
        assert_eq!(slots.install_if_absent(0, 3), &1);
        assert_eq!(slots.get(0), Some(&1));
        assert_eq!(slots.get(31), Some(&2));
        assert_eq!(slots.allocated_dense_chunks(), 2);
        assert_eq!(slots.allocated_overflow_chunks(), 0);

        assert_eq!(slots.install_if_absent(u64::MAX, 4), &4);
        assert_eq!(slots.get(u64::MAX), Some(&4));
        assert_eq!(slots.allocated_overflow_chunks(), 1);
    }

    #[test]
    fn atomic_once_box_publishes_one_value_and_reclaims_every_candidate() {
        struct Counted<'a> {
            value: usize,
            drops: &'a AtomicUsize,
        }
        impl Drop for Counted<'_> {
            fn drop(&mut self) {
                self.drops.fetch_add(1, Ordering::Relaxed);
            }
        }

        let drops = AtomicUsize::new(0);
        thread::scope(|scope| {
            let cell = Arc::new(AtomicOnceBox::new());
            let barrier = Arc::new(Barrier::new(8));
            let threads = (0..8)
                .map(|value| {
                    let cell = Arc::clone(&cell);
                    let barrier = Arc::clone(&barrier);
                    let drops = &drops;
                    scope.spawn(move || {
                        barrier.wait();
                        cell.publish_if_absent(Counted { value, drops }) as *const Counted<'_>
                            as usize
                    })
                })
                .collect::<Vec<_>>();
            let winners = threads
                .into_iter()
                .map(|thread| thread.join().expect("once-box publisher"))
                .collect::<Vec<_>>();
            assert!(winners.windows(2).all(|pair| pair[0] == pair[1]));
            assert!(cell.get().is_some_and(|winner| winner.value < 8));
            assert_eq!(drops.load(Ordering::Relaxed), 7);
        });
        assert_eq!(drops.load(Ordering::Relaxed), 8);
    }

    #[test]
    fn raw_box_ownership_paths_reclaim_exactly_once() {
        struct Counted<'a>(&'a AtomicUsize);
        impl Drop for Counted<'_> {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }

        let drops = AtomicUsize::new(0);
        {
            let once = AtomicOnceBox::new();
            once.publish_if_absent(Counted(&drops));
            once.publish_if_absent(Counted(&drops));
            assert_eq!(drops.load(Ordering::Relaxed), 1);

            let mailbox = AtomicTakeBox::new();
            assert!(mailbox.publish_if_empty(Counted(&drops)));
            assert!(!mailbox.publish_if_empty(Counted(&drops)));
            assert_eq!(drops.load(Ordering::Relaxed), 2);
            drop(mailbox.take());
            assert_eq!(drops.load(Ordering::Relaxed), 3);

            let sparse = SparseOnceBoxSlots::<Counted<'_>, 2, 2>::new();
            sparse.install_if_absent(0, Counted(&drops));
            sparse.install_if_absent(u64::MAX, Counted(&drops));
            assert!(sparse.get(0).is_some());
            assert!(sparse.get(u64::MAX).is_some());
        }
        assert_eq!(drops.load(Ordering::Relaxed), 6);
    }

    #[test]
    fn atomic_take_box_supports_publish_take_republish_cycles() {
        const COUNT: usize = 1_000;
        let mailbox = Arc::new(AtomicTakeBox::new());
        thread::scope(|scope| {
            let publisher = Arc::clone(&mailbox);
            scope.spawn(move || {
                for value in 0..COUNT {
                    let mut pending = value;
                    loop {
                        if publisher.publish_if_empty(pending) {
                            break;
                        }
                        pending = value;
                        thread::yield_now();
                    }
                }
            });
            let taker = Arc::clone(&mailbox);
            scope.spawn(move || {
                for expected in 0..COUNT {
                    loop {
                        if let Some(value) = taker.take() {
                            assert_eq!(value, expected);
                            break;
                        }
                        thread::yield_now();
                    }
                }
            });
        });
        assert!(mailbox.take().is_none());
    }

    #[test]
    fn atomic_take_box_delivers_each_publication_to_one_taker() {
        let mailbox = Arc::new(AtomicTakeBox::new());
        assert_eq!(mailbox.take(), None);
        assert!(mailbox.publish_if_empty(7));
        assert!(!mailbox.publish_if_empty(9));

        let barrier = Arc::new(Barrier::new(8));
        let threads = (0..8)
            .map(|_| {
                let mailbox = Arc::clone(&mailbox);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    mailbox.take()
                })
            })
            .collect::<Vec<_>>();
        let delivered = threads
            .into_iter()
            .filter_map(|thread| thread.join().expect("mailbox taker"))
            .collect::<Vec<_>>();
        assert_eq!(delivered, vec![7]);
        assert!(mailbox.publish_if_empty(11));
        assert_eq!(mailbox.take(), Some(11));
    }
}
