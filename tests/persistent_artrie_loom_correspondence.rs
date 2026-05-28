//! Bounded schedule checks for the byte lock-free ARTrie publication protocol.
//!
//! These are deliberately small loom models. They mirror the root-slot CAS,
//! cache publication, child handoff, and merge snapshot rules documented in
//! `LockFreeARTrieLinearizability.tla` without changing the production
//! implementation to depend on loom primitives.

#![cfg(feature = "persistent-artrie")]

use loom::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use loom::sync::{Arc, RwLock};
use loom::thread;

#[derive(Debug)]
struct ModelNode {
    contains_key: bool,
}

impl ModelNode {
    fn empty() -> Self {
        Self {
            contains_key: false,
        }
    }

    fn with_key() -> Self {
        Self { contains_key: true }
    }
}

#[derive(Debug)]
struct ModelRootSlot {
    ptr: RwLock<Option<Arc<ModelNode>>>,
}

impl ModelRootSlot {
    fn new(node: Arc<ModelNode>) -> Self {
        Self {
            ptr: RwLock::new(Some(node)),
        }
    }

    fn load(&self) -> Option<Arc<ModelNode>> {
        self.ptr.read().expect("root read lock").clone()
    }

    fn compare_exchange(
        &self,
        expected: &Arc<ModelNode>,
        new: Arc<ModelNode>,
    ) -> Result<Arc<ModelNode>, Arc<ModelNode>> {
        let mut guard = self.ptr.write().expect("root write lock");

        match guard.as_ref() {
            Some(current) if Arc::ptr_eq(current, expected) => {
                let old = Arc::clone(current);
                *guard = Some(new);
                Ok(old)
            }
            Some(current) => Err(Arc::clone(current)),
            None => Err(Arc::new(ModelNode::empty())),
        }
    }
}

fn insert_one_key(root: &ModelRootSlot, cache: &AtomicBool) -> bool {
    if cache.load(Ordering::Acquire) {
        return false;
    }

    loop {
        let current = root.load().expect("model root is initialized");

        if current.contains_key {
            cache.store(true, Ordering::Release);
            return false;
        }

        let new_root = Arc::new(ModelNode::with_key());
        match root.compare_exchange(&current, new_root) {
            Ok(_) => {
                cache.store(true, Ordering::Release);
                return true;
            }
            Err(_) => thread::yield_now(),
        }
    }
}

fn contains_one_key(root: &ModelRootSlot, cache: &AtomicBool) -> bool {
    cache.load(Ordering::Acquire) || root.load().expect("model root is initialized").contains_key
}

fn merge_once(cache: &AtomicBool, persisted: &AtomicBool) {
    if cache.load(Ordering::Acquire) {
        persisted.store(true, Ordering::Release);
    }
}

#[derive(Debug)]
struct ModelValueLeaf {
    value: AtomicUsize,
}

#[derive(Debug)]
struct ModelValueOverlay {
    leaf: RwLock<Option<Arc<ModelValueLeaf>>>,
    cache: AtomicBool,
    leaf_initializations: AtomicUsize,
}

impl ModelValueOverlay {
    fn new() -> Self {
        Self {
            leaf: RwLock::new(None),
            cache: AtomicBool::new(false),
            leaf_initializations: AtomicUsize::new(0),
        }
    }

    fn get_or_create_leaf(&self) -> Arc<ModelValueLeaf> {
        if let Some(leaf) = self.leaf.read().expect("value leaf read lock").clone() {
            return leaf;
        }

        let candidate = Arc::new(ModelValueLeaf {
            value: AtomicUsize::new(0),
        });
        let mut guard = self.leaf.write().expect("value leaf write lock");

        match guard.as_ref() {
            Some(existing) => Arc::clone(existing),
            None => {
                self.leaf_initializations.fetch_add(1, Ordering::SeqCst);
                *guard = Some(Arc::clone(&candidate));
                self.cache.store(true, Ordering::Release);
                candidate
            }
        }
    }

    fn increment(&self, delta: usize) -> usize {
        self.try_increment_checked(delta, usize::MAX)
            .expect("model increment overflow")
    }

    fn try_increment_checked(&self, delta: usize, max_value: usize) -> Result<usize, ()> {
        let leaf = self.get_or_create_leaf();
        loop {
            let current = leaf.value.load(Ordering::Acquire);
            let new_value = current.checked_add(delta).ok_or(())?;
            if new_value > max_value {
                return Err(());
            }
            match leaf.value.compare_exchange(
                current,
                new_value,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(new_value),
                Err(_) => thread::yield_now(),
            }
        }
    }

    fn get_value(&self) -> Option<usize> {
        self.leaf
            .read()
            .expect("value leaf read lock")
            .as_ref()
            .map(|leaf| leaf.value.load(Ordering::Acquire))
    }

    fn merge_snapshot(&self, persisted: &AtomicUsize) {
        if let Some(value) = self.get_value() {
            persisted.store(value, Ordering::Release);
        }
    }

