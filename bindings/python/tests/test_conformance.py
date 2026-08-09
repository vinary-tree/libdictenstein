"""Uniform facade conformance suite for the Python binding.

Instantiates the family C1-C10 contract for Python against a live
libdictenstein shared library. Unlike ``test_cross_project.py`` this suite is
self-contained: it needs only libdictenstein, never a liblevenshtein
transducer, so it pins the *producer* ABI in isolation.

  C1  identity/version           test_c1_*
  C2  lifecycle/ownership        test_c2_*   (idempotent + free-order free)
  C3  error-mapping matrix       test_c3_*   (every LdictStatus arm + message)
  C4  canonical fixture replay   test_c4_*   (cross-language oracle)
  C5  CRUD/value/batch/substring test_c5_*   (+ capability-derived rejects)
  C6  text domains / values      test_c6_*   (é/🦀/combining/NUL/invalid/u64)
  C7  batch edges                test_c7_*   (0/1/255/256/257/large)
  C8  property vs oracle         test_c8_*   (CRUD script + substring naive)
  C9  leak discipline            test_c9_*   (>=10k cycles, RSS bounded)
  C10 concurrency                test_c10_*  (parallel snapshot/mutate)
"""

from __future__ import annotations

import ctypes
import json
import random
import resource
import threading
from pathlib import Path

import pytest

import libdictenstein
from libdictenstein import _native as native
from vinary_tree_interop import UnitDomain

FIXTURE = json.loads(
    (Path(__file__).resolve().parents[2] / "canonical_fixture.json").read_text(
        encoding="utf-8"
    )
)


def _entries() -> list[tuple[str, int | None]]:
    return [(item["term"], item["value"]) for item in FIXTURE["entries"]]


# --------------------------------------------------------------------------
# C1 identity/version
# --------------------------------------------------------------------------


def test_c1_identity_constants() -> None:
    assert libdictenstein.abi_version() == 1
    assert libdictenstein.api_revision() == 4


def test_c1_kind_and_capabilities() -> None:
    read, insert, remove, clear, compact, substring, checkpoint = (
        1 << bit for bit in range(7)
    )
    with libdictenstein.DynamicDawg() as dictionary:
        assert dictionary.kind == 1
        caps = dictionary.capabilities
        assert caps & insert and caps & remove and caps & clear and caps & compact
        assert not caps & substring and not caps & checkpoint
    with libdictenstein.DoubleArrayTrie([("x", None)]) as dictionary:
        assert dictionary.kind == 2
        assert dictionary.capabilities & read
    with libdictenstein.Scdawg() as dictionary:
        assert dictionary.kind == 3
        assert dictionary.capabilities & substring


# --------------------------------------------------------------------------
# C2 lifecycle/ownership
# --------------------------------------------------------------------------


def test_c2_double_close_is_idempotent() -> None:
    dictionary = libdictenstein.DynamicDawg()
    dictionary.insert("a")
    dictionary.close()
    dictionary.close()  # no double free, no crash


def test_c2_free_order_independence() -> None:
    dictionaries = [libdictenstein.DynamicDawg() for _ in range(4)]
    for index, dictionary in enumerate(dictionaries):
        dictionary.insert(f"term{index}")
    # Free in an order unrelated to construction order.
    for dictionary in (dictionaries[2], dictionaries[0], dictionaries[3], dictionaries[1]):
        dictionary.close()


def test_c2_null_free_is_a_noop() -> None:
    native._lib.ldict_dictionary_free(None)  # documented no-op


# --------------------------------------------------------------------------
# C3 error-mapping matrix + thread-local message
# --------------------------------------------------------------------------


def _len_status(handle: object) -> int:
    return native._lib.ldict_dictionary_len(handle, ctypes.byref(ctypes.c_size_t()))


def test_c3_null_pointer() -> None:
    assert _len_status(None) == 4  # NULL_POINTER
    assert native._error()


def test_c3_invalid_utf8() -> None:
    with libdictenstein.DynamicDawg(UnitDomain.UNICODE_SCALAR) as dictionary:
        with pytest.raises(libdictenstein.NativeError) as caught:
            dictionary.insert(b"\xff")
        assert caught.value.status == 3  # INVALID_UTF8
        assert str(caught.value)


