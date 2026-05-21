//! Common trait for Adaptive Radix Trie implementations.
//!
//! This module provides the [`ARTrie`] trait that defines a unified API for both
//! byte-level ([`PersistentARTrie`]) and character-level ([`PersistentARTrieChar`])
//! tries.
//!
//! # Disk-Backed Architecture
//!
//! Both trie variants are designed for persistent storage:
//! - **`create(path)`** - Create a new trie file
//! - **`open(path)`** - Open an existing trie file
//! - **`open_with_recovery(path)`** - Open with crash recovery
//!
//! For in-memory tries, use the optimized implementations instead:
//! - [`DoubleArrayTrie`] / [`DoubleArrayTrieChar`] (fastest reads, insert-only)
//! - [`DynamicDawg`] / [`DynamicDawgChar`] (insert + remove, SIMD optimized)
//!
//! # Usage
//!
//! ```rust,ignore
//! use libdictenstein::ARTrie;
//! use libdictenstein::persistent_artrie::PersistentARTrie;
//! use libdictenstein::persistent_artrie_char::PersistentARTrieChar;
//!
//! // Generic function works with both variants
//! fn count_words<T: ARTrie>(trie: &T) -> usize {
//!     trie.len()
//! }
//!
//! // Byte-level trie (disk-backed)
//! let mut byte_trie = PersistentARTrie::<()>::create("words.part")?;
//! byte_trie.insert("hello");
//! byte_trie.checkpoint()?;
//!
//! // Character-level trie (disk-backed)
//! let mut char_trie = PersistentARTrieChar::<()>::create("unicode.artc")?;
//! char_trie.insert("日本語");
//! char_trie.checkpoint()?;
//!
//! // Generic code works with both
//! println!("Byte trie: {} words", count_words(&byte_trie));
//! println!("Char trie: {} words", count_words(&char_trie));
//! ```
//!
//! [`PersistentARTrie`]: crate::persistent_artrie::PersistentARTrie
//! [`PersistentARTrieChar`]: crate::persistent_artrie_char::PersistentARTrieChar
//! [`DoubleArrayTrie`]: crate::double_array_trie::DoubleArrayTrie
//! [`DoubleArrayTrieChar`]: crate::double_array_trie_char::DoubleArrayTrieChar
//! [`DynamicDawg`]: crate::dynamic_dawg::DynamicDawg
//! [`DynamicDawgChar`]: crate::dynamic_dawg_char::DynamicDawgChar

use std::path::Path;

use crate::persistent_artrie::error::Result;
use crate::persistent_artrie::recovery::RecoveryReport;
use crate::value::DictionaryValue;
use crate::CharUnit;

/// Common trait for Adaptive Radix Trie implementations.
///
/// This trait defines the unified API for both byte-level (`PersistentARTrie`)
/// and character-level (`PersistentARTrieChar`) tries. It enables generic code
/// that works with either variant.
///
/// # Disk-Backed Only
///
/// These tries are designed for persistent storage. Use `create()`, `open()`,
/// or `open_with_recovery()` to instantiate them. For in-memory tries, use
/// `DoubleArrayTrie` or `DynamicDawg` instead.
///
/// # Type Parameters
///
/// * `V` - The value type stored in the trie (must implement [`DictionaryValue`])
///
/// # Associated Types
///
/// * `Unit` - The edge label type (`u8` for bytes, `char` for Unicode)
///
/// # Example
///
/// ```rust,ignore
/// use libdictenstein::ARTrie;
/// use libdictenstein::persistent_artrie::PersistentARTrie;
///
/// fn process_vocabulary<T: ARTrie>(vocab: &T) {
///     println!("Vocabulary has {} terms", vocab.len());
///     if vocab.contains("hello") {
///         println!("Contains 'hello'");
///     }
/// }
///
/// let mut trie = PersistentARTrie::<()>::create("words.part")?;
/// trie.insert("hello");
/// process_vocabulary(&trie);
/// ```
pub trait ARTrie: Clone + Send + Sync {
    /// The unit type for edge labels.
    ///
    /// - `u8` for byte-level tries (PersistentARTrie)
    /// - `char` for character-level tries (PersistentARTrieChar)
    type Unit: CharUnit;

    /// The value type stored in the trie.
    type Value: DictionaryValue;

    // === Persistence Operations (constructors) ===

