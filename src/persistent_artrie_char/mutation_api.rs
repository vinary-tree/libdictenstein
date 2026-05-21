//! Public mutation API for `PersistentARTrieChar<V, S>`.
//!
//! Split out of char `dict_impl_char.rs` (lines ~280-332, ~53 LOC)
//! as the twenty-third Phase-6 char sub-module. Methods covered:
//!
//! - `insert` — WAL-logged term-only insert
//! - `insert_with_value` — WAL-logged term+value insert
//! - `remove` — WAL-logged remove
//!
//! These wrap the `_no_wal` core helpers that stay in
//! `dict_impl_char.rs` and route every operation through
//! `append_to_wal` (which honors group commit when enabled).

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::persistent_artrie::wal::WalRecord;
use crate::value::DictionaryValue;

impl<V: DictionaryValue, S: BlockStorage> super::PersistentARTrieChar<V, S> {
    /// Insert a term with WAL logging
    pub fn insert(&mut self, term: &str) -> Result<bool> {
        // Log to WAL first (routes through group commit if enabled)
        let record = WalRecord::Insert {
            term: term.as_bytes().to_vec(),
            value: None,
        };
        self.append_to_wal(record)?;

        // Mark version as being written (odd = in-progress)
        self.version.begin_write();
        let result = self.insert_impl_no_wal(term);
        // Mark version as stable (even = complete)
        self.version.end_write();

        Ok(result)
    }

    /// Insert a term with an associated value and WAL logging
    pub fn insert_with_value(&mut self, term: &str, value: V) -> Result<bool> {
        // Log to WAL first (routes through group commit if enabled)
        let value_bytes = crate::serialization::bincode_compat::serialize(&value).map_err(|e| {
            PersistentARTrieError::internal(format!("Failed to serialize value: {}", e))
        })?;
        let record = WalRecord::Insert {
            term: term.as_bytes().to_vec(),
            value: Some(value_bytes),
        };
        self.append_to_wal(record)?;

        // Mark version as being written (odd = in-progress)
        self.version.begin_write();
        let result = self.insert_impl_no_wal_with_value(term, value);
        // Mark version as stable (even = complete)
        self.version.end_write();

        Ok(result)
    }

    /// Remove a term with WAL logging
    pub fn remove(&mut self, term: &str) -> Result<bool> {
        // Log to WAL first (routes through group commit if enabled)
        let record = WalRecord::Remove {
            term: term.as_bytes().to_vec(),
        };
        self.append_to_wal(record)?;

        // Mark version as being written
        self.version.begin_write();
        let result = self.remove_impl_no_wal(term);
        self.version.end_write();

        Ok(result)
    }
}
