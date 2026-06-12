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
use crate::persistent_artrie::core::counter_codec;
use crate::persistent_artrie::core::key_encoding::CharKey;
use crate::persistent_artrie::core::overlay::durable_write::DurableOverlayWrite;
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
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
        // ([`Self::increment_via_value_cas`]).
        self.increment_via_value_cas(term, delta)
    }

    /// **General overlay increment via the value-CAS path** (Counter-bound).
    ///
    /// The fast `route_increment` seam only handles `V = u64` with a non-negative delta
    /// (the add-only commutative `BatchIncrement`). To support `i64` counters and
    /// decrements, this routes any other Counter increment through a read → `cur +
    /// delta` → CAS retry loop over the proven
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

    /// Atomically update or insert a value.
    ///
    /// # Returns
    ///
    /// `true` if a new term was inserted, `false` if an existing term was updated.
    ///
    /// The overlay is the sole representation, so for ALL `V` this routes to the
    /// SHARED GENERIC thin Order-A `upsert_cas_durable_default` (last-writer = the
    /// root-CAS winner).
    pub fn upsert(&self, term: &str, value: V) -> Result<bool> {
        // G5 (NH1): under the overlay, route to the SHARED GENERIC durable UPSERT
        // (always-write) for ANY V — there is no owned tree to fall through to.
        <Self as DurableOverlayWrite<CharKey, V, S>>::upsert_cas_durable_default(
            self,
            term.as_bytes(),
            value,
        )
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
        // G5 (NH2): under the overlay, route to the SHARED GENERIC overlay value-CAS
        // — bincode-BYTE comparison (no `PartialEq` on `DictionaryValue`), a
        // per-iteration `expected`-recheck on the freshly-loaded root, Order-A
        // durable (append `Upsert{new}` THEN publish, burn the LSN on a recheck
        // miss). The overlay is the sole representation, so this runs for ANY `V` —
        // there is no owned tree to fall through to.
        <Self as DurableOverlayWrite<CharKey, V, S>>::compare_and_swap_cas_durable_default(
            self,
            term.as_bytes(),
            expected,
            new_value,
        )
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
        // G5 (NH1): under the overlay, route to the SHARED GENERIC insert-once
        // get-or-insert (read-your-write: present? return it : insert the default
        // once, then return; the durable insert is genuinely insert-once so
        // concurrent racers converge on ONE value). The overlay is the sole
        // representation, so this runs for ANY V — there is no owned tree to fall
        // through to (the NH1 data-loss fix: a fall-through owned write would be
        // unranked → dropped on Overlay reopen).
        <Self as DurableOverlayWrite<CharKey, V, S>>::get_or_insert_durable_default(
            self,
            term.as_bytes(),
            default,
        )
    }
}
