//! Bijection-invariant tests for every `BijectiveDictionary` impl in the crate.
//!
//! Trait law under test:
//!
//! ```text
//! ∀ (k, v) in dictionary:
//!     get_value(k) == Some(v)  ⟺  get_term(&v).as_deref() == Some(k)
//! ```
//!
//! The previous `Option<&str>` trait signature forced the persistent-vocab
//! impls to stub the method to `None`, silently violating the invariant for
//! every caller. Switching to `Option<Cow<'_, str>>` (A1 in the crate-wide
//! tech-debt plan) lets each impl return the real reconstructed term — these
//! tests confirm the law holds across `BijectiveMap`,
//! `PersistentVocabARTrie`, and `SharedVocabARTrie`.

use std::borrow::Cow;

use libdictenstein::bijective::{BijectiveDictionary, BijectiveMap};
#[cfg(feature = "persistent-artrie")]
use libdictenstein::MappedDictionary;

#[test]
fn bijective_map_round_trips_via_trait() {
    let bimap = BijectiveMap::from_pairs([("alpha", 0u64), ("beta", 1), ("gamma", 2)]);

    for (term, value) in [("alpha", 0u64), ("beta", 1), ("gamma", 2)] {
        // Forward: get_value(term) == Some(value)
        assert_eq!(bimap.get_value(term), Some(value), "forward {term}");

        // Reverse via trait: get_term(&value) == Some(Cow::Owned(term.into()))
        let got = BijectiveDictionary::get_term(&bimap, &value);
        assert_eq!(got.as_deref(), Some(term), "reverse {value}");
        // BijectiveMap always clones into Cow::Owned (the read guard cannot
        // outlive the call).
        assert!(
            matches!(got, Some(Cow::Owned(_))),
            "BijectiveMap should return Cow::Owned, got {got:?}"
        );
    }

    // Missing values return None.
    assert!(BijectiveDictionary::get_term(&bimap, &999u64).is_none());
}

#[cfg(feature = "persistent-artrie")]
mod persistent_vocab {
    use super::*;

    use libdictenstein::persistent_artrie::vocab::PersistentVocabARTrie;
    use tempfile::tempdir;

    #[test]
    fn persistent_vocab_round_trips_via_trait() {
        let dir = tempdir().expect("create tempdir");
        let path = dir.path().join("vocab.dict");

        let vocab = PersistentVocabARTrie::create(&path).expect("create persistent vocab");
        let terms = ["apple", "banana", "cherry"];
        for t in &terms {
            vocab.insert(t).expect("insert term failed");
        }

        for (i, term) in terms.iter().enumerate() {
            let value = i as u64;

            // Forward
            assert_eq!(
                vocab.get_value(term),
                Some(value),
                "forward {term} → {value}"
            );

            // Reverse via trait — reconstructed on the fly, returned as
            // Cow::Owned.
            let got = BijectiveDictionary::get_term(&vocab, &value);
            assert_eq!(got.as_deref(), Some(*term), "reverse {value} → {term}");
            assert!(
                matches!(got, Some(Cow::Owned(_))),
                "PersistentVocabARTrie should return Cow::Owned, got {got:?}"
            );
        }

        assert!(BijectiveDictionary::get_term(&vocab, &9_999u64).is_none());
    }
}

#[cfg(feature = "persistent-artrie")]
mod shared_vocab {
    use super::*;

    use libdictenstein::persistent_artrie::vocab::{PersistentVocabARTrie, SharedVocabARTrie};
    use parking_lot::RwLock;
    use std::sync::Arc;
    use tempfile::tempdir;

    #[test]
    fn shared_vocab_round_trips_via_trait() {
        let dir = tempdir().expect("create tempdir");
        let path = dir.path().join("vocab.dict");

        let vocab = PersistentVocabARTrie::create(&path).expect("create persistent vocab");
        let shared: SharedVocabARTrie = Arc::new(RwLock::new(vocab));
        let terms = ["one", "two", "three"];
        {
            let g = shared.write();
            for t in &terms {
                g.insert(t).expect("insert term failed");
            }
        }

        for (i, term) in terms.iter().enumerate() {
            let value = i as u64;

            assert!(BijectiveDictionary::contains_value(&shared, &value));

            let got = BijectiveDictionary::get_term(&shared, &value);
            assert_eq!(got.as_deref(), Some(*term), "reverse {value} → {term}");
            assert!(
                matches!(got, Some(Cow::Owned(_))),
                "SharedVocabARTrie should return Cow::Owned, got {got:?}"
            );
        }

        assert!(BijectiveDictionary::get_term(&shared, &9_999u64).is_none());
    }
}
