from __future__ import annotations

from pathlib import Path

import libdictenstein
import liblevenshtein


def test_crud_batch_and_cross_project_query_snapshot() -> None:
    dictionary = libdictenstein.DynamicDawg()
    assert (
        dictionary.update_many([("cat", 1), ("cot", 2), ("cut", 3), ("scat", None)])
        == 4
    )
    assert len(dictionary) == 4
    assert dictionary.lookup("cot") == (True, 2)

    automaton = liblevenshtein.Transducer(dictionary)
    frozen = sorted(automaton.query("cat", 2), key=lambda item: str(item.term))
    cursor = automaton.query("cat", 2)
    first = next(cursor)

    assert dictionary.remove("cot")
    assert not dictionary.insert("cut", 30)
    assert dictionary.insert("cit", 5)
    dictionary.compact()
    dictionary.clear()
    assert dictionary.insert("new", 99)

    assert [item.term for item in automaton.query("cat", 8)] == ["new"]
    automaton.close()
    dictionary.close()

    observed = sorted([first, *cursor], key=lambda item: str(item.term))
    assert observed == frozen


def test_double_array_trie_composes_without_copying() -> None:
    dictionary = libdictenstein.DoubleArrayTrie(
        [("café", 7), ("caff", None), ("tea", 9)]
    )
    automaton = liblevenshtein.Transducer(dictionary)
    observed = sorted(automaton.query("cafe", 2), key=lambda match: str(match.term))
    assert [(match.term, match.id) for match in observed] == [
        ("caff", None),
        ("café", 7),
    ]
    automaton.close()
    dictionary.close()


def test_scdawg_long_lived_cursor_keeps_query_start_revision() -> None:
    dictionary = libdictenstein.Scdawg()
    assert dictionary.update_many([("cat", 1), ("cot", 2), ("cut", None)]) == 3
    automaton = liblevenshtein.Transducer(dictionary)
    cursor = automaton.query("cat", 2)
    first = next(cursor)
    assert dictionary.insert("cit", 4)
    frozen = sorted([first, *cursor], key=lambda match: str(match.term))
    assert "cit" not in {match.term for match in frozen}
    automaton.close()
    dictionary.close()


def test_persistent_artrie_checkpoint_keeps_query_start_revision(
    tmp_path: Path,
) -> None:
    path = tmp_path / "terms.part"
    dictionary = libdictenstein.PersistentARTrie.create(path)
    assert dictionary.update_many([("cat", 1), ("cot", 2), ("cut", None)]) == 3
    dictionary.checkpoint()
    automaton = liblevenshtein.Transducer(dictionary)
    cursor = automaton.query("cat", 2)
    first = next(cursor)
    assert dictionary.remove("cot")
    assert not dictionary.insert("cut", 30)
    assert dictionary.insert("cit", 4)
    dictionary.checkpoint()
    frozen = sorted([first, *cursor], key=lambda match: str(match.term))
    assert [(match.term, match.id) for match in frozen] == [
        ("cat", 1),
        ("cot", 2),
        ("cut", None),
    ]
    automaton.close()
    dictionary.close()
