//! Trie compaction configuration, statistics, and progress reporting.
//!
//! Split out of the monolithic byte `dict_impl.rs` (lines ~386-479) as the
//! first piece of the Phase-5 decomposition. The compaction *execution*
//! logic still lives on `PersistentARTrie` in `dict_impl.rs`; only the
//! configuration / observability types live here.

/// Configuration for trie compaction operations.
///
/// Compaction rebuilds the trie from scratch, eliminating orphaned nodes
/// and fragmentation that accumulate from update/delete operations.
///
/// # Example
///
/// ```rust,ignore
/// use libdictenstein::persistent_artrie::{PersistentARTrie, CompactionConfig};
///
/// let mut trie = PersistentARTrie::<u64>::open("data.artrie")?;
///
/// // In-place compaction with default settings
/// let stats = trie.compact(CompactionConfig::default(), |progress| {
///     println!("{}: {:.1}%", progress.phase, progress.percent_complete);
/// })?;
///
/// println!("Saved {:.1}% space", stats.space_savings_percent);
/// ```
#[derive(Debug, Clone)]
pub struct CompactionConfig {
    /// Target output path.
    ///
    /// - `None` (default): In-place compaction via atomic rename
    /// - `Some(path)`: Write to a new file, leaving original unchanged
    pub output_path: Option<std::path::PathBuf>,

    /// Progress callback interval (in terms).
    ///
    /// The progress callback is invoked every `progress_interval` terms.
    /// Set to 0 to disable progress callbacks. Default: 10,000.
    pub progress_interval: usize,

    /// Whether to verify data integrity after compaction.
    ///
    /// When enabled, verifies that the compacted trie has the same term count
    /// as the original. Default: true.
    pub verify_after_compact: bool,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            output_path: None,
            progress_interval: 10_000,
            verify_after_compact: true,
        }
    }
}

/// Statistics from a completed compaction operation.
#[derive(Debug, Clone)]
pub struct CompactionStats {
    /// Number of terms copied to the compacted trie.
    pub terms_copied: u64,

    /// Original file size in bytes before compaction.
    pub original_bytes: u64,

    /// Compacted file size in bytes after compaction.
    pub compacted_bytes: u64,

    /// Percentage of space saved (0.0 to 100.0).
    ///
    /// Calculated as: `(1.0 - compacted_bytes / original_bytes) * 100.0`
    pub space_savings_percent: f64,

    /// Duration of the compaction operation in milliseconds.
    pub duration_ms: u64,
}

/// Progress information during compaction.
///
/// Passed to the progress callback during `compact()` to report status.
#[derive(Debug, Clone)]
pub struct CompactionProgress {
    /// Current phase of compaction.
    ///
    /// Possible values:
    /// - `"copying"`: Iterating and copying terms
    /// - `"checkpointing"`: Persisting to disk
    /// - `"verifying"`: Verifying data integrity
    /// - `"finalizing"`: Atomic rename (in-place mode only)
    pub phase: &'static str,

    /// Number of terms processed so far.
    pub terms_processed: u64,

    /// Estimated total number of terms.
    pub estimated_total: u64,

    /// Percentage complete (0.0 to 100.0).
    pub percent_complete: f32,
}
