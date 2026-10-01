use std::sync::atomic::{AtomicUsize, Ordering};

/// Keep a WAL attempt visible to checkpointing from before its operation ID is
/// reserved until its prepare/commit sequence (or failed CAS) has finished.
/// A publication-only guard leaves a prepare segment vulnerable to pruning.
pub(super) struct SuffixWalOperation<'a>(&'a AtomicUsize);

impl<'a> SuffixWalOperation<'a> {
    pub(super) fn begin(inflight: &'a AtomicUsize) -> Self {
        inflight.fetch_add(1, Ordering::SeqCst);
        Self(inflight)
    }
}

impl Drop for SuffixWalOperation<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
