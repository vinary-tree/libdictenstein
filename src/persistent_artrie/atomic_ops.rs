//! Atomic read-modify-write operations for `PersistentARTrie<V, S>`.
//!
//! Split out of byte `dict_impl.rs` (lines ~5674-6027, ~354 LOC) as
//! the eighth Phase-5 byte sub-module. These operations provide
//! lock-free atomic semantics for concurrent access: while the
//! underlying storage uses `RwLock`, the API ensures atomic
//! read-modify-write semantics through CAS (Compare-And-Swap)
//! patterns and WAL logging.
//!
//! Methods covered:
//! - `increment` / `increment_bytes` / `fetch_add`
//! - `upsert` / `upsert_bytes`
//! - `compare_and_swap` / `compare_and_swap_bytes`
//! - `get_or_insert` / `get_or_insert_bytes`
//! - `get_value_bytes` / `contains_bytes` (byte-key lookup wrappers)

use super::block_storage::BlockStorage;
use super::dict_impl::PersistentARTrie;
use super::error::{PersistentARTrieError, Result};
use super::wal::WalRecord;
use crate::value::DictionaryValue;

impl<V: DictionaryValue + serde::Serialize + serde::de::DeserializeOwned, S: BlockStorage>
    PersistentARTrie<V, S>
{
    /// Atomically increment a numeric value associated with a term.
    ///
    /// If the term doesn't exist, inserts it with `delta` as the initial value.
    /// If the term exists but the value cannot be interpreted as i64, returns an error.
    ///
    /// This operation is atomic: the read-modify-write is performed under a lock,
    /// and the result is logged to WAL before returning.
    pub fn increment(&mut self, term: &str, delta: i64) -> Result<i64> {
        self.increment_bytes(term.as_bytes(), delta)
    }

    /// Atomically increment a value by term bytes.
    ///
    /// See [`increment`](Self::increment) for details.
    pub fn increment_bytes(&mut self, term: &[u8], delta: i64) -> Result<i64> {
        let current: i64 = match self.get_value_impl(term) {
            Some(v) => {
                let bytes = crate::serialization::bincode_compat::serialize(&v).map_err(|e| {
                    PersistentARTrieError::internal(format!("Serialization error: {}", e))
                })?;
                if bytes.len() == 8 {
                    i64::from_le_bytes(bytes.try_into().expect("expected 8 bytes"))
                } else {
                    crate::serialization::bincode_compat::deserialize::<i64>(&bytes).map_err(
                        |e| {
                            PersistentARTrieError::internal(format!(
                                "Value cannot be interpreted as i64: {}",
                                e
                            ))
                        },
                    )?
                }
            }
            None => 0,
        };

        let new_value = current + delta;

        let value_bytes = crate::serialization::bincode_compat::serialize(&new_value)
            .map_err(|e| PersistentARTrieError::internal(format!("Serialization error: {}", e)))?;
        let v: V =
            crate::serialization::bincode_compat::deserialize(&value_bytes).map_err(|e| {
                PersistentARTrieError::internal(format!("Cannot create value from i64: {}", e))
            })?;

        self.remove_impl_core(term);
        self.insert_impl_core(term, Some(v));

        if let Some(ref wal_writer) = self.wal_writer {
            let record = WalRecord::Increment {
                term: term.to_vec(),
                delta,
                result: new_value,
            };
            wal_writer.append(record).map_err(|e| {
                PersistentARTrieError::io_error(
                    "increment",
                    "WAL",
                    std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                )
            })?;
        }

        Ok(new_value)
    }

    /// Get value by raw byte key.
    ///
    /// Public wrapper around the private `get_value_impl` method for callers
    /// that already have byte keys (e.g., varint-encoded n-gram keys).
    #[inline]
    pub fn get_value_bytes(&self, term: &[u8]) -> Option<V>
    where
        V: Clone,
    {
        self.get_value_impl(term)
    }

    /// Check containment by raw byte key.
    ///
    /// Public wrapper around the private `contains_impl` method for callers
    /// that already have byte keys (e.g., varint-encoded n-gram keys).
    #[inline]
    pub fn contains_bytes(&self, term: &[u8]) -> bool {
        self.contains_impl(term)
    }

    /// Atomically update or insert a value.
    ///
    /// If the term exists, updates its value. If not, inserts the term with the value.
    /// This is atomic: the operation is logged to WAL before returning.
    ///
    /// Returns `true` if a new term was inserted, `false` if an existing term was updated.
    pub fn upsert(&mut self, term: &str, value: V) -> Result<bool> {
        self.upsert_bytes(term.as_bytes(), value)
    }

    /// Atomically upsert by term bytes.
    ///
    /// See [`upsert`](Self::upsert) for details.
    pub fn upsert_bytes(&mut self, term: &[u8], value: V) -> Result<bool> {
        let existed = self.contains_impl(term);

        self.remove_impl_core(term);
        self.insert_impl_core(term, Some(value.clone()));

        let value_bytes = crate::serialization::bincode_compat::serialize(&value)
            .map_err(|e| PersistentARTrieError::internal(format!("Serialization error: {}", e)))?;

        if let Some(ref wal_writer) = self.wal_writer {
            let record = WalRecord::Upsert {
                term: term.to_vec(),
                value: value_bytes,
            };
            wal_writer.append(record).map_err(|e| {
                PersistentARTrieError::io_error(
                    "upsert",
                    "WAL",
                    std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                )
            })?;
        }

        Ok(!existed)
    }

    /// Atomically compare and swap a value.
    ///
    /// Updates the value only if the current value matches `expected`.
    /// This provides optimistic concurrency control.
    ///
    /// Returns `Ok(true)` if the swap succeeded, `Ok(false)` if the current
    /// value didn't match expected.
    pub fn compare_and_swap(
        &mut self,
        term: &str,
        expected: Option<V>,
        new_value: V,
    ) -> Result<bool> {
        self.compare_and_swap_bytes(term.as_bytes(), expected, new_value)
    }

    /// Atomically compare and swap by term bytes.
    ///
    /// See [`compare_and_swap`](Self::compare_and_swap) for details.
    pub fn compare_and_swap_bytes(
        &mut self,
        term: &[u8],
        expected: Option<V>,
        new_value: V,
    ) -> Result<bool> {
        let current = self.get_value_impl(term);

        let matches = match (&current, &expected) {
            (None, None) => true,
            (Some(c), Some(e)) => {
                let c_bytes = crate::serialization::bincode_compat::serialize(c).ok();
                let e_bytes = crate::serialization::bincode_compat::serialize(e).ok();
                c_bytes == e_bytes
            }
            _ => false,
        };

        let expected_bytes = expected
            .as_ref()
            .and_then(|e| crate::serialization::bincode_compat::serialize(e).ok());
        let new_value_bytes = crate::serialization::bincode_compat::serialize(&new_value)
            .map_err(|e| PersistentARTrieError::internal(format!("Serialization error: {}", e)))?;

        if matches {
            self.remove_impl_core(term);
            self.insert_impl_core(term, Some(new_value));
        }

        if let Some(ref wal_writer) = self.wal_writer {
            let record = WalRecord::CompareAndSwap {
                term: term.to_vec(),
                expected: expected_bytes,
                new_value: new_value_bytes,
                success: matches,
            };
            wal_writer.append(record).map_err(|e| {
                PersistentARTrieError::io_error(
                    "compare_and_swap",
                    "WAL",
                    std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                )
            })?;
        }

        Ok(matches)
    }

    /// Get the current value and increment atomically (fetch-and-add).
    ///
    /// Returns the value *before* the increment.
    pub fn fetch_add(&mut self, term: &str, delta: i64) -> Result<i64> {
        let new_value = self.increment(term, delta)?;
        Ok(new_value - delta)
    }

    /// Get or insert a default value atomically.
    ///
    /// If the term exists, returns its current value.
    /// If not, inserts the default value and returns it.
    pub fn get_or_insert(&mut self, term: &str, default: V) -> Result<V> {
        self.get_or_insert_bytes(term.as_bytes(), default)
    }

    /// Get or insert by term bytes.
    ///
    /// See [`get_or_insert`](Self::get_or_insert) for details.
    pub fn get_or_insert_bytes(&mut self, term: &[u8], default: V) -> Result<V> {
        if let Some(v) = self.get_value_impl(term) {
            return Ok(v);
        }

        self.insert_impl_core(term, Some(default.clone()));

        let value_bytes = crate::serialization::bincode_compat::serialize(&default)
            .map_err(|e| PersistentARTrieError::internal(format!("Serialization error: {}", e)))?;

        if let Some(ref wal_writer) = self.wal_writer {
            let record = WalRecord::Upsert {
                term: term.to_vec(),
                value: value_bytes,
            };
            wal_writer.append(record).map_err(|e| {
                PersistentARTrieError::io_error(
                    "get_or_insert",
                    "WAL",
                    std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                )
            })?;
        }

        Ok(default)
    }
}
