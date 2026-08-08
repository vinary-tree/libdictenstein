from __future__ import annotations

from pathlib import Path

import libdictenstein


def test_double_array_trie_preserves_valueless_terms() -> None:
    dictionary = libdictenstein.DoubleArrayTrie(
        [("café", 7), ("caff", None), ("tea", 9)]
    )
    assert len(dictionary) == 3
    assert "café" in dictionary
    assert dictionary.get("caff") == (True, None)
    dictionary.close()


def test_scdawg_substring_semantics() -> None:
    dictionary = libdictenstein.Scdawg()
    assert dictionary.update_many([("cat", 1), ("cot", 2), ("cut", None)]) == 3
    assert dictionary.contains_substring("ot")
    assert dictionary.frequency("t") == 3
    assert dictionary.insert("cit", 4)
    assert dictionary.frequency("t") == 4
    dictionary.close()


def test_persistent_artrie_checkpoint_and_reopen(
    tmp_path: Path,
) -> None:
    path = tmp_path / "terms.part"
    dictionary = libdictenstein.PersistentARTrie.create(path)
    assert dictionary.update_many([("cat", 1), ("cot", 2), ("cut", None)]) == 3
    dictionary.checkpoint()
    assert dictionary.remove("cot")
    assert not dictionary.insert("cut", 30)
    assert dictionary.insert("cit", 4)
    dictionary.checkpoint()
    dictionary.close()

    reopened = libdictenstein.PersistentARTrie.open(path)
    assert "cot" not in reopened
    assert reopened.get("cut") == (True, 30)
    assert reopened.get("cit") == (True, 4)
    reopened.close()


def test_persistent_vocabulary_round_trip(tmp_path: Path) -> None:
    path = tmp_path / "terms.vocab"
    vocabulary = libdictenstein.PersistentVocabulary.create(path)
    assert vocabulary.insert("alpha", 41)
    assert vocabulary.insert("beta", 42)
    assert vocabulary.term(41) == "alpha"
    assert vocabulary.term(42) == "beta"
    vocabulary.checkpoint()
    vocabulary.close()

    reopened = libdictenstein.PersistentVocabulary.open(path)
    assert reopened.get("alpha") == (True, 41)
    assert reopened.term(42) == "beta"
    reopened.close()
