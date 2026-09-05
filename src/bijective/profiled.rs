//! Unit-preserving bidirectional dictionaries.
//!
//! [`ProfiledBijectiveMap`] is the native-unit counterpart to the legacy
//! string-oriented [`super::BijectiveMap`].  It keeps profile units in both
//! directions and therefore never coerces a byte, numeric, or other logical
//! alphabet through UTF-8 text.

use crate::dynamic_dawg::DynamicDawgGeneric;
use crate::{AtomProfile, AtomSequence, CharUnit, DictionaryValue};
use arc_swap::ArcSwap;
use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;

/// A bidirectional map over one dictionary-unit alphabet.
///
/// The forward index is the shared lock-free DAWG; the reverse index is an
/// atomically published copy-on-write map.  Reverse lookup returns an owned
/// unit vector so readers never retain an internal snapshot after publication.
#[derive(Debug)]
pub struct ProfiledBijectiveMap<U: CharUnit, V: DictionaryValue + Eq + Hash> {
    forward: DynamicDawgGeneric<U, V>,
    reverse: Arc<ArcSwap<HashMap<V, Vec<U>>>>,
}

impl<U: CharUnit, V: DictionaryValue + Eq + Hash> Clone for ProfiledBijectiveMap<U, V> {
    fn clone(&self) -> Self {
        Self {
            forward: self.forward.clone(),
            reverse: Arc::new(ArcSwap::from_pointee((*self.reverse.load_full()).clone())),
        }
    }
}

impl<U: CharUnit, V: DictionaryValue + Eq + Hash> Default for ProfiledBijectiveMap<U, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<U: CharUnit, V: DictionaryValue + Eq + Hash> ProfiledBijectiveMap<U, V> {
    /// Construct an empty unit-preserving bijection.
    pub fn new() -> Self {
        Self {
            forward: DynamicDawgGeneric::new(),
            reverse: Arc::new(ArcSwap::from_pointee(HashMap::new())),
        }
    }

    /// Construct an empty map with reverse-index capacity reserved.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            forward: DynamicDawgGeneric::new(),
            reverse: Arc::new(ArcSwap::from_pointee(HashMap::with_capacity(capacity))),
        }
    }

    #[inline]
    fn reverse_snapshot(&self) -> Arc<HashMap<V, Vec<U>>> {
        self.reverse.load_full()
    }

    fn mutate_reverse<R, F>(&self, mut f: F) -> R
    where
        F: FnMut(&mut HashMap<V, Vec<U>>) -> (R, bool),
    {
        loop {
            let current = self.reverse_snapshot();
            let mut next = (*current).clone();
            let (result, changed) = f(&mut next);
            if !changed {
                return result;
            }
            let previous = self.reverse.compare_and_swap(&current, Arc::new(next));
            if Arc::ptr_eq(&previous, &current) {
                return result;
            }
            std::hint::spin_loop();
        }
    }

    /// Insert a unit sequence, panicking on a duplicate key or value.
    pub fn insert_units(&self, units: &[U], value: V) {
        self.try_insert_units(units, value)
            .unwrap_or_else(|error| panic!("ProfiledBijectiveMap::insert_units: {error:?}"));
    }

    /// Insert a unit sequence while preserving the bijection invariant.
    pub fn try_insert_units(&self, units: &[U], value: V) -> Result<(), super::InsertError> {
        if self.forward.get_units_value(units).is_some() {
            return Err(super::InsertError::DuplicateTerm);
        }
        let units = units.to_vec();
        let result = self.mutate_reverse(|reverse| {
            if reverse.contains_key(&value) {
                (Err(super::InsertError::DuplicateValue), false)
            } else {
                reverse.insert(value.clone(), units.clone());
                (Ok(()), true)
            }
        });
        result?;
        if !self.forward.insert_units_with_value(&units, value.clone()) {
            self.mutate_reverse(|reverse| {
                let remove = reverse
                    .get(&value)
                    .is_some_and(|existing| existing == &units);
                if remove {
                    reverse.remove(&value);
                }
                ((), remove)
            });
            return Err(super::InsertError::DuplicateTerm);
        }
        Ok(())
    }

    /// Construct a map from profile-owned atom sequences.
    pub fn from_atom_sequences_with_values<P, I>(entries: I) -> Self
    where
        P: AtomProfile<Atom = U>,
        I: IntoIterator<Item = (AtomSequence<P>, V)>,
    {
        let map = Self::new();
        for (sequence, value) in entries {
            map.insert_units(sequence.as_atoms(), value);
        }
        map
    }

    /// Look up a value by native logical units.
    #[inline]
    pub fn get_units_value(&self, units: &[U]) -> Option<V> {
        self.forward.get_units_value(units)
    }

    /// Look up the native logical units associated with a value.
    #[inline]
    pub fn get_units(&self, value: &V) -> Option<Vec<U>> {
        self.reverse_snapshot().get(value).cloned()
    }

    /// Test forward membership without allocating.
    #[inline]
    pub fn contains_units(&self, units: &[U]) -> bool {
        self.forward.get_units_value(units).is_some()
    }

    /// Test reverse membership.
    #[inline]
    pub fn contains_value(&self, value: &V) -> bool {
        self.reverse_snapshot().contains_key(value)
    }

    /// Number of bijective entries.
    #[inline]
    pub fn len(&self) -> usize {
        self.reverse_snapshot().len()
    }

    /// Whether the map contains no entries.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Iterate owned unit/value pairs in reverse-index order.
    pub fn iter_units(&self) -> impl Iterator<Item = (Vec<U>, V)> {
        self.reverse_snapshot()
            .iter()
            .map(|(value, units)| (units.clone(), value.clone()))
            .collect::<Vec<_>>()
            .into_iter()
    }

    /// Borrow the forward unit-native dictionary for specialized consumers.
    #[inline]
    pub fn forward(&self) -> &DynamicDawgGeneric<U, V> {
        &self.forward
    }
}

#[cfg(test)]
mod tests {
    use super::ProfiledBijectiveMap;
    use crate::{AtomSequence, U32};

    #[test]
    fn preserves_numeric_units_in_both_directions() {
        let map = ProfiledBijectiveMap::<u32, u16>::from_atom_sequences_with_values::<U32, _>([(
            AtomSequence::<U32>::from_atoms([0x100, 0x200]),
            7,
        )]);
        assert_eq!(map.get_units_value(&[0x100, 0x200]), Some(7));
        assert_eq!(map.get_units(&7), Some(vec![0x100, 0x200]));
        assert!(map.contains_units(&[0x100, 0x200]));
    }

    #[test]
    fn duplicate_insert_does_not_change_reverse_mapping() {
        let map = ProfiledBijectiveMap::<u8, u16>::new();
        map.insert_units(b"ab", 1);
        assert_eq!(
            map.try_insert_units(b"ab", 2),
            Err(crate::bijective::InsertError::DuplicateTerm)
        );
        assert_eq!(
            map.try_insert_units(b"cd", 1),
            Err(crate::bijective::InsertError::DuplicateValue)
        );
        assert_eq!(map.get_units(&1), Some(b"ab".to_vec()));
    }
}
