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

#[test]
fn persistent_profile_sequence_survives_checkpoint_reopen() {
    let directory = tempfile::tempdir().expect("temporary vocabulary directory");
    let path = directory.path().join("profile-reopen.vocab");
    let sequence = AtomSequence::<UnicodeScalar>::from_atoms("日本語🎉".chars());
    let index;

    {
        let vocabulary = PersistentVocabARTrie::create(&path).expect("create vocabulary");
        index = vocabulary
            .insert_atom_sequence(&sequence)
            .expect("insert profile sequence");
        vocabulary.checkpoint().expect("checkpoint vocabulary");
    }

    let (reopened, report) =
        PersistentVocabARTrie::open_with_recovery(&path).expect("reopen vocabulary");
    assert!(report.mode.is_normal());
    assert_eq!(reopened.get_atom_sequence_index(&sequence), Some(index));
    assert_eq!(
        reopened
            .get_term_atom_sequence::<UnicodeScalar>(index)
            .expect("reverse reopened sequence")
            .as_atoms(),
        sequence.as_atoms()
    );
}
