//! `lling-llang` integration — `IdempotentSemiring` ⇒ `Lattice`.
//!
//! Feature-gated on `lling-llang`. Extracted from `union_zipper.rs` (C6
//! dedup); re-exported from [`crate::union_zipper`] for back-compat.

#![cfg(feature = "lling-llang")]

use super::lattice::Lattice;

/// Marker trait for types that implement Lattice via IdempotentSemiring.
///
/// When the `lling-llang` feature is enabled, any `IdempotentSemiring` automatically
/// implements `Lattice` where `join = plus` (⊕). This is because for idempotent
/// semirings, the `plus` operation satisfies the join-semilattice properties.
///
/// **Note**: The `meet` operation cannot be derived from semirings because the
/// semiring `times` (⊗) operation represents path composition, not lattice meet.
/// For semirings that need both join and meet, implement `Lattice` explicitly.
pub trait SemiringLattice:
    lling_llang::semiring::Semiring + lling_llang::semiring::IdempotentSemiring
{
}

impl<S> SemiringLattice for S where
    S: lling_llang::semiring::Semiring + lling_llang::semiring::IdempotentSemiring
{
}

/// Adapter for using IdempotentSemiring as a join-only Lattice.
///
/// This wraps a semiring value and provides `Lattice` implementation where:
/// - `join` = semiring `plus` (⊕)
/// - `meet` = semiring `times` (⊗) - **Note**: This may not be semantically correct
///   for all semirings since `times` is typically path composition, not lattice meet.
///
/// For proper lattice semantics, consider implementing `Lattice` directly on your type.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[cfg_attr(
    all(feature = "lling-llang", feature = "persistent-artrie"),
    derive(serde::Serialize, serde::Deserialize)
)]
#[cfg_attr(
    all(feature = "lling-llang", feature = "persistent-artrie"),
    serde(transparent)
)]
pub struct SemiringLatticeWrapper<S>(pub S);

impl<
        S: lling_llang::semiring::Semiring
            + lling_llang::semiring::IdempotentSemiring
            + Clone
            + Send
            + Sync,
    > Lattice for SemiringLatticeWrapper<S>
{
    #[inline]
    fn join(&self, other: &Self) -> Self {
        SemiringLatticeWrapper(self.0.plus(&other.0))
    }

    #[inline]
    fn meet(&self, other: &Self) -> Self {
        // Note: times is path composition, not necessarily lattice meet.
        // This works for some semirings (e.g., Boolean where times = AND)
        // but may not have correct semantics for others (e.g., Tropical where times = +).
        SemiringLatticeWrapper(self.0.times(&other.0))
    }
}

// Implement DictionaryValue for SemiringLatticeWrapper so it can be used with dictionaries
// When persistent-artrie is NOT enabled: basic bounds only
#[cfg(not(feature = "persistent-artrie"))]
impl<S: Clone + Default + Send + Sync + Unpin + 'static> crate::value::DictionaryValue
    for SemiringLatticeWrapper<S>
{
}

// When persistent-artrie IS enabled: require Serialize + DeserializeOwned
#[cfg(feature = "persistent-artrie")]
impl<
        S: Clone
            + Default
            + Send
            + Sync
            + Unpin
            + 'static
            + serde::Serialize
            + serde::de::DeserializeOwned,
    > crate::value::DictionaryValue for SemiringLatticeWrapper<S>
{
}
