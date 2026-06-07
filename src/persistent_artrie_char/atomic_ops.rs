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
use crate::persistent_artrie_core::key_encoding::CharKey;
use crate::persistent_artrie_core::overlay::durable_write::DurableOverlayWrite;
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
    pub fn increment(&mut self, term: &str, delta: i64) -> Result<i64>
    where
        V: crate::value::Counter,
    {
        if self.route_overlay() {
            if let Some(routed) = super::lockfree_value_route::route_increment(self, term, delta) {
                return routed;
            }
            // F2: the `route_increment` fast path is the u64 ADD-ONLY commutative seam
            // (`V = u64` + non-negative delta). Any OTHER Counter increment — an `i64`
            // counter, or a DECREMENT — routes to the general value-CAS path
            // ([`Self::increment_via_value_cas`]): a read → `cur + delta` → CAS retry
            // over the proven `compare_and_swap` (phantom-safe per
            // `LockFreeOverlayValueCas.tla`). This preserves the owned path's
            // i64-arithmetic increment for ALL Counter types on the overlay (no dropped
            // functionality); `fetch_add` inherits it.
            return self.increment_via_value_cas(term, delta);
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

    /// **F2 — general overlay increment via the value-CAS path** (Counter-bound).
    ///
    /// The fast `route_increment` seam only handles `V = u64` with a non-negative delta
    /// (the add-only commutative `BatchIncrement`). To avoid dropping the owned path's
    /// increment for `i64` counters and for decrements, this routes any other Counter
    /// increment through a read → `cur + delta` → CAS retry loop over the proven
    /// `compare_and_swap` (overlay-routed to `compare_and_swap_cas_durable`, phantom-safe
    /// per `LockFreeOverlayValueCas.tla` — a lost CAS burns an unranked record dropped on
    /// reopen). The value is read/written as an 8-byte LE `i64` (valid for `{i64, u64}`),
    /// matching the owned increment's i64-arithmetic semantics exactly (incl. the same
    /// `i64::MAX`-bounded counter domain).
    fn increment_via_value_cas(&mut self, term: &str, delta: i64) -> Result<i64>
    where
        V: crate::value::Counter,
    {
        let key = term.as_bytes();
        loop {
            let cur_v: Option<V> = <Self as DurableOverlayWrite<CharKey, V, S>>::value_read_faulting(
                self, key,
            )?;
            let cur_i64: i64 = match &cur_v {
                Some(v) => {
                    let bytes =
                        crate::serialization::bincode_compat::serialize(v).map_err(|e| {
                            PersistentARTrieError::internal(format!(
                                "Failed to serialize value: {}",
                                e
                            ))
                        })?;
                    if bytes.len() == 8 {
                        i64::from_le_bytes(bytes.try_into().expect("expected 8 bytes"))
                    } else {
                        crate::serialization::bincode_compat::deserialize::<i64>(&bytes).map_err(
                            |e| {
                                PersistentARTrieError::internal(format!(
                                    "value cannot be interpreted as i64: {}",
                                    e
                                ))
                            },
                        )?
                    }
                }
                None => 0,
            };
            let new_i64 = cur_i64.checked_add(delta).ok_or_else(|| {
                PersistentARTrieError::InvalidOperation(format!(
                    "increment overflow for term {:?}: {} + {} exceeds i64 range",
                    term, cur_i64, delta
                ))
            })?;
            // i64 -> V (Counter): an 8-byte LE word reinterpreted as the counter type
            // (matches the owned increment); for u64 this carries the i64 bit pattern,
            // identical to the owned path's domain.
            let new_bytes =
                crate::serialization::bincode_compat::serialize(&new_i64).map_err(|e| {
                    PersistentARTrieError::internal(format!(
                        "Failed to serialize new value: {}",
                        e
                    ))
                })?;
            let new_v: V = crate::serialization::bincode_compat::deserialize(&new_bytes)
                .map_err(|e| {
                    PersistentARTrieError::internal(format!(
                        "increment result {} cannot be stored as the value type: {}",
                        new_i64, e
                    ))
                })?;
            // CAS the recomputed value (expected = the value just read). A concurrent
            // change fails the CAS -> Ok(false) -> re-read + recompute + retry; the
            // burned durable record is dropped on Overlay reopen.
            if self.compare_and_swap(term, cur_v, new_v)? {
                return Ok(new_i64);
            }
            std::hint::spin_loop();
        }
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
        // Get current value. MUST be the OWNED read (not the E1-routed `get`): this
        // read-modify-write rebuilds the OWNED tree during crash recovery
        // (`apply_core_recovered_operation_no_wal` → BatchIncrement), and that rebuild
        // runs with `route_overlay()` already true (the trie was create-flipped before
        // the replay loop). Routing this read to the empty overlay would accumulate
        // every recovered delta from 0 — a silent counter under-count on reopen.
        let current: i64 = if let Some(v) = self.owned_get(term) {
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
        // Flip F0/G5 (NH1): under the overlay, route to the SHARED GENERIC durable
        // UPSERT (always-write) for ANY V — NEVER fall through to owned. Eligible V
        // now; arbitrary V at F2.
        if self.route_overlay() {
            return <Self as DurableOverlayWrite<CharKey, V, S>>::upsert_cas_durable_default(
                self,
                term.as_bytes(),
                value,
            );
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
        // Flip F0/G5 (NH2): under the overlay, route to the SHARED GENERIC overlay
        // value-CAS — bincode-BYTE comparison (no `PartialEq` on `DictionaryValue`),
        // a per-iteration `expected`-recheck on the freshly-loaded root, Order-A
        // durable (append `Upsert{new}` THEN publish, burn the LSN on a recheck
        // miss). This SUPERSEDES the prior reject (the design's NH2 regression fix):
        // the currently-eligible `V` ({(),u64}) gets working overlay CAS now, and
        // arbitrary `V` joins at the F2 eligibility flip. (Owned body below runs only
        // when `!route_overlay()` — ineligible `V`, or kill-switched.)
        if self.route_overlay() {
            return <Self as DurableOverlayWrite<CharKey, V, S>>::compare_and_swap_cas_durable_default(
                self,
                term.as_bytes(),
                expected,
                new_value,
            );
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
    pub fn fetch_add(&mut self, term: &str, delta: i64) -> Result<i64>
    where
        V: crate::value::Counter,
    {
        let new_value = self.increment(term, delta)?;
        Ok(new_value - delta)
    }

    /// Get or insert a default value atomically.
    ///
    /// If the term exists, returns its current value.
    /// If not, inserts the default value and returns it.
    pub fn get_or_insert(&mut self, term: &str, default: V) -> Result<V> {
        // Flip F0/G5 (NH1): under the overlay, route to the SHARED GENERIC
        // insert-once get-or-insert (read-your-write: present? return it : insert
        // the default once, then return; the durable insert is genuinely insert-once
        // so concurrent racers converge on ONE value). It NEVER falls through to the
        // owned tree for an overlay-routed trie — that is the NH1 data-loss fix (a
        // fall-through owned write would be unranked → dropped on Overlay reopen).
        // The currently-eligible V ({(),u64}) takes this now; arbitrary V at F2.
        // (Owned body below runs only when `!route_overlay()`.)
        if self.route_overlay() {
            return <Self as DurableOverlayWrite<CharKey, V, S>>::get_or_insert_durable_default(
                self,
                term.as_bytes(),
                default,
            );
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
