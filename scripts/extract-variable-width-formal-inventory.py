#!/usr/bin/env python3
"""Extract the authoritative VWENC declaration inventory deterministically.

This is deliberately an inventory extractor, not an implementation ledger: it
records only facts present in the checked Rocq/TLA+ sources and refuses to
silently merge duplicate identifiers.  A conformance ledger may consume the
JSON output and must supply the remaining test/oracle/control columns.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
from pathlib import Path

ROCQ_RE = re.compile(r"^(Theorem|Lemma|Corollary)\s+(VWENC_[A-Za-z0-9_]+)")
TLA_RE = re.compile(r"^(VWENC_[A-Za-z0-9_]+)\s*==")
CFG_RE = re.compile(r"^\s*(VWENC_[A-Za-z0-9_]+)\s*$")
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
    duplicate_numbers: dict[int, list[str]] = {}
    for source in sources:
        language = "rocq" if source.suffix == ".v" else "tla"
        pattern = ROCQ_RE if language == "rocq" else TLA_RE
        source_digest = hashlib.sha256(source.read_bytes()).hexdigest()
        for line_number, line in enumerate(source.read_text(encoding="utf-8").splitlines(), 1):
            match = pattern.match(line)
            if not match:
                continue
            identifier = match.group(2) if language == "rocq" else match.group(1)
            numeric_match = ID_RE.match(identifier)
            if numeric_match is None:
                raise SystemExit(f"malformed VWENC identifier: {identifier}")
            numeric_id = int(numeric_match.group(1))
            location = f"{source.relative_to(root)}:{line_number}"
            row = {
                "id": identifier,
                "numeric_id": numeric_id,
                "kind": match.group(1) if language == "rocq" else "TLA_assertion",
                "language": language,
                "semantic_area": (
                    "codec" if "Codec" in source.name else
                    "interning" if "Interning" in source.name else
                    "family_refinement"
                ),
                "source": location,
                "source_path": str(source.relative_to(root)),
                "source_line": line_number,
                "source_sha256": source_digest,
                "declaration": line.strip(),
                "negative_controls": [],
            }
            if identifier in found:
                duplicates.setdefault(identifier, [str(found[identifier]["source"])])
                duplicates[identifier].append(location)
            else:
                found[identifier] = row
            duplicate_numbers.setdefault(numeric_id, []).append(identifier)
    if duplicates:
        details = "; ".join(
            f"{identifier}: {', '.join(locations)}"
            for identifier, locations in sorted(duplicates.items())
        )
        raise SystemExit(f"duplicate VWENC declarations: {details}")
    colliding_numbers = {
        number: sorted(set(identifiers))
        for number, identifiers in duplicate_numbers.items()
        if len(set(identifiers)) > 1
    }
    if colliding_numbers:
        details = "; ".join(
            f"{number}: {', '.join(identifiers)}"
            for number, identifiers in sorted(colliding_numbers.items())
        )
        raise SystemExit(f"duplicate VWENC numeric identifiers: {details}")
    controls: dict[str, list[str]] = {}
    for config in sorted((root / "formal-verification/tla+").glob("VariableWidth*Unsafe.cfg")):
        for line in config.read_text(encoding="utf-8").splitlines():
            match = CFG_RE.match(line)
            if match:
                controls.setdefault(match.group(1), []).append(str(config.relative_to(root)))
    orphan_controls = sorted(set(controls) - set(found))
    if orphan_controls:
        raise SystemExit(
            "negative controls reference undeclared VWENC identifiers: "
            + ", ".join(orphan_controls)
        )
    for identifier, row in found.items():
        row["negative_controls"] = sorted(controls.get(identifier, []))

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
