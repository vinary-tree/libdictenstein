//! Cold exact-registry lifecycle serialization.
//!
//! Semantic writers never enter this gate. Their existing root CAS clears the
//! exact eviction binding at the same linearization point that publishes the
//! semantic successor. Exact eviction and fault transitions are likewise
//! lock-free root CASes over helped packed residency. This gate is confined to
//! checkpoint publication, retirement, and detached compatibility-catalog
//! replacement.

use std::sync::Arc;

use parking_lot::{Mutex, MutexGuard};

/// Trie-lifetime serialization for the remaining mutable exact-registry tail.
#[derive(Debug, Default)]
pub(crate) struct RegistryPublicationGate {
    lifecycle: Mutex<()>,
}

impl RegistryPublicationGate {
    /// Allocate one stable gate for a trie lifetime.
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Lock the short registry lifecycle critical section.
    pub(super) fn lock_lifecycle(&self) -> MutexGuard<'_, ()> {
        self.lifecycle.lock()
    }
}
