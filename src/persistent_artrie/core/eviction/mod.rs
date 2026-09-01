//! Memory pressure-driven eviction for bounded-memory ARTrie operation.
//!
//! This module implements SQLite-style memory management for the persistent ARTrie:
//! - **Memory pressure-driven** - Eviction triggered by [`MemoryPressureMonitor`], not after every checkpoint
//! - **Asynchronous** - Background eviction thread, non-blocking for client operations
//! - **Epoch-based safety** - Uses [`EpochManager`] to safely evict nodes without blocking readers
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                    PersistentARTrie<V>                          │
//! ├─────────────────────────────────────────────────────────────────┤
//! │  MemoryPressureMonitor (background thread)                      │
//! │    ↓ callback on Low/Critical pressure                          │
//! │  EvictionCoordinator                                            │
//! │    ↓ queues eviction request                                    │
//! │  Eviction Thread (async)                                        │
//! │    ├─ Wait for epoch quiescence (no old-epoch readers)          │
//! │    ├─ Select cold nodes via LRU/access tracking                 │
//! │    └─ Atomically swap ChildNode → DiskRef                       │
//! └─────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Usage
//!
//! ```text
//! use libdictenstein::persistent_artrie::{PersistentARTrie, EvictionConfig};
//! use libdictenstein::EvictableARTrie;
//!
//! // Create or open a trie
//! let mut trie = PersistentARTrie::<()>::create("words.part")?;
//!
//! // Enable memory pressure-driven eviction
//! let config = EvictionConfig::default();
//! trie.enable_eviction(config)?;
//!
//! // Normal operations continue...
//! trie.insert("hello");
//! trie.checkpoint()?;
//!
//! // Eviction happens automatically when memory pressure is detected
//! // Check stats for eviction activity
//! let stats = trie.eviction_stats();
//! println!("Nodes evicted: {}", stats.nodes_evicted);
//! ```
//!
//! [`MemoryPressureMonitor`]: crate::persistent_artrie::core::memory_monitor::MemoryPressureMonitor
//! [`EpochManager`]: crate::persistent_artrie::core::concurrency::EpochManager

mod atomic_residency;
mod config;
mod coordinator;
mod disk_registry;
pub mod lru_tracker;
mod publication_gate;
mod registry_build;

pub(crate) use atomic_residency::{
    AtomicResidencyGeneration, PackedResidencyDelta, PackedResidencyTransition,
    ResidencyHelpOutcome,
};
pub use config::{EvictionConfig, EvictionStats, EvictionUrgency};
pub use coordinator::EvictionCoordinator;
pub(crate) use coordinator::{
    ExactEvictionOutcome, ExactFaultOutcome, PreparedRegistryPublication,
    RegistryPublicationOutcome, RegistryTransitionAuthority, RetirementOutcome,
};
pub(crate) use disk_registry::{
    CompactEvictionBatch, CompactEvictionPolicy, LocalRegistryGraftStats, PreparedPackedResidency,
    PublishedRegistryCatalog, RegistryBuilderSubtree, RegistryBuilderSubtreeStart, RegistryFamily,
    RegistryGraftOutcome, RegistryPathId, RegistryStructuralSource,
};
pub use disk_registry::{DiskLocationRegistry, EvictableCharNode, EvictableNode};
pub use lru_tracker::{AccessTracker, LruRegistry};
pub(crate) use publication_gate::RegistryPublicationGate;
pub(crate) use registry_build::{
    scan_durable_registry_subtree, DiskRecordAddress, DurableRecordRef, DurableRegistryRecord,
    DurableRegistryScanEvent,
};

#[cfg(test)]
mod tests;
