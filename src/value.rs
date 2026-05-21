//! Generic value types for dictionary mappings.
//!
//! This module provides a flexible trait hierarchy for associating arbitrary values
//! with dictionary terms. This enables use cases like:
//!
//! - **Contextual code completion**: Map identifiers to scope IDs
//! - **Metadata-rich dictionaries**: Associate terms with tags, categories, or custom data
//! - **Multi-value mappings**: Store lists or sets of related information
//!
//! # Examples
//!
//! ```
//! use libdictenstein::value::*;
//!
//! // Simple literal values
//! let scope_id: u32 = 42;
//! assert!(scope_id.is_value());
//!
//! // Collection values
//! let scope_ids = vec![1, 2, 3];
//! assert!(scope_ids.is_value());
//!
//! // Filtering with predicates on scalar values
//! let scope: u32 = 20;
//! assert!(scope.matches_any(&|&id| id == 20));
//! assert!(!scope.matches_any(&|&id| id == 99));
//! ```

use std::collections::HashSet;
use std::hash::Hash;

#[cfg(feature = "persistent-artrie")]
use serde::{de::DeserializeOwned, Serialize};

/// Marker trait for types that can be stored as dictionary values.
///
/// Any type implementing `DictionaryValue` can be associated with terms in a dictionary.
/// The trait requires `Clone`, `Send`, and `Sync` to support concurrent access patterns
/// common in fuzzy search applications.
///
/// # Automatic Implementation
///
/// This trait is automatically implemented for common types including:
/// - Unit type `()` (for dictionaries without values)
/// - Primitives: `u8`, `u16`, `u32`, `u64`, `usize`, `i8`, `i16`, `i32`, `i64`, `isize`
/// - Strings: `String`, `&'static str`
/// - Collections: `Vec<T>`, `HashSet<T>`, `smallvec::SmallVec<A>`
///
/// # Custom Types
///
/// You can implement this trait for your own types:
///
/// ```
/// use libdictenstein::value::DictionaryValue;
/// use serde::{Serialize, Deserialize};
///
/// #[derive(Clone, Default, Serialize, Deserialize)]
/// struct Metadata {
///     category: String,
///     priority: u32,
/// }
///
/// impl DictionaryValue for Metadata {}
/// ```
///
/// # Serialization (persistent-artrie feature)
///
/// When the `persistent-artrie` feature is enabled, `DictionaryValue` additionally
/// requires `serde::Serialize + serde::de::DeserializeOwned` to support value persistence.
#[cfg(feature = "persistent-artrie")]
pub trait DictionaryValue:
    Clone + Default + Send + Sync + Unpin + 'static + Serialize + DeserializeOwned
{
    /// Returns `true` if this is a meaningful value (not unit type).
    ///
    /// Default implementation returns `true`. The unit type `()` overrides this
    /// to return `false` for backward compatibility with non-map dictionaries.
    fn is_value(&self) -> bool {
        true
    }
}

/// Marker trait for types that can be stored as dictionary values.
///
/// Any type implementing `DictionaryValue` can be associated with terms in a dictionary.
/// The trait requires `Clone`, `Send`, and `Sync` to support concurrent access patterns
/// common in fuzzy search applications.
///
/// # Automatic Implementation
///
/// This trait is automatically implemented for common types including:
/// - Unit type `()` (for dictionaries without values)
/// - Primitives: `u8`, `u16`, `u32`, `u64`, `usize`, `i8`, `i16`, `i32`, `i64`, `isize`
/// - Strings: `String`, `&'static str`
/// - Collections: `Vec<T>`, `HashSet<T>`, `smallvec::SmallVec<A>`
///
/// # Custom Types
///
/// You can implement this trait for your own types:
///
/// ```
/// use libdictenstein::value::DictionaryValue;
///
/// #[derive(Clone, Default)]
/// struct Metadata {
///     category: String,
///     priority: u32,
/// }
///
/// impl DictionaryValue for Metadata {}
/// ```
#[cfg(not(feature = "persistent-artrie"))]
pub trait DictionaryValue: Clone + Default + Send + Sync + Unpin + 'static {
    /// Returns `true` if this is a meaningful value (not unit type).
    ///
    /// Default implementation returns `true`. The unit type `()` overrides this
    /// to return `false` for backward compatibility with non-map dictionaries.
    fn is_value(&self) -> bool {
        true
    }
}

