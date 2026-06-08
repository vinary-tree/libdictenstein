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
use crate::persistent_artrie_core::counter_codec;
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
    /// Returns the new value as the native counter type `V` (`u64` for the n-gram
    /// counter; `i64` for an `i64`-typed trie) — NOT a lossy `i64` bit-pattern.
    /// `delta` stays a signed `i64` (a negative delta decrements; only a NEGATIVE
    /// RESULT, or a true `u64` overflow, is rejected).
    ///
    /// **Flip routing (design §2):** for the `V = u64` monomorph with `delta >= 0`
    /// and `route_overlay()`, this routes to the proven Order-A
    /// [`try_increment_cas_durable`](Self::try_increment_cas_durable) (add-only
    /// `BatchIncrement`, commutative-on-replay), whose `i128` count is re-encoded into
    /// `V`. A NEGATIVE delta or arbitrary `V` falls back to the general value-CAS path
    /// (dispatched via the SAFE `Any` downcast in `lockfree_value_route`, zero unsafe).
    /// Every counter-leaf read/write routes through `counter_codec` (the i128
    /// substrate), so a count above `i64::MAX` is honored and an overflow fails LOUD.
    pub fn increment(&mut self, term: &str, delta: i64) -> Result<V>
    where
        V: crate::value::Counter,
    {
        if self.route_overlay() {
            if let Some(routed) = super::lockfree_value_route::route_increment(self, term, delta) {
                // The route returns the new count as an i128; re-encode it into the
                // native counter type `V` (range-checked → LOUD overflow, no wrap).
                return routed.and_then(|count_i128| {
                    counter_codec::i128_to_counter_value::<V>(count_i128).ok_or_else(|| {
                        PersistentARTrieError::InvalidOperation(format!(
                            "increment overflow for term {:?}: new count {} is out of range for \
                             the counter value type",
                            term, count_i128
                        ))
                    })
                });
            }
            // F2: the `route_increment` fast path is the u64 ADD-ONLY commutative seam
            // (`V = u64` + non-negative delta). Any OTHER Counter increment — an `i64`
            // counter, or a DECREMENT — routes to the general value-CAS path
            // ([`Self::increment_via_value_cas`]): a read → `cur + delta` → CAS retry
            // over the proven `compare_and_swap` (phantom-safe per
            // `LockFreeOverlayValueCas.tla`). This preserves the owned path's
            // increment for ALL Counter types on the overlay (no dropped
            // functionality); `fetch_add` inherits it.
            return self.increment_via_value_cas(term, delta);
        }

        self.preflight_insert_with_value_no_wal(term)?;

        // Read the current count into the i128 substrate (confines the bincode
        // round-trip to `counter_codec`), add `delta` in i128, then re-encode the new
        // value into `V` (range-checked — an out-of-range result is a LOUD overflow,
        // never a silent wrap). `get` yields an owned `Option<V>`; borrow it.
        let cur_i128: i128 = match self.get(term) {
            Some(v) => counter_codec::counter_value_to_i128::<V>(&v).ok_or_else(|| {
                PersistentARTrieError::internal(
                    "increment: existing value is not an 8-byte counter leaf".to_string(),
                )
            })?,
            None => 0,
        };

        let new_i128 = cur_i128.checked_add(delta as i128).ok_or_else(|| {
            PersistentARTrieError::InvalidOperation(format!(
                "increment overflow for term {:?}: {} + {} overflows i128",
                term, cur_i128, delta
            ))
        })?;
        let new_value: V =
            counter_codec::i128_to_counter_value::<V>(new_i128).ok_or_else(|| {
                PersistentARTrieError::InvalidOperation(format!(
                "increment overflow for term {:?}: new count {} is out of range for the counter \
                 value type",
                term, new_i128
            ))
            })?;

        // Log to WAL first (routes through group commit if enabled). The
        // `Increment.result` field is `i64` (informational — recovery recomputes via
        // the delta); carry the i64 bit-pattern of the new count.
        let record = WalRecord::Increment {
            term: term.as_bytes().to_vec(),
            delta,
            result: counter_codec::counter_return_i64(new_i128),
        };
        self.append_to_wal(record)?;

        // Update the trie
        self.try_insert_impl_no_wal_with_value(term, new_value.clone())?;

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
    /// reopen). Every counter-leaf read/write routes through the `counter_codec` i128
    /// substrate (range-checked), so a count above `i64::MAX` is honored and a
    /// below-zero / overflow result fails LOUD. Returns the new value as the native
    /// counter type `V`.
    fn increment_via_value_cas(&mut self, term: &str, delta: i64) -> Result<V>
    where
        V: crate::value::Counter,
    {
        let key = term.as_bytes();
        loop {
            let cur_v: Option<V> =
                <Self as DurableOverlayWrite<CharKey, V, S>>::value_read_faulting(self, key)?;
            let cur_i128: i128 = match &cur_v {
                Some(v) => counter_codec::counter_value_to_i128::<V>(v).ok_or_else(|| {
                    PersistentARTrieError::internal(
                        "increment: existing value is not an 8-byte counter leaf".to_string(),
                    )
                })?,
                None => 0,
            };
            let new_i128 = cur_i128.checked_add(delta as i128).ok_or_else(|| {
                PersistentARTrieError::InvalidOperation(format!(
                    "increment overflow for term {:?}: {} + {} overflows i128",
                    term, cur_i128, delta
                ))
            })?;
            let new_v: V =
                counter_codec::i128_to_counter_value::<V>(new_i128).ok_or_else(|| {
                    PersistentARTrieError::InvalidOperation(format!(
                        "increment overflow for term {:?}: new count {} is out of range for the \
                     counter value type",
                        term, new_i128
                    ))
                })?;
            // CAS the recomputed value (expected = the value just read). A concurrent
            // change fails the CAS -> Ok(false) -> re-read + recompute + retry; the
            // burned durable record is dropped on Overlay reopen.
            if self.compare_and_swap(term, cur_v, new_v.clone())? {
                return Ok(new_v);
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
    #[allow(dead_code)] // L1.3: production-dead (the recovery appliers that called it are gone); retained for the in-crate owned white-box tests + L2/L3 owned-staging; removed with the owned path at L3.3
    pub(super) fn try_increment_impl_no_wal(&mut self, term: &str, delta: i64) -> Result<i64> {
        // Get current value. MUST be the OWNED read (not the E1-routed `get`): this
        // read-modify-write rebuilds the OWNED tree during crash recovery
        // (`apply_core_recovered_operation_no_wal` → BatchIncrement), and that rebuild
        // runs with `route_overlay()` already true (the trie was create-flipped before
        // the replay loop). Routing this read to the empty overlay would accumulate
        // every recovered delta from 0 — a silent counter under-count on reopen.
        //
        // The arithmetic runs in the `counter_codec` i128 substrate (full u64,
        // range-checked); the bincode round-trip is confined to the helper. The return
        // is the i64 bit-pattern of the new count (the caller only checks `.is_ok()`).
        // `owned_get` yields an owned `Option<V>`; borrow it.
        let current_i128: i128 = match self.owned_get(term) {
            Some(v) => counter_codec::counter_value_to_i128::<V>(&v).unwrap_or(0),
            None => 0,
        };

        let new_i128 = current_i128.checked_add(delta as i128).ok_or_else(|| {
            PersistentARTrieError::InvalidOperation(format!(
                "increment overflow for term {:?}: {} + {} overflows i128",
                term, current_i128, delta
            ))
        })?;
        let new_value: V =
            counter_codec::i128_to_counter_value::<V>(new_i128).ok_or_else(|| {
                PersistentARTrieError::InvalidOperation(format!(
                "increment overflow for term {:?}: new count {} is out of range for the counter \
                 value type",
                term, new_i128
            ))
            })?;

        // Update the trie (no WAL logging)
        self.try_insert_impl_no_wal_with_value(term, new_value)?;

        Ok(counter_codec::counter_return_i64(new_i128))
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
    pub fn upsert(&self, term: &str, value: V) -> Result<bool> {
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
        let current = self.get(term);

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
    /// Returns the value *before* the increment, as the native counter type `V`.
    /// The `new_value - delta` is computed in the `counter_codec` i128 substrate and
    /// re-encoded into `V` (range-checked).
    pub fn fetch_add(&mut self, term: &str, delta: i64) -> Result<V>
    where
        V: crate::value::Counter,
    {
        let new_v = self.increment(term, delta)?;
        let new_i128 = counter_codec::counter_value_to_i128::<V>(&new_v).ok_or_else(|| {
            PersistentARTrieError::internal(
                "fetch_add: increment result is not an 8-byte counter leaf".to_string(),
            )
        })?;
        counter_codec::i128_to_counter_value::<V>(new_i128 - delta as i128).ok_or_else(|| {
            PersistentARTrieError::InvalidOperation(format!(
                "fetch_add: pre-increment value {} is out of range for the counter value type",
                new_i128 - delta as i128
            ))
        })
    }

    /// Get or insert a default value atomically.
    ///
    /// If the term exists, returns its current value.
    /// If not, inserts the default value and returns it.
    pub fn get_or_insert(&self, term: &str, default: V) -> Result<V> {
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

        if let Some(v) = self.get(term) {
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