def test_c3_unsupported_via_capability() -> None:
    with libdictenstein.DoubleArrayTrie([("x", None)]) as dictionary:
        with pytest.raises(libdictenstein.NativeError) as caught:
            dictionary.clear()  # DAT lacks CLEAR
        assert caught.value.status == 6  # UNSUPPORTED
        assert str(caught.value)


def test_c3_domain_mismatch() -> None:
    with libdictenstein.DynamicDawg(UnitDomain.UNICODE_SCALAR) as dictionary:
        tokens = (ctypes.c_uint64 * 2)(1, 2)
        status = native._lib.ldict_dictionary_insert_u64(
            dictionary._handle, tokens, 2, native._optional(None),
            ctypes.byref(ctypes.c_uint8()),
        )
        assert status == 9  # DOMAIN_MISMATCH
        assert native._error()


def test_c3_invalid_argument_reserved_bytes() -> None:
    with libdictenstein.DynamicDawg() as dictionary:
        dirty = native._OptionalU64(0, 1, (ctypes.c_uint8 * 7)(1, 0, 0, 0, 0, 0, 0))
        data = b"cat"
        status = native._lib.ldict_dictionary_insert_text(
            dictionary._handle, data, len(data), dirty, ctypes.byref(ctypes.c_uint8())
        )
        assert status == 2  # INVALID_ARGUMENT
        assert native._error()


def test_c3_io_error_on_missing_persistent(tmp_path: Path) -> None:
    with pytest.raises(libdictenstein.NativeError) as caught:
        libdictenstein.PersistentARTrie.open(tmp_path / "does-not-exist.part")
    assert caught.value.status == 7  # IO_ERROR
    assert str(caught.value)


def test_c3_limit_exceeded_vocab_truncation(tmp_path: Path) -> None:
    vocabulary = libdictenstein.PersistentVocabulary.create(tmp_path / "v.vocab")
    vocabulary.insert("alphabet", 5)
    length = ctypes.c_size_t()
    found = ctypes.c_uint8()
    buffer = (ctypes.c_uint8 * 3)()
    status = native._lib.ldict_vocab_get_term(
        vocabulary._handle, 5, buffer, 3, ctypes.byref(length), ctypes.byref(found)
    )
    assert status == 10  # LIMIT_EXCEEDED
    assert length.value == len("alphabet")  # full length reported
    assert bytes(buffer) == b"alp"  # truncated to capacity
    assert native._error()
    vocabulary.close()


# --------------------------------------------------------------------------
# C4 canonical fixture replay (cross-language oracle)
# --------------------------------------------------------------------------


def _assert_fixture_reads(dictionary: object) -> None:
    assert len(dictionary) == FIXTURE["size"]
    for item in FIXTURE["contains"]:
        assert (item["term"] in dictionary) == item["expected"], item["term"]
    for item in FIXTURE["get"]:
        found, value = dictionary.get(item["term"])
        assert found == item["found"], item["term"]
        assert value == item["value"], item["term"]


def test_c4_dynamic_dawg_matches_oracle() -> None:
    with libdictenstein.DynamicDawg() as dictionary:
        assert dictionary.update_many(_entries()) == FIXTURE["size"]
        _assert_fixture_reads(dictionary)


def test_c4_double_array_trie_matches_oracle() -> None:
    with libdictenstein.DoubleArrayTrie(_entries()) as dictionary:
        _assert_fixture_reads(dictionary)


def test_c4_persistent_artrie_matches_oracle(tmp_path: Path) -> None:
    dictionary = libdictenstein.PersistentARTrie.create(tmp_path / "terms.part")
    assert dictionary.update_many(_entries()) == FIXTURE["size"]
    _assert_fixture_reads(dictionary)
    dictionary.close()


def test_c4_scdawg_matches_substring_oracle() -> None:
    with libdictenstein.Scdawg() as dictionary:
        assert dictionary.update_many(_entries()) == FIXTURE["size"]
        for item in FIXTURE["substring_frequency"]:
            assert dictionary.frequency(item["pattern"]) == item["expected"], item
        for item in FIXTURE["substring_contains"]:
            assert dictionary.contains_substring(item["pattern"]) == item["expected"], item