/// Trait for values that support filtering operations.
///
/// This trait enables efficient pruning of the search space during fuzzy queries
/// by testing predicates on values during graph traversal, rather than filtering
/// results after the fact.
///
/// # Atoms
///
/// Predicates operate on the value's **atom** type ([`Self::Atom`]):
///
/// - **Scalar values** (`u32`, `String`, `&'static str`, …) have `Atom = Self`,
///   so the predicate sees the value directly.
/// - **Collection values** (`Vec<T>`, `HashSet<T>`, `SmallVec<A>`) have
///   `Atom = T` (the element type), and predicates are applied per-element via
///   `self.iter().any(predicate)` / `self.iter().all(predicate)`.
///
/// # Performance
///
/// Filtering during traversal (using this trait) provides 10-100x speedups
/// compared to post-filtering, especially when the filter is highly selective.
///
/// # Examples
///
/// ```
/// use libdictenstein::value::FilterableValue;
///
/// // Scalar: predicate sees the value directly.
/// let scope: u32 = 4;
/// assert!(scope.matches_any(&|&id| id % 2 == 0));
/// assert!(!scope.matches_any(&|&id| id > 10));
///
/// // Collection: predicate is applied per element.
/// let scopes = vec![10u32, 20, 30];
/// assert!(scopes.matches_any(&|&id| id == 20));
/// assert!(scopes.matches_all(&|&id| id >= 10));
/// ```
pub trait FilterableValue: DictionaryValue {
    /// The atom type that predicates operate on.
    ///
    /// - For scalar values this is `Self`.
    /// - For collection values (`Vec<T>`, `HashSet<T>`, `SmallVec<A>`) this is
    ///   the element type, so predicates are `Fn(&T) -> bool`.
    type Atom: ?Sized;

    /// Tests if any atom of this value matches a predicate.
    ///
    /// For scalar values this tests the value itself with the predicate.
    /// For collection values this is equivalent to
    /// `self.iter().any(predicate)`.
    fn matches_any<F>(&self, predicate: &F) -> bool
    where
        F: Fn(&Self::Atom) -> bool;

    /// Tests if all atoms of this value match a predicate.
    ///
    /// For scalar values this is equivalent to [`Self::matches_any`].
    /// For collection values this is equivalent to
    /// `self.iter().all(predicate)`.
    fn matches_all<F>(&self, predicate: &F) -> bool
    where
        F: Fn(&Self::Atom) -> bool;
}

// =============================================================================
// Implementations for unit type (backward compatibility)
// =============================================================================

impl DictionaryValue for () {
    fn is_value(&self) -> bool {
        false // Unit type has no meaningful value
    }
}

impl FilterableValue for () {
    type Atom = ();

    fn matches_any<F>(&self, _predicate: &F) -> bool
    where
        F: Fn(&Self::Atom) -> bool,
    {
        true // No filter, accept everything
    }

    fn matches_all<F>(&self, _predicate: &F) -> bool
    where
        F: Fn(&Self::Atom) -> bool,
    {
        true // No filter, accept everything
    }
}

// =============================================================================
// Implementations for primitive types
// =============================================================================

macro_rules! impl_primitive_value {
    ($($t:ty),*) => {
        $(
            impl DictionaryValue for $t {}

            impl FilterableValue for $t {
                type Atom = Self;

                fn matches_any<F>(&self, predicate: &F) -> bool
                where
                    F: Fn(&Self::Atom) -> bool,
                {
                    predicate(self)
                }

                fn matches_all<F>(&self, predicate: &F) -> bool
                where
                    F: Fn(&Self::Atom) -> bool,
                {
                    predicate(self)
                }
            }
        )*
    };
}

impl_primitive_value!(u8, u16, u32, u64, usize, i8, i16, i32, i64, isize, bool, char);

// =============================================================================
// Implementations for string types
// =============================================================================

impl DictionaryValue for String {}

impl FilterableValue for String {
    type Atom = Self;

    fn matches_any<F>(&self, predicate: &F) -> bool
    where
        F: Fn(&Self::Atom) -> bool,
    {
        predicate(self)
    }

    fn matches_all<F>(&self, predicate: &F) -> bool
    where
        F: Fn(&Self::Atom) -> bool,
    {
        predicate(self)
    }
}

// Note: &'static str does not implement DeserializeOwned, so it cannot be used
// as a DictionaryValue when persistent-artrie is enabled. Use String instead.
#[cfg(not(feature = "persistent-artrie"))]
impl DictionaryValue for &'static str {}

#[cfg(not(feature = "persistent-artrie"))]
impl FilterableValue for &'static str {
    type Atom = Self;

    fn matches_any<F>(&self, predicate: &F) -> bool
    where
        F: Fn(&Self::Atom) -> bool,
    {
        predicate(self)
    }

    fn matches_all<F>(&self, predicate: &F) -> bool
    where
        F: Fn(&Self::Atom) -> bool,
    {
        predicate(self)
    }
}

// =============================================================================
// Implementations for Vec<T>
// =============================================================================

impl<T: DictionaryValue> DictionaryValue for Vec<T> {}

impl<T: FilterableValue> FilterableValue for Vec<T> {
    type Atom = T;

    fn matches_any<F>(&self, predicate: &F) -> bool
    where
        F: Fn(&Self::Atom) -> bool,
    {
        self.iter().any(predicate)
    }

    fn matches_all<F>(&self, predicate: &F) -> bool
    where
        F: Fn(&Self::Atom) -> bool,
    {
        self.iter().all(predicate)
    }
}

// =============================================================================
// Implementations for HashSet<T>
// =============================================================================

impl<T: DictionaryValue + Eq + Hash> DictionaryValue for HashSet<T> {}

impl<T: FilterableValue + Eq + Hash> FilterableValue for HashSet<T> {
    type Atom = T;

