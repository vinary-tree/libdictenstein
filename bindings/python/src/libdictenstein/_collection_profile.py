"""Deterministic public-facade collection traversal benchmark driver."""

from __future__ import annotations

import argparse
import json
import sys
import time
from collections.abc import Sequence

from . import DynamicDawg, UnitDomain

_SCHEMA = "libdictenstein.host-collection-traversal.v1"
_KEY_UNITS = 38
_U64_MASK = (1 << 64) - 1


def _positive(value: str) -> int:
    parsed = int(value)
    if parsed <= 0:
        raise argparse.ArgumentTypeError("must be positive")
    return parsed


def _nonnegative(value: str) -> int:
    parsed = int(value)
    if parsed < 0:
        raise argparse.ArgumentTypeError("must be nonnegative")
    return parsed


def _arguments(arguments: Sequence[str] | None) -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--arm", required=True, choices=("materialized", "stream", "stream-cancel")
    )
    parser.add_argument("--entries", type=_positive, default=65_536)
    parser.add_argument("--passes", type=_positive, default=1)
    parser.add_argument("--warmup-passes", type=_nonnegative, default=1)
    parser.add_argument("--batch-size", type=_positive, default=256)
    parser.add_argument("--early-cancel", type=_positive, default=64)
    return parser.parse_args(arguments)


def _corpus(size: int) -> list[tuple[bytes, int]]:
    return [
        (
            f"collection/{index & 0x0FFF:04x}/{index:08x}/shared-suffix".encode(),
            index,
        )
        for index in range(size)
    ]


def _expected(corpus: list[tuple[bytes, int]], limit: int) -> int:
    result = 0
    for key, value in sorted(corpus)[:limit]:
        result = (result + (len(key) ^ value)) & _U64_MASK
    return result


def _drain(dictionary: DynamicDawg, config: argparse.Namespace) -> tuple[int, int]:
    limit = (
        min(config.entries, config.early_cancel)
        if config.arm == "stream-cancel"
        else config.entries
    )
    if config.arm == "materialized":
        snapshot = dictionary.snapshot()
        checksum = sum(len(key) ^ (value or 0) for key, value in snapshot.items())
        return checksum & _U64_MASK, len(snapshot)

    checksum = 0
    count = 0
    with dictionary.stream_entries(
        batch_size=config.batch_size,
        max_units=config.batch_size * _KEY_UNITS,
    ) as stream:
        for key, value in stream:
            checksum = (checksum + (len(key) ^ (value or 0))) & _U64_MASK
            count += 1
            if count == limit:
                break
    return checksum, count


def run(arguments: Sequence[str] | None = None) -> dict[str, object]:
    """Run one selected arm and return its schema-valid machine row."""
    config = _arguments(arguments)
    corpus = _corpus(config.entries)
    expected = _expected(
        corpus,
        min(config.entries, config.early_cancel)
        if config.arm == "stream-cancel"
        else config.entries,
    )
    dictionary = DynamicDawg(UnitDomain.BYTE)
    try:
        inserted = dictionary.update_many(corpus)
        if inserted != len(corpus):
            raise RuntimeError(
                f"inserted {inserted} of {len(corpus)} generated entries"
            )
        consumed = (
            min(config.entries, config.early_cancel)
            if config.arm == "stream-cancel"
            else config.entries
        )
        for _ in range(config.warmup_passes):
            checksum, count = _drain(dictionary, config)
            if (count, checksum) != (consumed, expected):
                raise RuntimeError("warmup checksum or cardinality mismatch")

        started = time.perf_counter_ns()
        checksum = 0
        for _ in range(config.passes):
            pass_checksum, count = _drain(dictionary, config)
            if (count, pass_checksum) != (consumed, expected):
                raise RuntimeError("timed checksum or cardinality mismatch")
            checksum = (checksum + pass_checksum) & _U64_MASK
        elapsed_ns = max(1, time.perf_counter_ns() - started)
        if checksum != (expected * config.passes) & _U64_MASK:
            raise RuntimeError("aggregate checksum mismatch")
        return {
            "schema": _SCHEMA,
            "runtime": "python",
            "arm": config.arm,
            "dictionary_entries": config.entries,
            "consumed_entries_per_pass": consumed,
            "passes": config.passes,
            "warmup_passes": config.warmup_passes,
            "batch_size": None if config.arm == "materialized" else config.batch_size,
            "early_cancel": config.early_cancel
            if config.arm == "stream-cancel"
            else None,
            "elapsed_ns": elapsed_ns,
            "checksum": checksum,
        }
    finally:
        dictionary.close()


def main(arguments: Sequence[str] | None = None) -> int:
    try:
        print(json.dumps(run(arguments), separators=(",", ":")))
        return 0
    except (RuntimeError, ValueError, TypeError, ArithmeticError, OSError) as error:
        print(str(error), file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
