#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::vocab::PersistentVocabARTrie;
use libdictenstein::{AtomSequence, UnicodeScalar};

#[test]
fn persistent_vocabulary_profile_sequence_round_trip() {
    let directory = tempfile::tempdir().expect("temporary vocabulary directory");
    let path = directory.path().join("profile-sequence.vocab");
    let vocabulary = PersistentVocabARTrie::create(&path).expect("create vocabulary");
    let sequence = AtomSequence::<UnicodeScalar>::from_atoms("λ🎉".chars());

    let index = vocabulary
        .insert_atom_sequence(&sequence)
        .expect("insert profile sequence");
    assert_eq!(vocabulary.get_atom_sequence_index(&sequence), Some(index));
    assert_eq!(
        vocabulary
            .get_term_atom_sequence::<UnicodeScalar>(index)
            .expect("reverse profile sequence")
            .as_atoms(),
        sequence.as_atoms()
    );
}