    fn matches_any<F>(&self, predicate: &F) -> bool
    where
        F: Fn(&Self::Atom) -> bool,
    {
        self.iter().any(predicate)
    }

    fn matches_all<F>(&self, predicate: &F) -> bool
    where
        F: Fn(&Self::Atom) -> bool,
    {
        self.iter().all(predicate)
    }
}

// =============================================================================
// Implementations for SmallVec<A>
// =============================================================================

impl<A: smallvec::Array + Send + Sync + Unpin + 'static> DictionaryValue for smallvec::SmallVec<A> where
    A::Item: DictionaryValue
{
}

impl<A: smallvec::Array + Send + Sync + Unpin + 'static> FilterableValue for smallvec::SmallVec<A>
where
    A::Item: FilterableValue,
{
    type Atom = A::Item;

    fn matches_any<F>(&self, predicate: &F) -> bool
    where
        F: Fn(&Self::Atom) -> bool,
    {
        self.iter().any(predicate)
    }

    fn matches_all<F>(&self, predicate: &F) -> bool
    where
        F: Fn(&Self::Atom) -> bool,
    {
        self.iter().all(predicate)
    }
}

// =============================================================================
// Helper functions for common filtering patterns
// =============================================================================

/// Tests if a collection contains a specific value.
///
/// # Examples
///
/// ```
/// use libdictenstein::value::contains;
///
/// let scopes = vec![10, 20, 30];
/// assert!(contains(&scopes, &20));
/// assert!(!contains(&scopes, &99));
/// ```
pub fn contains<T: PartialEq>(collection: &[T], value: &T) -> bool {
    collection.contains(value)
}

/// Tests if a collection contains any value matching a predicate.
///
/// # Examples
///
/// ```
/// use libdictenstein::value::any;
///
/// let scopes = vec![10, 20, 30];
/// assert!(any(&scopes, |&id| id > 25));
/// assert!(!any(&scopes, |&id| id > 100));
/// ```
pub fn any<T, F>(collection: &[T], predicate: F) -> bool
where
    F: Fn(&T) -> bool,
{
    collection.iter().any(predicate)
}

/// Tests if all values in a collection match a predicate.
///
/// # Examples
///
/// ```
/// use libdictenstein::value::all;
///
/// let scopes = vec![10, 20, 30];
/// assert!(all(&scopes, |&id| id > 5));
/// assert!(!all(&scopes, |&id| id > 15));
/// ```
pub fn all<T, F>(collection: &[T], predicate: F) -> bool
where
    F: Fn(&T) -> bool,
{
    collection.iter().all(predicate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unit_type() {
        let unit = ();
        assert!(!unit.is_value());
        assert!(unit.matches_any(&|_| false)); // Unit always matches
        assert!(unit.matches_all(&|_| false)); // Unit always matches
    }

    #[test]
    fn test_primitives() {
        let num: u32 = 42;
        assert!(num.is_value());
        assert!(num.matches_any(&|&x| x == 42));
        assert!(!num.matches_any(&|&x| x == 99));
        assert!(num.matches_all(&|&x| x > 0));
    }

    #[test]
    fn test_strings() {
        let s = String::from("hello");
        assert!(s.is_value());
        assert!(s.matches_any(&|x| x == "hello"));
        assert!(!s.matches_any(&|x| x == "world"));
    }

    #[test]
    fn test_vec_per_element() {
        let v: Vec<u32> = vec![1, 2, 3];
        assert!(v.is_value());

        // Per-element semantics: predicate sees each &u32.
        assert!(v.matches_any(&|&x| x == 2));
        assert!(!v.matches_any(&|&x| x == 99));
        assert!(v.matches_all(&|&x| x > 0));
        assert!(!v.matches_all(&|&x| x > 1));
    }

    #[test]
    fn test_hashset_per_element() {
        let mut set: HashSet<u32> = HashSet::new();
        set.insert(10);
        set.insert(20);
        set.insert(30);

        assert!(set.is_value());
        // Per-element semantics: predicate sees each &u32.
        assert!(set.matches_any(&|&x| x == 20));
        assert!(!set.matches_any(&|&x| x == 99));
        assert!(set.matches_all(&|&x| x >= 10));
        assert!(!set.matches_all(&|&x| x > 15));
    }

    #[test]
    fn test_smallvec_per_element() {
        let sv: smallvec::SmallVec<[u32; 4]> = smallvec::smallvec![5, 10, 15];

        assert!(sv.matches_any(&|&x| x == 10));
        assert!(!sv.matches_any(&|&x| x == 999));
        assert!(sv.matches_all(&|&x| x >= 5));
        assert!(!sv.matches_all(&|&x| x > 10));
    }

    #[test]
    fn test_helper_functions() {
        let nums = vec![1, 2, 3, 4, 5];

        assert!(contains(&nums, &3));
        assert!(!contains(&nums, &10));

        assert!(any(&nums, |&x| x > 3));
        assert!(!any(&nums, |&x| x > 10));

        assert!(all(&nums, |&x| x > 0));
        assert!(!all(&nums, |&x| x > 2));
    }
}