    fn try_merge_snapshot_checked(
        &self,
        persisted: &AtomicUsize,
        max_value: usize,
    ) -> Result<(), ()> {
        if let Some(value) = self.get_value() {
            let current = persisted.load(Ordering::Acquire);
            let new_value = current.checked_add(value).ok_or(())?;
            if new_value > max_value {
                return Err(());
            }
            persisted.store(new_value, Ordering::Release);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ModelVocabSnapshot {
    entries: [Option<usize>; 2],
}

impl ModelVocabSnapshot {
    fn empty() -> Self {
        Self {
            entries: [None, None],
        }
    }

    fn get(&self, term: usize) -> Option<usize> {
        self.entries[term]
    }

    fn set(&mut self, term: usize, index: usize) {
        self.entries[term] = Some(index);
    }
}

#[derive(Debug)]
struct ModelVocabOverlay {
    root: RwLock<ModelVocabSnapshot>,
    cache: RwLock<ModelVocabSnapshot>,
    persisted: RwLock<ModelVocabSnapshot>,
    next_index: AtomicUsize,
    entry_count: AtomicUsize,
}

impl ModelVocabOverlay {
    fn new() -> Self {
        Self {
            root: RwLock::new(ModelVocabSnapshot::empty()),
            cache: RwLock::new(ModelVocabSnapshot::empty()),
            persisted: RwLock::new(ModelVocabSnapshot::empty()),
            next_index: AtomicUsize::new(0),
            entry_count: AtomicUsize::new(0),
        }
    }

    fn root_snapshot(&self) -> ModelVocabSnapshot {
        *self.root.read().expect("vocab root read lock")
    }

    fn cache_get(&self, term: usize) -> Option<usize> {
        self.cache.read().expect("vocab cache read lock").get(term)
    }

    fn cache_set(&self, term: usize, index: usize) {
        self.cache
            .write()
            .expect("vocab cache write lock")
            .set(term, index);
    }

    fn compare_exchange_root(
        &self,
        expected: ModelVocabSnapshot,
        new: ModelVocabSnapshot,
    ) -> Result<ModelVocabSnapshot, ModelVocabSnapshot> {
        let mut guard = self.root.write().expect("vocab root write lock");

        if *guard == expected {
            let old = *guard;
            *guard = new;
            Ok(old)
        } else {
            Err(*guard)
        }
    }

    fn insert(&self, term: usize) -> usize {
        if let Some(index) = self.cache_get(term) {
            return index;
        }

        if let Some(index) = self.root_snapshot().get(term) {
            self.cache_set(term, index);
            return index;
        }

        let claimed = self.next_index.fetch_add(1, Ordering::AcqRel);

        loop {
            let current = self.root_snapshot();

            if let Some(existing) = current.get(term) {
                self.cache_set(term, existing);
                return existing;
            }

            let mut next = current;
            next.set(term, claimed);

            match self.compare_exchange_root(current, next) {
                Ok(_) => {
                    self.cache_set(term, claimed);
                    self.entry_count.fetch_add(1, Ordering::AcqRel);
                    return claimed;
                }
                Err(actual) => {
                    if let Some(existing) = actual.get(term) {
                        self.cache_set(term, existing);
                        return existing;
                    }

                    thread::yield_now();
                }
            }
        }
    }

    fn merge_snapshot(&self) {
        let cache = *self.cache.read().expect("vocab cache read lock");
        let mut persisted = self.persisted.write().expect("vocab persisted write lock");

        for term in 0..cache.entries.len() {
            if let Some(index) = cache.get(term) {
                persisted.set(term, index);
            }
        }
    }

    fn persisted_get(&self, term: usize) -> Option<usize> {
        self.persisted
            .read()
            .expect("vocab persisted read lock")
            .get(term)
    }
}

#[derive(Debug)]
struct ModelDurabilityFrontier {
    next_lsn: AtomicUsize,
    durable: RwLock<[bool; 4]>,
    synced_lsn: AtomicUsize,
}

impl ModelDurabilityFrontier {
    fn new() -> Self {
        Self {
            next_lsn: AtomicUsize::new(1),
            durable: RwLock::new([false; 4]),
            synced_lsn: AtomicUsize::new(0),
        }
    }

    fn reserve_lsn(&self) -> usize {
        let lsn = self.next_lsn.fetch_add(1, Ordering::AcqRel);
        assert!(lsn < 4, "bounded model supports LSNs 1..=3");
        lsn
    }

    fn complete_lsn(&self, lsn: usize) {
        self.durable.write().expect("durable write lock")[lsn] = true;
    }

    fn publish_frontier(&self) {
        let durable = self.durable.read().expect("durable read lock");
        let mut candidate = 0;

        for lsn in 1..durable.len() {
            if durable[lsn] {
                candidate = lsn;
            } else {
                break;
            }
        }

        self.advance_synced_to(candidate);
    }

    fn advance_synced_to(&self, candidate: usize) {
        loop {
            let current = self.synced_lsn.load(Ordering::Acquire);
            if candidate <= current {
                return;
            }

            if self
                .synced_lsn
                .compare_exchange(current, candidate, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return;
            }
        }
    }

    fn notify_if_durable(&self, target_lsn: usize, acknowledged: &AtomicBool) {
        if self.synced_lsn.load(Ordering::Acquire) >= target_lsn {
            acknowledged.store(true, Ordering::Release);
        }
    }

    fn prefix_is_durable(&self, target_lsn: usize) -> bool {
        let durable = self.durable.read().expect("durable read lock");
        (1..=target_lsn).all(|lsn| durable[lsn])
    }
}

#[derive(Debug)]
struct ModelCheckpointPublisher {
    checkpoint_lsn: AtomicUsize,
}

impl ModelCheckpointPublisher {
    fn new() -> Self {
        Self {
            checkpoint_lsn: AtomicUsize::new(0),
        }
    }

    fn publish_checkpoint(&self, frontier: &ModelDurabilityFrontier, requested_lsn: usize) {
        let synced = frontier.synced_lsn.load(Ordering::Acquire);
        let publishable = requested_lsn.min(synced);

        loop {
            let current = self.checkpoint_lsn.load(Ordering::Acquire);
            if publishable <= current {
                return;
            }

            if self
                .checkpoint_lsn
                .compare_exchange(current, publishable, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return;
            }
        }
    }
}

#[derive(Debug)]
struct ModelConcurrentCheckpointVocab {
    publication_gate: RwLock<()>,
    visible: RwLock<[bool; 2]>,
    wal_lsn: RwLock<[usize; 2]>,
    checkpointed: RwLock<[bool; 2]>,
    next_lsn: AtomicUsize,
    truncated_through: AtomicUsize,
    dirty: AtomicBool,
}

impl ModelConcurrentCheckpointVocab {
    fn new() -> Self {
        Self {
            publication_gate: RwLock::new(()),
            visible: RwLock::new([false; 2]),
            wal_lsn: RwLock::new([0; 2]),
            checkpointed: RwLock::new([false; 2]),
            next_lsn: AtomicUsize::new(1),
            truncated_through: AtomicUsize::new(0),
            dirty: AtomicBool::new(false),
        }
    }

    fn insert(&self, term: usize) {
        let _publication_read = self.publication_gate.read().expect("publication read gate");
        let lsn = self.next_lsn.fetch_add(1, Ordering::AcqRel);
        assert!(lsn <= 2, "bounded checkpoint model supports two inserts");

        self.wal_lsn.write().expect("wal lsn write")[term] = lsn;
        self.visible.write().expect("visible write")[term] = true;
        self.dirty.store(true, Ordering::Release);
    }

    fn checkpoint(&self) {
        let _publication_write = self
            .publication_gate
            .write()
            .expect("publication write gate");
        let visible = *self.visible.read().expect("visible read");
        let wal_lsn = *self.wal_lsn.read().expect("wal lsn read");
        let checkpoint_lsn = visible
            .iter()
            .enumerate()
            .filter_map(|(term, is_visible)| is_visible.then_some(wal_lsn[term]))
            .max()
            .unwrap_or(0);

        *self.checkpointed.write().expect("checkpointed write") = visible;
        self.truncated_through
            .store(checkpoint_lsn, Ordering::Release);
        self.dirty.store(false, Ordering::Release);
    }

    fn sync_only(&self) {
        // WAL sync is not checkpoint publication.
    }

    fn rotate_wal(&self) {
        // WAL rotation is not checkpoint publication or truncation here.
    }

    fn recovery_contains(&self, term: usize) -> bool {
        let checkpointed = *self.checkpointed.read().expect("checkpointed read");
        if checkpointed[term] {
            return true;
        }

        let visible = *self.visible.read().expect("visible read");
        let wal_lsn = *self.wal_lsn.read().expect("wal lsn read");
        let truncated_through = self.truncated_through.load(Ordering::Acquire);

        visible[term] && wal_lsn[term] > truncated_through
    }

    fn assert_recovery_covers_visible(&self) {
        let visible = *self.visible.read().expect("visible read");
        for (term, is_visible) in visible.iter().copied().enumerate() {
            if is_visible {
                assert!(
                    self.recovery_contains(term),
                    "visible term {term} must be checkpointed or WAL-replayable"
                );
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ObservedVocabKind {
    Insert(usize),
    Read(usize),
    BatchFixed,
    Checkpoint,
    Recover,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ObservedVocabOp {
    kind: ObservedVocabKind,
    start: usize,
    finish: usize,
    ret: Option<usize>,
    batch_ret: [Option<usize>; 4],
    recovered: [Option<usize>; 2],
}

impl ObservedVocabOp {
    fn new(kind: ObservedVocabKind, start: usize, finish: usize) -> Self {
        Self {
            kind,
            start,
            finish,
            ret: None,
            batch_ret: [None; 4],
            recovered: [None; 2],
        }
    }
}

#[derive(Debug)]
struct ModelLinearizableVocab {
    publication_gate: RwLock<()>,
    visible: RwLock<[Option<usize>; 2]>,
    checkpointed: RwLock<[Option<usize>; 2]>,
    wal_live: RwLock<[bool; 2]>,
    next_index: AtomicUsize,
    clock: AtomicUsize,
    history: RwLock<Vec<ObservedVocabOp>>,
}

impl ModelLinearizableVocab {
    const BATCH_TERMS: [usize; 4] = [0, 0, 1, 0];

    fn new() -> Self {
        Self {
            publication_gate: RwLock::new(()),
            visible: RwLock::new([None; 2]),
            checkpointed: RwLock::new([None; 2]),
            wal_live: RwLock::new([false; 2]),
            next_index: AtomicUsize::new(0),
            clock: AtomicUsize::new(1),
            history: RwLock::new(Vec::new()),
        }
    }

    fn tick(&self) -> usize {
        self.clock.fetch_add(1, Ordering::SeqCst)
    }

    fn push_history(&self, op: ObservedVocabOp) {
        self.history.write().expect("history write").push(op);
    }

    fn insert(&self, term: usize) -> usize {
        let start = self.tick();
        let _publication_read = self.publication_gate.read().expect("publication read");
        thread::yield_now();

        let mut visible = self.visible.write().expect("visible write");
        let index = match visible[term] {
            Some(existing) => existing,
            None => {
                let index = self.next_index.fetch_add(1, Ordering::SeqCst);
                visible[term] = Some(index);
                self.wal_live.write().expect("wal write")[term] = true;
                index
            }
        };

        drop(visible);
        let finish = self.tick();
        let mut op = ObservedVocabOp::new(ObservedVocabKind::Insert(term), start, finish);
        op.ret = Some(index);
        self.push_history(op);
        index
    }

    fn read_index(&self, term: usize) -> Option<usize> {
        let start = self.tick();
        thread::yield_now();
        let result = self.visible.read().expect("visible read")[term];
        let finish = self.tick();

        let mut op = ObservedVocabOp::new(ObservedVocabKind::Read(term), start, finish);
        op.ret = result;
        self.push_history(op);
        result
    }

    fn insert_batch_fixed(&self) -> [usize; 4] {
        let start = self.tick();
        let _publication_read = self.publication_gate.read().expect("publication read");
        thread::yield_now();

        let mut visible = self.visible.write().expect("visible write");
        let mut wal_live = self.wal_live.write().expect("wal write");
        let mut result = [0; 4];

        for (slot, term) in Self::BATCH_TERMS.iter().copied().enumerate() {
            result[slot] = match visible[term] {
                Some(existing) => existing,
                None => {
                    let index = self.next_index.fetch_add(1, Ordering::SeqCst);
                    visible[term] = Some(index);
                    wal_live[term] = true;
                    index
                }
            };
        }

        drop(wal_live);
        drop(visible);
        let finish = self.tick();

        let mut op = ObservedVocabOp::new(ObservedVocabKind::BatchFixed, start, finish);
        op.batch_ret = result.map(Some);
        self.push_history(op);
        result
    }

    fn checkpoint(&self) -> usize {
        let start = self.tick();
        let _publication_write = self.publication_gate.write().expect("publication write");
        thread::yield_now();

        let visible = *self.visible.read().expect("visible read");
        *self.checkpointed.write().expect("checkpointed write") = visible;
        *self.wal_live.write().expect("wal write") = [false; 2];
        let count = visible.iter().filter(|entry| entry.is_some()).count();

        let finish = self.tick();
        let mut op = ObservedVocabOp::new(ObservedVocabKind::Checkpoint, start, finish);
        op.ret = Some(count);
        self.push_history(op);
        count
    }

    fn recover(&self) -> [Option<usize>; 2] {
        let start = self.tick();
        thread::yield_now();

        let visible = *self.visible.read().expect("visible read");
        let checkpointed = *self.checkpointed.read().expect("checkpointed read");
        let wal_live = *self.wal_live.read().expect("wal read");
        let mut recovered = [None; 2];

        for term in 0..2 {
            recovered[term] = if checkpointed[term].is_some() {
                checkpointed[term]
            } else if wal_live[term] {
                visible[term]
            } else {
                None
            };
        }

        let finish = self.tick();
        let mut op = ObservedVocabOp::new(ObservedVocabKind::Recover, start, finish);
        op.recovered = recovered;
        self.push_history(op);
        recovered
    }

    fn history(&self) -> Vec<ObservedVocabOp> {
        self.history.read().expect("history read").clone()
    }
}

fn apply_vocab_op(
    mut visible: [Option<usize>; 2],
    mut next_index: usize,
    op: ObservedVocabOp,
) -> Option<([Option<usize>; 2], usize)> {
    match op.kind {
        ObservedVocabKind::Insert(term) => match visible[term] {
            Some(existing) if op.ret == Some(existing) => Some((visible, next_index)),
            None if op.ret == Some(next_index) => {
                visible[term] = Some(next_index);
                Some((visible, next_index + 1))
            }
            _ => None,
        },
        ObservedVocabKind::Read(term) => {
            if op.ret == visible[term] {
                Some((visible, next_index))
            } else {
                None
            }
        }
        ObservedVocabKind::BatchFixed => {
            let mut expected = [None; 4];
            for (slot, term) in ModelLinearizableVocab::BATCH_TERMS
                .iter()
                .copied()
                .enumerate()
            {
                let index = match visible[term] {
                    Some(existing) => existing,
                    None => {
                        let index = next_index;
                        visible[term] = Some(index);
                        next_index += 1;
                        index
                    }
                };
                expected[slot] = Some(index);
            }

            if op.batch_ret == expected {
                Some((visible, next_index))
            } else {
                None
            }
        }
        ObservedVocabKind::Checkpoint => {
            let expected_count = visible.iter().filter(|entry| entry.is_some()).count();
            if op.ret == Some(expected_count) {
                Some((visible, next_index))
            } else {
                None
            }
        }
        ObservedVocabKind::Recover => {
            if op.recovered == visible {
                Some((visible, next_index))
            } else {
                None
            }
        }
    }
}

fn order_respects_real_time(history: &[ObservedVocabOp], order: &[usize]) -> bool {
    for later_pos in 0..order.len() {
        let later = history[order[later_pos]];
        for earlier_pos in (later_pos + 1)..order.len() {
            let earlier = history[order[earlier_pos]];
            if earlier.finish < later.start {
                return false;
            }
        }
    }
    true
}

fn order_matches_sequential_vocab(history: &[ObservedVocabOp], order: &[usize]) -> bool {
    let mut visible = [None; 2];
    let mut next_index = 0;

    for &idx in order {
        match apply_vocab_op(visible, next_index, history[idx]) {
            Some((next_visible, next_next_index)) => {
                visible = next_visible;
                next_index = next_next_index;
            }
            None => return false,
        }
    }

    true
}

fn has_linearizable_vocab_order(
    history: &[ObservedVocabOp],
    used: &mut [bool],
    order: &mut Vec<usize>,
) -> bool {
    if order.len() == history.len() {
        return order_respects_real_time(history, order)
            && order_matches_sequential_vocab(history, order);
    }

    for idx in 0..history.len() {
        if used[idx] {
            continue;
        }

        used[idx] = true;
        order.push(idx);

        if order_respects_real_time(history, order)
            && has_linearizable_vocab_order(history, used, order)
        {
            return true;
        }

        order.pop();
        used[idx] = false;
    }

    false
}

fn assert_vocab_history_linearizable(history: &[ObservedVocabOp]) {
    let mut used = vec![false; history.len()];
    let mut order = Vec::with_capacity(history.len());

    assert!(
        has_linearizable_vocab_order(history, &mut used, &mut order),
        "no sequential explanation for vocab history: {history:?}"
    );
}

#[derive(Debug)]
struct ModelVersionRegistry {
    state: RwLock<usize>,
    readers: AtomicUsize,
    gc_durable: AtomicBool,
}

impl ModelVersionRegistry {
    const ACTIVE: usize = 0;
    const RETIRED: usize = 1;
    const RECLAIMED: usize = 2;

    fn new() -> Self {
        Self {
            state: RwLock::new(Self::ACTIVE),
            readers: AtomicUsize::new(0),
            gc_durable: AtomicBool::new(false),
        }
    }

    fn retire(&self) {
        let mut state = self.state.write().expect("version state write lock");
        if *state == Self::ACTIVE {
            *state = Self::RETIRED;
        }
    }

    fn begin_read(&self) -> bool {
        let state = self.state.write().expect("version state write lock");
        if *state == Self::RECLAIMED {
            return false;
        }

        self.readers.fetch_add(1, Ordering::AcqRel);
        true
    }

    fn end_read(&self) {
        self.readers.fetch_sub(1, Ordering::AcqRel);
    }

    fn try_reclaim_with_durable_gc(&self) -> bool {
        let mut state = self.state.write().expect("version state write lock");
        if *state == Self::RETIRED && self.readers.load(Ordering::Acquire) == 0 {
            self.gc_durable.store(true, Ordering::Release);
            *state = Self::RECLAIMED;
            true
        } else {
            false
        }
    }

    fn is_reclaimed(&self) -> bool {
        *self.state.read().expect("version state read lock") == Self::RECLAIMED
    }
}

#[test]
fn atomic_root_compare_exchange_has_single_winner() {
    loom::model(|| {
        let root = Arc::new(ModelRootSlot::new(Arc::new(ModelNode::empty())));
        let expected = root.load().expect("initial root");
        let winners = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for _ in 0..2 {
            let root = Arc::clone(&root);
            let expected = Arc::clone(&expected);
            let winners = Arc::clone(&winners);

            handles.push(thread::spawn(move || {
                let candidate = Arc::new(ModelNode::with_key());
                if root.compare_exchange(&expected, candidate).is_ok() {
                    winners.fetch_add(1, Ordering::SeqCst);
                }
            }));
        }

        for handle in handles {
            handle.join().expect("thread completed");
        }

        assert_eq!(winners.load(Ordering::SeqCst), 1);
        assert!(root.load().expect("final root").contains_key);
    });
}

#[test]
fn duplicate_insert_linearizes_at_single_root_publish() {
    loom::model(|| {
        let root = Arc::new(ModelRootSlot::new(Arc::new(ModelNode::empty())));
        let cache = Arc::new(AtomicBool::new(false));
        let successful_inserts = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for _ in 0..2 {
            let root = Arc::clone(&root);
            let cache = Arc::clone(&cache);
            let successful_inserts = Arc::clone(&successful_inserts);

            handles.push(thread::spawn(move || {
                if insert_one_key(&root, &cache) {
                    successful_inserts.fetch_add(1, Ordering::SeqCst);
                }
            }));
        }

        for handle in handles {
            handle.join().expect("thread completed");
        }

        assert_eq!(successful_inserts.load(Ordering::SeqCst), 1);
        assert!(cache.load(Ordering::Acquire));
        assert!(contains_one_key(&root, &cache));
    });
}

#[test]
fn contains_after_insert_join_observes_published_key() {
    loom::model(|| {
        let root = Arc::new(ModelRootSlot::new(Arc::new(ModelNode::empty())));
        let cache = Arc::new(AtomicBool::new(false));
        let observed_visible = Arc::new(AtomicBool::new(false));

        let inserter = {
            let root = Arc::clone(&root);
            let cache = Arc::clone(&cache);
            thread::spawn(move || {
                insert_one_key(&root, &cache);
            })
        };

        let observer = {
            let root = Arc::clone(&root);
            let cache = Arc::clone(&cache);
            let observed_visible = Arc::clone(&observed_visible);
            thread::spawn(move || {
                if contains_one_key(&root, &cache) {
                    observed_visible.store(true, Ordering::Release);
                }
            })
        };

        inserter.join().expect("insert completed");
        observer.join().expect("observer completed");

        assert!(contains_one_key(&root, &cache));
        assert!(
            observed_visible.load(Ordering::Acquire) || contains_one_key(&root, &cache),
            "a concurrent contains may linearize before or after the insert"
        );
    });
}

#[test]
fn merge_snapshot_is_prefix_of_cache_publication() {
    loom::model(|| {
        let root = Arc::new(ModelRootSlot::new(Arc::new(ModelNode::empty())));
        let cache = Arc::new(AtomicBool::new(false));
        let persisted = Arc::new(AtomicBool::new(false));

        let inserter = {
            let root = Arc::clone(&root);
            let cache = Arc::clone(&cache);
            thread::spawn(move || {
                insert_one_key(&root, &cache);
            })
        };

        let merger = {
            let cache = Arc::clone(&cache);
            let persisted = Arc::clone(&persisted);
            thread::spawn(move || {
                merge_once(&cache, &persisted);
            })
        };

        inserter.join().expect("insert completed");
        merger.join().expect("merge completed");

        assert!(
            !persisted.load(Ordering::Acquire) || cache.load(Ordering::Acquire),
            "merge cannot persist a key before cache publication"
        );

        merge_once(&cache, &persisted);
        assert!(persisted.load(Ordering::Acquire));
        assert!(contains_one_key(&root, &cache));
    });
}

#[test]
fn char_increment_same_key_counts_each_successful_delta() {
    loom::model(|| {
        let overlay = Arc::new(ModelValueOverlay::new());
        let persisted = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for _ in 0..2 {
            let overlay = Arc::clone(&overlay);
            handles.push(thread::spawn(move || {
                overlay.increment(1);
            }));
        }

        for handle in handles {
            handle.join().expect("increment completed");
        }

        assert_eq!(overlay.get_value(), Some(2));
        assert!(overlay.cache.load(Ordering::Acquire));

        overlay.merge_snapshot(&persisted);
        assert_eq!(persisted.load(Ordering::Acquire), 2);
    });
}

#[test]
fn char_create_vs_increment_race_has_one_leaf_and_total_value() {
    loom::model(|| {
        let overlay = Arc::new(ModelValueOverlay::new());

        let first = {
            let overlay = Arc::clone(&overlay);
            thread::spawn(move || overlay.increment(1))
        };

        let second = {
            let overlay = Arc::clone(&overlay);
            thread::spawn(move || overlay.increment(2))
        };

        let first_result = first.join().expect("first increment completed");
        let second_result = second.join().expect("second increment completed");

        assert!(matches!(first_result, 1 | 3));
        assert!(matches!(second_result, 2 | 3));
        assert_eq!(overlay.leaf_initializations.load(Ordering::SeqCst), 1);
        assert_eq!(overlay.get_value(), Some(3));
    });
}

#[test]
fn char_checked_increment_overflow_preserves_overlay_value() {
    loom::model(|| {
        let overlay = ModelValueOverlay::new();

        assert_eq!(overlay.try_increment_checked(2, 3), Ok(2));
        assert_eq!(overlay.try_increment_checked(2, 3), Err(()));
        assert_eq!(overlay.get_value(), Some(2));
    });
}

#[test]
fn char_checked_merge_overflow_preserves_overlay_and_persistent_value() {
    loom::model(|| {
        let overlay = ModelValueOverlay::new();
        let persisted = AtomicUsize::new(2);

        assert_eq!(overlay.try_increment_checked(2, 3), Ok(2));
        assert_eq!(overlay.try_merge_snapshot_checked(&persisted, 3), Err(()));
        assert_eq!(persisted.load(Ordering::Acquire), 2);
        assert_eq!(overlay.get_value(), Some(2));

        persisted.store(1, Ordering::Release);
        assert_eq!(overlay.try_merge_snapshot_checked(&persisted, 3), Ok(()));
        assert_eq!(persisted.load(Ordering::Acquire), 3);
    });
}

#[test]
fn char_merge_snapshot_never_exceeds_visible_value() {
    loom::model(|| {
        let overlay = Arc::new(ModelValueOverlay::new());
        let persisted = Arc::new(AtomicUsize::new(0));

        let incrementer = {
            let overlay = Arc::clone(&overlay);
            thread::spawn(move || {
                overlay.increment(1);
                thread::yield_now();
                overlay.increment(1);
            })
        };

        let merger = {
            let overlay = Arc::clone(&overlay);
            let persisted = Arc::clone(&persisted);
            thread::spawn(move || {
                overlay.merge_snapshot(&persisted);
            })
        };

        incrementer.join().expect("increments completed");
        merger.join().expect("merge completed");

        let visible = overlay.get_value().unwrap_or(0);
        assert!(persisted.load(Ordering::Acquire) <= visible);

        overlay.merge_snapshot(&persisted);
        assert_eq!(persisted.load(Ordering::Acquire), visible);
    });
}

#[test]
fn vocab_duplicate_insert_returns_stable_index_and_allows_sparse_next_index() {
    loom::model(|| {
        const NOT_OBSERVED: usize = usize::MAX;

        let vocab = Arc::new(ModelVocabOverlay::new());
        let first_index = Arc::new(AtomicUsize::new(NOT_OBSERVED));
        let second_index = Arc::new(AtomicUsize::new(NOT_OBSERVED));

        let first = {
            let vocab = Arc::clone(&vocab);
            let first_index = Arc::clone(&first_index);
            thread::spawn(move || {
                first_index.store(vocab.insert(0), Ordering::Release);
            })
        };

        let second = {
            let vocab = Arc::clone(&vocab);
            let second_index = Arc::clone(&second_index);
            thread::spawn(move || {
                second_index.store(vocab.insert(0), Ordering::Release);
            })
        };

        first.join().expect("first vocab insert completed");
        second.join().expect("second vocab insert completed");

        let first_index = first_index.load(Ordering::Acquire);
        let second_index = second_index.load(Ordering::Acquire);
        let next_index = vocab.next_index.load(Ordering::Acquire);

        assert_ne!(first_index, NOT_OBSERVED);
        assert_eq!(first_index, second_index);
        assert_eq!(vocab.root_snapshot().get(0), Some(first_index));
        assert_eq!(vocab.cache_get(0), Some(first_index));
        assert_eq!(vocab.entry_count.load(Ordering::Acquire), 1);
        assert!((1..=2).contains(&next_index));
        assert!(next_index >= vocab.entry_count.load(Ordering::Acquire));
    });
}

#[test]
fn vocab_distinct_insert_commits_unique_indices_without_wasting_claims() {
    loom::model(|| {
        const NOT_OBSERVED: usize = usize::MAX;

        let vocab = Arc::new(ModelVocabOverlay::new());
        let first_index = Arc::new(AtomicUsize::new(NOT_OBSERVED));
        let second_index = Arc::new(AtomicUsize::new(NOT_OBSERVED));

        let first = {
            let vocab = Arc::clone(&vocab);
            let first_index = Arc::clone(&first_index);
            thread::spawn(move || {
                first_index.store(vocab.insert(0), Ordering::Release);
            })
        };

        let second = {
            let vocab = Arc::clone(&vocab);
            let second_index = Arc::clone(&second_index);
            thread::spawn(move || {
                second_index.store(vocab.insert(1), Ordering::Release);
            })
        };

        first.join().expect("first vocab insert completed");
        second.join().expect("second vocab insert completed");

        let first_index = first_index.load(Ordering::Acquire);
        let second_index = second_index.load(Ordering::Acquire);
        let root = vocab.root_snapshot();

        assert_ne!(first_index, NOT_OBSERVED);
        assert_ne!(second_index, NOT_OBSERVED);
        assert_ne!(first_index, second_index);
        assert_eq!(root.get(0), Some(first_index));
        assert_eq!(root.get(1), Some(second_index));
        assert_eq!(vocab.entry_count.load(Ordering::Acquire), 2);
        assert_eq!(vocab.next_index.load(Ordering::Acquire), 2);
    });
}

#[test]
fn vocab_merge_snapshot_preserves_cache_root_agreement() {
    loom::model(|| {
        let vocab = Arc::new(ModelVocabOverlay::new());

        let inserter = {
            let vocab = Arc::clone(&vocab);
            thread::spawn(move || {
                vocab.insert(0);
                thread::yield_now();
                vocab.insert(1);
            })
        };

        let merger = {
            let vocab = Arc::clone(&vocab);
            thread::spawn(move || {
                vocab.merge_snapshot();
            })
        };

        inserter.join().expect("vocab inserts completed");
        merger.join().expect("vocab merge completed");

        let root = vocab.root_snapshot();
        for term in 0..2 {
            if let Some(persisted) = vocab.persisted_get(term) {
                assert_eq!(root.get(term), Some(persisted));
            }
        }

        vocab.merge_snapshot();
        let root = vocab.root_snapshot();
        for term in 0..2 {
            assert_eq!(vocab.cache_get(term), root.get(term));
            assert_eq!(vocab.persisted_get(term), root.get(term));
        }
    });
}

#[test]
fn vocab_public_insert_read_history_has_sequential_explanation() {
    let mut builder = loom::model::Builder::new();
    builder.preemption_bound = Some(2);

    builder.check(|| {
        let vocab = Arc::new(ModelLinearizableVocab::new());

        let first_insert = {
            let vocab = Arc::clone(&vocab);
            thread::spawn(move || {
                vocab.insert(0);
            })
        };

        let duplicate_insert = {
            let vocab = Arc::clone(&vocab);
            thread::spawn(move || {
                vocab.insert(0);
            })
        };

        let reader = {
            let vocab = Arc::clone(&vocab);
            thread::spawn(move || {
                vocab.read_index(0);
            })
        };

        first_insert.join().expect("first insert completed");
        duplicate_insert.join().expect("duplicate insert completed");
        reader.join().expect("reader completed");

        assert_vocab_history_linearizable(&vocab.history());
    });
}

#[test]
fn vocab_public_batch_checkpoint_recover_history_is_linearizable() {
    let mut builder = loom::model::Builder::new();
    builder.preemption_bound = Some(2);

    builder.check(|| {
        let vocab = Arc::new(ModelLinearizableVocab::new());

        let batch = {
            let vocab = Arc::clone(&vocab);
            thread::spawn(move || {
                let result = vocab.insert_batch_fixed();
                assert_eq!(result[0], result[1]);
                assert_eq!(result[0], result[3]);
                assert_ne!(result[0], result[2]);
            })
        };

        let checkpoint = {
            let vocab = Arc::clone(&vocab);
            thread::spawn(move || {
                vocab.checkpoint();
            })
        };

        batch.join().expect("batch completed");
        checkpoint.join().expect("checkpoint completed");

        let recovered = vocab.recover();
        let visible = *vocab.visible.read().expect("visible read");
        assert_eq!(
            recovered, visible,
            "checkpoint plus retained WAL must recover all visible terms"
        );
        assert_vocab_history_linearizable(&vocab.history());
    });
}

#[test]
fn vocab_public_distinct_insert_checkpoint_recover_history_is_linearizable() {
    let mut builder = loom::model::Builder::new();
    builder.preemption_bound = Some(2);

    builder.check(|| {
        let vocab = Arc::new(ModelLinearizableVocab::new());

        let first = {
            let vocab = Arc::clone(&vocab);
            thread::spawn(move || {
                vocab.insert(0);
            })
        };

        let second = {
            let vocab = Arc::clone(&vocab);
            thread::spawn(move || {
                vocab.insert(1);
            })
        };

        let checkpoint = {
            let vocab = Arc::clone(&vocab);
            thread::spawn(move || {
                vocab.checkpoint();
            })
        };

        first.join().expect("first insert completed");
        second.join().expect("second insert completed");
        checkpoint.join().expect("checkpoint completed");

        vocab.recover();
        assert_vocab_history_linearizable(&vocab.history());
    });
}

#[test]
fn group_commit_publish_acknowledges_only_durable_prefix() {
    loom::model(|| {
        let frontier = Arc::new(ModelDurabilityFrontier::new());
        let acknowledged = Arc::new(AtomicBool::new(false));

        let first_lsn = frontier.reserve_lsn();
        let second_lsn = frontier.reserve_lsn();
        assert_eq!(first_lsn, 1);
        assert_eq!(second_lsn, 2);

        frontier.complete_lsn(second_lsn);
        frontier.publish_frontier();
        frontier.notify_if_durable(second_lsn, &acknowledged);

        assert!(!acknowledged.load(Ordering::Acquire));
        assert!(frontier.synced_lsn.load(Ordering::Acquire) < second_lsn);

        frontier.complete_lsn(first_lsn);
        frontier.publish_frontier();
        frontier.notify_if_durable(second_lsn, &acknowledged);

        assert!(acknowledged.load(Ordering::Acquire));
        assert_eq!(frontier.synced_lsn.load(Ordering::Acquire), second_lsn);
        assert!(frontier.prefix_is_durable(second_lsn));
    });
}

#[test]
fn concurrent_group_commit_reservations_publish_unique_contiguous_lsns() {
    loom::model(|| {
        const NOT_OBSERVED: usize = usize::MAX;

        let frontier = Arc::new(ModelDurabilityFrontier::new());
        let first_observed = Arc::new(AtomicUsize::new(NOT_OBSERVED));
        let second_observed = Arc::new(AtomicUsize::new(NOT_OBSERVED));

        let first = {
            let frontier = Arc::clone(&frontier);
            let first_observed = Arc::clone(&first_observed);
            thread::spawn(move || {
                let lsn = frontier.reserve_lsn();
                frontier.complete_lsn(lsn);
                frontier.publish_frontier();
                first_observed.store(lsn, Ordering::Release);
            })
        };

        let second = {
            let frontier = Arc::clone(&frontier);
            let second_observed = Arc::clone(&second_observed);
            thread::spawn(move || {
                let lsn = frontier.reserve_lsn();
                frontier.complete_lsn(lsn);
                frontier.publish_frontier();
                second_observed.store(lsn, Ordering::Release);
            })
        };

        first.join().expect("first group commit append completed");
        second.join().expect("second group commit append completed");
        frontier.publish_frontier();

        let first_lsn = first_observed.load(Ordering::Acquire);
        let second_lsn = second_observed.load(Ordering::Acquire);

        assert_ne!(first_lsn, NOT_OBSERVED);
        assert_ne!(second_lsn, NOT_OBSERVED);
        assert_ne!(first_lsn, second_lsn);
        assert_eq!(frontier.synced_lsn.load(Ordering::Acquire), 2);
        assert!(frontier.prefix_is_durable(2));
    });
}

#[test]
fn async_wal_out_of_order_completion_does_not_advance_past_gap() {
    loom::model(|| {
        let frontier = ModelDurabilityFrontier::new();
        let first_lsn = frontier.reserve_lsn();
        let second_lsn = frontier.reserve_lsn();

        frontier.complete_lsn(second_lsn);
        frontier.publish_frontier();

        assert_eq!(frontier.synced_lsn.load(Ordering::Acquire), 0);
        assert!(!frontier.prefix_is_durable(second_lsn));

        frontier.complete_lsn(first_lsn);
        frontier.publish_frontier();

        assert_eq!(frontier.synced_lsn.load(Ordering::Acquire), second_lsn);
        assert!(frontier.prefix_is_durable(second_lsn));
    });
}

#[test]
fn checkpoint_publication_never_exceeds_synced_frontier() {
    loom::model(|| {
        let frontier = Arc::new(ModelDurabilityFrontier::new());
        let checkpoint = Arc::new(ModelCheckpointPublisher::new());

        let lsn = frontier.reserve_lsn();

        let checkpoint_thread = {
            let frontier = Arc::clone(&frontier);
            let checkpoint = Arc::clone(&checkpoint);
            thread::spawn(move || {
                checkpoint.publish_checkpoint(&frontier, lsn);
            })
        };

        let append_thread = {
            let frontier = Arc::clone(&frontier);
            thread::spawn(move || {
                thread::yield_now();
                frontier.complete_lsn(lsn);
                frontier.publish_frontier();
            })
        };

        checkpoint_thread
            .join()
            .expect("checkpoint publication completed");
        append_thread.join().expect("append publication completed");

        checkpoint.publish_checkpoint(&frontier, lsn);

        let checkpoint_lsn = checkpoint.checkpoint_lsn.load(Ordering::Acquire);
        let synced_lsn = frontier.synced_lsn.load(Ordering::Acquire);

        assert!(checkpoint_lsn <= synced_lsn);
        assert!(frontier.prefix_is_durable(checkpoint_lsn));
    });
}

#[test]
fn concurrent_checkpoint_recovers_insert_racing_publication() {
    loom::model(|| {
        let vocab = Arc::new(ModelConcurrentCheckpointVocab::new());

        let inserter = {
            let vocab = Arc::clone(&vocab);
            thread::spawn(move || {
                vocab.insert(0);
            })
        };

        let checkpointer = {
            let vocab = Arc::clone(&vocab);
            thread::spawn(move || {
                vocab.checkpoint();
            })
        };

        inserter.join().expect("insert completed");
        checkpointer.join().expect("checkpoint completed");

        vocab.assert_recovery_covers_visible();
    });
}

#[test]
fn concurrent_checkpoint_with_two_inserts_preserves_replay_tail() {
    let mut builder = loom::model::Builder::new();
    builder.preemption_bound = Some(2);

    builder.check(|| {
        let vocab = Arc::new(ModelConcurrentCheckpointVocab::new());

        let first = {
            let vocab = Arc::clone(&vocab);
            thread::spawn(move || {
                vocab.insert(0);
            })
        };

        let checkpoint = {
            let vocab = Arc::clone(&vocab);
            thread::spawn(move || {
                vocab.checkpoint();
            })
        };

        let second = {
            let vocab = Arc::clone(&vocab);
            thread::spawn(move || {
                vocab.insert(1);
            })
        };

        first.join().expect("first insert completed");
        checkpoint.join().expect("checkpoint completed");
        second.join().expect("second insert completed");

        vocab.assert_recovery_covers_visible();
    });
}

#[test]
fn sync_and_rotate_do_not_publish_concurrent_checkpoint() {
    loom::model(|| {
        let vocab = ModelConcurrentCheckpointVocab::new();

        vocab.insert(0);
        vocab.sync_only();
        vocab.rotate_wal();

        assert!(vocab.dirty.load(Ordering::Acquire));
        assert_eq!(
            *vocab.checkpointed.read().expect("checkpointed read"),
            [false; 2],
            "sync/rotate must not publish checkpoint state"
        );
        vocab.assert_recovery_covers_visible();
    });
}

#[test]
fn version_gc_reader_guard_blocks_reclaim_until_drop() {
    loom::model(|| {
        let registry = Arc::new(ModelVersionRegistry::new());
        registry.retire();

        assert!(registry.begin_read());
        assert!(!registry.try_reclaim_with_durable_gc());
        assert!(!registry.is_reclaimed());

        registry.end_read();
        assert!(registry.try_reclaim_with_durable_gc());
        assert!(registry.gc_durable.load(Ordering::Acquire));
        assert!(registry.is_reclaimed());
    });
}

#[test]
fn version_gc_race_reclaims_only_without_active_reader() {
    loom::model(|| {
        let registry = Arc::new(ModelVersionRegistry::new());
        registry.retire();

        let reader = {
            let registry = Arc::clone(&registry);
            thread::spawn(move || {
                if registry.begin_read() {
                    thread::yield_now();
                    assert!(!registry.is_reclaimed());
                    registry.end_read();
                }
            })
        };

        let collector = {
            let registry = Arc::clone(&registry);
            thread::spawn(move || {
                thread::yield_now();
                registry.try_reclaim_with_durable_gc();
            })
        };

        reader.join().expect("reader completed");
        collector.join().expect("collector completed");

        if !registry.is_reclaimed() {
            assert!(registry.try_reclaim_with_durable_gc());
        }

        assert_eq!(registry.readers.load(Ordering::Acquire), 0);
        assert!(registry.gc_durable.load(Ordering::Acquire));
        assert!(registry.is_reclaimed());
    });
}

#[derive(Debug)]
struct DropTracked {
    drops: Arc<AtomicUsize>,
}

impl Drop for DropTracked {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Debug)]
struct ChildSlot {
    child: RwLock<Option<Arc<DropTracked>>>,
}

impl ChildSlot {
    fn new(child: Arc<DropTracked>) -> Self {
        Self {
            child: RwLock::new(Some(child)),
        }
    }

    fn load_child(&self) -> Option<Arc<DropTracked>> {
        self.child.read().expect("child read lock").clone()
    }
}

#[test]
fn child_pointer_handoff_keeps_arc_alive_for_readers() {
    loom::model(|| {
        let drops = Arc::new(AtomicUsize::new(0));
        let child = Arc::new(DropTracked {
            drops: Arc::clone(&drops),
        });
        let slot = Arc::new(ChildSlot::new(child));

        let mut handles = Vec::new();
        for _ in 0..2 {
            let slot = Arc::clone(&slot);
            handles.push(thread::spawn(move || {
                let child = slot.load_child().expect("child present");
                thread::yield_now();
                drop(child);
            }));
        }

        for handle in handles {
            handle.join().expect("reader completed");
        }

        assert_eq!(drops.load(Ordering::SeqCst), 0);
        drop(slot);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    });
}

// ============================================================================
// Char-trie eviction-vs-walk EBR (epoch-based reclamation) models.
//
// These mirror the protocol in `EvictionWalkEBR.tla` /
// `PersistentCharEpochReclamationSpec.v` and the implementation in
// `persistent_artrie_char::{reclaim, evict_char_nodes}` +
// `persistent_artrie_core::concurrency::EpochManager`, WITHOUT making the
// production code depend on loom primitives. Loom exhaustively explores the
// thread interleavings.
//
// Tool division for the gated drain-to-zero reclaim:
//
// The reclaim's safety (an unlinked node is freed only after every reader that
// could hold it has drained) rests on the SeqCst STORE-LOAD ordering between a
// reader's `enter_read` (store `active`) / pointer load and the evictor's unlink
// (store the slot) / `active_reader_count` load — a store-buffer (Dekker) litmus
// that ONLY full SeqCst forbids. Loom does not faithfully model SeqCst store-load
// ordering, so a loom model of this reclaim reports a SPURIOUS use-after-free that
// real SeqCst hardware (and the C11 model the implementation targets) forbid.
//
// That ordering is therefore verified where it can be modeled faithfully:
//   * `EvictionWalkEBR.tla` (TLC) — models the gated protocol under a true total
//     order; `NoUseAfterFree` holds with the gate (`Gated = TRUE`) and is VIOLATED
//     without it (`Gated = FALSE`), proving the gate is necessary;
//   * `PersistentCharEpochReclamationSpec.v` (Rocq) — proves NoUseAfterFree is a
//     state invariant of the unlink -> retire -> drain -> free protocol;
//   * `tests/persistent_char_ebr_correspondence.rs` — exercises the REAL code
//     (walks ‖ eviction over a reopened trie) under ASan/TSan.
//
// Loom's contribution below is the part it models robustly: the lock-free
// swizzle-install CAS race (AcqRel), which has no SC store-load dependency.
// ============================================================================

/// Two readers fault the SAME on-disk slot concurrently (CAS-install). Exactly one
/// wins and publishes its box; the loser frees its OWN (never-published) box and
/// adopts the winner's. Both observe the same installed box, and the loser's free
/// is never a double-free of the published box. Models `resolve_swizzled_ptr`'s
/// install race in `disk_io.rs`.
#[test]
fn swizzle_install_race_single_winner_no_double_free() {
    fn fault(slot: &AtomicUsize, loser_frees: &AtomicUsize, my_box: usize) -> usize {
        // Fast path: already installed by someone else.
        let cur = slot.load(Ordering::Acquire);
        if cur != 0 {
            return cur;
        }
        // Slow path: try to install my own box.
        match slot.compare_exchange(0, my_box, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => my_box, // won the race: my box is now published
            Err(winner) => {
                // lost: free my own unpublished box, adopt the winner's.
                loser_frees.fetch_add(1, Ordering::SeqCst);
                winner
            }
        }
    }

    loom::model(|| {
        let slot = Arc::new(AtomicUsize::new(0)); // 0 = on-disk / empty
        let loser_frees = Arc::new(AtomicUsize::new(0));

        let t1 = {
            let (slot, lf) = (slot.clone(), loser_frees.clone());
            thread::spawn(move || fault(&slot, &lf, 1))
        };
        let t2 = {
            let (slot, lf) = (slot.clone(), loser_frees.clone());
            thread::spawn(move || fault(&slot, &lf, 2))
        };
        let r1 = t1.join().expect("t1");
        let r2 = t2.join().expect("t2");

        let installed = slot.load(Ordering::Acquire);
        assert!(installed == 1 || installed == 2, "no box installed");
        assert_eq!(r1, installed, "reader 1 disagrees with installed box");
        assert_eq!(r2, installed, "reader 2 disagrees with installed box");
        assert!(
            loser_frees.load(Ordering::SeqCst) <= 1,
            "double-free: more than one loser box freed"
        );
    });
}