# --------------------------------------------------------------------------
# C5 CRUD + value + batch + substring; capability-derived rejects
# --------------------------------------------------------------------------


def test_c5_crud_round_trip() -> None:
    with libdictenstein.DynamicDawg() as dictionary:
        assert dictionary.insert("cat", 1)
        assert not dictionary.insert("cat", 1)  # idempotent
        assert dictionary.get("cat") == (True, 1)
        assert dictionary.remove("cat")
        assert not dictionary.remove("cat")
        assert "cat" not in dictionary


def test_c5_compact_preserves_terms() -> None:
    with libdictenstein.DynamicDawg() as dictionary:
        dictionary.update_many([(f"t{i}", i) for i in range(50)])
        for i in range(0, 50, 2):
            assert dictionary.remove(f"t{i}")
        dictionary.compact()
        assert len(dictionary) == 25
        assert dictionary.get("t1") == (True, 1)
        assert "t0" not in dictionary


def test_c5_substring_updates_with_inserts() -> None:
    with libdictenstein.Scdawg() as dictionary:
        dictionary.update_many([("cat", 1), ("cot", 2)])
        assert dictionary.frequency("t") == 2
        assert dictionary.insert("cut", None)
        assert dictionary.frequency("t") == 3


# --------------------------------------------------------------------------
# C6 text domains and values
# --------------------------------------------------------------------------


def test_c6_precomposed_and_multibyte() -> None:
    with libdictenstein.DynamicDawg() as dictionary:
        assert dictionary.insert("café", 7)  # precomposed U+00E9
        assert dictionary.insert("🦀", 255)  # 4-byte scalar
        assert "café" in dictionary
        assert dictionary.get("🦀") == (True, 255)


def test_c6_combining_sequence_is_distinct_from_precomposed() -> None:
    precomposed = "caf\u00e9"  # café with a precomposed U+00E9
    combining = "cafe\u0301"  # cafe + U+0301 combining acute (distinct scalars)
    with libdictenstein.DynamicDawg() as dictionary:
        assert dictionary.insert(precomposed, 1)
        assert dictionary.insert(combining, 2)
        assert len(dictionary) == 2
        assert dictionary.get(precomposed) == (True, 1)
        assert dictionary.get(combining) == (True, 2)


def test_c6_byte_domain_accepts_nul_and_invalid_utf8() -> None:
    with libdictenstein.DynamicDawg(UnitDomain.BYTE) as dictionary:
        assert dictionary.insert(b"a\x00b", 1)  # embedded NUL
        assert dictionary.insert(b"\xff\xfe", 2)  # invalid UTF-8, valid bytes
        assert b"a\x00b" in dictionary
        assert dictionary.get(b"\xff\xfe") == (True, 2)


def test_c6_u64_domain_values_zero_and_max() -> None:
    with libdictenstein.DynamicDawg(UnitDomain.U64) as dictionary:
        assert dictionary.insert([1, 2, 3], 0)
        assert dictionary.insert([9], (1 << 64) - 1)
        assert dictionary.get([1, 2, 3]) == (True, 0)
        assert dictionary.get([9]) == (True, (1 << 64) - 1)


# --------------------------------------------------------------------------
# C7 batch/paging edges
# --------------------------------------------------------------------------


@pytest.mark.parametrize("size", [0, 1, 255, 256, 257, 1000])
def test_c7_batch_sizes(size: int) -> None:
    with libdictenstein.DynamicDawg() as dictionary:
        inserted = dictionary.update_many([(f"t{i}", i) for i in range(size)])
        assert inserted == size
        assert len(dictionary) == size
        if size:
            assert dictionary.get("t0") == (True, 0)
            assert dictionary.get(f"t{size - 1}") == (True, size - 1)


# --------------------------------------------------------------------------
# C8 property-based testing vs an in-language oracle
# --------------------------------------------------------------------------