    /// Create a new persistent trie at the given path.
    ///
    /// This creates a new trie file with WAL for crash recovery.
    /// If a file already exists at the path, this will return an error.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the dictionary file (will also create `.wal` file)
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use libdictenstein::ARTrie;
    /// use libdictenstein::persistent_artrie::PersistentARTrie;
    ///
    /// let mut trie = PersistentARTrie::<()>::create("words.part")?;
    /// trie.insert("hello");
    /// trie.checkpoint()?;
    /// ```
    fn create<P: AsRef<Path>>(path: P) -> Result<Self>;

    /// Create with slot-level dirty tracking.
    ///
    /// This enables incremental checkpoints that write only modified slots
    /// instead of entire 256KB arenas, reducing checkpoint I/O by 90%+ for
    /// localized updates.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the dictionary file (must not exist)
    fn create_with_slot_tracking<P: AsRef<Path>>(path: P) -> Result<Self>;

    /// Open an existing persistent trie.
    ///
    /// This opens an existing dictionary file and replays the WAL if needed
    /// to recover from any crash.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the dictionary file
    fn open<P: AsRef<Path>>(path: P) -> Result<Self>;

    /// Open with slot-level dirty tracking.
    ///
    /// Slot-level tracking reduces checkpoint I/O by writing only modified slots
    /// instead of entire arenas. For vocabularies with localized updates, this
    /// can reduce checkpoint I/O by 90%+.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the dictionary file (must exist)
    fn open_with_slot_tracking<P: AsRef<Path>>(path: P) -> Result<Self>;

    /// Open with automatic crash recovery.
    ///
    /// This is the recommended way to open a trie that may have been corrupted
    /// by a crash (OOM kill, power failure, etc.).
    ///
    /// # Recovery Process
    ///
    /// 1. **Check if file exists** - If not, create a new trie
    /// 2. **Detect corruption** - Check header checksum, arena checksums
    /// 3. **If corrupted** - Rebuild from WAL archive segments
    /// 4. **Return trie with recovery report**
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the dictionary file
    ///
    /// # Returns
    ///
    /// Tuple of (trie, recovery_report) indicating what recovery was performed.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use libdictenstein::ARTrie;
    /// use libdictenstein::persistent_artrie::PersistentARTrie;
    ///
    /// let (trie, report) = PersistentARTrie::<i64>::open_with_recovery("data.part")?;
    ///
    /// if !report.mode.is_normal() {
    ///     eprintln!("Recovered from crash: {} records replayed", report.records_replayed);
    /// }
    /// ```
    fn open_with_recovery<P: AsRef<Path>>(path: P) -> Result<(Self, RecoveryReport)>;

    /// Open with crash recovery and slot-level dirty tracking.
    ///
    /// Combines `open_with_recovery()` functionality with slot-level tracking
    /// enabled. This is the recommended method for production use where both
    /// crash recovery and optimized incremental checkpoints are desired.
    ///
    /// Slot-level tracking reduces checkpoint I/O by 90%+ for localized updates
    /// by writing only modified slots instead of entire 256KB arenas.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the dictionary file
    ///
    /// # Returns
    ///
    /// Tuple of (trie, recovery_report) with slot tracking enabled.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use libdictenstein::ARTrie;
    /// use libdictenstein::persistent_artrie_char::PersistentARTrieChar;
    ///
    /// let (trie, report) = PersistentARTrieChar::<i64>::open_with_recovery_and_slot_tracking("data.artc")?;
    ///
    /// if !report.mode.is_normal() {
    ///     eprintln!("Recovered from crash: {} records replayed", report.records_replayed);
    /// }
    ///
    /// // Subsequent checkpoints write only modified slots
    /// trie.checkpoint()?;
    /// ```
    fn open_with_recovery_and_slot_tracking<P: AsRef<Path>>(
        path: P,
    ) -> Result<(Self, RecoveryReport)>;

    /// Enable slot-level dirty tracking for reduced checkpoint I/O.
    ///
    /// Slot-level tracking only flushes modified slots within arenas,
    /// reducing checkpoint I/O by 90%+ for localized updates.
    ///
    /// This is idempotent - calling when already enabled has no effect.
    /// Can be called after construction to enable tracking on a trie
    /// that was opened without it.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use libdictenstein::ARTrie;
    /// use libdictenstein::persistent_artrie_char::PersistentARTrieChar;
    ///
    /// let mut trie = PersistentARTrieChar::<()>::open("words.artc")?;
    /// trie.enable_slot_tracking(); // Enable after opening
    ///
    /// // Now checkpoints will use slot-level tracking
    /// trie.checkpoint()?;
    /// ```
    fn enable_slot_tracking(&self);

