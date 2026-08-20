from __future__ import annotations

import pytest
from libdictenstein import DynamicDawg, UnitDomain
from libdictenstein._collection_profile import run


def test_snapshot_views_are_owned_ordered_and_value_preserving() -> None:
    dictionary = DynamicDawg(UnitDomain.BYTE)
    dictionary.update_many([(b"\xff", 2**64 - 1), (b"\x00", None), (b"\x01", 0)])
    snapshot = dictionary.snapshot()
    dictionary.clear()
    dictionary.close()

    assert list(snapshot.items()) == [
        (b"\x00", None),
        (b"\x01", 0),
        (b"\xff", 2**64 - 1),
    ]
    assert set(snapshot.keys()) == {b"\x00", b"\x01", b"\xff"}
    assert list(snapshot.values()) == [None, 0, 2**64 - 1]


def test_batch_bound_and_early_close_retain_one_revision() -> None:
    dictionary = DynamicDawg(UnitDomain.UNICODE_SCALAR)
    dictionary.update_many([("é", 7), ("e", None)])
    stream = dictionary.stream_entries(batch_size=1, max_units=1)
    assert stream.exact_len == 2
    assert stream.snapshot_identity is not None
    dictionary["z"] = 9
    dictionary.close()
    with stream:
        assert list(stream) == [("e", None), ("é", 7)]
    stream.close()

    invalid = DynamicDawg()
    try:
        with pytest.raises(ValueError):
            invalid.stream_entries(batch_size=0)
    finally:
        invalid.close()


@pytest.mark.parametrize("arm", ["materialized", "stream", "stream-cancel"])
def test_profile_emits_common_machine_row(arm: str) -> None:
    row = run(
        [
            "--arm",
            arm,
            "--entries",
            "16",
            "--passes",
            "1",
            "--warmup-passes",
            "0",
            "--batch-size",
            "4",
            "--early-cancel",
            "3",
        ]
    )
    assert row["schema"] == "libdictenstein.host-collection-traversal.v1"
    assert row["consumed_entries_per_pass"] == (3 if arm == "stream-cancel" else 16)
    assert isinstance(row["checksum"], int)
