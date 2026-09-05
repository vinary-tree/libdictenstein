#!/usr/bin/env python3
"""Join formal declarations, executable tests, and TLC negative controls.

Coverage is derived only from registered names and control bindings.  The
output deliberately marks uncovered laws instead of silently treating them as
verified.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
from pathlib import Path


APPLICABILITY = {
    "codec": "all logical-unit codecs and boundary adapters",
    "interning": "interned vocabulary and coordinated ID-sequence profiles",
    "family_refinement": "all dictionary families and applicable profile specializations",
}

STACK_SAFETY = "iterative-or-heap-backed traversal; no library workload budget"
PERFORMANCE = {
    "codec": "linear in consumed input",
    "interning": "linear in atoms plus vocabulary/index operations",
    "family_refinement": "linear in logical units plus visited result structure",
}
ACCEPTANCE_COMMAND = "scripts/verify-variable-width-formal.sh"


def load_module(path: Path, name: str):
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise SystemExit(f"cannot load extractor: {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def build(root: Path) -> list[dict[str, object]]:
    formal = load_module(
        root / "scripts/extract-variable-width-formal-inventory.py", "vw_formal"
    ).declarations(root)
    tests = load_module(
        root / "scripts/extract-variable-width-test-inventory.py", "vw_tests"
    ).registrations(root)
    by_number: dict[int, list[dict[str, object]]] = {}
    for test in tests:
        for number in test["numeric_ids"]:
            by_number.setdefault(number, []).append(test)
    rows = []
    for declaration in formal:
        positive = sorted(
            {
                test["registration"]
                for test in by_number.get(declaration["numeric_id"], [])
            }
        )
        controls = declaration["negative_controls"]
        coverage = (
            "positive_and_negative"
            if positive and controls
            else "positive_only"
            if positive
            else "negative_only"
            if controls
            else "uncovered"
        )
        rows.append(
            {
                "id": declaration["id"],
                "numeric_id": declaration["numeric_id"],
                "semantic_area": declaration["semantic_area"],
                "owner_repository": "libdictenstein",
                "owner_layer": declaration["semantic_area"],
                "applicability": APPLICABILITY[declaration["semantic_area"]],
                "kind": declaration["kind"],
                "language": declaration["language"],
                "formal_source": declaration["source"],
                "formal_artifact": declaration["source_path"],
                "proof_kind": "Rocq_proposition" if declaration["language"] == "rocq" else "TLA_assertion",
                "declaration": declaration["declaration"],
                "positive_tests": positive,
                "negative_controls": controls,
                "assumptions": "finite input; explicit profile metadata",
                "trust_boundary": f"{declaration['semantic_area']}: formal model to implementation boundary",
                "stack_safety": STACK_SAFETY,
                "performance": PERFORMANCE[declaration["semantic_area"]],
                "acceptance_command": ACCEPTANCE_COMMAND,
                "evidence_artifact": declaration["source_path"],
                "coverage": coverage,
                "status": "registered" if coverage == "positive_and_negative" else "declared-awaiting-conformance",
            }
        )
    return rows


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    payload = json.dumps(build(args.root), indent=2, sort_keys=True) + "\n"
    if args.output:
        args.output.write_text(payload, encoding="utf-8")
    else:
        print(payload, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
