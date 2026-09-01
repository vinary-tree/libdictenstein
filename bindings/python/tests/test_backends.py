from __future__ import annotations

from collections.abc import Mapping, MutableMapping
from collections.abc import Set as AbstractSet
from pathlib import Path

import libdictenstein


def test_double_array_trie_preserves_valueless_terms() -> None:
    dictionary = libdictenstein.DoubleArrayTrie(
        [("café", 7), ("caff", None), ("tea", 9)]
    )
    assert len(dictionary) == 3
    assert "café" in dictionary
    assert dictionary.lookup("caff") == (True, None)
    assert dictionary["caff"] is None
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
    assert reopened.lookup("cut") == (True, 30)
    assert reopened.lookup("cit") == (True, 4)
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
    assert reopened.lookup("alpha") == (True, 41)
    assert reopened.term(42) == "beta"
    reopened.close()


def test_mapping_views_and_stream_are_snapshot_consistent() -> None:
    dictionary = libdictenstein.DynamicDawg()
    assert isinstance(dictionary, Mapping)
    assert isinstance(dictionary, MutableMapping)
    dictionary.update({"beta": None, "alpha": 1, "alphabet": 2})

    snapshot = dictionary.snapshot()
    keys = dictionary.keys()
    assert isinstance(keys, AbstractSet)
    stream = dictionary.stream_entries()
    first = next(stream)

    dictionary["gamma"] = 3
    del dictionary["beta"]

    assert list(snapshot) == ["alpha", "alphabet", "beta"]
    assert list(keys) == ["alpha", "alphabet", "beta"]
    assert [first, *stream] == [
        ("alpha", 1),
        ("alphabet", 2),
        ("beta", None),
    ]
    assert dictionary.get("missing") is None
    assert dictionary.get("missing", 9) == 9
    assert dictionary["gamma"] == 3
    stream.close()
    dictionary.close()


def test_byte_and_u64_iteration_preserve_native_domains() -> None:
    with libdictenstein.DynamicDawg(libdictenstein.UnitDomain.BYTE) as byte_dict:
        byte_dict[b"\xff\x00"] = 7
        byte_dict[b""] = None
        assert list(byte_dict.items()) == [(b"", None), (b"\xff\x00", 7)]

    with libdictenstein.DynamicDawg(libdictenstein.UnitDomain.U64) as token_dict:
        token_dict[(2,)] = 2
        token_dict[(1, 9)] = None
        token_dict[(1,)] = 1
        assert list(token_dict.items()) == [
            ((1,), 1),
            ((1, 9), None),
            ((2,), 2),
        ]
