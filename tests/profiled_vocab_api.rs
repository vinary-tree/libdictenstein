#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::vocab::PersistentVocabARTrie;
use libdictenstein::persistent_artrie::{
    PersistentARTrieU64, PersistentARTrieUleb128, PersistentARTrieUtf8, PersistentScdawg,
    PersistentScdawgChar, PersistentSuffixAutomaton, PersistentSuffixAutomatonChar,
    PersistentSuffixTree, PersistentSuffixTreeChar,
};
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

#[test]
fn persistent_profile_adapters_expose_canonical_metadata() {
    assert_eq!(
        PersistentARTrieUleb128::<()>::profile_descriptor().kind,
        libdictenstein::ProfileKind::Uleb128
    );
    assert_eq!(
        PersistentARTrieUtf8::<()>::profile_descriptor().kind,
        libdictenstein::ProfileKind::Utf8
    );
    assert_eq!(
        PersistentARTrieU64::<()>::profile_descriptor().kind,
        libdictenstein::ProfileKind::U64
    );
    assert_eq!(
        PersistentVocabARTrie::<
            libdictenstein::persistent_artrie::disk_manager::MmapDiskManager,
        >::profile_descriptor()
        .kind,
        libdictenstein::ProfileKind::UnicodeScalar
    );
    assert_eq!(
        PersistentSuffixAutomaton::<()>::profile_descriptor::<libdictenstein::Bytes>().kind,
        libdictenstein::ProfileKind::Bytes
    );
    assert_eq!(
        PersistentSuffixAutomatonChar::<()>::profile_descriptor::<libdictenstein::Utf8>().kind,
        libdictenstein::ProfileKind::Utf8
    );
    assert_eq!(
        PersistentSuffixTree::<()>::profile_descriptor::<libdictenstein::Bytes>().kind,
        libdictenstein::ProfileKind::Bytes
    );
    assert_eq!(
        PersistentSuffixTreeChar::<()>::profile_descriptor::<libdictenstein::UnicodeScalar>().kind,
        libdictenstein::ProfileKind::UnicodeScalar
    );
    assert_eq!(
        PersistentScdawg::<()>::profile_descriptor::<libdictenstein::Bytes>().kind,
        libdictenstein::ProfileKind::Bytes
    );
    assert_eq!(
        PersistentScdawgChar::<()>::profile_descriptor::<libdictenstein::Utf8>().kind,
        libdictenstein::ProfileKind::Utf8
    );
}