    /// Flush dirty arenas in sequential order for optimized disk I/O.
    ///
    /// Sorts dirty arenas by ID before flushing, improving I/O locality
    /// especially on rotational storage. Expected 5-15% faster checkpoints.
    ///
    /// # Errors
    ///
    /// Returns error if any arena flush fails.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use libdictenstein::ARTrie;
    /// use libdictenstein::persistent_artrie_char::PersistentARTrieChar;
    ///
    /// let mut trie = PersistentARTrieChar::<()>::create("words.artc")?;
    /// trie.insert("hello");
    /// trie.flush_sequential()?; // Sequential I/O optimization
    /// trie.checkpoint()?;
    /// ```
    fn flush_sequential(&self) -> Result<()>;

    // === Core Operations ===

    /// Insert a term with default value.
    ///
    /// # Returns
    ///
    /// `true` if the term was newly inserted, `false` if it already existed.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use libdictenstein::ARTrie;
    /// use libdictenstein::persistent_artrie::PersistentARTrie;
    ///
    /// let mut trie = PersistentARTrie::<()>::create("words.part")?;
    /// assert!(trie.insert("hello")); // New term
    /// assert!(!trie.insert("hello")); // Already exists
    /// ```
    fn insert(&self, term: &str) -> bool
    where
        Self::Value: Default;

    /// Insert a term with a specific value.
    ///
    /// # Returns
    ///
    /// `true` if the term was newly inserted, `false` if it already existed.
    fn insert_with_value(&self, term: &str, value: Self::Value) -> bool;

    /// Check if the trie contains a term.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use libdictenstein::ARTrie;
    /// use libdictenstein::persistent_artrie::PersistentARTrie;
    ///
    /// let mut trie = PersistentARTrie::<()>::create("words.part")?;
    /// trie.insert("hello");
    /// assert!(trie.contains("hello"));
    /// assert!(!trie.contains("world"));
    /// ```
    fn contains(&self, term: &str) -> bool;

    /// Get the value associated with a term.
    ///
    /// # Returns
    ///
    /// `Some(value)` if the term exists, `None` otherwise.
    fn get_value(&self, term: &str) -> Option<Self::Value>;

    /// Remove a term from the trie.
    ///
    /// # Returns
    ///
    /// `true` if the term was removed, `false` if it didn't exist.
    fn remove(&self, term: &str) -> bool;

    /// Get the number of terms in the trie.
    fn len(&self) -> usize;

    /// Check if the trie is empty.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    // === Persistence Operations ===

    /// Checkpoint current state to disk.
    ///
    /// This flushes all in-memory changes to the data file and writes
    /// a checkpoint record to the WAL. After a checkpoint, the WAL can
    /// be truncated to reclaim space.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use libdictenstein::ARTrie;
    /// use libdictenstein::persistent_artrie::PersistentARTrie;
    ///
    /// let mut trie = PersistentARTrie::<()>::create("words.part")?;
    /// trie.insert("hello");
    /// trie.checkpoint()?; // Durably persist to disk
    /// ```
    fn checkpoint(&self) -> Result<()>;

    /// Check if trie has unsaved changes.
    ///
    /// Returns `true` if any modifications have been made since the last
    /// checkpoint (or creation).
    fn is_dirty(&self) -> bool;

    // === Prefix Operations ===

    /// Remove all terms with the given prefix.
    ///
    /// Returns the number of terms removed.
    ///
    /// # Arguments
    ///
    /// * `prefix` - The string prefix of terms to remove
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use libdictenstein::ARTrie;
    /// use libdictenstein::persistent_artrie::PersistentARTrie;
    ///
    /// let mut trie = PersistentARTrie::<()>::create("words.part")?;
    /// trie.insert("apple");
    /// trie.insert("application");
    /// trie.insert("banana");
    ///
    /// let count = trie.remove_prefix("app");
    /// assert_eq!(count, 2); // Removed "apple", "application"
    /// ```
    fn remove_prefix(&self, prefix: &str) -> usize;

