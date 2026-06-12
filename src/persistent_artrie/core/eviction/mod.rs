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

mod config;
mod coordinator;
mod disk_registry;
pub mod lru_tracker;

pub use config::{EvictionConfig, EvictionStats, EvictionUrgency};
pub use coordinator::EvictionCoordinator;
pub use disk_registry::DiskLocationRegistry;
pub use lru_tracker::{AccessTracker, LruRegistry};

#[cfg(test)]
mod tests;
