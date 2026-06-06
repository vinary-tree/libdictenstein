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
    ///
    /// **M3 write-flip (C5):** under `route_overlay()` for the `V = i64` monomorph
    /// this routes to the proven Order-A
    /// [`try_increment_cas_durable`](Self::try_increment_cas_durable) (durable
    /// add-only `BatchIncrement`, commutative on replay) via the SAFE `Any`
    /// dispatch in [`super::lockfree_value_route::route_increment_bytes`]. The
    /// durable path applies the C4 non-negative bound (a negative delta is rejected
    /// LOUDLY rather than silently writing the owned tree the overlay path would not
    /// observe) and requires a UTF-8 key. Arbitrary `V` never reaches the routed
    /// branch (the overlay is enabled only for `V ∈ {(), i64}`). The owned body
    /// below is the verbatim pre-flip path (INERT until `route_overlay()` is true).
    pub fn increment_bytes(&mut self, term: &[u8], delta: i64) -> Result<i64> {
        if self.route_overlay() {
            if let Some(routed) = super::lockfree_value_route::route_increment_bytes(self, term, delta)
            {
                return routed;
            }
        }
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

        let new_value = current.checked_add(delta).ok_or_else(|| {
            PersistentARTrieError::InvalidOperation(format!(
                "increment overflow for term {:?}: {} + {} exceeds i64 range",
                String::from_utf8_lossy(term),
                current,
                delta
            ))
        })?;

        let value_bytes = crate::serialization::bincode_compat::serialize(&new_value)
            .map_err(|e| PersistentARTrieError::internal(format!("Serialization error: {}", e)))?;
        let v: V =
            crate::serialization::bincode_compat::deserialize(&value_bytes).map_err(|e| {
                PersistentARTrieError::internal(format!("Cannot create value from i64: {}", e))
            })?;

        let record = WalRecord::Increment {
            term: term.to_vec(),
            delta,
            result: new_value,
        };
        self.append_mutation_wal_record(record, "increment")?;

        self.remove_impl_core(term);
        self.insert_impl_core(term, Some(v));

        Ok(new_value)
    }

    /// Get value by raw byte key.
    ///
    /// Public wrapper around the private `get_value_impl` method for callers
    /// that already have byte keys (e.g., varint-encoded n-gram keys).
    ///
    /// **M3 read-flip (C6):** under `route_overlay()` the owned tree is empty
    /// (cleared on an Overlay-regime reopen), so this value-routes to the overlay
    /// (`overlay_get_value` → the SAFE `Any` dispatch: `i64` counter or `()`
    /// membership). **Empty-string support (H5):** the empty term "" IS the overlay
    /// ROOT — `overlay_get_value(b"")` reads `root.get_value()` / `root.is_final()`
    /// (the write path publishes "" to the root via fresh-root-CAS, and reestablish
    /// republishes it on reopen), so "" routes to the overlay like any other term
    /// (the former owned-only exception is removed). `Some(None)` from the overlay
    /// means handled-and-absent; `None` means an ineligible `V` (unreachable under
    /// `route_overlay()`), in which case we fall through to the owned read.
    #[inline]
    pub fn get_value_bytes(&self, term: &[u8]) -> Option<V>
    where
        V: Clone,
    {
        if self.route_overlay() {
            if let Some(routed) = self.overlay_get_value(term) {
                return routed;
            }
        }
        self.get_value_impl(term)
    }

    /// Check containment by raw byte key.
    ///
    /// Public wrapper around the private `contains_impl` method for callers
    /// that already have byte keys (e.g., varint-encoded n-gram keys).
    ///
    /// **M3 read-flip (C6):** under `route_overlay()` membership routes to the
    /// non-faulting lock-free overlay read (`overlay_contains`); the owned arm is
    /// the unchanged `contains_impl`. The empty term has no overlay representation,
    /// but `overlay_contains(b"")` returns whether the overlay ROOT is final (which
    /// it never is for a key-only overlay), matching the owned semantics for an
    /// absent empty term; callers needing the empty-term membership use the owned
    /// `get_value(b"")` exception above.
    #[inline]
    pub fn contains_bytes(&self, term: &[u8]) -> bool {
        if self.route_overlay() {
            // `contains_lockfree` IS the inherent body the trait's `overlay_contains`
            // seam delegates to (the non-faulting in-memory overlay walk).
            return self.contains_lockfree(term);
        }
        self.contains_impl(term)
    }

    /// **UN-routed** owned membership read — always reads the OWNED tree
    /// (`contains_impl`), never the overlay, regardless of `route_overlay()`. The
    /// byte twin of char's `owned_try_contains`. Used by the M2a/M3 reestablish tests
    /// to assert the owned tree was cleared AFTER reestablish (a routed `contains_bytes`
    /// would read the now-populated overlay). Named off the `owned_*` prefix so the
    /// D1 grep gate (which scans `fn owned_*` bodies for `contains(`) does not flag the
    /// `contains_impl` call.
    #[cfg_attr(not(test), allow(dead_code))]
    #[inline]
    pub(crate) fn unrouted_contains_bytes(&self, term: &[u8]) -> bool {
        self.contains_impl(term)
    }

    /// **UN-routed** owned value read — always reads the OWNED tree
    /// (`get_value_impl`), never the overlay. The byte twin of char's owned value
    /// reader. Used by the reestablish tests for the owned-cleared assertion. (Will
    /// also back the M4 recovery reestablish-survival assertions, the byte EDIT-3.)
    #[cfg_attr(not(test), allow(dead_code))]
    #[inline]
    pub(crate) fn unrouted_get_value_bytes(&self, term: &[u8]) -> Option<V>
    where
        V: Clone,
    {
        self.get_value_impl(term)
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
    ///
    /// **M3 write-flip (C5):** under `route_overlay()` for `V = i64` this routes to
    /// the Order-A [`upsert_cas_durable`](Self::upsert_cas_durable) (last-writer =
    /// the root-CAS winner) via the SAFE `Any` dispatch
    /// ([`super::lockfree_value_route::route_upsert_bytes`]); the durable path
    /// rejects a negative value (C4). Arbitrary `V` keeps the owned body. The owned
    /// body below is the verbatim pre-flip path (INERT until the flip).
    pub fn upsert_bytes(&mut self, term: &[u8], value: V) -> Result<bool> {
        if self.route_overlay() {
            if let Some(routed) = super::lockfree_value_route::route_upsert_bytes(self, term, &value)
            {
                return routed;
            }
        }
        let existed = self.contains_impl(term);

        let value_bytes = crate::serialization::bincode_compat::serialize(&value)
            .map_err(|e| PersistentARTrieError::internal(format!("Serialization error: {}", e)))?;

        let record = WalRecord::Upsert {
            term: term.to_vec(),
            value: value_bytes,
        };
        self.append_mutation_wal_record(record, "upsert")?;

        self.remove_impl_core(term);
        self.insert_impl_core(term, Some(value));

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
    ///
    /// **M3 reject (H4):** the byte overlay has NO value-level compare-and-swap
    /// primitive (only the root-version CAS arbitrates STRUCTURAL publication, not
    /// an expected-value match). Under `route_overlay()` this is rejected with
    /// `InvalidOperation` rather than writing the owned tree (which the overlay
    /// read/checkpoint path would not observe). Reachable only for the `i64`
    /// monomorph under the M4 flip; arbitrary `V` keeps `route_overlay()` false so
    /// the owned body runs.
    pub fn compare_and_swap_bytes(
        &mut self,
        term: &[u8],
        expected: Option<V>,
        new_value: V,
    ) -> Result<bool> {
        if self.route_overlay() {
            return Err(PersistentARTrieError::InvalidOperation(
                "compare_and_swap is not valid under the lock-free overlay write mode (no \
                 value-level CAS-with-expected primitive on the byte overlay); use \
                 OverlayWriteMode::OwnedTree"
                    .to_string(),
            ));
        }
        let current = self.get_value_impl(term);

        let (matches, expected_bytes) = match (&current, &expected) {
            (None, None) => (true, None),
            (Some(c), Some(e)) => {
                let c_bytes = crate::serialization::bincode_compat::serialize(c).map_err(|e| {
                    PersistentARTrieError::internal(format!("Serialization error: {}", e))
                })?;
                let e_bytes = crate::serialization::bincode_compat::serialize(e).map_err(|e| {
                    PersistentARTrieError::internal(format!("Serialization error: {}", e))
                })?;
                (c_bytes == e_bytes, Some(e_bytes))
            }
            _ => (false, None),
        };

        if !matches {
            return Ok(false);
        }

        let new_value_bytes = crate::serialization::bincode_compat::serialize(&new_value)
            .map_err(|e| PersistentARTrieError::internal(format!("Serialization error: {}", e)))?;

        let record = WalRecord::CompareAndSwap {
            term: term.to_vec(),
            expected: expected_bytes,
            new_value: new_value_bytes,
            success: true,
        };
        self.append_mutation_wal_record(record, "compare_and_swap")?;

        self.remove_impl_core(term);
        self.insert_impl_core(term, Some(new_value));

        Ok(true)
    }

    /// Get the current value and increment atomically (fetch-and-add).
    ///
    /// Returns the value *before* the increment.
    ///
    /// **M3 write-flip (C5):** inherits the overlay route transitively — it calls
    /// [`increment`](Self::increment) → `increment_bytes`, which routes to the
    /// durable `try_increment_cas_durable` under `route_overlay()`. The returned
    /// "before" value is `new_value - delta`, correct for both the owned and the
    /// overlay accumulated count.
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
    ///
    /// **M3 write-flip (C5):** under `route_overlay()` for `V = i64` this routes to
    /// the overlay — insert-if-absent via the durable
    /// [`insert_cas_with_value_durable`](Self::insert_cas_with_value_durable) then
    /// read the resulting value back (`get_lockfree`) — through the SAFE `Any`
    /// dispatch ([`super::lockfree_value_route::route_get_or_insert_bytes`]).
    /// Arbitrary `V` keeps the owned body. The owned body below is the verbatim
    /// pre-flip path (INERT until the flip).
    pub fn get_or_insert_bytes(&mut self, term: &[u8], default: V) -> Result<V> {
        if self.route_overlay() {
            if let Some(routed) =
                super::lockfree_value_route::route_get_or_insert_bytes(self, term, &default)
            {
                return routed;
            }
        }
        if let Some(v) = self.get_value_impl(term) {
            return Ok(v);
        }

        let value_bytes = crate::serialization::bincode_compat::serialize(&default)
            .map_err(|e| PersistentARTrieError::internal(format!("Serialization error: {}", e)))?;

        let record = WalRecord::Upsert {
            term: term.to_vec(),
            value: value_bytes,
        };
        self.append_mutation_wal_record(record, "get_or_insert")?;

        self.insert_impl_core(term, Some(default.clone()));

        Ok(default)
    }
}