    /// Iterate over terms with the given prefix.
    ///
    /// Returns `None` if the prefix path doesn't exist in the trie.
    /// Returns `Some(iterator)` that yields all terms starting with the prefix.
    ///
    /// # Arguments
    ///
    /// * `prefix` - The string prefix to search for
    ///
    /// # Returns
    ///
    /// * `Some(impl Iterator<Item = String>)` - Iterator over matching terms
    /// * `None` - If no terms with this prefix exist
    ///
    /// For typing-preserving iteration (each match as `Vec<Self::Unit>`
    /// rather than `String`, no UTF-8 conversion), see
    /// [`Self::iter_prefix_units`].
    fn iter_prefix(&self, prefix: &str) -> Option<Box<dyn Iterator<Item = String> + '_>>;

    /// Sibling of [`Self::iter_prefix`] that preserves the dictionary's
    /// `Unit` typing.
    ///
    /// Yields each matching term as `Vec<Self::Unit>` (e.g. `Vec<char>` for
    /// char-keyed tries, `Vec<u8>` for byte-keyed tries) instead of forcing
    /// every term through a `String` allocation + UTF-8 round-trip. Useful
    /// when the caller wants to manipulate the unit sequence directly (e.g.
    /// to apply unit-level transforms before re-encoding).
    ///
    /// Default implementation falls back to the existing [`Self::iter_prefix`]
    /// (re-encoding through `String`); impls with native unit-level
    /// traversal should override for efficiency.
    fn iter_prefix_units(
        &self,
        prefix: &str,
    ) -> Option<Box<dyn Iterator<Item = Vec<Self::Unit>> + '_>>
    where
        Self::Unit: From<u8> + 'static,
    {
        // Default: stringly path, then decode each String back to Vec<Unit>
        // via the u8-byte iterator. Impls that store units natively should
        // override this to avoid the round-trip.
        let strings = self.iter_prefix(prefix)?;
        Some(Box::new(strings.map(|s| {
            s.into_bytes().into_iter().map(Self::Unit::from).collect()
        })))
    }

    // === Durability Operations ===

    /// Flush in-memory changes to disk.
    ///
    /// This ensures all buffered writes are durably persisted. Unlike `checkpoint()`,
    /// this does not truncate the WAL.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use libdictenstein::ARTrie;
    /// use libdictenstein::persistent_artrie::PersistentARTrie;
    ///
    /// let mut trie = PersistentARTrie::<()>::create("words.part")?;
    /// trie.insert("hello");
    /// trie.sync()?; // Ensure durably persisted
    /// ```
    fn sync(&self) -> Result<()>;

    /// Get the current Log Sequence Number (LSN).
    ///
    /// The LSN monotonically increases with each write operation and is used
    /// for crash recovery. Higher LSN means more recent data.
    fn current_lsn(&self) -> u64;

    /// Get the last synced LSN.
    ///
    /// Returns `Some(lsn)` if data has been synced to disk, `None` if no sync
    /// has occurred yet.
    fn synced_lsn(&self) -> Option<u64>;

    /// Get the current durability policy.
    ///
    /// The policy determines when writes are durably persisted:
    /// - `Immediate`: Every write is immediately synced
    /// - `Periodic(interval)`: Syncs at regular intervals
    /// - `Manual`: Only syncs on explicit `sync()` calls
    ///
    /// The return type now points at the canonical home in
    /// `persistent_artrie_core::durability`. The old
    /// `persistent_artrie::dict_impl::DurabilityPolicy` path is a
    /// `pub use` re-export, kept for back-compat for one release.
    fn durability_policy(&self) -> crate::persistent_artrie_core::durability::DurabilityPolicy;

    // === Atomic Update Operations ===

    /// Insert or update a term's value.
    ///
    /// If the term exists, its value is updated. If it doesn't exist, it's inserted.
    ///
    /// # Returns
    ///
    /// `Ok(true)` if a new term was inserted, `Ok(false)` if an existing term was updated.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use libdictenstein::ARTrie;
    /// use libdictenstein::persistent_artrie_char::PersistentARTrieChar;
    ///
    /// let mut trie = PersistentARTrieChar::<i64>::create("counts.artc")?;
    /// trie.upsert("hello", 1)?;   // New term: returns Ok(true)
    /// trie.upsert("hello", 10)?;  // Update: returns Ok(false)
    /// ```
    fn upsert(&self, term: &str, value: Self::Value) -> Result<bool>;

