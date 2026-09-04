#!/usr/bin/env python3
"""Extract registered executable VWENC test names and source locations.

The inventory is intentionally a coverage index, not a claim that every formal
law already has a test.  It rejects duplicate registrations and registrations
for identifiers absent from the authoritative formal inventory.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

TEST_RE = re.compile(
    r"^\s*fn\s+(vwenc_(?:\d+)(?:_to_(?:\d+))?_[A-Za-z0-9_]+)\s*\("
)
ID_RE = re.compile(r"^vwenc_(\d+)(?:_to_(\d+))?_")


def formal_numbers(root: Path) -> set[int]:
    extractor = root / "scripts/extract-variable-width-formal-inventory.py"
    result = subprocess.run(
        [sys.executable, str(extractor), "--root", str(root)],
        check=True,
        capture_output=True,
        text=True,
    )
    return {int(row["numeric_id"]) for row in json.loads(result.stdout)}


def registrations(root: Path) -> list[dict[str, object]]:
    known = formal_numbers(root)
    rows: list[dict[str, object]] = []
    seen: set[str] = set()
    for source in sorted((root / "tests").rglob("*.rs")):
        for line_number, line in enumerate(source.read_text(encoding="utf-8").splitlines(), 1):
            match = TEST_RE.match(line)
            if not match:
                continue
            registration = match.group(1)
            if registration in seen:
                raise SystemExit(f"duplicate VWENC test registration: {registration}")
            seen.add(registration)
            id_match = ID_RE.match(registration)
            assert id_match is not None
            first = int(id_match.group(1))
            last = int(id_match.group(2) or first)
            numbers = list(range(first, last + 1))
            unknown = sorted(set(numbers) - known)
            if unknown:
                raise SystemExit(
                    f"VWENC test {registration} references undeclared identifiers: "
                    + ", ".join(map(str, unknown))
                )
            rows.append(
                {
                    "registration": registration,
                    "source": f"{source.relative_to(root)}:{line_number}",
                    "source_path": str(source.relative_to(root)),
                    "source_line": line_number,
                    "numeric_ids": numbers,
                }
            )
    return rows


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    payload = json.dumps(registrations(args.root), indent=2, sort_keys=True) + "\n"
    if args.output:
        args.output.write_text(payload, encoding="utf-8")
    else:
        print(payload, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
