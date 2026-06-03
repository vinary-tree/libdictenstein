//! Atomic read-modify-write operations for `PersistentARTrieChar<V, S>`.
//!
//! Split out of char `dict_impl_char.rs` (lines ~505-700, ~196 LOC)
//! as the nineteenth Phase-6 char sub-module. Methods covered:
//!
//! - `increment` / `try_increment_impl_no_wal` — i64 increment
//! - `upsert` — set value (insert-if-missing or update)
//! - `compare_and_swap` — atomic CAS update
//! - `fetch_add` — increment + return previous value
//! - `get_or_insert` — atomic default insertion

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::persistent_artrie::wal::WalRecord;
use crate::value::DictionaryValue;

impl<V: DictionaryValue + serde::Serialize + serde::de::DeserializeOwned, S: BlockStorage>
    super::PersistentARTrieChar<V, S>
{
    // Atomic Operations with WAL Support
    // ========================================================================

    /// Atomically increment a value by delta.
    ///
    /// If the term doesn't exist, inserts with `delta` as the initial value.
    /// The value must be serializable as an i64.
    ///
    /// # Returns
    ///
    /// The new value after incrementing.
    ///
    /// **Flip routing (design §2):** for the `V = u64` monomorph with `delta >= 0`
    /// and `route_overlay()`, this routes to the proven Order-A
    /// [`try_increment_cas_durable`](Self::try_increment_cas_durable) (add-only
    /// `BatchIncrement`, commutative-on-replay). A NEGATIVE delta or arbitrary `V`
    /// falls back to the owned tree (the design's "negative-delta increment
    /// unsupported" + "arbitrary V → forced OwnedTree" gaps — dispatched via the
    /// SAFE `Any` downcast in `lockfree_value_route`, zero unsafe).
    pub fn increment(&mut self, term: &str, delta: i64) -> Result<i64> {
        if self.route_overlay() {
            if let Some(routed) = super::lockfree_value_route::route_increment(self, term, delta) {
                return routed;
            }
            // delta < 0 or arbitrary V: fall through to the owned body.
        }

        self.preflight_insert_with_value_no_wal(term)?;

        // Get current value
        let current: i64 = if let Some(v) = self.get(term) {
            let bytes = crate::serialization::bincode_compat::serialize(&v).map_err(|e| {
                PersistentARTrieError::internal(format!("Failed to serialize value: {}", e))
            })?;
            if bytes.len() == 8 {
                i64::from_le_bytes(bytes.try_into().unwrap())
            } else {
                crate::serialization::bincode_compat::deserialize::<i64>(&bytes).map_err(|e| {
                    PersistentARTrieError::internal(format!("Failed to deserialize as i64: {}", e))
                })?
            }
        } else {
            0
        };

        let new_value = current.checked_add(delta).ok_or_else(|| {
            PersistentARTrieError::InvalidOperation(format!(
                "increment overflow for term {:?}: {} + {} exceeds i64 range",
                term, current, delta
            ))
        })?;

        // Create value from i64
        let value_bytes =
            crate::serialization::bincode_compat::serialize(&new_value).map_err(|e| {
                PersistentARTrieError::internal(format!("Failed to serialize new value: {}", e))
            })?;
        let v: V =
            crate::serialization::bincode_compat::deserialize(&value_bytes).map_err(|e| {
                PersistentARTrieError::internal(format!("Failed to deserialize as V: {}", e))
            })?;

        // Log to WAL first (routes through group commit if enabled)
        let record = WalRecord::Increment {
            term: term.as_bytes().to_vec(),
            delta,
            result: new_value,
        };
        self.append_to_wal(record)?;

        // Update the trie
        self.try_insert_impl_no_wal_with_value(term, v)?;

        Ok(new_value)
    }

    /// Internal increment without WAL logging (for batch operations).
    ///
    /// This is used by `commit_document()` for BatchIncrement operations where
    /// the WAL record has already been written.
    ///
    /// # Returns
    ///
    /// The new value after incrementing.
    pub(super) fn try_increment_impl_no_wal(&mut self, term: &str, delta: i64) -> Result<i64> {
        // Get current value
        let current: i64 = if let Some(v) = self.get(term) {
            let bytes = crate::serialization::bincode_compat::serialize(&v).unwrap_or_default();
            if bytes.len() == 8 {
                i64::from_le_bytes(bytes.try_into().unwrap())
            } else {
                crate::serialization::bincode_compat::deserialize::<i64>(&bytes).unwrap_or(0)
            }
        } else {
            0
        };

        let new_value = current.checked_add(delta).ok_or_else(|| {
            PersistentARTrieError::InvalidOperation(format!(
                "increment overflow for term {:?}: {} + {} exceeds i64 range",
                term, current, delta
            ))
        })?;

        // Create value from i64
        let value_bytes =
            crate::serialization::bincode_compat::serialize(&new_value).map_err(|e| {
                PersistentARTrieError::internal(format!("Failed to serialize new value: {}", e))
            })?;
        let v: V =
            crate::serialization::bincode_compat::deserialize(&value_bytes).map_err(|e| {
                PersistentARTrieError::internal(format!("Failed to deserialize as V: {}", e))
            })?;

        // Update the trie (no WAL logging)
        self.try_insert_impl_no_wal_with_value(term, v)?;

        Ok(new_value)
    }

    /// Atomically update or insert a value.
    ///
    /// # Returns
    ///
    /// `true` if a new term was inserted, `false` if an existing term was updated.
    ///
    /// **Flip routing (design §2):** for the `V = u64` monomorph with
    /// `route_overlay()`, routes to the thin Order-A
    /// [`upsert_cas_durable`](Self::upsert_cas_durable) (last-writer = the root-CAS
    /// winner). Arbitrary `V` falls back to the owned tree (SAFE `Any` dispatch).
    pub fn upsert(&mut self, term: &str, value: V) -> Result<bool> {
        if self.route_overlay() {
            if let Some(routed) = super::lockfree_value_route::route_upsert(self, term, &value) {
                return routed;
            }
            // Arbitrary V under LockFreeOverlay: fall through to the owned body.
        }

        self.preflight_insert_with_value_no_wal(term)?;
        let existed = self.contains(term);

        // Log to WAL first (routes through group commit if enabled)
        let value_bytes = crate::serialization::bincode_compat::serialize(&value).map_err(|e| {
            PersistentARTrieError::internal(format!("Failed to serialize value: {}", e))
        })?;
        let record = WalRecord::Upsert {
            term: term.as_bytes().to_vec(),
            value: value_bytes,
        };
        self.append_to_wal(record)?;

        // Update the trie
        self.try_insert_impl_no_wal_with_value(term, value)?;

        Ok(!existed)
    }

    /// Atomically compare and swap a value.
    ///
    /// Updates the value only if the current value matches `expected`.
    ///
    /// # Returns
    ///
    /// `true` if the swap succeeded, `false` if the current value didn't match expected.
    pub fn compare_and_swap(
        &mut self,
        term: &str,
        expected: Option<V>,
        new_value: V,
    ) -> Result<bool> {
        // Flip gap (design §1, named residual): the overlay has no value-level
        // compare-and-swap primitive (only the root-version CAS arbitrates
        // structural publication, not an expected-value match). Reject under
        // `route_overlay()` rather than write the owned tree (which the overlay
        // read/checkpoint path would not observe). Reachable only for the u64
        // monomorph under the F5 default; arbitrary V keeps `lockfree_root = None`
        // so `route_overlay()` is false and the owned body runs.
        if self.route_overlay() {
            return Err(PersistentARTrieError::InvalidOperation(
                "compare_and_swap is not supported under the lock-free overlay write mode (no \
                 value-level CAS primitive); use OverlayWriteMode::OwnedTree"
                    .to_string(),
            ));
        }

        self.preflight_insert_with_value_no_wal(term)?;
        let current = self.get(term).cloned();

        // Check if current matches expected
        let (matches, expected_bytes) = match (&current, &expected) {
            (None, None) => (true, None),
            (Some(c), Some(e)) => {
                let c_bytes = crate::serialization::bincode_compat::serialize(c).map_err(|e| {
                    PersistentARTrieError::internal(format!("Failed to serialize value: {}", e))
                })?;
                let e_bytes = crate::serialization::bincode_compat::serialize(e).map_err(|e| {
                    PersistentARTrieError::internal(format!("Failed to serialize value: {}", e))
                })?;
                (c_bytes == e_bytes, Some(e_bytes))
            }
            _ => (false, None),
        };

        if matches {
            // Log to WAL first (routes through group commit if enabled)
            let new_value_bytes = crate::serialization::bincode_compat::serialize(&new_value)
                .map_err(|e| {
                    PersistentARTrieError::internal(format!("Failed to serialize value: {}", e))
                })?;
            let record = WalRecord::CompareAndSwap {
                term: term.as_bytes().to_vec(),
                expected: expected_bytes,
                new_value: new_value_bytes,
                success: true,
            };
            self.append_to_wal(record)?;

            // Update the trie
            self.try_insert_impl_no_wal_with_value(term, new_value)?;
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
        // Flip routing (design §2): for the u64 monomorph under `route_overlay()`,
        // insert-if-absent into the overlay then read the resulting value back.
        // Arbitrary V falls back to the owned body (SAFE `Any` dispatch).
        if self.route_overlay() {
            if let Some(routed) =
                super::lockfree_value_route::route_get_or_insert(self, term, &default)
            {
                return routed;
            }
        }

        self.preflight_insert_with_value_no_wal(term)?;

        if let Some(v) = self.get(term).cloned() {
            return Ok(v);
        }

        // Log to WAL first (routes through group commit if enabled)
        let value_bytes =
            crate::serialization::bincode_compat::serialize(&default).map_err(|e| {
                PersistentARTrieError::internal(format!("Failed to serialize value: {}", e))
            })?;
        let record = WalRecord::Insert {
            term: term.as_bytes().to_vec(),
            value: Some(value_bytes),
        };
        self.append_to_wal(record)?;

        // Insert the default value
        self.try_insert_impl_no_wal_with_value(term, default.clone())?;

        Ok(default)
    }
}