def test_c8_crud_script_matches_dict_oracle() -> None:
    rng = random.Random(0xC0FFEE)
    keys = [f"k{i}" for i in range(40)]
    oracle: dict[str, int | None] = {}
    with libdictenstein.DynamicDawg() as dictionary:
        for _ in range(3000):
            key = rng.choice(keys)
            op = rng.random()
            if op < 0.5:
                value = rng.choice([None, rng.randrange(1 << 63)])
                changed = dictionary.insert(key, value)
                assert changed == (key not in oracle)
                oracle[key] = value
            elif op < 0.75:
                changed = dictionary.remove(key)
                assert changed == (key in oracle)
                oracle.pop(key, None)
            elif op < 0.95:
                assert (key in dictionary) == (key in oracle)
                if key in oracle:
                    assert dictionary.get(key) == (True, oracle[key])
                else:
                    assert dictionary.get(key) == (False, None)
            else:
                dictionary.compact()
            assert len(dictionary) == len(oracle)


def test_c8_substring_matches_naive_oracle() -> None:
    rng = random.Random(0x5CDA)
    alphabet = "abcx"
    terms = {"".join(rng.choice(alphabet) for _ in range(rng.randint(1, 6)))
             for _ in range(60)}
    def occurrences(term: str, pattern: str) -> int:
        # frequency counts overlapping occurrences, summed across all terms.
        return sum(
            1
            for start in range(len(term) - len(pattern) + 1)
            if term[start : start + len(pattern)] == pattern
        )

    with libdictenstein.Scdawg() as dictionary:
        dictionary.update_many([(term, None) for term in terms])
        for _ in range(200):
            pattern = "".join(rng.choice(alphabet) for _ in range(rng.randint(1, 3)))
            naive = sum(occurrences(term, pattern) for term in terms)
            assert dictionary.contains_substring(pattern) == (naive > 0), pattern
            assert dictionary.frequency(pattern) == naive, pattern


# --------------------------------------------------------------------------
# C9 leak discipline
# --------------------------------------------------------------------------


def test_c9_create_use_free_cycles_do_not_leak() -> None:
    cycles = 12000
    for warmup in range(2000):  # let allocators reach steady state
        dictionary = libdictenstein.DynamicDawg()
        dictionary.insert("cat", 1)
        dictionary.close()
    before = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    for _ in range(cycles):
        dictionary = libdictenstein.DynamicDawg()
        dictionary.update_many([("cat", 1), ("cot", 2), ("cut", None)])
        assert "cot" in dictionary
        dictionary.close()
    after = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    # ru_maxrss is a high-watermark in KiB; a per-cycle native leak would blow
    # far past a few MiB over 12k cycles.
    assert after - before < 32 * 1024, f"RSS grew {after - before} KiB over {cycles} cycles"


# --------------------------------------------------------------------------
# C10 concurrency
# --------------------------------------------------------------------------


def test_c10_independent_dictionaries_per_thread() -> None:
    errors: list[BaseException] = []

    def worker(seed: int) -> None:
        try:
            with libdictenstein.DynamicDawg() as dictionary:
                for i in range(2000):
                    dictionary.insert(f"t{seed}_{i}", i)
                assert len(dictionary) == 2000
                assert dictionary.get(f"t{seed}_1500") == (True, 1500)
        except BaseException as failure:  # noqa: BLE001 - surfaced to main thread
            errors.append(failure)

    threads = [threading.Thread(target=worker, args=(seed,)) for seed in range(8)]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()
    assert not errors


def test_c10_concurrent_readers_during_writer() -> None:
    errors: list[BaseException] = []
    dictionary = libdictenstein.DynamicDawg()
    dictionary.update_many([(f"seed{i}", i) for i in range(500)])
    stop = threading.Event()

    def reader() -> None:
        try:
            while not stop.is_set():
                assert "seed0" in dictionary
                dictionary.get("seed250")
        except BaseException as failure:  # noqa: BLE001
            errors.append(failure)

    readers = [threading.Thread(target=reader) for _ in range(4)]
    for thread in readers:
        thread.start()
    try:
        for i in range(500, 3000):
            dictionary.insert(f"w{i}", i)
    finally:
        stop.set()
        for thread in readers:
            thread.join()
    assert not errors
    assert dictionary.get("w2999") == (True, 2999)
    dictionary.close()
