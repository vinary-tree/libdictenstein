//! Profile-typed zipper adapters for logical-symbol combinators.
//!
//! A zipper's native unit type alone is not always enough to identify its
//! semantics: raw bytes and UTF-8 code units are both `u8`. `ProfiledZipper`
//! carries an [`AtomProfile`] at the type level, so product combinators can
//! only compose zippers declared over the same logical profile.

use crate::zipper::{DictZipper, ValuedDictZipper};
use crate::{AtomProfile, AtomSequence, ProfileKind, VariableWidthProfile};
use core::marker::PhantomData;

/// A zipper paired with its logical atom profile.
#[derive(Debug)]
pub struct ProfiledZipper<Z, P>
where
    Z: DictZipper,
    P: AtomProfile<Atom = Z::Unit>,
{
    inner: Z,
    marker: PhantomData<P>,
}

impl<Z, P> Clone for ProfiledZipper<Z, P>
where
    Z: DictZipper,
    P: AtomProfile<Atom = Z::Unit>,
{
    fn clone(&self) -> Self {
        Self::new(self.inner.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::double_array_trie::ascii::DoubleArrayTrie;
    use crate::double_array_trie::zipper::DoubleArrayTrieZipper;
    use crate::Bytes;

    #[test]
    fn profile_wrapper_preserves_logical_navigation() {
        let dictionary = DoubleArrayTrie::from_terms(["cat", "car"]);
        let zipper =
            ProfiledZipper::<_, Bytes>::new(DoubleArrayTrieZipper::new_from_dict(&dictionary));
        let sequence = AtomSequence::<Bytes>::from_atoms(b"cat".iter().copied());
        let terminal = zipper.descend_sequence(&sequence).unwrap();
        assert!(terminal.is_final());
        assert_eq!(terminal.path(), b"cat".to_vec());
        assert_eq!(
            ProfiledZipper::<DoubleArrayTrieZipper, Bytes>::profile_kind(),
            ProfileKind::Bytes
        );

        let exclusion = DoubleArrayTrie::from_terms(["cat"]);
        let difference = crate::difference_zipper::DifferenceZipper::new(
            ProfiledZipper::<_, Bytes>::new(DoubleArrayTrieZipper::new_from_dict(&dictionary)),
            ProfiledZipper::<_, Bytes>::new(DoubleArrayTrieZipper::new_from_dict(&exclusion)),
        );
        let mut difference = difference;
        for atom in sequence.as_atoms() {
            difference = difference
                .descend(*atom)
                .expect("profile-compatible difference path");
        }
        assert!(!difference.is_final());
    }
}

impl<Z, P> ProfiledZipper<Z, P>
where
    Z: DictZipper,
    P: AtomProfile<Atom = Z::Unit>,
{
    /// Wrap a zipper with a compile-time logical profile witness.
    pub const fn new(inner: Z) -> Self {
        Self {
            inner,
            marker: PhantomData,
        }
    }

    /// Borrow the underlying zipper without changing its profile.
    pub const fn as_inner(&self) -> &Z {
        &self.inner
    }

    /// Consume the adapter and return the underlying zipper.
    pub fn into_inner(self) -> Z {
        self.inner
    }

    /// Return the stable profile identity carried by this adapter.
    pub const fn profile() -> VariableWidthProfile {
        P::PROFILE
    }

    /// Return the built-in profile kind carried by this adapter.
    pub const fn profile_kind() -> ProfileKind {
        P::KIND
    }

    /// Convert a logical sequence into the profile's native units.
    pub fn descend_sequence(&self, sequence: &AtomSequence<P>) -> Option<Self> {
        let mut current = self.clone();
        for atom in sequence.as_atoms() {
            current = current.descend(*atom)?;
        }
        Some(current)
    }
}

impl<Z, P> DictZipper for ProfiledZipper<Z, P>
where
    Z: DictZipper,
    P: AtomProfile<Atom = Z::Unit>,
{
    type Unit = Z::Unit;

    fn is_final(&self) -> bool {
        self.inner.is_final()
    }

    fn descend(&self, label: Self::Unit) -> Option<Self> {
        self.inner.descend(label).map(Self::new)
    }

    fn children(&self) -> impl Iterator<Item = (Self::Unit, Self)> {
        self.inner
            .children()
            .map(|(label, child)| (label, Self::new(child)))
    }

    fn path(&self) -> Vec<Self::Unit> {
        self.inner.path()
    }
}

impl<Z, P> ValuedDictZipper for ProfiledZipper<Z, P>
where
    Z: ValuedDictZipper,
    P: AtomProfile<Atom = Z::Unit>,
{
    type Value = Z::Value;

    fn value(&self) -> Option<Self::Value> {
        self.inner.value()
    }
}