    /// Atomically increment a numeric value.
    ///
    /// If the term doesn't exist, it's created with the delta as its initial value.
    /// Requires the value type to be convertible to/from i64.
    ///
    /// # Arguments
    ///
    /// * `term` - The term whose value to increment
    /// * `delta` - The amount to add (can be negative for decrement)
    ///
    /// # Returns
    ///
    /// The new value after incrementing.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use libdictenstein::ARTrie;
    /// use libdictenstein::persistent_artrie_char::PersistentARTrieChar;
    ///
    /// let mut trie = PersistentARTrieChar::<i64>::create("counts.artc")?;
    /// let new_val = trie.increment("count", 1)?;  // Creates "count" = 1
    /// let new_val = trie.increment("count", 5)?;  // Updates to 6
    /// let new_val = trie.increment("count", -2)?; // Updates to 4
    /// ```
    fn increment(&self, term: &str, delta: i64) -> Result<i64>;
}

// === Atomic Operations Extension (requires serde) — DEPRECATED ===
//
// `ARTrieAtomicOps` was a transitional duplicate of `ARTrie`'s
// atomic-operation methods (`increment`, `upsert`) plus a `compare_and_swap`
// method. The duplicate signatures conflicted with the canonical `ARTrie`
// definitions:
//
//   ARTrie::increment   -> Result<i64>           (PersistentARTrieError)
//   ARTrieAtomicOps::increment -> Result<i64, String>   (String error)
//   ARTrie::upsert      -> Result<bool>
//   ARTrieAtomicOps::upsert    -> bool
//
// No impl of `ARTrieAtomicOps` ever shipped in this crate (verified by
// `rg 'impl.*ARTrieAtomicOps'` returning empty). The canonical methods on
// `ARTrie` cover the same surface with consistent return types.
// `compare_and_swap` remains available as an inherent method on
// `PersistentARTrie` / `PersistentARTrieChar` (`src/persistent_artrie/
// atomic_ops.rs:158`, `src/persistent_artrie_char/atomic_ops.rs:148`).
//
// The trait body is commented out (per CLAUDE.md "never delete to disable")
// and the `pub use` re-export at `src/lib.rs` is replaced with a
// `#[deprecated]` empty re-export so existing callers that named the trait
// receive a compiler warning rather than a silent break.
//
// Removal scheduled: next major version.
//
// pub trait ARTrieAtomicOps: ARTrie
// where
//     Self::Value: serde::Serialize + serde::de::DeserializeOwned,
// {
//     fn increment(&self, term: &str, delta: i64) -> std::result::Result<i64, String>;
//     fn upsert(&self, term: &str, value: Self::Value) -> bool;
//     fn compare_and_swap(
//         &self,
//         term: &str,
//         expected: Option<Self::Value>,
//         new_value: Self::Value,
//     ) -> bool;
// }

/// Deprecated placeholder for the removed `ARTrieAtomicOps` extension trait.
///
/// All methods that lived on it (`increment`, `upsert`, plus the
/// never-implemented `compare_and_swap`) are now on [`ARTrie`] directly. Use
/// the canonical [`ARTrie::increment`] / [`ARTrie::upsert`] methods, or the
/// inherent `compare_and_swap` on each persistent-ARTrie type.
#[deprecated(note = "ARTrieAtomicOps was a transitional duplicate of ARTrie's \
            atomic-op methods. Use ARTrie::increment / ARTrie::upsert; for \
            compare_and_swap use the inherent method on each persistent-ARTrie \
            type. This trait will be removed in the next major version.")]
pub trait ARTrieAtomicOps: ARTrie {}

// === Eviction Extension Trait ===

