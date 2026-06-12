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
use crate::persistent_artrie::core::counter_codec;
use crate::persistent_artrie::core::key_encoding::ByteKey;
use crate::persistent_artrie::core::overlay::durable_write::DurableOverlayWrite;
use crate::value::DictionaryValue;

impl<V: DictionaryValue + serde::Serialize + serde::de::DeserializeOwned, S: BlockStorage>
    PersistentARTrie<V, S>
{
    /// Atomically increment a numeric value associated with a term.
    ///
    /// If the term doesn't exist, inserts it with `delta` as the initial value.
    /// Returns the new value as the native counter type `V` (`u64` for the byte
    /// counter; `i64` for an `i64`-typed trie) — NOT a lossy `i64` bit-pattern.
    /// `delta` stays a signed `i64` (a negative delta decrements; only a NEGATIVE
    /// RESULT, or a true `u64` overflow, is rejected).
    ///
    /// This operation is atomic: the read-modify-write is performed under a lock,
    /// and the result is logged to WAL before returning.
    pub fn increment(&mut self, term: &str, delta: i64) -> Result<V>
    where
        V: crate::value::Counter,
    {
        self.increment_bytes(term.as_bytes(), delta)
    }

    /// Atomically increment a value by term bytes.
    ///
    /// See [`increment`](Self::increment) for details. Returns the new value as the
    /// native counter type `V`.
    ///
    /// **M3 write-flip (C5):** under `route_overlay()` for the `V = u64` monomorph
    /// with a non-negative delta this routes to the proven Order-A
    /// [`try_increment_cas_durable`](Self::try_increment_cas_durable) (durable
    /// add-only `BatchIncrement`, commutative on replay) via the SAFE `Any` dispatch
    /// in [`super::lockfree_value_route::route_increment_bytes`], whose `i128` count
    /// is re-encoded into `V`. A `u64` byte counter DECREMENT, an `i64` counter, or
    /// arbitrary `V` routes to the general value-CAS path (no dropped functionality;
    /// it replaces the prior FALL-THROUGH to the owned body — an unranked owned write
    /// dropped on Overlay reopen = data loss). That value-CAS path writes to the
    /// overlay, the sole representation. Every counter-leaf read/write routes through
    /// `counter_codec` (the i128 substrate).
    pub fn increment_bytes(&mut self, term: &[u8], delta: i64) -> Result<V>
    where
        V: crate::value::Counter,
    {
        if let Some(routed) = super::lockfree_value_route::route_increment_bytes(self, term, delta)
        {
            // The route returns the new count as an i128; re-encode it into the
            // native counter type `V` (range-checked). An out-of-range count is an
            // overflow → LOUD error (never a silent wrap).
            return routed.and_then(|count_i128| {
                counter_codec::i128_to_counter_value::<V>(count_i128).ok_or_else(|| {
                    PersistentARTrieError::InvalidOperation(format!(
                        "increment overflow for term {:?}: new count {} is out of range for \
                         the counter value type",
                        String::from_utf8_lossy(term),
                        count_i128
                    ))
                })
            });
        }
        // A decrement / `i64` counter / arbitrary `V` → the general value-CAS path.
        self.increment_bytes_via_value_cas(term, delta)
    }

    /// **F2 — general byte overlay increment via the value-CAS path** (Counter-bound).
    /// Byte twin of char's `increment_via_value_cas`: the `route_increment_bytes` fast
    /// path is the u64 add-only seam (byte counter = u64 + non-negative); an `i64`
    /// counter or a DECREMENT routes here — read → `cur + delta` → CAS retry over the
    /// proven `compare_and_swap_cas_durable` (phantom-safe per `LockFreeOverlayValueCas`).
    /// Preserves the increment for all Counter types via the overlay (the prior overlay
    /// fall-through to the owned tree was an unranked write dropped on reopen = data loss).
    /// Returns the new value as the native counter type `V`. Every counter-leaf read/write
    /// routes through the `counter_codec` i128 substrate (range-checked), so a count
    /// above `i64::MAX` is honored and a below-zero / overflow result fails LOUD.
    fn increment_bytes_via_value_cas(&mut self, term: &[u8], delta: i64) -> Result<V>
    where
        V: crate::value::Counter,
    {
        loop {
            let cur_v: Option<V> =
                <Self as DurableOverlayWrite<ByteKey, V, S>>::value_read_faulting(self, term)?;
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
                    String::from_utf8_lossy(term),
                    cur_i128,
                    delta
                ))
            })?;
            let new_v: V =
                counter_codec::i128_to_counter_value::<V>(new_i128).ok_or_else(|| {
                    PersistentARTrieError::InvalidOperation(format!(
                        "increment overflow for term {:?}: new count {} is out of range for the \
                     counter value type",
                        String::from_utf8_lossy(term),
                        new_i128
                    ))
                })?;
            match <Self as DurableOverlayWrite<ByteKey, V, S>>::compare_and_swap_cas_durable_default(
                self,
                term,
                cur_v,
                new_v.clone(),
            )? {
                true => return Ok(new_v),
                false => {
                    std::hint::spin_loop();
                }
            }
        }
    }

    /// Get value by raw byte key, for callers that already have byte keys (e.g.,
    /// varint-encoded n-gram keys).
    ///
    /// **L3.3c:** the overlay is the sole representation, so this value-routes to the
    /// overlay (`overlay_get_value` → the SAFE `Any` dispatch). **Empty-string support
    /// (H5):** the empty term "" IS the overlay ROOT — `overlay_get_value(b"")` reads
    /// `root.get_value()` / `root.is_final()` (the write path publishes "" to the root
    /// via fresh-root-CAS, and reestablish republishes it on reopen), so "" routes to
    /// the overlay like any other term.
    #[inline]
    pub fn get_value_bytes(&self, term: &[u8]) -> Option<V>
    where
        V: Clone,
    {
        // L3.3c: the overlay is the sole representation. `Some(inner)` is the answer; an
        // outer `None` (overlay-ineligible `V`) is unreachable since the overlay is the
        // sole representation for all `V`, so `.flatten()` is exact.
        self.overlay_get_value(term).flatten()
    }

    /// Check containment by raw byte key, for callers that already have byte keys
    /// (e.g., varint-encoded n-gram keys).
    ///
    /// **L3.3c:** the overlay is the sole representation, so membership routes to the
    /// non-faulting lock-free overlay read (`contains_lockfree`). **Empty-string support
    /// (H5):** the empty term "" IS the overlay ROOT — `contains_bytes(b"")` routes to
    /// `contains_lockfree(b"")`, which reads `root.is_final()` (the write path publishes
    /// "" to the root via fresh-root-CAS, and reestablish republishes it on reopen), so
    /// "" is a first-class member like any other term.
    #[inline]
    pub fn contains_bytes(&self, term: &[u8]) -> bool {
        // `contains_lockfree` IS the inherent body the trait's `overlay_contains`
        // seam delegates to (the non-faulting in-memory overlay walk).
        self.contains_lockfree(term)
    }

    // L3.3c: removed — `unrouted_contains_bytes` / `unrouted_get_value_bytes` read the
    // deleted owned tree (`contains_impl` / `get_value_impl`); with the owned tree gone
    // there is nothing un-routed to read (the reestablish-cleared assertions they backed
    // are obsolete now that the owned representation no longer exists).

    /// Atomically update or insert a value.
    ///
    /// If the term exists, updates its value. If not, inserts the term with the value.
    /// This is atomic: the operation is logged to WAL before returning.
    ///
    /// Returns `true` if a new term was inserted, `false` if an existing term was updated.
    pub fn upsert(&self, term: &str, value: V) -> Result<bool> {
        self.upsert_bytes(term.as_bytes(), value)
    }

    /// Atomically upsert by term bytes.
    ///
    /// See [`upsert`](Self::upsert) for details.
    ///
    /// The overlay is the sole representation, so for ALL `V` this routes to the
    /// SHARED GENERIC Order-A `upsert_cas_durable_default` (last-writer = the
    /// root-CAS winner).
    pub fn upsert_bytes(&self, term: &[u8], value: V) -> Result<bool> {
        // G5 (NH1): under the overlay, route to the SHARED GENERIC durable UPSERT
        // (always-write) for ANY V — there is no owned tree to fall through to.
        <Self as DurableOverlayWrite<ByteKey, V, S>>::upsert_cas_durable_default(self, term, value)
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
    /// The overlay is the sole representation, so for ALL `V` this routes to the
    /// SHARED GENERIC overlay value-CAS (`compare_and_swap_cas_durable_default`):
    /// a bincode-BYTE compare with a per-iteration `expected`-recheck, Order-A
    /// durable.
    pub fn compare_and_swap_bytes(
        &mut self,
        term: &[u8],
        expected: Option<V>,
        new_value: V,
    ) -> Result<bool> {
        // G5 (NH2): under the overlay, route to the SHARED GENERIC overlay value-CAS
        // (bincode-BYTE compare, per-iteration `expected`-recheck, Order-A durable)
        // for ANY V — there is no owned tree to fall through to.
        <Self as DurableOverlayWrite<ByteKey, V, S>>::compare_and_swap_cas_durable_default(
            self, term, expected, new_value,
        )
    }

    /// Get the current value and increment atomically (fetch-and-add).
    ///
    /// Returns the value *before* the increment, as the native counter type `V`.
    ///
    /// Inherits the overlay route transitively — it calls
    /// [`increment`](Self::increment) → `increment_bytes`, which routes to the
    /// durable `try_increment_cas_durable` on the overlay (the sole representation).
    /// The returned "before" value is `new_value - delta`, computed in the
    /// `counter_codec` i128 substrate and re-encoded into `V` (range-checked).
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
    pub fn get_or_insert(&mut self, term: &str, default: V) -> Result<V> {
        self.get_or_insert_bytes(term.as_bytes(), default)
    }

    /// Get or insert by term bytes.
    ///
    /// See [`get_or_insert`](Self::get_or_insert) for details.
    ///
    /// The overlay is the sole representation, so for ALL `V` this routes to the
    /// SHARED GENERIC `get_or_insert_durable_default` — insert-if-absent via the
    /// durable [`insert_cas_with_value_durable`](Self::insert_cas_with_value_durable)
    /// then read the resulting value back (read-your-write).
    pub fn get_or_insert_bytes(&mut self, term: &[u8], default: V) -> Result<V> {
        // G5 (NH1): under the overlay, route to the SHARED GENERIC insert-once
        // get-or-insert (read-your-write) for ANY V — there is no owned tree to fall
        // through to (the NH1 data-loss fix).
        <Self as DurableOverlayWrite<ByteKey, V, S>>::get_or_insert_durable_default(
            self, term, default,
        )
    }
}
