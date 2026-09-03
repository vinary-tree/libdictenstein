#!/usr/bin/env python3
"""Extract the authoritative VWENC declaration inventory deterministically.

This is deliberately an inventory extractor, not an implementation ledger: it
records only facts present in the checked Rocq/TLA+ sources and refuses to
silently merge duplicate identifiers.  A conformance ledger may consume the
JSON output and must supply the remaining test/oracle/control columns.
"""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path

ROCQ_RE = re.compile(r"^(?:Theorem|Lemma|Corollary)\s+(VWENC_[A-Za-z0-9_]+)")
TLA_RE = re.compile(r"^(VWENC_[A-Za-z0-9_]+)\s*==")
ID_RE = re.compile(r"^VWENC_(\d+)_")


def declarations(root: Path) -> list[dict[str, object]]:
    sources = [
        *sorted((root / "formal-verification/rocq/Spec").glob("VariableWidth*.v")),
        root / "formal-verification/tla+/VariableWidthCodecBoundary.tla",
        root / "formal-verification/tla+/VariableWidthVocabularyInterning.tla",
        root / "formal-verification/tla+/VariableWidthVocabularyPublication.tla",
        root / "formal-verification/tla+/VariableWidthFamilyRefinement.tla",
    ]
    found: dict[str, dict[str, object]] = {}
    duplicates: dict[str, list[str]] = {}
    for source in sources:
        language = "rocq" if source.suffix == ".v" else "tla"
        pattern = ROCQ_RE if language == "rocq" else TLA_RE
        for line_number, line in enumerate(source.read_text(encoding="utf-8").splitlines(), 1):
            match = pattern.match(line)
            if not match:
                continue
            identifier = match.group(1)
            location = f"{source.relative_to(root)}:{line_number}"
            row = {"id": identifier, "language": language, "source": location}
            if identifier in found:
                duplicates.setdefault(identifier, [str(found[identifier]["source"])])
                duplicates[identifier].append(location)
            else:
                found[identifier] = row
    if duplicates:
        details = "; ".join(
            f"{identifier}: {', '.join(locations)}"
            for identifier, locations in sorted(duplicates.items())
        )
        raise SystemExit(f"duplicate VWENC declarations: {details}")
    rows = []
    for identifier, row in sorted(found.items(), key=lambda item: (int(ID_RE.match(item[0]).group(1)), item[0])):
        rows.append(row)
    return rows


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    rows = declarations(args.root)
    payload = json.dumps(rows, indent=2, sort_keys=True) + "\n"
    if args.output:
        args.output.write_text(payload, encoding="utf-8")
    else:
        print(payload, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
