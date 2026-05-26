//! WAL durability policy enum, shared across all persistent ARTrie variants.
//!
//! Previously lived in `persistent_artrie::dict_impl`. Promoted to core so
//! both byte and vocab variants can reference it without crossing variant
//! boundaries.

/// Configurable durability policy for WAL synchronization.
///
/// This enum controls when fsync is called after WAL writes, providing a
/// trade-off between durability guarantees and performance.
///
/// # ACID Durability Guarantees
///
/// | Policy      | Guarantee | fsync Frequency | Use Case |
/// |-------------|-----------|-----------------|----------|
/// | Immediate   | Full      | Before public mutation/commit acknowledgement | ACID compliance (default) |
/// | GroupCommit | Full      | Batched when coordinated, blocking fallback otherwise | High throughput with group-commit feature |
/// | Periodic    | Bounded   | Checkpoint only | Performance-critical, accepts bounded loss |
/// | None        | None      | Never           | Testing only - data loss on crash |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DurabilityPolicy {
    /// fsync before acknowledging public mutations or committed transactions.
    ///
    /// This is the default policy providing the strongest durability guarantee.
    /// Every acknowledged public mutation is immediately durable on disk.
    #[default]
    Immediate,

    /// fsync is handled by group commit coordinator when one is installed.
    ///
    /// This policy delegates sync responsibility to the group commit system,
    /// which batches multiple commits into a single fsync for better throughput.
    /// Direct WAL paths use a blocking sync fallback so successful public
    /// acknowledgements still imply the appended LSN is synced.
    GroupCommit,

    /// fsync only at checkpoint boundaries.
    ///
    /// Provides better performance but accepts bounded data loss (up to one
    /// checkpoint interval) on crash. Suitable for applications that can
    /// tolerate some data loss for performance.
    Periodic,

    /// No fsync — for testing only.
    ///
    /// **WARNING**: This policy provides no durability guarantee. Data may be
    /// lost on any system failure. Use only for testing or ephemeral data.
    None,
}