/// Extension trait for memory pressure-driven node eviction.
///
/// Implemented by all persistent ARTrie variants that support bounded-memory
/// operation. Enables SQLite-style memory management where nodes are evicted
/// to disk when memory pressure is detected.
///
/// # Architecture
///
/// ```text
/// ┌─────────────────────────────────────────────────────────────────┐
/// │                    PersistentARTrie<V>                          │
/// ├─────────────────────────────────────────────────────────────────┤
/// │  MemoryPressureMonitor (background thread)                      │
/// │    ↓ callback on Low/Critical pressure                          │
/// │  EvictionCoordinator                                            │
/// │    ↓ queues eviction request                                    │
/// │  Eviction Thread (async)                                        │
/// │    ├─ Wait for epoch quiescence (no old-epoch readers)          │
/// │    ├─ Select cold nodes via LRU/access tracking                 │
/// │    └─ Atomically swap ChildNode → DiskRef                       │
/// └─────────────────────────────────────────────────────────────────┘
/// ```
///
/// # Example
///
/// ```rust,ignore
/// use libdictenstein::persistent_artrie::PersistentARTrie;
/// use libdictenstein::EvictableARTrie;
/// use libdictenstein::persistent_artrie::EvictionConfig;
///
/// let mut trie = PersistentARTrie::<()>::create("words.part")?;
///
/// // Enable memory pressure-driven eviction
/// let config = EvictionConfig::default();
/// trie.enable_eviction(config)?;
///
/// // Normal operations continue...
/// trie.insert("hello");
/// trie.checkpoint()?;
///
/// // Eviction happens automatically when memory pressure is detected
/// let stats = trie.eviction_stats();
/// println!("Nodes evicted: {}", stats.nodes_evicted);
///
/// // Disable eviction when done
/// trie.disable_eviction()?;
/// ```
pub trait EvictableARTrie: ARTrie {
    /// Enable memory pressure-driven eviction.
    ///
    /// This starts a background eviction thread that monitors memory pressure
    /// and evicts cold nodes to disk when pressure is detected. Nodes are
    /// evicted in LRU order (coldest first) to keep hot data in memory.
    ///
    /// # Arguments
    ///
    /// * `config` - Configuration for eviction behavior
    ///
    /// # Returns
    ///
    /// `Ok(())` on success, or an error if eviction could not be enabled.
    ///
    /// # Errors
    ///
    /// - If eviction is already enabled
    /// - If the eviction thread fails to start
    fn enable_eviction(
        &self,
        config: crate::persistent_artrie::eviction::EvictionConfig,
    ) -> Result<()>;

    /// Disable eviction and release resources.
    ///
    /// Stops the background eviction thread and releases any resources
    /// associated with eviction. Nodes currently in memory will remain
    /// in memory until the trie is closed.
    ///
    /// # Returns
    ///
    /// `Ok(())` on success, or an error if eviction could not be disabled.
    fn disable_eviction(&self) -> Result<()>;

    /// Check if eviction is currently enabled.
    fn eviction_enabled(&self) -> bool;

    /// Get eviction statistics.
    ///
    /// Returns a snapshot of eviction-related statistics including:
    /// - Total nodes evicted
    /// - Total bytes freed
    /// - Number of eviction cycles
    /// - Last eviction duration
    fn eviction_stats(&self) -> crate::persistent_artrie::eviction::EvictionStats;

    /// Manually trigger eviction (for testing/debugging).
    ///
    /// Forces an immediate eviction cycle, evicting up to `target_bytes`
    /// worth of nodes. This is primarily for testing; production code
    /// should rely on automatic memory pressure-driven eviction.
    ///
    /// # Arguments
    ///
    /// * `target_bytes` - Target amount of memory to free
    ///
    /// # Returns
    ///
    /// The number of nodes evicted and bytes freed.
    fn force_eviction(&self, target_bytes: usize) -> Result<(usize, usize)>;

    /// Record a node access for LRU tracking.
    ///
    /// Called during traversal to track which nodes are being accessed.
    /// Nodes with recent access are less likely to be evicted.
    ///
    /// This method is typically called internally during traversal and
    /// does not need to be called by user code.
    ///
    /// # Arguments
    ///
    /// * `path` - The path to the accessed node (sequence of edge labels)
    fn touch_node(&self, path: &[Self::Unit]);
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)]
    use super::*;

    // Test that the trait is object-safe (can be used as dyn ARTrie) is NOT expected
    // since we use associated types and generics. This is documented behavior.
    // Instead, we test that concrete implementations work with generic functions.

    #[allow(dead_code)]
    fn _test_generic_usage<T: ARTrie>(trie: &T)
    where
        T::Value: Default,
    {
        let _ = trie.len();
        let _ = trie.is_empty();
        let _ = trie.contains("test");
    }
}
